//! END-TO-END: `SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1`'s signal-3 FIX — the shared-secret/
//! signature compensating-control signal no longer treats an INCIDENTAL mention of a
//! secret-named identifier (an UPDATE's assignment target, an INSERT column, a RAISE argument)
//! as proof of an authorization check — wired into the brownfield SCAN, and proven to reach the
//! CURATED finding set as HIGH with a grounded citation through the real report pipeline, not
//! just the checker's own unit tests. Mirrors `privileged_no_authz_executor_e2e.rs`'s two-suite
//! pattern for the SAME rule id's pre-existing mechanism.
//!
//! This closes a real false exemption: the original signal 3 treated ANY identifier anywhere
//! in a function body containing `secret`/`token`/`signature` as proof of an authorization
//! check. A mutating, broadly-granted `SECURITY DEFINER` function that merely touches a column
//! named like a secret (revoking a stored API token) was silently exempted and never flagged,
//! even with zero actual authorization check anywhere in its body.
//!
//! FIXTURE: `tests/fixtures/privileged_no_authz_secret_guard_repo/supabase/migrations/0001_fn.sql`:
//!   - `public.revoke_token(p_user_id uuid)` — the planted defect: `SECURITY DEFINER`, an
//!     `UPDATE ... SET api_token = null` write with `api_token` as a bare assignment target (no
//!     real check anywhere), `GRANT EXECUTE ... TO authenticated` on the SAME function.
//!   - `public.handle_webhook` — the compensating-control safe twin: a genuine `WHERE token =
//!     p_token` comparison guard before the write, broadly granted to `anon` — proving the fix
//!     still keeps the real shared-secret-guard shape silent even in the SAME scanned corpus.
//!
//! ZERO API SPEND: `run_ai_review: false` throughout.

use std::collections::HashMap;
use std::path::Path;

use camerata_server::ai_audit::ScanMode;
use camerata_server::onboard::{self, SelectedRule};
use camerata_server::report_export::{self, DispositionWire, ReportOptions};

const RULE_PRIVILEGED_NO_AUTHZ: &str = "SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1";

/// The file + line the fixture's planted `revoke_token` defect sits at (the `CREATE FUNCTION`
/// statement's own start line). A named constant (not re-derived) so a future fixture edit
/// that shifts this line fails LOUDLY at the assertion rather than silently asserting against
/// whatever line is current.
const FN_FILE: &str = "supabase/migrations/0001_fn.sql";
const FN_LINE: usize = 6;

/// Copy the checked-in fixture tree into `dest`, then turn `dest` into a real one-commit git
/// repo so `onboard::capture_audited_ref`'s shell-outs succeed — mirrors
/// `privileged_no_authz_executor_e2e.rs::stage_fixture_as_git_repo` exactly (duplicated per
/// this repo's existing convention of each e2e test file owning its own fixture-staging
/// helpers).
fn stage_fixture_as_git_repo(dest: &Path) {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/privileged_no_authz_secret_guard_repo");
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
        "privileged-no-authz-secret-guard-e2e@camerata.local",
    ]);
    g(&[
        "config",
        "user.name",
        "Camerata Privileged-No-Authz-Secret-Guard E2E Test",
    ]);
    g(&["add", "."]);
    g(&[
        "commit",
        "-q",
        "-m",
        "e2e fixture: one bare-secret-column false exemption + one genuine secret-guard twin",
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
        directive: "privileged-no-authz-secret-guard e2e: flag a mutating, broadly-granted \
                    SECURITY DEFINER function with no REAL authorization check in its body"
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
async fn deterministic_scan_finds_the_bare_secret_column_assignment_but_not_the_genuine_guard() {
    let repo_spec = "e2e/privileged-no-authz-secret-guard-fixture".to_string();
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo(repo_dir.path());

    let report = run_fixture_scan(repo_dir.path(), &repo_spec).await;

    assert!(
        !report.gated,
        "a local-dir scan is never gated on a GitHub token"
    );
    assert_eq!(report.repos, vec![repo_spec.clone()]);

    // ═══ Exactly the ONE planted finding: `revoke_token`'s bare secret-column assignment. ═══
    // `handle_webhook` (the genuine WHERE-clause secret-comparison guard) produces NO finding —
    // proves the fix discriminates by MECHANISM (a real check) rather than any mention of
    // secret/token/signature anywhere in the body.
    let hits: Vec<&onboard::Finding> = report
        .findings
        .iter()
        .filter(|f| f.rule_id == RULE_PRIVILEGED_NO_AUTHZ)
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1 finding (revoke_token only): {:?}",
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
        finding.detail.contains("revoke_token"),
        "the message must name the offending function: {finding:?}"
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

/// The scan-path proof: proves the finding reaches the CURATED finding set through the real
/// report pipeline — not just `report.findings` (the sibling test above) — at High, with a
/// grounded citation (CWE-862), the SAME corpus rule the sibling `privileged_no_authz_executor_
/// e2e.rs` already grounds.
#[tokio::test]
async fn secret_guard_fix_finding_reaches_curated_findings_high_and_grounded() {
    let repo_spec = "e2e/privileged-no-authz-secret-guard-fixture".to_string();
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo(repo_dir.path());

    // ═══ STEP 1: the real scan (same entry point the cockpit uses). ═══
    let report = run_fixture_scan(repo_dir.path(), &repo_spec).await;
    let finding = report
        .findings
        .iter()
        .find(|f| f.rule_id == RULE_PRIVILEGED_NO_AUTHZ)
        .expect(
            "the planted bare-secret-column finding must be present (see the sibling scan test)",
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
        project_title: "Privileged-No-Authz-Secret-Guard E2E Fixture Audit".to_string(),
        prepared_by: "Camerata (automated e2e test)".to_string(),
        executive_summary_override: None,
        ..Default::default()
    };
    let json = report_export::build_report_json(&report, &dispositions, Some(&corpus), &opts);

    // ═══ STEP 3: the finding survives as a CURATED finding — High, right file:line, ═══
    // grounded citation (CWE-862 — Missing Authorization).
    let group = json
        .curated_findings
        .iter()
        .find(|g| g.rule_id == RULE_PRIVILEGED_NO_AUTHZ)
        .expect("SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1 must appear in curated_findings");
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
            .any(|s| s.url.contains("cwe.mitre.org")),
        "expected a CWE-862 citation: {:?}",
        group.citation.sources
    );

    // ═══ A High finding must land in an action tier (do_now/do_next/plan), never the ═══
    // informational appendix.
    assert!(
        json.matrix
            .informational
            .iter()
            .all(|f| f.rule_id != RULE_PRIVILEGED_NO_AUTHZ),
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
