#![allow(clippy::unwrap_used)]
//! END-TO-END: `ARCH-DEAD-CODE-1` through the real scan entry point (`onboard::audit_repos`,
//! deterministic-only, zero API spend). The unit tests in `camerata-checks` pin the algorithm;
//! these pin the WIRING that only the scan owns:
//!
//!   1. the reference-text tier reaches the architectural tier (a component that imports a
//!      composable keeps it alive) and reaches nothing else (a secret-shaped literal in a README
//!      is not a floor finding);
//!   2. a scan that stopped at its file limit does not run the rule, and the ledger does not
//!      credit it as a clean detector run.

use std::path::Path;

use camerata_server::ai_audit::ScanMode;
use camerata_server::onboard::{self, SelectedRule};

const RULE: &str = "ARCH-DEAD-CODE-1";

fn selected() -> Vec<SelectedRule> {
    vec![SelectedRule {
        id: RULE.to_string(),
        directive: "dead-code e2e".to_string(),
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

#[tokio::test]
async fn scan_reports_the_unreferenced_file_and_lets_reference_text_keep_files_alive() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "package.json", r#"{"private":true}"#);
    write(root, "src/app.ts", "import './wired';\n");
    write(root, "src/wired.ts", "export const wired = 1;\n");
    write(root, "src/orphan_widget.ts", "export const orphan = 1;\n");
    // Only a single-file component imports this composable: invisible to the import graph,
    // visible through the reference-text tier.
    write(
        root,
        "src/composables/useThing.ts",
        "export const useThing = 1;\n",
    );
    write(
        root,
        "src/App.vue",
        "<script setup>\nimport { useThing } from './composables/useThing'\n</script>\n",
    );
    // A secret-shaped literal in prose. Reference text is not secrets-floor input.
    let readme = concat!(
        "Setup notes.\nAPI_KEY=\"Zq9Xk3Pm7Rt2",
        "Vw5Yb8Nc4Jd6Hf1Ga0\"\n"
    );
    write(root, "README.md", readme);
    git(root, &["init", "-q", "-b", "main"]);
    git(
        root,
        &["config", "user.email", "dead-code-e2e@example.test"],
    );
    git(root, &["config", "user.name", "Dead Code E2E"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "fixture"]);

    let report = scan(root, "e2e/dead-code").await;

    let dead: Vec<&onboard::Finding> = report
        .findings
        .iter()
        .filter(|f| f.rule_id == RULE)
        .collect();
    assert_eq!(dead.len(), 1, "{:?}", report.findings);
    assert_eq!(dead[0].path, "src/orphan_widget.ts");
    assert_eq!(
        dead[0].severity, "medium",
        "the corpus ceiling clamps it at export"
    );
    assert!(dead[0].preview_tool.as_deref() == Some(onboard::ARCH_PREVIEW_TOOL));
    assert!(
        report.findings.iter().all(|f| f.path != "README.md"),
        "reference text is not secrets-floor input: {:?}",
        report.findings
    );
    assert!(
        report
            .findings
            .iter()
            .all(|f| f.path != "src/composables/useThing.ts"),
        "the component's import keeps the composable alive: {:?}",
        report.findings
    );
    assert_eq!(
        report.files_scanned, 5,
        "package.json, app.ts, wired.ts, orphan_widget.ts, useThing.ts: reference text is not \
         counted as scanned code"
    );

    let entry = report
        .ledger
        .rule(RULE)
        .expect("the rule is accounted for in the ledger");
    assert!(
        entry.ran,
        "a complete scan credits the detector run: {entry:?}"
    );
    assert_eq!(entry.findings_emitted, 1);
}

/// 20,000 files is `HARD_CAP_FILES`: the scan stops there and says so. With an importer
/// possibly among the dropped files, every file looks unreachable, so the rule is withheld.
#[tokio::test]
async fn a_scan_that_hit_its_file_limit_does_not_run_the_dead_code_rule() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "package.json", r#"{"private":true}"#);
    for i in 0..20_000 {
        write(
            root,
            &format!("src/bulk/m{i}_part.ts"),
            "export const v = 1;\n",
        );
    }
    let report = scan(root, "e2e/dead-code-truncated").await;

    assert!(
        report.findings.iter().all(|f| f.rule_id != RULE),
        "no dead-code finding on a truncated scan, where every file looks unreachable: {}",
        report.findings.iter().filter(|f| f.rule_id == RULE).count()
    );
    // The ledger must not read it as a clean run.
    let credited_clean = report
        .ledger
        .rule(RULE)
        .is_some_and(|e| e.ran && e.findings_emitted == 0 && e.coverage_note.is_none());
    assert!(
        !credited_clean,
        "a withheld rule must not be credited as a verified-clean detector run: {:?}",
        report.ledger.rule(RULE)
    );
}

/// Codegen and module-variant importers reach the checker through the scan feed: TanStack's
/// generated route tree and an `.mjs` module are the ONLY importers of the files below.
#[tokio::test]
async fn generated_and_module_variant_importers_keep_their_targets_alive() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "package.json", r#"{"private":true}"#);
    write(
        root,
        "src/main.tsx",
        "import { routeTree } from './routeTree.gen';\n",
    );
    write(
        root,
        "src/routeTree.gen.ts",
        "import { Route as About } from './routes/about'\n",
    );
    write(root, "src/routes/about.tsx", "export const Route = 1;\n");
    write(
        root,
        "src/db/index.mjs",
        "import { rows } from './seedRows.ts';\n",
    );
    write(root, "src/db/seedRows.ts", "export const rows = [];\n");
    write(root, "src/stranded_screen.tsx", "export const Route = 1;\n");
    git(root, &["init", "-q", "-b", "main"]);
    git(
        root,
        &["config", "user.email", "dead-code-e2e@example.test"],
    );
    git(root, &["config", "user.name", "Dead Code E2E"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "fixture"]);

    let report = scan(root, "e2e/dead-code-generated").await;
    let dead: Vec<&str> = report
        .findings
        .iter()
        .filter(|f| f.rule_id == RULE)
        .map(|f| f.path.as_str())
        .collect();
    assert_eq!(dead, ["src/stranded_screen.tsx"], "{:?}", report.findings);
}
