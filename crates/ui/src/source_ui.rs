//! The client source lifecycle surfaces: delete a client's code from this machine, fetch it back,
//! rescan only what changed, and the notes that explain what a deleted source disables.
//!
//! Pure decisions (which card state, when the delete button is enabled, the dialog's wording)
//! live in `camerata_ui_core::source` and are unit-tested there; this module is the Dioxus +
//! `rfd` + HTTP adapter around them (docs/design/2026-10-24_delete-and-refetch-client-source.md,
//! sections 6.6 to 6.9). The views are props-only (`DeleteSourceModalView`,
//! `RefetchDialogView`, `RecordBlock`) so they render in SSR tests with no server; the stateful
//! containers (`DangerZone`, `RefetchFlow`, `RescanChangesButton`, `DeletionRecordLink`) own the
//! signals and the BFF calls.
//!
//! Server contracts (all JSON; refusals are `{ ok:false, code, message }`):
//! - `GET  /api/projects/:id/source`
//! - `POST /api/projects/:id/repos/:repo/source/delete-preview`
//! - `POST /api/projects/:id/repos/:repo/source/delete`  body `{ confirm, include_uncommitted }`
//! - `GET  /api/projects/:id/source/records/:record/text`
//! - `GET  /api/projects/:id/repos/:repo/source/remote`
//! - `POST /api/projects/:id/repos/:repo/source/refetch`  body `{ target }`
//! - `POST /api/projects/:id/repos/:repo/rescan-changes`

use camerata_ui_core::readiness::{scan_block_text, ScanBlock};
use camerata_ui_core::source::{
    confirm_enabled, delta_header, refetch_options, RefetchChoice, SourceView,
};
use dioxus::prelude::*;
use serde::Deserialize;

use crate::cockpit::CockpitView;
use crate::readiness_gate::enc_repo;
use crate::toast::{push_toast, Toast, ToastKind};

// ── Wire types ────────────────────────────────────────────────────────────────

/// `GET /api/projects/:id/source`: every repo's source status plus the current inspection.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct SourceListing {
    #[serde(default)]
    pub repos: Vec<SourceView>,
    #[serde(default)]
    pub inspection: Option<InspectionStamp>,
}

/// The slice of the latest inspection the dialogs need.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct InspectionStamp {
    #[serde(default)]
    pub finished_at: String,
}

/// One line of the deletion plan: what will be removed and how much of it.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct PlanLine {
    pub label: String,
    pub path: String,
    pub counts: String,
}

/// `delete-preview`'s plan, as the confirmation modal shows it.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct DeletePlanView {
    #[serde(default)]
    pub item_view: Vec<PlanLine>,
    #[serde(default)]
    pub total_summary: String,
    /// Uncommitted changes and unpushed commits, each worded for the amber banner. Non-empty
    /// means the extra tick is required.
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub method: String,
}

/// A refusal or failure from the server, in the server's own plain sentence.
#[derive(Clone, Debug, PartialEq)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl ApiError {
    fn network() -> Self {
        Self {
            code: "network".to_string(),
            message: "Could not reach the Camerata server.".to_string(),
        }
    }

    fn unreadable() -> Self {
        Self {
            code: "unreadable".to_string(),
            message: "The server returned an unreadable response.".to_string(),
        }
    }

    /// Read `{ code, message }` off a refusal body.
    fn from_body(body: &serde_json::Value, fallback: &str) -> Self {
        Self {
            code: body
                .get("code")
                .and_then(|c| c.as_str())
                .unwrap_or("error")
                .to_string(),
            message: body
                .get("message")
                .or_else(|| body.get("error"))
                .and_then(|m| m.as_str())
                .unwrap_or(fallback)
                .to_string(),
        }
    }
}

/// What a finished deletion hands back: the record to show, and a note when some location could
/// not be removed.
#[derive(Clone, Debug, PartialEq)]
pub struct DeleteDone {
    pub record_id: String,
    pub partial_message: Option<String>,
}

/// What the remote probe found.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteProbe {
    pub default_branch: Option<String>,
    pub head_sha: String,
}

/// What a re-fetch ended in.
#[derive(Clone, Debug, PartialEq)]
pub struct RefetchDone {
    /// GitHub now calls the repository this; the project still records the old name.
    pub renamed_to: Option<String>,
    /// The inspected commit was asked for but could not be served; the latest source was fetched.
    pub notice: Option<String>,
}

/// What "Rescan changes" ended in.
#[derive(Clone, Debug, PartialEq)]
pub enum RescanStarted {
    /// Nothing differs from the inspection; no job was started.
    NothingChanged,
    /// A delta job is running; `header` is the scan surface's wording for it.
    Job { job_id: String, header: String },
}

// ── BFF calls ─────────────────────────────────────────────────────────────────

fn project_url(project_id: &str, tail: &str) -> String {
    format!("{}/api/projects/{}/{}", crate::bff_base(), project_id, tail)
}

fn repo_url(project_id: &str, repo: &str, tail: &str) -> String {
    project_url(project_id, &format!("repos/{}/{}", enc_repo(repo), tail))
}

/// Send a request and read the JSON body. `Ok` carries the body whatever `ok` says; the caller
/// decides what a refusal means for its call.
async fn call(request: reqwest::RequestBuilder) -> Result<serde_json::Value, ApiError> {
    let resp = request.send().await.map_err(|_| ApiError::network())?;
    resp.json().await.map_err(|_| ApiError::unreadable())
}

fn is_ok(body: &serde_json::Value) -> bool {
    body.get("ok").and_then(|b| b.as_bool()).unwrap_or(false)
}

/// `GET /api/projects/:id/source`; `None` on a transport failure or an unreadable body.
pub async fn fetch_source_listing(project_id: &str) -> Option<SourceListing> {
    let body = call(reqwest::Client::new().get(project_url(project_id, "source")))
        .await
        .ok()?;
    if !is_ok(&body) {
        return None;
    }
    serde_json::from_value(body).ok()
}

/// Run the deletion guard and get the plan, removing nothing.
pub async fn preview_delete(project_id: &str, repo: &str) -> Result<DeletePlanView, ApiError> {
    let body =
        call(reqwest::Client::new().post(repo_url(project_id, repo, "source/delete-preview")))
            .await?;
    if !is_ok(&body) {
        return Err(ApiError::from_body(
            &body,
            "The deletion could not be planned.",
        ));
    }
    body.get("plan")
        .cloned()
        .and_then(|p| serde_json::from_value(p).ok())
        .ok_or_else(ApiError::unreadable)
}

/// Delete the source. `confirm` is the text the operator typed; the server compares it to the
/// repo name itself, so the button gate in the UI is a convenience, never the guard.
pub async fn delete_source(
    project_id: &str,
    repo: &str,
    confirm: &str,
    include_uncommitted: bool,
) -> Result<DeleteDone, ApiError> {
    let body = call(
        reqwest::Client::new()
            .post(repo_url(project_id, repo, "source/delete"))
            .json(&serde_json::json!({
                "confirm": confirm,
                "include_uncommitted": include_uncommitted,
            })),
    )
    .await?;
    let record_id = body
        .get("record")
        .and_then(|r| r.get("id"))
        .and_then(|i| i.as_str())
        .map(str::to_string);
    match (is_ok(&body), record_id) {
        (true, Some(record_id)) => Ok(DeleteDone {
            record_id,
            partial_message: None,
        }),
        // A partial deletion still wrote its record: the record is the truth, show it.
        (false, Some(record_id))
            if body.get("code").and_then(|c| c.as_str()) == Some("partial") =>
        {
            Ok(DeleteDone {
                record_id,
                partial_message: body
                    .get("message")
                    .and_then(|m| m.as_str())
                    .map(str::to_string),
            })
        }
        _ => Err(ApiError::from_body(&body, "The deletion was refused.")),
    }
}

/// One deletion record as the plain text the client receives.
pub async fn fetch_record_text(project_id: &str, record_id: &str) -> Option<String> {
    let resp = reqwest::get(project_url(
        project_id,
        &format!("source/records/{record_id}/text"),
    ))
    .await
    .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.text().await.ok()
}

/// Ask the remote for its default branch and head (for the re-fetch dialog).
pub async fn probe_remote(project_id: &str, repo: &str) -> Result<RemoteProbe, ApiError> {
    let body =
        call(reqwest::Client::new().get(repo_url(project_id, repo, "source/remote"))).await?;
    if !is_ok(&body) {
        return Err(ApiError::from_body(&body, "GitHub could not be reached."));
    }
    let head_sha = body
        .get("head_sha")
        .and_then(|s| s.as_str())
        .ok_or_else(ApiError::unreadable)?
        .to_string();
    Ok(RemoteProbe {
        default_branch: body
            .get("default_branch")
            .and_then(|s| s.as_str())
            .map(str::to_string),
        head_sha,
    })
}

/// Clone the repo back. `inspected_sha` is only read for [`RefetchChoice::Inspected`].
pub async fn refetch_source(
    project_id: &str,
    repo: &str,
    choice: RefetchChoice,
    inspected_sha: Option<&str>,
) -> Result<RefetchDone, ApiError> {
    let target = match (choice, inspected_sha) {
        (RefetchChoice::Inspected, Some(sha)) => {
            serde_json::json!({ "kind": "commit", "sha": sha })
        }
        _ => serde_json::json!({ "kind": "latest" }),
    };
    let body = call(
        reqwest::Client::new()
            .post(repo_url(project_id, repo, "source/refetch"))
            .json(&serde_json::json!({ "target": target })),
    )
    .await?;
    let renamed_to = body
        .get("renamed_to")
        .and_then(|r| r.as_str())
        .map(str::to_string);
    if is_ok(&body) {
        return Ok(RefetchDone {
            renamed_to,
            notice: None,
        });
    }
    // The latest source IS in place; only the requested commit was unavailable.
    if body.get("code").and_then(|c| c.as_str()) == Some("commit_not_on_remote") {
        return Ok(RefetchDone {
            renamed_to,
            notice: body
                .get("message")
                .and_then(|m| m.as_str())
                .map(str::to_string),
        });
    }
    Err(ApiError::from_body(&body, "The re-fetch failed."))
}

/// Start the delta retest of everything that changed since the inspection.
pub async fn rescan_changes(project_id: &str, repo: &str) -> Result<RescanStarted, ApiError> {
    let body = call(
        reqwest::Client::new()
            .post(repo_url(project_id, repo, "rescan-changes"))
            .json(&serde_json::json!({})),
    )
    .await?;
    if !is_ok(&body) {
        return Err(ApiError::from_body(&body, "The rescan could not start."));
    }
    if body
        .get("nothing_changed")
        .and_then(|b| b.as_bool())
        .unwrap_or(false)
    {
        return Ok(RescanStarted::NothingChanged);
    }
    let job_id = body
        .get("job_id")
        .and_then(|j| j.as_str())
        .ok_or_else(ApiError::unreadable)?
        .to_string();
    let from = body.get("from_sha").and_then(|s| s.as_str()).unwrap_or("");
    let header = delta_header(
        from,
        body.get("commits_newer").and_then(|c| c.as_u64()),
        body.get("changed_files")
            .and_then(|c| c.as_u64())
            .unwrap_or(0),
    );
    Ok(RescanStarted::Job { job_id, header })
}

// ── Clipboard and file helpers ────────────────────────────────────────────────

/// The eval statement that copies `text` to the system clipboard. The text is embedded as a JSON
/// string literal so quotes, newlines and unicode round-trip verbatim. A hidden textarea plus
/// `execCommand('copy')` is the primary path (it works in the desktop webview, where the async
/// clipboard API is not available); the async API is the fallback. Pure.
pub(crate) fn copy_script(text: &str) -> Option<String> {
    let literal = serde_json::to_string(text).ok()?;
    Some(format!(
        "var t = {literal}; \
         var a = document.createElement('textarea'); \
         a.value = t; a.setAttribute('readonly', ''); \
         a.style.position = 'fixed'; a.style.opacity = '0'; \
         document.body.appendChild(a); a.select(); \
         var ok = false; \
         try {{ ok = document.execCommand('copy'); }} catch (e) {{ ok = false; }} \
         document.body.removeChild(a); \
         if (!ok && navigator.clipboard && navigator.clipboard.writeText) {{ \
           navigator.clipboard.writeText(t); ok = true; \
         }} \
         return ok;"
    ))
}

/// Copy `text` to the clipboard; `true` when something took it.
pub(crate) async fn copy_to_clipboard(text: &str) -> bool {
    let Some(script) = copy_script(text) else {
        return false;
    };
    matches!(
        dioxus::document::eval(&script).await,
        Ok(serde_json::Value::Bool(true))
    )
}

/// Save `text` through the native save dialog; `Ok(false)` when the operator cancelled.
async fn save_text(file_name: &str, text: &str) -> Result<bool, ()> {
    match rfd::AsyncFileDialog::new()
        .set_file_name(file_name)
        .save_file()
        .await
    {
        Some(file) => file
            .write(text.as_bytes())
            .await
            .map(|_| true)
            .map_err(|_| ()),
        None => Ok(false),
    }
}

/// Toast context with a private fallback so the containers render in tests with no app shell.
fn use_toasts() -> Signal<Vec<Toast>> {
    let fallback = use_signal(Vec::<Toast>::new);
    try_consume_context::<Signal<Vec<Toast>>>().unwrap_or(fallback)
}

/// Copy and save the record through the shared helpers, reporting by toast.
fn record_actions(
    toasts: Signal<Vec<Toast>>,
    record_id: String,
    text: String,
) -> (impl FnMut(()) + 'static, impl FnMut(()) + 'static) {
    let copy_text = text.clone();
    let copy = move |()| {
        let text = copy_text.clone();
        spawn(async move {
            if copy_to_clipboard(&text).await {
                push_toast(toasts, ToastKind::Info, "Copied the deletion record.");
            } else {
                push_toast(
                    toasts,
                    ToastKind::Error,
                    "Could not copy the record. Use Save record as .txt instead.",
                );
            }
        });
    };
    let save = move |()| {
        let (name, text) = (format!("{record_id}.txt"), text.clone());
        spawn(async move {
            match save_text(&name, &text).await {
                Ok(true) => push_toast(toasts, ToastKind::Info, format!("Saved {name}.")),
                Ok(false) => {}
                Err(()) => push_toast(toasts, ToastKind::Error, "Could not save the record."),
            }
        });
    };
    (copy, save)
}

// ── Navigation helpers ────────────────────────────────────────────────────────

/// Jump to another cockpit tab; a no-op outside the cockpit (tests).
fn go_to(view: Option<Signal<CockpitView>>, target: CockpitView) {
    if let Some(mut view) = view {
        view.set(target);
    }
}

// ── Record block (shared by the delete result and "View deletion record") ─────

/// The deletion record as text, with Copy and Save.
#[component]
pub fn RecordBlock(text: String, on_copy: EventHandler<()>, on_save: EventHandler<()>) -> Element {
    rsx! {
        div { class: "src-record",
            pre { class: "src-record-text", "{text}" }
            div { class: "src-record-actions",
                button { class: "btn-edit-sm", onclick: move |_| on_copy.call(()), "Copy record" }
                button { class: "btn-edit-sm", onclick: move |_| on_save.call(()), "Save record as .txt" }
            }
        }
    }
}

// ── Delete modal (props-only) ─────────────────────────────────────────────────

/// The "Delete <repo> from this machine" modal: plan, warnings, typed confirmation, result.
///
/// `plan` is `None` while the preview runs. `result` is `Some(record text)` once the deletion
/// ran; the modal then shows the record instead of the form. `refusal` is the server's sentence
/// for a refused preview or delete (red, nothing else changes).
#[component]
pub fn DeleteSourceModalView(
    repo: String,
    plan: Option<DeletePlanView>,
    typed: String,
    ticked: bool,
    busy: bool,
    refusal: Option<String>,
    result: Option<String>,
    result_note: Option<String>,
    on_typed: EventHandler<String>,
    on_tick: EventHandler<bool>,
    on_confirm: EventHandler<()>,
    on_cancel: EventHandler<()>,
    on_done: EventHandler<()>,
    on_copy: EventHandler<()>,
    on_save: EventHandler<()>,
) -> Element {
    let warnings_present = plan.as_ref().is_some_and(|p| !p.warnings.is_empty());
    let enabled = plan.is_some() && confirm_enabled(&typed, &repo, warnings_present, ticked);
    rsx! {
        div { class: "rule-modal-overlay",
            div { class: "rule-modal src-modal", onclick: move |e| e.stop_propagation(),
                div { class: "rule-modal-head",
                    span { class: "rule-modal-id", "Delete {repo} from this machine" }
                    if result.is_none() {
                        button {
                            class: "rule-modal-close",
                            disabled: busy,
                            onclick: move |_| on_cancel.call(()),
                            "\u{2715}"
                        }
                    }
                }
                if let Some(text) = result.clone() {
                    p { class: "rule-modal-detail",
                        "The source code was removed from this machine. Keep this record as your proof."
                    }
                    if let Some(note) = result_note.clone() {
                        p { class: "src-refusal", "{note}" }
                    }
                    RecordBlock { text, on_copy, on_save }
                    div { class: "src-modal-foot",
                        button { class: "btn-run", onclick: move |_| on_done.call(()), "Done" }
                    }
                } else {
                    match plan.clone() {
                        None => rsx! {
                            if refusal.is_none() {
                                p { class: "rule-modal-detail", "Checking what will be removed\u{2026}" }
                            }
                        },
                        Some(plan) => rsx! {
                            p { class: "rule-modal-detail",
                                "Camerata will remove the checkout and every other copy of this repository's code it holds on this machine. Findings, reports and exports stay."
                            }
                            ul { class: "src-plan",
                                for line in plan.item_view.iter() {
                                    li { key: "{line.label}{line.path}",
                                        span { class: "src-plan-label", "{line.label}" }
                                        span { class: "src-plan-path", "{line.path}" }
                                        span { class: "src-plan-counts", "{line.counts}" }
                                    }
                                }
                            }
                            p { class: "src-plan-total", "Total: {plan.total_summary}" }
                            for w in plan.warnings.iter() {
                                p { key: "{w}", class: "src-warning", "{w}" }
                            }
                            if warnings_present {
                                label { class: "src-tick",
                                    input {
                                        r#type: "checkbox",
                                        checked: ticked,
                                        disabled: busy,
                                        onchange: move |e| on_tick.call(e.checked()),
                                    }
                                    span { "Delete uncommitted work too" }
                                }
                            }
                            if !plan.method.is_empty() {
                                p { class: "src-method", "{plan.method}" }
                            }
                            label { class: "src-confirm-label",
                                "Type "
                                code { "{repo}" }
                                " to confirm"
                            }
                            input {
                                class: "addressee-input src-confirm-input",
                                placeholder: "{repo}",
                                value: "{typed}",
                                disabled: busy,
                                oninput: move |e| on_typed.call(e.value()),
                            }
                        },
                    }
                    if let Some(message) = refusal.clone() {
                        p { class: "src-refusal", "{message}" }
                    }
                    div { class: "src-modal-foot",
                        button {
                            class: "btn-edit-sm",
                            disabled: busy,
                            onclick: move |_| on_cancel.call(()),
                            "Cancel"
                        }
                        button {
                            class: "btn-solid-danger",
                            disabled: !enabled || busy,
                            onclick: move |_| on_confirm.call(()),
                            if busy { "Deleting\u{2026}" } else { "Delete source code" }
                        }
                    }
                }
            }
        }
    }
}

/// The collapsed "Danger zone" of a card whose source is on disk, and the modal it opens.
#[component]
pub fn DangerZone(project_id: String, repo: String, on_changed: EventHandler<()>) -> Element {
    let toasts = use_toasts();
    let mut open = use_signal(|| false);
    let mut plan = use_signal(|| Option::<DeletePlanView>::None);
    let mut typed = use_signal(String::new);
    let mut ticked = use_signal(|| false);
    let mut busy = use_signal(|| false);
    let mut refusal = use_signal(|| Option::<String>::None);
    let mut result = use_signal(|| Option::<(String, String)>::None); // (record id, text)
    let mut result_note = use_signal(|| Option::<String>::None);

    let start_preview = {
        let (pid, rp) = (project_id.clone(), repo.clone());
        move |_| {
            let (pid, rp) = (pid.clone(), rp.clone());
            plan.set(None);
            typed.set(String::new());
            ticked.set(false);
            refusal.set(None);
            result.set(None);
            result_note.set(None);
            open.set(true);
            spawn(async move {
                match preview_delete(&pid, &rp).await {
                    Ok(p) => plan.set(Some(p)),
                    Err(e) => refusal.set(Some(e.message)),
                }
            });
        }
    };

    let confirm = {
        let (pid, rp) = (project_id.clone(), repo.clone());
        move |()| {
            let (pid, rp, text, include) = (pid.clone(), rp.clone(), typed(), ticked());
            busy.set(true);
            refusal.set(None);
            spawn(async move {
                match delete_source(&pid, &rp, &text, include).await {
                    Ok(done) => {
                        let record = fetch_record_text(&pid, &done.record_id)
                            .await
                            .unwrap_or_else(|| {
                                format!(
                                    "The record {} was written, but could not be read back.",
                                    done.record_id
                                )
                            });
                        result_note.set(done.partial_message);
                        result.set(Some((done.record_id, record)));
                    }
                    Err(e) => refusal.set(Some(e.message)),
                }
                busy.set(false);
            });
        }
    };

    let (record_id, record_text) = result().unwrap_or_default();
    let (copy, save) = record_actions(toasts, record_id, record_text.clone());
    let finished = result().is_some();

    rsx! {
        details { class: "ws-danger",
            summary { class: "ws-danger-summary", "Danger zone" }
            p { class: "ws-danger-text",
                "Removes the checkout and every other copy Camerata holds. Findings and exports stay."
            }
            button { class: "btn-danger-outline", onclick: start_preview, "Delete source code from this machine" }
        }
        if open() {
            DeleteSourceModalView {
                repo: repo.clone(),
                plan: plan(),
                typed: typed(),
                ticked: ticked(),
                busy: busy(),
                refusal: refusal(),
                result: finished.then_some(record_text),
                result_note: result_note(),
                on_typed: move |v| typed.set(v),
                on_tick: move |v| ticked.set(v),
                on_confirm: confirm,
                on_cancel: move |_| {
                    open.set(false);
                    if finished {
                        on_changed.call(());
                    }
                },
                on_done: move |_| {
                    open.set(false);
                    on_changed.call(());
                },
                on_copy: copy,
                on_save: save,
            }
        }
    }
}

// ── Re-fetch dialog ───────────────────────────────────────────────────────────

/// Where the re-fetch dialog is in asking GitHub what is there.
#[derive(Clone, Debug, PartialEq)]
pub enum ProbeState {
    Loading,
    Ready(RemoteProbe),
    Failed(ApiError),
}

/// The re-fetch dialog: latest source or exactly the inspected commit, and where it lands.
#[component]
pub fn RefetchDialogView(
    repo: String,
    destination: String,
    probe: ProbeState,
    inspected_sha: Option<String>,
    inspected_at: Option<String>,
    choice: RefetchChoice,
    busy: bool,
    error: Option<String>,
    on_choice: EventHandler<RefetchChoice>,
    on_retry: EventHandler<()>,
    on_add_token: EventHandler<()>,
    on_confirm: EventHandler<()>,
    on_cancel: EventHandler<()>,
) -> Element {
    let ready = matches!(probe, ProbeState::Ready(_));
    rsx! {
        div { class: "rule-modal-overlay",
            div { class: "rule-modal src-modal", onclick: move |e| e.stop_propagation(),
                div { class: "rule-modal-head",
                    span { class: "rule-modal-id", "Re-fetch {repo}" }
                    button {
                        class: "rule-modal-close",
                        disabled: busy,
                        onclick: move |_| on_cancel.call(()),
                        "\u{2715}"
                    }
                }
                match probe.clone() {
                    ProbeState::Loading => rsx! {
                        p { class: "rule-modal-detail", "Checking GitHub\u{2026}" }
                    },
                    ProbeState::Failed(e) => rsx! {
                        p { class: "src-refusal", "{e.message}" }
                        div { class: "src-modal-foot",
                            if e.code == "no_token" {
                                button { class: "btn-edit-sm", onclick: move |_| on_add_token.call(()), "Add a token" }
                            }
                            button { class: "btn-edit-sm", onclick: move |_| on_retry.call(()), "Retry" }
                        }
                    },
                    ProbeState::Ready(remote) => {
                        let options = refetch_options(
                            remote.default_branch.as_deref(),
                            &remote.head_sha,
                            inspected_sha.as_deref(),
                            inspected_at.as_deref(),
                        );
                        rsx! {
                            label { class: "src-radio",
                                input {
                                    r#type: "radio",
                                    name: "refetch-choice",
                                    checked: choice == RefetchChoice::Latest,
                                    disabled: busy,
                                    onchange: move |_| on_choice.call(RefetchChoice::Latest),
                                }
                                span { "{options.latest_label}" }
                            }
                            if let Some(label) = options.inspected_label.clone() {
                                label { class: "src-radio",
                                    input {
                                        r#type: "radio",
                                        name: "refetch-choice",
                                        checked: choice == RefetchChoice::Inspected,
                                        disabled: busy,
                                        onchange: move |_| on_choice.call(RefetchChoice::Inspected),
                                    }
                                    span { "{label}" }
                                }
                                p { class: "src-method", "{options.inspected_hint}" }
                            }
                        }
                    }
                }
                p { class: "src-destination", "Into {destination}" }
                if let Some(message) = error.clone() {
                    p { class: "src-refusal", "{message}" }
                }
                div { class: "src-modal-foot",
                    button {
                        class: "btn-edit-sm",
                        disabled: busy,
                        onclick: move |_| on_cancel.call(()),
                        "Cancel"
                    }
                    button {
                        class: "btn-run",
                        disabled: !ready || busy,
                        onclick: move |_| on_confirm.call(()),
                        if busy { "Cloning\u{2026}" } else { "Re-fetch" }
                    }
                }
            }
        }
    }
}

/// The "Re-fetch source" primary button of a deleted card and its dialog.
#[component]
pub fn RefetchFlow(
    project_id: String,
    repo: String,
    destination: String,
    inspected_sha: Option<String>,
    inspected_at: Option<String>,
    on_done: EventHandler<RefetchDone>,
) -> Element {
    let nav = try_consume_context::<Signal<CockpitView>>();
    let mut open = use_signal(|| false);
    let mut probe = use_signal(|| ProbeState::Loading);
    let mut choice = use_signal(|| RefetchChoice::Latest);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| Option::<String>::None);

    let mut run_probe = {
        let (pid, rp) = (project_id.clone(), repo.clone());
        move || {
            let (pid, rp) = (pid.clone(), rp.clone());
            probe.set(ProbeState::Loading);
            spawn(async move {
                probe.set(match probe_remote(&pid, &rp).await {
                    Ok(found) => ProbeState::Ready(found),
                    Err(e) => ProbeState::Failed(e),
                });
            });
        }
    };

    let open_dialog = {
        let mut run_probe = run_probe.clone();
        move |_| {
            choice.set(RefetchChoice::Latest);
            error.set(None);
            open.set(true);
            run_probe();
        }
    };

    let confirm = {
        let (pid, rp, sha) = (project_id.clone(), repo.clone(), inspected_sha.clone());
        move |()| {
            let (pid, rp, sha, picked) = (pid.clone(), rp.clone(), sha.clone(), choice());
            busy.set(true);
            error.set(None);
            spawn(async move {
                match refetch_source(&pid, &rp, picked, sha.as_deref()).await {
                    Ok(done) => {
                        open.set(false);
                        on_done.call(done);
                    }
                    Err(e) => error.set(Some(e.message)),
                }
                busy.set(false);
            });
        }
    };

    rsx! {
        button { class: "btn-run", onclick: open_dialog, "Re-fetch source" }
        if open() {
            RefetchDialogView {
                repo: repo.clone(),
                destination: destination.clone(),
                probe: probe(),
                inspected_sha: inspected_sha.clone(),
                inspected_at: inspected_at.clone(),
                choice: choice(),
                busy: busy(),
                error: error(),
                on_choice: move |c| choice.set(c),
                on_retry: move |_| run_probe(),
                on_add_token: move |_| {
                    open.set(false);
                    go_to(nav, CockpitView::Credentials);
                },
                on_confirm: confirm,
                on_cancel: move |_| open.set(false),
            }
        }
    }
}

// ── Deletion record link ──────────────────────────────────────────────────────

/// "View deletion record": opens the record's text in a modal with Copy and Save.
#[component]
pub fn DeletionRecordLink(project_id: String, record_id: String) -> Element {
    let toasts = use_toasts();
    let mut open = use_signal(|| false);
    let mut text = use_signal(|| Option::<String>::None);
    let mut failed = use_signal(|| false);

    let show = {
        let (pid, rid) = (project_id.clone(), record_id.clone());
        move |_| {
            let (pid, rid) = (pid.clone(), rid.clone());
            open.set(true);
            failed.set(false);
            spawn(async move {
                match fetch_record_text(&pid, &rid).await {
                    Some(t) => text.set(Some(t)),
                    None => failed.set(true),
                }
            });
        }
    };
    let shown = text().unwrap_or_default();
    let (copy, save) = record_actions(toasts, record_id.clone(), shown.clone());

    rsx! {
        button { class: "btn-edit-sm", onclick: show, "View deletion record" }
        if open() {
            div { class: "rule-modal-overlay", onclick: move |_| open.set(false),
                div { class: "rule-modal src-modal", onclick: move |e| e.stop_propagation(),
                    div { class: "rule-modal-head",
                        span { class: "rule-modal-id", "Deletion record" }
                        button { class: "rule-modal-close", onclick: move |_| open.set(false), "\u{2715}" }
                    }
                    if failed() {
                        p { class: "src-refusal", "The record could not be loaded." }
                    } else if text().is_none() {
                        p { class: "rule-modal-detail", "Loading\u{2026}" }
                    } else {
                        RecordBlock { text: shown, on_copy: copy, on_save: save }
                    }
                }
            }
        }
    }
}

// ── Rescan changes ────────────────────────────────────────────────────────────

/// The primary action of a card whose source is newer than the inspection: retest only what
/// changed. On start the job is handed to the Onboard review (the same poller every audit job
/// uses) and the cockpit switches there.
#[component]
pub fn RescanChangesButton(project_id: String, repo: String) -> Element {
    let toasts = use_toasts();
    let nav = try_consume_context::<Signal<CockpitView>>();
    let job = try_consume_context::<Signal<Option<String>>>();
    let mut busy = use_signal(|| false);
    rsx! {
        button {
            class: "btn-run",
            disabled: busy(),
            title: "Retest only the files that changed since the inspection",
            onclick: move |_| {
                let (pid, rp) = (project_id.clone(), repo.clone());
                busy.set(true);
                spawn(async move {
                    match rescan_changes(&pid, &rp).await {
                        Ok(RescanStarted::NothingChanged) => push_toast(
                            toasts,
                            ToastKind::Info,
                            "Nothing changed since the inspected commit.",
                        ),
                        Ok(RescanStarted::Job { job_id, header }) => {
                            if let Some(mut job) = job {
                                job.set(Some(job_id));
                            }
                            push_toast(toasts, ToastKind::Info, header);
                            go_to(nav, CockpitView::Onboard);
                        }
                        Err(e) => push_toast(toasts, ToastKind::Error, e.message),
                    }
                    busy.set(false);
                });
            },
            if busy() { "Starting\u{2026}" } else { "Rescan changes" }
        }
    }
}

/// "Full re-inspection": the existing scan, on the Onboard tab.
#[component]
pub fn FullReinspectionButton(primary: bool) -> Element {
    let nav = try_consume_context::<Signal<CockpitView>>();
    rsx! {
        button {
            class: if primary { "btn-run" } else { "btn-edit-sm" },
            onclick: move |_| go_to(nav, CockpitView::Onboard),
            "Full re-inspection"
        }
    }
}

// ── What a deleted source disables ────────────────────────────────────────────

/// The scan block the cockpit shares with every surface, `None` while no repo's source is
/// deleted (or outside the cockpit).
pub fn use_scan_block() -> Option<ScanBlock> {
    try_consume_context::<Memo<Option<ScanBlock>>>().and_then(|block| block())
}

/// The explanation beside every control a deleted source disables, with the way back.
#[component]
pub fn ScanBlockNotice(block: ScanBlock) -> Element {
    let nav = try_consume_context::<Signal<CockpitView>>();
    let text = scan_block_text(&block);
    rsx! {
        p { class: "scan-block-hint",
            span { "{text}" }
            button {
                class: "btn-edit-sm scan-block-link",
                onclick: move |_| go_to(nav, CockpitView::Workspace),
                "Re-fetch source"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_script_embeds_the_text_as_one_escaped_literal() {
        let script = copy_script("line1\n\"quoted\" </script>").expect("encodes");
        assert!(
            script.contains(r#"var t = "line1\n\"quoted\" </script>";"#),
            "{script}"
        );
        assert!(script.ends_with("return ok;"));
    }

    #[test]
    fn a_refusal_reads_code_and_message_off_the_body() {
        let body = serde_json::json!({
            "ok": false,
            "code": "dirty_tree",
            "message": "Commit or push first.",
        });
        let e = ApiError::from_body(&body, "fallback");
        assert_eq!(e.code, "dirty_tree");
        assert_eq!(e.message, "Commit or push first.");
        let bare = ApiError::from_body(&serde_json::json!({}), "fallback");
        assert_eq!(bare.message, "fallback");
    }

    #[test]
    fn the_plan_deserializes_from_the_preview_body() {
        let plan: DeletePlanView = serde_json::from_value(serde_json::json!({
            "repo": "acme/app",
            "item_view": [{ "label": "checkout", "path": "/w/acme/app", "counts": "12 files, 3.4 KB" }],
            "total_summary": "12 files, 3.4 KB",
            "warnings": ["This checkout has uncommitted changes (2 items)"],
            "method": "Removed with remove_dir_all.",
            "items": [],
        }))
        .unwrap();
        assert_eq!(plan.item_view[0].label, "checkout");
        assert_eq!(plan.warnings.len(), 1);
    }

    // ── SSR render tests (props-only views) ───────────────────────────────────────

    fn plan(warnings: &[&str]) -> DeletePlanView {
        DeletePlanView {
            item_view: vec![
                PlanLine {
                    label: "checkout".to_string(),
                    path: "/w/acme/app".to_string(),
                    counts: "2,729 files, 182.9 MB".to_string(),
                },
                PlanLine {
                    label: "model session file".to_string(),
                    path: "/h/.claude/projects/x/abc.jsonl".to_string(),
                    counts: "12.0 KB".to_string(),
                },
            ],
            total_summary: "2,730 files, 182.9 MB".to_string(),
            warnings: warnings.iter().map(|w| (*w).to_string()).collect(),
            method: "Removed with remove_dir_all.".to_string(),
        }
    }

    #[derive(Clone, PartialEq, Props)]
    struct ModalCase {
        typed: String,
        warnings: Vec<String>,
        ticked: bool,
        refusal: Option<String>,
        loaded: bool,
    }

    fn render_modal(case: ModalCase) -> String {
        fn harness(case: ModalCase) -> Element {
            let warnings: Vec<&str> = case.warnings.iter().map(String::as_str).collect();
            rsx! {
                DeleteSourceModalView {
                    repo: "acme/app".to_string(),
                    plan: case.loaded.then(|| plan(&warnings)),
                    typed: case.typed.clone(),
                    ticked: case.ticked,
                    busy: false,
                    refusal: case.refusal.clone(),
                    result: None,
                    result_note: None,
                    on_typed: move |_| {},
                    on_tick: move |_| {},
                    on_confirm: move |_| {},
                    on_cancel: move |_| {},
                    on_done: move |_| {},
                    on_copy: move |_| {},
                    on_save: move |_| {},
                }
            }
        }
        let mut vdom = VirtualDom::new_with_props(harness, case);
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    }

    /// The opening tag of the destructive button, where `disabled` shows up when it is set.
    fn delete_button_tag(html: &str) -> String {
        let at = html
            .find("btn-solid-danger")
            .expect("the delete button renders");
        let start = html[..at].rfind("<button").expect("inside a button");
        let end = html[at..].find('>').expect("tag closes") + at;
        html[start..=end].to_string()
    }

    fn case(typed: &str, warnings: &[&str], ticked: bool) -> ModalCase {
        ModalCase {
            typed: typed.to_string(),
            warnings: warnings.iter().map(|w| (*w).to_string()).collect(),
            ticked,
            refusal: None,
            loaded: true,
        }
    }

    #[test]
    fn danger_modal_confirm_disabled_until_exact_name() {
        let wrong = [
            "",
            "acme/ap",
            "acme/app ",
            " acme/app",
            "ACME/APP",
            "acme/app2",
        ];
        for typed in wrong {
            let html = render_modal(case(typed, &[], false));
            let tag = delete_button_tag(&html);
            assert!(
                tag.contains("disabled"),
                "typed {typed:?} must not enable: {tag}"
            );
        }
        let html = render_modal(case("acme/app", &[], false));
        let tag = delete_button_tag(&html);
        assert!(
            !tag.contains("disabled"),
            "the exact name enables it: {tag}"
        );
        assert!(html.contains("Delete acme/app from this machine"));
        // The plan lists each location with its counts, and the totals.
        assert!(html.contains("2,729 files, 182.9 MB"));
        assert!(html.contains("model session file"));
        assert!(html.contains("Total: 2,730 files, 182.9 MB"));
        assert!(html.contains("Removed with remove_dir_all."));
    }

    #[test]
    fn danger_modal_shows_tick_only_when_warnings_present() {
        let clean = render_modal(case("acme/app", &[], false));
        assert!(!clean.contains("Delete uncommitted work too"), "{clean}");

        let warned = ["This checkout has uncommitted changes (3 items)"];
        let unticked = render_modal(case("acme/app", &warned, false));
        assert!(unticked.contains("Delete uncommitted work too"));
        assert!(unticked.contains("uncommitted changes (3 items)"));
        // The right name is not enough while the warning stands.
        assert!(delete_button_tag(&unticked).contains("disabled"));

        let ticked = render_modal(case("acme/app", &warned, true));
        assert!(!delete_button_tag(&ticked).contains("disabled"));
        // And the tick alone does not enable it either.
        let tick_only = render_modal(case("", &warned, true));
        assert!(delete_button_tag(&tick_only).contains("disabled"));
    }

    #[test]
    fn danger_modal_renders_refusal_message() {
        let mut refused = case("acme/app", &[], false);
        refused.loaded = false;
        refused.refusal = Some(
            "acme/app has no remote called origin, so Camerata cannot prove it is on GitHub."
                .to_string(),
        );
        let html = render_modal(refused);
        assert!(html.contains("src-refusal"), "{html}");
        assert!(html.contains("cannot prove it is on GitHub"));
        // No plan loaded means nothing can be confirmed.
        assert!(delete_button_tag(&html).contains("disabled"));
        assert!(!html.contains("Checking what will be removed"));
    }

    #[test]
    fn danger_modal_shows_the_record_after_deletion() {
        fn harness() -> Element {
            rsx! {
                DeleteSourceModalView {
                    repo: "acme/app".to_string(),
                    plan: None,
                    typed: String::new(),
                    ticked: false,
                    busy: false,
                    refusal: None,
                    result: Some("Camerata source deletion record\nRepository  acme/app".to_string()),
                    result_note: None,
                    on_typed: move |_| {},
                    on_tick: move |_| {},
                    on_confirm: move |_| {},
                    on_cancel: move |_| {},
                    on_done: move |_| {},
                    on_copy: move |_| {},
                    on_save: move |_| {},
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("Camerata source deletion record"));
        assert!(html.contains("Copy record"));
        assert!(html.contains("Save record as .txt"));
        assert!(html.contains(">Done<"));
        assert!(
            !html.contains("btn-solid-danger"),
            "the form is gone once it ran"
        );
    }

    fn render_refetch(probe: ProbeState, inspected: Option<&str>) -> String {
        #[derive(Clone, PartialEq, Props)]
        struct Case {
            probe: ProbeState,
            inspected: Option<String>,
        }
        fn harness(case: Case) -> Element {
            rsx! {
                RefetchDialogView {
                    repo: "acme/app".to_string(),
                    destination: "/w/acme/app".to_string(),
                    probe: case.probe.clone(),
                    inspected_sha: case.inspected.clone(),
                    inspected_at: Some("2026-10-24T10:05:00Z".to_string()),
                    choice: RefetchChoice::Latest,
                    busy: false,
                    error: None,
                    on_choice: move |_| {},
                    on_retry: move |_| {},
                    on_add_token: move |_| {},
                    on_confirm: move |_| {},
                    on_cancel: move |_| {},
                }
            }
        }
        let mut vdom = VirtualDom::new_with_props(
            harness,
            Case {
                probe,
                inspected: inspected.map(str::to_string),
            },
        );
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    }

    #[test]
    fn refetch_dialog_offers_latest_and_the_inspected_commit() {
        let head = "9b7e21c0000bbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let inspected = "3f2a9c14e0b7aaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let html = render_refetch(
            ProbeState::Ready(RemoteProbe {
                default_branch: Some("main".to_string()),
                head_sha: head.to_string(),
            }),
            Some(inspected),
        );
        assert!(html.contains("Latest from main (9b7e21c)"), "{html}");
        assert!(html.contains("The inspected commit 3f2a9c1, 24 Oct 2026"));
        assert!(html.contains("The clone still has full history."));
        assert!(html.contains("Into /w/acme/app"));
        assert!(html.contains(">Re-fetch<"));
    }

    #[test]
    fn refetch_dialog_waits_for_github_and_reports_its_errors() {
        let loading = render_refetch(ProbeState::Loading, None);
        assert!(loading.contains("Checking GitHub"));
        assert!(
            !loading.contains("type=\"radio\""),
            "no radios until GitHub answers"
        );

        let failed = render_refetch(
            ProbeState::Failed(ApiError {
                code: "no_token".to_string(),
                message: "No GitHub token is set. Add one under Credentials.".to_string(),
            }),
            None,
        );
        assert!(failed.contains("No GitHub token is set."));
        assert!(failed.contains("Add a token"));
        assert!(failed.contains("Retry"));

        let other = render_refetch(
            ProbeState::Failed(ApiError {
                code: "network".to_string(),
                message: "could not resolve host".to_string(),
            }),
            None,
        );
        assert!(
            !other.contains("Add a token"),
            "only a missing token links to Credentials"
        );
        assert!(other.contains("Retry"));
    }

    // ── wiremock: the BFF calls read the server's contracts ──────────────────────

    async fn mounted(
        http_method: &str,
        route: &str,
        status: u16,
        body: serde_json::Value,
    ) -> (wiremock::MockServer, camerata_test_env::EnvGuard) {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method(http_method))
            .and(path(route))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(&server)
            .await;
        let env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        (server, env)
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn preview_delete_returns_the_plan_or_the_servers_refusal() {
        let (_server, _env) = mounted(
            "POST",
            "/api/projects/p1/repos/acme%2Fapp/source/delete-preview",
            200,
            serde_json::json!({ "ok": true, "plan": {
                "item_view": [{ "label": "checkout", "path": "/w", "counts": "1 files, 1 B" }],
                "total_summary": "1 files, 1 B", "warnings": [], "method": "m",
            }}),
        )
        .await;
        let plan = preview_delete("p1", "acme/app").await.expect("plan");
        assert_eq!(plan.item_view.len(), 1);

        let (_server, _env) = mounted(
            "POST",
            "/api/projects/p1/repos/acme%2Fapp/source/delete-preview",
            409,
            serde_json::json!({ "ok": false, "code": "scan_in_progress",
                "message": "A scan of acme/app is running. Wait for it to finish." }),
        )
        .await;
        let err = preview_delete("p1", "acme/app").await.expect_err("refused");
        assert_eq!(err.code, "scan_in_progress");
        assert!(err.message.contains("Wait for it to finish"));
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn delete_source_sends_the_typed_text_and_reads_the_record_id() {
        let (server, _env) = mounted(
            "POST",
            "/api/projects/p1/repos/acme%2Fapp/source/delete",
            200,
            serde_json::json!({ "ok": true, "record": { "id": "del-1" }, "status": {} }),
        )
        .await;
        let done = delete_source("p1", "acme/app", "acme/app", true)
            .await
            .expect("deleted");
        assert_eq!(done.record_id, "del-1");
        assert_eq!(done.partial_message, None);
        let sent: serde_json::Value =
            serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
        assert_eq!(sent["confirm"], "acme/app");
        assert_eq!(sent["include_uncommitted"], true);

        // A partial deletion still wrote its record: the modal shows it, with the warning.
        let (_server, _env) = mounted(
            "POST",
            "/api/projects/p1/repos/acme%2Fapp/source/delete",
            200,
            serde_json::json!({ "ok": false, "code": "partial", "record": { "id": "del-2" },
                "message": "1 location could not be removed; see the record." }),
        )
        .await;
        let partial = delete_source("p1", "acme/app", "acme/app", false)
            .await
            .expect("record");
        assert_eq!(partial.record_id, "del-2");
        assert!(partial
            .partial_message
            .unwrap()
            .contains("could not be removed"));

        // A refusal has no record and surfaces the sentence.
        let (_server, _env) = mounted(
            "POST",
            "/api/projects/p1/repos/acme%2Fapp/source/delete",
            409,
            serde_json::json!({ "ok": false, "code": "confirmation_mismatch",
                "message": "The typed name does not match." }),
        )
        .await;
        let err = delete_source("p1", "acme/app", "nope", false)
            .await
            .expect_err("refused");
        assert_eq!(err.message, "The typed name does not match.");
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn refetch_maps_latest_commit_and_the_unreachable_commit_notice() {
        let (server, _env) = mounted(
            "POST",
            "/api/projects/p1/repos/acme%2Fapp/source/refetch",
            200,
            serde_json::json!({ "ok": true, "status": {}, "renamed_to": "acme/app2" }),
        )
        .await;
        let sha = "3f2a9c14e0b7aaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let done = refetch_source("p1", "acme/app", RefetchChoice::Inspected, Some(sha))
            .await
            .expect("fetched");
        assert_eq!(done.renamed_to.as_deref(), Some("acme/app2"));
        let sent: serde_json::Value =
            serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
        assert_eq!(
            sent["target"],
            serde_json::json!({ "kind": "commit", "sha": sha })
        );

        // Latest never sends a sha, even when one is known.
        let _ = refetch_source("p1", "acme/app", RefetchChoice::Latest, Some(sha)).await;
        let sent: serde_json::Value =
            serde_json::from_slice(&server.received_requests().await.unwrap()[1].body).unwrap();
        assert_eq!(sent["target"], serde_json::json!({ "kind": "latest" }));

        // The latest clone is in place; only the requested commit was unavailable.
        let (_server, _env) = mounted(
            "POST",
            "/api/projects/p1/repos/acme%2Fapp/source/refetch",
            200,
            serde_json::json!({ "ok": false, "code": "commit_not_on_remote",
                "message": "The inspected commit is not reachable.", "status": {} }),
        )
        .await;
        let notice = refetch_source("p1", "acme/app", RefetchChoice::Inspected, Some(sha))
            .await
            .expect("latest was fetched");
        assert!(notice.notice.unwrap().contains("not reachable"));

        let (_server, _env) = mounted(
            "POST",
            "/api/projects/p1/repos/acme%2Fapp/source/refetch",
            409,
            serde_json::json!({ "ok": false, "code": "auth_failed", "message": "GitHub refused the token." }),
        )
        .await;
        let err = refetch_source("p1", "acme/app", RefetchChoice::Latest, None)
            .await
            .expect_err("refused");
        assert_eq!(err.code, "auth_failed");
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn rescan_changes_starts_a_job_with_the_delta_header_or_reports_nothing_changed() {
        let (_server, _env) = mounted(
            "POST",
            "/api/projects/p1/repos/acme%2Fapp/rescan-changes",
            200,
            serde_json::json!({ "ok": true, "job_id": "job-9", "notes": [],
                "changed_files": 6, "from_sha": "3f2a9c14e0b7aaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "to_sha": "9b7e21c0000bbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "commits_newer": 14 }),
        )
        .await;
        assert_eq!(
            rescan_changes("p1", "acme/app").await.expect("started"),
            RescanStarted::Job {
                job_id: "job-9".to_string(),
                header: "Delta retest of changes since 3f2a9c1 (14 commits, 6 files changed)"
                    .to_string(),
            }
        );

        let (_server, _env) = mounted(
            "POST",
            "/api/projects/p1/repos/acme%2Fapp/rescan-changes",
            200,
            serde_json::json!({ "ok": true, "nothing_changed": true }),
        )
        .await;
        assert_eq!(
            rescan_changes("p1", "acme/app").await.expect("ok"),
            RescanStarted::NothingChanged
        );
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn probe_remote_reads_branch_and_head_and_the_error_code() {
        let (_server, _env) = mounted(
            "GET",
            "/api/projects/p1/repos/acme%2Fapp/source/remote",
            200,
            serde_json::json!({ "ok": true, "default_branch": "main", "head_sha": "9b7e21c0000", "head_date": null }),
        )
        .await;
        let found = probe_remote("p1", "acme/app").await.expect("probe");
        assert_eq!(found.default_branch.as_deref(), Some("main"));
        assert_eq!(found.head_sha, "9b7e21c0000");

        let (_server, _env) = mounted(
            "GET",
            "/api/projects/p1/repos/acme%2Fapp/source/remote",
            409,
            serde_json::json!({ "ok": false, "code": "no_token", "message": "No GitHub token is set." }),
        )
        .await;
        assert_eq!(
            probe_remote("p1", "acme/app").await.unwrap_err().code,
            "no_token"
        );
    }

    #[test]
    fn scan_block_notice_says_what_was_deleted_and_offers_the_way_back() {
        fn harness() -> Element {
            rsx! {
                ScanBlockNotice {
                    block: ScanBlock {
                        repos: vec![("acme/app".to_string(), "2026-10-24T17:40:19Z".to_string())],
                    },
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains(
            "Source for acme/app was deleted on 24 Oct 2026. Re-fetch source from the Workspace tab to rescan."
        ));
        assert!(html.contains("Re-fetch source"));
    }
}
