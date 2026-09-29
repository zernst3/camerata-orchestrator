//! END-TO-END: the deterministic `SEC-NO-QUERY-GRAMMAR-INJECTION-1` checker
//! (`QueryGrammarInjectionChecker`), wired into the brownfield SCAN, and proven to reach the
//! CURATED finding set — AND proven NOT to be hedged/downgraded by a co-present, CLEAN Row
//! Level Security setup. See `docs/plans/2026-09-29_codebase-inspection-hardening.md`, D3.
//!
//! D3's problem statement: a PostgREST `.or()` filter built by string-interpolating
//! request-derived input was previously written up as "validate and type the param" —
//! medium, needs-review, hedged "likely contained by RLS." That is wrong: the user controls
//! the filter GRAMMAR (can add conditions/columns), so this is an INJECTION class at High,
//! and RLS being present elsewhere in the repo does not make it safe to leave. This file
//! drives the checker through the REAL production entry points (`onboard::audit_repos` ->
//! `report_export::build_report_json`), not just the checker's own unit tests, mirroring
//! `weak_randomness_executor_e2e.rs`'s two-suite pattern for D2's sibling checker.
//!
//! FIXTURE: `tests/fixtures/query_grammar_injection_repo/`:
//!   - `src/api/orders.ts` — three functions: `searchOrders` (the planted defect — a
//!     request-derived `.or()` template-literal interpolation), `searchOrdersSafe` (the bound
//!     `.eq()` twin), `searchOrdersStatic` (a static, non-interpolated `.or()` twin).
//!   - `supabase/migrations/0001_init.sql` — enables RLS on the SAME `orders` table with a
//!     real, non-permissive policy (`using (auth.uid() = user_id)`) — genuine, clean RLS
//!     coverage in this repo, present specifically so the second test below can assert the
//!     injection finding's severity/bucket does not move because of it.
//!
//! ZERO API SPEND: `run_ai_review: false` throughout.

use std::collections::HashMap;
use std::path::Path;

use camerata_server::ai_audit::ScanMode;
use camerata_server::onboard::{self, SelectedRule};
use camerata_server::report_export::{self, DispositionWire, ReportOptions};

const RULE_QUERY_GRAMMAR_INJECTION: &str = "SEC-NO-QUERY-GRAMMAR-INJECTION-1";

/// The file + line the fixture's planted `searchOrders` defect sits at. A named constant (not
/// re-derived) so a future fixture edit that shifts this line fails LOUDLY at the assertion
/// rather than silently asserting against whatever line is current.
const ORDERS_FILE: &str = "src/api/orders.ts";
const ORDERS_OR_LINE: usize = 7;

/// Copy the checked-in fixture tree into `dest`, then turn `dest` into a real one-commit git
/// repo so `onboard::capture_audited_ref`'s shell-outs succeed — mirrors
/// `weak_randomness_executor_e2e.rs::stage_fixture_as_git_repo` exactly (duplicated per this
/// repo's existing convention of each e2e test file owning its own fixture-staging helpers).
fn stage_fixture_as_git_repo(dest: &Path) {
    let fixture_root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/query_grammar_injection_repo");
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
        "query-grammar-injection-e2e@camerata.local",
    ]);
    g(&[
        "config",
        "user.name",
        "Camerata Query-Grammar-Injection E2E Test",
    ]);
    g(&["add", "."]);
    g(&[
        "commit",
        "-q",
        "-m",
        "e2e fixture: one grammar-injection .or() + two safe twins + clean RLS elsewhere",
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

fn selected_query_grammar_injection_rule() -> Vec<SelectedRule> {
    vec![SelectedRule {
        id: RULE_QUERY_GRAMMAR_INJECTION.to_string(),
        directive:
            "query-grammar-injection e2e: flag user input interpolated into a filter grammar"
                .to_string(),
        repos: Vec::new(), // project-level: applies to every scanned repo
    }]
}

/// Run the real scan entry point over the fixture, deterministic-only. Note the selected rule
/// set is ONLY `SEC-NO-QUERY-GRAMMAR-INJECTION-1` — deliberately NOT the Supabase RLS rules —
/// because the NOT-HEDGED claim under test is that this finding's severity/bucket does not
/// depend on any RLS-related signal existing anywhere in the pipeline; the `supabase/
/// migrations/0001_init.sql` fixture file establishes RLS as a genuine fact of the REPO
/// regardless of which rules a given scan run happens to select.
async fn run_fixture_scan(repo_dir: &Path, repo_spec: &str) -> onboard::ScanReport {
    let sources = vec![(repo_spec.to_string(), repo_dir.to_path_buf())];
    let selected = selected_query_grammar_injection_rule();
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
async fn deterministic_scan_finds_the_planted_grammar_injection() {
    let repo_spec = "e2e/query-grammar-injection-fixture".to_string();
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo(repo_dir.path());

    let report = run_fixture_scan(repo_dir.path(), &repo_spec).await;

    assert!(
        !report.gated,
        "a local-dir scan is never gated on a GitHub token"
    );
    assert_eq!(report.repos, vec![repo_spec.clone()]);

    // ═══ Exactly the ONE planted finding: `searchOrders`'s interpolated `.or()`. ═══
    // Neither safe twin (`searchOrdersSafe` — bound `.eq()`; `searchOrdersStatic` — a static,
    // non-interpolated `.or()` string) produces a finding — proves the checker isn't just
    // flagging every `.or()` call or every function that touches `orders`.
    let hits: Vec<&onboard::Finding> = report
        .findings
        .iter()
        .filter(|f| f.rule_id == RULE_QUERY_GRAMMAR_INJECTION)
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one SEC-NO-QUERY-GRAMMAR-INJECTION-1 finding (searchOrders only): {:?}",
        report.findings
    );
    let finding = hits[0];

    assert_eq!(
        finding.severity, "high",
        "a query-grammar injection is an injection class at High, never medium/needs-review"
    );
    assert_eq!(finding.repo, repo_spec);
    assert_eq!(
        finding.path, ORDERS_FILE,
        "the finding must be attributed to the file building the filter"
    );
    assert_eq!(
        finding.line, ORDERS_OR_LINE,
        "must point at the interpolated .or() call's line"
    );
    assert!(
        finding.detail.contains("Row Level Security"),
        "the fix guidance must state RLS does not contain this class: {finding:?}"
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
    // Deterministic findings never carry an AI confidence tag or an AI- rule-id prefix — this
    // is the mechanism by which they bypass the AI calibration pass and the P3 citation gate
    // entirely (see `report_export::is_ai_tier`).
    assert!(finding.confidence.is_none());
    assert!(!finding.rule_id.starts_with("AI-"));

    // ═══ Provenance stamp sanity (mirrors weak_randomness_executor_e2e.rs) ═══
    assert!(!report.provenance.audited_refs.is_empty());
    let sha = report.provenance.audited_refs[0]
        .sha
        .as_deref()
        .expect("a real one-commit git repo must yield a SHA");
    assert_eq!(sha.len(), 40);
}

/// The scan-path proof D3 explicitly calls for: proves the checker's output reaches the
/// CURATED finding set through the real report pipeline — not just the checker's own unit
/// tests, and not just `report.findings` (the raw scan output the sibling test above already
/// covers). ALSO the NOT-HEDGED regression: the fixture repo has a genuine, CLEAN Row Level
/// Security setup on the very table the injection targets (`supabase/migrations/
/// 0001_init.sql`), and the finding's severity/bucket must be unaffected by that fact — a
/// grammar injection forges a different query before RLS is ever evaluated, so "RLS probably
/// contains it" is never a valid reason to hold this for review.
#[tokio::test]
async fn grammar_injection_finding_reaches_curated_findings_high_and_not_rls_hedged() {
    let repo_spec = "e2e/query-grammar-injection-fixture".to_string();
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo(repo_dir.path());

    // ═══ STEP 1: the real scan (same entry point the cockpit uses). ═══
    let report = run_fixture_scan(repo_dir.path(), &repo_spec).await;
    let finding = report
        .findings
        .iter()
        .find(|f| f.rule_id == RULE_QUERY_GRAMMAR_INJECTION)
        .expect(
            "the planted grammar-injection finding must be present (see the sibling scan test)",
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
        project_title: "Query-Grammar-Injection E2E Fixture Audit".to_string(),
        prepared_by: "Camerata (automated e2e test)".to_string(),
        executive_summary_override: None,
        ..Default::default()
    };
    let json = report_export::build_report_json(&report, &dispositions, Some(&corpus), &opts);

    // ═══ STEP 3: the finding survives as a CURATED finding — High severity (NOT downgraded ═══
    // to medium/needs-review despite the repo's genuine, clean RLS coverage), right file:line,
    // grounded citation (CWE-943/74 + the PostgREST operators docs, the SAME corpus rule P3
    // already grounded — not a bare "preview, not corpus-documented" label, and not the
    // "AI-advisory, model-inferred" stamp an uncited AI-tier finding would get).
    let group = json
        .curated_findings
        .iter()
        .find(|g| g.rule_id == RULE_QUERY_GRAMMAR_INJECTION)
        .expect(
            "SEC-NO-QUERY-GRAMMAR-INJECTION-1 must appear in curated_findings — this is what D3 closes",
        );
    assert_eq!(group.sites.len(), 1, "{:?}", group.sites);
    let site = &group.sites[0];
    assert_eq!(site.path, ORDERS_FILE);
    assert_eq!(site.line, ORDERS_OR_LINE);
    assert_eq!(
        site.severity, "high",
        "NOT-HEDGED: a clean, co-present RLS setup elsewhere in this repo must not move this \
         finding's severity — it stands on its own as an injection class: {site:?}"
    );
    assert!(site.detail.contains("Row Level Security"));
    // M4: an uncalibrated high-severity finding recommends "Do next" (never buried as
    // informational — `is_informational` hard-exempts critical/high regardless of family, and
    // never held for review by the P3 citation gate, which only ever applies to AI-tier
    // findings — see `report_export::is_ai_tier`/`is_uncited_ai_finding`).
    assert_eq!(
        site.disposition, "Open (recommended: Do next)",
        "no disposition was supplied -> stays Open, recommending its computed matrix bucket; \
         NOT held as needs-review"
    );
    assert_eq!(
        group.citation.kind, "grounded",
        "SEC-NO-QUERY-GRAMMAR-INJECTION-1 carries real citations (CWE-943, CWE-74, and the \
         PostgREST operators docs) in the bundled corpus — resolve_citation must prefer that \
         over the bare preview label: {:?}",
        group.citation
    );
    assert!(!group.citation.sources.is_empty());
    assert!(
        group
            .citation
            .sources
            .iter()
            .any(|s| s.url.contains("cwe.mitre.org")),
        "expected a CWE-943/74 citation: {:?}",
        group.citation.sources
    );
    assert!(
        group
            .citation
            .sources
            .iter()
            .any(|s| s.url.contains("postgrest.org")),
        "expected the PostgREST operators-docs citation: {:?}",
        group.citation.sources
    );

    // ═══ The finding must NOT be routed to the informational appendix (Bug 4's bucketing, or ═══
    // P3's citation gate) — it is a High-severity, fully-grounded, actionable defect, and the
    // repo's co-present clean RLS setup must not change that.
    assert!(
        json.matrix
            .informational
            .iter()
            .all(|f| f.rule_id != RULE_QUERY_GRAMMAR_INJECTION),
        "a High-severity, grounded finding must never land in the informational bucket, RLS or \
         no RLS: {:?}",
        json.matrix.informational
    );

    // Methodology sanity: nothing was dispositioned FalsePositive, so nothing is excluded.
    assert_eq!(json.methodology.excluded_false_positive, 0);

    // ═══ STEP 4: compile_pdf — gated on typst being on PATH (mirrors weak_randomness_executor_e2e.rs). ═══
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
