//! The advisory seam for the three clone-based rules (W29 Wave C):
//! `ARCH-SINGLE-SOURCE-LOGIC-1`, `ARCH-NAMING-ONE-CONCEPT-1` and `UI-SHARED-PRIMITIVE-1`.
//!
//! Spec: `docs/design/2026-10-21_w29-detector-implementation.md`, "Wiring the H rules: the
//! advisory seam" and the Wave C build notes.
//!
//! # What this module is for
//!
//! `camerata_checks::clone_rules` finds CANDIDATES deterministically: families of repeated code
//! with the facts a client can verify by diffing the listed lines. Whether a candidate is one
//! application decision, one concept or one UI mechanism is a judgment, and a registered
//! deterministic checker would ship every candidate with none. This module is the judgment, and
//! it is deliberately narrow:
//!
//! - ONE closed question per rule ([`prompt::question`]); the answer is a verdict word from a
//!   closed vocabulary. Only the rule's affirmative word ships a finding.
//! - `unsure`, an unparsed response, a verdict word outside the vocabulary, an id that was not
//!   asked about and a failed batch can never promote a candidate to a confirmed finding. They
//!   are counted and retried next scan (`unsure` is not cached). When the rule has at least one
//!   judged candidate, each such candidate is a Queue row marked unsure (held for the reviewer's
//!   decision, [`findings::unsure_findings`]); when no candidate of the rule was judged, the rule
//!   gets ONE `not_assessed` row. A rule never carries both: that would put it on two surfaces.
//! - The judge never sees a file: its input is [`camerata_checks::clone_rules::JudgeInput`],
//!   at most 3 members of at most 30 lines each, plus deterministic facts.
//! - Cost is bounded by construction: batches of [`BATCH_SIZE`], at most
//!   [`camerata_checks::clone_rules::CLONE_JUDGE_MAX_PER_RULE`] candidates per rule per scan, a
//!   content-keyed verdict cache, and the project's fast model tier. Candidates past the cap are
//!   counted and disclosed, never judged, never shipped.
//! - Offline (no model, deterministic-only, compliance block): zero findings, one
//!   `not_assessed` row per armed rule that has candidates, and the ledger says the judgment did
//!   not run. A rule with zero candidates ran clean.
//!
//! The model is whatever [`resolve_clone_judge_model`] says; it is stamped into
//! `ScanProvenance::clone_judge_model` so a before/after comparison can attribute a change to it.

use std::collections::{BTreeMap, HashSet};

use camerata_api_types::audit::NotAssessedReason;
use camerata_checks::clone_rules::{
    judge_input, Candidate, CandidateSet, CloneRule, CLONE_JUDGE_MAX_PER_RULE,
};

use crate::ai_audit::{scaled_max_tokens, AttemptRecord, FailedPass, UsageMeter};
use crate::llm::{LlmPort, LlmRequest};
use crate::scan_ledger::{RuleTier, ScanLedger};
use crate::stage_telemetry::Stage;
use crate::transcript::{AgentTranscript, TranscriptStore};

use super::Finding;

pub mod cache;
pub mod findings;
pub mod prompt;
pub mod redo;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod partial_reason_tests {
    use super::*;

    #[test]
    fn the_skip_reason_keeps_the_left_for_the_reviewer_suffix() {
        let counts = JudgeCounts {
            found: 5,
            unjudged: 3,
            held: 2,
            ..JudgeCounts::default()
        };
        assert!(partial_skip_reason(&counts).ends_with("; 2 left for the reviewer"));
        assert!(!partial_skip_reason(&JudgeCounts {
            found: 5,
            unjudged: 3,
            ..JudgeCounts::default()
        })
        .contains("left for the reviewer"));
    }
}

pub use cache::{cache_key, CachedVerdict, VerdictCache, MAX_CACHED_VERDICTS};
use prompt::{batch_prompt, candidate_json, parse_verdicts, system_prompt, Verdict};

/// Candidates per model call.
pub const BATCH_SIZE: usize = 16;

/// Asks per batch: the first, and ONE repair follow-up when the reply is not a verdict object.
pub const JUDGE_ATTEMPTS: usize = 2;

/// The longest sample of a reply an attempt record keeps (operator only, never in an export).
const REPLY_SAMPLE_CHARS: usize = 160;

/// The note appended to a batch whose first reply was not a verdict object (the wording the
/// other stages use for their one repair turn).
const REPAIR_NOTE: &str = "\n\nYour previous reply was refused: it was not one JSON object with \
     a \"verdicts\" array answering the candidates above. Answer again with the verdict object \
     only: one JSON object in exactly the form described, with every candidate id, and no other \
     text.\n";

/// The repair follow-up of `sent`: the same request with the refusal appended after the batch
/// (the cache prefix before it is unchanged).
pub(crate) fn repair_request(mut sent: LlmRequest) -> LlmRequest {
    sent.prompt.push_str(REPAIR_NOTE);
    sent
}

/// A redacted, length-capped start of a reply, for the operator's failure record.
fn reply_sample(text: &str) -> String {
    let redacted = crate::onboard::redact::redact_visible(text);
    camerata_llm::failure::scrub_message(&redacted, REPLY_SAMPLE_CHARS)
}

fn attempt_record(model: &str, kind: &str, message: &str) -> AttemptRecord {
    let mut record = AttemptRecord::new("", model);
    record.error_type = Some(kind.to_string());
    // The same redaction as the reply sample: an error text can echo a connection string or a key.
    record.message = camerata_llm::failure::scrub_message(
        &crate::onboard::redact::redact_visible(message),
        camerata_llm::failure::MAX_RECORD_MESSAGE_CHARS,
    );
    record
}

/// Output tokens budgeted per candidate (a verdict object with a short concept).
const TOKENS_PER_CANDIDATE: u32 = 80;

/// The pass name a failed batch is disclosed under (`FailedPass::pass`).
pub const FAILED_PASS_NAME: &str = "clone-candidate judgment";

/// Resolve the model the clone judge runs on: the caller's explicit pick, else the fast tier's
/// built-in primary (`camerata_fleet::tier::default_fast_model`, Haiku 5.5). Never the
/// environment. Never the audit model by default: this is
/// classification over bounded structured input, the fast band of the tier map
/// (`docs/decisions/2026-06-20_deterministic_model_tiering.md`).
pub fn resolve_clone_judge_model(caller_pick: Option<&str>) -> String {
    caller_pick
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(camerata_fleet::tier::default_fast_model)
}

/// What happened to one rule's candidates this scan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JudgeCounts {
    /// Candidates the producer found: judged plus over the cap.
    pub found: u32,
    /// Candidates with a definite verdict (a model answer or a cached one).
    pub judged: u32,
    /// Of those, the ones that shipped a finding.
    pub confirmed: u32,
    /// Of those, definite non-affirmative verdicts (`different`, `boilerplate`, `adapter`, ...).
    pub rejected: u32,
    /// Candidates within the cap that got no definite verdict: `unsure`, no verdict, a failed
    /// batch, or no model.
    pub unjudged: u32,
    /// Of `unjudged`, the ones the model looked at and answered "unsure" (an allowed verdict,
    /// never cached, never clean). The rest got no usable answer.
    pub unsure: u32,
    /// Of `unsure`, the ones shown as held Queue rows because the rule had judged candidates.
    /// The reviewer decides those; they are not a coverage gap.
    pub held: u32,
    /// Candidates past the per-scan cap: counted, never judged.
    pub over_cap: u32,
    /// Of `judged`, the ones answered from the verdict cache.
    pub cached: u32,
}

impl JudgeCounts {
    /// Whether every candidate the producer found got a definite verdict. A rule with nothing
    /// found is fully assessed by the deterministic pass alone.
    pub fn fully_assessed(&self) -> bool {
        self.unjudged == 0 && self.over_cap == 0
    }

    /// The candidates nobody looked at or no answer came back for: never judged (over the cap, a
    /// failed or unsent batch, no model) or unsure with no row. A held unsure row is not one.
    pub fn coverage_gaps(&self) -> u32 {
        self.unjudged.saturating_sub(self.held) + self.over_cap
    }
}

/// What a judged scan hands back.
#[derive(Debug, Clone, Default)]
pub struct JudgeOutcome {
    /// One finding per site of each confirmed candidate (a candidate written in four places
    /// is four rows), and one unsure row (`needs_review`) per site of each candidate the judge
    /// could not settle in a rule that had judged candidates.
    pub findings: Vec<Finding>,
    /// At most one `not_assessed` row per rule, and only for a rule with no judged candidate
    /// (one with a judged candidate shows its undecided ones as unsure rows in `findings`).
    pub not_assessed: Vec<Finding>,
    /// Per ARMED rule.
    pub counts: BTreeMap<CloneRule, JudgeCounts>,
    /// A batch that failed, disclosed in the methodology.
    pub failed: Vec<FailedPass>,
    /// Model calls made (cache hits cost none).
    pub model_calls: u32,
    /// A model was available for this scan.
    pub model_available: bool,
    /// Files the clone index read.
    pub files_read: u32,
    /// A cap stopped the index: an absence proves nothing.
    pub index_truncated: bool,
    /// Files UI-SHARED-PRIMITIVE-1 could read (React-family packages); zero means that rule had
    /// nothing in scope, so it cannot claim the absence of anything.
    pub ui_files: u32,
    /// The model the judge ran on, when it made at least one call.
    pub judge_model: Option<String>,
}

/// Everything the judge needs besides the model and the cache.
pub struct JudgeRequest<'a> {
    pub repo: &'a str,
    /// The scan's files (the same set the candidates were built from).
    pub files: &'a [(String, String)],
    pub set: CandidateSet,
    pub judge_model: &'a str,
    /// The rule corpus, for the client-facing definition quoted in the prompt. `None` degrades
    /// to the closed question alone.
    pub corpus: Option<&'a camerata_rules::RuleSet>,
    /// Why no model is available, for the not-assessed row (`ai_blocked_reason` or
    /// "deterministic-only scan"). Unused when a model is given.
    pub unavailable_reason: &'a str,
}

/// The rule's client-facing definition for the prompt: the default option's directive and the
/// rule's `qualifies` text.
fn rule_text(rule: CloneRule, corpus: Option<&camerata_rules::RuleSet>) -> String {
    let Some(r) = corpus.and_then(|c| c.get_by_id(rule.id())) else {
        return String::new();
    };
    let directive = r
        .options
        .iter()
        .find(|o| o.id == findings::default_option_id(rule))
        .map(|o| o.directive.clone())
        .unwrap_or_default();
    format!("{directive}\nScope: {}", r.summary)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// The one delimiter token of a judge run (every rule, every batch of one repo), drawn so that it
/// occurs in none of the excerpts it will frame. Under a run's seed it is the same token every time
/// for the same candidates, which is what lets a stored reply be found again by its request.
pub(crate) fn judge_nonce(
    req: &JudgeRequest<'_>,
    with_model: bool,
) -> crate::prompt_frame::FrameNonce {
    if with_model {
        let texts: Vec<String> = req
            .set
            .by_rule
            .values()
            .flatten()
            .map(|c| candidate_json(&judge_input(c, req.files)))
            .collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        crate::prompt_frame::FrameNonce::generate_avoiding(&refs)
    } else {
        crate::prompt_frame::FrameNonce::generate()
    }
}

/// One batch the judge asks about: the request exactly as it is sent, the candidate ids it asks
/// about and how the batch is named in a failure record.
pub(crate) struct JudgeBatch {
    pub request: LlmRequest,
    pub asked: Vec<String>,
    pub which: String,
}

/// The request for batch number `batch_idx` (of `n_batches`) of `rule`'s pending candidates. THE
/// one place a judge request is built: the scan asks with it, and the recheck of a saved run
/// rebuilds the very same request from it to find the stored reply (see [`redo`]).
pub(crate) fn batch_request(
    req: &JudgeRequest<'_>,
    nonce: &crate::prompt_frame::FrameNonce,
    rule: CloneRule,
    batch: &[&Candidate],
    batch_idx: usize,
    n_batches: usize,
) -> JudgeBatch {
    let system = crate::prompt_frame::with_untrusted_clause(system_prompt(rule), nonce);
    let text = rule_text(rule, req.corpus);
    let inputs: Vec<_> = batch.iter().map(|c| judge_input(c, req.files)).collect();
    let (prompt, prefix_len) = batch_prompt(req.repo, rule, &text, &inputs, nonce);
    // The judge always names its model (the transport refuses a request that names none).
    let request = LlmRequest::new(prompt)
        .with_system(system)
        .with_max_tokens(scaled_max_tokens(batch.len(), TOKENS_PER_CANDIDATE))
        .with_cache_prefix_len(prefix_len)
        .with_model(req.judge_model.to_string());
    let which = format!(
        "candidate batch {} of {n_batches} ({} candidates, {} to {})",
        batch_idx + 1,
        batch.len(),
        batch.first().map_or("", |c| c.id.as_str()),
        batch.last().map_or("", |c| c.id.as_str()),
    );
    JudgeBatch {
        request,
        asked: batch.iter().map(|c| c.id.clone()).collect(),
        which,
    }
}

/// Judge every armed rule's candidates. `llm == None` means no model this scan: nothing is
/// judged, nothing ships, and every armed rule with candidates gets a `not_assessed` row.
pub async fn judge_clone_candidates(
    llm: Option<&dyn LlmPort>,
    req: JudgeRequest<'_>,
    cache: &mut VerdictCache,
    meter: Option<&UsageMeter>,
    feedback: Option<(&TranscriptStore, &str)>,
) -> JudgeOutcome {
    let mut outcome = JudgeOutcome {
        model_available: llm.is_some(),
        files_read: req.set.index.files_read,
        index_truncated: req.set.index.truncated,
        ui_files: req.set.index.ui_files,
        ..JudgeOutcome::default()
    };
    // One delimiter token for the whole judge run (every rule, every batch), drawn so that it
    // occurs in none of the excerpts it will frame. The verdict cache key does not include it.
    let nonce = judge_nonce(&req, llm.is_some());
    for rule in CloneRule::ALL {
        let Some(candidates) = req.set.by_rule.get(&rule) else {
            continue; // not armed
        };
        let over_cap = req.set.over_cap.get(&rule).copied().unwrap_or(0);
        let mut counts = JudgeCounts {
            found: u32::try_from(candidates.len())
                .unwrap_or(u32::MAX)
                .saturating_add(over_cap),
            over_cap,
            ..JudgeCounts::default()
        };
        if candidates.is_empty() && over_cap == 0 {
            outcome.counts.insert(rule, counts);
            continue;
        }
        // One session per rule AND repo: a multi-repo scan shares one transcript key, and a
        // session named for the rule alone would be re-registered (and overwritten) by every
        // repo after the first.
        let session = format!("clone-judge:{}:{}", rule.id(), req.repo);
        register_transcript(feedback, &session, req.repo, rule, candidates, over_cap);

        let (judged, unsure_ids) = judge_rule(
            llm,
            &req,
            &nonce,
            rule,
            candidates,
            cache,
            meter,
            feedback,
            &session,
            &mut counts,
            &mut outcome,
        )
        .await;

        settle_rule(
            &req,
            rule,
            candidates,
            &judged,
            &unsure_ids,
            &mut counts,
            &mut outcome,
            llm.is_some(),
        );
        let call_failure = if llm.is_none() {
            Some("no model was available, so nothing was judged".to_string())
        } else {
            outcome
                .failed
                .iter()
                .find(|f| f.reason.starts_with(rule.id()))
                .map(|f| f.reason.clone())
        };
        finish_transcript(
            feedback,
            &session,
            rule,
            candidates,
            &judged,
            &counts,
            call_failure.as_deref(),
        );
        outcome.counts.insert(rule, counts);
    }
    cache.trim(MAX_CACHED_VERDICTS);
    outcome.judge_model = (outcome.model_calls > 0).then(|| req.judge_model.to_string());
    outcome
}

/// Settle one rule's candidates from the verdicts the model gave: count them, make the rows of the
/// confirmed ones, the held rows of the unsure ones and, when the rule has neither, its one
/// `not_assessed` row. Shared by the scan and by the recheck of a saved run, so both join a
/// verdict to the report the same way.
#[allow(clippy::too_many_arguments)]
pub(crate) fn settle_rule(
    req: &JudgeRequest<'_>,
    rule: CloneRule,
    candidates: &[Candidate],
    judged: &BTreeMap<String, Verdict>,
    unsure_ids: &HashSet<String>,
    counts: &mut JudgeCounts,
    outcome: &mut JudgeOutcome,
    with_model: bool,
) {
    // Findings, in rank order.
    for candidate in candidates {
        if let Some(verdict) = judged.get(&candidate.id) {
            if verdict.word == rule.affirmative_word() {
                counts.confirmed += 1;
                outcome.findings.extend(findings::confirmed_findings(
                    req.repo,
                    candidate,
                    &verdict.concept,
                    req.files,
                ));
            } else {
                counts.rejected += 1;
            }
        }
    }
    counts.judged = counts.confirmed + counts.rejected;
    counts.unjudged = u32::try_from(candidates.len())
        .unwrap_or(u32::MAX)
        .saturating_sub(counts.judged);

    // One rule, one surface. A candidate the model looked at and answered "unsure" about
    // is a Queue row marked unsure, held until the reviewer decides it (only when other
    // candidates were judged; a rule where nothing was settled is one not_assessed row).
    // A candidate nobody judged (over the cap, a failed or unsent batch) has no verdict to
    // show: it is a coverage gap the ledger discloses, never a held row. A rule gets its
    // single `not_assessed` row only when it produced no Queue row at all.
    if counts.judged > 0 {
        for candidate in candidates {
            if unsure_ids.contains(&candidate.id) && !judged.contains_key(&candidate.id) {
                counts.held += 1;
                outcome
                    .findings
                    .extend(findings::unsure_findings(req.repo, candidate, req.files));
            }
        }
    }
    if counts.confirmed == 0 && counts.held == 0 {
        if let Some(row) = not_assessed_row(req, rule, counts, with_model, outcome) {
            outcome.not_assessed.push(row);
        }
    }
}

/// Ask the model about one rule's uncached candidates, in batches. Returns the definite verdict
/// per candidate id (cached ones included); updates `counts.cached` and the outcome's call and
/// failure bookkeeping.
#[allow(clippy::too_many_arguments)]
async fn judge_rule(
    llm: Option<&dyn LlmPort>,
    req: &JudgeRequest<'_>,
    nonce: &crate::prompt_frame::FrameNonce,
    rule: CloneRule,
    candidates: &[Candidate],
    cache: &mut VerdictCache,
    meter: Option<&UsageMeter>,
    feedback: Option<(&TranscriptStore, &str)>,
    session: &str,
    counts: &mut JudgeCounts,
    outcome: &mut JudgeOutcome,
) -> (BTreeMap<String, Verdict>, HashSet<String>) {
    let mut judged: BTreeMap<String, Verdict> = BTreeMap::new();
    let mut unsure_ids: HashSet<String> = HashSet::new();
    let Some(llm) = llm else {
        return (judged, unsure_ids); // offline: nothing is judged, nothing is read from the cache
    };
    // Cached verdicts first: an unchanged family on a re-scan is never re-billed.
    let mut pending: Vec<&Candidate> = Vec::new();
    for candidate in candidates {
        let key = cache_key(candidate, req.judge_model);
        match cache.get(&key) {
            Some(hit) if rule.verdict_words().contains(&hit.verdict.as_str()) => {
                judged.insert(
                    candidate.id.clone(),
                    Verdict {
                        word: hit.verdict.clone(),
                        concept: hit.concept.clone(),
                    },
                );
                counts.cached += 1;
            }
            _ => pending.push(candidate),
        }
    }
    let mut first_failure: Option<String> = None;
    let mut stopped_by_limit = false;
    let mut attempts: Vec<AttemptRecord> = Vec::new();
    let n_batches = pending.chunks(BATCH_SIZE).count();
    'batches: for (batch_idx, batch) in pending.chunks(BATCH_SIZE).enumerate() {
        let JudgeBatch {
            request: first_request,
            which,
            ..
        } = batch_request(req, nonce, rule, batch, batch_idx, n_batches);
        let asked: HashSet<&str> = batch.iter().map(|c| c.id.as_str()).collect();
        // A reply that is not a verdict object gets ONE repair follow-up in this run, counted
        // as an attempt, before the batch is given up on.
        let mut request = first_request;
        let mut parsed: Option<prompt::ParsedBatch> = None;
        // A resume of a batch that was repaired: its first ask was turned down and stored failed,
        // and its repair reply is in the store. Going straight to the repair reply costs nothing;
        // asking the first request again would bill a reply that is turned down again.
        let mut first_round = 0;
        if !llm.replays(&request) && llm.replays(&repair_request(request.clone())) {
            request = repair_request(request);
            first_round = 1;
        }
        for round in first_round..JUDGE_ATTEMPTS {
            // Design A7: every model request passes the size check before it is sent. One that
            // does not fit the model's window is refused here, never cut and never sent; it is
            // disclosed as the failed part below and its candidates stay undecided (not
            // assessed, not clean).
            if let Err(too_large) = crate::ai_audit::request_budget::ensure_request_fits(&request) {
                first_failure.get_or_insert_with(|| too_large.to_string());
                append(feedback, session, &format!("batch not sent: {too_large}"));
                continue 'batches;
            }
            // SPEND LIMIT: every request is admitted like a scan pass or a calibration batch
            // (the stage check in the caller only decides whether the judge starts). One the
            // limit refuses is not sent, nor is any later one; its candidates stay undecided
            // (not assessed, not clean) and the stage is recorded as stopped part way. A request
            // the run store already holds a verdict for is answered from the store and costs
            // nothing, so the limit has nothing to refuse (a resume past its limit replays it).
            let _permit = match meter {
                Some(m) if !llm.replays(&request) => match m
                    .admit_for_stage(req.repo, crate::ai_audit::STAGE_CLONE_JUDGE)
                    .await
                {
                    Ok(permit) => Some(permit),
                    Err(_) => {
                        stopped_by_limit = true;
                        first_failure.get_or_insert_with(|| {
                            format!("{} before this batch was sent", m.stop_cause())
                        });
                        append(feedback, session, "batch not sent: the spend limit");
                        break 'batches;
                    }
                },
                _ => None,
            };
            if let Some(m) = meter {
                m.note_request(&request);
            }
            let request_key = crate::run_store::journal::request_key(&request);
            let sent = request.clone();
            // Streamed, so a transcript shows real output and the CLI leg keeps its heartbeat.
            let mut on_delta = |_: &str| {};
            outcome.model_calls += 1;
            let response = match llm.complete_streaming(request, &mut on_delta).await {
                Ok(response) => response,
                Err(e) => {
                    // A refused call carries what it was billed: the spend limit must see it,
                    // as it does for every other stage (and the outage stop watches every failed
                    // call).
                    if let Some(m) = meter {
                        m.record_failed(Stage::CloneJudge, &e);
                    }
                    first_failure.get_or_insert_with(|| format!("{e:#}"));
                    append(feedback, session, &format!("batch failed: {e:#}"));
                    attempts.push(attempt_record(
                        req.judge_model,
                        "transport",
                        &format!("{which}, attempt {}: {e:#}", round + 1),
                    ));
                    continue 'batches;
                }
            };
            if let Some(m) = meter {
                m.record_stage(Stage::CloneJudge, &response);
            }
            append(feedback, session, &response.text);
            let read = parse_verdicts(&response.text, rule, &asked);
            // A reply that names no asked candidate in the closed vocabulary leaves the whole
            // batch unjudged: it is turned down like one that is not a verdict object, never
            // stored good.
            if read.parsed && !read.verdicts.is_empty() {
                parsed = Some(read);
                break;
            }
            // Stored as failed, not as a success: a resume asks this request again instead of
            // replaying a reply that was turned down.
            llm.note_rejected_reply(
                &request_key,
                if read.parsed {
                    "the reply held no verdict for any candidate asked about, in the closed vocabulary"
                } else {
                    "the reply held no verdict object (a JSON object with a verdicts array)"
                },
                true,
            );
            attempts.push(attempt_record(
                req.judge_model,
                "content",
                &format!(
                    "{which}, attempt {}: not a verdict object; reply began: {}",
                    round + 1,
                    reply_sample(&response.text)
                ),
            ));
            request = repair_request(sent);
        }
        let Some(parsed) = parsed else {
            // Both asks were turned down: the batch is the disclosed failed part.
            first_failure
                .get_or_insert_with(|| "the judge's response was not a verdict object".to_string());
            continue;
        };
        for candidate in batch {
            let Some(verdict) = parsed.verdicts.get(&candidate.id) else {
                continue; // no verdict: unjudged
            };
            if verdict.word == "unsure" {
                counts.unsure += 1;
                unsure_ids.insert(candidate.id.clone());
                continue; // retried next scan: never cached, never a confirmed finding
            }
            cache.insert(
                cache_key(candidate, req.judge_model),
                CachedVerdict {
                    verdict: verdict.word.clone(),
                    concept: verdict.concept.clone(),
                    judged_at: now(),
                },
            );
            judged.insert(candidate.id.clone(), verdict.clone());
        }
    }
    if let Some(reason) = first_failure {
        outcome.failed.push(FailedPass {
            repo: req.repo.to_string(),
            pass: FAILED_PASS_NAME.to_string(),
            reason: format!("{}: {reason}", rule.id()),
            files_not_reviewed: 0,
            class: if stopped_by_limit {
                crate::ai_audit::FailureClass::SpendLimit
            } else {
                crate::ai_audit::FailureClass::Content
            },
            files: Vec::new(),
            attempts,
            scope_note: false,
        });
    }
    (judged, unsure_ids)
}

/// The one `not_assessed` row for a rule that left candidates undecided, if any.
fn not_assessed_row(
    req: &JudgeRequest<'_>,
    rule: CloneRule,
    counts: &JudgeCounts,
    model_available: bool,
    outcome: &JudgeOutcome,
) -> Option<Finding> {
    if counts.fully_assessed() {
        return None;
    }
    let id = rule.id();
    if !model_available {
        return Some(findings::not_assessed_row(
            req.repo,
            rule,
            NotAssessedReason::PassFailed,
            format!(
                "advisory judgment did not run ({}); {} candidate families found, 0 judged",
                req.unavailable_reason, counts.found
            ),
            format!(
                "{} candidate site groups for {id} were found by a deterministic search of the \
                 code, but the reviewed assessment that decides which of them are real did not \
                 run in this scan ({}), so the rule is reported as not assessed rather than \
                 clean.",
                counts.found, req.unavailable_reason
            ),
        ));
    }
    let failed = outcome.failed.iter().any(|f| f.reason.starts_with(id));
    let reason = if failed {
        NotAssessedReason::PassFailed
    } else {
        NotAssessedReason::ModelDeclined
    };
    if !failed {
        // The model answered; what it left unsettled is said in the operator's words.
        return Some(findings::not_assessed_row(
            req.repo,
            rule,
            reason,
            unsettled_summary(counts),
            format!(
                "{} of the {} candidate site groups for {id} were not settled in this scan: \
                 {}. Those candidates are not reported, so the rule is reported as not fully \
                 assessed rather than clean.",
                counts.unjudged + counts.over_cap,
                counts.found,
                unsettled_clauses(counts).join("; ")
            ),
        ));
    }
    let mut parts: Vec<String> = Vec::new();
    if counts.unjudged > 0 {
        parts.push(format!(
            "{} could not be decided{}",
            counts.unjudged,
            if failed { " (a request failed)" } else { "" }
        ));
    }
    if counts.over_cap > 0 {
        parts.push(format!(
            "{} were beyond the limit of {CLONE_JUDGE_MAX_PER_RULE} assessed per scan",
            counts.over_cap
        ));
    }
    let summary = parts.join("; ");
    Some(findings::not_assessed_row(
        req.repo,
        rule,
        reason,
        format!(
            "{} of {} candidate families were not assessed ({summary}); {} judged",
            counts.unjudged + counts.over_cap,
            counts.found,
            counts.judged
        ),
        format!(
            "{} of the {} candidate site groups for {id} were not assessed in this scan: \
             {summary}. Those candidates are not reported, so the rule is reported as not fully \
             assessed rather than clean.",
            counts.unjudged + counts.over_cap,
            counts.found
        ),
    ))
}

/// What the model left unsettled for a rule, as clauses in the operator's words.
fn unsettled_clauses(counts: &JudgeCounts) -> Vec<String> {
    let noun = |n: u32| if n == 1 { "candidate" } else { "candidates" };
    let mut clauses = Vec::new();
    if counts.unsure > 0 {
        clauses.push(format!(
            "the model was unsure about {} of {} {}",
            counts.unsure,
            counts.found,
            noun(counts.found)
        ));
    }
    let silent = counts.unjudged.saturating_sub(counts.unsure);
    if silent > 0 {
        clauses.push(format!("{silent} got no usable answer"));
    }
    if counts.over_cap > 0 {
        clauses.push(format!(
            "{} were beyond the limit of {CLONE_JUDGE_MAX_PER_RULE} assessed per scan",
            counts.over_cap
        ));
    }
    clauses
}

/// The row's short line: what was unsettled, then how many were judged.
fn unsettled_summary(counts: &JudgeCounts) -> String {
    format!(
        "{}; {} judged",
        unsettled_clauses(counts).join("; "),
        counts.judged
    )
}

fn register_transcript(
    feedback: Option<(&TranscriptStore, &str)>,
    session: &str,
    repo: &str,
    rule: CloneRule,
    candidates: &[Candidate],
    over_cap: u32,
) {
    let Some((store, key)) = feedback else {
        return;
    };
    let mut ranked = String::new();
    for c in candidates {
        let places: Vec<String> = c
            .facts
            .sites
            .iter()
            .map(|s| format!("{}:{}", s.path, s.lines()))
            .collect();
        ranked.push_str(&format!(
            "#{} {} agree={} {} tokens: {}\n",
            c.rank,
            c.id,
            c.facts.agree,
            c.facts.token_len,
            places.join(", ")
        ));
    }
    store.register(
        key,
        AgentTranscript {
            session_id: session.to_string(),
            role: format!(
                "clone judge {} on {repo}: {} candidates ranked ({over_cap} more over the cap)",
                rule.id(),
                candidates.len()
            ),
            prompt: format!("{}\n\nRanked candidates:\n{ranked}", system_prompt(rule)),
            output: String::new(),
            status: "running".to_string(),
        },
    );
}

fn append(feedback: Option<(&TranscriptStore, &str)>, session: &str, text: &str) {
    if let Some((store, key)) = feedback {
        store.append_output(key, session, text);
    }
}

fn finish_transcript(
    feedback: Option<(&TranscriptStore, &str)>,
    session: &str,
    rule: CloneRule,
    candidates: &[Candidate],
    judged: &BTreeMap<String, Verdict>,
    counts: &JudgeCounts,
    call_failure: Option<&str>,
) {
    let Some((store, key)) = feedback else {
        return;
    };
    for c in candidates {
        let line = match judged.get(&c.id) {
            Some(v) => format!("#{} {} -> {} {}", c.rank, c.id, v.word, v.concept),
            None => format!("#{} {} -> unjudged", c.rank, c.id),
        };
        store.append_output(key, session, &line);
    }
    store.append_output(
        key,
        session,
        &format!(
            "{}: found {}, judged {} ({} from cache), confirmed {}, unjudged {}, over cap {}",
            rule.id(),
            counts.found,
            counts.judged,
            counts.cached,
            counts.confirmed,
            counts.unjudged,
            counts.over_cap
        ),
    );
    // A call that answered is done, an honest "unsure" included; its line says what was not
    // settled. Only a call that really failed (no answer, an unusable answer, an error, the
    // spend limit) gets the failure look, with the reason.
    let (line, status) = match call_failure {
        Some(why) => (format!("Did not finish: {why}"), "blocked"),
        None => {
            let mut line = format!("Done: {} judged", counts.judged);
            if counts.unsure > 0 {
                line.push_str(&format!(", {} unsure", counts.unsure));
            }
            let silent = counts.unjudged.saturating_sub(counts.unsure);
            if silent > 0 {
                line.push_str(&format!(", {silent} with no usable answer"));
            }
            if counts.over_cap > 0 {
                line.push_str(&format!(", {} beyond the limit", counts.over_cap));
            }
            if counts.coverage_gaps() > 0 {
                line.push_str(" (reported as not checked)");
            } else if counts.held > 0 {
                line.push_str(" (held for a reviewer's decision)");
            }
            (line, "done")
        }
    };
    store.append_output(key, session, &line);
    store.set_status(key, session, status);
}

/// The ledger's skip reason for a rule whose judgment left candidates undecided. One producer for
/// the scan and for the recheck of a saved run.
pub fn partial_skip_reason(counts: &JudgeCounts) -> String {
    let held = if counts.held > 0 {
        format!("; {} left for the reviewer", counts.held)
    } else {
        String::new()
    };
    format!(
        "{FAILED_PASS_NAME} incomplete: {} of {} candidate sites were not assessed this scan \
         (over the limit or no answer){held}",
        counts.coverage_gaps(),
        counts.found
    )
}

/// Record every armed rule in the ledger, once per repo, under the architectural tier.
///
/// A rule is `ran` only when every candidate the producer found got a definite verdict (a rule
/// with nothing found ran clean). Otherwise it is recorded as not run with the reason, and the
/// matching coverage note is queued so the disclosure survives whichever later pass settles
/// `ran` (the semantic pass reads every rule too). Findings counted are the rows emitted, one
/// per site of each confirmed candidate, so the count agrees with the rows the report holds.
pub fn record_in_ledger(ledger: &mut ScanLedger, repo: &str, outcome: &JudgeOutcome) {
    for (rule, counts) in &outcome.counts {
        let id = rule.id();
        let fully = counts.coverage_gaps() == 0 && (outcome.model_available || counts.found == 0);
        let skip_reason = if fully {
            None
        } else if !outcome.model_available {
            ledger.note_advisory_unavailable_rule(repo, id);
            Some(crate::scan_ledger::ADVISORY_UNAVAILABLE_NOTE.to_string())
        } else {
            ledger.note_advisory_partial_rule(repo, id);
            Some(partial_skip_reason(counts))
        };
        let rows = outcome.findings.iter().filter(|f| f.rule_id == id).count();
        ledger.record_rule(
            repo,
            id,
            RuleTier::Architectural,
            fully,
            skip_reason,
            outcome.files_read as usize,
            rows,
        );
    }
}
