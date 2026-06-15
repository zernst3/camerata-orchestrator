//! The routine dashboard: a third surface to manage scheduled governed routines
//! (ADR `routine_dashboard`). A table of routines with their schedule, prompt,
//! permission scope, enabled state, and last-run summary, plus enable/disable,
//! run-now, and a create form. Run-now executes a governed run (real gate verdicts)
//! and records the summary. The auto-fire scheduler is the remaining wiring.

use dioxus::prelude::*;

/// A routine as the BFF reports it (`/api/routines`).
#[derive(Clone, PartialEq, serde::Deserialize)]
struct RoutineView {
    id: String,
    name: String,
    schedule: String,
    prompt: String,
    scope: String,
    enabled: bool,
    last_run: Option<RoutineRunSummaryView>,
}

#[derive(Clone, PartialEq, serde::Deserialize)]
struct RoutineRunSummaryView {
    outcome: String,
    #[allow(dead_code)]
    total_verdicts: usize,
    denies: usize,
    allows: usize,
}

async fn fetch_routines() -> Option<Vec<RoutineView>> {
    reqwest::get(format!("{}/api/routines", crate::BFF_URL))
        .await
        .ok()?
        .json::<Vec<RoutineView>>()
        .await
        .ok()
}

async fn set_enabled(id: &str, enabled: bool) -> Option<RoutineView> {
    reqwest::Client::new()
        .post(format!("{}/api/routines/{}/enable", crate::BFF_URL, id))
        .json(&serde_json::json!({ "enabled": enabled }))
        .send()
        .await
        .ok()?
        .json::<RoutineView>()
        .await
        .ok()
}

async fn run_now(id: &str) -> Option<RoutineView> {
    reqwest::Client::new()
        .post(format!("{}/api/routines/{}/run", crate::BFF_URL, id))
        .send()
        .await
        .ok()?
        .json::<RoutineView>()
        .await
        .ok()
}

async fn create_routine(name: &str, schedule: &str, prompt: &str, scope: &str) -> Option<RoutineView> {
    reqwest::Client::new()
        .post(format!("{}/api/routines", crate::BFF_URL))
        .json(&serde_json::json!({
            "name": name, "schedule": schedule, "prompt": prompt, "scope": scope
        }))
        .send()
        .await
        .ok()?
        .json::<RoutineView>()
        .await
        .ok()
}

#[component]
pub fn RoutineDashboard() -> Element {
    let mut refresh = use_signal(|| 0u32);
    let routines_res = use_resource(move || {
        let _dep = refresh();
        async move { fetch_routines().await }
    });

    let mut name = use_signal(String::new);
    let mut schedule = use_signal(|| "daily 09:00".to_string());
    let mut prompt = use_signal(String::new);
    let mut scope = use_signal(|| "read-only".to_string());

    let routines = routines_res.read().clone().flatten().unwrap_or_default();

    rsx! {
        div { class: "page page-wide routines-page",
            p { class: "eyebrow", "Automation" }
            h1 { class: "h1", "Routines" }
            p { class: "lede", "Scheduled governed runs. Each runs through the same gate as an interactive run; run one now to see its real verdicts summarized." }

            div { class: "routine-table",
                div { class: "routine-row routine-head",
                    span { "Routine" }
                    span { "Schedule" }
                    span { "Scope" }
                    span { "Last run" }
                    span { "" }
                }
                if routines.is_empty() {
                    p { class: "section-hint", "Loading…" }
                }
                for r in routines.iter() {
                    {
                        let id_toggle = r.id.clone();
                        let id_run = r.id.clone();
                        let enabled = r.enabled;
                        let last = r.last_run.clone();
                        let row_cls = if enabled { "routine-row" } else { "routine-row off" };
                        rsx! {
                            div { class: "{row_cls}",
                                div { class: "routine-name",
                                    span { class: "routine-title", "{r.name}" }
                                    span { class: "routine-prompt", "{r.prompt}" }
                                }
                                span { class: "routine-sched", "{r.schedule}" }
                                span { class: "routine-scope", "{r.scope}" }
                                span { class: "routine-last",
                                    {
                                        match last {
                                            Some(s) => rsx! {
                                                span { class: "routine-passed", "{s.outcome} · {s.denies} denied, {s.allows} allowed" }
                                            },
                                            None => rsx! { span { class: "routine-never", "not run yet" } },
                                        }
                                    }
                                }
                                div { class: "routine-actions",
                                    button {
                                        class: "btn-restart",
                                        onclick: move |_| {
                                            let id = id_toggle.clone();
                                            spawn(async move {
                                                if set_enabled(&id, !enabled).await.is_some() {
                                                    refresh += 1;
                                                }
                                            });
                                        },
                                        if enabled { "Disable" } else { "Enable" }
                                    }
                                    button {
                                        class: "btn-run-sm",
                                        onclick: move |_| {
                                            let id = id_run.clone();
                                            spawn(async move {
                                                if run_now(&id).await.is_some() {
                                                    refresh += 1;
                                                }
                                            });
                                        },
                                        "Run now"
                                    }
                                }
                            }
                        }
                    }
                }
            }

            div { class: "routine-create",
                p { class: "section-label", "Add a routine" }
                div { class: "routine-create-row",
                    input { class: "addressee-input", placeholder: "name", value: "{name}", oninput: move |e| name.set(e.value()) }
                    input { class: "addressee-input", placeholder: "schedule (e.g. daily 09:00)", value: "{schedule}", oninput: move |e| schedule.set(e.value()) }
                    input { class: "addressee-input", placeholder: "scope", value: "{scope}", oninput: move |e| scope.set(e.value()) }
                }
                input { class: "addressee-input routine-prompt-input", placeholder: "what it does (prompt)", value: "{prompt}", oninput: move |e| prompt.set(e.value()) }
                button {
                    class: "btn-run",
                    onclick: move |_| {
                        let (n, s, p, sc) = (name(), schedule(), prompt(), scope());
                        if n.is_empty() {
                            return;
                        }
                        spawn(async move {
                            if create_routine(&n, &s, &p, &sc).await.is_some() {
                                refresh += 1;
                            }
                        });
                        name.set(String::new());
                        prompt.set(String::new());
                    },
                    "Add routine"
                }
            }
        }
    }
}
