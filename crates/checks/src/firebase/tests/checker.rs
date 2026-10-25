//! The degradation contract and the file-set rows (design section 5.5).

use super::{files, findings, firestore, rows, run, today};
use crate::arch_checker::{ArchChecker, RepoView, ViolationGrade, SEVERITY_INFO};
use crate::firebase::{FirebaseRulesChecker, RULE_OPEN};

fn config_unsatisfied(f: &[(String, String)]) -> bool {
    FirebaseRulesChecker {
        today: Some(today()),
    }
    .config_unsatisfied_for(&RepoView {
        spec: "t/r",
        files: f,
    })
}

#[test]
fn a_degraded_rules_file_is_one_not_assessed_row_and_never_a_clean_credit() {
    // `deny read;` is a statement the reader does not understand.
    let f = firestore("    match /a/{x} { deny read; allow write: if true; }");
    let all = run(&f);
    assert_eq!(all.len(), 1, "one row and no rule findings: {all:#?}");
    assert_eq!(all[0].grade, ViolationGrade::NotAssessed);
    assert_eq!(all[0].severity, SEVERITY_INFO);
    assert_eq!(all[0].file, "firestore.rules");
    assert_eq!(all[0].line, 4);
    assert!(
        all[0]
            .message
            .contains("could not be read as a Firebase rules file"),
        "{}",
        all[0].message
    );
    assert!(all[0].message.contains("line 4"), "{}", all[0].message);
    assert!(all[0]
        .message
        .contains("so the Firebase rules checks were not run on it"));
    assert!(
        findings(&all).is_empty(),
        "the open write behind the bad statement must not ship"
    );
    assert!(
        config_unsatisfied(&f),
        "a repository whose only rules file is unreadable must not be credited as verified clean"
    );
}

#[test]
fn a_degraded_file_does_not_hide_findings_from_a_readable_one() {
    let f = files(&[
        (
            "firebase.json",
            r#"{ "firestore": [ { "database": "a", "rules": "a.rules" }, { "database": "b", "rules": "b.rules" } ] }"#,
        ),
        (
            "a.rules",
            "service cloud.firestore {\n  match /databases/{database}/documents {\n    match /x/{i} { allow write: if true; }\n  }\n}\n",
        ),
        ("b.rules", "service cloud.firestore { match /databases/{d}/documents { deny read; } }"),
    ]);
    let all = run(&f);
    assert_eq!(findings(&all).len(), 1, "{all:#?}");
    assert_eq!(rows(&all, RULE_OPEN, ViolationGrade::NotAssessed).len(), 1);
    assert!(!config_unsatisfied(&f), "one readable file still answers");
}

#[test]
fn a_repository_with_no_rules_file_is_one_not_assessed_row_never_clean() {
    let f = files(&[("firebase.json", "{}"), ("src/index.ts", "export {};")]);
    let all = run(&f);
    assert_eq!(all.len(), 1, "{all:#?}");
    assert_eq!(all[0].grade, ViolationGrade::NotAssessed);
    assert!(
        all[0].message.contains("Firebase console"),
        "{}",
        all[0].message
    );
    assert!(config_unsatisfied(&f));
}

#[test]
fn an_unreadable_firebase_json_is_reported_and_conventional_files_still_ship() {
    let f = files(&[
        ("firebase.json", "{ \"firestore\": "),
        (
            "firestore.rules",
            "service cloud.firestore {\n  match /databases/{database}/documents {\n    match /x/{i} { allow write: if true; }\n  }\n}\n",
        ),
    ]);
    let all = run(&f);
    let found = findings(&all);
    assert_eq!(found.len(), 1, "{all:#?}");
    assert!(
        found[0].message.contains("firebase.json could not be read"),
        "{}",
        found[0].message
    );
    assert_eq!(
        rows(&all, RULE_OPEN, ViolationGrade::NotAssessed).len(),
        1,
        "the config itself is reported: {all:#?}"
    );
}

#[test]
fn a_file_firebase_json_names_that_is_missing_is_reported() {
    let f = files(&[(
        "firebase.json",
        r#"{ "firestore": { "rules": "gone.rules" } }"#,
    )]);
    let all = run(&f);
    assert!(
        all.iter().any(|v| v.message.contains("`gone.rules`")),
        "{all:#?}"
    );
}

#[test]
fn a_service_that_is_not_firestore_or_storage_degrades_a_named_file() {
    let f = files(&[
        (
            "firebase.json",
            r#"{ "firestore": { "rules": "firestore.rules" } }"#,
        ),
        ("firestore.rules", "service cloud.spanner { }"),
    ]);
    let all = run(&f);
    assert_eq!(all.len(), 1, "{all:#?}");
    assert_eq!(all[0].grade, ViolationGrade::NotAssessed);
}

#[test]
fn the_checker_does_not_defer_to_the_advisory_tier_for_ci() {
    // Its rule ids stay in the advisory prompt (judgment halves), but its defects are facts.
    let c = FirebaseRulesChecker::default();
    assert!(c.advisory_coexisting());
    assert!(!c.defects_need_review());
}
