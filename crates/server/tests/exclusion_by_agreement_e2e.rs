#![allow(clippy::unwrap_used)]
//! END-TO-END (item 31): a file excluded by agreement is read by nothing.
//!
//! The fixture plants a secret-shaped key and an unreferenced file (a code defect the dead-code
//! rule reports) inside `seeds/`. The control run, with no exclusion, reports both, so the test
//! can tell. The run excluding `app/seeds/**` must carry neither in its findings, its report
//! text or its billed volume; Coverage names the file, and only by name, as excluded by
//! agreement; and `camerata count` bills exactly what the run billed.

use std::path::Path;

use camerata_server::ai_audit::ScanMode;
use camerata_server::onboard::count::count_repo;
use camerata_server::onboard::scope::ScanScope;
use camerata_server::onboard::{self, SelectedRule};

const RULE: &str = "ARCH-DEAD-CODE-1";
/// The planted secret-shaped value (the same shape the floor's own tests plant).
const PLANTED_TOKEN: &str = "ghp_0123456789012345678901234567890123456";
const SPEC: &str = "acme/app";

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

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "package.json", r#"{"private":true}"#);
    write(root, "src/app.ts", "import './wired';\n");
    write(root, "src/wired.ts", "export const wired = 1;\n");
    write(root, "src/orphan_widget.ts", "export const orphan = 1;\n");
    // The excluded file: a secret-shaped key and nothing imports it.
    write(
        root,
        "seeds/seed_data.ts",
        &format!("export const seedToken = \"{PLANTED_TOKEN}\";\nexport const rows = [1, 2];\n"),
    );
    git(root, &["init", "-q", "-b", "main"]);
    git(
        root,
        &["config", "user.email", "exclusion-e2e@example.test"],
    );
    git(root, &["config", "user.name", "Exclusion E2E"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "fixture"]);
    dir
}

async fn run(root: &Path, patterns: &[&str]) -> onboard::ScanReport {
    let (corpus, errors) =
        camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
    let patterns: Vec<String> = patterns.iter().map(|p| p.to_string()).collect();
    let controls = camerata_server::cost_cap::RunControls {
        scope: ScanScope::from_patterns(&patterns).unwrap(),
        ..Default::default()
    };
    let selected = vec![SelectedRule {
        id: RULE.to_string(),
        directive: "exclusion e2e".to_string(),
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
        controls,
    )
    .await;
    report
}

#[tokio::test]
async fn without_an_exclusion_the_planted_file_is_reported() {
    let dir = fixture();
    let report = run(dir.path(), &[]).await;
    assert!(
        report
            .findings
            .iter()
            .any(|f| f.path == "seeds/seed_data.ts"),
        "the control run must see the planted problem: {:?}",
        report.findings
    );
    assert!(report
        .findings
        .iter()
        .any(|f| f.path == "src/orphan_widget.ts" && f.rule_id == RULE));
}

#[tokio::test]
async fn an_excluded_file_appears_nowhere_but_its_name_in_coverage() {
    let dir = fixture();
    let report = run(dir.path(), &["app/seeds/**"]).await;

    // Findings, the secrets floor and the dead-code rule: nothing from the excluded folder.
    assert!(
        report
            .findings
            .iter()
            .all(|f| !f.path.starts_with("seeds/")),
        "{:?}",
        report.findings
    );
    // The rest of the repo is still inspected.
    assert!(report
        .findings
        .iter()
        .any(|f| f.path == "src/orphan_widget.ts" && f.rule_id == RULE));
    // Nothing of the file's content is in the report anywhere.
    let whole = serde_json::to_string(&report).unwrap();
    assert!(
        !whole.contains(PLANTED_TOKEN),
        "the planted key leaked into the report"
    );
    assert!(
        !whole.contains("seedToken"),
        "the excluded file's content leaked"
    );

    // Coverage lists it by name, with the reason, and the summary counts it.
    let gaps = camerata_server::report_export::coverage_gaps::coverage_gaps(&report);
    assert_eq!(gaps.skipped_files.len(), 1, "{:?}", gaps.skipped_files);
    let row = &gaps.skipped_files[0];
    assert_eq!(
        (row.repo.as_str(), row.path.as_str()),
        (SPEC, "seeds/seed_data.ts")
    );
    assert_eq!(row.reason, "excluded by agreement");
    assert_eq!(
        gaps.skipped_summary.as_deref(),
        Some(
            "1 file was not read for the inspection: 1 excluded by agreement. \
             1 file was not searched for secrets: 1 excluded by agreement."
        )
    );
}

#[tokio::test]
async fn the_count_bills_exactly_what_the_run_billed() {
    let dir = fixture();
    let scope = ScanScope::from_patterns(&["app/seeds/**".to_string()]).unwrap();
    let report = run(dir.path(), &["app/seeds/**"]).await;
    let count = count_repo(dir.path(), &scope.for_repo(SPEC)).unwrap();

    assert_eq!(
        count.total_lines, report.code_lines,
        "one list decides what is billed"
    );
    assert_eq!(count.total_files, report.files_scanned);
    assert_eq!(count.not_read.len(), 1);
    assert_eq!(count.not_read[0].reason, "excluded by agreement");
    assert_eq!(count.not_read[0].files[0].path, "seeds/seed_data.ts");

    // Without the exclusion both go up by the same amount, so the count is not just zero.
    let plain_report = run(dir.path(), &[]).await;
    let plain_count = count_repo(dir.path(), &ScanScope::none().for_repo(SPEC)).unwrap();
    assert_eq!(plain_count.total_lines, plain_report.code_lines);
    assert_eq!(plain_count.total_lines, count.total_lines + 2);
}
