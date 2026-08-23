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

/// Label for the "no output yet" placeholder shown while an agent's `output` is still
/// empty. `running` is the only status where "more is coming" framing is honest. Once an
/// agent reaches a TERMINAL status (`done`/`blocked`, the only two the server ever
/// transitions to — see `crates/server/src/ai_audit.rs`'s `set_status` call sites) with no
/// captured output, saying "waiting…" is simply wrong: nothing further is coming. This is
/// what kept the drawer reading "waiting…" forever after a scan finished when a pass
/// produced no free-text output (e.g. a fast/no-finding pass, or a backend that doesn't
/// stream per-agent deltas) — the old logic only special-cased `running` and fell through
/// to "waiting…" for every other status, including the terminal ones.
fn empty_output_label(status: &str) -> &'static str {
    match status {
        "running" => "thinking\u{2026}",
        "done" => "finished \u{2014} no output captured",
        "blocked" => "blocked \u{2014} no output",
        // Not currently reachable (the server only ever sets running/done/blocked), but kept
        // as the conservative fallback for a genuinely not-yet-started agent.
        _ => "waiting\u{2026}",
    }
}

async fn fetch_agents(run_id: &str) -> Option<Vec<AgentTranscript>> {
    let base = crate::bff_base();
    reqwest::get(format!("{base}/api/runs/{run_id}/agents"))
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
                                    // the background Bombe machine is already running via the
                                    // global loading count — just show the text label.
                                    if a.output.is_empty() {
                                        div { class: "agent-thinking",
                                            span { class: "agent-thinking-label",
                                                "{empty_output_label(&a.status)}"
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

#[cfg(test)]
mod tests {
    use super::empty_output_label;

    // Bug: the drawer got stuck reading "waiting…" forever after a scan finished, because
    // the old label logic only special-cased `running` and fell through to "waiting…" for
    // EVERY other status — including the terminal ones. `done`/`blocked` are the only two
    // terminal statuses the server ever sets (see `ai_audit.rs`'s `set_status` calls); once
    // an agent reaches either with no output, "waiting…" (implying more is coming) is wrong.
    #[test]
    fn terminal_statuses_never_render_waiting() {
        assert_eq!(empty_output_label("running"), "thinking\u{2026}");
        assert_ne!(empty_output_label("done"), "waiting\u{2026}");
        assert_ne!(empty_output_label("blocked"), "waiting\u{2026}");
    }

    #[test]
    fn done_and_blocked_get_distinct_terminal_labels() {
        assert_eq!(empty_output_label("done"), "finished \u{2014} no output captured");
        assert_eq!(empty_output_label("blocked"), "blocked \u{2014} no output");
    }

    // A genuinely unrecognized/not-yet-started status is the only case that still shows
    // "waiting…" — kept as the conservative fallback (not currently reachable in practice).
    #[test]
    fn unknown_status_falls_back_to_waiting() {
        assert_eq!(empty_output_label("queued"), "waiting\u{2026}");
    }

    // ── Tier-2 UI test: the network helper against a MOCK BFF (wiremock) ─────────
    // Verifies fetch_agents' request CONTRACT: it GETs /api/runs/:id/agents and parses the JSON body
    // into Vec<AgentTranscript>. Points the helper at a fake server via the CAMERATA_BFF_URL seam.
    // (The env override is process-global; these env-setting tests must not run concurrently with
    // another helper that reads bff_base(), so they're kept narrow.)
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_agents_gets_the_run_agents_path_and_parses_the_transcripts() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/runs/run-42/agents"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "session_id": "s1",
                    "role": "po",
                    "prompt": "Generated prompt for the PO agent.",
                    "output": "Some output.",
                    "status": "running"
                },
                {
                    "session_id": "s2",
                    "role": "architect",
                    "prompt": "Architect prompt.",
                    "output": "",
                    "status": "blocked"
                }
            ])))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let result = super::fetch_agents("run-42").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let agents = result.expect("a 200 with a JSON array parses into Some(Vec)");
        assert_eq!(agents.len(), 2, "both transcripts are parsed");
        assert_eq!(agents[0].session_id, "s1");
        assert_eq!(agents[0].role, "po");
        assert_eq!(agents[0].status, "running");
        assert_eq!(agents[0].prompt, "Generated prompt for the PO agent.");
        assert_eq!(agents[1].status, "blocked");
        assert_eq!(agents[1].output, "", "the empty-output (thinking) case round-trips");
        // `.expect(1)` on the mock asserts (on server drop) the helper hit GET /api/runs/run-42/agents
        // exactly once — i.e. it interpolated the run_id into the path correctly.
    }

    // fetch_agents returns None when the body is not the expected shape (a transcript field missing),
    // so the caller never overwrites the live list with garbage.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_agents_returns_none_on_malformed_body() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/runs/run-9/agents"))
            // Missing the required `status`/`output`/`prompt` fields -> deserialization fails.
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "session_id": "s1", "role": "po" }
            ])))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let result = super::fetch_agents("run-9").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(result.is_none(), "a body that doesn't match AgentTranscript yields None");
    }

    // ── Tier-1 UI test: render the AgentActivity component to HTML headlessly ─────
    // (VirtualDom + dioxus-ssr). The component uses only use_signal (no use_context), so no provider
    // setup is needed. With the drawer closed (open starts false), it renders just the toggle button;
    // polling never starts, so no network is touched. Asserts the toggle's static structure.
    mod render_tests {
        use super::super::AgentActivity;
        use dioxus::prelude::*;

        // run_id empty -> the toggle is disabled and shows the bare label (count is 0).
        fn empty_harness() -> Element {
            rsx! {
                AgentActivity { run_id: String::new() }
            }
        }

        #[test]
        fn renders_disabled_toggle_when_run_id_empty() {
            let mut vdom = VirtualDom::new(empty_harness);
            vdom.rebuild_in_place();
            let html = dioxus_ssr::render(&vdom);
            assert!(
                html.contains("agent-activity-toggle"),
                "the toggle button renders; html=\n{html}"
            );
            assert!(
                html.contains("Agent activity"),
                "the toggle label renders; html=\n{html}"
            );
            assert!(
                html.contains("disabled"),
                "the toggle is disabled with no run; html=\n{html}"
            );
        }

        // A non-empty run_id enables the toggle. The drawer stays closed on first render (open=false),
        // so the drawer body is NOT in the HTML — assert the closed/enabled toggle shape.
        fn enabled_harness() -> Element {
            rsx! {
                AgentActivity { run_id: "run-1".to_string() }
            }
        }

        #[test]
        fn renders_enabled_toggle_and_no_drawer_when_closed() {
            let mut vdom = VirtualDom::new(enabled_harness);
            vdom.rebuild_in_place();
            let html = dioxus_ssr::render(&vdom);
            assert!(
                html.contains("agent-activity-toggle"),
                "the toggle button renders; html=\n{html}"
            );
            assert!(
                html.contains("Agent activity"),
                "the toggle label renders; html=\n{html}"
            );
            assert!(
                !html.contains("agent-drawer"),
                "the drawer is closed on first render, so its body is absent; html=\n{html}"
            );
        }
    }
}
