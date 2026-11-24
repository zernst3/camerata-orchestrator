//! The shipped decision table (design section 2): one entry per key path of the three saved
//! types. Every decision the design left open is marked `(open)` with the reason.

use super::{enums, DecisionTable, KeyClass, Node, ValueClass};

/// The table's entries.
pub(super) fn build_standard() -> DecisionTable {
    use enums as e;
    use ValueClass::*;
    let mut t = DecisionTable::empty();

    // ── RunRecord ──
    t.leaves(
        "run",
        &[
            ("schema", Number),
            ("attempt", Number),
            ("estimate_usd", Number),
            ("created_at", Timestamp),
            ("updated_at", Timestamp),
            ("status", Enum(e::RUN_STATUS)),
            ("origin", Enum(e::RUN_ORIGIN)),
            ("engine_version", Allowed),
            ("rules_fingerprint", Allowed),
            ("run_id", Drop),
            ("project_id", Drop),
            ("project_key", Drop),
            ("nonce_seed", Drop),
        ],
    );
    // The build, rule library and clone judge a run started on, and its wall-clock segments.
    t.leaves(
        "run.pin.build",
        &[
            ("version", Version),
            ("commit", Hex),
            ("dirty", Bool),
            ("prompt_schema", Number),
            ("clone_prompt_schema", Number),
        ],
    );
    t.leaves(
        "run.pin",
        &[("corpus_version", Version), ("clone_judge_model", ModelId)],
    );
    t.leaves(
        "run.segments[]",
        &[
            ("started_unix_ms", Number),
            ("finished_unix_ms", Number),
            ("elapsed_ms", Number),
        ],
    );
    ai_scan_key(&mut t, "run.ai_scan_key");
    t.leaves(
        "run.models",
        &[
            ("audit", ModelId),
            ("calibration", ModelId),
            ("fix", ModelId),
            ("group_review", ModelId),
            ("finder_model", ModelId),
            ("backend", Enum(e::BACKEND)),
            ("mode", Enum(e::SCAN_MODE)),
            // (open) Not named by the design: a bool and an enum of two engine words.
            ("resolved", Bool),
            ("finder", Enum(e::FINDER)),
        ],
    );
    // (open) `models.*` is ModelId in the design, but the exclusion list holds the client's
    // own glob patterns, and a bare folder name can pass the model id pattern: dropped.
    t.insert("run.models.scan_exclusions[]", Node::Leaf(Drop));
    t.leaves(
        "run.repos[]",
        &[
            ("name", Drop),
            ("dir", Drop),
            ("commit", Drop),
            ("tree_at_start", Drop),
            ("tree_at_end", Drop),
            ("clean", Bool),
        ],
    );
    t.insert("run.args", Node::Args);
    t.leaves(
        "run.plan[]",
        &[
            ("id", PartId),
            ("repo", Repo),
            ("kind", Enum(e::PART_KIND)),
            ("status", Enum(e::PART_STATUS)),
            ("calls_done", Number),
            ("calls_not_done", Number),
            ("updated_at", Timestamp),
            ("note", Text),
        ],
    );
    t.insert("run.incomplete_reasons[]", Node::Leaf(Text));
    t.insert("run.notes[]", Node::Leaf(Text));

    // ── ScanLedger ──
    t.leaves(
        "ledger.rules[]",
        &[
            ("rule_id", RuleId),
            ("repo", Repo),
            ("tier", Enum(e::RULE_TIER)),
            ("ran", Bool),
            ("skip_reason", Text),
            ("files_evaluated", Number),
            ("findings_emitted", Number),
            ("coverage_note", Text),
            ("not_applicable", Bool),
            ("judged_not_a_problem", Bool),
            ("partial_reason", Text),
            ("not_run_reason", Text),
            ("repo_level", Bool),
        ],
    );
    t.insert("ledger.rules[].repos_not_run[]", Node::Leaf(Repo));
    t.leaves(
        "ledger.rules[].scope_coverage",
        &[
            ("checked", Number),
            ("of", Number),
            // (open) An engine word (`files`, `sites`): the skeleton keeps it.
            ("kind", Text),
            ("changed_files_only", Bool),
        ],
    );
    t.leaves(
        "ledger.stages[]",
        &[
            ("stage", Enum(e::LEDGER_STAGE)),
            ("repo", Repo),
            ("rows_in", Number),
            ("rows_out", Number),
            ("routed_held", Number),
            ("routed_informational", Number),
            ("deduped", Number),
            ("unaccounted", Number),
            ("model", ModelId),
        ],
    );
    merged_rows(&mut t, "ledger.stages[].merged_into[]");
    for list in ["other", "added"] {
        t.insert(&format!("ledger.stages[].{list}[][0]"), Node::Leaf(Text));
        t.insert(&format!("ledger.stages[].{list}[][1]"), Node::Leaf(Number));
    }
    t.insert("ledger.not_a_problem_notes[]", Node::Leaf(Text));
    t.leaves(
        "ledger.input",
        &[
            ("files_scanned", Number),
            ("files_excluded", Number),
            ("code_chars", Number),
            ("engine_artifacts_admitted", Number),
            ("skipped_beyond_cap_total", Number),
            ("audit_model", ModelId),
        ],
    );
    t.leaves(
        "ledger.input.skipped_files[]",
        &[
            ("repo", Repo),
            ("path", Path),
            ("reason", Enum(e::SKIP_REASON)),
            ("bytes", Number),
        ],
    );
    t.leaves(
        "ledger.input.skipped_totals",
        &[
            ("oversize", Number),
            ("beyond_file_cap", Number),
            ("not_text", Number),
            ("excluded_by_agreement", Number),
            ("long_line", Number),
            ("excluded_never_read", Number),
            ("generated", Number),
            ("generated_unsearched", Number),
            ("secret_over_budget", Number),
            ("secret_search_ran", Bool),
            ("secret_searched", Number),
            ("secret_over_ceiling", Number),
            ("secret_oversize_not_text", Number),
        ],
    );
    t.leaves(
        "ledger.input.file_cap_hits[]",
        &[("repo", Repo), ("not_scanned", Number), ("limit", Number)],
    );
    t.leaves(
        "ledger.path_hygiene",
        &[
            ("rows_total", Number),
            ("absolute", Number),
            ("backslash", Number),
            ("prose", Number),
            ("offending_rows", Number),
            // (open) Not named by the design: an example row's path, so the skeleton (which
            // hashes the path) rather than the raw value.
            ("worst_example", Text),
        ],
    );
    t.leaves(
        "ledger.evidence",
        &[
            ("checked", Number),
            ("verified", Number),
            ("unverified", Number),
            ("exempt", Number),
            ("site_checked", Number),
            ("site_exempt", Number),
            ("site_verified", Number),
            ("site_unverified", Number),
        ],
    );
    t.leaves(
        "ledger.content",
        &[
            ("rows_seen", Number),
            ("held", Number),
            ("wording_flags", Number),
        ],
    );
    t.insert("ledger.discarded.by_reason", Node::Keys(KeyClass::Text));
    t.insert("ledger.discarded.by_reason.*", Node::Leaf(Number));
    t.leaves(
        "ledger.escalation",
        &[
            ("model", ModelId),
            ("rate_per_kloc", Number),
            ("floor", Number),
            ("counted_lines", Number),
            ("budget", Number),
            ("candidates", Number),
            ("escalated", Number),
            ("candidate_rows", Number),
            ("escalated_rows", Number),
            ("over_budget", Number),
            ("total_sites", Number),
            ("share_escalated", Number),
            ("rules_reviewed", Number),
            ("rules_failed", Number),
        ],
    );
    for map in ["by_reason", "over_budget_by_reason"] {
        t.insert(
            &format!("ledger.escalation.{map}"),
            Node::Keys(KeyClass::Text),
        );
        t.insert(&format!("ledger.escalation.{map}.*"), Node::Leaf(Number));
    }
    t.insert("ledger.escalation.over_budget_reasons[]", Node::Leaf(Text));
    t.insert("ledger.escalation.unplaced[]", Node::Leaf(Text));
    t.leaves(
        "ledger.fix_merge",
        &[
            ("stage", Enum(e::LEDGER_STAGE)),
            ("sub_groups_in", Number),
            ("fixes_out", Number),
        ],
    );
    // (open) Pairs of fix and member ids: the design keeps only the counts, so dropped (the
    // array's length still shows how many were folded).
    t.insert("ledger.fix_merge.folded[][]", Node::Leaf(Drop));
    t.leaves(
        "ledger.place_merge",
        &[
            ("stage", Enum(e::LEDGER_STAGE)),
            ("rows_in", Number),
            ("places_out", Number),
            ("carried", Number),
        ],
    );
    t.leaves(
        "ledger.conventions.rules[]",
        &[
            ("repo", Repo),
            ("rule_id", RuleId),
            ("finder_rows", Number),
            ("departures", Number),
            ("conforming", Number),
            ("unknown", Number),
            ("recorded_places", Number),
            ("departure_rows", Number),
        ],
    );
    t.insert("ledger.conventions.lost_finder_rows", Node::Leaf(Number));
    t.leaves(
        "ledger.group_review",
        &[
            ("stage1_sites", Number),
            ("sites", Number),
            ("groups", Number),
            ("rules_in_play", Number),
            ("files", Number),
        ],
    );
    stage_telemetry(&mut t, "ledger.stage_telemetry");
    ai_scan_key(&mut t, "ledger.ai_key");
    efficiency(&mut t, "ledger.efficiency");
    t.leaves(
        "ledger.cost_cap",
        &[
            ("cap_usd", Number),
            ("spent_usd", Number),
            ("cost_unknown", Bool),
        ],
    );
    not_run(&mut t, "ledger.cost_cap");
    t.insert("ledger.cost_cap.notes[]", Node::Leaf(Text));
    t.leaves(
        "ledger.cost_cap.checkpoint",
        &[
            ("approved_usd", Number),
            ("spent_usd", Number),
            ("projected_usd", Number),
            ("pieces_done", Number),
            ("pieces_left", Number),
            ("stopped", Bool),
            ("new_estimate_usd", Number),
            ("difference_usd", Number),
            ("reserve_usd", Number),
            // (open) An engine sentence naming the piece the run stopped before.
            ("stopped_before", Text),
        ],
    );
    t.leaves(
        "ledger.outage.trigger",
        &[
            ("class", Enum(e::FAILURE_CLASS)),
            ("at", Timestamp),
            // (open) The outage's own sentence: the skeleton.
            ("detail", Text),
        ],
    );
    not_run(&mut t, "ledger.outage");
    t.leaves(
        "ledger.engagement",
        &[
            ("client_engagement", Bool),
            ("backend", Allowed),
            ("provider", Allowed),
            ("client_tooling_opt_in", Bool),
            ("client_tooling_allowed", Bool),
        ],
    );
    t.leaves(
        "ledger.engagement.models[]",
        &[("role", Allowed), ("model", ModelId)],
    );
    t.leaves(
        "ledger.incremental",
        &[
            ("changed", Number),
            ("carried_files", Number),
            ("carried_findings", Number),
            ("prior_sha", Drop),
        ],
    );
    t.insert("ledger.client_suppressions_found", Node::Leaf(Number));
    // (open) The design calls the values globs; the type holds the repos each rule is bound
    // to, so they get repo labels, and the keys (rule ids) the rule id class.
    t.insert("ledger.rule_scopes", Node::Keys(KeyClass::RuleId));
    t.insert("ledger.rule_scopes.*[]", Node::Leaf(Repo));
    t.insert("ledger.runner_notes[]", Node::Leaf(Text));
    t.insert("ledger.finder_coverage[].repo", Node::Leaf(Repo));
    t.leaves(
        "ledger.finder_coverage[].pieces[]",
        &[
            ("piece", Piece),
            ("files_owed", Number),
            ("read_in_full", Number),
            ("read_in_part", Number),
            ("sent_again", Number),
            ("not_reviewed", Number),
            ("context_files_read", Number),
            ("agent_list_matched", Bool),
        ],
    );
    t.insert(
        "ledger.finder_coverage[].pieces[].not_reviewed_files[]",
        Node::Leaf(Path),
    );
    t.leaves(
        "ledger.duplicate_checks.pairs[]",
        &[
            ("verdict", Enum(e::VERDICT)),
            ("model", ModelId),
            // (open) The model's reason about the client's code: the skeleton.
            ("reason", Text),
            // (open) Site keys and fix ids (repo, path and line inside): dropped.
            ("key_a", Drop),
            ("key_b", Drop),
            ("fix_a", Drop),
            ("fix_b", Drop),
        ],
    );
    t.insert("ledger.duplicate_checks.not_run", Node::Leaf(Text));

    // ── FailureReport ──
    t.insert("failures.generated_at", Node::Leaf(Timestamp));
    t.leaves(
        "failures.entries[]",
        &[
            ("repo", Repo),
            ("part", Text),
            ("class", Enum(e::FAILURE_CLASS)),
            ("first_at", Timestamp),
            ("last_at", Timestamp),
            ("resolved_at", Timestamp),
            ("message", Text),
        ],
    );
    t.insert("failures.entries[].files[]", Node::Leaf(Path));
    // `origin` and `cause_origin` (the error pointer, build step 2) are deliberately absent:
    // until step 2 decides them, they are dropped and listed under `undecided`.
    t.leaves(
        "failures.entries[].attempts[]",
        &[
            ("at", Timestamp),
            ("backend", Allowed),
            ("model", ModelId),
            ("http_status", Number),
            ("retry_after_secs", Number),
            ("wait_secs", Number),
            ("exit_code", Number),
            ("error_type", ErrorType),
            ("message", Text),
            ("stderr_tail", Text),
            ("request_id", Drop),
            // (open) Not named by the design: a bool and the checks' reasons.
            ("second_try", Bool),
        ],
    );
    t.insert(
        "failures.entries[].attempts[].rejections[]",
        Node::Leaf(Text),
    );
    for list in ["standard_fixes", "detail_fallbacks"] {
        t.leaves(
            &format!("failures.{list}[]"),
            &[
                ("repo", Repo),
                ("path", Path),
                ("line", Number),
                ("rule_id", RuleId),
                ("reasons", Text),
            ],
        );
    }
    t.insert("failures.runner_notes[]", Node::Leaf(Text));
    // (open) Not named by the design; the same notes as the ledger's `not_a_problem_notes`.
    t.insert("failures.judged_not_a_problem[]", Node::Leaf(Text));
    t
}

fn ai_scan_key(t: &mut DecisionTable, base: &str) {
    use ValueClass::*;
    // `ai_scan_key.*` is ALLOWED: each field is typed as what it holds.
    t.leaves(
        base,
        &[
            ("audit_model", ModelId),
            ("calibration_model", ModelId),
            ("prompt_schema_version", Number),
            ("directives_fingerprint", Allowed),
            ("terse_output", Bool),
            ("applies_to_routing", Bool),
        ],
    );
}

fn efficiency(t: &mut DecisionTable, base: &str) {
    let names: Vec<(&str, ValueClass)> = crate::efficiency::EfficiencyConfig::MEASURES
        .iter()
        .map(|(name, _)| (*name, ValueClass::Bool))
        .collect();
    t.leaves(base, &names);
}

fn merged_rows(t: &mut DecisionTable, base: &str) {
    t.leaves(
        base,
        &[
            ("rule_id", ValueClass::RuleId),
            ("target_rule_id", ValueClass::RuleId),
        ],
    );
}

fn not_run(t: &mut DecisionTable, base: &str) {
    use enums as e;
    use ValueClass::*;
    t.leaves(
        &format!("{base}.not_run_passes[]"),
        &[
            ("repo", Repo),
            ("group", Text),
            ("chunk", Number),
            ("n_chunks", Number),
            ("rule_batch", Number),
            ("n_rule_batches", Number),
            ("class", Enum(e::FAILURE_CLASS)),
        ],
    );
    t.insert(
        &format!("{base}.not_run_passes[].rule_ids[]"),
        Node::Leaf(RuleId),
    );
    t.insert(
        &format!("{base}.not_run_passes[].files[]"),
        Node::Leaf(Path),
    );
    t.leaves(
        &format!("{base}.not_run_stages[]"),
        &[
            ("repo", Repo),
            // (open) An engine stage name (`fix-specific generation`): the skeleton.
            ("stage", Text),
            ("partial", Bool),
            ("class", Enum(e::FAILURE_CLASS)),
        ],
    );
    t.insert(&format!("{base}.repos_not_finished[]"), Node::Leaf(Repo));
}

fn stage_telemetry(t: &mut DecisionTable, base: &str) {
    use ValueClass::*;
    t.insert(&format!("{base}.counted_lines"), Node::Leaf(Number));
    t.insert(
        &format!("{base}.stages[].stage"),
        Node::Leaf(Enum(enums::STAGE)),
    );
    t.leaves(
        &format!("{base}.stages[].usage"),
        &[
            ("calls", Number),
            ("input_tokens", Number),
            ("output_tokens", Number),
            ("cache_read_input_tokens", Number),
            ("cache_creation_input_tokens", Number),
            ("cost_usd", Number),
            ("cost_reported_calls", Number),
            ("wasted_calls", Number),
            ("wasted_input_tokens", Number),
            ("wasted_output_tokens", Number),
            ("wasted_cost_usd", Number),
            ("busy_ms", Number),
            ("wasted_busy_ms", Number),
            ("first_call_unix_ms", Number),
            ("last_call_unix_ms", Number),
            ("batch_wait_ms", Number),
            ("failed_busy_ms", Number),
            ("cache_miss_turns", Number),
        ],
    );
    t.insert(
        &format!("{base}.stages[].usage.models"),
        Node::Keys(KeyClass::ModelId),
    );
    t.insert(
        &format!("{base}.stages[].usage.models.*"),
        Node::Leaf(Number),
    );
    efficiency(t, &format!("{base}.efficiency"));
    t.leaves(
        &format!("{base}.fix_outcome"),
        &[
            ("sent_to_model", Number),
            ("answered", Number),
            ("gave_up", Number),
            ("cap_stopped", Number),
            ("transport_failed", Number),
            ("authored_without_model", Number),
        ],
    );
    t.leaves(
        &format!("{base}.routing"),
        &[("pairs_total", Number), ("pairs_skipped", Number)],
    );
    t.insert(
        &format!("{base}.candidates.rules"),
        Node::Keys(KeyClass::RuleId),
    );
    t.leaves(
        &format!("{base}.candidates.rules.*"),
        &[
            ("locator", Text),
            ("chunks_narrowed", Number),
            ("chunks_without_sites", Number),
            ("chunks_not_narrowed", Number),
            ("chunks_fallback", Number),
            ("digest_bytes_whole", Number),
            ("digest_bytes_sent", Number),
        ],
    );
    t.leaves(
        &format!("{base}.candidates.fallbacks[]"),
        &[
            ("repo", Repo),
            ("group", Text),
            ("chunk", Number),
            ("locator", Text),
            ("reason", Text),
        ],
    );
    t.leaves(
        &format!("{base}.detail_rejections"),
        &[
            ("mismatch", Number),
            ("drops_named_token", Number),
            ("invented_precondition", Number),
            ("missing_id", Number),
            ("truncated", Number),
            ("left_out", Number),
        ],
    );
    t.leaves(
        &format!("{base}.review_flags"),
        &[("detail", Number), ("fix", Number)],
    );
    t.insert(
        &format!("{base}.review_flags.by_kind"),
        Node::Keys(KeyClass::EngineKey),
    );
    t.insert(
        &format!("{base}.review_flags.by_kind.*"),
        Node::Leaf(Number),
    );
    t.leaves(
        &format!("{base}.repo_maps.calls[]"),
        &[
            ("repo", Repo),
            // (open) The chunk's group (a folder of the client's): the skeleton.
            ("group", Text),
            ("chunk", Number),
            ("files_in_map", Number),
            ("files_with_symbols", Number),
            ("outlined_files", Number),
        ],
    );
    t.leaves(
        &format!("{base}.repo_maps.fallbacks[]"),
        &[
            ("repo", Repo),
            ("files", Number),
            ("tree_tokens", Number),
            ("budget", Number),
        ],
    );
    t.leaves(
        &format!("{base}.repo_maps.splits"),
        &[
            ("chunks_split", Number),
            ("parts_added", Number),
            ("files_split_by_lines", Number),
        ],
    );
}
