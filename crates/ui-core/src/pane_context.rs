//! The detail pane's code context (`WORK_ORDER_REVIEW_SPEED.md` B.3), as pure logic.
//!
//! The pane's Evidence block renders the stored snippet at once. When the cursor has rested on a
//! finding for [`SETTLE_MS`], a batch of one goes to `POST /api/onboard/finding-context/batch`
//! and the stored line plus [`CONTEXT_RADIUS`] lines either side appear beneath the snippet.
//! Everything that decides WHEN to ask, WHAT to ask, WHETHER an answer may be shown and WHERE it
//! is kept lives here, with unit tests and no rendering framework:
//!
//! - [`SettleGate`]: the debounce. Moving the cursor three rows within 200 ms yields one request.
//! - [`ContextTarget`]: the request for one finding, its cache key and its match key.
//! - [`accept`]: the answer's gate. Only an `ok` read of the inspected commit that still matches
//!   the stored snippet and echoes the finding it was asked about is shown. Every other status,
//!   a working-tree read, a mismatch, a malformed body: `None`, and the pane renders exactly
//!   what it rendered before this feature existed.
//! - [`context_from_file`]: the same window cut from a whole file the viewer already holds, so
//!   an open viewer and the pane share what has been read.
//!
//! The cache lane lives in [`ViewerCache`](crate::viewer::ViewerCache) beside the file lane, keyed
//! by [`ContextKey`] (repo, commit, path AND line), so a pane window can never be mistaken for a
//! file and a file can never lend the pane another finding's lines.

use crate::triage::{finding_key, FindingView};
use crate::viewer::SourceFileView;
use crate::viewer_find::segments_text;

/// How long the cursor must rest on a finding before its context is requested.
pub const SETTLE_MS: u64 = 150;

/// Lines of context either side of the stored line. The server cuts the same window: the radius
/// and the snippet comparison are `camerata_api_types::snippet`, shared by both ends of the wire.
pub use camerata_api_types::snippet::{line_matches_snippet, CONTEXT_RADIUS};

/// The most lines a context window may have: the stored line and the radius either side.
pub const MAX_CONTEXT_LINES: usize = 2 * CONTEXT_RADIUS + 1;

/// How many context windows the client keeps (a window is a few short lines, so this is cheap).
pub const CONTEXT_CAPACITY: usize = 64;

// ── The debounce ─────────────────────────────────────────────────────────────────────────

/// Decides when the cursor has settled. Pure and clock-free: the caller passes the time.
///
/// Call [`moved`](Self::moved) whenever the focused finding changes and
/// [`due`](Self::due) [`SETTLE_MS`] later (once per move is fine): a key is handed out once, and
/// only when it has been the focused one for the whole settle time, so a burst of moves yields the
/// last key only.
#[derive(Clone, Debug, Default)]
pub struct SettleGate {
    /// The focused finding and when it became so.
    pending: Option<(String, u64)>,
    /// The last key [`due`](Self::due) handed out while it stayed focused.
    fired: Option<String>,
}

impl SettleGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// The cursor is on `key` (or on nothing) at `now_ms`. Calling it again for the key that is
    /// already pending does not restart its clock.
    pub fn moved(&mut self, key: Option<&str>, now_ms: u64) {
        match key {
            None => {
                self.pending = None;
                self.fired = None;
            }
            Some(key) => {
                if self.pending.as_ref().is_some_and(|(k, _)| k == key) {
                    return;
                }
                self.pending = Some((key.to_string(), now_ms));
                if self.fired.as_deref() != Some(key) {
                    self.fired = None;
                }
            }
        }
    }

    /// The key that has now been focused for [`SETTLE_MS`] and has not been handed out yet.
    pub fn due(&mut self, now_ms: u64) -> Option<String> {
        let (key, since) = self.pending.as_ref()?;
        if now_ms.saturating_sub(*since) < SETTLE_MS || self.fired.as_deref() == Some(key) {
            return None;
        }
        let key = key.clone();
        self.fired = Some(key.clone());
        Some(key)
    }
}

// ── The request ──────────────────────────────────────────────────────────────────────────

/// Where one context window is kept: the file at a commit, and the stored line. A window is the
/// lines around ONE line, so (unlike a file) it is keyed by the line too.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ContextKey {
    pub repo: String,
    pub sha: String,
    pub path: String,
    pub line: usize,
}

/// One tuple of the batch request (`POST /api/onboard/finding-context/batch`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct BatchItem {
    pub repo: String,
    pub path: String,
    pub line: usize,
    /// The inspected commit; left out when none is recorded, so the server uses its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    /// The first line of the stored snippet, so the server can say whether the line still matches.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expect: Option<String>,
    /// Echoed back unchanged: the finding's key, so an answer is matched to its finding.
    pub key: String,
}

/// The body of the batch request.
#[derive(Clone, Debug, serde::Serialize)]
pub struct BatchRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<&'a str>,
    pub items: &'a [BatchItem],
}

/// What the pane asks for one focused finding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextTarget {
    /// The finding's key ([`finding_key`]); an answer is shown only while this is focused.
    pub finding_key: String,
    pub item: BatchItem,
    /// Where the answer is kept. Its `sha` is empty when no commit is recorded, and the cache
    /// then refuses it (the server's default commit can change).
    pub cache: ContextKey,
}

impl ContextTarget {
    /// The request for `finding`, read at `sha` when the report recorded one. `None` for a
    /// finding with nothing to ask for (no repo, no path, no line): nothing is requested, and
    /// the pane keeps its snippet.
    pub fn for_finding(finding: &FindingView, sha: Option<&str>) -> Option<Self> {
        if finding.repo.is_empty() || finding.path.is_empty() || finding.line == 0 {
            return None;
        }
        let sha = sha.filter(|s| !s.is_empty());
        let expect = finding
            .snippet
            .lines()
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let key = finding_key(finding);
        Some(Self {
            finding_key: key.clone(),
            item: BatchItem {
                repo: finding.repo.clone(),
                path: finding.path.clone(),
                line: finding.line,
                sha: sha.map(str::to_string),
                expect,
                key,
            },
            cache: ContextKey {
                repo: finding.repo.clone(),
                sha: sha.unwrap_or_default().to_string(),
                path: finding.path.clone(),
                line: finding.line,
            },
        })
    }
}

// ── The answer ───────────────────────────────────────────────────────────────────────────

/// One entry of the batch response. Every field defaults, so an answer this build does not
/// recognise is a refusal to show rather than a parse failure.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct BatchAnswer {
    #[serde(default)]
    pub status: String,
    /// `git` (the inspected commit) or `working_tree`.
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub line: usize,
    #[serde(default)]
    pub start_line: usize,
    #[serde(default)]
    pub violation_line: usize,
    #[serde(default)]
    pub lines: Vec<String>,
    /// Defaults to `false`: an answer that does not say it matches is not shown.
    #[serde(default)]
    pub matches_snippet: bool,
}

/// The body of the batch response.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct BatchResponse {
    #[serde(default)]
    pub items: Vec<BatchAnswer>,
}

/// The lines shown under a stored snippet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneContext {
    /// The file line number of `lines[0]`.
    pub start_line: usize,
    /// The finding's line, highlighted.
    pub violation_line: usize,
    pub lines: Vec<String>,
}

impl PaneContext {
    /// `(file line number, text, is the violation line)` for each line, in order.
    pub fn rows(&self) -> impl Iterator<Item = (usize, &str, bool)> {
        self.lines.iter().enumerate().filter_map(move |(i, text)| {
            // A start line so large that numbering overflows has no rows to draw.
            let n = self.start_line.checked_add(i)?;
            Some((n, text.as_str(), n == self.violation_line))
        })
    }

    /// The text of the window's line `line`, if the window holds it.
    pub fn line(&self, line: usize) -> Option<&str> {
        let at = line.checked_sub(self.start_line)?;
        self.lines.get(at).map(String::as_str)
    }

    /// Whether the window still matches the first line of a stored snippet: the line it holds at
    /// `self.violation_line` against `expect`. A window is shared by every finding on its line,
    /// so each requester applies its own snippet.
    pub fn matches_snippet(&self, expect: Option<&str>) -> bool {
        match expect.map(str::trim).filter(|e| !e.is_empty()) {
            None => true,
            Some(expect) => self
                .line(self.violation_line)
                .is_some_and(|line| line_matches_snippet(line, expect)),
        }
    }

    /// Whether the window is well formed for a finding on `line`: numbered from 1, no longer
    /// than the radius allows, and holding the finding's own line.
    fn is_sound_for(&self, line: usize) -> bool {
        self.start_line >= 1
            && !self.lines.is_empty()
            && self.lines.len() <= MAX_CONTEXT_LINES
            && self.violation_line == line
            // No unchecked addition: `start_line` is the server's number and may be anything.
            // Every line of the window must be numberable, and the finding's line must be one.
            && self.start_line.checked_add(self.lines.len() - 1).is_some()
            && line >= self.start_line
            && line - self.start_line < self.lines.len()
    }
}

/// The context to show for `target` from the server's answer, or `None` (show nothing new).
///
/// False negatives over false positives: nothing is shown unless the answer is the read of the
/// inspected commit (`ok`, `git`), is the answer to THIS finding (its key and its repo, path and
/// line echo), says the line still matches the stored snippet, and is a sound window.
pub fn accept(target: &ContextTarget, response: &BatchResponse) -> Option<PaneContext> {
    let answer = response
        .items
        .iter()
        .find(|a| a.key.as_deref() == Some(target.finding_key.as_str()))?;
    let item = &target.item;
    if answer.status != "ok"
        || answer.source != "git"
        || !answer.matches_snippet
        || answer.repo != item.repo
        || answer.path != item.path
        || answer.line != item.line
        || answer.violation_line != item.line
    {
        return None;
    }
    let context = PaneContext {
        start_line: answer.start_line,
        violation_line: answer.violation_line,
        lines: answer.lines.clone(),
    };
    context.is_sound_for(item.line).then_some(context)
}

/// The same window, cut from a file the viewer already holds, so no request is needed. Only a
/// read of the inspected commit that carries every line of the window, and only while the
/// finding's line still matches `expect`; a working-tree read or a partial window is `None`.
pub fn context_from_file(
    view: &SourceFileView,
    line: usize,
    expect: Option<&str>,
) -> Option<PaneContext> {
    if line == 0 || !view.is_cacheable() || line > view.total() {
        return None;
    }
    let start = line.saturating_sub(CONTEXT_RADIUS).max(1);
    let end = (line + CONTEXT_RADIUS).min(view.total());
    let lines: Vec<String> = (start..=end)
        .map(|n| view.line(n).map(|segments| segments_text(segments)))
        .collect::<Option<_>>()?;
    let window = PaneContext {
        start_line: start,
        violation_line: line,
        lines,
    };
    window.matches_snippet(expect).then_some(window)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewer::WindowView;

    fn finding(path: &str, line: usize, snippet: &str) -> FindingView {
        serde_json::from_value(serde_json::json!({
            "repo": "me/api", "path": path, "line": line, "rule_id": "R-1", "severity": "high",
            "snippet": snippet, "detail": "d", "status": "active", "kind": "defect"
        }))
        .expect("fixture")
    }

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn target(line: usize) -> ContextTarget {
        ContextTarget::for_finding(&finding("src/a.rs", line, "let x = 1;\nmore"), Some(SHA))
            .expect("target")
    }

    fn answer_for(t: &ContextTarget) -> BatchAnswer {
        BatchAnswer {
            status: "ok".into(),
            source: "git".into(),
            key: Some(t.finding_key.clone()),
            repo: t.item.repo.clone(),
            path: t.item.path.clone(),
            line: t.item.line,
            start_line: t.item.line - 1,
            violation_line: t.item.line,
            lines: vec!["a".into(), "let x = 1;".into(), "c".into()],
            matches_snippet: true,
        }
    }

    fn response(a: BatchAnswer) -> BatchResponse {
        BatchResponse { items: vec![a] }
    }

    // ── SettleGate ───────────────────────────────────────────────────────────────────────

    #[test]
    fn three_moves_inside_200ms_hand_out_one_key_the_last() {
        let mut gate = SettleGate::new();
        let mut fired = Vec::new();
        gate.moved(Some("A"), 0);
        gate.moved(Some("B"), 60);
        gate.moved(Some("C"), 120);
        // The caller asks once per move, SETTLE_MS after it.
        for at in [150, 210, 270] {
            fired.extend(gate.due(at));
        }
        assert_eq!(
            fired,
            ["C"],
            "one request, for where the cursor came to rest"
        );
        assert_eq!(gate.due(1_000), None, "and it is handed out once");
    }

    #[test]
    fn a_key_is_not_due_before_it_has_rested_the_full_time() {
        let mut gate = SettleGate::new();
        gate.moved(Some("A"), 1_000);
        assert_eq!(gate.due(1_000 + SETTLE_MS - 1), None);
        assert_eq!(gate.due(1_000 + SETTLE_MS), Some("A".to_string()));
    }

    #[test]
    fn re_announcing_the_same_key_does_not_restart_its_clock() {
        let mut gate = SettleGate::new();
        gate.moved(Some("A"), 0);
        gate.moved(Some("A"), 100);
        assert_eq!(gate.due(150), Some("A".to_string()));
        gate.moved(Some("A"), 200);
        assert_eq!(
            gate.due(900),
            None,
            "already handed out while it stayed focused"
        );
    }

    #[test]
    fn leaving_and_coming_back_asks_again_and_a_blank_cursor_asks_nothing() {
        let mut gate = SettleGate::new();
        gate.moved(Some("A"), 0);
        assert_eq!(gate.due(150), Some("A".to_string()));
        gate.moved(Some("B"), 160);
        gate.moved(Some("A"), 170);
        assert_eq!(gate.due(320), Some("A".to_string()));
        gate.moved(None, 400);
        assert_eq!(gate.due(1_000), None);
    }

    // ── ContextTarget ────────────────────────────────────────────────────────────────────

    #[test]
    fn the_target_names_the_finding_the_commit_and_the_first_snippet_line() {
        let t = target(10);
        assert_eq!(t.item.repo, "me/api");
        assert_eq!(t.item.path, "src/a.rs");
        assert_eq!(t.item.line, 10);
        assert_eq!(t.item.sha.as_deref(), Some(SHA));
        assert_eq!(t.item.expect.as_deref(), Some("let x = 1;"));
        assert_eq!(t.item.key, t.finding_key);
        assert_eq!(
            t.cache,
            ContextKey {
                repo: "me/api".into(),
                sha: SHA.into(),
                path: "src/a.rs".into(),
                line: 10
            }
        );
    }

    #[test]
    fn a_finding_with_nothing_to_ask_for_is_not_requested() {
        assert!(ContextTarget::for_finding(&finding("src/a.rs", 0, "x"), Some(SHA)).is_none());
        assert!(ContextTarget::for_finding(&finding("", 4, "x"), Some(SHA)).is_none());
        let mut no_repo = finding("src/a.rs", 4, "x");
        no_repo.repo.clear();
        assert!(ContextTarget::for_finding(&no_repo, Some(SHA)).is_none());
    }

    #[test]
    fn no_recorded_commit_is_asked_without_one_and_cached_under_none() {
        let t = ContextTarget::for_finding(&finding("src/a.rs", 4, "x"), None).unwrap();
        assert_eq!(t.item.sha, None);
        assert_eq!(t.cache.sha, "");
        let wire = serde_json::to_value(&t.item).unwrap();
        assert!(
            wire.get("sha").is_none(),
            "no sha key when none is recorded: {wire}"
        );
        assert_eq!(wire["key"], t.finding_key.as_str());
    }

    #[test]
    fn the_request_body_carries_the_project_only_when_known() {
        let t = target(5);
        let items = [t.item.clone()];
        let with = serde_json::to_value(BatchRequest {
            project: Some("p1"),
            items: &items,
        })
        .unwrap();
        assert_eq!(with["project"], "p1");
        assert_eq!(with["items"][0]["line"], 5);
        let without = serde_json::to_value(BatchRequest {
            project: None,
            items: &items,
        })
        .unwrap();
        assert!(without.get("project").is_none());
    }

    // ── accept ───────────────────────────────────────────────────────────────────────────

    #[test]
    fn an_ok_read_of_the_inspected_commit_is_shown_with_the_violation_line_marked() {
        let t = target(10);
        let ctx = accept(&t, &response(answer_for(&t))).expect("shown");
        assert_eq!(ctx.start_line, 9);
        let rows: Vec<(usize, &str, bool)> = ctx.rows().collect();
        assert_eq!(
            rows,
            [(9, "a", false), (10, "let x = 1;", true), (11, "c", false)]
        );
    }

    #[test]
    fn every_non_ok_status_shows_nothing() {
        let t = target(10);
        for status in [
            "commit_unavailable",
            "source_deleted",
            "file_missing",
            "not_utf8",
            "too_large",
            "line_gone",
            "path_traversal",
            "unavailable",
            "",
            "something_new",
        ] {
            let mut a = answer_for(&t);
            a.status = status.into();
            assert_eq!(accept(&t, &response(a)), None, "{status}");
        }
    }

    #[test]
    fn a_working_tree_read_a_mismatch_or_a_missing_flag_shows_nothing() {
        let t = target(10);
        let mut working = answer_for(&t);
        working.source = "working_tree".into();
        assert_eq!(accept(&t, &response(working)), None);
        let mut stale = answer_for(&t);
        stale.matches_snippet = false;
        assert_eq!(accept(&t, &response(stale)), None);
        // An answer that never says it matches is not shown (`serde(default)` is false).
        let bare: BatchResponse = serde_json::from_value(serde_json::json!({
            "items": [{ "status": "ok", "source": "git", "key": t.finding_key,
                        "repo": "me/api", "path": "src/a.rs", "line": 10,
                        "start_line": 9, "violation_line": 10, "lines": ["a", "b", "c"] }]
        }))
        .unwrap();
        assert_eq!(accept(&t, &bare), None);
    }

    #[test]
    fn an_answer_for_another_finding_is_never_shown() {
        let t = target(10);
        let other = target(30);
        // Right key, wrong echo.
        let mut wrong_line = answer_for(&t);
        wrong_line.line = 30;
        assert_eq!(accept(&t, &response(wrong_line)), None);
        let mut wrong_path = answer_for(&t);
        wrong_path.path = "src/other.rs".into();
        assert_eq!(accept(&t, &response(wrong_path)), None);
        // Another finding's answer, however well formed.
        assert_eq!(accept(&t, &response(answer_for(&other))), None);
        // No answer at all.
        assert_eq!(accept(&t, &BatchResponse::default()), None);
    }

    #[test]
    fn an_unsound_window_is_refused() {
        let t = target(10);
        let mut too_long = answer_for(&t);
        too_long.lines = (0..MAX_CONTEXT_LINES + 1).map(|n| n.to_string()).collect();
        too_long.start_line = 5;
        assert_eq!(accept(&t, &response(too_long)), None);
        let mut off = answer_for(&t);
        off.start_line = 11; // the window does not hold line 10
        assert_eq!(accept(&t, &response(off)), None);
        let mut empty = answer_for(&t);
        empty.lines.clear();
        assert_eq!(accept(&t, &response(empty)), None);
        let mut zero = answer_for(&t);
        zero.start_line = 0;
        assert_eq!(accept(&t, &response(zero)), None);
    }

    // ── context_from_file ────────────────────────────────────────────────────────────────

    fn whole_file(total: usize) -> SourceFileView {
        SourceFileView {
            status: "ok".into(),
            source: "git".into(),
            total_lines: total,
            window: Some(WindowView {
                from: 1,
                to: total,
                total_lines: total,
            }),
            lines: (1..=total)
                .map(|n| vec![(None, "row ".to_string()), (Some(0), n.to_string())])
                .collect(),
            ..SourceFileView::default()
        }
    }

    #[test]
    fn a_held_file_yields_the_same_window_and_clips_at_both_ends() {
        let file = whole_file(20);
        let mid = context_from_file(&file, 10, Some("row 10")).unwrap();
        assert_eq!(mid.start_line, 7);
        assert_eq!(mid.lines.len(), 7);
        assert_eq!(mid.lines[3], "row 10");
        let first = context_from_file(&file, 1, None).unwrap();
        assert_eq!((first.start_line, first.lines.len()), (1, 4));
        let last = context_from_file(&file, 20, None).unwrap();
        assert_eq!((last.start_line, last.lines.len()), (17, 4));
    }

    #[test]
    fn a_held_file_that_does_not_qualify_yields_nothing() {
        let mut working = whole_file(20);
        working.source = "working_tree".into();
        assert_eq!(context_from_file(&working, 10, None), None);
        let file = whole_file(20);
        assert_eq!(context_from_file(&file, 0, None), None);
        assert_eq!(context_from_file(&file, 21, None), None, "past the end");
        // The line no longer matches the stored snippet.
        assert_eq!(context_from_file(&file, 10, Some("something else")), None);
        // A windowed answer that lacks part of the window is a miss, not a short context.
        let mut windowed = whole_file(20);
        windowed.window = Some(WindowView {
            from: 8,
            to: 20,
            total_lines: 20,
        });
        windowed.lines = windowed.lines.split_off(7);
        assert_eq!(context_from_file(&windowed, 9, None), None);
        assert!(context_from_file(&windowed, 12, None).is_some());
    }

    /// L1: the server's `start_line` is untrusted. One that would overflow when added to a
    /// length is refused, never wrapped around into a window that looks sound.
    #[test]
    fn a_start_line_at_the_top_of_the_range_overflows_nothing() {
        let t = target(usize::MAX);
        let mut a = answer_for(&t);
        a.start_line = usize::MAX;
        a.lines = vec!["only".into()];
        // The window is sound only if it holds the line, and `usize::MAX` is both.
        let held = accept(&t, &response(a.clone())).expect("a one-line window at the maximum");
        assert_eq!(held.rows().count(), 1);
        // Two lines starting at the maximum would number past it: refused, not wrapped.
        a.lines = vec!["a".into(), "b".into()];
        assert_eq!(accept(&t, &response(a)), None);
        // Rows of such a window cannot be numbered past the maximum either.
        let window = PaneContext {
            start_line: usize::MAX,
            violation_line: usize::MAX,
            lines: vec!["a".into(), "b".into(), "c".into()],
        };
        assert_eq!(window.rows().count(), 1, "only the line that has a number");
        // And a start line past the finding's line holds nothing of it.
        let t = target(10);
        let mut late = answer_for(&t);
        late.start_line = usize::MAX - 1;
        assert_eq!(accept(&t, &response(late)), None);
    }

    /// L11: a window cached for a line is shared by every finding on that line; each applies its
    /// own snippet.
    #[test]
    fn a_window_is_checked_against_each_requesters_own_snippet() {
        let window = PaneContext {
            start_line: 9,
            violation_line: 10,
            lines: vec!["a".into(), "let x = 1;".into(), "c".into()],
        };
        assert!(window.matches_snippet(Some("let x = 1;")));
        assert!(
            window.matches_snippet(Some("  let x  ")),
            "trimmed containment"
        );
        assert!(window.matches_snippet(None));
        assert!(window.matches_snippet(Some("  ")));
        assert!(!window.matches_snippet(Some("something else")));
        // A window that does not hold its own line matches no snippet.
        let broken = PaneContext {
            start_line: 1,
            violation_line: 10,
            lines: vec!["a".into()],
        };
        assert!(!broken.matches_snippet(Some("a")));
        assert!(broken.matches_snippet(None), "nothing to compare");
    }

    #[test]
    fn the_context_window_derives_from_the_shared_radius() {
        assert_eq!(
            MAX_CONTEXT_LINES,
            2 * camerata_api_types::snippet::CONTEXT_RADIUS + 1
        );
    }
}
