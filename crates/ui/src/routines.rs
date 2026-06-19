//! The routine dashboard: a third surface to manage scheduled governed routines
//! (ADR `routine_dashboard`). A table of routines with their schedule, prompt,
//! permission scope, enabled state, and last-run summary, plus enable/disable,
//! run-now, and a create form. Run-now executes a governed run (real gate verdicts)
//! and records the summary. The auto-fire scheduler is the remaining wiring.

use dioxus::prelude::*;

/// Weekday labels, Sunday-first (matches the `weekdays` toggle vector order).
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Serialize the structured schedule picker into the human-readable schedule string
/// the BFF stores (e.g. `daily 09:00`, `weekly Mon,Wed 09:00`, `monthly day 15 09:00`,
/// `once 2026-06-20 14:00`). The empty-field fallbacks keep the string well-formed
/// even before every control is touched.
fn build_schedule(freq: &str, time: &str, date: &str, weekdays: &[bool], monthday: u32) -> String {
    let t = if time.is_empty() { "09:00" } else { time };
    match freq {
        "once" => {
            if date.is_empty() {
                format!("once {t}")
            } else {
                format!("once {date} {t}")
            }
        }
        "weekly" => {
            let days: Vec<&str> = weekdays
                .iter()
                .enumerate()
                .filter(|(_, on)| **on)
                .map(|(i, _)| WEEKDAYS[i])
                .collect();
            let days_str = if days.is_empty() {
                "Mon".to_string()
            } else {
                days.join(",")
            };
            format!("weekly {days_str} {t}")
        }
        "monthly" => format!("monthly day {monthday} {t}"),
        _ => format!("daily {t}"),
    }
}

/// Parse a stored schedule string back into the picker state, for Edit prefill.
/// Returns `(freq, time, date, weekdays, monthday)`. Anything that doesn't match a
/// known shape falls back to a daily-09:00 default (the schedule string is still
/// shown verbatim in the row, so nothing is lost — the picker just starts neutral).
fn parse_schedule(s: &str) -> (String, String, String, Vec<bool>, u32) {
    let default_days = vec![false, true, false, false, false, false, false];
    let parts: Vec<&str> = s.split_whitespace().collect();
    match parts.as_slice() {
        ["daily", time] => (
            "daily".into(),
            (*time).into(),
            String::new(),
            default_days,
            1,
        ),
        ["weekly", days, time] => {
            let mut wd = vec![false; 7];
            for d in days.split(',') {
                if let Some(i) = WEEKDAYS.iter().position(|w| w.eq_ignore_ascii_case(d)) {
                    wd[i] = true;
                }
            }
            ("weekly".into(), (*time).into(), String::new(), wd, 1)
        }
        ["monthly", "day", n, time] => (
            "monthly".into(),
            (*time).into(),
            String::new(),
            default_days,
            n.parse::<u32>().unwrap_or(1).clamp(1, 31),
        ),
        ["once", date, time] => (
            "once".into(),
            (*time).into(),
            (*date).into(),
            default_days,
            1,
        ),
        ["once", time] => (
            "once".into(),
            (*time).into(),
            String::new(),
            default_days,
            1,
        ),
        _ => ("daily".into(), "09:00".into(), String::new(), default_days, 1),
    }
}

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
    /// Whether this routine is set up on this backend. Imported routines arrive
    /// un-provisioned and need a "Set up" before Start does anything. Defaults true so
    /// the field is optional against older BFFs.
    #[serde(default = "default_true")]
    provisioned: bool,
    /// When the scheduler last fired it (RFC3339). Carried for future display; not yet
    /// rendered.
    #[serde(default)]
    #[allow(dead_code)]
    last_fired: Option<String>,
}

fn default_true() -> bool {
    true
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

/// Provision an imported routine on this backend (the "Set up" action). Returns the
/// updated routine (now `provisioned`, still stopped).
async fn provision(id: &str) -> Option<RoutineView> {
    reqwest::Client::new()
        .post(format!("{}/api/routines/{}/provision", crate::BFF_URL, id))
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

async fn update_routine(
    id: &str,
    name: &str,
    schedule: &str,
    intent: &str,
    prompt: &str,
    scope: &str,
) -> Option<RoutineView> {
    reqwest::Client::new()
        .put(format!("{}/api/routines/{}", crate::BFF_URL, id))
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

async fn delete_routine(id: &str) -> bool {
    reqwest::Client::new()
        .delete(format!("{}/api/routines/{}", crate::BFF_URL, id))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
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
    // Structured schedule builder. These drive a typical frequency picker (one-off /
    // daily / weekly / monthly) and serialize to the `schedule` string on save —
    // the BFF stores a human-readable schedule, so the UI owns the shape.
    let mut freq = use_signal(|| "daily".to_string());
    let mut sched_time = use_signal(|| "09:00".to_string());
    let mut sched_date = use_signal(String::new);
    // One toggle per weekday, Sun..Sat; Mon on by default.
    let mut weekdays = use_signal(|| vec![false, true, false, false, false, false, false]);
    let mut monthday = use_signal(|| 1u32);
    // The user writes INTENT; the AI drafts the operational PROMPT for review.
    let mut intent = use_signal(String::new);
    let mut prompt = use_signal(String::new);
    let mut authored_by = use_signal(String::new);
    let mut drafting = use_signal(|| false);
    let mut scope = use_signal(|| "read-only".to_string());
    // When Some(id), the form is EDITING that routine (Save updates it) rather than
    // creating a new one. `pending_delete` holds the id awaiting a confirm click.
    let mut editing = use_signal(|| Option::<String>::None);
    let mut pending_delete = use_signal(|| Option::<String>::None);

    // Distinguish "still fetching" (outer None) from "resolved, but there are
    // genuinely none" — so an empty list shows its own state, not a stuck "Loading…".
    let loading = routines_res.read().is_none();
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
                if loading {
                    p { class: "section-hint", "Loading…" }
                } else if routines.is_empty() {
                    p { class: "routine-empty", "No routines yet. Add one below to schedule a governed run." }
                }
                for r in routines.iter() {
                    {
                        let id_toggle = r.id.clone();
                        let id_provision = r.id.clone();
                        let id_run = r.id.clone();
                        let id_del = r.id.clone();
                        let r_edit = r.clone();
                        let enabled = r.enabled;
                        let provisioned = r.provisioned;
                        let last = r.last_run.clone();
                        let is_pending_delete = pending_delete().as_deref() == Some(r.id.as_str());
                        let is_editing_row = editing().as_deref() == Some(r.id.as_str());
                        let row_cls = match (enabled, is_editing_row) {
                            (_, true) => "routine-row editing",
                            (true, _) => "routine-row",
                            (false, _) => "routine-row off",
                        };
                        rsx! {
                            div { class: "{row_cls}",
                                div { class: "routine-name",
                                    span { class: "routine-title", "{r.name}" }
                                    span { class: "routine-prompt", "{r.intent}" }
                                }
                                span { class: "routine-sched",
                                    "{r.schedule}"
                                    if !provisioned {
                                        span { class: "routine-needs-setup", "needs setup" }
                                    }
                                }
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
                                    if provisioned {
                                        // Start / Stop arms or disarms the scheduler for this routine.
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
                                            if enabled { "Stop" } else { "Start" }
                                        }
                                    } else {
                                        // Imported routine: must be set up on this backend before it can run.
                                        button {
                                            class: "btn-restart btn-setup",
                                            onclick: move |_| {
                                                let id = id_provision.clone();
                                                spawn(async move {
                                                    if provision(&id).await.is_some() {
                                                        refresh += 1;
                                                    }
                                                });
                                            },
                                            "Set up"
                                        }
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
                                    button {
                                        class: "btn-edit-sm",
                                        onclick: move |_| {
                                            // Prefill the form with this routine and switch it to edit mode.
                                            let rt = r_edit.clone();
                                            let (f, t, d, wd, md) = parse_schedule(&rt.schedule);
                                            name.set(rt.name.clone());
                                            freq.set(f);
                                            sched_time.set(t);
                                            sched_date.set(d);
                                            weekdays.set(wd);
                                            monthday.set(md);
                                            intent.set(rt.intent.clone());
                                            prompt.set(rt.prompt.clone());
                                            scope.set(rt.scope.clone());
                                            authored_by.set(String::new());
                                            editing.set(Some(rt.id.clone()));
                                            pending_delete.set(None);
                                        },
                                        "Edit"
                                    }
                                    button {
                                        class: if is_pending_delete { "btn-delete-sm confirm" } else { "btn-delete-sm" },
                                        onclick: move |_| {
                                            let id = id_del.clone();
                                            if pending_delete().as_deref() == Some(id.as_str()) {
                                                // Second click — actually delete.
                                                pending_delete.set(None);
                                                spawn(async move {
                                                    if delete_routine(&id).await {
                                                        refresh += 1;
                                                    }
                                                });
                                            } else {
                                                // First click — arm the confirm.
                                                pending_delete.set(Some(id));
                                            }
                                        },
                                        if is_pending_delete { "Confirm?" } else { "Delete" }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            div { class: "routine-create",
                p { class: "section-label",
                    if editing().is_some() { "Edit routine" } else { "Add a routine" }
                }
                p { class: "section-hint", "Describe what you want the routine to do. Camerata's lead engineer drafts the operational prompt (model tiering, directives, scope) from it — you review and edit before it runs." }
                div { class: "routine-create-row",
                    input { class: "addressee-input", placeholder: "name", value: "{name}", oninput: move |e| name.set(e.value()) }
                    label { class: "sched-field sched-scope-field",
                        span { "Permissions" }
                        select {
                            class: "addressee-input",
                            value: "{scope}",
                            onchange: move |e| scope.set(e.value()),
                            option { value: "read-only", "Read-only — inspect & report, no file changes" }
                            option { value: "write (gated)", "Write — gated edits on a branch, no push" }
                            option { value: "write + open PR", "Write + open PR — gated edits, pushed for review" }
                        }
                    }
                }
                p { class: "section-hint sched-scope-hint",
                    "Permissions cap what the unattended run may do. "
                    b { "Read-only" }
                    " can analyze the repo but writes nothing. "
                    b { "Write" }
                    " lets it edit files on a working branch (every write still passes the governance gate) without pushing. "
                    b { "Write + open PR" }
                    " also pushes that branch and opens a pull request for your review. Nothing auto-merges."
                }
                // Structured schedule picker — frequency, then the controls that
                // frequency needs (weekday toggles / day-of-month / one-off date),
                // plus a time. Serialized to the schedule string on save.
                div { class: "sched-picker",
                    div { class: "sched-freq",
                        {
                            let opts = [("once", "One-off"), ("daily", "Daily"), ("weekly", "Weekly"), ("monthly", "Monthly")];
                            rsx! {
                                for (val, label) in opts.iter() {
                                    {
                                        let v = val.to_string();
                                        let on = freq() == *val;
                                        let cls = if on { "sched-freq-btn on" } else { "sched-freq-btn" };
                                        rsx! {
                                            button {
                                                key: "{val}",
                                                class: "{cls}",
                                                onclick: move |_| freq.set(v.clone()),
                                                "{label}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    div { class: "sched-detail",
                        // Weekly: per-day toggles.
                        if freq() == "weekly" {
                            div { class: "sched-dow",
                                for i in 0..7usize {
                                    {
                                        let on = weekdays().get(i).copied().unwrap_or(false);
                                        let cls = if on { "sched-dow-btn on" } else { "sched-dow-btn" };
                                        rsx! {
                                            button {
                                                key: "{i}",
                                                class: "{cls}",
                                                onclick: move |_| {
                                                    let mut w = weekdays();
                                                    if i < w.len() { w[i] = !w[i]; }
                                                    weekdays.set(w);
                                                },
                                                "{WEEKDAYS[i]}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        // Monthly: day-of-month.
                        if freq() == "monthly" {
                            label { class: "sched-field",
                                span { "Day of month" }
                                input {
                                    class: "addressee-input sched-num",
                                    r#type: "number", min: "1", max: "31",
                                    value: "{monthday}",
                                    oninput: move |e| {
                                        if let Ok(n) = e.value().parse::<u32>() {
                                            monthday.set(n.clamp(1, 31));
                                        }
                                    },
                                }
                            }
                        }
                        // One-off: a calendar date.
                        if freq() == "once" {
                            label { class: "sched-field",
                                span { "Date" }
                                input {
                                    class: "addressee-input",
                                    r#type: "date",
                                    value: "{sched_date}",
                                    oninput: move |e| sched_date.set(e.value()),
                                }
                            }
                        }
                        // Time applies to every frequency.
                        label { class: "sched-field",
                            span { "Time" }
                            input {
                                class: "addressee-input",
                                r#type: "time",
                                value: "{sched_time}",
                                oninput: move |e| sched_time.set(e.value()),
                            }
                        }
                    }
                    p { class: "sched-preview",
                        "Schedule: "
                        span { class: "sched-preview-val", "{build_schedule(&freq(), &sched_time(), &sched_date(), &weekdays(), monthday())}" }
                    }
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
                div { class: "routine-save-row",
                    button {
                        class: "btn-run",
                        onclick: move |_| {
                            let s = build_schedule(&freq(), &sched_time(), &sched_date(), &weekdays(), monthday());
                            let (n, i, p, sc) = (name(), intent(), prompt(), scope());
                            if n.is_empty() || i.trim().is_empty() {
                                return;
                            }
                            let edit_id = editing();
                            spawn(async move {
                                let ok = match &edit_id {
                                    Some(id) => update_routine(id, &n, &s, &i, &p, &sc).await.is_some(),
                                    None => create_routine(&n, &s, &i, &p, &sc).await.is_some(),
                                };
                                if ok {
                                    refresh += 1;
                                }
                            });
                            // Reset the form back to a fresh "create" state.
                            name.set(String::new());
                            intent.set(String::new());
                            prompt.set(String::new());
                            authored_by.set(String::new());
                            freq.set("daily".to_string());
                            sched_time.set("09:00".to_string());
                            sched_date.set(String::new());
                            weekdays.set(vec![false, true, false, false, false, false, false]);
                            monthday.set(1);
                            scope.set("read-only".to_string());
                            editing.set(None);
                        },
                        if editing().is_some() { "Save changes" } else { "Add routine" }
                    }
                    if editing().is_some() {
                        button {
                            class: "btn-restart",
                            onclick: move |_| {
                                // Cancel edit: clear the form and drop edit mode.
                                name.set(String::new());
                                intent.set(String::new());
                                prompt.set(String::new());
                                authored_by.set(String::new());
                                freq.set("daily".to_string());
                                sched_time.set("09:00".to_string());
                                sched_date.set(String::new());
                                weekdays.set(vec![false, true, false, false, false, false, false]);
                                monthday.set(1);
                                scope.set("read-only".to_string());
                                editing.set(None);
                            },
                            "Cancel"
                        }
                    }
                }
            }
        }
    }
}
