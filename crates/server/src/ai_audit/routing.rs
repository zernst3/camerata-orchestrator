//! Stack exceptions, the routed (chunk, rule) units and the passes that run them, and the rows that record decided and unassessed rules.

use super::*;

/// P7 (the codebase-inspection hardening notes), A10: mark as thrown out (keep, never delete) any finding whose rule
/// carries a [`camerata_rules::StackException`] that applies to the repo's DETECTED stack +
/// this finding's own file path — e.g. `ARCH-MONOLITH-FIRST-1` flagging a `supabase/functions/`
/// directory on a Supabase-stack repo, where a second Edge Functions deployable is the
/// idiomatic pattern, not a monolith-topology violation. GENERAL by construction: this checks
/// every finding against whatever `[[stack_exception]]` blocks its own rule declares in the
/// corpus — it is not special-cased to any one rule or framework (see
/// [`camerata_rules::StackException`]'s doc comment for the mechanism). A finding whose rule id
/// isn't in the corpus (an AI-invented `AI-`-prefixed name, or a rule id the loaded corpus
/// doesn't contain) is never excepted — only a real corpus rule can declare an exception.
pub fn apply_stack_exceptions(
    findings: Vec<Finding>,
    detected_frameworks: &[String],
    corpus: &camerata_rules::RuleSet,
) -> Vec<Finding> {
    if detected_frameworks.is_empty() {
        return findings;
    }
    findings
        .into_iter()
        .map(|mut f| {
            let rule = corpus
                .get_by_id(&f.rule_id)
                .or_else(|| corpus.get_by_id(f.rule_id.trim_start_matches("AI-")));
            let excepted = match rule {
                Some(rule) => rule
                    .stack_exception_for(detected_frameworks, &f.path)
                    .is_some(),
                None => false,
            };
            if excepted {
                // A10: kept, marked thrown out, and left out of what ships.
                crate::engine_discard::mark(&mut f, crate::engine_discard::STACK_EXCEPTION);
            }
            f
        })
        .collect()
}

/// One route group's share of a scan, ready to run: the files its scope covers (re-chunked on
/// their own, so a chunk only holds files its rules can apply to), its rules, and how the
/// advisory pass is wired for it.
///
/// [`routed_units`] is the ONE place that decides which (chunk, rule) pairs a scan sends. The
/// real-time path ([`run_routed_passes`]), the batch path ([`run_routed_passes_batch`]) and the
/// routing telemetry all read it, so they cannot send different pairs.
pub(super) struct RoutedUnit {
    /// Index of the group in the route plan (stable session-id component).
    pub(super) index: usize,
    /// `pass[<lang>]`, `pass[<declared scope>]` or `pass[all]`.
    pub(super) label: String,
    /// What to tell the report when this group matched no file.
    empty_note: String,
    /// The files this group's scope covers.
    pub(super) files: Vec<(String, String)>,
    /// The group's `(id, directive)` pairs, in plan order.
    pub(super) rules: Vec<(String, String)>,
    /// True for a scoped group when a cross-cutting `All` group also runs this scan; see
    /// [`routed_units`].
    pub(super) advisory_disabled: bool,
}

/// Turn a route plan into the units that run. Every group of the plan becomes a unit (a group
/// that matched no file has empty `files`; the caller records it, see [`runnable_units`]).
///
/// # Advisory wiring (W2)
///
/// The advisory "flag novel issues beyond the adopted rules" pass must fire exactly once per
/// file chunk. A scoped group (id-prefix language or declared `applies_to`) disables it ONLY when
/// a cross-cutting `All` group ALSO runs this scan: that group already sees every file, so
/// advisory in a scoped group too would re-flag the same novel issue under a second invented
/// name. When there is no `All` group at all (every selected rule is scoped), nothing else would
/// ever ask "is there anything wrong beyond the listed rules?", so every scoped group keeps
/// advisory on. Id-prefix language groups are mutually exclusive by file, so that never
/// duplicates; two DECLARED scopes can overlap on a file, in which case that file's advisory
/// runs once per overlapping group and the duplicates collapse in the cross-chunk dedup and
/// location merge. That costs a few repeated advisory tokens in an unusual selection (every
/// cross-cutting rule scoped); the alternative, dropping advisory for a file, would lose findings.
pub(super) fn routed_units(
    files: &[(String, String)],
    route_plan: &crate::scan_routing::RoutePlan,
) -> Vec<RoutedUnit> {
    use crate::scan_routing::Scope;
    let has_all_group = route_plan
        .groups
        .iter()
        .any(|g| matches!(g.scope, Scope::All));
    route_plan
        .groups
        .iter()
        .enumerate()
        .map(|(index, group)| {
            let (label, empty_note) = match &group.scope {
                Scope::Language(lang) => (
                    format!("pass[{lang}]"),
                    format!("no {lang} files matched this scan's file set"),
                ),
                Scope::Declared { language, .. } => (
                    format!(
                        "pass[{}]",
                        language.map_or_else(|| "scoped".to_string(), str::to_string)
                    ),
                    "no file in this scan's file set is one this rule applies to".to_string(),
                ),
                Scope::All => (
                    "pass[all]".to_string(),
                    "no files matched this scan's file set".to_string(),
                ),
            };
            RoutedUnit {
                index,
                label,
                empty_note,
                files: crate::scan_routing::files_in_scope(files, &group.scope),
                rules: group.rule_pairs(),
                advisory_disabled: has_all_group && !matches!(group.scope, Scope::All),
            }
        })
        .collect()
}

/// The units that actually run: [`routed_units`] without the groups that matched no file.
///
/// Pass-1 W3 (A5.6, `NotAssessed`): a group that matched ZERO files was ARMED but nothing was
/// ever judged against it. Each of its rules is pushed to `skipped_rules` for the caller
/// (`audit_repo`) to turn into one honest `not_assessed` row, rather than leaving "never
/// evaluated" indistinguishable from "assessed and clean". Real-time and batch mode both go
/// through here, so both disclose the same skipped rules.
///
/// Also records the routing telemetry (how many (chunk, rule) pairs routing did not send) when
/// there is a `meter`.
pub(super) fn runnable_units(
    files: &[(String, String)],
    route_plan: &crate::scan_routing::RoutePlan,
    meter: Option<&UsageMeter>,
    skipped_rules: &mut Vec<(String, String)>,
) -> Vec<RoutedUnit> {
    let units = routed_units(files, route_plan);
    if let Some(m) = meter {
        // (chunk, rule) pairs: what an un-routed scan would send (every rule with every chunk)
        // against what the units send (each group's rules with that group's own chunks).
        let n_rules: usize = units.iter().map(|u| u.rules.len()).sum();
        let total = chunk_files(files, CHUNK_DIGEST_CHARS).len() * n_rules;
        let sent: usize = units
            .iter()
            .map(|u| chunk_files(&u.files, CHUNK_DIGEST_CHARS).len() * u.rules.len())
            .sum();
        m.record_routing(total as u64, total.saturating_sub(sent) as u64);
    }
    let mut runnable = Vec::with_capacity(units.len());
    for unit in units {
        if unit.files.is_empty() {
            skipped_rules.extend(
                unit.rules
                    .iter()
                    .map(|(id, _)| (id.clone(), unit.empty_note.clone())),
            );
        } else {
            runnable.push(unit);
        }
    }
    runnable
}

/// The units a Message Batch scan submits, one Message Batch each.
///
/// With `applies_to_routing` ON these are exactly the real-time path's [`runnable_units`], so
/// batch and real-time send the same (chunk, rule) pairs. With it OFF this is the batch scan as
/// it was before routing reached batch mode: ONE submission of every rule against every file,
/// labelled `pass`, advisory on. The switch therefore isolates routing completely: OFF in batch
/// mode is "no routing at all", not "language routing only".
pub(super) fn batch_units(
    files: &[(String, String)],
    selected: &[crate::scan_routing::AuditRule],
    route_plan: &crate::scan_routing::RoutePlan,
    applies_to_routing: bool,
    meter: Option<&UsageMeter>,
    skipped_rules: &mut Vec<(String, String)>,
) -> Vec<RoutedUnit> {
    if applies_to_routing {
        return runnable_units(files, route_plan, meter, skipped_rules);
    }
    vec![RoutedUnit {
        index: 0,
        label: "pass".to_string(),
        empty_note: String::new(),
        files: files.to_vec(),
        rules: crate::scan_routing::rule_pairs(selected),
        advisory_disabled: false,
    }]
}

/// Drop the "armed but nothing matched" disclosures that the scan's file set cannot support.
///
/// `runnable_units` reports a rule whose group matched no file in `files`. On an INCREMENTAL scan
/// `files` is only the changed files, so a rule whose matching files exist in the repo but did
/// not change this time would be reported as never assessed, while its earlier findings are
/// carried forward. A rule is only reported when the FULL file set (`full_files`) has no match
/// either.
pub(super) fn drop_skips_with_matches_in_full_set(
    skipped_rules: &mut Vec<(String, String)>,
    route_plan: &crate::scan_routing::RoutePlan,
    full_files: &[(String, String)],
) {
    let covered: std::collections::HashSet<String> = route_plan
        .groups
        .iter()
        .filter(|g| {
            full_files
                .iter()
                .any(|(p, c)| crate::scan_routing::file_in_scope(p, c, &g.scope))
        })
        .flat_map(|g| g.rules.iter().map(|r| r.id.to_ascii_uppercase()))
        .collect();
    skipped_rules.retain(|(id, _)| !covered.contains(&id.to_ascii_uppercase()));
}

/// The `(chunk, rule)` pairs a scan sends, as `(chunk's file paths, rule id)`, for a unit list.
#[cfg(test)]
pub(super) fn pair_set_of(
    units: &[RoutedUnit],
) -> std::collections::BTreeSet<(Vec<String>, String)> {
    units
        .iter()
        .flat_map(|u| {
            let chunk_paths: Vec<Vec<String>> = chunk_files(&u.files, CHUNK_DIGEST_CHARS)
                .iter()
                .map(|c| c.iter().map(|(p, _)| p.clone()).collect())
                .collect();
            u.rules.iter().flat_map(move |(id, _)| {
                chunk_paths
                    .iter()
                    .map(move |paths| (paths.clone(), id.clone()))
                    .collect::<Vec<_>>()
            })
        })
        .collect()
}

/// Run the real-time audit passes with rule-routing applied.
///
/// Runs each [`RoutedUnit`] of `route_plan` (see [`runnable_units`] and [`routed_units`]):
///
/// - The unit's files are chunked for context-window sizing.
/// - [`run_passes`] runs over the unit's rules, with `advisory_disabled` as [`routed_units`]
///   decides it (so the "flag novel issues" pass fires exactly once per file chunk when a
///   cross-cutting `All` group exists).
///
/// When routing produces no savings (all rules are cross-cutting, or only one group), this
/// degenerates to the previous single-group behavior with no overhead.
///
/// Returns the same tuple as [`run_passes`]: `(findings, proposed, requested, ok, failures)`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_routed_passes(
    llm: &dyn LlmPort,
    repo: &str,
    files: &[(String, String)],
    selected: &[crate::scan_routing::AuditRule],
    route_plan: &crate::scan_routing::RoutePlan,
    repo_map: &RepoMap,
    nonce: &crate::prompt_frame::FrameNonce,
    adopted: &std::collections::HashSet<String>,
    audit_model: Option<&str>,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    job: Option<(&crate::jobs::JobStore, &str)>,
    concurrency: usize,
    batch_size: usize,
    meter: Option<&UsageMeter>,
    // Pass-1 W3 (A5.6, `NotAssessed`): every (rule_id, note) whose routing group matched ZERO
    // files this scan — the rule was ARMED but nothing was ever judged against it. Collected
    // for the caller (`audit_repo`) to synthesize one honest `not_assessed` row per rule,
    // rather than leaving "never evaluated" indistinguishable from "assessed and clean".
    skipped_rules: &mut Vec<(String, String)>,
    // Rank 8: the candidate locators and their time limit (production in `audit_repo`).
    candidate_env: candidate_passes::CandidateEnv,
) -> PassesOut {
    // When there are no rules (free-form audit), skip routing and run one pass over all files.
    if selected.is_empty() {
        let chunks = chunk_files(files, CHUNK_DIGEST_CHARS);
        let empty_batch: &[(String, String)] = &[];
        let batches: Vec<&[(String, String)]> = vec![empty_batch];
        if let Some((jstore, jid)) = job {
            jstore.add_total(jid, chunks.len() * batches.len());
        }
        return run_passes(
            llm,
            repo,
            repo_map,
            nonce,
            adopted,
            audit_model,
            feedback,
            job,
            &chunks,
            None,
            &batches,
            concurrency,
            "pass",
            &format!("audit-{repo}"),
            meter,
            false, // advisory enabled: novel findings wanted in free-form mode
        )
        .await;
    }

    let units = runnable_units(files, route_plan, meter, skipped_rules);
    // Rank 8: what each unit's chunks look like to its rules (whole files, or candidate sites).
    let pass_sets = candidate_passes::plan_pass_sets(
        &units,
        selected,
        repo,
        efficiency_of(meter).candidates_prefilter,
        candidate_env,
        nonce,
        meter,
    )
    .await;

    // Pre-seed the job's total pass count across ALL route groups so the progress bar is
    // accurate from the start: per pass set, chunk count x rule-batch count.
    let total_pass_count: usize = pass_sets
        .iter()
        .flatten()
        .map(|set| set.pass_count(batch_size))
        .sum();
    // Only seed when > 0 (empty groups are no-ops and contribute 0 passes).
    if total_pass_count > 0 {
        if let Some((jstore, jid)) = job {
            jstore.add_total(jid, total_pass_count);
        }
    }

    let mut all_findings: Vec<Finding> = Vec::new();
    let mut all_proposed: Vec<ProposedRule> = Vec::new();
    let mut all_requested: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut total_ok: usize = 0;
    let mut failures: Vec<PassFailure> = Vec::new();

    for (unit, sets) in units.iter().zip(&pass_sets) {
        let unit_chunks = chunk_files(&unit.files, CHUNK_DIGEST_CHARS);
        for set in sets {
            let chunks = set.chunks_of(&unit_chunks);
            let batches = set.batches(batch_size);

            // NOTE: job total was pre-seeded above; do NOT call add_total again per group, as
            // that would double-count. run_passes calls inc_done per completed pass.
            let (gf, gp, gr, gok, ge) = run_passes(
                llm,
                repo,
                repo_map,
                nonce,
                adopted,
                audit_model,
                feedback,
                job,
                &chunks,
                set.digests.as_deref(),
                &batches,
                concurrency,
                &set.label,
                &set.session_prefix,
                meter,
                set.advisory_disabled,
            )
            .await;

            all_findings.extend(gf);
            all_proposed.extend(gp);
            all_requested.extend(gr);
            total_ok += gok;
            failures.extend(ge);
        }
    }

    (
        all_findings,
        all_proposed,
        all_requested,
        total_ok,
        failures,
    )
}

/// [`run_routed_passes`] for Message Batch mode: with `applies_to_routing` on, the same
/// [`runnable_units`], so batch mode sends exactly the (chunk, rule) pairs real-time mode does;
/// with it off, the unrouted batch scan ([`batch_units`]). Each route group is submitted as its own
/// Message Batch, one after the other (so the spend cap sees the cost of the earlier groups
/// before it admits the next, and a group's `advisory_disabled` is honoured per group, which is
/// what [`run_passes_batch`]'s own parameter is for).
///
/// A batch error in one group (only possible with `batch_default` off; see [`run_passes_batch`])
/// is that group's failed pass and the other groups' findings are kept. If no group completed,
/// the first error is returned, exactly as the single batch did before.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_routed_passes_batch(
    llm: &dyn batch_mode::BatchApi,
    repo: &str,
    files: &[(String, String)],
    selected: &[crate::scan_routing::AuditRule],
    route_plan: &crate::scan_routing::RoutePlan,
    repo_map: &RepoMap,
    nonce: &crate::prompt_frame::FrameNonce,
    adopted: &std::collections::HashSet<String>,
    audit_model: Option<&str>,
    job: Option<(&crate::jobs::JobStore, &str)>,
    batch_size: usize,
    meter: Option<&UsageMeter>,
    skipped_rules: &mut Vec<(String, String)>,
    candidate_env: candidate_passes::CandidateEnv,
) -> anyhow::Result<PassesOut> {
    if selected.is_empty() {
        let chunks = chunk_files(files, CHUNK_DIGEST_CHARS);
        let empty_batch: &[(String, String)] = &[];
        return run_passes_batch(
            llm,
            repo,
            repo_map,
            nonce,
            adopted,
            audit_model,
            job,
            &chunks,
            None,
            &[empty_batch],
            "pass",
            meter,
            false,
        )
        .await;
    }

    let units = batch_units(
        files,
        selected,
        route_plan,
        efficiency_of(meter).applies_to_routing,
        meter,
        skipped_rules,
    );
    let pass_sets = candidate_passes::plan_pass_sets(
        &units,
        selected,
        repo,
        efficiency_of(meter).candidates_prefilter,
        candidate_env,
        nonce,
        meter,
    )
    .await;
    let mut all_findings: Vec<Finding> = Vec::new();
    let mut all_proposed: Vec<ProposedRule> = Vec::new();
    let mut all_requested: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut total_ok: usize = 0;
    let mut failures: Vec<PassFailure> = Vec::new();
    let mut groups_completed: usize = 0;
    let mut first_error: Option<anyhow::Error> = None;
    for (unit, sets) in units.iter().zip(&pass_sets) {
        let unit_chunks = chunk_files(&unit.files, CHUNK_DIGEST_CHARS);
        for set in sets {
            let chunks = set.chunks_of(&unit_chunks);
            let batches = set.batches(batch_size);
            let group = run_passes_batch(
                llm,
                repo,
                repo_map,
                nonce,
                adopted,
                audit_model,
                job,
                &chunks,
                set.digests.as_deref(),
                &batches,
                &set.label,
                meter,
                set.advisory_disabled,
            )
            .await;
            match group {
                Ok((gf, gp, gr, gok, ge)) => {
                    all_findings.extend(gf);
                    all_proposed.extend(gp);
                    all_requested.extend(gr);
                    total_ok += gok;
                    failures.extend(ge);
                    groups_completed += 1;
                }
                // With `batch_default` off a failed submit, poll or fetch is fatal to its own
                // group (see `run_passes_batch`). Another group's replies are already paid for,
                // so the failure is this group's failed pass, not the whole scan's: it is
                // disclosed by name and the other groups' findings are kept. Only when NO group
                // completed (nothing was paid for, as with the single unrouted batch) does the
                // error propagate. A candidate pass set is its own group here.
                Err(error) => {
                    eprintln!(
                        "[camerata-server] batch mode: group {} failed: {error:#}",
                        set.label
                    );
                    failures.push(PassFailure::named(
                        format!("{} (the whole group)", set.label),
                        &format!("{error:#}"),
                    ));
                    first_error.get_or_insert(error);
                }
            }
        }
    }
    if groups_completed == 0 {
        if let Some(error) = first_error {
            return Err(error);
        }
    }
    Ok((
        all_findings,
        all_proposed,
        all_requested,
        total_ok,
        failures,
    ))
}

/// D7 — strip the corpus's `Rejected:` AUTHORING token from prose about to reach a client.
///
/// A `violates` option's `why` is interpolated verbatim into the defect detail a client
/// reads. The corpus's strongest authoring convention asks that `why` to open with the
/// literal token `Rejected:` — it forces the author to write a fault rather than a neutral
/// alternative, and the folder that adopted it scored 17 of 17 correct in the 244-rule
/// `violates` audit while others ran as low as 59 percent. The convention earns its keep at
/// authoring time and has no business in the deliverable.
///
/// Stripping at the render boundary rather than migrating the corpus is the owner's ruling
/// (2026-10-19): it costs one function, leaves all 71 existing uses correct, and avoids
/// touching 306 options to delete a token the authoring guide elsewhere calls a wart.
///
/// Only an OPENING token is removed, and the following clause is re-capitalised so the
/// sentence still reads. A `why` that merely discusses something being rejected keeps its
/// prose — see the test.
pub(super) fn strip_authoring_token(why: &str) -> String {
    /// The authoring tokens a `why` may open with. `Rejected:` is the documented convention
    /// (71 of 377 flagged options); `Avoided:` turned up in
    /// `PYTHON-TESTING-UNIT-LAYOUT-1 / scattered` during the 2026-10-20 rationale pass — the
    /// same idea in another author's hand. A LIST rather than one constant, because the next
    /// one will be neither of these and should cost one line to add rather than a bug report
    /// from a client who read it.
    const TOKENS: &[&str] = &["rejected:", "avoided:"];
    let trimmed = why.trim_start();
    let Some(token) = TOKENS
        .iter()
        .find(|t| trimmed.len() >= t.len() && trimmed[..t.len()].eq_ignore_ascii_case(t))
    else {
        return why.to_string();
    };
    let rest = trimmed[token.len()..].trim_start();
    let mut chars = rest.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Pass-1 W2 (A5.4, the audit findings tracker): decide what happens
/// to `effective_selected`'s directive entry for `rule_id`, given the OPTION that was DECIDED
/// for this rule this scan (forced by the operator, or model-recommended) — and, when that
/// option is the degenerate/absent form of the requirement, build the finding that must be
/// emitted instead of silently rewriting the directive to match it.
///
/// This is the single chokepoint both of `audit_repo`'s alternatives-block branches (the
/// operator-forced loop and the model-recommendation loop) route through, so the two paths can
/// never drift on what selecting a `violates` option actually does — the exact shape of the
/// shipped defect this closes (`ai_audit.rs` ~5885 / ~5921-5929, pre-fix) rewrote the directive
/// UNCONDITIONALLY for both paths.
///
/// - `option.violates == false` (the overwhelming majority of decisions: a genuine,
///   non-degenerate alternative): rewrites `effective_selected`'s directive entry for `rule_id`
///   to `option.directive`, exactly as before this fix — the per-chunk violation pass then
///   judges conformance against the newly-adopted directive. Returns `None` (no finding is
///   built here; any violation is the per-chunk pass's job, as always).
/// - `option.violates == true`: the directive is LEFT UNTOUCHED — `effective_selected` is not
///   modified at all, so the per-chunk pass keeps checking the rule's own pre-existing
///   directive (the project's actually-selected/default option), which means OTHER sites that
///   also happen to follow the defect can still be independently flagged by that pass. This
///   function's own return value is the GUARANTEED floor: a `Finding` carrying the rule's own
///   citation (`rule_id`) and remediation — `evaluated_option_id` is set to the violating
///   option's id, which `report_export::resolve_fix` already resolves into that option's
///   authored remediation text, exactly like any other multi-option finding; no remediation
///   text is duplicated here. Severity anchors at `"high"` (a confirmed, evidence-backed
///   determination that the repo follows the rule's own named defect, not a hedge) — a rule
///   author who judges that too aggressive for a specific rule can cap it via `max_severity`
///   (E2), which `report_export`/`xlsx_export` apply at export regardless of this anchor.
///   `located`/`path`/`line` come from `evidence` via `report_export::parse_evidence_citation`
///   when present (always true for a model-sourced recommendation — `parse_alternative_
///   recommendations` already forces `applicable = false` on any unevidenced claim, so this
///   function is never reached for one); an operator-forced pick carries no evidence by
///   design, so `located = false` there, and the pipeline's own unlocated-above-informational
///   safety net (`report_export::downgrade_unlocated_above_informational`) correctly demotes
///   that one shape to `info` rather than asserting an unverifiable "high" with nothing to
///   point at.
pub(super) fn apply_alternative_decision(
    effective_selected: &mut [crate::scan_routing::AuditRule],
    repo: &str,
    rule_id: &str,
    option: &RuleOptionView,
    recommendation_reasoning: &str,
    evidence: Option<&str>,
) -> Option<Finding> {
    if !option.violates {
        if let Some(entry) = effective_selected
            .iter_mut()
            .find(|r| r.id.eq_ignore_ascii_case(rule_id))
        {
            entry.directive = option.directive.clone();
        }
        return None;
    }
    let (path, line, located) = match evidence {
        Some(ev) if !ev.trim().is_empty() => {
            let (p, l) = crate::report_export::parse_evidence_citation(ev);
            // W21: `located` tracks whether this row has a real FILE LOCATION, so it
            // follows the parsed path rather than the mere presence of evidence. An
            // unparseable citation yields an empty path, and a row with no path is not
            // located no matter how much prose came with it.
            let anchored = !p.is_empty();
            (p, l, anchored)
        }
        _ => (String::new(), 0, false),
    };
    let snippet = evidence
        .filter(|e| !e.trim().is_empty())
        .map(|e| e.chars().take(200).collect::<String>())
        .unwrap_or_else(|| option.label.clone());
    let mut finding = Finding {
        repo: repo.to_string(),
        path,
        line,
        rule_id: rule_id.to_string(),
        severity: "high".to_string(),
        snippet,
        detail: format!(
            "This codebase was judged to follow \"{}\" for {rule_id} — {}. This option is the \
             absence/degenerate form of the rule's own requirement (marked `violates` in the \
             corpus), so it is reported as a defect rather than treated as a satisfied \
             alternative, even though the rule's own alternatives pass genuinely decided this \
             is what the repo does. {}",
            option.label,
            strip_authoring_token(&option.why),
            recommendation_reasoning
        ),
        located,
        evaluated_option_id: Some(option.id.clone()),
        // A5.6: the repo was judged to follow the rule's own named degenerate form — an
        // objective fault (the owner's Defect definition names this exact path), never a
        // consistency deviation.
        kind: Some(camerata_api_types::audit::FindingKind::Defect),
        ..Finding::default()
    };
    // `evidence` and the reasoning are model text.
    crate::onboard::redact::redact_finding(&mut finding);
    Some(finding)
}

/// Pass-1 W3 (A5.6/A5.2): reclassify every finding judged under a multi-option rule's
/// DECIDED, non-`violates` option as a [`FindingKind::Deviation`] — a consistency violation
/// against the convention the codebase was judged to follow — carrying the owner's canonical
/// sentence as structure: numerator (deviating sites), optional denominator (the model's
/// structured `sites_total`, display honesty only), and the observed convention's AUTHORED
/// option identity.
///
/// - The NUMERATOR is computed structurally right here (never model-counted): every surviving
///   row keyed to the rule counts one site, plus every absorbed merge site
///   (`Finding::also_locations`) that was originally flagged under the rule. A same-location
///   demotion into another row's `also_matches` is deliberately NOT counted (it has no site
///   record of its own) — the count can under-tell, never over-tell.
/// - The DENOMINATOR is `rec.assessed_sites`, raised to the numerator we actually counted when
///   the model's count is smaller (never a nonsense "3 of 2 sites", and never dropped: R1-4).
///   With no model count it stays `None`, "not counted".
/// - A rule whose decided option is `violates` is SKIPPED: its rows are defects (A5.4), not
///   consistency deviations. A `no_convention`/not-applicable recommendation is skipped too
///   (those rules have no decided convention to deviate from; their rows were hard-filtered).
///
/// Severity note (A5.2): this function does NOT touch severity. The deviation severity cap
/// (default `medium`, per-rule `deviation_ceiling` escape hatch) is applied at the export
/// choke-points (`report_export::clamp_deviation_severity`), the same layer the E2
/// `max_severity` clamp already lives at, so every artifact agrees.
pub(crate) fn assign_deviation_kinds(
    findings: &mut [Finding],
    recommendations: &[RuleRecommendation],
    alternatives: &[RuleAlternatives],
) {
    use camerata_api_types::audit::{DeviationKind, FindingKind};
    for rec in recommendations {
        if !rec.applicable || rec.no_convention {
            continue;
        }
        let Some(alt) = alternatives
            .iter()
            .find(|a| a.rule_id.eq_ignore_ascii_case(&rec.rule_id))
        else {
            continue;
        };
        let Some(opt) = alt
            .options
            .iter()
            .find(|o| o.id == rec.recommended_option_id)
        else {
            continue;
        };
        if opt.violates {
            // A5.4: the repo follows the rule's own named defect — every row under this rule
            // is (and stays) a Defect, never a consistency deviation.
            continue;
        }
        let deviating_sites: usize = findings
            .iter()
            .map(|f| {
                let own = usize::from(f.rule_id.eq_ignore_ascii_case(&rec.rule_id));
                let absorbed = f
                    .also_locations
                    .iter()
                    .filter(|l| l.rule_id.eq_ignore_ascii_case(&rec.rule_id))
                    .count();
                own + absorbed
            })
            .sum();
        if deviating_sites == 0 {
            continue;
        }
        // R1-4: a model count smaller than the sites we counted ourselves is raised to our
        // count (the denominator is at least the numerator), not dropped: dropping it left the
        // row with no denominator at all. No model count stays `None` (not counted).
        let assessed_sites = rec.assessed_sites.map(|total| total.max(deviating_sites));
        for f in findings
            .iter_mut()
            .filter(|f| f.rule_id.eq_ignore_ascii_case(&rec.rule_id))
        {
            f.kind = Some(FindingKind::Deviation(DeviationKind {
                deviating_sites,
                assessed_sites,
                convention_option_id: Some(opt.id.clone()),
                convention_label: opt.label.clone(),
            }));
        }
    }
}

/// Pass-1 W3 (A5.5): one [`FindingKind::ConventionObserved`] row per multi-option rule the
/// scan decided a genuine (non-`violates`) option for and found ZERO trace of a violation
/// under — the codebase consistently follows the convention. This set SHIPS in the product:
/// the audit derives the decisions, governance later enforces them.
///
/// "Zero trace" is deliberately the BROAD check: no surviving row keyed to the rule, no row
/// listing it in `also_matches` (a same-location demotion), and no absorbed merge site
/// (`also_locations`) originally flagged under it — a convention is only claimed "observed
/// with zero violations" when literally nothing in the final row set evidences one.
///
/// Informational by construction: severity `info`, routed to the informational bucket by the
/// existing severity machinery; `located` only when the recommendation carried a real
/// evidence citation (always true for a model-sourced recommendation; an operator-forced
/// pick carries none by design).
pub(crate) fn conventions_observed_findings(
    repo: &str,
    findings: &[Finding],
    recommendations: &[RuleRecommendation],
    alternatives: &[RuleAlternatives],
) -> Vec<Finding> {
    use camerata_api_types::audit::{ConventionObservedKind, FindingKind};
    let mut out = Vec::new();
    for rec in recommendations {
        if !rec.applicable || rec.no_convention {
            continue;
        }
        // W21/W22 — the row asserts "the codebase consistently follows X". Only the model's
        // OWN affirmative answer to that exact question licenses it.
        //
        // `rec.applicable` was doing this job and cannot: its contract is "already follows
        // this rule's concern OR concretely needs it", so it is `true` for a codebase that
        // conspicuously LACKS the thing the rule governs. That is how hearth shipped "The
        // codebase consistently follows 'Services own the transaction lifecycle...' - zero
        // violations were observed" about a repo the same paragraph described as having "no
        // transaction management yet" and "no existing pattern".
        //
        // `None` is refused along with `Some(false)`, deliberately. Every other optional
        // model field here fails OPEN so a rule is never silently dropped over a missing
        // key, and this one is the exception because the two directions are not symmetrical:
        // a missed convention row costs the client a paragraph they would have found mildly
        // interesting, while a wrongly-emitted one puts a false statement about their own
        // codebase in front of them over our name. The burden of evidence belongs on the
        // claim, not on its refutation.
        if rec.currently_followed != Some(true) {
            continue;
        }
        let Some(alt) = alternatives
            .iter()
            .find(|a| a.rule_id.eq_ignore_ascii_case(&rec.rule_id))
        else {
            continue;
        };
        let Some(opt) = alt
            .options
            .iter()
            .find(|o| o.id == rec.recommended_option_id)
        else {
            continue;
        };
        if opt.violates {
            continue;
        }
        let any_trace = findings.iter().any(|f| {
            f.rule_id.eq_ignore_ascii_case(&rec.rule_id)
                || f.also_matches
                    .iter()
                    .any(|m| m.eq_ignore_ascii_case(&rec.rule_id))
                || f.also_locations
                    .iter()
                    .any(|l| l.rule_id.eq_ignore_ascii_case(&rec.rule_id))
        });
        if any_trace {
            continue;
        }
        let (path, line, located) = match rec.evidence.as_deref() {
            Some(ev) if !ev.trim().is_empty() => {
                let (p, l) = crate::report_export::parse_evidence_citation(ev);
                (p, l, true)
            }
            _ => (String::new(), 0, false),
        };
        let snippet: String = rec
            .evidence
            .clone()
            .unwrap_or_else(|| opt.label.clone())
            .chars()
            .take(200)
            .collect();
        let mut finding = Finding {
            repo: repo.to_string(),
            path,
            line,
            rule_id: rec.rule_id.clone(),
            severity: "info".to_string(),
            snippet,
            detail: format!(
                "The codebase consistently follows \"{}\" for {}. Zero violations were \
                 observed this scan. Reported so the convention can be adopted as an explicit \
                 decision and enforced going forward. {}",
                opt.label, rec.rule_id, rec.recommendation_reasoning
            ),
            located,
            evaluated_option_id: Some(opt.id.clone()),
            kind: Some(FindingKind::ConventionObserved(ConventionObservedKind {
                convention_option_id: opt.id.clone(),
                convention_label: opt.label.clone(),
            })),
            ..Finding::default()
        };
        crate::onboard::redact::redact_finding(&mut finding);
        out.push(finding);
    }
    out
}

/// Pass-1 W3 (A5.3): exactly ONE [`FindingKind::NoConvention`] informational row per rule
/// whose recommendation says "no convention adopted" — NEVER N violations. The one-row
/// guarantee is structural, in three layers:
/// 1. the rule was dropped from the directive set before any violation pass ran
///    (`audit_repo`'s recommendation loop), so no chunk is ever asked about it;
/// 2. the post-calibration hard filter dropped any finding the model free-associated under
///    it anyway (`no_convention_rule_filtered`);
/// 3. this function keys on the per-rule recommendation record and dedupes by rule id, so N
///    rows for one rule cannot be synthesized even from a malformed recommendations vec.
pub(crate) fn no_convention_findings(
    repo: &str,
    recommendations: &[RuleRecommendation],
) -> Vec<Finding> {
    use camerata_api_types::audit::{FindingKind, NoConventionKind};
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for rec in recommendations {
        if !rec.no_convention || !rec.applicable {
            continue;
        }
        if !seen.insert(rec.rule_id.to_ascii_uppercase()) {
            continue;
        }
        let split: String = rec
            .observed_options
            .iter()
            .map(|o| match o.sites {
                Some(n) => format!("{} ({} site{})", o.label, n, if n == 1 { "" } else { "s" }),
                None => o.label.clone(),
            })
            .collect::<Vec<_>>()
            .join(" / ");
        let mut finding = Finding {
            repo: repo.to_string(),
            path: String::new(),
            line: 0,
            rule_id: rec.rule_id.clone(),
            severity: "info".to_string(),
            snippet: if split.is_empty() {
                "no convention adopted".to_string()
            } else {
                split
            },
            detail: format!(
                "No convention has been adopted for {}: the codebase is inconsistent with no \
                 clear majority among the rule's alternatives. One informational row is \
                 reported instead of per-site violations — adopting one option is the \
                 decision to make; future drift then becomes an enforceable deviation. {}",
                rec.rule_id, rec.recommendation_reasoning
            ),
            located: false,
            kind: Some(FindingKind::NoConvention(NoConventionKind {
                observed_options: rec.observed_options.clone(),
            })),
            ..Finding::default()
        };
        crate::onboard::redact::redact_finding(&mut finding);
        out.push(finding);
    }
    out
}

/// Pass-1 W3 (A5.6): one [`FindingKind::NotAssessed`] honesty row per rule that was ARMED
/// for this scan but never evaluated because its routing group matched zero files (see
/// `run_routed_passes`'s `skipped_rules` out-param). Distinguishable, by structured kind,
/// from "assessed and clean" — the exact distinction the owner's taxonomy demands.
pub(crate) fn not_assessed_findings(repo: &str, skipped: &[(String, String)]) -> Vec<Finding> {
    use camerata_api_types::audit::{FindingKind, NotAssessedKind, NotAssessedReason};
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (rule_id, note) in skipped {
        if !seen.insert(rule_id.to_ascii_uppercase()) {
            continue;
        }
        let mut finding = Finding {
            repo: repo.to_string(),
            path: String::new(),
            line: 0,
            rule_id: rule_id.clone(),
            severity: "info".to_string(),
            snippet: note.clone(),
            detail: format!(
                "{rule_id} was armed for this scan but could not be evaluated: {note}. \
                 Reported as not-assessed rather than silently reading as clean."
            ),
            located: false,
            kind: Some(FindingKind::NotAssessed(NotAssessedKind {
                reason: NotAssessedReason::NoFilesMatched,
                reason_detail: note.clone(),
            })),
            ..Finding::default()
        };
        crate::onboard::redact::redact_finding(&mut finding);
        out.push(finding);
    }
    out
}
