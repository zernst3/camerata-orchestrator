//! Failed-pass records, the retry rules that decide when a second try is allowed, and the stage trail.

use super::*;

/// How many times a scan pass whose reply is MALFORMED (complete, not cut off, but holding no
/// JSON object the parser can read) is sent again before the pass is declared failed. A
/// malformed reply is never parsed as "no findings": it either recovers within this bound or
/// becomes a [`FailedPass`] naming the files it did not review. Kept small because the same
/// prompt tends to fail the same way and every retry is billed.
pub(crate) const MALFORMED_REPLY_RETRIES: usize = 2;

/// The line a malformed-reply retry adds to the user prompt. A retry after a content failure
/// must change the request (the same prompt sent again pays twice for the same answer), and this
/// tells the model what was wrong with its reply. Camerata's own words, outside the framed client
/// content, appended after the whole original prompt so the cached prefix is unchanged.
pub(crate) const MALFORMED_REPLY_NOTE: &str = "Your previous reply to this exact request held no JSON object that parses. Reply with ONLY the JSON object described above, no prose before or after it.";

/// One scan pass (a file chunk x rule batch) that produced NO answer, and why. Collected by
/// every pass runner ([`run_passes`], [`run_passes_batch`], [`run_routed_passes`],
/// [`run_security_passes`]) instead of keeping only the last error, so a round where 1 of 40
/// passes failed and one where 39 of 40 failed are distinguishable, and every failed pass is
/// named. A model REFUSAL (`stop_reason: "refusal"`) is one of these, never an empty finding list.
#[derive(Debug, Clone)]
pub(super) struct PassFailure {
    /// Human-readable pass name, e.g. `pass[rust] 2/3 · rules 1/4`.
    pub(super) pass: String,
    /// The error text (for a refusal, it says so).
    pub(super) error: String,
    /// How many files the failed pass was shown (its chunk), so the disclosure can say how
    /// many went unreviewed. Zero when the pass is not over a file chunk.
    pub(super) files: usize,
    /// `Some(n)` when the reply was cut off at the output limit and `n` complete findings were
    /// recovered from the part that arrived (they are in the round's findings; the rest of the
    /// pass was not reviewed). `None` for a pass that produced nothing.
    pub(super) recovered: Option<usize>,
    /// Why the pass failed: what the run does with it next (second try, outage count).
    pub(super) class: FailureClass,
    /// What each attempt at the call got back, from the transport.
    pub(super) attempts: Vec<AttemptRecord>,
    /// The repo-relative files the pass was shown, for the failure report.
    file_names: Vec<String>,
}

impl PassFailure {
    /// A failure of rule-batch `bi` of `n_b` on file chunk `ci` of `n_c` in round `label`.
    pub(super) fn at(
        label: &str,
        ci: usize,
        n_c: usize,
        bi: usize,
        n_b: usize,
        error: &dyn std::fmt::Display,
    ) -> Self {
        Self {
            pass: format!("{label} {}/{n_c} · rules {}/{n_b}", ci + 1, bi + 1),
            error: error.to_string(),
            files: 0,
            recovered: None,
            class: FailureClass::Content,
            attempts: Vec::new(),
            file_names: Vec::new(),
        }
    }

    /// A failure with an already-built pass name.
    pub(super) fn named(pass: String, error: &dyn std::fmt::Display) -> Self {
        Self {
            pass,
            error: error.to_string(),
            files: 0,
            recovered: None,
            class: FailureClass::Content,
            attempts: Vec::new(),
            file_names: Vec::new(),
        }
    }

    /// The same failure, recording the cause: its class and the transport's attempt trail.
    pub(super) fn caused_by(mut self, e: &anyhow::Error) -> Self {
        self.class = classify_pass_error(e);
        self.attempts = camerata_llm::failure::attempts_of(e);
        if self.attempts.is_empty() {
            // The call itself answered (a cut-off, malformed or refused reply), so the
            // transport has no failed attempt to show; the record says what was wrong with the
            // answer.
            let mut record = AttemptRecord::new("", "");
            record.error_type = Some(self.class.as_str().to_string());
            record.message = camerata_llm::failure::scrub_message(
                &self.error,
                camerata_llm::failure::MAX_RECORD_MESSAGE_CHARS,
            );
            self.attempts.push(record);
        }
        self
    }

    /// The same failure, naming the model its attempts asked for where the record does not.
    pub(super) fn with_model(mut self, model: Option<&str>) -> Self {
        let model = model
            .map(str::to_string)
            .unwrap_or_else(|| resolve_audit_model(None));
        for a in self.attempts.iter_mut().filter(|a| a.model.is_empty()) {
            a.model.clone_from(&model);
        }
        self
    }

    /// The same failure, naming the files the pass was shown.
    pub(super) fn with_file_names<'a>(mut self, names: impl Iterator<Item = &'a String>) -> Self {
        self.file_names = names.cloned().collect();
        self
    }

    /// The same failure, recording that the pass was shown `files` files.
    pub(super) fn with_files(mut self, files: usize) -> Self {
        self.files = files;
        self
    }

    /// The same failure, recording that `n` complete findings were recovered from a reply that
    /// was cut off (see [`PartialPassReply`]).
    pub(super) fn with_recovered(mut self, n: usize) -> Self {
        self.recovered = Some(n);
        self
    }
}

/// What every pass runner returns: `(findings, proposed rules, requested files, passes that
/// answered, passes that did not)`.
pub(super) type PassesOut = (
    Vec<Finding>,
    Vec<ProposedRule>,
    std::collections::HashSet<String>,
    usize,
    Vec<PassFailure>,
);

/// One [`FailedPass`] row per failed pass, each naming the pass and carrying the real error,
/// plus how many passes in the round answered (so 1-of-40 reads differently from 39-of-40).
/// The ONE place a runner's failures become disclosure; used by the scan rounds and the
/// resolution round alike.
pub(super) fn failed_pass_rows(
    repo: &str,
    answered: usize,
    failures: &[PassFailure],
) -> Vec<FailedPass> {
    let total = answered + failures.len();
    failures
        .iter()
        .map(|f| FailedPass {
            repo: repo.to_string(),
            pass: format!("audit {}", f.pass),
            reason: match f.recovered {
                // A cut-off reply: its complete findings are in the report, the rest is not.
                Some(n) => format!(
                    "{} ({n} complete finding{} recovered from the part that arrived and are \
                     in this report; {answered} of {total} passes in this round answered in \
                     full; any finding past the cut-off is missing)",
                    batch_mode::disclosable_error(&f.error),
                    if n == 1 { "" } else { "s" }
                ),
                None => format!(
                    "{} ({answered} of {total} passes in this round answered; the findings \
                     this pass would have produced are missing)",
                    batch_mode::disclosable_error(&f.error)
                ),
            },
            files_not_reviewed: f.files,
            class: f.class,
            files: f.file_names.clone(),
            attempts: f.attempts.clone(),
            scope_note: false,
        })
        .collect()
}

/// The class of a pass error. A cut-off reply and a model refusal are content by construction;
/// everything else is classified by the transport's own typed failure, else from its text.
pub(crate) fn classify_pass_error(e: &anyhow::Error) -> FailureClass {
    if e.downcast_ref::<PartialPassReply>().is_some() || crate::llm::is_refusal(e) {
        return FailureClass::Content;
    }
    camerata_llm::failure::classify_error(e)
}

/// What a stage's failed calls looked like, kept for its failed pass: the class of the last
/// failure and every attempt with its time, the end-of-run second try marked.
#[derive(Debug, Clone, Default)]
pub(crate) struct StageTrail {
    pub attempts: Vec<AttemptRecord>,
    pub class: Option<FailureClass>,
}

impl StageTrail {
    /// A call of the stage failed with `e`: keep the transport's own attempt records (or one
    /// describing the failure when the error carries none).
    pub(crate) fn note_error(&mut self, e: &anyhow::Error, second_try: bool) {
        let class = classify_pass_error(e);
        let mut records = camerata_llm::failure::attempts_of(e);
        if records.is_empty() {
            records.push(Self::describe(class, &e.to_string()));
        }
        for r in &mut records {
            r.second_try = second_try;
            if r.model.is_empty() {
                r.model = resolve_audit_model(None);
            }
        }
        self.attempts.extend(records);
        self.class = Some(class);
    }

    /// An item the shared runner could not get a result for: keep the transport's own attempt
    /// records (or one describing the failure when it has none), and the class of the failure.
    pub(crate) fn note_failed_item(
        &mut self,
        class: FailureClass,
        message: &str,
        attempts: &[AttemptRecord],
    ) {
        let mut records = attempts.to_vec();
        if records.is_empty() {
            records.push(Self::describe(class, message));
        }
        for r in &mut records {
            if r.model.is_empty() {
                r.model = resolve_audit_model(None);
            }
        }
        self.attempts.extend(records);
        self.class = Some(class);
    }

    /// The stage's calls answered but the answer was unusable (no fix, no estimate): keep one
    /// record saying so, so the failed pass shows when each try ended without a result.
    ///
    /// `rejections` says why each reply of the part was turned down (see
    /// [`AttemptRecord::rejections`]); empty when the part never got a reply to turn down.
    pub(crate) fn note_unusable(
        &mut self,
        message: &str,
        second_try: bool,
        rejections: Vec<String>,
    ) {
        let mut record = Self::describe(FailureClass::Content, message);
        record.second_try = second_try;
        record.model = resolve_audit_model(None);
        record.rejections = bounded_rejections(rejections);
        self.attempts.push(record);
        self.class.get_or_insert(FailureClass::Content);
    }

    pub(super) fn describe(class: FailureClass, message: &str) -> AttemptRecord {
        let mut record = AttemptRecord::new("", "");
        record.error_type = Some(class.as_str().to_string());
        record.message = camerata_llm::failure::scrub_message(
            message,
            camerata_llm::failure::MAX_RECORD_MESSAGE_CHARS,
        );
        record
    }

    /// The class of the failed pass: that of the last failure, else content.
    pub(crate) fn class(&self) -> FailureClass {
        self.class.unwrap_or(FailureClass::Content)
    }
}

/// The most rejection reasons one attempt record keeps.
pub(super) const MAX_RECORDED_REJECTIONS: usize = 40;

/// Rejection reasons as an attempt record keeps them: each scrubbed of secrets and capped, the
/// first [`MAX_RECORDED_REJECTIONS`] of them, and a last line counting the rest.
pub(crate) fn bounded_rejections(reasons: Vec<String>) -> Vec<String> {
    let total = reasons.len();
    let mut out: Vec<String> = reasons
        .into_iter()
        .take(MAX_RECORDED_REJECTIONS)
        .map(|r| {
            camerata_llm::failure::scrub_message(
                &r,
                camerata_llm::failure::MAX_RECORD_MESSAGE_CHARS,
            )
        })
        .collect();
    if total > MAX_RECORDED_REJECTIONS {
        out.push(format!("and {} more", total - MAX_RECORDED_REJECTIONS));
    }
    out
}

/// The error a stage returns when both its tries failed: the second failure with the whole
/// trail (the first try's records first, the second's marked).
pub(crate) fn error_with_both_tries(first: &anyhow::Error, second: anyhow::Error) -> anyhow::Error {
    let mut trail = StageTrail::default();
    trail.note_error(first, false);
    trail.note_error(&second, true);
    let class = classify_pass_error(&second);
    let mut failure = camerata_llm::failure::LlmCallFailure::new(class, second.to_string());
    failure.attempts = trail.attempts;
    anyhow::Error::new(failure)
}

/// Rides on the error of a call whose Message Batch was accepted but whose results could not be
/// fetched (D4). The batch may still be running and billed, so the call is not sent again: no
/// second try covers it, and a resume polls the batch. Carries the batch id.
#[derive(Debug)]
pub(crate) struct LiveBatchHeld(pub(crate) String);

impl std::fmt::Display for LiveBatchHeld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "batch {} was submitted and may still be running and billed, so it was not submitted \
             again",
            self.0
        )
    }
}

/// True when a failed call of a stage is worth the end-of-run second try: its class is transport
/// or content, the run has not stopped, and its batch is not still live (a second copy of a
/// running batch's call is paid for again).
pub(crate) fn second_try_allowed(e: &anyhow::Error, meter: Option<&UsageMeter>) -> bool {
    classify_pass_error(e).is_retried_at_end()
        && e.downcast_ref::<LiveBatchHeld>().is_none()
        && !meter.is_some_and(UsageMeter::outage_stopped)
}

/// Why a pass failed when the outage stop refused a retry its reply needed (the pass was in
/// flight when the run stopped).
pub(crate) const OUTAGE_BEFORE_RETRY: &str = "the run stopped because the model provider could \
                                              not be reached before the reply could be retried";

/// What refused a retry a reply needed: the outage stop once the run has stopped, else the spend
/// limit. The class says which, so a pass caught in flight by the stop is recorded as an outage
/// stop and the operator is never told a spend limit was reached when there may be none.
pub(super) fn retry_blocked_cause(meter: Option<&UsageMeter>) -> (FailureClass, &'static str) {
    if meter.is_some_and(UsageMeter::outage_stopped) {
        (FailureClass::OutageStop, OUTAGE_BEFORE_RETRY)
    } else {
        (FailureClass::SpendLimit, SPEND_LIMIT_BEFORE_RETRY)
    }
}

/// The error a pass returns when a retry its reply needed was refused; see
/// [`retry_blocked_cause`].
pub(super) fn retry_blocked_error(meter: Option<&UsageMeter>) -> anyhow::Error {
    let (class, message) = retry_blocked_cause(meter);
    anyhow::Error::new(camerata_llm::failure::LlmCallFailure::new(class, message))
}
