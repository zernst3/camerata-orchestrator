//! Every `claude -p` call the engine makes runs in isolation from the launcher: a call that
//! reads no repo runs in an empty, engine-owned working directory (never the directory the run
//! was launched from, which may hold agent instructions and the operator's own notes), and every
//! call, repo-read or not, tells the CLI not to load project settings or the operator's memory.

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

fn build(req: &LlmRequest) -> tokio::process::Command {
    build_cli_command(req, "claude-haiku-5-5", "sid")
}

#[test]
fn a_call_without_a_repo_runs_in_an_empty_engine_owned_directory() {
    let cmd = build(&LlmRequest::new("hello"));
    let dir = cmd
        .as_std()
        .get_current_dir()
        .expect("a no-repo call sets its own working directory")
        .to_path_buf();
    assert!(dir.is_dir(), "{dir:?} must exist when the call spawns");
    assert_eq!(
        std::fs::read_dir(&dir).expect("readable").count(),
        0,
        "{dir:?} must be empty so no instruction file can be found in it"
    );
    assert!(
        dir.starts_with(std::fs::canonicalize(std::env::temp_dir()).expect("temp dir")),
        "{dir:?} must sit under the system temp dir, not a repo or the launch directory"
    );
    assert!(
        dir.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("camerata-claude-cwd")),
        "{dir:?} must be named as engine-owned"
    );
}

#[test]
fn a_no_repo_call_never_uses_the_launch_directory() {
    let launch = std::env::current_dir().expect("cwd");
    let cmd = build(&LlmRequest::new("hello"));
    let dir = cmd.as_std().get_current_dir().expect("cwd set");
    assert_ne!(dir, launch.as_path());
    assert!(!launch.starts_with(dir) && !dir.starts_with(&launch));
}

#[test]
fn a_repo_read_call_keeps_its_repo_directory() {
    let req = LlmRequest::new("hello").with_repo_read("/Users/auditor/clients/acme");
    let cmd = build(&req);
    assert_eq!(
        cmd.as_std().get_current_dir(),
        Some(std::path::Path::new("/Users/auditor/clients/acme"))
    );
}

#[test]
fn every_call_loads_only_user_settings() {
    for req in [
        LlmRequest::new("a"),
        LlmRequest::new("b").with_repo_read("/Users/auditor/clients/acme"),
    ] {
        let args = args_of(&build(&req));
        let at = args
            .iter()
            .position(|a| a == "--setting-sources")
            .unwrap_or_else(|| panic!("--setting-sources missing: {args:?}"));
        assert_eq!(args[at + 1], "user", "project and local settings stay out");
    }
}

#[test]
fn every_call_switches_the_operators_memory_off() {
    for req in [
        LlmRequest::new("a"),
        LlmRequest::new("b").with_repo_read("/Users/auditor/clients/acme"),
    ] {
        let cmd = build(&req);
        assert_eq!(
            env_of(&cmd, "CLAUDE_CODE_DISABLE_AUTO_MEMORY").as_deref(),
            Some("1")
        );
    }
}

#[test]
fn a_no_repo_call_also_switches_instruction_file_discovery_off() {
    let cmd = build(&LlmRequest::new("a"));
    assert_eq!(
        env_of(&cmd, "CLAUDE_CODE_DISABLE_CLAUDE_MDS").as_deref(),
        Some("1")
    );
}

/// The recorded session folder follows the real working directory of the spawn.
#[test]
fn the_recorded_session_directory_is_the_spawn_directory() {
    let req = LlmRequest::new("hello");
    let cmd = build(&req);
    assert_eq!(
        Some(spawn_dir(&req).as_path()),
        cmd.as_std().get_current_dir()
    );
    let repo = LlmRequest::new("hello").with_repo_read("/Users/auditor/clients/acme");
    assert_eq!(
        spawn_dir(&repo),
        std::path::PathBuf::from("/Users/auditor/clients/acme")
    );
}
