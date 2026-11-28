use super::*;

fn resp(
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    cost: Option<f64>,
) -> LlmResponse {
    LlmResponse {
        reported_model: None,
        cli_cost_usd: None,
        elapsed_ms: None,
        text: String::new(),
        model: "m".to_string(),
        backend: "api".to_string(),
        cost_usd: cost,
        input_tokens: Some(input),
        output_tokens: Some(output),
        cache_read_input_tokens: cache_read,
        cache_creation_input_tokens: cache_write,
        or_cache_discount: None,
        truncated: false,
    }
}

fn telemetry(counted_lines: usize, rows: &[(Stage, f64)]) -> StageTelemetry {
    let meter = StageMeter::default();
    for (stage, cost) in rows {
        meter.record(*stage, &resp(1000, 100, 0, 0, Some(*cost)));
    }
    meter.snapshot(counted_lines)
}

#[test]
fn each_stage_accumulates_tokens_cache_and_cost_separately() {
    let meter = StageMeter::default();
    meter.record(Stage::ScanPass, &resp(1000, 200, 300, 400, Some(0.5)));
    meter.record(Stage::ScanPass, &resp(2000, 100, 1000, 0, Some(0.25)));
    meter.record(Stage::Calibration, &resp(50, 10, 0, 0, Some(0.01)));

    let t = meter.snapshot(10_000);
    assert_eq!(t.counted_lines, 10_000);
    assert_eq!(
        t.stages.iter().map(|s| s.stage).collect::<Vec<_>>(),
        vec![Stage::ScanPass, Stage::Calibration],
        "only stages that ran, in pipeline order"
    );
    let scan = &t.stages[0].usage;
    assert_eq!(scan.calls, 2);
    assert_eq!(scan.input_tokens, 3000);
    assert_eq!(scan.output_tokens, 300);
    assert_eq!(scan.cache_read_input_tokens, 1300);
    assert_eq!(scan.cache_creation_input_tokens, 400);
    assert!((scan.cost_usd - 0.75).abs() < 1e-9);
    assert!(scan.cost_complete());

    let total = t.total();
    assert_eq!(total.calls, 3);
    assert_eq!(total.input_tokens, 3050);
    assert!((total.cost_usd - 0.76).abs() < 1e-9);
}

#[test]
fn a_call_with_no_reported_cost_marks_the_stage_cost_partial() {
    let meter = StageMeter::default();
    meter.record(Stage::DeepTier, &resp(10, 10, 0, 0, Some(0.1)));
    meter.record(Stage::DeepTier, &resp(10, 10, 0, 0, None));
    let t = meter.snapshot(1000);
    assert!(!t.stages[0].usage.cost_complete());
    assert!(!t.total().cost_complete());
}

#[test]
fn wasted_attempts_are_counted_inside_the_totals_and_never_added_twice() {
    let meter = StageMeter::default();
    let kept = resp(500, 50, 0, 0, Some(0.05));
    let thrown_away = resp(500, 80, 0, 0, Some(0.08));
    meter.record(Stage::FixGeneration, &kept);
    meter.record(Stage::FixGeneration, &thrown_away);
    meter.record_wasted(Stage::FixGeneration, &thrown_away);

    let u = meter.snapshot(1000).stages[0].usage.clone();
    assert_eq!(u.calls, 2, "wasted is a subset of calls, not extra calls");
    assert_eq!(u.input_tokens, 1000);
    assert_eq!(u.output_tokens, 130);
    assert_eq!(u.wasted_calls, 1);
    assert_eq!(u.wasted_input_tokens, 500);
    assert_eq!(u.wasted_output_tokens, 80);
    assert!((u.wasted_cost_usd - 0.08).abs() < 1e-9);
}

#[test]
fn a_partly_discarded_call_prorates_its_wasted_tokens_and_cost() {
    let meter = StageMeter::default();
    let r = resp(1000, 400, 0, 0, Some(1.0));
    meter.record(Stage::FixGeneration, &r);
    // One of four fixes in the batch was rejected.
    meter.record_wasted_share(Stage::FixGeneration, &r, 0.25);
    // A share outside 0..=1 is clamped, never a negative or inflated waste.
    meter.record_wasted_share(Stage::FixGeneration, &r, 7.0);
    meter.record_wasted_share(Stage::FixGeneration, &r, -3.0);

    let u = meter.snapshot(1000).stages[0].usage.clone();
    assert_eq!(u.calls, 1);
    assert_eq!(u.wasted_calls, 3);
    assert_eq!(u.wasted_input_tokens, 250 + 1000);
    assert_eq!(u.wasted_output_tokens, 100 + 400);
    assert!((u.wasted_cost_usd - (0.25 + 1.0)).abs() < 1e-9);
}

#[test]
fn cost_per_1k_lines_uses_the_counted_line_total() {
    let t = telemetry(
        20_000,
        &[(Stage::ScanPass, 3.0), (Stage::FixGeneration, 1.0)],
    );
    let per_1k = t.cost_per_1k_lines().expect("lines were counted");
    assert!(
        (per_1k - 0.2).abs() < 1e-9,
        "$4 over 20K lines is $0.20 per 1K, got {per_1k}"
    );
    assert_eq!(
        telemetry(0, &[(Stage::ScanPass, 1.0)]).cost_per_1k_lines(),
        None
    );
}

#[test]
fn the_table_names_each_stage_the_cache_columns_and_the_per_1k_cost() {
    let meter = StageMeter::default();
    meter.record(Stage::ScanPass, &resp(9000, 700, 4000, 2000, Some(2.0)));
    let wasted = resp(100, 40, 0, 0, Some(0.02));
    meter.record(Stage::FixGeneration, &wasted);
    meter.record_wasted(Stage::FixGeneration, &wasted);
    let table = meter.snapshot(8_000).render_table();

    for needle in [
        "STAGE TELEMETRY",
        "stage",
        "calls",
        "input",
        "output",
        "cache-rd",
        "cache-wr",
        "cost",
        "$/1K lines",
        "wasted",
        "scan",
        "fix",
        "total",
        "counted lines: 8000",
    ] {
        assert!(table.contains(needle), "missing `{needle}` in:\n{table}");
    }
    assert!(
        !table.contains("calibration"),
        "a stage that made no call has no row:\n{table}"
    );
    // scan: $2.00 over 8K lines = $0.2500 per 1K.
    assert!(table.contains("0.2500"), "{table}");
}

#[test]
fn an_empty_telemetry_says_no_model_call_ran() {
    let table = StageMeter::default().snapshot(500).render_table();
    assert!(table.contains("no model call"), "{table}");
}

#[test]
fn a_cost_rise_past_the_threshold_is_flagged_and_names_the_stage() {
    let base = telemetry(
        10_000,
        &[(Stage::ScanPass, 8.0), (Stage::FixGeneration, 2.0)],
    );
    // Scan stays; fix doubles. Overall $10 -> $12 = +20%.
    let cur = telemetry(
        10_000,
        &[(Stage::ScanPass, 8.0), (Stage::FixGeneration, 4.0)],
    );
    let r = cost_regression(&base, &cur, DEFAULT_COST_REGRESSION_PCT);
    assert!(r.is_regression(), "{r:?}");
    assert!((r.change_pct.unwrap() - 20.0).abs() < 1e-6);
    assert_eq!(r.stage_regressions.len(), 1);
    assert_eq!(r.stage_regressions[0].stage, Stage::FixGeneration);
    assert!(r.render().contains("REGRESSION"), "{}", r.render());
}

#[test]
fn a_rise_within_the_threshold_or_a_fall_passes() {
    let base = telemetry(10_000, &[(Stage::ScanPass, 10.0)]);
    let up_ten = telemetry(10_000, &[(Stage::ScanPass, 11.0)]);
    let down = telemetry(10_000, &[(Stage::ScanPass, 6.0)]);
    assert_eq!(
        cost_regression(&base, &up_ten, 15.0).verdict,
        RegressionVerdict::Within
    );
    assert_eq!(
        cost_regression(&base, &down, 15.0).verdict,
        RegressionVerdict::Within
    );
}

#[test]
fn the_comparison_is_per_counted_line_not_per_total_dollar() {
    // The same per-1K cost on a repo twice the size is NOT a regression.
    let base = telemetry(10_000, &[(Stage::ScanPass, 5.0)]);
    let bigger = telemetry(20_000, &[(Stage::ScanPass, 10.0)]);
    assert_eq!(
        cost_regression(&base, &bigger, 15.0).verdict,
        RegressionVerdict::Within
    );
}

#[test]
fn records_that_cannot_be_compared_are_not_a_pass() {
    let base = telemetry(10_000, &[(Stage::ScanPass, 5.0)]);
    let no_lines = telemetry(0, &[(Stage::ScanPass, 5.0)]);
    let no_cost = StageMeter::default().snapshot(10_000);
    let partial = {
        let m = StageMeter::default();
        m.record(Stage::ScanPass, &resp(1, 1, 0, 0, None));
        m.snapshot(10_000)
    };
    for other in [&no_lines, &no_cost, &partial] {
        let r = cost_regression(&base, other, 15.0);
        assert_eq!(r.verdict, RegressionVerdict::NotComparable, "{r:?}");
        assert!(!r.reason.is_empty());
        assert!(!r.is_regression(), "not-comparable is its own verdict");
        let r = cost_regression(other, &base, 15.0);
        assert_eq!(r.verdict, RegressionVerdict::NotComparable, "{r:?}");
    }
}

#[test]
fn telemetry_round_trips_through_json_so_a_ledger_can_carry_it() {
    let t = telemetry(7_000, &[(Stage::ScanPass, 1.5), (Stage::CloneJudge, 0.5)]);
    let json = serde_json::to_string(&t).unwrap();
    let back: StageTelemetry = serde_json::from_str(&json).unwrap();
    assert_eq!(t, back);
}

/// Rank 10: each call is counted against the model that served it, per stage and in the total,
/// and the fix give-up tally sums across repos.
#[test]
fn calls_are_counted_per_model_and_the_fix_tally_sums() {
    let meter = StageMeter::default();
    let on = |model: &str| LlmResponse {
        model: model.to_string(),
        ..resp(10, 1, 0, 0, Some(0.01))
    };
    meter.record(Stage::ScanPass, &on("claude-sonnet-5"));
    meter.record(Stage::FixGeneration, &on("claude-haiku-5-5"));
    meter.record(Stage::FixGeneration, &on("claude-haiku-5-5"));
    meter.record(Stage::FixGeneration, &on("claude-sonnet-5"));
    // (sent, answered, gave_up, cap_stopped, transport_failed, authored)
    for (sent, answered, gave_up, cap, transport, authored) in
        [(4, 3, 1, 1, 0, 2), (6, 5, 2, 0, 1, 0)]
    {
        meter.record_fix_outcome(FixOutcome {
            sent_to_model: sent,
            answered,
            gave_up,
            cap_stopped: cap,
            transport_failed: transport,
            authored_without_model: authored,
        });
    }

    let t = meter.snapshot(0);
    let fix = &t
        .stages
        .iter()
        .find(|s| s.stage == Stage::FixGeneration)
        .unwrap()
        .usage;
    assert_eq!(
        fix.render_models(),
        "claude-haiku-5-5 x2, claude-sonnet-5 x1"
    );
    assert_eq!(t.total().models.get("claude-sonnet-5"), Some(&2));
    let outcome = t.fix_outcome.unwrap();
    assert_eq!(
        (
            outcome.sent_to_model,
            outcome.answered,
            outcome.gave_up,
            outcome.cap_stopped,
            outcome.transport_failed,
            outcome.authored_without_model
        ),
        (10, 8, 3, 1, 1, 2)
    );
    assert_eq!(
        outcome.give_up_rate(),
        Some(3.0 / 8.0),
        "the rate is over the findings the model answered, not over those sent"
    );
    let table = t.render_table();
    assert!(
        table.contains("fix model: claude-haiku-5-5 x2, claude-sonnet-5 x1"),
        "{table}"
    );
    assert!(
        table.contains(
            "fix give-up rate: 37.5% (3 of the 8 findings the fix model answered ended with no \
             fix that passed the self-check; 2 findings were not answered: 1 stopped by the \
             spend limit, 1 failed in transport; 2 used"
        ),
        "{table}"
    );
    assert!(
        !table.contains("answered by the fix model"),
        "the figure counts the findings that gave up, never those answered: {table}"
    );

    assert_eq!(
        StageMeter::default().snapshot(0).fix_outcome,
        None,
        "no fix stage, no tally"
    );
}

/// The CLI backend echoes the model it was asked for and never reports what served the call, so
/// its calls are counted, and printed, as the REQUESTED model; the API's are the served model.
#[test]
fn cli_calls_are_counted_as_the_requested_model() {
    let meter = StageMeter::default();
    let on = |model: &str, backend: &str| LlmResponse {
        model: model.to_string(),
        backend: backend.to_string(),
        ..resp(10, 1, 0, 0, Some(0.01))
    };
    meter.record(Stage::FixGeneration, &on("claude-haiku-5-5", "cli"));
    meter.record(Stage::FixGeneration, &on("claude-haiku-5-5", "cli"));
    meter.record(Stage::FixGeneration, &on("claude-haiku-5-5", "api"));
    let t = meter.snapshot(0);
    let fix = &t
        .stages
        .iter()
        .find(|s| s.stage == Stage::FixGeneration)
        .unwrap()
        .usage;
    assert_eq!(fix.models.get("claude-haiku-5-5 (requested)"), Some(&2));
    assert_eq!(fix.models.get("claude-haiku-5-5"), Some(&1));
    assert_eq!(fix.models.values().sum::<u64>(), fix.calls);
    assert!(
        t.render_table()
            .contains("fix model: claude-haiku-5-5 x1, claude-haiku-5-5 (requested) x2"),
        "{}",
        t.render_table()
    );
}

fn timed(model: &str, elapsed_ms: u64) -> LlmResponse {
    LlmResponse {
        model: model.to_string(),
        elapsed_ms: Some(elapsed_ms),
        ..resp(100, 10, 0, 0, Some(0.1))
    }
}

#[test]
fn a_stage_sums_busy_time_and_spans_its_calls_even_when_they_overlap() {
    let meter = StageMeter::default();
    // Two calls in flight at once: A ran 9_000..10_000 (1_000 ms), B ran 9_500..10_200 (700 ms).
    meter.record_at(Stage::ScanPass, &timed("model-a", 1_000), 10_000);
    meter.record_at(Stage::ScanPass, &timed("model-b", 700), 10_200);
    meter.record_at(Stage::Calibration, &timed("model-a", 50), 20_000);

    let t = meter.snapshot(1_000);
    let scan = &t.stages[0].usage;
    assert_eq!(scan.busy_ms, 1_700, "the call times add up");
    assert_eq!(
        scan.first_call_unix_ms,
        Some(9_000),
        "the earliest call's start"
    );
    assert_eq!(
        scan.last_call_unix_ms,
        Some(10_200),
        "the latest call's end"
    );
    assert_eq!(
        scan.models,
        BTreeMap::from([("model-a".to_string(), 1), ("model-b".to_string(), 1)])
    );
    let total = t.total();
    assert_eq!(total.busy_ms, 1_750);
    assert_eq!(total.first_call_unix_ms, Some(9_000));
    assert_eq!(total.last_call_unix_ms, Some(20_000));
}

#[test]
fn a_wasted_attempt_counts_toward_busy_time_once() {
    let meter = StageMeter::default();
    let discarded = timed("model-a", 400);
    meter.record_at(Stage::FixGeneration, &discarded, 5_000);
    meter.record_at(Stage::FixGeneration, &timed("model-a", 600), 6_000);
    meter.record_wasted(Stage::FixGeneration, &discarded);

    let fix = &meter.snapshot(0).stages[0].usage;
    assert_eq!(
        fix.busy_ms, 1_000,
        "the discarded attempt is inside busy time, once"
    );
    assert_eq!(fix.wasted_busy_ms, 400, "and named as wasted");
    assert_eq!(fix.calls, 2);
}

#[test]
fn a_call_with_no_timing_adds_no_time_and_no_stamps() {
    let meter = StageMeter::default();
    meter.record_at(Stage::ScanPass, &resp(1, 1, 0, 0, None), 7_000);
    let scan = &meter.snapshot(0).stages[0].usage;
    assert_eq!(scan.calls, 1);
    assert_eq!(scan.busy_ms, 0);
    assert_eq!(scan.first_call_unix_ms, None);
    assert_eq!(scan.last_call_unix_ms, None);
}

#[test]
fn telemetry_with_run_figures_round_trips_and_an_older_record_still_loads() {
    let meter = StageMeter::default();
    meter.record_at(Stage::ScanPass, &timed("model-a", 1_000), 10_000);
    let t = meter.snapshot(500);
    let json = serde_json::to_string(&t).unwrap();
    let back: StageTelemetry = serde_json::from_str(&json).unwrap();
    assert_eq!(back, t, "a report with the new fields loads back equal");

    // A record written before the figures existed: strip them.
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    for stage in value["stages"].as_array_mut().unwrap() {
        let usage = stage["usage"].as_object_mut().unwrap();
        for key in [
            "busy_ms",
            "wasted_busy_ms",
            "first_call_unix_ms",
            "last_call_unix_ms",
        ] {
            assert!(usage.remove(key).is_some(), "{key} was serialized");
        }
    }
    let old: StageTelemetry = serde_json::from_value(value).expect("an older record loads");
    let usage = &old.stages[0].usage;
    assert_eq!((usage.busy_ms, usage.first_call_unix_ms), (0, None));
    assert_eq!(usage.calls, 1);
}

#[test]
fn batch_rounds_widen_the_stage_span_and_add_their_wait_apart_from_busy_time() {
    let meter = StageMeter::default();
    // Two rounds of the same stage (a batch and its resubmission); the rows carry no timing.
    meter.record_batch_round(Stage::ScanPass, 1_000, 4_000);
    meter.record_batch_round(Stage::ScanPass, 4_500, 5_000);
    meter.record(Stage::ScanPass, &resp(10, 1, 0, 0, Some(0.1)));
    let scan = &meter.snapshot(0).stages[0].usage;
    assert_eq!(scan.batch_wait_ms, 3_500);
    assert_eq!(scan.busy_ms, 0, "a wait is not call time");
    assert_eq!(scan.first_call_unix_ms, Some(1_000));
    assert!(scan.last_call_unix_ms >= Some(5_000));
}

// #243: cache hit share per stage. `input_tokens` already includes cache read and write, so the
// share is `cache_read_input_tokens / input_tokens`, never read over (input + read).
#[test]
fn the_cache_hit_share_is_cache_reads_over_billed_input() {
    let meter = StageMeter::default();
    meter.record(Stage::ScanPass, &resp(9000, 700, 4000, 2000, Some(2.0)));
    meter.record(Stage::ScanPass, &resp(1000, 100, 1000, 0, Some(0.1)));
    meter.record(Stage::FixGeneration, &resp(2000, 50, 0, 1500, Some(0.2)));
    let t = meter.snapshot(1_000);

    let scan = t.cache_hit_share(Stage::ScanPass).expect("scan had input");
    assert!((scan - 0.5).abs() < 1e-9, "5000 of 10000 read, got {scan}");
    let fix = t
        .cache_hit_share(Stage::FixGeneration)
        .expect("fix had input");
    assert!(fix.abs() < 1e-9, "a cache write is not a hit, got {fix}");
    let total = t.total().cache_hit_share().expect("the run had input");
    assert!((total - 5000.0 / 12000.0).abs() < 1e-9, "got {total}");
    assert_eq!(
        t.cache_hit_share(Stage::Calibration),
        None,
        "a stage that made no call has no share"
    );
}

#[test]
fn a_stage_with_no_input_has_no_cache_hit_share_and_the_table_says_n_a() {
    assert_eq!(StageUsage::default().cache_hit_share(), None);
    // Inconsistent data (reads reported, no input total) is not shown as a share above 100%.
    let odd = StageUsage {
        calls: 1,
        cache_read_input_tokens: 10,
        ..StageUsage::default()
    };
    assert_eq!(odd.cache_hit_share(), None);

    let meter = StageMeter::default();
    meter.record(Stage::ScanPass, &resp(0, 10, 0, 0, Some(0.0)));
    let t = meter.snapshot(100);
    assert_eq!(t.cache_hit_share(Stage::ScanPass), None);
    let table = t.render_table();
    let scan_row = table
        .lines()
        .find(|l| l.trim_start().starts_with("scan"))
        .unwrap_or_else(|| panic!("no scan row in:\n{table}"));
    assert!(scan_row.trim_end().ends_with("n/a"), "{scan_row}");
}

#[test]
fn a_snapshot_saved_before_the_cache_hit_share_existed_still_loads() {
    // The shape a run saved before #243: no share anywhere, and none of the later defaults.
    let json = r#"{
        "counted_lines": 400,
        "stages": [{
            "stage": "scan_pass",
            "usage": {
                "calls": 2,
                "input_tokens": 8000,
                "output_tokens": 300,
                "cache_read_input_tokens": 2000,
                "cache_creation_input_tokens": 1000,
                "cost_usd": 0.5
            }
        }]
    }"#;
    let old: StageTelemetry = serde_json::from_str(json).expect("an older snapshot loads");
    let share = old.cache_hit_share(Stage::ScanPass).expect("it had input");
    assert!((share - 0.25).abs() < 1e-9, "got {share}");
    assert!(
        old.render_table().contains("25.0%"),
        "{}",
        old.render_table()
    );
}

#[test]
fn the_table_shows_the_cache_hit_share_column() {
    let meter = StageMeter::default();
    meter.record(Stage::ScanPass, &resp(9000, 700, 4000, 2000, Some(2.0)));
    let table = meter.snapshot(8_000).render_table();
    let header = table
        .lines()
        .find(|l| l.contains("calls") && l.contains("cache-rd"))
        .unwrap_or_else(|| panic!("no header in:\n{table}"));
    assert!(header.contains("cache-hit"), "{header}");
    // 4000 of 9000 billed input tokens were cache reads.
    let scan_row = table
        .lines()
        .find(|l| l.trim_start().starts_with("scan"))
        .unwrap_or_else(|| panic!("no scan row in:\n{table}"));
    assert!(scan_row.trim_end().ends_with("44.4%"), "{scan_row}");
    let total_row = table
        .lines()
        .find(|l| l.trim_start().starts_with("total"))
        .unwrap_or_else(|| panic!("no total row in:\n{table}"));
    assert!(total_row.trim_end().ends_with("44.4%"), "{total_row}");
}

/// #584: each call is counted as batch (a Message Batches row) or live, and the counts survive a
/// save. A record written before the field existed loads with none.
#[test]
fn each_stage_counts_its_calls_by_mode() {
    let meter = StageMeter::default();
    let mut batch = resp(10, 1, 0, 0, None);
    batch.backend = "api/batch".to_string();
    meter.record(Stage::ScanPass, &batch);
    meter.record(Stage::ScanPass, &batch);
    meter.record(Stage::ScanPass, &resp(10, 1, 0, 0, None));
    let mut cli = resp(10, 1, 0, 0, None);
    cli.backend = "cli".to_string();
    meter.record(Stage::Calibration, &cli);
    let t = meter.snapshot(0);
    let saved = serde_json::to_value(&t).unwrap();
    assert_eq!(saved["stages"][0]["usage"]["modes"]["batch"], 2);
    assert_eq!(saved["stages"][0]["usage"]["modes"]["live"], 1);
    assert_eq!(saved["stages"][1]["usage"]["modes"]["live"], 1);
    assert_eq!(saved["stages"][1]["usage"]["modes"].get("batch"), None);
    let total = serde_json::to_value(t.total()).unwrap();
    assert_eq!(total["modes"]["live"], 2, "the total sums the modes");

    let mut older = saved;
    older["stages"][0]["usage"]
        .as_object_mut()
        .unwrap()
        .remove("modes");
    let back: StageTelemetry = serde_json::from_value(older).unwrap();
    assert!(back.stages[0].usage.modes.is_empty());
}
