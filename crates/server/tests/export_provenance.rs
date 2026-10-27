#![allow(clippy::unwrap_used)]
//! The client export's provenance, end to end: what `client_provenance::export_provenance_with`
//! puts in the manifest and README (design: `docs/design/2026-10-20_machine-readable-exports.md`).
//!
//! Three values were `null` in every package before this was wired:
//!
//! - the commit, now one per repo from the scan's recorded git identity;
//! - the corpus version, a content digest of the rule corpus;
//! - the rules-last-reviewed date, the oldest effective review date across the rules the scan
//!   used, which is honestly absent until every one of them has been reviewed.
//!
//! The last is the only freshness-derived value a client may see. These tests also prove that
//! reaching it does not let any operator-only maintenance text into the package, even when the
//! watermark has overdue domains (the case the maintenance nag exists for).

use std::collections::HashMap;
use std::io::Read;

use camerata_rules::{bare_rule, CorpusFreshness, RuleSet, OPERATOR_ONLY_MARKERS};
use camerata_server::client_provenance::export_provenance_with;
use camerata_server::onboard::{AuditedRef, Finding, LanguageVolume, ScanProvenance, ScanReport};
use camerata_server::report_export::{build_report_json, AuditReportJson, ReportOptions};
use camerata_server::{assemble_unreleased_zip as build_product_zip, product_export_readme};

const RULE_A: &str = "SEC-NO-RAW-SQL-CONCAT-1";
const RULE_B: &str = "RUST-DOMAIN-2";

fn report_with_refs(refs: Vec<AuditedRef>, audited: &[&str]) -> ScanReport {
    ScanReport {
        repos: vec!["acme/portal".to_string(), "acme/web".to_string()],
        stacks: Vec::new(),
        files_scanned: 10,
        test_file_count: 0,
        files_excluded: 0,
        code_chars: 5000,
        code_lines: 400,
        code_lines_by_language: vec![LanguageVolume {
            language: "Rust".to_string(),
            lines: 400,
        }],
        excluded_mechanical_rules: Vec::new(),
        findings: vec![Finding {
            repo: "acme/portal".to_string(),
            path: "src/db.rs".to_string(),
            line: 12,
            rule_id: RULE_A.to_string(),
            severity: "high".to_string(),
            snippet: "format!(\"select {}\", x)".to_string(),
            detail: "raw SQL concatenation".to_string(),
            ..Finding::default()
        }],
        proposed_rules: Vec::new(),
        gated: false,
        blocked: false,
        ai_blocked_reason: None,
        ai_error: None,
        message: None,
        actual_usage: None,
        deep: None,
        coverage_notes: Vec::new(),
        provenance: ScanProvenance {
            audited_refs: refs,
            audit_model: None,
            calibration_model: None,
            clone_judge_model: None,
            mode: "parallel".to_string(),
            thorough: false,
            deep: false,
            rules_fingerprint: "fp".to_string(),
            audited_rule_ids: audited.iter().map(ToString::to_string).collect(),
            camerata_version: "0.0.0-test".to_string(),
            osv_scanner_version: None,
            started_at: "2026-10-21T00:00:00Z".to_string(),
            finished_at: "2026-10-21T00:05:00Z".to_string(),
            model_sessions: Vec::new(),
        },
        recommendations: HashMap::new(),
        failed_passes: Vec::new(),
        ledger: camerata_server::scan_ledger::ScanLedger::new(),
    }
}

fn git_ref(repo: &str, sha: Option<&str>, dirty: bool) -> AuditedRef {
    AuditedRef {
        repo: repo.to_string(),
        sha: sha.map(str::to_string),
        branch: Some("main".to_string()),
        dirty,
        ..AuditedRef::default()
    }
}

fn default_report() -> ScanReport {
    report_with_refs(
        vec![git_ref("acme/portal", Some("abcdef1234567890"), false)],
        &[RULE_A, RULE_B],
    )
}

fn cover_of(report: &ScanReport) -> AuditReportJson {
    build_report_json(
        &report.clone().seal(),
        &HashMap::new(),
        None,
        &ReportOptions::default(),
    )
}

/// A corpus with the two audited rules, in the `sec` and `rust` packs.
fn corpus() -> RuleSet {
    let mut set = RuleSet::default();
    for (id, domain) in [(RULE_A, "sec"), (RULE_B, "rust:seaorm")] {
        let mut rule = bare_rule(id, domain);
        rule.version = Some(1);
        set.push(rule);
    }
    set
}

fn watermark(toml: &str) -> CorpusFreshness {
    CorpusFreshness::parse(toml).expect("watermark parses")
}

// ── commit, per repo ──────────────────────────────────────────────────────────────────────

#[test]
fn each_repo_carries_the_commit_the_scan_recorded_and_unrecorded_repos_carry_none() {
    let report = report_with_refs(
        vec![
            git_ref("acme/portal", Some("abcdef1234567890"), false),
            git_ref("acme/web", None, false), // not a git checkout: nothing recorded
        ],
        &[RULE_A],
    );
    let json = cover_of(&report);
    let p = export_provenance_with(&report, &json.cover, None, None);

    assert_eq!(p.repos.len(), 2);
    assert_eq!(p.repos[0].name, "acme/portal");
    assert_eq!(p.repos[0].commit.as_deref(), Some("abcdef1234567890"));
    assert!(!p.repos[0].dirty);
    assert_eq!(p.repos[1].name, "acme/web");
    assert_eq!(
        p.repos[1].commit, None,
        "an unrecorded commit is not guessed"
    );
    assert!(!p.repos[1].dirty);
}

#[test]
fn a_dirty_tree_is_disclosed_next_to_its_commit() {
    let report = report_with_refs(
        vec![
            git_ref("acme/portal", Some("aaaa"), true),
            git_ref("acme/web", Some("bbbb"), false),
        ],
        &[RULE_A],
    );
    let json = cover_of(&report);
    let p = export_provenance_with(&report, &json.cover, None, None);
    assert!(p.repos[0].dirty && !p.repos[1].dirty);

    let readme = product_export_readme("acme", &p);
    assert!(
        readme.contains("Commit (acme/portal): aaaa (the working tree had uncommitted changes)"),
        "{readme}"
    );
    assert!(readme.contains("Commit (acme/web): bbbb\n"), "{readme}");
}

// ── corpus version ────────────────────────────────────────────────────────────────────────

#[test]
fn corpus_version_is_the_corpus_digest_and_absent_without_a_corpus() {
    let report = default_report();
    let json = cover_of(&report);
    let corpus = corpus();

    let with = export_provenance_with(&report, &json.cover, Some(&corpus), None);
    assert_eq!(
        with.corpus_version.as_deref(),
        Some(camerata_rules::corpus_version(&corpus).as_str())
    );
    assert!(with
        .corpus_version
        .as_deref()
        .unwrap()
        .starts_with("sha256:"));

    let without = export_provenance_with(&report, &json.cover, None, None);
    assert_eq!(without.corpus_version, None);
}

// ── rules last reviewed ───────────────────────────────────────────────────────────────────

/// The honest value today: no domain has been confirmed, so no date can be claimed.
#[test]
fn rules_last_reviewed_is_none_while_the_used_domains_are_unconfirmed() {
    let report = default_report();
    let json = cover_of(&report);
    let corpus = corpus();
    let never = watermark("");
    let p = export_provenance_with(&report, &json.cover, Some(&corpus), Some(&never));
    assert_eq!(p.rules_last_reviewed, None);
}

/// Once every domain the scan used is confirmed, the CLIENT is told the oldest of them.
#[test]
fn rules_last_reviewed_is_the_oldest_date_across_the_domains_the_scan_used() {
    let report = default_report();
    let json = cover_of(&report);
    let corpus = corpus();
    let confirmed = watermark("[confirmed]\nsec = \"2026-10-18\"\nrust = \"2026-08-02\"\n");
    let p = export_provenance_with(&report, &json.cover, Some(&corpus), Some(&confirmed));
    assert_eq!(p.rules_last_reviewed.as_deref(), Some("2026-08-02"));

    // A confirmed domain the scan did not use does not pull the date down.
    let only_sec = report_with_refs(Vec::new(), &[RULE_A]);
    let json = cover_of(&only_sec);
    let wider = watermark(
        "[confirmed]\nsec = \"2026-10-18\"\nrust = \"2026-08-02\"\ngo = \"2020-01-01\"\n",
    );
    let p = export_provenance_with(&only_sec, &json.cover, Some(&corpus), Some(&wider));
    assert_eq!(p.rules_last_reviewed.as_deref(), Some("2026-10-18"));
}

#[test]
fn rules_last_reviewed_is_none_when_one_used_domain_is_unconfirmed() {
    let report = default_report();
    let json = cover_of(&report);
    let corpus = corpus();
    // `rust` was never confirmed: the newer `sec` date would flatter the rules.
    let partial = watermark("[confirmed]\nsec = \"2026-10-18\"\n");
    let p = export_provenance_with(&report, &json.cover, Some(&corpus), Some(&partial));
    assert_eq!(p.rules_last_reviewed, None);
}

#[test]
fn rules_last_reviewed_is_none_without_a_watermark_a_corpus_or_a_resolvable_rule_set() {
    let json_report = default_report();
    let json = cover_of(&json_report);
    let corpus = corpus();
    let confirmed = watermark("[confirmed]\nsec = \"2026-10-18\"\nrust = \"2026-08-02\"\n");

    // No watermark readable.
    let p = export_provenance_with(&json_report, &json.cover, Some(&corpus), None);
    assert_eq!(p.rules_last_reviewed, None);
    // No corpus loaded: the used rules cannot be resolved, so nothing can be dated.
    let p = export_provenance_with(&json_report, &json.cover, None, Some(&confirmed));
    assert_eq!(p.rules_last_reviewed, None);
    // The scan audited a rule the loaded corpus does not hold (a custom rule): no date can
    // vouch for it.
    let custom = report_with_refs(Vec::new(), &[RULE_A, "CUSTOM-RULE-1"]);
    let p = export_provenance_with(&custom, &json.cover, Some(&corpus), Some(&confirmed));
    assert_eq!(p.rules_last_reviewed, None);
    // A scan that audited nothing has nothing to date.
    let none = report_with_refs(Vec::new(), &[]);
    let p = export_provenance_with(&none, &json.cover, Some(&corpus), Some(&confirmed));
    assert_eq!(p.rules_last_reviewed, None);
}

// ── nothing operator-facing rides along ───────────────────────────────────────────────────

fn zip_text_entries(zip_bytes: &[u8]) -> Vec<(String, String)> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).expect("valid zip");
    let mut out = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        if let Ok(text) = String::from_utf8(bytes) {
            out.push((entry.name().to_string(), text));
        }
    }
    out
}

/// A watermark with OVERDUE and never-confirmed domains is exactly what the operator nag is
/// written for. The package may carry the one sanctioned date and none of that vocabulary.
#[test]
fn the_sanctioned_date_reaches_the_package_and_no_maintenance_text_comes_with_it() {
    let report = default_report();
    let json = cover_of(&report);
    let corpus = corpus();
    // `sec` and `rust` are confirmed (long ago, with a tiny window, so both are overdue);
    // `go` is a corpus pack nothing confirmed.
    let overdue = watermark(
        "warn_after_days = 1\n[confirmed]\nsec = \"2020-01-02\"\nrust = \"2020-01-01\"\n",
    );
    let provenance = export_provenance_with(&report, &json.cover, Some(&corpus), Some(&overdue));
    assert_eq!(
        provenance.rules_last_reviewed.as_deref(),
        Some("2020-01-01")
    );

    let readme = product_export_readme("acme", &provenance);
    let zip = build_product_zip(
        "acme",
        b"%PDF-1.7 stand-in",
        b"PK stand-in",
        b"{}",
        &readme,
        &[],
        &provenance,
    )
    .expect("zip assembles");

    let entries = zip_text_entries(&zip);
    let manifest = &entries
        .iter()
        .find(|(n, _)| n == "manifest.json")
        .unwrap()
        .1;
    assert!(
        manifest.contains("\"rules_last_reviewed\": \"2020-01-01\""),
        "{manifest}"
    );
    assert!(
        readme.contains("Rules last reviewed on 2020-01-01"),
        "{readme}"
    );

    for (name, text) in &entries {
        for marker in OPERATOR_ONLY_MARKERS {
            assert!(
                !text.contains(marker),
                "{name} contains operator-only text {marker:?}:\n{text}"
            );
        }
    }
}

// ---- model backend and provider (MASTER-SEQUENCE 22a) -----------------------------------------

/// Build the manifest `Value` and README for a provenance, through the real zip assembly.
fn manifest_and_readme(
    provenance: &camerata_server::ManifestProvenance,
) -> (serde_json::Value, String) {
    let readme = product_export_readme("acme", provenance);
    let zip = build_product_zip(
        "acme",
        b"%PDF-1.7 stand-in",
        b"PK stand-in",
        b"{}",
        &readme,
        &[],
        provenance,
    )
    .expect("zip assembles");
    let entries = zip_text_entries(&zip);
    let manifest = &entries
        .iter()
        .find(|(n, _)| n == "manifest.json")
        .unwrap()
        .1;
    (serde_json::from_str(manifest).unwrap(), readme)
}

fn provenance_with(
    model: Option<camerata_server::ModelRecord>,
) -> camerata_server::ManifestProvenance {
    let report = default_report();
    let json = cover_of(&report);
    let mut provenance = export_provenance_with(&report, &json.cover, None, None);
    provenance.model = model;
    provenance
}

#[test]
fn the_manifest_and_readme_carry_the_backend_and_provider() {
    let provenance = provenance_with(Some(camerata_server::ModelRecord::anthropic(
        camerata_server::llm::ProjectBackend::Api,
    )));
    let (manifest, readme) = manifest_and_readme(&provenance);

    assert_eq!(manifest["inspection"]["model_backend"], "api");
    assert_eq!(manifest["inspection"]["model_provider"], "anthropic");
    assert!(readme.contains("Model backend: api\n"), "{readme}");
    assert!(readme.contains("Model provider: anthropic\n"), "{readme}");
    assert!(
        camerata_server::release_gate::verify_readme_provenance(&readme, &manifest).is_empty(),
        "the gate's provenance agreement check passes"
    );
}

#[test]
fn a_future_provider_is_recorded_the_same_way() {
    let provenance = provenance_with(Some(camerata_server::ModelRecord::new(
        camerata_server::llm::ProjectBackend::Api,
        "bedrock",
    )));
    let (manifest, readme) = manifest_and_readme(&provenance);
    assert_eq!(manifest["inspection"]["model_provider"], "bedrock");
    assert!(readme.contains("Model provider: bedrock\n"), "{readme}");
    assert!(camerata_server::release_gate::verify_readme_provenance(&readme, &manifest).is_empty());
}

#[test]
fn the_gate_catches_a_readme_that_disagrees_with_the_manifest_on_the_backend() {
    let provenance = provenance_with(Some(camerata_server::ModelRecord::anthropic(
        camerata_server::llm::ProjectBackend::Api,
    )));
    let (manifest, readme) = manifest_and_readme(&provenance);
    let tampered = readme.replace("Model backend: api", "Model backend: cli");
    let problems = camerata_server::release_gate::verify_readme_provenance(&tampered, &manifest);
    assert!(
        problems
            .iter()
            .any(|p| format!("{p:?}").contains("Model backend")),
        "{problems:?}"
    );
}

#[test]
fn an_unrecorded_backend_is_null_in_the_manifest_and_absent_from_the_readme() {
    let provenance = provenance_with(None);
    let (manifest, readme) = manifest_and_readme(&provenance);
    assert!(manifest["inspection"]["model_backend"].is_null());
    assert!(!readme.contains("Model backend"), "{readme}");
    assert!(camerata_server::release_gate::verify_readme_provenance(&readme, &manifest).is_empty());
}

// ---- the spend limit: the export states THAT the run was capped, never how much ----

#[test]
fn a_capped_run_is_stated_in_the_manifest_and_the_readme_and_they_agree() {
    let mut provenance = provenance_with(None);
    provenance.cost_cap_reached = true;
    let (manifest, readme) = manifest_and_readme(&provenance);
    assert_eq!(manifest["inspection"]["cost_cap_reached"], true);
    assert!(readme.contains("Spend limit: reached"), "{readme}");
    assert!(readme.contains("INCOMPLETE"), "{readme}");
    assert!(camerata_server::release_gate::verify_readme_provenance(&readme, &manifest).is_empty());
    assert!(!manifest.to_string().contains("usd") && !readme.contains('$'));
}

#[test]
fn an_uncapped_run_says_false_in_the_manifest_and_nothing_in_the_readme() {
    let provenance = provenance_with(None);
    let (manifest, readme) = manifest_and_readme(&provenance);
    assert_eq!(manifest["inspection"]["cost_cap_reached"], false);
    assert!(!readme.contains("Spend limit"), "{readme}");
    assert!(camerata_server::release_gate::verify_readme_provenance(&readme, &manifest).is_empty());
}

#[test]
fn the_gate_catches_a_readme_that_hides_or_invents_the_spend_limit() {
    let mut capped = provenance_with(None);
    capped.cost_cap_reached = true;
    let (manifest, readme) = manifest_and_readme(&capped);
    // A README with the incompleteness line removed disagrees with the manifest.
    let hidden: String = readme
        .lines()
        .filter(|l| !l.starts_with("Spend limit"))
        .map(|l| format!("{l}\n"))
        .collect();
    let problems = camerata_server::release_gate::verify_readme_provenance(&hidden, &manifest);
    assert!(
        problems
            .iter()
            .any(|p| format!("{p:?}").contains("Spend limit")),
        "{problems:?}"
    );

    // And one that claims a cap the manifest does not record.
    let (clean_manifest, clean_readme) = manifest_and_readme(&provenance_with(None));
    let invented = clean_readme.replace(
        "Release checks:",
        &format!("{}\nRelease checks:", camerata_server::COST_CAP_README_LINE),
    );
    let problems =
        camerata_server::release_gate::verify_readme_provenance(&invented, &clean_manifest);
    assert!(
        problems
            .iter()
            .any(|p| format!("{p:?}").contains("Spend limit")),
        "{problems:?}"
    );
}
