//! The finding detail pane and the findings table component.

use super::*;

/// The DOM id of the pane's reason field (the on-demand prompt `I` and `F` raise).
pub(in crate::cockpit) const PANE_REASON_ID: &str = "camerata-pane-reason";

/// The DOM id of the Queue's own toolbar reason field.
pub(in crate::cockpit) const QUEUE_REASON_ID: &str = "camerata-queue-reason";

/// The DOM id of the docked detail pane's focusable region.
///
/// Named once because three things depend on it agreeing: the `aside`'s own `id`, the
/// `focus_detail_pane` helper, and the CSS focus ring that makes "the pane holds focus"
/// visible. The Q5 scoping is stated in terms of that condition, so the curator has to be
/// able to both see it and reach it.
pub(in crate::cockpit) const DETAIL_PANE_ID: &str = "camerata-finding-pane";

/// Move real keyboard focus into the detail pane.
///
/// Deliberately a DOM focus call rather than a signal write: `PaneFocus` is maintained by the
/// pane's own `focusin`/`focusout`, so it always reflects where focus actually is. Writing the
/// signal directly would let the pane report its keys armed while the keydown handler — which
/// lives on the pane element — received nothing.
pub(super) fn focus_detail_pane() {
    super::viewer::focus_element(DETAIL_PANE_ID);
}

/// One ABSORBED evidence site as the detail pane lists it (design §6).
#[derive(Clone, PartialEq)]
pub(in crate::cockpit) struct PaneSiteRow {
    /// `path:line`, or `repo path:line` when the absorbed site lives in a different repo
    /// from the primary (a cross-file merge can span two repos in one scan).
    pub location: String,
    /// The rule id this site was originally flagged under, before being absorbed.
    pub rule_id: String,
    /// The line this site cited.
    pub snippet: String,
    /// A pure CONSEQUENCE of the primary's root cause rather than another place needing the
    /// same fix. Rendered subordinated, under "also affects".
    pub consequence: bool,
}

/// THE FOCUSED FINDING, flattened into exactly what the pane renders.
///
/// Flat, owned, `PartialEq` data rather than the `FindingView` itself — the same shape (and
/// the same reason) as `DeviationBulkRow`: the pane is then a props-only component that
/// renders, and is therefore testable, without a chorale table, a disposition map, a network
/// resource or a VirtualDom-minted handle. SSR cannot press a key; what it CAN do is read
/// what the pane says about the row it is describing and about whether the keys are armed,
/// and those are the two things a keystroke test would otherwise have to take on trust.
#[derive(Clone, PartialEq)]
pub(in crate::cockpit) struct DetailPaneRow {
    /// The finding's `finding_key`: the same key the Queue's `RowCursor` and the code viewer
    /// use, written to the DOM so a test can read which finding each surface is on.
    pub finding_key: String,
    pub rule_id: String,
    /// From [`kind_chip_parts`] — the same chip the Queue's Kind column renders.
    pub kind_class: String,
    pub kind_label: String,
    pub kind_tip: String,
    /// A deviation's structured claim (`DeviationHeadline::sentence`), verbatim — the same
    /// words its group header carries. `None` for every other kind.
    pub claim: Option<String>,
    /// The CALIBRATED severity, authored word, read straight off `FindingView::severity`.
    /// Never the rank-prefixed Severity CELL value, which exists only so chorale's
    /// lexicographic column sort orders the ladder correctly.
    pub severity: String,
    pub status: String,
    pub repo: String,
    /// `path:line` of the primary site.
    pub location: String,
    /// Every absorbed site (design §6). Empty for an un-merged row.
    pub sites: Vec<PaneSiteRow>,
    /// `1 + sites.len()`, from `camerata_ui_core::triage::site_count`.
    pub site_count: usize,
    pub snippet: String,
    /// THE CITED STANDARD: the rule's own authored directive, as the rules corpus states it.
    pub standard: Option<String>,
    /// The finding's explanation, with the calibration `[needs review: …]` flag split off.
    pub explanation: String,
    pub needs_review: Option<String>,
    /// The review flags the engine left on the row (M4): a wording check failed and the best text
    /// shipped. Shown as chips so a reviewer sees what to read twice.
    pub review_flags: Vec<String>,
    /// Structured calibration confidence (`high` / `needs-review`).
    pub confidence: Option<String>,
    /// Structured remediation-effort estimate (`low` / `medium` / `high`).
    pub effort: Option<String>,
    /// The recommended fix, fetched by the call site from the SAME server function the
    /// report's "Fix:" line uses — never re-derived here.
    pub fix: Option<String>,
    /// Other rule ids violated at this same location.
    pub also_matches: Vec<String>,
    /// 1-based position of this row in the table, and the table's row count: "3 of 212".
    pub position: usize,
    pub total: usize,
    /// Whether the row is in a group the stacked view can show (a deviation or a defect), so the
    /// pane offers its "Stack" door only where `B` would do something.
    pub stackable: bool,
}

impl DetailPaneRow {
    /// Flatten the focused finding into the pane's props. The ONLY place a `DetailPaneRow` is
    /// built, so every field goes through one derivation: the kind chip is
    /// [`kind_chip_parts`]'s, the claim is `DeviationHeadline::sentence`'s, the site count is
    /// core's `site_count`, and the severity is the authored word rather than the sort value.
    ///
    /// `index` is 0-based (the cursor's); `position` is the 1-based number shown.
    pub(in crate::cockpit) fn from_finding(
        f: &FindingView,
        directive: Option<&str>,
        fix: Option<String>,
        index: usize,
        total: usize,
    ) -> Self {
        let (kind_class, kind_label, kind_tip) = kind_chip_parts(kind_tag(f));
        let (body, needs_review) = split_needs_review(&f.detail);
        Self {
            finding_key: finding_key(f),
            rule_id: f.rule_id.clone(),
            kind_class: kind_class.to_string(),
            kind_label: kind_label.to_string(),
            kind_tip: kind_tip.to_string(),
            claim: deviation_headline(f).map(|h| h.sentence()),
            severity: f.severity.clone(),
            status: f.status.clone(),
            repo: f.repo.clone(),
            location: format!("{}:{}", f.path, f.line),
            sites: f.also_locations.iter().map(|s| pane_site(f, s)).collect(),
            site_count: site_count(f),
            snippet: f.snippet.clone(),
            standard: directive
                .map(str::to_string)
                .filter(|d| !d.trim().is_empty()),
            explanation: body,
            needs_review,
            review_flags: camerata_ui_core::triage::review_flags_of(f),
            confidence: f.confidence.clone(),
            effort: f.effort.clone(),
            fix: fix.filter(|s| !s.trim().is_empty()),
            also_matches: f.also_matches.clone(),
            position: index + 1,
            total,
            stackable: is_stackable(f),
        }
    }
}

/// One absorbed site, with its repo named only when it differs from the primary's — a
/// same-repo merge does not need the owner/repo prefix on every line, and a cross-repo one
/// would be actively misleading without it.
pub(super) fn pane_site(primary: &FindingView, s: &MergedSiteView) -> PaneSiteRow {
    let location = if s.repo.is_empty() || s.repo == primary.repo {
        format!("{}:{}", s.path, s.line)
    } else {
        format!("{} {}:{}", s.repo, s.path, s.line)
    };
    PaneSiteRow {
        location,
        rule_id: s.rule_id.clone(),
        snippet: s.snippet.clone(),
        consequence: s.consequence,
    }
}

/// The lines of a code-context window: line number, text, and the violation line marked. THE
/// rendering of a window, shared by the detail pane and the stacked view's cards so the two
/// cannot draw the same lines two ways.
pub(in crate::cockpit) fn context_rows(c: &PaneContext) -> Element {
    rsx! {
        for (n, text, hit) in c.rows() {
            div {
                key: "{n}",
                class: if hit { "finding-ctx-line finding-ctx-line-hit" } else { "finding-ctx-line" },
                span { class: "finding-ctx-gutter", "{n}" }
                span { class: "finding-ctx-code", "{text}" }
            }
        }
    }
}

/// The DOM id of a Queue toolbar button that moves the selected rows to `target`. One per target
/// per triage table, so a table never has two with the same id.
pub(in crate::cockpit) fn toolbar_button_id(target: TriageState) -> String {
    format!(
        "camerata-toolbar-to-{}",
        target.label().to_ascii_lowercase().replace(' ', "-")
    )
}

/// The DOM id of the detail pane's Undo button.
pub(in crate::cockpit) const PANE_UNDO_ID: &str = "camerata-pane-undo";

/// The undo banner for an armed undo at time `now` (ms on the table's monotonic clock), or `None`
/// when there is none or its window has shut. Shared by the pane and the code viewer.
pub(in crate::cockpit) fn undo_banner_of(
    entry: &Option<UndoEntry>,
    now: u64,
) -> Option<UndoBanner> {
    entry.as_ref().and_then(|e| {
        e.is_live(now).then(|| UndoBanner {
            message: e.message().to_string(),
            // Ceiling, so a window with 1ms left still reads "1s" rather than "0s" — a
            // countdown that shows zero while the button still works is a small lie, and
            // this one is about how long the curator has to act.
            seconds_left: e.remaining_ms(now).div_ceil(1_000),
        })
    })
}

/// The live five-second undo, as the pane renders it.
#[derive(Clone, PartialEq)]
pub(in crate::cockpit) struct UndoBanner {
    /// What it will reverse — `UndoEntry::message`.
    pub message: String,
    /// Whole seconds left in the window, for the countdown.
    pub seconds_left: u64,
}

/// THE DOCKED DETAIL PANE (design §2; the owner's Q5 ruling, design §13).
///
/// > At 400+ rows the modal's open/decide/close cycle is the single biggest per-decision time
/// > cost in the current UI. A docked pane plus arrow-key row focus turns review into
/// > cursor-down, read, one keystroke.
///
/// ## chorale does not have this, and the design document says it does
///
/// Design §13's 2026-10-12 dependency check recorded `detail_renderer` /
/// `set_detail_column_enabled` / `detail_column_enabled` as "the docked pane" in chorale
/// 0.2.3. Reading the installed source, they are not: `detail_renderer` is an EXPANDABLE
/// INLINE ROW DETAIL, rendered by `detail_panel_tr` (chorale-dioxus 0.2.3
/// `components.rs:1998`) as `<tr class="chorale-detail-panel"><td colspan=N>…</td></tr>`
/// immediately beneath its parent row, with a chevron column to expand it. That is a
/// different thing, and it would defeat the gesture this pane exists for: an inline panel
/// pushes every row below it down the page, so cursor-down-read-decide would reflow the table
/// under the cursor on every single move. This is the same class of error step 2 found in the
/// same dependency check's group-header entry. Per the standing ruling not to context-switch
/// into chorale, the pane is built here.
///
/// What chorale DOES provide, and what the call site uses, is `visible_grouped_view` — the
/// post-filter, post-sort, post-COLLAPSE row order. The cursor walks that, which is why focus
/// survives a re-sort and a collapse by construction rather than by special-casing.
///
/// ## Props-only, deliberately
///
/// Every judgment is made in `camerata_ui_core::triage` and arrives here as data: whether the
/// keys are armed ([`pane_keys_armed`]), which disposition a key means ([`pane_command`]),
/// what the undo will reverse. This component chooses words and pixels, nothing else — the
/// same discipline `ProcessGate` and `DeviationBulkBar` are built on, and the reason SSR can
/// verify the scoping at all.
#[component]
pub(in crate::cockpit) fn FindingDetailPane(
    /// The focused row, or `None` when nothing is focused (an empty triage table, or every
    /// group collapsed).
    row: Option<DetailPaneRow>,
    /// Where keyboard focus is. THE scoping input — see [`pane_command`].
    focus: PaneFocus,
    /// The disposition whose reason is being collected, when the on-demand prompt is open.
    prompt: Option<TriageState>,
    /// The reason typed so far.
    prompt_reason: String,
    /// The live undo, when one is armed.
    undo: Option<UndoBanner>,
    /// Every keydown on the pane, as `(key, modifiers)`. The call site runs it through
    /// [`pane_command`]; this component never decides what a key means.
    on_key: Callback<(String, KeyMods)>,
    /// Focus moved into or out of the pane's cursor / its reason prompt.
    on_focus: Callback<PaneFocus>,
    on_prompt_reason: Callback<String>,
    on_prompt_commit: Callback<()>,
    on_prompt_cancel: Callback<()>,
    on_undo: Callback<()>,
    /// A disposition chosen with the MOUSE. The keys are an accelerator, not the only door.
    on_dispose: Callback<TriageState>,
    /// Open the finding-detail MODAL on this row. The pane replaces the modal as the Queue's
    /// row-click surface (design §9.6), but not as the deep dive: the modal carries the
    /// on-demand enclosing-code-block fetch, which is far too expensive to re-run on every
    /// cursor-down and so stays one deliberate click away.
    on_open_detail: Callback<()>,
    /// Open the RULE HUB on the focused row's rule (design §8).
    ///
    /// This is how a QUEUE ROW reaches the hub. The rule id in the row's own "Finding type"
    /// cell cannot be the door: a chorale cell renderer is an `Arc<dyn Fn(..) + Send + Sync>`
    /// and a Dioxus `Callback` is neither `Send` nor `Sync` — the same constraint that put the
    /// deviation bulk actions in a bar instead of inside the group header `<tr>`. The pane IS
    /// the Queue row's detail surface and it opens on the very click that focuses the row, so
    /// the hub sits one further click from any row, at exactly the place the curator is
    /// already reading.
    ///
    /// Defaulted so the pane's existing render tests keep compiling and asserting unchanged.
    #[props(default)]
    on_open_hub: Callback<()>,
    /// Open the CODE VIEWER on the focused finding: the `path:line` tag and the "Code" button
    /// are two of its doors (the `O` key and the modal's button are the others).
    #[props(default)]
    on_open_code: Callback<()>,
    /// Open the STACKED GROUP VIEW on the focused finding's rule group (`B`): every row of the
    /// group as a card, to decide the same fix in many places from one screen.
    #[props(default)]
    on_open_stack: Callback<()>,
    /// The stored line plus three lines either side, once the batch context endpoint has
    /// answered for this finding (work order B.3). `None` is the pane exactly as it was before
    /// this prop existed: the stored snippet alone. Anything but a good answer is `None`.
    #[props(default)]
    context: Option<PaneContext>,
    /// What this draft already decided about the focused finding's shape (work order B.5), as the
    /// chip text: `Previously on this shape: Ignored, 'test fixture, not shipped' (3 rows)`.
    /// `None` shows nothing. It is the reviewer's own text from this draft, drawn as a text node.
    #[props(default)]
    previously: Option<String>,
) -> Element {
    let armed = pane_keys_armed(focus);
    let pane_key = row
        .as_ref()
        .map(|r| super::viewer::dom_key(&r.finding_key))
        .unwrap_or_default();
    rsx! {
        aside {
            class: "finding-pane",
            id: "{DETAIL_PANE_ID}",
            "data-finding-key": "{pane_key}",
            // The pane is a focusable REGION: tabbing into it arms the keys, tabbing out
            // disarms them. That is the Q5 scoping made operable rather than merely checked —
            // there is a visible, reachable thing that "the detail pane holds focus" means.
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
                // Swallow only the presses the pane actually claims, so Tab still tabs and
                // Cmd-C still copies the snippet the curator is reading. The claim test is
                // `pane_command` itself — the same call the handler makes — so a key can
                // never be eaten by a pane that then does nothing with it.
                if pane_command(focus, &key, mods).is_some() {
                    e.prevent_default();
                    e.stop_propagation();
                }
                on_key.call((key, mods));
            },

            if let Some(r) = row.clone() {
                div { class: "pane-head",
                    // The rule id is the hub's door from a Queue row (see `on_open_hub`).
                    button {
                        class: "rule-hub-link pane-rule",
                        title: "Everything this scan knows about {r.rule_id}: its kind outcome, its counts, the convention behind it, and where it lives.",
                        onclick: move |_| on_open_hub.call(()),
                        "{r.rule_id}"
                    }
                    span { class: "{r.kind_class}", title: "{r.kind_tip}", "{r.kind_label}" }
                    span { class: "pane-pos", "{r.position} of {r.total}" }
                    button {
                        class: "btn-edit-sm pane-code",
                        title: "Open the file in the code viewer (O).",
                        onclick: move |_| on_open_code.call(()),
                        "Code"
                    }
                    if r.stackable {
                        button {
                            class: "btn-edit-sm pane-stack",
                            title: "Show every row of this rule group as a card with its code, to decide the same fix in many places at once (B).",
                            onclick: move |_| on_open_stack.call(()),
                            "Stack"
                        }
                    }
                    button {
                        class: "btn-edit-sm pane-full-detail",
                        title: "Open the full finding modal: the enclosing code block around this line, fetched on demand.",
                        onclick: move |_| on_open_detail.call(()),
                        "Full detail"
                    }
                }
                div { class: "pane-meta",
                    // The AUTHORED severity. Not the Severity column's cell value, which
                    // carries a calibrated rank prefix purely so chorale's lexicographic
                    // column sort orders the ladder; the badge, the row highlight, the CSV
                    // and the report all read the clean word, and so does this.
                    span { class: "rule-modal-tag", "severity \u{b7} {r.severity}" }
                    button {
                        class: "rule-modal-tag pane-code-link",
                        title: "Open this file in the code viewer (O).",
                        onclick: move |_| on_open_code.call(()),
                        "{r.location}"
                    }
                    if !r.repo.is_empty() {
                        span { class: "rule-modal-tag", "{r.repo}" }
                    }
                    span { class: "rule-modal-tag", "{r.status}" }
                    if let Some(c) = r.confidence.clone() {
                        span { class: "rule-modal-tag", "confidence \u{b7} {c}" }
                    }
                }
                // A deviation's structured claim, verbatim from the same builder the group
                // header uses. Nothing here is parsed out of prose.
                if let Some(claim) = r.claim.clone() {
                    p { class: "pane-claim", "{claim}" }
                }
                if let Some(d) = r.standard.clone() {
                    p { class: "rule-modal-label", "Cited standard" }
                    p { class: "rule-modal-detail", "{d}" }
                }
                // DECISION MEMORY (B.5). Information only: nothing here decides anything, and the
                // chip is absent for a shape the draft has only confirmed or saved as tech debt.
                if let Some(prev) = previously.clone() {
                    p {
                        class: "pane-previously",
                        "data-prior-decision": "true",
                        "{prev}"
                    }
                }
                p { class: "rule-modal-label", "Evidence" }
                pre { class: "pane-snippet", "{r.snippet}" }
                // The surrounding code fills in beneath the snippet once it arrives.
                if let Some(c) = context.clone() {
                    pre { class: "finding-ctx-pre pane-ctx", "data-pane-context": "ok",
                        {context_rows(&c)}
                    }
                }
                if !r.explanation.trim().is_empty() {
                    p { class: "rule-modal-detail",
                        "{r.explanation}"
                        if let Some(nr) = r.needs_review.clone() {
                            " "
                            b { class: "nr-inline",
                                if nr.is_empty() { "[needs review]" } else { "[needs review: {nr}]" }
                            }
                        }
                    }
                }
                if !r.review_flags.is_empty() {
                    p { class: "pane-review-flags", "data-review-flags": "true",
                        for flag in r.review_flags.iter() {
                            span { class: "review-flag-chip", key: "{flag}", "{flag}" }
                        }
                    }
                }
                // ABSORBED SITES (design §6) — the full-disclosure surface. Rendered only
                // when there is more than one site: "Sites (1)" on an un-merged row would be
                // a section that exists to say nothing.
                if !r.sites.is_empty() {
                    p { class: "rule-modal-label", "Sites ({r.site_count})" }
                    ul { class: "pane-sites",
                        li { class: "pane-site pane-site-primary",
                            span { class: "pane-site-loc", "{r.location}" }
                            span { class: "pane-site-rule", "{r.rule_id}" }
                        }
                        for (i, s) in r.sites.iter().enumerate() {
                            li {
                                key: "{s.location}-{i}",
                                class: if s.consequence { "pane-site pane-site-consequence" } else { "pane-site" },
                                if s.consequence {
                                    span { class: "pane-site-sub", "also affects" }
                                }
                                span { class: "pane-site-loc", "{s.location}" }
                                span { class: "pane-site-rule", "{s.rule_id}" }
                                if !s.snippet.is_empty() {
                                    span { class: "pane-site-snippet", "{s.snippet}" }
                                }
                            }
                        }
                    }
                }
                if !r.also_matches.is_empty() {
                    p { class: "rule-modal-label", "Also violates here" }
                    div { class: "pane-meta",
                        for rid in r.also_matches.iter() {
                            span { key: "{rid}", class: "rule-modal-tag", "{rid}" }
                        }
                    }
                }
                // THE FIX, with its structured effort estimate beside it. The text is the
                // SAME server function's output the report's "Fix:" line renders — fetched
                // by the call site, never re-derived here.
                if r.fix.is_some() || r.effort.is_some() {
                    div { class: "pane-fix-head",
                        p { class: "rule-modal-label", "Recommended fix" }
                        if let Some(e) = r.effort.clone() {
                            span {
                                class: "rule-modal-tag pane-effort",
                                title: "The calibration pass's remediation-effort estimate for this finding.",
                                "effort \u{b7} {e}"
                            }
                        }
                    }
                    if let Some(fix) = r.fix.clone() {
                        p { class: "rule-modal-detail", "{fix}" }
                    }
                }
            } else {
                p { class: "pane-empty",
                    "No finding focused. Click a row, or press Tab into this panel and use the \
                     arrow keys."
                }
            }

            // THE UNDO (Q5's five-second toast). Docked in the pane rather than in the
            // app-wide toast stack because that stack's cards are purely informational and
            // auto-dismiss on their own timer; this one carries an action whose window must
            // be the model's, and it must sit where the curator's eyes already are.
            if let Some(u) = undo.clone() {
                div { class: "pane-undo",
                    span { class: "pane-undo-msg", "{u.message}" }
                    button {
                        class: "btn-edit-sm pane-undo-btn",
                        id: PANE_UNDO_ID,
                        onclick: move |_| on_undo.call(()),
                        "Undo"
                    }
                    span { class: "pane-undo-left", "{u.seconds_left}s" }
                }
            }

            if row.is_some() {
                // THE ON-DEMAND REASON PROMPT (design §3). Replaces the two permanently
                // visible textareas for the keyboard path: it appears when `I` or `F` is
                // pressed, pre-focused, Enter commits, Esc cancels — and while it holds
                // focus the disposition keys are OFF, which is the composition the Q5
                // scoping clause asks for rather than a second mechanism bolted beside it.
                if let Some(state) = prompt {
                    div { class: "pane-reason-prompt",
                        label { class: "rule-modal-label",
                            "Reason for \u{201c}{state.label()}\u{201d} (required)"
                        }
                        textarea {
                            id: "{PANE_REASON_ID}",
                            class: "addressee-input pane-reason-input",
                            rows: "2",
                            autofocus: true,
                            placeholder: "why (the export reads this)",
                            value: "{prompt_reason}",
                            // Pre-focused, as design §3 specifies. `autofocus` alone is not
                            // reliable in a webview for an element appearing on a re-render,
                            // and getting this wrong is not cosmetic: the pane has already
                            // disarmed its keys (opening the prompt is the one place it does
                            // so EAGERLY, because disarming early is the safe direction and
                            // arming early is not), so a prompt nobody is focused in would
                            // be a dead surface with dead keys beside it.
                            onmounted: move |e| {
                                spawn(async move {
                                    let _ = e.set_focus(true).await;
                                });
                            },
                            onfocusin: move |_| on_focus.call(PaneFocus::ReasonPrompt),
                            oninput: move |e| on_prompt_reason.call(e.value()),
                            onkeydown: move |e: KeyboardEvent| {
                                // Belt AND braces. The pure gate above already refuses every
                                // key while `focus == ReasonPrompt`; stopping propagation
                                // here means the pane's handler never even sees a keystroke
                                // aimed at this textarea, so a focus-tracking bug cannot
                                // turn the first `c` of "cannot reproduce" into a Confirm.
                                e.stop_propagation();
                                match e.key() {
                                    Key::Escape => { on_prompt_cancel.call(()); }
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
                div { class: "pane-actions",
                    span {
                        class: if armed { "pane-keys armed" } else { "pane-keys off" },
                        // The indicator is `pane_keys_armed`, which is DEFINED as "would C
                        // produce a command". A pane that said "armed" while the handler
                        // refused would be the ships-inert shape in miniature.
                        if armed {
                            "Keys: C confirm \u{b7} I ignore \u{b7} T tech debt \u{b7} F false positive \u{b7} \u{2191}\u{2193} move \u{b7} O code \u{b7} B stack"
                        } else if prompt.is_some() {
                            "Keys are off while you type a reason. Enter commits, Esc cancels."
                        } else {
                            "Keys are off. Click this panel (or Tab into it) to arm C / I / T / F."
                        }
                    }
                    for state in TriageState::all().into_iter().filter(|s| s.is_decided()) {
                        button {
                            key: "{state.label()}",
                            class: "btn-edit-sm pane-dispose",
                            title: "Press {state_hotkey(state).unwrap_or(\"\").to_uppercase()} while this panel has focus.",
                            onclick: move |_| on_dispose.call(state),
                            "{state.label()}"
                        }
                    }
                }
            }
        }
    }
}

/// The findings table with TRIAGE: sort by repo/severity/type, filter, select rows
/// and Ignore / Resolve / Accept-as-tech-debt (open a ticket) them. Virtualized by
/// chorale, so a large audit doesn't choke the UI.
#[component]
pub(in crate::cockpit) fn FindingsTable(
    findings: Vec<FindingView>,
    repos: Vec<String>,
    descriptions: std::collections::HashMap<String, String>,
    // Which triage table this is (issue #26). Findings not in this state are filtered out;
    // the component is keyed on it by the parent, so a switch remounts with that table's set.
    #[props(default = TriageState::Unresolved)] triage_view: TriageState,
    // The lifted finding -> disposition map. Move actions write here; the row is then dropped
    // from this table (remove_rows) and reappears under its new table on the next switch.
    #[props(default)] dispositions: Signal<std::collections::HashMap<String, Disposition>>,
    /// How the Queue is cut (work order B.4): lifted into the results view so it is saved in the
    /// draft and survives this table remounting on a triage-view switch. The select above the
    /// table writes it; an effect below re-orders and re-groups the table to match.
    #[props(default)]
    order_preset: Signal<OrderPreset>,
    /// Open the RULE HUB (design §8). Raised from the two surfaces inside this table that
    /// carry a rule id and CAN carry a click: the docked detail pane (which is how a Queue row
    /// reaches it) and the deviation bulk bar (which is how a deviation group header does).
    #[props(default)]
    on_open_hub: Callback<RuleHubRequest>,
    /// THE HUB'S QUEUE JUMP, arriving as state rather than as a call: "show me this rule in
    /// the Queue" is a column filter, exactly as design §8 describes the File join — "a
    /// filter, not a hyperlink graph". The parent writes a rule id here; the effect below
    /// applies it to chorale's own `type` column filter, so the jump leaves the curator in a
    /// normal, visibly-filtered table he can clear with the controls already on screen.
    #[props(default)]
    queue_rule_filter: Signal<Option<String>>,
    /// The commit each repo was inspected at (`audited_refs[repo].sha`), for the code viewer.
    /// A repo with no entry is read at whatever commit the server recorded for the project's
    /// latest inspection.
    #[props(default)]
    audited_shas: std::collections::HashMap<String, String>,
    /// Repos whose scan and audit recorded different commits: the viewer says so.
    #[props(default)]
    sha_conflicts: std::collections::HashSet<String>,
    /// The origin remote each repo was scanned from (`audited_refs[repo].origin_url`), so the
    /// viewer's permalink can add a GitHub URL when the origin is GitHub.
    #[props(default)]
    audited_origins: std::collections::HashMap<String, String>,
    /// The project the findings belong to, passed to the code viewer's requests.
    #[props(default)]
    project_id: String,
    /// The tally over every Queue row (`TriageModel::counts`), taken once by the results view
    /// (which already needs it for the sub-tabs and the Process gate) and drawn as the progress
    /// strip above the table. `None` draws no strip.
    #[props(default)]
    counts: Option<TriageCounts>,
    /// The review timer (INTERNAL, step 7a). It observes the decisions `apply_rows` writes and the
    /// group the cursor is in, and the strip shows its state, Pause / Resume and time left. `None`
    /// (a table mounted without one) draws the plain counts strip and times nothing.
    #[props(default)]
    review: Option<super::review::ReviewHandle>,
    /// Open the internal review-time stats view (the strip's "Review time" button).
    #[props(default)]
    on_open_stats: Callback<()>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    // Whether the progress strip (and so the timer's two controls) is drawn above the table.
    let has_counts = counts.is_some();
    // Every queue row, before this table's triage-state filter: the viewer's gutter marks the
    // decided findings of a file too, so it needs the whole set.
    //
    // One shared copy for the table's lifetime (its data is frozen, like its rows): the viewer's
    // and the stack's links, decision memory and the toolbar's pre-fill all read it.
    let all_rows: std::sync::Arc<Vec<FindingView>> = use_hook({
        let all = findings.clone();
        move || std::sync::Arc::new(all)
    });
    // Filter to THIS table's triage state + apply the default triage order (enforced/new before
    // suppressed/debt, then critical → high → medium → low). Both the filter and the sort are the
    // pure `TriageModel::visible` derivation now (RUST-HEADLESS-CORE-1): a flat 200-row dump is
    // paralysis; this floats the exploitable-bug criticals to the very top so a hardcoded secret
    // can never sit below "no mappers crate." Absent from the map = Unresolved.
    let findings: Vec<FindingView> = {
        let model = TriageModel {
            dispositions: dispositions.peek().clone(),
            triage_view,
        };
        model.visible(&findings)
    };
    // Distinct repos for the repo multi-select filter. (Finding type is a Text/contains
    // filter now, so it needs no precomputed option list.)
    let mut filter_repos: Vec<String> = findings.iter().map(|f| f.repo.clone()).collect();
    filter_repos.sort();
    filter_repos.dedup();
    // Mint row ids ONCE per mount (see ProposedRulesTable): `RowId::new()` in the render
    // body would re-id every render and desync the Table from id_map/selection. This
    // component remounts on each new audit (it's gated behind `audited.is_some()` and the
    // re-audit clears it first), so freezing rows per mount tracks fresh findings while
    // keeping ids stable within a mount.
    let initial_preset = *order_preset.peek();
    let rows: Vec<(RowId, FindingView)> = use_hook({
        let findings = findings.clone();
        move || {
            // The rows are mounted in the preset's order: chorale keeps rows in the order it is
            // given and builds a group at the first row that belongs to it.
            order_findings(initial_preset, &findings)
                .into_iter()
                .map(|f| (RowId::new(), f))
                .collect()
        }
    });
    let id_map: std::collections::HashMap<RowId, FindingView> =
        rows.iter().map(|(r, f)| (*r, f.clone())).collect();
    let id_map_click = id_map.clone();
    // Row click opens the finding-detail modal (hosted by ScanResults, OUTSIDE this table's
    // subtree — same reason as the rule modal). Shows the violated rule's full directive +
    // the complete, untruncated explanation that the row cell clips.
    let mut detail_finding = use_context::<Signal<Option<FindingView>>>();
    // ── THE CODE VIEWER's open flag (viewer.rs). The rest of its state lives in `ViewerRoot`.
    let mut viewer_open = use_signal(|| false);
    let viewer_request = try_use_context::<super::viewer::ViewerRequest>();
    let in_techdebt = triage_view == TriageState::TechDebt;
    // The column set is rebuilt for the order indicator below, so the repo list is cloned
    // before `use_table` takes ownership of its copy.
    let indicator_repos = filter_repos.clone();
    let handle =
        use_table(move || finding_table_state(rows.clone(), filter_repos.clone(), in_techdebt));
    // ── THE HUB'S QUEUE JUMP, applied to chorale's own column filter ────────────────────
    //
    // `applied` is PEEKED, never read, so this effect subscribes to exactly one signal
    // (`queue_rule_filter`) and cannot re-trigger itself by recording what it just did. The
    // alternative — clearing the source signal after applying it — would have the effect
    // write the thing it reads, which is how a render loop starts.
    //
    // Re-jumping to a rule the table is ALREADY filtered to is correctly a no-op: the curator
    // is looking at exactly what the jump promises.
    let mut applied_rule_filter = use_signal(|| Option::<String>::None);
    use_effect(move || {
        let want = queue_rule_filter();
        if want != *applied_rule_filter.peek() {
            applied_rule_filter.set(want.clone());
            handle.set_filter(ColumnId("type"), want.map(FilterValue::Text));
        }
    });

    // ── The ORDER BAR: which order is on screen, and one click back to the default ───────
    //
    // Pass 2 step 2 (design §5; Zach 2026-10-12: "Keep sort with [unspecified] first, sure, as
    // long as the table can be manually sorted by me as the user"). chorale already owns the
    // manual sort itself — every column is `.sortable()` and `sort_enabled` makes the headers
    // clickable with its own ↑/↓ arrow. What it does not own is the DEFAULT: `state.sort` being
    // empty is chorale's "unsorted", but it is Camerata's curated order, and nothing on screen
    // said so or offered a way back to it from an arbitrary sort state.
    //
    // A memo rather than reading the table signal inline: this component renders the chorale
    // table, and subscribing it directly to `TableState` would re-render the whole subtree on
    // every scroll/selection write. The memo narrows the state to the sort vector, so the bar
    // below re-renders only when the sort actually changes.
    let sort_signal = handle.signal();
    let active_sort = use_memo(move || sort_signal.read().sort.clone());
    // The column set the bar resolves sorted-column headers against. Built once per mount and
    // shared by `Rc`, so the bar's props stay cheap to compare (`ColumnDef` holds `Arc`'d
    // closures and is not `PartialEq`).
    let order_columns =
        use_hook(|| std::rc::Rc::new(finding_columns(indicator_repos.clone(), in_techdebt)));
    // Subscribe to the disposition map so the bucket flag column re-renders when the architect
    // marks resolve-later/now (the renderer below captures this snapshot).
    let bucket_snapshot = dispositions.read().clone();
    // Two-level grouping: by RULE, then by FILE within each rule. chorale groups by an
    // ordered key list (it recurses through the Vec, one depth per key), so a rule violated
    // 4× across one file renders as "RULE (4)" → "handlers.rs (4)" → the 4 individual lines.
    // Counts come free on every header. This is a PRESENTATION view of the flat finding
    // list; the CSV export stays flat + lossless (one row per finding), unchanged.
    // Grouping + collapse-by-default (rule → file → lines, all collapsed on mount) is
    // owned by `CamerataTable` via `group_by` below; it applies the load-all-then-collapse
    // order that works around chorale's paginated `collapse_all_groups`.
    // ── THE ON-DEMAND REASON PROMPT (design §3, §9.2) ───────────────────────────────────
    //
    // REMOVED: `ignore_reason` and `fp_reason`, the two `use_signal(String)`s behind the two
    // permanently visible textareas. A durable ignore and a false positive each still REQUIRE
    // a reason — the invariant is unchanged and is still enforced inside `TriageModel` — but
    // the box is raised by the gesture instead of standing open beside the table.
    //
    // `toolbar_prompt` is `None` whenever no reason is being collected, which is the state
    // the Queue is in essentially all the time.
    let mut toolbar_prompt = use_signal(|| Option::<ToolbarReasonPrompt>::None);
    let mut toolbar_reason = use_signal(String::new);
    // The (sorted) rows for CSV export.
    let csv_rows = findings.clone();

    // ── RULE-LEVEL BULK over deviation groups (design §2/§4) ────────────────
    //
    // The groups are derived from THIS table's visible row set, so a group's count is exactly
    // the number of rows its button will move. `bulk_groups` keeps the full rows (for the
    // disposition write); `bulk_rows` is the flat props the bar renders.
    //
    // Unresolved only. Both of the bar's actions are decisions ABOUT an undecided group: a
    // deviation already moved to Confirmed, Ignored or Tech debt has had its rule-level
    // decision made. (Before §9.2 there was a second reason — `False positive all N` read the
    // reason textarea that existed only in the Unresolved toolbar. That textarea is gone and
    // the button raises its own prompt, so the Unresolved-only scoping now rests entirely on
    // the first reason, which was always the real one.) Rendering a dead bar in the other
    // four tables would be the 403-button shape the standing rule forbids.
    let bulk_groups = std::rc::Rc::new(if triage_view == TriageState::Unresolved {
        deviation_groups(&findings)
    } else {
        Vec::new()
    });
    let bulk_rows: Vec<DeviationBulkRow> = bulk_groups
        .iter()
        .map(DeviationBulkRow::from_group)
        .collect();
    // `finding_key` -> `RowId`, so a bulk action can drop exactly its own group's rows from
    // the table. Built from the same `id_map` the per-row buttons use, so the two paths
    // resolve rows identically.
    let key_to_row_id: std::collections::HashMap<String, RowId> = id_map
        .iter()
        .map(|(rid, f)| (finding_key(f), *rid))
        .collect();

    // ── THE DOCKED DETAIL PANE (design §2; the owner's Q5 ruling, §13) ──────────────────
    //
    // Everything below is wiring: the pure cursor, scoped key handler and undo all live in
    // `camerata_ui_core::triage` and are unit-tested there, and `FindingDetailPane` is
    // props-only and render-tested. This block owns only the signals and the effects.

    // THE ON-SCREEN ROW ORDER, as chorale itself has it. `visible_grouped_view` is
    // post-filter, post-sort AND post-collapse, so a manual column re-cut, a column filter
    // and a group collapse all reach the cursor as a change to this one list — which is what
    // makes "focus survives a re-sort" and "focus survives a collapse" one rule rather than
    // two features. A memo rather than an inline read, for the same reason `active_sort` is
    // one: subscribing this whole subtree to `TableState` would re-render the table on every
    // scroll and selection write.
    let visible_keys = use_memo({
        let id_map = id_map.clone();
        move || {
            let state = sort_signal.read();
            chorale_core::visible_grouped_view(&state)
                .into_iter()
                .filter_map(|r| match r {
                    chorale_core::GroupedRow::Data(rid, _) => id_map.get(&rid).map(finding_key),
                    // Group headers are not rows the cursor can land on; `GroupedRow` is
                    // `#[non_exhaustive]`, so anything chorale adds later is skipped rather
                    // than silently treated as a focusable row.
                    _ => None,
                })
                .collect::<Vec<String>>()
        }
    });
    // The cursor itself. Stored unresolved and resolved against the CURRENT frame's row list
    // on every render, which is why `RowCursor`'s methods are pure and non-mutating: the
    // resolution is a read, not a write, so it cannot loop.
    let mut cursor = use_signal(RowCursor::new);
    let mut pane_focus = use_signal(|| PaneFocus::Elsewhere);
    let mut prompt = use_signal(|| Option::<TriageState>::None);
    let mut prompt_reason = use_signal(String::new);
    // The live undo, plus what it would take to put the row back ON SCREEN (the disposition
    // revert is the model's job; chorale's row set is this layer's).
    let mut undo_entry = use_signal(|| Option::<UndoEntry>::None);
    let mut undo_restore = use_signal(|| Option::<Vec<(usize, RowId, FindingView)>>::None);
    // A MONOTONIC clock, not a wall clock: the five-second window is measured against time
    // elapsed since this table mounted, so a system-clock change (NTP step, timezone, the
    // laptop waking) can neither extend the window nor close it early.
    let epoch = use_hook(std::time::Instant::now);
    let mut now_ms = use_signal(|| 0u64);

    let resolved = cursor.read().resolve(&visible_keys());
    let focused_finding: Option<FindingView> = focused_key(&cursor.read(), &visible_keys())
        .and_then(|k| key_to_row_id.get(&k))
        .and_then(|rid| id_map.get(rid).cloned());
    // A second handle on the focused row for the "Full detail" callback, and a third for the
    // rule-hub door in the pane's header (the palette calls the same two). Each `move` closure
    // owns its own copy; there is no shared borrow to hand round.
    let focused_for_modal = focused_finding.clone();
    let focused_for_hub = focused_finding.clone();
    // And one for the palette, which needs to know whether a row is focused and whether it can be
    // stacked when it opens.
    let focused_for_palette = focused_finding.clone();
    // And a fourth and fifth, for the code viewer: the finding it is handed, and the finding
    // its "enclosing block" fallback opens in the modal.
    let focused_for_viewer_modal = focused_finding.clone();
    // THE FOCUSED RULE, as a MEMO. It has to be one: `use_resource` re-runs when a signal
    // read inside its closure changes, and a plain captured `Option<String>` is not one — a
    // resource built over the captured value would fetch the first focused rule's fix once
    // and then never again, so every subsequent row would show that first rule's fix. A memo
    // also only notifies when its VALUE changes, so cursoring within one rule's group does
    // not refetch.
    let focused_rule = use_memo({
        let id_map = id_map.clone();
        let key_to_row_id = key_to_row_id.clone();
        move || {
            focused_key(&cursor.read(), &visible_keys())
                .and_then(|k| key_to_row_id.get(&k))
                .and_then(|rid| id_map.get(rid))
                .map(|f| f.rule_id.clone())
        }
    });
    // The recommended fix for the focused rule, from the SAME server function the report's
    // "Fix:" line and the xlsx's "Recommended Fix" column call — never a second derivation
    // here.
    let fix_res = use_resource(move || {
        let rid = focused_rule();
        async move {
            match rid {
                Some(rid) => fetch_finding_fix(&rid).await.map(|v| v.fix),
                None => None,
            }
        }
    });
    let focused_fix: Option<String> = fix_res.read().clone().flatten();
    // The focused finding's code-context request (work order B.3): the stored line plus three
    // lines either side, fetched once the cursor settles and drawn under the stored snippet.
    // A memo, for the reason `focused_rule` is one: it only notifies when the finding changes.
    let focused_target = use_memo({
        let id_map = id_map.clone();
        let key_to_row_id = key_to_row_id.clone();
        let shas = audited_shas.clone();
        move || {
            focused_key(&cursor.read(), &visible_keys())
                .and_then(|k| key_to_row_id.get(&k))
                .and_then(|rid| id_map.get(rid))
                .and_then(|f| ContextTarget::for_finding(f, shas.get(&f.repo).map(String::as_str)))
        }
    });
    let pane_context: Option<PaneContext> =
        super::pane_context::use_pane_context(focused_target, project_id.clone());

    // THE REVIEW TIMER's "opened a group": the rule group the cursor is in (INTERNAL, step 7a).
    // A memo, so the effect below runs when the GROUP changes and not on every move within it.
    // Keyed on the rule group (repo and rule id), which does not change when the Queue is re-cut.
    let focused_group = use_memo({
        let id_map = id_map.clone();
        let key_to_row_id = key_to_row_id.clone();
        move || {
            focused_key(&cursor.read(), &visible_keys())
                .and_then(|k| key_to_row_id.get(&k))
                .and_then(|rid| id_map.get(rid))
                .map(camerata_ui_core::review_timer::group_key_of)
        }
    });
    let review_group = review.clone();
    use_effect(move || {
        if let (Some(review), Some(group)) = (&review_group, focused_group()) {
            review.open_group(&group);
        }
    });

    // THE ORDER PRESET (work order B.4). `applied` is what the table is cut by now (the preset
    // it mounted with, until a change); it is PEEKED so this effect subscribes to the preset
    // alone. The re-cut keeps every row and its id, so the cursor, which holds a finding KEY, is
    // untouched; the only thing a re-group can do to it is collapse the group it is in, so that
    // group is opened again.
    let mut applied_preset = use_signal(|| initial_preset);
    use_effect(move || {
        let want = order_preset();
        if want == *applied_preset.peek() {
            return;
        }
        applied_preset.set(want);
        // The RESOLVED focus, not the stored key: after a disposition the stored key has left the
        // table, and a re-cut aimed at it would leave every group shut and the pane blank.
        let focused = focused_key(&cursor.peek(), &visible_keys.peek());
        recut_table(handle, want, focused.as_deref());
    });
    let pane_row = focused_finding.as_ref().map(|f| {
        DetailPaneRow::from_finding(
            f,
            descriptions.get(&f.rule_id).map(String::as_str),
            focused_fix.clone(),
            resolved.index(),
            visible_keys().len(),
        )
    });
    let undo_banner = undo_banner_of(&undo_entry.read(), now_ms());
    // DECISION MEMORY (B.5): what the draft already decided about the focused finding's shape,
    // derived from the disposition map on every render (so a decision shows up at once and an
    // undo removes it with no bookkeeping). The chip shows it; `I` and `F` start their reason
    // prompt with it. It applies nothing.
    let focused_prior: Option<PriorDecision> = focused_finding
        .as_ref()
        .and_then(|f| prior_decisions(&all_rows, &dispositions.read(), f));
    let prior_for_keys = focused_prior.clone();

    // APPLY one disposition to the focused row, arm the undo, and advance the cursor.
    //
    // A single closure for every entry point — the four keys, the four pane buttons, and the
    // reason prompt's Commit — so there is exactly one path from "the curator decided" to
    // "the map changed", and it is the one that captures the undo. `dispose_with_undo` owns
    // the require-reason refusal and routes the write through the same per-row functions the
    // toolbar buttons call.
    // A `Callback` rather than a plain closure because three call sites share it (the keys,
    // the pane buttons, and the reason prompt's Commit) and `Callback` is `Copy`. It is a
    // `use_callback`: the handle is stable and it always runs the closure of the LATEST render,
    // which is what lets the code viewer (hosted outside this table) hold it and still decide
    // the finding the cursor is on now, not the one it was on when the viewer opened.
    let key_to_row_id_pane = key_to_row_id.clone();
    let focused_for_apply = focused_finding.clone();
    // The review timer is told after a decision is written, never before, and never writes one.
    let review_apply = review.clone();
    let review_undo = review.clone();
    let review_stack = review.clone();
    // THE ONE WRITE PATH for a decision with an undo: these rows to this state. The pane's keys
    // and buttons pass the focused row; the stacked view passes its checked cards. It answers
    // `true` only when a disposition was written (see `apply_disposition`).
    //
    // `dispose_with_undo` owns the require-reason refusal and routes the write through the same
    // per-row functions the toolbar buttons call. Every row that was in this table leaves it
    // (which is what advances the cursor, and what makes a bulk decision shrink the table), and
    // where each came from is remembered so ONE undo puts all of them back on screen, not merely
    // in the map.
    let apply_rows = use_callback(
        move |(rows, state, reason): (Vec<FindingView>, TriageState, String)| -> bool {
            // Every caller checks for an empty set first and says something suitable ("nothing
            // is checked", "no row is focused"), because only the caller knows what happened.
            // An empty set arriving here is therefore a bug in a new caller. In release it is
            // still refused (below, as a count-of-zero warning) rather than silently ignored.
            debug_assert!(
                !rows.is_empty(),
                "apply_rows needs rows; the caller says why not"
            );
            let now = epoch.elapsed().as_millis() as u64;
            now_ms.set(now);
            let mut model = TriageModel {
                dispositions: dispositions.peek().clone(),
                triage_view,
            };
            let Some(entry) = model.dispose_with_undo(&rows, state, &reason, now) else {
                // The required-reason invariant (or an empty row set). The same wording as
                // every other refusal, and it names the count.
                crate::toast::push_toast(
                    toasts,
                    crate::toast::ToastKind::Warning,
                    reason_required_message(state, rows.len()),
                );
                return false;
            };
            dispositions.set(model.dispositions);
            // The review timer observes the decision (INTERNAL, step 7a): the row is now
            // "reviewed by the operator, at this time, after this much active time".
            if let Some(review) = &review_apply {
                review.decided(&rows, state);
            }
            // Drop the rows from THIS triage table. Remember where each was (its position in the
            // table's own row list BEFORE the removal, so re-inserting in ascending order puts
            // each back where it was) so Undo can restore the screen too. A row that is not in
            // the table (decided already, and checked again on purpose in the stacked view) has
            // nothing to put back.
            let mut ids: Vec<RowId> = Vec::new();
            let mut restore: Vec<(usize, RowId, FindingView)> = Vec::new();
            {
                let table = sort_signal.peek();
                for f in &rows {
                    let Some(rid) = key_to_row_id_pane.get(&finding_key(f)).copied() else {
                        continue;
                    };
                    let Some(at) = table.rows.iter().position(|(r, _)| *r == rid) else {
                        continue;
                    };
                    if !ids.contains(&rid) {
                        ids.push(rid);
                        restore.push((at, rid, f.clone()));
                    }
                }
            }
            restore.sort_by_key(|(at, _, _)| *at);
            handle.remove_rows(&ids);
            undo_restore.set((!restore.is_empty()).then_some(restore));
            let armed = entry.clone();
            undo_entry.set(Some(entry));
            // Tick the countdown while this undo is armed, and clear it when the window shuts.
            // The loop exits the moment a NEWER undo replaces this one, so two dispositions
            // inside five seconds cannot have the older ticker close the newer window.
            spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    let t = epoch.elapsed().as_millis() as u64;
                    now_ms.set(t);
                    if undo_entry.peek().as_ref() != Some(&armed) {
                        break;
                    }
                    if !armed.is_live(t) {
                        undo_entry.set(None);
                        undo_restore.set(None);
                        break;
                    }
                }
            });
            true
        },
    );
    // APPLY one disposition to the focused row. A single closure for every entry point of the
    // pane and the viewer, over the one write path above.
    //
    // A `Callback` rather than a plain closure because several call sites share it and
    // `Callback` is `Copy`. It is a `use_callback`: the handle is stable and it always runs the
    // closure of the LATEST render, which is what lets the code viewer (hosted outside this
    // table) hold it and still decide the finding the cursor is on now, not the one it was on
    // when the viewer opened.
    //
    // Answers `true` only when a disposition was written. A caller must branch on this and not
    // infer it from the disposition map: a blank reason for a row already in that state leaves
    // the map unchanged and is still a refusal.
    let apply_disposition = use_callback(move |(state, reason): (TriageState, String)| -> bool {
        let Some(f) = focused_for_apply.clone() else {
            return false;
        };
        if !apply_rows.call((vec![f], state, reason)) {
            return false;
        }
        prompt.set(None);
        prompt_reason.set(String::new());
        true
    });

    // The stacked view's open state is declared here (not beside its other wiring) because the
    // pane prompt's cancel below needs to know whether an overlay owns the keyboard.
    let mut stack_open = use_signal(|| Option::<super::stack::StackOpen>::None);
    // CANCEL THE PANE'S REASON PROMPT: Esc, and an undo (which moves the cursor to the restored
    // row, and a prompt left open would then write its reason onto THAT row). One helper so the
    // two cannot differ. Focus goes back to the pane unless the viewer or the stack is open,
    // which own the keyboard and must not have it taken.
    let cancel_pane_prompt = use_callback(move |()| {
        prompt.set(None);
        prompt_reason.set(String::new());
        if !*viewer_open.peek() && stack_open.peek().is_none() {
            // Move REAL focus back, and let `focusin` set the state, never assert it.
            focus_detail_pane();
        }
    });
    // THE UNDO, shared by the pane's button, the code viewer's and the stacked view's: one
    // action, three surfaces.
    let undo_action = use_callback(move |()| {
        let Some(mut entry) = undo_entry.peek().clone() else {
            return;
        };
        let now = epoch.elapsed().as_millis() as u64;
        let mut model = TriageModel {
            dispositions: dispositions.peek().clone(),
            triage_view,
        };
        match model.undo(&mut entry, now) {
            UndoOutcome::Reverted(n) => {
                // A row the undo put back to Unresolved is no longer reviewed (a row it put back
                // to an earlier decision still is).
                if let Some(review) = &review_undo {
                    let now_undecided: Vec<String> = entry
                        .row_keys()
                        .filter(|k| {
                            model
                                .dispositions
                                .get(*k)
                                .is_none_or(|d| d.state == TriageState::Unresolved)
                        })
                        .map(str::to_string)
                        .collect();
                    review.undone(&now_undecided);
                }
                dispositions.set(model.dispositions);
                // A reason being typed in the pane was for the row the cursor was on; the
                // restore below moves the cursor, so the prompt goes first.
                if prompt.peek().is_some() {
                    cancel_pane_prompt.call(());
                }
                // Put the rows back ON SCREEN too. Reverting only the map would
                // leave the toast truthfully claiming a revert while the rows stayed
                // gone from the table, which is the half-done undo this is for. In
                // ascending position order, so each lands where it was.
                if let Some(restore) = undo_restore.peek().clone() {
                    for (at, rid, f) in &restore {
                        handle.insert_row(*at, *rid, f.clone());
                    }
                    if let Some((_, _, first)) = restore.first() {
                        cursor.set(RowCursor::focused_on(finding_key(first)));
                    }
                }
                crate::toast::push_toast(
                    toasts,
                    crate::toast::ToastKind::Info,
                    format!("Undone: {n} finding(s) put back."),
                );
            }
            // Both refusals are REPORTED. A visible Undo that silently does nothing
            // is the exact failure the window exists to prevent.
            UndoOutcome::WindowClosed => crate::toast::push_toast(
                toasts,
                crate::toast::ToastKind::Warning,
                "That undo expired (the window is five seconds). Move the finding back from its triage table.",
            ),
            UndoOutcome::AlreadyUndone => crate::toast::push_toast(
                toasts,
                crate::toast::ToastKind::Warning,
                "That action was already undone.",
            ),
        }
        undo_entry.set(None);
        undo_restore.set(None);
    });

    // ── COMMITTING A RAISED TOOLBAR REASON (design §3, §9.2) ───────────────────────────
    //
    // ONE commit path for every raiser: the toolbar's per-selection buttons, the bulk bar's
    // `False positive all N`, and (by its own prompt) the stacked view. It is `apply_rows`, the
    // same function behind the pane's keys, so a prompted decision gets the same five-second
    // undo and the same single refusal wording as every other, and the rows leave the table by
    // the same code.
    //
    // The REQUIRE-REASON INVARIANT lives where it always did, inside
    // `TriageModel::dispose_with_undo`: a blank reason is refused there, `apply_rows` reports it,
    // and a refusal leaves the prompt OPEN with what the curator typed still in it. Closing it
    // would throw away a half-written justification to punish a trailing space.
    let commit_toolbar_reason = Callback::new(move |()| {
        let Some(p) = toolbar_prompt.peek().clone() else {
            return;
        };
        let reason = toolbar_reason.peek().clone();
        if apply_rows.call((p.rows, p.state, reason)) {
            toolbar_prompt.set(None);
            toolbar_reason.set(String::new());
        }
    });

    // THE TOOLBAR'S DECISION BUTTONS (Confirm, Save as tech debt, Move to ...). Every one is
    // `apply_rows` over the rows selected in the table, so it has the same write, the same
    // five-second undo and the same refusal wording as every other door. A state that needs a
    // reason (Ignored, False positive) raises the toolbar's reason prompt instead of writing,
    // with the selection snapshotted into it; `commit_toolbar_reason` finishes it.
    let toolbar_decide = use_callback({
        let id_map = id_map.clone();
        let all_rows = std::sync::Arc::clone(&all_rows);
        move |decided: TriageState| {
            let rows: Vec<FindingView> = handle
                .selected_ids()
                .iter()
                .filter_map(|id| id_map.get(id).cloned())
                .collect();
            if rows.is_empty() {
                return;
            }
            if decided.requires_reason() {
                // Decision memory (B.5): the same rule as the stack's prompt, a starting text
                // only when every selected row is offered the same reason.
                toolbar_reason.set(shared_prefill(
                    &rows,
                    &all_rows,
                    &dispositions.peek(),
                    decided,
                ));
                toolbar_prompt.set(Some(ToolbarReasonPrompt {
                    state: decided,
                    rows,
                }));
            } else {
                apply_rows.call((rows, decided, String::new()));
            }
        }
    });
    // The tech-debt bucket flag. Not a decision (the row stays Tech debt and in this table), so
    // it is the one toolbar write that does not go through `apply_rows`.
    let toolbar_bucket = use_callback({
        let id_map = id_map.clone();
        move |bucket: TechDebtBucket| {
            let picked: Vec<FindingView> = handle
                .selected_ids()
                .iter()
                .filter_map(|id| id_map.get(id).cloned())
                .collect();
            if picked.is_empty() {
                return;
            }
            let mut model = TriageModel {
                dispositions: dispositions.peek().clone(),
                triage_view,
            };
            model.set_bucket(&picked, bucket);
            dispositions.set(model.dispositions);
            let word = match bucket {
                TechDebtBucket::Later => "resolve later",
                TechDebtBucket::Now => "resolve now",
            };
            crate::toast::push_toast(
                toasts,
                crate::toast::ToastKind::Info,
                format!("Marked {} as {word}.", picked.len()),
            );
        }
    });

    // A request from another surface (the finding modal's "Open in viewer"): aim the cursor at
    // that finding and open the viewer. The request is consumed as it is read, so it fires once.
    {
        let key_to_row_id = key_to_row_id.clone();
        use_effect(move || {
            let Some(req) = viewer_request else { return };
            let Some(want) = req.0.read().clone() else {
                return;
            };
            let mut req_sig = req.0;
            req_sig.set(None);
            if !key_to_row_id.contains_key(&want) {
                crate::toast::push_toast(
                    toasts,
                    crate::toast::ToastKind::Info,
                    "That finding is not in this table. Switch to the table it is in, then open the viewer.",
                );
                return;
            }
            match super::viewer::reach_finding(handle, &want) {
                Ok(()) => {
                    cursor.set(RowCursor::focused_on(want));
                    viewer_open.set(true);
                }
                Err(why) => crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, why),
            }
        });
    }

    // THE CODE VIEWER is hosted at the results root (`viewer::ViewerRoot`), OUTSIDE this table's
    // subtree, like every other overlay here (see `RuleDetailModal`: an overlay mounted as a
    // sibling of the chorale table has left a ghost node swallowing the next click in the
    // desktop webview). This table hands it the handles it needs. They are all Copy signals and
    // stable callbacks, so the link is registered once and never goes stale.
    let viewer_slot = try_use_context::<super::viewer::ViewerSlot>();
    //
    // THE STACKED GROUP VIEW (work order B.2) is hosted the same way, and it is a layer under the
    // viewer: the viewer's `Enter`-from-a-card door opens it above the stack, and closing it
    // gives the keyboard back to the stack when one is open (the same `focus_element` the pane's
    // return uses), else to the pane.
    let mut stack_serial = use_signal(|| 0u64);
    let viewer_close = use_callback(move |()| {
        viewer_open.set(false);
        if stack_open.peek().is_some() {
            super::viewer::focus_element(super::stack::STACK_ID);
        } else {
            focus_detail_pane();
        }
    });
    let viewer_open_modal = use_callback(move |()| {
        if let Some(f) = focused_for_viewer_modal.clone() {
            detail_finding.set(Some(f));
        }
    });
    {
        // Built once: the data is frozen for this table's lifetime, like its rows.
        let link = use_hook(|| super::viewer::ViewerLink {
            handle,
            cursor,
            visible_keys,
            dispositions,
            all_findings: std::sync::Arc::clone(&all_rows),
            shas: std::sync::Arc::new(audited_shas.clone()),
            sha_conflicts: std::sync::Arc::new(sha_conflicts.clone()),
            project_id: project_id.clone(),
            open: viewer_open,
            undo_entry,
            now_ms,
            on_undo: undo_action,
            apply_disposition,
            on_close: viewer_close,
            on_open_modal: viewer_open_modal,
            origins: std::sync::Arc::new(audited_origins.clone()),
            descriptions: std::sync::Arc::new(descriptions.clone()),
            on_open_hub,
        });
        use_effect(move || {
            if let Some(mut slot) = viewer_slot {
                slot.0.set(Some(link.clone()));
            }
        });
        use_drop(move || {
            if let Some(mut slot) = viewer_slot {
                slot.0.set(None);
            }
        });
    }

    // ── THE STACKED GROUP VIEW's doors and link (work order B.2) ───────────────────────────
    //
    // Two doors, one function: the pane's `B` and the bulk bar's "Review all N stacked" button
    // both ask for a rule group by its header line. The rows are every row of that group in
    // THIS table, in the table's current order preset; the stack takes them as a snapshot, so a
    // card stays on screen after its row has been decided and has left the table.
    let open_stack = use_callback({
        let findings = findings.clone();
        move |(label, origin, anchor): (String, StackOrigin, Option<String>)| {
            let ordered = order_findings(*order_preset.peek(), &findings);
            let rows = rows_of_group(&ordered, &label);
            if rows.is_empty() {
                crate::toast::push_toast(
                    toasts,
                    crate::toast::ToastKind::Info,
                    "There are no rows of that group in this table to stack.",
                );
                return;
            }
            // Opening a group's stacked view is opening the group, for the review timer.
            if let (Some(review), Some(first)) = (&review_stack, rows.first()) {
                review.open_group(&camerata_ui_core::review_timer::group_key_of(first));
            }
            let id = *stack_serial.peek() + 1;
            stack_serial.set(id);
            stack_open.set(Some(super::stack::StackOpen {
                id,
                label,
                rows,
                origin,
                anchor,
            }));
        }
    });
    let focused_for_stack = focused_finding.clone();
    let open_stack_from_pane = use_callback(move |()| match focused_for_stack.clone() {
        Some(f) if is_stackable(&f) => {
            open_stack.call((
                rule_group_label(&f),
                StackOrigin::Pane,
                Some(finding_key(&f)),
            ));
        }
        _ => crate::toast::push_toast(
            toasts,
            crate::toast::ToastKind::Info,
            "B stacks the rows of a deviation or defect group. Focus one of its rows first.",
        ),
    });
    // ── EVERY PANE COMMAND, ONE FUNCTION ─────────────────────────────────────────────────
    //
    // What a pane command DOES. The pane's keys, its buttons and the command palette all run it,
    // so a command means the same thing from every door and there is no second description to
    // drift: the keys build the command with `pane_command`, the buttons with
    // `PaneCommand::decide` or the command named, and the palette carries the same value.
    let run_pane_command = use_callback(move |cmd: PaneCommand| match cmd {
        PaneCommand::MoveFocus(dir) => {
            let next = cursor.peek().moved(&visible_keys(), dir);
            cursor.set(next);
        }
        PaneCommand::OpenCode => viewer_open.set(true),
        PaneCommand::OpenStack => open_stack_from_pane.call(()),
        PaneCommand::Dispose(state) => {
            apply_disposition.call((state, String::new()));
        }
        PaneCommand::PromptFor(state) => {
            // `I` and `F` OPEN the reason prompt; they never dispose. The export reads those
            // reasons, and a keystroke is not a licence to drop them. Opening it also moves
            // focus into the prompt, which disarms the keys for exactly as long as the curator
            // is typing.
            //
            // Decision memory (B.5) only chooses what the field STARTS with. The reviewer
            // still presses Enter, and what is stored is what the field holds then.
            prompt.set(Some(state));
            prompt_reason.set(prefill_reason(prior_for_keys.as_ref(), state));
            pane_focus.set(PaneFocus::ReasonPrompt);
        }
    });
    // The pane's "Full detail" button, and the palette's line for it.
    let open_detail = use_callback(move |()| {
        if let Some(f) = focused_for_modal.clone() {
            detail_finding.set(Some(f));
        }
    });
    // The pane's rule id button (the Rule Hub), and the palette's line for it.
    let open_hub_for_focused = use_callback(move |()| {
        if let Some(f) = focused_for_hub.clone() {
            on_open_hub.call(RuleHubRequest {
                repo: f.repo.clone(),
                rule_id: f.rule_id.clone(),
            });
        }
    });
    // The Order select's change, and the palette's lines for the cuts.
    let set_order = use_callback(move |p: OrderPreset| {
        let mut order_preset = order_preset;
        order_preset.set(p);
    });

    // ── THE COMMAND PALETTE (work order B.8) ─────────────────────────────────────────────
    //
    // One list over the review actions above and below, opened with Ctrl+K / Cmd+K or the
    // toolbar's "Commands" button. It adds no action: `run_palette_action` is a match onto the
    // functions the buttons and keys already call. Hosted at the results root (`palette.rs`),
    // like the other overlays, and registered below.
    let palette_slot = try_use_context::<super::palette::PaletteSlot>();
    // The inspection walkthrough, for the palette's "Start walkthrough" line.
    let tour_handle = try_use_context::<crate::walkthrough::TourHandle>();
    let mut palette_open = use_signal(|| Option::<super::palette::PaletteOpen>::None);
    let mut palette_serial = use_signal(|| 0u64);
    // THE GATE, read from the live state: refused while ANY reason prompt is open (the pane's or
    // the toolbar's, exactly like `stack_focus`), while the viewer or the stack is up over the
    // Queue, and while any dialog is up. The dialogs are read two ways: this table knows the
    // full-detail view and the Rule Hub by their state, and the review bridge reports what the
    // DOM holds (`dom_modal`: any `.rule-modal-overlay`) when the chord is pressed, which also
    // catches the review-time view and any dialog added later. One closure, used by opening and
    // by running, so they cannot disagree.
    let hub_ctx = try_use_context::<super::viewer::ViewerHubOpen>();
    let palette_gate_now = move |dom_modal: bool| {
        let known_dialog =
            detail_finding.peek().is_some() || hub_ctx.is_some_and(|h| h.0.peek().is_some());
        palette_gate(
            prompt.peek().is_some() || toolbar_prompt.peek().is_some(),
            *viewer_open.peek() || stack_open.peek().is_some(),
            dom_modal || known_dialog,
        )
    };
    // What the entries depend on, now. Used to build the list when the palette opens and again
    // when a line is run, to see whether the line still means what it said.
    let review_for_ctx = review.clone();
    let palette_context = use_callback(move |()| -> PaletteContext {
        // The timer's two controls live in the strip, which is drawn only with both a tally and
        // a timer: the palette offers them under the same condition.
        let strip = if has_counts {
            review_for_ctx.as_ref()
        } else {
            None
        };
        PaletteContext {
            has_focus: focused_for_palette.is_some(),
            stackable: focused_for_palette.as_ref().is_some_and(is_stackable),
            can_undo: undo_banner_of(&undo_entry.peek(), *now_ms.peek()).is_some(),
            order: *order_preset.peek(),
            timer_button: strip.map(|r| r.timer_button().to_string()),
            has_stats: strip.is_some(),
        }
    });
    // A refused run leaves the keyboard where it belongs: in the reason prompt that refused it,
    // never on the pane (whose next key would drop the half-written reason).
    let focus_open_prompt = move || {
        if prompt.peek().is_some() {
            super::viewer::focus_element(PANE_REASON_ID);
        } else if toolbar_prompt.peek().is_some() {
            super::viewer::focus_element(QUEUE_REASON_ID);
        }
    };
    let review_palette = review.clone();
    // Run one line. Answers `true` only when its action ran.
    let run_palette_action = use_callback(move |entry: PaletteEntry| -> bool {
        if let Some(why) = palette_gate_now(false).refusal() {
            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, why);
            focus_open_prompt();
            return false;
        }
        // The list was built when the palette opened. If what a line named is gone, or it now
        // says something else (the timer paused itself), say so rather than do nothing or the
        // opposite.
        if let Some(note) = stale_note(&entry, &palette_entries(&palette_context.call(()))) {
            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, note);
            super::palette::restore_focus();
            return false;
        }
        match entry.action {
            PaletteAction::Pane(cmd) => run_pane_command.call(cmd),
            PaletteAction::Undo => undo_action.call(()),
            PaletteAction::OpenHub => open_hub_for_focused.call(()),
            PaletteAction::FullDetail => open_detail.call(()),
            PaletteAction::SetOrder(p) => set_order.call(p),
            // The strip's Pause / Resume button calls this same method.
            PaletteAction::ToggleTimer => {
                if let Some(review) = &review_palette {
                    review.toggle();
                }
            }
            PaletteAction::OpenStats => on_open_stats.call(()),
            // The Help menu's "Start walkthrough" calls this same function. It asks the page
            // for a reason prompt, an overlay or a dialog itself and refuses under one.
            PaletteAction::StartWalkthrough => {
                if let Some(tour) = tour_handle {
                    // Only this line may start it while the palette (which is running it) is open.
                    tour.start(camerata_ui_core::walkthrough::StartSource::Palette);
                }
            }
            // The review shell's own lines: this palette never lists them.
            PaletteAction::GoLane(_) | PaletteAction::FirstNeedingDecision => return false,
        }
        true
    });
    // Ctrl+K / Cmd+K, and the Commands button: open the palette, or close it when it is open (and
    // give the keyboard back). `dom_modal` is what the page reported about dialogs at the press.
    let toggle_palette = use_callback(move |dom_modal: bool| {
        if palette_open.peek().is_some() {
            palette_open.set(None);
            super::palette::restore_focus();
            return;
        }
        if let Some(why) = palette_gate_now(dom_modal).refusal() {
            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, why);
            return;
        }
        super::palette::remember_focus();
        let id = *palette_serial.peek() + 1;
        palette_serial.set(id);
        palette_open.set(Some(super::palette::PaletteOpen {
            id,
            entries: palette_entries(&palette_context.call(())),
        }));
    });
    {
        let link = use_hook(|| super::palette::PaletteLink {
            open: palette_open,
            on_run: run_palette_action,
            on_chord: toggle_palette,
        });
        use_effect(move || {
            if let Some(mut slot) = palette_slot {
                slot.0.set(Some(link.clone()));
            }
        });
        use_drop(move || {
            if let Some(mut slot) = palette_slot {
                slot.0.set(None);
            }
        });
    }

    // The viewer, opened on a card: aim the Queue's one cursor at the finding, open the groups
    // that hide it, and open the viewer. A card whose row was decided has left the table, so
    // there is nothing for the viewer to land on, and the curator is told how to get it back.
    let stack_open_viewer = use_callback(move |key: String| -> bool {
        match super::viewer::reach_finding(handle, &key) {
            Ok(()) => {
                cursor.set(RowCursor::focused_on(key));
                viewer_open.set(true);
                true
            }
            Err(why) => {
                crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, why);
                false
            }
        }
    });
    // Closing: the Queue's cursor goes to the card the curator was on (when its row is still in
    // the table) and the keyboard goes back to where the stack was opened from. A bulk-bar door
    // that has left the bar (its whole group was decided) falls back to the pane.
    let stack_close = use_callback({
        let bulk_groups = std::rc::Rc::clone(&bulk_groups);
        move |key: Option<String>| {
            let Some(open) = stack_open.peek().clone() else {
                return;
            };
            stack_open.set(None);
            let origin = match open.origin {
                StackOrigin::BulkBar(i)
                    if bulk_groups.get(i).is_none_or(|g| g.label != open.label) =>
                {
                    StackOrigin::Pane
                }
                other => other,
            };
            if let Some(key) = key {
                if super::viewer::reach_finding(handle, &key).is_ok() {
                    cursor.set(RowCursor::focused_on(key));
                }
            }
            super::viewer::focus_element(&super::stack::origin_focus_id(origin));
        }
    });
    {
        let stack_slot = try_use_context::<super::stack::StackSlot>();
        let link = use_hook(|| super::stack::StackLink {
            open: stack_open,
            dispositions,
            all_findings: std::sync::Arc::clone(&all_rows),
            shas: std::sync::Arc::new(audited_shas.clone()),
            project_id: project_id.clone(),
            undo_entry,
            now_ms,
            on_undo: undo_action,
            on_dispose: apply_rows,
            on_open_viewer: stack_open_viewer,
            on_close: stack_close,
        });
        use_effect(move || {
            if let Some(mut slot) = stack_slot {
                slot.0.set(Some(link.clone()));
            }
        });
        use_drop(move || {
            if let Some(mut slot) = stack_slot {
                slot.0.set(None);
            }
        });
    }

    // SECURITY findings (the deterministic floor — the only tier ranked "critical") get a
    // red full-row highlight so they're unmistakable beyond the badge text. This now uses
    // chorale 0.2.3's `row_class` hook on the Table (below), not a per-cell stripe renderer.
    let row_renderers = {
        let mut m: std::collections::HashMap<ColumnId, RowCellRenderer<FindingView>> =
            std::collections::HashMap::new();
        // "Finding type": the primary rule id, with a hover tooltip of what it enforces, and
        // a "+N" chip when the server merged N other rules at this same location into the row
        // (the also_matches set). Row-aware so it can read both the description map and the
        // also_matches off the FindingView. Tooltip lists the demoted rule ids too.
        let desc = descriptions.clone();
        m.insert(
            ColumnId("type"),
            std::sync::Arc::new(move |f: &FindingView, val: &CellValue| {
                let rid = match val {
                    CellValue::Text(s) => s.clone(),
                    _ => String::new(),
                };
                let mut tip = desc.get(&rid).cloned().unwrap_or_else(|| rid.clone());
                if !f.also_matches.is_empty() {
                    tip = format!("{tip}\n\nAlso violates here: {}", f.also_matches.join(", "));
                }
                let extra = f.also_matches.len();
                rsx! {
                    span { title: "{tip}", "{rid}" }
                    if extra > 0 {
                        span { class: "finding-also-count", title: "{tip}", " +{extra}" }
                    }
                }
            }) as RowCellRenderer<FindingView>,
        );
        // KIND chip. Hand-rolled rather than a chorale `RenderKind::Badge` because the three
        // states are not three colors of the same thing: `unspecified` is an ALARM about the
        // pipeline (red OUTLINE, its own label), and a filled badge from the same palette would
        // read as just another kind. Defect is the red family, Deviation the blue.
        m.insert(
            ColumnId("kind"),
            std::sync::Arc::new(move |f: &FindingView, _val: &CellValue| {
                let (cls, label, tip) = kind_chip_parts(kind_tag(f));
                rsx! { span { class: "{cls}", title: "{tip}", "{label}" } }
            }) as RowCellRenderer<FindingView>,
        );
        // REMOVED (design §9.3): the "Needs review" cell renderer — an orange chip followed by
        // the model's hedging SENTENCE, inline, at 300px, on every row that had one. It was
        // the only renderer in the Queue that drew prose it had recovered with a regex. The
        // Confidence column that replaces it is a chorale `RenderKind::Badge` over
        // `confidence_tag`'s closed set, so there is nothing left to override here; the reason
        // text reaches the curator in the detail pane, which is where §9.3 puts it.
        // Tech-debt bucket flag: reads the live disposition snapshot for this finding and
        // renders a "Later" / "Now" badge. Present only in the tech-debt view.
        if in_techdebt {
            let snap = bucket_snapshot.clone();
            m.insert(
                ColumnId("bucket"),
                std::sync::Arc::new(move |f: &FindingView, _val: &CellValue| {
                    let bucket = snap
                        .get(&finding_key(f))
                        .map(|d| d.bucket)
                        .unwrap_or(TechDebtBucket::Later);
                    let (label, cls) = match bucket {
                        TechDebtBucket::Later => ("Later", "td-bucket later"),
                        TechDebtBucket::Now => ("Now", "td-bucket now"),
                    };
                    rsx! { span { class: "{cls}", "{label}" } }
                }) as RowCellRenderer<FindingView>,
            );
        }
        RowCellRenderers::new(m)
    };

    rsx! {
        // Everything the reviewer works in (strip, toolbar, table, pane) is one review surface:
        // key and click activity inside it is what the review timer counts (the bridge in
        // `review.rs` checks for this attribute). `display: contents` keeps it out of the layout.
        //
        // (The palette's Ctrl+K / Cmd+K is not heard here: the review bridge hears it at the
        // document, so it works anywhere on the results screen. See `review.rs`.)
        div { style: "display: contents", "data-review-surface": "true",
        // REMOVED, design §9.5: the two-swatch findings key ("Security findings
        // (deterministic, stop-the-line)" / "Architectural findings (everything else)").
        //
        // It was superseded by the Kind + Severity + Authority columns, which say more, per
        // row, in less space — and it was also WRONG about the thing it claimed to explain.
        // The red full-row highlight is `row_class`, below: `f.severity == "critical"`, with
        // no reference to security, to determinism or to the floor rule set. A critical
        // AI-ADVISORY finding is highlighted and is not a "security finding (deterministic)";
        // a non-critical floor hit is a deterministic security finding and is not highlighted.
        // The per-row replacements state both facts without the mismatch: the Severity badge
        // reads "Critical" on exactly the highlighted rows, and the Authority badge reads
        // "Rule · enforced" on exactly the deterministic-floor ones.
        //
        // The red full-row critical highlight itself STAYS (§9.5 says so explicitly).
        // THE PROGRESS STRIP (B.7): the same tally above whichever triage table is showing,
        // taken from `TriageModel::counts` over every Queue row. `dispositions.read()` is what
        // re-renders it on a decision and on an undo.
        if let Some(counts) = counts {
            if let Some(review) = review.clone() {
                super::review::LiveProgressStrip {
                    counts,
                    review,
                    on_open_stats,
                    // The button took DOM focus; the pane's keys need it back.
                    on_toggled: Callback::new(move |()| focus_detail_pane()),
                }
            } else {
                ProgressStrip { counts }
            }
        }
        div {
            class: "findings-toolbar",
            // The walkthrough points here for the review queue (the decisions live in this bar).
            "data-tour": camerata_ui_core::walkthrough::target::REVIEW_QUEUE,
            // View-specific triage actions. A move writes the new disposition for each
            // selected finding and drops it from this table; it reappears under its target
            // table on the next switch. Backend commit happens later, at Process.
            match triage_view {
                TriageState::Unresolved => rsx! {
                    // CONFIRM: the affirmative disposition, and the majority outcome of an audit
                    // (design §3). First and styled as the primary action, and the ONLY one with
                    // no reason in front of it: confirming makes no claim beyond the finding.
                    button {
                        class: "btn-run triage-confirm",
                        title: "I read these findings, they are real, and they ship in the report. No reason required. Local to this draft; the export reads it and Process commits nothing for it.",
                        id: "{toolbar_button_id(TriageState::Confirmed)}",
                        onclick: move |_| toolbar_decide.call(TriageState::Confirmed),
                        "Confirm \u{2192}"
                    }
                    button {
                        class: "btn-restart triage-ignore",
                        id: "{toolbar_button_id(TriageState::Ignored)}",
                        onclick: move |_| toolbar_decide.call(TriageState::Ignored),
                        "Ignore with reason \u{2192}"
                    }
                    button {
                        class: "btn-run",
                        id: "{toolbar_button_id(TriageState::TechDebt)}",
                        onclick: move |_| toolbar_decide.call(TriageState::TechDebt),
                        "Save as tech debt"
                    }
                    button {
                        class: "btn-restart triage-false-positive",
                        id: "{toolbar_button_id(TriageState::FalsePositive)}",
                        onclick: move |_| toolbar_decide.call(TriageState::FalsePositive),
                        "Mark false positive \u{2192}"
                    }
                },
                TriageState::Confirmed => rsx! {
                    button {
                        class: "btn-restart",
                        id: "{toolbar_button_id(TriageState::Unresolved)}",
                        onclick: move |_| toolbar_decide.call(TriageState::Unresolved),
                        "Move to Unresolved"
                    }
                },
                TriageState::Ignored => rsx! {
                    button {
                        class: "btn-restart",
                        id: "{toolbar_button_id(TriageState::Unresolved)}",
                        onclick: move |_| toolbar_decide.call(TriageState::Unresolved),
                        "Move to Unresolved"
                    }
                    button {
                        class: "btn-run",
                        id: "{toolbar_button_id(TriageState::TechDebt)}",
                        onclick: move |_| toolbar_decide.call(TriageState::TechDebt),
                        "Move to Tech debt"
                    }
                },
                TriageState::TechDebt => rsx! {
                    // Bucket the selected tech-debt findings. These stay in the table; only the
                    // Bucket flag column changes (it is not a decision, so it has no reason and
                    // no undo). Default is Later (a tracked ticket); Now pulls the finding into
                    // the dev engine as a fix story at Process.
                    button {
                        class: "btn-edit-sm",
                        id: "camerata-toolbar-bucket-later",
                        onclick: move |_| toolbar_bucket.call(TechDebtBucket::Later),
                        "Mark: resolve later"
                    }
                    button {
                        class: "btn-edit-sm",
                        id: "camerata-toolbar-bucket-now",
                        onclick: move |_| toolbar_bucket.call(TechDebtBucket::Now),
                        "Mark: resolve now"
                    }
                    button {
                        class: "btn-restart",
                        id: "{toolbar_button_id(TriageState::Unresolved)}",
                        onclick: move |_| toolbar_decide.call(TriageState::Unresolved),
                        "Move to Unresolved"
                    }
                    // Ignored requires a reason, so this raises the same prompt "Ignore with
                    // reason" does. It used to move the rows to Ignored with none.
                    button {
                        class: "btn-restart",
                        id: "{toolbar_button_id(TriageState::Ignored)}",
                        onclick: move |_| toolbar_decide.call(TriageState::Ignored),
                        "Move to Ignored with reason \u{2192}"
                    }
                },
                TriageState::FalsePositive => rsx! {
                    button {
                        class: "btn-restart",
                        id: "{toolbar_button_id(TriageState::Unresolved)}",
                        onclick: move |_| toolbar_decide.call(TriageState::Unresolved),
                        "Move to Unresolved"
                    }
                },
                // A merge is not a table (see `TriageState::MergedInto`): the view is never
                // set to it, and the merge screen owns the gesture.
                TriageState::MergedInto => rsx! {},
            }
            button {
                class: "btn-edit-sm",
                onclick: move |_| {
                    let csv = findings_csv(&csv_rows);
                    spawn(async move { let _ = save_csv("camerata-findings.csv", csv).await; });
                },
                "Export CSV"
            }
            button {
                class: "btn-edit-sm",
                id: super::palette::PALETTE_BUTTON_ID,
                "data-tour": camerata_ui_core::walkthrough::target::SPEED_TOOLS,
                title: "Open the command palette: every review action in one filtered list (Ctrl+K or Cmd+K).",
                onclick: move |_| toggle_palette.call(false),
                "Commands"
            }
        }
        // `clear_sort` drops the whole sort list in one act, which is why the bar's button
        // exists rather than relying on chorale's third click: a plain click cycles ONE column
        // back to unsorted, but a shift+click multi-sort leaves two or three columns set and no
        // single click undoes all of them. This is correct from any sort state. With
        // `state.sort` empty, chorale's `filtered_sorted_pairs` stops reordering entirely and
        // hands back `state.rows` — the rows exactly as `TriageModel::visible` ordered them —
        // so the restored default is byte-identical, not approximately restored.
        QueueOrderBar {
            order: queue_order(order_preset(), &active_sort(), &order_columns),
            on_reset: Callback::new(move |()| handle.clear_sort()),
            preset: order_preset(),
            on_preset: set_order,
        }
        // The rule-level deviation decisions, one row per group. Renders nothing when the
        // view holds no deviations.
        DeviationBulkBar {
            groups: bulk_rows,
            // The same write path as every other door (`apply_rows`): the group's rows, Confirmed,
            // with the five-second undo that names the count.
            on_confirm_all: Callback::new({
                let bulk_groups = std::rc::Rc::clone(&bulk_groups);
                move |i: usize| {
                    let Some(group) = bulk_groups.get(i) else { return };
                    apply_rows.call((group.rows.clone(), TriageState::Confirmed, String::new()));
                }
            }),
            on_review_stacked: Callback::new({
                let bulk_groups = std::rc::Rc::clone(&bulk_groups);
                move |i: usize| {
                    let Some(group) = bulk_groups.get(i) else { return };
                    open_stack.call((group.label.clone(), StackOrigin::BulkBar(i), None));
                }
            }),
            // The rule id on a bulk row opens the hub. The repo comes off the group's own
            // first row rather than from a table-level guess: `rule_hub` is scoped per
            // (repo, rule) and a wrong repo would silently describe a different assessment.
            on_open_hub: Callback::new({
                let bulk_groups = std::rc::Rc::clone(&bulk_groups);
                move |i: usize| {
                    let Some(group) = bulk_groups.get(i) else { return };
                    let Some(first) = group.rows.first() else { return };
                    on_open_hub.call(RuleHubRequest {
                        repo: first.repo.clone(),
                        rule_id: group.rule_id.clone(),
                    });
                }
            }),
            // §9.2 CHANGED THIS BUTTON'S REASON SOURCE, deliberately and visibly. It used to
            // read `fp_reason()` — the always-visible toolbar textarea — which meant a
            // rule-level act with a possibly stale reason typed for something else, and no
            // statement anywhere of how many rows it was about to move. It now raises the
            // SAME on-demand prompt the per-selection button raises, carrying this group's
            // index and its advertised count. The require-reason invariant does not move: it
            // is still `TriageModel::dispose_with_undo`'s refusal, enforced at commit.
            on_false_positive_all: Callback::new({
                let bulk_groups = std::rc::Rc::clone(&bulk_groups);
                let all_rows = std::sync::Arc::clone(&all_rows);
                move |i: usize| {
                    let Some(group) = bulk_groups.get(i) else { return };
                    toolbar_reason.set(shared_prefill(
                        &group.rows,
                        &all_rows,
                        &dispositions.peek(),
                        TriageState::FalsePositive,
                    ));
                    toolbar_prompt.set(Some(ToolbarReasonPrompt {
                        state: TriageState::FalsePositive,
                        rows: group.rows.clone(),
                    }));
                }
            }),
        }
        // The raised prompt, mounted once for both sources. Rendering nothing when closed is
        // the point of §9.2: the Queue's resting state has no reason box in it at all.
        if let Some(p) = toolbar_prompt() {
            QueueReasonPrompt {
                prompt: p,
                reason: toolbar_reason(),
                on_reason: Callback::new(move |r: String| toolbar_reason.set(r)),
                on_cancel: Callback::new(move |()| {
                    toolbar_prompt.set(None);
                    toolbar_reason.set(String::new());
                }),
                on_commit: commit_toolbar_reason,
            }
        }
        // THE SPLIT: the Queue table and its docked detail pane, side by side. The pane is a
        // SIBLING of the table rather than content inside it — chorale 0.2.3's
        // `detail_renderer` is an expandable inline row-detail `<tr>` beneath its parent row,
        // not a docked panel, so an inline detail would reflow the table under the cursor on
        // every cursor-down. See `FindingDetailPane`'s doc comment.
        div { class: "queue-split",
        div { class: "queue-split-table",
        CamerataTable {
            handle,
            sort_enabled: true,
            filter_enabled: true,
            selection_enabled: true,
            resize_enabled: true,
            // Grouped by RULE GROUP -> file; the wrapper collapses every group on mount so a
            // long audit opens collapsed and the architect drills in deliberately.
            //
            // The outer key is `RULE_GROUP_COLUMN`, not the visible "type" column, because the
            // group header's text IS that column's cell value in chorale — so grouping on
            // `rule_group_label` is what puts the deviation sentence on the header (design
            // §2/§4). It partitions identically to the rule id: the sentence's numbers are
            // group-level and repeat verbatim on every row of a rule.
            group_by: group_columns(initial_preset),
            // A flat cut has no groups, but the cursor must still walk every row.
            load_all: true,
            // 0.2.3: an expand-all / collapse-all control in the grouped header (the
            // findings table groups by rule -> file), so a long audit collapses at once.
            group_expand_toggle: true,
            row_cell_renderers: row_renderers,
            // Critical rows get a red full-row highlight via the 0.2.3 conditional
            // row-styling hook. §9.5 removed the legend above the table and kept THIS, so the
            // predicate is a named pure function rather than an inline closure: "the
            // highlight stays" is then an assertion a test can make, instead of a claim about
            // a closure no test can reach.
            row_class: RowClass::new(finding_row_class),
            // A row click FOCUSES the pane on that row; it no longer pops the modal
            // (design §9.6 — "the row-click modal for Queue rows, replaced by the docked
            // detail pane"). Mouse and keyboard drive the SAME `RowCursor`, so there is one
            // notion of "the focused row" rather than two that can disagree. The modal is
            // still one click away, from the pane's own "Full detail" button, and it still
            // earns that: it carries the on-demand enclosing-code-block fetch, which is far
            // too expensive to re-run on every cursor-down.
            on_row_click: Callback::new(move |rid: RowId| {
                // While the pane's reason prompt is open the reason being typed is FOR the
                // focused row: the commit decides whatever row the cursor is on. A click that
                // moved the cursor would send the sentence typed for row A to row B, so the
                // click is refused (the same rule as the stacked view's mouse doors under its
                // prompt) and, because the refusal also skips the focus call below, the
                // keyboard stays in the reason field. Commit or Esc first.
                if prompt.peek().is_some() {
                    crate::toast::push_toast(
                        toasts,
                        crate::toast::ToastKind::Info,
                        "Finish or cancel the reason first, then pick another row.",
                    );
                    return;
                }
                if let Some(f) = id_map_click.get(&rid) {
                    cursor.set(RowCursor::focused_on(finding_key(f)));
                    // Move REAL keyboard focus into the pane, and let the pane's own
                    // `focusin` set `PaneFocus::Cursor` from that. Setting the signal here
                    // instead would be a lie the curator could act on: the keydown handler
                    // lives on the pane element, so with DOM focus still in the table a `C`
                    // would reach nothing while the pane reported its keys armed. The one
                    // state the indicator reads must come from the one thing that is true.
                    focus_detail_pane();
                }
            }),
        }
        }
        FindingDetailPane {
            row: pane_row,
            focus: pane_focus(),
            prompt: prompt(),
            prompt_reason: prompt_reason(),
            undo: undo_banner,
            // THE SCOPED KEY HANDLER. Every press goes through core's `pane_command`, which
            // refuses everything unless `focus == PaneFocus::Cursor` — so the reason
            // textarea inches away cannot have its `c` read as a Confirm.
            on_key: Callback::new(move |(key, mods): (String, KeyMods)| {
                let here = *pane_focus.peek();
                if let Some(cmd) = pane_command(here, &key, mods) {
                    run_pane_command.call(cmd);
                }
            }),
            on_focus: Callback::new(move |f: PaneFocus| {
                // A blur out of the pane while the reason prompt is open must not disarm
                // the prompt's own focus state — the textarea is INSIDE the pane, so
                // focusing it fires the region's `focusout` first.
                //
                // The same holds the other way: closing the code viewer refocuses the pane,
                // and with the reason prompt still open that must not re-arm the keys.
                if let Some(next) = next_pane_focus(f, prompt.peek().is_some()) {
                    pane_focus.set(next);
                }
            }),
            on_prompt_reason: Callback::new(move |r: String| prompt_reason.set(r)),
            on_prompt_commit: Callback::new(move |()| {
                // Read into a local FIRST: a `peek()` guard in an `if let` scrutinee lives for
                // the whole block, and `apply_disposition` writes `prompt` (a borrow panic).
                let open = *prompt.peek();
                if let Some(state) = open {
                    let reason = prompt_reason.peek().clone();
                    apply_disposition.call((state, reason));
                    // Only hand focus back if the disposition actually LANDED.
                    // `dispose_with_undo` refuses a blank reason and leaves the prompt open;
                    // re-arming the keys over a still-open prompt would put the curator back
                    // in exactly the state the Q5 clause forbids, one keystroke from a
                    // Confirm while mid-sentence.
                    if prompt.peek().is_none() {
                        focus_detail_pane();
                    }
                }
            }),
            on_prompt_cancel: cancel_pane_prompt,
            on_undo: undo_action,
            // The pane's own buttons. A reason-carrying state opens the prompt first, exactly as
            // its key does: the same command, built by the same `decide`.
            on_dispose: Callback::new(move |state: TriageState| {
                run_pane_command.call(PaneCommand::decide(state));
            }),
            on_open_detail: open_detail,
            on_open_hub: open_hub_for_focused,
            on_open_code: Callback::new(move |()| run_pane_command.call(PaneCommand::OpenCode)),
            on_open_stack: open_stack_from_pane,
            context: pane_context,
            previously: focused_prior.as_ref().map(PriorDecision::chip_text),
        }
        }
        }
    }
}
