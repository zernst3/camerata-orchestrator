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
//!   asked about and a failed batch can never promote a candidate. They are counted, disclosed
//!   with ONE `not_assessed` row per rule, and retried next scan (`unsure` is not cached).
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

use crate::ai_audit::{scaled_max_tokens, FailedPass, UsageMeter};
use crate::llm::{LlmPort, LlmRequest};
use crate::scan_ledger::{RuleTier, ScanLedger};
use crate::transcript::{AgentTranscript, TranscriptStore};

use super::Finding;

pub mod cache;
pub mod findings;
pub mod prompt;

#[cfg(test)]
mod tests;

pub use cache::{cache_key, CachedVerdict, VerdictCache, MAX_CACHED_VERDICTS};
use prompt::{batch_prompt, parse_verdicts, system_prompt, Verdict};

/// Candidates per model call.
pub const BATCH_SIZE: usize = 16;

/// Output tokens budgeted per candidate (a verdict object with a short concept).
const TOKENS_PER_CANDIDATE: u32 = 80;

/// The pass name a failed batch is disclosed under (`FailedPass::pass`).
pub const FAILED_PASS_NAME: &str = "clone-candidate judgment";

/// Resolve the model the clone judge runs on: the caller's explicit pick, else
/// `CAMERATA_CLONE_JUDGE_MODEL`, else the project's fast tier primary
/// (`camerata_fleet::tier::default_fast_model`). Never the audit model by default: this is
/// classification over bounded structured input, the fast band of the tier map
/// (`docs/decisions/2026-06-20_deterministic_model_tiering.md`).
pub fn resolve_clone_judge_model(caller_pick: Option<&str>) -> String {
    caller_pick
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("CAMERATA_CLONE_JUDGE_MODEL")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
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
}

/// What a judged scan hands back.
#[derive(Debug, Clone, Default)]
pub struct JudgeOutcome {
    /// One finding per confirmed candidate.
    pub findings: Vec<Finding>,
    /// At most one `not_assessed` row per rule that left candidates undecided.
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
        let session = format!("clone-judge:{}", rule.id());
        register_transcript(feedback, &session, req.repo, rule, candidates, over_cap);

        let judged = judge_rule(
            llm,
            &req,
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

        // Findings, in rank order.
        for candidate in candidates {
            if let Some(verdict) = judged.get(&candidate.id) {
                if verdict.word == rule.affirmative_word() {
                    counts.confirmed += 1;
                    outcome.findings.push(findings::confirmed_finding(
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

        if let Some(row) = not_assessed_row(&req, rule, &counts, llm.is_some(), &outcome) {
            outcome.not_assessed.push(row);
        }
        finish_transcript(feedback, &session, rule, candidates, &judged, &counts);
        outcome.counts.insert(rule, counts);
    }
    cache.trim(MAX_CACHED_VERDICTS);
    outcome.judge_model = (outcome.model_calls > 0).then(|| req.judge_model.to_string());
    outcome
}

/// Ask the model about one rule's uncached candidates, in batches. Returns the definite verdict
/// per candidate id (cached ones included); updates `counts.cached` and the outcome's call and
/// failure bookkeeping.
#[allow(clippy::too_many_arguments)]
async fn judge_rule(
    llm: Option<&dyn LlmPort>,
    req: &JudgeRequest<'_>,
    rule: CloneRule,
    candidates: &[Candidate],
    cache: &mut VerdictCache,
    meter: Option<&UsageMeter>,
    feedback: Option<(&TranscriptStore, &str)>,
    session: &str,
    counts: &mut JudgeCounts,
    outcome: &mut JudgeOutcome,
) -> BTreeMap<String, Verdict> {
    let mut judged: BTreeMap<String, Verdict> = BTreeMap::new();
    let Some(llm) = llm else {
        return judged; // offline: nothing is judged, nothing is read from the cache
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
    let system = system_prompt(rule);
    let text = rule_text(rule, req.corpus);
    let mut first_failure: Option<String> = None;
    for batch in pending.chunks(BATCH_SIZE) {
        let inputs: Vec<_> = batch.iter().map(|c| judge_input(c, req.files)).collect();
        let (prompt, prefix_len) = batch_prompt(req.repo, rule, &text, &inputs);
        let mut request = LlmRequest::new(prompt)
            .with_system(system.clone())
            .with_max_tokens(scaled_max_tokens(batch.len(), TOKENS_PER_CANDIDATE))
            .with_cache_prefix_len(prefix_len);
        if !req.judge_model.is_empty() {
            request = request.with_model(req.judge_model.to_string());
        }
        // Streamed, so a transcript shows real output and the CLI leg keeps its heartbeat.
        let mut on_delta = |_: &str| {};
        outcome.model_calls += 1;
        let response = match llm.complete_streaming(request, &mut on_delta).await {
            Ok(response) => response,
            Err(e) => {
                first_failure.get_or_insert_with(|| format!("{e:#}"));
                append(feedback, session, &format!("batch failed: {e:#}"));
                continue;
            }
        };
        if let Some(m) = meter {
            m.record(&response);
        }
        append(feedback, session, &response.text);
        let asked: HashSet<&str> = batch.iter().map(|c| c.id.as_str()).collect();
        let parsed = parse_verdicts(&response.text, rule, &asked);
        if !parsed.parsed {
            first_failure
                .get_or_insert_with(|| "the judge's response was not a verdict object".to_string());
        }
        for candidate in batch {
            let Some(verdict) = parsed.verdicts.get(&candidate.id) else {
                continue; // no verdict: unjudged
            };
            if verdict.word == "unsure" {
                continue; // retried next scan: never cached, never shipped
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
        });
    }
    judged
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
    store.set_status(
        key,
        session,
        if counts.fully_assessed() {
            "done"
        } else {
            "blocked"
        },
    );
}

/// Record every armed rule in the ledger, once per repo, under the architectural tier.
///
/// A rule is `ran` only when every candidate the producer found got a definite verdict (a rule
/// with nothing found ran clean). Otherwise it is recorded as not run with the reason, and the
/// matching coverage note is queued so the disclosure survives whichever later pass settles
/// `ran` (the semantic pass reads every rule too). Findings counted are the confirmed ones.
pub fn record_in_ledger(ledger: &mut ScanLedger, outcome: &JudgeOutcome) {
    for (rule, counts) in &outcome.counts {
        let id = rule.id();
        let fully = counts.fully_assessed() && (outcome.model_available || counts.found == 0);
        let skip_reason = if fully {
            None
        } else if !outcome.model_available {
            ledger.note_advisory_unavailable_rule(id);
            Some(crate::scan_ledger::ADVISORY_UNAVAILABLE_NOTE.to_string())
        } else {
            ledger.note_advisory_partial_rule(id);
            Some(format!(
                "{FAILED_PASS_NAME} incomplete: {} of {} candidate sites were not assessed this \
                 scan",
                counts.unjudged + counts.over_cap,
                counts.found
            ))
        };
        ledger.record_rule(
            id,
            RuleTier::Architectural,
            fully,
            skip_reason,
            outcome.files_read as usize,
            counts.confirmed as usize,
        );
    }
}

/// The informational convention rows (work order section 83): for each armed rule that ran
/// completely and found nothing to report, one `ConventionObserved` row stating the detector's
/// fact. Emitted only when ALL of these hold, and silent otherwise:
///
/// - the rule is fully assessed (every candidate judged; none unjudged or over the cap);
/// - nothing was confirmed;
/// - no row at all under the rule's id from ANY tier in `findings` (the semantic pass also
///   reads every rule, so an AI-tier row, a merged-away row or an absorbed site counts as a
///   trace);
/// - the index was not truncated and the scan was not incomplete;
/// - the corpus rule and its default option are available to name the convention.
///
/// Called after the semantic pass has merged its findings, because only then can it see them.
pub fn convention_rows(
    repo: &str,
    outcome: &JudgeOutcome,
    findings: &[Finding],
    corpus: Option<&camerata_rules::RuleSet>,
    scan_incomplete: bool,
) -> Vec<Finding> {
    if outcome.index_truncated || scan_incomplete {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (rule, counts) in &outcome.counts {
        let fully = counts.fully_assessed() && (outcome.model_available || counts.found == 0);
        if !fully || counts.confirmed > 0 {
            continue;
        }
        // The rule must have had something to read: a repo with no source at all, or no
        // React-family UI for the UI rule, has nothing to be consistent about.
        if outcome.files_read == 0 || (*rule == CloneRule::SharedPrimitive && outcome.ui_files == 0)
        {
            continue;
        }
        let id = rule.id();
        let any_trace = findings.iter().any(|f| {
            f.rule_id.eq_ignore_ascii_case(id)
                || f.also_matches.iter().any(|m| m.eq_ignore_ascii_case(id))
                || f.also_locations
                    .iter()
                    .any(|l| l.rule_id.eq_ignore_ascii_case(id))
        });
        if any_trace {
            continue;
        }
        let Some(label) = corpus
            .and_then(|c| c.get_by_id(id))
            .and_then(|r| {
                r.options
                    .iter()
                    .find(|o| o.id == findings::default_option_id(*rule) && !o.violates)
            })
            .map(|o| o.label.clone())
        else {
            continue;
        };
        out.push(findings::convention_row(
            repo,
            *rule,
            &label,
            outcome.files_read,
            counts.judged,
        ));
    }
    out
}
