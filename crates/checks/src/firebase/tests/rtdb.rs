//! FIREBASE-RTDB-RULES-OPEN-1 (design section 7.8) and the Realtime Database half of
//! FIREBASE-RULES-AUTH-ONLY-1 (section 7.3).

use super::{findings, rows, rtdb, run};
use crate::arch_checker::{ViolationGrade, SEVERITY_CRITICAL, SEVERITY_HIGH, SEVERITY_MEDIUM};
use crate::firebase::{RULE_AUTH_ONLY, RULE_RTDB_OPEN};

#[test]
fn root_read_and_write_true_is_critical() {
    let all = run(&rtdb(r#"{ "rules": { ".read": true, ".write": true } }"#));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "one merged row: {f:#?}");
    assert_eq!(f[0].rule_id, RULE_RTDB_OPEN);
    assert_eq!(f[0].severity, SEVERITY_CRITICAL);
    assert_eq!(f[0].grade, ViolationGrade::Defect);
    assert_eq!(f[0].file, "database.rules.json");
    assert_eq!(f[0].object.as_deref(), Some("/"));
    assert!(
        f[0].message.contains("the whole database"),
        "{}",
        f[0].message
    );
}

#[test]
fn root_read_true_alone_is_high() {
    let all = run(&rtdb(
        r#"{ "rules": { ".read": "true", ".write": false } }"#,
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].severity, SEVERITY_HIGH);
}

#[test]
fn named_node_write_true_is_high() {
    let all = run(&rtdb(
        r#"{ "rules": { "status": { "$uid": { ".write": true } } } }"#,
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].severity, SEVERITY_HIGH);
    assert_eq!(f[0].object.as_deref(), Some("/status/$uid"));
}

#[test]
fn named_node_read_true_is_review_graded() {
    let all = run(&rtdb(
        r#"{ "rules": { "profiles": { ".read": true, "$uid": { ".write": "$uid === auth.uid" } } } }"#,
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].grade, ViolationGrade::Review);
    assert_eq!(f[0].severity, SEVERITY_MEDIUM);
    assert_eq!(f[0].line, 1);
}

#[test]
fn a_child_denial_under_a_parent_grant_is_reported_as_ineffective() {
    let json = "{\n  \"rules\": {\n    \"users\": {\n      \".read\": true,\n      \"$uid\": {\n        \"private\": { \".read\": false }\n      }\n    }\n  }\n}";
    let all = run(&rtdb(json));
    let shadow: Vec<_> = findings(&all).into_iter().filter(|v| v.line == 6).collect();
    assert_eq!(shadow.len(), 1, "{all:#?}");
    assert_eq!(shadow[0].rule_id, RULE_RTDB_OPEN);
    assert_eq!(shadow[0].grade, ViolationGrade::Defect);
    assert_eq!(shadow[0].object.as_deref(), Some("/users/$uid/private"));
    assert!(
        shadow[0].message.contains("denial has no effect"),
        "{}",
        shadow[0].message
    );
    assert!(
        shadow[0].message.contains("`/users`"),
        "{}",
        shadow[0].message
    );
    assert!(shadow[0].message.contains("line 4"));
}

#[test]
fn twin_public_node_read_true_is_silent() {
    let all = run(&rtdb(r#"{ "rules": { "public": { ".read": true } } }"#));
    assert!(findings(&all).is_empty(), "{all:#?}");
}

#[test]
fn twin_owner_scoped_node_is_silent() {
    let all = run(&rtdb(
        r#"{ "rules": { "users": { "$uid": { ".read": "$uid === auth.uid", ".write": "$uid === auth.uid" } } } }"#,
    ));
    assert!(findings(&all).is_empty(), "{all:#?}");
}

#[test]
fn twin_validate_rules_are_ignored() {
    let all = run(&rtdb(
        r#"{ "rules": { "posts": { ".validate": "newData.isString()", ".indexOn": ["time"] } } }"#,
    ));
    assert!(findings(&all).is_empty(), "{all:#?}");
}

#[test]
fn malformed_rules_json_degrades_to_not_assessed() {
    let f = rtdb("{ \"rules\": { \".read\": true, ");
    let all = run(&f);
    assert_eq!(all.len(), 1, "{all:#?}");
    assert_eq!(all[0].grade, ViolationGrade::NotAssessed);
    assert_eq!(all[0].rule_id, RULE_RTDB_OPEN);
    assert!(
        all[0].message.contains("Realtime Database rules"),
        "{}",
        all[0].message
    );
    assert!(findings(&all).is_empty());
}

// ── the Realtime Database half of FIREBASE-RULES-AUTH-ONLY-1 ────────────────────────────────

#[test]
fn auth_only_in_rtdb_root_is_high() {
    let all = run(&rtdb(
        r#"{ "rules": { ".read": "auth.uid !== null", ".write": "auth != null" } }"#,
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].rule_id, RULE_AUTH_ONLY);
    assert_eq!(f[0].severity, SEVERITY_HIGH);
    assert_eq!(f[0].grade, ViolationGrade::Defect);
}

#[test]
fn auth_only_rtdb_named_read_is_review_graded_and_write_is_high() {
    let all = run(&rtdb(
        r#"{ "rules": { "rooms": { ".read": "auth != null" }, "notes": { ".write": "auth != null" } } }"#,
    ));
    assert_eq!(
        rows(&all, RULE_AUTH_ONLY, ViolationGrade::Review).len(),
        1,
        "{all:#?}"
    );
    assert_eq!(rows(&all, RULE_AUTH_ONLY, ViolationGrade::Defect).len(), 1);
}

#[test]
fn a_root_auth_only_grant_shadows_a_child_denial() {
    let all = run(&rtdb(
        r#"{ "rules": { ".read": "auth != null", "secrets": { ".read": false } } }"#,
    ));
    let f = findings(&all);
    assert_eq!(f.len(), 2, "{f:#?}");
    assert!(f.iter().any(|v| v.message.contains("denial has no effect")));
}

#[test]
fn a_renamed_rtdb_rules_file_is_found_through_firebase_json() {
    let f = super::files(&[
        (
            "firebase.json",
            r#"{ "database": { "rules": "config/rtdb.json" } }"#,
        ),
        ("config/rtdb.json", r#"{ "rules": { ".write": true } }"#),
    ]);
    let all = run(&f);
    assert_eq!(findings(&all).len(), 1, "{all:#?}");
    assert_eq!(findings(&all)[0].file, "config/rtdb.json");
}

#[test]
fn an_unrelated_json_file_is_never_read_as_rtdb_rules() {
    let f = super::files(&[
        ("firebase.json", "{}"),
        ("package.json", r#"{ "rules": { ".write": true } }"#),
    ]);
    let all = run(&f);
    assert!(findings(&all).is_empty(), "{all:#?}");
}
