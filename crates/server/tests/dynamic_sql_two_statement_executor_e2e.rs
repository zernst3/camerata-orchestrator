//! END-TO-END: `SUPABASE-FUNC-DYNAMIC-SQL-INJECTION-1`'s intra-function DATAFLOW extension —
//! the build-then-EXECUTE idiom (`v_sql := '...' || p; execute v_sql;`), wired into the
//! brownfield SCAN, and proven to reach the CURATED finding set as CRITICAL (driven by a
//! co-present `GRANT EXECUTE ... TO anon`) through the real report pipeline, not just the
//! checker's own unit tests. Mirrors `dynamic_sql_exec_injection_executor_e2e.rs`'s two-suite
//! pattern for the SAME rule id's pre-existing inline shape.
//!
//! This closes the real detection gap `dynamic_sql_exec_checker.rs` shipped with: it only
//! recognized dynamic SQL assembled INLINE at the `EXECUTE` site. The build-then-EXECUTE idiom
//! (the query string assembled into a local variable first, then `EXECUTE`d by bare reference)
//! is arguably the MORE common real-world shape and was entirely invisible before this change.
//!
//! FIXTURE: `tests/fixtures/dynamic_sql_two_statement_repo/supabase/migrations/0001_fn.sql`:
//!   - `public.search_users(p text)` — the planted defect: `v_sql := '...' || p; execute
//!     v_sql;`, plus a `GRANT EXECUTE ... TO anon` on the SAME function in the SAME file, so the
//!     finding must land CRITICAL (not High).
//!   - `public.safe_l` / `public.safe_using` / `public.safe_literal_concat` /
//!     `public.safe_never_executed` — four safe twins (a `%L`-built variable, a static string
//!     bound via `USING`, a literal-only concatenation, and an unsafely-built variable that is
//!     never `EXECUTE`d at all) proving the checker discriminates by MECHANISM, not merely by
//!     containing the word `EXECUTE`.
//!
//! ZERO API SPEND: `run_ai_review: false` throughout.

use std::collections::HashMap;
use std::path::Path;

use camerata_server::ai_audit::ScanMode;
use camerata_server::onboard::{self, SelectedRule};
use camerata_server::report_export::{self, DispositionWire, ReportOptions};

const RULE_DYNAMIC_SQL_EXEC_INJECTION: &str = "SUPABASE-FUNC-DYNAMIC-SQL-INJECTION-1";

/// The file + line the fixture's planted `search_users` defect sits at (the `CREATE FUNCTION`
/// statement's own start line). A named constant (not re-derived) so a future fixture edit
/// that shifts this line fails LOUDLY at the assertion rather than silently asserting against
/// whatever line is current.
const FN_FILE: &str = "supabase/migrations/0001_fn.sql";
const FN_LINE: usize = 6;

/// Copy the checked-in fixture tree into `dest`, then turn `dest` into a real one-commit git
/// repo so `onboard::capture_audited_ref`'s shell-outs succeed — mirrors
/// `dynamic_sql_exec_injection_executor_e2e.rs::stage_fixture_as_git_repo` exactly (duplicated
/// per this repo's existing convention of each e2e test file owning its own fixture-staging
/// helpers).
fn stage_fixture_as_git_repo(dest: &Path) {
    let fixture_root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dynamic_sql_two_statement_repo");
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
        "dynamic-sql-two-statement-e2e@camerata.local",
    ]);
    g(&[
        "config",
        "user.name",
        "Camerata Dynamic-SQL-Two-Statement E2E Test",
    ]);
    g(&["add", "."]);
    g(&[
        "commit",
        "-q",
        "-m",
        "e2e fixture: one build-then-EXECUTE dynamic-SQL injection (with anon grant) + four safe twins",
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

fn selected_dynamic_sql_exec_injection_rule() -> Vec<SelectedRule> {
    vec![SelectedRule {
        id: RULE_DYNAMIC_SQL_EXEC_INJECTION.to_string(),
        directive: "dynamic-sql-two-statement e2e: flag a function that assembles a \
                    non-quoted dynamic query string into a local variable, then EXECUTEs \
                    that variable by bare reference"
            .to_string(),
        repos: Vec::new(), // project-level: applies to every scanned repo
    }]
}

/// Run the real scan entry point over the fixture, deterministic-only.
async fn run_fixture_scan(repo_dir: &Path, repo_spec: &str) -> onboard::ScanReport {
    let sources = vec![(repo_spec.to_string(), repo_dir.to_path_buf())];
    let selected = selected_dynamic_sql_exec_injection_rule();
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
async fn deterministic_scan_finds_the_planted_two_statement_dynamic_sql_injection() {
    let repo_spec = "e2e/dynamic-sql-two-statement-fixture".to_string();
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo(repo_dir.path());

    let report = run_fixture_scan(repo_dir.path(), &repo_spec).await;

    assert!(
        !report.gated,
        "a local-dir scan is never gated on a GitHub token"
    );
    assert_eq!(report.repos, vec![repo_spec.clone()]);

    // ═══ Exactly the ONE planted finding: `search_users`'s build-then-EXECUTE variable. ═══
    // None of the four safe twins (`safe_l`, `safe_using`, `safe_literal_concat`,
    // `safe_never_executed`) produce a finding — proves the dataflow scan discriminates by
    // MECHANISM, not just by containing the word EXECUTE or a local variable.
    let hits: Vec<&onboard::Finding> = report
        .findings
        .iter()
        .filter(|f| f.rule_id == RULE_DYNAMIC_SQL_EXEC_INJECTION)
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one SUPABASE-FUNC-DYNAMIC-SQL-INJECTION-1 finding (search_users only): {:?}",
        report.findings
    );
    let finding = hits[0];

    assert_eq!(
        finding.severity, "critical",
        "a co-present GRANT EXECUTE ... TO anon on the SAME function must drive Critical, not High"
    );
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
        finding.detail.contains("local variable"),
        "the message must name the dataflow mechanism: {finding:?}"
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

/// The scan-path proof: proves the dataflow-detected finding reaches the CURATED finding set
/// through the real report pipeline — not just `report.findings` (the sibling test above) —
/// at Critical, with the SAME grounded citation (CWE-89 + the PostgreSQL docs) the pre-existing
/// inline shape already carries, since both shapes answer the SAME rule id.
#[tokio::test]
async fn two_statement_finding_reaches_curated_findings_critical_and_grounded() {
    let repo_spec = "e2e/dynamic-sql-two-statement-fixture".to_string();
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo(repo_dir.path());

    // ═══ STEP 1: the real scan (same entry point the cockpit uses). ═══
    let report = run_fixture_scan(repo_dir.path(), &repo_spec).await;
    let finding = report
        .findings
        .iter()
        .find(|f| f.rule_id == RULE_DYNAMIC_SQL_EXEC_INJECTION)
        .expect(
            "the planted two-statement dynamic-SQL injection finding must be present (see the \
             sibling scan test)",
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
        project_title: "Dynamic-SQL-Two-Statement E2E Fixture Audit".to_string(),
        prepared_by: "Camerata (automated e2e test)".to_string(),
        executive_summary_override: None,
        ..Default::default()
    };
    let json = report_export::build_report_json(&report, &dispositions, Some(&corpus), &opts);

    // ═══ STEP 3: the finding survives as a CURATED finding — Critical, right file:line, ═══
    // grounded citation.
    let group = json
        .curated_findings
        .iter()
        .find(|g| g.rule_id == RULE_DYNAMIC_SQL_EXEC_INJECTION)
        .expect("SUPABASE-FUNC-DYNAMIC-SQL-INJECTION-1 must appear in curated_findings");
    assert_eq!(group.sites.len(), 1, "{:?}", group.sites);
    let site = &group.sites[0];
    assert_eq!(site.path, FN_FILE);
    assert_eq!(site.line, FN_LINE);
    assert_eq!(
        site.severity, "critical",
        "the anon grant must drive this to Critical: {site:?}"
    );
    assert_eq!(
        site.disposition, "Open (recommended: Do now)",
        "no disposition was supplied -> stays Open, recommending Do now for a Critical finding"
    );
    assert_eq!(group.citation.kind, "grounded");
    assert!(!group.citation.sources.is_empty());
    assert!(
        group
            .citation
            .sources
            .iter()
            .any(|s| s.url.contains("cwe.mitre.org")),
        "expected a CWE-89 citation: {:?}",
        group.citation.sources
    );

    // ═══ The finding must land in do_now (Critical is unconditionally do_now), never the ═══
    // informational appendix.
    assert!(
        json.matrix
            .informational
            .iter()
            .all(|f| f.rule_id != RULE_DYNAMIC_SQL_EXEC_INJECTION),
        "a Critical, grounded finding must never land in the informational bucket: {:?}",
        json.matrix.informational
    );
    assert!(
        json.matrix
            .do_now
            .iter()
            .any(|f| f.rule_id == RULE_DYNAMIC_SQL_EXEC_INJECTION),
        "a Critical finding must land in do_now: {:?}",
        json.matrix.do_now
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
