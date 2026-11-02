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

// ── M4: real junk is a per-row hold decided by a reviewer; wording is a flag and never blocks ──

use crate::report_export::{finding_key, DispositionWire};

fn package_with(
    report: ScanReport,
    dispositions: &HashMap<String, DispositionWire>,
) -> ProductPackage {
    let sealed = report.seal();
    let options = client_options();
    let json = build_report_json(&sealed, dispositions, None, &options);
    let data = render_data_json(&json).expect("data.json renders");
    assemble_package(
        &sealed,
        dispositions,
        None,
        &options,
        "acme-inspection",
        (json, data, b"%PDF-1.7 stand-in for the gate tests".to_vec()),
    )
    .expect("the package assembles")
}

fn decided(row: &Finding, state: &str) -> HashMap<String, DispositionWire> {
    HashMap::from([(
        finding_key(row),
        DispositionWire {
            state: state.to_string(),
            reason: "read it".to_string(),
            ..DispositionWire::default()
        },
    )])
}

/// An empty explanation and a model refusal where the explanation should be: real junk.
fn empty_and_refusal_rows() -> Vec<Finding> {
    vec![
        ai_row("AI-RAW-SQL", "db.query(sql)", "Raw SQL \u{2014} "),
        ai_row(
            "AI-SERVICE-ROLE",
            "createClient(url, serviceKey)",
            "Service role key \u{2014} I cannot determine whether this key reaches the browser.",
        ),
        ai_row(
            "AI-OPEN-REDIRECT",
            "redirect(req.query.next)",
            r#"{"why":"The redirect target is taken from the query string.","fix_direction":"Allow-list it."}"#,
        ),
    ]
}

/// An empty or refusal explanation holds the row, and the gate refuses the export until a
/// reviewer decision exists for that row.
#[test]
fn an_empty_or_refusal_explanation_holds_the_row_and_the_gate_refuses_until_it_is_decided() {
    for junk in empty_and_refusal_rows() {
        let rule = junk.rule_id.clone();
        let package = package_with(report_with(junk.clone()), &HashMap::new());
        match release(package) {
            Err(ReleaseError::Blocked(failure)) => {
                let problems = content_problems_of(&failure);
                assert_eq!(problems.len(), 1, "{rule}: {:?}", failure.report);
                assert_eq!(problems[0].evidence.as_deref(), Some(rule.as_str()));
                assert!(failure.to_string().contains("lib/supabase/admin.ts line 8"));
            }
            other => panic!("{rule}: junk must block the release: {other:?}"),
        }
        // The same row, decided in any state a reviewer can give, clears the gate.
        for state in ["Confirmed", "Ignored", "TechDebt", "FalsePositive"] {
            let released = release(package_with(
                report_with(junk.clone()),
                &decided(&junk, state),
            ));
            assert!(released.is_ok(), "{rule} {state}: {:?}", released.err());
        }
        // An empty or unreadable state is not a decision.
        for state in ["", "Unresolved", "NoSuchState"] {
            let package = package_with(report_with(junk.clone()), &decided(&junk, state));
            assert!(
                matches!(release(package), Err(ReleaseError::Blocked(_))),
                "{rule} {state:?}"
            );
        }
    }
}

/// The held row is a held row: the report routes it to the held bucket, not an action tier.
#[test]
fn a_junk_row_is_held_for_review_until_it_is_decided() {
    let sealed = report_with(ai_row("AI-RAW-SQL", "db.query(sql)", "Raw SQL \u{2014} ")).seal();
    let row = sealed
        .findings
        .iter()
        .find(|f| f.rule_id == "AI-RAW-SQL")
        .expect("the row");
    assert!(row
        .captures
        .contains_key(crate::junk_finding::JUNK_CONTENT_CAPTURE));
    assert!(crate::report_export::is_held_for_review(
        row,
        crate::report_export::Disposition::Unresolved
    ));
    assert!(!crate::report_export::is_held_for_review(
        row,
        crate::report_export::Disposition::Confirmed
    ));
}

/// A wording-only issue never blocks the gate: the row ships with a flag on it.
#[test]
fn a_wording_only_issue_never_blocks_the_release_gate() {
    let wording_rows = [
        ai_row(
            "AI-HARDCODED-CREDENTIAL",
            "API_KEY = \"changeme\"",
            "Placeholder credentials are committed \u{2014} Placeholder credentials are committed in the example file and loaded in production.",
        ),
        ai_row(
            "AI-EMPTY-TABLE",
            "rows.map(render)",
            "Empty table \u{2014} Treats every row as a placeholder finding when the table is empty.",
        ),
    ];
    for row in wording_rows {
        let rule = row.rule_id.clone();
        let package = package_with(report_with(row), &HashMap::new());
        assert!(package.undecided_junk_content.is_empty(), "{rule}");
        let released = release(package);
        assert!(released.is_ok(), "{rule}: {:?}", released.err());
    }
    // And the flag is on the sealed row, in the one list.
    let sealed = report_with(ai_row(
        "AI-HARDCODED-CREDENTIAL",
        "API_KEY = \"changeme\"",
        "Placeholder credentials are committed \u{2014} Loaded in production.",
    ))
    .seal();
    let flagged = sealed
        .findings
        .iter()
        .find(|f| f.rule_id == "AI-HARDCODED-CREDENTIAL")
        .expect("the row");
    assert_eq!(
        crate::review_flags::flags_of(flagged),
        vec!["placeholder_wording".to_string()]
    );
    assert_eq!(sealed.ledger.content_check().wording_flags, 1);
    assert_eq!(sealed.ledger.content_check().held, 0);
}

/// A standalone audit of a zip has no decisions, so it still reports a junk row, and only junk.
#[test]
fn a_standalone_audit_of_a_zip_reports_junk_but_not_wording() {
    let junk = release(package_with(
        report_with(ai_row("AI-RAW-SQL", "db.query(sql)", "Raw SQL \u{2014} ")),
        &decided(
            &ai_row("AI-RAW-SQL", "db.query(sql)", "Raw SQL \u{2014} "),
            "Confirmed",
        ),
    ))
    .expect("decided, so released");
    let audited = audit_zip(&junk.bytes, None);
    assert!(
        audited
            .checks
            .iter()
            .any(|o| o.check == CheckId::Content && !o.problems.is_empty()),
        "{audited:?}"
    );
    let wording = release(package_with(
        report_with(ai_row(
            "AI-HARDCODED-CREDENTIAL",
            "API_KEY = \"changeme\"",
            "Placeholder credentials are committed \u{2014} Loaded in production.",
        )),
        &HashMap::new(),
    ))
    .expect("wording never blocks");
    assert!(audit_zip(&wording.bytes, None).is_clean());
}
