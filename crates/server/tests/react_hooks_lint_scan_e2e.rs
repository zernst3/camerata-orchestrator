#![allow(clippy::unwrap_used)]
//! END-TO-END: `UI-REACT-HOOKS-LINT-1` through the real scan entry point (`onboard::audit_repos`,
//! deterministic-only, zero API spend). The unit tests in `camerata-checks` pin what the checker
//! reads; these pin the WIRING only the scan owns:
//!
//!   1. the scan's file feed carries the configuration shapes the checker reads (dotfiles such
//!      as `.eslintrc.json` included) and the corpus `max_severity` ceiling clamps the live
//!      finding to low;
//!   2. a repository the checker cannot answer for is reported as nothing AND is not credited as
//!      a verified-clean detector run;
//!   3. a scan that stopped at its file limit does not run the rule, since the configuration may
//!      be among the files it dropped.

use std::path::Path;

use camerata_server::ai_audit::ScanMode;
use camerata_server::onboard::{self, SelectedRule};

const RULE: &str = "UI-REACT-HOOKS-LINT-1";
const REACT_APP: &str =
    r#"{ "name": "web", "dependencies": { "react": "^18.2.0", "react-dom": "^18.2.0" } }"#;

fn selected() -> Vec<SelectedRule> {
    vec![SelectedRule {
        id: RULE.to_string(),
        directive: "react hooks lint e2e".to_string(),
        repos: Vec::new(), // project-level: applies to every scanned repo
    }]
}

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
        .unwrap_or_else(|e| panic!("failed to spawn git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn commit_all(root: &Path) {
    git(root, &["init", "-q", "-b", "main"]);
    git(
        root,
        &["config", "user.email", "hooks-lint-e2e@example.test"],
    );
    git(root, &["config", "user.name", "Hooks Lint E2E"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "fixture"]);
}

async fn scan(dir: &Path, spec: &str) -> onboard::ScanReport {
    let (corpus, errors) =
        camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
    let sources = vec![(spec.to_string(), dir.to_path_buf())];
    let (report, _manifest) = onboard::audit_repos(
        &sources,
        &selected(),
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
    )
    .await;
    report
}

fn hooks_findings(report: &onboard::ScanReport) -> Vec<&onboard::Finding> {
    report
        .findings
        .iter()
        .filter(|f| f.rule_id == RULE)
        .collect()
}

#[tokio::test]
async fn a_react_repo_whose_lint_config_lacks_the_hooks_rules_is_reported_at_low() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "package.json", REACT_APP);
    write(root, "src/App.jsx", "export const App = () => null;\n");
    // A dotfile: the scan feed must carry it for the checker to read it.
    write(
        root,
        ".eslintrc.json",
        r#"{ "extends": ["eslint:recommended", "plugin:react/recommended"] }"#,
    );
    commit_all(root);

    let report = scan(root, "e2e/hooks-lint-gap").await;
    let found = hooks_findings(&report);
    assert_eq!(found.len(), 1, "{:?}", report.findings);
    assert_eq!(found[0].path, ".eslintrc.json");
    assert_eq!(
        found[0].severity, "low",
        "the corpus `max_severity` ceiling clamps the live finding at scan time"
    );
    assert_eq!(
        found[0].preview_tool.as_deref(),
        Some(onboard::ARCH_PREVIEW_TOOL)
    );
    assert!(
        found[0].detail.contains("react-hooks/exhaustive-deps"),
        "{}",
        found[0].detail
    );

    let entry = report
        .ledger
        .rule(RULE)
        .expect("accounted for in the ledger");
    assert!(
        entry.ran,
        "a gap is a real answer and credits the detector run: {entry:?}"
    );
    assert_eq!(entry.findings_emitted, 1);
}

#[tokio::test]
async fn a_react_repo_with_no_lint_config_at_all_is_reported_on_the_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "package.json", REACT_APP);
    write(root, "src/App.jsx", "export const App = () => null;\n");
    commit_all(root);

    let report = scan(root, "e2e/hooks-lint-none").await;
    let found = hooks_findings(&report);
    assert_eq!(found.len(), 1, "{:?}", report.findings);
    assert_eq!(found[0].path, "package.json");
    assert!(
        found[0].detail.contains("no ESLint configuration"),
        "{}",
        found[0].detail
    );
}

#[tokio::test]
async fn a_repo_that_enables_the_rules_is_silent_and_credited_as_a_verified_answer() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "package.json", REACT_APP);
    write(root, "src/App.jsx", "export const App = () => null;\n");
    write(
        root,
        ".eslintrc.json",
        r#"{ "extends": ["next/core-web-vitals"] }"#,
    );
    commit_all(root);

    let report = scan(root, "e2e/hooks-lint-enabled").await;
    assert!(hooks_findings(&report).is_empty(), "{:?}", report.findings);
    let entry = report
        .ledger
        .rule(RULE)
        .expect("accounted for in the ledger");
    assert!(
        entry.ran && entry.findings_emitted == 0,
        "an enabling configuration is a verified-clean answer: {entry:?}"
    );
}

#[tokio::test]
async fn a_flat_config_is_silent_and_is_not_credited_as_a_clean_detector_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "package.json", REACT_APP);
    write(root, "src/App.jsx", "export const App = () => null;\n");
    write(root, "eslint.config.js", "export default [];\n");
    commit_all(root);

    let report = scan(root, "e2e/hooks-lint-flat").await;
    assert!(hooks_findings(&report).is_empty(), "{:?}", report.findings);
    let credited_clean = report
        .ledger
        .rule(RULE)
        .is_some_and(|e| e.ran && e.findings_emitted == 0 && e.coverage_note.is_none());
    assert!(
        !credited_clean,
        "a configuration the checker cannot read is not a verified-clean run: {:?}",
        report.ledger.rule(RULE)
    );
}

#[tokio::test]
async fn a_react_dependency_with_no_react_code_is_silent_and_not_credited_as_clean() {
    // A plugin that lists react as a peer dependency: no component, no import of React.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "package.json", REACT_APP);
    write(root, "src/index.ts", "export const plugin = () => 1;\n");
    commit_all(root);

    let report = scan(root, "e2e/hooks-lint-no-react-code").await;
    assert!(hooks_findings(&report).is_empty(), "{:?}", report.findings);
    let credited_clean = report
        .ledger
        .rule(RULE)
        .is_some_and(|e| e.ran && e.findings_emitted == 0 && e.coverage_note.is_none());
    assert!(
        !credited_clean,
        "not assessed is not verified clean: {:?}",
        report.ledger.rule(RULE)
    );
}

#[tokio::test]
async fn a_repo_that_is_not_a_react_repo_is_silent() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "package.json",
        r#"{ "name": "api", "dependencies": { "express": "^4.19.0" } }"#,
    );
    write(root, "src/server.js", "module.exports = {};\n");
    commit_all(root);

    let report = scan(root, "e2e/hooks-lint-not-react").await;
    assert!(hooks_findings(&report).is_empty(), "{:?}", report.findings);
}

/// 20,000 files is `HARD_CAP_FILES`: the scan stops there and says so. With the lint
/// configuration possibly among the dropped files, "no configuration enables the rules" cannot
/// be said, so the rule is withheld.
#[tokio::test]
async fn a_scan_that_hit_its_file_limit_does_not_run_the_hooks_lint_rule() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "package.json", REACT_APP);
    for i in 0..20_000 {
        write(
            root,
            &format!("src/bulk/m{i}_part.ts"),
            "export const v = 1;\n",
        );
    }
    let report = scan(root, "e2e/hooks-lint-truncated").await;

    assert!(
        hooks_findings(&report).is_empty(),
        "no gap is reported on a truncated scan: {}",
        hooks_findings(&report).len()
    );
    let note = report
        .ledger
        .rule(RULE)
        .and_then(|e| e.coverage_note.clone())
        .expect("the withheld rule carries a coverage note");
    assert_eq!(note, camerata_server::scan_ledger::INCOMPLETE_SCAN_NOTE);
}
