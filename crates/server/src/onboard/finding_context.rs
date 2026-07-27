//! On-demand enclosing-code-block lookup for ONE finding — backs
//! `GET /api/onboard/finding-context` (`docs/design/2026-07-27_finding-code-context.md`).
//!
//! Deliberately NOT scan-time cached: a finding modal opens rarely relative to scan volume
//! (a handful of reads per triage session vs. thousands of findings per scan), a per-finding
//! parse of one file is milliseconds, and computing on demand means the context always
//! reflects the file as it exists NOW rather than a stale scan-time snapshot that would need
//! its own invalidation story. See the design doc §3 for the full tradeoff.
//!
//! This module owns the pure/async lookup (path containment, the SINGLE-file read, response
//! assembly) so it is unit-testable without spinning up the axum router; `crate::lib`'s
//! handler is a thin adapter that resolves the repo dir from `AppState` and calls in here.
//!
//! # The floor: every failure degrades, nothing panics, nothing regresses
//!
//! The stored `FindingView::snippet` is the universal floor the UI already renders today.
//! Every failure mode this module can report — file missing, non-UTF8, oversized, the
//! violation line no longer existing, or a path-traversal refusal — maps to a `status` other
//! than `"ok"` plus a one-line human `reason`; the UI's job is to fall back to the stored
//! snippet plus that reason. This module can only ADD context on top of what the modal shows
//! today, never take away from it.

use std::path::{Path, PathBuf};

use camerata_checks::extract::enclosing::{enclosing_block, slice_lines};

use super::files::MAX_FILE_BYTES;

/// The result of one finding-context lookup. Serializes straight to the endpoint's JSON body
/// via [`to_json`](FindingContextOutcome::to_json) — see the design doc §3 for the wire shape.
#[derive(Debug, Clone, PartialEq)]
pub enum FindingContextOutcome {
    /// The enclosing block was resolved and read successfully.
    Ok {
        lines: Vec<String>,
        start_line: usize,
        end_line: usize,
        violation_line: usize,
        kind: &'static str,
        language: String,
        /// Whether the violation line's CURRENT content still contains (or is contained by)
        /// the caller-supplied `expect` snippet text — `true` when no `expect` was supplied
        /// (nothing to compare, so nothing is flagged stale).
        matches_snippet: bool,
    },
    /// The file doesn't exist at the resolved path (moved/deleted since the scan, or the repo
    /// itself isn't resolved locally — see `resolve_repo_dir`).
    FileMissing,
    /// The file exists but isn't valid UTF-8 (a binary file, or genuinely corrupt encoding).
    NotUtf8,
    /// The file exceeds [`MAX_FILE_BYTES`] — the same cap the onboarding scan itself enforces.
    TooLarge,
    /// The file is readable, but `line` is past its current end (the file shrank).
    LineGone,
    /// The requested `path` resolves outside the repo directory — refused before any read.
    PathTraversal,
}

impl FindingContextOutcome {
    /// The JSON body the endpoint returns. `"ok"` carries the full block; every other status
    /// carries a one-line human `reason` the UI shows alongside the stored snippet fallback.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            FindingContextOutcome::Ok {
                lines,
                start_line,
                end_line,
                violation_line,
                kind,
                language,
                matches_snippet,
            } => serde_json::json!({
                "status": "ok",
                "lines": lines,
                "start_line": start_line,
                "end_line": end_line,
                "violation_line": violation_line,
                "kind": kind,
                "language": language,
                "matches_snippet": matches_snippet,
            }),
            FindingContextOutcome::FileMissing => serde_json::json!({
                "status": "file_missing",
                "reason": "file not found — changed since scan?",
            }),
            FindingContextOutcome::NotUtf8 => serde_json::json!({
                "status": "not_utf8",
                "reason": "file is not valid UTF-8 (binary?)",
            }),
            FindingContextOutcome::TooLarge => serde_json::json!({
                "status": "too_large",
                "reason": format!("file exceeds the {} KB cap", MAX_FILE_BYTES / 1000),
            }),
            FindingContextOutcome::LineGone => serde_json::json!({
                "status": "line_gone",
                "reason": "file changed since scan",
            }),
            FindingContextOutcome::PathTraversal => serde_json::json!({
                "status": "path_traversal",
                "reason": "requested path escapes the repo directory",
            }),
        }
    }
}

/// Resolve + read the SINGLE file at `repo_dir` joined with `rel_path`, compute its enclosing
/// block around `line` (1-based), and compare the violation line's current content against
/// `expect` (trimmed containment either direction) for a staleness signal. Never panics; every
/// failure mode maps to a non-`Ok` [`FindingContextOutcome`] variant (see module docs).
///
/// This is the ONLY side-effecting piece here (reads exactly one file — never the whole-tree
/// `read_local_repo_files` walker). Path containment ([`resolve_contained_path`]) runs BEFORE
/// any filesystem read.
pub async fn lookup(
    repo_dir: &Path,
    rel_path: &str,
    line: usize,
    expect: Option<&str>,
) -> FindingContextOutcome {
    let Some(abs_path) = resolve_contained_path(repo_dir, rel_path) else {
        return FindingContextOutcome::PathTraversal;
    };

    let bytes = match tokio::fs::read(&abs_path).await {
        Ok(b) => b,
        Err(_) => return FindingContextOutcome::FileMissing,
    };
    if bytes.len() > MAX_FILE_BYTES {
        return FindingContextOutcome::TooLarge;
    }
    let source = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => return FindingContextOutcome::NotUtf8,
    };

    let Some(block) = enclosing_block(rel_path, &source, line) else {
        return FindingContextOutcome::LineGone;
    };

    let lines = slice_lines(&source, block.start_line, block.end_line);
    let matches_snippet = match expect.map(str::trim).filter(|e| !e.is_empty()) {
        None => true, // nothing to compare against — don't flag stale
        Some(exp) => source
            .lines()
            .nth(line.saturating_sub(1))
            .map(|actual| {
                let actual = actual.trim();
                actual.contains(exp) || exp.contains(actual)
            })
            .unwrap_or(false),
    };

    FindingContextOutcome::Ok {
        lines,
        start_line: block.start_line,
        end_line: block.end_line,
        violation_line: line,
        kind: block.kind.label(),
        language: language_label(rel_path),
        matches_snippet,
    }
}

/// `"sql"` for a `.sql` path, else the [`camerata_checks::extract::SourceLang`] label, else
/// `"text"` for anything the extractor layer doesn't cover — purely cosmetic (the UI's
/// "language" tag), never affects which block is resolved.
fn language_label(rel_path: &str) -> String {
    if rel_path.rsplit('.').next().map(|e| e.eq_ignore_ascii_case("sql")).unwrap_or(false) {
        return "sql".to_string();
    }
    camerata_checks::extract::lang_for_path(rel_path)
        .map(|l| l.label().to_string())
        .unwrap_or_else(|| "text".to_string())
}

/// Resolve `rel_path` against `repo_dir` and verify the result is still CONTAINED within it —
/// refuses a `../../etc/passwd`-style traversal, an absolute path pointing elsewhere, or any
/// symlink that would smuggle the read outside the repo. Mirrors the containment jail
/// `api_agent_driver::assert_in_worktree` uses for writes: canonicalize the deepest EXISTING
/// ancestor of both the target and the root (resolving any symlinks in it), then compare with
/// `starts_with`. Using the existing-ancestor form (rather than requiring the exact target to
/// exist) means containment can still be checked for a file that's already been deleted.
fn resolve_contained_path(repo_dir: &Path, rel_path: &str) -> Option<PathBuf> {
    if rel_path.trim().is_empty() {
        return None;
    }
    let candidate = repo_dir.join(rel_path);
    let root = canonicalize_existing_prefix(&normalize_lexical(repo_dir));
    let target = canonicalize_existing_prefix(&normalize_lexical(&candidate));
    if target.starts_with(&root) {
        Some(target)
    } else {
        None
    }
}

/// Lexically collapse `.`/`..` components WITHOUT touching the filesystem (no symlink
/// resolution) — the first pass before [`canonicalize_existing_prefix`] resolves symlinks in
/// whatever prefix actually exists on disk.
fn normalize_lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Canonicalize the deepest EXISTING ancestor of `p` (resolving any symlinks in it), then
/// rejoin the remaining not-yet-existing tail components as-is. Lets containment be verified
/// even for a path that doesn't exist (a deleted file, or a traversal attempt into a path that
/// was never real) — [`std::fs::canonicalize`] alone would just error in that case.
fn canonicalize_existing_prefix(p: &Path) -> PathBuf {
    let mut ancestor = p.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(canon) = std::fs::canonicalize(&ancestor) {
            let mut resolved = canon;
            for seg in tail.iter().rev() {
                resolved.push(seg);
            }
            return resolved;
        }
        match ancestor.file_name() {
            Some(name) => {
                tail.push(name.to_os_string());
                if !ancestor.pop() {
                    return normalize_lexical(p);
                }
            }
            None => return normalize_lexical(p),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, content: &str) -> PathBuf {
        let full = dir.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full, content).unwrap();
        full
    }

    #[tokio::test]
    async fn ok_for_a_real_fixture_finding() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "src/lib.rs",
            "fn f() -> i32 {\n    let bad = 1;\n    bad\n}\n",
        );
        let outcome = lookup(tmp.path(), "src/lib.rs", 2, None).await;
        match outcome {
            FindingContextOutcome::Ok {
                start_line,
                end_line,
                kind,
                language,
                matches_snippet,
                ..
            } => {
                assert_eq!((start_line, end_line), (1, 4));
                assert_eq!(kind, "function");
                assert_eq!(language, "rust");
                assert!(matches_snippet, "no `expect` supplied — must not flag stale");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn matches_snippet_false_when_expect_diverges() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "src/lib.rs", "fn f() {\n    let x = 1;\n}\n");
        let outcome = lookup(tmp.path(), "src/lib.rs", 2, Some("let x = 999;")).await;
        match outcome {
            FindingContextOutcome::Ok { matches_snippet, .. } => {
                assert!(!matches_snippet, "expect text doesn't match — must flag stale")
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn matches_snippet_true_when_expect_matches() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "src/lib.rs", "fn f() {\n    let x = 1;\n}\n");
        let outcome = lookup(tmp.path(), "src/lib.rs", 2, Some("let x = 1;")).await;
        match outcome {
            FindingContextOutcome::Ok { matches_snippet, .. } => assert!(matches_snippet),
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn path_traversal_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "src/lib.rs", "fn f() {}\n");
        for traversal in ["../../../../etc/passwd", "../outside.txt", "/etc/passwd"] {
            let outcome = lookup(tmp.path(), traversal, 1, None).await;
            assert_eq!(
                outcome,
                FindingContextOutcome::PathTraversal,
                "must refuse `{traversal}`"
            );
        }
    }

    #[tokio::test]
    async fn path_traversal_is_refused_even_when_the_target_does_not_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let outcome = lookup(tmp.path(), "../../nowhere/does/not/exist.rs", 1, None).await;
        assert_eq!(outcome, FindingContextOutcome::PathTraversal);
    }

    #[tokio::test]
    async fn missing_file_reports_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let outcome = lookup(tmp.path(), "src/does_not_exist.rs", 1, None).await;
        assert_eq!(outcome, FindingContextOutcome::FileMissing);
    }

    #[tokio::test]
    async fn oversized_file_reports_too_large() {
        let tmp = tempfile::tempdir().unwrap();
        let big = "x".repeat(MAX_FILE_BYTES + 1);
        write(tmp.path(), "src/huge.rs", &big);
        let outcome = lookup(tmp.path(), "src/huge.rs", 1, None).await;
        assert_eq!(outcome, FindingContextOutcome::TooLarge);
    }

    #[tokio::test]
    async fn non_utf8_file_reports_not_utf8() {
        let tmp = tempfile::tempdir().unwrap();
        let full = tmp.path().join("src/binary.rs");
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, [0xFFu8, 0xFE, 0x00, 0x01, 0xC0, 0xC1]).unwrap();
        let outcome = lookup(tmp.path(), "src/binary.rs", 1, None).await;
        assert_eq!(outcome, FindingContextOutcome::NotUtf8);
    }

    #[tokio::test]
    async fn line_past_eof_reports_line_gone() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "src/lib.rs", "fn f() {}\n");
        let outcome = lookup(tmp.path(), "src/lib.rs", 500, None).await;
        assert_eq!(outcome, FindingContextOutcome::LineGone);
    }

    #[tokio::test]
    async fn sql_language_label_and_raw_lines() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "supabase/policy.sql",
            "create table a (id int);\ncreate policy p on a\n  using (true);\n",
        );
        let outcome = lookup(tmp.path(), "supabase/policy.sql", 2, None).await;
        match outcome {
            FindingContextOutcome::Ok { language, kind, .. } => {
                assert_eq!(language, "sql");
                assert_eq!(kind, "sql_statement");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unresolvable_repo_dir_is_the_callers_responsibility_not_this_fns() {
        // `lookup` always receives an already-resolved repo_dir; a repo that isn't linked
        // locally at all is handled one layer up (the endpoint maps `resolve_repo_dir() ==
        // None` straight to `FileMissing` without calling `lookup`). Documented here so the
        // division of responsibility is explicit and doesn't silently drift.
        let tmp = tempfile::tempdir().unwrap();
        let never_created = tmp.path().join("not-a-real-checkout");
        let outcome = lookup(&never_created, "src/lib.rs", 1, None).await;
        assert_eq!(outcome, FindingContextOutcome::FileMissing);
    }
}
