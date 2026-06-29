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
//! The scope-4 + scope-5 tests assert that ALL transferable config (tier_map, step_models,
//! model_profile, l3_review, vision_enabled, max_iterations, stall_thresholds,
//! process_rule_config) round-trips through the import path. These were `#[ignore]` BUG
//! markers before issue #111; they are live now that the import path applies the full config.

use camerata_server::project::{
    default_max_iterations, export_ruleset, parse_ruleset, CustomRule, L3ReviewConfig,
    ModelProfile, Project, ProjectImport, ProjectRuleset, ProjectStore, RuleSelection,
    StallThresholds, StepKind,
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
                    repos: Vec::new(),
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
            // max_iterations (non-default).
            proj.max_iterations = 7;
            // stall_thresholds (non-default).
            proj.stall_thresholds = StallThresholds {
                watched_secs: 999,
                routine_secs: 4242,
            };
            // process_rule_config (non-default: flip branch_naming on).
            proj.process_rule_config.branch_naming.enabled = true;
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

/// Read a transferable-config field out of the export JSON the way the real
/// `ImportProjectReq` does: `serde(default)` for an absent/null key (back-compat with older
/// exports), otherwise the deserialized value.
fn field_or_default<T: serde::de::DeserializeOwned + Default>(
    export: &serde_json::Value,
    key: &str,
) -> T {
    match export.get(key) {
        Some(v) if !v.is_null() => serde_json::from_value(v.clone()).unwrap_or_default(),
        _ => T::default(),
    }
}

/// Simulate the import side EXACTLY as the real `import_project` handler does: read the full
/// transferable config `ImportProjectReq` deserializes (name / repos / ruleset / onboarded /
/// tier_map / process_rule_config / step_models / stall_thresholds / l3_review /
/// model_profile / vision_enabled / max_iterations) and call the public
/// `ProjectStore::import_or_overwrite` the handler invokes. Returns the imported project.
fn import_via_handler_path(
    store: &ProjectStore,
    export: &serde_json::Value,
    overwrite: bool,
) -> Project {
    let name = export["name"].as_str().unwrap().to_string();
    let import = ProjectImport {
        repos: field_or_default(export, "repos"),
        ruleset: field_or_default::<ProjectRuleset>(export, "ruleset"),
        onboarded: field_or_default(export, "onboarded"),
        max_iterations: match export.get("max_iterations") {
            Some(v) if !v.is_null() => {
                serde_json::from_value(v.clone()).unwrap_or_else(|_| default_max_iterations())
            }
            _ => default_max_iterations(),
        },
        tier_map: field_or_default(export, "tier_map"),
        process_rule_config: field_or_default(export, "process_rule_config"),
        step_models: field_or_default(export, "step_models"),
        stall_thresholds: field_or_default(export, "stall_thresholds"),
        l3_review: field_or_default(export, "l3_review"),
        model_profile: field_or_default(export, "model_profile"),
        vision_enabled: field_or_default(export, "vision_enabled"),
    };
    store
        .import_or_overwrite(&name, import, overwrite)
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
                repos: Vec::new(),
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
// SCOPE 4 — TRANSFERABLE CONFIG now travels through the import path (#111 fix)
//
//   The export serializes the full Project (so these fields ARE in the export JSON), and
//   `ImportProjectReq` / `import_or_overwrite` (via `ProjectImport`) now read + apply them —
//   so tier_map, step_models, model_profile, l3_review, and vision_enabled round-trip.
//   The ADR explicitly names every Project field as transferable config. (These were
//   `#[ignore]` BUG markers before #111; un-ignored now that the import path is fixed.)
// ════════════════════════════════════════════════════════════════════════════════════

#[test]
fn scope4_bug_tier_map_must_travel_through_import() {
    let src_store = ProjectStore::new();
    let source = rich_source_project(&src_store);
    let export = export_project_json(&source);
    let dst_store = ProjectStore::new();
    let imported = import_via_handler_path(&dst_store, &export, false);

    // tier_map is transferable config (ADR) and now round-trips through the import path.
    assert_eq!(
        imported.tier_map.strongest, "EXPORT-STRONGEST",
        "tier_map.strongest must travel with the export (ADR: tier_map is transferable config)"
    );
    assert_eq!(imported.tier_map.balanced, vec!["EXPORT-BALANCED".to_string()]);
    assert_eq!(imported.tier_map.fast, vec!["EXPORT-FAST".to_string()]);
    assert_eq!(imported.tier_map.vision, vec!["EXPORT-VISION".to_string()]);
}

#[test]
fn scope4_bug_step_models_must_travel_through_import() {
    let src_store = ProjectStore::new();
    let source = rich_source_project(&src_store);
    let export = export_project_json(&source);
    let dst_store = ProjectStore::new();
    let imported = import_via_handler_path(&dst_store, &export, false);

    // step models now round-trip through the import path (#111).
    assert_eq!(imported.model_for_step(StepKind::Audit), "EXPORT-AUDIT");
    assert_eq!(imported.model_for_step(StepKind::Decomposition), "EXPORT-DECOMP");
}

#[test]
fn scope4_bug_l3_profile_vision_must_travel_through_import() {
    let src_store = ProjectStore::new();
    let source = rich_source_project(&src_store);
    let export = export_project_json(&source);
    let dst_store = ProjectStore::new();
    let imported = import_via_handler_path(&dst_store, &export, false);

    // l3_review + model_profile + vision_enabled now round-trip through the import path (#111).
    assert!(imported.l3_review.enabled, "l3_review.enabled must travel");
    assert_eq!(imported.l3_review.model, "EXPORT-L3", "l3_review.model must travel");
    assert_eq!(
        imported.model_profile,
        ModelProfile::Custom,
        "model_profile must travel"
    );
    assert!(imported.vision_enabled, "vision_enabled must travel");
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 5 — FULL round-trip: EVERY transferable config field survives export -> import
//
//   One assertion bundle covering all of #111 at once: a new-project import (fresh store)
//   AND an overwrite-in-place import must both reproduce every Project config field from
//   the rich source. Guards against a future field being added to Project but missed in the
//   import path (the exact class of bug #111 was).
// ════════════════════════════════════════════════════════════════════════════════════

/// Assert that `imported` reproduces EVERY transferable config field of the rich source.
fn assert_full_config_round_trips(imported: &Project) {
    // repos / onboarded / ruleset (selections incl. chosen_option, cross-repo, process, custom).
    assert_eq!(imported.repos, vec!["me/api".to_string(), "me/web".to_string()]);
    assert_eq!(imported.onboarded, vec!["me/api".to_string()]);
    assert_eq!(imported.ruleset.selections[0].rule_id, "RUST-DOMAIN-1");
    assert_eq!(
        imported.ruleset.selections[0].chosen_option.as_deref(),
        Some("opt-b")
    );
    assert_eq!(imported.ruleset.cross_repo[0].rule_id, "INTEGRATION-API-CONTRACT-1");
    assert_eq!(imported.ruleset.process[0].rule_id, "PROCESS-CONVENTIONAL-COMMIT-1");
    assert_eq!(imported.ruleset.custom.len(), 1);
    assert_eq!(imported.ruleset.custom[0].name, "house-style");

    // tier_map (full chain incl. vision).
    assert_eq!(imported.tier_map.strongest, "EXPORT-STRONGEST");
    assert_eq!(imported.tier_map.balanced, vec!["EXPORT-BALANCED".to_string()]);
    assert_eq!(imported.tier_map.fast, vec!["EXPORT-FAST".to_string()]);
    assert_eq!(imported.tier_map.vision, vec!["EXPORT-VISION".to_string()]);

    // step_models.
    assert_eq!(imported.model_for_step(StepKind::Audit), "EXPORT-AUDIT");
    assert_eq!(imported.model_for_step(StepKind::Decomposition), "EXPORT-DECOMP");

    // l3_review / model_profile / vision_enabled.
    assert!(imported.l3_review.enabled);
    assert_eq!(imported.l3_review.model, "EXPORT-L3");
    assert_eq!(imported.model_profile, ModelProfile::Custom);
    assert!(imported.vision_enabled);

    // max_iterations / stall_thresholds / process_rule_config.
    assert_eq!(imported.max_iterations, 7);
    assert_eq!(imported.stall_thresholds.watched_secs, 999);
    assert_eq!(imported.stall_thresholds.routine_secs, 4242);
    assert!(imported.process_rule_config.branch_naming.enabled);
}

#[test]
fn scope5_full_config_round_trips_through_import_new_and_overwrite() {
    let src_store = ProjectStore::new();
    let source = rich_source_project(&src_store);
    let export = export_project_json(&source);

    // (a) New-project import into a fresh store: full config travels.
    let dst_store = ProjectStore::new();
    let imported_new = import_via_handler_path(&dst_store, &export, false);
    assert_full_config_round_trips(&imported_new);

    // (b) Overwrite-in-place import: a target seeded with all-default config, then overwritten
    //     by the rich export, must end up with the rich config (same id, full replace).
    let ow_store = ProjectStore::new();
    let stub = ow_store.create("RichProj", vec!["stale/repo".to_string()]).unwrap();
    let stub_id = stub.id.clone();
    let imported_ow = import_via_handler_path(&ow_store, &export, true);
    assert_eq!(imported_ow.id, stub_id, "overwrite keeps the same id");
    assert_eq!(ow_store.list().len(), 1, "no duplicate on same-name overwrite");
    assert_full_config_round_trips(&imported_ow);
}
