//! The possible-duplicate check with a scripted model (no network, no spend). Synthetic data only.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use camerata_api_types::audit::FindingKind;

use super::*;
use crate::cost_cap::RunControls;

/// A problem row of `rule` in `path` at `line`.
fn row(rule: &str, path: &str, line: usize, what: &str) -> Finding {
    Finding {
        repo: "acme/portal".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule.to_string(),
        severity: "high".to_string(),
        snippet: format!("call_{rule}()"),
        detail: what.to_string(),
        kind: Some(FindingKind::Defect),
        ..Finding::default()
    }
}

/// Two files with two candidate pairs each of two causes: `src/c.rs` (A at 10, B at 12) and
/// `src/d.rs` (A at 5, C at 7). Rule A has one cause over both files.
fn report() -> ScanReport {
    let mut report = crate::release_gate::test_scan_report();
    report.findings = vec![
        row("SEC-A-1", "src/c.rs", 10, "the first write-up"),
        row("SEC-B-1", "src/c.rs", 12, "the second write-up"),
        row("SEC-A-1", "src/d.rs", 5, "another write-up"),
        row("SEC-C-1", "src/d.rs", 7, "one more write-up"),
    ];
    report.assign_site_keys();
    report
}

fn response(text: &str, model: &str) -> LlmResponse {
    LlmResponse {
        reported_model: None,
        cli_cost_usd: None,
        text: text.to_string(),
        model: model.to_string(),
        backend: "stub".to_string(),
        cost_usd: Some(0.01),
        input_tokens: Some(1_000),
        output_tokens: Some(100),
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        or_cache_discount: None,
        truncated: false,
    }
}

/// A scripted model: `answer` gets each request's prompt and says what to reply; every request is
/// kept.
struct Script {
    answer: Box<dyn Fn(&str) -> String + Send + Sync>,
    seen: Mutex<Vec<LlmRequest>>,
    calls: AtomicUsize,
}

impl Script {
    fn new(answer: impl Fn(&str) -> String + Send + Sync + 'static) -> Self {
        Self {
            answer: Box::new(answer),
            seen: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl LlmPort for Script {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let text = (self.answer)(&req.prompt);
        let model = req.model.clone();
        self.seen.lock().unwrap().push(req);
        Ok(response(&text, &model))
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let resp = self.complete(req).await?;
        on_delta(&resp.text);
        Ok(resp)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// The reply of a model that answers every numbered pair of the prompt with `verdict`.
fn all(prompt: &str, verdict: &str) -> String {
    let n = prompt.matches("\nPair ").count() + usize::from(prompt.starts_with("Pair "));
    let pairs: Vec<_> = (1..=n)
        .map(
            |i| serde_json::json!({"n": i, "verdict": verdict, "reason": format!("{verdict} {i}")}),
        )
        .collect();
    serde_json::json!({ "pairs": pairs }).to_string()
}

/// `same` for the file `src/c.rs` and `different` for the other.
fn c_same_d_different(prompt: &str) -> String {
    if prompt.contains("File: src/c.rs") {
        all(prompt, "same")
    } else {
        all(prompt, "different")
    }
}

async fn run(report: &mut ScanReport, llm: &Script) {
    check_possible_duplicates(report, llm, &RunControls::default(), false, None, None).await;
}

fn hinted(report: &ScanReport, path: &str) -> bool {
    let idx = DupIndex::new(
        report.tree.as_ref(),
        &report.findings,
        report.ledger.duplicate_checks(),
    );
    report
        .findings
        .iter()
        .filter(|f| f.path == path)
        .all(|f| !idx.of(f).is_empty())
}

fn any_hint(report: &ScanReport) -> bool {
    let idx = DupIndex::new(
        report.tree.as_ref(),
        &report.findings,
        report.ledger.duplicate_checks(),
    );
    report.findings.iter().any(|f| !idx.of(f).is_empty())
}

#[test]
fn the_candidate_pairs_are_the_places_the_hint_has_always_paired() {
    let r = report();
    let pairs = DupIndex::candidates(r.tree.as_ref(), &r.findings);
    assert_eq!(pairs.len(), 2, "one pair in each file: {pairs:?}");
    for p in &pairs {
        assert_ne!(p.fix_a, p.fix_b, "two causes");
    }
}

#[tokio::test]
async fn only_the_pair_the_model_calls_the_same_flaw_is_hinted() {
    let mut r = report();
    let llm = Script::new(c_same_d_different);
    run(&mut r, &llm).await;
    assert_eq!(llm.calls(), 2, "one call for each file");
    assert!(hinted(&r, "src/c.rs"), "the pair answered same is hinted");
    assert!(
        !hinted(&r, "src/d.rs"),
        "the pair answered different is not"
    );
    // The reason the model gave is on the link, for the review detail.
    let idx = DupIndex::new(r.tree.as_ref(), &r.findings, r.ledger.duplicate_checks());
    let c = r.findings.iter().find(|f| f.path == "src/c.rs").unwrap();
    assert_eq!(idx.of(c)[0].reason, "same 1");
    // Both answers are stored, with the model that gave them.
    let stored = r.ledger.duplicate_checks();
    assert_eq!(stored.pairs.len(), 2);
    assert!(stored.pairs.iter().all(|p| p.model == "claude-sonnet-5-5"));
    assert!(stored.not_run.is_none());
}

#[tokio::test]
async fn an_unclear_pair_is_not_hinted() {
    let mut r = report();
    let llm = Script::new(|p| all(p, "unclear"));
    run(&mut r, &llm).await;
    assert_eq!(r.ledger.duplicate_checks().pairs.len(), 2, "stored");
    assert!(!any_hint(&r));
}

#[tokio::test]
async fn a_malformed_reply_gets_one_repair_turn() {
    let mut r = report();
    let llm = Script::new(|p| {
        if p.contains("File: src/c.rs") && !p.contains("previous reply was refused") {
            "I think these are the same.".to_string()
        } else {
            all(p, "same")
        }
    });
    run(&mut r, &llm).await;
    assert_eq!(
        llm.calls(),
        3,
        "the bad reply, its repair, and the other file"
    );
    assert!(hinted(&r, "src/c.rs") && hinted(&r, "src/d.rs"));
    let repairs = llm
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|q| q.prompt.contains("previous reply was refused"))
        .count();
    assert_eq!(repairs, 1, "exactly one repair turn");
}

#[tokio::test]
async fn a_second_malformed_reply_leaves_no_hints_and_one_note_and_fails_nothing() {
    let mut r = report();
    let failed_before = r.failed_passes.len();
    let reasons_before = r.incomplete_reasons();
    let llm = Script::new(|p| {
        if p.contains("File: src/c.rs") {
            "not json at all".to_string()
        } else {
            all(p, "same")
        }
    });
    run(&mut r, &llm).await;
    assert!(!any_hint(&r), "no same-place hint at all");
    let notes: Vec<&String> = r
        .ledger
        .runner_notes()
        .iter()
        .filter(|n| n.contains("duplicate check did not run"))
        .collect();
    assert_eq!(
        notes.len(),
        1,
        "one operator note: {:?}",
        r.ledger.runner_notes()
    );
    assert!(r.ledger.duplicate_checks().not_run.is_some());
    assert_eq!(r.failed_passes.len(), failed_before, "no failed part");
    assert_eq!(
        r.incomplete_reasons(),
        reasons_before,
        "the run is as complete as before"
    );
}

#[tokio::test]
async fn no_candidate_pair_means_no_model_call() {
    let mut r = crate::release_gate::test_scan_report();
    r.findings = vec![row("SEC-A-1", "src/c.rs", 10, "alone")];
    r.assign_site_keys();
    let llm = Script::new(|p| all(p, "same"));
    run(&mut r, &llm).await;
    assert_eq!(llm.calls(), 0);
    assert!(r.ledger.duplicate_checks().is_empty());
}

#[tokio::test]
async fn a_resume_replays_the_stored_answers_with_no_new_call() {
    let dir = tempfile::tempdir().unwrap();
    let run_dir = dir.path().join("run-1");
    let checkpoints = crate::checkpoint::checkpoint_dir_of(&run_dir);
    std::fs::create_dir_all(&checkpoints).unwrap();

    let mut first = report();
    let llm = Script::new(c_same_d_different);
    crate::checkpoint::with_run_checkpoints(
        Some(checkpoints.clone()),
        check_possible_duplicates(&mut first, &llm, &RunControls::default(), false, None, None),
    )
    .await;
    assert_eq!(llm.calls(), 2);
    // The answers are in the run folder too.
    let stored: DuplicateChecks =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join(FILE_NAME)).unwrap()).unwrap();
    assert_eq!(&stored, first.ledger.duplicate_checks());

    let mut resumed = report();
    let again = Script::new(|_| panic!("a resume must not ask the model again"));
    crate::checkpoint::with_run_checkpoints(
        Some(checkpoints),
        check_possible_duplicates(
            &mut resumed,
            &again,
            &RunControls::default(),
            false,
            None,
            None,
        ),
    )
    .await;
    assert_eq!(again.calls(), 0);
    assert!(hinted(&resumed, "src/c.rs") && !hinted(&resumed, "src/d.rs"));
    assert_eq!(
        resumed.ledger.duplicate_checks(),
        first.ledger.duplicate_checks()
    );
}

#[tokio::test]
async fn the_check_runs_on_the_set_model_and_is_metered_under_its_own_stage() {
    let mut r = report();
    let llm = Script::new(c_same_d_different);
    let mut controls = RunControls::default();
    controls.review.dup_check_model = "claude-opus-5-5".to_string();
    check_possible_duplicates(&mut r, &llm, &controls, false, None, None).await;
    assert!(llm
        .seen
        .lock()
        .unwrap()
        .iter()
        .all(|q| q.model == "claude-opus-5-5"));
    let telemetry = r.ledger.stage_telemetry().expect("stage telemetry");
    let row = telemetry
        .stages
        .iter()
        .find(|s| s.stage == Stage::DuplicateCheck)
        .expect("its own stage row");
    assert_eq!(row.usage.calls, 2);
    assert_eq!(r.actual_usage.as_ref().unwrap().calls, 2);

    // With nothing set, Sonnet 5.5.
    let mut r2 = report();
    let llm2 = Script::new(c_same_d_different);
    run(&mut r2, &llm2).await;
    assert!(llm2
        .seen
        .lock()
        .unwrap()
        .iter()
        .all(|q| q.model == "claude-sonnet-5-5"));
}

#[tokio::test]
async fn a_call_asks_about_at_most_twenty_pairs() {
    let mut r = crate::release_gate::test_scan_report();
    // 26 rows of 26 rules, one line apart in one file: many candidate pairs, none in one cause.
    r.findings = (0..26)
        .map(|i| {
            row(
                &format!("SEC-R{i}-1"),
                "src/big.rs",
                10 + i % 3 + (i / 3) * 4,
                "w",
            )
        })
        .collect();
    r.assign_site_keys();
    let candidates = DupIndex::candidates(r.tree.as_ref(), &r.findings);
    assert!(
        candidates.len() > MAX_PAIRS_PER_CALL,
        "{}",
        candidates.len()
    );
    let llm = Script::new(|p| all(p, "different"));
    run(&mut r, &llm).await;
    for q in llm.seen.lock().unwrap().iter() {
        assert!(q.prompt.matches("\nPair ").count() <= MAX_PAIRS_PER_CALL);
    }
    assert!(llm.calls() >= 2);
}

/// A spend-limit stop inside the check is a note, never a stopped run: the shared runner records a
/// not-run stage when the limit refuses a call, and none of that may reach the report's record.
fn assert_limit_stop_leaves_the_run_whole(
    r: &ScanReport,
    failed_before: usize,
    reasons: &[String],
) {
    assert!(!any_hint(r), "no hints");
    assert!(
        !r.ledger
            .cost_cap()
            .is_some_and(crate::cost_cap::CostCapRecord::stopped_the_run),
        "the check's limit stop must not stop the run: {:?}",
        r.ledger.cost_cap()
    );
    assert_eq!(r.failed_passes.len(), failed_before, "no failed pass");
    assert_eq!(
        &r.incomplete_reasons(),
        reasons,
        "the run is as complete as before"
    );
    assert_eq!(
        r.ledger.runner_notes().len(),
        1,
        "exactly one note: {:?}",
        r.ledger.runner_notes()
    );
    assert!(r.ledger.runner_notes()[0].contains("duplicate check"));
}

#[tokio::test]
async fn a_limit_reached_by_the_first_call_stops_the_check_and_not_the_run() {
    let mut r = report();
    let (failed, reasons) = (r.failed_passes.len(), r.incomplete_reasons());
    let controls = RunControls {
        max_cost_usd: Some(0.005),
        ..RunControls::default()
    };
    // One call at a time, so the first call's spend is on the meter before the second is admitted.
    let llm = Script::new(c_same_d_different);
    check_possible_duplicates(&mut r, &llm, &controls, true, None, None).await;
    assert_limit_stop_leaves_the_run_whole(&r, failed, &reasons);
}

#[tokio::test]
async fn a_cost_checkpoint_stop_stops_the_check_and_not_the_run() {
    let mut r = report();
    let (failed, reasons) = (r.failed_passes.len(), r.incomplete_reasons());
    r.ledger.record_cost_cap(crate::cost_cap::CostCapRecord {
        cap_usd: 100.0,
        spent_usd: 5.0,
        checkpoint: Some(crate::cost_checkpoint::CostCheckpoint {
            approved_usd: 100.0,
            spent_usd: 5.0,
            projected_usd: 120.0,
            pieces_done: 1,
            pieces_left: 3,
            stopped: true,
            new_estimate_usd: Some(120.0),
            difference_usd: Some(20.0),
            stopped_before: "agent finder".into(),
            reserve_usd: 0.0,
        }),
        ..Default::default()
    });
    let reasons_with_record = r.incomplete_reasons();
    let llm = Script::new(c_same_d_different);
    check_possible_duplicates(&mut r, &llm, &RunControls::default(), false, None, None).await;
    assert_eq!(llm.calls(), 0, "a paused run sends nothing");
    let _ = reasons;
    assert_limit_stop_leaves_the_run_whole(&r, failed, &reasons_with_record);
}

#[tokio::test]
async fn a_resume_on_a_different_model_asks_again_and_stores_the_new_model() {
    let dir = tempfile::tempdir().unwrap();
    let checkpoints = crate::checkpoint::checkpoint_dir_of(&dir.path().join("run-1"));
    std::fs::create_dir_all(&checkpoints).unwrap();

    let mut first = report();
    let llm = Script::new(c_same_d_different);
    crate::checkpoint::with_run_checkpoints(
        Some(checkpoints.clone()),
        check_possible_duplicates(&mut first, &llm, &RunControls::default(), false, None, None),
    )
    .await;
    assert_eq!(llm.calls(), 2);
    assert!(first
        .ledger
        .duplicate_checks()
        .pairs
        .iter()
        .all(|p| p.model == "claude-sonnet-5-5"));

    // The same model: nothing is asked again.
    let mut same = report();
    let none = Script::new(|_| panic!("the same model must not be asked again"));
    crate::checkpoint::with_run_checkpoints(
        Some(checkpoints.clone()),
        check_possible_duplicates(&mut same, &none, &RunControls::default(), false, None, None),
    )
    .await;
    assert_eq!(none.calls(), 0);

    // Another model: asked again, and the stored answers name it.
    let mut controls = RunControls::default();
    controls.review.dup_check_model = "claude-opus-5-5".to_string();
    let mut changed = report();
    let again = Script::new(c_same_d_different);
    crate::checkpoint::with_run_checkpoints(
        Some(checkpoints),
        check_possible_duplicates(&mut changed, &again, &controls, false, None, None),
    )
    .await;
    assert_eq!(again.calls(), 2, "a model change asks again");
    let stored = changed.ledger.duplicate_checks();
    assert_eq!(stored.pairs.len(), 2);
    assert!(
        stored.pairs.iter().all(|p| p.model == "claude-opus-5-5"),
        "{stored:?}"
    );
}
