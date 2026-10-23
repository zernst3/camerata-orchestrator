//! The JSON-tree walkers: every string, not a field list.

use serde_json::json;

use crate::fixtures::planted;
use crate::{find_secrets_in_json, redact_json_values, SecretShape};

#[test]
fn finds_a_secret_in_a_field_nobody_listed_with_its_path() {
    let key = planted(SecretShape::SupabaseSecretKey).value;
    let value = json!({
        "findings": [
            {"snippet": "clean"},
            {"also_locations": [{"snippet": "clean"}, {"snippet": format!("KEY={key}")}]},
        ],
        "a_field_added_tomorrow": {"nested": [format!("x {key} y")]},
    });
    let found = find_secrets_in_json(&value);
    // Object key order depends on whether serde_json's `preserve_order` feature is unified in by
    // another crate of the build, so compare as a sorted set.
    let mut paths: Vec<&str> = found.iter().map(|(path, _)| path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(
        paths,
        vec![
            "a_field_added_tomorrow.nested[0]",
            "findings[1].also_locations[1].snippet",
        ]
    );
}

#[test]
fn a_root_string_is_reported_at_the_root_path() {
    let key = planted(SecretShape::GitHubToken).value;
    let found = find_secrets_in_json(&json!(key));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, "$");
}

#[test]
fn a_secret_used_as_an_object_key_is_found_and_never_echoed_in_the_path() {
    let key = planted(SecretShape::StripeLiveSecretKey).value;
    let mut map = serde_json::Map::new();
    map.insert(key.clone(), json!("value"));
    let found = find_secrets_in_json(&serde_json::Value::Object(map));
    assert_eq!(found.len(), 1);
    assert!(!found[0].0.contains(&key), "the path must not echo the key");
}

#[test]
fn twins_and_numbers_are_ignored() {
    let value = json!({"n": 42, "b": true, "z": null, "k": "sb_secret_your_key_here"});
    assert!(find_secrets_in_json(&value).is_empty());
}

#[test]
fn redact_json_values_rewrites_every_string_and_counts() {
    let a = planted(SecretShape::StripeLiveSecretKey).value;
    let b = planted(SecretShape::SupabaseSecretKey).value;
    let mut value = json!({
        "list": [format!("a {a}"), "clean", {"deep": format!("{b} and {a}")}],
        "number": 7,
    });
    let count = redact_json_values(&mut value);
    assert_eq!(count, 3);
    let text = value.to_string();
    assert!(!text.contains(&a) && !text.contains(&b));
    assert!(text.contains("sk_live_…") && text.contains("sb_secret_…"));
    assert!(find_secrets_in_json(&value).is_empty());
    assert_eq!(redact_json_values(&mut value), 0, "idempotent");
}

#[test]
fn redact_json_values_redacts_object_keys_too() {
    let key = planted(SecretShape::StripeLiveSecretKey).value;
    let mut map = serde_json::Map::new();
    map.insert(key.clone(), json!("v"));
    let mut value = serde_json::Value::Object(map);
    assert_eq!(redact_json_values(&mut value), 1);
    assert!(!value.to_string().contains(&key));
}
