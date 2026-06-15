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

#[component]
pub fn CockpitApp() -> Element {
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

    let stories_loaded = stories_res.read().clone();
    let rules_loaded = rules_res.read().clone();
    // A resolved-but-None fetch means the BFF was unreachable / returned junk.
    let errored = matches!(&stories_loaded, Some(None)) || matches!(&rules_loaded, Some(None));

    match (stories_loaded, rules_loaded) {
        (Some(Some(story_list)), Some(Some(rules))) => {
            if story_list.is_empty() {
                return rsx! { CockpitNotice { kind: "empty".to_string() } };
            }
            let current = story_list[selected().min(story_list.len() - 1)].clone();
            let active_stage = active_stage_index(current.status);

            rsx! {
                div { class: "cockpit",
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

                            p { class: "cockpit-rail-label needs", "NEEDS YOU (2)" }
                            div { class: "needs-list",
                                button {
                                    class: "needs-item",
                                    onclick: move |_| selected.set(0),
                                    span { class: "needs-dot warn" }
                                    span { "Answer: currency for the export amounts?" }
                                }
                                button {
                                    class: "needs-item",
                                    onclick: move |_| selected.set(1),
                                    span { class: "needs-dot warn" }
                                    span { "QA the governed diff for CAM-2" }
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
        _ if errored => rsx! { CockpitNotice { kind: "error".to_string() } },
        _ => rsx! { CockpitNotice { kind: "loading".to_string() } },
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
    rsx! {
        div { class: "cockpit-topbar",
            div { class: "topbar-line1",
                span { class: "topbar-brand", "Camerata · Conductor" }
                span { class: "topbar-story", "{story.title}" }
                span { class: "topbar-status {badge_cls}", "{badge}" }
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
    rsx! {
        div { class: "live-run",
            div { class: "live-run-head",
                span { class: "live-run-title", "Governed run" }
                span { class: "live-run-status {status_cls}", "{status_label}" }
            }
            p { class: "panel-sub", "Real verdicts from the gate, as the run executes. In this token-free run the agent is scripted; the gate doing the deciding is the live one." }
            div { class: "live-events",
                for ev in run.events.iter() {
                    {
                        let vcls = if ev.verdict == "deny" { "live-event deny" } else { "live-event allow" };
                        let vlabel = if ev.verdict == "deny" { "DENIED" } else { "ALLOWED" };
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
