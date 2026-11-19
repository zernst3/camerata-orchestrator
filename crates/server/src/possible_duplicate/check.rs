//! The possible-duplicate check: a model answers, for each candidate pair, whether the two
//! write-ups are the same flaw.
//!
//! # Why
//!
//! Place alone cannot tell "the same flaw written twice" from "a different flaw nearby": round 4
//! measured 187 candidate pairs, 31 inside one planted problem and 156 across different ones. The
//! places stay as CANDIDATES ([`super::DupIndex::candidates`]); a model judges each, and only the
//! pairs it answers `same` become the hint ([`super::DupIndex::new`]).
//!
//! # Where it sits
//!
//! In the scan, right after the group reviewer (the stage that produces the Fix ids the candidate
//! pairs are built from) and before the export. [`check_possible_duplicates`] is called at each
//! place the scan calls the group reviewer. Exports and the review view make no model call: they
//! read the stored answers ([`DuplicateChecks`], on the scan ledger and in the run folder as
//! [`FILE_NAME`]).
//!
//! # What it never does
//!
//! The hint is an operator review aid. A failed, skipped or stopped check never fails the run,
//! never holds or locks an export and never touches a count: it leaves no hints and one operator
//! note (a run note on the ledger), and a resume tries again (what an earlier try already read is
//! replayed from the stage checkpoint and costs nothing). A run with no candidate pair makes no
//! call. The model is only ever the project setting or `--dup-check-model` ([`resolve_dup_check_model`]);
//! this stage reads no environment variable.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use super::{CandidatePair, DupIndex};
use crate::ai_audit::stage_batch::{
    run_stage, ItemReject, StageCtx, StageFeedback, StageItems, StageMode, StageSettings,
};
use crate::ai_audit::{scaled_max_tokens, stage_model, UsageMeter};
use crate::llm::{LlmPort, LlmRequest, LlmResponse};
use crate::onboard::{Finding, ScanReport};
use crate::stage_telemetry::Stage;

/// The most candidate pairs one call asks about.
pub const MAX_PAIRS_PER_CALL: usize = 20;

/// The stage checkpoint's name.
pub const CHECKPOINT_NAME: &str = "duplicate-check";

/// The file in the run folder that holds the stored answers.
pub const FILE_NAME: &str = "duplicate_checks.json";

/// Output tokens budgeted per pair: the number, the verdict and one short reason.
const OUTPUT_TOKENS_PER_PAIR: u32 = 80;

/// The longest what/why text and code quote one row shows in a pair, in characters.
const MAX_DETAIL_CHARS: usize = 700;
const MAX_SNIPPET_CHARS: usize = 400;

/// The longest reason kept, in characters.
const MAX_REASON_CHARS: usize = 240;

/// The model the check runs on: the pick (the `--dup-check-model` flag or the project setting),
/// else [`camerata_api_types::project::DUP_CHECK_DEFAULT_MODEL`]. Never the environment.
pub fn resolve_dup_check_model(pick: Option<&str>) -> String {
    pick.map(str::trim).filter(|m| !m.is_empty()).map_or_else(
        || camerata_api_types::project::DUP_CHECK_DEFAULT_MODEL.to_string(),
        str::to_string,
    )
}

/// What the model said about one pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// The same flaw written up twice: fixing one fixes the other.
    Same,
    /// Two different flaws that sit near each other.
    Different,
    /// The text shown did not settle it.
    Unclear,
}

impl Verdict {
    fn parse(word: &str) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "same" => Some(Self::Same),
            "different" => Some(Self::Different),
            "unclear" => Some(Self::Unclear),
            _ => None,
        }
    }
}

/// The stored answer for one candidate pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairCheck {
    /// The site keys of the two rows, the one first in the file first.
    pub key_a: String,
    pub key_b: String,
    /// Their Fix ids.
    pub fix_a: String,
    pub fix_b: String,
    pub verdict: Verdict,
    /// The model's one short reason.
    pub reason: String,
    /// The model that answered.
    pub model: String,
}

/// Every stored answer of a run, and why there are none when the check did not run. Operator-only:
/// it lives on the scan ledger and in the run folder, and no client package carries it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuplicateChecks {
    #[serde(default)]
    pub pairs: Vec<PairCheck>,
    /// Why the check did not run (or did not finish); `None` when it ran, or had nothing to ask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_run: Option<String>,
}

impl DuplicateChecks {
    /// Whether there is nothing to store (so a ledger with none serializes as it always did).
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty() && self.not_run.is_none()
    }

    /// The model's reason when it answered `same` for the pair of rows `a` and `b` (in either
    /// order); `None` for any other answer or no answer.
    pub fn reason_if_same(&self, a: &str, b: &str) -> Option<String> {
        self.pairs
            .iter()
            .find(|p| {
                p.verdict == Verdict::Same
                    && ((p.key_a == a && p.key_b == b) || (p.key_a == b && p.key_b == a))
            })
            .map(|p| p.reason.clone())
    }
}

// ── The prompt ─────────────────────────────────────────────────────────────────────────────

/// The system prompt: one question per pair, and what `same` has to mean.
pub fn system_prompt() -> String {
    "You check pairs of write-ups from a code audit. Each pair is two findings that sit within a \
     few lines of each other in one file, found under different causes. For each pair decide \
     whether the two are the SAME flaw written up twice, or two DIFFERENT flaws that happen to \
     sit near each other.\n\n\
     Say `same` only when fixing one would fix the other. Say `different` when each needs its own \
     fix, even if they are about the same line or the same kind of problem. Say `unclear` when \
     the text shown does not settle it. Give one short reason, one sentence.\n\n\
     Answer with one JSON object and nothing else:\n\
     {\"pairs\": [{\"n\": 1, \"verdict\": \"same\", \"reason\": \"...\"}]}\n\
     Answer every numbered pair exactly once, with its number as given. The verdict is one of \
     `same`, `different` or `unclear`."
        .to_string()
}

fn clip(text: &str, max: usize) -> String {
    let one: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > max {
        one.chars().take(max.saturating_sub(3)).collect::<String>() + "..."
    } else {
        one
    }
}

fn row_text(label: &str, f: &Finding) -> String {
    format!(
        "{label}: rule {} | title: {} | place: {}:{} | severity: {}\n  what and why: {}\n  code: {}\n",
        f.rule_id,
        clip(&crate::ai_audit::finding_headline(f), 200),
        f.path,
        f.line,
        f.severity,
        clip(&f.detail, MAX_DETAIL_CHARS),
        clip(&f.snippet, MAX_SNIPPET_CHARS),
    )
}

fn pairs_prompt(
    path: &str,
    pairs: &[(&Finding, &Finding)],
    nonce: &crate::prompt_frame::FrameNonce,
) -> String {
    let mut body = String::new();
    for (i, (a, b)) in pairs.iter().enumerate() {
        body.push_str(&format!("Pair {}\n", i + 1));
        body.push_str(&row_text("A", a));
        body.push_str(&row_text("B", b));
        body.push('\n');
    }
    format!(
        "File: {path}\nThere are {} pairs below. Say for each whether it is the same flaw written \
         up twice.\n\n{}",
        pairs.len(),
        crate::prompt_frame::frame_untrusted(nonce, &format!("pairs in {path}"), &body)
    )
}

// ── The stage's items ──────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct Reply {
    pairs: Vec<ReplyPair>,
}

#[derive(Deserialize)]
struct ReplyPair {
    n: usize,
    verdict: String,
    #[serde(default)]
    reason: String,
}

/// The calls of the check, in the shape the shared runner takes.
struct PairItems {
    model: String,
    order: Vec<String>,
    requests: BTreeMap<String, LlmRequest>,
    pairs_of: BTreeMap<String, Vec<CandidatePair>>,
    answers: BTreeMap<String, Vec<PairCheck>>,
}

impl StageItems for PairItems {
    type Id = String;

    fn items(&self) -> Vec<(String, LlmRequest)> {
        self.order
            .iter()
            .map(|id| (id.clone(), self.requests[id].clone()))
            .collect()
    }

    fn absorb(&mut self, id: &String, resp: LlmResponse) -> Result<(), ItemReject> {
        let asked = &self.pairs_of[id];
        let json = crate::ai_audit::extract_json_object(&resp.text)
            .ok_or_else(|| ItemReject::new("the reply holds no JSON object"))?;
        let reply: Reply = serde_json::from_str(json)
            .map_err(|e| ItemReject::new(format!("the reply is not the object asked for: {e}")))?;
        let mut checks: Vec<Option<PairCheck>> = vec![None; asked.len()];
        for answer in reply.pairs {
            let Some(slot) = answer
                .n
                .checked_sub(1)
                .filter(|i| *i < asked.len() && checks[*i].is_none())
            else {
                continue;
            };
            let verdict = Verdict::parse(&answer.verdict).ok_or_else(|| {
                ItemReject::new(format!(
                    "pair {} has the verdict `{}`, which is not same, different or unclear",
                    answer.n, answer.verdict
                ))
            })?;
            let p = &asked[slot];
            checks[slot] = Some(PairCheck {
                key_a: p.key_a.clone(),
                key_b: p.key_b.clone(),
                fix_a: p.fix_a.clone(),
                fix_b: p.fix_b.clone(),
                verdict,
                reason: clip(&answer.reason, MAX_REASON_CHARS),
                model: self.model.clone(),
            });
        }
        if let Some(missing) = checks.iter().position(Option::is_none) {
            return Err(ItemReject::new(format!(
                "pair {} has no answer; every numbered pair needs one",
                missing + 1
            )));
        }
        self.answers
            .insert(id.clone(), checks.into_iter().flatten().collect());
        Ok(())
    }

    fn retry(&self, id: &String, reject: &ItemReject) -> Option<LlmRequest> {
        let mut again = self.requests.get(id)?.clone();
        again.prompt.push_str(&format!(
            "\n\nYour previous reply was refused: {}. Answer again with one JSON object, an \
             answer for every numbered pair.\n",
            reject.reason
        ));
        crate::ai_audit::request_budget::ensure_request_fits(&again).ok()?;
        Some(again)
    }
}

/// One candidate pair with the two rows it is about.
type PairRows<'a> = (CandidatePair, &'a Finding, &'a Finding);

/// One call: its checkpoint id, its pairs and the rows each pair shows.
type PlannedCall<'a> = (String, Vec<CandidatePair>, Vec<(&'a Finding, &'a Finding)>);

/// Split the candidate pairs into calls: one file at a time, at most [`MAX_PAIRS_PER_CALL`] each.
fn plan_calls<'a>(
    model: &str,
    candidates: &[CandidatePair],
    rows: &HashMap<&str, &'a Finding>,
) -> Vec<PlannedCall<'a>> {
    let mut by_file: BTreeMap<(String, String), Vec<PairRows<'a>>> = BTreeMap::new();
    for c in candidates {
        let (Some(a), Some(b)) = (rows.get(c.key_a.as_str()), rows.get(c.key_b.as_str())) else {
            continue;
        };
        by_file
            .entry((a.repo.clone(), a.path.clone()))
            .or_default()
            .push((c.clone(), a, b));
    }
    let mut out = Vec::new();
    for group in by_file.values() {
        for chunk in group.chunks(MAX_PAIRS_PER_CALL) {
            let id = crate::checkpoint::batch_key(
                "dup",
                // The model is part of the id: a resume on another model asks again, so the stored
                // answers never name a model that did not give them.
                std::iter::once(model).chain(
                    chunk
                        .iter()
                        .flat_map(|(c, _, _)| [c.key_a.as_str(), c.key_b.as_str()]),
                ),
            );
            out.push((
                id,
                chunk.iter().map(|(c, _, _)| c.clone()).collect(),
                chunk.iter().map(|(_, a, b)| (*a, *b)).collect(),
            ));
        }
    }
    out
}

// ── The run ────────────────────────────────────────────────────────────────────────────────

/// Ask the model about every candidate pair of `report` and store the answers on its ledger and in
/// the run folder. See the module docs for what a failure does (nothing but a note).
///
/// The model and the spend limit come from `controls`; usage is metered under its own stage
/// ([`Stage::DuplicateCheck`]) on a meter that continues the scan's, and what it spent is folded
/// back into the report's usage and stage telemetry.
pub async fn check_possible_duplicates(
    report: &mut ScanReport,
    llm: &dyn LlmPort,
    controls: &crate::cost_cap::RunControls,
    cli_backend: bool,
    job: Option<(&crate::jobs::JobStore, &str)>,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
) {
    let candidates = DupIndex::candidates(report.tree.as_ref(), &report.findings);
    if candidates.is_empty() {
        report
            .ledger
            .record_duplicate_checks(DuplicateChecks::default());
        return;
    }
    let model = controls.dup_check_model_for();
    let prior = report.actual_usage.clone().unwrap_or_default();
    let meter = UsageMeter::continuing(controls.clone(), &prior);
    if let Some((jobs, id)) = job {
        let (jobs, id) = (jobs.clone(), id.to_string());
        meter.set_job_sink(crate::cost_checkpoint::CheckpointSink::new(move |cp| {
            jobs.set_cost_checkpoint(&id, cp.clone());
        }));
    }
    if let Some(stop) = report
        .ledger
        .cost_cap()
        .and_then(|c| c.checkpoint.as_ref())
        .filter(|c| c.stopped)
    {
        meter.seed_checkpoint_stop(stop);
    }

    let (checks, note) =
        run_check(
            report,
            llm,
            &meter,
            &model,
            &candidates,
            cli_backend,
            job,
            feedback,
        )
        .await;

    // What the stage spent joins the scan's usage, run notes and stage telemetry.
    if meter.snapshot().calls > 0 {
        report.actual_usage = Some(meter.snapshot());
    }
    report.ledger.record_runner_notes(meter.notes());
    if let Some(mut cap) = meter.cost_cap_record() {
        // Only the spend joins the record: a part the limit left out of THIS operator-only stage
        // is a note below, never a stopped run. `absorb` would also copy these four, and the
        // shared runner records a not-run stage when the limit refuses a call, which would add a
        // failed pass and hold the export.
        cap.not_run_passes.clear();
        cap.not_run_stages.clear();
        cap.repos_not_finished.clear();
        cap.checkpoint = None;
        match report.ledger.cost_cap().cloned() {
            Some(mut earlier) => {
                earlier.absorb(cap);
                report.ledger.record_cost_cap(earlier);
            }
            None => report.ledger.record_cost_cap(cap),
        }
    }
    let fresh = meter.stage_telemetry(report.code_lines);
    if !fresh.stages.is_empty() {
        let mut telemetry = report
            .ledger
            .stage_telemetry()
            .cloned()
            .unwrap_or_else(|| fresh.clone());
        for record in &fresh.stages {
            match telemetry
                .stages
                .iter_mut()
                .find(|s| s.stage == record.stage)
            {
                Some(have) => *have = record.clone(),
                None => telemetry.stages.push(record.clone()),
            }
        }
        telemetry
            .stages
            .sort_by_key(|s| Stage::ALL.iter().position(|a| *a == s.stage));
        report.ledger.record_stage_telemetry(telemetry);
    }

    let stored = DuplicateChecks {
        pairs: checks,
        not_run: note.clone(),
    };
    if let Some(note) = &note {
        report.ledger.record_runner_notes(vec![note.clone()]);
    }
    write_file(&stored);
    report.ledger.record_duplicate_checks(stored);
}

/// Ask the model about `candidates` on `meter`, unless the spend limit or a cost checkpoint stops
/// the stage first: every pair answered, or none (with the reason).
pub(crate) async fn run_check(
    report: &ScanReport,
    llm: &dyn LlmPort,
    meter: &UsageMeter,
    model: &str,
    candidates: &[CandidatePair],
    cli_backend: bool,
    job: Option<(&crate::jobs::JobStore, &str)>,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
) -> (Vec<PairCheck>, Option<String>) {
    if meter.checkpoint_stopped() || meter.cap_reached() {
        let why = if meter.checkpoint_stopped() {
            "the run is paused at its cost checkpoint".to_string()
        } else {
            meter.stop_cause().to_string()
        };
        return (
            Vec::new(),
            Some(format!("the duplicate check did not run: {why}")),
        );
    }
    send_calls(
        report, llm, meter, model, candidates, cli_backend, job, feedback,
    )
    .await
}

/// Send the calls and collect the answers.
async fn send_calls(
    report: &ScanReport,
    llm: &dyn LlmPort,
    meter: &UsageMeter,
    model: &str,
    candidates: &[CandidatePair],
    cli_backend: bool,
    job: Option<(&crate::jobs::JobStore, &str)>,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
) -> (Vec<PairCheck>, Option<String>) {
    let rows: HashMap<&str, &Finding> = report
        .findings
        .iter()
        .map(|f| (f.site_key.as_str(), f))
        .collect();
    let texts: Vec<String> = report
        .findings
        .iter()
        .flat_map(|f| [f.path.clone(), f.detail.clone(), f.snippet.clone()])
        .collect();
    let avoid: Vec<&str> = texts.iter().map(String::as_str).collect();
    let nonce = crate::prompt_frame::FrameNonce::generate_avoiding(&avoid);
    let system = crate::prompt_frame::with_untrusted_clause(system_prompt(), &nonce);

    let mut items = PairItems {
        model: model.to_string(),
        order: Vec::new(),
        requests: BTreeMap::new(),
        pairs_of: BTreeMap::new(),
        answers: BTreeMap::new(),
    };
    for (id, pairs, shown) in plan_calls(model, candidates, &rows) {
        let path = shown
            .first()
            .map(|(a, _)| a.path.clone())
            .unwrap_or_default();
        let request = LlmRequest::new(pairs_prompt(&path, &shown, &nonce))
            .with_system(system.clone())
            .with_max_tokens(scaled_max_tokens(pairs.len(), OUTPUT_TOKENS_PER_PAIR))
            .with_temperature(crate::ai_audit::AUDIT_TEMPERATURE)
            .with_model(stage_model(Some(model)));
        items.order.push(id.clone());
        items.requests.insert(id.clone(), request);
        items.pairs_of.insert(id, pairs);
    }
    if items.order.is_empty() {
        return (Vec::new(), None);
    }

    let repo = report.repos.join(", ");
    let checkpoint = crate::checkpoint::stage_checkpoint(CHECKPOINT_NAME);
    let ctx = StageCtx {
        repo: &repo,
        model,
        realtime: llm,
        batch: None,
        meter: Some(meter),
        job,
        checkpoint: checkpoint.as_ref(),
        settings: StageSettings::default(),
        feedback: feedback.map(|(store, key)| StageFeedback {
            store,
            key,
            name: "Duplicate check",
        }),
    };
    let mode = StageMode::parallel_default(cli_backend);
    let ran = run_stage(Stage::DuplicateCheck, &mut items, mode, &ctx).await;
    if !ran.failed.is_empty() || ran.cancelled.is_some() {
        let first = ran
            .failed
            .first()
            .map_or_else(|| "the run was cancelled".to_string(), |f| f.reason.clone());
        return (
            Vec::new(),
            Some(format!(
                "the duplicate check did not run to the end ({} of {} calls had no answer: {first}); \
                 no same-place hints are shown, and a resume tries again",
                ran.failed.len().max(1),
                items.order.len()
            )),
        );
    }
    let checks = items
        .order
        .iter()
        .filter_map(|id| items.answers.get(id))
        .flatten()
        .cloned()
        .collect();
    (checks, None)
}

/// Write the stored answers to the run folder, when a run is in scope. Best effort: the ledger
/// holds them too, and a failed write is never a failed run.
fn write_file(stored: &DuplicateChecks) {
    let Some(dir) = crate::checkpoint::run_dir_in_scope() else {
        return;
    };
    if let Ok(text) = serde_json::to_string_pretty(stored) {
        let _ = std::fs::write(dir.join(FILE_NAME), text);
    }
}

/// Helpers for the tests of this module and of the index and exports that read its answers.
#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;

    /// The answers of a model that said `same` to every candidate pair of the rows.
    pub(crate) fn every_candidate_same(
        tree: Option<&crate::review_tree::ReviewTree>,
        findings: &[Finding],
    ) -> DuplicateChecks {
        DuplicateChecks {
            pairs: DupIndex::candidates(tree, findings)
                .into_iter()
                .map(|c| PairCheck {
                    key_a: c.key_a,
                    key_b: c.key_b,
                    fix_a: c.fix_a,
                    fix_b: c.fix_b,
                    verdict: Verdict::Same,
                    reason: "the same flaw".to_string(),
                    model: "scripted".to_string(),
                })
                .collect(),
            not_run: None,
        }
    }
}

#[cfg(test)]
mod tests;
