//! The decision card for an AI-proposed `Pending` decision (gap 9).
//!
//! The investigation agent files each decision through the gateway's `propose_decision` tool;
//! the server stores it as a `Pending` record whose rationale starts `Recommended: {option}.`
//! and whose `alternatives_considered` holds the other option labels (the format lives in
//! `camerata_worktracker::investigation`, shared with the server, so both sides agree).
//! This card shows the options as a radio list with the recommended one preselected and lets
//! the architect approve it, approve another option, or reject it with a reason. Every button
//! posts the WHOLE decision list through [`post_decisions`], like the rest of the panel.
//!
//! The decision logic is pure functions over [`DecisionRecordView`] so it is unit-tested
//! without a VirtualDom; the component only wires it to the buttons.

use camerata_worktracker::investigation::{chosen_rationale, parse_recommended_rationale};
use dioxus::prelude::*;

use super::uow::{post_decisions, DecisionOutcomeView, DecisionRecordView, RevisionProvenanceView};

/// What the card shows for one proposed decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ProposedDecisionVm {
    /// The option the agent recommends (preselected).
    pub recommended: String,
    /// Every option, the recommended one first.
    pub options: Vec<String>,
    /// The agent's reasoning, without the `Recommended:` prefix.
    pub reasoning: String,
    /// One-line description per option, in `options` order; empty strings when unknown.
    pub descriptions: Vec<String>,
}

/// The card model for `d`, or `None` when `d` is not an AI proposal waiting on the architect:
/// it must be `Pending` and carry the `Recommended: {option}.` rationale. A hand-typed or
/// already-ruled decision gets the ordinary row instead.
pub(super) fn proposed_decision_vm(d: &DecisionRecordView) -> Option<ProposedDecisionVm> {
    if d.outcome != DecisionOutcomeView::Pending {
        return None;
    }
    let (recommended, reasoning) = parse_recommended_rationale(&d.rationale)?;
    let mut options = vec![recommended.clone()];
    for alt in &d.alternatives_considered {
        let alt = alt.trim();
        if !alt.is_empty() && !options.iter().any(|o| o.eq_ignore_ascii_case(alt)) {
            options.push(alt.to_string());
        }
    }
    // Descriptions line up with [recommended] + alternatives_considered. Use them only when
    // that alignment is certain (no option was skipped above and the counts match).
    let aligned = d.option_descriptions.len() == options.len()
        && options.len() == 1 + d.alternatives_considered.len();
    let descriptions = if aligned {
        d.option_descriptions
            .iter()
            .map(|x| x.trim().to_string())
            .collect()
    } else {
        vec![String::new(); options.len()]
    };
    Some(ProposedDecisionVm {
        recommended,
        options,
        reasoning,
        descriptions,
    })
}

fn user_provenance(now_rfc3339: &str) -> RevisionProvenanceView {
    RevisionProvenanceView {
        actor: "user".to_string(),
        at: now_rfc3339.to_string(),
    }
}

/// The architect accepts the recommended option: `Approved`, provenance the user at `now`.
/// The record otherwise stays as the agent wrote it.
pub(super) fn approve_recommended(d: &DecisionRecordView, now_rfc3339: &str) -> DecisionRecordView {
    let mut r = d.clone();
    r.outcome = DecisionOutcomeView::Approved;
    r.provenance = user_provenance(now_rfc3339);
    r
}

/// What the "Approve" button does: approve the recommended option, and only when the recommended
/// option is the one selected on screen. `None` when another radio is picked, so "Approve" can
/// never approve something other than what the card shows (use [`approve_other`] for that).
pub(super) fn approve_selected(
    d: &DecisionRecordView,
    selected: &str,
    now_rfc3339: &str,
) -> Option<DecisionRecordView> {
    let vm = proposed_decision_vm(d)?;
    if !vm.recommended.eq_ignore_ascii_case(selected.trim()) {
        return None;
    }
    Some(approve_recommended(d, now_rfc3339))
}

/// The architect picks a different option: `Approved` with the rationale
/// `Chosen: {option}. {architect text}` and every other option (the old recommendation
/// included) listed as an alternative. `None` when `option` is the recommended one (use
/// [`approve_recommended`]) or is not one of this decision's options.
pub(super) fn approve_other(
    d: &DecisionRecordView,
    option: &str,
    architect_text: &str,
    now_rfc3339: &str,
) -> Option<DecisionRecordView> {
    let vm = proposed_decision_vm(d)?;
    let chosen_at = vm
        .options
        .iter()
        .position(|o| o.eq_ignore_ascii_case(option.trim()))?;
    let chosen = vm.options[chosen_at].clone();
    if chosen.eq_ignore_ascii_case(&vm.recommended) {
        return None;
    }
    let mut r = d.clone();
    r.outcome = DecisionOutcomeView::Approved;
    r.rationale = chosen_rationale(&chosen, architect_text);
    // Keep the descriptions lined up: the chosen option first, then the rest.
    if vm.descriptions.iter().any(|x| !x.is_empty()) {
        let mut descs = vec![vm.descriptions[chosen_at].clone()];
        descs.extend(
            vm.descriptions
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != chosen_at)
                .map(|(_, x)| x.clone()),
        );
        r.option_descriptions = descs;
    } else {
        r.option_descriptions = Vec::new();
    }
    r.alternatives_considered = vm.options.into_iter().filter(|o| *o != chosen).collect();
    r.provenance = user_provenance(now_rfc3339);
    Some(r)
}

/// The architect rejects the decision. A rejection must say why (the agent reads the reason
/// when it re-investigates), so a blank reason is refused.
pub(super) fn reject(
    d: &DecisionRecordView,
    reason: &str,
    now_rfc3339: &str,
) -> Result<DecisionRecordView, &'static str> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err("Say why this decision is rejected.");
    }
    let mut r = d.clone();
    r.outcome = DecisionOutcomeView::Rejected {
        reason: reason.to_string(),
    };
    r.provenance = user_provenance(now_rfc3339);
    Ok(r)
}

/// The signals every action needs once it posts: the panel's busy flag, its refresh tick
/// and the toast list. Signals are `Copy`, so this is too.
#[derive(Clone, Copy)]
struct Wiring {
    busy: Signal<bool>,
    local_refresh: Signal<u32>,
    toasts: Signal<Vec<crate::toast::Toast>>,
}

/// Replace `decisions[index]` with `record` and post the whole list. On success bump the
/// panel's refresh tick; on failure toast `failure`.
fn submit(
    story_id: String,
    decisions: &[DecisionRecordView],
    index: usize,
    record: DecisionRecordView,
    wiring: Wiring,
    failure: &'static str,
) {
    let Wiring {
        mut busy,
        mut local_refresh,
        toasts,
    } = wiring;
    let mut updated = decisions.to_vec();
    if index >= updated.len() {
        return;
    }
    updated[index] = record;
    busy.set(true);
    spawn(async move {
        if post_decisions(&story_id, &updated).await {
            local_refresh += 1;
        } else {
            crate::toast::push_toast(
                toasts,
                crate::toast::ToastKind::Warning,
                failure.to_string(),
            );
        }
        busy.set(false);
    });
}

/// The card for `decisions[index]`, which must satisfy [`proposed_decision_vm`] (it renders
/// nothing otherwise). `decisions` is the whole list, because every action posts the whole list.
#[component]
pub(super) fn ProposedDecisionCard(
    story_id: String,
    decisions: Vec<DecisionRecordView>,
    index: usize,
    busy: Signal<bool>,
    local_refresh: Signal<u32>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let wiring = Wiring {
        busy,
        local_refresh,
        toasts,
    };
    let d = decisions.get(index).cloned();
    let vm = d.as_ref().and_then(proposed_decision_vm);
    let recommended = vm
        .as_ref()
        .map(|v| v.recommended.clone())
        .unwrap_or_default();
    let mut selected = use_signal(|| recommended.clone());
    let mut architect_text = use_signal(String::new);
    let mut rejecting = use_signal(|| false);
    let mut reason = use_signal(String::new);

    let (Some(d), Some(vm)) = (d, vm) else {
        return rsx! {};
    };
    let other_picked = selected() != vm.recommended;

    rsx! {
        div { key: "{d.artifact_id}", class: "uow-decision-card uow-decision-proposed",
            div { class: "uow-decision-head",
                span { class: "uow-decision-label", "{d.label}" }
                span { class: "wi-state neutral", "Pending" }
            }
            if !d.question.is_empty() {
                p { class: "uow-decision-q", "Q: {d.question}" }
            }
            div { class: "uow-decision-options",
                for (n, opt) in vm.options.iter().enumerate() {
                    {
                        let is_recommended = *opt == vm.recommended;
                        let description = vm.descriptions.get(n).cloned().unwrap_or_default();
                        let checked = *opt == selected();
                        let name = format!("decision-{}", d.artifact_id);
                        let value = opt.clone();
                        let for_change = opt.clone();
                        rsx! {
                            label { key: "{value}", class: "uow-decision-option",
                                input {
                                    r#type: "radio",
                                    name: "{name}",
                                    value: "{value}",
                                    checked,
                                    disabled: busy(),
                                    onchange: move |_| selected.set(for_change.clone()),
                                }
                                span { " {value}" }
                                if is_recommended {
                                    span { class: "uow-decision-recommended", " (recommended)" }
                                }
                                if !description.is_empty() {
                                    span { class: "uow-decision-option-desc", ": {description}" }
                                }
                            }
                        }
                    }
                }
            }
            if !vm.reasoning.is_empty() {
                p { class: "uow-decision-rationale", "{vm.reasoning}" }
            }
            input {
                class: "uow-decision-input",
                r#type: "text",
                placeholder: "Your reason for choosing another option (optional)",
                value: "{architect_text}",
                oninput: move |e| architect_text.set(e.value()),
            }
            div { class: "uow-decision-actions",
                button {
                    class: "btn-run",
                    // "Approve" approves the recommended option, so it is off while another
                    // radio is selected; "Approve another option" is the button for that.
                    disabled: busy() || other_picked,
                    onclick: {
                        let d = d.clone();
                        let decisions = decisions.clone();
                        let sid = story_id.clone();
                        move |_| {
                            if let Some(record) =
                                approve_selected(&d, &selected(), &chrono::Utc::now().to_rfc3339())
                            {
                                submit(
                                    sid.clone(),
                                    &decisions,
                                    index,
                                    record,
                                    wiring,
                                    "Could not approve the decision.",
                                );
                            }
                        }
                    },
                    "Approve"
                }
                button {
                    class: "btn-secondary",
                    disabled: busy() || !other_picked,
                    onclick: {
                        let d = d.clone();
                        let decisions = decisions.clone();
                        let sid = story_id.clone();
                        move |_| {
                            let now = chrono::Utc::now().to_rfc3339();
                            if let Some(record) = approve_other(&d, &selected(), &architect_text(), &now) {
                                submit(
                                    sid.clone(),
                                    &decisions,
                                    index,
                                    record,
                                    wiring,
                                    "Could not approve the decision.",
                                );
                            }
                        }
                    },
                    "Approve another option"
                }
                button {
                    class: "btn-secondary",
                    disabled: busy(),
                    onclick: move |_| rejecting.set(!rejecting()),
                    "Reject"
                }
            }
            if rejecting() {
                div { class: "uow-decision-reject",
                    input {
                        class: "uow-decision-input",
                        r#type: "text",
                        placeholder: "Why is this decision rejected?",
                        value: "{reason}",
                        oninput: move |e| reason.set(e.value()),
                    }
                    button {
                        class: "btn-secondary",
                        disabled: busy() || reason().trim().is_empty(),
                        onclick: {
                            let d = d.clone();
                            let decisions = decisions.clone();
                            let sid = story_id.clone();
                            move |_| match reject(&d, &reason(), &chrono::Utc::now().to_rfc3339()) {
                                Ok(record) => submit(
                                    sid.clone(),
                                    &decisions,
                                    index,
                                    record,
                                    wiring,
                                    "Could not reject the decision.",
                                ),
                                Err(msg) => crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Warning,
                                    msg.to_string(),
                                ),
                            }
                        },
                        "Confirm rejection"
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_worktracker::investigation::recommended_rationale;

    const NOW: &str = "2026-10-28T12:00:00+00:00";

    fn pending_ai() -> DecisionRecordView {
        DecisionRecordView {
            artifact_id: "acme/api#4/decision/session-storage".to_string(),
            story_id: "acme/api#4".to_string(),
            label: "Session storage".to_string(),
            question: "Where do sessions live?".to_string(),
            // Built by the same function the server uses, so the two sides cannot drift.
            rationale: recommended_rationale("Cookies", "src/auth.rs already sets a cookie."),
            alternatives_considered: vec!["JWT".to_string(), "Server sessions".to_string()],
            outcome: DecisionOutcomeView::Pending,
            provenance: RevisionProvenanceView {
                actor: "ai".to_string(),
                at: "2026-10-28T11:00:00+00:00".to_string(),
            },
            option_descriptions: Vec::new(),
        }
    }

    #[test]
    fn the_card_model_reads_the_server_format_recommended_first() {
        let vm = proposed_decision_vm(&pending_ai()).expect("an AI proposal");
        assert_eq!(vm.recommended, "Cookies");
        assert_eq!(vm.options, vec!["Cookies", "JWT", "Server sessions"]);
        assert_eq!(vm.reasoning, "src/auth.rs already sets a cookie.");
    }

    #[test]
    fn only_a_pending_ai_proposal_gets_a_card() {
        // Approved, rejected and hand-typed decisions keep the ordinary row.
        assert!(proposed_decision_vm(&approve_recommended(&pending_ai(), NOW)).is_none());
        let rejected = reject(&pending_ai(), "no", NOW).unwrap();
        assert!(proposed_decision_vm(&rejected).is_none());
        let mut hand = pending_ai();
        hand.rationale = "Chose JWT: stateless.".to_string();
        assert!(proposed_decision_vm(&hand).is_none());
    }

    #[test]
    fn approve_posts_the_record_as_approved_with_user_provenance() {
        let d = pending_ai();
        let approved = approve_recommended(&d, NOW);
        assert_eq!(approved.outcome, DecisionOutcomeView::Approved);
        assert_eq!(approved.provenance.actor, "user");
        assert_eq!(approved.provenance.at, NOW);
        // Approving the recommendation changes nothing else.
        assert_eq!(approved.rationale, d.rationale);
        assert_eq!(approved.alternatives_considered, d.alternatives_considered);

        let other =
            approve_other(&d, "JWT", "We run stateless workers.", NOW).expect("a real option");
        assert_eq!(other.outcome, DecisionOutcomeView::Approved);
        assert_eq!(other.provenance.actor, "user");
        assert_eq!(other.rationale, "Chosen: JWT. We run stateless workers.");
        assert_eq!(
            other.alternatives_considered,
            vec!["Cookies", "Server sessions"]
        );
        // The recommended option and a made-up one are not "another option".
        assert!(approve_other(&d, "Cookies", "x", NOW).is_none());
        assert!(approve_other(&d, "Carrier pigeon", "x", NOW).is_none());
    }

    #[test]
    fn reject_requires_a_reason() {
        let d = pending_ai();
        assert_eq!(
            reject(&d, "", NOW),
            Err("Say why this decision is rejected.")
        );
        assert!(reject(&d, "   \n", NOW).is_err());
        let r = reject(&d, "  Wrong question  ", NOW).expect("a reason was given");
        assert_eq!(
            r.outcome,
            DecisionOutcomeView::Rejected {
                reason: "Wrong question".to_string()
            }
        );
        assert_eq!(r.provenance.actor, "user");
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn the_whole_list_is_posted_with_the_approved_record_in_place() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/uow/acme%2Fapi%234/decisions"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

        let other = DecisionRecordView {
            artifact_id: "acme/api#4/decision/other".to_string(),
            label: "Other".to_string(),
            outcome: DecisionOutcomeView::Approved,
            ..pending_ai()
        };
        let list = vec![other.clone(), approve_recommended(&pending_ai(), NOW)];
        assert!(post_decisions("acme/api#4", &list).await);

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: Vec<serde_json::Value> = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body.len(), 2, "the whole list, not just the changed record");
        assert_eq!(body[1]["outcome"]["state"], "approved");
        assert_eq!(body[1]["provenance"]["actor"], "user");
        assert_eq!(body[1]["rationale"], pending_ai().rationale);
    }

    #[test]
    fn a_pending_ai_decision_renders_its_options_with_the_recommended_one_preselected() {
        fn harness() -> Element {
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            let busy = use_signal(|| false);
            let local_refresh = use_signal(|| 0u32);
            rsx! {
                ProposedDecisionCard {
                    story_id: "acme/api#4".to_string(),
                    decisions: vec![pending_ai()],
                    index: 0usize,
                    busy,
                    local_refresh,
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);

        assert!(html.contains("Session storage"));
        assert!(html.contains("Where do sessions live?"));
        // Three options as radios; only the recommended one is checked, and it is labelled.
        assert_eq!(html.matches("type=\"radio\"").count(), 3, "{html}");
        assert_eq!(html.matches("checked").count(), 1, "{html}");
        assert!(html.contains("(recommended)"));
        let checked_at = html.find("checked").unwrap();
        let cookies_at = html.find("value=\"Cookies\"").unwrap();
        let jwt_at = html.find("value=\"JWT\"").unwrap();
        assert!(
            cookies_at < checked_at && checked_at < jwt_at,
            "the checked radio is the Cookies one: {html}"
        );
        // The agent's reasoning shows without the machine prefix.
        assert!(html.contains("src/auth.rs already sets a cookie."));
        assert!(!html.contains("Recommended: Cookies"));
        // The three buttons, with the second one disabled until another option is picked.
        assert!(html.contains(">Approve<"));
        assert!(html.contains("Approve another option"));
        assert!(html.contains(">Reject<"));
        let another = html.find("Approve another option").unwrap();
        let button_open = html[..another].rfind("<button").unwrap();
        assert!(
            html[button_open..another].contains("disabled"),
            "Approve another option starts disabled: {html}"
        );
    }

    #[test]
    fn approve_only_approves_the_option_the_card_shows_as_selected() {
        let d = pending_ai();
        // The recommended radio is selected: Approve approves it (case does not matter).
        let ok = approve_selected(&d, "cookies", NOW).expect("recommended is selected");
        assert_eq!(ok.outcome, DecisionOutcomeView::Approved);
        assert_eq!(ok.rationale, d.rationale);
        // Another radio is selected: Approve refuses, so it cannot approve the wrong option.
        assert!(approve_selected(&d, "JWT", NOW).is_none());
        assert!(approve_selected(&d, "", NOW).is_none());
    }

    fn with_descriptions() -> DecisionRecordView {
        let mut d = pending_ai();
        d.option_descriptions = vec![
            "server side".to_string(),
            "stateless tokens".to_string(),
            "in memory".to_string(),
        ];
        d
    }

    #[test]
    fn option_descriptions_show_on_the_card_and_survive_choosing_another_option() {
        let d = with_descriptions();
        let vm = proposed_decision_vm(&d).unwrap();
        assert_eq!(
            vm.descriptions,
            vec!["server side", "stateless tokens", "in memory"]
        );
        // Misaligned descriptions are ignored rather than shown against the wrong option.
        let mut bad = d.clone();
        bad.option_descriptions = vec!["only one".to_string()];
        assert_eq!(
            proposed_decision_vm(&bad).unwrap().descriptions,
            vec![String::new(); 3]
        );
        // Choosing another option keeps the lists lined up: chosen first, then the rest.
        let other = approve_other(&d, "Server sessions", "x", NOW).unwrap();
        assert_eq!(other.alternatives_considered, vec!["Cookies", "JWT"]);
        assert_eq!(
            other.option_descriptions,
            vec!["in memory", "server side", "stateless tokens"]
        );
        // The descriptions render next to their options.
        fn harness() -> Element {
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            let busy = use_signal(|| false);
            let local_refresh = use_signal(|| 0u32);
            rsx! {
                ProposedDecisionCard {
                    story_id: "acme/api#4".to_string(),
                    decisions: vec![with_descriptions()],
                    index: 0usize,
                    busy,
                    local_refresh,
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("server side"), "{html}");
        assert!(html.contains("stateless tokens"), "{html}");
        assert!(html.contains("in memory"), "{html}");
    }

    #[test]
    fn the_record_round_trips_option_descriptions_so_posting_the_list_keeps_them() {
        let d = with_descriptions();
        let json = serde_json::to_string(&d).unwrap();
        let back: DecisionRecordView = serde_json::from_str(&json).unwrap();
        assert_eq!(back.option_descriptions, d.option_descriptions);
        // A record from an older server (no field) still loads.
        let old = r#"{"artifact_id":"S/decision/x","story_id":"S","outcome":{"state":"pending"}}"#;
        assert!(serde_json::from_str::<DecisionRecordView>(old)
            .unwrap()
            .option_descriptions
            .is_empty());
    }
}
