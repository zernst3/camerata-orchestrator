//! The in-process transport for the agent finder: a finder loop built on the wire code of
//! `api_agent_driver.rs`, with its five tools served from the in-memory room. Build 2 fills this
//! in; it implements [`super::PieceAgent`].

/// The API transport. Build 2 gives it its fields and its [`super::PieceAgent`] impl.
#[derive(Debug, Default)]
pub struct ApiPieceAgent;
