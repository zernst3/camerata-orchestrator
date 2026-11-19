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

pub use room::{BuiltRoom, NotServed, Room};

/// Which finder a run uses. `Engine` is the existing scan-pass finder (the default until the
/// graded comparison decides); `Agent` is this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FinderChoice {
    #[default]
    Engine,
    Agent,
}

impl From<camerata_api_types::project::ProjectFinder> for FinderChoice {
    fn from(f: camerata_api_types::project::ProjectFinder) -> Self {
        match f {
            camerata_api_types::project::ProjectFinder::Engine => FinderChoice::Engine,
            camerata_api_types::project::ProjectFinder::Agent => FinderChoice::Agent,
        }
    }
}

/// The finder's default model when `--finder-model` and the project settings name none.
pub const DEFAULT_FINDER_MODEL: &str = camerata_api_types::project::FINDER_DEFAULT_MODEL;

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

/// Why an agent run ended. The text of `Refused`, `TurnLimit` and `SpendLimit` is a short reason
/// for the failure report (operator side).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStop {
    Finished,
    /// The turn limit was reached; the text says which limit and after how many turns.
    TurnLimit(String),
    /// The piece's spend ceiling (or the run's spend limit) stopped it; the text says which.
    SpendLimit(String),
    /// The model refused; the text is the refusal as the provider gave it.
    Refused(String),
    Failed(String),
}

impl AgentStop {
    /// Whether the run ended normally (the last turn completed).
    pub fn is_finished(&self) -> bool {
        matches!(self, Self::Finished)
    }
}

/// A handle a transport hands back so a later turn can continue the same conversation (an API
/// message history, a command line session id). Opaque to everything but the transport that made
/// it; serialisable so a resume can keep it. Empty when the transport keeps nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConversationHandle(String);

impl ConversationHandle {
    pub fn new(inner: impl Into<String>) -> Self {
        Self(inner.into())
    }

    /// The transport's own text (a session id, a serialised history, a key into its store).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The record of one agent run over one piece, or of one follow-up turn on it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRun {
    /// Every tool call, in order.
    pub trace: Vec<ToolEvent>,
    /// The files the agent wrote under `_camerata/out/`: path (as written) to text.
    pub outputs: BTreeMap<String, String>,
    /// One entry for each model turn.
    pub usage: Vec<TurnUsage>,
    pub stop: AgentStop,
    /// Piece files whose whole text the transport put in a message to the model in a turn that
    /// completed. Filled ONLY by the transport, from [`FollowUp::render`]: a read through a tool
    /// is not in this list (the trace covers it) and a caller can never add to it. Coverage
    /// ([`coverage::coverage_of`]) treats these files as sent again.
    pub shown_in_full: Vec<String>,
    /// Where the conversation can be continued by [`PieceAgent::follow_up`].
    pub conversation: ConversationHandle,
}

impl AgentRun {
    /// A run that ended with `stop` and recorded nothing.
    pub fn empty(stop: AgentStop) -> Self {
        Self {
            trace: Vec::new(),
            outputs: BTreeMap::new(),
            usage: Vec::new(),
            stop,
            shown_in_full: Vec::new(),
            conversation: ConversationHandle::default(),
        }
    }
}

/// One follow-up turn on a conversation: the repair turn (an output file that could not be
/// used), the missing-rules turn, or send again (the files the first run did not read).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FollowUp {
    /// What to tell the agent.
    pub message: String,
    /// Piece files to put in the message in full, after the text. Empty for a turn that sends
    /// no file.
    pub send_files: Vec<String>,
}

impl FollowUp {
    /// A follow-up that sends text only.
    pub fn text(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            send_files: Vec::new(),
        }
    }

    /// A follow-up that also sends the whole text of `files` (the send-again step).
    pub fn with_files(message: impl Into<String>, files: &[String]) -> Self {
        Self {
            message: message.into(),
            send_files: files.to_vec(),
        }
    }

    /// The text of the user message and the files it carries in full. A file counts only when it
    /// is a piece file of `room`; a path the room does not serve as a piece file is left out and
    /// so is never reported as shown. Every transport builds its message with this, so what
    /// "shown in full" means is one rule.
    pub fn render(&self, room: &Room) -> (String, Vec<String>) {
        self.render_with(room, None)
    }

    /// [`Self::render`] with each file framed as untrusted client content under `nonce`: the
    /// form the API transport sends, where the files are client text. The command line transport
    /// (internal runs only) sends the plain form.
    pub fn render_framed(
        &self,
        room: &Room,
        nonce: &crate::prompt_frame::FrameNonce,
    ) -> (String, Vec<String>) {
        self.render_with(room, Some(nonce))
    }

    fn render_with(
        &self,
        room: &Room,
        nonce: Option<&crate::prompt_frame::FrameNonce>,
    ) -> (String, Vec<String>) {
        let mut text = self.message.clone();
        let mut shown = Vec::new();
        for path in &self.send_files {
            let Some(body) = room.piece_file_text(path) else {
                continue;
            };
            if shown.contains(path) {
                continue;
            }
            match nonce {
                Some(nonce) => {
                    // A token in a body can only have been put there after it was drawn.
                    let body = body.replace(nonce.as_str(), "[frame token removed]");
                    text.push_str("\n\n");
                    text.push_str(&crate::prompt_frame::frame_untrusted(nonce, path, &body));
                }
                None => text.push_str(&format!("\n\n--- {path} ---\n{body}")),
            }
            shown.push(path.clone());
        }
        (text, shown)
    }
}

/// The message that sends files again (design section 4).
pub const SEND_AGAIN_MESSAGE: &str = "These files of your part were not read in full. Their whole text follows. Review them and add any problems to new problems files, with the next unused problem numbers.";

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

    /// The longest one call may run on the clock: 60 seconds for each turn, at least 15 minutes
    /// and at most 4 hours. A call that passes it is stopped and the piece fails.
    pub fn wall_clock(&self) -> std::time::Duration {
        std::time::Duration::from_secs((self.max_turns as u64 * 60).clamp(900, 4 * 3600))
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
///
/// **Paid turns are always counted.** Once a transport has sent a request that may be billed, it
/// returns `Ok(AgentRun { stop: AgentStop::Failed(..), usage, .. })` for any later failure, with
/// the usage of every turn that was paid for, and never `Err`. `Err` is only for a failure before
/// anything was sent (a room that cannot be written, a missing key). A runner that sees `Err`
/// therefore knows no money was spent. The same holds for [`PieceAgent::follow_up`].
#[async_trait::async_trait]
pub trait PieceAgent: Send + Sync {
    /// Review the piece held by `room` under `brief`. The agent reads and writes only through
    /// the room's tools and returns its trace, the files it wrote and what each turn used.
    async fn run(&self, room: &Room, brief: &str, limits: &PieceLimits)
        -> anyhow::Result<AgentRun>;

    /// One more turn on the conversation of `prior` (its [`AgentRun::conversation`]): the
    /// repair turn, the missing-rules turn and send again. The returned run holds only this
    /// step's trace, outputs and usage, and its own conversation handle. Files named in
    /// `turn.send_files` go to the model in full through [`FollowUp::render`]; the transport
    /// lists them in [`AgentRun::shown_in_full`] only when the turn completed.
    async fn follow_up(
        &self,
        room: &Room,
        prior: &AgentRun,
        turn: &FollowUp,
        limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun>;

    /// The piece is over: let the transport drop what it keeps for `conversation`. A follow-up
    /// on a released handle is an `Err` before anything is sent. The default keeps nothing, so
    /// there is nothing to release.
    fn release(&self, _conversation: &ConversationHandle) {}

    /// True when the transport already records each of its turns (and failures) on the run's
    /// meter, so the runner must not record them a second time. The default is false.
    fn meters_its_own_turns(&self) -> bool {
        false
    }

    /// A line for the run's operator notes saying where the transport keeps a trace of each call
    /// on disk, when it does. The default keeps none.
    fn trace_note(&self) -> Option<String> {
        None
    }
}

/// Send the files the first run did not read (`unread`, repo-relative paths) once more (design
/// section 4). A thin helper over [`PieceAgent::follow_up`]: the trait has no separate send
/// again method.
pub async fn send_again(
    agent: &dyn PieceAgent,
    room: &Room,
    prior: &AgentRun,
    unread: &[String],
    limits: &PieceLimits,
) -> anyhow::Result<AgentRun> {
    agent
        .follow_up(
            room,
            prior,
            &FollowUp::with_files(SEND_AGAIN_MESSAGE, unread),
            limits,
        )
        .await
}

/// Pieces in flight at once: 3 on the API, 1 on the command line backend (design section 2).
pub fn pieces_in_flight_for(backend: crate::llm::Backend) -> usize {
    match backend {
        crate::llm::Backend::Api => 3,
        crate::llm::Backend::Cli => 1,
    }
}

/// The finder's model for a run: the explicit pick, else [`DEFAULT_FINDER_MODEL`]. Never read
/// from the environment.
pub fn finder_model_for(controls: &crate::cost_cap::RunControls) -> String {
    controls
        .finder_model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .unwrap_or(DEFAULT_FINDER_MODEL)
        .to_string()
}

/// The one factory for a run's piece agent. `api_key` is needed (and checked) only for the API
/// backend: a missing key is an error, never a quiet switch to the command line. `work_dir` is an
/// engine-owned folder under the run's state folder where the command line transport writes its
/// rooms; it is never inside a client's repository. `engagement` is whose run it is: the command
/// line transport refuses a client engagement itself. Build 3 calls this; builds 2 and 4 fill in
/// [`api_agent::ApiPieceAgent`] and [`cli_agent::CliPieceAgent`]. The API agent is always built
/// with the run's `meter`: an API agent with no meter would spend money no limit counts.
pub fn piece_agent_for(
    controls: &crate::cost_cap::RunControls,
    backend: crate::llm::Backend,
    api_key: Option<&str>,
    work_dir: &std::path::Path,
    meter: std::sync::Arc<crate::ai_audit::UsageMeter>,
    engagement: cli_agent::EngagementKind,
) -> anyhow::Result<Box<dyn PieceAgent>> {
    let model = finder_model_for(controls);
    match backend {
        crate::llm::Backend::Api => {
            let key = api_key
                .map(str::trim)
                .filter(|k| !k.is_empty())
                .ok_or_else(|| anyhow::anyhow!("the API backend needs an Anthropic key"))?;
            Ok(Box::new(
                api_agent::ApiPieceAgent::new(model, key).with_meter(meter),
            ))
        }
        crate::llm::Backend::Cli => {
            let mut agent =
                cli_agent::CliPieceAgent::new(model, work_dir.to_path_buf(), engagement);
            // Internal runs only: the traces sit beside the rooms folder in the run folder,
            // never inside a room.
            if engagement == cli_agent::EngagementKind::Internal {
                if let Some(parent) = work_dir.parent().filter(|p| !p.as_os_str().is_empty()) {
                    agent = agent.with_trace_dir(parent.join("finder-traces"));
                }
            }
            Ok(Box::new(agent))
        }
    }
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

/// One scripted follow-up turn: the calls to play and how the turn ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptedTurn {
    pub steps: Vec<ScriptStep>,
    pub stop: AgentStop,
}

/// A stand-in agent that plays back a list of tool calls against the room it is given and
/// returns the trace the room's tools produced. No model is called. Public so the runner's
/// tests (and the transports' parity test) can use it. Like a real transport it returns
/// `Ok(AgentRun)` with the usage even when the run ends in `Failed`.
#[derive(Debug, Clone)]
pub struct ScriptedAgent {
    pub first: Vec<ScriptStep>,
    /// The script a follow-up plays when no queued turn is left.
    pub again: Vec<ScriptStep>,
    pub stop: AgentStop,
    /// How a follow-up that plays the `again` script ends.
    pub again_stop: AgentStop,
    pub usage_per_turn: TurnUsage,
    queued: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<ScriptedTurn>>>,
    received: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    /// The conversations this stand-in still keeps (a real transport holds file text for each).
    kept: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}

static SCRIPTED_HANDLES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

impl ScriptedAgent {
    pub fn new(first: Vec<ScriptStep>) -> Self {
        Self {
            first,
            again: Vec::new(),
            stop: AgentStop::Finished,
            again_stop: AgentStop::Finished,
            usage_per_turn: TurnUsage::default(),
            queued: std::sync::Arc::default(),
            received: std::sync::Arc::default(),
            kept: std::sync::Arc::default(),
        }
    }

    /// How many conversations are still kept: one for each run or follow-up whose handle was not
    /// released (by [`PieceAgent::release`], or by the follow-up that continued from it). Zero at
    /// the end of a run.
    pub fn kept(&self) -> usize {
        self.kept
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
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

    /// How the `again` script ends (a send again that stops at the turn limit, say).
    #[must_use]
    pub fn with_again_stop(mut self, stop: AgentStop) -> Self {
        self.again_stop = stop;
        self
    }

    #[must_use]
    pub fn with_usage(mut self, usage: TurnUsage) -> Self {
        self.usage_per_turn = usage;
        self
    }

    /// Queue one follow-up turn. Queued turns play in order, one for each follow-up; when the
    /// queue is empty a follow-up plays `again`.
    #[must_use]
    pub fn with_follow_up(self, steps: Vec<ScriptStep>, stop: AgentStop) -> Self {
        self.queued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(ScriptedTurn { steps, stop });
        self
    }

    /// The user messages the follow-ups received, rendered, in order.
    pub fn messages(&self) -> Vec<String> {
        self.received
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Play `steps` against `room`. A refused tool call is not recorded, like a real transport
    /// that returns the refusal to the model without a trace entry for a file it never served.
    fn play(
        &self,
        room: &Room,
        steps: &[ScriptStep],
        stop: AgentStop,
        shown_in_full: Vec<String>,
    ) -> AgentRun {
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
        // Only a turn that completed shows its files to the model.
        let shown_in_full = if stop.is_finished() {
            shown_in_full
        } else {
            Vec::new()
        };
        AgentRun {
            trace,
            outputs: room.outputs(),
            usage: vec![self.usage_per_turn.clone()],
            stop,
            shown_in_full,
            conversation: {
                let id = format!(
                    "scripted-{}",
                    SCRIPTED_HANDLES.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                );
                self.kept
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(id.clone());
                ConversationHandle::new(id)
            },
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
        Ok(self.play(room, &self.first, self.stop.clone(), Vec::new()))
    }

    async fn follow_up(
        &self,
        room: &Room,
        prior: &AgentRun,
        turn: &FollowUp,
        _limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        // A follow-up continues from the handle of `prior` and releases it.
        self.release(&prior.conversation);
        let (text, shown) = turn.render(room);
        self.received
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(text);
        let queued = self
            .queued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front();
        Ok(match queued {
            Some(t) => self.play(room, &t.steps, t.stop, shown),
            None => self.play(room, &self.again, self.again_stop.clone(), shown),
        })
    }

    fn release(&self, handle: &ConversationHandle) {
        self.kept
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(handle.as_str());
    }
}
