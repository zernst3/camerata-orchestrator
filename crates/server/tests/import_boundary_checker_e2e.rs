//! END-TO-END: Pass 4b-2's `ImportBoundaryChecker`, driven through the REAL deterministic
//! scan entry point (`onboard::audit_repos`) exactly like `group_a_architectural_checkers_e2e.rs`
//! proves the Pass 4a checkers. See `docs/design/2026-07-27_ast-extractor-layer.md` §4 Group C
//! and the "Pass 4b-2 landed" note.
//!
//! ZERO API SPEND: `run_ai_review: false` throughout — no model call anywhere in this file.
//!
//! Two fixtures prove the D3 config-aware degradation END TO END, in the SAME scan:
//!
//! - `tests/fixtures/import_boundary_repo/` — a real `.camerata/architecture.toml` (4 layers:
//!   handlers/services/repositories/domain) PLUS both a Rust (`src/routes/orders.rs`) and a
//!   TypeScript (`src/routes/orders.ts`) handler file that each import the repositories layer
//!   directly (forbidden — handlers may only import services/domain). The services and
//!   repositories layers import compliantly (repositories -> domain, services ->
//!   repositories/domain), proving the compliant edges stay silent in the SAME repo the
//!   violation fires in.
//! - `tests/fixtures/import_boundary_unconfigured_repo/` — the IDENTICAL violation shape
//!   (a TS handler importing a repository directly) but with NO `.camerata/architecture.toml`
//!   at all — this repo must get ZERO deterministic findings (D3 abstain), and its rule ids
//!   must stay LLM-advisory-eligible for THIS repo specifically (proving the PER-REPO,
//!   config-aware nature of `checker_rule_ids_for_repo`, not just a static exclusion set).

use std::path::Path;

use camerata_checks::arch_checker::RepoView;
use camerata_server::ai_audit::ScanMode;
use camerata_server::onboard::{self, read_local_repo_files, ExtractedRepo, SelectedRule};

const RULE_NO_CROSS_BOUNDARY_IMPORTS: &str = "ARCH-NO-CROSS-BOUNDARY-IMPORTS-1";
const RULE_API_DTOS: &str = "ARCH-API-DTOS-1";
const RULE_STRICT_LAYERING: &str = "ARCH-STRICT-LAYERING-1";

/// Copy `fixture_name` (under `tests/fixtures/`) into `dest`, then turn `dest` into a real
/// one-commit git repo so `onboard::capture_audited_ref`'s shell-outs succeed — mirrors
/// `group_a_architectural_checkers_e2e.rs::stage_fixture_as_git_repo`.
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
    g(&["config", "user.email", "import-boundary-e2e@camerata.local"]);
    g(&["config", "user.name", "Camerata Import-Boundary E2E Test"]);
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

fn selected_import_boundary_rules() -> Vec<SelectedRule> {
    [RULE_NO_CROSS_BOUNDARY_IMPORTS, RULE_API_DTOS, RULE_STRICT_LAYERING]
        .into_iter()
        .map(|id| SelectedRule {
            id: id.to_string(),
            directive: format!("import-boundary e2e: {id}"),
            repos: Vec::new(), // project-level: applies to every scanned repo
        })
        .collect()
}

async fn run_fixture_scan(repo_dir: &Path, repo_spec: &str) -> onboard::ScanReport {
    let sources = vec![(repo_spec.to_string(), repo_dir.to_path_buf())];
    let selected = selected_import_boundary_rules();
    let (report, _manifest) = onboard::audit_repos(
        &sources,
        &selected,
        Vec::new(), // extra_notes
        None,       // model — unused, run_ai_review is false
        None,       // calibration_model — unused
        ScanMode::Sequential,
        false, // thorough — unused (no AI review)
        None,  // feedback
        None,  // job
        None,  // incremental_prior — full scan
        false, // deep
        false, // soc2_enabled
        false, // run_ai_review — zero API spend
        true,  // run_deterministic — the floor AND the architectural engine both run
        None,  // ledger
    )
    .await;
    report
}

#[tokio::test]
async fn configured_repo_fires_cross_boundary_violation_for_both_rust_and_ts_handlers() {
    let repo_spec = "e2e/import-boundary-configured".to_string();
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo("import_boundary_repo", repo_dir.path());

    let report = run_fixture_scan(repo_dir.path(), &repo_spec).await;

    assert!(!report.gated, "a local-dir scan is never gated on a GitHub token");
    assert_eq!(report.repos, vec![repo_spec.clone()]);

    let hits: Vec<&onboard::Finding> =
        report.findings.iter().filter(|f| f.rule_id == RULE_NO_CROSS_BOUNDARY_IMPORTS).collect();
    assert_eq!(
        hits.len(),
        2,
        "exactly one violation per language (the Rust AND the TS handler importing repositories \
         directly) — the compliant services/repositories edges must stay silent: {:?}",
        report.findings
    );

    let rust_hit = hits
        .iter()
        .find(|f| f.path == "src/routes/orders.rs")
        .unwrap_or_else(|| panic!("no Rust finding: {hits:#?}"));
    assert_eq!(rust_hit.line, 1, "must point at the `use` establishing the forbidden import");
    assert!(rust_hit.detail.contains("handlers"), "{}", rust_hit.detail);
    assert!(rust_hit.detail.contains("repositories"), "{}", rust_hit.detail);
    assert!(rust_hit.preview, "an architectural finding uses the existing preview mechanism");
    assert_eq!(rust_hit.preview_tool.as_deref(), Some("camerata-arch"));

    let ts_hit = hits
        .iter()
        .find(|f| f.path == "src/routes/orders.ts")
        .unwrap_or_else(|| panic!("no TS finding: {hits:#?}"));
    assert_eq!(ts_hit.line, 1, "must point at the import establishing the forbidden edge");
    assert!(ts_hit.detail.contains("handlers"), "{}", ts_hit.detail);
    assert!(ts_hit.detail.contains("repositories"), "{}", ts_hit.detail);

    // No finding is EVER attributed to the compliant services/repositories/domain files.
    for compliant_path in [
        "src/services/order_service.rs",
        "src/services/order_service.ts",
        "src/repositories/orders_repo.rs",
        "src/repositories/orders_repo.ts",
        "src/domain/order.rs",
        "src/domain/order.ts",
    ] {
        assert!(
            report.findings.iter().all(|f| f.path != compliant_path),
            "compliant file {compliant_path} must never be flagged: {:?}",
            report.findings
        );
    }

    assert!(!report.provenance.audited_refs.is_empty());
    let sha = report.provenance.audited_refs[0].sha.as_deref().expect("a real one-commit git repo must yield a SHA");
    assert_eq!(sha.len(), 40);
}

#[tokio::test]
async fn unconfigured_repo_abstains_entirely_scan_side() {
    let repo_spec = "e2e/import-boundary-unconfigured".to_string();
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo("import_boundary_unconfigured_repo", repo_dir.path());

    let report = run_fixture_scan(repo_dir.path(), &repo_spec).await;

    assert!(!report.gated);
    for rule in [RULE_NO_CROSS_BOUNDARY_IMPORTS, RULE_API_DTOS, RULE_STRICT_LAYERING] {
        assert!(
            report.findings.iter().all(|f| f.rule_id != rule),
            "no `.camerata/architecture.toml` -> zero deterministic {rule} findings, even though \
             the SAME handler-importing-repository shape is present: {:?}",
            report.findings
        );
    }
}

/// The D3 per-repo proof: build a `RepoView` from each fixture's REAL scan-read files (via
/// `read_local_repo_files`, the same function `audit_repos` calls internally) and assert
/// `checker_rule_ids_for_repo` diverges exactly as designed — configured excludes all three
/// rule ids from the LLM prompt; unconfigured leaves all three eligible. This is the
/// "config-aware, not a static set" contract Pass 4b-1 built and this checker is the first to
/// exercise for real.
#[tokio::test]
async fn checker_rule_ids_for_repo_is_config_aware_per_repo_not_a_static_set() {
    let configured_dir = tempfile::tempdir().expect("tempdir");
    stage_fixture_as_git_repo("import_boundary_repo", configured_dir.path());
    let unconfigured_dir = tempfile::tempdir().expect("tempdir");
    stage_fixture_as_git_repo("import_boundary_unconfigured_repo", unconfigured_dir.path());

    let ExtractedRepo { files: configured_files, .. } =
        read_local_repo_files(configured_dir.path()).expect("read configured fixture");
    let ExtractedRepo { files: unconfigured_files, .. } =
        read_local_repo_files(unconfigured_dir.path()).expect("read unconfigured fixture");

    let configured_view = RepoView { spec: "e2e/configured", files: &configured_files };
    let unconfigured_view = RepoView { spec: "e2e/unconfigured", files: &unconfigured_files };

    let configured_ids = camerata_checks::arch_checker::checker_rule_ids_for_repo(&configured_view);
    let unconfigured_ids = camerata_checks::arch_checker::checker_rule_ids_for_repo(&unconfigured_view);

    for rule in [RULE_NO_CROSS_BOUNDARY_IMPORTS, RULE_API_DTOS, RULE_STRICT_LAYERING] {
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
