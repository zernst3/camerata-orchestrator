# UI core extraction — make the cockpit RUST-HEADLESS-CORE-1 compliant

Status: IN PROGRESS (Phase 0 started 2026-07-01)
Tracks: issue #116 (under Epic #70 Tech Debt)
Rules: `RUST-HEADLESS-CORE-1` (structure) + `RUST-PURE-STATE-TRANSITIONS-1` (form)

## 1. Goal

The cockpit UI (`crates/ui`) currently holds its logic, state, and network calls directly inside Dioxus
components (573 `use_signal`/`use_context`/`use_resource` sites, no framework-agnostic core). This
extraction moves that logic and state into a new **`camerata-ui-core`** crate that has **no dependency
on Dioxus**, leaving `crates/ui` as a thin adapter that renders core state and dispatches inputs. The
compiler then guarantees the core is renderer-free, and the bulk of UI logic becomes unit-testable with
no VirtualDom.

## 2. Target architecture

```
  camerata-ui-core   (NEW, no dioxus dep — the compiler enforces it)
     ├─ data shapes   : the BFF view types (serde structs deserialized from the API)
     ├─ pure logic    : schedule build/parse, model-group building, schema mutations,
     │                  diff summaries, validation, formatting — no I/O, no framework
     ├─ state + transitions : per-surface State structs + pure `fn(state, input) -> state`
     └─ request/parse : build a request's shape + parse a response (pure); the actual
                        reqwest call is a side effect performed by the adapter

  crates/ui  (Dioxus ADAPTER — depends on camerata-ui-core)
     ├─ components     : rsx! + use_signal holding ONE core State per surface
     ├─ event handlers : translate a UI event into a core input, apply the pure transition
     ├─ side effects   : the actual reqwest calls + spawn (at the edge), then feed results
     │                   back into the core state
     └─ render         : project core state to rsx
```

**What goes in the core:** anything that does not need Dioxus — data types, pure functions, state
structs, pure transitions, request-building and response-parsing.

**What stays in the adapter:** `rsx!`, `use_*` hooks, the reqwest side-effect calls, `spawn`, toasts,
and rendering. Handlers become thin: map event to input, call a core transition, perform any side
effect at the edge, fold the result back into state.

## 3. Testing principle (non-negotiable — Zach, 2026-07-01)

**Coverage must not regress.** Every existing test's intent is preserved. Concretely:

- Logic that moves to the core is tested in the core with **the same assertions**, now as plain unit
  tests with no VirtualDom (most of today's pure-logic tests translate 1:1 — e.g. the `schedule` and
  `chat_model_groups` tests).
- State behavior that today is only reachable through an SSR render (or not tested at all because it
  was trapped in a component) becomes a **direct unit test of the pure transition** — this is where the
  architecture lets us make coverage STRONGER, not weaker.
- Components keep a **light SSR render test** for structure only (the shape renders, the key elements
  are present) — the Tier-1 pattern in `docs/UI_TESTING.md`. The behavior that used to be asserted
  awkwardly through SSR moves to the core unit test.
- Net effect: the same or better coverage, faster and far less brittle. A move is not "done" until the
  moved logic's tests moved with it and pass.

Do NOT delete a test to "simplify" the move. Translate it. If a test cannot be translated, that is a
signal the logic was not cleanly extracted — fix the extraction, do not drop the test.

## 4. Phasing (collision-aware)

PR #115 (overnight design-page work, still open) touches these UI files, so Phase 0 AVOIDS them:
`cockpit.rs`, `design.rs`, `main.rs`, `style.rs`, `workspace.rs`.

- **Phase 0 (now, this branch `feature/ui-core-extraction`):** stand up `camerata-ui-core` and extract
  the cleanly-pure logic (with its tests) from files #115 did NOT touch. Start with the self-contained
  pure functions: `routines.rs` schedule build/parse, `chat.rs` model-group building, and the other
  already-pure helpers across the non-colliding files. Prove the crate boundary + the test-translation
  pattern. Keep `cargo test --workspace` green.
- **Phase 1 (after #115 merges):** extract the colliding surfaces (`cockpit.rs`, `design.rs`,
  `workspace.rs`) the same way, now that their code is on main.
- **Phase 2 (surface by surface):** the state lift — replace each surface's in-component state with a
  core `State` struct + pure transitions; the component holds one signal and dispatches inputs. This is
  where the 573 hooks shrink. Do it one surface at a time, keeping green, translating tests as you go.

## 5. Per-surface recipe (repeatable)

1. Move the surface's pure functions + their tests into `camerata-ui-core`; re-import in the adapter.
2. Define a `SurfaceState` struct and pure `fn apply(state, input) -> state` transitions in the core;
   unit-test the transitions directly.
3. In the component, replace the scattered signals with one `use_signal(SurfaceState::default)`; each
   handler builds an input, calls `apply`, performs any side effect at the edge, folds the result back.
4. Translate the surface's tests: pure-logic + transition tests in the core; a light SSR structure test
   in the adapter. Confirm no coverage was lost.

## 6. Acceptance

- `crates/ui-core/Cargo.toml` has NO `dioxus` dependency (compiler-enforced renderer-free core).
- The majority of UI logic is unit-tested in `camerata-ui-core` with no VirtualDom.
- Total UI-related test count is >= today's, with the same behavioral coverage (translated, not dropped).
- `crates/ui` shrinks toward rendering + wiring; per-surface `use_*` hook density drops materially.

## 7. Status log

- **Phase 0 started 2026-07-01** on `feature/ui-core-extraction`: `camerata-ui-core` crate created.
  Two beachheads landed, both green with coverage preserved 1:1 (total UI-related tests unchanged at
  560; `ui` 560 -> 541, `ui-core` 0 -> 19):
  1. `routines.rs` schedule build/parse + `WEEKDAYS` + 11 tests -> `camerata_ui_core::schedule`.
  2. `chat.rs` `ModelOption`/`ModelsResp`/`grouped`/`chat_model_groups` + 8 tests ->
     `camerata_ui_core::models` (fields made `pub` for the cross-crate adapter).
  Commits local (9f38e52, 122f993, 424549d), NOT pushed (per Zach: push tonight).
- **Finding (2026-07-01): the CLEAN non-colliding self-contained beachheads are now exhausted.** The
  remaining extractions are entangled and should NOT be forced now:
  - Some touch the COLLIDING `cockpit.rs` (e.g. `det_tool_label` has a test in `cockpit.rs`, which
    #115 modified). Extracting them would edit `cockpit.rs` and conflict with PR #115.
  - The valuable logic (`AuditModelsResp` + `grouped`/`vision_grouped`; `rules.rs` `build_change_summary`,
    the `ColumnDef` extractors, `verif_badge`; `scan.rs` triage) drags cross-cutting view types
    referenced across many cockpit files. These are the same pattern as the model beachhead but touch
    5+ files each (compiler-verified, so mechanical, but larger).
  - Duplicate model types exist to dedup: `routines.rs` and `cockpit/scan.rs` each carry their own
    `ModelsResp`/`AuditModelsResp`; a unified `camerata_ui_core::models` can absorb them.
- **Recommended sequencing:** land PR #115 first, then do the cross-cutting extractions (including the
  `cockpit.rs`/`design.rs`/`workspace.rs` surfaces) in one focused pass, so nothing collides and the
  design-page state is extracted at the same time. Given the volume (~150 pure-logic tests across the
  cockpit files), a dedicated push (a focused session or an extraction routine like the design-page one)
  is the right vehicle for the bulk.
