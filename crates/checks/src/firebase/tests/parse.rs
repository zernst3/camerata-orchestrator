//! The reader, the file-set resolver and the condition recognisers.

use chrono::NaiveDate;

use super::files;
use crate::firebase::config::{resolve, RulesKind};
use crate::firebase::rtdb::{parse_rtdb, Key};
use crate::firebase::rules_model::{Cond, Target};
use crate::firebase::rules_parse::{parse_rules, render_path, Method, Scope, Segment, Service};

/// The template `firebase init` writes for Firestore (the test-mode scaffold).
const INIT_TEMPLATE: &str = "rules_version = '2';
service cloud.firestore {
  match /databases/{database}/documents {

    // This rule allows anyone with your database reference to view, edit,
    // and delete all data in your Firestore database. It is useful for getting
    // started, but it is configured to expire after 30 days because it
    // leaves your app open to attackers.
    match /{document=**} {
      allow read, write: if request.time < timestamp.date(2026, 11, 24);
    }
  }
}
";

fn no_scope() -> Scope<'static> {
    Scope::default()
}

fn cond(raw: &str) -> Cond {
    Cond::new(Some(raw), &no_scope())
}

#[test]
fn parses_the_firebase_init_default_firestore_template() {
    let file = parse_rules(INIT_TEMPLATE);
    assert!(file.errors.is_empty(), "{:?}", file.errors);
    assert_eq!(file.version, Some(2));
    assert_eq!(file.service, Some(Service::Firestore));
    let blocks = file.flatten();
    assert_eq!(blocks.len(), 2);
    let root = blocks[1].block;
    assert_eq!(
        render_path(&root.effective_path),
        "/databases/{database}/documents/{document=**}"
    );
    assert_eq!(root.allows.len(), 1);
    let allow = &root.allows[0];
    assert_eq!(allow.line, 10);
    assert_eq!(
        allow.methods.iter().copied().collect::<Vec<_>>(),
        vec![Method::Read, Method::Write]
    );
    let c = cond(allow.condition.as_deref().unwrap());
    assert_eq!(c.test_mode_expiry(), NaiveDate::from_ymd_opt(2026, 11, 24));
}

#[test]
fn no_rules_version_means_the_v1_engine() {
    let file = parse_rules("service cloud.firestore { match /databases/{d}/documents { } }");
    assert_eq!(file.version, None);
    assert!(file.errors.is_empty(), "{:?}", file.errors);
}

#[test]
fn parses_nested_and_flat_match_forms_to_the_same_effective_path() {
    let nested = parse_rules(
        "service cloud.firestore { match /databases/{database}/documents {
           match /cities/{city} { allow read; match /landmarks/{landmark} { allow read; } } } }",
    );
    let flat = parse_rules(
        "service cloud.firestore { match /databases/{database}/documents {
           match /cities/{city}/landmarks/{landmark} { allow read; } } }",
    );
    assert!(nested.errors.is_empty() && flat.errors.is_empty());
    let nested_inner = nested
        .flatten()
        .last()
        .unwrap()
        .block
        .effective_path
        .clone();
    let flat_inner = flat.flatten().last().unwrap().block.effective_path.clone();
    assert_eq!(nested_inner, flat_inner);
    assert_eq!(
        render_path(&flat_inner),
        "/databases/{database}/documents/cities/{city}/landmarks/{landmark}"
    );
}

#[test]
fn parses_functions_with_lets_and_inlines_them_into_conditions() {
    let file = parse_rules(
        "service cloud.firestore { match /databases/{database}/documents {
           function isSignedIn() { return request.auth != null; }
           function isOwner(userId) { let uid = request.auth.uid; return isSignedIn() && uid == userId; }
           match /users/{userId} { allow read, write: if isOwner(userId); }
           match /posts/{p} { allow read: if isSignedIn(); }
         } }",
    );
    assert!(file.errors.is_empty(), "{:?}", file.errors);
    let blocks = file.flatten();
    let users = blocks
        .iter()
        .find(|b| render_path(&b.block.segments) == "/users/{userId}")
        .unwrap();
    let c = Cond::new(users.block.allows[0].condition.as_deref(), &users.scope);
    assert_eq!(c.text(), "request.auth!=null&&request.auth.uid==userId");
    assert!(!c.is_auth_only(), "an owner check is not auth-only");
    assert_eq!(
        c.uid_targets(&users.scope.wildcards),
        vec![Target::Wildcard("userId".to_string())]
    );
    let posts = blocks
        .iter()
        .find(|b| render_path(&b.block.segments) == "/posts/{p}")
        .unwrap();
    let c = Cond::new(posts.block.allows[0].condition.as_deref(), &posts.scope);
    assert!(c.is_auth_only(), "{}", c.text());
}

#[test]
fn string_contents_with_braces_semicolons_and_slashes_do_not_split_statements() {
    let file = parse_rules(
        "service cloud.firestore { match /databases/{database}/documents {
           match /t/{x} { allow read: if resource.data.tag.matches('a;{b}/c'); allow write: if false; } } }",
    );
    assert!(file.errors.is_empty(), "{:?}", file.errors);
    let block = file.flatten().last().unwrap().block;
    assert_eq!(block.allows.len(), 2);
    assert_eq!(
        block.allows[0].condition.as_deref(),
        Some("resource.data.tag.matches('a;{b}/c')")
    );
}

#[test]
fn block_comments_and_line_comments_are_dropped_and_lines_are_kept() {
    let file = parse_rules(
        "// header\nservice cloud.firestore {\n  /* a { block ;\n comment */\n  match /databases/{d}/documents {\n    match /a/{x} {\n      allow read; // trailing\n    }\n  }\n}\n",
    );
    assert!(file.errors.is_empty(), "{:?}", file.errors);
    let block = file.flatten().last().unwrap().block;
    assert_eq!(block.line, 6);
    assert_eq!(block.allows[0].line, 7);
}

#[test]
fn an_unterminated_string_or_block_degrades_the_file_and_never_panics() {
    let truncated = parse_rules("service cloud.firestore { match /databases/{d}/documents { match /a/{x} { allow read: if resource.data.n == 'oops");
    assert!(truncated.is_degraded());
    let unclosed_block = parse_rules("service cloud.firestore { match /databases/{d}/documents {");
    assert!(unclosed_block.is_degraded());
    let stray_close = parse_rules("service cloud.firestore { } }");
    assert!(stray_close.is_degraded());
    let comment = parse_rules("service cloud.firestore { /* never closed");
    assert!(comment.is_degraded());
    // Binary noise, and a hostile file of many openers: no panic, no overflow, degraded.
    let noise: String = (0..4096u32)
        .map(|i| char::from_u32(0x20 + (i * 7919) % 0x2fff).unwrap_or('?'))
        .collect();
    let _ = parse_rules(&noise);
    assert!(parse_rules(&"{".repeat(100_000)).is_degraded());
    let deep = "match /a {".repeat(5_000);
    assert!(parse_rules(&format!("service cloud.firestore {{ {deep}")).is_degraded());
    assert!(parse_rules("").service.is_none());
}

#[test]
fn an_unknown_statement_degrades_the_file() {
    let file = parse_rules(
        "service cloud.firestore { match /databases/{d}/documents { match /a/{x} { deny read; } } }",
    );
    assert!(file.is_degraded());
    assert_eq!(file.errors[0].line, 1);
    let missing_semicolon = parse_rules(
        "service cloud.firestore { match /databases/{d}/documents { match /a/{x} { allow read: if true } } }",
    );
    assert!(missing_semicolon.is_degraded());
    let bad_method = parse_rules(
        "service cloud.firestore { match /databases/{d}/documents { match /a/{x} { allow fetch; } } }",
    );
    assert!(bad_method.is_degraded());
}

#[test]
fn a_service_other_than_firestore_or_storage_is_not_a_firebase_rules_file() {
    let file = parse_rules("service cloud.spanner { match /x { allow read; } }");
    assert_eq!(
        file.service,
        Some(Service::Other("cloud.spanner".to_string()))
    );
    let set = resolve(&files(&[(
        "rules/spanner.rules",
        "service cloud.spanner { match /x { allow read; } }",
    )]));
    assert!(set.assessed.is_empty());
    assert!(set.unreferenced.is_empty(), "left alone, with no row");
}

#[test]
fn a_udev_or_prometheus_rules_file_is_ignored_without_error() {
    let udev = "ACTION==\"add\", SUBSYSTEM==\"usb\", ATTR{idVendor}==\"1234\", MODE=\"0666\"\n";
    let prom = "groups:\n- name: a\n  rules:\n  - alert: Down\n    expr: up == 0\n";
    assert!(parse_rules(udev).service.is_none());
    let set = resolve(&files(&[
        ("etc/udev/70-usb.rules", udev),
        ("monitoring/alerts.rules", prom),
    ]));
    assert!(set.assessed.is_empty());
    assert!(set.unreferenced.is_empty());
    assert!(set.unreadable_configs.is_empty());
}

const RTDB_CASCADE: &str = r#"{
  // comments are allowed
  "rules": {
    "users": {
      ".read": true,
      "$uid": {
        ".write": "$uid === auth.uid",
        "private": { ".read": false }
      }
    }
  }
}"#;

#[test]
fn rtdb_rules_cascade_a_parent_grant_to_children() {
    let file = parse_rtdb(RTDB_CASCADE);
    assert!(file.errors.is_empty(), "{:?}", file.errors);
    let flat = file.flatten();
    let private = flat
        .iter()
        .find(|f| f.node.path == "/users/$uid/private")
        .unwrap();
    let (granting, rule) = private.granting(Key::Read, Cond::is_true).unwrap();
    assert_eq!(granting.path, "/users");
    assert_eq!(rule.line, 5);
    // The child's own denial is on line 8 and cannot take the parent's grant away.
    assert_eq!(private.node.read.as_ref().unwrap().line, 8);
    assert!(private.node.read.as_ref().unwrap().cond.is_false());
    let owner = flat.iter().find(|f| f.node.path == "/users/$uid").unwrap();
    assert!(owner.granting(Key::Write, Cond::is_true).is_none());
}

#[test]
fn rtdb_rules_that_are_not_a_rules_object_degrade() {
    assert!(parse_rtdb("{}").is_degraded());
    assert!(parse_rtdb("[1,2]").is_degraded());
    assert!(parse_rtdb("{\"rules\": [").is_degraded());
    assert!(parse_rtdb("{\"rules\": {\".read\": 5}}").is_degraded());
    assert!(parse_rtdb("not json at all").is_degraded());
    assert!(parse_rtdb(&"{\"a\":".repeat(100_000)).is_degraded());
    assert!(!parse_rtdb("{\"rules\": {\".validate\": \"newData.isString()\"}}").is_degraded());
}

#[test]
fn firebase_json_names_rules_files_relative_to_its_own_directory() {
    let set = resolve(&files(&[
        (
            "apps/a/firebase.json",
            r#"{ "firestore": { "rules": "rules/fs.rules" }, "storage": { "rules": "../shared/st.rules" } }"#,
        ),
        ("apps/a/rules/fs.rules", "service cloud.firestore { }"),
        ("apps/shared/st.rules", "service firebase.storage { }"),
        ("firestore.rules", "service cloud.firestore { }"),
    ]));
    let paths: Vec<(&str, RulesKind)> = set
        .assessed
        .iter()
        .map(|a| (a.path.as_str(), a.kind))
        .collect();
    assert_eq!(
        paths,
        vec![
            ("apps/a/rules/fs.rules", RulesKind::Firestore),
            ("apps/shared/st.rules", RulesKind::Storage),
        ]
    );
    // The root `firestore.rules` is not what apps/a/firebase.json deploys.
    assert!(set.unreferenced.iter().any(|u| u.path == "firestore.rules"));
}

#[test]
fn multi_database_and_multi_bucket_arrays_resolve_every_named_file() {
    let set = resolve(&files(&[
        (
            "firebase.json",
            r#"{
              "firestore": [
                { "database": "(default)", "rules": "fs.default.rules" },
                { "database": "analytics", "rules": "fs.analytics.rules" }
              ],
              "storage": [ { "bucket": "b1", "rules": "st.rules" } ],
              "database": { "rules": "rtdb.json" }
            }"#,
        ),
        ("fs.default.rules", "service cloud.firestore { }"),
        ("fs.analytics.rules", "service cloud.firestore { }"),
        ("st.rules", "service firebase.storage { }"),
        ("rtdb.json", "{\"rules\": {}}"),
    ]));
    assert_eq!(set.assessed.len(), 4);
    let analytics = set
        .assessed
        .iter()
        .find(|a| a.path == "fs.analytics.rules")
        .unwrap();
    assert_eq!(analytics.label.as_deref(), Some("analytics"));
    assert!(set
        .assessed
        .iter()
        .any(|a| a.path == "rtdb.json" && a.kind == RulesKind::Rtdb));
}

#[test]
fn conventional_files_beside_firebase_json_are_the_defaults() {
    let set = resolve(&files(&[
        ("firebase.json", "{}"),
        ("firestore.rules", "service cloud.firestore { }"),
        ("storage.rules", "service firebase.storage { }"),
        ("database.rules.json", "{\"rules\": {}}"),
    ]));
    assert_eq!(set.assessed.len(), 3);
    assert!(set.assessed.iter().all(|a| !a.config_unreadable));
    // An unreadable firebase.json still resolves by convention, and says so.
    let broken = resolve(&files(&[
        ("firebase.json", "{ \"firestore\": "),
        ("firestore.rules", "service cloud.firestore { }"),
    ]));
    assert_eq!(broken.unreadable_configs, vec!["firebase.json".to_string()]);
    assert_eq!(broken.assessed.len(), 1);
    assert!(broken.assessed[0].config_unreadable);
}

#[test]
fn with_no_firebase_json_the_conventional_basenames_are_assessed_anywhere() {
    let set = resolve(&files(&[
        ("backend/firestore.rules", "service cloud.firestore { }"),
        ("test/firestore.rules", "service cloud.firestore { }"),
    ]));
    assert!(!set.has_firebase_json);
    let paths: Vec<&str> = set.assessed.iter().map(|a| a.path.as_str()).collect();
    assert_eq!(paths, vec!["backend/firestore.rules"]);
}

#[test]
fn an_unreferenced_rules_file_is_parsed_but_produces_no_rule_findings() {
    let set = resolve(&files(&[
        (
            "firebase.json",
            r#"{ "firestore": { "rules": "firestore.rules" } }"#,
        ),
        ("firestore.rules", "service cloud.firestore { }"),
        ("rules/firestore.dev.rules", "service cloud.firestore { }"),
    ]));
    assert_eq!(set.assessed.len(), 1);
    assert_eq!(set.unreferenced.len(), 1);
    assert_eq!(set.unreferenced[0].path, "rules/firestore.dev.rules");
}

#[test]
fn a_test_rules_file_is_skipped_unless_firebase_json_names_it() {
    let skipped = resolve(&files(&[
        (
            "firebase.json",
            r#"{ "firestore": { "rules": "firestore.rules" } }"#,
        ),
        ("firestore.rules", "service cloud.firestore { }"),
        ("test/firestore.test.rules", "service cloud.firestore { }"),
        ("emulators/dev.rules", "service cloud.firestore { }"),
    ]));
    assert_eq!(skipped.assessed.len(), 1);
    assert!(skipped.unreferenced.is_empty(), "silently skipped, no row");
    let named = resolve(&files(&[
        (
            "firebase.json",
            r#"{ "firestore": { "rules": "test/firestore.test.rules" } }"#,
        ),
        ("test/firestore.test.rules", "service cloud.firestore { }"),
    ]));
    assert_eq!(named.assessed.len(), 1);
}

#[test]
fn a_file_firebase_json_names_but_the_scan_did_not_read_is_reported_missing() {
    let set = resolve(&files(&[(
        "firebase.json",
        r#"{ "firestore": { "rules": "gone.rules" } }"#,
    )]));
    assert!(set.assessed.is_empty());
    assert_eq!(set.missing_named[0].path, "gone.rules");
}

#[test]
fn recognisers_match_exact_shapes_only() {
    for yes in [
        "request.auth != null",
        "null != request.auth",
        "request.auth.uid != null",
        "request.auth.uid != ''",
        "request.auth.uid != \"\"",
        "request.auth.token != null",
        "request.auth != null && request.auth.uid != null",
        "(request.auth != null)",
        "auth != null",
        "auth.uid !== null",
        "request.auth != null || request.auth.uid != null",
    ] {
        assert!(cond(yes).is_auth_only(), "{yes}");
    }
    for no in [
        "request.auth != null && request.auth.token.email_verified",
        "request.auth != null && request.auth.token.email_verified == true",
        "request.auth.uid == userId",
        "request.auth != null && exists(/databases/$(database)/documents/rooms/$(roomId))",
        "true",
    ] {
        assert!(!cond(no).is_auth_only(), "{no}");
    }
    assert!(cond("true").is_true());
    assert!(cond("request.auth != null || true").is_true());
    assert!(cond("(true)").is_true());
    assert!(!cond("request.auth != null && true").is_true());
    assert!(!cond("resource.data.visibility == 'public'").is_true());
    assert!(Cond::new(None, &no_scope()).is_true());
    assert!(cond("false").is_false());
    assert!(cond("request.auth != null && false").is_false());
    assert!(!cond("request.auth != null || false").is_false());
    // Test mode.
    assert_eq!(
        cond("request.time < timestamp.date(2027, 1, 1)").test_mode_expiry(),
        NaiveDate::from_ymd_opt(2027, 1, 1)
    );
    assert_eq!(
        cond("request.time <= timestamp.date(2027, 1, 1) || request.auth.uid == 'x'")
            .test_mode_expiry(),
        NaiveDate::from_ymd_opt(2027, 1, 1)
    );
    assert_eq!(
        cond("request.time < timestamp.value(1798761600000)").test_mode_expiry(),
        NaiveDate::from_ymd_opt(2027, 1, 1)
    );
    for no in [
        "request.time > timestamp.date(2027, 1, 1)",
        "request.time < timestamp.date(2027, 1, 1) && request.auth != null",
        "request.time < timestamp.date(y, m, d)",
        "request.time < timestamp.date(2027, 13, 1)",
    ] {
        assert_eq!(cond(no).test_mode_expiry(), None, "{no}");
    }
}

#[test]
fn field_recognisers_read_the_incoming_and_stored_documents() {
    let c = cond("request.auth.uid == request.resource.data.authorId");
    assert_eq!(
        c.uid_targets(&[]),
        vec![Target::Incoming("authorId".to_string())]
    );
    let c = cond("resource.data.owner == request.auth.uid");
    assert_eq!(
        c.uid_targets(&[]),
        vec![Target::Existing("owner".to_string())]
    );
    assert!(cond("request.auth.uid == a || request.auth.uid == b").has_disjunction());
    assert!(cond("request.resource.data.keys().hasOnly(['a', 'b'])").validates_fields());
    assert!(cond("request.resource.data.name is string").validates_fields());
    assert!(!cond("request.auth.uid == userId").validates_fields());
    assert!(cond("request.resource.data.role == resource.data.role").pins_field("role", &[]));
    assert!(cond(
        "request.resource.data.diff(resource.data).affectedKeys().hasOnly(['displayName'])"
    )
    .pins_field("role", &[]));
    assert!(!cond(
        "request.resource.data.diff(resource.data).affectedKeys().hasOnly(['displayName', 'role'])"
    )
    .pins_field("role", &[]));
    assert!(!cond("request.auth.uid == userId").pins_field("role", &[]));
    let lookups = cond(
        "get(/databases/$(database)/documents/users/$(request.auth.uid)).data.role == 'admin'",
    )
    .role_lookups();
    assert_eq!(lookups.len(), 1);
    assert_eq!(lookups[0].collection, "users");
    assert_eq!(lookups[0].field, "role");
    assert!(cond(
        "get(/databases/$(database)/documents/users/$(request.resource.data.ownerId)).data.role == 'admin'"
    )
    .role_lookups()
    .is_empty());
}

#[test]
fn the_json_reader_is_lenient_about_comments_and_trailing_commas() {
    let (value, errors) = crate::firebase::json_lite::parse(
        "{ /* c */ \"a\": [1, 2,], // x\n \"b\": {\"c\": null,}, }",
    );
    assert!(errors.is_empty(), "{errors:?}");
    let value = value.unwrap();
    assert_eq!(value.get("b").unwrap().line, 2);
    let (_, errors) = crate::firebase::json_lite::parse("{ \"a\": }");
    assert!(!errors.is_empty());
}

#[test]
fn segments_render_as_written() {
    assert_eq!(
        Segment::Wildcard {
            name: "x".into(),
            recursive: true
        }
        .render(),
        "{x=**}"
    );
}
