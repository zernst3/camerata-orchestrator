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
    subprocess::{ClippyOutput, FmtOutput},
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
    use crate::subprocess::{ClippyOutput, FmtOutput};

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
