//! Rank 10: the fix stage's model and effort. The precedence is pure ([`resolve_fix_model`],
//! [`fix_effort`]); the request the completer receives, the pre-filter on the Haiku path and the
//! give-up tally go through [`generate_fix_specifics`] with a recording stub. No network.
//! The whole-pipeline view (the project and CLI setting reaching the call) is
//! `tests/fix_stage_model_e2e.rs`.

use std::sync::Mutex;

use super::*;
use crate::cost_cap::RunControls;
use crate::efficiency::EfficiencyConfig;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};

fn switch_off() -> EfficiencyConfig {
    let mut efficiency = EfficiencyConfig::default();
    efficiency.set("fix_on_haiku", false).unwrap();
    efficiency
}

#[test]
fn an_explicit_fix_model_wins_then_haiku_then_the_pre_rank_10_choice() {
    let (scan, cal) = (Some("scan-model"), Some("calibration-model"));
    // 1. explicit, whatever the switch says
    for on in [true, false] {
        assert_eq!(
            resolve_fix_model(Some("explicit"), on, scan, cal),
            Some("explicit")
        );
    }
    // 2. switch on: Haiku, whatever the scan and calibration models are
    for (m, c) in [(scan, cal), (scan, None), (None, cal), (None, None)] {
        assert_eq!(
            resolve_fix_model(None, true, m, c),
            Some(FIX_STAGE_HAIKU_MODEL)
        );
        // blank explicit counts as unset
        assert_eq!(
            resolve_fix_model(Some("  "), true, m, c),
            Some(FIX_STAGE_HAIKU_MODEL)
        );
    }
}

/// Switch off: exactly `calibration_model.or(model)`, for every combination, explicit unset or
/// blank. This is the expression the call site passed before rank 10.
#[test]
fn switch_off_is_exactly_calibration_or_scan_model() {
    for m in [Some("scan-model"), None] {
        for c in [Some("calibration-model"), None] {
            for explicit in [None, Some(""), Some(" ")] {
                assert_eq!(
                    resolve_fix_model(explicit, false, m, c),
                    c.or(m),
                    "model {m:?}, calibration {c:?}, explicit {explicit:?}"
                );
            }
        }
    }
}

#[test]
fn effort_is_sent_only_on_the_haiku_path_with_the_switch_on() {
    let on = EfficiencyConfig::default();
    assert_eq!(
        fix_effort(Some(FIX_STAGE_HAIKU_MODEL), on),
        Some(FIX_STAGE_EFFORT)
    );
    assert_eq!(FIX_STAGE_EFFORT, "low");
    assert_eq!(fix_effort(Some("claude-sonnet-5"), on), None);
    assert_eq!(fix_effort(None, on), None);
    // Off reproduces the pre-rank-10 request: no effort even if the model happens to be Haiku.
    assert_eq!(fix_effort(Some(FIX_STAGE_HAIKU_MODEL), switch_off()), None);
}

/// Records the model and effort of every request; answers with no fix (so every finding sent
/// to it is a give-up).
#[derive(Default)]
struct Recorder {
    requests: Mutex<Vec<(String, Option<String>)>>,
}

#[async_trait::async_trait]
impl LlmPort for Recorder {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.requests
            .lock()
            .unwrap()
            .push((req.model.clone(), req.effort.clone()));
        Ok(LlmResponse {
            text: r#"{"fixes":[]}"#.to_string(),
            model: req.model.clone(),
            backend: "stub".to_string(),
            cost_usd: Some(0.001),
            input_tokens: Some(100),
            output_tokens: Some(10),
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

fn meter(efficiency: EfficiencyConfig) -> UsageMeter {
    UsageMeter::with_controls(RunControls {
        efficiency,
        ..RunControls::default()
    })
}

/// A finding with code around it, so the no-code pre-filter does not take it.
fn finding_with_code() -> (Finding, Vec<(String, String)>) {
    let f = Finding {
        repo: "o/r".to_string(),
        path: "src/db.ts".to_string(),
        line: 2,
        rule_id: "ARCH-FIX-MODEL-TEST-1".to_string(),
        severity: "medium".to_string(),
        snippet: "query(raw)".to_string(),
        detail: "`runQuery` passes `raw` straight to `query`".to_string(),
        ..Finding::default()
    };
    let files = vec![(
        "src/db.ts".to_string(),
        "export function runQuery(raw: string) {\n  return query(raw);\n}\n".to_string(),
    )];
    (f, files)
}

#[tokio::test]
async fn the_chosen_model_and_effort_reach_the_completer_on_every_round() {
    for (efficiency, model, expected_effort) in [
        (
            EfficiencyConfig::default(),
            Some(FIX_STAGE_HAIKU_MODEL),
            Some(FIX_STAGE_EFFORT),
        ),
        (switch_off(), Some("claude-sonnet-5"), None),
    ] {
        let (f, files) = finding_with_code();
        let llm = Recorder::default();
        let meter = meter(efficiency);
        let _ =
            generate_fix_specifics(&llm, "o/r", vec![f], &files, model, Some(&meter), None).await;
        let requests = llm.requests.lock().unwrap().clone();
        // Every batched retry plus the solo retry: the parameter reaches each one.
        assert_eq!(requests.len(), MAX_FIX_REGENERATIONS + 2, "{requests:?}");
        for (m, effort) in &requests {
            assert_eq!(Some(m.as_str()), model, "{requests:?}");
            assert_eq!(effort.as_deref(), expected_effort, "{requests:?}");
        }
    }
}

#[tokio::test]
async fn the_give_up_rate_is_recorded_per_run() {
    let (f, files) = finding_with_code();
    let mut second = f.clone();
    second.line = 1;
    let llm = Recorder::default();
    let meter = meter(EfficiencyConfig::default());
    let _ = generate_fix_specifics(
        &llm,
        "o/r",
        vec![f, second],
        &files,
        Some(FIX_STAGE_HAIKU_MODEL),
        Some(&meter),
        None,
    )
    .await;
    let telemetry = meter.stage_telemetry(0);
    let outcome = telemetry.fix_outcome.expect("the fix stage reported");
    assert_eq!(outcome.sent_to_model, 2);
    assert_eq!(outcome.gave_up, 2, "the stub never returns a fix");
    assert_eq!(outcome.give_up_rate(), Some(1.0));
    let fix = telemetry
        .stages
        .iter()
        .find(|s| s.stage == Stage::FixGeneration)
        .unwrap();
    assert_eq!(
        fix.usage.models.get(FIX_STAGE_HAIKU_MODEL).copied(),
        Some(fix.usage.calls),
        "every fix call is counted against its model"
    );
}

/// The no-code pre-filter is unchanged on the Haiku path: a finding with no surrounding code and
/// an authored remediation never reaches the model, and is counted as such, not as a give-up.
#[tokio::test]
async fn the_no_code_prefilter_still_applies_on_the_haiku_path() {
    let (corpus, errors) =
        camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    assert!(errors.is_empty(), "{errors:?}");
    let mut f = Finding {
        repo: "o/r".to_string(),
        path: "supabase/migrations/1.sql".to_string(),
        line: 0,
        rule_id: "SUPABASE-RLS-ENABLED-1".to_string(),
        severity: "high".to_string(),
        snippet: "CREATE TABLE profiles (...)".to_string(),
        detail: "no RLS evidence for `profiles`".to_string(),
        ..Finding::default()
    };
    f.captures
        .insert("table".to_string(), "profiles".to_string());
    let llm = Recorder::default();
    let meter = meter(EfficiencyConfig::default());
    let (out, failed) = generate_fix_specifics(
        &llm,
        "o/r",
        vec![f],
        &[],
        Some(FIX_STAGE_HAIKU_MODEL),
        Some(&meter),
        Some(&corpus),
    )
    .await;
    assert!(llm.requests.lock().unwrap().is_empty(), "no model call");
    assert!(out[0].fix_specific.is_some(), "the authored remediation");
    assert!(failed.is_empty());
    let outcome = meter.stage_telemetry(0).fix_outcome.unwrap();
    assert_eq!(
        (
            outcome.sent_to_model,
            outcome.gave_up,
            outcome.authored_without_model
        ),
        (0, 0, 1)
    );
    assert_eq!(
        outcome.give_up_rate(),
        None,
        "nothing was sent to the model"
    );
}
