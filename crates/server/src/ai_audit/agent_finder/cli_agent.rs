//! The command line transport for the agent finder (internal fixture runs only): the room is
//! written to an engine-owned folder and the trace is read from the session file. Build 4 fills
//! this in; it implements [`super::PieceAgent`].

/// The command line transport. Build 4 gives it its fields and its [`super::PieceAgent`] impl.
#[derive(Debug, Default)]
pub struct CliPieceAgent;
