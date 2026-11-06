use super::*;

use camerata_ui_core::order_preset::{order_findings, order_rows, Grouping, OrderPreset};
use camerata_ui_core::pane_context::{ContextTarget, PaneContext};
use camerata_ui_core::stack::{is_stackable, rows_of_group, StackOrigin};
use camerata_ui_core::triage::focused_key;

// csv_field moved to camerata-ui-core::rules (shared by rules_csv there and findings_csv here).
pub(super) use camerata_ui_core::rules::csv_field;

// Scan-surface helpers (human_tokens, det_tool_label, default_finding_status)
// now live in the framework-agnostic core (RUST-HEADLESS-CORE-1); re-exported so the scan/cockpit
// call sites are unchanged. `default_finding_status` is re-exported via the triage block below
// (FindingView moved to core::triage), so it's not repeated here.
pub(super) use camerata_ui_core::scan::{det_tool_label, human_tokens};

// The onboarding-triage state machine + its data shapes now live in the framework-agnostic core
// (RUST-HEADLESS-CORE-1): FindingView, Disposition (+ its Default), TriageState, TechDebtBucket,
// finding_key, and the pure TriageModel + its TriageCounts tally. Re-exported so every existing
// scan/cockpit call site (and cockpit's `pub use scan::*`) is unchanged; the Dioxus layer keeps
// the signals, table effects, toasts, and rsx. (FindingView's `default_finding_status` serde
// provider lives with it in core::triage — re-exported from core::scan; not needed bare here.)
//
// `finding_state` is no longer re-exported: Pass 2 step 3 replaced this file's last caller (the
// hand-rolled disposition tally in `ScanResults`) with `TriageModel::counts`, and a re-export
// nothing imports is a warning, not an interface.
pub(super) use camerata_ui_core::triage::{
    finding_key, Disposition, FindingView, TechDebtBucket, TriageCounts, TriageModel, TriageState,
};

// Pass 2 step 1 (the pass-2 curator UI design notes): the five-kind taxonomy's
// surface routing, the Kind badge's wire tag, and the one-rule-one-surface invariant. All pure,
// all in core; this layer only renders them.
pub(super) use camerata_ui_core::triage::{
    findings_on_surface, kind_tag, rules_on_multiple_surfaces, surface_counts,
    unspecified_kind_count, KindSurface,
};

// Pass 2, design §8: THE RULE HUB. `rule_hub` is the one join — everything this scan knows
// about one (repo, rule) — and `RuleHubView` is already the flat owned props shape, so the
// strip that renders it needs no second DTO. The conforming count comes through as two
// integers deliberately: the conforming sites are not in the data, only the counts.
pub(super) use camerata_ui_core::triage::{rule_hub, RuleHubView};

// Pass 2 step 2: the MANUAL column sort over the curated default. `severity_sort_value` is the
// Severity column's cell value — the authored severity carrying its calibrated ladder position,
// because chorale sorts a column by exactly that value and 0.2.3 offers no sort-key hook. See
// the function's docs in core for why that is not a second calibration layer.
pub(super) use camerata_ui_core::triage::{severity_sort_value, severity_sort_values};

// Design §9.3: the CONFIDENCE column's cell value and its filter vocabulary, from the same
// closed set, so the two cannot drift. Structured fields only — `confidence_tag` is the
// replacement for a cell value that OR'd two flags with a regex over the model's English.
pub(super) use camerata_ui_core::triage::{confidence_tag, confidence_tags};

// Design §3/§9.2: the heading the ON-DEMAND reason prompt shows. It names the disposition AND
// the count, which the two always-visible textareas it replaces could not — a box beside a
// selection cannot say how many rows the button next to it will move.
pub(super) use camerata_ui_core::triage::bulk_reason_heading;

// Work order B.5/B.7: decision memory (what the draft already decided about a shape) and the
// progress strip's words. Both are pure and live in core; this layer draws them.
use camerata_ui_core::decision_memory::{
    prefill_reason, prior_decisions, shared_prefill, PriorDecision,
};

use camerata_ui_core::progress::{progress_text, FP_MEANING};

// Pass 2 step 2: the DEVIATION GROUP HEADER. `rule_group_label` is the Queue's group-by value
// (rule id, plus the structured sentence when the row is a deviation), so chorale renders the
// sentence as the group header's own label; `deviation_groups` is the same partition lifted out
// for the rule-level bulk actions. Both are pure and structured-fields-only — there is no prose
// path in either, and there must never be one.
pub(super) use camerata_ui_core::triage::{deviation_groups, rule_group_label, DeviationGroup};

pub(super) use camerata_ui_core::triage::{finding_location_label, repo_counts_line};

// Pass 2 step 4 (THE DOCKED DETAIL PANE, design §2 + the owner's Q5 ruling in §13). All pure
// and all in core: the row cursor that arrow keys drive, the SCOPED key handler (the whole of
// Q5's "only fire while the detail pane holds focus" clause lives in `pane_command`), and the
// five-second undo. This layer owns the focus signal, the keydown wiring and the rsx; it owns
// none of the rules.
use camerata_ui_core::palette::{
    entries as palette_entries, palette_gate, stale_note, PaletteAction, PaletteContext,
    PaletteEntry,
};

pub(super) use camerata_ui_core::triage::{
    deviation_headline, next_pane_focus, pane_command, pane_keys_armed, site_count, state_hotkey,
    KeyMods, MergedSiteView, PaneCommand, PaneFocus, RowCursor, UndoEntry, UndoOutcome,
};

// Pass 2, THE GATED CONVENTION COMMIT (design §3, Q3 + Q4 in §12/§13). Every rule about what
// is adoptable, what is adopted, what was actually OPENED and whether the gate is shut lives
// in `camerata_ui_core::adopt`, unit-tested with no VirtualDom. This layer owns the pixels,
// the two HTTP calls and the focus handling; it owns none of the decisions.
pub(super) use camerata_ui_core::adopt::{
    adoptable_conventions, adoption_key, commit_gate, merge_adoptions, split_rules,
    AdoptableConvention, AdoptedRule, AdoptionModel, CommitPlan, ProjectRulesetView, SplitRule,
};

mod audit_calls;
mod banners_panels;
mod exports;
mod findings_table;
mod onboard;
mod queue_table;
mod repo_health;
mod rescan;
mod scan_results;
mod wire;
pub use audit_calls::*;
pub use banners_panels::*;
pub use exports::*;
pub use findings_table::*;
pub use onboard::*;
pub use queue_table::*;
pub use repo_health::*;
pub use rescan::*;
pub use scan_results::*;
pub(super) use wire::*;

/// The export dialog offers the triage log as an unticked box (off by default).
#[cfg(test)]
mod export_dialog_triage_log_box;

/// The inspection setup's client-name field follows the project it is shown for. The field keeps
/// its own text while it is on screen, so without a key a switch from one project to another
/// reuses the first project's text and the next save posts it under the second project's id.
#[cfg(test)]
mod client_name_setup_follows_the_project;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod render_tests;

// ─────────────────────────────────────────────────────────────────────────────
// req_pass2_11 — THE DOCKED DETAIL PANE, rendered (design §2; Q5 ruling §13)
// ─────────────────────────────────────────────────────────────────────────────
//
// SSR cannot press a key, so the pane is a props-only component whose rendered output
// MANIFESTS the two things a keypress test could otherwise only assert about a helper: which
// row it is describing, and whether the keys are armed. The armed indicator is driven by
// `pane_keys_armed`, which is DEFINED as `pane_command(focus, "c", …).is_some()` — so a pane
// that renders "armed" while the handler would refuse is not reachable.
#[cfg(test)]
mod pane_render_tests;

#[cfg(test)]
mod rule_hub_tests;

// ─────────────────────────────────────────────────────────────────────────────
// req_pass2_14 — THE GATED CONVENTION COMMIT, rendered (design §3; Q3 + Q4)
// ─────────────────────────────────────────────────────────────────────────────
//
// SSR cannot type into an input or press a button, so everything here is a props-only
// component rendered in a state and read back out of the HTML — the same discipline the detail
// pane, the process gate and the bulk bar were built on.
//
// What the pure half already proves, in `camerata_ui_core::adopt`'s own suite: that the gate
// refuses twelve near-misses, that the unopened count follows the open gesture one at a time,
// that a deviating convention is adoptable, and that the ruleset round trip preserves every
// field. What THIS module adds is the half that only a render can show: that the component
// actually consults those functions rather than describing them.
#[cfg(test)]
mod convention_commit_tests;

// ─────────────────────────────────────────────────────────────────────────────
// Work order B.4 — THE ORDER PRESETS, mounted
// ─────────────────────────────────────────────────────────────────────────────
//
// The ordering itself is unit-tested in `camerata_ui_core::order_preset`. These mount the real
// `FindingsTable` and check the three things only a mounted table can show: each preset cuts the
// rows and the groups the way it says, switching preset on a live table keeps the cursor on the
// same finding, and the choice is part of the draft.
#[cfg(test)]
mod order_preset_tests;

/// B.4 check 1, the reload half: a saved draft that names a preset brings the Queue back cut
/// that way, and the next save carries it. Mounts the real results view against a fake BFF.
#[cfg(test)]
mod order_preset_reload_tests;

// ─────────────────────────────────────────────────────────────────────────────
// Work order B.4 follow-ups: wording, focus, scroll, a decided row, an unknown draft value
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod order_preset_followup_tests;
