//! Output-token budgets that scale with the size of an aggregate pass, and the batch sizes of the fix and alternatives stages.

use super::*;

// ════════════════════════════════════════════════════════════════════════════════════
// AGGREGATE-PASS TUNING (calibration / fix-generation / alternative-recommendation)
// ════════════════════════════════════════════════════════════════════════════════════
//
// `verify_findings`, `generate_fix_specifics`, and `recommend_alternatives` each used to
// bundle EVERY finding/rule in the scan into a single completion with a flat
// `max_tokens(4096)`. Two failure modes that never affected the small per-(chunk,
// rule-batch) `audit_pass` calls: (1) on the CLI backend, one giant non-streaming call had
// no progress signal, so a slow-but-live run tripped the flat 300s subprocess timeout with
// no partial-output salvage; (2) on the API backend, a large finding set's response
// legitimately needs more than 4096 output tokens and got silently truncated instead. Both
// passes now (a) route through `LlmPort::complete_streaming` even when there is no
// transcript to feed — the CLI leg's heartbeat resets on any progress and SALVAGES partial
// output on a late kill, instead of the flat non-streaming path's fixed deadline — and (b)
// split their input into bounded chunks, each an independent streamed call with a
// per-chunk `max_tokens` that scales with that chunk's item count, so no single call is
// asked to describe more than one bounded batch's worth of findings/rules.

/// Floor for [`scaled_max_tokens`] — the ORIGINAL flat budget every aggregate pass
/// hardcoded before this fix, kept as the minimum so a small finding/rule set (the common
/// case) sees zero behavior change.
pub(super) const AGGREGATE_MAX_TOKENS_FLOOR: u32 =
    camerata_ui_core::scan::AGGREGATE_MAX_TOKENS_FLOOR;

/// Ceiling for [`scaled_max_tokens`] — bounds a single call's requested budget regardless
/// of how many items land in one chunk. The per-pass `*_BATCH_SIZE` constants below keep
/// any one call's item count well under what would reach this in practice; the ceiling is
/// a second, independent guard against requesting an unreasonably large budget from the
/// API (e.g. if a caller ever skips chunking).
pub(super) const AGGREGATE_MAX_TOKENS_CEILING: u32 =
    camerata_ui_core::scan::AGGREGATE_MAX_TOKENS_CEILING;

/// `max_tokens` for a call describing `n_items` things, given a per-item token estimate —
/// clamped to `[AGGREGATE_MAX_TOKENS_FLOOR, AGGREGATE_MAX_TOKENS_CEILING]` so small batches
/// keep today's flat budget and large batches never request more than the ceiling.
pub(crate) fn scaled_max_tokens(n_items: usize, per_item_tokens: u32) -> u32 {
    (n_items as u32)
        .saturating_mul(per_item_tokens)
        .clamp(AGGREGATE_MAX_TOKENS_FLOOR, AGGREGATE_MAX_TOKENS_CEILING)
}

/// `max_tokens` for a call describing `n_items` things whose reply runs at `effort`: the
/// per-item text budget PLUS the effort's thinking allowance
/// ([`camerata_ui_core::scan::output_cap_tokens`]), within the floor and the ceiling. The 5.x
/// models think before they write and the thinking counts against `max_tokens`; a cap that
/// covers only the text cuts the reply off with nothing in it.
pub(crate) fn staged_max_tokens(n_items: usize, per_item_tokens: u32, effort: &str) -> u32 {
    camerata_ui_core::scan::output_cap_tokens(
        (n_items as u32).saturating_mul(per_item_tokens),
        effort,
    )
}

/// The effort each stage asks for (`output_config.effort`), or `None` for a stage that keeps the
/// model's own default (the deep tier and the clone judge). Per STAGE, never per model: the
/// registry's per-model default (Opus 5.5 and Haiku 5.5 default to `medium` and were forced to
/// `high` for every call) is only the fallback for a request that names none.
pub(crate) fn stage_effort(stage: Stage) -> Option<&'static str> {
    use camerata_ui_core::scan as s;
    match stage {
        Stage::ScanPass => Some(s::SCAN_EFFORT),
        Stage::DetailRegeneration => Some(s::DETAIL_EFFORT),
        Stage::Calibration => Some(s::CALIBRATION_EFFORT),
        Stage::GroupReview => Some(s::GROUP_REVIEW_EFFORT),
        Stage::Alternatives => Some(s::ALTERNATIVES_EFFORT),
        Stage::FixGeneration => Some(s::FIX_EFFORT),
        Stage::DeepTier | Stage::CloneJudge => None,
    }
}

/// `req` with `stage`'s effort set ([`stage_effort`]).
pub(crate) fn with_stage_effort(req: LlmRequest, stage: Stage) -> LlmRequest {
    match stage_effort(stage) {
        Some(effort) => req.with_effort(effort),
        None => req,
    }
}

/// `max_tokens` for one scan request holding `n_rules` rules: the measured per-rule output budget
/// ([`camerata_ui_core::scan::SCAN_OUTPUT_TOKENS_PER_RULE`], the figure the pre-flight estimate
/// prices the scan's output from) plus the scan's thinking allowance ([`staged_max_tokens`] at
/// [`camerata_ui_core::scan::SCAN_EFFORT`]), never below the flat cap the scan used before it
/// scaled ([`camerata_ui_core::scan::SCAN_MAX_TOKENS_FLOOR`]) and never above the shared ceiling.
/// A smoke run showed 8 replies of a 15-rule batch cut off at the flat 8192.
pub(crate) fn scan_max_tokens(n_rules: usize) -> u32 {
    staged_max_tokens(
        n_rules,
        camerata_ui_core::scan::SCAN_OUTPUT_TOKENS_PER_RULE,
        camerata_ui_core::scan::SCAN_EFFORT,
    )
    .max(camerata_ui_core::scan::SCAN_MAX_TOKENS_FLOOR)
}

/// `max_tokens` for one alternatives request asking about `n_rules` rules: the measured per-rule
/// budget ([`camerata_ui_core::scan::ALTERNATIVES_OUTPUT_TOKENS_PER_RULE`], the figure the
/// pre-flight estimate prices the stage from) plus the stage's thinking allowance, so never below
/// the flat 4096 the stage used before it scaled and never above the shared ceiling.
pub(crate) fn alternatives_max_tokens(n_rules: usize) -> u32 {
    staged_max_tokens(
        n_rules,
        camerata_ui_core::scan::ALTERNATIVES_OUTPUT_TOKENS_PER_RULE,
        camerata_ui_core::scan::ALTERNATIVES_EFFORT,
    )
}

/// Per-finding token estimate for the FIX-GENERATION pass's response — each entry is a
/// full prose sentence describing a concrete, codebase-specific change, materially longer
/// than a calibration verdict.
pub(super) const FIX_TOKENS_PER_FINDING: u32 = 250;

/// `max_tokens` for one fix call over `n_findings` findings: the per-fix text budget plus the
/// fix stage's thinking allowance.
pub(crate) fn fix_max_tokens(n_findings: usize) -> u32 {
    staged_max_tokens(
        n_findings,
        FIX_TOKENS_PER_FINDING,
        camerata_ui_core::scan::FIX_EFFORT,
    )
}

/// How many findings fit in ONE fix-generation call. Same rationale as
/// `CALIBRATION_BATCH_SIZE` (a full chunk already exceeds the old flat floor); smaller
/// than the calibration batch because each response entry is longer prose.
pub(super) const FIX_BATCH_SIZE: usize = 25;

/// How many rules fit in ONE alternative-recommendation call. Each rule's block lists
/// every option with its own rationale — comparable prose volume to a fix — so it's sized
/// like `FIX_BATCH_SIZE`.
pub(super) const ALTERNATIVES_BATCH_SIZE: usize = 25;
