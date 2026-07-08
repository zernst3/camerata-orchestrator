//! `WorkItem` — the UI-side wire mirror of the BFF's normalized work item, relocated here
//! (Phase C of the UI-core extraction, see `docs/plans/2026-07-01_ui-core-extraction.md`)
//! from `camerata_ui::cockpit::uow`, which re-exports it so its cockpit call sites resolve
//! unchanged. `camerata-ui-core`'s `GovDevState` holds pulled `WorkItem`s, and that crate
//! is renderer-free — so the shape lives in this pure-serde leaf, not in the Dioxus adapter.
//!
//! NOTE: this is the CLIENT-side deserialization mirror (everything `#[serde(default)]`,
//! resilient to missing fields). The server's emit-side shape stays in
//! `camerata_server::workitems::WorkItem`; the JSON wire contract is what ties them.

/// A normalized work item from any tracker provider (`POST /api/workitems/pull`,
/// `POST /api/workitems/refresh`). The server maps a provider's native issue (today:
/// the worktracker GitHub adapter's `CanonicalStory`) into this shape so the UI never
/// touches a provider-specific payload.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Debug, Default)]
pub struct WorkItem {
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
    /// The logins of the users assigned to the item. Empty when unassigned. Populated on
    /// the single-issue refresh path (Pull latest); the bulk pull + spine-resolved paths
    /// leave it empty (those sources don't carry it).
    #[serde(default)]
    pub assignees: Vec<String>,
    /// The item's last-updated ISO-8601 timestamp as the tracker returns it. Empty when
    /// absent. The update-poll uses it as the per-UoW last-seen baseline / change signal.
    #[serde(default)]
    pub updated_at: String,
}

// ── Assign (Phase D: `camerata-client`) ────────────────────────────────────────

/// Request body for `POST /api/workitems/assign` (mirrors the server's private
/// `WorkItemAssignReq` in `crates/server/src/lib.rs::workitems_assign`).
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Debug)]
pub struct AssignWorkItemRequest {
    pub work_item_id: String,
    pub assignee: String,
}

/// Response body for `POST /api/workitems/assign`: `{ ok, assignees, updated_at }`
/// (mirrors `crates/server/src/lib.rs::workitems_assign`'s `Json(serde_json::json!(...))`
/// body, which in turn carries `crate::github_issues::AssignOutcome`'s two fields plus
/// `ok: true`). This is the SAME wire shape as the UI's private
/// `camerata_ui::cockpit::uow::AssignResult` — that type stays `pub(super)` in the UI
/// crate; this is the shared, dependency-free mirror for non-UI callers (MCP/CLI).
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct AssignWorkItemResponse {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub assignees: Vec<String>,
    #[serde(default)]
    pub updated_at: String,
}
