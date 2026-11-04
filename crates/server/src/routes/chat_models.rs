//! Model listing and registry, provider policy, chat, usage and the client-lock helpers.

use crate::*;

/// The label (`anthropic/cli` or `anthropic/api`) for the backend the CHAT actually runs on, from
/// the same selection the chat uses ([`AppState::chat_llm`]), plus the refusal reason when the
/// setting is `Api` but no key is configured (the label then still names the SETTING, so it is
/// never claiming a transport the chat will not use).
pub(crate) fn chat_backend_label(state: &AppState) -> (String, Option<String>) {
    match state.chat_llm() {
        Ok(llm) => (llm.backend_label(), None),
        Err(reason) => {
            // `Blocked` only arises for the Api setting with no key: name the setting.
            let transport = match state.settings.chat_backend() {
                crate::llm::ProjectBackend::Api => "api",
                crate::llm::ProjectBackend::Cli => "cli",
            };
            (format!("anthropic/{transport}"), Some(reason))
        }
    }
}

/// The models the UI offers in its selector.
pub(crate) async fn list_models(State(state): State<AppState>) -> Json<serde_json::Value> {
    let (backend, backend_error) = chat_backend_label(&state);
    let models: Vec<_> = crate::llm::models()
        .iter()
        .map(|m| {
            serde_json::json!({
                "vendor": m.vendor,
                "label": m.label,
                "id": m.id,
                "price_in": m.price_in,
                "price_out": m.price_out,
            })
        })
        .collect();
    Json(serde_json::json!({
        "models": models,
        "default": crate::llm::DEFAULT_MODEL,
        "backend": backend,
        "backend_error": backend_error,
    }))
}

/// `GET /api/models/registry` — return the full model registry: OpenRouter cached
/// entries plus a Claude portion that is BACKEND-AWARE (Feature A, see
/// `docs/design/2026-08-27_backend-safety-and-live-models.md`) — the live Anthropic
/// `/v1/models` list when the API backend is active with a key and that fetch has
/// succeeded, else the hardcoded `CLAUDE_REGISTRY_MODELS` catalog (which is also what the
/// CLI backend always gets, since the CLI/subscription model set is not the API's).
/// `openrouter_fetched`/`anthropic_fetched` flags let the client prompt for a key + call
/// the matching refresh endpoint when a portion has never been populated.
pub(crate) async fn get_model_registry(
    State(state): State<AppState>,
) -> Json<crate::model_registry::RegistryResp> {
    let use_live_anthropic = use_live_anthropic_models(&state);
    let models = state.model_registry.entries_for_backend(use_live_anthropic);
    let openrouter_fetched = state.model_registry.openrouter_fetched();
    let anthropic_fetched = state.model_registry.anthropic_fetched();
    Json(crate::model_registry::RegistryResp {
        models,
        openrouter_fetched,
        anthropic_fetched,
    })
}

/// `POST /api/models/registry/refresh` — (re-)fetch the OpenRouter model catalog using
/// the credential stored in the OS keychain. Returns the count of fetched models plus the
/// full updated registry. Returns `{ attempted: false, openrouter_count: 0 }` when the
/// OpenRouter API key is not set (the Claude portion is unaffected).
pub(crate) async fn refresh_model_registry(
    State(state): State<AppState>,
) -> Json<crate::model_registry::RefreshResp> {
    let attempted = state
        .model_registry
        .try_refresh_from_store(state.credential_store.as_ref())
        .await;
    let use_live_anthropic = use_live_anthropic_models(&state);
    let models = state.model_registry.entries_for_backend(use_live_anthropic);
    let openrouter_count = models.iter().filter(|e| e.provider == "openrouter").count();
    Json(crate::model_registry::RefreshResp {
        openrouter_count,
        attempted,
        models,
    })
}

/// `POST /api/models/registry/refresh/anthropic` — (re-)fetch the LIVE Anthropic
/// `/v1/models` catalog using the Anthropic key stored in the OS keychain (or its
/// `ANTHROPIC_API_KEY` env fallback, both via `credentials::anthropic_api_key` — see
/// `try_refresh_anthropic_from_store`). Feature A's manual refresh trigger, mirroring
/// [`refresh_model_registry`] for OpenRouter. Returns `{ attempted: false,
/// anthropic_count: 0 }` when no Anthropic key is present; on any fetch error the cache is
/// set to empty (fail-soft) and the returned registry falls back to the hardcoded
/// catalog — this endpoint never returns a 5xx for an upstream Anthropic failure.
pub(crate) async fn refresh_anthropic_registry(
    State(state): State<AppState>,
) -> Json<crate::model_registry::AnthropicRefreshResp> {
    let attempted = state
        .model_registry
        .try_refresh_anthropic_from_store(state.credential_store.as_ref())
        .await;
    let use_live_anthropic = use_live_anthropic_models(&state);
    let models = state.model_registry.entries_for_backend(use_live_anthropic);
    // Count the Claude portion actually being served post-refresh (live if the backend
    // picked it up, else the hardcoded fallback count) — mirrors how
    // `refresh_model_registry`'s `openrouter_count` reflects the post-refresh state, not
    // just the raw fetch size.
    let anthropic_count = models.iter().filter(|e| e.provider == "claude").count();
    Json(crate::model_registry::AnthropicRefreshResp {
        anthropic_count,
        attempted,
        models,
    })
}

// ── Provider-safety (Pass 2 UI data source) ─────────────────────────────────────
//
// Thin routes over the Pass-1 `camerata_llm::ModelRegistry` / `SettingsStore` seams (see
// `docs/design/2026-07-28_openrouter-provider-safety.md`'s "Pass 2" section). Neither
// route touches enforcement — `provider_policy::provider_constraint_for_request` (the
// trust core) reads the SAME `ModelRegistry` cache and `SettingsStore` these routes
// read/write, so the picker can never show a state the enforcement path disagrees with.

/// One provider's picker-facing record: the full joined pricing + data-policy fields the
/// UI needs to render a row (name, region flag, cost, safety badge). Mirrors
/// `camerata_llm::model_registry::ProviderEndpointInfo` field-for-field; kept as a
/// separate wire type (rather than deriving `Serialize` on the lib type) so the JSON
/// shape is a deliberate, documented contract independent of the internal struct.
#[derive(serde::Serialize)]
pub(crate) struct ProviderEndpointWire {
    pub(crate) slug: String,
    pub(crate) name: String,
    pub(crate) region: Option<String>,
    pub(crate) price_in: f64,
    pub(crate) price_out: f64,
    pub(crate) training: bool,
    pub(crate) retains_prompts: bool,
}

#[derive(serde::Serialize)]
pub(crate) struct ModelProvidersResp {
    pub(crate) model_id: String,
    /// Every provider currently serving `model_id`, per OpenRouter's `/endpoints` +
    /// `/all-providers` join. Empty means either the model has no OpenRouter provider
    /// data at all, or the lazy fetch failed — the UI treats both as "provider data
    /// unavailable" and falls back to Auto (safe/fail-closed enforcement is unaffected
    /// either way; it lives server-side in `provider_policy::provider_constraint_for_request`,
    /// not in what this endpoint happens to return).
    pub(crate) providers: Vec<ProviderEndpointWire>,
}

#[derive(serde::Deserialize)]
pub(crate) struct ModelProvidersQuery {
    pub(crate) model_id: String,
}

/// `GET /api/models/providers?model_id=<id>` — the Pass-2 provider picker's data source.
/// Triggers the lazy per-model OpenRouter provider-safety fetch
/// (`ModelRegistry::ensure_safe_providers_loaded`) if `model_id`'s data isn't already
/// cached in this process, then returns the full per-provider record list
/// (`ModelRegistry::provider_endpoints_for`) for the picker to render rows from. `model_id`
/// is a query param (not a path segment) because OpenRouter model ids contain `/`
/// (e.g. `deepseek/deepseek-chat`), which axum path routing does not accept unescaped.
pub(crate) async fn get_model_providers(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<ModelProvidersQuery>,
) -> Json<ModelProvidersResp> {
    let api_key = crate::credentials::openrouter_api_key(state.credential_store.as_ref());
    state
        .model_registry
        .ensure_safe_providers_loaded(api_key.as_deref(), &q.model_id)
        .await;
    let providers = state
        .model_registry
        .provider_endpoints_for(&q.model_id)
        .unwrap_or_default()
        .into_iter()
        .map(|e| ProviderEndpointWire {
            slug: e.provider_slug,
            name: e.provider_name,
            region: e.region,
            price_in: e.price_in,
            price_out: e.price_out,
            training: e.training,
            retains_prompts: e.retains_prompts,
        })
        .collect();
    Json(ModelProvidersResp {
        model_id: q.model_id,
        providers,
    })
}

#[derive(serde::Deserialize)]
pub(crate) struct ProviderPolicyReq {
    pub(crate) safe_mode: bool,
    #[serde(default)]
    pub(crate) pinned_provider: Option<String>,
}

#[derive(serde::Serialize)]
pub(crate) struct ProviderPolicyResp {
    pub(crate) safe_mode: bool,
    pub(crate) pinned_provider: Option<String>,
}

/// `POST /api/settings/provider-policy` — persist the OpenRouter provider-safety policy
/// (the §3 settings-panel "Data safety" toggle + optional provider pin). Purely a
/// persistence seam: an unsafe/unknown pin is NOT rejected here — the enforcement layer
/// (`provider_policy::provider_constraint_for_request`) already drops an unsafe pin at
/// request-build time (never widens the constraint), so this handler's only job is to
/// store whatever the UI sends. Blank `pinned_provider` values collapse to `None`, same
/// convention as the other settings setters in this file (e.g. `set_chat_model`).
pub(crate) async fn set_provider_policy_handler(
    State(state): State<AppState>,
    Json(req): Json<ProviderPolicyReq>,
) -> Json<ProviderPolicyResp> {
    let pinned_provider = req.pinned_provider.filter(|p| !p.trim().is_empty());
    let updated =
        state
            .settings
            .set_provider_policy(camerata_llm::provider_policy::ProviderPolicy {
                safe_mode: req.safe_mode,
                pinned_provider,
            });
    Json(ProviderPolicyResp {
        safe_mode: updated.provider_policy.safe_mode,
        pinned_provider: updated.provider_policy.pinned_provider,
    })
}

/// One prior turn in a research-chat conversation, sent by the UI with each POST so the
/// model has memory of the thread. role must be "user" or "assistant".
#[derive(serde::Deserialize, Clone, PartialEq, Debug)]
pub(crate) struct ChatTurn {
    pub role: String,
    pub content: String,
}

#[derive(serde::Deserialize)]
pub(crate) struct ChatReq {
    pub(crate) prompt: String,
    #[serde(default)]
    pub(crate) model: String,
    #[serde(default)]
    pub(crate) system: Option<String>,
    /// Prior turns in chronological order (oldest first). Empty on the first message.
    /// The UI accumulates turns client-side and sends them with every POST.
    /// Back-compat: omitted or null → treated as empty (same as before this fix).
    #[serde(default)]
    pub(crate) history: Vec<ChatTurn>,
}

/// Maximum number of prior turns to embed in the prompt. Oldest turns beyond this limit
/// are dropped (FIFO) to keep token usage bounded. A "turn" is one user + one assistant
/// exchange, so 20 turns ≈ 40 messages ≈ several thousand tokens of context at most.
pub(crate) const CHAT_HISTORY_TURN_CAP: usize = 20;

/// Render prior conversation turns into a transcript block that is prepended to the
/// user's new message. When history is empty (first message or back-compat), returns
/// `None` so the caller can fall back to the bare single-prompt path.
///
/// Format:
/// ```text
/// Conversation so far:
/// User: <content>
/// Assistant: <content>
/// ...
///
/// User's new message:
/// <prompt>
/// ```
///
/// Roles are normalized: "user" maps to "User", anything else (including "assistant")
/// maps to "Assistant". The recent-window cap (`CHAT_HISTORY_TURN_CAP`) is applied
/// before rendering — oldest turns are dropped first.
pub(crate) fn render_chat_prompt(history: &[ChatTurn], prompt: &str) -> String {
    if history.is_empty() {
        return prompt.to_string();
    }

    // Apply cap: keep only the most-recent CHAT_HISTORY_TURN_CAP turns.
    // (Each element of `history` is a single message; cap on messages, not pairs.)
    let cap = CHAT_HISTORY_TURN_CAP;
    let capped: &[ChatTurn] = if history.len() > cap {
        // Drop the oldest turns — always drop from the front (FIFO eviction).
        &history[history.len() - cap..]
    } else {
        history
    };

    let mut out = String::from("Conversation so far:\n");
    for turn in capped {
        let label = if turn.role == "user" {
            "User"
        } else {
            "Assistant"
        };
        out.push_str(label);
        out.push_str(": ");
        out.push_str(&turn.content);
        out.push('\n');
    }
    out.push_str("\nUser's new message:\n");
    out.push_str(prompt);
    out
}

/// Resolve the GLOBAL chat assistant's model id. Precedence (highest first):
///   1. `req_model` — an explicit per-request model from the chat UI selector (the override).
///   2. `app_chat_model` — the app-level (cross-project) [`crate::settings::Settings::chat_model`].
///   3. [`crate::llm::DEFAULT_MODEL`] — the floor when neither is set.
///
/// Blank / whitespace-only candidates are treated as unset (trimmed-empty is ignored), so a
/// cleared selector or a cleared setting falls through to the next tier. Pure (no I/O) so the
/// precedence is unit-testable.
pub fn resolve_chat_model(req_model: Option<&str>, app_chat_model: Option<&str>) -> String {
    let pick = |m: Option<&str>| {
        m.map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    pick(req_model)
        .or_else(|| pick(app_chat_model))
        .unwrap_or_else(|| crate::llm::DEFAULT_MODEL.to_string())
}

/// The research chat: one completion through the configured backend. The side-by-side
/// chatbot uses this; it's also the smoke test that the model wiring works.
///
/// When `history` is present in the request body, prior turns are embedded into the
/// prompt text as a transcript block before the new message (see `render_chat_prompt`).
/// The grounding system prompt (`system`) is passed through unchanged so all four
/// context layers remain intact. An empty or absent `history` behaves exactly as before
/// this fix (back-compat: the single-prompt path is unaffected).
pub(crate) async fn chat(
    State(state): State<AppState>,
    Json(req): Json<ChatReq>,
) -> Result<Json<crate::llm::LlmResponse>, AppError> {
    // The chat assistant is a GLOBAL (cross-project) assistant, so its model is an APP-LEVEL
    // setting — NOT the per-project ResearchChat step. Precedence: explicit per-request model
    // (the UI override) > app-level `chat_model` setting > DEFAULT_MODEL floor.
    let model = resolve_chat_model(Some(&req.model), state.settings.get().chat_model.as_deref());
    // Route to the right LlmPort: OpenRouter for openrouter-provider models, Anthropic for all
    // others. Returns an error when the model is OpenRouter-provider but no key is set.
    // The Anthropic side runs on the transport chosen in Settings (Chat backend), with the key
    // resolved now; `Api` with no key refuses rather than silently downgrading to the CLI.
    let chat_llm = state.chat_llm().map_err(backend_refused)?;
    if let Some(why) = state.client_lock_step_model_refusal(&model) {
        return Err(backend_refused(why));
    }
    let completer = crate::llm::build_completer(
        &model,
        &state.model_registry,
        state.credential_store.as_ref(),
        std::sync::Arc::new(chat_llm),
        state.rate_limiter.clone(),
        &state.settings.provider_policy(),
    )
    .map_err(AppError)?;
    // Embed history into the prompt when prior turns exist; otherwise use the bare prompt.
    let full_prompt = render_chat_prompt(&req.history, &req.prompt);
    let mut r = crate::llm::LlmRequest::new(full_prompt).with_model(model);
    if let Some(system) = req.system {
        r = r.with_system(system);
    }
    Ok(Json(completer.complete(r).await?))
}

/// `GET /api/usage` — the cumulative, session-wide LLM usage snapshot for the cockpit's
/// persistent usage meter: total input/output/cache tokens, derived/reported $ cost, call
/// count, a by-model breakdown, and the current rate-limited state. Provider-agnostic (keys
/// off the vendor-neutral `LlmResponse` usage fields), so it works for Claude today and for
/// a future Gemini arm with no change. Observability only — reading it changes nothing.
pub(crate) async fn usage(
    State(state): State<AppState>,
) -> Json<crate::usage_ledger::UsageSnapshot> {
    Json(state.usage_ledger.snapshot())
}

// ── local workspace (checkouts) ───────────────────────────────────────────────

/// Whether an Anthropic API key is available for the `api` backend: exactly whether
/// [`crate::credentials::anthropic_api_key`] (the one resolver every model client uses: the key
/// saved in the app first, the `ANTHROPIC_API_KEY` env var as the fallback) finds one. The gate
/// and the clients share that function, so "the UI says the key is present" and "the call has
/// the key" cannot disagree.
pub(crate) fn anthropic_api_key_present(store: &dyn crate::credentials::CredentialStore) -> bool {
    crate::credentials::anthropic_api_key(store).is_some()
}

/// Whether the model-registry picker should be served the LIVE Anthropic `/v1/models`
/// list rather than the hardcoded `CLAUDE_REGISTRY_MODELS` catalog. `true` exactly when the
/// global CHAT backend (`settings.chat_backend` — see
/// `docs/design/2026-09-22_per-project-backend.md`) is `Api` AND an Anthropic key is
/// present — the CLI backend always gets the hardcoded list regardless of this check (the
/// CLI/subscription model set is not the API `/v1/models` set), and
/// `ModelRegistry::claude_entries_for_backend` separately fails soft to hardcoded even when
/// this returns `true`, if the live cache hasn't been populated yet or the last fetch
/// failed. This is deliberately keyed off the CHAT setting, not any project's: the model
/// picker this feeds is a cross-project/app-level surface, so it mirrors the one other
/// app-level backend setting rather than an arbitrary project's.
pub(crate) fn use_live_anthropic_models(state: &AppState) -> bool {
    state.settings.chat_backend() == crate::llm::ProjectBackend::Api
        && anthropic_api_key_present(state.credential_store.as_ref())
}

/// Resolve the compliance-safety backend decision (see
/// `docs/design/2026-09-22_per-project-backend.md`) for `project`. Centralizes the
/// two-input gather — the PROJECT's own `backend` setting and `anthropic_api_key_present`
/// (keychain or env) — so every enforcement seam (the audit, the gov-dev agent driver)
/// assembles the resolution identically and can't drift. `project = None` (no active/target
/// project) is treated as `ProjectBackend::Cli` — the same zero-setup default a
/// freshly-created project gets, so a project-less call is never MORE restrictive than a
/// real one either.
pub(crate) fn resolve_backend_for_project(
    state: &AppState,
    project: Option<&crate::project::Project>,
) -> crate::llm::BackendResolution {
    let has_api_key = anthropic_api_key_present(state.credential_store.as_ref());
    let backend = project.map(|p| p.backend).unwrap_or_default();
    crate::llm::resolve_backend(backend, has_api_key)
}

/// What the app tells the operator to change when the client-engagement lock refuses a run.
pub(crate) const APP_CLIENT_LOCK_FIX: &str =
    "In this project's Settings, set the backend to API and the \
     audit and calibration models to Claude models, or turn off Client engagement if this is \
     your own development and not client work.";

/// The client-engagement lock ([`crate::client_lock::refusal`]) for a scan of `project` with
/// these (already resolved) models: the refusal text with the app's fix appended, or `None` when
/// the scan may run. A scan with NO project is refused: there is no inspection setup to say it is
/// not client work, so it is a client engagement with no opt-in (the lock is default-on), and the
/// project-less backend floors to `Cli`, which that lock refuses anyway.
pub(crate) fn client_lock_refusal_for_scan(
    project: Option<&crate::project::Project>,
    model: Option<&str>,
    calibration_model: Option<&str>,
    deep: bool,
    fix_model: Option<&str>,
) -> Option<String> {
    let Some(project) = project else {
        return Some(
            "There is no active project, and a scan without one is treated as a client \
             engagement, which runs only on the Anthropic API with Anthropic models. Create or \
             select a project first (in its Settings, turn off Client engagement if this is your \
             own development and not client work). Nothing was read and no model was called."
                .to_string(),
        );
    };
    // The clone judge runs on the project's fast band.
    let models = crate::client_lock::models_a_scan_may_use(
        model,
        calibration_model,
        deep,
        fix_model,
        Some(project.tier_map.fast_primary()),
        Some(project.review.group_review_model.as_str()),
    );
    crate::client_lock::refusal(project.client_engagement, project.backend, &models)
        .map(|why| format!("{why} {APP_CLIENT_LOCK_FIX}"))
}

/// The client-engagement lock for a run that has only the `claude -p` CLI driver (the
/// investigation, branch update and PR feedback runs): such a run always uses the Claude Code
/// subscription, whatever the project's backend, so the ACTIVE project being a client engagement
/// refuses it, and so does having no active project (no setup says it is not client work, so it
/// is a client engagement with no opt-in, like a scan). `run` names the run in the message.
/// `None` when it may start.
pub(crate) fn cli_only_run_client_lock_refusal(state: &AppState, run: &str) -> Option<String> {
    let Some(project) = state.projects.active() else {
        return Some(format!(
            "There is no active project, and {run} without one is treated as a client engagement. \
             It runs only on the Claude Code subscription (the `cli` backend), which client work \
             refuses. Nothing was run. Create or select a project first (in its Settings, turn \
             off Client engagement if this is your own development)."
        ));
    };
    crate::client_lock::refusal(
        project.client_engagement,
        crate::project::ProjectBackend::Cli,
        &[],
    )
    .map(|_| {
        format!(
            "This is a client engagement, and {run} runs only on the Claude Code subscription \
             (the `cli` backend), which client work refuses. Nothing was run. Turn off Client \
             engagement in this project's Settings if this is your own development."
        )
    })
}

/// The report a scan refused by the client-engagement lock returns: nothing ran, under the same
/// "Scan blocked" state the UI renders for the compliance gate.
pub(crate) fn client_lock_blocked_report(
    repos: &[String],
    why: String,
) -> crate::onboard::ScanReport {
    let mut r = crate::onboard::ScanReport::gated(repos);
    r.gated = false;
    r.blocked = true;
    r.ai_blocked_reason = Some(why.clone());
    r.message = Some(why);
    r
}

/// The client-tooling decision for a scan of `project` (see [`crate::client_lock`]). A
/// project-less scan is not a client engagement.
pub(crate) fn project_client_tooling(
    project: Option<&crate::project::Project>,
) -> crate::client_lock::ClientTooling {
    project.map_or(crate::client_lock::ClientTooling::INTERNAL, |p| {
        crate::client_lock::ClientTooling::new(p.client_engagement, p.client_tooling_opt_in)
    })
}

/// The engagement record a scan of `project` writes into its ledger, with the models the scan
/// recorded as having run.
pub(crate) fn engagement_record_for_scan(
    project: Option<&crate::project::Project>,
    report: &crate::onboard::ScanReport,
) -> crate::client_lock::EngagementRecord {
    let model = ModelRecord::anthropic(project.map(|p| p.backend).unwrap_or_default())
        .with_client_engagement(project.is_some_and(|p| p.client_engagement))
        .with_models_from(&report.provenance);
    crate::client_lock::EngagementRecord::new(&model, project_client_tooling(project))
}

/// Every rule's PROJECT-level chosen alternative option, keyed by uppercased rule id —
/// gathered across `project.ruleset`'s three rule lists (repo-local `selections`, the
/// `cross_repo` set, and `process`). Feeds the audit-integrated alternative-recommendation
/// feature (`onboard::build_rule_alternatives`, `resolve_fix`'s chosen-option fallback) so
/// the "currently selected" marker and the Fix-text join both read the SAME source of truth
/// as everything else that resolves a rule's directive (`Rule::resolved_option`). A rule with
/// no persisted `chosen_option` is simply absent from the map — callers fall back to the
/// corpus rule's own `default_option` from there.
pub(crate) fn project_chosen_options(
    project: &crate::project::Project,
) -> std::collections::HashMap<String, String> {
    project
        .ruleset
        .selections
        .iter()
        .chain(&project.ruleset.cross_repo)
        .chain(&project.ruleset.process)
        .filter_map(|s| {
            s.chosen_option
                .as_ref()
                .map(|opt| (s.rule_id.trim().to_ascii_uppercase(), opt.clone()))
        })
        .collect()
}
