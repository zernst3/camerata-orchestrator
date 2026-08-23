//! `PythonTestFileNamingChecker`: pure path/naming logic (Pass 4a "Group A" — no AST, no new
//! dependencies). See `docs/design/2026-07-27_ast-extractor-layer.md` §4 Group A and the
//! corpus rule `crates/rules/principles/python/testing/python-testing-file-naming-1.toml`.
//!
//! # What this checks
//!
//! pytest's default discovery glob is `test_*.py` (the `*_test.py` suffix form is an
//! accepted alternate convention). A file that LOOKS like it was meant to be collected — its
//! name starts with `test` — but does not match either accepted form is SILENTLY SKIPPED by
//! pytest rather than failing the build (the rule's own `qualifies` text). This checker finds
//! exactly that stray-file class: it does not require every file under a test directory to be
//! a test file (helper/fixture modules are legitimate), only that a file whose name signals
//! test intent actually uses a name pytest will collect.

use crate::arch_checker::{ArchChecker, ArchViolation, RepoView, SEVERITY_MEDIUM};

pub const RULE_PYTHON_TEST_FILE_NAMING: &str = "PYTHON-TESTING-FILE-NAMING-1";

const RULE_IDS: &[&str] = &[RULE_PYTHON_TEST_FILE_NAMING];

/// `**` (Pass 4a seam amendment, `arch_checker::glob_match`) so both a top-level `tests/`
/// dir and an arbitrarily nested one match the same two globs.
const INTEREST_GLOBS: &[&str] = &["**/tests/**/*.py", "**/test/**/*.py"];

/// Files that legitimately live under a test directory but are pytest special files, not
/// test files themselves — never flagged even though their name might otherwise trip the
/// stray-prefix heuristic below (`conftest.py` ends in "...ftest.py").
const EXEMPT_FILENAMES: &[&str] = &["__init__.py", "conftest.py"];

pub struct PythonTestFileNamingChecker;

impl ArchChecker for PythonTestFileNamingChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        RULE_IDS
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        repo.files
            .iter()
            .filter(|(path, _)| crate::arch_checker::matches_any_glob(INTEREST_GLOBS, path))
            .filter_map(|(path, _)| {
                let filename = path.rsplit('/').next().unwrap_or(path);
                stray_test_file_violation(path, filename)
            })
            .collect()
    }
}

/// Whether `filename` (the last path segment of `path`, already known to sit under a test
/// directory and end in `.py`) is a stray misnamed test file pytest would silently skip —
/// and if so, the [`ArchViolation`] for it. `None` for a compliant name, an exempt special
/// file, or a filename that doesn't signal test intent at all (an ordinary helper module).
fn stray_test_file_violation(path: &str, filename: &str) -> Option<ArchViolation> {
    if EXEMPT_FILENAMES.contains(&filename) {
        return None;
    }
    // Compliant forms: `test_*.py` (pytest default) and `*_test.py` (accepted alternate).
    if filename.starts_with("test_") || filename.ends_with("_test.py") {
        return None;
    }
    // Stray form: the name signals test intent (starts with "test", case-insensitively) but
    // matches neither compliant pattern above — e.g. `testfoo.py`, `TestUtils.py`. This is
    // deliberately a PREFIX check, not a substring/suffix check: a suffix check (`ends_with
    // "test.py"`) would false-positive on ordinary words like `latest.py`, which is exactly
    // the ambiguity the design memo calls out as out of scope for this lexical-grade pass.
    if filename.to_ascii_lowercase().starts_with("test") {
        return Some(ArchViolation {
            rule_id: RULE_PYTHON_TEST_FILE_NAMING.to_string(),
            file: path.to_string(),
            line: 0, // file-level finding — no single line is meaningful
            object: Some(filename.to_string()),
            severity: SEVERITY_MEDIUM,
            message: format!(
                "`{filename}` sits under a test directory and its name signals test intent, but it matches \
                 neither of pytest's discovery patterns (`test_*.py`, `*_test.py`) — pytest's default \
                 `python_files = test_*.py` SILENTLY SKIPS this file rather than failing the build, so any \
                 tests inside it never run. Rename to `test_{}` (or add the `_test.py` suffix) so pytest \
                 collects it (PYTHON-TESTING-FILE-NAMING-1).",
                strip_leading_test(filename)
            ),
        });
    }
    None
}

/// Strip a leading `test`/`Test`/`TEST` (case-insensitive, whatever length matched) plus any
/// immediately-following separator, so the suggested rename reads naturally: `testfoo.py` ->
/// `foo.py`, `Test_Utils.py` -> `Utils.py`. Falls back to the original name if stripping
/// would leave nothing (defensive; `stray_test_file_violation` never calls this on a name
/// that is JUST "test.py" without more, but guard it anyway rather than panic on a slice).
fn strip_leading_test(filename: &str) -> &str {
    let lower = filename.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("test") {
        let stripped_len = filename.len() - rest.len();
        let after = &filename[stripped_len..];
        let after = after.trim_start_matches(['_', '-']);
        if !after.is_empty() {
            return after;
        }
    }
    filename
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view<'a>(files: &'a [(String, String)]) -> RepoView<'a> {
        RepoView { spec: "test/repo", files }
    }

    fn files(pairs: Vec<(&str, &str)>) -> Vec<(String, String)> {
        pairs.into_iter().map(|(p, c)| (p.to_string(), c.to_string())).collect()
    }

    #[test]
    fn flags_stray_test_prefixed_file_pytest_would_skip() {
        let f = files(vec![("tests/testfoo.py", "def check(): pass\n")]);
        let vs = PythonTestFileNamingChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].rule_id, RULE_PYTHON_TEST_FILE_NAMING);
        assert_eq!(vs[0].file, "tests/testfoo.py");
        assert_eq!(vs[0].severity, SEVERITY_MEDIUM);
        assert!(vs[0].message.contains("test_foo.py"), "{}", vs[0].message);
    }

    #[test]
    fn compliant_test_prefix_file_is_clean() {
        let f = files(vec![("tests/test_foo.py", "def test_it(): pass\n")]);
        assert!(PythonTestFileNamingChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn compliant_test_suffix_file_is_clean() {
        let f = files(vec![("tests/foo_test.py", "def test_it(): pass\n")]);
        assert!(PythonTestFileNamingChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn ordinary_helper_module_in_tests_dir_is_not_flagged() {
        // Not every file under tests/ is a test file — a fixtures/helpers module is legit
        // and does not signal test intent by name, so it must not be flagged.
        let f = files(vec![("tests/fixtures.py", "DATA = {}\n")]);
        assert!(PythonTestFileNamingChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn conftest_and_init_are_exempt_even_though_conftest_ends_in_test_py() {
        let f = files(vec![("tests/conftest.py", "import pytest\n"), ("tests/__init__.py", "")]);
        assert!(PythonTestFileNamingChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn suffix_like_ordinary_word_is_not_flagged() {
        // "latest.py" ends with "test.py" but does NOT start with "test" — must not fire
        // (proves this is a prefix check, not a suffix substring check).
        let f = files(vec![("tests/latest.py", "VERSION = 1\n")]);
        assert!(PythonTestFileNamingChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn nested_test_directory_via_double_star_glob_is_scoped_in() {
        let f = files(vec![("app/pkg/tests/sub/TestWidget.py", "pass\n")]);
        let vs = PythonTestFileNamingChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].file, "app/pkg/tests/sub/TestWidget.py");
    }

    #[test]
    fn file_outside_any_test_directory_is_never_scoped_in() {
        let f = files(vec![("app/testmodule.py", "pass\n")]);
        assert!(PythonTestFileNamingChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn non_python_file_under_tests_dir_is_ignored() {
        let f = files(vec![("tests/fixtures.json", "{}")]);
        assert!(PythonTestFileNamingChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn empty_file_does_not_panic_and_is_judged_purely_by_name() {
        let f = files(vec![("tests/teststub.py", "")]);
        let vs = PythonTestFileNamingChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    #[test]
    fn no_test_directory_files_never_applies() {
        let f = files(vec![("README.md", "hello"), ("app/main.py", "pass\n")]);
        assert!(!crate::arch_checker::checker_applies(&PythonTestFileNamingChecker, &f));
    }

    #[test]
    fn strip_leading_test_handles_bare_test_dot_py_without_panic() {
        // Defensive: a filename of exactly "test.py" — starts_with("test") is true, neither
        // compliant form matches ("test_" prefix requires the underscore; "_test.py" suffix
        // needs more before it), so it IS flagged; the rename suggestion must not panic.
        let f = files(vec![("tests/test.py", "pass\n")]);
        let vs = PythonTestFileNamingChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert!(!vs[0].message.is_empty());
    }
}
