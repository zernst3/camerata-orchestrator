//! The in-app code viewer (`WORK_ORDER_REVIEW_SPEED.md` section A, build step 2).
//!
//! A full-window overlay over the Queue that shows the file a finding lives in, as it was at
//! the inspected commit, and lets the curator decide the finding without leaving the code.
//!
//! Two components, split the way the detail pane is split:
//!
//! * [`CodeViewer`] is props-only. It chooses words and pixels and nothing else, so SSR tests can
//!   render every state (loading, code, each refusal, the banners, the gutter, the band).
//! * [`CodeViewerHost`] owns the signals and the I/O: the fetch, the cache, the scroll, the
//!   prompts. It makes no judgment. Every decision (which key means what, which finding is
//!   next, where the cursor goes after a disposition) is a pure function in
//!   `camerata_ui_core::viewer`, unit-tested without a browser.
//!
//! ## One cursor
//!
//! The viewer never has a "current finding" of its own. [`CodeViewerHost`] is handed the
//! finding the Queue's `RowCursor` is on, and every navigation writes to that cursor. The host
//! is keyed by the finding's key, so a new finding is a fresh host: scroll, prompts and the
//! fetch all start clean, and a cached file paints on the first frame.
//!
//! ## One disposition path
//!
//! A disposition key in the viewer calls the SAME `apply_disposition` callback the pane's keys
//! and buttons call. The viewer adds only what is its own: the reason prompt (kept apart from the
//! pane's, which would steal focus) and the "advance within this file" rule.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use camerata_ui_core::triage::{
    finding_key, finding_state, Disposition, FindingView, KeyMods, RowCursor, TriageState,
    UndoEntry,
};
use camerata_ui_core::viewer::{
    advance_after_disposition, allowed_in_reference_view, back_step, block_for, block_from_context,
    clears_search, close_target, cut_line, file_findings, finding_position, finish_disposition,
    gutter_marks, hub_close_focus, line_at_scroll, line_label, more_chars_label, page_request,
    parse_goto_line, prefetch_target, push_history, scroll_top_for_line, scrolled, severity_class,
    spacers, step_file_in_group, step_in_file, viewer_code_keys_armed, viewer_command,
    viewer_focus, viewer_keys_armed, virtual_range, BackStep, BannerKind, BlockView, CloseTarget,
    FileKey, FileStep, FocusReturn, GutterMark, HistoryEntry, OpenSurfaces, SessionCopy,
    SourceFileView, SourceOutcome, ViewerCache, ViewerCommand, ViewerFocus, CODE_LINE_H_PX,
    KEY_ROWS, MAX_LINE_CHARS, OVERSCAN_LINES, SCROLL_QUANTUM_LINES, SHA_CONFLICT_NOTE,
};
use camerata_ui_core::viewer_find::{
    cycle_match, first_match_from, location_plan, mark_matches, move_selection, permalink,
    popover_view, search_key, search_status, search_view, segments_text, symbol_prefill, Authority,
    LocationPlan, Mark, PopoverView, RefHit, RefsOutcome, SearchKey, SymbolRefsView,
};
use chorale_core::GroupKey;
use chorale_dioxus::UseTableHandle;
use dioxus::prelude::*;

use super::scan::{kind_chip_parts, undo_banner_of, RuleHubRequest};

/// The DOM id of the viewer's focusable root. The keydown handler lives here.
pub(super) const VIEWER_ID: &str = "camerata-code-viewer";
/// The DOM id of the scroll container.
pub(super) const SCROLL_ID: &str = "camerata-code-scroll";

/// What the viewer assumes of its viewport until the DOM says otherwise.
const DEFAULT_VIEWPORT_PX: f64 = 900.0;

/// A finding key made safe for an HTML attribute (the key joins its parts with NUL).
pub(super) fn dom_key(key: &str) -> String {
    key.replace('\0', "|")
}

/// Ask another surface to open the viewer on a finding, by key. Provided as context by the
/// results view; the Queue consumes it. The finding modal's "Open in viewer" button writes it.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct ViewerRequest(pub Signal<Option<String>>);

// ── The request ──────────────────────────────────────────────────────────────────────────

/// `GET /api/onboard/source-file`. `None` when the server cannot be reached or does not answer
/// with JSON. A refusal is an ANSWER (a status and a reason), not a `None`; the 400 for a path
/// outside the repo carries one too, so the body is read whatever the HTTP status.
pub(super) async fn fetch_source_file(
    repo: &str,
    path: &str,
    sha: Option<&str>,
    around: Option<usize>,
    project: Option<&str>,
) -> Option<SourceFileView> {
    let mut params = vec![("repo", repo.to_string()), ("path", path.to_string())];
    if let Some(sha) = sha.filter(|s| !s.is_empty()) {
        params.push(("sha", sha.to_string()));
    }
    if let Some(line) = around {
        params.push(("around", line.to_string()));
    }
    if let Some(project) = project.filter(|p| !p.is_empty()) {
        params.push(("project", project.to_string()));
    }
    let resp = reqwest::Client::new()
        .get(format!("{}/api/onboard/source-file", crate::bff_base()))
        .query(&params)
        .send()
        .await
        .ok()?;
    let value: serde_json::Value = resp.json().await.ok()?;
    let mut view: SourceFileView = serde_json::from_value(value).ok()?;
    // The server's `block` is for the line it was asked about; remember which that was.
    view.requested_around = around;
    Some(view)
}

/// The enclosing block of one line, from the small `finding-context` endpoint. Used when the file
/// came from the cache (fetched for another finding's line), so the band marks THIS finding's code
/// without refetching the whole file.
pub(super) async fn fetch_block(
    repo: &str,
    path: &str,
    line: usize,
    sha: Option<&str>,
    project: Option<&str>,
) -> Option<BlockView> {
    let mut params = vec![
        ("repo", repo.to_string()),
        ("path", path.to_string()),
        ("line", line.to_string()),
    ];
    if let Some(sha) = sha.filter(|s| !s.is_empty()) {
        params.push(("sha", sha.to_string()));
    }
    if let Some(project) = project.filter(|p| !p.is_empty()) {
        params.push(("project", project.to_string()));
    }
    let resp = reqwest::Client::new()
        .get(format!("{}/api/onboard/finding-context", crate::bff_base()))
        .query(&params)
        .send()
        .await
        .ok()?;
    let v: serde_json::Value = resp.json().await.ok()?;
    if v.get("status").and_then(|s| s.as_str()) != Some("ok") {
        return None;
    }
    block_from_context(
        usize::try_from(v.get("start_line")?.as_u64()?).ok()?,
        v.get("lines")?.as_array()?.len(),
        v.get("kind").and_then(|k| k.as_str()).unwrap_or(""),
    )
}

/// `GET /api/onboard/symbol-refs`. `None` when the server cannot be reached or does not answer
/// with JSON. A refusal (a deleted source, an unknown repo, an invalid symbol) is an ANSWER with
/// a status and a reason where the server sends one, so the list can say why it is empty.
pub(super) async fn fetch_symbol_refs(
    repo: &str,
    symbol: &str,
    sha: Option<&str>,
    project: Option<&str>,
) -> Option<SymbolRefsView> {
    let mut params = vec![("repo", repo.to_string()), ("symbol", symbol.to_string())];
    if let Some(sha) = sha.filter(|s| !s.is_empty()) {
        params.push(("sha", sha.to_string()));
    }
    if let Some(project) = project.filter(|p| !p.is_empty()) {
        params.push(("project", project.to_string()));
    }
    let resp = reqwest::Client::new()
        .get(format!("{}/api/onboard/symbol-refs", crate::bff_base()))
        .query(&params)
        .send()
        .await
        .ok()?;
    let status = resp.status();
    let value: serde_json::Value = resp.json().await.ok()?;
    let mut view: SymbolRefsView = serde_json::from_value(value.clone()).ok()?;
    if view.status.is_empty() {
        // A 400 refusal carries `{ok:false, code, message}`, not a status.
        view.status = "invalid".to_string();
        view.reason = value
            .get("message")
            .and_then(|m| m.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("The search was refused ({status})"));
    }
    if view.symbol.is_empty() {
        view.symbol = symbol.to_string();
    }
    Some(view)
}

/// The text a permalink is copied with, through the clipboard bridge's own channel when it is
/// installed (the desktop webview cannot reliably copy from page script), else the page-script
/// fallback every other "Copy" in the app uses. Returns the script for the bridge. Pure.
pub(super) fn bridge_copy_script(text: &str) -> Option<String> {
    let literal = serde_json::to_string(text).ok()?;
    Some(format!(
        "if (window.__camerataClipboardBridgeSend) {{ \
           window.__camerataClipboardBridgeSend({{ op: 'copy', text: {literal} }}); return true; \
         }} return false;"
    ))
}

/// Copy `text`; `true` when something took it.
async fn copy_text(text: &str) -> bool {
    if let Some(script) = bridge_copy_script(text) {
        if matches!(
            dioxus::document::eval(&script).await,
            Ok(serde_json::Value::Bool(true))
        ) {
            return true;
        }
    }
    crate::source_ui::copy_to_clipboard(text).await
}

// ── What the props-only component is given ───────────────────────────────────────────────

/// The in-file search, as the component draws it.
#[derive(Clone, PartialEq, Default)]
pub(super) struct SearchProps {
    pub needle: String,
    /// The field is open and owns the keyboard. Closed with a needle left: highlights only.
    pub field_open: bool,
    /// `3 of 12`, `3 of 12 (wrapped)`, `No matches`.
    pub status: String,
    /// `(file line, char column)` of every match, in file order.
    pub matches: Arc<Vec<(usize, usize)>>,
    pub needle_chars: usize,
    /// The match Enter will step from.
    pub current: Option<usize>,
}

/// The symbol prompt, as the component draws it.
#[derive(Clone, PartialEq)]
pub(super) struct SymbolPromptProps {
    pub text: String,
    pub error: Option<String>,
}

/// Where the references request stands.
#[derive(Clone, PartialEq)]
pub(super) enum RefsLoad {
    Loading,
    /// The server did not answer.
    Failed,
    Loaded(Arc<SymbolRefsView>),
}

/// The references list, as the component draws it.
#[derive(Clone, PartialEq)]
pub(super) struct RefsProps {
    pub symbol: String,
    pub load: RefsLoad,
    pub selected: usize,
}

/// Where the file stands.
#[derive(Clone, PartialEq)]
pub(super) enum FileStateView {
    Loading,
    /// The server did not answer.
    Failed,
    Loaded(Arc<SourceFileView>),
}

/// The finding the viewer is on, as the header and the highlight need it.
#[derive(Clone, PartialEq, Default)]
pub(super) struct ViewerFinding {
    pub repo: String,
    pub path: String,
    pub rule_id: String,
    pub severity: String,
    pub line: usize,
    /// The stored one-line evidence: the floor shown when no code can be.
    pub snippet: String,
    /// The first 7 characters of the commit the report recorded, when it recorded one.
    pub sha7: String,
    /// The scan and the audit recorded different commits for this repo.
    pub sha_conflict: bool,
}

impl ViewerFinding {
    pub(super) fn from_finding(f: &FindingView, sha: &str, sha_conflict: bool) -> Self {
        Self {
            repo: f.repo.clone(),
            path: f.path.clone(),
            rule_id: f.rule_id.clone(),
            severity: f.severity.clone(),
            line: f.line,
            snippet: f.snippet.clone(),
            sha7: camerata_ui_core::source::short_sha(sha).to_string(),
            sha_conflict,
        }
    }
}

fn key_mods(e: &KeyboardEvent) -> KeyMods {
    let m = e.modifiers();
    KeyMods {
        ctrl: m.contains(Modifiers::CONTROL),
        alt: m.contains(Modifiers::ALT),
        meta: m.contains(Modifiers::META),
        shift: m.contains(Modifiers::SHIFT),
    }
}

/// The class suffix for a known token class; anything the server sends that this build does not
/// style renders as plain text (the class name never reaches the DOM unchecked).
fn tok_class(view: &SourceFileView, index: usize) -> Option<&'static str> {
    const KNOWN: [&str; 10] = [
        "keyword",
        "string",
        "comment",
        "number",
        "function",
        "type",
        "variable",
        "property",
        "punctuation",
        "operator",
    ];
    let name = view.classes.get(index)?;
    KNOWN.iter().find(|k| **k == name).copied()
}

/// The highlighted text of one line, cut at [`MAX_LINE_CHARS`], with the search matches on it
/// (`ranges` are `(char column, chars)`; `current` is the column of the current match).
fn line_spans(
    view: &SourceFileView,
    segments: &[camerata_ui_core::viewer::Segment],
    ranges: &[(usize, usize)],
    current: Option<usize>,
) -> Element {
    let cut = cut_line(segments, MAX_LINE_CHARS);
    let (shown, more): (&[_], Option<usize>) = match &cut {
        Some((segs, more)) => (segs.as_slice(), Some(*more)),
        None => (segments, None),
    };
    if ranges.is_empty() {
        return rsx! {
            for (i, (class, text)) in shown.iter().enumerate() {
                if let Some(c) = class.and_then(|c| tok_class(view, c)) {
                    span { key: "{i}", class: "tok-{c}", "{text}" }
                } else {
                    span { key: "{i}", "{text}" }
                }
            }
            if let Some(n) = more {
                span { class: "cv-more", "{more_chars_label(n)}" }
            }
        };
    }
    let pieces = mark_matches(shown, ranges, current);
    rsx! {
        for (i, piece) in pieces.iter().enumerate() {
            span {
                key: "{i}",
                class: "{piece_class(view, piece.class, piece.mark)}",
                "{piece.text}"
            }
        }
        if let Some(n) = more {
            span { class: "cv-more", "{more_chars_label(n)}" }
        }
    }
}

/// The CSS classes of a run of a line: its syntax class and its search mark.
fn piece_class(view: &SourceFileView, class: Option<usize>, mark: Mark) -> String {
    let mut out = String::new();
    if let Some(c) = class.and_then(|c| tok_class(view, c)) {
        out.push_str("tok-");
        out.push_str(c);
    }
    match mark {
        Mark::None => {}
        Mark::Match => out.push_str(" cv-match"),
        Mark::Current => out.push_str(" cv-match cv-match-cur"),
    }
    out.trim().to_string()
}

/// The matches on line `n`: `(char column, chars)`, from the file-ordered match list.
fn ranges_on_line(search: Option<&SearchProps>, n: usize) -> Vec<(usize, usize)> {
    let Some(s) = search else {
        return Vec::new();
    };
    if s.needle_chars == 0 {
        return Vec::new();
    }
    let first = s.matches.partition_point(|(line, _)| *line < n);
    s.matches[first..]
        .iter()
        .take_while(|(line, _)| *line == n)
        .map(|(_, col)| (*col, s.needle_chars))
        .collect()
}

/// The code area: gutter, lines, band, highlight, minimap. Only the lines in
/// [`virtual_range`] are in the DOM.
#[allow(clippy::too_many_arguments)]
fn code_body(
    view: &SourceFileView,
    path_key: &str,
    severity: &str,
    hit: Option<usize>,
    block: Option<&BlockView>,
    marks: &[GutterMark],
    scroll_top: f64,
    viewport_px: f64,
    line_cursor: Option<usize>,
    search: Option<&SearchProps>,
    on_scroll: Callback<(f64, f64)>,
    on_scroll_ready: Callback<()>,
    on_mark: Callback<String>,
    on_line: Callback<usize>,
    on_hover: Callback<Option<usize>>,
) -> Element {
    let total = view.total();
    if total == 0 {
        return rsx! { div { class: "cv-note", "This file is empty." } };
    }
    let w = view.window_or_default();
    let range = virtual_range(
        total,
        scroll_top,
        viewport_px,
        CODE_LINE_H_PX,
        OVERSCAN_LINES,
    )
    .within(w.from, w.to);
    let (pad_top, pad_bottom) = spacers(total, range, CODE_LINE_H_PX);
    let mark_at: HashMap<usize, &GutterMark> = marks.iter().map(|m| (m.line, m)).collect();
    let sev = severity_class(severity);
    let current_at = search.and_then(|s| s.current.and_then(|i| s.matches.get(i).copied()));
    rsx! {
        div { class: "cv-body", key: "{path_key}",
            div {
                class: "cv-scroll",
                id: SCROLL_ID,
                onmounted: move |_| on_scroll_ready.call(()),
                onscroll: move |e: Event<ScrollData>| {
                    on_scroll.call((e.scroll_top(), f64::from(e.client_height())));
                },
                div {
                    class: "cv-lines",
                    style: "padding-top:{pad_top}px;padding-bottom:{pad_bottom}px;",
                    for n in range.first..=range.last {
                        {
                            let is_hit = hit == Some(n);
                            let in_band = block
                                .is_some_and(|b| n >= b.start_line && n <= b.end_line);
                            let class = line_class(is_hit, in_band, line_cursor == Some(n), sev);
                            let mark = mark_at.get(&n).copied();
                            let has_mark = mark.is_some();
                            let segments = view.line(n);
                            let ranges = ranges_on_line(search, n);
                            let current_col = current_at.filter(|(l, _)| *l == n).map(|(_, c)| c);
                            rsx! {
                                div {
                                    key: "{n}",
                                    class: "{class}",
                                    onmouseenter: move |_| {
                                        if is_hit {
                                            on_hover.call(Some(n));
                                        }
                                    },
                                    onmouseleave: move |_| {
                                        if is_hit {
                                            on_hover.call(None);
                                        }
                                    },
                                    // The whole gutter cell is the hover target, not the 10px dot: a
                                    // decided dot is a disabled button, which some webviews do not
                                    // send mouse events for.
                                    span {
                                        class: "cv-gutter",
                                        onmouseenter: move |_| {
                                            if has_mark {
                                                on_hover.call(Some(n));
                                            }
                                        },
                                        onmouseleave: move |_| {
                                            if has_mark {
                                                on_hover.call(None);
                                            }
                                        },
                                        if let Some(m) = mark {
                                            {
                                                let target = m.target.clone();
                                                let cls = mark_class(m);
                                                rsx! {
                                                    button {
                                                        class: "{cls}",
                                                        title: "{m.title}",
                                                        disabled: target.is_none(),
                                                        onclick: move |e: MouseEvent| {
                                                            e.stop_propagation();
                                                            if let Some(t) = target.clone() {
                                                                on_mark.call(t);
                                                            }
                                                        },
                                                        if m.count > 1 {
                                                            span { class: "cv-count", "{m.count}" }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    span {
                                        class: "cv-num",
                                        onclick: move |_| on_line.call(n),
                                        "{n}"
                                    }
                                    span { class: "cv-code",
                                        if let Some(segs) = segments {
                                            {line_spans(view, segs, &ranges, current_col)}
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // The minimap: every marker at file scale, plus the current finding's line.
            div { class: "cv-minimap",
                for m in marks.iter() {
                    span {
                        key: "{m.line}",
                        class: "{minimap_class(m)}",
                        style: "top:{minimap_top(m.line, total)}%;",
                        title: "{m.title}",
                    }
                }
                if let Some(hit) = hit {
                    span {
                        class: "cv-mini-cur",
                        style: "top:{minimap_top(hit, total)}%;",
                    }
                }
            }
        }
    }
}

/// The CSS classes of a code line.
pub(super) fn line_class(is_hit: bool, in_band: bool, is_cursor: bool, sev: &str) -> String {
    let mut c = String::from("cv-line");
    if in_band {
        c.push_str(" cv-line-band");
    }
    if is_hit {
        c.push_str(" cv-line-hit cv-sev-");
        c.push_str(sev);
    }
    if is_cursor {
        c.push_str(" cv-line-cursor");
    }
    c
}

/// A solid dot for an open finding, a hollow one once every finding on the line is decided.
pub(super) fn mark_class(m: &GutterMark) -> String {
    format!(
        "cv-mark cv-sev-{}{}",
        severity_class(&m.severity),
        if m.decided { " cv-mark-hollow" } else { "" }
    )
}

fn minimap_class(m: &GutterMark) -> String {
    format!(
        "cv-mini cv-sev-{}{}",
        severity_class(&m.severity),
        if m.decided { " cv-mark-hollow" } else { "" }
    )
}

fn minimap_top(line: usize, total: usize) -> String {
    let pct = if total == 0 {
        0.0
    } else {
        (line.saturating_sub(1)) as f64 / total as f64 * 100.0
    };
    format!("{pct:.2}")
}

/// A note in place of code: the reason, the stored evidence as the floor, and (for a file too
/// large to view) the way to the modal's enclosing-block section.
fn note_body(
    text: &str,
    snippet: Option<&str>,
    offer_block: bool,
    on_open_modal: Callback<()>,
) -> Element {
    rsx! {
        div { class: "cv-note",
            p { class: "cv-note-text", "{text}" }
            if offer_block {
                button {
                    class: "btn-edit-sm cv-note-btn",
                    onclick: move |_| on_open_modal.call(()),
                    "Show the enclosing block"
                }
            }
            if let Some(s) = snippet.filter(|s| !s.trim().is_empty()) {
                p { class: "rule-modal-label", "Evidence" }
                pre { class: "pane-snippet", "{s}" }
            }
        }
    }
}

/// The finding popover (A.5): at most eight lines, no fix text.
fn popover_card(
    p: &PopoverView,
    pinned: bool,
    line: Option<usize>,
    on_hover: Callback<Option<usize>>,
    on_hub: Callback<String>,
    on_goto: Callback<String>,
) -> Element {
    let (kind_class, kind_label, kind_tip) = kind_chip_parts(p.kind_tag);
    let rule = p.rule_id.clone();
    let class = if pinned {
        "cv-popover cv-popover-pinned"
    } else {
        "cv-popover"
    };
    rsx! {
        div {
            class: "{class}",
            title: if pinned { "Pinned. Space unpins it." } else { "Space pins it." },
            // The pointer may move from the line onto the card to click in it.
            onmouseenter: move |_| {
                if line.is_some() {
                    on_hover.call(line);
                }
            },
            onmouseleave: move |_| on_hover.call(None),
            div { class: "cv-pop-line cv-pop-head",
                button {
                    class: "cv-pop-rule",
                    title: "Open the Rule Hub (H)",
                    onclick: move |_| on_hub.call(rule.clone()),
                    "{p.rule_id}"
                }
                span { class: "{kind_class}", title: "{kind_tip}", "{kind_label}" }
                span { class: "cv-pop-auth", "{p.authority.label()}" }
            }
            if let Some(h) = p.headline.clone() {
                div { class: "cv-pop-line cv-pop-headline", "{h}" }
            }
            if let Some(d) = p.directive.clone() {
                div { class: "cv-pop-line cv-pop-directive", "{d}" }
            }
            div { class: "cv-pop-line",
                span { class: "cv-pop-sev {severity_class(&p.severity)}", "{p.severity}" }
                span { class: "cv-sep", "\u{b7}" }
                span { "{p.confidence}" }
                if let Some(e) = p.effort.clone() {
                    span { class: "cv-sep", "\u{b7}" }
                    span { "effort {e}" }
                }
            }
            if let Some(r) = p.needs_review.clone() {
                div { class: "cv-pop-line cv-pop-review", strong { "Needs review: {r}" } }
            }
            div { class: "cv-pop-line",
                span { "{p.disposition}" }
                if let Some(prev) = p.previously.clone() {
                    span { class: "cv-sep", "\u{b7}" }
                    span { class: "cv-pop-prev", "{prev}" }
                }
            }
            if !p.related.is_empty() {
                div { class: "cv-pop-line cv-pop-related",
                    span { "{p.related.len() + p.related_more} more in this file: " }
                    for r in p.related.iter() {
                        {
                            let key = r.key.clone();
                            let navigable = r.navigable;
                            rsx! {
                                if navigable {
                                    button {
                                        key: "{r.key}",
                                        class: "cv-pop-link",
                                        onclick: move |_| on_goto.call(key.clone()),
                                        "L{r.line} {r.rule_id}"
                                    }
                                } else {
                                    span { key: "{r.key}", class: "cv-pop-dim", "L{r.line} {r.rule_id}" }
                                }
                            }
                        }
                    }
                    if p.related_more > 0 {
                        span { class: "cv-pop-dim", "and {p.related_more} more" }
                    }
                }
            }
            if !p.also_matches.is_empty() {
                div { class: "cv-pop-line",
                    "Also violates here: {p.also_matches.join(\", \")}"
                }
            }
        }
    }
}

/// The symbol references list (A.6), at the right of the code.
fn refs_panel(r: &RefsProps, on_pick: Callback<usize>) -> Element {
    rsx! {
        aside { class: "cv-refs", "data-selected": "{r.selected}",
            match &r.load {
                RefsLoad::Loading => rsx! {
                    div { class: "cv-refs-head", "Searching for {r.symbol}\u{2026}" }
                },
                RefsLoad::Failed => rsx! {
                    div { class: "cv-refs-head", "The server did not answer. Esc goes back." }
                },
                RefsLoad::Loaded(v) => rsx! {
                    div { class: "cv-refs-head", "{v.summary()}" }
                    if let Some(b) = v.banner() {
                        div { class: "cv-banner cv-banner-amber", "{b}" }
                    }
                    match v.outcome() {
                        RefsOutcome::Note(text) => rsx! {
                            div { class: "cv-refs-note", "{text}" }
                        },
                        RefsOutcome::List => rsx! {
                            div { class: "cv-refs-list",
                                for (i, hit) in v.hits.iter().enumerate() {
                                    div {
                                        key: "{i}",
                                        class: if i == r.selected { "cv-ref cv-ref-sel" } else { "cv-ref" },
                                        onclick: move |_| on_pick.call(i),
                                        div { class: "cv-ref-loc", "{hit.path}:{hit.line}" }
                                        div { class: "cv-ref-text", "{hit.text}" }
                                    }
                                }
                            }
                        },
                    }
                },
            }
            div { class: "cv-refs-keys", "Up Down move \u{b7} Enter open \u{b7} Esc back" }
        }
    }
}

/// THE CODE VIEWER, props-only. See the module docs.
#[component]
#[allow(clippy::too_many_arguments)]
pub(super) fn CodeViewer(
    /// The finding the cursor is on.
    finding: ViewerFinding,
    /// Its key, for the `data-finding-key` attribute the one-cursor test reads.
    finding_key: String,
    file: FileStateView,
    /// The finding's own enclosing block, which the band marks. It is NOT read from `file`: a
    /// cached or paged answer carries the block of some other line.
    #[props(default)]
    block: Option<BlockView>,
    /// One marker per cited line in this file.
    #[props(default)]
    marks: Vec<GutterMark>,
    /// "finding K of M in this file".
    #[props(default)]
    position: (usize, usize),
    #[props(default)] scroll_top: f64,
    #[props(default = DEFAULT_VIEWPORT_PX)] viewport_px: f64,
    #[props(default)] line_cursor: Option<usize>,
    /// The path of a reference being shown, when it is not the finding's own file. The finding's
    /// line and band are then not drawn: they belong to another file.
    #[props(default)]
    viewing: Option<String>,
    /// The go-to-line field, when open.
    #[props(default)]
    goto: Option<String>,
    #[props(default)] goto_error: Option<String>,
    /// The in-file search: the field while it is open, the highlights after it closes.
    #[props(default)]
    search: Option<SearchProps>,
    /// The symbol prompt, when open.
    #[props(default)]
    symbol_prompt: Option<SymbolPromptProps>,
    /// The references list, when open.
    #[props(default)]
    refs: Option<RefsProps>,
    /// The popover, when it is showing (hovered or pinned).
    #[props(default)]
    popover: Option<PopoverView>,
    /// The line the hovered popover is about, so the card can hold itself open.
    #[props(default)]
    popover_line: Option<usize>,
    #[props(default)] pinned: bool,
    /// The Rule Hub is open above the viewer: the viewer's keys are off.
    #[props(default)]
    hub_open: bool,
    /// The disposition whose reason is being collected.
    #[props(default)]
    prompt: Option<TriageState>,
    #[props(default)] prompt_reason: String,
    /// The one-line message in the status strip.
    #[props(default)]
    status: Option<String>,
    #[props(default)] help: bool,
    #[props(default)] on_key: Callback<(String, KeyMods)>,
    #[props(default)] on_close: Callback<()>,
    /// `(scrollTop, clientHeight)` from the scroll container.
    #[props(default)]
    on_scroll: Callback<(f64, f64)>,
    #[props(default)] on_scroll_ready: Callback<()>,
    #[props(default)] on_goto_input: Callback<String>,
    #[props(default)] on_goto_commit: Callback<()>,
    #[props(default)] on_goto_cancel: Callback<()>,
    #[props(default)] on_search_input: Callback<String>,
    #[props(default)] on_search_key: Callback<SearchKey>,
    #[props(default)] on_symbol_input: Callback<String>,
    #[props(default)] on_symbol_commit: Callback<()>,
    #[props(default)] on_symbol_cancel: Callback<()>,
    #[props(default)] on_ref_pick: Callback<usize>,
    /// The mouse is over the finding's line or a gutter dot (`Some(line)`), or left it.
    #[props(default)]
    on_hover: Callback<Option<usize>>,
    /// A rule id in the popover was clicked: open the Rule Hub on it.
    #[props(default)]
    on_popover_hub: Callback<String>,
    /// A related finding in the popover was clicked.
    #[props(default)]
    on_popover_goto: Callback<String>,
    #[props(default)] on_prompt_reason: Callback<String>,
    #[props(default)] on_prompt_commit: Callback<()>,
    #[props(default)] on_prompt_cancel: Callback<()>,
    #[props(default)] on_mark: Callback<String>,
    #[props(default)] on_line: Callback<usize>,
    #[props(default)] on_open_modal: Callback<()>,
    /// The live undo bar, supplied by a component that owns the 250 ms ticker, so the ticker
    /// re-renders that bar and not the viewer.
    children: Element,
) -> Element {
    let focus = viewer_focus(OpenSurfaces {
        reason_prompt: prompt.is_some(),
        goto: goto.is_some(),
        search: search.as_ref().is_some_and(|s| s.field_open),
        symbol_prompt: symbol_prompt.is_some(),
        refs_list: refs.is_some(),
        rule_hub: hub_open,
    });
    let armed = viewer_keys_armed(focus);
    let code_keys = viewer_code_keys_armed(focus);
    let dom_finding_key = dom_key(&finding_key);
    let shown_path = viewing.clone().unwrap_or_else(|| finding.path.clone());
    let own_file = viewing.is_none();

    let loaded = match &file {
        FileStateView::Loaded(v) => Some(Arc::clone(v)),
        _ => None,
    };
    let outcome = loaded.as_ref().map(|v| v.outcome());
    let banner = loaded.as_ref().and_then(|v| v.banner());
    let total = loaded.as_ref().map_or(0, |v| v.total());
    let revision = match loaded.as_deref() {
        Some(v) if v.source == "working_tree" => "working tree".to_string(),
        Some(v) => v
            .short_commit()
            .map_or_else(|| finding.sha7.clone(), str::to_string),
        None => finding.sha7.clone(),
    };
    let shows_code = matches!(outcome, Some(SourceOutcome::Code));
    let here = line_cursor.unwrap_or(finding.line);
    let hit = own_file.then_some(finding.line);
    // The stored evidence line and the way to the finding's enclosing block belong to the
    // finding's own file; under a reference to another file they would describe the wrong code.
    let floor = own_file.then_some(finding.snippet.as_str());
    let search_open = search.as_ref().filter(|s| s.field_open).cloned();
    let highlights = search.clone().filter(|s| !s.needle.is_empty());

    rsx! {
        div {
            class: "rule-modal-overlay cv-overlay",
            onclick: move |_| on_close.call(()),
            div {
                class: "cv-panel",
                id: VIEWER_ID,
                tabindex: "0",
                "data-finding-key": "{dom_finding_key}",
                style: "--code-line-h:{CODE_LINE_H_PX}px;",
                onclick: move |e: MouseEvent| e.stop_propagation(),
                onmounted: move |e| {
                    spawn(async move {
                        let _ = e.set_focus(true).await;
                    });
                },
                onkeydown: move |e: KeyboardEvent| {
                    let mods = key_mods(&e);
                    let key = e.key().to_string();
                    // Swallow only the presses the viewer claims, so Tab still tabs and
                    // Cmd-C still copies code. The claim test is `viewer_command` itself.
                    if viewer_command(focus, &key, mods).is_some() {
                        e.prevent_default();
                        e.stop_propagation();
                    }
                    on_key.call((key, mods));
                },
                div { class: "cv-head",
                    span { class: "cv-repo", "{finding.repo}" }
                    if !revision.is_empty() {
                        span { class: "cv-rev", "@ {revision}" }
                    }
                    span { class: "cv-path", "{shown_path}" }
                    if !own_file {
                        span {
                            class: "cv-refchip",
                            title: "This is a reference, not a finding. Backspace goes back.",
                            "reference"
                        }
                    }
                    if shows_code {
                        span { class: "cv-sep", "\u{b7}" }
                        span { class: "cv-pos", "{line_label(here, total)}" }
                    }
                    if own_file && position.1 > 0 && position.0 > 0 {
                        span { class: "cv-sep", "\u{b7}" }
                        span { class: "cv-pos", "finding {position.0} of {position.1} in this file" }
                    }
                    span { class: "cv-rule {severity_class(&finding.severity)}", "{finding.rule_id}" }
                    button {
                        class: "rule-modal-close cv-close",
                        title: "Close (Esc)",
                        onclick: move |_| on_close.call(()),
                        "\u{2715}"
                    }
                }
                if let Some(b) = banner {
                    div {
                        class: if b.kind == BannerKind::Amber { "cv-banner cv-banner-amber" } else { "cv-banner cv-banner-muted" },
                        "{b.text}"
                    }
                }
                if finding.sha_conflict && shows_code {
                    div { class: "cv-banner cv-banner-amber", "{SHA_CONFLICT_NOTE}" }
                }
                if let Some(s) = status.clone() {
                    div { class: "cv-status", "{s}" }
                }
                if let Some(s) = search_open {
                    div { class: "cv-search",
                        label { class: "rule-modal-label", "Find" }
                        input {
                            class: "input cv-search-input",
                            r#type: "text",
                            placeholder: "search this file",
                            value: "{s.needle}",
                            onmounted: move |e| {
                                spawn(async move {
                                    let _ = e.set_focus(true).await;
                                });
                            },
                            oninput: move |e| on_search_input.call(e.value()),
                            onkeydown: move |e: KeyboardEvent| {
                                // The field owns its keys; only Enter, Shift+Enter and Esc mean
                                // anything, and nothing typed here reaches the viewer.
                                e.stop_propagation();
                                let shift = e.modifiers().contains(Modifiers::SHIFT);
                                if let Some(k) = search_key(&e.key().to_string(), shift) {
                                    e.prevent_default();
                                    on_search_key.call(k);
                                }
                            },
                        }
                        span { class: "cv-search-status", "{s.status}" }
                    }
                }
                div { class: "cv-main",
                    match (&file, &outcome) {
                        (FileStateView::Loading, _) => rsx! {
                            div { class: "cv-note", "Loading {shown_path}\u{2026}" }
                        },
                        (FileStateView::Failed, _) => rsx! {
                            {note_body("The server did not answer. The file could not be loaded.", floor, false, on_open_modal)}
                        },
                        (FileStateView::Loaded(v), Some(SourceOutcome::Code)) => rsx! {
                            {code_body(v, &shown_path, &finding.severity, hit, block.as_ref(), &marks, scroll_top, viewport_px, line_cursor, highlights.as_ref(), on_scroll, on_scroll_ready, on_mark, on_line, on_hover)}
                        },
                        // Deleted source: the message and NO code, not even the stored line.
                        (_, Some(SourceOutcome::SourceDeleted(text))) => rsx! {
                            {note_body(text, None, false, on_open_modal)}
                        },
                        (_, Some(SourceOutcome::TooLarge(text))) => rsx! {
                            {note_body(text, floor, own_file, on_open_modal)}
                        },
                        (_, Some(SourceOutcome::NotUtf8(text)))
                        | (_, Some(SourceOutcome::FileMissing(text)))
                        | (_, Some(SourceOutcome::Unavailable(text))) => rsx! {
                            {note_body(text, floor, false, on_open_modal)}
                        },
                        _ => rsx! {},
                    }
                    if let Some(r) = refs.clone() {
                        {refs_panel(&r, on_ref_pick)}
                    }
                    if let Some(p) = popover.clone() {
                        {popover_card(&p, pinned, popover_line, on_hover, on_popover_hub, on_popover_goto)}
                    }
                }
                if help {
                    div { class: "cv-help",
                        p { class: "rule-modal-label", "Keys" }
                        table {
                            for row in KEY_ROWS.iter() {
                                tr { key: "{row.label}",
                                    td { class: "cv-help-keys", "{row.label}" }
                                    td { "{row.action}" }
                                }
                            }
                        }
                    }
                }
                {children}
                if let Some(state) = prompt {
                    div { class: "pane-reason-prompt cv-prompt",
                        label { class: "rule-modal-label",
                            "Reason for \u{201c}{state.label()}\u{201d} (required)"
                        }
                        textarea {
                            class: "addressee-input pane-reason-input",
                            rows: "2",
                            placeholder: "why (the export reads this)",
                            value: "{prompt_reason}",
                            onmounted: move |e| {
                                spawn(async move {
                                    let _ = e.set_focus(true).await;
                                });
                            },
                            oninput: move |e| on_prompt_reason.call(e.value()),
                            onkeydown: move |e: KeyboardEvent| {
                                // The prompt owns its keys. Nothing typed here reaches the
                                // viewer's handler, and `viewer_command` refuses them anyway.
                                e.stop_propagation();
                                match e.key() {
                                    Key::Escape => on_prompt_cancel.call(()),
                                    Key::Enter if !e.modifiers().contains(Modifiers::SHIFT) => {
                                        e.prevent_default();
                                        on_prompt_commit.call(());
                                    }
                                    _ => {}
                                }
                            },
                        }
                        div { class: "pane-prompt-actions",
                            button {
                                class: "btn-run",
                                onclick: move |_| on_prompt_commit.call(()),
                                "Commit \u{2192}"
                            }
                            button {
                                class: "btn-edit-sm",
                                onclick: move |_| on_prompt_cancel.call(()),
                                "Cancel"
                            }
                        }
                    }
                }
                if let Some(p) = symbol_prompt {
                    div { class: "cv-goto cv-symbol",
                        label { class: "rule-modal-label", "Where is this symbol used?" }
                        input {
                            class: "input cv-goto-input cv-symbol-input",
                            r#type: "text",
                            placeholder: "symbol",
                            value: "{p.text}",
                            onmounted: move |e| {
                                spawn(async move {
                                    let _ = e.set_focus(true).await;
                                });
                            },
                            oninput: move |e| on_symbol_input.call(e.value()),
                            onkeydown: move |e: KeyboardEvent| {
                                e.stop_propagation();
                                match e.key() {
                                    Key::Escape => on_symbol_cancel.call(()),
                                    Key::Enter => {
                                        e.prevent_default();
                                        on_symbol_commit.call(());
                                    }
                                    _ => {}
                                }
                            },
                        }
                        if let Some(err) = p.error.clone() {
                            span { class: "cv-goto-error", "{err}" }
                        }
                    }
                }
                if let Some(text) = goto.clone() {
                    div { class: "cv-goto",
                        label { class: "rule-modal-label", "Go to line" }
                        input {
                            class: "input cv-goto-input",
                            r#type: "text",
                            placeholder: "line number",
                            value: "{text}",
                            onmounted: move |e| {
                                spawn(async move {
                                    let _ = e.set_focus(true).await;
                                });
                            },
                            oninput: move |e| on_goto_input.call(e.value()),
                            onkeydown: move |e: KeyboardEvent| {
                                e.stop_propagation();
                                match e.key() {
                                    Key::Escape => on_goto_cancel.call(()),
                                    Key::Enter => {
                                        e.prevent_default();
                                        on_goto_commit.call(());
                                    }
                                    _ => {}
                                }
                            },
                        }
                        if let Some(err) = goto_error.clone() {
                            span { class: "cv-goto-error", "{err}" }
                        }
                    }
                }
                div { class: if armed { "cv-keys armed" } else { "cv-keys off" },
                    if code_keys {
                        "C confirm \u{b7} T tech debt \u{b7} I ignore \u{b7} F false positive \u{b7} ] [ finding \u{b7} }} {{ file \u{b7} / find \u{b7} S symbol \u{b7} Y link \u{b7} H rule \u{b7} G line \u{b7} . this finding \u{b7} ? all keys \u{b7} Esc close"
                    } else if armed {
                        "Up Down choose a reference \u{b7} Enter open it \u{b7} Esc back to the code"
                    } else if focus == ViewerFocus::RuleHub {
                        "The Rule Hub is open. Esc closes it and returns here."
                    } else {
                        "Keys are off while you type. Enter commits, Esc cancels."
                    }
                }
            }
        }
    }
}

// ── The host ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
thread_local! {
    /// Test seam: the DOM ids focus was sent to, in order. SSR has no focus, but it can prove
    /// which element the app asked to focus.
    pub(super) static FOCUS_LOG: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Move real keyboard focus to the element with this DOM id (a no-op when there is none). THE
/// focus call: the viewer, the pane and the hub's return all go through it.
pub(super) fn focus_element(id: &str) {
    #[cfg(test)]
    FOCUS_LOG.with(|l| l.borrow_mut().push(id.to_string()));
    dioxus::document::eval(&format!(
        "var el=document.getElementById('{id}');if(el)el.focus();"
    ));
}

/// Put the page's focus back on the viewer root (after a prompt closes).
fn focus_viewer() {
    focus_element(VIEWER_ID);
}

/// When the Rule Hub closes, give the keyboard back to where it was opened from: the viewer
/// (its own host does that, so this stays out of its way) or the detail pane, on the finding the
/// cursor is still on, so the pane's keys work without a click. Called once, at the results root,
/// beside the hub. `slot` is the viewer's registration, whose `open` says whether it is up.
pub(super) fn use_hub_focus_return(hub: Signal<Option<RuleHubRequest>>, slot: Option<ViewerSlot>) {
    let mut was_open = use_signal(|| false);
    use_effect(move || {
        let now = hub.read().is_some();
        if *was_open.peek() && !now {
            let viewer_open =
                slot.is_some_and(|s| s.0.peek().as_ref().is_some_and(|l| *l.open.peek()));
            if hub_close_focus(viewer_open) == FocusReturn::Pane {
                focus_element(super::scan::DETAIL_PANE_ID);
            }
        }
        was_open.set(now);
    });
}

/// Script that brings the element matching `selector` (the selected row of a list) into view with
/// the minimum scroll, so a selection already on screen does not jump. Two animation frames: the
/// first lets Dioxus apply the new selection class, the second runs after the browser has laid it
/// out. THE follow-scroll: the references list and the stacked view's cards both use it.
pub(super) fn scroll_selected_js(selector: &str) -> String {
    format!(
        "requestAnimationFrame(function(){{requestAnimationFrame(function(){{\
var e=document.querySelector('{selector}');\
if(e){{e.scrollIntoView({{block:'nearest'}});}}}});}});"
    )
}

/// Keep the element matching `selector` on screen after a key moved the selection.
pub(super) fn follow_selected(selector: &str) {
    dioxus::document::eval(&scroll_selected_js(selector));
}

/// The selector of the selected row in the references list.
const SELECTED_REF: &str = ".cv-ref-sel";

/// Keep the selected reference on screen after an arrow key moved it.
fn follow_selected_ref() {
    follow_selected(SELECTED_REF);
}

/// Set the scroll container's `scrollTop`.
fn set_dom_scroll(px: f64) {
    dioxus::document::eval(&format!(
        "var e=document.getElementById('{SCROLL_ID}');if(e)e.scrollTop={px};"
    ));
}

/// How a finding's row relates to what the table shows.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Reach {
    /// On screen: the cursor can land on it.
    Visible,
    /// A row of the table, hidden only by these collapsed groups. Toggling them makes it visible.
    NeedsExpand(Vec<GroupKey>),
    /// Not a row of the table any more (it was decided and left, or the table was rebuilt).
    Gone,
    /// A row, but a column filter hides it.
    Filtered,
    /// The table is grouped by a column this code cannot read a value from.
    Unresolvable,
}

/// What a finding that has left the table (it was decided) is told. THE wording: the viewer's
/// status line and the stacked view's toast both show it, and only this case offers the way back.
pub(super) const GONE_PROBLEM: &str =
    "That finding has left the table. Undo its decision to bring it back.";

impl Reach {
    /// The status line for a finding the cursor cannot be aimed at. `None` when it can.
    pub(super) fn problem(&self) -> Option<&'static str> {
        match self {
            Reach::Visible | Reach::NeedsExpand(_) => None,
            Reach::Gone => Some(GONE_PROBLEM),
            Reach::Filtered => Some("That finding is hidden by a filter"),
            Reach::Unresolvable => Some("Could not open the group that holds that finding"),
        }
    }
}

/// Where `key`'s row stands, and which collapsed groups hide it.
///
/// The group keys come from the table's own `grouping`, one prefix per depth, so a different
/// grouping (an order preset) needs no change here. A cursor aimed at a row that is not visible
/// would clamp to row 0 without a word, so callers must check this first.
pub(super) fn plan_reach(state: &chorale_core::TableState<FindingView>, key: &str) -> Reach {
    let Some((_, row)) = state.rows.iter().find(|(_, f)| finding_key(f) == key) else {
        return Reach::Gone;
    };
    if !chorale_core::filtered_sorted_rows(state)
        .iter()
        .any(|f| finding_key(f) == key)
    {
        return Reach::Filtered;
    }
    let mut values: Vec<String> = Vec::new();
    let mut hiding = Vec::new();
    for column in &state.grouping {
        let Some(def) = state.columns.iter().find(|c| c.id == *column) else {
            return Reach::Unresolvable;
        };
        values.push((def.accessor)(row).to_csv_string());
        let group = GroupKey::from_values(&values);
        if state.collapsed_groups.contains(&group) {
            hiding.push(group);
        }
    }
    if hiding.is_empty() {
        Reach::Visible
    } else {
        Reach::NeedsExpand(hiding)
    }
}

/// Make `key`'s row visible by opening the groups that hide it (and only those). `Err` carries
/// the status line when it cannot be reached; nothing is changed then.
pub(super) fn reach_finding(
    handle: UseTableHandle<FindingView>,
    key: &str,
) -> Result<(), &'static str> {
    let plan = plan_reach(&handle.signal().peek(), key);
    match plan {
        Reach::Visible => Ok(()),
        Reach::NeedsExpand(groups) => {
            for group in groups {
                handle.toggle_group(group);
            }
            Ok(())
        }
        other => Err(other.problem().unwrap_or("That finding cannot be reached")),
    }
}

/// The rows the queue shows, in the order it shows them, filters applied.
fn ordered_rows(handle: UseTableHandle<FindingView>) -> Vec<FindingView> {
    chorale_core::filtered_sorted_rows(&handle.signal().peek())
}

fn navigable_keys(rows: &[FindingView]) -> HashSet<String> {
    rows.iter().map(finding_key).collect()
}

/// Everything the viewer needs from the Queue's table, registered by `FindingsTable`.
///
/// The viewer is hosted at the results root ([`ViewerRoot`]) and not inside the table: every
/// overlay in this app is, because an overlay mounted as a sibling of a chorale table has left a
/// ghost node swallowing the next click in the desktop webview (`RuleDetailModal`). So the table
/// hands over this link instead. Every field is a Copy signal, a stable callback, or data frozen
/// for the table's lifetime, so a link registered once never goes stale: `apply_disposition` and
/// `on_undo` are `use_callback` handles, which always run the latest render's closure.
#[derive(Clone)]
pub(super) struct ViewerLink {
    pub handle: UseTableHandle<FindingView>,
    /// THE cursor. The viewer reads and writes it; it has no other notion of "current".
    pub cursor: Signal<RowCursor>,
    pub visible_keys: Memo<Vec<String>>,
    pub dispositions: Signal<HashMap<String, Disposition>>,
    /// Every queue row, for the gutter (decided findings included).
    pub all_findings: Arc<Vec<FindingView>>,
    /// The inspected commit per repo, from the report's `audited_refs`.
    pub shas: Arc<HashMap<String, String>>,
    /// Repos whose scan and audit recorded different commits (the viewer says so).
    pub sha_conflicts: Arc<HashSet<String>>,
    pub project_id: String,
    pub open: Signal<bool>,
    pub undo_entry: Signal<Option<UndoEntry>>,
    pub now_ms: Signal<u64>,
    pub on_undo: Callback<()>,
    /// The pane's own disposition path. The viewer calls it; it has none of its own. Answers
    /// `true` only when a disposition was actually written.
    pub apply_disposition: Callback<(TriageState, String), bool>,
    /// Close the viewer and put DOM focus back on the pane.
    pub on_close: Callback<()>,
    /// Open the finding modal on the focused finding (the "too large" fallback surface).
    pub on_open_modal: Callback<()>,
    /// The origin remote the scan recorded per repo (tokenless), so `Y` can add a GitHub URL.
    pub origins: Arc<HashMap<String, String>>,
    /// The rule directive per rule id, for the popover. The same map the finding modal reads.
    pub descriptions: Arc<HashMap<String, String>>,
    /// Open the Rule Hub (`H`, and the rule id in the popover). The hub is hosted at the
    /// results root above the viewer; closing it leaves the viewer as it was.
    pub on_open_hub: Callback<RuleHubRequest>,
}

impl PartialEq for ViewerLink {
    fn eq(&self, other: &Self) -> bool {
        self.handle == other.handle
            && self.cursor == other.cursor
            && self.visible_keys == other.visible_keys
            && self.dispositions == other.dispositions
            && Arc::ptr_eq(&self.all_findings, &other.all_findings)
            && Arc::ptr_eq(&self.shas, &other.shas)
            && Arc::ptr_eq(&self.sha_conflicts, &other.sha_conflicts)
            && self.project_id == other.project_id
            && self.open == other.open
            && self.undo_entry == other.undo_entry
            && self.now_ms == other.now_ms
            && self.on_undo == other.on_undo
            && self.apply_disposition == other.apply_disposition
            && self.on_close == other.on_close
            && self.on_open_modal == other.on_open_modal
            && Arc::ptr_eq(&self.origins, &other.origins)
            && Arc::ptr_eq(&self.descriptions, &other.descriptions)
            && self.on_open_hub == other.on_open_hub
    }
}

/// Whether the Rule Hub is open, shared by the results root (which owns the hub) with the viewer
/// (which opens it with `H`, disarms its keys while it is up, and takes focus back when it
/// closes). Provided as context; a viewer mounted without it simply never sees the hub open.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct ViewerHubOpen(pub Signal<Option<RuleHubRequest>>);

/// Where the Queue registers its [`ViewerLink`]. Provided by the results view.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct ViewerSlot(pub Signal<Option<ViewerLink>>);

/// The live undo bar. It owns the ticker's subscription: `now_ms` changes four times a second
/// for five seconds after each decision, and only this small component re-renders for it.
#[component]
pub(super) fn ViewerUndoBar(
    undo_entry: Signal<Option<UndoEntry>>,
    now_ms: Signal<u64>,
    on_undo: Callback<()>,
    /// A DOM id for the Undo button, so a surface that shows this bar beside the pane's own
    /// (which has an Undo too) can be told apart from it.
    #[props(default)]
    button_id: Option<String>,
) -> Element {
    let Some(u) = undo_banner_of(&undo_entry.read(), *now_ms.read()) else {
        return rsx! {};
    };
    rsx! {
        div { class: "pane-undo cv-undo",
            span { class: "pane-undo-msg", "{u.message}" }
            button {
                class: "btn-edit-sm pane-undo-btn",
                id: button_id,
                onclick: move |_| on_undo.call(()),
                "Undo"
            }
            span { class: "pane-undo-left", "{u.seconds_left}s" }
        }
    }
}

/// The viewer's host at the results root. Renders nothing until the Queue has registered a link
/// and opened the viewer; then it resolves the finding the cursor is on and mounts
/// [`CodeViewerHost`] on it, keyed by that finding.
#[component]
pub(super) fn ViewerRoot() -> Element {
    let slot = try_use_context::<ViewerSlot>();
    let hub = try_use_context::<ViewerHubOpen>();
    let cache = try_use_context::<Signal<ViewerCache>>();
    let local_cache = use_signal(ViewerCache::new);
    let cache = cache.unwrap_or(local_cache);
    let mut history = use_signal(Vec::<HistoryEntry>::new);
    let mut status = use_signal(|| Option::<String>::None);
    let mut help = use_signal(|| false);
    // The last answer the LRU would not keep, for as long as this viewer stays open.
    let mut session = use_signal(SessionCopy::new);

    let Some(link) = slot.and_then(|s| s.0.read().clone()) else {
        return rsx! {};
    };
    if !*link.open.read() {
        return rsx! {};
    }
    // The ONE cursor, resolved the way the pane resolves it. Nothing here reads the undo clock:
    // that is the undo bar's subscription, not the viewer's.
    let resolved = link.cursor.read().resolve(&link.visible_keys.read());
    let finding = resolved.key().and_then(|k| {
        link.all_findings
            .iter()
            .find(|f| finding_key(f) == k)
            .cloned()
    });
    let close = link.on_close;
    let on_close = Callback::new(move |()| {
        // A new viewing starts with a clean history, no stale status line, and nothing kept
        // from this one.
        history.write().clear();
        status.set(None);
        help.set(false);
        session.write().clear();
        close.call(());
    });
    let Some(finding) = finding else {
        return rsx! {
            div { class: "rule-modal-overlay cv-overlay", onclick: move |_| on_close.call(()),
                div { class: "cv-panel cv-empty",
                    p { class: "cv-note-text", "No finding focused. Click a row first." }
                    button { class: "btn-edit-sm", onclick: move |_| on_close.call(()), "Close" }
                }
            }
        };
    };
    rsx! {
        CodeViewerHost {
            key: "{dom_key(&finding_key(&finding))}",
            finding,
            link,
            cache,
            session,
            history,
            status,
            help,
            hub,
            on_close,
        }
    }
}

/// How long a scroll must rest before the viewer asks for a new page of a windowed file.
const PAGE_DEBOUNCE_MS: u64 = 150;

/// How long the pointer must rest on the finding's line or a gutter dot before the popover shows.
const HOVER_DELAY_MS: u64 = 250;

/// How long after the pointer leaves before the popover goes, so it can be reached to click.
const HOVER_GRACE_MS: u64 = 200;

/// The in-file search while the host holds it.
#[derive(Clone, PartialEq, Default)]
struct SearchState {
    needle: String,
    /// The field is open. Closed with a needle left, the matches stay highlighted until the
    /// next navigation.
    field_open: bool,
    /// The match Enter steps from.
    current: Option<usize>,
    /// The last step wrapped past an end.
    wrapped: bool,
}

/// The references request while the host holds it.
#[derive(Clone, PartialEq)]
struct RefsState {
    symbol: String,
    load: RefsLoad,
    selected: usize,
}

/// The file whose view is a reference (not the finding's own), else `None`.
fn reference_path(finding_path: &str, viewing: Option<&String>) -> Option<String> {
    viewing.filter(|p| p.as_str() != finding_path).cloned()
}

/// THE STATEFUL VIEWER. Mounted while the viewer is open, keyed by the focused finding's key.
#[component]
#[allow(clippy::too_many_lines)]
pub(super) fn CodeViewerHost(
    /// The finding the queue's cursor is on.
    finding: FindingView,
    link: ViewerLink,
    cache: Signal<ViewerCache>,
    session: Signal<SessionCopy>,
    history: Signal<Vec<HistoryEntry>>,
    status: Signal<Option<String>>,
    help: Signal<bool>,
    #[props(default)] hub: Option<ViewerHubOpen>,
    on_close: Callback<()>,
) -> Element {
    let ViewerLink {
        handle,
        cursor,
        visible_keys,
        dispositions,
        all_findings,
        shas,
        sha_conflicts,
        project_id,
        undo_entry,
        now_ms,
        on_undo,
        apply_disposition,
        on_open_modal,
        origins,
        descriptions,
        on_open_hub,
        ..
    } = link;
    let key = finding_key(&finding);
    let sha = shas.get(&finding.repo).cloned().unwrap_or_default();
    let sha_conflict = sha_conflicts.contains(&finding.repo);
    let violation_line = finding.line;
    let mut cursor = cursor;
    let mut status = status;
    let mut help = help;
    let toasts = try_use_context::<Signal<Vec<crate::toast::Toast>>>();

    // Per-finding state. The host is keyed by the finding, so all of this starts clean when
    // the cursor moves.
    let initial_top = scroll_top_for_line(
        violation_line,
        CODE_LINE_H_PX,
        DEFAULT_VIEWPORT_PX,
        usize::MAX / 64,
    );
    let mut scroll_top = use_signal(|| initial_top);
    // The exact scroll position, written by the scroll handler and read only by the scroll
    // keys. Nothing reads it in render, so writing it re-renders nothing. (`scroll_top` is
    // quantised to 25 lines so a slow scroll does not re-render on every pixel.)
    let mut last_scroll = use_signal(|| initial_top);
    let mut viewport = use_signal(|| DEFAULT_VIEWPORT_PX);
    let mut line_cursor = use_signal(|| Option::<usize>::None);
    let mut goto = use_signal(|| Option::<String>::None);
    let mut goto_error = use_signal(|| Option::<String>::None);
    let mut prompt = use_signal(|| Option::<TriageState>::None);
    let mut prompt_reason = use_signal(String::new);
    let mut around = use_signal(|| Option::<usize>::None);
    // The debounce for paging a windowed file; neither is read in render.
    let mut page_target = use_signal(|| Option::<usize>::None);
    let mut page_timer = use_signal(|| false);
    // THIS finding's enclosing block, decided once from the first answer that can be trusted
    // (see `block_for`) and never replaced by a page or a cached answer for another line.
    // `None` is "not decided yet"; `Some(None)` is "this file has no block".
    let mut block = use_signal(|| Option::<Option<BlockView>>::None);
    // A file other than the finding's own that a symbol reference opened. A reference is a code
    // location, not a finding: it never moves the cursor.
    let mut viewing = use_signal(|| Option::<String>::None);
    let mut search = use_signal(|| Option::<SearchState>::None);
    let mut symbol_prompt = use_signal(|| Option::<(String, Option<String>)>::None);
    let mut refs = use_signal(|| Option::<RefsState>::None);
    let mut pinned = use_signal(|| false);
    let mut hover_line = use_signal(|| Option::<usize>::None);
    let mut hover_gen = use_signal(|| 0_u64);

    let view_path = reference_path(&finding.path, (*viewing.read()).as_ref())
        .unwrap_or_else(|| finding.path.clone());
    let in_reference = view_path != finding.path;
    let hub_open = hub.is_some_and(|h| h.0.read().is_some());

    let file_key = FileKey {
        repo: finding.repo.clone(),
        sha: sha.clone(),
        path: view_path.clone(),
    };

    // The fetch. A cached file answers without a request; an answer the LRU will not keep is
    // held for this open only. Either way the finding's own block is settled once. The answer
    // carries the key it was fetched for, so a file that is still loading is never drawn under
    // the path of the one before it.
    let file_res = use_resource({
        let finding = finding.clone();
        let sha = sha.clone();
        let project_id = project_id.clone();
        move || {
            let shown = reference_path(&finding.path, (*viewing.read()).as_ref());
            let is_reference = shown.is_some();
            let path = shown.unwrap_or_else(|| finding.path.clone());
            let wanted = around().unwrap_or(finding.line);
            let (repo, line) = (finding.repo.clone(), finding.line);
            let (sha, project_id) = (sha.clone(), project_id.clone());
            let file_key = FileKey {
                repo: repo.clone(),
                sha: sha.clone(),
                path: path.clone(),
            };
            let mut cache = cache;
            let mut session = session;
            async move {
                let result = async {
                    let hit = cache.write().get(&file_key, wanted);
                    let held = session.peek().lookup(&file_key, wanted);
                    let view = if let Some(hit) = hit.or(held) {
                        hit
                    } else {
                        let got = fetch_source_file(
                            &repo,
                            &path,
                            Some(&sha),
                            Some(wanted),
                            Some(&project_id),
                        )
                        .await?;
                        let view = Arc::new(got);
                        let kept = cache.write().put(file_key.clone(), Arc::clone(&view));
                        if !kept {
                            session.write().put(file_key.clone(), Arc::clone(&view));
                        }
                        view
                    };
                    if !is_reference && block.peek().is_none() {
                        let decided = match block_for(&view, line) {
                            Some(b) => b,
                            None => {
                                fetch_block(&repo, &path, line, Some(&sha), Some(&project_id)).await
                            }
                        };
                        block.set(Some(decided));
                    }
                    Some(view)
                }
                .await;
                (file_key, result)
            }
        }
    });

    // Prefetch the next finding's file in this group, once, in the background.
    use_effect({
        let finding = finding.clone();
        let shas = shas.clone();
        let project_id = project_id.clone();
        move || {
            let ordered = ordered_rows(handle);
            let Some((repo, path, line)) =
                prefetch_target(&ordered, &dispositions.peek(), &finding)
            else {
                return;
            };
            let sha = shas.get(&repo).cloned().unwrap_or_default();
            if sha.is_empty() {
                return;
            }
            let fk = FileKey {
                repo: repo.clone(),
                sha: sha.clone(),
                path: path.clone(),
            };
            if cache.peek().lookup(&fk, line).is_some() {
                return;
            }
            let project_id = project_id.clone();
            let mut cache = cache;
            spawn(async move {
                if let Some(got) =
                    fetch_source_file(&repo, &path, Some(&sha), Some(line), Some(&project_id)).await
                {
                    cache.write().put(fk, Arc::new(got));
                }
            });
        }
    });

    // The Rule Hub sits above the viewer; when it closes, the keyboard comes back here.
    let mut hub_was_open = use_signal(|| false);
    use_effect(move || {
        let now = hub.is_some_and(|h| h.0.read().is_some());
        if *hub_was_open.peek() && !now {
            focus_viewer();
        }
        hub_was_open.set(now);
    });

    // What the render shows.
    let wanted = around().unwrap_or(violation_line);
    let kept = cache
        .peek()
        .lookup(&file_key, wanted)
        .or_else(|| session.peek().lookup(&file_key, wanted));
    let file_state = match kept {
        Some(v) => FileStateView::Loaded(v),
        None => match file_res.read().clone() {
            Some((k, Some(v))) if k == file_key => FileStateView::Loaded(v),
            Some((k, None)) if k == file_key => FileStateView::Failed,
            _ => FileStateView::Loading,
        },
    };
    let total_lines = match &file_state {
        FileStateView::Loaded(v) => v.total(),
        _ => 0,
    };

    // The rows, this file's findings and the gutter are memos: they recompute when the table or
    // the dispositions change, and not for anything else (the undo clock, a scroll).
    let table = handle.signal();
    let ordered = use_memo(move || {
        let guard = table.read();
        let rows = chorale_core::filtered_sorted_rows(&guard);
        drop(guard);
        rows
    });
    let items = use_memo({
        let all_findings = all_findings.clone();
        let (repo, own_path) = (finding.repo.clone(), finding.path.clone());
        move || {
            let shown = reference_path(&own_path, (*viewing.read()).as_ref())
                .unwrap_or_else(|| own_path.clone());
            file_findings(
                &all_findings,
                &dispositions.read(),
                &repo,
                &shown,
                &navigable_keys(&ordered.read()),
            )
        }
    });
    let marks = use_memo(move || gutter_marks(&items.read()));
    let position = use_memo({
        let key = key.clone();
        move || finding_position(&items.read(), &key)
    });

    // ── Actions ──

    // Aim the queue's cursor at a finding. The cursor is the only notion of "current". A
    // finding the table cannot show any more is refused with a status line: aiming the cursor
    // at it would clamp to row 0 and the viewer would silently open on something else.
    let goto_finding = Callback::new(move |target: String| -> bool {
        if let Err(why) = reach_finding(handle, &target) {
            status.set(Some(why.to_string()));
            return false;
        }
        cursor.set(RowCursor::focused_on(target));
        status.set(None);
        true
    });

    let scroll_to_line = Callback::new(move |line: usize| {
        let top = scroll_top_for_line(line, CODE_LINE_H_PX, *viewport.peek(), total_lines);
        scroll_top.set(top);
        last_scroll.set(top);
        set_dom_scroll(top);
    });

    // Show `line` of `path`: a scroll in the file on screen, else a switch of file. Never a
    // cursor move: `path` is a place in the code, not a finding.
    let show_location = Callback::new({
        let own_path = finding.path.clone();
        move |(path, line): (String, usize)| {
            let current = reference_path(&own_path, (*viewing.peek()).as_ref())
                .unwrap_or_else(|| own_path.clone());
            line_cursor.set(Some(line));
            match location_plan(&own_path, &current, &path) {
                LocationPlan::Scroll => {
                    let top =
                        scroll_top_for_line(line, CODE_LINE_H_PX, *viewport.peek(), total_lines);
                    scroll_top.set(top);
                    last_scroll.set(top);
                    set_dom_scroll(top);
                }
                // Another file: the scroll container is rebuilt for it (it is keyed by path)
                // and takes its position from the signal when it mounts.
                plan => {
                    let top = scroll_top_for_line(
                        line,
                        CODE_LINE_H_PX,
                        *viewport.peek(),
                        usize::MAX / 64,
                    );
                    scroll_top.set(top);
                    last_scroll.set(top);
                    match plan {
                        LocationPlan::ShowReference(other) => {
                            viewing.set(Some(other));
                            around.set(Some(line));
                        }
                        _ => {
                            viewing.set(None);
                            around.set(None);
                        }
                    }
                }
            }
        }
    });

    // The line the viewer stands on, for the back stack and the permalink.
    let here_line = Callback::new(move |()| -> usize {
        (*line_cursor.peek()).unwrap_or(if viewing.peek().is_some() {
            1
        } else {
            violation_line
        })
    });

    // One path: decide, and if it landed, move on within the file.
    let dispose = Callback::new({
        let key = key.clone();
        let all_findings = all_findings.clone();
        let finding = finding.clone();
        move |(state, reason): (TriageState, String)| {
            let nav = navigable_keys(&ordered_rows(handle));
            let before = file_findings(
                &all_findings,
                &dispositions.peek(),
                &finding.repo,
                &finding.path,
                &nav,
            );
            // The target is chosen BEFORE the write: the decided row leaves the table.
            let target = advance_after_disposition(&before, &key);
            // The pane path's own answer, not an inference from the disposition map.
            let landed = apply_disposition.call((state, reason));
            let outcome = finish_disposition(landed, target);
            if !outcome.close_prompt {
                return;
            }
            prompt.set(None);
            prompt_reason.set(String::new());
            if let Some(t) = outcome.goto {
                goto_finding.call(t);
            }
            focus_viewer();
        }
    });

    // The loaded file, for the search and the symbol pre-fill.
    let loaded_view = match &file_state {
        FileStateView::Loaded(v) => Some(Arc::clone(v)),
        _ => None,
    };

    // The search: matches over the lines the viewer has, and the step to the next one.
    let go_to_match = Callback::new({
        let loaded_view = loaded_view.clone();
        move |state: SearchState| {
            let Some(v) = loaded_view.as_ref() else {
                return;
            };
            let found = search_view(v, &state.needle);
            if let Some((line, _)) = state.current.and_then(|i| found.matches.get(i).copied()) {
                line_cursor.set(Some(line));
                let top = scroll_top_for_line(line, CODE_LINE_H_PX, *viewport.peek(), total_lines);
                scroll_top.set(top);
                last_scroll.set(top);
                set_dom_scroll(top);
            }
        }
    });
    let on_search_input = Callback::new({
        let loaded_view = loaded_view.clone();
        move |text: String| {
            let from = (*line_cursor.peek()).unwrap_or(if viewing.peek().is_some() {
                1
            } else {
                violation_line
            });
            let matches = loaded_view
                .as_ref()
                .map(|v| search_view(v, &text).matches)
                .unwrap_or_default();
            let next = SearchState {
                current: first_match_from(&matches, from),
                needle: text,
                field_open: true,
                wrapped: false,
            };
            search.set(Some(next.clone()));
            go_to_match.call(next);
        }
    });
    let on_search_key = Callback::new({
        let loaded_view = loaded_view.clone();
        move |k: SearchKey| {
            let Some(mut state) = search.peek().clone() else {
                return;
            };
            match k {
                SearchKey::Close => {
                    state.field_open = false;
                    // An empty needle has nothing to leave highlighted.
                    search.set((!state.needle.is_empty()).then_some(state));
                    focus_viewer();
                }
                SearchKey::Next | SearchKey::Previous => {
                    let total = loaded_view
                        .as_ref()
                        .map_or(0, |v| search_view(v, &state.needle).matches.len());
                    if let Some((i, wrapped)) =
                        cycle_match(state.current, total, k == SearchKey::Next)
                    {
                        state.current = Some(i);
                        state.wrapped = wrapped;
                        search.set(Some(state.clone()));
                        go_to_match.call(state);
                    }
                }
            }
        }
    });

    // Open the symbol prompt, pre-filled from what the detector captured or the violation line.
    let open_symbols = Callback::new({
        let finding = finding.clone();
        let loaded_view = loaded_view.clone();
        move |()| {
            let line_text = if viewing.peek().is_none() {
                loaded_view
                    .as_ref()
                    .and_then(|v| v.line(violation_line).map(|s| segments_text(s)))
            } else {
                None
            }
            .unwrap_or_else(|| finding.snippet.clone());
            let text = symbol_prefill(&finding.captures, &line_text).unwrap_or_default();
            symbol_prompt.set(Some((text, None)));
        }
    });
    let run_symbols = Callback::new({
        let (repo, project_id, sha) = (finding.repo.clone(), project_id.clone(), sha.clone());
        move |()| {
            let text = symbol_prompt
                .peek()
                .as_ref()
                .map(|(t, _)| t.trim().to_string())
                .unwrap_or_default();
            if !camerata_api_types::symbol::is_valid_symbol(&text) {
                symbol_prompt.set(Some((
                    text,
                    Some("Letters, digits and _ $ - . : only, up to 128".to_string()),
                )));
                return;
            }
            symbol_prompt.set(None);
            refs.set(Some(RefsState {
                symbol: text.clone(),
                load: RefsLoad::Loading,
                selected: 0,
            }));
            let (repo, project_id, sha) = (repo.clone(), project_id.clone(), sha.clone());
            spawn(async move {
                let got = fetch_symbol_refs(&repo, &text, Some(&sha), Some(&project_id)).await;
                let mut state = refs.peek().clone();
                // A newer search replaced this one, or the list was closed: drop the answer.
                if let Some(s) = state.as_mut().filter(|s| s.symbol == text) {
                    s.load = match got {
                        Some(v) => RefsLoad::Loaded(Arc::new(v)),
                        None => RefsLoad::Failed,
                    };
                    refs.set(state);
                }
            });
        }
    });
    // Open a hit: push where the curator is onto the back stack, then show the hit's place. The
    // list closes and the keys return to the code, so Backspace comes back.
    let open_hit = Callback::new({
        let (key, own_path) = (key.clone(), finding.path.clone());
        move |hit: RefHit| {
            let from_path = reference_path(&own_path, (*viewing.peek()).as_ref())
                .unwrap_or_else(|| own_path.clone());
            push_history(
                &mut history.write(),
                HistoryEntry {
                    key: key.clone(),
                    path: from_path,
                    line: here_line.call(()),
                },
            );
            refs.set(None);
            search.set(None);
            show_location.call((hit.path, hit.line));
            focus_viewer();
        }
    });

    // The permalink of the place on screen.
    let copy_permalink = Callback::new({
        let (repo, own_path, sha) = (finding.repo.clone(), finding.path.clone(), sha.clone());
        let origins = origins.clone();
        let loaded_view = loaded_view.clone();
        move |()| {
            let path = reference_path(&own_path, (*viewing.peek()).as_ref())
                .unwrap_or_else(|| own_path.clone());
            // A permalink names a place in a commit. When the code on screen is the working tree
            // (the commit is not in this clone, or none was recorded) its line numbers are not
            // that commit's, so there is nothing true to link to.
            let Some(read) = loaded_view.as_ref().filter(|v| v.source == "git") else {
                status.set(Some(
                    "No inspected commit was recorded, so there is no permalink.".to_string(),
                ));
                return;
            };
            // The commit the viewer actually read when it was not given one.
            let sha = if sha.is_empty() {
                read.commit.clone()
            } else {
                sha.clone()
            };
            let line = here_line.call(());
            let Some(link) = permalink(
                &repo,
                &sha,
                &path,
                line,
                origins.get(&repo).map(String::as_str),
            ) else {
                status.set(Some(
                    "No inspected commit was recorded, so there is no permalink.".to_string(),
                ));
                return;
            };
            let text = link.clipboard_text();
            spawn(async move {
                if copy_text(&text).await {
                    status.set(Some("Copied the permalink.".to_string()));
                    if let Some(t) = toasts {
                        crate::toast::push_toast(
                            t,
                            crate::toast::ToastKind::Info,
                            "Copied the permalink.",
                        );
                    }
                } else {
                    status.set(Some("Could not copy the permalink.".to_string()));
                    if let Some(t) = toasts {
                        crate::toast::push_toast(
                            t,
                            crate::toast::ToastKind::Error,
                            "Could not copy the permalink.",
                        );
                    }
                }
            });
        }
    });

    let on_key = Callback::new({
        let key = key.clone();
        let finding = finding.clone();
        let all_for_keys = all_findings.clone();
        let loaded_view = loaded_view.clone();
        move |(k, m): (String, KeyMods)| {
            let focus = viewer_focus(OpenSurfaces {
                reason_prompt: prompt.peek().is_some(),
                goto: goto.peek().is_some(),
                search: search.peek().as_ref().is_some_and(|s| s.field_open),
                symbol_prompt: symbol_prompt.peek().is_some(),
                refs_list: refs.peek().is_some(),
                rule_hub: hub.is_some_and(|h| h.0.peek().is_some()),
            });
            if camerata_ui_core::viewer::closes_hub(focus, &k, m) {
                if let Some(mut h) = hub {
                    h.0.set(None);
                }
                return;
            }
            let Some(cmd) = viewer_command(focus, &k, m) else {
                return;
            };
            if viewing.peek().is_some() && !allowed_in_reference_view(cmd) {
                status.set(Some(
                    "A reference is on screen, not the finding. Backspace returns to it."
                        .to_string(),
                ));
                return;
            }
            if clears_search(cmd) {
                search.set(None);
            }
            match cmd {
                ViewerCommand::Close => {
                    let help_open = *help.peek();
                    match close_target(help_open) {
                        CloseTarget::Help => help.set(false),
                        CloseTarget::Viewer => on_close.call(()),
                    }
                }
                ViewerCommand::Dispose(state) => dispose.call((state, String::new())),
                ViewerCommand::PromptFor(state) => {
                    prompt.set(Some(state));
                    prompt_reason.set(String::new());
                }
                ViewerCommand::MoveRow(dir) => {
                    let next = cursor.peek().moved(&visible_keys.peek(), dir);
                    cursor.set(next);
                    status.set(None);
                }
                ViewerCommand::FindingInFile(dir) => {
                    let nav = navigable_keys(&ordered_rows(handle));
                    let items = file_findings(
                        &all_for_keys,
                        &dispositions.peek(),
                        &finding.repo,
                        &finding.path,
                        &nav,
                    );
                    match step_in_file(&items, &key, dir) {
                        FileStep::Move(t) => {
                            goto_finding.call(t);
                        }
                        FileStep::Stay(msg) => status.set(Some(msg)),
                    }
                }
                ViewerCommand::FileInGroup(dir) => {
                    let ordered = ordered_rows(handle);
                    match step_file_in_group(&ordered, &dispositions.peek(), &finding, dir) {
                        FileStep::Move(t) => {
                            goto_finding.call(t);
                        }
                        FileStep::Stay(msg) => status.set(Some(msg)),
                    }
                }
                ViewerCommand::BackToFinding => {
                    line_cursor.set(None);
                    if viewing.peek().is_some() {
                        viewing.set(None);
                        around.set(None);
                        let top = scroll_top_for_line(
                            violation_line,
                            CODE_LINE_H_PX,
                            *viewport.peek(),
                            usize::MAX / 64,
                        );
                        scroll_top.set(top);
                        last_scroll.set(top);
                    } else {
                        scroll_to_line.call(violation_line);
                    }
                }
                ViewerCommand::OpenGoToLine => {
                    goto.set(Some(String::new()));
                    goto_error.set(None);
                }
                ViewerCommand::Back => {
                    let step = back_step(&mut history.write(), &key, &finding.path);
                    match step {
                        BackStep::Line(line) => {
                            show_location.call((finding.path.clone(), line));
                        }
                        BackStep::Location { path, line } => {
                            show_location.call((path, line));
                        }
                        BackStep::Finding(target) => {
                            goto_finding.call(target);
                        }
                        BackStep::Nothing => {
                            status.set(Some("Nothing to go back to.".to_string()));
                        }
                    }
                }
                ViewerCommand::Scroll(sk) => {
                    let top = scrolled(
                        *last_scroll.peek(),
                        sk,
                        *viewport.peek(),
                        total_lines,
                        CODE_LINE_H_PX,
                    );
                    scroll_top.set(top);
                    last_scroll.set(top);
                    set_dom_scroll(top);
                }
                ViewerCommand::ToggleHelp => {
                    let now = *help.peek();
                    help.set(!now);
                }
                ViewerCommand::OpenSearch => {
                    if loaded_view.is_none() {
                        status.set(Some("There is no code to search.".to_string()));
                    } else {
                        let mut state = search.peek().clone().unwrap_or_default();
                        state.field_open = true;
                        search.set(Some(state));
                    }
                }
                ViewerCommand::OpenSymbols => open_symbols.call(()),
                ViewerCommand::CopyPermalink => copy_permalink.call(()),
                ViewerCommand::TogglePopover => {
                    let now = *pinned.peek();
                    pinned.set(!now);
                }
                ViewerCommand::OpenHub => {
                    on_open_hub.call(RuleHubRequest {
                        repo: finding.repo.clone(),
                        rule_id: finding.rule_id.clone(),
                    });
                }
                ViewerCommand::RefsMove(dir) => {
                    let mut state = refs.peek().clone();
                    if let Some(s) = state.as_mut() {
                        if let RefsLoad::Loaded(v) = &s.load {
                            s.selected = move_selection(s.selected, v.hits.len(), dir);
                        }
                    }
                    refs.set(state);
                    follow_selected_ref();
                }
                ViewerCommand::RefsOpen => {
                    let hit = refs.peek().as_ref().and_then(|s| match &s.load {
                        RefsLoad::Loaded(v) if v.outcome() == RefsOutcome::List => {
                            v.hits.get(s.selected).cloned()
                        }
                        _ => None,
                    });
                    match hit {
                        Some(hit) => open_hit.call(hit),
                        None => status.set(Some("There is no reference to open.".to_string())),
                    }
                }
                ViewerCommand::RefsClose => {
                    refs.set(None);
                    focus_viewer();
                }
            }
        }
    });

    let on_scroll = Callback::new({
        let file_state = file_state.clone();
        move |(top, client_h): (f64, f64)| {
            last_scroll.set(top);
            // Re-render only when the first rendered line would change.
            let step = |t: f64| line_at_scroll(t, CODE_LINE_H_PX) / SCROLL_QUANTUM_LINES;
            if step(top) != step(*scroll_top.peek()) {
                scroll_top.set(top);
            }
            if client_h > 50.0 && (client_h - *viewport.peek()).abs() > 1.0 {
                viewport.set(client_h);
            }
            // A file delivered as a window: ask for a new one when the viewport nears an edge,
            // once the scroll has rested. Asking on every event would cancel the fetch in
            // flight each time.
            let FileStateView::Loaded(v) = &file_state else {
                return;
            };
            let Some(w) = v.window.as_ref() else {
                return;
            };
            match page_request(w, top, *viewport.peek(), CODE_LINE_H_PX) {
                Some(centre) => {
                    page_target.set(Some(centre));
                    if !*page_timer.peek() {
                        page_timer.set(true);
                        spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_millis(PAGE_DEBOUNCE_MS))
                                .await;
                            page_timer.set(false);
                            let want = *page_target.peek();
                            if want.is_some() && *around.peek() != want {
                                around.set(want);
                            }
                        });
                    }
                }
                None => page_target.set(None),
            }
        }
    });

    // The scroll container just mounted: put it where the signal says, then refine with the
    // real viewport height so the violation line sits in the upper third.
    let on_scroll_ready = Callback::new(move |()| {
        set_dom_scroll(*scroll_top.peek());
        spawn(async move {
            let probe =
                format!("var e=document.getElementById('{SCROLL_ID}');return e?e.clientHeight:0;");
            let Ok(value) = dioxus::document::eval(&probe).await else {
                return;
            };
            let Some(h) = value.as_f64().filter(|h| *h > 100.0) else {
                return;
            };
            viewport.set(h);
            // Only re-aim at the finding if the curator has not moved within the file yet.
            if line_cursor.peek().is_none() && viewing.peek().is_none() {
                let top = scroll_top_for_line(violation_line, CODE_LINE_H_PX, h, total_lines);
                scroll_top.set(top);
                last_scroll.set(top);
                set_dom_scroll(top);
            }
        });
    });

    let commit_goto = Callback::new({
        let (key, own_path) = (key.clone(), finding.path.clone());
        move |()| {
            let text = goto.peek().clone().unwrap_or_default();
            match parse_goto_line(&text, total_lines) {
                Ok(line) => {
                    let from_path = reference_path(&own_path, (*viewing.peek()).as_ref())
                        .unwrap_or_else(|| own_path.clone());
                    push_history(
                        &mut history.write(),
                        HistoryEntry {
                            key: key.clone(),
                            path: from_path.clone(),
                            line: here_line.call(()),
                        },
                    );
                    show_location.call((from_path, line));
                    goto.set(None);
                    goto_error.set(None);
                    focus_viewer();
                }
                Err(msg) => goto_error.set(Some(msg)),
            }
        }
    });

    // The popover: for the finding on the hovered line, else (pinned) for this finding.
    let on_hover = Callback::new(move |line: Option<usize>| {
        let generation = *hover_gen.peek() + 1;
        hover_gen.set(generation);
        match line {
            Some(l) => {
                spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(HOVER_DELAY_MS)).await;
                    if *hover_gen.peek() == generation {
                        hover_line.set(Some(l));
                    }
                });
            }
            None => {
                spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(HOVER_GRACE_MS)).await;
                    if *hover_gen.peek() == generation {
                        hover_line.set(None);
                    }
                });
            }
        }
    });
    let popover_line = hover_line();
    let popover = (pinned() || popover_line.is_some())
        .then(|| {
            let items_now = items.read();
            let target_key = match popover_line {
                Some(l) if in_reference || l != violation_line => items_now
                    .iter()
                    .find(|i| i.line == l)
                    .map(|i| i.key.clone()),
                _ => Some(key.clone()),
            }?;
            let f = all_findings.iter().find(|f| finding_key(f) == target_key)?;
            // The "related in this file" list is the finding's own file's.
            let own_items;
            let related_from: &[camerata_ui_core::viewer::FileFinding] = if in_reference {
                own_items = file_findings(
                    &all_findings,
                    &dispositions.peek(),
                    &finding.repo,
                    &finding.path,
                    &navigable_keys(&ordered.peek()),
                );
                &own_items
            } else {
                &items_now
            };
            Some(popover_view(
                f,
                finding_state(&dispositions.peek(), f),
                descriptions.get(&f.rule_id).map(String::as_str),
                Authority::of(f, super::rules::is_enforced_floor(&f.rule_id)),
                related_from,
                // Decision memory (B.5) is not built: the popover has the seam and no data yet.
                None,
            ))
        })
        .flatten();

    // The search as the component draws it.
    let search_props = search().map(|s| {
        let result = loaded_view
            .as_ref()
            .filter(|_| !s.needle.is_empty())
            .map(|v| search_view(v, &s.needle))
            .unwrap_or_default();
        SearchProps {
            status: search_status(&s.needle, &result, s.current, s.wrapped),
            needle_chars: result.needle_chars,
            matches: Arc::new(result.matches),
            current: s.current,
            field_open: s.field_open,
            needle: s.needle,
        }
    });

    rsx! {
        CodeViewer {
            finding: ViewerFinding::from_finding(&finding, &sha, sha_conflict),
            finding_key: key.clone(),
            file: file_state,
            block: if in_reference { None } else { block().flatten() },
            marks: marks(),
            position: position(),
            scroll_top: scroll_top(),
            viewport_px: viewport(),
            line_cursor: line_cursor(),
            viewing: in_reference.then(|| view_path.clone()),
            goto: goto(),
            goto_error: goto_error(),
            search: search_props,
            symbol_prompt: symbol_prompt()
                .map(|(text, error)| SymbolPromptProps { text, error }),
            refs: refs()
                .map(|r| RefsProps {
                    symbol: r.symbol,
                    load: r.load,
                    selected: r.selected,
                }),
            popover,
            pinned: pinned(),
            hub_open,
            prompt: prompt(),
            prompt_reason: prompt_reason(),
            status: status(),
            help: help(),
            on_key,
            on_close,
            on_scroll,
            on_scroll_ready,
            on_goto_input: Callback::new(move |v: String| goto.set(Some(v))),
            on_goto_commit: commit_goto,
            on_goto_cancel: Callback::new(move |()| {
                goto.set(None);
                goto_error.set(None);
                focus_viewer();
            }),
            on_search_input,
            on_search_key,
            on_symbol_input: Callback::new(move |v: String| symbol_prompt.set(Some((v, None)))),
            on_symbol_commit: run_symbols,
            on_symbol_cancel: Callback::new(move |()| {
                symbol_prompt.set(None);
                focus_viewer();
            }),
            on_ref_pick: Callback::new(move |i: usize| {
                let hit = refs.peek().as_ref().and_then(|s| match &s.load {
                    RefsLoad::Loaded(v) => v.hits.get(i).cloned(),
                    _ => None,
                });
                if let Some(hit) = hit {
                    open_hit.call(hit);
                }
            }),
            on_hover,
            on_popover_hub: Callback::new({
                let repo = finding.repo.clone();
                move |rule_id: String| {
                    on_open_hub.call(RuleHubRequest {
                        repo: repo.clone(),
                        rule_id,
                    });
                }
            }),
            popover_line,
            on_popover_goto: Callback::new(move |target: String| {
                goto_finding.call(target);
            }),
            on_prompt_reason: Callback::new(move |v: String| prompt_reason.set(v)),
            on_prompt_commit: Callback::new(move |()| {
                if let Some(state) = *prompt.peek() {
                    dispose.call((state, prompt_reason.peek().clone()));
                }
            }),
            on_prompt_cancel: Callback::new(move |()| {
                prompt.set(None);
                prompt_reason.set(String::new());
                focus_viewer();
            }),
            on_mark: Callback::new(move |t: String| {
                goto_finding.call(t);
            }),
            on_line: Callback::new(move |n: usize| line_cursor.set(Some(n))),
            on_open_modal,
            ViewerUndoBar { undo_entry, now_ms, on_undo }
        }
    }
}

#[cfg(test)]
mod render_tests {
    //! Tier-1 render tests (docs/UI_TESTING.md). `CodeViewer` is props-only, so each state is a
    //! plain props value. SSR cannot press a key or scroll; what it CAN do is read which state
    //! the viewer renders for a response, how many lines it puts in the DOM, and which finding
    //! the viewer and the pane each say they are on.

    use super::*;
    use camerata_ui_core::viewer::WindowView;

    /// Everything the viewer takes, with sensible defaults.
    #[derive(Clone, PartialEq)]
    struct Case {
        file: FileStateView,
        marks: Vec<GutterMark>,
        position: (usize, usize),
        scroll_top: f64,
        prompt: Option<TriageState>,
        goto: Option<String>,
        help: bool,
        status: Option<String>,
        line_cursor: Option<usize>,
        /// The finding's own block. `None` falls back to the loaded view's block.
        block: Option<BlockView>,
        sha_conflict: bool,
        viewing: Option<String>,
        search: Option<SearchProps>,
        symbol_prompt: Option<SymbolPromptProps>,
        refs: Option<RefsProps>,
        popover: Option<PopoverView>,
        pinned: bool,
        hub_open: bool,
    }

    impl Default for Case {
        fn default() -> Self {
            Self {
                file: FileStateView::Loading,
                marks: Vec::new(),
                position: (0, 0),
                scroll_top: 0.0,
                prompt: None,
                goto: None,
                help: false,
                status: None,
                line_cursor: None,
                block: None,
                sha_conflict: false,
                viewing: None,
                search: None,
                symbol_prompt: None,
                refs: None,
                popover: None,
                pinned: false,
                hub_open: false,
            }
        }
    }

    fn finding() -> ViewerFinding {
        ViewerFinding {
            repo: "acme/app".into(),
            path: "src/db.rs".into(),
            rule_id: "SQL-INJECTION-1".into(),
            severity: "high".into(),
            line: 3,
            snippet: "let q = format!(\"select {}\", id);".into(),
            sha7: "abc1234".into(),
            sha_conflict: false,
        }
    }

    fn harness(case: Case) -> Element {
        let block = case.block.clone().or_else(|| match &case.file {
            FileStateView::Loaded(v) => v.block.clone(),
            _ => None,
        });
        let mut f = finding();
        f.sha_conflict = case.sha_conflict;
        rsx! {
            CodeViewer {
                finding: f,
                finding_key: "the-key".to_string(),
                block,
                file: case.file,
                marks: case.marks,
                position: case.position,
                scroll_top: case.scroll_top,
                prompt: case.prompt,
                goto: case.goto,
                help: case.help,
                status: case.status,
                line_cursor: case.line_cursor,
                viewing: case.viewing,
                search: case.search,
                symbol_prompt: case.symbol_prompt,
                refs: case.refs,
                popover: case.popover,
                pinned: case.pinned,
                hub_open: case.hub_open,
            }
        }
    }

    fn render(case: Case) -> String {
        let mut vdom = VirtualDom::new_with_props(harness, case);
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    }

    /// A response the way the server shapes it, for a file of `lines` lines `line N`.
    fn view(lines: usize, patch: impl FnOnce(&mut serde_json::Value)) -> Arc<SourceFileView> {
        let body: Vec<serde_json::Value> = (1..=lines)
            .map(|n| {
                if n == 1 {
                    serde_json::json!([[0, "fn"], [null, " main() {"]])
                } else if n == 2 {
                    serde_json::json!([[null, "    // note "], [2, "// c"]])
                } else {
                    serde_json::json!([[null, format!("    line {n}")]])
                }
            })
            .collect();
        let mut v = serde_json::json!({
            "status": "ok", "source": "git", "commit": "abc1234def5678900000000000000000000000ff",
            "head_differs": false, "language": "rust", "total_lines": lines,
            "window": {"from": 1, "to": lines, "total_lines": lines},
            "highlight": "ok",
            "classes": ["keyword", "string", "comment", "number", "function", "type", "variable",
                        "property", "punctuation", "operator"],
            "block": {"start_line": 2, "end_line": 4, "kind": "function"},
            "lines": body,
        });
        patch(&mut v);
        Arc::new(serde_json::from_value(v).expect("response"))
    }

    fn refusal(json: serde_json::Value) -> FileStateView {
        FileStateView::Loaded(Arc::new(serde_json::from_value(json).expect("response")))
    }

    fn count(html: &str, needle: &str) -> usize {
        html.matches(needle).count()
    }

    /// The number of `.cv-line` nodes (not `.cv-lines`, the container).
    fn line_nodes(html: &str) -> usize {
        count(html, "class=\"cv-line\"") + count(html, "class=\"cv-line ")
    }

    #[test]
    fn loading_shows_the_header_and_no_code() {
        let html = render(Case::default());
        assert!(html.contains("acme/app"), "{html}");
        assert!(html.contains("@ abc1234"), "{html}");
        assert!(html.contains("src/db.rs"));
        assert!(html.contains("Loading"));
        assert_eq!(line_nodes(&html), 0);
    }

    #[test]
    fn a_failed_request_says_so_and_keeps_the_stored_evidence() {
        let html = render(Case {
            file: FileStateView::Failed,
            ..Case::default()
        });
        assert!(html.contains("did not answer"));
        assert!(html.contains("select"), "the stored snippet is the floor");
        assert_eq!(line_nodes(&html), 0);
    }

    #[test]
    fn code_renders_numbered_highlighted_lines_with_the_band_and_the_hit() {
        let html = render(Case {
            file: FileStateView::Loaded(view(6, |_| {})),
            position: (2, 4),
            ..Case::default()
        });
        assert_eq!(line_nodes(&html), 6);
        // Header: revision from the response, line label, finding position.
        assert!(html.contains("@ abc1234"));
        assert!(html.contains("line 3 of 6"), "{html}");
        assert!(html.contains("finding 2 of 4 in this file"));
        // Token classes come from the response's class list.
        assert!(html.contains("tok-keyword"));
        assert!(html.contains("tok-comment"));
        // The enclosing block (lines 2..=4) is banded; the violation line (3) is the hit with
        // the severity class; line 1 and 5 are neither.
        assert_eq!(count(&html, "cv-line-band"), 3);
        assert_eq!(count(&html, "cv-line-hit cv-sev-high"), 1);
        assert!(
            html.contains("class=\"cv-line\""),
            "an unbanded line exists"
        );
        // No banner for a clean read of the inspected commit.
        assert!(!html.contains("cv-banner"));
        // Gutter line numbers.
        assert!(html.contains(">1</span>") && html.contains(">6</span>"));
    }

    #[test]
    fn the_working_tree_gets_the_amber_banner_with_the_responses_own_words() {
        // Commit not available in this clone.
        let html = render(Case {
            file: FileStateView::Loaded(view(4, |v| {
                v["status"] = "commit_unavailable".into();
                v["source"] = "working_tree".into();
                v["reason"] =
                    "Inspected commit not available in this clone; showing the working tree".into();
            })),
            ..Case::default()
        });
        assert!(html.contains("cv-banner-amber"), "{html}");
        assert!(
            html.contains("Inspected commit not available in this clone; showing the working tree")
        );
        assert!(
            html.contains("@ working tree"),
            "the header must not claim a commit"
        );
        assert_eq!(line_nodes(&html), 4, "the working tree is shown");

        // No commit was recorded: status is a plain `ok`, the banner keys on `source`.
        let html = render(Case {
            file: FileStateView::Loaded(view(4, |v| {
                v["source"] = "working_tree".into();
                v["commit"] = "".into();
            })),
            ..Case::default()
        });
        assert!(html.contains("cv-banner-amber"));
        assert!(html.contains("No inspected commit was recorded"));
    }

    #[test]
    fn a_checkout_that_moved_on_is_a_muted_note_not_the_amber_banner() {
        let html = render(Case {
            file: FileStateView::Loaded(view(4, |v| v["head_differs"] = true.into())),
            ..Case::default()
        });
        assert!(html.contains("cv-banner-muted"));
        assert!(!html.contains("cv-banner-amber"));
    }

    #[test]
    fn a_deleted_source_says_so_with_the_date_and_shows_no_code() {
        let html = render(Case {
            file: refusal(serde_json::json!({
                "status": "source_deleted", "source": "git",
                "reason": "Source deleted on 24 Oct 2026, re-fetch to view",
                "deleted_at": "2026-10-24T17:40:19Z", "record_id": "r1"
            })),
            ..Case::default()
        });
        assert!(
            html.contains("Source deleted on 24 Oct 2026, re-fetch to view"),
            "{html}"
        );
        assert_eq!(line_nodes(&html), 0);
        assert!(
            !html.contains("select {}"),
            "not even the stored evidence line"
        );
        assert!(!html.contains("cv-banner"));
    }

    #[test]
    fn a_file_too_large_to_view_offers_the_enclosing_block_instead() {
        let html = render(Case {
            file: refusal(serde_json::json!({
                "status": "too_large", "source": "git",
                "reason": "file is too large to view here (over 4 MB, or too slow to prepare)"
            })),
            ..Case::default()
        });
        assert!(html.contains("too large to view here"));
        assert!(html.contains("Show the enclosing block"));
        assert!(html.contains("select"), "the stored snippet is the floor");
        assert_eq!(line_nodes(&html), 0);
    }

    #[test]
    fn a_missing_or_binary_file_gives_its_reason_and_the_floor_but_no_modal_offer() {
        for (status, reason) in [
            (
                "file_missing",
                "file not found \u{2014} changed since scan?",
            ),
            ("not_utf8", "file is not valid UTF-8 (binary?)"),
            (
                "path_traversal",
                "requested path escapes the repo directory",
            ),
        ] {
            let html = render(Case {
                file: refusal(serde_json::json!({"status": status, "reason": reason})),
                ..Case::default()
            });
            assert!(
                html.contains("select"),
                "{status}: the stored snippet is the floor"
            );
            assert!(!html.contains("Show the enclosing block"), "{status}");
            assert_eq!(line_nodes(&html), 0, "{status}");
        }
        let html = render(Case {
            file: refusal(
                serde_json::json!({"status": "file_missing", "reason": "file not found"}),
            ),
            ..Case::default()
        });
        assert!(html.contains("file not found"));
    }

    /// The opening tags of the gutter dots (`<button class="cv-mark ...>`), nothing else.
    fn gutter_dots(html: &str) -> Vec<&str> {
        html.split("<button class=\"cv-mark ")
            .skip(1)
            .map(|rest| &rest[..rest.find('>').unwrap_or(rest.len())])
            .collect()
    }

    fn mark(line: usize, severity: &str, decided: bool, count: usize) -> GutterMark {
        GutterMark {
            line,
            count,
            severity: severity.into(),
            decided,
            target: (!decided).then(|| format!("k{line}")),
            title: format!("RULE-{line}"),
        }
    }

    #[test]
    fn the_gutter_shows_every_finding_with_decided_ones_hollow() {
        let marks = vec![
            mark(10, "critical", false, 1),
            mark(25, "high", true, 1),
            mark(40, "medium", false, 2),
            mark(90, "low", true, 1),
        ];
        let html = render(Case {
            file: FileStateView::Loaded(view(120, |v| v["block"] = serde_json::Value::Null)),
            marks,
            ..Case::default()
        });
        // Four gutter dots in the code area, two of them hollow. (The minimap repeats them with
        // its own class.)
        assert_eq!(count(&html, "class=\"cv-mark cv-sev-"), 4, "{html}");
        assert_eq!(count(&html, "cv-mark cv-sev-high cv-mark-hollow"), 1);
        assert_eq!(count(&html, "cv-mark cv-sev-low cv-mark-hollow"), 1);
        assert_eq!(count(&html, "cv-mark cv-sev-critical\""), 1);
        // A line with two findings carries a count badge; the decided dots are not clickable.
        assert_eq!(count(&html, "class=\"cv-count\""), 1);
        // Exactly the two decided dots are disabled, and they are the hollow ones.
        let dots = gutter_dots(&html);
        assert_eq!(dots.len(), 4);
        let disabled: Vec<&&str> = dots.iter().filter(|d| d.contains("disabled")).collect();
        assert_eq!(disabled.len(), 2, "{dots:?}");
        assert!(
            disabled.iter().all(|d| d.contains("cv-mark-hollow")),
            "{dots:?}"
        );
        assert!(dots
            .iter()
            .filter(|d| !d.contains("cv-mark-hollow"))
            .all(|d| !d.contains("disabled")));
        // The minimap has the same four markers plus the current finding's tick.
        assert_eq!(count(&html, "class=\"cv-mini cv-sev-"), 4);
        assert_eq!(count(&html, "cv-mini-cur"), 1);
    }

    #[test]
    fn a_25000_line_file_renders_under_500_lines_at_any_scroll_position() {
        let file = FileStateView::Loaded(view(25_000, |_| {}));
        let line_h = f64::from(CODE_LINE_H_PX);
        for top in [
            0.0,
            1_000.0,
            12_000.0 * line_h,
            24_000.0 * line_h,
            25_000.0 * line_h + 9_999.0,
        ] {
            let html = render(Case {
                file: file.clone(),
                scroll_top: top,
                ..Case::default()
            });
            let n = line_nodes(&html);
            assert!(n > 0 && n < 500, "{n} lines at scrollTop {top}");
            // The scroll height stays the whole file: padding stands in for what is not drawn.
            assert!(html.contains("padding-top:"), "{html}");
        }
    }

    #[test]
    fn a_windowed_file_draws_only_the_lines_it_carries() {
        // 5,000-line file delivered as lines 1000..1799.
        let lines: Vec<serde_json::Value> = (1000..=1799)
            .map(|n| serde_json::json!([[null, format!("L{n}")]]))
            .collect();
        let file = FileStateView::Loaded(view(0, |v| {
            v["total_lines"] = 5000.into();
            v["window"] = serde_json::json!({"from": 1000, "to": 1799, "total_lines": 5000});
            v["block"] = serde_json::Value::Null;
            v["lines"] = lines.into();
        }));
        // Scrolled to line 4500's neighbourhood: nothing of the window is in range.
        let html = render(Case {
            file: file.clone(),
            scroll_top: 20.0 * 4500.0,
            ..Case::default()
        });
        assert_eq!(line_nodes(&html), 0);
        // Scrolled onto the window.
        let html = render(Case {
            file,
            scroll_top: 20.0 * 1300.0,
            ..Case::default()
        });
        let n = line_nodes(&html);
        assert!(n > 0 && n < 500, "{n}");
        assert!(html.contains("L1300"));
        assert!(!html.contains(">L999<") && !html.contains(">L1800<"));
    }

    #[test]
    fn a_line_over_2000_chars_is_cut_with_its_remaining_count() {
        let file = FileStateView::Loaded(view(3, |v| {
            v["lines"][2] = serde_json::json!([[null, "z".repeat(2500)]]);
        }));
        let html = render(Case {
            file,
            ..Case::default()
        });
        assert!(html.contains("... (500 more chars)"), "{html}");
        assert!(!html.contains(&"z".repeat(2001)));
    }

    #[test]
    fn an_open_reason_prompt_turns_the_keys_off_and_asks_for_the_reason() {
        let html = render(Case {
            file: FileStateView::Loaded(view(4, |_| {})),
            prompt: Some(TriageState::FalsePositive),
            ..Case::default()
        });
        assert!(html.contains("Reason for"));
        assert!(html.contains("False positive"));
        assert!(html.contains("<textarea"));
        assert!(html.contains("cv-keys off"));
        assert!(html.contains("Keys are off while you type"));
        // Without a prompt the keys are on and the strip lists them.
        let html = render(Case {
            file: FileStateView::Loaded(view(4, |_| {})),
            ..Case::default()
        });
        assert!(html.contains("cv-keys armed"));
        assert!(html.contains("C confirm"));
        assert!(!html.contains("<textarea"));
    }

    #[test]
    fn go_to_line_help_and_status_render_when_asked() {
        let html = render(Case {
            file: FileStateView::Loaded(view(4, |_| {})),
            goto: Some("12".into()),
            help: true,
            status: Some("Last finding in this file. Press } for the next file.".into()),
            line_cursor: Some(2),
            ..Case::default()
        });
        assert!(html.contains("Go to line"));
        assert!(html.contains("cv-goto-input"));
        assert!(html.contains("cv-keys off"), "the goto field owns the keys");
        assert!(html.contains("cv-help"));
        for row in KEY_ROWS.iter() {
            assert!(html.contains(row.action), "{}", row.action);
        }
        assert!(html.contains("Last finding in this file"));
        assert!(html.contains("cv-line-cursor"));
        assert!(
            html.contains("line 2 of 4"),
            "the header follows the line cursor"
        );
    }

    /// The pane and the viewer are two views of ONE `RowCursor`: with the cursor on the second
    /// row, both name the second row's finding. The harness mounts them the way `FindingsTable`
    /// and the results root do: the pane from the cursor-resolved row, the viewer from a link
    /// holding the same cursor and visible-key memo.
    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn the_viewer_and_the_pane_name_the_same_finding_as_the_queues_cursor() {
        use super::super::scan::{finding_table_state, DetailPaneRow, FindingDetailPane};
        use camerata_ui_core::triage::PaneFocus;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // The mounted viewer asks for the file; answer, so nothing leaves the test.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/onboard/source-file"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"status": "file_missing", "reason": "file not found"}),
            ))
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

        fn finding(rule: &str, line: usize) -> FindingView {
            serde_json::from_value(serde_json::json!({
                "repo": "me/api", "path": "src/x.rs", "line": line, "rule_id": rule,
                "severity": "high", "snippet": format!("s{line}"), "detail": "d",
                "status": "active", "kind": "defect"
            }))
            .expect("fixture")
        }
        fn rows() -> Vec<FindingView> {
            vec![
                finding("SQL-INJECTION-1", 4),
                finding("SQL-INJECTION-1", 9),
                finding("SQL-INJECTION-1", 30),
            ]
        }
        fn app() -> Element {
            let all = rows();
            let table_rows: Vec<_> = all
                .iter()
                .map(|f| (chorale_core::RowId::new(), f.clone()))
                .collect();
            let handle = chorale_dioxus::use_table({
                let table_rows = table_rows.clone();
                move || finding_table_state(table_rows.clone(), vec!["me/api".to_string()], false)
            });
            let dispositions = use_signal(HashMap::<String, Disposition>::new);
            // The cursor is on the SECOND row, not the first.
            let cursor = use_signal({
                let second = finding_key(&all[1]);
                move || RowCursor::focused_on(second.clone())
            });
            let table = handle.signal();
            let visible_keys = use_memo(move || {
                chorale_core::visible_grouped_view(&table.read())
                    .into_iter()
                    .filter_map(|r| match r {
                        chorale_core::GroupedRow::Data(_, f) => Some(finding_key(&f)),
                        _ => None,
                    })
                    .collect::<Vec<String>>()
            });
            let open = use_signal(|| true);
            let link = ViewerLink {
                handle,
                cursor,
                visible_keys,
                dispositions,
                all_findings: Arc::new(all.clone()),
                shas: Arc::new(HashMap::new()),
                sha_conflicts: Arc::new(HashSet::new()),
                project_id: String::new(),
                open,
                undo_entry: use_signal(|| None),
                now_ms: use_signal(|| 0),
                on_undo: Callback::default(),
                apply_disposition: Callback::default(),
                on_close: Callback::default(),
                on_open_modal: Callback::default(),
                origins: Arc::new(HashMap::new()),
                descriptions: Arc::new(HashMap::new()),
                on_open_hub: Callback::default(),
            };
            use_context_provider(|| ViewerSlot(Signal::new(Some(link))));
            let resolved = cursor.read().resolve(&visible_keys());
            let focused = resolved
                .key()
                .and_then(|k| all.iter().find(|f| finding_key(f) == k).cloned());
            let row = focused
                .as_ref()
                .map(|f| DetailPaneRow::from_finding(f, None, None, resolved.index(), 3));
            rsx! {
                FindingDetailPane {
                    row,
                    focus: PaneFocus::Elsewhere,
                    prompt: None,
                    prompt_reason: String::new(),
                    undo: None,
                    on_key: Callback::default(),
                    on_focus: Callback::default(),
                    on_prompt_reason: Callback::default(),
                    on_prompt_commit: Callback::default(),
                    on_prompt_cancel: Callback::default(),
                    on_undo: Callback::default(),
                    on_dispose: Callback::default(),
                    on_open_detail: Callback::default(),
                }
                ViewerRoot {}
            }
        }
        let mut vdom = VirtualDom::new(app);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        let attr = "data-finding-key=\"";
        let keys: Vec<&str> = html
            .match_indices(attr)
            .map(|(i, _)| {
                let rest = &html[i + attr.len()..];
                &rest[..rest.find('"').expect("closing quote")]
            })
            .collect();
        // One on the pane, one on the viewer, and they are the same finding's key: the one the
        // cursor is on.
        assert_eq!(keys.len(), 2, "pane and viewer: {keys:?}");
        assert_eq!(keys[0], keys[1], "{html}");
        let second = dom_key(&finding_key(&finding("SQL-INJECTION-1", 9)));
        assert_eq!(keys[0], second, "the cursor is on the second row");
        // And the viewer's header is about that finding's file, with its position in the file.
        assert!(
            html.contains("cv-panel") && html.contains("src/x.rs"),
            "{html}"
        );
        assert!(html.contains("finding 2 of 3 in this file"), "{html}");
    }

    #[test]
    fn the_band_marks_the_findings_own_block_not_the_one_the_answer_carries() {
        // The answer's block is lines 2..=4 (it was fetched for another line). The finding's own
        // block is lines 5..=6.
        let own = BlockView {
            start_line: 5,
            end_line: 6,
            kind: "function".into(),
        };
        let html = render(Case {
            file: FileStateView::Loaded(view(8, |_| {})),
            block: Some(own),
            ..Case::default()
        });
        assert_eq!(count(&html, "cv-line-band"), 2, "{html}");
        // And with no block decided yet nothing is banded, whatever the answer carries.
        let html = render(Case {
            file: FileStateView::Loaded(view(8, |v| v["block"] = serde_json::Value::Null)),
            ..Case::default()
        });
        assert_eq!(count(&html, "cv-line-band"), 0);
    }

    #[test]
    fn a_repo_whose_runs_differ_in_commit_says_so_in_amber() {
        let html = render(Case {
            file: FileStateView::Loaded(view(4, |_| {})),
            sha_conflict: true,
            ..Case::default()
        });
        assert!(
            html.contains(
                "The scan and the audit ran at different commits; line numbers may differ"
            ),
            "{html}"
        );
        assert!(html.contains("cv-banner-amber"));
        let html = render(Case {
            file: FileStateView::Loaded(view(4, |_| {})),
            ..Case::default()
        });
        assert!(!html.contains("different commits"));
    }

    #[test]
    fn the_undo_bar_shows_a_live_undo_and_nothing_otherwise() {
        use camerata_ui_core::triage::TriageModel;
        fn bar_live() -> Element {
            let f: FindingView = serde_json::from_value(serde_json::json!({
                "repo": "o/r", "path": "a.rs", "line": 1, "rule_id": "R", "severity": "high",
                "snippet": "s", "detail": "d"
            }))
            .expect("fixture");
            let entry = TriageModel::new().dispose_with_undo(&[f], TriageState::Confirmed, "", 0);
            let undo_entry = use_signal(move || entry.clone());
            let now_ms = use_signal(|| 1_000);
            rsx! { ViewerUndoBar { undo_entry, now_ms, on_undo: Callback::default() } }
        }
        fn bar_idle() -> Element {
            let undo_entry = use_signal(|| None);
            let now_ms = use_signal(|| 0);
            rsx! { ViewerUndoBar { undo_entry, now_ms, on_undo: Callback::default() } }
        }
        let mut vdom = VirtualDom::new(bar_live);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("cv-undo") && html.contains("Undo"), "{html}");
        assert!(html.contains("4s"), "{html}");
        let mut vdom = VirtualDom::new(bar_idle);
        vdom.rebuild_in_place();
        assert!(!dioxus_ssr::render(&vdom).contains("cv-undo"));
    }

    #[test]
    fn a_response_the_viewer_does_not_recognise_renders_an_honest_note() {
        let html = render(Case {
            file: refusal(serde_json::json!({})),
            ..Case::default()
        });
        assert!(html.contains("could not be loaded"));
        let _ = (WindowView::default(), BlockView::default());
    }

    // ── step 3: search, symbols, popover, reference view, hub ──

    fn search_props(
        needle: &str,
        matches: &[(usize, usize)],
        current: Option<usize>,
    ) -> SearchProps {
        SearchProps {
            needle: needle.to_string(),
            field_open: true,
            status: search_status(
                needle,
                &camerata_ui_core::viewer_find::SearchResult {
                    matches: matches.to_vec(),
                    needle_chars: needle.chars().count(),
                    ..Default::default()
                },
                current,
                false,
            ),
            matches: Arc::new(matches.to_vec()),
            needle_chars: needle.chars().count(),
            current,
        }
    }

    #[test]
    fn search_highlights_every_match_on_screen_marks_the_current_one_and_counts() {
        // `    line N` holds "line" at col 4 of each of lines 3..6.
        let matches = [(3, 4), (4, 4), (5, 4), (6, 4)];
        let html = render(Case {
            file: FileStateView::Loaded(view(6, |_| {})),
            search: Some(search_props("line", &matches, Some(1))),
            ..Case::default()
        });
        assert_eq!(count(&html, "cv-match-cur"), 1, "{html}");
        // Every match is marked (the current one carries both classes).
        assert_eq!(count(&html, "cv-match"), 4 + 1, "{html}");
        assert!(html.contains("cv-search"), "the field is open");
        assert!(html.contains("2 of 4"), "{html}");
        assert!(html.contains("cv-keys off"), "the field owns the keys");
        assert!(html.contains(">line</span>"), "{html}");
    }

    #[test]
    fn a_closed_search_field_leaves_the_highlights_and_returns_the_keys() {
        let mut props = search_props("line", &[(3, 4)], Some(0));
        props.field_open = false;
        let html = render(Case {
            file: FileStateView::Loaded(view(6, |_| {})),
            search: Some(props),
            ..Case::default()
        });
        assert_eq!(count(&html, "cv-match-cur"), 1);
        assert!(!html.contains("class=\"cv-search\""), "{html}");
        assert!(html.contains("cv-keys armed"));
    }

    #[test]
    fn no_matches_says_so_and_marks_nothing() {
        let html = render(Case {
            file: FileStateView::Loaded(view(6, |_| {})),
            search: Some(search_props("zzz", &[], None)),
            ..Case::default()
        });
        assert!(html.contains("No matches"));
        assert_eq!(count(&html, "cv-match"), 0);
    }

    #[test]
    fn a_match_split_across_highlight_runs_is_marked_in_each_run() {
        // Line 1 is `fn` (a keyword run) then ` main() {`; a match for "n m" spans both runs.
        let html = render(Case {
            file: FileStateView::Loaded(view(2, |_| {})),
            search: Some(search_props("n m", &[(1, 1)], Some(0))),
            ..Case::default()
        });
        assert!(html.contains("tok-keyword cv-match cv-match-cur"), "{html}");
        assert!(html.contains("class=\"cv-match cv-match-cur\""), "{html}");
    }

    #[test]
    fn the_symbol_prompt_is_prefilled_and_owns_the_keys() {
        let html = render(Case {
            file: FileStateView::Loaded(view(4, |_| {})),
            symbol_prompt: Some(SymbolPromptProps {
                text: "build_sql".into(),
                error: Some("Letters, digits and _ $ - . : only, up to 128".into()),
            }),
            ..Case::default()
        });
        assert!(html.contains("cv-symbol-input"));
        assert!(html.contains("value=\"build_sql\""), "{html}");
        assert!(html.contains("Letters, digits"));
        assert!(html.contains("cv-keys off"));
    }

    fn refs_view(json: serde_json::Value) -> Arc<SymbolRefsView> {
        Arc::new(serde_json::from_value(json).expect("response"))
    }

    #[test]
    fn the_references_list_shows_hits_selection_and_its_own_keys() {
        let v = refs_view(serde_json::json!({
            "status": "ok", "source": "git", "symbol": "build_sql", "more": true,
            "hits": [
                {"path": "src/a.rs", "line": 3, "text": "build_sql(x)"},
                {"path": "src/b.rs", "line": 40, "text": "let q = build_sql(y);"},
            ]
        }));
        let html = render(Case {
            file: FileStateView::Loaded(view(4, |_| {})),
            refs: Some(RefsProps {
                symbol: "build_sql".into(),
                load: RefsLoad::Loaded(v),
                selected: 1,
            }),
            ..Case::default()
        });
        assert!(
            html.contains("2 references to build_sql (more not shown)"),
            "{html}"
        );
        assert!(html.contains("src/a.rs:3") && html.contains("src/b.rs:40"));
        assert_eq!(count(&html, "cv-ref-sel"), 1);
        // The selected row is the second one.
        let sel = html.find("cv-ref-sel").expect("selected row");
        assert!(html[sel..].contains("src/b.rs:40"));
        // The list owns the keys: the code's strip is off, the list's own is shown.
        assert!(html.contains("cv-keys armed"));
        assert!(html.contains("Up Down choose a reference"), "{html}");
        assert!(!html.contains("C confirm"));
    }

    #[test]
    fn the_references_list_says_why_it_is_empty() {
        for (json, want) in [
            (
                serde_json::json!({"status": "ok", "source": "git", "symbol": "zzz", "hits": []}),
                "No references to zzz",
            ),
            (
                serde_json::json!({
                    "status": "source_deleted",
                    "reason": "Source deleted on 24 Oct 2026, re-fetch to view"
                }),
                "Source deleted on 24 Oct 2026",
            ),
            (
                serde_json::json!({
                    "status": "commit_unavailable", "source": "working_tree", "symbol": "s",
                    "reason": "Inspected commit not available in this clone; showing the working tree",
                    "hits": [{"path": "a.rs", "line": 1, "text": "s"}]
                }),
                "Inspected commit not available",
            ),
        ] {
            let html = render(Case {
                file: FileStateView::Loaded(view(4, |_| {})),
                refs: Some(RefsProps {
                    symbol: "zzz".into(),
                    load: RefsLoad::Loaded(refs_view(json)),
                    selected: 0,
                }),
                ..Case::default()
            });
            assert!(html.contains(want), "{want}: {html}");
        }
        let html = render(Case {
            file: FileStateView::Loaded(view(4, |_| {})),
            refs: Some(RefsProps {
                symbol: "foo".into(),
                load: RefsLoad::Loading,
                selected: 0,
            }),
            ..Case::default()
        });
        assert!(html.contains("Searching for foo"));
        let html = render(Case {
            file: FileStateView::Loaded(view(4, |_| {})),
            refs: Some(RefsProps {
                symbol: "foo".into(),
                load: RefsLoad::Failed,
                selected: 0,
            }),
            ..Case::default()
        });
        assert!(html.contains("The server did not answer"));
    }

    #[test]
    fn a_search_that_did_not_run_is_a_failure_never_no_references() {
        for (status, reason) in [
            ("unavailable", "the search could not run"),
            (
                "too_large",
                "the search took too long; try a more specific symbol",
            ),
        ] {
            let html = render(Case {
                file: FileStateView::Loaded(view(4, |_| {})),
                refs: Some(RefsProps {
                    symbol: "zzz".into(),
                    load: RefsLoad::Loaded(refs_view(serde_json::json!({
                        "status": status, "reason": reason, "symbol": "zzz",
                        "hits": [], "more": false
                    }))),
                    selected: 0,
                }),
                ..Case::default()
            });
            assert!(
                html.contains("Could not search for zzz"),
                "{status}: {html}"
            );
            assert!(html.contains(reason), "{status}: {html}");
            assert!(!html.contains("No references"), "{status}: {html}");
            assert!(
                !html.contains("cv-banner-amber"),
                "no working-tree banner for a search that never ran"
            );
        }
        // Zero listed with the output cut short is not "none" either.
        let html = render(Case {
            file: FileStateView::Loaded(view(4, |_| {})),
            refs: Some(RefsProps {
                symbol: "zzz".into(),
                load: RefsLoad::Loaded(refs_view(serde_json::json!({
                    "status": "ok", "source": "git", "symbol": "zzz", "hits": [], "more": true
                }))),
                selected: 0,
            }),
            ..Case::default()
        });
        assert!(html.contains("cut short"), "{html}");
    }

    fn popover(extra: impl FnOnce(&mut serde_json::Value)) -> PopoverView {
        let mut f = serde_json::json!({
            "repo": "acme/app", "path": "src/db.rs", "line": 3, "rule_id": "SQL-INJECTION-1",
            "severity": "high", "snippet": "let q = format!(\"select {}\", id);",
            "detail": "User input reaches the query. It is concatenated. [needs review: dynamic table]",
            "kind": "defect", "confidence": "needs-review", "effort": "low",
            "also_matches": ["ARCH-NO-RAW-SQL-1"]
        });
        extra(&mut f);
        let f: FindingView = serde_json::from_value(f).expect("fixture");
        let own_key = finding_key(&f);
        let mk = |rule: &str, line: usize, navigable: bool| camerata_ui_core::viewer::FileFinding {
            key: if line == 3 {
                own_key.clone()
            } else {
                format!("k{line}")
            },
            line,
            rule_id: rule.to_string(),
            severity: "low".into(),
            state: TriageState::Unresolved,
            navigable,
        };
        popover_view(
            &f,
            TriageState::Unresolved,
            Some("Never build SQL by concatenating input."),
            Authority::Advisory,
            &[
                mk("SQL-INJECTION-1", 3, true),
                mk("XSS-1", 88, true),
                mk("CSRF-1", 214, false),
            ],
            None,
        )
    }

    #[test]
    fn the_popover_shows_up_to_eight_lines_and_no_fix() {
        let p = popover(|_| {});
        let html = render(Case {
            file: FileStateView::Loaded(view(6, |_| {})),
            popover: Some(p.clone()),
            ..Case::default()
        });
        assert_eq!(count(&html, "cv-pop-line"), p.line_count(), "{html}");
        assert!(p.line_count() <= 8);
        assert!(html.contains("cv-pop-rule"), "the rule id is the hub link");
        assert!(html.contains("kind-defect"), "{html}");
        assert!(html.contains("AI \u{b7} advisory"));
        assert!(html.contains("User input reaches the query."));
        assert!(!html.contains("It is concatenated."), "first sentence only");
        assert!(html.contains("Never build SQL by concatenating input."));
        assert!(html.contains("Needs review: dynamic table"));
        assert!(html.contains("effort low"));
        assert!(html.contains("Unresolved"));
        assert!(html.contains("2 more in this file"));
        assert!(html.contains("L88 XSS-1"));
        assert!(html.contains("Also violates here: ARCH-NO-RAW-SQL-1"));
        assert!(!html.contains("Previously"), "the memory seam is empty");
        assert!(!html.contains("Fix"), "no fix text in the popover");
        assert!(!html.contains("cv-popover-pinned"));
    }

    #[test]
    fn only_a_finding_the_cursor_can_reach_is_a_link_in_the_popover() {
        let html = render(Case {
            file: FileStateView::Loaded(view(6, |_| {})),
            popover: Some(popover(|_| {})),
            ..Case::default()
        });
        // L88 is reachable (a button); L214 was decided and left the table (plain text).
        assert_eq!(count(&html, "class=\"cv-pop-link\""), 1, "{html}");
        assert!(html.contains("cv-pop-dim"));
    }

    #[test]
    fn a_pinned_popover_says_so_and_a_deviation_reads_its_claim() {
        let p = popover(|f| {
            f["rule_id"] = "API-VERSIONING-1".into();
            f["kind"] = "deviation".into();
            f["deviating_sites"] = 2.into();
            f["assessed_sites"] = 47.into();
            f["convention_option_id"] = "versioned".into();
            f["convention_label"] = "URI-versioned endpoints".into();
        });
        let html = render(Case {
            file: FileStateView::Loaded(view(6, |_| {})),
            popover: Some(p),
            pinned: true,
            ..Case::default()
        });
        assert!(html.contains("cv-popover-pinned"));
        assert!(html.contains("kind-deviation"));
        assert!(html.contains("URI-versioned endpoints"), "{html}");
        assert!(!html.contains("User input reaches the query"));
    }

    #[test]
    fn a_reference_is_a_location_not_the_finding() {
        let html = render(Case {
            file: FileStateView::Loaded(view(6, |v| v["block"] = serde_json::Value::Null)),
            viewing: Some("src/other.rs".into()),
            line_cursor: Some(4),
            position: (0, 2),
            marks: vec![mark(5, "high", false, 1)],
            ..Case::default()
        });
        // The header names the file on screen and says it is a reference.
        assert!(html.contains("src/other.rs"), "{html}");
        assert!(!html.contains("src/db.rs"));
        assert!(html.contains("cv-refchip"));
        assert!(html.contains("line 4 of 6"));
        // The finding's line, band and minimap tick belong to its own file.
        assert_eq!(count(&html, "cv-line-hit"), 0, "{html}");
        assert_eq!(count(&html, "cv-line-band"), 0);
        assert_eq!(count(&html, "cv-mini-cur"), 0);
        assert!(!html.contains("in this file"));
        // The line the curator jumped to is marked.
        assert_eq!(count(&html, "cv-line-cursor"), 1);
        // The file's own findings are still marked.
        assert_eq!(count(&html, "class=\"cv-mark cv-sev-"), 1);
    }

    #[test]
    fn the_finding_in_its_own_file_has_no_reference_chip() {
        let html = render(Case {
            file: FileStateView::Loaded(view(6, |_| {})),
            ..Case::default()
        });
        assert!(!html.contains("cv-refchip"));
        assert_eq!(count(&html, "cv-mini-cur"), 1);
    }

    #[test]
    fn the_rule_hub_above_the_viewer_turns_the_keys_off_and_says_how_to_return() {
        let html = render(Case {
            file: FileStateView::Loaded(view(6, |_| {})),
            hub_open: true,
            ..Case::default()
        });
        assert!(html.contains("cv-keys off"));
        assert!(html.contains("The Rule Hub is open. Esc closes it and returns here."));
    }

    #[test]
    fn the_key_strip_names_the_new_keys() {
        let html = render(Case {
            file: FileStateView::Loaded(view(6, |_| {})),
            ..Case::default()
        });
        for needle in ["/ find", "S symbol", "Y link", "H rule"] {
            assert!(html.contains(needle), "{needle}");
        }
    }

    #[test]
    fn ranges_on_a_line_come_from_the_file_ordered_match_list() {
        let s = SearchProps {
            needle_chars: 3,
            matches: Arc::new(vec![(2, 5), (4, 0), (4, 9), (7, 1)]),
            ..SearchProps::default()
        };
        assert_eq!(ranges_on_line(Some(&s), 4), vec![(0, 3), (9, 3)]);
        assert_eq!(ranges_on_line(Some(&s), 3), Vec::<(usize, usize)>::new());
        assert_eq!(ranges_on_line(Some(&s), 7), vec![(1, 3)]);
        assert!(ranges_on_line(None, 4).is_empty());
        let empty = SearchProps::default();
        assert!(ranges_on_line(Some(&empty), 1).is_empty());
    }

    #[test]
    fn a_reference_path_is_only_a_path_that_is_not_the_findings_own() {
        let own = "src/db.rs".to_string();
        let other = "src/x.rs".to_string();
        assert_eq!(
            reference_path("src/db.rs", Some(&other)),
            Some(other.clone())
        );
        assert_eq!(reference_path("src/db.rs", Some(&own)), None);
        assert_eq!(reference_path("src/db.rs", None), None);
    }
}

#[cfg(test)]
mod network_tests {
    //! Tier-2 (docs/UI_TESTING.md): the request the viewer sends, and what it makes of the
    //! answers. The bodies are the shapes `crates/server/src/onboard/source_file.rs` produces.

    use super::*;
    use wiremock::matchers::{method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn the_request_carries_repo_path_sha_around_and_project() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/onboard/source-file"))
            .and(query_param("repo", "acme/app"))
            .and(query_param("path", "src/db.rs"))
            .and(query_param("sha", "a".repeat(40)))
            .and(query_param("around", "120"))
            .and(query_param("project", "proj-7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "ok", "source": "git", "commit": "a".repeat(40),
                "head_differs": false, "language": "rust", "total_lines": 2,
                "window": {"from": 1, "to": 2, "total_lines": 2}, "highlight": "ok",
                "classes": ["keyword"], "block": {"start_line": 1, "end_line": 2, "kind": "function"},
                "lines": [[[0, "fn"], [null, " f() {"]], [[null, "}"]]]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        let got = fetch_source_file(
            "acme/app",
            "src/db.rs",
            Some(&"a".repeat(40)),
            Some(120),
            Some("proj-7"),
        )
        .await
        .expect("an answer");
        assert_eq!(got.outcome(), SourceOutcome::Code);
        assert_eq!(got.total(), 2);
        assert_eq!(got.line(1).map(|l| l[0].1.as_str()), Some("fn"));
        assert_eq!(got.block.as_ref().map(|b| b.end_line), Some(2));
        // `.expect(1)` asserts on drop that the exact query went out once.
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn an_unknown_commit_and_project_are_left_off_the_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/onboard/source-file"))
            .and(query_param_is_missing("sha"))
            .and(query_param_is_missing("project"))
            .and(query_param_is_missing("around"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "file_missing", "reason": "file not found", "source": "working_tree"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        let got = fetch_source_file("acme/app", "gone.rs", Some(""), None, Some(""))
            .await
            .expect("a refusal is an answer");
        assert_eq!(
            got.outcome(),
            SourceOutcome::FileMissing("file not found".into())
        );
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn a_400_path_refusal_and_a_deleted_source_are_answers_not_failures() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/onboard/source-file"))
            .and(query_param("path", "../etc/passwd"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "status": "path_traversal", "reason": "requested path escapes the repo directory",
                "source": "git"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/onboard/source-file"))
            .and(query_param("path", "src/db.rs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "source_deleted", "source": "git",
                "reason": "Source deleted on 24 Oct 2026, re-fetch to view",
                "deleted_at": "2026-10-24T17:40:19Z", "record_id": "r1"
            })))
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

        let refused = fetch_source_file("acme/app", "../etc/passwd", None, None, None)
            .await
            .expect("the 400 still carries a body");
        assert!(
            matches!(refused.outcome(), SourceOutcome::Unavailable(t) if t.contains("escapes"))
        );

        let deleted = fetch_source_file("acme/app", "src/db.rs", None, None, None)
            .await
            .expect("answer");
        assert_eq!(
            deleted.outcome(),
            SourceOutcome::SourceDeleted("Source deleted on 24 Oct 2026, re-fetch to view".into())
        );
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn the_answer_remembers_which_line_it_was_asked_about() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/onboard/source-file"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "ok", "source": "git", "total_lines": 1,
                "window": {"from": 1, "to": 1, "total_lines": 1}, "lines": [[[null, "x"]]],
                "block": {"start_line": 1, "end_line": 1, "kind": "function"}
            })))
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        let asked = fetch_source_file("a/b", "x.rs", None, Some(1), None)
            .await
            .unwrap();
        assert!(block_for(&asked, 1).is_some(), "its block is for line 1");
        assert!(block_for(&asked, 7).is_none(), "and not for line 7");
        let unasked = fetch_source_file("a/b", "x.rs", None, None, None)
            .await
            .unwrap();
        assert!(block_for(&unasked, 1).is_none());
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn a_cached_file_gets_its_blocks_from_the_small_context_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/onboard/finding-context"))
            .and(query_param("repo", "acme/app"))
            .and(query_param("path", "src/db.rs"))
            .and(query_param("line", "88"))
            .and(query_param("sha", "abc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "ok", "start_line": 80, "end_line": 92, "violation_line": 88,
                "kind": "function", "lines": vec!["l"; 13], "language": "rust",
                "matches_snippet": true
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/onboard/finding-context"))
            .and(query_param("line", "5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "line_gone", "reason": "file changed since scan"
            })))
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        let b = fetch_block("acme/app", "src/db.rs", 88, Some("abc"), None)
            .await
            .unwrap();
        assert_eq!((b.start_line, b.end_line), (80, 92));
        assert!(fetch_block("acme/app", "src/db.rs", 5, None, None)
            .await
            .is_none());
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn the_symbol_request_carries_repo_symbol_sha_and_project() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/onboard/symbol-refs"))
            .and(query_param("repo", "acme/app"))
            .and(query_param("symbol", "build_sql"))
            .and(query_param("sha", "a".repeat(40)))
            .and(query_param("project", "proj-7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "ok", "source": "git", "commit": "a".repeat(40),
                "head_differs": false, "symbol": "build_sql", "more": true,
                "hits": [{"path": "src/a.rs", "line": 3, "text": "build_sql(x)"}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        let got = fetch_symbol_refs(
            "acme/app",
            "build_sql",
            Some(&"a".repeat(40)),
            Some("proj-7"),
        )
        .await
        .expect("an answer");
        assert_eq!(got.outcome(), RefsOutcome::List);
        assert_eq!(got.hits.len(), 1);
        assert_eq!(
            (got.hits[0].path.as_str(), got.hits[0].line),
            ("src/a.rs", 3)
        );
        assert!(got.more);
        assert_eq!(got.summary(), "1 reference to build_sql (more not shown)");
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn an_unknown_commit_and_project_are_left_off_the_symbol_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/onboard/symbol-refs"))
            .and(query_param_is_missing("sha"))
            .and(query_param_is_missing("project"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "commit_unavailable", "source": "working_tree",
                "reason": "Inspected commit not available in this clone; showing the working tree",
                "symbol": "foo", "hits": [], "more": false
            })))
            .expect(1)
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        let got = fetch_symbol_refs("acme/app", "foo", Some(""), Some(""))
            .await
            .expect("an answer");
        assert!(got
            .banner()
            .unwrap()
            .starts_with("Inspected commit not available"));
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn a_deleted_source_and_a_refused_symbol_are_answers_with_their_reasons() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/onboard/symbol-refs"))
            .and(query_param("symbol", "gone"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "source_deleted", "source": "git",
                "reason": "Source deleted on 24 Oct 2026, re-fetch to view",
                "deleted_at": "2026-10-24T17:40:19Z", "record_id": "r1"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/onboard/symbol-refs"))
            .and(query_param("symbol", "a b"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "ok": false, "code": "invalid_symbol",
                "message": "A symbol is 1 to 128 letters, digits or _ $ - . : characters."
            })))
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

        let deleted = fetch_symbol_refs("acme/app", "gone", None, None)
            .await
            .unwrap();
        assert_eq!(
            deleted.outcome(),
            RefsOutcome::Note("Source deleted on 24 Oct 2026, re-fetch to view".into())
        );
        let refused = fetch_symbol_refs("acme/app", "a b", None, None)
            .await
            .unwrap();
        assert!(matches!(
            refused.outcome(),
            RefsOutcome::Note(t) if t.starts_with("A symbol is 1 to 128")
        ));
        assert!(refused.hits.is_empty());
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn a_symbol_search_the_server_does_not_answer_is_none() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/onboard/symbol-refs"))
            .respond_with(ResponseTemplate::new(502).set_body_string("bad gateway"))
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        assert!(fetch_symbol_refs("a/b", "foo", None, None).await.is_none());
    }

    #[test]
    fn the_permalink_goes_through_the_clipboard_bridge_as_one_escaped_literal() {
        let script =
            bridge_copy_script("acme/app@abc1234:src/a \"b\".rs:3\nhttps://x/y#L3").unwrap();
        assert!(script.contains("window.__camerataClipboardBridgeSend"));
        assert!(script.contains("op: 'copy'"));
        assert!(script.contains("\\n"), "the newline is escaped: {script}");
        assert!(script.contains("\\\"b\\\""), "quotes are escaped: {script}");
        assert!(script.trim_end().ends_with("return false;"));
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn a_server_that_does_not_answer_with_json_is_none() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/onboard/source-file"))
            .respond_with(ResponseTemplate::new(502).set_body_string("bad gateway"))
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        assert!(fetch_source_file("a/b", "x.rs", None, None, None)
            .await
            .is_none());
    }
}

#[cfg(test)]
mod navigation_tests {
    //! The table plumbing the viewer needs: reaching a row whose group is collapsed, and refusing
    //! one the table no longer holds.

    use super::*;
    use chorale_core::{
        collapse_all_groups, remove_rows, set_filter, set_grouping, toggle_group,
        visible_grouped_view, ColumnId, FilterValue, GroupedRow, RowId, TableState,
    };

    fn finding(rule: &str, path: &str, line: usize) -> FindingView {
        serde_json::from_value(serde_json::json!({
            "repo": "me/api", "path": path, "line": line, "rule_id": rule, "severity": "high",
            "snippet": format!("s{line}"), "detail": "d", "status": "active", "kind": "defect"
        }))
        .expect("fixture")
    }

    fn queue(rows: &[FindingView]) -> TableState<FindingView> {
        let pairs = rows.iter().map(|f| (RowId::new(), f.clone())).collect();
        super::super::scan::finding_table_state(pairs, vec!["me/api".into()], false)
    }

    fn collapsed_queue(rows: &[FindingView]) -> TableState<FindingView> {
        let state = set_grouping(
            &queue(rows),
            vec![super::super::scan::RULE_GROUP_COLUMN, ColumnId("file")],
        );
        collapse_all_groups(&state)
    }

    fn visible(state: &TableState<FindingView>) -> Vec<String> {
        visible_grouped_view(state)
            .into_iter()
            .filter_map(|r| match r {
                GroupedRow::Data(_, f) => Some(finding_key(&f)),
                _ => None,
            })
            .collect()
    }

    fn open(mut state: TableState<FindingView>, plan: &Reach) -> TableState<FindingView> {
        let Reach::NeedsExpand(groups) = plan else {
            panic!("expected NeedsExpand, got {plan:?}");
        };
        for g in groups {
            state = toggle_group(&state, g);
        }
        state
    }

    #[test]
    fn opening_the_groups_that_hold_a_row_makes_it_reachable_and_nothing_else() {
        let rows = vec![
            finding("R-A", "a.rs", 10),
            finding("R-A", "b.rs", 20),
            finding("R-B", "c.rs", 30),
        ];
        let state = collapsed_queue(&rows);
        assert!(visible(&state).is_empty(), "everything starts collapsed");
        let target = finding_key(&rows[1]);

        let plan = plan_reach(&state, &target);
        assert!(
            matches!(&plan, Reach::NeedsExpand(g) if g.len() == 2),
            "{plan:?}"
        );
        let state = open(state, &plan);
        // Only that file's row appeared: its sibling file and the other rule stay collapsed.
        assert_eq!(visible(&state), vec![target.clone()]);
        assert_eq!(plan_reach(&state, &target), Reach::Visible);
    }

    #[test]
    fn a_row_in_an_already_open_group_needs_only_the_closed_one() {
        let rows = vec![finding("R-A", "a.rs", 10), finding("R-A", "b.rs", 20)];
        let state = collapsed_queue(&rows);
        let Reach::NeedsExpand(first) = plan_reach(&state, &finding_key(&rows[0])) else {
            panic!()
        };
        // Open the rule group only (the first of the two keys).
        let state = toggle_group(&state, &first[0]);
        let Reach::NeedsExpand(hiding) = plan_reach(&state, &finding_key(&rows[1])) else {
            panic!()
        };
        assert_eq!(hiding.len(), 1, "just the file group");
    }

    #[test]
    fn the_group_keys_come_from_the_tables_own_grouping() {
        let rows = vec![finding("R-A", "a.rs", 10), finding("R-A", "b.rs", 20)];
        let target = finding_key(&rows[0]);
        // Grouped by file only (an order preset would do this): one group to open.
        let by_file = collapse_all_groups(&set_grouping(&queue(&rows), vec![ColumnId("file")]));
        let plan = plan_reach(&by_file, &target);
        assert!(
            matches!(&plan, Reach::NeedsExpand(g) if g.len() == 1),
            "{plan:?}"
        );
        assert_eq!(visible(&open(by_file, &plan)), vec![target.clone()]);
        // Ungrouped: nothing hides it.
        assert_eq!(plan_reach(&queue(&rows), &target), Reach::Visible);
        // A grouping column this code cannot read a value from fails loud, not silent.
        let odd = set_grouping(&queue(&rows), vec![ColumnId("no-such-column")]);
        let plan = plan_reach(&odd, &target);
        assert_eq!(plan, Reach::Unresolvable);
        assert!(plan.problem().unwrap().contains("Could not open the group"));
    }

    #[test]
    fn a_finding_that_left_the_table_or_is_filtered_out_is_refused_with_a_message() {
        let rows = vec![finding("R-A", "a.rs", 10), finding("R-B", "b.rs", 20)];
        let state = queue(&rows);
        let gone_id = state.rows[0].0;
        let key_a = finding_key(&rows[0]);
        let gone = remove_rows(&state, &[gone_id]);
        let plan = plan_reach(&gone, &key_a);
        assert_eq!(plan, Reach::Gone);
        assert_eq!(plan.problem(), Some(GONE_PROBLEM));
        // Filtered by a column filter: still a row, not on screen.
        let filtered = set_filter(
            &state,
            ColumnId("type"),
            Some(FilterValue::Text("R-B".into())),
        );
        let plan = plan_reach(&filtered, &key_a);
        assert_eq!(plan, Reach::Filtered);
        assert_eq!(plan.problem(), Some("That finding is hidden by a filter"));
        assert_eq!(
            plan_reach(&filtered, &finding_key(&rows[1])),
            Reach::Visible
        );
        assert!(Reach::Visible.problem().is_none());
    }

    /// The review's sequence on the real table state: open on A, go-to-line (history), `]` to B,
    /// `[` to A, `c` on A (A leaves the table), Backspace. The history names A, which is gone, so
    /// the viewer must not move the cursor (a cursor aimed at it clamps to row 0, silently).
    #[test]
    fn backspace_to_a_finding_that_left_the_table_is_refused_and_the_cursor_stays() {
        use camerata_ui_core::viewer::{
            advance_after_disposition, file_findings, finish_disposition, step_in_file,
        };
        let rows = vec![
            finding("R-A", "a.rs", 10),
            finding("R-A", "a.rs", 40),
            finding("R-Z", "z.rs", 1),
        ];
        let (a, b) = (finding_key(&rows[0]), finding_key(&rows[1]));
        let mut state = queue(&rows);
        let nav = |s: &TableState<FindingView>| -> HashSet<String> {
            navigable_keys(&chorale_core::filtered_sorted_rows(s))
        };
        let none = HashMap::<String, Disposition>::new();

        let mut history = Vec::new();
        push_history(
            &mut history,
            HistoryEntry {
                key: a.clone(),
                path: "a.rs".into(),
                line: 10,
            },
        );
        let items = file_findings(&rows, &none, "me/api", "a.rs", &nav(&state));
        let FileStep::Move(to_b) =
            step_in_file(&items, &a, camerata_ui_core::triage::FocusMove::Down)
        else {
            panic!()
        };
        assert_eq!(to_b, b);
        assert_eq!(plan_reach(&state, &to_b), Reach::Visible);
        let FileStep::Move(to_a) =
            step_in_file(&items, &b, camerata_ui_core::triage::FocusMove::Up)
        else {
            panic!()
        };
        assert_eq!(to_a, a);
        // `c` on A: it landed, the viewer advances to B, and A is gone from the table.
        let target = advance_after_disposition(&items, &a);
        state = remove_rows(&state, &[state.rows[0].0]);
        let outcome = finish_disposition(true, target);
        assert_eq!(outcome.goto.as_deref(), Some(b.as_str()));
        assert_eq!(plan_reach(&state, &b), Reach::Visible);
        // Backspace: the entry is for A, not the finding on screen (B).
        let BackStep::Finding(back_to) = back_step(&mut history, &b, "a.rs") else {
            panic!()
        };
        assert_eq!(back_to, a);
        assert_eq!(plan_reach(&state, &back_to).problem(), Some(GONE_PROBLEM));
    }
}

#[cfg(test)]
mod wiring_tests {
    //! The real `FindingsTable` hands the viewer its link and takes it back. SSR cannot press a
    //! key, but `render_immediate` runs the effects and the re-render they cause, and a signal
    //! written from the test drives the rest.

    use super::super::scan::FindingsTable;
    use super::*;
    use std::cell::RefCell;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    type Handles = (Signal<Option<ViewerLink>>, Signal<bool>);

    thread_local! {
        static HANDLES: RefCell<Option<Handles>> =
            const { RefCell::new(None) };
    }

    fn finding(line: usize) -> FindingView {
        serde_json::from_value(serde_json::json!({
            "repo": "me/api", "path": "src/x.rs", "line": line, "rule_id": "SQL-INJECTION-1",
            "severity": "high", "snippet": format!("s{line}"), "detail": "d",
            "status": "active", "kind": "defect"
        }))
        .expect("fixture")
    }

    /// Let the pending effects run and apply the re-renders they cause.
    async fn settle(vdom: &mut VirtualDom) {
        for _ in 0..3 {
            let _ =
                tokio::time::timeout(std::time::Duration::from_millis(100), vdom.wait_for_work())
                    .await;
            vdom.render_immediate(&mut dioxus::dioxus_core::NoOpMutations);
        }
    }

    fn app() -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        use_context_provider(|| Signal::new(None::<FindingView>));
        let slot = use_context_provider(|| ViewerSlot(Signal::new(None)));
        let show = use_signal(|| true);
        HANDLES.with(|h| *h.borrow_mut() = Some((slot.0, show)));
        let dispositions = use_signal(HashMap::<String, Disposition>::new);
        rsx! {
            if show() {
                FindingsTable {
                    findings: vec![finding(4), finding(9)],
                    repos: vec!["me/api".to_string()],
                    descriptions: HashMap::new(),
                    dispositions,
                }
            }
            ViewerRoot {}
        }
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn the_table_registers_its_link_the_viewer_opens_on_it_and_unmounting_clears_it() {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

        let mut vdom = VirtualDom::new(app);
        vdom.rebuild_in_place();
        let (slot, show) = HANDLES.with(|h| h.borrow().expect("the harness stored its handles"));
        // Before the effects run the table has registered nothing.
        assert!(slot.peek().is_none());
        // The effects run, and the table's link is in the slot.
        settle(&mut vdom).await;
        let link = slot.peek().clone().expect("the table registered its link");
        let html = dioxus_ssr::render(&vdom);
        assert!(
            !html.contains("cv-panel"),
            "closed: the root renders nothing"
        );

        // Opening it (what `O` and the doors do) mounts the viewer at the root. Every group is
        // collapsed on mount, so the cursor is on no row and the root says so.
        vdom.in_runtime(|| {
            let mut open = link.open;
            open.set(true);
        });
        settle(&mut vdom).await;
        let html = dioxus_ssr::render(&vdom);
        assert!(
            html.contains("cv-empty") && html.contains("No finding focused"),
            "{html}"
        );

        // The pane's own `use_callback` handle, reached through the link: nothing is focused,
        // so nothing is written, and the answer is `false`, not an inference from the map.
        let landed = vdom.in_runtime(|| {
            link.apply_disposition
                .call((TriageState::Confirmed, String::new()))
        });
        assert!(!landed);
        assert!(link.dispositions.peek().is_empty());

        // Unmounting the table clears the slot (`use_drop`), so a stale link cannot outlive it.
        vdom.in_runtime(|| {
            let mut show = show;
            show.set(false);
        });
        settle(&mut vdom).await;
        assert!(slot.peek().is_none(), "the link must not outlive its table");
        assert!(!dioxus_ssr::render(&vdom).contains("cv-panel"));
    }
}

#[cfg(test)]
mod host_tests {
    //! The mounted viewer, driven by real key events (no browser): a symbol-reference excursion
    //! into another file and back, the Rule Hub round trip, and the search field. SSR cannot
    //! press a key; this dispatches `keydown` and `input` to the element ids the VirtualDom
    //! reports, then reads what the viewer renders and what the queue's cursor says.

    use super::*;
    use dioxus::dioxus_core::{Mutation, Mutations};
    use dioxus::html::{
        Code, Key, Location, Modifiers, SerializedFormData, SerializedKeyboardData,
    };
    use std::any::Any;
    use std::cell::RefCell;
    use std::rc::Rc;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// What the harness lets the test look at.
    #[derive(Clone, Copy)]
    struct Probe {
        cursor: Signal<RowCursor>,
        hub: Signal<Option<RuleHubRequest>>,
        decisions: Signal<usize>,
    }

    thread_local! {
        static PROBE: RefCell<Option<Probe>> = const { RefCell::new(None) };
    }

    fn finding(line: usize) -> FindingView {
        serde_json::from_value(serde_json::json!({
            "repo": "me/api", "path": "src/x.rs", "line": line, "rule_id": "SQL-INJECTION-1",
            "severity": "high", "snippet": format!("s{line}"), "detail": "d",
            "status": "active", "kind": "defect"
        }))
        .expect("fixture")
    }

    fn app() -> Element {
        let all = vec![finding(4), finding(9)];
        let table_rows: Vec<_> = all
            .iter()
            .map(|f| (chorale_core::RowId::new(), f.clone()))
            .collect();
        let handle = chorale_dioxus::use_table({
            let table_rows = table_rows.clone();
            move || {
                super::super::scan::finding_table_state(
                    table_rows.clone(),
                    vec!["me/api".to_string()],
                    false,
                )
            }
        });
        let dispositions = use_signal(HashMap::<String, Disposition>::new);
        let cursor = use_signal({
            let first = finding_key(&all[0]);
            move || RowCursor::focused_on(first.clone())
        });
        let table = handle.signal();
        let visible_keys = use_memo(move || {
            chorale_core::visible_grouped_view(&table.read())
                .into_iter()
                .filter_map(|r| match r {
                    chorale_core::GroupedRow::Data(_, f) => Some(finding_key(&f)),
                    _ => None,
                })
                .collect::<Vec<String>>()
        });
        let open = use_signal(|| true);
        let hub = use_signal(|| Option::<RuleHubRequest>::None);
        let decisions = use_signal(|| 0_usize);
        use_context_provider(|| ViewerHubOpen(hub));
        let link = ViewerLink {
            handle,
            cursor,
            visible_keys,
            dispositions,
            all_findings: Arc::new(all.clone()),
            shas: Arc::new(HashMap::new()),
            sha_conflicts: Arc::new(HashSet::new()),
            project_id: String::new(),
            open,
            undo_entry: use_signal(|| None),
            now_ms: use_signal(|| 0),
            on_undo: Callback::default(),
            apply_disposition: Callback::new({
                let mut decisions = decisions;
                move |_: (TriageState, String)| {
                    decisions += 1;
                    true
                }
            }),
            on_close: Callback::default(),
            on_open_modal: Callback::default(),
            origins: Arc::new(HashMap::new()),
            descriptions: Arc::new(HashMap::new()),
            on_open_hub: Callback::new({
                let mut hub = hub;
                move |r: RuleHubRequest| hub.set(Some(r))
            }),
        };
        let slot = use_context_provider(|| ViewerSlot(Signal::new(Some(link))));
        // What the results root does beside the hub.
        use_hub_focus_return(hub, Some(slot));
        PROBE.with(|p| {
            *p.borrow_mut() = Some(Probe {
                cursor,
                hub,
                decisions,
            })
        });
        rsx! { ViewerRoot {} }
    }

    /// A source-file answer for `lines` lines of `{prefix} N`, with `extra` replacing some.
    fn file_body(prefix: &str, lines: usize, extra: &[(usize, &str)]) -> serde_json::Value {
        let body: Vec<serde_json::Value> = (1..=lines)
            .map(|n| {
                let text = extra
                    .iter()
                    .find(|(l, _)| *l == n)
                    .map_or_else(|| format!("{prefix} {n}"), |(_, t)| (*t).to_string());
                serde_json::json!([[null, text]])
            })
            .collect();
        serde_json::json!({
            "status": "ok", "source": "git", "commit": "a".repeat(40), "head_differs": false,
            "language": "rust", "total_lines": lines,
            "window": {"from": 1, "to": lines, "total_lines": lines},
            "highlight": "ok", "classes": [], "block": null, "lines": body,
        })
    }

    async fn serve() -> MockServer {
        serve_with("git").await
    }

    /// The fixture server, with `src/x.rs` read from `source` (`git` or `working_tree`).
    async fn serve_with(source: &str) -> MockServer {
        let server = MockServer::start().await;
        let ok = |body: serde_json::Value| ResponseTemplate::new(200).set_body_json(body);
        Mock::given(method("GET"))
            .and(path("/api/onboard/source-file"))
            .and(query_param("path", "src/x.rs"))
            .respond_with(ok({
                let mut body = file_body("here", 14, &[(4, "    let q = build_sql(x);")]);
                body["source"] = source.into();
                if source != "git" {
                    body["status"] = "commit_unavailable".into();
                    body["reason"] =
                        "Inspected commit not available in this clone; showing the working tree"
                            .into();
                }
                body
            }))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/onboard/source-file"))
            .and(query_param("path", "src/other.rs"))
            .respond_with(ok(file_body("there", 6, &[])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/onboard/finding-context"))
            .respond_with(ok(serde_json::json!({"status": "file_missing"})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/onboard/symbol-refs"))
            .and(query_param("symbol", "build_sql"))
            .respond_with(ok(serde_json::json!({
                "status": "ok", "source": "git", "symbol": "build_sql", "more": false,
                "hits": [
                    {"path": "src/other.rs", "line": 3, "text": "build_sql(y)"},
                    {"path": "src/x.rs", "line": 11, "text": "build_sql(z)"},
                ]
            })))
            .mount(&server)
            .await;
        server
    }

    fn named_key(key: &str) -> Key {
        match key {
            "Enter" => Key::Enter,
            "Escape" => Key::Escape,
            "Backspace" => Key::Backspace,
            "ArrowDown" => Key::ArrowDown,
            "ArrowUp" => Key::ArrowUp,
            other => Key::Character(other.to_string()),
        }
    }

    /// Dispatch to `id`.
    fn send(vdom: &VirtualDom, name: &str, id: dioxus::dioxus_core::ElementId, data: Box<dyn Any>) {
        let platform: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(data));
        vdom.runtime()
            .handle_event(name, dioxus::dioxus_core::Event::new(platform, true), id);
    }

    fn keydown(vdom: &VirtualDom, id: dioxus::dioxus_core::ElementId, key: &str, mods: Modifiers) {
        let data = SerializedKeyboardData::new(
            named_key(key),
            Code::Unidentified,
            Location::Standard,
            false,
            mods,
            false,
        );
        send(vdom, "keydown", id, Box::new(data));
    }

    fn input(vdom: &VirtualDom, id: dioxus::dioxus_core::ElementId, value: &str) {
        let data = SerializedFormData::new(value.to_string(), Vec::new());
        send(vdom, "input", id, Box::new(data));
    }

    /// Everything the VirtualDom has told us about listeners so far.
    #[derive(Default)]
    struct Listeners(Vec<(String, dioxus::dioxus_core::ElementId)>);

    impl Listeners {
        fn take(&mut self, m: &Mutations) {
            for e in &m.edits {
                if let Mutation::NewEventListener { name, id } = e {
                    self.0.push((name.clone(), *id));
                }
            }
        }
        fn last(&self, name: &str) -> dioxus::dioxus_core::ElementId {
            self.0
                .iter()
                .rev()
                .find(|(n, _)| n == name)
                .map(|(_, id)| *id)
                .unwrap_or_else(|| panic!("no {name} listener among {:?}", self.0))
        }
    }

    /// Let pending work run (fetches, effects), collecting the listeners it creates.
    async fn settle(vdom: &mut VirtualDom, seen: &mut Listeners) {
        for _ in 0..4 {
            let _ =
                tokio::time::timeout(std::time::Duration::from_millis(50), vdom.wait_for_work())
                    .await;
            seen.take(&vdom.render_immediate_to_vec());
        }
    }

    fn html(vdom: &VirtualDom) -> String {
        dioxus_ssr::render(vdom)
    }

    fn data_key(html: &str) -> String {
        let attr = "data-finding-key=\"";
        let i = html.find(attr).expect("the viewer is mounted") + attr.len();
        html[i..i + html[i..].find('"').expect("closing quote")].to_string()
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn a_reference_excursion_never_moves_the_cursor_and_backspace_returns() {
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        let server = serve().await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

        let mut vdom = VirtualDom::new(app);
        let mut seen = Listeners::default();
        seen.take(&vdom.rebuild_to_vec());
        settle(&mut vdom, &mut seen).await;
        let probe = PROBE.with(|p| p.borrow().expect("probe"));
        let panel = seen.last("keydown");
        let key_before = data_key(&html(&vdom));
        let cursor_before = probe.cursor.peek().clone();

        let page = html(&vdom);
        assert!(
            page.contains("here 1") && page.contains("finding 1 of 2 in this file"),
            "{page}"
        );
        assert!(!page.contains("cv-refchip"));

        // S: the prompt is pre-filled with the longest identifier on the violation line.
        keydown(&vdom, panel, "s", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(page.contains("value=\"build_sql\""), "{page}");
        assert!(page.contains("cv-keys off"));
        // Enter in the field runs the search; the list shows both hits.
        let field = seen.last("keydown");
        keydown(&vdom, field, "Enter", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(page.contains("2 references to build_sql"), "{page}");
        assert!(page.contains("src/other.rs:3") && page.contains("src/x.rs:11"));
        assert!(page.contains("Up Down choose a reference"));

        // A decision key does nothing while the list owns the keys.
        keydown(&vdom, panel, "c", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        assert_eq!(
            *probe.decisions.peek(),
            0,
            "C is text-free but inert in the list"
        );

        // Enter opens the first hit: another file, shown as a reference, cursor untouched.
        keydown(&vdom, panel, "Enter", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(page.contains("cv-refchip"), "{page}");
        assert!(
            page.contains("src/other.rs") && page.contains("there 1"),
            "{page}"
        );
        assert!(page.contains("line 3 of 6"), "{page}");
        assert!(
            !page.contains("cv-line-hit"),
            "the finding's line is in the other file"
        );
        assert!(!page.contains("class=\"cv-refs\""), "the list closed");
        assert_eq!(
            data_key(&page),
            key_before,
            "the viewer is still on the same finding"
        );
        assert!(
            *probe.cursor.peek() == cursor_before,
            "the queue cursor did not move"
        );

        // A decision key is refused: the finding is not on screen.
        keydown(&vdom, panel, "c", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        assert_eq!(*probe.decisions.peek(), 0);
        assert!(html(&vdom).contains("A reference is on screen, not the finding"));
        keydown(&vdom, panel, "i", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        assert!(
            !html(&vdom).contains("Reason for"),
            "no reason prompt opens either"
        );

        // Backspace goes back to the finding's own line.
        keydown(&vdom, panel, "Backspace", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(!page.contains("cv-refchip"), "{page}");
        assert!(
            page.contains("src/x.rs") && page.contains("here 1"),
            "{page}"
        );
        assert!(page.contains("cv-line-hit"));
        assert_eq!(data_key(&page), key_before);
        assert!(*probe.cursor.peek() == cursor_before);
        assert_eq!(*probe.decisions.peek(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn a_hit_in_the_same_file_is_a_scroll_and_backspace_returns_to_the_line() {
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        let server = serve().await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

        let mut vdom = VirtualDom::new(app);
        let mut seen = Listeners::default();
        seen.take(&vdom.rebuild_to_vec());
        settle(&mut vdom, &mut seen).await;
        let panel = seen.last("keydown");

        keydown(&vdom, panel, "s", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        keydown(&vdom, seen.last("keydown"), "Enter", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        // Down to the second hit (src/x.rs:11), then open it.
        keydown(&vdom, panel, "ArrowDown", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        assert!(html(&vdom).contains("data-selected=\"1\""));
        keydown(&vdom, panel, "Enter", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(
            !page.contains("cv-refchip"),
            "same file: not a reference, {page}"
        );
        assert!(page.contains("line 11 of 14"), "{page}");
        // Back to the violation line (4).
        keydown(&vdom, panel, "Backspace", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        assert!(html(&vdom).contains("line 4 of 14"), "{}", html(&vdom));
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn escape_in_the_list_returns_to_the_code_and_a_bad_symbol_stays_in_the_prompt() {
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        let server = serve().await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

        let mut vdom = VirtualDom::new(app);
        let mut seen = Listeners::default();
        seen.take(&vdom.rebuild_to_vec());
        settle(&mut vdom, &mut seen).await;
        let panel = seen.last("keydown");

        // An invalid symbol is refused in the prompt; nothing is sent.
        keydown(&vdom, panel, "s", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        input(&vdom, seen.last("input"), "a b");
        settle(&mut vdom, &mut seen).await;
        keydown(&vdom, seen.last("keydown"), "Enter", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(
            page.contains("Letters, digits and _ $ - . : only"),
            "{page}"
        );
        assert!(page.contains("cv-symbol-input"), "the prompt stays open");
        // Esc cancels the prompt and re-arms the keys.
        keydown(&vdom, seen.last("keydown"), "Escape", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        assert!(!html(&vdom).contains("cv-symbol-input"));
        assert!(html(&vdom).contains("C confirm"));

        // A good one runs; Esc in the list closes it without leaving the viewer.
        keydown(&vdom, panel, "s", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        keydown(&vdom, seen.last("keydown"), "Enter", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        assert!(html(&vdom).contains("class=\"cv-refs\""));
        keydown(&vdom, panel, "Escape", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(!page.contains("class=\"cv-refs\""), "{page}");
        assert!(page.contains("cv-panel"), "the viewer is still open");
        assert!(page.contains("C confirm"), "the code's keys are back");
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn h_opens_the_hub_the_viewer_stands_aside_and_esc_returns() {
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        let server = serve().await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

        let mut vdom = VirtualDom::new(app);
        let mut seen = Listeners::default();
        seen.take(&vdom.rebuild_to_vec());
        settle(&mut vdom, &mut seen).await;
        let probe = PROBE.with(|p| p.borrow().expect("probe"));
        let panel = seen.last("keydown");
        let key_before = data_key(&html(&vdom));

        keydown(&vdom, panel, "h", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        assert_eq!(
            probe.hub.peek().clone(),
            Some(RuleHubRequest {
                repo: "me/api".into(),
                rule_id: "SQL-INJECTION-1".into()
            })
        );
        let page = html(&vdom);
        assert!(page.contains("The Rule Hub is open"), "{page}");
        // Every other key is inert while the hub is up.
        keydown(&vdom, panel, "c", Modifiers::empty());
        keydown(&vdom, panel, "s", Modifiers::empty());
        keydown(&vdom, panel, "Backspace", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        assert_eq!(*probe.decisions.peek(), 0);
        assert!(!html(&vdom).contains("cv-symbol-input"));

        // Esc closes the hub and the viewer is as it was.
        keydown(&vdom, panel, "Escape", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        assert!(probe.hub.peek().is_none());
        let page = html(&vdom);
        assert!(
            page.contains("cv-panel") && page.contains("C confirm"),
            "{page}"
        );
        assert_eq!(data_key(&page), key_before);
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_search_field_highlights_counts_cycles_with_a_wrap_and_keeps_highlights_after_esc()
    {
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        let server = serve().await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

        let mut vdom = VirtualDom::new(app);
        let mut seen = Listeners::default();
        seen.take(&vdom.rebuild_to_vec());
        settle(&mut vdom, &mut seen).await;
        let panel = seen.last("keydown");

        keydown(&vdom, panel, "/", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        assert!(html(&vdom).contains("cv-search-input"));
        // "here 1" is on lines 1 and 10..14: 6 matches, case-insensitively. Typing starts on the
        // first at or after the finding's line (4), which is line 10: the second match.
        input(&vdom, seen.last("input"), "HERE 1");
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(page.contains("2 of 6"), "{page}");
        assert_eq!(page.matches("cv-match-cur").count(), 1);
        // Enter steps forward; at the last match it wraps and says so.
        let field = seen.last("keydown");
        for _ in 0..4 {
            keydown(&vdom, field, "Enter", Modifiers::empty());
        }
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(
            page.contains("6 of 6") && !page.contains("wrapped"),
            "{page}"
        );
        keydown(&vdom, field, "Enter", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        assert!(html(&vdom).contains("1 of 6 (wrapped)"), "{}", html(&vdom));
        // Shift+Enter goes back, wrapping the other way.
        keydown(&vdom, field, "Enter", Modifiers::SHIFT);
        settle(&mut vdom, &mut seen).await;
        assert!(html(&vdom).contains("6 of 6 (wrapped)"), "{}", html(&vdom));
        // Esc closes the field and leaves the highlights; the keys are back.
        keydown(&vdom, field, "Escape", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(!page.contains("cv-search-input"), "{page}");
        assert!(page.contains("cv-match"), "the highlights stay");
        assert!(page.contains("C confirm"));
        // The next navigation (go to line) retires them.
        keydown(&vdom, panel, "g", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        assert!(!html(&vdom).contains("cv-match"), "{}", html(&vdom));
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn space_pins_the_popover_for_the_finding_and_space_unpins_it() {
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        let server = serve().await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

        let mut vdom = VirtualDom::new(app);
        let mut seen = Listeners::default();
        seen.take(&vdom.rebuild_to_vec());
        settle(&mut vdom, &mut seen).await;
        let panel = seen.last("keydown");
        assert!(!html(&vdom).contains("cv-popover"));

        keydown(&vdom, panel, " ", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(page.contains("cv-popover-pinned"), "{page}");
        assert!(
            page.contains("SQL-INJECTION-1") && page.contains("1 more in this file"),
            "{page}"
        );
        // The other finding in this file is a link to it.
        assert!(page.contains("L9 SQL-INJECTION-1"));

        keydown(&vdom, panel, " ", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        assert!(!html(&vdom).contains("cv-popover"));
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn y_reports_whether_the_permalink_was_copied() {
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        let server = serve().await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

        let mut vdom = VirtualDom::new(app);
        let mut seen = Listeners::default();
        seen.take(&vdom.rebuild_to_vec());
        settle(&mut vdom, &mut seen).await;
        keydown(&vdom, seen.last("keydown"), "y", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        // There is no webview here, so the copy cannot succeed; the point is that the viewer
        // answers either way, in words, and stays open. (The text copied is pinned in core.)
        let page = html(&vdom);
        assert!(
            page.contains("Copied the permalink.")
                || page.contains("Could not copy the permalink."),
            "{page}"
        );
        assert!(page.contains("cv-panel"));
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn y_refuses_when_the_code_on_screen_is_the_working_tree() {
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        let server = serve_with("working_tree").await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

        let mut vdom = VirtualDom::new(app);
        let mut seen = Listeners::default();
        seen.take(&vdom.rebuild_to_vec());
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(
            page.contains("cv-banner-amber"),
            "the working tree is shown: {page}"
        );
        keydown(&vdom, seen.last("keydown"), "y", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(
            page.contains("No inspected commit was recorded, so there is no permalink."),
            "{page}"
        );
        assert!(!page.contains("Copied the permalink."));
        assert!(
            !page.contains("Could not copy the permalink."),
            "nothing was attempted"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn a_reference_that_cannot_load_shows_no_evidence_line_of_the_finding() {
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        let server = MockServer::start().await;
        let ok = |body: serde_json::Value| ResponseTemplate::new(200).set_body_json(body);
        Mock::given(method("GET"))
            .and(path("/api/onboard/source-file"))
            .and(query_param("path", "src/x.rs"))
            .respond_with(ok(file_body(
                "here",
                14,
                &[(4, "    let q = build_sql(x);")],
            )))
            .mount(&server)
            .await;
        // The referenced file is gone, and a third is too big to view.
        Mock::given(method("GET"))
            .and(path("/api/onboard/source-file"))
            .and(query_param("path", "src/other.rs"))
            .respond_with(ok(serde_json::json!({
                "status": "too_large", "source": "git",
                "reason": "file is too large to view here (over 4 MB, or too slow to prepare)"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/onboard/finding-context"))
            .respond_with(ok(serde_json::json!({"status": "file_missing"})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/onboard/symbol-refs"))
            .respond_with(ok(serde_json::json!({
                "status": "ok", "source": "git", "symbol": "build_sql", "more": false,
                "hits": [{"path": "src/other.rs", "line": 3, "text": "build_sql(y)"}]
            })))
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

        let mut vdom = VirtualDom::new(app);
        let mut seen = Listeners::default();
        seen.take(&vdom.rebuild_to_vec());
        settle(&mut vdom, &mut seen).await;
        let panel = seen.last("keydown");
        keydown(&vdom, panel, "s", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        keydown(&vdom, seen.last("keydown"), "Enter", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        keydown(&vdom, panel, "Enter", Modifiers::empty());
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(page.contains("cv-refchip"), "{page}");
        assert!(page.contains("too large to view here"), "{page}");
        // The finding's stored line (`s4`) and the way to its enclosing block belong to its own
        // file: neither is offered under another file's refusal.
        assert!(!page.contains("pane-snippet"), "{page}");
        assert!(!page.contains("Show the enclosing block"), "{page}");
    }
    thread_local! {
        static HUB_CLOSED: RefCell<Option<Signal<usize>>> = const { RefCell::new(None) };
    }

    fn hub_app() -> Element {
        let mut closed = use_signal(|| 0_usize);
        HUB_CLOSED.with(|c| *c.borrow_mut() = Some(closed));
        let hub = camerata_ui_core::triage::rule_hub(&[finding(4)], "me/api", "SQL-INJECTION-1")
            .expect("the finding's rule has a hub");
        rsx! {
            super::super::scan::RuleHubModal {
                hub,
                on_jump: Callback::default(),
                on_close: Callback::new(move |()| closed += 1),
            }
        }
    }

    /// Esc closes the Rule Hub when the key goes to the hub itself, as it does after a click
    /// inside it (which used to leave Esc with nowhere to go).
    #[test]
    fn escape_closes_the_rule_hub_and_nothing_else_does() {
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        let mut vdom = VirtualDom::new(hub_app);
        let mut seen = Listeners::default();
        seen.take(&vdom.rebuild_to_vec());
        let page = html(&vdom);
        assert!(page.contains("tabindex=\"0\"") && page.contains(super::super::scan::RULE_HUB_ID));
        let modal = seen.last("keydown");
        let closed = HUB_CLOSED.with(|c| c.borrow().expect("signal"));
        for key in ["a", "Enter", "ArrowDown", " ", "h", "Backspace"] {
            keydown(&vdom, modal, key, Modifiers::empty());
        }
        assert_eq!(*closed.peek(), 0, "only Escape closes it");
        keydown(&vdom, modal, "Escape", Modifiers::empty());
        assert_eq!(*closed.peek(), 1);
    }

    #[test]
    fn the_selected_reference_is_scrolled_into_view_with_the_minimum_scroll() {
        let js = &scroll_selected_js(SELECTED_REF);
        assert!(js.contains(".cv-ref-sel"), "{js}");
        assert!(js.contains("scrollIntoView({block:'nearest'})"), "{js}");
        assert_eq!(js.matches("requestAnimationFrame").count(), 2);
        // The class it looks for is the one the list puts on the selected row.
        fn panel() -> Element {
            let v = Arc::new(SymbolRefsView {
                status: "ok".into(),
                symbol: "s".into(),
                hits: vec![RefHit::default(); 3],
                ..SymbolRefsView::default()
            });
            refs_panel(
                &RefsProps {
                    symbol: "s".into(),
                    load: RefsLoad::Loaded(v),
                    selected: 2,
                },
                Callback::default(),
            )
        }
        let mut vdom = VirtualDom::new(panel);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert_eq!(html.matches("cv-ref-sel").count(), 1, "{html}");
    }

    fn focus_log() -> Vec<String> {
        FOCUS_LOG.with(|l| l.borrow().clone())
    }

    /// Opened from the viewer with `H`: closing the hub puts the keyboard back on the viewer
    /// (and not on the pane underneath it), whichever way the hub is closed.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_hub_opened_from_the_viewer_returns_focus_to_the_viewer() {
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        let server = serve().await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

        let mut vdom = VirtualDom::new(app);
        let mut seen = Listeners::default();
        seen.take(&vdom.rebuild_to_vec());
        settle(&mut vdom, &mut seen).await;
        let probe = PROBE.with(|p| p.borrow().expect("probe"));
        let panel = seen.last("keydown");
        let key_before = data_key(&html(&vdom));

        for close in ["Escape", "outside"] {
            keydown(&vdom, panel, "h", Modifiers::empty());
            settle(&mut vdom, &mut seen).await;
            assert!(probe.hub.peek().is_some(), "{close}: the hub opened");
            FOCUS_LOG.with(|l| l.borrow_mut().clear());
            if close == "Escape" {
                keydown(&vdom, panel, "Escape", Modifiers::empty());
            } else {
                // The hub's own close button, backdrop or jump: the root clears the signal.
                vdom.in_runtime(|| {
                    let mut hub = probe.hub;
                    hub.set(None);
                });
            }
            settle(&mut vdom, &mut seen).await;
            assert!(probe.hub.peek().is_none());
            let log = focus_log();
            assert!(log.contains(&VIEWER_ID.to_string()), "{close}: {log:?}");
            assert!(
                !log.contains(&super::super::scan::DETAIL_PANE_ID.to_string()),
                "{close}: the pane must not take focus from the viewer: {log:?}"
            );
            assert_eq!(data_key(&html(&vdom)), key_before, "{close}: same finding");
        }
    }

    thread_local! {
        static PANE_HUB: RefCell<Option<Signal<Option<RuleHubRequest>>>> =
            const { RefCell::new(None) };
    }

    /// The results root with the viewer CLOSED: the hub and nothing else.
    fn pane_origin_app() -> Element {
        let hub = use_signal(|| Option::<RuleHubRequest>::None);
        // The Queue has registered its link, but the viewer is not open.
        let slot = use_context_provider(|| ViewerSlot(Signal::new(None)));
        use_hub_focus_return(hub, Some(slot));
        PANE_HUB.with(|h| *h.borrow_mut() = Some(hub));
        rsx! {}
    }

    /// Opened from the pane (or the deviation bar), viewer closed: closing the hub puts the
    /// keyboard back on the detail pane, where its keys work without a click.
    #[tokio::test(flavor = "current_thread")]
    async fn the_hub_opened_from_the_pane_returns_focus_to_the_pane() {
        let mut vdom = VirtualDom::new(pane_origin_app);
        let mut seen = Listeners::default();
        seen.take(&vdom.rebuild_to_vec());
        settle(&mut vdom, &mut seen).await;
        let hub = PANE_HUB.with(|h| h.borrow().expect("hub signal"));
        let open = || {
            Some(RuleHubRequest {
                repo: "me/api".into(),
                rule_id: "SQL-INJECTION-1".into(),
            })
        };

        for round in 0..2 {
            FOCUS_LOG.with(|l| l.borrow_mut().clear());
            vdom.in_runtime(|| {
                let mut hub = hub;
                hub.set(open());
            });
            settle(&mut vdom, &mut seen).await;
            assert!(
                focus_log().is_empty(),
                "round {round}: opening the hub moves nothing: {:?}",
                focus_log()
            );
            vdom.in_runtime(|| {
                let mut hub = hub;
                hub.set(None);
            });
            settle(&mut vdom, &mut seen).await;
            assert_eq!(
                focus_log().last().map(String::as_str),
                Some(super::super::scan::DETAIL_PANE_ID),
                "round {round}: {:?}",
                focus_log()
            );
            assert_eq!(focus_log().len(), 1, "one focus call per close");
        }
    }

    /// With the viewer open the root leaves the keyboard to the viewer.
    #[test]
    fn the_hub_gives_focus_to_the_viewer_exactly_when_it_is_open() {
        assert_eq!(hub_close_focus(true), FocusReturn::Viewer);
        assert_eq!(hub_close_focus(false), FocusReturn::Pane);
    }
}
