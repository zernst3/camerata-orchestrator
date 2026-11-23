#![allow(clippy::unwrap_used)]
//! End-to-end, ZERO model calls (item 31): `camerata inspect` and `camerata count` with a file
//! over the read limit.
//!
//! The run refuses to start while a file over the limit is not excluded by agreement, naming the
//! file with its size and writing nothing; with `--exclude` naming it the same run goes through,
//! its manifest states the same billable lines `camerata count` prints, and the export lists the
//! file as excluded by agreement. A pattern that is not valid is refused by name.

use std::io::Read;
use std::path::Path;

use camerata::count_cmd::{run_count, CountArgs};
use camerata::inspect_cmd::{run_inspect_with_key_presence, InspectArgs, InspectError};
use camerata_server::llm::ProjectBackend;

fn git(dest: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .current_dir(dest)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A one-commit repo: a small source file, and a file over the 1 MiB read limit under `seeds/`.
fn repo_with_a_big_seed_file() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("seeds")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn a() -> u32 {\n    1\n}\n").unwrap();
    std::fs::write(root.join("seeds/big.js"), "x".repeat(1024 * 1024 + 5)).unwrap();
    git(root, &["init", "-q", "-b", "main"]);
    git(
        root,
        &["config", "user.email", "exclusion-cli@example.test"],
    );
    git(root, &["config", "user.name", "Exclusion CLI"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "fixture"]);
    dir
}

/// A floor-only run (the compliance gate blocks the AI review, so no model is called).
fn args(repo: &Path, export: &Path, exclude: &[&str]) -> InspectArgs {
    InspectArgs {
        repo: repo.to_path_buf(),
        also_repos: Vec::new(),
        repo_names: Vec::new(),
        export: export.to_path_buf(),
        backend: ProjectBackend::Api,
        batch: false,
        realtime: false,
        model: None,
        calibration_model: None,
        fix_model: None,
        clone_judge_model: None,
        group_review_model: None,
        dup_check_model: None,
        escalation_rate: None,
        escalation_floor: None,
        full: true,
        verbose: false,
        max_cost_usd: None,
        efficiency: Vec::new(),
        client: "Example Client Ltd".to_string(),
        client_engagement: false,
        run_client_tooling: false,
        consensus: false,
        finder: Default::default(),
        exclude: exclude.iter().map(|p| p.to_string()).collect(),
    }
}

fn failed_text(err: InspectError) -> String {
    match err {
        InspectError::Failed(text) => text,
        other => format!("{other:?}"),
    }
}

const PLANTED_KEY: &str = "ghp_0123456789012345678901234567890123456";

fn read_zip_entry(export: &Path, name: &str) -> String {
    let bytes = std::fs::read(export).unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut text = String::new();
    archive
        .by_name(name)
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    text
}

/// Item 1 (Zach's ruling): a file over the limit is a warning, never a refusal. The run goes
/// ahead, the file is not read or billed, and the secret checks still find the key inside it.
#[tokio::test]
async fn a_file_over_the_limit_warns_and_the_run_goes_ahead_and_finds_its_key() {
    let repo = repo_with_a_big_seed_file();
    let mut big = "var row = 1;\n".repeat(90_000);
    big.push_str(&format!("var t = \"{PLANTED_KEY}\";\n"));
    std::fs::write(repo.path().join("seeds/big.js"), big).unwrap();
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("inspect.zip");

    let outcome = run_inspect_with_key_presence(args(repo.path(), &export, &[]), false)
        .await
        .expect("a file over the limit is a warning, so the run goes ahead");
    assert_eq!(outcome.actual_usage.as_ref().map_or(0, |u| u.calls), 0);
    let inspection = read_zip_entry(&export, "json/inspection.json");
    assert!(
        !inspection.contains(PLANTED_KEY),
        "the key is masked everywhere"
    );
    let value: serde_json::Value = serde_json::from_str(&inspection).unwrap();
    let hit = value["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .find(|f| f["path"] == "seeds/big.js" && f["rule_id"] == "SEC-NO-VENDOR-TOKEN-1")
        .unwrap_or_else(|| panic!("the key inside the over-limit file is found: {value}"));
    assert_eq!(hit["line"], 90_001);
    let summary = value["coverage"]["skipped_summary"]
        .as_str()
        .unwrap_or_default();
    assert!(
        summary.contains("1 file was not read for the inspection")
            && summary.contains("1 of these files was still searched for secrets"),
        "{summary}"
    );
    // Not billed: the manifest bills what `camerata count` prints.
    let manifest: serde_json::Value =
        serde_json::from_str(&read_zip_entry(&export, "manifest.json")).unwrap();
    assert_eq!(manifest["inspection"]["counted_lines"], 3);
}

#[tokio::test]
async fn excluding_the_file_by_agreement_lets_the_run_through_and_names_it_in_the_report() {
    let repo = repo_with_a_big_seed_file();
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("inspect.zip");

    // `*` stays within the repo-name folder, so this is `<repo>/seeds/**` for any folder name.
    let outcome = run_inspect_with_key_presence(args(repo.path(), &export, &["*/seeds/**"]), false)
        .await
        .expect("excluded by agreement, the file no longer blocks the run");
    assert_eq!(outcome.actual_usage.as_ref().map_or(0, |u| u.calls), 0);

    let bytes = std::fs::read(&export).unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut read_entry = |name: &str| {
        let mut text = String::new();
        archive
            .by_name(name)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        text
    };
    let inspection: serde_json::Value =
        serde_json::from_str(&read_entry("json/inspection.json")).unwrap();
    let skipped = inspection["coverage"]["skipped_files"].as_array().unwrap();
    assert_eq!(skipped.len(), 1, "{inspection}");
    assert_eq!(skipped[0]["path"], "seeds/big.js");
    assert_eq!(skipped[0]["reason"], "excluded by agreement");
    // An excluded file is never opened: no finding of any kind names it, and Coverage says it
    // was not searched for secrets.
    assert!(
        inspection["findings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["path"] != "seeds/big.js"),
        "{inspection}"
    );
    let summary = inspection["coverage"]["skipped_summary"].as_str().unwrap();
    assert_eq!(
        summary,
        "1 file was not read for the inspection: 1 excluded by agreement. \
         1 file was not searched for secrets: 1 excluded by agreement."
    );

    // The manifest bills what `camerata count` prints for the same exclusion.
    let manifest: serde_json::Value = serde_json::from_str(&read_entry("manifest.json")).unwrap();
    let counted = run_count(&CountArgs {
        repo: repo.path().to_path_buf(),
        json: true,
        exclude: vec!["*/seeds/**".to_string()],
        repo_name: None,
    })
    .unwrap();
    let counted: serde_json::Value = serde_json::from_str(&counted).unwrap();
    assert_eq!(
        manifest["inspection"]["counted_lines"],
        counted["total_lines"]
    );
    assert_eq!(counted["total_lines"], 3);
}

#[tokio::test]
async fn an_invalid_exclusion_pattern_is_refused_by_name() {
    let repo = repo_with_a_big_seed_file();
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("inspect.zip");
    let err = run_inspect_with_key_presence(args(repo.path(), &export, &["*/[broken"]), false)
        .await
        .expect_err("an invalid pattern is refused");
    let text = failed_text(err);
    assert!(text.contains("--exclude"), "{text}");
    assert!(text.contains("*/[broken"), "{text}");
    assert!(!export.exists());
}

#[test]
fn count_lists_the_over_limit_file_until_it_is_excluded() {
    let repo = repo_with_a_big_seed_file();
    let plain = run_count(&CountArgs {
        repo: repo.path().to_path_buf(),
        json: false,
        exclude: Vec::new(),
        repo_name: None,
    })
    .unwrap();
    assert!(
        plain.contains("not read, over the 1 MB file size limit (1 file):"),
        "{plain}"
    );
    assert!(plain.contains("  seeds/big.js (1.1 MB)"), "{plain}");

    let agreed = run_count(&CountArgs {
        repo: repo.path().to_path_buf(),
        json: false,
        exclude: vec!["*/seeds/**".to_string()],
        repo_name: None,
    })
    .unwrap();
    assert!(
        agreed.contains("not read, excluded by agreement (1 file):"),
        "{agreed}"
    );
    assert!(!agreed.contains("over the 1 MB"), "{agreed}");
}

/// Round 1 item 2: the count names the repo exactly as inspect does, so one exclusion pattern
/// means the same files in both, even when the clone folder is not called the repo.
#[tokio::test]
async fn count_and_run_bill_the_same_when_the_clone_folder_is_named_differently() {
    let repo = repo_with_a_big_seed_file(); // the folder name is random, the repo is "acme-app"
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("inspect.zip");
    let mut run = args(repo.path(), &export, &["acme-app/seeds/**"]);
    run.repo_names = vec!["acme/acme-app".to_string()];
    run_inspect_with_key_presence(run, false)
        .await
        .expect("the run goes through");
    let bytes = std::fs::read(&export).unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut manifest = String::new();
    archive
        .by_name("manifest.json")
        .unwrap()
        .read_to_string(&mut manifest)
        .unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();

    let counted = run_count(&CountArgs {
        repo: repo.path().to_path_buf(),
        json: true,
        exclude: vec!["acme-app/seeds/**".to_string()],
        repo_name: Some("acme/acme-app".to_string()),
    })
    .unwrap();
    let counted: serde_json::Value = serde_json::from_str(&counted).unwrap();
    assert_eq!(
        manifest["inspection"]["counted_lines"],
        counted["total_lines"]
    );
    assert_eq!(counted["total_lines"], 3);
    assert_eq!(counted["not_read"][0]["reason"], "excluded by agreement");
}

#[test]
fn count_refuses_a_pattern_that_matches_nothing_and_a_bad_repo_name() {
    let repo = repo_with_a_big_seed_file();
    let err = run_count(&CountArgs {
        repo: repo.path().to_path_buf(),
        json: false,
        exclude: vec!["*/sedes/**".to_string()],
        repo_name: None,
    })
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("*/sedes/**") && err.contains("matches no file"),
        "{err}"
    );
    let err = run_count(&CountArgs {
        repo: repo.path().to_path_buf(),
        json: false,
        exclude: Vec::new(),
        repo_name: Some("not a repo name".to_string()),
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("--repo-name"), "{err}");
}

#[tokio::test]
async fn a_run_is_refused_while_a_pattern_matches_nothing() {
    let repo = repo_with_a_big_seed_file();
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("inspect.zip");
    let err = run_inspect_with_key_presence(
        args(repo.path(), &export, &["*/seeds/**", "*/sedes/**"]),
        false,
    )
    .await
    .expect_err("a pattern that names nothing refuses the run");
    let text = failed_text(err);
    assert!(
        text.contains("*/sedes/**") && text.contains("match"),
        "{text}"
    );
    assert!(!export.exists());
}

/// Gap 5, item 1: a file with a line longer than 97 KB no longer refuses the CLI run; `count`
/// still lists it.
#[tokio::test]
async fn a_line_longer_than_the_page_warns_the_run_goes_ahead_and_shows_in_the_count() {
    let repo = repo_with_a_big_seed_file();
    std::fs::write(repo.path().join("seeds/big.js"), "x\n").unwrap();
    std::fs::write(repo.path().join("seeds/wide.js"), "y".repeat(100_001)).unwrap();
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("inspect.zip");
    run_inspect_with_key_presence(args(repo.path(), &export, &[]), false)
        .await
        .expect("a long line is a warning, so the run goes ahead");
    assert!(export.exists());
    let counted = run_count(&CountArgs {
        repo: repo.path().to_path_buf(),
        json: false,
        exclude: Vec::new(),
        repo_name: None,
    })
    .unwrap();
    assert!(
        counted.contains("not read, has a line longer than 97 KB (1 file):"),
        "{counted}"
    );
    assert!(counted.contains("seeds/wide.js"), "{counted}");
}

/// Round 2 item 3: `--exclude 'app/seeds/**'` with two repos both named `app` is refused, naming
/// both, instead of silently excluding in both.
#[tokio::test]
async fn an_exclude_pattern_with_a_name_two_repos_share_is_refused() {
    let one = repo_with_a_big_seed_file();
    let two = repo_with_a_big_seed_file();
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("inspect.zip");
    let mut run = args(one.path(), &export, &["app/seeds/**"]);
    run.also_repos = vec![two.path().to_path_buf()];
    run.repo_names = vec!["acme/app".to_string(), "other/app".to_string()];
    let text = failed_text(
        run_inspect_with_key_presence(run, false)
            .await
            .expect_err("refused"),
    );
    assert!(
        text.contains("ambiguous") && text.contains("acme/app") && text.contains("other/app"),
        "{text}"
    );
    assert!(!export.exists());
}

/// Round 2 item 1: a project-wide list works repo by repo for a quote: a pattern written for
/// another repo is not judged here (it would otherwise read as "matches no file"), it is printed
/// as "not for this repo", and the figures print.
#[test]
fn count_prints_figures_when_the_list_holds_another_repos_pattern() {
    let repo = repo_with_a_big_seed_file();
    let text = run_count(&CountArgs {
        repo: repo.path().to_path_buf(),
        json: false,
        exclude: vec![
            "acme-app/seeds/**".to_string(),
            "other/dump.sql".to_string(),
        ],
        repo_name: Some("acme/acme-app".to_string()),
    })
    .unwrap();
    assert!(text.contains("TOTAL"), "{text}");
    assert!(text.contains("not for this repo: other/dump.sql"), "{text}");
    assert!(
        text.contains("not read, excluded by agreement (1 file):"),
        "{text}"
    );
    // Its own void pattern is still refused.
    let err = run_count(&CountArgs {
        repo: repo.path().to_path_buf(),
        json: false,
        exclude: vec!["acme-app/sedes/**".to_string()],
        repo_name: Some("acme/acme-app".to_string()),
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("acme-app/sedes/**"), "{err}");
}

/// Zach's ruling: a pattern that matches only files never read (a `node_modules` folder) is a
/// warning, never a refusal; the package still lists it, marked as removing nothing that would
/// have been read. A pattern matching nothing at all still refuses (see above).
#[tokio::test]
async fn a_never_read_folder_pattern_lets_the_run_through_and_is_listed_with_its_marking() {
    let repo = repo_with_a_big_seed_file();
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("inspect.zip");
    run_inspect_with_key_presence(
        args(repo.path(), &export, &["*/seeds/**", "*/node_modules/**"]),
        false,
    )
    .await
    .expect("a pattern over a never-read folder is a warning, not a refusal");
    let bytes = std::fs::read(&export).unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut text = String::new();
    archive
        .by_name("json/inspection.json")
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    assert!(
        text.contains("excluded by agreement; nothing here is read by default"),
        "{text}"
    );
    assert!(text.contains("node_modules"), "{text}");
}

/// Item 10: the warning for a file over the limit is the line the CLI prints to stderr before the
/// run goes on (`write_start_warnings` is the function `run_inspect` prints with). `inspect` has no
/// AI-only mode, so there is no floor-off variant.
#[tokio::test]
async fn the_stderr_warning_for_a_file_over_the_limit_reads_as_ruled() {
    let repo = repo_with_a_big_seed_file();
    let sources = vec![("acme/app".to_string(), repo.path().to_path_buf())];
    let (refusal, warnings) = camerata_server::onboard::scope::start_checks(
        &sources,
        &camerata_server::onboard::scope::ScanScope::none(),
    )
    .await;
    assert_eq!(refusal, None);
    let mut stderr = Vec::new();
    camerata::inspect_cmd::write_start_warnings(&mut stderr, &warnings);
    let stderr = String::from_utf8(stderr).unwrap();
    assert_eq!(
        stderr,
        "warning: 1 file will not be read for the inspection: app/seeds/big.js (1.1 MB). \
         It is still searched for secrets by pattern checks, never by the model, if it is text, \
         and checked by name only if not\n"
    );
}
