//! Project handlers: projects, their rule sets, step models, backends, alternatives,
//! model profiles, VCS gate bypass and reconcile.

use crate::*;

/// Which work-tracker provider is active, and whether it is a live external tracker.
pub(crate) async fn provider_info(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "provider": state.provider.current().label,
        "live": state.provider.current().live,
    }))
}

// ── Projects ────────────────────────────────────────────────────────────────

pub(crate) async fn list_projects(
    State(state): State<AppState>,
) -> Json<Vec<crate::project::Project>> {
    Json(state.projects.list())
}

#[derive(serde::Deserialize)]
pub(crate) struct CreateProjectReq {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) repos: Vec<String>,
}

pub(crate) async fn create_project(
    State(state): State<AppState>,
    Json(req): Json<CreateProjectReq>,
) -> Json<serde_json::Value> {
    match state.projects.create(&req.name, req.repos) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "could not create project" })),
    }
}

/// `POST /api/apps` — Part 2 of the Rust-fullstack scaffolder
/// (`crate::create_app::run_create_app`). Body is an `AppRequirements`-shaped JSON
/// document. `FromScratch` returns `{ ok:true, strategy:"from_scratch", reason }`
/// without touching disk or GitHub; `Skeleton` scaffolds the app, creates + pushes a
/// private GitHub repo, and registers an already-onboarded Camerata project (with the
/// two invented rules seeded as CUSTOM rules), returning `{ ok:true,
/// strategy:"skeleton", path, repo_url, project_id }`. Fail-soft on any error (missing
/// workspace root, missing GitHub token, a GitHub API failure): `{ ok:false, message
/// }`, never a 500 — mirrors this crate's other best-effort endpoints.
///
/// Builds a REAL `ReqwestTransport` (using whatever GitHub token is configured — an
/// empty placeholder is harmless when the request turns out to be `FromScratch`,
/// since that path never calls it) and the real [`crate::create_app::LiveRepoPusher`];
/// this is a deliberate, explicitly user-invoked side effect (creating a repo), never
/// triggered implicitly.
pub(crate) async fn create_app(
    State(state): State<AppState>,
    Json(reqs): Json<camerata_scaffold::AppRequirements>,
) -> Json<serde_json::Value> {
    let token = state.github_token().unwrap_or_default();
    let transport = match camerata_worktracker::ReqwestTransport::new(format!("Bearer {token}")) {
        Ok(t) => t,
        Err(e) => {
            return Json(serde_json::json!({
                "ok": false,
                "message": format!("failed to build GitHub HTTP client: {e}"),
            }))
        }
    };
    let pusher = crate::create_app::LiveRepoPusher;
    match crate::create_app::run_create_app(&state, reqs, &transport, &pusher).await {
        Ok(body) => Json(body),
        Err(message) => Json(serde_json::json!({ "ok": false, "message": message })),
    }
}

pub(crate) async fn active_project(
    State(state): State<AppState>,
) -> Json<Option<crate::project::Project>> {
    Json(state.projects.active())
}

/// Delete a project.
pub(crate) async fn delete_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": state.projects.delete(&id) }))
}

/// A project export: the full project (name, repos, ruleset, onboarded) PLUS its
/// routines, so the autonomous plane travels with the project. `#[serde(flatten)]` keeps
/// the project fields at the top level, so an older importer that only reads project
/// fields still works (it just ignores `routines`).
#[derive(serde::Serialize)]
pub(crate) struct ProjectExportDoc {
    #[serde(flatten)]
    pub(crate) project: crate::project::Project,
    /// The project's routines. On import they arrive un-provisioned + stopped (the
    /// importer explicitly sets them up). Empty for a project with no routines.
    pub(crate) routines: Vec<Routine>,
}

/// Export a project as a portable JSON document (project + its routines) — for backup or
/// moving a project between machines/installs.
pub(crate) async fn export_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ProjectExportDoc>, AppError> {
    let project = state
        .projects
        .get(&id)
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("project not found: {id}")))?;
    let routines = state.routines.list_for_project(&id);
    // A project export is a backup file that moves between machines, not a client deliverable,
    // and a client's name should not travel in a file by default: it is left blank here (the
    // stored project keeps it) and the operator sets it again for the engagement on import.
    let mut project = project;
    project.client_name.clear();
    Ok(Json(ProjectExportDoc { project, routines }))
}

/// A project import document (a prior export). `id` in the JSON is ignored — the import
/// always gets a fresh id (new) or preserves the existing id (overwrite).
#[derive(serde::Deserialize)]
pub(crate) struct ImportProjectReq {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) repos: Vec<String>,
    #[serde(default)]
    pub(crate) ruleset: crate::project::ProjectRuleset,
    /// Repos already onboarded in the source project — travels with the export so
    /// onboarding state is preserved across machines.
    #[serde(default)]
    pub(crate) onboarded: Vec<String>,
    /// Loop-guard iteration cap (#111). `#[serde(default)]` (via the project's helper) keeps
    /// back-compat with older exports that omit it.
    #[serde(default = "crate::project::default_max_iterations")]
    pub(crate) max_iterations: usize,
    /// Per-capability-band model mapping (ORCH-MODEL-TIERING-1). Transferable config (#111);
    /// `#[serde(default)]` for back-compat with older exports.
    #[serde(default)]
    pub(crate) tier_map: crate::model_tier::TierMap,
    /// VCS-action gate configuration. Transferable config (#111).
    #[serde(default)]
    pub(crate) process_rule_config: camerata_checks::vcs_action::ProcessRuleConfig,
    /// Per-step model configuration for non-fleet AI steps. Transferable config (#111).
    #[serde(default)]
    pub(crate) step_models: crate::project::StepModels,
    /// Stall-detection thresholds. Transferable config (#111).
    #[serde(default)]
    pub(crate) stall_thresholds: crate::project::StallThresholds,
    /// Layer-3 agentic code-review gate config. Transferable config (#111).
    #[serde(default)]
    pub(crate) l3_review: crate::project::L3ReviewConfig,
    /// Model efficiency profile. Transferable config (#111).
    #[serde(default)]
    pub(crate) model_profile: crate::project::ModelProfile,
    /// Whether the Designer (vision/multimodal) band is enabled. Transferable config (#111).
    #[serde(default)]
    pub(crate) vision_enabled: bool,
    /// The free-text product brief (soft context, #112). Transferable.
    #[serde(default)]
    pub(crate) product_brief: String,
    /// The agent operating principles (#112). Transferable.
    #[serde(default)]
    pub(crate) operating_principles: Vec<crate::project::OperatingPrinciple>,
    /// The curated project memory (#112, Layer 3). Transferable.
    #[serde(default)]
    pub(crate) memory: Vec<crate::project::MemoryEntry>,
    /// The work hierarchy schema (design-page work-type graph). Transferable; `#[serde(default)]`
    /// seeds the common ladder for older exports that omit it.
    #[serde(default = "crate::project::default_hierarchy_schema")]
    pub(crate) hierarchy_schema: crate::project::HierarchySchema,
    /// When `false` (the default) a name collision returns `conflict: true` so the UI
    /// can ask before overwriting. Pass `true` to overwrite in place (same id, same
    /// name, replacing the full transferable config — repos/ruleset/onboarded plus
    /// tier_map/step_models/model_profile/l3_review/etc., #111).
    #[serde(default)]
    pub(crate) overwrite: bool,
    /// The source project's routines, travelling with the export. Imported routines are
    /// created un-provisioned + stopped. Empty (default) leaves the target's routines
    /// untouched; a non-empty list REPLACES the target project's routines.
    #[serde(default)]
    pub(crate) routines: Vec<ImportedRoutine>,
}

/// A routine inside a project import. Deserializes from an exported full `Routine` (extra
/// fields like id / enabled / last_run are ignored); only the authoring fields travel.
#[derive(serde::Deserialize)]
pub(crate) struct ImportedRoutine {
    pub(crate) name: String,
    pub(crate) schedule: String,
    #[serde(default)]
    pub(crate) intent: String,
    #[serde(default)]
    pub(crate) prompt: String,
    /// The routine's scope. GAP-8: an exported `Routine` now serializes `scope`
    /// as a STRUCTURED object, while older exports carry a plain string; the
    /// shared [`camerata_app_core::routine::deserialize_scope`] accepts BOTH so a
    /// project export from either era imports with no data loss.
    #[serde(
        default,
        deserialize_with = "camerata_app_core::routine::deserialize_scope"
    )]
    pub(crate) scope: crate::routine::RoutineScope,
    /// The routine's model travels with it; blank/absent falls back to the server default.
    #[serde(default)]
    pub(crate) model: Option<String>,
}

/// Create the imported routines under `project_id` (un-provisioned + stopped), replacing
/// any the target project already had. No-op when the import carried none, so importing a
/// routine-less export never wipes routines the importer added locally.
pub(crate) fn import_project_routines(
    state: &AppState,
    project_id: &str,
    routines: &[ImportedRoutine],
) {
    if routines.is_empty() {
        return;
    }
    let reqs: Vec<crate::routine::CreateRoutineReq> = routines
        .iter()
        .map(|r| crate::routine::CreateRoutineReq {
            name: r.name.clone(),
            schedule: r.schedule.clone(),
            intent: r.intent.clone(),
            prompt: r.prompt.clone(),
            // Feed the imported scope back through the wire (string) form; the
            // store re-parses it into the structured scope via
            // `RoutineScope::from_legacy_string`. `label()` preserves the
            // human-authored note (or a policy-derived label) with no data loss.
            scope: {
                let label = r.scope.label();
                if label.trim().is_empty() {
                    "read-only".to_string()
                } else {
                    label
                }
            },
            project_id: Some(project_id.to_string()),
            model: r.model.clone(),
        })
        .collect();
    state.routines.replace_for_project(project_id, &reqs);
}

/// Import a project from an exported JSON, make it active, and return it.
///
/// Conflict response (HTTP 200):
/// `{ "ok": false, "conflict": true, "name": "…", "message": "…" }`
///
/// Success response (HTTP 200):
/// `{ "ok": true, "project": {…}, "overwritten": <bool> }`
pub(crate) async fn import_project(
    State(state): State<AppState>,
    Json(req): Json<ImportProjectReq>,
) -> Json<serde_json::Value> {
    use crate::project::{ImportOutcome, ProjectImport};
    let name = req.name.clone();
    let imported_routines = req.routines;
    let import = ProjectImport {
        repos: req.repos,
        ruleset: req.ruleset,
        onboarded: req.onboarded,
        max_iterations: req.max_iterations,
        tier_map: req.tier_map,
        process_rule_config: req.process_rule_config,
        step_models: req.step_models,
        stall_thresholds: req.stall_thresholds,
        l3_review: req.l3_review,
        model_profile: req.model_profile,
        vision_enabled: req.vision_enabled,
        product_brief: req.product_brief,
        operating_principles: req.operating_principles,
        memory: req.memory,
        hierarchy_schema: req.hierarchy_schema,
    };
    match state
        .projects
        .import_or_overwrite(&req.name, import, req.overwrite)
    {
        Some(ImportOutcome::Created(p)) => {
            import_project_routines(&state, &p.id, &imported_routines);
            Json(serde_json::json!({ "ok": true, "project": p, "overwritten": false }))
        }
        Some(ImportOutcome::Overwritten(p)) => {
            import_project_routines(&state, &p.id, &imported_routines);
            Json(serde_json::json!({ "ok": true, "project": p, "overwritten": true }))
        }
        Some(ImportOutcome::Conflict) => Json(serde_json::json!({
            "ok": false,
            "conflict": true,
            "name": name,
            "message": format!("A project named \"{name}\" already exists. Importing will overwrite it."),
        })),
        None => Json(serde_json::json!({ "ok": false, "message": "could not import project" })),
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct SetActiveReq {
    pub(crate) id: String,
}

pub(crate) async fn set_active_project(
    State(state): State<AppState>,
    Json(req): Json<SetActiveReq>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": state.projects.set_active(&req.id) }))
}

/// Every corpus rule with its FULL context (title, domain, scope, options, default), regardless
/// of any detected stack — feeds the Rules view's all-rules reference table and the
/// option-switch editor. `propose_corpus_rules` with no repos returns the whole library
/// (un-suggested, repos empty); the Rules view cross-references each against the project's
/// selections to show which repos it's applied to.
pub(crate) async fn corpus_rules() -> Json<Vec<crate::onboard::ProposedRule>> {
    Json(crate::onboard::propose_corpus_rules(&[]).await)
}

/// Export the project's ruleset as JSON (the portable source of truth).
pub(crate) async fn export_project_ruleset(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    match state.projects.get(&id) {
        Some(p) => Json(serde_json::json!({ "ok": true, "ruleset": p.ruleset })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// Import a ruleset: upsert the BASE rules (selections / cross-repo / process)
/// while PRESERVING the project's existing custom rules.
pub(crate) async fn import_project_ruleset(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(incoming): Json<crate::project::ProjectRuleset>,
) -> Json<serde_json::Value> {
    let corpus = load_corpus_for_baseline().await;
    let updated = state.projects.update(&id, |p| {
        let known_before = selected_rule_ids(&p.ruleset);
        p.upsert_base_rules(
            incoming.selections.clone(),
            incoming.cross_repo.clone(),
            incoming.process.clone(),
        );
        // Rules new to this project are baselined from the corpus; ones it already had keep
        // their baseline (inherited by the upsert) or their absence of one.
        baseline_new_to_project(&mut p.ruleset, &known_before, corpus.as_ref());
        // Merge imported custom rules by name (imported wins), never dropping
        // existing ones that aren't in the import.
        p.merge_custom(&incoming.custom);
    });
    match updated {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// Add or edit (by name) a custom rule on a project. An existing name is replaced
/// (an explicit edit); a new name is added. Never drops other custom rules.
#[derive(serde::Deserialize)]
pub(crate) struct CustomRuleReq {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) body: String,
    #[serde(default)]
    pub(crate) domain: String,
    /// The repos this custom rule applies to (the UI multiselect). Empty = all repos.
    #[serde(default)]
    pub(crate) repos: Vec<String>,
}

pub(crate) async fn add_custom_rule(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<CustomRuleReq>,
) -> Json<serde_json::Value> {
    if req.name.trim().is_empty() {
        return Json(serde_json::json!({ "ok": false, "message": "name is required" }));
    }
    let rule = crate::project::CustomRule {
        name: req.name.trim().to_string(),
        body: req.body,
        // Keep `domain` in sync for back-compat: `*` when scoped to all repos, else the first
        // selected repo. The authoritative scoping is `repos`.
        domain: if req.repos.is_empty() {
            "*".to_string()
        } else if req.domain.trim().is_empty() {
            req.repos
                .first()
                .cloned()
                .unwrap_or_else(|| "*".to_string())
        } else {
            req.domain.trim().to_string()
        },
        repos: req.repos.clone(),
    };
    match state
        .projects
        .update(&id, |p| p.merge_custom(std::slice::from_ref(&rule)))
    {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// Update a project's loop-guard ceiling (#29): the max developer→checker
/// bounce-and-revise iterations a stage may take before the fleet stops and
/// raises the outstanding violations for human review. Clamped to at least `1`
/// (a found violation always earns one bounce; the guard caps the loop, it never
/// disables it).
#[derive(serde::Deserialize)]
pub(crate) struct MaxIterationsReq {
    pub(crate) max_iterations: usize,
}

pub(crate) async fn set_max_iterations(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<MaxIterationsReq>,
) -> Json<serde_json::Value> {
    match state
        .projects
        .update(&id, |p| p.set_max_iterations(req.max_iterations))
    {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// Explicitly delete a custom rule by name (the ONLY way a custom rule leaves a
/// project).
#[derive(serde::Deserialize)]
pub(crate) struct DeleteCustomReq {
    pub(crate) name: String,
}

pub(crate) async fn delete_custom_rule(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<DeleteCustomReq>,
) -> Json<serde_json::Value> {
    match state.projects.update(&id, |p| {
        p.remove_custom(&req.name);
    }) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

// ── Model-tiering tier-map endpoint (#63) ─────────────────────────────────────

/// Deserialise a chain field that may be `null` (absent), a JSON string (legacy single-model),
/// or a JSON array (new chain form). Mirrors `camerata_fleet::tier::deserialize_chain`.
pub(crate) fn deserialize_optional_chain<'de, D>(de: D) -> Result<Option<Vec<String>>, D::Error>
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
    let maybe: Option<StringOrVec> = Option::deserialize(de)?;
    Ok(maybe.map(|v| match v {
        StringOrVec::Single(s) => vec![s],
        StringOrVec::Many(v) => v,
    }))
}

/// Body for `POST /api/projects/:id/tier-map`. Mirrors [`crate::model_tier::TierMap`]
/// with all fields optional so callers can patch just the tiers they want.
#[derive(serde::Deserialize)]
pub(crate) struct SetTierMapReq {
    /// Model chain for fast (throughput) tasks. Single string or array (back-compat).
    #[serde(default, deserialize_with = "deserialize_optional_chain")]
    pub(crate) fast: Option<Vec<String>>,
    /// Model chain for balanced (mid-tier) tasks. Single string or array (back-compat).
    #[serde(default, deserialize_with = "deserialize_optional_chain")]
    pub(crate) balanced: Option<Vec<String>>,
    /// Model id for strongest (frontier-class) tasks.
    #[serde(default)]
    pub(crate) strongest: Option<String>,
    /// Model chain for the Designer (vision/multimodal) band. Optional; absent = leave unchanged.
    #[serde(default, deserialize_with = "deserialize_optional_chain")]
    pub(crate) vision: Option<Vec<String>>,
}

/// `POST /api/projects/:id/tier-map` — update the project's model-tier map.
///
/// Applies only the fields present in the request body (patch semantics): fields
/// absent or `null` leave the existing binding unchanged, so a caller that only
/// wants to update `fast` does not need to repeat the others.
pub(crate) async fn set_tier_map(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SetTierMapReq>,
) -> Json<serde_json::Value> {
    match state.projects.update(&id, |p| {
        if let Some(fast) = req.fast.filter(|v| !v.is_empty()) {
            p.tier_map.fast = fast;
        }
        if let Some(balanced) = req.balanced.filter(|v| !v.is_empty()) {
            p.tier_map.balanced = balanced;
        }
        if let Some(strongest) = req.strongest.filter(|s| !s.trim().is_empty()) {
            p.tier_map.strongest = strongest;
        }
        // vision: an empty vec clears the slot (user removed the model); any non-None
        // value (including []) is persisted. This lets the UI clear the vision chain.
        if let Some(vision) = req.vision {
            p.tier_map.vision = vision;
        }
        // A manual tier-band edit deviates from any preset -> the project is now Custom.
        p.mark_profile_custom();
    }) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// Body for `POST /api/projects/:id/vision-enabled`.
#[derive(serde::Deserialize)]
pub(crate) struct SetVisionEnabledReq {
    /// Whether to enable (`true`) or disable (`false`) the Designer (vision) band for this project.
    pub(crate) enabled: bool,
}

/// `POST /api/projects/:id/vision-enabled` — toggle the Designer (vision) band for a project.
///
/// Persists `Project::vision_enabled`. When `false` (the default), the orchestrator ignores
/// `tier_map.vision` even if populated. When `true`, the vision band is available and the
/// orchestrator may invoke the Designer for stories that require multimodal reasoning.
pub(crate) async fn set_vision_enabled(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SetVisionEnabledReq>,
) -> Json<serde_json::Value> {
    match state.projects.update(&id, |p| {
        p.vision_enabled = req.enabled;
    }) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// Body for `POST /api/projects/:id/product-brief`.
#[derive(serde::Deserialize)]
pub(crate) struct SetProductBriefReq {
    pub(crate) product_brief: String,
}

/// `POST /api/projects/:id/product-brief` — set the project's free-text product brief (#112).
/// Persists `Project::product_brief`; it then flows into agent grounding + travels with the export.
pub(crate) async fn set_product_brief(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SetProductBriefReq>,
) -> Json<serde_json::Value> {
    match state.projects.update(&id, |p| {
        p.product_brief = req.product_brief.trim().to_string();
    }) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// Body for `POST /api/projects/:id/client-name`.
#[derive(serde::Deserialize)]
pub(crate) struct SetClientNameReq {
    pub(crate) client_name: String,
}

/// `POST /api/projects/:id/client-name` — set who the inspection is for, part of the inspection
/// setup saved with the project. Every export fills the confidentiality line from it
/// (`Project::client_name`). Blank clears it (an export then refuses until it is set); a name
/// that is too long or spans lines is refused with the reason
/// (`report_export::validate_client_name`).
pub(crate) async fn set_client_name(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SetClientNameReq>,
) -> Json<serde_json::Value> {
    let name = match crate::report_export::validate_client_name(&req.client_name) {
        Ok(name) => name,
        Err(e) => return Json(serde_json::json!({ "ok": false, "message": e.to_string() })),
    };
    match state.projects.update(&id, |p| {
        p.client_name = name;
    }) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// Body for `POST /api/projects/:id/operating-principles`.
#[derive(serde::Deserialize)]
pub(crate) struct SetOperatingPrinciplesReq {
    pub(crate) operating_principles: Vec<crate::project::OperatingPrinciple>,
}

/// `POST /api/projects/:id/operating-principles` — replace the project's operating principles (#112).
/// The full list is sent (toggled defaults + any custom). Persists `Project::operating_principles`;
/// the enabled ones flow into agent grounding + travel with the export.
pub(crate) async fn set_operating_principles(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SetOperatingPrinciplesReq>,
) -> Json<serde_json::Value> {
    match state.projects.update(&id, |p| {
        p.operating_principles = req.operating_principles.clone();
    }) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// Body for `POST /api/projects/:id/memory` — add a project-memory entry (#112, Layer 3).
#[derive(serde::Deserialize)]
pub(crate) struct AddMemoryReq {
    #[serde(default)]
    pub(crate) kind: crate::project::MemoryKind,
    pub(crate) text: String,
}

/// `POST /api/projects/:id/memory` — add a project-memory entry. A human-added entry is `Approved`
/// immediately (the human IS the curator); agent proposals arrive `Proposed` via the propose path.
pub(crate) async fn add_memory(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<AddMemoryReq>,
) -> Json<serde_json::Value> {
    let text = req.text.trim().to_string();
    if text.is_empty() {
        return Json(serde_json::json!({ "ok": false, "message": "empty memory text" }));
    }
    match state.projects.update(&id, |p| {
        let entry = crate::project::MemoryEntry {
            id: p.next_memory_id(),
            kind: req.kind,
            text: text.clone(),
            source: "human".to_string(),
            status: crate::project::MemoryStatus::Approved,
            created: chrono::Utc::now().to_rfc3339(),
        };
        p.memory.push(entry);
    }) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// Body for `POST /api/projects/:id/memory/:eid` — patch one entry.
#[derive(serde::Deserialize)]
pub(crate) struct PatchMemoryReq {
    #[serde(default)]
    pub(crate) status: Option<crate::project::MemoryStatus>,
    #[serde(default)]
    pub(crate) text: Option<String>,
    #[serde(default)]
    pub(crate) kind: Option<crate::project::MemoryKind>,
}

/// `POST /api/projects/:id/memory/:eid` — patch a memory entry: approve/archive (status), or edit
/// its text/kind. The curation surface for both human and agent-proposed entries.
pub(crate) async fn patch_memory(
    State(state): State<AppState>,
    Path((id, eid)): Path<(String, String)>,
    Json(req): Json<PatchMemoryReq>,
) -> Json<serde_json::Value> {
    match state.projects.update(&id, |p| {
        if let Some(m) = p.memory.iter_mut().find(|m| m.id == eid) {
            if let Some(s) = req.status {
                m.status = s;
            }
            if let Some(t) = &req.text {
                m.text = t.trim().to_string();
            }
            if let Some(k) = req.kind {
                m.kind = k;
            }
        }
    }) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// `DELETE /api/projects/:id/memory/:eid` — remove a memory entry (discard a proposal, or prune).
pub(crate) async fn delete_memory(
    State(state): State<AppState>,
    Path((id, eid)): Path<(String, String)>,
) -> Json<serde_json::Value> {
    match state.projects.update(&id, |p| {
        p.memory.retain(|m| m.id != eid);
    }) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// Resolve the model id for a NON-FLEET AI step from the ACTIVE project's per-step config.
///
/// This is the single resolution point for the FALLBACK steps (story authoring,
/// decomposition, escalation, clarification): once a project exists, its
/// [`crate::project::Project::model_for_step`] value is authoritative — there is NO
/// env/const fallback. The ONLY remaining [`crate::llm::DEFAULT_MODEL`] floor is the
/// project-less edge (no active project at all, e.g. a smoke test before any project is
/// created). UI-picked steps (audit / calibration / research chat) do not use this helper
/// directly — they let an explicit request model override this default at their call site
/// via [`step_model_or`].
pub fn step_model(state: &AppState, step: crate::project::StepKind) -> String {
    state
        .projects
        .active()
        .map(|p| p.model_for_step(step).to_string())
        .unwrap_or_else(|| crate::llm::DEFAULT_MODEL.to_string())
}

/// UI-picked step resolution: the caller's explicit `requested` model wins when non-empty;
/// otherwise fall back to the active project's per-step default (see [`step_model`]). Used
/// by the audit / calibration / research-chat steps, where the UI still owns the
/// speed-vs-thoroughness override but the project supplies the default.
pub fn step_model_or(
    state: &AppState,
    step: crate::project::StepKind,
    requested: Option<&str>,
) -> String {
    match requested.map(str::trim).filter(|m| !m.is_empty()) {
        Some(m) => m.to_string(),
        None => step_model(state, step),
    }
}

/// The active project's explicit fix-stage model, or `None` when it has none (or there is no
/// active project). Unlike the other steps, the fix step is UNSET by default: an unset fix model
/// lets the `fix_on_haiku` efficiency measure decide (see
/// [`crate::ai_audit::resolve_fix_model`]), which a seeded default would silently override.
pub fn fix_step_model(state: &AppState) -> Option<String> {
    state
        .projects
        .active()
        .map(|p| {
            p.model_for_step(crate::project::StepKind::Fix)
                .trim()
                .to_string()
        })
        .filter(|m| !m.is_empty())
}

/// The clone judge's model for the active project: its fast (Haiku) band primary, or `None` when
/// there is no active project (the built-in default then applies). The judge is classification
/// over bounded input, the fast band's job; the project's tier map is where that band is set.
pub fn clone_judge_step_model(state: &AppState) -> Option<String> {
    state
        .projects
        .active()
        .map(|p| p.tier_map.fast_primary().trim().to_string())
        .filter(|m| !m.is_empty())
}

/// Body for `POST /api/projects/:id/step-models`: which NON-FLEET AI step and the model id
/// to bind to it. Patch semantics — one step per call, the others left untouched.
#[derive(serde::Deserialize)]
pub(crate) struct SetStepModelReq {
    /// The step key: `audit` | `calibration` | `fix` | `research_chat` | `story_authoring` |
    /// `decomposition` | `escalation` | `clarification`.
    pub(crate) step: String,
    /// The model id to bind (e.g. `claude-opus-5`). Blank is accepted for `fix` only, where it
    /// clears the setting back to the default (see [`fix_step_model`]).
    pub(crate) model: String,
}

/// Parse a step key from the request body into a [`StepKind`]. Tolerant of dash/space
/// separators (e.g. `research-chat`). Returns `None` for an unknown key.
pub(crate) fn parse_step_kind(s: &str) -> Option<crate::project::StepKind> {
    use crate::project::StepKind;
    match s
        .trim()
        .to_ascii_lowercase()
        .replace([' ', '-'], "_")
        .as_str()
    {
        "audit" => Some(StepKind::Audit),
        "calibration" => Some(StepKind::Calibration),
        "fix" => Some(StepKind::Fix),
        "research_chat" => Some(StepKind::ResearchChat),
        "story_authoring" => Some(StepKind::StoryAuthoring),
        "decomposition" => Some(StepKind::Decomposition),
        "escalation" => Some(StepKind::Escalation),
        "clarification" => Some(StepKind::Clarification),
        _ => None,
    }
}

/// `POST /api/projects/:id/step-models` — set the model for ONE non-fleet AI step on this
/// project. Patch semantics (one step per call); mirrors the tier-map write path —
/// mutates only the named project and persists. A blank `model` (except for `fix`, where it
/// clears the setting) or unknown `step` is a no-op error response (never silently mutates).
pub(crate) async fn set_step_model(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SetStepModelReq>,
) -> Json<serde_json::Value> {
    let Some(step) = parse_step_kind(&req.step) else {
        return Json(serde_json::json!({ "ok": false, "message": "unknown step" }));
    };
    let model = req.model.trim().to_string();
    if model.is_empty() && step != crate::project::StepKind::Fix {
        return Json(serde_json::json!({ "ok": false, "message": "model must not be empty" }));
    }
    match state.projects.set_step_model(&id, step, model) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// Body for `POST /api/projects/:id/backend`: the project's new backend setting
/// (`docs/design/2026-09-22_per-project-backend.md`). `backend` must be `"cli"` or `"api"`
/// (case-insensitive, trimmed); anything else is rejected with `{ ok: false, message }`.
#[derive(serde::Deserialize)]
pub(crate) struct SetProjectBackendReq {
    pub(crate) backend: String,
}

/// `POST /api/projects/:id/backend` — set the project's AI backend. `Cli` (the default) runs
/// on the Claude Code subscription (no key needed); `Api` runs on the Anthropic API and
/// hard-blocks — never silently falls back to the CLI — when no key is configured. This is
/// the source of truth for EVERY project-scoped model call (scan, alternative
/// recommendation, rescan, governed dev loop). Mirrors `set_step_model`'s shape exactly:
/// same `State`/`Path`/`Json` extraction, same `{ ok, project }` / `{ ok: false, message }`
/// response shape, no HTTP-status signaling — the UI reads `ok` + `message`.
pub(crate) async fn set_backend_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SetProjectBackendReq>,
) -> Json<serde_json::Value> {
    let backend = match req.backend.trim().to_ascii_lowercase().as_str() {
        "cli" => crate::project::ProjectBackend::Cli,
        "api" => crate::project::ProjectBackend::Api,
        _ => {
            return Json(serde_json::json!({
                "ok": false,
                "message": format!("invalid backend: {:?} (expected \"cli\" or \"api\")", req.backend)
            }));
        }
    };
    match state.projects.set_backend(&id, backend) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// Body for `POST /api/projects/:id/client-engagement`.
#[derive(serde::Deserialize)]
pub(crate) struct SetClientEngagementReq {
    pub(crate) client_engagement: bool,
    #[serde(default)]
    pub(crate) client_tooling_opt_in: bool,
}

/// `POST /api/projects/:id/client-engagement` — mark a project as a client engagement or not,
/// and set the operator's opt-in to run the client's own tooling (see `crate::client_lock`).
/// Same `{ ok, project }` / `{ ok: false, message }` shape as `set_backend_handler`.
pub(crate) async fn set_client_engagement_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SetClientEngagementReq>,
) -> Json<serde_json::Value> {
    match state.projects.set_client_engagement(
        &id,
        req.client_engagement,
        req.client_tooling_opt_in,
    ) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// Body for `POST /api/projects/:id/stall-thresholds`.
#[derive(serde::Deserialize)]
pub(crate) struct SetStallThresholdsReq {
    pub(crate) watched_secs: u64,
    pub(crate) routine_secs: u64,
}

/// `POST /api/projects/:id/stall-thresholds` — update the per-project stall-detection
/// thresholds. Both values must be positive; a zero value is rejected.
pub(crate) async fn set_stall_thresholds_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SetStallThresholdsReq>,
) -> Json<serde_json::Value> {
    if req.watched_secs == 0 || req.routine_secs == 0 {
        return Json(serde_json::json!({ "ok": false, "message": "thresholds must be > 0" }));
    }
    let thresholds = crate::project::StallThresholds {
        watched_secs: req.watched_secs,
        routine_secs: req.routine_secs,
    };
    match state.projects.set_stall_thresholds(&id, thresholds) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

// ── Audit-integrated alternative recommendation ───────────────────────────────
// (docs/design/2026-09-22_audit-integrated-alternatives.md)

/// One rule the operator wants forced to a specific option for a targeted rescan.
#[derive(serde::Deserialize)]
pub(crate) struct AlternativeOverride {
    pub(crate) rule_id: String,
    pub(crate) chosen_option_id: String,
}

/// Body for `POST /api/projects/:id/rescan-alternatives`.
#[derive(serde::Deserialize)]
pub(crate) struct RescanAlternativesReq {
    /// Go ahead when some of the project's repos have no local clone. Without it the rescan is
    /// refused and says which repos are missing.
    #[serde(default)]
    pub(crate) allow_partial: bool,
    #[serde(default)]
    pub(crate) overrides: Vec<AlternativeOverride>,
}

/// `POST /api/projects/:id/rescan-alternatives` — re-run the AI audit for ONLY the given
/// rules, FORCED to the operator's chosen option each (never asking the model to re-pick —
/// the operator already decided). Returns the updated findings for exactly those rules,
/// tagged `evaluated_option_id = chosen_option_id`, so the caller can merge them in place
/// over the existing findings table.
///
/// Validates every `rule_id` resolves to a real multi-option semantic rule in the corpus and
/// every `chosen_option_id` is a real option on it BEFORE any model call — an invalid
/// override is reported in `message` and simply excluded from the batch (the valid ones still
/// run). Routes through the SAME compliance-safety gate as the main audit
/// (`resolve_backend_for_project` -> `Blocked` refuses with the message; `Api`/`Cli` build the
/// `Llm` on that EXACT resolved transport) and builds its `Llm` via
/// `onboard::build_llm` (with the key from `credentials::anthropic_api_key`), so this endpoint's spend folds into the same cumulative
/// ledger as every other model call.
///
/// Also updates this project's stored `last_scan` in place: the rescanned rules' findings
/// replace their previous entries and `recommendations` is updated for those rule ids
/// (marked `operator_chosen: true`) — so an immediate `accept-alternatives` call sees the
/// operator's override without requiring the caller to round-trip anything back to the server.
pub(crate) async fn rescan_alternatives_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<RescanAlternativesReq>,
) -> Json<serde_json::Value> {
    let Some(project) = state.projects.get(&id) else {
        return Json(serde_json::json!({ "ok": false, "message": "no such project" }));
    };
    // Re-reads the project's source and calls the model, all inside this request: while it
    // does, deleting that source is refused rather than raced.
    let _scan_claim = state.source_ops.begin_scan(&project.id);

    // Dedup by rule id (uppercased) — last override for a given rule wins. Blank ids/options
    // are dropped rather than causing a hard error (defensive against a malformed request).
    let mut forced: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for o in &req.overrides {
        let rid = o.rule_id.trim();
        let opt = o.chosen_option_id.trim();
        if rid.is_empty() || opt.is_empty() {
            continue;
        }
        forced.insert(rid.to_ascii_uppercase(), opt.to_string());
    }
    if forced.is_empty() {
        return Json(serde_json::json!({
            "ok": false,
            "message": "No valid overrides supplied — each entry needs a non-empty rule_id and chosen_option_id."
        }));
    }

    let corpus_path = camerata_rules::corpus_path();
    if !corpus_path.exists() {
        return Json(serde_json::json!({ "ok": false, "message": "Rule corpus unavailable." }));
    }
    let (corpus, _errs) = camerata_rules::load_corpus_lenient(&corpus_path).await;

    // Validate every override AGAINST the corpus before any model call. A rule id that isn't a
    // real multi-option semantic rule, or an option id that isn't real on that rule, is
    // reported and excluded — the remaining valid overrides still run.
    let chosen_options = project_chosen_options(&project);
    let mut alternatives_by_rule: std::collections::HashMap<
        String,
        crate::ai_audit::RuleAlternatives,
    > = std::collections::HashMap::new();
    let mut invalid: Vec<String> = Vec::new();
    for (rid, opt_id) in &forced {
        match corpus.get_by_id(rid) {
            Some(rule) if crate::onboard::is_semantic_multi_option_rule(rule) => {
                let options: Vec<crate::onboard::RuleOptionView> = rule
                    .options
                    .iter()
                    .map(|o| crate::onboard::RuleOptionView {
                        id: o.id.clone(),
                        label: o.label.clone(),
                        directive: o.directive.clone(),
                        why: o.why.clone(),
                        violates: o.violates,
                    })
                    .collect();
                if !options.iter().any(|o| &o.id == opt_id) {
                    invalid.push(format!(
                        "{rid}: \"{opt_id}\" is not a real option on this rule"
                    ));
                    continue;
                }
                let selected_option_id = chosen_options
                    .get(rid)
                    .cloned()
                    .or_else(|| rule.default_option.clone());
                alternatives_by_rule.insert(
                    rid.clone(),
                    crate::ai_audit::RuleAlternatives {
                        rule_id: rid.clone(),
                        options,
                        selected_option_id,
                        // A5.3: same corpus-kind threading as build_rule_alternatives.
                        kind: rule.kind(),
                    },
                );
            }
            Some(_) => invalid.push(format!("{rid}: not a multi-option semantic rule")),
            None => invalid.push(format!("{rid}: unknown rule id")),
        }
    }
    if alternatives_by_rule.is_empty() {
        return Json(serde_json::json!({
            "ok": false,
            "message": format!("No valid rule overrides: {}", invalid.join("; "))
        }));
    }
    // Only the validated subset of `forced` drives the audit (an invalid override must never
    // reach `audit_repo`, which would otherwise fall back silently — reject it here instead).
    let forced: std::collections::HashMap<String, String> = forced
        .into_iter()
        .filter(|(rid, _)| alternatives_by_rule.contains_key(rid))
        .collect();

    // The client-engagement lock, before any repository is read (the same check the main
    // audit runs, over the same step models this rescan uses below).
    if let Some(why) = client_lock_refusal_for_scan(
        Some(&project),
        Some(&step_model_or(
            &state,
            crate::project::StepKind::Audit,
            None,
        )),
        Some(&step_model_or(
            &state,
            crate::project::StepKind::Calibration,
            None,
        )),
        false,
        fix_step_model(&state).as_deref(),
    ) {
        return Json(serde_json::json!({ "ok": false, "message": why }));
    }
    // The SAME compliance-safety gate the main audit uses.
    let backend_resolution = resolve_backend_for_project(&state, Some(&project));
    let llm_backend = match &backend_resolution {
        crate::llm::BackendResolution::Blocked { message } => {
            return Json(serde_json::json!({ "ok": false, "message": message }));
        }
        crate::llm::BackendResolution::Api => crate::llm::Backend::Api,
        crate::llm::BackendResolution::Cli => crate::llm::Backend::Cli,
    };
    let mut run_notes: Vec<String> = invalid
        .iter()
        .map(|m| format!("skipped invalid override — {m}"))
        .collect();

    // Same session bookkeeping as `audit_repos`: this rescan re-reads client source into model
    // prompts, so the Claude Code sessions it spawns belong on the inspection entry too.
    let model_sessions = crate::llm::SessionSink::new();
    let llm = crate::onboard::build_llm(
        state.credential_store.as_ref(),
        llm_backend,
        Some(state.usage_ledger.clone()),
    )
    .with_session_sink(model_sessions.clone());
    let LocalSources {
        sources,
        notes: source_notes,
        unresolved,
    } = resolve_local_sources(&state, &project.repos);
    if let Some(why) = partial_run_refusal(&unresolved, project.repos.len(), req.allow_partial) {
        return Json(serde_json::json!({ "ok": false, "message": why }));
    }
    run_notes.extend(source_notes);

    let audit_model = Some(step_model_or(&state, crate::project::StepKind::Audit, None));
    let calibration_model = Some(step_model_or(
        &state,
        crate::project::StepKind::Calibration,
        None,
    ));

    // Which rule ids apply to a given repo, per the project's own per-rule `repos` binding
    // (empty `repos` on a `RuleSelection` = project-level = applies to every repo).
    let applies_to_repo = |rid: &str, repo: &str| -> bool {
        [
            &project.ruleset.selections,
            &project.ruleset.cross_repo,
            &project.ruleset.process,
        ]
        .iter()
        .flat_map(|list| list.iter())
        .find(|s| s.rule_id.trim().to_ascii_uppercase() == *rid)
        .map(|s| s.repos.is_empty() || s.repos.iter().any(|r| r == repo))
        // A rule not found in the ruleset at all (shouldn't happen — it was already
        // audited once to have surfaced as an override candidate) defaults to
        // project-level so a rescan is never silently dropped.
        .unwrap_or(true)
    };

    let rescan_meter =
        crate::ai_audit::UsageMeter::with_controls(crate::cost_cap::RunControls::from_env());
    let mut all_findings: Vec<crate::onboard::Finding> = Vec::new();
    // A10: the rows this rescan's audit threw out, kept apart from the rows that ship.
    let mut all_discarded: Vec<crate::onboard::Finding> = Vec::new();
    // The repos whose rescan audit returned Ok, each with the rule ids it re-evaluated: the only
    // (rule, repo) pairs whose old rows the fold may replace.
    let mut refreshed: Vec<(String, Vec<String>)> = Vec::new();
    let mut all_recs: std::collections::HashMap<String, crate::ai_audit::RuleRecommendation> =
        std::collections::HashMap::new();
    // The same picks with the repo each was made for, one per (rule, repo): what the stored
    // report's per-repo list is updated from, so a multi-repo workbook never prints the pick a
    // rescan replaced.
    let mut all_recs_by_repo: Vec<crate::ai_audit::RuleRecommendation> = Vec::new();
    // W6: a sub-pass (e.g. a nested alternative-recommendation call) failing here must never
    // be silent either — surfaced in `run_notes` below and merged into the stored report.
    let mut all_failed_passes: Vec<crate::ai_audit::FailedPass> = Vec::new();
    for (spec, dir) in &sources {
        let applicable_ids: Vec<String> = alternatives_by_rule
            .keys()
            .filter(|rid| applies_to_repo(rid, spec))
            .cloned()
            .collect();
        if applicable_ids.is_empty() {
            continue;
        }
        let dir = dir.clone();
        let files =
            match tokio::task::spawn_blocking(move || crate::onboard::read_local_repo_files(&dir))
                .await
            {
                Ok(Ok(extracted)) => extracted.files,
                Ok(Err(e)) => {
                    run_notes.push(format!("{spec}: {e}"));
                    continue;
                }
                Err(e) => {
                    run_notes.push(format!("{spec}: rescan task failed ({e})"));
                    continue;
                }
            };
        let alt_slice: Vec<crate::ai_audit::RuleAlternatives> = applicable_ids
            .iter()
            .filter_map(|rid| alternatives_by_rule.get(rid).cloned())
            .collect();
        let selected_pairs: Vec<(String, String)> = alt_slice
            .iter()
            .map(|a| {
                let forced_id = forced.get(&a.rule_id).cloned().unwrap_or_default();
                let directive = a
                    .options
                    .iter()
                    .find(|o| o.id == forced_id)
                    .map(|o| o.directive.clone())
                    .unwrap_or_default();
                (a.rule_id.clone(), directive)
            })
            .collect();
        let audit_rules = crate::scan_routing::audit_rules(&selected_pairs, Some(&corpus));
        match crate::ai_audit::audit_repo(
            &llm,
            spec,
            &files,
            &audit_rules,
            &alt_slice,
            &forced,
            audit_model.as_deref(),
            calibration_model.as_deref(),
            crate::ai_audit::ScanMode::Parallel,
            false,
            Some((&state.transcripts, SCAN_AUDIT_KEY)),
            None,
            // A meter carries the efficiency switches from the environment (so
            // `CAMERATA_EFFICIENCY=terse_output=off` reaches this endpoint too). Without one the
            // terse scan output stays off: its detail regeneration must be metered.
            Some(&rescan_meter),
            None,
        )
        .await
        {
            Ok((findings, _proposed, recs, failed, _stage_samples)) => {
                refreshed.push((spec.to_string(), applicable_ids.clone()));
                // A10: rows the audit threw out come back marked; they are kept apart from the
                // rows that ship and folded into the stored report's `discarded` below.
                let (thrown_out, findings) = crate::engine_discard::split_discarded(findings);
                all_discarded.extend(thrown_out);
                all_findings.extend(findings);
                for mut r in recs {
                    r.repo = spec.to_string();
                    all_recs.insert(r.rule_id.clone(), r.clone());
                    all_recs_by_repo.push(r);
                }
                for f in &failed {
                    run_notes.push(format!("{spec}: {} not computed ({})", f.pass, f.reason));
                }
                all_failed_passes.extend(failed);
            }
            Err(e) => run_notes.push(format!("{spec}: rescan failed ({e})")),
        }
    }

    state.record_model_sessions(&id, model_sessions.snapshot());

    // A rescan builds Findings without going through `audit_repos`, so it must apply the
    // corpus-authored severity bounds itself: the findings returned below AND the ones folded
    // into the stored scan are what the triage view shows, and they must carry the same
    // severity the PDF/xlsx will bill. See `severity_bounds`.
    crate::severity_bounds::apply_corpus_severity_bounds(&mut all_findings, &corpus);

    // Merge into the project's stored last_scan so an immediate accept-alternatives call
    // (or a UI reload) sees the operator's override without any client round-trip.
    if let Some(mut report) = state.get_last_scan(&id) {
        report.fold_rescan_rows(
            &refreshed,
            all_findings.clone(),
            std::mem::take(&mut all_discarded),
        );
        report.fold_rescan_recommendations(all_recs_by_repo.clone());
        // W6: fold any freshly-failed pass into the stored report so a LATER export still
        // discloses it — never let a rescan's failure disappear once this handler returns.
        report.failed_passes.extend(all_failed_passes.clone());
        // Same inspection, a few rules re-evaluated: amend the current persisted entry rather
        // than appending a second one.
        state.amend_last_scan(id.clone(), report);
    }

    Json(serde_json::json!({
        "ok": true,
        "message": (!run_notes.is_empty()).then(|| run_notes.join(" · ")),
        "findings": all_findings,
        "recommendations": all_recs,
    }))
}

/// Body for `POST /api/projects/:id/accept-alternatives`. Empty `rule_ids` (or an absent
/// field, `{}`) means "accept all" — every rule in the last scan's `recommendations` map.
#[derive(serde::Deserialize, Default)]
pub(crate) struct AcceptAlternativesReq {
    #[serde(default)]
    pub(crate) rule_ids: Vec<String>,
}

/// `POST /api/projects/:id/accept-alternatives` — persist the CURRENT per-rule selection (the
/// AI's recommendation where the operator never overrode it, the operator's forced choice
/// where a `rescan-alternatives` call already updated it) into `RuleSelection.chosen_option`,
/// via the existing `ProjectStore` mutator path. Body `{}` accepts every rule in the last
/// scan's `recommendations` map; `{ "rule_ids": [...] }` accepts only the named subset.
/// Returns the updated ruleset. 404s (via `ok: false`, no HTTP-status signaling — matching
/// every other per-project mutator in this file) when the project has no completed scan to
/// read recommendations from.
pub(crate) async fn accept_alternatives_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<AcceptAlternativesReq>,
) -> Json<serde_json::Value> {
    if state.projects.get(&id).is_none() {
        return Json(serde_json::json!({ "ok": false, "message": "no such project" }));
    }
    let Some(report) = state.get_last_scan(&id) else {
        return Json(serde_json::json!({
            "ok": false,
            "message": "No scan results available for this project. Run an audit first."
        }));
    };
    let target_ids: Option<std::collections::HashSet<String>> =
        (!req.rule_ids.is_empty()).then(|| {
            req.rule_ids
                .iter()
                .map(|s| s.trim().to_ascii_uppercase())
                .collect()
        });

    let mut accepted = 0usize;
    let mut skipped: Vec<String> = Vec::new();
    for (rid, rec) in &report.recommendations {
        if let Some(ids) = &target_ids {
            if !ids.contains(rid) {
                continue;
            }
        }
        match state
            .projects
            .set_rule_chosen_option(&id, rid, &rec.recommended_option_id)
        {
            Some(true) => accepted += 1,
            Some(false) => skipped.push(rid.clone()),
            None => {
                return Json(serde_json::json!({ "ok": false, "message": "no such project" }));
            }
        }
    }

    match state.projects.get(&id) {
        Some(p) => Json(serde_json::json!({
            "ok": true,
            "accepted": accepted,
            "skipped": skipped,
            "ruleset": p.ruleset,
        })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

// ── L3 agentic code-review gate configuration (R7) ────────────────────────────

/// Body for `POST /api/projects/:id/l3-review`.
///
/// `enabled` controls whether the L3 reviewer runs after each governed development
/// stage. `model` pins the reviewer to a specific model id; an empty or absent
/// `model` means "use the project's `balanced` tier model" (the fallback defined in
/// [`crate::project::Project::l3_model`]).
#[derive(serde::Deserialize)]
pub(crate) struct SetL3ReviewReq {
    /// Whether the L3 reviewer is enabled for this project.
    pub(crate) enabled: bool,
    /// The model id for the L3 reviewer. Empty string = use the balanced tier fallback.
    #[serde(default)]
    pub(crate) model: String,
    /// What a run does when the reviewer cannot run. Absent keeps the project's current value.
    #[serde(default)]
    pub(crate) on_error: Option<crate::project::ReviewOnError>,
    /// Extra implementer passes after a bounce. Absent keeps the project's current value.
    #[serde(default)]
    pub(crate) fix_rounds: Option<u8>,
    /// The reviewer's model tier when no model is pinned. Absent keeps the current value.
    #[serde(default)]
    pub(crate) tier: Option<crate::project::ReviewTier>,
}

/// `POST /api/projects/:id/l3-review` — update the per-project L3 agentic code-review
/// gate configuration.
///
/// Accepts `{ "enabled": bool, "model": "<id or empty>" }` and writes it to the project
/// store. Returns the updated project on success, or an error when the project id is not
/// found. The `model` field is optional in the request body (defaults to empty string =
/// use the balanced tier fallback).
pub(crate) async fn set_l3_review_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SetL3ReviewReq>,
) -> Json<serde_json::Value> {
    // The three reviewer-policy fields are optional on the wire, so a client that only knows
    // `{ enabled, model }` never resets them: an absent field keeps the stored value.
    let current = state
        .projects
        .get(&id)
        .map(|p| p.l3_review)
        .unwrap_or_default();
    let config = crate::project::L3ReviewConfig {
        enabled: req.enabled,
        model: req.model.trim().to_string(),
        on_error: req.on_error.unwrap_or(current.on_error),
        fix_rounds: req
            .fix_rounds
            .unwrap_or(current.fix_rounds)
            .min(camerata_api_types::project::MAX_L3_FIX_ROUNDS),
        tier: req.tier.unwrap_or(current.tier),
    };
    match state.projects.set_l3_review(&id, config) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

// ── Work hierarchy schema (design-page work-type graph) ───────────────────────

/// `GET /api/projects/:id/hierarchy` — the project's work hierarchy schema (the design-page
/// work-type graph: the types the project uses + the allowed parent→child nestings). Returns the
/// seeded default ladder for a project that never customized it.
pub(crate) async fn get_hierarchy_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    match state.projects.get(&id) {
        // Resolve empty → default ladder so a project seeded/imported without a schema still
        // reports the usable default (the same fallback design-author + materialize apply).
        Some(p) => Json(serde_json::json!({
            "ok": true,
            "hierarchy": p.hierarchy_schema.resolve_effective(),
        })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// `POST /api/projects/:id/hierarchy` — replace the project's work hierarchy schema with the posted
/// one (the drag-and-drop builder saves the whole graph). Body is a `HierarchySchema`.
pub(crate) async fn set_hierarchy_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(schema): Json<crate::project::HierarchySchema>,
) -> Json<serde_json::Value> {
    match state.projects.set_hierarchy_schema(&id, schema) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

// ── Model Efficiency Profile ──────────────────────────────────────────────────

/// Query parameters for `GET /api/projects/:id/model-profile/preview`.
#[derive(serde::Deserialize)]
pub(crate) struct PreviewProfileQuery {
    pub(crate) profile: String,
}

/// `GET /api/projects/:id/model-profile/preview?profile=X` — compute the assignments a
/// profile would produce WITHOUT applying them. Returns the proposed tier_map, step_models,
/// and l3_review so the UI can show a "current → new" confirm popup before committing.
pub(crate) async fn preview_model_profile(
    State(state): State<AppState>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<PreviewProfileQuery>,
) -> Json<serde_json::Value> {
    let profile = match q.profile.to_ascii_lowercase().as_str() {
        "max_efficiency" | "maxefficiency" => crate::project::ModelProfile::MaxEfficiency,
        "balanced" => crate::project::ModelProfile::Balanced,
        "max_quality" | "maxquality" => crate::project::ModelProfile::MaxQuality,
        "custom" => crate::project::ModelProfile::Custom,
        _ => return Json(serde_json::json!({ "ok": false, "message": "unknown profile" })),
    };
    // Verify the project exists.
    if state.projects.get(&id).is_none() {
        return Json(serde_json::json!({ "ok": false, "message": "no such project" }));
    }
    let assignments =
        crate::model_profile_cascade::compute_profile_cascade(profile, &state.model_registry);
    match assignments {
        None => Json(serde_json::json!({
            "ok": true,
            "profile": q.profile,
            "noop": true,
            "message": "Custom profile: no cascade — user owns every entry.",
        })),
        Some(a) => Json(serde_json::json!({
            "ok": true,
            "profile": q.profile,
            "noop": false,
            "assignments": {
                "tier_map": a.tier_map,
                "step_models": a.step_models,
                "l3_review": a.l3_review,
            }
        })),
    }
}

/// Body for `POST /api/projects/:id/model-profile`.
#[derive(serde::Deserialize)]
pub(crate) struct ApplyModelProfileReq {
    /// The profile to apply: `"max_efficiency"` | `"balanced"` | `"max_quality"` | `"custom"`.
    pub(crate) profile: String,
}

/// `POST /api/projects/:id/model-profile { profile }` — set the profile and apply the
/// cascade to the project's tier-map, step-models, and L3 config. Auto-saves. Returns
/// the updated project.
pub(crate) async fn apply_model_profile(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ApplyModelProfileReq>,
) -> Json<serde_json::Value> {
    let profile = match req.profile.to_ascii_lowercase().as_str() {
        "max_efficiency" | "maxefficiency" => crate::project::ModelProfile::MaxEfficiency,
        "balanced" => crate::project::ModelProfile::Balanced,
        "max_quality" | "maxquality" => crate::project::ModelProfile::MaxQuality,
        "custom" => crate::project::ModelProfile::Custom,
        _ => return Json(serde_json::json!({ "ok": false, "message": "unknown profile" })),
    };

    let assignments =
        crate::model_profile_cascade::compute_profile_cascade(profile, &state.model_registry);

    let updated = state.projects.update(&id, |p| {
        p.model_profile = profile;
        if let Some(ref a) = assignments {
            p.tier_map = a.tier_map.clone();
            p.step_models = a.step_models.clone();
            // A profile assigns the reviewer's on/off and model. The reviewer POLICY (fail
            // closed, fix rounds, tier) is the architect's own choice and is kept.
            let policy = p.l3_review.clone();
            p.l3_review = crate::project::L3ReviewConfig {
                on_error: policy.on_error,
                fix_rounds: policy.fix_rounds,
                tier: policy.tier,
                ..a.l3_review.clone()
            };
        }
    });

    match updated {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

// ── VCS-gate process-rule configuration + auditable bypass (issue #65) ────────

/// `GET /api/projects/:id/process-rule-config` — read the project's current VCS-gate
/// process-rule configuration.
pub(crate) async fn get_process_rule_config(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    match state.projects.get(&id) {
        Some(p) => Json(serde_json::json!({
            "ok": true,
            "process_rule_config": p.process_rule_config,
        })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// `POST /api/projects/:id/process-rule-config` — replace the project's VCS-gate
/// process-rule configuration. The full [`ProcessRuleConfig`] document is expected
/// in the request body (partial updates are not supported; send the full object).
pub(crate) async fn set_process_rule_config_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(config): Json<camerata_checks::vcs_action::ProcessRuleConfig>,
) -> Json<serde_json::Value> {
    match state
        .projects
        .update(&id, |p| p.set_process_rule_config(config))
    {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// Request body for `POST /api/projects/:id/vcs-gate/bypass`.
#[derive(serde::Deserialize)]
pub(crate) struct VcsGateBypassReq {
    /// The [`VcsAction`] to evaluate.
    pub(crate) action: serde_json::Value,
    /// Non-empty bypass reason. A missing or empty reason is rejected (the same
    /// invariant as the suppression-waiver: a reason-less bypass is a hard error).
    pub(crate) reason: String,
}

/// `POST /api/projects/:id/vcs-gate/bypass` — evaluate a VCS action with an
/// auditable bypass.
///
/// Intended use: when Camerata's orchestration code knows that a specific action
/// cannot satisfy the active process rules for a documented, legitimate reason
/// (e.g. a machine-generated merge commit or a one-time onboarding branch), it
/// calls this endpoint instead of the normal gate path. The caller supplies the
/// action metadata AND a non-empty reason; the endpoint records the bypass so it
/// is visible in the evidence trail.
///
/// - Empty or missing `reason` → `400 Bad Request` (bypass without justification
///   is itself a gate violation).
/// - Action already passes the gate → `200 ok: true, bypassed: false` (no bypass
///   record produced; the action is clean).
/// - Action fails + reason present → `200 ok: true, bypassed: true, record: {...}`.
pub(crate) async fn vcs_gate_bypass(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<VcsGateBypassReq>,
) -> Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use camerata_checks::vcs_action::{
        build_rules, gate_or_bypass, BypassRequest, GateOrBypassResult, VcsAction,
    };

    // Reason-less bypass is rejected immediately (mirrors the suppression-waiver
    // invariant; a bypass must be auditable or it is not a bypass at all).
    let reason = req.reason.trim().to_string();
    if reason.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "ok": false,
                "message": "bypass rejected: a non-empty reason is required \
                            (mirror of the suppression-waiver invariant)"
            })),
        )
            .into_response();
    }

    // Look up the project and build its live rule set.
    let Some(project) = state.projects.get(&id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "ok": false, "message": "no such project" })),
        )
            .into_response();
    };

    let rules = build_rules(&project.process_rule_config);

    // Parse the action from the request. We accept the same JSON shape as
    // VcsAction's serde representation.
    let action: VcsAction = match serde_json::from_value(req.action) {
        Ok(a) => a,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "ok": false,
                    "message": format!("could not parse action: {e}")
                })),
            )
                .into_response();
        }
    };

    let bypass_req = BypassRequest { reason };
    match gate_or_bypass(&rules, &action, Some(&bypass_req)) {
        Ok(GateOrBypassResult::Passed) => {
            Json(serde_json::json!({ "ok": true, "bypassed": false })).into_response()
        }
        Ok(GateOrBypassResult::Bypassed(record)) => Json(serde_json::json!({
            "ok": true,
            "bypassed": true,
            "record": record,
        }))
        .into_response(),
        Ok(GateOrBypassResult::Failed(violations)) => {
            // Should be unreachable (bypass always converts Failed to Bypassed when
            // the reason is non-empty), but handle it defensively.
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "ok": false,
                    "message": "gate failed and bypass could not be applied",
                    "violations": violations.iter().map(|v| &v.detail).collect::<Vec<_>>(),
                })),
            )
                .into_response()
        }
        Err(_reason_required) => {
            // Should be unreachable (we checked `reason.is_empty()` above), but
            // handle it defensively.
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "ok": false,
                    "message": "bypass rejected: reason must not be empty"
                })),
            )
                .into_response()
        }
    }
}

/// Reconcile a project's repos with the rule-bank: read each repo's emitted gate
/// config (ground truth of what's applied) and rehydrate the full source rule
/// (alternatives + context) by id. Gated on the token (reads the repos). Returns
/// the applied rules with their source rehydrated.
pub(crate) async fn reconcile_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let Some(project) = state.projects.get(&id) else {
        return Json(serde_json::json!({ "ok": false, "message": "no such project" }));
    };
    // Local-first: read each repo's working copy (where emit writes by default); the token only
    // enables the GitHub governance-branch fallback for repos that aren't cloned locally.
    let token = state.github_token().unwrap_or_default();
    let workspace_root = state.settings.workspace_root();
    let sources: Vec<(String, Option<std::path::PathBuf>)> = project
        .repos
        .iter()
        .map(|r| {
            let override_path = state.settings.repo_path(r);
            let dir = crate::workspace::resolve_repo_dir(
                override_path.as_deref(),
                workspace_root.as_deref(),
                r,
            );
            (r.clone(), dir)
        })
        .collect();
    let applied = crate::reconcile::reconcile_repos_local(&sources, &token).await;
    // Adopt: mirror the repo-local selections into project state and pull custom rules back in,
    // so the selected-rules table reflects what is actually in the repos. cross_repo + process
    // are project-level (not in repo gate configs) and are left untouched. Only mutate when we
    // actually read rules — an empty read (nothing armed / no local clones) must not wipe state.
    let project = if applied.is_empty() {
        project
    } else {
        let (selections, custom) = crate::reconcile::adopt_from_applied(&applied);
        state
            .projects
            .update(&id, |p| {
                p.ruleset.selections = selections.clone();
                p.merge_custom(&custom);
            })
            .unwrap_or(project)
    };
    Json(serde_json::json!({ "ok": true, "applied": applied, "project": project }))
}

/// Connection health for the optional integrations (GitHub, Claude). Probes
/// GitHub reachability when a token is set so a 401/403/5xx surfaces as a real
/// error; integrations being absent is reported as "not configured" (a warning,
/// not an error, in the UI).
pub(crate) async fn connections_status(
    State(state): State<AppState>,
) -> Json<crate::connections::ConnectionsReport> {
    Json(crate::connections::probe(state.credential_store.as_ref()).await)
}
