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

/// Send a cancel request for a dev run. Fire-and-forget: 204 = success; any other
/// status or a network error is treated as benign (the run may already be done).
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

/// Sign off a run (issue #21). The architect's explicit gate after reviewing the
/// provenance; persists on the story's UoW. Returns the updated UoW on success.
pub(super) async fn sign_off_run(run_id: &str, by: &str, note: Option<&str>) -> Option<UowView> {
    reqwest::Client::new()
        .post(format!("{}/api/runs/{}/sign-off", crate::BFF_URL, run_id))
        .json(&serde_json::json!({ "by": by, "note": note }))
        .send()
        .await
        .ok()?
        .json::<UowView>()
        .await
        .ok()
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
/// the updated authoring UoW (with the refreshed draft + chat).
pub(super) async fn post_author_message(story_id: &str, message: &str) -> Option<AuthoringUowView> {
    reqwest::Client::new()
        .post(format!("{}/api/uow/{}/author", crate::BFF_URL, enc_seg(story_id)))
        .json(&serde_json::json!({ "message": message }))
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
    let uows_res = use_resource(move || {
        let _dep = uows_refresh();
        async move { fetch_uows().await }
    });

    let uows = uows_res.read().clone().flatten().unwrap_or_default();

    rsx! {
        div { class: "govdev",
            // ── LEFT NAV: Issue Management + one card per UoW ──────────────────
            aside { class: "govdev-nav",
                // Gear button: opens the project-settings popup (loop guard + tier map).
                // Lives at the top of the nav so it's always reachable regardless of UoW selection.
                div { class: "govdev-gear-row",
                    ProjectSettingsGear {}
                }
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
                            rsx! {
                                button {
                                    class: "{cls}",
                                    onclick: move |_| sel.set(GovDevSel::Uow(uid.clone())),
                                    span { class: "govdev-uow-title", "{title}" }
                                    div { class: "govdev-uow-meta",
                                        if !repo.is_empty() {
                                            span { class: "govdev-uow-repo", "{repo}" }
                                        }
                                        span { class: "govdev-uow-stage", "{stage}" }
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
/// `lvl{max_depth}`) followed by the visible data columns (Repo, #, Title, State,
/// Labels). The hierarchy columns drive `set_grouping` and have a minimal initial
/// width since Chorale renders them as group headers, not data columns.
///
/// `max_depth` must match the value used in `build_work_item_rows`; both are
/// derived from the same item list inside `WorkItemTable`.
pub(super) fn work_item_columns(max_depth: usize) -> Vec<ColumnDef<WorkItemRow>> {
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
        .filter(FilterKind::Text)
        .initial_width(180.0),
        ColumnDef::new(ColumnId("num"), "#", |r: &WorkItemRow| {
            CellValue::Text(format!("#{}", r.work_item.number))
        })
        .sortable()
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
    let (rows, max_depth): (Vec<(RowId, WorkItemRow)>, usize) = use_hook({
        let items = items.clone();
        move || {
            let built = build_work_item_rows(&items);
            let max_depth = built
                .first()
                .map(|r| r.hierarchy_cols.len().saturating_sub(1))
                .unwrap_or(0);
            let rows = built
                .into_iter()
                .map(|r| (RowId::new(), r))
                .collect();
            (rows, max_depth)
        }
    });
    let id_map: std::collections::HashMap<RowId, String> =
        rows.iter().map(|(r, row)| (*r, row.work_item.id.clone())).collect();
    let handle = use_table(move || TableState::new(rows.clone(), work_item_columns(max_depth)));
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
/// An optional "Parent ID" text input allows the architect to set a parent GitHub issue
/// number (e.g. "42" or "#42"). When set, the draft UoW carries it through to publish
/// time, where a native GitHub sub-issue link is created. Empty → no parent.
#[component]
pub(super) fn NewAuthoredUowButton(uows_refresh: Signal<u32>, sel: Signal<GovDevSel>) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut working = use_signal(|| false);
    // The parent issue number typed by the architect before hitting "New Unit of Work".
    let mut parent_id = use_signal(String::new);
    rsx! {
        div { class: "govdev-new-uow-area",
            // Parent ID input: optional, sits just above the action button.
            div { class: "govdev-parent-id-row",
                label {
                    r#for: "new-uow-parent-id",
                    class: "govdev-parent-id-label",
                    "Parent ID (optional)"
                }
                input {
                    id: "new-uow-parent-id",
                    class: "govdev-parent-id-input",
                    r#type: "text",
                    placeholder: "e.g. 42 or #42",
                    disabled: working(),
                    value: "{parent_id}",
                    oninput: move |e| parent_id.set(e.value()),
                }
            }
            button {
                class: "govdev-nav-top",
                disabled: working(),
                onclick: move |_| {
                    let mut sel = sel;
                    let mut uows_refresh = uows_refresh;
                    let toasts = toasts;
                    // Capture the current parent_id value before entering the async block.
                    let pid_val = parent_id().trim().to_string();
                    let pid = if pid_val.is_empty() { None } else { Some(pid_val) };
                    working.set(true);
                    spawn(async move {
                        match create_blank_uow(pid).await {
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
    let st = state_res
        .read()
        .clone()
        .flatten()
        .and_then(|u| u.authoring)
        .unwrap_or_default();

    let mut message = use_signal(String::new);
    let mut sending = use_signal(|| false);
    let mut publishing = use_signal(|| false);
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
                                            let _ = post_author_message(&id, &answer_text).await;
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
                            sending.set(true);
                            spawn(async move {
                                match post_author_message(&id, &msg).await {
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
                            });
                        }
                    },
                    if sending() { "Drafting…" } else { "Send" }
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

/// The dev controls for a selected Unit of Work. Reuses the EXISTING governed-dev
/// mechanisms — run the governed fleet THROUGH THE GATE, the clarify back-and-forth, and
/// sign-off — keyed to this UoW's id (the same key the existing endpoints use). Adds an
/// "Add comment to issue" box (`POST /api/workitems/comment`) and a "Pull latest work item"
/// button (`POST /api/workitems/refresh`). Provider-agnostic: it only reads the WorkItem DTO.
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

    // Increment 1: runs live ON THE STEPS, not a standalone button. We fetch the UoW
    // here (keyed on the same refresh tick the panel uses) so we know the current
    // lifecycle `stage` and can render the run control for the ACTIVE phase inline with
    // the lifecycle strip. The downstream `UowPanel` re-fetches the same UoW for its own
    // read-out, so the two stay in sync without sharing a fetch.
    let uow_for_stage = {
        let sid = uow.id.clone();
        use_resource(move || {
            let sid = sid.clone();
            let _dep = uow_refresh();
            async move { fetch_uow(&sid).await }
        })
    };
    let stage = uow_for_stage
        .read()
        .clone()
        .flatten()
        .map(|u| u.stage)
        .unwrap_or_default();

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
                dev_balanced.set(tm.balanced.clone());
            }
            if dev_fast.peek().is_empty() {
                dev_fast.set(tm.fast.clone());
            }
        }));
    }

    // Comment-to-issue composer (+ GitHub-style @-mention autocomplete).
    let mut comment_body = use_signal(String::new);
    let mut commenting = use_signal(|| false);
    // Pull-latest state.
    let mut refreshing = use_signal(|| false);

    // ── Work-item modal (opened from inside the UoW) ───────────────────────────
    // A local flag toggles the WorkItemDetail modal for THIS UoW's work item. The
    // modal's create/open-UoW action is hidden (the UoW already exists), so the
    // uows / sel / uows_refresh it requires are local throwaways here.
    let mut wi_modal_open = use_signal(|| false);
    let modal_uows_refresh = use_signal(|| 0u32);
    let modal_sel = use_signal(|| GovDevSel::IssueManagement);

    // ── @-mention autocomplete state ───────────────────────────────────────────
    // The repo's assignable users, fetched once per work item (the practical mention
    // set). Degrades to empty (no token / error) → the dropdown simply never shows.
    let assignees_res = {
        let wid = uow.work_item.clone().unwrap_or_default().id;
        use_resource(move || {
            let wid = wid.clone();
            async move { fetch_work_item_assignees(&wid).await }
        })
    };
    let assignees = assignees_res.read().clone().unwrap_or_default();
    // Whether the dropdown is showing (an active `@token` exists and matches).
    let mut mention_open = use_signal(|| false);

    let it = item.read().clone();
    let (state_label, state_cls) = work_item_state_badge(&it.state);

    rsx! {
        div { class: "uow-dev",
            // ── Work-item header (provider-agnostic read of the DTO) ───────────
            div { class: "uow-dev-head",
                span { class: "uow-dev-repo", "{it.repo}" }
                span { class: "uow-dev-num", "#{it.number}" }
                span { class: "wi-state {state_cls}", "{state_label}" }
                // Open the full work-item modal (title + body + ALL comments) in-app.
                button {
                    class: "btn-edit-sm",
                    onclick: move |_| wi_modal_open.set(true),
                    "Open work item"
                }
                // RETAINED: the direct link to the issue on the tracker.
                if !it.url.is_empty() {
                    a { class: "wi-detail-link", href: "{it.url}", target: "_blank", "Open issue ↗" }
                }
            }
            p { class: "uow-dev-title", "{it.title}" }

            // The work-item modal (with comments), opened from the head button above.
            // Its create/open-UoW action is hidden — this UoW already exists.
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

            // ── Pull latest work item ─────────────────────────────────────────
            div { class: "uow-dev-pull-row",
                button {
                    class: "btn-edit-sm",
                    disabled: refreshing(),
                    onclick: move |_| {
                        let wid = item.read().id.clone();
                        refreshing.set(true);
                        spawn(async move {
                            if let Some(updated) = refresh_work_item(&wid).await {
                                item.set(updated);
                            }
                            refreshing.set(false);
                        });
                    },
                    if refreshing() { "Pulling…" } else { "Pull latest work item" }
                }
                span { class: "section-hint", "Re-pull this issue from the tracker." }
            }

            // ── Gate self-check (reused) ──────────────────────────────────────
            GateSelfCheck {}

            // NOTE: Loop guard (max revise iterations) is a PROJECT-level setting and
            // has been moved to the gear-icon project-settings popup in GovernedDevPage.
            // It no longer lives here (per-UoW) to avoid implying it is per-UoW state.

            // ── Lifecycle steps with the ACTIVE phase's run control inline ────
            // Increment 1: runs live ON THE STEPS. The lifecycle strip shows the
            // ordered stages + the architect transitions ("Approve decisions"), and
            // the run control for the current phase is rendered inline beneath it —
            // it REPLACES the prior phase's control rather than stacking. The
            // investigation control owns the Intake → Investigating transition (with
            // its own model select); the development control runs the gated build with
            // a per-tier model map. The strongest tier leads and delegates simpler work.
            UowStepRunControls {
                story_id: uow_key.clone(),
                stage,
                uow_refresh,
                active_run,
                models: run_models_snap.clone(),
                invest_model,
                dev_strongest,
                dev_balanced,
                dev_fast,
            }

            // ── Agent activity for the active run (reused) ────────────────────
            {
                let rid = match active_run() {
                    Some(ref r) => r.id.clone(),
                    None => String::new(),
                };
                rsx! { crate::agent_activity::AgentActivity { run_id: rid } }
            }

            // ── AI-assisted "Update branch" (GitHub PR "Update branch", gated) ─
            // Targets THIS UoW's branch: pick a source branch (local or origin) and
            // merge it INTO the UoW branch. A clean merge commits; a conflict is
            // resolved by ONE gated agent (drives its own AgentActivity). Per-UoW
            // because it operates on this UoW's working branch.
            UowUpdateBranchControl {
                story_id: uow_key.clone(),
                uow_refresh,
                models: run_models_snap.clone(),
            }

            // ── PR lifecycle (Decision 2): open / pull / resolve / comment ────
            // Gated on the UoW having a branch (read from the same keyed UoW fetch the
            // lifecycle strip uses). The resolve action is gated EXACTLY like the dev run.
            UowPrControl {
                story_id: uow_key.clone(),
                has_branch: uow_for_stage
                    .read()
                    .clone()
                    .flatten()
                    .and_then(|u| u.branch)
                    .map(|b| !b.trim().is_empty())
                    .unwrap_or(false),
                uow_refresh,
                models: run_models_snap.clone(),
            }

            // ── The UoW panel (reused), keyed to this UoW ─────────────────────
            UowPanel { story_id: uow.id.clone(), uow_refresh }

            // ── The live run + provenance + sign-off (reused) ─────────────────
            if let Some(r) = active_run() {
                LiveRunPanel { run: r, uow_refresh }
            }

            // ── Add comment to the source issue (with @-mention autocomplete) ──
            // A comment with an @-mention IS how you loop a teammate in. As you type an
            // `@<partial>` token, a dropdown of the repo's ASSIGNABLE users (the practical
            // mention set GitHub resolves) appears; clicking one completes the @handle.
            // SCOPE: the candidate set is GitHub's assignees for the repo. A per-provider
            // mention wrapper (Jira/ADO user search) is the future generalization.
            div { class: "uow-comment",
                p { class: "clarify-h", "Add comment to issue" }
                p { class: "section-hint", "Posts a comment back onto the source issue via the tracker adapter. Type @ to mention an assignable teammate (GitHub resolves @handle)." }
                // The textarea wrapper is position-relative so the dropdown anchors to it.
                div { class: "uow-comment-box",
                    textarea {
                        class: "clarify-q",
                        value: "{comment_body}",
                        rows: "3",
                        placeholder: "Write a comment to post on the issue… (type @ to mention)",
                        oninput: move |e| {
                            let v = e.value();
                            // Recompute whether an active @token exists with matches.
                            let show = match active_mention_partial(&v) {
                                Some(p) => !filter_mention_candidates(&assignees, p).is_empty(),
                                None => false,
                            };
                            comment_body.set(v);
                            mention_open.set(show);
                        },
                    }
                    // The autocomplete dropdown: shown only when an active @token matches.
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
    loop {
        if let Some(rv) = fetch_run(&run_id).await {
            let done = rv.done;
            active_run.set(Some(rv));
            if done {
                uow_refresh += 1;
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    }
}

/// AI-assisted "Update branch" control (the GitHub PR "Update branch" pattern, gated).
///
/// Merges a user-selected SOURCE branch (local or origin) INTO this UoW's working
/// branch. A `<select>` is populated from `POST /api/uow/:story_id/branches`, grouped
/// into "Local" and "Origin" `<optgroup>`s (origin values carry an `origin:` prefix so
/// the handler knows the source kind). The "▶ Update branch (AI-assisted)" button POSTs
/// to `POST /api/uow/:story_id/update-branch`, then drives `AgentActivity` on the
/// returned run and refreshes the UoW when the run completes.
///
/// A clean merge commits server-side; a conflict is resolved by ONE gated agent (the
/// gate is preserved end to end). A server 4xx (e.g. no branch yet, repo not resolved
/// locally) raises a toast carrying the server's reason. Owns its OWN active-run signal
/// so it doesn't collide with the lifecycle run control.
#[component]
pub(super) fn UowUpdateBranchControl(
    story_id: String,
    uow_refresh: Signal<u32>,
    models: Option<AuditModelsResp>,
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

    // The selected source. The select's value carries the source kind: a bare branch
    // name is local; an `origin:`-prefixed value is an origin branch.
    let mut selected = use_signal(String::new);
    // The conflict-resolution agent's model (default = project strongest, editable).
    let model = use_signal(String::new);
    // Its own active run + busy flag (independent of the lifecycle run control).
    let active_run = use_signal(|| Option::<RunView>::None);
    let mut updating = use_signal(|| false);

    let has_branches = !branches.local.is_empty() || !branches.origin.is_empty();

    rsx! {
        div { class: "uow-step-control uow-update-branch",
            p { class: "uow-step-h", "Update branch (AI-assisted)" }
            p { class: "section-hint",
                "Merge a branch INTO this UoW's branch (GitHub's \"Update branch\"). A clean merge commits; conflicts are resolved by a gated agent."
            }
            if has_branches {
                div { class: "run-control-row",
                    select {
                        class: "uow-branch-select",
                        value: "{selected}",
                        onchange: move |e| selected.set(e.value()),
                        option { value: "", disabled: true, selected: selected().is_empty(), "Choose a branch…" }
                        if !branches.local.is_empty() {
                            optgroup { label: "Local",
                                for b in branches.local.iter() {
                                    option { key: "local:{b}", value: "{b}", "{b}" }
                                }
                            }
                        }
                        if !branches.origin.is_empty() {
                            optgroup { label: "Origin",
                                for b in branches.origin.iter() {
                                    option { key: "origin:{b}", value: "origin:{b}", "{b}" }
                                }
                            }
                        }
                    }
                    ModelSelect { models: models.clone(), selected: model }
                    button {
                        class: "btn-run",
                        disabled: updating() || selected().is_empty(),
                        onclick: move |_| {
                            let raw = selected();
                            if raw.is_empty() {
                                return;
                            }
                            // Decode the source kind from the option value's prefix.
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
                        if updating() { "Updating…" } else { "▶ Update branch (AI-assisted)" }
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
    stage: UowStage,
    uow_refresh: Signal<u32>,
    active_run: Signal<Option<RunView>>,
    models: Option<AuditModelsResp>,
    invest_model: Signal<String>,
    dev_strongest: Signal<String>,
    dev_balanced: Signal<String>,
    dev_fast: Signal<String>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();

    // The full ordered progression, rendered as a strip with the reached stages lit.
    const STAGES: &[UowStage] = &[
        UowStage::Intake,
        UowStage::Investigating,
        UowStage::DecisionsApproved,
        UowStage::Development,
        UowStage::AwaitingQa,
        UowStage::SignedOff,
    ];

    let sid_begin = story_id.clone();
    let sid_approve = story_id.clone();
    let sid_dev = story_id.clone();

    // One-time BOOTSTRAP toggle (default OFF, per-run, NOT persisted): when on, this dev
    // run skips ONLY the layer-2 post-task lint/test bounce so a brownfield repo can land
    // the linters/checkers layer-2 needs. The security gate (layer 1) + the no-code-first
    // decisions gate still apply. The architect turns it back off after the tooling lands.
    let mut bootstrap_skip_layer2 = use_signal(|| false);

    rsx! {
        div { class: "uow-lifecycle",
            span { class: "uow-field-label", "Lifecycle" }
            div { class: "uow-lifecycle-strip",
                for s in STAGES.iter().copied() {
                    {
                        let reached = s.ordinal() <= stage.ordinal();
                        let current = s == stage;
                        let mut cls = String::from("uow-stage-pip");
                        if reached { cls.push_str(" reached"); }
                        if current { cls.push_str(" current"); }
                        rsx! {
                            span { class: "{cls}", title: "{s.label()}", "{s.label()}" }
                        }
                    }
                }
            }

            // The run control for the CURRENT phase, inline with the steps. Only one
            // shows at a time — it replaces the prior phase's control.
            match stage {
                // INVESTIGATION: model select + Begin investigation (Intake → Investigating).
                UowStage::Intake => rsx! {
                    div { class: "uow-step-control",
                        p { class: "uow-step-h", "Investigation" }
                        div { class: "run-control-row",
                            button {
                                class: "btn-run",
                                onclick: move |_| {
                                    let sid = sid_begin.clone();
                                    let md = invest_model();
                                    spawn(async move {
                                        match begin_investigation_run(&sid, &md).await {
                                            Some(rid) => poll_run_to_done(rid, active_run, uow_refresh).await,
                                            None => crate::toast::push_toast(
                                                toasts,
                                                crate::toast::ToastKind::Warning,
                                                "Could not begin the investigation run.".to_string(),
                                            ),
                                        }
                                    });
                                },
                                "▶ Begin investigation"
                            }
                            ModelSelect { models: models.clone(), selected: invest_model }
                        }
                        p { class: "section-hint", "Runs an investigation pass, then advances the stage to Investigating." }
                    }
                },
                // DECISIONS APPROVED → ready to run development: 3 tier selects + run.
                UowStage::DecisionsApproved => rsx! {
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
                                    let sid = sid_dev.clone();
                                    let tm = TierMapView {
                                        strongest: dev_strongest(),
                                        balanced: dev_balanced(),
                                        fast: dev_fast(),
                                    };
                                    let skip_l2 = bootstrap_skip_layer2();
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
                                "▶ Run development (governed)"
                            }
                        }
                    }
                },
                _ => rsx! {},
            }

            // Architect transition: Approve decisions (Investigating → DecisionsApproved).
            // Kept where it was — enabled only at the Investigating stage (the server
            // enforces this too; disabling avoids a guaranteed-409 click).
            div { class: "uow-lifecycle-actions",
                button {
                    // Transition action → the onboarding SECONDARY variant (bordered),
                    // distinct from the accent primary run buttons but on the same system.
                    class: "btn-secondary",
                    disabled: stage != UowStage::Investigating,
                    onclick: move |_| {
                        let sid = sid_approve.clone();
                        let mut uow_refresh = uow_refresh;
                        spawn(async move {
                            match post_uow_transition(&sid, "approve-decisions").await {
                                TransitionOutcome::Ok => { uow_refresh += 1; }
                                TransitionOutcome::Blocked(reason) => crate::toast::push_toast(
                                    toasts, crate::toast::ToastKind::Warning, reason,
                                ),
                                TransitionOutcome::Failed => crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Warning,
                                    "Could not advance the lifecycle stage.".to_string(),
                                ),
                            }
                        });
                    },
                    "Approve decisions"
                }
            }
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
