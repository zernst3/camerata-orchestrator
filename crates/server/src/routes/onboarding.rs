//! Onboarding handlers: job status, finding context and fix, arming and applying
//! rules, drafts, ignores, CI rules, greenfield, emit and adopt.

use crate::*;

/// Poll an async audit job: status, progress (done/total passes), incremental findings, and
/// the final report once done. `null` for an unknown id.
/// Enriched job status response: the job state plus idle time and cancel flag.
#[derive(Debug, serde::Serialize)]
pub(crate) struct JobStatusResponse {
    pub(crate) job: crate::jobs::JobState,
    pub(crate) idle_ms: Option<u128>,
    pub(crate) cancel_requested: bool,
}

pub(crate) async fn onboard_audit_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    match state.jobs.get(&id) {
        Some(job) => {
            let idle_ms = state.jobs.idle_ms(&id, now_ms);
            let cancel_requested = state.jobs.is_cancel_requested(&id);
            Json(Some(JobStatusResponse {
                job,
                idle_ms,
                cancel_requested,
            }))
            .into_response()
        }
        None => Json::<Option<JobStatusResponse>>(None).into_response(),
    }
}

/// `POST /api/onboard/audit/job/:id/cancel` — cancel an in-flight audit job.
pub(crate) async fn cancel_audit_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    state.jobs.cancel(&id);
    StatusCode::NO_CONTENT
}

/// `GET /api/onboard/finding-context` — the enclosing-code-block lookup for the scan-review
/// finding modal (`docs/design/2026-07-27_finding-code-context.md`). ON-DEMAND, not scan-time
/// cached (see `onboard::finding_context` module docs for why): a handful of reads per triage
/// session, zero LLM tokens (a local read + deterministic parse).
///
/// The code is read from the INSPECTED COMMIT through git objects (so old findings stay accurate
/// while the checkout sits at the latest commit), from the working tree only when the scan had
/// no commit, and not at all when the repo's source was deleted (`status: "source_deleted"`).
/// Where it may be read from is decided by `client_source::read::source_for_read`, the one
/// resolver the code viewer will share. Every failure degrades to a `status` + `reason` the UI
/// shows beside the stored snippet; path traversal is the only non-200. The response is never
/// cached by the webview (`Cache-Control: no-store`).
pub(crate) async fn onboard_finding_context(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<crate::client_source::read::FindingContextParams>,
) -> Response {
    crate::client_source::read::finding_context(&state, q).await
}

/// `POST /api/onboard/finding-context/batch` — the stored line plus three lines either side for up
/// to 200 `(repo, path, line, sha)` tuples (`WORK_ORDER_REVIEW_SPEED.md` B.2 "Data" and B.3).
/// A POST only because the tuples do not fit a query string: it reads and writes nothing else.
/// Shares the single-finding resolver (`client_source::read::source_for_read`), path jail, git
/// read gate, redactor and status vocabulary; a deleted source answers `source_deleted` per item
/// and reads nothing. Never cached by the webview. Shape: `onboard::finding_context_batch`.
pub(crate) async fn onboard_finding_context_batch(
    State(state): State<AppState>,
    Json(body): Json<crate::onboard::finding_context_batch::BatchParams>,
) -> Response {
    crate::onboard::finding_context_batch::finding_context_batch(&state, body).await
}

/// `GET /api/onboard/source-file` — the whole file (or an 800-line window) behind the in-app code
/// viewer, read from the inspected commit through git objects, redacted line by line and
/// highlighted server-side (`WORK_ORDER_REVIEW_SPEED.md` A.2 / A.3). Shares the finding-context
/// resolver (`client_source::read::source_for_read`), path jail and redactor; a deleted source
/// answers `status: "source_deleted"` with the date and reads nothing. Read-only; the response
/// is never cached by the webview. Shape and statuses: `onboard::source_file` module docs.
pub(crate) async fn onboard_source_file(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<crate::onboard::source_file::SourceFileParams>,
) -> Response {
    crate::onboard::source_file::source_file(&state, q).await
}

/// `GET /api/onboard/symbol-refs`: where a symbol is used: one read-only whole-word search at
/// the inspected commit, noise paths dropped, every hit line redacted, at most 200 hits with a
/// `more` flag (`WORK_ORDER_REVIEW_SPEED.md` A.6). Shares the source-file resolver
/// (`client_source::read::source_for_read`), so a deleted source answers `status:
/// "source_deleted"` and runs nothing. Read-only; never cached by the webview. Shape and
/// statuses: `onboard::symbol_refs` module docs.
pub(crate) async fn onboard_symbol_refs(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<crate::onboard::symbol_refs::SymbolRefsParams>,
) -> Response {
    crate::onboard::symbol_refs::symbol_refs(&state, q).await
}

/// Query parameters for `GET /api/onboard/finding-fix`.
#[derive(serde::Deserialize)]
pub(crate) struct FindingFixQuery {
    pub(crate) rule_id: String,
}

/// `GET /api/onboard/finding-fix?rule_id=` — the recommended-fix lookup for the scan-review
/// finding modal. Resolves `rule_id` against the loaded corpus via
/// [`crate::report_export::resolve_fix`] — the EXACT SAME function the PDF's "Fix:" line and
/// the xlsx "Recommended Fix" column call. Routing the modal through this endpoint (rather
/// than having the UI re-derive the join over its own copy of the corpus) is what guarantees
/// byte-identical output across all three surfaces: one function, three callers, never a
/// second divergent computation.
///
/// Always 200; body is `{ "fix": "..." }` or `{ "fix": null }` — `null` (never a fabricated
/// sentence, and never the rule's `directive`) when the corpus is absent, the rule id has no
/// corpus entry, or the rule has no adopted default option/authored `remediation` — same
/// degrade-soft contract as `resolve_fix` itself. No project/repo resolution needed (the corpus
/// is global, not per-project), so unlike `onboard_finding_context` this handler takes no
/// `State`. This is a RULE-level lookup (this modal has only a `rule_id`, no specific finding),
/// so it resolves `resolve_fix` against `Finding::default()` — an authored remediation's
/// placeholder tokens render with their readable generic filler (e.g. "the affected table")
/// rather than naming a real object, exactly like any other finding whose detector didn't
/// capture one.
pub(crate) async fn onboard_finding_fix(
    axum::extract::Query(q): axum::extract::Query<FindingFixQuery>,
) -> impl IntoResponse {
    let corpus_path = camerata_rules::corpus_path();
    let corpus = if corpus_path.exists() {
        Some(camerata_rules::load_corpus_lenient(&corpus_path).await.0)
    } else {
        None
    };
    let fix = crate::report_export::resolve_fix(
        &q.rule_id,
        corpus.as_ref(),
        &crate::onboard::Finding::default(),
        None,
    );
    Json(serde_json::json!({ "fix": fix }))
}

#[derive(serde::Deserialize)]
pub(crate) struct DetectRepoReq {
    pub(crate) path: String,
}

/// Derive `owner/repo` from a LOCAL git checkout's origin remote — so the UI can let a
/// developer navigate to a repo folder instead of typing the identifier.
/// #14 — run the end-to-end gate-loop probe (both layers, deterministic, no model) and return a
/// GO/NO-GO the Governed Development screen surfaces as an in-app gate self-check.
pub(crate) async fn gate_probe() -> Json<serde_json::Value> {
    match camerata_fleet::gate_probe::run_gate_probe().await {
        Ok(r) => {
            let checks: Vec<serde_json::Value> = r
                .layer1
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "rule": c.rule,
                        "label": c.label,
                        "denied": c.denied,
                        "isolated_denied": c.isolated_denied,
                        "detail": c.detail,
                    })
                })
                .collect();
            Json(serde_json::json!({
                "ok": true,
                "go": r.go(),
                "story": r.story,
                "layer1": checks,
                "layer1_denied": r.layer1_denied_count(),
                "layer1_isolated": r.layer1_isolated_count(),
                "layer1_total": r.layer1_total(),
                "layer1_clean_allowed": r.layer1_clean_allowed,
                "layer2_bounced": r.layer2_bounced,
                "layer2_clean": r.layer2_clean,
                "agent_passes": r.agent_passes,
            }))
        }
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

pub(crate) async fn detect_repo(
    State(state): State<AppState>,
    Json(req): Json<DetectRepoReq>,
) -> Json<serde_json::Value> {
    match crate::workspace::detect_inspection_repo(std::path::Path::new(&req.path)).await {
        Ok(found) => {
            let repo = found.name;
            // #50: onboarding is one-time per repo. Report whether this repo is ALREADY onboarded
            // (in any project's onboarded list) so the UI can block a re-onboard and route the
            // user to the workspace instead of producing a duplicate set of issues + branch.
            let onboarded_in = state
                .projects
                .list()
                .into_iter()
                .find(|p| p.onboarded.iter().any(|r| r == &repo))
                .map(|p| p.name);
            Json(serde_json::json!({
                "ok": true,
                "repo": repo,
                "onboarded": onboarded_in.is_some(),
                "onboarded_project": onboarded_in,
                // A folder not on GitHub is added for inspection only; the UI says so.
                "github": found.github,
                "note": found.note,
            }))
        }
        Err(message) => Json(serde_json::json!({ "ok": false, "message": message })),
    }
}

/// Request to file accepted findings as a tech-debt ticket.
#[derive(serde::Deserialize)]
pub(crate) struct TicketReq {
    /// `owner/repo` to file the issue in.
    pub(crate) repo: String,
    #[serde(default)]
    pub(crate) title: Option<String>,
    /// The selected findings to record.
    pub(crate) findings: Vec<crate::onboard::Finding>,
}

/// Accept selected findings as tech debt: open a GitHub issue with them. Gated on
/// the token (needs Issues write). Returns `{ ok, url, message }`.
pub(crate) async fn onboard_ticket(
    State(state): State<AppState>,
    Json(req): Json<TicketReq>,
) -> Json<serde_json::Value> {
    let Some((owner, repo)) = req.repo.split_once('/') else {
        return Json(
            serde_json::json!({ "ok": false, "message": "Target repo must be `owner/repo`." }),
        );
    };
    if req.findings.is_empty() {
        return Json(serde_json::json!({ "ok": false, "message": "No findings selected." }));
    }
    let token = state.github_token();
    let Some(token) = token else {
        return Json(
            serde_json::json!({ "ok": false, "message": "Connect GitHub to file a ticket." }),
        );
    };
    let title = req.title.unwrap_or_else(|| {
        format!(
            "Tech debt: {} audit finding(s) accepted",
            req.findings.len()
        )
    });
    match crate::onboard::create_tech_debt_ticket(owner, repo, &token, &title, &req.findings).await
    {
        Ok(url) => Json(serde_json::json!({ "ok": true, "url": url })),
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

/// Request to arm a set of repos with the approved (resolved) rules.
#[derive(serde::Deserialize)]
pub(crate) struct ArmReq {
    /// Fully-resolved rules (each with its chosen directive + which repos it goes to).
    pub(crate) rules: Vec<crate::arm::ArmRule>,
    /// User-authored custom rules (#49) — saved to the project's ruleset.custom and rendered
    /// as CUSTOM-<name> blocks in AGENTS.md. `domain` routes them (`*` = all repos, else a repo).
    #[serde(default)]
    pub(crate) custom: Vec<crate::project::CustomRule>,
    /// The current findings to snapshot as the baseline (accepted pre-existing debt),
    /// so the team is unblocked at onboarding and the gate enforces only on new code.
    #[serde(default)]
    pub(crate) findings: Vec<ArmFinding>,
}

/// A finding the UI sends to arm, to be snapshotted into the per-repo baseline.
#[derive(serde::Deserialize)]
pub(crate) struct ArmFinding {
    #[serde(default)]
    pub(crate) repo: String,
    #[serde(default)]
    pub(crate) path: String,
    #[serde(default)]
    pub(crate) rule_id: String,
    #[serde(default)]
    pub(crate) snippet: String,
    #[serde(default)]
    pub(crate) status: String,
}

/// Build the per-repo `.camerata/baseline.json` contents from the armed findings:
/// snapshot the currently-enforced (active) violations as accepted pre-existing debt,
/// fingerprinted so the ratchet works. Returns `repo -> baseline JSON`.
pub(crate) fn baselines_from_findings(
    findings: &[ArmFinding],
    accepted_by: &str,
) -> std::collections::HashMap<String, String> {
    use crate::suppression::{baseline_entry, Baseline, FindingRef};
    let now = chrono::Utc::now().to_rfc3339();
    let mut by_repo: std::collections::HashMap<String, Baseline> = std::collections::HashMap::new();
    for f in findings {
        // Only snapshot real, currently-enforced violations — not the require-reason
        // meta-finding, not already-suppressed ones.
        if f.status != "active" || f.rule_id == crate::suppression::REASONLESS_RULE_ID {
            continue;
        }
        let fr = FindingRef {
            rule_id: f.rule_id.clone(),
            path: f.path.clone(),
            line: 0,
            snippet: f.snippet.clone(),
        };
        let entry = baseline_entry(&fr, accepted_by, &now, "pre-existing at onboarding");
        by_repo
            .entry(f.repo.clone())
            .or_default()
            .entries
            .push(entry);
    }
    by_repo
        .into_iter()
        .filter_map(|(repo, b)| {
            serde_json::to_string_pretty(&b)
                .ok()
                .map(|json| (repo, json))
        })
        .collect()
}

/// Compute the exact set of files Apply will write for each repo, from a resolved
/// `ArmReq`. This is the SINGLE source of truth for "what files land in each repo",
/// shared by `onboard_apply` (which then writes them) and `onboard_apply_preflight`
/// (which checks which already exist). Keeping both paths on this helper guarantees the
/// overwrite warning lists exactly the files Apply would clobber — no drift.
///
/// Returns `repo -> Vec<(repo_relative_path, content)>`. Repos with no rules/custom for
/// them are omitted (Apply skips them too).
pub(crate) fn apply_files_per_repo(
    req: &ArmReq,
    custom: &[crate::project::CustomRule],
) -> std::collections::BTreeMap<String, Vec<(String, String)>> {
    let repo_local: Vec<&crate::arm::ArmRule> = req
        .rules
        .iter()
        .filter(|r| r.scope != "cross-repo" && r.scope != "process")
        .collect();
    let mut repos: Vec<String> = repo_local.iter().flat_map(|r| r.repos.clone()).collect();
    repos.sort();
    repos.dedup();

    let baselines = baselines_from_findings(&req.findings, "architect");

    let mut out: std::collections::BTreeMap<String, Vec<(String, String)>> =
        std::collections::BTreeMap::new();
    for repo in &repos {
        let repo_rules: Vec<&crate::arm::ArmRule> = repo_local
            .iter()
            .copied()
            .filter(|r| r.repos.iter().any(|x| x == repo))
            .collect();
        let repo_custom: Vec<&crate::project::CustomRule> =
            custom.iter().filter(|c| c.applies_to_repo(repo)).collect();
        if repo_rules.is_empty() && repo_custom.is_empty() {
            continue;
        }
        let mut files = crate::arm::arm_files_for_repo(&repo_rules, &repo_custom);
        if let Some(baseline_json) = baselines.get(repo) {
            files.push((".camerata/baseline.json".to_string(), baseline_json.clone()));
        }
        out.insert(repo.clone(), files);
    }
    out
}

/// Preflight for Apply: for each target repo, resolve its local dir (same resolution Apply
/// uses) and report which governance files Camerata is about to write ALREADY EXIST on disk
/// and would be overwritten. The UI calls this BEFORE firing `onboard_apply` so it can warn
/// the architect and require explicit acknowledgement before clobbering hand-written files.
///
/// Returns `{ ok, repos: [{ repo, existing_files: [path...] }, ...] }`. Only repos with at
/// least one would-be-overwritten file are listed (an empty `repos` means Apply is safe and
/// the UI should proceed without nagging).
pub(crate) async fn onboard_apply_preflight(
    State(state): State<AppState>,
    Json(req): Json<ArmReq>,
) -> Json<serde_json::Value> {
    let workspace_root = state.settings.workspace_root();
    let custom = state
        .projects
        .active()
        .map(|p| p.ruleset.custom)
        .unwrap_or_default();

    let files_per_repo = apply_files_per_repo(&req, &custom);

    let mut repos_out = Vec::new();
    for (repo, files) in &files_per_repo {
        let override_path = state.settings.repo_path(repo);
        let Some(dir) = crate::workspace::resolve_repo_dir(
            override_path.as_deref(),
            workspace_root.as_deref(),
            repo,
        ) else {
            // No resolvable local dir → nothing to overwrite (Apply will report the
            // unresolved repo itself). Skip from the warning list.
            continue;
        };
        let will_write: Vec<String> = files.iter().map(|(p, _)| p.clone()).collect();
        let existing = crate::arm::existing_governance_files(&dir, &will_write);
        if !existing.is_empty() {
            repos_out.push(serde_json::json!({
                "repo": repo,
                "existing_files": existing,
            }));
        }
    }

    Json(serde_json::json!({ "ok": true, "repos": repos_out }))
}

/// Arm: install the approved ruleset into each repo via a governance PR (AGENTS.md
/// / CONVENTIONS.md / gate config), per the camerata-ai emit format. Gated on the
/// token (needs Contents + PR write). Returns a per-repo result list.
pub(crate) async fn onboard_arm(
    State(state): State<AppState>,
    Json(req): Json<ArmReq>,
) -> Json<serde_json::Value> {
    if req.rules.is_empty() {
        return Json(serde_json::json!({ "ok": false, "message": "No rules selected to arm." }));
    }
    let token = state.github_token();
    let Some(token) = token else {
        return Json(serde_json::json!({ "ok": false, "message": "Connect GitHub to arm." }));
    };

    // Save the armed ruleset to the active project (create one if none) so the
    // project is the source of truth and re-emit works. Baselines the rules against the
    // corpus they are about to be emitted from, so drift detection starts at apply time.
    let corpus = load_corpus_for_baseline().await;
    save_armed_to_project(&state, &req.rules, &req.custom, corpus.as_ref());

    // Only repo-local rules emit into repo files; cross-repo + process rules are
    // project-level (the gates read them from the project store). Defensively normalize the
    // leaked single-repo sentinel out of the incoming rules' repos (mapping it to the active
    // project's real repos) BEFORE the clone loop — shared helper, so no drift with
    // `resolve_project_arm_rules` / `onboard_apply`.
    let project_repos = state.projects.active().map(|p| p.repos).unwrap_or_default();
    let repo_local: Vec<crate::arm::ArmRule> = req
        .rules
        .iter()
        .filter(|r| r.scope != "cross-repo" && r.scope != "process")
        .map(|r| {
            let mut r = r.clone();
            r.repos = normalize_repos(&r.repos, &project_repos);
            r
        })
        .filter(|r| !r.repos.is_empty())
        .collect();
    let mut repos: Vec<String> = repo_local.iter().flat_map(|r| r.repos.clone()).collect();
    repos.sort();
    repos.dedup();
    let custom = state
        .projects
        .active()
        .map(|p| p.ruleset.custom)
        .unwrap_or_default();

    // Snapshot the current violations as the baseline (accepted pre-existing debt), so
    // the team is unblocked at onboarding and the gate enforces only on new code.
    let baselines = baselines_from_findings(&req.findings, "architect");

    let results = emit_to_repos(&repos, &repo_local, &custom, &baselines, &token).await;
    Json(serde_json::json!({ "ok": true, "results": results }))
}

/// Apply: write the approved ruleset onto a governance branch in each repo's LOCAL clone AND
/// push that branch to origin — WITHOUT opening a PR. The architect can edit the working copy
/// freely, then open the PR as a separate step (`onboard_open_pr`). Needs a workspace folder
/// set + a token with Contents write. The branch lands BOTH locally and on origin.
pub(crate) async fn onboard_apply(
    State(state): State<AppState>,
    Json(req): Json<ArmReq>,
) -> Json<serde_json::Value> {
    if req.rules.is_empty() && req.custom.is_empty() {
        return Json(serde_json::json!({ "ok": false, "message": "No rules selected to apply." }));
    }
    let token = state.github_token();
    let Some(token) = token else {
        return Json(
            serde_json::json!({ "ok": false, "message": "Connect GitHub to apply (the branch is pushed to origin)." }),
        );
    };
    // Apply writes into each repo's LOCAL clone. A repo's local dir is resolved per-repo:
    // a per-repo path override (chosen via repo health) wins; otherwise it's cloned under the
    // workspace folder. We no longer hard-require the workspace folder up front — a project
    // whose repos all have explicit local folders can apply without one.
    let workspace_root = state.settings.workspace_root();

    // Source of truth: save the armed ruleset to the active project (create one if none),
    // baselining each rule against the corpus it is emitted from (apply-time drift baseline).
    let corpus = load_corpus_for_baseline().await;
    save_armed_to_project(&state, &req.rules, &req.custom, corpus.as_ref());

    // Defensively normalize the leaked single-repo sentinel out of the INCOMING rules'
    // repos (the UI can POST it directly) BEFORE any clone loop, mapping it to the active
    // project's real repos. Uses the same helper as `resolve_project_arm_rules` so the two
    // paths can't drift.
    let project_repos = state.projects.active().map(|p| p.repos).unwrap_or_default();
    let repo_local: Vec<crate::arm::ArmRule> = req
        .rules
        .iter()
        .filter(|r| r.scope != "cross-repo" && r.scope != "process")
        .map(|r| {
            let mut r = r.clone();
            r.repos = normalize_repos(&r.repos, &project_repos);
            r
        })
        .filter(|r| !r.repos.is_empty())
        .collect();
    let mut repos: Vec<String> = repo_local.iter().flat_map(|r| r.repos.clone()).collect();
    repos.sort();
    repos.dedup();

    // Fail fast (with the actionable message) only when NOTHING local is available: no
    // workspace folder AND no per-repo override for any target repo. Otherwise apply
    // per-repo, reporting any individually-unresolved repo in its result row.
    let has_any_local =
        workspace_root.is_some() || repos.iter().any(|r| state.settings.repo_path(r).is_some());
    if !has_any_local {
        return Json(
            serde_json::json!({ "ok": false, "message": "Set a local workspace folder (Settings) or choose each repo's local folder (repo health) — Apply writes into the repo's local clone." }),
        );
    }

    let custom = state
        .projects
        .active()
        .map(|p| p.ruleset.custom)
        .unwrap_or_default();
    let baselines = baselines_from_findings(&req.findings, "architect");

    let mut results = Vec::new();
    let mut applied: Vec<String> = Vec::new();
    for repo in &repos {
        let repo_rules: Vec<&crate::arm::ArmRule> = repo_local
            .iter()
            .filter(|r| r.repos.iter().any(|x| x == repo))
            .collect();
        let repo_custom: Vec<&crate::project::CustomRule> =
            custom.iter().filter(|c| c.applies_to_repo(repo)).collect();
        if repo_rules.is_empty() && repo_custom.is_empty() {
            continue;
        }
        let mut files = crate::arm::arm_files_for_repo(&repo_rules, &repo_custom);
        if let Some(baseline_json) = baselines.get(repo) {
            files.push((".camerata/baseline.json".to_string(), baseline_json.clone()));
        }
        // Resolve THIS repo's local dir: per-repo override wins, else <workspace_root>/<repo>.
        let override_path = state.settings.repo_path(repo);
        let Some(dir) = crate::workspace::resolve_repo_dir(
            override_path.as_deref(),
            workspace_root.as_deref(),
            repo,
        ) else {
            results.push(serde_json::json!({
                "repo": repo, "ok": false,
                "message": "No local path — choose this repo's folder (repo health) or set a workspace folder."
            }));
            continue;
        };
        // Clone into the workspace root only when there's no explicit override (never clone
        // over a folder the architect chose by hand), and never for a repo whose source was
        // deleted on purpose: that stays deleted until "Re-fetch source".
        let clone_root = match crate::client_source::resolution::clone_root_for(
            &state,
            repo,
            override_path.as_deref(),
            workspace_root.as_deref(),
        ) {
            Ok(root) => root,
            Err(refusal) => {
                results.push(serde_json::json!({
                    "repo": repo, "ok": false, "code": refusal.code, "message": refusal.message
                }));
                continue;
            }
        };
        let msg = format!("chore(governance): apply Camerata ruleset to {repo}");
        match crate::workspace::apply_local_and_push(
            &dir,
            repo,
            clone_root,
            crate::arm::ARM_BRANCH,
            &files,
            &msg,
            &token,
        )
        .await
        {
            Ok(path) => {
                applied.push(repo.clone());
                results.push(serde_json::json!({
                    "repo": repo, "ok": true, "branch": crate::arm::ARM_BRANCH, "path": path
                }));
            }
            Err(e) => results.push(serde_json::json!({
                "repo": repo, "ok": false, "message": format!("{e}")
            })),
        }
    }
    // Applying the ruleset IS the completion act: mark the successfully-applied repos
    // onboarded on the active project (no audit required — onboarding never gates on the
    // optional violation scan). save_armed_to_project above guarantees an active project.
    if !applied.is_empty() {
        if let Some(active) = state.projects.active() {
            state
                .projects
                .update(&active.id, |p| p.mark_onboarded(&applied));
        }
    }
    Json(
        serde_json::json!({ "ok": true, "branch": crate::arm::ARM_BRANCH, "onboarded": applied, "results": results }),
    )
}

/// Per-repo local-path resolution for a project (issue #33): for each repo, is there a local
/// folder, is it a git checkout, and does its origin match? Drives the broken-path health
/// banner + per-repo icons + resolve action. Continuous (the UI calls it on load + after
/// import + after a resolve).
pub(crate) async fn project_repo_health(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let Some(project) = state.projects.get(&id) else {
        return Json(serde_json::json!({ "ok": false, "message": "unknown project", "repos": [] }));
    };
    let out = crate::client_source::resolution::resolve_all(&state, &project).await;
    Json(serde_json::json!({ "ok": true, "repos": out }))
}

#[derive(serde::Deserialize)]
pub(crate) struct RepoPathReq {
    pub(crate) repo: String,
    /// The chosen local folder; empty clears the override.
    #[serde(default)]
    pub(crate) path: String,
}

/// Set (or clear) a repo's machine-local path override, then re-validate it (issue #33).
pub(crate) async fn set_repo_path(
    State(state): State<AppState>,
    Json(req): Json<RepoPathReq>,
) -> Json<serde_json::Value> {
    let path = if req.path.trim().is_empty() {
        None
    } else {
        Some(req.path.clone())
    };
    state.settings.set_repo_path(&req.repo, path);
    // Re-resolve so the UI gets the post-set status without a second round trip.
    let workspace_root = state.settings.workspace_root();
    let override_path = state.settings.repo_path(&req.repo);
    let res = crate::workspace::repo_resolution(
        override_path.as_deref(),
        workspace_root.as_deref(),
        &req.repo,
    )
    .await;
    Json(serde_json::json!({ "ok": true, "resolution": res }))
}

/// `GET /api/projects/:id/readiness` — the project's DERIVED readiness gate (ADR
/// `2026-07-01_project-readiness-gate`).
///
/// Readiness is derived from whether each in-scope repo resolves to a real local git clone
/// (honoring the machine-local per-repo path override, else `<workspace_root>/<owner>/<repo>`, and
/// validating the folder's `origin` matches the repo). The pure classifier lives in
/// `camerata_app_core::readiness`; this adapter supplies the per-repo booleans via
/// `workspace::repo_resolution` (the same signal `project_repo_health` uses).
///
/// NOTE ON EXPOSURE: a SEPARATE route (not an extension of `GET .../checkout`). The `checkout`
/// response is built from `workspace::checkout_status`, which keys off `<root>/<owner>/<repo>`
/// ONLY and does NOT honor per-repo path overrides — so deriving readiness from it would be wrong
/// for any linked (overridden) repo. `repo_resolution` is the correct signal, and it is what the
/// health banner already uses, so a dedicated `readiness` route keyed off the SAME resolution is
/// the least-disruptive, correct exposure.
///
/// Response: `{ "ok": true, "readiness": "ready"|"unlinked"|"partial", "repos": [<RepoResolution>] }`
/// (the per-repo resolutions are included so the resolve modal can show exactly which repos need
/// linking). `{ "ok": false, "message": ... }` when the project id is unknown.
pub(crate) async fn project_readiness(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let Some(project) = state.projects.get(&id) else {
        return Json(serde_json::json!({
            "ok": false,
            "message": "unknown project",
            "readiness": camerata_app_core::readiness::ProjectReadiness::Unlinked,
            "repos": [],
        }));
    };
    let resolutions = crate::client_source::resolution::resolve_all(&state, &project).await;
    let links: Vec<camerata_app_core::readiness::RepoLink> = resolutions
        .iter()
        .map(crate::client_source::resolution::link_of)
        .collect();
    let readiness = camerata_app_core::readiness::classify_links(&links);
    Json(serde_json::json!({
        "ok": true,
        "readiness": readiness,
        "repos": resolutions,
    }))
}

/// Compute a project's derived readiness from the on-disk resolution of its repos. Shared by the
/// `readiness` endpoint's return value and the `link` endpoint (so a successful link returns the
/// FRESHLY-derived readiness). Returns `None` when the project id is unknown.
pub(crate) async fn derive_project_readiness(
    state: &AppState,
    id: &str,
) -> Option<camerata_app_core::readiness::ProjectReadiness> {
    let project = state.projects.get(id)?;
    let links: Vec<camerata_app_core::readiness::RepoLink> =
        crate::client_source::resolution::resolve_all(state, &project)
            .await
            .iter()
            .map(crate::client_source::resolution::link_of)
            .collect();
    Some(camerata_app_core::readiness::classify_links(&links))
}

#[derive(serde::Deserialize)]
pub(crate) struct LinkRepoReq {
    /// Absolute path to an existing local clone the architect selected.
    pub(crate) path: String,
}

/// `POST /api/projects/:id/repos/:repo/link` — LINK an existing local clone to a repo (ADR
/// `2026-07-01_project-readiness-gate`, the "Select existing" resolve path). `:repo` is
/// percent-encoded into one path segment (axum's `Path` extractor decodes it), matching the
/// repo-rule-override route convention.
///
/// Body: `{ "path": "/abs/path/to/clone" }`.
///
/// Validates the folder is a git clone whose `origin` matches the project's identity for `:repo`
/// (accepts both `https://github.com/owner/repo(.git)` and `git@github.com:owner/repo(.git)`,
/// normalized case-insensitively). On match it records the folder as this repo's machine-local
/// per-repo path OVERRIDE (via `settings.set_repo_path` — NOT on `Project`, so it is never
/// exported) and returns the freshly-derived readiness. On any failure NOTHING is recorded.
///
/// Success (200): `{ "ok": true, "readiness": "ready"|"unlinked"|"partial" }`
/// Failure (400): `{ "ok": false, "error": "<reason>" }` — bad path / not a git repo / origin
/// mismatch. The Clone path is a SEPARATE action (reuses `POST .../checkout`); this endpoint never
/// clones.
pub(crate) async fn link_repo(
    State(state): State<AppState>,
    Path((id, repo)): Path<(String, String)>,
    Json(req): Json<LinkRepoReq>,
) -> impl IntoResponse {
    // The project must exist.
    if state.projects.get(&id).is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "ok": false, "error": "unknown project" })),
        );
    }
    let path = req.path.trim();
    if path.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "ok": false, "error": "path must not be empty" })),
        );
    }
    // VALIDATE before recording anything: the folder must be a git clone whose origin matches.
    let folder = std::path::PathBuf::from(path);
    if let Err(reason) = crate::workspace::validate_link_target(&folder, &repo).await {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "ok": false, "error": reason })),
        );
    }
    // Match: record the machine-local per-repo override and persist.
    state.settings.set_repo_path(&repo, Some(path.to_string()));
    // Return the FRESH readiness so the UI can activate the project without a second round trip.
    let readiness = derive_project_readiness(&state, &id)
        .await
        .unwrap_or(camerata_app_core::readiness::ProjectReadiness::Unlinked);
    (
        StatusCode::OK,
        Json(serde_json::json!({ "ok": true, "readiness": readiness })),
    )
}

/// `POST /api/projects/:id/reset-onboarding` — clear the PROJECT-LEVEL onboarded marker so a
/// repo (or all of them) can be re-scanned, WITHOUT deleting the project, its repos, or its
/// ruleset. Onboarding is a one-time gate (`Project.onboarded`); this is the only UI-reachable
/// way to unwind it short of deleting the whole project.
///
/// Also drops the project's in-progress onboarding draft (`DraftStore::clear`) so a fresh scan
/// starts clean rather than resuming stale selections from before the reset.
///
/// Success (200): `{ "ok": true }`. Failure (400): `{ "ok": false, "error": "unknown project" }`.
pub(crate) async fn reset_onboarding(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if state.projects.get(&id).is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "ok": false, "error": "unknown project" })),
        );
    }
    state.projects.update(&id, |p| p.onboarded.clear());
    state.draft.clear(&id);
    (StatusCode::OK, Json(serde_json::json!({ "ok": true })))
}

/// Load the saved onboarding draft (scan + audit + selections + dispositions), or `null`.
/// The active project's id, or `""` when none is active (drafts are keyed per project so
/// opening another project never clobbers this one's in-progress onboarding).
pub(crate) fn active_project_id(state: &AppState) -> String {
    state.projects.active().map(|p| p.id).unwrap_or_default()
}

/// Resolve the VCS-action process-rule config for a project id (GAP-2). Falls back to the
/// active project when `id` does not resolve, and to `ProcessRuleConfig::default()` when
/// there is no project at all — so the chokepoint gate has a rule set to enforce in every
/// case (the default enforces the conventional-commit + commit-doc baseline).
pub(crate) fn process_rule_config_for(
    state: &AppState,
    id: &str,
) -> camerata_checks::vcs_action::ProcessRuleConfig {
    if let Some(p) = state.projects.get(id) {
        return p.process_rule_config;
    }
    state
        .projects
        .active()
        .map(|p| p.process_rule_config)
        .unwrap_or_default()
}

pub(crate) async fn onboard_draft_get(
    State(state): State<AppState>,
) -> Json<Option<serde_json::Value>> {
    let pid = active_project_id(&state);
    Json(state.draft.load(&pid))
}

/// Save/replace the active project's onboarding draft (opaque blob; the UI owns its shape).
pub(crate) async fn onboard_draft_save(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let pid = active_project_id(&state);
    state.draft.save(&pid, body);
    Json(serde_json::json!({ "ok": true }))
}

/// Drop the active project's onboarding draft (completed, or starting fresh).
pub(crate) async fn onboard_draft_clear(State(state): State<AppState>) -> Json<serde_json::Value> {
    let pid = active_project_id(&state);
    state.draft.clear(&pid);
    Json(serde_json::json!({ "ok": true }))
}

#[derive(serde::Deserialize)]
pub(crate) struct OpenPrReq {
    #[serde(default)]
    pub(crate) repos: Vec<String>,
}

/// Open the governance PR from the already-applied + pushed branch (the explicit, separate
/// step after `onboard_apply`). One PR per repo into its default branch.
pub(crate) async fn onboard_open_pr(
    State(state): State<AppState>,
    Json(req): Json<OpenPrReq>,
) -> Json<serde_json::Value> {
    let token = state.github_token();
    let Some(token) = token else {
        return Json(
            serde_json::json!({ "ok": false, "message": "Connect GitHub to open the PR." }),
        );
    };
    let mut repos: Vec<String> = req
        .repos
        .into_iter()
        .filter(|r| !r.trim().is_empty())
        .collect();
    repos.sort();
    repos.dedup();
    if repos.is_empty() {
        return Json(serde_json::json!({ "ok": false, "message": "No repos to open a PR for." }));
    }
    let title = "Camerata governance: adopt the selected ruleset";
    let body = "Adopts the Camerata-selected ruleset for this repo (AGENTS.md / CONVENTIONS.md / \
        CI gate / baseline). Applied locally and pushed by Camerata onboarding; opened as a PR \
        on request.";
    // GAP-2 chokepoint: gate the governance PR's metadata. This is an orchestration-internal
    // PR with a machine-generated title/body, so it takes an auditable bypass rather than
    // HARD-BLOCK — a project whose rules the fixed metadata already satisfies passes cleanly.
    let vcs_config = process_rule_config_for(&state, &active_project_id(&state));
    match crate::vcs_choke::gated_pr_or_bypass(
        &vcs_config,
        title,
        body,
        Some("orchestration-internal onboarding governance PR; title/body are machine-generated"),
    ) {
        Ok(_) => {}
        Err(e) => {
            return Json(
                serde_json::json!({ "ok": false, "blocked": true, "message": format!("{e}") }),
            );
        }
    }
    let mut results = Vec::new();
    for repo in &repos {
        match crate::workspace::open_branch_pr(repo, crate::arm::ARM_BRANCH, title, body, &token)
            .await
        {
            Ok(url) => results.push(serde_json::json!({ "repo": repo, "ok": true, "url": url })),
            Err(e) => results
                .push(serde_json::json!({ "repo": repo, "ok": false, "message": format!("{e}") })),
        }
    }
    Json(serde_json::json!({ "ok": true, "results": results }))
}

/// A finding subset the UI sends with a triage action (ignore). `rule_id` + `path` +
/// `snippet` identify the violation for the baseline waiver (extra fields are ignored).
#[derive(serde::Deserialize)]
pub(crate) struct OnboardFinding {
    #[serde(default)]
    pub(crate) rule_id: String,
    #[serde(default)]
    pub(crate) path: String,
    #[serde(default)]
    pub(crate) snippet: String,
}

#[derive(serde::Deserialize)]
pub(crate) struct IgnoreReq {
    pub(crate) repo: String,
    pub(crate) findings: Vec<OnboardFinding>,
    /// Mandatory justification — a reason-less suppression is rejected (the invariant).
    pub(crate) reason: String,
    #[serde(default)]
    pub(crate) ticket: Option<String>,
}

/// Fetch and parse a repo's committed `.camerata/baseline.json` (default branch), or an
/// empty baseline if absent.
pub(crate) async fn fetch_baseline(
    owner: &str,
    repo: &str,
    token: &str,
) -> crate::suppression::Baseline {
    use base64::Engine as _;
    use camerata_worktracker::{HttpTransport, ReqwestTransport};
    let Ok(transport) = ReqwestTransport::new(format!("Bearer {token}")) else {
        return crate::suppression::Baseline::default();
    };
    let url =
        format!("https://api.github.com/repos/{owner}/{repo}/contents/.camerata/baseline.json");
    let Ok(resp) = transport.get(&url).await else {
        return crate::suppression::Baseline::default();
    };
    if !(200..300).contains(&resp.status) {
        return crate::suppression::Baseline::default();
    }
    let decoded = serde_json::from_str::<serde_json::Value>(&resp.body)
        .ok()
        .and_then(|v| v["content"].as_str().map(|s| s.replace('\n', "")))
        .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok());
    decoded
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Durable, auditable IGNORE: append the selected findings to the repo's central
/// baseline as reasoned suppressions (reason mandatory; who/when stamped; optional
/// ticket tie-back), and open a governed PR. NOT a one-time dismissal — it persists,
/// shows in the diff, and rolls up into the audit registry.
pub(crate) async fn onboard_ignore(
    State(state): State<AppState>,
    Json(req): Json<IgnoreReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    if req.reason.trim().is_empty() {
        return Err(AppError::bad_request(anyhow::anyhow!(
            "a reason is required to ignore a finding"
        )));
    }
    let token = state.github_token().unwrap_or_default();
    if token.trim().is_empty() {
        return Err(AppError(anyhow::anyhow!(
            "connect GitHub to record an ignore"
        )));
    }
    let (owner, name) = req
        .repo
        .split_once('/')
        .ok_or_else(|| AppError::bad_request(anyhow::anyhow!("repo must be owner/repo")))?;

    let mut baseline = fetch_baseline(owner, name, &token).await;
    let now = chrono::Utc::now().to_rfc3339();
    for f in &req.findings {
        let fr = crate::suppression::FindingRef {
            rule_id: f.rule_id.clone(),
            path: f.path.clone(),
            line: 0,
            snippet: f.snippet.clone(),
        };
        let mut entry =
            crate::suppression::baseline_entry(&fr, "architect", &now, req.reason.trim());
        entry.kind = "ignore".to_string();
        entry.ticket = req.ticket.clone();
        baseline.entries.push(entry);
    }
    let json = serde_json::to_string_pretty(&baseline)
        .map_err(|e| AppError(anyhow::anyhow!("serialize baseline: {e}")))?;
    let url = crate::arm::arm_repo(
        owner,
        name,
        &token,
        &[(".camerata/baseline.json".to_string(), json)],
    )
    .await?;
    Ok(Json(
        serde_json::json!({ "ok": true, "url": url, "ignored": req.findings.len() }),
    ))
}

/// The central suppression registry for a project: every inline waiver + baseline
/// entry across its repos, with stale flags. The auditable "everything we've waived" view.
pub(crate) async fn project_suppressions(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let project = state
        .projects
        .get(&id)
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("project not found: {id}")))?;
    // A repo whose source was deleted has no tree to read: say so (with the date) instead of
    // silently listing nothing for it. The other repos are still read.
    let deleted: Vec<(String, String)> = project
        .repos
        .iter()
        .filter_map(|repo| {
            crate::client_source::status::deleted_record(&state, &project.id, repo)
                .map(|r| (repo.clone(), r.completed_at))
        })
        .collect();
    // Local-first: the waiver registry reads each repo's local working tree, not GitHub.
    let sources = resolve_local_sources(&state, &project.repos).sources;
    // Read-only, as a GET must be: this used to fast-forward `git pull` every repo first, which
    // moved a checkout past the inspected commit just by opening a screen (and cannot work on a
    // detached HEAD, where a re-fetch at the inspected commit leaves the tree). The Workspace
    // tab's "Clone / update all repos" is the deliberate way to refresh.
    let entries = crate::onboard::suppression_registry(&sources).await;
    Ok(Json(serde_json::json!({
        "ok": true,
        "source_deleted": !deleted.is_empty(),
        "deleted_at": deleted.first().map(|(_, at)| at.as_str()),
        "deleted_repos": deleted
            .iter()
            .map(|(repo, at)| serde_json::json!({ "repo": repo, "deleted_at": at }))
            .collect::<Vec<_>>(),
        "entries": entries,
    })))
}

/// One CI-tier rule sent by the UI for story generation. Carries the rule id and title
/// for display in the issue body, plus the linter hint (first source with one) so the
/// mechanical story can name the exact off-the-shelf tool without looking it up.
#[derive(serde::Deserialize)]
pub(crate) struct CiStoryRule {
    pub(crate) id: String,
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) linter: Option<String>,
}

#[derive(serde::Deserialize)]
pub(crate) struct CiRulesReq {
    pub(crate) repo: String,
    /// "mechanical" or "architectural" — which tier this story covers.
    pub(crate) tier: String,
    /// The rules of that tier to list in the issue body.
    #[serde(default)]
    pub(crate) rules: Vec<CiStoryRule>,
}

/// Finish onboarding for the active project. The post-scan steps (audit, triage, Apply,
/// wire-CI) are all optional, so this is the explicit "I'm done" action — and it must
/// PERSIST the onboarding result, not just flip a flag: it rebuilds the project's repos +
/// selected rules from the draft (the repos/rules otherwise only save at Apply), marks the
/// repos onboarded, and clears the draft.
pub(crate) async fn onboard_complete(State(state): State<AppState>) -> Json<serde_json::Value> {
    let Some(active) = state.projects.active() else {
        return Json(serde_json::json!({ "ok": false, "message": "no active project" }));
    };
    let pid = active.id.clone();

    // The draft is UI-owned JSON; we read only the fields we need to recover the result.
    #[derive(serde::Deserialize)]
    struct DraftRule {
        #[serde(default)]
        id: String,
        #[serde(default)]
        scope: String,
        #[serde(default)]
        default_option: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct DraftScan {
        #[serde(default)]
        repos: Vec<String>,
        /// Repos the scan could not read (`(repo, reason)`). They belong to the project all
        /// the same, so completing onboarding keeps them rather than saving the survivors.
        #[serde(default)]
        repos_not_inspected: Vec<(String, String)>,
        #[serde(default)]
        proposed_rules: Vec<DraftRule>,
    }
    #[derive(serde::Deserialize)]
    struct DraftForComplete {
        scan: DraftScan,
        #[serde(default)]
        repo_selection: std::collections::HashMap<String, Vec<String>>,
        #[serde(default)]
        custom: Vec<crate::project::CustomRule>,
    }

    let parsed = state
        .draft
        .load(&pid)
        .and_then(|v| serde_json::from_value::<DraftForComplete>(v).ok());

    let (repos, selections, cross_repo, process, custom) = if let Some(d) = parsed {
        use crate::project::RuleSelection;
        // rule_id -> the repos that selected it.
        let mut by_rule: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for (repo, ids) in &d.repo_selection {
            for id in ids {
                by_rule.entry(id.clone()).or_default().push(repo.clone());
            }
        }
        let by_id: std::collections::HashMap<&str, &DraftRule> = d
            .scan
            .proposed_rules
            .iter()
            .map(|r| (r.id.as_str(), r))
            .collect();
        let (mut selections, mut cross_repo, mut process) = (Vec::new(), Vec::new(), Vec::new());
        for (rule_id, mut repos) in by_rule {
            repos.sort();
            repos.dedup();
            let Some(pr) = by_id.get(rule_id.as_str()) else {
                continue;
            };
            let sel = RuleSelection {
                rule_id: rule_id.clone(),
                chosen_option: pr.default_option.clone(),
                repos,
                ..Default::default()
            };
            match pr.scope.as_str() {
                "cross-repo" => cross_repo.push(sel),
                "process" => process.push(sel),
                _ => selections.push(sel),
            }
        }
        let mut repos = d.scan.repos;
        for (repo, _) in d.scan.repos_not_inspected {
            if !repos.contains(&repo) {
                repos.push(repo);
            }
        }
        (repos, selections, cross_repo, process, d.custom)
    } else {
        (
            active.repos.clone(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    };

    let has_rules = !selections.is_empty() || !cross_repo.is_empty() || !process.is_empty();
    let corpus = if has_rules {
        load_corpus_for_baseline().await
    } else {
        None
    };
    state.projects.update(&pid, |p| {
        if has_rules {
            // Replace the base rules from onboarding; custom rules are preserved. Goes
            // through `upsert_base_rules` so a baseline written by an earlier Apply is
            // carried across this rebuild-from-draft instead of erased; rules new to the
            // project are baselined from the corpus.
            let known_before = selected_rule_ids(&p.ruleset);
            p.upsert_base_rules(selections.clone(), cross_repo.clone(), process.clone());
            baseline_new_to_project(&mut p.ruleset, &known_before, corpus.as_ref());
        }
        // Upsert the draft's custom rules (#49) by name (preserve any pre-existing).
        for c in &custom {
            if let Some(slot) = p.ruleset.custom.iter_mut().find(|x| x.name == c.name) {
                *slot = c.clone();
            } else {
                p.ruleset.custom.push(c.clone());
            }
        }
        // Adds the repos to the project AND marks them onboarded (union, deduped).
        p.mark_onboarded(&repos);
    });
    state.draft.clear(&pid);
    Json(serde_json::json!({ "ok": true, "onboarded": repos }))
}

// ── CI story body helpers ─────────────────────────────────────────────────────
//
// These functions are extracted from `onboard_ci_rules` so they can be unit-
// tested without spinning up a GitHub token or an HTTP server. Each produces the
// complete Markdown issue body for one tier.

/// The preamble shared by both tier stories. Explains the SSOT model up-front so any
/// developer or agent picking up either story understands how `.camerata/checks.toml`
/// drives BOTH layers before reading the tier-specific instructions.
pub(crate) fn ci_story_ssot_preamble() -> &'static str {
    "## Single Source of Truth: `.camerata/checks.toml`

The canonical place to declare a custom deterministic check is **`.camerata/checks.toml`**. \
Adding ONE entry there makes the check enforced at BOTH:

- **Layer 2** (in-loop dev gate): checks marked `in_loop = true` run in the governed dev \
  loop after each agent task. A violation bounces the work back before a PR is even opened — \
  the same way a failing `cargo clippy` would.
- **Layer 3** (generated CI workflow): the entire manifest (both `in_loop = true` AND \
  `in_loop = false`) is consumed by Camerata's workflow generator \
  (`POST /api/projects/active/generate-ci-workflow`) to produce \
  `.github/workflows/camerata-gates.yml`. CI is always the superset backstop.

**Parity is structural, not by convention.** Both the Layer-2 runner and the Layer-3 workflow \
generator call the SAME shared functions over the manifest, so they cannot drift by construction.

### Schema

```toml
# .camerata/checks.toml

[[check]]
id       = \"DEP-CRUISER-LAYERING-1\"
name     = \"dependency-cruiser layering\"
tool     = \"dependency-cruiser\"
version  = \"6.3.0\"                 # exact, not a range — L2/L3 must run the same version
install  = \"npm install -g dependency-cruiser@6.3.0\"
command  = \"depcruise --config .dependency-cruiser.cjs src\"
severity = \"high\"
in_loop  = true                    # true = also run at Layer 2; false = CI-only (needs secrets/services)
```

Field reference:

| Field      | Required | Semantics |
|------------|----------|-----------|
| `id`       | yes      | Stable rule id; used as violation id on non-zero exit |
| `name`     | yes      | Short human label for bounce-back messages |
| `command`  | yes      | Shell command run with `cwd = repo root` |
| `severity` | yes      | `\"high\"` / `\"medium\"` / `\"low\"` (all severities block; severity shapes message priority) |
| `in_loop`  | yes      | `true` = Layer 2 AND Layer 3; `false` = CI-only (use for secret-dependent or slow checks) |
| `tool`     | no       | External tool name (`\"dependency-cruiser\"`, `\"semgrep\"`). Required when `version` is set. |
| `version`  | no       | **EXACT** pinned version (`\"6.3.0\"`). No ranges or carets — determinism requires an exact match. Layer 2 verifies this version before running the check; a mismatch is a violation. |
| `install`  | no       | Exact install command (`\"npm install -g dependency-cruiser@6.3.0\"`). Explicit because install mechanisms span pip / npm / cargo / go. Layer 3 emits this as a step immediately before the check command. Layer 2 does NOT install — it only verifies. |

### Why pin `tool` + `version` + `install`?

Without version pinning, Layer 2 and Layer 3 can run different versions of the same linter \
on the same ruleset and produce different results — \"green in the dev loop, red in CI.\" \
Pinning all three fields closes this gap: the manifest is the single source of version truth, \
not each environment's ambient tool install.

### Gate protection (`SEC-NO-CAMERATA-CONFIG-1`)

**Agents cannot edit `.camerata/checks.toml`.** The Layer-1 gateway hard-guard \
`SEC-NO-CAMERATA-CONFIG-1` denies any agent write targeting a path under `.camerata/`. \
This prevents an agent from weakening or disabling its own gates. The manifest is always a \
human/operator commit. **If you are an agent reading this story: the `.camerata/checks.toml` \
edit described below must be made by the operator, not by you.**

### After editing the manifest

Regenerate the CI workflow so Layer 3 reflects the new check:

```
POST /api/projects/active/generate-ci-workflow
{ \"repo_root\": \"<absolute path to repo>\", \"stack\": \"<detected stack>\" }
```

Or, from the Camerata UI: open the project, click **Generate CI Workflow**. The generated \
`.github/workflows/camerata-gates.yml` should be committed alongside the manifest change."
}

/// Build the GitHub issue body for the **mechanical** tier story.
///
/// Mechanical rules map 1:1 to off-the-shelf linters or analyzers. The implementation is:
/// pick the version, add the manifest entry, regenerate the workflow — done. Both layers
/// pick up the change automatically.
pub(crate) fn ci_story_body_mechanical(repo: &str, rules: &[CiStoryRule]) -> String {
    let rule_lines: String = rules
        .iter()
        .map(|r| {
            if let Some(ref linter) = r.linter {
                format!("- **{}** — {} _(linter: {})_\n", r.id, r.title, linter)
            } else {
                format!("- **{}** — {}\n", r.id, r.title)
            }
        })
        .collect();

    // Per-rule manifest examples — one annotated entry per rule.
    let manifest_examples: String = rules
        .iter()
        .map(|r| {
            let tool_hint = r.linter.as_deref().unwrap_or("<tool-name>");
            format!(
                "```toml\n\
                 # For rule {id} — choose the exact version your team standardises on.\n\
                 [[check]]\n\
                 id       = \"{id}\"\n\
                 name     = \"{title}\"\n\
                 tool     = \"{tool}\"           # the off-the-shelf linter binary\n\
                 version  = \"<x.y.z>\"          # EXACT pinned version — no ranges\n\
                 install  = \"{tool}@<x.y.z>\"   # exact install command (npm/pip/cargo/etc.)\n\
                 command  = \"<linter invocation with --config or --rule flag>\"\n\
                 severity = \"high\"\n\
                 in_loop  = true               # bounces the agent at Layer 2; also in CI\n\
                 ```\n\n",
                id = r.id,
                title = r.title,
                tool = tool_hint,
            )
        })
        .collect();

    // Rule-specific cadence guidance: the dep-audit rule requires the developer
    // to choose and implement a cadence (weekly cron, per-PR, in-loop, or manual).
    // Camerata does NOT build a scheduling engine — the cadence is a project decision
    // carried here as concrete implementation guidance.
    let dep_audit_cadence_section = if rules.iter().any(|r| r.id == "CICD-DEPENDENCY-AUDIT-1") {
        "\n---\n\n\
             ## Dependency vulnerability scanning — cadence (CICD-DEPENDENCY-AUDIT-1)\n\n\
             **Camerata does not build a scheduling engine.** The cadence for dependency \
             scanning is a project decision you make on the rule, and the developer \
             implements the cadence (cron job, PR trigger, in-loop, or on-demand) when \
             wiring CI. The manifest entry above registers the tool and version; the \
             CI workflow wires the trigger.\n\n\
             ### Cadence options\n\n\
             Choose ONE of the following cadences and implement it in your CI workflow:\n\n\
             #### Option A — Weekly scheduled CI job (recommended)\n\n\
             Catches newly-disclosed CVEs that affect already-merged dependencies — the \
             class of vulnerability a per-PR scan misses entirely. Low noise, no per-PR \
             friction. The standard cadence for dependency scanning.\n\n\
             ```yaml\n\
             # .github/workflows/dep-audit.yml\n\
             on:\n\
             \x20 schedule:\n\
             \x20   - cron: '0 3 * * 1'   # every Monday at 03:00 UTC\n\
             \x20 workflow_dispatch:        # allow manual on-demand runs\n\
             ```\n\n\
             Manifest entry — set `in_loop = false` (CI-only cadence):\n\n\
             ```toml\n\
             [[check]]\n\
             id       = \"CICD-DEPENDENCY-AUDIT-1\"\n\
             name     = \"dependency vulnerability scan\"\n\
             tool     = \"osv-scanner\"\n\
             version  = \"<x.y.z>\"          # EXACT pinned version — no ranges\n\
             install  = \"curl -sSfL https://github.com/google/osv-scanner/releases/download/v<x.y.z>/osv-scanner_linux_amd64 -o osv-scanner && chmod +x osv-scanner && sudo mv osv-scanner /usr/local/bin/\"\n\
             command  = \"osv-scanner -r .\"\n\
             severity = \"high\"\n\
             in_loop  = false               # weekly CI job; not run on every agent task\n\
             ```\n\n\
             #### Option B — Per-PR / push trigger\n\n\
             Catches a vulnerable dependency the moment it lands in a PR. Higher per-PR \
             friction; appropriate for regulated environments or low-tolerance security postures.\n\n\
             ```yaml\n\
             on:\n\
             \x20 pull_request:\n\
             \x20 push:\n\
             \x20   branches: [main]\n\
             ```\n\n\
             Use the same manifest entry as Option A (`in_loop = false`).\n\n\
             #### Option C — In-loop AND CI (every-pass, highest coverage)\n\n\
             Set `in_loop = true` so osv-scanner also runs in the Camerata Layer-2 \
             in-loop dev gate after each agent task. osv-scanner is fast (lock-file \
             read, no full build), so in-loop overhead is typically a few seconds.\n\n\
             ```toml\n\
             in_loop  = true   # bounces the agent at Layer 2 too; also in CI\n\
             ```\n\n\
             Pair with any CI trigger (weekly cron, per-PR, or both).\n\n\
             #### Option D — Manual / on-demand only\n\n\
             `workflow_dispatch` only. No automated trigger. Appropriate for early-stage \
             projects or teams that fire the scan deliberately (pre-release, post-dep-bump).\n\n\
             ```yaml\n\
             on:\n\
             \x20 workflow_dispatch:\n\
             ```\n\n\
             Use `in_loop = false` in the manifest.\n\n\
             ### osv-scanner invocation\n\n\
             ```sh\n\
             # Recursive scan from repo root — reads all supported lock files.\n\
             osv-scanner --format json -r . | tee osv-results.json\n\
             osv-scanner -r . 2>&1 || exit 1\n\
             ```\n\n\
             `osv-scanner -r .` exits non-zero on any finding — which is what the \
             `.camerata/checks.toml` manifest runner and CI gate both test. Pin the \
             osv-scanner version in `tool` + `version` + `install` in the manifest \
             so Layer 2 and Layer 3 always run the same binary.\n\n\
             ### Checklist for this rule\n\n\
             - [ ] Decide the cadence (A, B, C, or D above).\n\
             - [ ] Add the `[[check]]` entry to `.camerata/checks.toml` with pinned \
             `tool`, `version`, and `install`; set `in_loop` to match the cadence.\n\
             - [ ] Commit `.camerata/checks.toml` (human/operator commit — agents \
             cannot write `.camerata/`).\n\
             - [ ] Wire the CI workflow trigger (cron/pull_request/workflow_dispatch) \
             in `.github/workflows/dep-audit.yml`.\n\
             - [ ] Regenerate `.github/workflows/camerata-gates.yml` via Camerata.\n\
             - [ ] Verify the scan fires at the chosen cadence and fails on a known \
             vulnerable lockfile (e.g. introduce a known-CVE package, confirm exit non-zero).\n"
    } else {
        ""
    };

    let preamble = ci_story_ssot_preamble();
    format!(
        "{preamble}\n\n\
         ---\n\n\
         ## This story: MECHANICAL tier\n\n\
         **This story covers the MECHANICAL tier only.** Each rule below maps to a real \
         off-the-shelf linter or analyzer. The entire implementation is:\n\n\
         1. Decide the exact tool version your team will standardise on.\n\
         2. Add the manifest entry to `.camerata/checks.toml` (see examples below).\n\
         3. Commit the manifest (operator/human commit — agents cannot write `.camerata/`).\n\
         4. Regenerate the CI workflow: both Layer 2 and Layer 3 automatically enforce \
         the check. No separate CI wiring step is needed.\n\n\
         **Repo:** `{repo}`\n\n\
         **Rules to wire:**\n\
         {rule_lines}\n\
         ---\n\n\
         ## Manifest entries to add\n\n\
         For each rule, add one `[[check]]` block to `.camerata/checks.toml`. \
         Examples (fill in the real version and invocation):\n\n\
         {manifest_examples}\
         > **Version pinning is not optional.** Without it, Layer 2 and Layer 3 can run \
         > different tool versions on the same ruleset and produce different results. \
         > Pin the exact version; use `install` so CI installs it before running the check.\
         {dep_audit_cadence_section}\n\n\
         ---\n\n\
         ## Implementation checklist\n\n\
         - [ ] For each rule: confirm the linter is already in the repo or add it as a dev dependency.\n\
         - [ ] Add the `[[check]]` entry to `.camerata/checks.toml` with pinned `tool`, `version`, \
         and `install`.\n\
         - [ ] Commit `.camerata/checks.toml` (human/operator commit).\n\
         - [ ] Regenerate `.github/workflows/camerata-gates.yml` via Camerata and commit it.\n\
         - [ ] Open a draft PR; verify CI passes with the new check step visible in the workflow run.\n\
         - [ ] Do not weaken or delete existing checks.\n\n\
         _Filed by Camerata onboarding._"
    )
}

/// Build the GitHub issue body for the **architectural** tier story.
///
/// Architectural rules are deterministic (a PR either violates them or it does not) but have
/// NO off-the-shelf linter. Each requires a bespoke checker — a script, custom Semgrep rule,
/// AST pass, or dependency-graph query — designed and scoped by the team before wiring.
///
/// The canonical worked example is API-layering enforcement via `dependency-cruiser`:
/// a JS/TS repo uses a `.dependency-cruiser.cjs` config that asserts the allowed import
/// graph; `depcruise` returns non-zero on violation.  Register it in the manifest, and
/// both Layer 2 and Layer 3 enforce it identically with no separate wiring.
pub(crate) fn ci_story_body_architectural(repo: &str, rules: &[CiStoryRule]) -> String {
    let rule_lines: String = rules
        .iter()
        .map(|r| format!("- **{}** — {}\n", r.id, r.title))
        .collect();

    let preamble = ci_story_ssot_preamble();
    format!(
        "{preamble}\n\n\
         ---\n\n\
         ## This story: ARCHITECTURAL tier\n\n\
         **This story covers the ARCHITECTURAL tier only.** Each rule below is deterministic \
         but has **no off-the-shelf linter**. Each one requires a bespoke checker — a script, \
         custom Semgrep rule, AST pass, or dependency-graph query — that the team must DESIGN \
         and SCOPE before implementation begins. **This is design work first, not a \
         configuration task.**\n\n\
         Scope each rule as a sub-task. Do not block the mechanical CI story on this work.\n\n\
         **Repo:** `{repo}`\n\n\
         **Rules that need a custom checker:**\n\
         {rule_lines}\n\
         ---\n\n\
         ## Before you build anything: `camerata-check` may already cover this\n\n\
         Camerata ships a growing set of NATIVE deterministic checkers for rules in this exact \
         tier (Rust/TS/JS/Python AST + Supabase migration-timeline replay — see \
         `docs/design/2026-07-27_ast-extractor-layer.md`). Some of the rules listed above may \
         already be answered by one of them with zero custom-checker work. Before writing a \
         bespoke script, run the `camerata-check` binary (Layer-3 parity distributable, \
         `docs/design/2026-07-26_architectural-executor-feasibility.md` §2.4) against this \
         repo:\n\n\
         ```sh\n\
         camerata-check . --format json\n\
         ```\n\n\
         - Exit `0` and no matching finding in the output → check whether the rule id is one \
         `camerata-check` doesn't own yet (`unmatched_rule_ids` in the JSON) before assuming \
         it's clean — an unmatched id still needs the bespoke-checker path below.\n\
         - A non-zero exit with the rule id present in `violations` → the native checker \
         already covers this rule; skip Steps 1-2 below and wire `camerata-check` straight into \
         CI (see the snippet at the end of this story) instead of building anything.\n\n\
         A handful of rules are DELIBERATELY not covered by any native checker and stay \
         AI-review-only by design (not an oversight) — e.g. `ARCH-STRUCTURED-ERRORS-1` (the \
         honest mechanism is a runtime contract test, not static analysis) and \
         `ARCH-EXACT-DECIMALS-1` (needs a project-specific `[decimals]` config annotation before \
         it's checkable at all). For those, the bespoke-checker path below is still correct.\n\n\
         ---\n\n\
         ## How to implement each rule (step-by-step)\n\n\
         For each rule in the list above that `camerata-check` does NOT already cover, follow \
         this process:\n\n\
         ### Step 1 — Design the deterministic checker\n\n\
         Choose a strategy that returns **exit 0 on pass, non-zero on violation**, with \
         CWD = repo root. Options (not exhaustive):\n\n\
         - **dependency-cruiser** (JS/TS layering): write a `.dependency-cruiser.cjs` config \
         that declares the allowed import graph; the checker command is \
         `depcruise --config .dependency-cruiser.cjs src`.\n\
         - **Custom Semgrep rule**: write a `.semgrep.yml` with a `pattern-not-inside` rule \
         and run `semgrep --config .semgrep.yml --error .`.\n\
         - **Shell/Python script**: traverse the AST or grep for the pattern; exit non-zero \
         when a violation is found. Place under `scripts/` and make it executable.\n\
         - **Any other deterministic static tool**: anything that can be invoked from the \
         repo root and signals pass/fail via exit code.\n\n\
         **Worked example — API layering (`ARCH-API-LAYERING-1`):**\n\n\
         The rule asserts that service modules never import from repository modules directly \
         (services orchestrate; repositories own data access). `dependency-cruiser` can \
         encode this as a forbidden dependency arc:\n\n\
         ```js\n\
         // .dependency-cruiser.cjs\n\
         module.exports = {{\n\
           forbidden: [\n\
             {{\n\
               name: 'no-service-to-repo-direct',\n\
               severity: 'error',\n\
               from: {{ path: 'src/services/' }},\n\
               to:   {{ path: 'src/repositories/' }},\n\
             }},\n\
           ],\n\
         }};\n\
         ```\n\n\
         Run `depcruise --config .dependency-cruiser.cjs src` — zero output + exit 0 means \
         clean; non-zero means a service is importing a repository directly.\n\n\
         ### Step 2 — Add the manifest entry to `.camerata/checks.toml`\n\n\
         Once the checker script/config is committed and verified locally, register it in \
         the manifest. Pin the exact tool version so Layer 2 and Layer 3 run identically:\n\n\
         ```toml\n\
         # .camerata/checks.toml\n\
         [[check]]\n\
         id       = \"ARCH-API-LAYERING-1\"       # matches the rule id in the corpus\n\
         name     = \"API layering\"\n\
         tool     = \"dependency-cruiser\"         # omit if using a repo script with no external tool\n\
         version  = \"6.3.0\"                      # EXACT pinned version — no ranges\n\
         install  = \"npm install -g dependency-cruiser@6.3.0\"\n\
         command  = \"depcruise --config .dependency-cruiser.cjs src\"\n\
         severity = \"high\"\n\
         in_loop  = true                          # bounces the agent at Layer 2 too\n\
         ```\n\n\
         For a repo-local script (no external tool required):\n\n\
         ```toml\n\
         [[check]]\n\
         id       = \"ARCH-MY-CUSTOM-RULE-1\"\n\
         name     = \"<rule description>\"\n\
         command  = \"scripts/check_my_rule.sh\"   # must be executable; cwd = repo root\n\
         severity = \"high\"\n\
         in_loop  = true\n\
         # tool / version / install omitted when no external binary is required\n\
         ```\n\n\
         If `camerata-check` already covers the rule (see the section above), register IT as \
         the tool instead of a bespoke script — no design phase needed:\n\n\
         ```toml\n\
         [[check]]\n\
         id       = \"SUPABASE-RLS-ENABLED-1\"    # any rule id camerata-check's registry answers\n\
         name     = \"Supabase RLS (native camerata-check)\"\n\
         tool     = \"camerata-check\"\n\
         version  = \"<pinned release/build>\"    # pin however you distribute the binary\n\
         command  = \"camerata-check . --rule-id SUPABASE-RLS-ENABLED-1\"\n\
         severity = \"critical\"\n\
         in_loop  = true\n\
         ```\n\n\
         > **Note:** `camerata-check` itself is a release-ops follow-up (no crates.io/binary \
         > publish pipeline exists yet) — for now, build it from source \
         > (`cargo build --release -p camerata-check`) and vendor or cache the resulting binary \
         > in your CI image; `install` above should reflect however your team distributes it.\n\n\
         > **Gate protection:** `.camerata/checks.toml` is protected by `SEC-NO-CAMERATA-CONFIG-1`. \
         > Agents cannot write to `.camerata/`. This manifest edit MUST be a human/operator \
         > commit.\n\n\
         ### Step 3 — Regenerate the CI workflow\n\n\
         After committing the manifest, regenerate `.github/workflows/camerata-gates.yml`:\n\n\
         ```\n\
         POST /api/projects/active/generate-ci-workflow\n\
         {{ \"repo_root\": \"<absolute path>\", \"stack\": \"<detected stack>\" }}\n\
         ```\n\n\
         Or use the Camerata UI: open the project, click **Generate CI Workflow**. \
         Commit the updated workflow file.\n\n\
         ### Step 4 — Verify at both layers\n\n\
         - **Layer 2**: trigger a governed dev run in Camerata and confirm the check fires \
         and bounces on a known violation.\n\
         - **Layer 3**: open a PR and confirm the check step appears in the generated \
         `.github/workflows/camerata-gates.yml` workflow run.\n\n\
         The result: one definition in `.camerata/checks.toml`, enforced identically at \
         Layer 2 and Layer 3. No separate L2 vs L3 wiring is needed.\n\n\
         ---\n\n\
         ## Implementation checklist (per rule)\n\n\
         - [ ] Agree on the checker strategy and scope it (design phase).\n\
         - [ ] Build and verify the checker in isolation (returns 0 on pass, non-zero on violation).\n\
         - [ ] Commit the checker script/config to the repo.\n\
         - [ ] Add the `[[check]]` entry to `.camerata/checks.toml` with pinned `tool` + \
         `version` + `install` (if using an external tool).\n\
         - [ ] Commit `.camerata/checks.toml` (human/operator commit — not an agent).\n\
         - [ ] Regenerate `.github/workflows/camerata-gates.yml` via Camerata and commit it.\n\
         - [ ] Verify the check fires at Layer 2 (governed dev run) and Layer 3 (CI on PR).\n\
         - [ ] Do not weaken or delete existing checks.\n\
         - [ ] Scope each rule as its own sub-task if the list is long. Do not block the \
         mechanical CI story on this work.\n\n\
         _Filed by Camerata onboarding._"
    )
}

/// Build the GitHub issue body for the **vcs-metadata** tier story.
///
/// These rules (PROCESS-CONVENTIONAL-COMMIT-1, PROCESS-COMMIT-DOC-1,
/// PROCESS-BRANCH-NAMING-1, PROCESS-ADO-LINK-1) validate VCS METADATA (commit
/// messages, branch names, PR titles) created at commit/PR time — NOT the code
/// diff. Unlike the architectural tier, they are NOT wired into
/// `.camerata/checks.toml` (which drives the layer-2 in-loop code gate): no code
/// gate ever sees a commit message. Their layer-4 enforcement is a commit/branch
/// METADATA CI check — a commitlint config plus a branch-name CI step — entirely
/// separate from the code-check manifest. Camerata's own pipeline already enforces
/// the same rules at the VCS-action gate (commit/PR time); this story extends that
/// enforcement to commits/PRs made OUTSIDE Camerata, via the repo's own CI.
pub(crate) fn ci_story_body_vcs_metadata(repo: &str, rules: &[CiStoryRule]) -> String {
    let rule_lines: String = rules
        .iter()
        .map(|r| format!("- **{}** — {}\n", r.id, r.title))
        .collect();

    format!(
        "## VCS-metadata process rules — layer-4 (CI) enforcement\n\n\
         **This story covers VCS-METADATA process rules.** These rules validate \
         the commit message, branch name, and PR title — metadata produced at \
         commit/PR time, **not** the code diff.\n\n\
         > **Why these are different from the other CI stories.** The mechanical and \
         > architectural stories register checks in `.camerata/checks.toml`, which \
         > drives BOTH the layer-2 in-loop code gate and the layer-3 CI code gate. \
         > **These VCS-metadata rules are NOT enforced at layer 2** — no code gate \
         > ever sees a commit message, branch name, or PR title. So the usual SSOT \
         > layer-2+4 parity does not apply. Their enforcement is:\n\
         > 1. **Camerata's own VCS-action gate** (already active): Camerata validates \
         >    the metadata at commit/PR time inside its pipeline. Nothing to wire for \
         >    work that goes through Camerata.\n\
         > 2. **Layer 4 (this story): a commit/branch METADATA CI check** that catches \
         >    commits/PRs made OUTSIDE Camerata — a `commitlint` config plus a \
         >    branch-name CI step. This is separate from `.camerata/checks.toml`.\n\n\
         **Repo:** `{repo}`\n\n\
         **VCS-metadata rules selected for this repo:**\n\
         {rule_lines}\n\
         > **Parameters live in Camerata Settings, not here.** The allowed \
         > conventional-commit types, branch prefixes, minimum commit-body length, \
         > and story-id / ticket format are configured per-project in Camerata \
         > (Settings → VCS gate, `process_rule_config`). Mirror those same values in \
         > the CI config below so the layer-4 check agrees with Camerata's gate.\n\n\
         ---\n\n\
         ## How to wire the layer-4 metadata check\n\n\
         ### Commit message + PR title (PROCESS-CONVENTIONAL-COMMIT-1, \
         PROCESS-COMMIT-DOC-1, PROCESS-ADO-LINK-1)\n\n\
         Use **commitlint** to validate commit messages (and, via a PR-title lint \
         step, the PR title). Add a `commitlint.config.js` whose rules mirror the \
         project's `process_rule_config`:\n\n\
         ```js\n\
         // commitlint.config.js\n\
         module.exports = {{\n\
           extends: ['@commitlint/config-conventional'],\n\
           rules: {{\n\
             // PROCESS-CONVENTIONAL-COMMIT-1: restrict to the configured types\n\
             'type-enum': [2, 'always', ['feat', 'fix', 'chore', 'docs', 'refactor', 'test', 'perf', 'build', 'ci', 'style', 'revert']],\n\
             // PROCESS-COMMIT-DOC-1: require a substantive body\n\
             'body-min-length': [2, 'always', 20],\n\
             // PROCESS-ADO-LINK-1: require a ticket reference (e.g. AB#123) — a custom\n\
             // rule or a `references-empty` / regex plugin keyed to your tracker prefix\n\
           }},\n\
         }};\n\
         ```\n\n\
         Wire it in CI on `pull_request` (lint every commit in the range, and the PR title):\n\n\
         ```yaml\n\
         # .github/workflows/commit-metadata.yml\n\
         name: commit-metadata\n\
         on: [pull_request]\n\
         jobs:\n\
           commitlint:\n\
             runs-on: ubuntu-latest\n\
             steps:\n\
               - uses: actions/checkout@v4\n\
                 with: {{ fetch-depth: 0 }}\n\
               - uses: actions/setup-node@v4\n\
                 with: {{ node-version: '20' }}\n\
               - run: npm i -D @commitlint/cli @commitlint/config-conventional\n\
               - run: npx commitlint --from \"${{{{ github.event.pull_request.base.sha }}}}\" --to \"${{{{ github.event.pull_request.head.sha }}}}\"\n\
         ```\n\n\
         ### Branch name (PROCESS-BRANCH-NAMING-1)\n\n\
         No code tool sees the branch name; add a small CI step that inspects the ref \
         and fails on a name without an allowed prefix (mirror the prefixes from \
         `process_rule_config`):\n\n\
         ```yaml\n\
           branch-name:\n\
             runs-on: ubuntu-latest\n\
             steps:\n\
               - name: Check branch prefix\n\
                 run: |\n\
                   branch=\"${{{{ github.head_ref }}}}\"\n\
                   case \"$branch\" in\n\
                     feature/*|release/*|hotfix/*) echo \"ok: $branch\" ;;\n\
                     *) echo \"::error::branch '$branch' must start with feature/, release/, or hotfix/\"; exit 1 ;;\n\
                   esac\n\
         ```\n\n\
         ---\n\n\
         ## Implementation checklist\n\n\
         - [ ] Read the project's `process_rule_config` (Camerata → Settings → VCS gate) \
         and copy the active values (types, prefixes, min body length, ticket/story-id format).\n\
         - [ ] Add `commitlint.config.js` mirroring those values (only for the selected rules).\n\
         - [ ] Add the `commit-metadata` workflow (commitlint over the PR commit range + PR title).\n\
         - [ ] Add the branch-name CI step mirroring the configured prefixes (only if \
         PROCESS-BRANCH-NAMING-1 is selected).\n\
         - [ ] Do NOT add these to `.camerata/checks.toml` — they are NOT layer-2 code checks.\n\
         - [ ] Confirm Camerata's own VCS-action gate is already enforcing the same rules \
         in-pipeline (no wiring needed for work that goes through Camerata).\n\n\
         _Filed by Camerata onboarding._"
    )
}

/// Emit a tier-specific "wire CI rules" story as a GitHub issue.
///
/// Three tiers are supported:
/// - "mechanical"   — rules that map 1:1 to an off-the-shelf linter/analyzer. Wiring is
///   straightforward: add a manifest entry to `.camerata/checks.toml` (the SSOT) and both
///   Layer 2 (in-loop) and Layer 3 (CI) automatically enforce it.
/// - "architectural" — rules that are also deterministic but require a bespoke AST or static-
///   analysis checker the team must DESIGN before implementing. This story should be scoped
///   and refined first; it should NOT ride with the mechanical story.
/// - "vcs-metadata" — process rules that validate commit/branch/PR METADATA (not the code
///   diff). NOT layer-2 enforceable and NOT registered in `.camerata/checks.toml`; their
///   layer-4 enforcement is a commitlint config + a branch-name CI step. Camerata's own
///   VCS-action gate already enforces them in-pipeline.
///
/// The UI files each story separately so the tracks land as distinct GitHub issues.
/// Each story carries the HOW-TO so a developer or AI agent can implement the check
/// correctly without additional hand-holding.
pub(crate) async fn onboard_ci_rules(
    State(state): State<AppState>,
    Json(req): Json<CiRulesReq>,
) -> Json<serde_json::Value> {
    let Some((owner, repo)) = req.repo.split_once('/') else {
        return Json(serde_json::json!({ "ok": false, "message": "repo must be owner/repo" }));
    };
    if req.tier != "mechanical" && req.tier != "architectural" && req.tier != "vcs-metadata" {
        return Json(serde_json::json!({
            "ok": false,
            "message": format!("unknown tier '{}': must be 'mechanical', 'architectural', or 'vcs-metadata'", req.tier)
        }));
    }
    if req.rules.is_empty() {
        return Json(serde_json::json!({
            "ok": false,
            "message": format!("no {} rules to wire", req.tier)
        }));
    }
    let token = state.github_token();
    let Some(token) = token else {
        return Json(
            serde_json::json!({ "ok": false, "message": "Connect GitHub to create the story issue." }),
        );
    };

    let (title, body) = match req.tier.as_str() {
        "mechanical" => {
            let t = format!(
                "Wire mechanical (off-the-shelf linter) rules into CI — {}",
                req.repo
            );
            let b = ci_story_body_mechanical(repo, &req.rules);
            (t, b)
        }
        "vcs-metadata" => {
            let t = format!(
                "Wire VCS-metadata process rules into CI (commit/branch metadata check) — {}",
                req.repo
            );
            let b = ci_story_body_vcs_metadata(repo, &req.rules);
            (t, b)
        }
        _ => {
            // architectural
            let t = format!(
                "Wire architectural (custom-checker) rules into CI — {}",
                req.repo
            );
            let b = ci_story_body_architectural(repo, &req.rules);
            (t, b)
        }
    };

    match crate::onboard::create_issue(owner, repo, &token, &title, &body).await {
        Ok(url) => Json(serde_json::json!({ "ok": true, "url": url })),
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

// ── CI workflow generator endpoint ───────────────────────────────────────────

/// Request body for the CI workflow generation endpoint.
///
/// The caller supplies the active project's repo root path and the detected
/// language stack. The server loads `.camerata/checks.toml` from the root and
/// generates the complete `.github/workflows/camerata-gates.yml` YAML.
#[derive(serde::Deserialize)]
pub(crate) struct GenerateCiWorkflowReq {
    /// Absolute path to the repo/worktree root on the server's filesystem.
    pub(crate) repo_root: String,
    /// The detected language stack (defaults to Rust when omitted).
    #[serde(default)]
    pub(crate) stack: crate::workflow_gen::RepoStack,
}

/// `POST /api/projects/active/generate-ci-workflow`
///
/// Load `.camerata/checks.toml` from `req.repo_root`, generate the
/// `.github/workflows/camerata-gates.yml` YAML for the given stack, write it to
/// disk under `repo_root`, and return the YAML body.
///
/// A missing manifest is NOT an error — it generates a workflow with only the
/// built-in language steps. The caller can store the YAML in the repo.
pub(crate) async fn generate_ci_workflow(
    Json(req): Json<GenerateCiWorkflowReq>,
) -> axum::response::Response {
    use crate::workflow_gen::generate_gates_workflow;
    use axum::response::IntoResponse;
    use camerata_checks::manifest::load_manifest;
    use std::path::Path;

    let root = Path::new(&req.repo_root);
    // Only ever write into an existing git checkout. Without this the route created
    // `<repo_root>/.github/workflows/` (and every missing parent) for ANY path, which brought
    // back the folder of a repo whose source was deleted on purpose and then blocked re-fetch.
    if !crate::workspace::is_git_repo(root) {
        return crate::client_source::Refusal::conflict(
            "not_a_git_repo",
            format!(
                "{} is not a git checkout, so no workflow was written.",
                root.display()
            ),
        )
        .into_response();
    }

    // Load manifest (best-effort: absent/invalid → empty).
    let manifest = match load_manifest(root) {
        Ok(Some(m)) => m,
        Ok(None) => camerata_checks::CheckManifest::default(),
        Err(e) => {
            eprintln!(
                "[camerata-server] generate-ci-workflow: manifest parse error: {}; \
                 generating without manifest checks",
                e
            );
            camerata_checks::CheckManifest::default()
        }
    };

    let yaml = generate_gates_workflow(&req.stack, &manifest);

    // Write to <repo_root>/.github/workflows/camerata-gates.yml.
    let workflow_path = root
        .join(".github")
        .join("workflows")
        .join("camerata-gates.yml");

    if let Some(parent) = workflow_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Json(serde_json::json!({
                "ok": false,
                "message": format!("failed to create workflow dir: {e}")
            }))
            .into_response();
        }
    }

    if let Err(e) = std::fs::write(&workflow_path, &yaml) {
        return Json(serde_json::json!({
            "ok": false,
            "message": format!("failed to write workflow file: {e}")
        }))
        .into_response();
    }

    Json(serde_json::json!({
        "ok": true,
        "path": workflow_path.display().to_string(),
        "yaml": yaml
    }))
    .into_response()
}

// ── Greenfield scaffold handler ───────────────────────────────────────────────

/// Request body for the greenfield scaffold endpoint.
///
/// The caller supplies the new repo's name (used as the commit label), the local
/// path where the repo should be created, the resolved rules to bake in, and any
/// custom rules from the active project. All fields are validated server-side
/// before the blocking scaffold runs.
#[derive(serde::Deserialize)]
pub(crate) struct GreenfieldReq {
    /// Human name / label for the new repo (used in the initial commit message).
    /// Does NOT need to be an `owner/repo` — it can be a bare project name until
    /// the user connects the remote. Required.
    pub(crate) name: String,
    /// Absolute path on disk where the new repo directory should be created.
    /// The directory MUST NOT already exist; scaffold creates it. Required.
    pub(crate) path: String,
    /// The resolved rules to install (same shape as `ArmReq.rules`). Optional —
    /// an empty list scaffolds just the git repo + an empty gate config.
    #[serde(default)]
    pub(crate) rules: Vec<crate::arm::ArmRule>,
    /// Custom rules from the active project to carry into the emit. Optional.
    #[serde(default)]
    pub(crate) custom: Vec<crate::project::CustomRule>,
}

/// Greenfield onboarding: scaffold a NEW local git repo with governance baked in
/// from commit zero. This is the counterpart to the brownfield apply flow: instead
/// of writing governance files into an EXISTING repo, it CREATES the repo directory,
/// emits the governance files (AGENTS.md / CONVENTIONS.md / .camerata/rules.json /
/// CI workflow) using the same `arm_files_for_repo` primitive as apply, and commits
/// them as the initial commit.
///
/// The handler does NOT push to GitHub — the new repo is local-only until the
/// architect connects it to a remote. It marks the scaffolded repo as onboarded in
/// the active project (creating one from the name if none is active).
///
/// The scaffold is a blocking operation (filesystem + git) and is run via
/// `tokio::task::spawn_blocking` to avoid blocking the async runtime.
pub(crate) async fn onboard_greenfield(
    State(state): State<AppState>,
    Json(req): Json<GreenfieldReq>,
) -> Json<serde_json::Value> {
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Json(serde_json::json!({ "ok": false, "message": "Name the new repo." }));
    }
    let path_str = req.path.trim().to_string();
    if path_str.is_empty() {
        return Json(
            serde_json::json!({ "ok": false, "message": "Choose a directory for the new repo." }),
        );
    }
    let dest = std::path::PathBuf::from(&path_str);

    // Resolve the repo-local rules (drop cross-repo / process: they're project-level,
    // not written into any single repo's files). Clone the vecs so they are 'static
    // and can be moved into the spawn_blocking closure.
    let repo_local: Vec<crate::arm::ArmRule> = req
        .rules
        .into_iter()
        .filter(|r| r.scope != "cross-repo" && r.scope != "process")
        .collect();
    let custom: Vec<crate::project::CustomRule> = req.custom;
    let name_clone = name.clone();

    // Run the blocking scaffold (filesystem + git) off the async runtime.
    // Move owned vecs (not refs) into the closure to satisfy the 'static bound.
    let result = tokio::task::spawn_blocking(move || {
        let rule_refs: Vec<&crate::arm::ArmRule> = repo_local.iter().collect();
        let custom_refs: Vec<&crate::project::CustomRule> = custom.iter().collect();
        crate::onboard::scaffold_greenfield_blocking(&dest, &rule_refs, &custom_refs, &name_clone)
    })
    .await;

    let scaffold = match result {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            return Json(serde_json::json!({ "ok": false, "message": format!("{e}") }));
        }
        Err(e) => {
            return Json(
                serde_json::json!({ "ok": false, "message": format!("scaffold task: {e}") }),
            );
        }
    };

    // Mark the new repo as onboarded in the active project (creating one if needed
    // so the name appears in the repos list and the onboarded badge lights up).
    let repo_label = name.clone();
    let pid = match state.projects.active() {
        Some(p) => p.id,
        None => match state.projects.create(&repo_label, vec![repo_label.clone()]) {
            Some(p) => p.id,
            None => {
                // Project store unavailable — continue anyway; the scaffold itself succeeded.
                return Json(serde_json::json!({
                    "ok": true,
                    "path": scaffold.path,
                    "files_written": scaffold.files_written,
                    "commit_sha": scaffold.commit_sha,
                    "message": scaffold.message,
                }));
            }
        },
    };
    state.projects.update(&pid, |p| {
        p.mark_onboarded(std::slice::from_ref(&repo_label));
    });

    // Record the scaffolded dir as the repo's local path override so subsequent
    // scan/apply operations resolve it correctly without a workspace root.
    let path_clone = scaffold.path.clone();
    state.settings.set_repo_path(&repo_label, Some(path_clone));

    Json(serde_json::json!({
        "ok": true,
        "path": scaffold.path,
        "files_written": scaffold.files_written,
        "commit_sha": scaffold.commit_sha,
        "message": scaffold.message,
    }))
}

/// Classify the armed rules by scope and save them to the active project (creating
/// one from the rules' repos if none exists). This is the upsert: it replaces the
/// project's BASE rules (selections / cross-repo / process) and leaves custom rules
/// untouched.
///
/// This is an APPLY: the rules are about to be emitted at their current corpus text, so each
/// selection is (re)baselined against `corpus` ([`rebaseline_selections`]) and drift detection
/// covers it from this moment. Without a corpus (or for a rule it lacks) nothing is invented,
/// and `upsert_base_rules` carries the project's existing baseline across the replace.
pub(crate) fn save_armed_to_project(
    state: &AppState,
    rules: &[crate::arm::ArmRule],
    custom: &[crate::project::CustomRule],
    corpus: Option<&camerata_rules::RuleSet>,
) {
    use crate::project::RuleSelection;
    // First pass: gather the REAL repos this arm touches, skipping the UI-internal
    // single-repo sentinel (a NUL-prefixed map key that is never a real `owner/repo`).
    // This set seeds the project's repo list and, below, resolves any leaked sentinel in a
    // selection's repos — persisting the sentinel would break clone/emit forever
    // (PUBLISH-2).
    let mut all_repos = std::collections::BTreeSet::new();
    for r in rules {
        for repo in &r.repos {
            if !repo.contains('\0') {
                all_repos.insert(repo.clone());
            }
        }
    }
    // Repo-scoped custom rules pull their repo into the project too (covers a custom-only apply).
    for c in custom {
        let d = c.domain.trim();
        if !d.is_empty() && d != "*" && !d.contains('\0') {
            all_repos.insert(d.to_string());
        }
    }
    let all_repos_vec: Vec<String> = all_repos.iter().cloned().collect();
    let mut selections = Vec::new();
    let mut cross = Vec::new();
    let mut process = Vec::new();
    for r in rules {
        let s = RuleSelection {
            rule_id: r.id.clone(),
            chosen_option: r.option.clone(),
            // Normalize before persisting so a leaked sentinel expands to the real repos
            // instead of being written into the ruleset.
            repos: normalize_repos(&r.repos, &all_repos_vec),
            ..Default::default()
        };
        match r.scope.as_str() {
            "cross-repo" => cross.push(s),
            "process" => process.push(s),
            _ => selections.push(s),
        }
    }
    rebaseline_selections(&mut selections, corpus);
    rebaseline_selections(&mut cross, corpus);
    rebaseline_selections(&mut process, corpus);
    let pid = match state.projects.active() {
        Some(p) => p.id,
        None => match state
            .projects
            .create("My project", all_repos.iter().cloned().collect())
        {
            Some(p) => p.id,
            None => return,
        },
    };
    state.projects.update(&pid, |p| {
        p.upsert_base_rules(selections, cross, process);
        // Upsert custom rules by name (preserve any not in this apply).
        for c in custom {
            if let Some(slot) = p.ruleset.custom.iter_mut().find(|x| x.name == c.name) {
                *slot = c.clone();
            } else {
                p.ruleset.custom.push(c.clone());
            }
        }
        for repo in &all_repos {
            if !p.repos.contains(repo) {
                p.repos.push(repo.clone());
            }
        }
    });
}

/// Emit the repo-local `rules` + the `custom` rules into each repo in `repos`, one
/// governance PR per repo. Shared by initial arm and re-emit. Each repo receives the
/// rules bound to it plus the domain-matching custom rules.
pub(crate) async fn emit_to_repos(
    repos: &[String],
    rules: &[crate::arm::ArmRule],
    custom: &[crate::project::CustomRule],
    baselines: &std::collections::HashMap<String, String>,
    token: &str,
) -> Vec<serde_json::Value> {
    let mut results = Vec::new();
    for repo in repos {
        let Some((owner, name)) = repo.split_once('/') else {
            results.push(
                serde_json::json!({ "repo": repo, "ok": false, "message": "not owner/repo" }),
            );
            continue;
        };
        let repo_rules: Vec<&crate::arm::ArmRule> = rules
            .iter()
            .filter(|r| r.repos.iter().any(|x| x == repo))
            .collect();
        let repo_custom: Vec<&crate::project::CustomRule> =
            custom.iter().filter(|c| c.applies_to_repo(repo)).collect();
        if repo_rules.is_empty() && repo_custom.is_empty() {
            continue;
        }
        let mut files = crate::arm::arm_files_for_repo(&repo_rules, &repo_custom);
        // Include this repo's baseline (accepted pre-existing debt) in the same PR, so
        // the gate it installs enforces only on new code from day one.
        if let Some(baseline_json) = baselines.get(repo) {
            files.push((".camerata/baseline.json".to_string(), baseline_json.clone()));
        }
        match crate::arm::arm_repo(owner, name, token, &files).await {
            Ok(url) => results.push(serde_json::json!({ "repo": repo, "ok": true, "url": url })),
            Err(e) => results
                .push(serde_json::json!({ "repo": repo, "ok": false, "message": format!("{e}") })),
        }
    }
    results
}

/// Defensively normalize a rule's `repos` list against the project's actual repos.
///
/// The UI keys a SINGLE-repo scan's selection under an internal sentinel
/// (`"\u{0}__single_repo__"`, deliberately NUL-prefixed so it can never collide with a
/// real `owner/repo`). That sentinel is a UI-internal MAP KEY, never a real repo — but a
/// bug let it leak into a selection's persisted `repos`, from where it reached the emit /
/// clone path and blew up with `git clone "\0__single_repo__"` ("nul byte found in
/// provided data").
///
/// Any entry that contains a NUL byte is the leaked sentinel: replace it with the
/// project's actual repos (`project_repos`). The sentinel only ever meant "the
/// single/default repo", so expanding it to ALL of the project's repos is the safe
/// fallback (single-repo project → the one repo; multi-repo → all of them). Real
/// `owner/repo` entries pass through untouched. The result is deduped (order-preserving)
/// so a sentinel + a real duplicate can't double-emit.
pub(crate) fn normalize_repos(repos: &[String], project_repos: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(repos.len());
    for repo in repos {
        if repo.contains('\0') {
            // Leaked internal sentinel — never a real repo. Expand to the project's repos.
            for real in project_repos {
                if !real.contains('\0') && !out.contains(real) {
                    out.push(real.clone());
                }
            }
        } else if !out.contains(repo) {
            out.push(repo.clone());
        }
    }
    out
}

/// Resolve a project's base selections into emittable rules: the directive comes
/// from the corpus rule's chosen alternative (or default), or the gateway content
/// rule's description. The rule-bank is the source; the project stores only the
/// selection (id + chosen option + repos).
pub(crate) async fn resolve_project_arm_rules(
    project: &crate::project::Project,
) -> Vec<crate::arm::ArmRule> {
    let mut out = resolve_project_selection_rules(project).await;
    // Drop any rule whose repos normalized to empty (a leaked sentinel on a project with
    // no real repos): it has nowhere to emit and would only be dead weight downstream.
    out.retain(|r| !r.repos.is_empty());
    out
}

/// Every base selection of the project resolved to a rule (directive, enforcement, placement),
/// INCLUDING project-level selections whose placement is empty. [`resolve_project_arm_rules`] is
/// this minus the rules with nowhere to emit; a SCAN, which audits project-level rules too, uses
/// this directly.
pub(crate) async fn resolve_project_selection_rules(
    project: &crate::project::Project,
) -> Vec<crate::arm::ArmRule> {
    let corpus_path = camerata_rules::corpus_path();
    let set = if corpus_path.exists() {
        Some(camerata_rules::load_corpus_lenient(&corpus_path).await.0)
    } else {
        None
    };
    let mut out = Vec::new();
    for sel in &project.ruleset.selections {
        if let Some(rule) = set.as_ref().and_then(|s| s.get_by_id(&sel.rule_id)) {
            let directive = sel
                .chosen_option
                .as_ref()
                .and_then(|oid| rule.options.iter().find(|o| &o.id == oid))
                .or_else(|| {
                    rule.default_option
                        .as_ref()
                        .and_then(|d| rule.options.iter().find(|o| &o.id == d))
                })
                .map(|o| o.directive.clone())
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| rule.summary.clone());
            let enforcement = rule.enforcement.as_str();
            out.push(crate::arm::ArmRule {
                id: sel.rule_id.clone(),
                title: rule.title.clone(),
                directive,
                option: sel.chosen_option.clone(),
                enforcement: enforcement.to_string(),
                scope: "repo-local".to_string(),
                conformance: None,
                repos: normalize_repos(&sel.repos, &project.repos),
            });
            continue;
        }
        // A gateway content rule (its description is the directive), else a minimal
        // emit using the id (drift — applied but no rich source).
        let (title, directive) = camerata_gateway::RULE_REGISTRY
            .iter()
            .find(|e| e.id == sel.rule_id)
            .map(|e| (e.description.to_string(), e.description.to_string()))
            .unwrap_or_else(|| (sel.rule_id.clone(), sel.rule_id.clone()));
        out.push(crate::arm::ArmRule {
            id: sel.rule_id.clone(),
            title,
            directive,
            option: sel.chosen_option.clone(),
            enforcement: "mechanical".to_string(),
            scope: "repo-local".to_string(),
            conformance: None,
            repos: normalize_repos(&sel.repos, &project.repos),
        });
    }
    out
}

/// Re-emit a project's ruleset (the source of truth) into its repos: rebuild the
/// emit from the project's base selections + custom rules and open a PR per repo.
/// Gated on the token. This is "save -> re-emit": editing the ruleset and emitting
/// produces one updated source-of-truth emit, custom rules preserved.
pub(crate) async fn emit_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let Some(project) = state.projects.get(&id) else {
        return Json(serde_json::json!({ "ok": false, "message": "no such project" }));
    };
    let token = state.github_token();
    let Some(token) = token else {
        return Json(serde_json::json!({ "ok": false, "message": "Connect GitHub to emit." }));
    };
    let rules = resolve_project_arm_rules(&project).await;
    if rules.is_empty() && project.ruleset.custom.is_empty() {
        return Json(
            serde_json::json!({ "ok": false, "message": "Nothing to emit — this project has no repo-local rules or custom rules yet." }),
        );
    }
    // Derive the emit repo set as the UNION of the project's repos and the repos its
    // repo-local rules actually target. Iterating only `project.repos` silently omits a
    // selection scoped to a repo later removed from the project (PUBLISH-7). Warn on those
    // out-of-project targets so the drift is visible rather than silent.
    let project_set: std::collections::BTreeSet<&str> =
        project.repos.iter().map(String::as_str).collect();
    let mut repo_set: std::collections::BTreeSet<String> = project.repos.iter().cloned().collect();
    let mut warnings: Vec<String> = Vec::new();
    for r in &rules {
        if r.scope == "cross-repo" || r.scope == "process" {
            continue;
        }
        for repo in &r.repos {
            if !project_set.contains(repo.as_str()) {
                warnings.push(format!(
                    "rule `{}` is scoped to `{repo}`, which is not in the project's repos; emitting anyway",
                    r.id
                ));
            }
            repo_set.insert(repo.clone());
        }
    }
    let repos: Vec<String> = repo_set.into_iter().collect();
    // Re-emit carries no new baseline (it's a ruleset refresh, not onboarding).
    let no_baselines = std::collections::HashMap::new();
    let results = emit_to_repos(
        &repos,
        &rules,
        &project.ruleset.custom,
        &no_baselines,
        &token,
    )
    .await;
    Json(serde_json::json!({ "ok": true, "results": results, "warnings": warnings }))
}

/// Re-emit a project's ruleset directly into the LOCAL working copies of its repos,
/// unconditionally — regardless of onboarding state. Rebuilds AGENTS.md +
/// CONVENTIONS.md + the gate configs from the project's CURRENT principle sources and
/// ALWAYS writes them to disk. Does NOT open a PR and does NOT require the repo to be
/// already onboarded. This is the "re-sync my local clones" action for an architect who
/// edited the ruleset after onboarding and wants the local files to reflect the current
/// selections without going through a full re-onboard.
///
/// Needs a workspace folder set (or per-repo path overrides via repo health) — Apply
/// writes into the repo's local clone. The token is required so the branch can also be
/// pushed to origin (keeping the remote in sync with the local write).
pub(crate) async fn emit_project_local(
    State(state): State<AppState>,
    Path(id): Path<String>,
    // Tolerant of a missing/empty/malformed body: it defaults to all-false (write locally only),
    // the safe baseline. The UI always sends the explicit toggles.
    req: Option<Json<EmitLocalReq>>,
) -> Json<serde_json::Value> {
    let req = req.map(|Json(r)| r).unwrap_or_default();
    // Enforce the cascade defensively (the UI also gates it): a PR needs a push, a push needs a
    // branch. All-false = write the governance files into the working copy only.
    let (want_branch, want_push, want_pr) =
        normalize_emit_cascade(req.branch, req.push, req.open_pr);

    let Some(project) = state.projects.get(&id) else {
        return Json(serde_json::json!({ "ok": false, "message": "no such project" }));
    };
    let token = state.github_token().unwrap_or_default();
    if want_push && token.is_empty() {
        return Json(
            serde_json::json!({ "ok": false, "message": "A GitHub token is required to push or open a PR (set CAMERATA_GITHUB_TOKEN). Emit locally without push, or connect GitHub." }),
        );
    }

    let rules = resolve_project_arm_rules(&project).await;
    if rules.is_empty() && project.ruleset.custom.is_empty() {
        return Json(
            serde_json::json!({ "ok": false, "message": "Nothing to emit — this project has no repo-local rules or custom rules yet." }),
        );
    }

    // Re-emit carries no new baseline (it's a ruleset refresh, not onboarding).
    let no_baselines: std::collections::HashMap<String, String> = Default::default();
    let workspace_root = state.settings.workspace_root();

    // Filter to repo-local rules only (cross-repo / process rules are project-level).
    let repo_local: Vec<crate::arm::ArmRule> = rules
        .iter()
        .filter(|r| r.scope != "cross-repo" && r.scope != "process")
        .cloned()
        .collect();
    let mut repos: Vec<String> = repo_local.iter().flat_map(|r| r.repos.clone()).collect();
    repos.sort();
    repos.dedup();

    let has_any_local =
        workspace_root.is_some() || repos.iter().any(|r| state.settings.repo_path(r).is_some());
    if !has_any_local {
        return Json(
            serde_json::json!({ "ok": false, "message": "Set a local workspace folder (Settings) or choose each repo's local folder (repo health) — emit-local writes into the repo's local clone." }),
        );
    }

    let custom = project.ruleset.custom.clone();
    let mut results = Vec::new();
    for repo in &repos {
        let repo_rules: Vec<&crate::arm::ArmRule> = repo_local
            .iter()
            .filter(|r| r.repos.iter().any(|x| x == repo))
            .collect();
        let repo_custom: Vec<&crate::project::CustomRule> =
            custom.iter().filter(|c| c.applies_to_repo(repo)).collect();
        if repo_rules.is_empty() && repo_custom.is_empty() {
            continue;
        }
        // Build the governance files unconditionally — no onboarding-state gate.
        let mut files = crate::arm::arm_files_for_repo(&repo_rules, &repo_custom);
        if let Some(baseline_json) = no_baselines.get(repo) {
            files.push((".camerata/baseline.json".to_string(), baseline_json.clone()));
        }
        // Resolve this repo's local dir: per-repo override wins, else <workspace_root>/<repo>.
        let override_path = state.settings.repo_path(repo);
        let Some(dir) = crate::workspace::resolve_repo_dir(
            override_path.as_deref(),
            workspace_root.as_deref(),
            repo,
        ) else {
            results.push(serde_json::json!({
                "repo": repo, "ok": false,
                "message": "No local path — choose this repo's folder (repo health) or set a workspace folder."
            }));
            continue;
        };
        let clone_root = match crate::client_source::resolution::clone_root_for(
            &state,
            repo,
            override_path.as_deref(),
            workspace_root.as_deref(),
        ) {
            Ok(root) => root,
            Err(refusal) => {
                results.push(serde_json::json!({
                    "repo": repo, "ok": false, "code": refusal.code, "message": refusal.message
                }));
                continue;
            }
        };
        let msg = format!("chore(governance): re-emit Camerata ruleset to {repo}");
        match crate::workspace::apply_local(
            &dir,
            repo,
            clone_root,
            crate::arm::ARM_BRANCH,
            &files,
            &msg,
            &token,
            want_branch,
            want_push,
        )
        .await
        {
            Ok(path) => {
                let mut entry = serde_json::json!({ "repo": repo, "ok": true, "path": path });
                if want_branch {
                    entry["branch"] = crate::arm::ARM_BRANCH.into();
                }
                if want_pr {
                    let title = format!("chore(governance): Camerata ruleset for {repo}");
                    let body = "Camerata-managed governance files (AGENTS.md / CONVENTIONS.md / .camerata gate config), regenerated from the project ruleset.";
                    // GAP-2 chokepoint: gate the governance PR's metadata (auditable bypass —
                    // machine-generated title/body). A HARD-BLOCK here would strand the
                    // already-emitted + pushed branch, so we record a bypass rather than block.
                    let vcs_config = process_rule_config_for(&state, &active_project_id(&state));
                    if let Ok(Some(record)) = crate::vcs_choke::gated_pr_or_bypass(
                        &vcs_config,
                        &title,
                        body,
                        Some("orchestration-internal governance PR; title/body are machine-generated"),
                    ) {
                        entry["vcs_gate"] = record.into();
                    }
                    match crate::workspace::open_branch_pr(
                        repo,
                        crate::arm::ARM_BRANCH,
                        &title,
                        body,
                        &token,
                    )
                    .await
                    {
                        Ok(url) => {
                            entry["pr_url"] = url.into();
                        }
                        Err(e) => {
                            entry["ok"] = serde_json::Value::Bool(false);
                            entry["message"] =
                                format!("emitted + pushed, but opening the PR failed: {e}").into();
                        }
                    }
                }
                results.push(entry);
            }
            Err(e) => results.push(serde_json::json!({
                "repo": repo, "ok": false, "message": format!("{e}")
            })),
        }
    }
    Json(serde_json::json!({ "ok": true, "results": results }))
}

/// The escalation toggles for [`emit_project_local`], cascading (each requires the previous):
/// `branch` (commit onto the managed branch) → `push` (push it to origin) → `open_pr`. All
/// false = write the governance files into the working copy only (the default local emit).
#[derive(serde::Deserialize, Default)]
pub(crate) struct EmitLocalReq {
    #[serde(default)]
    pub(crate) branch: bool,
    #[serde(default)]
    pub(crate) push: bool,
    #[serde(default)]
    pub(crate) open_pr: bool,
}

/// Normalize the emit cascade so each level implies the ones beneath it: opening a PR requires
/// a push, and a push requires a branch. Returns `(want_branch, want_push, want_pr)`. This makes
/// a malformed request (e.g. `push=true` with `branch=false`) safe rather than pushing the
/// current branch.
pub(crate) fn normalize_emit_cascade(
    branch: bool,
    push: bool,
    open_pr: bool,
) -> (bool, bool, bool) {
    let want_pr = open_pr;
    let want_push = push || want_pr;
    let want_branch = branch || want_push;
    (want_branch, want_push, want_pr)
}

/// Query for draining the notification feed: only items newer than `since`.
#[derive(serde::Deserialize)]
pub(crate) struct NotifyQuery {
    #[serde(default)]
    pub(crate) since: u64,
}

/// The notification feed the UI polls (env-configurable cadence) and turns into
/// toasts. Returns items with id > `since` plus the new cursor to send next time.
pub(crate) async fn notifications_feed(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<NotifyQuery>,
) -> Json<serde_json::Value> {
    let (items, cursor) = state.notifications.since(q.since);
    Json(serde_json::json!({ "notifications": items, "cursor": cursor }))
}

/// Request to adopt an external work item by its tracker id.
#[derive(serde::Deserialize)]
pub(crate) struct AdoptReq {
    pub(crate) external_id: String,
    /// Optional source container coordinate (GitHub `owner/repo`, Jira/ADO
    /// project). Lets the BFF adopt from any repo the connection can reach, not
    /// just a default. Omitted falls back to the provider's default container.
    #[serde(default)]
    pub(crate) container: Option<String>,
}

/// Adopt a story from the active tracker into the spine: ingest the work item by id
/// via the provider and upsert it into the `StoryStore`. With the native provider this
/// only succeeds for a seeded id; with GitHub configured it pulls a real issue.
pub(crate) async fn adopt_story(
    State(state): State<AppState>,
    Json(req): Json<AdoptReq>,
) -> Result<Json<CanonicalStory>, AppError> {
    let reference = ExternalRef {
        provider: state.provider.current().provider.kind(),
        external_id: req.external_id.clone(),
        container: req.container.clone(),
        url: String::new(),
        revision: None,
    };
    let story = state
        .provider
        .current()
        .provider
        .ingest_story(&reference)
        .await
        .map_err(AppError)?;
    state
        .stories
        .upsert(story.clone())
        .await
        .map_err(AppError)?;
    Ok(Json(story))
}

// ── GitHub Issue intake (#20) ─────────────────────────────────────────────────

/// Query for `GET /api/github/issues` — the `owner/repo` whose open issues to list.
#[derive(serde::Deserialize)]
pub(crate) struct GithubIssuesQuery {
    pub(crate) repo: String,
}

/// List a repo's OPEN GitHub issues for the adopt picker. Gated on
/// `CAMERATA_GITHUB_TOKEN`: with no token (or an unreachable API / bad repo) this
/// returns `{ ok: false, issues: [], message }` with an empty list — it NEVER
/// errors out or panics, so the UI degrades to a "Connect GitHub" hint. The token
/// is never echoed back. Pull requests are filtered out by the parser.
pub(crate) async fn github_issues_list(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<GithubIssuesQuery>,
) -> Json<serde_json::Value> {
    let repo = q.repo.trim();
    if repo.is_empty() {
        return Json(serde_json::json!({
            "ok": false,
            "issues": [],
            "message": "Provide a repo as `owner/name`.",
        }));
    }
    let token = state.github_token();
    let Some(token) = token else {
        return Json(serde_json::json!({
            "ok": false,
            "issues": [],
            "message": "Connect GitHub to list issues.",
        }));
    };
    match crate::github_issues::list_open_issues(repo, &token).await {
        Ok(issues) => Json(serde_json::json!({ "ok": true, "issues": issues })),
        // Surface a redacted error message — never the token, never the raw URL.
        Err(e) => Json(serde_json::json!({
            "ok": false,
            "issues": [],
            "message": format!("Could not list issues for {repo}: {e}"),
        })),
    }
}

/// Request to adopt a specific GitHub issue onto the spine. The title/body are sent
/// from the picker (already fetched in the list call) so adoption needs no second
/// round-trip to GitHub.
#[derive(serde::Deserialize)]
pub(crate) struct AdoptIssueReq {
    /// The source repo as `owner/name`.
    pub(crate) repo: String,
    /// The issue number.
    pub(crate) number: u64,
    /// The issue title.
    #[serde(default)]
    pub(crate) title: String,
    /// The issue body (markdown). May be empty.
    #[serde(default)]
    pub(crate) body: String,
}

/// Adopt a GitHub issue (including an onboarding-emitted one) into the canonical
/// story spine: map it to a `CanonicalStory` with an `ExternalRef` pointing at the
/// issue and upsert it into the `StoryStore`. Upsert is idempotent — re-adopting the
/// same issue refreshes the spine row rather than duplicating it. This path is
/// token-free (the issue fields travel in the request), so it works the same in a
/// test as in production.
pub(crate) async fn adopt_issue(
    State(state): State<AppState>,
    Json(req): Json<AdoptIssueReq>,
) -> Result<Json<CanonicalStory>, AppError> {
    let repo = req.repo.trim();
    if camerata_worktracker::RepoCoord::parse(repo).is_none() {
        return Err(AppError::bad_request(anyhow::anyhow!(
            "repo must be `owner/name`, got `{repo}`"
        )));
    }
    let story = crate::github_issues::issue_to_story(repo, req.number, &req.title, &req.body);
    state
        .stories
        .upsert(story.clone())
        .await
        .map_err(AppError)?;
    Ok(Json(story))
}
