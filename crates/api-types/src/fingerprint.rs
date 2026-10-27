//! The finding fingerprint, defined once for every crate that needs it.
//!
//! A fingerprint is the identity of a violation's SHAPE: the rule plus the offending snippet with
//! its whitespace collapsed. Two places use it and must agree to the byte:
//!
//! * the server's suppression baseline (`camerata_server::suppression`), which COMMITS the value
//!   to `.camerata/baseline.json` and matches it on later scans, and
//! * the review UI's decision memory (`camerata_ui_core::decision_memory`), which recognises "the
//!   same rule on the same shape of line" inside one draft.
//!
//! It lives in this leaf crate (no `camerata-*` dependency) so both can call the same function
//! instead of the UI keeping a copy that could drift from the baseline's. The pinned values in the
//! tests below are the ones the baseline wrote before the function moved here; a change to the
//! algorithm would silently un-baseline every committed suppression, so they must not change.

/// A stable, dependency-free FNV-1a 64-bit hash. Stable across machines and Rust versions (unlike
/// `DefaultHasher`), which a COMMITTED baseline file requires. Also used by the server's scan cache
/// to fingerprint whole-file content with the same hash.
pub fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Fingerprint a violation by rule + normalized offending snippet. Whitespace is collapsed so
/// reformatting/indent shifts don't break the match, but a real edit to the offending code does
/// (so touching debt un-baselines it, which is the ratchet).
pub fn fingerprint(rule_id: &str, snippet: &str) -> String {
    let norm = snippet.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{:016x}", fnv1a(&format!("{rule_id}|{norm}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference values for FNV-1a 64 (the published test vectors).
    #[test]
    fn fnv1a_matches_the_published_vectors() {
        assert_eq!(fnv1a(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a("a"), 0xaf63_dc4c_8601_ec8c);
    }

    /// Values the baseline produced BEFORE the function moved here. If one changes, every
    /// committed baseline entry stops matching.
    #[test]
    fn fingerprints_are_the_values_the_baseline_already_committed() {
        assert_eq!(fingerprint("SEC-X", "let t = \"abc\";"), "61727d97b8fb274a");
        assert_eq!(fingerprint("API-1", "code at 7"), "2158fd5ef247a6a2");
        assert_eq!(fingerprint("R", "fn foo () {}"), "c3410fbee30c442a");
    }

    #[test]
    fn whitespace_is_collapsed_and_the_rule_is_part_of_the_shape() {
        assert_eq!(
            fingerprint("R", "  fn\tfoo ()  {}\n"),
            fingerprint("R", "fn foo () {}")
        );
        assert_ne!(fingerprint("R", "x"), fingerprint("S", "x"));
        assert_ne!(fingerprint("R", "x"), fingerprint("R", "y"));
    }
}
