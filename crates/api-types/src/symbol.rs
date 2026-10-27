//! What a "symbol" is for the code viewer's references search (`GET /api/onboard/symbol-refs`).
//!
//! One rule, shared by both ends of the wire: the UI uses it to decide what it may pre-fill and
//! send, the server uses it to refuse anything else before a process is started. A symbol is
//! data for a fixed-string, whole-word search; it is never a pattern and never an option, so the
//! accepted alphabet has no space, quote, bracket, glob or regex operator in it.

/// Longest symbol accepted, in bytes.
pub const MAX_SYMBOL_BYTES: usize = 128;

/// Whether `symbol` may be searched for: 1 to [`MAX_SYMBOL_BYTES`] bytes of letters, digits and
/// `_ $ - . :`, with at least one letter, digit or `_` (so `---` and `...` are refused).
pub fn is_valid_symbol(symbol: &str) -> bool {
    !symbol.is_empty()
        && symbol.len() <= MAX_SYMBOL_BYTES
        && symbol
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '$' | '-' | '.' | ':'))
        && symbol.chars().any(|c| c.is_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbols_are_identifier_like_or_refused() {
        for good in [
            "foo",
            "_x",
            "$el",
            "a.b",
            "crate::db",
            "kebab-case",
            "-rf",
            "--help",
            "Größe",
            "x1",
        ] {
            assert!(is_valid_symbol(good), "{good}");
        }
        for bad in [
            "",
            " ",
            "a b",
            "a\tb",
            "a\nb",
            "foo(",
            ".*",
            "[a-z]+",
            "a|b",
            "a;rm",
            "`id`",
            "$(id)",
            "a'b",
            "a\"b",
            "a\\b",
            "a/b",
            "a=b",
            "a\0b",
            "---",
            "...",
            ":",
            "$",
            &"a".repeat(MAX_SYMBOL_BYTES + 1),
        ] {
            assert!(!is_valid_symbol(bad), "{bad:?}");
        }
        assert!(is_valid_symbol(&"a".repeat(MAX_SYMBOL_BYTES)));
    }
}
