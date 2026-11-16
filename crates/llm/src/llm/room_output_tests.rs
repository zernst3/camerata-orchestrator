//! The clean-room write grant: a call that reviews a room folder may also write into that room's
//! `_camerata/out/` and nowhere else. Tests read the argument list and the environment the
//! command is built with, so no `claude` program is needed.

use super::*;

fn args_of(cmd: &tokio::process::Command) -> Vec<String> {
    cmd.as_std()
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

fn env_of(cmd: &tokio::process::Command, key: &str) -> Option<String> {
    cmd.as_std()
        .get_envs()
        .find(|(k, _)| *k == key)
        .and_then(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
}

const ROOM: &str = "/Users/auditor/state/rooms/room-1";

fn read_only() -> LlmRequest {
    LlmRequest::new("hi").with_repo_read(ROOM)
}

fn room_call() -> LlmRequest {
    read_only().with_room_output(ROOM, None)
}

fn build(req: &LlmRequest) -> tokio::process::Command {
    build_cli_command(req, "claude-fable-5-2", "sid")
}

#[test]
fn without_the_option_the_argument_list_is_what_it_always_was() {
    let args = args_of(&build(&read_only()));
    let expected: Vec<String> = [
        "-p",
        "hi",
        "--model",
        "claude-fable-5-2",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--session-id",
        "sid",
        "--setting-sources",
        "user",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--permission-mode",
        "dontAsk",
        "--allowedTools",
        "Read Glob Grep LS",
        "--add-dir",
        ROOM,
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    assert_eq!(args, expected);
    assert_eq!(
        env_of(&build(&read_only()), "CLAUDE_CODE_DISABLE_CLAUDE_MDS"),
        None
    );
    assert_eq!(
        build(&read_only()).as_std().get_program(),
        std::ffi::OsStr::new("claude")
    );
}

#[test]
fn with_the_option_write_is_allowed_for_the_out_folder_of_the_room_only() {
    let args = args_of(&build(&room_call()));
    eprintln!("ARGV {}", args.join(" \\\n     "));
    let rule = format!("Edit/{ROOM}/_camerata/out/**").replacen("Edit/", "Edit(/", 1) + ")";
    assert_eq!(
        rule,
        "Edit(//Users/auditor/state/rooms/room-1/_camerata/out/**)"
    );
    assert!(args.contains(&rule), "{args:?}");
    // The rule is a separate value of the allow list, so a room path with a space stays whole.
    let at = args.iter().position(|a| a == "--allowedTools").unwrap();
    // Read, Glob, Grep and LS are each scoped to the room; the write rule follows them.
    let reads: Vec<String> = ["Read", "Glob", "Grep", "LS"]
        .iter()
        .map(|t| format!("{t}(//Users/auditor/state/rooms/room-1/**)"))
        .collect();
    assert_eq!(args[at + 1..at + 5], reads[..]);
    assert_eq!(args[at + 5], rule);
    // No grant wider than the folder: neither a bare Write or Edit nor a pattern above it.
    for a in &args {
        assert_ne!(a, "Write");
        assert_ne!(a, "Edit");
        assert!(!a.starts_with("Write("), "{a}");
        if a.starts_with("Edit(") {
            assert!(a.contains("/_camerata/out/"), "{a}");
        }
    }
}

#[test]
fn only_the_read_tools_and_the_scoped_write_are_ever_allowed() {
    let args = args_of(&build(&room_call()));
    let at = args.iter().position(|a| a == "--allowedTools").unwrap();
    let allowed: Vec<&String> = args[at + 1..]
        .iter()
        .take_while(|a| !a.starts_with("--"))
        .collect();
    assert_eq!(allowed.len(), 5, "{allowed:?}");
    for banned in [
        "Bash",
        "WebFetch",
        "WebSearch",
        "Task",
        "Agent",
        "NotebookEdit",
    ] {
        assert!(
            !allowed
                .iter()
                .any(|a| a.split_whitespace().any(|w| w == banned)),
            "{banned} is allowed: {allowed:?}"
        );
    }
}

#[test]
fn shell_web_task_and_agent_tools_are_denied_outright() {
    let args = args_of(&build(&room_call()));
    let d = args.iter().position(|a| a == "--disallowedTools").unwrap();
    let denied: Vec<&String> = args[d + 1..]
        .iter()
        .take_while(|a| !a.starts_with("--"))
        .collect();
    for tool in ["Bash", "WebFetch", "WebSearch", "Task", "Agent"] {
        assert!(
            denied.iter().any(|a| *a == tool),
            "{tool} not denied: {denied:?}"
        );
    }
    // Denying the bare name Edit would deny the Write tool as well; it is never in the list.
    assert!(
        !denied.iter().any(|a| *a == "Edit" || *a == "Write"),
        "{denied:?}"
    );
}

#[test]
fn the_room_call_runs_without_any_instruction_file() {
    assert_eq!(
        env_of(&build(&room_call()), "CLAUDE_CODE_DISABLE_CLAUDE_MDS").as_deref(),
        Some("1")
    );
    assert_eq!(
        env_of(&build(&room_call()), "CLAUDE_CODE_DISABLE_AUTO_MEMORY").as_deref(),
        Some("1")
    );
}

#[test]
fn the_program_can_be_named_and_defaults_to_claude_on_the_path() {
    let named = LlmRequest::new("hi")
        .with_repo_read(ROOM)
        .with_room_output(ROOM, Some("/Users/auditor/app/claude".into()));
    assert_eq!(
        build(&named).as_std().get_program(),
        std::ffi::OsStr::new("/Users/auditor/app/claude")
    );
    assert_eq!(
        build(&room_call()).as_std().get_program(),
        std::ffi::OsStr::new("claude")
    );
}

#[test]
fn a_request_with_no_repo_read_folder_gets_no_write() {
    let args = args_of(&build(&LlmRequest::new("hi").with_room_output(ROOM, None)));
    assert!(!args.iter().any(|a| a.starts_with("Edit(")), "{args:?}");
    assert!(args.windows(2).any(|w| w == ["--tools", ""]));
}

#[test]
fn a_room_path_with_pattern_characters_matches_only_itself() {
    let rule = room_output_rule(std::path::Path::new("/Users/auditor/a[1]/b*/room"));
    assert_eq!(
        rule,
        r"Edit(//Users/auditor/a\[1\]/b\*/room/_camerata/out/**)"
    );
}

#[test]
fn the_api_backend_ignores_the_option() {
    let plain = read_only().with_model("claude-fable-5-2");
    let room = room_call().with_model("claude-fable-5-2");
    let basis = PriceBasis::realtime();
    assert_eq!(
        messages_request_body(&plain, "claude-fable-5-2", basis.cache_ttl),
        messages_request_body(&room, "claude-fable-5-2", basis.cache_ttl)
    );
}

fn allowed_of(args: &[String]) -> Vec<&String> {
    let at = args.iter().position(|a| a == "--allowedTools").unwrap();
    args[at + 1..]
        .iter()
        .take_while(|a| !a.starts_with("--"))
        .collect()
}

#[test]
fn a_room_call_loads_project_settings_only_never_the_users() {
    let args = args_of(&build(&room_call()));
    let at = args.iter().position(|a| a == "--setting-sources").unwrap();
    assert_eq!(args[at + 1], "project", "{args:?}");
    assert_eq!(
        args.iter().filter(|a| *a == "--setting-sources").count(),
        1,
        "{args:?}"
    );
    assert!(!args.iter().any(|a| a == "user"), "{args:?}");
    for flag in ["--strict-mcp-config", "--disable-slash-commands"] {
        assert!(args.iter().any(|a| a == flag), "{flag} missing: {args:?}");
    }
    // The other calls keep the user's settings (the login) as before.
    let plain = args_of(&build(&read_only()));
    let at = plain.iter().position(|a| a == "--setting-sources").unwrap();
    assert_eq!(plain[at + 1], "user");
}

#[test]
fn a_room_call_may_read_only_inside_the_room() {
    let args = args_of(&build(&room_call()));
    let allowed = allowed_of(&args);
    for tool in ["Read", "Glob", "Grep", "LS"] {
        let want = format!("{tool}(//Users/auditor/state/rooms/room-1/**)");
        assert!(allowed.iter().any(|a| **a == want), "{want} in {allowed:?}");
        assert!(
            !allowed.iter().any(|a| **a == tool),
            "bare {tool} allowed: {allowed:?}"
        );
    }
    assert!(
        !allowed.iter().any(|a| a.contains(' ')),
        "no multi-tool value: {allowed:?}"
    );
}

#[test]
fn the_read_rules_escape_pattern_characters_of_the_room_path() {
    let rule = room_read_rule("Grep", std::path::Path::new("/Users/auditor/a[1]/b*/room"));
    assert_eq!(rule, r"Grep(//Users/auditor/a\[1\]/b\*/room/**)");
}

#[test]
fn a_room_call_carries_its_turn_limit_and_keeps_no_session_file() {
    let args = args_of(&build(&room_call().with_room_max_turns(77)));
    let at = args
        .iter()
        .position(|a| a == "--max-turns")
        .expect("--max-turns");
    assert_eq!(args[at + 1], "77");
    assert!(
        args.iter().any(|a| a == "--no-session-persistence"),
        "{args:?}"
    );
    // Without a limit set there is no flag; a plain call never gets either flag.
    assert!(!args_of(&build(&room_call()))
        .iter()
        .any(|a| a == "--max-turns"));
    let plain = args_of(&build(&read_only()));
    assert!(!plain
        .iter()
        .any(|a| a == "--max-turns" || a == "--no-session-persistence"));
}

#[test]
fn a_room_call_offers_exactly_the_room_tools() {
    let args = args_of(&build(&room_call()));
    let at = args
        .iter()
        .position(|a| a == "--tools")
        .expect("--tools on a room call");
    assert_eq!(args[at + 1], "Read,Glob,Grep,LS,Write,Edit", "{args:?}");
    assert_eq!(args.iter().filter(|a| *a == "--tools").count(), 1);
}

#[test]
fn an_extra_folder_never_widens_a_room_call() {
    let mut req = room_call();
    req.repo_read_extra_dirs = vec![std::path::PathBuf::from("/Users/auditor/other-repo")];
    let args = args_of(&build(&req));
    let dirs: Vec<&String> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| *a == "--add-dir")
        .map(|(i, _)| &args[i + 1])
        .collect();
    assert_eq!(dirs, vec![ROOM], "{args:?}");
    // A call that is not a room call keeps its extra folders.
    let mut plain = read_only();
    plain.repo_read_extra_dirs = vec![std::path::PathBuf::from("/Users/auditor/other-repo")];
    let plain_args = args_of(&build(&plain));
    assert_eq!(
        plain_args.iter().filter(|a| *a == "--add-dir").count(),
        2,
        "{plain_args:?}"
    );
}
