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
    finding_key, Disposition, FindingView, KeyMods, RowCursor, TriageState, UndoEntry,
};
use camerata_ui_core::viewer::{
    advance_after_disposition, cut_line, file_findings, finding_position, gutter_marks,
    line_at_scroll, line_label, more_chars_label, page_request, parse_goto_line, prefetch_target,
    push_history, scroll_top_for_line, scrolled, severity_class, spacers, step_file_in_group,
    step_in_file, viewer_command, viewer_focus, viewer_keys_armed, virtual_range, BannerKind,
    FileKey, FileStep, GutterMark, HistoryEntry, SourceFileView, SourceOutcome, ViewerCache,
    ViewerCommand, CODE_LINE_H_PX, KEY_ROWS, MAX_LINE_CHARS, OVERSCAN_LINES, SCROLL_QUANTUM_LINES,
};
use chorale_core::GroupKey;
use chorale_dioxus::UseTableHandle;
use dioxus::prelude::*;

use super::scan::{undo_banner_of, UndoBanner, RULE_GROUP_COLUMN};

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
    serde_json::from_value(value).ok()
}

// ── What the props-only component is given ───────────────────────────────────────────────

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
}

impl ViewerFinding {
    pub(super) fn from_finding(f: &FindingView, sha: &str) -> Self {
        Self {
            repo: f.repo.clone(),
            path: f.path.clone(),
            rule_id: f.rule_id.clone(),
            severity: f.severity.clone(),
            line: f.line,
            snippet: f.snippet.clone(),
            sha7: camerata_ui_core::source::short_sha(sha).to_string(),
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

/// The highlighted text of one line, cut at [`MAX_LINE_CHARS`].
fn line_spans(view: &SourceFileView, segments: &[camerata_ui_core::viewer::Segment]) -> Element {
    let cut = cut_line(segments, MAX_LINE_CHARS);
    let (shown, more): (&[_], Option<usize>) = match &cut {
        Some((segs, more)) => (segs.as_slice(), Some(*more)),
        None => (segments, None),
    };
    rsx! {
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
    }
}

/// The code area: gutter, lines, band, highlight, minimap. Only the lines in
/// [`virtual_range`] are in the DOM.
#[allow(clippy::too_many_arguments)]
fn code_body(
    view: &SourceFileView,
    finding: &ViewerFinding,
    marks: &[GutterMark],
    scroll_top: f64,
    viewport_px: f64,
    line_cursor: Option<usize>,
    on_scroll: Callback<(f64, f64)>,
    on_scroll_ready: Callback<()>,
    on_mark: Callback<String>,
    on_line: Callback<usize>,
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
    let block = view.block.clone();
    let sev = severity_class(&finding.severity);
    let hit = finding.line;
    rsx! {
        div { class: "cv-body",
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
                            let in_band = block
                                .as_ref()
                                .is_some_and(|b| n >= b.start_line && n <= b.end_line);
                            let class = line_class(n == hit, in_band, line_cursor == Some(n), sev);
                            let mark = mark_at.get(&n).copied();
                            let segments = view.line(n);
                            rsx! {
                                div { key: "{n}", class: "{class}",
                                    span { class: "cv-gutter",
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
                                            {line_spans(view, segs)}
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
                span {
                    class: "cv-mini-cur",
                    style: "top:{minimap_top(hit, total)}%;",
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

/// THE CODE VIEWER, props-only. See the module docs.
#[component]
#[allow(clippy::too_many_arguments)]
pub(super) fn CodeViewer(
    /// The finding the cursor is on.
    finding: ViewerFinding,
    /// Its key, for the `data-finding-key` attribute the one-cursor test reads.
    finding_key: String,
    file: FileStateView,
    /// One marker per cited line in this file.
    #[props(default)]
    marks: Vec<GutterMark>,
    /// "finding K of M in this file".
    #[props(default)]
    position: (usize, usize),
    #[props(default)] scroll_top: f64,
    #[props(default = DEFAULT_VIEWPORT_PX)] viewport_px: f64,
    #[props(default)] line_cursor: Option<usize>,
    /// The go-to-line field, when open.
    #[props(default)]
    goto: Option<String>,
    #[props(default)] goto_error: Option<String>,
    /// The disposition whose reason is being collected.
    #[props(default)]
    prompt: Option<TriageState>,
    #[props(default)] prompt_reason: String,
    #[props(default)] undo: Option<UndoBanner>,
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
    #[props(default)] on_prompt_reason: Callback<String>,
    #[props(default)] on_prompt_commit: Callback<()>,
    #[props(default)] on_prompt_cancel: Callback<()>,
    #[props(default)] on_undo: Callback<()>,
    #[props(default)] on_mark: Callback<String>,
    #[props(default)] on_line: Callback<usize>,
    #[props(default)] on_open_modal: Callback<()>,
) -> Element {
    let focus = viewer_focus(prompt.is_some(), goto.is_some());
    let armed = viewer_keys_armed(focus);
    let dom_finding_key = dom_key(&finding_key);

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
                    span { class: "cv-path", "{finding.path}" }
                    if shows_code {
                        span { class: "cv-sep", "\u{b7}" }
                        span { class: "cv-pos", "{line_label(here, total)}" }
                    }
                    if position.1 > 0 && position.0 > 0 {
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
                if let Some(s) = status.clone() {
                    div { class: "cv-status", "{s}" }
                }
                match (&file, &outcome) {
                    (FileStateView::Loading, _) => rsx! {
                        div { class: "cv-note", "Loading {finding.path}\u{2026}" }
                    },
                    (FileStateView::Failed, _) => rsx! {
                        {note_body("The server did not answer. The file could not be loaded.", Some(&finding.snippet), false, on_open_modal)}
                    },
                    (FileStateView::Loaded(v), Some(SourceOutcome::Code)) => rsx! {
                        {code_body(v, &finding, &marks, scroll_top, viewport_px, line_cursor, on_scroll, on_scroll_ready, on_mark, on_line)}
                    },
                    // Deleted source: the message and NO code, not even the stored line.
                    (_, Some(SourceOutcome::SourceDeleted(text))) => rsx! {
                        {note_body(text, None, false, on_open_modal)}
                    },
                    (_, Some(SourceOutcome::TooLarge(text))) => rsx! {
                        {note_body(text, Some(&finding.snippet), true, on_open_modal)}
                    },
                    (_, Some(SourceOutcome::NotUtf8(text)))
                    | (_, Some(SourceOutcome::FileMissing(text)))
                    | (_, Some(SourceOutcome::Unavailable(text))) => rsx! {
                        {note_body(text, Some(&finding.snippet), false, on_open_modal)}
                    },
                    _ => rsx! {},
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
                if let Some(u) = undo.clone() {
                    div { class: "pane-undo cv-undo",
                        span { class: "pane-undo-msg", "{u.message}" }
                        button {
                            class: "btn-edit-sm pane-undo-btn",
                            onclick: move |_| on_undo.call(()),
                            "Undo"
                        }
                        span { class: "pane-undo-left", "{u.seconds_left}s" }
                    }
                }
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
                    if armed {
                        "C confirm \u{b7} T tech debt \u{b7} I ignore \u{b7} F false positive \u{b7} ] [ finding \u{b7} }} {{ file \u{b7} G line \u{b7} . this finding \u{b7} ? all keys \u{b7} Esc close"
                    } else {
                        "Keys are off while you type. Enter commits, Esc cancels."
                    }
                }
            }
        }
    }
}

// ── The host ─────────────────────────────────────────────────────────────────────────────

/// Put the page's focus back on the viewer root (after a prompt closes).
fn focus_viewer() {
    dioxus::document::eval(&format!(
        "var el=document.getElementById('{VIEWER_ID}');if(el)el.focus();"
    ));
}

/// Set the scroll container's `scrollTop`.
fn set_dom_scroll(px: f64) {
    dioxus::document::eval(&format!(
        "var e=document.getElementById('{SCROLL_ID}');if(e)e.scrollTop={px};"
    ));
}

/// The collapsed groups that hide `key`'s row. The queue is grouped rule group then file, and
/// every group starts collapsed; a collapsed row is not in chorale's visible view, and a cursor
/// aimed at it would clamp to a neighbour. At most two groups (the rule group, then the file).
pub(super) fn collapsed_groups_hiding(
    state: &chorale_core::TableState<FindingView>,
    key: &str,
) -> Vec<GroupKey> {
    let Some((_, row)) = state.rows.iter().find(|(_, f)| finding_key(f) == key) else {
        return Vec::new();
    };
    let value = |id| {
        state
            .columns
            .iter()
            .find(|c| c.id == id)
            .map(|c| (c.accessor)(row).to_csv_string())
    };
    let (Some(rule), Some(file)) = (
        value(RULE_GROUP_COLUMN),
        value(chorale_core::ColumnId("file")),
    ) else {
        return Vec::new();
    };
    [
        GroupKey::from_values(std::slice::from_ref(&rule)),
        GroupKey::from_values(&[rule, file]),
    ]
    .into_iter()
    .filter(|g| state.collapsed_groups.contains(g))
    .collect()
}

/// Expand whatever groups hide `key`'s row, so the cursor can land on it. Only the groups that
/// hold the row are opened; the rest of the table stays as the curator left it.
pub(super) fn expand_groups_for(handle: UseTableHandle<FindingView>, key: &str) {
    let hiding = collapsed_groups_hiding(&handle.signal().peek(), key);
    for group in hiding {
        handle.toggle_group(group);
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
    pub project_id: String,
    pub open: Signal<bool>,
    pub undo_entry: Signal<Option<UndoEntry>>,
    pub now_ms: Signal<u64>,
    pub on_undo: Callback<()>,
    /// The pane's own disposition path. The viewer calls it; it has none of its own.
    pub apply_disposition: Callback<(TriageState, String)>,
    /// Close the viewer and put DOM focus back on the pane.
    pub on_close: Callback<()>,
    /// Open the finding modal on the focused finding (the "too large" fallback surface).
    pub on_open_modal: Callback<()>,
}

impl PartialEq for ViewerLink {
    fn eq(&self, other: &Self) -> bool {
        self.handle == other.handle
            && self.cursor == other.cursor
            && self.visible_keys == other.visible_keys
            && self.dispositions == other.dispositions
            && Arc::ptr_eq(&self.all_findings, &other.all_findings)
            && Arc::ptr_eq(&self.shas, &other.shas)
            && self.project_id == other.project_id
            && self.open == other.open
            && self.undo_entry == other.undo_entry
            && self.now_ms == other.now_ms
            && self.on_undo == other.on_undo
            && self.apply_disposition == other.apply_disposition
            && self.on_close == other.on_close
            && self.on_open_modal == other.on_open_modal
    }
}

/// Where the Queue registers its [`ViewerLink`]. Provided by the results view.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct ViewerSlot(pub Signal<Option<ViewerLink>>);

/// The viewer's host at the results root. Renders nothing until the Queue has registered a link
/// and opened the viewer; then it resolves the finding the cursor is on and mounts
/// [`CodeViewerHost`] on it, keyed by that finding.
#[component]
pub(super) fn ViewerRoot() -> Element {
    let slot = try_use_context::<ViewerSlot>();
    let cache = try_use_context::<Signal<ViewerCache>>();
    let local_cache = use_signal(ViewerCache::new);
    let cache = cache.unwrap_or(local_cache);
    let mut history = use_signal(Vec::<HistoryEntry>::new);
    let mut status = use_signal(|| Option::<String>::None);
    let mut help = use_signal(|| false);

    let Some(link) = slot.and_then(|s| s.0.read().clone()) else {
        return rsx! {};
    };
    if !*link.open.read() {
        return rsx! {};
    }
    // The ONE cursor, resolved the way the pane resolves it.
    let resolved = link.cursor.read().resolve(&link.visible_keys.read());
    let finding = resolved.key().and_then(|k| {
        link.all_findings
            .iter()
            .find(|f| finding_key(f) == k)
            .cloned()
    });
    let undo = undo_banner_of(&link.undo_entry.read(), *link.now_ms.read());
    let close = link.on_close;
    let on_close = Callback::new(move |()| {
        // A new viewing starts with a clean history and no stale status line.
        history.write().clear();
        status.set(None);
        help.set(false);
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
            history,
            status,
            help,
            undo,
            on_close,
        }
    }
}

/// THE STATEFUL VIEWER. Mounted while the viewer is open, keyed by the focused finding's key.
#[component]
pub(super) fn CodeViewerHost(
    /// The finding the queue's cursor is on.
    finding: FindingView,
    link: ViewerLink,
    cache: Signal<ViewerCache>,
    history: Signal<Vec<HistoryEntry>>,
    status: Signal<Option<String>>,
    help: Signal<bool>,
    undo: Option<UndoBanner>,
    on_close: Callback<()>,
) -> Element {
    let ViewerLink {
        handle,
        cursor,
        visible_keys,
        dispositions,
        all_findings,
        shas,
        project_id,
        on_undo,
        apply_disposition,
        on_open_modal,
        ..
    } = link;
    let key = finding_key(&finding);
    let sha = shas.get(&finding.repo).cloned().unwrap_or_default();
    let violation_line = finding.line;
    let mut cursor = cursor;
    let mut status = status;
    let mut help = help;

    // Per-finding state. The host is keyed by the finding, so all of this starts clean when
    // the cursor moves.
    let mut scroll_top = use_signal(|| {
        scroll_top_for_line(
            violation_line,
            CODE_LINE_H_PX,
            DEFAULT_VIEWPORT_PX,
            usize::MAX / 64,
        )
    });
    let mut viewport = use_signal(|| DEFAULT_VIEWPORT_PX);
    let mut line_cursor = use_signal(|| Option::<usize>::None);
    let mut goto = use_signal(|| Option::<String>::None);
    let mut goto_error = use_signal(|| Option::<String>::None);
    let mut prompt = use_signal(|| Option::<TriageState>::None);
    let mut prompt_reason = use_signal(String::new);
    let mut around = use_signal(|| Option::<usize>::None);

    let file_key = FileKey {
        repo: finding.repo.clone(),
        sha: sha.clone(),
        path: finding.path.clone(),
    };

    // The fetch. A cached file answers without a request; the answer is also written to the
    // cache (read-only git reads only), which is what makes the next visit instant.
    let file_res = use_resource({
        let finding = finding.clone();
        let sha = sha.clone();
        let project_id = project_id.clone();
        let file_key = file_key.clone();
        move || {
            let wanted = around().unwrap_or(finding.line);
            let (repo, path) = (finding.repo.clone(), finding.path.clone());
            let (sha, project_id, file_key) = (sha.clone(), project_id.clone(), file_key.clone());
            let mut cache = cache;
            async move {
                if let Some(hit) = cache.write().get(&file_key, wanted) {
                    return Some(hit);
                }
                let got =
                    fetch_source_file(&repo, &path, Some(&sha), Some(wanted), Some(&project_id))
                        .await?;
                let view = Arc::new(got);
                cache.write().put(file_key, Arc::clone(&view));
                Some(view)
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

    // What the render shows.
    let wanted = around().unwrap_or(violation_line);
    let cached = cache.peek().lookup(&file_key, wanted);
    let file_state = match cached.or_else(|| file_res.read().clone().flatten()) {
        Some(v) => FileStateView::Loaded(v),
        None if file_res.read().is_none() => FileStateView::Loading,
        None => FileStateView::Failed,
    };
    let total_lines = match &file_state {
        FileStateView::Loaded(v) => v.total(),
        _ => 0,
    };

    let ordered = {
        let state = handle.signal();
        let guard = state.read();
        let rows = chorale_core::filtered_sorted_rows(&guard);
        drop(guard);
        rows
    };
    let items = file_findings(
        &all_findings,
        &dispositions.read(),
        &finding.repo,
        &finding.path,
        &navigable_keys(&ordered),
    );
    let marks = gutter_marks(&items);
    let position = finding_position(&items, &key);

    // ── Actions ──

    // Aim the queue's cursor at a finding. The cursor is the only notion of "current".
    let goto_finding = Callback::new(move |target: String| {
        expand_groups_for(handle, &target);
        cursor.set(RowCursor::focused_on(target));
        status.set(None);
    });

    let scroll_to_line = Callback::new(move |line: usize| {
        let top = scroll_top_for_line(line, CODE_LINE_H_PX, *viewport.peek(), total_lines);
        scroll_top.set(top);
        set_dom_scroll(top);
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
            let target = advance_after_disposition(&before, &key);
            apply_disposition.call((state, reason));
            let landed = dispositions.peek().get(&key).map(|d| d.state) == Some(state);
            if !landed {
                return;
            }
            prompt.set(None);
            prompt_reason.set(String::new());
            if let Some(t) = target {
                goto_finding.call(t);
            }
            focus_viewer();
        }
    });

    let on_key = Callback::new({
        let key = key.clone();
        let finding = finding.clone();
        let all_for_keys = all_findings.clone();
        move |(k, m): (String, KeyMods)| {
            let focus = viewer_focus(prompt.peek().is_some(), goto.peek().is_some());
            let Some(cmd) = viewer_command(focus, &k, m) else {
                return;
            };
            match cmd {
                ViewerCommand::Close => on_close.call(()),
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
                        FileStep::Move(t) => goto_finding.call(t),
                        FileStep::Stay(msg) => status.set(Some(msg)),
                    }
                }
                ViewerCommand::FileInGroup(dir) => {
                    let ordered = ordered_rows(handle);
                    match step_file_in_group(&ordered, &dispositions.peek(), &finding, dir) {
                        FileStep::Move(t) => goto_finding.call(t),
                        FileStep::Stay(msg) => status.set(Some(msg)),
                    }
                }
                ViewerCommand::BackToFinding => {
                    line_cursor.set(None);
                    scroll_to_line.call(violation_line);
                }
                ViewerCommand::OpenGoToLine => {
                    goto.set(Some(String::new()));
                    goto_error.set(None);
                }
                ViewerCommand::Back => {
                    let entry = history.write().pop();
                    match entry {
                        Some(e) if e.key == key => {
                            line_cursor.set(Some(e.line));
                            scroll_to_line.call(e.line);
                        }
                        Some(e) => goto_finding.call(e.key),
                        None => status.set(Some("Nothing to go back to.".to_string())),
                    }
                }
                ViewerCommand::Scroll(sk) => {
                    let top = scrolled(
                        *scroll_top.peek(),
                        sk,
                        *viewport.peek(),
                        total_lines,
                        CODE_LINE_H_PX,
                    );
                    scroll_top.set(top);
                    set_dom_scroll(top);
                }
                ViewerCommand::ToggleHelp => {
                    let now = *help.peek();
                    help.set(!now);
                }
            }
        }
    });

    let on_scroll = Callback::new({
        let file_state = file_state.clone();
        move |(top, client_h): (f64, f64)| {
            // Re-render only when the first rendered line would change.
            let step = |t: f64| line_at_scroll(t, CODE_LINE_H_PX) / SCROLL_QUANTUM_LINES;
            if step(top) != step(*scroll_top.peek()) {
                scroll_top.set(top);
            }
            if client_h > 50.0 && (client_h - *viewport.peek()).abs() > 1.0 {
                viewport.set(client_h);
            }
            // A file delivered as a window: ask for a new one when the viewport nears an edge.
            if let FileStateView::Loaded(v) = &file_state {
                if let Some(w) = v.window.as_ref() {
                    if let Some(centre) = page_request(w, top, *viewport.peek(), CODE_LINE_H_PX) {
                        if *around.peek() != Some(centre) {
                            around.set(Some(centre));
                        }
                    }
                }
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
            if line_cursor.peek().is_none() {
                let top = scroll_top_for_line(violation_line, CODE_LINE_H_PX, h, total_lines);
                scroll_top.set(top);
                set_dom_scroll(top);
            }
        });
    });

    let commit_goto = Callback::new({
        let key = key.clone();
        move |()| {
            let text = goto.peek().clone().unwrap_or_default();
            match parse_goto_line(&text, total_lines) {
                Ok(line) => {
                    let from = line_cursor.peek().unwrap_or(violation_line);
                    push_history(
                        &mut history.write(),
                        HistoryEntry {
                            key: key.clone(),
                            line: from,
                        },
                    );
                    line_cursor.set(Some(line));
                    scroll_to_line.call(line);
                    goto.set(None);
                    goto_error.set(None);
                    focus_viewer();
                }
                Err(msg) => goto_error.set(Some(msg)),
            }
        }
    });

    rsx! {
        CodeViewer {
            finding: ViewerFinding::from_finding(&finding, &sha),
            finding_key: key.clone(),
            file: file_state,
            marks,
            position,
            scroll_top: scroll_top(),
            viewport_px: viewport(),
            line_cursor: line_cursor(),
            goto: goto(),
            goto_error: goto_error(),
            prompt: prompt(),
            prompt_reason: prompt_reason(),
            undo,
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
            on_undo,
            on_mark: goto_finding,
            on_line: Callback::new(move |n: usize| line_cursor.set(Some(n))),
            on_open_modal,
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
    use camerata_ui_core::viewer::{BlockView, WindowView};

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
        }
    }

    fn harness(case: Case) -> Element {
        rsx! {
            CodeViewer {
                finding: finding(),
                finding_key: "the-key".to_string(),
                file: case.file,
                marks: case.marks,
                position: case.position,
                scroll_top: case.scroll_top,
                prompt: case.prompt,
                goto: case.goto,
                help: case.help,
                status: case.status,
                line_cursor: case.line_cursor,
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
        assert_eq!(count(&html, "disabled"), 2);
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
                project_id: String::new(),
                open,
                undo_entry: use_signal(|| None),
                now_ms: use_signal(|| 0),
                on_undo: Callback::default(),
                apply_disposition: Callback::default(),
                on_close: Callback::default(),
                on_open_modal: Callback::default(),
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
    fn a_response_the_viewer_does_not_recognise_renders_an_honest_note() {
        let html = render(Case {
            file: refusal(serde_json::json!({})),
            ..Case::default()
        });
        assert!(html.contains("could not be loaded"));
        let _ = (WindowView::default(), BlockView::default());
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
    //! The one piece of table plumbing the viewer needs: reaching a row whose group is collapsed.

    use super::*;
    use chorale_core::{
        collapse_all_groups, set_grouping, toggle_group, visible_grouped_view, ColumnId,
        GroupedRow, RowId, TableState,
    };

    fn finding(rule: &str, path: &str, line: usize) -> FindingView {
        serde_json::from_value(serde_json::json!({
            "repo": "me/api", "path": path, "line": line, "rule_id": rule, "severity": "high",
            "snippet": format!("s{line}"), "detail": "d", "status": "active", "kind": "defect"
        }))
        .expect("fixture")
    }

    fn collapsed_queue(rows: &[FindingView]) -> TableState<FindingView> {
        let pairs = rows.iter().map(|f| (RowId::new(), f.clone())).collect();
        let state = super::super::scan::finding_table_state(pairs, vec!["me/api".into()], false);
        let state = set_grouping(&state, vec![RULE_GROUP_COLUMN, ColumnId("file")]);
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

    #[test]
    fn opening_the_two_groups_that_hold_a_row_makes_it_reachable_and_nothing_else() {
        let rows = vec![
            finding("R-A", "a.rs", 10),
            finding("R-A", "b.rs", 20),
            finding("R-B", "c.rs", 30),
        ];
        let mut state = collapsed_queue(&rows);
        assert!(visible(&state).is_empty(), "everything starts collapsed");
        let target = finding_key(&rows[1]);

        let hiding = collapsed_groups_hiding(&state, &target);
        assert_eq!(hiding.len(), 2, "the rule group and the file");
        for g in &hiding {
            state = toggle_group(&state, g);
        }
        // Only that file's row appeared: its sibling file and the other rule stay collapsed.
        assert_eq!(visible(&state), vec![target.clone()]);
        // Nothing left hiding it, so a second call is a no-op.
        assert!(collapsed_groups_hiding(&state, &target).is_empty());
        // A key that is not in the table hides nothing.
        assert!(collapsed_groups_hiding(&state, "nope").is_empty());
    }

    #[test]
    fn a_row_in_an_already_open_group_needs_only_the_closed_one() {
        let rows = vec![finding("R-A", "a.rs", 10), finding("R-A", "b.rs", 20)];
        let mut state = collapsed_queue(&rows);
        let first = collapsed_groups_hiding(&state, &finding_key(&rows[0]));
        // Open the rule group only (the first of the two keys).
        state = toggle_group(&state, &first[0]);
        let hiding = collapsed_groups_hiding(&state, &finding_key(&rows[1]));
        assert_eq!(hiding.len(), 1, "just the file group");
    }
}
