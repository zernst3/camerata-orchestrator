use super::*;
use crate::llm::LlmResponse;
use crate::stage_telemetry::{FixOutcome, StageMeter};

fn call(model: &str, backend: &str) -> LlmResponse {
    LlmResponse {
        reported_model: None,
        cli_cost_usd: None,
        elapsed_ms: Some(5),
        text: String::new(),
        model: model.to_string(),
        backend: backend.to_string(),
        cost_usd: Some(0.5),
        input_tokens: Some(100),
        output_tokens: Some(10),
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        or_cache_discount: None,
        truncated: false,
    }
}

fn map(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

#[test]
fn the_record_copies_each_stage_s_models_and_modes_and_the_fix_fallbacks() {
    let meter = StageMeter::default();
    meter.record(Stage::ScanPass, &call("acme-scan", "api/batch"));
    meter.record(Stage::ScanPass, &call("acme-scan", "api"));
    meter.record(Stage::FixGeneration, &call("acme-fix", "cli"));
    meter.record_fix_outcome(FixOutcome {
        sent_to_model: 6,
        answered: 4,
        gave_up: 2,
        cap_stopped: 1,
        transport_failed: 1,
        authored_without_model: 3,
    });
    let record = RecordedStageModels::from_telemetry(&meter.snapshot(0));
    assert_eq!(
        record.stages,
        vec![
            StageModelRecord {
                stage: Stage::ScanPass,
                models: map(&[("acme-scan", 2)]),
                modes: map(&[("batch", 1), ("live", 1)]),
            },
            StageModelRecord {
                stage: Stage::FixGeneration,
                models: map(&[("acme-fix (requested)", 1)]),
                modes: map(&[("live", 1)]),
            },
        ]
    );
    // Sent to the model and ended on the standard fix: gave up, stopped by the limit, failed.
    assert_eq!(record.standard_fix_fallbacks, Some(4));
}

/// The record holds model ids and modes only: no cost, token or timing figure reaches it.
#[test]
fn the_record_carries_no_cost_or_timing_figure() {
    let meter = StageMeter::default();
    meter.record(Stage::GroupReview, &call("acme-review", "api"));
    let record = RecordedStageModels::from_telemetry(&meter.snapshot(400));
    let text = serde_json::to_string(&record).unwrap();
    for term in ["cost", "0.5", "token", "busy", "elapsed", "unix", "calls\""] {
        assert!(!text.contains(term), "`{term}` in {text}");
    }
    assert!(text.contains("acme-review"), "{text}");
}

#[test]
fn a_later_stage_replaces_its_own_record_and_keeps_pipeline_order() {
    let scan = StageMeter::default();
    scan.record(Stage::ScanPass, &call("acme-scan", "api"));
    scan.record(Stage::GroupReview, &call("acme-old-review", "api"));
    let mut record = RecordedStageModels::from_telemetry(&scan.snapshot(0));

    let later = StageMeter::default();
    later.record(Stage::DuplicateCheck, &call("acme-dup", "api/batch"));
    later.record(Stage::GroupReview, &call("acme-review", "api"));
    record.absorb(&later.snapshot(0));
    assert_eq!(
        record.stages.iter().map(|s| s.stage).collect::<Vec<_>>(),
        vec![Stage::ScanPass, Stage::GroupReview, Stage::DuplicateCheck]
    );
    assert_eq!(
        record.stage(Stage::GroupReview).unwrap().models,
        map(&[("acme-review", 1)])
    );
    assert_eq!(
        record.stage(Stage::DuplicateCheck).unwrap().modes,
        map(&[("batch", 1)])
    );
}

#[test]
fn the_ledger_keeps_the_record_through_a_save_and_an_older_ledger_has_none() {
    let meter = StageMeter::default();
    meter.record(Stage::Finder, &call("acme-finder", "api"));
    let mut ledger = crate::scan_ledger::ScanLedger::new();
    ledger.record_model_stages(RecordedStageModels::from_telemetry(&meter.snapshot(0)));
    let saved = serde_json::to_value(&ledger).unwrap();
    let back: crate::scan_ledger::ScanLedger = serde_json::from_value(saved.clone()).unwrap();
    assert_eq!(
        back.model_stages()
            .and_then(|r| r.stage(Stage::Finder))
            .map(|s| s.models.clone()),
        Some(map(&[("acme-finder", 1)]))
    );
    let mut older = saved;
    older.as_object_mut().unwrap().remove("model_stages");
    let older: crate::scan_ledger::ScanLedger = serde_json::from_value(older).unwrap();
    assert!(older.model_stages().is_none());
}

/// A call counted outside the meter (a recheck's re-asked part) adds to what the stage holds,
/// in model and in mode, and a new stage joins in pipeline order.
#[test]
fn calls_added_outside_the_meter_add_to_the_stage() {
    let meter = StageMeter::default();
    meter.record(Stage::ScanPass, &call("acme-scan", "api"));
    meter.record(Stage::CloneJudge, &call("acme-judge", "api"));
    meter.record(Stage::CloneJudge, &call("acme-judge", "api"));
    let mut record = RecordedStageModels::from_telemetry(&meter.snapshot(0));
    record.add_calls(Stage::CloneJudge, "acme-judge", "live", 1);
    record.add_calls(Stage::Calibration, "acme-cal (requested)", "live", 2);
    assert_eq!(
        record.stage(Stage::CloneJudge).unwrap().models,
        map(&[("acme-judge", 3)])
    );
    assert_eq!(
        record.stage(Stage::CloneJudge).unwrap().modes,
        map(&[("live", 3)])
    );
    assert_eq!(
        record.stages.iter().map(|s| s.stage).collect::<Vec<_>>(),
        vec![Stage::ScanPass, Stage::Calibration, Stage::CloneJudge]
    );
    assert_eq!(
        record.stage(Stage::Calibration).unwrap().models,
        map(&[("acme-cal (requested)", 2)])
    );
}

/// A record started for a run saved before the record existed says so through a save, and a
/// later stage folded in keeps it partial.
#[test]
fn a_partial_record_stays_partial_through_a_save_and_a_later_stage() {
    let mut record = RecordedStageModels::partial();
    let later = StageMeter::default();
    later.record(Stage::DuplicateCheck, &call("acme-dup", "api"));
    record.absorb(&later.snapshot(0));
    let back: RecordedStageModels =
        serde_json::from_value(serde_json::to_value(&record).unwrap()).unwrap();
    assert!(back.partial);
    assert_eq!(back.stages.len(), 1);
    let full = RecordedStageModels::from_telemetry(&later.snapshot(0));
    assert!(!serde_json::to_string(&full).unwrap().contains("partial"));
}
