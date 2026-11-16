//! The command line transport for the agent finder (internal fixture runs only).
//!
//! One call is one `claude -p` run through the existing [`crate::llm::Llm`] command line path
//! (the model-input chokepoint, the key scrub and the retry policy all apply). The room is
//! written to an engine-owned folder under the run's state folder, never inside a repository:
//! the repository's files under `code/`, the room's own files (`_camerata/`) beside it. The
//! session runs with the room's root as its working directory and loads the project's settings
//! only, so a client's own `.claude/` or `.mcp.json` (under `code/`) is just a file to review.
//! The model reads and writes only inside the room. Output files are read back from the folder's
//! `_camerata/out/`. The trace is read from the stream-json records the call prints on stdout
//! (the call keeps no session file). A stream that cannot be found fails the piece: coverage is
//! never assumed.
//!
//! The cli backend is internal only: a client engagement is refused here, before a room is
//! written, because the session data would hold the client's code.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use super::room::{CODE_DIR, OUT_DIR};
use super::{
    AgentRun, AgentStop, ConversationHandle, FollowUp, PieceAgent, PieceLimits, Room, ToolEvent,
    TurnUsage,
};
use crate::llm::{Llm, LlmRequest, SessionSink, ROOM_TOOLS};

/// The lines a Read returns when the call names no `limit` (the tool's own default).
const DEFAULT_READ_LINES: usize = 2_000;

/// The largest message a follow-up carries inline. A bigger one lists the files and leaves the
/// agent to read them from the folder (a process argument has a size limit).
const MAX_INLINE_BYTES: usize = 400_000;

/// The environment variable that names the `claude` program when the caller names none.
pub const PROGRAM_ENV: &str = "CAMERATA_CLAUDE_PROGRAM";

/// What the agent is told about the layout, after the brief or the follow-up message.
const LAYOUT_NOTE: &str = "The files of this part are in the folder `code/`. Every path in these instructions (in `_camerata/context/tree.txt`, `neighbours.md` and `context-files.txt`, and the `path` of a problem, a lead and `unread.json`) is relative to `code/`, so the file `src/a.rs` is `code/src/a.rs` on disk. The `_camerata/` folder is beside `code/`, not inside it.";

/// Whose work the call is. The command line backend is for internal runs only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngagementKind {
    Internal,
    Client,
}

/// The command line transport.
#[derive(Debug, Clone)]
pub struct CliPieceAgent {
    pub(crate) model: String,
    /// An engine-owned folder under the run's state folder; never inside a client's repository.
    pub(crate) work_dir: PathBuf,
    /// The `claude` program to run. `None` = `claude` from `PATH`.
    program: Option<PathBuf>,
    engagement: EngagementKind,
    /// The wall-clock bound of one call. `None` = the piece limits' own bound.
    wall_clock: Option<Duration>,
}

impl CliPieceAgent {
    pub fn new(model: impl Into<String>, work_dir: PathBuf, engagement: EngagementKind) -> Self {
        Self {
            model: model.into(),
            work_dir,
            program: std::env::var_os(PROGRAM_ENV)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            engagement,
            wall_clock: None,
        }
    }

    /// Run this `claude` program (the desktop app ships a newer one than a terminal install).
    #[must_use]
    pub fn with_claude_program(mut self, program: PathBuf) -> Self {
        self.program = Some(program);
        self
    }

    /// Bound one call to `bound` of wall-clock time instead of the piece limits' own bound.
    #[must_use]
    pub fn with_wall_clock(mut self, bound: Duration) -> Self {
        self.wall_clock = Some(bound);
        self
    }

    /// A client engagement may not use this backend: its session data would hold client code.
    /// Checked before anything is written or started.
    fn refuse_client(&self) -> anyhow::Result<()> {
        if self.engagement == EngagementKind::Client {
            anyhow::bail!(
                "the command line backend is for internal runs only: a client engagement is \
                 refused here, before any room is written, because its session data would hold \
                 the client's code"
            );
        }
        Ok(())
    }

    /// One call on `room`: write it out, run the program, read everything back. `prior` holds
    /// the output files already written by earlier steps (they are put in the folder so the
    /// agent can see them and only new or changed files come back).
    async fn call(
        &self,
        room: &Room,
        prompt: String,
        prior: &BTreeMap<String, String>,
        limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        // Everything before the program starts is an `Err`: nothing was sent.
        self.refuse_client()?;
        std::fs::create_dir_all(&self.work_dir)?;
        let dir = self.work_dir.join(format!(
            "room-{}",
            camerata_llm::session_sink::new_session_id()
        ));
        std::fs::create_dir_all(&dir)?;
        room.materialize_under_code(&dir)?;
        for (path, text) in prior {
            let target = dir.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(target, text)?;
        }
        let resolved = std::fs::canonicalize(&dir)?;
        let long_lines = room.long_lines();

        // The sink keeps the stdout stream of each attempt: the call keeps no session file.
        let sink = SessionSink::with_stream_capture();
        // The command line transport never uses an API key: the keyless command line constructor.
        let llm = Llm::from_env().with_session_sink(sink.clone());
        let request = LlmRequest::new(format!("{prompt}\n\n{LAYOUT_NOTE}"))
            .with_model(self.model.clone())
            .with_repo_read(&resolved)
            .with_room_output(&resolved, self.program.clone())
            .with_room_max_turns(limits.max_turns);
        let bound = self.wall_clock.unwrap_or_else(|| limits.wall_clock());
        // The child is killed when the future is dropped (`kill_on_drop`), so a call that passes
        // the bound is stopped, not left running.
        let reply = tokio::time::timeout(bound, llm.complete(request)).await;

        let sessions = sink.snapshot();
        let mut roots = vec![resolved.clone(), dir.clone()];
        roots.dedup();
        // One parse for each attempt; `None` = the attempt printed no record we can read.
        let parsed: Vec<Option<ParsedSession>> = sessions
            .iter()
            .map(|s| {
                sink.stream_lines(&s.session_id)
                    .filter(|lines| !lines.is_empty())
                    .map(|lines| parse_session(&lines.join("\n"), &roots, &long_lines))
            })
            .collect();
        let finished_ok = matches!(reply, Ok(Ok(_)));

        // The reads of the reply that was accepted: the last attempt's only. An earlier attempt
        // failed and its reads were not part of the reply that came back.
        let mut trace = Vec::new();
        let mut outside: Vec<String> = parsed
            .iter()
            .flatten()
            .flat_map(|p| p.outside.iter().cloned())
            .collect();
        outside.sort();
        outside.dedup();
        if finished_ok {
            if let Some(Some(last)) = parsed.last() {
                trace = last.trace.clone();
            }
        } else {
            trace = parsed
                .iter()
                .flatten()
                .flat_map(|p| p.trace.clone())
                .collect();
        }

        let mut outputs = collect_outputs(&resolved);
        outputs.retain(|path, text| prior.get(path) != Some(text));
        let handle = sessions
            .last()
            .map(|s| ConversationHandle::new(s.session_id.clone()))
            .unwrap_or_default();

        let turn_limit_hit = parsed.iter().flatten().any(|p| {
            p.result
                .as_ref()
                .is_some_and(|r| r.subtype == "error_max_turns")
        });
        let (usage, mut stop) = match reply {
            Ok(Ok(resp)) => (
                vec![TurnUsage {
                    input_tokens: resp.input_tokens.unwrap_or(0),
                    output_tokens: resp.output_tokens.unwrap_or(0),
                    cache_read_input_tokens: resp.cache_read_input_tokens,
                    cache_creation_input_tokens: resp.cache_creation_input_tokens,
                    cost_usd: resp.cost_usd,
                }],
                AgentStop::Finished,
            ),
            other => {
                let stop = match other {
                    Err(_) => AgentStop::Failed(format!(
                        "the call passed its wall-clock bound of {} seconds and was stopped",
                        bound.as_secs()
                    )),
                    Ok(Err(e)) if camerata_llm::llm::is_refusal(&e) => {
                        AgentStop::Refused(format!("{e:#}"))
                    }
                    Ok(Err(_)) if turn_limit_hit => AgentStop::TurnLimit(format!(
                        "the call reached its limit of {} turns",
                        limits.max_turns
                    )),
                    Ok(Err(e)) => AgentStop::Failed(format!("{e:#}")),
                    Ok(Ok(_)) => unreachable!("handled above"),
                };
                // The reply is lost, so the paid turns come from the stream: the final result
                // record of each attempt when it printed one, else its model requests.
                let usage = parsed
                    .iter()
                    .flatten()
                    .flat_map(|p| match &p.result {
                        Some(r) => vec![r.usage.clone()],
                        None => p.usage.clone(),
                    })
                    .collect();
                (usage, stop)
            }
        };
        if stop.is_finished() {
            if !outside.is_empty() {
                stop = AgentStop::Failed(format!(
                    "the agent used a path outside the review room ({}), so the piece cannot be \
                     counted as reviewed",
                    outside.join(", ")
                ));
            } else if parsed
                .last()
                .and_then(Option::as_ref)
                .is_none_or(|p| p.assistant_records == 0)
            {
                let id = sessions
                    .last()
                    .map(|s| s.session_id.clone())
                    .unwrap_or_default();
                stop = AgentStop::Failed(format!(
                    "the stream of the call could not be found ({id}), so what the agent read \
                     is unknown and the piece cannot be counted as reviewed"
                ));
            }
        }
        Ok(AgentRun {
            trace,
            outputs,
            usage,
            stop,
            shown_in_full: Vec::new(),
            conversation: handle,
        })
    }
}

#[async_trait::async_trait]
impl PieceAgent for CliPieceAgent {
    async fn run(
        &self,
        room: &Room,
        brief: &str,
        limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        self.call(room, brief.to_string(), &BTreeMap::new(), limits)
            .await
    }

    async fn follow_up(
        &self,
        room: &Room,
        prior: &AgentRun,
        turn: &FollowUp,
        limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        self.refuse_client()?;
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
                    .map(|p| format!("- {}\n", Room::disk_path(p)))
                    .collect::<String>()
            );
            shown.clear();
        }
        let mut run = self.call(room, text, &prior.outputs, limits).await?;
        if run.stop.is_finished() {
            run.shown_in_full = shown;
        }
        Ok(run)
    }
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

/// The final `result` record of a call: what the whole call used and how it ended.
#[derive(Debug, Clone)]
pub(crate) struct ResultRecord {
    pub usage: TurnUsage,
    pub subtype: String,
}

/// What a call's stream holds that the transport needs.
#[derive(Debug, Default)]
pub(crate) struct ParsedSession {
    pub trace: Vec<ToolEvent>,
    /// One entry for each model request, the last usage seen for it.
    pub usage: Vec<TurnUsage>,
    /// What the final result record says the call used, when the stream has one.
    pub result: Option<ResultRecord>,
    /// Every path a tool call named that is outside the room, as the agent wrote it. Such a
    /// call is not in the trace; the transport fails the piece when there is one.
    pub outside: Vec<String>,
    /// How many assistant records the stream held. A call that really ran prints at least one;
    /// none means the trace is missing, not that the agent read nothing.
    pub assistant_records: usize,
}

/// The path as the room names it, or `None` when it is outside the room. An absolute path is
/// taken relative to a room root; a relative one is relative to the room root (the working
/// directory). `..` is resolved by name, and a path that climbs above the room is outside. The
/// repository's files live under `code/` on disk and are named without it in the room, so that
/// part is stripped; the room's own `_camerata/` files keep their name.
fn room_path(path: &str, roots: &[PathBuf]) -> Option<String> {
    let given = Path::new(path);
    let rest: PathBuf = if given.is_absolute() {
        roots
            .iter()
            .find_map(|root| given.strip_prefix(root).ok())?
            .to_path_buf()
    } else {
        given.to_path_buf()
    };
    let mut parts: Vec<String> = Vec::new();
    for component in rest.components() {
        match component {
            Component::Normal(name) => {
                let name = name.to_string_lossy().into_owned();
                // A home-relative (`~`) or variable (`$HOME`) first part is expanded by the tool
                // to a place outside the room; it is never a folder of the room.
                if parts.is_empty() && (name.starts_with('~') || name.starts_with('$')) {
                    return None;
                }
                parts.push(name);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if parts.first().map(String::as_str) == Some(CODE_DIR) {
        parts.remove(0);
    }
    Some(if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    })
}

/// Whether any segment of a path or glob argument is `..`, or starts with `~` or `$`. Segments
/// are split on `/` and also on `{`, `}` and `,`, so brace alternatives are seen. The argument is
/// outside the room whatever else it holds.
fn escapes(value: &str) -> bool {
    // An absolute path inside a brace alternative (`{/etc,code}/*`) is outside.
    let bytes = value.as_bytes();
    if bytes
        .windows(2)
        .any(|w| (w[0] == b'{' || w[0] == b',') && w[1] == b'/')
    {
        return true;
    }
    value
        .split(['/', '{', '}', ','])
        .any(|seg| seg == ".." || seg.starts_with('~') || seg.starts_with('$'))
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

/// Read the records a call printed (stream-json, one per line; a session file has the same
/// shape). `long_lines` holds, for each room file, the lines the Read tool cuts for being too
/// long: a Read that returned one did not show it whole.
pub(crate) fn parse_session(
    text: &str,
    roots: &[PathBuf],
    long_lines: &BTreeMap<String, Vec<usize>>,
) -> ParsedSession {
    struct Use {
        name: String,
        input: Value,
    }
    let mut uses: Vec<(String, Use)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    // tool_use id to (is_error, lines returned)
    let mut results: HashMap<String, (bool, usize)> = HashMap::new();
    let mut usage_by_request: Vec<(String, TurnUsage)> = Vec::new();
    let mut result: Option<ResultRecord> = None;
    // How many tool calls came before the last context compaction: what a Read before it showed
    // is no longer in the agent's context.
    let mut compacted_at = 0usize;
    let mut assistant_records = 0usize;
    for line in text.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let blocks = record["message"]["content"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        match record["type"].as_str() {
            Some("system") if record["subtype"].as_str() == Some("compact_boundary") => {
                compacted_at = uses.len();
            }
            Some("result") => {
                let u = &record["usage"];
                let base = u["input_tokens"].as_u64().unwrap_or(0);
                let cache_read = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
                let cache_creation = u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                result = Some(ResultRecord {
                    usage: TurnUsage {
                        // The same figure the accepted reply reports: input with its cache part.
                        input_tokens: base + cache_read + cache_creation,
                        output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
                        cache_read_input_tokens: cache_read,
                        cache_creation_input_tokens: cache_creation,
                        cost_usd: record["total_cost_usd"].as_f64(),
                    },
                    subtype: record["subtype"].as_str().unwrap_or("").to_string(),
                });
            }
            Some("assistant") => {
                assistant_records += 1;
                for b in &blocks {
                    if b["type"] == "tool_use" {
                        if let (Some(id), Some(name)) = (b["id"].as_str(), b["name"].as_str()) {
                            if seen.insert(id.to_string()) {
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
                }
                let u = &record["message"]["usage"];
                if u.is_object() {
                    // The session file spells the request id `requestId`; a stream record has
                    // only the message id.
                    let key = record["request_id"]
                        .as_str()
                        .or_else(|| record["requestId"].as_str())
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
    let mut outside: Vec<String> = Vec::new();
    for (index, (id, u)) in uses.iter().enumerate() {
        let (failed, returned) = results.get(id).copied().unwrap_or((false, 0));
        let text_of = |key: &str| u.input[key].as_str().unwrap_or("").to_string();
        // A path argument the call named, checked against the room. `None` = outside.
        let mut place = |raw: String, strict: bool| -> Option<String> {
            let placed = if strict && escapes(&raw) {
                None
            } else {
                room_path(&raw, roots)
            };
            if placed.is_none() {
                outside.push(raw);
            }
            placed
        };
        // The room offers a closed set of tools. A call to any other tool that went through
        // means the program did not hold to it, and nothing about what it touched is known.
        if !failed && !ROOM_TOOLS.contains(&u.name.as_str()) && u.name != "TodoWrite" {
            outside.push(format!("the tool {}", u.name));
            continue;
        }
        match u.name.as_str() {
            "Read" => {
                let Some(path) = place(text_of("file_path"), false) else {
                    continue;
                };
                // Reads before the last compaction were dropped from the agent's context (the
                // files are sent again), so they cover nothing.
                if index < compacted_at {
                    continue;
                }
                let first_line = u.input["offset"]
                    .as_u64()
                    .map_or(1, |n| (n as usize).max(1));
                let line_count = u.input["limit"]
                    .as_u64()
                    .map_or(DEFAULT_READ_LINES, |n| n as usize);
                let returned = if failed { 0 } else { returned };
                trace.extend(split_read(
                    path, first_line, line_count, returned, long_lines,
                ));
            }
            "Glob" => {
                let path_arg = text_of("path");
                if !path_arg.is_empty() {
                    place(path_arg.clone(), true);
                }
                let pattern = text_of("pattern");
                // The literal part of the pattern before its first wildcard is a path: checked
                // alone and under the `path` argument, so `..`, `~` and absolute prefixes
                // count as outside whichever way the tool resolves them.
                let prefix: String = pattern
                    .chars()
                    .take_while(|c| !matches!(c, '*' | '?' | '[' | '{'))
                    .collect();
                place(pattern.clone(), true);
                if !prefix.is_empty() {
                    place(prefix.clone(), true);
                    if !path_arg.is_empty() {
                        place(format!("{}/{prefix}", path_arg.trim_end_matches('/')), true);
                    }
                }
                if !failed {
                    trace.push(ToolEvent::Glob { pattern });
                }
            }
            "Grep" => {
                let path_arg = text_of("path");
                let glob_arg = text_of("glob");
                if !glob_arg.is_empty() {
                    place(glob_arg, true);
                }
                if !path_arg.is_empty() {
                    place(path_arg, true);
                }
                if !failed {
                    trace.push(ToolEvent::Grep {
                        pattern: text_of("pattern"),
                    });
                }
            }
            "LS" => {
                if let Some(path) = place(text_of("path"), false) {
                    trace.push(ToolEvent::Ls { path });
                }
            }
            // A refused write left nothing behind and is not recorded, but a write aimed
            // outside the room is still a path outside the room.
            "Write" => {
                if let Some(path) = place(text_of("file_path"), false) {
                    if !failed {
                        trace.push(ToolEvent::Write { path });
                    }
                }
            }
            "Edit" | "MultiEdit" => {
                place(text_of("file_path"), false);
            }
            "NotebookEdit" => {
                place(text_of("notebook_path"), false);
            }
            _ => {}
        }
    }
    ParsedSession {
        trace,
        usage: usage_by_request.into_iter().map(|(_, u)| u).collect(),
        result,
        outside,
        assistant_records,
    }
}

/// One Read as trace events. The Read tool cuts a line over the character limit, so a line of
/// `long_lines` that the read returned was not shown whole: the read is split around it, and the
/// line is covered by no event.
fn split_read(
    path: String,
    first_line: usize,
    line_count: usize,
    returned: usize,
    long_lines: &BTreeMap<String, Vec<usize>>,
) -> Vec<ToolEvent> {
    let whole = ToolEvent::Read {
        path: path.clone(),
        first_line,
        line_count,
        lines_returned: returned,
    };
    let Some(long) = long_lines.get(&path) else {
        return vec![whole];
    };
    if returned == 0 {
        return vec![whole];
    }
    let last = first_line + returned - 1;
    let mut out = Vec::new();
    let mut from = first_line;
    for &cut in long.iter().filter(|&&l| (first_line..=last).contains(&l)) {
        if cut > from {
            out.push(ToolEvent::Read {
                path: path.clone(),
                first_line: from,
                line_count: cut - from,
                lines_returned: cut - from,
            });
        }
        from = cut + 1;
    }
    if from <= last {
        out.push(ToolEvent::Read {
            path: path.clone(),
            first_line: from,
            line_count: last - from + 1,
            lines_returned: last - from + 1,
        });
    }
    if out.is_empty() {
        // Every line returned was cut: the file was touched and nothing of it was shown whole.
        out.push(ToolEvent::Read {
            path,
            first_line,
            line_count,
            lines_returned: 0,
        });
    }
    out
}

#[cfg(test)]
#[path = "cli_agent_tests.rs"]
pub(crate) mod tests;
