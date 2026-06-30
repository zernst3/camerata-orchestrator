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

pub(crate) use dioxus::prelude::*;

// Chorale (crates.io, headless table) backs the brownfield audit-findings and
// proposed-rules tables — the surfaces where the data genuinely scales.
// `pub(crate) use` so submodules inherit these via `use super::*;`.
pub(crate) use chorale_core::{
    BadgeVariant, BadgeVariantMap, CellValue, ColumnDef, ColumnId, FilterKind, PaginationMode,
    RenderKind, RowId, TableState, Theme,
};
pub(crate) use chorale_dioxus::{use_table, RowCellRenderer, RowCellRenderers, RowClass, Table};

// ── Projects ───────────────────────────────────────────────────────────────────


#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct RuleSelectionView {
    rule_id: String,
    #[serde(default)]
    chosen_option: Option<String>,
    #[serde(default)]
    repos: Vec<String>,
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct CustomRuleView {
    name: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    domain: String,
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
struct RulesetView {
    #[serde(default)]
    selections: Vec<RuleSelectionView>,
    #[serde(default)]
    cross_repo: Vec<RuleSelectionView>,
    #[serde(default)]
    process: Vec<RuleSelectionView>,
    #[serde(default)]
    custom: Vec<CustomRuleView>,
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct ProjectView {
    id: String,
    name: String,
    #[serde(default)]
    repos: Vec<String>,
    #[serde(default)]
    ruleset: RulesetView,
    /// Repos that have been onboarded (`owner/repo`). A repo not here is "not yet onboarded".
    #[serde(default)]
    onboarded: Vec<String>,
    /// Max developer→checker bounce-and-revise iterations a stage may take before the
    /// fleet stops the loop and raises the outstanding violations for human review (#29).
    /// Defaults to 1.
    #[serde(default = "default_max_iterations")]
    max_iterations: usize,
    /// The project's model tier map: fast/balanced/strongest -> model id.
    /// Serde default fills in the fleet defaults when the field is absent (back-compat).
    #[serde(default)]
    tier_map: TierMapView,
    /// Per-step model config for the NON-FLEET AI steps (audit, calibration, research chat,
    /// story authoring, decomposition, escalation, clarification). Serde default fills in
    /// the shipped default model per slot when the field is absent (back-compat).
    #[serde(default)]
    step_models: StepModelsView,
    /// Per-project stall-detection thresholds. `#[serde(default)]` so older payloads
    /// that omit the field get the server's built-in defaults.
    #[serde(default)]
    stall_thresholds: StallThresholdsView,
    /// L3 agentic code-review gate (R7). Off by default. `#[serde(default)]` so
    /// projects written before this field existed deserialise with the disabled default.
    #[serde(default)]
    l3_review: L3ReviewView,
    /// The project's model efficiency profile.
    /// Serde default = Balanced.
    #[serde(default = "default_model_profile_str")]
    model_profile: String, // "balanced" | "max_efficiency" | "max_quality" | "custom"
    /// Whether the Designer (vision/multimodal) band is enabled for this project.
    /// Defaults to false (disabled). When true, vision-capable stages are available.
    #[serde(default)]
    vision_enabled: bool,
    /// Free-text product brief (#112): the soft context fed into agent grounding. Travels with
    /// the project export. Absent → empty.
    #[serde(default)]
    product_brief: String,
    /// Agent operating principles (#112): how-we-work conduct, seeded with defaults + toggleable.
    #[serde(default)]
    operating_principles: Vec<OperatingPrincipleView>,
    /// Project memory (#112, Layer 3): the accumulating, human-curated learnings.
    #[serde(default)]
    memory: Vec<MemoryEntryView>,
}

/// One project-memory entry as the BFF reports it (mirrors the server's `MemoryEntry`). Enum-ish
/// fields are plain strings (snake_case) so the UI doesn't need to mirror the server enums.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct MemoryEntryView {
    id: String,
    #[serde(default)]
    kind: String, // "decision" | "pattern" | "gotcha" | "constraint"
    text: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    status: String, // "proposed" | "approved" | "archived"
}

/// One agent operating principle as the BFF reports it (mirrors the server's `OperatingPrinciple`).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct OperatingPrincipleView {
    id: String,
    text: String,
    #[serde(default = "default_true_op")]
    enabled: bool,
}

fn default_true_op() -> bool {
    true
}

/// UI mirror of `camerata_server::project::L3ReviewConfig`.
/// Default: off, empty model (falls back to balanced tier).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct L3ReviewView {
    #[serde(default)]
    enabled: bool,
    /// The model id for the L3 reviewer. Empty = use the project's balanced tier model.
    #[serde(default)]
    model: String,
}

impl Default for L3ReviewView {
    fn default() -> Self {
        Self { enabled: false, model: String::new() }
    }
}

/// UI mirror of `camerata_server::project::StepModels`. One model-id slot per NON-FLEET AI
/// step. Serde defaults match the server's `DEFAULT_MODEL`.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct StepModelsView {
    #[serde(default = "default_step_model_str")]
    audit: String,
    #[serde(default = "default_step_model_str")]
    calibration: String,
    #[serde(default = "default_step_model_str")]
    research_chat: String,
    #[serde(default = "default_step_model_str")]
    story_authoring: String,
    #[serde(default = "default_step_model_str")]
    decomposition: String,
    #[serde(default = "default_step_model_str")]
    escalation: String,
    #[serde(default = "default_step_model_str")]
    clarification: String,
}

/// The shipped server `DEFAULT_MODEL` (`crate::llm::DEFAULT_MODEL`). Kept in sync here so a
/// project JSON missing a step field renders the same default the server seeds at creation.
fn default_step_model_str() -> String {
    "claude-sonnet-4-6".to_string()
}

impl Default for StepModelsView {
    fn default() -> Self {
        Self {
            audit: default_step_model_str(),
            calibration: default_step_model_str(),
            research_chat: default_step_model_str(),
            story_authoring: default_step_model_str(),
            decomposition: default_step_model_str(),
            escalation: default_step_model_str(),
            clarification: default_step_model_str(),
        }
    }
}

/// UI mirror of `camerata_server::project::StallThresholds`. Two u64 slots.
/// Serde defaults match the server's defaults (120s watched, 600s routine).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct StallThresholdsView {
    #[serde(default = "default_watched_secs")]
    watched_secs: u64,
    #[serde(default = "default_routine_secs")]
    routine_secs: u64,
}

fn default_watched_secs() -> u64 { 120 }

fn default_routine_secs() -> u64 { 600 }

impl Default for StallThresholdsView {
    fn default() -> Self {
        Self { watched_secs: default_watched_secs(), routine_secs: default_routine_secs() }
    }
}

/// UI mirror of `camerata_fleet::tier::TierMap`. Three model-id slots, one per
/// capability band. `fast` and `balanced` are ordered chains (Vec<String>); `strongest`
/// stays a single model. `vision` is an optional chain for the Designer (vision) band.
/// Serde defaults match the fleet defaults.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct TierMapView {
    #[serde(default = "default_fast_chain_str")]
    fast: Vec<String>,
    #[serde(default = "default_balanced_chain_str")]
    balanced: Vec<String>,
    #[serde(default = "default_strongest_model_str")]
    strongest: String,
    /// Model chain for the Designer (vision/multimodal) band. Empty = no model pinned.
    #[serde(default)]
    vision: Vec<String>,
}

fn default_fast_chain_str() -> Vec<String> {
    vec!["claude-haiku-4-5-20251001".to_string()]
}

fn default_balanced_chain_str() -> Vec<String> {
    vec!["claude-sonnet-4-6".to_string()]
}

fn default_strongest_model_str() -> String {
    "claude-opus-4-8".to_string()
}

impl Default for TierMapView {
    fn default() -> Self {
        Self {
            fast: default_fast_chain_str(),
            balanced: default_balanced_chain_str(),
            strongest: default_strongest_model_str(),
            vision: Vec::new(),
        }
    }
}

fn default_max_iterations() -> usize {
    1
}

fn default_model_profile_str() -> String {
    "balanced".to_string()
}

/// GET a JSON resource from the BFF, retrying on a connection failure so a fetch that
/// races the embedded server's startup is not rendered as "empty". The desktop app boots
/// its server in-process; the first request(s) can land before it accepts connections,
/// which previously showed the projects list empty until a remount re-fetched. Retries for
/// ~2.5s, then gives up (returns None) so a genuinely-down server still fails. A successful
/// request whose body is empty / `null` is NOT retried — that is real data, not a race.
async fn bff_get_json<T: serde::de::DeserializeOwned>(path: &str) -> Option<T> {
    let url = format!("{}{}", crate::bff_base(), path);
    for attempt in 0..10u32 {
        match reqwest::get(url.as_str()).await {
            Ok(resp) => return resp.json::<T>().await.ok(),
            Err(_) if attempt < 9 => {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            Err(_) => return None,
        }
    }
    None
}

async fn fetch_projects() -> Option<Vec<ProjectView>> {
    bff_get_json::<Vec<ProjectView>>("/api/projects").await
}

/// One model's slice of the cumulative usage breakdown.
#[derive(Clone, PartialEq, Default, serde::Deserialize)]
struct ModelUsageView {
    #[serde(default)]
    model: String,
    #[serde(default)]
    tokens: u64,
    #[serde(default)]
    cost: f64,
    #[serde(default)]
    calls: u64,
}

/// The last rate-limit event the server observed.
#[derive(Clone, PartialEq, Default, serde::Deserialize)]
struct RateLimitEventView {
    #[serde(default)]
    when_unix: u64,
    #[serde(default)]
    detail: String,
}

/// The cumulative session-wide usage snapshot from `GET /api/usage`.
#[derive(Clone, PartialEq, Default, serde::Deserialize)]
struct UsageView {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read: u64,
    #[serde(default)]
    cache_creation: u64,
    #[serde(default)]
    total_cost_usd: f64,
    #[serde(default)]
    calls: u64,
    #[serde(default)]
    by_model: Vec<ModelUsageView>,
    #[serde(default)]
    rate_limited: bool,
    #[serde(default)]
    last_rate_limit: Option<RateLimitEventView>,
}

impl UsageView {
    /// Total tokens (input + output) for the compact headline figure.
    fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

async fn fetch_usage() -> Option<UsageView> {
    bff_get_json::<UsageView>("/api/usage").await
}

/// Format a token count compactly: 12 / 3.4k / 1.2M.
fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

async fn fetch_active_project() -> Option<ProjectView> {
    bff_get_json::<Option<ProjectView>>("/api/projects/active")
        .await
        .flatten()
}

async fn create_project(name: &str, repos: Vec<String>) -> Option<ProjectView> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/projects", crate::bff_base()))
        .json(&serde_json::json!({ "name": name, "repos": repos }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    serde_json::from_value(v.get("project")?.clone()).ok()
}

async fn set_active_project(id: &str) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/projects/active", crate::bff_base()))
        .json(&serde_json::json!({ "id": id }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Update the project's model-tier map (fast / balanced / strongest / vision model ids).
/// Uses the `POST /api/projects/:id/tier-map` endpoint added in #63. Patch semantics:
/// all bands are always sent so a single round-trip sets the whole map.
async fn set_project_tier_map(id: &str, map: &TierMapView) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/projects/{}/tier-map", crate::bff_base(), id))
        .json(&serde_json::json!({
            "fast":     map.fast,
            "balanced": map.balanced,
            "strongest": map.strongest,
            "vision":   map.vision,
        }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Toggle the Designer (vision) band for a project.
/// Uses `POST /api/projects/:id/vision-enabled {enabled: bool}`.
pub(super) async fn set_project_vision_enabled(id: &str, enabled: bool) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/projects/{}/vision-enabled", crate::bff_base(), id))
        .json(&serde_json::json!({ "enabled": enabled }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Set the model for ONE non-fleet AI step on a project. Uses the
/// `POST /api/projects/:id/step-models` endpoint (patch semantics: one step per call). The
/// `id` is the SCOPED project id passed into the editor — never a global — so the mutation
/// targets exactly that project (per-project isolation is preserved end to end).
/// `pub(super)` so both `rules.rs` (Settings) and `scan.rs` (onboarding) can call the same
/// endpoint and keep Audit + Calibration bidirectionally synced.
pub(super) async fn set_project_step_model(id: &str, step: &str, model: &str) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/projects/{}/step-models", crate::bff_base(), id))
        .json(&serde_json::json!({ "step": step, "model": model }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Update a project's loop-guard ceiling (#29): the max developer→checker
/// bounce-and-revise iterations a stage may take before the fleet stops and
/// raises the outstanding violations for human review.
async fn set_max_iterations(id: &str, max_iterations: usize) -> bool {
    reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/max-iterations",
            crate::bff_base(),
            id
        ))
        .json(&serde_json::json!({ "max_iterations": max_iterations }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Save the project's free-text product brief (#112).
async fn set_product_brief(id: &str, brief: &str) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/projects/{}/product-brief", crate::bff_base(), id))
        .json(&serde_json::json!({ "product_brief": brief }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Add a project-memory entry (#112, Layer 3) — human-added, persisted as Approved.
async fn add_memory_entry(id: &str, kind: &str, text: &str) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/projects/{}/memory", crate::bff_base(), id))
        .json(&serde_json::json!({ "kind": kind, "text": text }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Patch a memory entry's status (`"approved"` | `"archived"` | `"proposed"`).
async fn patch_memory_status(id: &str, eid: &str, status: &str) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/projects/{}/memory/{}", crate::bff_base(), id, eid))
        .json(&serde_json::json!({ "status": status }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Save an edit to a memory entry's text + kind (the view/edit modal).
async fn save_memory_edit(id: &str, eid: &str, kind: &str, text: &str) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/projects/{}/memory/{}", crate::bff_base(), id, eid))
        .json(&serde_json::json!({ "kind": kind, "text": text }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Delete a memory entry (discard a proposal, or prune).
async fn delete_memory_entry(id: &str, eid: &str) -> bool {
    reqwest::Client::new()
        .delete(format!("{}/api/projects/{}/memory/{}", crate::bff_base(), id, eid))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// One curation action on a memory entry (keeps the per-button onclicks tiny).
#[derive(Clone, Copy)]
enum MemAction {
    Approve,
    Archive,
    Delete,
}

/// Run a memory curation action, then bump `refresh` so the settings view re-fetches.
fn run_mem_action(
    id: String,
    eid: String,
    action: MemAction,
    mut busy: Signal<bool>,
    mut refresh: Signal<u32>,
) {
    busy.set(true);
    spawn(async move {
        let ok = match action {
            MemAction::Approve => patch_memory_status(&id, &eid, "approved").await,
            MemAction::Archive => patch_memory_status(&id, &eid, "archived").await,
            MemAction::Delete => delete_memory_entry(&id, &eid).await,
        };
        busy.set(false);
        if ok {
            refresh += 1;
        }
    });
}

/// Save the project's operating principles (#112) — the full toggled/edited list.
async fn set_operating_principles(id: &str, principles: &[OperatingPrincipleView]) -> bool {
    reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/operating-principles",
            crate::bff_base(),
            id
        ))
        .json(&serde_json::json!({ "operating_principles": principles }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Editor for the per-project PRODUCT BRIEF (#112): a scaffolded free-text area + Save. The brief
/// feeds agent grounding (the "why / for-whom / quality bar") and travels with the project export.
#[component]
fn ProductBriefEditor(project: ProjectView, refresh: Signal<u32>) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut brief = use_signal(|| project.product_brief.clone());
    let mut saving = use_signal(|| false);
    let pid = project.id.clone();
    rsx! {
        div { class: "soft-ctx-card",
            p { class: "soft-ctx-title", "Product brief" }
            p { class: "soft-ctx-sub",
                "What this product is, who it's for, the quality bar, the non-negotiables. Agents read \
                 it ABOVE the rules to make judgment calls the per-story spec didn't anticipate. \
                 Travels with the project export."
            }
            textarea {
                class: "soft-ctx-brief",
                rows: 9,
                placeholder: "## What is this product?\n(one paragraph: what it does, for whom)\n\n## Who uses it, and what do they care about most?\n\n## What does \"good\" look like here? (the quality bar)\n\n## Non-negotiables\n\n## Out of scope / non-goals",
                value: "{brief}",
                disabled: saving(),
                oninput: move |e| brief.set(e.value()),
            }
            button {
                class: "btn-run",
                disabled: saving(),
                onclick: move |_| {
                    let pid = pid.clone();
                    let b = brief();
                    saving.set(true);
                    spawn(async move {
                        let ok = set_product_brief(&pid, &b).await;
                        saving.set(false);
                        let mut refresh = refresh;
                        if ok {
                            refresh += 1;
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, "Product brief saved.");
                        } else {
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Could not save the brief.");
                        }
                    });
                },
                if saving() { "Saving\u{2026}" } else { "Save brief" }
            }
        }
    }
}

/// Editor for the per-project OPERATING PRINCIPLES (#112): toggle the shipped defaults, add custom
/// ones, Save. The ENABLED principles are woven into every agent's context. Travels with export.
#[component]
fn OperatingPrinciplesEditor(project: ProjectView, refresh: Signal<u32>) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut principles = use_signal(|| project.operating_principles.clone());
    let mut new_text = use_signal(String::new);
    let mut saving = use_signal(|| false);
    let pid = project.id.clone();
    rsx! {
        div { class: "soft-ctx-card",
            p { class: "soft-ctx-title", "Operating principles" }
            p { class: "soft-ctx-sub",
                "How a good engineer works on THIS project (conduct, not the code). Toggle the defaults \
                 or add your own; the enabled ones are woven into every agent's context. Travels with \
                 the project export."
            }
            div { class: "op-list",
                for (i, p) in principles().iter().enumerate() {
                    label {
                        key: "{p.id}",
                        class: if p.enabled { "op-row on" } else { "op-row" },
                        input {
                            r#type: "checkbox",
                            checked: p.enabled,
                            disabled: saving(),
                            onchange: move |_| {
                                let mut v = principles();
                                v[i].enabled = !v[i].enabled;
                                principles.set(v);
                            },
                        }
                        span { class: "op-text", "{p.text}" }
                    }
                }
            }
            div { class: "op-add",
                input {
                    class: "op-add-input",
                    placeholder: "Add a custom principle\u{2026}",
                    value: "{new_text}",
                    disabled: saving(),
                    oninput: move |e| new_text.set(e.value()),
                }
                button {
                    class: "btn-restart",
                    disabled: new_text().trim().is_empty() || saving(),
                    onclick: move |_| {
                        let t = new_text().trim().to_string();
                        if t.is_empty() { return; }
                        let mut v = principles();
                        let id = format!("custom-{}", v.len() + 1);
                        v.push(OperatingPrincipleView { id, text: t, enabled: true });
                        principles.set(v);
                        new_text.set(String::new());
                    },
                    "Add"
                }
            }
            button {
                class: "btn-run",
                disabled: saving(),
                onclick: move |_| {
                    let pid = pid.clone();
                    let list = principles();
                    saving.set(true);
                    spawn(async move {
                        let ok = set_operating_principles(&pid, &list).await;
                        saving.set(false);
                        let mut refresh = refresh;
                        if ok {
                            refresh += 1;
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, "Operating principles saved.");
                        } else {
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Could not save the principles.");
                        }
                    });
                },
                if saving() { "Saving\u{2026}" } else { "Save principles" }
            }
        }
    }
}

/// Editor for the per-project PROJECT MEMORY (#112, Layer 3): review proposed entries (Approve /
/// Delete), manage approved/archived ones, and add learnings. Approved entries feed grounding;
/// everything travels with the project export.
#[component]
fn MemoryEditor(project: ProjectView, refresh: Signal<u32>) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut new_text = use_signal(String::new);
    let mut new_kind = use_signal(|| "decision".to_string());
    let busy = use_signal(|| false);
    // View/edit modal: Some(entry-id) while open, with the working draft text + kind.
    let mut edit_id = use_signal(|| None::<String>);
    let mut edit_text = use_signal(String::new);
    let mut edit_kind = use_signal(|| "decision".to_string());
    let pid = project.id.clone();
    let pid_modal = project.id.clone();

    // Proposed (review me) first, then approved, then archived.
    let mut entries = project.memory.clone();
    let rank = |s: &str| match s {
        "proposed" => 0,
        "approved" => 1,
        _ => 2,
    };
    entries.sort_by_key(|m| rank(&m.status));
    let proposed_count = entries.iter().filter(|m| m.status == "proposed").count();

    rsx! {
        div { class: "soft-ctx-card",
            p { class: "soft-ctx-title",
                "Project memory"
                if proposed_count > 0 {
                    span { class: "mem-badge", "{proposed_count} to review" }
                }
            }
            p { class: "soft-ctx-sub",
                "Durable learnings (decisions, patterns, gotchas, constraints) that carry across runs. \
                 Approved entries feed every agent's context; agents propose, you curate. Travels with \
                 the project export."
            }
            if entries.is_empty() {
                p { class: "soft-ctx-sub mem-empty",
                    "No memory yet. Add a learning below; agents will also propose them as they work."
                }
            }
            div { class: "mem-list",
                for m in entries.iter() {
                    {
                        let (id_app, eid_app) = (pid.clone(), m.id.clone());
                        let (id_arc, eid_arc) = (pid.clone(), m.id.clone());
                        let (id_del, eid_del) = (pid.clone(), m.id.clone());
                        let (open_text, open_kind, open_eid) = (m.text.clone(), m.kind.clone(), m.id.clone());
                        let approved = m.status == "approved";
                        rsx! {
                            div { key: "{m.id}", class: "mem-row mem-{m.status}",
                                div {
                                    class: "mem-row-main mem-clickable",
                                    title: "Click to view / edit",
                                    onclick: move |_| {
                                        edit_text.set(open_text.clone());
                                        edit_kind.set(open_kind.clone());
                                        edit_id.set(Some(open_eid.clone()));
                                    },
                                    span { class: "mem-kind", "{m.kind}" }
                                    span { class: "mem-text", "{m.text}" }
                                    if !m.source.is_empty() {
                                        span { class: "mem-src", "{m.source}" }
                                    }
                                }
                                div { class: "mem-actions",
                                    if !approved {
                                        button {
                                            class: "mem-btn",
                                            disabled: busy(),
                                            onclick: move |_| run_mem_action(id_app.clone(), eid_app.clone(), MemAction::Approve, busy, refresh),
                                            "Approve"
                                        }
                                    }
                                    if approved {
                                        button {
                                            class: "mem-btn",
                                            disabled: busy(),
                                            onclick: move |_| run_mem_action(id_arc.clone(), eid_arc.clone(), MemAction::Archive, busy, refresh),
                                            "Archive"
                                        }
                                    }
                                    button {
                                        class: "mem-btn mem-del",
                                        disabled: busy(),
                                        onclick: move |_| run_mem_action(id_del.clone(), eid_del.clone(), MemAction::Delete, busy, refresh),
                                        "Delete"
                                    }
                                }
                            }
                        }
                    }
                }
            }
            div { class: "mem-add",
                select {
                    class: "mem-kind-select",
                    disabled: busy(),
                    value: "{new_kind}",
                    onchange: move |e| new_kind.set(e.value()),
                    option { value: "decision", "decision" }
                    option { value: "pattern", "pattern" }
                    option { value: "gotcha", "gotcha" }
                    option { value: "constraint", "constraint" }
                }
                input {
                    class: "op-add-input",
                    placeholder: "Add a learning\u{2026}",
                    value: "{new_text}",
                    disabled: busy(),
                    oninput: move |e| new_text.set(e.value()),
                }
                button {
                    class: "btn-restart",
                    disabled: new_text().trim().is_empty() || busy(),
                    onclick: move |_| {
                        let id = pid.clone();
                        let t = new_text();
                        let k = new_kind();
                        if t.trim().is_empty() { return; }
                        let mut busy = busy;
                        busy.set(true);
                        spawn(async move {
                            let ok = add_memory_entry(&id, &k, &t).await;
                            busy.set(false);
                            let mut refresh = refresh;
                            let mut new_text = new_text;
                            if ok {
                                new_text.set(String::new());
                                refresh += 1;
                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, "Added to project memory.");
                            } else {
                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Could not add the entry.");
                            }
                        });
                    },
                    "Add"
                }
            }

            // ── View / edit modal ───────────────────────────────────────────
            if let Some(eid) = edit_id() {
                div {
                    class: "rule-modal-overlay",
                    onclick: move |_| edit_id.set(None),
                    div {
                        class: "rule-modal mem-edit-modal",
                        onclick: move |e| e.stop_propagation(),
                        div { class: "rule-modal-head",
                            span { class: "rule-modal-id", "Edit learning" }
                            button { class: "rule-modal-close", onclick: move |_| edit_id.set(None), "\u{2715}" }
                        }
                        select {
                            class: "mem-kind-select",
                            value: "{edit_kind}",
                            onchange: move |e| edit_kind.set(e.value()),
                            option { value: "decision", "decision" }
                            option { value: "pattern", "pattern" }
                            option { value: "gotcha", "gotcha" }
                            option { value: "constraint", "constraint" }
                        }
                        textarea {
                            class: "mem-edit-text",
                            rows: 8,
                            value: "{edit_text}",
                            oninput: move |e| edit_text.set(e.value()),
                        }
                        div { class: "mem-edit-actions",
                            button {
                                class: "btn-run",
                                disabled: edit_text().trim().is_empty(),
                                onclick: move |_| {
                                    let id = pid_modal.clone();
                                    let eid = eid.clone();
                                    let t = edit_text();
                                    let k = edit_kind();
                                    spawn(async move {
                                        let ok = save_memory_edit(&id, &eid, &k, &t).await;
                                        let mut refresh = refresh;
                                        let mut edit_id = edit_id;
                                        if ok {
                                            edit_id.set(None);
                                            refresh += 1;
                                        }
                                    });
                                },
                                "Save"
                            }
                            button { class: "mem-btn", onclick: move |_| edit_id.set(None), "Cancel" }
                        }
                    }
                }
            }
        }
    }
}

/// Set the L3 agentic code-review gate configuration for a project.
/// Uses the `POST /api/projects/:id/l3-review` endpoint (R7).
async fn set_project_l3_review(id: &str, enabled: bool, model: &str) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/projects/{}/l3-review", crate::bff_base(), id))
        .json(&serde_json::json!({ "enabled": enabled, "model": model }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Fetch the proposed model assignments for a profile WITHOUT applying them.
pub(super) async fn preview_model_profile(project_id: &str, profile: &str) -> Option<serde_json::Value> {
    reqwest::get(format!(
        "{}/api/projects/{}/model-profile/preview?profile={}",
        crate::bff_base(),
        project_id,
        profile
    ))
    .await
    .ok()?
    .json::<serde_json::Value>()
    .await
    .ok()
}

/// Apply a profile to a project (set profile + cascade + auto-save).
pub(super) async fn apply_model_profile(project_id: &str, profile: &str) -> Option<serde_json::Value> {
    reqwest::Client::new()
        .post(format!("{}/api/projects/{}/model-profile", crate::bff_base(), project_id))
        .json(&serde_json::json!({ "profile": profile }))
        .send()
        .await
        .ok()?
        .json::<serde_json::Value>()
        .await
        .ok()
}

/// Set the stall-detection thresholds for a project.
/// Uses the `POST /api/projects/:id/stall-thresholds` endpoint.
async fn set_project_stall_thresholds(id: &str, watched_secs: u64, routine_secs: u64) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/projects/{}/stall-thresholds", crate::bff_base(), id))
        .json(&serde_json::json!({ "watched_secs": watched_secs, "routine_secs": routine_secs }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct AppliedOptionView {
    id: String,
    label: String,
    #[serde(default)]
    directive: String,
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct AppliedRuleView {
    id: String,
    repo: String,
    title: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    chosen_option: Option<String>,
    #[serde(default)]
    chosen_label: Option<String>,
    #[serde(default)]
    options: Vec<AppliedOptionView>,
    #[serde(default)]
    is_custom: bool,
    #[serde(default)]
    in_corpus: bool,
}

/// Reconcile the project's repos with the rule-bank: what's actually applied,
/// rehydrated with the full source rule (alternatives + context).
async fn fetch_reconcile(project_id: &str) -> Option<Vec<AppliedRuleView>> {
    let v: serde_json::Value = reqwest::get(format!(
        "{}/api/projects/{}/reconcile",
        crate::bff_base(),
        project_id
    ))
    .await
    .ok()?
    .json()
    .await
    .ok()?;
    if !v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        return None;
    }
    serde_json::from_value(v.get("applied").cloned()?).ok()
}

/// The active work-tracker connection as the BFF reports it (`GET /api/provider`).
#[derive(serde::Deserialize, Clone, PartialEq)]
struct ProviderView {
    /// Human label, e.g. "native (in-process)" or "github (token; …)".
    provider: String,
    /// True when a real external tracker (GitHub) is wired.
    live: bool,
}

/// Fetch the active provider/connection from the BFF.
async fn fetch_provider() -> Option<ProviderView> {
    reqwest::get(format!("{}/api/provider", crate::bff_base()))
        .await
        .ok()?
        .json::<ProviderView>()
        .await
        .ok()
}

/// A run as the BFF reports it (`GET /api/runs/:id`): status plus the REAL gate
/// verdicts produced so far.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct RunView {
    #[serde(default)]
    id: String,
    story_id: String,
    status: String,
    events: Vec<RunGateEvent>,
    done: bool,
    #[serde(default)]
    mode: String,
    /// Milliseconds since last recorded run activity (0 when idle tracking unavailable).
    #[serde(default)]
    idle_ms: u128,
    /// True when the run has been idle longer than `stall_threshold_ms`.
    #[serde(default)]
    stalled: bool,
    /// The active stall threshold in milliseconds.
    #[serde(default)]
    stall_threshold_ms: u128,
    /// Whether the run's policy on stall is to alert or auto-cancel.
    #[serde(default)]
    stall_policy: String,
    /// Human-readable failure reason for a `failed` run (e.g. after auto-cancel on stall).
    #[serde(default)]
    failure_reason: Option<String>,
}

/// One event in a run's development-activity stream. Reused for ALL observability
/// layers: the `layer` field ("layer-1" gate, "layer-2" check, "tier", "delegate",
/// "stage"/"fleet", "checks") plus `verdict` drive a distinct label + colour, so a live
/// dev run reads as a concise activity log. `layer` is `#[serde(default)]` so older /
/// scripted payloads that omit it deserialize unchanged (they fall back to the gate
/// layer).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct RunGateEvent {
    #[serde(default)]
    layer: String,
    verdict: String,
    rule: Option<String>,
    detail: String,
}

/// The display label + CSS class for a run-activity event, derived from its `layer` +
/// `verdict`. PURE so the mapping is unit-testable without rendering. The class is one of
/// the existing `live-event {variant}` families plus the new layer variants; the label is
/// a short, human tag (no chain-of-thought).
fn live_event_style(layer: &str, verdict: &str) -> (&'static str, &'static str) {
    match layer {
        // Layer-1 deny-before-execute gate: allow / deny (the bounce-back).
        "layer-1" => match verdict {
            "deny" => ("GATE DENY", "live-event deny"),
            "allow" => ("GATE ALLOW", "live-event allow"),
            _ => ("GATE", "live-event info"),
        },
        // Layer-2 post-task lint/test check + the bounce-and-revise pass.
        "layer-2" => match verdict {
            "pass" => ("LAYER-2 PASS", "live-event allow"),
            "fail" => ("LAYER-2 FAIL", "live-event deny"),
            "revise" => ("REVISE", "live-event revise"),
            // legacy scripted "bounce" verdict.
            "bounce" => ("REVISE", "live-event revise"),
            _ => ("LAYER-2", "live-event info"),
        },
        // Delegation dispatch / return (+ INCOMPLETE escalation).
        "delegate" => match verdict {
            "dispatch" => ("DELEGATE", "live-event delegate"),
            "incomplete" => ("DELEGATE INCOMPLETE", "live-event deny"),
            _ => ("DELEGATE RETURN", "live-event delegate"),
        },
        // Phase 3b: the agent raised a structured clarifying question; the run paused
        // ("pause") or resumed on the answer ("info").
        "clarification" => match verdict {
            "pause" => ("WAITING ON YOU", "live-event revise"),
            _ => ("CLARIFICATION", "live-event info"),
        },
        // Model/tier routing per spawned agent.
        "tier" => ("TIER", "live-event tier"),
        // cargo build/test verification.
        "checks" => match verdict {
            "allow" => ("CHECKS PASS", "live-event allow"),
            "deny" => ("CHECKS FAIL", "live-event deny"),
            _ => ("CHECKS", "live-event info"),
        },
        // Stage / fleet lifecycle + setup.
        "stage" => match verdict {
            "fail" => ("STAGE", "live-event deny"),
            _ => ("STAGE", "live-event info"),
        },
        // Stall-detection synthetic event: the run has been idle longer than the threshold.
        "stall" => ("STALL", "live-event stall"),
        "setup" => ("SETUP", "live-event info"),
        // Default (incl. "fleet" lifecycle, empty/legacy): fall back to the verdict.
        _ => match verdict {
            "deny" | "error" => (
                if verdict == "error" { "ERROR" } else { "DENY" },
                "live-event deny",
            ),
            "allow" => ("ALLOW", "live-event allow"),
            _ => ("INFO", "live-event info"),
        },
    }
}

/// Format an idle duration from milliseconds into a human-readable string.
/// e.g. 90_000 → "1m 30s", 5_000 → "5s", 65_000 → "1m 5s".
fn format_idle(idle_ms: u128) -> String {
    let total_secs = idle_ms / 1000;
    if total_secs < 60 {
        format!("{total_secs}s")
    } else {
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        if secs == 0 {
            format!("{mins}m")
        } else {
            format!("{mins}m {secs}s")
        }
    }
}

/// True when a run is in a non-terminal, cancellable state.
fn run_is_cancellable(status: &str, done: bool) -> bool {
    !done && !matches!(status, "failed" | "cancelled")
}

/// True when a stall warning banner should be shown for a run.
fn run_stall_banner_visible(stalled: bool, done: bool) -> bool {
    stalled && !done
}

/// The outcome of attempting to start a governed run. The no-code-first gate (Pillar 2)
/// can BLOCK the start with a precise reason (server 409), which the cockpit surfaces as
/// a toast instead of silently doing nothing.
enum StartRunOutcome {
    /// The run started; carries its id for polling.
    Started(String),
    /// The development gate blocked the start; carries the server's reason.
    Blocked(String),
    /// Transport / decode failure.
    Failed,
}

/// Start a governed DEVELOPMENT run for a story (the build phase, run from the
/// `DecisionsApproved` step).
///
/// Sends the per-UoW tier map so the fleet's orchestrator (the strongest tier)
/// leads and delegates simpler work to the balanced / fast tiers:
/// `POST /api/stories/:id/run` with body
/// `{ "tier_map": { "strongest": "<id>", "balanced": "<id>", "fast": "<id>" } }`.
///
/// Returns [`StartRunOutcome::Started`] with the run id on success, or
/// [`StartRunOutcome::Blocked`] (with the gate's reason) when the no-code-first gate
/// refuses the start because the story's decisions are not all approved. A transport
/// or decode failure maps to [`StartRunOutcome::Failed`].
/// Build the request body for a development run:
/// `{ "tier_map": { "strongest": "<id>", "balanced": "<id>", "fast": "<id>" } }`,
/// plus `"skip_layer2": true` ONLY when the one-time bootstrap toggle is on (omitted
/// otherwise, so the default-off behaviour is exactly today's). `skip_layer2` is the
/// bootstrap escape hatch: it skips ONLY the post-task layer-2 lint/test bounce so a
/// brownfield repo can install the tooling layer-2 needs. Layer 1 (the security gate)
/// and the no-code-first decisions gate still apply. Extracted as a pure fn so the wire
/// shape is unit-testable.
fn dev_run_body(tier_map: &TierMapView, skip_layer2: bool) -> serde_json::Value {
    let mut body = serde_json::json!({
        "tier_map": {
            "strongest": tier_map.strongest,
            "balanced": tier_map.balanced,
            "fast": tier_map.fast,
        }
    });
    // Only include the flag when set, so a normal run's body is byte-for-byte today's.
    if skip_layer2 {
        body["skip_layer2"] = serde_json::Value::Bool(true);
    }
    body
}

/// Percent-encode a value for use as a single URL PATH SEGMENT.
///
/// UoW / story ids are `owner/repo#num`. Used raw in a path, the `/` breaks
/// single-segment routing and the `#` is even dropped by the client as a URL
/// fragment (so the server never sees it). Encode everything outside the RFC 3986
/// unreserved set; axum's `Path` extractor percent-decodes it back on the server.
fn enc_seg(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

async fn start_dev_run(
    story_id: &str,
    tier_map: &TierMapView,
    skip_layer2: bool,
) -> StartRunOutcome {
    let body = dev_run_body(tier_map, skip_layer2);
    let resp = match reqwest::Client::new()
        .post(format!("{}/api/stories/{}/run", crate::bff_base(), enc_seg(story_id)))
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return StartRunOutcome::Failed,
    };
    if resp.status().as_u16() == 409 {
        let reason = resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("reason").and_then(|r| r.as_str().map(String::from)))
            .unwrap_or_else(|| "The development gate blocked this run.".to_string());
        return StartRunOutcome::Blocked(reason);
    }
    let Ok(v) = resp.json::<serde_json::Value>().await else {
        return StartRunOutcome::Failed;
    };
    match v.get("run_id").and_then(|r| r.as_str()) {
        Some(id) => StartRunOutcome::Started(id.to_string()),
        None => StartRunOutcome::Failed,
    }
}

/// The outcome of starting an INVESTIGATION run. Distinguishes the three cases the
/// caller must react to differently:
/// - `Started(run_id)` — the stage moved Intake → Investigating and a run was created;
///   the caller drives the live agent activity on it.
/// - `Blocked(reason)` — the server 409'd (the UoW was NOT at Intake, e.g. a prior
///   begin already advanced it). The caller surfaces the precise reason AND refreshes
///   the UoW so the now-stale "Begin investigation" button is replaced by the control
///   for the real stage. This is the fix for the "Could not begin the investigation
///   run" toast that appeared when the displayed button was stale (the stage signal had
///   defaulted to Intake while the fetch was still loading / had failed).
/// - `Failed` — a transport / decode error (no run created, no reason available).
pub(crate) enum BeginInvestigationOutcome {
    Started(String),
    Blocked(String),
    Failed,
}

/// Start an INVESTIGATION run for a story (the intake → investigating transition,
/// run from the `Intake` step).
///
/// `POST /api/uow/:story_id/begin-investigation` with body `{ "model": "<id>" }`.
/// On 2xx the server returns `{ "run_id", "story_id" }`; on a blocked transition it
/// 409s with `{ "reason": "<why>" }`. Maps the response into a
/// [`BeginInvestigationOutcome`] so the caller can react precisely (start / surface the
/// block reason + refresh / report a transport failure) instead of collapsing every
/// non-success into a single generic toast.
async fn begin_investigation_run(story_id: &str, model: &str) -> BeginInvestigationOutcome {
    let resp = match reqwest::Client::new()
        .post(format!(
            "{}/api/uow/{}/begin-investigation",
            crate::bff_base(),
            enc_seg(story_id)
        ))
        .json(&serde_json::json!({ "model": model }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return BeginInvestigationOutcome::Failed,
    };
    if resp.status().as_u16() == 409 {
        // The server returns { "reason": "<why>" } for a blocked transition.
        let reason = resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("reason").and_then(|r| r.as_str().map(String::from)))
            .unwrap_or_else(|| "The investigation could not begin from the current stage.".to_string());
        return BeginInvestigationOutcome::Blocked(reason);
    }
    if !resp.status().is_success() {
        return BeginInvestigationOutcome::Failed;
    }
    match resp
        .json::<serde_json::Value>()
        .await
        .ok()
        .and_then(|v| v.get("run_id").and_then(|r| r.as_str().map(String::from)))
    {
        Some(run_id) => BeginInvestigationOutcome::Started(run_id),
        None => BeginInvestigationOutcome::Failed,
    }
}

/// Which view the enterprise cockpit is showing. Routines live INSIDE the cockpit
/// (it's an architect tool), reached via the cockpit's own nav, not a top-level app.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CockpitView {
    /// The control surface: the story spine + center stage + inspector.
    Stories,
    /// Onboard a repo into governance (brownfield: install rules into an
    /// existing repo; greenfield: scaffold a new one). The ENTRY POINT for a
    /// repo new to Camerata — distinct from a story's Investigation phase.
    Onboard,
    /// Manage the active project's ruleset (repo-local / cross-repo / process /
    /// custom) AFTER onboarding — the ongoing control surface over the same
    /// project ruleset the brownfield flow first populates.
    Rules,
    /// The scheduled-routine dashboard.
    Routines,
    /// The local workspace: clone the active project's repos into the chosen folder,
    /// see their checkout status, and ship a branch (push + PR).
    Workspace,
    /// In-app documentation viewer: USER_GUIDE.md and TECHNICAL.md rendered as markdown.
    Docs,
    /// App-wide credentials (OpenRouter key, GitHub token) stored in the OS keychain.
    Credentials,
}

/// The top-level screen of the enterprise app: the projects home, or inside a project.
/// A project CONTAINS everything (repos, ruleset, baseline, workspace), so nothing in
/// the cockpit is reachable until you open one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CockpitScreen {
    /// The projects home: pick one to open, create a new one, or import.
    Projects,
    /// Inside the active project (the cockpit tabs).
    InProject,
}

/// Which surface the projects-home navbar is showing. The home screen exposes only the
/// project-INDEPENDENT surfaces (Docs + global Settings) alongside the project list, so docs and
/// the global settings (credentials, the global chat-assistant model, Bombe) are reachable before
/// any project exists.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HomeView {
    /// The project list (pick / create / import).
    Projects,
    /// The in-app docs viewer (project-independent).
    Docs,
    /// Global-only settings (credentials, chat-assistant model, Bombe).
    Settings,
}

/// The shell for the enterprise edition: shows the projects home first; the cockpit only
/// renders once a project is open. The screen is shared via context so the cockpit's nav
/// can navigate back to the projects list.
#[component]
pub fn CockpitShell() -> Element {
    let screen = use_signal(|| CockpitScreen::Projects);
    use_context_provider(|| screen);
    match screen() {
        CockpitScreen::Projects => rsx! { ProjectGate {} },
        CockpitScreen::InProject => rsx! { CockpitApp {} },
    }
}

/// Export a project as a JSON file (native save dialog). Returns true on success.
async fn export_project_json(id: &str, name: &str) -> bool {
    let Ok(resp) = reqwest::get(format!("{}/api/projects/{}/export", crate::bff_base(), id)).await
    else {
        return false;
    };
    let Ok(text) = resp.text().await else {
        return false;
    };
    // Slug the project name for the filename: lowercase, spaces → hyphens, strip non-alnum.
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let filename = format!("camerata-project-{slug}.json");
    match rfd::AsyncFileDialog::new()
        .set_file_name(&filename)
        .save_file()
        .await
    {
        Some(file) => file.write(text.as_bytes()).await.is_ok(),
        None => false,
    }
}

/// Result of an import attempt (first pass with `overwrite: false`).
#[derive(Clone, PartialEq)]
enum ImportResult {
    /// The project was created or silently overwritten; the returned project is active.
    Imported(ProjectView),
    /// A project with the same name already exists; the user must confirm before we overwrite.
    /// Holds the name for display and the raw JSON body to re-POST with `overwrite: true`.
    Conflict { name: String, payload: String },
    /// Something went wrong (network, parse, etc.).
    Failed,
}

/// Delete a project by id. Returns true on success.
async fn delete_project(id: &str) -> bool {
    reqwest::Client::new()
        .delete(format!("{}/api/projects/{}", crate::bff_base(), id))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Open a file picker, read the JSON, and POST it to the import endpoint with
/// `overwrite: false`. Returns `ImportResult` so the caller can decide whether to
/// prompt for overwrite confirmation.
async fn import_project_json() -> ImportResult {
    let Some(file) = rfd::AsyncFileDialog::new()
        .add_filter("JSON", &["json"])
        .pick_file()
        .await
    else {
        return ImportResult::Failed;
    };
    let Ok(raw) = String::from_utf8(file.read().await) else {
        return ImportResult::Failed;
    };
    import_project_payload(&raw, false).await
}

/// POST `payload` to /api/projects/import with the given `overwrite` flag. Shared by
/// the first-pass attempt and the confirmed overwrite.
async fn import_project_payload(payload: &str, overwrite: bool) -> ImportResult {
    // Merge the `overwrite` flag into the payload without re-parsing the whole doc:
    // parse into a Value, set the flag, then re-serialise.
    let mut body: serde_json::Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(_) => return ImportResult::Failed,
    };
    if let Some(obj) = body.as_object_mut() {
        obj.insert("overwrite".to_string(), serde_json::Value::Bool(overwrite));
    }
    let Ok(body_str) = serde_json::to_string(&body) else {
        return ImportResult::Failed;
    };
    let Ok(resp) = reqwest::Client::new()
        .post(format!("{}/api/projects/import", crate::bff_base()))
        .header("content-type", "application/json")
        .body(body_str)
        .send()
        .await
    else {
        return ImportResult::Failed;
    };
    let Ok(v) = resp.json::<serde_json::Value>().await else {
        return ImportResult::Failed;
    };
    if v.get("conflict").and_then(|b| b.as_bool()).unwrap_or(false) {
        let name = v
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        ImportResult::Conflict {
            name,
            payload: payload.to_string(),
        }
    } else if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        match serde_json::from_value::<ProjectView>(v.get("project").cloned().unwrap_or_default()) {
            Ok(p) => ImportResult::Imported(p),
            Err(_) => ImportResult::Failed,
        }
    } else {
        ImportResult::Failed
    }
}

/// The projects-home shell: a LIMITED navbar (Projects · Docs · Settings + the usage meter) over
/// the project-independent surfaces, so Docs and the GLOBAL settings are reachable before any
/// project exists. The project list itself is [`ProjectsHome`]; the full cockpit nav only appears
/// once a project is open.
#[component]
fn ProjectGate() -> Element {
    let home_view = use_signal(|| HomeView::Projects);
    rsx! {
        div { class: "cockpit",
            HomeNav { home_view }
            match home_view() {
                // The project list owns its own scroll (.project-gate) so it sits directly under
                // the nav; Docs/Settings render .page and need the tinted cockpit-scroll wrapper.
                HomeView::Projects => rsx! { ProjectsHome {} },
                HomeView::Docs => rsx! { div { class: "cockpit-scroll", DocsView {} } },
                HomeView::Settings => rsx! {
                    div { class: "cockpit-scroll", SettingsView { global_only: true } }
                },
            }
        }
    }
}

/// The projects-home navbar: only the project-INDEPENDENT tabs (Docs + global Settings) plus the
/// always-on usage meter (token cost), so they work before any project is open.
#[component]
fn HomeNav(home_view: Signal<HomeView>) -> Element {
    let mut hv = home_view;
    let cls = |v: HomeView| {
        if hv() == v {
            "cockpit-nav-tab on"
        } else {
            "cockpit-nav-tab"
        }
    };
    rsx! {
        div { class: "cockpit-nav",
            button {
                class: cls(HomeView::Projects),
                onclick: move |_| hv.set(HomeView::Projects),
                "Projects"
            }
            button {
                class: cls(HomeView::Docs),
                onclick: move |_| hv.set(HomeView::Docs),
                "Docs"
            }
            button {
                class: cls(HomeView::Settings),
                onclick: move |_| hv.set(HomeView::Settings),
                "Settings"
            }
            UsageMeter {}
        }
    }
}

/// The project list (pick / create / import), rendered inside [`ProjectGate`]'s shell. Nothing
/// project-scoped is reachable until one is opened.
#[component]
fn ProjectsHome() -> Element {
    let mut screen = use_context::<Signal<CockpitScreen>>();
    let mut refresh = use_signal(|| 0u32);
    let projects = use_resource(move || {
        let _ = refresh();
        async move { fetch_projects().await }
    });
    let mut new_name = use_signal(String::new);
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    // The project id awaiting a delete confirm (two-click, with a warning toast).
    let mut pending_delete = use_signal(|| Option::<String>::None);
    // An import that hit a name collision: holds (project name, raw JSON payload).
    // While set, a confirm modal is visible.
    let mut pending_import_overwrite = use_signal(|| Option::<(String, String)>::None);
    // True when the just-opened project has an in-progress onboarding draft: show the
    // "continue or start over" prompt before entering (the project is already active on
    // the server, so the draft endpoints are scoped to it).
    let mut resume_prompt = use_signal(|| false);
    let list = projects.read().clone().flatten().unwrap_or_default();

    rsx! {
        // Overwrite-confirm modal — shown when an import collides with an existing name.
        if let Some((ref conflict_name, ref conflict_payload)) = pending_import_overwrite() {
            {
                let conflict_name = conflict_name.clone();
                let conflict_payload = conflict_payload.clone();
                rsx! {
                    div { class: "rule-modal-overlay", onclick: move |_| pending_import_overwrite.set(None),
                        div { class: "rule-modal", onclick: move |e| e.stop_propagation(),
                            div { class: "rule-modal-head",
                                span { class: "rule-modal-id", "Overwrite project?" }
                                button {
                                    class: "rule-modal-close",
                                    onclick: move |_| pending_import_overwrite.set(None),
                                    "\u{2715}"
                                }
                            }
                            p { class: "rule-modal-detail",
                                "A project named \u{201c}{conflict_name}\u{201d} already exists. \
                                 Overwriting will replace its repos, ruleset, and onboarded state \
                                 but keep its id. This cannot be undone."
                            }
                            div { class: "onboard-leave-actions",
                                button {
                                    class: "btn-edit-sm",
                                    onclick: move |_| pending_import_overwrite.set(None),
                                    "Cancel"
                                }
                                button {
                                    class: "btn-edit-sm pg-btn-danger",
                                    onclick: move |_| {
                                        pending_import_overwrite.set(None);
                                        let payload = conflict_payload.clone();
                                        spawn(async move {
                                            match import_project_payload(&payload, true).await {
                                                ImportResult::Imported(p) => {
                                                    // Set the imported project active so the
                                                    // cockpit and chat ground on it.
                                                    let _ = set_active_project(&p.id).await;
                                                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, "Imported. Resolve the repo paths in the Rules view.");
                                                    refresh += 1;
                                                    screen.set(CockpitScreen::InProject);
                                                }
                                                _ => {
                                                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Import failed.");
                                                }
                                            }
                                        });
                                    },
                                    "Overwrite"
                                }
                            }
                        }
                    }
                }
            }
        }

        // Resume-onboarding prompt — shown when an opened project has an in-progress draft.
        if resume_prompt() {
            div { class: "rule-modal-overlay",
                div { class: "rule-modal", onclick: move |e| e.stop_propagation(),
                    div { class: "rule-modal-head",
                        span { class: "rule-modal-id", "Onboarding in progress" }
                    }
                    p { class: "rule-modal-detail",
                        "This project has an onboarding you didn't finish. Continue where you \
                         left off, or start over from a fresh scan?"
                    }
                    div { class: "onboard-leave-actions",
                        button {
                            // Secondary button sized to match the primary beside it (danger tint).
                            class: "btn-secondary danger",
                            onclick: move |_| {
                                spawn(async move {
                                    // Discard the saved draft, then enter for a fresh onboarding.
                                    clear_onboarding_draft().await;
                                    resume_prompt.set(false);
                                    screen.set(CockpitScreen::InProject);
                                });
                            },
                            "Start over"
                        }
                        button {
                            class: "btn-run",
                            onclick: move |_| {
                                // Keep the draft; OnboardView restores it when opened.
                                resume_prompt.set(false);
                                screen.set(CockpitScreen::InProject);
                            },
                            "Continue where you left off"
                        }
                    }
                }
            }
        }

        div { class: "project-gate",
            div { class: "pg-inner",
                p { class: "eyebrow", "Camerata" }
                h1 { class: "h1", "Your projects" }
                p { class: "lede", "A project is the container for everything — its repos, ruleset, baseline, and workspace. Open one to begin, or create a new one." }

                if list.is_empty() {
                    p { class: "pg-empty", "No projects yet. Create one below to begin." }
                } else {
                    div { class: "pg-list",
                        for p in list.iter() {
                            {
                                let id_export = p.id.clone();
                                let name_export = p.name.clone();
                                let id_open = p.id.clone();
                                let id_del = p.id.clone();
                                let name_del = p.name.clone();
                                let is_pending = pending_delete().as_deref() == Some(p.id.as_str());
                                rsx! {
                                    div { class: "pg-card", key: "{p.id}",
                                        div { class: "pg-card-main",
                                            span { class: "pg-card-name", "{p.name}" }
                                            span { class: "pg-card-meta", "{p.repos.len()} repo(s) · {p.ruleset.selections.len()} repo-rules" }
                                            {
                                                let n_on = p.onboarded.iter().filter(|r| p.repos.contains(r)).count();
                                                let total = p.repos.len();
                                                let cls = if total > 0 && n_on == total { "pg-onboard-badge done" } else { "pg-onboard-badge" };
                                                rsx! {
                                                    span { class: "{cls}",
                                                        if total > 0 && n_on == total { "✓ onboarded" }
                                                        else if n_on > 0 { "{n_on}/{total} onboarded" }
                                                        else { "not yet onboarded" }
                                                    }
                                                }
                                            }
                                        }
                                        div { class: "pg-card-actions",
                                            button {
                                                class: "pg-btn-secondary",
                                                onclick: move |_| {
                                                    let id = id_export.clone();
                                                    let name = name_export.clone();
                                                    spawn(async move { let _ = export_project_json(&id, &name).await; });
                                                },
                                                "Export"
                                            }
                                            button {
                                                class: if is_pending { "pg-btn-danger confirm" } else { "pg-btn-danger" },
                                                onclick: move |_| {
                                                    let id = id_del.clone();
                                                    if pending_delete().as_deref() == Some(id.as_str()) {
                                                        // Second click — delete.
                                                        pending_delete.set(None);
                                                        spawn(async move {
                                                            if delete_project(&id).await {
                                                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, "Project deleted.");
                                                                refresh += 1;
                                                            } else {
                                                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Could not delete the project.");
                                                            }
                                                        });
                                                    } else {
                                                        // First click — warn + arm the confirm.
                                                        pending_delete.set(Some(id));
                                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Warning, format!("Click Confirm to permanently delete \u{201c}{name_del}\u{201d}. This can't be undone."));
                                                    }
                                                },
                                                if is_pending { "Confirm delete" } else { "Delete" }
                                            }
                                            button {
                                                class: "btn-run",
                                                onclick: move |_| {
                                                    let id = id_open.clone();
                                                    spawn(async move {
                                                        if set_active_project(&id).await {
                                                            // If this project has an in-progress onboarding
                                                            // draft (server scopes the draft to the now-active
                                                            // project), ask before entering instead of silently
                                                            // resuming. Otherwise go straight in.
                                                            if load_onboarding_draft().await.is_some() {
                                                                resume_prompt.set(true);
                                                            } else {
                                                                screen.set(CockpitScreen::InProject);
                                                            }
                                                        }
                                                    });
                                                },
                                                "Open"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                div { class: "pg-create",
                    p { class: "section-label", "Create a project" }
                    p { class: "section-hint", "A project starts empty — just a name. You add repos, rules, and everything else from inside it (the way an Azure resource group works)." }
                    div { class: "pg-create-row",
                        input { class: "addressee-input", placeholder: "project name", value: "{new_name}", oninput: move |e| new_name.set(e.value()) }
                        button {
                            class: "btn-run",
                            onclick: move |_| {
                                let name = new_name();
                                if name.trim().is_empty() { return; }
                                spawn(async move {
                                    if let Some(p) = create_project(&name, Vec::new()).await {
                                        // Newly-created project: set it active so chat grounds on it.
                                        let _ = set_active_project(&p.id).await;
                                        screen.set(CockpitScreen::InProject);
                                    }
                                });
                            },
                            "Create & open"
                        }
                    }
                    button {
                        class: "btn-edit-sm pg-import",
                        onclick: move |_| {
                            spawn(async move {
                                match import_project_json().await {
                                    ImportResult::Imported(p) => {
                                        // Set the imported project active so the cockpit and
                                        // chat ground on it (mirrors the "Open" button path).
                                        let _ = set_active_project(&p.id).await;
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, "Imported. Resolve the repo paths in the Rules view.");
                                        refresh += 1;
                                        screen.set(CockpitScreen::InProject);
                                    }
                                    ImportResult::Conflict { name, payload } => {
                                        pending_import_overwrite.set(Some((name, payload)));
                                    }
                                    ImportResult::Failed => {}
                                }
                            });
                        },
                        "Import project (JSON)…"
                    }
                }
            }
        }
    }
}

/// The cockpit's internal nav: switch between the control surface (stories) and the
/// routine dashboard. Both are architect tools, so both live in the Enterprise app.
#[component]
fn CockpitNav(view: Signal<CockpitView>) -> Element {
    let mut view = view;
    let mut screen = use_context::<Signal<CockpitScreen>>();
    // Leaving is safe: onboarding state auto-saves per project, so navigating back just
    // leaves (the resume prompt restores it on return). No "you'll lose your work" warning.
    let cls = |v: CockpitView| {
        if view() == v {
            "cockpit-nav-tab on"
        } else {
            "cockpit-nav-tab"
        }
    };
    rsx! {
        div { class: "cockpit-nav",
            button {
                class: "cockpit-nav-tab back",
                onclick: move |_| {
                    screen.set(CockpitScreen::Projects);
                },
                "← Projects"
            }
            button {
                class: cls(CockpitView::Onboard),
                onclick: move |_| view.set(CockpitView::Onboard),
                "Onboard repos"
            }
            button {
                class: cls(CockpitView::Stories),
                onclick: move |_| view.set(CockpitView::Stories),
                "Governed Development"
            }
            button {
                class: cls(CockpitView::Rules),
                onclick: move |_| view.set(CockpitView::Rules),
                "Rules"
            }
            button {
                class: cls(CockpitView::Routines),
                onclick: move |_| view.set(CockpitView::Routines),
                "Routines"
            }
            button {
                class: cls(CockpitView::Workspace),
                onclick: move |_| view.set(CockpitView::Workspace),
                "Repository Workspace"
            }
            button {
                class: cls(CockpitView::Docs),
                onclick: move |_| view.set(CockpitView::Docs),
                "Docs"
            }
            button {
                class: cls(CockpitView::Credentials),
                onclick: move |_| view.set(CockpitView::Credentials),
                "Settings"
            }
            // Persistent cumulative usage meter, pinned to the right of the nav row.
            UsageMeter {}
        }
    }
}

/// A compact, always-visible cumulative LLM usage readout (tokens · $ · calls), polling
/// `GET /api/usage` every few seconds. When the server reports `rate_limited`, it swaps the
/// normal readout for a distinct amber "Rate-limited — retrying" badge. Clicking it toggles
/// a small by-model breakdown. Provider-agnostic by virtue of the endpoint: the $ figure is
/// derived from tokens when the backend doesn't report one (the Gemini-shape case).
#[component]
fn UsageMeter() -> Element {
    let mut usage = use_signal(|| None::<UsageView>);
    let mut expanded = use_signal(|| false);

    // Poll every ~4s, forever, mirroring the `poll_job` cadence pattern. A failed fetch
    // leaves the last good value in place (the meter never flickers to empty on a blip).
    use_future(move || async move {
        loop {
            if let Some(u) = fetch_usage().await {
                usage.set(Some(u));
            }
            tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        }
    });

    let Some(u) = usage() else {
        // Until the first poll lands, render a neutral placeholder so the nav layout is stable.
        return rsx! {
            div { class: "usage-meter usage-meter-loading", title: "Cumulative LLM usage",
                span { class: "usage-dim", "usage —" }
            }
        };
    };

    if u.rate_limited {
        let detail = u
            .last_rate_limit
            .as_ref()
            .map(|e| e.detail.clone())
            .unwrap_or_else(|| "provider is throttling requests".to_string());
        return rsx! {
            div { class: "usage-meter usage-meter-rl", title: "{detail}",
                span { class: "usage-rl-dot" }
                span { "Rate-limited — retrying" }
            }
        };
    }

    let tokens = fmt_tokens(u.total_tokens());
    let cost = format!("${:.2}", u.total_cost_usd);
    let calls = u.calls;
    let by_model = u.by_model.clone();
    let is_expanded = expanded();

    rsx! {
        div { class: "usage-meter-wrap",
            button {
                class: "usage-meter",
                title: "Cumulative LLM usage this session — click for the by-model breakdown",
                onclick: move |_| expanded.toggle(),
                span { class: "usage-num", "{tokens}" }
                span { class: "usage-unit", "tok" }
                span { class: "usage-sep", "·" }
                span { class: "usage-num", "{cost}" }
                span { class: "usage-sep", "·" }
                span { class: "usage-num", "{calls}" }
                span { class: "usage-unit", "calls" }
            }
            if is_expanded {
                div { class: "usage-breakdown",
                    if by_model.is_empty() {
                        div { class: "usage-breakdown-empty", "No model calls yet." }
                    } else {
                        table { class: "usage-breakdown-table",
                            thead {
                                tr {
                                    th { "Model" }
                                    th { class: "usage-r", "Tokens" }
                                    th { class: "usage-r", "Cost" }
                                    th { class: "usage-r", "Calls" }
                                }
                            }
                            tbody {
                                for m in by_model.iter() {
                                    tr { key: "{m.model}",
                                        td { "{m.model}" }
                                        td { class: "usage-r", "{fmt_tokens(m.tokens)}" }
                                        td { class: "usage-r", "${m.cost:.2}" }
                                        td { class: "usage-r", "{m.calls}" }
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

/// The `/api/gate-probe` result (#14): the end-to-end gate-loop go/no-go.
#[derive(Clone, PartialEq, serde::Deserialize)]
struct GateProbeView {
    #[serde(default)]
    go: bool,
    #[serde(default)]
    layer1_denied: usize,
    #[serde(default)]
    layer1_total: usize,
    #[serde(default)]
    layer1_clean_allowed: bool,
    #[serde(default)]
    layer2_bounced: bool,
    #[serde(default)]
    layer2_clean: bool,
}

async fn fetch_gate_probe() -> Option<GateProbeView> {
    reqwest::Client::new()
        .post(format!("{}/api/gate-probe", crate::bff_base()))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()
}

/// In-app gate self-check (#14): runs the deterministic end-to-end gate-loop probe and shows a
/// GO/NO-GO — deny-before-execute denied a forbidden write, and the bounce-and-revise loop
/// resolved a planted violation. The thesis, verifiable in one click (no model, no network out).
#[component]
fn GateSelfCheck() -> Element {
    let mut running = use_signal(|| false);
    let mut result = use_signal(|| Option::<GateProbeView>::None);
    rsx! {
        div { class: "gate-selfcheck",
            div { class: "gate-selfcheck-head",
                span { class: "gate-selfcheck-title", "Gate self-check" }
                span { class: "gate-selfcheck-sub", "Deny-before-execute + bounce-and-revise, end to end — deterministic, no model." }
                button {
                    class: "btn-edit-sm",
                    disabled: running(),
                    onclick: move |_| {
                        running.set(true);
                        spawn(async move {
                            result.set(fetch_gate_probe().await);
                            running.set(false);
                        });
                    },
                    if running() { "Running…" } else { "Run gate self-check" }
                }
            }
            if let Some(r) = result() {
                {
                    let l1 = format!(
                        "Layer 1: deny-before-execute — {}/{} floor rules enforced · clean write {}",
                        r.layer1_denied,
                        r.layer1_total,
                        if r.layer1_clean_allowed { "allowed" } else { "DENIED (deny-all!)" }
                    );
                    let l2 = format!("Layer 2: bounced={}, revise resolved={}", r.layer2_bounced, r.layer2_clean);
                    let (badge, cls) = if r.go { ("GO", "gate-selfcheck-verdict go") } else { ("NO-GO", "gate-selfcheck-verdict nogo") };
                    rsx! {
                        div { class: "{cls}",
                            span { class: "gate-selfcheck-badge", "{badge}" }
                            div { class: "gate-selfcheck-lines",
                                span { "{l1}" }
                                span { "{l2}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The loop-guard control (#29): adjust the active project's max developer→checker
/// bounce-and-revise iterations. Reads the current value from the active project and
/// writes it back via `POST /api/projects/:id/max-iterations`. On reaching the cap a
/// dirty stage stops and surfaces its outstanding violations for human review; the
/// shipped default is 1 (a single bounce).
#[component]
fn LoopGuardControl() -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    // Fetch the active project so we know the project id + current ceiling.
    let active = use_resource(fetch_active_project);
    // Local edit state, seeded once from the fetched value.
    let mut value = use_signal(|| 1usize);
    let mut seeded = use_signal(|| false);
    let mut saving = use_signal(|| false);

    let proj = active.read().clone().flatten();
    if let Some(p) = &proj {
        if !seeded() {
            value.set(p.max_iterations.max(1));
            seeded.set(true);
        }
    }

    let Some(project) = proj else {
        return rsx! {
            div { class: "loop-guard",
                span { class: "loop-guard-title", "Loop guard" }
                span { class: "loop-guard-sub", "Create or select a project to configure the bounce-and-revise ceiling." }
            }
        };
    };
    let pid = project.id.clone();

    rsx! {
        div { class: "loop-guard",
            div { class: "loop-guard-head",
                span { class: "loop-guard-title", "Loop guard — max revise iterations" }
                span { class: "loop-guard-sub",
                    "How many developer→checker bounce-and-revise passes a dirty stage may take before the loop stops and the outstanding violations are raised for review. Default: 1."
                }
            }
            div { class: "loop-guard-row",
                button {
                    class: "btn-edit-sm",
                    disabled: value() <= 1 || saving(),
                    onclick: move |_| {
                        let v = value().saturating_sub(1).max(1);
                        value.set(v);
                    },
                    "−"
                }
                input {
                    class: "loop-guard-input",
                    r#type: "number",
                    min: "1",
                    max: "20",
                    value: "{value}",
                    oninput: move |e| {
                        if let Ok(n) = e.value().parse::<usize>() {
                            value.set(n.clamp(1, 20));
                        }
                    },
                }
                button {
                    class: "btn-edit-sm",
                    disabled: value() >= 20 || saving(),
                    onclick: move |_| {
                        let v = (value() + 1).min(20);
                        value.set(v);
                    },
                    "+"
                }
                button {
                    class: "btn-edit-sm loop-guard-save",
                    disabled: saving(),
                    onclick: move |_| {
                        let pid = pid.clone();
                        let n = value();
                        saving.set(true);
                        spawn(async move {
                            let ok = set_max_iterations(&pid, n).await;
                            saving.set(false);
                            if ok {
                                crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Info,
                                    &format!("Loop guard set to {n} iteration(s)."),
                                );
                            } else {
                                crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Error,
                                    "Could not update the loop guard.",
                                );
                            }
                        });
                    },
                    if saving() { "Saving…" } else { "Save" }
                }
            }
        }
    }
}

#[component]
pub fn CockpitApp() -> Element {
    // Which cockpit view (control surface vs routines). Declared first so all hooks
    // below run unconditionally in a stable order regardless of the view.
    let mut view = use_signal(|| CockpitView::Stories);
    // Shared so a child (e.g. ScanResults' "Complete onboarding") can switch the tab.
    use_context_provider(|| view);
    // On open, land on the right view: Onboard while onboarding is incomplete, Governed
    // Development once every repo is onboarded. Set ONCE (a guard) so it never overrides the
    // user's manual nav after the first load.
    let active_proj = use_resource(fetch_active_project);
    let mut view_inited = use_signal(|| false);
    use_effect(move || {
        if view_inited() {
            return;
        }
        if let Some(maybe) = &*active_proj.read() {
            let fully_onboarded = matches!(
                maybe,
                Some(p) if !p.repos.is_empty() && p.repos.iter().all(|r| p.onboarded.contains(r))
            );
            view.set(if fully_onboarded {
                CockpitView::Stories
            } else {
                CockpitView::Onboard
            });
            view_inited.set(true);
        }
    });

    // The active connection (native vs GitHub), shown honestly in the Onboard view.
    let provider_res = use_resource(fetch_provider);

    // Onboard state lifted to app scope so it SURVIVES navigating between cockpit views:
    // the Phase-1 scan result, and the id of an in-flight async audit job. A background
    // job keeps running server-side regardless; these let the UI re-attach (resume the
    // poll, re-show the scan) when the user returns to Onboard instead of losing it.
    let onboard_scan = use_signal(|| Option::<ScanReportView>::None);
    use_context_provider(|| onboard_scan);
    let active_audit_job = use_signal(|| Option::<String>::None);
    use_context_provider(|| active_audit_job);

    // Governed-Development sub-view selection lifted to app scope so it SURVIVES leaving the
    // Stories tab and returning: navigating to Settings/Rules and back lands you on the same
    // UoW (or Issue Management) you left, instead of resetting to Issue Management.
    // GovernedDevPage reads this via use_context instead of owning a local signal that
    // unmounts (and resets) every time the page leaves the tree.
    let govdev_sel = use_signal(|| uow::GovDevSel::IssueManagement);
    use_context_provider(|| govdev_sel);

    // Routines + Onboard live inside the cockpit (architect tools). All hooks above
    // have run, so branching here is safe.
    if view() == CockpitView::Onboard {
        let conn = provider_res.read().clone().flatten();
        return rsx! {
            div { class: "cockpit",
                AppUpdateBanner {}
                CockpitNav { view }
                div { class: "cockpit-scroll",
                    OnboardView { connection: conn }
                }
            }
        };
    }
    if view() == CockpitView::Rules {
        return rsx! {
            div { class: "cockpit",
                AppUpdateBanner {}
                CockpitNav { view }
                div { class: "cockpit-scroll",
                    RulesView {}
                }
            }
        };
    }
    if view() == CockpitView::Routines {
        return rsx! {
            div { class: "cockpit",
                AppUpdateBanner {}
                CockpitNav { view }
                div { class: "cockpit-scroll",
                    crate::routines::RoutineDashboard {}
                }
            }
        };
    }
    if view() == CockpitView::Workspace {
        return rsx! {
            div { class: "cockpit",
                AppUpdateBanner {}
                CockpitNav { view }
                div { class: "cockpit-scroll",
                    crate::workspace::WorkspaceView {}
                }
            }
        };
    }
    if view() == CockpitView::Docs {
        return rsx! {
            div { class: "cockpit",
                AppUpdateBanner {}
                CockpitNav { view }
                div { class: "cockpit-scroll",
                    DocsView {}
                }
            }
        };
    }
    if view() == CockpitView::Credentials {
        return rsx! {
            div { class: "cockpit",
                AppUpdateBanner {}
                CockpitNav { view }
                div { class: "cockpit-scroll",
                    SettingsView { global_only: false }
                }
            }
        };
    }

    // The Governed Development page (work-item / UoW surface). It owns its own data
    // fetching and selection state; CockpitApp just hosts it inside the shell chrome.
    rsx! {
        div { class: "cockpit",
            AppUpdateBanner {}
            CockpitNav { view }
            div { class: "cockpit-scroll",
                GovernedDevPage {}
            }
        }
    }
}

/// The consolidated Settings page.
///
/// Two explicit sections:
///   1. "Cross-project" — credentials + Bombe. Tokens/keys span all projects.
///   2. "This project"  — loop guard, model profile, tier map, step models,
///                        stall thresholds, L3 review. All scoped to the
///                        active project.
/// The subset of `GET /api/settings` this control needs: the app-level chat model.
#[derive(serde::Deserialize)]
struct ChatModelSettingLite {
    #[serde(default)]
    chat_model: Option<String>,
}

/// Fetch the app-level (cross-project) chat assistant model from `GET /api/settings`.
async fn fetch_app_chat_model() -> Option<String> {
    let s: ChatModelSettingLite = reqwest::get(format!("{}/api/settings", crate::bff_base()))
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    s.chat_model.filter(|m| !m.trim().is_empty())
}

/// Persist the app-level chat assistant model via `POST /api/settings/chat-model`.
async fn save_app_chat_model(model: &str) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/settings/chat-model", crate::bff_base()))
        .json(&serde_json::json!({ "model": model }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Cross-project chat-assistant model selector. The in-app chat is a GLOBAL assistant, so its
/// model is an APP-LEVEL setting (applies to every project). Reads/writes the app-level
/// `chat_model` via `GET /api/settings` + `POST /api/settings/chat-model`, using the same
/// registry-backed `<select>` pattern as the per-project step-model editors. The default when
/// unset is the registry's default (first Claude model) — the server applies its DEFAULT_MODEL
/// floor when the setting is blank.
#[component]
fn ChatModelSetting() -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let models = use_resource(|| async move { scan::fetch_audit_models().await });
    let models = models.read().clone().flatten();
    let app_model = use_resource(fetch_app_chat_model);
    let app_model = app_model.read().clone().flatten();

    let mut selected = use_signal(String::new);
    let mut saving = use_signal(|| false);
    // Seed once: prefer the persisted app-level chat model, else the registry default.
    if selected().is_empty() {
        if let Some(m) = &app_model {
            selected.set(m.clone());
        } else if let Some(m) = &models {
            if !m.default.is_empty() {
                selected.set(m.default.clone());
            }
        }
    }

    rsx! {
        div { class: "tier-map-editor step-models-editor",
            p { class: "section-hint tier-map-hint",
                "The model the floating chat assistant uses. It is a single, global assistant, so \
                 this applies to ALL projects. The per-request override the chat panel may send \
                 still takes precedence for that one call. Saves immediately."
            }
            div { class: "tier-map-rows",
                div { class: "tier-map-row step-model-row",
                    span { class: "step-model-label-wrap",
                        label { class: "tier-map-band-label", "Chat assistant" }
                    }
                    if let Some(m) = models {
                        select {
                            class: "tier-map-input run-model-select",
                            value: "{selected}",
                            disabled: saving(),
                            onchange: move |e| {
                                let model = e.value();
                                selected.set(model.clone());
                                saving.set(true);
                                spawn(async move {
                                    if save_app_chat_model(&model).await {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, "Chat assistant model saved.");
                                    } else {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Could not save the chat assistant model.");
                                    }
                                    saving.set(false);
                                });
                            },
                            for (group_label , opts) in m.grouped().into_iter() {
                                optgroup { label: "{group_label}",
                                    for opt in opts.into_iter() {
                                        option {
                                            value: "{opt.id}",
                                            selected: selected() == opt.id,
                                            "{opt.label}"
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
}

#[component]
fn SettingsView(global_only: bool) -> Element {
    let refresh = use_signal(|| 0u32);
    let active = use_resource(move || {
        let _ = refresh();
        async move { fetch_active_project().await }
    });

    let proj = active.read().clone().flatten();

    rsx! {
        div { class: "page",
            p { class: "eyebrow", "Settings" }
            h1 { class: "h1", "Settings" }

            // ── Section 1: Cross-project ──────────────────────────────────────
            // Credentials and Bombe toggle apply to all projects — they are
            // stored in the OS keychain and the app-wide local store, not
            // per-project. Make the scope explicit so users don't expect
            // per-project isolation here.
            h2 { class: "settings-section-heading", "Cross-project \u{2014} applies to all projects" }
            p { class: "proj-settings-scope-note",
                "These tokens and keys are shared across every project. Changing them affects all governed runs, regardless of which project is active."
            }
            crate::credentials::CredentialsSettings {}

            // ── Chat assistant model (cross-project) ──────────────────────────
            // The in-app chat is a GLOBAL assistant, so its model is an app-level
            // setting that applies to every project (not a per-project step).
            p { class: "section-label settings-label", "Chat assistant model" }
            ChatModelSetting {}

            // ── Section 2: This project ───────────────────────────────────────
            // All settings below are scoped to the currently active project. Hidden entirely in
            // the home/global-only view (reached before any project is open).
            if !global_only {
            h2 { class: "settings-section-heading", "This project" }
            p { class: "proj-settings-scope-note",
                "These settings apply to the active project and affect all governed runs within it."
            }

            {match &proj {
                None => rsx! {
                    p { class: "section-hint", "Create or select a project to configure project-level settings." }
                },
                Some(p) => {
                    let p_owned = p.clone();
                    rsx! {
                        // ── Loop guard ────────────────────────────────────────
                        LoopGuardControl {}

                        // ── Suggested model levels ────────────────────────────
                        p { class: "section-label settings-label", "Suggested model levels" }
                        rules::ModelProfileEditor { project: p_owned.clone(), refresh }

                        // ── Fleet model bands ─────────────────────────────────
                        p { class: "section-label settings-label", "Fleet model bands" }
                        rules::TierMapEditor { project: p_owned.clone() }

                        // ── Helper-agent models ───────────────────────────────
                        p { class: "section-label settings-label", "Helper-agent models" }
                        rules::StepModelsEditor { project: p_owned.clone() }

                        // ── L3 agentic code review ────────────────────────────
                        p { class: "section-label settings-label", "L3 AI code review" }
                        rules::L3ReviewEditor { project: p_owned.clone() }

                        // ── Stall thresholds ──────────────────────────────────
                        p { class: "section-label settings-label", "Stall thresholds" }
                        rules::StallThresholdsEditor { project: p_owned.clone() }

                        // ── Commit / PR gate (#65): bypass mode + per-rule toggles ──
                        p { class: "section-label settings-label", "Commit / PR gate" }
                        crate::vcs_settings::VcsGateSettings { project_id: p_owned.id.clone() }

                        // ── Soft context (#112): the product brief + operating principles that
                        // feed agent grounding and travel with the project export ──
                        p { class: "section-label settings-label", "Soft context" }
                        ProductBriefEditor { project: p_owned.clone(), refresh }
                        OperatingPrinciplesEditor { project: p_owned.clone(), refresh }
                        MemoryEditor { project: p_owned.clone(), refresh }
                    }
                }
            }}
            }
        }
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
            "No stories yet — clean slate",
            "Nothing is seeded. Connect GitHub, then onboard one or more repos (the \u{201c}Onboard repos\u{201d} tab) or adopt a tracker issue to bring real stories into the spine.",
        ),
    };
    rsx! {
        div { class: "cockpit-notice",
            p { class: "cockpit-notice-title", "{title}" }
            p { class: "cockpit-notice-body", "{body}" }
        }
    }
}

impl Default for Disposition {
    fn default() -> Self {
        Self {
            state: TriageState::Unresolved,
            reason: String::new(),
            bucket: TechDebtBucket::Later,
        }
    }
}

/// Custom-rule helpers for onboarding. `domain` routes a rule: a repo's `owner/repo` =
/// repo-scoped (the "Custom" domain, shown only in that repo's table); `*` = all repos (the
/// "Custom Global" domain, shown everywhere like a project-level rule). `body` is the free-text
/// directive — the architect owns its wording. (Struct defined above; this adds onboarding methods.)
impl CustomRuleView {
    /// True for a Custom Global rule (applies to every repo).
    fn is_global(&self) -> bool {
        let d = self.domain.trim();
        d == "*" || d.is_empty()
    }
    /// Stable table id for this custom rule.
    fn rule_id(&self) -> String {
        format!("CUSTOM-{}", self.name)
    }
    /// Render as a proposed-rule row so it lives in the table alongside corpus rules. A single
    /// option carries the body, so directive resolution (audit/arm) returns the body, not the
    /// name, and it never reads as "needs a choice".
    fn to_proposed(&self, all_repos: &[String]) -> ProposedRuleView {
        let repos = if self.is_global() {
            all_repos.to_vec()
        } else {
            vec![self.domain.clone()]
        };
        ProposedRuleView {
            id: self.rule_id(),
            title: self.name.clone(),
            kind: "review".to_string(),
            enforcement: "prose".to_string(),
            options: vec![RuleOptionView {
                id: "custom".to_string(),
                label: self.name.clone(),
                directive: self.body.clone(),
                why: String::new(),
            }],
            default_option: Some("custom".to_string()),
            decision_question: None,
            decision_why: None,
            scope: "repo-local".to_string(),
            domain: if self.is_global() {
                "Custom Global".to_string()
            } else {
                "Custom".to_string()
            },
            repos,
            placement: "Guidance in AGENTS.md, reviewed at PR (custom · prose)".to_string(),
            finding_count: 0,
            recommended: true,
            // Custom rules are user-authored; they don't go through the corpus grounding
            // ladder. Emit them as `verified` (the architect authored + trusts them) so
            // they show the checkmark badge alongside corpus-verified rules, and
            // is_auto_recommended = true so they are pre-checked on the proposed-rules table.
            is_auto_recommended: true,
            verification: "verified".to_string(),
            sources: Vec::new(),
        }
    }
}

/// Feature-flag map returned by `GET /api/feature-flags`.
/// Keys are flag names; values are booleans.
/// A missing flag is treated as `false` (conservative default: the feature is off).
#[derive(Clone, PartialEq, serde::Deserialize, Default)]
struct FeatureFlagMap {
    /// SOC-2 gap analysis section in the deep-tier results. When `false`, the
    /// SOC-2 gap table and the SOC-2 portion of the deep-export are hidden.
    #[serde(default)]
    soc2: bool,
    /// Flat catch-all for any flag the UI hasn't explicitly modelled yet.
    #[serde(flatten)]
    extra: std::collections::HashMap<String, serde_json::Value>,
}

/// Fetch the current feature-flag state from the server. Returns the default map
/// (all flags off) when the endpoint is unreachable, so older server versions
/// don't break the UI.
async fn fetch_feature_flags() -> FeatureFlagMap {
    let result: Option<FeatureFlagMap> = async {
        reqwest::get(format!("{}/api/feature-flags", crate::bff_base()))
            .await
            .ok()?
            .json::<FeatureFlagMap>()
            .await
            .ok()
    }
    .await;
    result.unwrap_or_default()
}

/// Minimum release info returned by `GET /api/release`.
/// The server checks the latest GitHub release tag and reports whether the running
/// binary is behind.
#[derive(Clone, PartialEq, serde::Deserialize)]
struct AppReleaseView {
    /// Version string of the running binary (e.g. `"0.4.1"`).
    current: String,
    /// Latest published release tag (e.g. `"0.4.2"`). `None` when the check
    /// hasn't run yet or the GitHub API was unreachable.
    #[serde(default)]
    latest: Option<String>,
    /// True when `latest > current` (server-side semver compare).
    #[serde(default)]
    update_available: bool,
    /// HTML / Markdown release notes for `latest` (empty when not available).
    #[serde(default)]
    release_notes: String,
}

async fn fetch_app_release() -> Option<AppReleaseView> {
    reqwest::get(format!("{}/api/release", crate::bff_base()))
        .await
        .ok()?
        .json::<AppReleaseView>()
        .await
        .ok()
}

/// App-update banner. Shown across the top of every cockpit tab when the server
/// reports a newer release. Dismissible within the session (not persisted).
#[component]
fn AppUpdateBanner() -> Element {
    let release_res = use_resource(fetch_app_release);
    let mut dismissed = use_signal(|| false);

    let Some(rel) = release_res.read().clone().flatten() else {
        return rsx! {};
    };
    if !rel.update_available || dismissed() {
        return rsx! {};
    }
    let latest = rel.latest.clone().unwrap_or_default();
    let current = rel.current.clone();
    rsx! {
        div { class: "app-update-banner",
            span { class: "app-update-icon", "\u{2B06}" }
            span { class: "app-update-text",
                "Camerata {latest} is available (you are running {current}). "
                if !rel.release_notes.is_empty() {
                    span { class: "app-update-notes", "{rel.release_notes}" }
                }
            }
            a {
                class: "app-update-link",
                href: "https://github.com/zernst3/camerata-orchestrator/releases",
                target: "_blank",
                "View release"
            }
            button {
                class: "app-update-dismiss",
                onclick: move |_| dismissed.set(true),
                "\u{00D7}"
            }
        }
    }
}

/// In-app documentation viewer. Renders USER_GUIDE.md and TECHNICAL.md as
/// markdown, with a toggle to switch between them. Uses the same `md_to_html`
/// renderer and `.chat-turn-text.md` CSS that the chat bubble uses, so tables,
/// code blocks, and headings are styled consistently.
const DOCS_USER_GUIDE: &str = include_str!("../../../docs/USER_GUIDE.md");

const DOCS_TECHNICAL: &str = include_str!("../../../docs/TECHNICAL.md");

#[derive(Clone, Copy, PartialEq, Eq)]
enum DocsTab {
    UserGuide,
    Technical,
}

#[component]
fn DocsView() -> Element {
    let mut tab = use_signal(|| DocsTab::UserGuide);

    let content = match tab() {
        DocsTab::UserGuide => crate::md::md_to_html(DOCS_USER_GUIDE),
        DocsTab::Technical => crate::md::md_to_html(DOCS_TECHNICAL),
    };

    rsx! {
        div { class: "docs-view",
            div { class: "docs-tabs",
                button {
                    class: if tab() == DocsTab::UserGuide { "chat-mode-btn active" } else { "chat-mode-btn" },
                    onclick: move |_| tab.set(DocsTab::UserGuide),
                    "User Guide"
                }
                button {
                    class: if tab() == DocsTab::Technical { "chat-mode-btn active" } else { "chat-mode-btn" },
                    onclick: move |_| tab.set(DocsTab::Technical),
                    "Technical"
                }
            }
            div { class: "docs-body chat-turn-text md", dangerous_inner_html: content }
        }
    }
}

pub mod live_run;
pub mod rules;
pub mod scan;
pub mod table;
pub mod uow;

pub use live_run::*;
pub use rules::*;
pub use scan::*;
pub use table::*;
pub use uow::*;


#[cfg(test)]
mod tests {
    use super::{
        det_tool_label, dev_run_body, estimate_audit_cost, format_idle, is_enforced_floor,
        live_event_style, run_is_cancellable, run_stall_banner_visible, run_status_badge,
        FindingView, JobStatusEnvelope, JobStateView, RunGateEvent, RunView, StallThresholdsView,
        TierMapView,
    };

    /// The job-state view deserializes the server's `deterministic` progress section
    /// (per-tool rows + done/total). An old server omitting the field defaults to empty.
    #[test]
    fn job_state_view_parses_deterministic_progress() {
        let json = r#"{
            "status": "running", "done": 0, "total": 0, "findings": [],
            "deterministic": {
                "tools": [
                    {"tool": "floor", "status": "done", "findings": 3},
                    {"tool": "clippy", "status": "running", "findings": 0}
                ],
                "done": 1, "total": 2
            }
        }"#;
        let js: JobStateView = serde_json::from_str(json).unwrap();
        assert_eq!((js.deterministic.done, js.deterministic.total), (1, 2));
        assert_eq!(js.deterministic.tools.len(), 2);
        assert_eq!(js.deterministic.tools[0].tool, "floor");
        assert_eq!(js.deterministic.tools[0].status, "done");
        assert_eq!(js.deterministic.tools[0].findings, 3);
        assert_eq!(js.deterministic.tools[1].status, "running");

        // Back-compat: a payload WITHOUT the field deserializes to an empty progress.
        let legacy: JobStateView =
            serde_json::from_str(r#"{"status":"running","done":1,"total":4,"findings":[]}"#)
                .unwrap();
        assert_eq!((legacy.deterministic.done, legacy.deterministic.total), (0, 0));
        assert!(legacy.deterministic.tools.is_empty());
    }

    /// The per-tool label maps the wire tool names to friendly labels.
    #[test]
    fn deterministic_tool_labels() {
        assert_eq!(det_tool_label("floor"), "Security floor");
        assert_eq!(det_tool_label("unrouted"), "Unrouted rules");
        // Linters pass through unchanged.
        assert_eq!(det_tool_label("clippy"), "clippy");
        assert_eq!(det_tool_label("ruff"), "ruff");
    }

    /// `fmt_tokens` compacts a raw token count into the headline figure shown in the usage
    /// meter: bare digits below 1k, `N.Nk` in the thousands, `N.NM` in the millions. Asserts
    /// the exact rendered strings (the `{:.1}` rounding is load-bearing — the meter is narrow).
    #[test]
    fn fmt_tokens_formats_each_magnitude_band() {
        use super::fmt_tokens;
        // Below 1k: the raw integer, no suffix.
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(12), "12");
        assert_eq!(fmt_tokens(999), "999");
        // Thousands: one decimal place + `k`. 1000 rounds to "1.0k"; 1500 to "1.5k".
        assert_eq!(fmt_tokens(1_000), "1.0k");
        assert_eq!(fmt_tokens(1_500), "1.5k");
        assert_eq!(fmt_tokens(3_400), "3.4k");
        assert_eq!(fmt_tokens(999_999), "1000.0k"); // still below the 1M cutoff
        // Millions: one decimal place + `M`.
        assert_eq!(fmt_tokens(1_000_000), "1.0M");
        assert_eq!(fmt_tokens(1_200_000), "1.2M");
    }

    /// The development-run body must match the frozen backend contract exactly:
    /// `{ "tier_map": { "strongest", "balanced", "fast" } }`.
    #[test]
    fn dev_run_body_matches_frozen_contract() {
        let tm = TierMapView {
            strongest: "opus-x".to_string(),
            balanced: vec!["sonnet-x".to_string()],
            fast: vec!["haiku-x".to_string()],
            vision: vec![],
        };
        let body = dev_run_body(&tm, false);
        let tier = body.get("tier_map").expect("tier_map key present");
        assert_eq!(tier.get("strongest").unwrap(), "opus-x");
        // fast/balanced are now arrays; check the first element.
        assert_eq!(tier["balanced"][0], "sonnet-x");
        assert_eq!(tier["fast"][0], "haiku-x");
        // Exactly the three tier keys, nothing else.
        assert_eq!(tier.as_object().unwrap().len(), 3);
        // Default (skip_layer2 = false): body is exactly today's — just tier_map, no flag.
        assert_eq!(body.as_object().unwrap().len(), 1);
        assert!(body.get("skip_layer2").is_none());
    }

    /// The run-activity event view parses the new `layer` field (and tolerates its
    /// absence on legacy / scripted payloads via `#[serde(default)]`).
    #[test]
    fn run_gate_event_parses_layer_and_defaults_when_absent() {
        // New-shape: carries a layer.
        let with_layer: RunGateEvent = serde_json::from_str(
            r#"{"layer":"layer-2","verdict":"fail","rule":"RUST-FMT","detail":"stage 1/2 failed layer-2: RUST-FMT."}"#,
        )
        .unwrap();
        assert_eq!(with_layer.layer, "layer-2");
        assert_eq!(with_layer.verdict, "fail");
        assert_eq!(with_layer.rule.as_deref(), Some("RUST-FMT"));

        // Legacy/scripted shape: no layer → defaults to empty (falls back to gate layer).
        let no_layer: RunGateEvent =
            serde_json::from_str(r#"{"verdict":"deny","rule":"GOV-1","detail":"x"}"#).unwrap();
        assert_eq!(no_layer.layer, "");
        assert_eq!(no_layer.verdict, "deny");
    }

    /// The per-layer/verdict styling gives each observability kind a distinct label +
    /// class so the activity log reads clearly. Asserts the load-bearing mappings.
    #[test]
    fn live_event_style_labels_each_layer_distinctly() {
        assert_eq!(live_event_style("layer-1", "deny"), ("GATE DENY", "live-event deny"));
        assert_eq!(live_event_style("layer-1", "allow"), ("GATE ALLOW", "live-event allow"));
        assert_eq!(live_event_style("layer-2", "pass"), ("LAYER-2 PASS", "live-event allow"));
        assert_eq!(live_event_style("layer-2", "fail"), ("LAYER-2 FAIL", "live-event deny"));
        assert_eq!(live_event_style("layer-2", "revise"), ("REVISE", "live-event revise"));
        assert_eq!(live_event_style("tier", "info"), ("TIER", "live-event tier"));
        assert_eq!(
            live_event_style("delegate", "dispatch"),
            ("DELEGATE", "live-event delegate")
        );
        assert_eq!(
            live_event_style("delegate", "incomplete"),
            ("DELEGATE INCOMPLETE", "live-event deny")
        );
        assert_eq!(live_event_style("checks", "allow"), ("CHECKS PASS", "live-event allow"));
        // Legacy/empty layer falls back to verdict-based styling.
        assert_eq!(live_event_style("", "deny"), ("DENY", "live-event deny"));
        assert_eq!(live_event_style("", "allow"), ("ALLOW", "live-event allow"));
    }

    /// The bootstrap toggle adds `skip_layer2: true` to the body ONLY when on, and never
    /// when off (so a normal run is byte-for-byte the existing contract).
    #[test]
    fn dev_run_body_includes_skip_layer2_only_when_on() {
        let tm = TierMapView {
            strongest: "opus-x".to_string(),
            balanced: vec!["sonnet-x".to_string()],
            fast: vec!["haiku-x".to_string()],
            vision: vec![],
        };
        // OFF: no flag at all.
        let off = dev_run_body(&tm, false);
        assert!(off.get("skip_layer2").is_none(), "off must omit the flag");

        // ON: the flag is present and true; tier_map is unchanged.
        let on = dev_run_body(&tm, true);
        assert_eq!(on.get("skip_layer2").unwrap(), &serde_json::Value::Bool(true));
        let tier = on.get("tier_map").expect("tier_map still present");
        assert_eq!(tier.get("strongest").unwrap(), "opus-x");
        // Exactly tier_map + skip_layer2.
        assert_eq!(on.as_object().unwrap().len(), 2);
    }

    /// The default TierMapView feeds the per-phase model defaults. Investigation defaults
    /// to the strongest tier; the dev-run body carries all three.
    #[test]
    fn default_tier_map_seeds_all_three_tiers() {
        let tm = TierMapView::default();
        assert!(!tm.strongest.is_empty());
        // fast/balanced are now Vec<String>; non-empty means at least one model in the chain.
        assert!(!tm.balanced.is_empty());
        assert!(!tm.fast.is_empty());
        let body = dev_run_body(&tm, false);
        let tier = body.get("tier_map").unwrap();
        assert_eq!(tier.get("strongest").unwrap(), &tm.strongest);
        // fast/balanced are arrays; verify the first element matches the primary.
        assert_eq!(tier["balanced"][0].as_str().unwrap(), tm.balanced[0]);
        assert_eq!(tier["fast"][0].as_str().unwrap(), tm.fast[0]);
    }

    /// Sequential mode (1 batch per chunk) has no caching reuse across batches — the
    /// estimate must match the pre-caching math (full digest price every pass).
    #[test]
    fn sequential_mode_no_cache_discount() {
        // Small repo: 100k chars, 0 rules, sequential.
        let (toks, dollars, passes) =
            estimate_audit_cost(100_000, 0, "sequential", 3.0, 15.0, 3.0, 15.0, false, false, false);
        assert_eq!(passes, 1, "0 rules + sequential = one pass");
        assert!(toks > 0, "some tokens");
        assert!(dollars > 0.0, "some cost");
    }

    /// Parallel mode with multiple batches should cost LESS than the naive per-batch full
    /// price because subsequent batches read the digest from cache at ~0.1×.
    #[test]
    fn parallel_multi_batch_cheaper_than_sequential_sum() {
        // 30 rules -> ceil(30/15)=2 batches; 350k chars = 1 chunk.
        let (_, dollars_parallel, passes_parallel) =
            estimate_audit_cost(350_000, 30, "parallel", 3.0, 15.0, 3.0, 15.0, false, false, false);
        assert_eq!(passes_parallel, 2, "2 batches for 30 rules");

        // If we ran sequential with 30 rules we get 1 pass; run twice to simulate
        // the naive "pay full price twice" baseline.
        let (_, dollars_seq_single, _) =
            estimate_audit_cost(350_000, 30, "sequential", 3.0, 15.0, 3.0, 15.0, false, false, false);
        let naive_two_passes = dollars_seq_single * 2.0;

        assert!(
            dollars_parallel < naive_two_passes,
            "caching makes 2 parallel batches cheaper than naive 2× sequential: {dollars_parallel:.4} < {naive_two_passes:.4}"
        );
    }

    /// Single-batch parallel (1 rule, or 0 rules) has nothing to cache — no second batch
    /// to amortise over, so the discount path is not taken.
    #[test]
    fn parallel_single_batch_no_discount() {
        // 1 rule -> 1 batch in parallel mode.
        let (toks1, dollars1, passes1) =
            estimate_audit_cost(350_000, 1, "parallel", 3.0, 15.0, 3.0, 15.0, false, false, false);
        let (toks_seq, dollars_seq, passes_seq) =
            estimate_audit_cost(350_000, 1, "sequential", 3.0, 15.0, 3.0, 15.0, false, false, false);
        assert_eq!(passes1, 1);
        assert_eq!(passes_seq, 1);
        // Token counts should be in the same ballpark (both are 1 pass over the same chunk).
        // The cache-write surcharge on the parallel path makes it *slightly* higher than
        // sequential, but they should be within 30% of each other.
        let ratio = toks1 as f64 / toks_seq as f64;
        assert!(
            ratio < 1.3,
            "single-batch parallel not much more expensive than sequential: ratio={ratio:.2}"
        );
        let _ = (dollars1, dollars_seq); // exercise the values without asserting exact amounts
    }

    /// Thorough mode triples the calibration cost; the estimate should grow accordingly.
    #[test]
    fn thorough_mode_costs_more_than_default() {
        let (_, dollars_default, _) =
            estimate_audit_cost(200_000, 15, "parallel", 3.0, 15.0, 1.0, 5.0, false, false, false);
        let (_, dollars_thorough, _) =
            estimate_audit_cost(200_000, 15, "parallel", 3.0, 15.0, 1.0, 5.0, true, false, false);
        assert!(
            dollars_thorough > dollars_default,
            "thorough costs more: {dollars_thorough:.4} > {dollars_default:.4}"
        );
    }

    /// Batch mode applies a flat 50% discount to the SCAN passes vs. parallel on the same
    /// config. Calibration is NOT discounted (it always runs real-time). The pass count
    /// is identical (same chunking + rule-batching).
    #[test]
    fn batch_mode_cheaper_than_parallel_due_to_scan_discount() {
        // 30 rules, 350k chars = 1 chunk, 2 rule-batches. Calibration = same model.
        let (_, dollars_parallel, passes_parallel) =
            estimate_audit_cost(350_000, 30, "parallel", 3.0, 15.0, 3.0, 15.0, false, false, false);
        let (_, dollars_batch, passes_batch) =
            estimate_audit_cost(350_000, 30, "batch", 3.0, 15.0, 3.0, 15.0, false, false, false);
        assert_eq!(
            passes_parallel, passes_batch,
            "same pass count in parallel and batch (only pricing differs)"
        );
        // Batch must be cheaper than parallel (scan discount applied), but the ratio is
        // not exactly 0.5 because calibration is priced at full rate in both modes.
        assert!(
            dollars_batch < dollars_parallel,
            "batch is cheaper than parallel: {dollars_batch:.4} < {dollars_parallel:.4}"
        );
        // The discount is at least 25% overall (scan dominates in a 2-batch, 1-chunk case).
        let ratio = dollars_batch / dollars_parallel;
        assert!(
            ratio < 0.75,
            "batch should be at least 25% cheaper than parallel: ratio={ratio:.4}"
        );
    }

    /// Batch mode with 0 rules (free-form, 1 pass per chunk): calibration cost is
    /// identical in both modes; scan cost is halved. Total must be cheaper in batch mode.
    #[test]
    fn batch_mode_zero_rules_cheaper_than_parallel() {
        let (_, dollars_parallel, _) =
            estimate_audit_cost(200_000, 0, "parallel", 3.0, 15.0, 3.0, 15.0, false, false, false);
        let (_, dollars_batch, _) =
            estimate_audit_cost(200_000, 0, "batch", 3.0, 15.0, 3.0, 15.0, false, false, false);
        assert!(
            dollars_batch < dollars_parallel,
            "batch cheaper even with 0 rules: {dollars_batch:.4} < {dollars_parallel:.4}"
        );
    }

    /// Deep tier (three extra whole-repo passes) must ADD to the dollar figure, and it must be
    /// the single priciest option vs. thorough or full-vs-incremental on the same config.
    #[test]
    fn deep_tier_costs_more_and_is_the_priciest_option() {
        let base = |deep: bool, thorough: bool| {
            estimate_audit_cost(350_000, 30, "parallel", 3.0, 15.0, 3.0, 15.0, thorough, false, deep).1
        };
        let standard = base(false, false);
        let thorough = base(false, true);
        let deep = base(true, false);
        assert!(deep > standard, "deep adds cost: {deep:.4} > {standard:.4}");
        assert!(
            deep > thorough,
            "deep is the priciest option (more than thorough): {deep:.4} > {thorough:.4}"
        );
    }

    /// The incremental flag is plumbed through but, with no changed-file breakdown available
    /// client-side, prices the same full-scan figure (over-estimate by design). It must not
    /// blow up the estimate and must equal the full-scan number for the same inputs.
    #[test]
    fn incremental_flag_prices_same_as_full_today() {
        let full =
            estimate_audit_cost(350_000, 30, "parallel", 3.0, 15.0, 3.0, 15.0, false, false, false);
        let incremental =
            estimate_audit_cost(350_000, 30, "parallel", 3.0, 15.0, 3.0, 15.0, false, true, false);
        assert_eq!(
            full.1, incremental.1,
            "incremental prices the full set today (no changed-file data): {} vs {}",
            full.1, incremental.1
        );
    }

    // ── estimate_audit_cost — AI-scan-off and free-model pricing ─────────────

    /// When AI scan is off (run_ai_review=false), the UI passes 0.0 for all model prices.
    /// estimate_audit_cost must return $0 in that case (no LLM calls, no token spend).
    #[test]
    fn ai_scan_off_zero_prices_yields_zero_dollars() {
        // Simulate the UI's behaviour when run_ai_review() is false: both model prices
        // are clamped to (0.0, 0.0) before calling estimate_audit_cost.
        let (toks, dollars, _passes) =
            estimate_audit_cost(350_000, 30, "parallel", 0.0, 0.0, 0.0, 0.0, false, false, false);
        assert_eq!(
            dollars, 0.0,
            "zero prices must produce $0 estimate (AI scan off): got {dollars}"
        );
        // Token count is still computed (for informational display) even at $0.
        assert!(toks > 0, "token count should still be non-zero even when prices are zero");
    }

    /// A free OpenRouter model has price_in=0.0 and price_out=0.0.  Passing those values
    /// must produce a $0 estimate (the model is free, so no cost regardless of token count).
    #[test]
    fn free_model_zero_prices_yields_zero_dollars() {
        let (_, dollars, _) =
            estimate_audit_cost(200_000, 15, "parallel", 0.0, 0.0, 0.0, 0.0, false, false, false);
        assert_eq!(
            dollars, 0.0,
            "free model (price_in=price_out=0) must yield $0 estimate: got {dollars}"
        );
    }

    /// A paid model with known registry prices must produce a non-zero estimate, and the
    /// estimate must scale with price: doubling the model price doubles the dollar figure.
    #[test]
    fn paid_model_registry_prices_produce_nonzero_and_scale_linearly() {
        let (_, dollars_base, _) =
            estimate_audit_cost(200_000, 15, "parallel", 1.0, 5.0, 1.0, 5.0, false, false, false);
        let (_, dollars_double, _) =
            estimate_audit_cost(200_000, 15, "parallel", 2.0, 10.0, 2.0, 10.0, false, false, false);
        assert!(dollars_base > 0.0, "paid model must yield non-zero estimate: {dollars_base}");
        // Doubling prices must double the dollar figure (the function is linear in price).
        let ratio = dollars_double / dollars_base;
        assert!(
            (ratio - 2.0).abs() < 0.001,
            "doubling model prices must double the estimate: ratio={ratio:.4}"
        );
    }

    /// Sonnet 4.6 registry prices ($3/$15 per M) produce a meaningful estimate for a
    /// medium-sized repo scan — sanity-checks the default fallback used by the UI.
    #[test]
    fn sonnet_registry_price_estimate_is_positive() {
        // Sonnet 4.6 list price: $3 in / $15 out per million tokens.
        let (_, dollars, _) =
            estimate_audit_cost(350_000, 30, "parallel", 3.0, 15.0, 3.0, 15.0, false, false, false);
        assert!(
            dollars > 0.0,
            "Sonnet-priced estimate must be positive for a 350k-char, 30-rule scan: {dollars}"
        );
    }

    // ── selection_key() unit tests ────────────────────────────────────────────

    use super::{selection_key, SINGLE_REPO_SELECTION_KEY};

    #[test]
    fn selection_key_empty_view_repo_uses_sentinel() {
        assert_eq!(selection_key(""), SINGLE_REPO_SELECTION_KEY);
    }

    #[test]
    fn selection_key_named_repo_is_passthrough() {
        assert_eq!(selection_key("owner/repo"), "owner/repo");
    }

    #[test]
    fn selection_key_sentinel_cannot_collide_with_a_real_repo() {
        // A real `owner/repo` never contains a NUL byte, so the sentinel can't be a repo name.
        assert!(SINGLE_REPO_SELECTION_KEY.contains('\u{0}'));
        assert_ne!(selection_key("a/b"), SINGLE_REPO_SELECTION_KEY);
    }

    // ── verif_badge() unit tests ──────────────────────────────────────────────
    //
    // Pure function, no DOM — each test just asserts label + CSS modifier.
    // Coverage: all four canonical values + an unknown value (falls back to draft).

    use super::{verif_badge, verif_sources_tooltip, RuleSourceView};

    #[test]
    fn verif_badge_verified_returns_checkmark_label_and_green_class() {
        let (label, cls) = verif_badge("verified");
        assert!(label.contains("Verified"), "label should mention Verified, got: {label}");
        assert_eq!(cls, "verified");
    }

    #[test]
    fn verif_badge_grounded_returns_grounded_label_and_blue_class() {
        let (label, cls) = verif_badge("grounded");
        assert!(label.contains("Grounded"), "label should mention Grounded, got: {label}");
        // Grounded must carry its own distinct symbol (the circled source-dot), separate from
        // the verified checkmark, so it's a clear table status not a faint tint.
        assert!(label.contains('\u{29bf}'), "grounded label should carry its source-dot symbol");
        assert!(!label.contains('\u{2713}'), "grounded must NOT reuse the verified checkmark");
        assert_eq!(cls, "grounded");
    }

    #[test]
    fn verif_badge_draft_returns_draft_label_and_gray_class() {
        let (label, cls) = verif_badge("draft");
        assert_eq!(label, "Draft");
        assert_eq!(cls, "draft");
    }

    #[test]
    fn verif_badge_needs_recheck_returns_distinct_label_and_class() {
        let (label, cls) = verif_badge("needs_recheck");
        assert!(label.contains("re-check") || label.contains("recheck"), "label should signal re-check, got: {label}");
        assert_eq!(cls, "needs-recheck");
    }

    #[test]
    fn verif_badge_unknown_value_falls_back_to_draft() {
        // An unrecognised value (e.g. a future extension the UI hasn't caught up to)
        // must not panic and must fall back to the `draft` visual.
        let (label, cls) = verif_badge("something_new");
        assert_eq!(label, "Draft");
        assert_eq!(cls, "draft");
    }

    #[test]
    fn verif_sources_tooltip_empty_sources_returns_empty_string() {
        assert_eq!(verif_sources_tooltip(&[]), "");
    }

    #[test]
    fn verif_sources_tooltip_with_linter_includes_bracket_suffix() {
        let sources = vec![RuleSourceView {
            url: "https://example.com".to_string(),
            title: "Example rule".to_string(),
            linter: Some("eslint: no-unused-vars".to_string()),
        }];
        let tip = verif_sources_tooltip(&sources);
        assert!(tip.contains("Example rule"), "title absent: {tip}");
        assert!(tip.contains("eslint: no-unused-vars"), "linter absent: {tip}");
    }

    #[test]
    fn verif_sources_tooltip_without_linter_uses_title_only() {
        let sources = vec![RuleSourceView {
            url: "https://example.com".to_string(),
            title: "Google Style Guide".to_string(),
            linter: None,
        }];
        let tip = verif_sources_tooltip(&sources);
        assert_eq!(tip, "Google Style Guide");
    }

    #[test]
    fn verif_sources_tooltip_multiple_sources_joined_with_separator() {
        let sources = vec![
            RuleSourceView { url: "u1".to_string(), title: "Source A".to_string(), linter: None },
            RuleSourceView { url: "u2".to_string(), title: "Source B".to_string(), linter: Some("tool: rule".to_string()) },
        ];
        let tip = verif_sources_tooltip(&sources);
        // Both titles must appear; the separator is " · " (middle dot).
        assert!(tip.contains("Source A"), "Source A missing: {tip}");
        assert!(tip.contains("Source B"), "Source B missing: {tip}");
        assert!(tip.contains(" · "), "separator missing: {tip}");
    }

    // ── pw/cockpit-ui: Feature 1 — effective_auto_recommended ─────────────────
    // Tests for the `ProposedRuleView::effective_auto_recommended()` helper which
    // is the single truth-gate for pre-checking a proposed rule during onboarding.
    //
    // The server is now AUTHORITATIVE: `is_auto_recommended` encodes the full gate
    // (stack-relevance + grounded/verified + !opt_in_only). The UI returns it
    // directly without re-deriving from `recommended` or `verification`.
    //
    // Regression guard for the opt_in_only fallback bug: CodeQL/Semgrep rules are
    // grounded + recommended (= true) but carry is_auto_recommended: false from the
    // server. The old fallback `recommended && grounded/verified` would have
    // overridden that and pre-checked them; the new impl must NOT.

    use super::{FeatureFlagMap, ProposedRuleView};

    fn make_proposed_rule(recommended: bool, verification: &str, is_auto_recommended: bool) -> ProposedRuleView {
        ProposedRuleView {
            id: "TEST-1".to_string(),
            title: "Test rule".to_string(),
            kind: "structural".to_string(),
            enforcement: "error".to_string(),
            options: vec![],
            default_option: None,
            decision_question: None,
            decision_why: None,
            scope: "repo".to_string(),
            domain: "test".to_string(),
            repos: vec![],
            placement: "AGENTS.md".to_string(),
            finding_count: 0,
            recommended,
            is_auto_recommended,
            verification: verification.to_string(),
            sources: vec![],
        }
    }

    /// Server sends `is_auto_recommended: true` → rule is pre-checked.
    #[test]
    fn effective_auto_recommended_server_true_pre_checks() {
        let r = make_proposed_rule(false, "draft", true);
        assert!(r.effective_auto_recommended(), "server flag true must pre-check");
    }

    /// Server sends `is_auto_recommended: false` → rule is NOT pre-checked,
    /// even when `recommended` is true and verification is "grounded".
    /// This is the exact CodeQL/Semgrep opt_in_only regression guard:
    /// those rules are grounded + recommended but the server sets
    /// is_auto_recommended: false (because opt_in_only = true on the Rule).
    /// The UI must honour the server's explicit false and NOT fall back to
    /// `recommended && grounded` — that fallback was the root cause of the bug.
    #[test]
    fn effective_auto_recommended_server_false_not_pre_checked_even_if_recommended_and_grounded() {
        // Simulates CICD-CODEQL-SECURITY-SCAN-1 / CICD-SEMGREP-SECURITY-SCAN-1:
        // grounded, stack-relevant (recommended=true), but opt_in_only → server sends false.
        let r = make_proposed_rule(true, "grounded", false);
        assert!(
            !r.effective_auto_recommended(),
            "server false must not be overridden by recommended+grounded (opt_in_only regression guard)"
        );
    }

    /// Same regression guard for `verified` provenance level.
    #[test]
    fn effective_auto_recommended_server_false_not_pre_checked_even_if_recommended_and_verified() {
        let r = make_proposed_rule(true, "verified", false);
        assert!(
            !r.effective_auto_recommended(),
            "server false must not be overridden by recommended+verified"
        );
    }

    /// Draft rules with server false are not pre-checked.
    #[test]
    fn effective_auto_recommended_server_false_draft_not_pre_checked() {
        let r = make_proposed_rule(true, "draft", false);
        assert!(!r.effective_auto_recommended(), "draft rules with server false must not be pre-checked");
    }

    /// `needs_recheck` with server false is not pre-checked.
    #[test]
    fn effective_auto_recommended_server_false_needs_recheck_not_pre_checked() {
        let r = make_proposed_rule(true, "needs_recheck", false);
        assert!(!r.effective_auto_recommended(), "needs_recheck with server false must not be pre-checked");
    }

    /// Not recommended at all with server false: not pre-checked.
    #[test]
    fn effective_auto_recommended_server_false_not_recommended_not_pre_checked() {
        let r = make_proposed_rule(false, "grounded", false);
        assert!(!r.effective_auto_recommended(), "grounded-but-not-recommended with server false must not be pre-checked");
    }

    // ── pw/cockpit-ui: Feature 5 — FeatureFlagMap deserialization ─────────────
    // The feature flag map uses `#[serde(flatten)]` to absorb unknown future flags.
    // These tests confirm the known `soc2` field parses correctly and unknown
    // keys are absorbed without error.

    #[test]
    fn feature_flag_map_soc2_true() {
        let json = r#"{"soc2": true}"#;
        let m: FeatureFlagMap = serde_json::from_str(json).unwrap();
        assert!(m.soc2, "soc2 should be true");
    }

    #[test]
    fn feature_flag_map_soc2_false() {
        let json = r#"{"soc2": false}"#;
        let m: FeatureFlagMap = serde_json::from_str(json).unwrap();
        assert!(!m.soc2, "soc2 should be false");
    }

    #[test]
    fn feature_flag_map_defaults_to_false_when_key_absent() {
        let json = r#"{}"#;
        let m: FeatureFlagMap = serde_json::from_str(json).unwrap();
        assert!(!m.soc2, "soc2 should default to false when absent");
    }

    #[test]
    fn feature_flag_map_extra_keys_do_not_error() {
        let json = r#"{"soc2": true, "future_flag": true, "another": 42}"#;
        let m: FeatureFlagMap = serde_json::from_str(json).unwrap();
        assert!(m.soc2, "soc2 should still parse when extra keys present");
        assert_eq!(m.extra.len(), 2, "extra keys should be absorbed into the extra map");
    }

    #[test]
    fn feature_flag_map_default_impl_all_flags_off() {
        let m = FeatureFlagMap::default();
        assert!(!m.soc2, "default FeatureFlagMap should have all flags off");
        assert!(m.extra.is_empty(), "default extra should be empty");
    }

    // ── Governed Development: pure work-item / UoW helpers ─────────────────────
    use super::{
        active_mention_partial, ancestor_path, apply_mention_selection, build_work_item_rows,
        create_or_open_label, existing_uow_for, filter_mention_candidates, labels_summary,
        render_pulled_issues_for_chat, work_item_state_badge, UowListEntry, UowStage, WorkItem,
    };

    fn wi(id: &str) -> WorkItem {
        WorkItem {
            id: id.to_string(),
            provider: "github".to_string(),
            repo: "acme/web".to_string(),
            number: 1,
            title: "t".to_string(),
            body: String::new(),
            state: "open".to_string(),
            url: String::new(),
            labels: vec![],
            parent_number: None,
        }
    }

    #[test]
    fn state_badge_maps_open_closed_and_unknown() {
        assert_eq!(work_item_state_badge("open"), ("OPEN", "active"));
        // Casing is normalized.
        assert_eq!(work_item_state_badge("OPEN"), ("OPEN", "active"));
        assert_eq!(work_item_state_badge("closed"), ("CLOSED", "done"));
        assert_eq!(work_item_state_badge("weird"), ("UNKNOWN", "neutral"));
    }

    // ── ancestor_path ───────────────────────────────────────────────────────

    fn make_wi(number: u64, parent_number: Option<u64>) -> WorkItem {
        WorkItem {
            id: format!("github:o/r#{number}"),
            provider: "github".to_string(),
            repo: "o/r".to_string(),
            number,
            title: format!("Issue {number}"),
            body: String::new(),
            state: "open".to_string(),
            url: String::new(),
            labels: Vec::new(),
            parent_number,
        }
    }

    #[test]
    fn ancestor_path_three_level_chain() {
        // root(1) -> child(2) -> grandchild(3)
        let root = make_wi(1, None);
        let child = make_wi(2, Some(1));
        let grandchild = make_wi(3, Some(2));
        let items = [root, child, grandchild];
        let by_number: std::collections::HashMap<u64, &WorkItem> =
            items.iter().map(|it| (it.number, it)).collect();

        // Root has no ancestors.
        assert_eq!(ancestor_path(&by_number, &items[0]), vec![] as Vec<u64>);
        // Child has one ancestor: root.
        assert_eq!(ancestor_path(&by_number, &items[1]), vec![1]);
        // Grandchild has two ancestors: root, then child.
        assert_eq!(ancestor_path(&by_number, &items[2]), vec![1, 2]);
    }

    #[test]
    fn ancestor_path_cycle_guard_stops_walk() {
        // Malformed cycle: 10 -> 11 -> 10 (should stop, not infinite-loop).
        let a = make_wi(10, Some(11));
        let b = make_wi(11, Some(10));
        let items = [a, b];
        let by_number: std::collections::HashMap<u64, &WorkItem> =
            items.iter().map(|it| (it.number, it)).collect();
        // The cycle is detected after at most 1 real step; the result is finite.
        let path_a = ancestor_path(&by_number, &items[0]);
        let path_b = ancestor_path(&by_number, &items[1]);
        // Neither path should contain a repeated number.
        assert!(path_a.len() <= 2, "cycle guard kept path short for a");
        assert!(path_b.len() <= 2, "cycle guard kept path short for b");
    }

    #[test]
    fn ancestor_path_missing_ancestor_stops_walk() {
        // child(5) -> parent(99), but 99 is not in the list.
        let child = make_wi(5, Some(99));
        let items = [child];
        let by_number: std::collections::HashMap<u64, &WorkItem> =
            items.iter().map(|it| (it.number, it)).collect();
        // Walk records 99 as the first ancestor, then stops (99 not in by_number).
        let path = ancestor_path(&by_number, &items[0]);
        assert_eq!(path, vec![99]);
    }

    // ── build_work_item_rows ────────────────────────────────────────────────

    #[test]
    fn build_work_item_rows_two_level_parent_child_and_standalone() {
        // 2-level: root(10) -> child(11), standalone(99).
        let root = make_wi(10, None);
        let child = make_wi(11, Some(10));
        let standalone = make_wi(99, None);
        let rows = build_work_item_rows(&[root, child, standalone]);
        assert_eq!(rows.len(), 3);
        // max_depth = 1 (child has depth 1) → tiers = 1 → exactly ONE grouping column
        // per row (no phantom extra tier for a flat epic→children tree).
        assert_eq!(rows[0].hierarchy_cols.len(), 1, "root row has 1 col");
        assert_eq!(rows[1].hierarchy_cols.len(), 1, "child row has 1 col");
        assert_eq!(rows[2].hierarchy_cols.len(), 1, "standalone row has 1 col");

        // Root (number 10, has children): grouped under its own label.
        assert_eq!(rows[0].hierarchy_cols[0], "#10: Issue 10");
        // Child (number 11, leaf): grouped under its PARENT's label — a ROW in the
        // parent's group, NOT a phantom one-item subgroup named after itself.
        assert_eq!(rows[1].hierarchy_cols[0], "#10: Issue 10");
        // Standalone.
        assert_eq!(rows[2].hierarchy_cols[0], "(no parent)");
    }

    #[test]
    fn build_work_item_rows_three_level_chain() {
        // root(1) -> child(2) -> grandchild(3), standalone(9).
        let root = make_wi(1, None);
        let child = make_wi(2, Some(1));
        let grandchild = make_wi(3, Some(2));
        let standalone = make_wi(9, None);
        let items = [root, child, grandchild, standalone];
        let rows = build_work_item_rows(&items);
        assert_eq!(rows.len(), 4);
        // max_depth = 2 → tiers = 2 → each row has 2 hierarchy cols (one per ancestor
        // tier; the item itself is the row, not an extra tier).
        for row in &rows {
            assert_eq!(row.hierarchy_cols.len(), 2, "all rows have 2 cols at max_depth=2");
        }

        // Root (number 1, depth 0, has children): own label in both tiers.
        assert_eq!(rows[0].hierarchy_cols[0], "#1: Issue 1");
        assert_eq!(rows[0].hierarchy_cols[1], "#1: Issue 1");

        // Child (number 2, depth 1, IS a parent of 3): lvl0=root, lvl1=own — it heads
        // its own subgroup so its grandchild nests under it.
        assert_eq!(rows[1].hierarchy_cols[0], "#1: Issue 1");
        assert_eq!(rows[1].hierarchy_cols[1], "#2: Issue 2");

        // Grandchild (number 3, depth 2, leaf): lvl0=root, lvl1=its parent (#2).
        assert_eq!(rows[2].hierarchy_cols[0], "#1: Issue 1");
        assert_eq!(rows[2].hierarchy_cols[1], "#2: Issue 2");

        // Standalone: all cols = "(no parent)".
        assert_eq!(rows[3].hierarchy_cols[0], "(no parent)");
        assert_eq!(rows[3].hierarchy_cols[1], "(no parent)");
    }

    #[test]
    fn build_work_item_rows_orphan_child_uses_not_pulled_label() {
        // Child whose parent is NOT in the fetched list (e.g. filtered/closed).
        // ancestor_path records 99 as the first ancestor; since 99 is not in
        // by_number the label becomes "#99: (not pulled)".
        let orphan = make_wi(20, Some(99));
        let rows = build_work_item_rows(&[orphan]);
        assert_eq!(rows.len(), 1);
        // max_depth = 1 (orphan has depth 1) → tiers = 1 → one col. The orphan is a row
        // under its not-pulled parent's group, not a self-subgroup.
        assert_eq!(rows[0].hierarchy_cols.len(), 1);
        assert_eq!(rows[0].hierarchy_cols[0], "#99: (not pulled)");
    }

    #[test]
    fn build_work_item_rows_all_standalone_single_col() {
        // When no item has a parent the table is flat (max_depth=0, 1 col each).
        let a = make_wi(1, None);
        let b = make_wi(2, None);
        let rows = build_work_item_rows(&[a, b]);
        for row in &rows {
            assert_eq!(row.hierarchy_cols.len(), 1);
            assert_eq!(row.hierarchy_cols[0], "(no parent)");
        }
    }

    // ── render_pulled_issues_for_chat ───────────────────────────────────────

    #[test]
    fn render_pulled_issues_for_chat_three_level_indentation() {
        // root(1) -> child(2) -> grandchild(3), standalone(9).
        let root = make_wi(1, None);
        let child = make_wi(2, Some(1));
        let grandchild = make_wi(3, Some(2));
        let standalone = make_wi(9, None);
        let items = [root, child, grandchild, standalone];
        let section = render_pulled_issues_for_chat(&items);

        // Root at depth 0: no indent.
        assert!(section.contains("- #1 [open]: Issue 1\n"), "root at no indent");
        // Child at depth 1: 2-space indent.
        assert!(section.contains("  - #2 [open]: Issue 2\n"), "child 2-space indent");
        // Grandchild at depth 2: 4-space indent.
        assert!(section.contains("    - #3 [open]: Issue 3\n"), "grandchild 4-space indent");
        // Standalone at depth 0: no indent.
        assert!(section.contains("- #9 [open]: Issue 9\n"), "standalone no indent");
        // No "Epic" wording in output.
        assert!(!section.contains("Epic"), "no Epic wording in chat output");
    }

    #[test]
    fn render_pulled_issues_for_chat_orphan_rendered_at_root() {
        // Child whose parent (99) is not in the list: rendered at depth 0.
        let orphan = make_wi(5, Some(99));
        let section = render_pulled_issues_for_chat(&[orphan]);
        assert!(section.contains("- #5 [open]: Issue 5\n"), "orphan at root level");
    }

    #[test]
    fn render_pulled_issues_for_chat_empty_returns_empty() {
        assert_eq!(render_pulled_issues_for_chat(&[]), String::new());
    }

    #[test]
    fn labels_summary_joins_and_placeholders() {
        assert_eq!(labels_summary(&[]), "—");
        assert_eq!(labels_summary(&["bug".to_string()]), "bug");
        assert_eq!(
            labels_summary(&["bug".to_string(), "ui".to_string()]),
            "bug, ui"
        );
    }

    #[test]
    fn create_or_open_label_dedup_logic() {
        assert_eq!(create_or_open_label(false), "Create Unit of Work from this issue");
        assert_eq!(create_or_open_label(true), "Open Unit of Work");
    }

    #[test]
    fn existing_uow_for_matches_by_work_item_id() {
        let uows = vec![
            UowListEntry {
                id: "uow-1".to_string(),
                work_item: Some(wi("github:acme/web#10")),
                stage: UowStage::Development,
                authoring: false,
            },
            UowListEntry {
                id: "uow-2".to_string(),
                work_item: Some(wi("github:acme/web#11")),
                stage: UowStage::Intake,
                authoring: false,
            },
        ];
        // A match returns the right UoW (drives "Open Unit of Work").
        let found = existing_uow_for(&uows, "github:acme/web#11");
        assert_eq!(found.map(|u| u.id.as_str()), Some("uow-2"));
        // No match -> None (drives "Create Unit of Work").
        assert!(existing_uow_for(&uows, "github:acme/web#99").is_none());
        // The dedup display logic composes: no UoW -> Create label.
        assert_eq!(
            create_or_open_label(existing_uow_for(&uows, "github:acme/web#99").is_some()),
            "Create Unit of Work from this issue"
        );
        assert_eq!(
            create_or_open_label(existing_uow_for(&uows, "github:acme/web#10").is_some()),
            "Open Unit of Work"
        );
    }

    #[test]
    fn active_mention_partial_detects_trailing_at_token() {
        // A bare `@` at the tail is an active token with an empty partial.
        assert_eq!(active_mention_partial("hey @"), Some(""));
        // `@oct` -> partial `oct`.
        assert_eq!(active_mention_partial("hey @oct"), Some("oct"));
        // No trailing token -> None.
        assert_eq!(active_mention_partial("hey there"), None);
        // A completed mention followed by a space is no longer the trailing token.
        assert_eq!(active_mention_partial("hey @octocat "), None);
        // An email-ish token (second `@`) is not a mention.
        assert_eq!(active_mention_partial("ping a@b"), None);
        // Empty input -> None.
        assert_eq!(active_mention_partial(""), None);
    }

    #[test]
    fn apply_mention_selection_replaces_trailing_token() {
        // Completes the active partial with the chosen login + trailing space.
        assert_eq!(apply_mention_selection("hey @oct", "octocat"), "hey @octocat ");
        // A bare `@` completes to the full handle.
        assert_eq!(apply_mention_selection("hey @", "hubot"), "hey @hubot ");
        // No active token -> appends a mention (with a separating space).
        assert_eq!(apply_mention_selection("hello", "octocat"), "hello @octocat ");
        // Appending onto empty input needs no leading space.
        assert_eq!(apply_mention_selection("", "octocat"), "@octocat ");
    }

    #[test]
    fn filter_mention_candidates_is_case_insensitive_and_capped() {
        let users: Vec<String> = vec!["Octocat", "octo-bot", "hubot", "alice"]
            .into_iter()
            .map(String::from)
            .collect();
        // Case-insensitive `contains` match on the partial.
        let m = filter_mention_candidates(&users, "OCT");
        assert_eq!(m, vec!["Octocat", "octo-bot"]);
        // Empty partial returns the leading set (so a bare `@` shows suggestions).
        assert_eq!(filter_mention_candidates(&users, "").len(), 4);
        // A non-match yields an empty list (dropdown hidden).
        assert!(filter_mention_candidates(&users, "zzz").is_empty());
    }

    #[test]
    fn work_item_deserializes_from_contract_shape() {
        let json = r#"{
            "id": "github:acme/web#42",
            "provider": "github",
            "repo": "acme/web",
            "number": 42,
            "title": "Add CSV export",
            "body": "Members CSV",
            "state": "open",
            "url": "https://github.com/acme/web/issues/42",
            "labels": ["enhancement", "ui"]
        }"#;
        let item: WorkItem = serde_json::from_str(json).unwrap();
        assert_eq!(item.id, "github:acme/web#42");
        assert_eq!(item.number, 42);
        assert_eq!(item.labels, vec!["enhancement".to_string(), "ui".to_string()]);
        assert_eq!(labels_summary(&item.labels), "enhancement, ui");
    }

    // ── CiRulesPanel helpers ───────────────────────────────────────────────────

    use super::{
        ci_rule_items_from_proposed, ci_rule_items_from_selections, first_linter,
        RuleSelectionView,
    };

    fn make_rule(id: &str, enforcement: &str, linter: Option<&str>) -> ProposedRuleView {
        ProposedRuleView {
            id: id.to_string(),
            title: format!("{id} title"),
            kind: "structural".to_string(),
            enforcement: enforcement.to_string(),
            options: vec![],
            default_option: None,
            decision_question: None,
            decision_why: None,
            scope: "repo".to_string(),
            domain: "test".to_string(),
            repos: vec![],
            placement: "CONVENTIONS.md".to_string(),
            finding_count: 0,
            recommended: false,
            is_auto_recommended: false,
            verification: "draft".to_string(),
            sources: linter
                .map(|l| vec![RuleSourceView {
                    url: "https://example.com".to_string(),
                    title: "Source".to_string(),
                    linter: Some(l.to_string()),
                }])
                .unwrap_or_default(),
        }
    }

    /// Only mechanical and architectural rules pass the CI-tier filter.
    #[test]
    fn ci_rule_items_from_proposed_filters_to_ci_tier_only() {
        let rules = vec![
            make_rule("MECH-1", "mechanical", Some("eslint: rule")),
            make_rule("ARCH-1", "architectural", None),
            make_rule("STRUCT-1", "structured", None),
            make_rule("PROSE-1", "prose", None),
        ];
        let items = ci_rule_items_from_proposed(&rules);
        assert_eq!(items.len(), 2, "only mechanical + architectural should pass");
        assert!(items.iter().any(|i| i.id == "MECH-1"));
        assert!(items.iter().any(|i| i.id == "ARCH-1"));
        assert!(!items.iter().any(|i| i.id == "STRUCT-1"));
        assert!(!items.iter().any(|i| i.id == "PROSE-1"));
    }

    /// The linter is carried through from the first source with one.
    #[test]
    fn ci_rule_items_from_proposed_carries_linter_from_first_source() {
        let rules = vec![make_rule("MECH-1", "mechanical", Some("eslint: no-unused-vars"))];
        let items = ci_rule_items_from_proposed(&rules);
        assert_eq!(items[0].linter.as_deref(), Some("eslint: no-unused-vars"));
    }

    /// Rules with no linter source yield None.
    #[test]
    fn ci_rule_items_from_proposed_none_linter_when_no_source() {
        let rules = vec![make_rule("ARCH-1", "architectural", None)];
        let items = ci_rule_items_from_proposed(&rules);
        assert!(items[0].linter.is_none());
    }

    /// Empty input produces empty output.
    #[test]
    fn ci_rule_items_from_proposed_empty_input() {
        assert!(ci_rule_items_from_proposed(&[]).is_empty());
    }

    /// `ci_rule_items_from_selections` joins by rule_id and filters to CI tier.
    #[test]
    fn ci_rule_items_from_selections_joins_and_filters() {
        let corpus = vec![
            make_rule("MECH-1", "mechanical", Some("eslint: rule")),
            make_rule("ARCH-1", "architectural", None),
            make_rule("PROSE-1", "prose", None),
        ];
        let selections = vec![
            RuleSelectionView { rule_id: "MECH-1".to_string(), chosen_option: None, repos: vec![] },
            RuleSelectionView { rule_id: "ARCH-1".to_string(), chosen_option: None, repos: vec![] },
            RuleSelectionView { rule_id: "PROSE-1".to_string(), chosen_option: None, repos: vec![] },
        ];
        let items = ci_rule_items_from_selections(&selections, &corpus);
        assert_eq!(items.len(), 2, "prose should be excluded");
        assert!(items.iter().any(|i| i.id == "MECH-1"));
        assert!(items.iter().any(|i| i.id == "ARCH-1"));
    }

    /// A selection whose rule_id is not in the corpus is silently dropped.
    #[test]
    fn ci_rule_items_from_selections_drops_unknown_rule_ids() {
        let corpus = vec![make_rule("MECH-1", "mechanical", None)];
        let selections = vec![
            RuleSelectionView { rule_id: "MECH-1".to_string(), chosen_option: None, repos: vec![] },
            RuleSelectionView { rule_id: "GHOST-99".to_string(), chosen_option: None, repos: vec![] },
        ];
        let items = ci_rule_items_from_selections(&selections, &corpus);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "MECH-1");
    }

    /// `first_linter` returns the linter from the first matching source.
    #[test]
    fn first_linter_returns_first_nonempty_linter() {
        let rule = ProposedRuleView {
            sources: vec![
                RuleSourceView { url: "u1".to_string(), title: "S1".to_string(), linter: None },
                RuleSourceView { url: "u2".to_string(), title: "S2".to_string(), linter: Some("mypy".to_string()) },
            ],
            ..make_rule("R-1", "mechanical", None)
        };
        assert_eq!(first_linter(&rule).as_deref(), Some("mypy"));
    }

    /// `first_linter` skips empty-string linters.
    #[test]
    fn first_linter_skips_empty_string() {
        let rule = ProposedRuleView {
            sources: vec![RuleSourceView {
                url: "u".to_string(),
                title: "S".to_string(),
                linter: Some(String::new()),
            }],
            ..make_rule("R-1", "mechanical", None)
        };
        assert!(first_linter(&rule).is_none());
    }

    // ── CI-security Part B: scan-time preview findings in triage ──────────────

    /// The Authority column's classification: a preview finding reads "preview"
    /// (distinct from an enforced floor hit and from an AI-advisory finding). This
    /// mirrors the inline accessor in `finding_columns`.
    fn authority_label(f: &FindingView) -> &'static str {
        if f.preview {
            "preview"
        } else if is_enforced_floor(&f.rule_id) {
            "enforced"
        } else {
            "advisory"
        }
    }

    #[test]
    fn preview_finding_labeled_distinctly() {
        // A scan-time preview finding (deterministic tool, not yet wired): "preview".
        let preview: FindingView = serde_json::from_str(
            r#"{"repo":"me/api","path":"q.py","line":12,"rule_id":"S608",
                "severity":"medium","snippet":"x","detail":"d",
                "preview":true,"preview_tool":"ruff"}"#,
        )
        .unwrap();
        assert!(preview.preview);
        assert_eq!(preview.preview_tool.as_deref(), Some("ruff"));
        assert_eq!(authority_label(&preview), "preview");

        // An enforced floor finding stays "enforced".
        let floor: FindingView = serde_json::from_str(
            r#"{"repo":"me/api","path":"a.rs","line":1,
                "rule_id":"SEC-NO-HARDCODED-SECRETS-1","severity":"high",
                "snippet":"x","detail":"d"}"#,
        )
        .unwrap();
        assert!(!floor.preview, "absent preview field defaults to false (back-compat)");
        assert_eq!(authority_label(&floor), "enforced");

        // An AI-advisory finding (no preview, not a floor id) stays "advisory".
        let ai: FindingView = serde_json::from_str(
            r#"{"repo":"me/api","path":"x.rs","line":2,"rule_id":"AI-FOO",
                "severity":"medium","snippet":"x","detail":"d"}"#,
        )
        .unwrap();
        assert_eq!(authority_label(&ai), "advisory");
    }

    /// Pure: idle duration formatting from milliseconds.
    #[test]
    fn format_idle_formats_durations() {
        assert_eq!(format_idle(0), "0s");
        assert_eq!(format_idle(5_000), "5s");
        assert_eq!(format_idle(59_000), "59s");
        assert_eq!(format_idle(60_000), "1m");
        assert_eq!(format_idle(65_000), "1m 5s");
        assert_eq!(format_idle(90_000), "1m 30s");
        assert_eq!(format_idle(3_600_000), "60m");
    }

    /// Pure: cancellable-state predicate.
    #[test]
    fn run_is_cancellable_predicate() {
        // Running states are cancellable.
        assert!(run_is_cancellable("executing", false));
        assert!(run_is_cancellable("gating", false));
        assert!(run_is_cancellable("awaiting_clarification", false));
        // Terminal states are not cancellable.
        assert!(!run_is_cancellable("failed", true));
        assert!(!run_is_cancellable("cancelled", true));
        // done=true always non-cancellable.
        assert!(!run_is_cancellable("executing", true));
        // failed/cancelled with done=false are also non-cancellable (status check).
        assert!(!run_is_cancellable("failed", false));
        assert!(!run_is_cancellable("cancelled", false));
    }

    /// Pure: stall banner visibility predicate.
    #[test]
    fn run_stall_banner_visible_predicate() {
        assert!(run_stall_banner_visible(true, false));
        assert!(!run_stall_banner_visible(false, false));
        assert!(!run_stall_banner_visible(true, true));
        assert!(!run_stall_banner_visible(false, true));
    }

    /// `live_event_style` maps the "stall" family to the amber/warning treatment.
    #[test]
    fn live_event_style_stall_family() {
        let (label, cls) = live_event_style("stall", "");
        assert_eq!(label, "STALL");
        assert_eq!(cls, "live-event stall");
    }

    /// `RunView` deserializes with back-compat defaults when stall fields are absent.
    #[test]
    fn run_view_back_compat_defaults() {
        let json = r#"{"id":"r1","story_id":"s1","status":"executing","events":[],"done":false,"mode":"scripted"}"#;
        let rv: RunView = serde_json::from_str(json).unwrap();
        assert_eq!(rv.idle_ms, 0);
        assert!(!rv.stalled);
        assert_eq!(rv.stall_threshold_ms, 0);
        assert!(rv.failure_reason.is_none());
    }

    /// `RunView` deserializes with stall fields when present (new server shape).
    #[test]
    fn run_view_parses_stall_fields() {
        let json = r#"{
            "id":"r2","story_id":"s1","status":"executing","events":[],"done":false,"mode":"live",
            "idle_ms":95000,"stalled":true,"stall_threshold_ms":120000,
            "stall_policy":"alert","failure_reason":null
        }"#;
        let rv: RunView = serde_json::from_str(json).unwrap();
        assert_eq!(rv.idle_ms, 95_000);
        assert!(rv.stalled);
        assert_eq!(rv.stall_threshold_ms, 120_000);
        assert_eq!(rv.stall_policy, "alert");
        assert!(rv.failure_reason.is_none());
    }

    /// `RunView` captures `failure_reason` for failed runs.
    #[test]
    fn run_view_parses_failure_reason() {
        let json = r#"{
            "id":"r3","story_id":"s1","status":"failed","events":[],"done":true,"mode":"live",
            "idle_ms":0,"stalled":false,"stall_threshold_ms":120000,
            "stall_policy":"cancel","failure_reason":"Stall timeout exceeded"
        }"#;
        let rv: RunView = serde_json::from_str(json).unwrap();
        assert_eq!(rv.status, "failed");
        assert_eq!(rv.failure_reason.as_deref(), Some("Stall timeout exceeded"));
    }

    /// `run_status_badge` maps `failed` and `cancelled` to their correct badge variants.
    #[test]
    fn run_status_badge_terminal_states() {
        let (label, cls) = run_status_badge("failed");
        assert_eq!(label, "FAILED");
        assert_eq!(cls, "error");
        let (label, cls) = run_status_badge("cancelled");
        assert_eq!(label, "CANCELLED");
        assert_eq!(cls, "neutral");
    }

    /// `StallThresholdsView` deserializes and defaults correctly.
    #[test]
    fn stall_thresholds_view_defaults() {
        // With both fields present.
        let s: StallThresholdsView = serde_json::from_str(r#"{"watched_secs":60,"routine_secs":300}"#).unwrap();
        assert_eq!(s.watched_secs, 60);
        assert_eq!(s.routine_secs, 300);
        // Back-compat: no fields → defaults.
        let d: StallThresholdsView = serde_json::from_str("{}").unwrap();
        assert_eq!(d.watched_secs, 120);
        assert_eq!(d.routine_secs, 600);
    }

    /// `JobStatusEnvelope` parses the wrapped job shape.
    #[test]
    fn job_status_envelope_parses_wrapped_job() {
        let json = r#"{
            "job": {"status": "running", "done": 2, "total": 10, "findings": [], "deterministic": {"tools":[],"done":0,"total":0}},
            "idle_ms": 5000,
            "cancel_requested": false
        }"#;
        let env: JobStatusEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(env.job.status, "running");
        assert_eq!(env.job.done, 2);
        assert_eq!(env.idle_ms, Some(5000));
        assert!(!env.cancel_requested);
    }

    // ── Decisions-review surface helpers (pure) ───────────────────────────────────

    #[test]
    fn is_placeholder_note_detects_live_mode_off_and_empty() {
        use super::uow::is_placeholder_note;
        assert!(is_placeholder_note(
            "Investigation pending — live mode is off, so no analysis agent ran."
        ));
        assert!(is_placeholder_note("  "));
        assert!(is_placeholder_note(""));
        // A real note is NOT a placeholder.
        assert!(!is_placeholder_note(
            "The export must exclude archived members; chose cursor pagination."
        ));
    }

    #[test]
    fn slugify_decision_label_is_kebab_and_safe() {
        use super::uow::slugify_decision_label;
        assert_eq!(
            slugify_decision_label("Auth strategy: JWT vs session"),
            "auth-strategy-jwt-vs-session"
        );
        assert_eq!(slugify_decision_label("  Trim --- me  "), "trim-me");
        // No alphanumerics → a stable fallback (never an empty artifact-id segment).
        assert_eq!(slugify_decision_label("???"), "decision");
        assert_eq!(slugify_decision_label(""), "decision");
    }

    #[test]
    fn reviewed_for_placeholder_waives_review_when_no_real_output() {
        use super::uow::reviewed_for_placeholder;
        // Real output: the note-review requirement stands.
        assert!(!reviewed_for_placeholder(false, false));
        assert!(reviewed_for_placeholder(true, false));
        // No real output (placeholder/absent): review requirement is waived.
        assert!(reviewed_for_placeholder(false, true));
        assert!(reviewed_for_placeholder(true, true));
    }

    #[test]
    fn investigation_review_view_deserializes_outcome_tag() {
        use super::uow::{DecisionOutcomeView, InvestigationReviewView};
        let json = r#"{
            "story_id": "S-1",
            "note_present": true,
            "note": { "story_id": "S-1", "note": "analysis", "reviewed": true,
                      "provenance": { "actor": "ai", "at": "2026-06-24T00:00:00Z" } },
            "decisions": [
                { "artifact_id": "S-1/decision/a", "story_id": "S-1", "label": "A",
                  "question": "Q?", "rationale": "R", "alternatives_considered": [],
                  "outcome": { "state": "approved" },
                  "provenance": { "actor": "user", "at": "2026-06-24T00:00:00Z" } },
                { "artifact_id": "S-1/decision/b", "story_id": "S-1", "label": "B",
                  "question": "", "rationale": "", "alternatives_considered": [],
                  "outcome": { "state": "rejected", "reason": "needs work" },
                  "provenance": { "actor": "user", "at": "2026-06-24T00:00:00Z" } }
            ]
        }"#;
        let v: InvestigationReviewView = serde_json::from_str(json).unwrap();
        assert!(v.note_present);
        assert!(v.note.unwrap().reviewed);
        assert_eq!(v.decisions.len(), 2);
        assert_eq!(v.decisions[0].outcome, DecisionOutcomeView::Approved);
        match &v.decisions[1].outcome {
            DecisionOutcomeView::Rejected { reason } => assert_eq!(reason, "needs work"),
            other => panic!("expected rejected, got {other:?}"),
        }
    }

    // ══════════════════════════════════════════════════════════════════════════
    // Tier 2 — network-helper tests (wiremock against a mock BFF).
    //
    // Each helper here was converted from `crate::BFF_URL` to `crate::bff_base()`
    // (the test seam). We point that seam at a wiremock server via the
    // CAMERATA_BFF_URL env override, call the helper, and assert the request it
    // issues (method + path, and for mutating calls the EXACT body via body_json
    // with `.expect(1)` so the body is verified on server drop).
    //
    // CAMERATA_BFF_URL is process-global, and cargo runs tests on parallel
    // threads, so EVERY test that sets it carries `#[serial_test::serial(bff_env)]`
    // (tokio outer, serial inner) to serialize them crate-wide. `set_var` +
    // `remove_var` around the call still bracket the override.
    // ══════════════════════════════════════════════════════════════════════════

    /// `fetch_usage` GETs /api/usage and parses the cumulative usage snapshot.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_usage_parses_snapshot() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/usage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "input_tokens": 1000,
                "output_tokens": 500,
                "total_cost_usd": 0.42,
                "calls": 7,
                "by_model": [ { "model": "claude-sonnet-4-6", "tokens": 1500, "cost": 0.42, "calls": 7 } ],
                "rate_limited": false
            })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let u = super::fetch_usage().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let u = u.expect("usage parses");
        assert_eq!(u.total_tokens(), 1500);
        assert_eq!(u.calls, 7);
        assert!(!u.rate_limited);
        assert_eq!(u.by_model.len(), 1);
        assert_eq!(u.by_model[0].model, "claude-sonnet-4-6");
    }

    /// `fetch_active_project` GETs /api/projects/active and flattens the `Option<ProjectView>`.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_active_project_parses_and_flattens() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/active"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "proj-9", "name": "Acme", "max_iterations": 3
            })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let p = super::fetch_active_project().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let p = p.expect("active project parses");
        assert_eq!(p.id, "proj-9");
        assert_eq!(p.name, "Acme");
        assert_eq!(p.max_iterations, 3);
    }

    /// `fetch_active_project` returns None when the server reports a `null` active project.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_active_project_null_is_none() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/active"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::Value::Null))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let p = super::fetch_active_project().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(p.is_none(), "a null active project flattens to None");
    }

    /// `create_project` POSTs {name, repos} and reads back the nested `project`.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn create_project_posts_name_and_repos() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects"))
            .and(body_json(serde_json::json!({ "name": "New", "repos": ["acme/web"] })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "project": { "id": "p-new", "name": "New", "repos": ["acme/web"] }
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let p = super::create_project("New", vec!["acme/web".to_string()]).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let p = p.expect("project parses from the nested key");
        assert_eq!(p.id, "p-new");
        assert_eq!(p.repos, vec!["acme/web".to_string()]);
    }

    /// `set_active_project` POSTs {id} and maps a 2xx to true.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn set_active_project_posts_id() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/active"))
            .and(body_json(serde_json::json!({ "id": "p-7" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::set_active_project("p-7").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok, "2xx maps to true");
    }

    /// `set_project_tier_map` POSTs all four bands in one round-trip.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn set_project_tier_map_posts_all_bands() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/p-1/tier-map"))
            .and(body_json(serde_json::json!({
                "fast": ["haiku-x"],
                "balanced": ["sonnet-x"],
                "strongest": "opus-x",
                "vision": []
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let tm = TierMapView {
            strongest: "opus-x".to_string(),
            balanced: vec!["sonnet-x".to_string()],
            fast: vec!["haiku-x".to_string()],
            vision: vec![],
        };
        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::set_project_tier_map("p-1", &tm).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok);
    }

    /// `set_project_vision_enabled` POSTs {enabled} to the per-project endpoint.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn set_project_vision_enabled_posts_flag() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/p-1/vision-enabled"))
            .and(body_json(serde_json::json!({ "enabled": true })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::set_project_vision_enabled("p-1", true).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok);
    }

    /// `set_project_step_model` POSTs {step, model} (patch semantics: one step per call).
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn set_project_step_model_posts_step_and_model() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/p-1/step-models"))
            .and(body_json(serde_json::json!({ "step": "audit", "model": "claude-opus-4-8" })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::set_project_step_model("p-1", "audit", "claude-opus-4-8").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok);
    }

    /// `set_project_step_model` is a single endpoint parameterised by the `step` string.
    /// One test exercises two representative non-fleet step variants (clarification,
    /// decomposition) to lock that the step name is passed through verbatim in the body
    /// (a per-variant wiremock test would be redundant — same path, same shape).
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn set_project_step_model_passes_step_name_through_for_each_variant() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        for step in ["clarification", "decomposition"] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/projects/p-9/step-models"))
                .and(body_json(serde_json::json!({ "step": step, "model": "claude-sonnet-4-6" })))
                .respond_with(ResponseTemplate::new(200))
                .expect(1)
                .mount(&server)
                .await;

            std::env::set_var("CAMERATA_BFF_URL", server.uri());
            let ok = super::set_project_step_model("p-9", step, "claude-sonnet-4-6").await;
            std::env::remove_var("CAMERATA_BFF_URL");

            assert!(ok, "step variant `{step}` POSTs its name verbatim and maps 2xx to true");
        }
    }

    /// `fetch_projects` GETs /api/projects and parses a `Vec<ProjectView>`. Asserts the
    /// list is decoded in order and the per-project serde defaults fill in (a bare
    /// {id, name} project is valid).
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_projects_parses_the_list() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": "p-1", "name": "Acme", "repos": ["acme/web"] },
                { "id": "p-2", "name": "Globex" }
            ])))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let projects = super::fetch_projects().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let projects = projects.expect("the projects list parses");
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0].id, "p-1");
        assert_eq!(projects[0].repos, vec!["acme/web".to_string()]);
        // The second project omits `repos`; the serde default fills an empty Vec.
        assert_eq!(projects[1].id, "p-2");
        assert_eq!(projects[1].name, "Globex");
        assert!(projects[1].repos.is_empty());
        // `max_iterations` defaults to 1 when absent (back-compat default).
        assert_eq!(projects[1].max_iterations, 1);
    }

    /// `set_max_iterations` POSTs {max_iterations} to the loop-guard endpoint.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn set_max_iterations_posts_value() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/p-1/max-iterations"))
            .and(body_json(serde_json::json!({ "max_iterations": 5 })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::set_max_iterations("p-1", 5).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok);
    }

    /// `set_product_brief` POSTs {product_brief}.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn set_product_brief_posts_text() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/p-1/product-brief"))
            .and(body_json(serde_json::json!({ "product_brief": "We build X for Y." })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::set_product_brief("p-1", "We build X for Y.").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok);
    }

    /// `add_memory_entry` POSTs {kind, text} to the project memory endpoint.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn add_memory_entry_posts_kind_and_text() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/p-1/memory"))
            .and(body_json(serde_json::json!({ "kind": "gotcha", "text": "Watch the FK index." })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::add_memory_entry("p-1", "gotcha", "Watch the FK index.").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok);
    }

    /// `patch_memory_status` POSTs {status} to the per-entry endpoint.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn patch_memory_status_posts_status() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/p-1/memory/e-9"))
            .and(body_json(serde_json::json!({ "status": "approved" })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::patch_memory_status("p-1", "e-9", "approved").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok);
    }

    /// `save_memory_edit` POSTs {kind, text} to the per-entry endpoint.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn save_memory_edit_posts_kind_and_text() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/p-1/memory/e-9"))
            .and(body_json(serde_json::json!({ "kind": "decision", "text": "Edited." })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::save_memory_edit("p-1", "e-9", "decision", "Edited.").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok);
    }

    /// `delete_memory_entry` DELETEs the per-entry endpoint.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn delete_memory_entry_sends_delete() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/projects/p-1/memory/e-9"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::delete_memory_entry("p-1", "e-9").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok);
    }

    /// `set_operating_principles` POSTs the full list under {operating_principles}.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn set_operating_principles_posts_full_list() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/p-1/operating-principles"))
            .and(body_json(serde_json::json!({
                "operating_principles": [ { "id": "op-1", "text": "Write tests.", "enabled": true } ]
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let principles = vec![super::OperatingPrincipleView {
            id: "op-1".to_string(),
            text: "Write tests.".to_string(),
            enabled: true,
        }];
        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::set_operating_principles("p-1", &principles).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok);
    }

    /// `set_project_l3_review` POSTs {enabled, model}.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn set_project_l3_review_posts_enabled_and_model() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/p-1/l3-review"))
            .and(body_json(serde_json::json!({ "enabled": true, "model": "claude-opus-4-8" })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::set_project_l3_review("p-1", true, "claude-opus-4-8").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok);
    }

    /// `set_project_stall_thresholds` POSTs {watched_secs, routine_secs}.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn set_project_stall_thresholds_posts_both_secs() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/p-1/stall-thresholds"))
            .and(body_json(serde_json::json!({ "watched_secs": 90, "routine_secs": 450 })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::set_project_stall_thresholds("p-1", 90, 450).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok);
    }

    /// `delete_project` DELETEs /api/projects/:id.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn delete_project_sends_delete() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/projects/p-9"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::delete_project("p-9").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok);
    }

    /// `fetch_provider` GETs /api/provider and parses the connection view.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_provider_parses_connection() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/provider"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "provider": "github (token)", "live": true
            })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let p = super::fetch_provider().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let p = p.expect("provider parses");
        assert_eq!(p.provider, "github (token)");
        assert!(p.live);
    }

    /// `fetch_gate_probe` POSTs /api/gate-probe and parses the go/no-go.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_gate_probe_parses_verdict() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/gate-probe"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "go": true,
                "layer1_denied": 5,
                "layer1_total": 5,
                "layer1_clean_allowed": true,
                "layer2_bounced": true,
                "layer2_clean": true
            })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let r = super::fetch_gate_probe().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let r = r.expect("gate probe parses");
        assert!(r.go);
        assert_eq!(r.layer1_denied, 5);
        assert!(r.layer1_clean_allowed);
        assert!(r.layer2_clean);
    }

    /// `fetch_app_release` GETs /api/release and parses the update info.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_app_release_parses_update_info() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/release"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "current": "0.4.1",
                "latest": "0.4.2",
                "update_available": true,
                "release_notes": "Bug fixes."
            })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let rel = super::fetch_app_release().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let rel = rel.expect("release parses");
        assert_eq!(rel.current, "0.4.1");
        assert_eq!(rel.latest.as_deref(), Some("0.4.2"));
        assert!(rel.update_available);
    }

    /// `fetch_feature_flags` GETs /api/feature-flags and parses the flag map.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_feature_flags_parses_map() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/feature-flags"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "soc2": true })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let flags = super::fetch_feature_flags().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(flags.soc2, "soc2 flag parses from the server");
    }

    /// `fetch_feature_flags` degrades to an all-off default when the endpoint 500s.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_feature_flags_defaults_on_error() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/feature-flags"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let flags = super::fetch_feature_flags().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(!flags.soc2, "an error degrades to all-off (the conservative default)");
    }

    /// `fetch_app_chat_model` GETs /api/settings and returns the non-empty chat model.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_app_chat_model_returns_non_empty() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/settings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "chat_model": "claude-opus-4-8"
            })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let m = super::fetch_app_chat_model().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert_eq!(m.as_deref(), Some("claude-opus-4-8"));
    }

    /// `save_app_chat_model` POSTs {model} to /api/settings/chat-model.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn save_app_chat_model_posts_model() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings/chat-model"))
            .and(body_json(serde_json::json!({ "model": "claude-sonnet-4-6" })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::save_app_chat_model("claude-sonnet-4-6").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok);
    }

    /// `fetch_reconcile` GETs /api/projects/:id/reconcile and reads `applied` when `ok`.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_reconcile_reads_applied_when_ok() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/p-1/reconcile"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "applied": [
                    { "id": "RULE-1", "repo": "acme/web", "title": "No secrets", "summary": "s" }
                ]
            })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let applied = super::fetch_reconcile("p-1").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let applied = applied.expect("applied list parses");
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].id, "RULE-1");
        assert_eq!(applied[0].repo, "acme/web");
    }

    /// `fetch_reconcile` returns None when the server reports `ok: false`.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_reconcile_none_when_not_ok() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/p-1/reconcile"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": false })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let applied = super::fetch_reconcile("p-1").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(applied.is_none(), "ok:false yields None");
    }

    /// `apply_model_profile` POSTs {profile} and returns the JSON body.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn apply_model_profile_posts_profile() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/p-1/model-profile"))
            .and(body_json(serde_json::json!({ "profile": "max_quality" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let v = super::apply_model_profile("p-1", "max_quality").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let v = v.expect("apply returns a JSON body");
        assert_eq!(v.get("ok").and_then(|b| b.as_bool()), Some(true));
    }

    /// `preview_model_profile` GETs the preview endpoint with the profile query param.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn preview_model_profile_gets_with_query() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/p-1/model-profile/preview"))
            .and(query_param("profile", "balanced"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "preview": true })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let v = super::preview_model_profile("p-1", "balanced").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let v = v.expect("preview returns a JSON body");
        assert_eq!(v.get("preview").and_then(|b| b.as_bool()), Some(true));
    }

    /// `start_dev_run` POSTs the tier_map body to the story-run path and returns
    /// Started with the run id. (Uses a plain story id; `enc_seg`'s percent-encoding
    /// is unit-tested separately so this test's path is unambiguous.)
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn start_dev_run_started_returns_run_id() {
        use super::StartRunOutcome;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/stories/CAM-7/run"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "run_id": "run-7" })))
            .expect(1)
            .mount(&server)
            .await;

        let tm = TierMapView::default();
        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let outcome = super::start_dev_run("CAM-7", &tm, false).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        match outcome {
            StartRunOutcome::Started(id) => assert_eq!(id, "run-7"),
            _ => panic!("expected Started"),
        }
    }

    /// `enc_seg` percent-encodes the reserved chars in `owner/repo#num` story ids so the
    /// '/' doesn't break single-segment routing and the '#' isn't dropped as a fragment.
    #[test]
    fn enc_seg_encodes_slash_and_hash() {
        assert_eq!(super::enc_seg("acme/web#42"), "acme%2Fweb%2342");
        // Unreserved chars pass through unchanged.
        assert_eq!(super::enc_seg("CAM-7_v.1~x"), "CAM-7_v.1~x");
    }

    /// `start_dev_run` maps a 409 with a reason to Blocked(reason).
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn start_dev_run_409_is_blocked_with_reason() {
        use super::StartRunOutcome;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/stories/s-1/run"))
            .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
                "reason": "Decisions are not all approved."
            })))
            .mount(&server)
            .await;

        let tm = TierMapView::default();
        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let outcome = super::start_dev_run("s-1", &tm, false).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        match outcome {
            StartRunOutcome::Blocked(reason) => assert_eq!(reason, "Decisions are not all approved."),
            _ => panic!("expected Blocked"),
        }
    }

    /// `begin_investigation_run` POSTs {model} and returns Started with the run id.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn begin_investigation_run_started_posts_model() {
        use super::BeginInvestigationOutcome;
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/uow/s-1/begin-investigation"))
            .and(body_json(serde_json::json!({ "model": "claude-opus-4-8" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "run_id": "run-3", "story_id": "s-1"
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let outcome = super::begin_investigation_run("s-1", "claude-opus-4-8").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        match outcome {
            BeginInvestigationOutcome::Started(id) => assert_eq!(id, "run-3"),
            _ => panic!("expected Started"),
        }
    }

    /// `begin_investigation_run` maps a 409 with a reason to Blocked(reason).
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn begin_investigation_run_409_is_blocked() {
        use super::BeginInvestigationOutcome;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/uow/s-1/begin-investigation"))
            .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
                "reason": "Not at intake."
            })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let outcome = super::begin_investigation_run("s-1", "m").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        match outcome {
            BeginInvestigationOutcome::Blocked(reason) => assert_eq!(reason, "Not at intake."),
            _ => panic!("expected Blocked"),
        }
    }

    /// `import_project_payload` merges the `overwrite` flag and parses an Imported result.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn import_project_payload_imported() {
        use super::ImportResult;
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // The helper parses the payload, inserts overwrite:true, and re-serialises.
        Mock::given(method("POST"))
            .and(path("/api/projects/import"))
            .and(body_json(serde_json::json!({ "name": "Imp", "overwrite": true })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "project": { "id": "p-imp", "name": "Imp" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let res = super::import_project_payload(r#"{"name":"Imp"}"#, true).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        match res {
            ImportResult::Imported(p) => assert_eq!(p.id, "p-imp"),
            _ => panic!("expected Imported"),
        }
    }

    /// `import_project_payload` surfaces a name-collision Conflict (with name + payload).
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn import_project_payload_conflict() {
        use super::ImportResult;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/import"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "conflict": true, "name": "Existing"
            })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let res = super::import_project_payload(r#"{"name":"Existing"}"#, false).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        match res {
            ImportResult::Conflict { name, payload } => {
                assert_eq!(name, "Existing");
                // The original (flag-free) payload is preserved for the confirmed re-POST.
                assert_eq!(payload, r#"{"name":"Existing"}"#);
            }
            _ => panic!("expected Conflict"),
        }
    }

    // ══════════════════════════════════════════════════════════════════════════
    // Tier 1 — render tests (dioxus-ssr). Render a component headlessly and assert
    // its static STRUCTURE via stable class names / labels. No interaction, no
    // async-loaded data (use_resource is pending on first render → fallback branch).
    // SSR HTML-escapes special chars, so we assert on class names / leading words.
    // ══════════════════════════════════════════════════════════════════════════

    /// `CockpitNotice` is props-only (no context, no hooks beyond the match). Each
    /// `kind` renders its distinct title + body.
    #[test]
    fn cockpit_notice_renders_error_kind() {
        use dioxus::prelude::*;

        fn harness() -> Element {
            rsx! { super::CockpitNotice { kind: "error".to_string() } }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("cockpit-notice-title"), "the title class renders; html=\n{html}");
        // SSR escapes the apostrophe in "Can't" to &#39;, so assert on the apostrophe-free portion.
        assert!(html.contains("reach the engine"), "the error title; html=\n{html}");
    }

    /// `CockpitNotice` "loading" kind renders the distinct "connecting" copy (a different
    /// title + body from the error/empty variants), so the operator sees a transient
    /// "reaching the engine" state rather than the alarming "can't reach" error.
    #[test]
    fn cockpit_notice_renders_loading_kind() {
        use dioxus::prelude::*;

        fn harness() -> Element {
            rsx! { super::CockpitNotice { kind: "loading".to_string() } }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("cockpit-notice-title"), "the title class renders; html=\n{html}");
        // The loading title is "Connecting to the engine…" — distinct from the error copy.
        assert!(html.contains("Connecting to the engine"), "the loading title; html=\n{html}");
        assert!(
            html.contains("Reaching the local Camerata server"),
            "the loading body; html=\n{html}"
        );
        // It must NOT show the error-kind copy (proves the match picked the loading arm).
        assert!(!html.contains("isn&#39;t responding"), "loading is not the error variant; html=\n{html}");
    }

    /// `CockpitNotice` with an unknown kind falls back to the empty-state copy.
    #[test]
    fn cockpit_notice_unknown_kind_is_empty_state() {
        use dioxus::prelude::*;

        fn harness() -> Element {
            rsx! { super::CockpitNotice { kind: "anything-else".to_string() } }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("No stories yet"), "the empty-state title; html=\n{html}");
    }

    /// `GateSelfCheck` renders its title + the run button before any result is fetched
    /// (no context needed; the result use_signal starts None).
    #[test]
    fn gate_self_check_renders_title_and_button() {
        use dioxus::prelude::*;

        fn harness() -> Element {
            rsx! { super::GateSelfCheck {} }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("gate-selfcheck"), "the wrapper class; html=\n{html}");
        assert!(html.contains("Gate self-check"), "the title; html=\n{html}");
        assert!(html.contains("Run gate self-check"), "the run button label; html=\n{html}");
    }

    /// `HomeNav` renders the three project-independent tabs (a bound signal prop; no
    /// context). The active tab carries the `on` modifier class.
    #[test]
    fn home_nav_renders_three_tabs_with_active_modifier() {
        use dioxus::prelude::*;

        fn harness() -> Element {
            let hv = use_signal(|| super::HomeView::Projects);
            rsx! { super::HomeNav { home_view: hv } }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("cockpit-nav"), "the nav wrapper; html=\n{html}");
        assert!(html.contains("Projects"), "the Projects tab; html=\n{html}");
        assert!(html.contains("Docs"), "the Docs tab; html=\n{html}");
        assert!(html.contains("Settings"), "the Settings tab; html=\n{html}");
        // Projects is the active view → its button carries the `on` modifier.
        assert!(html.contains("cockpit-nav-tab on"), "the active tab modifier; html=\n{html}");
    }

    /// `DocsView` renders its two doc tabs + the markdown body wrapper. The body uses
    /// `dangerous_inner_html` (does NOT render under SSR), so we assert the surrounding
    /// structure only, not the rendered markdown.
    #[test]
    fn docs_view_renders_tabs_and_body_wrapper() {
        use dioxus::prelude::*;

        fn harness() -> Element {
            rsx! { super::DocsView {} }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("docs-view"), "the docs wrapper; html=\n{html}");
        assert!(html.contains("User Guide"), "the user-guide tab; html=\n{html}");
        assert!(html.contains("Technical"), "the technical tab; html=\n{html}");
        assert!(html.contains("docs-body"), "the body wrapper (dangerous_inner_html); html=\n{html}");
    }

    /// `UsageMeter` renders its neutral placeholder before the first poll lands (the
    /// usage use_signal starts None, and the use_future fetch is pending under SSR).
    #[test]
    fn usage_meter_renders_loading_placeholder() {
        use dioxus::prelude::*;

        fn harness() -> Element {
            rsx! { super::UsageMeter {} }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("usage-meter-loading"), "the loading placeholder class; html=\n{html}");
    }

    // Build a minimal ProjectView fixture from JSON (it has many serde-default
    // fields; a bare {id, name} fills the rest with defaults).
    fn project_fixture() -> super::ProjectView {
        serde_json::from_value(serde_json::json!({ "id": "p-1", "name": "Acme" }))
            .expect("valid ProjectView fixture")
    }

    /// `ProductBriefEditor` needs the toast context (use_context::<Signal<Vec<Toast>>>),
    /// so the harness provides it. Asserts the card structure + the brief textarea.
    #[test]
    fn product_brief_editor_renders_card_and_textarea() {
        use dioxus::prelude::*;

        fn harness() -> Element {
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            let refresh = use_signal(|| 0u32);
            rsx! { super::ProductBriefEditor { project: project_fixture(), refresh } }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("soft-ctx-card"), "the card wrapper; html=\n{html}");
        assert!(html.contains("Product brief"), "the title; html=\n{html}");
        assert!(html.contains("soft-ctx-brief"), "the brief textarea; html=\n{html}");
        assert!(html.contains("Save brief"), "the save button label; html=\n{html}");
    }

    /// `OperatingPrinciplesEditor` (toast context provided) renders the card + add control.
    #[test]
    fn operating_principles_editor_renders_card_and_add() {
        use dioxus::prelude::*;

        fn harness() -> Element {
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            let refresh = use_signal(|| 0u32);
            rsx! { super::OperatingPrinciplesEditor { project: project_fixture(), refresh } }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("soft-ctx-card"), "the card wrapper; html=\n{html}");
        assert!(html.contains("Operating principles"), "the title; html=\n{html}");
        assert!(html.contains("op-add"), "the add-principle row; html=\n{html}");
        assert!(html.contains("Save principles"), "the save button; html=\n{html}");
    }

    /// `MemoryEditor` (toast context provided) renders the card, the empty-state line
    /// (the fixture has no memory), and the add-learning controls.
    #[test]
    fn memory_editor_renders_card_empty_state_and_add() {
        use dioxus::prelude::*;

        fn harness() -> Element {
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            let refresh = use_signal(|| 0u32);
            rsx! { super::MemoryEditor { project: project_fixture(), refresh } }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("soft-ctx-card"), "the card wrapper; html=\n{html}");
        assert!(html.contains("Project memory"), "the title; html=\n{html}");
        assert!(html.contains("mem-empty"), "the empty-state line (no memory in the fixture); html=\n{html}");
        assert!(html.contains("mem-kind-select"), "the kind selector; html=\n{html}");
    }
}
