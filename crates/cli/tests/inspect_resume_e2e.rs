#![allow(clippy::unwrap_used)]
//! Run to completion, builder B: a run is saved part by part and a resume runs only what remains.
//! Zach, 2026-10-28: "if a true failure happens (the AI service is down), he must be able to
//! re-trigger only that portion later", and "any interruption should cost minutes, never the
//! run".
//!
//! Over the fixture repo with a SCRIPTED model (no network, no spend). The model's answer to a
//! request depends only on the request, so a run that is interrupted by a failure and then
//! resumed must give the very export an uninterrupted run gives (the key test of item 3).

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use camerata::inspect_cmd::{
    resume_inspect, run_inspect_pipeline_with, InspectArgs, InspectError, InspectOutcome, Seams,
    EXIT_INCOMPLETE_RUN, EXIT_OUTAGE_STOPPED,
};
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse, ProjectBackend};
use camerata_server::run_store::journal::classify;
use camerata_server::run_store::PartKind;

/// What the scripted model refuses to answer while `down` is set.
#[derive(Clone, Copy, Default)]
struct Outage {
    /// Every fix-text call fails like an unreachable provider.
    fix_stage: bool,
    /// Every scan call whose prompt frames this file fails.
    scan_of: Option<&'static str>,
    /// Every scan call whose prompt frames this file gets an answer that holds no JSON.
    malformed_scan_of: Option<&'static str>,
}

struct Scripted {
    down: Mutex<Outage>,
    calls: AtomicUsize,
    /// `kind:digest` of every request sent, in order.
    sent: Mutex<Vec<(PartKind, String)>>,
}

impl Scripted {
    fn new(down: Outage) -> Self {
        Self {
            down: Mutex::new(down),
            calls: AtomicUsize::new(0),
            sent: Mutex::new(Vec::new()),
        }
    }
    fn set(&self, down: Outage) {
        *self.down.lock().unwrap() = down;
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
    fn sent_of(&self, kind: PartKind) -> usize {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| *k == kind)
            .count()
    }

    fn answer(&self, req: &LlmRequest) -> anyhow::Result<LlmResponse> {
        let (kind, _repo, files) = classify(req);
        let down = *self.down.lock().unwrap();
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.sent.lock().unwrap().push((kind, String::new()));
        let unreachable = (kind == PartKind::Fix && down.fix_stage)
            || (kind == PartKind::Scan
                && down
                    .scan_of
                    .is_some_and(|f| files.iter().any(|p| p.ends_with(f))));
        if unreachable {
            return Err(anyhow::Error::new(
                camerata_server::failure::LlmCallFailure::new(
                    camerata_server::failure::FailureClass::Transport,
                    "Anthropic API HTTP 529: overloaded",
                ),
            ));
        }
        let malformed = kind == PartKind::Scan
            && down
                .malformed_scan_of
                .is_some_and(|f| files.iter().any(|p| p.ends_with(f)));
        let text = match kind {
            _ if malformed => "I could not produce a JSON answer for these files.".to_string(),
            PartKind::Scan => {
                let findings: Vec<serde_json::Value> = files
                    .iter()
                    .map(|path| {
                        serde_json::json!({
                            "path": path, "line": 1, "severity": "medium",
                            "rule": "unchecked-input",
                            "title": format!("Input is not checked in {path}"),
                            "code": "// FIXTURE",
                            "detail": format!("{path} passes input on without checking it."),
                        })
                    })
                    .collect();
                serde_json::json!({"findings": findings, "proposed_rules": [], "needs_files": []})
                    .to_string()
            }
            PartKind::Calibration => {
                // One verdict per `[N] (severity` line of the prompt.
                let n = req
                    .prompt
                    .lines()
                    .filter(|l| l.starts_with('[') && l.contains("(severity"))
                    .count();
                let verdicts: Vec<serde_json::Value> = (0..n)
                    .map(|i| {
                        serde_json::json!({
                            "index": i, "severity": "medium", "confidence": "high",
                            "reason": "checked", "effort": "low",
                        })
                    })
                    .collect();
                serde_json::json!({ "verdicts": verdicts }).to_string()
            }
            PartKind::Fix => {
                let n = req
                    .prompt
                    .lines()
                    .filter(|l| l.starts_with("<<<UNTRUSTED ") && l.contains("\"finding "))
                    .count();
                let fixes: Vec<serde_json::Value> = (0..n)
                    .map(|i| serde_json::json!({"index": i, "fix": "Check the input before it is used."}))
                    .collect();
                serde_json::json!({ "fixes": fixes }).to_string()
            }
            _ => r#"{"findings":[],"proposed_rules":[],"needs_files":[],"verdicts":[],"fixes":[],"recommendations":[]}"#.to_string(),
        };
        Ok(LlmResponse {
            text,
            model: req.model.clone(),
            backend: "stub".to_string(),
            cost_usd: Some(0.01),
            input_tokens: Some(1_000),
            output_tokens: Some(100),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        })
    }
}

#[async_trait]
impl LlmPort for Scripted {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.answer(&req)
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let r = self.answer(&req)?;
        on_delta(&r.text);
        Ok(r)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn typst_available(test: &str) -> bool {
    camerata_test_env::typst_on_path_for(test)
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
    g(&["config", "user.email", "resume-e2e@camerata.local"]);
    g(&["config", "user.name", "Resume E2E"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "fixture"]);
}

fn args(repos: &[&Path], names: &[&str], export: &Path) -> InspectArgs {
    InspectArgs {
        repo: repos[0].to_path_buf(),
        also_repos: repos[1..].iter().map(|p| p.to_path_buf()).collect(),
        repo_names: names.iter().map(|n| (*n).to_string()).collect(),
        export: export.to_path_buf(),
        backend: ProjectBackend::Api,
        batch: false,
        realtime: true,
        model: None,
        calibration_model: None,
        fix_model: None,
        full: true,
        verbose: false,
        max_cost_usd: None,
        efficiency: vec![
            "warm_schedule=off".to_string(),
            "terse_output=off".to_string(),
        ],
        client: "Example Client Ltd".to_string(),
        client_engagement: true,
        run_client_tooling: false,
        consensus: false,
    }
}

fn seams<'a>(llm: &'a Scripted, sink: &'a Mutex<Vec<String>>) -> Seams<'a> {
    Seams {
        resume: None,
        llm: Some(llm),
        preflight_sink: Box::leak(Box::new(move |text: &str| {
            sink.lock().unwrap().push(text.to_string());
        })),
    }
}

fn store() -> camerata_server::credentials::MemoryCredentialStore {
    camerata_server::credentials::MemoryCredentialStore::new()
}

/// The decompressed entries of a zip by name.
fn entries(path: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut out = BTreeMap::new();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        out.insert(file.name().to_string(), bytes);
    }
    out
}

/// Keys that describe the run, not the findings: when it ran, what it cost, which attempt.
const RUN_METADATA: &[&str] = &[
    "generated_at",
    "sha256",
    "short_sha",
    "size_bytes",
    "bytes",
    "commit",
    "sha",
    "head",
    "branch",
    "dir",
    "scanned_at",
    "created_at",
    "started_at",
    "finished_at",
    "completed_at",
    "timestamp",
    "model_sessions",
    "actual_usage",
    "usage",
    "cost_usd",
    "cost",
    "duration_ms",
    "elapsed_ms",
    "stage_telemetry",
    "input_tokens",
    "output_tokens",
    "cache_read_input_tokens",
    "cache_creation_input_tokens",
    "calls",
];

fn strip_run_metadata(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(map) => {
            map.retain(|k, _| !RUN_METADATA.contains(&k.as_str()));
            map.values_mut().for_each(strip_run_metadata);
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(strip_run_metadata),
        _ => {}
    }
}

/// What two exports must agree on: the findings, the manifest's content, the README and the
/// workbook's sheets, with the run's own metadata left out. (The PDF is compiled from the same
/// data; its bytes carry the clock, so the data is what is compared.)
fn comparable(path: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let short_commit = regex::Regex::new(r"-[0-9a-f]{7}-").unwrap();
    let named_json = regex::Regex::new(r"acme-one-[0-9a-f]{7}([.-])").unwrap();
    let xlsx_time = regex::Regex::new(r"\d{4}-\d{2}-\d{2} \d{2}:\d{2} UTC").unwrap();
    let on_main = regex::Regex::new(r"\b[0-9a-f]{7} on main").unwrap();
    let any_time =
        regex::Regex::new(r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}(:\d{2})?Z?( UTC)?").unwrap();
    let hash40 = regex::Regex::new(r"[0-9a-f]{40}").unwrap();
    let named_text = regex::Regex::new(r"-[0-9a-f]{7}([.-])").unwrap();
    for (name, bytes) in entries(path) {
        // The file names carry the short commit, which differs between two checkouts.
        let name = short_commit.replace(&name, "-<commit>-").into_owned();
        if name.ends_with(".pdf") {
            continue;
        }
        if name.ends_with(".json") {
            let mut v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            strip_run_metadata(&mut v);
            let text = serde_json::to_string_pretty(&v).unwrap();
            out.insert(
                name,
                named_json
                    .replace_all(&text, "acme-one-<commit>$1")
                    .into_owned(),
            );
        } else if name.ends_with(".xlsx") {
            let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
            for i in 0..archive.len() {
                let mut file = archive.by_index(i).unwrap();
                let inner = file.name().to_string();
                if inner.starts_with("docProps") {
                    continue;
                }
                let mut text = String::new();
                let _ = file.read_to_string(&mut text);
                let text = xlsx_time.replace_all(&text, "<time>").into_owned();
                out.insert(
                    format!("{name}!{inner}"),
                    on_main.replace_all(&text, "<commit> on main").into_owned(),
                );
            }
        } else {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let text = any_time.replace_all(&text, "<time>").into_owned();
            let text = hash40.replace_all(&text, "<commit>").into_owned();
            out.insert(
                name,
                named_text.replace_all(&text, "-<commit>$1").into_owned(),
            );
        }
    }
    out
}

fn assert_same_exports(resumed: &Path, uninterrupted: &Path) {
    let a = comparable(resumed);
    let b = comparable(uninterrupted);
    assert_eq!(
        a.keys().collect::<Vec<_>>(),
        b.keys().collect::<Vec<_>>(),
        "the same files in the resumed export and the uninterrupted one"
    );
    for (name, text) in &a {
        if text != &b[name] {
            let other = &b[name];
            let first = text
                .lines()
                .zip(other.lines())
                .enumerate()
                .find(|(_, (x, y))| x != y);
            panic!(
                "{name} differs between the resumed and the uninterrupted run; first difference: \
                 {first:?}; lengths {} and {}",
                text.len(),
                other.len()
            );
        }
    }
}

struct Env {
    _state: tempfile::TempDir,
    _g1: camerata_test_env::EnvGuard,
    _g2: camerata_test_env::EnvGuard,
}

fn env() -> Env {
    let state = tempfile::tempdir().unwrap();
    let g1 = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
    let g2 = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
    Env {
        _state: state,
        _g1: g1,
        _g2: g2,
    }
}

async fn run(
    llm: &Scripted,
    repos: &[&Path],
    names: &[&str],
    export: &Path,
) -> Result<InspectOutcome, InspectError> {
    let sink = Mutex::new(Vec::new());
    run_inspect_pipeline_with(
        args(repos, names, export),
        true,
        &store(),
        seams(llm, &sink),
    )
    .await
}

async fn resume(
    llm: &Scripted,
    run_id: &str,
    export: Option<PathBuf>,
) -> Result<InspectOutcome, InspectError> {
    let sink = Mutex::new(Vec::new());
    resume_inspect(run_id, &[], export, None, true, &store(), seams(llm, &sink)).await
}

fn the_run_id(err: &InspectError) -> String {
    match err {
        InspectError::IncompleteRun(run) => run.run_id.clone().expect("the run was saved"),
        other => panic!("expected a refused export, got {other}"),
    }
}

/// One repo, the fix stage down: the export is refused, nothing is written; after the provider is
/// back the resume sends only the fix calls and the export is the uninterrupted run's.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_resume_after_a_failed_sub_pass_equals_the_uninterrupted_run() {
    if !typst_available("a_resume_after_a_failed_sub_pass") {
        return;
    }
    let _env = env();
    let repo = tempfile::tempdir().unwrap();
    stage_fixture(repo.path());
    let out = tempfile::tempdir().unwrap();

    // The uninterrupted run, in its own state.
    let baseline_export = out.path().join("baseline.zip");
    let baseline_repo = tempfile::tempdir().unwrap();
    stage_fixture(baseline_repo.path());
    let healthy = Scripted::new(Outage::default());
    let baseline = run(
        &healthy,
        &[baseline_repo.path()],
        &["acme/one"],
        &baseline_export,
    )
    .await
    .expect("the uninterrupted run completes");
    assert!(
        baseline.incomplete_reasons.is_empty(),
        "{:?}",
        baseline.incomplete_reasons
    );
    assert!(
        healthy.sent_of(PartKind::Fix) > 0,
        "the fixture reaches the fix stage"
    );
    let healthy_calls = healthy.calls();

    // The interrupted run: the fix stage is down.
    let export = out.path().join("resumed.zip");
    let flaky = Scripted::new(Outage {
        fix_stage: true,
        ..Outage::default()
    });
    let err = run(&flaky, &[repo.path()], &["acme/one"], &export)
        .await
        .expect_err("a client export of a run with a failed part is refused");
    // The fix calls fail like an unreachable provider, three in a row: the outage stop.
    assert_eq!(err.exit_code(), EXIT_OUTAGE_STOPPED);
    assert!(!export.exists(), "no zip is written for a client");
    let run_id = the_run_id(&err);
    let record = camerata_server::run_store::read_record(&run_id).unwrap();
    assert_eq!(
        record.status,
        camerata_server::run_store::RunStatus::Incomplete
    );
    let remaining: Vec<PartKind> = record.remaining_parts().iter().map(|p| p.kind).collect();
    assert!(remaining.contains(&PartKind::Fix), "{remaining:?}");
    assert!(!remaining.contains(&PartKind::Scan), "{remaining:?}");
    let sent_before = flaky.calls();
    // An incomplete run leaves no baseline behind: the incremental cache is written only when the
    // run is complete.
    let manifest = camerata_server::scan_cache::state_dir::manifest_cache_path(repo.path());
    assert!(
        !manifest.exists(),
        "an incomplete run is not a reusable baseline"
    );
    let ledger = camerata_server::scan_cache::state_dir::ledger_path(repo.path());
    let (failures_json, _md) = camerata_server::failure_report::paths_next_to(&ledger);
    let before: camerata_server::failure_report::FailureReport =
        serde_json::from_slice(&std::fs::read(&failures_json).unwrap()).unwrap();
    assert!(before.unresolved().count() >= 1, "the failure is on record");

    // The provider is back; resume.
    flaky.set(Outage::default());
    let outcome = resume(&flaky, &run_id, Some(export.clone()))
        .await
        .expect("the resume completes");
    assert!(
        outcome.incomplete_reasons.is_empty(),
        "{:?}",
        outcome.incomplete_reasons
    );
    assert!(
        export.exists(),
        "the export unlocks once everything is complete"
    );
    // Only the fix stage (and nothing it feeds) was sent again: no scan, calibration or
    // alternatives call.
    let resumed_calls = flaky.calls() - sent_before;
    assert!(
        resumed_calls < healthy_calls,
        "a resume sends less than a whole run ({resumed_calls} against {healthy_calls})"
    );
    assert_eq!(
        flaky.sent_of(PartKind::Scan),
        healthy.sent_of(PartKind::Scan),
        "no scan call was sent a second time"
    );
    assert_eq!(
        flaky.sent_of(PartKind::Calibration),
        healthy.sent_of(PartKind::Calibration),
        "no calibration call was sent a second time"
    );
    assert!(
        manifest.exists(),
        "a complete run becomes the reusable baseline"
    );
    let after: camerata_server::failure_report::FailureReport =
        serde_json::from_slice(&std::fs::read(&failures_json).unwrap()).unwrap();
    assert_eq!(after.unresolved().count(), 0, "nothing fails any more");
    assert!(
        after.entries.iter().all(|e| e.resolved_at.is_some()) && !after.entries.is_empty(),
        "the failures stay as history with the time they were resolved"
    );
    assert!(
        after.entries.iter().any(|e| e.attempts.len() >= 2),
        "the attempt history is kept"
    );
    let done = camerata_server::run_store::read_record(&run_id).unwrap();
    assert_eq!(done.status, camerata_server::run_store::RunStatus::Complete);
    assert!(done.attempt >= 2);
    assert_same_exports(&export, &baseline_export);

    // A complete run resumed again is a no-op that says so.
    let again = resume(&flaky, &run_id, None)
        .await
        .expect_err("nothing to resume");
    assert!(matches!(again, InspectError::AlreadyComplete(_)), "{again}");
    assert_eq!(again.exit_code(), 0);
    assert!(again.to_string().contains("nothing to resume"), "{again}");
}

/// A scan pass down: the resume asks the model for that pass only.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_resume_after_a_failed_scan_pass_equals_the_uninterrupted_run() {
    if !typst_available("a_resume_after_a_failed_scan_pass") {
        return;
    }
    let _env = env();
    let repo = tempfile::tempdir().unwrap();
    stage_fixture(repo.path());
    let out = tempfile::tempdir().unwrap();
    let baseline_export = out.path().join("baseline.zip");
    let baseline_repo = tempfile::tempdir().unwrap();
    stage_fixture(baseline_repo.path());
    let healthy = Scripted::new(Outage::default());
    run(
        &healthy,
        &[baseline_repo.path()],
        &["acme/one"],
        &baseline_export,
    )
    .await
    .expect("the uninterrupted run completes");

    let export = out.path().join("resumed.zip");
    let flaky = Scripted::new(Outage {
        scan_of: Some("src/db.rs"),
        ..Outage::default()
    });
    let err = run(&flaky, &[repo.path()], &["acme/one"], &export)
        .await
        .expect_err("refused");
    assert_eq!(err.exit_code(), EXIT_OUTAGE_STOPPED);
    assert!(!export.exists());
    let run_id = the_run_id(&err);
    flaky.set(Outage::default());
    let outcome = resume(&flaky, &run_id, Some(export.clone()))
        .await
        .expect("the resume completes");
    assert!(
        outcome.incomplete_reasons.is_empty(),
        "{:?}",
        outcome.incomplete_reasons
    );
    assert_same_exports(&export, &baseline_export);
}

/// Two repos, a different part down in each: one resume finishes both.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_resume_of_two_repos_with_a_failure_in_each_equals_the_uninterrupted_run() {
    if !typst_available("a_resume_of_two_repos") {
        return;
    }
    let _env = env();
    let out = tempfile::tempdir().unwrap();
    let names = ["acme/one", "acme/two"];
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (c, d) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    for dir in [&a, &b, &c, &d] {
        stage_fixture(dir.path());
    }
    let baseline_export = out.path().join("baseline.zip");
    let healthy = Scripted::new(Outage::default());
    run(&healthy, &[a.path(), b.path()], &names, &baseline_export)
        .await
        .expect("the uninterrupted run completes");

    let export = out.path().join("resumed.zip");
    let flaky = Scripted::new(Outage {
        fix_stage: true,
        scan_of: Some("src/client.rs"),
        ..Outage::default()
    });
    let err = run(&flaky, &[c.path(), d.path()], &names, &export)
        .await
        .expect_err("refused");
    assert_eq!(err.exit_code(), EXIT_OUTAGE_STOPPED);
    let run_id = the_run_id(&err);
    flaky.set(Outage::default());
    let outcome = resume(&flaky, &run_id, Some(export.clone()))
        .await
        .expect("the resume completes");
    assert!(
        outcome.incomplete_reasons.is_empty(),
        "{:?}",
        outcome.incomplete_reasons
    );
    assert_same_exports(&export, &baseline_export);
}

/// A resume is refused in plain words when the repo moved on.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_resume_is_refused_when_the_repo_is_not_as_it_was() {
    if !typst_available("a_resume_is_refused") {
        return;
    }
    let _env = env();
    let repo = tempfile::tempdir().unwrap();
    stage_fixture(repo.path());
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("x.zip");
    let flaky = Scripted::new(Outage {
        fix_stage: true,
        ..Outage::default()
    });
    let err = run(&flaky, &[repo.path()], &["acme/one"], &export)
        .await
        .expect_err("refused");
    let run_id = the_run_id(&err);
    flaky.set(Outage::default());
    std::fs::write(repo.path().join("src/new.rs"), "pub fn x() {}\n").unwrap();
    let refused = resume(&flaky, &run_id, Some(export.clone()))
        .await
        .expect_err("a changed tree is refused");
    let text = refused.to_string();
    assert!(text.contains("changes that were not there"), "{text}");
    assert_eq!(flaky.calls(), flaky.sent.lock().unwrap().len());
    assert!(!export.exists());
}

/// One scan pass answers with something that is not JSON: a content failure, not an outage. The
/// run goes on and finishes everything else; the export is refused (exit 6); the resume asks the
/// model for that pass again and for nothing else.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_malformed_answer_is_a_failed_part_and_the_resume_asks_for_that_part_only() {
    if !typst_available("a_malformed_answer_is_a_failed_part") {
        return;
    }
    let _env = env();
    let repo = tempfile::tempdir().unwrap();
    stage_fixture(repo.path());
    let out = tempfile::tempdir().unwrap();
    let baseline_export = out.path().join("baseline.zip");
    let baseline_repo = tempfile::tempdir().unwrap();
    stage_fixture(baseline_repo.path());
    let healthy = Scripted::new(Outage::default());
    run(
        &healthy,
        &[baseline_repo.path()],
        &["acme/one"],
        &baseline_export,
    )
    .await
    .expect("the uninterrupted run completes");

    let export = out.path().join("resumed.zip");
    let flaky = Scripted::new(Outage {
        malformed_scan_of: Some("src/db.rs"),
        ..Outage::default()
    });
    let err = run(&flaky, &[repo.path()], &["acme/one"], &export)
        .await
        .expect_err("refused");
    assert_eq!(err.exit_code(), EXIT_INCOMPLETE_RUN);
    assert!(!export.exists());
    let run_id = the_run_id(&err);
    let scan_calls_before = flaky.sent_of(PartKind::Scan);
    flaky.set(Outage::default());
    let before = flaky.calls();
    let outcome = resume(&flaky, &run_id, Some(export.clone()))
        .await
        .expect("the resume completes");
    assert!(
        outcome.incomplete_reasons.is_empty(),
        "{:?}",
        outcome.incomplete_reasons
    );
    assert!(
        flaky.sent_of(PartKind::Scan) > scan_calls_before,
        "the failed scan pass was asked again"
    );
    assert!(
        flaky.calls() - before < healthy.calls(),
        "the resume sent less than a whole run"
    );
    assert_same_exports(&export, &baseline_export);
}

mod batch {
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;

    use camerata_server::ai_audit::{BatchApi, BatchSeam};
    use camerata_server::llm::{
        BatchItem, BatchRequestCounts, BatchResultRow, BatchStatus, CacheTtl,
    };

    use super::*;

    /// A Message Batches API over the scripted model: each item is answered as the model would
    /// answer the same request. While `poll_down`, the batch is accepted but its status cannot
    /// be read, like a process that died after the submit.
    pub struct FakeBatchApi {
        pub model: Scripted,
        pub submits: AtomicUsize,
        pub fetches: AtomicUsize,
        pub poll_down: AtomicBool,
        items: Mutex<HashMap<String, Vec<BatchItem>>>,
    }

    impl FakeBatchApi {
        pub fn new() -> Self {
            Self {
                model: Scripted::new(Outage::default()),
                submits: AtomicUsize::new(0),
                fetches: AtomicUsize::new(0),
                poll_down: AtomicBool::new(false),
                items: Mutex::new(HashMap::new()),
            }
        }
    }

    pub struct Shared(pub std::sync::Arc<FakeBatchApi>);

    fn request_of(item: &BatchItem) -> LlmRequest {
        let text = match item.params.messages[0].content.as_str() {
            Some(t) => t.to_string(),
            None => item.params.messages[0]
                .content
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|b| b["text"].as_str())
                .collect::<String>(),
        };
        let mut req = LlmRequest::new(text);
        req.system.clone_from(&item.params.system);
        req
    }

    #[async_trait]
    impl BatchApi for Shared {
        fn has_api_key(&self) -> bool {
            true
        }
        fn realtime(&self) -> &dyn LlmPort {
            &self.0.model
        }
        async fn count_input_tokens(&self, _req: &LlmRequest, _model: &str) -> Option<u64> {
            Some(1_000)
        }
        async fn submit(&self, items: Vec<BatchItem>) -> anyhow::Result<String> {
            let n = self.0.submits.fetch_add(1, Ordering::SeqCst);
            let id = format!("msgbatch_{n}");
            self.0.items.lock().unwrap().insert(id.clone(), items);
            Ok(id)
        }
        async fn poll(&self, _id: &str) -> anyhow::Result<BatchStatus> {
            if self.0.poll_down.load(Ordering::SeqCst) {
                anyhow::bail!("Message Batches API HTTP 503: try later");
            }
            Ok(BatchStatus {
                processing_status: "ended".to_string(),
                request_counts: BatchRequestCounts::default(),
            })
        }
        async fn fetch(&self, id: &str, _ttl: CacheTtl) -> anyhow::Result<Vec<BatchResultRow>> {
            self.0.fetches.fetch_add(1, Ordering::SeqCst);
            let items = self
                .0
                .items
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .unwrap_or_default();
            items
                .iter()
                .map(|item| {
                    Ok(BatchResultRow {
                        custom_id: item.custom_id.clone(),
                        response: Some(self.0.model.answer(&request_of(item))?),
                        error: None,
                        billed: None,
                    })
                })
                .collect()
        }
    }

    fn batch_args(repo: &Path, export: &Path) -> InspectArgs {
        let mut a = args(&[repo], &["acme/one"], export);
        a.realtime = false;
        a.batch = true;
        a
    }

    async fn run_batch(
        api: &std::sync::Arc<FakeBatchApi>,
        repo: &Path,
        export: &Path,
    ) -> Result<InspectOutcome, InspectError> {
        let seam = BatchSeam::new(std::sync::Arc::new(Shared(api.clone())));
        let sink = Mutex::new(Vec::new());
        run_inspect_pipeline_with(
            batch_args(repo, export),
            true,
            &store(),
            Seams {
                resume: None,
                llm: Some(&seam),
                preflight_sink: Box::leak(Box::new(|text: &str| {
                    sink.lock().unwrap().push(text.to_string());
                })),
            },
        )
        .await
    }

    /// The process dies after a batch is submitted (modelled by a status that cannot be read,
    /// with the process ending its attempt): the resume polls and fetches the same batch and
    /// submits nothing new, and the export is the uninterrupted run's.
    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn a_batch_submitted_before_the_interruption_is_fetched_not_submitted_again() {
        if !typst_available("a_batch_submitted_before_the_interruption") {
            return;
        }
        let _env = env();
        let _poll = camerata_test_env::EnvGuard::set("CAMERATA_BATCH_POLL_SECS", "1");
        let out = tempfile::tempdir().unwrap();

        let baseline_repo = tempfile::tempdir().unwrap();
        stage_fixture(baseline_repo.path());
        let baseline_export = out.path().join("baseline.zip");
        let healthy = std::sync::Arc::new(FakeBatchApi::new());
        run_batch(&healthy, baseline_repo.path(), &baseline_export)
            .await
            .expect("the uninterrupted batch run completes");

        let repo = tempfile::tempdir().unwrap();
        stage_fixture(repo.path());
        let export = out.path().join("resumed.zip");
        let api = std::sync::Arc::new(FakeBatchApi::new());
        api.poll_down.store(true, Ordering::SeqCst);
        let err = run_batch(&api, repo.path(), &export)
            .await
            .expect_err("a batch whose status cannot be read leaves the run incomplete");
        assert_ne!(err.exit_code(), 0);
        assert!(!export.exists());
        let submitted = api.submits.load(Ordering::SeqCst);
        assert!(submitted >= 1, "the batch was submitted");
        let run_id = the_run_id(&err);
        assert!(
            std::fs::read_dir(
                camerata_server::run_store::runs_dir()
                    .join(&run_id)
                    .join("batches")
            )
            .unwrap()
            .count()
                >= 1,
            "the batch id is on disk"
        );

        api.poll_down.store(false, Ordering::SeqCst);
        let seam = BatchSeam::new(std::sync::Arc::new(Shared(api.clone())));
        let sink = Mutex::new(Vec::new());
        let outcome = resume_inspect(
            &run_id,
            &[],
            Some(export.clone()),
            None,
            true,
            &store(),
            Seams {
                resume: None,
                llm: Some(&seam),
                preflight_sink: Box::leak(Box::new(|text: &str| {
                    sink.lock().unwrap().push(text.to_string());
                })),
            },
        )
        .await
        .expect("the resume completes");
        assert!(
            outcome.incomplete_reasons.is_empty(),
            "{:?}",
            outcome.incomplete_reasons
        );
        // The first batch was not submitted a second time: across both attempts the batches
        // submitted are exactly those an uninterrupted run submits (its second round follows the
        // first round's results, so the interrupted attempt never got to it).
        assert_eq!(
            api.submits.load(Ordering::SeqCst),
            healthy.submits.load(Ordering::SeqCst),
            "the batch submitted before the interruption was not submitted again"
        );
        assert!(
            api.fetches.load(Ordering::SeqCst) >= 1,
            "it fetched the batch"
        );
        assert_same_exports(&export, &baseline_export);
    }
}

/// A run another process is working on refuses a second worker.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_run_being_worked_on_refuses_a_second_worker() {
    if !typst_available("a_run_being_worked_on_refuses_a_second_worker") {
        return;
    }
    let _env = env();
    let repo = tempfile::tempdir().unwrap();
    stage_fixture(repo.path());
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("x.zip");
    let flaky = Scripted::new(Outage {
        fix_stage: true,
        ..Outage::default()
    });
    let err = run(&flaky, &[repo.path()], &["acme/one"], &export)
        .await
        .expect_err("refused");
    let run_id = the_run_id(&err);
    // Another process takes the run and is working on it.
    let busy = camerata_server::run_store::RunStore::open(&run_id).unwrap();
    flaky.set(Outage::default());
    let refused = resume(&flaky, &run_id, Some(export.clone()))
        .await
        .expect_err("a run in use is not resumed");
    assert!(
        refused.to_string().contains("being worked on right now"),
        "{refused}"
    );
    drop(busy);
}
