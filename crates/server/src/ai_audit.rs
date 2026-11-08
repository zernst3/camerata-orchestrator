//! The AI architectural audit — the half of brownfield that genuinely needs a model.
//!
//! The deterministic scan ([`crate::onboard::audit_files`]) catches MECHANICAL
//! violations (hardcoded secrets, raw SQL, path escapes) precisely, line by line —
//! that is the "linting" tier, and determinism is the right tool there. This pass is
//! the other tier: it READS the code and finds the GENUINE architectural / security
//! violations that are not line-level lint —
//!   - a write/mutation path with no authorization check,
//!   - services reaching the database directly, bypassing the repository layer,
//!   - N+1 query patterns,
//!   - imports that cross a module boundary they shouldn't,
//!   - inconsistent money/date/id handling across modules,
//!   - god objects / dead abstractions / duplicated logic that should be one seam.
//!
//! AI DISCOVERS these; the architect APPROVES; approved rules become gate config
//! (mechanical where possible) or AI-assisted integration checks (where a rule is
//! inherently semantic). Enforcement stays deterministic-or-codified; discovery is AI.
//!
//! Output is the SAME `Finding` / `ProposedRule` shapes the deterministic scan emits,
//! so the onboarding UI renders both tiers in one table. AI findings carry an `AI-`
//! rule-id prefix so the UI can mark their provenance.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::llm::{Llm, LlmPort, LlmRequest};
use crate::onboard::{Finding, ProposedRule, RuleOptionView};
#[cfg(test)]
use crate::review_tree::test_support::{
    cluster_heads, clusters, collapse_located, head_of, head_row, same_cause_hint,
};
use crate::stage_telemetry::{Stage, StageMeter, StageTelemetry};

// ════════════════════════════════════════════════════════════════════════════════════
// AUDIT-INTEGRATED ALTERNATIVE RECOMMENDATION (docs/design/2026-09-22_audit-integrated-
// alternatives.md)
// ════════════════════════════════════════════════════════════════════════════════════
//
// For each MULTI-OPTION semantic rule (>= 2 `[[option]]`s, AI-judged tier — never the
// deterministic floor, never a single-option/mechanical rule), the audit feeds the model
// EVERY option instead of one pre-resolved directive, and asks it to (a) recommend the
// option that best fits this codebase and (b) report violations AGAINST that recommended
// option. See `recommend_alternatives` (the dedicated pass that decides this ONCE per rule,
// grounded in real code) and `audit_repo`'s use of it.

/// One multi-option semantic rule's full alternative set, fed to the recommendation pass.
/// Built by the caller (`onboard::audit_repos` for the main scan, the `rescan-alternatives`
/// endpoint for a targeted re-check) by joining the project's selected rule ids against the
/// loaded corpus.
#[derive(Debug, Clone)]
pub struct RuleAlternatives {
    /// The rule id (uppercased, matching the `adopted` normalization used throughout this
    /// module).
    pub rule_id: String,
    /// Every option this rule defines — id, label, directive, and rationale.
    pub options: Vec<RuleOptionView>,
    /// The option currently selected for this project: the project's `chosen_option` for
    /// this rule if one was made, else the corpus `default_option`, else `None` (nothing
    /// selected yet — a rule with no default that the architect never chose an alternative
    /// for). Used both as the "currently selected" marker shown to the model and as the
    /// fail-soft fallback when the model's answer can't be trusted.
    pub selected_option_id: Option<String>,
    /// Pass-1 W3 (A5.3/A5.6): the rule's corpus [`camerata_rules::RuleKind`], carried into the
    /// alternatives pass so the "no convention adopted" outcome can be honored STRUCTURALLY for
    /// pure [`camerata_rules::RuleKind::Decision`] rules only — a `default_with_exceptions`
    /// rule has an externally-correct default, so an inconsistent codebase is still judged
    /// against that default rather than excused into a no-convention row. `None` (a corpus
    /// rule predating classification, or a test fixture) is treated like
    /// non-`Decision`: the no-convention outcome is NOT honored, failing safe toward the
    /// pre-existing judge-against-the-pick behavior.
    pub kind: Option<camerata_rules::RuleKind>,
}

pub use camerata_llm::failure::{AttemptRecord, FailureClass};

/// A named audit pass that failed or timed out for a repo — e.g. the alternative-
/// recommendation pass hitting the CLI's hang timeout. Captured here instead of only
/// `eprintln!`-ing to stderr, so the report export (`report_export::build_report_json`) can
/// disclose the gap EXPLICITLY in the methodology and summary rather than silently shipping
/// a report that never mentions the pass ran at all. `pass` is a short, stable, general name
/// (never a fixture-specific string) — any pass this fail-soft pattern applies to can push
/// one of these. See `ScanReport::failed_passes` and W6 in
/// the cycle-2 queue-hardening notes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FailedPass {
    /// The repo this pass failed for (`owner/repo`).
    pub repo: String,
    /// A short, stable, human-readable pass name (e.g. "rule-alternative recommendations")
    /// used verbatim in the disclosure text.
    pub pass: String,
    /// The error text, verbatim (e.g. the CLI timeout message), so the operator sees the
    /// real reason rather than a generic placeholder.
    pub reason: String,
    /// How many files this pass was shown and therefore did NOT review, for a scan pass (a
    /// file chunk x rule batch). Zero for a pass that is not over files (the recommendation,
    /// estimate and fix passes). `serde(default)` so a report persisted before this existed
    /// still loads. Rendered under Coverage by [`Self::coverage_line`].
    #[serde(default)]
    pub files_not_reviewed: usize,
    /// Why the part failed. Decides what the run did next (second try, outage count) and what the
    /// operator is told. `serde(default)` (content) so a ledger written before the class loads.
    #[serde(default)]
    pub class: FailureClass,
    /// The repo-relative files the part was shown and so did not review, for the failure report.
    /// Empty for a part that is not over files. Operator only; never rendered into an export.
    #[serde(default)]
    pub files: Vec<String>,
    /// What each attempt got back, oldest first, the end-of-run second try included. Operator
    /// only; never rendered into an export.
    #[serde(default)]
    pub attempts: Vec<AttemptRecord>,
    /// True for a disclosed LIMIT of this inspection's scope (a tool that was not run by policy
    /// or does not apply to the code), not a part that failed. Shown in Coverage like any
    /// disclosure; it never makes the run incomplete, so it never locks a client export.
    #[serde(default)]
    pub scope_note: bool,
}

impl FailedPass {
    /// The Coverage line for this failed pass: `"<repo>, <pass>: pass failed, N files not
    /// reviewed by the AI pass"`. A pass over no files states only that it failed. The full
    /// reason stays in the methodology disclosure; this is the one-line coverage fact.
    pub fn coverage_line(&self) -> String {
        if self.files_not_reviewed == 0 {
            return format!("{}, {}: pass failed", self.repo, self.pass);
        }
        let files = if self.files_not_reviewed == 1 {
            "1 file".to_string()
        } else {
            format!("{} files", self.files_not_reviewed)
        };
        format!(
            "{}, {}: pass failed, {files} not reviewed by the AI pass",
            self.repo, self.pass
        )
    }
}

/// One rule's recommendation: which option the AI judged best-fitting for this codebase (or
/// the operator's forced choice for a targeted rescan), plus the reasoning, plus whether the
/// raw model answer had to be corrected.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RuleRecommendation {
    /// The rule id this recommendation is for (uppercased).
    pub rule_id: String,
    /// The repo (`owner/repo`) this recommendation was made for. Each repo of a multi-repo scan
    /// is assessed on its own, so the convention found in one is not the convention of another.
    /// Empty for a recommendation made without a repo in view (an older report, a targeted
    /// re-assessment that has not been given one).
    #[serde(default)]
    pub repo: String,
    /// The option id violations were (or will be) reported against.
    pub recommended_option_id: String,
    /// 1-3 sentences explaining the pick, grounded in the codebase evidence the model saw.
    /// For an operator-forced pick this is a short fixed note, not a model explanation.
    pub recommendation_reasoning: String,
    /// True when the model's raw `recommended_option_id` was not a real option on this rule
    /// (or was missing/unparseable) and this entry's `recommended_option_id` is therefore the
    /// FALLBACK (the project's selected/default option), not what the model actually said.
    /// Surfaced so the UI can flag "the model's answer was corrected" rather than silently
    /// presenting a fallback as if it were a genuine recommendation.
    #[serde(default)]
    pub hallucinated: bool,
    /// True when this entry reflects the OPERATOR's explicit choice (via `rescan-alternatives`
    /// or `accept-alternatives`), not the AI's own pick. The UI's per-rule state machine uses
    /// this to render "your choice" (locked) instead of "AI recommended".
    #[serde(default)]
    pub operator_chosen: bool,
    /// The `file:line` (or `file:line — short note`) citation in the REPO that actually
    /// FOLLOWS or NEEDS the recommended option — e.g. `src/api/list_users.rs:42` for a
    /// cursor-pagination pick, grounding the recommendation in real code rather than a model
    /// assertion. `None` when the model found no such evidence (or the field was blank/absent)
    /// — see `applicable`'s doc comment: an absent evidence line FORCES `applicable = false`
    /// for a model-sourced recommendation, because an unevidenced "this codebase does/needs X"
    /// claim is never trusted (P7, the codebase-inspection hardening notes).
    /// Always `None` for an operator-forced pick (`operator_chosen: true`) — the operator's
    /// explicit choice needs no repo evidence to be trusted.
    #[serde(default)]
    pub evidence: Option<String>,
    /// Whether this rule's underlying CONCERN applies to this codebase at all. `false` means
    /// the codebase neither follows nor needs this rule's decision (e.g. no pagination
    /// anywhere, no API versioning anywhere) — `audit_repo` then drops the rule from the
    /// directive set actually checked, so it emits ZERO findings instead of a false "this
    /// project adopted the X model" claim. Defaults to `true` (back-compat: an
    /// operator-forced pick, or a recommendation that predates this field, is treated as
    /// applicable — the pre-existing behavior of always checking every selected multi-option
    /// rule). See [`parse_alternative_recommendations`] for how a model's `applicable: true`
    /// claim is validated against `evidence` before being trusted.
    #[serde(default = "default_recommendation_applicable")]
    pub applicable: bool,
    /// Pass-1 W3 (A5.3): `true` when the model judged that this rule's concern APPLIES but the
    /// codebase is INCONSISTENT with no clear majority — no convention has been adopted. Only
    /// ever set for a [`camerata_rules::RuleKind::Decision`] rule (the parser refuses it for
    /// any other kind — see [`parse_alternative_recommendations`]), and only when the claim is
    /// backed by a validated observed-usage breakdown or a real evidence citation. The caller
    /// (`audit_repo`) must then (a) DROP the rule from the directive set the violation passes
    /// check, (b) hard-filter any stray finding under it, and (c) synthesize exactly ONE
    /// `FindingKind::NoConvention` row — never N violations.
    #[serde(default)]
    pub no_convention: bool,
    /// Pass-1 W3 (A5.3/A5.6): the validated per-option observed-usage breakdown behind a
    /// `no_convention` outcome — each entry's `option_id` was checked against the rule's REAL
    /// options at parse time (an invented id is dropped, never carried) and its `label` is the
    /// AUTHORED option label, not model prose. Site counts are the model's own structured
    /// numbers (`Option<usize>` — absent means observed-but-not-counted, never fabricated).
    /// Empty for every normal (conventional) recommendation.
    #[serde(default)]
    pub observed_options: Vec<camerata_api_types::audit::ObservedOptionUsage>,
    /// Pass-1 W3 (A5.2/A5.6): the model's structured count of how many sites this rule's
    /// decision applies to in this codebase (`"sites_total"` in the recommendation JSON) — the
    /// DENOMINATOR of the deviation sentence ("2 of 47 endpoints"). `None` when the model gave
    /// no count or a non-positive one; consumers must then render the numerator alone rather
    /// than fabricate. Never used for severity, bucketing, or any gating decision — display
    /// honesty only.
    #[serde(default)]
    pub assessed_sites: Option<usize>,
    /// W21/W22 — whether the codebase ACTUALLY FOLLOWS the recommended option today, as
    /// distinct from the rule's concern merely applying to it.
    ///
    /// # Why this exists as its own field
    ///
    /// [`Self::applicable`] fuses two different states. Its own contract, and the prompt's
    /// (`"already follows this rule's concern OR concretely needs it"`), make
    /// `applicable: true` mean *either*. That is correct for deciding whether to evaluate a
    /// rule, and useless for deciding whether a convention is in force — yet
    /// [`conventions_observed_findings`] read `applicable: true` plus a non-`violates` option
    /// plus no surviving row and concluded "the codebase consistently follows X, zero
    /// violations were observed".
    ///
    /// On the 2026-10-18 hearth export that produced rows which contradicted themselves in a
    /// single paragraph. `ARCH-EXPLICIT-TX-1` shipped: *"The codebase consistently follows
    /// 'Services own the transaction lifecycle…' — zero violations were observed this scan.
    /// \[…\] The codebase has no transaction management yet \[…\] No existing pattern uses
    /// ambient or per-call transactions, so this is the right aspirational target."* Six of
    /// its 26 convention rows carried that tell by keyword; the graders found 13 of 26 by
    /// reading them.
    ///
    /// The model was not wrong. The prompt tells it to say plainly when a rule applies but
    /// nothing follows it yet, and it did — in `recommendation_reasoning`, as prose, which no
    /// severity pass or renderer can read. This field captures the distinction the model is
    /// ALREADY making instead of discarding it, which is the same prose-to-structure
    /// correction applied everywhere else in this effort.
    ///
    /// `None` means the model did not say. Consumers must treat absence as NOT followed: a
    /// convention claim is an assertion about the client's codebase that we print over our own
    /// name, so the burden of evidence sits on claiming it, never on refuting it.
    #[serde(default)]
    pub currently_followed: Option<bool>,
}

/// Serde default for [`RuleRecommendation::applicable`] — see that field's doc comment.
fn default_recommendation_applicable() -> bool {
    true
}

impl Default for RuleRecommendation {
    /// Manual (not derived) so `applicable` defaults to `true` here too — the same value the
    /// serde default produces — rather than `bool::default() == false`, which would silently
    /// contradict the field's own documented default.
    fn default() -> Self {
        RuleRecommendation {
            repo: String::new(),
            rule_id: String::new(),
            recommended_option_id: String::new(),
            recommendation_reasoning: String::new(),
            hallucinated: false,
            operator_chosen: false,
            evidence: None,
            no_convention: false,
            observed_options: Vec::new(),
            assessed_sites: None,
            currently_followed: None,
            applicable: true,
        }
    }
}

mod failures;
pub(crate) use failures::*;
mod usage_meter;
pub use usage_meter::*;
mod digest_parse;
pub use digest_parse::*;
mod severity_rules;
pub use severity_rules::*;
mod token_caps;
pub(crate) use token_caps::*;
mod alternatives;
pub use alternatives::*;
mod fix_stage;
pub use fix_stage::*;
mod scan_passes;
pub use scan_passes::*;
mod finding_merge;
pub use finding_merge::*;
mod routing;
pub use routing::*;
mod audit_run;
pub use audit_run::*;
mod deep_tier;
pub use deep_tier::*;

pub(crate) mod detail_regen;
pub(crate) mod reply_schemas;
use detail_regen::{regenerate_full_details, regenerate_full_details_in};
use stage_batch::StageRoute;

#[cfg(test)]
mod detail_regen_tests;

mod batch_mode;

mod calibration;
#[cfg(test)]
mod calibration_tests;
pub mod group_review;
#[cfg(test)]
mod group_review_tests;
#[cfg(test)]
mod loop_batch;
#[cfg(test)]
mod runner_adoption_tests;
pub mod stage_batch;
#[cfg(test)]
mod stage_batch_tests;
pub use batch_mode::{
    resolve_scan_mode, BatchApi, BatchSeam, ScanModeChoice, CLI_BACKEND_REALTIME_NOTICE,
};
use calibration::*;
pub use calibration::{
    apply_verdicts, outlier_reasons_of, stage1_of, verify_findings, verify_system_prompt, Real,
    Stage1Record, OUTLIER_REASONS_CAPTURE, STAGE1_CAPTURE,
};
pub(crate) use calibration::{is_verdict_reply, mentions_exploit_precondition};

#[cfg(test)]
mod redaction_tests;

#[cfg(test)]
mod dash_tests;

#[cfg(test)]
mod stage_telemetry_tests;

#[cfg(test)]
mod cache_schedule_tests;

#[cfg(test)]
mod retry_tests;

#[cfg(test)]
mod run_completion_tests;

#[cfg(test)]
mod second_try_tests;

#[cfg(test)]
mod rejection_reason_tests;

#[cfg(test)]
mod injection_tests;

#[cfg(test)]
mod efficiency_cap_tests;

#[cfg(test)]
mod spend_limit_stages_tests;

#[cfg(test)]
mod fix_model_tests;

#[cfg(test)]
mod batch_default_tests;

#[cfg(test)]
mod applies_to_routing_tests;

mod candidate_passes;

mod repo_map;
pub(crate) mod request_budget;
pub(crate) use repo_map::RepoMap;

#[cfg(test)]
mod candidate_prefilter_tests;

#[cfg(test)]
mod repo_map_tests;

#[cfg(test)]
mod request_budget_tests;

#[cfg(test)]
mod batch_split_tests;

#[cfg(test)]
mod scale_tests;

#[cfg(test)]
mod deep_tier_budget_tests;

#[cfg(test)]
mod malformed_reply_tests;

#[cfg(test)]
mod fn_batch_a_tests;

#[cfg(test)]
mod smoke_findings_tests;

#[cfg(test)]
mod drift_tests;

#[cfg(test)]
mod output_caps_tests;

#[cfg(test)]
mod structured_output_tests;

#[cfg(test)]
mod opaque_id_tests;

#[cfg(test)]
mod check_prompt_tests;

#[cfg(test)]
mod why_fix_direction_tests;

#[cfg(test)]
mod alternatives_cache_tests;

#[cfg(test)]
mod repo_grounding_tests;

#[cfg(test)]
mod stored_replay_tests;

#[cfg(test)]
mod resume_rejected_tests;

#[cfg(test)]
mod tests;
