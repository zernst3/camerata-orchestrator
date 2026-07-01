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
    classify_task, default_balanced_chain, default_balanced_model, default_fast_chain,
    default_fast_model, default_strongest_model,
    CapabilityBand, TierMap,
};

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::ProjectStore;

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
            fast: vec!["my-haiku".to_string()],
            balanced: vec!["my-sonnet".to_string()],
            strongest: "my-opus".to_string(),
            vision: vec![],
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

    // The two PURE serde tests (tier_map_defaults_when_absent_from_legacy_project_json,
    // tier_map_custom_values_survive_project_json_roundtrip) moved to
    // `camerata_app_core::project`'s test module alongside the Project type itself
    // (#117, backend headless-core split). The three tests below stay here because each
    // constructs a `ProjectStore` (the persistence adapter, which lives in this crate).

    #[test]
    fn tier_map_vision_round_trips_through_project_save_load() {
        // tier_map.vision must survive a full save → reload cycle through the ProjectStore.
        let dir = std::env::temp_dir().join(format!(
            "camerata-vision-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("projects.json");

        let id = {
            let store = crate::project::ProjectStore::load_or_new(path.clone());
            let p = store.create("VisionProj", vec![]).unwrap();
            store
                .update(&p.id, |proj| {
                    proj.tier_map.vision = vec!["minimax/minimax-01:free".to_string()];
                })
                .unwrap();
            p.id
        };

        // Re-load from disk and check vision survived.
        let reloaded = crate::project::ProjectStore::load_or_new(path.clone());
        let p = reloaded.get(&id).expect("project survived reload");
        assert_eq!(
            p.tier_map.vision,
            vec!["minimax/minimax-01:free".to_string()],
            "tier_map.vision must survive persistence + reload"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
