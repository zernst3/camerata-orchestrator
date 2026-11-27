//! The pre-flight before Start (#564): what `camerata inspect --plan-only` prints, shown on the
//! scan screen before the inspection is sent.
//!
//! "Inspect code against selected rules" asks the server for the plan of the inspection the
//! screen describes (`POST /api/onboard/plan`, no model call, no credential read) and shows it
//! here: the lines, the files admitted and left out by reason, the projected cost, the suggested
//! spend limit, every warning and what Start would say. Start sends the inspection; it is off
//! while the plan is loading and when the server says Start would refuse (a client inspection
//! with no spend limit, or a limit under the projection). The server refuses those starts itself;
//! this panel only says so first.

use super::*;

/// The html id of the panel, of its Start button and of its Back button.
pub(in crate::cockpit) const PREFLIGHT_ID: &str = "inspection-preflight";
pub(in crate::cockpit) const PREFLIGHT_START_ID: &str = "preflight-start";
pub(in crate::cockpit) const PREFLIGHT_BACK_ID: &str = "preflight-back";

/// The inspection the operator asked for, held while its pre-flight is on screen.
#[derive(Clone, PartialEq)]
pub(in crate::cockpit) struct PendingStart {
    pub repos: Vec<String>,
    pub rules: Vec<SelectedAuditRule>,
    pub exec_mode: String,
    pub incremental: bool,
    pub deep: bool,
    pub run_ai_review: bool,
    pub run_deterministic: bool,
    pub max_cost_usd: Option<f64>,
}

impl PendingStart {
    /// The request body, the same one Start sends.
    fn body(&self) -> serde_json::Value {
        audit_request_json(
            &self.repos,
            &self.rules,
            &self.exec_mode,
            self.incremental,
            self.deep,
            self.run_ai_review,
            self.run_deterministic,
            self.max_cost_usd,
        )
    }
}

/// The files of one repo the inspection would leave out for one reason.
#[derive(Clone, PartialEq, Debug, Default, serde::Deserialize)]
pub(in crate::cockpit) struct LeftOutView {
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub files: usize,
}

/// What the inspection would read of one repo.
#[derive(Clone, PartialEq, Debug, Default, serde::Deserialize)]
pub(in crate::cockpit) struct RepoAdmissionView {
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub files: usize,
    #[serde(default)]
    pub lines: usize,
    #[serde(default)]
    pub left_out: Vec<LeftOutView>,
}

/// The full-scan estimate's suggested spend limit.
#[derive(Clone, PartialEq, Debug, Default, serde::Deserialize)]
pub(in crate::cockpit) struct FullScanView {
    #[serde(default)]
    pub limit: f64,
}

/// The plan, as the server sends it.
#[derive(Clone, PartialEq, Debug, Default, serde::Deserialize)]
pub(in crate::cockpit) struct PlanView {
    #[serde(default)]
    pub repos: Vec<RepoAdmissionView>,
    #[serde(default)]
    pub total_files: usize,
    #[serde(default)]
    pub total_lines: usize,
    #[serde(default)]
    pub projected_usd: Option<f64>,
    #[serde(default)]
    pub full_scan: FullScanView,
    #[serde(default)]
    pub limit_usd: Option<f64>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub text: String,
}

/// Where the pre-flight stands.
#[derive(Clone, PartialEq, Debug)]
pub(in crate::cockpit) enum PreflightState {
    Loading,
    /// The plan, and what Start would say when it would refuse.
    Ready {
        plan: Box<PlanView>,
        start_refusal: Option<String>,
    },
    /// The plan could not be made; the reason.
    Failed(String),
}

impl PreflightState {
    /// Whether Start may be pressed: not while loading, and not when the server says it would
    /// refuse. A plan that could not be made leaves Start on: the start makes its own checks and
    /// says why it does not start.
    pub(in crate::cockpit) fn start_allowed(&self) -> bool {
        match self {
            Self::Loading => false,
            Self::Ready { start_refusal, .. } => start_refusal.is_none(),
            Self::Failed(_) => true,
        }
    }
}

/// The plan of `pending` from the server.
pub(in crate::cockpit) async fn fetch_plan(pending: &PendingStart) -> PreflightState {
    let answer = reqwest::Client::new()
        .post(format!("{}/api/onboard/plan", crate::bff_base()))
        .json(&pending.body())
        .send()
        .await;
    let value: serde_json::Value = match answer {
        Ok(resp) => match resp.json().await {
            Ok(v) => v,
            Err(_) => {
                return PreflightState::Failed("the server's answer could not be read".to_string())
            }
        },
        Err(_) => {
            return PreflightState::Failed("the server could not be reached".to_string());
        }
    };
    if value["ok"] != serde_json::Value::Bool(true) {
        return PreflightState::Failed(
            value["error"]
                .as_str()
                .unwrap_or("the server gave no reason")
                .to_string(),
        );
    }
    match serde_json::from_value::<PlanView>(value["plan"].clone()) {
        Ok(plan) => PreflightState::Ready {
            plan: Box::new(plan),
            start_refusal: value["start_refusal"].as_str().map(str::to_string),
        },
        Err(_) => PreflightState::Failed("the server's plan could not be read".to_string()),
    }
}

/// `n` with thousands separators.
fn count(v: usize) -> String {
    let digits = v.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The panel. `on_start` sends the inspection; `on_back` closes the panel without sending it.
#[component]
pub(in crate::cockpit) fn PreflightPanel(
    state: PreflightState,
    on_start: EventHandler<()>,
    on_back: EventHandler<()>,
) -> Element {
    let allowed = state.start_allowed();
    rsx! {
        div { id: "{PREFLIGHT_ID}", class: "preflight-panel",
            p { class: "scan-section-h", "Pre-flight" }
            p { class: "scan-section-sub",
                "What this inspection would read and cost, worked out before anything is sent to a model. The same figures as the command line's plan."
            }
            match &state {
                PreflightState::Loading => rsx! {
                    p { class: "preflight-loading", "Working out the pre-flight\u{2026}" }
                },
                PreflightState::Failed(why) => rsx! {
                    p { class: "preflight-failed", role: "alert",
                        "The pre-flight could not be made: {why}. Start makes its own checks and says why if it does not start."
                    }
                },
                PreflightState::Ready { plan, start_refusal } => rsx! {
                    ul { class: "preflight-figures",
                        li { "Lines: {count(plan.total_lines)}" }
                        li { "Files admitted: {count(plan.total_files)}" }
                        for repo in plan.repos.iter() {
                            li { key: "{repo.repo}",
                                "{repo.repo}: {count(repo.files)} files admitted, {count(repo.lines)} lines"
                                if repo.left_out.is_empty() {
                                    "; none left out"
                                }
                                for row in repo.left_out.iter() {
                                    "; {count(row.files)} left out ({row.reason})"
                                }
                            }
                        }
                        li {
                            match plan.projected_usd {
                                Some(usd) => rsx! { "Projected cost: about ${usd:.2} (projected, API prices)" },
                                None => rsx! { "Projected cost: not available (a model has no list price)" },
                            }
                        }
                        li { "Suggested spend limit: ${plan.full_scan.limit:.2}" }
                        li {
                            match plan.limit_usd {
                                Some(usd) => rsx! { "Spend limit: ${usd:.2}" },
                                None => rsx! { "Spend limit: none" },
                            }
                        }
                    }
                    if plan.warnings.is_empty() {
                        p { class: "preflight-warnings", "Warnings: none" }
                    } else {
                        div { class: "preflight-warnings",
                            p { "Warnings ({plan.warnings.len()}):" }
                            ul {
                                for (i, w) in plan.warnings.iter().enumerate() {
                                    li { key: "{i}", "{w}" }
                                }
                            }
                        }
                    }
                    if let Some(why) = start_refusal {
                        p { class: "preflight-refusal", role: "alert", "{why}" }
                    }
                    details { class: "preflight-text",
                        summary { "The plan as the command line prints it" }
                        pre { "{plan.text}" }
                    }
                },
            }
            div { class: "preflight-actions",
                button {
                    id: "{PREFLIGHT_START_ID}",
                    class: "btn-run",
                    disabled: !allowed,
                    onclick: move |_| {
                        if allowed {
                            on_start.call(());
                        }
                    },
                    "Start inspection"
                }
                button {
                    id: "{PREFLIGHT_BACK_ID}",
                    class: "btn-secondary",
                    onclick: move |_| on_back.call(()),
                    "Back"
                }
            }
        }
    }
}
