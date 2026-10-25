//! FIREBASE-RULES-RECURSIVE-WILDCARD-1 (design section 7.6).

use super::{findings, firestore, rows, run, storage};
use crate::arch_checker::{ViolationGrade, SEVERITY_CRITICAL, SEVERITY_HIGH};
use crate::firebase::{RULE_AUTH_ONLY, RULE_OPEN, RULE_RECURSIVE_WILDCARD, RULE_TEST_MODE};

const THREE_NARROWER: &str = "    match /{document=**} { allow read, write: if request.auth != null; }\n    match /users/{uid} { allow read, write: if request.auth.uid == uid; }\n    match /orders/{id} { allow read: if resource.data.owner == request.auth.uid; }\n    match /invoices/{id} { allow write: if false; }";

#[test]
fn broad_auth_only_grant_lists_every_overridden_narrower_rule() {
    let all = run(&firestore(THREE_NARROWER));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "exactly one row, no AUTH-ONLY row: {f:#?}");
    assert_eq!(f[0].rule_id, RULE_RECURSIVE_WILDCARD);
    assert_eq!(f[0].line, 4);
    assert_eq!(f[0].severity, SEVERITY_HIGH);
    assert_eq!(f[0].grade, ViolationGrade::Defect);
    let m = &f[0].message;
    assert!(m.contains("lines 5, 6 and 7"), "{m}");
    assert!(m.contains("`/users/{uid}`"), "{m}");
    assert!(m.contains("`/orders/{id}`"), "{m}");
    assert!(m.contains("`/invoices/{id}`"), "{m}");
    assert!(m.contains("allowed by line 4"), "{m}");
    assert!(rows(&all, RULE_AUTH_ONLY, ViolationGrade::Defect).is_empty());
}

#[test]
fn broad_open_grant_with_narrower_rules_is_critical_and_owns_the_fact() {
    let all = run(&firestore(
        "    match /{document=**} { allow read, write: if true; }\n    match /users/{uid} { allow write: if request.auth.uid == uid; }",
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].rule_id, RULE_RECURSIVE_WILDCARD);
    assert_eq!(f[0].severity, SEVERITY_CRITICAL);
    assert!(rows(&all, RULE_OPEN, ViolationGrade::Defect).is_empty());
}

#[test]
fn broad_test_mode_grant_with_narrower_rules() {
    let all = run(&firestore(
        "    match /{document=**} { allow read, write: if request.time < timestamp.date(2026, 12, 31); }\n    match /users/{uid} { allow write: if request.auth.uid == uid; }",
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].rule_id, RULE_RECURSIVE_WILDCARD);
    assert_eq!(f[0].severity, SEVERITY_CRITICAL);
    assert!(rows(&all, RULE_TEST_MODE, ViolationGrade::Defect).is_empty());
}

#[test]
fn twin_recursive_wildcard_locked_with_false_is_silent() {
    let all = run(&firestore(
        "    match /{document=**} { allow read, write: if false; }\n    match /users/{uid} { allow read, write: if request.auth.uid == uid; }",
    ));
    assert!(findings(&all).is_empty(), "{all:#?}");
}

#[test]
fn twin_recursive_wildcard_alone_is_reported_by_open_not_here() {
    let all = run(&firestore(
        "    match /{document=**} { allow read, write: if true; }",
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].rule_id, RULE_OPEN);
}

#[test]
fn twin_admin_override_with_a_claims_condition_is_silent() {
    let all = run(&firestore(
        "    match /{document=**} { allow read, write: if request.auth.token.admin == true; }\n    match /users/{uid} { allow read, write: if request.auth.uid == uid; }",
    ));
    assert!(findings(&all).is_empty(), "{all:#?}");
}

#[test]
fn twin_a_narrower_rule_that_is_itself_open_is_not_overridden() {
    let all = run(&firestore(
        "    match /{document=**} { allow read: if request.auth != null; }\n    match /pages/{p} { allow read: if true; }",
    ));
    assert!(
        rows(&all, RULE_RECURSIVE_WILDCARD, ViolationGrade::Defect).is_empty(),
        "{all:#?}"
    );
}

#[test]
fn twin_non_overlapping_methods_are_not_overridden() {
    let all = run(&firestore(
        "    match /{document=**} { allow read: if request.auth != null; }\n    match /users/{uid} { allow write: if request.auth.uid == uid; }",
    ));
    assert!(
        rows(&all, RULE_RECURSIVE_WILDCARD, ViolationGrade::Defect).is_empty()
            && rows(&all, RULE_RECURSIVE_WILDCARD, ViolationGrade::Review).is_empty(),
        "{all:#?}"
    );
}

#[test]
fn mid_path_wildcard_covers_matching_subcollections() {
    let all = run(&firestore(
        "    match /{path=**}/comments/{c} { allow read, write: if request.auth != null; }\n    match /posts/{p}/comments/{c} { allow write: if request.auth.uid == resource.data.author; }\n    match /posts/{p} { allow write: if request.auth.uid == resource.data.author; }",
    ));
    let hits = rows(&all, RULE_RECURSIVE_WILDCARD, ViolationGrade::Defect);
    assert_eq!(hits.len(), 1, "{all:#?}");
    assert!(
        hits[0].message.contains("line 5"),
        "only the comments rule is covered: {}",
        hits[0].message
    );
    assert!(!hits[0].message.contains("line 6"), "{}", hits[0].message);
}

#[test]
fn a_v1_recursive_wildcard_needs_one_or_more_segments() {
    // In v1 `{x=**}` matches one or more segments, so it does not cover the bare prefix itself.
    let v1 = "service cloud.firestore {\n  match /databases/{database}/documents {\n    match /posts/{rest=**} { allow read: if request.auth != null; }\n    match /posts { allow read: if false; }\n  }\n}\n";
    let f = super::files(&[
        (
            "firebase.json",
            r#"{ "firestore": { "rules": "firestore.rules" } }"#,
        ),
        ("firestore.rules", v1),
    ]);
    let all = run(&f);
    assert!(
        rows(&all, RULE_RECURSIVE_WILDCARD, ViolationGrade::Defect).is_empty(),
        "{all:#?}"
    );
}

#[test]
fn a_storage_wildcard_grant_over_narrower_rules_is_reported() {
    let all = run(&storage(
        "    match /{allPaths=**} { allow read, write: if true; }\n    match /private/{f} { allow read: if request.auth != null && false; }",
    ));
    let hits = rows(&all, RULE_RECURSIVE_WILDCARD, ViolationGrade::Defect);
    assert_eq!(hits.len(), 1, "{all:#?}");
    assert_eq!(hits[0].severity, SEVERITY_CRITICAL);
}
