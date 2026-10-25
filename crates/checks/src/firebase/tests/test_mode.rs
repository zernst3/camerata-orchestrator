//! FIREBASE-RULES-TEST-MODE-1 (design section 7.2); today is pinned to 2026-10-25.

use super::{findings, firestore, rows, run, storage};
use crate::arch_checker::{ViolationGrade, SEVERITY_CRITICAL, SEVERITY_HIGH, SEVERITY_MEDIUM};
use crate::firebase::RULE_TEST_MODE;

#[test]
fn future_test_mode_at_the_root_is_critical() {
    // The firebase-tools template.
    let all = run(&firestore(
        "    match /{document=**} {\n      allow read, write: if request.time < timestamp.date(2026, 11, 24);\n    }",
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].rule_id, RULE_TEST_MODE);
    assert_eq!(f[0].severity, SEVERITY_CRITICAL);
    assert_eq!(f[0].grade, ViolationGrade::Defect);
    assert_eq!(f[0].line, 5);
    assert!(
        f[0].message.contains("24 November 2026"),
        "{}",
        f[0].message
    );
    assert!(f[0].message.contains("test-mode scaffold"));
}

#[test]
fn future_test_mode_on_a_collection_is_high() {
    let all = run(&firestore(
        "    match /notes/{n} { allow read, write: if request.time < timestamp.date(2027, 1, 1); }",
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].severity, SEVERITY_HIGH);
    assert_eq!(f[0].object.as_deref(), Some("/notes/{n}"));
}

#[test]
fn past_test_mode_is_review_graded_medium_and_names_both_readings() {
    let all = run(&firestore(
        "    match /{document=**} { allow read, write: if request.time < timestamp.date(2025, 11, 24); }",
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].grade, ViolationGrade::Review);
    assert_eq!(f[0].severity, SEVERITY_MEDIUM);
    assert!(
        f[0].message.contains("24 November 2025"),
        "{}",
        f[0].message
    );
    assert!(f[0].message.contains("which has passed"));
    assert!(f[0].message.contains("not the rules in this repository"));
}

#[test]
fn test_mode_in_storage_rules_is_reported() {
    let all = run(&storage(
        "    match /{allPaths=**} { allow read, write: if request.time < timestamp.date(2026, 12, 1); }",
    ));
    let hits = rows(&all, RULE_TEST_MODE, ViolationGrade::Defect);
    assert_eq!(hits.len(), 1, "{all:#?}");
    assert_eq!(hits[0].severity, SEVERITY_CRITICAL);
    assert_eq!(hits[0].file, "storage.rules");
}

#[test]
fn twin_embargo_with_greater_than_is_silent() {
    let all = run(&firestore(
        "    match /posts/{p} { allow read: if request.time > timestamp.date(2026, 1, 1); }",
    ));
    assert!(findings(&all).is_empty(), "{all:#?}");
}

#[test]
fn twin_time_clause_with_an_auth_conjunct_is_silent() {
    let all = run(&firestore(
        "    match /posts/{p} { allow read: if request.time < timestamp.date(2027, 1, 1) && request.auth != null; }",
    ));
    assert!(findings(&all).is_empty(), "{all:#?}");
}

#[test]
fn twin_timestamp_date_with_variables_is_silent() {
    let all = run(&firestore(
        "    match /posts/{p} { allow read: if request.time < timestamp.date(y, m, d); }",
    ));
    assert!(findings(&all).is_empty(), "{all:#?}");
}
