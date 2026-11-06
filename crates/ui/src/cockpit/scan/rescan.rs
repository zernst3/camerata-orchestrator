//! The alternatives controls, the why-modal, the rescan bar and the rule alternatives panel.

use super::*;

/// Feature B — the compliance-safety gate (backend-safety-and-live-models design doc):
/// BACK-COMPAT FALLBACK ONLY. The server now sets an explicit `ScanReport.blocked` flag on a
/// `Blocked` resolution (API-only project, no Anthropic key), which the caller keys off
/// directly. This shape-inference remains solely for reports serialized before that flag
/// existed. It detects the zero-stats-plus-message shape a block leaves behind — but excludes
/// any `⚠ COMPLIANCE:`-prefixed message, since that marks a `CliFallbackWarn` scan (which DID
/// run, just on the CLI) and could otherwise be mistaken for a block when it happened to scan
/// an empty repo.
pub(super) fn scan_report_looks_blocked(report: &ScanReportView) -> bool {
    !report.gated
        && report.files_scanned == 0
        && report.findings.is_empty()
        && report.proposed_rules.is_empty()
        && report
            .message
            .as_deref()
            .is_some_and(|m| !m.trim().is_empty() && !m.contains(COMPLIANCE_NOTE_PREFIX))
}

/// The exact prefix `audit_repos` (crates/server/src/onboard.rs) attaches to a
/// `CliFallbackWarn` resolution's message before folding it into the report's joined-by-
/// " · " `notes` -> `message`. Kept as a single constant so the UI-side split below and any
/// future match stay byte-for-byte in sync with the server string.
pub(super) const COMPLIANCE_NOTE_PREFIX: &str = "\u{26a0} COMPLIANCE:";

/// Split a `ScanReport::message` into `(compliance-warning notes, remaining notes)`.
///
/// `audit_repos` joins every note for the run with `" · "` into one string (see
/// crates/server/src/onboard.rs ~line 600), so a `⚠ COMPLIANCE:`-prefixed `CliFallbackWarn`
/// warning can arrive interleaved with ordinary per-repo scan notes ("AI review deselected",
/// "audit failed", etc). This pulls the compliance-prefixed segments out (prefix stripped,
/// trimmed) so the caller can render them in a dedicated, higher-alarm banner instead of
/// letting them blend into the routine note line. Returns `other` as `None` when nothing is
/// left over, so the caller can skip rendering an empty `.scan-note` box.
pub(super) fn split_compliance_notes(message: &str) -> (Vec<String>, Option<String>) {
    let mut compliance = Vec::new();
    let mut other = Vec::new();
    for part in message.split(" · ") {
        let trimmed = part.trim();
        match trimmed.strip_prefix(COMPLIANCE_NOTE_PREFIX) {
            Some(rest) => compliance.push(rest.trim().to_string()),
            None => {
                if !trimmed.is_empty() {
                    other.push(trimmed.to_string());
                }
            }
        }
    }
    let other = if other.is_empty() {
        None
    } else {
        Some(other.join(" · "))
    };
    (compliance, other)
}

/// Rescan button / sticky-bar label: singular "rule" for exactly one staged override, plural
/// otherwise. Pure so the pluralization is unit-tested without a render.
pub(in crate::cockpit) fn rescan_button_label(pending: usize) -> String {
    if pending == 1 {
        "Rescan 1 overridden rule".to_string()
    } else {
        format!("Rescan {pending} overridden rules")
    }
}

/// Pure staging decision for the "Change" dropdown: picking the option a rule is ALREADY
/// evaluated against clears any staged override for it (back to the AI's pick, or whatever a
/// prior rescan applied); picking anything else stages that rule's override. Extracted out of
/// the `<select>`'s `onchange` handler so this decision has a directly-testable pure core — a
/// Tier-1 SSR render test cannot simulate a real `<select>` change event (see docs/UI_TESTING.md).
pub(in crate::cockpit) fn stage_or_clear_override(
    pending: &mut std::collections::HashMap<String, String>,
    rule_id: &str,
    evaluated_option_id: &str,
    picked_option_id: &str,
) {
    if picked_option_id == evaluated_option_id {
        pending.remove(rule_id);
    } else {
        pending.insert(rule_id.to_string(), picked_option_id.to_string());
    }
}

/// ONE multi-option rule's ALTERNATIVES CONTROL LINE, flattened into exactly what renders.
///
/// Flat, owned, `PartialEq` data rather than the three sources it is joined from
/// (`ProposedRuleView`, `RuleRecommendationView`, the staged-override map) — the same shape,
/// and the same reason, as `DetailPaneRow` and `DeviationBulkRow`. Every judgement (which
/// option the rule is CURRENTLY evaluated against, which badge that earns, whether the
/// override is staged) is made once in [`alternative_controls_row`] and unit-tested there;
/// the component chooses words and pixels.
///
/// Built as a precondition of design §9.1, which moves this line off the standalone panel and
/// into the surfaces that describe the rule. A line that can only be produced inside one
/// component's render body cannot move anywhere.
#[derive(Clone, PartialEq)]
pub(in crate::cockpit) struct AlternativeControlsRow {
    pub rule_id: String,
    pub title: String,
    /// How many of this scan's findings are attributed to this rule. Zero is a normal and
    /// important value: a multi-option rule the codebase fully complies with still needs
    /// reviewing, because the review is of the OPTION, not of the violations.
    pub finding_count: usize,
    /// The option the rule is evaluated against RIGHT NOW, resolved in priority order:
    /// a staged override, else the option the findings were actually produced under, else
    /// the AI's recommendation, else the rule's declared default.
    pub evaluated_option_id: String,
    /// That option's authored label, or `(none)` when the id matches no option on the rule.
    pub evaluated_label: String,
    pub badge_class: &'static str,
    pub badge_text: &'static str,
    /// An override is staged for this rule and has not been rescanned yet.
    pub staged: bool,
    /// The architect has explicitly accepted this rule's current pick in this session.
    pub accepted: bool,
    /// The model judged the rule applicable to this repo. `false` renders a warning.
    pub applicable: bool,
    /// The `file:line` citation the model grounded its pick in, when it gave one.
    pub evidence: Option<String>,
    /// `(option_id, label)` for the Change select, in the rule's own order.
    pub options: Vec<(String, String)>,
    /// The model's reasoning, for the Why? modal.
    pub reasoning: String,
    /// The model's raw answer was not a real option and the pick shown is a fallback.
    pub hallucinated: bool,
}

/// Build one rule's control line out of the scan's three sources. PURE, so the priority order
/// below is unit-testable rather than buried in a render body.
///
/// `evaluated_by_rule` maps rule id -> the option id that rule's findings were produced under
/// (every finding for one rule shares it); `staged` is the pending override, if any.
pub(in crate::cockpit) fn alternative_controls_row(
    rule: &ProposedRuleView,
    rec: &RuleRecommendationView,
    finding_count: usize,
    evaluated_in_findings: Option<&str>,
    staged: Option<&str>,
    accepted: bool,
) -> AlternativeControlsRow {
    // THE PRIORITY ORDER, and why it is this way round. A staged override wins because it is
    // what the next rescan will use and the curator needs to see what he just picked. Below
    // that, the option the FINDINGS were produced under beats the AI's recommendation,
    // because a prior rescan can have already applied an override that the recommendation
    // field does not know about — showing the recommendation there would describe the scan
    // the curator is NOT looking at.
    let evaluated_option_id = staged
        .map(str::to_string)
        .or_else(|| evaluated_in_findings.map(str::to_string))
        .or_else(|| Some(rec.recommended_option_id.clone()).filter(|s| !s.is_empty()))
        .or_else(|| rule.default_option.clone())
        .unwrap_or_default();
    let evaluated_label = rule
        .options
        .iter()
        .find(|o| o.id == evaluated_option_id)
        .map(|o| o.label.clone())
        .unwrap_or_else(|| "(none)".to_string());
    let (badge_class, badge_text) = if staged.is_some() {
        ("rec-badge rec-badge-pending", "Override, pending rescan")
    } else if rec.operator_chosen {
        ("rec-badge rec-badge-yours", "Your choice")
    } else {
        ("rec-badge rec-badge-ai", "AI recommended")
    };
    AlternativeControlsRow {
        rule_id: rule.id.clone(),
        title: rule.title.clone(),
        finding_count,
        evaluated_option_id,
        evaluated_label,
        badge_class,
        badge_text,
        staged: staged.is_some(),
        accepted,
        applicable: rec.applicable,
        evidence: rec.evidence.clone().filter(|e| !e.is_empty()),
        options: rule
            .options
            .iter()
            .map(|o| (o.id.clone(), o.label.clone()))
            .collect(),
        reasoning: rec.recommendation_reasoning.clone(),
        hallucinated: rec.hallucinated,
    }
}

/// THE ALTERNATIVES CONTROL LINE, rendered (design §4's group-header line, §9.1's relocation).
///
/// > Evaluated against: URI-versioned endpoints (AI recommended) · Why? · Change
///
/// Props-only, so SSR can render it in every badge state — including the two a click would be
/// needed to reach through the panel (an override staged, and an accepted rule). That is the
/// same discipline `ProcessGate`, `DeviationBulkBar` and `FindingDetailPane` are built on, and
/// it is what makes §9.1's relocation checkable rather than hopeful: the line can now be
/// rendered anywhere a rule id is, and asserted on wherever it lands.
#[component]
pub(in crate::cockpit) fn RuleAlternativeControls(
    row: AlternativeControlsRow,
    /// Open the Why? modal for this rule.
    on_why: Callback<()>,
    /// The curator picked an option in the Change select. Invoked with the picked option id;
    /// the caller decides whether that stages an override or clears one
    /// (see [`stage_or_clear_override`]).
    on_pick: Callback<String>,
) -> Element {
    let evaluated_id = row.evaluated_option_id.clone();
    rsx! {
        div { class: "rec-row", key: "{row.rule_id}",
            div { class: "rec-row-main",
                span { class: "rec-row-id", "{row.rule_id}" }
                span { class: "rec-row-title", "{row.title}" }
                span { class: "rec-row-count",
                    if row.finding_count == 0 { "0 findings" } else { "{row.finding_count} finding(s)" }
                }
            }
            div { class: "rec-line",
                "Evaluated against: "
                strong { "{row.evaluated_label}" }
                span { class: "{row.badge_class}", "{row.badge_text}" }
                if row.accepted {
                    span { class: "rec-badge rec-badge-accepted", "Accepted" }
                }
                if !row.applicable {
                    span { class: "rec-not-applicable", "Not applicable to this repo" }
                }
                button {
                    class: "btn-edit-sm rec-why",
                    onclick: move |_| on_why.call(()),
                    "Why?"
                }
            }
            // Evidence (P7): the file:line citation the model grounded its pick in — "a wrong
            // pick costs a glance, not a false finding". Absent for legacy reports and
            // operator-forced picks with no model evidence, so this line only renders when
            // present.
            if let Some(ev) = row.evidence.clone() {
                span { class: "rec-evidence", "Evidence: {ev}" }
            }
            select {
                class: "rec-select",
                value: "{evaluated_id}",
                onchange: move |e: Event<FormData>| on_pick.call(e.value()),
                for (id , label) in row.options.iter() {
                    option { key: "{id}", value: "{id}", "{label}" }
                }
            }
        }
    }
}

/// The "Why?" explanation modal: shows the recommendation's reasoning (or a fallback note if
/// none was recorded) plus a hallucination notice when the model's raw answer had to be
/// corrected. Extracted out of `RuleAlternativesPanel` as its own props-only component so it
/// can be render-tested directly in its "open" shape — the parent only mounts it conditionally
/// on a local `use_signal`, which a Tier-1 SSR test cannot toggle via a simulated click.
#[component]
pub(in crate::cockpit) fn RuleWhyModal(
    rule_id: String,
    title: String,
    reasoning: String,
    hallucinated: bool,
    on_close: EventHandler<()>,
) -> Element {
    rsx! {
        div { class: "rule-modal-overlay", onclick: move |_| on_close.call(()),
            div { class: "rule-modal", onclick: move |e| e.stop_propagation(),
                div { class: "rule-modal-head",
                    span { class: "rule-modal-id", "{rule_id}" }
                    button { class: "rule-modal-close", onclick: move |_| on_close.call(()), "\u{2715}" }
                }
                p { class: "rule-modal-title", "{title}" }
                p { class: "rule-modal-label", "Why this alternative" }
                p { class: "rec-modal-reasoning",
                    if reasoning.is_empty() {
                        "No reasoning was recorded for this recommendation."
                    } else {
                        "{reasoning}"
                    }
                }
                if hallucinated {
                    span { class: "rec-hallucinated-note",
                        "The model did not return a valid choice for this rule; this is the selected or default option shown instead."
                    }
                }
            }
        }
    }
}

/// The sticky batched-rescan action bar: appears once at least one override is staged, showing
/// the count and a button that fires the batched rescan. Extracted out of
/// `RuleAlternativesPanel` as its own props-driven component so the "N staged overrides shows
/// the bar with the right count" shape is render-testable directly (a Tier-1 SSR test cannot
/// stage an override via a simulated `<select>` change to reach this state through the parent).
#[component]
pub(in crate::cockpit) fn RescanStickyBar(
    pending_count: usize,
    rescanning: bool,
    on_rescan: EventHandler<()>,
) -> Element {
    // A rescan re-reads the source, so it is off while any of the project's source is deleted
    // (the hint carries the way back).
    let block = crate::source_ui::use_scan_block();
    rsx! {
        div { class: "rec-sticky-bar",
            span { class: "rec-sticky-label", "{rescan_button_label(pending_count)}" }
            button {
                class: "btn-run",
                disabled: rescanning || block.is_some(),
                onclick: move |_| on_rescan.call(()),
                if rescanning { "Rescanning\u{2026}" } else { "{rescan_button_label(pending_count)}" }
            }
            if let Some(block) = block.clone() {
                crate::source_ui::ScanBlockNotice { block }
            }
        }
    }
}

/// The "Rule alternatives" review panel (audit-integrated alternative recommendation,
/// docs/design/2026-09-22_audit-integrated-alternatives.md). Lists every multi-option rule the
/// audit evaluated (every key of `recommendations`), including rules with zero violations, so
/// the architect can review — and, if they disagree, override — any of them.
///
/// This single panel serves BOTH halves of the design's UX: the per-rule "Evaluated against:
/// <option> (AI recommended) · Why? · Change" control line (normally a findings-table
/// group-header line) AND the standalone "every rule in scope" review panel. chorale's grouped
/// `Table` (the findings table below) exposes no group-header content-injection hook, so rather
/// than duplicate this row UI across two disconnected surfaces — risking the staged-override
/// state diverging between them — it is rendered ONCE, directly above the findings table it
/// describes, which reads exactly like a header for what follows.
///
/// Rendered (and re-keyed) alongside the findings table in `ScanResults`, so a fresh audit
/// remounts it with clean staged-override state.
///
/// # Design §9.1: WHY THIS PANEL IS STILL HERE
///
/// §9.1 is the removals sweep's first item and asks for this panel to go, with its
/// Evaluated-against line, Why?, Change select and staged-override/rescan flow moving "into
/// the rule group headers and the rule hub modal". Both destinations were checked against the
/// built product rather than against the design's description of it, and between them they do
/// not cover what this panel does. The halves:
///
/// * **MOVED (done).** The Evaluated-against line and the Why? answer now also render inside
///   [`RuleHubStrip`], built by the SAME `alternative_controls_row` this panel uses, so the
///   two cannot disagree. A curator reading any rule id in the Queue reaches that context in
///   one click, which is §9.1's actual complaint — "duplicating per-rule context away from the
///   rows it describes" — addressed.
///
/// * **CANNOT MOVE: the group headers take no content.** Established at build time and
///   recorded in the design's own §13 retraction: `render_grouped_row` / `group_header_tr` are
///   private in `chorale-dioxus` 0.2.3 and `group_header_class` is a CSS class string. The
///   deviation SENTENCE reaches the header only because it IS the grouped-on cell value; a
///   `<select>` and a button cannot be a cell value. (chorale is not to be modified.)
///
/// * **CANNOT MOVE: the hub cannot reach a rule with no findings.**
///   `camerata_ui_core::triage::rule_hub` returns `None` when a rule has no rows in the repo,
///   deliberately — its own doc says "there is no '0 findings' hub, an empty strip is a dead
///   affordance". But this panel exists precisely to list "every multi-option rule the audit
///   evaluated (every key of `recommendations`), INCLUDING RULES WITH ZERO VIOLATIONS, so the
///   architect can review — and, if they disagree, override — any of them". A clean rule is
///   the one whose option choice is least visible and most in need of a second look. Deleting
///   the panel would make those rules unreachable: no Queue row, no panel row, no hub.
///
/// So the Change select, the staged-override map and the rescan/accept flow stay here, where
/// every rule in scope can be reached. The hub names this panel as their home rather than
/// leaving the curator to hunt for it.
///
/// Removing this panel is possible, but it needs a surface that enumerates the rules the scan
/// found nothing for — which is a new thing to build, not a relocation, and §9.1 does not
/// describe one.
#[component]
pub(in crate::cockpit) fn RuleAlternativesPanel(
    project_id: String,
    proposed_rules: Vec<ProposedRuleView>,
    findings: Vec<FindingView>,
    repos: Vec<String>,
    recommendations: std::collections::HashMap<String, RuleRecommendationView>,
    audit: Signal<Option<ScanReportView>>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    // The architect's per-repo alternative choices (ScanResults provides this via context) —
    // updated here too after a successful rescan, so Arm's directive resolution matches the
    // operator's forced choice rather than the pre-override default.
    let mut chosen = use_context::<Signal<std::collections::HashMap<String, String>>>();

    // rule_id -> staged override option id. A failed rescan leaves the override in place (so
    // the architect can retry or adjust) rather than silently discarding it.
    let mut pending = use_signal(std::collections::HashMap::<String, String>::new);
    // Which rule's Why modal is open (None = closed).
    let mut why_open = use_signal(|| Option::<String>::None);
    let mut rescanning = use_signal(|| false);
    let mut accepting = use_signal(|| false);
    // Rule ids the architect has explicitly accepted THIS session — purely a local echo of the
    // server-persisted chosen_option so the "Accepted" badge updates without a re-fetch.
    let mut accepted_ids = use_signal(std::collections::HashSet::<String>::new);

    // The rules in scope: every multi-option rule the audit recommended on, joined against the
    // scan's proposed-rules list for its title + options (the audit response itself carries no
    // rule metadata — only the scan's `proposed_rules` does).
    let mut rows: Vec<&ProposedRuleView> = proposed_rules
        .iter()
        .filter(|r| recommendations.contains_key(&r.id) && r.options.len() >= 2)
        .collect();
    rows.sort_by(|a, b| a.id.cmp(&b.id));

    if rows.is_empty() {
        // No multi-option rule was part of this audit — nothing to review.
        return rsx! {};
    }
    // Computed BEFORE the rsx! block (not derived from `rows` inside a `move` closure below) so
    // the Accept button's closure can take ownership without fighting `rows`'s borrow.
    let rule_ids_in_scope: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
    // A separate clone for the rescan closure below (each `move` closure needs its own owned
    // copy; `rule_ids_in_scope` itself is still needed, untouched, by the Accept closure).
    let rule_ids_for_rescan_filter = rule_ids_in_scope.clone();
    // Likewise: `project_id` is a plain `String` prop (not `Copy`), and BOTH the rescan and
    // Accept closures below are `move` closures that need their own owned copy — a `move`
    // closure takes the captured variable itself, not a borrow, so referencing the shared prop
    // from two separate `move` closures needs two clones made before either closure exists.
    let project_id_for_rescan = project_id.clone();

    // rule_id -> finding count (0 for a clean rule) + the option id its findings were actually
    // evaluated under (every finding for one rule shares the same evaluated_option_id).
    let mut finding_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut evaluated_by_rule: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for f in &findings {
        *finding_counts.entry(f.rule_id.clone()).or_insert(0) += 1;
        if let Some(oid) = &f.evaluated_option_id {
            evaluated_by_rule
                .entry(f.rule_id.clone())
                .or_insert_with(|| oid.clone());
        }
    }

    // Only count/send overrides for rules STILL in scope — a stale entry left over from a
    // previous audit (a different rule selection) must never inflate the sticky bar or get
    // sent to the rescan endpoint for a rule that no longer even renders a row.
    let pending_count = rows
        .iter()
        .filter(|r| pending.read().contains_key(&r.id))
        .count();

    rsx! {
        div { class: "rec-panel",
            p { class: "rec-panel-head", "Rule alternatives" }
            p { class: "rec-panel-sub",
                "For every rule with more than one adopted alternative, the audit picked the option it judged best fitting and evaluated the codebase against it (shown below as \"Evaluated against\"). Review each one; change any you disagree with, then rescan just the ones you changed."
            }
            div { class: "rec-rows",
                for r in rows.iter() {
                    {
                        let rid = r.id.clone();
                        let rid_for_select = rid.clone();
                        let rid_for_why = rid.clone();
                        let rec = recommendations.get(&rid).cloned().unwrap_or_default();
                        // ONE derivation, shared with every other surface that will render
                        // this line (design §9.1). The priority order that resolves
                        // "evaluated against" used to live inline here; it is now
                        // `alternative_controls_row`, pure and unit-tested.
                        let row = alternative_controls_row(
                            r,
                            &rec,
                            finding_counts.get(&rid).copied().unwrap_or(0),
                            evaluated_by_rule.get(&rid).map(String::as_str),
                            pending.read().get(&rid).map(String::as_str),
                            accepted_ids.read().contains(&rid),
                        );
                        let evaluated_id = row.evaluated_option_id.clone();
                        rsx! {
                            RuleAlternativeControls {
                                key: "{rid}",
                                row,
                                on_why: Callback::new(move |()| why_open.set(Some(rid_for_why.clone()))),
                                on_pick: Callback::new(move |picked: String| {
                                    stage_or_clear_override(
                                        &mut pending.write(),
                                        &rid_for_select,
                                        &evaluated_id,
                                        &picked,
                                    );
                                }),
                            }
                        }
                    }
                }
            }
        }
        // Why modal: shared across rows, keyed by whichever rule's "Why?" was clicked.
        if let Some(rid) = why_open() {
            {
                let title = rows
                    .iter()
                    .find(|r| r.id == rid)
                    .map(|r| r.title.clone())
                    .unwrap_or_default();
                let rec = recommendations.get(&rid).cloned().unwrap_or_default();
                rsx! {
                    RuleWhyModal {
                        rule_id: rid.clone(),
                        title,
                        reasoning: rec.recommendation_reasoning,
                        hallucinated: rec.hallucinated,
                        on_close: move |_| why_open.set(None),
                    }
                }
            }
        }
        // Sticky batched-rescan bar: appears once at least one override is staged. Drives the
        // centralized LoadingGuard (inside `rescan_alternatives`) for the whole round trip —
        // no ad-hoc spinner; `rescanning` only swaps the button's own label.
        if pending_count > 0 {
            RescanStickyBar {
                pending_count,
                rescanning: rescanning(),
                on_rescan: move |_| {
                    // Filtered to rules still in scope — a stale entry left over from a
                    // previous audit run reusing this same mounted component instance (a
                    // different rule selection, no remount in between) must never be sent.
                    let overrides: Vec<AlternativeOverride> = pending
                        .read()
                        .iter()
                        .filter(|(rule_id, _)| rule_ids_for_rescan_filter.contains(rule_id))
                        .map(|(rule_id, chosen_option_id)| AlternativeOverride {
                            rule_id: rule_id.clone(),
                            chosen_option_id: chosen_option_id.clone(),
                        })
                        .collect();
                    if overrides.is_empty() {
                        return;
                    }
                    let pid = project_id_for_rescan.clone();
                    let repos = repos.clone();
                    let rescanned_ids: std::collections::HashSet<String> =
                        overrides.iter().map(|o| o.rule_id.clone()).collect();
                    let overrides_snapshot = overrides.clone();
                    rescanning.set(true);
                    spawn(async move {
                        match rescan_alternatives(&pid, &overrides).await {
                            Some(resp) if resp.ok => {
                                let current_report = audit.peek().clone();
                                if let Some(current) = current_report {
                                    let (merged_findings, merged_recs) = merge_rescanned_alternatives(
                                        &current.findings,
                                        &current.recommendations,
                                        &rescanned_ids,
                                        resp.findings,
                                        resp.recommendations,
                                    );
                                    let mut updated = current;
                                    updated.findings = merged_findings;
                                    updated.recommendations = merged_recs;
                                    audit.set(Some(updated));
                                }
                                for o in &overrides_snapshot {
                                    for repo in &repos {
                                        chosen
                                            .write()
                                            .insert(chosen_key(repo, &o.rule_id), o.chosen_option_id.clone());
                                    }
                                }
                                pending.write().clear();
                                let msg = resp.message.unwrap_or_else(|| {
                                    format!("Rescanned {} rule(s).", overrides_snapshot.len())
                                });
                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, msg);
                            }
                            Some(resp) => {
                                crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Error,
                                    resp.message.unwrap_or_else(|| "Rescan failed.".to_string()),
                                );
                            }
                            None => {
                                crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Error,
                                    "Rescan failed. Could not reach the server.",
                                );
                            }
                        }
                        rescanning.set(false);
                    });
                },
            }
        }
        // Accept alternatives: persists the CURRENT per-rule selection (the AI's where the
        // architect never overrode it, the operator's where a rescan already applied one) into
        // the project's chosen_option. Disabled while an override is staged but not yet
        // rescanned — accepting then would persist the STALE (pre-override) recommendation.
        div { class: "findings-toolbar",
            button {
                class: "btn-edit-sm",
                disabled: accepting() || pending_count > 0,
                title: if pending_count > 0 { "Rescan the pending override(s) first." } else { "" },
                onclick: move |_| {
                    let pid = project_id.clone();
                    let all_ids = rule_ids_in_scope.clone();
                    accepting.set(true);
                    spawn(async move {
                        match accept_alternatives(&pid, None).await {
                            Some(resp) if resp.ok => {
                                accepted_ids.write().extend(all_ids);
                                crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Info,
                                    format!("Accepted {} alternative(s).", resp.accepted),
                                );
                            }
                            Some(resp) => {
                                crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Error,
                                    resp.message.unwrap_or_else(|| "Accept failed.".to_string()),
                                );
                            }
                            None => {
                                crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Error,
                                    "Accept failed. Could not reach the server.",
                                );
                            }
                        }
                        accepting.set(false);
                    });
                },
                if accepting() { "Accepting\u{2026}" } else { "Accept alternatives" }
            }
        }
    }
}
