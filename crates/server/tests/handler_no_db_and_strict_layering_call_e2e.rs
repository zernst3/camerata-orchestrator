//! END-TO-END: Pass 4c's two config-gated call-site AST checkers — the PRODUCTION
//! `ARCH-HANDLER-NO-DB-1` (`camerata_checks::handler_no_db_checker::HandlerNoDbChecker`) and
//! the `ARCH-STRICT-LAYERING-1` CALL facet
//! (`camerata_checks::strict_layering_call_checker::StrictLayeringCallChecker`) — driven
//! through the REAL deterministic scan entry point (`onboard::audit_repos`), mirroring
//! `import_boundary_checker_e2e.rs`'s convention for Pass 4b-2. See
//! `docs/design/2026-07-27_ast-extractor-layer.md` §4 Group D.
//!
//! ZERO API SPEND: `run_ai_review: false` throughout — no model call anywhere in this file.
//!
//! Two fixtures prove the D3 config-aware degradation END TO END:
//!
//! - `tests/fixtures/handler_no_db_repo/` — a real `.camerata/architecture.toml` (3 layers +
//!   `[db]`) with a Rust AND a TypeScript HANDLER file that each call a `db` handle directly
//!   (forbidden), a SERVICE that wraps a repository call in `db.transaction(...)` (the rule's
//!   own exemption — compliant), and a REPOSITORY that queries `db` directly (`allowed_in` —
//!   compliant). Both violating files must fire BOTH rule ids at `high` severity (config
//!   makes handler classification and the DB-handle marker list fully deterministic).
//! - `tests/fixtures/handler_no_db_unconfigured_repo/` — the IDENTICAL TS handler shape (same
//!   `listOrders`/`db.query` content) but with NO `.camerata/architecture.toml` at all: since
//!   the function name carries no handler-ish marker either, this repo gets ZERO findings for
//!   either rule id (not even a needs-review one) — proving the config is doing REAL
//!   classification work, not just upgrading severity.

use std::path::Path;

use camerata_checks::arch_checker::RepoView;
use camerata_server::ai_audit::ScanMode;
use camerata_server::onboard::{self, read_local_repo_files, ExtractedRepo, SelectedRule};

const RULE_HANDLER_NO_DB: &str = "ARCH-HANDLER-NO-DB-1";
const RULE_STRICT_LAYERING: &str = "ARCH-STRICT-LAYERING-1";
const RULE_NO_CROSS_BOUNDARY_IMPORTS: &str = "ARCH-NO-CROSS-BOUNDARY-IMPORTS-1";
const RULE_API_DTOS: &str = "ARCH-API-DTOS-1";

/// Copy `fixture_name` (under `tests/fixtures/`) into `dest`, then turn `dest` into a real
/// one-commit git repo — mirrors `import_boundary_checker_e2e.rs`'s own helper.
fn stage_fixture_as_git_repo(fixture_name: &str, dest: &Path) {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(fixture_name);
    copy_dir_recursive(&fixture_root, dest);

    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dest)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn git {args:?}: {e}"));
        assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "handler-no-db-e2e@camerata.local"]);
    g(&["config", "user.name", "Camerata Handler-No-DB E2E Test"]);
    g(&["add", "."]);
    let commit_msg = format!("e2e fixture: {fixture_name}");
    g(&["commit", "-q", "-m", &commit_msg]);
}

fn copy_dir_recursive(src: &Path, dest: &Path) {
    std::fs::create_dir_all(dest).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let ty = entry.file_type().unwrap();
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap_or_else(|e| panic!("failed to copy fixture file {from:?} -> {to:?}: {e}"));
        }
    }
}

fn selected_rules() -> Vec<SelectedRule> {
    [RULE_HANDLER_NO_DB, RULE_STRICT_LAYERING, RULE_NO_CROSS_BOUNDARY_IMPORTS, RULE_API_DTOS]
        .into_iter()
        .map(|id| SelectedRule {
            id: id.to_string(),
            directive: format!("handler-no-db / strict-layering-call e2e: {id}"),
            repos: Vec::new(),
        })
        .collect()
}

async fn run_fixture_scan(repo_dir: &Path, repo_spec: &str) -> onboard::ScanReport {
    let sources = vec![(repo_spec.to_string(), repo_dir.to_path_buf())];
    let selected = selected_rules();
    let (report, _manifest) = onboard::audit_repos(
        &sources,
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
        false, // run_ai_review — zero API spend
        true,  // run_deterministic
        None,
    )
    .await;
    report
}

#[tokio::test]
async fn configured_repo_fires_both_rule_ids_on_both_language_handlers() {
    let repo_spec = "e2e/handler-no-db-configured".to_string();
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo("handler_no_db_repo", repo_dir.path());

    let report = run_fixture_scan(repo_dir.path(), &repo_spec).await;
    assert!(!report.gated);
    assert_eq!(report.repos, vec![repo_spec.clone()]);

    for rule in [RULE_HANDLER_NO_DB, RULE_STRICT_LAYERING] {
        let hits: Vec<&onboard::Finding> = report.findings.iter().filter(|f| f.rule_id == rule).collect();
        assert_eq!(
            hits.len(),
            2,
            "{rule}: exactly one finding per language (Rust + TS handler calling db directly), \
             the compliant service (tx-wrapped) and repository (allowed_in) must stay silent: {:?}",
            report.findings
        );
        for hit in &hits {
            assert_eq!(hit.severity, "high", "{rule} deterministic mode must be high severity: {hit:?}");
            assert!(!hit.detail.contains("[needs review"), "{rule} must not be needs-review: {}", hit.detail);
            assert!(hit.preview, "an architectural finding uses the existing preview mechanism");
            assert_eq!(hit.preview_tool.as_deref(), Some("camerata-arch"));
        }
        assert!(hits.iter().any(|f| f.path == "src/routes/orders.rs"), "{rule}: missing Rust hit: {hits:#?}");
        assert!(hits.iter().any(|f| f.path == "src/routes/orders.ts"), "{rule}: missing TS hit: {hits:#?}");
    }

    // The compliant files must never appear under either rule id.
    for compliant_path in ["src/services/order_service.rs", "src/repositories/orders_repo.rs"] {
        assert!(
            report
                .findings
                .iter()
                .all(|f| !(f.path == compliant_path && (f.rule_id == RULE_HANDLER_NO_DB || f.rule_id == RULE_STRICT_LAYERING))),
            "compliant file {compliant_path} must never be flagged under either rule: {:?}",
            report.findings
        );
    }

    assert!(!report.provenance.audited_refs.is_empty());
    let sha = report.provenance.audited_refs[0].sha.as_deref().expect("a real one-commit git repo must yield a SHA");
    assert_eq!(sha.len(), 40);
}

#[tokio::test]
async fn unconfigured_repo_with_the_identical_handler_shape_gets_zero_findings() {
    let repo_spec = "e2e/handler-no-db-unconfigured".to_string();
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo("handler_no_db_unconfigured_repo", repo_dir.path());

    let report = run_fixture_scan(repo_dir.path(), &repo_spec).await;
    assert!(!report.gated);

    for rule in [RULE_HANDLER_NO_DB, RULE_STRICT_LAYERING] {
        assert!(
            report.findings.iter().all(|f| f.rule_id != rule),
            "no .camerata/architecture.toml AND no handler-ish name marker -> zero {rule} findings, \
             even though the identical `db.query(...)` shape is present: {:?}",
            report.findings
        );
    }
}

/// The D3 per-repo proof, mirroring `import_boundary_checker_e2e.rs`'s own test: build a
/// `RepoView` from each fixture's REAL scan-read files and assert `checker_rule_ids_for_repo`
/// diverges exactly as designed.
#[tokio::test]
async fn checker_rule_ids_for_repo_is_config_aware_per_repo_for_both_checkers() {
    let configured_dir = tempfile::tempdir().expect("tempdir");
    stage_fixture_as_git_repo("handler_no_db_repo", configured_dir.path());
    let unconfigured_dir = tempfile::tempdir().expect("tempdir");
    stage_fixture_as_git_repo("handler_no_db_unconfigured_repo", unconfigured_dir.path());

    let ExtractedRepo { files: configured_files, .. } =
        read_local_repo_files(configured_dir.path()).expect("read configured fixture");
    let ExtractedRepo { files: unconfigured_files, .. } =
        read_local_repo_files(unconfigured_dir.path()).expect("read unconfigured fixture");

    let configured_view = RepoView { spec: "e2e/configured", files: &configured_files };
    let unconfigured_view = RepoView { spec: "e2e/unconfigured", files: &unconfigured_files };

    let configured_ids = camerata_checks::arch_checker::checker_rule_ids_for_repo(&configured_view);
    let unconfigured_ids = camerata_checks::arch_checker::checker_rule_ids_for_repo(&unconfigured_view);

    for rule in [RULE_HANDLER_NO_DB, RULE_STRICT_LAYERING] {
        assert!(
            configured_ids.contains(rule),
            "{rule} must be excluded from the LLM prompt for the CONFIGURED repo: {configured_ids:?}"
        );
        assert!(
            !unconfigured_ids.contains(rule),
            "{rule} must stay LLM-advisory-eligible for the UNCONFIGURED repo: {unconfigured_ids:?}"
        );
    }
}
