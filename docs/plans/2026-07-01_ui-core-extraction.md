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

- **Session update 2026-07-01 (post-#115-merge).** PR #115 merged; `cockpit.rs` no longer collides.
  Six more beachheads landed on `feature/ui-core-extraction`, each green with coverage preserved 1:1.
  Running totals: **ui-core 50 tests, ui 516, total 566** (unchanged from the pre-extraction 566).
  Beachheads that filled the ui-core modules:
  3. `cockpit/scan.rs` `human_tokens` + `det_tool_label` + `default_finding_status` (+3 tests, incl. the
     merged `cockpit.rs` `det_tool_label` "ruff" duplicate) -> `camerata_ui_core::scan`.
  4. `cockpit/rules.rs` `verif_badge` + `split_needs_review` (+8 tests) -> `camerata_ui_core::rules`.
  5. `cockpit.rs` `format_idle` (+1 test) -> `camerata_ui_core::run` (`pub(crate)` re-export so the
     `live_run`/`scan` descendants keep working).
  6. `cockpit.rs` run cluster: `live_event_style` + `run_is_cancellable` + `run_stall_banner_visible`
     (+4 tests) -> `camerata_ui_core::run`.
  7. `cockpit/scan.rs` `estimate_audit_cost` (the full pricing model) + **all 15** pricing tests
     (consolidated from the cockpit + scan test modules, incl. the 400k/20 monotonicity cases) ->
     `camerata_ui_core::scan`. Also dropped a stray orphaned `human_tokens` doc-comment and the
     now-unused run-cluster test imports; cleared the pre-existing #115 `design.rs` unused-import warning.
  Commits are LOCAL (fb3b456, f0e14ce, 19818b4, and predecessors), NOT pushed (per Zach: push tonight).

- **Next resume point — the rules view-model cluster (largest remaining frontend beachhead).**
  `bucket_of` + `rules_csv` are thin, but they hang off `ProposedRuleView`, a *central* view type:
  embedded in `scan.rs`'s `ScanReportView` (`proposed_rules: Vec<ProposedRuleView>`), constructed via
  `to_proposed()`, and referenced ~10x across `scan.rs` plus several `rules.rs` response types. The
  cluster to move together is `SelectionBucket` + `bucket_of` + `RuleOptionView` + `RuleSourceView` +
  `ProposedRuleView` + `default_draft` + `rules_csv` (all dioxus-free serde/pure — same category as the
  `models` move). **Caveat discovered:** `rules_csv` depends on `csv_field`, defined in `cockpit/scan.rs`
  and *shared* with scan.rs's own finding-CSV export — so moving `rules_csv` also pulls `csv_field` into
  ui-core (re-export back to scan.rs). That makes it a two-file re-export move (rules.rs + scan.rs), the
  biggest so far. Mechanical + compiler-verified, but wants a fresh budget, not a long-session tail.
  After it: the `use_*` hook state-lift (the genuinely structural part).

- **Phase 2 beachhead landed (2026-07-08, commit `206a067`, branch `feat/adapter-ladder-headless-core`):
  the governed-dev state lift.** This is the first Phase 2 surface (a `use_*` hook state-lift, not just
  a pure-function move): the three governed-dev `GlobalSignal`s (`UOW_LAST_SEEN`, `UOW_CHANGED`,
  `PULLED_WORK_ITEMS`) collapsed into one `GovDevState` TEA model in `camerata_ui_core::govdev`, driven
  by a `GovDevMsg` enum (`PollObserved`/`PulledLatest`/`WorkItemsPulled`) through a single pure `apply()`
  reducer, with selectors (`is_changed`/`changed_count`/`pulled_for`/`assignee_label`) replacing direct
  signal reads. The poll/change-detection logic that used to live in the view is now a headless,
  unit-tested transition; the Dioxus adapter holds one `GlobalSignal<GovDevState>` and only translates
  events into messages. `WorkItem` moved to `camerata-api-types` so the core can hold it without a
  framework dependency. `ui-core` stays dioxus-free throughout. Driven out of the 2026-07-04 Fable 5
  audit's GAP-3 escalation, not this plan's own sequencing, so it landed out of order relative to the
  rules view-model cluster above; that cluster and the rest of the surface-by-surface state lift remain
  open. See `docs/decisions/2026-07-08_adapter-ladder-and-headless-core.md` for the full batch this rode
  in with (GAP-1/GAP-3/GAP-7).
