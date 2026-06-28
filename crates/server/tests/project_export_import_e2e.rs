//! Hermetic end-to-end regression net for PROJECT EXPORT / IMPORT round-trip.
//!
//! THE CLAIM under test (ADR `2026-06-21_project_config_vs_data_separation` + the
//! project-portability decision): a project's transferable CONFIG survives an
//! export → import round-trip, while local DATA (UoW state, settings, drafts) does NOT
//! travel. The ADR names the transferable config explicitly: "its repos (`owner/repo`),
//! ruleset, onboarded-state, and tier_map".
//!
//! The export side serializes the FULL `Project` (the real `export_project` handler emits
//! `#[serde(flatten)] project` — so every project field is in the export JSON). The import
//! side is the real `ProjectStore::import_or_overwrite` the `import_project` handler calls.
//!
//! HERMETIC: NO network, NO disk needed (in-memory `ProjectStore`), NO scan.
//!
//! ⚠️ Several assertions here are `#[ignore]`-marked BUG markers: the import path drops
//! transferable config the export carries. See the per-test BUG notes + the agent summary.

use camerata_server::project::{
    export_ruleset, parse_ruleset, CustomRule, L3ReviewConfig, ModelProfile, Project,
    ProjectRuleset, ProjectStore, RuleSelection, StepKind,
};

// ════════════════════════════════════════════════════════════════════════════════════
// Shared fixtures
// ════════════════════════════════════════════════════════════════════════════════════

fn sel(id: &str, option: Option<&str>, repos: &[&str]) -> RuleSelection {
    RuleSelection {
        rule_id: id.to_string(),
        chosen_option: option.map(|s| s.to_string()),
        repos: repos.iter().map(|s| s.to_string()).collect(),
    }
}

/// A project with NON-default config in every transferable field, so a dropped field on
/// import is impossible to miss.
fn rich_source_project(store: &ProjectStore) -> Project {
    let p = store
        .create("RichProj", vec!["me/api".to_string(), "me/web".to_string()])
        .unwrap();
    store
        .update(&p.id, |proj| {
            // ruleset: a base selection w/ chosen_option, a cross-repo, a process, + a custom rule.
            proj.ruleset = ProjectRuleset {
                selections: vec![sel("RUST-DOMAIN-1", Some("opt-b"), &["me/api"])],
                cross_repo: vec![sel("INTEGRATION-API-CONTRACT-1", None, &[])],
                process: vec![sel("PROCESS-CONVENTIONAL-COMMIT-1", None, &[])],
                custom: vec![CustomRule {
                    name: "house-style".to_string(),
                    body: "Prefer explicit over terse.".to_string(),
                    domain: "*".to_string(),
                }],
            };
            // onboarded set.
            proj.onboarded = vec!["me/api".to_string()];
            // tier_map (named transferable config in the ADR).
            proj.tier_map.strongest = "EXPORT-STRONGEST".to_string();
            proj.tier_map.balanced = vec!["EXPORT-BALANCED".to_string()];
            proj.tier_map.fast = vec!["EXPORT-FAST".to_string()];
            proj.tier_map.vision = vec!["EXPORT-VISION".to_string()];
            // step_models.
            proj.set_model_for_step(StepKind::Audit, "EXPORT-AUDIT".to_string());
            proj.set_model_for_step(StepKind::Decomposition, "EXPORT-DECOMP".to_string());
            // l3_review.
            proj.l3_review = L3ReviewConfig {
                enabled: true,
                model: "EXPORT-L3".to_string(),
            };
            // vision_enabled.
            proj.vision_enabled = true;
            // model_profile (set last; the setters above flip it to Custom anyway).
            proj.model_profile = ModelProfile::Custom;
        })
        .unwrap()
}

/// Simulate the export side EXACTLY as the `export_project` handler does: serialize the
/// full `Project` to JSON (the handler's `#[serde(flatten)] project`). The returned JSON is
/// the portable export document the import side consumes.
fn export_project_json(project: &Project) -> serde_json::Value {
    serde_json::to_value(project).expect("a Project serializes")
}

/// Simulate the import side EXACTLY as the real `import_project` handler does: read the
/// fields `ImportProjectReq` deserializes (name / repos / ruleset / onboarded) and call the
/// public `ProjectStore::import_or_overwrite` the handler invokes. Returns the imported
/// project.
fn import_via_handler_path(
    store: &ProjectStore,
    export: &serde_json::Value,
    overwrite: bool,
) -> Project {
    let name = export["name"].as_str().unwrap().to_string();
    let repos: Vec<String> = export["repos"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let ruleset: ProjectRuleset =
        serde_json::from_value(export["ruleset"].clone()).unwrap_or_default();
    let onboarded: Vec<String> = export["onboarded"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    store
        .import_or_overwrite(&name, repos, ruleset, onboarded, overwrite)
        .unwrap()
        .into_project()
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 1 — CONFIG that DOES travel through the real import path (passes today)
// ════════════════════════════════════════════════════════════════════════════════════

#[test]
fn scope1_ruleset_onboarded_repos_travel_through_export_import() {
    let src_store = ProjectStore::new();
    let source = rich_source_project(&src_store);
    let export = export_project_json(&source);

    // Import into a FRESH store (a different machine).
    let dst_store = ProjectStore::new();
    let imported = import_via_handler_path(&dst_store, &export, false);

    // repos travel.
    assert_eq!(imported.repos, vec!["me/api".to_string(), "me/web".to_string()]);

    // onboarded set travels.
    assert_eq!(
        imported.onboarded,
        vec!["me/api".to_string()],
        "onboarded set must travel with the export (ADR: onboarded-state is transferable)"
    );

    // ruleset travels in full: selections + chosen_option, cross-repo, process, custom.
    assert_eq!(imported.ruleset.selections.len(), 1);
    assert_eq!(imported.ruleset.selections[0].rule_id, "RUST-DOMAIN-1");
    assert_eq!(
        imported.ruleset.selections[0].chosen_option.as_deref(),
        Some("opt-b"),
        "the chosen alternative must travel"
    );
    assert_eq!(imported.ruleset.cross_repo[0].rule_id, "INTEGRATION-API-CONTRACT-1");
    assert_eq!(imported.ruleset.process[0].rule_id, "PROCESS-CONVENTIONAL-COMMIT-1");
    assert_eq!(imported.ruleset.custom.len(), 1);
    assert_eq!(imported.ruleset.custom[0].name, "house-style");
    assert_eq!(imported.ruleset.custom[0].body, "Prefer explicit over terse.");
}

#[test]
fn scope1_ruleset_export_helper_round_trips() {
    // The dedicated ruleset export/import helpers round-trip the full ruleset (selections +
    // chosen_option + cross-repo + process + custom).
    let store = ProjectStore::new();
    let source = rich_source_project(&store);
    let json = export_ruleset(&source);
    let back = parse_ruleset(&json).unwrap();
    assert_eq!(back, source.ruleset, "ruleset survives export_ruleset -> parse_ruleset");
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 2 — DATA that must NOT travel (settings / drafts / UoW state are separate stores)
// ════════════════════════════════════════════════════════════════════════════════════

#[test]
fn scope2_export_document_carries_no_uow_or_settings_or_drafts() {
    // The export is a Project serialization. By construction it has NO UoW / settings /
    // draft keys — those live in separate stores (uow.json / settings.json / drafts), never
    // in projects.json. This is the config-vs-data separation invariant (ADR).
    let store = ProjectStore::new();
    let source = rich_source_project(&store);
    let export = export_project_json(&source);
    let obj = export.as_object().expect("export is a JSON object");

    for forbidden in [
        "uow",
        "uows",
        "unit_of_work",
        "stage",
        "decisions",
        "sign_off",
        "gate_provenance",
        "settings",
        "chat_model",
        "workspace_root",
        "repo_paths",
        "drafts",
        "draft",
    ] {
        assert!(
            !obj.contains_key(forbidden),
            "the project export must NOT carry local DATA field `{forbidden}` (config-vs-data \
             separation): exported keys = {:?}",
            obj.keys().collect::<Vec<_>>()
        );
    }
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 3 — Same-name import = UPSERT / overwrite-in-place (keeps the id)
// ════════════════════════════════════════════════════════════════════════════════════

#[test]
fn scope3_same_name_import_overwrites_in_place_keeping_id() {
    let store = ProjectStore::new();
    let original = store
        .create("Twin", vec!["me/old".to_string()])
        .unwrap();
    let original_id = original.id.clone();

    // A second project (export from "elsewhere") with the SAME name but new config.
    let source_store = ProjectStore::new();
    let mut source = source_store.create("Twin", vec!["me/new".to_string()]).unwrap();
    source = source_store
        .update(&source.id, |p| {
            p.ruleset.selections = vec![sel("R-IMPORTED", None, &["me/new"])];
            p.onboarded = vec!["me/new".to_string()];
        })
        .unwrap();
    let export = export_project_json(&source);

    // overwrite=true -> UPSERT in place, same id.
    let imported = import_via_handler_path(&store, &export, true);
    assert_eq!(imported.id, original_id, "overwrite keeps the existing id");
    assert_eq!(imported.repos, vec!["me/new".to_string()]);
    assert_eq!(imported.ruleset.selections[0].rule_id, "R-IMPORTED");
    assert_eq!(imported.onboarded, vec!["me/new".to_string()]);
    assert_eq!(store.list().len(), 1, "no duplicate project created on same-name import");
}

#[test]
fn scope3_custom_rules_preserved_on_a_base_rules_import() {
    // Extends `upsert_base_preserves_custom_rules` to the full export -> import round-trip:
    // a project with custom rules, re-importing a BASE-rules-only ruleset, must KEEP the
    // custom rules. This mirrors the real `import_project_ruleset` handler (upsert_base_rules
    // + merge_custom).
    let store = ProjectStore::new();
    let p = store.create("KeepCustom", vec!["me/api".to_string()]).unwrap();
    store
        .update(&p.id, |proj| {
            proj.ruleset.selections = vec![sel("OLD-BASE", None, &["me/api"])];
            proj.ruleset.custom = vec![CustomRule {
                name: "house-style".to_string(),
                body: "Prefer explicit.".to_string(),
                domain: "*".to_string(),
            }];
        })
        .unwrap();

    // Import a BASE-only ruleset (no custom) — exactly what the ruleset import handler does.
    let incoming = ProjectRuleset {
        selections: vec![sel("NEW-BASE-1", None, &["me/api"]), sel("NEW-BASE-2", None, &["me/api"])],
        ..Default::default()
    };
    let updated = store
        .update(&p.id, |proj| {
            proj.upsert_base_rules(
                incoming.selections.clone(),
                incoming.cross_repo.clone(),
                incoming.process.clone(),
            );
            proj.merge_custom(&incoming.custom);
        })
        .unwrap();

    // Base rules replaced...
    assert_eq!(updated.ruleset.selections.len(), 2);
    assert_eq!(updated.ruleset.selections[0].rule_id, "NEW-BASE-1");
    // ...custom rule PRESERVED.
    assert_eq!(updated.ruleset.custom.len(), 1, "custom rule survives a base-rules import");
    assert_eq!(updated.ruleset.custom[0].name, "house-style");
    assert_eq!(updated.ruleset.custom[0].body, "Prefer explicit.");
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 4 — TRANSFERABLE CONFIG that the import path DROPS  (BUG markers)
//
//   The export serializes the full Project (so these fields ARE in the export JSON), but
//   `ImportProjectReq` / `import_or_overwrite` only read name/repos/ruleset/onboarded — so
//   tier_map, step_models, model_profile, l3_review, and vision_enabled are LOST on import.
//   The ADR explicitly names tier_map as transferable config, so dropping it is a defect.
//   These are kept as `#[ignore]` BUG markers (fix requires changing the import handler +
//   `import_or_overwrite` signature — not a safe blind edit). See the agent summary.
// ════════════════════════════════════════════════════════════════════════════════════

#[test]
#[ignore = "BUG: import drops tier_map — export carries it but import_or_overwrite/ImportProjectReq do not read it (ADR names tier_map as transferable config)"]
fn scope4_bug_tier_map_must_travel_through_import() {
    let src_store = ProjectStore::new();
    let source = rich_source_project(&src_store);
    let export = export_project_json(&source);
    let dst_store = ProjectStore::new();
    let imported = import_via_handler_path(&dst_store, &export, false);

    // FAILS today: imported.tier_map is TierMap::default(), not the exported values.
    // (Assertion that documents intended behavior — tier_map is transferable config.)
    assert_eq!(
        imported.tier_map.strongest, "EXPORT-STRONGEST",
        "tier_map.strongest must travel with the export (ADR: tier_map is transferable config)"
    );
    assert_eq!(imported.tier_map.balanced, vec!["EXPORT-BALANCED".to_string()]);
    assert_eq!(imported.tier_map.fast, vec!["EXPORT-FAST".to_string()]);
    assert_eq!(imported.tier_map.vision, vec!["EXPORT-VISION".to_string()]);
}

#[test]
#[ignore = "BUG: import drops step_models — export carries them but the import path does not read them"]
fn scope4_bug_step_models_must_travel_through_import() {
    let src_store = ProjectStore::new();
    let source = rich_source_project(&src_store);
    let export = export_project_json(&source);
    let dst_store = ProjectStore::new();
    let imported = import_via_handler_path(&dst_store, &export, false);

    // FAILS today: step models reset to DEFAULT_MODEL on import.
    assert_eq!(imported.model_for_step(StepKind::Audit), "EXPORT-AUDIT");
    assert_eq!(imported.model_for_step(StepKind::Decomposition), "EXPORT-DECOMP");
}

#[test]
#[ignore = "BUG: import drops l3_review + model_profile + vision_enabled — export carries them but the import path does not read them"]
fn scope4_bug_l3_profile_vision_must_travel_through_import() {
    let src_store = ProjectStore::new();
    let source = rich_source_project(&src_store);
    let export = export_project_json(&source);
    let dst_store = ProjectStore::new();
    let imported = import_via_handler_path(&dst_store, &export, false);

    // FAILS today: all three reset to defaults on import.
    assert!(imported.l3_review.enabled, "l3_review.enabled must travel");
    assert_eq!(imported.l3_review.model, "EXPORT-L3", "l3_review.model must travel");
    assert_eq!(
        imported.model_profile,
        ModelProfile::Custom,
        "model_profile must travel"
    );
    assert!(imported.vision_enabled, "vision_enabled must travel");
}
