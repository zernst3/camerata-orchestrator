//! M5: rows from outside tools (eslint, semgrep and the other preview tools) join the report after
//! the audit, and get the same evidence check as the audit's own rows.

use crate::onboard::evidence_check::{verify_and_record, EVIDENCE_UNVERIFIED_CAPTURE};
use crate::onboard::Finding;

const FILE: &str = "\
import express from 'express';

const app = express();

app.get('/users/:id', async (req, res) => {
  eval(req.query.code);
  res.json({});
});
";

fn files() -> Vec<(String, String)> {
    vec![("src/app.ts".to_string(), FILE.to_string())]
}

fn tool_row(line: usize, snippet: &str) -> Finding {
    Finding {
        repo: "owner/repo".to_string(),
        path: "src/app.ts".to_string(),
        line,
        rule_id: "SEC-NO-EVAL-1".to_string(),
        severity: "high".to_string(),
        snippet: snippet.to_string(),
        detail: "eval of request data".to_string(),
        preview: true,
        preview_tool: Some("eslint".to_string()),
        kind: Some(camerata_api_types::audit::FindingKind::Defect),
        ..Finding::default()
    }
}

fn report() -> crate::onboard::ScanReport {
    let mut report = crate::release_gate::test_scan_report();
    report.findings.clear();
    report
}

#[test]
fn a_tool_row_whose_snippet_is_not_at_its_line_is_flagged_and_held() {
    let mut report = report();
    let previews = vec![
        tool_row(6, "eval(req.query.code);"),
        tool_row(6, "child_process.exec(userInput)"),
    ];
    crate::join_repo_previews(&mut report, previews, None, &files(), None);
    assert_eq!(report.findings.len(), 2, "no tool row is dropped");
    let flagged: Vec<&Finding> = report
        .findings
        .iter()
        .filter(|f| f.captures.contains_key(EVIDENCE_UNVERIFIED_CAPTURE))
        .collect();
    assert_eq!(flagged.len(), 1, "{:?}", report.findings);
    assert_eq!(flagged[0].snippet, "child_process.exec(userInput)");
    assert!(crate::report_export::is_held_for_review(
        flagged[0],
        crate::report_export::Disposition::Unresolved
    ));
}

#[test]
fn the_counts_from_both_passes_land_in_one_reconciled_tally() {
    let mut report = report();
    // The audit's own pass saw two rows, one of them unverified.
    let mut audit_rows = vec![
        Finding {
            repo: "owner/repo".to_string(),
            path: "src/app.ts".to_string(),
            line: 3,
            rule_id: "AI-X".to_string(),
            snippet: "const app = express();".to_string(),
            kind: Some(camerata_api_types::audit::FindingKind::Defect),
            ..Finding::default()
        },
        Finding {
            line: 3,
            snippet: "made up".to_string(),
            ..Finding {
                repo: "owner/repo".to_string(),
                path: "src/app.ts".to_string(),
                rule_id: "AI-Y".to_string(),
                kind: Some(camerata_api_types::audit::FindingKind::Defect),
                ..Finding::default()
            }
        },
    ];
    verify_and_record(&mut report.ledger, &mut audit_rows, &files(), None);
    report.findings.extend(audit_rows);
    // Then the tool pass: one verified, one not.
    crate::join_repo_previews(
        &mut report,
        vec![tool_row(6, "eval(req.query.code);"), tool_row(6, "nope")],
        None,
        &files(),
        None,
    );
    let counts = report.ledger.evidence_check();
    assert_eq!(
        (
            counts.checked,
            counts.verified,
            counts.unverified,
            counts.exempt
        ),
        (4, 2, 2, 0)
    );
    assert!(counts.reconciles());
    assert_eq!(counts.rows_seen(), report.findings.len());
}
