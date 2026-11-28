//! Redo a clone check part that did not finish, from the data a saved run holds (the recheck of an
//! incomplete inspection, `camerata inspect --recheck-saved`).
//!
//! A clone check part (one rule on one repo) fails when the judge's reply to a batch of candidates
//! cannot be read. The scan that made the run stored that reply under the digest of its request.
//! This module does only the delta:
//!
//! 1. [`plan_rule`] finds the repo's clone candidates again with the deterministic clone search
//!    and builds each batch's request with [`super::batch_request`], the one function the scan
//!    builds it with, so the request is the one the scan sent and its digest names the stored
//!    reply.
//! 2. [`read_stored`] reads that stored reply (and the stored repair reply of the batch) with
//!    the current reader. Zero model calls.
//! 3. Only for a batch no stored reply settles, [`ask_once`] sends the request once.
//! 4. [`settle`] joins the verdicts the way the scan does ([`super::settle_rule`]).
//!
//! The caller decides whether a call may be made at all; nothing here sends anything unless it
//! is handed a model and told to ask.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use camerata_checks::clone_rules::{Candidate, CandidateSet, CloneRule};

use super::prompt::{parse_verdicts, ParsedBatch, Verdict};
use super::{batch_request, judge_nonce, repair_request, settle_rule, JudgeBatch};
use super::{JudgeCounts, JudgeOutcome, JudgeRequest, BATCH_SIZE};
use crate::llm::LlmPort;

/// One batch of a rule's candidates, and what is known of its answer.
pub struct PlannedBatch {
    pub(crate) batch: JudgeBatch,
    /// The digest of the request, the name its stored reply is filed under.
    pub key: String,
    /// The verdicts a stored reply gave, when one could be read.
    pub read: Option<ParsedBatch>,
    /// The first build accepted a stored reply of this batch and merged its verdicts into the
    /// saved report: its rows are saved and are never made again.
    pub merged: bool,
    /// The run stored a call (answered or not) under this batch's request digest: the scan sent
    /// this very batch. A batch with none is one the scan never sent, or batched differently (its
    /// verdict cache held some candidates, and the run does not record which).
    pub sent: bool,
    /// A stored reply that reads for only part of the batch's candidates: `(read, asked)`. Such a
    /// reply does not settle the batch.
    pub partly_read: Option<(usize, usize)>,
}

impl PlannedBatch {
    /// How the batch is named to the operator: which candidates it asks about.
    pub fn describe(&self) -> String {
        match self.partly_read {
            Some((read, asked)) => format!(
                "{}; its stored reply covers only {read} of {asked} candidates",
                self.batch.which
            ),
            None => self.batch.which.clone(),
        }
    }

    /// The request, for the spend check and the listing of what would be asked.
    pub fn request(&self) -> &crate::llm::LlmRequest {
        &self.batch.request
    }
}

/// A rule's candidates found again, split into the batches the scan asked.
pub struct RulePlan {
    pub rule: CloneRule,
    pub repo: String,
    pub batches: Vec<PlannedBatch>,
    candidates: Vec<Candidate>,
    set: CandidateSet,
}

impl RulePlan {
    /// How many candidates the rule found (over the cap too).
    pub fn found(&self) -> u32 {
        self.set.found(self.rule)
    }

    /// The batches no stored reply settled, which a call may ask. A batch the first build merged
    /// is never one of them: its verdicts are already in the saved report.
    pub fn unread(&self) -> impl Iterator<Item = &PlannedBatch> {
        self.batches
            .iter()
            .filter(|b| b.read.is_none() && !b.merged)
    }

    /// The batches no stored call exists for, which this command never asks.
    pub fn unsent(&self) -> impl Iterator<Item = &PlannedBatch> {
        self.batches.iter().filter(|b| !b.sent)
    }

    /// The family id (`captures["family"]` of its rows) of every candidate asked in `batch`.
    fn families_of(batch: &PlannedBatch) -> Vec<String> {
        batch.batch.asked.iter().map(|id| family_of(id)).collect()
    }

    /// Tie the rule's saved rows to its batches, by candidate id, before anything is redone.
    ///
    /// Every saved row (a row of the report or a set-aside one) must name its candidate
    /// (`captures["family"]`) and that candidate must be one this search finds. A batch the first
    /// build merged must read now and cover every candidate asked, and each of its candidates
    /// judged confirmed or unsure must be accounted for: a row, a set-aside row, or a row the
    /// collapse folded into another (the site it cited is in `saved.folded`); each judged
    /// otherwise must have no row. No saved row may belong to a candidate of a batch that was not
    /// merged: redoing that batch would give the candidate a second row. Anything else cannot be
    /// tied, and the answer is the reason, in plain words, never a guess.
    pub fn tie_to_saved_rows(&self, saved: &SavedTraces<'_>) -> Result<(), String> {
        let all: HashSet<String> = self.candidates.iter().map(|c| family_of(&c.id)).collect();
        let mut have: HashSet<String> = HashSet::new();
        for row in saved.rows.iter().chain(&saved.discarded) {
            let Some(family) = row.captures.get("family") else {
                return Err("a saved row of the rule names no candidate".to_string());
            };
            if !all.contains(family) {
                return Err(format!(
                    "a saved row of the rule names a candidate ({family}) this search no longer \
                     finds"
                ));
            }
            have.insert(family.clone());
        }
        for batch in &self.batches {
            let families = Self::families_of(batch);
            if !batch.merged {
                if let Some(f) = families.iter().find(|f| have.contains(*f)) {
                    return Err(format!(
                        "{} has no merged reply but a saved row already names its candidate {f}, \
                         so redoing it would give that candidate a second row",
                        batch.describe()
                    ));
                }
                continue;
            }
            let Some(read) = &batch.read else {
                return Err(format!(
                    "{} was merged by the first build but its stored reply cannot be read now",
                    batch.describe()
                ));
            };
            let judged = batch
                .batch
                .asked
                .iter()
                .filter(|id| read.verdicts.contains_key(*id))
                .count();
            if judged < batch.batch.asked.len() {
                return Err(format!(
                    "{} was merged by the first build with a verdict for only {judged} of {} \
                     candidates, and this command does not ask again for part of a batch",
                    batch.describe(),
                    batch.batch.asked.len()
                ));
            }
            for (id, family) in batch.batch.asked.iter().zip(&families) {
                let shows = read
                    .verdicts
                    .get(id)
                    .is_some_and(|v| v.word == self.rule.affirmative_word() || v.word == "unsure");
                let row = have.contains(family);
                let accounted = row || self.folded_into_other_rows(id, saved);
                if (shows && !accounted) || (!shows && row) {
                    return Err(format!(
                        "the saved rows disagree with the stored verdict of candidate {family} in \
                         {}",
                        batch.describe()
                    ));
                }
            }
        }
        Ok(())
    }

    /// Whether a row the collapse folded into another row cites one of the candidate's sites under
    /// this rule.
    fn folded_into_other_rows(&self, id: &str, saved: &SavedTraces<'_>) -> bool {
        self.candidates
            .iter()
            .filter(|c| c.id == id)
            .flat_map(|c| &c.facts.sites)
            .any(|site| {
                saved
                    .folded
                    .iter()
                    .any(|(path, line)| *path == site.path && *line == site.start_line as usize)
            })
    }
}

/// What a report keeps of one rule's rows on one repo, for [`RulePlan::tie_to_saved_rows`].
pub struct SavedTraces<'a> {
    /// The rule's rows in the report, its not-assessed row left out.
    pub rows: Vec<&'a crate::onboard::Finding>,
    /// The rule's rows the scan set aside (stack exceptions and the like).
    pub discarded: Vec<&'a crate::onboard::Finding>,
    /// The `(path, line)` of every source the collapse folded into another row under this rule.
    pub folded: Vec<(&'a str, usize)>,
}

/// What a plan needs of the repo and the run.
pub struct RedoInput<'a> {
    pub repo: &'a str,
    /// The files the scan read for the repo (the same set the candidates were built from).
    pub files: &'a [(String, String)],
    /// The clone rules armed on the repo: every selected clone rule, since the delimiter token of
    /// a batch is drawn to avoid the text of every candidate of every armed rule.
    pub armed: &'a HashSet<&'a str>,
    pub rule: CloneRule,
    /// The clone check model the run recorded.
    pub judge_model: &'a str,
    pub corpus: Option<&'a camerata_rules::RuleSet>,
    /// The run folder whose stored replies are read.
    pub run_dir: &'a Path,
}

/// The family id a candidate's rows carry: its id without the rule's prefix.
fn family_of(id: &str) -> String {
    id.split_once(':').map_or(id, |(_, rest)| rest).to_string()
}

/// The clone rule a failed part's reason names (`FailedPass::reason` starts with the rule id).
pub fn rule_of_failure(reason: &str) -> Option<CloneRule> {
    CloneRule::ALL
        .into_iter()
        .find(|rule| reason.starts_with(rule.id()))
}

/// Rebuild the rule's batches and read what the run stored for each. Run it inside the run's
/// prompt seed ([`crate::prompt_frame::with_seed`]), or the delimiter token is not the one the
/// scan drew and no stored reply is found.
pub fn plan_rule(input: &RedoInput<'_>) -> RulePlan {
    let set = camerata_checks::clone_rules::candidates(input.files, input.armed);
    plan_set(input, set)
}

/// [`plan_rule`] over a candidate set already in hand (the digest tests hand in the set the scan
/// judged).
pub fn plan_set(input: &RedoInput<'_>, set: CandidateSet) -> RulePlan {
    let candidates = set.by_rule.get(&input.rule).cloned().unwrap_or_default();
    let req = JudgeRequest {
        repo: input.repo,
        files: input.files,
        set: set.clone(),
        judge_model: input.judge_model,
        corpus: input.corpus,
        unavailable_reason: "",
    };
    let nonce = judge_nonce(&req, true);
    let pending: Vec<&Candidate> = candidates.iter().collect();
    let n_batches = pending.chunks(BATCH_SIZE).count();
    let batches = pending
        .chunks(BATCH_SIZE)
        .enumerate()
        .map(|(idx, chunk)| {
            let batch = batch_request(&req, &nonce, input.rule, chunk, idx, n_batches);
            let key = crate::run_store::journal::request_key(&batch.request);
            let stored = read_stored(input.run_dir, input.rule, &batch);
            PlannedBatch {
                batch,
                key,
                read: stored.read,
                merged: stored.merged,
                sent: stored.sent,
                partly_read: stored.partly_read,
            }
        })
        .collect();
    RulePlan {
        rule: input.rule,
        repo: input.repo.to_string(),
        batches,
        candidates,
        set,
    }
}

/// What the run stored for a batch (its first ask, then its repair).
struct Stored {
    /// The verdicts of the reply the first build accepted when there is one, else of the first
    /// stored reply that reads for EVERY candidate of the batch.
    read: Option<ParsedBatch>,
    /// The first build accepted a reply: its verdicts are in the saved report.
    merged: bool,
    /// Any call is stored under the batch's request digest.
    sent: bool,
    /// The most a stored reply that reads but misses some candidates covers: `(read, asked)`.
    partly_read: Option<(usize, usize)>,
}

/// Nothing is sent.
fn read_stored(run_dir: &Path, rule: CloneRule, batch: &JudgeBatch) -> Stored {
    use crate::run_store::journal::EntryStatus;
    let asked: HashSet<&str> = batch.asked.iter().map(String::as_str).collect();
    let first = crate::run_store::journal::request_key(&batch.request);
    let repair = crate::run_store::journal::request_key(&repair_request(batch.request.clone()));
    let sent = [&first, &repair]
        .iter()
        .any(|key| crate::run_store::journal::has_stored_call(run_dir, key));
    let replies: Vec<(EntryStatus, String)> = [first, repair]
        .iter()
        .flat_map(|key| crate::run_store::journal::stored_replies(run_dir, key))
        .collect();
    let parse = |text: &str| {
        let read = parse_verdicts(text, rule, &asked);
        (read.parsed && !read.verdicts.is_empty()).then_some(read)
    };
    let accepted: Vec<&(EntryStatus, String)> = replies
        .iter()
        .filter(|(status, _)| *status == EntryStatus::Done)
        .collect();
    if !accepted.is_empty() {
        return Stored {
            read: accepted.iter().find_map(|(_, text)| parse(text)),
            merged: true,
            sent,
            partly_read: None,
        };
    }
    let mut partly_read: Option<(usize, usize)> = None;
    for (_, text) in &replies {
        let Some(read) = parse(text) else { continue };
        if read.verdicts.len() >= batch.asked.len() {
            return Stored {
                read: Some(read),
                merged: false,
                sent,
                partly_read: None,
            };
        }
        let covered = read.verdicts.len();
        if partly_read.is_none_or(|(most, _)| covered > most) {
            partly_read = Some((covered, batch.asked.len()));
        }
    }
    Stored {
        read: None,
        merged: false,
        sent,
        partly_read,
    }
}

/// What one asked call cost, to put on the run's usage and ledger.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpentCall {
    pub model: String,
    pub cost_usd: Option<f64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

/// The result of asking a batch once: the verdicts or why they could not be had, and what the
/// call cost when one was sent.
pub struct Asked {
    pub read: Result<ParsedBatch, String>,
    pub spent: Option<SpentCall>,
}

/// Ask `batch` once and read the reply, as the scan's judge asks: the request must fit the
/// model's window (never sent otherwise), and a reply that holds no verdict is recorded as
/// rejected through `llm` so the run never stores it as a good one.
pub async fn ask_once(llm: &dyn LlmPort, rule: CloneRule, batch: &PlannedBatch) -> Asked {
    let request = batch.batch.request.clone();
    if let Err(too_large) = crate::ai_audit::request_budget::ensure_request_fits(&request) {
        return Asked {
            read: Err(too_large.to_string()),
            spent: None,
        };
    }
    let key = crate::run_store::journal::request_key(&request);
    let mut on_delta = |_: &str| {};
    let response = match llm.complete_streaming(request, &mut on_delta).await {
        Ok(r) => r,
        Err(e) => {
            return Asked {
                read: Err(format!("the call failed: {e:#}")),
                spent: None,
            }
        }
    };
    let spent = Some(SpentCall {
        model: response.model.clone(),
        cost_usd: response.cost_usd,
        input_tokens: response.input_tokens.unwrap_or(0),
        output_tokens: response.output_tokens.unwrap_or(0),
        cache_read_input_tokens: response.cache_read_input_tokens,
        cache_creation_input_tokens: response.cache_creation_input_tokens,
    });
    let asked: HashSet<&str> = batch.batch.asked.iter().map(String::as_str).collect();
    let read = parse_verdicts(&response.text, rule, &asked);
    if read.parsed && !asked.is_empty() && read.verdicts.len() >= asked.len() {
        return Asked {
            read: Ok(read),
            spent,
        };
    }
    llm.note_rejected_reply(
        &key,
        if read.parsed {
            "the reply held no verdict for any candidate asked about, in the closed vocabulary"
        } else {
            "the reply held no verdict object (a JSON object with a verdicts array)"
        },
        true,
    );
    let left = asked.len().saturating_sub(read.verdicts.len());
    Asked {
        read: Err(if read.verdicts.is_empty() {
            "the reply still held no verdict for any candidate asked about".to_string()
        } else {
            format!(
                "the reply judged only {} candidates: {left} of {} candidates left",
                read.verdicts.len(),
                asked.len()
            )
        }),
        spent,
    }
}

/// Join the verdicts of every batch the way the scan does and return the rule's outcome: the
/// rows of the confirmed candidates, the held rows of the unsure ones and, when it has neither,
/// the rule's one not-assessed row, with the counts. Every batch must have its verdicts.
pub fn settle(
    plan: RulePlan,
    answers: &[ParsedBatch],
    files: &[(String, String)],
    judge_model: &str,
    corpus: Option<&camerata_rules::RuleSet>,
) -> (JudgeOutcome, JudgeCounts) {
    let rule = plan.rule;
    let mut judged: BTreeMap<String, Verdict> = BTreeMap::new();
    let mut unsure_ids: HashSet<String> = HashSet::new();
    let mut counts = JudgeCounts {
        found: plan.found(),
        over_cap: plan.set.over_cap.get(&rule).copied().unwrap_or(0),
        ..JudgeCounts::default()
    };
    for (planned, parsed) in plan.batches.iter().zip(answers) {
        for id in &planned.batch.asked {
            let Some(verdict) = parsed.verdicts.get(id) else {
                continue; // no verdict: unjudged
            };
            if verdict.word == "unsure" {
                counts.unsure += 1;
                unsure_ids.insert(id.clone());
            } else {
                judged.insert(id.clone(), verdict.clone());
            }
        }
    }
    // A batch the first build merged has its rows saved: it counts toward the rule's totals but
    // makes no row here, so no candidate gets a second one.
    let merged: HashSet<String> = plan
        .batches
        .iter()
        .filter(|b| b.merged)
        .flat_map(RulePlan::families_of)
        .collect();
    let kept = |f: &crate::onboard::Finding| {
        f.captures
            .get("family")
            .is_none_or(|family| !merged.contains(family))
    };
    let repo = plan.repo.clone();
    let req = JudgeRequest {
        repo: &repo,
        files,
        set: plan.set,
        judge_model,
        corpus,
        unavailable_reason: "",
    };
    let mut outcome = JudgeOutcome {
        model_available: true,
        files_read: req.set.index.files_read,
        index_truncated: req.set.index.truncated,
        ui_files: req.set.index.ui_files,
        ..JudgeOutcome::default()
    };
    settle_rule(
        &req,
        rule,
        &plan.candidates,
        &judged,
        &unsure_ids,
        &mut counts,
        &mut outcome,
        true,
    );
    outcome.findings.retain(kept);
    outcome.counts.insert(rule, counts.clone());
    (outcome, counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{LlmRequest, LlmResponse};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Never(AtomicUsize);

    #[async_trait::async_trait]
    impl LlmPort for Never {
        async fn complete(&self, _: LlmRequest) -> anyhow::Result<LlmResponse> {
            self.0.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("must not be sent")
        }
        async fn complete_streaming(
            &self,
            r: LlmRequest,
            _: &mut (dyn for<'a> FnMut(&'a str) + Send),
        ) -> anyhow::Result<LlmResponse> {
            self.complete(r).await
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    /// A request that does not fit the model's window is refused, never sent (as the scan's judge
    /// does).
    #[tokio::test]
    async fn a_request_that_does_not_fit_the_window_is_never_sent() {
        let request =
            LlmRequest::new("x".repeat(20_000_000)).with_model("claude-haiku-4-5".to_string());
        let batch = PlannedBatch {
            batch: JudgeBatch {
                request,
                asked: vec!["c1".to_string()],
                which: "candidate batch 1 of 1".to_string(),
            },
            key: String::new(),
            read: None,
            merged: false,
            sent: true,
            partly_read: None,
        };
        let llm = Never(AtomicUsize::new(0));
        let asked = ask_once(&llm, CloneRule::SingleSourceLogic, &batch).await;
        assert!(asked.read.is_err());
        assert!(asked.spent.is_none());
        assert_eq!(llm.0.load(Ordering::SeqCst), 0, "nothing was sent");
    }
}
