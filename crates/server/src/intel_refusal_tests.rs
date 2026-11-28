//! The owner's ruling on #587: Intel Macs are not supported. The app's two scan paths (the
//! synchronous `onboard_audit` and the background audit job) refuse on one before any model call,
//! in one plain line, and install nothing.

use super::*;
use camerata_test_env::EnvGuard;

struct Fixture {
    state: AppState,
    fakes: crate::pinned_tools::fixtures::FakeTools,
    _root: tempfile::TempDir,
    _env: Vec<EnvGuard>,
}

/// A project over `acme/one` (a committed git folder), served on an Intel Mac.
fn on_an_intel_mac() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/app.py"), "import os\nq = os.environ['Q']\n").unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "intel@camerata.local"]);
    git(&["config", "user.name", "Intel Test"]);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "fixture"]);
    let env = vec![
        EnvGuard::set("CAMERATA_STATE_DIR", root.path().join("state")),
        EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1"),
    ];
    let fakes = crate::pinned_tools::fixtures::FakeTools::new();
    let pins = fakes.env.clone();
    let intel = crate::pinned_tools::ToolEnv::for_machine(
        "macos",
        "x86_64",
        fakes.tooling(),
        fakes.source.clone(),
    )
    .with_pins(move |a| pins.pins(a));
    let state = AppState::new(Arc::new(InMemoryStoryStore::new())).with_tool_env(intel);
    let project = state
        .projects
        .create("Intel", vec!["acme/one".to_string()])
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
    Fixture {
        state,
        fakes,
        _root: root,
        _env: env,
    }
}

fn audit_request() -> AuditReq {
    serde_json::from_value(serde_json::json!({
        "repos": ["acme/one"],
        "mode": "parallel",
        "incremental": false,
        "run_ai_review": true,
        "run_deterministic": true,
    }))
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(process_env)]
async fn the_synchronous_scan_refuses_an_intel_mac_in_one_line_before_any_model_call() {
    let fx = on_an_intel_mac();
    let Json(report) = onboard_audit(State(fx.state.clone()), Json(audit_request())).await;
    assert_eq!(
        report.ai_error.as_deref(),
        Some(crate::pinned_tools::platform::INTEL_MAC_REFUSAL),
        "{:?}",
        report.message
    );
    assert!(report.findings.is_empty(), "nothing was scanned");
    assert!(report.actual_usage.is_none(), "no model was asked");
    assert!(
        fx.fakes.source.fetched().is_empty(),
        "nothing was downloaded"
    );
    assert!(!fx.fakes.tooling().exists(), "nothing was installed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(process_env)]
async fn the_audit_job_refuses_an_intel_mac_in_one_line_before_any_model_call() {
    let fx = on_an_intel_mac();
    let id = start_audit_job(&fx.state, audit_request(), AuditJobOptions::default()).await;
    let started = std::time::Instant::now();
    let job = loop {
        let job = fx.state.jobs.get(&id).expect("the job exists");
        if job.status != "running" {
            break job;
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(120));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    assert_eq!(job.status, "failed", "{:?}", job.message);
    assert_eq!(
        job.message.as_deref(),
        Some(crate::pinned_tools::platform::INTEL_MAC_REFUSAL)
    );
    assert!(job.report.is_none(), "no report was made");
    assert!(
        fx.fakes.source.fetched().is_empty(),
        "nothing was downloaded"
    );
    assert!(!fx.fakes.tooling().exists(), "nothing was installed");
}
