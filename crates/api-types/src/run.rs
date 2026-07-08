//! Run wire shapes — the CLIENT-side deserialization mirror of a governed run's status,
//! added for Phase D of the DTO extraction (`camerata-client`, the typed HTTP client over
//! the BFF's `/api/*` routes).
//!
//! The real, behavior-carrying definitions live in `camerata_app_core::run` (`Run`,
//! `RunStatus`, `GateEvent`, `RunKind`, `StallPolicy`) and the server-only
//! `RunStatusResponse` wrapper in `crates/server/src/lib.rs` (returned by
//! `GET /api/runs/:id`, handler `get_run`). This module re-declares the SAME wire shape
//! as a pure-serde mirror so `camerata-api-types` (zero `camerata-*` deps — see the crate
//! doc) can type the response without depending on `camerata-app-core` (which pulls in
//! `camerata-core`/`camerata-liveness` for the domain types this crate must not carry).
//!
//! `Run::tracker` (a `#[serde(skip)]` `LivenessTracker`) never reaches the wire, so it has
//! no mirror field here. `RunStatusResponse` on the server `#[serde(flatten)]`s `Run` and
//! then ALSO declares its own `stall_policy` / `failure_reason` fields — the wire JSON
//! therefore has each of those two keys twice (same value both times, since the server
//! copies them from the same `Run`). A single flat struct here (rather than replicating
//! the flatten) resolves that harmlessly: `serde_json` keeps the last-seen value for a
//! duplicate key, which is identical to the first in every case.
//!
//! `RunStatus::Failed` serializes to the bare string `"failed"` on the wire (the reason
//! travels separately in `failure_reason`), so the client mirror below is a plain
//! `snake_case` string enum with no `Failed { reason }` payload — see `camerata_app_core`'s
//! doc comment on `RunStatus` for the full legacy-object-form deserialize story (server-
//! internal only; never emitted).

use serde::{Deserialize, Serialize};

/// Mirrors `camerata_app_core::run::RunKind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    Watched,
    Autonomous,
}

/// Mirrors `camerata_app_core::run::StallPolicy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StallPolicy {
    Alert,
    Cancel,
}

/// Mirrors the WIRE form of `camerata_app_core::run::RunStatus` (bare `snake_case`
/// strings only — the legacy `{"failed": {"reason": ...}}` object form is a
/// server-internal deserialize fallback that is never emitted, so this client mirror
/// does not need to accept it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Planned,
    Executing,
    Gating,
    AwaitingClarification,
    AwaitingReview,
    AwaitingQa,
    Failed,
    Cancelled,
}

/// Mirrors `camerata_app_core::run::GateEvent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateEvent {
    pub seq: usize,
    pub layer: String,
    pub verdict: String,
    #[serde(default)]
    pub rule: Option<String>,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub content_hash: Option<String>,
}

/// Mirrors `GET /api/runs/:id`'s response body: `camerata_server`'s `RunStatusResponse`,
/// which is `camerata_app_core::run::Run` `#[serde(flatten)]`-ed plus `idle_ms`,
/// `stalled`, and `stall_threshold_ms` (see the module doc for the duplicate-key note on
/// `stall_policy`/`failure_reason`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunStatusResponse {
    pub id: String,
    pub story_id: String,
    pub status: RunStatus,
    #[serde(default)]
    pub events: Vec<GateEvent>,
    pub done: bool,
    pub mode: String,
    #[serde(default)]
    pub last_progress_label: String,
    pub kind: RunKind,
    pub stall_policy: StallPolicy,
    #[serde(default)]
    pub failure_reason: Option<String>,
    pub idle_ms: u128,
    pub stalled: bool,
    pub stall_threshold_ms: u128,
}

/// Optional request body for `POST /api/stories/:id/run` (mirrors the server's
/// `StartRunReq` in `crates/server/src/lib.rs`).
///
/// `tier_map` is left as raw JSON: its real shape is `camerata_fleet::tier::TierMap`
/// (four fields — `fast`/`balanced`/`strongest`/`vision` — with custom
/// `deserialize_with` chain-parsing helpers), which this pure-serde leaf crate must not
/// depend on (api-types has zero `camerata-*` deps by design). First-rung callers
/// (the MCP server / HTTP CLI) that only need the single-model or scripted path can
/// leave this `None`; a caller that needs a tiered run can still send the raw JSON
/// object the server expects.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StartRunRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier_map: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_layer2: Option<bool>,
}

/// The success response body for `POST /api/stories/:id/run`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartRunResponse {
    pub run_id: String,
    pub story_id: String,
    pub mode: String,
}
