//! The vocabulary shared by the three ecosystems: which call names assert, and which module
//! paths are test-support places whose helpers the checker cannot see into.

/// Lowercase substrings of a called name that mark it as checking something (an assertion,
/// an assertion helper, a matcher, a snapshot, an expected failure). Deliberately broad: a name
/// matched by mistake only keeps a test from being reported.
const ASSERTING_WORDS: &[&str] = &[
    "assert", "expect", "verify", "check", "should", "ensure", "validate", "match", "snapshot",
    "fail", "raise", "throw", "warns",
];

/// Whole names that check something without an assertion word in them: nose's `eq_` and `ok_`,
/// pytest-assume's `assume`.
const ASSERTING_NAMES: &[&str] = &["eq_", "ok_", "assume"];

/// Lowercase substrings of a module path that mark it as a place for test helpers.
const SUPPORT_WORDS: &[&str] = &[
    "test", "spec", "helper", "support", "fixture", "util", "common", "harness", "mock", "setup",
    "conftest", "assert", "check",
];

/// Whether a called name (the last segment of a path or member chain) reads as a check.
pub(super) fn names_an_assertion(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ASSERTING_NAMES.contains(&lower.as_str()) || ASSERTING_WORDS.iter().any(|w| lower.contains(w))
}

/// Whether a module path or specifier names a test-support place.
pub(super) fn is_support_module(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    SUPPORT_WORDS.iter().any(|w| lower.contains(w))
}

/// How deep a same-file helper is followed when looking for an assertion inside it.
pub(super) const HELPER_DEPTH: usize = 3;

/// The shared sentence of every finding, after the test is named.
pub(super) fn message(subject: &str, empty: bool) -> String {
    let what = if empty {
        "has an empty body"
    } else {
        "has no assertion: nothing in its body checks a result (no assertion call, no assertion \
         helper, no expected error)"
    };
    format!(
        "{subject} {what}. It passes whenever the code it runs does not fail, whatever that code \
         returns, so it reports green for broken behavior and counts toward coverage it does not \
         provide. Add an assertion on the behavior the test is named for, or remove the test."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assertion_names_are_matched_case_insensitively() {
        for name in [
            "expect",
            "assertEqual",
            "assert_called_once_with",
            "toMatchSnapshot",
            "verifyAll",
            "checkResult",
            "shouldRender",
            "pytest_raises",
            "toThrow",
            "eq_",
            "ok_",
            "assume",
        ] {
            assert!(names_an_assertion(name), "{name}");
        }
        for name in [
            "render",
            "click",
            "fetch",
            "new",
            "build",
            "book_order",
            "assumed",
        ] {
            assert!(!names_an_assertion(name), "{name}");
        }
    }

    #[test]
    fn support_modules_are_recognised() {
        for path in [
            "../test-utils",
            "./helpers",
            "tests.common",
            "crate::support",
            "@/testing",
        ] {
            assert!(is_support_module(path), "{path}");
        }
        assert!(!is_support_module("../src/orders"));
    }
}
