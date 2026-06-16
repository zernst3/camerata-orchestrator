//! The Agent-activity drawer: the window into the prompting Camerata otherwise
//! abstracts away. For a run, it shows one button per agent; selecting one reveals the
//! GENERATED prompt that agent was handed (the user never typed it) and its output as
//! the run progresses. Polled live. Built to upgrade to token streaming + parallel
//! agents for free when the engine does (it just renders whatever the transcript holds).

use std::time::Duration;

use dioxus::prelude::*;

/// One agent's transcript (`GET /api/runs/:id/agents`).
#[derive(Clone, PartialEq, serde::Deserialize)]
struct AgentTranscript {
    session_id: String,
    role: String,
    prompt: String,
    output: String,
    status: String,
}

async fn fetch_agents(run_id: &str) -> Option<Vec<AgentTranscript>> {
    reqwest::get(format!("{}/api/runs/{}/agents", crate::BFF_URL, run_id))
        .await
        .ok()?
        .json::<Vec<AgentTranscript>>()
        .await
        .ok()
}

/// The icon toggle + drawer for a run's agents. `run_id` empty -> nothing to show.
#[component]
pub fn AgentActivity(run_id: String) -> Element {
    let mut open = use_signal(|| false);
    let mut agents = use_signal(Vec::<AgentTranscript>::new);
    let mut selected = use_signal(|| 0usize);

    // Poll the transcript while the drawer is open. Started by the open toggle; the
    // loop self-stops when the drawer closes.
    let rid_for_poll = run_id.clone();
    let start_polling = move || {
        let rid = rid_for_poll.clone();
        spawn(async move {
            loop {
                if let Some(list) = fetch_agents(&rid).await {
                    agents.set(list);
                }
                if !open() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(700)).await;
            }
        });
    };

    let count = agents().len();
    let any_running = agents().iter().any(|a| a.status == "running");

    rsx! {
        div { class: "agent-activity",
            button {
                class: "agent-activity-toggle",
                disabled: run_id.is_empty(),
                onclick: move |_| {
                    let now = !open();
                    open.set(now);
                    if now {
                        start_polling();
                    }
                },
                if any_running {
                    span { class: "spinner spinner-sm" }
                }
                if count > 0 {
                    "🤖 Agent activity ({count})"
                } else {
                    "🤖 Agent activity"
                }
            }

            if open() {
                div { class: "agent-drawer",
                    if agents().is_empty() {
                        p { class: "agent-drawer-empty", "No agent activity yet — start a run to see each agent's generated prompt and output." }
                    } else {
                        div { class: "agent-tabs",
                            for (i , a) in agents().iter().enumerate() {
                                {
                                    let on = i == selected();
                                    let cls = match (on, a.status.as_str()) {
                                        (true, _) => "agent-tab on",
                                        (false, "blocked") => "agent-tab blocked",
                                        _ => "agent-tab",
                                    };
                                    rsx! {
                                        button {
                                            key: "{a.session_id}",
                                            class: "{cls}",
                                            onclick: move |_| selected.set(i),
                                            span { class: "agent-tab-role", "{a.role}" }
                                            span { class: "agent-tab-status {a.status}",
                                                if a.status == "running" {
                                                    span { class: "spinner spinner-sm" }
                                                }
                                                "{a.status}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        {
                            let idx = selected().min(agents().len().saturating_sub(1));
                            let a = agents()[idx].clone();
                            // Estimated token usage (~chars/4). Honest best-effort: real
                            // API/CLI usage isn't surfaced through the LLM layer yet, so
                            // this is an approximation that grows live as output streams.
                            let in_tok = a.prompt.chars().count() / 4;
                            let out_tok = a.output.chars().count() / 4;
                            rsx! {
                                div { class: "agent-detail",
                                    p { class: "agent-detail-label",
                                        "Generated prompt "
                                        span { class: "agent-tokens", "~{in_tok} tok in" }
                                    }
                                    pre { class: "agent-prompt", "{a.prompt}" }
                                    p { class: "agent-detail-label",
                                        "Output "
                                        span { class: "agent-tokens",
                                            "~{out_tok} tok out"
                                            if a.status == "running" { " · streaming" }
                                        }
                                    }
                                    // While the agent is thinking with nothing emitted yet,
                                    // run the Bombe — the "it's actually working" affordance.
                                    if a.output.is_empty() {
                                        div { class: "agent-thinking",
                                            crate::bombe::BombeSpinner { title: "Camerata is thinking\u{2026}".to_string() }
                                            span { class: "agent-thinking-label",
                                                if a.status == "running" { "thinking\u{2026}" } else { "waiting\u{2026}" }
                                            }
                                        }
                                    } else {
                                        pre { class: "agent-output", "{a.output}" }
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
