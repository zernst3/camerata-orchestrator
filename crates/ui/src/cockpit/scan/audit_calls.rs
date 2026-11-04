//! Audit start and rescan calls, the model list, the job polling and the deterministic progress block.

use super::*;

/// A rule the user selected to audit against, carrying its per-repo binding. An empty
/// `repos` means PROJECT-LEVEL (every repo); a non-empty `repos` scopes the rule to just
/// those repos. The backend audits each repo against only the rules that apply to it, so a
/// multi-repo scan runs each repo against its own rules ∪ the project-level set.
#[derive(Clone, PartialEq)]
pub(in crate::cockpit) struct SelectedAuditRule {
    pub id: String,
    pub directive: String,
    pub repos: Vec<String>,
}

/// Serialize selected rules into the audit request shape (`{id, directive, repos}` each).
pub(in crate::cockpit) fn audit_rules_json(rules: &[SelectedAuditRule]) -> Vec<serde_json::Value> {
    rules
        .iter()
        .map(|r| serde_json::json!({ "id": r.id, "directive": r.directive, "repos": r.repos }))
        .collect()
}

/// The body of an audit request, shared by the synchronous and the background-job routes. The
/// retired `thorough` option is not part of it: calibration runs one pass per site whatever a
/// client sends, so nothing in the request can change that.
#[allow(clippy::too_many_arguments)]
pub(in crate::cockpit) fn audit_request_json(
    repos: &[String],
    rules: &[SelectedAuditRule],
    model: &str,
    calibration_model: &str,
    mode: &str,
    incremental: bool,
    deep: bool,
    run_ai_review: bool,
    run_deterministic: bool,
    max_cost_usd: Option<f64>,
) -> serde_json::Value {
    serde_json::json!({
        "repos": repos,
        "rules": audit_rules_json(rules),
        "model": model,
        "calibration_model": calibration_model,
        "mode": mode,
        "incremental": incremental,
        "deep": deep,
        "run_ai_review": run_ai_review,
        "run_deterministic": run_deterministic,
        "max_cost_usd": max_cost_usd,
    })
}

/// Phase 2 — audit the repos against the selected rules (each carrying its repo binding).
/// When `deep` is true, the server also runs the three deep-tier lenses (SOC-2 gap,
/// deep security, threat model) and attaches the results to `report.deep`.
#[allow(clippy::too_many_arguments)]
pub(in crate::cockpit) async fn audit_against(
    repos: &[String],
    rules: &[SelectedAuditRule],
    model: &str,
    calibration_model: &str,
    mode: &str,
    incremental: bool,
    deep: bool,
    run_ai_review: bool,
    run_deterministic: bool,
    max_cost_usd: Option<f64>,
) -> Option<ScanReportView> {
    // Loading guard: the Bombe machine runs for the full audit round-trip.
    let _guard = crate::loading::LoadingGuard::new();
    let body = audit_request_json(
        repos,
        rules,
        model,
        calibration_model,
        mode,
        incremental,
        deep,
        run_ai_review,
        run_deterministic,
        max_cost_usd,
    );
    reqwest::Client::new()
        .post(format!("{}/api/onboard/audit", crate::bff_base()))
        .json(&body)
        .send()
        .await
        .ok()?
        .json::<ScanReportView>()
        .await
        .ok()
}

// ── Audit-integrated alternative recommendation: batched rescan + accept ───────────────────
// docs/design/2026-09-22_audit-integrated-alternatives.md — Phase 2 (UI). The server side
// (Phase 1) is done; this is the wire contract the two endpoints already speak.

/// One staged override: force `rule_id` to `chosen_option_id` on the next batched rescan.
/// Mirrors the server's `AlternativeOverride` request-body entry
/// (`POST /api/projects/:id/rescan-alternatives`).
#[derive(Clone, PartialEq, serde::Serialize)]
pub(in crate::cockpit) struct AlternativeOverride {
    pub rule_id: String,
    pub chosen_option_id: String,
}

/// Response body of `POST /api/projects/:id/rescan-alternatives`: `{ ok, message, findings,
/// recommendations }`. `findings` + `recommendations` cover ONLY the rescanned rules — the
/// caller merges them in place over the rest of the held audit report via
/// [`merge_rescanned_alternatives`]. `ok: false` (e.g. the backend `Blocked` compliance gate,
/// or every submitted override being invalid) carries the reason in `message`; the caller
/// surfaces it rather than silently failing.
#[derive(Clone, PartialEq, Default, serde::Deserialize, serde::Serialize)]
pub(in crate::cockpit) struct RescanAlternativesResp {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub findings: Vec<FindingView>,
    #[serde(default)]
    pub recommendations: std::collections::HashMap<String, RuleRecommendationView>,
}

/// POST the batched rescan for exactly the given overridden rules. This is an AI call (it
/// re-runs the audit for the given rules, forced to the operator's chosen options) — the guard
/// here drives the background Bombe machine for the whole round trip, same as `audit_against`
/// and `scan_repos` above. Returns `None` only on a network/transport/parse failure; a
/// server-reported `{ ok: false, message }` still deserializes into `Some(..)` so the caller
/// can read `message` (see this type's doc comment).
pub(in crate::cockpit) async fn rescan_alternatives(
    project_id: &str,
    overrides: &[AlternativeOverride],
) -> Option<RescanAlternativesResp> {
    let _guard = crate::loading::LoadingGuard::new();
    reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/rescan-alternatives",
            crate::bff_base(),
            project_id
        ))
        .json(&serde_json::json!({ "overrides": overrides }))
        .send()
        .await
        .ok()?
        .json::<RescanAlternativesResp>()
        .await
        .ok()
}

/// Response body of `POST /api/projects/:id/accept-alternatives`: `{ ok, message?, accepted,
/// skipped }` (`accepted` is a COUNT, not a list — mirrors the server's
/// `accept_alternatives_handler`). `ruleset` is intentionally not modeled here: the caller
/// doesn't need to re-parse the project's full ruleset shape to reflect an accept — it already
/// knows which rules it asked to accept.
#[derive(Clone, PartialEq, Default, serde::Deserialize, serde::Serialize)]
pub(in crate::cockpit) struct AcceptAlternativesResp {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub accepted: usize,
    #[serde(default)]
    pub skipped: Vec<String>,
}

/// POST `accept-alternatives`. `rule_ids: None` accepts EVERY rule in the project's last-scan
/// recommendations (the server's `{}` "accept all" shape); `Some(ids)` scopes to that subset.
/// Not an AI call (it only persists the current selection into `chosen_option`), so — unlike
/// `rescan_alternatives` — this does NOT take a `LoadingGuard`; no model runs.
pub(in crate::cockpit) async fn accept_alternatives(
    project_id: &str,
    rule_ids: Option<&[String]>,
) -> Option<AcceptAlternativesResp> {
    let body = match rule_ids {
        Some(ids) => serde_json::json!({ "rule_ids": ids }),
        None => serde_json::json!({}),
    };
    reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/accept-alternatives",
            crate::bff_base(),
            project_id
        ))
        .json(&body)
        .send()
        .await
        .ok()?
        .json::<AcceptAlternativesResp>()
        .await
        .ok()
}

/// Merge a batched rescan's updated findings + recommendations into the audit report's
/// existing findings + recommendations, replacing ONLY the rescanned rules' rows — every other
/// rule's findings and recommendations are left untouched. Pure (no signals, no Dioxus), so the
/// merge is unit-tested directly; the call site just assigns the two returned values back onto
/// the held `ScanReportView`.
pub(in crate::cockpit) fn merge_rescanned_alternatives(
    existing_findings: &[FindingView],
    existing_recommendations: &std::collections::HashMap<String, RuleRecommendationView>,
    rescanned_rule_ids: &std::collections::HashSet<String>,
    new_findings: Vec<FindingView>,
    new_recommendations: std::collections::HashMap<String, RuleRecommendationView>,
) -> (
    Vec<FindingView>,
    std::collections::HashMap<String, RuleRecommendationView>,
) {
    let mut findings: Vec<FindingView> = existing_findings
        .iter()
        .filter(|f| !rescanned_rule_ids.contains(&f.rule_id))
        .cloned()
        .collect();
    findings.extend(new_findings);
    let mut recommendations = existing_recommendations.clone();
    for (rule_id, rec) in new_recommendations {
        recommendations.insert(rule_id, rec);
    }
    (findings, recommendations)
}

/// One model entry in a selector, sourced from `GET /api/models/registry`.
///
/// `label` carries a badge-enriched display string (e.g. "DeepSeek R1  $0.55/M · tool-use · 64K · cache").
/// `provider` groups the entry in `<optgroup>` elements ("claude" | "openrouter").
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub(in crate::cockpit) struct AuditModelOption {
    /// Badge-enriched display label. Built by [`model_badge_label`] at fetch time.
    pub label: String,
    /// The model id passed to the API / CLI (stable key; stored in project settings).
    pub id: String,
    /// Provider key: "claude" or "openrouter". Used for `<optgroup>` grouping.
    #[serde(default)]
    pub provider: String,
    /// Whether the model is free to call (prompt + completion price = 0).
    #[serde(default)]
    pub free: bool,
    /// Whether the model supports tool use.
    #[serde(default)]
    pub tool_use: bool,
    /// Context window in tokens.
    #[serde(default)]
    pub context: u64,
    /// USD per million tokens (input / output). Drives the pre-audit cost estimate.
    #[serde(default)]
    pub price_in: f64,
    #[serde(default)]
    pub price_out: f64,
    /// Whether this model supports prompt caching.
    #[serde(default)]
    pub caching: bool,
    /// Whether this model supports vision / multimodal input (images).
    /// Used to filter the Designer (vision) band model selector.
    #[serde(default)]
    pub vision: bool,
    /// Whether `price_in`/`price_out` are a real, known price. `false` for a live-fetched
    /// Anthropic model (Feature A) whose id isn't in the hardcoded price table yet —
    /// `model_badge_label` shows "price unknown" instead of a `$0`/`M` badge in that case,
    /// so an un-priced model never LOOKS free. Absent in older/mocked JSON defaults to
    /// `true` (every price this endpoint ever served before Feature A was known).
    #[serde(default = "default_price_known")]
    pub price_known: bool,
}

pub(super) fn default_price_known() -> bool {
    true
}

/// Build a badge-enriched display label from registry entry fields.
///
/// Format: "<display>  FREE · tool-use · 200K · cache"
/// Or for paid: "<display>  $0.55/M · tool-use · 64K · cache"
///
/// - Price: `FREE` if free, `price unknown` if `!price_known` (a live-fetched model absent
///   from the hardcoded price table — see [`AuditModelOption::price_known`]), else
///   `$<price_out>/M` (output price, compact).
/// - `tool-use`: always shown (absence as `no-tools` for OpenRouter models lacking it).
/// - Context: `<N>K` (e.g. `200K`, `64K`).
/// - `cache`: shown only when `caching` is true.
///
/// Badges are separated by ` · ` and appended after two spaces following the display name.
pub(super) fn model_badge_label(
    display: &str,
    free: bool,
    tool_use: bool,
    context: u64,
    price_out: f64,
    caching: bool,
    price_known: bool,
) -> String {
    let mut parts = Vec::<String>::new();

    // Price badge: FREE, "price unknown" (never a misleading $0), or $<price>/M.
    if free {
        parts.push("FREE".to_string());
    } else if !price_known {
        parts.push("price unknown".to_string());
    } else if price_out > 0.0 {
        // Compact price: show 2 sig figs but strip trailing zeros.
        // e.g. 15.0 → "$15/M", 0.55 → "$0.55/M", 3.0 → "$3/M"
        let formatted = if price_out >= 10.0 {
            format!("${:.0}/M", price_out)
        } else if price_out >= 1.0 {
            // Up to 1 decimal place, strip trailing zero.
            let s = format!("{:.1}", price_out);
            format!("${}/M", s.trim_end_matches('0').trim_end_matches('.'))
        } else {
            // Small prices: 2 decimal places, strip trailing zeros.
            let s = format!("{:.2}", price_out);
            format!("${}/M", s.trim_end_matches('0').trim_end_matches('.'))
        };
        parts.push(formatted);
    }

    // Tool-use: always shown (absence flagged for OpenRouter models).
    if tool_use {
        parts.push("tool-use".to_string());
    } else {
        parts.push("no-tools".to_string());
    }

    // Context window.
    if context > 0 {
        let ctx_k = context / 1000;
        parts.push(format!("{ctx_k}K"));
    }

    // Caching tag.
    if caching {
        parts.push("cache".to_string());
    }

    if parts.is_empty() {
        display.to_string()
    } else {
        format!("{}  {}", display, parts.join(" · "))
    }
}

/// Shape of the raw registry wire response from `GET /api/models/registry`.
#[derive(serde::Deserialize)]
pub(super) struct RegistryResp {
    models: Vec<RegistryEntryWire>,
    #[serde(default)]
    openrouter_fetched: bool,
}

/// One entry from the registry wire response. Mirrors `camerata_api_types::model_registry::RegistryEntry` (subset, fields tolerated absent).
#[derive(serde::Deserialize)]
pub(super) struct RegistryEntryWire {
    id: String,
    display: String,
    provider: String,
    #[serde(default)]
    free: bool,
    #[serde(default)]
    tool_use: bool,
    #[serde(default)]
    context: u64,
    #[serde(default)]
    price_in: f64,
    #[serde(default)]
    price_out: f64,
    #[serde(default)]
    caching: bool,
    /// Whether the model supports vision / multimodal input (images).
    #[serde(default)]
    vision: bool,
    /// Whether `price_in`/`price_out` are a real, known price. See
    /// [`AuditModelOption::price_known`].
    #[serde(default = "default_price_known")]
    price_known: bool,
}

impl RegistryEntryWire {
    fn to_option(&self) -> AuditModelOption {
        AuditModelOption {
            label: model_badge_label(
                &self.display,
                self.free,
                self.tool_use,
                self.context,
                self.price_out,
                self.caching,
                self.price_known,
            ),
            id: self.id.clone(),
            provider: self.provider.clone(),
            free: self.free,
            tool_use: self.tool_use,
            context: self.context,
            price_in: self.price_in,
            price_out: self.price_out,
            caching: self.caching,
            vision: self.vision,
            price_known: self.price_known,
        }
    }
}

/// One repo's size for the pre-audit estimate, from `POST /api/onboard/preflight`: what the scan
/// will read from it, or why it cannot be read.
#[derive(Clone, PartialEq, serde::Deserialize)]
pub(in crate::cockpit) struct RepoSizeView {
    pub repo: String,
    #[serde(default)]
    pub files: usize,
    #[serde(default)]
    pub code_chars: usize,
    #[serde(default)]
    pub error: Option<String>,
}

/// Ask the server how big each repo is as the scan will read it. The scan chunks and calibrates
/// every repo on its own, so a project of several repos is estimated repo by repo, not as one
/// stream of code. `None` when the server cannot be reached (the caller falls back to the one
/// total it already has).
pub(in crate::cockpit) async fn fetch_repo_sizes(repos: &[String]) -> Option<Vec<RepoSizeView>> {
    reqwest::Client::new()
        .post(format!("{}/api/onboard/preflight", crate::bff_base()))
        .json(&serde_json::json!({ "repos": repos }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub(in crate::cockpit) struct AuditModelsResp {
    pub models: Vec<AuditModelOption>,
    /// The default model id to pre-select. For the registry this is the first Claude model.
    #[serde(default)]
    pub default: String,
    /// Whether the OpenRouter portion of the registry has been fetched.
    #[serde(default)]
    pub openrouter_fetched: bool,
}

impl AuditModelsResp {
    /// Return models grouped by provider for `<optgroup>` rendering.
    /// Each group is `(group_label, entries)`. Claude always comes first.
    pub fn grouped(&self) -> Vec<(&'static str, Vec<&AuditModelOption>)> {
        let claude: Vec<&AuditModelOption> = self
            .models
            .iter()
            .filter(|m| m.provider == "claude")
            .collect();
        let openrouter: Vec<&AuditModelOption> = self
            .models
            .iter()
            .filter(|m| m.provider == "openrouter")
            .collect();
        let mut groups = Vec::new();
        if !claude.is_empty() {
            groups.push(("Claude (subscription)", claude));
        }
        if !openrouter.is_empty() {
            groups.push(("OpenRouter", openrouter));
        }
        groups
    }

    /// Return only vision-capable models, grouped by provider.
    // dead_code: the Designer (vision) band selector now lists ALL models (06003633), so the
    // vision-only grouping is unwired; kept (with its tests) because
    // The UI-core extraction notes list it among the pure logic to extract.
    #[allow(dead_code)]
    pub fn vision_grouped(&self) -> Vec<(&'static str, Vec<&AuditModelOption>)> {
        let claude: Vec<&AuditModelOption> = self
            .models
            .iter()
            .filter(|m| m.provider == "claude" && m.vision)
            .collect();
        let openrouter: Vec<&AuditModelOption> = self
            .models
            .iter()
            .filter(|m| m.provider == "openrouter" && m.vision)
            .collect();
        let mut groups = Vec::new();
        if !claude.is_empty() {
            groups.push(("Claude (subscription)", claude));
        }
        if !openrouter.is_empty() {
            groups.push(("OpenRouter", openrouter));
        }
        groups
    }
}

/// Fetch the model registry (`GET /api/models/registry`) and map entries to selector options.
///
/// Falls back gracefully: if OpenRouter key is not set, only Claude entries are returned.
/// Returns `None` only when the server is unreachable.
pub(in crate::cockpit) async fn fetch_audit_models() -> Option<AuditModelsResp> {
    let resp = reqwest::get(format!("{}/api/models/registry", crate::bff_base()))
        .await
        .ok()?
        .json::<RegistryResp>()
        .await
        .ok()?;

    let models: Vec<AuditModelOption> = resp.models.iter().map(|e| e.to_option()).collect();
    // Default = first Claude model (Opus), or first model overall if no Claude entries.
    let default = models
        .iter()
        .find(|m| m.provider == "claude")
        .or_else(|| models.first())
        .map(|m| m.id.clone())
        .unwrap_or_default();

    Some(AuditModelsResp {
        models,
        default,
        openrouter_fetched: resp.openrouter_fetched,
    })
}

/// One deterministic-scan tool's live progress (mirror of the server's `DetToolProgress`).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
pub(in crate::cockpit) struct DetToolProgressView {
    #[serde(default)]
    pub tool: String,
    /// `starting` | `running` | `done`.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub findings: usize,
}

/// The deterministic pass's progress (mirror of the server's `DetProgress`): per-tool rows
/// plus an overall done/total. Drives the "Deterministic scan" progress component.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
pub(in crate::cockpit) struct DetProgressView {
    #[serde(default)]
    pub tools: Vec<DetToolProgressView>,
    #[serde(default)]
    pub done: usize,
    #[serde(default)]
    pub total: usize,
}

/// A polled async-audit job (`GET /api/onboard/audit/job/:id`).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
pub(in crate::cockpit) struct JobStateView {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub done: usize,
    #[serde(default)]
    pub total: usize,
    /// Passes refused (spend limit) that did not run; not part of `done`.
    #[serde(default)]
    pub not_run: usize,
    #[serde(default)]
    pub findings: Vec<FindingView>,
    /// Live deterministic-pass progress (floor + preview tools). Empty until a tool registers.
    #[serde(default)]
    pub deterministic: DetProgressView,
    #[serde(default)]
    pub report: Option<ScanReportView>,
    #[serde(default)]
    pub message: Option<String>,
    /// Batch mode (#61): the Anthropic Message Batch id currently being processed
    /// (`msgbatch_01...`). Surfaced in the job-progress status line so the user can
    /// look it up in the Anthropic console. `None` for parallel/sequential mode jobs.
    #[serde(default)]
    pub batch_id: Option<String>,
}

/// Full envelope for `GET /api/onboard/audit/job/:id` — the server wraps
/// `JobState` under a `job:` key alongside stall/cancel metadata.
#[derive(Clone, serde::Deserialize, Default)]
pub(in crate::cockpit) struct JobStatusEnvelope {
    pub job: JobStateView,
    /// Milliseconds since last job progress update. `None` if no activity recorded yet.
    #[serde(default)]
    pub idle_ms: Option<u128>,
    /// True if a cancel has been requested for this job.
    // dead_code: part of the documented `GET .../job/:id` wire contract, deserialized "for
    // future use" (docs/decisions/2026-06-23_stall_ui_warn_stop_thresholds.md); the UI does
    // not yet surface a "cancelling" state. Only tests read it today.
    #[allow(dead_code)]
    #[serde(default)]
    pub cancel_requested: bool,
}

/// Mode 3: START an async audit job, returning its id (the request returns immediately).
/// `deep` forwards the opt-in deep compliance & security tier (#55); the server
/// runs the three lenses after the standard audit completes and attaches the result
/// to the final job report's `deep` field.
#[allow(clippy::too_many_arguments)]
pub(in crate::cockpit) async fn audit_job_start(
    repos: &[String],
    rules: &[SelectedAuditRule],
    model: &str,
    calibration_model: &str,
    exec_mode: &str,
    incremental: bool,
    deep: bool,
    run_ai_review: bool,
    run_deterministic: bool,
    max_cost_usd: Option<f64>,
) -> Option<String> {
    let body = audit_request_json(
        repos,
        rules,
        model,
        calibration_model,
        exec_mode,
        incremental,
        deep,
        run_ai_review,
        run_deterministic,
        max_cost_usd,
    );
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/onboard/audit/start", crate::bff_base()))
        .json(&body)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    v.get("job_id").and_then(|j| j.as_str()).map(String::from)
}

/// Recommend a scan mode by the scanned codebase's SCALE (the design's auto-select):
/// multi-repo or a large codebase → Background job (decoupled, walk-away); otherwise
/// `auto`, the server's default (a Message Batch on the API backend, real time on the CLI
/// backend; `camerata_server::ai_audit::resolve_scan_mode`). Parallel and Sequential are never
/// auto-recommended: picking one forces real time. The user can always change it.
pub(in crate::cockpit) fn recommend_scan_mode(report: &ScanReportView) -> String {
    if report.repos.len() > 1 || report.files_scanned > 150 {
        "job".to_string()
    } else {
        "auto".to_string()
    }
}

/// Poll an async audit job for progress + incremental findings + the final report.
/// What "Run what remains" would do: shown before it starts (mirror of the server's preview).
#[derive(Clone, PartialEq, serde::Deserialize, Default, Debug)]
pub(in crate::cockpit) struct ResumePlanView {
    /// The parts that remain, the stored and unsent calls and the estimate, ready to show.
    #[serde(default)]
    pub text: String,
    /// Why the resume cannot start now, in plain words; `None` when it can.
    #[serde(default)]
    pub refusal: Option<String>,
}

/// Fetch the resume plan of a saved run.
pub(in crate::cockpit) async fn resume_plan(run_id: &str) -> Result<ResumePlanView, String> {
    let resp = reqwest::get(format!(
        "{}/api/inspections/{run_id}/plan",
        crate::bff_base()
    ))
    .await
    .map_err(|e| format!("could not reach the app's server: {e}"))?;
    if !resp.status().is_success() {
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        return Err(body["error"]
            .as_str()
            .unwrap_or("that run is not saved any more")
            .to_string());
    }
    resp.json::<ResumePlanView>()
        .await
        .map_err(|e| format!("the plan could not be read: {e}"))
}

/// Start the resume of a saved run as a background job; returns the job id, or the plain reason.
pub(in crate::cockpit) async fn resume_start(run_id: &str) -> Result<String, String> {
    let resp = reqwest::Client::new()
        .post(format!(
            "{}/api/inspections/{run_id}/resume",
            crate::bff_base()
        ))
        .send()
        .await
        .map_err(|e| format!("could not reach the app's server: {e}"))?;
    let ok = resp.status().is_success();
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    if ok {
        body["job_id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "the server did not return a job".to_string())
    } else {
        Err(body["error"]
            .as_str()
            .unwrap_or("the run could not be resumed")
            .to_string())
    }
}

/// The whole of "Run what remains" for one run, with the Bombe held from the first request to the
/// end of the polling: the guard is the first thing built, so it covers the start request, the
/// job's whole run (`poll`) and every early return. `start` and `poll` are the requests, passed in
/// so the flow is testable without a server.
pub(in crate::cockpit) async fn run_what_remains<S, SF, P, PF>(
    loading_count: crate::loading::LoadingCount,
    start: S,
    poll: P,
) -> Result<(), String>
where
    S: FnOnce() -> SF,
    SF: std::future::Future<Output = Result<String, String>>,
    P: FnOnce(String) -> PF,
    PF: std::future::Future<Output = ()>,
{
    let _guard = crate::loading::LoadingGuard::from_count(loading_count);
    let job_id = start().await?;
    poll(job_id).await;
    Ok(())
}

pub(in crate::cockpit) async fn audit_job_poll(job_id: &str) -> Option<JobStatusEnvelope> {
    reqwest::get(format!(
        "{}/api/onboard/audit/job/{}",
        crate::bff_base(),
        job_id
    ))
    .await
    .ok()?
    .json::<Option<JobStatusEnvelope>>()
    .await
    .ok()
    .flatten()
}

/// Drive an async audit job to completion: poll every ~1.5s, update progress + (on done) the
/// final report, clearing the shared `active_audit_job` so a later mount doesn't re-resume.
/// Shared by the manual start AND the resume-on-mount path. Gives up after a few misses (the
/// job vanished, e.g. the server restarted) so it can't spin forever.
///
/// SILENT-DEATH FIX: `failed` used to clear the spinner and stop, with nothing else — the
/// job's `message` (the server's `jobs.fail` reason) was read off the wire but never shown
/// anywhere, so a failed background scan looked EXACTLY like nothing had happened. Same gap
/// on the "job vanished" (misses >= 3) path: no report, no message, no signal it even ran.
/// Both paths now set `job_failed_message`, which `ScanJobFailedBanner` renders
/// unconditionally (not gated on a report existing, since a failed/vanished job never
/// produces one).
///
/// BOMBE-SILENT-MISS FIX: this used to take its guard via `LoadingGuard::new()`, resolving the
/// `LoadingCount` context from WITHIN this function's own body — which runs inside a detached
/// `spawn`ed task (both call sites `.await` this from inside `spawn(async move { .. })` / a
/// `use_future`). Context resolution there is not reliable (see the pitfall note on
/// `LoadingGuard::new`), so the guard could silently no-op and the Bombe would never animate for
/// a running scan. `loading_count` is now REQUIRED and must be captured by the caller in its own
/// render-scope body (`use_context::<LoadingCount>()`) before the `spawn`/`use_future`, then
/// passed straight through here.
#[allow(clippy::too_many_arguments)]
pub(in crate::cockpit) async fn poll_job(
    jid: String,
    mut audit: Signal<Option<ScanReportView>>,
    mut auditing: Signal<bool>,
    mut job_progress: Signal<Option<(usize, usize, usize, usize)>>,
    // Live DETERMINISTIC-pass progress (floor + preview tools), rendered by the
    // "Deterministic scan" component above the AI agent-activity drawer. `None` clears it.
    mut det_progress: Signal<Option<DetProgressView>>,
    mut active_audit_job: Signal<Option<String>>,
    mut scan_idle_ms: Signal<Option<u128>>,
    // Set on `failed` (with the server's reason) or when the job vanishes mid-poll; cleared
    // by the caller when a fresh scan starts. `None` renders nothing (`ScanJobFailedBanner`).
    mut job_failed_message: Signal<Option<String>>,
    // Captured by the CALLER in its component render scope (never resolved from context in
    // here) — see the BOMBE-SILENT-MISS note above.
    loading_count: crate::loading::LoadingCount,
) {
    // Loading guard held for the ENTIRE poll loop so the Bombe machine stays
    // active until the background job reports done/failed/cancelled.
    let _guard = crate::loading::LoadingGuard::from_count(loading_count);
    let mut misses = 0u32;
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        match audit_job_poll(&jid).await {
            Some(js) => {
                misses = 0;
                scan_idle_ms.set(js.idle_ms);
                job_progress.set(Some((
                    js.job.done,
                    js.job.total,
                    js.job.findings.len(),
                    js.job.not_run,
                )));
                // Surface the deterministic progress whenever any tool has registered, so the
                // component appears the moment the floor starts (not only once findings land).
                if js.job.deterministic.total > 0 {
                    det_progress.set(Some(js.job.deterministic.clone()));
                }
                match js.job.status.as_str() {
                    "done" => {
                        audit.set(js.job.report);
                        auditing.set(false);
                        job_progress.set(None);
                        det_progress.set(None);
                        active_audit_job.set(None);
                        break;
                    }
                    "failed" => {
                        job_failed_message.set(Some(js.job.message.clone().unwrap_or_else(|| {
                            "The scan failed for an unspecified reason.".to_string()
                        })));
                        auditing.set(false);
                        job_progress.set(None);
                        det_progress.set(None);
                        active_audit_job.set(None);
                        break;
                    }
                    "cancelled" => {
                        auditing.set(false);
                        job_progress.set(None);
                        det_progress.set(None);
                        active_audit_job.set(None);
                        break;
                    }
                    _ => {}
                }
            }
            None => {
                misses += 1;
                if misses >= 3 {
                    job_failed_message.set(Some(
                        "Lost contact with the running scan (the server may have restarted \
                         or the job was lost). Please retry."
                            .to_string(),
                    ));
                    auditing.set(false);
                    job_progress.set(None);
                    det_progress.set(None);
                    active_audit_job.set(None);
                    break;
                }
            }
        }
    }
}

/// The "Deterministic scan" progress component — rendered ABOVE the AI agent-activity drawer.
/// Shows the deterministic pass's per-tool state (start/run/done + findings count) and an
/// overall done/total bar. It's the PRIMARY progress view in deterministic-only mode, where
/// the AI drawer is empty. Styled to match the existing job-progress UI.
#[component]
pub(in crate::cockpit) fn DeterministicProgress(progress: DetProgressView) -> Element {
    let pct = (progress.done * 100)
        .checked_div(progress.total)
        .unwrap_or(0)
        .min(100);
    rsx! {
        div { class: "det-progress",
            div { class: "det-progress-head",
                span { class: "det-progress-title", "Deterministic scan" }
                span { class: "det-progress-count", "{progress.done}/{progress.total} tools" }
            }
            div { class: "det-progress-track",
                div { class: "det-progress-fill", style: "width: {pct}%" }
            }
            div { class: "det-progress-tools",
                for t in progress.tools.iter() {
                    {
                        let label = det_tool_label(&t.tool);
                        let status_class = match t.status.as_str() {
                            "done" => "det-tool det-tool-done",
                            "running" => "det-tool det-tool-running",
                            _ => "det-tool det-tool-starting",
                        };
                        let glyph = match t.status.as_str() {
                            "done" => "\u{2713}",   // ✓
                            "running" => "\u{2026}", // …
                            _ => "\u{00b7}",         // ·
                        };
                        rsx! {
                            div { key: "{t.tool}", class: "{status_class}",
                                span { class: "det-tool-glyph", "{glyph}" }
                                span { class: "det-tool-name", "{label}" }
                                if t.status == "done" {
                                    span { class: "det-tool-findings", "{t.findings} finding(s)" }
                                } else {
                                    span { class: "det-tool-state", "{t.status}" }
                                }
                            }
                        }
                    }
                }
            }
            span { class: "det-progress-note",
                "Deterministic scans run locally — no LLM, no tokens. The security floor is always-on; preview tools (clippy/ruff/eslint/semgrep) run for your selected mechanical rules."
            }
        }
    }
}
