//! END-TO-END: the deterministic `SUPABASE-FUNC-DYNAMIC-SQL-INJECTION-1` checker
//! (`DynamicSqlExecInjectionChecker`), wired into the brownfield SCAN, and proven to reach the
//! CURATED finding set as CRITICAL (driven by a co-present `GRANT EXECUTE ... TO anon`) — not
//! just the checker's own unit tests. Mirrors `query_grammar_injection_executor_e2e.rs`'s
//! two-suite pattern for the sibling checker this one closes the hold-out gap next to.
//!
//! This closes the ONE detection miss found on an unseen hold-out repo: a SQL/plpgsql function
//! whose body `EXECUTE`s a dynamically-assembled query string was invisible to every existing
//! checker (`SEC-NO-QUERY-GRAMMAR-INJECTION-1` only scans APPLICATION-language source files,
//! never `.sql`, and has no concept of a function body at all).
//!
//! FIXTURE: `tests/fixtures/dynamic_sql_exec_injection_repo/supabase/migrations/0001_fn.sql`:
//!   - `public.run_report(p text)` — the planted defect: `EXECUTE format('... %s ...', p)`,
//!     plus a `GRANT EXECUTE ... TO anon` on the SAME function in the SAME file, so the finding
//!     must land CRITICAL (not High).
//!   - `public.safe_l` / `public.safe_i` / `public.safe_using` / `public.safe_static` — four
//!     safe twins (the escaping `%L`/`%I` placeholders, a bound `USING` argument, and a fully
//!     static query) proving the checker discriminates by MECHANISM, not by merely containing
//!     the word `EXECUTE` or touching the same table.
//!
//! ZERO API SPEND: `run_ai_review: false` throughout.

use std::collections::HashMap;
use std::path::Path;

use camerata_server::ai_audit::ScanMode;
use camerata_server::onboard::{self, SelectedRule};
use camerata_server::report_export::{self, DispositionWire, ReportOptions};

const RULE_DYNAMIC_SQL_EXEC_INJECTION: &str = "SUPABASE-FUNC-DYNAMIC-SQL-INJECTION-1";

/// The file + line the fixture's planted `run_report` defect sits at (the `CREATE FUNCTION`
/// statement's own start line — see `dynamic_sql_exec_checker`'s attribution, which mirrors
/// `search_path_checker`'s own "attribute to the establishing statement" convention). A named
/// constant (not re-derived) so a future fixture edit that shifts this line fails LOUDLY at the
/// assertion rather than silently asserting against whatever line is current.
const FN_FILE: &str = "supabase/migrations/0001_fn.sql";
const FN_LINE: usize = 4;

/// Copy the checked-in fixture tree into `dest`, then turn `dest` into a real one-commit git
/// repo so `onboard::capture_audited_ref`'s shell-outs succeed — mirrors
/// `query_grammar_injection_executor_e2e.rs::stage_fixture_as_git_repo` exactly (duplicated per
/// this repo's existing convention of each e2e test file owning its own fixture-staging
/// helpers).
fn stage_fixture_as_git_repo(dest: &Path) {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/dynamic_sql_exec_injection_repo");
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
        "dynamic-sql-exec-injection-e2e@camerata.local",
    ]);
    g(&[
        "config",
        "user.name",
        "Camerata Dynamic-SQL-Exec-Injection E2E Test",
    ]);
    g(&["add", "."]);
    g(&[
        "commit",
        "-q",
        "-m",
        "e2e fixture: one dynamic-SQL-exec injection (with anon grant) + four safe twins",
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
        directive: "dynamic-sql-exec-injection e2e: flag a function EXECUTE-ing a \
                    non-quoted, dynamically-assembled query string"
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
async fn deterministic_scan_finds_the_planted_dynamic_sql_injection() {
    let repo_spec = "e2e/dynamic-sql-exec-injection-fixture".to_string();
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo(repo_dir.path());

    let report = run_fixture_scan(repo_dir.path(), &repo_spec).await;

    assert!(
        !report.gated,
        "a local-dir scan is never gated on a GitHub token"
    );
    assert_eq!(report.repos, vec![repo_spec.clone()]);

    // ═══ Exactly the ONE planted finding: `run_report`'s unsafe `format()` %s call. ═══
    // None of the four safe twins (`safe_l`, `safe_i`, `safe_using`, `safe_static`) produce a
    // finding — proves the checker isn't just flagging every function containing `EXECUTE`.
    let hits: Vec<&onboard::Finding> = report
        .findings
        .iter()
        .filter(|f| f.rule_id == RULE_DYNAMIC_SQL_EXEC_INJECTION)
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one SUPABASE-FUNC-DYNAMIC-SQL-INJECTION-1 finding (run_report only): {:?}",
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
        finding.detail.to_ascii_lowercase().contains("format()"),
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

    // ═══ Provenance stamp sanity (mirrors query_grammar_injection_executor_e2e.rs) ═══
    assert!(!report.provenance.audited_refs.is_empty());
    let sha = report.provenance.audited_refs[0]
        .sha
        .as_deref()
        .expect("a real one-commit git repo must yield a SHA");
    assert_eq!(sha.len(), 40);
}

/// The scan-path proof: proves the checker's output reaches the CURATED finding set through the
/// real report pipeline — not just the checker's own unit tests, and not just `report.findings`
/// (the raw scan output the sibling test above already covers) — at Critical, with a grounded
/// citation (CWE-89 + the PostgreSQL dynamic-SQL/format() docs).
#[tokio::test]
async fn dynamic_sql_injection_finding_reaches_curated_findings_critical_and_grounded() {
    let repo_spec = "e2e/dynamic-sql-exec-injection-fixture".to_string();
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo(repo_dir.path());

    // ═══ STEP 1: the real scan (same entry point the cockpit uses). ═══
    let report = run_fixture_scan(repo_dir.path(), &repo_spec).await;
    let finding = report
        .findings
        .iter()
        .find(|f| f.rule_id == RULE_DYNAMIC_SQL_EXEC_INJECTION)
        .expect(
            "the planted dynamic-SQL injection finding must be present (see the sibling scan test)",
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
        project_title: "Dynamic-SQL-Exec-Injection E2E Fixture Audit".to_string(),
        prepared_by: "Camerata (automated e2e test)".to_string(),
        executive_summary_override: None,
        ..Default::default()
    };
    let json = report_export::build_report_json(&report, &dispositions, Some(&corpus), &opts);

    // ═══ STEP 3: the finding survives as a CURATED finding — Critical, right file:line, ═══
    // grounded citation (CWE-89 + the PostgreSQL docs, the SAME corpus rule this hardening
    // cycle grounded — not a bare "preview, not corpus-documented" label).
    let group = json
        .curated_findings
        .iter()
        .find(|g| g.rule_id == RULE_DYNAMIC_SQL_EXEC_INJECTION)
        .expect(
            "SUPABASE-FUNC-DYNAMIC-SQL-INJECTION-1 must appear in curated_findings — this is \
             what the hold-out hardening cycle closes",
        );
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
    assert_eq!(
        group.citation.kind, "grounded",
        "SUPABASE-FUNC-DYNAMIC-SQL-INJECTION-1 carries real citations (CWE-89 and the \
         PostgreSQL docs) in the bundled corpus: {:?}",
        group.citation
    );
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
    assert!(
        group
            .citation
            .sources
            .iter()
            .any(|s| s.url.contains("postgresql.org")),
        "expected a PostgreSQL docs citation: {:?}",
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

    // Methodology sanity: nothing was dispositioned FalsePositive, so nothing is excluded.
    assert_eq!(json.methodology.excluded_false_positive, 0);

    // ═══ STEP 4: compile_pdf — gated on typst being on PATH (mirrors
    // query_grammar_injection_executor_e2e.rs / weak_randomness_executor_e2e.rs). ═══
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
