//! A style linter's rule ships as one Fix, and nothing hides: every place stays in All problems
//! and in `findings.json`, counted in the totals, carrying the group's decision. Synthetic
//! fixtures only.

use camerata_api_types::audit::FindingKind;
use camerata_api_types::decisions::DecisionState;
use serde_json::Value;

use super::support::*;
use crate::onboard::{Finding, SealedReport};
use crate::release_gate::xlsx_sheet_grid;
use crate::report_export::apply_overlay;
use crate::xlsx_export::vocabulary as v;

const PLACES: usize = 500;

/// One place ESLint's `no-var` flagged, the way `scan_tools` writes a preview row.
fn no_var_row(i: usize) -> Finding {
    Finding {
        repo: "acme/portal".to_string(),
        path: format!("web/mod{}/file{}.js", i % 7, i / 10),
        line: 1 + (i % 10) * 3,
        rule_id: "no-var".to_string(),
        severity: "low".to_string(),
        snippet: "Unexpected var, use let or const instead.".to_string(),
        detail: "Unexpected var, use let or const instead. (found by eslint during this scan; \
                 not yet part of the CI pipeline)."
            .to_string(),
        status: "suppressed-baseline".to_string(),
        preview: true,
        preview_tool: Some("eslint".to_string()),
        kind: Some(FindingKind::Defect),
        ..Finding::default()
    }
}

/// 500 places of `no-var`, beside two SQL rows and a layering row, sealed. The group reviewer
/// had split the lint rule into two groups before sealing.
fn lint_report() -> SealedReport {
    let mut report = crate::release_gate::test_scan_report();
    let mut rows: Vec<Finding> = (0..PLACES).map(no_var_row).collect();
    rows.push(sql_row("src/a.rs", 10));
    rows.push(sql_row("src/b.rs", 20));
    rows.push(layering_row("src/d.rs", 40));
    report.findings = rows;
    let mut report = report.seal().into_inner();
    for f in report
        .findings
        .iter_mut()
        .filter(|f| f.rule_id == "no-var")
        .take(150)
    {
        let head = f.group_path[2]
            .rsplit_once('/')
            .map_or(String::new(), |(h, _)| h.to_string());
        f.group_path[2] = format!("{head}/3");
    }
    report.attach_tree();
    report.seal()
}

fn lint_keys(sealed: &SealedReport) -> Vec<String> {
    let mut keys: Vec<String> = sealed
        .findings
        .iter()
        .filter(|f| f.rule_id == "no-var")
        .map(|f| f.site_key.clone())
        .collect();
    keys.sort();
    keys
}

fn workbook(entries: &std::collections::BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    entries
        .iter()
        .find(|(n, _)| n.ends_with(".xlsx"))
        .map(|(_, b)| b.clone())
        .expect("a workbook")
}

fn col(grid: &[Vec<String>], header: &str) -> usize {
    grid[0]
        .iter()
        .position(|h| h == header)
        .unwrap_or_else(|| panic!("no {header:?} column in {:?}", grid[0]))
}

#[test]
fn a_lint_rule_is_one_fix_and_every_place_stays_in_the_package() {
    let sealed = lint_report();
    let tree = sealed.tree.as_ref().expect("a tree");
    let keys = lint_keys(&sealed);
    assert_eq!(keys.len(), PLACES);
    let group = sealed
        .findings
        .iter()
        .find(|f| f.rule_id == "no-var")
        .unwrap()
        .group_path[2]
        .clone();
    assert_eq!(
        tree.batch_members(&group),
        keys,
        "the rule group of one place is every place of the rule"
    );

    // The operator decides the rule once: fix it everywhere.
    let records = node_decision(
        "dec-000001",
        &group,
        &keys,
        DecisionState::Confirmed,
        "Replace var with let or const across the bundle.",
    );
    let reviewed = apply_overlay(&sealed, &records, &[], &[]);
    let entries = released_entries(&reviewed, &client_options());

    // findings.json: every place is a row, each carrying the group's decision.
    let findings: Value = serde_json::from_slice(&entries["findings.json"]).unwrap();
    let rows = findings["findings"].as_array().expect("a findings array");
    let lint: Vec<&Value> = rows.iter().filter(|r| r["rule_id"] == "no-var").collect();
    assert_eq!(lint.len(), PLACES, "every place is in findings.json");
    assert_eq!(rows.len(), PLACES + 3, "and every other problem too");
    let fix_ids: std::collections::BTreeSet<&str> =
        lint.iter().filter_map(|r| r["fix_id"].as_str()).collect();
    assert_eq!(fix_ids.len(), 1, "one Fix for the rule: {fix_ids:?}");
    let fix_id = fix_ids.into_iter().next().unwrap().to_string();

    // The tree in findings.json counts every place under the one group.
    let nodes = findings["tree"]["nodes"].as_array().expect("a tree");
    let sub = nodes
        .iter()
        .find(|n| n["id"] == group.as_str())
        .expect("the rule group");
    assert_eq!(sub["sites"], PLACES);

    // The workbook: one Fix plan row for the rule, listing every place; every place in All
    // problems.
    let xlsx = workbook(&entries);
    let plan = xlsx_sheet_grid(&xlsx, v::FIX_PLAN).unwrap();
    let lint_plan: Vec<&Vec<String>> = plan
        .iter()
        .skip(1)
        .filter(|r| r[col(&plan, v::H_FIX_ID)] == fix_id)
        .collect();
    assert_eq!(
        lint_plan.len(),
        1,
        "one plan row for the rule: {lint_plan:?}"
    );
    assert_eq!(lint_plan[0][col(&plan, v::H_PLACES)], PLACES.to_string());
    let problems = xlsx_sheet_grid(&xlsx, v::ALL_PROBLEMS).unwrap();
    let fix_col = col(&problems, v::H_FIX_ID);
    assert_eq!(
        problems
            .iter()
            .skip(1)
            .filter(|r| r[fix_col] == fix_id)
            .count(),
        PLACES,
        "every place is in All problems"
    );
}
