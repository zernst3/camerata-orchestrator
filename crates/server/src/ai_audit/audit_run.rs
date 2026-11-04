//! The audit entry point that ties the scan, merge, alternatives and calibration stages together for one repository.

use super::*;

/// Run the AI architectural audit for one repo. Returns the findings + proposed rules.
///
/// The repo is audited in CONTEXT-SIZED CHUNKS (see `chunk_files`): a real repo is far too
/// large for one model context (a 2.8M-char repo is ~700k tokens vs a 200k window), and the
/// old single-digest path silently fed the model only the first ~10% of files — so a
/// blatant violation in a later file produced zero findings purely because the file was
/// never in the input. Every chunk is audited against the full ruleset and the findings are
/// aggregated. A model/transport failure on a chunk is noted and the audit continues, so a
/// single bad pass never discards the others.
///
/// ## Rule-routing (Lever 2)
///
/// When `selected` contains language-scoped rules (e.g. `RUST-*`, `REACT-*`), `audit_repo`
/// groups them by [`crate::scan_routing::Scope`] via [`crate::scan_routing::plan_routes`] and
/// runs each group against ONLY the files that group's language matches. Cross-cutting groups
/// (`Scope::All`) see every file; a `RUST-` group sees only `.rs` files, etc.
///
/// ### Advisory-pass interaction
///
/// The advisory "flag novel issues beyond the adopted rules" task is gated to `bi==0` in
/// `run_passes` so novel issues are not re-flagged under N invented names across N rule-batches
/// of the same chunk. Routing adds a second dimension: if we ran advisory in every language
/// group WHILE an All group also exists, a `.rs` file would get advisory in the rust group AND
/// the All group — bringing back the duplicate-novel-finding problem. The safe wiring
/// (W2-amended — see `run_routed_passes`'s `has_all_group`):
///
/// - When a cross-cutting **All group** exists, it alone runs with advisory **enabled**:
///   novel issues are discovered once, against every file, on the first batch of each chunk;
///   every **language group** runs with `advisory_disabled = true` so it never re-triggers the
///   advisory pass over files the All group already covers.
/// - When NO All group exists at all (a purely language-scoped rule selection — e.g. a project
///   that selected only `RUST-*` rules), advisory stays **enabled** in every language group
///   instead of going dark for the whole scan: `Scope::Language` groups are mutually exclusive
///   by file (`scan_routing::file_language` maps each file to at most one language), so this
///   cannot duplicate a finding, and it is the ONLY way such a scan ever gets the model's
///   backstop "is there anything wrong here beyond the listed rules?" pass at all. Before this
///   fix, a language-only rule selection silently suppressed novel-issue discovery for the
///   entire scan — the semantic-tier's security backstop role never fired, indistinguishable
///   from "nothing to report."
///
/// Net: novel findings appear exactly once per file chunk that belongs to any group, language-
/// scoped rules skip unmatched files, and no finding is missed.
///
/// ### Declared scope (`applies_to`)
///
/// A rule may also declare `applies_to` in its TOML (file globs and languages). With the
/// `applies_to_routing` efficiency switch on, such a rule is grouped under its declared scope
/// (intersected with its id-prefix language, if any) and sent only with the files that scope
/// covers; a file whose language cannot be determined still gets it. With the switch off the
/// field is ignored and only the id-prefix routing above applies.
///
/// ### Batch mode
///
/// The Batch execution path honours the SAME plan: [`run_routed_passes_batch`] reads the same
/// [`runnable_units`] as the real-time path and submits one Message Batch per route group, so
/// both modes send the same (chunk, rule) pairs.
#[allow(clippy::too_many_arguments)]
pub async fn audit_repo(
    llm: &dyn LlmPort,
    repo: &str,
    files: &[(String, String)],
    // The rules to audit, with the routing metadata the corpus declares for each
    // (`AuditRule::applies_to`). Rule metadata, not `(id, directive)` pairs, so the next piece
    // of per-rule routing metadata is one more field on `AuditRule` and not a signature change.
    selected: &[crate::scan_routing::AuditRule],
    // MULTI-OPTION semantic rules present in `selected` for this repo (built by the caller
    // from the loaded corpus — see `onboard::build_rule_alternatives`). Empty when this
    // project/repo has no multi-option semantic rules selected, or when the caller is a
    // context (deep-tier, resolution round) that doesn't apply this feature.
    alternatives: &[RuleAlternatives],
    // Operator-forced picks for a targeted `rescan-alternatives` call: rule id (uppercased)
    // -> the option id the operator chose. A rule id present here SKIPS the model
    // recommendation pass entirely (the operator already decided) and its directive is
    // rewritten straight to the forced option's directive. Empty for a normal full scan.
    forced: &std::collections::HashMap<String, String>,
    model: Option<&str>,
    calibration_model: Option<&str>,
    mode: ScanMode,
    thorough: bool,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    job: Option<(&crate::jobs::JobStore, &str)>,
    meter: Option<&UsageMeter>,
    // The full repo file set to build the repo MAP from, when it differs from `files`. On an
    // incremental scan `files` is only the CHANGED bodies, but the repo map should still cover
    // the WHOLE repo so cross-file rules keep their architectural view. `None` → use `files`.
    map_files: Option<&[(String, String)]>,
) -> anyhow::Result<(
    Vec<Finding>,
    Vec<ProposedRule>,
    Vec<RuleRecommendation>,
    Vec<FailedPass>,
    Vec<crate::scan_ledger::StageSample>,
)> {
    if files.is_empty() {
        return Ok((Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()));
    }
    // Redact BEFORE any code is sent to a model (docs/design/2026-10-23, section 5). Everything
    // below (the digest, the repo map, the fix context window, line anchoring) reads this model-
    // facing set: same paths, same line numbers, every credential value replaced by its redacted
    // form, so a `path:line` the model cites still resolves and a snippet it quotes still anchors.
    let mut model_files = crate::onboard::redact::files_for_model(files);
    let mut model_map_files = map_files.map(crate::onboard::redact::files_for_model);
    // H5: one canonical input order. The file walk follows the filesystem's directory order and
    // the rule selection its caller's, and either reorders every prompt (and so the answer) when
    // it changes. Path order for files, id order for rules, so the same inputs always produce
    // the same prompts. Stable sorts: equal keys keep their relative order.
    model_files.sort_by(|a, b| a.0.cmp(&b.0));
    if let Some(m) = model_map_files.as_mut() {
        m.sort_by(|a, b| a.0.cmp(&b.0));
    }
    let mut ordered_selected = selected.to_vec();
    ordered_selected.sort_by(|a, b| a.id.cmp(&b.id));
    let selected = ordered_selected.as_slice();
    let files = model_files.as_slice();
    let map_files = model_map_files.as_deref();
    // W1: this call's own merge/calibration stage samples, returned alongside the normal
    // result (mirroring the EXISTING `Vec<FailedPass>` accumulation pattern already used
    // throughout this function) rather than threading a `&mut ScanLedger` through this
    // already-large parameter list — see `crate::scan_ledger::StageSample`'s doc comment.
    let mut stage_samples: Vec<crate::scan_ledger::StageSample> = Vec::new();
    // The delimiter token for every model call of this audit (alternatives, scan and resolution
    // passes). One per run, so the passes keep sharing their cached prefix; see `crate::prompt_frame`.
    // Drawn so that it occurs in no path or file body the run frames (the repo map's set included).
    let nonce = crate::prompt_frame::FrameNonce::generate_avoiding_files(&[
        files,
        map_files.unwrap_or(files),
    ]);
    // Key findings to the adopted rule ids (so a violation shows under e.g.
    // ARCH-STRICT-LAYERING-1, not an invented AI- name).
    let adopted: std::collections::HashSet<String> =
        selected.iter().map(|r| r.id.to_ascii_uppercase()).collect();
    // Model selection: the USER's per-audit choice wins; else the built-in default. ALWAYS resolved here, to the same id the pre-flight priced and the
    // ledger records (`resolve_audit_model`). It used to stay `None` when nothing was pinned, on
    // the theory that the transport's own default would then serve both scan and calibration;
    // but that default was the PROCESS default (an environment variable, now removed), which the pre-flight, the
    // ledger key, the batch path and the spend limit's price check never consult. A run then
    // reported one model and called another, and when the other was unpriced the spend limit
    // could not count its calls and refused the rest of the run (2026-10-29).
    let audit_model: Option<String> = Some(resolve_audit_model(model));
    // Cross-file context for every chunk (which dirs are which layer, where types live): the
    // complete file tree leads every call, and the symbols of a call's chunk files and their
    // neighbours follow it. On an incremental scan this is built from the whole repo, not just the
    // changed files. The tree budget follows the scan model's window, so one run has one tree.
    let repo_map = RepoMap::new(
        map_files.unwrap_or(files),
        &nonce,
        repo_map::tree_budget_for_window(
            audit_model
                .as_deref()
                .map_or(request_budget::DEFAULT_CONTEXT_WINDOW_TOKENS, |m| {
                    request_budget::context_window_tokens(m)
                }),
        ),
    );
    // Calibration model: the user's calibration pick wins; else the SCAN model, so the audit is end to end on one model by default (no silent
    // default-model calibration). The UI exposes this as its own picker. The same resolution
    // the pre-flight lists the calibration model by.
    let calib_model: Option<String> = Some(resolve_calibration_model(
        calibration_model,
        audit_model.as_deref().unwrap_or_default(),
    ));

    // ── Multi-option semantic rules: decide (or accept the operator's forced choice of) ONE
    // option per rule, then REWRITE that rule's entry in `effective_selected` to the decided
    // option's directive. Downstream (routing, chunking, the per-chunk violation passes) is
    // then COMPLETELY UNCHANGED from the pre-existing single-directive pipeline — every chunk
    // checks the SAME directive for a given rule, which is what guarantees "violations are
    // always relative to ONE chosen alternative" (the design doc's coherence principle) without
    // any cross-chunk coordination. See `recommend_alternatives`'s doc comment for the chunk-0
    // grounding tradeoff.
    let mut effective_selected: Vec<crate::scan_routing::AuditRule> = selected.to_vec();
    let mut recommendations: Vec<RuleRecommendation> = Vec::new();
    // W6: any pass that fails/times out for this repo, captured so the export can disclose it
    // explicitly instead of only logging to stderr. See `FailedPass`'s doc comment.
    let mut failed_passes: Vec<FailedPass> = Vec::new();
    // M1: a file tree too large to send in full is a coverage limitation. It is recorded in the
    // run's telemetry and disclosed in the report's Coverage, never silent.
    if let Some(fallback) = repo_map.tree_fallback() {
        if let Some(m) = meter {
            m.record_tree_fallback(crate::stage_telemetry::RepoTreeFallback {
                repo: repo.to_string(),
                files: fallback.files,
                tree_tokens: fallback.tree_tokens,
                budget: fallback.budget,
            });
        }
        failed_passes.push(FailedPass {
            repo: repo.to_string(),
            pass: "full file list in model calls".to_string(),
            reason: fallback.disclosure(),
            files_not_reviewed: 0,
            class: crate::ai_audit::FailureClass::Content,
            files: Vec::new(),
            attempts: Vec::new(),
            scope_note: true,
        });
    }
    // Pass-1 W2 (A5.4): one `Finding` per rule whose DECIDED option (forced or model-
    // recommended) is marked `violates` — see `apply_alternative_decision`'s doc comment. These
    // are spliced into the finding set AFTER calibration (below, past the P7 not-applicable
    // filter) rather than fed through the per-chunk violation prompt or the LLM calibration
    // round-trip: the decision that the repo follows the defect option has ALREADY been made,
    // by this same alternatives pass: re-asking a second model call to "confirm" it would be
    // redundant spend for no new information, and this emission is the GUARANTEED floor the
    // rule must never fall below regardless of whether any other pass also catches the same
    // site independently (over-telling a duplicate row is explicitly preferred over the
    // silent-zero defect this whole mechanism exists to fix).
    let mut violates_findings: Vec<Finding> = Vec::new();
    if !alternatives.is_empty() {
        let (forced_alts, ask_alts): (Vec<RuleAlternatives>, Vec<RuleAlternatives>) = alternatives
            .iter()
            .cloned()
            .partition(|a| forced.contains_key(&a.rule_id));
        // Operator-forced picks: no model call, validate the forced id is real, fall back to
        // the currently-selected/default option (flagged) when it is not.
        for a in &forced_alts {
            let requested_id = forced.get(&a.rule_id).cloned().unwrap_or_default();
            let (final_id, hallucinated) = if a.options.iter().any(|o| o.id == requested_id) {
                (requested_id, false)
            } else {
                let fb = a
                    .selected_option_id
                    .clone()
                    .or_else(|| a.options.first().map(|o| o.id.clone()))
                    .unwrap_or_default();
                (fb, true)
            };
            recommendations.push(RuleRecommendation {
                repo: String::new(),
                rule_id: a.rule_id.clone(),
                recommended_option_id: final_id.clone(),
                recommendation_reasoning: if hallucinated {
                    "The requested option id is not valid for this rule; kept the previously \
                     selected option instead."
                        .to_string()
                } else {
                    "Operator-selected option for a targeted rescan.".to_string()
                },
                hallucinated,
                operator_chosen: true,
                // An operator's explicit forced pick needs no repo evidence — they already
                // decided the rule applies (that's what forcing a choice for it means).
                evidence: None,
                applicable: true,
                // An operator's forced pick IS the convention decision — never no-convention,
                // and no model counting happened on this path.
                no_convention: false,
                observed_options: Vec::new(),
                assessed_sites: None,
                currently_followed: None,
            });
            if let Some(opt) = a.options.iter().find(|o| o.id == final_id) {
                if let Some(f) = apply_alternative_decision(
                    &mut effective_selected,
                    repo,
                    &a.rule_id,
                    opt,
                    "Operator-selected option for a targeted rescan.",
                    None,
                ) {
                    violates_findings.push(f);
                }
            }
        }
        // Everything else: ask the model to recommend, in ONE dedicated pass covering every
        // rule left. Fail-soft — a failed recommendation pass leaves those rules at their
        // pre-existing (project-selected/default) directive, exactly today's behavior, rather
        // than aborting the whole audit.
        // SPEND CAP: a stage that would start after the cap was reached is skipped and recorded
        // as NOT RUN; its rules keep their pre-existing directive, as when the pass fails.
        let alternatives_blocked = !ask_alts.is_empty()
            && meter.is_some_and(|m| m.stage_blocked(repo, STAGE_ALTERNATIVES));
        if !ask_alts.is_empty() && !alternatives_blocked {
            match recommend_alternatives(
                llm,
                repo,
                files,
                map_files.unwrap_or(files),
                &nonce,
                &ask_alts,
                audit_model.as_deref(),
                feedback,
                meter,
            )
            .await
            {
                Ok((recs, unfinished)) => {
                    // Rules a cut-off reply never reached keep their default directive (they
                    // have no entry in `recs`); the run says so rather than saying nothing.
                    if let Some(reason) = unfinished {
                        failed_passes.push(FailedPass {
                            repo: repo.to_string(),
                            pass: "rule-alternative recommendations".to_string(),
                            reason,
                            files_not_reviewed: 0,
                            class: crate::ai_audit::FailureClass::Content,
                            files: Vec::new(),
                            attempts: Vec::new(),
                            scope_note: false,
                        });
                    }
                    for rec in &recs {
                        let Some(a) = ask_alts.iter().find(|a| a.rule_id == rec.rule_id) else {
                            continue;
                        };
                        if !rec.applicable {
                            // P7: no evidence this rule's concern applies to this codebase at
                            // all (no pagination anywhere, no API versioning anywhere, …) —
                            // drop it from the directive set the violation passes actually
                            // check, so it emits ZERO findings instead of checking violations
                            // against a convention nothing in the repo established.
                            effective_selected.retain(|r| !r.id.eq_ignore_ascii_case(&a.rule_id));
                            continue;
                        }
                        if rec.no_convention {
                            // Pass-1 W3 (A5.3): no convention adopted — the codebase is
                            // inconsistent with no clear majority, so judging per-site
                            // violations against ANY pick would fabricate N violations out of
                            // an unmade decision. Sever the per-site path structurally: the
                            // rule leaves the directive set (so no chunk pass ever asks about
                            // it), the post-calibration hard filter below drops any stray
                            // finding the model free-associates under it anyway, and exactly
                            // ONE informational `no_convention` row is synthesized from this
                            // recommendation (see `no_convention_findings`).
                            effective_selected.retain(|r| !r.id.eq_ignore_ascii_case(&a.rule_id));
                            continue;
                        }
                        if let Some(opt) =
                            a.options.iter().find(|o| o.id == rec.recommended_option_id)
                        {
                            if let Some(f) = apply_alternative_decision(
                                &mut effective_selected,
                                repo,
                                &a.rule_id,
                                opt,
                                &rec.recommendation_reasoning,
                                rec.evidence.as_deref(),
                            ) {
                                violates_findings.push(f);
                            }
                        }
                    }
                    recommendations.extend(recs);
                }
                Err(e) => {
                    eprintln!(
                        "[camerata-server] alternative-recommendation pass failed for {repo}: {e}"
                    );
                    // W6: never a SILENT omission — the export must say this pass did not run.
                    failed_passes.push(FailedPass {
                        repo: repo.to_string(),
                        pass: "rule-alternative recommendations".to_string(),
                        reason: e.to_string(),
                        files_not_reviewed: 0,
                        class: classify_pass_error(&e),
                        files: Vec::new(),
                        attempts: camerata_llm::failure::attempts_of(&e),
                        scope_note: false,
                    });
                }
            }
        }
    }

    // Mode is the speed/scale knob: Sequential = 1 call per chunk with all rules; Parallel =
    // rule-batches × file-chunks run concurrently; Batch = one Anthropic Message Batch at
    // 50% discount, reassembled by custom_id.
    let (concurrency, batch_size) = mode.tuning(efficiency_of(meter));

    // Pass-1 W3 (A5.6): rules whose routing group matched zero files — see
    // `run_routed_passes`'s `skipped_rules` doc comment. Real-time and batch mode both fill it
    // (they share `runnable_units`), so both disclose the same skipped rules.
    let mut skipped_rules: Vec<(String, String)> = Vec::new();

    // ── Rule-routing plan ───────────────────────────────────────────────────────────
    // Group `effective_selected` rules by scope so each group only audits the files its scope
    // covers: the rule id's language prefix, and (with the `applies_to_routing` switch on) the
    // rule's declared `applies_to`. Real-time and batch mode execute the SAME plan.
    let route_plan = crate::scan_routing::plan_routes(
        &effective_selected,
        files,
        efficiency_of(meter).applies_to_routing,
    );
    if route_plan.saved_fraction() > 0.0 {
        eprintln!(
            "[camerata-server] rule-routing: {:.0}% input reduction for {repo} ({} groups, {} rules routed)",
            route_plan.saved_fraction() * 100.0,
            route_plan.groups.len(),
            effective_selected.len(),
        );
    }

    // ── Dispatch to the appropriate execution engine ─────────────────────────────────
    let (mut all_findings, mut all_proposed, requested, ok_passes, pass_failures) =
        if mode == ScanMode::Batch {
            // Batch path: submit every route group's (chunk × rule-batch) pairs as a Message
            // Batch, poll to completion, reassemble by custom_id. The job's add_total is called
            // inside run_passes_batch (it knows the full item count before any network I/O).
            //
            // The Message-Batches path is concrete-only (API-key-gated; `submit_batch` et al.
            // are not part of the minimal `LlmPort` seam), so recover the concrete `&Llm` via
            // the `as_any` downcast. In production this is always a real `Llm`, so the downcast
            // always succeeds and the behavior is unchanged. A non-`Llm` completer (a test stub)
            // can only drive the non-batch real-time path; batch mode is not reachable for it.
            // A `BatchSeam` (a test double carrying its own `BatchApi`) is the one other
            // completer that can drive the batch path.
            let batch_api: &dyn batch_mode::BatchApi =
                if let Some(seam) = llm.as_any().downcast_ref::<batch_mode::BatchSeam>() {
                    seam.api()
                } else {
                    llm.as_any().downcast_ref::<Llm>().ok_or_else(|| {
                        anyhow::anyhow!(
                            "batch mode requires the concrete Llm client (the Message-Batches \
                             API is not part of the LlmPort seam); use parallel/sequential mode \
                             with a custom completer"
                        )
                    })?
                };
            run_routed_passes_batch(
                batch_api,
                repo,
                files,
                &effective_selected,
                &route_plan,
                &repo_map,
                &nonce,
                &adopted,
                audit_model.as_deref(),
                job,
                batch_size,
                meter,
                &mut skipped_rules,
                candidate_passes::CandidateEnv::production(),
            )
            .await?
        } else {
            // ── Real-time path (parallel or sequential) with rule-routing ────────────────
            //
            // When routing produces only one group (all rules are cross-cutting, or no rules at
            // all), the loop runs once with no difference from the old single-pass behavior.
            // When routing produces multiple groups, each group runs its rules over its own
            // (smaller) file subset. Advisory is enabled only in the All group.
            run_routed_passes(
                llm,
                repo,
                files,
                &effective_selected,
                &route_plan,
                &repo_map,
                &nonce,
                &adopted,
                audit_model.as_deref(),
                feedback,
                job,
                concurrency,
                batch_size,
                meter,
                &mut skipped_rules,
                candidate_passes::CandidateEnv::production(),
            )
            .await
        };

    // A rule is only "not assessed" when the WHOLE repo has no file in its scope: on an
    // incremental scan `files` is just the changed files.
    drop_skips_with_matches_in_full_set(
        &mut skipped_rules,
        &route_plan,
        map_files.unwrap_or(files),
    );

    // Every pass failed -> surface the error so the caller notes the AI audit was skipped
    // (the deterministic findings still return independently). Each pass already finalized
    // its own transcript status, so the UI spinner stops regardless.
    // A cut-off reply that still yielded complete findings is not "nothing came back": those
    // findings are kept and the failure is disclosed as a failed pass below (H1).
    if ok_passes == 0 && all_findings.is_empty() {
        if let Some(last) = pass_failures.last() {
            return Err(anyhow::anyhow!(
                "all {} audit pass(es) failed; last: {}: {}",
                pass_failures.len(),
                last.pass,
                last.error
            ));
        }
    } else {
        // PARTIAL failure: some passes answered, others did not (a timeout, an HTTP error, or
        // a model REFUSAL, `stop_reason: "refusal"`). Those passes' findings are missing from
        // this result, so the export must say so, naming every failed pass, rather than
        // present the survivors as the whole audit.
        failed_passes.extend(failed_pass_rows(repo, ok_passes, &pass_failures));
    }

    // ── Resolution round ────────────────────────────────────────────────────────────
    // Earlier passes may have DEFERRED a judgment because it needed the bodies of files
    // not in that pass (the residual cross-body limit of chunking). Pull exactly those
    // files together and re-audit once — so a cross-file rule the model couldn't decide
    // in a single pass gets resolved instead of silently missed. SINGLE round (the
    // resolution passes' own needs_files are ignored) to keep it bounded.
    //
    // The resolution round runs the FULL selected rule set against the requested files (no
    // per-rule routing) so no cross-file deferred judgment is inadvertently skipped. Advisory
    // is enabled (default) since this is an independent pass over a small file set.
    let requested_norm: std::collections::HashSet<String> = requested
        .iter()
        .map(|p| normalize_requested_path(p))
        .collect();
    let resolution: Vec<(String, String)> = files
        .iter()
        .filter(|(p, _)| requested_norm.contains(&normalize_requested_path(p)))
        .cloned()
        .collect();
    // M6: a file a pass asked for but that cannot be put in front of the model is DISCLOSED, not
    // forgotten: the judgment that wanted it stays unresolved and the report says so.
    {
        let have: std::collections::HashSet<String> = files
            .iter()
            .map(|(p, _)| normalize_requested_path(p))
            .collect();
        let in_repo: std::collections::HashSet<String> = map_files
            .unwrap_or(files)
            .iter()
            .map(|(p, _)| normalize_requested_path(p))
            .collect();
        let mut missing: Vec<&String> = requested_norm
            .iter()
            .filter(|p| !p.is_empty() && !have.contains(*p))
            .collect();
        missing.sort();
        let (unchanged, unknown): (Vec<&String>, Vec<&String>) =
            missing.into_iter().partition(|p| in_repo.contains(*p));
        let name_list = |v: &[&String]| {
            let shown: Vec<&str> = v.iter().take(10).map(|s| s.as_str()).collect();
            let more = v.len().saturating_sub(shown.len());
            if more > 0 {
                format!("{} and {more} more", shown.join(", "))
            } else {
                shown.join(", ")
            }
        };
        if !unknown.is_empty() {
            failed_passes.push(FailedPass {
                repo: repo.to_string(),
                pass: "deferred judgment".to_string(),
                reason: format!(
                    "requested files not in scan: {}. A rule that needed them could not be \
                     judged.",
                    name_list(&unknown)
                ),
                files_not_reviewed: 0,
                class: crate::ai_audit::FailureClass::Content,
                files: Vec::new(),
                attempts: Vec::new(),
                scope_note: false,
            });
        }
        if !unchanged.is_empty() {
            failed_passes.push(FailedPass {
                repo: repo.to_string(),
                pass: "deferred judgment".to_string(),
                reason: format!(
                    "requested files not re-read in this run (unchanged since the last \
                     inspection): {}. A rule that needed them was not re-judged.",
                    name_list(&unchanged)
                ),
                files_not_reviewed: 0,
                class: crate::ai_audit::FailureClass::Content,
                files: Vec::new(),
                attempts: Vec::new(),
                scope_note: false,
            });
        }
    }
    if !resolution.is_empty() {
        let effective_pairs = crate::scan_routing::rule_pairs(&effective_selected);
        let batches_res: Vec<&[(String, String)]> = if effective_pairs.is_empty() {
            vec![&effective_pairs]
        } else {
            effective_pairs.chunks(batch_size.max(1)).collect()
        };
        let res_chunks = chunk_files(&resolution, CHUNK_DIGEST_CHARS);
        // BUG-4 fix: in Batch mode, run_passes_batch already called add_total with the
        // full batch item count and the batch's inc_done calls bring done to that value.
        // A second add_total here would inflate the denominator AFTER the batch completes,
        // causing the UI progress bar to temporarily drop from 100% back to a lower value.
        // In Batch mode we skip the add_total for the resolution round; the resolution
        // items' inc_done calls will push done slightly past the total, which clamps at
        // 100% on the UI side and is far less disruptive than the denominator-inflate glitch.
        // In non-Batch mode the main passes pre-seed the total via run_routed_passes and the
        // resolution items legitimately extend the denominator.
        if mode != ScanMode::Batch {
            if let Some((jstore, jid)) = job {
                jstore.add_total(jid, res_chunks.len() * batches_res.len());
            }
        }
        // Resolution always runs on the real-time parallel engine (even in batch mode):
        // the resolution set is small (typically 1-5 files) and the polling overhead of a
        // separate batch submission outweighs the marginal discount.
        let res_concurrency = if mode == ScanMode::Batch {
            PARALLEL_CONCURRENCY
        } else {
            concurrency
        };
        let (rf, rp, _rn, res_ok, res_failures) = run_passes(
            llm,
            repo,
            &repo_map,
            &nonce,
            &adopted,
            audit_model.as_deref(),
            feedback,
            job,
            &res_chunks,
            None,
            &batches_res,
            res_concurrency,
            "resolution",
            &format!("audit-{repo}-res"),
            meter,
            false, // advisory enabled: novel findings in resolution files are wanted
        )
        .await;
        all_findings.extend(rf);
        all_proposed.extend(rp);
        // The resolution round's failures are disclosed the same way as the scan passes': a
        // refused or timed-out resolution pass yields zero findings for the files it was to
        // re-judge, which must never read as "resolved clean". The main round answered, so even
        // a total resolution failure is a disclosure row, not an `Err`.
        failed_passes.extend(failed_pass_rows(repo, res_ok, &res_failures));
    }

    // Resolve each finding's line DETERMINISTICALLY from its verbatim snippet before dedup,
    // so the model's unreliable line counting can't (a) mislocate a finding or (b) defeat the
    // location merge. The model says WHAT (the snippet); plain code finds WHERE.
    resolve_finding_lines(&mut all_findings, files);

    // Cross-chunk dedup + cross-name LOCATION MERGE: the shared repo map means the same
    // issue can surface in more than one pass. Step 1 drops byte-identical
    // (path, line, rule_id) repeats. Step 2 collapses rows that are mechanically the same site
    // (the same rule family, the same file, the same place); every collapsed source stays on
    // the survivor in `flagged_by`. A row the model named differently at the same line is NOT
    // collapsed: it stays its own row and the grouping pass links it. This is N-in / M-out
    // (M <= N) with every dropped row accounted for, not the calibration pass's N-in/N-out
    // scoring.
    // A10: rows the scan parser marked thrown out (placeholder text) are kept apart from the rows
    // that go through the merges, calibration and the fix stage, and rejoin the result at the end.
    let mut parked: Vec<Finding> = Vec::new();
    {
        let (thrown_out, kept) =
            crate::engine_discard::split_discarded(std::mem::take(&mut all_findings));
        all_findings = kept;
        parked.extend(thrown_out);
    }
    {
        let rows_in = all_findings.len();
        dedup_findings_keep_max_severity(&mut all_findings);
        let rows_after_dedup = all_findings.len();
        crate::review_tree::mark_located(&mut all_findings, files);
        let collapse = crate::review_tree::collapse_identical(
            std::mem::take(&mut all_findings),
            &crate::review_tree::ToolMap::builtin(),
        );
        let mut acc = collapse.accounting();
        acc.deduped = rows_in.saturating_sub(rows_after_dedup);
        all_findings = collapse.rows;
        let rows_out = all_findings.len();
        stage_samples.push(crate::scan_ledger::StageSample {
            stage: "ai-location-merge".to_string(),
            rows_in,
            rows_out,
            accounting: acc,
            model: None,
        });
        let mut seen_p = std::collections::HashSet::new();
        all_proposed.retain(|p| seen_p.insert(p.id.clone()));
    }

    // RANK 4 `terse_output`: the scan asked for a one-sentence `detail`; the survivors of the
    // first merge get their full detail written NOW, before calibration, because calibration
    // (its prompt, the precondition check, the D5 floor and D6 ceiling), the cross-family merge
    // that runs after this function and the fix self-check all decide from the explanation.
    // The collapse above reads no `detail`, so it ran on the short one. A finding whose
    // detail could not be written stays, marked, and is reported as a failed pass. See
    // `detail_regen`'s module comment for the per-consumer table.
    if meter.is_none() {
        warn_terse_needs_meter(repo);
    }
    if terse_output_of(meter) {
        // The whole redacted file set, not just the files this run scanned: on an incremental
        // run `files` is only the changed ones, and a statement can name any file of the repo.
        let (regenerated, regen_failed) = regenerate_full_details(
            llm,
            repo,
            std::mem::take(&mut all_findings),
            map_files.unwrap_or(files),
            &crate::scan_routing::rule_pairs(&effective_selected),
            &nonce,
            audit_model.as_deref(),
            meter,
        )
        .await;
        all_findings = regenerated;
        failed_passes.extend(regen_failed);
    }

    // Calibration pass over ALL aggregated findings: recalibrate severity + flag
    // low-confidence findings. It does NOT drop anything — recall-first discovery hands
    // every finding to the architect. Skipped entirely when there's nothing to calibrate
    // (no findings → no point spending a round-trip).
    //
    // This pass runs AFTER every chunk×rule pass has reported "done", and it's a single
    // synchronous round-trip over all findings — so without its own visible agent the UI
    // showed every pass "done" while the spinner kept turning for another minute.
    // `verify_findings` registers its OWN transcript agent (real prompt, real output —
    // see its doc comment) so the cockpit shows "calibrating N findings" with actual
    // content instead of a mystery hang or an empty placeholder. (Dedup/merge also
    // shrinks N, so this round is now faster too.)
    let pre_calibration_len = all_findings.len();
    // `--thorough` (three-pass consensus) is removed; the flag is ignored for one release with a
    // notice, and stage 2's recorded disagreement is the second opinion.
    if let Some(notice) = thorough_notice(thorough) {
        eprintln!("[camerata-server] {notice}");
    }
    // Whether stage 1 ran, so the ledger names its model only when a model was asked.
    let mut calibration_ran = false;
    let verified = if all_findings.is_empty()
        || meter.is_some_and(|m| m.stage_blocked(repo, STAGE_CALIBRATION))
    {
        // Nothing to calibrate, or the spend cap was reached (recorded as NOT RUN): the
        // findings go on exactly as the scan produced them, none dropped.
        all_findings
    } else {
        calibration_ran = true;
        // Repo-shape line for the proportionality signal (Bug 4 §2b): detected stack + code-file
        // count, from signals already computed. detect_stack is the same one grounding uses.
        let stack = crate::onboard::detect_stack(repo, files);
        let repo_shape = if stack.frameworks.is_empty() {
            format!("This repository has {} code files.", files.len())
        } else {
            format!(
                "This is a {} repo with {} code files.",
                stack.frameworks.join(", "),
                files.len()
            )
        };
        let calibration_rules = crate::scan_routing::rule_pairs(&effective_selected);
        let (verified, estimation_failed) = verify_findings(
            llm,
            repo,
            all_findings,
            calib_model.as_deref(),
            feedback,
            meter,
            &Stage1Inputs::new(&repo_shape)
                .with_files(map_files.unwrap_or(files))
                .with_rules(&calibration_rules)
                .with_concurrency(concurrency)
                .with_batch(mode == ScanMode::Batch)
                .with_job(job),
        )
        .await;
        // C3-1b: disclose a run-wide estimation-pass failure the same way the
        // alternative-recommendation pass already does — see `FailedPass`'s doc comment.
        failed_passes.extend(estimation_failed);
        verified
    };
    let mut verified = verified;
    // P7: a rule marked NOT APPLICABLE (no evidence its concern applies to this codebase at
    // all) must emit ZERO findings. Dropping it from `effective_selected` earlier stops the
    // per-chunk prompt from asking about it at all, but this hard filter is the actual
    // guarantee — it also catches the rare case where a model free-associates a violation
    // under that rule id anyway (the prompt's "flag any other genuine issues you find"
    // catch-all), whether cited under the bare adopted id or the `AI-` invented-name prefix.
    let mut dropped_not_applicable = 0usize;
    let mut dropped_no_convention = 0usize;
    if !recommendations.is_empty() {
        let not_applicable: std::collections::HashSet<String> = recommendations
            .iter()
            .filter(|r| !r.applicable)
            .map(|r| r.rule_id.to_ascii_uppercase())
            .collect();
        if !not_applicable.is_empty() {
            for f in verified.iter_mut() {
                let bare = f.rule_id.trim_start_matches("AI-").to_ascii_uppercase();
                if not_applicable.contains(&bare) && !crate::engine_discard::is_discarded(f) {
                    crate::engine_discard::mark(f, crate::engine_discard::RULE_NOT_APPLICABLE);
                    dropped_not_applicable += 1;
                }
            }
        }
        // Pass-1 W3 (A5.3): the no-convention twin of the P7 filter above, with its own
        // accounting label. Dropping the rule from `effective_selected` upstream stops the
        // prompts from asking about it; THIS is the structural guarantee that no stray
        // free-associated finding under a no-convention rule can ever ship as a violation —
        // exactly ONE informational row (synthesized below, AFTER this filter) represents
        // the rule instead.
        let no_convention: std::collections::HashSet<String> = recommendations
            .iter()
            .filter(|r| r.no_convention)
            .map(|r| r.rule_id.to_ascii_uppercase())
            .collect();
        if !no_convention.is_empty() {
            for f in verified.iter_mut() {
                let bare = f.rule_id.trim_start_matches("AI-").to_ascii_uppercase();
                if no_convention.contains(&bare) && !crate::engine_discard::is_discarded(f) {
                    crate::engine_discard::mark(f, crate::engine_discard::NO_CONVENTION);
                    dropped_no_convention += 1;
                }
            }
        }
    }
    // D1: the detail and calibration steps write "discard this finding" into a placeholder row's
    // explanation; act on it for the rows the model invented, and account for the removal below.
    let dropped_discarded = mark_discarded_invented_rows(
        repo,
        &mut verified,
        &crate::engine_discard::RestoredKeys::default(),
    );
    // A10: the rows thrown out above are KEPT, but apart from the rows that go on: the stages
    // below (the option splice, kind assignment, the convention emissions) must not see them. The
    // calibration stage accounts for them as leaving it (`other` below); they rejoin the returned
    // set at the end, still marked, and the caller keeps them apart from the rows that ship.
    let (thrown_out, kept_after_calibration) =
        crate::engine_discard::split_discarded(std::mem::take(&mut verified));
    verified = kept_after_calibration;
    parked.extend(thrown_out);
    // W1: calibration itself never drops a row (see this block's own doc comment above); the
    // ONLY row-count reductions between `pre_calibration_len` and here are the P7
    // not-applicable filter, the A5.3 no-convention filter and the D1 discard filter just
    // above, so those are the explicit dispositions this stage accounts for.
    {
        let rows_out = verified.len();
        let mut acc = crate::scan_ledger::StageAccounting::default();
        if dropped_discarded > 0 {
            acc.other
                .push(("discarded_placeholder_row".to_string(), dropped_discarded));
        }
        if dropped_not_applicable > 0 {
            acc.other.push((
                "not_applicable_rule_filtered".to_string(),
                dropped_not_applicable,
            ));
        }
        if dropped_no_convention > 0 {
            acc.other.push((
                "no_convention_rule_filtered".to_string(),
                dropped_no_convention,
            ));
        }
        stage_samples.push(crate::scan_ledger::StageSample {
            stage: "ai-calibration".to_string(),
            rows_in: pre_calibration_len,
            rows_out,
            accounting: acc,
            // The resolved stage 1 model, recorded only when the stage asked a model. It comes
            // from the `--calibration-model` flag or the project setting, else the scan model.
            model: calibration_ran.then(|| calib_model.clone()).flatten(),
        });
    }
    // Pass-1 W2 (A5.4): splice in the `violates`-option findings AFTER calibration (so this
    // scan never spends a second LLM round re-confirming a determination the alternatives pass
    // already made) and BEFORE the `evaluated_option_id` blanket-tag below, so each one still
    // gets that tag applied (redundantly-but-harmlessly, since `apply_alternative_decision`
    // already set it) rather than needing a parallel code path. Recorded as its own ADDITION-
    // side ledger stage (`StageAccounting::push_added`) — the exact precedent `report_export::
    // apply_evidence_gate`'s `"evidence-gate-synthesis"` stage set for the W1 seal fix: a row
    // appended after the row set would otherwise have been considered final must disclose
    // itself, never silently grow the finding count.
    if !violates_findings.is_empty() {
        let rows_in = verified.len();
        verified.extend(violates_findings);
        let rows_out = verified.len();
        let mut acc = crate::scan_ledger::StageAccounting::default();
        acc.push_added("violates_option_emitted", rows_out - rows_in);
        stage_samples.push(crate::scan_ledger::StageSample {
            stage: "violates-option-emission".to_string(),
            rows_in,
            rows_out,
            accounting: acc,
            model: None,
        });
    }
    // Tag every finding under a multi-option rule with the option it was actually judged
    // against — the report layer (`report_export::resolve_fix`) reads this so the Fix text
    // matches the evaluated option, never a stale default. Every finding for a given rule_id
    // in THIS response was checked against the SAME directive (the rewrite above), so this is
    // a safe blanket tag, not a per-finding guess.
    if !recommendations.is_empty() {
        let by_rule: std::collections::HashMap<&str, &str> = recommendations
            .iter()
            // W3 (A5.3): a no-convention rule was judged against NO option — its
            // `recommended_option_id` is a shape-filling fallback, never an evaluation
            // target, so it must not blanket-tag anything.
            .filter(|r| !r.no_convention)
            .map(|r| (r.rule_id.as_str(), r.recommended_option_id.as_str()))
            .collect();
        for f in verified.iter_mut() {
            if let Some(id) = by_rule.get(f.rule_id.to_ascii_uppercase().as_str()) {
                f.evaluated_option_id = Some((*id).to_string());
            }
        }
    }
    // ── Pass-1 W3 (A5.6): kind assignment + the three informational emissions ─────────
    // Runs strictly AFTER the hard filters and the violates-option splice, so (a) every row
    // that will ship is present to be classified, and (b) the zero-violation check behind
    // `convention_observed` sees the final row set. Each synthesized emission is recorded as
    // its own ADDITION-side ledger stage — the exact precedent the A5.4 violates-option
    // splice set: a row appended after the set would otherwise be final must disclose
    // itself, never silently grow the finding count.
    assign_deviation_kinds(&mut verified, &recommendations, alternatives);
    for (stage, rows) in [
        (
            "convention-observed-emission",
            conventions_observed_findings(repo, &verified, &recommendations, alternatives),
        ),
        (
            "no-convention-emission",
            no_convention_findings(repo, &recommendations),
        ),
        (
            "not-assessed-emission",
            not_assessed_findings(repo, &skipped_rules),
        ),
    ] {
        if rows.is_empty() {
            continue;
        }
        let rows_in = verified.len();
        verified.extend(rows);
        let rows_out = verified.len();
        let mut acc = crate::scan_ledger::StageAccounting::default();
        acc.push_added(stage, rows_out - rows_in);
        stage_samples.push(crate::scan_ledger::StageSample {
            stage: stage.to_string(),
            rows_in,
            rows_out,
            accounting: acc,
            model: None,
        });
    }
    // A10: the rows thrown out along the way come back marked, after the rows that ship.
    verified.extend(parked);
    Ok((
        verified,
        all_proposed,
        recommendations,
        failed_passes,
        stage_samples,
    ))
}
