//! #586: a row the collapse folded into a row of another rule is not left without a fix. The
//! row's fix also names the merged rule and carries that rule's standard remediation text from
//! the library, in the PDF data, json/inspection.json and the workbook, from the stored rows alone
//! (no model call). A merged rule with no standard remediation text in the library fails the
//! release check and is named, so the gap cannot hide.

use std::collections::HashMap;

use serde_json::Value;

use super::tests::{client_options, scan_report};
use super::*;
use crate::onboard::ScanReport;
use crate::report_export::{build_report_json, render_data_json};

const SURVIVOR: &str = "SEC-ACME-SURVIVOR-1";
const MERGED: &str = "ARCH-ACME-MERGED-1";
const MERGED_TEXT: &str = "Parse the value in the handler before it is used.";
const MERGED_TITLE: &str = "Parse inputs in the handler";

fn rule(id: &str, remediation: Option<&str>) -> camerata_rules::Rule {
    let mut r = camerata_rules::bare_rule(id, "universal");
    r.title = if id == MERGED {
        MERGED_TITLE.to_string()
    } else {
        "Bind query values".to_string()
    };
    r.options = vec![camerata_rules::RuleOption {
        id: "only".into(),
        label: "Only".into(),
        directive: "do it".into(),
        why: "w".into(),
        remediation: remediation.map(str::to_string),
        finding_headline: None,
        finding_detail: None,
        escalation: None,
        effort: None,
        violates: false,
    }];
    r.default_option = Some("only".into());
    r
}

fn corpus(merged: Option<Option<&str>>) -> camerata_rules::RuleSet {
    let mut set = camerata_rules::RuleSet::default();
    set.push(rule(SURVIVOR, Some("Bind the value as a parameter.")));
    if let Some(text) = merged {
        set.push(rule(MERGED, text));
    }
    set
}

fn report() -> ScanReport {
    let mut report = scan_report();
    report.findings[0].rule_id = SURVIVOR.to_string();
    report.findings[0].flagged_by = vec![crate::review_tree::FlaggedBy {
        rule_id: MERGED.to_string(),
        tool: None,
        site_key: "k".to_string(),
        path: "src/db.rs".to_string(),
        line: 14,
    }];
    report
}

fn package_for(corpus: &camerata_rules::RuleSet) -> ProductPackage {
    package_with_options(corpus, client_options())
}

fn package_with_options(
    corpus: &camerata_rules::RuleSet,
    options: crate::report_export::ReportOptions,
) -> ProductPackage {
    let sealed = report().seal();
    let reviewed = crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new());
    let json = build_report_json(&reviewed, Some(corpus), &options);
    let data = render_data_json(&json).expect("data.json renders");
    assemble_package(
        &reviewed,
        Some(corpus),
        &options,
        "acme-inspection",
        (json, data, b"%PDF-1.7 stand-in for the gate tests".to_vec()),
    )
    .expect("the package assembles")
}

#[test]
fn a_merged_rule_with_text_is_named_with_its_standard_remediation_in_every_export() {
    let package = package_for(&corpus(Some(Some(MERGED_TEXT))));
    let doc: Value = serde_json::from_slice(&package.findings_json).expect("parses");
    let fix = doc["findings"][0]["fix"].as_str().expect("a fix");
    assert!(
        fix.contains(MERGED_TITLE) && fix.contains(MERGED_TEXT),
        "{fix}"
    );
    assert!(
        !fix.contains(MERGED),
        "no raw rule id in client text: {fix}"
    );
    assert_eq!(
        doc["findings"][0]["fix_merged_rules"],
        serde_json::json!([MERGED]),
        "a structured field names the merged rules the fix covers"
    );
    assert!(
        fix.contains("Bind the value"),
        "the survivor's own fix stays: {fix}"
    );
    assert!(
        package.data_json.contains(MERGED_TITLE) && package.data_json.contains(MERGED_TEXT),
        "the PDF data carries it"
    );
    let strings = xlsx_strings(&package.xlsx).expect("strings").join("\n");
    assert!(
        strings.contains(MERGED_TEXT),
        "the workbook's fix column carries it"
    );
    let released = release(package).expect("a package with the text clears the gate");
    assert!(released.report.is_clean());
}

#[test]
fn a_merged_rule_with_no_standard_text_fails_the_release_check_and_is_named() {
    // Absent from the library, and present with no remediation: both are a gap.
    for library in [corpus(None), corpus(Some(None))] {
        let package = package_for(&library);
        let Err(ReleaseError::Blocked(failure)) = release(package) else {
            panic!("a merged rule with no standard text must block the release");
        };
        let named: Vec<String> = failure
            .report
            .problems()
            .filter(|(check, _)| *check == CheckId::FixParity)
            .map(|(_, p)| p.message.clone())
            .collect();
        assert!(
            named.iter().any(|m| m.contains(MERGED)),
            "the check names the rule: {named:?}"
        );
    }
}

/// The merged rule's text is the remediation of the option the project selected, not its default.
#[test]
fn a_merged_rule_gets_the_text_of_the_option_the_project_selected() {
    const DEFAULT_TEXT: &str = "Use the default way to parse.";
    const CHOSEN_TEXT: &str = "Use the chosen way to parse.";
    let mut merged = rule(MERGED, Some(DEFAULT_TEXT));
    merged.options.push(camerata_rules::RuleOption {
        id: "other".into(),
        label: "Other".into(),
        directive: "do it another way".into(),
        why: "w".into(),
        remediation: Some(CHOSEN_TEXT.into()),
        finding_headline: None,
        finding_detail: None,
        escalation: None,
        effort: None,
        violates: false,
    });
    let mut library = camerata_rules::RuleSet::default();
    library.push(rule(SURVIVOR, Some("Bind the value as a parameter.")));
    library.push(merged);
    let mut options = client_options();
    options
        .chosen_options
        .insert(MERGED.to_string(), "other".to_string());
    let package = package_with_options(&library, options);
    let doc: Value = serde_json::from_slice(&package.findings_json).expect("parses");
    let fix = doc["findings"][0]["fix"].as_str().expect("a fix");
    assert!(fix.contains(CHOSEN_TEXT), "{fix}");
    assert!(!fix.contains(DEFAULT_TEXT), "{fix}");
    assert!(package.data_json.contains(CHOSEN_TEXT), "the PDF data too");
    // Nothing selected: the rule's default, as the survivor's own fix resolves.
    let package = package_for(&library);
    let doc: Value = serde_json::from_slice(&package.findings_json).expect("parses");
    assert!(doc["findings"][0]["fix"]
        .as_str()
        .is_some_and(|f| f.contains(DEFAULT_TEXT)));
}

/// The premise of skipping tool-origin merged rows: the collapse folds an external tool row into
/// a survivor only when the tool maps to the same native rule as the survivor, so the survivor's
/// own fix covers it. A tool mapped to another rule stays a row of its own.
#[test]
fn the_collapse_folds_a_tool_row_only_into_a_row_of_the_native_rule_it_enforces() {
    let survivor = |rule: &str| {
        let mut f = scan_report().findings.remove(0);
        f.rule_id = rule.to_string();
        f.located = true;
        f
    };
    let tool_row = |rule: &str| {
        let mut f = survivor(rule);
        f.preview_tool = Some("acme-linter".to_string());
        f
    };
    let map_to = |native: &str| {
        let selectors: HashMap<String, Vec<String>> =
            HashMap::from([(native.to_string(), vec!["acme-tool-rule".to_string()])]);
        crate::review_tree::ToolMap::from_selectors(Some(&selectors))
    };
    let same = crate::review_tree::collapse_identical(
        vec![survivor(SURVIVOR), tool_row("ACME-TOOL-RULE")],
        &map_to(SURVIVOR),
    );
    assert_eq!(same.rows.len(), 1, "same native rule: folded");
    assert!(same.rows[0].flagged_by.iter().all(|b| b.tool.is_some()));
    let other = crate::review_tree::collapse_identical(
        vec![survivor(SURVIVOR), tool_row("ACME-TOOL-RULE")],
        &map_to(MERGED),
    );
    assert_eq!(other.rows.len(), 2, "another native rule: not folded");
}

/// #586 review: the merged rule's text follows the same chain as the row's own fix: the selected
/// option, and when that option has no remediation the rule's default option. The export is then
/// not refused.
#[test]
fn a_merged_rule_whose_selected_option_has_no_text_falls_back_to_the_default_options_text() {
    let mut merged = rule(MERGED, Some(MERGED_TEXT));
    merged.options.push(camerata_rules::RuleOption {
        id: "bare".into(),
        label: "Bare".into(),
        directive: "do it bare".into(),
        why: "w".into(),
        remediation: None,
        finding_headline: None,
        finding_detail: None,
        escalation: None,
        effort: None,
        violates: false,
    });
    let mut library = camerata_rules::RuleSet::default();
    library.push(rule(SURVIVOR, Some("Bind the value as a parameter.")));
    library.push(merged);
    let mut options = client_options();
    options
        .chosen_options
        .insert(MERGED.to_string(), "bare".to_string());
    let package = package_with_options(&library, options);
    let doc: Value = serde_json::from_slice(&package.findings_json).expect("parses");
    let fix = doc["findings"][0]["fix"].as_str().expect("a fix");
    assert!(fix.contains(MERGED_TEXT), "{fix}");
    let released = release(package).expect("the export is not refused");
    assert!(released.report.is_clean());
}

/// #586 review: the "missing a fix" bucket and the rendered fix read the same chosen options. A
/// row whose own rule has no remediation but whose merged rule's selected option has text is not
/// held for a missing fix.
#[test]
fn the_missing_fix_bucket_reads_the_chosen_option_of_the_merged_rule() {
    let mut survivor = rule(SURVIVOR, None);
    survivor.title = "Bind query values".to_string();
    let mut merged = rule(MERGED, None);
    merged.options.push(camerata_rules::RuleOption {
        id: "texted".into(),
        label: "Texted".into(),
        directive: "do it".into(),
        why: "w".into(),
        remediation: Some(MERGED_TEXT.into()),
        finding_headline: None,
        finding_detail: None,
        escalation: None,
        effort: None,
        violates: false,
    });
    let mut library = camerata_rules::RuleSet::default();
    library.push(survivor);
    library.push(merged);
    let mut options = client_options();
    options
        .chosen_options
        .insert(MERGED.to_string(), "texted".to_string());
    let package = package_with_options(&library, options);
    let doc: Value = serde_json::from_slice(&package.findings_json).expect("parses");
    let row = &doc["findings"][0];
    assert!(
        row["fix"].as_str().is_some_and(|f| f.contains(MERGED_TEXT)),
        "the fix is rendered: {row}"
    );
    assert_ne!(
        row["bucket"], "held",
        "a row with a rendered fix is not held for a missing fix: {row}"
    );
}
