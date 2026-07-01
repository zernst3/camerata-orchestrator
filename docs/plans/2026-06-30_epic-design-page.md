# Design Page — co-design a work hierarchy with an AI, draft it as a tree, publish as a batch

Status: DRAFT (design proposal, not yet built)
Date: 2026-06-30
Owner: Zach
Related: `2026-06-22_uow_ai_story_authoring.md`, `2026-06-23_uow_parent_id_field.md`, `2026-06-22_issues_table_grouping_and_chat_context.md`, `2026-06-27_model-efficiency-and-provider-agnostic-plan.md` (Designer MODULE), companion `2026-06-30_workspaces-page.md`

## 0. Terminology

A **design** is the catch-all unit of work this page produces: a top-level item plus the tree of work it decomposes into. Most orgs would call the top item an "epic," but the hierarchy is configurable, so we use the neutral word **design** for the whole activity and the top node. A **node** is any item in the tree (a design, feature, story, defect, etc.).

## 1. The idea

A page inside Camerata where a person co-designs a **hierarchy of work** with an AI, the way we design work in a chat session today: talk it through, let the AI propose the shape, let it **draft the tree**, review and iterate while everything is still a **draft** (nothing on the board yet), then **publish** the whole tree as a batch into the issue tracker.

What makes it more than "story authoring with extra steps":

1. **The hierarchy is a project-defined, drag-and-drop type graph.** Real orgs are strict about which work types nest under which (a Feature may parent a Story and/or a Defect, a Story may parent a Task, etc.). The person builds their org's rules by drag-and-drop from a palette of default types (Initiative, Epic, Feature, Story, Defect, Task, Bug) plus their own **custom** types. This schema is **saved as a project-level field** and feeds the AI so it drafts against the right structure.
2. **The drafts land in a relationship-mapped table**, hierarchy in place, exactly like the published GitHub issues table.
3. **It absorbs the deferred UI Designer and adds diagrams.** A node can carry a UI mockup (via the already-built vision pipeline) and **diagrams** (Mermaid), with a **separate Lucidchart export**.

For the MVP we ship the **GitHub Issues adapter only**. Because GitHub has no native "Epic" (it is all freetext), default and custom types are equally just **labels**, which makes this model a clean fit. Adapter-native, typed hierarchies (ADO's Initiative/Epic/Feature/Story/Task) come with those adapters later.

## 2. What this builds on (already shipped)

Largely a **composition** of existing primitives.

| Primitive | Where | What it gives us |
|---|---|---|
| AI story authoring | `POST /api/uow/blank`, `/api/uow/:id/author`, `/api/uow/:id/publish` (`crates/server/src/lib.rs:751`) | The draft-key + AI clarification chat + draft title/body + publish engine. Generalized from one story to a tree. |
| Draft UoW model | `UnitOfWork` + `AuthoringState` (`crates/server/src/uow.rs:351`) | `story_id = "draft-<token>"`, `authoring: Some(..)`, `work_item: None` until publish. |
| Parent/child link | `parent_id` on the draft, `link_sub_issue` at publish (`uow.rs:443`, `github_issues.rs`) | Native GitHub sub-issue wiring, fail-soft. Nests up to 8 deep. |
| Relationship table (read) | `IssueSummary.parent_number`, `WorkItemRow.parent_label`, `set_grouping([ColumnId("parent")])` (`2026-06-22_issues_table_grouping...`) | A hierarchical, grouped Chorale table. The draft tree reuses this exact pattern; the published tree lands back in it. |
| Designer AGENT (IR pipeline) | `delegate { tier: "vision" }` → HTML/Tailwind IR → logic tier → `rsx!` (`crates/gateway/src/delegate.rs`, `DesignerBand`) | A **built** vision pipeline emitting an HTML/Tailwind mockup. The deferred user-facing "module" is a thin surface over it. |
| Governed run spawn | `start_governed_run(...)` (`crates/server/src/lib.rs:1485`) | The analog for an agent doing multi-step work. |

Net-new: the **hierarchy schema builder**, the **N-level draft tree** (model + table), **batch publish** across levels, and **diagrams + Lucidchart export**.

## 3. Hierarchy schema (the drag-and-drop type graph) — a saved project-level field

This is the centerpiece. Per project, a **saved, portable** `HierarchySchema` declares the work types and the allowed parent→child relationships.

```
HierarchySchema {                    // PROJECT-LEVEL, persisted on Project, travels in export
  types: Vec<WorkType {
    name: String,                    // "Feature", "Defect", or a custom "Spike"
    builtin: bool,                   // from the default palette vs. user-added
    is_design_root: bool,           // may this type start a design (be the top node)?
  }>,
  relations: Vec<TypeRelation {      // a DAG: "child may nest under parent"
    parent: String,
    child: String,
  }>,
}
```

### 3.1 The builder UI
- **Palette** of default types (Initiative, Epic, Feature, Story, Defect, Task, Bug) plus a **"+ Custom type"** freetext entry.
- **Canvas**: drag a type in, connect parent → child by drag-and-drop. A parent may have **multiple child types** (Feature → Story AND Defect). A child type may sit under **multiple parents** (a Defect under Feature or Story), so `relations` is a general DAG, not a single ladder. Mark which types are **design roots** (usually Epic or Initiative).
- **Custom types slot in anywhere.** Because GitHub types are freetext, a custom type behaves identically to a built-in one: it is just a name that can connect to any other type. A project can also go fully custom (its own type set, no built-ins).
- **Info affordance**: each default type carries an `(i)` hover explaining the Scrum/ADO meaning (e.g. "Epic: a large body of work delivered over multiple sprints, decomposed into Features/Stories"), so people unfamiliar with the terminology are not lost.

### 3.2 How it feeds the AI
The schema is injected into the design agent's grounding: the allowed types, their meanings, the root type(s), and the nesting rules ("Feature children may be Story or Defect; Story children may be Task"). The agent drafts a tree that respects the schema, and node-add affordances in the UI only offer child types the schema permits.

### 3.3 Why project-level and saved is critical
Teams reuse one hierarchy across all their work. Storing it on the project (and exporting it with the project, alongside `TierMap`/`DesignerBand`/rulesets) means a design session starts from the team's real structure every time, and the structure travels when the project is shared.

### 3.4 Positioning: Camerata's own, more capable alternative to GitHub Issue Types
Because the schema lives in Camerata at the **project level** (not in a tracker's org settings), it is effectively Camerata's own answer to GitHub Issue Types, and a more capable one:
- **Per-project, not org-locked.** Every project defines its own taxonomy; no org-admin gatekeeping, and it works on personal-account repos where GitHub Issue Types do not exist.
- **Freetext + custom types anywhere.** No fixed enum and no cap.
- **Relationship-aware.** It encodes the parent→child nesting RULES (which types may nest under which). GitHub Issue Types are a flat, per-issue label; they do not model hierarchy relationships at all.
- **Portable and tracker-agnostic.** It travels with the project export, so a team carries its work taxonomy between repos and, later, between trackers. Where an adapter has native typing (GitHub Issue Types when present, ADO work-item types), Camerata maps down onto it; where it does not, Camerata's schema still holds.

This is a real differentiator, not just an implementation convenience: an org that manages its hierarchy in Camerata gets a customizable, relationship-enforcing, portable typing system that outlives any single tracker.

## 4. UX flow

```
  (one-time / per-project)  HIERARCHY SCHEMA BUILDER
     palette: [Initiative][Epic][Feature][Story][Defect][Task][Bug][+ Custom]
     canvas:  Epic ─┬─▶ Feature ─┬─▶ Story ─▶ Task
                    │            └─▶ Defect
                    └─▶ (design root ●)                       [Save to project]

  New design (root type: Epic)
     │
     ▼
  ┌── DESIGN CANVAS ──────────────────────────────────────────────────┐
  │  ┌ draft tree (relationship table) ┐   ┌ selected node ─────────┐  │
  │  │ ▾ Epic     Checkout revamp draft │   │ Story: Auth UI         │  │
  │  │   ▾ Feature  Auth         draft  │   │ ┌ design chat ──────┐  │  │
  │  │       Story  Auth UI  ◀sel draft │   │ │ you: … / ai: … ?  │  │  │
  │  │       Defect Legacy 500s  draft  │   │ └───────────────────┘  │  │
  │  │   ▾ Feature  Payments     draft  │   │ preview: body          │  │
  │  │  [+ add child ▾ (Story|Defect)]  │   │ mockup: [Design UI]    │  │
  │  └──────────────────────────────────┘   │ diagrams: [+ Arch]     │  │
  │                                          │  ↳ [Export Lucidchart] │  │
  │  target repo: [owner/repo ▾]   [ Publish design (6 items) ]        │  │
  └───────────────────────────────────────────────────────────────────┘
```

1. **(Once) build the hierarchy schema** for the project by drag-and-drop; save it.
2. **Start a design** rooted at a design-root type.
3. **Design with the AI (chat)** — the `author` loop, but the agent proposes **child nodes at schema-permitted types** (`proposed_children: [{ type, title, body }]`).
4. **Materialize the tree.** Accept/edit: each proposed node becomes a draft node linked to its parent. Add/remove/re-parent nodes; the "add child" menu only offers schema-allowed types. **Once created, the drafts render immediately in the relationship table with the hierarchy in place.**
5. **Per-node artifacts** — UI mockup (vision pipeline) and/or diagrams (Mermaid), with a separate Lucidchart export (section 7).
6. **Iterate freely** — everything is drafts.
7. **Publish the batch** — top-down; lands in the existing grouped issues table.

## 5. Data model (nodes)

Reuse `UnitOfWork`. Add:

- `node_type: Option<String>` — the schema type this node represents (`"Feature"`, `"Defect"`, a custom name). `#[serde(default)]`.
- `draft_parent_id: Option<String>` — the parent **draft** node's `story_id` (`draft-<token>`), forming the N-level tree pre-publish; resolves into the numeric `parent_id` at publish. Keeps `parent_id`'s digits-only, published contract intact.
- `design_artifacts: DesignArtifacts` (section 7).

The draft table reuses the published table's grouping: a `DraftRow` wrapper computes indentation from `draft_parent_id`, grouped via the same `set_grouping` idiom. Tree depth is validated against the adapter max (8 for GitHub) and against the schema (a node may only take child types `relations` allows).

## 6. Adapter mapping (GitHub Issues for the MVP)

The schema, types, and tree are **abstract**; each work-tracker adapter maps them.

- **GitHub Issues (MVP):** each node → an issue; nesting → **sub-issues** (`link_sub_issue`), up to 8 deep. `node_type` → a **label** (e.g. `type:feature`, `type:spike`). Labels are the right fit because they are **freetext, repo-level, and unlimited**, so **built-in and custom types map identically** with no org setup. Note that GitHub's newer **Issue Types** are a *different* mechanism and NOT a fit for the baseline: they are an **org-level, admin-defined controlled enum** (one per issue, not freetext, capped, unavailable on personal-account repos). Using them would force every custom type through org-admin configuration. They are therefore at most an **opportunistic extra** later (when an org has Issue Types configured and a schema type name matches one, set the native type in addition to the label), never the baseline. (Issue Types are a newer, evolving feature; confirm current behavior before building on them.)
- **ADO (future adapter):** the same schema maps to ADO's native Initiative/Epic/Feature/Story/Defect/Task work-item types and their enforced links. No page changes, only the adapter.

## 7. Design artifacts on a node: mockups + diagrams

```
DesignArtifacts {
  mockup: Option<UiMockup { html: String, notes: String }>,
  diagrams: Vec<Diagram { kind: DiagramKind, mermaid: String, title: String }>,
}
DiagramKind = Architecture | ApiRouting | Sequence | EntityRelationship | Flow | Other
```

- **UI mockup (folds in the deferred UI Designer).** The "designer module" was deferred as "a separate user-facing UI/UX mockup tool" (`2026-06-27...:213`). It need not be separate: the **designer agent already emits HTML/Tailwind IR**. A node's "Design UI" action runs that pipeline in **mockup mode** (produce the IR as a requirement artifact; do not translate to `rsx!`; do not enter the dev hierarchy). Gating reuses `DesignerBand` + the vision-capable-model check.
  - **Real-time preview in the desktop webview.** The cockpit is a wry/WebKit webview, so a mockup's HTML/Tailwind renders live with no extra runtime: inject it into a **sandboxed `<iframe srcdoc>`** with the Tailwind CDN (or the target's compiled CSS) on the canvas. As the agent produces/streams the IR, the preview pane updates. At publish the IR travels in the issue body so the dev-hierarchy designer/logic tier can translate it to `rsx!` later.
  - **Fidelity: representative, NOT 1:1 (locked, Zach 2026-06-30).** The mockup is a *realistic sketch*, like a mockup would be in real life. It is explicitly NOT expected to match the final product pixel-for-pixel — that accuracy is not practical and not the goal. What it MUST capture is the **thematic aesthetic, styling, and layout** consistent with **what already exists**, derived from the requirements. Concretely: the vision agent is grounded on the **target project's** existing design system (surfaced theme tokens, component/layout patterns from its codebase) plus the story's requirements, so the mockup "looks like the app it belongs to" at the level of theme/layout/spacing/components — a faithful impression, not a spec. For a greenfield target with no existing UI, it grounds on the requirements plus a sensible default aesthetic. Prompt + acceptance framing should reinforce "capture the look and structure, do not chase pixel accuracy."
  - **Window scope: intentionally minimal (locked, Zach 2026-06-30).** For now the mockup window is JUST two things: a **free-text back-and-forth chat with the agent**, and a **live `<iframe>` preview that updates in real time** as the agent revises the HTML/Tailwind. That is the entire surface — no layer panels, no component pickers, no canvas editing tools. This reuses the existing chat/authoring pattern plus the iframe, which keeps it simple and low-risk to build.
  - **Artifact flow: the mockup becomes a FILE the UoW carries (locked, Zach 2026-06-30).** When the design session is done, the mockup is saved as an **HTML file embedded on the story / Unit of Work**. This surfaces a real requirement: **Units of Work must support attached files** (import + storage + access) — a mockup is useless if the UoW cannot access it. So this increment adds a UoW **attachments** concept (a named file + content/mime, stored with the UoW, portable) and the design session writes the mockup there. At publish, the mockup file must make it INTO the created GitHub issue. **Open implementation question**: GitHub's REST API has no clean "attach an arbitrary file to an issue" endpoint (issue attachments go through the web UI's user-attachments CDN). Candidate approaches to decide at build time: (a) embed the HTML inline in the issue body inside a collapsed `<details>` block; (b) commit the HTML into the repo (e.g. `docs/mockups/<story>.html`) and link it from the issue; (c) a gist + link. Whichever we pick, the **UoW attachment support is the prerequisite** and is in scope here.
- **Diagrams (Mermaid).** The AI generates diagrams as **Mermaid** text (architecture, API-routing, sequence, ER, flow). Mermaid is text (agent-authorable, diffs cleanly), renders **natively in GitHub issue bodies**, and embeds as a fenced ` ```mermaid ` block at publish with zero conversion. Renders in a canvas preview (Mermaid JS in the same webview via a small interop, or a server render; impl detail).
- **Lucidchart export (separate button).** Alongside the Mermaid artifact, a distinct **"Export for Lucidchart"** action emits a Lucidchart-importable file from the same diagram source. Note: raw SVG is confirmed NOT importable by Lucidchart (per Zach's experience), so the target is a Lucidchart-supported format. **Exact format is an open implementation question** (external, verify against Lucidchart's current supported imports): leading candidates are Visio `.vsdx` or a draw.io XML export generated from the Mermaid source. Decide at build time after verifying what Lucidchart accepts today. This stays a separate button precisely because it is a different, heavier path than the inline Mermaid.

## 8. API surface

| Endpoint | Body | Returns | Notes |
|---|---|---|---|
| `GET/PUT /api/projects/:id/hierarchy` | `HierarchySchema` | schema | The saved project-level type graph (section 3). |
| `POST /api/designs/blank` | `{ root_type }` | `{ design_id }` | Draft root node at a design-root type. |
| `POST /api/designs/:id/author` | `{ message }` | full node `UnitOfWork` | Design-mode prompt; parses `title/body/reply` plus `proposed_children: [{ type, title, body }]` (types constrained to the schema). |
| `POST /api/designs/:id/nodes` | `{ parent_draft_id, nodes: [{ type, title, body }] }` | `{ node_ids }` | Materialize children; server validates types against `relations`. |
| `DELETE /api/designs/:id/nodes/:node_id` | | `{ ok }` | Remove a draft subtree. |
| `POST /api/designs/:node_id/diagram` | `{ kind, prompt }` | `{ diagram }` | AI-generate a Mermaid diagram. |
| `GET /api/designs/:node_id/diagram/:idx/lucid` | | file | Lucidchart-importable export (format per section 7). |
| `POST /api/designs/:node_id/mockup` | `{ prompt }` | `{ mockup }` | Vision pipeline, mockup mode. |
| `POST /api/designs/:id/publish` | `{ repo }` | `{ nodes: ["#N", ...], warnings: [...] }` | Batch top-down publish. |

Per-node refinement reuses `POST /api/uow/:node_id/author`.

## 9. Publish

**Draft-until-publish, top-down at publish.** Walk the tree top-down: create each node's issue (reuse `onboard::create_issue`), apply its `type:<name>` label, resolve `draft_parent_id` to the parent's fresh number, `link_sub_issue`, embed diagrams/mockup in the body, collect any `parent_link_warning`. Fail-soft per node; return created numbers plus warnings. Lands in the grouped issues table. Concurrent siblings are a later enhancement.

## 10. Open questions (decisions to confirm before build)

1. **Draft link field**: `draft_parent_id` as proposed (recommended), keeping `parent_id` digits-only.
2. **Publish atomicity**: top-down, sequential, fail-soft (recommended for v1) vs. all-or-nothing with rollback.
3. **~~Node-type on GitHub~~ RESOLVED**: use `type:<name>` labels (freetext, zero-config, custom types free). Native GitHub issue types are an optional later nicety.
4. **Schema enforcement strictness**: does the AI + UI strictly forbid schema-violating nesting, or warn-and-allow (for the "just let me put anything anywhere" case)? (Recommend strict for AI drafting + schema-limited add menus, with a "free-form" project schema option for people who want no rules.)
5. **Lucidchart export format**: verify Lucidchart's current supported imports; pick `.vsdx` vs. draw.io XML vs. other. SVG is out. (External fact to confirm at build time.)
6. **Diagram render on canvas**: JS interop Mermaid vs. server-side render. (Impl detail, non-blocking.)

## 11. Phasing

- **Increment 1 — schema builder + manual drafting.** `HierarchySchema` (drag-and-drop, saved project-level, default + custom types, `(i)` hovers), `node_type`, `draft_parent_id`, the draft relationship table, manual add/remove/re-parent (schema-limited), batch top-down publish with `type:*` labels. Proves the model end to end on the built publish path. GitHub only.
- **Increment 2 — AI decomposition.** Design-mode prompt proposes schema-valid children; "Propose nodes" materializes them; per-node refinement via the existing authoring panel; schema feeds grounding.
- **Increment 3 — design artifacts.** Mermaid diagrams per node; Lucidchart export button; UI mockup via the vision IR pipeline; inline previews; artifacts travel into publish.
- **Increment 4 — polish.** Concurrent sibling publish, subtree regeneration, draft persistence/resume.

## 12. Scope caveat (MVP)

The schema, types, tree, and artifacts are built **tracker-agnostic and depth-flexible** on purpose, but the MVP ships **only the GitHub Issues adapter** (sub-issues up to 8 deep; `type:<name>` labels for both built-in and custom types). ADO-style typed, enforced hierarchies come with their adapters and need no changes to this page.
