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
use axum::extract::Query;
use axum::http::Request;
use http_body_util::BodyExt;

const FAKE_CLAUDE: &str = r#"#!/usr/bin/env python3
import json, os, re, sys, time
args = sys.argv[1:]
# The run's pre-flight reads the Claude Code version before any model call; it is not a call.
if args == ["--version"]:
    print(os.environ.get("FAKE_VERSION", "2.1.321 (Claude Code)"))
    sys.exit(0)
prompt = args[args.index("-p") + 1] if "-p" in args else ""
system = args[args.index("--system-prompt") + 1] if "--system-prompt" in args else ""
if os.environ.get("CAMERATA_FAKE_WARM"):
    sys.exit(0)
def note(stage):
    down = os.environ.get("FAKE_503_STAGE_FILE", "")
    if down and os.path.exists(down) and open(down).read().strip() == stage:
        sys.stderr.write("API Error: 503 Service Unavailable\\n")
        sys.exit(1)
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
elif "first reviewer of an automated audit" in system:
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
        out = json.dumps({"severity": "medium", "real": "yes", "reason": "checked",
                          "effort": "low"})
elif "architect's first reader of this rule group" in system:
    note("group_review")
    model_log = os.environ.get("FAKE_MODEL_LOG")
    if model_log:
        with open(model_log, "a") as f:
            f.write("group_review " + args[args.index("--model") + 1] + "\n")
    gr_flag = os.environ.get("FAKE_GROUP_REVIEW_DOWN", "")
    idx = [int(m) for m in re.findall(r'^(\d+): ', prompt, re.M)]
    if gr_flag and os.path.exists(gr_flag) and open(gr_flag).read().strip() == "unavailable":
        sys.stderr.write("API Error: 503 Service Unavailable\\n")
        sys.exit(1)
    elif gr_flag and os.path.exists(gr_flag) and open(gr_flag).read().strip() == "transport":
        sys.stderr.write("API Error: Unable to connect to API (ECONNREFUSED): connection refused\\n")
        sys.exit(1)
    elif gr_flag and os.path.exists(gr_flag):
        out = "I cannot review this group right now."
    else:
        out = json.dumps({"summary": "The fixture's sites share one cause.",
                          "subgroups": [{"label": "All", "cause": "one shared cause", "summary": "One cause.", "members": idx}],
                          "outliers": [], "escalations": [], "disagreements": []})
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
    // The plan carries the limit the run was saved with (null when it had none), which the
    // confirmation prefills.
    assert!(plan.get("saved_limit_usd").is_some(), "{plan}");
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

    // A raised limit is honoured only for a run the spend limit stopped: this one failed a part.
    let refused = run_resume_start(
        State(state.clone()),
        Path(run_id.clone()),
        Query(ResumeQuery {
            max_cost_usd: Some(500.0),
        }),
    )
    .await
    .into_response();
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    let why = body_json(refused).await["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(why.contains("spend limit"), "{why}");

    // ── POST /api/inspections/:id/resume: the provider is back; only the failed part runs ────────
    std::fs::remove_file(&calibration_down).unwrap();
    let resume = run_resume_start(
        State(state.clone()),
        Path(run_id.clone()),
        Query(ResumeQuery::default()),
    )
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
    let again = run_resume_start(
        State(state.clone()),
        Path(run_id.clone()),
        Query(ResumeQuery::default()),
    )
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
    let resume = run_resume_start(
        State(state.clone()),
        Path("no-such-run".to_string()),
        Query(ResumeQuery::default()),
    )
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
            group_review: String::new(),
            finder: String::new(),
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
            group_review: String::new(),
            finder: String::new(),
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
    let refused = run_resume_start(
        State(state.clone()),
        Path(y_run.clone()),
        Query(ResumeQuery::default()),
    )
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
        let refused = run_resume_start(
            State(state.clone()),
            Path(old_one.clone()),
            Query(ResumeQuery::default()),
        )
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
    assert!(
        baseline.incomplete_reasons().is_empty(),
        "{:?}",
        baseline.failed_passes
    );
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
    let resume = run_resume_start(
        State(state.clone()),
        Path(run_id.clone()),
        Query(ResumeQuery::default()),
    )
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

// ---- the kept group review model on a cockpit resume ----

/// A cockpit scan of the fixture whose group review part failed (the provider was down for it), and
/// the paths a test needs to resume it. The fake `claude` plays the model, so a test sees every
/// call in `log` and the model each group review call asked for in `model_log`.
struct InterruptedScan {
    _root: tempfile::TempDir,
    _guards: Vec<camerata_test_env::EnvGuard>,
    state: AppState,
    project_id: String,
    run_id: String,
    log: std::path::PathBuf,
    model_log: std::path::PathBuf,
    /// While this file exists the fake group review call fails.
    group_review_down: std::path::PathBuf,
    /// The file whose presence is the outage this scan ran under (the group review's, or the
    /// stage 503 file); removing it ends the outage.
    outage_flag: std::path::PathBuf,
    /// The first scan's final job state.
    job: crate::jobs::JobState,
}

async fn interrupted_scan() -> InterruptedScan {
    interrupted_scan_with("down").await
}

/// As [`interrupted_scan`], with the group review call failing the way `mode` says: `down` is a
/// reply that is not JSON, `transport` is a connection failure with no reply at all.
async fn interrupted_scan_with(mode: &str) -> InterruptedScan {
    scan_rig(mode, false).await
}

/// The same rig, with the project's finder set to the agent and no assumption about how the scan
/// ends (the fake `claude` plays the model for the engine's passes, not for the agent finder).
async fn interrupted_scan_with_finder() -> InterruptedScan {
    scan_rig("down", true).await
}

async fn scan_rig(mode: &str, agent_finder: bool) -> InterruptedScan {
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
    let model_log = root.path().join("models.log");
    // The calibration answer is fine here; the group review call is what is down.
    let group_review_down = root.path().join("group-review-down");
    // `stage503:<stage>` takes that stage's calls down with a 503 instead of the group review's.
    let stage_down = root.path().join("stage-503");
    let outage_flag = if let Some(stage) = mode.strip_prefix("stage503:") {
        std::fs::write(&stage_down, stage).unwrap();
        stage_down.clone()
    } else {
        std::fs::write(&group_review_down, mode).unwrap();
        group_review_down.clone()
    };
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let guards = vec![
        camerata_test_env::EnvGuard::set("FAKE_GROUP_REVIEW_DOWN", &group_review_down),
        camerata_test_env::EnvGuard::set("FAKE_503_STAGE_FILE", &stage_down),
        camerata_test_env::EnvGuard::set("PATH", &path),
        camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", &state_dir),
        camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1"),
        camerata_test_env::EnvGuard::set(crate::scan_tools::DISABLE_SEMGREP_ENV_VAR, "1"),
        camerata_test_env::EnvGuard::set("FAKE_LOG", &log),
        camerata_test_env::EnvGuard::set("FAKE_MODEL_LOG", &model_log),
        camerata_test_env::EnvGuard::set(
            crate::efficiency::EFFICIENCY_ENV,
            "terse_output=off,warm_schedule=off",
        ),
        // Anything that would leave the machine goes to a closed local port instead.
        camerata_test_env::EnvGuard::set("HTTPS_PROXY", "http://127.0.0.1:9"),
        camerata_test_env::EnvGuard::set("ALL_PROXY", "http://127.0.0.1:9"),
    ];
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
    if agent_finder {
        let project = state.projects.get(&project.id).unwrap();
        state
            .projects
            .set_review_settings(
                &project.id,
                camerata_api_types::project::ReviewSettings {
                    finder: camerata_api_types::project::ProjectFinder::Agent,
                    ..project.review
                },
            )
            .unwrap()
            .unwrap();
    }
    let job_id = start_audit_job(&state, audit_request(), AuditJobOptions::default()).await;
    let job = finished(&state, &job_id).await;
    if agent_finder {
        // Whatever the job's status, the run's folder is the only one in the state folder.
        let run_id = std::fs::read_dir(crate::run_store::runs_dir())
            .expect("the scan made a run folder")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .next()
            .expect("the scan saved a run");
        return InterruptedScan {
            _root: root,
            _guards: guards,
            state,
            project_id: project.id,
            run_id,
            log,
            model_log,
            group_review_down,
            outage_flag,
            job,
        };
    }
    assert_eq!(job.status, "done", "{:?}", job.message);
    let first = state.get_last_scan(&project.id).unwrap();
    let run_id = first.run_id.clone().expect("the scan was saved as a run");
    assert_eq!(
        crate::run_store::read_record(&run_id).unwrap().status,
        crate::run_store::RunStatus::Incomplete
    );
    InterruptedScan {
        _root: root,
        _guards: guards,
        state,
        project_id: project.id,
        run_id,
        log,
        model_log,
        group_review_down,
        outage_flag,
        job,
    }
}

/// Rewrite the saved record of `run_id` the way an earlier build, or a hand edit, could have
/// left it.
fn patch_saved_run(run_id: &str, patch: impl FnOnce(&mut serde_json::Value)) {
    let path = crate::run_store::runs_dir().join(run_id).join("run.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).expect("the record is JSON");
    patch(&mut value);
    std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
}

/// A client engagement checks the group review model a cockpit resume keeps. A run recorded on
/// `vendor/x` (started as the maintainer's own development) resumed in a project that is now a
/// client engagement on the API is refused before any model call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(process_env)]
async fn a_cockpit_resume_is_refused_when_the_kept_group_review_model_is_not_allowed_for_client_work(
) {
    if !camerata_test_env::python3_on_path_for("a_cockpit_resume_is_refused_when_the_kept_group") {
        return;
    }
    let rig = interrupted_scan().await;
    patch_saved_run(&rig.run_id, |v| {
        v["models"]["group_review"] = serde_json::json!("vendor/x");
        v["models"]["backend"] = serde_json::json!("api");
    });
    // The project is a client engagement on the API now, with a key (a test value that never
    // leaves the machine).
    rig.state
        .credential_store
        .set(crate::credentials::ANTHROPIC_API_KEY, "sk-ant-test-value")
        .unwrap();
    rig.state
        .projects
        .set_backend(&rig.project_id, crate::project::ProjectBackend::Api)
        .unwrap();
    rig.state
        .projects
        .set_client_engagement(&rig.project_id, true, false)
        .unwrap();
    std::fs::remove_file(&rig.group_review_down).unwrap();
    let calls_before = stage_calls(&rig.log).len();

    let resume = run_resume_start(
        State(rig.state.clone()),
        Path(rig.run_id.clone()),
        Query(ResumeQuery::default()),
    )
    .await
    .into_response();
    assert_eq!(resume.status(), StatusCode::OK);
    let job_id = body_json(resume).await["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let job = finished(&rig.state, &job_id).await;
    assert_eq!(
        job.status, "failed",
        "a client engagement must not send client code to the kept model: {:?}",
        job.message
    );
    let why = job.message.clone().unwrap_or_default();
    assert!(why.contains("client engagement"), "{why}");
    assert!(why.contains("vendor/x"), "{why}");
    assert_eq!(
        stage_calls(&rig.log).len(),
        calls_before,
        "no model call was made"
    );
}

/// The group review calls the fake `claude` answered, as the models they asked for.
fn group_review_models(model_log: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(model_log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.strip_prefix("group_review "))
        .map(str::to_string)
        .collect()
}

/// Run what remains for `rig`'s run; the job's final state.
async fn resume_job(rig: &InterruptedScan) -> crate::jobs::JobState {
    let resume = run_resume_start(
        State(rig.state.clone()),
        Path(rig.run_id.clone()),
        Query(ResumeQuery::default()),
    )
    .await
    .into_response();
    assert_eq!(resume.status(), StatusCode::OK);
    let job_id = body_json(resume).await["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    finished(&rig.state, &job_id).await
}

/// A run recorded on Fable 5.1, resumed in a project that has no pick of its own (it holds the
/// built-in default, Fable 5.2), asks Fable 5.1 for its group review and says so on the run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(process_env)]
async fn a_cockpit_resume_with_no_group_review_pick_stays_on_the_recorded_model_and_says_so() {
    if !camerata_test_env::python3_on_path_for("a_cockpit_resume_with_no_group_review_pick") {
        return;
    }
    let rig = interrupted_scan().await;
    patch_saved_run(&rig.run_id, |v| {
        v["models"]["group_review"] = serde_json::json!("claude-fable-5-1");
    });
    std::fs::remove_file(&rig.group_review_down).unwrap();
    let asked_before = group_review_models(&rig.model_log).len();
    let job = resume_job(&rig).await;
    assert_eq!(job.status, "done", "{:?}", job.message);
    let asked = group_review_models(&rig.model_log);
    assert!(
        asked.len() > asked_before,
        "the resume asked the group reviewer"
    );
    for model in &asked[asked_before..] {
        assert_eq!(model, "claude-fable-5-1");
    }
    let notice = "Group review stays on claude-fable-5-1, the model this run started with.";
    let record = crate::run_store::read_record(&rig.run_id).unwrap();
    assert_eq!(
        record.notes.iter().filter(|n| n.as_str() == notice).count(),
        1,
        "{:?}",
        record.notes
    );
}

/// A project whose saved group review model differs from the one the run started on is an
/// explicit different pick: the resume is refused before any model call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(process_env)]
async fn a_cockpit_resume_with_a_different_group_review_pick_is_refused() {
    if !camerata_test_env::python3_on_path_for("a_cockpit_resume_with_a_different_group_review") {
        return;
    }
    let rig = interrupted_scan().await;
    patch_saved_run(&rig.run_id, |v| {
        v["models"]["group_review"] = serde_json::json!("claude-fable-5-1");
    });
    rig.state
        .projects
        .set_review_settings(
            &rig.project_id,
            camerata_api_types::project::ReviewSettings {
                group_review_model: "claude-opus-5-5".to_string(),
                ..camerata_api_types::project::ReviewSettings::default()
            },
        )
        .expect("the project exists")
        .expect("the setting is accepted");
    std::fs::remove_file(&rig.group_review_down).unwrap();
    let calls_before = stage_calls(&rig.log).len();
    let job = resume_job(&rig).await;
    assert_eq!(job.status, "failed", "{:?}", job.message);
    let why = job.message.clone().unwrap_or_default();
    assert!(
        why.contains("claude-fable-5-1") && why.contains("claude-opus-5-5"),
        "{why}"
    );
    assert_eq!(
        stage_calls(&rig.log).len(),
        calls_before,
        "no model call was made"
    );
}

/// The version check follows the kept group review model: a run recorded on a model with no
/// minimum resumes on an old Claude Code even though the project's current default model needs a
/// newer one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(process_env)]
async fn a_cockpit_resume_checks_the_version_for_the_kept_group_review_model() {
    if !camerata_test_env::python3_on_path_for("a_cockpit_resume_checks_the_version_for_the_kept") {
        return;
    }
    let rig = interrupted_scan().await;
    patch_saved_run(&rig.run_id, |v| {
        v["models"]["group_review"] = serde_json::json!("claude-fable-5-1");
    });
    std::fs::remove_file(&rig.group_review_down).unwrap();
    let _old = camerata_test_env::EnvGuard::set("FAKE_VERSION", "2.1.285 (Claude Code)");
    let job = resume_job(&rig).await;
    assert_eq!(job.status, "done", "{:?}", job.message);
}

/// A resume whose kept group review model needs a newer Claude Code than the one installed is
/// refused before any model call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(process_env)]
async fn a_cockpit_resume_on_a_kept_model_needing_a_newer_claude_code_is_refused() {
    if !camerata_test_env::python3_on_path_for("a_cockpit_resume_on_a_kept_model_needing") {
        return;
    }
    let rig = interrupted_scan().await;
    patch_saved_run(&rig.run_id, |v| {
        v["models"]["group_review"] = serde_json::json!("claude-fable-5-2");
    });
    std::fs::remove_file(&rig.group_review_down).unwrap();
    let _old = camerata_test_env::EnvGuard::set("FAKE_VERSION", "2.1.285 (Claude Code)");
    let calls_before = stage_calls(&rig.log).len();
    let job = resume_job(&rig).await;
    assert_eq!(job.status, "failed", "{:?}", job.message);
    let why = job.message.clone().unwrap_or_default();
    assert!(why.contains("2.1.285") && why.contains("2.1.290"), "{why}");
    assert_eq!(stage_calls(&rig.log).len(), calls_before, "no model call");
}

/// Run to completion: a connection failure on the group review call ends the scan in bounded
/// time with the run incomplete, never hung. (Seen 2026-11-16: the job sat in retries for more
/// than five minutes.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(process_env)]
async fn a_503_outage_on_the_group_review_ends_the_scan_in_bounded_time_as_a_failed_part() {
    group_review_outage_ends_in_bounded_time("unavailable").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(process_env)]
async fn a_transport_outage_on_the_group_review_ends_the_scan_in_bounded_time_as_a_failed_part() {
    group_review_outage_ends_in_bounded_time("transport").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(process_env)]
async fn a_503_outage_on_the_scan_ends_the_job_in_bounded_time() {
    group_review_outage_ends_in_bounded_time("stage503:audit").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(process_env)]
async fn a_503_outage_on_calibration_ends_the_job_in_bounded_time() {
    group_review_outage_ends_in_bounded_time("stage503:calibration").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(process_env)]
async fn a_503_outage_on_the_fix_stage_ends_the_job_in_bounded_time() {
    group_review_outage_ends_in_bounded_time("stage503:fix").await;
}

/// Records every wait a retry asks for and returns at once, so an outage test asserts the
/// WAITS (what the policy decided) instead of how long the machine took.
#[derive(Default)]
struct WaitRecorder {
    waits: std::sync::Mutex<Vec<std::time::Duration>>,
}

impl WaitRecorder {
    fn secs(&self) -> Vec<u64> {
        self.waits
            .lock()
            .unwrap()
            .iter()
            .map(std::time::Duration::as_secs)
            .collect()
    }
}

impl camerata_llm::Sleeper for WaitRecorder {
    fn sleep<'a>(
        &'a self,
        d: std::time::Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        self.waits.lock().unwrap().push(d);
        Box::pin(async {})
    }
}

/// Installs a [`WaitRecorder`] as the process sleeper and removes it again, panic or not.
struct RecorderGuard(Arc<WaitRecorder>);

impl RecorderGuard {
    fn install() -> Self {
        let recorder = Arc::new(WaitRecorder::default());
        camerata_llm::set_process_sleeper(Some(recorder.clone()));
        Self(recorder)
    }
}

impl Drop for RecorderGuard {
    fn drop(&mut self) {
        camerata_llm::set_process_sleeper(None);
    }
}

async fn group_review_outage_ends_in_bounded_time(mode: &str) {
    if !camerata_test_env::python3_on_path_for("a_transport_outage_on_the_group_review") {
        return;
    }
    let recorder = RecorderGuard::install();
    let rig = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        interrupted_scan_with(mode),
    )
    .await
    .expect("the scan job hung on an outage (no end within 120 s)");
    assert_eq!(
        crate::run_store::read_record(&rig.run_id).unwrap().status,
        crate::run_store::RunStatus::Incomplete
    );
    // What the policy decided: it waited, no single call spent more than its 60 s budget waiting,
    // and each wait is one of the schedule's steps (5, 20, 60 clipped to 35 for a 503; 2, 8, 30
    // for a lost connection).
    let waits = recorder.0.secs();
    assert!(!waits.is_empty(), "the outage was retried: {waits:?}");
    let allowed: &[u64] = if mode == "transport" {
        &[2, 8, 30]
    } else {
        &[5, 20, 35]
    };
    for w in &waits {
        assert!(
            allowed.contains(w),
            "wait {w} is not a step of {allowed:?}: {waits:?}"
        );
    }
    let per_call: u64 = if mode == "transport" { 40 } else { 60 };
    assert_eq!(
        waits.iter().sum::<u64>() % per_call,
        0,
        "every call ran its whole schedule and no more: {waits:?}"
    );
    assert!(
        waits.contains(allowed.last().unwrap()),
        "the last step of the schedule was taken (a 503's is clipped to the 35 s left): {waits:?}"
    );
    // The failed part is listed, so a resume knows what to run.
    let scan = rig.state.get_last_scan(&rig.project_id).unwrap();
    assert!(
        scan.failed_passes.iter().any(|f| !f.scope_note),
        "the failed part is listed: {:?}",
        scan.failed_passes
    );
    // The outage ends and a resume finishes the run.
    std::fs::remove_file(&rig.outage_flag).unwrap();
    let job = resume_job(&rig).await;
    assert_eq!(job.status, "done", "{:?}", job.message);
    assert_eq!(
        crate::run_store::read_record(&rig.run_id).unwrap().status,
        crate::run_store::RunStatus::Complete
    );
}

// ---- a run keeps its finder for life, and a server scan on an agent project runs the agent ----

/// A run that started on the engine finder resumes on it even when the project's finder was
/// changed to the agent in between: the run says so once, records the engine, and builds no rooms.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(process_env)]
async fn a_cockpit_resume_keeps_the_finder_the_run_started_with_and_says_so() {
    if !camerata_test_env::python3_on_path_for("a_cockpit_resume_keeps_the_finder_the_run") {
        return;
    }
    let rig = interrupted_scan().await;
    let project = rig.state.projects.get(&rig.project_id).unwrap();
    rig.state
        .projects
        .set_review_settings(
            &rig.project_id,
            camerata_api_types::project::ReviewSettings {
                finder: camerata_api_types::project::ProjectFinder::Agent,
                ..project.review
            },
        )
        .expect("the project exists")
        .expect("the setting is accepted");
    std::fs::remove_file(&rig.outage_flag).unwrap();
    let job = resume_job(&rig).await;
    assert_eq!(job.status, "done", "{:?}", job.message);
    let notice = "The finder stays on engine, the one this run started with.";
    let record = crate::run_store::read_record(&rig.run_id).unwrap();
    assert_eq!(
        record.notes.iter().filter(|n| n.as_str() == notice).count(),
        1,
        "{:?}",
        record.notes
    );
    assert_eq!(record.models.finder, "engine");
    assert!(
        !crate::run_store::runs_dir()
            .join(&rig.run_id)
            .join("finder-rooms")
            .exists(),
        "the engine finder builds no rooms"
    );
}

/// The one server scan with an agent project: the agent finder runs (its rooms are built in the
/// run's folder, with the setup the server built), not the engine's scan pass.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(process_env)]
async fn a_server_scan_on_an_agent_project_runs_the_agent_finder() {
    if !camerata_test_env::python3_on_path_for("a_server_scan_on_an_agent_project") {
        return;
    }
    let rig = interrupted_scan_with_finder().await;
    let job = &rig.job;
    let message = job.message.clone().unwrap_or_default();
    assert!(!message.contains("needs a finder setup"), "{message}");
    if let Some(report) = rig.state.get_last_scan(&rig.project_id) {
        let err = report.ai_error.clone().unwrap_or_default();
        assert!(!err.contains("needs a finder setup"), "{err}");
    }
    let rooms = crate::run_store::runs_dir()
        .join(&rig.run_id)
        .join("finder-rooms");
    let made = std::fs::read_dir(&rooms)
        .unwrap_or_else(|e| panic!("{} was not made: {e}", rooms.display()))
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("room-"))
        .count();
    assert!(made >= 1, "no room was built in {}", rooms.display());
    assert!(
        !stage_calls(&rig.log).iter().any(|s| s == "audit"),
        "the engine's scan pass ran: {:?}",
        stage_calls(&rig.log)
    );
}

#[test]
fn a_resume_with_a_limit_raises_the_saved_runs_spend_limit() {
    let saved: AuditReq =
        serde_json::from_value(serde_json::json!({ "max_cost_usd": 260.0 })).unwrap();
    let raised = ResumeQuery {
        max_cost_usd: Some(320.7),
    }
    .apply(saved);
    assert_eq!(raised.max_cost_usd, Some(320.7));
    let saved: AuditReq =
        serde_json::from_value(serde_json::json!({ "max_cost_usd": 260.0 })).unwrap();
    let kept = ResumeQuery::default().apply(saved);
    assert_eq!(
        kept.max_cost_usd,
        Some(260.0),
        "no limit sent keeps the saved one"
    );
}

#[test]
fn a_raised_limit_is_honoured_only_for_a_run_the_spend_limit_stopped() {
    let stopped = vec![
        "the spend limit stopped the run; 2 parts did not run and were never checked".to_string(),
    ];
    assert!(reasons_name_spend_limit(&stopped));
    let outage_or_limit = vec![
        "the spend limit or an outage stopped the run; 1 part did not run and was never checked"
            .to_string(),
    ];
    assert!(reasons_name_spend_limit(&outage_or_limit));
    let failed = vec!["1 part failed (a/b, calibration)".to_string()];
    assert!(!reasons_name_spend_limit(&failed));
    assert!(!reasons_name_spend_limit(&[]));
    let outage = vec!["the run stopped because the model provider could not be reached; 1 part did not run and was never checked".to_string()];
    assert!(!reasons_name_spend_limit(&outage));

    // A stopped checkpoint in the run folder is the other way a run is a spend-limit stop.
    let dir = tempfile::tempdir().unwrap();
    assert!(!checkpoint_stopped(dir.path()));
    let cp = crate::cost_checkpoint::CostCheckpoint {
        stopped: true,
        ..Default::default()
    };
    std::fs::write(
        dir.path().join(crate::cost_checkpoint::CHECKPOINT_FILE),
        serde_json::to_vec(&cp).unwrap(),
    )
    .unwrap();
    assert!(checkpoint_stopped(dir.path()));
    let running = crate::cost_checkpoint::CostCheckpoint::default();
    std::fs::write(
        dir.path().join(crate::cost_checkpoint::CHECKPOINT_FILE),
        serde_json::to_vec(&running).unwrap(),
    )
    .unwrap();
    assert!(!checkpoint_stopped(dir.path()));
}

#[test]
fn a_limit_that_is_not_a_positive_number_is_the_smallest_limit_never_no_limit() {
    for bad in [f64::NAN, 0.0, -5.0, f64::INFINITY, f64::NEG_INFINITY] {
        let req: AuditReq =
            serde_json::from_value(serde_json::json!({ "max_cost_usd": null })).unwrap();
        let req = AuditReq {
            max_cost_usd: Some(bad),
            ..req
        };
        let controls = req.run_controls(None, None);
        assert_eq!(
            controls.max_cost_usd,
            Some(f64::MIN_POSITIVE),
            "{bad} must not remove the guard"
        );
    }
}
