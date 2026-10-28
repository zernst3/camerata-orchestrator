//! Per-stage cost telemetry for one inspection (operator-only).
//!
//! The scan's [`crate::ai_audit::UsageMeter`] used to fold every model call into ONE aggregate,
//! so "where did the money go" could only be answered by hand: output was ~60% of the hearth
//! bill, but no per-stage split existed to say whether that was scan passes, fix retries or the
//! deep tier, and three runs of the same zip cost $12.3, $16.4 and $14.6 with nothing to say why
//! (`SPIKE_TOKEN_EFFICIENCY_RESULTS.md`, item 14). This module is that split:
//!
//! * [`Stage`] names the pipeline stages that call a model;
//! * [`StageMeter`] accumulates tokens, cache read/write and cost per stage, plus the tokens a
//!   stage spent on attempts whose output was thrown away ([`StageMeter::record_wasted`]);
//! * [`StageTelemetry`] is the immutable snapshot, joined to the scan's counted-line total so it
//!   can state cost per 1K counted lines, the unit that stays comparable across repos;
//! * [`cost_regression`] compares two snapshots of the same fixture and flags a cost-per-1K-lines
//!   increase above a threshold, so a later fixture sweep can fail on a regression.
//!
//! # Operator-only, never exported
//!
//! The snapshot lives on [`crate::scan_ledger::ScanLedger`] (the operator ledger the CLI persists
//! and prints), NOT on the client export. Nothing here is read by `report_export`, the workbook
//! or the manifest; the export format is a protected area and cost figures are not client
//! content. `report_export` has a test that pins this.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::llm::LlmResponse;

/// A pipeline stage that makes model calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Chunk x rule-batch scan passes (`audit_pass`, batch items included).
    ScanPass,
    /// Alternative-option recommendation for multi-option rules.
    Alternatives,
    /// The post-merge regeneration of full finding detail, after a terse scan
    /// (`terse_output`; `detail_regen`).
    DetailRegeneration,
    /// The skeptic pass: verification, severity/confidence calibration and hour estimates
    /// (`verify_findings`).
    Calibration,
    /// Codebase-specific fix generation, retries included.
    FixGeneration,
    /// The opt-in deep compliance and security tier.
    DeepTier,
    /// The clone-advisory judge.
    CloneJudge,
}

impl Stage {
    /// Every stage, in pipeline order (the order the table prints).
    pub const ALL: [Stage; 7] = [
        Stage::ScanPass,
        Stage::Alternatives,
        Stage::DetailRegeneration,
        Stage::Calibration,
        Stage::FixGeneration,
        Stage::DeepTier,
        Stage::CloneJudge,
    ];

    /// The short name the table and the logs use.
    pub fn label(self) -> &'static str {
        match self {
            Stage::ScanPass => "scan",
            Stage::Alternatives => "alternatives",
            Stage::DetailRegeneration => "detail",
            Stage::Calibration => "calibration",
            Stage::FixGeneration => "fix",
            Stage::DeepTier => "deep",
            Stage::CloneJudge => "clone-judge",
        }
    }
}

/// What one stage spent. `input_tokens` is the billed input side INCLUDING cache read and
/// write (the same convention as [`LlmResponse::input_tokens`]).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StageUsage {
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cost_usd: f64,
    /// Calls that reported a cost. `cost_usd` is a partial sum when this is below `calls`.
    #[serde(default)]
    pub cost_reported_calls: u64,
    /// Calls whose output was discarded (a rejected fix, a truncated pass that was retried).
    /// A SUBSET of `calls`, already counted in every field above; never added to totals twice.
    #[serde(default)]
    pub wasted_calls: u64,
    #[serde(default)]
    pub wasted_input_tokens: u64,
    #[serde(default)]
    pub wasted_output_tokens: u64,
    #[serde(default)]
    pub wasted_cost_usd: f64,
    /// Calls per model id, so a stage that runs on a different model than the scan (the fix
    /// stage on Haiku, rank 10) states what it ran on. The API backend reports the model that
    /// served the call; the CLI backend does not, so its calls are keyed by the REQUESTED model
    /// with a ` (requested)` suffix (see [`model_key`]). Sums to `calls`. Empty for a record
    /// written before this existed.
    #[serde(default)]
    pub models: BTreeMap<String, u64>,
}

/// The key a call is counted under in [`StageUsage::models`]: the served model id when the
/// backend reports it (the API does), else the requested id marked `(requested)` (the CLI
/// backend echoes the id it was asked for and never says what served the call).
fn model_key(r: &LlmResponse) -> String {
    if r.backend == "cli" {
        format!("{} (requested)", r.model)
    } else {
        r.model.clone()
    }
}

impl StageUsage {
    /// True when every call in this stage reported a cost, so `cost_usd` is not an
    /// understatement. An empty stage is trivially complete.
    pub fn cost_complete(&self) -> bool {
        self.cost_reported_calls == self.calls
    }

    fn fold(&mut self, r: &LlmResponse) {
        self.calls += 1;
        self.input_tokens += r.input_tokens.unwrap_or(0);
        self.output_tokens += r.output_tokens.unwrap_or(0);
        self.cache_read_input_tokens += r.cache_read_input_tokens;
        self.cache_creation_input_tokens += r.cache_creation_input_tokens;
        if let Some(c) = r.cost_usd {
            self.cost_usd += c;
            self.cost_reported_calls += 1;
        }
        *self.models.entry(model_key(r)).or_default() += 1;
    }

    fn fold_wasted(&mut self, r: &LlmResponse, share: f64) {
        let share = share.clamp(0.0, 1.0);
        let prorate = |tokens: Option<u64>| (tokens.unwrap_or(0) as f64 * share).round() as u64;
        self.wasted_calls += 1;
        self.wasted_input_tokens += prorate(r.input_tokens);
        self.wasted_output_tokens += prorate(r.output_tokens);
        self.wasted_cost_usd += r.cost_usd.unwrap_or(0.0) * share;
    }

    fn add(&mut self, other: &StageUsage) {
        self.calls += other.calls;
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_input_tokens += other.cache_read_input_tokens;
        self.cache_creation_input_tokens += other.cache_creation_input_tokens;
        self.cost_usd += other.cost_usd;
        self.cost_reported_calls += other.cost_reported_calls;
        self.wasted_calls += other.wasted_calls;
        self.wasted_input_tokens += other.wasted_input_tokens;
        self.wasted_output_tokens += other.wasted_output_tokens;
        self.wasted_cost_usd += other.wasted_cost_usd;
        for (model, calls) in &other.models {
            *self.models.entry(model.clone()).or_default() += calls;
        }
    }

    /// `model xN, other xM`, in model-id order. Empty when no call recorded a model.
    pub fn render_models(&self) -> String {
        self.models
            .iter()
            .map(|(model, calls)| format!("{model} x{calls}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// How the fix stage ended for the findings it was given, summed over every repo in the run. The
/// give-up rate is the quality signal a cheaper fix model is judged on: a finding the model
/// answered whose fix never passed the self-check falls back to the rule's authored remediation
/// instead of a codebase-specific fix. A finding the model was never heard from on (the spend
/// limit stopped it, or every call failed in transport) says nothing about the model's quality
/// and is reported separately, never as a give-up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixOutcome {
    /// Findings a fix call was actually made for (after carried fixes and the no-code
    /// pre-filter). A finding the spend limit refused every batch for is not in this figure.
    pub sent_to_model: u64,
    /// Of those, findings that received at least one parsed response. The give-up rate's
    /// denominator.
    #[serde(default)]
    pub answered: u64,
    /// Of the answered findings, those with no fix that passed the self-check after every retry.
    pub gave_up: u64,
    /// Findings left without a fix because the spend limit stopped the stage before any call
    /// for them was answered. Not a give-up.
    #[serde(default)]
    pub cap_stopped: u64,
    /// Findings left without a fix because every call for them failed in transport (timeout,
    /// HTTP error). Not a give-up.
    #[serde(default)]
    pub transport_failed: u64,
    /// Findings the no-code pre-filter gave the authored remediation with no model call. Not in
    /// any figure above.
    pub authored_without_model: u64,
}

impl FixOutcome {
    /// `gave_up / answered`; `None` when the model answered nothing.
    pub fn give_up_rate(&self) -> Option<f64> {
        (self.answered > 0).then(|| self.gave_up as f64 / self.answered as f64)
    }

    fn add(&mut self, other: FixOutcome) {
        self.sent_to_model += other.sent_to_model;
        self.answered += other.answered;
        self.gave_up += other.gave_up;
        self.cap_stopped += other.cap_stopped;
        self.transport_failed += other.transport_failed;
        self.authored_without_model += other.authored_without_model;
    }
}

/// How much the rule router (`scan_routing`) narrowed the scan: of the (chunk, rule) pairs an
/// un-routed scan would have sent, how many routing did NOT send. Counted over every repo of
/// the run. Operator-only, like the rest of this module.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingTelemetry {
    /// (chunk, rule) pairs an un-routed scan would send: every rule with every chunk.
    pub pairs_total: u64,
    /// Of those, the pairs routing did not send (language prefix and, with `applies_to_routing`
    /// on, declared `applies_to`).
    pub pairs_skipped: u64,
}

impl RoutingTelemetry {
    /// Skipped share of the total in percent; `None` when no pair was counted.
    pub fn skipped_percent(&self) -> Option<f64> {
        (self.pairs_total > 0).then(|| self.pairs_skipped as f64 * 100.0 / self.pairs_total as f64)
    }
}

/// What one (chunk, locator) evaluation of the candidate prefilter decided for a chunk
/// (token-efficiency rank 8). See `ai_audit`'s `candidate_passes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateOutcome {
    /// The rule was sent the chunk's candidate sites instead of its whole files.
    Narrowed,
    /// The locator read every file and found no site: the rule had nothing to judge there and
    /// no pass was sent for it.
    NoSites,
    /// The sites (plus the files the locator cannot read, carried whole) were not small enough
    /// to be worth a separate prompt, so the rule stayed in the chunk's whole-file prompt.
    NotNarrowed,
    /// The locator errored, timed out or could not parse a file: the rule was sent the chunk's
    /// whole files.
    Fallback,
}

/// One rule's candidate-prefilter totals over the run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleCandidateUsage {
    /// The locator the rule names.
    pub locator: String,
    /// Chunks where the rule was sent candidate sites only.
    pub chunks_narrowed: u64,
    /// Chunks where the locator found no site and no pass was sent for the rule.
    pub chunks_without_sites: u64,
    /// Chunks where narrowing would not have saved enough, so the rule saw whole files.
    pub chunks_not_narrowed: u64,
    /// Chunks where the locator failed and the rule saw whole files (the false-negative guard).
    pub chunks_fallback: u64,
    /// Code digest bytes the rule would have been sent with whole files, over every chunk.
    pub digest_bytes_whole: u64,
    /// Code digest bytes the rule was actually sent.
    pub digest_bytes_sent: u64,
}

impl RuleCandidateUsage {
    /// Digest bytes the rule was not sent.
    pub fn bytes_saved(&self) -> u64 {
        self.digest_bytes_whole
            .saturating_sub(self.digest_bytes_sent)
    }
}

/// One fallback to whole files: which chunk, which locator, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateFallback {
    pub repo: String,
    /// The route group's label (`pass[scoped]`).
    pub group: String,
    /// 1-based chunk index within the group.
    pub chunk: usize,
    pub locator: String,
    /// What failed (names the file when one file failed to parse).
    pub reason: String,
}

/// The candidate prefilter's run totals (token-efficiency rank 8): per rule, how many chunks it
/// was narrowed in and how many digest bytes that kept out of its prompts, plus every fallback.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateTelemetry {
    /// Keyed by rule id.
    pub rules: BTreeMap<String, RuleCandidateUsage>,
    /// Every chunk that fell back to whole files because its locator failed, in record order.
    pub fallbacks: Vec<CandidateFallback>,
}

impl CandidateTelemetry {
    /// Digest bytes not sent, summed over every rule.
    pub fn bytes_saved(&self) -> u64 {
        self.rules
            .values()
            .map(RuleCandidateUsage::bytes_saved)
            .sum()
    }
}

/// Why the detail-regeneration stage (rank 4) did not write a finding's full explanation on an
/// attempt, counted once per finding per attempt under one reason (a finding that fails three
/// rounds counts three times; a reply that gives it two entries, or none, still counts once).
/// Operator telemetry only: the client export and the failed-pass reason carry at most the
/// [`Self::total`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetailRejections {
    /// The entry echoed a path or line that is not the finding's.
    pub mismatch: u64,
    /// The text dropped a path or identifier the short statement named.
    pub drops_named_token: u64,
    /// The text added an exploit precondition nobody had made.
    pub invented_precondition: u64,
    /// The finding's entry had no usable id or text, carried an id that is not the batch's or
    /// shared its id with another entry.
    pub missing_id: u64,
    /// A finding whose entry was lost to a reply cut off at the output limit.
    pub truncated: u64,
    /// A finding the reply gave no entry for at all.
    pub left_out: u64,
}

impl DetailRejections {
    /// Every counted attempt.
    pub fn total(&self) -> u64 {
        self.mismatch
            + self.drops_named_token
            + self.invented_precondition
            + self.missing_id
            + self.truncated
            + self.left_out
    }

    /// Add another tally to this one.
    pub fn add(&mut self, other: &Self) {
        self.mismatch += other.mismatch;
        self.drops_named_token += other.drops_named_token;
        self.invented_precondition += other.invented_precondition;
        self.missing_id += other.missing_id;
        self.truncated += other.truncated;
        self.left_out += other.left_out;
    }

    /// The one operator-table line.
    fn render(&self) -> String {
        format!(
            "detail entries turned down: {} ({} wrong place, {} dropped a name, {} added a \
             condition, {} missing id, {} cut off, {} left out)",
            self.total(),
            self.mismatch,
            self.drops_named_token,
            self.invented_precondition,
            self.missing_id,
            self.truncated,
            self.left_out
        )
    }
}

/// Thread-safe per-stage accumulator (scan passes run concurrently).
#[derive(Default)]
pub struct StageMeter {
    stages: Mutex<BTreeMap<Stage, StageUsage>>,
    /// `None` until a fix-generation pass reports; stays `None` on a run with no fix stage.
    fix_outcome: Mutex<Option<FixOutcome>>,
    routing: Mutex<Option<RoutingTelemetry>>,
    candidates: Mutex<Option<CandidateTelemetry>>,
    detail_rejections: Mutex<Option<DetailRejections>>,
}

impl StageMeter {
    /// Fold one completion into `stage`.
    pub fn record(&self, stage: Stage, r: &LlmResponse) {
        self.with_stage(stage, |u| u.fold(r));
    }

    /// Mark a completion that [`Self::record`] already counted as one whose output was thrown
    /// away. Adds to the stage's `wasted_*` fields only; the totals are untouched.
    pub fn record_wasted(&self, stage: Stage, r: &LlmResponse) {
        self.record_wasted_share(stage, r, 1.0);
    }

    /// Like [`Self::record_wasted`] for a completion only PART of whose output was thrown away
    /// (a batched fix call where some fixes passed the self-check and some did not): `share` is
    /// the discarded fraction, clamped to `0.0..=1.0`, and the tokens and cost are prorated by
    /// it. The call still counts once in `wasted_calls`.
    pub fn record_wasted_share(&self, stage: Stage, r: &LlmResponse, share: f64) {
        self.with_stage(stage, |u| u.fold_wasted(r, share));
    }

    /// Add to the run's fix give-up tally.
    pub fn record_fix_outcome(&self, outcome: FixOutcome) {
        self.fix_outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert_with(FixOutcome::default)
            .add(outcome);
    }

    /// Add one repo's routing counts to the run's total.
    pub fn record_routing(&self, pairs_total: u64, pairs_skipped: u64) {
        let mut routing = self
            .routing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let r = routing.get_or_insert_with(RoutingTelemetry::default);
        r.pairs_total = r.pairs_total.saturating_add(pairs_total);
        r.pairs_skipped = r.pairs_skipped.saturating_add(pairs_skipped);
    }

    /// Add one regeneration run's rejection counts to the run's total.
    pub fn record_detail_rejections(&self, rejections: DetailRejections) {
        self.detail_rejections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert_with(DetailRejections::default)
            .add(&rejections);
    }

    /// Add one (chunk, rule) decision of the candidate prefilter to the run's total.
    /// `whole_bytes` is the chunk's whole-file digest size, `sent_bytes` what the rule was sent.
    pub fn record_candidate(
        &self,
        rule_id: &str,
        locator: &str,
        outcome: CandidateOutcome,
        whole_bytes: u64,
        sent_bytes: u64,
    ) {
        self.with_candidates(|c| {
            let u = c.rules.entry(rule_id.to_string()).or_default();
            if u.locator.is_empty() {
                u.locator = locator.to_string();
            }
            let counter = match outcome {
                CandidateOutcome::Narrowed => &mut u.chunks_narrowed,
                CandidateOutcome::NoSites => &mut u.chunks_without_sites,
                CandidateOutcome::NotNarrowed => &mut u.chunks_not_narrowed,
                CandidateOutcome::Fallback => &mut u.chunks_fallback,
            };
            *counter = counter.saturating_add(1);
            u.digest_bytes_whole = u.digest_bytes_whole.saturating_add(whole_bytes);
            u.digest_bytes_sent = u.digest_bytes_sent.saturating_add(sent_bytes);
        });
    }

    /// Record one chunk's fallback to whole files.
    pub fn record_candidate_fallback(&self, fallback: CandidateFallback) {
        self.with_candidates(|c| c.fallbacks.push(fallback));
    }

    fn with_candidates(&self, f: impl FnOnce(&mut CandidateTelemetry)) {
        let mut candidates = self
            .candidates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(candidates.get_or_insert_with(CandidateTelemetry::default));
    }

    /// Snapshot, joined to the scan's counted-line total.
    pub fn snapshot(&self, counted_lines: usize) -> StageTelemetry {
        let fix_outcome = *self
            .fix_outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let routing = *self
            .routing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let candidates = self
            .candidates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let detail_rejections = *self
            .detail_rejections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stages = self.lock();
        StageTelemetry {
            counted_lines,
            efficiency: None,
            fix_outcome,
            routing,
            candidates,
            detail_rejections,
            stages: Stage::ALL
                .iter()
                .filter_map(|stage| {
                    stages
                        .get(stage)
                        .filter(|u| u.calls > 0)
                        .map(|usage| StageRecord {
                            stage: *stage,
                            usage: usage.clone(),
                        })
                })
                .collect(),
        }
    }

    fn with_stage(&self, stage: Stage, f: impl FnOnce(&mut StageUsage)) {
        f(self.lock().entry(stage).or_default());
    }

    /// Telemetry must never take the scan down: a poisoned lock still holds valid counters
    /// (every critical section is a handful of additions), so recover the guard.
    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<Stage, StageUsage>> {
        self.stages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// One stage's row in a [`StageTelemetry`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageRecord {
    pub stage: Stage,
    pub usage: StageUsage,
}

/// Immutable per-stage snapshot of one inspection.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StageTelemetry {
    /// The `camerata count` line total for the scanned repo (same filter as the scan).
    pub counted_lines: usize,
    /// Only stages that made at least one call, in pipeline order.
    pub stages: Vec<StageRecord>,
    /// Which efficiency measures were on for this run ([`crate::efficiency`]), so a cost figure
    /// is never read without knowing what produced it. `None` for a record written before the
    /// switches existed.
    #[serde(default)]
    pub efficiency: Option<crate::efficiency::EfficiencyConfig>,
    /// The fix stage's give-up tally for the run. `None` when the fix stage never ran (or for a
    /// record written before this existed).
    #[serde(default)]
    pub fix_outcome: Option<FixOutcome>,
    /// How many (chunk, rule) pairs the rule router skipped, so the sweep can see the saving
    /// `applies_to_routing` buys. `None` when no repo reached the routing step.
    #[serde(default)]
    pub routing: Option<RoutingTelemetry>,
    /// What the candidate prefilter narrowed, per rule, and every fallback to whole files, so the
    /// sweep can see the saving `candidates_prefilter` buys and check the guard fired where it
    /// should. `None` when no rule with `candidates` ran.
    #[serde(default)]
    pub candidates: Option<CandidateTelemetry>,
    /// Why the detail-regeneration stage turned entries down, by reason. `None` when the stage
    /// never ran (or for a record written before this existed).
    #[serde(default)]
    pub detail_rejections: Option<DetailRejections>,
}

impl StageTelemetry {
    /// All stages summed.
    pub fn total(&self) -> StageUsage {
        let mut t = StageUsage::default();
        for s in &self.stages {
            t.add(&s.usage);
        }
        t
    }

    /// Dollars per 1,000 counted lines; `None` when there are no counted lines.
    pub fn cost_per_1k_lines(&self) -> Option<f64> {
        per_1k(self.total().cost_usd, self.counted_lines)
    }

    /// The fixed-width per-stage table printed at the end of `camerata inspect`.
    pub fn render_table(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        writeln!(
            out,
            "== STAGE TELEMETRY (operator only; not part of the client export) =="
        )
        .ok();
        if let Some(efficiency) = &self.efficiency {
            writeln!(out, "  efficiency measures: {}", efficiency.render()).ok();
        }
        if let Some(routing) = &self.routing {
            let share = routing
                .skipped_percent()
                .map_or_else(String::new, |p| format!(" ({p:.1}%)"));
            writeln!(
                out,
                "  rule routing: {} of {} (chunk, rule) pairs not sent{share}",
                routing.pairs_skipped, routing.pairs_total
            )
            .ok();
        }
        if let Some(candidates) = &self.candidates {
            render_candidates(&mut out, candidates);
        }
        if self.stages.is_empty() {
            writeln!(out, "  (no model call ran this scan)").ok();
            writeln!(out, "  counted lines: {}", self.counted_lines).ok();
            return out;
        }
        writeln!(
            out,
            "  {:<13} {:>5} {:>10} {:>9} {:>10} {:>10} {:>9} {:>11} {:>19}",
            "stage",
            "calls",
            "input",
            "output",
            "cache-rd",
            "cache-wr",
            "cost",
            "$/1K lines",
            "wasted (in/out)"
        )
        .ok();
        let row = |out: &mut String, name: &str, u: &StageUsage, lines: usize| {
            let partial = if u.cost_complete() { "" } else { " PARTIAL" };
            let per_1k =
                per_1k(u.cost_usd, lines).map_or_else(|| "n/a".to_string(), |v| format!("{v:.4}"));
            writeln!(
                out,
                "  {:<13} {:>5} {:>10} {:>9} {:>10} {:>10} {:>9} {:>11} {:>19}{partial}",
                name,
                u.calls,
                u.input_tokens,
                u.output_tokens,
                u.cache_read_input_tokens,
                u.cache_creation_input_tokens,
                format!("${:.4}", u.cost_usd),
                per_1k,
                format!("{}/{}", u.wasted_input_tokens, u.wasted_output_tokens),
            )
            .ok();
        };
        for s in &self.stages {
            row(&mut out, s.stage.label(), &s.usage, self.counted_lines);
        }
        row(&mut out, "total", &self.total(), self.counted_lines);
        for s in &self.stages {
            let models = s.usage.render_models();
            if !models.is_empty() {
                writeln!(out, "  {} model: {models}", s.stage.label()).ok();
            }
        }
        if let Some(rejections) = &self.detail_rejections {
            writeln!(out, "  {}", rejections.render()).ok();
        }
        if let Some(fix) = &self.fix_outcome {
            let rate = fix
                .give_up_rate()
                .map_or_else(|| "n/a".to_string(), |r| format!("{:.1}%", r * 100.0));
            writeln!(
                out,
                "  fix give-up rate: {rate} ({} of {} answered by the fix model; {} not answered: {} \
                 stopped by the spend limit, {} failed in transport; {} used the authored \
                 remediation with no model call)",
                fix.gave_up,
                fix.answered,
                fix.cap_stopped + fix.transport_failed,
                fix.cap_stopped,
                fix.transport_failed,
                fix.authored_without_model
            )
            .ok();
        }
        writeln!(out, "  counted lines: {}", self.counted_lines).ok();
        out
    }
}

fn per_1k(cost_usd: f64, counted_lines: usize) -> Option<f64> {
    if counted_lines == 0 {
        return None;
    }
    Some(cost_usd / (counted_lines as f64 / 1000.0))
}

/// The default regression threshold, per the spike's acceptance rule: cost per 1K counted lines
/// may not rise more than 15% against the previous sweep without a noted change.
pub const DEFAULT_COST_REGRESSION_PCT: f64 = 15.0;

/// Outcome of comparing two telemetry records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegressionVerdict {
    Within,
    Regressed,
    /// The records cannot be compared (no lines, no cost, or partial cost). Callers treat this
    /// as a failure to check, never as a pass.
    NotComparable,
}

/// One stage whose cost per 1K lines moved between the two records.
#[derive(Debug, Clone, PartialEq)]
pub struct StageCostChange {
    pub stage: Stage,
    pub baseline_per_1k: f64,
    pub current_per_1k: f64,
    pub change_pct: f64,
}

/// The result of [`cost_regression`].
#[derive(Debug, Clone, PartialEq)]
pub struct CostRegression {
    pub verdict: RegressionVerdict,
    pub threshold_pct: f64,
    pub baseline_per_1k: Option<f64>,
    pub current_per_1k: Option<f64>,
    /// Percent change of the overall cost per 1K lines (`None` when not comparable).
    pub change_pct: Option<f64>,
    /// Why the records were not comparable (empty otherwise).
    pub reason: String,
    /// Stages (present in both records) whose own cost per 1K lines rose above the threshold.
    pub stage_regressions: Vec<StageCostChange>,
}

impl CostRegression {
    pub fn is_regression(&self) -> bool {
        self.verdict == RegressionVerdict::Regressed
    }

    /// A short operator-readable verdict.
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        match self.verdict {
            RegressionVerdict::NotComparable => {
                write!(
                    out,
                    "cost regression check: NOT COMPARABLE ({})",
                    self.reason
                )
                .ok();
            }
            verdict => {
                let label = if verdict == RegressionVerdict::Regressed {
                    "REGRESSION"
                } else {
                    "ok"
                };
                write!(
                    out,
                    "cost regression check: {label}: ${:.4} -> ${:.4} per 1K counted lines \
                     ({:+.1}%, threshold +{:.1}%)",
                    self.baseline_per_1k.unwrap_or(0.0),
                    self.current_per_1k.unwrap_or(0.0),
                    self.change_pct.unwrap_or(0.0),
                    self.threshold_pct,
                )
                .ok();
                for c in &self.stage_regressions {
                    write!(
                        out,
                        "\n  stage {}: ${:.4} -> ${:.4} per 1K ({:+.1}%)",
                        c.stage.label(),
                        c.baseline_per_1k,
                        c.current_per_1k,
                        c.change_pct,
                    )
                    .ok();
                }
            }
        }
        out
    }
}

/// Compare two telemetry records of the SAME fixture. Flags a regression when the current cost
/// per 1K counted lines exceeds the baseline's by more than `threshold_pct` percent.
pub fn cost_regression(
    baseline: &StageTelemetry,
    current: &StageTelemetry,
    threshold_pct: f64,
) -> CostRegression {
    let not_comparable = |reason: String| CostRegression {
        verdict: RegressionVerdict::NotComparable,
        threshold_pct,
        baseline_per_1k: baseline.cost_per_1k_lines(),
        current_per_1k: current.cost_per_1k_lines(),
        change_pct: None,
        reason,
        stage_regressions: Vec::new(),
    };
    for (name, t) in [("baseline", baseline), ("current", current)] {
        if t.counted_lines == 0 {
            return not_comparable(format!("the {name} record has no counted lines"));
        }
        let total = t.total();
        if total.calls == 0 {
            return not_comparable(format!("the {name} record made no model call"));
        }
        if !total.cost_complete() {
            return not_comparable(format!(
                "the {name} record's cost is partial (some calls reported no cost)"
            ));
        }
    }
    let (Some(base), Some(cur)) = (baseline.cost_per_1k_lines(), current.cost_per_1k_lines())
    else {
        return not_comparable("a record has no cost per 1K lines".to_string());
    };
    if base <= 0.0 {
        return not_comparable("the baseline cost is zero".to_string());
    }
    let change_pct = (cur - base) / base * 100.0;
    let stage_regressions = Stage::ALL
        .iter()
        .filter_map(|stage| {
            let b = per_1k(stage_cost(baseline, *stage)?, baseline.counted_lines)?;
            let c = per_1k(stage_cost(current, *stage)?, current.counted_lines)?;
            (b > 0.0 && (c - b) / b * 100.0 > threshold_pct).then(|| StageCostChange {
                stage: *stage,
                baseline_per_1k: b,
                current_per_1k: c,
                change_pct: (c - b) / b * 100.0,
            })
        })
        .collect();
    CostRegression {
        verdict: if change_pct > threshold_pct {
            RegressionVerdict::Regressed
        } else {
            RegressionVerdict::Within
        },
        threshold_pct,
        baseline_per_1k: Some(base),
        current_per_1k: Some(cur),
        change_pct: Some(change_pct),
        reason: String::new(),
        stage_regressions,
    }
}

fn stage_cost(t: &StageTelemetry, stage: Stage) -> Option<f64> {
    t.stages
        .iter()
        .find(|s| s.stage == stage)
        .map(|s| s.usage.cost_usd)
}

/// The candidate-prefilter lines of [`StageTelemetry::render_table`]: one summary line, one line
/// per rule, one line per fallback.
fn render_candidates(out: &mut String, c: &CandidateTelemetry) {
    use std::fmt::Write as _;
    let whole: u64 = c.rules.values().map(|u| u.digest_bytes_whole).sum();
    let share = if whole > 0 {
        format!(" ({:.1}%)", c.bytes_saved() as f64 * 100.0 / whole as f64)
    } else {
        String::new()
    };
    writeln!(
        out,
        "  candidate prefilter: {} rules, {} of {} digest bytes not sent{share}, {} fallbacks to whole files",
        c.rules.len(),
        c.bytes_saved(),
        whole,
        c.fallbacks.len()
    )
    .ok();
    for (id, u) in &c.rules {
        writeln!(
            out,
            "    {id} [{}]: narrowed {}, no sites {}, not narrowed {}, fallback {}; {} bytes saved",
            u.locator,
            u.chunks_narrowed,
            u.chunks_without_sites,
            u.chunks_not_narrowed,
            u.chunks_fallback,
            u.bytes_saved()
        )
        .ok();
    }
    for f in &c.fallbacks {
        writeln!(
            out,
            "    fallback: {} {} chunk {} [{}]: {}",
            f.repo, f.group, f.chunk, f.locator, f.reason
        )
        .ok();
    }
}

#[cfg(test)]
mod tests;
