use super::*;


/// One selectable option on a structured clarification (mirrors the server's
/// `ClarifyOption`): a label + a benefit/drawback description.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Debug, Default)]
pub(super) struct ClarifyOptionView {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub description: String,
}

/// One clarification (mirrors the server's `Clarification` for the fields the UI
/// needs). Free-text-only questions have an empty `options` and `allow_free_text`.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Debug, Default)]
pub(super) struct ClarificationView {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub story_id: String,
    #[serde(default)]
    pub question: String,
    #[serde(default)]
    pub addressee: String,
    #[serde(default)]
    pub options: Vec<ClarifyOptionView>,
    #[serde(default)]
    pub multi_select: bool,
    #[serde(default)]
    pub allow_free_text: bool,
    #[serde(default)]
    pub answer: Option<String>,
}

/// Fetch the OPEN clarifications for a story (`GET /api/stories/:id/clarifications`,
/// filtered to open ones client-side). Used to surface a story-authoring pause point.
pub(super) async fn fetch_clarifications_for_story(story_id: &str) -> Vec<ClarificationView> {
    let resp = match reqwest::get(format!(
        "{}/api/stories/{}/clarifications",
        crate::bff_base(),
        enc_seg(story_id)
    ))
    .await
    {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    resp.json::<Vec<ClarificationView>>().await.unwrap_or_default()
}

/// Fetch only the OPEN clarifications for a story (used as a story-authoring pause point).
pub(super) async fn fetch_open_clarifications_for_story(story_id: &str) -> Vec<ClarificationView> {
    fetch_clarifications_for_story(story_id)
        .await
        .into_iter()
        .filter(|c| c.answer.is_none())
        .collect()
}

/// Fetch ALL open clarifications across every story (`GET /api/clarifications`),
/// driving the NEEDS YOU queue.
pub(super) async fn fetch_all_open_clarifications() -> Vec<ClarificationView> {
    let resp = match reqwest::get(format!("{}/api/clarifications", crate::bff_base())).await {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    resp.json::<Vec<ClarificationView>>().await.unwrap_or_default()
}

/// Fetch all OPEN UoW (Governed Development) review escalations across every story — paused runs
/// awaiting an Approve / Amend / Reject. Filters the shared escalation feed to UoW subjects.
pub(super) async fn fetch_open_uow_escalations() -> Vec<crate::routines::EscalationView> {
    let resp = match reqwest::get(format!("{}/api/escalations?open=true", crate::bff_base())).await {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    resp.json::<Vec<crate::routines::EscalationView>>()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.subject_kind == "uow")
        .collect()
}

/// Resolve a UoW review with the human's free-text decision + the chosen action
/// (`"approve"` | `"amend"` | `"reject"`). Approve/Amend resume the paused run from its checkpoint;
/// Reject reverts the worktree and stops it. Returns the resolved escalation.
pub(super) async fn answer_uow_escalation(
    id: &str,
    answer: &str,
    action: &str,
) -> Option<crate::routines::EscalationView> {
    reqwest::Client::new()
        .post(format!("{}/api/escalations/{}/answer", crate::bff_base(), id))
        .json(&serde_json::json!({ "answer": answer, "action": action }))
        .send()
        .await
        .ok()?
        .json::<crate::routines::EscalationView>()
        .await
        .ok()
}

/// Discuss a UoW review with the lead-engineer agent (`POST /api/escalations/:id/chat`). Chatting
/// NEVER decides the review (only Approve/Amend/Reject does); it returns the escalation with the
/// human turn + the agent's reply appended, so the panel can show the thread.
pub(super) async fn chat_uow_escalation(
    id: &str,
    message: &str,
    model: &str,
) -> Option<crate::routines::EscalationView> {
    reqwest::Client::new()
        .post(format!("{}/api/escalations/{}/chat", crate::bff_base(), id))
        .json(&serde_json::json!({ "message": message, "model": model }))
        .send()
        .await
        .ok()?
        .json::<crate::routines::EscalationView>()
        .await
        .ok()
}

/// Resolve a UoW review (free fn so each action button can call it without moving a shared
/// closure). Fills a sensible default decision for a bare Approve/Reject; Amend requires text.
fn submit_uow_review(
    esc_id: String,
    decision_text: String,
    action: &'static str,
    mut submitting: Signal<bool>,
    on_resolved: EventHandler<()>,
) {
    if submitting() {
        return;
    }
    if action == "amend" && decision_text.trim().is_empty() {
        return;
    }
    submitting.set(true);
    spawn(async move {
        let _guard = crate::loading::LoadingGuard::new();
        let answer = if decision_text.trim().is_empty() {
            match action {
                "approve" => "Approved: proceed with the change as-is.".to_string(),
                "reject" => "Rejected: discard this change and stop.".to_string(),
                _ => decision_text,
            }
        } else {
            decision_text
        };
        let ok = answer_uow_escalation(&esc_id, &answer, action).await.is_some();
        submitting.set(false);
        if ok {
            on_resolved.call(());
        }
    });
}

/// The Governed Development REVIEW panel — the human-in-the-loop surface for a run paused at
/// `RunStatus::AwaitingReview` (e.g. the test-tamper guard). Shows what happened (the rule, what it
/// stopped for, the context, suggestions) and a free-text decision plus three actions: Approve
/// (resume as-is), Amend (resume with a correction), Reject (revert + stop). Resolving re-spawns or
/// stops the run server-side and drops this off the NEEDS YOU queue.
#[component]
pub(super) fn UowReviewPanel(
    esc: crate::routines::EscalationView,
    on_resolved: EventHandler<()>,
) -> Element {
    // Local copy so a chat reply updates the displayed thread immediately (the prop is immutable).
    let esc0 = esc.clone();
    let mut esc_view = use_signal(move || esc0);
    let mut decision = use_signal(String::new);
    let submitting = use_signal(|| false);
    let mut chat_input = use_signal(String::new);
    let mut chatting = use_signal(|| false);
    // The app-wide chat-assistant model (the lead engineer the review chats with).
    let chat_model = use_resource(|| super::fetch_app_chat_model());

    let e = esc_view();
    let id_approve = e.id.clone();
    let id_amend = e.id.clone();
    let id_reject = e.id.clone();
    let id_chat = e.id.clone();
    let model_default = chat_model
        .read()
        .clone()
        .flatten()
        .unwrap_or_else(|| "claude-sonnet-4-6".to_string());

    rsx! {
        div { class: "uow-review-card",
            div { class: "uow-review-head",
                span { class: "uow-review-badge", "NEEDS YOUR REVIEW" }
                span { class: "uow-review-rule", "{e.reason}" }
            }
            p { class: "uow-review-stopped", "{e.stopped_for}" }
            if !e.raw_context.is_empty() {
                p { class: "uow-review-context", "{e.raw_context}" }
            }
            if !e.suggestions.is_empty() {
                ul { class: "uow-review-suggestions",
                    for s in e.suggestions.iter() {
                        li { key: "{s}", "{s}" }
                    }
                }
            }
            // ── Discuss with the lead engineer (chatting NEVER decides) ────────────────
            if !e.conversation.is_empty() {
                div { class: "uow-review-chat-log",
                    for (i, m) in e.conversation.iter().enumerate() {
                        div {
                            key: "{i}",
                            class: if m.role == "assistant" { "uow-review-msg ai" } else { "uow-review-msg user" },
                            "{m.text}"
                        }
                    }
                }
            }
            div { class: "uow-review-chat",
                textarea {
                    class: "uow-review-chat-input",
                    rows: 2,
                    placeholder: "Ask the lead engineer about this (does NOT decide)\u{2026}",
                    value: "{chat_input}",
                    disabled: chatting(),
                    oninput: move |ev| chat_input.set(ev.value()),
                }
                button {
                    class: "btn-restart uow-review-ask",
                    disabled: chat_input().trim().is_empty() || chatting(),
                    onclick: move |_| {
                        let id = id_chat.clone();
                        let msg = chat_input();
                        let md = model_default.clone();
                        if msg.trim().is_empty() { return; }
                        chatting.set(true);
                        spawn(async move {
                            if let Some(updated) = chat_uow_escalation(&id, &msg, &md).await {
                                esc_view.set(updated);
                                chat_input.set(String::new());
                            }
                            chatting.set(false);
                        });
                    },
                    if chatting() { "Asking\u{2026}" } else { "Ask" }
                }
            }
            // ── Decide ─────────────────────────────────────────────────────────────────
            textarea {
                class: "uow-review-input",
                rows: 3,
                placeholder: "Your decision (optional for Approve/Reject; required to Amend)\u{2026}",
                value: "{decision}",
                disabled: submitting(),
                oninput: move |ev| decision.set(ev.value()),
            }
            div { class: "uow-review-actions",
                button {
                    class: "btn-run uow-review-approve",
                    disabled: submitting(),
                    onclick: move |_| submit_uow_review(id_approve.clone(), decision(), "approve", submitting, on_resolved),
                    "Approve & resume"
                }
                button {
                    class: "btn-run uow-review-amend",
                    disabled: submitting() || decision().trim().is_empty(),
                    onclick: move |_| submit_uow_review(id_amend.clone(), decision(), "amend", submitting, on_resolved),
                    "Amend & resume"
                }
                button {
                    class: "btn-stop uow-review-reject",
                    disabled: submitting(),
                    onclick: move |_| submit_uow_review(id_reject.clone(), decision(), "reject", submitting, on_resolved),
                    "Reject & revert"
                }
            }
            if submitting() {
                p { class: "uow-review-status", "Applying your decision\u{2026}" }
            }
        }
    }
}

/// Submit a structured answer to a clarification (`POST /api/clarifications/:cid/answer`).
/// Posts `{ selected, free_text, answered_by }`. Returns true on success.
pub(super) async fn answer_clarification(
    cid: &str,
    selected: Vec<String>,
    free_text: Option<String>,
) -> bool {
    reqwest::Client::new()
        .post(format!(
            "{}/api/clarifications/{}/answer",
            crate::bff_base(),
            enc_seg(cid)
        ))
        .json(&serde_json::json!({
            "selected": selected,
            "free_text": free_text,
            "answered_by": "you",
        }))
        .send()
        .await
        .ok()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// A reusable AskUserQuestion-style panel for one clarification: renders the question,
/// each option as a label + benefit/drawback description (radio for single-select,
/// checkboxes for multi-select), plus an "Other" free-text field when `allow_free_text`.
/// On submit it posts `{selected, free_text}` to the answer endpoint and calls `on_answered`.
///
/// Reused at every clarification point in the dev console (the story-authoring pause
/// point and the NEEDS YOU queue). A pure free-text question (empty options +
/// `allow_free_text`) renders just the "Other" box, so it stays back-compatible.
#[component]
pub(super) fn ClarifyQuestion(clar: ClarificationView, on_answered: EventHandler<()>) -> Element {
    // Selected option labels (one for single-select, many for multi-select).
    let mut selected = use_signal(Vec::<String>::new);
    let mut other = use_signal(String::new);
    let mut submitting = use_signal(|| false);

    let multi = clar.multi_select;
    let allow_free_text = clar.allow_free_text;
    let cid = clar.id.clone();

    // A submit is valid once there's at least one selection or non-empty free-text.
    let can_submit = !selected().is_empty() || !other().trim().is_empty();

    rsx! {
        div { class: "clarify-q-card",
            p { class: "clarify-q-question", "{clar.question}" }
            if !clar.addressee.is_empty() {
                p { class: "clarify-q-addressee", "for {clar.addressee}" }
            }
            div { class: "clarify-q-options",
                for opt in clar.options.iter() {
                    {
                        let label = opt.label.clone();
                        let checked = selected().contains(&label);
                        rsx! {
                            label {
                                key: "{label}",
                                class: if checked { "clarify-q-option on" } else { "clarify-q-option" },
                                input {
                                    r#type: if multi { "checkbox" } else { "radio" },
                                    name: "clarify-{cid}",
                                    checked,
                                    // Lock the options the instant a submit is in flight so the
                                    // answer can't be changed mid-submit (matches the button lock).
                                    disabled: submitting(),
                                    onchange: {
                                        let label = label.clone();
                                        move |_| {
                                            let mut cur = selected();
                                            if multi {
                                                if let Some(pos) = cur.iter().position(|x| x == &label) {
                                                    cur.remove(pos);
                                                } else {
                                                    cur.push(label.clone());
                                                }
                                            } else {
                                                cur = vec![label.clone()];
                                            }
                                            selected.set(cur);
                                        }
                                    },
                                }
                                span { class: "clarify-q-option-body",
                                    span { class: "clarify-q-option-label", "{opt.label}" }
                                    span { class: "clarify-q-option-desc", "{opt.description}" }
                                }
                            }
                        }
                    }
                }
            }
            if allow_free_text {
                div { class: "clarify-q-other",
                    label { class: "clarify-q-other-label",
                        if clar.options.is_empty() { "Your answer" } else { "Other" }
                    }
                    textarea {
                        class: "clarify-q-other-input",
                        rows: 2,
                        placeholder: "Type a different answer…",
                        value: "{other}",
                        // Lock the free-text input while the submit is in flight.
                        disabled: submitting(),
                        oninput: move |e| other.set(e.value()),
                    }
                }
            }
            div { class: "clarify-q-submit-row",
                button {
                    class: "btn-run",
                    disabled: submitting() || !can_submit,
                    onclick: {
                        let cid = cid.clone();
                        move |_| {
                            // Lock IMMEDIATELY on click (synchronous, before the await) so a
                            // double-click can't fire a second submit and the inputs lock at once.
                            if submitting() { return; }
                            let cid = cid.clone();
                            let sel = selected();
                            let ft = {
                                let t = other().trim().to_string();
                                if t.is_empty() { None } else { Some(t) }
                            };
                            let on_answered = on_answered;
                            submitting.set(true);
                            spawn(async move {
                                let _guard = crate::loading::LoadingGuard::new();
                                let ok = answer_clarification(&cid, sel, ft).await;
                                submitting.set(false);
                                if ok {
                                    on_answered.call(());
                                }
                            });
                        }
                    },
                    if submitting() { "Submitting…" } else { "Submit answer" }
                }
                // Submitting: the background Bombe machine activates via the
                // global loading guard held by the spawn task above.
            }
        }
    }
}

/// The NEEDS YOU queue: every OPEN clarification across every story, each rendered as
/// the AskUserQuestion-style `ClarifyQuestion` so it can be answered in place. These are
/// the resumable pause points — they persist server-side, so the user can leave and come
/// back to any unanswered question. Answering one re-fetches the queue (it drops off).
#[component]
pub(super) fn NeedsYouQueue() -> Element {
    let refresh = use_signal(|| 0u32);
    let open = use_resource(move || {
        let _dep = refresh();
        async move { fetch_all_open_clarifications().await }
    });
    // UoW (Governed Development) review escalations — paused runs awaiting Approve/Amend/Reject.
    let reviews = use_resource(move || {
        let _dep = refresh();
        async move { fetch_open_uow_escalations().await }
    });
    let open = open.read().clone().unwrap_or_default();
    let reviews = reviews.read().clone().unwrap_or_default();
    let total = open.len() + reviews.len();

    rsx! {
        div { class: "needs-you",
            p { class: "govdev-nav-label", "NEEDS YOU ({total})" }
            if total == 0 {
                p { class: "needs-empty", "Nothing waiting on you." }
            } else {
                div { class: "needs-list",
                    // Paused-run reviews first (a blocked run is the most urgent thing).
                    for esc in reviews.iter() {
                        UowReviewPanel {
                            key: "{esc.id}",
                            esc: esc.clone(),
                            on_resolved: move |_| {
                                let mut refresh = refresh;
                                refresh += 1;
                            },
                        }
                    }
                    for clar in open.iter() {
                        ClarifyQuestion {
                            key: "{clar.id}",
                            clar: clar.clone(),
                            on_answered: move |_| {
                                let mut refresh = refresh;
                                refresh += 1;
                            },
                        }
                    }
                }
            }
        }
    }
}

/// The live governed run: the real gate verdicts from the BFF run engine, streamed
/// in as the run walks to completion.
#[component]
pub(super) fn LiveRunPanel(run: RunView, uow_refresh: Signal<u32>) -> Element {
    let (status_label, status_cls) = run_status_badge(&run.status);
    let live = run.mode == "live";
    let mode_label = if live { "live fleet" } else { "scripted · token-free" };
    let sub = if live {
        "A real governed fleet (claude -p) under the gate. Stage and bounce events are reported as they happen."
    } else {
        "Token-free run: the agent is scripted, but the gate doing the deciding is the live one. Real deny/allow verdicts."
    };
    let run_id = run.id.clone();
    let cancellable = run_is_cancellable(&run.status, run.done);
    let show_stall = run_stall_banner_visible(run.stalled, run.done);
    let idle_label = format_idle(run.idle_ms);
    let failure_reason = run.failure_reason.clone().unwrap_or_default();
    let _toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();

    rsx! {
        div { class: "live-run",
            div { class: "live-run-head",
                span { class: "live-run-title", "Governed run" }
                span { class: "live-run-mode", "{mode_label}" }
                span { class: "live-run-status {status_cls}", "{status_label}" }
                // While running, the background Bombe machine is already active
                // via the poll_run_to_done loading guard — no inline spinner needed.
                // Stop button: always available while the run is in a running/non-terminal state.
                if cancellable {
                    button {
                        class: "btn-stop",
                        onclick: move |_| {
                            let rid = run_id.clone();
                            spawn(async move {
                                cancel_run(&rid).await;
                                // The poll loop will pick up the cancelled state on the next tick.
                            });
                        },
                        "\u{25a0} Stop"
                    }
                }
            }
            p { class: "panel-sub", "{sub}" }

            // Terminal states: failed and cancelled.
            if run.status == "failed" {
                div { class: "run-terminal-failed",
                    span { class: "run-terminal-label", "Run failed" }
                    if !failure_reason.is_empty() {
                        span { class: "run-terminal-reason", ": {failure_reason}" }
                    }
                }
            }
            if run.status == "cancelled" {
                div { class: "run-terminal-cancelled",
                    span { class: "run-terminal-label", "Cancelled" }
                }
            }

            // Stall warning: shown when the run has been idle longer than the threshold
            // but has NOT yet failed or been cancelled. Amber / warning treatment.
            if show_stall {
                div { class: "run-stall-warning",
                    div { class: "run-stall-warning-head",
                        span { class: "run-stall-icon", "\u{26a0}" }
                        span { class: "run-stall-title", "No progress for {idle_label} — possible stall" }
                        // Prominent Stop button inside the warning banner for quick action.
                        if cancellable {
                            button {
                                class: "btn-stop btn-stop-stall",
                                onclick: {
                                    let rid = run.id.clone();
                                    move |_| {
                                        let rid = rid.clone();
                                        spawn(async move { cancel_run(&rid).await; });
                                    }
                                },
                                "\u{25a0} Stop run"
                            }
                        }
                    }
                }
            }

            // Phase 3b: awaiting clarification.
            if run.status == "awaiting_clarification" {
                RunClarificationPrompt { story_id: run.story_id.clone(), uow_refresh }
            }

            p { class: "panel-sub live-events-caption",
                "Development activity — gate decisions, layer-2 checks, tier/delegation, and stage transitions as they happen."
            }
            div { class: "live-events",
                for ev in run.events.iter() {
                    {
                        let (vlabel, vcls) = live_event_style(&ev.layer, &ev.verdict);
                        rsx! {
                            div { class: "{vcls}",
                                div { class: "live-event-head",
                                    span { class: "live-event-verdict", "{vlabel}" }
                                    if let Some(rule) = ev.rule.clone() {
                                        span { class: "live-event-rule", "{rule}" }
                                    }
                                }
                                p { class: "live-event-detail", "{ev.detail}" }
                            }
                        }
                    }
                }
                if run.events.is_empty() {
                    if run.done {
                        p { class: "live-events-empty", "No activity recorded for this run." }
                    } else {
                        div { class: "live-events-empty",
                            p { "Spinning up the fleet\u{2026}" }
                        }
                    }
                }
            }

            if run.done {
                RunProvenancePanel { run_id: run.id.clone(), uow_refresh }
            }
        }
    }
}

/// Phase 3b: the inline "this run is waiting on you" prompt shown in [`LiveRunPanel`] when
/// a run is parked at `AwaitingClarification`. Fetches the story's OPEN clarifications and
/// renders each with the reused 3a [`ClarifyQuestion`]; answering one posts to the answer
/// endpoint (which triggers the server-side resume) and bumps `uow_refresh` so the panel
/// re-polls and the run picks back up.
#[component]
pub(super) fn RunClarificationPrompt(story_id: String, uow_refresh: Signal<u32>) -> Element {
    let mut local_refresh = use_signal(|| 0u32);
    let sid = story_id.clone();
    let open = use_resource(move || {
        let sid = sid.clone();
        let _dep = local_refresh();
        async move { fetch_open_clarifications_for_story(&sid).await }
    });
    let open = open.read().clone().unwrap_or_default();

    rsx! {
        div { class: "run-awaiting-clarify",
            p { class: "run-awaiting-clarify-h",
                "This run is waiting on you — the gated agent raised a question it can't decide itself."
            }
            if open.is_empty() {
                p { class: "needs-empty", "Loading the question…" }
            } else {
                for clar in open.iter() {
                    ClarifyQuestion {
                        key: "{clar.id}",
                        clar: clar.clone(),
                        on_answered: move |_| {
                            // Re-fetch this prompt (the answered question drops off) and bump
                            // the UoW refresh so the run panel re-polls and shows the resume.
                            local_refresh += 1;
                            uow_refresh += 1;
                        },
                    }
                }
            }
        }
    }
}

/// The provenance summary for a completed run plus the architect's sign-off action
/// (issue #21). Fetches `GET /api/runs/:id/provenance`; the sign-off button posts to
/// `POST /api/runs/:id/sign-off` and bumps `uow_refresh` so the UoW panel reflects it.
#[component]
pub(super) fn RunProvenancePanel(run_id: String, uow_refresh: Signal<u32>) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let rid = run_id.clone();
    let prov_res = use_resource(move || {
        let rid = rid.clone();
        async move { fetch_provenance(&rid).await }
    });
    let mut signing = use_signal(|| false);
    let mut signed = use_signal(|| false);
    // When sign-off is blocked by a Critical scoped-scan finding (issue #53) the server
    // 409s until a non-empty waiver reason is supplied. We surface a waiver box on the
    // first block and re-submit WITH the reason; empty stays blocked.
    let mut waive_required = use_signal(|| false);
    let mut waive_reason = use_signal(String::new);

    let prov = prov_res.read().clone().flatten();

    rsx! {
        div { class: "run-provenance",
            p { class: "run-provenance-h", "PROVENANCE" }
            match prov {
                Some(p) => rsx! {
                    div { class: "provenance-tallies",
                        span { class: "provenance-tally",
                            span { class: "provenance-num", "{p.allow_count}" }
                            " allowed"
                        }
                        span { class: "provenance-tally deny",
                            span { class: "provenance-num", "{p.deny_count}" }
                            " denied"
                        }
                        span { class: "provenance-tally bounce",
                            span { class: "provenance-num", "{p.total_bounces}" }
                            " total bounces"
                        }
                    }
                    if !p.rules_fired.is_empty() {
                        p { class: "provenance-fired",
                            "Rules that bounced a write: {p.rules_fired.join(\", \")}"
                        }
                    }
                    p { class: "provenance-inforce",
                        "Rules in force ({p.rules_in_force.len()}): {p.rules_in_force.join(\", \")}"
                    }
                },
                None => rsx! {
                    p { class: "provenance-empty", "Computing provenance…" }
                },
            }

            // The explicit sign-off action — never automatic.
            div { class: "run-signoff-row",
                if signed() {
                    span { class: "run-signoff-done", "✓ Signed off" }
                } else {
                    // When a Critical finding blocks sign-off, the architect must justify
                    // the waiver before re-submitting (the server rejects an empty reason).
                    if waive_required() {
                        textarea {
                            class: "uow-waive-reason",
                            placeholder: "A Critical security finding blocks sign-off. Explain why it is acceptable to ship…",
                            value: "{waive_reason}",
                            oninput: move |e| waive_reason.set(e.value()),
                        }
                    }
                    button {
                        class: "btn-run",
                        // While a waiver is required, only enable once a reason is typed.
                        disabled: signing() || (waive_required() && waive_reason().trim().is_empty()),
                        onclick: move |_| {
                            let rid = run_id.clone();
                            let toasts = toasts;
                            let mut uow_refresh = uow_refresh;
                            let waive = waive_reason().trim().to_string();
                            let waive_opt = if waive.is_empty() { None } else { Some(waive) };
                            signing.set(true);
                            spawn(async move {
                                match sign_off_run(&rid, "architect", None, waive_opt.as_deref()).await {
                                    SignOffOutcome::Ok(_) => {
                                        signing.set(false);
                                        signed.set(true);
                                        waive_required.set(false);
                                        uow_refresh += 1;
                                        crate::toast::push_toast(
                                            toasts,
                                            crate::toast::ToastKind::Info,
                                            "Run signed off.".to_string(),
                                        );
                                    }
                                    SignOffOutcome::Blocked(reason) => {
                                        signing.set(false);
                                        // Reveal the waiver box and surface the precise reason.
                                        waive_required.set(true);
                                        crate::toast::push_toast(
                                            toasts,
                                            crate::toast::ToastKind::Warning,
                                            reason,
                                        );
                                    }
                                    SignOffOutcome::Failed => {
                                        signing.set(false);
                                        crate::toast::push_toast(
                                            toasts,
                                            crate::toast::ToastKind::Warning,
                                            "Could not sign off the run.".to_string(),
                                        );
                                    }
                                }
                            });
                        },
                        if signing() {
                            "Signing off…"
                        } else if waive_required() {
                            "✓ Sign off with waiver"
                        } else {
                            "✓ Sign off this run"
                        }
                    }
                }
                span { class: "section-hint", "Camerata never auto-opens a PR or signs off. Review the provenance, then sign off explicitly." }
            }
        }
    }
}

/// The Unit of Work dev panel for a selected story.
///
/// Shows the dev-side projection alongside the story's tracker status:
/// - Dev status control (3-state segmented control: New / In progress / Done).
/// - Branch ref (if set, read-only here — auto-populated by the governed run).
/// - AI development history (HistoryEntry rows: ts · kind · text), read-only.
///
/// Fetch is keyed by `story_id` so switching stories reloads the UoW. A shared
/// `uow_refresh` tick lets the spine badges update after a status change.
///
/// NOTE: branch + history are designed to be auto-populated by the governed run
/// (Pillar 2). They are settable via the API endpoints; the UI shows them here.
/// A `<select>` of model options, generic over the bound signal. Renders nothing
/// until the model list has loaded. Used by every per-step run control.
///
/// Options are grouped by provider (`<optgroup>` — Claude first, then OpenRouter).
/// Each label carries badges: [FREE], [no-tools], [NNNk ctx]. When only Claude is
/// present (no OpenRouter key set) there is a single flat group.
#[component]
pub(super) fn ModelSelect(models: Option<AuditModelsResp>, selected: Signal<String>) -> Element {
    let mut selected = selected;
    rsx! {
        if let Some(m) = models {
            select {
                class: "run-model-select",
                value: "{selected}",
                onchange: move |e| selected.set(e.value()),
                for (group_label , opts) in m.grouped().into_iter() {
                    optgroup { label: "{group_label}",
                        for opt in opts.into_iter() {
                            option {
                                value: "{opt.id}",
                                selected: selected() == opt.id,
                                "{opt.label}"
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod render_tests {
    use super::{ClarificationView, ClarifyQuestion};
    use dioxus::prelude::*;

    // A root component that mounts the component under test. It runs INSIDE the VirtualDom runtime,
    // so the inner component's hooks (use_signal) + event-handler creation work.
    fn clarify_harness() -> Element {
        rsx! {
            ClarifyQuestion {
                clar: ClarificationView {
                    id: "c1".to_string(),
                    question: "Which storage backend?".to_string(),
                    ..Default::default()
                },
                on_answered: move |_| {},
            }
        }
    }

    // Tier-1 UI test: render a component to HTML headlessly (VirtualDom + dioxus-ssr) and assert its
    // STRUCTURE. No browser / wasm. Catches "the component renders the wrong shape / an element
    // vanished" bugs (e.g. the recurring model-selector-disappeared class). Static render only: it
    // asserts the presence of text/elements, not click behavior or async-loaded data.
    #[test]
    fn clarify_question_renders_question_and_submit() {
        let mut vdom = VirtualDom::new(clarify_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            html.contains("Which storage backend?"),
            "the question text renders; html=\n{html}"
        );
        assert!(
            html.to_lowercase().contains("submit"),
            "a submit affordance renders; html=\n{html}"
        );
    }

    // ── Tier-1 render: ModelSelect (props-only, no context) ────────────────────
    // The model selector has regressed away repeatedly; this guards that a populated
    // model list renders the <select> with its grouped <optgroup> + <option>s.
    fn model_select_harness() -> Element {
        // AuditModelsResp / AuditModelOption don't derive Default, but both derive
        // Deserialize — build the fixture from JSON (the same wire shape the BFF sends).
        let models: super::super::scan::AuditModelsResp = serde_json::from_value(serde_json::json!({
            "models": [
                { "label": "Claude Sonnet 4.6", "id": "claude-sonnet-4-6", "provider": "claude" },
                { "label": "DeepSeek R1", "id": "deepseek-r1", "provider": "openrouter" }
            ],
            "default": "claude-sonnet-4-6",
            "openrouter_fetched": true
        }))
        .expect("valid AuditModelsResp fixture");
        rsx! {
            ModelSelectHarnessInner { models }
        }
    }

    // Inner wrapper that owns the bound Signal<String> ModelSelect needs.
    #[component]
    fn ModelSelectHarnessInner(models: super::super::scan::AuditModelsResp) -> Element {
        let selected = use_signal(|| "claude-sonnet-4-6".to_string());
        rsx! {
            super::ModelSelect { models: Some(models), selected }
        }
    }

    #[test]
    fn model_select_renders_options_grouped_by_provider() {
        let mut vdom = VirtualDom::new(model_select_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("run-model-select"), "the <select> renders; html=\n{html}");
        assert!(html.contains("Claude (subscription)"), "claude optgroup; html=\n{html}");
        assert!(html.contains("OpenRouter"), "openrouter optgroup; html=\n{html}");
        assert!(html.contains("Claude Sonnet 4.6"), "claude option label; html=\n{html}");
        assert!(html.contains("DeepSeek R1"), "openrouter option label; html=\n{html}");
    }

    #[test]
    fn model_select_renders_nothing_when_models_absent() {
        // ModelSelect is `if let Some(m) = models { ... }` — None must render no <select>.
        fn none_harness() -> Element {
            let selected = use_signal(String::new);
            rsx! {
                super::ModelSelect { models: None, selected }
            }
        }
        let mut vdom = VirtualDom::new(none_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            !html.contains("run-model-select"),
            "no <select> when the model list hasn't loaded; html=\n{html}"
        );
    }

    // ── Tier-1 render: UowReviewPanel (the governed-development review surface) ──
    // Constructed via serde (EscalationView only derives Deserialize). The chat_model
    // use_resource is pending on first render, so it falls back to its default model —
    // that's fine, we assert the static review structure, not the async-loaded model.
    fn uow_panel_harness() -> Element {
        let esc: crate::routines::EscalationView = serde_json::from_value(serde_json::json!({
            "id": "esc-1",
            "routine_id": "r1",
            "routine_name": "dev",
            "subject_kind": "uow",
            "reason": "TEST-TAMPER-1",
            "stopped_for": "The agent edited a test assertion.",
            "suggestions": ["Restore the original assertion."],
            "raw_context": "diff context here",
            "status": "open",
            "created": "2026-06-30T00:00:00Z",
            "conversation": [
                { "role": "user", "text": "Why did this stop?" },
                { "role": "assistant", "text": "A test assertion was weakened." }
            ]
        }))
        .expect("valid EscalationView fixture");
        rsx! {
            super::UowReviewPanel { esc, on_resolved: move |_| {} }
        }
    }

    #[test]
    fn uow_review_panel_renders_reason_actions_and_thread() {
        let mut vdom = VirtualDom::new(uow_panel_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("NEEDS YOUR REVIEW"), "the review badge; html=\n{html}");
        assert!(html.contains("TEST-TAMPER-1"), "the rule/reason; html=\n{html}");
        assert!(
            html.contains("The agent edited a test assertion."),
            "the stopped-for line; html=\n{html}"
        );
        assert!(
            html.contains("Restore the original assertion."),
            "a suggestion renders; html=\n{html}"
        );
        assert!(html.contains("A test assertion was weakened."), "the chat thread; html=\n{html}");
        // SSR HTML-escapes '&' to '&#38;', so assert on the stable class names + the unescaped
        // leading word rather than the literal "Approve & resume".
        assert!(html.contains("uow-review-approve") && html.contains("Approve"), "approve action; html=\n{html}");
        assert!(html.contains("uow-review-reject") && html.contains("Reject"), "reject action; html=\n{html}");
    }

    // ── Tier-1 render: RunProvenancePanel (needs the toast context) ─────────────
    // It calls use_context::<Signal<Vec<Toast>>>() so the harness MUST provide it,
    // else the component panics. The provenance use_resource is pending on first
    // render, so it shows the "Computing provenance…" branch — we assert the static
    // surrounding structure (heading + sign-off button), not the loaded tallies.
    fn provenance_harness() -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        let uow_refresh = use_signal(|| 0u32);
        rsx! {
            super::RunProvenancePanel { run_id: "run-1".to_string(), uow_refresh }
        }
    }

    #[test]
    fn run_provenance_panel_renders_heading_and_signoff() {
        let mut vdom = VirtualDom::new(provenance_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("PROVENANCE"), "the provenance heading; html=\n{html}");
        assert!(
            html.contains("Sign off this run") || html.contains("Computing provenance"),
            "the sign-off affordance / pending branch renders; html=\n{html}"
        );
        assert!(
            html.contains("never auto-opens a PR"),
            "the explicit-sign-off hint renders; html=\n{html}"
        );
    }
}

#[cfg(test)]
mod tests {
    // `CAMERATA_BFF_URL` is a process-global env override (see crate::bff_base()). `cargo test` runs
    // tests on parallel threads, so two tests that set it could clobber each other. This mutex
    // serializes the env-mutating Tier-2 tests against each other; we recover from poisoning so one
    // failing test doesn't cascade-fail the rest. (We can't add serial_test without touching Cargo.toml.)
    static BFF_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn bff_env_guard() -> std::sync::MutexGuard<'static, ()> {
        BFF_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ── Tier-2: fetch_clarifications_for_story — GET the story's clarifications ──
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_clarifications_for_story_gets_and_parses() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env = bff_env_guard();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/stories/story-9/clarifications"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": "c1", "question": "Which backend?", "answer": null },
                { "id": "c2", "question": "Which queue?", "answer": "redis" }
            ])))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let clars = super::fetch_clarifications_for_story("story-9").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert_eq!(clars.len(), 2, "both clarifications parse");
        assert_eq!(clars[0].id, "c1");
        assert_eq!(clars[0].question, "Which backend?");
        assert!(clars[0].answer.is_none());
        assert_eq!(clars[1].answer.as_deref(), Some("redis"));
    }

    // ── Tier-2: fetch_open_clarifications_for_story — same GET, filters answered ──
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_open_clarifications_for_story_filters_answered() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env = bff_env_guard();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/stories/story-9/clarifications"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": "c1", "question": "Open one", "answer": null },
                { "id": "c2", "question": "Answered one", "answer": "done" }
            ])))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let open = super::fetch_open_clarifications_for_story("story-9").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert_eq!(open.len(), 1, "only the unanswered clarification survives the filter");
        assert_eq!(open[0].id, "c1");
    }

    // ── Tier-2: fetch_all_open_clarifications — GET the cross-story queue ────────
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_all_open_clarifications_gets_and_parses() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env = bff_env_guard();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/clarifications"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": "c1", "question": "Q1" },
                { "id": "c2", "question": "Q2" }
            ])))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let clars = super::fetch_all_open_clarifications().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert_eq!(clars.len(), 2);
        assert_eq!(clars[0].question, "Q1");
        assert_eq!(clars[1].id, "c2");
    }

    // ── Tier-2: fetch_open_uow_escalations — GET ?open=true, keep subject_kind=="uow" ──
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_open_uow_escalations_filters_to_uow_subjects() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env = bff_env_guard();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/escalations"))
            .and(query_param("open", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": "e1", "routine_id": "r1", "routine_name": "dev",
                    "subject_kind": "uow", "reason": "TEST-TAMPER-1",
                    "stopped_for": "edited a test", "status": "open",
                    "created": "2026-06-30T00:00:00Z"
                },
                {
                    "id": "e2", "routine_id": "r2", "routine_name": "nightly",
                    "subject_kind": "routine", "reason": "OTHER",
                    "stopped_for": "something else", "status": "open",
                    "created": "2026-06-30T00:00:00Z"
                }
            ])))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let uow = super::fetch_open_uow_escalations().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert_eq!(uow.len(), 1, "only the uow-subject escalation survives the filter");
        assert_eq!(uow[0].id, "e1");
        assert_eq!(uow[0].subject_kind, "uow");
    }

    // ── Tier-2: answer_uow_escalation — POST {answer, action}, parse the result ──
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn answer_uow_escalation_posts_answer_and_action() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env = bff_env_guard();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/escalations/esc-7/answer"))
            .and(body_json(serde_json::json!({
                "answer": "Approved: proceed.",
                "action": "approve"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "esc-7", "routine_id": "r1", "routine_name": "dev",
                "subject_kind": "uow", "reason": "TEST-TAMPER-1",
                "stopped_for": "edited a test", "status": "resolved",
                "created": "2026-06-30T00:00:00Z"
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let out = super::answer_uow_escalation("esc-7", "Approved: proceed.", "approve").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let out = out.expect("the resolved escalation parses");
        assert_eq!(out.id, "esc-7");
        assert_eq!(out.status, "resolved");
        // `.expect(1)` asserts (on server drop) the exact {answer, action} body was posted.
    }

    // ── Tier-2: chat_uow_escalation — POST {message, model}, parse the result ───
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn chat_uow_escalation_posts_message_and_model() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env = bff_env_guard();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/escalations/esc-3/chat"))
            .and(body_json(serde_json::json!({
                "message": "Why did this stop?",
                "model": "claude-sonnet-4-6"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "esc-3", "routine_id": "r1", "routine_name": "dev",
                "subject_kind": "uow", "reason": "TEST-TAMPER-1",
                "stopped_for": "edited a test", "status": "open",
                "created": "2026-06-30T00:00:00Z",
                "conversation": [
                    { "role": "user", "text": "Why did this stop?" },
                    { "role": "assistant", "text": "A test assertion was weakened." }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let out =
            super::chat_uow_escalation("esc-3", "Why did this stop?", "claude-sonnet-4-6").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let out = out.expect("the escalation with the appended turns parses");
        assert_eq!(out.conversation.len(), 2);
        assert_eq!(out.conversation[1].role, "assistant");
        // `.expect(1)` asserts the exact {message, model} body was posted (a wrong field is a real bug).
    }

    // ── Tier-2: answer_clarification — POST {selected, free_text, answered_by} ──
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn answer_clarification_posts_selected_and_free_text() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env = bff_env_guard();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/clarifications/clar-2/answer"))
            .and(body_json(serde_json::json!({
                "selected": ["Postgres"],
                "free_text": "lean toward managed",
                "answered_by": "you"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::answer_clarification(
            "clar-2",
            vec!["Postgres".to_string()],
            Some("lean toward managed".to_string()),
        )
        .await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok, "a 2xx answer is reported as success");
        // `.expect(1)` asserts the exact {selected, free_text, answered_by} body was posted.
    }

    // ── Tier-2: answer_clarification — a null free_text serializes to JSON null ─
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn answer_clarification_omits_free_text_as_null() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env = bff_env_guard();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/clarifications/clar-5/answer"))
            .and(body_json(serde_json::json!({
                "selected": ["Yes"],
                "free_text": null,
                "answered_by": "you"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::answer_clarification("clar-5", vec!["Yes".to_string()], None).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok, "a single-select answer with no free text posts free_text: null");
    }

    // ── Tier-2: answer_clarification — a non-2xx response is reported as false ──
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn answer_clarification_reports_failure_on_non_2xx() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env = bff_env_guard();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/clarifications/clar-9/answer"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::answer_clarification("clar-9", vec!["X".to_string()], None).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(!ok, "a 500 from the answer endpoint is reported as failure");
    }
}
