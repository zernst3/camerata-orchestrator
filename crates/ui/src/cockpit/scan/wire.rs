//! Save dialogs, the CSV export, the project ruleset and adoption calls, the scan report view types and the onboarding draft and decision sync.

use super::*;

/// Pop a native save dialog and write `content`. Returns true on success.
pub(in crate::cockpit) async fn save_csv(default_name: &str, content: String) -> bool {
    save_text(default_name, content).await == Some(true)
}

/// [`save_csv`] that tells a cancelled dialog (`None`) from a failed write (`Some(false)`), for
/// callers that report a failure.
pub(in crate::cockpit) async fn save_text(default_name: &str, content: String) -> Option<bool> {
    use camerata_ui::platform::SaveOutcome;
    match camerata_ui::platform::current()
        .save_file(default_name, content.as_bytes())
        .await
    {
        SaveOutcome::Saved => Some(true),
        SaveOutcome::Failed => Some(false),
        SaveOutcome::Cancelled => None,
    }
}

/// Byte-based sibling of `save_csv` — same native-save-dialog idiom, but for raw bytes
/// (the audit-report PDF export; `save_csv`'s `String` param can't carry PDF bytes, which
/// aren't valid UTF-8 in general).
pub(in crate::cockpit) async fn save_bytes(default_name: &str, content: Vec<u8>) -> bool {
    camerata_ui::platform::current()
        .save_file(default_name, &content)
        .await
        == camerata_ui::platform::SaveOutcome::Saved
}

/// Build CSV for the audit findings table.
pub(in crate::cockpit) fn findings_csv(findings: &[FindingView]) -> String {
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

// ── `split_needs_review` IN THE UI: what §9.3 retires, and the two uses that survive ──
//
// §9.3 asks for "the `split_needs_review` prose-parsing path in the UI ... retired entirely
// ONCE THE STRUCTURED FIELD IS UNIVERSAL". It is not universal: `FindingView::confidence` is
// `Option<String>`, and a row serialized before the field existed carries the `[needs review:
// …]` marker and nothing else. So the retirement is partial, and the line it is drawn on is
// the one the principle is actually about — prose must not DECIDE anything:
//
//   RETIRED (this commit): the Queue's hedging column. Its cell value, and therefore its
//   filter vocabulary and its sort key, were derived in part by regex over `f.detail`. That is
//   a UI classifying a row on the strength of a sentence, which is what "structured identity,
//   never prose" forbids. `confidence_tag` reads structured fields only.
//
//   KEPT, deliberately: `DetailPaneRow::from_finding` and the finding modal. Both call it to
//   SPLIT text the producer already wrote into body + marker so the marker can be bolded for a
//   human reader. Nothing is classified, filtered, counted or sorted by the result, and §9.3's
//   own next clause — "the reason text lives in the detail pane" — is satisfied by exactly
//   these two call sites. Deleting them would silently drop a legacy row's only disclosure of
//   its hedge, which is the capability loss this sweep is supposed to avoid, not cause.
//
// When `confidence` becomes universal upstream, the two survivors become deletable and the
// retirement completes. Until then they are the honest half.

// ── The convention commit's two HTTP calls (design §3; owner's Q3/Q4) ─────────
//
// THERE IS NO NEW SERVER SURFACE HERE, and that is deliberate. Both routes already exist and
// already do exactly this; the only thing Pass 2 adds is the curator's decision about WHICH
// rules travel them.
//
//   1. `GET  /api/projects/:id/ruleset`     — `export_project_ruleset`
//   2. `POST /api/projects/:id/ruleset`     — `import_project_ruleset` -> `upsert_base_rules`
//   3. `POST /api/projects/:id/emit-local`  — `emit_project_local` -> `camerata_server::arm`,
//      which renders AGENTS.md / CONVENTIONS.md / the gate config into the repo's local
//      checkout. The cascade is all-false: write the files in place, no branch, no push, no
//      PR. Adoption is a decision about what the standard IS; shipping it is a separate act
//      with its own controls on the Rules screen.
//
// Step 1 exists because step 2 REPLACES `selections` wholesale (`upsert_base_rules` assigns
// the three lists). Posting only the adopted rules would delete every other selection the
// project has. The read-modify-write is therefore not an optimization, it is the contract.

/// Read the project's current base ruleset (`GET /api/projects/:id/ruleset`).
///
/// `None` on a transport/parse failure or an `ok: false` body — the caller must not proceed
/// to the write, because a merge against a default (empty) ruleset would erase the project's
/// selections.
pub(in crate::cockpit) async fn fetch_project_ruleset(
    project_id: &str,
) -> Option<ProjectRulesetView> {
    let v: serde_json::Value = reqwest::get(format!(
        "{}/api/projects/{}/ruleset",
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
    serde_json::from_value(v.get("ruleset")?.clone()).ok()
}

/// Write the merged base ruleset back (`POST /api/projects/:id/ruleset`).
///
/// The body carries no `custom` key, so the server's `merge_custom` sees nothing and every
/// architect-authored custom rule stays exactly where it is. See [`ProjectRulesetView`].
pub(in crate::cockpit) async fn save_project_ruleset(
    project_id: &str,
    ruleset: &ProjectRulesetView,
) -> bool {
    let Ok(resp) = reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/ruleset",
            crate::bff_base(),
            project_id
        ))
        .json(ruleset)
        .send()
        .await
    else {
        return false;
    };
    if !resp.status().is_success() {
        return false;
    }
    resp.json::<serde_json::Value>()
        .await
        .map(|v| v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false))
        .unwrap_or(false)
}

/// COMMIT the curator's adopted rules into one destination repository's `AGENTS.md`.
///
/// Read, merge, write, emit — in that order, and it STOPS at the first failure rather than
/// pressing on. A failed read followed by a write would post a merge built on an empty
/// ruleset, which is the one failure mode here that destroys data rather than merely not
/// doing anything.
///
/// Returns `(ok, one sentence for the toast)`.
pub(in crate::cockpit) async fn commit_adoptions(
    project_id: &str,
    destination_repo: &str,
    adopted: &[AdoptedRule],
) -> (bool, String) {
    if adopted.is_empty() {
        return (false, "Nothing selected to write.".to_string());
    }
    let Some(current) = fetch_project_ruleset(project_id).await else {
        return (
            false,
            "Could not read the project's current ruleset, so nothing was written. \
             (Writing without it would have erased the project's other rule selections.)"
                .to_string(),
        );
    };
    let merged = merge_adoptions(&current, destination_repo, adopted);
    if !save_project_ruleset(project_id, &merged).await {
        return (
            false,
            "Could not save the project's ruleset. Nothing was written to the repository."
                .to_string(),
        );
    }
    let n = adopted.len();
    let (ok, emit_msg) = super::rules::emit_project_local(project_id, false, false, false).await;
    if ok {
        (
            true,
            format!("Wrote {n} into {destination_repo}. {emit_msg}"),
        )
    } else {
        (
            false,
            format!(
                "Saved {n} to the project's ruleset, but writing the governance files failed: \
                 {emit_msg}"
            ),
        )
    }
}

/// Which panel raised the commit dialog, where it is writing, and what has been typed.
///
/// Session state. The typed destination is deliberately NOT persisted anywhere: a half-typed
/// confirmation restored from a draft would be a gate the curator did not pass.
#[derive(Clone, PartialEq, Debug)]
pub(in crate::cockpit) struct CommitDialogState {
    /// `KindSurface::Conventions` or `KindSurface::NoConvention` — which plan to rebuild.
    pub surface: KindSurface,
    /// The destination repository. The string the curator has to type.
    pub destination: String,
    /// What has been typed so far.
    pub typed: String,
}

/// Durable ignore: record the findings as reasoned baseline suppressions (governed PR).
/// Returns the PR URL.
pub(in crate::cockpit) async fn ignore_findings(
    repo: &str,
    findings: &[FindingView],
    reason: &str,
    ticket: Option<String>,
) -> Option<String> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/onboard/ignore", crate::bff_base()))
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
pub(in crate::cockpit) struct ActualUsageView {
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
pub(in crate::cockpit) struct Soc2GapView {
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
pub(in crate::cockpit) struct DeepLensResultView {
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
pub(in crate::cockpit) struct DeepReportView {
    pub lenses: Vec<DeepLensResultView>,
    /// Always `true` — the whole tier is advisory.
    #[serde(default)]
    pub advisory: bool,
    /// Honesty disclaimer for the whole tier.
    #[serde(default)]
    pub disclaimer: String,
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
pub(in crate::cockpit) struct CoverageNoteView {
    pub tool: String,
    pub message: String,
}

/// One audited repo's git identity, mirroring `camerata_server::onboard::AuditedRef` on the
/// wire. Not yet rendered (that's the PDF-export pass); the fields are mirrored now so a
/// server-side addition is never silently dropped by serde here (see the round-trip contract
/// tests at the bottom of this module).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
pub(in crate::cockpit) struct AuditedRefView {
    pub repo: String,
    #[serde(default)]
    pub sha: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub dirty: bool,
    /// The origin remote as observed at scan time (tokenless). Informational.
    #[serde(default)]
    pub origin_url: Option<String>,
    /// The origin's default branch (`main` / `master`), when the clone knew it.
    #[serde(default)]
    pub default_branch: Option<String>,
}

/// The scan's provenance stamp, mirroring `camerata_server::onboard::ScanProvenance` on the
/// wire. Not yet rendered in the cockpit (Pass B / the PDF export reads it); mirrored here so
/// the wire contract stays in sync.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
pub(in crate::cockpit) struct ScanProvenanceView {
    #[serde(default)]
    pub audited_refs: Vec<AuditedRefView>,
    #[serde(default)]
    pub audit_model: Option<String>,
    #[serde(default)]
    pub calibration_model: Option<String>,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub thorough: bool,
    #[serde(default)]
    pub deep: bool,
    #[serde(default)]
    pub rules_fingerprint: String,
    #[serde(default)]
    pub audited_rule_ids: Vec<String>,
    #[serde(default)]
    pub camerata_version: String,
    #[serde(default)]
    pub osv_scanner_version: Option<String>,
    #[serde(default)]
    pub started_at: String,
    #[serde(default)]
    pub finished_at: String,
}

/// One rule's alternative recommendation, as surfaced by the audit-integrated
/// alternative-recommendation feature (docs/design/2026-09-22_audit-integrated-alternatives.md).
/// Mirrors `camerata_server::ai_audit::RuleRecommendation` (minus its own `rule_id` field,
/// since the UI reaches this through `ScanReportView::recommendations`, a map ALREADY keyed by
/// rule id). Present only for multi-option semantic rules; single-option / mechanical /
/// deterministic-floor rules never get an entry.
#[derive(Clone, PartialEq, Default, serde::Deserialize, serde::Serialize)]
pub(in crate::cockpit) struct RuleRecommendationView {
    /// The option id violations were (or will be) reported against.
    #[serde(default)]
    pub recommended_option_id: String,
    /// 1-3 sentences explaining the pick, grounded in the codebase evidence the model saw (or,
    /// for an operator-forced pick, a short fixed note instead of a model explanation).
    #[serde(default)]
    pub recommendation_reasoning: String,
    /// True when the model's raw answer wasn't a real option on this rule (or was
    /// missing/unparseable) and `recommended_option_id` is therefore a FALLBACK, not what the
    /// model actually said. The UI shows a note rather than presenting the fallback as genuine.
    #[serde(default)]
    pub hallucinated: bool,
    /// True when this entry reflects the OPERATOR's explicit choice (via `rescan-alternatives`
    /// or `accept-alternatives`), not the AI's own pick. Drives "your choice" vs "AI
    /// recommended" in the review panel.
    #[serde(default)]
    pub operator_chosen: bool,
    /// The `file:line` (or `file:line — short note`) citation in the repo the model grounded its
    /// pick in (P7, the codebase-inspection hardening notes). `None` when the
    /// model gave no evidence for this rule (or, for an operator-forced pick, no evidence is
    /// expected). Mirrors `camerata_server::ai_audit::RuleRecommendation::evidence`.
    /// `#[serde(default)]` so a report from before this field existed still deserializes as
    /// `None` (no evidence line shown, not an error).
    #[serde(default)]
    pub evidence: Option<String>,
    /// False when the model itself judged this rule NOT applicable to the repo (e.g. a rule
    /// aimed at a stack the codebase doesn't use) — surfaced as "Not applicable to this repo" so
    /// a wrong pick costs a glance rather than reading as a real finding. Mirrors
    /// `camerata_server::ai_audit::RuleRecommendation::applicable`, whose own default is `true`
    /// (`default_recommendation_applicable`): a report from before this field existed carries no
    /// applicability signal at all, and defaulting THAT case to `false` would wrongly flag every
    /// legacy recommendation as not-applicable, so the UI default matches the server's `true`
    /// rather than `bool`'s own `false` default.
    #[serde(default = "default_recommendation_applicable")]
    pub applicable: bool,
}

pub(super) fn default_recommendation_applicable() -> bool {
    true
}

/// A pass or stage that failed or did not run (mirror of the server's `FailedPass`). Before this
/// the desktop app dropped the whole list, so a scan the spend limit cut short, or a pass that
/// failed, looked complete.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default, Debug)]
pub(in crate::cockpit) struct FailedPassView {
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub pass: String,
    #[serde(default)]
    pub reason: String,
    /// Why the part failed (transport, auth_or_billing, content, spend_limit, outage_stop, io).
    /// Empty for a report from before the class existed.
    #[serde(default)]
    pub class: String,
    /// The files the part was shown and did not review.
    #[serde(default)]
    pub files: Vec<String>,
    /// What each attempt got back. Operator only; the cockpit shows it, no export carries it.
    #[serde(default)]
    pub attempts: Vec<AttemptView>,
    /// A disclosed limit of the scope, not a failed part.
    #[serde(default)]
    pub scope_note: bool,
}

/// One attempt at a failed part (mirror of the transport's attempt record).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default, Debug)]
pub(in crate::cockpit) struct AttemptView {
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub backend: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub http_status: Option<u16>,
    #[serde(default)]
    pub error_type: Option<String>,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub retry_after_secs: Option<u64>,
    #[serde(default)]
    pub wait_secs: Option<u64>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub stderr_tail: Option<String>,
    #[serde(default)]
    pub second_try: bool,
    /// Why each reply of the part was turned down (empty when none came back to turn down).
    #[serde(default)]
    pub rejections: Vec<String>,
}

impl AttemptView {
    /// The one-line form shown in the failure panel.
    pub(in crate::cockpit) fn line(&self) -> String {
        let mut out = format!("{} {} {}", self.at, self.backend, self.model);
        if self.second_try {
            out.push_str(" (second try)");
        }
        if let Some(s) = self.http_status {
            out.push_str(&format!(", HTTP {s}"));
        }
        if let Some(t) = &self.error_type {
            out.push_str(&format!(", {t}"));
        }
        if let Some(r) = &self.request_id {
            out.push_str(&format!(", request {r}"));
        }
        if let Some(r) = self.retry_after_secs {
            out.push_str(&format!(", retry-after {r}s"));
        }
        if let Some(w) = self.wait_secs {
            out.push_str(&format!(", waited {w}s"));
        }
        if let Some(c) = self.exit_code {
            out.push_str(&format!(", exit {c}"));
        }
        if !self.message.is_empty() {
            out.push_str(&format!(": {}", self.message));
        }
        if let Some(t) = &self.stderr_tail {
            out.push_str(&format!(" [stderr: {t}]"));
        }
        out
    }
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub(in crate::cockpit) struct ScanReportView {
    /// Every pass or stage that failed or did not run. Non-empty means the scan is INCOMPLETE.
    #[serde(default)]
    pub failed_passes: Vec<FailedPassView>,
    /// The saved run this scan is; "Run what remains" resumes it. `None` for a scan that could
    /// not be saved.
    #[serde(default)]
    pub run_id: Option<String>,
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
    /// Feature B compliance block: the scan was REFUSED (API-only project, no Anthropic key).
    /// Set explicitly by the server (a `BackendResolution::Blocked`) so we render the block
    /// under its own heading rather than inferring it from the report's shape. `serde(default)`
    /// so reports persisted before this field existed still deserialize (as `false`).
    #[serde(default)]
    pub blocked: bool,
    /// Set when the compliance backend gate was `Blocked` for this run but a local pass
    /// (the deterministic floor and/or preview linters / dep-audit) was requested and DID
    /// run anyway — so this report carries real (partial) findings, not a silent empty
    /// table. Distinct from `blocked`: `blocked` means nothing ran at all; this means the
    /// AI review specifically did not run. Mirrors
    /// `camerata_server::onboard::ScanReport::ai_blocked_reason`. `#[serde(default)]` so a
    /// report from before this field existed still deserializes as `None`.
    #[serde(default)]
    pub ai_blocked_reason: Option<String>,
    /// Set when the AI review was requested (not compliance-blocked) but every pass failed
    /// with a real error (bad model id, an API/transport error, an auth failure, …), so the
    /// AI review produced no findings. Carries the first such error's text verbatim. Mirrors
    /// `camerata_server::onboard::ScanReport::ai_error`. `#[serde(default)]` for the same
    /// back-compat reason as `ai_blocked_reason`.
    #[serde(default)]
    pub ai_error: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    /// OPT-IN deep compliance & security tier output (#55). `None` unless the audit
    /// request sent `deep: true`. Everything inside is ADVISORY + model-inferred.
    #[serde(default)]
    pub deep: Option<DeepReportView>,
    /// Coverage notes from the scan preview (tools skipped or unavailable).
    #[serde(default)]
    pub coverage_notes: Vec<CoverageNoteView>,
    /// What was actually audited: git refs, models/mode/rule-selection, tool versions.
    /// Mirrors `camerata_server::onboard::ScanReport::provenance`. Additive
    /// (`#[serde(default)]`) so a report from before this field existed still
    /// deserializes, with an empty/default stamp.
    #[serde(default)]
    pub provenance: ScanProvenanceView,
    /// Per-rule alternative recommendations (audit-integrated alternative recommendation,
    /// docs/design/2026-09-22_audit-integrated-alternatives.md), keyed by rule id. Present for
    /// every multi-option semantic rule that was part of this audit, REGARDLESS of whether it
    /// produced any findings (so the review panel can list zero-violation rules too).
    /// `#[serde(default)]` so a report from before this field existed still deserializes, with
    /// an empty map (no recommendations to show — matches the pre-feature UI exactly).
    #[serde(default)]
    pub recommendations: std::collections::HashMap<String, RuleRecommendationView>,
    /// Where the services of a multi-repo project differ on a rule: an observation, read-only,
    /// shown in its own tab. Mirrors `camerata_server::onboard::ScanReport::
    /// patterns_across_services`; empty (no tab) for a project of one repo.
    #[serde(default)]
    pub patterns_across_services: camerata_ui_core::patterns::PatternsView,
}

pub(in crate::cockpit) async fn scan_repos(repos: &[String]) -> Option<ScanReportView> {
    // Hold a loading guard for the full duration of the scan request so the
    // background Bombe machine runs while the server is scanning.
    let _guard = crate::loading::LoadingGuard::new();
    reqwest::Client::new()
        .post(format!("{}/api/onboard/scan", crate::bff_base()))
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
pub(in crate::cockpit) struct OnboardingDraft {
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
    /// OLD DRAFTS ONLY. The reviewer's decisions live on the server now (the decision store). A
    /// draft saved before that holds them here; the first load after the upgrade migrates them
    /// to the store and the draft is saved blank from then on (it is written empty, never with
    /// the live dispositions, except when the migration failed and they must not be lost).
    #[serde(default)]
    pub dispositions: std::collections::HashMap<String, Disposition>,
    /// The decision store's state as of the last time it could be read, so a load that cannot
    /// reach it still shows what was decided and can queue an undo. `None`: never read.
    #[serde(default)]
    pub last_known: Option<std::collections::HashMap<String, Disposition>>,
    /// When (and on which inspection) each queued change in `dispositions` was decided, so a
    /// change stored late still carries its own run and moment.
    #[serde(default)]
    pub pending_stamps: std::collections::HashMap<String, camerata_ui_core::decisions::Stamp>,
    #[serde(default)]
    pub viewed_repo: String,
    // Read leniently: a view setting a newer build wrote must not fail the whole draft.
    #[serde(default, deserialize_with = "camerata_ui_core::lenient::deserialize")]
    pub triage_view: TriageState,
    /// How the Queue is cut (work order B.4): sticky view state, like `triage_view`. A draft
    /// saved before presets existed reads as the curated order.
    #[serde(default, deserialize_with = "camerata_ui_core::lenient::deserialize")]
    pub order_preset: OrderPreset,
    /// The review timer's saved record: per-group active time and the per-row "reviewed" record
    /// (step 7a). INTERNAL ONLY: it lives in the draft and in the internal stats view and is
    /// never part of an export request (`export_product_zip` sends the dispositions and the
    /// report options only; the server's release-gate tests inspect every client artifact for its
    /// field names). A draft saved before it existed reads as an empty log, and a log of a shape
    /// this build does not know reads as an empty log, so the scan and the dispositions still load.
    #[serde(default)]
    pub review_log: camerata_ui_core::review_timer::SavedReviewLog,
}

/// Load the saved onboarding draft, or None when nothing is in progress.
pub(in crate::cockpit) async fn load_onboarding_draft() -> Option<OnboardingDraft> {
    reqwest::Client::new()
        .get(format!("{}/api/onboard/draft", crate::bff_base()))
        .send()
        .await
        .ok()?
        .json::<Option<OnboardingDraft>>()
        .await
        .ok()
        .flatten()
}

/// The decision store over HTTP: the one place the UI talks to the decision routes.
pub(super) struct HttpDecisions {
    pub(super) project_id: String,
}

impl HttpDecisions {
    fn url(&self, tail: &str) -> String {
        format!(
            "{}/api/projects/{}/decisions{}",
            crate::bff_base(),
            self.project_id,
            tail
        )
    }
}

/// What a refusing response says, in the server's own words when it sent any.
pub(super) async fn refusal_of(
    resp: reqwest::Response,
) -> camerata_ui_core::decision_sync::OpError {
    use camerata_ui_core::decision_sync::OpError;
    let status = resp.status();
    if status.is_server_error() {
        return OpError::Unreachable(format!("the server answered {status}"));
    }
    let message = resp
        .json::<camerata_api_types::decisions::DecisionError>()
        .await
        .map(|e| e.message)
        .unwrap_or_else(|_| format!("the server answered {status}"));
    OpError::Refused {
        status: status.as_u16(),
        message,
    }
}

impl camerata_ui_core::decision_sync::DecisionTransport for HttpDecisions {
    async fn fetch(&self) -> Option<camerata_api_types::decisions::DecisionsResp> {
        let resp = reqwest::Client::new().get(self.url("")).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.json().await.ok()
    }

    async fn migrate(
        &self,
        req: &camerata_api_types::decisions::MigrateDraftReq,
    ) -> Result<(), camerata_ui_core::decision_sync::OpError> {
        use camerata_ui_core::decision_sync::OpError;
        let resp = reqwest::Client::new()
            .post(self.url(""))
            .json(req)
            .send()
            .await
            .map_err(|e| OpError::Unreachable(e.to_string()))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(refusal_of(resp).await)
        }
    }

    async fn post(
        &self,
        req: &camerata_api_types::decisions::NewDecisionReq,
    ) -> Result<String, camerata_ui_core::decision_sync::OpError> {
        use camerata_ui_core::decision_sync::OpError;
        let resp = reqwest::Client::new()
            .post(self.url(""))
            .json(req)
            .send()
            .await
            .map_err(|e| OpError::Unreachable(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(refusal_of(resp).await);
        }
        let done: camerata_api_types::decisions::DecisionRecorded = resp
            .json()
            .await
            .map_err(|e| OpError::Unreachable(e.to_string()))?;
        Ok(done.decision.id)
    }

    async fn revoke(
        &self,
        decision_id: &str,
    ) -> Result<(), camerata_ui_core::decision_sync::OpError> {
        use camerata_ui_core::decision_sync::OpError;
        let resp = reqwest::Client::new()
            .post(self.url(&format!("/{decision_id}/revoke")))
            .send()
            .await
            .map_err(|e| OpError::Unreachable(e.to_string()))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(refusal_of(resp).await)
        }
    }
}

/// What the review screen holds of the decision store: the write-through bridge and whether the
/// store could be read. Provided to the results subtree so the export panel can flush it.
#[derive(Clone)]
pub(super) struct DecisionSync {
    pub(super) bridge: std::rc::Rc<std::cell::RefCell<camerata_ui_core::decisions::DecisionBridge>>,
    pub(super) project_id: Signal<String>,
    /// The sites in words, for an error that names one (`path:line`).
    pub(super) sites: Signal<std::collections::HashMap<String, String>>,
}

impl DecisionSync {
    /// Store what the screen has changed, and say what was refused. Errors are returned as the
    /// text the reviewer reads.
    pub(super) async fn flush(
        &self,
        dispositions: Signal<std::collections::HashMap<String, Disposition>>,
    ) -> camerata_ui_core::decision_sync::SyncOutcome {
        let transport = HttpDecisions {
            project_id: self.project_id.peek().clone(),
        };
        let sites = self.sites;
        camerata_ui_core::decision_sync::sync_decisions(
            &self.bridge,
            &transport,
            || dispositions.peek().clone(),
            |key| {
                sites
                    .peek()
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| key.to_string())
            },
        )
        .await
    }
}

/// Read the decision store into the screen: migrate an old draft's dispositions once the server
/// can, show the decisions with the queued changes over them, and set the banner when the store
/// could not be read. `queue` is the draft's (first load) or the live bridge's (a retry).
pub(super) async fn apply_decision_load(
    queue: camerata_ui_core::decisions::DraftQueue,
    bridge: std::rc::Rc<std::cell::RefCell<camerata_ui_core::decisions::DecisionBridge>>,
    mut dispositions: Signal<std::collections::HashMap<String, Disposition>>,
    mut availability: Signal<Option<camerata_ui_core::decisions::Availability>>,
    mut banner: Signal<Option<&'static str>>,
    mut project_id: Signal<String>,
) {
    let pid = fetch_active_project()
        .await
        .map(|p| p.id)
        .unwrap_or_default();
    project_id.set(pid.clone());
    let loaded = if pid.is_empty() {
        camerata_ui_core::decision_sync::load_decisions(&NoProject, &queue).await
    } else {
        camerata_ui_core::decision_sync::load_decisions(&HttpDecisions { project_id: pid }, &queue)
            .await
    };
    dispositions.set(loaded.shown);
    availability.set(Some(loaded.bridge.availability()));
    banner.set(loaded.banner);
    *bridge.borrow_mut() = loaded.bridge;
}

/// The store when there is no active project: it cannot be reached.
pub(super) struct NoProject;

impl camerata_ui_core::decision_sync::DecisionTransport for NoProject {
    async fn fetch(&self) -> Option<camerata_api_types::decisions::DecisionsResp> {
        None
    }
    async fn migrate(
        &self,
        _: &camerata_api_types::decisions::MigrateDraftReq,
    ) -> Result<(), camerata_ui_core::decision_sync::OpError> {
        Err(camerata_ui_core::decision_sync::OpError::Unreachable(
            "no active project".to_string(),
        ))
    }
    async fn post(
        &self,
        _: &camerata_api_types::decisions::NewDecisionReq,
    ) -> Result<String, camerata_ui_core::decision_sync::OpError> {
        Err(camerata_ui_core::decision_sync::OpError::Unreachable(
            "no active project".to_string(),
        ))
    }
    async fn revoke(&self, _: &str) -> Result<(), camerata_ui_core::decision_sync::OpError> {
        Err(camerata_ui_core::decision_sync::OpError::Unreachable(
            "no active project".to_string(),
        ))
    }
}

/// Persist the current onboarding draft (best-effort; failure is non-fatal).
pub(in crate::cockpit) async fn save_onboarding_draft(draft: &OnboardingDraft) {
    let _ = reqwest::Client::new()
        .post(format!("{}/api/onboard/draft", crate::bff_base()))
        .json(draft)
        .send()
        .await;
}

/// Save the draft through `queue` so saves cannot cross: the server replaces the whole blob on
/// every save, so with two in flight an older draft could land after a newer one and undo a
/// decision. Only one save is out at a time; a draft submitted while one is out replaces any
/// older one waiting, and the newest is sent when the save finishes. `after_each` runs after each
/// completed request. THE one way the draft is auto-saved.
pub(in crate::cockpit) async fn save_onboarding_draft_ordered(
    queue: &std::rc::Rc<
        std::cell::RefCell<camerata_ui_core::save_queue::SaveQueue<OnboardingDraft>>,
    >,
    draft: OnboardingDraft,
    mut after_each: impl FnMut(),
) {
    let first = queue.borrow_mut().submit(draft);
    let Some(mut next) = first else { return };
    loop {
        save_onboarding_draft(&next).await;
        after_each();
        let following = queue.borrow_mut().complete();
        match following {
            Some(newer) => next = newer,
            None => break,
        }
    }
}

/// Drop the saved draft (a fresh scan starts a new session; clearing avoids re-seeding the
/// previous run's audit/dispositions onto it).
pub(in crate::cockpit) async fn clear_onboarding_draft() {
    let _ = reqwest::Client::new()
        .post(format!("{}/api/onboard/draft/clear", crate::bff_base()))
        .send()
        .await;
}

/// Finish onboarding for the active project: marks its repos onboarded and clears the
/// draft. The post-scan steps (audit / triage / apply / wire-CI) are all optional, so this
/// is the explicit "I'm done" action. Returns true on success.
pub(in crate::cockpit) async fn complete_onboarding() -> bool {
    let Ok(resp) = reqwest::Client::new()
        .post(format!("{}/api/onboard/complete", crate::bff_base()))
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
