//! The cockpit's path to a saved run: the scan job saves the run, a job with a failed part leaves
//! the client export locked, `GET /api/runs/:id/plan` says what remains, and
//! `POST /api/runs/:id/resume` runs only what remains through the same resume the command line
//! uses, after which the lock lifts. A fake `claude` on PATH plays the model; no live calls.
//!
//! The fake answers the audit prompt with one finding per framed file and the fix prompt with
//! one fix per finding. Its calibration answer is malformed while a flag file exists, so the
//! first job ends with a failed calibration part; the flag is removed before the resume.

use super::*;
use http_body_util::BodyExt;

const FAKE_CLAUDE: &str = r#"#!/usr/bin/env python3
import json, os, re, sys
args = sys.argv[1:]
prompt = args[args.index("-p") + 1] if "-p" in args else ""
system = args[args.index("--system-prompt") + 1] if "--system-prompt" in args else ""
if os.environ.get("CAMERATA_FAKE_WARM"):
    sys.exit(0)
def note(stage):
    log = os.environ.get("FAKE_LOG")
    if log:
        with open(log, "a") as f:
            f.write(stage + "\n")
labels = re.findall(r'^<<<UNTRUSTED \S+ "([^"]*)">>>$', prompt, re.M)
if "CONFORMANCE audit" in system:
    note("audit")
    findings = [
        {"path": p, "line": 1, "severity": "medium", "rule": "unchecked-input",
         "title": "Input is not checked in " + p, "code": "// FIXTURE",
         "detail": p + " passes input on without checking it."}
        for p in labels if p != "repo map"
    ]
    out = json.dumps({"findings": findings, "proposed_rules": [], "needs_files": []})
elif "calibrating an automated audit" in system:
    note("calibration")
    flag = os.environ.get("FAKE_CALIBRATION_DOWN", "")
    if flag and os.path.exists(flag):
        out = "I cannot calibrate these findings right now."
    else:
        n = len([l for l in prompt.splitlines() if l.startswith("[") and "(severity" in l])
        out = json.dumps({"verdicts": [{"index": i, "severity": "medium", "confidence": "high",
                                        "reason": "checked", "effort": "low"} for i in range(n)]})
elif "ONE concrete, codebase-specific fix" in system:
    note("fix")
    n = len([l for l in labels if l.startswith("finding ")])
    out = json.dumps({"fixes": [{"index": i, "fix": "Check the input before it is used."}
                                for i in range(n)]})
else:
    note("other")
    out = json.dumps({"findings": [], "proposed_rules": [], "needs_files": [], "verdicts": [],
                      "fixes": [], "recommendations": []})
print(json.dumps({"type": "result", "is_error": False, "result": out,
                  "total_cost_usd": 0.01,
                  "usage": {"input_tokens": 1000, "output_tokens": 100}}))
"#;

fn stage_fixture(dest: &std::path::Path) {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/e2e_report_repo");
    fn copy(src: &std::path::Path, dest: &std::path::Path) {
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
    std::fs::write(dest.join(".gitignore"), "target/\nCargo.lock\n").unwrap();
    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dest)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "resume-test@camerata.local"]);
    g(&["config", "user.name", "Resume Test"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "fixture"]);
}

async fn body_json(resp: Response) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// Wait for the audit job `id` to leave `running`; the job's final state.
async fn finished(state: &AppState, id: &str) -> crate::jobs::JobState {
    let started = std::time::Instant::now();
    loop {
        let job = state.jobs.get(id).expect("the job exists");
        if job.status != "running" {
            return job;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(300),
            "the job did not finish in five minutes"
        );
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

fn stage_calls(log: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn audit_request() -> AuditReq {
    serde_json::from_value(serde_json::json!({
        "repos": ["acme/one"],
        "rules": [{
            "id": "unchecked-input",
            "directive": "Every input must be checked before it is used.",
        }],
        "mode": "parallel",
        "incremental": false,
        "run_ai_review": true,
        "run_deterministic": false,
    }))
    .unwrap()
}

/// A cockpit scan whose calibration part fails is saved as a run that locks the client export;
/// the plan endpoint names the part; the resume endpoint runs only that part through the shared
/// resume, and the lock lifts. The resumed scan sends no audit call again. Once complete, both
/// endpoints say so instead of starting anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(process_env)]
async fn a_cockpit_scan_with_a_failed_part_resumes_through_the_endpoint_and_the_lock_lifts() {
    if !camerata_test_env::python3_on_path_for(
        "a_cockpit_scan_with_a_failed_part_resumes_through_the_endpoint",
    ) {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    let state_dir = root.path().join("state");
    let repo = root.path().join("repo");
    for d in [&bin, &state_dir, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    {
        use std::os::unix::fs::PermissionsExt;
        let claude = bin.join("claude");
        std::fs::write(&claude, FAKE_CLAUDE).unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::process::Command::new(&claude)
            .env("CAMERATA_FAKE_WARM", "1")
            .status();
    }
    stage_fixture(&repo);
    let log = root.path().join("calls.log");
    let calibration_down = root.path().join("calibration-down");
    std::fs::write(&calibration_down, "down").unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let _path = camerata_test_env::EnvGuard::set("PATH", &path);
    let _state_dir = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", &state_dir);
    let _dep = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
    let _semgrep =
        camerata_test_env::EnvGuard::set(crate::scan_tools::DISABLE_SEMGREP_ENV_VAR, "1");
    let _log = camerata_test_env::EnvGuard::set("FAKE_LOG", &log);
    let _down = camerata_test_env::EnvGuard::set("FAKE_CALIBRATION_DOWN", &calibration_down);
    // As the kill tests run the command line: no warm-up calls and no detail regeneration step,
    // so the fake answers the audit, calibration and fix prompts only.
    let _efficiency = camerata_test_env::EnvGuard::set(
        crate::efficiency::EFFICIENCY_ENV,
        "terse_output=off,warm_schedule=off",
    );

    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let project = state
        .projects
        .create("Resume", vec!["acme/one".to_string()])
        .unwrap();
    state
        .projects
        .set_backend(&project.id, crate::project::ProjectBackend::Cli)
        .unwrap();
    state
        .projects
        .set_client_engagement(&project.id, false, false)
        .unwrap();
    state
        .settings
        .set_repo_path("acme/one", Some(repo.to_string_lossy().into_owned()));

    // ── The scan: the calibration answer is malformed, so that part fails ──────────────────
    let job_id = start_audit_job(&state, audit_request(), AuditJobOptions::default()).await;
    let job = finished(&state, &job_id).await;
    assert_eq!(job.status, "done", "{:?}", job.message);
    let first = state
        .get_last_scan(&project.id)
        .expect("the scan is the project's last scan");
    let run_id = first.run_id.clone().expect("the scan was saved as a run");
    assert!(
        !first.findings.is_empty(),
        "the fake audit answer gives one finding per file"
    );
    let failed: Vec<&crate::ai_audit::FailedPass> = first
        .failed_passes
        .iter()
        .filter(|f| !f.scope_note)
        .collect();
    assert!(
        failed.iter().any(|f| {
            (f.pass.contains("calibration") || f.pass.contains("estimation"))
                && f.class == crate::ai_audit::FailureClass::Content
        }),
        "the calibration part failed: {failed:?}"
    );
    let client = crate::report_export::ReportOptions {
        internal: false,
        ..Default::default()
    };
    assert!(
        crate::report_export::require_complete_run(&first, &client).is_err(),
        "a client export of the incomplete scan is refused"
    );
    let record = crate::run_store::read_record(&run_id).unwrap();
    assert_eq!(record.status, crate::run_store::RunStatus::Incomplete);
    let audit_calls_before = stage_calls(&log).iter().filter(|s| *s == "audit").count();
    assert!(audit_calls_before > 0);

    // ── GET /api/runs/:id/plan: what remains, and no refusal ───────────────────────────────
    let plan = run_resume_plan(Path(run_id.clone())).await.into_response();
    assert_eq!(plan.status(), StatusCode::OK);
    let plan = body_json(plan).await;
    assert!(plan["refusal"].is_null(), "{plan}");
    assert_eq!(plan["state"], "incomplete", "{plan}");
    let remaining: Vec<String> = plan["estimate"]["remaining"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        remaining.iter().any(|r| r.contains("calibration")),
        "the calibration part (recorded under its pass name) is remaining: {remaining:?}"
    );
    assert!(
        !remaining.iter().any(|r| r.contains("AI scan")),
        "the audit part is done and is not remaining: {remaining:?}"
    );

    // ── POST /api/runs/:id/resume: the provider is back; only the failed part runs ────────
    std::fs::remove_file(&calibration_down).unwrap();
    let resume = run_resume_start(State(state.clone()), Path(run_id.clone()))
        .await
        .into_response();
    assert_eq!(resume.status(), StatusCode::OK);
    let resume_job = body_json(resume).await["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let job = finished(&state, &resume_job).await;
    assert_eq!(job.status, "done", "{:?}", job.message);
    let second = state.get_last_scan(&project.id).unwrap();
    assert_eq!(second.run_id.as_deref(), Some(run_id.as_str()));
    assert!(
        second.incomplete_reasons().is_empty(),
        "{:?}",
        second.incomplete_reasons()
    );
    assert!(
        crate::report_export::require_complete_run(&second, &client).is_ok(),
        "the client export lock lifts"
    );
    assert_eq!(
        second.findings.len(),
        first.findings.len(),
        "the resumed scan has the findings of the first (the audit replies were replayed)"
    );
    let audit_calls_after = stage_calls(&log).iter().filter(|s| *s == "audit").count();
    assert_eq!(
        audit_calls_after, audit_calls_before,
        "the resume sent no audit call again"
    );
    let record = crate::run_store::read_record(&run_id).unwrap();
    assert_eq!(record.status, crate::run_store::RunStatus::Complete);

    // ── A complete run: the plan says so and the resume endpoint starts nothing ───────────
    let plan = body_json(run_resume_plan(Path(run_id.clone())).await.into_response()).await;
    assert_eq!(plan["state"], "complete", "{plan}");
    assert!(
        plan["refusal"]
            .as_str()
            .is_some_and(|r| r.contains("already complete")),
        "{plan}"
    );
    let again = run_resume_start(State(state.clone()), Path(run_id.clone()))
        .await
        .into_response();
    let again_job = body_json(again).await["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let job = finished(&state, &again_job).await;
    assert_eq!(job.status, "failed", "{:?}", job.message);
    assert!(
        job.message
            .as_deref()
            .unwrap_or_default()
            .contains("already complete"),
        "{:?}",
        job.message
    );
}

/// The two endpoints on a run that does not exist: a plain 404 with the reason, no job.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_run_endpoints_answer_404_for_an_unknown_run() {
    let state_dir = tempfile::tempdir().unwrap();
    let _state_dir = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state_dir.path());
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let plan = run_resume_plan(Path("no-such-run".to_string()))
        .await
        .into_response();
    assert_eq!(plan.status(), StatusCode::NOT_FOUND);
    assert!(body_json(plan).await["error"]
        .as_str()
        .unwrap()
        .contains("no saved run"));
    let resume = run_resume_start(State(state.clone()), Path("no-such-run".to_string()))
        .await
        .into_response();
    assert_eq!(resume.status(), StatusCode::NOT_FOUND);
}
