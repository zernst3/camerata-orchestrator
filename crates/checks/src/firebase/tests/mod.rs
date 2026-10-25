//! Tests for the Firebase module, one file per concern (design section 5.6 and section 7).

mod parse;

/// `(path, content)` pairs, the shape every checker input takes.
pub(crate) fn files(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(p, c)| (p.to_string(), c.to_string()))
        .collect()
}
