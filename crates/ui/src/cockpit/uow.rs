use super::*;


/// The branches a UoW can merge FROM (`POST /api/uow/:story_id/branches`), split by
/// where they live. Populates the "Update branch" picker.
#[derive(Clone, Default, PartialEq, serde::Deserialize)]
pub(super) struct MergeSourceBranchesView {
    #[serde(default)]
    pub local: Vec<String>,
    #[serde(default)]
    pub origin: Vec<String>,
}

/// Fetch the mergeable branches for a UoW. Empty lists on any failure / no clone.
pub(super) async fn fetch_uow_branches(story_id: &str) -> MergeSourceBranchesView {
    let resp = reqwest::Client::new()
        .post(format!(
            "{}/api/uow/{}/branches",
            crate::BFF_URL,
            enc_seg(story_id)
        ))
        .send()
        .await;
    match resp {
        Ok(r) => r
            .json::<MergeSourceBranchesView>()
            .await
            .unwrap_or_default(),
        Err(_) => MergeSourceBranchesView::default(),
    }
}

/// Start an AI-assisted update-branch run for a UoW: merge `source_branch` (from
/// `source` = "local"/"origin") INTO the UoW's branch. Returns the run id to poll, or
/// a `Blocked` reason (server 4xx, e.g. no branch yet) surfaced as a toast.
pub(super) async fn start_update_branch_run(
    story_id: &str,
    source_branch: &str,
    source: &str,
    model: &str,
) -> StartRunOutcome {
    let resp = match reqwest::Client::new()
        .post(format!(
            "{}/api/uow/{}/update-branch",
            crate::BFF_URL,
            enc_seg(story_id)
        ))
        .json(&serde_json::json!({
            "source_branch": source_branch,
            "source": source,
            "model": model,
        }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return StartRunOutcome::Failed,
    };
    if resp.status().as_u16() == 400 {
        let reason = resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("error").and_then(|r| r.as_str().map(String::from)))
            .unwrap_or_else(|| "The update-branch request was rejected.".to_string());
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

/// A pull request's state as the BFF returns it (`GET /api/uow/:id/pr` → `pr`).
#[derive(Clone, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub(super) struct PrInfoView {
    #[serde(default)]
    pub number: u64,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub head_branch: String,
    #[serde(default)]
    pub base_branch: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub mergeable: Option<bool>,
}

/// One PR comment (issue or review), normalized.
#[derive(Clone, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub(super) struct PrCommentView {
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub review: bool,
}

/// A PR head commit's CI summary.
#[derive(Clone, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub(super) struct PrChecksView {
    #[serde(default)]
    pub passed: usize,
    #[serde(default)]
    pub failed: usize,
    #[serde(default)]
    pub pending: usize,
    #[serde(default)]
    pub failing: Vec<String>,
}

/// The full `GET /api/uow/:id/pr` payload (graceful: `ok=false` + `pr=null` when no PR).
#[derive(Clone, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub(super) struct PrInfoResult {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub pr: Option<PrInfoView>,
    #[serde(default)]
    pub comments: Vec<PrCommentView>,
    #[serde(default)]
    pub checks: Option<PrChecksView>,
    #[serde(default)]
    pub message: String,
}

/// Pull the PR info for a UoW (state + comments + checks). Always returns a value: the
/// server degrades to `ok=false` rather than erroring, so a network failure maps to the
/// same "no PR" empty payload.
pub(super) async fn fetch_uow_pr(story_id: &str) -> PrInfoResult {
    let resp = reqwest::get(format!("{}/api/uow/{}/pr", crate::BFF_URL, enc_seg(story_id))).await;
    match resp {
        Ok(r) => r.json::<PrInfoResult>().await.unwrap_or_default(),
        Err(_) => PrInfoResult::default(),
    }
}

/// The outcome of opening a PR: the stored number + url, or a reason to toast.
pub(super) enum OpenPrOutcome {
    Opened(u64, String),
    Blocked(String),
    Failed,
}

/// Push the UoW branch + open a PR into `base_branch` (empty → server default branch).
pub(super) async fn open_uow_pr(story_id: &str, base_branch: &str) -> OpenPrOutcome {
    let resp = match reqwest::Client::new()
        .post(format!("{}/api/uow/{}/pr/open", crate::BFF_URL, enc_seg(story_id)))
        .json(&serde_json::json!({ "base_branch": base_branch }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return OpenPrOutcome::Failed,
    };
    if resp.status().as_u16() == 400 {
        let reason = resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("error").and_then(|r| r.as_str().map(String::from)))
            .unwrap_or_else(|| "The open-PR request was rejected.".to_string());
        return OpenPrOutcome::Blocked(reason);
    }
    let Ok(v) = resp.json::<serde_json::Value>().await else {
        return OpenPrOutcome::Failed;
    };
    match v.get("pr_number").and_then(|n| n.as_u64()) {
        Some(n) => OpenPrOutcome::Opened(
            n,
            v.get("pr_url").and_then(|u| u.as_str()).unwrap_or_default().to_string(),
        ),
        None => OpenPrOutcome::Failed,
    }
}

// ── 3-phase cockpit state persistence (#105) ──────────────────────────────────

/// Persist the Intake free-text context for the investigation agent (3-phase doc §3).
/// Fire-and-forget: returns `true` on a 2xx, `false` on any failure (the UI keeps the
/// local value either way).
pub(super) async fn save_intake_context(story_id: &str, context: &str) -> bool {
    reqwest::Client::new()
        .post(format!(
            "{}/api/uow/{}/intake/context",
            crate::BFF_URL,
            enc_seg(story_id)
        ))
        .json(&serde_json::json!({ "context": context }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Persist the per-story repo/branch scope (R6). `repos` is the full in-scope set; each
/// entry is `{ repo, branch }` where `branch` is the tagged `BranchMode` JSON
/// (`{"mode":"existing","branch_name":…}` or `{"mode":"new_from_base","base":…,"new_name":…}`).
/// Fire-and-forget: returns `true` on a 2xx.
pub(super) async fn save_intake_repos(story_id: &str, repos: serde_json::Value) -> bool {
    reqwest::Client::new()
        .post(format!(
            "{}/api/uow/{}/intake/repos",
            crate::BFF_URL,
            enc_seg(story_id)
        ))
        .json(&serde_json::json!({ "repos": repos }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Append one turn (`role` = `"user"` | `"agent"`) to the investigation/refinement agent
/// chat transcript (3-phase doc §4). Returns `true` on a 2xx.
pub(super) async fn append_investigation_chat(story_id: &str, role: &str, text: &str) -> bool {
    reqwest::Client::new()
        .post(format!(
            "{}/api/uow/{}/investigation/chat",
            crate::BFF_URL,
            enc_seg(story_id)
        ))
        .json(&serde_json::json!({ "role": role, "text": text }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Append one turn (`role` = `"user"` | `"agent"`) to the development agent chat
/// transcript (3-phase doc §5). Returns `true` on a 2xx.
pub(super) async fn append_development_chat(story_id: &str, role: &str, text: &str) -> bool {
    reqwest::Client::new()
        .post(format!(
            "{}/api/uow/{}/development/chat",
            crate::BFF_URL,
            enc_seg(story_id)
        ))
        .json(&serde_json::json!({ "role": role, "text": text }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Persist the prose interface contract + the boundary flag (R3.g / §4.6). Returns `true`
/// on a 2xx.
pub(super) async fn save_contract(story_id: &str, contract: &str, crosses_boundary: bool) -> bool {
    reqwest::Client::new()
        .post(format!(
            "{}/api/uow/{}/contract",
            crate::BFF_URL,
            enc_seg(story_id)
        ))
        .json(&serde_json::json!({ "contract": contract, "crosses_boundary": crosses_boundary }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Patch the 3-phase cockpit meta (any subset of viewed phase + finished flags +
/// done/archived). `body` is the JSON object with only the fields to change. Returns
/// `true` on a 2xx.
pub(super) async fn save_meta(story_id: &str, body: serde_json::Value) -> bool {
    reqwest::Client::new()
        .post(format!(
            "{}/api/uow/{}/meta",
            crate::BFF_URL,
            enc_seg(story_id)
        ))
        .json(&body)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Post a comment on the UoW's PR. Returns the created comment url on success.
pub(super) async fn comment_on_uow_pr(story_id: &str, body: &str) -> Option<String> {
    let resp = reqwest::Client::new()
        .post(format!("{}/api/uow/{}/pr/comment", crate::BFF_URL, enc_seg(story_id)))
        .json(&serde_json::json!({ "body": body }))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v = resp.json::<serde_json::Value>().await.ok()?;
    v.get("url").and_then(|u| u.as_str()).map(String::from)
}

/// Start the gated "resolve PR feedback" run for a UoW. Mirrors `start_update_branch_run`.
pub(super) async fn start_pr_resolve_run(story_id: &str, model: &str) -> StartRunOutcome {
    let resp = match reqwest::Client::new()
        .post(format!("{}/api/uow/{}/pr/resolve", crate::BFF_URL, enc_seg(story_id)))
        .json(&serde_json::json!({ "model": model }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return StartRunOutcome::Failed,
    };
    if resp.status().as_u16() == 400 {
        let reason = resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("error").and_then(|r| r.as_str().map(String::from)))
            .unwrap_or_else(|| "The resolve request was rejected.".to_string());
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

/// Fetch the current state of a run.
pub(super) async fn fetch_run(run_id: &str) -> Option<RunView> {
    reqwest::get(format!("{}/api/runs/{}", crate::BFF_URL, run_id))
        .await
        .ok()?
        .json::<RunView>()
        .await
        .ok()
}

/// A run's provenance summary as the BFF reports it (`GET /api/runs/:id/provenance`):
/// the rules in force, the gate deny/allow tallies, and total bounces (issue #21).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub(super) struct RunProvenanceView {
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub story_id: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub rules_in_force: Vec<String>,
    #[serde(default)]
    pub deny_count: usize,
    #[serde(default)]
    pub allow_count: usize,
    #[serde(default)]
    pub total_bounces: usize,
    #[serde(default)]
    pub rules_fired: Vec<String>,
}

/// Fetch the provenance summary for a run.
pub(super) async fn fetch_provenance(run_id: &str) -> Option<RunProvenanceView> {
    reqwest::get(format!("{}/api/runs/{}/provenance", crate::BFF_URL, run_id))
        .await
        .ok()?
        .json::<RunProvenanceView>()
        .await
        .ok()
}

/// Send a stop/cancel request for ANY active run (investigation / dev / update-branch /
/// resolve). Fire-and-forget: 204 = success; any other status or a network error is
/// treated as benign (the run may already be done). The server aborts the driving task,
/// reaping any live agent subprocess, and marks the run `cancelled` (a terminal state the
/// poller ends on).
pub(super) async fn cancel_run(run_id: &str) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/runs/{}/cancel", crate::BFF_URL, run_id))
        .send()
        .await
        .map(|r| r.status() == reqwest::StatusCode::NO_CONTENT)
        .unwrap_or(false)
}

/// Send a cancel request for an audit job. Fire-and-forget; 204 = success.
pub(super) async fn cancel_audit_job(job_id: &str) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/onboard/audit/job/{}/cancel", crate::BFF_URL, job_id))
        .send()
        .await
        .map(|r| r.status() == reqwest::StatusCode::NO_CONTENT)
        .unwrap_or(false)
}

/// The outcome of a sign-off attempt. `Blocked` carries the server's reason for a
/// Critical-finding 409 (issue #53) so the UI can prompt the architect to waive with a
/// justification instead of collapsing it into a dead-end "Could not sign off" toast.
pub(super) enum SignOffOutcome {
    Ok(Box<UowView>),
    /// A Critical scoped-scan finding blocks sign-off until a non-empty `waive_reason`
    /// is supplied. Carries the server's human-readable reason.
    Blocked(String),
    Failed,
}

/// Sign off a run (issue #21). The architect's explicit gate after reviewing the
/// provenance; persists on the story's UoW. `waive_reason`, when `Some` and non-empty,
/// waives a Critical scoped-scan finding (issue #53) that would otherwise 409.
///
/// Maps the response: 2xx → the updated UoW; 409 → `Blocked(reason)` (the architect must
/// supply a waive reason); anything else → `Failed`.
pub(super) async fn sign_off_run(
    run_id: &str,
    by: &str,
    note: Option<&str>,
    waive_reason: Option<&str>,
) -> SignOffOutcome {
    let resp = match reqwest::Client::new()
        .post(format!("{}/api/runs/{}/sign-off", crate::BFF_URL, run_id))
        .json(&serde_json::json!({ "by": by, "note": note, "waive_reason": waive_reason }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return SignOffOutcome::Failed,
    };
    if resp.status().as_u16() == 409 {
        let reason = resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("reason").and_then(|r| r.as_str().map(String::from)))
            .unwrap_or_else(|| {
                "Sign-off is blocked by a Critical security finding. Supply a waiver reason."
                    .to_string()
            });
        return SignOffOutcome::Blocked(reason);
    }
    if !resp.status().is_success() {
        return SignOffOutcome::Failed;
    }
    match resp.json::<UowView>().await.ok() {
        Some(uow) => SignOffOutcome::Ok(Box::new(uow)),
        None => SignOffOutcome::Failed,
    }
}

/// Map a run status string to a label + badge CSS modifier.
pub(super) fn run_status_badge(status: &str) -> (&'static str, &'static str) {
    match status {
        "planned" => ("PLANNED", "neutral"),
        "executing" => ("EXECUTING", "active"),
        "gating" => ("GATING", "active"),
        // Phase 3b: the gated agent raised a clarifying question; the run is parked
        // waiting on a human answer (it resumes when answered).
        "awaiting_clarification" => ("WAITING ON YOU", "warn"),
        "awaiting_qa" => ("AWAITING QA", "warn"),
        "failed" => ("FAILED", "error"),
        "cancelled" => ("CANCELLED", "neutral"),
        _ => ("RUNNING", "active"),
    }
}

/// The dev status of a Unit of Work. Shown alongside the story's tracker status.
/// New = gray, In progress = accent, Done = green.
#[derive(Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize, Default, Debug)]
#[serde(rename_all = "snake_case")]
pub(super) enum DevStatus {
    #[default]
    New,
    InProgress,
    Done,
}

impl DevStatus {
    fn label(self) -> &'static str {
        match self {
            Self::New => "New",
            Self::InProgress => "In progress",
            Self::Done => "Done",
        }
    }

    /// Wire string for `POST /api/uow/:id/status`.
    fn wire_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::InProgress => "in_progress",
            Self::Done => "done",
        }
    }
}

/// The governed-development lifecycle stage of a Unit of Work (Pillar 2). Mirrors
/// `camerata_server::lifecycle::UowStage`; orthogonal to (and richer than) `DevStatus`.
#[derive(Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize, Default, Debug)]
#[serde(rename_all = "snake_case")]
pub(super) enum UowStage {
    #[default]
    Intake,
    Investigating,
    DecisionsApproved,
    Development,
    AwaitingQa,
    SignedOff,
}

impl UowStage {
    /// A short display label for the lifecycle strip.
    fn label(self) -> &'static str {
        match self {
            Self::Intake => "Intake",
            Self::Investigating => "Investigating",
            Self::DecisionsApproved => "Decisions approved",
            Self::Development => "Development",
            Self::AwaitingQa => "Awaiting QA",
            Self::SignedOff => "Signed off",
        }
    }

    /// Monotonic ordinal (0 = Intake .. 5 = SignedOff), for "has reached" comparisons.
    fn ordinal(self) -> usize {
        match self {
            Self::Intake => 0,
            Self::Investigating => 1,
            Self::DecisionsApproved => 2,
            Self::Development => 3,
            Self::AwaitingQa => 4,
            Self::SignedOff => 5,
        }
    }
}

// ── 3-phase cockpit types ─────────────────────────────────────────────────────

/// Which of the three cockpit phases the user has selected to view.
/// Navigation is FREE — selecting only changes the view, never auto-advances.
#[derive(Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize, Default, Debug)]
#[serde(rename_all = "snake_case")]
pub(super) enum PhaseTab {
    #[default]
    Intake,
    Investigation,
    Development,
}

impl PhaseTab {
    fn label(self) -> &'static str {
        match self {
            Self::Intake => "Intake",
            Self::Investigation => "Investigation & Refinement",
            Self::Development => "Development",
        }
    }
}

/// Per-UoW metadata for the 3-phase cockpit shell.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
pub(super) struct UowMeta {
    /// Informational UoW status (mirrors stage, informational only).
    #[serde(default)]
    pub status_label: String,
    /// Which phase the user last selected to view.
    #[serde(default)]
    pub viewed_phase: PhaseTab,
    /// Phase finish flags.
    #[serde(default)]
    pub intake_finished: bool,
    #[serde(default)]
    pub investigation_finished: bool,
    #[serde(default)]
    pub development_finished: bool,
    /// Done/archived flag (replaces SignedOff as a terminal state).
    #[serde(default)]
    pub done: bool,
}

/// Maps the old `UowStage` to the 3-phase `PhaseTab` for migration / initialisation.
///
/// - `Intake` → Intake
/// - `Investigating`, `DecisionsApproved` → Investigation
/// - `Development`, `AwaitingQa` → Development
/// - `SignedOff` → Development (+ caller should set `meta.done = true`)
pub(super) fn stage_to_phase(stage: UowStage) -> PhaseTab {
    match stage {
        UowStage::Intake => PhaseTab::Intake,
        UowStage::Investigating | UowStage::DecisionsApproved => PhaseTab::Investigation,
        UowStage::Development | UowStage::AwaitingQa | UowStage::SignedOff => {
            PhaseTab::Development
        }
    }
}

/// Returns a short, informational display label for the UoW's current lifecycle stage.
/// This is purely informational — it never drives control flow.
pub(super) fn stage_to_status_label(stage: UowStage) -> &'static str {
    match stage {
        UowStage::Intake => "Intake",
        UowStage::Investigating => "Investigating",
        UowStage::DecisionsApproved => "Decisions Approved",
        UowStage::Development => "In Development",
        UowStage::AwaitingQa => "Awaiting QA",
        UowStage::SignedOff => "Signed Off",
    }
}

// ── End 3-phase cockpit types ─────────────────────────────────────────────────

/// A single entry in the AI development history.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub(super) struct HistoryEntryView {
    pub ts: String,
    pub kind: String,
    pub text: String,
}

/// The frozen gate provenance stamped onto a UoW after a governed run finishes.
/// Mirrors `camerata_server::uow::GateProvenance`.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub(super) struct GateProvenanceView {
    pub run_id: String,
    pub mode: String,
    pub allow_count: usize,
    pub deny_count: usize,
    pub total_bounces: usize,
    #[serde(default)]
    pub rules_fired: Vec<String>,
    #[serde(default)]
    pub recorded: String,
}

/// An architect's sign-off on a story's governed run (issue #21).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub(super) struct SignOffView {
    pub ts: String,
    pub by: String,
    pub run_id: String,
    #[serde(default)]
    pub note: Option<String>,
}

/// One turn in a per-phase agent chat transcript (mirrors `camerata_server::uow::ChatTurn`).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
pub(super) struct ChatTurnView {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub text: String,
}

/// One in-scope repo + its branch mode (mirrors `camerata_server::uow::RepoScope`). The
/// `branch` field is the tagged-enum `BranchMode` JSON, kept raw so the UI can read either
/// variant without a sum-type round-trip.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub(super) struct RepoScopeView {
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub branch: serde_json::Value,
}

/// The persisted Intake state (mirrors `camerata_server::uow::IntakeState`).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
pub(super) struct IntakeStateView {
    #[serde(default)]
    pub context: String,
    #[serde(default)]
    pub repos: Vec<RepoScopeView>,
}

/// The persisted Investigation state (mirrors `camerata_server::uow::InvestigationState`).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
pub(super) struct InvestigationStateView {
    #[serde(default)]
    pub chat: Vec<ChatTurnView>,
    #[serde(default)]
    pub contract: String,
    #[serde(default)]
    pub crosses_boundary: bool,
}

/// The persisted Development state (mirrors `camerata_server::uow::DevelopmentState`).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
pub(super) struct DevelopmentStateView {
    #[serde(default)]
    pub chat: Vec<ChatTurnView>,
}

/// The Unit of Work as returned by `GET /api/uow/:story_id`.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub(super) struct UowView {
    pub story_id: String,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub dev_status: DevStatus,
    /// The governed-development lifecycle stage (Pillar 2).
    #[serde(default)]
    pub stage: UowStage,
    #[serde(default)]
    pub history: Vec<HistoryEntryView>,
    /// The frozen gate provenance from the most recent completed run, if any.
    #[serde(default)]
    pub gate_provenance: Option<GateProvenanceView>,
    #[serde(default)]
    pub sign_off: Option<SignOffView>,
    /// The persisted Intake state (#105): free-text context + repo/branch scope (R6).
    #[serde(default)]
    pub intake: IntakeStateView,
    /// The persisted Investigation state (#105): refinement chat + prose contract (R3.g).
    #[serde(default)]
    pub investigation: InvestigationStateView,
    /// The persisted Development state (#105): dev-agent chat transcript.
    #[serde(default)]
    pub development: DevelopmentStateView,
    /// The 3-phase cockpit meta (#105): viewed phase, finished flags, done/archived.
    #[serde(default)]
    pub meta: UowMeta,
    #[serde(default)]
    pub updated: String,
}

/// Fetch the UoW for a single story (get-or-create semantics).
pub(super) async fn fetch_uow(story_id: &str) -> Option<UowView> {
    reqwest::get(format!("{}/api/uow/{}", crate::BFF_URL, enc_seg(story_id)))
        .await
        .ok()?
        .json::<UowView>()
        .await
        .ok()
}

/// POST a new dev-status for a story's UoW. Returns `Some(())` on a 2xx. The server
/// responds with the full `UnitOfWork` (not the UI's `UowView`), so we DO NOT try to
/// deserialize the body — the caller just bumps the refresh tick and re-fetches the UoW.
/// (Deserializing into `UowView` here was the bug behind a false "Could not update dev
/// status" toast even when the server succeeded.)
pub(super) async fn post_uow_status(story_id: &str, status: DevStatus) -> Option<()> {
    let resp = reqwest::Client::new()
        .post(format!("{}/api/uow/{}/status", crate::BFF_URL, enc_seg(story_id)))
        .json(&serde_json::json!({ "status": status.wire_str() }))
        .send()
        .await
        .ok()?;
    resp.status().is_success().then_some(())
}

/// The outcome of a lifecycle transition POST. `Ok` carries the updated UoW; `Blocked`
/// carries the server's human-readable reason (a 409); `Failed` is a transport error.
pub(super) enum TransitionOutcome {
    /// The transition succeeded; the panel re-fetches the updated UoW via the refresh
    /// tick, so the updated body is not carried here.
    Ok,
    Blocked(String),
    Failed,
}

/// POST a lifecycle transition (`begin-investigation` / `approve-decisions`) and map the
/// response: 2xx → the updated UoW, 409 → the block reason, anything else → Failed.
pub(super) async fn post_uow_transition(story_id: &str, action: &str) -> TransitionOutcome {
    let url = format!("{}/api/uow/{}/{}", crate::BFF_URL, enc_seg(story_id), action);
    let resp = match reqwest::Client::new().post(url).send().await {
        Ok(r) => r,
        Err(_) => return TransitionOutcome::Failed,
    };
    if resp.status().is_success() {
        TransitionOutcome::Ok
    } else if resp.status().as_u16() == 409 {
        // The server returns { "reason": "<why>" } for a blocked transition.
        let reason = resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("reason").and_then(|r| r.as_str().map(String::from)))
            .unwrap_or_else(|| "Transition blocked.".to_string());
        TransitionOutcome::Blocked(reason)
    } else {
        TransitionOutcome::Failed
    }
}

/// A normalized work item from any tracker provider (`POST /api/workitems/pull`,
/// `POST /api/workitems/refresh`). The server maps a provider's native issue (today:
/// the worktracker GitHub adapter's `CanonicalStory`) into this shape so the UI never
/// touches a provider-specific payload.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Debug, Default)]
pub(super) struct WorkItem {
    /// Stable cross-provider id, e.g. `"github:OWNER/REPO#123"`. The dedup key for UoWs.
    pub id: String,
    /// The provider that owns this item (today always `"github"`).
    #[serde(default)]
    pub provider: String,
    /// `OWNER/REPO` the item belongs to. Each pulled item carries its own repo.
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub number: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// `"open"` | `"closed"`.
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub labels: Vec<String>,
    /// The parent issue number when this item is a GitHub sub-issue (Epic → child).
    /// `None` for top-level or standalone issues. Populated from the server's
    /// `IssueSummary::parent_number` on a pull.
    #[serde(default)]
    pub parent_number: Option<u64>,
}

/// A work item augmented with N-level hierarchy grouping columns for the Chorale table.
///
/// `hierarchy_cols[L]` holds the group label for depth level L (0 = root ancestor).
/// Chorale's multi-column grouping produces genuinely nested subgroups when
/// `set_grouping(vec![ColumnId("lvl0"), ColumnId("lvl1"), ...])` is called.
///
/// Derivation (see `build_work_item_rows` / `ancestor_path`):
///   - Level 0 = the root ancestor's label (`"#N: <title>"`), or `"(no parent)"` for
///     standalone issues.
///   - Level 1..max_depth = intermediate / direct-parent labels.
///   - If an item is shallower than the table's max depth, the deepest column repeats
///     the item's own level-0 label so it renders directly under its parent group
///     rather than floating in an empty subgroup.
///
/// This is a VIEW-layer concern; nothing here is stored on the server `WorkItem`.
#[derive(Clone, PartialEq, Debug)]
pub(super) struct WorkItemRow {
    pub work_item: WorkItem,
    /// Per-depth group labels for Chorale: `hierarchy_cols[0]` = root level,
    /// `hierarchy_cols[1]` = one level down, and so on. Length = `max_depth + 1`
    /// across the whole table (padded with the item's own deepest label for shallow items).
    pub hierarchy_cols: Vec<String>,
}

/// Walk `parent_number` links within `by_number` and return the chain of ancestor
/// issue numbers from the **root down to (but not including) `item` itself**.
///
/// The walk stops when:
/// - `parent_number` is `None` (root reached), or
/// - the parent number is absent from `by_number` (parent not pulled / filtered), or
/// - 8 hops have been taken (GitHub's native sub-issue depth ceiling).
///
/// A visited-number set guards against malformed cycles: if the same number
/// is seen twice the walk stops immediately, treating the last known node as root.
///
/// Returns root-first. An item with no parent returns an empty `Vec`.
pub(super) fn ancestor_path(by_number: &std::collections::HashMap<u64, &WorkItem>, item: &WorkItem) -> Vec<u64> {
    let mut path: Vec<u64> = Vec::with_capacity(8);
    let mut visited: std::collections::HashSet<u64> = std::collections::HashSet::new();
    visited.insert(item.number);
    let mut current = item;
    // Walk upward; cap at 8 hops (GitHub sub-issue depth ceiling).
    for _ in 0..8 {
        let Some(pn) = current.parent_number else { break };
        if !visited.insert(pn) {
            // Cycle detected — stop.
            break;
        }
        path.push(pn);
        let Some(parent) = by_number.get(&pn) else { break };
        current = parent;
    }
    path.reverse(); // root-first
    path
}

/// Format a `WorkItem` reference as a group-header label: `"#N: <title>"`.
pub(super) fn issue_group_label(item: &WorkItem) -> String {
    format!("#{}: {}", item.number, item.title)
}

/// Build the table rows for the work-item table, computing per-depth hierarchy
/// columns from the full item list. Pure (no I/O) so it is unit-testable.
///
/// Grouping uses one Chorale column per depth level (`lvl0`, `lvl1`, …) so that
/// `set_grouping(vec![ColumnId("lvl0"), ColumnId("lvl1"), …])` produces genuinely
/// nested subgroups at arbitrary depth (GitHub caps at 8).
///
/// Column derivation per item:
/// - `lvl0` = root ancestor label (`"#N: <title>"`), or `"(no parent)"` if the
///   item has no ancestor in the pulled set (standalone or orphan).
/// - `lvl1..lvlK` = intermediate / direct-parent labels.
/// - Items shallower than `max_depth` repeat their deepest label in the remaining
///   columns so Chorale places them directly under their parent group.
///
/// Also returns the `max_depth` discovered across all items (0 = all standalone).
pub(super) fn build_work_item_rows(items: &[WorkItem]) -> Vec<WorkItemRow> {
    use std::collections::HashMap;
    // O(1) parent-title lookups.
    let by_number: HashMap<u64, &WorkItem> =
        items.iter().map(|it| (it.number, it)).collect();

    // Phase 1: compute each item's ancestor path and its own label.
    struct ItemMeta<'a> {
        item: &'a WorkItem,
        ancestors: Vec<u64>, // root-first ancestor numbers
    }
    let metas: Vec<ItemMeta<'_>> = items
        .iter()
        .map(|it| ItemMeta {
            item: it,
            ancestors: ancestor_path(&by_number, it),
        })
        .collect();

    // Phase 2: determine max ancestor depth (root=0, child=1, grandchild=2, …).
    let max_depth = metas.iter().map(|m| m.ancestors.len()).max().unwrap_or(0);
    // One grouping tier per real ancestor level; at least one so the table always has
    // a group column (the "(no parent)" bucket when everything is standalone). Using
    // `max_depth` (not `max_depth + 1`) is what stops a flat epic→children tree from
    // rendering an extra phantom tier.
    let tiers = max_depth.max(1);

    // Issues that are themselves the parent of something in the pulled set.
    let parents: std::collections::HashSet<u64> =
        items.iter().filter_map(|it| it.parent_number).collect();

    // Phase 3: build rows — `hierarchy_cols` length = `tiers` for every row.
    metas
        .into_iter()
        .map(|m| {
            let depth = m.ancestors.len(); // 0 for root / standalone
            let mut cols: Vec<String> = Vec::with_capacity(tiers);
            if depth == 0 {
                // Root parent, standalone, or orphan (its parent wasn't pulled).
                let label = if parents.contains(&m.item.number) || m.item.parent_number.is_some()
                {
                    issue_group_label(m.item)
                } else {
                    "(no parent)".to_string()
                };
                for _ in 0..tiers {
                    cols.push(label.clone());
                }
            } else {
                // One column per ancestor (root-first).
                for ancestor_num in &m.ancestors {
                    let label = by_number
                        .get(ancestor_num)
                        .copied()
                        .map(issue_group_label)
                        .unwrap_or_else(|| format!("#{ancestor_num}: (not pulled)"));
                    cols.push(label);
                }
                // Pad shallow items to `tiers`. A PARENT repeats its OWN label so its
                // descendants nest under it (it heads its own subgroup). A LEAF repeats
                // its DIRECT PARENT's label (the last ancestor) so it stays a ROW in the
                // parent's group instead of forming a phantom one-item subgroup named
                // after itself — the bug that made every leaf look like its own child.
                let pad = if parents.contains(&m.item.number) {
                    issue_group_label(m.item)
                } else {
                    cols.last().cloned().unwrap_or_else(|| issue_group_label(m.item))
                };
                while cols.len() < tiers {
                    cols.push(pad.clone());
                }
                cols.truncate(tiers);
            }
            WorkItemRow { work_item: m.item.clone(), hierarchy_cols: cols }
        })
        .collect()
}

/// Render a compact issue-spine section for the chat system prompt (Layer 3b).
/// Produces a depth-indented tree (two spaces per level) so the model understands
/// the full N-level hierarchy. Capped at 200 issues to keep the prompt bounded.
/// Pure (no I/O).
///
/// Rendering order: roots first (recursively with their subtrees), then items
/// whose parent was not pulled (orphan children), then standalone issues.
/// Depth indentation: 2 spaces per ancestor level.
pub(super) fn render_pulled_issues_for_chat(items: &[WorkItem]) -> String {
    if items.is_empty() {
        return String::new();
    }
    use std::collections::HashMap;

    let by_number: HashMap<u64, &WorkItem> =
        items.iter().map(|it| (it.number, it)).collect();
    // Map each parent number to its direct children (sorted for stable output).
    let mut children_of: HashMap<u64, Vec<&WorkItem>> = HashMap::new();
    for it in items {
        if let Some(pn) = it.parent_number {
            children_of.entry(pn).or_default().push(it);
        }
    }
    for v in children_of.values_mut() {
        v.sort_by_key(|it| it.number);
    }

    let mut s = String::new();
    s.push_str(&format!("{} issue(s):\n", items.len().min(200)));
    let mut count = 0usize;

    // Recursively render an item and all of its descendants.
    fn render_item(
        item: &WorkItem,
        depth: usize,
        children_of: &HashMap<u64, Vec<&WorkItem>>,
        s: &mut String,
        count: &mut usize,
    ) {
        if *count >= 200 { return; }
        *count += 1;
        let indent = "  ".repeat(depth);
        s.push_str(&format!(
            "{}- #{} [{}]: {}\n",
            indent, item.number, item.state, item.title
        ));
        if let Some(kids) = children_of.get(&item.number) {
            for kid in kids {
                render_item(kid, depth + 1, children_of, s, count);
            }
        }
    }

    // Roots: items with no parent in the pulled set.
    let roots: Vec<&WorkItem> = {
        let mut v: Vec<&WorkItem> = items
            .iter()
            .filter(|it| it.parent_number.map_or(true, |pn| !by_number.contains_key(&pn))
                && it.parent_number.is_none())
            .collect();
        v.sort_by_key(|it| it.number);
        v
    };
    for root in &roots {
        render_item(root, 0, &children_of, &mut s, &mut count);
    }

    // Orphan children: parent was not pulled.
    let orphans: Vec<&WorkItem> = {
        let mut v: Vec<&WorkItem> = items
            .iter()
            .filter(|it| {
                it.parent_number
                    .map(|pn| !by_number.contains_key(&pn))
                    .unwrap_or(false)
            })
            .collect();
        v.sort_by_key(|it| it.number);
        v
    };
    for orphan in &orphans {
        // Render as a subtree from the orphan; its own children are still renderable.
        render_item(orphan, 0, &children_of, &mut s, &mut count);
    }

    s
}

/// App-lifetime cache of the last work-item pull, keyed by project id (so switching
/// projects never shows stale items). A `GlobalSignal` persists for the lifetime of the
/// process, so navigating away from Governed Development and back does NOT require a
/// re-pull — the pull is held in memory until Camerata closes or the user pulls again.
/// Manual pull only; there is no auto-poll.
pub(super) static PULLED_WORK_ITEMS: GlobalSignal<Option<(String, Vec<WorkItem>)>> =
    Signal::global(|| None);

/// Returns the pre-rendered issue spine for the chat system prompt (Layer 3b), reading
/// from the app-lifetime `PULLED_WORK_ITEMS` cache. Returns `None` when no pull has
/// happened this session (the chat caller omits the layer entirely in that case).
/// Called from `main.rs` to pass the section into `ChatBubble` without exposing the
/// `PULLED_WORK_ITEMS` signal or `WorkItem` type outside this module.
pub(crate) fn pulled_issues_chat_section() -> Option<String> {
    let guard = PULLED_WORK_ITEMS.read();
    let (_, items) = guard.as_ref()?;
    let section = render_pulled_issues_for_chat(items);
    if section.is_empty() {
        None
    } else {
        Some(section)
    }
}

/// The `POST /api/workitems/pull` envelope.
#[derive(Clone, PartialEq, serde::Deserialize, Default)]
pub(super) struct PullWorkItemsResult {
    #[serde(default)]
    pub items: Vec<WorkItem>,
}

/// One Unit of Work as `GET /api/uows` reports it: the UoW id, the WorkItem it
/// references, and its lifecycle stage. The `id` doubles as the key the existing
/// governed-dev endpoints are keyed by (the server reconciles UoW id ↔ story id), so
/// the reused dev controls below address this UoW through it.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Debug)]
pub(super) struct UowListEntry {
    pub id: String,
    /// The work item this UoW references, or `None` for a blank/authoring DRAFT UoW that
    /// has not been published to the board yet.
    #[serde(default)]
    pub work_item: Option<WorkItem>,
    #[serde(default)]
    pub stage: UowStage,
    /// `true` when this is a blank/authoring DRAFT UoW (render the authoring panel).
    #[serde(default)]
    pub authoring: bool,
}

/// The `GET /api/uows` envelope.
#[derive(Clone, PartialEq, serde::Deserialize, Default)]
pub(super) struct UowsResult {
    #[serde(default)]
    pub uows: Vec<UowListEntry>,
}

/// The `POST /api/uow/from-workitem` result. `created=false` means a UoW already
/// existed for that work item (dedup by external ref) and was returned as-is.
#[derive(Clone, PartialEq, serde::Deserialize, Default)]
pub(super) struct FromWorkItemResult {
    #[serde(default)]
    pub uow_id: String,
    #[serde(default)]
    pub created: bool,
}

/// One comment on a work item (`POST /api/workitems/comments`), flattened for the
/// modal. Mirrors the server's `IssueComment` shape.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Debug, Default)]
pub(super) struct WorkItemComment {
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub body: String,
    /// ISO-8601 created-at as the tracker returns it; the UI shows it as-is.
    #[serde(default)]
    pub created_at: String,
}

/// The `POST /api/workitems/comments` envelope.
#[derive(Clone, PartialEq, serde::Deserialize, Default)]
pub(super) struct WorkItemCommentsResult {
    #[serde(default)]
    pub comments: Vec<WorkItemComment>,
}

/// The `POST /api/workitems/assignees` envelope: the repo's assignable user logins
/// (the practical @-mention set).
#[derive(Clone, PartialEq, serde::Deserialize, Default)]
pub(super) struct WorkItemAssigneesResult {
    #[serde(default)]
    pub users: Vec<String>,
}

/// The label a work item's State badge shows. Pure mapping over the wire string so
/// any casing / unknown value still renders sensibly. Returns (display, css-modifier).
pub(super) fn work_item_state_badge(state: &str) -> (&'static str, &'static str) {
    match state.to_ascii_lowercase().as_str() {
        "open" => ("OPEN", "active"),
        "closed" => ("CLOSED", "done"),
        _ => ("UNKNOWN", "neutral"),
    }
}

/// The compact label one work item's row shows in the table's Labels column. Joins
/// with commas; empty -> an em-dash placeholder. Pure so it is unit-testable.
pub(super) fn labels_summary(labels: &[String]) -> String {
    if labels.is_empty() {
        "—".to_string()
    } else {
        labels.join(", ")
    }
}

/// Whether a work item already has a UoW (dedup display logic). When true, the detail
/// view shows "Open Unit of Work" (and the existing UoW id) instead of a Create button.
/// Matching is by the work item's stable id against each UoW's referenced work item id.
pub(super) fn existing_uow_for<'a>(uows: &'a [UowListEntry], work_item_id: &str) -> Option<&'a UowListEntry> {
    uows.iter()
        .find(|u| u.work_item.as_ref().is_some_and(|wi| wi.id == work_item_id))
}

/// The button label for the create/open affordance, given whether a UoW already exists.
/// Pure: drives both the table-row action and the detail view consistently.
pub(super) fn create_or_open_label(has_uow: bool) -> &'static str {
    if has_uow {
        "Open Unit of Work"
    } else {
        "Create Unit of Work from this issue"
    }
}

/// Pull ALL open issues across ALL the active project's repos (`POST /api/workitems/pull`).
/// Manual / user-triggered; no cache. Body is empty (the server uses the active project).
pub(super) async fn pull_work_items() -> Option<Vec<WorkItem>> {
    reqwest::Client::new()
        .post(format!("{}/api/workitems/pull", crate::BFF_URL))
        .json(&serde_json::json!({}))
        .send()
        .await
        .ok()?
        .json::<PullWorkItemsResult>()
        .await
        .ok()
        .map(|r| r.items)
}

/// List all Units of Work with their referenced WorkItem + lifecycle stage
/// (`GET /api/uows`).
pub(super) async fn fetch_uows() -> Option<Vec<UowListEntry>> {
    reqwest::get(format!("{}/api/uows", crate::BFF_URL))
        .await
        .ok()?
        .json::<UowsResult>()
        .await
        .ok()
        .map(|r| r.uows)
}

/// Create a UoW referencing a work item (`POST /api/uow/from-workitem`). Dedups by
/// external ref server-side: an existing UoW comes back with `created=false`.
pub(super) async fn create_uow_from_work_item(work_item_id: &str) -> Option<FromWorkItemResult> {
    reqwest::Client::new()
        .post(format!("{}/api/uow/from-workitem", crate::BFF_URL))
        .json(&serde_json::json!({ "work_item_id": work_item_id }))
        .send()
        .await
        .ok()?
        .json::<FromWorkItemResult>()
        .await
        .ok()
}

/// One message in the story-authoring clarification chat (mirrors the server's
/// `AuthorChatMessage`). `role` is `"user"` or `"ai"`.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Debug, Default)]
pub(super) struct AuthorChatMessageView {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub text: String,
}

/// The story-authoring state of a draft UoW (mirrors the server's `AuthoringState`).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Debug, Default)]
pub(super) struct AuthoringStateView {
    #[serde(default)]
    pub requirements_prompt: String,
    #[serde(default)]
    pub chat: Vec<AuthorChatMessageView>,
    #[serde(default)]
    pub draft_title: String,
    #[serde(default)]
    pub draft_body: String,
}

/// The subset of the server's `UnitOfWork` the authoring panel reads back from the
/// `POST /api/uow/:id/author` and `GET /api/uow/:id` endpoints.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Debug, Default)]
pub(super) struct AuthoringUowView {
    #[serde(default)]
    pub story_id: String,
    #[serde(default)]
    pub authoring: Option<AuthoringStateView>,
    #[serde(default)]
    pub work_item: Option<String>,
    /// The normalized parent issue number stored on the draft (set from the authoring
    /// screen). `None` → no parent link will be created at publish time.
    #[serde(default)]
    pub parent_id: Option<String>,
}

/// Create a blank draft UoW to author a story (`POST /api/uow/blank`). When
/// `parent_id` is `Some` and non-empty, it is sent as the `parent_id` field so the
/// server can create a native GitHub sub-issue link at publish time. An empty string
/// or `None` both produce `{ "parent_id": null }` (no parent). Returns the new draft
/// id on success.
pub(super) async fn create_blank_uow(parent_id: Option<String>) -> Option<String> {
    // Normalize empty → None so the server receives null (not "").
    let pid = parent_id.filter(|s| !s.trim().is_empty());
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/uow/blank", crate::BFF_URL))
        .json(&serde_json::json!({ "parent_id": pid }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    v.get("uow_id")
        .and_then(|x| x.as_str())
        .map(String::from)
}

/// Delete a UoW entirely (`DELETE /api/uow/:id`). Returns `true` on a 2xx. The UI gates
/// this behind an "are you sure?" confirmation before calling it.
pub(super) async fn delete_uow(story_id: &str) -> bool {
    reqwest::Client::new()
        .delete(format!("{}/api/uow/{}", crate::BFF_URL, enc_seg(story_id)))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Set (or clear) a draft UoW's parent issue (`POST /api/uow/:id/set-draft-parent`). An
/// empty / whitespace-only `parent_id` clears the parent (sent as null). Returns `true`
/// on a 2xx. The parent is picked on the authoring screen; the publish step consumes the
/// stored value to create a native GitHub sub-issue link.
pub(super) async fn set_draft_parent(story_id: &str, parent_id: &str) -> bool {
    let pid = parent_id.trim();
    let body = if pid.is_empty() {
        serde_json::json!({ "parent_id": null })
    } else {
        serde_json::json!({ "parent_id": pid })
    };
    reqwest::Client::new()
        .post(format!(
            "{}/api/uow/{}/set-draft-parent",
            crate::BFF_URL,
            enc_seg(story_id)
        ))
        .json(&body)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Fetch a draft UoW's current authoring state (`GET /api/uow/:id`).
pub(super) async fn fetch_authoring_uow(story_id: &str) -> Option<AuthoringUowView> {
    reqwest::get(format!("{}/api/uow/{}", crate::BFF_URL, enc_seg(story_id)))
        .await
        .ok()?
        .json::<AuthoringUowView>()
        .await
        .ok()
}

/// Send a message to the story-authoring assistant (`POST /api/uow/:id/author`). Returns
/// the updated authoring UoW (with the refreshed draft + chat). `model` is the per-turn
/// model override from the UI selector; an empty string defers to the server's project
/// StoryAuthoring step model (back-compat).
pub(super) async fn post_author_message(
    story_id: &str,
    message: &str,
    model: &str,
) -> Option<AuthoringUowView> {
    let mut body = serde_json::json!({ "message": message });
    if !model.trim().is_empty() {
        body["model"] = serde_json::Value::String(model.trim().to_string());
    }
    reqwest::Client::new()
        .post(format!("{}/api/uow/{}/author", crate::BFF_URL, enc_seg(story_id)))
        .json(&body)
        .send()
        .await
        .ok()?
        .json::<AuthoringUowView>()
        .await
        .ok()
}

/// The outcome of publishing a drafted story to the board (`POST /api/uow/:id/publish`).
pub(super) enum PublishOutcome {
    /// Published + linked; the UoW is now a normal linked UoW.
    Ok,
    /// The server rejected the publish (4xx) with a human-readable reason.
    Rejected(String),
    /// A transport failure.
    Failed,
}

/// Publish a drafted story to the board and link the UoW (`POST /api/uow/:id/publish`).
pub(super) async fn post_publish(story_id: &str, repo: &str) -> PublishOutcome {
    let url = format!("{}/api/uow/{}/publish", crate::BFF_URL, enc_seg(story_id));
    let resp = match reqwest::Client::new()
        .post(url)
        .json(&serde_json::json!({ "repo": repo }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return PublishOutcome::Failed,
    };
    if resp.status().is_success() {
        PublishOutcome::Ok
    } else {
        // The server returns { "error": "<why>" } for an AppError 4xx/5xx.
        let reason = resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| {
                v.get("error")
                    .or_else(|| v.get("message"))
                    .and_then(|r| r.as_str().map(String::from))
            })
            .unwrap_or_else(|| "Could not publish the story.".to_string());
        PublishOutcome::Rejected(reason)
    }
}

/// Re-pull a single work item (`POST /api/workitems/refresh`).
pub(super) async fn refresh_work_item(work_item_id: &str) -> Option<WorkItem> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/workitems/refresh", crate::BFF_URL))
        .json(&serde_json::json!({ "work_item_id": work_item_id }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    serde_json::from_value(v.get("item")?.clone()).ok()
}

/// Comment back onto the source issue (`POST /api/workitems/comment`). Returns the
/// comment url on success.
pub(super) async fn comment_on_work_item(work_item_id: &str, body: &str) -> Option<String> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/workitems/comment", crate::BFF_URL))
        .json(&serde_json::json!({ "work_item_id": work_item_id, "body": body }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    // `ok` must be truthy; surface the url it returns.
    if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        Some(
            v.get("url")
                .and_then(|u| u.as_str())
                .unwrap_or_default()
                .to_string(),
        )
    } else {
        None
    }
}

/// Set (link) a parent issue for a work item (`POST /api/workitems/set-parent`). Returns
/// `Ok(())` on success or `Err(message)` with the server's reason on failure. The number
/// is sent as a string; the server normalizes `"42"` / `"#42"`.
pub(super) async fn set_work_item_parent(
    work_item_id: &str,
    parent_number: &str,
) -> Result<(), String> {
    let res = reqwest::Client::new()
        .post(format!("{}/api/workitems/set-parent", crate::BFF_URL))
        .json(&serde_json::json!({
            "work_item_id": work_item_id,
            "parent_number": parent_number,
        }))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    let v: serde_json::Value = res
        .json()
        .await
        .map_err(|e| format!("bad response: {e}"))?;
    if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        Ok(())
    } else {
        Err(v
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("could not set parent")
            .to_string())
    }
}

/// Read the COMMENTS on a work item (`POST /api/workitems/comments`). Degrades to an
/// empty list (server returns `{ comments: [] }` token-less / on error).
pub(super) async fn fetch_work_item_comments(work_item_id: &str) -> Vec<WorkItemComment> {
    let res = reqwest::Client::new()
        .post(format!("{}/api/workitems/comments", crate::BFF_URL))
        .json(&serde_json::json!({ "work_item_id": work_item_id }))
        .send()
        .await
        .ok();
    match res {
        Some(r) => r
            .json::<WorkItemCommentsResult>()
            .await
            .ok()
            .map(|r| r.comments)
            .unwrap_or_default(),
        None => Vec::new(),
    }
}

/// Read the assignable users for a work item's repo (`POST /api/workitems/assignees`).
/// Degrades to an empty list (server returns `{ users: [] }` token-less / on error).
pub(super) async fn fetch_work_item_assignees(work_item_id: &str) -> Vec<String> {
    let res = reqwest::Client::new()
        .post(format!("{}/api/workitems/assignees", crate::BFF_URL))
        .json(&serde_json::json!({ "work_item_id": work_item_id }))
        .send()
        .await
        .ok();
    match res {
        Some(r) => r
            .json::<WorkItemAssigneesResult>()
            .await
            .ok()
            .map(|r| r.users)
            .unwrap_or_default(),
        None => Vec::new(),
    }
}

/// Detect an ACTIVE `@<partial>` mention token at the END of the comment text, for the
/// autocomplete dropdown. Pragmatic approach: we look only at the LAST whitespace-
/// separated token of the whole value. If it starts with `@` and contains no further
/// `@`, the part after the `@` is the active partial (possibly empty, right after
/// typing `@`). Returns `None` when there is no active token (the dropdown stays hidden).
///
/// KNOWN LIMITATION: this tracks the tail of the value, not the caret. Editing a mention
/// in the MIDDLE of already-typed text does not re-open the dropdown. Full mid-text caret
/// tracking is a follow-up; the tail case covers the overwhelming common path (type then
/// mention).
pub(super) fn active_mention_partial(value: &str) -> Option<&str> {
    // A trailing whitespace means the user just finished a token (e.g. a completed
    // mention + space): there is no ACTIVE token, so the dropdown closes.
    if value.is_empty() || value.ends_with(char::is_whitespace) {
        return None;
    }
    // The last whitespace-delimited token is the active one.
    let last = value.rsplit(char::is_whitespace).next()?;
    let partial = last.strip_prefix('@')?;
    // A second `@` inside the token (e.g. an email-ish `a@b`) is not a mention token.
    if partial.contains('@') {
        return None;
    }
    Some(partial)
}

/// Replace the trailing active `@<partial>` token in `value` with `@<login> ` (trailing
/// space so the user keeps typing after the completed mention). Pure; used when the user
/// clicks a dropdown suggestion. If there is no active token, appends `@<login> `.
pub(super) fn apply_mention_selection(value: &str, login: &str) -> String {
    match active_mention_partial(value) {
        Some(partial) => {
            // Trim exactly the trailing `@partial` (its byte length) off the value.
            let token_len = 1 + partial.len(); // the `@` plus the partial
            let keep = &value[..value.len() - token_len];
            format!("{keep}@{login} ")
        }
        None => {
            let mut out = value.to_string();
            if !out.is_empty() && !out.ends_with(' ') && !out.ends_with('\n') {
                out.push(' ');
            }
            out.push('@');
            out.push_str(login);
            out.push(' ');
            out
        }
    }
}

/// Filter the assignable logins by the active partial (case-insensitive prefix-ish
/// `contains` match), capped to a short dropdown. An empty partial returns the first
/// few (so typing a bare `@` shows the set). Pure; unit-tested.
pub(super) fn filter_mention_candidates(users: &[String], partial: &str) -> Vec<String> {
    let needle = partial.to_lowercase();
    users
        .iter()
        .filter(|u| needle.is_empty() || u.to_lowercase().contains(&needle))
        .take(8)
        .cloned()
        .collect()
}

// ── Decisions-review surface (Investigating stage) ───────────────────────────────

/// The provenance of an investigation artifact / decision revision (who + when).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Debug, Default)]
pub(super) struct RevisionProvenanceView {
    #[serde(default)]
    pub actor: String,
    #[serde(default)]
    pub at: String,
}

/// A story's investigation note as the BFF serializes it
/// (`camerata_worktracker::investigation::InvestigationArtifact`).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Debug, Default)]
pub(super) struct InvestigationNoteView {
    #[serde(default)]
    pub story_id: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub reviewed: bool,
    #[serde(default)]
    pub provenance: RevisionProvenanceView,
}

/// A decision record's approval outcome. The wire form is an internally-tagged enum
/// (`{ "state": "pending" | "approved" | "rejected", "reason"? }`).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Debug)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(super) enum DecisionOutcomeView {
    Pending,
    Approved,
    Rejected {
        #[serde(default)]
        reason: String,
    },
}

impl Default for DecisionOutcomeView {
    fn default() -> Self {
        DecisionOutcomeView::Pending
    }
}

impl DecisionOutcomeView {
    fn label(&self) -> &'static str {
        match self {
            DecisionOutcomeView::Pending => "Pending",
            DecisionOutcomeView::Approved => "Approved",
            DecisionOutcomeView::Rejected { .. } => "Rejected",
        }
    }
    fn css(&self) -> &'static str {
        match self {
            DecisionOutcomeView::Pending => "neutral",
            DecisionOutcomeView::Approved => "done",
            DecisionOutcomeView::Rejected { .. } => "error",
        }
    }
    fn is_approved(&self) -> bool {
        matches!(self, DecisionOutcomeView::Approved)
    }
}

/// One decision record (mirrors `camerata_worktracker::investigation::DecisionRecord`).
/// Round-trips through serde so the UI can POST the full set back to `/decisions`.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Debug)]
pub(super) struct DecisionRecordView {
    pub artifact_id: String,
    pub story_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub question: String,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub alternatives_considered: Vec<String>,
    pub outcome: DecisionOutcomeView,
    #[serde(default)]
    pub provenance: RevisionProvenanceView,
}

/// The `GET /api/uow/:id/investigation` envelope.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Debug, Default)]
pub(super) struct InvestigationReviewView {
    #[serde(default)]
    pub story_id: String,
    #[serde(default)]
    pub note_present: bool,
    #[serde(default)]
    pub note: Option<InvestigationNoteView>,
    #[serde(default)]
    pub decisions: Vec<DecisionRecordView>,
}

/// Fetch the investigation note + decisions for the decisions-review surface.
pub(super) async fn fetch_investigation_review(story_id: &str) -> Option<InvestigationReviewView> {
    reqwest::get(format!(
        "{}/api/uow/{}/investigation",
        crate::BFF_URL,
        enc_seg(story_id)
    ))
    .await
    .ok()?
    .json::<InvestigationReviewView>()
    .await
    .ok()
}

/// Replace the full decision-record set for a story (`POST /api/uow/:id/decisions`). The
/// server stores them as the source of truth the development gate reads. Returns `true`
/// on a 2xx.
pub(super) async fn post_decisions(story_id: &str, decisions: &[DecisionRecordView]) -> bool {
    reqwest::Client::new()
        .post(format!(
            "{}/api/uow/{}/decisions",
            crate::BFF_URL,
            enc_seg(story_id)
        ))
        .json(decisions)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Mark the story's investigation note reviewed (`POST /api/uow/:id/investigation/review`).
/// Returns `true` when the server reports `ok` (a new reviewed revision was written).
pub(super) async fn mark_investigation_reviewed(story_id: &str) -> bool {
    let v: serde_json::Value = match reqwest::Client::new()
        .post(format!(
            "{}/api/uow/{}/investigation/review",
            crate::BFF_URL,
            enc_seg(story_id)
        ))
        .send()
        .await
    {
        Ok(r) => r.json().await.unwrap_or(serde_json::json!({})),
        Err(_) => return false,
    };
    v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false)
}

/// Whether an investigation note's text is the token-free placeholder (live mode off /
/// no real agent output). Used to surface an explicit "no output" state instead of a
/// silent Investigating with a meaningless note. Pure.
pub(super) fn is_placeholder_note(note: &str) -> bool {
    let n = note.to_ascii_lowercase();
    n.contains("live mode is off")
        || n.contains("live mode off")
        || n.contains("investigation pending")
        || note.trim().is_empty()
}

/// Which sub-view of the Governed Development page is selected in the left nav:
/// the top-level Issue Management panel, or a specific UoW's dev controls.
#[derive(Clone, PartialEq, Eq)]
pub(super) enum GovDevSel {
    /// The Issue Management panel (connection summary + pull + work-item table).
    IssueManagement,
    /// A selected UoW (by its id), showing that UoW's dev controls.
    Uow(String),
}

/// The Governed Development page. Left: "Issue Management" entry + a card per UoW.
/// Right (main): the issue-management panel, or the selected UoW's dev controls.
#[component]
pub(super) fn GovernedDevPage() -> Element {
    // Selection in the left nav. Defaults to the Issue Management panel.
    let mut sel = use_signal(|| GovDevSel::IssueManagement);

    // The UoW list (left-nav cards). Re-fetched whenever this tick bumps (e.g. after a
    // UoW is created from a work item).
    let uows_refresh = use_signal(|| 0u32);
    // Which UoW (by id) is awaiting a delete confirmation, if any. Gates the trash icon.
    let mut confirm_delete = use_signal(|| Option::<String>::None);
    let uows_res = use_resource(move || {
        let _dep = uows_refresh();
        async move { fetch_uows().await }
    });

    let uows = uows_res.read().clone().flatten().unwrap_or_default();

    rsx! {
        div { class: "govdev",
            // ── LEFT NAV: Issue Management + one card per UoW ──────────────────
            aside { class: "govdev-nav",
                button {
                    class: if sel() == GovDevSel::IssueManagement { "govdev-nav-top on" } else { "govdev-nav-top" },
                    onclick: move |_| sel.set(GovDevSel::IssueManagement),
                    span { class: "govdev-nav-top-title", "Issue Management" }
                    span { class: "govdev-nav-top-sub", "Pull issues · create Units of Work" }
                }
                // ── New Unit of Work: author a story with AI ──────────────────────
                NewAuthoredUowButton { uows_refresh, sel }
                // ── NEEDS YOU: open structured clarifications (resumable pause points) ──
                NeedsYouQueue {}
                p { class: "govdev-nav-label", "UNITS OF WORK ({uows.len()})" }
                div { class: "govdev-uow-list",
                    if uows.is_empty() {
                        p { class: "govdev-uow-empty", "No Units of Work yet. Pull work items and create one from an issue, or author a new story with AI." }
                    }
                    for u in uows.iter() {
                        {
                            let uid = u.id.clone();
                            let selected = sel() == GovDevSel::Uow(uid.clone());
                            let cls = if selected { "govdev-uow-card sel" } else { "govdev-uow-card" };
                            // A draft (authoring) UoW has no work item yet; show a draft label.
                            let title = match &u.work_item {
                                Some(wi) => wi.title.clone(),
                                None => "Untitled draft story".to_string(),
                            };
                            let repo = match &u.work_item {
                                Some(wi) => wi.repo.clone(),
                                None => String::new(),
                            };
                            let stage = if u.authoring { "Authoring" } else { u.stage.label() };
                            let confirming = confirm_delete() == Some(uid.clone());
                            let uid_sel = uid.clone();
                            let uid_trash = uid.clone();
                            let uid_yes = uid.clone();
                            rsx! {
                                div { class: "{cls}",
                                    div {
                                        class: "govdev-uow-cardmain",
                                        onclick: move |_| sel.set(GovDevSel::Uow(uid_sel.clone())),
                                        span { class: "govdev-uow-title", "{title}" }
                                        div { class: "govdev-uow-meta",
                                            if !repo.is_empty() {
                                                span { class: "govdev-uow-repo", "{repo}" }
                                            }
                                            span { class: "govdev-uow-stage", "{stage}" }
                                        }
                                    }
                                    if confirming {
                                        div { class: "govdev-uow-confirm",
                                            span { class: "govdev-uow-confirm-q", "Delete?" }
                                            button {
                                                class: "govdev-uow-confirm-yes",
                                                title: "Confirm delete",
                                                onclick: move |_| {
                                                    let sid = uid_yes.clone();
                                                    let mut uows_refresh = uows_refresh;
                                                    let mut sel = sel;
                                                    let mut confirm_delete = confirm_delete;
                                                    spawn(async move {
                                                        if delete_uow(&sid).await {
                                                            if sel() == GovDevSel::Uow(sid.clone()) {
                                                                sel.set(GovDevSel::IssueManagement);
                                                            }
                                                            uows_refresh += 1;
                                                        }
                                                        confirm_delete.set(None);
                                                    });
                                                },
                                                "Delete"
                                            }
                                            button {
                                                class: "govdev-uow-confirm-no",
                                                onclick: move |_| confirm_delete.set(None),
                                                "Cancel"
                                            }
                                        }
                                    } else {
                                        button {
                                            class: "govdev-uow-trash",
                                            title: "Delete this Unit of Work",
                                            onclick: move |_| confirm_delete.set(Some(uid_trash.clone())),
                                            "\u{1f5d1}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── MAIN: the issue-management panel, or a UoW's dev controls ──────
            section { class: "govdev-main",
                match sel() {
                    GovDevSel::IssueManagement => rsx! {
                        IssueManagementPanel { uows: uows.clone(), uows_refresh, sel }
                    },
                    GovDevSel::Uow(uid) => {
                        match uows.iter().find(|u| u.id == uid).cloned() {
                            // Key by the UoW id so switching UoWs REMOUNTS the controls with
                            // fresh per-UoW state. Without the key, Dioxus reused one instance and
                            // just swapped the prop, so the first UoW's use_signal/use_resource
                            // state (dev status, stage, run model, etc.) bled into every other UoW.
                            //
                            // A DRAFT (authoring) UoW renders the story-authoring panel instead of
                            // the dev controls; once published it becomes a normal linked UoW.
                            Some(u) if u.authoring => rsx! {
                                StoryAuthoringPanel { key: "{u.id}", uow_id: u.id.clone(), uows_refresh, sel }
                            },
                            Some(u) => rsx! { UowDevControls { key: "{u.id}", uow: u } },
                            // The UoW vanished from the list (e.g. between refreshes): fall back.
                            None => rsx! {
                                p { class: "section-hint", "That Unit of Work is no longer available." }
                            },
                        }
                    }
                }
            }
        }
    }
}

/// The Issue Management panel: a GitHub-specific connection summary + a "Pull work items"
/// button, then a provider-agnostic table of pulled `WorkItem`s and a row-detail view.
///
/// PROVIDER-ADAPTER SEAM: the connection summary + the pull action are the only
/// GitHub-aware pieces here (the BFF resolves the active project's GitHub repos). The
/// table and the detail view operate purely on `WorkItem`, so a future Jira/ADO panel
/// reuses them verbatim.
#[component]
pub(super) fn IssueManagementPanel(
    uows: Vec<UowListEntry>,
    uows_refresh: Signal<u32>,
    sel: Signal<GovDevSel>,
) -> Element {
    let provider_res = use_resource(fetch_provider);
    let active_proj = use_resource(fetch_active_project);

    let mut pulling = use_signal(|| false);
    // The work item whose detail is open (by stable id), if any.
    let mut detail_id = use_signal(|| Option::<String>::None);
    // Bumped on every pull and used as the table's `key`, so the Chorale work-item table
    // remounts with fresh rows (it initializes its rows once per mount via use_table).
    let mut pull_seq = use_signal(|| 0u32);

    let conn = provider_res.read().clone().flatten();
    let proj = active_proj.read().clone().flatten();
    let repos = proj.as_ref().map(|p| p.repos.clone()).unwrap_or_default();

    // GITHUB-SPECIFIC connection summary.
    let (conn_cls, conn_label) = match &conn {
        Some(p) if p.live => ("conn-ok", format!("● {} connected", p.provider)),
        Some(p) => ("conn-warn", format!("● {} (no GitHub token)", p.provider)),
        None => ("conn-warn", "● connecting…".to_string()),
    };

    // The pulled work items come from an APP-LIFETIME cache (survives navigating away and
    // back), keyed by project id so a project switch never shows stale items. None = not
    // pulled yet for the active project.
    let proj_id = proj.as_ref().map(|p| p.id.clone()).unwrap_or_default();
    let item_list: Option<Vec<WorkItem>> = match PULLED_WORK_ITEMS.read().clone() {
        Some((pid, list)) if !proj_id.is_empty() && pid == proj_id => Some(list),
        _ => None,
    };
    // Resolve the open detail item against the current pull.
    let open_item = match (&item_list, detail_id()) {
        (Some(list), Some(id)) => list.iter().find(|it| it.id == id).cloned(),
        _ => None,
    };

    rsx! {
        div { class: "issue-mgmt",
            p { class: "govdev-h", "Issue Management" }

            // ── Connection summary (GitHub adapter) ───────────────────────────
            div { class: "issue-conn",
                div { class: "issue-conn-line",
                    span { class: "issue-conn-label", "Provider" }
                    span { class: "issue-conn-prov", "GitHub" }
                    span { class: "{conn_cls}", "{conn_label}" }
                }
                div { class: "issue-conn-line",
                    span { class: "issue-conn-label", "Repositories" }
                    if repos.is_empty() {
                        span { class: "issue-conn-none", "No repos on the active project." }
                    } else {
                        span { class: "issue-conn-repos", "{repos.join(\", \")}" }
                    }
                }
            }

            div { class: "issue-pull-row",
                button {
                    class: "btn-run",
                    disabled: pulling(),
                    onclick: {
                        let proj_id = proj_id.clone();
                        move |_| {
                            let proj_id = proj_id.clone();
                            pulling.set(true);
                            spawn(async move {
                                let pulled = pull_work_items().await.unwrap_or_default();
                                *PULLED_WORK_ITEMS.write() = Some((proj_id, pulled));
                                detail_id.set(None);
                                pull_seq += 1;
                                pulling.set(false);
                            });
                        }
                    },
                    if pulling() { "Pulling…" } else { "Pull work items" }
                }
                span { class: "section-hint", "Pulls all open issues across the active project's repos. Manual; no cache." }
            }

            // ── The work-item table (provider-agnostic) ───────────────────────
            match item_list {
                None => rsx! {
                    p { class: "section-hint", "No work items pulled yet — press \u{201c}Pull work items\u{201d}." }
                },
                Some(list) if list.is_empty() => rsx! {
                    p { class: "section-hint", "No open work items found across the active project's repos." }
                },
                Some(list) => rsx! {
                    WorkItemTable {
                        key: "{pull_seq}",
                        items: list,
                        on_open: EventHandler::new(move |id: String| detail_id.set(Some(id))),
                    }
                },
            }

            // ── The detail view for a clicked row (provider-agnostic) ─────────
            if let Some(it) = open_item {
                WorkItemDetail {
                    item: it,
                    uows: uows.clone(),
                    on_close: EventHandler::new(move |_| detail_id.set(None)),
                    uows_refresh,
                    sel,
                    // After a "Set parent" succeeds, re-pull so the parent-driven grouping
                    // updates (the same path the "Pull work items" button uses).
                    on_set_parent: EventHandler::new({
                        let proj_id = proj_id.clone();
                        move |_| {
                            let proj_id = proj_id.clone();
                            spawn(async move {
                                let pulled = pull_work_items().await.unwrap_or_default();
                                *PULLED_WORK_ITEMS.write() = Some((proj_id, pulled));
                                pull_seq += 1;
                            });
                        }
                    }),
                }
            }
        }
    }
}

/// Chorale column set for the work-item table.
///
/// Emits one hidden grouping column per hierarchy depth level (`lvl0`, `lvl1`, …,
/// `lvl{max_depth}`) followed by the visible data columns (Repo, ID, Title, State,
/// Labels). The hierarchy columns drive `set_grouping` and have a minimal initial
/// width since Chorale renders them as group headers, not data columns.
///
/// `max_depth` must match the value used in `build_work_item_rows`; both are
/// derived from the same item list inside `WorkItemTable`.
/// `repo_options` and `state_options` are the distinct values present in the
/// current item list, used to populate the multi-select filters.
pub(super) fn work_item_columns(
    max_depth: usize,
    repo_options: Vec<String>,
    state_options: Vec<String>,
) -> Vec<ColumnDef<WorkItemRow>> {
    let state_badges = BadgeVariantMap::new()
        .with("open", BadgeVariant::new("OPEN", "green"))
        .with("closed", BadgeVariant::new("CLOSED", "gray"))
        .with_fallback(BadgeVariant::new("Unknown", "gray"));

    // One grouping column per hierarchy level.
    // `ColumnId` wraps `&'static str`; we leak a heap-allocated string so the
    // `'static` bound is met for dynamically named columns. The number of leaked
    // strings is bounded by `max_depth + 1` (≤ 9 per table construction).
    let mut cols: Vec<ColumnDef<WorkItemRow>> = (0..=max_depth)
        .map(|lvl| {
            let id: &'static str =
                Box::leak(format!("lvl{lvl}").into_boxed_str());
            // The column header is intentionally blank — Chorale renders the
            // cell value as the group-header label, so no additional header text
            // is needed at the column level.
            ColumnDef::new(
                ColumnId(id),
                "",
                move |r: &WorkItemRow| {
                    CellValue::Text(
                        r.hierarchy_cols.get(lvl).cloned().unwrap_or_default(),
                    )
                },
            )
            .initial_width(220.0)
        })
        .collect();

    // Visible data columns.
    cols.extend([
        ColumnDef::new(ColumnId("repo"), "Repo", |r: &WorkItemRow| {
            CellValue::Text(r.work_item.repo.clone())
        })
        .sortable()
        .filter(FilterKind::MultiSelect { options: repo_options })
        .initial_width(180.0),
        ColumnDef::new(ColumnId("num"), "ID", |r: &WorkItemRow| {
            CellValue::Text(format!("#{}", r.work_item.number))
        })
        .sortable()
        .filter(FilterKind::Text)
        .initial_width(80.0),
        ColumnDef::new(ColumnId("title"), "Title", |r: &WorkItemRow| {
            CellValue::Text(r.work_item.title.clone())
        })
        .sortable()
        .filter(FilterKind::Text)
        .initial_width(420.0),
        ColumnDef::new(ColumnId("state"), "State", |r: &WorkItemRow| {
            CellValue::Text(r.work_item.state.to_ascii_lowercase())
        })
        .sortable()
        .filter(FilterKind::MultiSelect { options: state_options })
        .render_kind(RenderKind::Badge(state_badges))
        .initial_width(110.0),
        ColumnDef::new(ColumnId("labels"), "Labels", |r: &WorkItemRow| {
            CellValue::Text(labels_summary(&r.work_item.labels))
        })
        .filter(FilterKind::Text)
        .initial_width(240.0),
    ]);
    cols
}

/// A provider-agnostic Chorale table of `WorkItem`s with N-level hierarchy grouping.
/// Columns: one hidden `lvlN` column per ancestor depth (grouping), then Repo, #,
/// Title, State, Labels as visible data columns.
/// Clicking a row opens its detail modal via `on_open` — the parent
/// (`IssueManagementPanel`) hosts the modal, outside this table's subtree.
#[component]
pub(super) fn WorkItemTable(items: Vec<WorkItem>, on_open: EventHandler<String>) -> Element {
    // Build rows and derive max_depth from the same item list so the column set
    // and the grouping call are always in sync.
    let (rows, max_depth, repo_options, state_options): (
        Vec<(RowId, WorkItemRow)>,
        usize,
        Vec<String>,
        Vec<String>,
    ) = use_hook({
        let items = items.clone();
        move || {
            let built = build_work_item_rows(&items);
            let max_depth = built
                .first()
                .map(|r| r.hierarchy_cols.len().saturating_sub(1))
                .unwrap_or(0);
            // Derive distinct sorted repo and state values for the multi-select filters.
            let mut repos: Vec<String> = {
                let mut seen = std::collections::BTreeSet::new();
                for r in &built { seen.insert(r.work_item.repo.clone()); }
                seen.into_iter().collect()
            };
            repos.sort();
            let mut states: Vec<String> = {
                let mut seen = std::collections::BTreeSet::new();
                for r in &built { seen.insert(r.work_item.state.to_ascii_lowercase()); }
                seen.into_iter().collect()
            };
            states.sort();
            let rows = built
                .into_iter()
                .map(|r| (RowId::new(), r))
                .collect();
            (rows, max_depth, repos, states)
        }
    });
    let id_map: std::collections::HashMap<RowId, String> =
        rows.iter().map(|(r, row)| (*r, row.work_item.id.clone())).collect();
    let handle = use_table(move || {
        TableState::new(rows.clone(), work_item_columns(max_depth, repo_options.clone(), state_options.clone()))
    });
    // Group by all hierarchy levels (lvl0..lvl{max_depth}), producing genuinely
    // nested subgroups. Mirrors the 2-level findings-triage pattern.
    use_hook(move || {
        let grouping: Vec<ColumnId> = (0..=max_depth)
            .map(|lvl| ColumnId(Box::leak(format!("lvl{lvl}").into_boxed_str())))
            .collect();
        handle.set_grouping(grouping);
    });
    rsx! {
        Table {
            handle,
            sort_enabled: true,
            filter_enabled: true,
            sticky_header: true,
            theme: Theme::Dark,
            on_row_click: Callback::new(move |rid: RowId| {
                if let Some(id) = id_map.get(&rid) {
                    on_open.call(id.clone());
                }
            }),
        }
    }
}

/// The detail view for one work item: full title + body + state + a link to the issue,
/// the comments thread, plus (optionally) the create/open-UoW affordance (dedup-aware).
/// Provider-agnostic.
///
/// `show_uow_action` defaults to true (the work-item TABLE opens it to create/open a
/// UoW). When opened from INSIDE an existing UoW's dev controls, pass `false` to hide the
/// redundant create/open-UoW button — the UoW already exists.
#[component]
pub(super) fn WorkItemDetail(
    item: WorkItem,
    uows: Vec<UowListEntry>,
    on_close: EventHandler<()>,
    uows_refresh: Signal<u32>,
    sel: Signal<GovDevSel>,
    #[props(default = true)] show_uow_action: bool,
    /// Called after a successful "Set parent" so the caller can re-pull / re-group the
    /// work items (the grouping is parent-driven). `None` (the default) hides the
    /// "Set parent" affordance — used where there's no pulled-items context to refresh.
    #[props(default)]
    on_set_parent: Option<EventHandler<()>>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let (state_label, state_cls) = work_item_state_badge(&item.state);
    let existing = existing_uow_for(&uows, &item.id).cloned();
    // "Set parent" affordance state: the typed parent number + an in-flight flag.
    let mut parent_input = use_signal(String::new);
    let mut setting_parent = use_signal(|| false);
    // Fetch this work item's comments once per item id (re-fetches if the id changes).
    let comments_res = {
        let wid = item.id.clone();
        use_resource(move || {
            let wid = wid.clone();
            async move { fetch_work_item_comments(&wid).await }
        })
    };
    let comments = comments_res.read().clone();
    rsx! {
        // Modal overlay (click backdrop to close); the inner box stops propagation so
        // clicks inside don't dismiss. Same overlay/box pattern as the rule detail modal.
        div { class: "rule-modal-overlay", onclick: move |_| on_close.call(()),
            div { class: "rule-modal wi-detail-modal", onclick: move |e| e.stop_propagation(),
                div { class: "wi-detail-head",
                    span { class: "wi-detail-repo", "{item.repo}" }
                    span { class: "wi-detail-num", "#{item.number}" }
                    span { class: "wi-state {state_cls}", "{state_label}" }
                    button {
                        class: "rule-modal-close",
                        onclick: move |_| on_close.call(()),
                        "\u{2715}"
                    }
                }
                p { class: "wi-detail-title", "{item.title}" }
                if item.body.is_empty() {
                    p { class: "wi-detail-body empty", "(no description)" }
                } else {
                    // GitHub issue bodies are Markdown — render to HTML (same renderer as
                    // the chat bubble and docs view), not raw text.
                    div {
                        class: "wi-detail-body md chat-turn-text",
                        dangerous_inner_html: crate::md::md_to_html(&item.body),
                    }
                }
                if !item.url.is_empty() {
                    a { class: "wi-detail-link", href: "{item.url}", target: "_blank", "Open issue \u{2197}" }
                }

                // ── Set parent (native GitHub sub-issue link) ─────────────────────
                // Shown only when the caller can refresh the grouping after a change.
                // The current parent (when set) is surfaced so re-parenting is informed.
                if let Some(on_set_parent) = on_set_parent {
                    div { class: "wi-set-parent",
                        p { class: "wi-set-parent-h", "Parent issue" }
                        if let Some(pn) = item.parent_number {
                            p { class: "section-hint", "Currently a sub-issue of #{pn}. Enter a number to re-parent." }
                        } else {
                            p { class: "section-hint", "No parent. Enter an issue number to make this a sub-issue." }
                        }
                        div { class: "wi-set-parent-row",
                            input {
                                class: "govdev-parent-id-input",
                                r#type: "text",
                                placeholder: "e.g. 42 or #42",
                                disabled: setting_parent(),
                                value: "{parent_input}",
                                oninput: move |e| parent_input.set(e.value()),
                            }
                            button {
                                class: "btn-edit-sm",
                                disabled: setting_parent() || parent_input().trim().is_empty(),
                                onclick: {
                                    let wid = item.id.clone();
                                    move |_| {
                                        let wid = wid.clone();
                                        let number = parent_input().trim().to_string();
                                        let toasts = toasts;
                                        let on_set_parent = on_set_parent;
                                        setting_parent.set(true);
                                        spawn(async move {
                                            match set_work_item_parent(&wid, &number).await {
                                                Ok(()) => {
                                                    crate::toast::push_toast(
                                                        toasts,
                                                        crate::toast::ToastKind::Info,
                                                        format!("Set parent to #{}.", number.trim_start_matches('#')),
                                                    );
                                                    parent_input.set(String::new());
                                                    setting_parent.set(false);
                                                    // Re-pull so the grouping reflects the new linkage.
                                                    on_set_parent.call(());
                                                }
                                                Err(msg) => {
                                                    crate::toast::push_toast(
                                                        toasts,
                                                        crate::toast::ToastKind::Warning,
                                                        msg,
                                                    );
                                                    setting_parent.set(false);
                                                }
                                            }
                                        });
                                    }
                                },
                                if setting_parent() { "Setting…" } else { "Set parent" }
                            }
                        }
                    }
                }

                // ── Comments thread (read-only, fetched for this item) ────────────
                div { class: "wi-comments",
                    p { class: "wi-comments-h", "Comments" }
                    match comments {
                        // Still loading the comments fetch.
                        None => rsx! { p { class: "section-hint", "Loading comments…" } },
                        Some(list) if list.is_empty() => rsx! {
                            p { class: "wi-comments-empty section-hint", "No comments." }
                        },
                        Some(list) => rsx! {
                            for (i , c) in list.into_iter().enumerate() {
                                div { key: "{i}", class: "wi-comment",
                                    div { class: "wi-comment-meta",
                                        span { class: "wi-comment-author", "{c.author}" }
                                        if !c.created_at.is_empty() {
                                            span { class: "wi-comment-date", "{c.created_at}" }
                                        }
                                    }
                                    if c.body.is_empty() {
                                        p { class: "wi-comment-body empty", "(empty comment)" }
                                    } else {
                                        div {
                                            class: "wi-comment-body md chat-turn-text",
                                            dangerous_inner_html: crate::md::md_to_html(&c.body),
                                        }
                                    }
                                }
                            }
                        },
                    }
                }

                if show_uow_action {
                    div { class: "wi-detail-actions",
                        CreateOrOpenUow { item: item.clone(), existing, uows_refresh, sel, compact: false }
                    }
                }
            }
        }
    }
}

/// The dedup-aware create/open-UoW button shared by the table rows and the detail view.
/// If a UoW already exists for the work item, it shows "Open Unit of Work" and selects it;
/// otherwise it creates one (`POST /api/uow/from-workitem`), bumps the UoW list, and opens
/// the new UoW. `compact` renders the small in-row variant.
#[component]
pub(super) fn CreateOrOpenUow(
    item: WorkItem,
    existing: Option<UowListEntry>,
    uows_refresh: Signal<u32>,
    sel: Signal<GovDevSel>,
    compact: bool,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut working = use_signal(|| false);
    let has_uow = existing.is_some();
    let label = create_or_open_label(has_uow);
    let base_cls = if compact { "btn-edit-sm" } else { "btn-run" };

    rsx! {
        button {
            class: "{base_cls}",
            disabled: working(),
            // Stop the row's onclick (open-detail) from also firing.
            onclick: move |evt| {
                evt.stop_propagation();
                let mut sel = sel;
                let mut uows_refresh = uows_refresh;
                let toasts = toasts;
                if let Some(ref u) = existing {
                    sel.set(GovDevSel::Uow(u.id.clone()));
                    return;
                }
                let wid = item.id.clone();
                working.set(true);
                spawn(async move {
                    match create_uow_from_work_item(&wid).await {
                        Some(res) => {
                            uows_refresh += 1;
                            sel.set(GovDevSel::Uow(res.uow_id.clone()));
                            if !res.created {
                                crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Info,
                                    "A Unit of Work already existed for this issue — opened it.".to_string(),
                                );
                            }
                        }
                        None => {
                            crate::toast::push_toast(
                                toasts,
                                crate::toast::ToastKind::Warning,
                                "Could not create a Unit of Work from this issue.".to_string(),
                            );
                        }
                    }
                    working.set(false);
                });
            },
            if working() { "Working…" } else { "{label}" }
        }
    }
}

/// The "New Unit of Work — author a story" action in the left nav. Creates a blank draft
/// UoW (`POST /api/uow/blank`) and selects it so the authoring panel opens. The inverse of
/// "create from issue": author the story first, then publish it to the board.
///
/// The "Parent ID (optional)" field moved OUT of the nav and INTO the authoring screen
/// (see [`StoryAuthoringPanel`]), so the nav card is now just this button. The draft is
/// created with no parent; the architect sets the parent (if any) on the authoring screen.
#[component]
pub(super) fn NewAuthoredUowButton(uows_refresh: Signal<u32>, sel: Signal<GovDevSel>) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut working = use_signal(|| false);
    rsx! {
        div { class: "govdev-new-uow-area",
            button {
                class: "govdev-nav-top",
                disabled: working(),
                onclick: move |_| {
                    let mut sel = sel;
                    let mut uows_refresh = uows_refresh;
                    let toasts = toasts;
                    working.set(true);
                    spawn(async move {
                        // Create with no parent; the parent is set on the authoring screen.
                        match create_blank_uow(None).await {
                            Some(id) => {
                                uows_refresh += 1;
                                sel.set(GovDevSel::Uow(id));
                            }
                            None => {
                                crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Warning,
                                    "Could not create a new draft Unit of Work.".to_string(),
                                );
                            }
                        }
                        working.set(false);
                    });
                },
                span { class: "govdev-nav-top-title",
                    if working() { "Creating…" } else { "\u{2728} New Unit of Work — author a story" }
                }
                span { class: "govdev-nav-top-sub", "Draft a story with AI · push to the board" }
            }
        }
    }
}

/// The story-authoring panel for a DRAFT (blank/authoring) UoW. A requirements + clarify
/// chat (`POST /api/uow/:id/author`), a live draft preview, a target-repo picker (the
/// project's repos), and a "Push to board & link" button (`POST /api/uow/:id/publish`).
///
/// Story authoring is an LLM text-generation assist — NOT a code-writing agent — so the
/// governed-dev gate is NOT in this path (same class as the chat assistant). On a successful
/// publish the UoW becomes a normal linked UoW and its dev controls take over.
#[component]
pub(super) fn StoryAuthoringPanel(
    uow_id: String,
    uows_refresh: Signal<u32>,
    sel: Signal<GovDevSel>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();

    // The active project's repos for the target picker.
    let active_proj = use_resource(fetch_active_project);
    let repos = active_proj
        .read()
        .clone()
        .flatten()
        .map(|p| p.repos.clone())
        .unwrap_or_default();

    // The authoring state, re-fetched whenever this tick bumps (after each chat turn).
    let refresh = use_signal(|| 0u32);
    let state_res = {
        let id = uow_id.clone();
        use_resource(move || {
            let id = id.clone();
            let _dep = refresh();
            async move { fetch_authoring_uow(&id).await }
        })
    };
    let full = state_res.read().clone().flatten().unwrap_or_default();
    let st = full.authoring.clone().unwrap_or_default();

    let mut message = use_signal(String::new);
    let mut sending = use_signal(|| false);
    let mut publishing = use_signal(|| false);

    // ── Send model selector (mirrors the investigation control) ─────────────────
    // The model option list (same source the run controls use) and the chosen model for
    // the authoring/clarification send. Seeded from the active project's strongest tier
    // once it loads, before the user touches it; editable per-turn.
    let run_models_res = use_resource(fetch_audit_models);
    let run_models_snap = run_models_res.read().clone().flatten();
    let mut send_model = use_signal(String::new);
    {
        let strongest = active_proj
            .read()
            .clone()
            .flatten()
            .map(|p| p.tier_map.strongest.clone())
            .unwrap_or_default();
        use_effect(use_reactive(&strongest, move |strongest| {
            if send_model.peek().is_empty() && !strongest.is_empty() {
                send_model.set(strongest);
            }
        }));
    }

    // ── Stop handle for the in-flight author send ───────────────────────────────
    // The author send is a single synchronous LLM completion on the server (NOT a tracked,
    // cancellable run), so there is no run-id to POST /cancel. Stop therefore aborts the
    // in-flight request task locally and resets the UI to idle. We keep the spawned task's
    // handle so the Stop button can cancel it.
    let mut send_task = use_signal(|| Option::<dioxus::core::Task>::None);

    // ── Parent issue (moved here from the nav) ──────────────────────────────────
    // Seeded once from the draft's stored parent_id; the architect edits it here and
    // it persists to the draft on change (the publish step creates the sub-issue link).
    let mut parent_id = use_signal(String::new);
    let mut parent_seeded = use_signal(|| false);
    if !parent_seeded() {
        if let Some(pid) = full.parent_id.clone() {
            parent_id.set(pid);
        }
        parent_seeded.set(true);
    }
    let mut saving_parent = use_signal(|| false);
    // The selected target repo (defaults to the first project repo when present).
    let mut target_repo = use_signal(String::new);
    if target_repo().is_empty() {
        if let Some(first) = repos.first() {
            target_repo.set(first.clone());
        }
    }

    let draft_title = st.draft_title.clone();
    let draft_body = st.draft_body.clone();
    let body_html = crate::md::md_to_html(&draft_body);
    let chat = st.chat.clone();
    let has_draft = !draft_title.trim().is_empty();

    // Open structured clarifications for this draft story. These are resumable pause
    // points: when the assistant asks a question with options it is posted as a
    // structured clarification (server-side), surfaced here via the reusable
    // ClarifyQuestion component, and the answer is fed back into the authoring chat.
    let open_clars = {
        let id = uow_id.clone();
        use_resource(move || {
            let id = id.clone();
            let _dep = refresh();
            async move { fetch_open_clarifications_for_story(&id).await }
        })
    };
    let open_clars = open_clars.read().clone().unwrap_or_default();

    rsx! {
        div { class: "uow-dev story-authoring",
            div { class: "uow-dev-head",
                span { class: "uow-dev-repo", "\u{2728} Author a story with AI" }
            }
            p { class: "section-hint",
                "Describe the requirements; the assistant drafts a GitHub-issue-style story and \
                 asks clarifying questions. When the draft looks right, push it to the board."
            }

            // ── Clarification chat ────────────────────────────────────────────
            div { class: "authoring-chat",
                if chat.is_empty() {
                    p { class: "section-hint", "Start by describing what the story should accomplish." }
                }
                for m in chat.iter() {
                    {
                        let who = if m.role == "ai" { "Assistant" } else { "You" };
                        let cls = if m.role == "ai" { "authoring-msg ai" } else { "authoring-msg user" };
                        rsx! {
                            div { class: "{cls}",
                                span { class: "authoring-msg-role", "{who}" }
                                p { class: "authoring-msg-text", "{m.text}" }
                            }
                        }
                    }
                }
            }

            // ── Structured clarification (resumable pause point) ────────────────
            // When the assistant asked a question with options, answer it here via
            // the AskUserQuestion-style component. The answer is fed back into the
            // authoring chat as the next message so the draft refines.
            for clar in open_clars.iter() {
                {
                    let id = uow_id.clone();
                    let summary_q = clar.question.clone();
                    rsx! {
                        div { key: "{clar.id}", class: "authoring-clarify",
                            ClarifyQuestion {
                                clar: clar.clone(),
                                on_answered: move |_| {
                                    // Feed the answer back into the authoring loop. We re-fetch
                                    // the clarification's recorded summary by re-drafting from the
                                    // chat: post the user's choice (summary already saved server-side)
                                    // as the next author message so the assistant refines the draft.
                                    let id = id.clone();
                                    let q = summary_q.clone();
                                    let mut refresh = refresh;
                                    let md = send_model();
                                    spawn(async move {
                                        // Re-read the now-answered clarification to get its summary,
                                        // then feed it back as the author's reply.
                                        let answer_text = fetch_clarifications_for_story(&id)
                                            .await
                                            .into_iter()
                                            .find(|c| c.question == q)
                                            .and_then(|c| c.answer)
                                            .unwrap_or_default();
                                        if !answer_text.trim().is_empty() {
                                            let _ = post_author_message(&id, &answer_text, &md).await;
                                        }
                                        refresh += 1;
                                    });
                                },
                            }
                        }
                    }
                }
            }

            div { class: "authoring-input-row",
                textarea {
                    class: "authoring-input",
                    rows: 3,
                    placeholder: "Describe the requirements, or answer the assistant's question…",
                    value: "{message}",
                    oninput: move |e| message.set(e.value()),
                }
                button {
                    class: "btn-run",
                    disabled: sending() || message().trim().is_empty(),
                    onclick: {
                        let id = uow_id.clone();
                        move |_| {
                            let id = id.clone();
                            let mut refresh = refresh;
                            let toasts = toasts;
                            let msg = message().trim().to_string();
                            if msg.is_empty() { return; }
                            let md = send_model();
                            sending.set(true);
                            // Track the spawned task so the Stop button can abort it.
                            let task = spawn(async move {
                                let _guard = crate::loading::LoadingGuard::new();
                                match post_author_message(&id, &msg, &md).await {
                                    Some(_) => {
                                        message.set(String::new());
                                        refresh += 1;
                                    }
                                    None => {
                                        crate::toast::push_toast(
                                            toasts,
                                            crate::toast::ToastKind::Warning,
                                            "The authoring assistant did not respond. Try again.".to_string(),
                                        );
                                    }
                                }
                                sending.set(false);
                                send_task.set(None);
                            });
                            send_task.set(Some(task));
                        }
                    },
                    if sending() { "Drafting…" } else { "Send" }
                }
                // Model selector — mirrors the investigation "Begin investigation" control.
                ModelSelect { models: run_models_snap.clone(), selected: send_model }
                // In-progress: the background Bombe machine activates via the global
                // loading guard held by the spawn task above — no inline spinner needed.
                // Stop button — shown only while a send is in flight. The author send is not
                // a tracked run, so Stop aborts the in-flight request task and resets to idle.
                if sending() {
                    button {
                        class: "btn-stop",
                        onclick: move |_| {
                            if let Some(task) = send_task.take() {
                                task.cancel();
                            }
                            sending.set(false);
                        },
                        "\u{25a0} Stop"
                    }
                }
            }

            // ── Live draft preview ────────────────────────────────────────────
            div { class: "authoring-preview",
                p { class: "uow-dev-section-h", "Draft preview" }
                if has_draft {
                    p { class: "uow-dev-title", "{draft_title}" }
                    div { class: "chat-md", dangerous_inner_html: "{body_html}" }
                } else {
                    p { class: "section-hint", "No draft yet — send a message to start the draft." }
                }
            }

            // ── Parent issue (optional) ───────────────────────────────────────
            // Moved here from the left nav: set a parent GitHub issue number so the
            // published story is created as a native sub-issue of it. Persists to the
            // draft on change; empty clears the parent.
            div { class: "authoring-parent",
                p { class: "uow-dev-section-h", "Parent issue (optional)" }
                div { class: "govdev-parent-id-row",
                    label {
                        r#for: "authoring-parent-id",
                        class: "govdev-parent-id-label",
                        "Parent ID"
                    }
                    input {
                        id: "authoring-parent-id",
                        class: "govdev-parent-id-input",
                        r#type: "text",
                        placeholder: "e.g. 42 or #42",
                        disabled: saving_parent(),
                        value: "{parent_id}",
                        oninput: move |e| parent_id.set(e.value()),
                        // Persist on blur so the draft carries the parent into publish.
                        onblur: {
                            let id = uow_id.clone();
                            move |_| {
                                let id = id.clone();
                                let val = parent_id();
                                let toasts = toasts;
                                saving_parent.set(true);
                                spawn(async move {
                                    if !set_draft_parent(&id, &val).await {
                                        crate::toast::push_toast(
                                            toasts,
                                            crate::toast::ToastKind::Warning,
                                            "Could not save the parent issue on the draft.".to_string(),
                                        );
                                    }
                                    saving_parent.set(false);
                                });
                            }
                        },
                    }
                }
                p { class: "section-hint",
                    "Sets a parent GitHub issue. On publish, this story is created as a native sub-issue. Leave blank for no parent."
                }
            }

            // ── Push to board & link ──────────────────────────────────────────
            div { class: "authoring-publish",
                p { class: "uow-dev-section-h", "Push to board" }
                if repos.is_empty() {
                    p { class: "section-hint", "No repos on the active project. Add one to publish the story." }
                } else {
                    div { class: "authoring-publish-row",
                        label { class: "authoring-repo-label", "Target repo" }
                        select {
                            class: "authoring-repo-select",
                            value: "{target_repo}",
                            onchange: move |e| target_repo.set(e.value()),
                            for r in repos.iter() {
                                option { value: "{r}", "{r}" }
                            }
                        }
                        button {
                            class: "btn-run",
                            disabled: publishing() || !has_draft || target_repo().is_empty(),
                            onclick: {
                                let id = uow_id.clone();
                                move |_| {
                                    let id = id.clone();
                                    let repo = target_repo();
                                    let mut sel = sel;
                                    let mut uows_refresh = uows_refresh;
                                    let toasts = toasts;
                                    publishing.set(true);
                                    spawn(async move {
                                        match post_publish(&id, &repo).await {
                                            PublishOutcome::Ok => {
                                                crate::toast::push_toast(
                                                    toasts,
                                                    crate::toast::ToastKind::Info,
                                                    "Story published to the board and linked.".to_string(),
                                                );
                                                uows_refresh += 1;
                                                // Re-select the SAME UoW id: it is now a linked UoW,
                                                // so the dev controls render in place.
                                                sel.set(GovDevSel::Uow(id.clone()));
                                            }
                                            PublishOutcome::Rejected(reason) => {
                                                crate::toast::push_toast(
                                                    toasts,
                                                    crate::toast::ToastKind::Warning,
                                                    reason,
                                                );
                                            }
                                            PublishOutcome::Failed => {
                                                crate::toast::push_toast(
                                                    toasts,
                                                    crate::toast::ToastKind::Warning,
                                                    "Could not reach the server to publish.".to_string(),
                                                );
                                            }
                                        }
                                        publishing.set(false);
                                    });
                                }
                            },
                            if publishing() { "Publishing…" } else { "Push to board & link" }
                        }
                    }
                    p { class: "section-hint", "Creates a GitHub issue from the draft and links this Unit of Work to it." }
                }
            }
        }
    }
}

/// The 3-phase Governed-Development cockpit shell for a selected Unit of Work.
///
/// Top bar (always visible):
/// - UoW status badge (informational only, derived from lifecycle stage)
/// - "Pull latest work item" button
/// - Phase selector: Intake / Investigation & Refinement / Development (free navigation)
///
/// Phase body delegates to `IntakePhaseView`, `InvestigationPhaseView`, or
/// `DevelopmentPhaseView`. The "Stop run" control is surfaced in the top bar whenever a
/// run is live.
///
/// The `GateSelfCheck {}` UI box has been removed from this component (the gateway
/// enforcement code in cockpit.rs is preserved; only the UI invocation is removed).
#[component]
pub(super) fn UowDevControls(uow: UowListEntry) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    // The UoW id keys every reused governed-dev endpoint (run, sign-off, UoW panel).
    let uow_key = uow.id.clone();

    // A local copy of the work item so "Pull latest" can refresh the displayed metadata
    // without re-fetching the whole UoW list. `UowDevControls` is only rendered for a
    // LINKED UoW (its work item is `Some`); fall back to a default if somehow absent.
    let mut item = use_signal(|| uow.work_item.clone().unwrap_or_default());
    // Re-sync the displayed item when the selected UoW changes (prop change).
    use_effect(use_reactive(&uow.work_item, move |wi| {
        item.set(wi.unwrap_or_default())
    }));

    // The reused per-UoW UoW panel / run live behind a refresh tick, same as the old page.
    let uow_refresh = use_signal(|| 0u32);

    // Fetch the UoW (keyed on the same refresh tick the panel uses) so we know the
    // current lifecycle `stage` and can derive the status label + seed the phase tab.
    let uow_for_stage = {
        let sid = uow.id.clone();
        use_resource(move || {
            let sid = sid.clone();
            let _dep = uow_refresh();
            async move { fetch_uow(&sid).await }
        })
    };
    // The KNOWN lifecycle stage, or `None` when the UoW fetch is still loading OR failed.
    let stage: Option<UowStage> = uow_for_stage
        .read()
        .as_ref()
        .and_then(|opt| opt.as_ref().map(|u| u.stage));

    // Live run state for THIS UoW (governed fleet through the gate).
    let active_run = use_signal(|| Option::<RunView>::None);

    // Model option list for every per-step selector.
    let run_models_res = use_resource(fetch_audit_models);
    let run_models_snap = run_models_res.read().clone().flatten();

    // The active project's tier map seeds the per-phase model defaults: investigation
    // defaults to the strongest tier; development pre-fills all three tiers. Each is
    // editable per-UoW for the run, without mutating the saved project tier map.
    let project_res = use_resource(fetch_active_project);
    let project_tier_map = project_res
        .read()
        .clone()
        .flatten()
        .map(|p| p.tier_map)
        .unwrap_or_default();

    // INVESTIGATION model (single select; default = project strongest).
    let mut invest_model = use_signal(String::new);
    // DEVELOPMENT tier models (three selects; defaults from the project tier map).
    let mut dev_strongest = use_signal(String::new);
    let mut dev_balanced = use_signal(String::new);
    let mut dev_fast = use_signal(String::new);
    // Seed the per-phase selectors from the project tier map once it loads, before the
    // user has touched them. Re-seeds if the active project changes.
    {
        let tm = project_tier_map.clone();
        use_effect(use_reactive(&tm, move |tm| {
            if invest_model.peek().is_empty() {
                invest_model.set(tm.strongest.clone());
            }
            if dev_strongest.peek().is_empty() {
                dev_strongest.set(tm.strongest.clone());
            }
            if dev_balanced.peek().is_empty() {
                dev_balanced.set(tm.balanced.first().cloned().unwrap_or_default());
            }
            if dev_fast.peek().is_empty() {
                dev_fast.set(tm.fast.first().cloned().unwrap_or_default());
            }
        }));
    }

    // Pull-latest state.
    let mut refreshing = use_signal(|| false);

    // ── Work-item modal (opened from inside the UoW) ───────────────────────────
    let mut wi_modal_open = use_signal(|| false);
    let modal_uows_refresh = use_signal(|| 0u32);
    let modal_sel = use_signal(|| GovDevSel::IssueManagement);

    // ── 3-phase shell state ────────────────────────────────────────────────────
    // Phase tab: initialised from the lifecycle stage once known, then freely navigable.
    let mut phase = use_signal(|| PhaseTab::Intake);
    // Per-phase finish flags. Persisted to the UoW meta (#105) so Finish/Reopen survives
    // sessions; hydrated from the meta on load below.
    let mut intake_finished = use_signal(|| false);
    let mut investigation_finished = use_signal(|| false);
    let mut development_finished = use_signal(|| false);

    // Hydrate the per-phase finished flags from the persisted UoW meta (#105). One-shot so
    // a refresh doesn't fight a just-toggled Finish/Reopen before it persists.
    let mut meta_hydrated = use_signal(|| false);
    {
        let loaded_meta = uow_for_stage
            .read()
            .clone()
            .flatten()
            .map(|u| u.meta);
        use_effect(use_reactive(&loaded_meta, move |loaded_meta| {
            if meta_hydrated() {
                return;
            }
            if let Some(meta) = loaded_meta {
                intake_finished.set(meta.intake_finished);
                investigation_finished.set(meta.investigation_finished);
                development_finished.set(meta.development_finished);
                meta_hydrated.set(true);
            }
        }));
    }

    // Persist the finished flags to the UoW meta whenever they change, AFTER hydration (so
    // the initial hydrate doesn't echo back a redundant write). Fire-and-forget.
    {
        let sid = uow_key.clone();
        use_effect(move || {
            let i = intake_finished();
            let v = investigation_finished();
            let d = development_finished();
            if !meta_hydrated() {
                return;
            }
            let sid = sid.clone();
            spawn(async move {
                let _ = save_meta(
                    &sid,
                    serde_json::json!({
                        "intake_finished": i,
                        "investigation_finished": v,
                        "development_finished": d,
                    }),
                )
                .await;
            });
        });
    }

    // Seed the phase tab from the lifecycle stage the FIRST time the stage loads.
    // We use a one-shot flag so user navigation isn't overwritten on every refresh.
    let mut phase_seeded = use_signal(|| false);
    use_effect(move || {
        if !phase_seeded() {
            if let Some(st) = stage {
                phase.set(stage_to_phase(st));
                phase_seeded.set(true);
            }
        }
    });

    // Persist the viewed phase to the UoW meta when the architect navigates (#105). The
    // viewed phase is informational view state; it never drives control flow.
    {
        let sid = uow_key.clone();
        use_effect(move || {
            let p = phase();
            // Don't persist the default until the stage-seed has run, to avoid clobbering a
            // stored viewed_phase with the initial Intake default before seeding.
            if !phase_seeded() {
                return;
            }
            let wire = match p {
                PhaseTab::Intake => "intake",
                PhaseTab::Investigation => "investigation",
                PhaseTab::Development => "development",
            };
            let sid = sid.clone();
            spawn(async move {
                let _ = save_meta(&sid, serde_json::json!({ "viewed_phase": wire })).await;
            });
        });
    }

    // Informational status label — purely display, never drives control flow.
    let status_label = stage
        .map(stage_to_status_label)
        .unwrap_or("Loading…");

    // Stop-run busy flag (surfaced in top bar when a run is live).
    let mut stopping = use_signal(|| false);

    let it = item.read().clone();
    let (state_label, state_cls) = work_item_state_badge(&it.state);

    rsx! {
        div { class: "uow-dev",
            // ── Work-item header (provider-agnostic read of the DTO) ───────────
            div { class: "uow-dev-head",
                span { class: "uow-dev-repo", "{it.repo}" }
                span { class: "uow-dev-num", "#{it.number}" }
                span { class: "wi-state {state_cls}", "{state_label}" }
                button {
                    class: "btn-edit-sm",
                    onclick: move |_| wi_modal_open.set(true),
                    "Open work item"
                }
                if !it.url.is_empty() {
                    a { class: "wi-detail-link", href: "{it.url}", target: "_blank", "Open issue ↗" }
                }
            }
            p { class: "uow-dev-title", "{it.title}" }

            if wi_modal_open() {
                WorkItemDetail {
                    item: it.clone(),
                    uows: Vec::new(),
                    on_close: EventHandler::new(move |_| wi_modal_open.set(false)),
                    uows_refresh: modal_uows_refresh,
                    sel: modal_sel,
                    show_uow_action: false,
                }
            }

            // ── Top bar: status · pull · phase selector · stop ────────────────
            div { class: "uow-phase-topbar",
                // Status (informational only — derived from lifecycle stage, not a control)
                div { class: "uow-phase-status",
                    span { class: "uow-status-label", "Status:" }
                    span { class: "uow-status-badge", "{status_label}" }
                }

                // Pull latest work item
                button {
                    class: "btn-edit-sm",
                    disabled: refreshing(),
                    onclick: move |_| {
                        let wid = item.read().id.clone();
                        let toasts = toasts;
                        refreshing.set(true);
                        spawn(async move {
                            match refresh_work_item(&wid).await {
                                Some(updated) => item.set(updated),
                                None => crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Warning,
                                    "Could not re-pull the work item from the tracker (check the GitHub token / that the issue still exists).".to_string(),
                                ),
                            }
                            refreshing.set(false);
                        });
                    },
                    if refreshing() { "Pulling…" } else { "Pull latest work item" }
                }

                // Phase selector — free navigation, never auto-advances
                div { class: "uow-phase-tabs",
                    {
                        let tabs = [
                            (PhaseTab::Intake, intake_finished()),
                            (PhaseTab::Investigation, investigation_finished()),
                            (PhaseTab::Development, development_finished()),
                        ];
                        rsx! {
                            for (tab, finished) in tabs {
                                {
                                    let is_active = phase() == tab;
                                    let mut cls = String::from("uow-phase-tab");
                                    if is_active { cls.push_str(" active"); }
                                    if finished { cls.push_str(" finished"); }
                                    let label = if finished {
                                        format!("{} (done)", tab.label())
                                    } else {
                                        tab.label().to_string()
                                    };
                                    rsx! {
                                        button {
                                            class: "{cls}",
                                            onclick: move |_| phase.set(tab),
                                            "{label}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Stop run — visible whenever a run is live, regardless of phase
                {
                    let active = active_run();
                    let stoppable = active.as_ref().map(|r| !r.done).unwrap_or(false);
                    let rid = active.as_ref().map(|r| r.id.clone()).unwrap_or_default();
                    if stoppable {
                        rsx! {
                            div { class: "uow-run-stop-row",
                                button {
                                    class: "btn-secondary uow-run-stop",
                                    disabled: stopping(),
                                    onclick: move |_| {
                                        let rid = rid.clone();
                                        let mut uow_refresh = uow_refresh;
                                        let toasts = toasts;
                                        stopping.set(true);
                                        spawn(async move {
                                            let ok = cancel_run(&rid).await;
                                            if ok {
                                                crate::toast::push_toast(
                                                    toasts,
                                                    crate::toast::ToastKind::Info,
                                                    "Stopping the run…".to_string(),
                                                );
                                            } else {
                                                crate::toast::push_toast(
                                                    toasts,
                                                    crate::toast::ToastKind::Warning,
                                                    "Could not stop the run (it may have already finished).".to_string(),
                                                );
                                            }
                                            uow_refresh += 1;
                                            stopping.set(false);
                                        });
                                    },
                                    if stopping() { "Stopping…" } else { "■ Stop run" }
                                }
                                span { class: "section-hint", "Cancels the running agent and stops this run." }
                            }
                        }
                    } else {
                        rsx! {}
                    }
                }
            }

            // ── Phase body: delegate to the selected phase view ───────────────
            match phase() {
                PhaseTab::Intake => rsx! {
                    IntakePhaseView {
                        story_id: uow_key.clone(),
                        uow_refresh,
                        item,
                        intake_finished,
                        models: run_models_snap.clone(),
                    }
                },
                PhaseTab::Investigation => rsx! {
                    InvestigationPhaseView {
                        story_id: uow_key.clone(),
                        story_work_item_id: item.read().id.clone(),
                        uow_refresh,
                        active_run,
                        models: run_models_snap.clone(),
                        invest_model,
                        investigation_finished,
                        stage,
                    }
                },
                PhaseTab::Development => rsx! {
                    DevelopmentPhaseView {
                        story_id: uow_key.clone(),
                        uow_refresh,
                        active_run,
                        models: run_models_snap.clone(),
                        dev_strongest,
                        dev_balanced,
                        dev_fast,
                        development_finished,
                        uow_for_stage,
                    }
                },
            }
        }
    }
}

// ── Phase view components ──────────────────────────────────────────────────────

/// Per-repo branch mode selection for Intake scoping.
#[derive(Clone, PartialEq, Debug, Default)]
pub(super) enum BranchModeChoice {
    #[default]
    NewFromBase,
    Existing,
}

/// Branch selection state for one in-scope repo.
#[derive(Clone, PartialEq, Debug, Default)]
pub(super) struct RepoBranchScope {
    pub mode: BranchModeChoice,
    /// When mode=Existing: the existing branch name.
    pub existing_branch: String,
    /// When mode=NewFromBase: the base branch to create from.
    pub base_branch: String,
}

/// Map a persisted `BranchMode` JSON value (tagged enum) back into the UI's
/// `RepoBranchScope`. Unknown / malformed values default to a new-from-base scope.
pub(super) fn repo_scope_from_view(branch: &serde_json::Value) -> RepoBranchScope {
    let mode_tag = branch.get("mode").and_then(|m| m.as_str()).unwrap_or("");
    match mode_tag {
        "existing" => RepoBranchScope {
            mode: BranchModeChoice::Existing,
            existing_branch: branch
                .get("branch_name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            base_branch: String::new(),
        },
        _ => RepoBranchScope {
            mode: BranchModeChoice::NewFromBase,
            existing_branch: String::new(),
            base_branch: branch
                .get("base")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        },
    }
}

/// Build the `repos` JSON array (the wire shape `POST /api/uow/:id/intake/repos` expects:
/// each entry `{ repo, branch: <tagged BranchMode> }`) from the UI's selection + per-repo
/// branch modes. Repos that are selected but have no explicit mode default to new-from-base.
pub(super) fn repo_scope_payload(
    selected: &[String],
    modes: &std::collections::HashMap<String, RepoBranchScope>,
) -> serde_json::Value {
    let entries: Vec<serde_json::Value> = selected
        .iter()
        .map(|repo| {
            let scope = modes.get(repo).cloned().unwrap_or_default();
            let branch = match scope.mode {
                BranchModeChoice::Existing => serde_json::json!({
                    "mode": "existing",
                    "branch_name": scope.existing_branch,
                }),
                BranchModeChoice::NewFromBase => serde_json::json!({
                    "mode": "new_from_base",
                    "base": scope.base_branch,
                    "new_name": "",
                }),
            };
            serde_json::json!({ "repo": repo, "branch": branch })
        })
        .collect();
    serde_json::Value::Array(entries)
}

/// Oversight mode for phase components — controls where clarification dialogs
/// and escalations are routed (per §8).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) enum OversightMode {
    /// Dialogs block inline; answered now (the dev console default).
    #[default]
    Interactive,
    /// Dialogs are emitted to a triage backlog; answered later (routines).
    Batched,
}

/// Intake phase view.
///
/// Contains:
/// - AI-assisted "Update branch" control
/// - Story body + comments inline
/// - Free-text context for the investigation agent
/// - Repo/branch scoping
/// - "Add comment to issue" section
/// - Finish / Reopen controls
#[component]
pub(super) fn IntakePhaseView(
    story_id: String,
    uow_refresh: Signal<u32>,
    item: Signal<WorkItem>,
    mut intake_finished: Signal<bool>,
    models: Option<AuditModelsResp>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();

    // @-mention autocomplete state
    let assignees_res = {
        let wid = item.read().id.clone();
        use_resource(move || {
            let wid = wid.clone();
            async move { fetch_work_item_assignees(&wid).await }
        })
    };
    let assignees = assignees_res.read().clone().unwrap_or_default();
    let mut comment_body = use_signal(String::new);
    let mut commenting = use_signal(|| false);
    let mut mention_open = use_signal(|| false);

    if intake_finished() {
        return rsx! {
            div { class: "uow-phase-body uow-phase-finished",
                div { class: "uow-phase-finished-header",
                    span { class: "uow-phase-finished-label", "Intake (finished)" }
                    button {
                        class: "btn-secondary",
                        onclick: move |_| intake_finished.set(false),
                        "Reopen Intake"
                    }
                }
                p { class: "section-hint", "(Intake finished — reopen to edit)" }
            }
        };
    }

    // Fetch comments for the story inline display.
    let comments_res = {
        let wid = item.read().id.clone();
        use_resource(move || {
            let wid = wid.clone();
            async move { fetch_work_item_comments(&wid).await }
        })
    };

    // Repo/branch scoping — R6.
    let project_res = use_resource(fetch_active_project);
    let project_repos = project_res.read().clone().flatten().map(|p| p.repos.clone()).unwrap_or_default();

    let mut selected_repos: Signal<Vec<String>> = use_signal(Vec::new);
    let mut repo_branch_modes: Signal<std::collections::HashMap<String, RepoBranchScope>> =
        use_signal(std::collections::HashMap::new);

    // Free-text context for the investigation agent.
    let mut intake_context = use_signal(String::new);

    // ── Hydrate intake state from the persisted UoW (#105) ────────────────────────
    // Fetch the UoW once and seed the signals from its `intake` state before the user
    // has touched them. A one-shot flag prevents a refresh from clobbering edits.
    let intake_uow_res = {
        let sid = story_id.clone();
        use_resource(move || {
            let sid = sid.clone();
            let _dep = uow_refresh();
            async move { fetch_uow(&sid).await }
        })
    };
    let mut intake_hydrated = use_signal(|| false);
    {
        let loaded = intake_uow_res.read().clone().flatten();
        use_effect(use_reactive(&loaded, move |loaded| {
            if intake_hydrated() {
                return;
            }
            if let Some(uow) = loaded {
                intake_context.set(uow.intake.context.clone());
                let mut sel = Vec::new();
                let mut modes = std::collections::HashMap::new();
                for rs in uow.intake.repos.iter() {
                    sel.push(rs.repo.clone());
                    modes.insert(rs.repo.clone(), repo_scope_from_view(&rs.branch));
                }
                selected_repos.set(sel);
                repo_branch_modes.set(modes);
                intake_hydrated.set(true);
            }
        }));
    }

    // Persist the current repo/branch scope to the backend (R6). Called after any scope
    // edit. Fire-and-forget so a transient failure never blocks the UI.
    let persist_repos = {
        let sid = story_id.clone();
        move || {
            let sid = sid.clone();
            let payload = repo_scope_payload(&selected_repos(), &repo_branch_modes());
            spawn(async move {
                let _ = save_intake_repos(&sid, payload).await;
            });
        }
    };

    rsx! {
        div { class: "uow-phase-body",
            // "Update branch" controls — one per in-scope repo (populated from Intake
            // scoping below). Falls back to a hint when no repos are in scope yet.
            if selected_repos().is_empty() {
                div { class: "uow-step-control uow-update-branch",
                    p { class: "uow-step-h", "Update branch" }
                    p { class: "section-hint",
                        "Select the in-scope repos below to enable per-repo Update branch controls."
                    }
                }
            } else {
                for repo in selected_repos().iter().cloned().collect::<Vec<_>>() {
                    UowUpdateBranchControl {
                        key: "{repo}",
                        story_id: story_id.clone(),
                        uow_refresh,
                        models: models.clone(),
                        repo_label: repo,
                    }
                }
            }

            // ── Story + comments inline ───────────────────────────────────────────
            div { class: "uow-dev-section",
                p { class: "uow-dev-section-h", "{item.read().title}" }
                {
                    let body = item.read().body.clone();
                    if body.is_empty() {
                        rsx! { p { class: "wi-detail-body empty", "(no description)" } }
                    } else {
                        rsx! {
                            div {
                                class: "wi-detail-body md chat-turn-text",
                                dangerous_inner_html: crate::md::md_to_html(&body),
                            }
                        }
                    }
                }
                div { class: "wi-comments",
                    p { class: "wi-comments-h", "Comments" }
                    {
                        let comments = comments_res.read().clone();
                        match comments {
                            None => rsx! { p { class: "section-hint", "Loading comments…" } },
                            Some(list) if list.is_empty() => rsx! {
                                p { class: "wi-comments-empty section-hint", "No comments." }
                            },
                            Some(list) => rsx! {
                                for (i , c) in list.into_iter().enumerate() {
                                    div { key: "{i}", class: "wi-comment",
                                        div { class: "wi-comment-meta",
                                            span { class: "wi-comment-author", "{c.author}" }
                                            if !c.created_at.is_empty() {
                                                span { class: "wi-comment-date", "{c.created_at}" }
                                            }
                                        }
                                        if c.body.is_empty() {
                                            p { class: "wi-comment-body empty", "(empty comment)" }
                                        } else {
                                            div {
                                                class: "wi-comment-body md chat-turn-text",
                                                dangerous_inner_html: crate::md::md_to_html(&c.body),
                                            }
                                        }
                                    }
                                }
                            },
                        }
                    }
                }
            }

            // ── Free-text context for investigation agent ─────────────────────────
            div { class: "uow-dev-section",
                p { class: "uow-dev-section-h", "Context for the investigation agent" }
                textarea {
                    class: "intake-context-input",
                    rows: "4",
                    placeholder: "Add extra context for the investigation agent — anything the story doesn't capture…",
                    value: "{intake_context}",
                    oninput: move |e| intake_context.set(e.value()),
                    // Persist on blur so the free-text context survives sessions (#105).
                    onblur: {
                        let sid = story_id.clone();
                        move |_| {
                            let sid = sid.clone();
                            let ctx = intake_context();
                            spawn(async move {
                                let _ = save_intake_context(&sid, &ctx).await;
                            });
                        }
                    },
                }
            }

            // ── Repos in scope ────────────────────────────────────────────────────
            div { class: "uow-dev-section",
                p { class: "uow-dev-section-h", "Repos in scope" }
                p { class: "section-hint",
                    "Select which repos this story touches. Out-of-scope repos are not grounded into the investigation agent."
                }
                for repo in project_repos.iter() {
                    {
                        let repo = repo.clone();
                        let is_selected = selected_repos().contains(&repo);
                        rsx! {
                            div { key: "{repo}", class: "intake-repo-row",
                                label {
                                    input {
                                        class: "intake-repo-check",
                                        r#type: "checkbox",
                                        checked: is_selected,
                                        onchange: {
                                            let repo = repo.clone();
                                            let persist_repos = persist_repos.clone();
                                            move |_| {
                                                let mut cur = selected_repos();
                                                if let Some(pos) = cur.iter().position(|r| r == &repo) {
                                                    cur.remove(pos);
                                                } else {
                                                    cur.push(repo.clone());
                                                }
                                                selected_repos.set(cur);
                                                persist_repos.clone()();
                                            }
                                        },
                                    }
                                    " {repo}"
                                }
                                if is_selected {
                                    {
                                        let scope = repo_branch_modes().get(&repo).cloned().unwrap_or_default();
                                        let is_existing = scope.mode == BranchModeChoice::Existing;
                                        rsx! {
                                            div { class: "intake-branch-mode",
                                                label {
                                                    input {
                                                        r#type: "radio",
                                                        name: "branch-mode-{repo}",
                                                        checked: !is_existing,
                                                        onchange: {
                                                            let repo = repo.clone();
                                                            let persist_repos = persist_repos.clone();
                                                            move |_| {
                                                                let mut map = repo_branch_modes();
                                                                let entry = map.entry(repo.clone()).or_default();
                                                                entry.mode = BranchModeChoice::NewFromBase;
                                                                repo_branch_modes.set(map);
                                                                persist_repos.clone()();
                                                            }
                                                        },
                                                    }
                                                    " New branch from base"
                                                }
                                                label {
                                                    input {
                                                        r#type: "radio",
                                                        name: "branch-mode-{repo}",
                                                        checked: is_existing,
                                                        onchange: {
                                                            let repo = repo.clone();
                                                            let persist_repos = persist_repos.clone();
                                                            move |_| {
                                                                let mut map = repo_branch_modes();
                                                                let entry = map.entry(repo.clone()).or_default();
                                                                entry.mode = BranchModeChoice::Existing;
                                                                repo_branch_modes.set(map);
                                                                persist_repos.clone()();
                                                            }
                                                        },
                                                    }
                                                    " Existing branch"
                                                }
                                                if is_existing {
                                                    input {
                                                        r#type: "text",
                                                        placeholder: "Branch name",
                                                        value: "{scope.existing_branch}",
                                                        oninput: {
                                                            let repo = repo.clone();
                                                            move |e| {
                                                                let mut map = repo_branch_modes();
                                                                let entry = map.entry(repo.clone()).or_default();
                                                                entry.existing_branch = e.value();
                                                                repo_branch_modes.set(map);
                                                            }
                                                        },
                                                        // Persist the branch name when the field loses focus (#105).
                                                        onblur: {
                                                            let persist_repos = persist_repos.clone();
                                                            move |_| persist_repos.clone()()
                                                        },
                                                    }
                                                } else {
                                                    input {
                                                        r#type: "text",
                                                        placeholder: "Base branch (e.g. main)",
                                                        value: "{scope.base_branch}",
                                                        oninput: {
                                                            let repo = repo.clone();
                                                            move |e| {
                                                                let mut map = repo_branch_modes();
                                                                let entry = map.entry(repo.clone()).or_default();
                                                                entry.base_branch = e.value();
                                                                repo_branch_modes.set(map);
                                                            }
                                                        },
                                                        // Persist the base branch when the field loses focus (#105).
                                                        onblur: {
                                                            let persist_repos = persist_repos.clone();
                                                            move |_| persist_repos.clone()()
                                                        },
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

            // Add comment to the source issue (with @-mention autocomplete)
            div { class: "uow-comment",
                p { class: "clarify-h", "Add comment to issue" }
                p { class: "section-hint",
                    "Posts a comment back onto the source issue via the tracker adapter. Type @ to mention an assignable teammate (GitHub resolves @handle)."
                }
                div { class: "uow-comment-box",
                    textarea {
                        class: "clarify-q",
                        value: "{comment_body}",
                        rows: "3",
                        placeholder: "Write a comment to post on the issue… (type @ to mention)",
                        oninput: move |e| {
                            let v = e.value();
                            let show = match active_mention_partial(&v) {
                                Some(p) => !filter_mention_candidates(&assignees, p).is_empty(),
                                None => false,
                            };
                            comment_body.set(v);
                            mention_open.set(show);
                        },
                    }
                    if mention_open() {
                        {
                            let partial = active_mention_partial(&comment_body()).unwrap_or("").to_string();
                            let candidates = filter_mention_candidates(&assignees, &partial);
                            rsx! {
                                div { class: "uow-mention-dropdown",
                                    for login in candidates {
                                        button {
                                            key: "{login}",
                                            class: "uow-mention-option",
                                            onclick: {
                                                let login = login.clone();
                                                move |_| {
                                                    let next = apply_mention_selection(&comment_body(), &login);
                                                    comment_body.set(next);
                                                    mention_open.set(false);
                                                }
                                            },
                                            "@{login}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                button {
                    class: "btn-run",
                    disabled: commenting(),
                    onclick: move |_| {
                        let wid = item.read().id.clone();
                        let body = comment_body();
                        if body.trim().is_empty() {
                            return;
                        }
                        let toasts = toasts;
                        commenting.set(true);
                        spawn(async move {
                            match comment_on_work_item(&wid, &body).await {
                                Some(_url) => {
                                    comment_body.set(String::new());
                                    crate::toast::push_toast(
                                        toasts,
                                        crate::toast::ToastKind::Info,
                                        "Comment posted to the issue.".to_string(),
                                    );
                                }
                                None => {
                                    crate::toast::push_toast(
                                        toasts,
                                        crate::toast::ToastKind::Warning,
                                        "Could not post the comment.".to_string(),
                                    );
                                }
                            }
                            commenting.set(false);
                        });
                    },
                    if commenting() { "Posting…" } else { "Post comment" }
                }
            }

            // Finish Intake
            div { class: "uow-phase-finish-row",
                button {
                    class: "btn-secondary",
                    onclick: move |_| intake_finished.set(true),
                    "Finish Intake"
                }
            }
        }
    }
}

/// Investigation & Refinement phase view.
///
/// Contains:
/// - Lifecycle strip + Begin investigation / Approve decisions controls (via `UowStepRunControls`)
/// - Decisions review panel when stage is Investigating / DecisionsApproved
/// - Agent activity for the active run
/// - Clarification dialogs
/// - Agent chat (investigation scope)
/// - Board-visible story comments + add-comment control
/// - Interface contract section
/// - Finish / Reopen controls
#[component]
pub(super) fn InvestigationPhaseView(
    story_id: String,
    /// The work item id for board-visible comment posting and comments fetch.
    story_work_item_id: String,
    uow_refresh: Signal<u32>,
    active_run: Signal<Option<RunView>>,
    models: Option<AuditModelsResp>,
    invest_model: Signal<String>,
    mut investigation_finished: Signal<bool>,
    stage: Option<UowStage>,
    /// Whether this phase is embedded in an Interactive (inline) or Batched (routine triage)
    /// context. Controls where clarification dialogs route their answers.
    #[props(default = OversightMode::Interactive)]
    oversight: OversightMode,
) -> Element {
    let toasts_inv = use_context::<Signal<Vec<crate::toast::Toast>>>();

    // Clarification refresh counter — bumped on each answer.
    let mut clarify_refresh = use_signal(|| 0u32);

    // Fetch open clarifications for this story.
    let open_clars_res = {
        let id = story_id.clone();
        use_resource(move || {
            let id = id.clone();
            let _dep = clarify_refresh();
            async move { fetch_open_clarifications_for_story(&id).await }
        })
    };

    // Agent chat — persisted to the UoW investigation transcript (#105). User turns and
    // the (stubbed) agent reply are both appended via the backend so the refinement
    // session survives sessions.
    let mut chat_messages: Signal<Vec<(String, String)>> = use_signal(Vec::new);
    let mut chat_input = use_signal(String::new);
    let mut chat_sending = use_signal(|| false);

    // Hydrate the chat transcript + contract from the persisted UoW (#105). One-shot so a
    // refresh doesn't clobber in-progress edits.
    let invest_uow_res = {
        let sid = story_id.clone();
        use_resource(move || {
            let sid = sid.clone();
            let _dep = uow_refresh();
            async move { fetch_uow(&sid).await }
        })
    };
    let mut invest_hydrated = use_signal(|| false);

    // Board-visible story comments — assignees + comment state.
    let invest_assignees_res = {
        let wid = story_work_item_id.clone();
        use_resource(move || {
            let wid = wid.clone();
            async move { fetch_work_item_assignees(&wid).await }
        })
    };
    let invest_assignees = invest_assignees_res.read().clone().unwrap_or_default();
    let mut invest_comment_body = use_signal(String::new);
    let mut invest_commenting = use_signal(|| false);
    let mut invest_mention_open = use_signal(|| false);

    // Contract section.
    let mut show_contract = use_signal(|| false);
    let mut contract_text = use_signal(String::new);
    let mut contract_crosses_boundary = use_signal(|| false);

    // Hydrate chat + contract from the persisted UoW investigation state (#105).
    {
        let loaded = invest_uow_res.read().clone().flatten();
        use_effect(use_reactive(&loaded, move |loaded| {
            if invest_hydrated() {
                return;
            }
            if let Some(uow) = loaded {
                let msgs: Vec<(String, String)> = uow
                    .investigation
                    .chat
                    .iter()
                    .map(|t| {
                        // Normalize the persisted "agent" role to the UI's "assistant" class.
                        let role = if t.role == "user" { "user" } else { "assistant" };
                        (role.to_string(), t.text.clone())
                    })
                    .collect();
                chat_messages.set(msgs);
                contract_text.set(uow.investigation.contract.clone());
                contract_crosses_boundary.set(uow.investigation.crosses_boundary);
                if !uow.investigation.contract.trim().is_empty() {
                    show_contract.set(true);
                }
                invest_hydrated.set(true);
            }
        }));
    }

    let _ = oversight; // used for routing in a future PR; field is established here

    if investigation_finished() {
        return rsx! {
            div { class: "uow-phase-body uow-phase-finished",
                div { class: "uow-phase-finished-header",
                    span { class: "uow-phase-finished-label", "Investigation & Refinement (finished)" }
                    button {
                        class: "btn-secondary",
                        onclick: move |_| investigation_finished.set(false),
                        "Reopen Investigation"
                    }
                }
                p { class: "section-hint", "(Investigation finished — reopen to edit)" }
            }
        };
    }

    rsx! {
        div { class: "uow-phase-body",
            // Begin investigation run control (Investigation & Refinement phase only — §4.1)
            UowStepRunControls {
                story_id: story_id.clone(),
                stage,
                uow_refresh,
                active_run,
                models: models.clone(),
                invest_model,
            }

            // Agent activity for the active run
            {
                let rid = match active_run() {
                    Some(ref r) => r.id.clone(),
                    None => String::new(),
                };
                rsx! { crate::agent_activity::AgentActivity { run_id: rid } }
            }

            // Decisions-review surface (Investigating stage)
            if stage == Some(UowStage::Investigating) || stage == Some(UowStage::DecisionsApproved) {
                DecisionsReviewPanel { story_id: story_id.clone(), uow_refresh }
            }

            // ── Clarifications ────────────────────────────────────────────────────
            {
                let clars = open_clars_res.read().clone().unwrap_or_default();
                if clars.is_empty() {
                    rsx! {}
                } else {
                    rsx! {
                        div { class: "uow-dev-section",
                            p { class: "uow-dev-section-h", "Clarifications" }
                            for clar in clars.iter() {
                                {
                                    rsx! {
                                        div { key: "{clar.id}", class: "authoring-clarify",
                                            ClarifyQuestion {
                                                clar: clar.clone(),
                                                on_answered: move |_| {
                                                    clarify_refresh += 1;
                                                },
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── Agent chat (investigation scope) ──────────────────────────────────
            div { class: "uow-agent-chat",
                p { class: "uow-dev-section-h", "Agent chat (investigation scope)" }
                for (i , (role , text)) in chat_messages().into_iter().enumerate() {
                    {
                        let role_cls = if role == "user" { "user" } else { "assistant" };
                        rsx! {
                            div { key: "{i}", class: "uow-agent-chat-turn {role_cls}",
                                span { class: "uow-agent-chat-role", "{role}" }
                                p { class: "uow-agent-chat-text", "{text}" }
                            }
                        }
                    }
                }
                div { class: "uow-agent-chat-input-row",
                    textarea {
                        rows: "3",
                        placeholder: "Message the investigation agent…",
                        value: "{chat_input}",
                        disabled: chat_sending(),
                        oninput: move |e| chat_input.set(e.value()),
                    }
                    button {
                        class: "btn-run",
                        disabled: chat_sending() || chat_input().trim().is_empty(),
                        onclick: {
                            let sid = story_id.clone();
                            move |_| {
                            let sid = sid.clone();
                            let msg = chat_input().trim().to_string();
                            if msg.is_empty() { return; }
                            chat_sending.set(true);
                            let mut msgs = chat_messages();
                            msgs.push(("user".to_string(), msg.clone()));
                            chat_messages.set(msgs);
                            chat_input.set(String::new());
                            spawn(async move {
                                // Persist the user turn to the UoW investigation transcript (#105).
                                let _ = append_investigation_chat(&sid, "user", &msg).await;
                                // TODO(#105): call the real investigation agent chat endpoint (B2).
                                // For now the agent reply is a stub; still persist it so the
                                // transcript round-trips on reload.
                                let stub = "(Investigation agent response — coming soon. TODO #105)".to_string();
                                let _ = append_investigation_chat(&sid, "agent", &stub).await;
                                let mut msgs = chat_messages();
                                msgs.push(("assistant".to_string(), stub));
                                chat_messages.set(msgs);
                                chat_sending.set(false);
                            });
                            }
                        },
                        if chat_sending() { "Sending…" } else { "Send" }
                    }
                }
            }

            // ── Board-visible story comments ───────────────────────────────────────
            div { class: "uow-comment",
                p { class: "clarify-h", "Add comment to issue" }
                p { class: "section-hint",
                    "Posts a comment back onto the source issue via the tracker adapter. Type @ to mention an assignable teammate (GitHub resolves @handle)."
                }
                div { class: "uow-comment-box",
                    textarea {
                        class: "clarify-q",
                        value: "{invest_comment_body}",
                        rows: "3",
                        placeholder: "Write a comment to post on the issue… (type @ to mention)",
                        oninput: move |e| {
                            let v = e.value();
                            let show = match active_mention_partial(&v) {
                                Some(p) => !filter_mention_candidates(&invest_assignees, p).is_empty(),
                                None => false,
                            };
                            invest_comment_body.set(v);
                            invest_mention_open.set(show);
                        },
                    }
                    if invest_mention_open() {
                        {
                            let partial = active_mention_partial(&invest_comment_body()).unwrap_or("").to_string();
                            let candidates = filter_mention_candidates(&invest_assignees, &partial);
                            rsx! {
                                div { class: "uow-mention-dropdown",
                                    for login in candidates {
                                        button {
                                            key: "{login}",
                                            class: "uow-mention-option",
                                            onclick: {
                                                let login = login.clone();
                                                move |_| {
                                                    let next = apply_mention_selection(&invest_comment_body(), &login);
                                                    invest_comment_body.set(next);
                                                    invest_mention_open.set(false);
                                                }
                                            },
                                            "@{login}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                button {
                    class: "btn-run",
                    disabled: invest_commenting(),
                    onclick: {
                        let wid = story_work_item_id.clone();
                        move |_| {
                            let wid = wid.clone();
                            let body = invest_comment_body();
                            if body.trim().is_empty() { return; }
                            let toasts = toasts_inv;
                            invest_commenting.set(true);
                            spawn(async move {
                                match comment_on_work_item(&wid, &body).await {
                                    Some(_url) => {
                                        invest_comment_body.set(String::new());
                                        crate::toast::push_toast(
                                            toasts,
                                            crate::toast::ToastKind::Info,
                                            "Comment posted to the issue.".to_string(),
                                        );
                                    }
                                    None => {
                                        crate::toast::push_toast(
                                            toasts,
                                            crate::toast::ToastKind::Warning,
                                            "Could not post the comment.".to_string(),
                                        );
                                    }
                                }
                                invest_commenting.set(false);
                            });
                        }
                    },
                    if invest_commenting() { "Posting…" } else { "Post comment" }
                }
            }

            // ── Interface contract (R3.g / §4.6) ─────────────────────────────────
            div { class: "uow-contract-section",
                p { class: "uow-dev-section-h", "Interface contract (R3.g)" }
                p { class: "section-hint",
                    "Required only when the story's work crosses a shared interface boundary (API + caller, service + consumer, shared schema + users). Leave blank for single-side changes."
                }
                button {
                    class: "btn-secondary",
                    onclick: move |_| show_contract.set(!show_contract()),
                    if show_contract() { "Hide contract" } else { "Settle interface contract" }
                }
                if show_contract() {
                    textarea {
                        class: "intake-context-input",
                        rows: "6",
                        placeholder: "Describe the interface contract — API shapes, shared types, service interfaces the cross-repo work must satisfy…",
                        value: "{contract_text}",
                        oninput: move |e| contract_text.set(e.value()),
                    }
                    label { class: "uow-contract-boundary",
                        input {
                            r#type: "checkbox",
                            checked: contract_crosses_boundary(),
                            onchange: move |_| {
                                let v = !contract_crosses_boundary();
                                contract_crosses_boundary.set(v);
                            },
                        }
                        " Work crosses a contract boundary (a contract is required before Development)"
                    }
                    div { class: "uow-contract-actions",
                        button {
                            class: "btn-run",
                            onclick: {
                                let sid = story_id.clone();
                                move |_| {
                                    let sid = sid.clone();
                                    let contract = contract_text();
                                    let crosses = contract_crosses_boundary();
                                    let toasts = toasts_inv;
                                    spawn(async move {
                                        // Persist the prose contract + boundary flag (#105 / R3.g).
                                        let ok = save_contract(&sid, &contract, crosses).await;
                                        crate::toast::push_toast(
                                            toasts,
                                            if ok { crate::toast::ToastKind::Info } else { crate::toast::ToastKind::Warning },
                                            if ok { "Contract saved.".to_string() } else { "Could not save the contract.".to_string() },
                                        );
                                    });
                                }
                            },
                            "Save contract"
                        }
                        button {
                            class: "btn-secondary",
                            disabled: true,
                            title: "Coming soon — requires fan-out agent (TODO #105)",
                            "Draft with agent"
                        }
                    }
                }
            }

            // Finish Investigation
            div { class: "uow-phase-finish-row",
                button {
                    class: "btn-secondary",
                    onclick: move |_| investigation_finished.set(true),
                    "Finish Investigation"
                }
            }
        }
    }
}

/// Development phase view.
///
/// Contains:
/// 1. Contract boundary guard — when the UoW crosses a contract boundary and no contract
///    exists in Investigation & Refinement, shows a "needs a contract" block instead of
///    the Begin Development button.
/// 2. Begin Development button (always available when no contract block applies).
/// 3. Agent activity for the active run.
/// 4. Clarifications pause — when the run is awaiting clarification, shows structured
///    dialogs (single/multi-select + Other + chat back); answering resumes the run.
/// 5. Branch name + agent output display so the user can go test the branch.
/// 6. Bug-fix loop — free-text bug report + gated re-run on the same branch.
/// 7. Layer-2 results display (runs automatically at the end of the dev cycle — no button).
/// 8. Ship panel (§5.7) — per in-scope repo:
///    - Base-branch picker
///    - Push / Open PR / Comment link each gated on the prior step
///    - Ship all repos chain button
/// 9. UoW panel (history / provenance / sign-off readout)
/// 10. Live run panel for the active run
/// 11. Done state (read-only + archived)
///
/// Accepts `oversight: OversightMode` (default Interactive).
#[component]
pub(super) fn DevelopmentPhaseView(
    story_id: String,
    uow_refresh: Signal<u32>,
    active_run: Signal<Option<RunView>>,
    models: Option<AuditModelsResp>,
    dev_strongest: Signal<String>,
    dev_balanced: Signal<String>,
    dev_fast: Signal<String>,
    mut development_finished: Signal<bool>,
    uow_for_stage: Resource<Option<UowView>>,
    /// Whether this phase is embedded in an Interactive (inline) or Batched (routine triage)
    /// context. Controls where clarification dialogs route their answers (§8).
    #[props(default = OversightMode::Interactive)]
    oversight: OversightMode,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();

    // story_id clone for the dev-run button (must be captured by value in the closure).
    let story_id_dev = story_id.clone();

    // One-time BOOTSTRAP toggle (default OFF, per-run, NOT persisted): when on, this dev
    // run skips ONLY the layer-2 post-task lint/test bounce so a brownfield repo can land
    // the linters/checkers layer-2 needs. The security gate (layer 1) + the no-code-first
    // decisions gate still apply. The architect turns it back off after the tooling lands.
    let mut bootstrap_skip_layer2 = use_signal(|| false);

    // Derive stage from the shared uow_for_stage resource (informational; not used to gate
    // Begin Development — §5.1: the button is always available unless the contract block applies).
    let _stage: Option<UowStage> = uow_for_stage
        .read()
        .as_ref()
        .and_then(|opt| opt.as_ref().map(|u| u.stage));

    let branch_name: Option<String> = uow_for_stage
        .read()
        .clone()
        .flatten()
        .and_then(|u| u.branch)
        .filter(|b| !b.trim().is_empty());

    let has_branch = branch_name.is_some();

    // ── Contract boundary guard (R3.g / §4.6 / §5.1) ─────────────────────────────
    // UI-level guard: when the story crosses a contract boundary and no contract exists,
    // show a "needs a contract" block instead of letting the user begin development.
    // The true backend enforcement is the orchestrator refusing to start — marked TODO(#105).
    //
    // TODO(#105): The backend (orchestrator) is the real gatekeeper — it inspects the UoW
    // state and refuses to begin development if a contract boundary is crossed but no contract
    // artifact exists. This UI guard is a UX hint ONLY and does not replace that enforcement.
    //
    // Derive `has_contract` + `crosses_contract_boundary` from the persisted UoW
    // investigation state (#105). The contract is settled in Investigation & Refinement and
    // read back here; the orchestrator remains the true run-time enforcer (B2/B3).
    let invest_state: Option<InvestigationStateView> = uow_for_stage
        .read()
        .clone()
        .flatten()
        .map(|u| u.investigation);
    let mut has_contract = use_signal(|| false);
    let mut crosses_contract_boundary = use_signal(|| false);
    // Keep the contract-gate signals in sync as the shared UoW resource resolves/refreshes.
    {
        let invest_state = invest_state.clone();
        use_effect(use_reactive(&invest_state, move |invest_state| {
            has_contract.set(
                invest_state
                    .as_ref()
                    .is_some_and(|i| !i.contract.trim().is_empty()),
            );
            crosses_contract_boundary
                .set(invest_state.as_ref().is_some_and(|i| i.crosses_boundary));
        }));
    }

    // ── Clarification state ────────────────────────────────────────────────────────
    let mut clarify_refresh = use_signal(|| 0u32);
    let open_clars_res = {
        let id = story_id.clone();
        use_resource(move || {
            let id = id.clone();
            let _dep = clarify_refresh();
            // Also re-read when the parent UoW refreshes (e.g. after a run finishes).
            let _dep2 = uow_refresh();
            async move { fetch_open_clarifications_for_story(&id).await }
        })
    };

    // ── Dev agent chat (development scope) ─────────────────────────────────────────
    // Persisted to the UoW development transcript (#105). User turns + the stubbed agent
    // reply are appended via the backend so the chat-back survives sessions.
    let mut dev_chat_messages: Signal<Vec<(String, String)>> = use_signal(Vec::new);
    let mut dev_chat_input = use_signal(String::new);
    let mut dev_chat_sending = use_signal(|| false);
    // Hydrate the dev chat from the persisted UoW development state (#105). One-shot.
    let mut dev_chat_hydrated = use_signal(|| false);
    {
        let dev_state: Option<DevelopmentStateView> = uow_for_stage
            .read()
            .clone()
            .flatten()
            .map(|u| u.development);
        use_effect(use_reactive(&dev_state, move |dev_state| {
            if dev_chat_hydrated() {
                return;
            }
            if let Some(dev) = dev_state {
                let msgs: Vec<(String, String)> = dev
                    .chat
                    .iter()
                    .map(|t| {
                        let role = if t.role == "user" { "user" } else { "assistant" };
                        (role.to_string(), t.text.clone())
                    })
                    .collect();
                dev_chat_messages.set(msgs);
                dev_chat_hydrated.set(true);
            }
        }));
    }

    // ── Bug-fix loop ─────────────────────────────────────────────────────────────
    let mut bug_report = use_signal(String::new);
    let mut bug_fix_running = use_signal(|| false);

    // ── Layer-2 results (displayed after a dev run completes — no button) ─────────
    // TODO(#105): fetch real Layer-2 bounce results from the run's provenance endpoint
    let layer2_results: Option<String> = uow_for_stage
        .read()
        .clone()
        .flatten()
        .and_then(|u| u.gate_provenance)
        .map(|p| format!(
            "Layer-2: {} allowed, {} denied ({} bounces). Rules fired: {}",
            p.allow_count,
            p.deny_count,
            p.total_bounces,
            if p.rules_fired.is_empty() {
                "none".to_string()
            } else {
                p.rules_fired.join(", ")
            }
        ));

    // ── Ship panel state ──────────────────────────────────────────────────────────
    // Base branch for opening the PR (empty → server default branch).
    let mut ship_base_branch = use_signal(String::new);
    // Track the per-repo ship steps. Today's backend is single-repo; the per-repo
    // Ship row rendering uses the in-scope repos from Intake scoping state.
    // TODO(#105): bind to the UoW's in-scope repos from Intake state (R6 / §3);
    // for now we derive the repo list from the UoW's work item repo field.
    let in_scope_repos: Vec<String> = {
        // TODO(#105): replace with actual in-scope repos from UoW intake state (R6)
        // For now, derive from the active project or the work item's repo.
        let project_res = use_resource(fetch_active_project);
        let repos = project_res.read().clone().flatten().map(|p| p.repos.clone()).unwrap_or_default();
        if repos.is_empty() {
            // Graceful fallback: no repos configured means nothing to ship.
            Vec::new()
        } else {
            repos
        }
    };

    // Per-repo ship step state: (pushed, pr_opened, commented)
    // Each repo has its own push/PR/comment gating chain.
    // TODO(#105): true multi-repo ship fan-out via fan_out() tool (R3); today this is
    // a per-repo UI rendering over the existing single-repo PR path.
    let mut ship_pushed = use_signal(|| false);
    let mut ship_pr_opened = use_signal(|| false);
    let mut ship_commented = use_signal(|| false);
    let mut ship_running = use_signal(|| false);
    let mut ship_all_running = use_signal(|| false);

    // Fetch branches for the base-branch picker (reused from UowPrControl).
    let branches_res = {
        let sid = story_id.clone();
        use_resource(move || {
            let sid = sid.clone();
            let _dep = uow_refresh();
            async move { fetch_uow_branches(&sid).await }
        })
    };
    let branches = branches_res.read().clone().unwrap_or_default();

    let _ = oversight; // established for routine reuse; routing used in future PR

    // ── Done state: read-only + archived ─────────────────────────────────────────
    if development_finished() {
        return rsx! {
            div { class: "uow-phase-body uow-phase-finished",
                div { class: "uow-phase-finished-header",
                    span { class: "uow-phase-finished-label", "Development (done)" }
                    button {
                        class: "btn-secondary",
                        onclick: move |_| development_finished.set(false),
                        "Reopen Development"
                    }
                }
                p { class: "section-hint", "(Development archived — reopen to resume work)" }

                // Read-only branch display in done state
                if let Some(ref branch) = branch_name {
                    div { class: "uow-dev-section",
                        p { class: "uow-dev-section-h", "Branch" }
                        span { class: "uow-branch-val", "{branch}" }
                    }
                }

                // Read-only gate provenance / Layer-2 results in done state
                if let Some(ref l2) = layer2_results {
                    div { class: "uow-dev-section",
                        p { class: "uow-dev-section-h", "Layer-2 results" }
                        p { class: "section-hint", "{l2}" }
                    }
                }

                UowPanel { story_id: story_id.clone(), uow_refresh }
            }
        };
    }

    // ── Contract boundary block ───────────────────────────────────────────────────
    // When the story's work crosses a contract boundary and no contract has been settled
    // in Investigation & Refinement, show a block instead of the Begin Development button.
    // This mirrors the orchestrator's refuse-and-push-back behavior (R3.g / §5.1). Both
    // flags are derived from the persisted UoW investigation state above (#105); the
    // orchestrator remains the true run-time enforcer (B2/B3).
    let show_contract_block = crosses_contract_boundary() && !has_contract();

    rsx! {
        div { class: "uow-phase-body",

            // ── §1: Contract boundary guard ─────────────────────────────────────
            if show_contract_block {
                div { class: "uow-dev-section uow-contract-block",
                    p { class: "uow-dev-section-h", "Contract required before Development" }
                    p { class: "section-hint",
                        "This story's work crosses a shared interface boundary (an API and its caller, \
                         a service and its consumer, or a shared schema and its users). The orchestrator \
                         requires a settled contract artifact in Investigation & Refinement before \
                         Development can begin. Switch to the Investigation & Refinement tab to settle it."
                    }
                    // TODO(#105): the backend enforcer is the orchestrator refusing to start
                    // the dev run when no contract exists and the boundary is crossed (R3.g).
                    p { class: "section-hint",
                        "UI-level guard only — the orchestrator enforces this at run time. TODO(#105)."
                    }
                }
            }

            // ── §2: Begin Development — always available (§5.1), contract block applied above
            // The one precondition is the contract gate; if that doesn't apply we always
            // show the development run control regardless of whether Intake / Investigation
            // have run. No lifecycle-stage gating here — the architect says "go" and the
            // orchestrator enforces the contract/Layer-1 rules at run time.
            if !show_contract_block {
                div { class: "uow-step-control",
                    p { class: "uow-step-h", "Development" }
                    div { class: "uow-tier-grid",
                        div { class: "uow-tier-field",
                            span { class: "uow-field-label", "Strongest" }
                            ModelSelect { models: models.clone(), selected: dev_strongest }
                        }
                        div { class: "uow-tier-field",
                            span { class: "uow-field-label", "Balanced" }
                            ModelSelect { models: models.clone(), selected: dev_balanced }
                        }
                        div { class: "uow-tier-field",
                            span { class: "uow-field-label", "Fast" }
                            ModelSelect { models: models.clone(), selected: dev_fast }
                        }
                    }
                    p { class: "section-hint", "The strongest tier orchestrates and delegates simpler work to the balanced and fast tiers." }
                    // One-time bootstrap escape hatch (default OFF, per-run). Skips ONLY
                    // layer-2; the security gate (layer 1) still applies.
                    label { class: "uow-bootstrap-toggle",
                        input {
                            r#type: "checkbox",
                            checked: bootstrap_skip_layer2(),
                            onchange: move |e| bootstrap_skip_layer2.set(e.checked()),
                        }
                        span { class: "uow-bootstrap-text",
                            span { class: "uow-bootstrap-label", "Bootstrap run — skip layer-2 checks" }
                            span { class: "uow-bootstrap-hint",
                                "For the run that installs the linters/checkers layer-2 needs. The security gate (layer 1) still applies. Turn off afterward."
                            }
                        }
                    }
                    div { class: "run-control-row",
                        button {
                            class: "btn-run",
                            onclick: move |_| {
                                let sid = story_id_dev.clone();
                                let tm = TierMapView {
                                    strongest: dev_strongest(),
                                    balanced: vec![dev_balanced()],
                                    fast: vec![dev_fast()],
                                    vision: vec![],
                                };
                                let skip_l2 = bootstrap_skip_layer2();
                                let toasts = toasts;
                                spawn(async move {
                                    match start_dev_run(&sid, &tm, skip_l2).await {
                                        StartRunOutcome::Started(rid) => {
                                            poll_run_to_done(rid, active_run, uow_refresh).await
                                        }
                                        StartRunOutcome::Blocked(reason) => crate::toast::push_toast(
                                            toasts,
                                            crate::toast::ToastKind::Warning,
                                            reason,
                                        ),
                                        StartRunOutcome::Failed => crate::toast::push_toast(
                                            toasts,
                                            crate::toast::ToastKind::Warning,
                                            "Could not start the governed development run.".to_string(),
                                        ),
                                    }
                                });
                            },
                            "\u{25b6} Begin Development"
                        }
                    }
                }
            }

            // ── §3: Agent activity for the active run ────────────────────────────
            {
                let rid = match active_run() {
                    Some(ref r) => r.id.clone(),
                    None => String::new(),
                };
                rsx! { crate::agent_activity::AgentActivity { run_id: rid } }
            }

            // ── §4: Clarifications pause (awaiting_clarification state) ──────────
            // When the orchestrator asks for clarification the run parks; answering resumes it.
            // Reuses the Investigation ClarifyQuestion pattern verbatim (same component).
            {
                let clars = open_clars_res.read().clone().unwrap_or_default();
                if clars.is_empty() {
                    rsx! {}
                } else {
                    rsx! {
                        div { class: "uow-dev-section",
                            p { class: "uow-dev-section-h", "Clarifications (run paused)" }
                            p { class: "section-hint",
                                "The development agent is waiting for your input. Answer each question to resume the run."
                            }
                            for clar in clars.iter() {
                                {
                                    rsx! {
                                        div { key: "{clar.id}", class: "authoring-clarify",
                                            ClarifyQuestion {
                                                clar: clar.clone(),
                                                on_answered: move |_| {
                                                    clarify_refresh += 1;
                                                },
                                            }
                                        }
                                    }
                                }
                            }
                            // Chat back during a clarification pause — scoped to this UoW + repo.
                            // TODO(#105): persist to UoW development transcript and call
                            // the dev-agent clarification-chat endpoint.
                            div { class: "uow-agent-chat",
                                p { class: "uow-dev-section-h", "Chat back (development scope)" }
                                for (i , (role , text)) in dev_chat_messages().into_iter().enumerate() {
                                    {
                                        let role_cls = if role == "user" { "user" } else { "assistant" };
                                        rsx! {
                                            div { key: "{i}", class: "uow-agent-chat-turn {role_cls}",
                                                span { class: "uow-agent-chat-role", "{role}" }
                                                p { class: "uow-agent-chat-text", "{text}" }
                                            }
                                        }
                                    }
                                }
                                div { class: "uow-agent-chat-input-row",
                                    textarea {
                                        rows: "3",
                                        placeholder: "Add context for the development agent…",
                                        value: "{dev_chat_input}",
                                        disabled: dev_chat_sending(),
                                        oninput: move |e| dev_chat_input.set(e.value()),
                                    }
                                    button {
                                        class: "btn-run",
                                        disabled: dev_chat_sending() || dev_chat_input().trim().is_empty(),
                                        onclick: {
                                            let sid = story_id.clone();
                                            move |_| {
                                            let sid = sid.clone();
                                            let msg = dev_chat_input().trim().to_string();
                                            if msg.is_empty() { return; }
                                            dev_chat_sending.set(true);
                                            let mut msgs = dev_chat_messages();
                                            msgs.push(("user".to_string(), msg.clone()));
                                            dev_chat_messages.set(msgs);
                                            dev_chat_input.set(String::new());
                                            spawn(async move {
                                                // Persist the user turn to the UoW development transcript (#105).
                                                let _ = append_development_chat(&sid, "user", &msg).await;
                                                // TODO(#105): call the real dev-agent chat endpoint (B3). For now the
                                                // agent reply is a stub; still persist it so the chat round-trips.
                                                let stub = "(Development agent response — coming soon. TODO #105)".to_string();
                                                let _ = append_development_chat(&sid, "agent", &stub).await;
                                                let mut msgs = dev_chat_messages();
                                                msgs.push(("assistant".to_string(), stub));
                                                dev_chat_messages.set(msgs);
                                                dev_chat_sending.set(false);
                                            });
                                            }
                                        },
                                        if dev_chat_sending() { "Sending…" } else { "Send" }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── §5: Branch name + agent output (go test the branch) ─────────────
            div { class: "uow-dev-section",
                p { class: "uow-dev-section-h", "Branch" }
                if let Some(ref branch) = branch_name {
                    div { class: "uow-branch-row",
                        span { class: "uow-branch-val", "{branch}" }
                        p { class: "section-hint", "Check out this branch and test the changes." }
                    }
                } else {
                    p { class: "section-hint", "No branch yet — start a development run to create one." }
                }
            }

            // ── §6: Bug-fix loop ─────────────────────────────────────────────────
            // Free-text bug report → a gated re-run on the same branch (re-runnable dev).
            // Uses the existing development run path (same gate as the initial dev run).
            div { class: "uow-dev-section",
                p { class: "uow-dev-section-h", "Bug-fix loop" }
                p { class: "section-hint",
                    "Found a bug? Describe it here and a gated agent will attempt the fix on the same branch."
                }
                if !has_branch {
                    p { class: "section-hint", "Requires a branch — run development first." }
                } else {
                    textarea {
                        class: "intake-context-input",
                        rows: "4",
                        placeholder: "Describe the bug — what you expected vs. what happened, steps to reproduce…",
                        value: "{bug_report}",
                        disabled: bug_fix_running(),
                        oninput: move |e| bug_report.set(e.value()),
                    }
                    div { class: "run-control-row",
                        button {
                            class: "btn-run",
                            disabled: bug_fix_running() || bug_report().trim().is_empty(),
                            onclick: {
                                let sid = story_id.clone();
                                let tm = TierMapView {
                                    strongest: dev_strongest(),
                                    balanced: vec![dev_balanced()],
                                    fast: vec![dev_fast()],
                                    vision: vec![],
                                };
                                move |_| {
                                    let sid = sid.clone();
                                    let tm = tm.clone();
                                    let report = bug_report().trim().to_string();
                                    if report.is_empty() { return; }
                                    let toasts = toasts;
                                    bug_fix_running.set(true);
                                    spawn(async move {
                                        // TODO(#105): use a dedicated bug-fix endpoint that threads
                                        // the bug report into the agent context. Today we reuse
                                        // start_dev_run on the same branch (the orchestrator sees
                                        // the bug report in the UoW transcript via the history).
                                        match start_dev_run(&sid, &tm, false).await {
                                            StartRunOutcome::Started(rid) => {
                                                poll_run_to_done(rid, active_run, uow_refresh).await;
                                            }
                                            StartRunOutcome::Blocked(reason) => crate::toast::push_toast(
                                                toasts,
                                                crate::toast::ToastKind::Warning,
                                                reason,
                                            ),
                                            StartRunOutcome::Failed => crate::toast::push_toast(
                                                toasts,
                                                crate::toast::ToastKind::Warning,
                                                "Could not start the bug-fix run.".to_string(),
                                            ),
                                        }
                                        bug_fix_running.set(false);
                                    });
                                }
                            },
                            if bug_fix_running() { "Running bug-fix…" } else { "▶ Run bug-fix (gated)" }
                        }
                    }
                    p { class: "section-hint",
                        "Gated re-run on the same branch. The Layer-1 security gate and Layer-2 mechanical bounce both apply."
                    }
                }
            }

            // ── §7: Layer-2 results (automatic — no button) ─────────────────────
            // Layer-2 runs automatically at the end of every dev cycle and bounces failures
            // back to the agent. We display the last known results from gate provenance.
            // TODO(#105): fetch live Layer-2 bounce results from the run provenance endpoint;
            // today we read the frozen gate provenance stamped on the UoW after the run.
            div { class: "uow-dev-section",
                p { class: "uow-dev-section-h", "Layer-2 results (automatic)" }
                if let Some(ref l2) = layer2_results {
                    p { class: "section-hint", "{l2}" }
                    p { class: "section-hint",
                        "Layer-2 runs automatically at the end of the dev cycle. Failures bounce back to the agent to fix."
                    }
                } else {
                    p { class: "section-hint",
                        "No Layer-2 results yet. Results appear here after the first development run completes."
                    }
                }
                // TODO(#105): integrate live Layer-2 bounce stream and Layer-3 opt-in agentic
                // code reviewer (R7); both run with / parallel to Layer-2.
            }

            // ── §8: Ship panel (§5.7) — per in-scope repo ───────────────────────
            // One Ship row per in-scope repo. The fan-out guarantees each repo's branch
            // is already coherent before this point (fleet doc R3.e-f).
            // TODO(#105): true multi-repo fan-out via fan_out() tool (R3);
            // today the per-repo rows share the existing single-repo PR path.
            div { class: "uow-dev-section",
                p { class: "uow-dev-section-h", "Ship" }
                p { class: "section-hint",
                    "Push the branch, open a PR, and link it on the story — per in-scope repo."
                }

                // Base-branch picker (shared across all repos for now)
                div { class: "run-control-row",
                    span { class: "uow-field-label", "Base branch" }
                    select {
                        class: "uow-branch-select",
                        value: "{ship_base_branch}",
                        onchange: move |e| ship_base_branch.set(e.value()),
                        option { value: "", selected: ship_base_branch().is_empty(), "Default branch" }
                        if !branches.local.is_empty() {
                            optgroup { label: "Local",
                                for b in branches.local.iter() {
                                    option { key: "ship-local:{b}", value: "{b}", "{b}" }
                                }
                            }
                        }
                        if !branches.origin.is_empty() {
                            optgroup { label: "Origin",
                                for b in branches.origin.iter() {
                                    option { key: "ship-origin:{b}", value: "{b}", "{b}" }
                                }
                            }
                        }
                    }
                }

                if !has_branch {
                    p { class: "section-hint", "No branch yet — run development first before shipping." }
                } else if in_scope_repos.is_empty() {
                    p { class: "section-hint", "No repos in scope. Select repos at Intake to enable Ship." }
                } else {
                    // One Ship row per in-scope repo
                    // TODO(#105): each repo should have its own push/PR/comment state once
                    // true multi-repo fan-out (R3) gives each repo its own branch.
                    for repo in in_scope_repos.iter() {
                        {
                            let repo = repo.clone();
                            let sid_push = story_id.clone();
                            let sid_pr = story_id.clone();
                            let sid_comment = story_id.clone();
                            rsx! {
                                div { key: "{repo}", class: "uow-ship-row",
                                    span { class: "uow-ship-repo", "{repo}" }
                                    div { class: "run-control-row",
                                        // Push branch (always enabled when has_branch)
                                        button {
                                            class: "btn-run",
                                            disabled: ship_running(),
                                            onclick: {
                                                let sid = sid_push.clone();
                                                let base = ship_base_branch();
                                                move |_| {
                                                    let sid = sid.clone();
                                                    let base = base.clone();
                                                    let toasts = toasts;
                                                    ship_running.set(true);
                                                    spawn(async move {
                                                        // Push is implicit in open_uow_pr (server pushes before opening).
                                                        // TODO(#105): expose a separate push-only endpoint per repo.
                                                        match open_uow_pr(&sid, &base).await {
                                                            OpenPrOutcome::Opened(n, url) => {
                                                                crate::toast::push_toast(
                                                                    toasts,
                                                                    crate::toast::ToastKind::Info,
                                                                    format!("Pushed and opened PR #{n}: {url}"),
                                                                );
                                                                ship_pushed.set(true);
                                                                ship_pr_opened.set(true);
                                                                uow_refresh += 1;
                                                            }
                                                            OpenPrOutcome::Blocked(reason) => crate::toast::push_toast(
                                                                toasts, crate::toast::ToastKind::Warning, reason,
                                                            ),
                                                            OpenPrOutcome::Failed => crate::toast::push_toast(
                                                                toasts,
                                                                crate::toast::ToastKind::Warning,
                                                                "Could not push / open PR.".to_string(),
                                                            ),
                                                        }
                                                        ship_running.set(false);
                                                    });
                                                }
                                            },
                                            if ship_running() { "Pushing…" } else { "Push branch" }
                                        }

                                        // Open PR — gated on Push having completed
                                        button {
                                            class: "btn-run",
                                            disabled: !ship_pushed() || ship_running(),
                                            onclick: {
                                                let sid = sid_pr.clone();
                                                let base = ship_base_branch();
                                                move |_| {
                                                    let sid = sid.clone();
                                                    let base = base.clone();
                                                    let toasts = toasts;
                                                    ship_running.set(true);
                                                    spawn(async move {
                                                        match open_uow_pr(&sid, &base).await {
                                                            OpenPrOutcome::Opened(n, url) => {
                                                                crate::toast::push_toast(
                                                                    toasts,
                                                                    crate::toast::ToastKind::Info,
                                                                    format!("Opened PR #{n}: {url}"),
                                                                );
                                                                ship_pr_opened.set(true);
                                                                uow_refresh += 1;
                                                            }
                                                            OpenPrOutcome::Blocked(reason) => crate::toast::push_toast(
                                                                toasts, crate::toast::ToastKind::Warning, reason,
                                                            ),
                                                            OpenPrOutcome::Failed => crate::toast::push_toast(
                                                                toasts,
                                                                crate::toast::ToastKind::Warning,
                                                                "Could not open PR.".to_string(),
                                                            ),
                                                        }
                                                        ship_running.set(false);
                                                    });
                                                }
                                            },
                                            "Open PR"
                                        }

                                        // Comment link on story — gated on PR having been opened
                                        button {
                                            class: "btn-secondary",
                                            disabled: !ship_pr_opened() || ship_running(),
                                            onclick: {
                                                let sid = sid_comment.clone();
                                                move |_| {
                                                    let sid = sid.clone();
                                                    let toasts = toasts;
                                                    ship_running.set(true);
                                                    spawn(async move {
                                                        // Post a comment on the story's work item linking the PR.
                                                        // TODO(#105): fetch the PR url from the UoW's pr info to
                                                        // include in the comment body.
                                                        let body = "Development complete — see the pull request for the changes.".to_string();
                                                        // We post the comment on the PR itself (the work item comment
                                                        // path is analogous; both go through the tracker adapter).
                                                        match comment_on_uow_pr(&sid, &body).await {
                                                            Some(_url) => {
                                                                crate::toast::push_toast(
                                                                    toasts,
                                                                    crate::toast::ToastKind::Info,
                                                                    "Comment posted linking the PR.".to_string(),
                                                                );
                                                                ship_commented.set(true);
                                                            }
                                                            None => crate::toast::push_toast(
                                                                toasts,
                                                                crate::toast::ToastKind::Warning,
                                                                "Could not post the link comment.".to_string(),
                                                            ),
                                                        }
                                                        ship_running.set(false);
                                                    });
                                                }
                                            },
                                            "Comment link"
                                        }
                                    }

                                    // Inline ship-step state display
                                    div { class: "uow-ship-status",
                                        if ship_pushed() {
                                            span { class: "uow-ship-step done", "pushed \u{2713}" }
                                        }
                                        if ship_pr_opened() {
                                            span { class: "uow-ship-step done", "PR \u{2713}" }
                                        }
                                        if ship_commented() {
                                            span { class: "uow-ship-step done", "commented \u{2713}" }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Ship all repos chain button — runs push → open-PR → comment in sequence
                    // for all in-scope repos, halting on first failure.
                    // TODO(#105): true multi-repo Ship all via fan_out(); today this is a
                    // sequential single-repo chain (the per-repo rows share one backend path).
                    div { class: "uow-ship-all-row",
                        button {
                            class: "btn-run",
                            disabled: ship_all_running() || !has_branch,
                            onclick: {
                                let sid = story_id.clone();
                                move |_| {
                                    let sid = sid.clone();
                                    let base = ship_base_branch();
                                    let toasts = toasts;
                                    ship_all_running.set(true);
                                    spawn(async move {
                                        // TODO(#105): true multi-repo fan_out(); today we run the
                                        // single-repo path sequentially as a placeholder.
                                        let push_result = open_uow_pr(&sid, &base).await;
                                        match push_result {
                                            OpenPrOutcome::Opened(n, url) => {
                                                ship_pushed.set(true);
                                                ship_pr_opened.set(true);
                                                uow_refresh += 1;
                                                // Post the link comment.
                                                let body = format!("Development complete — PR #{n}: {url}");
                                                match comment_on_uow_pr(&sid, &body).await {
                                                    Some(_) => {
                                                        ship_commented.set(true);
                                                        crate::toast::push_toast(
                                                            toasts,
                                                            crate::toast::ToastKind::Info,
                                                            format!("Ship complete — pushed, PR #{n}, commented."),
                                                        );
                                                    }
                                                    None => crate::toast::push_toast(
                                                        toasts,
                                                        crate::toast::ToastKind::Warning,
                                                        "Push + PR opened, but comment failed.".to_string(),
                                                    ),
                                                }
                                            }
                                            OpenPrOutcome::Blocked(reason) => crate::toast::push_toast(
                                                toasts, crate::toast::ToastKind::Warning, reason,
                                            ),
                                            OpenPrOutcome::Failed => crate::toast::push_toast(
                                                toasts,
                                                crate::toast::ToastKind::Warning,
                                                "Ship all failed at push/PR step.".to_string(),
                                            ),
                                        }
                                        ship_all_running.set(false);
                                    });
                                }
                            },
                            if ship_all_running() {
                                "Shipping\u{2026}"
                            } else {
                                "Ship all repos \u{2192}"
                            }
                        }
                        p { class: "section-hint",
                            "Chains push \u{2192} open PR \u{2192} comment for all in-scope repos. Halts on first failure. TODO(#105): true multi-repo fan-out."
                        }
                    }
                }
            }

            // ── UoW panel (history / provenance / sign-off) ──────────────────────
            UowPanel { story_id: story_id.clone(), uow_refresh }

            // ── Live run panel ───────────────────────────────────────────────────
            if let Some(r) = active_run() {
                LiveRunPanel { run: r, uow_refresh }
            }

            // ── PR lifecycle panel (reused existing control) ──────────────────────
            UowPrControl {
                story_id: story_id.clone(),
                has_branch,
                uow_refresh,
                models: models.clone(),
            }

            // ── Finish Development / Mark Done ────────────────────────────────────
            div { class: "uow-phase-finish-row",
                button {
                    class: "btn-secondary",
                    onclick: move |_| development_finished.set(true),
                    "Mark Done (archive)"
                }
                p { class: "section-hint",
                    "Marks this UoW done and archives it (read-only). The UoW is never deleted — deletion is a separate, explicit act."
                }
            }
        }
    }
}

/// A CI-tier rule item for display in `CiRulesPanel` and for posting to the server.
/// Constructed at each call site from `ProposedRuleView` (onboarding) or from the
/// corpus + project selections (Rules panel). Only `enforcement == "mechanical"` or
/// `enforcement == "architectural"` items are CI-tier; structured/prose are excluded.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct CiRuleItem {
    pub id: String,
    pub title: String,
    pub enforcement: String,
    #[serde(default)]
    pub linter: Option<String>,
}

/// Extract the first linter hint from a `ProposedRuleView`'s sources, if any.
pub(super) fn first_linter(rule: &ProposedRuleView) -> Option<String> {
    rule.sources
        .iter()
        .find_map(|s| s.linter.clone().filter(|l| !l.is_empty()))
}

/// Build `CiRuleItem`s from a proposed-rules list, keeping only CI-tier enforcement
/// levels ("mechanical" and "architectural"). Used at the onboarding call sites where
/// `proposed_rules` is already available on the scan report.
pub(super) fn ci_rule_items_from_proposed(rules: &[ProposedRuleView]) -> Vec<CiRuleItem> {
    rules
        .iter()
        .filter(|r| r.enforcement == "mechanical" || r.enforcement == "architectural")
        .map(|r| CiRuleItem {
            id: r.id.clone(),
            title: r.title.clone(),
            enforcement: r.enforcement.clone(),
            linter: first_linter(r),
        })
        .collect()
}

/// Build `CiRuleItem`s from the project's applied selections joined with the corpus.
/// Used at the Rules-panel call site where we have `RuleSelectionView`s (rule_id only)
/// and must look up enforcement + title from the corpus `Vec<ProposedRuleView>`.
pub(super) fn ci_rule_items_from_selections(
    selections: &[RuleSelectionView],
    corpus: &[ProposedRuleView],
) -> Vec<CiRuleItem> {
    let corpus_map: std::collections::HashMap<&str, &ProposedRuleView> =
        corpus.iter().map(|r| (r.id.as_str(), r)).collect();
    selections
        .iter()
        .filter_map(|s| corpus_map.get(s.rule_id.as_str()).copied())
        .filter(|r| r.enforcement == "mechanical" || r.enforcement == "architectural")
        .map(|r| CiRuleItem {
            id: r.id.clone(),
            title: r.title.clone(),
            enforcement: r.enforcement.clone(),
            linter: first_linter(r),
        })
        .collect()
}

/// POST /api/onboard/ci-rules for a single tier. Returns the GitHub issue URL on success.
pub(super) async fn wire_ci_rules_tier(
    repo: &str,
    tier: &str,
    rules: Vec<CiRuleItem>,
) -> Result<String, String> {
    let payload = serde_json::json!({
        "repo": repo,
        "tier": tier,
        "rules": rules,
    });
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/onboard/ci-rules", crate::BFF_URL))
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("invalid response: {e}"))?;
    let ok = v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false);
    if !ok {
        let msg = v
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        return Err(msg.to_string());
    }
    v.get("url")
        .and_then(|u| u.as_str())
        .map(String::from)
        .ok_or_else(|| "server returned ok but no url".to_string())
}

/// The "add CI-enforced rules" panel, split by enforcement tier.
///
/// Mechanical and architectural rules are both deterministic CI-tier checks. Mechanical
/// rules map to an existing off-the-shelf linter (simple to wire). Architectural rules
/// require a custom checker and team refinement before implementing.
///
/// The panel renders TWO separate "Create story" buttons — one per tier — so the two
/// tracks land as distinct GitHub issues and can be scheduled independently. A button
/// is shown only when that tier has at least one rule. Both buttons are per-repo.
#[component]
pub(super) fn CiRulesPanel(repos: Vec<String>, rules: Vec<CiRuleItem>) -> Element {
    let mut msg = use_signal(String::new);
    let mut busy = use_signal(|| false);

    let mechanical: Vec<CiRuleItem> = rules
        .iter()
        .filter(|r| r.enforcement == "mechanical")
        .cloned()
        .collect();
    let architectural: Vec<CiRuleItem> = rules
        .iter()
        .filter(|r| r.enforcement == "architectural")
        .cloned()
        .collect();

    let has_mechanical = !mechanical.is_empty();
    let has_architectural = !architectural.is_empty();

    rsx! {
        div { class: "fix-panel",
            p { class: "scan-section-h", "Add CI-enforced rules" }
            p { class: "scan-section-sub",
                "Mechanical and architectural rules are both deterministic CI-tier checks. \
                 Mechanical rules map to an existing off-the-shelf linter (simple to wire). \
                 Architectural rules require a custom checker and team refinement before implementing. \
                 Each tier files a separate GitHub issue so the two tracks can be scheduled independently."
            }
            for repo in repos.iter() {
                {
                    let repo = repo.clone();
                    let mech_rules = mechanical.clone();
                    let arch_rules = architectural.clone();
                    rsx! {
                        div { class: "fix-row", key: "{repo}",
                            span { class: "fix-repo", "{repo}" }
                            if has_mechanical {
                                {
                                    let repo_m = repo.clone();
                                    let rules_m = mech_rules.clone();
                                    rsx! {
                                        button {
                                            class: "btn-run",
                                            disabled: busy(),
                                            onclick: move |_| {
                                                let r = repo_m.clone();
                                                let rules = rules_m.clone();
                                                busy.set(true);
                                                msg.set(String::new());
                                                spawn(async move {
                                                    match wire_ci_rules_tier(&r, "mechanical", rules).await {
                                                        Ok(url) => msg.set(format!(
                                                            "Filed mechanical CI-rules story for {r}: {url}"
                                                        )),
                                                        Err(e) => msg.set(format!(
                                                            "Could not file mechanical story for {r}: {e}"
                                                        )),
                                                    }
                                                    busy.set(false);
                                                });
                                            },
                                            "Create mechanical-rules CI story"
                                        }
                                    }
                                }
                            }
                            if has_architectural {
                                {
                                    let repo_a = repo.clone();
                                    let rules_a = arch_rules.clone();
                                    rsx! {
                                        button {
                                            class: "btn-run",
                                            disabled: busy(),
                                            onclick: move |_| {
                                                let r = repo_a.clone();
                                                let rules = rules_a.clone();
                                                busy.set(true);
                                                msg.set(String::new());
                                                spawn(async move {
                                                    match wire_ci_rules_tier(&r, "architectural", rules).await {
                                                        Ok(url) => msg.set(format!(
                                                            "Filed architectural CI-rules story for {r}: {url}"
                                                        )),
                                                        Err(e) => msg.set(format!(
                                                            "Could not file architectural story for {r}: {e}"
                                                        )),
                                                    }
                                                    busy.set(false);
                                                });
                                            },
                                            "Create architectural-rules CI story"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if !msg().is_empty() {
                p { class: "fix-msg", "{msg}" }
            }
        }
    }
}

/// Poll a started run to completion, pushing each snapshot to `active_run` and
/// bumping `uow_refresh` once it finishes (so the panel / stage re-fetch). Shared by
/// the investigation and development run controls.
pub(super) async fn poll_run_to_done(
    run_id: String,
    mut active_run: Signal<Option<RunView>>,
    mut uow_refresh: Signal<u32>,
) {
    // Loading guard for the entire poll loop — Bombe machine stays active
    // for the full duration of the live run (investigation or development).
    let _guard = crate::loading::LoadingGuard::new();
    let mut misses = 0u32;
    loop {
        match fetch_run(&run_id).await {
            Some(rv) => {
                misses = 0;
                let done = rv.done;
                active_run.set(Some(rv));
                if done {
                    uow_refresh += 1;
                    break;
                }
            }
            None => {
                // The run vanished or never registered — don't poll forever. Refresh once
                // so the UI reconciles to the real server state, then stop.
                misses += 1;
                if misses >= 5 {
                    uow_refresh += 1;
                    break;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    }
}

/// "Update branch" control (the GitHub PR "Update branch" pattern, gated).
///
/// Merges a user-selected SOURCE branch (local or origin) INTO this UoW's working
/// branch. A searchable combobox (`<input>` + `<datalist>`) is populated from
/// `POST /api/uow/:story_id/branches`; the user can type to filter the branch list.
/// Branch values carry an `origin:` prefix for origin branches so the handler knows
/// the source kind. The "▶ Update branch" button POSTs to
/// `POST /api/uow/:story_id/update-branch`, then drives `AgentActivity` on the
/// returned run and refreshes the UoW when the run completes.
///
/// A clean merge commits server-side; a conflict is resolved by ONE gated agent (the
/// gate is preserved end to end). A server 4xx (e.g. no branch yet, repo not resolved
/// locally) raises a toast carrying the server's reason. Owns its OWN active-run signal
/// so it doesn't collide with the lifecycle run control.
///
/// `repo_label` is displayed as the target-repo heading so it's clear which repo's
/// branch is being updated when multiple in-scope repos are rendered.
#[component]
pub(super) fn UowUpdateBranchControl(
    story_id: String,
    uow_refresh: Signal<u32>,
    models: Option<AuditModelsResp>,
    /// Owner/repo string shown in the heading so the user knows which repo this row targets.
    repo_label: String,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();

    // The mergeable branches, fetched once per UoW (and after a refresh tick).
    let branches_res = {
        let sid = story_id.clone();
        use_resource(move || {
            let sid = sid.clone();
            let _dep = uow_refresh();
            async move { fetch_uow_branches(&sid).await }
        })
    };
    let branches = branches_res.read().clone().unwrap_or_default();

    // Build a flat option list for the datalist: local branches bare, origin branches
    // with an `origin:` prefix so the submit handler can decode the source kind.
    let all_options: Vec<(String, String)> = {
        let mut opts = Vec::new();
        for b in &branches.local {
            opts.push((b.clone(), b.clone()));
        }
        for b in &branches.origin {
            opts.push((format!("origin:{b}"), format!("origin/{b}")));
        }
        opts
    };

    // The combobox input text (what the user typed / selected).
    // We store the raw option VALUE (with the `origin:` prefix when applicable) so the
    // submit handler can decode the source kind without an extra lookup.
    let mut input_text = use_signal(String::new);
    // The conflict-resolution agent's model (default = project strongest, editable).
    let model = use_signal(String::new);
    // Its own active run + busy flag (independent of the lifecycle run control).
    let active_run = use_signal(|| Option::<RunView>::None);
    let mut updating = use_signal(|| false);

    let has_branches = !all_options.is_empty();
    // A stable datalist id derived from the story_id + repo_label to avoid collisions
    // when multiple per-repo rows are rendered on the same page.
    let datalist_id = format!(
        "ub-branches-{}-{}",
        story_id.replace(['/', '#', ' '], "-"),
        repo_label.replace(['/', '#', ' '], "-"),
    );

    rsx! {
        div { class: "uow-step-control uow-update-branch",
            div { class: "uow-update-branch-repo-header",
                p { class: "uow-step-h", "Update branch" }
                span { class: "uow-update-branch-repo-label", "Pulling into " strong { "{repo_label}" } }
            }
            p { class: "section-hint",
                "Merge a branch INTO this UoW's branch (GitHub's \"Update branch\"). "
                "A clean merge commits directly; AI resolves conflicts if any."
            }
            if has_branches {
                div { class: "run-control-row",
                    // Searchable combobox: native <input list="..."> + <datalist>.
                    // The user types to filter; selecting a suggestion sets the raw value
                    // (with `origin:` prefix for remote branches).
                    div { class: "uow-branch-combobox",
                        input {
                            class: "uow-branch-input",
                            r#type: "text",
                            list: "{datalist_id}",
                            placeholder: "Search or choose a branch…",
                            value: "{input_text}",
                            oninput: move |e| input_text.set(e.value()),
                        }
                        datalist { id: "{datalist_id}",
                            for (val, label) in all_options.iter() {
                                option { key: "{val}", value: "{val}", "{label}" }
                            }
                        }
                    }
                    ModelSelect { models: models.clone(), selected: model }
                    button {
                        class: "btn-run",
                        disabled: updating() || input_text().trim().is_empty(),
                        onclick: move |_| {
                            let raw = input_text().trim().to_string();
                            if raw.is_empty() {
                                return;
                            }
                            // Decode the source kind from the value's `origin:` prefix.
                            let (source, branch) = match raw.strip_prefix("origin:") {
                                Some(b) => ("origin".to_string(), b.to_string()),
                                None => ("local".to_string(), raw.clone()),
                            };
                            let sid = story_id.clone();
                            let md = model();
                            updating.set(true);
                            spawn(async move {
                                match start_update_branch_run(&sid, &branch, &source, &md).await {
                                    StartRunOutcome::Started(rid) => {
                                        poll_run_to_done(rid, active_run, uow_refresh).await;
                                    }
                                    StartRunOutcome::Blocked(reason) => crate::toast::push_toast(
                                        toasts,
                                        crate::toast::ToastKind::Warning,
                                        reason,
                                    ),
                                    StartRunOutcome::Failed => crate::toast::push_toast(
                                        toasts,
                                        crate::toast::ToastKind::Warning,
                                        "Could not start the update-branch run.".to_string(),
                                    ),
                                }
                                updating.set(false);
                            });
                        },
                        if updating() { "Updating…" } else { "▶ Update branch" }
                    }
                }
                // The gated run's live activity (conflict-resolution agent), when running.
                {
                    let rid = match active_run() {
                        Some(ref r) => r.id.clone(),
                        None => String::new(),
                    };
                    rsx! { crate::agent_activity::AgentActivity { run_id: rid } }
                }
            } else {
                p { class: "section-hint",
                    "No branches available — the repo must be cloned locally (set its path in the Rules view)."
                }
            }
        }
    }
}

/// The per-UoW PR lifecycle panel (Decision 2). Gated on the UoW having a branch.
///
/// - **Push & open PR** with a target/base-branch picker (populated from the same
///   `/branches` endpoint the update-branch picker uses). Shows the stored PR number +
///   link once known.
/// - **Pull PR info** → renders the PR state + CI checks (pass/fail/pending + failing
///   names) + comments.
/// - **Resolve with agent** → fires the GATED resolve run; its activity surfaces via the
///   reused `AgentActivity` (the gate is preserved end to end, same as the dev run).
/// - **Add comment** → posts a comment on the PR.
///
/// `has_branch` gates the whole panel: a UoW with no branch yet has nothing to PR.
#[component]
pub(super) fn UowPrControl(
    story_id: String,
    has_branch: bool,
    mut uow_refresh: Signal<u32>,
    models: Option<AuditModelsResp>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();

    // Per-action owned clones of the story id: each onclick closure moves its own copy
    // (a `String` is not `Copy`, so the captures can't share one binding).
    let sid_open = story_id.clone();
    let sid_pull = story_id.clone();
    let sid_resolve = story_id.clone();
    let sid_comment = story_id.clone();

    // The base-branch picker reuses the mergeable-branches endpoint (local + origin).
    let branches_res = {
        let sid = story_id.clone();
        use_resource(move || {
            let sid = sid.clone();
            let _dep = uow_refresh();
            async move { fetch_uow_branches(&sid).await }
        })
    };
    let branches = branches_res.read().clone().unwrap_or_default();

    // The selected base branch for opening the PR (empty → server default branch).
    let mut base_branch = use_signal(String::new);
    // The pulled PR info (None until "Pull PR info" runs).
    let mut pr_info = use_signal(|| Option::<PrInfoResult>::None);
    // Busy flags + the resolve run's own active-run signal.
    let mut opening = use_signal(|| false);
    let mut pulling = use_signal(|| false);
    let mut resolving = use_signal(|| false);
    let active_run = use_signal(|| Option::<RunView>::None);
    // The PR comment composer.
    let mut pr_comment = use_signal(String::new);
    let mut commenting = use_signal(|| false);
    // The model for the gated resolve agent (default = project strongest, editable).
    let resolve_model = use_signal(String::new);

    if !has_branch {
        return rsx! {
            div { class: "uow-step-control uow-pr-panel",
                p { class: "uow-step-h", "Pull request" }
                p { class: "section-hint",
                    "This UoW has no branch yet — start development first, then open a PR for its branch."
                }
            }
        };
    }

    rsx! {
        div { class: "uow-step-control uow-pr-panel",
            p { class: "uow-step-h", "Pull request" }
            p { class: "section-hint",
                "Push this UoW's branch and open a PR, pull its state / CI / comments, resolve feedback with a gated agent, or comment."
            }

            // ── The stored PR (number + link), once known ──────────────────────
            {
                let info = pr_info.read().clone();
                match info.as_ref().and_then(|r| r.pr.clone()) {
                    Some(pr) => rsx! {
                        div { class: "uow-pr-head",
                            span { class: "uow-pr-num", "PR #{pr.number}" }
                            span { class: "uow-pr-state", "{pr.state}" }
                            span { class: "section-hint", "{pr.base_branch} ← {pr.head_branch}" }
                            if !pr.url.is_empty() {
                                a { class: "wi-detail-link", href: "{pr.url}", target: "_blank", "Open PR ↗" }
                            }
                        }
                    },
                    None => rsx! {},
                }
            }

            // ── Push & open PR (with a base-branch picker) ─────────────────────
            div { class: "run-control-row",
                select {
                    class: "uow-branch-select",
                    value: "{base_branch}",
                    onchange: move |e| base_branch.set(e.value()),
                    option { value: "", selected: base_branch().is_empty(), "Default branch" }
                    if !branches.local.is_empty() {
                        optgroup { label: "Local",
                            for b in branches.local.iter() {
                                option { key: "base-local:{b}", value: "{b}", "{b}" }
                            }
                        }
                    }
                    if !branches.origin.is_empty() {
                        optgroup { label: "Origin",
                            for b in branches.origin.iter() {
                                option { key: "base-origin:{b}", value: "{b}", "{b}" }
                            }
                        }
                    }
                }
                button {
                    class: "btn-run",
                    disabled: opening(),
                    onclick: move |_| {
                        let sid = sid_open.clone();
                        let base = base_branch();
                        opening.set(true);
                        spawn(async move {
                            match open_uow_pr(&sid, &base).await {
                                OpenPrOutcome::Opened(n, url) => {
                                    crate::toast::push_toast(
                                        toasts,
                                        crate::toast::ToastKind::Info,
                                        format!("Opened PR #{n}: {url}"),
                                    );
                                    uow_refresh += 1;
                                    // Pull fresh info so the head + link render immediately.
                                    if let Some(r) = Some(fetch_uow_pr(&sid).await) {
                                        pr_info.set(Some(r));
                                    }
                                }
                                OpenPrOutcome::Blocked(reason) => crate::toast::push_toast(
                                    toasts, crate::toast::ToastKind::Warning, reason,
                                ),
                                OpenPrOutcome::Failed => crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Warning,
                                    "Could not open the PR.".to_string(),
                                ),
                            }
                            opening.set(false);
                        });
                    },
                    if opening() { "Opening…" } else { "Push & open PR" }
                }
                button {
                    class: "btn-secondary",
                    disabled: pulling(),
                    onclick: move |_| {
                        let sid = sid_pull.clone();
                        pulling.set(true);
                        spawn(async move {
                            let r = fetch_uow_pr(&sid).await;
                            pr_info.set(Some(r));
                            pulling.set(false);
                        });
                    },
                    if pulling() { "Pulling…" } else { "Pull PR info" }
                }
            }

            // ── PR state + CI checks + comments ────────────────────────────────
            {
                let info = pr_info.read().clone();
                match info {
                    None => rsx! {},
                    Some(r) if r.pr.is_none() => rsx! {
                        p { class: "section-hint",
                            if r.message.is_empty() { "No PR for this UoW yet." } else { "{r.message}" }
                        }
                    },
                    Some(r) => {
                        let checks = r.checks.clone().unwrap_or_default();
                        let no_checks = checks.passed == 0 && checks.failed == 0 && checks.pending == 0;
                        rsx! {
                            div { class: "uow-pr-checks",
                                if no_checks {
                                    span { class: "section-hint", "No CI checks reported." }
                                } else {
                                    span { class: "uow-pr-check pass", "✓ {checks.passed} passed" }
                                    span { class: "uow-pr-check fail", "✗ {checks.failed} failed" }
                                    span { class: "uow-pr-check pending", "● {checks.pending} pending" }
                                    if !checks.failing.is_empty() {
                                        span { class: "section-hint", "Failing: {checks.failing.join(\", \")}" }
                                    }
                                }
                            }
                            div { class: "uow-pr-comments",
                                if r.comments.is_empty() {
                                    p { class: "section-hint", "No comments." }
                                } else {
                                    for (i, c) in r.comments.iter().enumerate() {
                                        div { key: "pr-c-{i}", class: "wi-comment",
                                            span { class: "wi-comment-author",
                                                "{c.author}"
                                                if c.review { span { class: "section-hint", " · review" } }
                                            }
                                            span { class: "wi-comment-date", "{c.created_at}" }
                                            p { class: "wi-comment-body", "{c.body}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── Resolve PR feedback with a gated agent ─────────────────────────
            div { class: "run-control-row",
                ModelSelect { models: models.clone(), selected: resolve_model }
                button {
                    class: "btn-run",
                    disabled: resolving(),
                    onclick: move |_| {
                        let sid = sid_resolve.clone();
                        let md = resolve_model();
                        resolving.set(true);
                        spawn(async move {
                            match start_pr_resolve_run(&sid, &md).await {
                                StartRunOutcome::Started(rid) => {
                                    poll_run_to_done(rid, active_run, uow_refresh).await;
                                }
                                StartRunOutcome::Blocked(reason) => crate::toast::push_toast(
                                    toasts, crate::toast::ToastKind::Warning, reason,
                                ),
                                StartRunOutcome::Failed => crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Warning,
                                    "Could not start the PR-resolve run.".to_string(),
                                ),
                            }
                            resolving.set(false);
                        });
                    },
                    if resolving() { "Resolving…" } else { "▶ Resolve with agent (gated)" }
                }
            }
            p { class: "section-hint",
                "Feeds open review comments + failing check names to ONE governed agent (same gate as the dev run) to fix, commit, and push."
            }
            // The gated resolve run's live activity, when running.
            {
                let rid = match active_run() {
                    Some(ref r) => r.id.clone(),
                    None => String::new(),
                };
                rsx! { crate::agent_activity::AgentActivity { run_id: rid } }
            }

            // ── Add a comment to the PR ────────────────────────────────────────
            div { class: "uow-comment",
                p { class: "clarify-h", "Add comment to PR" }
                textarea {
                    class: "clarify-q",
                    value: "{pr_comment}",
                    rows: "3",
                    placeholder: "Write a comment to post on the pull request…",
                    oninput: move |e| pr_comment.set(e.value()),
                }
                button {
                    class: "btn-run",
                    disabled: commenting(),
                    onclick: move |_| {
                        let sid = sid_comment.clone();
                        let body = pr_comment();
                        if body.trim().is_empty() {
                            return;
                        }
                        commenting.set(true);
                        spawn(async move {
                            match comment_on_uow_pr(&sid, &body).await {
                                Some(_url) => {
                                    pr_comment.set(String::new());
                                    crate::toast::push_toast(
                                        toasts,
                                        crate::toast::ToastKind::Info,
                                        "Comment posted to the PR.".to_string(),
                                    );
                                }
                                None => crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Warning,
                                    "Could not post the PR comment.".to_string(),
                                ),
                            }
                            commenting.set(false);
                        });
                    },
                    if commenting() { "Posting…" } else { "Post PR comment" }
                }
            }
        }
    }
}

/// The decisions-review surface, shown at the Investigating stage. It makes the
/// otherwise-invisible investigation output actionable:
///
/// - Renders the investigation NOTE (Markdown). When the note is the token-free
///   placeholder (live mode off) or absent, it shows an EXPLICIT "no output" state with
///   the reason — never a silent Investigating.
/// - Lists the proposed DECISION records with per-decision Approve / Reject controls
///   (each POSTs the full updated set to `/decisions`, matching the server's shape).
/// - Lets the architect ADD a decision manually (needed when the agent surfaced none, so
///   the development gate — which requires ≥1 approved decision — can be satisfied).
/// - A "Mark investigation reviewed" control (ROUTE-B).
///
/// Once the note is reviewed AND every decision is approved, the architect uses the
/// existing "Approve decisions" transition (rendered by [`UowStepRunControls`]) to advance
/// to DecisionsApproved. This panel keeps the data model consistent: it always POSTs the
/// complete decision set the server stores.
#[component]
pub(super) fn DecisionsReviewPanel(story_id: String, uow_refresh: Signal<u32>) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();

    // Local refresh tick so an approve/reject/add re-reads without bouncing the whole UoW.
    let local_refresh = use_signal(|| 0u32);
    let review_res = {
        let sid = story_id.clone();
        use_resource(move || {
            let sid = sid.clone();
            let _dep = local_refresh();
            // Also re-read when the parent UoW refreshes (e.g. after the run completes).
            let _dep2 = uow_refresh();
            async move { fetch_investigation_review(&sid).await }
        })
    };
    let review = review_res.read().clone().flatten().unwrap_or_default();

    let note = review.note.clone();
    let note_text = note.as_ref().map(|n| n.note.clone()).unwrap_or_default();
    let reviewed = note.as_ref().map(|n| n.reviewed).unwrap_or(false);
    let no_real_output = !review.note_present || is_placeholder_note(&note_text);
    let note_html = crate::md::md_to_html(&note_text);
    let decisions = review.decisions.clone();
    let all_approved =
        !decisions.is_empty() && decisions.iter().all(|d| d.outcome.is_approved());

    // New-decision composer state.
    let mut new_label = use_signal(String::new);
    let mut new_question = use_signal(String::new);
    let mut new_rationale = use_signal(String::new);
    let mut busy = use_signal(|| false);

    rsx! {
        div { class: "uow-decisions-review",
            p { class: "uow-step-h", "Investigation & decisions" }

            // ── Investigation note ─────────────────────────────────────────────
            if no_real_output {
                div { class: "uow-investigation-empty",
                    p { class: "uow-investigation-empty-h", "Investigation produced no output" }
                    p { class: "section-hint",
                        if review.note_present {
                            "The investigation ran without a live agent (live mode is off), so it recorded a placeholder instead of a real analysis. Enable CAMERATA_LIVE_BUILD=1 and re-run Begin investigation for a real note — or record the decisions below manually to proceed."
                        } else {
                            "No investigation note has been recorded yet. If the run just finished, give it a moment and refresh; otherwise record the decisions below manually to proceed."
                        }
                    }
                }
            } else {
                div { class: "uow-investigation-note",
                    div { class: "uow-investigation-note-head",
                        span { class: "uow-field-label", "Investigation note" }
                        if reviewed {
                            span { class: "wi-state done", "REVIEWED" }
                        } else {
                            span { class: "wi-state neutral", "UNREVIEWED" }
                        }
                    }
                    div { class: "chat-md", dangerous_inner_html: "{note_html}" }
                    if !reviewed {
                        button {
                            class: "btn-secondary",
                            disabled: busy(),
                            onclick: {
                                let sid = story_id.clone();
                                move |_| {
                                    let sid = sid.clone();
                                    let mut local_refresh = local_refresh;
                                    let toasts = toasts;
                                    busy.set(true);
                                    spawn(async move {
                                        if mark_investigation_reviewed(&sid).await {
                                            local_refresh += 1;
                                        } else {
                                            crate::toast::push_toast(
                                                toasts,
                                                crate::toast::ToastKind::Warning,
                                                "Could not mark the note reviewed (it may already be reviewed).".to_string(),
                                            );
                                        }
                                        busy.set(false);
                                    });
                                }
                            },
                            "Mark investigation reviewed"
                        }
                    }
                }
            }

            // ── Decisions ──────────────────────────────────────────────────────
            div { class: "uow-decisions",
                p { class: "uow-field-label", "Decisions ({decisions.len()})" }
                if decisions.is_empty() {
                    p { class: "section-hint",
                        "No decisions recorded yet. The development gate requires at least one APPROVED decision. Add one below."
                    }
                }
                for (i, d) in decisions.iter().enumerate() {
                    {
                        let (olabel, ocss) = (d.outcome.label(), d.outcome.css());
                        let decisions_for_approve = decisions.clone();
                        let decisions_for_reject = decisions.clone();
                        let sid_a = story_id.clone();
                        let sid_r = story_id.clone();
                        rsx! {
                            div { key: "{d.artifact_id}", class: "uow-decision-card",
                                div { class: "uow-decision-head",
                                    span { class: "uow-decision-label", "{d.label}" }
                                    span { class: "wi-state {ocss}", "{olabel}" }
                                }
                                if !d.question.is_empty() {
                                    p { class: "uow-decision-q", "Q: {d.question}" }
                                }
                                if !d.rationale.is_empty() {
                                    p { class: "uow-decision-rationale", "{d.rationale}" }
                                }
                                if let DecisionOutcomeView::Rejected { reason } = &d.outcome {
                                    if !reason.is_empty() {
                                        p { class: "uow-decision-reject-reason", "Rejected: {reason}" }
                                    }
                                }
                                div { class: "uow-decision-actions",
                                    if !d.outcome.is_approved() {
                                        button {
                                            class: "btn-secondary",
                                            disabled: busy(),
                                            onclick: move |_| {
                                                let sid = sid_a.clone();
                                                let mut updated = decisions_for_approve.clone();
                                                updated[i].outcome = DecisionOutcomeView::Approved;
                                                let mut local_refresh = local_refresh;
                                                let toasts = toasts;
                                                busy.set(true);
                                                spawn(async move {
                                                    if post_decisions(&sid, &updated).await {
                                                        local_refresh += 1;
                                                    } else {
                                                        crate::toast::push_toast(
                                                            toasts,
                                                            crate::toast::ToastKind::Warning,
                                                            "Could not approve the decision.".to_string(),
                                                        );
                                                    }
                                                    busy.set(false);
                                                });
                                            },
                                            "Approve"
                                        }
                                    }
                                    if d.outcome.is_approved() {
                                        button {
                                            class: "btn-secondary",
                                            disabled: busy(),
                                            onclick: move |_| {
                                                let sid = sid_r.clone();
                                                let mut updated = decisions_for_reject.clone();
                                                updated[i].outcome = DecisionOutcomeView::Rejected {
                                                    reason: "Needs changes".to_string(),
                                                };
                                                let mut local_refresh = local_refresh;
                                                let toasts = toasts;
                                                busy.set(true);
                                                spawn(async move {
                                                    if post_decisions(&sid, &updated).await {
                                                        local_refresh += 1;
                                                    } else {
                                                        crate::toast::push_toast(
                                                            toasts,
                                                            crate::toast::ToastKind::Warning,
                                                            "Could not update the decision.".to_string(),
                                                        );
                                                    }
                                                    busy.set(false);
                                                });
                                            },
                                            "Needs changes"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // ── Add a decision manually ─────────────────────────────────────
                div { class: "uow-decision-add",
                    p { class: "uow-field-label", "Add a decision" }
                    input {
                        class: "uow-decision-input",
                        r#type: "text",
                        placeholder: "Label (e.g. Auth strategy: JWT vs session)",
                        value: "{new_label}",
                        oninput: move |e| new_label.set(e.value()),
                    }
                    input {
                        class: "uow-decision-input",
                        r#type: "text",
                        placeholder: "Question / ambiguity",
                        value: "{new_question}",
                        oninput: move |e| new_question.set(e.value()),
                    }
                    textarea {
                        class: "uow-decision-input",
                        rows: 2,
                        placeholder: "Rationale / chosen option",
                        value: "{new_rationale}",
                        oninput: move |e| new_rationale.set(e.value()),
                    }
                    button {
                        class: "btn-run",
                        disabled: busy() || new_label().trim().is_empty(),
                        onclick: {
                            let sid = story_id.clone();
                            let existing = decisions.clone();
                            move |_| {
                                let sid = sid.clone();
                                let label = new_label().trim().to_string();
                                if label.is_empty() { return; }
                                // Build the new record, mirroring the server's DecisionRecord
                                // shape. The artifact_id follows the documented convention
                                // "{story_id}/decision/{slug}". Architect-authored, so it
                                // starts Approved (the architect is recording a settled call).
                                let slug = slugify_decision_label(&label);
                                let rec = DecisionRecordView {
                                    artifact_id: format!("{sid}/decision/{slug}"),
                                    story_id: sid.clone(),
                                    label,
                                    question: new_question().trim().to_string(),
                                    rationale: new_rationale().trim().to_string(),
                                    alternatives_considered: Vec::new(),
                                    outcome: DecisionOutcomeView::Approved,
                                    provenance: RevisionProvenanceView::default(),
                                };
                                let mut updated = existing.clone();
                                updated.push(rec);
                                let mut local_refresh = local_refresh;
                                let toasts = toasts;
                                busy.set(true);
                                spawn(async move {
                                    if post_decisions(&sid, &updated).await {
                                        new_label.set(String::new());
                                        new_question.set(String::new());
                                        new_rationale.set(String::new());
                                        local_refresh += 1;
                                    } else {
                                        crate::toast::push_toast(
                                            toasts,
                                            crate::toast::ToastKind::Warning,
                                            "Could not add the decision.".to_string(),
                                        );
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                        "Add decision (approved)"
                    }
                }

                // ── Readiness hint for the Approve-decisions transition ─────────
                {
                    let ready = reviewed && all_approved && !no_real_output;
                    let ready_placeholder = reviewed_for_placeholder(reviewed, no_real_output) && all_approved;
                    if ready || ready_placeholder {
                        rsx! {
                            p { class: "uow-decisions-ready",
                                "Ready — use \"Approve decisions\" below to advance to Decisions approved."
                            }
                        }
                    } else {
                        rsx! {
                            p { class: "section-hint",
                                "To advance: every decision must be Approved"
                                if !no_real_output { " and the investigation note marked reviewed" }
                                "."
                            }
                        }
                    }
                }
            }
        }
    }
}

/// kebab-case slug for a decision label (alnum runs → hyphens, lowercased, trimmed).
/// Mirrors the `"{story_id}/decision/{slug}"` artifact-id convention. Pure + testable.
pub(super) fn slugify_decision_label(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut prev_dash = false;
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "decision".to_string()
    } else {
        trimmed
    }
}

/// Whether the note-review requirement is satisfied for the readiness hint. When the
/// investigation produced no real output (placeholder / absent), there is no meaningful
/// note to review, so the note-review requirement is treated as satisfied and only the
/// decision gate governs readiness. Pure.
pub(super) fn reviewed_for_placeholder(reviewed: bool, no_real_output: bool) -> bool {
    reviewed || no_real_output
}

/// The lifecycle strip + the run control for the CURRENT phase, rendered inline with
/// the steps (Increment 1). Runs live ON THE STEPS: the control shown is the one for
/// the active stage and it REPLACES the prior phase's control rather than stacking.
///
/// - **Intake** → a single model `<select>` (default = project strongest, editable)
///   beside a **Begin investigation** button that calls `begin_investigation_run` and
///   then drives the live agent activity on the returned run. The server transitions
///   the stage Intake → Investigating.
/// - **Investigating** → the architect's **Approve decisions** transition
///   (Investigating → DecisionsApproved), which the server gates on the story's
///   decision records and 409s (with a precise reason) if not all are approved.
/// - **DecisionsApproved** → three per-tier model `<select>`s (Strongest / Balanced /
///   Fast, defaulted from the project tier map, editable for this run) beside a
///   **Run development (governed)** button that calls `start_dev_run` with the tier
///   map. The strongest tier leads and delegates simpler work to the others.
///
/// Later stages (`Development`, `AwaitingQa`, `SignedOff`) are engine-driven — set by
/// the gated run, its provenance watcher, and the explicit sign-off — so no run
/// control is shown for them here. A blocked transition or run raises a toast carrying
/// the server's reason.
#[component]
pub(super) fn UowStepRunControls(
    story_id: String,
    /// The KNOWN lifecycle stage, or `None` while the UoW fetch is still loading / has
    /// failed. The run control renders ONLY when the stage is known, so a stale Intake
    /// button can never appear over a UoW that is actually past Intake (the cause of the
    /// spurious "Could not begin the investigation run." 409 toast).
    stage: Option<UowStage>,
    uow_refresh: Signal<u32>,
    active_run: Signal<Option<RunView>>,
    models: Option<AuditModelsResp>,
    invest_model: Signal<String>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();

    let sid_begin = story_id.clone();

    // Busy flag for "Begin investigation": disables the button + shows the Bombe while the
    // begin request is in flight, so a double-click can't fire a SECOND begin (the first
    // already advanced the stage server-side, and the second would 409).
    let mut starting = use_signal(|| false);

    rsx! {
        // ── Begin investigation: model select + run button ─────────────────────
        // Shown only in the Investigation & Refinement phase (§4.1). Nothing renders
        // until the stage is known so a stale button cannot fire a 409.
        match stage {
            None => rsx! {
                div { class: "uow-step-control",
                    p { class: "section-hint", "Loading…" }
                }
            },
            Some(UowStage::Intake) => rsx! {
                div { class: "uow-step-control",
                    p { class: "uow-step-h", "Investigation" }
                    div { class: "run-control-row",
                        button {
                            class: "btn-run",
                            disabled: starting(),
                            onclick: move |_| {
                                // Guard: ignore re-clicks while a begin is already in flight.
                                if starting() {
                                    return;
                                }
                                starting.set(true);
                                let sid = sid_begin.clone();
                                let md = invest_model();
                                let mut uow_refresh = uow_refresh;
                                let mut starting = starting;
                                spawn(async move {
                                    // Loading guard: Bombe machine runs while the investigation is in flight.
                                    let _guard = crate::loading::LoadingGuard::new();
                                    match begin_investigation_run(&sid, &md).await {
                                        crate::cockpit::BeginInvestigationOutcome::Started(rid) => {
                                            // The server advances Intake → Investigating BEFORE it
                                            // returns, so refresh the lifecycle IMMEDIATELY — the
                                            // stage + control flip to Investigating right now, not
                                            // when the run finishes. A live-off placeholder run
                                            // completes instantly with nothing to stream; without
                                            // this immediate bump the stale Intake button stayed up
                                            // and a second click 409'd. Then stream the run.
                                            uow_refresh += 1;
                                            poll_run_to_done(rid, active_run, uow_refresh).await;
                                        }
                                        // The UoW was not at Intake (e.g. a prior begin already
                                        // advanced it but the displayed button was stale). Surface
                                        // the server's precise reason AND refresh so the now-correct
                                        // control replaces the stale "Begin investigation" button.
                                        crate::cockpit::BeginInvestigationOutcome::Blocked(reason) => {
                                            uow_refresh += 1;
                                            crate::toast::push_toast(
                                                toasts,
                                                crate::toast::ToastKind::Warning,
                                                reason,
                                            );
                                        }
                                        crate::cockpit::BeginInvestigationOutcome::Failed => {
                                            crate::toast::push_toast(
                                                toasts,
                                                crate::toast::ToastKind::Warning,
                                                "Could not begin the investigation run.".to_string(),
                                            );
                                        }
                                    }
                                    starting.set(false);
                                });
                            },
                            if starting() {
                                span { "Starting\u{2026}" }
                            } else {
                                "\u{25b6} Begin investigation"
                            }
                        }
                        ModelSelect { models: models.clone(), selected: invest_model }
                    }
                    p { class: "section-hint", "Runs an investigation pass, then advances the stage to Investigating." }
                }
            },
            // All other stages (Investigating, DecisionsApproved, Development, …): no
            // investigation run control is shown here — the investigation is already
            // underway or complete.
            Some(_) => rsx! {},
        }
    }
}

#[component]
pub(super) fn UowPanel(story_id: String, uow_refresh: Signal<u32>) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let sid = story_id.clone();
    let uow_data = use_resource(move || {
        let sid = sid.clone();
        // Re-fetch when the shared tick bumps (e.g. after a sign-off) so the panel
        // reflects the latest sign-off / history without a manual reload.
        let _dep = uow_refresh();
        async move { fetch_uow(&sid).await }
    });

    let uow = uow_data.read().clone().flatten();
    let dev_status = uow.as_ref().map(|u| u.dev_status).unwrap_or_default();
    let branch = uow.as_ref().and_then(|u| u.branch.clone());
    let history = uow.as_ref().map(|u| u.history.clone()).unwrap_or_default();
    let sign_off = uow.as_ref().and_then(|u| u.sign_off.clone());
    let gate_provenance = uow.as_ref().and_then(|u| u.gate_provenance.clone());

    // The three status options for the segmented control.
    const STATUS_OPTS: &[DevStatus] = &[DevStatus::New, DevStatus::InProgress, DevStatus::Done];

    rsx! {
        div { class: "uow-panel",
            p { class: "uow-panel-h", "UNIT OF WORK" }

            // The governed-development lifecycle strip + per-phase run controls now
            // live with the steps in `UowStepRunControls` (rendered above this panel by
            // `UowDevControls`), so runs sit ON the steps. This panel keeps the
            // post-run read-out: dev status, branch, gate provenance, sign-off, history.

            // ── Dev status: 3-state segmented control ──────────────────────────
            div { class: "uow-status-row",
                span { class: "uow-field-label", "Dev status" }
                div { class: "uow-seg",
                    for opt in STATUS_OPTS.iter().copied() {
                        {
                            let sid = story_id.clone();
                            let active = opt == dev_status;
                            let cls = if active { "uow-seg-btn active" } else { "uow-seg-btn" };
                            rsx! {
                                button {
                                    class: "{cls}",
                                    onclick: move |_| {
                                        let sid = sid.clone();
                                        let mut uow_refresh = uow_refresh;
                                        let toasts = toasts;
                                        spawn(async move {
                                            if post_uow_status(&sid, opt).await.is_some() {
                                                // Bump both: the panel re-fetches its own UoW,
                                                // and the spine badges refresh via the map.
                                                uow_refresh += 1;
                                            } else {
                                                crate::toast::push_toast(
                                                    toasts,
                                                    crate::toast::ToastKind::Warning,
                                                    "Could not update dev status.".to_string(),
                                                );
                                            }
                                        });
                                    },
                                    "{opt.label()}"
                                }
                            }
                        }
                    }
                }
            }

            // ── Branch ref (read-only; auto-populated by the governed run) ─────
            div { class: "uow-branch-row",
                span { class: "uow-field-label", "Branch" }
                if let Some(ref b) = branch {
                    span { class: "uow-branch-val", "{b}" }
                } else {
                    span { class: "uow-branch-none", "not set" }
                }
            }

            // ── Frozen gate provenance (Pillar 2): the durable QA-review record ─
            // Stamped onto the UoW when a governed run finishes, so the honest gate
            // accounting survives even after the in-memory run is gone.
            if let Some(ref p) = gate_provenance {
                div { class: "uow-provenance",
                    span { class: "uow-field-label", "Gate provenance" }
                    span { class: "uow-provenance-val",
                        "run {p.run_id} ({p.mode}) — {p.allow_count} allowed, {p.deny_count} denied ({p.total_bounces} bounces)"
                    }
                    if !p.rules_fired.is_empty() {
                        span { class: "uow-provenance-rules",
                            "Bounced: {p.rules_fired.join(\", \")}"
                        }
                    }
                }
            }

            // ── Sign-off (issue #21): the architect's explicit approval, if any ─
            div { class: "uow-signoff-row",
                span { class: "uow-field-label", "Sign-off" }
                if let Some(ref so) = sign_off {
                    span { class: "uow-signoff-val", "✓ {so.by} · run {so.run_id} · {so.ts}" }
                } else {
                    span { class: "uow-signoff-none", "not signed off" }
                }
            }

            // ── AI development history ─────────────────────────────────────────
            div { class: "uow-history",
                p { class: "uow-history-h", "AI history" }
                if history.is_empty() {
                    p { class: "uow-history-empty", "No history yet — the governed run will append entries here." }
                } else {
                    div { class: "uow-history-list",
                        for entry in history.iter() {
                            div { class: "uow-history-row",
                                span { class: "uow-hist-ts", "{entry.ts}" }
                                span { class: "uow-hist-kind", "{entry.kind}" }
                                span { class: "uow-hist-text", "{entry.text}" }
                            }
                        }
                    }
                }
            }
        }
    }
}
