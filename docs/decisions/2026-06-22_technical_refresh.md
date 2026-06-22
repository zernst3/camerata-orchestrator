# TECHNICAL.md refresh — features shipped 2026-06-21 / 2026-06-22

**Date:** 2026-06-22 · **Scope:** `docs/TECHNICAL.md` only (+ this doc).

This pass brought `docs/TECHNICAL.md` current with the features shipped since the last accuracy
pass. Every added/updated claim was verified against the shipped code (`crates/server`,
`crates/ui/src/cockpit.rs`, `crates/rules`, `crates/checks`, `crates/fleet`) and cites the file
the change lives in; the 2026-06-21/2026-06-22 decision docs were used as the certified design
source and confirmed against code.

## Sections added

1. **§5 — Two opt-in/tier schema flags (`opt_in_only`, `layer3_only`).** `RuleToml`/`Rule`
   booleans, `#[serde(default)]` false, accessors `is_opt_in_only()`/`is_layer3_only()`, propose
   logic ANDing `!r.is_opt_in_only()` in `onboard.rs`. Verified `crates/rules/src/lib.rs` and
   `crates/server/src/onboard.rs:924-928`.
2. **§5 — The two CI/CD security rules.** `CICD-SEMGREP-SECURITY-SCAN-1` (layer3_only false) and
   `CICD-CODEQL-SECURITY-SCAN-1` (layer3_only true), both mechanical / opt_in_only / no default.
   Verified `crates/rules/principles/ci-cd/cicd-{semgrep,codeql}-security-scan-1.toml`.
3. **§6 — Scan-time deterministic preview.** `Finding.preview`/`preview_tool`, `scan_tools`
   module (linter→tool via `tool_for_rule`/`tool_for_linter`, `group_by_tool`, `selector_for_linter`,
   SARIF/JSON parsing for clippy/ruff/eslint/semgrep end-to-end, graceful `note_finding`,
   layer3_only excluded), `merge_scan_preview` at both audit entry points, preview = advisory
   (`status = suppressed-baseline`) vs the repo-pinned gate. Verified `crates/server/src/scan_tools.rs`
   and `crates/server/src/lib.rs` (`merge_scan_preview`, `split_scannable_rules`).
4. **§6 — Scan-type selector + deterministic progress.** `AuditReq.run_ai_review`/`run_deterministic`
   (default-true), `effective_scan_modes` (both-false → both-true), `audit_repos` gating
   (deterministic-only = zero model calls), `JobState.deterministic: DetProgress` + `JobStore`
   det_* methods, `DeterministicProgress` UI above the AI drawer, deterministic-only → job path.
   Verified `crates/server/src/lib.rs`, `onboard.rs`, `jobs.rs`, `crates/ui/src/cockpit.rs`.
5. **§6 — CI-wiring targets the repo's canonical check command (both layers).** Updated the
   `onboard_ci_rules` paragraph. Verified `crates/server/src/lib.rs:2993,3025,3058`.
6. **§3 — Layer-2 bootstrap bypass (`skip_layer2`).** `StartRunReq.skip_layer2`, `layer2_runner`
   → `NoopChecks`, additive fleet entry points, layer-1 + decisions gate untouched, UI toggle.
   Verified `crates/server/src/lib.rs:510`, `crates/fleet/src/lib.rs:70`.
7. **§3 — Ruby/Java/C# layer-2 runners.** Completed the multilang runner list to all seven
   languages. Verified `crates/checks/src/multilang.rs` (RubyCheckRunner/JavaCheckRunner/
   CSharpCheckRunner present).
8. **§10 — AI story-authoring.** `/api/uow/blank`, `/author`, `/publish`; `AuthoringState` +
   `work_item` fields; draft-id-no-rekey; reuse of `onboard::create_issue` + `Llm`; no gate in
   this path. Verified `crates/server/src/uow.rs`, `crates/server/src/lib.rs:357-359`.
9. **§10 — AI-assisted Update-branch.** `/branches` + `/update-branch`, merge→conflict→gated-agent
   flow reusing `governed_role` + `prepare_session`, fail-closed. Verified
   `crates/server/src/update_branch_run.rs`, routes `lib.rs:367-368`.
10. **§10 — Work-item comments/assignees endpoints.** `/api/workitems/comments`,
    `/api/workitems/assignees`. Verified `crates/server/src/lib.rs:352-353`.
11. **§11 — Governed Development page** updated for the project-settings gear popup (loop-guard +
    tier-map moved out of `UowDevControls`), the open-work-item modal + comments + `@`-mention
    composer, the bootstrap toggle, the Update-branch control, and `NewAuthoredUowButton`/
    `StoryAuthoringPanel`. Verified `crates/ui/src/cockpit.rs` (`ProjectSettingsGear`,
    `NewAuthoredUowButton`, `StoryAuthoringPanel`, `DeterministicProgress`).

## Sanity-checked (unchanged, confirmed accurate)

- Stepped runs / tiering / `delegate` (§10) — endpoint shapes and depth guards match
  `live_fleet.rs` / `gateway/src/delegate.rs`.
- WorkItem / UoW (§10) and the 7-language layer-2 coverage table (§5a Axis B) — match
  `workitems.rs` / `uow.rs` / `checks`.

## Flags / unverifiable

- None invented. The live `create_issue` HTTP publish path and the live `claude -p` paths are
  exercised only with a real token / `CAMERATA_LIVE_BUILD=1`; the doc states their token-free
  graceful-degradation behaviour, which IS covered by tests, and does not assert live behaviour as
  separately verified here.
