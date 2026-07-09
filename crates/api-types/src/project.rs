//! Pure-serde project sub-shapes, relocated here (Phase A of the DTO extraction) from
//! `camerata_app_core::project`, which re-exports every name below so
//! `camerata_app_core::project::X` call sites resolve unchanged.
//!
//! Only the fully self-contained pieces move: [`L3ReviewConfig`], [`StallThresholds`],
//! and [`StepModels`], plus the constants/serde-default functions their `Default`/serde
//! impls require ([`DEFAULT_MODEL`], [`DEFAULT_ROUTINE_STALL_SECS`], [`default_model`],
//! [`default_watched_secs`], [`default_routine_secs`]). The `Project` aggregate itself
//! stays in `camerata_app_core` — it references `TierMap` (camerata-fleet) and
//! `ProcessRuleConfig` (camerata-checks), which this pure serde leaf crate must not
//! depend on.

use serde::{Deserialize, Serialize};

/// The default model when none is configured / requested. Capable by default; override
/// per call or via `CAMERATA_LLM_MODEL`.
pub const DEFAULT_MODEL: &str = "claude-sonnet-4-6";

/// serde default for each [`StepModels`] field — the shipped [`DEFAULT_MODEL`]. Used so a
/// project JSON written before a given step field existed deserializes to the default
/// rather than failing (mirrors [`default_max_iterations`] in `camerata_app_core::project`).
pub fn default_model() -> String {
    DEFAULT_MODEL.to_string()
}

pub fn default_watched_secs() -> u64 {
    std::env::var("CAMERATA_RUN_STALL_THRESHOLD_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120)
}

/// Default stall threshold (seconds) for ROUTINE / autonomous (walk-away) runs. LIFECYCLE-6:
/// autonomous runs auto-cancel on stall with NO architect watching, so the default is
/// deliberately GENEROUS (30 min) to avoid killing a legitimately long unattended run. A stall
/// only trips when the liveness heartbeat has been silent this whole window. When the env
/// override (`CAMERATA_RUN_STALL_THRESHOLD_SECS`) is set it takes precedence (scaled x5 off the
/// watched base, min floor 120s), so ops can tune it up OR down; absent it, the generous default.
pub const DEFAULT_ROUTINE_STALL_SECS: u64 = 1_800;

pub fn default_routine_secs() -> u64 {
    std::env::var("CAMERATA_RUN_STALL_THRESHOLD_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|s| s.max(120) * 5)
        .unwrap_or(DEFAULT_ROUTINE_STALL_SECS)
}

/// Per-project Layer-3 (agentic code-review) configuration (R7).
///
/// Default: off, using the project's `balanced` tier model when enabled.
/// Serialises with `serde(default)` so projects written before this field existed
/// deserialise to the disabled default without migration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct L3ReviewConfig {
    /// Whether L3 is enabled for this project.
    #[serde(default)]
    pub enabled: bool,
    /// The model id that runs the L3 reviewer. Empty = use the project's `balanced` tier model.
    #[serde(default)]
    pub model: String,
}

impl Default for L3ReviewConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: String::new(),
        }
    }
}

/// Per-project stall detection thresholds, split by context (watched = interactive,
/// routine = autonomous/walk-away). Mirrors the `StepModels` pattern.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StallThresholds {
    #[serde(default = "default_watched_secs")]
    pub watched_secs: u64,
    #[serde(default = "default_routine_secs")]
    pub routine_secs: u64,
}

impl Default for StallThresholds {
    fn default() -> Self {
        Self {
            watched_secs: default_watched_secs(),
            routine_secs: default_routine_secs(),
        }
    }
}

/// Per-project, per-step model configuration for every NON-FLEET AI step.
///
/// One model-id slot per `StepKind` (`camerata_app_core::project::StepKind`). This
/// mirrors `TierMap` exactly: `serde(default)` on every field (legacy-JSON back-compat),
/// a [`Default`] impl seeding every slot with [`DEFAULT_MODEL`], and per-project storage
/// on `Project` mutated only through `ProjectStore::set_step_model`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StepModels {
    #[serde(default = "default_model")]
    pub audit: String,
    #[serde(default = "default_model")]
    pub calibration: String,
    #[serde(default = "default_model")]
    pub research_chat: String,
    #[serde(default = "default_model")]
    pub story_authoring: String,
    #[serde(default = "default_model")]
    pub decomposition: String,
    #[serde(default = "default_model")]
    pub escalation: String,
    #[serde(default = "default_model")]
    pub clarification: String,
}

impl Default for StepModels {
    fn default() -> Self {
        Self {
            audit: DEFAULT_MODEL.to_string(),
            calibration: DEFAULT_MODEL.to_string(),
            research_chat: DEFAULT_MODEL.to_string(),
            story_authoring: DEFAULT_MODEL.to_string(),
            decomposition: DEFAULT_MODEL.to_string(),
            escalation: DEFAULT_MODEL.to_string(),
            clarification: DEFAULT_MODEL.to_string(),
        }
    }
}
