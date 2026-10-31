//! Defect D1 at the release gate: a package holding a junk model finding is refused, a package
//! holding a real one is not. A real package is built through `assemble_package` around a
//! stand-in PDF, like the gate's other tests, so these run without Typst.

use std::collections::HashMap;

use super::tests::{client_options, scan_report};
use super::*;
use crate::onboard::{Finding, ScanReport};
use crate::report_export::{build_report_json, render_data_json};

fn package_for(report: ScanReport) -> ProductPackage {
    let sealed = report.seal();
    let options = client_options();
    let json = build_report_json(&sealed, &HashMap::new(), None, &options);
    let data = render_data_json(&json).expect("data.json renders");
    assemble_package(
        &sealed,
        &HashMap::new(),
        None,
        &options,
        "acme-inspection",
        (json, data, b"%PDF-1.7 stand-in for the gate tests".to_vec()),
    )
    .expect("the package assembles")
}

fn ai_row(rule_id: &str, snippet: &str, detail: &str) -> Finding {
    Finding {
        repo: "acme/portal".to_string(),
        path: "lib/supabase/admin.ts".to_string(),
        line: 8,
        rule_id: rule_id.to_string(),
        severity: "low".to_string(),
        snippet: snippet.to_string(),
        detail: detail.to_string(),
        ..Finding::default()
    }
}

fn report_with(extra: Finding) -> ScanReport {
    let mut report = scan_report();
    report.findings.push(extra);
    report
}

fn content_problems_of(failure: &GateFailure) -> Vec<&Problem> {
    failure
        .report
        .checks
        .iter()
        .filter(|o| o.check == CheckId::Content)
        .flat_map(|o| o.problems.iter())
        .collect()
}

/// The incident: rule `JAVASCRIPT-TYPESRIPT-PLACEHOLDER`, title `placeholder`, code `x`, and the
/// explanation the detail step wrote ("Discard this finding").
#[test]
fn a_package_holding_the_placeholder_row_is_refused() {
    let junk = ai_row(
        "AI-JAVASCRIPT-TYPESRIPT-PLACEHOLDER",
        "x",
        "placeholder \u{2014} This finding contains only a placeholder statement with the code 'x'. Discard this finding.",
    );
    match release(package_for(report_with(junk))) {
        Err(ReleaseError::Blocked(failure)) => {
            let problems = content_problems_of(&failure);
            assert_eq!(problems.len(), 1, "{:?}", failure.report);
            assert_eq!(problems[0].file, "findings.json");
            assert_eq!(
                problems[0].evidence.as_deref(),
                Some("AI-JAVASCRIPT-TYPESRIPT-PLACEHOLDER")
            );
        }
        other => panic!("the junk row must block the release: {other:?}"),
    }
}

/// Each junk shape alone blocks: a placeholder rule id, no evidence at all, a discard verdict.
#[test]
fn each_junk_shape_alone_blocks_the_release() {
    for junk in [
        ai_row(
            "AI-TYPESCRIPT-PLACEHOLDER",
            "db.query(sql)",
            "Raw SQL \u{2014} Built from user input.",
        ),
        ai_row("AI-RAW-SQL", "x", "x"),
        ai_row(
            "AI-RAW-SQL",
            "db.query(sql)",
            "Raw SQL \u{2014} Built from input. Discard this finding.",
        ),
    ] {
        let detail = junk.detail.clone();
        assert!(
            matches!(
                release(package_for(report_with(junk))),
                Err(ReleaseError::Blocked(_))
            ),
            "{detail}"
        );
    }
}

/// A real invented row, and a row under an adopted rule with a short explanation, clear the gate.
#[test]
fn real_rows_clear_the_content_check() {
    let real = ai_row(
        "AI-SERVICE-ROLE-IN-BUNDLE",
        "export const admin = createClient(url, serviceKey)",
        "Service role key reaches the browser \u{2014} The admin client is built with the key in a module the browser imports.",
    );
    let released = release(package_for(report_with(real))).expect("a real row clears the gate");
    assert!(released.report.is_clean());
}
