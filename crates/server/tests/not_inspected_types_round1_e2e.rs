#![allow(clippy::unwrap_used)]
//! Round 1 of the not-read listing (GitHub #338): a file the engine opens for its declared
//! runtime or package versions is not called plainly "not read"; more hand-written types are
//! listed; the count line tallies the not-read files by type and caps their paths; `count --json`
//! carries an operator-only tally of types nothing claims; a tracked file missing from disk is
//! not listed; Coverage says the files are not billed.

use std::path::Path;

use camerata_server::ai_audit::ScanMode;
use camerata_server::onboard::count::{count_repo, RepoCount};
use camerata_server::onboard::scope::ScanScope;
use camerata_server::onboard::{self, SelectedRule};

const SPEC: &str = "acme/app";
const PLAIN: &str = "this file type is not inspected yet";
const VERSIONS: &str = "this file type is not inspected yet; only its declared runtime and \
                        package versions are checked";

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

/// A committed repo of `files` (everything added with `-f`, so a gitignored file is tracked).
fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (rel, body) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.email", "round1@example.test"]);
    git(root, &["config", "user.name", "Round One"]);
    git(root, &["add", "-f", "."]);
    git(root, &["commit", "-q", "-m", "fixture"]);
    dir
}

fn count(root: &Path) -> RepoCount {
    count_repo(root, &ScanScope::none().for_repo(SPEC)).unwrap()
}

fn group_paths(count: &RepoCount, reason: &str) -> Vec<String> {
    count
        .not_read
        .iter()
        .find(|g| g.reason == reason)
        .map(|g| g.files.iter().map(|f| f.path.clone()).collect())
        .unwrap_or_default()
}

async fn run(root: &Path) -> onboard::ScanReport {
    let (corpus, errors) =
        camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    assert!(errors.is_empty(), "{errors:?}");
    let selected = vec![SelectedRule {
        id: "ARCH-DEAD-CODE-1".to_string(),
        directive: "round 1".to_string(),
        repos: Vec::new(),
    }];
    let (report, _manifest) = onboard::audit_repos_controlled(
        &camerata_server::credentials::MemoryCredentialStore::new(),
        &[(SPEC.to_string(), root.to_path_buf())],
        &selected,
        Vec::new(),
        None,
        None,
        ScanMode::Sequential,
        false,
        None,
        None,
        None,
        false,
        false,
        false,
        true,
        None,
        camerata_server::llm::BackendResolution::Api,
        Some(&corpus),
        &std::collections::HashMap::new(),
        camerata_server::cost_cap::RunControls::default(),
    )
    .await;
    report
}

const CSPROJ: &str =
    "<Project>\n  <PropertyGroup>\n    <TargetFramework>net6.0</TargetFramework>\n  </PropertyGroup>\n</Project>\n";

fn versions_fixture() -> tempfile::TempDir {
    repo(&[
        ("package.json", r#"{"private":true}"#),
        ("src/a.ts", "export const a = 1;\n"),
        ("src/App.csproj", CSPROJ),
        ("Gemfile", "source 'https://rubygems.org'\n"),
        ("Views/Index.cshtml", "<h1>Hi</h1>\n"),
    ])
}

#[test]
fn files_the_engine_opens_for_versions_get_their_own_label_in_the_count() {
    let dir = versions_fixture();
    let c = count(dir.path());
    assert_eq!(group_paths(&c, VERSIONS), ["Gemfile", "src/App.csproj"]);
    assert_eq!(group_paths(&c, PLAIN), ["Views/Index.cshtml"]);
}

#[tokio::test]
async fn the_start_warning_and_preflight_never_say_not_read_of_a_versions_file() {
    let dir = versions_fixture();
    let sources = vec![(SPEC.to_string(), dir.path().to_path_buf())];
    let (_none, warnings) =
        camerata_server::onboard::scope::start_checks(&sources, &ScanScope::none()).await;
    let line = warnings
        .iter()
        .find(|w| w.contains("declared runtime and package versions"))
        .unwrap_or_else(|| panic!("no warning for the versions files: {warnings:?}"));
    assert!(line.contains("app/src/App.csproj"), "{line}");
    assert!(!line.contains("will not be read"), "{line}");
    assert!(!line.contains("Index.cshtml"), "{line}");

    let facts =
        camerata_server::preflight::gather_scan_facts(&sources, &[], None, &ScanScope::none())
            .await;
    let rows = camerata_server::preflight::repo_sizes(&[SPEC.to_string()], &facts, &[]);
    let row = serde_json::to_value(&rows[0]).unwrap();
    assert_eq!(
        row["not_inspected_versions_only_paths"],
        serde_json::json!(["app/Gemfile", "app/src/App.csproj"]),
        "{row}"
    );
    assert_eq!(
        row["not_inspected_paths"],
        serde_json::json!(["app/Views/Index.cshtml"]),
        "{row}"
    );
}

#[tokio::test]
async fn coverage_never_calls_a_versions_file_plainly_not_read() {
    let dir = versions_fixture();
    let report = run(dir.path()).await;
    let gaps = camerata_server::report_export::coverage_gaps::coverage_gaps(&report);
    let reason_of = |path: &str| {
        gaps.skipped_files
            .iter()
            .find(|r| r.path == path)
            .unwrap_or_else(|| panic!("{path} not in {:?}", gaps.skipped_files))
            .reason
            .clone()
    };
    assert_eq!(reason_of("src/App.csproj"), VERSIONS);
    assert_eq!(reason_of("Gemfile"), VERSIONS);
    assert_eq!(reason_of("Views/Index.cshtml"), PLAIN);
    let summary = gaps.skipped_summary.unwrap();
    assert!(summary.contains("(not billed)"), "{summary}");
}

/// The review's M2 types, each a plain not-read file; `Pipfile` is read by the dependency check,
/// so it carries the versions label.
#[test]
fn more_hand_written_types_are_listed_as_not_read() {
    let names = [
        "a.cxx",
        "a.hh",
        "a.hxx",
        "a.aspx",
        "a.ascx",
        "a.master",
        "a.xaml",
        "a.resx",
        "a.jsp",
        "a.handlebars",
        "a.cmd",
        "a.phtml",
    ];
    let mut files: Vec<(&str, &str)> = vec![("package.json", r#"{"private":true}"#)];
    files.extend(names.iter().map(|n| (*n, "x\n")));
    files.push(("Pipfile", "[packages]\n"));
    let dir = repo(&files);
    let c = count(dir.path());
    let mut want: Vec<String> = names.iter().map(|n| (*n).to_string()).collect();
    want.sort();
    assert_eq!(group_paths(&c, PLAIN), want);
    assert_eq!(group_paths(&c, VERSIONS), ["Pipfile"]);
}

#[test]
fn the_count_line_tallies_by_type_and_caps_the_paths() {
    let mut files: Vec<(String, &str)> = (0..12)
        .map(|i| (format!("Views/v{i:02}.cshtml"), "x\n"))
        .collect();
    for i in 0..3 {
        files.push((format!("s/q{i}.graphql"), "x\n"));
    }
    files.push(("package.json".to_string(), r#"{"private":true}"#));
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, b)| (p.as_str(), *b)).collect();
    let dir = repo(&refs);
    let text = count(dir.path()).to_text();
    let line = text
        .lines()
        .find(|l| l.starts_with("not read, this file type is not inspected yet"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(
        line.starts_with(
            "not read, this file type is not inspected yet (15 files, by type: 12 .cshtml, \
             3 .graphql): "
        ),
        "{line}"
    );
    assert!(line.ends_with("; and 5 more"), "{line}");
}

#[test]
fn count_json_carries_the_not_read_group_and_an_operator_only_tally_of_unclaimed_types() {
    let dir = repo(&[
        ("package.json", r#"{"private":true}"#),
        ("src/a.ts", "export const a = 1;\n"),
        ("Views/Index.cshtml", "<h1>Hi</h1>\n"),
        ("notes/a.xyz", "x\n"),
        ("notes/b.xyz", "x\n"),
        ("assets/logo.png", "x\n"),
        ("node_modules/dep/c.zzz", "x\n"),
    ]);
    let json = serde_json::to_value(count(dir.path())).unwrap();
    assert_eq!(json["not_read"][0]["reason"], PLAIN, "{json}");
    assert_eq!(
        json["not_read"][0]["files"][0]["path"], "Views/Index.cshtml",
        "{json}"
    );
    assert_eq!(
        json["unclassified_types"],
        serde_json::json!([
            {"extension": ".xyz", "files": 2},
            {"extension": ".png", "files": 1}
        ]),
        "{json}"
    );
}

#[test]
fn a_tracked_file_missing_from_disk_is_not_listed() {
    let dir = repo(&[
        ("package.json", r#"{"private":true}"#),
        ("old/schema.graphql", "type Q { a: Int }\n"),
        ("Views/Index.cshtml", "<h1>Hi</h1>\n"),
    ]);
    std::fs::remove_file(dir.path().join("old/schema.graphql")).unwrap();
    let c = count(dir.path());
    assert_eq!(group_paths(&c, PLAIN), ["Views/Index.cshtml"]);
}

#[test]
fn a_tracked_and_gitignored_listed_type_is_listed() {
    let dir = repo(&[
        ("package.json", r#"{"private":true}"#),
        (".gitignore", "Views/\n"),
        ("Views/Index.cshtml", "<h1>Hi</h1>\n"),
    ]);
    let c = count(dir.path());
    assert_eq!(group_paths(&c, PLAIN), ["Views/Index.cshtml"]);
}
