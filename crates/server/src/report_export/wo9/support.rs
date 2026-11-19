//! Builders the WO9 export tests share: a small sealed report, decisions of every shape and an
//! edit. Home paths in fixtures use /Users/auditor/....

use camerata_api_types::decisions::{Decision, DecisionState, Scope};
use camerata_api_types::edits::{Edit, EditField};

use crate::onboard::{Finding, SealedReport};

pub const NOW: &str = "2026-11-04T09:00:00Z";

/// One SQL-concatenation finding at `path:line`.
pub fn sql_row(path: &str, line: usize) -> Finding {
    Finding {
        repo: "acme/portal".to_string(),
        path: path.to_string(),
        line,
        rule_id: "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
        severity: "high".to_string(),
        snippet: format!("format!(\"select {{}}\", x{line})"),
        detail: "Raw SQL \u{2014} The query is built from the request.".to_string(),
        ..Finding::default()
    }
}

/// A report of `n` SQL rows in `src/file_<i>.rs`, sealed.
pub fn sealed_of(n: usize) -> SealedReport {
    let mut report = crate::release_gate::test_scan_report();
    report.findings = (0..n)
        .map(|i| sql_row(&format!("src/file_{i}.rs"), 10 + i))
        .collect();
    report.seal()
}

pub fn decision(id: &str, key: &str, state: DecisionState, reason: &str) -> Decision {
    Decision {
        id: id.to_string(),
        key: key.to_string(),
        state,
        reason: reason.to_string(),
        scope: Scope::Site,
        applies_to: Vec::new(),
        except: Vec::new(),
        inherited_from: None,
        decided_by: "SECRET-REVIEWER".to_string(),
        decided_at: NOW.to_string(),
        run_id: "insp-1".to_string(),
        engine_version: "0.0.0-test".to_string(),
        needed_me: None,
        sample_verdict: None,
        confirmed_by_client: None,
        revoked_at: None,
    }
}

/// A decision on the tree node `node_id` covering `keys`: the node record, then one per-site
/// record each, as the decision store writes it.
pub fn node_decision(
    id: &str,
    node_id: &str,
    keys: &[String],
    state: DecisionState,
    reason: &str,
) -> Vec<Decision> {
    let mut out = vec![Decision {
        scope: Scope::Node(node_id.to_string()),
        applies_to: keys.to_vec(),
        ..decision(id, node_id, state.clone(), reason)
    }];
    for (i, key) in keys.iter().enumerate() {
        out.push(Decision {
            id: format!("{id}.{i:05}"),
            inherited_from: Some(id.to_string()),
            ..decision("x", key, state.clone(), reason)
        });
    }
    out
}

pub fn edit_of(n: usize, f: &Finding, field: EditField, new: &str) -> Edit {
    Edit {
        id: format!("edit-{n:06}"),
        key: f.site_key.clone(),
        field,
        old: String::new(),
        new: new.to_string(),
        reason: "SECRET-REASON the wording was wrong".to_string(),
        by: "SECRET-REVIEWER".to_string(),
        at: NOW.to_string(),
        run_id: "insp-1".to_string(),
        fingerprint_at_edit: camerata_api_types::fingerprint::fingerprint(&f.rule_id, &f.snippet),
        stale: false,
        discarded_at: None,
        reapplied_from: None,
    }
}

use std::collections::BTreeMap;

use crate::release_gate::{read_zip_entries, release, ProductPackage};
use crate::report_export::{build_report_json, render_data_json, ReportOptions, ReviewedReport};

pub const STAND_IN_PDF: &[u8] = b"%PDF-1.7 stand-in for the WO9 tests";
pub const STEM: &str = "acme-inspection";

/// A row of another rule, so a report holds more than one rule node.
pub fn layering_row(path: &str, line: usize) -> Finding {
    Finding {
        rule_id: "ARCH-LAYERING-1".to_string(),
        severity: "medium".to_string(),
        snippet: format!("use crate::db::pool{line};"),
        detail: "Layering \u{2014} The handler reaches the database directly.".to_string(),
        ..sql_row(path, line)
    }
}

/// A dependency advisory row (the dependency lane).
pub fn dependency_row() -> Finding {
    Finding {
        repo: "acme/portal".to_string(),
        path: "package.json".to_string(),
        line: 1,
        rule_id: crate::dep_audit::DEP_AUDIT_RULE_ID.to_string(),
        severity: "high".to_string(),
        snippet: "lodash@4.17.0".to_string(),
        detail: "GHSA-test: prototype pollution".to_string(),
        ..Finding::default()
    }
}

/// Three SQL rows, two layering rows and a dependency advisory, sealed.
pub fn mixed_sealed() -> SealedReport {
    let mut report = crate::release_gate::test_scan_report();
    report.findings = vec![
        sql_row("src/a.rs", 10),
        sql_row("src/b.rs", 20),
        sql_row("src/c.rs", 30),
        layering_row("src/d.rs", 40),
        layering_row("src/e.rs", 50),
        dependency_row(),
    ];
    report.seal()
}

/// A table sheet's grid with its header row first: the title and first line above the header of
/// All source rows are removed.
pub fn sheet_table(xlsx: &[u8], sheet: &str) -> Vec<Vec<String>> {
    let grid = crate::release_gate::xlsx_sheet_grid(xlsx, sheet).unwrap_or_else(|e| panic!("{e}"));
    let above = if sheet == "All source rows" { 2 } else { 0 };
    grid.into_iter().skip(above).collect()
}

pub fn key_at(sealed: &SealedReport, path: &str) -> String {
    sealed
        .findings
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("no row in {path}"))
        .site_key
        .clone()
}

pub fn row_at(sealed: &SealedReport, path: &str) -> Finding {
    sealed
        .findings
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("no row in {path}"))
        .clone()
}

/// The decisions the mixed report is reviewed with: a group decision on the two SQL rows of
/// `src/a.rs` and `src/b.rs`, a false positive on `src/e.rs`; and an edit of `src/c.rs`'s headline.
pub fn mixed_review(sealed: &SealedReport) -> (Vec<Decision>, Vec<Edit>) {
    let mut records = node_decision(
        "dec-000001",
        "rule:SEC-NO-RAW-SQL-CONCAT-1",
        &[key_at(sealed, "src/a.rs"), key_at(sealed, "src/b.rs")],
        DecisionState::Confirmed,
        "the same query helper",
    );
    records.push(decision(
        "dec-000002",
        &key_at(sealed, "src/e.rs"),
        DecisionState::FalsePositive,
        "the layer is allowed here",
    ));
    let edits = vec![edit_of(
        1,
        &row_at(sealed, "src/c.rs"),
        EditField::Headline,
        "Query built from input",
    )];
    (records, edits)
}

pub fn client_options() -> ReportOptions {
    ReportOptions {
        client_name: "Example Client Ltd".to_string(),
        ..ReportOptions::default()
    }
}

/// The package the production assembly builds around a stand-in PDF.
pub fn package_of(reviewed: &ReviewedReport, options: &ReportOptions) -> ProductPackage {
    let json = build_report_json(reviewed, None, options);
    let data = render_data_json(&json).expect("data.json renders");
    crate::release_gate::assemble_package(
        reviewed,
        None,
        options,
        STEM,
        (json, data, STAND_IN_PDF.to_vec()),
    )
    .expect("the package assembles")
}

/// The released zip's entries, by name.
pub fn released_entries(
    reviewed: &ReviewedReport,
    options: &ReportOptions,
) -> BTreeMap<String, Vec<u8>> {
    let zip = release(package_of(reviewed, options))
        .unwrap_or_else(|e| panic!("the package must clear the gate: {e}"))
        .bytes;
    read_zip_entries(&zip).expect("a zip")
}
