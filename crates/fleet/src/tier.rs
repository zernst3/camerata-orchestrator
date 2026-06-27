//! Deterministic model-tiering for the governed fleet (ORCH-MODEL-TIERING-1).
//!
//! Three building blocks:
//!
//! 1. **[`CapabilityBand`]** — a stable, vendor-neutral label (`Fast` /
//!    `Balanced` / `Strongest`) that outlasts any model generation. Tasks are
//!    classified into bands; the band → model binding is configuration.
//!
//! 2. **[`TierMap`]** — the user-editable mapping from each [`CapabilityBand`]
//!    to a concrete model id. Stored on the project config with serde defaults so
//!    existing projects load cleanly without migration. The shipped default uses
//!    the three Claude tiers: Haiku (fast), Sonnet (balanced), Opus (strongest).
//!
//! 3. **[`classify_task`]** — a pure, deterministic classifier that maps a
//!    [`camerata_intake::PlanTask`] to a [`CapabilityBand`]. The heuristic is:
//!    - `Test` tasks → `Fast` (mechanical; fluency, not depth)
//!    - `Database` tasks → `Balanced` (structured; mid-tier is correct and cheaper)
//!    - `Frontend` tasks → `Balanced` (bounded reasoning over view/screen code)
//!    - `Backend` tasks → `Strongest` (domain logic is a one-way-door)
//!
//!    A task description may carry a per-task override prefix `[TIER:fast]`,
//!    `[TIER:balanced]`, or `[TIER:strongest]` (case-insensitive, leading
//!    whitespace stripped) to force a specific band regardless of `TaskKind`.
//!
//! # Usage in the fleet
//!
//! The entry point is [`build_from_plan_with_tier_map`]: it classifies each
//! [`PlanTask`], resolves the model id from the [`TierMap`], and threads that
//! per-stage model into the driver via `with_model(id)`. All existing single-model
//! entry points ([`build_from_plan`], [`build_from_plan_with_model`]) are
//! unchanged.
//!
//! [`build_from_plan`]: super::build_from_plan
//! [`build_from_plan_with_model`]: super::build_from_plan_with_model

use camerata_intake::{PlanTask, TaskKind};
use serde::{Deserialize, Serialize};

// ─── CapabilityBand ──────────────────────────────────────────────────────────

/// A stable, vendor-neutral capability label for the three cost/quality tiers.
///
/// Bands describe *what the task needs*, not which concrete model to use. The
/// model binding is a configuration concern resolved by [`TierMap`], so upgrading
/// model generations is a config change, not a code change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityBand {
    /// Mechanical / test-generation tasks. Prioritises throughput and cost.
    Fast,
    /// Standard implementation tasks with clear constraints.
    Balanced,
    /// Architectural, domain-level, or one-way-door decisions. Uses the most
    /// capable available model.
    Strongest,
}

impl CapabilityBand {
    /// A lowercase display label for logs and UI.
    pub fn label(&self) -> &'static str {
        match self {
            CapabilityBand::Fast => "fast",
            CapabilityBand::Balanced => "balanced",
            CapabilityBand::Strongest => "strongest",
        }
    }

    /// Parse a lowercase label back to a band; returns `None` on unknown input.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "fast" => Some(CapabilityBand::Fast),
            "balanced" => Some(CapabilityBand::Balanced),
            "strongest" => Some(CapabilityBand::Strongest),
            _ => None,
        }
    }
}

// ─── TierMap ─────────────────────────────────────────────────────────────────

/// Deserialise a `Vec<String>` field that may be persisted as either a JSON string
/// (legacy single-model form written by the previous `String` field) or a JSON array
/// (the new chain form). A bare string becomes a 1-element Vec; an array is taken
/// as-is; missing/null falls through to the field's `#[serde(default)]`.
fn deserialize_chain<'de, D>(de: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrVec {
        Single(String),
        Many(Vec<String>),
    }

    match StringOrVec::deserialize(de)? {
        StringOrVec::Single(s) => Ok(vec![s]),
        StringOrVec::Many(v) => Ok(v),
    }
}

/// The model-id chains for each [`CapabilityBand`], stored as project configuration.
///
/// `fast` and `balanced` are **ordered chains** (`Vec<String>`): the first entry is the
/// primary model; subsequent entries are fallbacks tried in order on retryable errors
/// (429 / 5xx / timeout). `strongest` remains a single model (orchestrator reliability
/// requires a single well-known model; fallbacks add complexity without benefit there).
///
/// **Back-compat**: a project JSON written with the old `String` form (e.g.
/// `"fast": "claude-haiku-4-5-20251001"`) deserialises correctly — the custom
/// `deserialize_chain` helper wraps the bare string into a 1-element Vec.
///
/// Serde defaults on every field ensure a project persisted before this struct existed
/// deserialises cleanly — the same back-compat pattern as `max_iterations` on the
/// server-side `Project`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierMap {
    /// Ordered model-id chain for [`CapabilityBand::Fast`] tasks (primary → fallbacks).
    #[serde(default = "default_fast_chain", deserialize_with = "deserialize_chain")]
    pub fast: Vec<String>,
    /// Ordered model-id chain for [`CapabilityBand::Balanced`] tasks (primary → fallbacks).
    #[serde(default = "default_balanced_chain", deserialize_with = "deserialize_chain")]
    pub balanced: Vec<String>,
    /// Model id for [`CapabilityBand::Strongest`] tasks. Single model — orchestrator
    /// reliability requires a single well-known model.
    #[serde(default = "default_strongest_model")]
    pub strongest: String,
}

/// The shipped default chain for [`TierMap::fast`]: Haiku only (1-element).
pub fn default_fast_chain() -> Vec<String> {
    vec!["claude-haiku-4-5-20251001".to_string()]
}

/// The shipped default chain for [`TierMap::balanced`]: Sonnet only (1-element).
pub fn default_balanced_chain() -> Vec<String> {
    vec!["claude-sonnet-4-6".to_string()]
}

/// The shipped default for [`TierMap::fast`] primary (back-compat for callers that
/// needed the old `default_fast_model()` function).
pub fn default_fast_model() -> String {
    "claude-haiku-4-5-20251001".to_string()
}

/// The shipped default for [`TierMap::balanced`] primary (back-compat).
pub fn default_balanced_model() -> String {
    "claude-sonnet-4-6".to_string()
}

/// The shipped default for [`TierMap::strongest`]: Claude Opus 4.8 (frontier-class).
pub fn default_strongest_model() -> String {
    "claude-opus-4-8".to_string()
}

impl Default for TierMap {
    fn default() -> Self {
        Self {
            fast: default_fast_chain(),
            balanced: default_balanced_chain(),
            strongest: default_strongest_model(),
        }
    }
}

impl TierMap {
    /// The primary (first) model in the `fast` chain. Never empty when default is used.
    pub fn fast_primary(&self) -> &str {
        self.fast.first().map(String::as_str).unwrap_or("claude-haiku-4-5-20251001")
    }

    /// The full `fast` fallback chain as a slice.
    pub fn fast_chain(&self) -> &[String] {
        &self.fast
    }

    /// The primary (first) model in the `balanced` chain. Never empty when default is used.
    pub fn balanced_primary(&self) -> &str {
        self.balanced.first().map(String::as_str).unwrap_or("claude-sonnet-4-6")
    }

    /// The full `balanced` fallback chain as a slice.
    pub fn balanced_chain(&self) -> &[String] {
        &self.balanced
    }

    /// Resolve the primary concrete model id for `band`.
    pub fn model_for(&self, band: CapabilityBand) -> &str {
        match band {
            CapabilityBand::Fast => self.fast_primary(),
            CapabilityBand::Balanced => self.balanced_primary(),
            CapabilityBand::Strongest => &self.strongest,
        }
    }

    /// The full chain for a band. `Strongest` returns a single-element slice.
    pub fn chain_for(&self, band: CapabilityBand) -> &[String] {
        match band {
            CapabilityBand::Fast => self.fast_chain(),
            CapabilityBand::Balanced => self.balanced_chain(),
            CapabilityBand::Strongest => std::slice::from_ref(&self.strongest),
        }
    }

    /// Classify `task` and resolve its primary model id in one call.
    pub fn model_for_task(&self, task: &PlanTask) -> &str {
        self.model_for(classify_task(task))
    }
}

// ─── classify_task ────────────────────────────────────────────────────────────

/// Classify a [`PlanTask`] into a [`CapabilityBand`].
///
/// Classification is deterministic — no randomness, no I/O, no network calls.
/// The same task always maps to the same band, making this safe to call in the
/// fleet-assembly loop and trivial to unit-test.
///
/// **Per-task override syntax** — a task description starting with `[TIER:fast]`,
/// `[TIER:balanced]`, or `[TIER:strongest]` (case-insensitive, after stripping
/// leading whitespace) overrides the heuristic for that task. This lets the lead
/// engineer pin a band without touching the [`TierMap`].
///
/// **Heuristic**:
///
/// | [`TaskKind`] | Band | Rationale |
/// |---|---|---|
/// | `Test` | `Fast` | Test generation is mechanical: read the types, produce assertions. |
/// | `Database` | `Balanced` | Schema design is structured; mid-tier gets it right at lower cost. |
/// | `Frontend` | `Balanced` | View/screen code: real reasoning over a bounded design space. |
/// | `Backend` | `Strongest` | Domain logic and API surface: one-way-door type/invariant choices. |
pub fn classify_task(task: &PlanTask) -> CapabilityBand {
    // --- Per-task override: `[TIER:<band>]` prefix in the description ---
    let trimmed = task.description.trim();
    // Fast path: check the ASCII-lowercased start for the three known prefixes.
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("[tier:fast]") {
        return CapabilityBand::Fast;
    }
    if lower.starts_with("[tier:balanced]") {
        return CapabilityBand::Balanced;
    }
    if lower.starts_with("[tier:strongest]") {
        return CapabilityBand::Strongest;
    }
    // General case: parse `[TIER:<anything>]` at the start, to give a clear
    // fallback for unrecognised bands (they fall through to the heuristic below).
    if let Some(rest) = trimmed.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            let tag = rest[..end].trim();
            if let Some(label) = tag.strip_prefix("TIER:").or_else(|| tag.strip_prefix("tier:")) {
                if let Some(band) = CapabilityBand::parse(label) {
                    return band;
                }
            }
        }
    }

    // --- TaskKind heuristic ---
    match task.kind {
        TaskKind::Test => CapabilityBand::Fast,
        TaskKind::Database => CapabilityBand::Balanced,
        TaskKind::Frontend => CapabilityBand::Balanced,
        TaskKind::Backend => CapabilityBand::Strongest,
    }
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn task(kind: TaskKind, description: &str) -> PlanTask {
        PlanTask {
            role: "Agent".to_string(),
            kind,
            description: description.to_string(),
        }
    }

    // ── CapabilityBand ────────────────────────────────────────────────────────

    #[test]
    fn band_labels_are_stable() {
        assert_eq!(CapabilityBand::Fast.label(), "fast");
        assert_eq!(CapabilityBand::Balanced.label(), "balanced");
        assert_eq!(CapabilityBand::Strongest.label(), "strongest");
    }

    #[test]
    fn band_parse_roundtrips_all_variants() {
        for band in [
            CapabilityBand::Fast,
            CapabilityBand::Balanced,
            CapabilityBand::Strongest,
        ] {
            let parsed = CapabilityBand::parse(band.label());
            assert_eq!(parsed, Some(band), "parse({}) roundtripped", band.label());
        }
    }

    #[test]
    fn band_parse_is_case_insensitive() {
        assert_eq!(CapabilityBand::parse("FAST"), Some(CapabilityBand::Fast));
        assert_eq!(
            CapabilityBand::parse("Balanced"),
            Some(CapabilityBand::Balanced)
        );
        assert_eq!(
            CapabilityBand::parse("  Strongest  "),
            Some(CapabilityBand::Strongest)
        );
    }

    #[test]
    fn band_parse_unknown_returns_none() {
        assert_eq!(CapabilityBand::parse("ultra"), None);
        assert_eq!(CapabilityBand::parse(""), None);
        assert_eq!(CapabilityBand::parse("medium"), None);
    }

    // ── TierMap defaults ──────────────────────────────────────────────────────

    #[test]
    fn default_tier_map_matches_catalog_ids() {
        let m = TierMap::default();
        // fast/balanced are now Vec<String>; check the primary (first) element.
        assert_eq!(m.fast_primary(), "claude-haiku-4-5-20251001");
        assert_eq!(m.balanced_primary(), "claude-sonnet-4-6");
        assert_eq!(m.strongest, "claude-opus-4-8");
    }

    #[test]
    fn tier_map_model_for_each_band() {
        let m = TierMap::default();
        assert_eq!(m.model_for(CapabilityBand::Fast), "claude-haiku-4-5-20251001");
        assert_eq!(m.model_for(CapabilityBand::Balanced), "claude-sonnet-4-6");
        assert_eq!(m.model_for(CapabilityBand::Strongest), "claude-opus-4-8");
    }

    #[test]
    fn tier_map_serde_roundtrip() {
        let original = TierMap::default();
        let json = serde_json::to_string(&original).unwrap();
        let back: TierMap = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn tier_map_deserialises_with_defaults_when_fields_absent() {
        // An empty object (e.g. a project persisted before TierMap existed) fills defaults.
        let json = r#"{}"#;
        let m: TierMap = serde_json::from_str(json).unwrap();
        assert_eq!(m, TierMap::default());
    }

    #[test]
    fn tier_map_custom_values_round_trip() {
        let json = r#"{"fast":["my-haiku"],"balanced":["my-sonnet"],"strongest":"my-opus"}"#;
        let m: TierMap = serde_json::from_str(json).unwrap();
        assert_eq!(m.fast, vec!["my-haiku"]);
        assert_eq!(m.balanced, vec!["my-sonnet"]);
        assert_eq!(m.strongest, "my-opus");
    }

    // ── classify_task heuristic ───────────────────────────────────────────────

    #[test]
    fn test_tasks_map_to_fast() {
        assert_eq!(
            classify_task(&task(TaskKind::Test, "add unit tests")),
            CapabilityBand::Fast
        );
    }

    #[test]
    fn database_tasks_map_to_balanced() {
        assert_eq!(
            classify_task(&task(TaskKind::Database, "schema for expenses")),
            CapabilityBand::Balanced
        );
    }

    #[test]
    fn frontend_tasks_map_to_balanced() {
        assert_eq!(
            classify_task(&task(TaskKind::Frontend, "expense list screen")),
            CapabilityBand::Balanced
        );
    }

    #[test]
    fn backend_tasks_map_to_strongest() {
        assert_eq!(
            classify_task(&task(TaskKind::Backend, "domain types and service layer")),
            CapabilityBand::Strongest
        );
    }

    // ── classify_task override syntax ─────────────────────────────────────────

    #[test]
    fn tier_prefix_overrides_heuristic_to_fast() {
        // Backend task pinned to Fast.
        let t = task(TaskKind::Backend, "[TIER:fast] generate boilerplate");
        assert_eq!(classify_task(&t), CapabilityBand::Fast);
    }

    #[test]
    fn tier_prefix_overrides_heuristic_to_balanced() {
        // Test task pinned to Balanced.
        let t = task(TaskKind::Test, "[TIER:balanced] integration test with complex setup");
        assert_eq!(classify_task(&t), CapabilityBand::Balanced);
    }

    #[test]
    fn tier_prefix_overrides_heuristic_to_strongest() {
        // Database task pinned to Strongest.
        let t = task(
            TaskKind::Database,
            "[TIER:strongest] design the cross-shard sharding key",
        );
        assert_eq!(classify_task(&t), CapabilityBand::Strongest);
    }

    #[test]
    fn tier_prefix_is_case_insensitive() {
        let t = task(TaskKind::Test, "[tier:strongest] complex orchestration");
        assert_eq!(classify_task(&t), CapabilityBand::Strongest);
    }

    #[test]
    fn tier_prefix_with_leading_whitespace_is_stripped() {
        let t = task(TaskKind::Backend, "  [TIER:fast] boring task");
        assert_eq!(classify_task(&t), CapabilityBand::Fast);
    }

    #[test]
    fn unknown_tier_prefix_falls_back_to_heuristic() {
        let t = task(TaskKind::Test, "[TIER:ultra] some test");
        // Unknown band — falls through to heuristic: Test -> Fast.
        assert_eq!(classify_task(&t), CapabilityBand::Fast);
    }

    // ── TierMap::model_for_task end-to-end ───────────────────────────────────

    #[test]
    fn model_for_task_resolves_through_classify_then_map() {
        let m = TierMap::default();

        let backend = task(TaskKind::Backend, "domain types");
        let test_t = task(TaskKind::Test, "unit tests");
        let db = task(TaskKind::Database, "schema");
        let frontend = task(TaskKind::Frontend, "list view");

        assert_eq!(m.model_for_task(&backend), "claude-opus-4-8");
        assert_eq!(m.model_for_task(&test_t), "claude-haiku-4-5-20251001");
        assert_eq!(m.model_for_task(&db), "claude-sonnet-4-6");
        assert_eq!(m.model_for_task(&frontend), "claude-sonnet-4-6");
    }

    #[test]
    fn model_for_task_honours_override_prefix() {
        let m = TierMap::default();
        let t = task(TaskKind::Backend, "[TIER:fast] quick scaffold");
        // Backend normally -> Strongest, but override forces Fast -> Haiku.
        assert_eq!(m.model_for_task(&t), "claude-haiku-4-5-20251001");
    }

    // ── TierMap chain shape + back-compat ─────────────────────────────────────

    #[test]
    fn single_string_back_compat_fast_deserializes_to_vec() {
        // A project JSON written with the OLD `"fast": "<model>"` string form
        // must deserialise into a 1-element Vec (back-compat via deserialize_chain).
        let json = r#"{"fast":"claude-haiku-4-5-20251001","balanced":"claude-sonnet-4-6","strongest":"claude-opus-4-8"}"#;
        let m: TierMap = serde_json::from_str(json).unwrap();
        assert_eq!(m.fast, vec!["claude-haiku-4-5-20251001"],
            "legacy fast string must deserialise to 1-element Vec");
        assert_eq!(m.balanced, vec!["claude-sonnet-4-6"],
            "legacy balanced string must deserialise to 1-element Vec");
        assert_eq!(m.strongest, "claude-opus-4-8",
            "strongest stays a String");
    }

    #[test]
    fn chain_form_deserializes_multiple_models() {
        let json = r#"{"fast":["free-coder:free","claude-haiku-4-5-20251001"],"balanced":["qwen:free","claude-sonnet-4-6"],"strongest":"claude-opus-4-8"}"#;
        let m: TierMap = serde_json::from_str(json).unwrap();
        assert_eq!(m.fast, vec!["free-coder:free", "claude-haiku-4-5-20251001"]);
        assert_eq!(m.balanced, vec!["qwen:free", "claude-sonnet-4-6"]);
        assert_eq!(m.fast_primary(), "free-coder:free",
            "primary is the first element");
        assert_eq!(m.balanced_primary(), "qwen:free");
    }

    #[test]
    fn fast_primary_and_chain_accessors() {
        let m = TierMap {
            fast: vec!["a".into(), "b".into()],
            balanced: vec!["c".into()],
            strongest: "d".into(),
        };
        assert_eq!(m.fast_primary(), "a");
        assert_eq!(m.fast_chain(), &["a", "b"]);
        assert_eq!(m.balanced_primary(), "c");
        assert_eq!(m.balanced_chain(), &["c"]);
    }

    #[test]
    fn chain_for_strongest_returns_single_element_slice() {
        let m = TierMap::default();
        let chain = m.chain_for(CapabilityBand::Strongest);
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0], "claude-opus-4-8");
    }

    #[test]
    fn missing_fast_balanced_fields_fill_default_chains() {
        // An empty JSON object (project pre-TierMap): defaults fill in 1-element chains.
        let m: TierMap = serde_json::from_str("{}").unwrap();
        assert_eq!(m.fast, vec!["claude-haiku-4-5-20251001"]);
        assert_eq!(m.balanced, vec!["claude-sonnet-4-6"]);
    }
}
