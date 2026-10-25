//! The deterministic edges: OWNERSHIP (7.4), FIELD-VALIDATION (7.5) and ADMIN-CLAIMS (7.9).

use super::{findings, firestore, rows, run};
use crate::arch_checker::{ViolationGrade, SEVERITY_HIGH, SEVERITY_MEDIUM};
use crate::firebase::{
    RULE_ADMIN_CLAIMS, RULE_AUTH_ONLY, RULE_FIELD_VALIDATION, RULE_OPEN, RULE_OWNERSHIP,
};

// ── OWNERSHIP ───────────────────────────────────────────────────────────────────────────────

#[test]
fn update_checking_only_the_incoming_owner_is_high() {
    let all = run(&firestore(
        "    match /posts/{postId} { allow update: if request.auth.uid == request.resource.data.authorId; }",
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].rule_id, RULE_OWNERSHIP);
    assert_eq!(f[0].severity, SEVERITY_HIGH);
    assert_eq!(f[0].grade, ViolationGrade::Defect);
    assert_eq!(f[0].line, 4);
    assert!(
        f[0].message.contains("INCOMING document"),
        "{}",
        f[0].message
    );
    assert!(f[0].message.contains("`request.resource.data.authorId`"));
    assert!(f[0].message.contains("`resource.data.authorId`"));
}

#[test]
fn write_checking_only_the_incoming_owner_is_high() {
    let all = run(&firestore(
        "    match /posts/{postId} { allow write: if request.auth.uid == request.resource.data.authorId; }",
    ));
    assert_eq!(
        rows(&all, RULE_OWNERSHIP, ViolationGrade::Defect).len(),
        1,
        "{all:#?}"
    );
}

#[test]
fn twin_create_checking_the_incoming_owner_is_correct() {
    let all = run(&firestore(
        "    match /posts/{postId} { allow create: if request.auth.uid == request.resource.data.authorId; }",
    ));
    assert!(findings(&all).is_empty(), "{all:#?}");
}

#[test]
fn twin_update_that_pins_the_owner_field_is_silent() {
    let all = run(&firestore(
        "    match /posts/{postId} { allow update: if request.auth.uid == request.resource.data.authorId && request.resource.data.authorId == resource.data.authorId; }",
    ));
    assert!(findings(&all).is_empty(), "{all:#?}");
}

#[test]
fn twin_wildcard_owner_comparison_is_silent() {
    let all = run(&firestore(
        "    match /users/{userId} { allow write: if request.auth.uid == userId && request.resource.data.keys().hasOnly(['name']); }",
    ));
    assert!(findings(&all).is_empty(), "{all:#?}");
}

#[test]
fn twin_owner_helper_reading_resource_data_is_silent() {
    let all = run(&firestore(
        "    function isOwner() { return request.auth.uid == resource.data.owner; }\n    match /posts/{p} { allow update: if isOwner() && request.resource.data.title is string; }",
    ));
    assert!(findings(&all).is_empty(), "{all:#?}");
}

#[test]
fn twin_disjunction_is_not_recognised() {
    let all = run(&firestore(
        "    match /posts/{p} { allow update: if request.auth.uid == request.resource.data.authorId || request.auth.token.admin == true; }",
    ));
    assert!(findings(&all).is_empty(), "{all:#?}");
}

// ── FIELD-VALIDATION ────────────────────────────────────────────────────────────────────────

#[test]
fn owner_scoped_write_with_no_field_validation_is_a_low_review_row() {
    let all = run(&firestore(
        "    match /users/{uid} { allow write: if request.auth.uid == uid; }",
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].rule_id, RULE_FIELD_VALIDATION);
    assert_eq!(f[0].grade, ViolationGrade::Review);
    assert_eq!(f[0].severity, SEVERITY_MEDIUM);
    assert_eq!(f[0].line, 4);
    assert!(f[0].message.contains("`/users/{uid}`"), "{}", f[0].message);
    assert!(f[0].message.contains("schemaless"));
    assert!(f[0].message.contains("validated elsewhere"));
}

#[test]
fn one_row_per_match_block_not_per_allow() {
    let all = run(&firestore(
        "    match /users/{uid} {\n      allow create: if request.auth.uid == uid;\n      allow update: if request.auth.uid == uid;\n    }",
    ));
    assert_eq!(
        rows(&all, RULE_FIELD_VALIDATION, ViolationGrade::Review).len(),
        1,
        "{all:#?}"
    );
}

#[test]
fn twin_has_only_key_allowlist_is_silent() {
    let all = run(&firestore(
        "    match /users/{uid} { allow write: if request.auth.uid == uid && request.resource.data.keys().hasOnly(['name', 'bio']); }",
    ));
    assert!(findings(&all).is_empty(), "{all:#?}");
}

#[test]
fn twin_affected_keys_diff_is_silent() {
    let all = run(&firestore(
        "    match /users/{uid} { allow update: if request.auth.uid == uid && request.resource.data.diff(resource.data).affectedKeys().hasOnly(['name']); }",
    ));
    assert!(findings(&all).is_empty(), "{all:#?}");
}

#[test]
fn twin_single_type_check_is_silent() {
    let all = run(&firestore(
        "    match /users/{uid} { allow write: if request.auth.uid == uid && request.resource.data.name is string; }",
    ));
    assert!(findings(&all).is_empty(), "{all:#?}");
}

#[test]
fn twin_open_or_auth_only_writes_belong_to_other_rules() {
    let open = run(&firestore("    match /a/{x} { allow write: if true; }"));
    assert_eq!(rows(&open, RULE_OPEN, ViolationGrade::Defect).len(), 1);
    assert!(rows(&open, RULE_FIELD_VALIDATION, ViolationGrade::Review).is_empty());
    let auth = run(&firestore(
        "    match /a/{x} { allow write: if request.auth != null; }",
    ));
    assert_eq!(rows(&auth, RULE_AUTH_ONLY, ViolationGrade::Defect).len(), 1);
    assert!(rows(&auth, RULE_FIELD_VALIDATION, ViolationGrade::Review).is_empty());
}

#[test]
fn twin_read_only_block_is_silent() {
    let all = run(&firestore(
        "    match /users/{uid} { allow read: if request.auth.uid == uid; }",
    ));
    assert!(findings(&all).is_empty(), "{all:#?}");
}

#[test]
fn twin_locked_and_delete_only_writes_are_silent() {
    let all = run(&firestore(
        "    match /a/{x} { allow write: if false; }\n    match /b/{x} { allow delete: if request.auth.uid == resource.data.owner; }",
    ));
    assert!(findings(&all).is_empty(), "{all:#?}");
}

// ── ADMIN-CLAIMS ────────────────────────────────────────────────────────────────────────────

const ADMIN_LOOKUP: &str =
    "get(/databases/$(database)/documents/users/$(request.auth.uid)).data.role == 'admin'";

#[test]
fn role_read_from_a_self_writable_user_document_is_high() {
    let all = run(&firestore(&format!(
        "    match /users/{{uid}} {{ allow read, write: if request.auth.uid == uid; }}\n    match /reports/{{r}} {{ allow write: if {ADMIN_LOOKUP}; }}"
    )));
    let hits = rows(&all, RULE_ADMIN_CLAIMS, ViolationGrade::Defect);
    assert_eq!(hits.len(), 1, "{all:#?}");
    assert_eq!(hits[0].severity, SEVERITY_HIGH);
    assert_eq!(hits[0].line, 5, "reported at the grant");
    assert!(hits[0].message.contains("line 4"), "{}", hits[0].message);
    assert!(hits[0].message.contains("`role`"));
    assert!(hits[0].message.contains("`users`"));
}

#[test]
fn role_lookup_through_a_helper_is_recognised() {
    let all = run(&firestore(&format!(
        "    function isAdmin() {{ return {ADMIN_LOOKUP}; }}\n    match /users/{{uid}} {{ allow write: if request.auth.uid == uid; }}\n    match /reports/{{r}} {{ allow write: if isAdmin(); }}"
    )));
    assert_eq!(
        rows(&all, RULE_ADMIN_CLAIMS, ViolationGrade::Defect).len(),
        1,
        "{all:#?}"
    );
}

#[test]
fn twin_role_document_with_no_client_write_is_silent() {
    for users in [
        "    match /users/{uid} { allow read: if request.auth.uid == uid; allow write: if false; }",
        "    match /users/{uid} { allow read: if request.auth.uid == uid; allow write: if request.auth.token.admin == true; }",
    ] {
        let all = run(&firestore(&format!(
            "{users}\n    match /reports/{{r}} {{ allow write: if {ADMIN_LOOKUP}; }}"
        )));
        assert!(
            rows(&all, RULE_ADMIN_CLAIMS, ViolationGrade::Defect).is_empty(),
            "{users}: {all:#?}"
        );
    }
}

#[test]
fn twin_owner_write_that_pins_the_role_field_is_silent() {
    let all = run(&firestore(&format!(
        "    match /users/{{uid}} {{ allow update: if request.auth.uid == uid && request.resource.data.role == resource.data.role; }}\n    match /reports/{{r}} {{ allow write: if {ADMIN_LOOKUP}; }}"
    )));
    assert!(
        rows(&all, RULE_ADMIN_CLAIMS, ViolationGrade::Defect).is_empty(),
        "{all:#?}"
    );
}

#[test]
fn twin_owner_write_with_has_only_excluding_role_is_silent() {
    let all = run(&firestore(&format!(
        "    match /users/{{uid}} {{ allow update: if request.auth.uid == uid && request.resource.data.diff(resource.data).affectedKeys().hasOnly(['displayName', 'photoURL']); }}\n    match /reports/{{r}} {{ allow write: if {ADMIN_LOOKUP}; }}"
    )));
    assert!(
        rows(&all, RULE_ADMIN_CLAIMS, ViolationGrade::Defect).is_empty(),
        "{all:#?}"
    );
}

#[test]
fn twin_custom_claims_check_is_silent() {
    let all = run(&firestore(
        "    match /users/{uid} { allow write: if request.auth.uid == uid; }\n    match /reports/{r} { allow write: if request.auth.token.admin == true; }",
    ));
    assert!(
        rows(&all, RULE_ADMIN_CLAIMS, ViolationGrade::Defect).is_empty(),
        "{all:#?}"
    );
}

#[test]
fn twin_lookup_keyed_on_another_document_is_silent() {
    let all = run(&firestore(
        "    match /users/{uid} { allow write: if request.auth.uid == uid; }\n    match /reports/{r} { allow write: if get(/databases/$(database)/documents/users/$(request.resource.data.ownerId)).data.role == 'admin'; }",
    ));
    assert!(
        rows(&all, RULE_ADMIN_CLAIMS, ViolationGrade::Defect).is_empty(),
        "{all:#?}"
    );
}
