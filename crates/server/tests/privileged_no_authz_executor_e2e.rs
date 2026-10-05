//! END-TO-END: the deterministic `SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1` checker
//! (`PrivilegedFunctionNoAuthzChecker`), wired into the brownfield SCAN, and proven to reach the
//! CURATED finding set as HIGH with a grounded citation — not just the checker's own unit tests.
//! Mirrors `dynamic_sql_exec_injection_executor_e2e.rs`'s two-suite pattern for the sibling
//! checker this one was built next to (C6-A2: the class only the AI tier caught — hedged,
//! uncited, fix-less — on an unseen hold-out repo).
//!
//! FIXTURE: `tests/fixtures/privileged_no_authz_repo/supabase/migrations/0001_fn.sql`:
//!   - `public.delete_account(p_id uuid)` — the planted defect: `SECURITY DEFINER`, a `DELETE`
//!     write, `GRANT EXECUTE ... TO anon` on the SAME function in the SAME file, and NO
//!     identity/role/ownership/secret predicate in its body.
//!   - `public.safe_with_predicate` / `public.safe_service_role_only` / `public.safe_readonly` /
//!     `public.safe_invoker` — four safe twins (an `auth.uid()` ownership check, a grant to a
//!     non-broad service role only, a read-only body, and a non-`SECURITY DEFINER` function)
//!     proving the checker discriminates by MECHANISM, not by merely being a database function.
//!
//! ZERO API SPEND: `run_ai_review: false` throughout.

use std::collections::HashMap;
use std::path::Path;

use camerata_server::ai_audit::ScanMode;
use camerata_server::onboard::{self, SelectedRule};
use camerata_server::report_export::{self, DispositionWire, ReportOptions};

const RULE_PRIVILEGED_NO_AUTHZ: &str = "SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1";

/// The file + line the fixture's planted `delete_account` defect sits at (the `CREATE FUNCTION`
/// statement's own start line). A named constant (not re-derived) so a future fixture edit that
/// shifts this line fails LOUDLY at the assertion rather than silently asserting against
/// whatever line is current.
const FN_FILE: &str = "supabase/migrations/0001_fn.sql";
const FN_LINE: usize = 5;

/// Copy the checked-in fixture tree into `dest`, then turn `dest` into a real one-commit git
/// repo so `onboard::capture_audited_ref`'s shell-outs succeed — mirrors
/// `dynamic_sql_exec_injection_executor_e2e.rs::stage_fixture_as_git_repo` exactly (duplicated
/// per this repo's existing convention of each e2e test file owning its own fixture-staging
/// helpers).
fn stage_fixture_as_git_repo(dest: &Path) {
    let fixture_root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/privileged_no_authz_repo");
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
        "privileged-no-authz-e2e@camerata.local",
    ]);
    g(&[
        "config",
        "user.name",
        "Camerata Privileged-No-Authz E2E Test",
    ]);
    g(&["add", "."]);
    g(&[
        "commit",
        "-q",
        "-m",
        "e2e fixture: one privileged-no-authz function (with anon grant) + four safe twins",
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

fn selected_privileged_no_authz_rule() -> Vec<SelectedRule> {
    vec![SelectedRule {
        id: RULE_PRIVILEGED_NO_AUTHZ.to_string(),
        directive: "privileged-no-authz e2e: flag a SECURITY DEFINER function that writes data, \
                    is broadly granted, and has no authorization predicate"
            .to_string(),
        repos: Vec::new(), // project-level: applies to every scanned repo
    }]
}

/// Run the real scan entry point over the fixture, deterministic-only.
async fn run_fixture_scan(repo_dir: &Path, repo_spec: &str) -> onboard::ScanReport {
    let sources = vec![(repo_spec.to_string(), repo_dir.to_path_buf())];
    let selected = selected_privileged_no_authz_rule();
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
async fn deterministic_scan_finds_the_planted_privileged_no_authz_function() {
    let repo_spec = "e2e/privileged-no-authz-fixture".to_string();
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo(repo_dir.path());

    let report = run_fixture_scan(repo_dir.path(), &repo_spec).await;

    assert!(
        !report.gated,
        "a local-dir scan is never gated on a GitHub token"
    );
    assert_eq!(report.repos, vec![repo_spec.clone()]);

    // ═══ Exactly the ONE planted finding: `delete_account`'s unguarded privileged write. ═══
    // None of the four safe twins (`safe_with_predicate`, `safe_service_role_only`,
    // `safe_readonly`, `safe_invoker`) produce a finding — proves the checker isn't just
    // flagging every SECURITY DEFINER function, or every function that writes.
    let hits: Vec<&onboard::Finding> = report
        .findings
        .iter()
        .filter(|f| f.rule_id == RULE_PRIVILEGED_NO_AUTHZ)
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1 finding (delete_account only): {:?}",
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
        finding.detail.contains("anon"),
        "the message must name the reachability path: {finding:?}"
    );
    assert!(
        finding
            .detail
            .to_ascii_lowercase()
            .contains("security definer"),
        "the message must name the offending mechanism: {finding:?}"
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
    // Deterministic findings never carry an AI confidence tag or an AI- rule-id prefix — the
    // floor must fire on its own, never relying on the AI/semantic tier as a backstop.
    assert!(finding.confidence.is_none());
    assert!(!finding.rule_id.starts_with("AI-"));

    // ═══ Provenance stamp sanity (mirrors dynamic_sql_exec_injection_executor_e2e.rs) ═══
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
/// citation (CWE-862 + OWASP ASVS V4 + the Supabase SECURITY DEFINER docs).
#[tokio::test]
async fn privileged_no_authz_finding_reaches_curated_findings_high_and_grounded() {
    let repo_spec = "e2e/privileged-no-authz-fixture".to_string();
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo(repo_dir.path());

    // ═══ STEP 1: the real scan (same entry point the cockpit uses). ═══
    let report = run_fixture_scan(repo_dir.path(), &repo_spec).await;
    let finding = report
        .findings
        .iter()
        .find(|f| f.rule_id == RULE_PRIVILEGED_NO_AUTHZ)
        .expect(
            "the planted privileged-no-authz finding must be present (see the sibling scan test)",
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
        project_title: "Privileged-No-Authz E2E Fixture Audit".to_string(),
        prepared_by: "Camerata (automated e2e test)".to_string(),
        executive_summary_override: None,
        ..Default::default()
    };
    let json = report_export::build_report_json(&report, &dispositions, Some(&corpus), &opts);

    // ═══ STEP 3: the finding survives as a CURATED finding — High, right file:line, grounded ═══
    // citation (CWE-862 + OWASP ASVS V4 + the Supabase docs — the SAME corpus rule this
    // hardening cycle grounded — not a bare "preview, not corpus-documented" label).
    let group = json
        .curated_findings
        .iter()
        .find(|g| g.rule_id == RULE_PRIVILEGED_NO_AUTHZ)
        .expect(
            "SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1 must appear in curated_findings — this is \
             what the hold-out hardening cycle closes",
        );
    assert_eq!(group.sites.len(), 1, "{:?}", group.sites);
    let site = &group.sites[0];
    assert_eq!(site.path, FN_FILE);
    assert_eq!(site.line, FN_LINE);
    assert_eq!(site.severity, "high", "{site:?}");
    // M4 (mirrors `search_path_executor_e2e.rs`): an uncalibrated High-severity deterministic
    // finding (no `Finding::effort`) recommends "Do next", not "Do now" — `matrix_bucket` only
    // routes High to `do_now` when `effort == Some("low")`, and a deterministic architectural
    // finding is never calibrated.
    assert_eq!(
        site.disposition, "Open (recommended: Do next)",
        "no disposition was supplied -> stays Open, recommending its computed matrix bucket"
    );
    assert_eq!(
        group.citation.kind, "grounded",
        "SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1 carries real citations (CWE-862, OWASP ASVS, and \
         Supabase's own docs) in the bundled corpus: {:?}",
        group.citation
    );
    assert!(!group.citation.sources.is_empty());
    assert!(
        group
            .citation
            .sources
            .iter()
            .any(|s| s.url.contains("cwe.mitre.org")),
        "expected a CWE-862 citation: {:?}",
        group.citation.sources
    );
    assert!(
        group
            .citation
            .sources
            .iter()
            .any(|s| s.url.contains("owasp.org")),
        "expected an OWASP ASVS citation: {:?}",
        group.citation.sources
    );
    assert!(
        group
            .citation
            .sources
            .iter()
            .any(|s| s.url.contains("supabase.com")),
        "expected a Supabase docs citation: {:?}",
        group.citation.sources
    );

    // ═══ The finding must NOT be routed to the informational appendix — it is a High-severity, ═══
    // fully-grounded, actionable defect (mirrors `search_path_executor_e2e.rs`'s identical
    // assertion for its own uncalibrated High-severity finding).
    assert!(
        json.matrix
            .informational
            .iter()
            .all(|f| f.rule_id != RULE_PRIVILEGED_NO_AUTHZ),
        "a High-severity, grounded finding must never land in the informational bucket: {:?}",
        json.matrix.informational
    );
    assert!(
        json.matrix
            .do_next
            .iter()
            .any(|f| f.rule_id == RULE_PRIVILEGED_NO_AUTHZ),
        "an uncalibrated High finding lands in do_next (do_now requires effort == low): {:?}",
        json.matrix.do_next
    );

    // Methodology sanity: nothing was dispositioned FalsePositive, so nothing is excluded.
    assert_eq!(json.methodology.excluded_false_positive, 0);

    // ═══ STEP 4: compile_pdf — gated on typst being on PATH (mirrors ═══
    // dynamic_sql_exec_injection_executor_e2e.rs / weak_randomness_executor_e2e.rs). ═══
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
