//! #582: a network drop during the clone check. The judge sends through the run's model seam, so a
//! short drop costs minutes and the verdicts are those of an undisturbed check; a long drop stops
//! the run.

use super::*;
use crate::failure::{FailureClass, LlmCallFailure};
use crate::network_wait::InstantWaits;
use crate::operator_stop::RunControl;

/// Refuses the first `down` sends as a dropped connection, then answers with the affirmative word.
fn dropping(down: usize) -> StubJudge {
    let word = CloneRule::SingleSourceLogic.affirmative_word();
    StubJudge::new(move |req, n| {
        if n < down {
            Err(anyhow::Error::new(LlmCallFailure::new(
                FailureClass::Transport,
                "Anthropic API request failed: connection reset by peer",
            )))
        } else {
            Ok(verdicts(&asked_ids(req), word, "the order discount"))
        }
    })
}

/// What a judge outcome found, comparable across runs.
fn found(outcome: &JudgeOutcome) -> Vec<(String, String, usize)> {
    let mut out: Vec<(String, String, usize)> = outcome
        .findings
        .iter()
        .map(|f| (f.rule_id.clone(), f.path.clone(), f.line))
        .collect();
    out.sort();
    out
}

async fn judge_through_the_seam(stub: &StubJudge, meter: &UsageMeter) -> JudgeOutcome {
    let files = discount_repo("0.1", "0.15");
    let set = set_for(&files, &[CloneRule::SingleSourceLogic]);
    let seam = crate::network_wait::wrap(stub, std::sync::Arc::new(RunControl::new()));
    let mut cache = VerdictCache::default();
    judge_clone_candidates(
        Some(seam.as_ref()),
        request(&files, set),
        &mut cache,
        Some(meter),
        None,
    )
    .await
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_short_drop_during_the_clone_check_waits_and_gives_the_undisturbed_verdicts() {
    let waits = InstantWaits::install();
    let undisturbed = judge_through_the_seam(&dropping(0), &UsageMeter::default()).await;
    assert!(!undisturbed.findings.is_empty(), "the fixture confirms a candidate");
    assert!(waits.waits().is_empty());

    let meter = UsageMeter::default();
    let stub = dropping(2);
    let outcome = judge_through_the_seam(&stub, &meter).await;
    assert_eq!(
        waits.waits(),
        crate::network_wait::NETWORK_WAIT_STEPS[..2].to_vec(),
        "the check waited for the network"
    );
    assert_eq!(found(&outcome), found(&undisturbed));
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    assert!(!meter.outage_stopped());
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_long_drop_during_the_clone_check_stops_the_run() {
    let waits = InstantWaits::install();
    let meter = UsageMeter::default();
    let outcome = judge_through_the_seam(&dropping(usize::MAX), &meter).await;
    assert!(
        meter.outage_stopped(),
        "the clone check's network wait ran out with nothing through: the outage stop"
    );
    assert!(outcome.findings.is_empty());
    assert!(
        waits.waits().iter().sum::<std::time::Duration>()
            >= crate::network_wait::NETWORK_WAIT_TOTAL
    );
}
