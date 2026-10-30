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

/// The default model when none is configured / requested. Capable by default; a project or a
/// call names another one explicitly (never the environment).
pub const DEFAULT_MODEL: &str = "claude-sonnet-5-5";

/// The model the fix stage runs on when no fix model is set and the `fix_on_haiku` efficiency
/// measure is on (the default). The fix call is short, narrow, evidence-bound prose and every
/// answer is checked by the self-check, so it is the stage where a cheaper model costs the least
/// quality. Named once, here beside [`DEFAULT_MODEL`]: the server resolves it
/// (`camerata_server::ai_audit::resolve_fix_model`) and the cockpit hint and the CLI help print
/// its name through [`model_display_name`].
pub const FIX_STAGE_HAIKU_MODEL: &str = "claude-haiku-5-5";

/// A model id as a person reads it: `claude-haiku-5-5` is `Claude Haiku 5.5`. Words are
/// capitalized and an all-digit run becomes a dotted version; an id that does not follow the
/// `claude-<family>-<major>-<minor>` shape comes back unchanged apart from capitalization.
pub fn model_display_name(id: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut version: Vec<&str> = Vec::new();
    let flush = |version: &mut Vec<&str>, words: &mut Vec<String>| {
        if !version.is_empty() {
            words.push(version.join("."));
            version.clear();
        }
    };
    for part in id.split('-').filter(|p| !p.is_empty()) {
        if part.chars().all(|c| c.is_ascii_digit()) {
            version.push(part);
        } else {
            flush(&mut version, &mut words);
            let mut chars = part.chars();
            let head = chars.next().map(|c| c.to_ascii_uppercase());
            words.push(head.into_iter().chain(chars).collect());
        }
    }
    flush(&mut version, &mut words);
    words.join(" ")
}

/// What the fix stage runs on by default, for display (`Claude Haiku 5.5`).
pub fn fix_stage_haiku_display_name() -> String {
    model_display_name(FIX_STAGE_HAIKU_MODEL)
}

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

/// The backend a project (or the global chat assistant) uses for every one of its AI calls
/// — see `docs/design/2026-09-22_per-project-backend.md`. Replaces the old per-project
/// `cli_active: bool` compliance-safety flag AND the global `CAMERATA_LLM_BACKEND`-driven
/// app preference: there are now exactly two backend SETTINGS in the whole app (this type
/// used for both) and no env-driven third axis.
///
/// - **`Cli`** (the serde default AND the [`Default`] impl, i.e. the value of an ABSENT
///   `backend` field in a legacy persisted file, so legacy behavior never changes) — the
///   Claude Code CLI, i.e. the operator's own logged-in subscription. No API key needed.
///   A NEW project does not start here: it starts on [`ProjectBackend::for_new_project`].
/// - **`Api`** — the Anthropic Messages API. Needs an Anthropic key (saved in the app, else the `ANTHROPIC_API_KEY` env fallback). A
///   missing key never falls back to the CLI — `resolve_backend` reports
///   `BackendResolution::Blocked` and the caller must not start any model call. This is the
///   compliant, metered, single-party chain nameable in a client contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProjectBackend {
    #[default]
    Cli,
    Api,
}

impl ProjectBackend {
    /// The backend a brand-new project starts on: the metered Anthropic API, the compliant
    /// single-party chain. Deliberately distinct from [`Default`] (`Cli`), which is what a
    /// legacy persisted project WITHOUT a `backend` field deserializes to, so changing what
    /// new projects start on never silently switches an existing one.
    pub fn for_new_project() -> Self {
        ProjectBackend::Api
    }

    /// The lowercase name this backend serializes as, for the places that record or print it
    /// (the export manifest, the README, the operator report). Deliberately NOT named `label`:
    /// an inherent `label` would shadow the UI's display-label extension ("CLI" / "API").
    pub fn provenance_name(self) -> &'static str {
        match self {
            ProjectBackend::Cli => "cli",
            ProjectBackend::Api => "api",
        }
    }
}

/// Per-project Layer-3 (agentic code-review) configuration (R7).
///
/// Default: off, using the project's `balanced` tier model when enabled.
/// Serialises with `serde(default)` so projects written before this field existed
/// deserialise to the disabled default without migration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct L3ReviewConfig {
    /// Whether L3 is enabled for this project.
    #[serde(default)]
    pub enabled: bool,
    /// The model id that runs the L3 reviewer. Empty = use the project's `balanced` tier model.
    #[serde(default)]
    pub model: String,
    /// What a run does when the reviewer itself cannot run (an outage, a refused call).
    /// `Advisory` (default, today's behaviour) logs it and lets the run continue;
    /// `FailClosed` stops the run, so no run passes without a review.
    #[serde(default)]
    pub on_error: ReviewOnError,
    /// Extra implementer passes allowed after a reviewer bounce, beyond the project's
    /// `max_iterations`. `0` (default) keeps today's behaviour: with the default single pass
    /// a bounce ends the run. Each round costs a whole implementer session plus one review.
    #[serde(default)]
    pub fix_rounds: u8,
    /// Which model tier runs the review when no model is pinned in `model`. `Balanced`
    /// (default) is today's behaviour; `Strongest` costs more per call.
    #[serde(default)]
    pub tier: ReviewTier,
}

/// The most extra implementer passes `L3ReviewConfig::fix_rounds` may carry (the settings
/// panel offers 0, 1 and 2). The server clamps to this whatever a client posts.
pub const MAX_L3_FIX_ROUNDS: u8 = 2;

/// What a run does when the Layer-3 reviewer cannot run.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReviewOnError {
    /// Log the error and let the run continue (today's behaviour).
    #[default]
    Advisory,
    /// Stop the run: a review is required before a run passes.
    FailClosed,
}

/// Which model tier runs the Layer-3 review when no model is pinned.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReviewTier {
    /// The project's balanced tier model (today's behaviour).
    #[default]
    Balanced,
    /// The project's strongest tier model.
    Strongest,
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
///
/// One exception to "every slot is seeded": [`StepModels::fix`] is EMPTY by default, meaning
/// "not set". The fix stage's default depends on a run-time efficiency switch (Haiku when on,
/// the calibration model when off), and a seeded model id would read as an explicit choice
/// that silently overrides that switch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StepModels {
    #[serde(default = "default_model")]
    pub audit: String,
    #[serde(default = "default_model")]
    pub calibration: String,
    /// The fix stage's model. Empty = not set (the default; see the type's doc).
    #[serde(default)]
    pub fix: String,
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
            fix: String::new(),
            research_chat: DEFAULT_MODEL.to_string(),
            story_authoring: DEFAULT_MODEL.to_string(),
            decomposition: DEFAULT_MODEL.to_string(),
            escalation: DEFAULT_MODEL.to_string(),
            clarification: DEFAULT_MODEL.to_string(),
        }
    }
}

#[cfg(test)]
mod fix_stage_model_name_tests {
    use super::*;

    /// The built-in default is Sonnet 5.5 and the cheap stages default to Haiku 5.5; older ids
    /// stay selectable but are never a default.
    #[test]
    fn the_built_in_defaults_are_sonnet_5_5_and_haiku_5_5() {
        assert_eq!(DEFAULT_MODEL, "claude-sonnet-5-5");
        assert_eq!(FIX_STAGE_HAIKU_MODEL, "claude-haiku-5-5");
        assert_eq!(default_model(), "claude-sonnet-5-5");
        assert_eq!(StepModels::default().audit, "claude-sonnet-5-5");
    }

    #[test]
    fn a_model_id_is_displayed_as_family_and_dotted_version() {
        assert_eq!(model_display_name("claude-haiku-5-5"), "Claude Haiku 5.5");
        assert_eq!(model_display_name("claude-sonnet-5"), "Claude Sonnet 5");
        assert_eq!(model_display_name("claude-fable-5-1"), "Claude Fable 5.1");
        assert_eq!(fix_stage_haiku_display_name(), "Claude Haiku 5.5");
    }
}

#[cfg(test)]
mod l3_review_config_tests {
    use super::*;

    /// A project file written before the reviewer settings existed must read as today's
    /// behaviour: balanced tier, advisory on error, no fix round.
    #[test]
    fn l3_review_config_defaults_match_todays_behaviour_from_an_old_project_json() {
        let cfg: L3ReviewConfig = serde_json::from_str(r#"{"enabled":true,"model":""}"#).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.on_error, ReviewOnError::Advisory);
        assert_eq!(cfg.fix_rounds, 0);
        assert_eq!(cfg.tier, ReviewTier::Balanced);
        assert_eq!(L3ReviewConfig::default().on_error, ReviewOnError::Advisory);
    }

    #[test]
    fn the_new_settings_round_trip_as_snake_case() {
        let cfg = L3ReviewConfig {
            enabled: true,
            model: String::new(),
            on_error: ReviewOnError::FailClosed,
            fix_rounds: 2,
            tier: ReviewTier::Strongest,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains(r#""on_error":"fail_closed""#));
        assert!(json.contains(r#""tier":"strongest""#));
        let back: L3ReviewConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);
    }
}
