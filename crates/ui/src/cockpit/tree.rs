//! The tree lane (review UI spec section 5; design A6 and section 7).
//!
//! Two layers, per spec rule 1:
//!
//! - [`TreeScreen`] and the small views under it are props-only. Every judgment they draw (the
//!   counts line, the chips, which keys are armed, the batch sentence) comes from
//!   `camerata_ui_core::tree`; they choose words and pixels. The gallery renders them from
//!   fixtures and the tests render them from props.
//! - [`TreeHost`] owns the signals and the I/O. It reads through `ReviewApi` (never a request
//!   inside a view), loads a node's sites 500 at a time when the node opens (spec U1), and writes
//!   decisions and summary edits through the same handle.
//!
//! # Why the site rows are not a `CamerataTable`
//!
//! The spec draws the left column as one grouped `CamerataTable`. Three things in it do not fit
//! chorale 0.2.3, which is why the site rows are a plain table here (the node headers, the
//! prompts and the keyboard map all reuse the Queue's classes and idioms):
//!
//! 1. A chorale group header carries no host content (the Queue's own comment on
//!    `RULE_GROUP_COLUMN` says the same), so a node's chevron, check and counts line cannot sit
//!    in it.
//! 2. A chorale cell renderer is `Fn + Send + Sync` and cannot hold a Dioxus `Callback`, so the
//!    tick checkbox on an outlier row and a row's focus cannot be a cell.
//! 3. The keyboard cursor walks the order the server returned the sites in, and a node pages by
//!    500 under its own header; a sortable, filterable table re-orders rows under the cursor.
//!
//! So sort and filter are not offered on these rows: the sites arrive in the server's fixed order
//! (severity, path, line), which is the order the spec's Severity and File:line sorts give.

use std::collections::BTreeSet;
use std::rc::Rc;

use camerata_api_types::review_view::{SiteRow, TreeNodeView, TreeView, SITES_PAGE_SIZE};
use camerata_ui_core::tree::batch::{
    batch_allowed, batch_blocked_reason, batch_heading, batch_plan, decision_request,
    decision_state_for, plan_sentence, site_decision_request, stale_summary_note,
    summary_edit_request, summary_text, BatchPlan, TreeUndo,
};
use camerata_ui_core::tree::export::Door;
use camerata_ui_core::tree::keys::{keys_hint, tree_command, tree_keys_armed, TreeCommand};
use camerata_ui_core::tree::shell::{lane_for_key, NOT_GROUP_REVIEWED, NO_INSPECTION};
use camerata_ui_core::tree::state::{
    Load, LoadedSites, SitesRequest, TreeIndex, TreeRef, TreeState,
};
use camerata_ui_core::tree::{
    count_terms, decision_cell, dom_token, file_line, indent_px, is_fully_decided, is_outlier,
    kind_tag_of, marker_cell, marker_chips, severity_badge, severity_label, stage1_cell,
    stage2_cell, thousands, CountTone, DecisionTone,
};
use camerata_ui_core::triage::{KeyMods, PaneCommand, PaneFocus, TriageState};
use dioxus::prelude::*;

use super::scan::kind_chip_parts;
use camerata_ui::review_io::{fetch_page, read_all_sites, ReviewApi};

/// The DOM id of the tree's focusable region. Tabbing into it arms the keys.
pub(crate) const TREE_REGION_ID: &str = "camerata-tree-region";

// ── What the screen shows ─────────────────────────────────────────────────────────────────

/// How the left column stands (spec section 5, "States").
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum TreeStatus {
    /// The inspection has no sites: "No inspection to review."
    Empty,
    /// The tree request is in flight: five gray skeleton rows.
    Loading,
    /// The tree request failed, with the server's reason. A tree that had loaded stays.
    Failed(String),
    /// The tree is there.
    Ready,
}

/// A prompt open in the right pane.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum TreePrompt {
    /// Reading every page of a node to work out what a batch would write.
    Planning {
        node: String,
        state: TriageState,
        read: usize,
        total: usize,
    },
    /// The plan is in; the reviewer types the reason.
    Batch {
        node: String,
        state: TriageState,
        plan: BatchPlan,
        reason: String,
        error: Option<String>,
    },
    /// The sites could not be read, so there is no plan.
    PlanFailed { node: String, message: String },
    /// Editing a node's summary.
    Summary {
        node: String,
        text: String,
        reason: String,
        error: Option<String>,
    },
    /// The reason for one site's Ignore or False positive.
    Site {
        node: String,
        key: String,
        state: TriageState,
        reason: String,
        error: Option<String>,
    },
}

impl TreePrompt {
    /// The node the prompt is about (for a site, the node it sits under).
    pub(crate) fn node(&self) -> &str {
        match self {
            Self::Planning { node, .. }
            | Self::Batch { node, .. }
            | Self::PlanFailed { node, .. }
            | Self::Summary { node, .. }
            | Self::Site { node, .. } => node,
        }
    }
}

/// The undo bar as drawn.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct UndoBanner {
    pub message: String,
    pub seconds_left: u64,
}

/// The two reasons the screen is not just the tree.
const SKELETON_ROWS: usize = 5;

// ── The screen ────────────────────────────────────────────────────────────────────────────

/// The whole tree lane: the tree on the left, the focused node or site on the right.
#[component]
pub(crate) fn TreeScreen(
    index: Rc<TreeIndex>,
    state: TreeState,
    status: TreeStatus,
    group_reviewed: bool,
    focus: PaneFocus,
    prompt: Option<TreePrompt>,
    undo: Option<UndoBanner>,
    /// The window is narrow, so the right pane sits under the tree (spec U6).
    #[props(default)]
    narrow: bool,
    /// Every keydown on the tree, as `(key, modifiers)`. The host runs it through
    /// `tree_command`; this component never decides what a key means.
    #[props(default)]
    on_key: Callback<(String, KeyMods)>,
    #[props(default)] on_focus: Callback<PaneFocus>,
    #[props(default)] on_toggle: Callback<String>,
    #[props(default)] on_focus_node: Callback<String>,
    #[props(default)] on_focus_site: Callback<(String, String)>,
    #[props(default)] on_tick: Callback<String>,
    #[props(default)] on_more: Callback<String>,
    #[props(default)] on_retry_node: Callback<String>,
    #[props(default)] on_retry: Callback<()>,
    #[props(default)] on_batch: Callback<TriageState>,
    #[props(default)] on_decide_site: Callback<TriageState>,
    #[props(default)] on_edit_summary: Callback<()>,
    #[props(default)] on_prompt_reason: Callback<String>,
    #[props(default)] on_prompt_text: Callback<String>,
    #[props(default)] on_prompt_commit: Callback<()>,
    #[props(default)] on_prompt_cancel: Callback<()>,
    #[props(default)] on_undo: Callback<()>,
) -> Element {
    let armed = tree_keys_armed(focus);
    let prompt_open = prompt.is_some();
    let cursor_kind = state.cursor_kind();
    let blocks = tree_blocks(&index, &state);
    let focused_node = match &state.cursor {
        Some(TreeRef::Node(id)) => index.node(id).cloned(),
        _ => None,
    };
    let focused_site = match &state.cursor {
        Some(TreeRef::Site { node, key }) => {
            state.site(node, key).cloned().map(|r| (node.clone(), r))
        }
        _ => None,
    };
    let narrow_class = if narrow {
        "tree-lane narrow"
    } else {
        "tree-lane"
    };
    rsx! {
        div { class: "{narrow_class}",
            div { class: "tree-left",
                if !group_reviewed && status == TreeStatus::Ready {
                    p { class: "review-note", "{NOT_GROUP_REVIEWED}" }
                }
                if let TreeStatus::Failed(message) = &status {
                    div { class: "review-banner", role: "alert",
                        span { class: "review-banner-msg", "{message}" }
                        button { class: "btn-edit-sm", onclick: move |_| on_retry.call(()), "Retry" }
                    }
                }
                {
                    match &status {
                        TreeStatus::Empty => rsx! {
                            p { class: "pane-empty", "{NO_INSPECTION}" }
                        },
                        TreeStatus::Loading => rsx! {
                            p { class: "review-loading", "Loading the tree" }
                            div { class: "tree-skel",
                                for n in 0..SKELETON_ROWS {
                                    div { key: "{n}", class: "tree-skel-row" }
                                }
                            }
                        },
                        _ => rsx! {},
                    }
                }
                if !index.is_empty() {
                    div {
                        class: "tree-region",
                        id: "{TREE_REGION_ID}",
                        // A focusable REGION: tabbing in arms the keys, tabbing out disarms them.
                        tabindex: "0",
                        onfocusin: move |_| on_focus.call(PaneFocus::Cursor),
                        onfocusout: move |_| on_focus.call(PaneFocus::Elsewhere),
                        onkeydown: move |e: KeyboardEvent| {
                            let m = e.modifiers();
                            let mods = KeyMods {
                                ctrl: m.contains(Modifiers::CONTROL),
                                alt: m.contains(Modifiers::ALT),
                                meta: m.contains(Modifiers::META),
                                shift: m.contains(Modifiers::SHIFT),
                            };
                            let key = e.key().to_string();
                            // Swallow only the presses the tree claims, so Tab still tabs and
                            // Cmd-C still copies. The claim test is `tree_command` itself.
                            if tree_command(focus, &key, mods, cursor_kind).is_some() {
                                e.prevent_default();
                                e.stop_propagation();
                            }
                            on_key.call((key, mods));
                        },
                        for block in blocks {
                            {
                                match block {
                                    Block::Node { node, open, focused } => rsx! {
                                        NodeRow {
                                            key: "n-{node.id}",
                                            node,
                                            open,
                                            focused,
                                            on_toggle,
                                            on_focus: on_focus_node,
                                        }
                                    },
                                    Block::Sites { node, loaded, cursor_key } => rsx! {
                                        SiteTable {
                                            key: "s-{node}",
                                            node,
                                            loaded,
                                            cursor_key,
                                            ticked: state.ticked.clone(),
                                            on_focus_site,
                                            on_tick,
                                            on_more,
                                            on_retry: on_retry_node,
                                        }
                                    },
                                }
                            }
                        }
                    }
                    div { class: "pane-actions tree-keys",
                        span {
                            class: if armed { "pane-keys armed" } else { "pane-keys off" },
                            "{keys_hint(armed, prompt_open)}"
                        }
                    }
                }
            }
            div { class: "tree-right",
                if let Some(banner) = undo {
                    TreeUndoBar { banner, on_undo }
                }
                if let Some(node) = focused_node {
                    NodeSide {
                        crumbs: index.breadcrumb(&node.id),
                        node,
                        prompt: prompt.clone(),
                        on_batch,
                        on_edit_summary,
                        on_prompt_reason,
                        on_prompt_text,
                        on_prompt_commit,
                        on_prompt_cancel,
                    }
                } else if let Some((node, row)) = focused_site {
                    SitePane {
                        position: site_position(&state, &node, &row),
                        node_label: index.node(&node).map(|n| n.label.clone()).unwrap_or_default(),
                        ticked: state.ticked.contains(&row.site_key),
                        row,
                        prompt: prompt.clone(),
                        on_tick,
                        on_decide: on_decide_site,
                        on_prompt_reason,
                        on_prompt_commit,
                        on_prompt_cancel,
                    }
                } else {
                    p { class: "pane-empty", "Select a node or a site." }
                }
            }
        }
    }
}

/// `7 of 12` for the site the cursor is on.
fn site_position(state: &TreeState, node: &str, row: &SiteRow) -> String {
    let Some(loaded) = state.loaded.get(node) else {
        return String::new();
    };
    let at = loaded
        .rows
        .iter()
        .position(|r| r.site_key == row.site_key)
        .map_or(0, |p| p + 1);
    format!("{at} of {}", thousands(loaded.total.max(loaded.rows.len())))
}

// ── Blocks ────────────────────────────────────────────────────────────────────────────────

/// One piece of the left column: a node header, or the sites of an open sub-group under it.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Block {
    Node {
        node: TreeNodeView,
        open: bool,
        focused: bool,
    },
    Sites {
        node: String,
        loaded: LoadedSites,
        cursor_key: Option<String>,
    },
}

/// The left column as blocks, in order: only what shows (the top level, and under an open node
/// its children, or for an open sub-group its sites).
pub(crate) fn tree_blocks(index: &TreeIndex, state: &TreeState) -> Vec<Block> {
    let mut out = Vec::new();
    let mut stack: Vec<String> = index.roots().map(|n| n.id.clone()).collect();
    stack.reverse();
    while let Some(id) = stack.pop() {
        let Some(node) = index.node(&id) else {
            continue;
        };
        let open = state.is_open(&id);
        out.push(Block::Node {
            node: node.clone(),
            open,
            focused: state.cursor == Some(TreeRef::Node(id.clone())),
        });
        if !open {
            continue;
        }
        if index.holds_sites(&id) {
            if let Some(loaded) = state.loaded.get(&id) {
                let cursor_key = match &state.cursor {
                    Some(TreeRef::Site { node, key }) if *node == id => Some(key.clone()),
                    _ => None,
                };
                out.push(Block::Sites {
                    node: id.clone(),
                    loaded: loaded.clone(),
                    cursor_key,
                });
            }
        } else {
            let kids: Vec<String> = index.children(&id).map(|n| n.id.clone()).collect();
            stack.extend(kids.into_iter().rev());
        }
    }
    out
}

// ── A node header ─────────────────────────────────────────────────────────────────────────

/// One node of the tree: chevron, a green check when every site is decided, the label and the
/// counts line (spec section 2).
#[component]
fn NodeRow(
    node: TreeNodeView,
    open: bool,
    focused: bool,
    on_toggle: Callback<String>,
    on_focus: Callback<String>,
) -> Element {
    let token = dom_token(&node.id);
    let indent = indent_px(node.level);
    let terms = count_terms(&node.counts);
    let decided = is_fully_decided(&node.counts);
    let id_toggle = node.id.clone();
    let id_focus = node.id.clone();
    let class = match (focused, node.thrown_out) {
        (true, true) => "tree-node focused thrown-out",
        (true, false) => "tree-node focused",
        (false, true) => "tree-node thrown-out",
        (false, false) => "tree-node",
    };
    let flags = node.flags.join("\n");
    let n_flags = node.flags.len();
    rsx! {
        div {
            class: "{class}",
            id: "tree-node-{token}",
            "data-node": "{token}",
            style: "margin-left: {indent}px",
            onclick: move |_| on_focus.call(id_focus.clone()),
            button {
                class: "tree-chevron",
                title: if open { "Close" } else { "Open" },
                "aria-expanded": "{open}",
                onclick: move |e| {
                    e.stop_propagation();
                    on_toggle.call(id_toggle.clone());
                },
                if open { "v" } else { ">" }
            }
            if decided {
                span { class: "tree-check", title: "Every site here is decided", "\u{2713}" }
            }
            span { class: "tree-label", title: "{node.label}", "{node.label}" }
            if n_flags > 0 {
                span { class: "tree-flag", title: "{flags}",
                    if n_flags == 1 { "1 flag" } else { "{n_flags} flags" }
                }
            }
            span { class: "tree-counts",
                for (i, term) in terms.into_iter().enumerate() {
                    span {
                        key: "{i}",
                        class: match term.tone {
                            CountTone::Plain => "",
                            CountTone::Amber => "count-amber",
                            CountTone::Orange => "count-orange",
                            CountTone::Red => "count-red",
                        },
                        if i > 0 { "\u{b7} " }
                        "{term.text}"
                    }
                }
            }
        }
    }
}

// ── The sites of a sub-group ──────────────────────────────────────────────────────────────

/// The sites loaded so far under an open sub-group, with "Show next 500" under them.
#[component]
fn SiteTable(
    node: String,
    loaded: LoadedSites,
    cursor_key: Option<String>,
    ticked: BTreeSet<String>,
    on_focus_site: Callback<(String, String)>,
    on_tick: Callback<String>,
    on_more: Callback<String>,
    on_retry: Callback<String>,
) -> Element {
    let indent = indent_px(camerata_api_types::review_view::TreeLevel::Subgroup) + 16;
    let node_token = dom_token(&node);
    let more_label = loaded.show_more_label();
    let first_load = loaded.rows.is_empty() && loaded.load == Load::Loading;
    rsx! {
        div { class: "tree-sites", style: "margin-left: {indent}px", "data-node": "{node_token}",
            if first_load {
                p { class: "sites-loading", "Loading sites" }
            }
            if !loaded.rows.is_empty() {
                table { class: "site-table",
                    thead {
                        tr {
                            th { class: "col-tick", "" }
                            th { "Markers" }
                            th { "Severity" }
                            th { "Kind" }
                            th { "File:line" }
                            th { "Headline" }
                            th { "Stage 1" }
                            th { "Stage 2" }
                            th { "Decision" }
                        }
                    }
                    tbody {
                        for row in loaded.rows.iter().cloned() {
                            SiteRowView {
                                key: "{dom_token(&row.site_key)}",
                                node: node.clone(),
                                focused: cursor_key.as_deref() == Some(row.site_key.as_str()),
                                ticked: ticked.contains(&row.site_key),
                                row,
                                on_focus_site,
                                on_tick,
                            }
                        }
                    }
                }
            }
            {
                match &loaded.load {
                    Load::Failed(message) => rsx! {
                        div { class: "sites-error",
                            span { "{message}" }
                            button {
                                class: "btn-edit-sm",
                                onclick: {
                                    let node = node.clone();
                                    move |_| on_retry.call(node.clone())
                                },
                                "Retry"
                            }
                        }
                    },
                    Load::Loading if !first_load => rsx! {
                        p { class: "sites-loading", "Loading the next sites" }
                    },
                    _ => rsx! {},
                }
            }
            if let Some(label) = more_label {
                if loaded.load == Load::Idle {
                    div { class: "sites-more",
                        button {
                            class: "btn-edit-sm",
                            onclick: {
                                let node = node.clone();
                                move |_| on_more.call(node.clone())
                            },
                            "{label}"
                        }
                        span { "{thousands(loaded.rows.len())} of {thousands(loaded.total)} loaded" }
                    }
                }
            }
        }
    }
}

/// One site row (spec section 5, "Columns of a site row").
#[component]
fn SiteRowView(
    node: String,
    row: SiteRow,
    focused: bool,
    ticked: bool,
    on_focus_site: Callback<(String, String)>,
    on_tick: Callback<String>,
) -> Element {
    let token = dom_token(&row.site_key);
    let cell = marker_cell(&row);
    let fold = cell.fold_label();
    let fold_tip = cell.fold_tip();
    let (kind_class, kind_label, kind_tip) = kind_chip_parts(kind_tag_of(&row));
    let decision = decision_cell(&row);
    let outlier = is_outlier(&row);
    let escalated = row
        .markers
        .contains(&camerata_api_types::review_view::Marker::Escalated);
    let mut class = String::from("site-row");
    if row.severity == "critical" {
        class.push_str(" finding-row-critical");
    }
    if escalated {
        class.push_str(" row-escalated");
    }
    if row.held.is_some() {
        class.push_str(" row-held");
    }
    if focused {
        class.push_str(" focused");
    }
    let stage2 = stage2_cell(&row);
    let key_row = row.site_key.clone();
    let key_tick = row.site_key.clone();
    let node_row = node.clone();
    rsx! {
        tr {
            class: "{class}",
            id: "site-{token}",
            "data-site": "{token}",
            "data-node": "{dom_token(&node)}",
            onclick: move |_| on_focus_site.call((node_row.clone(), key_row.clone())),
            td { class: "col-tick",
                if outlier {
                    input {
                        r#type: "checkbox",
                        checked: ticked,
                        title: "Include this outlier in the next batch decision",
                        "aria-label": "Include this outlier in the next batch decision",
                        onclick: move |e| e.stop_propagation(),
                        onchange: move |_| on_tick.call(key_tick.clone()),
                    }
                }
            }
            td { class: "col-markers",
                span { class: "mark-cell",
                    for chip in cell.shown.iter() {
                        span { key: "{chip.class}", class: "{chip.class}", title: "{chip.tip}", "{chip.label}" }
                    }
                    if let Some(fold) = fold {
                        span { class: "mark-fold", title: "{fold_tip}", "{fold}" }
                    }
                }
            }
            td { class: "col-sev",
                span { class: "tree-badge tree-badge-{severity_badge(&row.severity)}",
                    "{severity_label(&row.severity)}"
                }
            }
            td { class: "col-kind",
                span { class: "{kind_class}", title: "{kind_tip}", "{kind_label}" }
            }
            td { class: "col-loc", title: "{row.repo}", "{file_line(&row)}" }
            td { class: "col-headline", title: "{row.headline}", "{row.headline}" }
            td { class: "col-s1", title: "{row.stage1.reason}", "{stage1_cell(&row)}" }
            td { class: "col-s2",
                title: row.stage2.as_ref().map(|s| s.why.clone()).unwrap_or_default(),
                "{stage2}"
            }
            td { class: "col-dec",
                span {
                    class: match decision.tone {
                        DecisionTone::Decided => "dec-decided",
                        DecisionTone::Unresolved => "dec-unresolved",
                    },
                    "{decision.label}"
                }
                if decision.inherited {
                    span { class: "dec-inherited", "inherited" }
                }
            }
        }
    }
}

// ── The right pane: a node ────────────────────────────────────────────────────────────────

/// The focused node: breadcrumb, summary, the four batch decisions, the counts.
#[component]
fn NodeSide(
    node: TreeNodeView,
    crumbs: Vec<(String, String)>,
    prompt: Option<TreePrompt>,
    on_batch: Callback<TriageState>,
    on_edit_summary: Callback<()>,
    on_prompt_reason: Callback<String>,
    on_prompt_text: Callback<String>,
    on_prompt_commit: Callback<()>,
    on_prompt_cancel: Callback<()>,
) -> Element {
    let allowed = batch_allowed(&node);
    let blocked = batch_blocked_reason(&node).unwrap_or_default();
    let c = node.counts;
    let crumb_text = crumbs
        .iter()
        .map(|(_, label)| label.as_str())
        .collect::<Vec<_>>()
        .join(" > ");
    let edited = node.summary_source == camerata_api_types::review_view::SummarySource::Reviewer;
    let stale_note = stale_summary_note(&node);
    let prompt_here = prompt.filter(|p| p.node() == node.id);
    let editing_summary = matches!(prompt_here, Some(TreePrompt::Summary { .. }));
    let decision_note = node.decision.as_ref().map(|d| {
        format!(
            "{} on {}: {} sites covered, {} left out. \u{201c}{}\u{201d}",
            camerata_ui_core::tree::decision_label(&d.state),
            camerata_ui_core::tree::short_date(&d.decided_at),
            thousands(d.covered),
            thousands(d.left_out),
            d.reason
        )
    });
    rsx! {
        div { class: "node-head",
            div { class: "node-crumb", "{crumb_text}" }
            h3 { class: "node-title", "{node.label}" }
            if let Some(cause) = node.cause.clone() {
                p { class: "node-summary-none", "{cause}" }
            }
            div { class: "node-summary-head",
                span { if edited { "Summary (edited by you)" } else { "Summary (model)" } }
                if !editing_summary {
                    button {
                        class: "btn-edit-sm",
                        title: "Edit this summary. It needs a reason. Press E while the tree has focus.",
                        onclick: move |_| on_edit_summary.call(()),
                        "Edit"
                    }
                }
            }
            if node.summary.is_some() {
                p { class: "node-summary", "{summary_text(&node)}" }
            } else {
                p { class: "node-summary-none", "No summary for this node." }
            }
            if let Some(note) = stale_note {
                p { class: "node-stale-note", "{note}" }
            }
            if !node.flags.is_empty() {
                div { class: "node-flags",
                    for (i, flag) in node.flags.iter().enumerate() {
                        span { key: "{i}", class: "review-flag-chip", "{flag}" }
                    }
                }
            }
            div { class: "node-actions",
                for (state, label) in BATCH_BUTTONS {
                    button {
                        key: "{label}",
                        class: "btn-edit-sm pane-dispose",
                        disabled: !allowed,
                        title: if allowed { "Decide every site here that the batch covers. It needs a reason." } else { "{blocked}" },
                        onclick: move |_| on_batch.call(state),
                        "{label}"
                    }
                }
                if allowed && (c.outliers > 0 || c.held > 0) {
                    span { class: "node-actions-note",
                        "{batch_note(c.outliers, c.held)}"
                    }
                }
            }
            if let Some(p) = prompt_here {
                TreePromptView {
                    prompt: p,
                    node_label: node.label.clone(),
                    on_reason: on_prompt_reason,
                    on_text: on_prompt_text,
                    on_commit: on_prompt_commit,
                    on_cancel: on_prompt_cancel,
                }
            }
            div { class: "node-facts",
                span { "Sites {thousands(c.sites)}" }
                if c.escalated > 0 { span { class: "count-amber", "Escalated {thousands(c.escalated)}" } }
                if c.outliers > 0 { span { class: "count-orange", "Outliers {thousands(c.outliers)}" } }
                if c.new_since_decision > 0 { span { class: "count-orange", "New since decision {thousands(c.new_since_decision)}" } }
                if c.stale > 0 { span { "Stale {thousands(c.stale)}" } }
                if c.held > 0 { span { class: "count-red", "Held {thousands(c.held)}" } }
                span { "Decided {thousands(c.decided)}" }
            }
            if let Some(note) = decision_note {
                p { class: "node-decision", "{note}" }
            }
        }
    }
}

/// The four batch decisions on a node head, with the label each carries.
const BATCH_BUTTONS: [(TriageState, &str); 4] = [
    (TriageState::Confirmed, "Confirm all"),
    (TriageState::Ignored, "Ignore all"),
    (TriageState::FalsePositive, "False positive all"),
    (TriageState::TechDebt, "Tech debt all"),
];

/// What a batch leaves out, from the node's own counts (the prompt gives the exact plan).
fn batch_note(outliers: usize, held: usize) -> String {
    let mut parts = Vec::new();
    if outliers > 0 {
        parts.push(format!(
            "{} {} excluded unless you tick {}",
            thousands(outliers),
            if outliers == 1 {
                "outlier is"
            } else {
                "outliers are"
            },
            if outliers == 1 { "it" } else { "them" }
        ));
    }
    if held > 0 {
        parts.push(format!(
            "{} held {} always excluded",
            thousands(held),
            if held == 1 { "row is" } else { "rows are" }
        ));
    }
    format!("{}.", parts.join("; "))
}

// ── Prompts ───────────────────────────────────────────────────────────────────────────────

/// The prompt that is open: the batch plan and its reason, the summary editor, or a site's
/// reason. The same amber-edged card as the Queue's (`queue-reason-prompt`): the same act.
#[component]
fn TreePromptView(
    prompt: TreePrompt,
    node_label: String,
    on_reason: Callback<String>,
    on_text: Callback<String>,
    on_commit: Callback<()>,
    on_cancel: Callback<()>,
) -> Element {
    match prompt {
        TreePrompt::Planning { read, total, .. } => rsx! {
            div { class: "queue-reason-prompt",
                p { class: "batch-read",
                    "Reading the sites to plan the batch: {thousands(read)} of {thousands(total)}."
                }
                div { class: "queue-prompt-actions",
                    button { class: "btn-edit-sm queue-prompt-cancel", onclick: move |_| on_cancel.call(()), "Cancel" }
                }
            }
        },
        TreePrompt::PlanFailed { message, .. } => rsx! {
            div { class: "queue-reason-prompt",
                p { class: "batch-error", role: "alert", "{message}" }
                div { class: "queue-prompt-actions",
                    button { class: "btn-edit-sm queue-prompt-cancel", onclick: move |_| on_cancel.call(()), "Close" }
                }
            }
        },
        TreePrompt::Batch {
            state,
            plan,
            reason,
            error,
            ..
        } => {
            let heading = batch_heading(state, &plan, &node_label);
            let sentence = plan_sentence(&plan);
            rsx! {
                div { class: "queue-reason-prompt",
                    label { class: "rule-modal-label", "{heading}" }
                    p { class: "batch-sentence", "{sentence}" }
                    ReasonField { reason, placeholder: "why (the export reads this)", on_reason, on_commit, on_cancel }
                    if let Some(error) = error {
                        p { class: "batch-error", role: "alert", "{error}" }
                    }
                    PromptButtons { commit_disabled: plan.is_empty(), on_commit, on_cancel }
                }
            }
        }
        TreePrompt::Summary {
            text,
            reason,
            error,
            ..
        } => rsx! {
            div { class: "queue-reason-prompt",
                label { class: "rule-modal-label", "Edit the summary of this node" }
                textarea {
                    class: "addressee-input summary-input",
                    rows: "6",
                    autofocus: true,
                    value: "{text}",
                    oninput: move |e| on_text.call(e.value()),
                    onkeydown: move |e: KeyboardEvent| {
                        e.stop_propagation();
                        if e.key() == Key::Escape {
                            on_cancel.call(());
                        }
                    },
                }
                label { class: "rule-modal-label", "Why the model's summary was wrong (required)" }
                ReasonField { reason, placeholder: "why (recorded with the edit)", on_reason, on_commit, on_cancel }
                if let Some(error) = error {
                    p { class: "batch-error", role: "alert", "{error}" }
                }
                PromptButtons { commit_disabled: false, on_commit, on_cancel }
            }
        },
        TreePrompt::Site {
            state,
            reason,
            error,
            ..
        } => rsx! {
            div { class: "queue-reason-prompt",
                label { class: "rule-modal-label", "Reason for \u{201c}{state.label()}\u{201d} on 1 site (required)" }
                ReasonField { reason, placeholder: "why (the export reads this)", on_reason, on_commit, on_cancel }
                if let Some(error) = error {
                    p { class: "batch-error", role: "alert", "{error}" }
                }
                PromptButtons { commit_disabled: false, on_commit, on_cancel }
            }
        },
    }
}

/// The reason field: pre-focused, Enter commits, Esc cancels, and a letter typed here is never
/// read as a decision.
#[component]
fn ReasonField(
    reason: String,
    placeholder: String,
    on_reason: Callback<String>,
    on_commit: Callback<()>,
    on_cancel: Callback<()>,
) -> Element {
    rsx! {
        textarea {
            class: "addressee-input queue-reason-input",
            rows: "2",
            placeholder: "{placeholder}",
            value: "{reason}",
            onmounted: move |e| {
                spawn(async move {
                    let _ = e.set_focus(true).await;
                });
            },
            oninput: move |e| on_reason.call(e.value()),
            onkeydown: move |e: KeyboardEvent| {
                e.stop_propagation();
                match e.key() {
                    Key::Escape => on_cancel.call(()),
                    Key::Enter if !e.modifiers().contains(Modifiers::SHIFT) => {
                        e.prevent_default();
                        on_commit.call(());
                    }
                    _ => {}
                }
            },
        }
    }
}

#[component]
fn PromptButtons(
    commit_disabled: bool,
    on_commit: Callback<()>,
    on_cancel: Callback<()>,
) -> Element {
    rsx! {
        div { class: "queue-prompt-actions",
            button {
                class: "btn-run queue-prompt-commit",
                disabled: commit_disabled,
                onclick: move |_| on_commit.call(()),
                "Commit \u{2192}"
            }
            button { class: "btn-edit-sm queue-prompt-cancel", onclick: move |_| on_cancel.call(()), "Cancel" }
        }
    }
}

/// The five second undo bar, in the Queue's markup.
#[component]
fn TreeUndoBar(banner: UndoBanner, on_undo: Callback<()>) -> Element {
    rsx! {
        div { class: "pane-undo",
            span { class: "pane-undo-msg", "{banner.message}" }
            button { class: "btn-edit-sm pane-undo-btn", onclick: move |_| on_undo.call(()), "Undo" }
            span { class: "pane-undo-left", "{banner.seconds_left}s" }
        }
    }
}

// ── The right pane: a site ────────────────────────────────────────────────────────────────

/// The focused site, read-only: the row's own facts and its decision. The extended detail pane
/// (edit a field, merge, split, add, the stale and moved actions) is the site detail's.
#[component]
fn SitePane(
    row: SiteRow,
    node_label: String,
    position: String,
    ticked: bool,
    prompt: Option<TreePrompt>,
    on_tick: Callback<String>,
    on_decide: Callback<TriageState>,
    on_prompt_reason: Callback<String>,
    on_prompt_commit: Callback<()>,
    on_prompt_cancel: Callback<()>,
) -> Element {
    let (kind_class, kind_label, kind_tip) = kind_chip_parts(kind_tag_of(&row));
    let chips = marker_chips(&row);
    let decision = decision_cell(&row);
    let outlier = is_outlier(&row);
    let key_tick = row.site_key.clone();
    let site_prompt =
        prompt.filter(|p| matches!(p, TreePrompt::Site { key, .. } if *key == row.site_key));
    let stage2_why = row
        .stage2
        .as_ref()
        .map(|s| s.why.clone())
        .filter(|w| !w.is_empty());
    let outlier_why = row
        .stage2
        .as_ref()
        .map(|s| s.outlier_why.clone())
        .filter(|w| !w.is_empty());
    rsx! {
        div { class: "node-head",
            div { class: "site-pane-head",
                span { class: "pane-rule", "{row.rule_id}" }
                span { class: "{kind_class}", title: "{kind_tip}", "{kind_label}" }
                for chip in chips {
                    span { key: "{chip.class}", class: "{chip.class}", title: "{chip.tip}", "{chip.label}" }
                }
                span { class: "pane-pos", "{position}" }
            }
            div { class: "node-crumb", "{node_label}" }
            p { class: "site-pane-line", span { class: "site-pane-label", "Headline" } "{row.headline}" }
            p { class: "site-pane-line",
                span { class: "site-pane-label", "Severity" }
                span { class: "tree-badge tree-badge-{severity_badge(&row.severity)}", "{severity_label(&row.severity)}" }
            }
            p { class: "site-pane-line",
                span { class: "site-pane-label", "Where" }
                span { class: "site-pane-code", "{file_line(&row)}" }
            }
            if !row.detail.is_empty() {
                p { class: "site-pane-line", span { class: "site-pane-label", "Detail" } "{row.detail}" }
            }
            if !row.fix.is_empty() {
                p { class: "site-pane-line", span { class: "site-pane-label", "Fix" } "{row.fix}" }
            }
            if !row.snippet.is_empty() {
                pre { class: "pane-snippet", "{row.snippet}" }
            }
            p { class: "site-pane-line",
                span { class: "site-pane-label", "Stage 1" }
                "{stage1_cell(&row)}"
                if !row.stage1.reason.is_empty() { " \u{b7} {row.stage1.reason}" }
            }
            if let Some(why) = stage2_why {
                p { class: "site-pane-line", span { class: "site-pane-label", "Stage 2" } "{stage2_cell(&row)} \u{b7} {why}" }
            }
            if let Some(why) = outlier_why {
                p { class: "site-pane-line", span { class: "site-pane-label", "Outlier" } "{why}" }
            }
            p { class: "site-pane-line",
                span { class: "site-pane-label", "Decision" }
                span {
                    class: match decision.tone {
                        DecisionTone::Decided => "dec-decided",
                        DecisionTone::Unresolved => "dec-unresolved",
                    },
                    "{decision.label}"
                }
                if decision.inherited { span { class: "dec-inherited", "inherited from its group" } }
            }
            if outlier {
                label { class: "site-pane-line",
                    input {
                        r#type: "checkbox",
                        checked: ticked,
                        onchange: move |_| on_tick.call(key_tick.clone()),
                    }
                    " Include this outlier in the next batch decision"
                }
            }
            if let Some(p) = site_prompt {
                TreePromptView {
                    prompt: p,
                    node_label,
                    on_reason: on_prompt_reason,
                    on_text: Callback::default(),
                    on_commit: on_prompt_commit,
                    on_cancel: on_prompt_cancel,
                }
            }
            div { class: "pane-actions",
                for state in [TriageState::Confirmed, TriageState::Ignored, TriageState::TechDebt, TriageState::FalsePositive] {
                    button {
                        key: "{state.label()}",
                        class: "btn-edit-sm pane-dispose",
                        onclick: move |_| on_decide.call(state),
                        "{state.label()}"
                    }
                }
            }
        }
    }
}

// ── The host ──────────────────────────────────────────────────────────────────────────────

/// A request from outside the tree to show a site (the export panel's `open`): the shell
/// switches to the Tree lane and the tree opens it.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct DoorRequest(pub Signal<Option<Door>>);

/// A counter the tree bumps after every write, so the shell re-reads its counts and its gate.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct ShellRefresh(pub Signal<u32>);

/// The tree lane with its signals and its I/O.
#[component]
pub(crate) fn TreeHost() -> Element {
    let api = use_context::<ReviewApi>();
    let refresh = try_use_context::<ShellRefresh>();
    let door = try_use_context::<DoorRequest>();
    let lanes = try_use_context::<super::review_shell::LaneControl>();
    let toasts = try_use_context::<Signal<Vec<crate::toast::Toast>>>();
    let epoch = use_hook(std::time::Instant::now);

    let mut loaded_tree = use_signal(|| Option::<TreeView>::None);
    let mut tree_error = use_signal(|| Option::<String>::None);
    let mut tree_loading = use_signal(|| true);
    let mut state = use_signal(TreeState::default);
    let mut focus = use_signal(|| PaneFocus::Elsewhere);
    let mut prompt = use_signal(|| Option::<TreePrompt>::None);
    let mut undo = use_signal(|| Option::<TreeUndo>::None);
    let mut now_ms = use_signal(|| 0u64);

    let index = use_memo(move || {
        Rc::new(
            loaded_tree
                .read()
                .as_ref()
                .map(TreeIndex::new)
                .unwrap_or_default(),
        )
    });

    // The tree: load it, and load it again after every write. What loaded stays when a reload
    // fails.
    let reload_tree = use_callback({
        let api = api.clone();
        move |()| {
            let api = api.clone();
            tree_loading.set(true);
            spawn(async move {
                match api.reads.tree().await {
                    Ok(t) => {
                        loaded_tree.set(Some(t));
                        tree_error.set(None);
                    }
                    Err(e) => tree_error.set(Some(e.message)),
                }
                tree_loading.set(false);
            });
        }
    });
    use_hook(|| reload_tree.call(()));

    // Read what the state asks for, one page per request.
    let fetch = use_callback({
        let api = api.clone();
        move |requests: Vec<SitesRequest>| {
            for req in requests {
                let api = api.clone();
                spawn(async move {
                    match fetch_page(&*api.reads, &req).await {
                        Ok(page) => state.write().apply_page(&page),
                        Err(e) => state.write().apply_failure(&req.node, &e.message),
                    }
                });
            }
        }
    });

    // After a write: the tree counts, the open sub-groups' sites and the shell's counts.
    let after_write = use_callback(move |()| {
        reload_tree.call(());
        let idx = index.peek().clone();
        let roots: Vec<String> = idx.roots().map(|n| n.id.clone()).collect();
        let mut requests = Vec::new();
        for root in roots {
            requests.extend(state.write().reload_under(&idx, &root));
        }
        fetch.call(requests);
        if let Some(mut r) = refresh {
            *r.0.write() += 1;
        }
    });

    let toast = use_callback(move |(kind, message): (crate::toast::ToastKind, String)| {
        if let Some(toasts) = toasts {
            crate::toast::push_toast(toasts, kind, message);
        }
    });

    // The undo clock: four ticks a second, only while an undo is armed.
    use_future(move || async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            if undo.peek().is_some() {
                now_ms.set(epoch.elapsed().as_millis() as u64);
            }
        }
    });

    let run_id = move || {
        loaded_tree
            .peek()
            .as_ref()
            .map(|t| t.run_id.clone())
            .unwrap_or_default()
    };

    // A door from the export panel: open the site's place in the tree and focus it.
    use_effect({
        let api = api.clone();
        move || {
            let Some(door) = door else {
                return;
            };
            let Some(wanted) = door.0.read().clone() else {
                return;
            };
            if loaded_tree.read().is_none() {
                return;
            }
            let api = api.clone();
            let mut door_slot = door.0;
            spawn(async move {
                let idx = index.peek().clone();
                match wanted {
                    Door::Node { id } => {
                        let requests = state.write().open_path(&idx, &id);
                        state.write().focus_node(&id);
                        fetch.call(requests);
                    }
                    Door::Site { key, rule_id } => {
                        if let Some(sub) = locate_site(&*api.reads, &idx, &rule_id, &key).await {
                            let requests = state.write().open_path(&idx, &sub);
                            fetch.call(requests);
                            state.write().focus_site(&sub, &key);
                        } else {
                            toast.call((
                                crate::toast::ToastKind::Info,
                                "That row is not in the tree any more.".to_string(),
                            ));
                        }
                    }
                }
                door_slot.set(None);
            });
        }
    });

    let status = if loaded_tree.read().is_some() {
        if index.read().is_empty() {
            TreeStatus::Empty
        } else if let Some(message) = tree_error.read().clone() {
            TreeStatus::Failed(message)
        } else {
            TreeStatus::Ready
        }
    } else if let Some(message) = tree_error.read().clone() {
        TreeStatus::Failed(message)
    } else if *tree_loading.read() {
        TreeStatus::Loading
    } else {
        TreeStatus::Empty
    };
    let group_reviewed = loaded_tree.read().as_ref().is_none_or(|t| t.group_reviewed);
    let banner = undo.read().as_ref().and_then(|u| {
        u.seconds_left(now_ms()).map(|seconds_left| UndoBanner {
            message: u.message.clone(),
            seconds_left,
        })
    });

    // One site decision: apply now, or raise the reason prompt.
    let decide_site = use_callback({
        let api = api.clone();
        move |(node, key, st, reason): (String, String, TriageState, String)| {
            let Some(decision) = decision_state_for(st) else {
                return;
            };
            let req = site_decision_request(&key, decision, &reason, &run_id());
            let api = api.clone();
            spawn(async move {
                match api.writes.post_decision(req).await {
                    Ok(done) => {
                        undo.set(Some(TreeUndo::for_site(
                            &done.decision.id,
                            st,
                            epoch.elapsed().as_millis() as u64,
                        )));
                        prompt.set(None);
                        let _ = node;
                        after_write.call(());
                    }
                    Err(e) => {
                        if let Some(TreePrompt::Site { error, .. }) = prompt.write().as_mut() {
                            *error = Some(e.message);
                        } else {
                            toast.call((crate::toast::ToastKind::Error, e.message));
                        }
                    }
                }
            });
        }
    });

    // `C`, `I`, `F` or `T` on a node: read every page of the node, work out the plan, and raise
    // the prompt with its sentence.
    let open_batch = use_callback({
        let api = api.clone();
        move |(node, st): (String, TriageState)| {
            let Some(decision) = decision_state_for(st) else {
                return;
            };
            let idx = index.peek().clone();
            let Some(view) = idx.node(&node).cloned() else {
                return;
            };
            if !batch_allowed(&view) {
                if let Some(why) = batch_blocked_reason(&view) {
                    toast.call((crate::toast::ToastKind::Info, why.to_string()));
                }
                return;
            }
            prompt.set(Some(TreePrompt::Planning {
                node: node.clone(),
                state: st,
                read: 0,
                total: view.counts.sites,
            }));
            let api = api.clone();
            spawn(async move {
                let progress_node = node.clone();
                let ticked = state.peek().ticked.clone();
                let planned = plan_batch(&*api.reads, &node, &decision, &ticked, |read, total| {
                    let still_planning = matches!(
                        prompt.peek().as_ref(),
                        Some(TreePrompt::Planning { node: n, .. }) if *n == progress_node
                    );
                    if still_planning {
                        prompt.set(Some(TreePrompt::Planning {
                            node: progress_node.clone(),
                            state: st,
                            read,
                            total,
                        }));
                    }
                })
                .await;
                // The reviewer may have closed the prompt while the sites were coming.
                if !matches!(prompt.peek().as_ref(), Some(TreePrompt::Planning { node: n, .. }) if *n == node)
                {
                    return;
                }
                match planned {
                    Ok(plan) => prompt.set(Some(TreePrompt::Batch {
                        node,
                        state: st,
                        plan,
                        reason: String::new(),
                        error: None,
                    })),
                    Err(message) => prompt.set(Some(TreePrompt::PlanFailed { node, message })),
                }
            });
        }
    });

    let commit = use_callback({
        let api = api.clone();
        move |()| {
            let current = prompt.peek().clone();
            match current {
                Some(TreePrompt::Batch {
                    node,
                    state: st,
                    plan,
                    reason,
                    ..
                }) => {
                    let api = api.clone();
                    let run = run_id();
                    spawn(async move {
                        match commit_batch(&*api.writes, &plan, &reason, &run).await {
                            Ok(decision_id) => {
                                undo.set(Some(TreeUndo::for_batch(
                                    &decision_id,
                                    plan.keys.len(),
                                    st,
                                    epoch.elapsed().as_millis() as u64,
                                )));
                                state.write().ticked.retain(|k| !plan.keys.contains(k));
                                prompt.set(None);
                                after_write.call(());
                            }
                            Err(message) => prompt.set(Some(TreePrompt::Batch {
                                node,
                                state: st,
                                plan,
                                reason,
                                error: Some(message),
                            })),
                        }
                    });
                }
                Some(TreePrompt::Summary {
                    node, text, reason, ..
                }) => {
                    let idx = index.peek().clone();
                    let Some(view) = idx.node(&node).cloned() else {
                        return;
                    };
                    let api = api.clone();
                    let run = run_id();
                    spawn(async move {
                        match commit_summary(&*api.writes, &view, &text, &reason, &run).await {
                            Ok(()) => {
                                prompt.set(None);
                                toast.call((
                                    crate::toast::ToastKind::Info,
                                    "Summary saved. It replaces the model's summary in the review and the exports."
                                        .to_string(),
                                ));
                                after_write.call(());
                            }
                            Err(message) => prompt.set(Some(TreePrompt::Summary {
                                node,
                                text,
                                reason,
                                error: Some(message),
                            })),
                        }
                    });
                }
                Some(TreePrompt::Site {
                    node,
                    key,
                    state: st,
                    reason,
                    ..
                }) => {
                    if reason.trim().is_empty() {
                        prompt.set(Some(TreePrompt::Site {
                            node,
                            key,
                            state: st,
                            reason,
                            error: Some(format!(
                                "A reason is required to move 1 site to \u{201c}{}\u{201d}.",
                                st.label()
                            )),
                        }));
                    } else {
                        decide_site.call((node, key, st, reason));
                    }
                }
                _ => {}
            }
        }
    });

    let on_key = use_callback(move |(key, mods): (String, KeyMods)| {
        let idx = index.peek().clone();
        let kind = state.peek().cursor_kind();
        // `1` to `5` switch lanes while the tree's own region has focus (no text field does).
        if *focus.peek() == PaneFocus::Cursor {
            if let Some(mut lanes) = lanes {
                let claimed = mods.ctrl || mods.alt || mods.meta;
                if let Some(next) = lane_for_key(&key, claimed, false) {
                    lanes.0.set(next);
                    return;
                }
            }
        }
        let Some(cmd) = tree_command(*focus.peek(), &key, mods, kind) else {
            return;
        };
        match cmd {
            TreeCommand::Move(dir) => state.write().move_cursor(&idx, dir),
            TreeCommand::Open => {
                let requests = state.write().press_right(&idx);
                fetch.call(requests);
            }
            TreeCommand::Close => state.write().press_left(&idx),
            TreeCommand::Enter => {
                let requests = state.write().press_enter(&idx);
                fetch.call(requests);
            }
            TreeCommand::CollapseAll => state.write().collapse_all(&idx),
            TreeCommand::TickOutlier => {
                let cursor = state.peek().cursor.clone();
                if let Some(TreeRef::Site { node, key }) = cursor {
                    let row = state.peek().site(&node, &key).cloned();
                    if let Some(row) = row {
                        state.write().toggle_tick(&key, &row);
                    }
                }
            }
            TreeCommand::BatchPrompt(st) => {
                if let Some(TreeRef::Node(id)) = state.peek().cursor.clone() {
                    open_batch.call((id, st));
                }
            }
            TreeCommand::Site(PaneCommand::Dispose(st)) => {
                if let Some(TreeRef::Site { node, key }) = state.peek().cursor.clone() {
                    decide_site.call((node, key, st, String::new()));
                }
            }
            TreeCommand::Site(PaneCommand::PromptFor(st)) => {
                if let Some(TreeRef::Site { node, key }) = state.peek().cursor.clone() {
                    prompt.set(Some(TreePrompt::Site {
                        node,
                        key,
                        state: st,
                        reason: String::new(),
                        error: None,
                    }));
                }
            }
            TreeCommand::EditSummary => {
                if let Some(TreeRef::Node(id)) = state.peek().cursor.clone() {
                    open_summary(&idx, &id, prompt);
                }
            }
            TreeCommand::Site(_) | TreeCommand::OpenCode | TreeCommand::OpenStack => {}
        }
    });

    let on_toggle = use_callback(move |id: String| {
        let idx = index.peek().clone();
        if state.peek().is_open(&id) {
            state.write().close_node(&idx, &id);
        } else {
            let requests = state.write().open_node(&idx, &id);
            fetch.call(requests);
        }
    });

    let on_undo = use_callback({
        let api = api.clone();
        move |()| {
            let Some(entry) = undo.peek().clone() else {
                return;
            };
            undo.set(None);
            let api = api.clone();
            spawn(async move {
                match api.writes.revoke_decision(&entry.decision_id).await {
                    Ok(_) => after_write.call(()),
                    Err(e) => toast.call((crate::toast::ToastKind::Error, e.message)),
                }
            });
        }
    });

    rsx! {
        TreeScreen {
            index: index.read().clone(),
            state: state.read().clone(),
            status,
            group_reviewed,
            focus: focus(),
            prompt: prompt.read().clone(),
            undo: banner,
            on_key,
            on_focus: move |f: PaneFocus| {
                if let Some(next) = camerata_ui_core::triage::next_pane_focus(f, prompt.peek().is_some()) {
                    focus.set(next);
                }
            },
            on_toggle,
            on_focus_node: move |id: String| state.write().focus_node(&id),
            on_focus_site: move |(node, key): (String, String)| state.write().focus_site(&node, &key),
            on_tick: move |key: String| {
                let found = {
                    let st = state.peek();
                    st.loaded
                        .values()
                        .flat_map(|l| l.rows.iter())
                        .find(|r| r.site_key == key)
                        .cloned()
                };
                if let Some(row) = found {
                    state.write().toggle_tick(&key, &row);
                }
            },
            on_more: move |node: String| {
                let request = state.write().request_more(&node);
                fetch.call(request.into_iter().collect());
            },
            on_retry_node: move |node: String| {
                let idx = index.peek().clone();
                let requests = state.write().open_node(&idx, &node);
                fetch.call(requests);
            },
            on_retry: move |()| reload_tree.call(()),
            on_batch: move |st: TriageState| {
                if let Some(TreeRef::Node(id)) = state.peek().cursor.clone() {
                    open_batch.call((id, st));
                }
            },
            on_decide_site: move |st: TriageState| {
                if let Some(TreeRef::Site { node, key }) = state.peek().cursor.clone() {
                    if st.requires_reason() {
                        prompt.set(Some(TreePrompt::Site { node, key, state: st, reason: String::new(), error: None }));
                    } else {
                        decide_site.call((node, key, st, String::new()));
                    }
                }
            },
            on_edit_summary: move |()| {
                if let Some(TreeRef::Node(id)) = state.peek().cursor.clone() {
                    let idx = index.peek().clone();
                    open_summary(&idx, &id, prompt);
                }
            },
            on_prompt_reason: move |text: String| match prompt.write().as_mut() {
                Some(
                    TreePrompt::Batch { reason, .. }
                    | TreePrompt::Summary { reason, .. }
                    | TreePrompt::Site { reason, .. },
                ) => *reason = text,
                _ => {}
            },
            on_prompt_text: move |text: String| {
                if let Some(TreePrompt::Summary { text: t, .. }) = prompt.write().as_mut() {
                    *t = text;
                }
            },
            on_prompt_commit: move |()| commit.call(()),
            on_prompt_cancel: move |()| {
                prompt.set(None);
                super::viewer::focus_element(TREE_REGION_ID);
            },
            on_undo,
        }
    }
}

fn open_summary(index: &TreeIndex, node: &str, mut prompt: Signal<Option<TreePrompt>>) {
    let Some(view) = index.node(node) else {
        return;
    };
    prompt.set(Some(TreePrompt::Summary {
        node: node.to_string(),
        text: summary_text(view).to_string(),
        reason: String::new(),
        error: None,
    }));
}

/// What a batch decision of `decision` at `node` would write. Reads every page of the node (500
/// at a time), works out the plan and keeps none of the rows: the tree holds only the sites it
/// has opened. `on_page` is told how many sites have been read so far.
pub(crate) async fn plan_batch(
    reads: &dyn camerata_ui::review_source::ReviewSource,
    node: &str,
    decision: &camerata_api_types::decisions::DecisionState,
    ticked: &BTreeSet<String>,
    on_page: impl FnMut(usize, usize),
) -> Result<BatchPlan, String> {
    let rows = read_all_sites(reads, node, on_page)
        .await
        .map_err(|e| e.message)?;
    Ok(batch_plan(node, decision, &rows, ticked))
}

/// Record the batch decision. Returns the id of the node record (the one the undo bar revokes).
/// A blank reason or an empty plan is refused here, before anything is posted, in the words the
/// reviewer reads; a refusal from the server comes back as its own words.
pub(crate) async fn commit_batch(
    writes: &dyn camerata_ui::review_io::ReviewWrites,
    plan: &BatchPlan,
    reason: &str,
    run_id: &str,
) -> Result<String, String> {
    let req = decision_request(plan, reason, run_id).map_err(|e| e.message().to_string())?;
    let done = writes.post_decision(req).await.map_err(|e| e.message)?;
    Ok(done.decision.id)
}

/// Record the edit of a node's summary (spec U4).
pub(crate) async fn commit_summary(
    writes: &dyn camerata_ui::review_io::ReviewWrites,
    node: &TreeNodeView,
    text: &str,
    reason: &str,
    run_id: &str,
) -> Result<(), String> {
    let req =
        summary_edit_request(node, text, reason, run_id).map_err(|e| e.message().to_string())?;
    writes.post_edit(req).await.map_err(|e| e.message)?;
    Ok(())
}

/// The sub-group a site sits in, found by reading the pages of its rule (every site of a rule is
/// under the rule's node, and the sites route reads one node; there is no read by key). `None`
/// when the tree has no such rule or the rule's sites do not include the key.
pub(crate) async fn locate_site(
    source: &dyn camerata_ui::review_source::ReviewSource,
    index: &TreeIndex,
    rule_id: &str,
    key: &str,
) -> Option<String> {
    let rule_node = format!("rule:{rule_id}");
    index.node(&rule_node)?;
    let mut cursor: Option<String> = None;
    loop {
        let page = source
            .sites_page(&rule_node, cursor.as_deref(), SITES_PAGE_SIZE)
            .await
            .ok()?;
        if let Some(row) = page.rows.iter().find(|r| r.site_key == key) {
            return row.group_path.get(2).cloned();
        }
        cursor = Some(page.next_cursor?);
    }
}

#[cfg(test)]
mod tests;
