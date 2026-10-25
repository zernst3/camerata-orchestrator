//! FIREBASE-RULES-OPEN-1 (design section 7.1).

use super::{files, findings, firestore, rows, run, storage};
use crate::arch_checker::{ViolationGrade, SEVERITY_CRITICAL, SEVERITY_HIGH, SEVERITY_MEDIUM};
use crate::firebase::RULE_OPEN;

#[test]
fn open_write_on_a_collection_is_high() {
    let all = run(&firestore(
        "    match /orders/{id} { allow read, write: if true; }",
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].rule_id, RULE_OPEN);
    assert_eq!(f[0].grade, ViolationGrade::Defect);
    assert_eq!(f[0].severity, SEVERITY_HIGH);
    assert_eq!(f[0].line, 4);
    assert_eq!(f[0].file, "firestore.rules");
    assert_eq!(f[0].object.as_deref(), Some("/orders/{id}"));
    assert!(
        f[0].message.contains("create, change and delete"),
        "{}",
        f[0].message
    );
    assert!(f[0].message.contains("`allow read, write: if true`"));
    assert!(f[0].message.contains("confirm against the deployed rules"));
}

#[test]
fn open_read_and_write_at_the_root_is_critical() {
    // The docs' "open access" snippet.
    let all = run(&firestore(
        "    match /{document=**} { allow read, write: if true; }",
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].severity, SEVERITY_CRITICAL);
    assert_eq!(f[0].grade, ViolationGrade::Defect);
    assert_eq!(f[0].object.as_deref(), Some("/{document=**}"));
    assert!(f[0].message.contains("in the database"), "{}", f[0].message);
}

#[test]
fn open_create_only_is_medium() {
    let all = run(&firestore(
        "    match /submissions/{id} {\n      allow create: if true;\n      allow read: if request.auth.uid == resource.data.uid;\n    }",
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].severity, SEVERITY_MEDIUM);
    assert_eq!(f[0].grade, ViolationGrade::Defect);
    assert_eq!(f[0].line, 5);
}

#[test]
fn open_read_at_the_root_is_high_and_asserted() {
    let all = run(&firestore(
        "    match /{document=**} { allow read: if true; }",
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].severity, SEVERITY_HIGH);
    assert_eq!(f[0].grade, ViolationGrade::Defect);
    assert!(f[0]
        .message
        .contains("to read every document in the database"));
}

#[test]
fn open_read_on_a_collection_is_review_graded_medium() {
    let all = run(&firestore(
        "    match /profiles/{uid} {\n      allow read: if true;\n      allow write: if request.auth.uid == uid;\n    }",
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].grade, ViolationGrade::Review);
    assert_eq!(f[0].severity, SEVERITY_MEDIUM);
    assert_eq!(f[0].line, 5);
    assert!(
        f[0].message.contains("meant to be public"),
        "{}",
        f[0].message
    );
}

#[test]
fn open_read_beside_open_write_in_the_same_block_is_one_row() {
    let all = run(&firestore(
        "    match /orders/{id} {\n      allow read: if true;\n      allow write: if true;\n    }",
    ));
    let f = findings(&all);
    assert_eq!(
        f.len(),
        1,
        "the read is a clause of the write finding: {f:#?}"
    );
    assert_eq!(f[0].line, 6);
    assert_eq!(f[0].severity, SEVERITY_HIGH);
}

#[test]
fn or_true_makes_a_grant_unconditional() {
    let all = run(&firestore(
        "    match /orders/{id} { allow write: if request.auth != null || true; }",
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].rule_id, RULE_OPEN);
    assert_eq!(f[0].severity, SEVERITY_HIGH);
}

#[test]
fn a_missing_condition_is_unconditional() {
    let all = run(&firestore("    match /orders/{id} { allow write; }"));
    assert_eq!(rows(&all, RULE_OPEN, ViolationGrade::Defect).len(), 1);
}

#[test]
fn twin_public_collection_read_is_silent() {
    for body in [
        "    match /public/{doc} { allow read: if true; }",
        "    match /catalog/{item} { allow read: if true; }",
    ] {
        let all = run(&firestore(body));
        assert!(findings(&all).is_empty(), "{body}: {all:#?}");
    }
}

#[test]
fn twin_locked_mode_default_is_silent() {
    let all = run(&firestore(
        "    match /{document=**} { allow read, write: if false; }",
    ));
    assert!(findings(&all).is_empty(), "{all:#?}");
}

#[test]
fn twin_unrecognised_condition_is_silent() {
    let all = run(&firestore(
        "    match /posts/{id} { allow read: if resource.data.visibility == 'public'; }",
    ));
    assert!(findings(&all).is_empty(), "{all:#?}");
}

#[test]
fn twin_draft_and_test_rules_files_produce_no_defect() {
    let open_everything =
        "rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n    match /{document=**} { allow read, write: if true; }\n  }\n}\n";
    let clean = "rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n    match /{document=**} { allow read, write: if false; }\n  }\n}\n";
    let f = files(&[
        (
            "firebase.json",
            r#"{ "firestore": { "rules": "firestore.rules" } }"#,
        ),
        ("firestore.rules", clean),
        ("rules/firestore.dev.rules", open_everything),
        ("test/firestore.test.rules", open_everything),
    ]);
    let all = run(&f);
    assert!(
        findings(&all).is_empty(),
        "no defect from a draft or test file: {all:#?}"
    );
    let notes = rows(&all, RULE_OPEN, ViolationGrade::NotAssessed);
    assert_eq!(
        notes.len(),
        1,
        "one not-assessed row for the dev file: {notes:#?}"
    );
    assert_eq!(notes[0].file, "rules/firestore.dev.rules");
    assert!(
        notes[0].message.contains("does not reference"),
        "{}",
        notes[0].message
    );
}

#[test]
fn twin_storage_file_is_not_this_rule() {
    let all = run(&storage(
        "    match /{allPaths=**} { allow read, write: if true; }",
    ));
    assert!(
        rows(&all, RULE_OPEN, ViolationGrade::Defect).is_empty(),
        "storage is FIREBASE-STORAGE-RULES-OPEN-1's: {all:#?}"
    );
}

#[test]
fn findings_are_sorted_and_stable() {
    let body =
        "    match /b/{x} { allow write: if true; }\n    match /a/{x} { allow write: if true; }";
    let first = run(&firestore(body));
    let second = run(&firestore(body));
    assert_eq!(first, second);
    let lines: Vec<usize> = findings(&first).iter().map(|v| v.line).collect();
    let mut sorted = lines.clone();
    sorted.sort_unstable();
    assert_eq!(lines, sorted);
}

#[test]
fn a_named_database_is_named_in_the_finding() {
    let f = files(&[
        (
            "firebase.json",
            r#"{ "firestore": [ { "database": "analytics", "rules": "analytics.rules" } ] }"#,
        ),
        (
            "analytics.rules",
            "rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n    match /events/{e} { allow write: if true; }\n  }\n}\n",
        ),
    ]);
    let all = run(&f);
    let found = findings(&all);
    assert_eq!(found.len(), 1);
    assert!(
        found[0].message.contains("the rules for `analytics`"),
        "{}",
        found[0].message
    );
}
