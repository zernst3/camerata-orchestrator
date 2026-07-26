//! END-TO-END: the deterministic-scan → PDF-report pipeline, driven through the REAL
//! entry points a manual test would exercise — no stubbing of the scan, the finding
//! classification, the report serializer, or (when available) the Typst compiler.
//!
//! This is the automated backstop for the brownfield audit-report feature
//! (`docs/design/2026-07-23_brownfield-audit-report.md`, Pass A + Pass B): Zach tests the
//! cockpit manually; this suite proves the same spine works headlessly in CI.
//!
//! ZERO API SPEND: the AI-audit tier (semantic/architectural findings, requiring a live
//! LLM call) is deliberately NOT exercised here — `run_ai_review: false` skips every model
//! call. That tier is covered by `crates/server/src/ai_audit.rs`'s own unit tests (which
//! mock the model). This test drives the OTHER half: the always-on, token-free
//! deterministic security floor (`onboard::audit_repos` with `run_deterministic: true`) →
//! its `ScanReport` (with a real provenance stamp) → the auditor's triage dispositions →
//! `report_export::build_report_json` → (gated on `typst` being on PATH)
//! `report_export::compile_pdf`.
//!
//! FIXTURE: `tests/fixtures/e2e_report_repo/` is a tiny, committed, non-workspace-member
//! "repo" (see its `Cargo.toml`'s own doc comment) with THREE deliberately-planted
//! deterministic-floor violations, one per file, chosen to hit three DIFFERENT
//! `onboard::AUDIT_RULES` arms (so the report exercises FalsePositive exclusion, Ignored
//! accepted-risk, and a still-open finding all from real scanner output, not
//! hand-constructed `Finding` values):
//!   - `src/config.rs`  → SEC-NO-HARDCODED-SECRETS-1  (dispositioned FalsePositive)
//!   - `src/db.rs`      → SEC-NO-RAW-SQL-CONCAT-1     (dispositioned Ignored, with a reason)
//!   - `src/client.rs`  → ARCH-NO-SECRETS-IN-URL-1    (left Unresolved / "Open")
//!
//! The fixture directory itself carries NO `.git` (a nested `.git` inside this repo's
//! working tree is awkward to commit cleanly — git does not track nested repos well).
//! Instead the test COPIES the fixture into a fresh temp dir and `git init`s it there,
//! mirroring the same shell-out pattern `onboard::capture_audited_ref` itself relies on
//! (`workspace.rs`'s `init_repo_with_initial_commit` / ad-hoc test helpers use the
//! identical init → config user.email/name → add → commit sequence).

use std::collections::HashMap;
use std::path::Path;

use camerata_server::ai_audit::ScanMode;
use camerata_server::onboard::{self, AUDIT_RULES};
use camerata_server::report_export::{self, DispositionWire, ReportOptions};

/// Copy the checked-in fixture tree into `dest` (a fresh temp dir), then turn `dest` into
/// a real one-commit git repo so `onboard::capture_audited_ref`'s `git rev-parse HEAD` /
/// `--abbrev-ref HEAD` / `status --porcelain` shell-outs all succeed — the scan's
/// provenance stamp needs a real SHA, not a `None`.
fn stage_fixture_as_git_repo(dest: &Path) {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/e2e_report_repo");
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
    g(&["config", "user.email", "e2e-report-pipeline@camerata.local"]);
    g(&["config", "user.name", "Camerata E2E Test"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "e2e fixture: three planted deterministic findings"]);
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
            std::fs::copy(&from, &to).unwrap_or_else(|e| {
                panic!("failed to copy fixture file {from:?} -> {to:?}: {e}")
            });
        }
    }
}

/// Best-effort `typst` presence check (mirrors `report_export`'s own internal test gate) —
/// the PDF-compile stage of this test skips gracefully rather than hard-failing CI
/// environments that don't have Typst installed.
fn typst_on_path() -> bool {
    std::process::Command::new("typst")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some()
}

#[tokio::test]
async fn e2e_deterministic_scan_to_pdf_report_pipeline() {
    let repo_spec = "e2e/report-fixture".to_string();
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo(repo_dir.path());

    let sources = vec![(repo_spec.clone(), repo_dir.path().to_path_buf())];

    // The provenance stamp's `audited_rule_ids` comes from `selected`, NOT from
    // `proposed_rules` (see `onboard::ScanProvenance::audited_rule_ids`'s doc comment).
    // For a deterministic-only scan the semantically meaningful "what did this run
    // audit" answer IS the floor's own rule set — every `AUDIT_RULES` id project-level
    // (empty `repos` => applies to every scanned repo).
    let selected: Vec<onboard::SelectedRule> = AUDIT_RULES
        .iter()
        .map(|id| onboard::SelectedRule {
            id: id.to_string(),
            directive: format!("Deterministic floor rule audited by the e2e pipeline test: {id}"),
            repos: Vec::new(),
        })
        .collect();

    // ═══ STEP 1: run the REAL scan entry point (deterministic-only: zero API spend). ═══
    let (report, _manifest) = onboard::audit_repos(
        &sources,
        &selected,
        Vec::new(),   // extra_notes
        None,         // model — unused, run_ai_review is false
        None,         // calibration_model — unused
        ScanMode::Sequential,
        false,        // thorough — unused (no AI review)
        None,         // feedback
        None,         // job
        None,         // incremental_prior — full scan
        false,        // deep — the opt-in AI compliance tier stays off
        false,        // soc2_enabled — irrelevant with deep off
        false,        // run_ai_review — THE zero-spend switch: no model calls at all
        true,         // run_deterministic — the always-on security floor DOES run
        None,         // ledger
    )
    .await;

    assert!(!report.gated, "a local-dir scan is never gated on a GitHub token");
    assert_eq!(report.repos, vec![repo_spec.clone()]);
    // 5 auditable files: Cargo.toml (a CODE_EXTS "toml") + 4 .rs files.
    assert_eq!(
        report.files_scanned, 5,
        "expected exactly the 5 fixture files to be read: {:?}",
        report.findings.iter().map(|f| &f.path).collect::<Vec<_>>()
    );

    // ═══ STEP 2: the scan produced the THREE planted deterministic findings, and the ═══
    // provenance stamp is populated — a real SHA (Pass A's `capture_audited_ref` ran
    // `git rev-parse HEAD` against a real one-commit repo) and a non-empty
    // `audited_rule_ids` (proving the selection-to-provenance wiring, not just a serde
    // round-trip of a hand-built `ScanProvenance`).
    let find_one = |rule_id: &str| -> &onboard::Finding {
        let matches: Vec<&onboard::Finding> =
            report.findings.iter().filter(|f| f.rule_id == rule_id).collect();
        assert_eq!(
            matches.len(),
            1,
            "expected exactly one {rule_id} finding, got {matches:?} (all findings: {:?})",
            report.findings
        );
        matches[0]
    };
    let secrets_finding = find_one("SEC-NO-HARDCODED-SECRETS-1");
    assert_eq!(secrets_finding.path, "src/config.rs");
    let sql_finding = find_one("SEC-NO-RAW-SQL-CONCAT-1");
    assert_eq!(sql_finding.path, "src/db.rs");
    let url_finding = find_one("ARCH-NO-SECRETS-IN-URL-1");
    assert_eq!(url_finding.path, "src/client.rs");

    let provenance = &report.provenance;
    assert_eq!(provenance.audited_refs.len(), 1);
    let audited_ref = &provenance.audited_refs[0];
    assert_eq!(audited_ref.repo, repo_spec);
    let sha = audited_ref
        .sha
        .as_deref()
        .expect("a real one-commit git repo must yield a SHA from `git rev-parse HEAD`");
    assert_eq!(sha.len(), 40, "a full git SHA is 40 hex chars, got {sha:?}");
    assert!(!audited_ref.dirty, "nothing was left uncommitted after `git commit`");
    assert!(
        !provenance.audited_rule_ids.is_empty(),
        "audited_rule_ids must be populated from `selected`"
    );
    assert_eq!(provenance.audited_rule_ids, AUDIT_RULES.to_vec());
    assert!(
        !provenance.camerata_version.is_empty(),
        "camerata_version must be stamped"
    );

    // ═══ STEP 3: build the auditor's triage dispositions, keyed by the REAL finding_key. ═══
    let mut dispositions: HashMap<String, DispositionWire> = HashMap::new();
    dispositions.insert(
        report_export::finding_key(secrets_finding),
        DispositionWire {
            state: "FalsePositive".to_string(),
            reason: "planted test fixture, not a live credential".to_string(),
            bucket: String::new(),
        },
    );
    dispositions.insert(
        report_export::finding_key(sql_finding),
        DispositionWire {
            state: "Ignored".to_string(),
            reason: "internal-only query path, parameterization ticketed separately".to_string(),
            bucket: String::new(),
        },
    );
    // `url_finding` intentionally gets NO entry — it stays Unresolved/"Open".

    // ═══ STEP 4: build_report_json — the serializer, over the REAL scan report. ═══
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, corpus_errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        corpus_errors.is_empty(),
        "the bundled rule corpus must load cleanly: {corpus_errors:?}"
    );

    let opts = ReportOptions {
        client_name: "Acme Corp".to_string(),
        project_title: "E2E Report Pipeline Fixture Audit".to_string(),
        prepared_by: "Camerata (automated e2e test)".to_string(),
        executive_summary_override: None,
    };
    let json = report_export::build_report_json(&report, &dispositions, Some(&corpus), &opts);

    // FalsePositive: excluded from EVERY section, counted once in methodology.
    assert_eq!(json.methodology.excluded_false_positive, 1);
    assert_eq!(json.executive_summary.excluded_false_positive, 1);
    assert!(
        !json
            .curated_findings
            .iter()
            .any(|g| g.rule_id == "SEC-NO-HARDCODED-SECRETS-1"),
        "the FalsePositive-dispositioned finding must not appear in curated findings: {:?}",
        json.curated_findings
    );
    assert!(
        json.matrix
            .do_now
            .iter()
            .chain(&json.matrix.do_next)
            .chain(&json.matrix.plan)
            .chain(&json.matrix.accepted)
            .all(|f| f.rule_id != "SEC-NO-HARDCODED-SECRETS-1"),
        "the FalsePositive finding must not land in any matrix cell"
    );

    // Ignored: appears as accepted-risk, in the matrix's `accepted` cell.
    let sql_group = json
        .curated_findings
        .iter()
        .find(|g| g.rule_id == "SEC-NO-RAW-SQL-CONCAT-1")
        .expect("the Ignored finding's rule group must still be present");
    assert_eq!(sql_group.sites.len(), 1);
    assert_eq!(
        sql_group.sites[0].disposition,
        "Accepted risk: internal-only query path, parameterization ticketed separately"
    );
    assert!(
        json.matrix.accepted.iter().any(|f| f.path == "src/db.rs"),
        "the Ignored finding must land in the matrix's accepted cell: {:?}",
        json.matrix.accepted
    );

    // Unresolved: still "Open", and the citation join resolved a REAL corpus rule id to
    // its cited sources (ARCH-NO-SECRETS-IN-URL-1 is a grounded universal rule — see
    // crates/rules/principles/universal/arch-no-secrets-in-url-1.toml).
    let url_group = json
        .curated_findings
        .iter()
        .find(|g| g.rule_id == "ARCH-NO-SECRETS-IN-URL-1")
        .expect("the Unresolved finding's rule group must be present");
    assert_eq!(url_group.sites.len(), 1);
    assert_eq!(url_group.sites[0].disposition, "Open");
    assert_eq!(
        url_group.citation.kind, "grounded",
        "ARCH-NO-SECRETS-IN-URL-1 is grounded in the bundled corpus: {:?}",
        url_group.citation
    );
    assert!(
        url_group
            .citation
            .sources
            .iter()
            .any(|s| s.url.contains("cwe.mitre.org") || s.url.contains("rfc-editor.org")),
        "expected a CWE/RFC citation for ARCH-NO-SECRETS-IN-URL-1, got: {:?}",
        url_group.citation.sources
    );

    // Cover carries the provenance SHA through to the report-ready shape.
    let cover_ref = json
        .cover
        .audited_refs
        .iter()
        .find(|r| r.repo == repo_spec)
        .expect("the cover must carry this repo's audited ref");
    assert_eq!(cover_ref.sha.as_deref(), Some(sha));
    assert_eq!(cover_ref.short_sha.as_deref(), Some(&sha[..7]));

    // ═══ STEP 5: compile_pdf — gated on `typst` being on PATH; skip, don't fail, if absent. ═══
    if !typst_on_path() {
        eprintln!(
            "e2e_deterministic_scan_to_pdf_report_pipeline: skipping the PDF-compile \
             assertion — `typst` is not on PATH in this environment"
        );
        return;
    }
    let pdf = report_export::compile_pdf(&json)
        .await
        .expect("compile_pdf must succeed against a real report when typst is installed");
    assert!(!pdf.is_empty(), "the compiled PDF must be non-empty");
    assert!(
        pdf.starts_with(b"%PDF"),
        "the compiled output must start with the PDF magic bytes"
    );
}
