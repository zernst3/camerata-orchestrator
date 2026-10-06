//! END-TO-END: the deterministic `SUPABASE-FUNC-DEFINER-CROSS-TENANT-READ-1` checker
//! (`CrossTenantDefinerReadChecker`), wired into the brownfield SCAN, and proven to reach the
//! CURATED finding set as HIGH with a grounded citation — not just the checker's own unit
//! tests. Mirrors `privileged_no_authz_executor_e2e.rs`'s two-suite pattern for the WRITE-shape
//! sibling rule this one closes the READ-shape gap next to.
//!
//! `SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1` already covers a mutating `SECURITY DEFINER` function
//! with no caller-identity check. This rule covers the uncovered READ shape of the identical
//! defect: a `SECURITY DEFINER` function that returns rows filtered ONLY by its own parameter
//! (a parent/tenant/owner id supplied by the caller), granted EXECUTE to a broad role, with no
//! predicate tying those rows to the CALLER's own identity — CWE-639 (Authorization Bypass
//! Through User-Controlled Key), the cross-tenant-read-via-RPC shape.
//!
//! FIXTURE: `tests/fixtures/cross_tenant_definer_read_repo/supabase/migrations/0001_fn.sql`:
//!   - `public.get_org_invoices(p_org_id uuid)` — the planted defect: `SECURITY DEFINER`,
//!     `return query select * from invoices where org_id = p_org_id`, `GRANT EXECUTE ... TO
//!     authenticated` on the SAME function in the SAME file, and NO identity/role/ownership/
//!     secret predicate in its body.
//!   - `public.safe_with_predicate` / `public.safe_service_role_only` / `public.safe_invoker` —
//!     three safe twins (an `auth.uid()` membership check, a grant to a non-broad service role
//!     only, and a non-`SECURITY DEFINER` invoker-rights function) proving the checker
//!     discriminates by MECHANISM, not by merely being a `SECURITY DEFINER` read filtered by a
//!     parameter.
//!
//! ZERO API SPEND: `run_ai_review: false` throughout.

use std::collections::HashMap;
use std::path::Path;

use camerata_server::ai_audit::ScanMode;
use camerata_server::onboard::{self, SelectedRule};
use camerata_server::report_export::{self, DispositionWire, ReportOptions};

const RULE_DEFINER_CROSS_TENANT_READ: &str = "SUPABASE-FUNC-DEFINER-CROSS-TENANT-READ-1";

/// The file + line the fixture's planted `get_org_invoices` defect sits at (the `CREATE
/// FUNCTION` statement's own start line). A named constant (not re-derived) so a future
/// fixture edit that shifts this line fails LOUDLY at the assertion rather than silently
/// asserting against whatever line is current.
const FN_FILE: &str = "supabase/migrations/0001_fn.sql";
const FN_LINE: usize = 7;

/// Copy the checked-in fixture tree into `dest`, then turn `dest` into a real one-commit git
/// repo so `onboard::capture_audited_ref`'s shell-outs succeed — mirrors
/// `privileged_no_authz_executor_e2e.rs::stage_fixture_as_git_repo` exactly (duplicated per
/// this repo's existing convention of each e2e test file owning its own fixture-staging
/// helpers).
fn stage_fixture_as_git_repo(dest: &Path) {
    let fixture_root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cross_tenant_definer_read_repo");
    copy_dir_recursive(&fixture_root, dest);

    let g = |args: &[&str]| {
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
    };
    g(&["init", "-q", "-b", "main"]);
    g(&[
        "config",
        "user.email",
        "cross-tenant-definer-read-e2e@camerata.local",
    ]);
    g(&[
        "config",
        "user.name",
        "Camerata Cross-Tenant-Definer-Read E2E Test",
    ]);
    g(&["add", "."]);
    g(&[
        "commit",
        "-q",
        "-m",
        "e2e fixture: one cross-tenant DEFINER read (with authenticated grant) + three safe twins",
    ]);
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
            std::fs::copy(&from, &to)
                .unwrap_or_else(|e| panic!("failed to copy fixture file {from:?} -> {to:?}: {e}"));
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

fn selected_cross_tenant_definer_read_rule() -> Vec<SelectedRule> {
    vec![SelectedRule {
        id: RULE_DEFINER_CROSS_TENANT_READ.to_string(),
        directive: "cross-tenant-definer-read e2e: flag a SECURITY DEFINER function that \
                    returns rows filtered only by its own caller-supplied parameter, with no \
                    identity check"
            .to_string(),
        repos: Vec::new(), // project-level: applies to every scanned repo
    }]
}

/// Run the real scan entry point over the fixture, deterministic-only.
async fn run_fixture_scan(repo_dir: &Path, repo_spec: &str) -> onboard::ScanReport {
    let sources = vec![(repo_spec.to_string(), repo_dir.to_path_buf())];
    let selected = selected_cross_tenant_definer_read_rule();
    let (report, _manifest) = onboard::audit_repos(
        &sources,
        &selected,
        Vec::new(), // extra_notes
        None,       // model — unused, run_ai_review is false
        None,       // calibration_model — unused
        ScanMode::Sequential,
        false,                                        // thorough — unused (no AI review)
        None,                                         // feedback
        None,                                         // job
        None,                                         // incremental_prior — full scan
        false,                                        // deep
        false,                                        // soc2_enabled
        false,                                        // run_ai_review — zero API spend
        true, // run_deterministic — the floor AND the architectural engine both run
        None, // ledger
        camerata_server::llm::BackendResolution::Api, // backend gate: not under test here
        None, // corpus
        &std::collections::HashMap::new(), // chosen_options
    )
    .await;
    report
}

#[tokio::test]
async fn deterministic_scan_finds_the_planted_cross_tenant_definer_read() {
    let repo_spec = "e2e/cross-tenant-definer-read-fixture".to_string();
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo(repo_dir.path());

    let report = run_fixture_scan(repo_dir.path(), &repo_spec).await;

    assert!(
        !report.gated,
        "a local-dir scan is never gated on a GitHub token"
    );
    assert_eq!(report.repos, vec![repo_spec.clone()]);

    // ═══ Exactly the ONE planted finding: `get_org_invoices`'s unchecked cross-tenant read. ═══
    // None of the three safe twins (`safe_with_predicate`, `safe_service_role_only`,
    // `safe_invoker`) produce a finding — proves the checker discriminates by MECHANISM, not by
    // merely being a SECURITY DEFINER function filtered by a parameter.
    let hits: Vec<&onboard::Finding> = report
        .findings
        .iter()
        .filter(|f| f.rule_id == RULE_DEFINER_CROSS_TENANT_READ)
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one SUPABASE-FUNC-DEFINER-CROSS-TENANT-READ-1 finding (get_org_invoices only): {:?}",
        report.findings
    );
    let finding = hits[0];

    assert_eq!(finding.severity, "high");
    assert_eq!(finding.repo, repo_spec);
    assert_eq!(
        finding.path, FN_FILE,
        "the finding must be attributed to the file defining the function"
    );
    assert_eq!(
        finding.line, FN_LINE,
        "must point at the CREATE FUNCTION statement's own line"
    );
    assert!(
        finding.detail.contains("get_org_invoices"),
        "the message must name the offending function: {finding:?}"
    );
    assert!(
        finding.detail.contains("CWE-639"),
        "the message must name the CWE class: {finding:?}"
    );

    // ═══ Provenance: deterministic-arch tagging (the mechanism the UI badges on) ═══
    assert!(
        finding.preview,
        "an architectural finding uses the existing preview mechanism"
    );
    assert_eq!(
        finding.preview_tool.as_deref(),
        Some("camerata-arch"),
        "must carry the camerata-arch provenance tag"
    );
    assert_eq!(
        finding.status, "active",
        "no suppression/baseline in the fixture — must be active"
    );
    assert!(finding.confidence.is_none());
    assert!(!finding.rule_id.starts_with("AI-"));

    // ═══ Provenance stamp sanity ═══
    assert!(!report.provenance.audited_refs.is_empty());
    let sha = report.provenance.audited_refs[0]
        .sha
        .as_deref()
        .expect("a real one-commit git repo must yield a SHA");
    assert_eq!(sha.len(), 40);
}

/// The scan-path proof: proves the checker's output reaches the CURATED finding set through the
/// real report pipeline — not just the checker's own unit tests, and not just `report.findings`
/// (the raw scan output the sibling test above already covers) — at High, with a grounded
/// citation (CWE-639).
#[tokio::test]
async fn cross_tenant_read_finding_reaches_curated_findings_high_and_grounded() {
    let repo_spec = "e2e/cross-tenant-definer-read-fixture".to_string();
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo(repo_dir.path());

    // ═══ STEP 1: the real scan (same entry point the cockpit uses). ═══
    let report = run_fixture_scan(repo_dir.path(), &repo_spec).await;
    let finding = report
        .findings
        .iter()
        .find(|f| f.rule_id == RULE_DEFINER_CROSS_TENANT_READ)
        .expect(
            "the planted cross-tenant DEFINER read finding must be present (see the sibling \
             scan test)",
        );
    assert_eq!(finding.preview_tool.as_deref(), Some("camerata-arch"));

    // ═══ STEP 2: build_report_json against the REAL bundled corpus — no disposition ═══
    // supplied, so the finding stays Open/Unresolved.
    let dispositions: HashMap<String, DispositionWire> = HashMap::new();
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, corpus_errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        corpus_errors.is_empty(),
        "the bundled rule corpus must load cleanly: {corpus_errors:?}"
    );

    let opts = ReportOptions {
        client_name: "Acme Corp".to_string(),
        project_title: "Cross-Tenant-Definer-Read E2E Fixture Audit".to_string(),
        prepared_by: "Camerata (automated e2e test)".to_string(),
        executive_summary_override: None,
        ..Default::default()
    };
    let json = report_export::build_report_json(&report, &dispositions, Some(&corpus), &opts);

    // ═══ STEP 3: the finding survives as a CURATED finding — High, right file:line, ═══
    // grounded citation (CWE-639 — Authorization Bypass Through User-Controlled Key — the SAME
    // corpus rule this change grounds).
    let group = json
        .curated_findings
        .iter()
        .find(|g| g.rule_id == RULE_DEFINER_CROSS_TENANT_READ)
        .expect(
            "SUPABASE-FUNC-DEFINER-CROSS-TENANT-READ-1 must appear in curated_findings — this \
             is the READ-shape gap this change closes",
        );
    assert_eq!(group.sites.len(), 1, "{:?}", group.sites);
    let site = &group.sites[0];
    assert_eq!(site.path, FN_FILE);
    assert_eq!(site.line, FN_LINE);
    assert_eq!(site.severity, "high", "{site:?}");
    assert_eq!(group.citation.kind, "grounded");
    assert!(!group.citation.sources.is_empty());
    assert!(
        group
            .citation
            .sources
            .iter()
            .any(|s| s.url.contains("cwe.mitre.org") && s.url.contains("639")),
        "expected a CWE-639 citation: {:?}",
        group.citation.sources
    );

    // ═══ A High finding must land in an action tier (do_now/do_next/plan), never the ═══
    // informational appendix.
    assert!(
        json.matrix
            .informational
            .iter()
            .all(|f| f.rule_id != RULE_DEFINER_CROSS_TENANT_READ),
        "a High, grounded finding must never land in the informational bucket: {:?}",
        json.matrix.informational
    );

    assert_eq!(json.methodology.excluded_false_positive, 0);

    // ═══ STEP 4: compile_pdf — gated on typst being on PATH. ═══
    if !typst_on_path() {
        eprintln!("skipping PDF compile assertion: typst not on PATH");
        return;
    }
    let pdf_bytes = report_export::compile_pdf(&json)
        .await
        .expect("typst compile must succeed for a well-formed report");
    assert!(
        pdf_bytes.starts_with(b"%PDF"),
        "output must be a real PDF (starts with the %PDF magic bytes)"
    );
    assert!(
        pdf_bytes.len() > 1000,
        "a real compiled PDF is not a trivially tiny file"
    );
}
