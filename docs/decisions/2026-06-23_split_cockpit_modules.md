# 2026-06-23: Split cockpit.rs into cockpit/ submodule directory

## Context

`crates/ui/src/cockpit.rs` grew to 14,166 lines — a single-file monolith containing
the entire cockpit UI surface: rules management, scan/onboard flows, UoW governance,
and live-run panels.

## Decision

Split the file into a Rust module tree:

- `cockpit.rs` — module root: shared types, project/usage fetch fns, shell components
  (CockpitApp, CockpitShell, CockpitNav, UsageMeter, …), model selector, docs view,
  deep-report export, the full `#[cfg(test)]` block.
- `cockpit/rules.rs` — all rules-management UI (CustomRulesTable, SuppressionsPanel,
  ProjectRulesTable, AllRulesTable, RulesDetailModalHost, TierMapEditor,
  StepModelsEditor, StallThresholdsEditor, RulesView, RuleCount, ProposedRulesTable,
  RuleDetailModal, CustomRulesPanel, SingleRuleEditor, RuleDriftNotice).
- `cockpit/scan.rs` — scan/onboard UI (RepoHealthPanel, OnboardView, GreenfieldForm,
  GreenfieldResultView, ScanResults, DeterministicProgress, FindingsTable).
- `cockpit/uow.rs` — UoW governance (GovernedDevPage, IssueManagementPanel,
  WorkItemTable, WorkItemDetail, CreateOrOpenUow, NewAuthoredUowButton,
  StoryAuthoringPanel, UowDevControls, UowUpdateBranchControl, UowPrControl,
  UowStepRunControls, UowPanel, CiRulesPanel).
- `cockpit/live_run.rs` — live execution (LiveRunPanel, RunClarificationPrompt,
  RunProvenancePanel, ClarifyQuestion, NeedsYouQueue).

## Rationale

- **Readability**: navigating 14K lines in a single file is untenable; the four
  functional clusters are conceptually independent.
- **Edit surface**: each developer feature touches one submodule, not the monolith.
- **NOT compile-time**: Rust compiles crate-level, not file-level. This split has
  zero effect on incremental compile times.

## Re-export strategy

`cockpit.rs` re-exports each submodule with `pub use rules::*; pub use scan::*;
pub use uow::*; pub use live_run::*;`. The `#[cfg(test)]` module stays in the root
and uses `use super::X` paths, which resolve through the re-exports — no test
migration needed.

Shared types (RunView, RunGateEvent, StartRunOutcome, ProviderView, TierMapView,
ProjectView, RuleSelectionView, CustomRuleView, RulesetView, enc_seg, FeatureFlagMap,
etc.) stay in the root as `pub(crate)`.

## Status

Implemented. All 110 tests pass.
