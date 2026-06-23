//! Model-tiering integration for the server / project layer.
//!
//! The core tier types ([`CapabilityBand`], [`TierMap`], [`classify_task`]) live
//! in `camerata_fleet::tier` — that is where they are used at runtime. This
//! module re-exports them under the `camerata_server` namespace so other server
//! modules (especially `project`) can import them with a clean local path, and
//! so the server's tests can exercise the Project-level integration without
//! crossing crate boundaries.
//!
//! The re-exported [`TierMap`] is what is stored on [`crate::project::Project`]
//! (serde-default back-compat).

pub use camerata_fleet::tier::{
    classify_task, default_balanced_model, default_fast_model, default_strongest_model,
    CapabilityBand, TierMap,
};

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Project, ProjectRuleset, ProjectStore};

    // ── Project-level tier_map integration ────────────────────────────────────

    #[test]
    fn new_projects_get_default_tier_map() {
        let store = ProjectStore::new();
        let p = store.create("MyProj", vec![]).unwrap();
        assert_eq!(
            p.tier_map,
            TierMap::default(),
            "freshly created project must carry the default tier map"
        );
    }

    #[test]
    fn project_tier_map_serde_roundtrip_through_store() {
        // Write a project with a custom tier map, serialise, deserialise, check.
        let custom_map = TierMap {
            fast: "my-haiku".to_string(),
            balanced: "my-sonnet".to_string(),
            strongest: "my-opus".to_string(),
        };
        let store = ProjectStore::new();
        let mut p = store.create("TierProj", vec![]).unwrap();
        // Update via store.
        p = store
            .update(&p.id, |proj| {
                proj.tier_map = custom_map.clone();
            })
            .unwrap();
        assert_eq!(p.tier_map, custom_map);

        // Re-fetch from store: the map must survive the round-trip through serde.
        let fetched = store.get(&p.id).unwrap();
        assert_eq!(fetched.tier_map, custom_map);
    }

    #[test]
    fn tier_map_defaults_when_absent_from_legacy_project_json() {
        // A project JSON written before tier_map existed must deserialise correctly
        // with serde filling in the default TierMap. Mirrors the max_iterations test.
        let json = r#"{
            "id": "proj-1",
            "name": "Legacy",
            "repos": [],
            "ruleset": {},
            "onboarded": []
        }"#;
        let p: Project = serde_json::from_str(json).unwrap();
        assert_eq!(
            p.tier_map,
            TierMap::default(),
            "legacy project must deserialise with default tier_map"
        );
    }

    #[test]
    fn tier_map_custom_values_survive_project_json_roundtrip() {
        let original = Project {
            id: "p".into(),
            name: "P".into(),
            repos: vec![],
            onboarded: vec![],
            max_iterations: crate::project::default_max_iterations(),
            tier_map: TierMap {
                fast: "haiku-custom".into(),
                balanced: "sonnet-custom".into(),
                strongest: "opus-custom".into(),
            },
            process_rule_config: camerata_checks::vcs_action::ProcessRuleConfig::default(),
            step_models: crate::project::StepModels::default(),
            stall_thresholds: crate::project::StallThresholds::default(),
            ruleset: ProjectRuleset::default(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tier_map, original.tier_map);
    }
}
