//! The command line transport, against a stand-in program that replays a recorded trace. No
//! model is called: the stand-in writes canned files into the room and a canned session file,
//! then prints a canned result line.

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
use super::CliPieceAgent;

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

/// The standard recorded piece: every kind of call, a denied write and a read outside the room.
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
        call(
            "t8",
            "Read",
            json!({"file_path": "/Users/auditor/other.rs"}),
        ),
        answer("t8", "no such file", None, true),
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
        for d in ["log", "projects", "work"] {
            std::fs::create_dir_all(rig.path().join(d)).unwrap();
        }
        rig
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Call number `n` (1-based) replays `trace`, drops `outputs` into the room's out folder and
    /// prints `result`.
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
if [ -f "{d}/replay-$n.jsonl" ]; then
  mkdir -p "{d}/projects/-room"
  sed "s#@ROOM@#$(pwd -P)#g" "{d}/replay-$n.jsonl" > "{d}/projects/-room/$sid.jsonl"
fi
if [ -d "{d}/out-$n" ]; then
  mkdir -p _camerata/out
  cp -R "{d}/out-$n/." _camerata/out/
fi
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
        CliPieceAgent::new(MODEL, self.path().join("work"))
            .with_claude_program(self.program())
            .with_claude_projects_dir(self.path().join("projects"))
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
            ToolEvent::Read {
                path: "/Users/auditor/other.rs".into(),
                first_line: 1,
                line_count: 2000,
                lines_returned: 0
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
async fn a_missing_session_file_fails_the_piece_and_coverage_is_never_assumed() {
    // The stand-in writes the output files and prints a good result, but no session file.
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
        AgentStop::Failed(why) => assert!(why.contains("session"), "{why}"),
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
    let agent = CliPieceAgent::new(MODEL, rig.path().join("work"))
        .with_claude_program(rig.path().join("no-such-program"))
        .with_claude_projects_dir(rig.path().join("projects"));
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
    let mut allowed: Vec<String> = room.paths();
    allowed.sort();
    assert_eq!(listed, allowed, "nothing but the room's own files");
    assert!(
        !listed.iter().any(|p| p == "src/b.rs" || p == "README.md"),
        "{listed:?}"
    );
}

#[tokio::test]
async fn the_session_runs_in_isolation_with_the_room_as_its_folder() {
    let rig = Rig::new().call(1, Some(full_replay()), &[], OK_RESULT);
    let room = room_for(&["src/a.rs"], &repo());
    let run = rig.agent().run(&room, "b", &limits()).await.unwrap();
    let args = rig.log("args-1.txt");
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
    assert!(listed.contains("./src/b.rs"), "{listed}");
    assert!(!listed.contains("./src/a.rs"), "{listed}");
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
