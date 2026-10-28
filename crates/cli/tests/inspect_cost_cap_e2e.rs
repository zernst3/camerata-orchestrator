#![allow(clippy::unwrap_used)]
//! `camerata inspect` pre-flight estimate and `--max-cost-usd`, end to end over the fixture repo
//! with a SCRIPTED model (no network, no spend):
//!
//! * the estimate is printed BEFORE the first model call;
//! * a capped run stops, lists what it did not run, writes an export that says it is incomplete
//!   (manifest flag and README line, no dollar figure), and reports itself as capped so `main`
//!   exits non-zero;
//! * an uncapped run's export says nothing of the sort.

use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use camerata::inspect_cmd::{
    run_inspect_pipeline_with, InspectArgs, InspectOutcome, Seams, EXIT_COST_CAPPED,
};
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse, ProjectBackend};

struct Scripted {
    cost: f64,
    log: Arc<Mutex<Vec<String>>>,
    calls: AtomicUsize,
}

#[async_trait]
impl LlmPort for Scripted {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.log.lock().unwrap().push("model call".to_string());
        Ok(LlmResponse {
            text: r#"{"findings":[],"proposed_rules":[],"needs_files":[],"verdicts":[],"fixes":[],"recommendations":[]}"#
                .to_string(),
            model: req.model.clone(),
            backend: "stub".to_string(),
            cost_usd: Some(self.cost),
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

fn typst_on_path() -> bool {
    std::process::Command::new("typst")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some()
}

/// Whether the export can be built here. These tests are the only end-to-end proof that a capped
/// run's export says it is incomplete, so a missing `typst` must never read as a pass: under CI
/// (the `CI` environment variable is set) it FAILS the test; locally it prints a loud banner on
/// stderr and skips.
fn typst_available(test: &str) -> bool {
    if typst_on_path() {
        return true;
    }
    assert!(
        std::env::var_os("CI").is_none(),
        "{test}: `typst` is not on PATH, and this test cannot run without it. Install typst in \
         the CI image; a silent skip would hide a broken capped-run export."
    );
    eprintln!(
        "\n*** SKIPPED {test}: `typst` is not on PATH, so the capped-run export was NOT \
         exercised. Install typst (brew install typst) to run it. ***\n"
    );
    false
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
    g(&["config", "user.email", "cap-e2e@camerata.local"]);
    g(&["config", "user.name", "Cap E2E"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "fixture"]);
}

fn args(repo: &Path, export: &Path, cap: Option<f64>) -> InspectArgs {
    InspectArgs {
        repo: repo.to_path_buf(),
        export: export.to_path_buf(),
        backend: ProjectBackend::Api,
        batch: false,
        model: None,
        calibration_model: None,
        // A spend limit needs every model priced, and the fix stage's default (Haiku 5.5) has no
        // list price until the model registry carries it: a capped run names a priced fix model.
        fix_model: cap.map(|_| camerata_server::llm::DEFAULT_MODEL.to_string()),
        full: true,
        verbose: false,
        max_cost_usd: cap,
        efficiency: vec!["warm_schedule=off".to_string()],
    }
}

async fn run(
    cap: Option<f64>,
    cost: f64,
) -> (
    InspectOutcome,
    Vec<String>,
    std::path::PathBuf,
    tempfile::TempDir,
) {
    let repo = tempfile::tempdir().unwrap();
    stage_fixture(repo.path());
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("inspect.zip");
    let log = Arc::new(Mutex::new(Vec::new()));
    let llm = Scripted {
        cost,
        log: log.clone(),
        calls: AtomicUsize::new(0),
    };
    let sink_log = log.clone();
    let outcome = run_inspect_pipeline_with(
        args(repo.path(), &export, cap),
        true,
        &camerata_server::credentials::MemoryCredentialStore::new(),
        Seams {
            llm: Some(&llm),
            preflight_sink: &|text| {
                assert!(text.contains("PRE-FLIGHT ESTIMATE"), "{text}");
                sink_log.lock().unwrap().push("preflight".to_string());
            },
        },
    )
    .await
    .expect("the inspection completes");
    let events = log.lock().unwrap().clone();
    (outcome, events, export, out)
}

fn zip_entry(path: &Path, name: &str) -> String {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut text = String::new();
    archive
        .by_name(name)
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    text
}

// Serial: the pipeline reads process-env switches (dep audit, semgrep opt-out).
#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_estimate_is_printed_before_the_first_model_call() {
    if !typst_available("the_estimate_is_printed_before_the_first_model_call") {
        return;
    }
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
    let (outcome, events, _export, _dir) = run(None, 0.01).await;

    assert_eq!(
        events.first().map(String::as_str),
        Some("preflight"),
        "{events:?}"
    );
    assert!(
        events.iter().any(|e| e == "model call"),
        "the scan did call the model"
    );
    let text = outcome
        .preflight
        .clone()
        .expect("the estimate is on the outcome too");
    assert!(
        text.contains("an estimate, not a quote") && text.contains("basis:"),
        "{text}"
    );
    assert!(text.contains("spend limit: none"), "{text}");
    // The switch the flag turned off is stated in the final report.
    assert!(!outcome.efficiency.warm_schedule);
    let report = camerata::inspect_cmd::format_outcome_report(&outcome);
    assert!(report.contains("warm_schedule=off"), "{report}");
    assert!(!outcome.cost_capped());
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_capped_run_is_loud_incomplete_and_never_leaks_a_dollar_figure_into_the_export() {
    if !typst_available(
        "a_capped_run_is_loud_incomplete_and_never_leaks_a_dollar_figure_into_the_export",
    ) {
        return;
    }
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
    // Every call costs $1 against a $1.50 limit: the first call or two run, the rest cannot.
    let (outcome, events, export, _dir) = run(Some(1.5), 1.0).await;

    assert!(outcome.cost_capped(), "the limit stopped the run");
    assert_ne!(EXIT_COST_CAPPED, 0);
    let cap = outcome.cost_cap.as_ref().unwrap();
    assert!(cap.stopped_the_run());
    let calls = events.iter().filter(|e| *e == "model call").count();
    assert!(calls <= 3, "stopped early, got {calls} calls: {events:?}");

    // Operator output: plain, with the amounts and the NOT RUN list.
    let report = camerata::inspect_cmd::format_outcome_report(&outcome);
    assert!(report.contains("spend limit: $1.50"), "{report}");
    assert!(
        report.contains("REACHED") && report.contains("INCOMPLETE"),
        "{report}"
    );
    assert!(
        outcome.ledger_summary.contains("NOT RUN"),
        "{}",
        outcome.ledger_summary
    );

    // The export: marked incomplete in the manifest and the README, with no cost number.
    let manifest = zip_entry(&export, "manifest.json");
    let value: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    assert_eq!(value["inspection"]["cost_cap_reached"], true, "{manifest}");
    let readme = zip_entry(&export, "README.txt");
    assert!(readme.contains("Spend limit: reached"), "{readme}");
    assert!(readme.contains("INCOMPLETE"), "{readme}");
    for name in ["manifest.json", "README.txt", "findings.json"] {
        let text = zip_entry(&export, name);
        assert!(
            !text.contains("1.50") && !text.contains("max_cost") && !text.contains("cost_usd"),
            "{name} must carry no cost number"
        );
    }
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn an_uncapped_exports_manifest_says_the_run_was_not_capped() {
    if !typst_available("an_uncapped_exports_manifest_says_the_run_was_not_capped") {
        return;
    }
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
    let (outcome, _events, export, _dir) = run(None, 0.01).await;
    assert!(!outcome.cost_capped());
    let value: serde_json::Value =
        serde_json::from_str(&zip_entry(&export, "manifest.json")).unwrap();
    assert_eq!(value["inspection"]["cost_cap_reached"], false);
    assert!(!zip_entry(&export, "README.txt").contains("Spend limit"));
}

/// No typst needed: the refusal happens before the estimate, before any model call, before the
/// export is even considered.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_spend_limit_with_an_unpriced_model_is_refused_before_the_estimate_and_any_model_call() {
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
    let repo = tempfile::tempdir().unwrap();
    stage_fixture(repo.path());
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("inspect.zip");
    let run = |cap: Option<f64>| {
        let mut a = args(repo.path(), &export, cap);
        a.model = Some("some-free-model:free".to_string());
        a.efficiency = Vec::new();
        a
    };
    let log = Arc::new(Mutex::new(Vec::new()));
    let llm = Scripted {
        cost: 0.01,
        log: log.clone(),
        calls: AtomicUsize::new(0),
    };
    let sink_log = log.clone();
    let err = run_inspect_pipeline_with(
        run(Some(5.0)),
        true,
        &camerata_server::credentials::MemoryCredentialStore::new(),
        Seams {
            llm: Some(&llm),
            preflight_sink: &|_| sink_log.lock().unwrap().push("preflight".to_string()),
        },
    )
    .await
    .expect_err("an unenforceable limit must stop the run");
    let message = err.to_string();
    assert!(message.contains("`some-free-model:free`"), "{message}");
    assert!(message.contains("cannot be enforced"), "{message}");
    assert_eq!(err.exit_code(), 1);
    assert!(
        log.lock().unwrap().is_empty(),
        "no estimate and no model call: {:?}",
        log.lock().unwrap()
    );
    assert!(!export.exists(), "no export was written");
}
