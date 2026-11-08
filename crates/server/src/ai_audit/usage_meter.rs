//! The usage meter (tokens, spend, request admission against the spend limit), the stage names and model resolution.

use super::*;

/// Aggregated REAL usage across every LLM call in one audit — all chunk×rule passes, the
/// resolution round, and the calibration pass. Lets the UI show ACTUAL vs the pre-scan
/// estimate. Thread-safe (passes run concurrently); cost is held in micro-dollars to stay
/// integer-atomic.
#[derive(Default)]
pub struct UsageMeter {
    input_tokens: AtomicU64,
    output_tokens: AtomicU64,
    cost_micro_usd: AtomicU64,
    /// The part of `cost_micro_usd` that came from replies a checkpoint held from an earlier
    /// attempt ([`Self::record_resumed`]). The spend limit adds the run's whole stored spend up
    /// front ([`RunControls::stored_spend_usd`]), so this share is taken off again.
    resumed_micro_usd: AtomicU64,
    calls: AtomicU64,
    cost_calls: AtomicU64,
    /// Tokens served from the prompt cache (billed at ~0.1× input rate). Populated only
    /// when the API backend is in use with `cache_breakpoints` set on the request.
    cache_read_input_tokens: AtomicU64,
    /// Tokens written to the prompt cache (billed at ~1.25× input rate, one-time per TTL).
    /// Populated only when the API backend is active with prompt caching enabled.
    cache_creation_input_tokens: AtomicU64,
    /// The same calls split by pipeline stage (operator telemetry; never exported — see
    /// [`crate::stage_telemetry`]). Always updated together with the aggregate above.
    stages: StageMeter,
    /// The largest dollar cost any single call has had so far, in micro-dollars. The cap's
    /// per-pass reserve (see [`crate::cost_cap`]).
    max_call_micro_usd: AtomicU64,
    /// The largest price any REQUEST this run has built could cost, in micro-dollars: its input at
    /// the full input rate plus its `max_tokens` at the output rate ([`request_ceiling_usd`]). The
    /// other half of the cap's per-pass reserve, so the reserve tracks what a call is allowed to
    /// cost and not only what past calls happened to cost (see [`crate::cost_cap`]).
    max_request_micro_usd: AtomicU64,
    /// How this run executes: efficiency switches and the spend cap. Set once, before the first
    /// call, by [`Self::with_controls`]; the default is every measure on and no cap.
    controls: crate::cost_cap::RunControls,
    /// The cap's mutable record: passes in flight, and what the cap stopped.
    budget: std::sync::Mutex<crate::cost_cap::BudgetState>,
    /// Wakes passes waiting for the first reported cost (see [`Self::admit`]).
    admission: tokio::sync::Notify,
    /// The outage stop: a run-wide counter of consecutive transport failures. Once it trips,
    /// admission refuses every new part. See [`crate::run_health`].
    health: crate::run_health::RunHealth,
}

/// Why a pass was not admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapReached;

/// Proof that a pass was admitted under the spend cap; releases its slot when dropped (after
/// the pass, its truncation retry included, has finished).
pub struct PassPermit<'a> {
    meter: &'a UsageMeter,
}

impl Drop for PassPermit<'_> {
    fn drop(&mut self) {
        {
            let mut budget = self.meter.budget();
            budget.in_flight = budget.in_flight.saturating_sub(1);
        }
        self.meter.admission.notify_waiters();
    }
}

/// The answer to one admission attempt.
pub(super) enum Admission<'a> {
    Admitted(PassPermit<'a>),
    /// The cap forbids another pass.
    CapReached,
    /// No call has reported a cost yet and a pass is already in flight, so there is nothing to
    /// reserve against: wait for it.
    Wait,
}

impl UsageMeter {
    /// A meter for a run that executes under `controls`.
    pub fn with_controls(controls: crate::cost_cap::RunControls) -> Self {
        Self {
            controls,
            ..Self::default()
        }
    }

    /// A meter for a stage that runs after the scan's own meter was closed (the group reviewer
    /// reads the final report): it starts from the usage the scan already made, so a spend limit
    /// counts the whole run and not only this stage.
    pub fn continuing(controls: crate::cost_cap::RunControls, prior: &ActualUsage) -> Self {
        let meter = Self::with_controls(controls);
        meter
            .input_tokens
            .store(prior.input_tokens, Ordering::Relaxed);
        meter
            .output_tokens
            .store(prior.output_tokens, Ordering::Relaxed);
        meter
            .cache_read_input_tokens
            .store(prior.cache_read_input_tokens, Ordering::Relaxed);
        meter
            .cache_creation_input_tokens
            .store(prior.cache_creation_input_tokens, Ordering::Relaxed);
        meter.cost_micro_usd.store(
            (prior.cost_usd.max(0.0) * 1_000_000.0).round() as u64,
            Ordering::Relaxed,
        );
        meter.resumed_micro_usd.store(
            (prior.resumed_cost_usd.max(0.0) * 1_000_000.0).round() as u64,
            Ordering::Relaxed,
        );
        meter.calls.store(prior.calls, Ordering::Relaxed);
        // Every call counted so far reported its cost only if the prior figure was complete.
        meter.cost_calls.store(
            if prior.cost_complete { prior.calls } else { 0 },
            Ordering::Relaxed,
        );
        meter
    }

    /// The efficiency switches this run executes under. A call site with no meter (a unit test
    /// of one function) uses [`efficiency_of`].
    pub fn efficiency(&self) -> crate::efficiency::EfficiencyConfig {
        self.controls.efficiency
    }

    /// The run's controls, as built.
    pub fn controls(&self) -> &crate::cost_cap::RunControls {
        &self.controls
    }

    /// The run-wide outage counter.
    pub fn health(&self) -> &crate::run_health::RunHealth {
        &self.health
    }

    /// True once the outage stop has tripped: no new part is sent.
    pub fn outage_stopped(&self) -> bool {
        self.health.is_stopped()
    }

    /// The class a part refused right now is recorded with: an outage stop, else the spend limit.
    fn not_run_class(&self) -> FailureClass {
        if self.health.is_stopped() {
            FailureClass::OutageStop
        } else {
            FailureClass::SpendLimit
        }
    }

    /// Actual spend so far, in dollars, as the telemetry and the printed total count it.
    pub fn spent_usd(&self) -> f64 {
        self.cost_micro_usd.load(Ordering::Relaxed) as f64 / 1_000_000.0
    }

    /// What the spend limit is checked against: this attempt's spend plus, on a resumed run, what
    /// the stored replies cost that this meter does not see again
    /// ([`crate::cost_cap::RunControls::stored_spend_usd`]). The limit counts the whole run. The
    /// usage figures and the telemetry stay what this attempt paid ([`Self::spent_usd`]).
    pub fn limit_spend_usd(&self) -> f64 {
        let resumed = self.resumed_micro_usd.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        (self.spent_usd() + self.controls.stored_spend_usd.max(0.0) - resumed).max(0.0)
    }

    /// True once the run's spend has reached the cap. Always false with no cap.
    pub fn cap_reached(&self) -> bool {
        self.controls
            .max_cost_usd
            .is_some_and(|cap| self.limit_spend_usd() >= cap)
    }

    /// True when one MORE call from a pass that already holds its permit would go past the
    /// limit: the same test [`Self::admit`] applies to a new pass (spend so far plus one call's
    /// reserve for every pass in flight, this one included, against the cap), without taking a
    /// permit. A retry of a cut-off or malformed reply checks this before it is sent. Always
    /// false with no cap; true once any call's cost was unknowable (the cap cannot be enforced).
    pub fn retry_blocked(&self) -> bool {
        // After an outage stop no call is sent, a retry of an admitted pass included.
        if self.health.is_stopped() {
            return true;
        }
        let Some(cap) = self.controls.max_cost_usd else {
            return false;
        };
        let budget = self.budget();
        if budget.cost_unknown {
            return true;
        }
        let reserve = self.reserve_micro_usd() as f64 / 1_000_000.0;
        self.limit_spend_usd() + budget.in_flight.max(1) as f64 * reserve >= cap
    }

    /// What one pass is assumed to cost while it is in flight, in micro-dollars: the larger of
    /// the most any single call has cost so far and the most any request built so far could
    /// cost. Taking the request's ceiling into account means the first pass of a run is no
    /// longer admitted against a zero reserve, and that a generous `max_tokens` (a 128,000-token
    /// ceiling on Opus is $2.56 of output alone) tightens admission instead of widening the
    /// overshoot the cap can reach.
    pub(super) fn reserve_micro_usd(&self) -> u64 {
        self.max_call_micro_usd
            .load(Ordering::Relaxed)
            .max(self.max_request_micro_usd.load(Ordering::Relaxed))
    }

    /// Tell the cap about a request about to be sent: the most it could cost joins the reserve
    /// ([`request_ceiling_usd`]). Call it for every request, before it is sent; with no cap set
    /// it does nothing. A request whose model has no list price adds nothing here (a call that
    /// cannot be priced already makes a cap unenforceable, and the pre-flight refuses it).
    pub fn note_request(&self, req: &LlmRequest) {
        if self.controls.max_cost_usd.is_none() {
            return;
        }
        let Some(ceiling) = request_ceiling_usd(req) else {
            return;
        };
        let micro = crate::llm::usd_to_micro_ceil(ceiling);
        let before = self
            .max_request_micro_usd
            .fetch_max(micro, Ordering::Relaxed);
        if before == 0 && micro > 0 {
            // Passes waiting for a first figure to reserve against can go on.
            self.admission.notify_waiters();
        }
    }

    fn budget(&self) -> std::sync::MutexGuard<'_, crate::cost_cap::BudgetState> {
        self.budget
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// One admission attempt. Admitted only while `spent + in_flight * reserve < cap` (see the
    /// overshoot bound in [`crate::cost_cap`]); with no cap, always admitted. The check and the
    /// in-flight count move together under one lock, so two passes cannot both claim the last
    /// slot.
    fn try_admit(&self) -> Admission<'_> {
        // The outage stop refuses every new part, cap or no cap.
        if self.health.is_stopped() {
            return Admission::CapReached;
        }
        let mut budget = self.budget();
        if let Some(cap) = self.controls.max_cost_usd {
            // A call's cost was unknowable, so the cap cannot be enforced: admit nothing more.
            if budget.cost_unknown {
                return Admission::CapReached;
            }
            let reserve_micro = self.reserve_micro_usd();
            let reserve = reserve_micro as f64 / 1_000_000.0;
            // The reserve includes what a request in flight MAY cost ([`Self::note_request`]), so
            // a pass whose request alone reaches the limit closes admission at once, whether or
            // not any call has reported a cost yet.
            if self.limit_spend_usd() + budget.in_flight as f64 * reserve >= cap {
                return Admission::CapReached;
            }
            // Until one call has reported a cost the run has not seen what a call really costs
            // (a request's ceiling is an upper bound, usually far above it), so only ONE pass may
            // be in flight: a cap must never be a number that concurrent passes can all slip
            // under at once (warm_schedule off, a free first call, ...).
            if self.max_call_micro_usd.load(Ordering::Relaxed) == 0 && budget.in_flight >= 1 {
                return Admission::Wait;
            }
        }
        budget.in_flight += 1;
        Admission::Admitted(PassPermit { meter: self })
    }

    /// Ask to start one scan pass. Waits (without spending anything) while the only thing
    /// standing in the way is that no cost is known yet; refuses with [`CapReached`] when the
    /// cap forbids the pass. An admitted pass holds its permit until it is dropped.
    pub async fn admit(&self) -> Result<PassPermit<'_>, CapReached> {
        loop {
            // Register for wake-ups BEFORE checking, so a release between the check and the
            // wait is not missed.
            let woken = self.admission.notified();
            tokio::pin!(woken);
            woken.as_mut().enable();
            match self.try_admit() {
                Admission::Admitted(permit) => return Ok(permit),
                Admission::CapReached => return Err(CapReached),
                Admission::Wait => woken.await,
            }
        }
    }

    /// Record a scan pass the cap refused.
    pub fn record_not_run(&self, mut pass: crate::cost_cap::NotRunPass) {
        pass.class = self.not_run_class();
        self.budget().not_run_passes.push(pass);
    }

    /// [`Self::admit`] for one call of a stage that is not a scan pass (a calibration batch, a
    /// fix batch, a deep-tier call). A refusal records the stage as stopped PART WAY, because
    /// the stage's earlier calls ran.
    pub async fn admit_for_stage(
        &self,
        repo: &str,
        stage: &str,
    ) -> Result<PassPermit<'_>, CapReached> {
        let permit = self.admit().await;
        if permit.is_err() {
            self.record_stage_not_run(repo, stage, true);
        }
        permit
    }

    pub(super) fn record_stage_not_run(&self, repo: &str, stage: &str, partial: bool) {
        let mut budget = self.budget();
        if !budget
            .not_run_stages
            .iter()
            .any(|s| s.repo == repo && s.stage == stage)
        {
            budget.not_run_stages.push(crate::cost_cap::NotRunStage {
                repo: repo.to_string(),
                stage: stage.to_string(),
                partial,
                class: self.not_run_class(),
            });
        }
    }

    /// Stage boundary check: true (and the stage is recorded as NOT RUN) when the stage must
    /// not start, so the caller skips it. `stage` is the name the failed-pass list shows.
    ///
    /// A stage is blocked once spend has reached the cap, AND once the cap has already stopped
    /// anything (a refused scan pass, or an earlier stage): after the cap has bitten, nothing
    /// later may start, which is what keeps the overshoot bounded.
    /// [`Self::stage_blocked`] for a stage that has stored work to replay: `stored_work` is true
    /// when its checkpoint holds absorbed replies from an earlier attempt, or (for a stage with
    /// no checkpoint, whose replies the journal answers) when the run is a resume. Such a stage
    /// STARTS whatever the run's spend, because replaying what was paid for costs nothing; each
    /// fresh send inside it is still admitted one by one and refused past the limit, so the
    /// limit stops only fresh sends. An outage stop still blocks it.
    pub fn stage_blocked_unless_stored(&self, repo: &str, stage: &str, stored_work: bool) -> bool {
        if stored_work && !self.health.is_stopped() && !self.cost_unknown() {
            return false;
        }
        self.stage_blocked(repo, stage)
    }

    /// True when this run is a resume that carries stored spend: the journal and the stages'
    /// checkpoints may hold replies for work that would otherwise be refused.
    pub fn is_resume(&self) -> bool {
        self.controls.stored_spend_usd > 0.0
    }

    pub fn stage_blocked(&self, repo: &str, stage: &str) -> bool {
        if self.health.is_stopped() {
            self.record_stage_not_run(repo, stage, false);
            return true;
        }
        let Some(_) = self.controls.max_cost_usd else {
            return false;
        };
        let already_stopped = {
            let budget = self.budget();
            !budget.not_run_passes.is_empty()
                || !budget.not_run_stages.is_empty()
                || budget.cost_unknown
        };
        if !self.cap_reached() && !already_stopped {
            return false;
        }
        self.record_stage_not_run(repo, stage, false);
        true
    }

    /// Repo boundary check: true (and the repo is recorded as not finished) when the cap has
    /// been reached or has already stopped something, so the caller skips the repo's model work
    /// without reading its files into digests first. The repo's own token-free checks are not
    /// affected; the caller still runs them. A run with no cap never blocks a repo.
    pub fn repo_blocked(&self, repo: &str) -> bool {
        // A resume reads the repo whatever its spend: the stored passes replay at no cost and each
        // fresh pass is admitted on its own.
        if self.is_resume() && !self.health.is_stopped() && !self.cost_unknown() {
            return false;
        }
        if self.health.is_stopped() {
            let mut budget = self.budget();
            if !budget.outage_repos.iter().any(|r| r == repo) {
                budget.outage_repos.push(repo.to_string());
            }
            return true;
        }
        let Some(_) = self.controls.max_cost_usd else {
            return false;
        };
        let already_stopped = {
            let budget = self.budget();
            !budget.not_run_passes.is_empty()
                || !budget.not_run_stages.is_empty()
                || !budget.repos_not_finished.is_empty()
                || budget.cost_unknown
        };
        if !self.cap_reached() && !already_stopped {
            return false;
        }
        let mut budget = self.budget();
        if !budget.repos_not_finished.iter().any(|r| r == repo) {
            budget.repos_not_finished.push(repo.to_string());
        }
        true
    }

    /// Batch gate: a submitted Message Batch cannot be cancelled cheaply, so the cap is checked
    /// BEFORE submitting. Returns the operator-facing reason to refuse, or `None` to submit.
    /// `projected_usd` is the caller's projection of what this submission commits (its exact
    /// input tokens at the batch price); the pre-flight estimate of the whole run, when the
    /// caller supplied one, can raise it. Pure: [`Self::record_note`] keeps the reason.
    ///
    /// `projected_usd` is `None` when the caller could not project the batch at all. With a cap
    /// set and no projection from anywhere, the cap cannot be enforced, so the submission is
    /// REFUSED and says so: a limit that silently does nothing is the worst outcome. The
    /// pre-flight estimate covers the whole run, so only what remains of it (estimate minus
    /// spend, floored at zero) is counted against each submission.
    pub fn batch_refusal_reason(&self, projected_usd: Option<f64>) -> Option<String> {
        let cap = self.controls.max_cost_usd?;
        let spent = self.limit_spend_usd();
        let remaining_estimate = self
            .controls
            .estimate_hint_usd
            .map(|hint| (hint - spent).max(0.0));
        let projected = match (projected_usd, remaining_estimate) {
            (None, None) => {
                return Some(format!(
                    "batch NOT submitted: the ${cap:.2} limit could not be enforced, because \
                     neither the exact token count nor the estimate could project this batch's \
                     cost, and a submitted batch cannot be cancelled cheaply"
                ));
            }
            (a, b) => a.unwrap_or(0.0).max(b.unwrap_or(0.0)),
        };
        if spent + projected <= cap {
            return None;
        }
        Some(format!(
            "batch NOT submitted: spent ${spent:.2} plus a projected ${projected:.2} would pass \
             the ${cap:.2} limit, and a submitted batch cannot be cancelled cheaply, so the \
             limit is checked before submitting"
        ))
    }

    /// Keep an operator-facing note on the cap's record (a refused batch submission).
    pub fn record_note(&self, note: String) {
        self.budget().notes.push(note);
    }

    /// Every operator-facing note the run kept on this meter, with or without a spend limit
    /// (the cap record carries them only when a limit was set).
    pub fn notes(&self) -> Vec<String> {
        self.budget().notes.clone()
    }

    /// What the cap did so far; `None` when no cap was set.
    pub fn cost_cap_record(&self) -> Option<crate::cost_cap::CostCapRecord> {
        let cap_usd = self.controls.max_cost_usd?;
        let budget = self.budget();
        Some(crate::cost_cap::CostCapRecord {
            cap_usd,
            spent_usd: self.limit_spend_usd(),
            // Only what the spend limit stopped: an outage stop has its own record.
            not_run_passes: budget
                .not_run_passes
                .iter()
                .filter(|p| p.class == FailureClass::SpendLimit)
                .cloned()
                .collect(),
            not_run_stages: budget
                .not_run_stages
                .iter()
                .filter(|s| s.class == FailureClass::SpendLimit)
                .cloned()
                .collect(),
            notes: budget.notes.clone(),
            repos_not_finished: budget.repos_not_finished.clone(),
            cost_unknown: budget.cost_unknown,
        })
    }

    /// True once a call's cost could not be determined while a limit was set: the limit can no
    /// longer be enforced, so nothing further is admitted (see [`Self::try_admit`]). What the
    /// refusals that follow are reported as depends on this: the limit was NOT reached.
    pub fn cost_unknown(&self) -> bool {
        self.budget().cost_unknown
    }

    /// What the outage stop left undone; `None` when the run never stopped.
    pub fn outage_record(&self) -> Option<crate::run_health::OutageRecord> {
        let trigger = self.health.trigger()?;
        let budget = self.budget();
        Some(crate::run_health::OutageRecord {
            trigger,
            not_run_passes: budget
                .not_run_passes
                .iter()
                .filter(|p| p.class == FailureClass::OutageStop)
                .cloned()
                .collect(),
            not_run_stages: budget
                .not_run_stages
                .iter()
                .filter(|s| s.class == FailureClass::OutageStop)
                .cloned()
                .collect(),
            repos_not_finished: budget.outage_repos.clone(),
        })
    }
}

/// [`UsageMeter::record_failed`] for a call site that may have no meter.
pub(super) fn meter_failed(meter: Option<&UsageMeter>, stage: Stage, error: &anyhow::Error) {
    if let Some(m) = meter {
        m.record_failed(stage, error);
    }
}

/// The most one request can cost, in dollars: its whole prompt (system and user text, at a
/// conservative three characters a token) at the full input rate, no cache discount, plus its
/// `max_tokens` (never above what the model can write) at the output rate. `None` for a model
/// the registry has no price for. The figure a spend limit reserves per call
/// ([`UsageMeter::note_request`]).
pub fn request_ceiling_usd(req: &LlmRequest) -> Option<f64> {
    let chars = req.prompt.len() + req.system.as_ref().map_or(0, String::len);
    let input = (chars as u64).div_ceil(3).max(1);
    let output = match camerata_llm::model_registry::max_output_for(&req.model) {
        Some(limit) => req.max_tokens.min(limit),
        None => req.max_tokens,
    };
    crate::llm::usage_cost_usd(
        &req.model,
        &crate::llm::CallUsage {
            input: Some(input),
            output: Some(u64::from(output)),
            cache_read: 0,
            cache_creation: 0,
            cache_creation_1h: None,
        },
        crate::llm::PriceBasis::realtime(),
    )
}

/// [`UsageMeter::note_request`] for a call site that may have no meter.
pub(crate) fn note_request(meter: Option<&UsageMeter>, req: &LlmRequest) {
    if let Some(m) = meter {
        m.note_request(req);
    }
}

/// [`UsageMeter::admit`] for a call site that may have no meter (no meter: always admitted, no
/// permit).
pub(super) async fn admit_pass(
    meter: Option<&UsageMeter>,
) -> Result<Option<PassPermit<'_>>, CapReached> {
    match meter {
        Some(m) => m.admit().await.map(Some),
        None => Ok(None),
    }
}

/// [`UsageMeter::admit_for_stage`] for a call site that may have no meter.
pub(super) async fn admit_stage<'a>(
    meter: Option<&'a UsageMeter>,
    repo: &str,
    stage: &str,
) -> Result<Option<PassPermit<'a>>, CapReached> {
    match meter {
        Some(m) => m.admit_for_stage(repo, stage).await.map(Some),
        None => Ok(None),
    }
}

/// Stage names as the failed-pass list shows them (also the spend-cap record's keys).
pub const STAGE_ALTERNATIVES: &str = "rule-alternative recommendations";
pub const STAGE_CALIBRATION: &str = "calibration (verification and hour estimates)";
pub const STAGE_DETAIL: &str = "detail regeneration";
pub const STAGE_FIX: &str = "fix-specific generation";
pub const STAGE_CLONE_JUDGE: &str = "clone-advisory judge";
pub const STAGE_DEEP: &str = "deep compliance tier";

/// The run estimate's whole-run dollars for a batch scan over `chunks` and `batches`, priced at
/// `model` (scan and calibration alike; `camerata_ui_core::scan::estimate_run_cost_bounds`, the
/// same arithmetic the pre-flight prints and the cockpit shows). The fallback projection for the
/// batch spend-cap gate when the exact token count is unavailable; `None` when the model has no
/// list price. The batches given are the work as routed, so they count as one group; the fix
/// stage and the clone judge run after the submission and are not part of what it commits.
pub(super) fn batch_estimate_usd(
    model: &str,
    chunks: &[&[(String, String)]],
    batches: &[&[(String, String)]],
    terse_output: bool,
) -> Option<f64> {
    use camerata_ui_core::scan::{
        estimate_run_cost_bounds, PricedModel, RunFlags, RunModels, RunRepoInput,
    };
    let price = crate::llm::models().into_iter().find(|m| m.id == model)?;
    let code_chars: usize = chunks
        .iter()
        .flat_map(|c| c.iter())
        .map(|(_, content)| content.len())
        .sum();
    let rules: usize = batches.iter().map(|b| b.len()).sum();
    let priced = PricedModel {
        id: model,
        price_in: price.price_in,
        price_out: price.price_out,
    };
    let bounds = estimate_run_cost_bounds(
        &RunRepoInput {
            repo: "batch".to_string(),
            code_chars,
            files: chunks.iter().map(|c| c.len()).sum(),
            selected: rules,
            multi_option_selected: 0,
            routing_groups: 1,
        },
        "batch",
        &RunModels {
            scan: priced,
            calibration: priced,
            group_review: None,
            fix: None,
            clone_judge: None,
        },
        RunFlags {
            deep: false,
            backend: crate::llm::ProjectBackend::Api,
            terse_output,
        },
    );
    Some(bounds.expected.1)
}

/// Whether the `terse_output` measure applies to a call site that may have no meter.
///
/// Unlike the other measures this one is OFF without a meter. It asks the model for less and
/// then spends a second stage to get the rest back; that stage must be metered (the spend limit,
/// the telemetry row and the operator's opt-out all live on the meter), so a path with no meter
/// runs the scan exactly as it ran before rank 4. [`warn_terse_needs_meter`] says so once.
pub fn terse_output_of(meter: Option<&UsageMeter>) -> bool {
    meter.is_some_and(|m| m.efficiency().terse_output)
}

/// The note for a path that has no meter: `terse_output` would have applied, and did not.
pub(super) fn warn_terse_needs_meter(repo: &str) {
    eprintln!(
        "[camerata-server] {repo}: terse_output needs a run meter (spend limit, telemetry, \
         opt-out), and this path has none; scanning with the full detail instead"
    );
}

/// The efficiency switches in force for a call site that may have no meter (every measure on).
pub fn efficiency_of(meter: Option<&UsageMeter>) -> crate::efficiency::EfficiencyConfig {
    meter.map(UsageMeter::efficiency).unwrap_or_default()
}

impl UsageMeter {
    /// Fold one completion into the aggregate AND into `stage`'s own row. Every model call
    /// names its stage, so the per-stage table can never drift from the aggregate.
    pub fn record_stage(&self, stage: Stage, r: &crate::llm::LlmResponse) {
        self.stages.record(stage, r);
        self.record(stage, r);
        // A reply came back: the provider is reachable, so the outage streak starts over.
        self.health.note_success();
    }

    /// [`Self::record_stage`] for a reply a checkpoint holds from an earlier attempt of this run:
    /// its usage joins the meter like any call's, and its cost is also counted as RESUMED, so the
    /// spend limit (which adds the whole stored spend up front) does not count it twice.
    pub fn record_resumed(&self, stage: Stage, r: &crate::llm::LlmResponse) {
        self.stages.record(stage, r);
        let micro = self.record(stage, r);
        self.resumed_micro_usd.fetch_add(micro, Ordering::Relaxed);
        self.health.note_success();
    }

    /// Mark a completion already passed to [`Self::record_stage`] as one whose output was
    /// thrown away (a rejected fix, a truncated pass that had to be repeated), so the table
    /// can show how much of a stage's spend bought nothing. Does not touch the aggregate.
    pub fn record_wasted(&self, stage: Stage, r: &crate::llm::LlmResponse) {
        self.stages.record_wasted(stage, r);
    }

    /// [`Self::record_wasted`] for a completion only partly discarded; see
    /// [`StageMeter::record_wasted_share`].
    pub fn record_wasted_share(&self, stage: Stage, r: &crate::llm::LlmResponse, share: f64) {
        self.stages.record_wasted_share(stage, r, share);
    }

    /// Add one fix-generation pass's counts to the run's fix give-up tally (see
    /// [`crate::stage_telemetry::FixOutcome`]).
    pub fn record_fix_outcome(&self, outcome: crate::stage_telemetry::FixOutcome) {
        self.stages.record_fix_outcome(outcome);
    }

    /// Add the detail-regeneration stage's rejection counts to the run's tally (operator
    /// telemetry only).
    pub fn record_detail_rejections(&self, rejections: crate::stage_telemetry::DetailRejections) {
        self.stages.record_detail_rejections(rejections);
    }

    /// Add the review flags a stage wrote on rows to the run's tally (operator telemetry only).
    pub fn record_review_flags(&self, stage: crate::review_flags::FlagStage, flags: &[String]) {
        self.stages.record_review_flags(stage, flags);
    }

    /// Record how many (chunk, rule) pairs the rule router skipped for one repo.
    pub fn record_routing(&self, pairs_total: u64, pairs_skipped: u64) {
        self.stages.record_routing(pairs_total, pairs_skipped);
    }

    /// Record one (chunk, rule) decision of the candidate prefilter (rank 8).
    pub fn record_candidate(
        &self,
        rule_id: &str,
        locator: &str,
        outcome: crate::stage_telemetry::CandidateOutcome,
        whole_bytes: u64,
        sent_bytes: u64,
    ) {
        self.stages
            .record_candidate(rule_id, locator, outcome, whole_bytes, sent_bytes);
    }

    /// Record what one call's repo map held (M1).
    pub fn record_repo_map(&self, call: crate::stage_telemetry::RepoMapCall) {
        self.stages.record_repo_map(call);
    }

    /// Add one call's request splits to the run's total (M1).
    pub fn record_request_splits(&self, splits: crate::stage_telemetry::RequestSplits) {
        self.stages.record_request_splits(splits);
    }

    /// Record that a repo's file tree fell back to a directory outline (M1).
    pub fn record_tree_fallback(&self, fallback: crate::stage_telemetry::RepoTreeFallback) {
        self.stages.record_tree_fallback(fallback);
    }

    /// Record one chunk's candidate-prefilter fallback to whole files (rank 8).
    pub fn record_candidate_fallback(&self, fallback: crate::stage_telemetry::CandidateFallback) {
        self.stages.record_candidate_fallback(fallback);
    }

    /// The per-stage telemetry so far, joined to the scan's counted-line total.
    pub fn stage_telemetry(&self, counted_lines: usize) -> StageTelemetry {
        let mut telemetry = self.stages.snapshot(counted_lines);
        telemetry.efficiency = Some(self.controls.efficiency);
        telemetry
    }

    /// Fold one completion's reported usage into the aggregate. Missing fields are simply not
    /// counted. Private: callers go through [`Self::record_stage`]. `stage` names the call in
    /// the note written when its cost cannot be determined.
    fn record(&self, stage: Stage, r: &crate::llm::LlmResponse) -> u64 {
        let mut counted_micro = 0u64;
        if let Some(i) = r.input_tokens {
            self.input_tokens.fetch_add(i, Ordering::Relaxed);
        }
        if let Some(o) = r.output_tokens {
            self.output_tokens.fetch_add(o, Ordering::Relaxed);
        }
        // With a cap set a call's cost MUST be known. A reported figure is used as is; a call that
        // reported tokens but no dollars is priced from them (the same function every other cost
        // path uses); a call that cannot be priced either way makes the cap unenforceable.
        let cost = match r.cost_usd {
            Some(c) => Some(c),
            None if self.controls.max_cost_usd.is_some() => {
                let derived = crate::llm::usage_cost_usd(
                    &r.model,
                    &crate::llm::CallUsage {
                        input: r.input_tokens,
                        output: r.output_tokens,
                        cache_read: r.cache_read_input_tokens,
                        cache_creation: r.cache_creation_input_tokens,
                        cache_creation_1h: None,
                    },
                    crate::llm::PriceBasis::for_response(r),
                );
                if derived.is_none() {
                    self.mark_cost_unknown(stage, r);
                }
                derived
            }
            None => None,
        };
        if let Some(c) = cost {
            // Rounded UP: this feeds the cap, and flooring could read a run that is over the
            // limit by a fraction of a micro-dollar as under it.
            let micro = crate::llm::usd_to_micro_ceil(c);
            counted_micro = micro;
            self.cost_micro_usd.fetch_add(micro, Ordering::Relaxed);
            let before = self.max_call_micro_usd.fetch_max(micro, Ordering::Relaxed);
            if before == 0 && micro > 0 {
                self.admission.notify_waiters();
            }
            self.cost_calls.fetch_add(1, Ordering::Relaxed);
        }
        // Cache breakdowns are additive across calls (each call contributes its own share
        // of reads / creations independently).
        self.cache_read_input_tokens
            .fetch_add(r.cache_read_input_tokens, Ordering::Relaxed);
        self.cache_creation_input_tokens
            .fetch_add(r.cache_creation_input_tokens, Ordering::Relaxed);
        self.calls.fetch_add(1, Ordering::Relaxed);
        counted_micro
    }

    /// A call's cost could not be determined while a cap is set: remember that the cap is no
    /// longer enforceable (nothing further is admitted) and say so once, in the cap's record,
    /// naming the model, the stage and the token counts of the call, so the operator can find it
    /// in the transcript and price it by hand.
    fn mark_cost_unknown(&self, stage: Stage, r: &crate::llm::LlmResponse) {
        {
            let mut budget = self.budget();
            if budget.cost_unknown {
                return;
            }
            budget.cost_unknown = true;
            let tokens = match (r.input_tokens, r.output_tokens) {
                (Some(i), Some(o)) => format!("{i} input and {o} output tokens"),
                _ => "no token counts".to_string(),
            };
            budget.notes.push(format!(
                "The spend limit could not be enforced past this point: a call to `{}` in the {} \
                 stage reported no cost and its tokens could not be priced ({tokens}), so what \
                 it cost is unknown. No further passes were started (NOT RUN).",
                r.model,
                stage.label()
            ));
        }
        self.admission.notify_waiters();
    }

    /// Count a call that FAILED but was still billed: a model refusal carries the usage it was
    /// charged (see [`crate::llm::ModelRefusal`]). A refused call costs what any other call
    /// costs, so the cap must see it; without this a run of refusals reads as $0 spent and never
    /// stops. A failure that carries no usage (a transport error) changes nothing.
    pub fn record_failed(&self, stage: Stage, error: &anyhow::Error) {
        if let Some(billed) = crate::llm::refusal_billed(error) {
            self.stages.record(stage, billed);
            self.record(stage, billed);
        }
        // The outage stop watches every failed call, after the transport's own retries.
        self.health
            .note_failure(classify_pass_error(error), &error.to_string());
    }

    pub fn snapshot(&self) -> ActualUsage {
        let calls = self.calls.load(Ordering::Relaxed);
        let cost_calls = self.cost_calls.load(Ordering::Relaxed);
        ActualUsage {
            input_tokens: self.input_tokens.load(Ordering::Relaxed),
            output_tokens: self.output_tokens.load(Ordering::Relaxed),
            cost_usd: self.cost_micro_usd.load(Ordering::Relaxed) as f64 / 1_000_000.0,
            calls,
            // Every call that ran reported a cost — so the dollar figure is complete, not a
            // partial sum that would understate (some calls' usage may be unreported).
            cost_complete: calls > 0 && cost_calls == calls,
            cache_read_input_tokens: self.cache_read_input_tokens.load(Ordering::Relaxed),
            cache_creation_input_tokens: self.cache_creation_input_tokens.load(Ordering::Relaxed),
            resumed_cost_usd: self.resumed_micro_usd.load(Ordering::Relaxed) as f64 / 1_000_000.0,
        }
    }
}

/// Resolve the model the audit pass will actually use: the caller's explicit pick (the open
/// project's setting in the cockpit, `--model` on the command line), else the built-in default
/// ([`crate::llm::DEFAULT_MODEL`], Sonnet 5.5). Never the environment.
///
/// # Why this is a function and not three copies of four lines
///
/// It was three copies. `audit_repo` picked a model one way, `generate_fix_specifics`
/// re-derived it with a comment reading "Resolve the model the same way `audit_pass` does",
/// and what reached `ScanProvenance.audit_model` was neither — it was the caller's RAW pick,
/// so a run driven by the default recorded `None`.
///
/// That is not a tidiness problem. `None` in provenance was read for months as "no model
/// ran", when it actually meant "nobody wrote down which model ran", and the 2026-10-16
/// five-repo sweep recorded `claude-sonnet-5` on its baseline and nothing on either later
/// run. The whole before/after comparison was therefore unfalsifiable in both directions, and
/// a week went into reasoning about an engine regression that did not exist.
///
/// Returning the RESOLVED value means provenance can state what ran, the integrity gate's
/// `model_unrecorded` family can mean what it says, and a future reader of an export can tell
/// the two cases apart.
pub fn resolve_audit_model(caller_pick: Option<&str>) -> String {
    caller_pick
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| crate::llm::DEFAULT_MODEL.to_string())
}

/// The model stage 1 (the per-site first reviewer) runs on: the caller's calibration pick (the
/// `--calibration-model` flag or the project setting), else the built-in stage 1 default, Opus 5.5
/// ([`camerata_api_types::project::STAGE1_DEFAULT_MODEL`], review tree design A13). The scan keeps
/// its own default; the two are chosen separately now. Never the environment. The one resolution
/// the pre-flight ([`crate::cost_cap::models_the_run_will_use`]) and the stage share, so the model
/// the run names and prices is the model it calls.
///
/// `_scan_model` is no longer the fallback; it stays in the signature so the callers that resolve
/// the scan model first read the same.
pub fn resolve_calibration_model(calibration_pick: Option<&str>, _scan_model: &str) -> String {
    calibration_pick
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map_or_else(
            || camerata_api_types::project::STAGE1_DEFAULT_MODEL.to_string(),
            str::to_string,
        )
}

/// The model a stage's request NAMES: the stage's pick when it has one, else the run's default
/// audit model ([`resolve_audit_model`]). Never empty.
///
/// A request with an empty model is not "the default": the transport fills it with the PROCESS
/// default (an environment variable, whatever an operator's `.env` said; it is gone), a model the pre-flight
/// never named, the ledger never recorded and the spend limit's price check never saw. On
/// 2026-10-29 that sent the alternatives stage to an unpriced id; its cost was unknown, so the
/// limit refused every later pass with about $9.99 of a $10 limit unspent. Every audit-stage
/// request goes through here so that cannot recur.
pub fn stage_model(pick: Option<&str>) -> String {
    resolve_audit_model(pick)
}

/// A snapshot of real audit usage, serialized onto the scan report for the UI's
/// actual-vs-estimated readout.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ActualUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub calls: u64,
    /// True when every call contributed a cost (the dollar total isn't a partial sum).
    pub cost_complete: bool,
    /// Tokens served from the prompt cache across all calls in this audit (billed at ~0.1×
    /// the normal input rate). Zero when the CLI backend is in use or caching is disabled.
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    /// Tokens written to the prompt cache across all calls (billed at ~1.25× input rate,
    /// once per 5-minute TTL window). Zero when the CLI backend is in use or caching is
    /// disabled.
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    /// The part of `cost_usd` that came from replies stored by an earlier attempt of the run
    /// (a resumed stage's checkpoint). The spend limit uses it to count a stored dollar once.
    #[serde(default)]
    pub resumed_cost_usd: f64,
}
