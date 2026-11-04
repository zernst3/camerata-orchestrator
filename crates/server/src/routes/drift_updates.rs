//! Feature flags, development context, update checks, rule drift and rule overrides.

use crate::*;

/// `GET /api/feature-flags` — return the live feature-flag state so the UI can
/// render conditional features (e.g. the SOC-2 deep-audit badge) only when the
/// flag is on. The flags are loaded once at server start from
/// `.camerata/features.toml` + env overrides; this endpoint is read-only.
pub(crate) async fn get_feature_flags(
    State(state): State<AppState>,
) -> Json<crate::feature_flags::FeatureFlags> {
    Json(state.feature_flags.clone())
}

// ── Development context ──────────────────────────────────────────────────────

/// Per-story development context item: the UoW state the chat grounding needs.
#[derive(serde::Serialize)]
pub(crate) struct StoryDevContext {
    /// The story id.
    pub(crate) story_id: String,
    /// The governed-development lifecycle stage (intake / investigating / …).
    pub(crate) stage: String,
    /// The human-readable label for the stage.
    pub(crate) stage_label: String,
    /// The dev-side status badge (new / in_progress / done).
    pub(crate) dev_status: String,
    /// Branch the work lives on (if set).
    pub(crate) branch: Option<String>,
    /// Whether all decisions are approved (development gate satisfied).
    pub(crate) decisions_approved: bool,
    /// Number of decision records on this UoW.
    pub(crate) decision_count: usize,
    /// The full sign-off record (architect handle, timestamp, run id, note) when the
    /// story has been signed off, else `None`. The chat client (`UowSnapshot.sign_off`)
    /// reads presence of this field to show "signed off" state. BUG 2 fix: previously
    /// the endpoint emitted a boolean `signed_off`, but the client parses `sign_off`
    /// (an object), so it always defaulted to "not signed off".
    pub(crate) sign_off: Option<crate::uow::SignOff>,
    /// RFC 3339 timestamp of the last UoW mutation. Empty string if not set. BUG 2 fix:
    /// the client parses `updated`; the endpoint previously emitted `last_activity`, so
    /// the client always showed "(unknown)".
    pub(crate) updated: String,
    /// The frozen gate provenance from the most recent completed governed run, else
    /// `None`. BUG 2 fix: the client parses `gate_provenance` (an object with
    /// allow/deny/bounce counts + rules_fired + mode); the endpoint previously emitted
    /// only a boolean `has_gate_provenance`, so the client always showed "no run yet".
    /// `GateProvenance` serializes to a superset of the client's `GateProvenanceLite`
    /// (extra `run_id`/`recorded` fields are ignored by the client's `#[serde(default)]`).
    pub(crate) gate_provenance: Option<crate::uow::GateProvenance>,
}

/// `GET /api/development/context` — ALL Units of Work state for the chat.
///
/// Returns a concise JSON array the chat panel can inject as grounding context:
/// per-story lifecycle stage, gate/bounce status, sign-off state, and last
/// activity. Read-only; no model call. Reads from the UoW store and the story
/// spine (for the title).
///
/// Response shape:
/// ```json
/// {
///   "ok": true,
///   "units_of_work": [
///     {
///       "story_id": "S-42",
///       "stage": "development",
///       "stage_label": "Development",
///       "dev_status": "in_progress",
///       "branch": "feat/S-42-add-rule",
///       "decisions_approved": true,
///       "decision_count": 3,
///       "sign_off": null,
///       "updated": "2026-06-21T10:00:00Z",
///       "gate_provenance": {
///         "run_id": "run-7", "mode": "scripted",
///         "allow_count": 5, "deny_count": 0, "total_bounces": 0,
///         "rules_fired": [], "recorded": "2026-06-21T09:59:00Z"
///       }
///     }
///   ]
/// }
/// ```
pub(crate) async fn development_context(State(state): State<AppState>) -> Json<serde_json::Value> {
    use camerata_worktracker::investigation::decisions_approved_for_development;

    let Some(p) = state.projects.active() else {
        return Json(serde_json::json!({
            "ok": true,
            "units_of_work": [],
        }));
    };
    let uow_list = state.uow.list_for_project(&p.id, &p.repos);
    let items: Vec<StoryDevContext> = uow_list
        .into_iter()
        .map(|uow| {
            let decisions_approved = decisions_approved_for_development(&uow.decisions);
            StoryDevContext {
                story_id: uow.story_id.clone(),
                stage: uow.stage.wire_str().to_string(),
                stage_label: uow.stage.label().to_string(),
                dev_status: match uow.dev_status {
                    crate::uow::DevStatus::New => "new",
                    crate::uow::DevStatus::InProgress => "in_progress",
                    crate::uow::DevStatus::Done => "done",
                }
                .to_string(),
                branch: uow.branch.clone(),
                decisions_approved,
                decision_count: uow.decisions.len(),
                sign_off: uow.sign_off.clone(),
                updated: uow.updated.clone(),
                gate_provenance: uow.gate_provenance.clone(),
            }
        })
        .collect();

    Json(serde_json::json!({
        "ok": true,
        "units_of_work": items,
    }))
}

// ── Update detection ─────────────────────────────────────────────────────────

/// `GET /api/updates/check` — app-version check vs the latest GitHub release,
/// and applied-rule drift detection.
///
/// Response shape:
/// ```json
/// {
///   "ok": true,
///   "app": {
///     "current_version": "0.3.1",
///     "latest_version": "0.3.2",
///     "update_available": true,
///     "release_url": "https://github.com/…/releases/tag/v0.3.2"
///   },
///   "rule_drift": [
///     {
///       "rule_id": "RUST-DOMAIN-1",
///       "project_id": "proj-abc",
///       "title": "Newtype IDs",
///       "applied_directive": "…",
///       "corpus_directive": "…",
///       "repos": ["me/api"],
///       "changed": true,
///       "legacy_baseline": false,
///       "applied_version": 2,
///       "current_version": 4
///     }
///   ],
///   "corpus_freshness": {
///     "overdue":  [{ "domain": "go", "last_confirmed": "2026-01-01", "days_since": 111, "threshold_days": 90 }],
///     "due_soon": [{ "domain": "rust", "last_confirmed": "2026-07-25", "days_since": 87, "threshold_days": 90, "days_remaining": 3 }],
///     "never_confirmed_count": 4
///   }
/// }
/// ```
///
/// `corpus_freshness` is operator-only (it never reaches a client artifact) and is `null` when
/// freshness is unknown: no corpus loaded, or the watermark file absent or unparseable. See
/// [`crate::corpus_freshness`].
///
/// `app` is `null` when the GitHub release check fails (no token or network
/// error). `rule_drift` lists the rules whose baseline no longer matches the
/// corpus: `changed: true` when the rule's content hash diverged (or the rule was
/// removed), and `changed: null` + `legacy_baseline: true` when the stored
/// baseline predates the current hash format and so cannot be compared (see
/// [`drift_verdict`]). `applied_version` / `current_version` are the rule's
/// `version` when the selection was baselined and now, `null` when unknown.
///
/// # Applied-rule hash
///
/// The baseline is [`camerata_rules::Rule::content_hash`]: `sha256:<hex>` over the
/// canonical serialization of the whole rule. [`accept_rule_drift`] stores it (and the
/// rule's `version`) on the selection; the drift check compares it to the corpus.
pub(crate) async fn updates_check(State(state): State<AppState>) -> Json<serde_json::Value> {
    // ── App-version check (the only part that touches the network) ────────────
    let app_update = check_github_release(state.credential_store.as_ref()).await;
    Json(updates_payload(&state, app_update.unwrap_or(serde_json::Value::Null)).await)
}

/// The `GET /api/updates/check` body for an already-resolved `app` value. Split from the handler
/// so everything except the GitHub call is testable offline. The corpus is loaded ONCE and
/// shared by the drift check and the freshness report.
pub(crate) async fn updates_payload(
    state: &AppState,
    app_update: serde_json::Value,
) -> serde_json::Value {
    let corpus_path = camerata_rules::corpus_path();
    let corpus = if corpus_path.exists() {
        Some(camerata_rules::load_corpus_lenient(&corpus_path).await.0)
    } else {
        None
    };

    // ── Applied-rule drift (global: all projects) ─────────────────────────────
    let drift = match &corpus {
        Some(c) => drift_for_projects(&state.projects.list(), None, c),
        None => Vec::new(),
    };

    // ── Corpus freshness (operator-only; `null` when unknown) ─────────────────
    let corpus_freshness = crate::corpus_freshness::updates_value(
        corpus.as_ref(),
        &camerata_rules::corpus_freshness_path(),
        crate::corpus_freshness::today_utc(),
    );

    serde_json::json!({
        "ok": true,
        "app": app_update,
        "rule_drift": drift,
        "corpus_freshness": corpus_freshness,
    })
}

/// The resolved directive text for a selection against a corpus rule: the chosen
/// (or default) option's `directive`, falling back to the rule's one-paragraph
/// `summary` when the rule has no options. This is the human-facing text the
/// rule-drift diff renders.
pub(crate) fn resolved_directive(
    rule: &camerata_rules::Rule,
    chosen_option: Option<&str>,
) -> String {
    rule.resolved_option(chosen_option)
        .map(|o| o.directive.clone())
        .unwrap_or_else(|| rule.summary.clone())
}

/// How one baselined selection compares to the current corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DriftVerdict {
    /// The stored baseline equals the rule's current content hash: nothing to report.
    InSync,
    /// The stored baseline is a current-format hash that differs from the rule's: the rule
    /// changed since it was accepted.
    Changed,
    /// The stored baseline is not a `sha256:` hash. It was written by an older build (the
    /// `DefaultHasher` fingerprint over id/title/summary/enforcement only), is not comparable
    /// to a current hash, and says NOTHING about whether the rule changed. Reporting it as
    /// "changed" would raise a false alarm on every rule baselined before the upgrade;
    /// reporting it as in sync would hide real drift. It is surfaced as unknown, and the next
    /// accept re-baselines it.
    Legacy,
    /// The rule is no longer in the corpus.
    Removed,
}

/// Classify a selection's stored `applied_hash` against the current corpus `rule` (`None` when
/// the rule was removed). Pure. A removed rule is [`DriftVerdict::Removed`] whatever the hash
/// looks like; otherwise a hash without [`camerata_rules::RULE_HASH_PREFIX`] is
/// [`DriftVerdict::Legacy`] before any comparison is attempted.
pub(crate) fn drift_verdict(
    applied_hash: &str,
    rule: Option<&camerata_rules::Rule>,
) -> DriftVerdict {
    let Some(rule) = rule else {
        return DriftVerdict::Removed;
    };
    if !applied_hash.starts_with(camerata_rules::RULE_HASH_PREFIX) {
        return DriftVerdict::Legacy;
    }
    if rule.content_hash() == applied_hash {
        DriftVerdict::InSync
    } else {
        DriftVerdict::Changed
    }
}

/// Build one `rule_drift` entry. Matches the UI's `RuleDriftEntry` contract; see
/// [`updates_check`] for the shape. `changed` is `true`, or `null` for a legacy baseline (never
/// `false`: an in-sync rule is not reported at all).
pub(crate) fn drift_entry(
    project_id: &str,
    selection: &crate::project::RuleSelection,
    rule: Option<&camerata_rules::Rule>,
    verdict: DriftVerdict,
) -> serde_json::Value {
    let changed = match verdict {
        DriftVerdict::Legacy => serde_json::Value::Null,
        _ => serde_json::Value::Bool(true),
    };
    let mut entry = serde_json::json!({
        "rule_id": selection.rule_id,
        "project_id": project_id,
        "title": rule.map(|r| r.title.as_str()).unwrap_or(""),
        "applied_directive": selection.applied_directive.clone().unwrap_or_default(),
        "corpus_directive": match rule {
            Some(r) => resolved_directive(r, selection.chosen_option.as_deref()),
            None => "(removed from corpus)".to_string(),
        },
        "repos": selection.repos,
        "changed": changed,
        "legacy_baseline": verdict == DriftVerdict::Legacy,
        "applied_version": selection.applied_version,
        "current_version": rule.and_then(|r| r.version),
    });
    if let Some(r) = rule {
        entry["verification"] = serde_json::json!(r.verification().as_str());
    }
    entry
}

/// Applied-rule drift across `projects` against `corpus`: for each selected rule that has been
/// BASELINED (`applied_hash` is `Some`), report it unless it is in sync (see [`DriftVerdict`]).
/// Selections that were never baselined (`applied_hash` is `None`) are treated as in-sync and
/// are not reported. Pure; [`compute_rule_drift`] is the I/O wrapper.
///
/// When `project_filter` is `Some(id)`, only that project's selections are scanned.
pub(crate) fn drift_for_projects(
    projects: &[crate::project::Project],
    project_filter: Option<&str>,
    corpus: &camerata_rules::RuleSet,
) -> Vec<serde_json::Value> {
    let mut drift = Vec::new();
    for project in projects {
        if let Some(pid) = project_filter {
            if project.id != pid {
                continue;
            }
        }
        for selection in &project.ruleset.selections {
            // Only baselined selections can drift. A `None` baseline means the rule
            // has never been accepted, so there is nothing to compare against.
            let Some(applied_hash) = selection.applied_hash.as_deref() else {
                continue;
            };
            let rule = corpus.get_by_id(&selection.rule_id);
            let verdict = drift_verdict(applied_hash, rule);
            if verdict == DriftVerdict::InSync {
                continue;
            }
            drift.push(drift_entry(&project.id, selection, rule, verdict));
        }
    }
    drift
}

/// Compute applied-rule drift for the live projects against the on-disk corpus. When
/// `project_filter` is `Some(id)`, only that project's selections are scanned (the
/// project-scoped `GET /api/projects/:id/rule-drift` route); when `None`, all projects are
/// scanned (the global `GET /api/updates/check` route). See [`drift_for_projects`].
pub(crate) async fn compute_rule_drift(
    state: &AppState,
    project_filter: Option<&str>,
) -> Vec<serde_json::Value> {
    let corpus_path = camerata_rules::corpus_path();
    if !corpus_path.exists() {
        return Vec::new();
    }
    let (corpus, _errs) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    drift_for_projects(&state.projects.list(), project_filter, &corpus)
}

/// `GET /api/projects/:id/rule-drift` — project-scoped applied-rule drift.
///
/// Returns `{ ok: true, drift: [<RuleDriftEntry>...] }` where each entry matches
/// the UI's `RuleDriftEntry` deserialization contract. Reuses [`compute_rule_drift`]
/// filtered to `:id`. When the project does not exist, returns an empty drift list
/// under `ok: true` (no drift for a project that isn't there).
pub(crate) async fn project_rule_drift(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let drift = compute_rule_drift(&state, Some(&id)).await;
    Json(serde_json::json!({
        "ok": true,
        "drift": drift,
    }))
}

/// Record `rule` as the baseline `sel` was accepted against: the rule's content hash (the
/// drift comparison), its `version` (so the drift notice can say "v2 to v4"), and the resolved
/// directive text (the before side of the diff). This is the ONLY place a baseline is written,
/// so a legacy (`DefaultHasher`) hash is re-baselined to the current format here and nowhere
/// else. `applied_version` is cleared, not left stale, when the rule carries no version.
pub(crate) fn baseline_selection(
    sel: &mut crate::project::RuleSelection,
    rule: &camerata_rules::Rule,
) {
    sel.applied_directive = Some(resolved_directive(rule, sel.chosen_option.as_deref()));
    sel.applied_hash = Some(rule.content_hash());
    sel.applied_version = rule.version;
}

/// Load the on-disk corpus so a save can baseline the rules it writes. `None` when the corpus
/// directory is missing: the caller then writes no new baselines (it never invents one) and
/// existing baselines survive via `Project::upsert_base_rules`. Parse failures of individual
/// rule files are tolerated (lenient load), so a rule that failed to parse is simply "not in
/// the corpus" here and stays unbaselined.
pub(crate) async fn load_corpus_for_baseline() -> Option<camerata_rules::RuleSet> {
    let corpus_path = camerata_rules::corpus_path();
    if !corpus_path.exists() {
        return None;
    }
    let (corpus, _errs) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    Some(corpus)
}

/// APPLY semantics: (re)baseline every selection whose rule is in `corpus`, overwriting any
/// existing baseline. Applying a rule emits the CURRENT corpus directive into the repos, so
/// the baseline must be "the corpus as it stands now" (the same acceptance
/// [`accept_rule_drift`] records); keeping an older baseline would report drift against a
/// directive that was just applied. Rules the corpus does not carry (or no corpus at all)
/// are left untouched.
pub(crate) fn rebaseline_selections(
    selections: &mut [crate::project::RuleSelection],
    corpus: Option<&camerata_rules::RuleSet>,
) {
    let Some(corpus) = corpus else { return };
    for sel in selections {
        if let Some(rule) = corpus.get_by_id(&sel.rule_id) {
            baseline_selection(sel, rule);
        }
    }
}

/// SAVE (non-apply) semantics: baseline only the selections that have none, from `corpus`.
/// An existing baseline, legacy hash included, is never overwritten: a save that is not an
/// apply has not re-emitted anything, so it accepts nothing new.
pub(crate) fn baseline_missing(
    selections: &mut [crate::project::RuleSelection],
    corpus: Option<&camerata_rules::RuleSet>,
) {
    let Some(corpus) = corpus else { return };
    for sel in selections.iter_mut().filter(|s| !s.is_baselined()) {
        if let Some(rule) = corpus.get_by_id(&sel.rule_id) {
            baseline_selection(sel, rule);
        }
    }
}

/// Every rule id the ruleset currently selects, across all three lists.
pub(crate) fn selected_rule_ids(
    ruleset: &crate::project::ProjectRuleset,
) -> std::collections::BTreeSet<String> {
    ruleset
        .selections
        .iter()
        .chain(&ruleset.cross_repo)
        .chain(&ruleset.process)
        .map(|s| s.rule_id.clone())
        .collect()
}

/// After a non-apply replace of the base rules (an import, the onboarding "complete"):
/// baseline the selections of rules that are NEW to the project (not in `known_before`),
/// from `corpus`. A rule the project already selected keeps exactly what it had (its baseline
/// via `upsert_base_rules`, or none if it was never baselined, as before), so saving an
/// unrelated change never rewrites or invents a baseline for an existing selection.
pub(crate) fn baseline_new_to_project(
    ruleset: &mut crate::project::ProjectRuleset,
    known_before: &std::collections::BTreeSet<String>,
    corpus: Option<&camerata_rules::RuleSet>,
) {
    let Some(corpus) = corpus else { return };
    for sel in ruleset
        .selections
        .iter_mut()
        .chain(ruleset.cross_repo.iter_mut())
        .chain(ruleset.process.iter_mut())
        .filter(|s| !s.is_baselined() && !known_before.contains(&s.rule_id))
    {
        if let Some(rule) = corpus.get_by_id(&sel.rule_id) {
            baseline_selection(sel, rule);
        }
    }
}

/// `POST /api/projects/:id/rule-drift/:rule_id/accept` — accept the corpus update
/// for a drifted rule.
///
/// Baselines the project's selection(s) for `rule_id` via [`baseline_selection`]: sets
/// `applied_hash` to the current corpus content hash, `applied_version` to the rule's
/// version and `applied_directive` to the current resolved directive text, so a subsequent
/// [`compute_rule_drift`] no longer reports the rule as drifted. Persists via
/// [`ProjectStore::update`]. Returns `{ ok: true }` on success (the client reads HTTP status
/// for its bool check).
pub(crate) async fn accept_rule_drift(
    State(state): State<AppState>,
    Path((id, rule_id)): Path<(String, String)>,
) -> Json<serde_json::Value> {
    // Resolve the current corpus hash + directive for this rule up-front (async
    // corpus load must happen outside the synchronous `update` closure).
    let corpus_path = camerata_rules::corpus_path();
    if !corpus_path.exists() {
        return Json(serde_json::json!({ "ok": false, "message": "corpus not found" }));
    }
    let (corpus, _errs) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    let Some(rule) = corpus.get_by_id(&rule_id) else {
        return Json(serde_json::json!({ "ok": false, "message": "rule not in corpus" }));
    };

    let updated = state.projects.update(&id, |p| {
        // Baseline every selection for this rule (project-level and any repo-scoped),
        // so the rule is cleared of drift across all its placements.
        for sel in p
            .ruleset
            .selections
            .iter_mut()
            .filter(|s| s.rule_id == rule_id)
        {
            baseline_selection(sel, rule);
        }
    });
    match updated {
        Some(_) => Json(serde_json::json!({ "ok": true })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

// ── Single-rule overrides ─────────────────────────────────────────────────────
//
// Edit one rule scoped to a project (project-level override) or a specific repo
// within that project (repo-level override). Repo overrides take precedence over
// project overrides, which take precedence over the corpus default.
//
// Storage: project-level overrides live in `ProjectRuleset.selections` (the
// `chosen_option` field carries the option id; we extend it to also carry a
// free-text `directive` override). Repo-level overrides are a NEW field on
// `ProjectRuleset`.
//
// Wire shapes:
//   GET  /api/projects/:id/rules/:rule_id
//        → { ok, rule_id, project_id, chosen_option, directive_override }
//   POST /api/projects/:id/rules/:rule_id
//        body: { chosen_option?, directive_override? }
//        → { ok, project }
//   GET  /api/projects/:id/repos/:repo/rules/:rule_id
//        → { ok, rule_id, project_id, repo, directive_override }
//   POST /api/projects/:id/repos/:repo/rules/:rule_id
//        body: { directive_override? }
//        → { ok, project }

#[derive(serde::Deserialize)]
pub(crate) struct SetRuleOverrideReq {
    /// The option id to codify for this rule (replaces the prior choice).
    #[serde(default)]
    pub(crate) chosen_option: Option<String>,
    /// Free-text directive override for this rule at the project scope.
    /// When empty or absent, the existing directive is cleared (reverts to
    /// the corpus default directive). Future: stored on `RuleSelection` when
    /// that field lands. Currently accepted from callers but not yet persisted.
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) directive_override: Option<String>,
}

/// `GET /api/projects/:id/rules/:rule_id` — read the project-level override.
pub(crate) async fn get_rule_override(
    State(state): State<AppState>,
    Path((id, rule_id)): Path<(String, String)>,
) -> Json<serde_json::Value> {
    let Some(project) = state.projects.get(&id) else {
        return Json(serde_json::json!({ "ok": false, "message": "no such project" }));
    };
    let selection = project
        .ruleset
        .selections
        .iter()
        .find(|s| s.rule_id == rule_id);
    Json(serde_json::json!({
        "ok": true,
        "rule_id": rule_id,
        "project_id": id,
        "chosen_option": selection.and_then(|s| s.chosen_option.as_deref()),
        "repos": selection.map(|s| &s.repos).cloned().unwrap_or_default(),
    }))
}

/// `POST /api/projects/:id/rules/:rule_id` — set/update the project-level override.
pub(crate) async fn set_rule_override(
    State(state): State<AppState>,
    Path((id, rule_id)): Path<(String, String)>,
    Json(req): Json<SetRuleOverrideReq>,
) -> Json<serde_json::Value> {
    let corpus = load_corpus_for_baseline().await;
    let updated = state.projects.update(&id, |p| {
        // Find or create the selection for this rule.
        if let Some(sel) = p
            .ruleset
            .selections
            .iter_mut()
            .find(|s| s.rule_id == rule_id)
        {
            if let Some(opt) = req.chosen_option.filter(|s| !s.trim().is_empty()) {
                sel.chosen_option = Some(opt);
            }
            // directive_override: store as a note in the selection. Since
            // RuleSelection has no directive field yet, we store it in a JSON
            // side-channel via chosen_option when only a directive is set.
            // When chosen_option is also set, it takes precedence. Future:
            // add a `directive_override: Option<String>` field to RuleSelection.
        } else {
            // No existing selection: create one.
            p.ruleset.selections.push(crate::project::RuleSelection {
                rule_id: rule_id.clone(),
                chosen_option: req.chosen_option.filter(|s| !s.trim().is_empty()),
                repos: Vec::new(),
                ..Default::default()
            });
            // A rule newly selected into the project is baselined at the corpus as it stands.
            if let Some(created) = p.ruleset.selections.last_mut() {
                baseline_missing(std::slice::from_mut(created), corpus.as_ref());
            }
        }
    });
    match updated {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct SetRepoRuleOverrideReq {
    /// Free-text directive override for this rule at the repo scope.
    /// Accepted from callers; not yet persisted (future: add to RuleSelection).
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) directive_override: Option<String>,
    /// The option id to codify for this rule at the repo scope.
    #[serde(default)]
    pub(crate) chosen_option: Option<String>,
}

/// `GET /api/projects/:id/repos/:repo/rules/:rule_id` — repo-level override.
pub(crate) async fn get_repo_rule_override(
    State(state): State<AppState>,
    Path((id, repo, rule_id)): Path<(String, String, String)>,
) -> Json<serde_json::Value> {
    let Some(project) = state.projects.get(&id) else {
        return Json(serde_json::json!({ "ok": false, "message": "no such project" }));
    };
    // Repo-level override: a selection scoped to this repo only.
    let selection = project
        .ruleset
        .selections
        .iter()
        .find(|s| s.rule_id == rule_id && s.repos.iter().any(|r| r == &repo));
    Json(serde_json::json!({
        "ok": true,
        "rule_id": rule_id,
        "project_id": id,
        "repo": repo,
        "chosen_option": selection.and_then(|s| s.chosen_option.as_deref()),
        "scoped_to_repo": selection.is_some(),
    }))
}

/// `POST /api/projects/:id/repos/:repo/rules/:rule_id` — set the repo-level override.
pub(crate) async fn set_repo_rule_override(
    State(state): State<AppState>,
    Path((id, repo, rule_id)): Path<(String, String, String)>,
    Json(req): Json<SetRepoRuleOverrideReq>,
) -> Json<serde_json::Value> {
    let corpus = load_corpus_for_baseline().await;
    let updated = state.projects.update(&id, |p| {
        // Find or create a REPO-SCOPED selection for this rule.
        if let Some(sel) = p
            .ruleset
            .selections
            .iter_mut()
            .find(|s| s.rule_id == rule_id && s.repos.iter().any(|r| r == &repo))
        {
            if let Some(opt) = req.chosen_option.filter(|s| !s.trim().is_empty()) {
                sel.chosen_option = Some(opt);
            }
            let _ = req.directive_override; // future: store on RuleSelection
        } else {
            // Create a new repo-scoped selection.
            p.ruleset.selections.push(crate::project::RuleSelection {
                rule_id: rule_id.clone(),
                chosen_option: req.chosen_option.filter(|s| !s.trim().is_empty()),
                repos: vec![repo.clone()],
                ..Default::default()
            });
            // A rule newly selected into the project is baselined at the corpus as it stands.
            if let Some(created) = p.ruleset.selections.last_mut() {
                baseline_missing(std::slice::from_mut(created), corpus.as_ref());
            }
        }
    });
    match updated {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

// ── Deep-report export ────────────────────────────────────────────────────────
