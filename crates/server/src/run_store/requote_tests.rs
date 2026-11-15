//! The two-step quote on a resume (owner ruling R-9): a run that made its re-quote after the scan
//! stage prices what remains from it, and the replies a resume already holds for the work ahead
//! come off the live re-quote.

use std::collections::BTreeMap;

use super::journal::{
    load_requotes, save_requote, stored_ahead_usd, CallEntry, CallFile, EntryStatus,
};
use super::*;
use camerata_ui_core::scan::Requote;

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

/// A run whose pre-flight said $10.00.
fn new_run(repo: &tempfile::TempDir) -> Arc<RunStore> {
    RunStore::create(NewRun {
        project_key: "proj-1".to_string(),
        repos: vec![("acme/api".to_string(), repo.path().to_path_buf())],
        models: ModelsRecord {
            audit: "claude-a".to_string(),
            calibration: "claude-a".to_string(),
            fix: "claude-a".to_string(),
            backend: "api".to_string(),
            mode: "parallel".to_string(),
            resolved: true,
        },
        ai_scan_key: AiScanKey {
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

/// Store one good reply of `kind` about `repo` that cost `cost`.
fn store_call(dir: &std::path::Path, key: &str, kind: PartKind, repo: &str, cost: f64) {
    let file = CallFile {
        key: key.to_string(),
        kind,
        repo: repo.to_string(),
        files: Vec::new(),
        entries: BTreeMap::from([(
            0,
            CallEntry {
                attempt: 0,
                status: EntryStatus::Done,
                at: "2026-11-15T00:00:00Z".to_string(),
                response: None,
                error: None,
                class: None,
                cost_usd: cost,
                reason: None,
                output_path: None,
            },
        )]),
    };
    std::fs::create_dir_all(dir.join("calls")).unwrap();
    std::fs::write(
        dir.join("calls").join(format!("{key}.json")),
        serde_json::to_vec(&file).unwrap(),
    )
    .unwrap();
}

fn requote(expected: f64, ceiling: f64) -> Requote {
    Requote {
        repo: "acme/api".to_string(),
        sites: 120,
        files: 40,
        scan_spent_usd: 0.4,
        expected_usd: expected,
        ceiling_usd: ceiling,
        remaining_expected_usd: expected - 0.4,
        remaining_ceiling_usd: ceiling - 0.4,
        preflight_expected_usd: 10.0,
        preflight_ceiling_usd: 12.0,
        cap_usd: None,
        resumed: false,
    }
}

/// R-9: a resume of a run that made its re-quote prices what remains from the re-quote, less what
/// the stored calls cost, and says so in the pre-flight's format; without one it still prices
/// what remains from the pre-flight figure, as before.
#[test]
#[serial_test::serial(process_env)]
fn a_resume_requotes_only_the_remaining_work() {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    let id = store.run_id();
    let dir = runs_dir().join(&id);
    drop(store);
    store_call(&dir, "scan-1", PartKind::Scan, "acme/api", 0.4);
    store_call(&dir, "cal-1", PartKind::Calibration, "acme/api", 0.6);

    // No re-quote yet: what remains is the pre-flight's $10.00 less the $1.00 spent.
    let before = estimate_resume(&id).unwrap();
    assert!((before.usd - 9.0).abs() < 1e-9, "{before:?}");
    assert!(!before.render().contains("re-quote"), "{}", before.render());

    save_requote(&dir, &requote(3.0, 3.5));
    assert_eq!(load_requotes(&dir).len(), 1);
    let after = estimate_resume(&id).unwrap();
    assert!(
        (after.usd - 2.0).abs() < 1e-9,
        "what remains is the re-quote's $3.00 less the $1.00 the stored calls cost: {after:?}"
    );
    let text = after.render();
    assert!(
        text.contains(
            "re-quote after the scan stage: about $2.00, at most $2.50 for the work that remains"
        ),
        "{text}"
    );
    assert!(text.contains("acme/api"), "{text}");
    for dash in ['\u{2014}', '\u{2013}'] {
        assert!(!text.contains(dash), "{text}");
    }

    // A later re-quote (the resumed attempt's own) supersedes the earlier one.
    save_requote(&dir, &requote(4.0, 4.0));
    assert!((estimate_resume(&id).unwrap().usd - 3.0).abs() < 1e-9);
}

/// The live re-quote of a resumed run takes off the stored replies of the work still ahead: this
/// repo's stages after the scan, any reply about a later repo, and the run-wide stages. The scan
/// stage's own replies and an earlier repo's are behind it and do not count.
#[test]
fn the_stored_replies_ahead_are_the_later_stages_and_the_later_repos() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    store_call(d, "scan-api", PartKind::Scan, "acme/api", 1.0);
    store_call(d, "alt-api", PartKind::Alternatives, "acme/api", 2.0);
    store_call(d, "cal-api", PartKind::Calibration, "acme/api", 0.25);
    store_call(d, "fix-api", PartKind::Fix, "acme/api", 0.5);
    store_call(d, "scan-web", PartKind::Scan, "acme/web", 0.125);
    store_call(d, "review", PartKind::GroupReview, "", 0.0625);
    store_call(d, "cal-old", PartKind::Calibration, "acme/old", 4.0);
    let ahead = stored_ahead_usd(d, "acme/api", &["acme/web"]);
    assert!(
        (ahead - (0.25 + 0.5 + 0.125 + 0.0625)).abs() < 1e-9,
        "{ahead}"
    );
    assert_eq!(
        stored_ahead_usd(tempfile::tempdir().unwrap().path(), "acme/api", &[]),
        0.0
    );
}
