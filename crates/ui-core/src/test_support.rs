//! Shared test helpers (compiled for tests only).

use std::collections::BTreeSet;

use serde_json::Value;

/// Every FIELD name written in `value`, recursively. `entry_maps` names the fields whose own keys
/// are data and not field names (the review log's maps, keyed by group and row keys): the keys
/// of those maps are not collected, the entries' fields are.
pub(crate) fn json_keys(value: &Value, entry_maps: &[&str], out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                out.insert(key.clone());
                if entry_maps.contains(&key.as_str()) {
                    for entry in inner.as_object().into_iter().flat_map(|m| m.values()) {
                        json_keys(entry, entry_maps, out);
                    }
                } else {
                    json_keys(inner, entry_maps, out);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|i| json_keys(i, entry_maps, out)),
        _ => {}
    }
}

/// The field maps of the review log, whose keys are group and row keys.
pub(crate) const LOG_ENTRY_MAPS: &[&str] = &["review_groups", "review_rows"];

/// The field names written by `value`, as a set.
pub(crate) fn keys_written(value: &Value, entry_maps: &[&str]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    json_keys(value, entry_maps, &mut out);
    out
}

/// Assert every key in `written` is on the internal-only deny-list or is one of its generic words.
pub(crate) fn assert_all_listed(written: &BTreeSet<String>, what: &str) {
    use camerata_api_types::review_timing::{GENERIC_KEYS, INTERNAL_ONLY_KEYS};
    for key in written {
        assert!(
            INTERNAL_ONLY_KEYS.contains(&key.as_str()) || GENERIC_KEYS.contains(&key.as_str()),
            "{what} writes `{key}`, which is not in camerata_api_types::review_timing"
        );
    }
}
