//! A transport that meters its own turns on the run's meter is not metered a second time by the
//! runner: each turn's cost and each failure counts once.

use super::*;
use crate::ai_audit::UsageMeter;

/// A stand-in that records every turn and failure on the shared meter itself, like the API agent.
pub(super) struct SelfMetering {
    pub(super) inner: ScriptedAgent,
    pub(super) meter: Arc<UsageMeter>,
}

impl SelfMetering {
    fn meter_run(&self, mut run: AgentRun) -> AgentRun {
        // A request that failed returned no reply, so it holds no turn usage.
        if matches!(run.stop, AgentStop::Failed(_)) {
            run.usage.clear();
        }
        for u in &run.usage {
            self.meter.record_stage(
                crate::stage_telemetry::Stage::Finder,
                &super::super::piece::response_of(&options(), u),
            );
        }
        if let AgentStop::Failed(why) = &run.stop {
            self.meter.record_failed(
                crate::stage_telemetry::Stage::Finder,
                &anyhow::anyhow!("{why}"),
            );
        }
        run
    }
}

#[async_trait::async_trait]
impl PieceAgent for SelfMetering {
    async fn run(
        &self,
        room: &Room,
        brief: &str,
        limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        Ok(self.meter_run(self.inner.run(room, brief, limits).await?))
    }

    async fn follow_up(
        &self,
        room: &Room,
        prior: &AgentRun,
        turn: &FollowUp,
        limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        Ok(self.meter_run(self.inner.follow_up(room, prior, turn, limits).await?))
    }

    fn release(&self, handle: &super::super::super::ConversationHandle) {
        self.inner.release(handle);
    }

    fn meters_its_own_turns(&self) -> bool {
        true
    }
}

async fn go(agent: &SelfMetering) -> FinderResult {
    find_repo(
        agent,
        REPO,
        &repo_files(),
        &rules(),
        &corpus(),
        &RunControls::default(),
        &options(),
        Some(&agent.meter),
    )
    .await
    .expect("the finder returns a result")
}

#[tokio::test]
async fn a_self_metering_agents_turns_are_on_the_shared_meter_once() {
    // The main review leaves SEC-TWO-1 unanswered, so one rules follow-up is sent: two turns.
    let first = full_script(
        &[("src/a.rs", 6), ("src/b.rs", 4)],
        vec![],
        vec![],
        vec![answer_clean("ARCH-ONE-1", "src/a.rs", 1)],
    );
    let inner = ScriptedAgent::new(first)
        .with_usage(usage(1.0))
        .with_follow_up(
            vec![out(
                "checklist.json",
                &json!({"answers": [answer_clean("SEC-TWO-1", "src/b.rs", 1)]}),
            )],
            AgentStop::Finished,
        );
    let agent = SelfMetering {
        inner,
        meter: Arc::new(UsageMeter::default()),
    };
    let _ = go(&agent).await;
    let snap = agent.meter.snapshot();
    assert_eq!(snap.calls, 2, "two turns, counted once each");
    assert!((snap.cost_usd - 2.0).abs() < 1e-9, "{}", snap.cost_usd);
}

#[tokio::test]
async fn a_self_metering_agents_failure_advances_the_outage_streak_once() {
    let inner = ScriptedAgent::new(good_script(vec![], vec![]))
        .with_stop(AgentStop::Failed("connection reset by peer".into()));
    let agent = SelfMetering {
        inner,
        meter: Arc::new(UsageMeter::default()),
    };
    let _ = go(&agent).await;
    assert_eq!(agent.meter.health().streak(), 1);
}

#[test]
fn a_piece_stopped_mid_conversation_by_the_run_stop_keeps_its_outage_class() {
    use super::super::piece::stop_class;
    use camerata_llm::failure::FailureClass;
    let meter = UsageMeter::default();
    meter.health().note_failure(
        camerata_llm::failure::FailureClass::AuthOrBilling,
        "Your credit balance is too low to access the Anthropic API.",
    );
    let text = super::super::super::api_agent::run_stopped_text(&meter);
    assert_eq!(
        stop_class(&AgentStop::Failed(text.clone())),
        FailureClass::OutageStop,
        "the stop text must keep its class: {text}"
    );
    assert!(
        text.contains("refused the request for billing (credit balance)"),
        "{text}"
    );
    assert!(!text.contains("could not be reached"), "{text}");

    // An unreachable provider keeps the plain sentence, and the same class.
    let meter = UsageMeter::default();
    let text = super::super::super::api_agent::run_stopped_text(&meter);
    assert_eq!(
        stop_class(&AgentStop::Failed(text.clone())),
        FailureClass::OutageStop,
        "{text}"
    );
    // A stop that is not the run's own keeps the class its text gives.
    assert_eq!(
        stop_class(&AgentStop::Failed("connection reset by peer".into())),
        FailureClass::Transport
    );
}

/// A stored finder record counts its input side with the cache reads and writes folded in (every
/// transport stores it so). The stage row shows that figure once, not the figure plus the cache
/// again. The record is a made-up sample: 100 plain, 800 read and 100 written is 1,000 in all.
#[test]
fn a_stored_finder_record_reads_back_as_one_row_without_counting_its_cache_twice() {
    let line = crate::checkpoint::AbsorbedLine {
        key: "src/a.rs".to_string(),
        model: "acme-model-1".to_string(),
        backend: "cli".to_string(),
        cost_usd: Some(0.25),
        input_tokens: Some(1_000),
        output_tokens: Some(50),
        cache_read_input_tokens: 800,
        cache_creation_input_tokens: 100,
        text: String::new(),
        members: Vec::new(),
        batch_id: None,
    };
    let usage = super::super::line_usage(&line);
    let meter = UsageMeter::default();
    meter.record_stage(
        crate::stage_telemetry::Stage::Finder,
        &super::super::piece::response_of(&options(), &usage),
    );
    let telemetry = meter.stage_telemetry(0);
    let row = telemetry
        .stages
        .iter()
        .find(|s| s.stage == crate::stage_telemetry::Stage::Finder)
        .expect("a finder row");
    assert_eq!(row.usage.input_tokens, 1_000);
    assert_eq!(row.usage.cache_read_input_tokens, 800);
    assert_eq!(row.usage.cache_creation_input_tokens, 100);
    assert!((row.usage.cost_usd - 0.25).abs() < 1e-9);
}

/// The meter's input figure for a turn is the turn's own input, which already holds its cache.
#[test]
fn a_turns_meter_input_is_its_own_input_figure() {
    let usage = super::super::super::TurnUsage {
        input_tokens: 1_000,
        output_tokens: 0,
        cache_read_input_tokens: 800,
        cache_creation_input_tokens: 100,
        cost_usd: None,
    };
    let resp = super::super::piece::response_of(&options(), &usage);
    assert_eq!(resp.input_tokens, Some(1_000));
}
