#![allow(clippy::unwrap_used)]
//! END-TO-END (GitHub #338, "list now, inspect later"): a hand-written file of a type the
//! inspection does not read yet is never silent. A `.graphql` schema and a `.cshtml` Razor view
//! beside normal code are listed as not read, with the reason "this file type is not inspected
//! yet", in `camerata count`, in the pre-start check (the command line warning and the preflight
//! wire the app reads) and in Coverage. Neither is billed: the billed total is the total of the
//! same repo without them. A file of such a type inside a noise folder stays noise and is listed
//! nowhere.

use std::path::Path;

use camerata_server::ai_audit::ScanMode;
use camerata_server::onboard::count::count_repo;
use camerata_server::onboard::scope::ScanScope;
use camerata_server::onboard::{self, SelectedRule};

const RULE: &str = "ARCH-DEAD-CODE-1";
const SPEC: &str = "acme/app";
const REASON: &str = "this file type is not inspected yet";
/// The two hand-written files of types not inspected yet, in path order.
const UNREAD: [&str; 2] = ["Views/Home/Index.cshtml", "schema.graphql"];
/// A file of such a type inside a noise folder (a dependency): noise, never listed.
const NOISE_SCHEMA: &str = "node_modules/dep/schema.graphql";

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

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

/// Normal code and a dependency schema, plus (when `with_unread`) the two files of types not
/// inspected yet. Without them it is the control the billed total is compared against.
fn fixture(with_unread: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "package.json", r#"{"private":true}"#);
    write(root, "src/app.ts", "import './wired';\n");
    write(root, "src/wired.ts", "export const wired = 1;\n");
    write(root, NOISE_SCHEMA, "type Dep {\n  id: ID\n}\n");
    if with_unread {
        write(root, "schema.graphql", "type Query {\n  hello: String\n}\n");
        write(
            root,
            "Views/Home/Index.cshtml",
            "@model HomeModel\n<h1>Hello</h1>\n",
        );
    }
    git(root, &["init", "-q", "-b", "main"]);
    git(
        root,
        &["config", "user.email", "not-inspected-e2e@example.test"],
    );
    git(root, &["config", "user.name", "Not Inspected E2E"]);
    git(root, &["add", "-f", "."]);
    git(root, &["commit", "-q", "-m", "fixture"]);
    dir
}

async fn run(root: &Path) -> onboard::ScanReport {
    let (corpus, errors) =
        camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
    let selected = vec![SelectedRule {
        id: RULE.to_string(),
        directive: "not inspected e2e".to_string(),
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
        false, // run_ai_review: zero API spend
        true,  // run_deterministic: the floor and the architectural engine
        None,
        camerata_server::llm::BackendResolution::Api,
        Some(&corpus),
        &std::collections::HashMap::new(),
        camerata_server::cost_cap::RunControls::default(),
    )
    .await;
    report
}

#[test]
fn the_count_lists_both_files_as_not_read_and_bills_neither() {
    let dir = fixture(true);
    let control = fixture(false);
    let count = count_repo(dir.path(), &ScanScope::none().for_repo(SPEC)).unwrap();
    let plain = count_repo(control.path(), &ScanScope::none().for_repo(SPEC)).unwrap();

    // Not billed: the billed total is the total without them.
    assert_eq!(
        (count.total_files, count.total_lines),
        (plain.total_files, plain.total_lines)
    );
    // The noise filter is unchanged by them.
    assert_eq!(count.excluded_noise_files, plain.excluded_noise_files);

    let group = count
        .not_read
        .iter()
        .find(|g| g.reason == REASON)
        .unwrap_or_else(|| panic!("no not-read group for the type: {:?}", count.not_read));
    let paths: Vec<&str> = group.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, UNREAD);
    assert!(
        plain.not_read.iter().all(|g| g.reason != REASON),
        "{:?}",
        plain.not_read
    );

    let text = count.to_text();
    assert!(
        text.contains(
            "not read, this file type is not inspected yet (2 files, by type: 1 .cshtml, \
             1 .graphql): \
             Views/Home/Index.cshtml; schema.graphql\n"
        ),
        "{text}"
    );
    // A separate line, never inside the billed TOTAL row.
    let total_row = text.lines().find(|l| l.starts_with("TOTAL")).unwrap();
    assert!(!total_row.contains("graphql"), "{total_row}");
    assert!(
        !text.contains("node_modules"),
        "noise is not listed: {text}"
    );
}

#[tokio::test]
async fn the_pre_start_check_lists_both_files_with_the_reason() {
    let dir = fixture(true);
    let sources = vec![(SPEC.to_string(), dir.path().to_path_buf())];

    // The command line's warning.
    let (refusal, warnings) =
        camerata_server::onboard::scope::start_checks(&sources, &ScanScope::none()).await;
    assert_eq!(
        refusal, None,
        "a type not inspected yet never holds a start"
    );
    let line = warnings
        .iter()
        .find(|w| w.contains(REASON))
        .unwrap_or_else(|| panic!("no warning names the type: {warnings:?}"));
    assert_eq!(
        line,
        "2 files will not be read for the inspection, billed or searched for secrets: this \
         file type is not inspected yet. Not inspected yet: app/Views/Home/Index.cshtml; \
         app/schema.graphql."
    );
    assert!(
        warnings.iter().all(|w| !w.contains("node_modules")),
        "noise is not listed: {warnings:?}"
    );

    // The preflight wire the app's pre-start check is drawn from.
    let facts =
        camerata_server::preflight::gather_scan_facts(&sources, &[], None, &ScanScope::none())
            .await;
    let rows = camerata_server::preflight::repo_sizes(&[SPEC.to_string()], &facts, &[]);
    let row = serde_json::to_value(&rows[0]).unwrap();
    assert_eq!(
        row["not_inspected_paths"],
        serde_json::json!(["app/Views/Home/Index.cshtml", "app/schema.graphql"]),
        "{row}"
    );
    // Not billed: the estimate counts the code files alone.
    assert_eq!(row["files"], serde_json::json!(3), "{row}");
}

#[tokio::test]
async fn coverage_lists_both_files_as_not_read_and_the_billed_volume_is_unchanged() {
    let dir = fixture(true);
    let control = fixture(false);
    let report = run(dir.path()).await;
    let plain = run(control.path()).await;

    // Not billed: the run reads and counts exactly what it reads without them.
    assert_eq!(
        (report.files_scanned, report.code_lines),
        (plain.files_scanned, plain.code_lines)
    );

    let gaps = camerata_server::report_export::coverage_gaps::coverage_gaps(&report);
    let rows: Vec<(&str, &str, &str)> = gaps
        .skipped_files
        .iter()
        .map(|r| (r.repo.as_str(), r.path.as_str(), r.reason.as_str()))
        .collect();
    assert_eq!(
        rows,
        [(SPEC, UNREAD[0], REASON), (SPEC, UNREAD[1], REASON)],
        "{:?}",
        gaps.skipped_files
    );
    assert_eq!(
        gaps.skipped_summary.as_deref(),
        Some(
            "2 files were not read for the inspection: 2 of a file type not inspected yet \
             (not billed). 2 files were not searched for secrets: 2 of a file type not inspected yet."
        )
    );
    // The noise schema is in no Coverage row.
    assert!(gaps.skipped_files.iter().all(|r| r.path != NOISE_SCHEMA));
    // The control has nothing to list.
    let plain_gaps = camerata_server::report_export::coverage_gaps::coverage_gaps(&plain);
    assert!(
        plain_gaps.skipped_files.is_empty(),
        "{:?}",
        plain_gaps.skipped_files
    );
}
