//! Scan entry handlers: the local-sources resolver, `onboard_scan`, the audit request
//! and corpus loading.

use crate::*;

/// Request to scan repo(s) for the brownfield audit. Accepts a SET of repos (a
/// brownfield onboarding spans inter-related repos); a single `repo` is also
/// accepted for convenience.
#[derive(serde::Deserialize)]
pub(crate) struct ScanReq {
    /// `owner/repo` entries to scan together.
    #[serde(default)]
    pub(crate) repos: Vec<String>,
    /// Convenience single repo (folded into `repos`).
    #[serde(default)]
    pub(crate) repo: Option<String>,
    /// Go ahead when some repos have no local clone; the report then lists them as not
    /// inspected. Without it the scan is refused and says which repos are missing.
    #[serde(default)]
    pub(crate) allow_partial: bool,
}

/// Brownfield scan: audit a SET of existing repos against the content rules and
/// propose a starter ruleset, aggregating across all of them. Gated on the GitHub
/// token — without it, returns a gated report (no scan) so the UI shows "connect
/// GitHub". The audit reuses the gate's own arms, so it reports exactly what the
/// gate would deny on a new write.
/// Resolve a set of repos to their LOCAL working-tree dirs for an onboarding read (scan /
/// audit / waiver registry). Local-first: a repo's dir is its per-repo path override, else
/// `<workspace_root>/<owner>/<repo>`. Repos with no local git clone are returned as NOTES
/// (not sources) so the caller can surface "browse to the repo's folder" — onboarding reads
/// code from disk, never from GitHub.
pub(crate) fn resolve_local_sources(state: &AppState, repos: &[String]) -> LocalSources {
    let workspace_root = state.settings.workspace_root();
    let mut resolved = LocalSources::default();
    for spec in repos {
        let spec = spec.trim();
        if spec.is_empty() {
            continue;
        }
        let override_path = state.settings.repo_path(spec);
        match crate::workspace::resolve_repo_dir(
            override_path.as_deref(),
            workspace_root.as_deref(),
            spec,
        ) {
            Some(dir) if dir.join(".git").exists() => {
                resolved.sources.push((spec.to_string(), dir));
            }
            Some(dir) => {
                resolved.unresolved.push((
                    spec.to_string(),
                    format!("{} is not a local git clone", dir.display()),
                ));
            }
            None => resolved
                .unresolved
                .push((spec.to_string(), "no local folder is set".to_string())),
        }
    }
    resolved.notes = resolved
        .unresolved
        .iter()
        .map(|(spec, why)| format!("{spec}: {why}, browse to the repo's folder"))
        .collect();
    resolved
}

/// What [`resolve_local_sources`] found: the repos with a local clone, and the ones without.
#[derive(Default)]
pub(crate) struct LocalSources {
    /// `(spec, folder)` for every repo that has a local git clone.
    pub sources: Vec<(String, std::path::PathBuf)>,
    /// One note per repo that has none, in the words the scan message uses.
    pub notes: Vec<String>,
    /// `(spec, reason)` for every repo that has none. A run that goes ahead without them must
    /// say so in its report; one that cannot is refused ([`partial_run_refusal`]).
    pub unresolved: Vec<(String, String)>,
}

/// The refusal for a run that would inspect fewer repos than it was asked to, or `None` when
/// every repo resolved or the caller said it accepts a partial run. A project of eleven repos
/// must never produce a report for ten without anyone having said so: the default is to stop and
/// name the repos, and `allow_partial` is the explicit way to go ahead and have the report list
/// them as not inspected.
pub(crate) fn partial_run_refusal(
    unresolved: &[(String, String)],
    total: usize,
    allow_partial: bool,
) -> Option<String> {
    if unresolved.is_empty() || allow_partial {
        return None;
    }
    let list = unresolved
        .iter()
        .map(|(spec, why)| format!("{spec} ({why})"))
        .collect::<Vec<_>>()
        .join("; ");
    Some(format!(
        "Cannot inspect {} of {total} repositories: {list}. Set the folder for each one, or run \
         again with allow_partial to inspect the rest; the report then lists these as not inspected.",
        unresolved.len()
    ))
}

/// Record every repo that could not be resolved on `report` as a repo not inspected: a failed
/// `repository read` pass, a not-run row for every rule selected for it, and an entry in
/// [`crate::onboard::ScanReport::repos_not_inspected`]. Used by a run that went ahead without
/// them (`allow_partial`).
pub(crate) fn record_unresolved_repos(
    report: &mut crate::onboard::ScanReport,
    unresolved: &[(String, String)],
    selected: &[crate::onboard::SelectedRule],
    corpus: Option<&camerata_rules::RuleSet>,
) {
    for (spec, why) in unresolved {
        report.note_repo_not_inspected(spec, why, selected, corpus);
    }
}

/// [`record_unresolved_repos`] for a run that has just built its incremental manifest. The
/// manifest was stamped complete or not before these repos were known, so it is corrected here,
/// before it is stored: a manifest that says the AI review covered everything, from a run that
/// never read a repo, would be reused as a complete baseline by the next scan.
pub(crate) fn record_unresolved_repos_before_storing(
    report: &mut crate::onboard::ScanReport,
    manifest: &mut crate::scan_cache::ScanManifest,
    unresolved: &[(String, String)],
    selected: &[crate::onboard::SelectedRule],
    corpus: Option<&camerata_rules::RuleSet>,
) {
    record_unresolved_repos(report, unresolved, selected, corpus);
    if !report.ai_review_complete() {
        manifest.ai_covered = false;
    }
}

pub(crate) async fn onboard_scan(
    State(state): State<AppState>,
    Json(req): Json<ScanReq>,
) -> Json<crate::onboard::ScanReport> {
    let mut repos = req.repos;
    if let Some(r) = req.repo {
        repos.push(r);
    }
    repos.retain(|r| !r.trim().is_empty());
    if repos.is_empty() {
        let mut r = crate::onboard::ScanReport::gated(&repos);
        r.gated = false;
        r.message = Some("Add at least one repo (browse to its local folder) to scan.".to_string());
        return Json(r);
    }

    // Local-first: scan reads the repos' local working trees, not GitHub. No token needed.
    // While it reads them, deleting that source is refused rather than raced (see `onboard_audit`).
    let _scan_claim = state
        .projects
        .active()
        .map(|p| state.source_ops.begin_scan(&p.id));
    let LocalSources {
        sources,
        notes,
        unresolved,
    } = resolve_local_sources(&state, &repos);
    if let Some(why) = partial_run_refusal(&unresolved, repos.len(), req.allow_partial) {
        // The same shape a client-engagement refusal has (`blocked`, with the reason), which the
        // desktop already renders as a refusal banner. A bare message on an otherwise empty
        // report would show as a successful scan that found nothing.
        return Json(client_lock_blocked_report(&repos, why));
    }
    let mut report = crate::onboard::scan_repos(&sources, notes).await;
    for (spec, why) in &unresolved {
        report.note_repo_not_inspected(spec, why, &[], None);
    }
    // Populate the per-project last-scan store so the chat assistant sees deterministic
    // results immediately. The async audit path (`onboard_audit_start`) and the sync audit
    // (`onboard_audit`) already do this; without it here, a floor-only `/api/onboard/scan`
    // leaves `last_scan` empty and the chat shows "no scan results yet". Fail-soft, keyed by
    // the active project (the same id `active_project_context` reads).
    if let Some(id) = state.projects.active().map(|p| p.id) {
        state.set_last_scan(id, report.clone());
    }
    Json(report)
}

/// One selected rule the Phase-2 audit runs against, with its per-repo binding. An empty
/// `repos` means PROJECT-LEVEL (scanned against every repo); a non-empty `repos` scopes the
/// rule to just those repos. The UI sends each repo's selections plus the project-level set,
/// and the audit scopes each repo to the rules that apply to it.
#[derive(serde::Deserialize, serde::Serialize)]
pub(crate) struct AuditRuleReq {
    #[serde(default)]
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) directive: String,
    /// Repos this rule applies to. Empty = project-level (all repos). Omitted by older
    /// single-repo callers, which correctly reads as "applies to the one repo scanned".
    #[serde(default)]
    pub(crate) repos: Vec<String>,
}

#[derive(serde::Deserialize, serde::Serialize)]
pub(crate) struct AuditReq {
    /// Go ahead when some repos have no local clone; the report then lists them as not
    /// inspected and the release gate refuses to package it. Without it the audit is refused
    /// and says which repos are missing.
    #[serde(default)]
    pub(crate) allow_partial: bool,
    /// The scan settings' spend limit in US dollars (`camerata inspect --max-cost-usd`'s twin).
    /// `None` = no limit. The run stops between passes once actual spend reaches it and says
    /// what it did not run; see [`crate::cost_cap`].
    #[serde(default)]
    pub(crate) max_cost_usd: Option<f64>,
    #[serde(default)]
    pub(crate) repos: Vec<String>,
    #[serde(default)]
    pub(crate) rules: Vec<AuditRuleReq>,
    /// The model the USER picked for this audit (company-agnostic id, e.g.
    /// `claude-sonnet-5`). None / empty → the server default. The user owns the
    /// thoroughness-vs-speed trade-off by choosing here.
    #[serde(default)]
    pub(crate) model: Option<String>,
    /// The model the user picked for the CALIBRATION pass (severity recalibration +
    /// confidence tagging). Its own knob so a customer can run a cheap scan with a stronger
    /// verify (or keep it end-to-end). None / empty → falls back to the scan model.
    #[serde(default)]
    pub(crate) calibration_model: Option<String>,
    /// The execution mode: `parallel` (default) or `sequential`. Speed/scale knob,
    /// orthogonal to model + rules.
    #[serde(default)]
    pub(crate) mode: Option<String>,
    /// Thorough calibration (#51): when true, the calibration pass runs multiple times and takes
    /// a conservative consensus, plus a proportionality signal. Costs more AI; opt-in.
    #[serde(default)]
    pub(crate) thorough: bool,
    /// Incremental scan: when true (the default), the AI audit only re-scans files whose content
    /// changed since the last scan of this project, carrying forward cached findings for unchanged
    /// files. The UI's "Full scan (ignore incremental cache)" checkbox sends `false` to force a
    /// clean pass over every file. The first scan of a project is always full (no cache yet).
    #[serde(default = "default_true")]
    pub(crate) incremental: bool,
    /// OPT-IN deep compliance & security tier (#55, in-MVP per #62). When true, AFTER the
    /// standard audit Camerata runs the three deep lenses — SOC-2 readiness gap analysis, a
    /// deep security audit (beyond the deterministic floor), and a threat model — over each
    /// repo on the selected model, and attaches the result to the report's `deep` field.
    /// Defaults to FALSE: it is the MOST EXPENSIVE tier (three extra whole-repo passes) and
    /// must never run by default. Its output is ADVISORY + model-inferred (#62), not a SOC-2
    /// report and not a penetration test.
    #[serde(default)]
    pub(crate) deep: bool,
    /// Scan-type selector (Part C) — run the AI architectural review (the LLM scan of
    /// architectural/structured/prose rules, plus the deep tier when `deep`). Defaults to
    /// TRUE (today's behaviour). When false the audit makes NO model calls — the LLM passes
    /// are skipped entirely (no tokens). If BOTH this and `run_deterministic` arrive false,
    /// `effective_scan_modes` forces both back to true (never a no-op scan).
    #[serde(default = "default_true")]
    pub(crate) run_ai_review: bool,
    /// Scan-type selector (Part C) — run the DETERMINISTIC scans: the always-on security
    /// floor plus the scan-time mechanical preview pass. Defaults to TRUE. When false the
    /// floor + `merge_scan_preview` are skipped. Deterministic-only (this true, `run_ai_review`
    /// false) is fast and uses no LLM / no tokens.
    #[serde(default = "default_true")]
    pub(crate) run_deterministic: bool,
}

/// serde default for an opt-OUT boolean (defaults to `true` when the field is absent).
pub(crate) fn default_true() -> bool {
    true
}

/// Resolve the scan-type selector flags into the effective `(run_ai_review, run_deterministic)`
/// pair. Both default true; a request that turns BOTH off is a no-op scan, so we force both
/// back ON rather than do nothing (the decision: default-both over a 4xx — the scan still runs
/// useful work and the UI keeps both checked, so this is only reachable by a hand-crafted
/// request). Returns the pair plus whether a both-false coercion happened (for a note).
pub fn effective_scan_modes(run_ai_review: bool, run_deterministic: bool) -> (bool, bool, bool) {
    if !run_ai_review && !run_deterministic {
        (true, true, true)
    } else {
        (run_ai_review, run_deterministic, false)
    }
}

/// The transcript key the scan/audit AI activity registers under (the Agent-activity
/// drawer polls `/api/runs/scan-audit/agents`).
pub(crate) const SCAN_AUDIT_KEY: &str = "scan-audit";

/// Phase 2 — audit the repos AGAINST the selected rules (the deterministic security floor
/// plus the AI audit parameterized by the chosen rules). Returns the findings report. The
/// AI activity (prompts and output) registers into the transcript store so the UI can
/// show, live, that the model is actually working.
///
/// W4 (root-cause fix): this used to PARTITION selected audit rules into the ones the code-only
/// AI scan should check (prose / structured) and the CI-tier ones it should NOT (mechanical /
/// architectural), on the theory that a CI-tier declaration meant a deterministic CI gate
/// already covered the rule, so a static-code LLM pass would be redundant at best. That theory
/// was wrong: "mechanical" means "deterministically checkable IN PRINCIPLE, once someone builds
/// a repo-specific CI gate", not "actually checked, on THIS unseen repo, right now" — a rule
/// like strict layering depends on a repo-specific layer map no generic detector ships with the
/// product, so on a repo Camerata has never scanned before, no gate exists for it yet. Excluding
/// such a rule from the model AND shipping no detector for it meant NOTHING ever evaluated it,
/// while the report still called it clean — the defect this function's rename-in-place fixes.
///
/// Every selected rule is now AI-scannable (`scannable` == `selected`, unchanged); `excluded` is
/// kept, ALWAYS EMPTY, purely for tuple-shape/JSON back-compat with existing callers — nothing
/// is excluded from the semantic pass on CI-tier grounds anymore. `preview_rules` is UNRELATED
/// to AI-scannability and keeps its original, narrower meaning: the subset of CI-tier mechanical
/// rules the SCAN-TIME deterministic linter preview pass ([`crate::scan_tools::run_scan_tools`])
/// can additionally run locally (mechanical, and NOT `layer3_only` — CodeQL / paid tiers never
/// preview). The loaded `corpus` is returned so the caller can resolve each rule's linter source
/// without re-loading it.
///
/// Returns `(scannable, excluded_ci_tier_ids, preview_rules, corpus, corpus_load_failures)`.
/// `excluded_ci_tier_ids` is now always `Vec::new()`.
///
/// # W13: the fifth element is NOT optional to consume
///
/// This used to be a 4-tuple that dropped `load_corpus_lenient`'s error list on the floor
/// (`load_corpus_lenient(&corpus_path).await.0`). A rule file with a typo'd enum value failed to
/// parse, left the corpus, and nothing reported it — not the ledger, not the coverage sheet, not
/// `camerata integrity-gate`, not a log line. Every caller that goes on to run a SCAN must pass
/// the fifth element to [`record_corpus_load_failures`] once its `ScanReport` exists; see that
/// function's doc comment for where the information then goes.
///
/// `pub`: also called directly by `camerata inspect` (the headless CLI, mirroring
/// `onboard_audit`'s own pipeline so the two never drift — see
/// `crates/cli/src/inspect_cmd.rs`).
pub async fn split_scannable_rules(
    selected: Vec<crate::onboard::SelectedRule>,
) -> (
    Vec<crate::onboard::SelectedRule>,
    Vec<String>,
    Vec<crate::onboard::SelectedRule>,
    Option<camerata_rules::RuleSet>,
    Vec<CorpusLoadFailure>,
) {
    split_scannable_rules_from(&camerata_rules::corpus_path(), selected).await
}

/// [`split_scannable_rules`] against an EXPLICIT corpus directory.
///
/// The seam exists for one reason: `camerata_rules::corpus_path()` reads the process-global
/// `CAMERATA_CORPUS_PATH` env var, so a test that wanted to drive this function over a
/// deliberately-malformed synthetic corpus could only do it by mutating process state and
/// racing every other test in the binary. With the path as a parameter, the W13 tests point at
/// a `tempfile` corpus directly and nothing global moves.
pub async fn split_scannable_rules_from(
    corpus_dir: &std::path::Path,
    selected: Vec<crate::onboard::SelectedRule>,
) -> (
    Vec<crate::onboard::SelectedRule>,
    Vec<String>,
    Vec<crate::onboard::SelectedRule>,
    Option<camerata_rules::RuleSet>,
    Vec<CorpusLoadFailure>,
) {
    let (set, load_failures) = load_corpus_for_scan(corpus_dir).await;
    // A CI-tier mechanical rule is PREVIEW-runnable unless it is layer3_only. This is the ONLY
    // thing this function still partitions on — see the doc comment above for why AI-scannability
    // no longer depends on CI-tier at all.
    let is_preview_runnable = |id: &str| -> bool {
        set.as_ref()
            .and_then(|s| s.get_by_id(id))
            .map(|r| r.enforcement.is_ci_enforced() && !r.is_layer3_only())
            .unwrap_or(false)
    };
    let preview: Vec<crate::onboard::SelectedRule> = selected
        .iter()
        .filter(|r| is_preview_runnable(&r.id))
        .cloned()
        .collect();
    // Every selected rule reaches the semantic pass now — no CI-tier exclusion.
    let scannable = selected;
    let excluded: Vec<String> = Vec::new();
    (scannable, excluded, preview, set, load_failures)
}

/// W13 — ONE rule file the corpus loader could not parse, kept rather than discarded.
///
/// The three fields are deliberately split by AUDIENCE, because conflating them is how this
/// defect's sibling (W6's `run_capture_stdout` dropping stderr) wasted an operator's time:
///
/// - [`Self::rule_id`] is the only CLIENT-SAFE field. It is a plain rule id, which a client
///   already sees throughout the deliverable.
/// - [`Self::path`] and [`Self::error`] are OPERATOR-ONLY. The path names the reviewer's own
///   filesystem layout and the error names TOML internals; neither belongs in a client
///   artifact, and [`record_corpus_load_failures`] keeps both out of one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusLoadFailure {
    /// The rule id, derived from the file STEM by [`rule_id_from_corpus_path`] — the id
    /// declared INSIDE the file is unreadable by definition here.
    pub rule_id: String,
    /// Full path to the offending file. Operator-only; never exported.
    pub path: std::path::PathBuf,
    /// The loader's own message (`RulesError`'s `Display`), naming the TOML problem.
    /// Operator-only; never exported.
    pub error: String,
}

/// Derive the rule id for an unparseable corpus file from its FILE STEM, uppercased.
///
/// In this corpus the stem IS the lowercased rule id for every single file
/// (`principles/api-layer/arch-handler-no-db-1.toml` declares `id = "ARCH-HANDLER-NO-DB-1"`),
/// so the stem is the most faithful handle available — and the id inside the file cannot be
/// read, because failing to read the file is the whole problem.
///
/// `load_corpus_lenient` has one error shape that is NOT a per-file parse failure: when the
/// recursive walk itself fails it returns `(corpus_dir, err)`, i.e. a DIRECTORY. A directory's
/// stem is not a rule id, so that case gets the explicit sentinel `RULE-CORPUS` rather than a
/// plausible-looking lie (a line reading `API-LAYER: rule file could not be loaded` would send
/// a reader hunting for a rule that does not exist).
pub fn rule_id_from_corpus_path(path: &std::path::Path) -> String {
    let is_toml_file = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("toml"));
    if !is_toml_file {
        return "RULE-CORPUS".to_string();
    }
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_uppercase())
        .unwrap_or_else(|| "RULE-CORPUS".to_string())
}

/// W13 — load the rule corpus for a scan: the rules that parsed, PLUS an honest record of
/// every file that did not.
///
/// This is the replacement for the `load_corpus_lenient(&p).await.0` idiom on the scan path.
/// `None` for the corpus means the directory does not exist at all (the pre-existing
/// degrade-soft contract every corpus-optional join in this codebase already has); an EMPTY
/// failure list means every `.toml` under it parsed.
///
/// Deliberately NOT `camerata_rules::load_corpus` (the strict, `Result`-returning loader). It
/// exists, but switching production to it would mean one malformed rule file fails an entire
/// client scan — trading an invisible failure for a brittle one. The corpus still loads
/// leniently; what changes is that the errors now go somewhere.
pub async fn load_corpus_for_scan(
    corpus_dir: &std::path::Path,
) -> (Option<camerata_rules::RuleSet>, Vec<CorpusLoadFailure>) {
    if !corpus_dir.exists() {
        return (None, Vec::new());
    }
    let (set, errors) = camerata_rules::load_corpus_lenient(corpus_dir).await;
    // The failure report's scrubber keeps a long rule id out of its credential redaction.
    camerata_llm::failure::register_rule_ids(set.iter().map(|r| r.id.0.clone()));
    let failures = errors
        .into_iter()
        .map(|(path, error)| CorpusLoadFailure {
            rule_id: rule_id_from_corpus_path(&path),
            path,
            error: error.to_string(),
        })
        .collect();
    (Some(set), failures)
}

/// The [`crate::onboard::CoverageNote::tool`] value W13's corpus-load failures register under.
///
/// Not a real tool name, exactly like the pre-existing `"unrouted"` sentinel — it is the key
/// `report_export::tool_coverage_disclosures` matches to render the one concise client-facing
/// line. Kept here next to its only producer, and read by `report_export` through this
/// constant rather than a retyped literal so the two cannot drift.
pub const RULE_CORPUS_COVERAGE_TOOL: &str = "rule-corpus";

/// The [`crate::onboard::CoverageNote::tool`] value for the Firebase scope statement. Like
/// [`RULE_CORPUS_COVERAGE_TOOL`] it is not a tool name: it keys the one written client sentence
/// `report_export::tool_coverage_disclosures` renders for a repository whose stack includes
/// Firebase.
pub const FIREBASE_SCOPE_COVERAGE_TOOL: &str = "firebase-scope";

/// What "Firebase coverage" means in a report (client voice, no dashes): the honest scope of the
/// Firebase rules, so a reader never mistakes a clean Firebase section for coverage of Dart client
/// logic or of rules that live only in the console.
pub const FIREBASE_SCOPE_SENTENCE: &str = "Firebase coverage in this inspection is the security rules files in the repository (Cloud Firestore, Cloud Storage and Realtime Database), the Firebase project configuration, and Cloud Functions written in JavaScript or TypeScript. Dart and Flutter client code was read for committed secrets and by the advisory review, but it was not parsed structurally and no Flutter specific rule was applied. Rules and settings that exist only in the Firebase console are not visible to a repository inspection.";

/// W13 — route a scan's corpus-load failures to the four places built for exactly this, so a
/// rule that never parsed can no longer disappear.
///
/// Called by every scan entry point (`onboard_audit`, `onboard_audit_start`, and `camerata
/// inspect`) right after `onboard::audit_repos` returns, i.e. as soon as a `ScanReport` with a
/// `ScanLedger` exists to record into.
///
/// Each failure lands in all four, split strictly by audience:
///
/// 1. **The ledger** — `ran = false` with [`crate::scan_ledger::CORPUS_LOAD_FAILED_REASON`].
///    From there `excluded_rules()` carries it into the report JSON's `rules_not_run` and the
///    workbook's "Rules not run this scan" sheet, and `healthy_rule_ids()` can never call it
///    verified-clean.
/// 2. **`camerata integrity-gate`** — the ledger entry classifies as
///    [`crate::integrity_gate::SkipClass::CorpusLoadFailed`], which is never justified under
///    any profile and fails the gate with its own `corpus_load_failure` code.
/// 3. **The client, concisely (G3)** — ONE [`crate::onboard::CoverageNote`] per failure under
///    [`RULE_CORPUS_COVERAGE_TOOL`], which `report_export::tool_coverage_disclosures` collapses
///    into a single sentence for the PDF/JSON `failed_passes` and the workbook's Coverage
///    sheet. The note's own `message` carries the operator detail and is NEVER exported: only
///    `osv-scanner` notes have their raw message exported (see `build_report_json`'s
///    `dep_coverage_notes`), and every other tool's note is replaced by that one written line.
/// 4. **The operator log** — `tracing::error!` with the full path and the parser's own message.
///    `error!`, not `warn!`: a rule silently leaving the corpus degrades the engine a client is
///    paying to have run.
pub fn record_corpus_load_failures(
    report: &mut crate::onboard::ScanReport,
    failures: &[CorpusLoadFailure],
) {
    for failure in failures {
        tracing::error!(
            rule_id = %failure.rule_id,
            path = %failure.path.display(),
            error = %failure.error,
            "a rule corpus file failed to load; the rule did not run this scan and is \
             disclosed as not-run in the report, the ledger and the integrity gate",
        );
        report.ledger.record_corpus_load_failure(&failure.rule_id);
        report.coverage_notes.push(crate::onboard::CoverageNote {
            tool_failed: false,
            repo: None,
            tool: RULE_CORPUS_COVERAGE_TOOL.to_string(),
            // Operator-facing detail. See this function's doc comment, point 3, for why this
            // text reaches the UI and the ledger but never a client artifact.
            message: format!(
                "{} did not parse, so rule {} was not loaded and did not run: {}",
                failure.path.display(),
                failure.rule_id,
                failure.error,
            ),
        });
    }
}

/// Extract a human-readable message from a caught panic payload (`std::panic::catch_unwind`
/// / `futures::FutureExt::catch_unwind`'s `Err` side). A panic conventionally carries either
/// a `&'static str` (a string-literal `panic!("...")`) or an owned `String` (a formatted
/// `panic!("{x}")`); anything else (a custom payload type) falls back to a generic label so
/// this ALWAYS produces a usable message rather than failing to report one at all — the
/// whole point of catching the panic here is to surface it, never to trade one silent
/// failure for another.
///
/// Takes the `Box<dyn Any + Send>` BY REFERENCE, deliberately — not `&(dyn Any + Send)`.
/// `Box<dyn Any + Send>` itself satisfies `Any`'s blanket impl, so a caller passing
/// `&payload` where `payload: Box<dyn Any + Send>` to a `&(dyn Any + Send)` parameter
/// would coerce via UNSIZING the box itself (treating "the Box IS the Any") rather than
/// DEREFING into its contents, and `downcast_ref` would then always miss — silently
/// reporting every real panic as "no panic message available". Taking `&Box<...>` here
/// means `payload.downcast_ref()` resolves through Rust's method-call auto-deref (which
/// unlike argument coercion, derefs through the `Box` to the `dyn Any` it targets first),
/// so the natural call `panic_message(&panic)` is simply correct at every call site.
pub(crate) fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "no panic message available".to_string()
    }
}

/// Run one job's background audit `body` to completion, guaranteeing `jid` lands in a
/// TERMINAL job state (`done` or `failed`) no matter how `body` exits.
///
/// This is the panic guard for the class of "the scan silently dies with no error" bugs:
/// `body` is expected to end by calling `jobs.finish(jid, ...)` or `jobs.fail(jid, ...)`
/// itself on every path it knows about (empty sources, a clean completion, ...). What it
/// canNOT protect against on its own is a PANIC partway through — a stuck subprocess
/// turning into an unwrap, an indexing bug, anything — which would otherwise unwind
/// straight out of the spawned task with nobody ever calling `fail`/`finish`, leaving the
/// job stuck at `running` forever and the UI watching nothing happen. Wrapping `body` in
/// `catch_unwind` here means that no matter what it does internally, EITHER it reaches its
/// own terminal call, OR this function reaches one on its behalf.
///
/// `AssertUnwindSafe` is required because `body` is expected to close over `&mut` locals
/// (an accumulating report, loop counters) that aren't automatically `UnwindSafe`. That is
/// sound here: every value that must survive a panic (the job store, scan cache, and
/// friends) is behind `Arc<Mutex<_>>`, which IS unwind-safe via mutex poisoning; everything
/// else is plain owned data that is simply dropped, unobserved, when `body` panics.
///
/// Calling `jobs.fail` after `body` already called `jobs.finish` (or `fail`) would be a
/// bug, but it can't happen: `catch_unwind` only returns `Err` when `body` panicked, and a
/// panic can only occur BEFORE `body`'s own terminal call runs (it's always the last thing
/// on every path) — never after, since there is no code left to panic in.
pub(crate) async fn run_job_body_with_panic_guard<F>(
    jobs: &crate::jobs::JobStore,
    jid: &str,
    body: F,
) where
    F: std::future::Future<Output = ()>,
{
    use futures::FutureExt;
    if let Err(panic) = std::panic::AssertUnwindSafe(body).catch_unwind().await {
        let msg = panic_message(&panic);
        jobs.fail(
            jid,
            format!(
                "The scan crashed unexpectedly: {msg}. Please retry; if it keeps happening, \
                 this is a bug worth reporting."
            ),
        );
    }
}
