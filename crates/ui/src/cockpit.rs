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
//! - RIGHT: an inspector that binds to the selection (rules show statement + rationale
//!   + selectable alternatives).
//!
//! Honesty note: this is a faithful shell over representative cockpit state, not a
//! live fleet run. The story spine uses the REAL canonical types
//! (`camerata_worktracker::{CanonicalStory, FeatureStatus}`) and the rule inspector
//! shows the REAL enforced rules from `camerata_gateway::RULE_REGISTRY` verbatim. The
//! live fleet wiring (the same path `worktracker-demo` / `po-demo` exercise) is the
//! next increment behind this surface.

use dioxus::prelude::*;

use camerata_gateway::{RuleEntry, RULE_REGISTRY};
use camerata_worktracker::{CanonicalStory, FeatureStatus};

/// The rules the cockpit inspector showcases: every enforced rule EXCEPT GOV-1.
///
/// GOV-1 ("deny writes whose path contains the substring 'forbidden'") is the
/// verification fixture the live-demo and acceptance tests fire against; it stays
/// in the registry (and the test suite) but is deliberately not surfaced here,
/// because as a product rule it is trivial and would undercut the substantive
/// SEC/ARCH rules that earn the inspector its credibility. The remaining four are
/// all real, enforced rules with non-trivial checks.
fn showcase_rules() -> Vec<&'static RuleEntry> {
    RULE_REGISTRY.iter().filter(|e| e.id != "GOV-1").collect()
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

/// Representative cockpit state, built from the real canonical Story type. Three
/// stories in different lifecycle states so the spine and the center stage show
/// the range a steering architect actually sees.
fn seed_stories() -> Vec<CanonicalStory> {
    vec![
        CanonicalStory {
            id: "CAM-1".into(),
            external_ref: None,
            title: "Add CSV export to org members".into(),
            description: "As an org admin I want to export the member directory to CSV \
                          so I can reconcile it against payroll."
                .into(),
            status: FeatureStatus::Executing,
            created_by: "architect".into(),
        },
        CanonicalStory {
            id: "CAM-2".into(),
            external_ref: None,
            title: "Fix timezone handling in reminders".into(),
            description: "Reminder emails fire in UTC instead of the org's local zone."
                .into(),
            status: FeatureStatus::SignedOff,
            created_by: "architect".into(),
        },
        CanonicalStory {
            id: "CAM-3".into(),
            external_ref: None,
            title: "Invite-only org signup".into(),
            description: "Gate new-member signup behind an invitation from an org admin."
                .into(),
            status: FeatureStatus::Blocked,
            created_by: "architect".into(),
        },
    ]
}

/// The fleet for the active (Executing) story: Backend gated, Frontend running,
/// Integrate pending. Mirrors the wireframe's status strip.
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
    let stories = use_signal(seed_stories);
    let fleet = use_signal(seed_fleet);
    let mut selected = use_signal(|| 0usize);
    // The inspector binds to a selected enforced rule (real data from the gateway).
    let mut selected_rule = use_signal(|| 0usize);

    let story_list = stories();
    let current = story_list[selected().min(story_list.len() - 1)].clone();
    let active_stage = active_stage_index(current.status);

    rsx! {
        div { class: "cockpit",
            // ── Top bar: story, live status, cost meter, fleet count, conn, gate ──
            CockpitTopBar { story: current.clone() }

            div { class: "cockpit-body",
                // ── LEFT: story spine + NEEDS YOU queue ──
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
                        StagePanel { story: current.clone(), fleet: fleet() }
                    }

                    // Always-visible status strip: the governed fleet + the gate tally.
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

                // ── RIGHT: inspector. Real enforced rules from the gateway. ──
                aside { class: "cockpit-inspector",
                    p { class: "cockpit-rail-label", "INSPECTOR" }
                    p { class: "inspector-hint", "The rules this fleet is governed by. These are the gate's actual enforced rules." }
                    div { class: "rule-list",
                        for (i , entry) in showcase_rules().iter().enumerate() {
                            {
                                let sel = i == selected_rule();
                                let cls = if sel { "rule-chip sel" } else { "rule-chip" };
                                rsx! {
                                    button {
                                        class: "{cls}",
                                        onclick: move |_| selected_rule.set(i),
                                        "{entry.id}"
                                    }
                                }
                            }
                        }
                    }
                    {
                        let rules = showcase_rules();
                        let entry = rules[selected_rule().min(rules.len() - 1)];
                        rsx! {
                            div { class: "rule-detail",
                                p { class: "rule-id", "{entry.id}" }
                                p { class: "rule-enforce",
                                    span { class: "enforce-dot" }
                                    "deterministic, active"
                                }
                                p { class: "rule-label", "Statement" }
                                p { class: "rule-statement", "{entry.description}" }
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
