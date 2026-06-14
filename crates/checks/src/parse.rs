//! Output-to-RuleId mapping layer.
//!
//! Pure functions (RUST-PURE-STATE-TRANSITIONS-1): no I/O, no subprocess calls.
//! All functions take the raw text returned by the subprocess layer and return
//! the rule ids that were violated.
//!
//! Keeping this layer separate from the subprocess layer means the mapping
//! logic is unit-testable with static fixture strings.

use camerata_core::RuleId;

use crate::{
    clippy_rule, fmt_rule,
    subprocess::{ClippyOutput, FmtOutput, TestOutput},
    test_rule,
};

/// Map `cargo fmt --check` output to violated rule ids.
///
/// `cargo fmt --check` exits non-zero and prints the names of files that would
/// be reformatted when formatting is needed. If the exit code is non-zero we
/// emit `RUST-FMT`; otherwise we return an empty vec.
pub fn map_fmt_output(output: &FmtOutput) -> Vec<RuleId> {
    if output.success {
        return vec![];
    }
    vec![fmt_rule()]
}

/// Map `cargo clippy` output to violated rule ids.
///
/// clippy exits non-zero when `-D warnings` is in effect and any lint fires.
/// We also scan the combined text for the canonical `error[` and `warning[`
/// prefixes so a future caller that injects captured output (e.g. from CI
/// artefacts) gets correct results even if exit code is unavailable.
///
/// Rule: at least one lint diagnostic → `RUST-CLIPPY`.
pub fn map_clippy_output(output: &ClippyOutput) -> Vec<RuleId> {
    if !output.success {
        return vec![clippy_rule()];
    }
    // Secondary signal: scan the text even when exit code says success, because
    // some callers might inject pre-captured output with exit=0.
    if has_clippy_diagnostic(&output.combined) {
        return vec![clippy_rule()];
    }
    vec![]
}

/// Map `cargo test` output to violated rule ids.
///
/// `cargo test` exits non-zero when a test fails OR the crate does not compile.
/// Either is a layer-2 violation worth a bounce-back, so a non-zero exit maps to
/// `RUST-TEST`. As with clippy, we also scan the text so a caller that injects
/// pre-captured output (exit code unavailable) still gets a correct result.
///
/// Rule: a failing test OR a compile error → `RUST-TEST`.
pub fn map_test_output(output: &TestOutput) -> Vec<RuleId> {
    if !output.success {
        return vec![test_rule()];
    }
    if has_test_failure(&output.combined) {
        return vec![test_rule()];
    }
    vec![]
}

/// Returns true if the text indicates a failed test run or a compile failure.
///
/// Matches the canonical signals libtest / cargo emit:
/// - `test result: FAILED` — the libtest summary line on any failure
/// - a `failures:` block header libtest prints before listing failed tests
/// - `error[E…]` / `error: aborting` — the crate did not compile, so tests
///   could not even run (reuses the same rustc openers clippy keys off)
///
/// Conservative on purpose: it does not fire on the mere word "failed" in prose
/// (e.g. a test literally named `it_handles_failed_login`), only on the libtest
/// summary form `test result: FAILED`.
pub fn has_test_failure(text: &str) -> bool {
    text.lines().any(|line| {
        let t = line.trim_start();
        t.starts_with("test result: FAILED")
            || t == "failures:"
            || t.starts_with("error[")
            || t.starts_with("error: aborting")
            || t.contains("\"level\":\"error\"")
    })
}

/// Returns true if the text contains at least one clippy/rustc diagnostic line.
///
/// Matches the canonical prefixes emitted by rustc:
/// - `error[E…]`  — hard errors
/// - `warning[clippy::…]` — clippy lints
/// - `error: aborting` — summary line
///
/// This is intentionally conservative: only match lines that unambiguously
/// indicate a real diagnostic, not arbitrary mentions of the word "error".
pub fn has_clippy_diagnostic(text: &str) -> bool {
    text.lines().any(|line| {
        let t = line.trim_start();
        // rustc diagnostic openers
        (t.starts_with("error[") || t.starts_with("warning["))
            // rustc aborting summary
            || t.starts_with("error: aborting")
            // clippy json format: "level":"error" or "level":"warning"
            || (t.contains("\"level\":\"error\"") || t.contains("\"level\":\"warning\""))
    })
}

// ─── tests (ORCH-NEW-PATH-TESTS-1) ───────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subprocess::{ClippyOutput, FmtOutput, TestOutput};

    // ── fmt mapping ──────────────────────────────────────────────────────────

    #[test]
    fn fmt_clean_worktree_returns_no_violations() {
        let output = FmtOutput {
            combined: String::new(),
            success: true,
        };
        assert_eq!(map_fmt_output(&output), vec![]);
    }

    #[test]
    fn fmt_unformatted_file_returns_rust_fmt_rule() {
        // `cargo fmt --check` prints the file path to stderr and exits 1.
        let output = FmtOutput {
            combined: "src/main.rs\n".to_string(),
            success: false,
        };
        assert_eq!(map_fmt_output(&output), vec![crate::fmt_rule()]);
    }

    // ── clippy mapping ───────────────────────────────────────────────────────

    #[test]
    fn clippy_clean_output_returns_no_violations() {
        let output = ClippyOutput {
            combined: "    Checking my-crate v0.1.0\n    Finished dev [unoptimized + debuginfo] target(s) in 0.42s\n".to_string(),
            success: true,
        };
        assert_eq!(map_clippy_output(&output), vec![]);
    }

    #[test]
    fn clippy_nonzero_exit_returns_rust_clippy_rule() {
        // Non-zero exit is the primary signal.
        let output = ClippyOutput {
            combined: "error: aborting due to previous error\n".to_string(),
            success: false,
        };
        assert_eq!(map_clippy_output(&output), vec![crate::clippy_rule()]);
    }

    #[test]
    fn clippy_warning_diagnostic_in_text_returns_rule_even_when_exit_zero() {
        // Some CI pipelines capture output and replay it; handle exit-0 text.
        let output = ClippyOutput {
            combined: "warning[clippy::unwrap_used]: used `unwrap()` on a `Result`\n --> src/main.rs:10:5\n".to_string(),
            success: true,
        };
        assert_eq!(map_clippy_output(&output), vec![crate::clippy_rule()]);
    }

    #[test]
    fn clippy_error_diagnostic_in_text_returns_rule_even_when_exit_zero() {
        let output = ClippyOutput {
            combined: "error[E0308]: mismatched types\n --> src/lib.rs:5:14\n".to_string(),
            success: true,
        };
        assert_eq!(map_clippy_output(&output), vec![crate::clippy_rule()]);
    }

    // ── test mapping ───────────────────────────────────────────────────────────

    #[test]
    fn test_all_passing_returns_no_violations() {
        let output = TestOutput {
            combined: "test result: ok. 12 passed; 0 failed; 0 ignored\n".to_string(),
            success: true,
        };
        assert_eq!(map_test_output(&output), vec![]);
    }

    #[test]
    fn test_nonzero_exit_returns_rust_test_rule() {
        let output = TestOutput {
            combined: "test result: FAILED. 10 passed; 2 failed; 0 ignored\n".to_string(),
            success: false,
        };
        assert_eq!(map_test_output(&output), vec![crate::test_rule()]);
    }

    #[test]
    fn test_failure_in_text_returns_rule_even_when_exit_zero() {
        // Injected/replayed output where the exit code is unavailable.
        let output = TestOutput {
            combined: "running 3 tests\ntest result: FAILED. 2 passed; 1 failed; 0 ignored\n"
                .to_string(),
            success: true,
        };
        assert_eq!(map_test_output(&output), vec![crate::test_rule()]);
    }

    #[test]
    fn test_compile_failure_returns_rule() {
        // The crate did not compile, so tests could not run.
        let output = TestOutput {
            combined: "error[E0425]: cannot find value `x` in this scope\n".to_string(),
            success: false,
        };
        assert_eq!(map_test_output(&output), vec![crate::test_rule()]);
    }

    #[test]
    fn test_failure_detection_ignores_test_named_failed() {
        // A passing run whose test names contain "failed" must NOT trip the scan.
        assert!(!has_test_failure(
            "test login::it_rejects_failed_password ... ok\ntest result: ok. 1 passed; 0 failed\n"
        ));
    }

    #[test]
    fn test_failure_detection_recognises_summary_and_block() {
        assert!(has_test_failure(
            "test result: FAILED. 0 passed; 1 failed\n"
        ));
        assert!(has_test_failure("\nfailures:\n    tests::it_works\n"));
    }

    // ── has_clippy_diagnostic ─────────────────────────────────────────────────

    #[test]
    fn diagnostic_detection_recognises_error_bracket() {
        assert!(has_clippy_diagnostic("error[E0412]: cannot find type\n"));
    }

    #[test]
    fn diagnostic_detection_recognises_warning_bracket() {
        assert!(has_clippy_diagnostic(
            "warning[clippy::needless_return]: needless return\n"
        ));
    }

    #[test]
    fn diagnostic_detection_recognises_aborting_line() {
        assert!(has_clippy_diagnostic(
            "error: aborting due to 3 previous errors\n"
        ));
    }

    #[test]
    fn diagnostic_detection_ignores_plain_prose() {
        // A log line mentioning "error" without the bracketed form must not trigger.
        assert!(!has_clippy_diagnostic(
            "    Compiling my-crate v0.1.0\n    Finished in 1.2s\n"
        ));
    }

    #[test]
    fn diagnostic_detection_handles_leading_whitespace() {
        // rustc indents diagnostic lines with spaces.
        assert!(has_clippy_diagnostic(
            "    error[E0308]: mismatched types\n"
        ));
    }

    #[test]
    fn diagnostic_detection_handles_json_format_error() {
        // JSON output from `cargo clippy --message-format=json`
        let json_line = r#"{"reason":"compiler-message","message":{"level":"error","message":"unused variable"}}"#;
        assert!(has_clippy_diagnostic(json_line));
    }

    #[test]
    fn diagnostic_detection_handles_json_format_warning() {
        let json_line = r#"{"reason":"compiler-message","message":{"level":"warning","message":"unused import"}}"#;
        assert!(has_clippy_diagnostic(json_line));
    }
}
