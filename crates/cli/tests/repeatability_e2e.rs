#![allow(clippy::unwrap_used)]
//! `camerata repeatability` end to end over the fixture repo with a FAKE completer (no network,
//! no spend): the estimate is printed before any model call, a run without `--max-cost-usd` or
//! with a limit below two estimates is refused before any model call, and two runs whose model
//! drifts report exactly the findings that differ, in text and JSON.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use camerata::repeatability_cmd::{
    run_repeatability_with, OutputFormat, RepeatabilityArgs, RepeatabilityOutcome, Seams,
    NO_CAP_REFUSAL,
};
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse, ProjectBackend};

/// A model that answers every call with one finding the two runs share plus one finding that
/// depends on which run is in progress: the drift the command exists to measure. Its rule ids
/// are not adopted rules, so the parser files them under `AI-` ids, as it does a real model's.
struct Drifting {
    run: Arc<AtomicUsize>,
    calls_per_run: Arc<Mutex<[usize; 3]>>,
    log: Arc<Mutex<Vec<String>>>,
    /// Every call made during this run fails (0 = none).
    fail_run: usize,
}

#[async_trait]
impl LlmPort for Drifting {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let run = self.run.load(Ordering::SeqCst);
        self.calls_per_run.lock().unwrap()[run] += 1;
        self.log.lock().unwrap().push("model call".to_string());
        if run == self.fail_run {
            anyhow::bail!("the model is unavailable");
        }
        let drift = if run == 1 {
            r#"{"rule":"DRIFT-ONLY-IN-RUN-ONE-1","severity":"high","path":"src/lib.rs","line":4,"code":"pub fn noop() {}","title":"first run only","detail":"reported by the first run only"}"#
        } else {
            r#"{"rule":"DRIFT-ONLY-IN-RUN-TWO-1","severity":"low","path":"src/client.rs","line":7,"code":"pub fn webhook_url() -> String {","title":"second run only","detail":"reported by the second run only"}"#
        };
        let text = format!(
            r#"{{"findings":[{{"rule":"SHARED-ACROSS-RUNS-1","severity":"medium","path":"src/db.rs","line":8,"code":"pub fn find_user_query(user_id: &str) -> String {{","title":"shared","detail":"reported by both runs"}},{drift}],"proposed_rules":[],"needs_files":[],"verdicts":[],"fixes":[],"recommendations":[]}}"#
        );
        Ok(LlmResponse {
            text,
            model: req.model.clone(),
            backend: "stub".to_string(),
            cost_usd: Some(0.001),
            input_tokens: Some(1_000),
            output_tokens: Some(100),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        })
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let r = self.complete(req).await?;
        on_delta(&r.text);
        Ok(r)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn stage_fixture(dest: &Path) {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../server/tests/fixtures/e2e_report_repo");
    fn copy(src: &Path, dest: &Path) {
        std::fs::create_dir_all(dest).unwrap();
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let to = dest.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy(&entry.path(), &to);
            } else {
                std::fs::copy(entry.path(), to).unwrap();
            }
        }
    }
    copy(&root, dest);
    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dest)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "repeatability-e2e@camerata.local"]);
    g(&["config", "user.name", "Repeatability E2E"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "fixture"]);
}

struct Harness {
    result: Result<RepeatabilityOutcome, String>,
    events: Vec<String>,
    calls_per_run: [usize; 3],
    _repo: tempfile::TempDir,
}

async fn run(cap: Option<f64>) -> Harness {
    run_failing(cap, 0).await
}

async fn run_failing(cap: Option<f64>, fail_run: usize) -> Harness {
    let repo = tempfile::tempdir().unwrap();
    stage_fixture(repo.path());
    let run_no = Arc::new(AtomicUsize::new(0));
    let calls_per_run = Arc::new(Mutex::new([0usize; 3]));
    let log = Arc::new(Mutex::new(Vec::new()));
    let llm = Drifting {
        run: run_no.clone(),
        calls_per_run: calls_per_run.clone(),
        log: log.clone(),
        fail_run,
    };
    let sink_log = log.clone();
    let start_log = log.clone();
    let result = run_repeatability_with(
        RepeatabilityArgs {
            repo: repo.path().to_path_buf(),
            backend: ProjectBackend::Api,
            model: None,
            calibration_model: None,
            max_cost_usd: cap,
            // The scripted model answers every call with scan JSON, so the terse scan's extra
            // full-explanation step (which needs its own reply shape) is switched off.
            efficiency: vec![
                "warm_schedule=off".to_string(),
                "terse_output=off".to_string(),
            ],
            format: OutputFormat::Text,
        },
        true,
        &camerata_server::credentials::MemoryCredentialStore::new(),
        Seams {
            llm: Some(&llm),
            preflight_sink: &|text| {
                assert!(text.contains("PRE-FLIGHT ESTIMATE"), "{text}");
                assert!(text.contains("two full runs"), "{text}");
                sink_log.lock().unwrap().push("preflight".to_string());
            },
            on_run_start: &|n| {
                run_no.store(n, Ordering::SeqCst);
                start_log.lock().unwrap().push(format!("run {n}"));
            },
        },
    )
    .await;
    let events = log.lock().unwrap().clone();
    let calls = *calls_per_run.lock().unwrap();
    Harness {
        result,
        events,
        calls_per_run: calls,
        _repo: repo,
    }
}

#[tokio::test]
async fn without_a_spend_limit_it_prints_the_estimate_then_refuses_before_any_model_call() {
    let h = run(None).await;
    let err = h.result.expect_err("no --max-cost-usd must be refused");
    assert_eq!(err, NO_CAP_REFUSAL);
    assert_eq!(
        h.events,
        vec!["preflight".to_string()],
        "estimate printed, nothing else"
    );
}

#[tokio::test]
async fn a_limit_below_two_estimates_is_refused_before_any_model_call() {
    let h = run(Some(0.000_001)).await;
    let err = h
        .result
        .expect_err("a limit below the estimate must be refused");
    assert!(err.contains("at least"), "{err}");
    assert_eq!(h.events, vec!["preflight".to_string()]);
}

#[tokio::test]
async fn two_runs_of_a_drifting_model_report_exactly_what_differs() {
    let h = run(Some(1_000.0)).await;
    let outcome = h.result.expect("both runs complete");

    // Order: the estimate, then run 1's calls, then run 2's.
    assert_eq!(h.events.first().map(String::as_str), Some("preflight"));
    let run2 = h.events.iter().position(|e| e == "run 2").unwrap();
    assert_eq!(h.events[1], "run 1");
    assert!(h.events[2..run2].iter().all(|e| e == "model call"));

    // Every cache bypassed: the second run asked the model as often as the first.
    assert!(h.calls_per_run[1] > 0);
    assert_eq!(
        h.calls_per_run[1], h.calls_per_run[2],
        "{:?}",
        h.calls_per_run
    );
    assert!(
        outcome.efficiency.contains("result_cache=off"),
        "{}",
        outcome.efficiency
    );

    assert!(outcome.complete, "{:?}", outcome.runs);
    assert_eq!(outcome.exit_code(), 0);
    assert!(!outcome.identical);
    let d = &outcome.diff;
    let first_only: Vec<&str> = d.only_in_first.iter().map(|f| f.rule_id.as_str()).collect();
    let second_only: Vec<&str> = d
        .only_in_second
        .iter()
        .map(|f| f.rule_id.as_str())
        .collect();
    assert_eq!(first_only, vec!["AI-DRIFT-ONLY-IN-RUN-ONE-1"]);
    assert_eq!(second_only, vec!["AI-DRIFT-ONLY-IN-RUN-TWO-1"]);
    assert!(
        d.matched >= 1,
        "the shared finding and the deterministic floor match"
    );
    let rule = |id: &str| d.by_rule.iter().find(|r| r.key == id).cloned().unwrap();
    assert_eq!(rule("AI-SHARED-ACROSS-RUNS-1").delta(), 0);
    assert_eq!(rule("AI-DRIFT-ONLY-IN-RUN-ONE-1").delta(), -1);
    assert_eq!(rule("AI-DRIFT-ONLY-IN-RUN-TWO-1").delta(), 1);
    let deterministic = d.by_tier.iter().find(|r| r.key == "deterministic").unwrap();
    assert_eq!(
        deterministic.delta(),
        0,
        "the floor is the control: it never drifts"
    );

    let text = outcome.render_text();
    for needle in [
        "REPEATABILITY",
        "By tier",
        "By severity",
        "By rule",
        "Only in run 1",
        "AI-DRIFT-ONLY-IN-RUN-ONE-1",
        "Only in run 2",
        "AI-DRIFT-ONLY-IN-RUN-TWO-1",
        "the runs DIFFER",
    ] {
        assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
    }

    let json: serde_json::Value = serde_json::from_str(&outcome.render_json()).unwrap();
    assert_eq!(json["identical"], false);
    assert_eq!(json["complete"], true);
    assert_eq!(json["runs"].as_array().unwrap().len(), 2);
    assert_eq!(
        json["diff"]["only_in_second"][0]["rule_id"],
        "AI-DRIFT-ONLY-IN-RUN-TWO-1"
    );
}

#[tokio::test]
async fn a_run_whose_scan_passes_failed_is_flagged_incomplete_and_exits_non_zero() {
    let h = run_failing(Some(1_000.0), 2).await;
    let outcome = h.result.expect("the comparison is still printed");
    assert!(outcome.runs[0].complete, "{:?}", outcome.runs[0]);
    assert!(!outcome.runs[1].complete, "{:?}", outcome.runs[1]);
    assert!(!outcome.runs[1].notes.is_empty());
    assert!(!outcome.complete);
    assert_eq!(
        outcome.exit_code(),
        camerata::repeatability_cmd::EXIT_INCOMPLETE
    );
    assert!(
        outcome
            .render_text()
            .contains("at least one run is incomplete"),
        "{}",
        outcome.render_text()
    );
}
