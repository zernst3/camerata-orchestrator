//! The scan results component.

use super::*;

/// Renders one brownfield scan's results: the audit summary, the findings table,
/// and the proposed-rules table. Keyed by the parent so a new scan remounts the
/// chorale tables with fresh rows.
#[component]
pub(in crate::cockpit) fn ScanResults(report: ScanReportView) -> Element {
    // Phase 2 audit result (findings against the selected rules); None until the
    // architect picks rules and runs the audit.
    let mut audit = use_signal(|| Option::<ScanReportView>::None);
    let mut auditing = use_signal(|| false);
    // The model the user picks for the audit — they own the thoroughness/speed trade-off.
    // Company-agnostic list comes from the server (`/api/models`); seed from its default.
    let models_res = use_resource(fetch_audit_models);
    let models = models_res.read().clone().flatten();
    // Fetch the active project so the Audit + Calibration selectors read their initial
    // value from `project.step_models` — the same source Settings writes to. This makes
    // both screens bidirectionally synced: a change here persists to step_models (via the
    // shared endpoint), so it shows in Settings next time; a change in Settings shows here
    // next time the onboarding scan mounts (because we seed from step_models, not from the
    // models-registry default).
    let active_project_res = use_resource(fetch_active_project);
    let active_project = active_project_res.read().clone().flatten();
    // Derive the project id once (empty string = no active project; the persist calls are
    // no-ops in that case since the endpoint requires a real id). Memoised as a String so
    // the onchange closures can clone it cheaply.
    let project_id = active_project
        .as_ref()
        .map(|p| p.id.clone())
        .unwrap_or_default();
    // Seed priority: project.step_models.audit (Settings-authoritative) → models.default
    // (registry fallback for first-time / no-project state). The guard is `is_empty()` so
    // a user actively choosing a different model in THIS session is never overwritten by a
    // later async re-render of the active-project resource.
    let mut audit_model = use_signal(String::new);
    if audit_model().is_empty() {
        let from_step = active_project
            .as_ref()
            .map(|p| p.step_models.audit.clone())
            .filter(|s| !s.is_empty());
        let from_default = models
            .as_ref()
            .map(|m| m.default.clone())
            .filter(|s| !s.is_empty());
        if let Some(m) = from_step.or(from_default) {
            audit_model.set(m);
        }
    }
    // Calibration model — its OWN picker (severity recalibration + confidence tagging).
    // Seed priority: project.step_models.calibration (Settings-authoritative) → current
    // audit_model (so "the model you picked" is used end-to-end unless deliberately split).
    let mut calibration_model = use_signal(String::new);
    if calibration_model().is_empty() {
        let from_step = active_project
            .as_ref()
            .map(|p| p.step_models.calibration.clone())
            .filter(|s| !s.is_empty());
        let from_audit = Some(audit_model()).filter(|s| !s.is_empty());
        if let Some(m) = from_step.or(from_audit) {
            calibration_model.set(m);
        }
    }
    // Scan mode (user-facing): "auto" (default: the server picks batch on the API backend),
    // "parallel" / "sequential" (forced real time), "batch" (forced batch), or "job" (async:
    // submit, walk away, poll). Job sends "auto", so it batches on the API backend too.
    // AUTO-SELECTED by the scan's scale; the user can override.
    let recommended_mode = recommend_scan_mode(&report);
    let mut audit_mode = use_signal(|| recommended_mode.clone());
    // Thorough calibration (#51): opt-in, costs more AI. Off by default.
    // Full scan: when ON, ignore the incremental cache and re-audit every file. Off by default
    // (so re-scans are incremental — only changed files cost AI tokens). The first scan of a
    // project is full regardless (no cache yet).
    let mut audit_full_scan = use_signal(|| false);
    // Spend limit (USD): blank = none. The run stops between passes once actual spend reaches
    // it and lists what it did not run (the server twin of `camerata inspect --max-cost-usd`).
    let mut audit_spend_limit =
        use_signal(|| camerata_ui_core::scan::DEFAULT_SPEND_LIMIT_TEXT.to_string());
    // Deep compliance & security tier (#55): opt-in, the most expensive tier.
    // Runs three extra whole-repo passes (SOC-2 gap analysis, deep security audit,
    // threat model) after the standard audit and attaches the results as `report.deep`.
    // Output is ADVISORY — never a SOC-2 report or a penetration test.
    let mut audit_deep = use_signal(|| false);
    // Scan-type selector (Part C): which scans to run. Both default ON (today's behaviour).
    // "AI architectural review" = the LLM scan of architectural/structured/prose rules (and
    // the deep tier). "Deterministic scans" = the always-on security floor + the mechanical
    // preview linters — fast, no LLM, no tokens. Deselecting AI sends `run_ai_review=false`
    // (the server makes zero model calls); deselecting deterministic skips the floor + preview.
    let mut run_ai_review = use_signal(|| true);
    let mut run_deterministic = use_signal(|| true);
    // pw/cockpit-ui Feature 5: feature-flag map. Controls per-feature affordances —
    // SOC-2 section visibility, deep-export scope. Fetched once on mount; degrades
    // gracefully (all flags default to false) when the server is old.
    let feature_flags_res = use_resource(fetch_feature_flags);
    let feature_flags = feature_flags_res.read().clone().unwrap_or_default();
    // Live progress for an async job: (passes done, passes total, findings so far).
    let mut job_progress = use_signal(|| Option::<(usize, usize, usize, usize)>::None);
    // Live DETERMINISTIC-pass progress (floor + preview tools), rendered above the AI
    // agent-activity drawer. Primary progress view in deterministic-only mode (no AI drawer).
    let mut det_progress = use_signal(|| Option::<DetProgressView>::None);
    // Background-job FAILURE message (silent-death fix): set by `poll_job` when a job ends
    // `failed` or vanishes mid-poll. Rendered by `ScanJobFailedBanner`, UNCONDITIONALLY —
    // not gated on a report existing, since a failed job never produces one.
    let mut job_failed_message = use_signal(|| Option::<String>::None);
    // The in-flight async job id (app-scope, survives navigation). RESUME: if a job was
    // already running when this view (re)mounted, re-attach the poll instead of losing it.
    let mut active_audit_job = use_context::<Signal<Option<String>>>();
    // The audited repos whose source was deleted from this machine: audit, apply and open-PR
    // all need the source on disk, so they switch off and say why.
    let scan_blocked = crate::source_ui::use_scan_block()
        .and_then(|b| camerata_ui_core::readiness::scan_block_for(&b, &report.repos));
    let scan_idle_ms = use_signal(|| Option::<u128>::None);
    // Captured HERE, in the component's render body, where `LoadingCount` context resolution is
    // guaranteed — never resolved from inside `poll_job`'s own body, which runs detached inside
    // this `use_future`'s async block (see the BOMBE-SILENT-MISS note on `poll_job`).
    let loading_count = use_context::<crate::loading::LoadingCount>();
    // "Run what remains": the plan is fetched first and shown with its estimate; confirming
    // starts the resume job and polls it with the Bombe held for the whole run.
    let mut resume_plan_shown = use_signal(|| Option::<ResumePlanView>::None);
    let on_plan = EventHandler::new(move |run_id: String| {
        spawn(async move {
            match resume_plan(&run_id).await {
                Ok(plan) => resume_plan_shown.set(Some(plan)),
                Err(why) => resume_plan_shown.set(Some(ResumePlanView {
                    text: String::new(),
                    refusal: Some(why),
                })),
            }
        });
    });
    let on_resume = EventHandler::new(move |run_id: String| {
        resume_plan_shown.set(None);
        job_failed_message.set(None);
        auditing.set(true);
        spawn(async move {
            let result = run_what_remains(
                loading_count,
                || async { resume_start(&run_id).await },
                |jid| async move {
                    active_audit_job.set(Some(jid.clone()));
                    poll_job(
                        jid,
                        audit,
                        auditing,
                        job_progress,
                        det_progress,
                        active_audit_job,
                        scan_idle_ms,
                        job_failed_message,
                        loading_count,
                    )
                    .await;
                },
            )
            .await;
            if let Err(why) = result {
                auditing.set(false);
                job_failed_message.set(Some(why));
            }
        });
    });
    use_future(move || async move {
        if let Some(jid) = active_audit_job.peek().clone() {
            auditing.set(true);
            poll_job(
                jid,
                audit,
                auditing,
                job_progress,
                det_progress,
                active_audit_job,
                scan_idle_ms,
                job_failed_message,
                loading_count,
            )
            .await;
        }
    });
    // Selected-rule count, set by ProposedRulesTable and read here for the cost estimate
    // (the estimate also depends on the model + mode pickers, which live in this component).
    let selected_count = use_signal(|| 0usize);
    use_context_provider(|| selected_count);

    // Per-repo rule selection. For a multi-repo scan the architect views ONE repo's rule
    // table at a time (the single-select below) and each repo keeps its own picks. This
    // lifted `repo -> selected rule ids` map is the source of truth the per-repo tables seed
    // from and write back to, so switching repos preserves each repo's selection and one
    // audit covers every repo against its own rules. Empty for a single-repo scan (the table
    // then behaves as the original whole-set table).
    //
    // PRE-SEED every repo with its recommended rules so a repo the architect never opens
    // still audits against a sensible default set (not just the always-on security floor).
    let repo_seed = {
        let mut m = std::collections::HashMap::<String, Vec<String>>::new();
        if report.repos.len() > 1 {
            for repo in &report.repos {
                let ids: Vec<String> = report
                    .proposed_rules
                    .iter()
                    .filter(|r| {
                        r.effective_auto_recommended() && r.repos.iter().any(|rp| rp == repo)
                    })
                    .map(|r| r.id.clone())
                    .collect();
                m.insert(repo.clone(), ids);
            }
        }
        m
    };
    let repo_selection = use_signal(|| repo_seed);
    // Shared so the custom-rules panel can auto-select a newly created rule for its repo(s).
    use_context_provider(|| repo_selection);
    // Which repo's rule table is in view. Defaults to the first scanned repo. Provided as
    // context so the rule-detail modal + the table key the per-repo `chosen` map by it.
    // Per-repo sizes for the estimate (several repos only): fetched once per mount.
    let repo_sizes = {
        let repos = report.repos.clone();
        use_resource(move || {
            let repos = repos.clone();
            async move {
                if repos.len() > 1 {
                    fetch_repo_sizes(&repos).await
                } else {
                    None
                }
            }
        })
    };
    let viewed_repo = use_signal(|| report.repos.first().cloned().unwrap_or_default());
    use_context_provider(|| viewed_repo);
    let mut viewed_repo = viewed_repo;
    let multi_repo = report.repos.len() > 1;
    let audited = audit.read().clone();
    let findings: Vec<FindingView> = audited
        .as_ref()
        .map(|a| a.findings.clone())
        .unwrap_or_default();

    // "High severity" stat covers the top two tiers (critical + high) so the exploitable
    // criticals are never invisible in the summary.
    let high = findings
        .iter()
        .filter(|f| f.severity == "critical" || f.severity == "high")
        .count();
    let enforced = findings.iter().filter(|f| f.status == "active").count();
    let suppressed = findings.len().saturating_sub(enforced);

    // The architect's PER-REPO alternative choices, keyed `chosen_key(repo, rule_id)` ->
    // option id. Per-repo so picking an alternative for a rule in one repo doesn't change
    // another repo's choice. Seeded with each rule's default for every scanned repo.
    let chosen = use_signal(|| {
        let mut m = std::collections::HashMap::<String, String>::new();
        for repo in &report.repos {
            for r in &report.proposed_rules {
                if let Some(d) = &r.default_option {
                    m.insert(chosen_key(repo, &r.id), d.clone());
                }
            }
        }
        m
    });
    use_context_provider(|| chosen);

    // User-authored custom rules (Custom + Custom Global), shared via context so the table, the
    // create/edit/delete modal, and the audit/arm closures all read/write the same list. Seeded
    // from the active project's existing custom rules (so re-opening shows them); the draft
    // restore below overlays any in-flight onboarding additions.
    let custom_rules = use_signal(Vec::<CustomRuleView>::new);
    use_context_provider(|| custom_rules);

    // Per-rule repo placement OVERRIDE (rule id -> repos it installs into). Starts EMPTY:
    // an entry exists only when the architect explicitly overrides a rule's target repos.
    // With no entry, arm falls back to the per-repo SELECTION — i.e. the rule installs into
    // exactly the repos whose table checked it (matching what the audit scans). Seeding this
    // with each rule's scan binding (the old behavior) made the override always-present, so
    // arm ignored the per-repo selection and pushed every "available" rule to all repos.
    // (There's no placement-editor UI yet; this map is the seam for one.)
    let placement = use_signal(std::collections::HashMap::<String, Vec<String>>::new);
    use_context_provider(|| placement);

    // The open row-detail rule, shared with ProposedRulesTable. Provided HERE (not in
    // the table) so the modal renders at this subtree's root, outside the chorale
    // table — see RuleDetailModal / ProposedRulesTable for the reopen-bug rationale.
    let detail_rule = use_signal(|| Option::<ProposedRuleView>::None);
    use_context_provider(|| detail_rule);

    // The open finding (row-click) — same host-outside-the-table pattern, shared with
    // FindingsTable. The modal shows the violated rule's directive + the full detail.
    let mut detail_finding = use_signal(|| Option::<FindingView>::None);
    use_context_provider(|| detail_finding);
    // The code viewer's two shared pieces: the file cache (last 8 files, so it outlives a switch
    // between triage tables) and the request the finding modal's "Open in viewer" button writes.
    use_context_provider(|| Signal::new(camerata_ui_core::viewer::ViewerCache::new()));
    let viewer_request = super::viewer::ViewerRequest(use_signal(|| Option::<String>::None));
    use_context_provider(|| viewer_request);
    // Where the Queue registers what the viewer needs (see `FindingsTable`).
    use_context_provider(|| super::viewer::ViewerSlot(Signal::new(None)));
    // The same for the stacked group view (work order B.2).
    use_context_provider(|| super::stack::StackSlot(Signal::new(None)));
    use_context_provider(|| super::palette::PaletteSlot(Signal::new(None)));

    // rule id -> what it enforces (the chosen/default alternative's directive, else
    // the rule title), for the findings-table rule-id hover.
    let descriptions: std::collections::HashMap<String, String> = report
        .proposed_rules
        .iter()
        .map(|r| {
            let picked = chosen
                .read()
                .get(&chosen_key(&viewed_repo(), &r.id))
                .cloned()
                .or_else(|| r.default_option.clone());
            let desc = picked
                .and_then(|oid| {
                    r.options
                        .iter()
                        .find(|o| o.id == oid)
                        .map(|o| o.directive.clone())
                })
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| r.title.clone());
            (r.id.clone(), desc)
        })
        .collect();

    let descriptions_modal = descriptions.clone();

    // ── Triage state (issue #26) ──────────────────────────────────────────────
    // Each finding lives in one of three tables: Unresolved (the default), Ignored, or
    // Tech debt. The architect moves findings between them until nothing is Unresolved, then
    // Processes the ignored + tech-debt buckets. State is LOCAL until Process.
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    // The shared scan state (reset to None to "start over") + the cockpit view (so
    // "Complete onboarding" can switch the tab to Governed Development).
    let mut onboard_scan = use_context::<Signal<Option<ScanReportView>>>();
    let mut view = use_context::<Signal<CockpitView>>();
    // When the draft last auto-saved (shown with a check), the two-click "start over"
    // arm, and the in-flight "complete onboarding" flag.
    let mut last_saved = use_signal(|| Option::<String>::None);
    let mut restart_arm = use_signal(|| false);
    let mut finishing = use_signal(|| false);
    let dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
    let mut triage_view = use_signal(|| TriageState::Unresolved);
    // How the Queue is cut (work order B.4). Saved in the draft beside `triage_view`, so a
    // reload comes back to the same cut. Lifted here so it outlives the table, which remounts
    // on a triage-view switch.
    let order_preset = use_signal(OrderPreset::default);
    // THE REVIEW TIMER (INTERNAL ONLY, step 7a): per-group active time and the per-row reviewed
    // record. Created here so it outlives the table, which remounts on a triage-view switch; its
    // log is saved in the draft beside the dispositions. The bridge starts with it and stops when
    // this view unmounts.
    let review = super::review::use_review_handle();
    // The draft's auto-save goes through one in-flight slot (`save_onboarding_draft_ordered`).
    let save_queue = use_hook(|| {
        std::rc::Rc::new(std::cell::RefCell::new(
            camerata_ui_core::save_queue::SaveQueue::<OnboardingDraft>::default(),
        ))
    });
    super::review::use_review_clock(&review);
    // The viewer and the stacked view draw the timer's Pause / Resume control too (they cover the
    // strip), and find the one timer here.
    use_context_provider(|| review.clone());
    let mut stats_open = use_signal(|| false);
    // Which kind surface is on screen (Pass 2 step 1). Session state, deliberately NOT
    // persisted in the draft: it is a lens, not a decision, and the curator always wants to
    // resume on the Queue. Adding it to `OnboardingDraft` would change a persisted wire shape
    // for no curator benefit.
    let mut kind_view = use_signal(|| KindSurface::Queue);
    // The "Patterns across services" tab of a multi-repo project. It is an observation, not a
    // kind of finding, so it is a lens of its own beside the kind switch and not a fifth kind.
    let mut patterns_open = use_signal(|| false);
    // The rule library, for the titles the patterns tab prints (the report prints titles too).
    let pattern_titles_res = use_resource(fetch_corpus_rules);
    let pattern_titles = move || {
        pattern_titles_res
            .read()
            .as_ref()
            .and_then(|loaded| loaded.as_ref())
            .map(|rules| {
                rules
                    .iter()
                    .map(|r| (r.id.clone(), r.title.clone()))
                    .collect::<std::collections::HashMap<String, String>>()
            })
    };
    // THE RULE HUB (design §8): which (repo, rule) the hub is open on, and the Queue jump it
    // can perform. Both session-local lenses like `kind_view`, and deliberately not persisted
    // for the same reason: an open modal is not a decision.
    let mut hub_open = use_signal(|| Option::<RuleHubRequest>::None);
    // The code viewer opens the hub with `H`, disarms its keys while it is up and takes the
    // keyboard back when it closes, so it needs to see this signal.
    use_context_provider(|| super::viewer::ViewerHubOpen(hub_open));
    // Closing it gives the keyboard back to where it was opened from: the viewer (which does
    // that itself) or the detail pane.
    super::viewer::use_hub_focus_return(hub_open, try_use_context::<super::viewer::ViewerSlot>());
    let mut queue_rule_filter = use_signal(|| Option::<String>::None);
    let mut processing = use_signal(|| false);
    // ── THE GATED CONVENTION COMMIT (design §3; Q3 + Q4) ──────────────────────
    // The curator's adopt/skip state, his openness record, and his `NoConvention` picks. One
    // model serves both informational panels because a pick and an adoption write the same
    // thing by the same path (Q3).
    //
    // Session state, like `kind_view` and `hub_open`, and deliberately NOT in the draft. The
    // openness record in particular must not be restored: "I opened this one" is a claim
    // about THIS sitting, and a stale one would quietly shrink the number the whole gate
    // exists to show.
    let mut adoption = use_signal(AdoptionModel::new);
    let mut commit_dialog = use_signal(|| Option::<CommitDialogState>::None);
    let mut unopened_only = use_signal(|| false);
    let mut committing = use_signal(|| false);

    // ── Auto-save / restore the onboarding draft (issue #27) ──────────────────
    // On mount, rehydrate this scan's audit / selection / dispositions / view from the saved
    // draft (only when it's the SAME scan). The `draft_loaded` gate keeps the save effect from
    // overwriting the draft with initial (un-rehydrated) state before the restore runs.
    let mut audit_w = audit;
    let mut repo_selection_w = repo_selection;
    let dispositions_w = dispositions;
    let mut chosen_w = chosen;
    let mut custom_rules_w = custom_rules;
    let mut order_preset_w = order_preset;
    let mut draft_loaded = use_signal(|| false);
    // The write-through bridge to the server's decision store. `bridge_ready` is set once the
    // store has been read (and the old draft's dispositions migrated); until then, and whenever
    // the store could not be reached, the draft carries every disposition itself so nothing is
    // lost. Once ready, the draft carries only the changes the store does not hold yet.
    let decision_bridge = use_hook(|| {
        std::rc::Rc::new(std::cell::RefCell::new(
            camerata_ui_core::decisions::DecisionBridge::default(),
        ))
    });
    // `None` while the store is still being read: the queue shows as it always did.
    let decision_availability =
        use_signal(|| Option::<camerata_ui_core::decisions::Availability>::None);
    let decision_banner = use_signal(|| Option::<&'static str>::None);
    let bridge_pid = use_signal(String::new);
    let mut bridge_tick = use_signal(|| 0u32);
    // Changes the store has refused, in words naming the site: shown until they are stored.
    let mut sync_errors = use_signal(Vec::<String>::new);
    // The sites in words (`path:line`), for those errors.
    let mut site_labels = use_signal(std::collections::HashMap::<String, String>::new);
    use_context_provider(|| DecisionSync {
        bridge: decision_bridge.clone(),
        project_id: bridge_pid,
        sites: site_labels,
    });
    use_context_provider(|| draft_loaded);
    {
        let report_repos = report.repos.clone();
        let review_restore = review.clone();
        let decision_bridge_load = decision_bridge.clone();
        use_future(move || {
            let report_repos = report_repos.clone();
            let review_restore = review_restore.clone();
            let decision_bridge_load = decision_bridge_load.clone();
            async move {
                let draft = load_onboarding_draft().await;
                // The draft's dispositions: old-style ones to migrate, or changes the store did
                // not hold yet when it was saved. Only a draft of THIS scan counts.
                let draft_queue = draft
                    .as_ref()
                    .filter(|d| d.scan.repos == report_repos)
                    .map(|d| camerata_ui_core::decisions::DraftQueue {
                        pending: d.dispositions.clone(),
                        last_known: d.last_known.clone(),
                        stamps: d.pending_stamps.clone(),
                    })
                    .unwrap_or_default();
                if let Some(d) = draft {
                    if d.scan.repos == report_repos {
                        if d.review_log.just_failed {
                            // Part of the saved review-time record could not be read. What could
                            // be read is loaded, and the original is kept verbatim in the draft.
                            eprintln!("[camerata-ui] part of the saved review log could not be read; the original is kept in the draft");
                            crate::toast::push_toast(
                                toasts,
                                crate::toast::ToastKind::Warning,
                                "Part of the saved review-time record could not be read. The rest was loaded, and the original is kept untouched in your saved review.",
                            );
                        }
                        review_restore.restore(d.review_log.clone());
                        if d.audit.is_some() {
                            audit_w.set(d.audit);
                        }
                        if !d.repo_selection.is_empty() {
                            repo_selection_w.set(d.repo_selection);
                        }
                        if !d.chosen.is_empty() {
                            chosen_w.set(d.chosen);
                        }
                        if !d.custom.is_empty() {
                            custom_rules_w.set(d.custom);
                        }
                        if !d.viewed_repo.is_empty() {
                            viewed_repo.set(d.viewed_repo);
                        }
                        triage_view.set(d.triage_view);
                        order_preset_w.set(d.order_preset);
                    }
                }
                // The decisions live on the server. Read them (migrating an old draft's once),
                // show them with the draft's not-yet-stored changes over them, and set the
                // banner if the store cannot be read.
                apply_decision_load(
                    draft_queue,
                    decision_bridge_load.clone(),
                    dispositions_w,
                    decision_availability,
                    decision_banner,
                    bridge_pid,
                )
                .await;
                draft_loaded.set(true);
            }
        });
    }
    {
        let report = report.clone();
        let review_save = review.clone();
        let save_queue = save_queue.clone();
        let decision_bridge_save = decision_bridge.clone();
        use_effect(move || {
            // Track every persisted slice so the effect re-runs on any change.
            let audit_v = audit.read().clone();
            let sel = repo_selection.read().clone();
            let cho = chosen.read().clone();
            let cust = custom_rules.read().clone();
            let disp = dispositions.read().clone();
            let vr = viewed_repo();
            let tv = triage_view();
            let op = order_preset();
            let availability = decision_availability();
            let _ = bridge_tick();
            // The review log as last flushed by the timer (decisions, group changes, pauses and
            // every 30 seconds of work). Reading the handle's signal is what re-runs this on a
            // flush; the timer's own state is not a signal.
            let mut review_log = camerata_ui_core::review_timer::SavedReviewLog {
                log: review_save.saved_log(),
                // Whatever could not be read when the draft was loaded is written back verbatim.
                unreadable: review_save.unreadable(),
                just_failed: false,
            };
            if !draft_loaded() {
                return;
            }
            // Each group's reviewed flag is refreshed from the dispositions right here, the one
            // place the log is written out, so it can never be stale in the draft.
            let queue_rows = audit_v
                .as_ref()
                .map(|a| findings_on_surface(&a.findings, KindSurface::Queue));
            if let Some(rows) = queue_rows {
                review_log
                    .log
                    .sync_groups(&camerata_ui_core::review_timer::undecided_by_group(
                        &rows, &disp,
                    ));
            }
            // The sites in words, for an error that names one.
            if let Some(a) = audit_v.as_ref() {
                let labels: std::collections::HashMap<String, String> = a
                    .findings
                    .iter()
                    .map(|f| (finding_key(f), format!("{}:{}", f.path, f.line)))
                    .collect();
                site_labels.set(labels);
            }
            // Stamp every new change with the run it is made on and the moment, then take what
            // the draft keeps of the queue. While the decision state is unknown that is the
            // queue exactly as it was loaded.
            let queue_state = {
                let mut bridge = decision_bridge_save.borrow_mut();
                if availability
                    .is_some_and(|a| a != camerata_ui_core::decisions::Availability::Unknown)
                {
                    let run = bridge.run_id().to_string();
                    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                    bridge.observe(&disp, &run, &now);
                }
                bridge.draft_queue(&disp)
            };
            let draft = OnboardingDraft {
                scan: report.clone(),
                audit: audit_v,
                repo_selection: sel,
                chosen: cho,
                custom: cust,
                // The decisions are the server's. The draft keeps only the queue: the changes
                // the store does not hold yet (an Unresolved entry is an undo waiting to be
                // revoked), the store's state as last read, and when each change was decided.
                dispositions: queue_state.pending,
                last_known: queue_state.last_known,
                pending_stamps: queue_state.stamps,
                viewed_repo: vr,
                triage_view: tv,
                order_preset: op,
                review_log,
            };
            let queue = save_queue.clone();
            spawn(async move {
                save_onboarding_draft_ordered(&queue, draft, || {
                    // Stamp the local time so the UI can show "auto-saved at HH:MM:SS".
                    last_saved.set(Some(
                        chrono::Local::now().format("%-I:%M:%S %p").to_string(),
                    ));
                })
                .await;
            });
            // Write the screen's decisions through to the store as they happen.
            if availability == Some(camerata_ui_core::decisions::Availability::Live) {
                let sync = DecisionSync {
                    bridge: decision_bridge_save.clone(),
                    project_id: bridge_pid,
                    sites: site_labels,
                };
                spawn(async move {
                    let out = sync.flush(dispositions).await;
                    if let Some(e) = &out.unreachable {
                        eprintln!("[camerata-ui] could not reach the decision store: {e}");
                    }
                    // A refused change is shown, naming the site and the reason, until it is
                    // stored. Each new refusal is also toasted once.
                    let texts: Vec<String> = out.refused.iter().map(|r| r.text()).collect();
                    for t in &texts {
                        if !sync_errors.peek().contains(t) {
                            crate::toast::push_toast(
                                toasts,
                                crate::toast::ToastKind::Error,
                                t.clone(),
                            );
                        }
                    }
                    if *sync_errors.peek() != texts {
                        sync_errors.set(texts);
                    }
                    if out.stored_any {
                        // Re-save the draft now that those changes are stored.
                        bridge_tick += 1;
                    }
                });
            }
        });
    }

    // ── Pass 2 step 1: the kind switcher is the PRIMARY axis ─────────────────
    // The five-kind taxonomy splits one list into four surfaces: the Queue (the two accusing
    // kinds plus any kind-less alarm row) and three informational panels. Everything below
    // that is per-disposition triage belongs to the Queue ALONE — counts, sub-tabs, the table,
    // and Process — so each is computed over `queue_findings`, not the raw set. Leaving the
    // informational rows in the disposition tally parked them as Unresolved forever, which the
    // Process gate blocks on.
    let kind_counts = surface_counts(&findings);
    let queue_findings = findings_on_surface(&findings, KindSurface::Queue);
    // Kind-less rows are an ALARM (an unmapped emission path), surfaced as a red chip on the
    // Queue tab. Never hidden, never folded into Defect.
    let n_unspecified = unspecified_kind_count(&findings);

    // Live per-table counts (recompute reactively as dispositions change).
    //
    // Pass 2 step 3: this was an inline five-way loop that duplicated `TriageModel::counts`.
    // With a fifth disposition the duplicate became a real drift risk — a state the loop
    // forgot would show a permanent zero on its tab AND silently stop blocking the Process
    // gate — so the tally is now taken by core's own function, the one the unit tests cover.
    let triage_model = TriageModel {
        dispositions: dispositions.read().clone(),
        triage_view: triage_view(),
    };
    let triage_counts: TriageCounts = triage_model.counts(&queue_findings);
    // The same tally per repo, shown under the tabs when the scan covers several repos.
    let repo_counts: Vec<(String, TriageCounts)> = triage_model.counts_by_repo(&queue_findings);

    // THE RULE HUB's data for whichever rule is open, derived once per render from the scan's
    // own rows by core's `rule_hub`. A request for a rule with no rows in that repo resolves
    // to `None` and the modal simply does not mount — the hub describes what a scan FOUND, and
    // an empty strip would be a dead affordance.
    let open_hub: Option<RuleHubView> = hub_open
        .read()
        .as_ref()
        .and_then(|r| rule_hub(&findings, &r.repo, &r.rule_id));

    // THE ALTERNATIVES CONTEXT for the open hub (design §9.1): the Evaluated-against line and
    // the Why? answer, at the surface every rule id already opens. `None` unless the rule is
    // multi-option AND the audit recorded a recommendation for it, which is the common case —
    // the hub must say nothing rather than render an empty line.
    //
    // Built from exactly the three sources `RuleAlternativesPanel` joins, through the SAME
    // pure `alternative_controls_row`, so the hub and the panel cannot disagree about which
    // option a rule is evaluated against. `staged` is `None` here because the staged-override
    // map is the panel's own state: the hub reports what the scan did, and the panel is where
    // a change is made. See the panel's doc comment.
    let open_hub_alternatives: Option<AlternativeControlsRow> =
        hub_open.read().as_ref().and_then(|req| {
            let rec = audited
                .as_ref()
                .and_then(|a| a.recommendations.get(&req.rule_id))
                .cloned()?;
            let rule = report
                .proposed_rules
                .iter()
                .find(|r| r.id == req.rule_id && r.options.len() >= 2)?;
            let scoped: Vec<&FindingView> = findings
                .iter()
                .filter(|f| f.repo == req.repo && f.rule_id == req.rule_id)
                .collect();
            let evaluated = scoped
                .iter()
                .find_map(|f| f.evaluated_option_id.clone())
                .filter(|o| !o.is_empty());
            Some(alternative_controls_row(
                rule,
                &rec,
                scoped.len(),
                evaluated.as_deref(),
                None,
                false,
            ))
        });

    rsx! {
        // The decision store could not be read: say so, never draw a decision as Unresolved.
        if let Some(text) = decision_banner() {
            div { class: "decisions-banner", role: "alert",
                span { "{text}" }
                button {
                    class: "btn-run",
                    onclick: {
                        let bridge = decision_bridge.clone();
                        move |_| {
                            let bridge = bridge.clone();
                            spawn(async move {
                                let queue = bridge.borrow().draft_queue(&dispositions.peek());
                                apply_decision_load(
                                    queue,
                                    bridge,
                                    dispositions,
                                    decision_availability,
                                    decision_banner,
                                    bridge_pid,
                                )
                                .await;
                            });
                        }
                    },
                    "Retry"
                }
            }
        }
        // Changes the store refused. They stay in the saved review and are not exported.
        for text in sync_errors() {
            div { class: "decisions-error", role: "alert", "{text}" }
        }
        // Row-detail modal: hosted here, at the results-subtree root, so it is NOT a
        // sibling of the chorale table (see RuleDetailModal for why).
        RuleDetailModal {}
        // THE INTERNAL REVIEW-TIME STATS (step 7a): per repo, per group type, per severity.
        // Hosted here for the same reason as every overlay. Computed when it opens from the live
        // log and the current dispositions; close and reopen to refresh. Not part of any export.
        if stats_open() {
            {
                let stats = camerata_ui_core::review_stats::ReviewStats::compute(
                    &review.log(),
                    &queue_findings,
                    &dispositions.read(),
                );
                let json = stats.to_json();
                rsx! {
                    super::review::ReviewStatsModal {
                        stats,
                        on_save: Callback::new(move |()| {
                            let json = json.clone();
                            spawn(async move {
                                // A cancelled dialog is silent; a failed write is not.
                                if save_text("camerata-review-time.json", json).await
                                    == Some(false)
                                {
                                    crate::toast::push_toast(
                                        toasts,
                                        crate::toast::ToastKind::Error,
                                        "Could not save the review-time stats file. Check the folder and try again.",
                                    );
                                }
                            });
                        }),
                        on_close: Callback::new(move |()| {
                            stats_open.set(false);
                            focus_detail_pane();
                        }),
                    }
                }
            }
        }
        // THE STACKED GROUP VIEW (work order B.2), hosted here for the same reason. It is a
        // layer UNDER the code viewer (lower z-index, and first in the DOM so order agrees).
        super::stack::StackRoot {}
        // THE COMMAND PALETTE (work order B.8), hosted here for the same reason. It is refused
        // while the viewer or the stack is up, so it never opens over them.
        super::palette::PaletteRoot {}
        // THE CODE VIEWER, hosted here for the same reason: it is a full-window overlay.
        super::viewer::ViewerRoot {}
        // THE RULE HUB (design §8). Hosted at the same root and for the same reason: it is an
        // overlay, and an overlay that is a sibling of a chorale table left a ghost
        // click-eater in the desktop webview.
        if let Some(hub) = open_hub {
            RuleHubModal {
                hub,
                alternatives: open_hub_alternatives,
                // ONE jump, and it is the derivation's own `hub.jump`. Into the Queue it is a
                // COLUMN FILTER — design §8's "a filter, not a hyperlink graph" — so the
                // curator lands in a normal, visibly filtered table with the controls to clear
                // it already on screen, rather than in a special mode he has to find his way
                // out of. Into a panel it is just the kind switcher, flipped.
                on_jump: Callback::new(move |surface: KindSurface| {
                    if surface == KindSurface::Queue {
                        queue_rule_filter
                            .set(hub_open.peek().as_ref().map(|r| r.rule_id.clone()));
                    }
                    kind_view.set(surface);
                    hub_open.set(None);
                }),
                on_close: Callback::new(move |()| hub_open.set(None)),
            }
        }
        // THE COMMIT DIALOG (Q4's gate). Hosted at the results-subtree root for the same
        // reason the hub is: an overlay that is a sibling of a chorale table left a ghost
        // click-eater in the desktop webview.
        //
        // ON FOCUS AND THE PANE'S KEYS. The dialog takes keyboard focus, and the destination
        // it asks the curator to type contains `c`, `i`, `t` and `f` more often than not. It
        // cannot arm the detail pane's disposition keys, for two independent reasons, and
        // both are asserted rather than assumed:
        //
        //  * STRUCTURAL — the dialog is raised only from the Conventions and No-convention
        //    panels, which render in the `else if` branches where `FindingsTable` (and so the
        //    pane, and so the keydown handler that lives on it) is not mounted at all.
        //  * BY CONSTRUCTION — `pane_command`'s scope gate is unconditional: anything but
        //    `PaneFocus::Cursor` returns `None` for every key, and the dialog's input is not
        //    the pane's cursor. `pane_keys_armed` is DEFINED as that same call, so the
        //    indicator cannot disagree with the handler.
        if let Some(dialog) = commit_dialog() {
            {
                let plan = match dialog.surface {
                    KindSurface::NoConvention => CommitPlan::recommendations(
                        &split_rules(&findings),
                        &adoption.read(),
                        &dialog.destination,
                    ),
                    _ => CommitPlan::conventions(
                        &adoptable_conventions(&findings),
                        &adoption.read(),
                        &dialog.destination,
                    ),
                };
                let typed = dialog.typed.clone();
                let commit_plan = plan.clone();
                let project_id_commit = project_id.clone();
                rsx! {
                    ConventionCommitDialog {
                        plan,
                        typed: typed.clone(),
                        busy: committing(),
                        on_typed: Callback::new(move |v: String| {
                            if let Some(d) = commit_dialog.write().as_mut() {
                                d.typed = v;
                            }
                        }),
                        on_cancel: Callback::new(move |()| commit_dialog.set(None)),
                        // Exit 2 — the safer path, offered rather than implied. It closes the
                        // dialog and narrows the panel to exactly the rules the commit would
                        // write unopened. It does NOT mark them opened: that would be the gate
                        // clearing its own warning.
                        on_review_unopened: Callback::new(move |()| {
                            unopened_only.set(true);
                            commit_dialog.set(None);
                        }),
                        on_commit: Callback::new(move |()| {
                            // THE ENFORCEMENT. The button's `disabled` attribute is a hint to
                            // the eye; this re-check is what actually holds, and it reads the
                            // same `commit_gate` the dialog rendered.
                            let typed = commit_dialog
                                .peek()
                                .as_ref()
                                .map(|d| d.typed.clone())
                                .unwrap_or_default();
                            if !commit_gate(&commit_plan, &typed).is_armed() {
                                return;
                            }
                            let project_id = project_id_commit.clone();
                            let destination = commit_plan.destination_repo.clone();
                            let adopted = commit_plan.adopted.clone();
                            committing.set(true);
                            spawn(async move {
                                let (ok, msg) =
                                    commit_adoptions(&project_id, &destination, &adopted).await;
                                committing.set(false);
                                if ok {
                                    commit_dialog.set(None);
                                }
                                crate::toast::push_toast(
                                    toasts,
                                    if ok {
                                        crate::toast::ToastKind::Info
                                    } else {
                                        crate::toast::ToastKind::Error
                                    },
                                    msg,
                                );
                            });
                        }),
                    }
                }
            }
        }
        // Finding-detail modal: click any findings row to read the violated rule's full
        // directive + the complete explanation the row cell truncates.
        if let Some(f) = detail_finding() {
            {
                let directive = descriptions_modal.get(&f.rule_id).cloned();
                let mut detail_finding = detail_finding;
                rsx! {
                    div { class: "rule-modal-overlay", onclick: move |_| detail_finding.set(None),
                        div { class: "rule-modal", onclick: move |e| e.stop_propagation(),
                            div { class: "rule-modal-head",
                                span { class: "rule-modal-id", "{f.rule_id}" }
                                button { class: "rule-modal-close", onclick: move |_| detail_finding.set(None), "\u{2715}" }
                            }
                            div { class: "rule-modal-meta",
                                span { class: "rule-modal-tag", "severity · {f.severity}" }
                                span { class: "rule-modal-tag", "{finding_location_label(&f)}" }
                                span { class: "rule-modal-tag", "{f.status}" }
                                // Authority: deterministic floor (stable, gateable) vs AI-advisory
                                // (model-inferred, id/severity may vary run-to-run).
                                if is_enforced_floor(&f.rule_id) {
                                    span { class: "rule-modal-tag", "enforced · deterministic (stable id)" }
                                } else {
                                    span { class: "rule-modal-tag", "AI · advisory (id may vary run-to-run)" }
                                }
                            }
                            if let Some(d) = directive {
                                p { class: "rule-modal-label", "Rule violated" }
                                p { class: "rule-modal-detail", "{d}" }
                            }
                            p { class: "rule-modal-label", "Finding" }
                            p { class: "rule-modal-title", "{f.snippet}" }
                            // Enclosing-code-block context (finding-code-context feature,
                            // docs/design/2026-07-27_finding-code-context.md): keyed by
                            // repo/path/line so switching findings remounts this section and
                            // starts a fresh on-demand fetch rather than showing stale content
                            // left over from the previously open finding.
                            FindingCodeContext {
                                key: "{f.repo}\u{1f}{f.path}\u{1f}{f.line}",
                                finding: f.clone(),
                                // The viewer lives in the Queue, so the door only exists while
                                // the Queue is the surface on screen. The modal closes; the
                                // Queue aims its cursor at this finding and opens the viewer.
                                on_open_viewer: (kind_view() == KindSurface::Queue).then(|| {
                                    let key = finding_key(&f);
                                    let mut req = viewer_request.0;
                                    Callback::new(move |()| {
                                        req.set(Some(key.clone()));
                                        detail_finding.set(None);
                                    })
                                }),
                            }
                            // Recommended fix (docs/design/2026-07-26_audit-report-refinements.md):
                            // keyed by rule_id so switching findings remounts this section and
                            // starts a fresh on-demand fetch, same idiom as FindingCodeContext.
                            FindingRecommendedFix {
                                key: "{f.rule_id}",
                                rule_id: f.rule_id.clone(),
                            }
                            p { class: "rule-modal-label", "Explanation" }
                            {
                                // Bold the calibration "[needs review: …]" flag so the reason it
                                // was hedged stands out from the explanation body.
                                let (body, nr) = split_needs_review(&f.detail);
                                rsx! {
                                    p { class: "rule-modal-detail",
                                        "{body}"
                                        if let Some(reason) = nr {
                                            " "
                                            b { class: "nr-inline",
                                                if reason.is_empty() { "[needs review]" } else { "[needs review: {reason}]" }
                                            }
                                        }
                                    }
                                }
                            }
                            if !f.also_matches.is_empty() {
                                p { class: "rule-modal-label", "Also violates at this location" }
                                div { class: "rule-modal-meta",
                                    for rid in f.also_matches.iter() {
                                        span { class: "rule-modal-tag", "{rid}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        div { class: "scan-results",
            if let Some(msg) = report.message.clone() {
                {
                    // Feature B — the compliance-safety gate: a `CliFallbackWarn` resolution
                    // (API-required, no key, but the project's "Allow Claude CLI" toggle is ON)
                    // doesn't abort the scan — it proceeds on the operator's personal-subscription
                    // CLI and folds a `⚠ COMPLIANCE:`-prefixed warning into this same joined-by-
                    // " · " message alongside routine scan notes (see `audit_repos` in
                    // crates/server/src/onboard.rs). Split those parts out into a dedicated,
                    // higher-alarm banner so a compliance-relevant event can never be mistaken
                    // for routine scan trivia buried in the plain note line.
                    let (compliance_notes, other_notes) = split_compliance_notes(&msg);
                    rsx! {
                        for note in compliance_notes.iter() {
                            div { class: "compliance-warning-banner",
                                span { class: "compliance-warning-icon", "\u{26a0}" }
                                div {
                                    p { class: "compliance-warning-title", "Compliance warning" }
                                    p { class: "compliance-warning-text", "{note}" }
                                }
                            }
                        }
                        if let Some(rest) = other_notes {
                            p { class: "scan-note", "{rest}" }
                        }
                    }
                }
            }
            div { class: "scan-summary",
                span { class: "scan-stat",
                    span { class: "scan-stat-n", "{report.repos.len()}" }
                    " repos"
                }
                span { class: "scan-stat",
                    span { class: "scan-stat-n", "{findings.len()}" }
                    " findings"
                }
                span { class: "scan-stat",
                    span { class: "scan-stat-n high", "{high}" }
                    " high severity"
                }
                span { class: "scan-stat",
                    span { class: "scan-stat-n high", "{enforced}" }
                    " enforced (new)"
                }
                span { class: "scan-stat",
                    span { class: "scan-stat-n", "{suppressed}" }
                    " suppressed (debt/waived)"
                }
                span { class: "scan-stat",
                    span { class: "scan-stat-n", "{report.files_scanned}" }
                    " files scanned"
                }
                if report.files_excluded > 0 {
                    span { class: "scan-stat",
                        span { class: "scan-stat-n", "{report.files_excluded}" }
                        " excluded as noise"
                    }
                }
                if !report.excluded_mechanical_rules.is_empty() {
                    span { class: "scan-stat",
                        title: "{report.excluded_mechanical_rules.join(\", \")}",
                        span { class: "scan-stat-n", "{report.excluded_mechanical_rules.len()}" }
                        " mechanical rule(s) enforced in CI, not scanned"
                    }
                }
            }
            if !report.coverage_notes.is_empty() {
                div { class: "scan-coverage-notes",
                    p { class: "scan-coverage-notes-title", "Scan coverage" }
                    for note in report.coverage_notes.iter() {
                        div { class: "scan-coverage-note",
                            span { class: "scan-coverage-note-tool", "{note.tool}" }
                            span { class: "scan-coverage-note-msg", " — {note.message}" }
                        }
                    }
                }
            }

            // Onboarding status + lifecycle actions. The post-scan steps (audit, triage,
            // apply, wire-CI) are all optional, so "Complete onboarding" is available here.
            div { class: "onboard-actionbar",
                if let Some(ts) = last_saved() {
                    span { class: "onboard-saved", "✓ Auto-saved at {ts}" }
                }
                div { class: "onboard-actionbar-spacer" }
                button {
                    class: "btn-secondary danger",
                    onclick: move |_| {
                        if restart_arm() {
                            spawn(async move {
                                clear_onboarding_draft().await;
                                restart_arm.set(false);
                                onboard_scan.set(None);
                            });
                        } else {
                            restart_arm.set(true);
                        }
                    },
                    if restart_arm() { "Confirm: discard & rescan?" } else { "Start over" }
                }
                button {
                    class: "btn-run",
                    disabled: finishing(),
                    onclick: move |_| {
                        finishing.set(true);
                        spawn(async move {
                            if complete_onboarding().await {
                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, "Onboarding complete. Rules saved to the project.");
                                onboard_scan.set(None);
                                view.set(CockpitView::Stories);
                            } else {
                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Could not complete onboarding.");
                            }
                            finishing.set(false);
                        });
                    },
                    if finishing() { "Finishing…" } else { "Complete onboarding" }
                }
            }

            if !report.stacks.is_empty() {
                div { class: "scan-stacks",
                    for s in report.stacks.iter() {
                        div { class: "scan-stack",
                            span { class: "scan-stack-repo", "{s.repo}" }
                            span { class: "scan-stack-tech",
                                {
                                    let mut tech = s.languages.clone();
                                    tech.extend(s.frameworks.clone());
                                    if tech.is_empty() { "stack not detected".to_string() } else { tech.join(" · ") }
                                }
                            }
                        }
                    }
                }
            }

            // ── Phase 1 result: the proposed ruleset to pick from ──────────────
            p {
                class: "scan-section-h",
                // The walkthrough points here for the stack rules.
                "data-tour": camerata_ui_core::walkthrough::target::RULES,
                "Step 1 — proposed starter ruleset"
            }
            p { class: "scan-section-sub", "Camerata mapped the stack and proposes these rules. Pick the ones to enforce and choose alternatives; you own the final set (arming generates the governance PR)." }
            p { class: "scan-section-sub", "Click a rule row to read its full context and choose its alternative." }
            // Per-repo view switch (multi-repo only): pick which repo's recommended-rule
            // table to view + select. Each repo keeps its own selection; the audit runs every
            // repo against its own picks.
            if multi_repo {
                div { class: "repo-select",
                    label { class: "repo-select-label", "Repo ruleset:" }
                    select {
                        class: "repo-select-input",
                        value: "{viewed_repo}",
                        onchange: move |e| viewed_repo.set(e.value()),
                        for repo in report.repos.iter() {
                            option { key: "{repo}", value: "{repo}", "{repo}" }
                        }
                    }
                    span { class: "repo-select-hint",
                        "Showing rules for this repo. Each repo has its own selection; the audit scans every repo against its own rules."
                    }
                }
            }
            // Author custom rules (#49) — they appear in the Custom / Custom Global groups below.
            CustomRulesPanel { all_repos: report.repos.clone() }
            {
                let repos_audit = report.repos.clone();
                // Per-repo binding drives RECOMMENDATION (pre-selection), NOT visibility: every
                // repo's table shows the WHOLE rule library so the architect can manually add
                // ANY rule to ANY repo. (Filtering visibility by the repo binding hid rules that
                // were auto-suggested for a sibling repo — e.g. ci-cd suggested for repo A never
                // appeared in repo B's table, so it couldn't be added there at all.) The viewed
                // repo only changes which rules are pre-checked, via the seeded per-repo selection.
                let view_repo = if multi_repo { viewed_repo() } else { String::new() };
                // Merge in the user's custom rules (#49). VISIBLE in this repo's table = corpus
                // rules + Custom Global + this repo's own Custom rules (repo-scoped customs don't
                // leak into sibling repos). The cross-repo `all_rules` lookup gets EVERY custom
                // (all repos) so arm/audit can resolve them. The table is re-keyed on a custom
                // signature so creating/editing/deleting a custom rule remounts it with the change.
                let actual_repo = viewed_repo();
                let (visible_customs, all_customs, custom_sig) = {
                    let cust = custom_rules.read();
                    let all_repos = report.repos.clone();
                    let visible: Vec<ProposedRuleView> = cust
                        .iter()
                        .filter(|c| c.is_global() || c.domain.trim() == actual_repo)
                        .map(|c| c.to_proposed(&all_repos))
                        .collect();
                    let all: Vec<ProposedRuleView> = cust.iter().map(|c| c.to_proposed(&all_repos)).collect();
                    let sig = cust.iter().map(|c| format!("{}\u{1}{}", c.name, c.domain)).collect::<Vec<_>>().join("\u{2}");
                    (visible, all, sig)
                };
                let mut all_rules = report.proposed_rules.clone();
                all_rules.extend(all_customs);
                let mut visible_rules = report.proposed_rules.clone();
                visible_rules.extend(visible_customs);
                rsx! {
                    ProposedRulesTable {
                        // Key on the viewed repo + custom signature so switching repos OR adding/
                        // editing/deleting a custom rule remounts the table with the change.
                        key: "{view_repo}\u{1f}{custom_sig}",
                        rules: visible_rules,
                        all_rules,
                        view_repo,
                        repo_selection,
                        findings: findings.clone(),
                        auditing: auditing(),
                        scan_block: scan_blocked.clone(),
                        on_audit: move |rules: Vec<SelectedAuditRule>| {
                            let repos = repos_audit.clone();
                            let model = audit_model();
                            let calib = calibration_model();
                            let mode = audit_mode();
                            let deep = audit_deep();
                            // Scan-type selector: both default true; if a user somehow unticks
                            // both, send both true (the server also coerces — never a no-op).
                            let mut ai = run_ai_review();
                            let mut det = run_deterministic();
                            if !ai && !det { ai = true; det = true; }
                            // Full scan forces a clean pass; otherwise the scan is incremental
                            // (only files changed since the last scan cost AI tokens).
                            let incremental = !audit_full_scan();
                            // A spend limit that is not a positive number stops the scan from
                            // starting (and says why) rather than silently running unlimited.
                            let max_cost = match camerata_ui_core::scan::parse_spend_limit(&audit_spend_limit()) {
                                Ok(v) => v,
                                Err(msg) => {
                                    job_failed_message.set(Some(msg));
                                    return;
                                }
                            };
                            // Deterministic-only is fast, but its PROGRESS is only pollable on
                            // the async job path (the sync path holds one request and returns
                            // the final report). So route a deterministic-ONLY scan through the
                            // job path regardless of the picked mode — that's where the
                            // per-tool progress streams into the "Deterministic scan" component.
                            let deterministic_only = det && !ai;
                            let use_job = mode == "job" || deterministic_only;
                            // Clear the PREVIOUS run's findings so a re-audit starts from a
                            // blank Findings table instead of showing stale results while
                            // the new audit runs (the server also clears the transcript).
                            // Also reset per-finding triage state, view filter, and the open
                            // detail modal — they're specific to the previous run's findings.
                            // repo_selection / chosen / custom_rules are NOT reset: those are
                            // the architect's persistent rule choices that survive a re-audit.
                            audit.set(None);
                            // Decisions are the server's and carry across a re-audit by site key
                            // (an exact match keeps its decision; a moved or new site is
                            // Unresolved because its key is not in the map). The map is NOT
                            // cleared: the write-through bridge would read that as the reviewer
                            // undoing every decision.
                            triage_view.set(TriageState::Unresolved);
                            detail_finding.set(None);
                            job_progress.set(None);
                            det_progress.set(None);
                            // Clear any PREVIOUS run's failure banner — a fresh scan starting
                            // is not itself a failure, and a stale message here would look
                            // like this new run already failed before it even began.
                            job_failed_message.set(None);
                            auditing.set(true);
                            if use_job {
                                // Async job: submit, record the id (app-scope, so a later
                                // mount can resume), then poll. The server runs it decoupled
                                // from any single request.
                                let mut active_audit_job = active_audit_job;
                                // `loading_count` was captured in the component's render body
                                // (see its `use_context` above) — NOT resolved from context in
                                // here, since this closure body runs inside a detached `spawn`
                                // where that resolution is unreliable (BOMBE-SILENT-MISS note on
                                // `poll_job`). `LoadingCount` is `Copy`, so capturing it in this
                                // `move` closure and again in the inner `spawn` below is just two
                                // cheap copies of the same signal, same as `job_progress` etc.
                                let loading_count = loading_count;
                                spawn(async move {
                                    let _guard = crate::loading::LoadingGuard::from_count(loading_count);
                                    let Some(jid) = audit_job_start(&repos, &rules, &model, &calib, "auto", incremental, deep, ai, det, max_cost).await else {
                                        auditing.set(false);
                                        job_failed_message.set(Some(
                                            "Could not start the background scan (the request \
                                             to submit it failed). Please retry."
                                                .to_string(),
                                        ));
                                        return;
                                    };
                                    active_audit_job.set(Some(jid.clone()));
                                    poll_job(
                                        jid,
                                        audit,
                                        auditing,
                                        job_progress,
                                        det_progress,
                                        active_audit_job,
                                        scan_idle_ms,
                                        job_failed_message,
                                        loading_count,
                                    )
                                    .await;
                                });
                            } else {
                                // Synchronous: hold the request until the (shorter) run finishes.
                                spawn(async move {
                                    let result = audit_against(&repos, &rules, &model, &calib, &mode, incremental, deep, ai, det, max_cost).await;
                                    if result.is_none() {
                                        // The sync request itself failed (network error, server
                                        // unreachable) — the server-side crash path already
                                        // returns a normal 200 JSON with `ai_error` set, so
                                        // `None` here means the response never arrived at all.
                                        job_failed_message.set(Some(
                                            "The scan request failed (no response from the \
                                             server). Please retry."
                                                .to_string(),
                                        ));
                                    }
                                    audit.set(result);
                                    auditing.set(false);
                                });
                            }
                        },
                    }
                }
            }

            // ── Phase 2: the audit runs from the table's "Audit selected" button ──
            div { class: "audit-cta",
                p { class: "scan-section-h", "Step 2 — audit the code against your selected rules (optional)" }
                p { class: "scan-section-sub", "The audit is OPTIONAL — you can Apply the rules above and finish onboarding without it. Run it when you want to see existing violations to triage. Tick the rules, then press “Audit code against selected rules”. The deterministic security rules (secrets / raw-SQL / secret-URLs) always run as the enforced floor; the AI checks the code against ONLY your selected rules AND flags anything else worth a look (advisory)." }
                // Model picker — the user owns the speed/thoroughness trade-off. List is
                // company-agnostic, served by /api/models/registry. Options are grouped
                // by provider (Claude first, then OpenRouter); each label has badges
                // (FREE / no-tools / NNNk ctx). Degrades to Claude-only when no OpenRouter
                // key is configured.
                if let Some(m) = models.as_ref() {
                    div { class: "audit-model-row",
                        label { class: "audit-model-label", "Audit model" }
                        select {
                            class: "audit-model-select",
                            disabled: auditing(),
                            value: "{audit_model}",
                            onchange: {
                                let pid = project_id.clone();
                                move |e: Event<FormData>| {
                                    let model = e.value();
                                    audit_model.set(model.clone());
                                    // Persist to project.step_models so Settings stays in sync.
                                    let pid = pid.clone();
                                    spawn(async move {
                                        if !pid.is_empty() {
                                            set_project_step_model(&pid, "audit", &model).await;
                                        }
                                    });
                                }
                            },
                            for (group_label , opts) in m.grouped().into_iter() {
                                optgroup { label: "{group_label}",
                                    for opt in opts.into_iter() {
                                        option { key: "{opt.id}", value: "{opt.id}", "{opt.label}" }
                                    }
                                }
                            }
                        }
                        span { class: "audit-model-hint", "Faster models finish sooner; stronger models catch more." }
                    }
                    // Calibration model — its OWN tier. The scan finds; calibration
                    // recalibrates severity + tags confidence. Defaults to the scan model
                    // (end-to-end); split it to run a cheap scan with a stronger verify.
                    div { class: "audit-model-row",
                        label { class: "audit-model-label", "Calibration model" }
                        select {
                            class: "audit-model-select",
                            disabled: auditing(),
                            value: "{calibration_model}",
                            onchange: {
                                let pid = project_id.clone();
                                move |e: Event<FormData>| {
                                    let model = e.value();
                                    calibration_model.set(model.clone());
                                    // Persist to project.step_models so Settings stays in sync.
                                    let pid = pid.clone();
                                    spawn(async move {
                                        if !pid.is_empty() {
                                            set_project_step_model(&pid, "calibration", &model).await;
                                        }
                                    });
                                }
                            },
                            for (group_label , opts) in m.grouped().into_iter() {
                                optgroup { label: "{group_label}",
                                    for opt in opts.into_iter() {
                                        option { key: "{opt.id}", value: "{opt.id}", "{opt.label}" }
                                    }
                                }
                            }
                        }
                        span { class: "audit-model-hint", "Recalibrates severity + flags low-confidence findings. Default = the scan model; pick a stronger one for cheap-scan-plus-smart-verify." }
                    }
                }
                // Scan-type selector (Part C) — pick WHICH scans run. Both default ON
                // (today's behaviour). Deterministic-only is fast and uses no LLM / no tokens.
                div { class: "audit-model-row",
                    label { class: "audit-model-label", "What to scan" }
                    div { class: "scan-type-selector",
                        label { class: "audit-thorough-toggle",
                            input {
                                r#type: "checkbox",
                                checked: run_ai_review(),
                                disabled: auditing(),
                                onchange: move |e| run_ai_review.set(e.checked()),
                            }
                            span { "AI architectural review" }
                        }
                        label { class: "audit-thorough-toggle",
                            input {
                                r#type: "checkbox",
                                checked: run_deterministic(),
                                disabled: auditing(),
                                onchange: move |e| run_deterministic.set(e.checked()),
                            }
                            span { "Deterministic scans (floor + linters)" }
                        }
                    }
                    span { class: "audit-model-hint",
                        if run_deterministic() && !run_ai_review() {
                            "Deterministic-only: the always-on security floor + the mechanical preview linters run LOCALLY — fast, no LLM, NO TOKENS. The AI review is skipped entirely (zero model calls). Best for a quick check or QA of the deterministic pass."
                        } else if run_ai_review() && !run_deterministic() {
                            "AI-only: the LLM checks the code against your selected architectural/structured/prose rules. The deterministic floor + preview linters are skipped."
                        } else {
                            "Both run (default): the deterministic floor + linters (free, no tokens) AND the AI architectural review. Untick AI for a fast, token-free deterministic-only scan. At least one must be ticked — unticking both runs both."
                        }
                    }
                }
                // Execution mode — speed/scale knob, separate from the model (quality) and
                // the rule selection (coverage). Parallel is the recommended default.
                div { class: "audit-model-row",
                    label { class: "audit-model-label", "Scan mode" }
                    select {
                        class: "audit-model-select",
                        disabled: auditing(),
                        value: "{audit_mode}",
                        onchange: move |e| audit_mode.set(e.value()),
                        option { value: "auto", "Default (batch on the API backend, else parallel)" }
                        option { value: "parallel", "Parallel (real time)" }
                        option { value: "sequential", "Sequential (real time, slower, gentlest)" }
                        option { value: "job", "Background job (walk away, default mode)" }
                        option { value: "batch", "Batch (50% off, async, API key required)" }
                    }
                    if audit_mode() == recommended_mode {
                        span { class: "audit-mode-rec", "✓ auto-selected for this scan's size" }
                    }
                    span { class: "audit-model-hint", "Default runs the scan as a Message Batch on the API backend (half price, results when the whole batch is done) and in real time on the CLI backend. Parallel and Sequential force real time. Parallel runs rule-batches concurrently. Background job runs server-side so you can leave and watch findings stream in — best for huge / multi-repo scans. Batch uses the Anthropic Message Batches API for a flat 50% discount on all tokens; requires an Anthropic API key (added in the app, or set as ANTHROPIC_API_KEY) and the api backend; results arrive asynchronously (seconds to minutes, up to 24h on very large scans)." }
                }
                // Incremental scan — on by default; re-scans only pay AI for changed files.
                // The checkbox forces a full re-scan over every file.
                div { class: "audit-model-row",
                    label { class: "audit-model-label", "Full scan" }
                    label { class: "audit-thorough-toggle",
                        input {
                            r#type: "checkbox",
                            checked: audit_full_scan(),
                            disabled: auditing(),
                            onchange: move |e| audit_full_scan.set(e.checked()),
                        }
                        span { "Re-scan every file (ignore the incremental cache)" }
                    }
                    span { class: "audit-model-hint",
                        "By default a re-scan is INCREMENTAL: only files whose content changed since the last scan of this project are sent to the AI, and findings for unchanged files are reused from cache — so re-running after a small edit costs a fraction of the tokens. The first scan of a project is always full (no cache yet). Tick this to ignore the cache and re-audit the whole codebase from scratch (e.g. after changing your rule selection, or to refresh every finding)."
                    }
                }
                // Spend limit: optional hard stop on the scan's AI spend, checked between passes.
                div {
                    class: "audit-model-row",
                    "data-tour": camerata_ui_core::walkthrough::target::SPEND_LIMIT,
                    label { class: "audit-model-label", "Spend limit (USD)" }
                    input {
                        r#type: "text",
                        placeholder: "no limit",
                        value: "{audit_spend_limit}",
                        disabled: auditing(),
                        oninput: move |e| audit_spend_limit.set(e.value()),
                    }
                    span { class: "audit-model-hint",
                        "Optional. The scan stops between passes once its actual AI spend reaches this amount, lists every pass it did not run, and marks the result incomplete. Passes already started finish, so it can end about one pass over the limit. Leave blank for no limit."
                    }
                }
                // Deep compliance & security tier (#55): opt-in, ADVISORY, expensive.
                // Gated on the `soc2` feature flag — hidden entirely when soc2 is disabled
                // (this is the SOC-2-headlined surface; set via .camerata/features.toml or
                // CAMERATA_FEATURE_SOC2=false). The server also skips the lens when off.
                if feature_flags.soc2 && run_ai_review() {
                    div { class: "audit-model-row",
                        label { class: "audit-model-label", "Deep compliance & security (opt-in)" }
                        label { class: "audit-thorough-toggle",
                            input {
                                r#type: "checkbox",
                                checked: audit_deep(),
                                disabled: auditing(),
                                onchange: move |e| audit_deep.set(e.checked()),
                            }
                            span { "Run SOC-2 gap analysis, deep security audit, and threat model" }
                        }
                        span { class: "audit-model-hint deep-tier-warning",
                            "ADVISORY ONLY — not a SOC-2 report and not a penetration test. \
                             Camerata sees static code only; controls that depend on org-level evidence \
                             (HR policies, vendor contracts, access reviews) cannot be assessed from code. \
                             Three extra whole-repo passes run after the standard audit. \
                             This is the MOST EXPENSIVE tier (~3 extra whole-repo passes). \
                             Enable only when you explicitly want compliance gap analysis for this codebase."
                        }
                    }
                }
                // If the architect SKIPS the audit, the post-scan section below (which hosts the
                // CI-wiring story) never renders because `code_chars == 0`. But wiring mechanical
                // rules into CI only needs the SELECTED rules, not a code scan — so offer the
                // CI-story affordance here too, so it's reachable straight after rule selection.
                if report.code_chars == 0 {
                    div { class: "onboard-final-step",
                        span { class: "onboard-step-eyebrow", "Optional: wire CI rules into CI" }
                        CiRulesPanel {
                            repos: report.repos.clone(),
                            rules: ci_rule_items_from_proposed(&report.proposed_rules),
                        }
                        p { class: "section-hint", "You can file the CI-wiring stories (GitHub issues) from your selected rules without running the audit. Optional, and not required to finish onboarding." }
                    }
                }
                // Inspection setup: who the inspection is for. Saved with the project, it fills
                // the confidentiality line on every page and sheet of every export, and the
                // export refuses to run while it is blank (2026-10-24 decision).
                if let Some(p) = active_project.as_ref() {
                    ClientNameSetup {
                        project_id: p.id.clone(),
                        initial: p.client_name.clone(),
                    }
                }
                // Cost: the pre-audit ESTIMATE for this configuration (model + calibration
                // model + mode + ticked rules), and — once the audit has run — the ACTUAL
                // billed usage beside it, so the estimate is verifiable, not a black box.
                if report.code_chars > 0 {
                    {
                        let price = |id: &str, fallback: (f64, f64)| {
                            models.as_ref()
                                .and_then(|m| m.models.iter().find(|o| o.id == id).map(|o| (o.price_in, o.price_out)))
                                .unwrap_or(fallback)
                        };
                        // When AI scan is off, every model price is zeroed — no LLM calls
                        // means no token spend. The deep tier also requires AI, so it too is
                        // zeroed. This ensures the estimate correctly shows $0 (or "<$0.01")
                        // when the user has selected deterministic-only mode.
                        let ai_on = run_ai_review();
                        let (a_in, a_out) = if ai_on { price(&audit_model(), (3.0, 15.0)) } else { (0.0, 0.0) };
                        let (c_in, c_out) = if ai_on { price(&calibration_model(), (a_in, a_out)) } else { (0.0, 0.0) };
                        // The fix stage runs on its default model (the server's `fix_on_haiku`
                        // switch is on by default); priced when the model list prices it.
                        let fix_model_id = camerata_api_types::project::FIX_STAGE_HAIKU_MODEL;
                        let fix_price = if ai_on {
                            models.as_ref().and_then(|m| m.models.iter().find(|o| o.id == fix_model_id).map(|o| (o.price_in, o.price_out)))
                        } else {
                            None
                        };
                        // The group reviewer (stage 2) runs on the project's group review model,
                        // else its built-in default; priced when the model list prices it.
                        let group_review_id = active_project
                            .as_ref()
                            .map(|p| p.review.group_review_model.trim().to_string())
                            .filter(|m| !m.is_empty())
                            .unwrap_or_else(|| camerata_api_types::project::STAGE2_DEFAULT_MODEL.to_string());
                        let group_review_price = if ai_on {
                            models.as_ref().and_then(|m| m.models.iter().find(|o| o.id == group_review_id).map(|o| (o.price_in, o.price_out)))
                        } else {
                            None
                        };
                        let scan_id = audit_model();
                        let calibration_id = calibration_model();
                        let sel = selected_count();
                        // Mirror the request flags the audit will actually send: incremental
                        // scope (only changed files cost tokens unless Full scan is ticked; the
                        // estimate prices a full scan and the note below says so) and the deep
                        // SOC-2/security tier (three extra whole-repo passes).
                        let incremental = !audit_full_scan();
                        // Deep tier only runs with AI; force it off in the estimate when AI is off.
                        let deep = audit_deep() && ai_on;
                        // The project's AI backend (docs/design/2026-09-22_per-project-backend.md)
                        // changes how the estimator prices the shared repo digest's prompt cache
                        // (CLI can't reuse a prior call's cache; API/batch can) — defaults to Cli,
                        // the safe/zero-setup default, when no active project has loaded yet.
                        let backend = active_project.as_ref().map(|p| p.backend).unwrap_or_default();
                        // Best-effort count of multi-option rules in the proposed corpus (the
                        // live per-rule selection set isn't available here) — sizes the one-off
                        // alternative-recommendation pass the audit will run.
                        let multi_option_selected =
                            report.proposed_rules.iter().filter(|r| r.options.len() >= 2).count();
                        let est_mode = camerata_ui_core::scan::estimate_mode(&audit_mode(), backend).to_string();
                        // THE run estimate (`camerata_ui_core::scan::cockpit_run_estimate`): the
                        // same arithmetic the command line's pre-flight prints, over the inputs
                        // the cockpit has (each repo as one routing group: the cockpit has no
                        // file list to route, so its figure sits a little under the pre-flight's).
                        fn priced(id: &str, price_in: f64, price_out: f64) -> camerata_ui_core::scan::PricedModel<'_> {
                            camerata_ui_core::scan::PricedModel { id, price_in, price_out }
                        }
                        let flags = camerata_ui_core::scan::RunFlags {
                            deep,
                            backend,
                            terse_output: true,
                        };
                        let run_estimate = |inputs: &[camerata_ui_core::scan::RunRepoInput]| {
                            camerata_ui_core::scan::cockpit_run_estimate(
                                inputs,
                                &est_mode,
                                priced(&scan_id, a_in, a_out),
                                priced(&calibration_id, c_in, c_out),
                                group_review_price.map(|(i, o)| priced(&group_review_id, i, o)),
                                fix_price.map(|(i, o)| priced(fix_model_id, i, o)),
                                flags,
                            )
                        };
                        // Several repos: one estimate per repo, summed (the scan chunks and
                        // calibrates each repo on its own). Single repo, or sizes not back yet:
                        // the one-stream estimate, unchanged.
                        let sizes: Option<Vec<RepoSizeView>> = if multi_repo { (*repo_sizes.read()).clone().flatten() } else { None };
                        let mut repo_lines: Vec<String> = Vec::new();
                        let (toks, dollars, passes) = match sizes {
                            Some(sizes) => {
                                let selection = repo_selection.read().clone();
                                // A repo is priced only when its rule choice is known: a guessed
                                // rule count would put a made-up figure in front of a spend decision.
                                let inputs: Vec<camerata_ui_core::scan::RunRepoInput> = sizes
                                    .iter()
                                    .filter(|z| z.error.is_none())
                                    .filter_map(|z| {
                                        let ids = selection.get(&z.repo)?;
                                        Some(camerata_ui_core::scan::RunRepoInput {
                                            repo: z.repo.clone(),
                                            code_chars: z.code_chars,
                                            selected: ids.len(),
                                            multi_option_selected: report.proposed_rules.iter().filter(|r| r.options.len() >= 2 && ids.contains(&r.id)).count(),
                                            routing_groups: 1,
                                        })
                                    })
                                    .collect();
                                let est = run_estimate(&inputs);
                                for z in sizes.iter() {
                                    match (&z.error, est.rows.iter().find(|r| r.repo == z.repo)) {
                                        (Some(why), _) => repo_lines.push(camerata_ui_core::scan::unreadable_repo_line(&z.repo, why)),
                                        (None, Some(row)) => repo_lines.push(camerata_ui_core::scan::repo_estimate_line(&z.repo, z.files, z.code_chars, row)),
                                        (None, None) => repo_lines.push(camerata_ui_core::scan::unknown_rules_repo_line(&z.repo)),
                                    }
                                }
                                (est.tokens, est.dollars, est.passes)
                            }
                            None => {
                                let est = run_estimate(&[camerata_ui_core::scan::RunRepoInput {
                                    repo: report.repos.first().cloned().unwrap_or_default(),
                                    code_chars: report.code_chars,
                                    selected: sel,
                                    multi_option_selected,
                                    routing_groups: 1,
                                }]);
                                (est.tokens, est.dollars, est.passes)
                            }
                        };
                        let code_toks = human_tokens((report.code_chars as f64 / 4.0) as u64);
                        let dollar_str = if dollars < 0.01 { "<$0.01".to_string() } else { format!("~${dollars:.2}") };
                        // ACTUAL, once the audit finished and the backend reported usage.
                        let actual = audited.as_ref().and_then(|a| a.actual_usage.clone()).filter(|u| u.calls > 0);
                        rsx! {
                            div {
                                class: "audit-cost",
                                if let Some(u) = actual {
                                    {
                                        let act_toks = u.input_tokens + u.output_tokens;
                                        let act_dollar = if !u.cost_complete { "n/a".to_string() }
                                            else if u.cost_usd < 0.01 { "<$0.01".to_string() }
                                            else { format!("${:.2}", u.cost_usd) };
                                        // Show cache savings line when the API backend ran with
                                        // prompt caching active (cache_read > 0 means the cache
                                        // was hit at least once; creation > 0 means the cache
                                        // was written at least once).
                                        let cache_active = u.cache_read_input_tokens > 0
                                            || u.cache_creation_input_tokens > 0;
                                        rsx! {
                                            div { class: "audit-cost-main",
                                                span { class: "audit-cost-label", "Actual cost" }
                                                span { class: "audit-cost-val", "{act_dollar}" }
                                                span { class: "audit-cost-meta", "{human_tokens(act_toks)} tokens · {u.calls} calls · est. was {dollar_str}" }
                                            }
                                            p { class: "audit-cost-note",
                                                if u.cost_complete {
                                                    "Real billed usage for this run ({human_tokens(u.input_tokens)} in / {human_tokens(u.output_tokens)} out). "
                                                } else {
                                                    "Real token usage shown; a $ figure needs every call to report cost (some didn't, so it's omitted to avoid understating). "
                                                }
                                                if cache_active {
                                                    "Prompt cache: {human_tokens(u.cache_creation_input_tokens)} tok written (1.25x), {human_tokens(u.cache_read_input_tokens)} tok read (0.1x). "
                                                }
                                                "The deterministic security floor ran free. Next time you audit PR diffs — pennies."
                                            }
                                        }
                                    }
                                } else {
                                    div { class: "audit-cost-main",
                                        span { class: "audit-cost-label", "Estimated cost" }
                                        span { class: "audit-cost-val", "{dollar_str}" }
                                        span { class: "audit-cost-meta", "~{human_tokens(toks)} tokens · {passes} pass(es) · {sel} rule(s)" }
                                    }
                                    if !repo_lines.is_empty() {
                                        ul { class: "audit-cost-repos",
                                            for line in repo_lines.iter() {
                                                li { key: "{line}", "{line}" }
                                            }
                                        }
                                    }
                                    p { class: "audit-cost-note",
                                        // Approximate in both directions now, not uniformly "biased high": output
                                        // tokens (found issues) dominate findings-heavy scans regardless of backend,
                                        // but the input side differs by backend. On CLI, every call is a fresh
                                        // `claude` subprocess that can't reuse another call's prompt cache, so each
                                        // pass pays the cache-write surcharge with no read discount to offset it —
                                        // this estimate already prices that in, it isn't a caching UPSIDE to expect.
                                        // On API/batch, prompt-caching across batches keeps the input side down, so
                                        // the real bill can land below this figure.
                                        if backend == ProjectBackend::Cli {
                                            "Approximate — output tokens (found issues) dominate findings-heavy scans. This is priced for the CLI backend: each call is a separate subprocess with its own fresh prompt cache, so there's no cross-call caching discount to expect here (switching to the API backend, with a key configured, usually costs less for the same scan). "
                                        } else {
                                            "Approximate — output tokens (found issues) dominate findings-heavy scans; prompt-caching across batches keeps the input side down, so the real bill can land below this figure. "
                                        }
                                        if incremental {
                                            "Scope: INCREMENTAL — only files changed since the last scan are billed, so the real cost is usually well below this whole-repo figure (priced over ~{code_toks} tokens, {report.files_scanned} files). Tick Full scan to re-audit everything. "
                                        } else {
                                            "Scope: FULL — every file is re-audited (~{code_toks} tokens, {report.files_scanned} files). "
                                        }
                                        "The deterministic security floor (secrets / raw-SQL / secret-URLs) runs free. "
                                        "After this, you audit PR diffs — pennies. Cheaper model or Sequential mode lowers this."
                                        if deep {
                                            span { class: "audit-cost-deep-note",
                                                " Deep tier is ON and INCLUDED above: ~3 extra whole-repo passes (SOC-2 gap, deep security, threat model) at the audit model. This is the MOST EXPENSIVE option — it dominates the figure. Untick Deep to drop it."
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                // Live progress for an async job: a determinate bar (it grows as repos are
                // discovered) + a findings-so-far count, so a walk-away scan shows life.
                if let Some((done, total, nf, not_run)) = job_progress() {
                    {
                        let pct = (done * 100).checked_div(total).unwrap_or(0).min(100);
                        let n_repos = report.repos.len();
                        rsx! {
                            div { class: "job-progress",
                                div { class: "job-progress-track",
                                    div { class: "job-progress-fill", style: "width: {pct}%" }
                                }
                                span { class: "job-progress-label", "{job_progress_label(done, total, nf, not_run)}" }
                                // Multi-repo scans run ONE repo at a time, and the pass
                                // denominator grows as each repo is reached — so the agent
                                // activity below shows only the repo currently running, not all
                                // of them. Make the full scope explicit so it doesn't look like
                                // only one repo is being scanned.
                                if n_repos > 1 {
                                    div { class: "job-progress-scope",
                                        span { class: "job-progress-scope-h", "{n_repos} repos in scope, scanned one at a time:" }
                                        for repo in report.repos.iter() {
                                            span { key: "{repo}", class: "job-progress-repo", "{repo}" }
                                        }
                                        span { class: "job-progress-scope-note", "The pass count climbs as each repo is reached; agent activity shows the repo running now." }
                                    }
                                }
                            }
                        }
                    }
                }
                // While the audit runs the background Bombe machine activates via
                // the global loading guard — just show the text label inline.
                if auditing() {
                    div { class: "audit-thinking",
                        span { class: "audit-thinking-label", "Camerata is auditing your code\u{2026}" }
                        button {
                            class: "btn-stop",
                            onclick: move |_| {
                                let jid = active_audit_job().unwrap_or_default();
                                spawn(async move { cancel_audit_job(&jid).await; });
                            },
                            "\u{25a0} Stop scan"
                        }
                    }
                }
                // Scan stall warning: shown when the job has been idle above the threshold.
                // The server sends idle_ms; we show a warning if it's over 2 minutes (120s).
                if let Some(idle) = scan_idle_ms() {
                    if idle > 120_000 && auditing() {
                        div { class: "run-stall-warning scan-stall-warning",
                            div { class: "run-stall-warning-head",
                                span { class: "run-stall-icon", "\u{26a0}" }
                                span { class: "run-stall-title", "No progress for {format_idle(idle)} — scan may be stalled" }
                                button {
                                    class: "btn-stop btn-stop-stall",
                                    onclick: move |_| {
                                        let jid = active_audit_job().unwrap_or_default();
                                        spawn(async move { cancel_audit_job(&jid).await; });
                                    },
                                    "\u{25a0} Stop scan"
                                }
                            }
                        }
                    }
                }
                // Deterministic-scan PROGRESS — rendered ABOVE the AI agent-activity drawer.
                // Shows the deterministic pass's per-tool start/run/done + findings count and
                // an overall done/total. It's the PRIMARY progress view in deterministic-only
                // mode (where the AI drawer below is empty).
                if let Some(dp) = det_progress() {
                    DeterministicProgress { progress: dp }
                }
                // Background-job FAILURE banner (silent-death fix): rendered UNCONDITIONALLY,
                // not gated on `audited.is_some()` below — a failed or vanished job never
                // produces a report, so this is the only place a failure would ever surface.
                ScanJobFailedBanner { message: job_failed_message() }
                // Live feedback: open this to watch the AI's actual prompt + output for
                // the audit (so you can trust it's really working, not hung). Shown ONLY for
                // a current-session audit (running or done THIS mount). The transcript lives
                // server-side, so without this gate a remount (e.g. switching cockpit tabs
                // and back) re-renders the PREVIOUS run's transcript while the findings —
                // which are client state — are gone: a confusing half-restored state. Gating
                // it on the same lifecycle as the findings keeps the two consistent (both
                // present during/after an audit, both absent on a fresh remount).
                if auditing() || audited.is_some() {
                    crate::agent_activity::AgentActivity { run_id: "scan-audit".to_string() }
                }
            }

            // ── Findings (after the audit runs) ────────────────────────────────
            if audited.is_some() {
                p { class: "scan-section-h", "Findings" }
                p { class: "scan-section-sub", "The Queue holds what the scan ACCUSES the codebase of — defects and deviations. Triage each into one of four tables: leave it Unresolved, Ignore it (with a reason), save it as Tech debt, or mark it a False positive (with a reason — the tool was wrong; excluded from the report, never baselined). The other three tabs are informational: they assert nothing is wrong and need no triage. When nothing is Unresolved, Process the ignored + tech-debt buckets." }

                // The one-rule-one-surface invariant (design §8), asserted. Silent in the
                // healthy case, which is every case; loud when the pipeline mislabels a rule.
                KindSurfaceConflictBanner { findings: findings.clone() }

                // ── AI-review-did-not-run / AI-review-failed banner ────────────────
                // Compliance-gate fix: a `Blocked` project (API-only, no Anthropic key) or a
                // hard AI-pass failure (bad model id, an API/CLI error, an auth failure) must
                // NEVER look like a silent, clean scan. Rendered UNCONDITIONALLY whenever
                // either field is set on the audited report — including when `findings` is
                // empty, since an empty-but-blocked/failed table is exactly the case that used
                // to look like a clean pass. This is intentionally its own banner (not folded
                // into the gate box above, which only covers the pre-audit `scan()` report):
                // the deterministic floor (and preview linters / dep-audit) may well have run
                // and produced real findings below, so this is a partial-result notice, not a
                // "nothing happened" gate. Extracted into its own component so it's directly
                // SSR-testable without standing up the whole `ScanResults` tree (which needs
                // network resources under `use_resource`).
                AiReviewBlockedBanner {
                    ai_blocked_reason: audited.as_ref().and_then(|a| a.ai_blocked_reason.clone()),
                    ai_error: audited.as_ref().and_then(|a| a.ai_error.clone()),
                }
                // Passes or stages that failed or did not run (spend limit, failures): the scan
                // is INCOMPLETE and must say so wherever its results are read.
                IncompletePassesBanner {
                    failed_passes: audited.as_ref().map(|a| a.failed_passes.clone()).unwrap_or_default(),
                    compact: false,
                    run_id: audited.as_ref().and_then(|a| a.run_id.clone()),
                    plan: resume_plan_shown(),
                    busy: auditing(),
                    on_plan: Some(on_plan),
                    on_resume: Some(on_resume),
                }

                // PRIMARY axis: the kind switcher. Flipping it flips the action vocabulary too,
                // because the decisions genuinely differ — the Queue disposes rows, the three
                // informational panels decide per RULE (and `Not assessed` decides nothing).
                div { class: "kind-switch",
                    for (surface , count) in kind_counts.iter().copied() {
                        {
                            let active = !patterns_open() && kind_view() == surface;
                            rsx! {
                                button {
                                    key: "{surface.label()}",
                                    class: if active { "kind-tab active" } else { "kind-tab" },
                                    onclick: move |_| {
                                        patterns_open.set(false);
                                        kind_view.set(surface);
                                    },
                                    "{surface.label()} "
                                    span { class: "triage-tab-count", "{count}" }
                                    // The alarm chip: rows that reached the UI with no kind at
                                    // all. They live in the Queue, visibly flagged — an
                                    // unmapped emission path is a bug to fix upstream, never a
                                    // silent bucket and never a Defect.
                                    if surface == KindSurface::Queue && n_unspecified > 0 {
                                        span {
                                            class: "kind-unspecified-chip",
                                            title: "These rows reached the UI with no kind. That is an unmapped emission path in the pipeline, not a finding class.",
                                            "{n_unspecified} unspecified"
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // The observation tab, only for a project of several repos.
                    if audited.as_ref().is_some_and(|a| a.patterns_across_services.is_present()) {
                        button {
                            class: if patterns_open() { "kind-tab active" } else { "kind-tab" },
                            onclick: move |_| patterns_open.set(true),
                            "Patterns across services "
                            span { class: "triage-tab-count",
                                "{audited.as_ref().map_or(0, |a| a.patterns_across_services.count_with_titles(pattern_titles().as_ref()))}"
                            }
                        }
                    }
                }

                if patterns_open() {
                    PatternsPanel {
                        view: audited
                            .as_ref()
                            .map(|a| a.patterns_across_services.clone())
                            .unwrap_or_default(),
                        titles: pattern_titles(),
                    }
                } else if kind_view() == KindSurface::Queue
                    && decision_availability()
                        != Some(camerata_ui_core::decisions::Availability::Unknown)
                {
                    // Single-select over the triage tables, each with a live count. The set
                    // and its order come from `TriageState::all()` and the count from
                    // `TriageCounts::of`, so a disposition can never exist without a tab or
                    // sit behind a tab that shows a permanent zero.
                    div { class: "triage-switch",
                        for st in TriageState::all() {
                            {
                                let count = triage_counts.of(st);
                                let active = triage_view() == st;
                                rsx! {
                                    button {
                                        key: "{st.label()}",
                                        class: if active { "triage-tab active" } else { "triage-tab" },
                                        onclick: move |_| triage_view.set(st),
                                        "{st.label()} "
                                        span { class: "triage-tab-count", "{count}" }
                                    }
                                }
                            }
                        }
                    }

                    // Several repos: how much is left to review in each, beside the total above.
                    if repo_counts.len() > 1 {
                        div { class: "triage-repo-counts",
                            for (repo, counts) in repo_counts.iter() {
                                span {
                                    key: "{repo}",
                                    class: "triage-repo-count",
                                    "{repo_counts_line(repo, *counts)}"
                                }
                            }
                        }
                    }

                    // Rule alternatives (audit-integrated alternative recommendation): reviews
                    // every multi-option rule the audit evaluated, doubling as the informational
                    // "group header" for the findings table right below it. Keyed on the audit's
                    // own started_at stamp so a fresh audit run (a different rule selection) gets a
                    // clean remount instead of reusing stale staged-override state.
                    {
                        let recommendations = audited
                            .as_ref()
                            .map(|a| a.recommendations.clone())
                            .unwrap_or_default();
                        let audit_stamp = audited
                            .as_ref()
                            .map(|a| a.provenance.started_at.clone())
                            .unwrap_or_default();
                        rsx! {
                            RuleAlternativesPanel {
                                key: "{audit_stamp}",
                                project_id: project_id.clone(),
                                proposed_rules: report.proposed_rules.clone(),
                                findings: findings.clone(),
                                repos: report.repos.clone(),
                                recommendations,
                                audit,
                            }
                        }
                    }

                    // Wrapped so the key is the first node in its block (Dioxus requirement);
                    // keying on the view remounts the table so its frozen rows reflect the switch.
                    // Fed the QUEUE rows only: the informational kinds have their own panels.
                    {
                        let sha_choice = audited_shas(&report, audited.as_ref());
                        rsx! {
                            FindingsTable {
                                key: "{triage_view().label()}",
                                findings: queue_findings.clone(),
                                repos: report.repos.clone(),
                                descriptions: descriptions.clone(),
                                triage_view: triage_view(),
                                dispositions,
                                order_preset,
                                on_open_hub: Callback::new(move |r: RuleHubRequest| hub_open.set(Some(r))),
                                queue_rule_filter,
                                audited_shas: sha_choice.shas.clone(),
                                sha_conflicts: sha_choice.conflicts.clone(),
                                audited_origins: audited_origins(&report, audited.as_ref()),
                                project_id: project_id.clone(),
                                counts: Some(triage_counts),
                                review: Some(review.clone()),
                                on_open_stats: Callback::new(move |()| stats_open.set(true)),
                            }
                        }
                    }
                } else if kind_view() == KindSurface::Conventions {
                    // The WHOLE finding set, not this surface's rows: per §13 the convention
                    // behind a rule's Deviation rows is adoptable too, and those rows live on
                    // the Queue. `adoptable_conventions` does the routing.
                    ConventionsPanel {
                        findings: findings.clone(),
                        on_open_hub: Callback::new(move |r: RuleHubRequest| hub_open.set(Some(r))),
                        model: adoption(),
                        busy: committing(),
                        only_unopened: unopened_only(),
                        on_clear_filter: Callback::new(move |()| unopened_only.set(false)),
                        on_toggle: Callback::new(move |key: String| {
                            adoption.write().toggle_adopted(&key);
                        }),
                        // THE ONE WRITER of the openness record. Nothing else in this file
                        // calls `mark_opened`, which is what makes the dialog's "not opened"
                        // count a record of a gesture rather than an inference.
                        on_open: Callback::new(move |key: String| {
                            adoption.write().mark_opened(&key);
                        }),
                        on_commit: Callback::new(move |repo: String| {
                            commit_dialog.set(Some(CommitDialogState {
                                surface: KindSurface::Conventions,
                                destination: repo,
                                typed: String::new(),
                            }));
                        }),
                        // §13's "with a jump into the Queue". Byte-for-byte the rule hub's
                        // own jump (a column filter plus the kind switch), so there is one
                        // way to land on a rule's rows rather than two that can diverge.
                        on_review_exceptions: Callback::new(move |rule_id: String| {
                            queue_rule_filter.set(Some(rule_id));
                            kind_view.set(KindSurface::Queue);
                        }),
                    }
                } else if kind_view() == KindSurface::NoConvention {
                    NoConventionPanel {
                        findings: findings_on_surface(&findings, KindSurface::NoConvention),
                        on_open_hub: Callback::new(move |r: RuleHubRequest| hub_open.set(Some(r))),
                        model: adoption(),
                        busy: committing(),
                        on_recommend: Callback::new(move |pick: RecommendationPick| {
                            adoption.write().set_recommendation(&pick.key, pick.option_id);
                        }),
                        on_open: Callback::new(move |key: String| {
                            adoption.write().mark_opened(&key);
                        }),
                        on_commit: Callback::new(move |repo: String| {
                            commit_dialog.set(Some(CommitDialogState {
                                surface: KindSurface::NoConvention,
                                destination: repo,
                                typed: String::new(),
                            }));
                        }),
                    }
                } else {
                    NotAssessedPanel {
                        findings: findings_on_surface(&findings, KindSurface::NotAssessed),
                        on_open_hub: Callback::new(move |r: RuleHubRequest| hub_open.set(Some(r))),
                    }
                }

                // Process: commit the ignored bucket to the baseline and file the tech-debt
                // bucket as tickets. Enabled only once nothing remains OUTSTANDING — which,
                // since Pass 2 step 3, a Confirmed row is not: it is decided, just decided to
                // be real. The gate's whole rule lives in `TriageCounts` (core, unit-tested);
                // `ProcessGate` renders it and this closure performs the commit.
                if !patterns_open() && kind_view() == KindSurface::Queue && (triage_view() == TriageState::TechDebt || triage_counts.outstanding() == 0) {
                    {
                    // QUEUE rows only. Process commits dispositions (baseline waivers,
                    // tickets); an informational row has no disposition to commit, and
                    // including one could only produce a waiver for a compliment.
                    let findings_for_process = queue_findings.clone();
                    rsx! {
                    ProcessGate {
                        counts: triage_counts,
                        processing: processing(),
                        on_process: Callback::new(move |()| {
                                let d = dispositions.read().clone();
                                // Group ignored findings by (repo, reason) -> baseline waiver;
                                // group tech-debt by repo -> a tracked ticket.
                                let mut ignore_groups: std::collections::HashMap<(String, String), Vec<FindingView>> = Default::default();
                                // Tech-debt "resolve later" -> a tracked ticket (GitHub issue), grouped by repo.
                                let mut debt_later: std::collections::HashMap<String, Vec<FindingView>> = Default::default();
                                // Tech-debt "resolve now" -> ALSO a GitHub issue (the story), grouped by repo.
                                // The story makes it into GitHub now (Pillar 1); the dev-engine INGEST of a
                                // resolve-now story is Pillar 2 — same issue, flagged in its title for pickup.
                                let mut debt_now: std::collections::HashMap<String, Vec<FindingView>> = Default::default();
                                for f in &findings_for_process {
                                    let disp = d.get(&finding_key(f)).cloned().unwrap_or_default();
                                    match disp.state {
                                        TriageState::Ignored => {
                                            ignore_groups.entry((f.repo.clone(), disp.reason.clone())).or_default().push(f.clone());
                                        }
                                        TriageState::TechDebt => match disp.bucket {
                                            TechDebtBucket::Later => debt_later.entry(f.repo.clone()).or_default().push(f.clone()),
                                            TechDebtBucket::Now => debt_now.entry(f.repo.clone()).or_default().push(f.clone()),
                                        },
                                        // FalsePositive is a Process-time NO-OP by design (CRITICAL,
                                        // differs from Ignored): it never reaches the baseline and
                                        // never files a ticket. Its only effects are (report export,
                                        // separately) exclusion from the findings section + a
                                        // methodology counter. See `TriageState::FalsePositive`'s doc.
                                        TriageState::FalsePositive => {}
                                        // Confirmed is a Process-time NO-OP too, for a
                                        // different reason than FalsePositive: the finding is
                                        // REAL, so there is nothing to waive and nothing to
                                        // ticket — the client's action item is the finding
                                        // itself, which the report already carries. Owner's
                                        // Q2 ruling (2026-10-12): Confirmed stays local to
                                        // this draft and the export reads it; a durable
                                        // per-finding record across engagements is a backend
                                        // surface Pass 2 does not invent.
                                        TriageState::Confirmed => {}
                                        TriageState::Unresolved => {}
                                        // A merged row is the same problem as another row: there
                                        // is nothing to waive or ticket for it either.
                                        TriageState::MergedInto => {}
                                    }
                                }
                                if ignore_groups.is_empty() && debt_later.is_empty() && debt_now.is_empty() { return; }
                                processing.set(true);
                                spawn(async move {
                                    let mut ok = 0usize;
                                    let mut failed = 0usize;
                                    for ((repo, reason), group) in &ignore_groups {
                                        let r = if reason.trim().is_empty() { "Accepted during onboarding triage".to_string() } else { reason.clone() };
                                        match ignore_findings(repo, group, &r, None).await {
                                            Some(_) => ok += group.len(),
                                            None => failed += group.len(),
                                        }
                                    }
                                    for (repo, group) in &debt_later {
                                        match create_ticket(repo, group, None).await {
                                            Some(_) => ok += group.len(),
                                            None => failed += group.len(),
                                        }
                                    }
                                    // Resolve-now -> a GitHub issue titled so the dev layer (Pillar 2) can pick
                                    // it up for ingest. For Pillar 1 the win is: the story lands in GitHub.
                                    for (repo, group) in &debt_now {
                                        let title = format!("Tech debt (resolve now): {} finding(s) for the dev engine", group.len());
                                        match create_ticket(repo, group, Some(&title)).await {
                                            Some(_) => ok += group.len(),
                                            None => failed += group.len(),
                                        }
                                    }
                                    let msg = format!("Processed {ok} finding(s): ignores → baseline; tech-debt → GitHub issues (resolve-now issues are flagged for the dev engine when Pillar 2 lands).");
                                    if failed == 0 {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, msg);
                                    } else {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Warning, format!("Processed {ok}; {failed} failed (needs GitHub Issues write)."));
                                    }
                                    processing.set(false);
                                });
                        }),
                    }
                    }
                    }
                }

                // ── PDF audit-report export (brownfield audit hardening, Pass B) ──────
                // Board-forwardable export of the CURRENT triage state (including any
                // still-Unresolved findings — a draft mid-engagement report is legitimate;
                // this is never gated on the outstanding count). Project id + the lifted
                // dispositions map come from this component's own state.
                {
                    let pid_report = project_id.clone();
                    let pid_remediation = project_id.clone();
                    rsx! {
                        IncompletePassesBanner {
                            failed_passes: audited.as_ref().map(|a| a.failed_passes.clone()).unwrap_or_default(),
                            compact: true,
                        }
                        AuditReportExportPanel {
                            project_id: pid_report,
                            dispositions,
                        }
                        super::remediation::RemediationPlanPanel { project_id: pid_remediation, dispositions }
                    }
                }

                // ── Deep compliance & security tier output (#55) ──────────────────────
                // Shown only when the audit ran with deep:true and the server returned the
                // three-lens report. Everything here is ADVISORY — never a SOC-2 report or
                // a penetration test. The disclaimer at the top of each lens makes this explicit.
                //
                // pw/cockpit-ui Feature 5: the SOC-2 lens (soc2-gap) is gated by the `soc2`
                // feature flag. When the flag is off, the SOC-2 section is hidden entirely;
                // deep-security and threat-model still render. The flag state comes from the
                // `feature_flags` map fetched at mount time.
                {
                    let soc2_on = feature_flags.soc2;
                    if let Some(deep) = audited.as_ref().and_then(|a| a.deep.clone()) {
                        rsx! {
                        div { class: "deep-tier-panel",
                            p { class: "deep-tier-heading", "Deep compliance & security tier (ADVISORY)" }
                            // Tier-level disclaimer — surfaced prominently before any findings.
                            if !deep.disclaimer.is_empty() {
                                p { class: "deep-tier-disclaimer", "{deep.disclaimer}" }
                            } else {
                                p { class: "deep-tier-disclaimer",
                                    "ADVISORY ONLY. This output is model-inferred from static code. \
                                     It is not a SOC-2 report, not a certification, and not a penetration test. \
                                     Controls that require organisational evidence (policies, HR, vendor contracts) \
                                     cannot be assessed from code alone. A qualified professional must review and \
                                     validate these findings before any compliance or security claim is made."
                                }
                            }
                            // Feature 5: when soc2 flag is OFF, show a notice that the SOC-2
                            // affordance is disabled for this workspace, but do NOT hide the
                            // deep-security or threat-model sections.
                            if !soc2_on {
                                p { class: "deep-soc2-disabled-notice",
                                    "\u{1F512} SOC-2 gap analysis is disabled for this workspace \
                                     (feature flag \u{2018}soc2\u{2019} is off). \
                                     Deep security and threat model results are shown below."
                                }
                            }
                            for lens in deep.lenses.iter() {
                                {
                                    // Feature 5: skip the soc2-gap lens when the flag is off.
                                    if lens.lens == "soc2-gap" && !soc2_on {
                                        rsx! {}
                                    } else {
                                    let (heading, description) = match lens.lens.as_str() {
                                        "soc2-gap"       => ("SOC-2 Readiness Gap Analysis",
                                                              "Maps the repo's detectable practices against SOC-2 Common Criteria and reports gaps. \
                                                               This is a gap analysis, not a SOC-2 report. \
                                                               Controls needing organisational evidence are marked unknown."),
                                        "deep-security"  => ("Deep Security Audit",
                                                              "Authorization, authentication, sensitive-data handling, and injection paths beyond the \
                                                               deterministic floor. Every finding is advisory — a human must validate each one."),
                                        "threat-model"   => ("Threat Model",
                                                              "Entry points, trust boundaries, sensitive-data paths, and STRIDE-flavoured threats with \
                                                               mitigation directions. Model-inferred from the repo structure."),
                                        other            => (other, ""),
                                    };
                                    let lens = lens.clone();
                                    rsx! {
                                        div { class: "deep-lens", key: "{lens.lens}",
                                            p { class: "deep-lens-heading", "{heading}" }
                                            p { class: "deep-lens-desc", "{description}" }
                                            if !lens.disclaimer.is_empty() {
                                                p { class: "deep-lens-disclaimer", "{lens.disclaimer}" }
                                            }
                                            if !lens.summary.is_empty() {
                                                p { class: "deep-lens-summary", "{lens.summary}" }
                                            }
                                            // SOC-2 gap table (only rendered when soc2 flag is on,
                                            // which is guaranteed by the lens filter above — belt + suspenders).
                                            if !lens.soc2_gaps.is_empty() && soc2_on {
                                                div { class: "soc2-gap-table",
                                                    div { class: "soc2-gap-row header",
                                                        span { class: "soc2-col-ctrl", "Control" }
                                                        span { class: "soc2-col-title", "Title" }
                                                        span { class: "soc2-col-status", "Status" }
                                                        span { class: "soc2-col-obs", "Observed" }
                                                        span { class: "soc2-col-gap", "Gap / Remediation" }
                                                    }
                                                    for (i, gap) in lens.soc2_gaps.iter().enumerate() {
                                                        div { key: "{i}", class: "soc2-gap-row soc2-status-{gap.status}",
                                                            span { class: "soc2-col-ctrl", "{gap.control}" }
                                                            span { class: "soc2-col-title", "{gap.title}" }
                                                            span { class: "soc2-col-status soc2-badge-{gap.status}", "{gap.status}" }
                                                            span { class: "soc2-col-obs", "{gap.observed}" }
                                                            span { class: "soc2-col-gap", "{gap.gap}" }
                                                        }
                                                    }
                                                }
                                            }
                                            // Free-text detail (deep-security + threat-model)
                                            if !lens.detail.is_empty() {
                                                pre { class: "deep-lens-detail", "{lens.detail}" }
                                            }
                                        }
                                    }
                                    }
                                }
                            }

                            // pw/cockpit-ui Feature 4: deep-report export button.
                            // Placed at the bottom of the deep-tier panel so it's visible after
                            // reviewing the findings. Project id comes from the active project.
                            {
                                let pid_export = project_id.clone();
                                rsx! {
                                    DeepReportExportPanel {
                                        project_id: pid_export,
                                        soc2_enabled: soc2_on,
                                    }
                                }
                            }
                        }
                        }
                    } else {
                        rsx! {}
                    }
                }

                // ── Optional: wire CI rules into CI (#32) ─────────────────────────
                // Files per-tier STORIES (GitHub issues) per repo to add the selected CI-tier
                // rules to that repo's existing CI as enforced lint gates. Mechanical and
                // architectural land as SEPARATE issues — architectural needs team refinement
                // first. This is OPTIONAL — it does NOT gate "onboarded". Use "Complete
                // onboarding" above to finish at any point; the dev layer picks up each CI
                // story independently.
                if triage_counts.outstanding() == 0 {
                    div { class: "onboard-final-step",
                        span { class: "onboard-step-eyebrow", "Optional: wire CI rules into CI" }
                        CiRulesPanel {
                            repos: report.repos.clone(),
                            rules: ci_rule_items_from_proposed(&report.proposed_rules),
                        }
                        p { class: "section-hint", "Optional, and independent of the tech-debt work above. This files per-tier CI-wiring stories (GitHub issues — one mechanical, one architectural); neither is required to finish onboarding — use \u{201c}Complete onboarding\u{201d} whenever you're ready." }
                    }
                }
            }
        }
    }
}
