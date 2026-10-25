//! FIREBASE-STORAGE-RULES-OPEN-1 (design section 7.7).

use super::{files, findings, firestore, rows, run, storage};
use crate::arch_checker::{ViolationGrade, SEVERITY_CRITICAL, SEVERITY_HIGH, SEVERITY_MEDIUM};
use crate::firebase::{RULE_AUTH_ONLY, RULE_OPEN, RULE_STORAGE_OPEN};

#[test]
fn open_write_on_a_storage_folder_is_high() {
    let all = run(&storage(
        "    match /uploads/{userId}/{file} { allow write: if true; }",
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].rule_id, RULE_STORAGE_OPEN);
    assert_eq!(f[0].severity, SEVERITY_HIGH);
    assert_eq!(f[0].grade, ViolationGrade::Defect);
    assert_eq!(f[0].file, "storage.rules");
    assert_eq!(f[0].object.as_deref(), Some("/uploads/{userId}/{file}"));
    assert!(
        f[0].message.contains("upload, replace and delete"),
        "{}",
        f[0].message
    );
}

#[test]
fn open_upload_alone_is_high_in_storage() {
    let all = run(&storage(
        "    match /inbox/{file} { allow create: if true; }",
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].severity, SEVERITY_HIGH);
}

#[test]
fn open_read_and_write_at_the_storage_root_is_critical() {
    let all = run(&storage(
        "    match /{allPaths=**} { allow read, write: if true; }",
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].severity, SEVERITY_CRITICAL);
    assert!(f[0].message.contains("in the bucket"), "{}", f[0].message);
}

#[test]
fn auth_only_write_anywhere_is_high() {
    let all = run(&storage(
        "    match /shared/{file} { allow write: if request.auth != null; }",
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].rule_id, RULE_STORAGE_OPEN);
    assert_eq!(f[0].severity, SEVERITY_HIGH);
    assert!(
        f[0].message.contains("not only their own"),
        "{}",
        f[0].message
    );
}

/// Firebase's own Storage example (the "Public" folder with `allow read; allow write: if
/// request.auth != null`): the vocabulary says the folder is meant to be public, so a signed-in
/// write there is held for review rather than asserted.
#[test]
fn firebase_docs_public_folder_example_is_review_graded_not_a_defect() {
    let all = run(&storage(
        "    match /images/{imageId} {\n      allow read: if request.auth != null;\n      allow write: if request.auth != null && request.resource.size < 5 * 1024 * 1024 && request.resource.contentType.matches('image/.*');\n    }\n    match /users/{userId}/{allPaths=**} {\n      allow read, write: if request.auth != null && request.auth.uid == userId;\n    }\n    match /public/{imageId} {\n      allow read;\n      allow write: if request.auth != null;\n    }",
    ));
    assert!(
        rows(&all, RULE_STORAGE_OPEN, ViolationGrade::Defect).is_empty(),
        "no asserted finding: {all:#?}"
    );
    let public: Vec<_> = rows(&all, RULE_STORAGE_OPEN, ViolationGrade::Review)
        .into_iter()
        .filter(|v| v.object.as_deref() == Some("/public/{imageId}"))
        .collect();
    assert_eq!(public.len(), 1, "{all:#?}");
    assert_eq!(public[0].severity, SEVERITY_MEDIUM);
    assert_eq!(public[0].line, 13);
    assert!(
        public[0].message.contains("public by design"),
        "{}",
        public[0].message
    );
}

/// The same fact with the owner's Storage vocabulary: an auth-only update or delete on a path that
/// is not public stays asserted.
#[test]
fn auth_only_replace_and_delete_on_a_non_public_folder_stay_asserted() {
    let all = run(&storage(
        "    match /documents/{file} { allow update, delete: if request.auth != null; }",
    ));
    let f = rows(&all, RULE_STORAGE_OPEN, ViolationGrade::Defect);
    assert_eq!(f.len(), 1, "{all:#?}");
    assert_eq!(f[0].severity, SEVERITY_HIGH);
}

/// An OPEN write (signed in or not) to a public folder is not softened: only auth-only is.
#[test]
fn an_open_write_to_a_public_folder_stays_asserted() {
    let all = run(&storage(
        "    match /public/{file} { allow write: if true; }",
    ));
    let f = rows(&all, RULE_STORAGE_OPEN, ViolationGrade::Defect);
    assert_eq!(f.len(), 1, "{all:#?}");
    assert_eq!(f[0].severity, SEVERITY_HIGH);
}

#[test]
fn auth_only_read_at_the_root_is_medium() {
    // The old Storage default, read side.
    let all = run(&storage(
        "    match /{allPaths=**} { allow read: if request.auth != null; }",
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].severity, SEVERITY_MEDIUM);
    assert_eq!(f[0].grade, ViolationGrade::Defect);
}

#[test]
fn auth_only_in_storage_root_is_high() {
    // The old Storage default in full: read and write for any signed-in user. It is Storage's
    // fact, so it carries the Storage rule id, not the Firestore AUTH-ONLY one.
    let all = run(&storage(
        "    match /{allPaths=**} { allow read, write: if request.auth != null; }",
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].rule_id, RULE_STORAGE_OPEN);
    assert_eq!(f[0].severity, SEVERITY_HIGH);
    assert!(rows(&all, RULE_AUTH_ONLY, ViolationGrade::Defect).is_empty());
}

#[test]
fn open_read_on_uploads_is_review_graded() {
    let all = run(&storage(
        "    match /uploads/{userId}/{file} {\n      allow read;\n      allow write: if request.auth.uid == userId;\n    }",
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].grade, ViolationGrade::Review);
    assert_eq!(f[0].severity, SEVERITY_MEDIUM);
    assert_eq!(f[0].line, 5);
    assert!(
        f[0].message.contains("anyone with the URL"),
        "{}",
        f[0].message
    );
    assert!(f[0].message.contains("`/uploads/{userId}/{file}`"));
}

#[test]
fn twin_public_folder_open_read_is_silent() {
    for folder in ["public", "static", "assets"] {
        let all = run(&storage(&format!(
            "    match /{folder}/{{file}} {{ allow read: if true; }}"
        )));
        assert!(findings(&all).is_empty(), "{folder}: {all:#?}");
    }
}

#[test]
fn twin_size_capped_public_read_is_silent() {
    let all = run(&storage(
        "    match /public/{imageId} {\n      allow read;\n      allow write: if request.resource.size < 5 * 1024 * 1024 && request.resource.contentType.matches('image/.*');\n    }",
    ));
    assert!(findings(&all).is_empty(), "{all:#?}");
}

#[test]
fn twin_owner_scoped_folder_is_silent() {
    let all = run(&storage(
        "    match /users/{userId}/{file} { allow read, write: if request.auth.uid == userId; }",
    ));
    assert!(findings(&all).is_empty(), "{all:#?}");
}

#[test]
fn twin_firestore_file_is_not_this_rule() {
    let all = run(&firestore(
        "    match /orders/{id} { allow write: if true; }",
    ));
    assert!(rows(&all, RULE_STORAGE_OPEN, ViolationGrade::Defect).is_empty());
    assert_eq!(rows(&all, RULE_OPEN, ViolationGrade::Defect).len(), 1);
}

#[test]
fn the_legacy_cloud_storage_service_header_is_storage() {
    let f = files(&[
        ("firebase.json", r#"{ "storage": { "rules": "storage.rules" } }"#),
        (
            "storage.rules",
            "service cloud.storage {\n  match /b/{bucket}/o {\n    match /x/{f} { allow write: if true; }\n  }\n}\n",
        ),
    ]);
    let all = run(&f);
    assert_eq!(
        rows(&all, RULE_STORAGE_OPEN, ViolationGrade::Defect).len(),
        1,
        "{all:#?}"
    );
}
