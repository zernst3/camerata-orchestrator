//! END-TO-END: the deterministic architectural-rule executor (Pass 1 — the seam + the
//! Supabase RLS migration-replay checkers, wired into the brownfield SCAN only). See
//! `docs/design/2026-07-26_architectural-executor-feasibility.md`.
//!
//! Two suites, mirroring `e2e_report_pipeline.rs`'s pattern for the floor:
//!
//!   1. `deterministic_scan_finds_the_planted_rls_hole` — drives the REAL scan entry point
//!      (`onboard::audit_repos`, deterministic-only, zero API spend) over a committed fixture
//!      Supabase repo and asserts the planted `SUPABASE-RLS-ENABLED-1` finding fires with the
//!      right table, the right migration file:line, the `camerata-arch` provenance tag, and
//!      the repo-vs-production honesty caveat — while the CLEAN table in the same fixture
//!      produces NO finding (proving the checker isn't just flagging every table).
//!
//!   2. `architectural_finding_rides_through_report_to_pdf` — takes that SAME scan output
//!      through the report spine (`report_export::build_report_json` -> `compile_pdf`,
//!      gated on `typst` being on PATH) and asserts the finding survives as a curated
//!      finding with its table/file/line/caveat intact, proving the arch engine integrates
//!      with the existing report pipeline end to end, not just the scan.
//!
//! ZERO API SPEND: `run_ai_review: false` throughout — no model call anywhere in this file.
//!
//! FIXTURE: `tests/fixtures/supabase_rls_repo/` — a `supabase/config.toml` exposing only
//! `public`, plus one migration declaring `public.profiles` (created, RLS NEVER enabled —
//! the planted hole) and `public.orders` (RLS enabled + a real policy — the clean control).

use std::collections::HashMap;
use std::path::Path;

use camerata_server::ai_audit::ScanMode;
use camerata_server::onboard::{self, SelectedRule};
use camerata_server::report_export::{self, DispositionWire, ReportOptions};

const RULE_RLS_ENABLED: &str = "SUPABASE-RLS-ENABLED-1";
const RULE_RLS_NO_POLICY: &str = "SUPABASE-RLS-NO-POLICY-1";
const RULE_RLS_POLICY_DISABLED: &str = "SUPABASE-RLS-POLICY-DISABLED-1";

/// The migration file + line the fixture's `CREATE TABLE public.profiles` statement sits at
/// — the last (and only) statement establishing its RLS state, since RLS is never enabled
/// for it anywhere in the fixture's migration history. Kept as named constants (not
/// re-derived) so a future fixture edit that shifts this line fails LOUDLY at the assertion,
/// rather than silently asserting against whatever line happens to be current.
const PROFILES_MIGRATION_FILE: &str = "supabase/migrations/20240301000000_init.sql";
const PROFILES_MIGRATION_LINE: usize = 5;

/// Copy the checked-in fixture tree into `dest`, then turn `dest` into a real one-commit git
/// repo so `onboard::capture_audited_ref`'s shell-outs succeed — mirrors
/// `e2e_report_pipeline.rs::stage_fixture_as_git_repo` exactly (duplicated per this repo's
/// existing convention of each e2e test file owning its own fixture-staging helpers).
fn stage_fixture_as_git_repo(dest: &Path) {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/supabase_rls_repo");
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
    g(&["config", "user.email", "architectural-e2e@camerata.local"]);
    g(&["config", "user.name", "Camerata Architectural E2E Test"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "e2e fixture: one RLS hole + one clean table"]);
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

fn selected_rls_rules() -> Vec<SelectedRule> {
    [RULE_RLS_ENABLED, RULE_RLS_NO_POLICY, RULE_RLS_POLICY_DISABLED]
        .into_iter()
        .map(|id| SelectedRule {
            id: id.to_string(),
            directive: format!("architectural-executor e2e: {id}"),
            repos: Vec::new(), // project-level: applies to every scanned repo
        })
        .collect()
}

/// Run the real scan entry point over the fixture, deterministic-only. Shared by both
/// suites below.
async fn run_fixture_scan(repo_dir: &Path, repo_spec: &str) -> onboard::ScanReport {
    let sources = vec![(repo_spec.to_string(), repo_dir.to_path_buf())];
    let selected = selected_rls_rules();
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
async fn deterministic_scan_finds_the_planted_rls_hole() {
    let repo_spec = "e2e/supabase-rls-fixture".to_string();
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo(repo_dir.path());

    let report = run_fixture_scan(repo_dir.path(), &repo_spec).await;

    assert!(!report.gated, "a local-dir scan is never gated on a GitHub token");
    assert_eq!(report.repos, vec![repo_spec.clone()]);

    // ═══ Exactly the ONE planted finding: `profiles` has no RLS. No finding for `orders` ═══
    // (clean: RLS enabled + a real policy) — proves the checker isn't just flagging every
    // table it sees, and no NO-POLICY/POLICY-DISABLED false positive fires either.
    let rls_findings: Vec<&onboard::Finding> = report
        .findings
        .iter()
        .filter(|f| f.rule_id == RULE_RLS_ENABLED || f.rule_id == RULE_RLS_NO_POLICY || f.rule_id == RULE_RLS_POLICY_DISABLED)
        .collect();
    assert_eq!(
        rls_findings.len(),
        1,
        "expected exactly one architectural RLS finding (profiles, no RLS): {:?}",
        report.findings
    );
    let finding = rls_findings[0];

    assert_eq!(finding.rule_id, RULE_RLS_ENABLED);
    assert_eq!(finding.severity, "critical", "an exposed table with no RLS is critical");
    assert_eq!(finding.repo, repo_spec);
    assert_eq!(
        finding.path, PROFILES_MIGRATION_FILE,
        "the finding must be attributed to the migration that established (never changed) the RLS state"
    );
    assert_eq!(finding.line, PROFILES_MIGRATION_LINE, "must point at the CREATE TABLE statement's line");
    assert!(finding.snippet.contains("profiles"), "snippet must name the table: {:?}", finding.snippet);
    assert!(finding.detail.contains("profiles"), "detail must name the table: {:?}", finding.detail);
    assert!(
        finding.detail.to_lowercase().contains("confirm against"),
        "detail must carry the repo-vs-production honesty caveat: {:?}",
        finding.detail
    );
    assert!(
        finding.detail.to_lowercase().contains("dashboard"),
        "detail must call out the Supabase-dashboard drift case specifically: {:?}",
        finding.detail
    );

    // ═══ Provenance: deterministic-arch tagging (the mechanism the UI badges on) ═══
    assert!(finding.preview, "an architectural finding uses the existing preview mechanism");
    assert_eq!(
        finding.preview_tool.as_deref(),
        Some("camerata-arch"),
        "must carry the camerata-arch provenance tag"
    );
    assert_eq!(finding.status, "active", "no suppression/baseline in the fixture — must be active");

    // ═══ Provenance stamp sanity (mirrors e2e_report_pipeline.rs) ═══
    assert!(!report.provenance.audited_refs.is_empty());
    let sha = report.provenance.audited_refs[0]
        .sha
        .as_deref()
        .expect("a real one-commit git repo must yield a SHA");
    assert_eq!(sha.len(), 40);
}

#[tokio::test]
async fn architectural_finding_rides_through_report_to_pdf() {
    let repo_spec = "e2e/supabase-rls-fixture".to_string();
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo(repo_dir.path());

    // ═══ STEP 1: the real scan (same entry point the cockpit uses). ═══
    let report = run_fixture_scan(repo_dir.path(), &repo_spec).await;
    let finding = report
        .findings
        .iter()
        .find(|f| f.rule_id == RULE_RLS_ENABLED)
        .expect("the planted RLS finding must be present (see deterministic_scan_finds_the_planted_rls_hole)");
    assert_eq!(finding.preview_tool.as_deref(), Some("camerata-arch"));

    // ═══ STEP 2: build_report_json — no disposition supplied, so it stays Open/Unresolved. ═══
    let dispositions: HashMap<String, DispositionWire> = HashMap::new();
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, corpus_errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(corpus_errors.is_empty(), "the bundled rule corpus must load cleanly: {corpus_errors:?}");

    let opts = ReportOptions {
        client_name: "Acme Corp".to_string(),
        project_title: "Architectural Executor E2E Fixture Audit".to_string(),
        prepared_by: "Camerata (automated e2e test)".to_string(),
        executive_summary_override: None,
    };
    let json = report_export::build_report_json(&report, &dispositions, Some(&corpus), &opts);

    // ═══ STEP 3: the finding survives as a CURATED finding, with the table/file/line/caveat ═══
    // intact and (because SUPABASE-RLS-ENABLED-1 is a real grounded corpus rule) a citation
    // resolved from the corpus rather than the bare preview label — proving the report
    // pipeline treats an architectural finding exactly like any other real finding.
    let group = json
        .curated_findings
        .iter()
        .find(|g| g.rule_id == RULE_RLS_ENABLED)
        .expect("SUPABASE-RLS-ENABLED-1 must appear in curated_findings");
    assert_eq!(group.sites.len(), 1, "{:?}", group.sites);
    let site = &group.sites[0];
    assert_eq!(site.path, PROFILES_MIGRATION_FILE);
    assert_eq!(site.line, PROFILES_MIGRATION_LINE);
    assert_eq!(site.severity, "critical");
    assert!(site.detail.contains("profiles"));
    assert!(site.detail.to_lowercase().contains("confirm against"));
    // M3: an uncalibrated critical finding always recommends "Do now" (M7 names the bucket).
    assert_eq!(
        site.disposition, "Open (recommended: Do now)",
        "no disposition was supplied -> stays Open, recommending its computed matrix bucket"
    );
    assert_eq!(
        group.citation.kind, "grounded",
        "SUPABASE-RLS-ENABLED-1 has real cited sources in the corpus — resolve_citation must prefer that \
         over the bare preview label: {:?}",
        group.citation
    );
    assert!(!group.citation.sources.is_empty());

    // Methodology sanity: nothing was dispositioned FalsePositive, so nothing is excluded.
    assert_eq!(json.methodology.excluded_false_positive, 0);

    // ═══ STEP 4: compile_pdf — gated on typst being on PATH (mirrors e2e_report_pipeline.rs). ═══
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
