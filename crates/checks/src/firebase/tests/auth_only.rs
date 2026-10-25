//! FIREBASE-RULES-AUTH-ONLY-1 (design section 7.3), over Firestore. Storage auth-only is owned by
//! FIREBASE-STORAGE-RULES-OPEN-1 and Realtime Database auth-only by the same rule family's RTDB
//! reader, so one fact has one owner (G7).

use super::{firestore, of, rows, run};
use crate::arch_checker::{ViolationGrade, SEVERITY_HIGH, SEVERITY_MEDIUM};
use crate::firebase::RULE_AUTH_ONLY;

#[test]
fn auth_only_at_the_firestore_root_is_high() {
    let all = run(&firestore(
        "    match /{document=**} { allow read, write: if request.auth.uid != null; }",
    ));
    let f = of(&all, RULE_AUTH_ONLY);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].rule_id, RULE_AUTH_ONLY);
    assert_eq!(f[0].severity, SEVERITY_HIGH);
    assert_eq!(f[0].grade, ViolationGrade::Defect);
    assert!(
        f[0].message
            .contains("read and write every document in the database"),
        "{}",
        f[0].message
    );
}

#[test]
fn auth_only_write_on_a_collection_is_high() {
    let all = run(&firestore(
        "    match /posts/{postId} { allow update, delete: if request.auth != null; }",
    ));
    let f = of(&all, RULE_AUTH_ONLY);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].severity, SEVERITY_HIGH);
    assert!(
        f[0].message.contains("not only their own"),
        "{}",
        f[0].message
    );
}

#[test]
fn auth_only_write_on_a_public_path_is_review_graded_medium() {
    for path in ["/public/{doc}", "/catalog/{item}", "/announcements/{a}"] {
        let all = run(&firestore(&format!(
            "    match {path} {{ allow update, delete: if request.auth != null; }}"
        )));
        assert!(
            rows(&all, RULE_AUTH_ONLY, ViolationGrade::Defect).is_empty(),
            "{path}: {all:#?}"
        );
        let f = rows(&all, RULE_AUTH_ONLY, ViolationGrade::Review);
        assert_eq!(f.len(), 1, "{path}: {all:#?}");
        assert_eq!(f[0].severity, SEVERITY_MEDIUM);
        assert!(
            f[0].message.contains("public by design"),
            "{}",
            f[0].message
        );
    }
}

#[test]
fn auth_only_write_on_a_shared_but_not_public_path_stays_asserted() {
    let all = run(&firestore(
        "    match /shared/{doc} { allow update, delete: if request.auth != null; }",
    ));
    assert_eq!(
        rows(&all, RULE_AUTH_ONLY, ViolationGrade::Defect).len(),
        1,
        "{all:#?}"
    );
}

#[test]
fn auth_only_read_on_a_collection_is_review_graded_medium() {
    let all = run(&firestore(
        "    match /users/{uid} { allow read: if request.auth != null; }",
    ));
    let f = of(&all, RULE_AUTH_ONLY);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].grade, ViolationGrade::Review);
    assert_eq!(f[0].severity, SEVERITY_MEDIUM);
    assert!(f[0].message.contains("shared among all users by design"));
}

#[test]
fn auth_only_through_a_helper_function_is_recognised() {
    let all = run(&firestore(
        "    function isSignedIn() { return request.auth != null; }\n    match /posts/{p} { allow read: if isSignedIn(); }",
    ));
    let hits = rows(&all, RULE_AUTH_ONLY, ViolationGrade::Review);
    assert_eq!(hits.len(), 1, "{all:#?}");
}

#[test]
fn twin_auth_plus_membership_check_is_silent() {
    let all = run(&firestore(
        "    match /messages/{m} { allow read: if request.auth != null && exists(/databases/$(database)/documents/rooms/$(roomId)/members/$(request.auth.uid)); }",
    ));
    assert!(of(&all, RULE_AUTH_ONLY).is_empty(), "{all:#?}");
}

#[test]
fn twin_email_verified_conjunct_is_silent() {
    let all = run(&firestore(
        "    match /posts/{p} { allow write: if request.auth != null && request.auth.token.email_verified == true; }",
    ));
    assert!(of(&all, RULE_AUTH_ONLY).is_empty(), "{all:#?}");
}

#[test]
fn twin_owner_check_through_helper_is_not_auth_only() {
    let all = run(&firestore(
        "    function isOwner(userId) { return request.auth.uid == userId; }\n    match /users/{userId} { allow read, write: if isOwner(userId); }",
    ));
    assert!(of(&all, RULE_AUTH_ONLY).is_empty(), "{all:#?}");
}

#[test]
fn twin_public_collection_auth_only_read_is_silent() {
    let all = run(&firestore(
        "    match /catalog/{item} { allow read: if request.auth != null; }",
    ));
    assert!(of(&all, RULE_AUTH_ONLY).is_empty(), "{all:#?}");
}

#[test]
fn auth_only_create_alone_is_not_a_finding() {
    // Creating a document does not change or delete anyone else's data.
    let all = run(&firestore(
        "    match /posts/{p} { allow create: if request.auth != null; }",
    ));
    assert!(of(&all, RULE_AUTH_ONLY).is_empty(), "{all:#?}");
}
