//! The release gate refuses to export while the review screen holds a change the decision store
//! has not accepted. Packages are built through `assemble_package` around a stand-in PDF, like
//! the gate's other tests, so these run without Typst.

use std::collections::HashMap;

use camerata_api_types::decisions::{UnsyncedChange, UnsyncedKind};

use super::tests::{client_options, scan_report};
use super::*;
use crate::report_export::{build_report_json, render_data_json};

fn package_with_unsynced(changes: Vec<UnsyncedChange>) -> (ProductPackage, String) {
    let sealed = scan_report().seal();
    let site = sealed.findings[0].site_key.clone();
    let options = ReportOptions {
        unsynced_changes: changes
            .into_iter()
            .map(|c| UnsyncedChange {
                key: if c.key.is_empty() {
                    site.clone()
                } else {
                    c.key
                },
                ..c
            })
            .collect(),
        ..client_options()
    };
    let dispositions = HashMap::new();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &dispositions),
        None,
        &options,
    );
    let data = render_data_json(&json).expect("data.json renders");
    let package = assemble_package(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &dispositions),
        None,
        &options,
        "acme-inspection",
        (json, data, b"%PDF-1.7 stand-in for the gate tests".to_vec()),
    )
    .expect("the package assembles");
    (package, site)
}

fn unsynced_problems(failure: &GateFailure) -> Vec<&Problem> {
    failure
        .report
        .checks
        .iter()
        .filter(|o| o.check == CheckId::UnsyncedReviewerChanges)
        .flat_map(|o| o.problems.iter())
        .collect()
}

#[test]
fn export_is_refused_while_a_revoke_is_pending_and_allowed_once_it_syncs() {
    let undo = UnsyncedChange {
        key: String::new(),
        change: UnsyncedKind::Undo,
    };
    let (package, _site) = package_with_unsynced(vec![undo]);
    match release(package) {
        Err(ReleaseError::Blocked(failure)) => {
            let problems = unsynced_problems(&failure);
            assert_eq!(problems.len(), 1, "{:?}", failure.report);
            // It names the site and says the undo has not reached the store.
            assert!(
                problems[0].location.contains(".rs") || problems[0].location.contains('/'),
                "{}",
                problems[0].location
            );
            assert!(
                problems[0].message.contains("undo"),
                "{}",
                problems[0].message
            );
            assert!(failure.to_string().contains("unsynced reviewer changes"));
        }
        other => panic!("a pending revoke must block the export: {other:?}"),
    }

    // Once it syncs the request lists nothing, and the export goes through.
    let (package, _) = package_with_unsynced(Vec::new());
    let released = release(package).unwrap_or_else(|e| panic!("{e}"));
    assert!(released
        .report
        .check_names()
        .contains(&"unsynced reviewer changes"));
}

#[test]
fn a_pending_decision_is_refused_too_and_each_change_is_named() {
    let changes = vec![
        UnsyncedChange {
            key: String::new(),
            change: UnsyncedKind::Decision,
        },
        UnsyncedChange {
            key: "no-such-site".to_string(),
            change: UnsyncedKind::Undo,
        },
    ];
    let (package, _) = package_with_unsynced(changes);
    let Err(ReleaseError::Blocked(failure)) = release(package) else {
        panic!("blocked");
    };
    let problems = unsynced_problems(&failure);
    assert_eq!(problems.len(), 2);
    assert!(problems.iter().any(|p| p.message.contains("decision")));
    // A key no row carries is still named, so nothing is dropped silently.
    assert!(problems.iter().any(|p| p.location.contains("no-such-site")));
}

#[test]
fn the_export_request_options_carry_the_unsynced_changes() {
    let options: ReportOptions = serde_json::from_value(serde_json::json!({
        "project_title": "T",
        "unsynced_changes": [{ "key": "k", "change": "undo" }]
    }))
    .unwrap();
    assert_eq!(options.unsynced_changes.len(), 1);
    assert_eq!(options.unsynced_changes[0].change, UnsyncedKind::Undo);
    // An older client that sends none is not blocked.
    let none: ReportOptions = serde_json::from_value(serde_json::json!({})).unwrap();
    assert!(none.unsynced_changes.is_empty());
}
