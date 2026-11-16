//! The in-process transport for the agent finder: a finder loop built on the wire code of
//! `api_agent_driver.rs`, with its five tools served from the in-memory room. Build 2 fills the
//! body of the [`super::PieceAgent`] impl; the constructor below is the contract with the factory
//! ([`super::piece_agent_for`]).

use super::{AgentRun, FollowUp, PieceAgent, PieceLimits, Room};

/// The API transport.
pub struct ApiPieceAgent {
    pub(crate) model: String,
    api_key: String,
}

impl std::fmt::Debug for ApiPieceAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiPieceAgent")
            .field("model", &self.model)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

impl ApiPieceAgent {
    /// `model` is the resolved finder model; `api_key` the Anthropic key (never logged).
    pub fn new(model: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            api_key: api_key.into(),
        }
    }

    /// The key, for the wire code build 2 writes.
    #[allow(dead_code)]
    pub(crate) fn api_key(&self) -> &str {
        &self.api_key
    }
}

#[async_trait::async_trait]
impl PieceAgent for ApiPieceAgent {
    async fn run(
        &self,
        _room: &Room,
        _brief: &str,
        _limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        // Nothing is sent, so nothing is paid: `Err` is allowed here.
        anyhow::bail!("the API transport for the agent finder is built in build 2")
    }

    async fn follow_up(
        &self,
        _room: &Room,
        _prior: &AgentRun,
        _turn: &FollowUp,
        _limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        anyhow::bail!("the API transport for the agent finder is built in build 2")
    }
}
