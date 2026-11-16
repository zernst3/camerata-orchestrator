//! The command line transport for the agent finder (internal fixture runs only).
//!
//! One call is one `claude -p` run through the existing [`crate::llm::Llm`] command line path
//! (the model-input chokepoint, the key scrub and the retry policy all apply). The room is
//! written to an engine-owned folder under the run's state folder, never inside a repository;
//! the model reads and writes only there. Output files are read back from the folder's
//! `_camerata/out/`. The trace is read from the session file the call leaves (the same
//! tool-call records `claude -p --output-format stream-json` prints), located by the session id
//! the engine chose for the call. A session file that cannot be found fails the piece: coverage
//! is never assumed.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::room::OUT_DIR;
use super::{
    AgentRun, AgentStop, ConversationHandle, FollowUp, PieceAgent, PieceLimits, Room, ToolEvent,
    TurnUsage,
};
use crate::llm::{Llm, LlmRequest, SessionSink};

/// The lines a Read returns when the call names no `limit` (the tool's own default).
const DEFAULT_READ_LINES: usize = 2_000;

/// The largest message a follow-up carries inline. A bigger one lists the files and leaves the
/// agent to read them from the folder (a process argument has a size limit).
const MAX_INLINE_BYTES: usize = 400_000;

/// The environment variable that names the `claude` program when the caller names none.
pub const PROGRAM_ENV: &str = "CAMERATA_CLAUDE_PROGRAM";

/// The command line transport.
#[derive(Debug, Clone)]
pub struct CliPieceAgent {
    pub(crate) model: String,
    /// An engine-owned folder under the run's state folder; never inside a client's repository.
    pub(crate) work_dir: PathBuf,
    /// The `claude` program to run. `None` = `claude` from `PATH`.
    program: Option<PathBuf>,
    /// Where Claude Code keeps session files. `None` = its default place.
    projects_dir: Option<PathBuf>,
}

impl CliPieceAgent {
    pub fn new(model: impl Into<String>, work_dir: PathBuf) -> Self {
        Self {
            model: model.into(),
            work_dir,
            program: std::env::var_os(PROGRAM_ENV)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            projects_dir: None,
        }
    }

    /// Run this `claude` program (the desktop app ships a newer one than a terminal install).
    #[must_use]
    pub fn with_claude_program(mut self, program: PathBuf) -> Self {
        self.program = Some(program);
        self
    }

    /// Look for session files under `dir` (Claude Code's `projects` folder).
    #[must_use]
    pub fn with_claude_projects_dir(mut self, dir: PathBuf) -> Self {
        self.projects_dir = Some(dir);
        self
    }

    fn projects_dir(&self) -> Option<PathBuf> {
        if let Some(dir) = &self.projects_dir {
            return Some(dir.clone());
        }
        if let Some(cfg) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(cfg).join("projects"));
        }
        dirs::home_dir().map(|h| h.join(".claude").join("projects"))
    }

    /// One call on `room`: write it out, run the program, read everything back. `prior` holds
    /// the output files already written by earlier steps (they are put in the folder so the
    /// agent can see them and only new or changed files come back).
    async fn call(
        &self,
        room: &Room,
        prompt: String,
        prior: &BTreeMap<String, String>,
    ) -> anyhow::Result<CallOutcome> {
        // Everything before the program starts is an `Err`: nothing was sent.
        std::fs::create_dir_all(&self.work_dir)?;
        let dir = self.work_dir.join(format!(
            "room-{}",
            camerata_llm::session_sink::new_session_id()
        ));
        std::fs::create_dir_all(&dir)?;
        room.materialize(&dir)?;
        for (path, text) in prior {
            let target = dir.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(target, text)?;
        }
        let resolved = std::fs::canonicalize(&dir)?;

        let sink = SessionSink::new();
        // The command line transport never uses an API key: the keyless command line constructor.
        let llm = Llm::from_env().with_session_sink(sink.clone());
        let request = LlmRequest::new(prompt)
            .with_model(self.model.clone())
            .with_repo_read(&resolved)
            .with_room_output(&resolved, self.program.clone());
        let reply = llm.complete(request).await;

        let sessions = sink.snapshot();
        let mut roots = vec![resolved.clone(), dir.clone()];
        roots.dedup();
        let mut trace = Vec::new();
        let mut session_usage = Vec::new();
        let mut missing = Vec::new();
        if let Some(projects) = self.projects_dir() {
            for session in &sessions {
                match find_session(&projects, &session.session_id)
                    .and_then(|p| std::fs::read_to_string(p).ok())
                {
                    Some(text) => {
                        let parsed = parse_session(&text, &roots);
                        trace.extend(parsed.trace);
                        session_usage.extend(parsed.usage);
                    }
                    None => missing.push(session.session_id.clone()),
                }
            }
        } else {
            missing.extend(sessions.iter().map(|s| s.session_id.clone()));
        }

        let mut outputs = collect_outputs(&resolved);
        outputs.retain(|path, text| prior.get(path) != Some(text));
        let handle = sessions
            .last()
            .map(|s| ConversationHandle::new(s.session_id.clone()))
            .unwrap_or_default();

        let (usage, mut stop) = match reply {
            Ok(resp) => (
                vec![TurnUsage {
                    input_tokens: resp.input_tokens.unwrap_or(0),
                    output_tokens: resp.output_tokens.unwrap_or(0),
                    cache_read_input_tokens: resp.cache_read_input_tokens,
                    cache_creation_input_tokens: resp.cache_creation_input_tokens,
                    cost_usd: resp.cost_usd,
                }],
                AgentStop::Finished,
            ),
            Err(e) => {
                let why = format!("{e:#}");
                let stop = if why.to_ascii_lowercase().contains("refus") {
                    AgentStop::Refused(why)
                } else {
                    AgentStop::Failed(why)
                };
                // The reply is lost, so the paid turns are counted from the session file.
                (session_usage, stop)
            }
        };
        if stop.is_finished() && !missing.is_empty() {
            stop = AgentStop::Failed(format!(
                "the session file of the call could not be found ({}), so what the agent read \
                 is unknown and the piece cannot be counted as reviewed",
                missing.join(", ")
            ));
        }
        Ok(CallOutcome {
            run: AgentRun {
                trace,
                outputs,
                usage,
                stop,
                shown_in_full: Vec::new(),
                conversation: handle,
            },
        })
    }
}

struct CallOutcome {
    run: AgentRun,
}

#[async_trait::async_trait]
impl PieceAgent for CliPieceAgent {
    async fn run(
        &self,
        room: &Room,
        brief: &str,
        _limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        Ok(self
            .call(room, brief.to_string(), &BTreeMap::new())
            .await?
            .run)
    }

    async fn follow_up(
        &self,
        room: &Room,
        prior: &AgentRun,
        turn: &FollowUp,
        _limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        // A new call on a room that holds only the files to send again (or the whole room for a
        // turn that sends none). The earlier output files are put back in the folder.
        let sub;
        let room = if turn.send_files.is_empty() {
            room
        } else {
            sub = room.only(&turn.send_files);
            &sub
        };
        let (mut text, mut shown) = turn.render(room);
        if text.len() > MAX_INLINE_BYTES {
            text = format!(
                "{}\n\nThe files are in this folder. Read each one in full:\n{}",
                turn.message,
                room.piece_files()
                    .iter()
                    .map(|p| format!("- {p}\n"))
                    .collect::<String>()
            );
            shown.clear();
        }
        let mut run = self.call(room, text, &prior.outputs).await?.run;
        if run.stop.is_finished() {
            run.shown_in_full = shown;
        }
        Ok(run)
    }
}

/// `<projects>/<any folder>/<id>.jsonl`, the way the client source purge finds a session file.
fn find_session(projects: &Path, id: &str) -> Option<PathBuf> {
    if id.is_empty() || id.contains('/') || id.contains("..") {
        return None;
    }
    std::fs::read_dir(projects)
        .ok()?
        .flatten()
        .map(|e| e.path().join(format!("{id}.jsonl")))
        .find(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file()))
}

/// Every regular file under `<room>/_camerata/out/`: path as the room names it to text.
fn collect_outputs(room: &Path) -> BTreeMap<String, String> {
    fn walk(dir: &Path, base: &Path, out: &mut BTreeMap<String, String>) {
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in read.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                walk(&path, base, out);
            } else if meta.is_file() {
                if let (Ok(rel), Ok(bytes)) = (path.strip_prefix(base), std::fs::read(&path)) {
                    let rel = rel
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/");
                    out.insert(rel, String::from_utf8_lossy(&bytes).into_owned());
                }
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(&room.join(OUT_DIR), room, &mut out);
    out
}

/// What a session file holds that the transport needs.
#[derive(Debug, Default)]
pub(crate) struct ParsedSession {
    pub trace: Vec<ToolEvent>,
    /// One entry for each model request, the last usage seen for it.
    pub usage: Vec<TurnUsage>,
}

/// The path as the room names it: relative to a room folder, no leading `./`. A path outside
/// every root is kept as the agent wrote it (it names nothing of the room).
fn room_path(path: &str, roots: &[PathBuf]) -> String {
    for root in roots {
        if let Ok(rest) = Path::new(path).strip_prefix(root) {
            let rest = rest.to_string_lossy().into_owned();
            return if rest.is_empty() {
                ".".to_string()
            } else {
                rest
            };
        }
    }
    path.trim_start_matches("./").to_string()
}

/// How many lines a tool result returned: the tool's own count when the record carries it, else
/// the numbered lines of the text.
fn lines_returned(record: &Value, result: &Value) -> usize {
    for key in ["toolUseResult", "tool_use_result"] {
        if let Some(n) = record[key]["file"]["numLines"].as_u64() {
            return n as usize;
        }
    }
    let text = match &result["content"] {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    text.lines()
        .filter(|l| {
            let digits = l
                .trim_start()
                .chars()
                .take_while(char::is_ascii_digit)
                .count();
            digits > 0
                && l.trim_start()[digits..]
                    .chars()
                    .next()
                    .is_some_and(|c| c == '\t' || c == '\u{2192}')
        })
        .count()
}

/// Read a session file (or a stream-json trace: the records have the same shape).
pub(crate) fn parse_session(text: &str, roots: &[PathBuf]) -> ParsedSession {
    struct Use {
        name: String,
        input: Value,
    }
    let mut uses: Vec<(String, Use)> = Vec::new();
    // tool_use id to (is_error, lines returned)
    let mut results: HashMap<String, (bool, usize)> = HashMap::new();
    let mut usage_by_request: Vec<(String, TurnUsage)> = Vec::new();
    for line in text.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let blocks = record["message"]["content"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        match record["type"].as_str() {
            Some("assistant") => {
                for b in &blocks {
                    if b["type"] == "tool_use" {
                        if let (Some(id), Some(name)) = (b["id"].as_str(), b["name"].as_str()) {
                            uses.push((
                                id.to_string(),
                                Use {
                                    name: name.to_string(),
                                    input: b["input"].clone(),
                                },
                            ));
                        }
                    }
                }
                let u = &record["message"]["usage"];
                if u.is_object() {
                    let key = record["request_id"]
                        .as_str()
                        .or_else(|| record["message"]["id"].as_str())
                        .unwrap_or("")
                        .to_string();
                    let turn = TurnUsage {
                        input_tokens: u["input_tokens"].as_u64().unwrap_or(0),
                        output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
                        cache_read_input_tokens: u["cache_read_input_tokens"].as_u64().unwrap_or(0),
                        cache_creation_input_tokens: u["cache_creation_input_tokens"]
                            .as_u64()
                            .unwrap_or(0),
                        cost_usd: None,
                    };
                    match usage_by_request
                        .iter_mut()
                        .find(|(k, _)| !key.is_empty() && *k == key)
                    {
                        Some(slot) => slot.1 = turn,
                        None => usage_by_request.push((key, turn)),
                    }
                }
            }
            Some("user") => {
                for b in &blocks {
                    if b["type"] == "tool_result" {
                        if let Some(id) = b["tool_use_id"].as_str() {
                            let failed = b["is_error"].as_bool() == Some(true);
                            let lines = if failed {
                                0
                            } else {
                                lines_returned(&record, b)
                            };
                            results.insert(id.to_string(), (failed, lines));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let mut trace = Vec::new();
    for (id, u) in &uses {
        let (failed, returned) = results.get(id).copied().unwrap_or((false, 0));
        let text_of = |key: &str| u.input[key].as_str().unwrap_or("").to_string();
        match u.name.as_str() {
            "Read" => trace.push(ToolEvent::Read {
                path: room_path(&text_of("file_path"), roots),
                first_line: u.input["offset"]
                    .as_u64()
                    .map_or(1, |n| (n as usize).max(1)),
                line_count: u.input["limit"]
                    .as_u64()
                    .map_or(DEFAULT_READ_LINES, |n| n as usize),
                lines_returned: if failed { 0 } else { returned },
            }),
            "Glob" => trace.push(ToolEvent::Glob {
                pattern: text_of("pattern"),
            }),
            "Grep" => trace.push(ToolEvent::Grep {
                pattern: text_of("pattern"),
            }),
            "LS" => trace.push(ToolEvent::Ls {
                path: room_path(&text_of("path"), roots),
            }),
            // A refused write left nothing behind and is not recorded.
            "Write" if !failed => trace.push(ToolEvent::Write {
                path: room_path(&text_of("file_path"), roots),
            }),
            _ => {}
        }
    }
    ParsedSession {
        trace,
        usage: usage_by_request.into_iter().map(|(_, u)| u).collect(),
    }
}

#[cfg(test)]
#[path = "cli_agent_tests.rs"]
pub(crate) mod tests;
