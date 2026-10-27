//! Pure logic for the in-app code viewer (`WORK_ORDER_REVIEW_SPEED.md` section A).
//!
//! The viewer is a read-only, keyboard-driven overlay that shows the file a finding lives in,
//! as it was at the inspected commit. Every judgment it makes lives here, as plain functions
//! with unit tests and no rendering framework: which key means what ([`viewer_command`]),
//! which finding is next in the file or in the group ([`step_in_file`],
//! [`step_file_in_group`]), where the cursor goes after a disposition
//! ([`advance_after_disposition`]), what the gutter shows ([`gutter_marks`]), which lines are
//! in the DOM ([`virtual_range`]), where a long line is cut ([`cut_line`]) and which files are
//! kept in the client cache ([`ViewerCache`]). The Dioxus layer chooses words and pixels only.
//!
//! ## One cursor
//!
//! The viewer never owns a "current finding". Every navigation function here returns a finding
//! KEY, and the call site writes it to the queue's [`RowCursor`](crate::triage::RowCursor).
//! The pane underneath follows the cursor, so the viewer, the pane and the queue cannot
//! disagree about which finding is current.
//!
//! ## One key map per surface, a small shared core
//!
//! [`viewer_command`] is a whitelist like [`pane_command`]. It does not re-implement the
//! disposition keys: it asks `pane_command` and maps the answer, so `C T I F` and the arrows
//! mean exactly what they mean in the pane, by construction. A test asserts the two maps share
//! only the keys listed in [`SHARED_KEYS`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::pane_context::{context_from_file, ContextKey, PaneContext, CONTEXT_CAPACITY};
use crate::source::{context_note, short_sha, ContextNote};
use crate::triage::{
    finding_key, finding_state, pane_command, rule_group_label, Disposition, FindingView,
    FocusMove, KeyMods, PaneCommand, PaneFocus, TriageState,
};

// ── The wire shape of GET /api/onboard/source-file ───────────────────────────────────────

/// One run of a line: `(class index, or None for plain text, text)`.
pub type Segment = (Option<usize>, String);

/// Which lines of the file this response carries (1-based, inclusive).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct WindowView {
    #[serde(default)]
    pub from: usize,
    #[serde(default)]
    pub to: usize,
    #[serde(default)]
    pub total_lines: usize,
}

/// The enclosing block of the line the request was centred on.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct BlockView {
    #[serde(default)]
    pub start_line: usize,
    #[serde(default)]
    pub end_line: usize,
    #[serde(default)]
    pub kind: String,
}

/// One `GET /api/onboard/source-file` response, deserialized as it came. Every field defaults,
/// so an answer this build does not recognise degrades to "nothing to show" rather than failing
/// to parse. `Eq` so an `Arc` of it compares by pointer first.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct SourceFileView {
    #[serde(default)]
    pub status: String,
    /// `git` (the inspected commit) or `working_tree`.
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub commit: String,
    #[serde(default)]
    pub head_differs: bool,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub deleted_at: Option<String>,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub total_lines: usize,
    #[serde(default)]
    pub window: Option<WindowView>,
    /// `ok`, `plain` or `skipped`.
    #[serde(default)]
    pub highlight: String,
    /// Class names, indexed by the first element of a [`Segment`].
    #[serde(default)]
    pub classes: Vec<String>,
    #[serde(default)]
    pub block: Option<BlockView>,
    /// The lines of `window`, in order.
    #[serde(default)]
    pub lines: Vec<Vec<Segment>>,
    /// The `around` line the request that produced this answer named. Set by the client, not
    /// the server: the server's `block` is for that line (see [`block_for`]).
    #[serde(skip)]
    pub requested_around: Option<usize>,
}

/// What a response means for the viewer's body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceOutcome {
    /// Lines to show.
    Code,
    /// The client source was deleted on purpose. The text is ready to show; no code is shown.
    SourceDeleted(String),
    /// Over the viewer's cap. The modal's enclosing-block section is offered instead.
    TooLarge(String),
    NotUtf8(String),
    FileMissing(String),
    /// Anything else (path refused, an unknown status).
    Unavailable(String),
}

/// A line above the code, from the response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Banner {
    pub kind: BannerKind,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BannerKind {
    /// The code is NOT from the inspected commit: the working tree is shown.
    Amber,
    /// A note: the code is from the inspected commit and the checkout has moved on.
    Muted,
}

impl SourceFileView {
    /// The window, or an empty one when the response has none.
    pub fn window_or_default(&self) -> WindowView {
        self.window.clone().unwrap_or_default()
    }

    /// The number of lines in the whole file.
    pub fn total(&self) -> usize {
        self.window
            .as_ref()
            .map(|w| w.total_lines)
            .filter(|n| *n > 0)
            .unwrap_or(self.total_lines)
    }

    /// The segments of file line `n` (1-based), when this response carries it.
    pub fn line(&self, n: usize) -> Option<&Vec<Segment>> {
        let w = self.window.as_ref()?;
        if n < w.from || n > w.to {
            return None;
        }
        self.lines.get(n - w.from)
    }

    /// Whether this response carries file line `n`, or `n` is past the end of a file whose end
    /// it carries (so a stale line number does not force a refetch forever).
    pub fn covers(&self, n: usize) -> bool {
        let Some(w) = self.window.as_ref() else {
            return false;
        };
        let n = n.max(1);
        (n >= w.from && n <= w.to) || (n > w.total_lines && w.to >= w.total_lines)
    }

    /// Whether this response is the whole file.
    pub fn is_whole(&self) -> bool {
        self.window
            .as_ref()
            .is_some_and(|w| w.from <= 1 && w.to >= w.total_lines)
    }

    /// Whether this answer may be kept in the client cache: a read of the inspected commit.
    /// A working-tree read can change under the viewer, and a refusal can be fixed by a
    /// re-fetch of the source, so neither is kept.
    pub fn is_cacheable(&self) -> bool {
        self.status == "ok" && self.source == "git" && self.window.is_some()
    }

    /// The text for a deleted source: the response's own, else the shared wording.
    fn deleted_message(&self) -> String {
        if !self.reason.trim().is_empty() {
            return self.reason.clone();
        }
        match context_note(
            "source_deleted",
            &self.source,
            &self.commit,
            false,
            self.deleted_at.as_deref(),
        ) {
            Some(ContextNote::SourceDeleted(text)) => text,
            _ => "Source deleted, re-fetch to view".to_string(),
        }
    }

    fn reason_or(&self, fallback: &str) -> String {
        if self.reason.trim().is_empty() {
            fallback.to_string()
        } else {
            self.reason.clone()
        }
    }

    /// What this response means for the body of the viewer.
    pub fn outcome(&self) -> SourceOutcome {
        if self.window.is_some() && matches!(self.status.as_str(), "ok" | "commit_unavailable") {
            return SourceOutcome::Code;
        }
        match self.status.as_str() {
            "source_deleted" => SourceOutcome::SourceDeleted(self.deleted_message()),
            "too_large" => SourceOutcome::TooLarge(self.reason_or("File is too large to view")),
            "not_utf8" => SourceOutcome::NotUtf8(self.reason_or("File is not valid UTF-8")),
            "file_missing" => SourceOutcome::FileMissing(self.reason_or("File not found")),
            "path_traversal" => {
                SourceOutcome::Unavailable(self.reason_or("This file path was refused"))
            }
            _ => SourceOutcome::Unavailable(self.reason_or("This file could not be loaded")),
        }
    }

    /// The banner for a response that shows code. Keyed on `source`, never on `status` alone:
    /// the amber banner appears exactly when the code is the working tree, which is both the
    /// "commit not available in this clone" case and the "no commit was recorded" case. The
    /// text is the response's own `reason` when it has one.
    pub fn banner(&self) -> Option<Banner> {
        if self.outcome() != SourceOutcome::Code {
            return None;
        }
        if self.source == "working_tree" {
            return Some(Banner {
                kind: BannerKind::Amber,
                text: self.reason_or("No inspected commit was recorded; showing the working tree"),
            });
        }
        match context_note(
            &self.status,
            &self.source,
            &self.commit,
            self.head_differs,
            None,
        ) {
            Some(ContextNote::NewerSource(text)) => Some(Banner {
                kind: BannerKind::Muted,
                text,
            }),
            _ => None,
        }
    }

    /// The 7-character commit this response was read at, when it was read from git.
    pub fn short_commit(&self) -> Option<&str> {
        (self.source == "git" && !self.commit.is_empty()).then(|| short_sha(&self.commit))
    }
}

// ── The client cache (A.2 step 8) ────────────────────────────────────────────────────────

/// How many files the client keeps.
pub const CACHE_CAPACITY: usize = 8;

/// A file as the cache knows it: the repo, the inspected commit and the path.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FileKey {
    pub repo: String,
    pub sha: String,
    pub path: String,
}

/// An LRU of the last [`CACHE_CAPACITY`] files (and, in a lane of their own, the detail pane's
/// context windows), so moving between findings in one file never
/// refetches and returning to a file in the same group is instant.
///
/// Keyed by [`FileKey`]. A file with no recorded commit (`sha` empty) is never kept, because
/// the server then answers with whatever the project's latest inspection says and that can
/// change. A windowed answer is kept as the most recent window of its file; a lookup for a
/// line outside that window is a miss.
#[derive(Clone, Debug, Default)]
pub struct ViewerCache {
    /// Least recently used first.
    entries: Vec<(FileKey, Arc<SourceFileView>)>,
    /// The detail pane's small windows (see [`crate::pane_context`]), in their own lane so they
    /// are never mistaken for a file and never push one out. Oldest first.
    contexts: Vec<(ContextKey, Arc<PaneContext>)>,
}

impl ViewerCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The keys, least recently used first.
    pub fn keys(&self) -> Vec<FileKey> {
        self.entries.iter().map(|(k, _)| k.clone()).collect()
    }

    /// A read-only lookup: the cached answer when it carries `line`. Does not change the
    /// recency order, so the render path can call it.
    pub fn lookup(&self, key: &FileKey, line: usize) -> Option<Arc<SourceFileView>> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .filter(|(_, v)| v.covers(line))
            .map(|(_, v)| Arc::clone(v))
    }

    /// [`lookup`](Self::lookup), and mark the file most recently used.
    pub fn get(&mut self, key: &FileKey, line: usize) -> Option<Arc<SourceFileView>> {
        let pos = self.entries.iter().position(|(k, _)| k == key)?;
        if !self.entries[pos].1.covers(line) {
            return None;
        }
        let entry = self.entries.remove(pos);
        let view = Arc::clone(&entry.1);
        self.entries.push(entry);
        Some(view)
    }

    /// Keep `view` under `key` as the most recently used file, dropping the least recently
    /// used beyond [`CACHE_CAPACITY`]. Returns whether it was kept.
    pub fn put(&mut self, key: FileKey, view: Arc<SourceFileView>) -> bool {
        if key.sha.is_empty() || !view.is_cacheable() {
            return false;
        }
        self.entries.retain(|(k, _)| *k != key);
        self.entries.push((key, view));
        while self.entries.len() > CACHE_CAPACITY {
            self.entries.remove(0);
        }
        true
    }

    /// How many context windows are kept.
    pub fn context_len(&self) -> usize {
        self.contexts.len()
    }

    /// The pane's window for one finding, if one is held: first a window fetched for exactly
    /// that finding (same repo, commit, path AND line), else the same window cut from a whole
    /// file this cache holds, so an open viewer and the pane share what has been read. `expect`
    /// is the first line of the stored snippet; a held file whose line no longer matches it
    /// yields nothing. A key with no commit is never answered (the server's default can change).
    ///
    /// Only the lines of a file are used, never its `block`: that is for another line.
    pub fn context(&self, key: &ContextKey, expect: Option<&str>) -> Option<Arc<PaneContext>> {
        if key.sha.is_empty() {
            return None;
        }
        if let Some((_, held)) = self.contexts.iter().find(|(k, _)| k == key) {
            return Some(Arc::clone(held));
        }
        let file = FileKey {
            repo: key.repo.clone(),
            sha: key.sha.clone(),
            path: key.path.clone(),
        };
        let (_, view) = self.entries.iter().find(|(k, _)| *k == file)?;
        context_from_file(view, key.line, expect).map(Arc::new)
    }

    /// Keep a pane window under `key`, newest last, dropping the oldest beyond
    /// [`CONTEXT_CAPACITY`]. Returns whether it was kept (a key with no commit is not).
    pub fn put_context(&mut self, key: ContextKey, context: Arc<PaneContext>) -> bool {
        if key.sha.is_empty() {
            return false;
        }
        self.contexts.retain(|(k, _)| *k != key);
        self.contexts.push((key, context));
        while self.contexts.len() > CONTEXT_CAPACITY {
            self.contexts.remove(0);
        }
        true
    }
}

// ── Long lines (A.3) ─────────────────────────────────────────────────────────────────────

/// A line longer than this is cut, so one minified bundle line cannot freeze layout.
pub const MAX_LINE_CHARS: usize = 2000;

/// The tail shown after a cut line.
pub fn more_chars_label(more: usize) -> String {
    format!("... ({more} more chars)")
}

/// Cut `segments` to at most `max` characters. `None` when the line already fits. Otherwise the
/// shortened segments (cut on a character boundary) and how many characters were dropped.
pub fn cut_line(segments: &[Segment], max: usize) -> Option<(Vec<Segment>, usize)> {
    // Characters never outnumber bytes, so a line within `max` bytes fits without counting.
    if segments.iter().map(|(_, t)| t.len()).sum::<usize>() <= max {
        return None;
    }
    let total: usize = segments.iter().map(|(_, t)| t.chars().count()).sum();
    if total <= max {
        return None;
    }
    let mut left = max;
    let mut out = Vec::new();
    for (class, text) in segments {
        if left == 0 {
            break;
        }
        let n = text.chars().count();
        if n <= left {
            out.push((*class, text.clone()));
            left -= n;
        } else {
            out.push((*class, text.chars().take(left).collect()));
            left = 0;
        }
    }
    Some((out, total - max))
}

// ── Virtualised lines (A.9) ──────────────────────────────────────────────────────────────

/// The fixed height of one code line, in CSS pixels. The stylesheet reads the same number
/// through `--code-line-h`, so `scrollTop` maps to a line index by arithmetic alone.
pub const CODE_LINE_H_PX: u32 = 20;

/// Lines rendered above and below the viewport.
pub const OVERSCAN_LINES: usize = 150;

/// The first rendered line snaps down to a multiple of this, so a slow scroll re-renders every
/// 25 lines rather than on every pixel.
pub const SCROLL_QUANTUM_LINES: usize = 25;

/// A range of file lines to put in the DOM (1-based, inclusive). Empty when `first > last`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VirtualRange {
    pub first: usize,
    pub last: usize,
}

impl VirtualRange {
    pub fn is_empty(self) -> bool {
        self.first > self.last
    }

    pub fn len(self) -> usize {
        if self.is_empty() {
            0
        } else {
            self.last - self.first + 1
        }
    }

    /// The part of this range inside `[from, to]`.
    pub fn within(self, from: usize, to: usize) -> Self {
        Self {
            first: self.first.max(from),
            last: self.last.min(to),
        }
    }
}

/// The 1-based line at the top edge of a viewport scrolled to `scroll_top` pixels.
pub fn line_at_scroll(scroll_top: f64, line_h: u32) -> usize {
    let h = f64::from(line_h.max(1));
    (scroll_top.max(0.0) / h).floor() as usize + 1
}

/// The lines to render for a viewport: the visible lines plus [`OVERSCAN_LINES`] either side,
/// clamped to the file. `total_lines == 0` gives an empty range.
pub fn virtual_range(
    total_lines: usize,
    scroll_top: f64,
    viewport_px: f64,
    line_h: u32,
    overscan: usize,
) -> VirtualRange {
    if total_lines == 0 {
        return VirtualRange { first: 1, last: 0 };
    }
    let top = line_at_scroll(scroll_top, line_h).min(total_lines);
    let bottom = line_at_scroll(scroll_top + viewport_px.max(0.0), line_h).min(total_lines);
    // Snap the top down so scrolling re-renders in steps.
    let snapped = ((top - 1) / SCROLL_QUANTUM_LINES) * SCROLL_QUANTUM_LINES + 1;
    VirtualRange {
        first: snapped.saturating_sub(overscan).max(1),
        last: (bottom + overscan).min(total_lines),
    }
}

/// The spacer heights (top, bottom) in pixels that stand in for the lines not rendered, so the
/// scroll height is always `total_lines * line_h`.
pub fn spacers(total_lines: usize, range: VirtualRange, line_h: u32) -> (u64, u64) {
    let h = u64::from(line_h);
    if range.is_empty() {
        return (total_lines as u64 * h, 0);
    }
    (
        (range.first as u64 - 1) * h,
        total_lines.saturating_sub(range.last) as u64 * h,
    )
}

/// The `scrollTop` that puts `line` in the upper third of the viewport, so the code below it
/// (what a finding is about) is visible. Never negative, never past the end of the file.
pub fn scroll_top_for_line(line: usize, line_h: u32, viewport_px: f64, total_lines: usize) -> f64 {
    let h = f64::from(line_h);
    let want = (line.max(1) - 1) as f64 * h - viewport_px / 3.0;
    let max = (total_lines as f64 * h - viewport_px).max(0.0);
    want.clamp(0.0, max)
}

/// Lines of a window left unrendered at its edges before the viewer asks for a new window.
pub const PAGE_MARGIN_LINES: usize = 50;

/// For a file delivered as a window (400 KB to 4 MB): the line to centre a new window on when
/// the viewport has come within [`PAGE_MARGIN_LINES`] of a window edge that is not the file's
/// edge. `None` for a whole file or when the viewport is well inside the window.
pub fn page_request(
    window: &WindowView,
    scroll_top: f64,
    viewport_px: f64,
    line_h: u32,
) -> Option<usize> {
    let total = window.total_lines;
    if total == 0 || (window.from <= 1 && window.to >= total) {
        return None;
    }
    let first = line_at_scroll(scroll_top, line_h).min(total);
    let last = line_at_scroll(scroll_top + viewport_px, line_h).min(total);
    let near_start = window.from > 1 && first < window.from + PAGE_MARGIN_LINES;
    let near_end = window.to < total && last + PAGE_MARGIN_LINES > window.to;
    (near_start || near_end).then(|| ((first + last) / 2).clamp(1, total))
}

/// Keys that scroll the viewport and leave the cursor alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollKey {
    PageUp,
    PageDown,
    Home,
    End,
}

/// The `scrollTop` after a scroll key. A page is the viewport less two lines of overlap.
pub fn scrolled(
    scroll_top: f64,
    key: ScrollKey,
    viewport_px: f64,
    total_lines: usize,
    line_h: u32,
) -> f64 {
    let h = f64::from(line_h);
    let page = (viewport_px - 2.0 * h).max(h);
    let max = (total_lines as f64 * h - viewport_px).max(0.0);
    match key {
        ScrollKey::PageUp => (scroll_top - page).max(0.0),
        ScrollKey::PageDown => (scroll_top + page).min(max),
        ScrollKey::Home => 0.0,
        ScrollKey::End => max,
    }
}

// ── The key map (A.8) ────────────────────────────────────────────────────────────────────

/// Where keyboard focus is inside the viewer. Only [`ViewerFocus::Code`] and
/// [`ViewerFocus::RefsList`] arm commands: the list answers its own four keys (move, open,
/// close), and every other focus is a text field or another surface that owns every key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewerFocus {
    Code,
    /// The in-file search field (`/`).
    SearchField,
    GoToLine,
    /// The symbol prompt (`S`).
    SymbolPrompt,
    /// The reason prompt for `I` and `F`.
    ReasonPrompt,
    /// The list of symbol references.
    RefsList,
    /// The Rule Hub is open above the viewer (`H`). It owns the keyboard until it closes.
    RuleHub,
}

/// Which prompts and surfaces are open over the code.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OpenSurfaces {
    pub reason_prompt: bool,
    pub goto: bool,
    pub search: bool,
    pub symbol_prompt: bool,
    pub refs_list: bool,
    pub rule_hub: bool,
}

/// The focus implied by which prompts are open. Pure, so the viewer needs no DOM focus
/// tracking: a prompt is open or it is not. An overlay above the viewer wins, then the prompt
/// that was opened last in the order a reviewer reaches them.
pub fn viewer_focus(open: OpenSurfaces) -> ViewerFocus {
    if open.rule_hub {
        ViewerFocus::RuleHub
    } else if open.reason_prompt {
        ViewerFocus::ReasonPrompt
    } else if open.symbol_prompt {
        ViewerFocus::SymbolPrompt
    } else if open.search {
        ViewerFocus::SearchField
    } else if open.goto {
        ViewerFocus::GoToLine
    } else if open.refs_list {
        ViewerFocus::RefsList
    } else {
        ViewerFocus::Code
    }
}

/// What a press in the viewer asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewerCommand {
    /// Close the viewer.
    Close,
    /// Apply this disposition at once (the same path as the pane).
    Dispose(TriageState),
    /// Open the reason prompt for this disposition (the same path as the pane).
    PromptFor(TriageState),
    /// Next or previous row in queue order.
    MoveRow(FocusMove),
    /// Next (`Down`) or previous (`Up`) finding in this file.
    FindingInFile(FocusMove),
    /// Next (`Down`) or previous (`Up`) file in this group.
    FileInGroup(FocusMove),
    /// Scroll back to the current finding's line.
    BackToFinding,
    OpenGoToLine,
    /// Pop the viewer history.
    Back,
    Scroll(ScrollKey),
    /// Show or hide the key list.
    ToggleHelp,
    /// Open the in-file search field (`/`).
    OpenSearch,
    /// Open the symbol prompt (`S`).
    OpenSymbols,
    /// Copy the permalink of the line on screen (`Y`).
    CopyPermalink,
    /// Pin or unpin the finding popover (`Space`).
    TogglePopover,
    /// Open the Rule Hub for the current rule (`H`).
    OpenHub,
    /// In the references list: next or previous hit.
    RefsMove(FocusMove),
    /// In the references list: open the selected hit.
    RefsOpen,
    /// In the references list: back to the code.
    RefsClose,
}

/// THE VIEWER'S SCOPED KEY HANDLER, as a pure function.
///
/// A whitelist, like [`pane_command`]: any key not named here, in any focus other than
/// [`ViewerFocus::Code`], returns `None`. Ctrl, Alt and Meta presses are refused so Cmd-C
/// still copies code and Cmd-F is not swallowed; the one exception is `Alt+ArrowLeft`, the
/// browser's "back".
///
/// The disposition keys and the arrows are NOT spelled out here. They are asked of
/// [`pane_command`] and mapped, so `C T I F ArrowUp ArrowDown` mean exactly what they mean in
/// the pane, `I` and `F` prompt for a reason, and a new disposition hotkey reaches the viewer
/// by being added once.
pub fn viewer_command(focus: ViewerFocus, key: &str, mods: KeyMods) -> Option<ViewerCommand> {
    if focus == ViewerFocus::RefsList {
        return refs_command(key, mods);
    }
    if focus != ViewerFocus::Code {
        return None;
    }
    if mods.ctrl || mods.meta {
        return None;
    }
    if mods.alt {
        return (key == "ArrowLeft" && !mods.shift).then_some(ViewerCommand::Back);
    }
    match key {
        "Escape" => return Some(ViewerCommand::Close),
        "Backspace" => return Some(ViewerCommand::Back),
        "PageUp" => return Some(ViewerCommand::Scroll(ScrollKey::PageUp)),
        "PageDown" => return Some(ViewerCommand::Scroll(ScrollKey::PageDown)),
        "Home" => return Some(ViewerCommand::Scroll(ScrollKey::Home)),
        "End" => return Some(ViewerCommand::Scroll(ScrollKey::End)),
        _ => {}
    }
    if let Some(cmd) = pane_command(PaneFocus::Cursor, key, mods) {
        return match cmd {
            PaneCommand::Dispose(s) => Some(ViewerCommand::Dispose(s)),
            PaneCommand::PromptFor(s) => Some(ViewerCommand::PromptFor(s)),
            PaneCommand::MoveFocus(d) => Some(ViewerCommand::MoveRow(d)),
            // `O` opens the viewer; inside it there is nothing to open.
            PaneCommand::OpenCode => None,
        };
    }
    if key.chars().count() != 1 {
        return None;
    }
    match key.to_ascii_lowercase().as_str() {
        "j" => Some(ViewerCommand::MoveRow(FocusMove::Down)),
        "k" => Some(ViewerCommand::MoveRow(FocusMove::Up)),
        "]" => Some(ViewerCommand::FindingInFile(FocusMove::Down)),
        "[" => Some(ViewerCommand::FindingInFile(FocusMove::Up)),
        "}" => Some(ViewerCommand::FileInGroup(FocusMove::Down)),
        "{" => Some(ViewerCommand::FileInGroup(FocusMove::Up)),
        "." => Some(ViewerCommand::BackToFinding),
        "g" => Some(ViewerCommand::OpenGoToLine),
        "?" => Some(ViewerCommand::ToggleHelp),
        "/" => Some(ViewerCommand::OpenSearch),
        "s" => Some(ViewerCommand::OpenSymbols),
        "y" => Some(ViewerCommand::CopyPermalink),
        " " => Some(ViewerCommand::TogglePopover),
        "h" => Some(ViewerCommand::OpenHub),
        _ => None,
    }
}

/// The references list's own four keys. Nothing else is answered there, so `C` cannot decide a
/// finding while the curator is choosing a hit; a modifier refuses everything.
fn refs_command(key: &str, mods: KeyMods) -> Option<ViewerCommand> {
    if mods.ctrl || mods.alt || mods.meta {
        return None;
    }
    match key {
        "ArrowDown" | "j" | "J" => Some(ViewerCommand::RefsMove(FocusMove::Down)),
        "ArrowUp" | "k" | "K" => Some(ViewerCommand::RefsMove(FocusMove::Up)),
        "Enter" => Some(ViewerCommand::RefsOpen),
        "Escape" => Some(ViewerCommand::RefsClose),
        _ => None,
    }
}

/// Whether the viewer answers keys under this focus. `Esc` is answered by exactly the two
/// focuses that arm commands ([`ViewerFocus::Code`] closes the viewer, [`ViewerFocus::RefsList`]
/// closes the list), so the strip and the handler cannot drift apart.
pub fn viewer_keys_armed(focus: ViewerFocus) -> bool {
    viewer_command(focus, "Escape", KeyMods::none()).is_some()
}

/// Whether this press closes the Rule Hub that `H` opened above the viewer. The hub has no key
/// handling of its own and DOM focus stays on the viewer, so the viewer relays `Esc` to it; no
/// other key does anything while the hub is up.
pub fn closes_hub(focus: ViewerFocus, key: &str, mods: KeyMods) -> bool {
    focus == ViewerFocus::RuleHub && key == "Escape" && !(mods.ctrl || mods.alt || mods.meta)
}

/// Where the keyboard goes back to when the Rule Hub closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusReturn {
    /// The code viewer, which opened the hub with `H` (or its popover) and is still open.
    Viewer,
    /// The detail pane (or nowhere, when this surface has no pane): the hub was opened from the
    /// Queue itself, with the viewer closed.
    Pane,
}

/// Where focus returns when the Rule Hub closes. The hub is opened from the pane, the deviation
/// bar or the viewer; the viewer is the only one of those that is a layer over the others, so
/// it is the destination exactly when it is open.
pub fn hub_close_focus(viewer_open: bool) -> FocusReturn {
    if viewer_open {
        FocusReturn::Viewer
    } else {
        FocusReturn::Pane
    }
}

/// Whether the key strip should show the code's keys (as opposed to the list's).
pub fn viewer_code_keys_armed(focus: ViewerFocus) -> bool {
    viewer_command(focus, "c", KeyMods::none()).is_some()
}

/// Whether `cmd` may run while a reference in ANOTHER file is on screen. The queue's current
/// finding is not on screen then, so a decision key must not act on code the curator cannot see.
/// Everything that moves the cursor, reads or leaves is allowed (a cursor move ends the
/// excursion, since the viewer re-opens on the new finding).
pub fn allowed_in_reference_view(cmd: ViewerCommand) -> bool {
    !matches!(cmd, ViewerCommand::Dispose(_) | ViewerCommand::PromptFor(_))
}

/// Whether `cmd` is a navigation that retires the search highlights ("until the next
/// navigation"). Scrolling, the key list, the popover and the search itself keep them.
pub fn clears_search(cmd: ViewerCommand) -> bool {
    matches!(
        cmd,
        ViewerCommand::MoveRow(_)
            | ViewerCommand::FindingInFile(_)
            | ViewerCommand::FileInGroup(_)
            | ViewerCommand::BackToFinding
            | ViewerCommand::OpenGoToLine
            | ViewerCommand::Back
            | ViewerCommand::RefsOpen
    )
}

/// The keys the pane and the viewer deliberately share (lower case; the letters are matched
/// case-insensitively). Everything else in one map must be absent from the other.
pub const SHARED_KEYS: [&str; 6] = ["c", "t", "i", "f", "ArrowUp", "ArrowDown"];

/// One row of the key list: what is shown, what it does, and the presses that prove it (key,
/// with Alt held). A test checks that these probes are exactly the keys the map answers.
pub struct KeyRow {
    pub label: &'static str,
    pub action: &'static str,
    pub probes: &'static [(&'static str, bool)],
}

/// The viewer's keys, for the `?` list and the user guide.
pub const KEY_ROWS: [KeyRow; 16] = [
    KeyRow {
        label: "Esc",
        action: "Close the key list, else the viewer",
        probes: &[("Escape", false)],
    },
    KeyRow {
        label: "C  T",
        action: "Confirm, tech debt (applied at once, 5 second undo)",
        probes: &[("c", false), ("t", false)],
    },
    KeyRow {
        label: "I  F",
        action: "Ignore, false positive (asks for a reason)",
        probes: &[("i", false), ("f", false)],
    },
    KeyRow {
        label: "Down  Up  J  K",
        action: "Next or previous row in the queue",
        probes: &[
            ("ArrowDown", false),
            ("ArrowUp", false),
            ("j", false),
            ("k", false),
        ],
    },
    KeyRow {
        label: "]  [",
        action: "Next or previous finding in this file",
        probes: &[("]", false), ("[", false)],
    },
    KeyRow {
        label: "}  {",
        action: "Next or previous file in this group",
        probes: &[("}", false), ("{", false)],
    },
    KeyRow {
        label: ".",
        action: "Back to the line of this finding",
        probes: &[(".", false)],
    },
    KeyRow {
        label: "G",
        action: "Go to a line",
        probes: &[("g", false)],
    },
    KeyRow {
        label: "/",
        action: "Search this file (Enter next, Shift+Enter previous, Esc closes)",
        probes: &[("/", false)],
    },
    KeyRow {
        label: "S",
        action: "Where else is this symbol used (Enter runs, then Up Down, Enter opens, Esc back)",
        probes: &[("s", false)],
    },
    KeyRow {
        label: "Y",
        action: "Copy a permalink to this line",
        probes: &[("y", false)],
    },
    KeyRow {
        label: "Space",
        action: "Pin or unpin the finding popover",
        probes: &[(" ", false)],
    },
    KeyRow {
        label: "H",
        action: "Open the Rule Hub for this rule (closing it returns here)",
        probes: &[("h", false)],
    },
    KeyRow {
        label: "Backspace  Alt+Left",
        action: "Back to where you were",
        probes: &[("Backspace", false), ("ArrowLeft", true)],
    },
    KeyRow {
        label: "PgUp  PgDn  Home  End",
        action: "Scroll (the cursor stays)",
        probes: &[
            ("PageUp", false),
            ("PageDown", false),
            ("Home", false),
            ("End", false),
        ],
    },
    KeyRow {
        label: "?",
        action: "Show or hide this list",
        probes: &[("?", false)],
    },
];

// ── Findings in a file: the gutter, `]` and `[`, and what comes after a decision ─────────

/// One finding in the file the viewer is showing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileFinding {
    /// [`finding_key`].
    pub key: String,
    pub line: usize,
    pub rule_id: String,
    pub severity: String,
    pub state: TriageState,
    /// Whether the queue table holds this finding as a row, so the cursor can land on it. A
    /// decided finding has left the Unresolved table: its marker is shown hollow and is not a
    /// stop for `]`.
    pub navigable: bool,
}

/// Every finding of `(repo, path)` in line order (then rule id), with its state and whether the
/// cursor can reach it. `navigable_keys` is the key set of the rows in the queue table.
pub fn file_findings(
    findings: &[FindingView],
    dispositions: &HashMap<String, Disposition>,
    repo: &str,
    path: &str,
    navigable_keys: &HashSet<String>,
) -> Vec<FileFinding> {
    let mut out: Vec<FileFinding> = findings
        .iter()
        .filter(|f| f.repo == repo && f.path == path)
        .map(|f| {
            let key = finding_key(f);
            FileFinding {
                navigable: navigable_keys.contains(&key),
                key,
                line: f.line,
                rule_id: f.rule_id.clone(),
                severity: f.severity.clone(),
                state: finding_state(dispositions, f),
            }
        })
        .collect();
    out.sort_by(|a, b| (a.line, &a.rule_id, &a.key).cmp(&(b.line, &b.rule_id, &b.key)));
    out
}

/// Severity ladder position, worst first. Unknown words sort last.
pub fn severity_rank(severity: &str) -> u8 {
    match severity.to_ascii_lowercase().as_str() {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        "low" => 3,
        _ => 4,
    }
}

/// The class suffix for a severity (`critical`, `high`, `medium`, `low`); anything else is `low`.
pub fn severity_class(severity: &str) -> &'static str {
    match severity_rank(severity) {
        0 => "critical",
        1 => "high",
        2 => "medium",
        _ => "low",
    }
}

/// One marker in the gutter and on the minimap: all the findings that cite one line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GutterMark {
    pub line: usize,
    /// How many findings cite this line (a count badge shows when more than one).
    pub count: usize,
    /// The worst severity among them.
    pub severity: String,
    /// Every finding on this line is decided: the dot is hollow.
    pub decided: bool,
    /// The finding a click moves the cursor to: the first one the cursor can reach.
    pub target: Option<String>,
    /// A tooltip: `rule id (state)` per finding.
    pub title: String,
}

/// The markers for a file, one per cited line, in line order. `items` is [`file_findings`].
pub fn gutter_marks(items: &[FileFinding]) -> Vec<GutterMark> {
    let mut marks: Vec<GutterMark> = Vec::new();
    for item in items {
        let label = if item.state.is_decided() {
            format!("{} ({})", item.rule_id, item.state.label())
        } else {
            item.rule_id.clone()
        };
        match marks.last_mut() {
            Some(m) if m.line == item.line => {
                m.count += 1;
                if severity_rank(&item.severity) < severity_rank(&m.severity) {
                    m.severity.clone_from(&item.severity);
                }
                m.decided &= item.state.is_decided();
                if m.target.is_none() && item.navigable {
                    m.target = Some(item.key.clone());
                }
                m.title = format!("{}, {label}", m.title);
            }
            _ => marks.push(GutterMark {
                line: item.line,
                count: 1,
                severity: item.severity.clone(),
                decided: item.state.is_decided(),
                target: item.navigable.then(|| item.key.clone()),
                title: label,
            }),
        }
    }
    marks
}

/// "finding K of M" for the finding `key` among `items`: `(K, M)`. `K` is 0 when `key` is not
/// in the file's list.
pub fn finding_position(items: &[FileFinding], key: &str) -> (usize, usize) {
    let k = items.iter().position(|i| i.key == key).map_or(0, |i| i + 1);
    (k, items.len())
}

/// The result of a step: move the cursor to a finding, or stay and say why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileStep {
    Move(String),
    Stay(String),
}

/// `]` and `[`: the next or previous finding in this file, in line order. NO wrap: at the end
/// it stays and says so, so a repeated keypress cannot silently cross into another file. Only
/// findings the cursor can reach are stops.
pub fn step_in_file(items: &[FileFinding], current_key: &str, dir: FocusMove) -> FileStep {
    let Some(here) = items.iter().position(|i| i.key == current_key) else {
        return FileStep::Stay("This finding is not listed for this file".to_string());
    };
    let found = match dir {
        FocusMove::Down => items[here + 1..].iter().find(|i| i.navigable),
        FocusMove::Up => items[..here].iter().rev().find(|i| i.navigable),
    };
    match found {
        Some(i) => FileStep::Move(i.key.clone()),
        None => FileStep::Stay(match dir {
            FocusMove::Down => "Last finding in this file. Press } for the next file.".to_string(),
            FocusMove::Up => {
                "First finding in this file. Press { for the previous file.".to_string()
            }
        }),
    }
}

/// The files of one rule group, in the order the table shows them (first appearance in
/// `ordered`, which is the table's filtered and sorted rows).
pub fn files_in_group(ordered: &[FindingView], group_label: &str) -> Vec<(String, String)> {
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut files = Vec::new();
    for f in ordered {
        if rule_group_label(f) != group_label {
            continue;
        }
        let id = (f.repo.clone(), f.path.clone());
        if seen.insert(id.clone()) {
            files.push(id);
        }
    }
    files
}

/// `}` and `{`: the next or previous file in the current rule group. It moves to the first
/// undecided finding of that file in the group (the first finding if all are decided), by
/// line. No wrap.
pub fn step_file_in_group(
    ordered: &[FindingView],
    dispositions: &HashMap<String, Disposition>,
    current: &FindingView,
    dir: FocusMove,
) -> FileStep {
    let label = rule_group_label(current);
    let files = files_in_group(ordered, &label);
    let here = files
        .iter()
        .position(|(repo, path)| *repo == current.repo && *path == current.path);
    let Some(here) = here else {
        return FileStep::Stay("This file is not in the table".to_string());
    };
    let target = match dir {
        FocusMove::Down => files.get(here + 1),
        FocusMove::Up => here.checked_sub(1).and_then(|i| files.get(i)),
    };
    let Some((repo, path)) = target else {
        return FileStep::Stay(match dir {
            FocusMove::Down => "Last file in this group.".to_string(),
            FocusMove::Up => "First file in this group.".to_string(),
        });
    };
    let mut rows: Vec<&FindingView> = ordered
        .iter()
        .filter(|f| f.repo == *repo && f.path == *path && rule_group_label(f) == label)
        .collect();
    rows.sort_by_key(|f| f.line);
    let pick = rows
        .iter()
        .find(|f| !finding_state(dispositions, f).is_decided())
        .or_else(|| rows.first());
    match pick {
        Some(f) => FileStep::Move(finding_key(f)),
        None => FileStep::Stay("No finding in that file".to_string()),
    }
}

/// The finding the next file in the group would open on, for the background prefetch: its
/// `(repo, path, line)`.
pub fn prefetch_target(
    ordered: &[FindingView],
    dispositions: &HashMap<String, Disposition>,
    current: &FindingView,
) -> Option<(String, String, usize)> {
    match step_file_in_group(ordered, dispositions, current, FocusMove::Down) {
        FileStep::Move(key) => ordered
            .iter()
            .find(|f| finding_key(f) == key)
            .map(|f| (f.repo.clone(), f.path.clone(), f.line)),
        FileStep::Stay(_) => None,
    }
}

/// After a disposition lands, where the viewer goes: the next undecided finding in this file
/// (after the one just decided, else the first one before it). `items` is the file's list as
/// it was BEFORE the decision. `None` when this file has no other undecided finding; the call
/// site then leaves the cursor where the queue put it, which is the next undecided row in
/// queue order (the decided row has left the table, so the cursor clamps to its successor).
pub fn advance_after_disposition(items: &[FileFinding], decided_key: &str) -> Option<String> {
    let here = items.iter().position(|i| i.key == decided_key)?;
    let open = |i: &&FileFinding| i.key != decided_key && i.navigable && !i.state.is_decided();
    items[here + 1..]
        .iter()
        .find(open)
        .or_else(|| items[..here].iter().find(open))
        .map(|i| i.key.clone())
}

// ── Go to line, and the history ──────────────────────────────────────────────────────────

/// Read the go-to-line field: a line number, optionally with an `L` in front. `Err` is the
/// message to show.
pub fn parse_goto_line(input: &str, total_lines: usize) -> Result<usize, String> {
    let text = input.trim().trim_start_matches(['L', 'l']);
    if total_lines == 0 {
        return Err("This file has no lines".to_string());
    }
    match text.parse::<usize>() {
        Ok(n) if (1..=total_lines).contains(&n) => Ok(n),
        _ => Err(format!("Enter a line from 1 to {total_lines}")),
    }
}

/// One place the viewer can go back to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    pub key: String,
    /// The file the position was in. The finding's own file for a position in the finding's
    /// code; another file after a symbol-reference jump.
    pub path: String,
    pub line: usize,
}

/// How many places the history keeps.
pub const HISTORY_CAP: usize = 50;

/// Remember `entry` as the place to go back to. A repeat of the top entry is not stored twice;
/// the oldest are dropped beyond [`HISTORY_CAP`].
pub fn push_history(stack: &mut Vec<HistoryEntry>, entry: HistoryEntry) {
    if stack.last() == Some(&entry) {
        return;
    }
    stack.push(entry);
    if stack.len() > HISTORY_CAP {
        stack.remove(0);
    }
}

/// The header strip's line label: `line 12 of 340`.
pub fn line_label(line: usize, total: usize) -> String {
    format!("line {line} of {total}")
}

// ── The finding's block, kept apart from the file's ──────────────────────────────────────

/// The enclosing block of the finding's own line, if `view` was requested for that line.
///
/// The server computes `block` for the `around` the request named, so a response only carries
/// THIS finding's block when it was fetched for this finding's line. A cached whole file was
/// fetched for another finding's line, and a page of a windowed file for the centre of the
/// viewport; their `block` marks other code. `Some(block)` (which may itself be `None`: a plain
/// file has no block) is a block that can be trusted; `None` means ask again.
pub fn block_for(view: &SourceFileView, violation_line: usize) -> Option<Option<BlockView>> {
    (view.requested_around == Some(violation_line)).then(|| view.block.clone())
}

/// The block for a small `finding-context` answer: `start_line` and the number of lines.
pub fn block_from_context(start_line: usize, line_count: usize, kind: &str) -> Option<BlockView> {
    (start_line > 0 && line_count > 0).then(|| BlockView {
        start_line,
        end_line: start_line + line_count - 1,
        kind: kind.to_string(),
    })
}

// ── What stays for the life of one open viewer ───────────────────────────────────────────

/// The last answer the LRU would not keep (a working-tree read, a refusal, or a repo with no
/// recorded commit), kept only while the viewer stays open so moving between findings in one file
/// does not refetch it. Cleared when the viewer closes; never shared across opens.
#[derive(Clone, Debug, Default)]
pub struct SessionCopy {
    entry: Option<(FileKey, Arc<SourceFileView>)>,
}

impl SessionCopy {
    pub fn new() -> Self {
        Self::default()
    }

    /// The kept answer for `key`, if it carries `line`.
    pub fn lookup(&self, key: &FileKey, line: usize) -> Option<Arc<SourceFileView>> {
        self.entry
            .as_ref()
            .filter(|(k, v)| k == key && v.covers(line))
            .map(|(_, v)| Arc::clone(v))
    }

    pub fn put(&mut self, key: FileKey, view: Arc<SourceFileView>) {
        self.entry = Some((key, view));
    }

    pub fn clear(&mut self) {
        self.entry = None;
    }

    pub fn is_empty(&self) -> bool {
        self.entry.is_none()
    }
}

// ── Back, closing, and what follows a decision ───────────────────────────────────────────

/// Where `Backspace` goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackStep {
    /// Scroll to this line of the finding's own file.
    Line(usize),
    /// Show this line of this file, which is not the finding's own (a reference excursion
    /// that went through more than one file).
    Location { path: String, line: usize },
    /// Move the cursor to this finding (the viewer then opens on it).
    Finding(String),
    /// The history is empty.
    Nothing,
}

/// Pop the history. An entry for the finding on screen is a scroll (in its own file, or in the
/// other file a reference jump went to); an entry for another finding is a cursor move, which the
/// caller must check can still be reached. `finding_path` is the file of the finding on screen.
pub fn back_step(
    history: &mut Vec<HistoryEntry>,
    current_key: &str,
    finding_path: &str,
) -> BackStep {
    match history.pop() {
        Some(e) if e.key == current_key && e.path == finding_path => BackStep::Line(e.line),
        Some(e) if e.key == current_key => BackStep::Location {
            path: e.path,
            line: e.line,
        },
        Some(e) => BackStep::Finding(e.key),
        None => BackStep::Nothing,
    }
}

/// What `Esc` closes: the key list if it is open, else the viewer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseTarget {
    Help,
    Viewer,
}

pub fn close_target(help_open: bool) -> CloseTarget {
    if help_open {
        CloseTarget::Help
    } else {
        CloseTarget::Viewer
    }
}

/// What the viewer does once the pane's disposition path has answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisposeOutcome {
    /// Close the viewer's reason prompt. False when the write was refused: the prompt stays open
    /// with what was typed.
    pub close_prompt: bool,
    /// The finding to move the cursor to, when the decision landed and the file has another
    /// undecided finding. `None` leaves the cursor where the queue put it.
    pub goto: Option<String>,
}

/// `landed` is the pane path's own answer (true only when it wrote a disposition), NOT an
/// inference from the disposition map: a blank reason for a row already in that state leaves the
/// map unchanged and must still read as refused. `target` was chosen BEFORE the write.
pub fn finish_disposition(landed: bool, target: Option<String>) -> DisposeOutcome {
    if landed {
        DisposeOutcome {
            close_prompt: true,
            goto: target,
        }
    } else {
        DisposeOutcome {
            close_prompt: false,
            goto: None,
        }
    }
}

// ── Which commit a repo is read at ───────────────────────────────────────────────────────

/// The commit per repo, and the repos whose scan and audit recorded different commits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShaChoice {
    pub shas: HashMap<String, String>,
    /// Repos where the two runs disagree. They are read at the scan's commit and the viewer says
    /// so.
    pub conflicts: HashSet<String>,
}

/// The note shown when a repo's two runs recorded different commits.
pub const SHA_CONFLICT_NOTE: &str =
    "The scan and the audit ran at different commits; line numbers may differ";

/// Merge the `(repo, sha)` pairs the deterministic scan recorded with those the audit recorded.
/// A finding carries no origin, so which run produced it cannot be told. Where only one run
/// recorded a commit that one is used. Where both did and they agree, that is the commit. Where
/// they differ the scan's wins and the repo is listed in `conflicts`: never a silent pick.
pub fn merge_audited_shas(
    scan: &[(String, Option<String>)],
    audit: &[(String, Option<String>)],
) -> ShaChoice {
    let known = |list: &[(String, Option<String>)]| -> HashMap<String, String> {
        list.iter()
            .filter_map(|(repo, sha)| {
                sha.as_ref()
                    .filter(|s| !s.is_empty())
                    .map(|s| (repo.clone(), s.clone()))
            })
            .collect()
    };
    let (scan, audit) = (known(scan), known(audit));
    let mut out = ShaChoice::default();
    for (repo, sha) in &audit {
        out.shas.insert(repo.clone(), sha.clone());
    }
    for (repo, sha) in &scan {
        if audit.get(repo).is_some_and(|a| a != sha) {
            out.conflicts.insert(repo.clone());
        }
        out.shas.insert(repo.clone(), sha.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::triage::TriageState::{Confirmed, FalsePositive, Ignored, TechDebt};

    fn none() -> KeyMods {
        KeyMods::none()
    }

    fn mods(ctrl: bool, alt: bool, meta: bool, shift: bool) -> KeyMods {
        KeyMods {
            ctrl,
            alt,
            meta,
            shift,
        }
    }

    // ── wire shape ──

    fn parse(json: serde_json::Value) -> SourceFileView {
        serde_json::from_value(json).expect("parses")
    }

    fn ok_json() -> serde_json::Value {
        serde_json::json!({
            "status": "ok", "source": "git", "commit": "a".repeat(40), "head_differs": false,
            "language": "rust", "total_lines": 3,
            "window": {"from": 1, "to": 3, "total_lines": 3},
            "highlight": "ok", "classes": ["keyword", "string"],
            "block": {"start_line": 1, "end_line": 3, "kind": "function"},
            "lines": [[[0, "fn"], [null, " f() {"]], [[null, "  let s = "], [1, "\"x\""]], [[null, "}"]]]
        })
    }

    #[test]
    fn the_response_shape_of_the_server_parses() {
        let v = parse(ok_json());
        assert_eq!(v.outcome(), SourceOutcome::Code);
        assert_eq!(v.total(), 3);
        assert_eq!(v.line(1).unwrap()[0], (Some(0), "fn".to_string()));
        assert_eq!(v.line(2).unwrap()[0], (None, "  let s = ".to_string()));
        assert!(v.line(4).is_none());
        assert_eq!(v.block.as_ref().unwrap().kind, "function");
        assert_eq!(v.short_commit(), Some("aaaaaaa"));
        assert!(v.banner().is_none(), "a clean git read has no banner");
        assert!(v.is_cacheable() && v.is_whole());
    }

    #[test]
    fn the_amber_banner_keys_on_source_not_on_status_alone() {
        // Inspected commit unavailable: the response's own reason.
        let mut j = ok_json();
        j["status"] = "commit_unavailable".into();
        j["source"] = "working_tree".into();
        j["reason"] =
            "Inspected commit not available in this clone; showing the working tree".into();
        let v = parse(j);
        assert_eq!(v.outcome(), SourceOutcome::Code);
        let b = v.banner().unwrap();
        assert_eq!(b.kind, BannerKind::Amber);
        assert_eq!(
            b.text,
            "Inspected commit not available in this clone; showing the working tree"
        );
        assert!(!v.is_cacheable());

        // No commit was recorded: status is plain `ok` but the source is the working tree.
        let mut j = ok_json();
        j["source"] = "working_tree".into();
        j["commit"] = "".into();
        let v = parse(j);
        assert_eq!(v.banner().unwrap().kind, BannerKind::Amber);
        assert!(!v.is_cacheable());

        // A git read has no amber banner, whatever else the status says.
        let mut j = ok_json();
        j["head_differs"] = true.into();
        let b = parse(j).banner().unwrap();
        assert_eq!(b.kind, BannerKind::Muted);
        assert!(b.text.contains("aaaaaaa"));
    }

    #[test]
    fn a_deleted_source_says_so_and_shows_no_code() {
        let v = parse(serde_json::json!({
            "status": "source_deleted", "source": "git",
            "reason": "Source deleted on 24 Oct 2026, re-fetch to view",
            "deleted_at": "2026-10-24T17:40:19Z", "record_id": "r1"
        }));
        assert_eq!(
            v.outcome(),
            SourceOutcome::SourceDeleted("Source deleted on 24 Oct 2026, re-fetch to view".into())
        );
        assert!(v.banner().is_none());
        // Without a reason the shared wording is built from `deleted_at`.
        let v = parse(serde_json::json!({
            "status": "source_deleted", "deleted_at": "2026-10-24T17:40:19Z"
        }));
        assert_eq!(
            v.outcome(),
            SourceOutcome::SourceDeleted("Source deleted on 24 Oct 2026, re-fetch to view".into())
        );
    }

    #[test]
    fn every_refusal_maps_to_its_own_outcome() {
        let o = |status: &str, reason: &str| {
            parse(serde_json::json!({"status": status, "reason": reason})).outcome()
        };
        assert_eq!(o("too_large", "big"), SourceOutcome::TooLarge("big".into()));
        assert_eq!(o("not_utf8", "bin"), SourceOutcome::NotUtf8("bin".into()));
        assert_eq!(
            o("file_missing", "gone"),
            SourceOutcome::FileMissing("gone".into())
        );
        assert_eq!(
            o("path_traversal", "no"),
            SourceOutcome::Unavailable("no".into())
        );
        assert!(matches!(o("what", ""), SourceOutcome::Unavailable(t) if !t.is_empty()));
        // A commit_unavailable answer with no lines (the working tree file is gone too).
        assert!(matches!(
            o("commit_unavailable", "x"),
            SourceOutcome::Unavailable(_)
        ));
    }

    // ── cache ──

    fn key(path: &str) -> FileKey {
        FileKey {
            repo: "o/r".into(),
            sha: "a".repeat(40),
            path: path.into(),
        }
    }

    fn cached() -> Arc<SourceFileView> {
        Arc::new(parse(ok_json()))
    }

    #[test]
    fn the_cache_keeps_the_last_eight_files_and_evicts_the_least_recently_used() {
        let mut c = ViewerCache::new();
        for i in 0..8 {
            assert!(c.put(key(&format!("f{i}")), cached()));
        }
        assert_eq!(c.len(), CACHE_CAPACITY);
        // Touch f0 so f1 becomes the oldest.
        assert!(c.get(&key("f0"), 1).is_some());
        assert!(c.put(key("f8"), cached()));
        assert_eq!(c.len(), 8);
        assert!(
            c.lookup(&key("f1"), 1).is_none(),
            "f1 was least recently used"
        );
        assert!(c.lookup(&key("f0"), 1).is_some());
        assert!(c.lookup(&key("f8"), 1).is_some());
        assert_eq!(c.keys().last(), Some(&key("f8")));
    }

    #[test]
    fn the_cache_refuses_what_must_not_be_kept() {
        let mut c = ViewerCache::new();
        // No recorded commit.
        let mut k = key("a");
        k.sha.clear();
        assert!(!c.put(k, cached()));
        // The working tree.
        let mut j = ok_json();
        j["source"] = "working_tree".into();
        assert!(!c.put(key("b"), Arc::new(parse(j))));
        // A refusal.
        assert!(!c.put(
            key("c"),
            Arc::new(parse(serde_json::json!({"status": "too_large"})))
        ));
        assert!(c.is_empty());
    }

    // ── the pane's context lane ──

    fn ctx_key(path: &str, line: usize) -> ContextKey {
        ContextKey {
            repo: "o/r".into(),
            sha: "a".repeat(40),
            path: path.into(),
            line,
        }
    }

    fn window(first: usize) -> Arc<PaneContext> {
        Arc::new(PaneContext {
            start_line: first,
            violation_line: first + 1,
            lines: vec!["a".into(), "b".into(), "c".into()],
        })
    }

    #[test]
    fn a_pane_window_is_keyed_by_its_line_and_never_touches_the_file_lane() {
        let mut c = ViewerCache::new();
        assert!(c.put(key("f"), cached()));
        assert!(c.put_context(ctx_key("f", 5), window(4)));
        assert!(c.put_context(ctx_key("f", 9), window(8)));
        // The two windows of one file are two entries, and neither is a file.
        assert_eq!(c.context_len(), 2);
        assert_eq!(c.len(), 1);
        assert_eq!(c.context(&ctx_key("f", 5), None), Some(window(4)));
        assert_eq!(c.context(&ctx_key("f", 9), None), Some(window(8)));
        // The file lane still answers for the file as before.
        assert!(c.lookup(&key("f"), 1).is_some());
        // A window kept for one line does not answer for another.
        assert_eq!(c.context(&ctx_key("f", 6), None), None);
    }

    #[test]
    fn pane_windows_have_their_own_capacity_and_never_evict_a_file() {
        let mut c = ViewerCache::new();
        for i in 0..CACHE_CAPACITY {
            assert!(c.put(key(&format!("f{i}")), cached()));
        }
        for line in 1..=CONTEXT_CAPACITY + 10 {
            assert!(c.put_context(ctx_key("g", line), window(line)));
        }
        assert_eq!(c.context_len(), CONTEXT_CAPACITY);
        assert_eq!(c.len(), CACHE_CAPACITY, "windows never push a file out");
        assert_eq!(
            c.context(&ctx_key("g", 1), None),
            None,
            "the oldest went first"
        );
        assert!(c
            .context(&ctx_key("g", CONTEXT_CAPACITY + 10), None)
            .is_some());
        // And files never push a window out.
        for i in 100..120 {
            c.put(key(&format!("f{i}")), cached());
        }
        assert_eq!(c.context_len(), CONTEXT_CAPACITY);
    }

    #[test]
    fn a_window_with_no_commit_is_not_kept_or_answered() {
        let mut c = ViewerCache::new();
        let mut k = ctx_key("f", 5);
        k.sha.clear();
        assert!(!c.put_context(k.clone(), window(4)));
        assert_eq!(c.context(&k, None), None);
        assert_eq!(c.context_len(), 0);
    }

    /// The viewer and the pane share what has been read: a window can be cut from a whole file
    /// the viewer holds, and it is the lines of THAT file at THAT line, never its block.
    #[test]
    fn a_held_whole_file_answers_for_the_pane_with_the_right_lines() {
        let mut c = ViewerCache::new();
        let mut file = parse(ok_json());
        file.block = Some(BlockView {
            start_line: 1,
            end_line: 3,
            kind: "function".into(),
        });
        let total = file.total();
        assert!(c.put(key("f"), Arc::new(file)));
        assert!(total >= 1);
        let got = c
            .context(&ctx_key("f", 1), None)
            .expect("cut from the held file");
        assert_eq!(got.violation_line, 1);
        assert_eq!(got.start_line, 1);
        // Asking again for another line of the same file cuts that line's window.
        if total >= 2 {
            let second = c.context(&ctx_key("f", 2), None).expect("second line");
            assert_eq!(second.violation_line, 2);
        }
        // A file that is not held yields nothing.
        assert_eq!(c.context(&ctx_key("other", 1), None), None);
        // Nothing was written to the pane lane by reading.
        assert_eq!(c.context_len(), 0);
    }

    #[test]
    fn a_windowed_entry_only_answers_lines_it_carries() {
        let mut j = ok_json();
        j["total_lines"] = 5000.into();
        j["window"] = serde_json::json!({"from": 100, "to": 102, "total_lines": 5000});
        let mut c = ViewerCache::new();
        assert!(c.put(key("big"), Arc::new(parse(j))));
        assert!(c.lookup(&key("big"), 101).is_some());
        assert!(c.lookup(&key("big"), 3000).is_none());
        assert!(c.get(&key("big"), 3000).is_none());
        // A put of the same file at another window replaces it.
        assert!(c.put(key("big"), cached()));
        assert_eq!(c.len(), 1);
    }

    // ── long lines ──

    #[test]
    fn a_line_over_two_thousand_chars_is_cut_with_a_count() {
        let long = "x".repeat(2500);
        let segs = vec![(Some(0), "ab".to_string()), (None, long)];
        let (cut, more) = cut_line(&segs, MAX_LINE_CHARS).unwrap();
        assert_eq!(more, 502);
        let kept: usize = cut.iter().map(|(_, t)| t.chars().count()).sum();
        assert_eq!(kept, 2000);
        assert_eq!(cut[0], (Some(0), "ab".to_string()));
        assert_eq!(more_chars_label(more), "... (502 more chars)");
        // Exactly the cap fits.
        assert!(cut_line(&[(None, "y".repeat(2000))], MAX_LINE_CHARS).is_none());
        // Segments after the cut point are dropped whole.
        let segs = vec![
            (None, "a".repeat(1999)),
            (None, "bcd".into()),
            (None, "ef".into()),
        ];
        let (cut, more) = cut_line(&segs, 2000).unwrap();
        assert_eq!((cut.len(), more), (2, 4));
    }

    #[test]
    fn the_cut_never_splits_a_character() {
        let segs = vec![(None, "é".repeat(3000))];
        let (cut, more) = cut_line(&segs, 2000).unwrap();
        assert_eq!(cut[0].1.chars().count(), 2000);
        assert_eq!(more, 1000);
        // Multi-byte text over the byte count but under the char count is not cut.
        assert!(cut_line(&[(None, "é".repeat(1500))], 2000).is_none());
    }

    // ── virtual window ──

    #[test]
    fn the_virtual_range_is_the_viewport_plus_overscan_and_clamped() {
        let h = CODE_LINE_H_PX;
        // At the top of a long file.
        let r = virtual_range(25_000, 0.0, 900.0, h, OVERSCAN_LINES);
        assert_eq!(r.first, 1);
        assert_eq!(r.last, 46 + OVERSCAN_LINES);
        // In the middle: 150 lines above and below the 45 visible, top snapped to 25.
        let r = virtual_range(25_000, 12_000.0 * 20.0, 900.0, h, OVERSCAN_LINES);
        assert!(r.first <= 12_001 - OVERSCAN_LINES && r.first > 12_001 - OVERSCAN_LINES - 25);
        assert!(r.last >= 12_001 + 45 + OVERSCAN_LINES - 1);
        // Past the end: clamped.
        let r = virtual_range(100, 1_000_000.0, 900.0, h, OVERSCAN_LINES);
        assert_eq!(r.last, 100);
        assert!(r.first >= 1);
        assert_eq!(virtual_range(0, 0.0, 900.0, h, 150).len(), 0);
    }

    #[test]
    fn a_25000_line_file_never_renders_500_lines() {
        // Any scroll position, any realistic viewport.
        for viewport in [400.0, 900.0, 1400.0, 2000.0] {
            let mut top = 0.0;
            while top <= 25_000.0 * 20.0 + 5_000.0 {
                let r = virtual_range(25_000, top, viewport, CODE_LINE_H_PX, OVERSCAN_LINES);
                assert!(r.len() < 500, "{} lines at {top}/{viewport}", r.len());
                assert!(!r.is_empty());
                top += 777.0;
            }
        }
    }

    #[test]
    fn spacers_make_the_scroll_height_the_whole_file() {
        let h = CODE_LINE_H_PX;
        let r = virtual_range(25_000, 100_000.0, 900.0, h, OVERSCAN_LINES);
        let (top, bottom) = spacers(25_000, r, h);
        assert_eq!(
            top + r.len() as u64 * u64::from(h) + bottom,
            25_000 * u64::from(h)
        );
        let empty = VirtualRange { first: 5, last: 4 };
        assert_eq!(spacers(10, empty, h), (10 * u64::from(h), 0));
        // Clamped to a window: lines outside it are blank space.
        let w = r.within(r.first + 10, r.last - 10);
        let (top, bottom) = spacers(25_000, w, h);
        assert_eq!(
            top + w.len() as u64 * u64::from(h) + bottom,
            25_000 * u64::from(h)
        );
    }

    #[test]
    fn the_violation_line_sits_in_the_upper_third() {
        let h = CODE_LINE_H_PX;
        // Line 500, 900px viewport: the line's top is 300px below the viewport's top.
        let top = scroll_top_for_line(500, h, 900.0, 10_000);
        assert!((499.0 * 20.0 - top - 300.0).abs() < f64::EPSILON);
        // Near the top and the bottom it is clamped.
        assert!((scroll_top_for_line(3, h, 900.0, 10_000)).abs() < f64::EPSILON);
        let end = scroll_top_for_line(10_000, h, 900.0, 10_000);
        assert!((end - (10_000.0 * 20.0 - 900.0)).abs() < f64::EPSILON);
        // A file shorter than the viewport never scrolls.
        assert!((scroll_top_for_line(30, h, 900.0, 40)).abs() < f64::EPSILON);
        assert_eq!(line_at_scroll(0.0, h), 1);
        assert_eq!(line_at_scroll(41.0, h), 3);
    }

    #[test]
    fn a_window_asks_for_a_new_page_near_its_edge_only() {
        let w = WindowView {
            from: 1000,
            to: 1799,
            total_lines: 10_000,
        };
        let h = CODE_LINE_H_PX;
        // Well inside: no request.
        assert_eq!(page_request(&w, 1400.0 * 20.0, 900.0, h), None);
        // Close to the end of the window: centre on what is on screen.
        let near_end = page_request(&w, 1740.0 * 20.0, 900.0, h).unwrap();
        assert!((1740..=1790).contains(&near_end), "{near_end}");
        // Close to the start.
        assert!(page_request(&w, 1010.0 * 20.0, 900.0, h).is_some());
        // The first window of a file has no start to page to.
        let first = WindowView {
            from: 1,
            to: 800,
            total_lines: 10_000,
        };
        assert_eq!(page_request(&first, 0.0, 900.0, h), None);
        // The last window has no end to page to.
        let last = WindowView {
            from: 9201,
            to: 10_000,
            total_lines: 10_000,
        };
        assert_eq!(page_request(&last, 9_990.0 * 20.0, 900.0, h), None);
        // A whole file never pages.
        let whole = WindowView {
            from: 1,
            to: 300,
            total_lines: 300,
        };
        assert_eq!(page_request(&whole, 100.0 * 20.0, 900.0, h), None);
    }

    #[test]
    fn scroll_keys_move_the_viewport_and_clamp() {
        let h = CODE_LINE_H_PX;
        let total = 1000;
        let down = scrolled(0.0, ScrollKey::PageDown, 900.0, total, h);
        assert!(
            (down - 860.0).abs() < f64::EPSILON,
            "a page is the viewport less two lines"
        );
        assert!((scrolled(down, ScrollKey::PageUp, 900.0, total, h)).abs() < f64::EPSILON);
        assert!((scrolled(5.0, ScrollKey::PageUp, 900.0, total, h)).abs() < f64::EPSILON);
        assert!(
            (scrolled(0.0, ScrollKey::End, 900.0, total, h) - (1000.0 * 20.0 - 900.0)).abs()
                < f64::EPSILON
        );
        assert!((scrolled(500.0, ScrollKey::Home, 900.0, total, h)).abs() < f64::EPSILON);
        let max = 1000.0 * 20.0 - 900.0;
        assert!(
            (scrolled(max - 10.0, ScrollKey::PageDown, 900.0, total, h) - max).abs() < f64::EPSILON
        );
    }

    // ── the key map ──

    #[test]
    fn viewer_keys_map_to_their_commands() {
        use ViewerCommand::*;
        let cases: &[(&str, ViewerCommand)] = &[
            ("Escape", Close),
            ("c", Dispose(Confirmed)),
            ("C", Dispose(Confirmed)),
            ("t", Dispose(TechDebt)),
            ("i", PromptFor(Ignored)),
            ("F", PromptFor(FalsePositive)),
            ("ArrowDown", MoveRow(FocusMove::Down)),
            ("ArrowUp", MoveRow(FocusMove::Up)),
            ("j", MoveRow(FocusMove::Down)),
            ("K", MoveRow(FocusMove::Up)),
            ("]", FindingInFile(FocusMove::Down)),
            ("[", FindingInFile(FocusMove::Up)),
            ("}", FileInGroup(FocusMove::Down)),
            ("{", FileInGroup(FocusMove::Up)),
            (".", BackToFinding),
            ("g", OpenGoToLine),
            ("G", OpenGoToLine),
            ("Backspace", Back),
            ("PageUp", Scroll(ScrollKey::PageUp)),
            ("PageDown", Scroll(ScrollKey::PageDown)),
            ("Home", Scroll(ScrollKey::Home)),
            ("End", Scroll(ScrollKey::End)),
            ("?", ToggleHelp),
            ("/", OpenSearch),
            ("s", OpenSymbols),
            ("S", OpenSymbols),
            ("y", CopyPermalink),
            ("Y", CopyPermalink),
            (" ", TogglePopover),
            ("h", OpenHub),
            ("H", OpenHub),
        ];
        for (key, want) in cases {
            assert_eq!(
                viewer_command(ViewerFocus::Code, key, none()),
                Some(*want),
                "{key}"
            );
        }
        // Shift is how braces and capitals are typed; it does not cancel a key.
        assert_eq!(
            viewer_command(ViewerFocus::Code, "}", mods(false, false, false, true)),
            Some(FileInGroup(FocusMove::Down))
        );
        // Alt+ArrowLeft is Back.
        assert_eq!(
            viewer_command(
                ViewerFocus::Code,
                "ArrowLeft",
                mods(false, true, false, false)
            ),
            Some(Back)
        );
    }

    #[test]
    fn the_dispositions_in_the_viewer_are_the_panes() {
        // Same states, same prompt-or-apply split, for every hotkey the pane knows.
        for state in TriageState::all() {
            let Some(hot) = crate::triage::state_hotkey(state) else {
                continue;
            };
            let pane = pane_command(PaneFocus::Cursor, hot, none());
            let viewer = viewer_command(ViewerFocus::Code, hot, none());
            match (pane, viewer) {
                (Some(PaneCommand::Dispose(a)), Some(ViewerCommand::Dispose(b)))
                | (Some(PaneCommand::PromptFor(a)), Some(ViewerCommand::PromptFor(b))) => {
                    assert_eq!(a, b);
                }
                other => panic!("{hot}: {other:?}"),
            }
        }
    }

    #[test]
    fn every_ctrl_alt_or_meta_press_is_refused_except_alt_arrow_left() {
        let universe = key_universe();
        for key in &universe {
            for m in [
                mods(true, false, false, false),
                mods(true, false, false, true),
                mods(false, false, true, false),
                mods(false, false, true, true),
                mods(true, true, true, false),
                mods(false, true, false, false),
                mods(false, true, false, true),
                mods(true, true, false, false),
            ] {
                let got = viewer_command(ViewerFocus::Code, key, m);
                let allowed = key == "ArrowLeft" && m.alt && !m.ctrl && !m.meta && !m.shift;
                if allowed {
                    assert_eq!(got, Some(ViewerCommand::Back));
                } else {
                    assert_eq!(
                        got, None,
                        "{key} with {m:?} must pass through to the webview"
                    );
                }
            }
        }
    }

    #[test]
    fn open_prompts_disarm_every_key() {
        let universe = key_universe();
        for focus in [
            ViewerFocus::SearchField,
            ViewerFocus::GoToLine,
            ViewerFocus::SymbolPrompt,
            ViewerFocus::ReasonPrompt,
            ViewerFocus::RuleHub,
        ] {
            assert!(!viewer_keys_armed(focus));
            for key in &universe {
                for shift in [false, true] {
                    assert_eq!(
                        viewer_command(focus, key, mods(false, false, false, shift)),
                        None,
                        "{key} under {focus:?} must be text"
                    );
                }
            }
        }
        // Typing a sentence into the reason prompt fires nothing.
        for ch in "cannot reproduce; test-only ] [ . ?".chars() {
            assert_eq!(
                viewer_command(ViewerFocus::ReasonPrompt, &ch.to_string(), none()),
                None
            );
        }
        assert!(viewer_keys_armed(ViewerFocus::Code));
        // Typing a search needle or a symbol fires nothing either.
        for focus in [ViewerFocus::SearchField, ViewerFocus::SymbolPrompt] {
            for ch in "/syh .]}[{gG?cCtTiIfF-_".chars() {
                assert_eq!(viewer_command(focus, &ch.to_string(), none()), None);
            }
        }
    }

    #[test]
    fn only_the_code_and_the_references_list_arm_commands() {
        let all = [
            ViewerFocus::Code,
            ViewerFocus::SearchField,
            ViewerFocus::GoToLine,
            ViewerFocus::SymbolPrompt,
            ViewerFocus::ReasonPrompt,
            ViewerFocus::RefsList,
            ViewerFocus::RuleHub,
        ];
        let universe = key_universe();
        for focus in all {
            let answers = universe
                .iter()
                .any(|k| viewer_command(focus, k, none()).is_some());
            let arms = matches!(focus, ViewerFocus::Code | ViewerFocus::RefsList);
            assert_eq!(answers, arms, "{focus:?}");
            assert_eq!(viewer_keys_armed(focus), arms, "{focus:?}");
        }
        assert!(viewer_code_keys_armed(ViewerFocus::Code));
        assert!(!viewer_code_keys_armed(ViewerFocus::RefsList));
    }

    #[test]
    fn the_references_list_answers_exactly_its_own_keys() {
        use ViewerCommand::*;
        let list = ViewerFocus::RefsList;
        let cases: &[(&str, ViewerCommand)] = &[
            ("ArrowDown", RefsMove(FocusMove::Down)),
            ("ArrowUp", RefsMove(FocusMove::Up)),
            ("j", RefsMove(FocusMove::Down)),
            ("K", RefsMove(FocusMove::Up)),
            ("Enter", RefsOpen),
            ("Escape", RefsClose),
        ];
        for (key, want) in cases {
            assert_eq!(viewer_command(list, key, none()), Some(*want), "{key}");
        }
        // Nothing else: a decision key cannot act while a hit is being chosen.
        let mut answered: Vec<String> = key_universe()
            .into_iter()
            .filter(|k| viewer_command(list, k, none()).is_some())
            .collect();
        answered.sort();
        let mut want: Vec<String> = [
            "ArrowDown",
            "ArrowUp",
            "j",
            "J",
            "k",
            "K",
            "Enter",
            "Escape",
        ]
        .iter()
        .map(|k| (*k).to_string())
        .collect();
        want.sort();
        assert_eq!(answered, want);
        // Held modifiers pass through.
        for key in ["ArrowDown", "Enter", "Escape", "j"] {
            for m in [
                mods(true, false, false, false),
                mods(false, true, false, false),
                mods(false, false, true, false),
            ] {
                assert_eq!(viewer_command(list, key, m), None, "{key} {m:?}");
            }
        }
    }

    #[test]
    fn the_focus_follows_the_surfaces_that_are_open() {
        let open = |f: fn(&mut OpenSurfaces)| {
            let mut s = OpenSurfaces::default();
            f(&mut s);
            viewer_focus(s)
        };
        assert_eq!(viewer_focus(OpenSurfaces::default()), ViewerFocus::Code);
        assert_eq!(open(|s| s.reason_prompt = true), ViewerFocus::ReasonPrompt);
        assert_eq!(open(|s| s.goto = true), ViewerFocus::GoToLine);
        assert_eq!(open(|s| s.search = true), ViewerFocus::SearchField);
        assert_eq!(open(|s| s.symbol_prompt = true), ViewerFocus::SymbolPrompt);
        assert_eq!(open(|s| s.refs_list = true), ViewerFocus::RefsList);
        assert_eq!(open(|s| s.rule_hub = true), ViewerFocus::RuleHub);
        // The hub sits above everything; a prompt beats the list beneath it.
        let all = OpenSurfaces {
            reason_prompt: true,
            goto: true,
            search: true,
            symbol_prompt: true,
            refs_list: true,
            rule_hub: true,
        };
        assert_eq!(viewer_focus(all), ViewerFocus::RuleHub);
        assert_eq!(
            viewer_focus(OpenSurfaces {
                rule_hub: false,
                ..all
            }),
            ViewerFocus::ReasonPrompt
        );
        assert_eq!(
            viewer_focus(OpenSurfaces {
                refs_list: true,
                search: true,
                ..OpenSurfaces::default()
            }),
            ViewerFocus::SearchField
        );
    }

    #[test]
    fn the_hub_hands_the_keyboard_back_to_whoever_opened_it() {
        assert_eq!(hub_close_focus(true), FocusReturn::Viewer);
        assert_eq!(hub_close_focus(false), FocusReturn::Pane);
    }

    #[test]
    fn escape_closes_the_hub_and_nothing_else_does() {
        let esc = |m| closes_hub(ViewerFocus::RuleHub, "Escape", m);
        assert!(esc(none()));
        assert!(esc(mods(false, false, false, true)));
        assert!(!esc(mods(true, false, false, false)));
        assert!(!esc(mods(false, true, false, false)));
        // Only while the hub is the focus, and only Escape.
        for focus in [
            ViewerFocus::Code,
            ViewerFocus::RefsList,
            ViewerFocus::SearchField,
            ViewerFocus::ReasonPrompt,
        ] {
            assert!(!closes_hub(focus, "Escape", none()), "{focus:?}");
        }
        for key in key_universe().into_iter().filter(|k| k != "Escape") {
            assert!(!closes_hub(ViewerFocus::RuleHub, &key, none()), "{key}");
        }
    }

    #[test]
    fn a_decision_key_does_not_act_on_code_that_is_not_on_screen() {
        use ViewerCommand::*;
        assert!(!allowed_in_reference_view(Dispose(Confirmed)));
        assert!(!allowed_in_reference_view(PromptFor(Ignored)));
        for cmd in [
            Close,
            MoveRow(FocusMove::Down),
            FindingInFile(FocusMove::Up),
            FileInGroup(FocusMove::Down),
            BackToFinding,
            OpenGoToLine,
            Back,
            Scroll(ScrollKey::End),
            ToggleHelp,
            OpenSearch,
            OpenSymbols,
            CopyPermalink,
            TogglePopover,
            OpenHub,
        ] {
            assert!(allowed_in_reference_view(cmd), "{cmd:?}");
        }
    }

    #[test]
    fn search_highlights_survive_scrolling_and_end_at_the_next_navigation() {
        use ViewerCommand::*;
        for cmd in [
            MoveRow(FocusMove::Down),
            FindingInFile(FocusMove::Down),
            FileInGroup(FocusMove::Up),
            BackToFinding,
            OpenGoToLine,
            Back,
            RefsOpen,
        ] {
            assert!(clears_search(cmd), "{cmd:?}");
        }
        for cmd in [
            Scroll(ScrollKey::PageDown),
            ToggleHelp,
            TogglePopover,
            OpenSearch,
            OpenSymbols,
            CopyPermalink,
            Dispose(Confirmed),
            Close,
        ] {
            assert!(!clears_search(cmd), "{cmd:?}");
        }
    }

    /// Every key a keyboard can send that this test knows about: all printable ASCII plus the
    /// named keys.
    fn key_universe() -> Vec<String> {
        let mut keys: Vec<String> = (0x20u8..=0x7e).map(|b| (b as char).to_string()).collect();
        for named in [
            "Escape",
            "Enter",
            "Tab",
            "Backspace",
            "Delete",
            "Insert",
            "ArrowUp",
            "ArrowDown",
            "ArrowLeft",
            "ArrowRight",
            "PageUp",
            "PageDown",
            "Home",
            "End",
            "Shift",
            "Control",
            "Alt",
            "Meta",
            "CapsLock",
            "F1",
            "F5",
            "F12",
        ] {
            keys.push(named.to_string());
        }
        keys
    }

    fn normalise(key: &str) -> String {
        if key.chars().count() == 1 {
            key.to_ascii_lowercase()
        } else {
            key.to_string()
        }
    }

    #[test]
    fn the_pane_and_the_viewer_share_only_the_listed_keys() {
        let mut shared: Vec<String> = Vec::new();
        for key in key_universe() {
            let pane = pane_command(PaneFocus::Cursor, &key, none()).is_some();
            let viewer = viewer_command(ViewerFocus::Code, &key, none()).is_some();
            if pane && viewer {
                shared.push(normalise(&key));
            }
        }
        shared.sort();
        shared.dedup();
        let mut want: Vec<String> = SHARED_KEYS.iter().map(|k| (*k).to_string()).collect();
        want.sort();
        assert_eq!(
            shared, want,
            "a key was added to one map that the other already has"
        );
        // The pane's `O` is the pane's alone.
        assert!(pane_command(PaneFocus::Cursor, "o", none()).is_some());
        assert!(viewer_command(ViewerFocus::Code, "o", none()).is_none());
    }

    #[test]
    fn the_key_list_is_exactly_the_keys_the_map_answers() {
        // Every listed press works...
        let mut listed: Vec<String> = Vec::new();
        for row in &KEY_ROWS {
            assert!(!row.label.is_empty() && !row.action.is_empty());
            for (key, alt) in row.probes {
                let m = mods(false, *alt, false, false);
                assert!(
                    viewer_command(ViewerFocus::Code, key, m).is_some(),
                    "{} lists {key} but the map ignores it",
                    row.label
                );
                if !alt {
                    listed.push(normalise(key));
                }
            }
        }
        // ...and nothing works that is not listed.
        let mut answered: Vec<String> = key_universe()
            .into_iter()
            .filter(|k| viewer_command(ViewerFocus::Code, k, none()).is_some())
            .map(|k| normalise(&k))
            .collect();
        answered.sort();
        answered.dedup();
        listed.sort();
        listed.dedup();
        assert_eq!(answered, listed);
    }

    #[test]
    fn keys_the_order_deliberately_leaves_unbound_stay_unbound() {
        // A.8: no N/P (ambiguous between finding and file), no U (an undo must not be a stray
        // key), no Enter on the code (native text selection is never fought).
        for key in ["n", "N", "p", "P", "u", "U", "Enter"] {
            assert_eq!(
                viewer_command(ViewerFocus::Code, key, none()),
                None,
                "{key} must stay unbound"
            );
        }
    }

    #[test]
    fn the_new_keys_are_the_viewers_alone() {
        for key in ["/", "s", "S", "y", "Y", " ", "h", "H"] {
            assert_eq!(pane_command(PaneFocus::Cursor, key, none()), None, "{key}");
            assert!(
                viewer_command(ViewerFocus::Code, key, none()).is_some(),
                "{key}"
            );
            // A held modifier leaves them to the webview (Cmd-F, Cmd-S, Ctrl-Y...).
            for m in [
                mods(true, false, false, false),
                mods(false, false, true, false),
                mods(false, true, false, false),
            ] {
                assert_eq!(
                    viewer_command(ViewerFocus::Code, key, m),
                    None,
                    "{key} {m:?}"
                );
            }
        }
    }

    // ── findings in a file ──

    fn finding(rule: &str, path: &str, line: usize, severity: &str) -> FindingView {
        serde_json::from_value(serde_json::json!({
            "repo": "o/r", "path": path, "line": line, "rule_id": rule, "severity": severity,
            "snippet": format!("{rule}@{line}"), "detail": "d"
        }))
        .expect("finding")
    }

    fn decide(map: &mut HashMap<String, Disposition>, f: &FindingView, state: TriageState) {
        map.insert(
            finding_key(f),
            Disposition {
                state,
                ..Disposition::default()
            },
        );
    }

    fn four() -> (Vec<FindingView>, HashMap<String, Disposition>) {
        let fs = vec![
            finding("R-B", "a.rs", 40, "low"),
            finding("R-A", "a.rs", 10, "critical"),
            finding("R-C", "a.rs", 25, "high"),
            finding("R-D", "a.rs", 90, "medium"),
            finding("R-A", "other.rs", 5, "high"),
        ];
        let mut d = HashMap::new();
        decide(&mut d, &fs[2], Confirmed);
        decide(&mut d, &fs[3], Ignored);
        (fs, d)
    }

    fn navigable(fs: &[FindingView], d: &HashMap<String, Disposition>) -> HashSet<String> {
        // The Unresolved table holds the undecided rows.
        fs.iter()
            .filter(|f| !finding_state(d, f).is_decided())
            .map(finding_key)
            .collect()
    }

    #[test]
    fn a_file_with_four_findings_two_decided_shows_four_markers_two_hollow() {
        let (fs, d) = four();
        let items = file_findings(&fs, &d, "o/r", "a.rs", &navigable(&fs, &d));
        assert_eq!(items.len(), 4, "the other file's finding is not listed");
        assert_eq!(
            items.iter().map(|i| i.line).collect::<Vec<_>>(),
            [10, 25, 40, 90]
        );
        let marks = gutter_marks(&items);
        assert_eq!(marks.len(), 4);
        assert_eq!(marks.iter().filter(|m| m.decided).count(), 2);
        assert_eq!(marks[0].severity, "critical");
        assert!(!marks[0].decided && marks[0].target.is_some());
        assert!(
            marks[1].decided && marks[1].target.is_none(),
            "a decided marker is not a stop"
        );
        assert!(marks[1].title.contains("Confirmed"));
    }

    #[test]
    fn findings_on_one_line_share_a_marker_with_a_count_and_the_worst_severity() {
        let fs = vec![
            finding("R-A", "a.rs", 7, "low"),
            finding("R-B", "a.rs", 7, "high"),
            finding("R-C", "a.rs", 9, "medium"),
        ];
        let d = HashMap::new();
        let items = file_findings(&fs, &d, "o/r", "a.rs", &navigable(&fs, &d));
        let marks = gutter_marks(&items);
        assert_eq!(marks.len(), 2);
        assert_eq!((marks[0].count, marks[0].severity.as_str()), (2, "high"));
        assert_eq!(marks[0].target.as_deref(), Some(items[0].key.as_str()));
        // Hollow only when every finding on the line is decided.
        let mut d = HashMap::new();
        decide(&mut d, &fs[0], Confirmed);
        let items = file_findings(&fs, &d, "o/r", "a.rs", &navigable(&fs, &d));
        assert!(!gutter_marks(&items)[0].decided);
        decide(&mut d, &fs[1], Confirmed);
        let items = file_findings(&fs, &d, "o/r", "a.rs", &navigable(&fs, &d));
        assert!(gutter_marks(&items)[0].decided);
    }

    #[test]
    fn right_bracket_visits_findings_in_line_order_and_stops_at_the_last() {
        let (fs, d) = four();
        let items = file_findings(&fs, &d, "o/r", "a.rs", &navigable(&fs, &d));
        let first = items[0].key.clone();
        // Decided findings (25, 90) are not stops: 10 -> 40 -> end.
        let FileStep::Move(second) = step_in_file(&items, &first, FocusMove::Down) else {
            panic!("expected a move");
        };
        assert_eq!(second, items[2].key);
        match step_in_file(&items, &second, FocusMove::Down) {
            FileStep::Stay(msg) => {
                assert_eq!(msg, "Last finding in this file. Press } for the next file.")
            }
            other => panic!("must not wrap: {other:?}"),
        }
        // And back.
        assert_eq!(
            step_in_file(&items, &second, FocusMove::Up),
            FileStep::Move(first.clone())
        );
        match step_in_file(&items, &first, FocusMove::Up) {
            FileStep::Stay(msg) => assert!(msg.starts_with("First finding in this file")),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            step_in_file(&items, "no-such-key", FocusMove::Down),
            FileStep::Stay(_)
        ));
    }

    #[test]
    fn same_line_findings_are_separate_stops() {
        let fs = vec![
            finding("R-A", "a.rs", 7, "low"),
            finding("R-B", "a.rs", 7, "low"),
        ];
        let d = HashMap::new();
        let items = file_findings(&fs, &d, "o/r", "a.rs", &navigable(&fs, &d));
        assert_eq!(
            step_in_file(&items, &items[0].key, FocusMove::Down),
            FileStep::Move(items[1].key.clone())
        );
    }

    #[test]
    fn brace_opens_the_next_file_in_the_group_on_its_first_undecided_finding() {
        let fs = vec![
            finding("R-A", "a.rs", 10, "high"),
            finding("R-A", "b.rs", 30, "high"),
            finding("R-A", "b.rs", 60, "high"),
            finding("R-A", "c.rs", 5, "high"),
            finding("R-Z", "z.rs", 1, "high"),
        ];
        let mut d = HashMap::new();
        decide(&mut d, &fs[1], Confirmed);
        // The table order is the slice order. b.rs line 30 is decided: the first UNDECIDED is 60.
        let step = step_file_in_group(&fs, &d, &fs[0], FocusMove::Down);
        assert_eq!(step, FileStep::Move(finding_key(&fs[2])));
        // Next from b.rs is c.rs; z.rs is another group and never appears.
        assert_eq!(
            step_file_in_group(&fs, &d, &fs[2], FocusMove::Down),
            FileStep::Move(finding_key(&fs[3]))
        );
        // End of the group: stays, says so, and does not wrap.
        assert_eq!(
            step_file_in_group(&fs, &d, &fs[3], FocusMove::Down),
            FileStep::Stay("Last file in this group.".to_string())
        );
        assert_eq!(
            step_file_in_group(&fs, &d, &fs[0], FocusMove::Up),
            FileStep::Stay("First file in this group.".to_string())
        );
        // All decided: falls back to the first finding of the file.
        decide(&mut d, &fs[2], Confirmed);
        assert_eq!(
            step_file_in_group(&fs, &d, &fs[0], FocusMove::Down),
            FileStep::Move(finding_key(&fs[1]))
        );
        assert_eq!(
            files_in_group(&fs, "R-A"),
            vec![
                ("o/r".to_string(), "a.rs".to_string()),
                ("o/r".to_string(), "b.rs".to_string()),
                ("o/r".to_string(), "c.rs".to_string())
            ]
        );
    }

    #[test]
    fn prefetch_targets_the_next_file_in_the_group() {
        let fs = vec![
            finding("R-A", "a.rs", 10, "high"),
            finding("R-A", "b.rs", 30, "high"),
        ];
        let d = HashMap::new();
        assert_eq!(
            prefetch_target(&fs, &d, &fs[0]),
            Some(("o/r".to_string(), "b.rs".to_string(), 30))
        );
        assert_eq!(prefetch_target(&fs, &d, &fs[1]), None);
    }

    #[test]
    fn after_a_decision_the_viewer_goes_to_the_next_undecided_finding_in_the_file() {
        let fs = vec![
            finding("R-A", "a.rs", 10, "high"),
            finding("R-B", "a.rs", 20, "high"),
            finding("R-C", "a.rs", 30, "high"),
        ];
        let d = HashMap::new();
        let items = file_findings(&fs, &d, "o/r", "a.rs", &navigable(&fs, &d));
        // Deciding the first goes to the second; deciding the second goes to the third.
        assert_eq!(
            advance_after_disposition(&items, &items[0].key),
            Some(items[1].key.clone())
        );
        assert_eq!(
            advance_after_disposition(&items, &items[1].key),
            Some(items[2].key.clone())
        );
        // Deciding the last wraps to the first still-undecided one above it, so the file is
        // finished before the viewer leaves it.
        assert_eq!(
            advance_after_disposition(&items, &items[2].key),
            Some(items[0].key.clone())
        );
        // Already-decided findings are skipped.
        let mut d = HashMap::new();
        decide(&mut d, &fs[1], Confirmed);
        let items = file_findings(&fs, &d, "o/r", "a.rs", &navigable(&fs, &d));
        assert_eq!(
            advance_after_disposition(&items, &items[0].key),
            Some(items[2].key.clone())
        );
        // The only undecided finding: nothing in this file, so the queue's own order decides.
        let one = vec![finding("R-A", "a.rs", 10, "high")];
        let items = file_findings(
            &one,
            &HashMap::new(),
            "o/r",
            "a.rs",
            &navigable(&one, &HashMap::new()),
        );
        assert_eq!(advance_after_disposition(&items, &items[0].key), None);
        assert_eq!(advance_after_disposition(&items, "missing"), None);
    }

    #[test]
    fn the_header_counts_the_finding_among_the_file_s() {
        let (fs, d) = four();
        let items = file_findings(&fs, &d, "o/r", "a.rs", &navigable(&fs, &d));
        assert_eq!(finding_position(&items, &items[2].key), (3, 4));
        assert_eq!(finding_position(&items, "x"), (0, 4));
        assert_eq!(line_label(12, 340), "line 12 of 340");
    }

    // ── go to line and history ──

    #[test]
    fn go_to_line_reads_a_number_and_refuses_the_rest() {
        assert_eq!(parse_goto_line("42", 100), Ok(42));
        assert_eq!(parse_goto_line("  L42 ", 100), Ok(42));
        assert_eq!(parse_goto_line("100", 100), Ok(100));
        for bad in ["", "0", "101", "abc", "-3", "4.5"] {
            assert_eq!(
                parse_goto_line(bad, 100),
                Err("Enter a line from 1 to 100".to_string()),
                "{bad:?}"
            );
        }
        assert!(parse_goto_line("1", 0).is_err());
    }

    #[test]
    fn the_history_is_capped_and_ignores_a_repeat() {
        let mut h = Vec::new();
        let e = |n: usize| HistoryEntry {
            key: "k".into(),
            path: "a.rs".into(),
            line: n,
        };
        push_history(&mut h, e(1));
        push_history(&mut h, e(1));
        assert_eq!(h.len(), 1);
        for n in 2..=(HISTORY_CAP + 10) {
            push_history(&mut h, e(n));
        }
        assert_eq!(h.len(), HISTORY_CAP);
        assert_eq!(h.last(), Some(&e(HISTORY_CAP + 10)));
        assert_eq!(h.first(), Some(&e(11)));
    }

    // ── one cursor ──

    #[test]
    fn every_navigation_lands_on_the_cursor_and_the_cursor_is_the_viewers_finding() {
        use crate::triage::RowCursor;
        let fs = vec![
            finding("R-A", "a.rs", 10, "high"),
            finding("R-A", "a.rs", 20, "high"),
            finding("R-A", "b.rs", 5, "high"),
        ];
        let visible: Vec<String> = fs.iter().map(finding_key).collect();
        let d = HashMap::new();
        let nav = navigable(&fs, &d);

        // `]` moves the cursor to the next finding in the file.
        let items = file_findings(&fs, &d, "o/r", "a.rs", &nav);
        let mut cursor = RowCursor::focused_on(&items[0].key).resolve(&visible);
        let FileStep::Move(k) = step_in_file(&items, cursor.key().unwrap(), FocusMove::Down) else {
            panic!()
        };
        cursor = RowCursor::focused_on(k).resolve(&visible);
        assert_eq!(cursor.key(), Some(finding_key(&fs[1]).as_str()));

        // `}` moves it to the next file's finding.
        let FileStep::Move(k) = step_file_in_group(&fs, &d, &fs[1], FocusMove::Down) else {
            panic!()
        };
        cursor = RowCursor::focused_on(k).resolve(&visible);
        assert_eq!(cursor.key(), Some(finding_key(&fs[2]).as_str()));

        // ArrowDown / J is the pane's own cursor move.
        let cursor = RowCursor::focused_on(finding_key(&fs[0])).resolve(&visible);
        let Some(ViewerCommand::MoveRow(dir)) = viewer_command(ViewerFocus::Code, "j", none())
        else {
            panic!()
        };
        assert_eq!(
            cursor.moved(&visible, dir).key(),
            Some(finding_key(&fs[1]).as_str())
        );
        // A gutter click is `focused_on(target)`.
        let marks = gutter_marks(&items);
        let target = marks[1].target.clone().unwrap();
        assert_eq!(
            RowCursor::focused_on(target).resolve(&visible).key(),
            Some(finding_key(&fs[1]).as_str())
        );
    }

    // ── block, session copy, back, close, dispose outcome, shas ──

    #[test]
    fn a_block_is_trusted_only_for_the_line_the_request_named() {
        let mut v = parse(ok_json());
        v.requested_around = Some(3);
        assert_eq!(block_for(&v, 3).unwrap().unwrap().kind, "function");
        // A cached file fetched for another finding's line, or a page centred elsewhere.
        assert_eq!(block_for(&v, 9), None);
        v.requested_around = None;
        assert_eq!(block_for(&v, 3), None);
        // A plain file has no block, and that is an answer.
        let mut plain = parse(ok_json());
        plain.block = None;
        plain.requested_around = Some(3);
        assert_eq!(block_for(&plain, 3), Some(None));
        assert_eq!(block_from_context(10, 5, "function").unwrap().end_line, 14);
        assert!(block_from_context(0, 5, "function").is_none());
        assert!(block_from_context(10, 0, "function").is_none());
    }

    #[test]
    fn the_session_copy_holds_one_answer_per_open_and_clears() {
        let mut s = SessionCopy::new();
        let mut j = ok_json();
        j["source"] = "working_tree".into();
        let view = Arc::new(parse(j));
        s.put(key("a.rs"), Arc::clone(&view));
        assert!(s.lookup(&key("a.rs"), 2).is_some());
        assert!(
            s.lookup(&key("b.rs"), 2).is_none(),
            "another file is a miss"
        );
        s.put(key("b.rs"), view);
        assert!(s.lookup(&key("a.rs"), 2).is_none(), "one answer is kept");
        s.clear();
        assert!(s.is_empty() && s.lookup(&key("b.rs"), 2).is_none());
    }

    #[test]
    fn back_pops_a_scroll_for_this_finding_and_a_move_for_another() {
        let e = |k: &str, line| HistoryEntry {
            key: k.into(),
            path: "a.rs".into(),
            line,
        };
        let mut h = vec![e("a", 5), e("b", 9)];
        assert_eq!(
            back_step(&mut h, "a", "a.rs"),
            BackStep::Finding("b".into())
        );
        assert_eq!(back_step(&mut h, "a", "a.rs"), BackStep::Line(5));
        assert_eq!(back_step(&mut h, "a", "a.rs"), BackStep::Nothing);
    }

    #[test]
    fn back_returns_through_a_reference_excursion_one_hop_at_a_time() {
        // On finding `a` (in a.rs) at line 10: S, jump to b.rs:40, S again, jump to c.rs:7.
        let mut h = Vec::new();
        let at = |path: &str, line| HistoryEntry {
            key: "a".into(),
            path: path.into(),
            line,
        };
        push_history(&mut h, at("a.rs", 10));
        push_history(&mut h, at("b.rs", 40));
        // Backspace: from c.rs to b.rs:40 (still not the finding's file).
        assert_eq!(
            back_step(&mut h, "a", "a.rs"),
            BackStep::Location {
                path: "b.rs".into(),
                line: 40
            }
        );
        // Backspace: from b.rs to the finding's own line.
        assert_eq!(back_step(&mut h, "a", "a.rs"), BackStep::Line(10));
        assert_eq!(back_step(&mut h, "a", "a.rs"), BackStep::Nothing);
    }

    #[test]
    fn escape_closes_the_key_list_before_the_viewer() {
        assert_eq!(close_target(true), CloseTarget::Help);
        assert_eq!(close_target(false), CloseTarget::Viewer);
    }

    #[test]
    fn a_refused_decision_keeps_the_prompt_and_a_landed_one_advances() {
        assert_eq!(
            finish_disposition(false, Some("next".into())),
            DisposeOutcome {
                close_prompt: false,
                goto: None
            }
        );
        assert_eq!(
            finish_disposition(true, Some("next".into())),
            DisposeOutcome {
                close_prompt: true,
                goto: Some("next".into())
            }
        );
        assert_eq!(
            finish_disposition(true, None),
            DisposeOutcome {
                close_prompt: true,
                goto: None
            }
        );
    }

    #[test]
    fn the_scan_wins_where_the_runs_differ_and_the_difference_is_named() {
        let s = |r: &str, sha: Option<&str>| (r.to_string(), sha.map(str::to_string));
        let scan = vec![
            s("o/a", Some("1111")),
            s("o/b", Some("2222")),
            s("o/c", None),
            s("o/d", Some("")),
        ];
        let audit = vec![
            s("o/a", Some("1111")),
            s("o/b", Some("9999")),
            s("o/c", Some("3333")),
            s("o/e", Some("5555")),
        ];
        let c = merge_audited_shas(&scan, &audit);
        assert_eq!(c.shas["o/a"], "1111");
        assert_eq!(c.shas["o/b"], "2222", "the scan's commit");
        assert_eq!(c.shas["o/c"], "3333", "only the audit recorded one");
        assert_eq!(c.shas["o/e"], "5555");
        assert!(!c.shas.contains_key("o/d"), "an empty sha is no sha");
        assert_eq!(c.conflicts, HashSet::from(["o/b".to_string()]));
        assert!(SHA_CONFLICT_NOTE.contains("different commits"));
        assert_eq!(merge_audited_shas(&[], &[]), ShaChoice::default());
    }

    /// The whole sequence the review asked for, on the pure pieces: open on A, go to a line
    /// (history), `]` to B, `[` back to A, `c` on A, then Backspace. The history entry names A,
    /// which has left the table, so the cursor must not move.
    #[test]
    fn backspace_after_deciding_the_finding_in_history_does_not_move_the_cursor() {
        use crate::triage::RowCursor;
        let fs = vec![
            finding("R-A", "a.rs", 10, "high"),
            finding("R-B", "a.rs", 40, "high"),
        ];
        let mut d = HashMap::new();
        let mut table: Vec<String> = fs.iter().map(finding_key).collect();
        let (a, b) = (table[0].clone(), table[1].clone());
        let nav = |table: &[String]| table.iter().cloned().collect::<HashSet<_>>();

        // Open on A; G12 pushes (A, 10).
        let mut history = Vec::new();
        push_history(
            &mut history,
            HistoryEntry {
                key: a.clone(),
                path: "a.rs".into(),
                line: 10,
            },
        );
        // `]` to B, `[` back to A.
        let items = file_findings(&fs, &d, "o/r", "a.rs", &nav(&table));
        let FileStep::Move(k) = step_in_file(&items, &a, FocusMove::Down) else {
            panic!()
        };
        assert_eq!(k, b);
        assert_eq!(
            RowCursor::focused_on(k).resolve(&table).key(),
            Some(b.as_str())
        );
        let FileStep::Move(k) = step_in_file(&items, &b, FocusMove::Up) else {
            panic!()
        };
        assert_eq!(
            RowCursor::focused_on(k).resolve(&table).key(),
            Some(a.as_str())
        );
        // `c` on A: it lands, the target is B, A leaves the table.
        let target = advance_after_disposition(&items, &a);
        decide(&mut d, &fs[0], Confirmed);
        table.retain(|k| *k != a);
        let outcome = finish_disposition(true, target);
        assert_eq!(outcome.goto.as_deref(), Some(b.as_str()));
        let cursor = RowCursor::focused_on(b.clone()).resolve(&table);
        assert_eq!(cursor.key(), Some(b.as_str()));
        // Backspace: the entry is for A, another finding than the one on screen (B).
        let BackStep::Finding(target) = back_step(&mut history, &b, "a.rs") else {
            panic!()
        };
        assert_eq!(target, a);
        // The caller's reach check: A is no longer a row, so the cursor stays on B.
        assert!(!table.contains(&target), "A has left the table");
        assert_eq!(cursor.resolve(&table).key(), Some(b.as_str()));
        // Had the cursor been aimed at A anyway, resolve would have clamped to row 0: the check
        // is what prevents it.
        assert_eq!(RowCursor::focused_on(a).resolve(&table).index(), 0);
    }
}
