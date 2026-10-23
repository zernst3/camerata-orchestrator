//! JSON-tree helpers: redact every string, and find every secret in every string. Both walk the
//! WHOLE tree (values and object keys), never a field list, so a field added tomorrow is covered.

use serde_json::{Map, Value};

use crate::detect::{detect_secrets, SecretMatch};
use crate::redact::{redact, redact_str};

/// Redact every string value (and object key) in a JSON tree in place; returns the number of
/// replaced matches.
pub fn redact_json_values(value: &mut Value) -> usize {
    match value {
        Value::String(text) => {
            let redacted = redact(text);
            let count = redacted.redactions.len();
            if count > 0 {
                *text = redacted.text;
            }
            count
        }
        Value::Array(items) => items.iter_mut().map(redact_json_values).sum(),
        Value::Object(map) => redact_object(map),
        Value::Null | Value::Bool(_) | Value::Number(_) => 0,
    }
}

fn redact_object(map: &mut Map<String, Value>) -> usize {
    let mut count = 0;
    let key_needs_redaction = map.keys().any(|key| !detect_secrets(key).is_empty());
    if key_needs_redaction {
        let entries = std::mem::take(map);
        for (key, mut value) in entries {
            count += redact_json_values(&mut value);
            let new_key = redact_str(&key).into_owned();
            if new_key != key {
                count += 1;
            }
            map.insert(new_key, value);
        }
    } else {
        for value in map.values_mut() {
            count += redact_json_values(value);
        }
    }
    count
}

/// A path segment for an object key. A key that is itself a secret is never echoed.
fn key_segment(key: &str) -> String {
    if detect_secrets(key).is_empty() {
        key.to_string()
    } else {
        "<redacted key>".to_string()
    }
}

/// Walk every string in a JSON tree and report non-twin matches with a JSON-pointer-style path
/// (`findings[0].also_locations[1].snippet`; the root is `$`). The release gate's JSON check.
pub fn find_secrets_in_json(value: &Value) -> Vec<(String, SecretMatch)> {
    let mut found = Vec::new();
    walk(value, "$".to_string(), &mut found);
    found
}

fn walk(value: &Value, path: String, found: &mut Vec<(String, SecretMatch)>) {
    match value {
        Value::String(text) => {
            for m in detect_secrets(text) {
                found.push((path.clone(), m));
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                walk(item, format!("{path}[{index}]"), found);
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                let child = if path == "$" {
                    key_segment(key)
                } else {
                    format!("{path}.{}", key_segment(key))
                };
                for m in detect_secrets(key) {
                    found.push((child.clone(), m));
                }
                walk(item, child, found);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}
