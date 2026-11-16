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
