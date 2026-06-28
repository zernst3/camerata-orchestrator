//! Pure unified-diff inspection for the **test-tamper guard** (AGENTIC-NO-TEST-TAMPER-1).
//!
//! An agent can make a failing suite go green the cheap way: edit the test that
//! caught its broken code rather than fix the code. This module detects that
//! pattern by reading the agent's unified git diff — with **no I/O** — and
//! flagging any EXISTING test that was modified or deleted.
//!
//! The contract (see `crates/rules/principles/agentic/agentic-no-test-tamper-1.toml`):
//!   - Adding a brand-new test file (`new file mode`, all `+` lines) → allowed.
//!   - Appending a new `#[test] fn` to an existing test file (pure `+` hunk) → allowed.
//!   - Modifying an existing test's body (any `-`/changed line) → **Modified** (escalate).
//!   - Deleting a test file (`deleted file mode`, all `-` lines) → **Deleted** (escalate).
//!   - Any change to a NON-test file → ignored entirely.
//!
//! "Test file" is decided by [`camerata_gateway::is_test_or_fixture_path`] — the
//! same predicate the gateway uses everywhere, so the guard agrees with the rest
//! of the system about what counts as a test or fixture.
//!
//! This is a pure function so it is fully unit-testable from a string of diff text;
//! the dev-run wiring in `dev_implement_run.rs` feeds it the worktree diff.

/// How an existing test file was tampered with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TamperKind {
    /// An existing line was removed or changed in a test/fixture file.
    Modified,
    /// A test/fixture file was deleted outright.
    Deleted,
}

/// One tampering finding: the test/fixture file and how it was tampered with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestTamperFinding {
    /// The path of the test/fixture file (as it appears in the diff).
    pub file: String,
    pub kind: TamperKind,
}

/// Per-file accumulator while walking the diff.
struct FileState {
    /// Path of the file under the current `diff --git` header.
    path: String,
    /// True when this file's header declared `deleted file mode`.
    deleted: bool,
    /// True when any content line in a hunk was a removal/change of an existing
    /// line (a `-` line that is not the file header and not pure whitespace).
    has_removal: bool,
}

impl FileState {
    fn new(path: String) -> Self {
        Self {
            path,
            deleted: false,
            has_removal: false,
        }
    }
}

/// Inspect a unified git diff and return a finding for each EXISTING test/fixture
/// file that was modified or deleted. Pure: no filesystem or process access.
///
/// Non-test files are ignored. Pure-addition diffs (new test files, appended
/// test functions) produce no findings — adding tests is always allowed.
pub fn detect_test_tampering(diff: &str) -> Vec<TestTamperFinding> {
    let mut findings = Vec::new();
    let mut current: Option<FileState> = None;

    // Flush the current file's accumulated state into a finding (if it tampered).
    let flush = |state: Option<FileState>, findings: &mut Vec<TestTamperFinding>| {
        let Some(state) = state else { return };
        // Only test/fixture files are in scope.
        if !camerata_gateway::is_test_or_fixture_path(&state.path) {
            return;
        }
        if state.deleted {
            findings.push(TestTamperFinding {
                file: state.path,
                kind: TamperKind::Deleted,
            });
        } else if state.has_removal {
            findings.push(TestTamperFinding {
                file: state.path,
                kind: TamperKind::Modified,
            });
        }
    };

    for line in diff.lines() {
        // A new file header starts a new per-file accumulator.
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // Close out the previous file before starting the next.
            flush(current.take(), &mut findings);
            current = Some(FileState::new(parse_diff_git_path(rest)));
            continue;
        }

        let Some(state) = current.as_mut() else {
            // Lines before the first `diff --git` (e.g. a covering message) — ignore.
            continue;
        };

        // `deleted file mode 100644` → the whole file was removed.
        if line.starts_with("deleted file mode") {
            state.deleted = true;
            continue;
        }

        // File header lines (`--- a/x`, `+++ b/x`) are NOT content changes.
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            continue;
        }

        // A `-` content line that is not pure whitespace is a removal/change of an
        // existing line. (`+` lines are pure additions and never flagged here.)
        if let Some(removed) = line.strip_prefix('-') {
            if !removed.trim().is_empty() {
                state.has_removal = true;
            }
        }
    }

    // Flush the final file.
    flush(current.take(), &mut findings);

    findings
}

/// Extract the file path from a `diff --git a/<path> b/<path>` header tail
/// (the part after `diff --git `). Prefers the `b/` (post-image) path; falls
/// back to the `a/` path for deletions. Returns the raw tail if it can't parse.
fn parse_diff_git_path(rest: &str) -> String {
    // The tail looks like `a/path/to/file b/path/to/file`. Paths can contain
    // spaces, but the common (and our) case does not; split on the ` b/` marker.
    if let Some(idx) = rest.find(" b/") {
        let b_path = &rest[idx + 3..];
        if !b_path.is_empty() {
            return b_path.trim().to_string();
        }
    }
    // Fall back to the a/ path (deletions have a meaningful a/ path).
    let a = rest.strip_prefix("a/").unwrap_or(rest);
    // Cut at the first " b/" if present, else first whitespace.
    let a = a.split(" b/").next().unwrap_or(a);
    a.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (a) Adding a brand-new test file → no finding (all `+`, `new file mode`).
    #[test]
    fn new_test_file_is_not_flagged() {
        let diff = "\
diff --git a/crates/foo/tests/new_test.rs b/crates/foo/tests/new_test.rs
new file mode 100644
index 0000000..abc1234
--- /dev/null
+++ b/crates/foo/tests/new_test.rs
@@ -0,0 +1,3 @@
+#[test]
+fn brand_new() {
+    assert!(true);
+}
";
        assert_eq!(detect_test_tampering(diff), vec![]);
    }

    /// (b) Appending a new `#[test] fn` to an existing test file (only `+` lines in
    /// the hunk) → no finding. Adding tests is always allowed.
    #[test]
    fn appending_test_fn_pure_addition_is_not_flagged() {
        let diff = "\
diff --git a/crates/foo/tests/suite.rs b/crates/foo/tests/suite.rs
index 1111111..2222222 100644
--- a/crates/foo/tests/suite.rs
+++ b/crates/foo/tests/suite.rs
@@ -10,3 +10,9 @@ fn existing() {
     assert!(true);
 }
+
+#[test]
+fn newly_appended() {
+    assert_eq!(2 + 2, 4);
+}
";
        assert_eq!(detect_test_tampering(diff), vec![]);
    }

    /// (c) Modifying an existing test's body (a `-` line) → Modified.
    #[test]
    fn modifying_existing_test_body_is_modified() {
        let diff = "\
diff --git a/crates/foo/tests/suite.rs b/crates/foo/tests/suite.rs
index 1111111..2222222 100644
--- a/crates/foo/tests/suite.rs
+++ b/crates/foo/tests/suite.rs
@@ -1,4 +1,4 @@
 #[test]
 fn existing() {
-    assert_eq!(compute(), 4);
+    assert_eq!(compute(), 5);
 }
";
        let findings = detect_test_tampering(diff);
        assert_eq!(
            findings,
            vec![TestTamperFinding {
                file: "crates/foo/tests/suite.rs".to_string(),
                kind: TamperKind::Modified,
            }]
        );
    }

    /// (d) Deleting a test file → Deleted.
    #[test]
    fn deleting_test_file_is_deleted() {
        let diff = "\
diff --git a/crates/foo/tests/suite.rs b/crates/foo/tests/suite.rs
deleted file mode 100644
index 1111111..0000000
--- a/crates/foo/tests/suite.rs
+++ /dev/null
@@ -1,4 +0,0 @@
-#[test]
-fn existing() {
-    assert_eq!(compute(), 4);
-}
";
        let findings = detect_test_tampering(diff);
        assert_eq!(
            findings,
            vec![TestTamperFinding {
                file: "crates/foo/tests/suite.rs".to_string(),
                kind: TamperKind::Deleted,
            }]
        );
    }

    /// (e) Changes to a NON-test file (even with `-` lines) → ignored.
    #[test]
    fn non_test_file_changes_are_ignored() {
        let diff = "\
diff --git a/crates/foo/src/lib.rs b/crates/foo/src/lib.rs
index 1111111..2222222 100644
--- a/crates/foo/src/lib.rs
+++ b/crates/foo/src/lib.rs
@@ -1,4 +1,4 @@
 pub fn compute() -> i32 {
-    4
+    5
 }
";
        assert_eq!(detect_test_tampering(diff), vec![]);
    }

    /// (f) Mixed: one test file modified + one non-test file changed → exactly one
    /// finding (the test file).
    #[test]
    fn mixed_test_and_non_test_yields_one_finding() {
        let diff = "\
diff --git a/crates/foo/src/lib.rs b/crates/foo/src/lib.rs
index 1111111..2222222 100644
--- a/crates/foo/src/lib.rs
+++ b/crates/foo/src/lib.rs
@@ -1,4 +1,4 @@
 pub fn compute() -> i32 {
-    4
+    5
 }
diff --git a/crates/foo/tests/suite.rs b/crates/foo/tests/suite.rs
index 3333333..4444444 100644
--- a/crates/foo/tests/suite.rs
+++ b/crates/foo/tests/suite.rs
@@ -1,4 +1,4 @@
 #[test]
 fn existing() {
-    assert_eq!(compute(), 4);
+    assert_eq!(compute(), 5);
 }
";
        let findings = detect_test_tampering(diff);
        assert_eq!(
            findings,
            vec![TestTamperFinding {
                file: "crates/foo/tests/suite.rs".to_string(),
                kind: TamperKind::Modified,
            }]
        );
    }
}
