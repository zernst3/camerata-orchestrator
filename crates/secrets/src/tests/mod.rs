//! Unit tests for the detector and redactor (spec section 8.1). Every secret value is built at
//! runtime by [`crate::fixtures`]; this crate's source contains no secret-shaped literal.

mod detection;
mod json_walk;
mod redaction;
mod rules;
mod source_hygiene;
mod twins;
mod vendors;

use crate::{detect_all, SecretMatch, SecretShape};

/// The single match in `text`, panicking with the shape list (never the text) otherwise.
fn only_match(text: &str) -> SecretMatch {
    let mut matches = detect_all(text);
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one match, found shapes {:?}",
        matches.iter().map(|m| m.shape).collect::<Vec<_>>()
    );
    matches.remove(0)
}

/// The last four chars of `value`.
fn tail(value: &str) -> String {
    value
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn count(value: &str) -> usize {
    value.chars().count()
}

fn all_shapes() -> &'static [SecretShape] {
    SecretShape::ALL
}
