//! The agent finder: a second finder behind a switch. One model agent reviews each piece of the
//! code in a review room, the rules are its checklist, and coverage is read from its tool trace.
//! Design: camerata-internal `docs/plans/2026-11-16_agent-finder-design.md`.
//!
//! This module root holds the shared types and the one trait the transports implement
//! ([`PieceAgent`]). The parts with no model call and no transport live in the submodules:
//! [`pieces`], [`room`], [`checklist`], [`reply`], [`coverage`] and [`brief_text`]. The runner
//! ([`run`]), the two transports ([`api_agent`], [`cli_agent`]) and the run comparison
//! ([`run_diff`]) are declared here as stubs so no later build edits this file.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub mod api_agent;
pub mod brief_text;
pub mod checklist;
pub mod cli_agent;
pub mod coverage;
pub mod pieces;
pub mod reply;
pub mod room;
pub mod run;
pub mod run_diff;

#[cfg(test)]
mod brief_text_tests;
#[cfg(test)]
mod checklist_tests;
#[cfg(test)]
mod coverage_tests;
#[cfg(test)]
mod pieces_tests;
#[cfg(test)]
mod reply_tests;
#[cfg(test)]
mod room_tests;
#[cfg(test)]
mod settings_tests;
#[cfg(test)]
mod types_tests;

pub use room::Room;

/// Which finder a run uses. `Engine` is the existing scan-pass finder (the default until the
/// graded comparison decides); `Agent` is this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FinderChoice {
    #[default]
    Engine,
    Agent,
}

/// The finder's default model when `--finder-model` and the project settings name none.
pub const DEFAULT_FINDER_MODEL: &str = "claude-fable-5-2";

/// One piece of a unit: the files one agent reviews. A piece never crosses a unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Piece {
    /// Unit name, piece number and a digest of the file list. Stable for the same input; the
    /// checkpoint and the failure report use it.
    pub key: String,
    /// The unit the piece belongs to.
    pub unit: String,
    /// Repo-relative paths of the piece's files, in path order.
    pub files: Vec<String>,
}

/// One recorded tool call. The trace is the only source of coverage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolEvent {
    /// A Read: the path asked for, the first line asked for (1-based), the line count asked
    /// for, and how many lines the tool returned (a read cut short returns fewer).
    Read {
        path: String,
        first_line: usize,
        line_count: usize,
        lines_returned: usize,
    },
    Glob {
        pattern: String,
    },
    Grep {
        pattern: String,
    },
    Ls {
        path: String,
    },
    Write {
        path: String,
    },
}

/// What one model turn used. `cost_usd` is filled by the transport when it can price the turn.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TurnUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cost_usd: Option<f64>,
}

/// Why an agent run ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStop {
    Finished,
    TurnLimit,
    SpendLimit,
    Refused,
    Failed(String),
}

/// The record of one agent run over one piece.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRun {
    /// Every tool call, in order.
    pub trace: Vec<ToolEvent>,
    /// The files the agent wrote under `_camerata/out/`: path (as written) to text.
    pub outputs: BTreeMap<String, String>,
    /// One entry for each model turn.
    pub usage: Vec<TurnUsage>,
    pub stop: AgentStop,
}

impl AgentRun {
    /// A run that ended with `stop` and recorded nothing.
    pub fn empty(stop: AgentStop) -> Self {
        Self {
            trace: Vec::new(),
            outputs: BTreeMap::new(),
            usage: Vec::new(),
            stop,
        }
    }
}

/// The limits one piece runs under (design section 2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PieceLimits {
    pub max_turns: usize,
    pub max_turn_tokens: u32,
    pub ceiling_usd: f64,
}

impl PieceLimits {
    /// Turns for a piece of `files` files: 2 for each file plus 30, at least 60, at most 400.
    pub fn turns_for(files: usize) -> usize {
        (2 * files + 30).clamp(60, 400)
    }

    /// The limits for a piece of `files` files whose estimated cost is `estimate_usd`: the
    /// turn limit above, 32,000 tokens a turn and four times the estimate (at least $3).
    pub fn for_piece(files: usize, estimate_usd: f64) -> Self {
        Self {
            max_turns: Self::turns_for(files),
            max_turn_tokens: 32_000,
            ceiling_usd: (4.0 * estimate_usd).max(3.0),
        }
    }
}

/// A source of agent runs: the in-process tool loop, the command line tool, or a script.
#[async_trait::async_trait]
pub trait PieceAgent: Send + Sync {
    /// Review the piece held by `room` under `brief`. The agent reads and writes only through
    /// the room's tools and returns its trace, the files it wrote and what each turn used.
    async fn run(&self, room: &Room, brief: &str, limits: &PieceLimits)
        -> anyhow::Result<AgentRun>;

    /// Send the files the first run did not read (`unread`, repo-relative paths) once more
    /// (design section 4). `prior` is the first run. The returned run holds only this step's
    /// trace, outputs and usage.
    async fn send_again(
        &self,
        room: &Room,
        prior: &AgentRun,
        unread: &[String],
        limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun>;
}

/// One step of a [`ScriptedAgent`] script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptStep {
    Read {
        path: String,
        first_line: usize,
        count: usize,
    },
    Glob(String),
    Grep(String),
    Ls(String),
    Write {
        path: String,
        text: String,
    },
}

/// A stand-in agent that plays back a list of tool calls against the room it is given and
/// returns the trace the room's tools produced. No model is called. Public so the runner's
/// tests (and the transports' parity test) can use it.
#[derive(Debug, Clone)]
pub struct ScriptedAgent {
    pub first: Vec<ScriptStep>,
    pub again: Vec<ScriptStep>,
    pub stop: AgentStop,
    pub usage_per_turn: TurnUsage,
}

impl ScriptedAgent {
    pub fn new(first: Vec<ScriptStep>) -> Self {
        Self {
            first,
            again: Vec::new(),
            stop: AgentStop::Finished,
            usage_per_turn: TurnUsage::default(),
        }
    }

    #[must_use]
    pub fn with_again(mut self, again: Vec<ScriptStep>) -> Self {
        self.again = again;
        self
    }

    #[must_use]
    pub fn with_stop(mut self, stop: AgentStop) -> Self {
        self.stop = stop;
        self
    }

    /// Play `steps` against `room`. A refused tool call is not recorded, like a real transport
    /// that returns the refusal to the model without a trace entry for a file it never served.
    fn play(&self, room: &Room, steps: &[ScriptStep]) -> AgentRun {
        let mut trace = Vec::new();
        for step in steps {
            match step {
                ScriptStep::Read {
                    path,
                    first_line,
                    count,
                } => {
                    let returned = room
                        .read(path, *first_line, *count)
                        .map_or(0, |r| r.lines_returned);
                    trace.push(ToolEvent::Read {
                        path: path.clone(),
                        first_line: *first_line,
                        line_count: *count,
                        lines_returned: returned,
                    });
                }
                ScriptStep::Glob(pattern) => {
                    let _ = room.glob(pattern);
                    trace.push(ToolEvent::Glob {
                        pattern: pattern.clone(),
                    });
                }
                ScriptStep::Grep(pattern) => {
                    let _ = room.grep(pattern, None);
                    trace.push(ToolEvent::Grep {
                        pattern: pattern.clone(),
                    });
                }
                ScriptStep::Ls(path) => {
                    let _ = room.ls(path);
                    trace.push(ToolEvent::Ls { path: path.clone() });
                }
                ScriptStep::Write { path, text } => {
                    if room.write(path, text).is_ok() {
                        trace.push(ToolEvent::Write { path: path.clone() });
                    }
                }
            }
        }
        AgentRun {
            trace,
            outputs: room.outputs(),
            usage: vec![self.usage_per_turn.clone()],
            stop: self.stop.clone(),
        }
    }
}

#[async_trait::async_trait]
impl PieceAgent for ScriptedAgent {
    async fn run(
        &self,
        room: &Room,
        _brief: &str,
        _limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        Ok(self.play(room, &self.first))
    }

    async fn send_again(
        &self,
        room: &Room,
        _prior: &AgentRun,
        _unread: &[String],
        _limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        Ok(self.play(room, &self.again))
    }
}
