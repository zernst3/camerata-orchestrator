//! The cockpit's path to a saved run: the scan job saves the run, a job with a failed part leaves
//! the client export locked, `GET /api/inspections/:id/plan` says what remains, and
//! `POST /api/inspections/:id/resume` runs only what remains through the same resume the command line
//! uses, after which the lock lifts. A fake `claude` on PATH plays the model; no live calls.
//!
//! The fake answers the audit prompt with one finding per framed file and the fix prompt with
//! one fix per finding. Its calibration answer is malformed while a flag file exists, so the
//! first job ends with a failed calibration part; the flag is removed before the resume. While a
//! second flag file exists its calibration call hangs, so a test can end a job in the middle of
//! the run.

use super::*;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;

const FAKE_CLAUDE: &str = r#"#!/usr/bin/env python3
import json, os, re, sys, time
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
    hang = os.environ.get("FAKE_CALIBRATION_HANG", "")
    if hang and os.path.exists(hang):
        # The call never answers: the test ends the job while this is in flight.
        with open(hang + ".pid", "w") as f:
            f.write(str(os.getpid()))
        time.sleep(600)
    flag = os.environ.get("FAKE_CALIBRATION_DOWN", "")
    if flag and os.path.exists(flag):
        out = "I cannot calibrate these findings right now."
    else:
        n = len([l for l in prompt.splitlines() if l.startswith("[") and "(severity" in l])
        out = json.dumps({"verdicts": [{"index": i, "severity": "medium", "confidence": "high",
                                        "reason": "checked", "effort": "low"} for i in range(n)]})
elif "ONE concrete, codebase-specific fix" in system:
    note("fix")
    out = json.dumps({"fixes": [{"id": l[len("finding "):], "fix": "Check the input before it is used."}
                                for l in labels if l.startswith("finding ")]})
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
    assert_eq!(
        record.project_id.as_deref(),
        Some(project.id.as_str()),
        "a scan started from the cockpit records the project it was started in"
    );
    let audit_calls_before = stage_calls(&log).iter().filter(|s| *s == "audit").count();
    assert!(audit_calls_before > 0);

    // ── GET /api/inspections/:id/plan: what remains, and no refusal ───────────────────────────────
    let plan = run_resume_plan(State(state.clone()), Path(run_id.clone()))
        .await
        .into_response();
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

    // ── POST /api/inspections/:id/resume: the provider is back; only the failed part runs ────────
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
    let plan = body_json(
        run_resume_plan(State(state.clone()), Path(run_id.clone()))
            .await
            .into_response(),
    )
    .await;
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
    let plan = run_resume_plan(State(state.clone()), Path("no-such-run".to_string()))
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

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    use tower::ServiceExt;
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

/// A cockpit run of `dir` named `name`, as the scan job would save it, not finished.
fn saved_cockpit_run(
    name: &str,
    dir: &std::path::Path,
    args: serde_json::Value,
) -> Arc<crate::run_store::RunStore> {
    use crate::run_store as rs;
    rs::RunStore::create(rs::NewRun {
        project_key: "listing-test".to_string(),
        repos: vec![(name.to_string(), dir.to_path_buf())],
        models: rs::ModelsRecord {
            audit: "m".to_string(),
            calibration: "m".to_string(),
            fix: String::new(),
            backend: "cli".to_string(),
            mode: "parallel".to_string(),
            resolved: true,
        },
        ai_scan_key: crate::scan_cache::AiScanKey::new("m", Some("m"), &[]),
        args,
        estimate_usd: None,
        include_alternatives: false,
        origin: rs::RunOrigin::Cockpit,
        project_id: None,
    })
    .unwrap()
}

/// A cockpit run of the repos `repos` (name and folder), started in `project_id` (`None` = saved
/// before runs recorded their project), not finished.
fn saved_cockpit_run_in(
    project_id: Option<&str>,
    repos: &[(&str, &std::path::Path)],
) -> Arc<crate::run_store::RunStore> {
    use crate::run_store as rs;
    rs::RunStore::create(rs::NewRun {
        project_key: "listing-test".to_string(),
        repos: repos
            .iter()
            .map(|(name, dir)| (name.to_string(), dir.to_path_buf()))
            .collect(),
        models: rs::ModelsRecord {
            audit: "m".to_string(),
            calibration: "m".to_string(),
            fix: String::new(),
            backend: "cli".to_string(),
            mode: "parallel".to_string(),
            resolved: true,
        },
        ai_scan_key: crate::scan_cache::AiScanKey::new("m", Some("m"), &[]),
        args: serde_json::to_value(audit_request()).unwrap(),
        estimate_usd: None,
        include_alternatives: false,
        origin: rs::RunOrigin::Cockpit,
        project_id: project_id.map(str::to_string),
    })
    .unwrap()
}

/// The ids `GET /api/inspections?project_id=` lists for `project_id`.
async fn listed_ids(app: &axum::Router, project_id: &str) -> Vec<String> {
    let (status, body) = get_json(
        app.clone(),
        &format!("/api/inspections?project_id={project_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body["inspections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect()
}

/// A saved cockpit inspection belongs to the project it was started in. Project X holds
/// [acme/one] and project Y holds [acme/one, acme/two]: Y's scan of acme/one alone is listed in Y
/// and not in X (the repos alone would offer it to both), and resuming it from X is refused in
/// words naming both projects, with no job started. A cockpit run saved before runs recorded a
/// project is placed by its repos only when exactly one project holds them all, and otherwise
/// listed nowhere (and refused), with the reason in the run listing the command line shows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(process_env)]
async fn a_saved_cockpit_inspection_is_listed_and_resumed_only_in_the_project_that_started_it() {
    let root = tempfile::tempdir().unwrap();
    let state_dir = root.path().join("state");
    let one = root.path().join("one");
    let two = root.path().join("two");
    for d in [&state_dir, &one, &two] {
        std::fs::create_dir_all(d).unwrap();
    }
    stage_fixture(&one);
    stage_fixture(&two);
    let _state_dir = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", &state_dir);

    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let x = state
        .projects
        .create("Client X", vec!["acme/one".to_string()])
        .unwrap();
    let y = state
        .projects
        .create(
            "Client Y",
            vec!["acme/one".to_string(), "acme/two".to_string()],
        )
        .unwrap();
    state
        .settings
        .set_repo_path("acme/one", Some(one.to_string_lossy().into_owned()));
    state
        .settings
        .set_repo_path("acme/two", Some(two.to_string_lossy().into_owned()));
    let app = router(state.clone());

    // Y scanned acme/one alone; X never did.
    let y_run = saved_cockpit_run_in(Some(&y.id), &[("acme/one", one.as_path())]).run_id();
    assert_eq!(listed_ids(&app, &y.id).await, vec![y_run.clone()]);
    assert!(
        listed_ids(&app, &x.id).await.is_empty(),
        "Y's inspection is not offered in X, though acme/one is one of X's repos"
    );

    // Resuming it from X is refused and starts nothing; the saved run is untouched.
    assert!(state.projects.set_active(&x.id));
    let refused = run_resume_start(State(state.clone()), Path(y_run.clone()))
        .await
        .into_response();
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    let body = body_json(refused).await;
    assert!(body["job_id"].is_null(), "no job was started: {body}");
    let why = body["error"].as_str().unwrap().to_string();
    assert!(
        why.contains("Client Y") && why.contains("Client X"),
        "the refusal names both projects: {why}"
    );
    assert_eq!(
        crate::run_store::read_record(&y_run).unwrap().attempt,
        1,
        "the refused resume did not touch the run"
    );
    // The plan ("what would Run what remains do") is refused from X the same way: a run is
    // previewed only in the project that started it. From Y it answers.
    let plan = run_resume_plan(State(state.clone()), Path(y_run.clone()))
        .await
        .into_response();
    assert_eq!(plan.status(), StatusCode::CONFLICT);
    let why = body_json(plan).await["error"].as_str().unwrap().to_string();
    assert!(
        why.contains("Client Y") && why.contains("Client X"),
        "the plan's refusal names both projects: {why}"
    );
    assert!(state.projects.set_active(&y.id));
    let plan = run_resume_plan(State(state.clone()), Path(y_run.clone()))
        .await
        .into_response();
    assert_eq!(plan.status(), StatusCode::OK);
    assert!(state.projects.set_active(&x.id));

    // An old cockpit run with no project id: acme/two is held by Y alone, so it is placed in Y.
    let old_two = saved_cockpit_run_in(None, &[("acme/two", two.as_path())]).run_id();
    assert!(listed_ids(&app, &y.id).await.contains(&old_two));
    assert!(!listed_ids(&app, &x.id).await.contains(&old_two));
    // acme/one is held by both projects: the old run is listed nowhere and says why in the run
    // listing the command line shows.
    let old_one = saved_cockpit_run_in(None, &[("acme/one", one.as_path())]).run_id();
    assert!(!listed_ids(&app, &x.id).await.contains(&old_one));
    assert!(!listed_ids(&app, &y.id).await.contains(&old_one));
    let runs = crate::run_store::list_runs();
    let summary = runs.iter().find(|r| r.run_id == old_one).unwrap();
    assert!(
        summary
            .note
            .as_deref()
            .is_some_and(|n| n.contains("saved before runs recorded their project")),
        "{:?}",
        summary.note
    );
    // Neither project can resume it.
    for open in [&x.id, &y.id] {
        assert!(state.projects.set_active(open));
        let refused = run_resume_start(State(state.clone()), Path(old_one.clone()))
            .await
            .into_response();
        assert_eq!(refused.status(), StatusCode::CONFLICT);
    }
    // A run that names its project has no such note.
    let named = runs.iter().find(|r| r.run_id == y_run).unwrap();
    assert_eq!(named.project_id.as_deref(), Some(y.id.as_str()));
    assert!(named.note.is_none());
}

/// A cockpit run's project id is read back from the record, and a record written before the field
/// existed reads as no project.
#[test]
#[serial_test::serial(process_env)]
fn a_run_record_without_a_project_id_reads_as_no_project() {
    let state_dir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    stage_fixture(repo.path());
    let _state_dir = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state_dir.path());
    let named = saved_cockpit_run_in(Some("proj-1"), &[("acme/one", repo.path())]);
    let id = named.run_id();
    assert_eq!(
        crate::run_store::read_record(&id)
            .unwrap()
            .project_id
            .as_deref(),
        Some("proj-1")
    );
    let path = crate::run_store::runs_dir().join(&id).join("run.json");
    let mut json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    json.as_object_mut().unwrap().remove("project_id");
    std::fs::write(&path, json.to_string()).unwrap();
    assert_eq!(crate::run_store::read_record(&id).unwrap().project_id, None);
}

/// `GET /api/inspections` lists the open project's saved inspections with where each stands, a
/// live one is never offered for resuming, another project's runs are not listed, the plan and
/// resume live under the same namespace, and the two old `/api/runs/:id/...` paths still answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(process_env)]
async fn the_inspections_endpoint_lists_the_open_projects_saved_runs() {
    let root = tempfile::tempdir().unwrap();
    let state_dir = root.path().join("state");
    let repo = root.path().join("repo");
    let other_repo = root.path().join("other");
    for d in [&state_dir, &repo, &other_repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    stage_fixture(&repo);
    stage_fixture(&other_repo);
    let _state_dir = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", &state_dir);

    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let project = state
        .projects
        .create("Listing", vec!["acme/one".to_string()])
        .unwrap();
    state
        .settings
        .set_repo_path("acme/one", Some(repo.to_string_lossy().into_owned()));
    let app = router(state.clone());

    // Nothing saved yet.
    let (status, body) = get_json(app.clone(), "/api/inspections").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["project_id"], project.id.as_str());
    assert_eq!(body["inspections"].as_array().unwrap().len(), 0, "{body}");

    // One interrupted (its holder is gone), one live (still held), one of another project.
    let args = serde_json::to_value(audit_request()).unwrap();
    let interrupted_id = {
        let store = saved_cockpit_run("acme/one", &repo, args.clone());
        store.run_id()
    };
    let live = saved_cockpit_run("acme/one", &repo, args.clone());
    let live_id = live.run_id();
    let stranger = saved_cockpit_run("acme/elsewhere", &other_repo, args.clone());
    let stranger_id = stranger.run_id();

    let (status, body) = get_json(app.clone(), "/api/inspections").await;
    assert_eq!(status, StatusCode::OK);
    let listed = body["inspections"].as_array().unwrap();
    assert_eq!(listed.len(), 2, "only this project's runs: {body}");
    assert!(
        !listed.iter().any(|r| r["id"] == stranger_id.as_str()),
        "{body}"
    );
    let row = |id: &str| {
        listed
            .iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("{id} is listed: {body}"))
    };
    let interrupted = row(&interrupted_id);
    assert_eq!(interrupted["status"], "interrupted");
    assert_eq!(interrupted["resumable_here"], true);
    assert_eq!(interrupted["repos"][0], "acme/one");
    assert!(interrupted["remaining_parts"].as_u64().unwrap() > 0);
    assert!(interrupted["started"].is_string());
    assert!(interrupted["estimate_usd"].is_number());
    let live_row = row(&live_id);
    assert_eq!(live_row["status"], "live");
    assert_eq!(
        live_row["resumable_here"], false,
        "a running inspection is never offered"
    );

    // The plan and the resume answer under the namespace, and under the old paths.
    for prefix in ["/api/inspections", "/api/runs"] {
        let (status, plan) =
            get_json(app.clone(), &format!("{prefix}/{interrupted_id}/plan")).await;
        assert_eq!(status, StatusCode::OK, "{prefix}: {plan}");
        assert_eq!(plan["state"], "interrupted", "{prefix}: {plan}");
        let (status, _) = get_json(app.clone(), &format!("{prefix}/no-such-run/plan")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{prefix}");
    }
    // A live inspection's plan refuses in plain words.
    let (_, plan) = get_json(app.clone(), &format!("/api/inspections/{live_id}/plan")).await;
    assert!(
        plan["refusal"]
            .as_str()
            .is_some_and(|r| r.contains("being worked on right now")),
        "{plan}"
    );
    drop((live, stranger));
}

/// What the findings of a scan are, without the fields that name the run (ids and times): the
/// content a reader would compare between two runs of the same code.
fn findings_content(report: &crate::onboard::ScanReport) -> Vec<serde_json::Value> {
    let mut rows: Vec<serde_json::Value> = report
        .findings
        .iter()
        .map(|f| serde_json::to_value(f).unwrap())
        .collect();
    rows.sort_by_key(|v| v.to_string());
    rows
}

/// Stop every fake `claude` still hanging in a calibration call: the pid it wrote is the one to
/// stop, so no test process outlives the test.
fn stop_hanging_claude(hang_flag: &std::path::Path) {
    let pid_file = std::path::PathBuf::from(format!("{}.pid", hang_flag.display()));
    if let Ok(pid) = std::fs::read_to_string(pid_file) {
        let _ = std::process::Command::new("kill").arg(pid.trim()).status();
    }
}

/// Zach's rule: "any interruption should cost minutes, never the run". A cockpit scan job whose
/// task dies mid-run (here: the runtime it ran on is shut down while a calibration call is in
/// flight, the way an app crash or a laptop dying ends it) is listed as Interrupted, the plan
/// offers the parts that remain, and "Run what remains" completes it with the findings an
/// uninterrupted job gives and no call repeated for a stored reply. While the job runs, the
/// listing says Live and never offers it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(process_env)]
async fn a_cockpit_scan_killed_mid_run_is_interrupted_and_run_what_remains_completes_it() {
    if !camerata_test_env::python3_on_path_for(
        "a_cockpit_scan_killed_mid_run_is_interrupted_and_run_what_remains_completes_it",
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
    let baseline_log = root.path().join("baseline-calls.log");
    let log = root.path().join("calls.log");
    let hang_flag = root.path().join("calibration-hangs");
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
    let _hang = camerata_test_env::EnvGuard::set("FAKE_CALIBRATION_HANG", &hang_flag);
    let _efficiency = camerata_test_env::EnvGuard::set(
        crate::efficiency::EFFICIENCY_ENV,
        "terse_output=off,warm_schedule=off",
    );

    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let project = state
        .projects
        .create("Killed", vec!["acme/one".to_string()])
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
    let app = router(state.clone());

    // ── The uninterrupted job, for what a whole scan gives ────────────────────────────────
    let baseline = {
        let _log = camerata_test_env::EnvGuard::set("FAKE_LOG", &baseline_log);
        let job_id = start_audit_job(&state, audit_request(), AuditJobOptions::default()).await;
        let job = finished(&state, &job_id).await;
        assert_eq!(job.status, "done", "{:?}", job.message);
        state.get_last_scan(&project.id).expect("the baseline scan")
    };
    assert!(baseline.incomplete_reasons().is_empty());
    assert!(!baseline.findings.is_empty());
    let baseline_audit_calls = stage_calls(&baseline_log)
        .iter()
        .filter(|s| *s == "audit")
        .count();
    assert!(baseline_audit_calls > 0);

    // ── The job that is killed: its calibration call hangs, then its runtime goes away ────
    std::fs::write(&hang_flag, "hang").unwrap();
    let _log = camerata_test_env::EnvGuard::set("FAKE_LOG", &log);
    let job_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let job_id = {
        let state = state.clone();
        job_runtime
            .spawn(async move {
                start_audit_job(&state, audit_request(), AuditJobOptions::default()).await
            })
            .await
            .unwrap()
    };
    let started = std::time::Instant::now();
    while !stage_calls(&log).iter().any(|s| s == "calibration") {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(300),
            "the job never reached calibration: {:?}",
            state.jobs.get(&job_id).map(|j| j.message)
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let audit_calls_before_kill = stage_calls(&log).iter().filter(|s| *s == "audit").count();
    assert_eq!(audit_calls_before_kill, baseline_audit_calls);

    // While the job runs the listing says live and never offers it.
    let (_, listed) = get_json(app.clone(), "/api/inspections").await;
    let running: Vec<&serde_json::Value> = listed["inspections"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["status"] == "live")
        .collect();
    assert_eq!(running.len(), 1, "{listed}");
    assert_eq!(running[0]["resumable_here"], false, "{listed}");
    let run_id = running[0]["id"].as_str().unwrap().to_string();
    let (_, plan) = get_json(app.clone(), &format!("/api/inspections/{run_id}/plan")).await;
    assert!(
        plan["refusal"]
            .as_str()
            .is_some_and(|r| r.contains("being worked on right now")),
        "a live run's resume is refused: {plan}"
    );

    // The kill: every task of the job goes, and with them the run's lock.
    job_runtime.shutdown_background();
    let started = std::time::Instant::now();
    loop {
        let liveness = crate::run_store::liveness_of(&crate::run_store::runs_dir().join(&run_id));
        if liveness != crate::run_store::Liveness::Live {
            break;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "the killed job still holds its run's lock"
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    stop_hanging_claude(&hang_flag);

    // ── Interrupted in the listing, with what remains and no refusal ──────────────────────
    let (_, listed) = get_json(app.clone(), "/api/inspections").await;
    let row = listed["inspections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == run_id.as_str())
        .unwrap_or_else(|| panic!("the killed run is listed: {listed}"));
    assert_eq!(row["status"], "interrupted", "{row}");
    assert_eq!(row["resumable_here"], true, "{row}");
    assert!(row["remaining_parts"].as_u64().unwrap() > 0, "{row}");
    let (_, plan) = get_json(app.clone(), &format!("/api/inspections/{run_id}/plan")).await;
    assert_eq!(plan["state"], "interrupted", "{plan}");
    assert!(plan["refusal"].is_null(), "{plan}");
    let remaining = plan["text"].as_str().unwrap();
    assert!(remaining.contains("calibration"), "{remaining}");
    assert_eq!(
        plan["estimate"]["calls_stored"].as_u64().unwrap() as usize,
        audit_calls_before_kill,
        "the audit replies are stored and will not be sent again: {remaining}"
    );
    assert_eq!(
        crate::run_store::read_record(&run_id).unwrap().status,
        crate::run_store::RunStatus::Running,
        "the record still says running: nothing finished it"
    );

    // ── Run what remains ──────────────────────────────────────────────────────────────────
    std::fs::remove_file(&hang_flag).unwrap();
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
    let resumed = state.get_last_scan(&project.id).unwrap();
    assert_eq!(resumed.run_id.as_deref(), Some(run_id.as_str()));
    assert!(
        resumed.incomplete_reasons().is_empty(),
        "{:?}",
        resumed.incomplete_reasons()
    );
    assert_eq!(
        findings_content(&resumed),
        findings_content(&baseline),
        "the resumed job has the findings of the uninterrupted one"
    );
    let calls = stage_calls(&log);
    assert_eq!(
        calls.iter().filter(|s| *s == "audit").count(),
        audit_calls_before_kill,
        "no audit call was repeated for a stored reply: {calls:?}"
    );
    assert_eq!(
        calls.iter().filter(|s| *s == "fix").count(),
        stage_calls(&baseline_log)
            .iter()
            .filter(|s| *s == "fix")
            .count(),
        "the fix stage ran once, after the resume: {calls:?}"
    );
    let record = crate::run_store::read_record(&run_id).unwrap();
    assert_eq!(record.status, crate::run_store::RunStatus::Complete);
    let (_, listed) = get_json(app, "/api/inspections").await;
    let row = listed["inspections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == run_id.as_str())
        .unwrap();
    assert_eq!(row["status"], "complete", "{row}");
    assert_eq!(row["resumable_here"], false, "{row}");
}
