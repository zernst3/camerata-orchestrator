//! The scan banners and the conventions, no-convention, not-assessed and patterns panels.

use super::*;

/// The background-JOB failure banner: renders when an async audit job ends in the
/// `failed` state (the server's `jobs.fail` — see `run_job_body_with_panic_guard` and the
/// "no local repos" / panic-guard failure paths in `camerata_server`'s
/// `onboard_audit_start`), or when polling loses contact with a job that never reported
/// back. Distinct from [`AiReviewBlockedBanner`]: that one renders from a COMPLETED
/// report's `ai_blocked_reason` / `ai_error` fields, but a failed (or vanished) job never
/// produces a report at all (`audit` stays `None`), so without this banner the scan just
/// stops — the spinner clears and nothing else appears. That silent stop is the exact
/// "the scan dies with no error" bug this closes. Reuses the `onboard-gate` banner classes
/// for visual consistency with `AiReviewBlockedBanner`. Its own component (rather than
/// inline in `ScanResults`) so it is directly SSR-testable without standing up
/// `ScanResults`'s network-backed resources.
#[component]
pub(in crate::cockpit) fn ScanJobFailedBanner(message: Option<String>) -> Element {
    rsx! {
        if let Some(msg) = message {
            div {
                class: "onboard-gate scan-job-failed-banner",
                "data-tour": camerata_ui_core::walkthrough::target::FAILED_PASSES,
                span { class: "onboard-gate-dot" }
                div {
                    p { class: "onboard-gate-h", "Scan failed" }
                    p { class: "onboard-gate-b", "{msg}" }
                    p { class: "onboard-gate-hint",
                        "The background scan stopped before it could finish. Check your project's backend setup (Claude CLI on PATH, or an Anthropic API key), then retry the scan."
                    }
                }
            }
        }
    }
}

/// The progress line under the bar. Passes the spend limit refused are named on their own and
/// are NOT in `done`, so a stopped scan never reads as complete.
pub(in crate::cockpit) fn job_progress_label(
    done: usize,
    total: usize,
    findings: usize,
    not_run: usize,
) -> String {
    if not_run > 0 {
        format!("{done}/{total} passes \u{b7} {not_run} NOT RUN (spend limit) \u{b7} {findings} finding(s) so far")
    } else {
        format!("{done}/{total} passes \u{b7} {findings} finding(s) so far")
    }
}

/// The INCOMPLETE banner: renders whenever any pass or stage failed or did not run (a spend
/// limit stop, a failed pass, a skipped stage) and nothing otherwise. Before this the desktop
/// app showed no failed pass of any cause, so an incomplete scan read as a clean one.
/// `compact` is the one-line form shown beside the export button, where the export will carry
/// the same disclosure. Its own component so it is SSR-testable without `ScanResults`.
#[component]
pub(in crate::cockpit) fn IncompletePassesBanner(
    failed_passes: Vec<FailedPassView>,
    compact: bool,
    // The saved run this scan is. With it, "Run what remains" is live; without, it stays off.
    #[props(default)] run_id: Option<String>,
    // What the resume would do, once asked for (`on_plan`): shown before anything is sent.
    #[props(default)] plan: Option<ResumePlanView>,
    // True while the resume job runs.
    #[props(default)] busy: bool,
    #[props(default)] on_plan: Option<EventHandler<String>>,
    #[props(default)] on_resume: Option<EventHandler<String>>,
) -> Element {
    if failed_passes.is_empty() {
        return rsx! {};
    }
    let n = failed_passes.len();
    let stopped_by_limit = failed_passes
        .iter()
        .any(|f| f.reason.contains("spend limit"));
    if compact {
        return rsx! {
            div { class: "onboard-gate scan-incomplete-banner scan-incomplete-compact",
                span { class: "onboard-gate-dot" }
                p { class: "onboard-gate-b",
                    "Incomplete: {n} pass(es) or stage(s) did not run. The export will say so."
                }
            }
        };
    }
    rsx! {
        div { class: "onboard-gate scan-incomplete-banner",
            span { class: "onboard-gate-dot" }
            div {
                p { class: "onboard-gate-h", "Incomplete: {n} pass(es) or stage(s) did not run" }
                if stopped_by_limit {
                    p { class: "onboard-gate-b",
                        "Stopped by the spend limit. The findings below cover only the part of the code and the rules that did run."
                    }
                }
                ul { class: "scan-incomplete-list",
                    for (i , f) in failed_passes.iter().enumerate() {
                        li { key: "{i}",
                            details { class: "scan-failure",
                                summary { "{f.repo}: {f.pass} \u{2014} {f.reason}" }
                                p { class: "scan-failure-meta",
                                    "Class: {f.class}. Repo: {f.repo}. Part: {f.pass}. Files: {f.files.len()}."
                                }
                                if !f.files.is_empty() {
                                    p { class: "scan-failure-files", "{f.files.join(\", \")}" }
                                }
                                ol { class: "scan-failure-attempts",
                                    for (n , a) in f.attempts.iter().enumerate() {
                                        li { key: "{n}",
                                            "{a.line()}"
                                            if !a.rejections.is_empty() {
                                                ul { class: "scan-failure-rejections",
                                                    for (m , r) in a.rejections.iter().enumerate() {
                                                        li { key: "{m}", "Turned down: {r}" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                button {
                                    class: "scan-failure-copy",
                                    r#type: "button",
                                    onclick: {
                                        let entry = f.clone();
                                        move |_| {
                                            let text = serde_json::to_string_pretty(&entry).unwrap_or_default();
                                            spawn(async move {
                                                let _ = crate::source_ui::copy_to_clipboard(&text).await;
                                            });
                                        }
                                    },
                                    "Copy as JSON"
                                }
                            }
                        }
                    }
                }
                button {
                    class: "scan-run-what-remains",
                    r#type: "button",
                    disabled: run_id.is_none() || busy,
                    title: if run_id.is_some() {
                        "Show what would run again and what it should cost. Nothing is sent until you confirm."
                    } else {
                        "This scan was not saved, so it cannot be resumed. Run it again."
                    },
                    onclick: {
                        let run_id = run_id.clone();
                        move |_| {
                            if let (Some(id), Some(handler)) = (run_id.clone(), on_plan) {
                                handler.call(id);
                            }
                        }
                    },
                    "Run what remains"
                }
                if let Some(plan) = plan.clone() {
                    div { class: "scan-resume-plan",
                        pre { class: "scan-resume-plan-text", "{plan.text}" }
                        if let Some(why) = plan.refusal.clone() {
                            p { class: "onboard-gate-b scan-resume-refusal", "Cannot resume now: {why}" }
                        } else {
                            button {
                                class: "scan-resume-confirm",
                                r#type: "button",
                                disabled: busy,
                                onclick: {
                                    let run_id = run_id.clone();
                                    move |_| {
                                        if let (Some(id), Some(handler)) = (run_id.clone(), on_resume) {
                                            handler.call(id);
                                        }
                                    }
                                },
                                "Start: run only these parts"
                            }
                        }
                    }
                }
                p { class: "onboard-gate-hint",
                    "A clean result here does NOT mean the missing parts are clean. Raise the limit or re-run to cover them."
                }
            }
        }
    }
}

/// The compliance-gate fix's unmissable banner: renders when the audit's AI review either
/// (a) did not run at all because the compliance backend gate was `Blocked` (`ai_blocked_reason`
/// set), or (b) was requested but every pass failed with a real error (`ai_error` set). Renders
/// nothing when both are `None` (the ordinary happy path — most audits). Deliberately its own
/// small component (rather than inline in `ScanResults`) so it is directly SSR-testable without
/// standing up `ScanResults`'s network-backed resources. See
/// `camerata_server::onboard::ScanReport::ai_blocked_reason` / `ai_error` for the server side.
#[component]
pub(in crate::cockpit) fn AiReviewBlockedBanner(
    ai_blocked_reason: Option<String>,
    ai_error: Option<String>,
) -> Element {
    rsx! {
        if let Some(reason) = ai_blocked_reason {
            div {
                class: "onboard-gate ai-review-blocked-banner",
                "data-tour": camerata_ui_core::walkthrough::target::FAILED_PASSES,
                span { class: "onboard-gate-dot" }
                div {
                    p { class: "onboard-gate-h", "AI review did not run" }
                    p { class: "onboard-gate-b", "AI review did not run: {reason}" }
                    p { class: "onboard-gate-hint",
                        "Add an Anthropic API key in Settings, or enable \u{201c}Allow Claude CLI (personal subscription)\u{201d} for this project, then re-run the audit for the AI review. The deterministic security floor still ran; its findings (if any) are shown below."
                    }
                }
            }
        } else if let Some(err) = ai_error {
            div {
                class: "onboard-gate ai-review-error-banner",
                "data-tour": camerata_ui_core::walkthrough::target::FAILED_PASSES,
                span { class: "onboard-gate-dot" }
                div {
                    p { class: "onboard-gate-h", "AI review failed" }
                    p { class: "onboard-gate-b", "AI review did not run: {err}" }
                    p { class: "onboard-gate-hint",
                        "Check the selected audit model id and your Anthropic API key or Claude CLI setup, then re-run the audit. The deterministic security floor still ran; its findings (if any) are shown below."
                    }
                }
            }
        }
    }
}

/// The one-rule-one-surface invariant, rendered (design §8).
///
/// The pipeline guarantees a rule emits deviations OR a convention row OR one no-convention row
/// OR a not-assessed row — never two in the same scan. The UI ASSERTS that rather than
/// defending against it: when it is violated, something upstream is mislabelling rows, and
/// quietly picking a winner would hide the bug while corrupting the curator's counts. So this
/// banner names the rule and the surfaces it reached, and nothing else changes — the rows still
/// render where they routed, because suppressing them would hide evidence too.
///
/// Renders nothing in the healthy case, which is every case.
#[component]
pub(in crate::cockpit) fn KindSurfaceConflictBanner(findings: Vec<FindingView>) -> Element {
    let conflicts = rules_on_multiple_surfaces(&findings);
    if conflicts.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "kind-conflict-banner",
            p { class: "kind-conflict-h",
                "Pipeline bug: {conflicts.len()} rule(s) reached more than one kind surface in this scan"
            }
            p { class: "kind-conflict-b",
                "A rule emits deviations OR a convention OR one no-convention row OR a not-assessed row \u{2014} never two. The counts below are therefore not trustworthy for these rules. This is an upstream emission bug, not something to triage here."
            }
            ul { class: "kind-conflict-list",
                for ((repo , rule_id) , surfaces) in conflicts.iter() {
                    {
                        let names: Vec<&'static str> = surfaces.iter().map(|s| s.label()).collect();
                        rsx! {
                            li { key: "{repo}\u{0}{rule_id}", "{rule_id} ({repo}) \u{2192} {names.join(\", \")}" }
                        }
                    }
                }
            }
        }
    }
}

/// CONVENTIONS panel — the conventions this audit DERIVED from the client's own codebase, and
/// the decision to write them into that codebase's `AGENTS.md`.
///
/// ## What is in the list
///
/// Not only the `ConventionObserved` rows. Per the owner's §13 reversal, the convention BEHIND
/// a rule's `Deviation` rows is in here too, labelled "observed with N exceptions" with a jump
/// into the Queue for those exceptions:
///
/// > "I could decide that 'this is the correct convention even though there are inconsistencies
/// > that exist'. The remediation of those inconsistencies is a separate process."
///
/// So this panel takes the WHOLE finding set, not just the rows routed to its own surface, and
/// derives its list through `adopt::adoptable_conventions`. The exception count is context; no
/// code here reads it to decide anything.
///
/// ## Opt-out, and what that costs
///
/// Adopt is pre-checked (Q4) and Skip is the gesture. That makes the common case fast and it
/// makes one thing dangerous, which the commit dialog is built to say out loud: a convention
/// nobody opened can land in a client's `AGENTS.md`. This panel therefore records an OPENED
/// gesture per row — the "Open" button, which reveals the row's detail — and feeds it to
/// [`CommitPlan::unopened_count`]. Rendering a row is not opening it; nothing else writes that
/// record.
///
/// NO SEVERITY is rendered, here or on the other two panels (design §9.4): informational kinds
/// ship at `info` by construction, so a severity badge would imply a ranking decision that does
/// not exist.
///
/// Props-only, like every other Pass-2 component, so SSR can render each state.
#[component]
pub(in crate::cockpit) fn ConventionsPanel(
    /// The scan's findings. The WHOLE set, not one surface's rows — see the type doc.
    findings: Vec<FindingView>,
    /// Open the RULE HUB on a listed rule (design §8: "reachable from every surface that
    /// shows a rule id"). Defaulted so the panel's existing render tests keep compiling and
    /// asserting unchanged.
    #[props(default)]
    on_open_hub: Callback<RuleHubRequest>,
    /// The curator's adopt/skip and openness state. Default = nothing skipped, nothing
    /// opened, which is the opt-out starting position.
    #[props(default)]
    model: AdoptionModel,
    /// Flip one convention between Adopt and Skip. Carries its [`adoption_key`].
    #[props(default)]
    on_toggle: Callback<String>,
    /// The curator OPENED this convention's detail. The ONE writer of the openness record.
    #[props(default)]
    on_open: Callback<String>,
    /// Raise the commit dialog for one destination repository.
    #[props(default)]
    on_commit: Callback<String>,
    /// JUMP INTO THE QUEUE for one rule's exceptions, carrying the rule id (§13: a convention
    /// with deviations is adoptable "with a jump into the Queue").
    ///
    /// Deliberately the SAME gesture the rule hub's own jump performs — a column filter on
    /// the Queue, design §8's "a filter, not a hyperlink graph" — rather than a second
    /// navigation concept. It does not mark the convention opened and it does not change its
    /// adopt state: looking at the exceptions is not a decision about the convention.
    #[props(default)]
    on_review_exceptions: Callback<String>,
    /// Show ONLY the adopted-but-unopened rows — the commit dialog's third exit.
    #[props(default)]
    only_unopened: bool,
    /// Leave the unopened-only view.
    #[props(default)]
    on_clear_filter: Callback<()>,
    /// Whether a commit is in flight (disables the commit bars).
    #[props(default)]
    busy: bool,
) -> Element {
    let all = adoptable_conventions(&findings);
    if all.is_empty() {
        return rsx! {
            p { class: "kind-panel-empty", "No conventions observed in this scan." }
        };
    }
    let with_exceptions = all.iter().filter(|c| c.deviating_sites > 0).count();
    // One group per DESTINATION repo, because one commit writes one `AGENTS.md` and the gate
    // asks for one repository name. A single-repo scan renders one group.
    let mut by_repo: std::collections::BTreeMap<String, Vec<AdoptableConvention>> =
        Default::default();
    for c in &all {
        by_repo.entry(c.repo.clone()).or_default().push(c.clone());
    }
    rsx! {
        div { class: "kind-panel",
            p { class: "kind-panel-sub",
                "Conventions this audit derived from the client's own codebase. Adopting one writes it into that repository's AGENTS.md, where it governs how future AI agents work on the code. Adoption is OPT-OUT: every convention below is selected unless you skip it."
            }
            if with_exceptions > 0 {
                p { class: "kind-panel-sub adopt-exceptions-note",
                    "{with_exceptions} of these are conventions the codebase mostly follows; their exceptions are rows on the Queue tab and are counted there, not here. A convention is adoptable whatever its exception count \u{2014} adopting states the standard going forward; remediating the exceptions is separate work."
                }
            }
            if only_unopened {
                div { class: "adopt-filter-bar",
                    span { class: "adopt-filter-label", "Showing only what this commit would write without your having opened it." }
                    button {
                        class: "btn-ghost adopt-filter-clear",
                        onclick: move |_| on_clear_filter.call(()),
                        "Show all"
                    }
                }
            }
            for (repo , rows) in by_repo.iter() {
                {
                    let plan = CommitPlan::conventions(&all, &model, repo);
                    let unopened: std::collections::BTreeSet<String> =
                        plan.unopened_rule_ids().into_iter().collect();
                    let shown: Vec<AdoptableConvention> = if only_unopened {
                        rows.iter().filter(|c| unopened.contains(&c.rule_id)).cloned().collect()
                    } else {
                        rows.clone()
                    };
                    rsx! {
                        div { key: "{repo}", class: "kind-panel-group adopt-group",
                            p { class: "kind-panel-group-h", "{repo}" }
                            ul { class: "kind-panel-list",
                                for c in shown.iter() {
                                    {
                                        let key = c.key();
                                        let adopted = model.is_adopted(&key);
                                        let opened = model.was_opened(&key);
                                        let adoptable = c.is_adoptable();
                                        let req = RuleHubRequest {
                                            repo: c.repo.clone(),
                                            rule_id: c.rule_id.clone(),
                                        };
                                        let toggle_key = key.clone();
                                        let open_key = key.clone();
                                        rsx! {
                                            li {
                                                key: "{key}",
                                                class: if adopted && adoptable { "kind-panel-row adopt-row adopted" } else { "kind-panel-row adopt-row skipped" },
                                                span {
                                                    class: if adoptable && adopted { "adopt-state on" } else { "adopt-state off" },
                                                    if !adoptable {
                                                        "Cannot write"
                                                    } else if adopted {
                                                        "Adopt"
                                                    } else {
                                                        "Skipped"
                                                    }
                                                }
                                                button {
                                                    class: "rule-hub-link kind-panel-rule",
                                                    title: "Everything this scan knows about {c.rule_id}.",
                                                    onclick: move |_| on_open_hub.call(req.clone()),
                                                    "{c.rule_id}"
                                                }
                                                span { class: "kind-panel-label", "{c.label}" }
                                                span { class: "adopt-status", "{c.status_line()}" }
                                                if !opened {
                                                    span { class: "adopt-unopened-flag", title: "You have not opened this one.", "not opened" }
                                                }
                                                span { class: "kind-panel-repo", "{c.repo}" }
                                                button {
                                                    class: "btn-ghost adopt-open",
                                                    onclick: move |_| on_open.call(open_key.clone()),
                                                    if opened { "Opened" } else { "Open" }
                                                }
                                                if adoptable {
                                                    button {
                                                        class: "btn-ghost adopt-toggle",
                                                        onclick: move |_| on_toggle.call(toggle_key.clone()),
                                                        if adopted { "Skip" } else { "Adopt" }
                                                    }
                                                }
                                                if opened {
                                                    div { class: "adopt-detail",
                                                        p { class: "adopt-detail-line",
                                                            "Writes \u{201c}{c.label}\u{201d} into {c.repo}'s AGENTS.md as this project's position on {c.rule_id}."
                                                        }
                                                        if let Some(reason) = c.blocked_reason() {
                                                            p { class: "adopt-detail-blocked", "{reason}" }
                                                        }
                                                        if c.deviating_sites > 0 {
                                                            p { class: "adopt-detail-line",
                                                                "{c.deviating_sites} site(s) do not follow it today. Those are Queue rows under {c.rule_id}; dispositioning them is separate work and does not gate this adoption."
                                                            }
                                                            {
                                                                let jump_rule = c.rule_id.clone();
                                                                let n = c.deviating_sites;
                                                                rsx! {
                                                                    button {
                                                                        class: "btn-ghost adopt-jump",
                                                                        onclick: move |_| on_review_exceptions.call(jump_rule.clone()),
                                                                        "Review the {n} exceptions in the Queue"
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            if shown.is_empty() {
                                p { class: "kind-panel-empty", "Nothing unopened in {repo}." }
                            }
                            AdoptCommitBar {
                                plan,
                                busy,
                                on_commit: {
                                    let repo = repo.clone();
                                    Callback::new(move |()| on_commit.call(repo.clone()))
                                },
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The footer under one destination's decision list: what a commit would write, and the button
/// that raises the gate.
///
/// Separate from the dialog on purpose. This bar states the counts WITHOUT the typed
/// confirmation, so the curator can read "27 to write, 3 skipped, 25 of them never opened"
/// before committing to the dialog at all — and can go open some of them instead.
///
/// Props-only.
#[component]
pub(in crate::cockpit) fn AdoptCommitBar(
    /// What a commit for this destination would do.
    plan: CommitPlan,
    /// A commit is in flight.
    #[props(default)]
    busy: bool,
    /// Raise the commit dialog.
    on_commit: Callback<()>,
) -> Element {
    let n = plan.adopted_count();
    let unopened = plan.unopened_count();
    rsx! {
        div { class: "adopt-commit-bar",
            span { class: "adopt-commit-counts", "{plan.counts_line()}" }
            if unopened > 0 {
                span { class: "adopt-commit-unopened", "{unopened} not opened" }
            }
            button {
                class: "btn-primary adopt-commit-btn",
                disabled: n == 0 || busy,
                onclick: move |_| on_commit.call(()),
                if n == 0 {
                    "Nothing selected to write"
                } else {
                    "Write {n} into {plan.destination_repo}\u{2026}"
                }
            }
        }
    }
}

/// THE COMMIT DIALOG — the gate (owner's Q4, design §13; the reviewed layout mockups).
///
/// > "Yes it should be opt out by default, make sure it is thoroughly gated and not an
/// > accidental click."
///
/// Five things the ruling requires, each one visible in the rsx below:
///
/// 1. **The destination repository, named.** Not "the repo", not "this project".
/// 2. **The adopted count and the skipped count.**
/// 3. **How many were adopted by default and never opened** — the number that matters, and the
///    one opt-out makes necessary. [`CommitPlan::unopened_count`] is a count of a recorded
///    gesture, not a ratio or a guess.
/// 4. **The repository name TYPED, not a checkbox.** A checkbox is muscle memory at 1am; typing
///    the destination cannot be done without reading it. The predicate is core's
///    [`commit_gate`] and the button reads nothing else.
/// 5. **Three exits.** Cancel, Review the unopened set, Commit. The safer path is offered
///    rather than only implied — which is the whole reason the third exit exists.
///
/// The third exit is hidden when there is nothing unopened to review, per the standing
/// hide-dead-end-affordances rule: a button that filters to an empty list is the 403-button bug
/// in miniature.
///
/// Props-only. SSR renders every state: untyped, mismatched, armed, empty, busy.
#[component]
pub(in crate::cockpit) fn ConventionCommitDialog(
    /// Exactly what the commit will do.
    plan: CommitPlan,
    /// What the curator has typed into the destination field so far.
    typed: String,
    /// The commit is in flight; every exit is disabled rather than hidden.
    #[props(default)]
    busy: bool,
    /// The destination field changed.
    on_typed: Callback<String>,
    /// Exit 1 — close, write nothing.
    on_cancel: Callback<()>,
    /// Exit 2 — close, and filter the panel to the rules this commit would write unopened.
    on_review_unopened: Callback<()>,
    /// Exit 3 — write. Only ever called when [`commit_gate`] is armed; the handler re-checks.
    on_commit: Callback<()>,
) -> Element {
    let gate = commit_gate(&plan, &typed);
    let armed = gate.is_armed();
    let unopened = plan.unopened_count();
    let destination = plan.destination_repo.clone();
    rsx! {
        div { class: "rule-modal-overlay adopt-dialog-overlay",
            div {
                class: "rule-modal adopt-dialog",
                role: "dialog",
                "aria-modal": "true",
                onclick: move |e| e.stop_propagation(),
                div { class: "rule-modal-head",
                    span { class: "rule-modal-id", "Write to {destination}" }
                }
                p { class: "adopt-dialog-headline", "{plan.headline()}" }
                p { class: "adopt-dialog-dest",
                    "Destination repository: "
                    span { class: "adopt-dialog-dest-name", "{destination}" }
                }
                p { class: "adopt-dialog-counts", "{plan.counts_line()}" }
                if let Some(warning) = plan.unopened_warning() {
                    p { class: "adopt-dialog-warning", "{warning}" }
                }
                ul { class: "adopt-dialog-list",
                    for rule in plan.adopted.iter() {
                        li { key: "{rule.rule_id}", class: "adopt-dialog-item",
                            span { class: "adopt-dialog-item-rule", "{rule.rule_id}" }
                            span { class: "adopt-dialog-item-label", "{rule.label}" }
                            if !rule.opened {
                                span { class: "adopt-unopened-flag", "not opened" }
                            }
                        }
                    }
                }
                label { class: "adopt-dialog-confirm-label", r#for: "adopt-dialog-confirm",
                    "Type {destination} to confirm the destination."
                }
                input {
                    id: "adopt-dialog-confirm",
                    class: "adopt-dialog-confirm-input",
                    r#type: "text",
                    autofocus: true,
                    autocomplete: "off",
                    placeholder: "{destination}",
                    value: "{typed}",
                    disabled: busy,
                    oninput: move |e| on_typed.call(e.value()),
                }
                if let Some(hint) = gate.hint(&destination) {
                    p { class: "adopt-dialog-hint", "{hint}" }
                }
                div { class: "adopt-dialog-exits",
                    button {
                        class: "btn-ghost adopt-dialog-cancel",
                        disabled: busy,
                        onclick: move |_| on_cancel.call(()),
                        "Cancel"
                    }
                    if unopened > 0 {
                        button {
                            class: "btn-ghost adopt-dialog-review",
                            disabled: busy,
                            onclick: move |_| on_review_unopened.call(()),
                            "Review the {unopened} not opened"
                        }
                    }
                    button {
                        class: if armed { "btn-primary adopt-dialog-commit armed" } else { "btn-primary adopt-dialog-commit shut" },
                        // The ONLY predicate. `commit_gate` is the same function the hint
                        // above reads, so a dialog that says "type the name" while offering a
                        // live button is unreachable.
                        disabled: !armed || busy,
                        onclick: move |_| {
                            if armed {
                                on_commit.call(());
                            }
                        },
                        if busy { "Writing\u{2026}" } else { "Commit" }
                    }
                }
            }
        }
    }
}

/// One `NoConvention` option pick, as the panel raises it.
#[derive(Clone, PartialEq, Debug)]
pub(in crate::cockpit) struct RecommendationPick {
    /// The rule's [`adoption_key`].
    pub key: String,
    /// The authored option to recommend, or `None` for "leave as reported".
    pub option_id: Option<String>,
}

/// NO CONVENTION panel — `NoConvention` rows: the codebase is inconsistent with no clear
/// majority. Exactly one row per rule (A5.3), listing the options actually observed with their
/// site counts where the producer could count them.
///
/// ## Picking an option is a GOVERNANCE ACT, and the panel says so
///
/// The owner's Q3 ruling:
///
/// > a `NoConvention` option pick IS client-facing. It feeds the derived decision set, so it is
/// > a governance act and must say so in the UI. It therefore inherits Q4's gating: the same
/// > typed-destination confirm, the same honest count of what was not opened.
///
/// So a pick here produces the SAME [`AdoptedRule`] an adoption produces, travels the same
/// [`CommitPlan`] and passes the same [`ConventionCommitDialog`]. The one difference is the
/// default, and it runs the other way: **"Leave as reported" is pre-selected.** Nothing is
/// written unless the curator chooses something, because unlike a convention the audit did not
/// derive an answer here — it found a split, and inventing a default would be the UI picking
/// the client's standard for them.
///
/// NO SEVERITY, here or anywhere informational (design §9.4).
#[component]
pub(in crate::cockpit) fn NoConventionPanel(
    findings: Vec<FindingView>,
    /// Open the RULE HUB on a listed rule. See [`ConventionsPanel`].
    #[props(default)]
    on_open_hub: Callback<RuleHubRequest>,
    /// The curator's picks and openness record. Default = everything left as reported.
    #[props(default)]
    model: AdoptionModel,
    /// A pick changed.
    #[props(default)]
    on_recommend: Callback<RecommendationPick>,
    /// The curator OPENED this rule's detail. The ONE writer of the openness record.
    #[props(default)]
    on_open: Callback<String>,
    /// Raise the commit dialog for one destination repository.
    #[props(default)]
    on_commit: Callback<String>,
    /// A commit is in flight.
    #[props(default)]
    busy: bool,
) -> Element {
    let all = split_rules(&findings);
    if all.is_empty() {
        return rsx! {
            p { class: "kind-panel-empty", "Every assessed rule reached a convention \u{2014} nothing split." }
        };
    }
    let mut by_repo: std::collections::BTreeMap<String, Vec<SplitRule>> = Default::default();
    for r in &all {
        by_repo.entry(r.repo.clone()).or_default().push(r.clone());
    }
    // The prose the detail row needs when the producer enumerated nothing. Keyed the same way
    // the rules are, so a row with no options still shows its own honest explanation.
    let detail_of: std::collections::BTreeMap<String, String> = findings
        .iter()
        .filter(|f| {
            matches!(
                f.kind,
                Some(camerata_api_types::audit::FindingKind::NoConvention(_))
            )
        })
        .map(|f| (adoption_key(&f.repo, &f.rule_id), f.detail.clone()))
        .collect();
    rsx! {
        div { class: "kind-panel",
            p { class: "kind-panel-sub",
                "Rules where the codebase is split with no clear majority. Recommending an option is a CLIENT-FACING governance act, not a note to yourself: it is written into that repository's AGENTS.md and governs how future AI agents work on the code. The default is to leave the split as reported and write nothing."
            }
            for (repo , rows) in by_repo.iter() {
                {
                    let plan = CommitPlan::recommendations(&all, &model, repo);
                    rsx! {
                        div { key: "{repo}", class: "kind-panel-group adopt-group",
                            p { class: "kind-panel-group-h", "{repo}" }
                            ul { class: "kind-panel-list",
                                for r in rows.iter() {
                                    {
                                        let key = r.key();
                                        let picked = model.recommendation(&key).map(str::to_string);
                                        let opened = model.was_opened(&key);
                                        let req = RuleHubRequest {
                                            repo: r.repo.clone(),
                                            rule_id: r.rule_id.clone(),
                                        };
                                        let open_key = key.clone();
                                        let pick_key = key.clone();
                                        let options = r.options.clone();
                                        let summary: Vec<String> = r
                                            .options
                                            .iter()
                                            .filter_map(|(id, _, _)| r.option_line(id))
                                            .collect();
                                        let detail = detail_of.get(&key).cloned().unwrap_or_default();
                                        rsx! {
                                            li {
                                                key: "{key}",
                                                class: if picked.is_some() { "kind-panel-row adopt-row adopted" } else { "kind-panel-row adopt-row skipped" },
                                                span {
                                                    class: if picked.is_some() { "adopt-state on" } else { "adopt-state off" },
                                                    if picked.is_some() { "Recommend" } else { "Leave as reported" }
                                                }
                                                button {
                                                    class: "rule-hub-link kind-panel-rule",
                                                    title: "Everything this scan knows about {r.rule_id}.",
                                                    onclick: move |_| on_open_hub.call(req.clone()),
                                                    "{r.rule_id}"
                                                }
                                                if summary.is_empty() {
                                                    span { class: "kind-panel-label", "{detail}" }
                                                } else {
                                                    span { class: "kind-panel-label", "{summary.join(\" \u{00b7} \")}" }
                                                }
                                                if picked.is_some() && !opened {
                                                    span { class: "adopt-unopened-flag", title: "You have not opened this one.", "not opened" }
                                                }
                                                span { class: "kind-panel-repo", "{r.repo}" }
                                                button {
                                                    class: "btn-ghost adopt-open",
                                                    onclick: move |_| on_open.call(open_key.clone()),
                                                    if opened { "Opened" } else { "Open" }
                                                }
                                                select {
                                                    class: "adopt-pick",
                                                    // "Leave as reported" is the first entry AND the
                                                    // default, so the keyboard-first gesture on an
                                                    // untouched row changes nothing.
                                                    value: picked.clone().unwrap_or_default(),
                                                    onchange: move |e| {
                                                        let v = e.value();
                                                        on_recommend
                                                            .call(RecommendationPick {
                                                                key: pick_key.clone(),
                                                                option_id: if v.is_empty() { None } else { Some(v) },
                                                            });
                                                    },
                                                    option { value: "", "Leave as reported" }
                                                    for (id , label , sites) in options.iter() {
                                                        option { key: "{id}", value: "{id}",
                                                            if let Some(n) = sites {
                                                                "Recommend: {label} ({n} sites)"
                                                            } else {
                                                                "Recommend: {label}"
                                                            }
                                                        }
                                                    }
                                                }
                                                if opened {
                                                    div { class: "adopt-detail",
                                                        p { class: "adopt-detail-line",
                                                            "Nothing is written for this rule unless you recommend an option. A recommendation becomes {r.repo}'s stated position on {r.rule_id} in its AGENTS.md."
                                                        }
                                                        if !detail.is_empty() {
                                                            p { class: "adopt-detail-line", "{detail}" }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            AdoptCommitBar {
                                plan,
                                busy,
                                on_commit: {
                                    let repo = repo.clone();
                                    Callback::new(move |()| on_commit.call(repo.clone()))
                                },
                            }
                        }
                    }
                }
            }
        }
    }
}

/// NOT ASSESSED panel — `NotAssessed` rows: the rule was armed but could not be evaluated.
/// Honesty rows, grouped by the structured [`NotAssessedReason`] vocabulary.
///
/// NO buttons, now or later, until a per-rule re-run mechanism actually exists. A disabled
/// "re-run" would be the 403-button bug the standing hide-dead-end-affordances rule forbids.
#[component]
pub(in crate::cockpit) fn NotAssessedPanel(
    findings: Vec<FindingView>,
    /// Open the RULE HUB on a listed rule. See [`ConventionsPanel`].
    #[props(default)]
    on_open_hub: Callback<RuleHubRequest>,
) -> Element {
    use camerata_api_types::audit::FindingKind;
    if findings.is_empty() {
        return rsx! {
            p { class: "kind-panel-empty", "Every armed rule was assessed." }
        };
    }
    // Group by the structured reason (a closed vocabulary, so grouping is structural, not
    // prose-matched). BTreeMap for a stable order across renders.
    let mut by_reason: std::collections::BTreeMap<&'static str, Vec<FindingView>> =
        Default::default();
    for f in &findings {
        let reason = match &f.kind {
            Some(FindingKind::NotAssessed(na)) => na.reason.as_str(),
            _ => "unknown",
        };
        by_reason.entry(reason).or_default().push(f.clone());
    }
    rsx! {
        div { class: "kind-panel",
            p { class: "kind-panel-sub",
                "Rules that were armed but could not be evaluated. These are honesty rows, not findings \u{2014} the scan cannot vouch for them either way."
            }
            for (reason , rows) in by_reason.iter() {
                div { key: "{reason}", class: "kind-panel-group",
                    p { class: "kind-panel-group-h", "{reason} \u{00b7} {rows.len()}" }
                    ul { class: "kind-panel-list",
                        for f in rows.iter() {
                            {
                                let detail = match &f.kind {
                                    Some(FindingKind::NotAssessed(na)) => na.reason_detail.clone(),
                                    _ => String::new(),
                                };
                                rsx! {
                                    li { key: "{f.repo}\u{0}{f.rule_id}", class: "kind-panel-row",
                                        {
                                            let req = RuleHubRequest {
                                                repo: f.repo.clone(),
                                                rule_id: f.rule_id.clone(),
                                            };
                                            rsx! {
                                                button {
                                                    class: "rule-hub-link kind-panel-rule",
                                                    title: "Everything this scan knows about {f.rule_id}.",
                                                    onclick: move |_| on_open_hub.call(req.clone()),
                                                    "{f.rule_id}"
                                                }
                                            }
                                        }
                                        span { class: "kind-panel-label", "{detail}" }
                                        span { class: "kind-panel-repo", "{f.repo}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The read-only "Patterns across services" tab: the rules the services of a multi-repo project
/// differ on, one grid per rule. An observation, not a finding: nothing here is triaged, rated
/// or counted, and a service where a rule does not apply, or was not reviewed, is shown as such
/// and never as clean. Mirrors the report's section (`camerata_ui_core::patterns`).
#[component]
pub(in crate::cockpit) fn PatternsPanel(
    view: camerata_ui_core::patterns::PatternsView,
    titles: Option<std::collections::HashMap<String, String>>,
) -> Element {
    // The same rows the report prints: under the rule's title, and without a row whose rule is
    // not in the loaded library.
    let shown = view.rows_with_titles(titles.as_ref());
    if shown.is_empty() {
        return rsx! {
            p { class: "kind-panel-empty",
                "No rule was found violated in one service and clean in another."
            }
        };
    }
    rsx! {
        div { class: "kind-panel patterns-panel",
            p { class: "kind-panel-sub",
                "Where the services of this project differ on a rule. An observation, not a finding: it carries no severity and is in no count. A service where a rule does not apply, or was not reviewed, is shown as such and is never counted as clean."
            }
            for (row , title) in shown.iter() {
                div { key: "{row.rule_id}", class: "kind-panel-group",
                    p { class: "kind-panel-group-h", "{title}" }
                    p { class: "kind-panel-sub",
                        "{camerata_ui_core::patterns::pattern_sentence(title)}"
                    }
                    ul { class: "kind-panel-list",
                        for cell in row.cells.iter() {
                            li { key: "{cell.repo}", class: "kind-panel-row",
                                span { class: "kind-panel-repo", "{cell.repo}" }
                                span { class: "kind-panel-label", "{cell.label()}" }
                            }
                        }
                    }
                }
            }
            for theme in view.themes.iter() {
                div { key: "{theme.name}", class: "kind-panel-group",
                    p { class: "kind-panel-group-h", "{theme.name}" }
                    p { class: "kind-panel-sub", "{theme.note}" }
                }
            }
        }
    }
}
