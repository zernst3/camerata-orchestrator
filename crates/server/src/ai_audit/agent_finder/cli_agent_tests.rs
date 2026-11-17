//! The command line transport, against a stand-in program that replays a recorded trace. No
//! model is called: the stand-in writes canned files into the room, prints a canned stream of
//! tool-call records on stdout (the call keeps no session file) and then a canned result line.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use camerata_secrets::fixtures::planted;
use camerata_secrets::SecretShape;
use serde_json::{json, Value};

use super::super::checklist::{CheckOutcome, Checklist, ChecklistRule};
use super::super::coverage::{coverage_of, PieceCoverage};
use super::super::reply::parse_outputs;
use super::super::Piece;
use super::super::{
    send_again, AgentRun, AgentStop, PieceAgent, PieceLimits, Room, ScriptStep, ScriptedAgent,
    ToolEvent,
};
use super::{parse_session, CliPieceAgent, EngagementKind};

const MODEL: &str = "claude-fable-5-2";

fn f(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

fn numbered(n: usize, tag: &str) -> String {
    (1..=n).map(|i| format!("// {tag} line {i}\n")).collect()
}

fn repo() -> Vec<(String, String)> {
    vec![
        f("src/a.rs", &numbered(30, "a")),
        f("src/b.rs", &numbered(3, "b")),
        f("README.md", "# readme\n"),
    ]
}

fn piece(files: &[&str]) -> Piece {
    Piece {
        key: "acme/app#1:00000000".into(),
        unit: "acme/app".into(),
        files: files.iter().map(|s| (*s).to_string()).collect(),
    }
}

fn room_for(files: &[&str], repo_files: &[(String, String)]) -> Room {
    Room::build(&piece(files), repo_files, "# checklist\n").room
}

fn limits() -> PieceLimits {
    PieceLimits::for_piece(2, 1.0)
}

// ── The recorded trace, in the shape of a Claude Code session file ──────────────────────────────

fn usage() -> Value {
    json!({"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 100,
           "cache_creation_input_tokens": 50})
}

fn call(id: &str, name: &str, input: Value) -> String {
    json!({"type": "assistant", "request_id": format!("req_{id}"), "message": {
        "id": format!("msg_{id}"), "role": "assistant", "usage": usage(),
        "content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}})
    .to_string()
}

fn answer(id: &str, content: &str, lines: Option<usize>, is_error: bool) -> String {
    let mut line = json!({"type": "user", "message": {"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": id, "content": content, "is_error": is_error}]}});
    if let Some(n) = lines {
        line["toolUseResult"] = json!({"type": "text", "file": {"numLines": n}});
    }
    line.to_string()
}

/// A replay: every line, joined. `@ROOM@` stands for the room folder the stand-in is run in.
fn replay(lines: &[String]) -> String {
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

fn problems_json(n: u32, path: &str) -> String {
    json!([{ "n": n, "path": path, "line": 2, "end_line": 2, "severity": "high",
        "title": "Handler skips the check", "code": "// a line 2", "what": "It skips the check.",
        "why": "Anyone can call it.", "fix": "Check the caller.", "sure": "yes", "effort": "low",
        "label": "missing check" }])
    .to_string()
}

fn checklist_json() -> String {
    json!({"citations": [{"n": 1, "rule": "R-1"}],
           "answers": [{"rule": "R-1", "answer": "problems", "problems": [1]}]})
    .to_string()
}

/// The standard recorded piece: every kind of call and a denied write.
fn full_replay() -> String {
    replay(&[
        call("t1", "Glob", json!({"pattern": "**/*.rs"})),
        answer("t1", "src/a.rs\nsrc/b.rs", None, false),
        call("t2", "Read", json!({"file_path": "@ROOM@/src/a.rs"})),
        answer("t2", "1\t// a line 1", Some(30), false),
        call(
            "t3",
            "Read",
            json!({"file_path": "@ROOM@/src/b.rs", "offset": 2, "limit": 1}),
        ),
        answer("t3", "2\t// b line 2", Some(1), false),
        call("t4", "Grep", json!({"pattern": "unsafe"})),
        answer("t4", "No matches", None, false),
        call("t5", "LS", json!({"path": "@ROOM@/src"})),
        answer("t5", "a.rs\nb.rs", None, false),
        call(
            "t6",
            "Write",
            json!({"file_path": "@ROOM@/_camerata/out/problems-1.json", "content": "x"}),
        ),
        answer("t6", "written", None, false),
        call(
            "t7",
            "Write",
            json!({"file_path": "@ROOM@/src/a.rs", "content": "x"}),
        ),
        answer("t7", "permission denied", None, true),
    ])
}

// ── The stand-in program ────────────────────────────────────────────────────────────────────────

const OK_RESULT: &str = r#"{"type":"result","subtype":"success","is_error":false,"result":"done","total_cost_usd":0.5,"usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":100,"cache_creation_input_tokens":50}}"#;
const ERR_RESULT: &str = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"boom","total_cost_usd":0.5,"usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":100,"cache_creation_input_tokens":50}}"#;

struct Rig {
    dir: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        let rig = Self {
            dir: tempfile::tempdir().expect("temp dir"),
        };
        for d in ["log", "work"] {
            std::fs::create_dir_all(rig.path().join(d)).unwrap();
        }
        rig
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Call number `n` (1-based) prints `trace` (when given) on stdout, drops `outputs` into the
    /// room's out folder and then prints `result`.
    fn call(
        self,
        n: usize,
        trace: Option<String>,
        outputs: &[(&str, String)],
        result: &str,
    ) -> Self {
        if let Some(t) = trace {
            std::fs::write(self.path().join(format!("replay-{n}.jsonl")), t).unwrap();
        }
        let out = self.path().join(format!("out-{n}"));
        std::fs::create_dir_all(&out).unwrap();
        for (name, text) in outputs {
            std::fs::write(out.join(name), text).unwrap();
        }
        std::fs::write(self.path().join(format!("result-{n}.json")), result).unwrap();
        self
    }

    /// Call number `n` exits with `code` after its trace, printing no result.
    fn exit(self, n: usize, code: i32) -> Self {
        std::fs::write(self.path().join(format!("exit-{n}")), code.to_string()).unwrap();
        self
    }

    /// Call number `n` sleeps `secs` seconds after its trace, printing no result.
    fn sleep(self, n: usize, secs: u64) -> Self {
        std::fs::write(self.path().join(format!("sleep-{n}")), secs.to_string()).unwrap();
        self
    }

    /// Write the stand-in program and return it.
    fn program(&self) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let d = self.path().display();
        let script = format!(
            r#"#!/bin/sh
LOG="{d}/log"
n=$(( $(ls "$LOG" | grep -c '^args-') + 1 ))
printf '%s\n' "$@" > "$LOG/args-$n.txt"
printf '%s\n' "$CLAUDE_CODE_DISABLE_CLAUDE_MDS" > "$LOG/mds-$n.txt"
pwd -P > "$LOG/cwd-$n.txt"
find . -type f | sort > "$LOG/files-$n.txt"
sid=''
prev=''
for a in "$@"; do
  if [ "$prev" = "--session-id" ]; then sid="$a"; fi
  prev="$a"
done
printf '%s\n' "$sid" > "$LOG/sid-$n.txt"
if [ -d "{d}/out-$n" ]; then
  mkdir -p _camerata/out
  cp -R "{d}/out-$n/." _camerata/out/
fi
if [ -f "{d}/replay-$n.jsonl" ]; then
  sed "s#@ROOM@#$(pwd -P)#g" "{d}/replay-$n.jsonl"
fi
if [ -f "{d}/sleep-$n" ]; then exec sleep "$(cat "{d}/sleep-$n")"; fi
if [ -f "{d}/exit-$n" ]; then exit "$(cat "{d}/exit-$n")"; fi
cat "{d}/result-$n.json"
exit 0
"#
        );
        let bin = self.path().join("claude");
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    fn agent(&self) -> CliPieceAgent {
        CliPieceAgent::new(MODEL, self.path().join("work"), EngagementKind::Internal)
            .with_program(self.program())
    }

    fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.path().join("log").join(name))
            .unwrap_or_else(|e| panic!("{name}: {e}"))
    }
}

fn read_tree(dir: &Path, out: &mut BTreeMap<PathBuf, String>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            read_tree(&p, out);
        } else {
            out.insert(
                p.clone(),
                String::from_utf8_lossy(&std::fs::read(&p).unwrap()).into_owned(),
            );
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn outputs_and_trace_come_back_as_scripted() {
    let rig = Rig::new().call(
        1,
        Some(full_replay()),
        &[
            ("problems-1.json", problems_json(1, "src/a.rs")),
            ("card.md", "a card".into()),
        ],
        OK_RESULT,
    );
    let room = room_for(&["src/a.rs", "src/b.rs"], &repo());
    let run = rig
        .agent()
        .run(&room, "THE BRIEF", &limits())
        .await
        .expect("a run");
    assert_eq!(run.stop, AgentStop::Finished, "{:?}", run.stop);
    assert_eq!(
        run.trace,
        vec![
            ToolEvent::Glob {
                pattern: "**/*.rs".into()
            },
            ToolEvent::Read {
                path: "src/a.rs".into(),
                first_line: 1,
                line_count: 2000,
                lines_returned: 30
            },
            ToolEvent::Read {
                path: "src/b.rs".into(),
                first_line: 2,
                line_count: 1,
                lines_returned: 1
            },
            ToolEvent::Grep {
                pattern: "unsafe".into()
            },
            ToolEvent::Ls { path: "src".into() },
            ToolEvent::Write {
                path: "_camerata/out/problems-1.json".into()
            },
        ]
    );
    let keys: Vec<&String> = run.outputs.keys().collect();
    assert_eq!(
        keys,
        ["_camerata/out/card.md", "_camerata/out/problems-1.json"]
    );
    assert_eq!(run.outputs["_camerata/out/card.md"], "a card");
    assert!(run.usage.iter().map(|u| u.input_tokens).sum::<u64>() > 0);
    assert!(run.usage.iter().filter_map(|u| u.cost_usd).sum::<f64>() > 0.0);
    // The brief is what the program was asked.
    assert!(rig.log("args-1.txt").lines().any(|l| l == "THE BRIEF"));
}

#[tokio::test]
async fn a_read_with_offset_and_limit_is_recorded_as_a_partial_read() {
    let rig = Rig::new().call(1, Some(full_replay()), &[], OK_RESULT);
    let room = room_for(&["src/a.rs", "src/b.rs"], &repo());
    let run = rig.agent().run(&room, "b", &limits()).await.unwrap();
    let cov = coverage_of(
        "k",
        &[run],
        &room.piece_line_counts(),
        &[],
        room.context_files(),
        None,
    );
    assert_eq!(
        cov.files["src/a.rs"],
        super::super::coverage::FileState::ReadInFull
    );
    assert_eq!(
        cov.files["src/b.rs"],
        super::super::coverage::FileState::ReadInPart(vec![(2, 2)])
    );
}

#[tokio::test]
async fn a_missing_stream_fails_the_piece_and_coverage_is_never_assumed() {
    // The stand-in writes the output files and prints a good result, but no tool-call record.
    let rig = Rig::new().call(
        1,
        None,
        &[("problems-1.json", problems_json(1, "src/a.rs"))],
        OK_RESULT,
    );
    let room = room_for(&["src/a.rs"], &repo());
    let run = rig
        .agent()
        .run(&room, "b", &limits())
        .await
        .expect("a failed run, not an error");
    match &run.stop {
        AgentStop::Failed(why) => assert!(why.contains("stream"), "{why}"),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(run.trace.is_empty());
    assert!(
        run.usage.iter().map(|u| u.input_tokens).sum::<u64>() > 0,
        "the paid call is counted"
    );
}

/// Paid turns are always counted: a call that fails after it was paid for comes back as
/// `Ok(AgentRun)` with `Failed` and the usage, never as `Err`.
#[tokio::test]
async fn a_call_that_fails_after_it_was_paid_for_still_reports_its_usage() {
    let rig = Rig::new().call(1, Some(full_replay()), &[], ERR_RESULT);
    let room = room_for(&["src/a.rs", "src/b.rs"], &repo());
    let run = rig
        .agent()
        .run(&room, "b", &limits())
        .await
        .expect("a call that was paid for is never an Err");
    match &run.stop {
        AgentStop::Failed(why) => assert!(why.contains("boom"), "{why}"),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(
        run.usage.iter().map(|u| u.input_tokens).sum::<u64>() > 0,
        "{:?}",
        run.usage
    );
    assert!(
        !run.trace.is_empty(),
        "the work done before the failure is kept"
    );
}

#[tokio::test]
async fn a_program_that_cannot_start_is_an_error_before_anything_was_sent_or_a_failed_run_with_no_usage(
) {
    let rig = Rig::new();
    let agent = CliPieceAgent::new(MODEL, rig.path().join("work"), EngagementKind::Internal)
        .with_program(rig.path().join("no-such-program"));
    let room = room_for(&["src/a.rs"], &repo());
    match agent.run(&room, "b", &limits()).await {
        Err(_) => {}
        Ok(run) => {
            assert!(matches!(run.stop, AgentStop::Failed(_)));
            assert!(run.usage.is_empty());
        }
    }
}

#[tokio::test]
async fn the_room_folder_holds_redacted_text_only() {
    let secret = planted(SecretShape::StripeLiveSecretKey);
    let mut files = repo();
    files[0].1 = format!("const KEY = \"{}\";\n{}", secret.text, numbered(5, "a"));
    let rig = Rig::new().call(1, Some(full_replay()), &[], OK_RESULT);
    let room = room_for(&["src/a.rs"], &files);
    rig.agent().run(&room, "b", &limits()).await.unwrap();
    let mut tree = BTreeMap::new();
    read_tree(&rig.path().join("work"), &mut tree);
    assert!(
        tree.keys().any(|p| p.ends_with("src/a.rs")),
        "the piece file is in the folder"
    );
    for (path, text) in &tree {
        assert!(!text.contains(&secret.text), "{path:?} holds the secret");
    }
}

#[tokio::test]
async fn the_clean_room_holds_only_the_piece_its_context_and_the_rooms_own_files() {
    let rig = Rig::new().call(1, Some(full_replay()), &[], OK_RESULT);
    let room = room_for(&["src/a.rs"], &repo());
    rig.agent().run(&room, "b", &limits()).await.unwrap();
    let listed: Vec<String> = rig
        .log("files-1.txt")
        .lines()
        .map(|l| l.trim_start_matches("./").to_string())
        .collect();
    let mut allowed: Vec<String> = room.paths().iter().map(|p| Room::disk_path(p)).collect();
    allowed.sort();
    assert_eq!(listed, allowed, "nothing but the room's own files");
    // The repository's files sit under `code/`, the room's own beside it.
    assert!(listed.iter().any(|p| p == "code/src/a.rs"), "{listed:?}");
    assert!(
        listed.iter().any(|p| p == "_camerata/checklist.md"),
        "{listed:?}"
    );
    assert!(
        !listed
            .iter()
            .any(|p| p == "code/src/b.rs" || p == "code/README.md"),
        "{listed:?}"
    );
}

#[tokio::test]
async fn the_session_runs_in_isolation_with_the_room_as_its_folder() {
    let rig = Rig::new().call(1, Some(full_replay()), &[], OK_RESULT);
    let room = room_for(&["src/a.rs"], &repo());
    let run = rig.agent().run(&room, "b", &limits()).await.unwrap();
    let args = rig.log("args-1.txt");
    assert_eq!(
        args_after(&args, "--setting-sources").as_deref(),
        Some("project")
    );
    for flag in [
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--setting-sources",
        "--disallowedTools",
        "stream-json",
    ] {
        assert!(
            args.lines().any(|l| l == flag),
            "{flag} missing from {args}"
        );
    }
    for tool in ["Bash", "WebFetch", "WebSearch", "Task", "Agent"] {
        assert!(args.lines().any(|l| l == tool), "{tool} is not denied");
    }
    assert!(
        args.lines()
            .any(|l| l.starts_with("Edit(//") && l.ends_with("/_camerata/out/**)")),
        "{args}"
    );
    assert_eq!(
        rig.log("mds-1.txt").trim(),
        "1",
        "no instruction file is read"
    );
    let cwd = PathBuf::from(rig.log("cwd-1.txt").trim());
    let work = std::fs::canonicalize(rig.path().join("work")).unwrap();
    assert!(cwd.starts_with(&work), "{cwd:?} is not under {work:?}");
    assert!(
        !cwd.starts_with("/tmp") && !cwd.starts_with("/private/tmp"),
        "{cwd:?}"
    );
    // The handle is the session id: small.
    assert_eq!(run.conversation.as_str().len(), 36);
    assert_eq!(run.conversation.as_str(), rig.log("sid-1.txt").trim());
}

#[tokio::test]
async fn send_again_is_a_second_call_on_a_room_holding_only_the_unread_files() {
    let rig = Rig::new()
        .call(
            1,
            Some(full_replay()),
            &[("problems-1.json", problems_json(1, "src/a.rs"))],
            OK_RESULT,
        )
        .call(
            2,
            Some(replay(&[
                call("u1", "Read", json!({"file_path": "@ROOM@/src/b.rs"})),
                answer("u1", "1\t// b line 1", Some(3), false),
            ])),
            &[("problems-2.json", problems_json(2, "src/b.rs"))],
            OK_RESULT,
        );
    let room = room_for(&["src/a.rs", "src/b.rs"], &repo());
    let agent = rig.agent();
    let first = agent.run(&room, "b", &limits()).await.unwrap();
    let again = send_again(&agent, &room, &first, &["src/b.rs".to_string()], &limits())
        .await
        .unwrap();
    assert_eq!(again.stop, AgentStop::Finished);
    let listed = rig.log("files-2.txt");
    assert!(listed.contains("./code/src/b.rs"), "{listed}");
    assert!(!listed.contains("./code/src/a.rs"), "{listed}");
    assert!(listed.contains("_camerata/checklist.md"), "{listed}");
    // Only this step's work comes back.
    let keys: Vec<&String> = again.outputs.keys().collect();
    assert_eq!(keys, ["_camerata/out/problems-2.json"]);
    assert_eq!(
        again.trace,
        vec![ToolEvent::Read {
            path: "src/b.rs".into(),
            first_line: 1,
            line_count: 2000,
            lines_returned: 3
        }]
    );
    // The file went to the model in full, in the message.
    assert_eq!(again.shown_in_full, vec!["src/b.rs".to_string()]);
    assert!(rig.log("args-2.txt").contains("// b line 3"));
    assert_ne!(first.conversation, again.conversation);
}

#[tokio::test]
async fn a_failed_send_again_shows_nothing_in_full() {
    let rig = Rig::new()
        .call(1, Some(full_replay()), &[], OK_RESULT)
        .call(2, Some(replay(&[])), &[], ERR_RESULT);
    let room = room_for(&["src/a.rs", "src/b.rs"], &repo());
    let agent = rig.agent();
    let first = agent.run(&room, "b", &limits()).await.unwrap();
    let again = send_again(&agent, &room, &first, &["src/b.rs".to_string()], &limits())
        .await
        .unwrap();
    assert!(matches!(again.stop, AgentStop::Failed(_)));
    assert!(again.shown_in_full.is_empty());
}

#[test]
fn a_client_engagement_still_refuses_the_cli_backend_for_the_finder() {
    let why = crate::client_lock::refusal(
        true,
        crate::llm::ProjectBackend::Cli,
        &[(
            "the finder".to_string(),
            super::super::DEFAULT_FINDER_MODEL.to_string(),
        )],
    )
    .expect("refused");
    assert!(why.contains("`cli` backend"), "{why}");
}

/// A client folder named `_Camerata` (any case) collides with the room's own folder on a case
/// insensitive disk. It is refused the same way as `_camerata`: not served, owed, reported.
#[test]
fn a_client_folder_named_like_the_rooms_folder_in_any_case_is_not_served() {
    for dir in ["_camerata", "_Camerata", "_CAMERATA", "_cAmErAtA"] {
        let path = format!("{dir}/notes.rs");
        let mut files = repo();
        files.push(f(&path, "pub fn x() {}\n"));
        let built = Room::build(&piece(&["src/a.rs", &path]), &files, "# checklist\n");
        assert!(
            built.not_served.iter().any(|n| n.path == path),
            "{dir}: {:?}",
            built.not_served
        );
        assert!(!built.room.paths().iter().any(|p| p == &path), "{dir}");
        assert!(
            !built.room.piece_files().iter().any(|p| p == &path),
            "{dir}"
        );
        let tmp = tempfile::tempdir().unwrap();
        built.room.materialize(tmp.path()).unwrap();
        let mut tree = BTreeMap::new();
        read_tree(tmp.path(), &mut tree);
        assert!(
            !tree.values().any(|t| t.contains("pub fn x()")),
            "{dir}: the client's file reached the folder"
        );
    }
}

// ── Parity: one scripted piece through the stand-in and through `ScriptedAgent` ─────────────────

/// What the parity test compares: the parsed problems, the checklist result and the coverage.
#[derive(Debug, PartialEq)]
pub(crate) struct ParitySummary {
    pub problems: Vec<(u32, String, usize, String, String)>,
    pub checklist: CheckOutcome,
    pub coverage: PieceCoverage,
}

/// The summary of one run, through build 1's own `reply`, `checklist` and `coverage`. Public to
/// the crate so the API transport's test can compare itself with the same helper.
pub(crate) fn parity_summary(room: &Room, checklist: &Checklist, run: &AgentRun) -> ParitySummary {
    let parsed = parse_outputs(&run.outputs, room.piece_files());
    let file = parsed
        .checklist
        .as_ref()
        .expect("the run wrote checklist.json");
    let counts = room.piece_line_counts();
    ParitySummary {
        problems: parsed
            .problems
            .iter()
            .map(|p| {
                (
                    p.n,
                    p.path.clone(),
                    p.line,
                    p.severity.clone(),
                    p.title.clone(),
                )
            })
            .collect(),
        checklist: checklist.check(file, &parsed.problems, &run.trace, &counts),
        coverage: coverage_of(
            "k",
            std::slice::from_ref(run),
            &counts,
            &[],
            room.context_files(),
            parsed.unread.as_deref(),
        ),
    }
}

pub(crate) fn parity_checklist() -> Checklist {
    Checklist {
        rules: vec![ChecklistRule {
            id: "R-1".into(),
            title: "No unchecked handlers".into(),
            directive: "Check the caller.".into(),
            min_severity: None,
            max_severity: None,
            options: Vec::new(),
        }],
    }
}

/// The one scripted piece: the steps a `ScriptedAgent` plays.
pub(crate) fn parity_script() -> Vec<ScriptStep> {
    vec![
        ScriptStep::Glob("**/*.rs".into()),
        ScriptStep::Read {
            path: "src/a.rs".into(),
            first_line: 1,
            count: 2000,
        },
        ScriptStep::Read {
            path: "src/b.rs".into(),
            first_line: 1,
            count: 2,
        },
        ScriptStep::Write {
            path: "_camerata/out/problems-1.json".into(),
            text: problems_json(1, "src/a.rs"),
        },
        ScriptStep::Write {
            path: "_camerata/out/checklist.json".into(),
            text: checklist_json(),
        },
    ]
}

/// The same piece as a recording for the stand-in program.
fn parity_replay() -> String {
    replay(&[
        call("p1", "Glob", json!({"pattern": "**/*.rs"})),
        answer("p1", "src/a.rs\nsrc/b.rs", None, false),
        call("p2", "Read", json!({"file_path": "@ROOM@/src/a.rs"})),
        answer("p2", "1\t// a line 1", Some(30), false),
        call(
            "p3",
            "Read",
            json!({"file_path": "@ROOM@/src/b.rs", "limit": 2}),
        ),
        answer("p3", "1\t// b line 1", Some(2), false),
        call(
            "p4",
            "Write",
            json!({"file_path": "@ROOM@/_camerata/out/problems-1.json", "content": "x"}),
        ),
        answer("p4", "ok", None, false),
        call(
            "p5",
            "Write",
            json!({"file_path": "@ROOM@/_camerata/out/checklist.json", "content": "x"}),
        ),
        answer("p5", "ok", None, false),
    ])
}

#[tokio::test]
async fn the_command_line_transport_and_the_scripted_agent_give_the_same_parsed_result() {
    let rig = Rig::new().call(
        1,
        Some(parity_replay()),
        &[
            ("problems-1.json", problems_json(1, "src/a.rs")),
            ("checklist.json", checklist_json()),
        ],
        OK_RESULT,
    );
    let scripted_room = room_for(&["src/a.rs", "src/b.rs"], &repo());
    let scripted = ScriptedAgent::new(parity_script())
        .run(&scripted_room, "b", &limits())
        .await
        .unwrap();
    let cli_room = room_for(&["src/a.rs", "src/b.rs"], &repo());
    let cli = rig.agent().run(&cli_room, "b", &limits()).await.unwrap();
    let want = parity_summary(&scripted_room, &parity_checklist(), &scripted);
    let got = parity_summary(&cli_room, &parity_checklist(), &cli);
    assert_eq!(want.problems.len(), 1);
    assert_eq!(got, want);
}

// ── Round 1 fixes ───────────────────────────────────────────────────────────────────────────────

fn args_after(args: &str, flag: &str) -> Option<String> {
    let lines: Vec<&str> = args.lines().collect();
    let at = lines.iter().position(|l| *l == flag)?;
    lines.get(at + 1).map(|l| (*l).to_string())
}

/// A one-call replay, answered, for the outside-the-room cases.
fn one_call(name: &str, input: Value) -> String {
    replay(&[call("o1", name, input), answer("o1", "denied", None, true)])
}

#[tokio::test]
async fn the_session_loads_project_settings_only_and_a_pieces_own_claude_files_are_just_files() {
    let mut files = repo();
    files.push(f(".claude/settings.json", "{\"hooks\": {}}\n"));
    files.push(f(".mcp.json", "{\"mcpServers\": {}}\n"));
    let rig = Rig::new().call(1, Some(full_replay()), &[], OK_RESULT);
    let room = room_for(&["src/a.rs", ".claude/settings.json", ".mcp.json"], &files);
    assert!(room
        .piece_files()
        .iter()
        .any(|p| p == ".claude/settings.json"));
    assert!(room.piece_files().iter().any(|p| p == ".mcp.json"));
    assert!(room.contains(".claude/settings.json") && room.contains(".mcp.json"));
    rig.agent().run(&room, "b", &limits()).await.unwrap();
    let args = rig.log("args-1.txt");
    assert_eq!(
        args_after(&args, "--setting-sources").as_deref(),
        Some("project"),
        "{args}"
    );
    assert!(!args.lines().any(|l| l == "user"), "{args}");
    assert!(args.lines().any(|l| l == "--strict-mcp-config"), "{args}");
    assert!(
        args.lines().any(|l| l == "--disable-slash-commands"),
        "{args}"
    );
    // The client's files are under `code/`; nothing of them is at the working directory.
    let listed = rig.log("files-1.txt");
    assert!(listed.contains("./code/.claude/settings.json"), "{listed}");
    assert!(listed.contains("./code/.mcp.json"), "{listed}");
    assert!(!listed.lines().any(|l| l == "./.mcp.json"), "{listed}");
    assert!(
        !listed.lines().any(|l| l.starts_with("./.claude/")),
        "{listed}"
    );
    // The working directory is the room's root, the folder that holds `code/`.
    let cwd = PathBuf::from(rig.log("cwd-1.txt").trim());
    assert!(cwd.join("code").is_dir() && cwd.join("_camerata").is_dir());
}

#[tokio::test]
async fn the_agent_is_told_where_the_files_are() {
    let rig = Rig::new().call(1, Some(full_replay()), &[], OK_RESULT);
    let room = room_for(&["src/a.rs"], &repo());
    rig.agent()
        .run(&room, "THE BRIEF", &limits())
        .await
        .unwrap();
    let args = rig.log("args-1.txt");
    assert!(args.contains("THE BRIEF"), "{args}");
    assert!(args.contains("`code/`"), "{args}");
}

#[tokio::test]
async fn a_read_outside_the_room_fails_the_piece() {
    for (name, input) in [
        ("Read", json!({"file_path": "/Users/auditor/other/x"})),
        (
            "Grep",
            json!({"pattern": "k", "path": "/Users/auditor/other"}),
        ),
        (
            "Glob",
            json!({"pattern": "*.rs", "path": "/Users/auditor/other"}),
        ),
        ("LS", json!({"path": "/Users/auditor/other"})),
        ("Read", json!({"file_path": "@ROOM@/../outside.txt"})),
        ("Read", json!({"file_path": "../outside.txt"})),
        ("Read", json!({"file_path": "~/other/x"})),
        ("Read", json!({"file_path": "$HOME/other/x"})),
        ("Grep", json!({"pattern": "k", "path": "~/other"})),
        ("Glob", json!({"pattern": "../../**/*KEY*"})),
        ("Glob", json!({"pattern": "~/other/*.md"})),
        ("Glob", json!({"pattern": "src/../../x/*"})),
        ("Glob", json!({"pattern": "{../..,x}/*"})),
        ("Glob", json!({"pattern": "$HOME/x"})),
        ("Glob", json!({"pattern": "~/x"})),
        (
            "Glob",
            json!({"pattern": "{/Users/auditor/other,code}/**/*"}),
        ),
        ("Glob", json!({"pattern": "x/{a,../../b}/*"})),
        ("Glob", json!({"pattern": "**/../../*KEY*"})),
        ("Glob", json!({"pattern": "*.rs", "path": "{x,$HOME}/y"})),
        ("Grep", json!({"pattern": "k", "glob": "../**"})),
        (
            "Write",
            json!({"file_path": "/Users/auditor/x", "content": "x"}),
        ),
    ] {
        let rig = Rig::new().call(1, Some(one_call(name, input.clone())), &[], OK_RESULT);
        let room = room_for(&["src/a.rs"], &repo());
        let run = rig.agent().run(&room, "b", &limits()).await.unwrap();
        match &run.stop {
            AgentStop::Failed(why) => assert!(why.contains("outside"), "{name} {input}: {why}"),
            other => panic!("{name} {input}: expected Failed, got {other:?}"),
        }
        assert!(
            !run.trace
                .iter()
                .any(|e| matches!(e, ToolEvent::Read { path, .. } if path.contains("auditor"))),
            "{name}: an outside path is not kept as written"
        );
    }
}

#[tokio::test]
async fn paths_inside_the_room_are_not_outside() {
    let rig = Rig::new().call(
        1,
        Some(replay(&[
            call("i1", "Read", json!({"file_path": "@ROOM@/code/src/a.rs"})),
            answer("i1", "1\tx", Some(30), false),
            call("i2", "Grep", json!({"pattern": "k", "path": "@ROOM@/code"})),
            answer("i2", "none", None, false),
            call(
                "i3",
                "Read",
                json!({"file_path": "@ROOM@/_camerata/checklist.md"}),
            ),
            answer("i3", "1\tx", Some(1), false),
            call(
                "i4",
                "Read",
                json!({"file_path": "@ROOM@/code/../_camerata/checklist.md"}),
            ),
            answer("i4", "1\tx", Some(1), false),
        ])),
        &[],
        OK_RESULT,
    );
    let room = room_for(&["src/a.rs"], &repo());
    let run = rig.agent().run(&room, "b", &limits()).await.unwrap();
    assert_eq!(run.stop, AgentStop::Finished, "{:?}", run.stop);
    let reads: Vec<&str> = run
        .trace
        .iter()
        .filter_map(|e| match e {
            ToolEvent::Read { path, .. } => Some(path.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        reads,
        [
            "src/a.rs",
            "_camerata/checklist.md",
            "_camerata/checklist.md"
        ]
    );
}

#[tokio::test]
async fn a_client_engagement_is_refused_before_a_room_is_written_or_the_program_started() {
    let rig = Rig::new().call(1, Some(full_replay()), &[], OK_RESULT);
    let agent = CliPieceAgent::new(MODEL, rig.path().join("work"), EngagementKind::Client)
        .with_program(rig.program());
    let room = room_for(&["src/a.rs"], &repo());
    let err = agent.run(&room, "b", &limits()).await.expect_err("refused");
    assert!(err.to_string().contains("internal"), "{err}");
    let prior = AgentRun::empty(AgentStop::Finished);
    let err = agent
        .follow_up(
            &room,
            &prior,
            &super::super::FollowUp::text("again"),
            &limits(),
        )
        .await
        .expect_err("refused");
    assert!(err.to_string().contains("internal"), "{err}");
    let work = rig.path().join("work");
    assert_eq!(
        std::fs::read_dir(&work).unwrap().count(),
        0,
        "no room folder was created"
    );
    assert_eq!(
        std::fs::read_dir(rig.path().join("log")).unwrap().count(),
        0,
        "the program was never started"
    );
}

#[tokio::test]
async fn the_call_is_bounded_in_turns_and_keeps_no_session_file() {
    let rig = Rig::new().call(1, Some(full_replay()), &[], OK_RESULT);
    let room = room_for(&["src/a.rs"], &repo());
    let lim = limits();
    rig.agent().run(&room, "b", &lim).await.unwrap();
    let args = rig.log("args-1.txt");
    assert_eq!(
        args_after(&args, "--max-turns"),
        Some(lim.max_turns.to_string()),
        "{args}"
    );
    assert!(
        args.lines().any(|l| l == "--no-session-persistence"),
        "{args}"
    );
}

#[tokio::test]
async fn the_transport_works_with_no_session_file_anywhere() {
    // The stand-in writes no session file; the trace comes from stdout alone.
    let rig = Rig::new().call(1, Some(full_replay()), &[], OK_RESULT);
    let room = room_for(&["src/a.rs", "src/b.rs"], &repo());
    let run = rig.agent().run(&room, "b", &limits()).await.unwrap();
    assert_eq!(run.stop, AgentStop::Finished, "{:?}", run.stop);
    assert!(!run.trace.is_empty());
}

#[tokio::test]
async fn a_call_that_passes_the_wall_clock_bound_fails_inside_it_with_the_trace_usage() {
    let rig = Rig::new()
        .call(1, Some(full_replay()), &[], OK_RESULT)
        .sleep(1, 30);
    let room = room_for(&["src/a.rs"], &repo());
    let started = std::time::Instant::now();
    let run = rig
        .agent()
        .with_wall_clock(std::time::Duration::from_secs(2))
        .run(&room, "b", &limits())
        .await
        .expect("a failed run, not an error");
    assert!(started.elapsed() < std::time::Duration::from_secs(15));
    match &run.stop {
        AgentStop::Failed(why) => assert!(why.contains("wall-clock"), "{why}"),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(!run.trace.is_empty(), "the work done is kept");
    assert!(
        run.usage.iter().map(|u| u.input_tokens).sum::<u64>() > 0,
        "usage comes from the trace: {:?}",
        run.usage
    );
}

#[test]
fn the_wall_clock_bound_follows_the_turn_limit_within_fixed_bounds() {
    use std::time::Duration;
    assert_eq!(
        PieceLimits::for_piece(2, 1.0).wall_clock(),
        Duration::from_secs(3600)
    );
    assert_eq!(
        PieceLimits::for_piece(1000, 1.0).wall_clock(),
        Duration::from_secs(4 * 3600)
    );
    let mut small = PieceLimits::for_piece(2, 1.0);
    small.max_turns = 3;
    assert_eq!(small.wall_clock(), Duration::from_secs(900));
}

/// Coverage (a): when the reply is good, only the last attempt's reads count. The first attempt
/// read src/a.rs and then failed; the retry that answered read src/b.rs only.
#[tokio::test]
async fn a_failed_attempts_reads_never_count_toward_the_retrys_reply() {
    let first = replay(&[
        call("a1", "Read", json!({"file_path": "@ROOM@/code/src/a.rs"})),
        answer("a1", "1\tx", Some(30), false),
    ]);
    let second = replay(&[
        call("b1", "Read", json!({"file_path": "@ROOM@/code/src/b.rs"})),
        answer("b1", "1\tx", Some(3), false),
    ]);
    let rig = Rig::new()
        .call(1, Some(first), &[], OK_RESULT)
        .exit(1, 1)
        .call(2, Some(second), &[], OK_RESULT);
    let room = room_for(&["src/a.rs", "src/b.rs"], &repo());
    let run = rig.agent().run(&room, "b", &limits()).await.unwrap();
    assert_eq!(run.stop, AgentStop::Finished, "{:?}", run.stop);
    let cov = coverage_of(
        "k",
        &[run],
        &room.piece_line_counts(),
        &[],
        room.context_files(),
        None,
    );
    assert_eq!(
        cov.files["src/b.rs"],
        super::super::coverage::FileState::ReadInFull
    );
    assert_eq!(
        cov.files["src/a.rs"],
        super::super::coverage::FileState::NotReviewed,
        "the failed attempt's read does not count"
    );
}

/// Coverage (b): a read before a context compaction is gone from the agent's context.
#[tokio::test]
async fn reads_before_a_context_compaction_do_not_count() {
    let rig = Rig::new().call(
        1,
        Some(replay(&[
            call("c1", "Read", json!({"file_path": "@ROOM@/code/src/a.rs"})),
            answer("c1", "1\tx", Some(30), false),
            json!({"type": "system", "subtype": "compact_boundary"}).to_string(),
            call("c2", "Read", json!({"file_path": "@ROOM@/code/src/b.rs"})),
            answer("c2", "1\tx", Some(3), false),
        ])),
        &[],
        OK_RESULT,
    );
    let room = room_for(&["src/a.rs", "src/b.rs"], &repo());
    let run = rig.agent().run(&room, "b", &limits()).await.unwrap();
    assert_eq!(
        run.trace,
        vec![ToolEvent::Read {
            path: "src/b.rs".into(),
            first_line: 1,
            line_count: 2000,
            lines_returned: 3
        }]
    );
}

/// Coverage (c): Claude Code cuts a line over 2,000 characters, so a read that returned such a
/// line did not show it whole.
#[tokio::test]
async fn a_file_with_a_line_over_two_thousand_characters_is_never_read_in_full_by_trace() {
    let mut text = numbered(10, "w");
    text = text.replacen("// w line 5\n", &format!("// {}\n", "x".repeat(2_500)), 1);
    let files = vec![f("src/w.rs", &text)];
    let rig = Rig::new().call(
        1,
        Some(replay(&[
            call("w1", "Read", json!({"file_path": "@ROOM@/code/src/w.rs"})),
            answer("w1", "1\tx", Some(10), false),
        ])),
        &[],
        OK_RESULT,
    );
    let room = room_for(&["src/w.rs"], &files);
    let run = rig.agent().run(&room, "b", &limits()).await.unwrap();
    let cov = coverage_of(
        "k",
        &[run],
        &room.piece_line_counts(),
        &[],
        room.context_files(),
        None,
    );
    assert_eq!(
        cov.files["src/w.rs"],
        super::super::coverage::FileState::ReadInPart(vec![(1, 4), (6, 10)])
    );
    assert_eq!(cov.still_unread, vec!["src/w.rs".to_string()]);
}

/// A refusal is detected from the result record's stop reason, never from the words of the error.
#[tokio::test]
async fn a_refusal_comes_from_the_structured_stop_reason_not_from_a_word() {
    let refusal = r#"{"type":"result","subtype":"success","is_error":false,"stop_reason":"refusal","result":"no","total_cost_usd":0.5,"usage":{"input_tokens":10,"output_tokens":5}}"#;
    let rig = Rig::new().call(1, Some(full_replay()), &[], refusal);
    let room = room_for(&["src/a.rs"], &repo());
    let run = rig.agent().run(&room, "b", &limits()).await.unwrap();
    assert!(matches!(run.stop, AgentStop::Refused(_)), "{:?}", run.stop);

    // The word alone, in an ordinary failure, is a failure.
    let wordy = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"the refusal handler crashed","total_cost_usd":0.5,"usage":{"input_tokens":10,"output_tokens":5}}"#;
    let rig = Rig::new().call(1, Some(full_replay()), &[], wordy);
    let run = rig.agent().run(&room, "b", &limits()).await.unwrap();
    assert!(matches!(run.stop, AgentStop::Failed(_)), "{:?}", run.stop);
}

#[tokio::test]
async fn running_out_of_turns_is_a_turn_limit_stop() {
    let max_turns = r#"{"type":"result","subtype":"error_max_turns","is_error":true,"result":"partial","total_cost_usd":0.5,"usage":{"input_tokens":10,"output_tokens":5}}"#;
    let rig = Rig::new().call(1, Some(full_replay()), &[], max_turns);
    let room = room_for(&["src/a.rs"], &repo());
    let run = rig.agent().run(&room, "b", &limits()).await.unwrap();
    assert!(
        matches!(run.stop, AgentStop::TurnLimit(_)),
        "{:?}",
        run.stop
    );
}

/// A failed call's tokens come from the result record on stdout when it printed one.
#[tokio::test]
async fn a_failed_calls_usage_comes_from_its_result_record() {
    let rig = Rig::new().call(1, Some(full_replay()), &[], ERR_RESULT);
    let room = room_for(&["src/a.rs"], &repo());
    let run = rig.agent().run(&room, "b", &limits()).await.unwrap();
    assert_eq!(run.usage.len(), 1, "{:?}", run.usage);
    let u = &run.usage[0];
    assert_eq!(u.output_tokens, 5);
    assert_eq!(u.cache_read_input_tokens, 100);
    assert_eq!(u.cache_creation_input_tokens, 50);
    assert_eq!(u.input_tokens, 10 + 100 + 50);
    assert_eq!(u.cost_usd, Some(0.5));
}

/// The session file spells the request id `requestId`: two records of one request are one turn.
#[test]
fn a_session_file_request_id_is_read_in_its_own_spelling() {
    let rec = |out: u64| {
        json!({"type": "assistant", "requestId": "req_1", "message": {"role": "assistant",
            "usage": {"input_tokens": 1, "output_tokens": out}, "content": []}})
        .to_string()
    };
    let parsed = parse_session(
        &format!("{}\n{}\n", rec(3), rec(9)),
        &[PathBuf::from("/room")],
        &BTreeMap::new(),
    );
    assert_eq!(parsed.usage.len(), 1, "{:?}", parsed.usage);
    assert_eq!(parsed.usage[0].output_tokens, 9);
}

#[tokio::test]
async fn a_successful_call_to_a_tool_outside_the_set_fails_the_piece() {
    for name in ["Bash", "WebFetch", "Task", "SomeFutureTool", "NotebookEdit"] {
        let rig = Rig::new().call(
            1,
            Some(replay(&[
                call("u1", name, json!({"command": "ls ~"})),
                answer("u1", "ok", None, false),
            ])),
            &[],
            OK_RESULT,
        );
        let room = room_for(&["src/a.rs"], &repo());
        let run = rig.agent().run(&room, "b", &limits()).await.unwrap();
        match &run.stop {
            AgentStop::Failed(why) => assert!(why.contains("outside"), "{name}: {why}"),
            other => panic!("{name}: expected Failed, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_refused_call_to_a_tool_outside_the_set_and_the_todo_tool_are_fine() {
    let rig = Rig::new().call(
        1,
        Some(replay(&[
            call("u1", "Bash", json!({"command": "ls"})),
            answer("u1", "denied", None, true),
            call("u2", "TodoWrite", json!({"todos": []})),
            answer("u2", "ok", None, false),
        ])),
        &[],
        OK_RESULT,
    );
    let room = room_for(&["src/a.rs"], &repo());
    let run = rig.agent().run(&room, "b", &limits()).await.unwrap();
    assert!(!matches!(run.stop, AgentStop::Failed(_)), "{:?}", run.stop);
}

#[tokio::test]
async fn a_denied_search_inside_the_room_records_nothing_and_one_outside_still_fails() {
    let rig = Rig::new().call(
        1,
        Some(replay(&[
            call("s1", "Grep", json!({"pattern": "k", "path": "@ROOM@/code"})),
            answer("s1", "denied", None, true),
            call("s2", "Glob", json!({"pattern": "src/*.rs"})),
            answer("s2", "denied", None, true),
        ])),
        &[],
        OK_RESULT,
    );
    let room = room_for(&["src/a.rs"], &repo());
    let run = rig.agent().run(&room, "b", &limits()).await.unwrap();
    assert!(
        !run.trace
            .iter()
            .any(|e| matches!(e, ToolEvent::Grep { .. } | ToolEvent::Glob { .. })),
        "{:?}",
        run.trace
    );
    // The same denied calls aimed outside the room fail the piece.
    let rig = Rig::new().call(
        1,
        Some(replay(&[
            call("s1", "Grep", json!({"pattern": "k", "path": "~/other"})),
            answer("s1", "denied", None, true),
        ])),
        &[],
        OK_RESULT,
    );
    let run = rig.agent().run(&room, "b", &limits()).await.unwrap();
    assert!(matches!(run.stop, AgentStop::Failed(_)), "{:?}", run.stop);
}

#[tokio::test]
async fn real_folder_names_with_a_dollar_or_tilde_inside_a_path_are_fine() {
    for (name, input) in [
        ("Glob", json!({"pattern": "routes/$id.tsx"})),
        ("Grep", json!({"pattern": "k", "path": "app/~shared"})),
        ("Glob", json!({"pattern": "{src,routes}/$id.tsx"})),
    ] {
        let rig = Rig::new().call(1, Some(one_call(name, input.clone())), &[], OK_RESULT);
        let room = room_for(&["src/a.rs"], &repo());
        let run = rig.agent().run(&room, "b", &limits()).await.unwrap();
        assert!(
            !matches!(run.stop, AgentStop::Failed(_)),
            "{name} {input}: {:?}",
            run.stop
        );
    }
}

#[tokio::test]
async fn a_write_that_went_through_outside_the_out_folder_fails_the_piece() {
    for (name, input) in [
        (
            "Write",
            json!({"file_path": "@ROOM@/code/src/a.rs", "content": "x"}),
        ),
        ("Write", json!({"file_path": "src/a.rs", "content": "x"})),
        (
            "Edit",
            json!({"file_path": "@ROOM@/_camerata/checklist.md", "old_string": "a", "new_string": "b"}),
        ),
        (
            "MultiEdit",
            json!({"file_path": "@ROOM@/code/src/a.rs", "edits": []}),
        ),
        (
            "NotebookEdit",
            json!({"notebook_path": "@ROOM@/code/n.ipynb", "new_source": "x"}),
        ),
    ] {
        let rig = Rig::new().call(
            1,
            Some(replay(&[
                call("w1", name, input.clone()),
                answer("w1", "ok", None, false),
            ])),
            &[],
            OK_RESULT,
        );
        let room = room_for(&["src/a.rs"], &repo());
        let run = rig.agent().run(&room, "b", &limits()).await.unwrap();
        assert!(
            matches!(run.stop, AgentStop::Failed(_)),
            "{name} {input}: {:?}",
            run.stop
        );
    }
}

#[tokio::test]
async fn writes_into_the_out_folder_and_refused_writes_are_fine() {
    for (name, input, refused) in [
        (
            "Write",
            json!({"file_path": "@ROOM@/_camerata/out/problems-1.json", "content": "x"}),
            false,
        ),
        (
            "Write",
            json!({"file_path": "_camerata/out/x.json", "content": "x"}),
            false,
        ),
        (
            "Edit",
            json!({"file_path": "@ROOM@/code/../_camerata/out/y", "old_string": "a", "new_string": "b"}),
            false,
        ),
        (
            "Write",
            json!({"file_path": "@ROOM@/code/src/a.rs", "content": "x"}),
            true,
        ),
        (
            "Edit",
            json!({"file_path": "@ROOM@/_camerata/checklist.md", "old_string": "a", "new_string": "b"}),
            true,
        ),
    ] {
        let rig = Rig::new().call(
            1,
            Some(replay(&[
                call("w1", name, input.clone()),
                answer("w1", "x", None, refused),
            ])),
            &[],
            OK_RESULT,
        );
        let room = room_for(&["src/a.rs"], &repo());
        let run = rig.agent().run(&room, "b", &limits()).await.unwrap();
        assert!(
            !matches!(run.stop, AgentStop::Failed(_)),
            "{name} {input}: {:?}",
            run.stop
        );
    }
}

#[test]
fn the_cli_agent_does_not_meter_its_own_turns_so_the_runner_records_them() {
    let agent = CliPieceAgent::new(MODEL, PathBuf::from("/run/rooms"), EngagementKind::Internal);
    assert!(!agent.meters_its_own_turns());
}

#[tokio::test]
async fn each_call_leaves_its_stream_records_outside_the_room() {
    let rig = Rig::new()
        .call(1, Some(full_replay()), &[], OK_RESULT)
        .call(2, Some(full_replay()), &[], OK_RESULT);
    let traces = rig.path().join("finder-traces");
    let agent = rig.agent().with_trace_dir(traces.clone());
    let room = room_for(&["src/a.rs"], &repo());
    let first = agent.run(&room, "b", &limits()).await.unwrap();
    agent
        .follow_up(
            &room,
            &first,
            &super::super::FollowUp::text("again".to_string()),
            &limits(),
        )
        .await
        .unwrap();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&traces)
        .expect("the trace folder exists")
        .flatten()
        .map(|e| e.path())
        .collect();
    files.sort();
    assert_eq!(files.len(), 2, "one trace file per call: {files:?}");
    for f in &files {
        assert_eq!(f.extension().and_then(|e| e.to_str()), Some("jsonl"));
        let text = std::fs::read_to_string(f).unwrap();
        let records: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).expect("each line is one JSON record"))
            .collect();
        assert!(records.iter().any(|r| r["type"] == "assistant"), "{text}");
        assert!(records.iter().any(|r| r["type"] == "result"), "{text}");
    }
    let mut room_tree = BTreeMap::new();
    read_tree(&rig.path().join("work"), &mut room_tree);
    assert!(
        room_tree
            .keys()
            .all(|p| !p.to_string_lossy().contains("finder-traces")
                && p.extension().and_then(|e| e.to_str()) != Some("jsonl")),
        "nothing of the trace is under the room: {:?}",
        room_tree.keys()
    );
    assert!(agent
        .trace_note()
        .is_some_and(|n| n.contains("finder-traces")));
}

// ── Claude Code's own saved tool result ─────────────────────────────────────────────────────────

const SAVED_ROOM: &str = "/Users/someone/work/state/camerata/runs/r1/finder-rooms/room-3d4f-ab_c";
const SAVED_PROJECT: &str =
    "-Users-someone-work-state-camerata-runs-r1-finder-rooms-room-3d4f-ab-c";

fn saved_path(project: &str, session: &str, sub: &str) -> String {
    format!("/Users/someone/.claude/projects/{project}/{session}/{sub}/toolu_011jz.txt")
}

/// A stream where a Grep result was too large and Claude Code saved it to `saved`, then the model
/// ran `tool` with `input` on `named`.
fn saved_result_stream(saved: &str, tool: &str, input: Value) -> String {
    let lines = [
        json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "g1", "name": "Grep", "input": {"pattern": "x"}}]}}),
        json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "g1", "content": format!(
                "<persisted-output>\nOutput too large (25.2KB). Full output saved to: {saved}\n\nPreview (first 2KB):\n1:a\n</persisted-output>")}]}}),
        json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "r1", "name": tool, "input": input}]}}),
        json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "r1", "content": "15\tline"}]}}),
    ];
    lines
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_saved(stream: &str) -> super::ParsedSession {
    parse_session(stream, &[PathBuf::from(SAVED_ROOM)], &BTreeMap::new())
}

/// The saved file of the agent's own earlier call is not outside the room and covers nothing.
#[test]
fn a_read_of_the_saved_result_of_its_own_call_is_not_outside() {
    let saved = saved_path(SAVED_PROJECT, "df722648-ec6a", "tool-results");
    let parsed = parse_saved(&saved_result_stream(
        &saved,
        "Read",
        json!({"file_path": saved, "offset": 15}),
    ));
    assert!(parsed.outside.is_empty(), "{:?}", parsed.outside);
    assert!(
        !parsed
            .trace
            .iter()
            .any(|e| matches!(e, ToolEvent::Read { .. })),
        "{:?}",
        parsed.trace
    );
}

#[test]
fn a_saved_result_path_that_is_not_the_one_announced_stays_outside() {
    let saved = saved_path(SAVED_PROJECT, "df722648-ec6a", "tool-results");
    let cases = [
        saved_path(SAVED_PROJECT, "other-session", "tool-results"),
        saved_path(SAVED_PROJECT, "df722648-ec6a", "memory"),
        saved_path(
            "-Users-someone-work-state-camerata-runs-r1-finder-rooms-room-OTHER",
            "df722648-ec6a",
            "tool-results",
        ),
        format!("/Users/someone/.claude/projects/{SAVED_PROJECT}/df722648-ec6a/tool-results/../../x.txt"),
    ];
    for path in cases {
        let parsed = parse_saved(&saved_result_stream(
            &saved,
            "Read",
            json!({"file_path": path}),
        ));
        assert_eq!(parsed.outside, vec![path.clone()], "{path}");
    }
}

/// Only a Read gets the exception, and only when the announced path is in this room's project
/// folder (a result saved elsewhere is not trusted just because a result said so).
#[test]
fn only_a_read_of_the_saved_result_is_exempt() {
    let saved = saved_path(SAVED_PROJECT, "df722648-ec6a", "tool-results");
    for (tool, input) in [
        ("Grep", json!({"pattern": "x", "path": saved})),
        ("Glob", json!({"pattern": saved})),
        ("Write", json!({"file_path": saved, "content": "x"})),
    ] {
        let parsed = parse_saved(&saved_result_stream(&saved, tool, input));
        assert!(!parsed.outside.is_empty(), "{tool}");
    }
    let foreign = saved_path("-Users-someone-elsewhere", "df722648-ec6a", "tool-results");
    let parsed = parse_saved(&saved_result_stream(
        &foreign,
        "Read",
        json!({"file_path": foreign}),
    ));
    assert_eq!(parsed.outside, vec![foreign]);
}
