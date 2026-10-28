#![allow(clippy::unwrap_used)]
//! END-TO-END (scripted model, zero spend): which model and effort the fix stage calls, through
//! the real brownfield pipeline (`onboard::audit_repos_with_llm_controlled`), rank 10.
//!
//! The fixture's only finding comes from the always-on secrets floor (a runtime-built key in a
//! source file, so the fix prompt has surrounding code and the no-code pre-filter does not take
//! it). The scripted model records every fix call and answers with no fix, so each run also
//! exercises the give-up path and its telemetry.
//!
//! Pinned here:
//!   1. precedence: an explicit fix model wins; else Haiku with `fix_on_haiku` on (the default);
//!   2. `fix_on_haiku` off reproduces the pre-rank-10 choice exactly (`calibration_model.or(model)`,
//!      and no model at all when neither is set);
//!   3. the chosen model and effort reach the completer (effort only on the Haiku path);
//!   4. the operator telemetry records the fix stage's model per call and the give-up rate.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use async_trait::async_trait;
use camerata_server::ai_audit::{ScanMode, FIX_STAGE_EFFORT, FIX_STAGE_HAIKU_MODEL};
use camerata_server::cost_cap::RunControls;
use camerata_server::efficiency::EfficiencyConfig;
use camerata_server::llm::{BackendResolution, LlmPort, LlmRequest, LlmResponse};
use camerata_server::onboard::{self, ScanReport};

/// What one fix call asked for.
#[derive(Debug, Clone, PartialEq)]
struct FixCall {
    model: String,
    effort: Option<String>,
}

/// Answers every call with an empty, well-formed result and records each fix-stage request.
/// With `fix_calls_unpriced` the fix calls report tokens but no dollar cost, so a spend-limited
/// run has to price them from the model registry; every other call reports `$0.001`.
#[derive(Default)]
struct Recorder {
    fix_calls: Mutex<Vec<FixCall>>,
    other_calls: Mutex<usize>,
    fix_calls_unpriced: bool,
}

impl Recorder {
    fn fix_calls(&self) -> Vec<FixCall> {
        self.fix_calls.lock().unwrap().clone()
    }
}

fn is_fix_call(req: &LlmRequest) -> bool {
    req.system
        .as_deref()
        .is_some_and(|s| s.contains("concrete, codebase-specific fix per finding"))
}

#[async_trait]
impl LlmPort for Recorder {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let fix = is_fix_call(&req);
        if fix {
            self.fix_calls.lock().unwrap().push(FixCall {
                model: req.model.clone(),
                effort: req.effort.clone(),
            });
        } else {
            *self.other_calls.lock().unwrap() += 1;
        }
        Ok(LlmResponse {
            text: r#"{"findings":[],"proposed_rules":[],"needs_files":[],"verdicts":[],"fixes":[],"recommendations":[]}"#
                .to_string(),
            model: req.model.clone(),
            backend: "stub".to_string(),
            cost_usd: (!(fix && self.fix_calls_unpriced)).then_some(0.001),
            input_tokens: Some(1_000),
            output_tokens: Some(100),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        })
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let r = self.complete(req).await?;
        on_delta(&r.text);
        Ok(r)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// A runtime-built Google API key: no key-shaped literal lives in this source.
fn google_key() -> String {
    format!(
        "AIza{}",
        camerata_secrets::fixtures::body(
            35,
            "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
            1010
        )
    )
}

fn stage_repo(dest: &Path) {
    std::fs::create_dir_all(dest.join("src")).unwrap();
    std::fs::write(
        dest.join("src/maps.ts"),
        format!(
            "export function mapsUrl(q: string): string {{\n  const key = \"{}\";\n  return `https://maps.example.test/?q=${{q}}&key=${{key}}`;\n}}\n",
            google_key()
        ),
    )
    .unwrap();
}

async fn scan(
    llm: &Recorder,
    dir: &Path,
    model: Option<&str>,
    calibration_model: Option<&str>,
    controls: RunControls,
) -> ScanReport {
    let sources = vec![("acme/maps".to_string(), dir.to_path_buf())];
    let (report, _manifest) = onboard::audit_repos_with_llm_controlled(
        llm,
        &sources,
        &[],
        Vec::new(),
        model,
        calibration_model,
        ScanMode::Parallel,
        false,
        None,
        None,
        None,
        false,
        false,
        true,
        true,
        BackendResolution::Api,
        None,
        &HashMap::new(),
        controls,
    )
    .await;
    report
}

fn switch_off() -> EfficiencyConfig {
    let mut efficiency = EfficiencyConfig::default();
    efficiency.set("fix_on_haiku", false).unwrap();
    efficiency
}

/// Run one scan and return the fix calls it made (at least one: the fixture always has a
/// finding with code around it).
async fn fix_calls_for(
    model: Option<&str>,
    calibration_model: Option<&str>,
    controls: RunControls,
) -> (Vec<FixCall>, ScanReport) {
    let dir = tempfile::tempdir().unwrap();
    stage_repo(dir.path());
    let llm = Recorder::default();
    let report = scan(&llm, dir.path(), model, calibration_model, controls).await;
    let calls = llm.fix_calls();
    assert!(
        !calls.is_empty(),
        "the fixture's floor finding must reach the fix stage; findings: {:?}",
        report.findings
    );
    (calls, report)
}

#[tokio::test]
async fn by_default_the_fix_stage_calls_haiku_at_the_named_effort() {
    let (calls, _) = fix_calls_for(
        Some("scan-model"),
        Some("calibration-model"),
        RunControls::default(),
    )
    .await;
    for call in &calls {
        assert_eq!(call.model, FIX_STAGE_HAIKU_MODEL, "{calls:?}");
        assert_eq!(call.effort.as_deref(), Some(FIX_STAGE_EFFORT), "{calls:?}");
    }
}

#[tokio::test]
async fn an_explicit_fix_model_wins_over_the_switch_in_both_positions() {
    for efficiency in [EfficiencyConfig::default(), switch_off()] {
        let controls = RunControls {
            efficiency,
            fix_model: Some("explicit-fix-model".to_string()),
            ..RunControls::default()
        };
        let (calls, _) =
            fix_calls_for(Some("scan-model"), Some("calibration-model"), controls).await;
        for call in &calls {
            assert_eq!(call.model, "explicit-fix-model", "{calls:?}");
            assert_eq!(
                call.effort, None,
                "effort is only sent when the fix model is Haiku: {calls:?}"
            );
        }
    }
}

/// One effort rule: an explicit Haiku pick gets `low` with the switch on or off.
#[tokio::test]
async fn an_explicit_haiku_pick_gets_the_same_effort_with_the_switch_on_or_off() {
    for efficiency in [EfficiencyConfig::default(), switch_off()] {
        let controls = RunControls {
            efficiency,
            fix_model: Some(FIX_STAGE_HAIKU_MODEL.to_string()),
            ..RunControls::default()
        };
        let (calls, _) =
            fix_calls_for(Some("scan-model"), Some("calibration-model"), controls).await;
        for call in &calls {
            assert_eq!(call.model, FIX_STAGE_HAIKU_MODEL, "{calls:?}");
            assert_eq!(call.effort.as_deref(), Some(FIX_STAGE_EFFORT), "{calls:?}");
        }
    }
}

/// A spend-limited run with no fix model set is admitted, and the fix stage's spend is priced at
/// Haiku 5.5 rates (not the scan model's): the fix calls report tokens but no dollars, so the
/// run's total is the other calls' reported cost plus the fix calls priced from the registry.
#[tokio::test]
async fn a_capped_default_run_is_admitted_and_fix_spend_uses_haiku_rates() {
    let dir = tempfile::tempdir().unwrap();
    stage_repo(dir.path());
    let llm = Recorder {
        fix_calls_unpriced: true,
        ..Recorder::default()
    };
    let controls = RunControls {
        max_cost_usd: Some(50.0),
        ..RunControls::default()
    };
    let report = scan(&llm, dir.path(), None, None, controls).await;
    assert!(
        report.ai_blocked_reason.is_none() && !report.blocked,
        "the capped default run was refused: {:?}",
        report.ai_blocked_reason
    );
    let fix_calls = llm.fix_calls();
    assert!(!fix_calls.is_empty(), "the fix stage ran");
    assert!(
        fix_calls.iter().all(|c| c.model == FIX_STAGE_HAIKU_MODEL),
        "{fix_calls:?}"
    );

    let usage = |model: &str| {
        camerata_server::llm::usage_cost_usd(
            model,
            &camerata_server::llm::CallUsage {
                input: Some(1_000),
                output: Some(100),
                cache_read: 0,
                cache_creation: 0,
                cache_creation_1h: None,
            },
            camerata_server::llm::PriceBasis::realtime(),
        )
        .unwrap()
    };
    let haiku_per_call = usage(FIX_STAGE_HAIKU_MODEL);
    assert!(
        haiku_per_call < usage(camerata_server::llm::DEFAULT_MODEL),
        "Haiku is the cheaper fix model"
    );
    let other_calls = *llm.other_calls.lock().unwrap();
    let expected = other_calls as f64 * 0.001 + fix_calls.len() as f64 * haiku_per_call;
    let actual = report.actual_usage.as_ref().unwrap().cost_usd;
    assert!(
        (actual - expected).abs() < 1e-6,
        "spend {actual} should be {expected}: {other_calls} other calls at $0.001 plus {} fix \
         calls at the Haiku rate {haiku_per_call}",
        fix_calls.len()
    );
}

#[tokio::test]
async fn a_blank_explicit_fix_model_counts_as_unset() {
    let controls = RunControls {
        fix_model: Some("   ".to_string()),
        ..RunControls::default()
    };
    let (calls, _) = fix_calls_for(Some("scan-model"), None, controls).await;
    assert!(
        calls.iter().all(|c| c.model == FIX_STAGE_HAIKU_MODEL),
        "{calls:?}"
    );
}

/// Switch off: exactly `calibration_model.or(model)`, and with neither set no model id at all
/// (the transport picks its default), as before rank 10. No effort in any case.
#[tokio::test]
async fn switch_off_reproduces_the_pre_rank_10_fix_model_exactly() {
    let off = || RunControls {
        efficiency: switch_off(),
        ..RunControls::default()
    };
    let cases: [(Option<&str>, Option<&str>, &str); 4] = [
        (
            Some("scan-model"),
            Some("calibration-model"),
            "calibration-model",
        ),
        (Some("scan-model"), None, "scan-model"),
        (None, Some("calibration-model"), "calibration-model"),
        (None, None, ""),
    ];
    for (model, calibration_model, expected) in cases {
        let (calls, _) = fix_calls_for(model, calibration_model, off()).await;
        for call in &calls {
            assert_eq!(
                call.model, expected,
                "model {model:?}, calibration {calibration_model:?}: {calls:?}"
            );
            assert_eq!(call.effort, None, "{calls:?}");
        }
    }
}

#[tokio::test]
async fn the_telemetry_records_the_fix_model_per_call_and_the_give_up_rate() {
    let (calls, report) = fix_calls_for(Some("scan-model"), None, RunControls::default()).await;
    let telemetry = report.ledger.stage_telemetry().expect("a model ran");

    let fix = telemetry
        .stages
        .iter()
        .find(|s| s.stage == camerata_server::stage_telemetry::Stage::FixGeneration)
        .expect("the fix stage made calls");
    assert_eq!(
        fix.usage.models.get(FIX_STAGE_HAIKU_MODEL).copied(),
        Some(calls.len() as u64),
        "every fix call is counted against the model that served it: {:?}",
        fix.usage.models
    );

    // The scripted model never returns a fix, so every finding sent to it is a give-up.
    let outcome = telemetry.fix_outcome.expect("the fix stage reported");
    assert!(outcome.sent_to_model >= 1, "{outcome:?}");
    assert_eq!(outcome.answered, outcome.sent_to_model, "{outcome:?}");
    assert_eq!(outcome.gave_up, outcome.answered, "{outcome:?}");
    assert_eq!((outcome.cap_stopped, outcome.transport_failed), (0, 0));
    assert_eq!(outcome.give_up_rate(), Some(1.0));

    let table = telemetry.render_table();
    assert!(
        table.contains(&format!(
            "fix model: {FIX_STAGE_HAIKU_MODEL} x{}",
            calls.len()
        )),
        "{table}"
    );
    assert!(table.contains("fix give-up rate: 100.0%"), "{table}");
    assert!(
        table.contains("0 not answered: 0 stopped by the spend limit, 0 failed in transport"),
        "{table}"
    );

    // The tally is on the operator ledger, which persists as JSON; it survives the round trip.
    let back: camerata_server::scan_ledger::ScanLedger =
        serde_json::from_str(&serde_json::to_string(&report.ledger).unwrap()).unwrap();
    assert_eq!(back.stage_telemetry().unwrap().fix_outcome, Some(outcome));
}
