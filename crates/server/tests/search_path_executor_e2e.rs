//! END-TO-END: the deterministic `SUPABASE-FUNC-SEARCH-PATH-1` checker, wired into the
//! brownfield SCAN, and proven to reach the CURATED finding set. See
//! `docs/plans/2026-09-29_codebase-inspection-hardening.md`, D1.
//!
//! D1 was filed as a REGRESSION: a `SECURITY DEFINER` plpgsql function with no pinned
//! `search_path` (next to a sibling that IS `SECURITY INVOKER` with `search_path` pinned)
//! was not flagged in a real brownfield export, even though `SupabaseFnSearchPathChecker`
//! exists and an older fixture run DID catch this class. This file is the regression-closing
//! test the plan calls for: it drives the checker through the REAL production entry points
//! (`onboard::audit_repos` -> `report_export::build_report_json`), not just the checker's own
//! unit tests, so a future change that re-drops this finding anywhere in that pipeline
//! (rule arming, glob matching, merge/dedup routing, the citation gate) fails HERE.
//!
//! Mirrors `architectural_executor_e2e.rs`'s pattern exactly (same fixture-staging shape, same
//! two-suite split) for the sibling Supabase migration-replay checker.
//!
//! FIXTURE: `tests/fixtures/search_path_repo/` — one migration with THREE functions, a
//! realistic multi-line shape (LANGUAGE before SECURITY, `create or replace`, a
//! semicolon-bearing dollar-quoted body) rather than the checker unit tests' minimal
//! single-line form:
//!   - `grant_temporary_access` — SECURITY DEFINER, no `SET search_path` (the planted defect).
//!   - `list_own_records` — SECURITY INVOKER, `search_path` pinned anyway (safe twin #1).
//!   - `rotate_share_link` — SECURITY DEFINER, `search_path` correctly pinned (safe twin #2).
//!
//! ZERO API SPEND: `run_ai_review: false` throughout.

use std::collections::HashMap;
use std::path::Path;

use camerata_server::ai_audit::ScanMode;
use camerata_server::onboard::{self, SelectedRule};
use camerata_server::report_export::{self, DispositionWire, ReportOptions};

const RULE_SEARCH_PATH: &str = "SUPABASE-FUNC-SEARCH-PATH-1";

/// The migration file + line the fixture's planted `grant_temporary_access` function sits at.
/// A named constant (not re-derived) so a future fixture edit that shifts this line fails
/// LOUDLY at the assertion rather than silently asserting against whatever line is current.
const FUNCTIONS_MIGRATION_FILE: &str = "supabase/migrations/20240301000000_functions.sql";
const GRANT_TEMPORARY_ACCESS_LINE: usize = 5;

/// Copy the checked-in fixture tree into `dest`, then turn `dest` into a real one-commit git
/// repo so `onboard::capture_audited_ref`'s shell-outs succeed — mirrors
/// `architectural_executor_e2e.rs::stage_fixture_as_git_repo` exactly (duplicated per this
/// repo's existing convention of each e2e test file owning its own fixture-staging helpers).
fn stage_fixture_as_git_repo(dest: &Path) {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/search_path_repo");
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
    g(&["config", "user.email", "search-path-e2e@camerata.local"]);
    g(&["config", "user.name", "Camerata Search-Path E2E Test"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "e2e fixture: one unpinned SECURITY DEFINER + two safe twins"]);
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

fn typst_on_path() -> bool {
    std::process::Command::new("typst")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some()
}

fn selected_search_path_rule() -> Vec<SelectedRule> {
    vec![SelectedRule {
        id: RULE_SEARCH_PATH.to_string(),
        directive: "search-path e2e: flag SECURITY DEFINER functions with no pinned search_path".to_string(),
        repos: Vec::new(), // project-level: applies to every scanned repo
    }]
}

/// Run the real scan entry point over the fixture, deterministic-only. Shared by both suites
/// below.
async fn run_fixture_scan(repo_dir: &Path, repo_spec: &str) -> onboard::ScanReport {
    let sources = vec![(repo_spec.to_string(), repo_dir.to_path_buf())];
    let selected = selected_search_path_rule();
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
        camerata_server::llm::BackendResolution::Api, // backend gate: not under test here
        None, // corpus
        &std::collections::HashMap::new(), // chosen_options
    )
    .await;
    report
}

#[tokio::test]
async fn deterministic_scan_finds_the_planted_search_path_hole() {
    let repo_spec = "e2e/search-path-fixture".to_string();
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo(repo_dir.path());

    let report = run_fixture_scan(repo_dir.path(), &repo_spec).await;

    assert!(!report.gated, "a local-dir scan is never gated on a GitHub token");
    assert_eq!(report.repos, vec![repo_spec.clone()]);

    // ═══ Exactly the ONE planted finding: `grant_temporary_access` has no search_path. ═══
    // Neither safe twin (`list_own_records` — invoker + pinned; `rotate_share_link` — definer
    // + correctly pinned) produces a finding — proves the checker isn't just flagging every
    // SECURITY DEFINER function, or every function in the file.
    let hits: Vec<&onboard::Finding> =
        report.findings.iter().filter(|f| f.rule_id == RULE_SEARCH_PATH).collect();
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one SUPABASE-FUNC-SEARCH-PATH-1 finding (grant_temporary_access only): {:?}",
        report.findings
    );
    let finding = hits[0];

    assert_eq!(finding.severity, "high", "an unpinned SECURITY DEFINER function is high, not critical/medium");
    assert_eq!(finding.repo, repo_spec);
    assert_eq!(
        finding.path, FUNCTIONS_MIGRATION_FILE,
        "the finding must be attributed to the migration that defines the function"
    );
    assert_eq!(finding.line, GRANT_TEMPORARY_ACCESS_LINE, "must point at the CREATE FUNCTION statement's line");
    assert!(
        finding.snippet.contains("grant_temporary_access") || finding.detail.contains("grant_temporary_access"),
        "finding must name the offending function, not either safe twin: {finding:?}"
    );
    assert!(
        !finding.detail.contains("list_own_records") && !finding.detail.contains("rotate_share_link"),
        "finding must not conflate the offending function with either safe twin: {finding:?}"
    );

    // ═══ Provenance: deterministic-arch tagging (the mechanism the UI badges on) ═══
    assert!(finding.preview, "an architectural finding uses the existing preview mechanism");
    assert_eq!(finding.preview_tool.as_deref(), Some("camerata-arch"), "must carry the camerata-arch provenance tag");
    assert_eq!(finding.status, "active", "no suppression/baseline in the fixture — must be active");

    // ═══ Provenance stamp sanity (mirrors architectural_executor_e2e.rs) ═══
    assert!(!report.provenance.audited_refs.is_empty());
    let sha = report.provenance.audited_refs[0].sha.as_deref().expect("a real one-commit git repo must yield a SHA");
    assert_eq!(sha.len(), 40);
}

/// The scan-path regression test D1 explicitly calls for: proves the checker's output reaches
/// the CURATED finding set through the real report pipeline — not just the checker's own unit
/// tests, and not just `report.findings` (the raw scan output `deterministic_scan_finds_the_
/// planted_search_path_hole` already covers). This is where the plan's "not dropped by
/// routing/filtering" concern is actually closed: `build_report_json` runs the P1 merge/dedup
/// passes and the P3 citation gate over `report.findings` before anything lands in
/// `curated_findings`, and either one could in principle swallow this finding even though the
/// raw scan found it.
#[tokio::test]
async fn search_path_finding_reaches_curated_findings_with_a_grounded_citation() {
    let repo_spec = "e2e/search-path-fixture".to_string();
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo(repo_dir.path());

    // ═══ STEP 1: the real scan (same entry point the cockpit uses). ═══
    let report = run_fixture_scan(repo_dir.path(), &repo_spec).await;
    let finding = report
        .findings
        .iter()
        .find(|f| f.rule_id == RULE_SEARCH_PATH)
        .expect("the planted search-path finding must be present (see the sibling scan test)");
    assert_eq!(finding.preview_tool.as_deref(), Some("camerata-arch"));

    // ═══ STEP 2: build_report_json against the REAL bundled corpus — no disposition ═══
    // supplied, so the finding stays Open/Unresolved.
    let dispositions: HashMap<String, DispositionWire> = HashMap::new();
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, corpus_errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(corpus_errors.is_empty(), "the bundled rule corpus must load cleanly: {corpus_errors:?}");

    let opts = ReportOptions {
        client_name: "Acme Corp".to_string(),
        project_title: "Search-Path E2E Fixture Audit".to_string(),
        prepared_by: "Camerata (automated e2e test)".to_string(),
        executive_summary_override: None,
        ..Default::default()
    };
    let json = report_export::build_report_json(&report, &dispositions, Some(&corpus), &opts);

    // ═══ STEP 3: the finding survives as a CURATED finding — High severity, right file:line, ═══
    // grounded citation (not the "AI-advisory, model-inferred." the P3 gate would stamp on an
    // uncited AI-tier finding, and not the bare "preview, not corpus-documented" label a
    // deterministic finding gets when its rule id has NO corpus citation).
    let group = json
        .curated_findings
        .iter()
        .find(|g| g.rule_id == RULE_SEARCH_PATH)
        .expect("SUPABASE-FUNC-SEARCH-PATH-1 must appear in curated_findings — this is the regression D1 closes");
    assert_eq!(group.sites.len(), 1, "{:?}", group.sites);
    let site = &group.sites[0];
    assert_eq!(site.path, FUNCTIONS_MIGRATION_FILE);
    assert_eq!(site.line, GRANT_TEMPORARY_ACCESS_LINE);
    assert_eq!(site.severity, "high");
    assert!(site.detail.contains("grant_temporary_access"));
    // M4: an uncalibrated high-severity finding recommends "Do next" (never buried as
    // informational — `is_informational` hard-exempts critical/high regardless of family).
    assert_eq!(
        site.disposition, "Open (recommended: Do next)",
        "no disposition was supplied -> stays Open, recommending its computed matrix bucket"
    );
    assert_eq!(
        group.citation.kind, "grounded",
        "SUPABASE-FUNC-SEARCH-PATH-1 carries real citations (PostgreSQL's own \"Writing SECURITY \
         DEFINER Functions Safely\" + the Supabase database linter's function-search-path-mutable \
         check) in the bundled corpus — resolve_citation must prefer that over the bare preview \
         label: {:?}",
        group.citation
    );
    assert!(!group.citation.sources.is_empty());
    assert!(
        group.citation.sources.iter().any(|s| s.url.contains("postgresql.org")),
        "expected the PostgreSQL CREATE FUNCTION security-definer citation: {:?}",
        group.citation.sources
    );
    assert!(
        group.citation.sources.iter().any(|s| s.url.contains("supabase.com")),
        "expected the Supabase database-linter citation: {:?}",
        group.citation.sources
    );

    // ═══ The finding must NOT be routed to the informational appendix (Bug 4's bucketing, or ═══
    // P3's citation gate) — it is a High-severity, fully-grounded, actionable defect.
    assert!(
        json.matrix.informational.iter().all(|f| f.rule_id != RULE_SEARCH_PATH),
        "a High-severity, grounded finding must never land in the informational bucket: {:?}",
        json.matrix.informational
    );

    // Methodology sanity: nothing was dispositioned FalsePositive, so nothing is excluded.
    assert_eq!(json.methodology.excluded_false_positive, 0);

    // ═══ STEP 4: compile_pdf — gated on typst being on PATH (mirrors architectural_executor_e2e.rs). ═══
    if !typst_on_path() {
        eprintln!("skipping PDF compile assertion: typst not on PATH");
        return;
    }
    let pdf_bytes = report_export::compile_pdf(&json)
        .await
        .expect("typst compile must succeed for a well-formed report");
    assert!(pdf_bytes.starts_with(b"%PDF"), "output must be a real PDF (starts with the %PDF magic bytes)");
    assert!(pdf_bytes.len() > 1000, "a real compiled PDF is not a trivially tiny file");
}
