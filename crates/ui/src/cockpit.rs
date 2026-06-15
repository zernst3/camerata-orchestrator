//! The enterprise / architect surface: the single-pane cockpit.
//!
//! Where the consumer app-builder surface is a calm, guided, one-decision-per-screen
//! wizard (the human is led), the cockpit is a dense control surface (the human
//! steers a fleet). It is the UI realization of `docs/UI_DESIGN.md` section 2: three
//! panes on one screen, nothing opens a separate window.
//!
//! - LEFT: the story spine (every story + its lifecycle status) and a NEEDS YOU queue.
//! - CENTER: a stage that swaps by the selected story's status, with a live status
//!   strip showing the governed fleet and the gate activity.
//! - RIGHT: an inspector that binds to the selection (the gate's enforced rules).
//!
//! Wiring: the spine and the inspector rules are fetched from the BFF
//! (`camerata-server`) over HTTP (`/api/stories`, `/api/rules`), not read in-process,
//! the same client/server split that makes the server cloud-hostable. The fleet and
//! gate-activity panels are still representative; live execution + a status stream are
//! the next phase (the same path `worktracker-demo` / `po-demo` exercise).

use dioxus::prelude::*;

use camerata_worktracker::{CanonicalStory, FeatureStatus};

/// One enforced gate rule, as returned by the BFF `/api/rules` endpoint (GOV-1 is
/// filtered out server-side). The cockpit just renders what the BFF returns.
#[derive(Clone, PartialEq, serde::Deserialize)]
struct CockpitRule {
    id: String,
    statement: String,
}

/// Fetch the canonical story spine from the BFF.
async fn fetch_stories() -> Option<Vec<CanonicalStory>> {
    reqwest::get(format!("{}/api/stories", crate::BFF_URL))
        .await
        .ok()?
        .json::<Vec<CanonicalStory>>()
        .await
        .ok()
}

/// Fetch the gate's enforced rules from the BFF.
async fn fetch_rules() -> Option<Vec<CockpitRule>> {
    reqwest::get(format!("{}/api/rules", crate::BFF_URL))
        .await
        .ok()?
        .json::<Vec<CockpitRule>>()
        .await
        .ok()
}

/// A run as the BFF reports it (`GET /api/runs/:id`): status plus the REAL gate
/// verdicts produced so far.
#[derive(Clone, PartialEq, serde::Deserialize)]
struct RunView {
    story_id: String,
    status: String,
    events: Vec<RunGateEvent>,
    done: bool,
    #[serde(default)]
    mode: String,
}

/// One real gate verdict in a run.
#[derive(Clone, PartialEq, serde::Deserialize)]
struct RunGateEvent {
    verdict: String,
    rule: Option<String>,
    detail: String,
}

/// Start a governed run for a story; returns the run id.
async fn start_run(story_id: &str) -> Option<String> {
    let resp = reqwest::Client::new()
        .post(format!("{}/api/stories/{}/run", crate::BFF_URL, story_id))
        .send()
        .await
        .ok()?;
    let v: serde_json::Value = resp.json().await.ok()?;
    v.get("run_id")?.as_str().map(|s| s.to_string())
}

/// Fetch the current state of a run.
async fn fetch_run(run_id: &str) -> Option<RunView> {
    reqwest::get(format!("{}/api/runs/{}", crate::BFF_URL, run_id))
        .await
        .ok()?
        .json::<RunView>()
        .await
        .ok()
}

/// Map a run status string to a label + badge CSS modifier.
fn run_status_badge(status: &str) -> (&'static str, &'static str) {
    match status {
        "planned" => ("PLANNED", "neutral"),
        "executing" => ("EXECUTING", "active"),
        "gating" => ("GATING", "active"),
        "awaiting_qa" => ("AWAITING QA", "warn"),
        _ => ("RUNNING", "active"),
    }
}

/// A clarification as the BFF reports it (`/api/stories/:id/clarifications`).
#[derive(Clone, PartialEq, serde::Deserialize)]
struct ClarificationView {
    id: String,
    story_id: String,
    question: String,
    addressee: String,
    answer: Option<String>,
    answered_by: Option<String>,
}

/// Fetch all OPEN clarifications across stories (the NEEDS YOU queue).
async fn fetch_open_clarifications() -> Option<Vec<ClarificationView>> {
    reqwest::get(format!("{}/api/clarifications", crate::BFF_URL))
        .await
        .ok()?
        .json::<Vec<ClarificationView>>()
        .await
        .ok()
}

/// Fetch the clarifications on a story.
async fn fetch_clarifications(story_id: &str) -> Option<Vec<ClarificationView>> {
    reqwest::get(format!(
        "{}/api/stories/{}/clarifications",
        crate::BFF_URL,
        story_id
    ))
    .await
    .ok()?
    .json::<Vec<ClarificationView>>()
    .await
    .ok()
}

/// Post a clarifying question on a story, addressed to `addressee`.
async fn post_clarification(
    story_id: &str,
    question: &str,
    addressee: &str,
) -> Option<ClarificationView> {
    reqwest::Client::new()
        .post(format!(
            "{}/api/stories/{}/clarifications",
            crate::BFF_URL,
            story_id
        ))
        .json(&serde_json::json!({ "question": question, "addressee": addressee }))
        .send()
        .await
        .ok()?
        .json::<ClarificationView>()
        .await
        .ok()
}

/// Record the answer to a clarification.
async fn answer_clarification(cid: &str, answer: &str, answered_by: &str) -> Option<ClarificationView> {
    reqwest::Client::new()
        .post(format!("{}/api/clarifications/{}/answer", crate::BFF_URL, cid))
        .json(&serde_json::json!({ "answer": answer, "answered_by": answered_by }))
        .send()
        .await
        .ok()?
        .json::<ClarificationView>()
        .await
        .ok()
}

/// A proposed child story from decomposition (editable before commit). Serializes
/// back to the BFF on commit.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct ProposedChildView {
    kind: String,
    title: String,
    description: String,
}

/// Propose the component children for a parent (not yet created).
async fn fetch_proposal(story_id: &str) -> Option<Vec<ProposedChildView>> {
    reqwest::Client::new()
        .post(format!("{}/api/stories/{}/decompose", crate::BFF_URL, story_id))
        .send()
        .await
        .ok()?
        .json::<Vec<ProposedChildView>>()
        .await
        .ok()
}

/// Commit the edited children; returns the created child stories.
async fn commit_children(story_id: &str, children: &[ProposedChildView]) -> Option<Vec<CanonicalStory>> {
    reqwest::Client::new()
        .post(format!(
            "{}/api/stories/{}/decompose/commit",
            crate::BFF_URL,
            story_id
        ))
        .json(&serde_json::json!({ "children": children }))
        .send()
        .await
        .ok()?
        .json::<Vec<CanonicalStory>>()
        .await
        .ok()
}

/// The committed children of a parent.
async fn fetch_children(story_id: &str) -> Option<Vec<CanonicalStory>> {
    reqwest::get(format!("{}/api/stories/{}/children", crate::BFF_URL, story_id))
        .await
        .ok()?
        .json::<Vec<CanonicalStory>>()
        .await
        .ok()
}

/// One agent in the governed fleet, as the status strip renders it.
#[derive(Clone, PartialEq)]
struct FleetAgent {
    role: &'static str,
    /// Plain-language state for the strip.
    state: &'static str,
    /// CSS modifier: "gated" (passed), "exec" (running), "pending".
    state_class: &'static str,
}

/// The fleet for the active (Executing) story: Backend gated, Frontend running,
/// Integrate pending. Representative until the live execution stream lands (Phase 3).
fn seed_fleet() -> Vec<FleetAgent> {
    vec![
        FleetAgent {
            role: "Backend",
            state: "gated",
            state_class: "gated",
        },
        FleetAgent {
            role: "Frontend",
            state: "executing",
            state_class: "exec",
        },
        FleetAgent {
            role: "Integrate",
            state: "pending",
            state_class: "pending",
        },
    ]
}

/// Map a canonical status to a short label + a badge CSS modifier.
fn status_badge(status: FeatureStatus) -> (&'static str, &'static str) {
    match status {
        FeatureStatus::Intake => ("INTAKE", "neutral"),
        FeatureStatus::Investigating => ("INVESTIGATING", "active"),
        FeatureStatus::AwaitingClarification => ("NEEDS ANSWER", "warn"),
        FeatureStatus::Planned => ("PLANNED", "neutral"),
        FeatureStatus::Executing => ("EXECUTING", "active"),
        FeatureStatus::Gating => ("GATING", "active"),
        FeatureStatus::AwaitingQa => ("AWAITING QA", "warn"),
        FeatureStatus::SignedOff => ("SIGNED OFF", "done"),
        FeatureStatus::Done => ("DONE", "done"),
        FeatureStatus::Blocked => ("BLOCKED", "block"),
        FeatureStatus::Rejected => ("REJECTED", "block"),
    }
}

/// Which of the five read-only stage tabs is the active one for a given status.
/// The tabs are indicators driven by the engine, not free navigation.
fn active_stage_index(status: FeatureStatus) -> usize {
    match status {
        FeatureStatus::Intake => 0,
        FeatureStatus::Investigating | FeatureStatus::AwaitingClarification => 1,
        FeatureStatus::Planned => 2,
        FeatureStatus::Executing | FeatureStatus::Gating | FeatureStatus::Blocked => 3,
        FeatureStatus::AwaitingQa | FeatureStatus::SignedOff | FeatureStatus::Done => 4,
        FeatureStatus::Rejected => 0,
    }
}

const STAGE_TABS: &[&str] = &["INTAKE", "INVESTIGATION", "PLAN", "STATUS", "QA"];

/// Which view the enterprise cockpit is showing. Routines live INSIDE the cockpit
/// (it's an architect tool), reached via the cockpit's own nav, not a top-level app.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CockpitView {
    Stories,
    Routines,
}

/// The cockpit's internal nav: switch between the control surface (stories) and the
/// routine dashboard. Both are architect tools, so both live in the Enterprise app.
#[component]
fn CockpitNav(view: Signal<CockpitView>) -> Element {
    let mut view = view;
    let stories_cls = if view() == CockpitView::Stories {
        "cockpit-nav-tab on"
    } else {
        "cockpit-nav-tab"
    };
    let routines_cls = if view() == CockpitView::Routines {
        "cockpit-nav-tab on"
    } else {
        "cockpit-nav-tab"
    };
    rsx! {
        div { class: "cockpit-nav",
            button {
                class: "{stories_cls}",
                onclick: move |_| view.set(CockpitView::Stories),
                "Control surface"
            }
            button {
                class: "{routines_cls}",
                onclick: move |_| view.set(CockpitView::Routines),
                "Routines"
            }
        }
    }
}

#[component]
pub fn CockpitApp() -> Element {
    // Which cockpit view (control surface vs routines). Declared first so all hooks
    // below run unconditionally in a stable order regardless of the view.
    let view = use_signal(|| CockpitView::Stories);

    // Both data sets come from the BFF over HTTP. `use_resource` runs the fetch when
    // the cockpit mounts; the embedded server (see main.rs) is up by then.
    let stories_res = use_resource(fetch_stories);
    let rules_res = use_resource(fetch_rules);

    let fleet = use_signal(seed_fleet);
    let mut selected = use_signal(|| 0usize);
    let mut selected_rule = use_signal(|| 0usize);
    // The live run for the selected story, if one has been started. Polled to
    // completion; its gate events are REAL verdicts from the BFF run engine.
    let mut active_run = use_signal(|| Option::<RunView>::None);

    // A shared refresh tick: bumped whenever a clarification is posted or answered,
    // so both the NEEDS YOU queue here and the per-story thread refetch together.
    let clarify_refresh = use_signal(|| 0u32);
    use_context_provider(|| clarify_refresh);
    let open_clars_res = use_resource(move || {
        let _dep = clarify_refresh();
        async move { fetch_open_clarifications().await }
    });

    let stories_loaded = stories_res.read().clone();
    let rules_loaded = rules_res.read().clone();
    // A resolved-but-None fetch means the BFF was unreachable / returned junk.
    let errored = matches!(&stories_loaded, Some(None)) || matches!(&rules_loaded, Some(None));

    // Routines live inside the cockpit (an architect tool). All hooks above have run,
    // so branching here is safe.
    if view() == CockpitView::Routines {
        return rsx! {
            div { class: "cockpit",
                CockpitNav { view }
                div { class: "cockpit-scroll",
                    crate::routines::RoutineDashboard {}
                }
            }
        };
    }

    match (stories_loaded, rules_loaded) {
        (Some(Some(story_list)), Some(Some(rules))) => {
            if story_list.is_empty() {
                return rsx! {
                    div { class: "cockpit",
                        CockpitNav { view }
                        CockpitNotice { kind: "empty".to_string() }
                    }
                };
            }
            let current = story_list[selected().min(story_list.len() - 1)].clone();
            let active_stage = active_stage_index(current.status);

            rsx! {
                div { class: "cockpit",
                    CockpitNav { view }
                    CockpitTopBar { story: current.clone() }

                    div { class: "cockpit-body",
                        // ── LEFT: story spine (from /api/stories) + NEEDS YOU queue ──
                        aside { class: "cockpit-rail",
                            p { class: "cockpit-rail-label", "STORY SPINE" }
                            div { class: "spine-list",
                                for (i , s) in story_list.iter().enumerate() {
                                    {
                                        let (badge, badge_cls) = status_badge(s.status);
                                        let sel = i == selected();
                                        let cls = if sel { "spine-item sel" } else { "spine-item" };
                                        rsx! {
                                            button {
                                                class: "{cls}",
                                                onclick: move |_| selected.set(i),
                                                span { class: "spine-title", "{s.title}" }
                                                span { class: "spine-badge {badge_cls}", "{badge}" }
                                            }
                                        }
                                    }
                                }
                                button { class: "spine-new", "+ New story" }
                            }

                            {
                                let open_clars = open_clars_res.read().clone().flatten().unwrap_or_default();
                                let n = open_clars.len();
                                rsx! {
                                    p { class: "cockpit-rail-label needs", "NEEDS YOU ({n})" }
                                    div { class: "needs-list",
                                        if open_clars.is_empty() {
                                            p { class: "needs-empty", "Nothing needs you right now." }
                                        }
                                        for c in open_clars.iter() {
                                            {
                                                let target = story_list.iter().position(|s| s.id == c.story_id);
                                                let q = c.question.clone();
                                                let who = c.addressee.clone();
                                                rsx! {
                                                    button {
                                                        class: "needs-item",
                                                        onclick: move |_| {
                                                            if let Some(i) = target {
                                                                selected.set(i);
                                                            }
                                                        },
                                                        span { class: "needs-dot warn" }
                                                        span {
                                                            span { class: "needs-q", "{q}" }
                                                            span { class: "needs-who", "asked {who}" }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        // ── CENTER: stage tabs + active stage panel + status strip ──
                        section { class: "cockpit-stage",
                            div { class: "stage-tabs",
                                for (i , tab) in STAGE_TABS.iter().enumerate() {
                                    {
                                        let cls = if i == active_stage { "stage-tab on" } else { "stage-tab" };
                                        rsx! { span { class: "{cls}", "{tab}" } }
                                    }
                                }
                            }

                            div { class: "stage-panel",
                                // Run control: start a governed run for this story and
                                // poll it to completion, streaming the real gate verdicts.
                                {
                                    let sid = current.id.clone();
                                    rsx! {
                                        button {
                                            class: "btn-run",
                                            onclick: move |_| {
                                                let sid = sid.clone();
                                                spawn(async move {
                                                    if let Some(rid) = start_run(&sid).await {
                                                        loop {
                                                            if let Some(rv) = fetch_run(&rid).await {
                                                                let done = rv.done;
                                                                active_run.set(Some(rv));
                                                                if done {
                                                                    break;
                                                                }
                                                            }
                                                            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
                                                        }
                                                    }
                                                });
                                            },
                                            "▶ Run this story (governed)"
                                        }
                                    }
                                }

                                // While a run for THIS story is live, show it; otherwise
                                // the representative panel for the story's status.
                                {
                                    match active_run() {
                                        Some(r) if r.story_id == current.id => rsx! { LiveRunPanel { run: r } },
                                        _ => rsx! { StagePanel { story: current.clone(), fleet: fleet() } },
                                    }
                                }

                                // The clarify-bridge: ask the team a question, pick who
                                // to ask, and see the thread. In-process now.
                                ClarifySection { story_id: current.id.clone() }

                                // Decomposition: split this story into component
                                // children per the practice, review/edit, create.
                                DecomposeSection { story_id: current.id.clone() }
                            }

                            div { class: "status-strip",
                                div { class: "strip-fleet",
                                    for (i , a) in fleet().iter().enumerate() {
                                        {
                                            let arrow = i + 1 < fleet().len();
                                            rsx! {
                                                span { class: "fleet-pill {a.state_class}",
                                                    span { class: "fleet-role", "{a.role}" }
                                                    span { class: "fleet-state", "{a.state}" }
                                                }
                                                if arrow {
                                                    span { class: "fleet-arrow", "→" }
                                                }
                                            }
                                        }
                                    }
                                }
                                div { class: "strip-gates",
                                    span { class: "gate-tally",
                                        span { class: "gate-num", "1" }
                                        " layer-1 deny"
                                    }
                                    span { class: "gate-tally",
                                        span { class: "gate-num", "1" }
                                        " layer-2 bounce"
                                    }
                                }
                            }
                        }

                        // ── RIGHT: inspector. Enforced rules from /api/rules. ──
                        aside { class: "cockpit-inspector",
                            p { class: "cockpit-rail-label", "INSPECTOR" }
                            p { class: "inspector-hint", "The rules this fleet is governed by. These are the gate's actual enforced rules." }
                            div { class: "rule-list",
                                for (i , r) in rules.iter().enumerate() {
                                    {
                                        let sel = i == selected_rule();
                                        let cls = if sel { "rule-chip sel" } else { "rule-chip" };
                                        rsx! {
                                            button {
                                                class: "{cls}",
                                                onclick: move |_| selected_rule.set(i),
                                                "{r.id}"
                                            }
                                        }
                                    }
                                }
                            }
                            {
                                let idx = selected_rule().min(rules.len().saturating_sub(1));
                                let r = &rules[idx];
                                rsx! {
                                    div { class: "rule-detail",
                                        p { class: "rule-id", "{r.id}" }
                                        p { class: "rule-enforce",
                                            span { class: "enforce-dot" }
                                            "deterministic, active"
                                        }
                                        p { class: "rule-label", "Statement" }
                                        p { class: "rule-statement", "{r.statement}" }
                                        p { class: "rule-label", "Enforcement" }
                                        p { class: "rule-statement", "Checked at the MCP tool boundary before the write executes (deny-before-execute), and re-checked out-of-process after the task. Binary pass/fail." }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        _ if errored => rsx! {
            div { class: "cockpit",
                CockpitNav { view }
                CockpitNotice { kind: "error".to_string() }
            }
        },
        _ => rsx! {
            div { class: "cockpit",
                CockpitNav { view }
                CockpitNotice { kind: "loading".to_string() }
            }
        },
    }
}

/// Loading / error / empty placeholder for the cockpit, shown while the BFF fetch
/// is pending or if it fails.
#[component]
fn CockpitNotice(kind: String) -> Element {
    let (title, body) = match kind.as_str() {
        "loading" => (
            "Connecting to the engine…",
            "Reaching the local Camerata server.",
        ),
        "error" => (
            "Can't reach the engine",
            "The Camerata server isn't responding on localhost:8787. It starts with the app; if this persists, restart the app.",
        ),
        _ => (
            "No stories yet",
            "Adopt a story to start steering. (Story adoption from a tracker lands in a later phase.)",
        ),
    };
    rsx! {
        div { class: "cockpit-notice",
            p { class: "cockpit-notice-title", "{title}" }
            p { class: "cockpit-notice-body", "{body}" }
        }
    }
}

#[component]
fn CockpitTopBar(story: CanonicalStory) -> Element {
    let (badge, badge_cls) = status_badge(story.status);

    // SOURCE (where it's tracked) vs BUILD TARGETS (where its code lands) — the
    // two independent axes from the credential-delegated-scope decision.
    let source = match story.external_ref.as_ref() {
        Some(r) => format!("{:?} {}", r.provider, r.external_id),
        None => "native".to_string(),
    };
    let targets = if story.targets.is_empty() {
        "no targets yet".to_string()
    } else {
        story
            .targets
            .iter()
            .map(|t| match &t.role {
                Some(role) => format!("{} ({role})", t.repo),
                None => t.repo.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    };

    rsx! {
        div { class: "cockpit-topbar",
            div { class: "topbar-line1",
                span { class: "topbar-brand", "Camerata · Conductor" }
                span { class: "topbar-story", "{story.title}" }
                span { class: "topbar-status {badge_cls}", "{badge}" }
            }
            div { class: "topbar-line3",
                span { class: "topbar-axis-label", "source:" }
                span { class: "topbar-axis-val", "{source}" }
                span { class: "topbar-sep", "·" }
                span { class: "topbar-axis-label", "targets:" }
                span { class: "topbar-axis-val", "{targets}" }
            }
            div { class: "topbar-line2",
                span { class: "topbar-meter", "spent: $4.10 ",
                    span { class: "meter-est", "(~$100 Max 5x est)" }
                }
                span { class: "topbar-sep", "·" }
                span { "agents: 1 live" }
                span { class: "topbar-sep", "·" }
                span { class: "conn-ok", "● Connected" }
                span { class: "topbar-sep", "·" }
                span { class: "conn-warn", "gate: 1 layer-1 deny · 1 layer-2 bounce" }
            }
        }
    }
}

/// The center-stage body, swapped by the selected story's status.
#[component]
fn StagePanel(story: CanonicalStory, fleet: Vec<FleetAgent>) -> Element {
    match story.status {
        FeatureStatus::Executing | FeatureStatus::Gating => rsx! {
            div { class: "panel-exec",
                p { class: "panel-h", "{story.title}" }
                p { class: "panel-sub", "The governed fleet is executing. Each role works in an isolated worktree; every write passes the gate before integration." }
                div { class: "exec-agents",
                    for a in fleet.iter() {
                        div { class: "exec-agent {a.state_class}",
                            div { class: "exec-agent-head",
                                span { class: "exec-role", "{a.role}" }
                                span { class: "exec-state", "{a.state}" }
                            }
                            p { class: "exec-note",
                                {
                                    match a.state_class {
                                        "gated" => "Diff produced and passed the gate. Ready to integrate.",
                                        "exec" => "Writing the member-export endpoint and view.",
                                        _ => "Waiting on the upstream API contract.",
                                    }
                                }
                            }
                        }
                    }
                }
                div { class: "gate-activity",
                    p { class: "gate-activity-h", "Gate activity" }
                    div { class: "gate-event",
                        span { class: "gate-layer l1", "LAYER 1 · DENY" }
                        p { class: "gate-event-text",
                            "Frontend tried to write a hardcoded API token into the export config. "
                            span { class: "gate-rule", "SEC-NO-HARDCODED-SECRETS-1" }
                            " denied the write before it reached disk. The architect never had to catch it."
                        }
                    }
                    div { class: "gate-event",
                        span { class: "gate-layer l2", "LAYER 2 · BOUNCE" }
                        p { class: "gate-event-text",
                            "Backend's first diff built a SQL query by string concatenation. "
                            span { class: "gate-rule", "SEC-NO-RAW-SQL-CONCAT-1" }
                            " bounced it post-task; it revised and passed on the next attempt."
                        }
                    }
                }
            }
        },
        FeatureStatus::SignedOff | FeatureStatus::Done => rsx! {
            div { class: "panel-done",
                p { class: "panel-h", "{story.title}" }
                p { class: "panel-sub", "Signed off and ready to ship. Full provenance is attached." }
                div { class: "prov-line",
                    span { class: "prov-k", "diff" }
                    span { class: "prov-v", "3 files, +84 / -12, all gates passed" }
                }
                div { class: "prov-line",
                    span { class: "prov-k", "rules passed" }
                    span { class: "prov-v", "SEC-NO-HARDCODED-SECRETS-1 · SEC-NO-PATH-ESCAPE-1 · RUST-FMT · RUST-CLIPPY · RUST-TEST" }
                }
                div { class: "prov-line",
                    span { class: "prov-k", "sign-off" }
                    span { class: "prov-v", "architect, after QA" }
                }
            }
        },
        FeatureStatus::Blocked => rsx! {
            div { class: "panel-blocked",
                p { class: "panel-h", "{story.title}" }
                p { class: "panel-sub blocked", "Blocked, waiting on a decision." }
                p { class: "blocked-reason", "The invite flow needs a product decision: should an expired invite be re-sendable, or must the admin issue a fresh one? Routed to the requirements owner; execution is paused until they answer." }
            }
        },
        _ => rsx! {
            div { class: "panel-generic",
                p { class: "panel-h", "{story.title}" }
                p { class: "panel-sub", "{story.description}" }
            }
        },
    }
}

/// The live governed run: the real gate verdicts from the BFF run engine, streamed
/// in as the run walks to completion.
#[component]
fn LiveRunPanel(run: RunView) -> Element {
    let (status_label, status_cls) = run_status_badge(&run.status);
    let live = run.mode == "live";
    let mode_label = if live { "live fleet" } else { "scripted · token-free" };
    let sub = if live {
        "A real governed fleet (claude -p) under the gate. Stage and bounce events are reported as they happen."
    } else {
        "Token-free run: the agent is scripted, but the gate doing the deciding is the live one. Real deny/allow verdicts."
    };
    rsx! {
        div { class: "live-run",
            div { class: "live-run-head",
                span { class: "live-run-title", "Governed run" }
                span { class: "live-run-mode", "{mode_label}" }
                span { class: "live-run-status {status_cls}", "{status_label}" }
            }
            p { class: "panel-sub", "{sub}" }
            div { class: "live-events",
                for ev in run.events.iter() {
                    {
                        let vcls = match ev.verdict.as_str() {
                            "deny" => "live-event deny",
                            "allow" => "live-event allow",
                            _ => "live-event info",
                        };
                        let vlabel = ev.verdict.to_uppercase();
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
                    p { class: "live-events-empty", "Spinning up the fleet…" }
                }
            }
        }
    }
}

/// The clarify-bridge composer + thread: review a question, pick who to ask (the
/// per-question addressee picker), post it, and record the reply. Wired to the BFF
/// in-process; the live-tracker comment write-back is the provider phase.
#[component]
fn ClarifySection(story_id: String) -> Element {
    // Shared with the NEEDS YOU queue so posting/answering refetches both.
    let mut refresh = use_context::<Signal<u32>>();
    let sid_res = story_id.clone();
    let clars = use_resource(move || {
        let sid = sid_res.clone();
        let _dep = refresh();
        async move { fetch_clarifications(&sid).await }
    });

    let mut question = use_signal(|| {
        "Should the CSV export include archived members, or only currently active ones?"
            .to_string()
    });
    let mut addressee = use_signal(|| "@maria-pm".to_string());

    // Representative suggestions; on a live tracker these come from the ticket's
    // participants (assignee, reporter), plus "you" and a free-typed handle.
    let suggestions = ["@maria-pm", "@jdoe", "you"];

    let sid_post = story_id.clone();

    rsx! {
        div { class: "clarify",
            p { class: "clarify-h", "Ask the team" }
            p { class: "section-hint", "Review the question, pick who to ask, and post it. In-process now; this posts to the real tracker comment (with an @-mention) in the provider phase." }
            textarea {
                class: "clarify-q",
                value: "{question}",
                rows: "2",
                oninput: move |e| question.set(e.value()),
            }
            p { class: "clarify-label", "Ask:" }
            div { class: "clarify-addressees",
                for s in suggestions {
                    {
                        let sel = addressee() == s;
                        let cls = if sel { "addressee-chip sel" } else { "addressee-chip" };
                        rsx! {
                            button {
                                class: "{cls}",
                                onclick: move |_| addressee.set(s.to_string()),
                                "{s}"
                            }
                        }
                    }
                }
                input {
                    class: "addressee-input",
                    placeholder: "or type a handle…",
                    oninput: move |e| addressee.set(e.value()),
                }
            }
            button {
                class: "btn-run",
                onclick: move |_| {
                    let sid = sid_post.clone();
                    let q = question();
                    let a = addressee();
                    spawn(async move {
                        if post_clarification(&sid, &q, &a).await.is_some() {
                            refresh += 1;
                        }
                    });
                },
                "Post the question"
            }

            div { class: "clarify-thread",
                {
                    match clars() {
                        Some(Some(list)) if !list.is_empty() => rsx! {
                            for c in list {
                                ClarificationCard { clar: c, refresh }
                            }
                        },
                        Some(Some(_)) => rsx! { p { class: "section-hint", "No questions posted yet." } },
                        Some(None) => rsx! { p { class: "section-hint", "(Couldn't load the thread.)" } },
                        None => rsx! { p { class: "section-hint", "Loading…" } },
                    }
                }
            }
        }
    }
}

/// One clarification in the thread: shows the question + addressee, an answer input
/// while open, or the recorded reply once answered.
#[component]
fn ClarificationCard(clar: ClarificationView, refresh: Signal<u32>) -> Element {
    let mut refresh = refresh;
    let mut answer_text = use_signal(String::new);
    let open = clar.answer.is_none();
    let cid = clar.id.clone();
    let cls = if open { "clar-card open" } else { "clar-card answered" };

    rsx! {
        div { class: "{cls}",
            p { class: "clar-card-q", "{clar.question}" }
            p { class: "clar-card-meta", "to {clar.addressee}" }
            if open {
                div { class: "clar-answer-row",
                    input {
                        class: "addressee-input",
                        placeholder: "record the reply…",
                        value: "{answer_text}",
                        oninput: move |e| answer_text.set(e.value()),
                    }
                    button {
                        class: "btn-restart",
                        onclick: move |_| {
                            let cid = cid.clone();
                            let ans = answer_text();
                            spawn(async move {
                                if !ans.is_empty()
                                    && answer_clarification(&cid, &ans, "you").await.is_some()
                                {
                                    refresh += 1;
                                }
                            });
                        },
                        "Record answer"
                    }
                }
            } else {
                div { class: "clar-answered",
                    span { class: "clar-answer-by", "{clar.answered_by.clone().unwrap_or_default()} answered" }
                    p { class: "clar-answer-text", "{clar.answer.clone().unwrap_or_default()}" }
                }
            }
        }
    }
}

/// Decompose a parent story into component children: propose, edit titles, create.
/// Created children are real stories on the spine (visible in the left rail on the
/// next mount); the tracker write-back is the provider phase.
#[component]
fn DecomposeSection(story_id: String) -> Element {
    let mut proposed = use_signal(|| Option::<Vec<ProposedChildView>>::None);
    let mut child_refresh = use_signal(|| 0u32);
    let sid_children = story_id.clone();
    let children_res = use_resource(move || {
        let sid = sid_children.clone();
        let _dep = child_refresh();
        async move { fetch_children(&sid).await }
    });

    let sid_propose = story_id.clone();
    let sid_commit = story_id.clone();

    rsx! {
        div { class: "decompose",
            p { class: "clarify-h", "Decompose into component stories" }
            p { class: "section-hint", "Split this feature into the component stories your practice calls for (here: a UI story and an API story). Review and edit, then create. Creating writes them to the tracker as child work items in the provider phase." }
            button {
                class: "btn-run",
                onclick: move |_| {
                    let sid = sid_propose.clone();
                    spawn(async move {
                        if let Some(p) = fetch_proposal(&sid).await {
                            proposed.set(Some(p));
                        }
                    });
                },
                "Propose children"
            }

            {
                match proposed() {
                    Some(list) if !list.is_empty() => rsx! {
                        div { class: "proposed-list",
                            for (i , pc) in list.iter().enumerate() {
                                {
                                    let kind = pc.kind.clone();
                                    let title = pc.title.clone();
                                    rsx! {
                                        div { class: "proposed-child",
                                            span { class: "proposed-kind", "{kind}" }
                                            input {
                                                class: "addressee-input proposed-title",
                                                value: "{title}",
                                                oninput: move |e| {
                                                    if let Some(v) = proposed.write().as_mut() {
                                                        if let Some(item) = v.get_mut(i) {
                                                            item.title = e.value();
                                                        }
                                                    }
                                                },
                                            }
                                        }
                                    }
                                }
                            }
                            button {
                                class: "btn-run",
                                onclick: move |_| {
                                    let sid = sid_commit.clone();
                                    let children = proposed().unwrap_or_default();
                                    spawn(async move {
                                        if commit_children(&sid, &children).await.is_some() {
                                            proposed.set(None);
                                            child_refresh += 1;
                                        }
                                    });
                                },
                                "Create these stories"
                            }
                        }
                    },
                    _ => rsx! {},
                }
            }

            {
                let kids = children_res.read().clone().flatten().unwrap_or_default();
                if kids.is_empty() {
                    rsx! {}
                } else {
                    rsx! {
                        div { class: "children-list",
                            p { class: "clarify-label", "Component stories" }
                            for k in kids.iter() {
                                div { class: "child-row",
                                    span { class: "child-id", "{k.id}" }
                                    span { class: "child-title", "{k.title}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
