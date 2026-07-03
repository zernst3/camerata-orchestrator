# ADR: Design Canvas — designs as first-class, lifecycled Units of Work

**Date:** 2026-07-02
**Status:** Accepted (increments shipped; artifacts staged)
**Builds on:** `2026-06-30_epic-design-page.md` (plan), `2026-06-22_uow_ai_story_authoring.md`, `2026-06-23_uow_parent_id_field.md`

## Context

The Design Canvas lets a person co-design a hierarchy of work with an AI: talk it through,
let the model propose a tree of child nodes, review and iterate while everything is still a
draft (nothing on the board yet), then publish the whole tree as a batch of GitHub issues.
The plan (`2026-06-30_epic-design-page.md`) established the model: a **design** is a top node
plus the tree of work it decomposes into, built as a composition of the already-shipped
authoring primitives (draft UoW, AI author loop, `parent_id`/sub-issue linking, the grouped
Chorale table).

The first cut proved the pipeline but left a design as an anonymous, transient artifact. A
design persisted only as a set of draft UnitOfWork records with a `draft-<token>` story_id;
there was no way to enumerate a project's designs, no lifecycle (a design was neither
published nor archived, just "drafts sitting in the store"), and no way to delete a whole
design. Several contract and grounding bugs also made the canvas fail in ordinary use: the
client and server disagreed on the blank-design and publish payloads, the AI-drafted child
`body` was silently dropped on materialize, the mockup flow hard-required a typed prompt, and
an empty project hierarchy schema resolved to `ALLOWED_CHILD_TYPES: []` so the planner
proposed no children at all.

This ADR records the decisions that turned the Design Canvas from a working-but-fragile
pipeline into a manageable, discoverable surface, and it places that surface in the product's
adapter architecture.

## Decision

### 1. A design is a Unit of Work with an explicit design-root marker and its own status

A design root is a `UnitOfWork` carrying two new fields (`crates/server/src/uow.rs`):

- `is_design_root: bool` (serde default `false`). Stamped `true` by
  `create_blank_design` only when `draft_parent_id.is_none()`, i.e. only for the top node.
  The `draft-<token>` story_id prefix is shared with ordinary AI-authored draft stories, so
  it is NOT a reliable design marker. An explicit boolean is unambiguous, cheap, and
  back-compat: legacy records read as `false`, which is correct because no design canvas
  existed when they were written.
- `design_status: Option<String>`, one of `draft`, `published`, `archived` (the const
  `DESIGN_STATUSES` in `crates/server/src/lib.rs`). Defaulted to `Some("draft")` at
  create-time; `None` reads as draft. This is the design's OWN publish/archive lifecycle and
  is deliberately distinct from the development-run lifecycle (`UowStage`/`DevStatus`), which
  tracks a story through governed development. A design gets stamped `published`
  (best-effort) after a successful publish, and `archived`/`draft` via the status endpoint.

Designs are persisted and auto-saved on every author turn server-side, enumerated per
project, and deletable as a whole tree. This mirrors the UoW status model the rest of the
product already uses, so designs behave like every other first-class entity rather than like
scratch state.

### 2. Per-project list + lifecycle + delete endpoints

Registered alongside the existing `/api/designs/*` and `/api/projects/:id/*` blocks:

- `GET /api/projects/:id/designs` returns `{ designs: [ { id, title, node_type, status,
  node_count, updated } ] }`, one summary per design root owned by the project, sorted
  newest-first by `updated`.
- `POST /api/designs/:id/status { status }` sets a root's status and returns the updated
  summary. `400` on an unknown status, `404` when `:id` is not a design root.
- `DELETE /api/designs/:id` removes the whole tree (root plus descendants) via
  `remove_design_subtree`; `404` when `:id` is not a design root. (A single child is deleted
  through `DELETE /api/designs/:id/nodes/:node_id` instead.)

Backing store methods (`crates/server/src/uow.rs`): `list_design_roots_for_project`
(is-design-root plus no parent plus project match), `get_design_root` (root-or-`None`, so
non-roots `404`), and `set_design_status` (root-guarded, persisted).

The UI surfaces this in the canvas empty state: a project's saved designs render as rows with
a title, a status badge (draft = neutral, published = green, archived = muted), a
"Type · N nodes" meta line, and a trimmed updated date. A row opens its tree; a two-step
inline delete (trash, then "Confirm?") calls the delete endpoint and re-fetches. Inside an
open design, the header shows the status badge, an Archive/Unarchive toggle, a back control,
and a subtle "✓ Saved" auto-save indicator. Pure helpers (`meta_label`, status → badge class,
`short_updated`) live in `camerata-ui-core::designs` with no Dioxus dependency, per
RUST-HEADLESS-CORE-1, and are unit-tested.

### 3. The planner proposes children against the project's configured hierarchy, and an empty schema resolves to the default ladder

The design planner does NOT assume a hardcoded Epic → Story shape. It drafts against the
project's saved `HierarchySchema` (the drag-and-drop type graph): the allowed types, the root
type(s), and the parent → child nesting rules feed the author prompt, and materialize
validates every proposed child against `relations`.

An empty or absent schema is a bug source, not a valid configuration: it makes the author
prompt emit `ALLOWED_CHILD_TYPES: []` (so the model proposes nothing and any proposals are
stripped) and makes materialize reject every child, leaving only the root node. The fix is a
single source of truth: `HierarchySchema::{is_usable, resolve_effective}` in
`crates/app-core/src/project.rs` resolves an empty schema (no types, or types with no
relations) to `default_hierarchy_schema()`, and returns a usable custom schema verbatim so an
intentional taxonomy is never clobbered. `AppState::effective_hierarchy_schema()` wraps this
and is used by BOTH the author handler and the materialize handler, so proposal, stripping,
and validation always agree on the same non-empty schema. `GET /api/projects/:id/hierarchy`
resolves empty → default too, so the UI reads the effective schema. Import seeding
(`import_or_overwrite`, both create and overwrite branches) applies `resolve_effective` so a
bare import that omitted a schema gets the default ladder. This resolution is in-flight only;
it does not persist the default onto the project.

The "+ Add child node" affordance is schema-driven for the same reason: it fetches the
project's effective schema and offers the first allowed child type for the selected node's
type, rendering no button for a leaf type. It no longer hardcodes "Story", which under an
Epic was schema-invalid and produced a dead-end button (materialize rejected it).

### 4. Mockup generation grounds on node + parent context even with an empty prompt

The "Generate HTML mockup" flow no longer hard-requires a typed instruction. When the
"Describe the UI" box is blank, the server (`uow_generate_mockup`) grounds the mockup in the
node's own story (`draft_title`, `draft_body`, `requirements_prompt`) plus its parent node
(resolved via `draft_parent_id`, prepended as "Parent {node_type}: {title}\n{body}"). It
fails only when the message is empty AND there is nothing at all to ground on. When the
message is empty a synthesized instruction stands in; a typed message is kept verbatim. The
prompt build is extracted into a pure, unit-tested `build_mockup_prompt`. The client
(`MockupPanel`) drops its early empty-message guard so Generate fires on a blank box (the
LoadingGuard is retained), and the hint copy tells the user a blank prompt still generates
from the item's story plus parent context.

### 5. The AI-drafted child `body` is preserved end to end

An earlier bug dropped the AI-drafted child `body` on materialize, so accepted proposals
became empty-bodied nodes. Three drop points in `crates/ui/src/design.rs` were fixed: the
client `ProposedChild` struct now carries `#[serde(default)] pub body: String` (serde was
silently discarding it on deserialize); the proposed-children render shows the full
"NodeType: Title" heading plus the markdown body (not a title-only chip); and materialize
sends `c.body` instead of a hardcoded `""`, so the drafted content flows into the new nodes.

### 6. The Design Canvas is one adapter over the headless core

Framing, not a code change: the Design Canvas is one **adapter** over Camerata's headless
core capabilities, the same core that governed development, scanning, and authoring already
sit on. The design surface, a chat/voice adapter, an MCP adapter, and a future richer UI are
all envisioned as peers over the same core (the "holodeck"/LCARS endgame). The canvas
composes existing core primitives (the draft UoW, the author loop, sub-issue linking, the
grouped table, the vision pipeline) rather than owning new domain logic, which is exactly what
being an adapter means. The pure helpers living in `camerata-ui-core` (Dioxus-free) reinforce
the boundary. This is the honest architectural placement; the full adapter ladder is staged,
not built (see Honest limits).

## Consequences

- Designs are managed entities: listable per project, statused, auto-saved, and deletable,
  consistent with how the rest of the product treats Units of Work.
- `is_design_root` is the durable marker; nothing keys design behavior off the `draft-`
  story_id prefix (which it shares with ordinary drafts).
- Design status and development status are two independent lifecycles on the same UoW type
  and must not be conflated: publishing a design is not the same event as a story completing
  a dev run.
- Hierarchy resolution has one owner (`resolve_effective` via `effective_hierarchy_schema`),
  so the author prompt, materialize validation, the hierarchy GET, and import seeding cannot
  drift apart. Add-child menus and the planner both read it, so no schema-invalid affordance
  should reach the user.
- The client/server design contract is pinned by wiremock tests (blank parses `design_id`,
  publish parses `nodes` as success) after four silent contract mismatches (New tree, Add
  child, Publish all) shipped and had to be repaired.

## Honest limits

- **The adapter ladder is aspirational.** Only the desktop UI adapter exists. Chat, voice,
  and MCP adapters over the same core are the intended endgame, not shipped surfaces. The
  "one adapter over the core" framing is an accurate description of the current design's
  placement and a statement of direction, not a claim that peer adapters exist.
- **GitHub Issues is the only publish adapter.** The schema, node types, and tree are built
  tracker-agnostic and depth-flexible on purpose, but publish targets GitHub only (sub-issues
  up to 8 deep; `type:<name>` labels for built-in and custom types alike). ADO-style typed,
  enforced hierarchies come with their adapters later.
- **Design artifacts are partly staged.** Mockup generation and its live `<iframe srcdoc>`
  preview are in. Mermaid diagrams and the separate Lucidchart export are planned; the
  Lucidchart export format is an unresolved external question (raw SVG is confirmed not
  importable). The UoW attachment concept that would let a mockup travel into the published
  issue as a real file is a prerequisite still being worked out (candidates: inline
  `<details>`, a committed repo file, or a gist link).
- **Publish is fail-soft, not atomic.** The tree publishes top-down and sequentially, failing
  soft per node and returning created numbers plus warnings. There is no all-or-nothing
  rollback and no concurrent-sibling publish yet.
- **Mockup fidelity is representative, not 1:1.** The mockup is a realistic sketch that
  captures theme, layout, and components consistent with the target app, not a pixel-accurate
  spec. Chasing pixel accuracy is explicitly a non-goal.
