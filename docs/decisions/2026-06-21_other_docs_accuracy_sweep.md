# Other Docs Accuracy Sweep — 2026-06-21

Accuracy pass on public/user-facing docs OUTSIDE USER_GUIDE.md and TECHNICAL.md.
Verified against crate source before each change; no edits made without code confirmation.

---

## Per-file results

### README.md
**Changed.** Line describing onboarding's story-emission said "a 'wire mechanical rules
into CI' task" (singular). Code in `crates/server/src/lib.rs` (`create_ci_story`,
`POST /api/onboard/ci-story`) produces TWO distinct stories — one for mechanical
(off-the-shelf linter) rules and one for architectural (custom-checker) rules — confirmed
by the `"mechanical"` / `"architectural"` tier branch in the handler and by UI text at
cockpit.rs:8605. Fixed to "up to two CI-wiring tasks (one for mechanical rules mapping to
off-the-shelf linters, one for architectural rules requiring custom checkers)."

### docs/RATIONALE.md
**Changed.** Section 2 described Layer 2 as only running `cargo fmt`, `cargo clippy`,
and `cargo test`. `crates/checks/src/multilang.rs` ships cross-language, polyglot,
repo-pinned, fail-closed runners: `JsCheckRunner` (npm run lint + npm run test),
`PythonCheckRunner` (ruff + pytest in `.camerata-venv`), `GoCheckRunner` (gofmt + go vet +
go test). The `runner_for_worktree` selector is wired at the fleet injection point.
Updated to enumerate all four language runners.

### docs/ENFORCEMENT.md
**Changed.** Lane 2 subsection listed only `RUST-FMT` and `RUST-CLIPPY`, and the
prose only mentioned `RustCheckRunner`. The summary table row for `Layer-2 (checks)`
also listed only three Rust rule ids.

Fixes applied:
1. Lane 2 subsection: replaced the two-row Rust-only table with a six-row polyglot
   table (`RUST-FMT`, `RUST-CLIPPY`, `RUST-TEST`, `LAYER2-JS-CHECKS-1`,
   `LAYER2-PY-CHECKS-1`, `LAYER2-GO-CHECKS-1`). Added cross-language description,
   fail-closed / repo-pinned / polyglot properties, and test references.
2. Lead-in count: "plus three have layer-2 enforcement" updated to "plus six have
   layer-2 enforcement (three Rust + one JS/TS + one Python + one Go)."
3. Summary table row: updated to list all six Layer-2 rule ids with a cross-language
   description.

### docs/ARCHITECTURE.md
**No changes needed — verified current.** Layer-2 description is intentionally
high-level ("lint / AST / rule audit"), no Rust-specific tooling named. No WorkItem/UoW
or CI-wiring staleness detected.

### docs/VISION.md
**No changes needed — verified current.** Purely conceptual / directional; none of the
recent changes contradict it.

### docs/CONSUMER_UX.md
**No changes needed — verified current.** Consumer-mode spec; not touched by governed-dev
WorkItem/UoW rebuild, multilang layer-2, or SOC-2 flag changes.

### docs/RATIONALE.md (beyond Layer-2 change above)
**No further changes.** The rest of the doc (gate architecture, provider neutrality,
interaction design, scope limits) is accurate.

### docs/PROVIDER_NEUTRALITY.md
**No changes needed — verified current.** Gate provider-neutrality proof is structural and
unchanged; multilang runners don't affect this.

### docs/GITHUB_SETUP.md
**No changes needed — verified current.** Documents the GitHub provider wiring for the
lower-level story spine (`/api/stories/adopt`). That route still exists in
`crates/server/src/lib.rs:259`. The new WorkItem/UoW layer (`/api/uow/from-workitem`)
is a separate, additive governed-dev surface — it doesn't replace the underlying spine.
"Adopting a story" in this doc refers to the story-spine endpoint, not the UoW flow.
The doc's scope is GitHub auth and provider selection, which is unchanged.

### docs/RULE_COVERAGE.md
**No changes needed — verified current.** Discusses the CORPUS coverage axis (rule
authoring + full AST-level JS/TS CheckRunner with eslint custom rules). `multilang.rs`
ships a basic `JsCheckRunner` (npm run lint/test) but not the full eslint+tsc+custom-AST
vision the doc describes as the milestone. RULE_COVERAGE.md's "The task" framing remains
accurate: the moat-level JS/TS CheckRunner is not yet shipped.

---

## Skip list — not touched

USER_GUIDE.md, TECHNICAL.md, BACKLOG.md, DEBT_INVENTORY_*, PHASE0_*, PHASE0_TASKS.md,
UI_TASKS.md, UI_BACKLOG.md, UI_DESIGN.md, TECH_DESIGN.md, SESSION_*, TEST_WHEN_BACK.md,
LIVE_RUN_VERIFICATION.md, CORPUS_AUDIT_*, DEMO_COMMANDED_VIOLATION.md,
RUST_CORE_VERIFICATION.md, WORKTRACKER_INTEGRATION.md — none were read or modified.
