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
    /// The user's plain-language description (what they want).
    #[serde(default)]
    intent: String,
    /// The AI-authored operational prompt (shown on demand).
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

async fn create_routine(
    name: &str,
    schedule: &str,
    intent: &str,
    prompt: &str,
    scope: &str,
) -> Option<RoutineView> {
    reqwest::Client::new()
        .post(format!("{}/api/routines", crate::BFF_URL))
        .json(&serde_json::json!({
            "name": name, "schedule": schedule, "intent": intent, "prompt": prompt, "scope": scope
        }))
        .send()
        .await
        .ok()?
        .json::<RoutineView>()
        .await
        .ok()
}

/// Draft the operational prompt from the user's intent. Returns (prompt, authored_by).
async fn draft_prompt(intent: &str, scope: &str) -> Option<(String, String)> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/routines/draft-prompt", crate::BFF_URL))
        .json(&serde_json::json!({ "intent": intent, "scope": scope }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let prompt = v.get("prompt")?.as_str()?.to_string();
    let authored_by = v
        .get("authored_by")
        .and_then(|a| a.as_str())
        .unwrap_or("scaffold")
        .to_string();
    Some((prompt, authored_by))
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
    // The user writes INTENT; the AI drafts the operational PROMPT for review.
    let mut intent = use_signal(String::new);
    let mut prompt = use_signal(String::new);
    let mut authored_by = use_signal(String::new);
    let mut drafting = use_signal(|| false);
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
                                    span { class: "routine-prompt", "{r.intent}" }
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
                p { class: "section-hint", "Describe what you want the routine to do. Camerata's lead engineer drafts the operational prompt (model tiering, directives, scope) from it — you review and edit before it runs." }
                div { class: "routine-create-row",
                    input { class: "addressee-input", placeholder: "name", value: "{name}", oninput: move |e| name.set(e.value()) }
                    input { class: "addressee-input", placeholder: "schedule (e.g. daily 09:00)", value: "{schedule}", oninput: move |e| schedule.set(e.value()) }
                    input { class: "addressee-input", placeholder: "scope", value: "{scope}", oninput: move |e| scope.set(e.value()) }
                }
                // INTENT: what the user wants (their words).
                textarea {
                    class: "routine-intent-input",
                    rows: "2",
                    placeholder: "Describe what you want this routine to do (e.g. \"nightly, scan deps for advisories and open governed PRs for safe upgrades\")",
                    value: "{intent}",
                    oninput: move |e| intent.set(e.value()),
                }
                // DRAFT the operational prompt from the intent.
                div { class: "routine-draft-row",
                    button {
                        class: "btn-restart",
                        disabled: intent().trim().is_empty() || drafting(),
                        onclick: move |_| {
                            let (i, sc) = (intent(), scope());
                            if i.trim().is_empty() { return; }
                            drafting.set(true);
                            spawn(async move {
                                if let Some((p, by)) = draft_prompt(&i, &sc).await {
                                    prompt.set(p);
                                    authored_by.set(by);
                                }
                                drafting.set(false);
                            });
                        },
                        if drafting() { "Drafting…" } else { "Draft operational prompt" }
                    }
                    if !authored_by().is_empty() {
                        span { class: "routine-authored",
                            {
                                if authored_by() == "claude" {
                                    "authored by the lead engineer — review & edit below"
                                } else {
                                    "draft scaffold (connect Claude for a fully-authored prompt) — review & edit below"
                                }
                            }
                        }
                    }
                }
                // REVIEW the operational prompt (editable).
                textarea {
                    class: "routine-prompt-input",
                    rows: "7",
                    placeholder: "The operational prompt the agent will run (draft it above, then review/edit). Leave empty to scaffold from your description on save.",
                    value: "{prompt}",
                    oninput: move |e| prompt.set(e.value()),
                }
                button {
                    class: "btn-run",
                    onclick: move |_| {
                        let (n, s, i, p, sc) = (name(), schedule(), intent(), prompt(), scope());
                        if n.is_empty() || i.trim().is_empty() {
                            return;
                        }
                        spawn(async move {
                            if create_routine(&n, &s, &i, &p, &sc).await.is_some() {
                                refresh += 1;
                            }
                        });
                        name.set(String::new());
                        intent.set(String::new());
                        prompt.set(String::new());
                        authored_by.set(String::new());
                    },
                    "Add routine"
                }
            }
        }
    }
}
