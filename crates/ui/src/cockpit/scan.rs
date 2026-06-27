use super::*;


/// One repo's local-path resolution status (issue #33), from `/api/projects/:id/repo-health`.
#[derive(Clone, PartialEq, serde::Deserialize)]
pub(super) struct RepoResolutionView {
    pub repo: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub resolved: bool,
    #[serde(default)]
    pub reason: String,
}

pub(super) async fn fetch_repo_health(project_id: &str) -> Option<Vec<RepoResolutionView>> {
    let v: serde_json::Value = reqwest::get(format!(
        "{}/api/projects/{}/repo-health",
        crate::BFF_URL,
        project_id
    ))
    .await
    .ok()?
    .json()
    .await
    .ok()?;
    serde_json::from_value(v.get("repos")?.clone()).ok()
}

pub(super) async fn set_repo_path(repo: &str, path: &str) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/repo-path", crate::BFF_URL))
        .json(&serde_json::json!({ "repo": repo, "path": path }))
        .send()
        .await
        .is_ok()
}

/// The broken-path health check (issue #33): for each of a project's repos, shows whether it
/// resolves to a local git checkout, with a per-repo "Resolve…" folder picker for the broken
/// ones. Refreshes on mount and after a resolve. Shown wherever a project's repos matter
/// (the Rules view today); the same data backs an import's "resolve paths" prompt.
#[component]
pub(super) fn RepoHealthPanel(project_id: String) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut refresh = use_signal(|| 0u32);
    let pid = project_id.clone();
    let health = use_resource(move || {
        let pid = pid.clone();
        let _ = refresh();
        async move { fetch_repo_health(&pid).await }
    });
    let repos = health.read().clone().flatten().unwrap_or_default();
    if repos.is_empty() {
        return rsx! {};
    }
    let broken = repos.iter().filter(|r| !r.resolved).count();
    rsx! {
        div { class: "repo-health",
            if broken == 0 {
                p { class: "repo-health-ok", "✓ All {repos.len()} repo path(s) resolve to a local checkout." }
            } else {
                div { class: "repo-health-warn",
                    span { class: "repo-health-warn-h", "⚠ {broken} repo path(s) need resolving" }
                    p { class: "section-hint", "These repos don't point at a local git checkout on this machine (common right after importing a project). Resolve each before working on it." }
                }
            }
            for r in repos.iter() {
                {
                    let repo = r.repo.clone();
                    let resolved = r.resolved;
                    let reason = r.reason.clone();
                    let path = r.path.clone().unwrap_or_default();
                    rsx! {
                        div { class: "repo-health-row", key: "{r.repo}",
                            span { class: if resolved { "repo-health-icon ok" } else { "repo-health-icon bad" },
                                if resolved { "✓" } else { "⚠" }
                            }
                            span { class: "repo-health-repo", "{r.repo}" }
                            if resolved {
                                span { class: "repo-health-path", "{path}" }
                            } else {
                                span { class: "repo-health-reason", "{reason}" }
                                button {
                                    class: "btn-edit-sm",
                                    onclick: move |_| {
                                        let repo = repo.clone();
                                        spawn(async move {
                                            if let Some(folder) = rfd::AsyncFileDialog::new()
                                                .set_title("Choose this repo's local folder")
                                                .pick_folder()
                                                .await
                                            {
                                                let p = folder.path().to_string_lossy().to_string();
                                                if set_repo_path(&repo, &p).await {
                                                    refresh += 1;
                                                } else {
                                                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Couldn't save the repo path.");
                                                }
                                            }
                                        });
                                    },
                                    "Resolve…"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Quote a CSV field if it contains a comma, quote, or newline (RFC 4180).
pub(super) fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Pop a native save dialog and write `content`. Returns true on success.
pub(super) async fn save_csv(default_name: &str, content: String) -> bool {
    match rfd::AsyncFileDialog::new()
        .set_file_name(default_name)
        .save_file()
        .await
    {
        Some(file) => file.write(content.as_bytes()).await.is_ok(),
        None => false,
    }
}

/// Build CSV for the audit findings table.
pub(super) fn findings_csv(findings: &[FindingView]) -> String {
    // Flat + lossless: one row per finding, every column. NOT grouped/merged — a machine
    // consumer (script, pivot, SIEM, compliance pipeline) groups/filters itself and needs
    // full fidelity. `also_matches` carries the other rule ids the location-merge folded in,
    // so no rule is dropped from the export (space-separated; the grouping the UI shows is
    // recoverable from rule_id + also_matches + path + line).
    // `preview`/`preview_tool` carry the scan-time deterministic-preview provenance (Part B):
    // a preview finding is deterministic but NOT enforced until the CI story wires it.
    let mut out = String::from(
        "repo,severity,status,rule_id,also_matches,path,line,snippet,detail,preview,preview_tool\n",
    );
    for f in findings {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{}\n",
            csv_field(&f.repo),
            csv_field(&f.severity),
            csv_field(&f.status),
            csv_field(&f.rule_id),
            csv_field(&f.also_matches.join(" ")),
            csv_field(&f.path),
            f.line,
            csv_field(&f.snippet),
            csv_field(&f.detail),
            f.preview,
            csv_field(f.preview_tool.as_deref().unwrap_or("")),
        ));
    }
    out
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub(super) struct FindingView {
    #[serde(default)]
    pub repo: String,
    pub path: String,
    pub line: usize,
    pub rule_id: String,
    pub severity: String,
    pub snippet: String,
    pub detail: String,
    /// `active` (enforced), `suppressed-inline`, or `suppressed-baseline`.
    #[serde(default = "default_finding_status")]
    pub status: String,
    /// Other rule ids this same location also violates (the server merged them into this
    /// row). Empty for an un-merged finding. Surfaced as a "+N" on the rule and listed in
    /// the detail modal.
    #[serde(default)]
    pub also_matches: Vec<String>,
    /// PREVIEW (CI-security Part B): the server's scan-time deterministic preview pass ran
    /// the rule's underlying tool ITSELF and produced this finding, even though the rule is
    /// NOT yet wired into the repo's gate. Deterministic (stable tool rule-id) but ADVISORY:
    /// "preview — not enforced until wired". Defaults to `false` (back-compatible).
    #[serde(default)]
    pub preview: bool,
    /// For a preview finding, the tool that produced it (`clippy` | `ruff` | `eslint` |
    /// `semgrep`). `None` for non-preview findings. Shown in the Authority badge label.
    #[serde(default)]
    pub preview_tool: Option<String>,
    /// True when this finding is in test/fixture scope.
    #[serde(default)]
    pub in_test: bool,
    /// True when this finding needs manual verification.
    #[serde(default)]
    pub needs_review: bool,
}

pub(super) fn default_finding_status() -> String {
    "active".to_string()
}

/// Where a finding sits in onboarding triage. The architect moves each finding between these
/// three tables (a single-select switches the view) until nothing is Unresolved; then the
/// ignored and tech-debt buckets are processed. This is LOCAL triage state — the backend
/// commit (baseline waiver / ticket / dev-engine import) happens at Process, not on each move.
#[derive(Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub(super) enum TriageState {
    #[default]
    Unresolved,
    Ignored,
    TechDebt,
}

impl TriageState {
    fn label(self) -> &'static str {
        match self {
            Self::Unresolved => "Unresolved",
            Self::Ignored => "Ignored",
            Self::TechDebt => "Tech debt",
        }
    }
}

/// Which tech-debt bucket a finding is in: resolve LATER (file a tracked ticket) or NOW (pull
/// into the dev engine as the first story). Only meaningful when state == TechDebt.
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) enum TechDebtBucket {
    Later,
    Now,
}

/// One finding's triage disposition: its table, the (required) ignore reason, and its
/// tech-debt bucket. Absence from the dispositions map == Unresolved with defaults.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct Disposition {
    pub state: TriageState,
    pub reason: String,
    pub bucket: TechDebtBucket,
}

/// Stable identity for a finding across the triage tables (repo + rule + location + snippet),
/// so its disposition survives table switches and re-sorts.
pub(super) fn finding_key(f: &FindingView) -> String {
    format!(
        "{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
        f.repo, f.rule_id, f.path, f.line, f.snippet
    )
}

/// The disposition state for a finding (Unresolved when absent from the map).
pub(super) fn finding_state(
    dispositions: &std::collections::HashMap<String, Disposition>,
    f: &FindingView,
) -> TriageState {
    dispositions
        .get(&finding_key(f))
        .map(|d| d.state)
        .unwrap_or(TriageState::Unresolved)
}

/// Durable ignore: record the findings as reasoned baseline suppressions (governed PR).
/// Returns the PR URL.
pub(super) async fn ignore_findings(
    repo: &str,
    findings: &[FindingView],
    reason: &str,
    ticket: Option<String>,
) -> Option<String> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/onboard/ignore", crate::BFF_URL))
        .json(&serde_json::json!({ "repo": repo, "findings": findings, "reason": reason, "ticket": ticket }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    if !v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        return None;
    }
    v.get("url").and_then(|u| u.as_str()).map(String::from)
}

/// Real audit usage from the server (the actual half of actual-vs-estimated).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
pub(super) struct ActualUsageView {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub calls: u64,
    /// False when some calls didn't report a cost (the dollar figure is a partial sum).
    #[serde(default)]
    pub cost_complete: bool,
    /// Tokens served from the prompt cache (billed at ~0.1x input). Nonzero only when
    /// the API backend ran with prompt caching active (multi-batch parallel scans).
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    /// Tokens written to the prompt cache (billed at ~1.25x input, one-time per TTL).
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
}

/// One SOC-2 control gap entry from the deep tier, mirroring `DeepReport.soc2_gaps`.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub(super) struct Soc2GapView {
    pub control: String,
    #[serde(default)]
    pub title: String,
    /// One of "met" | "partial" | "gap" | "unknown".
    pub status: String,
    #[serde(default)]
    pub observed: String,
    #[serde(default)]
    pub gap: String,
}

/// One deep-tier lens result (SOC-2 gap / deep-security / threat-model).
/// Mirrors `ai_audit::DeepLensResult` on the wire.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub(super) struct DeepLensResultView {
    /// Stable id: "soc2-gap" | "deep-security" | "threat-model".
    pub lens: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub soc2_gaps: Vec<Soc2GapView>,
    /// Extra security / threat findings (deep-security + threat-model free-text content).
    #[serde(default)]
    pub detail: String,
    /// Always `true` for the deep tier — the whole tier is model-inferred, advisory.
    #[serde(default)]
    pub advisory: bool,
    /// Per-lens honesty disclaimer surfaced in the UI.
    #[serde(default)]
    pub disclaimer: String,
}

/// The top-level deep-tier output attached to a scan report when `deep: true` was sent.
/// Mirrors `ai_audit::DeepReport` on the wire.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub(super) struct DeepReportView {
    pub lenses: Vec<DeepLensResultView>,
    /// Always `true` — the whole tier is advisory.
    #[serde(default)]
    pub advisory: bool,
    /// Honesty disclaimer for the whole tier.
    #[serde(default)]
    pub disclaimer: String,
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
pub(super) struct CoverageNoteView {
    pub tool: String,
    pub message: String,
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub(super) struct ScanReportView {
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub stacks: Vec<StackView>,
    pub files_scanned: usize,
    #[serde(default)]
    pub files_excluded: usize,
    #[serde(default)]
    pub code_chars: usize,
    /// Mechanical rule ids dropped from the code-only scan (enforced in CI instead).
    #[serde(default)]
    pub excluded_mechanical_rules: Vec<String>,
    #[serde(default)]
    pub actual_usage: Option<ActualUsageView>,
    pub findings: Vec<FindingView>,
    pub proposed_rules: Vec<ProposedRuleView>,
    pub gated: bool,
    #[serde(default)]
    pub message: Option<String>,
    /// OPT-IN deep compliance & security tier output (#55). `None` unless the audit
    /// request sent `deep: true`. Everything inside is ADVISORY + model-inferred.
    #[serde(default)]
    pub deep: Option<DeepReportView>,
    /// Coverage notes from the scan preview (tools skipped or unavailable).
    #[serde(default)]
    pub coverage_notes: Vec<CoverageNoteView>,
}

pub(super) async fn scan_repos(repos: &[String]) -> Option<ScanReportView> {
    // Hold a loading guard for the full duration of the scan request so the
    // background Bombe machine runs while the server is scanning.
    let _guard = crate::loading::LoadingGuard::new();
    reqwest::Client::new()
        .post(format!("{}/api/onboard/scan", crate::BFF_URL))
        .json(&serde_json::json!({ "repos": repos }))
        .send()
        .await
        .ok()?
        .json::<ScanReportView>()
        .await
        .ok()
}

/// The persisted in-flight onboarding state (issue #27). Saved continuously so a brownfield
/// onboarding survives an app restart — the architect doesn't re-scan to keep testing the
/// post-scan features. The scan + audit (the expensive artifacts) plus the per-repo rule
/// selection, triage dispositions, and view state are all sticky.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct OnboardingDraft {
    pub scan: ScanReportView,
    #[serde(default)]
    pub audit: Option<ScanReportView>,
    #[serde(default)]
    pub repo_selection: std::collections::HashMap<String, Vec<String>>,
    // The architect's chosen alternative per rule (rule id -> option id). Persisted so a
    // non-default option survives reload — without this the choices reset to defaults.
    #[serde(default)]
    pub chosen: std::collections::HashMap<String, String>,
    // User-authored custom rules created during onboarding (Custom + Custom Global). Persisted
    // so they survive reload; written to the project's ruleset.custom on apply/complete.
    #[serde(default)]
    pub custom: Vec<CustomRuleView>,
    #[serde(default)]
    pub dispositions: std::collections::HashMap<String, Disposition>,
    #[serde(default)]
    pub viewed_repo: String,
    #[serde(default)]
    pub triage_view: TriageState,
}

/// Load the saved onboarding draft, or None when nothing is in progress.
pub(super) async fn load_onboarding_draft() -> Option<OnboardingDraft> {
    reqwest::Client::new()
        .get(format!("{}/api/onboard/draft", crate::BFF_URL))
        .send()
        .await
        .ok()?
        .json::<Option<OnboardingDraft>>()
        .await
        .ok()
        .flatten()
}

/// Persist the current onboarding draft (best-effort; failure is non-fatal).
pub(super) async fn save_onboarding_draft(draft: &OnboardingDraft) {
    let _ = reqwest::Client::new()
        .post(format!("{}/api/onboard/draft", crate::BFF_URL))
        .json(draft)
        .send()
        .await;
}

/// Drop the saved draft (a fresh scan starts a new session; clearing avoids re-seeding the
/// previous run's audit/dispositions onto it).
pub(super) async fn clear_onboarding_draft() {
    let _ = reqwest::Client::new()
        .post(format!("{}/api/onboard/draft/clear", crate::BFF_URL))
        .send()
        .await;
}

/// Finish onboarding for the active project: marks its repos onboarded and clears the
/// draft. The post-scan steps (audit / triage / apply / wire-CI) are all optional, so this
/// is the explicit "I'm done" action. Returns true on success.
pub(super) async fn complete_onboarding() -> bool {
    let Ok(resp) = reqwest::Client::new()
        .post(format!("{}/api/onboard/complete", crate::BFF_URL))
        .send()
        .await
    else {
        return false;
    };
    resp.json::<serde_json::Value>()
        .await
        .ok()
        .and_then(|v| v.get("ok").and_then(|b| b.as_bool()))
        .unwrap_or(false)
}

/// A rule the user selected to audit against, carrying its per-repo binding. An empty
/// `repos` means PROJECT-LEVEL (every repo); a non-empty `repos` scopes the rule to just
/// those repos. The backend audits each repo against only the rules that apply to it, so a
/// multi-repo scan runs each repo against its own rules ∪ the project-level set.
#[derive(Clone, PartialEq)]
pub(super) struct SelectedAuditRule {
    pub id: String,
    pub directive: String,
    pub repos: Vec<String>,
}

/// Serialize selected rules into the audit request shape (`{id, directive, repos}` each).
pub(super) fn audit_rules_json(rules: &[SelectedAuditRule]) -> Vec<serde_json::Value> {
    rules
        .iter()
        .map(|r| serde_json::json!({ "id": r.id, "directive": r.directive, "repos": r.repos }))
        .collect()
}

/// Phase 2 — audit the repos against the selected rules (each carrying its repo binding).
/// When `deep` is true, the server also runs the three deep-tier lenses (SOC-2 gap,
/// deep security, threat model) and attaches the results to `report.deep`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn audit_against(
    repos: &[String],
    rules: &[SelectedAuditRule],
    model: &str,
    calibration_model: &str,
    mode: &str,
    thorough: bool,
    incremental: bool,
    deep: bool,
    run_ai_review: bool,
    run_deterministic: bool,
) -> Option<ScanReportView> {
    // Loading guard: the Bombe machine runs for the full audit round-trip.
    let _guard = crate::loading::LoadingGuard::new();
    let rule_json = audit_rules_json(rules);
    reqwest::Client::new()
        .post(format!("{}/api/onboard/audit", crate::BFF_URL))
        .json(&serde_json::json!({
            "repos": repos,
            "rules": rule_json,
            "model": model,
            "calibration_model": calibration_model,
            "mode": mode,
            "thorough": thorough,
            "incremental": incremental,
            "deep": deep,
            "run_ai_review": run_ai_review,
            "run_deterministic": run_deterministic,
        }))
        .send()
        .await
        .ok()?
        .json::<ScanReportView>()
        .await
        .ok()
}

/// One model entry in a selector, sourced from `GET /api/models/registry`.
///
/// `label` carries a badge-enriched display string (e.g. "DeepSeek R1  $0.55/M · tool-use · 64K · cache").
/// `provider` groups the entry in `<optgroup>` elements ("claude" | "openrouter").
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub(super) struct AuditModelOption {
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
}

/// Build a badge-enriched display label from registry entry fields.
///
/// Format: "<display>  FREE · tool-use · 200K · cache"
/// Or for paid: "<display>  $0.55/M · tool-use · 64K · cache"
///
/// - Price: `FREE` if free, else `$<price_out>/M` (output price, compact).
/// - `tool-use`: always shown (absence as `no-tools` for OpenRouter models lacking it).
/// - Context: `<N>K` (e.g. `200K`, `64K`).
/// - `cache`: shown only when `caching` is true.
///
/// Badges are separated by ` · ` and appended after two spaces following the display name.
fn model_badge_label(
    display: &str,
    free: bool,
    tool_use: bool,
    context: u64,
    price_out: f64,
    caching: bool,
) -> String {
    let mut parts = Vec::<String>::new();

    // Price badge: FREE or $<price>/M (output price).
    if free {
        parts.push("FREE".to_string());
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
struct RegistryResp {
    models: Vec<RegistryEntryWire>,
    #[serde(default)]
    openrouter_fetched: bool,
}

/// One entry from the registry wire response. Mirrors `camerata_server::model_registry::RegistryEntry`.
#[derive(serde::Deserialize)]
struct RegistryEntryWire {
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
            ),
            id: self.id.clone(),
            provider: self.provider.clone(),
            free: self.free,
            tool_use: self.tool_use,
            context: self.context,
            price_in: self.price_in,
            price_out: self.price_out,
            caching: self.caching,
        }
    }
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub(super) struct AuditModelsResp {
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
        let claude: Vec<&AuditModelOption> =
            self.models.iter().filter(|m| m.provider == "claude").collect();
        let openrouter: Vec<&AuditModelOption> =
            self.models.iter().filter(|m| m.provider == "openrouter").collect();
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
pub(super) async fn fetch_audit_models() -> Option<AuditModelsResp> {
    let resp = reqwest::get(format!("{}/api/models/registry", crate::BFF_URL))
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

    Some(AuditModelsResp { models, default, openrouter_fetched: resp.openrouter_fetched })
}

/// Rough pre-audit cost estimate, returned as (total_tokens, dollars, passes). Mirrors the
/// server's chunk/batch math (ai_audit) so the number tracks what the audit actually sends.
///
/// Input and output are priced SEPARATELY (output bills ~5× input and dominates
/// findings-heavy scans). The estimate is deliberately biased slightly CONSERVATIVE (high):
/// an estimate that turns into a smaller bill is a pleasant surprise; one that turns into
/// a bigger bill is broken trust.
///
/// PROMPT CACHING: for multi-batch parallel scans (the default), the codebase prefix (repo
/// map + chunk digest) is the same across every rule-batch for a given chunk. When the API
/// backend is in use the server marks this prefix with `cache_control: ephemeral` so the
/// provider caches it after the first batch and reads it at ~0.1× for subsequent batches.
/// The estimate models this:
///   - batch 0 per chunk: full input price + 1.25× cache-write surcharge on the digest
///   - batches 1..N per chunk: digest tokens read from cache at 0.1× instead of 1.0×
/// Sequential mode (one batch per chunk) has no prefix reuse across batches, so no caching
/// discount applies. CLI backend also skips caching (no-op there).
///
/// The FUDGE factor keeps the estimate conservative overall even after the cache discount,
/// since the calibration pass (over aggregated findings) and the resolution round are
/// modeled at full price.
/// `code_chars` is the in-scope code size. The caller is responsible for passing the size of
/// the SCANNED file set: the whole repo for a full scan. For an incremental re-scan only the
/// CHANGED files are actually sent to the AI, but the client has no per-file / changed-file
/// token breakdown today (`ScanReportView` carries only the repo-total `code_chars`), so we
/// price the FULL set and flag `incremental` in the readout as a known over-estimate. See the
/// followup in `docs/decisions/2026-06-20_ui_bugfixes.md`.
///
/// `deep` (the SOC-2 / deep-security / threat-model tier) adds three EXTRA whole-repo prose
/// passes at the AUDIT model on top of the standard scan + calibration: each re-reads the full
/// `code_chars` as input and emits a long prose report. Deep is therefore the priciest option
/// and the returned dollar figure reflects that, not just a prose warning.
#[allow(clippy::too_many_arguments)]
pub(super) fn estimate_audit_cost(
    code_chars: usize,
    selected: usize,
    mode: &str,
    audit_in: f64,
    audit_out: f64,
    calib_in: f64,
    calib_out: f64,
    thorough: bool,
    incremental: bool,
    deep: bool,
) -> (u64, f64, usize) {
    const CHUNK_DIGEST_CHARS: usize = 350_000;
    const RULE_BATCH_SIZE: usize = 15;
    const CHARS_PER_TOKEN: f64 = 4.0;
    // Per-pass overhead (rules block + system prompt) that varies per batch and is never
    // cached. The digest + repo map form the cached prefix, so only this remainder is
    // re-sent at full price for subsequent batches.
    const OVERHEAD_CHARS_PER_PASS: usize = 10_000;
    // Output is findings: a baseline per pass plus a term that scales with code scanned
    // (so a findings-dense or large scan isn't under-counted on the half that bites most).
    const OUT_TOKENS_PER_PASS: f64 = 2_200.0;
    const OUTPUT_PER_CODE_TOKEN: f64 = 0.02;
    // Resolution round + general conservatism. Biased HIGH on purpose: logged real runs
    // (budget-mini ~2.24×, chorale ~1.75×) came in UNDER estimate even before caching, and
    // an audit that costs more than quoted is the bad surprise.
    const FUDGE: f64 = 1.4;
    // Prompt-cache pricing multipliers (Anthropic list pricing as of 2024-07):
    //   write (first batch per chunk): 1.25× input
    //   read  (subsequent batches):    0.10× input
    const CACHE_WRITE_MULT: f64 = 1.25;
    const CACHE_READ_MULT: f64 = 0.10;
    // Deep tier (#55): three EXTRA whole-repo passes (SOC-2 gap, deep security, threat model).
    // Each reads the full code once and emits a long prose report. Priced at the audit model.
    const DEEP_PASSES: f64 = 3.0;
    // A deep pass emits far more prose than a per-rule finding pass (full report per lens).
    const DEEP_OUT_TOKENS_PER_PASS: f64 = 8_000.0;

    // Batch mode (#61): the Anthropic Message Batches API charges a flat 50% discount on
    // ALL input and output tokens for the SCAN passes (which are submitted as a batch).
    // The calibration pass always runs real-time (a single call over aggregated findings
    // — not batched), so calib pricing is NOT discounted.
    let batch_discount = if mode == "batch" { 0.5 } else { 1.0 };
    let (eff_audit_in, eff_audit_out) = (audit_in * batch_discount, audit_out * batch_discount);
    // Calibration is real-time even in batch mode: one call over the aggregated findings.
    let (eff_calib_in, eff_calib_out) = (calib_in, calib_out);

    let chunks = code_chars.div_ceil(CHUNK_DIGEST_CHARS).max(1);
    let batches = if mode == "sequential" {
        1
    } else {
        selected.div_ceil(RULE_BATCH_SIZE).max(1)
    };
    let passes = chunks * batches;
    let code_tokens = code_chars as f64 / CHARS_PER_TOKEN;

    // ── Scan passes, priced at the AUDIT model (with batch discount applied) ──
    //
    // Without caching: the full digest is re-sent at full input price every pass.
    // With caching (parallel/batch mode, batches > 1): per chunk, batch 0 pays full input
    // + the one-time 1.25× cache-write surcharge; batches 1..N read the cached digest at
    // 0.1×. Sequential (batches == 1) has no reuse, so no discount.
    //
    // Overhead tokens (rules block, system prompt) are always sent at full price since they
    // vary per batch.
    let scan_in = if batches <= 1 {
        // No caching benefit: every batch pays full price for the digest.
        (code_chars * batches + OVERHEAD_CHARS_PER_PASS * passes) as f64 / CHARS_PER_TOKEN
    } else {
        // Batch 0 per chunk: full digest price + cache-write surcharge.
        // Batches 1..N per chunk: digest at cache-read rate (0.1×).
        let digest_tokens_per_chunk = code_chars as f64 / chunks as f64 / CHARS_PER_TOKEN;
        let write_cost = digest_tokens_per_chunk * CACHE_WRITE_MULT * chunks as f64;
        let read_cost = digest_tokens_per_chunk
            * CACHE_READ_MULT
            * (batches.saturating_sub(1)) as f64
            * chunks as f64;
        // Overhead (never cached) is full price for every pass.
        let overhead_cost = OVERHEAD_CHARS_PER_PASS as f64 / CHARS_PER_TOKEN * passes as f64;
        write_cost + read_cost + overhead_cost
    };
    let scan_out =
        OUT_TOKENS_PER_PASS * passes as f64 + OUTPUT_PER_CODE_TOKEN * code_tokens * batches as f64;

    // ── Calibration: ONE pass over all findings, priced at the CALIBRATION model. It
    // re-reads roughly the scan's output (the findings) and RE-EMITS each finding with a
    // corrected/verified body. So its output rides with the full findings volume, ~1× the
    // scan's output. Thorough mode (#51) runs ~3× for multi-vote consensus.
    let cal_passes = if thorough { 3.0 } else { 1.0 };
    let cal_in = scan_out * cal_passes;
    let cal_out = scan_out * cal_passes;

    // ── Deep tier: three EXTRA whole-repo prose passes at the AUDIT model. Each reads the
    // full code (no per-rule batching, no caching discount — distinct prompts per lens) and
    // emits a long prose report. This is the dominant cost when enabled, which is why deep is
    // surfaced as the priciest option in the readout. Batch discount does NOT apply (these run
    // real-time as part of the deep lens flow, not in the scan batch).
    let (deep_in, deep_out) = if deep {
        let full_code_tokens = code_chars as f64 / CHARS_PER_TOKEN;
        let din = full_code_tokens * DEEP_PASSES;
        let dout = DEEP_OUT_TOKENS_PER_PASS * DEEP_PASSES;
        (din, dout)
    } else {
        (0.0, 0.0)
    };

    // Incremental scope (only changed files actually billed) would lower the scan portion, but
    // the client has no changed-file token breakdown today (see fn doc + followup), so we keep
    // the full-scan price and let the readout flag incremental as an over-estimate. Bind the
    // flag so its role is explicit even though the number is unchanged here.
    let _ = incremental;

    let dollars = ((scan_in * eff_audit_in + scan_out * eff_audit_out)
        + (cal_in * eff_calib_in + cal_out * eff_calib_out)
        + (deep_in * audit_in + deep_out * audit_out))
        / 1_000_000.0
        * FUDGE;
    let total_tokens =
        ((scan_in + scan_out + cal_in + cal_out + deep_in + deep_out) * FUDGE) as u64;
    (total_tokens, dollars, passes)
}

/// Compact human token count: 2.0M / 350k / 900.
pub(super) fn human_tokens(t: u64) -> String {
    if t >= 1_000_000 {
        format!("{:.1}M", t as f64 / 1_000_000.0)
    } else if t >= 1_000 {
        format!("{:.0}k", t as f64 / 1_000.0)
    } else {
        t.to_string()
    }
}

/// One deterministic-scan tool's live progress (mirror of the server's `DetToolProgress`).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
pub(super) struct DetToolProgressView {
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
pub(super) struct DetProgressView {
    #[serde(default)]
    pub tools: Vec<DetToolProgressView>,
    #[serde(default)]
    pub done: usize,
    #[serde(default)]
    pub total: usize,
}

/// A polled async-audit job (`GET /api/onboard/audit/job/:id`).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
pub(super) struct JobStateView {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub done: usize,
    #[serde(default)]
    pub total: usize,
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
pub(super) struct JobStatusEnvelope {
    pub job: JobStateView,
    /// Milliseconds since last job progress update. `None` if no activity recorded yet.
    #[serde(default)]
    pub idle_ms: Option<u128>,
    /// True if a cancel has been requested for this job.
    #[serde(default)]
    pub cancel_requested: bool,
}

/// Mode 3: START an async audit job, returning its id (the request returns immediately).
/// `deep` forwards the opt-in deep compliance & security tier (#55); the server
/// runs the three lenses after the standard audit completes and attaches the result
/// to the final job report's `deep` field.
#[allow(clippy::too_many_arguments)]
pub(super) async fn audit_job_start(
    repos: &[String],
    rules: &[SelectedAuditRule],
    model: &str,
    calibration_model: &str,
    exec_mode: &str,
    thorough: bool,
    incremental: bool,
    deep: bool,
    run_ai_review: bool,
    run_deterministic: bool,
) -> Option<String> {
    let rule_json = audit_rules_json(rules);
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/onboard/audit/start", crate::BFF_URL))
        .json(&serde_json::json!({
            "repos": repos,
            "rules": rule_json,
            "model": model,
            "calibration_model": calibration_model,
            "mode": exec_mode,
            "thorough": thorough,
            "incremental": incremental,
            "deep": deep,
            "run_ai_review": run_ai_review,
            "run_deterministic": run_deterministic,
        }))
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
/// Parallel (fast enough to wait on). Sequential is never auto-recommended — it's a manual
/// gentle/debug override. The user can always change it.
pub(super) fn recommend_scan_mode(report: &ScanReportView) -> String {
    if report.repos.len() > 1 || report.files_scanned > 150 {
        "job".to_string()
    } else {
        "parallel".to_string()
    }
}

/// Poll an async audit job for progress + incremental findings + the final report.
pub(super) async fn audit_job_poll(job_id: &str) -> Option<JobStatusEnvelope> {
    reqwest::get(format!(
        "{}/api/onboard/audit/job/{}",
        crate::BFF_URL,
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
#[allow(clippy::too_many_arguments)]
pub(super) async fn poll_job(
    jid: String,
    mut audit: Signal<Option<ScanReportView>>,
    mut auditing: Signal<bool>,
    mut job_progress: Signal<Option<(usize, usize, usize)>>,
    // Live DETERMINISTIC-pass progress (floor + preview tools), rendered by the
    // "Deterministic scan" component above the AI agent-activity drawer. `None` clears it.
    mut det_progress: Signal<Option<DetProgressView>>,
    mut active_audit_job: Signal<Option<String>>,
    mut scan_idle_ms: Signal<Option<u128>>,
) {
    // Loading guard held for the ENTIRE poll loop so the Bombe machine stays
    // active until the background job reports done/failed/cancelled.
    let _guard = crate::loading::LoadingGuard::new();
    let mut misses = 0u32;
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        match audit_job_poll(&jid).await {
            Some(js) => {
                misses = 0;
                scan_idle_ms.set(js.idle_ms);
                job_progress.set(Some((js.job.done, js.job.total, js.job.findings.len())));
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
pub(super) fn DeterministicProgress(progress: DetProgressView) -> Element {
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

/// Friendly label for a deterministic tool name. `floor` is the always-on security scanner;
/// the rest are the scan-preview linters; `unrouted` collects rules with no driveable tool.
pub(super) fn det_tool_label(tool: &str) -> String {
    match tool {
        "floor" => "Security floor".to_string(),
        "unrouted" => "Unrouted rules".to_string(),
        other => other.to_string(),
    }
}

pub(super) fn finding_columns(repos: Vec<String>, show_bucket: bool) -> Vec<ColumnDef<FindingView>> {
    // chorale 0.2.3's palette has a native orange, so each severity gets a distinct color
    // straight from RenderKind::Badge — no custom cell renderer needed (Critical = red,
    // High = orange, Medium = yellow, Low = gray).
    let sev = BadgeVariantMap::new()
        .with("critical", BadgeVariant::new("Critical", "red"))
        .with("high", BadgeVariant::new("High", "orange"))
        .with("medium", BadgeVariant::new("Medium", "yellow"))
        .with("low", BadgeVariant::new("Low", "gray"));
    let mut cols = vec![
        ColumnDef::new(ColumnId("repo"), "Repo", |f: &FindingView| {
            CellValue::Text(f.repo.clone())
        })
        .sortable()
        .filter(FilterKind::MultiSelect { options: repos })
        .initial_width(180.0),
        ColumnDef::new(ColumnId("severity"), "Severity", |f: &FindingView| {
            CellValue::Text(f.severity.clone())
        })
        .sortable()
        .filter(FilterKind::MultiSelect {
            options: vec![
                "critical".to_string(),
                "high".to_string(),
                "medium".to_string(),
                "low".to_string(),
            ],
        })
        .render_kind(RenderKind::Badge(sev))
        .initial_width(110.0),
        // AUTHORITY, not just provenance: a DETERMINISTIC-FLOOR hit is ENFORCED (regex/logic,
        // repeatable, gateable, stable id); EVERY other finding is ADVISORY (model-inferred,
        // review-only, id/severity may drift run-to-run, never auto-blocks). This is the
        // enforcement-vs-convention split rendered as a column. NOTE: advisory covers BOTH
        // `AI-*` invented ids AND the AI judging code against a corpus rule (e.g. RUST-DIOXUS-11)
        // — the old `AI-` prefix check mislabeled the latter as enforced. Keyed on the floor set.
        // PREVIEW is a THIRD authority tier between enforced and advisory: a scan-time
        // deterministic-tool finding (stable rule-id, no model in the trust path) that is NOT
        // yet wired into the repo's gate. It must read DISTINCTLY from an enforced floor hit
        // ("preview — not enforced until wired") AND from an AI-advisory finding (deterministic,
        // not model-inferred). The CI story still has to wire it for the gate to block on it.
        ColumnDef::new(ColumnId("authority"), "Authority", |f: &FindingView| {
            CellValue::Text(if f.preview {
                "preview".to_string()
            } else if is_enforced_floor(&f.rule_id) {
                "enforced".to_string()
            } else {
                "advisory".to_string()
            })
        })
        .sortable()
        .filter(FilterKind::MultiSelect {
            options: vec![
                "enforced".to_string(),
                "preview".to_string(),
                "advisory".to_string(),
            ],
        })
        .render_kind(RenderKind::Badge(
            BadgeVariantMap::new()
                // chorale 0.2.3 added blue/purple to the palette, so the authorities
                // read as distinct colors (no more gray fallback collision).
                .with("enforced", BadgeVariant::new("Rule · enforced", "green"))
                .with("preview", BadgeVariant::new("Preview · not enforced until wired", "purple"))
                .with("advisory", BadgeVariant::new("AI · advisory", "blue")),
        ))
        .initial_width(220.0),
        ColumnDef::new(ColumnId("type"), "Finding type", |f: &FindingView| {
            CellValue::Text(f.rule_id.clone())
        })
        .sortable()
        // String lookup, not multi-select: rule ids are many and the architect typically
        // wants "show me everything matching ARCH-" or a specific id, not a checkbox list.
        .filter(FilterKind::Text)
        .initial_width(250.0),
        // "Needs review": the calibration pass's applicability flag with its reason. Every
        // finding technically needs review; THESE are flagged for a specific reason (usually
        // over-engineering / YAGNI on a small codebase). Surfaced as its own column + reason
        // so the architect can triage the hedged ones at a glance. Text-filterable so you can
        // show only the flagged rows. Drawn by a cell renderer (orange chip + reason).
        // The cell VALUE is just yes/no ("does this need review?") so the filter is a simple
        // two-option toggle, not a free-text box over every distinct reason. The visible chip +
        // reason are drawn by the row renderer (which reads f.detail), so the reason still shows.
        ColumnDef::new(
            ColumnId("needs_review"),
            "Needs review",
            |f: &FindingView| {
                CellValue::Text(if f.in_test {
                    "test".to_string()
                } else if f.needs_review || split_needs_review(&f.detail).1.is_some() {
                    "yes".to_string()
                } else {
                    "no".to_string()
                })
            },
        )
        .sortable()
        .filter(FilterKind::MultiSelect {
            options: vec!["test".to_string(), "yes".to_string(), "no".to_string()],
        })
        .render_kind(RenderKind::Badge(
            BadgeVariantMap::new()
                .with("test", BadgeVariant::new("Test", "yellow"))
                .with("yes", BadgeVariant::new("Needs review", "orange"))
                .with("no", BadgeVariant::new("", "gray")),
        ))
        .initial_width(300.0),
        // The ratchet: enforced (active = new/changed) vs suppressed (baseline debt or
        // an inline waiver). Report shows all; the gate blocks only the enforced ones.
        ColumnDef::new(ColumnId("status"), "Enforcement", |f: &FindingView| {
            CellValue::Text(match f.status.as_str() {
                "suppressed-baseline" => "baseline".to_string(),
                "suppressed-inline" => "waived".to_string(),
                "suppressed-self-reference" => "self-ref".to_string(),
                _ => "enforced".to_string(),
            })
        })
        .sortable()
        .render_kind(RenderKind::Badge(
            BadgeVariantMap::new()
                .with("enforced", BadgeVariant::new("Enforced", "red"))
                .with("baseline", BadgeVariant::new("Baseline debt", "gray"))
                .with("waived", BadgeVariant::new("Waived", "yellow"))
                .with("self-ref", BadgeVariant::new("Self-referential", "gray")),
        ))
        .initial_width(150.0),
        // Second grouping level: the FILE (path only). The findings table groups by
        // rule → file, so a rule violated 4× across one file collapses under a
        // "handlers.rs (4)" sub-header instead of 4 loose rows. Path-only (not path:line)
        // so all sites in a file share one group; the line lives in the Line column.
        ColumnDef::new(ColumnId("file"), "File", |f: &FindingView| {
            CellValue::Text(f.path.clone())
        })
        .sortable()
        .filter(FilterKind::Text)
        .initial_width(260.0),
        ColumnDef::new(ColumnId("loc"), "Line", |f: &FindingView| {
            CellValue::Text(f.line.to_string())
        })
        .sortable()
        .initial_width(80.0),
        ColumnDef::new(ColumnId("snippet"), "Snippet", |f: &FindingView| {
            CellValue::Text(f.snippet.clone())
        })
        .initial_width(380.0),
    ];
    // The tech-debt bucket flag (resolve later / now). Drawn by a row renderer that reads the
    // live disposition map; the accessor is a placeholder. Only present in the tech-debt view.
    if show_bucket {
        cols.push(
            ColumnDef::new(ColumnId("bucket"), "Bucket", |_f: &FindingView| {
                CellValue::Text(String::new())
            })
            .initial_width(120.0),
        );
    }
    cols
}

/// The findings table with TRIAGE: sort by repo/severity/type, filter, select rows
/// and Ignore / Resolve / Accept-as-tech-debt (open a ticket) them. Virtualized by
/// chorale, so a large audit doesn't choke the UI.
#[component]
pub(super) fn FindingsTable(
    findings: Vec<FindingView>,
    repos: Vec<String>,
    descriptions: std::collections::HashMap<String, String>,
    // Which triage table this is (issue #26). Findings not in this state are filtered out;
    // the component is keyed on it by the parent, so a switch remounts with that table's set.
    #[props(default = TriageState::Unresolved)] triage_view: TriageState,
    // The lifted finding -> disposition map. Move actions write here; the row is then dropped
    // from this table (remove_rows) and reappears under its new table on the next switch.
    #[props(default)] dispositions: Signal<std::collections::HashMap<String, Disposition>>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    // Keep only the findings in THIS table's triage state (absent from the map = Unresolved).
    let findings: Vec<FindingView> = {
        let d = dispositions.peek();
        findings
            .into_iter()
            .filter(|f| finding_state(&d, f) == triage_view)
            .collect()
    };
    // Default order leads triage with what matters: enforced (new) before suppressed
    // (debt/waived), then by severity (critical → high → medium → low). A flat 200-row dump
    // is paralysis; this floats the exploitable-bug criticals to the very top so a
    // hardcoded secret can never sit below "no mappers crate."
    let mut findings = findings;
    findings.sort_by_key(|f| {
        let enforced = if f.status == "active" { 0 } else { 1 };
        let sev = match f.severity.as_str() {
            "critical" => 0,
            "high" => 1,
            "medium" => 2,
            _ => 3,
        };
        (enforced, sev)
    });
    // Distinct repos for the repo multi-select filter. (Finding type is a Text/contains
    // filter now, so it needs no precomputed option list.)
    let mut filter_repos: Vec<String> = findings.iter().map(|f| f.repo.clone()).collect();
    filter_repos.sort();
    filter_repos.dedup();
    // Mint row ids ONCE per mount (see ProposedRulesTable): `RowId::new()` in the render
    // body would re-id every render and desync the Table from id_map/selection. This
    // component remounts on each new audit (it's gated behind `audited.is_some()` and the
    // re-audit clears it first), so freezing rows per mount tracks fresh findings while
    // keeping ids stable within a mount.
    let rows: Vec<(RowId, FindingView)> = use_hook({
        let findings = findings.clone();
        move || findings.iter().map(|f| (RowId::new(), f.clone())).collect()
    });
    let id_map: std::collections::HashMap<RowId, FindingView> =
        rows.iter().map(|(r, f)| (*r, f.clone())).collect();
    let id_map_click = id_map.clone();
    // Row click opens the finding-detail modal (hosted by ScanResults, OUTSIDE this table's
    // subtree — same reason as the rule modal). Shows the violated rule's full directive +
    // the complete, untruncated explanation that the row cell clips.
    let mut detail_finding = use_context::<Signal<Option<FindingView>>>();
    let in_techdebt = triage_view == TriageState::TechDebt;
    let handle = use_table(move || {
        TableState::new(
            rows.clone(),
            finding_columns(filter_repos.clone(), in_techdebt),
        )
    });
    // Subscribe to the disposition map so the bucket flag column re-renders when the architect
    // marks resolve-later/now (the renderer below captures this snapshot).
    let bucket_snapshot = dispositions.read().clone();
    // Two-level grouping: by RULE, then by FILE within each rule. chorale groups by an
    // ordered key list (it recurses through the Vec, one depth per key), so a rule violated
    // 4× across one file renders as "RULE (4)" → "handlers.rs (4)" → the 4 individual lines.
    // Counts come free on every header. This is a PRESENTATION view of the flat finding
    // list; the CSV export stays flat + lossless (one row per finding), unchanged.
    use_hook(move || {
        handle.set_grouping(vec![ColumnId("type"), ColumnId("file")]);
        // Load all groups first, then collapse all by default — the architect drills in
        // rule → file → lines. (collapse_all only collapses groups in the loaded view, so it
        // must run after the page size is raised.)
        handle.set_pagination_mode(PaginationMode::InfiniteScroll);
        let _ = handle.set_page_size(5000);
        handle.collapse_all_groups();
    });
    // A durable ignore requires a reason (the require-reason invariant), captured here and
    // stored on the disposition; it's committed to the baseline at Process.
    let mut ignore_reason = use_signal(String::new);
    // Two id_map clones: each triage table renders two move buttons, and the two closures in
    // an arm each move a clone. Match arms are mutually exclusive, so the same two clones
    // serve every arm.
    let id_map_a = id_map.clone();
    let id_map_b = id_map.clone();
    // Two more clones for the tech-debt bucket buttons (resolve later / now).
    let id_map_c = id_map.clone();
    let id_map_d = id_map.clone();
    // The (sorted) rows for CSV export.
    let csv_rows = findings.clone();

    // SECURITY findings (the deterministic floor — the only tier ranked "critical") get a
    // red full-row highlight so they're unmistakable beyond the badge text. This now uses
    // chorale 0.2.3's `row_class` hook on the Table (below), not a per-cell stripe renderer.
    let row_renderers = {
        let mut m: std::collections::HashMap<ColumnId, RowCellRenderer<FindingView>> =
            std::collections::HashMap::new();
        // "Finding type": the primary rule id, with a hover tooltip of what it enforces, and
        // a "+N" chip when the server merged N other rules at this same location into the row
        // (the also_matches set). Row-aware so it can read both the description map and the
        // also_matches off the FindingView. Tooltip lists the demoted rule ids too.
        let desc = descriptions.clone();
        m.insert(
            ColumnId("type"),
            std::sync::Arc::new(move |f: &FindingView, val: &CellValue| {
                let rid = match val {
                    CellValue::Text(s) => s.clone(),
                    _ => String::new(),
                };
                let mut tip = desc.get(&rid).cloned().unwrap_or_else(|| rid.clone());
                if !f.also_matches.is_empty() {
                    tip = format!("{tip}\n\nAlso violates here: {}", f.also_matches.join(", "));
                }
                let extra = f.also_matches.len();
                rsx! {
                    span { title: "{tip}", "{rid}" }
                    if extra > 0 {
                        span { class: "finding-also-count", title: "{tip}", " +{extra}" }
                    }
                }
            }) as RowCellRenderer<FindingView>,
        );
        // "Needs review" flag + reason: an orange chip when the calibration pass hedged this
        // finding, followed by the specific reason. Blank when not flagged.
        m.insert(
            ColumnId("needs_review"),
            std::sync::Arc::new(
                move |f: &FindingView, _val: &CellValue| {
                    if f.in_test {
                        rsx! { span { class: "badge badge-yellow", "Test" } }
                    } else {
                        match split_needs_review(&f.detail).1 {
                            Some(reason) => {
                                let reason = reason.clone();
                                rsx! {
                                    span { class: "nr-flag", "Needs review" }
                                    if !reason.is_empty() {
                                        span { class: "nr-reason", " {reason}" }
                                    }
                                }
                            }
                            None => rsx! {},
                        }
                    }
                },
            ) as RowCellRenderer<FindingView>,
        );
        // Tech-debt bucket flag: reads the live disposition snapshot for this finding and
        // renders a "Later" / "Now" badge. Present only in the tech-debt view.
        if in_techdebt {
            let snap = bucket_snapshot.clone();
            m.insert(
                ColumnId("bucket"),
                std::sync::Arc::new(move |f: &FindingView, _val: &CellValue| {
                    let bucket = snap
                        .get(&finding_key(f))
                        .map(|d| d.bucket)
                        .unwrap_or(TechDebtBucket::Later);
                    let (label, cls) = match bucket {
                        TechDebtBucket::Later => ("Later", "td-bucket later"),
                        TechDebtBucket::Now => ("Now", "td-bucket now"),
                    };
                    rsx! { span { class: "{cls}", "{label}" } }
                }) as RowCellRenderer<FindingView>,
            );
        }
        RowCellRenderers::new(m)
    };

    rsx! {
        // Key: what the red row highlight means. Security (deterministic, Critical) vs the rest.
        div { class: "findings-key",
            span { class: "findings-key-item",
                span { class: "findings-key-swatch crit" }
                "Security findings (deterministic, stop-the-line)"
            }
            span { class: "findings-key-item",
                span { class: "findings-key-swatch arch" }
                "Architectural findings (everything else)"
            }
        }
        div { class: "findings-toolbar",
            // View-specific triage actions. A move writes the new disposition for each
            // selected finding and drops it from this table; it reappears under its target
            // table on the next switch. Backend commit happens later, at Process.
            match triage_view {
                TriageState::Unresolved => rsx! {
                    input {
                        class: "addressee-input ignore-reason",
                        placeholder: "reason to ignore (required)",
                        value: "{ignore_reason}",
                        oninput: move |e| ignore_reason.set(e.value()),
                    }
                    button {
                        class: "btn-restart",
                        onclick: move |_| {
                            let sel = handle.selected_ids();
                            let picked: Vec<FindingView> = sel.iter().filter_map(|id| id_map_a.get(id).cloned()).collect();
                            if picked.is_empty() { return; }
                            let reason = ignore_reason();
                            if reason.trim().is_empty() {
                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Warning, "A reason is required to ignore a finding (it's recorded in the baseline at Process).");
                                return;
                            }
                            let mut d = dispositions.peek().clone();
                            for f in &picked {
                                let e = d.entry(finding_key(f)).or_default();
                                e.state = TriageState::Ignored;
                                e.reason = reason.clone();
                            }
                            dispositions.set(d);
                            handle.remove_rows(&sel);
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Moved {} to Ignored.", picked.len()));
                        },
                        "Ignore with reason \u{2192}"
                    }
                    button {
                        class: "btn-run",
                        onclick: move |_| {
                            let sel = handle.selected_ids();
                            let picked: Vec<FindingView> = sel.iter().filter_map(|id| id_map_b.get(id).cloned()).collect();
                            if picked.is_empty() { return; }
                            let mut d = dispositions.peek().clone();
                            for f in &picked { d.entry(finding_key(f)).or_default().state = TriageState::TechDebt; }
                            dispositions.set(d);
                            handle.remove_rows(&sel);
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Moved {} to Tech debt.", picked.len()));
                        },
                        "Save as tech debt"
                    }
                },
                TriageState::Ignored => rsx! {
                    button {
                        class: "btn-restart",
                        onclick: move |_| {
                            let sel = handle.selected_ids();
                            let picked: Vec<FindingView> = sel.iter().filter_map(|id| id_map_a.get(id).cloned()).collect();
                            if picked.is_empty() { return; }
                            let mut d = dispositions.peek().clone();
                            for f in &picked { d.entry(finding_key(f)).or_default().state = TriageState::Unresolved; }
                            dispositions.set(d);
                            handle.remove_rows(&sel);
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Moved {} back to Unresolved.", picked.len()));
                        },
                        "Move to Unresolved"
                    }
                    button {
                        class: "btn-run",
                        onclick: move |_| {
                            let sel = handle.selected_ids();
                            let picked: Vec<FindingView> = sel.iter().filter_map(|id| id_map_b.get(id).cloned()).collect();
                            if picked.is_empty() { return; }
                            let mut d = dispositions.peek().clone();
                            for f in &picked { d.entry(finding_key(f)).or_default().state = TriageState::TechDebt; }
                            dispositions.set(d);
                            handle.remove_rows(&sel);
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Moved {} to Tech debt.", picked.len()));
                        },
                        "Move to Tech debt"
                    }
                },
                TriageState::TechDebt => rsx! {
                    // Bucket the selected tech-debt findings. These stay in the table; only the
                    // Bucket flag column changes. Default is Later (a tracked ticket); Now pulls
                    // the finding into the dev engine as a fix story at Process.
                    button {
                        class: "btn-edit-sm",
                        onclick: move |_| {
                            let sel = handle.selected_ids();
                            let picked: Vec<FindingView> = sel.iter().filter_map(|id| id_map_c.get(id).cloned()).collect();
                            if picked.is_empty() { return; }
                            let mut d = dispositions.peek().clone();
                            for f in &picked { d.entry(finding_key(f)).or_default().bucket = TechDebtBucket::Later; }
                            dispositions.set(d);
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Marked {} as resolve later.", picked.len()));
                        },
                        "Mark: resolve later"
                    }
                    button {
                        class: "btn-edit-sm",
                        onclick: move |_| {
                            let sel = handle.selected_ids();
                            let picked: Vec<FindingView> = sel.iter().filter_map(|id| id_map_d.get(id).cloned()).collect();
                            if picked.is_empty() { return; }
                            let mut d = dispositions.peek().clone();
                            for f in &picked { d.entry(finding_key(f)).or_default().bucket = TechDebtBucket::Now; }
                            dispositions.set(d);
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Marked {} as resolve now.", picked.len()));
                        },
                        "Mark: resolve now"
                    }
                    button {
                        class: "btn-restart",
                        onclick: move |_| {
                            let sel = handle.selected_ids();
                            let picked: Vec<FindingView> = sel.iter().filter_map(|id| id_map_a.get(id).cloned()).collect();
                            if picked.is_empty() { return; }
                            let mut d = dispositions.peek().clone();
                            for f in &picked { d.entry(finding_key(f)).or_default().state = TriageState::Unresolved; }
                            dispositions.set(d);
                            handle.remove_rows(&sel);
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Moved {} back to Unresolved.", picked.len()));
                        },
                        "Move to Unresolved"
                    }
                    button {
                        class: "btn-restart",
                        onclick: move |_| {
                            let sel = handle.selected_ids();
                            let picked: Vec<FindingView> = sel.iter().filter_map(|id| id_map_b.get(id).cloned()).collect();
                            if picked.is_empty() { return; }
                            let mut d = dispositions.peek().clone();
                            for f in &picked { d.entry(finding_key(f)).or_default().state = TriageState::Ignored; }
                            dispositions.set(d);
                            handle.remove_rows(&sel);
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Moved {} to Ignored.", picked.len()));
                        },
                        "Move to Ignored"
                    }
                },
            }
            button {
                class: "btn-edit-sm",
                onclick: move |_| {
                    let csv = findings_csv(&csv_rows);
                    spawn(async move { let _ = save_csv("camerata-findings.csv", csv).await; });
                },
                "Export CSV"
            }
        }
        Table {
            handle,
            sort_enabled: true,
            filter_enabled: true,
            selection_enabled: true,
            resize_enabled: true,
            // Pin the column header to the top of the table's scroll viewport so it
            // stays visible while scrolling a long findings list.
            sticky_header: true,
            // 0.2.3: an expand-all / collapse-all control in the grouped header (the
            // findings table groups by rule -> file), so a long audit collapses at once.
            group_expand_toggle: true,
            theme: Theme::Dark,
            row_cell_renderers: row_renderers,
            // Critical (security-floor) rows get a red full-row highlight via the 0.2.3
            // conditional row-styling hook — replaces the old per-cell stripe renderer.
            row_class: RowClass::new(|f: &FindingView| {
                (f.severity == "critical").then(|| "finding-row-critical".to_string())
            }),
            on_row_click: Callback::new(move |rid: RowId| {
                if let Some(f) = id_map_click.get(&rid) {
                    detail_finding.set(Some(f.clone()));
                }
            }),
        }
    }
}

/// Which onboarding path the user is setting up.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum OnboardPath {
    /// Install governance into an EXISTING repo (scan → propose → audit → arm).
    Brownfield,
    /// Scaffold a NEW repo with the rules baked in from commit zero.
    Greenfield,
}

/// The repo-onboarding ENTRY POINT: bring a repo new to Camerata under
/// governance. Brownfield (existing repo) and greenfield (new repo) are the two
/// paths. This is distinct from a story's Investigation phase — onboarding sets
/// up the REPO's rules + CI gate; Investigation is per-STORY refinement.
///
/// Connection-gated and honest: the scan/audit/arm engine runs against GitHub, so
/// the actionable steps light up once a GitHub token is connected. Until then it
/// explains exactly what each step will do.
/// The outcome of trying to derive a repo from a navigated-to folder.
pub(super) enum RepoDetect {
    /// The user cancelled the dialog.
    Cancelled,
    /// Derived `owner/repo` AND the local folder it lives in (recorded as the repo's path).
    /// `onboarded_in` names the project that already onboarded this repo, if any (#50): onboarding
    /// is one-time, so the caller blocks a re-onboard and routes the user to the workspace.
    Found {
        repo: String,
        path: String,
        onboarded_in: Option<String>,
    },
    /// Couldn't derive one — carries a human reason for a toast.
    Failed(String),
}

/// Let the user NAVIGATE to a local repo folder; derive its `owner/repo` from the git
/// origin remote (server-side).
pub(super) async fn detect_local_repo() -> RepoDetect {
    let Some(folder) = rfd::AsyncFileDialog::new()
        .set_title("Choose a local repo folder")
        .pick_folder()
        .await
    else {
        return RepoDetect::Cancelled;
    };
    let path = folder.path().to_string_lossy().to_string();
    let resp = match reqwest::Client::new()
        .post(format!("{}/api/git/detect-repo", crate::BFF_URL))
        .json(&serde_json::json!({ "path": path }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return RepoDetect::Failed(format!("couldn't reach the local server ({e})")),
    };
    let v: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return RepoDetect::Failed("unexpected response from the server".to_string()),
    };
    if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        match v.get("repo").and_then(|r| r.as_str()) {
            Some(r) => RepoDetect::Found {
                repo: r.to_string(),
                path,
                onboarded_in: v
                    .get("onboarded_project")
                    .and_then(|p| p.as_str())
                    .map(|s| s.to_string()),
            },
            None => RepoDetect::Failed("no repo in the response".to_string()),
        }
    } else {
        RepoDetect::Failed(
            v.get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("could not detect a repo in that folder")
                .to_string(),
        )
    }
}

/// The result of a greenfield scaffold call, as returned by `POST /api/onboard/greenfield`.
#[derive(Clone, PartialEq, serde::Deserialize)]
pub(super) struct GreenfieldScaffoldResult {
    pub ok: bool,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub files_written: Vec<String>,
    #[serde(default)]
    pub commit_sha: String,
    #[serde(default)]
    pub message: String,
}

/// Resolve the adopted directive for a corpus rule: uses the default option's
/// directive, falling back to the rule title when options are absent or the default
/// is unset. Mirrors the resolve logic in the brownfield apply path.
pub(super) fn resolve_gf_directive(r: &ProposedRuleView) -> String {
    if r.options.is_empty() {
        return r.title.clone();
    }
    r.default_option
        .as_ref()
        .and_then(|oid| r.options.iter().find(|o| &o.id == oid))
        .map(|o| o.directive.clone())
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| r.title.clone())
}

/// Call `POST /api/onboard/greenfield` with the given name, local directory path,
/// and selected arm rules. Resolves each `ProposedRuleView` into an `ArmRuleReq`
/// (directive resolved from the default option). Returns `None` on network failure.
pub(super) async fn scaffold_greenfield_api(
    name: &str,
    dest_path: &str,
    rules: &[ProposedRuleView],
) -> Option<GreenfieldScaffoldResult> {
    // Resolve each corpus rule to its ArmRuleReq shape (id + resolved directive).
    let arm_rules: Vec<ArmRuleReq> = rules
        .iter()
        .filter(|r| r.scope != "cross-repo" && r.scope != "process")
        .map(|r| ArmRuleReq {
            id: r.id.clone(),
            title: r.title.clone(),
            directive: resolve_gf_directive(r),
            option: r.default_option.clone(),
            enforcement: r.enforcement.clone(),
            scope: "repo-local".to_string(),
            repos: vec![name.to_string()],
        })
        .collect();
    reqwest::Client::new()
        .post(format!("{}/api/onboard/greenfield", crate::BFF_URL))
        .json(&serde_json::json!({
            "name": name,
            "path": dest_path,
            "rules": arm_rules,
        }))
        .send()
        .await
        .ok()?
        .json::<GreenfieldScaffoldResult>()
        .await
        .ok()
}

#[component]
pub(super) fn OnboardView(connection: Option<ProviderView>) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut path = use_signal(|| OnboardPath::Brownfield);
    let mut repo = use_signal(String::new);
    // The scan result lives in app-scope context so it survives leaving + returning to this
    // view (a running audit job keeps going server-side; the scan shouldn't vanish).
    let mut scan = use_context::<Signal<Option<ScanReportView>>>();
    let mut scanning = use_signal(|| false);
    let connected = connection.as_ref().map(|c| c.live).unwrap_or(false);

    // Greenfield-specific state: the new repo's name, the local directory to create,
    // the selected corpus rules to bake in, the in-progress flag, and the scaffold result.
    // These signals are mutated inside the GreenfieldForm child component — the parent
    // holds them in scope so they survive path switches.
    let gf_name = use_signal(String::new);
    let gf_path = use_signal(String::new);
    // Set of corpus rule ids the user selected to bake into the new repo.
    let gf_selected_ids = use_signal(|| std::collections::BTreeSet::<String>::new());
    let gf_scaffolding = use_signal(|| false);
    let gf_result = use_signal(|| Option::<GreenfieldScaffoldResult>::None);
    // Load corpus rules for the greenfield picker (the full library, no scan needed).
    let corpus_res = use_resource(fetch_corpus_rules);
    let corpus_rules: Vec<ProposedRuleView> = corpus_res.read().clone().flatten().unwrap_or_default();

    // RESTORE a saved onboarding draft on first mount (issue #27): if there's no live scan
    // but a draft exists on disk, bring its scan back so the architect resumes exactly where
    // they left off — no re-scan. ScanResults then rehydrates its own selection/dispositions/
    // audit from the same draft.
    use_future(move || async move {
        if scan.peek().is_none() {
            if let Some(draft) = load_onboarding_draft().await {
                // Rehydrate the repos textarea too — otherwise it reloads empty (showing the
                // placeholder) even though the scan + rules restored, which reads as "the repos
                // were lost." The repo set is exactly the scanned repos.
                if repo.peek().trim().is_empty() {
                    repo.set(draft.scan.repos.join("\n"));
                }
                scan.set(Some(draft.scan));
            }
        }
    });

    let brownfield_cls = if path() == OnboardPath::Brownfield {
        "onboard-path on"
    } else {
        "onboard-path"
    };
    let greenfield_cls = if path() == OnboardPath::Greenfield {
        "onboard-path on"
    } else {
        "onboard-path"
    };

    // The flow steps differ slightly by path; both are gated on a connection.
    let steps: &[(&str, &str)] = match path() {
        OnboardPath::Brownfield => &[
            ("Point at the repo(s)", "Name the existing owner/repo(s) your token can reach, or browse to a local folder."),
            ("Scan + propose per-repo rules", "Camerata detects each repo's stack and proposes a starter ruleset per repo — you review, you don't author from scratch."),
            ("Pick rules", "Select rules per repo (project-level rules apply to all). Click a rule to read its options and choose an alternative."),
            ("Audit (optional) + triage", "Optionally scan the code against your selected rules + the security floor, then triage findings (Unresolved / Ignored / Tech debt). Not required to finish onboarding."),
            ("Add rules to repo(s)", "Write the governance files onto a camerata/onboard-governance branch in each local clone and push it — no PR (Open governance PR separately). Applying marks the repo onboarded."),
            ("Wire mechanical rules into CI", "The final step: add the selected mechanical rules to each repo's existing CI as enforced lint gates."),
        ],
        OnboardPath::Greenfield => &[
            ("Name the new repo", "Camerata scaffolds it with the rules baked in from commit zero."),
            ("Pick the starter ruleset", "Start from the corpus defaults for your stack; edit and approve."),
            ("Scaffold + arm", "Create the repo with CONVENTIONS.md/AGENTS.md, the CI gate, and the gate config already in place — governed from the first commit."),
        ],
    };

    rsx! {
        div { class: "onboard",
            div { class: "onboard-head",
                p { class: "onboard-title", "Onboard repos into governance" }
                p { class: "onboard-sub", "Bring a repo new to Camerata under the gate. This sets up the REPO's rules and CI enforcement — separate from a story's Investigation phase, which refines one piece of work." }
            }

            // Path chooser.
            div { class: "onboard-paths",
                button {
                    class: "{brownfield_cls}",
                    onclick: move |_| path.set(OnboardPath::Brownfield),
                    span { class: "onboard-path-h", "Brownfield" }
                    span { class: "onboard-path-d", "Install governance into an existing repo." }
                }
                button {
                    class: "{greenfield_cls}",
                    onclick: move |_| path.set(OnboardPath::Greenfield),
                    span { class: "onboard-path-h", "Greenfield" }
                    span { class: "onboard-path-d", "Scaffold a new repo, governed from commit zero." }
                }
            }

            // No GitHub gate: onboarding reads LOCAL code only. A token is only needed LATER
            // (development time) to push the governance branch / open a PR — surface that when
            // it's missing, but never block onboarding on it.
            if !connected {
                div { class: "onboard-note",
                    "Onboarding works on your local repo folders — no GitHub connection needed here. (A token is only needed later, to push the governance branch and open a PR.)"
                }
            }

            // ── Brownfield path: browse existing repos + scan ─────────────────
            if path() == OnboardPath::Brownfield {
                div { class: "onboard-repo-block",
                    label { class: "onboard-repo-label", "Repositories — browse to each repo's local folder (a feature often spans several)" }
                    {
                        let names: Vec<String> = repo()
                            .lines()
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                        rsx! {
                            if names.is_empty() {
                                p { class: "onboard-repos-empty", "No repos yet — browse to a local repo folder to add one." }
                            } else {
                                div { class: "onboard-repos-list",
                                    for name in names {
                                        {
                                            let name_rm = name.clone();
                                            rsx! {
                                                div { class: "onboard-repo-chip", key: "{name}",
                                                    span { class: "onboard-repo-chip-name", "{name}" }
                                                    button {
                                                        class: "onboard-repo-chip-x",
                                                        title: "Remove",
                                                        onclick: move |_| {
                                                            let kept: Vec<String> = repo()
                                                                .lines()
                                                                .map(|s| s.trim().to_string())
                                                                .filter(|s| !s.is_empty() && s != &name_rm)
                                                                .collect();
                                                            repo.set(kept.join("\n"));
                                                        },
                                                        "\u{2715}"
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    button {
                        class: "btn-edit-sm onboard-browse",
                        onclick: move |_| {
                            spawn(async move {
                                match detect_local_repo().await {
                                    RepoDetect::Cancelled => {}
                                    // #50: block re-onboarding a repo that's already onboarded.
                                    RepoDetect::Found { repo: found, onboarded_in: Some(project), .. } => {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, format!("{found} is already onboarded (project \u{201c}{project}\u{201d}). Onboarding is one-time — add it to your workspace to work on it, instead of re-onboarding."));
                                    }
                                    RepoDetect::Found { repo: found, path: folder, .. } => {
                                        let saved = set_repo_path(&found, &folder).await;
                                        let mut cur = repo();
                                        let exists = cur.split([',', '\n']).any(|s| s.trim() == found);
                                        if exists {
                                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("{found} is already in the list."));
                                        } else {
                                            if !cur.trim().is_empty() && !cur.ends_with('\n') {
                                                cur.push('\n');
                                            }
                                            cur.push_str(&found);
                                            repo.set(cur);
                                            if saved {
                                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Added {found} ({folder})"));
                                            } else {
                                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, format!("Added {found}, but couldn't record its local path."));
                                            }
                                        }
                                    }
                                    RepoDetect::Failed(msg) => {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, format!("Couldn't read that folder: {msg}. It must be a local git repo with a GitHub origin remote."));
                                    }
                                }
                            });
                        },
                        "Browse for a local repo folder\u{2026}"
                    }
                    button {
                        class: "onboard-cta",
                        disabled: repo().trim().is_empty() || scanning(),
                        onclick: move |_| {
                            let repos: Vec<String> = repo()
                                .lines()
                                .flat_map(|l| l.split(','))
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                                .collect();
                            if repos.is_empty() { return; }
                            scanning.set(true);
                            spawn(async move {
                                clear_onboarding_draft().await;
                                scan.set(scan_repos(&repos).await);
                                scanning.set(false);
                            });
                        },
                        if scanning() { "Scanning\u{2026}" } else { "Scan repos" }
                    }
                }
            }

            // ── Greenfield path: name + directory + starter ruleset ────────────
            if path() == OnboardPath::Greenfield {
                GreenfieldForm {
                    gf_name,
                    gf_path,
                    gf_selected_ids,
                    gf_scaffolding,
                    gf_result,
                    corpus_rules: corpus_rules.clone(),
                    toasts,
                }
            }

            // Brownfield-only: scan results + flow steps.
            if path() == OnboardPath::Brownfield {
                // Scan results: the audit findings + proposed-rules tables (chorale).
                if let Some(report) = scan() {
                    if report.gated {
                        div { class: "onboard-gate",
                            span { class: "onboard-gate-dot" }
                            div {
                                p { class: "onboard-gate-h", "Scan not run" }
                                p { class: "onboard-gate-b", "{report.message.clone().unwrap_or_default()}" }
                            }
                        }
                    } else {
                        {
                            // Key by the SCAN's identity (repo set + proposed-rule count) so a
                            // RE-SCAN remounts ScanResults/ProposedRulesTable with fresh rows and
                            // a fresh "recommended -> selected" pass.
                            let scan_key = format!(
                                "{}|{}",
                                report.repos.join(","),
                                report.proposed_rules.len()
                            );
                            rsx! { ScanResults { key: "{scan_key}", report } }
                        }
                    }
                }

                // The flow (shown until a scan has run).
                if scan().is_none() {
                    div { class: "onboard-steps",
                        for (i , (h , b)) in steps.iter().enumerate() {
                            div { class: "onboard-step",
                                span { class: "onboard-step-n", "{i + 1}" }
                                div {
                                    p { class: "onboard-step-h", "{h}" }
                                    p { class: "onboard-step-b", "{b}" }
                                }
                            }
                        }
                    }
                }
            }

            // Greenfield: flow steps (shown until scaffolding, handled inside GreenfieldForm).
            if path() == OnboardPath::Greenfield && gf_result().is_none() && !gf_scaffolding() {
                div { class: "onboard-steps",
                    for (i , (h , b)) in steps.iter().enumerate() {
                        div { class: "onboard-step",
                            span { class: "onboard-step-n", "{i + 1}" }
                            div {
                                p { class: "onboard-step-h", "{h}" }
                                p { class: "onboard-step-b", "{b}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Greenfield onboarding form: name the new repo, pick a local directory, select
/// starter rules from the corpus, and scaffold the repo with governance baked in
/// from commit zero.
///
/// Reuses the same arm emit path as brownfield apply — the governance files emitted
/// here are identical in structure to what a brownfield onboarding would write.
#[allow(clippy::too_many_arguments)]
#[component]
pub(super) fn GreenfieldForm(
    mut gf_name: Signal<String>,
    mut gf_path: Signal<String>,
    mut gf_selected_ids: Signal<std::collections::BTreeSet<String>>,
    mut gf_scaffolding: Signal<bool>,
    mut gf_result: Signal<Option<GreenfieldScaffoldResult>>,
    corpus_rules: Vec<ProposedRuleView>,
    toasts: Signal<Vec<crate::toast::Toast>>,
) -> Element {
    let can_scaffold = !gf_name().trim().is_empty() && !gf_path().trim().is_empty();
    // Split corpus into recommended (suggested for the greenfield starter set) and the rest.
    let (recommended, available): (Vec<_>, Vec<_>) =
        corpus_rules.iter().partition(|r| r.recommended);

    rsx! {
        div { class: "gf-form",
            // Step 1: name the repo.
            div { class: "gf-field",
                label { class: "gf-label", "New repo name" }
                input {
                    class: "gf-input",
                    r#type: "text",
                    placeholder: "my-project",
                    value: "{gf_name}",
                    oninput: move |e| {
                        gf_name.set(e.value());
                        // Clear a prior scaffold result when the name changes.
                        gf_result.set(None);
                    },
                }
                p { class: "gf-hint", "Used as the initial commit label. You can connect a GitHub remote later." }
            }

            // Step 2: choose the local directory.
            div { class: "gf-field",
                label { class: "gf-label", "Local directory" }
                div { class: "gf-dir-row",
                    span { class: "gf-dir-path",
                        if gf_path().is_empty() {
                            span { class: "gf-dir-empty", "No directory chosen yet" }
                        } else {
                            "{gf_path()}"
                        }
                    }
                    button {
                        class: "btn-edit-sm",
                        onclick: move |_| {
                            spawn(async move {
                                let Some(folder) = rfd::AsyncFileDialog::new()
                                    .set_title("Choose the PARENT folder — Camerata creates the repo directory inside it")
                                    .pick_folder()
                                    .await
                                else {
                                    return;
                                };
                                let parent = folder.path().to_string_lossy().to_string();
                                let name = gf_name.peek().trim().to_string();
                                let dest = if name.is_empty() {
                                    parent.clone()
                                } else {
                                    format!("{}/{}", parent.trim_end_matches('/'), name)
                                };
                                gf_path.set(dest);
                                gf_result.set(None);
                            });
                        },
                        "Choose parent folder\u{2026}"
                    }
                }
                p { class: "gf-hint", "Camerata creates a new directory here for the repo. The directory must not already exist." }
            }

            // Step 3: starter ruleset picker.
            div { class: "gf-field",
                label { class: "gf-label", "Starter ruleset" }
                p { class: "gf-hint", "Select the rules to bake in from the first commit. Recommended rules are pre-ticked. You can change these after onboarding." }
                if corpus_rules.is_empty() {
                    p { class: "gf-hint", "Loading corpus rules\u{2026}" }
                } else {
                    div { class: "gf-rules-list",
                        // Recommended rules first.
                        if !recommended.is_empty() {
                            p { class: "gf-rules-group-h", "Recommended" }
                            for rule in &recommended {
                                {
                                    let rid = rule.id.clone();
                                    let rid2 = rid.clone();
                                    let checked = gf_selected_ids().contains(&rid);
                                    rsx! {
                                        label { class: "gf-rule-row", key: "{rid}",
                                            input {
                                                r#type: "checkbox",
                                                checked,
                                                onchange: move |_| {
                                                    let mut ids = gf_selected_ids();
                                                    if ids.contains(&rid2) { ids.remove(&rid2); } else { ids.insert(rid2.clone()); }
                                                    gf_selected_ids.set(ids);
                                                },
                                            }
                                            span { class: "gf-rule-id", "{rule.id}" }
                                            span { class: "gf-rule-title", " \u{2014} {rule.title}" }
                                            if !rule.domain.is_empty() && rule.domain != "*" {
                                                span { class: "gf-rule-domain", " [{rule.domain}]" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        // Available (not pre-ticked) rules.
                        if !available.is_empty() {
                            p { class: "gf-rules-group-h", "Available" }
                            for rule in &available {
                                {
                                    let rid = rule.id.clone();
                                    let rid2 = rid.clone();
                                    let checked = gf_selected_ids().contains(&rid);
                                    rsx! {
                                        label { class: "gf-rule-row", key: "{rid}",
                                            input {
                                                r#type: "checkbox",
                                                checked,
                                                onchange: move |_| {
                                                    let mut ids = gf_selected_ids();
                                                    if ids.contains(&rid2) { ids.remove(&rid2); } else { ids.insert(rid2.clone()); }
                                                    gf_selected_ids.set(ids);
                                                },
                                            }
                                            span { class: "gf-rule-id", "{rule.id}" }
                                            span { class: "gf-rule-title", " \u{2014} {rule.title}" }
                                            if !rule.domain.is_empty() && rule.domain != "*" {
                                                span { class: "gf-rule-domain", " [{rule.domain}]" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Step 4: scaffold CTA.
            button {
                class: "onboard-cta",
                disabled: !can_scaffold || gf_scaffolding(),
                onclick: move |_| {
                    let name = gf_name().trim().to_string();
                    let dest = gf_path().trim().to_string();
                    if name.is_empty() {
                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Warning, "Enter a name for the new repo.");
                        return;
                    }
                    if dest.is_empty() {
                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Warning, "Choose a directory for the new repo.");
                        return;
                    }
                    // Resolve the selected corpus rules from the full list.
                    let ids = gf_selected_ids();
                    let selected: Vec<ProposedRuleView> = corpus_rules
                        .iter()
                        .filter(|r| ids.contains(&r.id))
                        .cloned()
                        .collect();
                    gf_scaffolding.set(true);
                    gf_result.set(None);
                    spawn(async move {
                        let result = scaffold_greenfield_api(&name, &dest, &selected).await;
                        gf_scaffolding.set(false);
                        gf_result.set(result);
                    });
                },
                if gf_scaffolding() { "Scaffolding\u{2026}" } else { "Scaffold repo" }
            }

            // Step 5: result.
            if let Some(result) = gf_result() {
                GreenfieldResultView { result }
            }
        }
    }
}

/// Displays the outcome of a greenfield scaffold: success with file list and commit
/// sha, or an error message.
#[component]
pub(super) fn GreenfieldResultView(result: GreenfieldScaffoldResult) -> Element {
    if result.ok {
        rsx! {
            div { class: "gf-result gf-result-ok",
                p { class: "gf-result-h", "Repo scaffolded" }
                p { class: "gf-result-msg", "{result.message}" }
                div { class: "gf-result-files",
                    p { class: "gf-result-files-h", "Files committed:" }
                    ul {
                        for f in &result.files_written {
                            li { class: "gf-result-file", "{f}" }
                        }
                    }
                }
                if !result.commit_sha.is_empty() {
                    p { class: "gf-result-sha", "Initial commit: {result.commit_sha}" }
                }
                p { class: "gf-result-path", "Location: {result.path}" }
                p { class: "gf-result-next",
                    "The repo is governed from the first commit. Connect it to a remote \
                     (e.g. create a GitHub repo and add it as origin) whenever you are ready."
                }
            }
        }
    } else {
        rsx! {
            div { class: "gf-result gf-result-err",
                p { class: "gf-result-h", "Scaffold failed" }
                p { class: "gf-result-msg", "{result.message}" }
            }
        }
    }
}

/// Renders one brownfield scan's results: the audit summary, the findings table,
/// and the proposed-rules table. Keyed by the parent so a new scan remounts the
/// chorale tables with fresh rows.
#[component]
pub(super) fn ScanResults(report: ScanReportView) -> Element {
    // Phase 2 audit result (findings against the selected rules); None until the
    // architect picks rules and runs the audit.
    let mut audit = use_signal(|| Option::<ScanReportView>::None);
    let mut auditing = use_signal(|| false);
    // The model the user picks for the audit — they own the thoroughness/speed trade-off.
    // Company-agnostic list comes from the server (`/api/models`); seed from its default.
    let models_res = use_resource(fetch_audit_models);
    let models = models_res.read().clone().flatten();
    let mut audit_model = use_signal(String::new);
    if audit_model().is_empty() {
        if let Some(m) = &models {
            if !m.default.is_empty() {
                audit_model.set(m.default.clone());
            }
        }
    }
    // Calibration model — its OWN picker (severity recalibration + confidence tagging). A
    // customer can run a cheap scan with a stronger verify, or keep it end-to-end. Defaults
    // to the scan model so "the model you picked" is genuinely used across the board unless
    // the user deliberately splits the tiers.
    let mut calibration_model = use_signal(String::new);
    if calibration_model().is_empty() && !audit_model().is_empty() {
        calibration_model.set(audit_model());
    }
    // Scan mode (user-facing): "parallel" (default), "sequential" (gentle), or "job"
    // (async — submit, walk away, poll). Job uses parallel execution + async delivery.
    // AUTO-SELECTED by the scan's scale; the user can override.
    let recommended_mode = recommend_scan_mode(&report);
    let mut audit_mode = use_signal(|| recommended_mode.clone());
    // Thorough calibration (#51): opt-in, costs more AI. Off by default.
    let mut audit_thorough = use_signal(|| false);
    // Full scan: when ON, ignore the incremental cache and re-audit every file. Off by default
    // (so re-scans are incremental — only changed files cost AI tokens). The first scan of a
    // project is full regardless (no cache yet).
    let mut audit_full_scan = use_signal(|| false);
    // Deep compliance & security tier (#55): opt-in, the most expensive tier.
    // Runs three extra whole-repo passes (SOC-2 gap analysis, deep security audit,
    // threat model) after the standard audit and attaches the results as `report.deep`.
    // Output is ADVISORY — never a SOC-2 report or a penetration test.
    let mut audit_deep = use_signal(|| false);
    // Scan-type selector (Part C): which scans to run. Both default ON (today's behaviour).
    // "AI architectural review" = the LLM scan of architectural/structured/prose rules (and
    // the deep tier). "Deterministic scans" = the always-on security floor + the mechanical
    // preview linters — fast, no LLM, no tokens. Deselecting AI sends `run_ai_review=false`
    // (the server makes zero model calls); deselecting deterministic skips the floor + preview.
    let mut run_ai_review = use_signal(|| true);
    let mut run_deterministic = use_signal(|| true);
    // pw/cockpit-ui Feature 5: feature-flag map. Controls per-feature affordances —
    // SOC-2 section visibility, deep-export scope. Fetched once on mount; degrades
    // gracefully (all flags default to false) when the server is old.
    let feature_flags_res = use_resource(fetch_feature_flags);
    let feature_flags = feature_flags_res
        .read()
        .clone()
        .unwrap_or_default();
    // Live progress for an async job: (passes done, passes total, findings so far).
    let mut job_progress = use_signal(|| Option::<(usize, usize, usize)>::None);
    // Live DETERMINISTIC-pass progress (floor + preview tools), rendered above the AI
    // agent-activity drawer. Primary progress view in deterministic-only mode (no AI drawer).
    let mut det_progress = use_signal(|| Option::<DetProgressView>::None);
    // The in-flight async job id (app-scope, survives navigation). RESUME: if a job was
    // already running when this view (re)mounted, re-attach the poll instead of losing it.
    let active_audit_job = use_context::<Signal<Option<String>>>();
    let scan_idle_ms = use_signal(|| Option::<u128>::None);
    use_future(move || async move {
        if let Some(jid) = active_audit_job.peek().clone() {
            auditing.set(true);
            poll_job(jid, audit, auditing, job_progress, det_progress, active_audit_job, scan_idle_ms).await;
        }
    });
    // Selected-rule count, set by ProposedRulesTable and read here for the cost estimate
    // (the estimate also depends on the model + mode pickers, which live in this component).
    let selected_count = use_signal(|| 0usize);
    use_context_provider(|| selected_count);

    // Per-repo rule selection. For a multi-repo scan the architect views ONE repo's rule
    // table at a time (the single-select below) and each repo keeps its own picks. This
    // lifted `repo -> selected rule ids` map is the source of truth the per-repo tables seed
    // from and write back to, so switching repos preserves each repo's selection and one
    // audit covers every repo against its own rules. Empty for a single-repo scan (the table
    // then behaves as the original whole-set table).
    //
    // PRE-SEED every repo with its recommended rules so a repo the architect never opens
    // still audits against a sensible default set (not just the always-on security floor).
    let repo_seed = {
        let mut m = std::collections::HashMap::<String, Vec<String>>::new();
        if report.repos.len() > 1 {
            for repo in &report.repos {
                let ids: Vec<String> = report
                    .proposed_rules
                    .iter()
                    .filter(|r| r.effective_auto_recommended() && r.repos.iter().any(|rp| rp == repo))
                    .map(|r| r.id.clone())
                    .collect();
                m.insert(repo.clone(), ids);
            }
        }
        m
    };
    let repo_selection = use_signal(|| repo_seed);
    // Shared so the custom-rules panel can auto-select a newly created rule for its repo(s).
    use_context_provider(|| repo_selection);
    // Which repo's rule table is in view. Defaults to the first scanned repo. Provided as
    // context so the rule-detail modal + the table key the per-repo `chosen` map by it.
    let viewed_repo = use_signal(|| report.repos.first().cloned().unwrap_or_default());
    use_context_provider(|| viewed_repo);
    let mut viewed_repo = viewed_repo;
    let multi_repo = report.repos.len() > 1;
    let audited = audit.read().clone();
    let findings: Vec<FindingView> = audited
        .as_ref()
        .map(|a| a.findings.clone())
        .unwrap_or_default();

    // "High severity" stat covers the top two tiers (critical + high) so the exploitable
    // criticals are never invisible in the summary.
    let high = findings
        .iter()
        .filter(|f| f.severity == "critical" || f.severity == "high")
        .count();
    let enforced = findings.iter().filter(|f| f.status == "active").count();
    let suppressed = findings.len().saturating_sub(enforced);

    // The architect's PER-REPO alternative choices, keyed `chosen_key(repo, rule_id)` ->
    // option id. Per-repo so picking an alternative for a rule in one repo doesn't change
    // another repo's choice. Seeded with each rule's default for every scanned repo.
    let chosen = use_signal(|| {
        let mut m = std::collections::HashMap::<String, String>::new();
        for repo in &report.repos {
            for r in &report.proposed_rules {
                if let Some(d) = &r.default_option {
                    m.insert(chosen_key(repo, &r.id), d.clone());
                }
            }
        }
        m
    });
    use_context_provider(|| chosen);

    // User-authored custom rules (Custom + Custom Global), shared via context so the table, the
    // create/edit/delete modal, and the audit/arm closures all read/write the same list. Seeded
    // from the active project's existing custom rules (so re-opening shows them); the draft
    // restore below overlays any in-flight onboarding additions.
    let custom_rules = use_signal(Vec::<CustomRuleView>::new);
    use_context_provider(|| custom_rules);

    // Per-rule repo placement OVERRIDE (rule id -> repos it installs into). Starts EMPTY:
    // an entry exists only when the architect explicitly overrides a rule's target repos.
    // With no entry, arm falls back to the per-repo SELECTION — i.e. the rule installs into
    // exactly the repos whose table checked it (matching what the audit scans). Seeding this
    // with each rule's scan binding (the old behavior) made the override always-present, so
    // arm ignored the per-repo selection and pushed every "available" rule to all repos.
    // (There's no placement-editor UI yet; this map is the seam for one.)
    let placement = use_signal(std::collections::HashMap::<String, Vec<String>>::new);
    use_context_provider(|| placement);

    // The open row-detail rule, shared with ProposedRulesTable. Provided HERE (not in
    // the table) so the modal renders at this subtree's root, outside the chorale
    // table — see RuleDetailModal / ProposedRulesTable for the reopen-bug rationale.
    let detail_rule = use_signal(|| Option::<ProposedRuleView>::None);
    use_context_provider(|| detail_rule);

    // The open finding (row-click) — same host-outside-the-table pattern, shared with
    // FindingsTable. The modal shows the violated rule's directive + the full detail.
    let mut detail_finding = use_signal(|| Option::<FindingView>::None);
    use_context_provider(|| detail_finding);

    // rule id -> what it enforces (the chosen/default alternative's directive, else
    // the rule title), for the findings-table rule-id hover.
    let descriptions: std::collections::HashMap<String, String> = report
        .proposed_rules
        .iter()
        .map(|r| {
            let picked = chosen
                .read()
                .get(&chosen_key(&viewed_repo(), &r.id))
                .cloned()
                .or_else(|| r.default_option.clone());
            let desc = picked
                .and_then(|oid| {
                    r.options
                        .iter()
                        .find(|o| o.id == oid)
                        .map(|o| o.directive.clone())
                })
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| r.title.clone());
            (r.id.clone(), desc)
        })
        .collect();

    let descriptions_modal = descriptions.clone();

    // ── Triage state (issue #26) ──────────────────────────────────────────────
    // Each finding lives in one of three tables: Unresolved (the default), Ignored, or
    // Tech debt. The architect moves findings between them until nothing is Unresolved, then
    // Processes the ignored + tech-debt buckets. State is LOCAL until Process.
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    // The shared scan state (reset to None to "start over") + the cockpit view (so
    // "Complete onboarding" can switch the tab to Governed Development).
    let mut onboard_scan = use_context::<Signal<Option<ScanReportView>>>();
    let mut view = use_context::<Signal<CockpitView>>();
    // When the draft last auto-saved (shown with a check), the two-click "start over"
    // arm, and the in-flight "complete onboarding" flag.
    let mut last_saved = use_signal(|| Option::<String>::None);
    let mut restart_arm = use_signal(|| false);
    let mut finishing = use_signal(|| false);
    let mut dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
    let mut triage_view = use_signal(|| TriageState::Unresolved);
    let mut processing = use_signal(|| false);

    // ── Auto-save / restore the onboarding draft (issue #27) ──────────────────
    // On mount, rehydrate this scan's audit / selection / dispositions / view from the saved
    // draft (only when it's the SAME scan). The `draft_loaded` gate keeps the save effect from
    // overwriting the draft with initial (un-rehydrated) state before the restore runs.
    let mut audit_w = audit;
    let mut repo_selection_w = repo_selection;
    let mut dispositions_w = dispositions;
    let mut chosen_w = chosen;
    let mut custom_rules_w = custom_rules;
    let mut draft_loaded = use_signal(|| false);
    use_context_provider(|| draft_loaded);
    {
        let report_repos = report.repos.clone();
        use_future(move || {
            let report_repos = report_repos.clone();
            async move {
                if let Some(d) = load_onboarding_draft().await {
                    if d.scan.repos == report_repos {
                        if d.audit.is_some() {
                            audit_w.set(d.audit);
                        }
                        if !d.repo_selection.is_empty() {
                            repo_selection_w.set(d.repo_selection);
                        }
                        if !d.chosen.is_empty() {
                            chosen_w.set(d.chosen);
                        }
                        if !d.custom.is_empty() {
                            custom_rules_w.set(d.custom);
                        }
                        if !d.dispositions.is_empty() {
                            dispositions_w.set(d.dispositions);
                        }
                        if !d.viewed_repo.is_empty() {
                            viewed_repo.set(d.viewed_repo);
                        }
                        triage_view.set(d.triage_view);
                    }
                }
                draft_loaded.set(true);
            }
        });
    }
    {
        let report = report.clone();
        use_effect(move || {
            // Track every persisted slice so the effect re-runs on any change.
            let audit_v = audit.read().clone();
            let sel = repo_selection.read().clone();
            let cho = chosen.read().clone();
            let cust = custom_rules.read().clone();
            let disp = dispositions.read().clone();
            let vr = viewed_repo();
            let tv = triage_view();
            if !draft_loaded() {
                return;
            }
            let draft = OnboardingDraft {
                scan: report.clone(),
                audit: audit_v,
                repo_selection: sel,
                chosen: cho,
                custom: cust,
                dispositions: disp,
                viewed_repo: vr,
                triage_view: tv,
            };
            spawn(async move {
                save_onboarding_draft(&draft).await;
                // Stamp the local time so the UI can show "auto-saved at HH:MM:SS".
                last_saved.set(Some(
                    chrono::Local::now().format("%-I:%M:%S %p").to_string(),
                ));
            });
        });
    }

    // Live per-table counts (recompute reactively as dispositions change).
    let (n_unresolved, n_ignored, n_techdebt) = {
        let d = dispositions.read();
        let mut u = 0usize;
        let mut i = 0usize;
        let mut t = 0usize;
        for f in &findings {
            match finding_state(&d, f) {
                TriageState::Unresolved => u += 1,
                TriageState::Ignored => i += 1,
                TriageState::TechDebt => t += 1,
            }
        }
        (u, i, t)
    };

    rsx! {
        // Row-detail modal: hosted here, at the results-subtree root, so it is NOT a
        // sibling of the chorale table (see RuleDetailModal for why).
        RuleDetailModal {}
        // Finding-detail modal: click any findings row to read the violated rule's full
        // directive + the complete explanation the row cell truncates.
        if let Some(f) = detail_finding() {
            {
                let directive = descriptions_modal.get(&f.rule_id).cloned();
                let mut detail_finding = detail_finding;
                rsx! {
                    div { class: "rule-modal-overlay", onclick: move |_| detail_finding.set(None),
                        div { class: "rule-modal", onclick: move |e| e.stop_propagation(),
                            div { class: "rule-modal-head",
                                span { class: "rule-modal-id", "{f.rule_id}" }
                                button { class: "rule-modal-close", onclick: move |_| detail_finding.set(None), "\u{2715}" }
                            }
                            div { class: "rule-modal-meta",
                                span { class: "rule-modal-tag", "severity · {f.severity}" }
                                span { class: "rule-modal-tag", "{f.path}:{f.line}" }
                                span { class: "rule-modal-tag", "{f.status}" }
                                // Authority: deterministic floor (stable, gateable) vs AI-advisory
                                // (model-inferred, id/severity may vary run-to-run).
                                if is_enforced_floor(&f.rule_id) {
                                    span { class: "rule-modal-tag", "enforced · deterministic (stable id)" }
                                } else {
                                    span { class: "rule-modal-tag", "AI · advisory (id may vary run-to-run)" }
                                }
                            }
                            if let Some(d) = directive {
                                p { class: "rule-modal-label", "Rule violated" }
                                p { class: "rule-modal-detail", "{d}" }
                            }
                            p { class: "rule-modal-label", "Finding" }
                            p { class: "rule-modal-title", "{f.snippet}" }
                            p { class: "rule-modal-label", "Explanation" }
                            {
                                // Bold the calibration "[needs review: …]" flag so the reason it
                                // was hedged stands out from the explanation body.
                                let (body, nr) = split_needs_review(&f.detail);
                                rsx! {
                                    p { class: "rule-modal-detail",
                                        "{body}"
                                        if let Some(reason) = nr {
                                            " "
                                            b { class: "nr-inline",
                                                if reason.is_empty() { "[needs review]" } else { "[needs review: {reason}]" }
                                            }
                                        }
                                    }
                                }
                            }
                            if !f.also_matches.is_empty() {
                                p { class: "rule-modal-label", "Also violates at this location" }
                                div { class: "rule-modal-meta",
                                    for rid in f.also_matches.iter() {
                                        span { class: "rule-modal-tag", "{rid}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        div { class: "scan-results",
            if let Some(msg) = report.message.clone() {
                p { class: "scan-note", "{msg}" }
            }
            div { class: "scan-summary",
                span { class: "scan-stat",
                    span { class: "scan-stat-n", "{report.repos.len()}" }
                    " repos"
                }
                span { class: "scan-stat",
                    span { class: "scan-stat-n", "{findings.len()}" }
                    " findings"
                }
                span { class: "scan-stat",
                    span { class: "scan-stat-n high", "{high}" }
                    " high severity"
                }
                span { class: "scan-stat",
                    span { class: "scan-stat-n high", "{enforced}" }
                    " enforced (new)"
                }
                span { class: "scan-stat",
                    span { class: "scan-stat-n", "{suppressed}" }
                    " suppressed (debt/waived)"
                }
                span { class: "scan-stat",
                    span { class: "scan-stat-n", "{report.files_scanned}" }
                    " files scanned"
                }
                if report.files_excluded > 0 {
                    span { class: "scan-stat",
                        span { class: "scan-stat-n", "{report.files_excluded}" }
                        " excluded as noise"
                    }
                }
                if !report.excluded_mechanical_rules.is_empty() {
                    span { class: "scan-stat",
                        title: "{report.excluded_mechanical_rules.join(\", \")}",
                        span { class: "scan-stat-n", "{report.excluded_mechanical_rules.len()}" }
                        " mechanical rule(s) enforced in CI, not scanned"
                    }
                }
            }
            if !report.coverage_notes.is_empty() {
                div { class: "scan-coverage-notes",
                    p { class: "scan-coverage-notes-title", "Scan coverage" }
                    for note in report.coverage_notes.iter() {
                        div { class: "scan-coverage-note",
                            span { class: "scan-coverage-note-tool", "{note.tool}" }
                            span { class: "scan-coverage-note-msg", " — {note.message}" }
                        }
                    }
                }
            }

            // Onboarding status + lifecycle actions. The post-scan steps (audit, triage,
            // apply, wire-CI) are all optional, so "Complete onboarding" is available here.
            div { class: "onboard-actionbar",
                if let Some(ts) = last_saved() {
                    span { class: "onboard-saved", "✓ Auto-saved at {ts}" }
                }
                div { class: "onboard-actionbar-spacer" }
                button {
                    class: "btn-secondary danger",
                    onclick: move |_| {
                        if restart_arm() {
                            spawn(async move {
                                clear_onboarding_draft().await;
                                restart_arm.set(false);
                                onboard_scan.set(None);
                            });
                        } else {
                            restart_arm.set(true);
                        }
                    },
                    if restart_arm() { "Confirm: discard & rescan?" } else { "Start over" }
                }
                button {
                    class: "btn-run",
                    disabled: finishing(),
                    onclick: move |_| {
                        finishing.set(true);
                        spawn(async move {
                            if complete_onboarding().await {
                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, "Onboarding complete. Rules saved to the project.");
                                onboard_scan.set(None);
                                view.set(CockpitView::Stories);
                            } else {
                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Could not complete onboarding.");
                            }
                            finishing.set(false);
                        });
                    },
                    if finishing() { "Finishing…" } else { "Complete onboarding" }
                }
            }

            if !report.stacks.is_empty() {
                div { class: "scan-stacks",
                    for s in report.stacks.iter() {
                        div { class: "scan-stack",
                            span { class: "scan-stack-repo", "{s.repo}" }
                            span { class: "scan-stack-tech",
                                {
                                    let mut tech = s.languages.clone();
                                    tech.extend(s.frameworks.clone());
                                    if tech.is_empty() { "stack not detected".to_string() } else { tech.join(" · ") }
                                }
                            }
                        }
                    }
                }
            }

            // ── Phase 1 result: the proposed ruleset to pick from ──────────────
            p { class: "scan-section-h", "Step 1 — proposed starter ruleset" }
            p { class: "scan-section-sub", "Camerata mapped the stack and proposes these rules. Pick the ones to enforce and choose alternatives; you own the final set (arming generates the governance PR)." }
            p { class: "scan-section-sub", "Click a rule row to read its full context and choose its alternative." }
            // Per-repo view switch (multi-repo only): pick which repo's recommended-rule
            // table to view + select. Each repo keeps its own selection; the audit runs every
            // repo against its own picks.
            if multi_repo {
                div { class: "repo-select",
                    label { class: "repo-select-label", "Repo ruleset:" }
                    select {
                        class: "repo-select-input",
                        value: "{viewed_repo}",
                        onchange: move |e| viewed_repo.set(e.value()),
                        for repo in report.repos.iter() {
                            option { key: "{repo}", value: "{repo}", "{repo}" }
                        }
                    }
                    span { class: "repo-select-hint",
                        "Showing rules for this repo. Each repo has its own selection; the audit scans every repo against its own rules."
                    }
                }
            }
            // Author custom rules (#49) — they appear in the Custom / Custom Global groups below.
            CustomRulesPanel { all_repos: report.repos.clone() }
            {
                let repos_audit = report.repos.clone();
                // Per-repo binding drives RECOMMENDATION (pre-selection), NOT visibility: every
                // repo's table shows the WHOLE rule library so the architect can manually add
                // ANY rule to ANY repo. (Filtering visibility by the repo binding hid rules that
                // were auto-suggested for a sibling repo — e.g. ci-cd suggested for repo A never
                // appeared in repo B's table, so it couldn't be added there at all.) The viewed
                // repo only changes which rules are pre-checked, via the seeded per-repo selection.
                let view_repo = if multi_repo { viewed_repo() } else { String::new() };
                // Merge in the user's custom rules (#49). VISIBLE in this repo's table = corpus
                // rules + Custom Global + this repo's own Custom rules (repo-scoped customs don't
                // leak into sibling repos). The cross-repo `all_rules` lookup gets EVERY custom
                // (all repos) so arm/audit can resolve them. The table is re-keyed on a custom
                // signature so creating/editing/deleting a custom rule remounts it with the change.
                let actual_repo = viewed_repo();
                let (visible_customs, all_customs, custom_sig) = {
                    let cust = custom_rules.read();
                    let all_repos = report.repos.clone();
                    let visible: Vec<ProposedRuleView> = cust
                        .iter()
                        .filter(|c| c.is_global() || c.domain.trim() == actual_repo)
                        .map(|c| c.to_proposed(&all_repos))
                        .collect();
                    let all: Vec<ProposedRuleView> = cust.iter().map(|c| c.to_proposed(&all_repos)).collect();
                    let sig = cust.iter().map(|c| format!("{}\u{1}{}", c.name, c.domain)).collect::<Vec<_>>().join("\u{2}");
                    (visible, all, sig)
                };
                let mut all_rules = report.proposed_rules.clone();
                all_rules.extend(all_customs);
                let mut visible_rules = report.proposed_rules.clone();
                visible_rules.extend(visible_customs);
                rsx! {
                    ProposedRulesTable {
                        // Key on the viewed repo + custom signature so switching repos OR adding/
                        // editing/deleting a custom rule remounts the table with the change.
                        key: "{view_repo}\u{1f}{custom_sig}",
                        rules: visible_rules,
                        all_rules,
                        view_repo,
                        repo_selection,
                        findings: findings.clone(),
                        auditing: auditing(),
                        on_audit: move |rules: Vec<SelectedAuditRule>| {
                            let repos = repos_audit.clone();
                            let model = audit_model();
                            let calib = calibration_model();
                            let mode = audit_mode();
                            let thorough = audit_thorough();
                            let deep = audit_deep();
                            // Scan-type selector: both default true; if a user somehow unticks
                            // both, send both true (the server also coerces — never a no-op).
                            let mut ai = run_ai_review();
                            let mut det = run_deterministic();
                            if !ai && !det { ai = true; det = true; }
                            // Full scan forces a clean pass; otherwise the scan is incremental
                            // (only files changed since the last scan cost AI tokens).
                            let incremental = !audit_full_scan();
                            // Deterministic-only is fast, but its PROGRESS is only pollable on
                            // the async job path (the sync path holds one request and returns
                            // the final report). So route a deterministic-ONLY scan through the
                            // job path regardless of the picked mode — that's where the
                            // per-tool progress streams into the "Deterministic scan" component.
                            let deterministic_only = det && !ai;
                            let use_job = mode == "job" || deterministic_only;
                            // Clear the PREVIOUS run's findings so a re-audit starts from a
                            // blank Findings table instead of showing stale results while
                            // the new audit runs (the server also clears the transcript).
                            // Also reset per-finding triage state, view filter, and the open
                            // detail modal — they're specific to the previous run's findings.
                            // repo_selection / chosen / custom_rules are NOT reset: those are
                            // the architect's persistent rule choices that survive a re-audit.
                            audit.set(None);
                            dispositions.set(std::collections::HashMap::new());
                            triage_view.set(TriageState::Unresolved);
                            detail_finding.set(None);
                            job_progress.set(None);
                            det_progress.set(None);
                            auditing.set(true);
                            if use_job {
                                // Async job: submit, record the id (app-scope, so a later
                                // mount can resume), then poll. The server runs it decoupled
                                // from any single request.
                                let mut active_audit_job = active_audit_job;
                                spawn(async move {
                                    let Some(jid) = audit_job_start(&repos, &rules, &model, &calib, "parallel", thorough, incremental, deep, ai, det).await else {
                                        auditing.set(false);
                                        return;
                                    };
                                    active_audit_job.set(Some(jid.clone()));
                                    poll_job(jid, audit, auditing, job_progress, det_progress, active_audit_job, scan_idle_ms).await;
                                });
                            } else {
                                // Synchronous: hold the request until the (shorter) run finishes.
                                spawn(async move {
                                    audit.set(audit_against(&repos, &rules, &model, &calib, &mode, thorough, incremental, deep, ai, det).await);
                                    auditing.set(false);
                                });
                            }
                        },
                    }
                }
            }

            // ── Phase 2: the audit runs from the table's "Audit selected" button ──
            div { class: "audit-cta",
                p { class: "scan-section-h", "Step 2 — audit the code against your selected rules (optional)" }
                p { class: "scan-section-sub", "The audit is OPTIONAL — you can Apply the rules above and finish onboarding without it. Run it when you want to see existing violations to triage. Tick the rules, then press “Audit code against selected rules”. The deterministic security rules (secrets / raw-SQL / secret-URLs) always run as the enforced floor; the AI checks the code against ONLY your selected rules AND flags anything else worth a look (advisory)." }
                // Model picker — the user owns the speed/thoroughness trade-off. List is
                // company-agnostic, served by /api/models/registry. Options are grouped
                // by provider (Claude first, then OpenRouter); each label has badges
                // (FREE / no-tools / NNNk ctx). Degrades to Claude-only when no OpenRouter
                // key is configured.
                if let Some(m) = models.as_ref() {
                    div { class: "audit-model-row",
                        label { class: "audit-model-label", "Audit model" }
                        select {
                            class: "audit-model-select",
                            disabled: auditing(),
                            value: "{audit_model}",
                            onchange: move |e| audit_model.set(e.value()),
                            for (group_label , opts) in m.grouped().into_iter() {
                                optgroup { label: "{group_label}",
                                    for opt in opts.into_iter() {
                                        option { key: "{opt.id}", value: "{opt.id}", "{opt.label}" }
                                    }
                                }
                            }
                        }
                        span { class: "audit-model-hint", "Faster models finish sooner; stronger models catch more." }
                    }
                    // Calibration model — its OWN tier. The scan finds; calibration
                    // recalibrates severity + tags confidence. Defaults to the scan model
                    // (end-to-end); split it to run a cheap scan with a stronger verify.
                    div { class: "audit-model-row",
                        label { class: "audit-model-label", "Calibration model" }
                        select {
                            class: "audit-model-select",
                            disabled: auditing(),
                            value: "{calibration_model}",
                            onchange: move |e| calibration_model.set(e.value()),
                            for (group_label , opts) in m.grouped().into_iter() {
                                optgroup { label: "{group_label}",
                                    for opt in opts.into_iter() {
                                        option { key: "{opt.id}", value: "{opt.id}", "{opt.label}" }
                                    }
                                }
                            }
                        }
                        span { class: "audit-model-hint", "Recalibrates severity + flags low-confidence findings. Default = the scan model; pick a stronger one for cheap-scan-plus-smart-verify." }
                    }
                }
                // Scan-type selector (Part C) — pick WHICH scans run. Both default ON
                // (today's behaviour). Deterministic-only is fast and uses no LLM / no tokens.
                div { class: "audit-model-row",
                    label { class: "audit-model-label", "What to scan" }
                    div { class: "scan-type-selector",
                        label { class: "audit-thorough-toggle",
                            input {
                                r#type: "checkbox",
                                checked: run_ai_review(),
                                disabled: auditing(),
                                onchange: move |e| run_ai_review.set(e.checked()),
                            }
                            span { "AI architectural review" }
                        }
                        label { class: "audit-thorough-toggle",
                            input {
                                r#type: "checkbox",
                                checked: run_deterministic(),
                                disabled: auditing(),
                                onchange: move |e| run_deterministic.set(e.checked()),
                            }
                            span { "Deterministic scans (floor + linters)" }
                        }
                    }
                    span { class: "audit-model-hint",
                        if run_deterministic() && !run_ai_review() {
                            "Deterministic-only: the always-on security floor + the mechanical preview linters run LOCALLY — fast, no LLM, NO TOKENS. The AI review is skipped entirely (zero model calls). Best for a quick check or QA of the deterministic pass."
                        } else if run_ai_review() && !run_deterministic() {
                            "AI-only: the LLM checks the code against your selected architectural/structured/prose rules. The deterministic floor + preview linters are skipped."
                        } else {
                            "Both run (default): the deterministic floor + linters (free, no tokens) AND the AI architectural review. Untick AI for a fast, token-free deterministic-only scan. At least one must be ticked — unticking both runs both."
                        }
                    }
                }
                // Execution mode — speed/scale knob, separate from the model (quality) and
                // the rule selection (coverage). Parallel is the recommended default.
                div { class: "audit-model-row",
                    label { class: "audit-model-label", "Scan mode" }
                    select {
                        class: "audit-model-select",
                        disabled: auditing(),
                        value: "{audit_mode}",
                        onchange: move |e| audit_mode.set(e.value()),
                        option { value: "parallel", "Parallel" }
                        option { value: "sequential", "Sequential (slower, gentlest)" }
                        option { value: "job", "Background job (walk away)" }
                        option { value: "batch", "Batch (50% off — async, API key required)" }
                    }
                    if audit_mode() == recommended_mode {
                        span { class: "audit-mode-rec", "✓ auto-selected for this scan's size" }
                    }
                    span { class: "audit-model-hint", "Parallel runs rule-batches concurrently. Background job runs server-side so you can leave and watch findings stream in — best for huge / multi-repo scans. Batch uses the Anthropic Message Batches API for a flat 50% discount on all tokens; requires ANTHROPIC_API_KEY and the api backend; results arrive asynchronously (seconds to minutes, up to 24h on very large scans)." }
                }
                // Thorough calibration (#51) — opt-in, costs more AI.
                div { class: "audit-model-row",
                    label { class: "audit-model-label", "Thorough calibration" }
                    label { class: "audit-thorough-toggle",
                        input {
                            r#type: "checkbox",
                            checked: audit_thorough(),
                            disabled: auditing(),
                            onchange: move |e| audit_thorough.set(e.checked()),
                        }
                        span { "Cross-check the calibration (uses more AI)" }
                    }
                    span { class: "audit-model-hint",
                        "Calibration is the step AFTER the scan that recalibrates each finding's severity and flags debatable ones for review — it never drops a finding. Thorough mode runs that pass ~3× and keeps the conservative consensus (so one over-confident pass can't push a debatable architectural preference to HIGH), and judges findings proportionally to the repo's size. Noticeably more AI calls. Optional — the standard single-pass calibration is on by default."
                    }
                }
                // Incremental scan — on by default; re-scans only pay AI for changed files.
                // The checkbox forces a full re-scan over every file.
                div { class: "audit-model-row",
                    label { class: "audit-model-label", "Full scan" }
                    label { class: "audit-thorough-toggle",
                        input {
                            r#type: "checkbox",
                            checked: audit_full_scan(),
                            disabled: auditing(),
                            onchange: move |e| audit_full_scan.set(e.checked()),
                        }
                        span { "Re-scan every file (ignore the incremental cache)" }
                    }
                    span { class: "audit-model-hint",
                        "By default a re-scan is INCREMENTAL: only files whose content changed since the last scan of this project are sent to the AI, and findings for unchanged files are reused from cache — so re-running after a small edit costs a fraction of the tokens. The first scan of a project is always full (no cache yet). Tick this to ignore the cache and re-audit the whole codebase from scratch (e.g. after changing your rule selection, or to refresh every finding)."
                    }
                }
                // Deep compliance & security tier (#55): opt-in, ADVISORY, expensive.
                // Gated on the `soc2` feature flag — hidden entirely when soc2 is disabled
                // (this is the SOC-2-headlined surface; set via .camerata/features.toml or
                // CAMERATA_FEATURE_SOC2=false). The server also skips the lens when off.
                if feature_flags.soc2 && run_ai_review() {
                    div { class: "audit-model-row",
                        label { class: "audit-model-label", "Deep compliance & security (opt-in)" }
                        label { class: "audit-thorough-toggle",
                            input {
                                r#type: "checkbox",
                                checked: audit_deep(),
                                disabled: auditing(),
                                onchange: move |e| audit_deep.set(e.checked()),
                            }
                            span { "Run SOC-2 gap analysis, deep security audit, and threat model" }
                        }
                        span { class: "audit-model-hint deep-tier-warning",
                            "ADVISORY ONLY — not a SOC-2 report and not a penetration test. \
                             Camerata sees static code only; controls that depend on org-level evidence \
                             (HR policies, vendor contracts, access reviews) cannot be assessed from code. \
                             Three extra whole-repo passes run after the standard audit. \
                             This is the MOST EXPENSIVE tier (~3 extra whole-repo passes). \
                             Enable only when you explicitly want compliance gap analysis for this codebase."
                        }
                    }
                }
                // If the architect SKIPS the audit, the post-scan section below (which hosts the
                // CI-wiring story) never renders because `code_chars == 0`. But wiring mechanical
                // rules into CI only needs the SELECTED rules, not a code scan — so offer the
                // CI-story affordance here too, so it's reachable straight after rule selection.
                if report.code_chars == 0 {
                    div { class: "onboard-final-step",
                        span { class: "onboard-step-eyebrow", "Optional: wire CI rules into CI" }
                        CiRulesPanel {
                            repos: report.repos.clone(),
                            rules: ci_rule_items_from_proposed(&report.proposed_rules),
                        }
                        p { class: "section-hint", "You can file the CI-wiring stories (GitHub issues) from your selected rules without running the audit. Optional, and not required to finish onboarding." }
                    }
                }
                // Cost: the pre-audit ESTIMATE for this configuration (model + calibration
                // model + mode + ticked rules), and — once the audit has run — the ACTUAL
                // billed usage beside it, so the estimate is verifiable, not a black box.
                if report.code_chars > 0 {
                    {
                        let price = |id: &str, fallback: (f64, f64)| {
                            models.as_ref()
                                .and_then(|m| m.models.iter().find(|o| o.id == id).map(|o| (o.price_in, o.price_out)))
                                .unwrap_or(fallback)
                        };
                        // When AI scan is off, every model price is zeroed — no LLM calls
                        // means no token spend. The deep tier also requires AI, so it too is
                        // zeroed. This ensures the estimate correctly shows $0 (or "<$0.01")
                        // when the user has selected deterministic-only mode.
                        let ai_on = run_ai_review();
                        let (a_in, a_out) = if ai_on { price(&audit_model(), (3.0, 15.0)) } else { (0.0, 0.0) };
                        let (c_in, c_out) = if ai_on { price(&calibration_model(), (a_in, a_out)) } else { (0.0, 0.0) };
                        let sel = selected_count();
                        // Mirror the request flags the audit will actually send: incremental
                        // scope (only changed files cost tokens unless Full scan is ticked) and
                        // the deep SOC-2/security tier (three extra whole-repo passes).
                        let incremental = !audit_full_scan();
                        // Deep tier only runs with AI; force it off in the estimate when AI is off.
                        let deep = audit_deep() && ai_on;
                        let (toks, dollars, passes) = estimate_audit_cost(report.code_chars, sel, &audit_mode(), a_in, a_out, c_in, c_out, audit_thorough(), incremental, deep);
                        let code_toks = human_tokens((report.code_chars as f64 / 4.0) as u64);
                        let dollar_str = if dollars < 0.01 { "<$0.01".to_string() } else { format!("~${dollars:.2}") };
                        // ACTUAL, once the audit finished and the backend reported usage.
                        let actual = audited.as_ref().and_then(|a| a.actual_usage.clone()).filter(|u| u.calls > 0);
                        rsx! {
                            div { class: "audit-cost",
                                if let Some(u) = actual {
                                    {
                                        let act_toks = u.input_tokens + u.output_tokens;
                                        let act_dollar = if !u.cost_complete { "n/a".to_string() }
                                            else if u.cost_usd < 0.01 { "<$0.01".to_string() }
                                            else { format!("${:.2}", u.cost_usd) };
                                        // Show cache savings line when the API backend ran with
                                        // prompt caching active (cache_read > 0 means the cache
                                        // was hit at least once; creation > 0 means the cache
                                        // was written at least once).
                                        let cache_active = u.cache_read_input_tokens > 0
                                            || u.cache_creation_input_tokens > 0;
                                        rsx! {
                                            div { class: "audit-cost-main",
                                                span { class: "audit-cost-label", "Actual cost" }
                                                span { class: "audit-cost-val", "{act_dollar}" }
                                                span { class: "audit-cost-meta", "{human_tokens(act_toks)} tokens · {u.calls} calls · est. was {dollar_str}" }
                                            }
                                            p { class: "audit-cost-note",
                                                if u.cost_complete {
                                                    "Real billed usage for this run ({human_tokens(u.input_tokens)} in / {human_tokens(u.output_tokens)} out). "
                                                } else {
                                                    "Real token usage shown; a $ figure needs every call to report cost (some didn't, so it's omitted to avoid understating). "
                                                }
                                                if cache_active {
                                                    "Prompt cache: {human_tokens(u.cache_creation_input_tokens)} tok written (1.25x), {human_tokens(u.cache_read_input_tokens)} tok read (0.1x). "
                                                }
                                                "The deterministic security floor ran free. Next time you audit PR diffs — pennies."
                                            }
                                        }
                                    }
                                } else {
                                    div { class: "audit-cost-main",
                                        span { class: "audit-cost-label", "Estimated cost" }
                                        span { class: "audit-cost-val", "{dollar_str}" }
                                        span { class: "audit-cost-meta", "~{human_tokens(toks)} tokens · {passes} pass(es) · {sel} rule(s)" }
                                    }
                                    p { class: "audit-cost-note",
                                        "Approximate, biased high (input + output priced separately; output bills ~5× and dominates findings-heavy scans). "
                                        if incremental {
                                            "Scope: INCREMENTAL — only files changed since the last scan are billed, so the real cost is usually well below this whole-repo figure (priced over ~{code_toks} tokens, {report.files_scanned} files). Tick Full scan to re-audit everything. "
                                        } else {
                                            "Scope: FULL — every file is re-audited (~{code_toks} tokens, {report.files_scanned} files). "
                                        }
                                        "Prompt-caching can make the actual bill lower. "
                                        "The deterministic security floor (secrets / raw-SQL / secret-URLs) runs free. "
                                        "After this, you audit PR diffs — pennies. Cheaper model or Sequential mode lowers this."
                                        if deep {
                                            span { class: "audit-cost-deep-note",
                                                " Deep tier is ON and INCLUDED above: ~3 extra whole-repo passes (SOC-2 gap, deep security, threat model) at the audit model. This is the MOST EXPENSIVE option — it dominates the figure. Untick Deep to drop it."
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                // Live progress for an async job: a determinate bar (it grows as repos are
                // discovered) + a findings-so-far count, so a walk-away scan shows life.
                if let Some((done, total, nf)) = job_progress() {
                    {
                        let pct = (done * 100).checked_div(total).unwrap_or(0).min(100);
                        let n_repos = report.repos.len();
                        rsx! {
                            div { class: "job-progress",
                                div { class: "job-progress-track",
                                    div { class: "job-progress-fill", style: "width: {pct}%" }
                                }
                                span { class: "job-progress-label", "{done}/{total} passes · {nf} finding(s) so far" }
                                // Multi-repo scans run ONE repo at a time, and the pass
                                // denominator grows as each repo is reached — so the agent
                                // activity below shows only the repo currently running, not all
                                // of them. Make the full scope explicit so it doesn't look like
                                // only one repo is being scanned.
                                if n_repos > 1 {
                                    div { class: "job-progress-scope",
                                        span { class: "job-progress-scope-h", "{n_repos} repos in scope, scanned one at a time:" }
                                        for repo in report.repos.iter() {
                                            span { key: "{repo}", class: "job-progress-repo", "{repo}" }
                                        }
                                        span { class: "job-progress-scope-note", "The pass count climbs as each repo is reached; agent activity shows the repo running now." }
                                    }
                                }
                            }
                        }
                    }
                }
                // While the audit runs the background Bombe machine activates via
                // the global loading guard — just show the text label inline.
                if auditing() {
                    div { class: "audit-thinking",
                        span { class: "audit-thinking-label", "Camerata is auditing your code\u{2026}" }
                        button {
                            class: "btn-stop",
                            onclick: move |_| {
                                let jid = active_audit_job().unwrap_or_default();
                                spawn(async move { cancel_audit_job(&jid).await; });
                            },
                            "\u{25a0} Stop scan"
                        }
                    }
                }
                // Scan stall warning: shown when the job has been idle above the threshold.
                // The server sends idle_ms; we show a warning if it's over 2 minutes (120s).
                if let Some(idle) = scan_idle_ms() {
                    if idle > 120_000 && auditing() {
                        div { class: "run-stall-warning scan-stall-warning",
                            div { class: "run-stall-warning-head",
                                span { class: "run-stall-icon", "\u{26a0}" }
                                span { class: "run-stall-title", "No progress for {format_idle(idle)} — scan may be stalled" }
                                button {
                                    class: "btn-stop btn-stop-stall",
                                    onclick: move |_| {
                                        let jid = active_audit_job().unwrap_or_default();
                                        spawn(async move { cancel_audit_job(&jid).await; });
                                    },
                                    "\u{25a0} Stop scan"
                                }
                            }
                        }
                    }
                }
                // Deterministic-scan PROGRESS — rendered ABOVE the AI agent-activity drawer.
                // Shows the deterministic pass's per-tool start/run/done + findings count and
                // an overall done/total. It's the PRIMARY progress view in deterministic-only
                // mode (where the AI drawer below is empty).
                if let Some(dp) = det_progress() {
                    DeterministicProgress { progress: dp }
                }
                // Live feedback: open this to watch the AI's actual prompt + output for
                // the audit (so you can trust it's really working, not hung). Shown ONLY for
                // a current-session audit (running or done THIS mount). The transcript lives
                // server-side, so without this gate a remount (e.g. switching cockpit tabs
                // and back) re-renders the PREVIOUS run's transcript while the findings —
                // which are client state — are gone: a confusing half-restored state. Gating
                // it on the same lifecycle as the findings keeps the two consistent (both
                // present during/after an audit, both absent on a fresh remount).
                if auditing() || audited.is_some() {
                    crate::agent_activity::AgentActivity { run_id: "scan-audit".to_string() }
                }
            }

            // ── Findings (after the audit runs) ────────────────────────────────
            if audited.is_some() {
                p { class: "scan-section-h", "Findings" }
                p { class: "scan-section-sub", "Triage every finding into one of three tables: leave it Unresolved, Ignore it (with a reason), or save it as Tech debt. Switch tables below; selected findings move between tables. When nothing is Unresolved, Process the ignored + tech-debt buckets." }

                // Single-select over the three triage tables, each with a live count.
                div { class: "triage-switch",
                    for st in [TriageState::Unresolved, TriageState::Ignored, TriageState::TechDebt] {
                        {
                            let count = match st { TriageState::Unresolved => n_unresolved, TriageState::Ignored => n_ignored, TriageState::TechDebt => n_techdebt };
                            let active = triage_view() == st;
                            rsx! {
                                button {
                                    key: "{st.label()}",
                                    class: if active { "triage-tab active" } else { "triage-tab" },
                                    onclick: move |_| triage_view.set(st),
                                    "{st.label()} "
                                    span { class: "triage-tab-count", "{count}" }
                                }
                            }
                        }
                    }
                }

                // Wrapped so the key is the first node in its block (Dioxus requirement);
                // keying on the view remounts the table so its frozen rows reflect the switch.
                {
                    rsx! {
                        FindingsTable {
                            key: "{triage_view().label()}",
                            findings: findings.clone(),
                            repos: report.repos.clone(),
                            descriptions: descriptions.clone(),
                            triage_view: triage_view(),
                            dispositions,
                        }
                    }
                }

                // Process: commit the ignored bucket to the baseline and file the tech-debt
                // bucket as tickets. Enabled only once nothing remains Unresolved.
                if triage_view() == TriageState::TechDebt || n_unresolved == 0 {
                    {
                    let findings_for_process = findings.clone();
                    rsx! {
                    div { class: "triage-process",
                        if n_unresolved > 0 {
                            p { class: "section-hint", "Resolve the {n_unresolved} remaining Unresolved finding(s) (Ignore or save as Tech debt) before Processing." }
                        }
                        button {
                            class: "btn-run",
                            disabled: processing() || n_unresolved > 0 || (n_ignored == 0 && n_techdebt == 0),
                            onclick: move |_| {
                                let d = dispositions.read().clone();
                                // Group ignored findings by (repo, reason) -> baseline waiver;
                                // group tech-debt by repo -> a tracked ticket.
                                let mut ignore_groups: std::collections::HashMap<(String, String), Vec<FindingView>> = Default::default();
                                // Tech-debt "resolve later" -> a tracked ticket (GitHub issue), grouped by repo.
                                let mut debt_later: std::collections::HashMap<String, Vec<FindingView>> = Default::default();
                                // Tech-debt "resolve now" -> ALSO a GitHub issue (the story), grouped by repo.
                                // The story makes it into GitHub now (Pillar 1); the dev-engine INGEST of a
                                // resolve-now story is Pillar 2 — same issue, flagged in its title for pickup.
                                let mut debt_now: std::collections::HashMap<String, Vec<FindingView>> = Default::default();
                                for f in &findings_for_process {
                                    let disp = d.get(&finding_key(f)).cloned().unwrap_or_default();
                                    match disp.state {
                                        TriageState::Ignored => {
                                            ignore_groups.entry((f.repo.clone(), disp.reason.clone())).or_default().push(f.clone());
                                        }
                                        TriageState::TechDebt => match disp.bucket {
                                            TechDebtBucket::Later => debt_later.entry(f.repo.clone()).or_default().push(f.clone()),
                                            TechDebtBucket::Now => debt_now.entry(f.repo.clone()).or_default().push(f.clone()),
                                        },
                                        TriageState::Unresolved => {}
                                    }
                                }
                                if ignore_groups.is_empty() && debt_later.is_empty() && debt_now.is_empty() { return; }
                                processing.set(true);
                                spawn(async move {
                                    let mut ok = 0usize;
                                    let mut failed = 0usize;
                                    for ((repo, reason), group) in &ignore_groups {
                                        let r = if reason.trim().is_empty() { "Accepted during onboarding triage".to_string() } else { reason.clone() };
                                        match ignore_findings(repo, group, &r, None).await {
                                            Some(_) => ok += group.len(),
                                            None => failed += group.len(),
                                        }
                                    }
                                    for (repo, group) in &debt_later {
                                        match create_ticket(repo, group, None).await {
                                            Some(_) => ok += group.len(),
                                            None => failed += group.len(),
                                        }
                                    }
                                    // Resolve-now -> a GitHub issue titled so the dev layer (Pillar 2) can pick
                                    // it up for ingest. For Pillar 1 the win is: the story lands in GitHub.
                                    for (repo, group) in &debt_now {
                                        let title = format!("Tech debt (resolve now): {} finding(s) for the dev engine", group.len());
                                        match create_ticket(repo, group, Some(&title)).await {
                                            Some(_) => ok += group.len(),
                                            None => failed += group.len(),
                                        }
                                    }
                                    let msg = format!("Processed {ok} finding(s): ignores → baseline; tech-debt → GitHub issues (resolve-now issues are flagged for the dev engine when Pillar 2 lands).");
                                    if failed == 0 {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, msg);
                                    } else {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Warning, format!("Processed {ok}; {failed} failed (needs GitHub Issues write)."));
                                    }
                                    processing.set(false);
                                });
                            },
                            if processing() { "Processing…" } else { "Process ignored + tech-debt buckets" }
                        }
                    }
                    }
                    }
                }

                // ── Deep compliance & security tier output (#55) ──────────────────────
                // Shown only when the audit ran with deep:true and the server returned the
                // three-lens report. Everything here is ADVISORY — never a SOC-2 report or
                // a penetration test. The disclaimer at the top of each lens makes this explicit.
                //
                // pw/cockpit-ui Feature 5: the SOC-2 lens (soc2-gap) is gated by the `soc2`
                // feature flag. When the flag is off, the SOC-2 section is hidden entirely;
                // deep-security and threat-model still render. The flag state comes from the
                // `feature_flags` map fetched at mount time.
                {
                    let soc2_on = feature_flags.soc2;
                    if let Some(deep) = audited.as_ref().and_then(|a| a.deep.clone()) {
                        rsx! {
                        div { class: "deep-tier-panel",
                            p { class: "deep-tier-heading", "Deep compliance & security tier (ADVISORY)" }
                            // Tier-level disclaimer — surfaced prominently before any findings.
                            if !deep.disclaimer.is_empty() {
                                p { class: "deep-tier-disclaimer", "{deep.disclaimer}" }
                            } else {
                                p { class: "deep-tier-disclaimer",
                                    "ADVISORY ONLY. This output is model-inferred from static code. \
                                     It is not a SOC-2 report, not a certification, and not a penetration test. \
                                     Controls that require organisational evidence (policies, HR, vendor contracts) \
                                     cannot be assessed from code alone. A qualified professional must review and \
                                     validate these findings before any compliance or security claim is made."
                                }
                            }
                            // Feature 5: when soc2 flag is OFF, show a notice that the SOC-2
                            // affordance is disabled for this workspace, but do NOT hide the
                            // deep-security or threat-model sections.
                            if !soc2_on {
                                p { class: "deep-soc2-disabled-notice",
                                    "\u{1F512} SOC-2 gap analysis is disabled for this workspace \
                                     (feature flag \u{2018}soc2\u{2019} is off). \
                                     Deep security and threat model results are shown below."
                                }
                            }
                            for lens in deep.lenses.iter() {
                                {
                                    // Feature 5: skip the soc2-gap lens when the flag is off.
                                    if lens.lens == "soc2-gap" && !soc2_on {
                                        rsx! {}
                                    } else {
                                    let (heading, description) = match lens.lens.as_str() {
                                        "soc2-gap"       => ("SOC-2 Readiness Gap Analysis",
                                                              "Maps the repo's detectable practices against SOC-2 Common Criteria and reports gaps. \
                                                               This is a gap analysis, not a SOC-2 report. \
                                                               Controls needing organisational evidence are marked unknown."),
                                        "deep-security"  => ("Deep Security Audit",
                                                              "Authorization, authentication, sensitive-data handling, and injection paths beyond the \
                                                               deterministic floor. Every finding is advisory — a human must validate each one."),
                                        "threat-model"   => ("Threat Model",
                                                              "Entry points, trust boundaries, sensitive-data paths, and STRIDE-flavoured threats with \
                                                               mitigation directions. Model-inferred from the repo structure."),
                                        other            => (other, ""),
                                    };
                                    let lens = lens.clone();
                                    rsx! {
                                        div { class: "deep-lens", key: "{lens.lens}",
                                            p { class: "deep-lens-heading", "{heading}" }
                                            p { class: "deep-lens-desc", "{description}" }
                                            if !lens.disclaimer.is_empty() {
                                                p { class: "deep-lens-disclaimer", "{lens.disclaimer}" }
                                            }
                                            if !lens.summary.is_empty() {
                                                p { class: "deep-lens-summary", "{lens.summary}" }
                                            }
                                            // SOC-2 gap table (only rendered when soc2 flag is on,
                                            // which is guaranteed by the lens filter above — belt + suspenders).
                                            if !lens.soc2_gaps.is_empty() && soc2_on {
                                                div { class: "soc2-gap-table",
                                                    div { class: "soc2-gap-row header",
                                                        span { class: "soc2-col-ctrl", "Control" }
                                                        span { class: "soc2-col-title", "Title" }
                                                        span { class: "soc2-col-status", "Status" }
                                                        span { class: "soc2-col-obs", "Observed" }
                                                        span { class: "soc2-col-gap", "Gap / Remediation" }
                                                    }
                                                    for (i, gap) in lens.soc2_gaps.iter().enumerate() {
                                                        div { key: "{i}", class: "soc2-gap-row soc2-status-{gap.status}",
                                                            span { class: "soc2-col-ctrl", "{gap.control}" }
                                                            span { class: "soc2-col-title", "{gap.title}" }
                                                            span { class: "soc2-col-status soc2-badge-{gap.status}", "{gap.status}" }
                                                            span { class: "soc2-col-obs", "{gap.observed}" }
                                                            span { class: "soc2-col-gap", "{gap.gap}" }
                                                        }
                                                    }
                                                }
                                            }
                                            // Free-text detail (deep-security + threat-model)
                                            if !lens.detail.is_empty() {
                                                pre { class: "deep-lens-detail", "{lens.detail}" }
                                            }
                                        }
                                    }
                                    }
                                }
                            }

                            // pw/cockpit-ui Feature 4: deep-report export button.
                            // Placed at the bottom of the deep-tier panel so it's visible after
                            // reviewing the findings. Project id comes from the active project.
                            {
                                let pid_export = report.repos.first().cloned().unwrap_or_default();
                                rsx! {
                                    DeepReportExportPanel {
                                        project_id: pid_export,
                                        soc2_enabled: soc2_on,
                                    }
                                }
                            }
                        }
                        }
                    } else {
                        rsx! {}
                    }
                }

                // ── Optional: wire CI rules into CI (#32) ─────────────────────────
                // Files per-tier STORIES (GitHub issues) per repo to add the selected CI-tier
                // rules to that repo's existing CI as enforced lint gates. Mechanical and
                // architectural land as SEPARATE issues — architectural needs team refinement
                // first. This is OPTIONAL — it does NOT gate "onboarded". Use "Complete
                // onboarding" above to finish at any point; the dev layer picks up each CI
                // story independently.
                if n_unresolved == 0 {
                    div { class: "onboard-final-step",
                        span { class: "onboard-step-eyebrow", "Optional: wire CI rules into CI" }
                        CiRulesPanel {
                            repos: report.repos.clone(),
                            rules: ci_rule_items_from_proposed(&report.proposed_rules),
                        }
                        p { class: "section-hint", "Optional, and independent of the tech-debt work above. This files per-tier CI-wiring stories (GitHub issues — one mechanical, one architectural); neither is required to finish onboarding — use \u{201c}Complete onboarding\u{201d} whenever you're ready." }
                    }
                }
            }
        }
    }
}

/// Fetch and return the Markdown deep report for the active project from
/// `GET /api/projects/:id/deep-report`. Returns the Markdown string on success.
/// The `soc2` parameter controls whether the SOC-2 section is included.
pub(super) async fn fetch_deep_report(project_id: &str, include_soc2: bool) -> Option<String> {
    let url = format!(
        "{}/api/projects/{}/deep-report?include_soc2={}",
        crate::BFF_URL,
        project_id,
        include_soc2
    );
    let resp = reqwest::get(url).await.ok()?;
    if resp.status().is_success() {
        resp.text().await.ok()
    } else {
        None
    }
}

/// The deep-report export panel: a single button that calls the export endpoint and
/// shows the resulting Markdown in a scrollable modal. The SOC-2 section is only
/// included when the `soc2` feature flag is on (Feature 5 gate).
///
/// Placed in the Onboard view after the audit findings, below the deep-tier results.
#[component]
pub(super) fn DeepReportExportPanel(project_id: String, soc2_enabled: bool) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut loading = use_signal(|| false);
    let mut report_md: Signal<Option<String>> = use_signal(|| None);

    rsx! {
        div { class: "deep-export-panel",
            p { class: "section-label", "Export deep compliance report" }
            p { class: "section-hint",
                "Downloads the full deep-tier analysis as a Markdown document. \
                 Includes deep security findings and threat model."
                if soc2_enabled {
                    " SOC-2 gap analysis is also included."
                }
                if !soc2_enabled {
                    " SOC-2 gap analysis is disabled for this workspace (feature flag off)."
                }
            }
            button {
                class: "btn-run",
                disabled: loading(),
                onclick: move |_| {
                    let pid = project_id.clone();
                    let include_soc2 = soc2_enabled;
                    loading.set(true);
                    spawn(async move {
                        match fetch_deep_report(&pid, include_soc2).await {
                            Some(md) => report_md.set(Some(md)),
                            None => crate::toast::push_toast(
                                toasts,
                                crate::toast::ToastKind::Error,
                                "Deep report export failed — run an audit with deep tier enabled first.",
                            ),
                        }
                        loading.set(false);
                    });
                },
                if loading() { "Exporting\u{2026}" } else { "Export deep report (Markdown)" }
            }
            if let Some(md) = report_md.read().clone() {
                div { class: "deep-export-modal-overlay",
                    onclick: move |_| report_md.set(None),
                    div { class: "deep-export-modal",
                        onclick: move |e| e.stop_propagation(),
                        div { class: "deep-export-modal-head",
                            p { class: "deep-export-modal-title", "Deep compliance report" }
                            button {
                                class: "rule-modal-close",
                                onclick: move |_| report_md.set(None),
                                "\u{00D7}"
                            }
                        }
                        textarea {
                            class: "deep-export-body",
                            readonly: true,
                            value: "{md}",
                        }
                        button {
                            class: "btn-edit-sm",
                            onclick: move |_| {
                                let md_copy = md.clone();
                                spawn(async move {
                                    let _ = save_csv("camerata-deep-report.md", md_copy).await;
                                });
                            },
                            "Save to file"
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::OnboardingDraft;
    use crate::cockpit::rules::SINGLE_REPO_SELECTION_KEY;

    fn minimal_scan_json() -> &'static str {
        r#"{"files_scanned":0,"findings":[],"proposed_rules":[],"gated":false}"#
    }

    /// Old drafts that predate the `dispositions` or `triage_view` fields must still
    /// deserialize cleanly — every field on OnboardingDraft has `#[serde(default)]`
    /// except `scan`, which is always written. The scan object itself requires
    /// `files_scanned`, `findings`, `proposed_rules`, and `gated` (no `#[serde(default)]`
    /// on those). All other OnboardingDraft fields (`repo_selection`, `chosen`, `custom`,
    /// `dispositions`, `viewed_repo`, `triage_view`, `audit`) default to empty/None.
    #[test]
    fn onboarding_draft_back_compat_missing_optional_fields() {
        // A minimal draft from before dispositions / triage_view / viewed_repo were added.
        let json = format!(r#"{{"scan": {}}}"#, minimal_scan_json());
        let d: OnboardingDraft = serde_json::from_str(&json).expect("back-compat deserialization failed");
        assert!(d.dispositions.is_empty(), "dispositions should default to empty");
        assert!(d.repo_selection.is_empty(), "repo_selection should default to empty");
        assert!(d.chosen.is_empty(), "chosen should default to empty");
        assert!(d.custom.is_empty(), "custom should default to empty");
        assert!(d.audit.is_none(), "audit should default to None");
        assert!(d.viewed_repo.is_empty(), "viewed_repo should default to empty string");
    }

    /// A draft with repo_selection and chosen round-trips through serde without data loss.
    #[test]
    fn onboarding_draft_selection_round_trips() {
        let mut repo_selection = std::collections::HashMap::new();
        repo_selection.insert(
            SINGLE_REPO_SELECTION_KEY.to_string(),
            vec!["rule-a".to_string(), "rule-b".to_string()],
        );
        repo_selection.insert(
            "my-repo".to_string(),
            vec!["rule-c".to_string()],
        );
        let mut chosen = std::collections::HashMap::new();
        chosen.insert("rule-a".to_string(), "opt-2".to_string());

        let json = format!(r#"{{
            "scan": {},
            "repo_selection": {},
            "chosen": {}
        }}"#,
            minimal_scan_json(),
            serde_json::to_string(&repo_selection).unwrap(),
            serde_json::to_string(&chosen).unwrap(),
        );

        let d: OnboardingDraft = serde_json::from_str(&json).expect("round-trip deserialization failed");

        // repo_selection is preserved exactly.
        assert_eq!(
            d.repo_selection.get(SINGLE_REPO_SELECTION_KEY).map(|v| {
                let mut s = v.clone();
                s.sort();
                s
            }),
            Some(vec!["rule-a".to_string(), "rule-b".to_string()]),
            "single-repo sentinel selection must survive round-trip"
        );
        assert_eq!(
            d.repo_selection.get("my-repo").cloned(),
            Some(vec!["rule-c".to_string()]),
            "named-repo selection must survive round-trip"
        );

        // chosen is preserved exactly.
        assert_eq!(
            d.chosen.get("rule-a").cloned(),
            Some("opt-2".to_string()),
            "chosen option must survive round-trip"
        );
    }

    /// Demonstrates the baseline-preservation invariant in pure logic:
    /// the restore path layers user picks on top of suggested defaults —
    /// rules the user never touched stay selected (suggested), and only
    /// rules the user explicitly changed/deselected are overridden.
    ///
    /// This mirrors what the `suggested_ids` derivation in ProposedRulesTable does:
    /// when `repo_selection` has a saved entry for this repo, restore it EXACTLY
    /// (user picks win); when there is no saved entry, fall back to recommended-only.
    /// The "delta layering" is enforced by what gets persisted: the writeback writes
    /// the live effective selection (suggested baseline + user changes) every time the
    /// user ticks a checkbox, so the saved state is always the effective set.
    #[test]
    fn baseline_preservation_effective_selection_merges_correctly() {
        // Simulate: suggested rules A, B, C (all recommended).
        // User deselects B and selects D (non-recommended).
        // After user interaction, effective selection = {A, C, D}.
        // This is what gets saved in repo_selection.
        let suggested: std::collections::HashSet<&str> = ["rule-a", "rule-b", "rule-c"].iter().copied().collect();
        let user_effective: std::collections::HashSet<&str> = ["rule-a", "rule-c", "rule-d"].iter().copied().collect();

        // On save: repo_selection holds the user's effective selection.
        let mut repo_selection = std::collections::HashMap::new();
        repo_selection.insert(
            SINGLE_REPO_SELECTION_KEY.to_string(),
            user_effective.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        );

        // On restore: the table seed logic sees a saved entry and restores it exactly —
        // it does NOT fall back to the suggested baseline (that would lose the user's delta).
        let saved: Option<std::collections::HashSet<String>> = repo_selection
            .get(SINGLE_REPO_SELECTION_KEY)
            .map(|ids| ids.iter().cloned().collect());

        assert!(saved.is_some(), "saved entry must be present after user interaction");
        let restored: std::collections::HashSet<&str> =
            saved.as_ref().unwrap().iter().map(|s| s.as_str()).collect();

        // rule-b: user deselected — must NOT be in the restored set.
        assert!(!restored.contains("rule-b"), "user-deselected rule must not be restored");
        // rule-d: user added (non-recommended) — must be in the restored set.
        assert!(restored.contains("rule-d"), "user-added non-recommended rule must be restored");
        // rule-a, rule-c: user kept (untouched suggestions) — must be in the restored set.
        assert!(restored.contains("rule-a"), "untouched suggested rule must be preserved");
        assert!(restored.contains("rule-c"), "untouched suggested rule must be preserved");
        // The suggested baseline for rule-b is NOT respected once the user has saved a delta.
        assert!(!suggested.is_empty(), "sanity: suggested set is non-empty");
    }

    /// When there is NO saved entry for a repo (fresh first-view), the seed logic
    /// must fall back to the suggested/recommended rules — the user's first view
    /// pre-selects exactly the recommended set, not an empty table.
    #[test]
    fn baseline_fallback_to_recommended_on_fresh_view() {
        // An empty repo_selection (no saved draft yet).
        let repo_selection: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();

        let saved: Option<std::collections::HashSet<String>> = repo_selection
            .get(SINGLE_REPO_SELECTION_KEY)
            .map(|ids| ids.iter().cloned().collect());

        // The `suggested_ids` derivation in ProposedRulesTable uses `None` to fall back
        // to `effective_auto_recommended()` — this assert verifies that branch is taken.
        assert!(
            saved.is_none(),
            "fresh view with no saved entry must trigger recommended fallback"
        );
    }

    // ── model_badge_label ────────────────────────────────────────────────────

    #[test]
    fn badge_free_model_shows_free_not_price() {
        let label = super::model_badge_label("DeepSeek R1", true, true, 64_000, 0.0, false);
        assert!(label.contains("FREE"), "free model must show FREE: {label}");
        assert!(!label.contains('$'), "free model must not show a price: {label}");
    }

    #[test]
    fn badge_paid_model_shows_price_per_million() {
        // $0.55/M output
        let label = super::model_badge_label("DeepSeek R1", false, true, 64_000, 0.55, false);
        assert!(label.contains("$0.55/M"), "paid model must show output price: {label}");
        assert!(!label.contains("FREE"), "paid model must not show FREE: {label}");
    }

    #[test]
    fn badge_round_price_strips_trailing_decimal() {
        // $15.0/M → "$15/M" (no trailing zero)
        let label = super::model_badge_label("Opus", false, true, 200_000, 15.0, false);
        assert!(label.contains("$15/M"), "round price must strip decimal: {label}");
    }

    #[test]
    fn badge_caching_true_shows_cache_tag() {
        let label = super::model_badge_label("DeepSeek R1", false, true, 64_000, 0.55, true);
        assert!(label.contains("cache"), "caching model must show cache tag: {label}");
    }

    #[test]
    fn badge_caching_false_omits_cache_tag() {
        let label = super::model_badge_label("Llama 3.1", false, true, 128_000, 0.10, false);
        assert!(!label.contains("cache"), "non-caching model must not show cache tag: {label}");
    }

    #[test]
    fn badge_full_format_example() {
        // "DeepSeek R1  $0.55/M · tool-use · 64K · cache"
        let label = super::model_badge_label("DeepSeek R1", false, true, 64_000, 0.55, true);
        assert_eq!(label, "DeepSeek R1  $0.55/M · tool-use · 64K · cache");
    }

    #[test]
    fn badge_free_with_cache() {
        let label = super::model_badge_label("DeepSeek R1 Free", true, true, 64_000, 0.0, true);
        assert!(label.contains("FREE"), "free label: {label}");
        assert!(label.contains("cache"), "free+cache label: {label}");
    }
}
