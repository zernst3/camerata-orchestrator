//! R5 fix round: a resume replays everything the run stored, whatever its spend, and the spend
//! limit refuses only FRESH sends.
//!
//! Every test here runs through the real run-store journal (a first attempt stores its replies, a
//! second attempt opens the same run and asks again), with a counting stub underneath, because the
//! defects lived in the seam between the limit's admission and the journal's replay.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::*;
use crate::cost_cap::RunControls;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};
use crate::run_store::{NewRun, RunOrigin, RunStore};

const REPO: &str = "acme/widgets";

/// Answers every request with a reply every stage accepts and counts what it is asked. A prompt
/// containing `COSTLY` costs $10, any other $2.
struct Counting {
    calls: AtomicUsize,
    /// A request whose prompt contains one of these fails (a pass the first attempt never got).
    fail_on: Vec<&'static str>,
}

impl Counting {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            fail_on: Vec::new(),
        }
    }
    fn failing_on(fail_on: &[&'static str]) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            fail_on: fail_on.to_vec(),
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl LlmPort for Counting {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_on.iter().any(|m| req.prompt.contains(m)) {
            anyhow::bail!("the provider could not be reached");
        }
        let cost = if req.prompt.contains("COSTLY") {
            10.0
        } else {
            2.0
        };
        Ok(LlmResponse {
            reported_model: None,
            text: r#"{"findings":[],"real":"yes","reason":"ok"}"#.to_string(),
            model: req.model.clone(),
            backend: "api".to_string(),
            cost_usd: Some(cost),
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

fn state_dir() -> (tempfile::TempDir, camerata_test_env::EnvGuard) {
    let dir = tempfile::tempdir().unwrap();
    let guard = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", dir.path());
    (dir, guard)
}

fn git_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dir.path())
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "t@example.com"]);
    g(&["config", "user.name", "T"]);
    std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "one"]);
    dir
}

fn new_run(repo: &tempfile::TempDir) -> Arc<RunStore> {
    RunStore::create(NewRun {
        project_key: "proj-r5".to_string(),
        repos: vec![(REPO.to_string(), repo.path().to_path_buf())],
        models: crate::run_store::ModelsRecord {
            audit: "claude-a".to_string(),
            calibration: "claude-a".to_string(),
            fix: "claude-a".to_string(),
            backend: "api".to_string(),
            mode: "parallel".to_string(),
            resolved: true,
        },
        ai_scan_key: crate::scan_cache::AiScanKey {
            audit_model: "claude-a".to_string(),
            calibration_model: String::new(),
            prompt_schema_version: 2,
            directives_fingerprint: "fp1".to_string(),
            terse_output: true,
            applies_to_routing: true,
        },
        args: serde_json::json!({"client": "x"}),
        estimate_usd: Some(10.0),
        include_alternatives: true,
        origin: RunOrigin::Cli,
        project_id: None,
    })
    .unwrap()
}

/// Run `fut` the way a run does: with its prompt seed in scope.
async fn seeded<F: std::future::Future>(fut: F) -> F::Output {
    crate::prompt_frame::with_seed(Some([9u8; crate::prompt_frame::NONCE_BYTES]), fut).await
}

fn capped(cap: Option<f64>, stored: f64) -> UsageMeter {
    UsageMeter::with_controls(RunControls {
        max_cost_usd: cap,
        stored_spend_usd: stored,
        ..RunControls::default()
    })
}

/// `n_batches` scan passes (one chunk, one rule each) through `llm`, one at a time.
async fn scan_round(llm: &dyn LlmPort, n_batches: usize, meter: &UsageMeter) -> usize {
    let files = vec![("a.rs".to_string(), "fn a() {}".to_string())];
    let chunks: Vec<&[(String, String)]> = vec![files.as_slice()];
    let rule_batches: Vec<Vec<(String, String)>> = (0..n_batches)
        .map(|n| vec![(format!("RULE-{n}"), format!("rule {n}"))])
        .collect();
    let batches: Vec<&[(String, String)]> = rule_batches.iter().map(Vec::as_slice).collect();
    let (_f, _p, _r, ok, _err) = run_passes(
        llm,
        REPO,
        &RepoMap::whole_text("REPO MAP:\nfoo.rs: fn foo()\n"),
        &crate::prompt_frame::FrameNonce::generate_avoiding(&[]),
        &std::collections::HashSet::new(),
        None,
        None,
        None,
        &chunks,
        None,
        &batches,
        1,
        "pass[all]",
        "r5r",
        Some(meter),
        false,
    )
    .await;
    ok
}

fn findings(n: usize) -> Vec<Finding> {
    (0..n)
        .map(|i| Finding {
            repo: REPO.to_string(),
            path: format!("src/f{i}.rs"),
            line: 3,
            rule_id: "ARCH-1".to_string(),
            severity: "high".to_string(),
            snippet: format!("snippet {i}"),
            detail: "detail".to_string(),
            ..Finding::default()
        })
        .collect()
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_resume_past_the_limit_replays_every_stored_scan_pass_and_sends_nothing_fresh() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    seeded(async {
        let first = Counting::new();
        let journal = crate::run_store::journal::wrap(&first, store.clone());
        let ok = scan_round(journal.as_ref(), 3, &capped(None, 0.0)).await;
        assert_eq!((ok, first.calls()), (3, 3), "the first attempt pays for 3");
    })
    .await;
    let id = store.run_id();
    drop(store);

    let store = RunStore::open(&id).unwrap();
    assert!(
        store.stored_usd() >= 6.0,
        "three passes at $2 are stored: {}",
        store.stored_usd()
    );
    seeded(async {
        let second = Counting::new();
        let journal = crate::run_store::journal::wrap(&second, store.clone());
        // The stored spend ($6) is past the $5 limit.
        let meter = capped(Some(5.0), store.stored_usd());
        let ok = scan_round(journal.as_ref(), 3, &meter).await;
        assert_eq!(ok, 3, "every stored pass is replayed");
        assert_eq!(second.calls(), 0, "nothing fresh is sent");
        let record = meter.cost_cap_record().expect("a limit was set");
        assert!(
            record.not_run_passes.is_empty() && !record.stopped_the_run(),
            "stored work is not reported as NOT RUN: {:?}",
            record.not_run_passes
        );
    })
    .await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_resume_under_the_limit_sends_fresh_calls_until_it_reaches_it() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    seeded(async {
        // Five passes asked, the last three never answered: two are stored ($4).
        let first = Counting::failing_on(&["RULE-2", "RULE-3", "RULE-4"]);
        let journal = crate::run_store::journal::wrap(&first, store.clone());
        assert_eq!(scan_round(journal.as_ref(), 5, &capped(None, 0.0)).await, 2);
    })
    .await;
    let id = store.run_id();
    drop(store);

    let store = RunStore::open(&id).unwrap();
    seeded(async {
        let second = Counting::new();
        let journal = crate::run_store::journal::wrap(&second, store.clone());
        // $4 stored against a $7 limit: room for one fresh $2 pass, not for the other three.
        let meter = capped(Some(7.0), store.stored_usd());
        let ok = scan_round(journal.as_ref(), 5, &meter).await;
        let fresh = second.calls();
        assert!(
            (1..=2).contains(&fresh),
            "fresh calls until the limit: {fresh}"
        );
        assert_eq!(ok, 2 + fresh, "the stored passes and the fresh ones ran");
        let record = meter.cost_cap_record().expect("a limit was set");
        assert_eq!(
            record.not_run_passes.len(),
            3 - fresh,
            "only the passes the limit refused are NOT RUN"
        );
    })
    .await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_tutorly_shape_sends_no_fresh_scan_pass_when_the_whole_stored_spend_is_past_the_limit()
{
    // Stored: two scan passes ($4) and one costly calibration-stage call ($10), $14 in all
    // against a $5 limit. The scan and other calls alone ($4) are under the limit, which is what
    // let a fresh pass through before the whole stored figure was counted.
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    seeded(async {
        let first = Counting::failing_on(&["RULE-2"]);
        let journal = crate::run_store::journal::wrap(&first, store.clone());
        assert_eq!(scan_round(journal.as_ref(), 3, &capped(None, 0.0)).await, 2);
        journal
            .complete(LlmRequest::new("COSTLY calibrate").with_system(verify_system_prompt()))
            .await
            .unwrap();
    })
    .await;
    let id = store.run_id();
    drop(store);

    let store = RunStore::open(&id).unwrap();
    assert!(store.stored_usd() >= 14.0, "{}", store.stored_usd());
    seeded(async {
        let second = Counting::new();
        let journal = crate::run_store::journal::wrap(&second, store.clone());
        let meter = capped(Some(5.0), store.stored_usd());
        let ok = scan_round(journal.as_ref(), 3, &meter).await;
        assert_eq!(second.calls(), 0, "no fresh scan pass is sent");
        assert_eq!(ok, 2, "the two stored passes are replayed");
        let record = meter.cost_cap_record().expect("a limit was set");
        assert_eq!(
            record.not_run_passes.len(),
            1,
            "only the pass that was never stored is NOT RUN"
        );
    })
    .await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_calibration_checkpoint_replays_past_the_limit_and_starts_its_stage() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    let dir = store.checkpoint_dir();
    seeded(async {
        crate::checkpoint::with_run_checkpoints(Some(dir.clone()), async {
            let first = Counting::new();
            let journal = crate::run_store::journal::wrap(&first, store.clone());
            let (out, _) = verify_findings(
                journal.as_ref(),
                REPO,
                findings(4),
                None,
                None,
                Some(&capped(None, 0.0)),
                &Stage1Inputs::new("A repo."),
            )
            .await;
            assert_eq!(out.len(), 4);
            assert!(first.calls() >= 1, "the first attempt paid for calibration");
        })
        .await;
    })
    .await;
    let id = store.run_id();
    drop(store);

    let store = RunStore::open(&id).unwrap();
    seeded(async {
        crate::checkpoint::with_run_checkpoints(Some(dir.clone()), async {
            let second = Counting::new();
            let journal = crate::run_store::journal::wrap(&second, store.clone());
            let meter = capped(Some(1.0), store.stored_usd());
            assert!(meter.cap_reached(), "the stored spend is past the limit");
            // The stage gate: a stage with stored work starts, one without does not.
            assert!(
                !meter.stage_blocked_unless_stored(REPO, STAGE_CALIBRATION, true),
                "a stage with a stored checkpoint starts"
            );
            assert!(meter.stage_blocked_unless_stored(REPO, STAGE_FIX, false));
            let (out, failed) = verify_findings(
                journal.as_ref(),
                REPO,
                findings(4),
                None,
                None,
                Some(&meter),
                &Stage1Inputs::new("A repo."),
            )
            .await;
            assert_eq!(out.len(), 4);
            let _ = failed;
            assert_eq!(second.calls(), 0, "nothing fresh is sent");
            let record = meter.cost_cap_record().expect("a limit was set");
            assert!(
                record
                    .not_run_stages
                    .iter()
                    .all(|s| s.stage != STAGE_CALIBRATION),
                "stored calibration is not NOT RUN: {:?}",
                record.not_run_stages
            );
        })
        .await;
    })
    .await;
}

// ── the meter alone ─────────────────────────────────────────────────────────────────────────

fn reply(cost: f64) -> LlmResponse {
    LlmResponse {
        reported_model: None,
        text: String::new(),
        model: "m".into(),
        backend: "api".into(),
        cost_usd: Some(cost),
        input_tokens: Some(1),
        output_tokens: Some(1),
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        or_cache_discount: None,
        truncated: false,
    }
}

#[tokio::test]
async fn the_whole_stored_spend_closes_admission_before_any_stage_has_replayed() {
    // Stored $43.51 against a $25 limit: the first scan pass is refused, before a checkpoint
    // stage has put a cent of its stored usage on the meter.
    let meter = capped(Some(25.0), 43.51);
    assert!(meter.admit().await.is_err());
}

#[test]
fn a_replayed_checkpoint_reply_is_not_counted_twice_against_the_limit() {
    let meter = capped(Some(25.0), 20.0);
    assert!((meter.limit_spend_usd() - 20.0).abs() < 1e-9);
    meter.record_resumed(Stage::Calibration, &reply(10.0));
    assert!(
        (meter.limit_spend_usd() - 20.0).abs() < 1e-9,
        "the stored figure already holds it: {}",
        meter.limit_spend_usd()
    );
    assert!(
        (meter.spent_usd() - 10.0).abs() < 1e-9,
        "the usage figures still show it"
    );
    meter.record_stage(Stage::Calibration, &reply(3.0));
    assert!((meter.limit_spend_usd() - 23.0).abs() < 1e-9);
}

#[test]
fn the_stage_that_continues_a_run_carries_the_replayed_share() {
    let controls = RunControls {
        max_cost_usd: Some(25.0),
        stored_spend_usd: 20.0,
        ..RunControls::default()
    };
    let first = UsageMeter::with_controls(controls.clone());
    first.record_resumed(Stage::Calibration, &reply(10.0));
    let next = UsageMeter::continuing(controls, &first.snapshot());
    assert!(
        (next.limit_spend_usd() - 20.0).abs() < 1e-9,
        "{}",
        next.limit_spend_usd()
    );
}
