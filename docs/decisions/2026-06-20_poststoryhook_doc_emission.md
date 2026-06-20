# PostStoryHook: per-story documentation emission (PROC-STORY-DOCS-1)

**Date:** 2026-06-20
**Status:** Accepted
**Story:** dev5/poststoryhook
**Rule:** PROC-STORY-DOCS-1 (default: `per-story-docs`)

## Context

PROC-STORY-DOCS-1 established that a governed agent should emit two durable DRAFT
documentation files per story at completion time:

- `docs/<story-id>/technical/<story-id>-dev.md` (developer audience)
- `docs/<story-id>/user/<story-id>-guide.md` (user audience)

These files are meant to be reviewed and refined during the PR, not authored by hand
from scratch. The rule's default option (`per-story-docs`) is the only active variant;
the other three options (`living-central-docs`, `adr-per-change`, `mechanical-minimum`)
are defensible alternatives the project considered but did not adopt.

Prior to this work, the rule existed in the corpus but had no code implementation that
actually wrote the files.

## Decisions

### 1. Where does the trait live? (crates/agent vs crates/server vs crates/core)

**Chosen:** `crates/agent/src/post_story_hook.rs`

**Rationale:** The agent crate is the natural home for anything that touches the
post-run completion path. It already defines the `AgentDriver` seam that governs what
happens during a run; the post-story hook is the symmetric "after the run" seam. Placing
the trait here also makes it available to the fleet and CLI paths without a dep on the
heavier `camerata-server` crate. An alternative was `crates/core`, but that crate is
intentionally zero-I/O (no filesystem writes), so a hook that writes files does not
belong there.

### 2. Content assembly: deterministic vs LLM-polished

**Chosen:** Deterministic by default. LLM polish is behind a flag (not yet implemented).

**Rationale:** Determinism makes the emitter hermetic and unit-testable without stubs
or mocks. The content is assembled entirely from the `StoryCompletion` snapshot (story
id, decision records, run summary, sign-off timestamp). An LLM polish pass could enrich
the narrative but would require a model call, add latency, and make tests non-hermetic.
The PROC-STORY-DOCS-1 directive says these are DRAFTs for the architect to refine in
the PR — a deterministic starting point is more honest and equally useful.

A `PostStoryHook` implementor that adds LLM polish is straightforward: implement the
trait, wrap `StoryDocEmitter`, and call a model after the deterministic assembly. The
architecture deliberately leaves this seam open without requiring it.

### 3. Where is the hook called? (UowStore::sign_off vs UowStore::finish_development vs sign_off_run handler)

**Chosen:** Inside `UowStore::sign_off`, after the sign-off is persisted and flushed.

**Rationale:** `sign_off` is the explicit, never-automatic QA gate — the true
"story complete" moment in the governed lifecycle. `finish_development` transitions to
`AwaitingQa` but does not mean the work is done; the architect's sign-off is required.
The handler in `lib.rs` would also work but would duplicate logic across every path
that can trigger sign-off (today just the API, but possibly the fleet in the future).
Keeping it inside `UowStore::sign_off` is the single-point-of-truth approach.

Hook failures are intentionally non-fatal: the sign-off is already persisted before the
hook fires. A doc-write failure logs to stderr and the caller receives the signed-off
UoW unchanged.

### 4. How is the convention choice enforced?

**Chosen:** The chosen `DocConvention` variant is stored on the `StoryDocEmitter` at
construction time. For non-default conventions the `emit` method returns `Ok(vec![])`.

**Rationale:** The project's PROC-STORY-DOCS-1 selection lives in
`Project.ruleset.process[*].chosen_option` (a `RuleSelection`). At the point where
`UowStore::sign_off` fires, the store does not have access to the `ProjectStore`. The
cleanest boundary is to resolve the convention once (during `AppState` construction,
where both stores are available) and bake it into the `StoryDocEmitter` that is
attached via `with_story_doc_hook`. This avoids threading `ProjectStore` into `UowStore`
and keeps the hook path synchronous and simple.

### 5. How is the workspace root passed to the hook?

**Chosen:** Stored on `UowStore` via `with_workspace_root`, defaulting to the cwd
(`PathBuf::from(".")`) if not set.

**Rationale:** `UowStore::sign_off` is a `&self` synchronous method. It cannot reach
into `AppState.settings` (no back-reference). The workspace root is set once during
server startup (in `AppState::from_env`) and is stable for the lifetime of the process.
Storing it on the `UowStore` is the same pattern used for `artifacts` and `runtime`.

A `None` workspace root falls back to the cwd, which will cause the doc write to fail
gracefully if the cwd is not the repo root — a non-fatal hook failure is already logged
and does not block sign-off.

### 6. What is the Doc emission format?

**Chosen:** Markdown with a YAML-style front-matter header, audience-appropriate sections,
and `DRAFT` badges. Technical doc: run summary + decisions table + implementation-notes
stub. User guide: what-changed + approved-decisions summary + how-to + migration stubs.

**Rationale:** Both files should be immediately recognisable as DRAFT starting points,
not polished documentation. The `DRAFT` badge + editorial prompts (`_(Fill in: ...)_`)
signal to the architect that revision is expected. The front-matter header carries
`story_id` and `signed_off_at` for traceability. Splitting technical vs user content
keeps developer rationale out of the user guide, which is the PROC-STORY-DOCS-1 intent.

## Files changed

- `crates/agent/src/post_story_hook.rs` (new) — `PostStoryHook` trait, `StoryCompletion`
  struct, `DocConvention` enum, `StoryDocEmitter` impl, 27 unit tests.
- `crates/agent/src/lib.rs` — `pub mod post_story_hook` + re-exports.
- `crates/agent/Cargo.toml` — added `camerata-worktracker`, `chrono` deps; `tempfile`
  dev-dep.
- `crates/server/src/uow.rs` — `post_story_hook` + `workspace_root` fields on
  `UowStore`; `with_story_doc_hook` + `with_workspace_root` builders; hook call in
  `sign_off`; 5 integration tests.
- `crates/server/Cargo.toml` — added `camerata-agent` dep; `tempfile` dev-dep.

## Alternatives not taken

- **Trait in `crates/core`:** core is zero-I/O; filesystem-writing hooks do not belong
  there.
- **Hook in the HTTP handler (`sign_off_run`):** would require repeating the hook call
  in every future path that can trigger sign-off.
- **LLM polish by default:** non-hermetic, adds latency, conflicts with the
  "DRAFT for the architect to refine" philosophy.
- **Passing `ProjectStore` to `UowStore::sign_off`:** over-coupling; the convention is
  a construction-time choice, not a per-call parameter.
