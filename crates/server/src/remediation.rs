//! Inspection findings become units of work (roadmap 2c, gap 6).
//!
//! One action on the inspection results turns the operator's confirmed findings into a
//! remediation plan of native units of work. The grouping is the pure planner
//! ([`crate::remediation_plan`]); this module is everything around it:
//!
//! - which findings are work at all ([`eligible_findings`]: Confirmed only, TechDebt when the
//!   operator ticks it; false positives, ignored and unresolved findings never become work);
//! - the two routes, `POST /api/projects/:id/remediation/plan` (a preview that creates nothing)
//!   and `POST /api/projects/:id/remediation/create` (one native unit per planned group, each
//!   carrying a [`FindingsSource`] back to every finding it closes);
//! - the rules and findings text the implementer and the reviewer read
//!   ([`grounding_for_unit`], [`extra_grounding_for`]);
//! - the verdict a later delta retest writes onto each unit ([`retest_verdict`],
//!   [`record_retest_evidence`]).
//!
//! The curator's dispositions are client-local (the UI keeps them); like the export, the plan
//! request carries them. Creating units is a data operation and is always allowed: the client
//! lock applies when a unit runs, and the plan response carries its notice so the console can
//! say so up front.
//!
//! # Choices worth knowing
//!
//! - **The lock notice's model.** The notice asks `dev_loop_client_lock_refusal` about the
//!   project's strongest-tier model (`tier_map.strongest`), because that is the model the tiered
//!   dev run picks for the implementer. A project that pins another model for the run sees the
//!   run's own refusal at the first run; the notice is advice up front, never the gate.
//! - **Disposition keys.** Triage arrives keyed by `report_export::finding_key`, built from the
//!   inspection's stored (already redacted) findings, exactly as the UI keys its own triage. The
//!   export looks a finding up by the zero-width-stripped copy, so a key that only matches after
//!   that sanitising is accepted here too ([`eligible_findings`]).
//! - **Merged findings.** A finding a shared-fix group absorbed is a second entry in
//!   `source.findings` whose `merged_into` names the finding that stands for the group; the PR
//!   body derives its "merged into the row above" rows from that.
//! - **Creating is one claim.** [`remediation_create`] holds a per-project claim from the dedup
//!   check to the last unit, so two overlapping creates make one set of units.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use camerata_api_types::fingerprint::fingerprint;
use camerata_api_types::uow::{
    validate_branch_name, validate_repo_name, AuditedCommit, FindingsSource, RetestEvidence,
    SourceFinding, UowSource,
};
use camerata_worktracker::{CanonicalStory, FeatureStatus};
use serde::{Deserialize, Serialize};

use crate::inspections::{InspectionEntry, InspectionKind};
use crate::lifecycle::UowStage;
use crate::onboard::{Finding, ScanReport};
use crate::remediation_plan::{PlanFinding, PlanOptions, PlannedUnit, RuleInfo};
use crate::report_export::{finding_key, DispositionWire};
use crate::scan_ledger::RuleTier;
use crate::uow::{BranchMode, RepoScope, UnitOfWork, UowStore};
use crate::AppState;

/// Body of both routes: the same plan, previewed or created.
#[derive(Deserialize)]
pub(crate) struct RemediationReq {
    /// Defaults to the project's latest inspection.
    #[serde(default)]
    inspection_id: Option<String>,
    /// The curator's current triage, keyed by `finding_key` (same as the export).
    #[serde(default)]
    dispositions: HashMap<String, DispositionWire>,
    #[serde(default)]
    include_tech_debt: bool,
    #[serde(default)]
    options: PlanOptions,
}

/// How many findings were left out of the plan, and why.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Skipped {
    pub false_positive: usize,
    pub ignored: usize,
    pub tech_debt: usize,
    pub unresolved: usize,
    /// Advisory tool previews are not remediation work.
    pub preview: usize,
    /// A finding with no file and no line cannot be fixed from a list.
    pub unlocated: usize,
}

// ── Which findings are work ─────────────────────────────────────────────────────

/// The findings of `findings` that are remediation work, as planner input, and a count of the
/// rest. Confirmed always; TechDebt only when `include_tech_debt`. A finding with no
/// disposition is Unresolved (the export treats it the same way). Nothing here depends on
/// triage being complete.
pub(crate) fn eligible_findings(
    findings: &[Finding],
    dispositions: &HashMap<String, DispositionWire>,
    include_tech_debt: bool,
) -> (Vec<PlanFinding>, Skipped) {
    let mut skipped = Skipped::default();
    let mut out = Vec::new();
    for f in findings {
        if f.preview {
            skipped.preview += 1;
            continue;
        }
        if camerata_rules::severity_bounds::is_unlocated(&f.path, f.line) {
            skipped.unlocated += 1;
            continue;
        }
        let key = finding_key(f);
        let wire = dispositions.get(&key).or_else(|| {
            // The export keys its lookup by the sanitised finding; a key that only matches
            // after sanitising is still the curator's decision about this finding.
            let sanitised = finding_key(&crate::report_export::sanitize_finding_text(f));
            if sanitised == key {
                None
            } else {
                dispositions.get(&sanitised)
            }
        });
        let state = wire.map_or("", |w| w.state.as_str());
        match state {
            "Confirmed" => {}
            "TechDebt" if include_tech_debt => {}
            "TechDebt" => {
                skipped.tech_debt += 1;
                continue;
            }
            "FalsePositive" => {
                skipped.false_positive += 1;
                continue;
            }
            "Ignored" => {
                skipped.ignored += 1;
                continue;
            }
            _ => {
                skipped.unresolved += 1;
                continue;
            }
        }
        out.push(PlanFinding {
            key,
            fingerprint: fingerprint(&f.rule_id, &f.snippet),
            repo: f.repo.clone(),
            path: f.path.clone(),
            line: f.line,
            rule_id: f.rule_id.clone(),
            severity: f.severity.clone(),
            snippet: camerata_secrets::redact_str(&f.snippet).into_owned(),
            also_matches: f.also_matches.clone(),
        });
    }
    (out, skipped)
}

fn source_finding(f: &PlanFinding) -> SourceFinding {
    SourceFinding {
        key: f.key.clone(),
        fingerprint: f.fingerprint.clone(),
        repo: f.repo.clone(),
        path: f.path.clone(),
        line: f.line,
        rule_id: f.rule_id.clone(),
        severity: f.severity.clone(),
        snippet: f.snippet.clone(),
        merged_into: None,
    }
}

// ── Rules ───────────────────────────────────────────────────────────────────────

/// Title and outside standards for each of `ids` from the rule corpus. An id the corpus does
/// not know is left out (callers fall back to the id as its own title); a missing corpus gives
/// an empty map.
pub(crate) async fn corpus_rule_infos(ids: &BTreeSet<String>) -> HashMap<String, RuleInfo> {
    let path = camerata_rules::corpus_path();
    if ids.is_empty() || !path.exists() {
        return HashMap::new();
    }
    let corpus = camerata_rules::load_corpus_lenient(&path).await.0;
    ids.iter()
        .filter_map(|id| {
            corpus.get_by_id(id).map(|rule| {
                (
                    id.clone(),
                    RuleInfo {
                        id: id.clone(),
                        title: rule.title.clone(),
                        sources: rule
                            .sources
                            .iter()
                            .map(|s| (s.title.clone(), s.url.clone()))
                            .collect(),
                    },
                )
            })
        })
        .collect()
}

fn rule_title<'a>(rules: &'a HashMap<String, RuleInfo>, id: &'a str) -> &'a str {
    rules.get(id).map_or(id, |r| r.title.as_str())
}

// ── The plan, resolved against what already exists ──────────────────────────────

/// One planned unit after dedup against the units that already exist.
struct ResolvedUnit {
    planned: PlannedUnit,
    /// Every finding the unit will close, the uncovered ones only: primary findings first,
    /// then those a shared-fix group absorbed.
    findings: Vec<SourceFinding>,
    title: String,
    /// The existing unit that already covers every finding, when there is one.
    covered_by: Option<Covering>,
}

#[derive(Clone)]
struct Covering {
    story_id: String,
    native_number: Option<u64>,
}

impl Covering {
    fn label(&self) -> String {
        match self.native_number {
            Some(n) => format!("uow-{n}"),
            None => self.story_id.clone(),
        }
    }
}

struct Computed {
    entry: InspectionEntry,
    units: Vec<ResolvedUnit>,
    rules: HashMap<String, RuleInfo>,
    notes: Vec<String>,
    skipped: Skipped,
    development_lock: Option<String>,
}

type CoverageKey = (String, String, String, String);

fn coverage_key(f: &SourceFinding) -> CoverageKey {
    (
        f.repo.to_ascii_lowercase(),
        f.path.clone(),
        f.rule_id.clone(),
        f.fingerprint.clone(),
    )
}

fn parse_time(text: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(text).ok()
}

/// A unit that has been through QA (or past it): its work is on a branch for review, so its
/// retests say whether it worked.
fn shipped(stage: UowStage) -> bool {
    matches!(stage, UowStage::AwaitingQa | UowStage::SignedOff)
}

/// The findings existing units already close, keyed line-shift tolerantly (repo, file, rule,
/// fingerprint). A unit covers its findings while its stage is before Awaiting QA, whatever its
/// retests say (a retest from before the unit ran must not make it look unfinished). Once it
/// has shipped, a finding its latest delta retest still reports is no longer covered (the unit
/// did not fix it), and neither is anything when the inspection being planned started after
/// that retest (a regression found later is work). `inspection_started_at` is that inspection's
/// start time (RFC 3339).
///
/// Units of other projects count too when they share a repository with this one, because the
/// unit list is by repository: a finding another project's unit already covers in a shared
/// repository is not planned twice.
fn existing_coverage(
    units: &[UnitOfWork],
    inspection_started_at: &str,
) -> HashMap<CoverageKey, Covering> {
    let started = parse_time(inspection_started_at);
    let mut map = HashMap::new();
    for u in units {
        let Some(src) = u.findings_source() else {
            continue;
        };
        let done = shipped(u.stage);
        let latest = u.retests.last();
        if done {
            let retest_at = latest.and_then(|r| parse_time(&r.recorded));
            if matches!((started, retest_at), (Some(planned), Some(retest)) if planned > retest) {
                continue;
            }
        }
        let still_open: HashSet<&str> = match (done, latest) {
            (true, Some(r)) => r.still_open.iter().map(String::as_str).collect(),
            _ => HashSet::new(),
        };
        for f in &src.findings {
            if still_open.contains(f.fingerprint.as_str()) {
                continue;
            }
            map.entry(coverage_key(f)).or_insert_with(|| Covering {
                story_id: u.story_id.clone(),
                native_number: u.native_number,
            });
        }
    }
    map
}

fn unit_title(
    primary: &str,
    rules: &HashMap<String, RuleInfo>,
    n: usize,
    repo: &str,
    area: Option<&str>,
) -> String {
    let noun = if n == 1 { "finding" } else { "findings" };
    let area = area.map(|a| format!(", {a}")).unwrap_or_default();
    format!(
        "Fix {primary}: {} ({n} {noun} in {repo}{area})",
        rule_title(rules, primary)
    )
}

async fn compute(
    state: &AppState,
    project: &crate::project::Project,
    req: &RemediationReq,
) -> Result<Computed, Response> {
    let entry = match req.inspection_id.as_deref().map(str::trim) {
        Some(id) if !id.is_empty() => state
            .inspections
            .list(&project.id)
            .into_iter()
            .find(|e| e.id == id),
        _ => state.inspections.latest(&project.id),
    };
    let Some(entry) = entry else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "This project has no recorded inspection. Run an audit first."
            })),
        )
            .into_response());
    };

    let (eligible, skipped) = eligible_findings(
        &entry.report.findings,
        &req.dispositions,
        req.include_tech_debt,
    );
    let mut rule_ids = BTreeSet::new();
    for f in &eligible {
        rule_ids.insert(f.rule_id.clone());
        rule_ids.extend(f.also_matches.iter().cloned());
    }
    let rules = corpus_rule_infos(&rule_ids).await;
    let plan = crate::remediation_plan::plan(eligible, &rules, &req.options);

    let existing = state.uow.list_for_project(&project.id, &project.repos);
    let coverage = existing_coverage(&existing, &entry.started_at);
    let mut notes = plan.notes.clone();
    let mut units = Vec::new();
    for planned in plan.units {
        // The unit's findings, then those a shared-fix group absorbed, each pointing at the
        // finding at its location that stands for the group.
        let all: Vec<SourceFinding> = planned
            .findings
            .iter()
            .map(source_finding)
            .chain(planned.merged.iter().map(|m| {
                let mut found = source_finding(m);
                found.merged_into = planned
                    .findings
                    .iter()
                    .find(|p| p.path == m.path && p.line == m.line)
                    .map(|p| p.key.clone());
                found
            }))
            .collect();
        let (covered, open): (Vec<SourceFinding>, Vec<SourceFinding>) = all
            .into_iter()
            .partition(|f| coverage.contains_key(&coverage_key(f)));
        let first_cover = covered
            .first()
            .and_then(|f| coverage.get(&coverage_key(f)))
            .cloned();
        let title_for = |n: usize| {
            unit_title(
                &planned.rule_ids[0],
                &rules,
                n,
                &planned.repo,
                planned.area.as_deref(),
            )
        };
        if open.is_empty() {
            let title = title_for(covered.len());
            units.push(ResolvedUnit {
                findings: covered,
                title,
                covered_by: first_cover,
                planned,
            });
            continue;
        }
        if !covered.is_empty() {
            if let Some(by) = first_cover {
                notes.push(format!(
                    "{}: {} of its findings already belong to {}; this unit keeps the other {}.",
                    planned.rule_ids[0],
                    covered.len(),
                    by.label(),
                    open.len()
                ));
            }
        }
        let title = title_for(open.len());
        units.push(ResolvedUnit {
            findings: open,
            title,
            covered_by: None,
            planned,
        });
    }

    // The lock notice uses the model the dev loop will pick for the implementer (the strongest
    // tier), so the console can say up front that these units cannot run yet.
    let development_lock = crate::dev_implement_run::dev_loop_client_lock_refusal(
        project,
        &project.tier_map.strongest,
    );

    Ok(Computed {
        entry,
        units,
        rules,
        notes,
        skipped,
        development_lock,
    })
}

fn audited_json(entry: &InspectionEntry) -> Vec<serde_json::Value> {
    entry
        .repos
        .iter()
        .map(|r| serde_json::json!({ "repo": r.repo, "sha": r.sha }))
        .collect()
}

fn distinct_files(findings: &[SourceFinding]) -> usize {
    findings
        .iter()
        .map(|f| f.path.as_str())
        .collect::<HashSet<_>>()
        .len()
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": message.into() }))).into_response()
}

fn no_such_project() -> Response {
    error(StatusCode::NOT_FOUND, "no such project")
}

// ── Routes ──────────────────────────────────────────────────────────────────────

/// `POST /api/projects/:id/remediation/plan`: the plan as a preview. Pure; creates nothing.
pub(crate) async fn remediation_plan_preview(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<RemediationReq>,
) -> Response {
    let Some(project) = state.projects.get(&id) else {
        return no_such_project();
    };
    let computed = match compute(&state, &project, &req).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let units: Vec<serde_json::Value> = computed
        .units
        .iter()
        .map(|u| {
            serde_json::json!({
                "title": u.title,
                "repo": u.planned.repo,
                "branch": u.planned.branch,
                "rule_ids": u.planned.rule_ids,
                "area": u.planned.area,
                "severity": u.planned.severity,
                "findings": u.findings,
                "files": distinct_files(&u.findings),
                "depends_on_index": u.planned.depends_on,
                "already_covered_by": u.covered_by.as_ref().map(|c| c.story_id.clone()),
                "already_covered_label": u.covered_by.as_ref().map(Covering::label),
            })
        })
        .collect();
    Json(serde_json::json!({
        "ok": true,
        "inspection_id": computed.entry.id,
        "audited": audited_json(&computed.entry),
        "units": units,
        "skipped": computed.skipped,
        "notes": computed.notes,
        "development_lock": computed.development_lock,
    }))
    .into_response()
}

/// The branches already taken in `repo` by existing units (their recorded branch or the one
/// their Intake scope names), lower-cased.
fn taken_branches(existing: &[UnitOfWork], repo: &str) -> HashSet<String> {
    let mut taken = HashSet::new();
    for u in existing {
        if let Some(b) = u.branch.as_deref() {
            if u.repo().is_some_and(|r| r.eq_ignore_ascii_case(repo)) {
                taken.insert(b.to_ascii_lowercase());
            }
        }
        for scope in &u.intake.repos {
            if !scope.repo.eq_ignore_ascii_case(repo) {
                continue;
            }
            if let BranchMode::NewFromBase { new_name, .. } = &scope.branch {
                if !new_name.is_empty() {
                    taken.insert(new_name.to_ascii_lowercase());
                }
            }
        }
    }
    taken
}

/// The most numbered names tried for one branch before giving up.
const BRANCH_TRIES: usize = 1000;

/// `branch`, or `branch-2`, `branch-3` and so on: the first that is valid and not taken. An
/// error after [`BRANCH_TRIES`] names, or when the name can never be valid.
fn free_branch(branch: &str, taken: &HashSet<String>) -> Result<String, String> {
    let mut candidate = branch.to_string();
    for n in 2..=BRANCH_TRIES + 1 {
        if !taken.contains(&candidate.to_ascii_lowercase())
            && validate_branch_name(&candidate).is_ok()
        {
            return Ok(candidate);
        }
        candidate = format!("{branch}-{n}");
    }
    Err(format!(
        "No free branch name was found for `{branch}` after {BRANCH_TRIES} tries. Remove the \
         old remediation branches or units and plan again."
    ))
}

fn table_cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\r', '\n'], " ")
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// The Markdown story the implementer and the reviewer read: the rules to satisfy and the
/// findings to close. Snippets are not in it (they are in the source, redacted); the agent
/// reads the real file.
fn story_description(
    inspection_id: &str,
    repo: &str,
    sha: Option<&str>,
    source: &FindingsSource,
    rules: &HashMap<String, RuleInfo>,
) -> String {
    let mut out = String::from("## Rules to satisfy\n");
    for id in &source.rule_ids {
        let standard = match rules.get(id).and_then(|r| r.sources.first()) {
            Some((title, url)) => format!("{} ({})", table_cell(title), table_cell(url)),
            None => "no outside source listed".to_string(),
        };
        out.push_str(&format!(
            "- {id}: {}. Standard: {standard}.\n",
            table_cell(rule_title(rules, id)).trim_end_matches('.')
        ));
    }
    let at = match sha.filter(|s| !s.is_empty()) {
        Some(sha) => format!("{repo} at {sha}"),
        None => format!("{repo}, commit not recorded"),
    };
    out.push_str(&format!(
        "\n## Findings to close (inspection {inspection_id}, {at})\n\n"
    ));
    out.push_str("| # | File | Line | Rule | Severity | Finding |\n");
    out.push_str("| --- | --- | --- | --- | --- | --- |\n");
    for (i, f) in source.findings.iter().enumerate() {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            i + 1,
            table_cell(&f.path),
            f.line,
            table_cell(&f.rule_id),
            table_cell(&f.severity),
            table_cell(&f.fingerprint)
        ));
    }
    out.push_str(
        "\nClose every row. A finding counts as closed when the next delta retest no longer \
         reports it.",
    );
    out
}

/// The per-project claim [`remediation_create`] holds (a pseudo repository name in the
/// source-operation claims, which no real repository can equal).
const CREATE_CLAIM: &str = "remediation:create";

/// `POST /api/projects/:id/remediation/create`: runs the same plan, then creates one native
/// unit per planned group that is not already covered.
pub(crate) async fn remediation_create(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<RemediationReq>,
) -> Response {
    let Some(project) = state.projects.get(&id) else {
        return no_such_project();
    };
    // One create at a time per project, from the dedup check to the last unit: two overlapping
    // creates would both see "nothing covered yet" and make two sets.
    let Some(_claim) = state.source_ops.try_begin(&project.id, CREATE_CLAIM) else {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "remediation_in_progress",
                "message": "Units of work are already being created for this project. Wait \
                            for that to finish, then plan again."
            })),
        )
            .into_response();
    };
    if state.jobs.has_running_for_project(&project.id) || state.source_ops.scan_running(&project.id)
    {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "scan_in_progress",
                "message": "A scan is running for this project. Wait for it to finish, then \
                            plan again: the triage you sent may be against a report that is \
                            being replaced."
            })),
        )
            .into_response();
    }
    let computed = match compute(&state, &project, &req).await {
        Ok(c) => c,
        Err(r) => return r,
    };

    // Validate every unit before creating any, so a bad repository never leaves half a plan.
    for u in computed.units.iter().filter(|u| u.covered_by.is_none()) {
        let repo = &u.planned.repo;
        if let Err(reason) = validate_repo_name(repo) {
            return error(StatusCode::BAD_REQUEST, format!("{}: {reason}", u.title));
        }
        if !crate::run_target::repo_in_project(repo, Some(project.repos.as_slice())) {
            return error(
                StatusCode::BAD_REQUEST,
                format!(
                    "`{repo}` is not one of this project's repositories ({}).",
                    project.repos.join(", ")
                ),
            );
        }
    }

    let existing = state.uow.list_for_project(&project.id, &project.repos);
    let mut taken: HashMap<String, HashSet<String>> = HashMap::new();
    // Plan index -> the story id that stands for it (the new unit, or the existing one that
    // already covers it), so a dependency can name it.
    let mut story_of: Vec<Option<String>> = vec![None; computed.units.len()];
    let mut created = Vec::new();
    let mut skipped_existing = Vec::new();
    let scoped = |index: usize, title: &str, what: &str| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "could not create unit {} of the plan ({title}): {what}",
                index + 1
            ),
        )
    };

    for (index, u) in computed.units.iter().enumerate() {
        if let Some(by) = &u.covered_by {
            story_of[index] = Some(by.story_id.clone());
            skipped_existing.push(serde_json::json!({
                "title": u.title,
                "covered_by": by.story_id,
                "label": by.label(),
            }));
            continue;
        }
        let repo = u.planned.repo.clone();
        let taken_here = taken
            .entry(repo.to_ascii_lowercase())
            .or_insert_with(|| taken_branches(&existing, &repo));
        let branch = match free_branch(&u.planned.branch, taken_here) {
            Ok(b) => b,
            Err(reason) => return error(StatusCode::CONFLICT, reason),
        };
        taken_here.insert(branch.to_ascii_lowercase());

        let uow = match state
            .uow
            .create_native(Some(repo.clone()), Some(project.id.clone()))
        {
            Ok(u) => u,
            Err(reason) => return error(StatusCode::BAD_REQUEST, reason),
        };
        let sha = computed.entry.sha_for(&repo).map(str::to_string);
        let source = FindingsSource {
            inspection_id: computed.entry.id.clone(),
            audited: vec![AuditedCommit {
                repo: repo.clone(),
                sha: sha.clone(),
            }],
            rule_ids: u.planned.rule_ids.clone(),
            rules_fingerprint: computed.entry.rules_fingerprint.clone(),
            area: u.planned.area.clone(),
            findings: u.findings.clone(),
            depends_on: u
                .planned
                .depends_on
                .iter()
                .filter_map(|&d| story_of.get(d).cloned().flatten())
                .collect(),
        };
        let description = story_description(
            &computed.entry.id,
            &repo,
            sha.as_deref(),
            &source,
            &computed.rules,
        );
        state
            .uow
            .set_source(&uow.story_id, Some(UowSource::Findings(source.clone())));
        if let Err(reason) = state.uow.set_intake_repos(
            &uow.story_id,
            vec![RepoScope {
                repo: repo.clone(),
                branch: BranchMode::NewFromBase {
                    base: String::new(),
                    new_name: branch.clone(),
                },
            }],
        ) {
            state.uow.delete(&uow.story_id);
            return scoped(index, &u.title, &reason);
        }
        let at = match sha.as_deref().filter(|s| !s.is_empty()) {
            Some(sha) => format!("commit {}", short_sha(sha)),
            None => "a commit that was not recorded".to_string(),
        };
        state.uow.set_intake_context(
            &uow.story_id,
            &format!(
                "Created from inspection {} at {at}. Fix every listed finding and nothing else. \
                 Do not change behaviour outside the listed files unless the fix requires it, \
                 and say so in the run report when it does.",
                computed.entry.id
            ),
        );
        let story = CanonicalStory {
            id: uow.story_id.clone(),
            external_ref: None,
            title: u.title.clone(),
            description,
            status: FeatureStatus::Intake,
            created_by: "camerata".to_string(),
            targets: vec![],
        };
        if let Err(e) = state.stories.upsert(story).await {
            // No spec behind it: do not leave a unit the run cannot read.
            state.uow.delete(&uow.story_id);
            return scoped(
                index,
                &u.title,
                &format!("the story could not be recorded: {e}"),
            );
        }
        state.uow.append_history(
            &uow.story_id,
            "created",
            &format!(
                "Created from inspection {}: {} finding(s) under {}.",
                computed.entry.id,
                u.findings.len(),
                u.planned.rule_ids.join(", ")
            ),
        );
        story_of[index] = Some(uow.story_id.clone());
        created.push(serde_json::json!({
            "uow_id": uow.story_id,
            "native_number": uow.native_number,
            "title": u.title,
            "repo": repo,
            "branch": branch,
        }));
    }

    Json(serde_json::json!({
        "ok": true,
        "inspection_id": computed.entry.id,
        "created": created,
        "skipped_existing": skipped_existing,
        "notes": computed.notes,
        "development_lock": computed.development_lock,
    }))
    .into_response()
}

// ── The rules and findings the implementer and the reviewer read ────────────────

/// The block that tells the implementer (and the reviewer, who reads the same prose) which
/// findings this change must close and under which rules. `None` for a unit that did not come
/// from findings.
pub(crate) fn grounding_for_unit(
    uow: &UnitOfWork,
    rules: &HashMap<String, RuleInfo>,
) -> Option<String> {
    let src = uow.findings_source()?;
    let mut out = String::from("## Findings this change must close\n");
    let mut repos: Vec<&str> = Vec::new();
    for f in &src.findings {
        if !repos.contains(&f.repo.as_str()) {
            repos.push(f.repo.as_str());
        }
    }
    if repos.is_empty() {
        out.push_str(&format!("Inspection {}.\n", src.inspection_id));
    }
    for repo in &repos {
        let sha = src
            .audited
            .iter()
            .find(|a| a.repo.eq_ignore_ascii_case(repo))
            .and_then(|a| a.sha.as_deref())
            .filter(|s| !s.is_empty());
        match sha {
            Some(sha) => out.push_str(&format!(
                "Inspection {}; {repo} at {sha}.\n",
                src.inspection_id
            )),
            None => out.push_str(&format!(
                "Inspection {}; {repo} (commit not recorded).\n",
                src.inspection_id
            )),
        }
    }
    for f in &src.findings {
        out.push_str(&format!(
            "- {}:{} {} ({}) finding {}\n",
            f.path, f.line, f.rule_id, f.severity, f.fingerprint
        ));
    }
    out.push_str("Rules:\n");
    for id in &src.rule_ids {
        out.push_str(&format!("{id}: {}\n", rule_title(rules, id)));
    }
    out.push_str(
        "Change only what these findings need. Report any finding you could not close and why.",
    );
    Some(out)
}

/// The `extra_grounding` for a run of `story_id`: `existing` (a resume directive, say) followed
/// by the findings block when the unit came from findings. `existing` alone for every other
/// unit, so non-findings runs are unchanged.
pub(crate) async fn extra_grounding_for(
    uow: &UowStore,
    story_id: &str,
    existing: Option<String>,
) -> Option<String> {
    let Some(unit) = uow.get(story_id).filter(|u| u.findings_source().is_some()) else {
        return existing;
    };
    let ids: BTreeSet<String> = unit.source_rule_ids().into_iter().collect();
    let rules = corpus_rule_infos(&ids).await;
    let Some(block) = grounding_for_unit(&unit, &rules) else {
        return existing;
    };
    Some(match existing {
        Some(prior) if !prior.trim().is_empty() => format!("{prior}\n\n{block}"),
        _ => block,
    })
}

// ── Delta retest evidence ───────────────────────────────────────────────────────

/// The retest's verdict on each of a unit's findings, as lists of fingerprints.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Verdict {
    pub resolved: Vec<String>,
    pub still_open: Vec<String>,
    pub not_rechecked: Vec<String>,
    /// Findings reported at another file than the unit recorded (also in `still_open`):
    /// fingerprint to the new path.
    pub moved: BTreeMap<String, String>,
    /// The retest's AI review did not finish.
    pub ai_incomplete: bool,
}

/// Whether the retest's AI review did not finish: an error, a block or a failed pass.
fn ai_review_incomplete(report: &ScanReport) -> bool {
    report.ai_error.is_some()
        || report.ai_blocked_reason.is_some()
        || !report.failed_passes.is_empty()
}

/// Whether `rule_id` is judged by the AI review (the semantic tier), so a clean report from a
/// retest whose AI review did not finish says nothing about it.
fn is_semantic(report: &ScanReport, rule_id: &str) -> bool {
    rule_id.starts_with("AI-")
        || report
            .ledger
            .rule(rule_id)
            .is_some_and(|e| e.tier == RuleTier::Semantic)
}

/// The retest's verdict on each of `source`'s findings.
///
/// A finding is present when the retest report has one in the same repository and file for the
/// same rule with the same fingerprint, whatever line it moved to; a row that demoted the rule
/// into `also_matches` at that file counts too. One that is absent from its file but reported
/// under the same rule and fingerprint in another file is still present, moved: a rename is not a
/// fix. A finding in a repository the retest did not cover is neither resolved nor open. When
/// the retest's AI review did not finish, an absent semantic-tier finding is not rechecked
/// rather than resolved.
pub(crate) fn retest_verdict(
    source: &FindingsSource,
    report: &ScanReport,
    covered_repos: &[String],
) -> Verdict {
    let mut verdict = Verdict {
        ai_incomplete: ai_review_incomplete(report),
        ..Verdict::default()
    };
    let mut by_file: HashMap<(String, &str), Vec<&Finding>> = HashMap::new();
    for f in &report.findings {
        by_file
            .entry((f.repo.to_ascii_lowercase(), f.path.as_str()))
            .or_default()
            .push(f);
    }
    let matches_rule = |f: &Finding, sf: &SourceFinding| {
        (f.rule_id == sf.rule_id || f.also_matches.contains(&sf.rule_id))
            && fingerprint(&sf.rule_id, &f.snippet) == sf.fingerprint
    };
    // A report row at a path the unit itself records for the same finding is that finding's, not
    // somebody else's that moved.
    let recorded_here = |sf: &SourceFinding, path: &str| {
        source.findings.iter().any(|o| {
            o.repo.eq_ignore_ascii_case(&sf.repo)
                && o.rule_id == sf.rule_id
                && o.fingerprint == sf.fingerprint
                && o.path == path
        })
    };
    for sf in &source.findings {
        if !covered_repos
            .iter()
            .any(|r| r.eq_ignore_ascii_case(&sf.repo))
        {
            verdict.not_rechecked.push(sf.fingerprint.clone());
            continue;
        }
        let present = by_file
            .get(&(sf.repo.to_ascii_lowercase(), sf.path.as_str()))
            .is_some_and(|rows| rows.iter().any(|f| matches_rule(f, sf)));
        if present {
            verdict.still_open.push(sf.fingerprint.clone());
            continue;
        }
        let moved_to = report
            .findings
            .iter()
            .filter(|f| f.repo.eq_ignore_ascii_case(&sf.repo) && f.path != sf.path)
            .filter(|f| matches_rule(f, sf) && !recorded_here(sf, &f.path))
            .map(|f| f.path.as_str())
            .min();
        if let Some(path) = moved_to {
            verdict.still_open.push(sf.fingerprint.clone());
            verdict
                .moved
                .insert(sf.fingerprint.clone(), path.to_string());
        } else if verdict.ai_incomplete && is_semantic(report, &sf.rule_id) {
            verdict.not_rechecked.push(sf.fingerprint.clone());
        } else {
            verdict.resolved.push(sf.fingerprint.clone());
        }
    }
    verdict
}

/// Write a delta retest's verdict onto every findings unit it answers for: a unit of
/// `project_id` whose source is the retest's base inspection or that audited a repository the
/// retest covered, and at least one of whose repositories the retest did cover. A no-op for an
/// entry that is not a delta retest. Whether the rule selection changed is judged per unit, from
/// the rule fingerprint the unit recorded when it was made; `base_rules_fingerprint` (the base
/// inspection's, "" when unknown) is only the fallback for a unit that recorded none.
pub(crate) fn record_retest_evidence(
    uow: &UowStore,
    project_id: &str,
    entry: &InspectionEntry,
    base_rules_fingerprint: &str,
) {
    let InspectionKind::DeltaRetest {
        base_inspection_id,
        from_sha,
        to_sha,
    } = &entry.kind
    else {
        return;
    };
    let covered: Vec<String> = entry.repos.iter().map(|r| r.repo.clone()).collect();
    for unit in uow.list() {
        if unit.project_id.as_deref() != Some(project_id) {
            continue;
        }
        let Some(src) = unit.findings_source() else {
            continue;
        };
        let answers = src.inspection_id == *base_inspection_id
            || src
                .audited
                .iter()
                .any(|a| covered.iter().any(|c| c.eq_ignore_ascii_case(&a.repo)));
        let touches = src
            .findings
            .iter()
            .any(|f| covered.iter().any(|c| c.eq_ignore_ascii_case(&f.repo)));
        if !answers || !touches {
            continue;
        }
        let compared = if src.rules_fingerprint.is_empty() {
            base_rules_fingerprint
        } else {
            src.rules_fingerprint.as_str()
        };
        let rules_changed = !compared.is_empty() && compared != entry.rules_fingerprint;
        let verdict = retest_verdict(src, &entry.report, &covered);
        let total = src.findings.len();
        let mut text = format!(
            "Delta retest {} at {}: {} of {total} findings gone, {} still present",
            entry.id,
            short_sha(to_sha),
            verdict.resolved.len(),
            verdict.still_open.len()
        );
        if !verdict.moved.is_empty() {
            text.push_str(&format!(" ({} moved to another file)", verdict.moved.len()));
        }
        if !verdict.not_rechecked.is_empty() {
            text.push_str(&format!(", {} not rechecked", verdict.not_rechecked.len()));
        }
        if verdict.ai_incomplete {
            text.push_str(
                "; the AI review did not finish, so semantic findings could not be rechecked",
            );
        }
        if rules_changed {
            text.push_str("; the rule selection changed since the inspection");
        }
        text.push('.');
        let evidence = RetestEvidence {
            inspection_id: entry.id.clone(),
            from_sha: from_sha.clone(),
            to_sha: to_sha.clone(),
            recorded: chrono::Utc::now().to_rfc3339(),
            resolved: verdict.resolved,
            still_open: verdict.still_open,
            not_rechecked: verdict.not_rechecked,
            rules_changed,
            ai_incomplete: verdict.ai_incomplete,
            moved: verdict.moved,
        };
        uow.push_retest(&unit.story_id, evidence, &text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(rule: &str, path: &str, line: usize, snippet: &str) -> Finding {
        Finding {
            repo: "acme/api".to_string(),
            path: path.to_string(),
            line,
            rule_id: rule.to_string(),
            severity: "high".to_string(),
            snippet: snippet.to_string(),
            ..Finding::default()
        }
    }

    fn wire(state: &str) -> DispositionWire {
        DispositionWire {
            state: state.to_string(),
            ..DispositionWire::default()
        }
    }

    fn triage(pairs: &[(&Finding, &str)]) -> HashMap<String, DispositionWire> {
        pairs
            .iter()
            .map(|(f, s)| (finding_key(f), wire(s)))
            .collect()
    }

    #[test]
    fn confirmed_findings_become_plan_input_and_false_positive_ignored_unresolved_never_do() {
        let confirmed = finding("A-1", "src/a.rs", 1, "one");
        let fp = finding("A-1", "src/b.rs", 2, "two");
        let ignored = finding("A-1", "src/c.rs", 3, "three");
        let debt = finding("A-1", "src/d.rs", 4, "four");
        let untouched = finding("A-1", "src/e.rs", 5, "five");
        let all = vec![
            confirmed.clone(),
            fp.clone(),
            ignored.clone(),
            debt.clone(),
            untouched.clone(),
        ];
        let dispositions = triage(&[
            (&confirmed, "Confirmed"),
            (&fp, "FalsePositive"),
            (&ignored, "Ignored"),
            (&debt, "TechDebt"),
        ]);

        let (eligible, skipped) = eligible_findings(&all, &dispositions, false);
        assert_eq!(
            eligible.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            vec!["src/a.rs"],
            "only the Confirmed finding is work"
        );
        assert_eq!(
            skipped,
            Skipped {
                false_positive: 1,
                ignored: 1,
                tech_debt: 1,
                unresolved: 1,
                preview: 0,
                unlocated: 0
            }
        );

        let (with_debt, skipped) = eligible_findings(&all, &dispositions, true);
        let paths: Vec<&str> = with_debt.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["src/a.rs", "src/d.rs"],
            "TechDebt joins when ticked"
        );
        assert_eq!(skipped.tech_debt, 0);
        assert_eq!(
            (skipped.false_positive, skipped.ignored, skipped.unresolved),
            (1, 1, 1)
        );
    }

    #[test]
    fn a_planned_finding_carries_both_identities_and_a_redacted_snippet() {
        // Assembled at run time so no secret-shaped literal sits in the source.
        let token = format!("ghp_{}", "aB3dE6gH9jK2mN5pQ8sT1vW4yZ7cF0hJ3kLm");
        let f = finding("A-1", "src/a.rs", 7, &format!("let k = \"{token}\";"));
        let dispositions = triage(&[(&f, "Confirmed")]);
        let (eligible, _) = eligible_findings(std::slice::from_ref(&f), &dispositions, false);
        let got = &eligible[0];
        assert_eq!(got.key, finding_key(&f));
        assert_eq!(got.fingerprint, fingerprint("A-1", &f.snippet));
        assert!(
            !got.snippet.contains(&token),
            "the snippet is redacted: {}",
            got.snippet
        );
    }

    #[test]
    fn preview_and_unlocated_findings_are_skipped_and_counted() {
        let mut preview = finding("A-1", "src/a.rs", 1, "x");
        preview.preview = true;
        let unlocated = finding("A-1", "", 0, "y");
        let ok = finding("A-1", "src/ok.rs", 2, "z");
        let dispositions = triage(&[
            (&preview, "Confirmed"),
            (&unlocated, "Confirmed"),
            (&ok, "Confirmed"),
        ]);
        let (eligible, skipped) =
            eligible_findings(&[preview, unlocated, ok], &dispositions, false);
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].path, "src/ok.rs");
        assert_eq!((skipped.preview, skipped.unlocated), (1, 1));
        assert_eq!(
            skipped.unresolved, 0,
            "a skipped finding is not also unresolved"
        );
    }

    fn source_with(findings: Vec<SourceFinding>, rules: &[&str]) -> FindingsSource {
        FindingsSource {
            inspection_id: "insp-1".to_string(),
            audited: vec![AuditedCommit {
                repo: "acme/api".to_string(),
                sha: Some("0123456789abcdef".to_string()),
            }],
            rule_ids: rules.iter().map(|r| r.to_string()).collect(),
            findings,
            ..FindingsSource::default()
        }
    }

    fn sf(rule: &str, path: &str, line: usize, snippet: &str) -> SourceFinding {
        SourceFinding {
            key: format!("k-{path}-{line}"),
            fingerprint: fingerprint(rule, snippet),
            repo: "acme/api".to_string(),
            path: path.to_string(),
            line,
            rule_id: rule.to_string(),
            severity: "high".to_string(),
            snippet: snippet.to_string(),
            merged_into: None,
        }
    }

    fn unit_with(source: FindingsSource) -> UnitOfWork {
        UnitOfWork {
            story_id: "uow-x".to_string(),
            source: Some(UowSource::Findings(source)),
            ..UnitOfWork::default()
        }
    }

    #[test]
    fn grounding_for_unit_lists_every_finding_and_rule_and_names_the_inspection_commit() {
        let src = source_with(
            vec![
                sf("SEC-A-1", "src/db.rs", 42, "q"),
                sf("SEC-B-2", "src/web.rs", 7, "r"),
            ],
            &["SEC-A-1", "SEC-B-2"],
        );
        let rules: HashMap<String, RuleInfo> = [(
            "SEC-A-1".to_string(),
            RuleInfo {
                id: "SEC-A-1".to_string(),
                title: "No raw SQL".to_string(),
                sources: vec![],
            },
        )]
        .into();
        let text = grounding_for_unit(&unit_with(src.clone()), &rules).expect("a findings unit");
        assert!(
            text.starts_with("## Findings this change must close\n"),
            "{text}"
        );
        assert!(
            text.contains("Inspection insp-1; acme/api at 0123456789abcdef."),
            "{text}"
        );
        for f in &src.findings {
            assert!(
                text.contains(&format!(
                    "- {}:{} {} (high) finding {}",
                    f.path, f.line, f.rule_id, f.fingerprint
                )),
                "{text}"
            );
        }
        assert!(text.contains("SEC-A-1: No raw SQL"), "{text}");
        assert!(
            text.contains("SEC-B-2: SEC-B-2"),
            "unknown rule falls back to its id: {text}"
        );
        assert!(
            text.ends_with("Report any finding you could not close and why."),
            "{text}"
        );
        assert!(!text.contains('\u{2014}'), "no em dash");
        // A unit from anywhere else has nothing to add.
        assert!(grounding_for_unit(&UnitOfWork::default(), &rules).is_none());
    }

    fn report_with(findings: Vec<Finding>) -> ScanReport {
        let mut r = ScanReport::gated(&["acme/api".to_string()]);
        r.gated = false;
        r.findings = findings;
        r
    }

    #[test]
    fn retest_verdict_marks_absent_findings_resolved_present_ones_open_and_uncovered_repos_not_rechecked(
    ) {
        let gone = sf("A-1", "src/gone.rs", 10, "gone snippet");
        let moved = sf("A-1", "src/moved.rs", 20, "moved snippet");
        let mut other = sf("A-1", "src/other.rs", 5, "other snippet");
        other.repo = "acme/web".to_string();
        let src = source_with(vec![gone.clone(), moved.clone(), other.clone()], &["A-1"]);
        // The moved finding sits on a different line now; the fingerprint still matches.
        let report = report_with(vec![finding("A-1", "src/moved.rs", 31, "moved snippet")]);
        let v = retest_verdict(&src, &report, &["acme/api".to_string()]);
        assert_eq!(v.resolved, vec![gone.fingerprint]);
        assert_eq!(v.still_open, vec![moved.fingerprint]);
        assert_eq!(v.not_rechecked, vec![other.fingerprint]);
        assert!(v.moved.is_empty(), "same file, new line: not a move");
    }

    #[test]
    fn retest_verdict_does_not_confuse_another_rule_with_the_same_snippet() {
        let a = sf("A-1", "src/a.rs", 1, "same");
        let src = source_with(vec![a.clone()], &["A-1"]);
        // Another rule on the same line, and the same line under another rule elsewhere: neither
        // is this finding.
        let report = report_with(vec![
            finding("B-2", "src/a.rs", 1, "same"),
            finding("B-2", "src/elsewhere.rs", 1, "same"),
        ]);
        let v = retest_verdict(&src, &report, &["acme/api".to_string()]);
        assert_eq!((v.resolved.len(), v.still_open.len()), (1, 0));
    }

    #[test]
    fn retest_verdict_sees_a_demoted_rule_in_also_matches_as_present() {
        let secondary = sf("B-2", "src/a.rs", 4, "shared line");
        let src = source_with(vec![secondary.clone()], &["B-2"]);
        // The retest merged the location into one row under A-1 and demoted B-2.
        let mut merged = finding("A-1", "src/a.rs", 4, "shared line");
        merged.also_matches = vec!["B-2".to_string()];
        let report = report_with(vec![merged]);
        let v = retest_verdict(&src, &report, &["acme/api".to_string()]);
        assert!(v.resolved.is_empty(), "{:?}", v.resolved);
        assert_eq!(v.still_open, vec![secondary.fingerprint]);
    }

    fn delta_entry(report: ScanReport, repos: &[&str], rules_fp: &str) -> InspectionEntry {
        let mut report = report;
        report.provenance.audited_refs = repos
            .iter()
            .map(|r| crate::onboard::AuditedRef {
                repo: r.to_string(),
                sha: Some("fedcba9876543210".to_string()),
                ..crate::onboard::AuditedRef::default()
            })
            .collect();
        report.provenance.rules_fingerprint = rules_fp.to_string();
        let mut entry = InspectionEntry::from_report(
            "proj-1",
            report,
            InspectionKind::DeltaRetest {
                base_inspection_id: "insp-1".to_string(),
                from_sha: "0123456789abcdef".to_string(),
                to_sha: "fedcba9876543210".to_string(),
            },
        );
        entry.id = "insp-d".to_string();
        entry
    }

    #[test]
    fn a_delta_retest_records_evidence_on_the_unit() {
        let store = UowStore::new();
        let unit = store
            .create_native(Some("acme/api".to_string()), Some("proj-1".to_string()))
            .unwrap();
        let gone = sf("A-1", "src/gone.rs", 10, "gone snippet");
        let kept = sf("A-1", "src/kept.rs", 20, "kept snippet");
        store.set_source(
            &unit.story_id,
            Some(UowSource::Findings(source_with(
                vec![gone.clone(), kept.clone()],
                &["A-1"],
            ))),
        );
        // A unit of another project is never touched.
        let foreign = store
            .create_native(Some("acme/api".to_string()), Some("other".to_string()))
            .unwrap();
        store.set_source(
            &foreign.story_id,
            Some(UowSource::Findings(source_with(
                vec![gone.clone()],
                &["A-1"],
            ))),
        );

        let entry = delta_entry(
            report_with(vec![finding("A-1", "src/kept.rs", 22, "kept snippet")]),
            &["acme/api"],
            "rules-new",
        );
        record_retest_evidence(&store, "proj-1", &entry, "rules-old");

        let got = store.get(&unit.story_id).unwrap();
        assert_eq!(got.retests.len(), 1);
        let ev = &got.retests[0];
        assert_eq!(ev.inspection_id, "insp-d");
        assert_eq!(ev.resolved, vec![gone.fingerprint]);
        assert_eq!(ev.still_open, vec![kept.fingerprint]);
        assert!(ev.not_rechecked.is_empty());
        assert!(ev.rules_changed);
        assert_eq!(
            (ev.from_sha.as_str(), ev.to_sha.as_str()),
            ("0123456789abcdef", "fedcba9876543210")
        );
        let line = &got.history.last().unwrap();
        assert_eq!(line.kind, "retest");
        assert_eq!(
            line.text,
            "Delta retest insp-d at fedcba9: 1 of 2 findings gone, 1 still present; the rule selection changed since the inspection."
        );
        assert!(store.get(&foreign.story_id).unwrap().retests.is_empty());

        // The same rules: no change flag, and a full-inspection entry records nothing.
        let same = delta_entry(report_with(vec![]), &["acme/api"], "rules-old");
        record_retest_evidence(&store, "proj-1", &same, "rules-old");
        let got = store.get(&unit.story_id).unwrap();
        assert_eq!(got.retests.len(), 2);
        assert!(!got.retests[1].rules_changed);
        assert!(got
            .history
            .last()
            .unwrap()
            .text
            .contains("2 of 2 findings gone, 0 still present."));
        let mut full = same;
        full.kind = InspectionKind::Full;
        record_retest_evidence(&store, "proj-1", &full, "rules-old");
        assert_eq!(store.get(&unit.story_id).unwrap().retests.len(), 2);
    }

    #[test]
    fn a_retest_that_covered_none_of_the_units_repositories_records_nothing() {
        let store = UowStore::new();
        let unit = store
            .create_native(Some("acme/api".to_string()), Some("proj-1".to_string()))
            .unwrap();
        store.set_source(
            &unit.story_id,
            Some(UowSource::Findings(source_with(
                vec![sf("A-1", "src/a.rs", 1, "x")],
                &["A-1"],
            ))),
        );
        let entry = delta_entry(report_with(vec![]), &["acme/web"], "r");
        record_retest_evidence(&store, "proj-1", &entry, "r");
        assert!(store.get(&unit.story_id).unwrap().retests.is_empty());
    }

    const RETEST_AT: &str = "2026-10-28T10:00:00+00:00";
    const BEFORE_RETEST: &str = "2026-10-28T09:00:00+00:00";
    const AFTER_RETEST: &str = "2026-10-28T11:00:00+00:00";

    /// A unit at `stage` whose latest retest (at `RETEST_AT`) still reports `b` and cleared `a`.
    fn unit_after_a_retest(stage: UowStage) -> (UnitOfWork, SourceFinding, SourceFinding) {
        let a = sf("A-1", "src/a.rs", 1, "x");
        let b = sf("A-1", "src/b.rs", 2, "y");
        let mut unit = unit_with(source_with(vec![a.clone(), b.clone()], &["A-1"]));
        unit.stage = stage;
        unit.retests.push(RetestEvidence {
            recorded: RETEST_AT.to_string(),
            still_open: vec![b.fingerprint.clone()],
            resolved: vec![a.fingerprint.clone()],
            ..RetestEvidence::default()
        });
        (unit, a, b)
    }

    #[test]
    fn an_unshipped_unit_covers_its_findings_whatever_its_retests_say() {
        // A retest from before the unit ran lists everything as still open; the unit has not
        // shipped, so it still owns the work, even for an inspection planned after that retest.
        for stage in [
            UowStage::Intake,
            UowStage::Investigating,
            UowStage::DecisionsApproved,
            UowStage::Development,
        ] {
            let (unit, a, b) = unit_after_a_retest(stage);
            let cover = existing_coverage(&[unit], AFTER_RETEST);
            assert!(cover.contains_key(&coverage_key(&a)), "{stage:?}");
            assert!(cover.contains_key(&coverage_key(&b)), "{stage:?}");
        }
    }

    #[test]
    fn a_shipped_unit_stops_covering_findings_its_latest_retest_still_reports() {
        for stage in [UowStage::AwaitingQa, UowStage::SignedOff] {
            let (unit, a, b) = unit_after_a_retest(stage);
            let cover = existing_coverage(&[unit], BEFORE_RETEST);
            assert!(cover.contains_key(&coverage_key(&a)));
            assert!(
                !cover.contains_key(&coverage_key(&b)),
                "a unit that did not close a finding must not block a new one"
            );
        }
    }

    #[test]
    fn a_shipped_unit_stops_covering_when_the_inspection_started_after_its_latest_retest() {
        // The finding was cleared by the retest, but a newer inspection reports it again: a
        // regression, which is work.
        let (unit, a, b) = unit_after_a_retest(UowStage::AwaitingQa);
        let cover = existing_coverage(&[unit], AFTER_RETEST);
        assert!(cover.is_empty(), "{:?}", cover.keys().collect::<Vec<_>>());
        let _ = (a, b);
    }

    #[test]
    fn a_shipped_unit_with_no_retest_keeps_covering_its_findings() {
        let a = sf("A-1", "src/a.rs", 1, "x");
        let mut unit = unit_with(source_with(vec![a.clone()], &["A-1"]));
        unit.stage = UowStage::AwaitingQa;
        let cover = existing_coverage(&[unit], AFTER_RETEST);
        assert!(cover.contains_key(&coverage_key(&a)));
    }

    #[test]
    fn a_retest_with_an_unfinished_ai_review_never_marks_a_semantic_finding_resolved() {
        let semantic = sf("AI-NO-DEAD-CODE-1", "src/a.rs", 1, "x");
        let ledger_semantic = sf("ARCH-9", "src/b.rs", 2, "y");
        let deterministic = sf("SEC-A-1", "src/c.rs", 3, "z");
        let present = sf("AI-NO-DEAD-CODE-1", "src/d.rs", 4, "w");
        let src = source_with(
            vec![
                semantic.clone(),
                ledger_semantic.clone(),
                deterministic.clone(),
                present.clone(),
            ],
            &["AI-NO-DEAD-CODE-1"],
        );
        let covered = ["acme/api".to_string()];
        let mut report = report_with(vec![finding("AI-NO-DEAD-CODE-1", "src/d.rs", 4, "w")]);
        report
            .ledger
            .record_rule("", "ARCH-9", RuleTier::Semantic, true, None, 1, 0);

        // A complete AI review: absent means resolved.
        let ok = retest_verdict(&src, &report, &covered);
        assert!(!ok.ai_incomplete);
        assert_eq!(ok.resolved.len(), 3);

        for break_it in [
            |r: &mut ScanReport| r.ai_error = Some("every pass failed".to_string()),
            |r: &mut ScanReport| r.ai_blocked_reason = Some("blocked by the lock".to_string()),
        ] {
            let mut report = report.clone();
            break_it(&mut report);
            let v = retest_verdict(&src, &report, &covered);
            assert!(v.ai_incomplete);
            assert_eq!(
                v.not_rechecked,
                vec![
                    semantic.fingerprint.clone(),
                    ledger_semantic.fingerprint.clone()
                ],
                "semantic findings are not rechecked, by AI- id or by ledger tier"
            );
            assert_eq!(v.resolved, vec![deterministic.fingerprint.clone()]);
            assert_eq!(
                v.still_open,
                vec![present.fingerprint.clone()],
                "a semantic finding the retest did report is still present"
            );
        }
    }

    #[test]
    fn the_history_line_says_when_the_ai_review_did_not_finish() {
        let store = UowStore::new();
        let unit = store
            .create_native(Some("acme/api".to_string()), Some("proj-1".to_string()))
            .unwrap();
        store.set_source(
            &unit.story_id,
            Some(UowSource::Findings(source_with(
                vec![
                    sf("AI-X-1", "src/a.rs", 1, "x"),
                    sf("SEC-A-1", "src/b.rs", 2, "y"),
                ],
                &["AI-X-1"],
            ))),
        );
        let mut report = report_with(vec![]);
        report.ai_error = Some("boom".to_string());
        let entry = delta_entry(report, &["acme/api"], "r");
        record_retest_evidence(&store, "proj-1", &entry, "r");
        let got = store.get(&unit.story_id).unwrap();
        assert!(got.retests[0].ai_incomplete);
        assert_eq!(got.retests[0].not_rechecked.len(), 1);
        assert_eq!(got.retests[0].resolved.len(), 1);
        assert_eq!(
            got.history.last().unwrap().text,
            "Delta retest insp-d at fedcba9: 1 of 2 findings gone, 0 still present, 1 not rechecked; the AI review did not finish, so semantic findings could not be rechecked."
        );
    }

    #[test]
    fn a_renamed_file_is_not_a_fix() {
        let old = sf("SEC-A-1", "src/old.rs", 5, "risky call");
        let src = source_with(vec![old.clone()], &["SEC-A-1"]);
        let covered = ["acme/api".to_string()];
        // The same line now lives in src/new.rs.
        let report = report_with(vec![finding("SEC-A-1", "src/new.rs", 5, "risky call")]);
        let v = retest_verdict(&src, &report, &covered);
        assert!(v.resolved.is_empty());
        assert_eq!(v.still_open, vec![old.fingerprint.clone()]);
        assert_eq!(v.moved.get(&old.fingerprint).unwrap(), "src/new.rs");

        // When the unit itself records that finding at src/new.rs too, that row is its own
        // finding's, not the old one moved: the old one is genuinely gone.
        let twin = sf("SEC-A-1", "src/new.rs", 5, "risky call");
        let src = source_with(vec![old.clone(), twin], &["SEC-A-1"]);
        let v = retest_verdict(&src, &report, &covered);
        assert_eq!(v.resolved, vec![old.fingerprint.clone()]);
        assert!(v.moved.is_empty());
    }

    #[test]
    fn the_history_line_names_how_many_findings_moved() {
        let store = UowStore::new();
        let unit = store
            .create_native(Some("acme/api".to_string()), Some("proj-1".to_string()))
            .unwrap();
        store.set_source(
            &unit.story_id,
            Some(UowSource::Findings(source_with(
                vec![sf("SEC-A-1", "src/old.rs", 5, "risky call")],
                &["SEC-A-1"],
            ))),
        );
        let entry = delta_entry(
            report_with(vec![finding("SEC-A-1", "src/new.rs", 5, "risky call")]),
            &["acme/api"],
            "r",
        );
        record_retest_evidence(&store, "proj-1", &entry, "r");
        let got = store.get(&unit.story_id).unwrap();
        assert_eq!(got.retests[0].moved.len(), 1);
        assert!(got
            .history
            .last()
            .unwrap()
            .text
            .contains("0 of 1 findings gone, 1 still present (1 moved to another file)."));
    }

    #[test]
    fn rules_changed_compares_the_units_own_fingerprint_and_falls_back_to_the_base() {
        let store = UowStore::new();
        let make = |fp: &str| {
            let unit = store
                .create_native(Some("acme/api".to_string()), Some("proj-1".to_string()))
                .unwrap();
            let mut src = source_with(vec![sf("A-1", "src/a.rs", 1, "x")], &["A-1"]);
            src.rules_fingerprint = fp.to_string();
            store.set_source(&unit.story_id, Some(UowSource::Findings(src)));
            unit.story_id
        };
        let same = make("rules-now");
        let older = make("rules-then");
        let unrecorded = make("");
        let entry = delta_entry(report_with(vec![]), &["acme/api"], "rules-now");
        // The base inspection's fingerprint differs from the retest's, but each unit that
        // recorded its own is judged by it.
        record_retest_evidence(&store, "proj-1", &entry, "rules-base");
        let flag = |id: &str| store.get(id).unwrap().retests[0].rules_changed;
        assert!(!flag(&same), "the unit's own fingerprint matches");
        assert!(flag(&older), "the unit's own fingerprint differs");
        assert!(
            flag(&unrecorded),
            "no own fingerprint: the base is the fallback"
        );
    }

    #[tokio::test]
    async fn extra_grounding_keeps_the_resume_directive_first_and_adds_the_findings_block() {
        let store = UowStore::new();
        let unit = store
            .create_native(Some("acme/api".to_string()), Some("proj-1".to_string()))
            .unwrap();
        store.set_source(
            &unit.story_id,
            Some(UowSource::Findings(source_with(
                vec![sf("SEC-A-1", "src/a.rs", 1, "x")],
                &["SEC-A-1"],
            ))),
        );
        let directive = "## RESUMED AFTER HUMAN REVIEW\n\nApprove it.".to_string();
        let got = extra_grounding_for(&store, &unit.story_id, Some(directive.clone()))
            .await
            .expect("grounding");
        assert!(got.starts_with(&directive), "{got}");
        let at = got
            .find("## Findings this change must close")
            .expect("the block");
        assert!(at > directive.len(), "{got}");
        // A unit that did not come from findings keeps only what it had.
        let plain = store.create_native(None, None).unwrap();
        assert_eq!(
            extra_grounding_for(&store, &plain.story_id, Some(directive.clone())).await,
            Some(directive)
        );
        assert_eq!(
            extra_grounding_for(&store, &plain.story_id, None).await,
            None
        );
    }

    #[test]
    fn free_branch_appends_a_number_when_the_name_is_taken() {
        let taken: HashSet<String> = ["camerata/remediation/a-1".to_string()].into();
        assert_eq!(
            free_branch("camerata/remediation/a-1", &taken).unwrap(),
            "camerata/remediation/a-1-2"
        );
        assert_eq!(
            free_branch("camerata/remediation/b-2", &taken).unwrap(),
            "camerata/remediation/b-2"
        );
    }

    #[test]
    fn free_branch_gives_up_with_a_plain_error_past_the_cap() {
        let mut taken: HashSet<String> = HashSet::new();
        taken.insert("camerata/remediation/a-1".to_string());
        for n in 2..=BRANCH_TRIES {
            taken.insert(format!("camerata/remediation/a-1-{n}"));
        }
        let err = free_branch("camerata/remediation/a-1", &taken).unwrap_err();
        assert!(err.contains("No free branch name"), "{err}");
        // One short of the cap still finds a name.
        taken.remove(&format!("camerata/remediation/a-1-{}", BRANCH_TRIES));
        assert_eq!(
            free_branch("camerata/remediation/a-1", &taken).unwrap(),
            format!("camerata/remediation/a-1-{}", BRANCH_TRIES)
        );
    }
}

#[cfg(test)]
#[path = "remediation_tests.rs"]
mod http_tests;
