//! A rules file that may not be what deploys is held for review, never asserted (owner call,
//! 2026-10-25): `firebase.json` does not reference it, no `firebase.json` exists, or it sits
//! under an examples, samples or docs folder.

use super::{files, findings, rows, run};
use crate::arch_checker::{
    ArchChecker, ArchViolation, RepoView, ViolationGrade, SEVERITY_CRITICAL, SEVERITY_HIGH,
};
use crate::firebase::{
    FirebaseRulesTestsChecker, RULE_OPEN, RULE_RTDB_OPEN, RULE_RULES_TESTS, RULE_STORAGE_OPEN,
};

const OPEN_ROOT: &str = "rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n    match /{document=**} { allow read, write: if true; }\n  }\n}\n";

fn tests_rows(f: &[(String, String)]) -> Vec<ArchViolation> {
    FirebaseRulesTestsChecker.check(&RepoView {
        spec: "t/r",
        files: f,
    })
}

/// The one finding, which must be review-graded and name the reason.
fn the_held_row(all: &[ArchViolation], reason: &str) {
    let f = findings(all);
    let open: Vec<_> = f.iter().filter(|v| v.rule_id == RULE_OPEN).collect();
    assert_eq!(open.len(), 1, "{all:#?}");
    assert_eq!(open[0].grade, ViolationGrade::Review, "{all:#?}");
    assert!(
        open[0].severity != SEVERITY_CRITICAL,
        "a held row is never critical: {}",
        open[0].severity
    );
    assert_eq!(open[0].severity, SEVERITY_HIGH);
    assert!(open[0].message.contains(reason), "{}", open[0].message);
}

/// Reviewer case `stale_conventional`: `firebase.json` exists and says nothing about Firestore.
#[test]
fn a_conventional_file_firebase_json_does_not_reference_is_held_for_review() {
    let f = files(&[
        ("firebase.json", r#"{ "hosting": { "public": "dist" } }"#),
        ("firestore.rules", OPEN_ROOT),
    ]);
    the_held_row(&run(&f), "does not reference this file");
}

/// A rules file in a repository with no `firebase.json` at all.
#[test]
fn a_rules_file_in_a_repository_with_no_firebase_json_is_held_for_review() {
    let f = files(&[("firestore.rules", OPEN_ROOT)]);
    the_held_row(&run(&f), "No `firebase.json` in this repository");
}

/// Reviewer case `examples_dir`.
#[test]
fn a_rules_file_under_an_examples_folder_is_held_for_review() {
    for dir in [
        "examples", "example", "samples", "sample", "docs", "Examples",
    ] {
        let f = files(&[(&format!("{dir}/firestore.rules"), OPEN_ROOT)]);
        the_held_row(&run(&f), "examples, samples or docs folder");
    }
}

/// The examples rule holds even when a `firebase.json` names the file.
#[test]
fn an_example_file_a_firebase_json_names_is_still_held_for_review() {
    let f = files(&[
        (
            "examples/chat/firebase.json",
            r#"{ "firestore": { "rules": "firestore.rules" } }"#,
        ),
        ("examples/chat/firestore.rules", OPEN_ROOT),
    ]);
    the_held_row(&run(&f), "examples, samples or docs folder");
}

/// A file `firebase.json` names, outside any example folder, is what deploys: asserted.
#[test]
fn twin_a_file_firebase_json_names_is_asserted() {
    let f = files(&[
        (
            "firebase.json",
            r#"{ "firestore": { "rules": "firestore.rules" } }"#,
        ),
        ("firestore.rules", OPEN_ROOT),
    ]);
    let all = run(&f);
    assert_eq!(
        rows(&all, RULE_OPEN, ViolationGrade::Defect).len(),
        1,
        "{all:#?}"
    );
}

/// Only a whole folder name marks an example: `my-examples` and `docsify` are ordinary folders.
#[test]
fn twin_look_alike_folder_names_are_not_example_folders() {
    for dir in ["my-examples", "docsify", "exampleapp", "documentation"] {
        let f = files(&[
            (
                &format!("{dir}/firebase.json"),
                r#"{ "firestore": { "rules": "firestore.rules" } }"#,
            ),
            (&format!("{dir}/firestore.rules"), OPEN_ROOT),
        ]);
        let all = run(&f);
        assert_eq!(
            rows(&all, RULE_OPEN, ViolationGrade::Defect).len(),
            1,
            "{dir}: {all:#?}"
        );
    }
}

/// The folder is a directory of the path: a file NAMED like one is not under it.
#[test]
fn twin_a_file_named_like_an_example_folder_is_not_under_one() {
    let f = files(&[
        (
            "firebase.json",
            r#"{ "firestore": { "rules": "examples" } }"#,
        ),
        ("examples", OPEN_ROOT),
    ]);
    let all = run(&f);
    assert_eq!(
        rows(&all, RULE_OPEN, ViolationGrade::Defect).len(),
        1,
        "{all:#?}"
    );
}

#[test]
fn storage_and_realtime_database_files_are_held_too() {
    let f = files(&[
        (
            "storage.rules",
            "rules_version = '2';\nservice firebase.storage {\n  match /b/{bucket}/o {\n    match /{allPaths=**} { allow read, write: if true; }\n  }\n}\n",
        ),
        (
            "database.rules.json",
            r#"{ "rules": { ".read": true, ".write": true } }"#,
        ),
    ]);
    let all = run(&f);
    assert!(
        rows(&all, RULE_STORAGE_OPEN, ViolationGrade::Defect).is_empty()
            && rows(&all, RULE_RTDB_OPEN, ViolationGrade::Defect).is_empty(),
        "{all:#?}"
    );
    let held: Vec<_> = findings(&all)
        .into_iter()
        .filter(|v| v.rule_id == RULE_STORAGE_OPEN || v.rule_id == RULE_RTDB_OPEN)
        .collect();
    assert_eq!(held.len(), 2, "{all:#?}");
    assert!(held.iter().all(|v| v.grade == ViolationGrade::Review));
    assert!(held.iter().all(|v| v.severity != SEVERITY_CRITICAL));
}

/// The rules-tests row says the repository DEPLOYS rules; for a file that may not deploy, it is
/// held rather than asserted.
#[test]
fn the_rules_tests_row_is_held_when_no_assessed_file_is_confirmed() {
    let stale = files(&[("firestore.rules", OPEN_ROOT)]);
    let rows = tests_rows(&stale);
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(rows[0].rule_id, RULE_RULES_TESTS);
    assert_eq!(rows[0].grade, ViolationGrade::Review);
    assert!(
        rows[0]
            .message
            .contains("No `firebase.json` in this repository"),
        "{}",
        rows[0].message
    );
}

#[test]
fn the_rules_tests_row_names_only_the_confirmed_files_and_stays_asserted() {
    let f = files(&[
        (
            "firebase.json",
            r#"{ "firestore": { "rules": "firestore.rules" } }"#,
        ),
        ("firestore.rules", OPEN_ROOT),
        ("examples/firestore.rules", OPEN_ROOT),
    ]);
    let rows = tests_rows(&f);
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(rows[0].grade, ViolationGrade::Defect);
    assert!(rows[0].message.contains("`firestore.rules`"));
    assert!(
        !rows[0].message.contains("examples/"),
        "{}",
        rows[0].message
    );
}
