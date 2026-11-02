//! Every path that hands a finished report to a reader hands it keyed. The stable site key
//! (`camerata_api_types::site_key`) is assigned by `ScanReport::assign_site_keys` where a report
//! becomes final; each test here drives one of those paths and checks the report that LEAVES it,
//! not a copy a later step re-keys:
//!
//! * the last-scan store (`AppState::set_last_scan_in_memory`),
//! * `POST /api/onboard/scan` (`onboard_scan`), which returns no findings at all (see its test),
//! * `POST /api/onboard/audit` (`onboard_audit`),
//! * the async audit job (`start_audit_job`).
//!
//! The audit paths run on a real git folder with the same line twice in one file (two sites with
//! one fingerprint, told apart only by the ordinal) and, where the path runs the dependency
//! audit, a fake `osv-scanner` on PATH that reports two advisories on one package version. No
//! model is called (deterministic scans only), and the vulnerability feeds are pointed at a dead
//! local proxy so nothing leaves the machine.

use super::*;
use camerata_test_env::EnvGuard;

const URL_RULE: &str = "ARCH-NO-SECRETS-IN-URL-1";

/// `osv-scanner --version` works; a scan prints two advisories on `time@0.1.45` in the folder
/// it was given and exits 1, which is osv-scanner's "vulnerabilities found".
const FAKE_OSV: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "osv-scanner 0.0.0"; exit 0; fi
d="$4"
cat <<JSON
{"results":[{"source":{"path":"$d/Cargo.lock","type":"lockfile"},
 "packages":[{"package":{"name":"time","version":"0.1.45","ecosystem":"crates.io"},
 "vulnerabilities":[
  {"id":"RUSTSEC-2020-0071","summary":"first","database_specific":{"severity":"HIGH"}},
  {"id":"RUSTSEC-2021-0001","summary":"second","database_specific":{"severity":"HIGH"}}
 ]}]}]}
JSON
exit 1
"#;

const REPEATED_URL: &str =
    "    \"https://hooks.example.com/notify?token=abc123def456\".to_string()\n";

struct Fixture {
    state: AppState,
    _root: tempfile::TempDir,
    _env: Vec<EnvGuard>,
}

/// A project over `acme/one`, a git folder holding the repeated line, and (with `osv`) a fake
/// osv-scanner first on PATH.
fn fixture(osv: bool) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    let bin = root.path().join("bin");
    let state_dir = root.path().join("state");
    for d in [&repo, &bin, &state_dir, &repo.join("src")] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(
        repo.join("src/client.rs"),
        format!("pub fn a() -> String {{\n{REPEATED_URL}}}\npub fn b() -> String {{\n{REPEATED_URL}}}\n"),
    )
    .unwrap();
    std::fs::write(repo.join(".gitignore"), "target/\n").unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "site-key@camerata.local"]);
    git(&["config", "user.name", "Site Key Test"]);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "fixture"]);

    let mut env = Vec::new();
    if osv {
        use std::os::unix::fs::PermissionsExt;
        let osv_bin = bin.join("osv-scanner");
        std::fs::write(&osv_bin, FAKE_OSV).unwrap();
        std::fs::set_permissions(&osv_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        env.push(EnvGuard::set("PATH", path));
    } else {
        env.push(EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1"));
    }
    env.push(EnvGuard::set("CAMERATA_STATE_DIR", &state_dir));
    env.push(EnvGuard::set(
        crate::scan_tools::DISABLE_SEMGREP_ENV_VAR,
        "1",
    ));
    // The vulnerability feeds go to a closed local port, so they fail at once and offline.
    for var in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
    ] {
        env.push(EnvGuard::set(var, "http://127.0.0.1:9"));
    }

    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let project = state
        .projects
        .create("Keys", vec!["acme/one".to_string()])
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
        _root: root,
        _env: env,
    }
}

fn audit_request() -> AuditReq {
    serde_json::from_value(serde_json::json!({
        "repos": ["acme/one"],
        "mode": "parallel",
        "incremental": false,
        "run_ai_review": false,
        "run_deterministic": true,
    }))
    .unwrap()
}

/// Every finding is keyed and no two share a key; the repeated line gives two rows of one
/// fingerprint with different keys; and, when `advisories`, so do the two advisories.
fn assert_keyed(report: &crate::onboard::ScanReport, advisories: bool) {
    assert!(!report.findings.is_empty(), "the scan found nothing");
    for f in &report.findings {
        assert!(
            !f.site_key.is_empty(),
            "{} at {}:{} has no site key",
            f.rule_id,
            f.path,
            f.line
        );
    }
    let keys: std::collections::BTreeSet<&str> = report
        .findings
        .iter()
        .map(|f| f.site_key.as_str())
        .collect();
    assert_eq!(
        keys.len(),
        report.findings.len(),
        "two rows share a site key"
    );
    let repeats: Vec<&str> = report
        .findings
        .iter()
        .filter(|f| f.rule_id == URL_RULE)
        .map(|f| f.site_key.as_str())
        .collect();
    assert_eq!(
        repeats.len(),
        2,
        "the repeated line is two rows: {repeats:?}"
    );
    assert_ne!(repeats[0], repeats[1]);
    if advisories {
        let deps: Vec<&crate::onboard::Finding> = report
            .findings
            .iter()
            .filter(|f| f.rule_id == crate::dep_audit::DEP_AUDIT_RULE_ID)
            .collect();
        assert_eq!(deps.len(), 2, "two advisories on one package version");
        assert_eq!(deps[0].snippet, deps[1].snippet);
        assert_ne!(deps[0].site_key, deps[1].site_key);
    }
}

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

/// An unkeyed report as a pipeline builds it: a repeated line and two advisories on one package.
fn unkeyed_report() -> crate::onboard::ScanReport {
    let row = |line: usize| crate::onboard::Finding {
        repo: "acme/one".to_string(),
        path: "src/client.rs".to_string(),
        line,
        rule_id: URL_RULE.to_string(),
        snippet: "same()".to_string(),
        ..Default::default()
    };
    let mut report = crate::onboard::ScanReport::gated(&["acme/one".to_string()]);
    report.gated = false;
    report.findings = vec![row(9), row(3)];
    report.findings.extend(
        crate::dep_audit::parse_osv_json(
            "acme/one",
            std::path::Path::new("/w"),
            r#"{"results":[{"source":{"path":"/w/Cargo.lock","type":"lockfile"},
              "packages":[{"package":{"name":"time","version":"0.1.45","ecosystem":"crates.io"},
              "vulnerabilities":[
                {"id":"RUSTSEC-2021-0001","summary":"second","database_specific":{"severity":"HIGH"}},
                {"id":"RUSTSEC-2020-0071","summary":"first","database_specific":{"severity":"HIGH"}}
              ]}]}]}"#,
        )
        .unwrap(),
    );
    assert!(report.findings.iter().all(|f| f.site_key.is_empty()));
    report
}

#[test]
fn the_last_scan_store_hands_out_keyed_reports() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    state.set_last_scan_in_memory("proj-1".to_string(), unkeyed_report());
    let stored = state.get_last_scan("proj-1").expect("stored");
    assert_keyed(&stored, true);
    let recent = state.get_recent_scan().expect("the project-agnostic copy");
    assert_keyed(&recent, true);
}

/// `POST /api/onboard/scan` reads the folders (stacks, proposals) and runs no check, so its
/// report has no findings to key. If it ever gains some, this fails and the route must key them
/// the way the audit routes do (`ScanReport::assign_site_keys`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(process_env)]
async fn onboard_scan_reports_no_findings_so_there_is_nothing_to_key() {
    let fx = fixture(false);
    let Json(report) = onboard_scan(
        State(fx.state.clone()),
        Json(serde_json::from_value(serde_json::json!({ "repos": ["acme/one"] })).unwrap()),
    )
    .await;
    assert!(report.files_scanned > 0, "the folder was read");
    assert!(
        report.findings.is_empty(),
        "the floor scan now returns findings; key them before they leave the route: {:?}",
        report.findings.len()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(process_env)]
async fn onboard_audit_returns_a_keyed_report() {
    let fx = fixture(true);
    let Json(report) = onboard_audit(State(fx.state.clone()), Json(audit_request())).await;
    assert_keyed(&report, true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(process_env)]
async fn the_audit_job_returns_a_keyed_report() {
    let fx = fixture(true);
    let id = start_audit_job(&fx.state, audit_request(), AuditJobOptions::default()).await;
    let job = finished(&fx.state, &id).await;
    assert_eq!(job.status, "done", "{:?}", job.message);
    assert_keyed(
        &job.report.expect("the finished job carries its report"),
        true,
    );
}
