//! The command line transport for the agent finder (internal fixture runs only): the room is
//! written to an engine-owned folder and the trace is read from the session file. Build 4 fills
//! the body of the [`super::PieceAgent`] impl; the constructor below is the contract with the
//! factory ([`super::piece_agent_for`]).

use std::path::PathBuf;

use super::{AgentRun, FollowUp, PieceAgent, PieceLimits, Room};

/// The command line transport.
#[derive(Debug, Clone)]
pub struct CliPieceAgent {
    /// Read by the body build 4 writes.
    #[allow(dead_code)]
    pub(crate) model: String,
    /// An engine-owned folder under the run's state folder; never inside a client's repository.
    #[allow(dead_code)]
    pub(crate) work_dir: PathBuf,
}

impl CliPieceAgent {
    pub fn new(model: impl Into<String>, work_dir: PathBuf) -> Self {
        Self {
            model: model.into(),
            work_dir,
        }
    }
}

#[async_trait::async_trait]
impl PieceAgent for CliPieceAgent {
    async fn run(
        &self,
        _room: &Room,
        _brief: &str,
        _limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        // Nothing is sent, so nothing is paid: `Err` is allowed here.
        anyhow::bail!("the command line transport for the agent finder is built in build 4")
    }

    async fn follow_up(
        &self,
        _room: &Room,
        _prior: &AgentRun,
        _turn: &FollowUp,
        _limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        anyhow::bail!("the command line transport for the agent finder is built in build 4")
    }
}
