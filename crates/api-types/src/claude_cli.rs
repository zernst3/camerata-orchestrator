//! The one place that decides what a `claude` subprocess inherits from the parent process.
//!
//! Camerata drives the Claude Code CLI (`claude -p ...`) from several crates: the completion
//! seam (`camerata-llm`), the governed agent runtime (`camerata-agent`) and the intake
//! engine / reviewer (`camerata-intake`). That path is the SUBSCRIPTION backend: it must never
//! silently bill the Anthropic API because an `ANTHROPIC_API_KEY` from a `.env` file was
//! inherited into the child. With the variable removed, `claude` can only use its own login.
//!
//! Every non-test spawn of the `claude` binary calls [`scrub_anthropic_key_env`] (a source-scan
//! test in `camerata-server` fails if one does not). The variable's name is private to this
//! module so a caller cannot reach for it; removing it from a child's environment is not a read
//! of the credential.

/// The env var the `claude` CLI would pick up as an API key. Private on purpose.
const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// Remove the Anthropic API key from a `claude` subprocess's environment. Takes the std command
/// so it works for both `std::process::Command` and, via `as_std_mut()`,
/// `tokio::process::Command`, without this leaf crate depending on tokio.
pub fn scrub_anthropic_key_env(cmd: &mut std::process::Command) {
    cmd.env_remove(ANTHROPIC_API_KEY_ENV);
}

/// An empty directory the engine owns, for `claude -p` calls that read no repo.
///
/// Claude Code loads the agent instruction files and the operator's own auto-memory that belong
/// to its working directory. A call that does not set one inherits the launcher's, so a run
/// started from a repo checkout put that repo's instructions and the operator's private notes
/// (about 9,000 tokens) into every audit prompt. This directory is created once per process
/// under the system temp dir, holds nothing, and is never a repo.
///
/// If it cannot be created the path is returned anyway: the spawn then fails on the missing
/// directory, so a call never falls back to the launcher's directory.
pub fn engine_empty_cwd() -> std::path::PathBuf {
    static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("camerata-claude-cwd-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        // The resolved path: Claude Code names the session folder after the real directory, and
        // the recorded session must point at the folder it wrote (macOS temp is a symlink).
        std::fs::canonicalize(&dir).unwrap_or(dir)
    })
    .clone()
}

/// Keep the launcher's and the operator's instructions out of a `claude` call.
///
/// - `--setting-sources user`: project and local settings files are not loaded (the user's own
///   settings still are, which keeps the CLI's login working).
/// - `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`: the operator's per-project auto-memory is not loaded.
/// - When the call reads no repo (`repo_dir` is `None`) it also runs in [`engine_empty_cwd`] and
///   sets `CLAUDE_CODE_DISABLE_CLAUDE_MDS=1`, so no instruction file is discovered at all.
///   A call that reads a repo keeps that repo as its working directory.
///
/// Takes the std command so it works for `tokio::process::Command` via `as_std_mut()`.
pub fn isolate_claude_call(cmd: &mut std::process::Command, repo_dir: Option<&std::path::Path>) {
    cmd.arg("--setting-sources")
        .arg("user")
        .env("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1");
    match repo_dir {
        Some(dir) => {
            cmd.current_dir(dir);
        }
        None => {
            cmd.current_dir(engine_empty_cwd())
                .env("CLAUDE_CODE_DISABLE_CLAUDE_MDS", "1");
        }
    }
}

/// Isolation for a clean-room review call (the agent finder's command line transport).
///
/// Like [`isolate_claude_call`], but the settings that load are the PROJECT's only
/// (`--setting-sources project`), never the operator's own: the operator's user settings carry
/// broad allow rules (bare Read, Edit, Write, Bash, WebFetch) and extra directories that would
/// let the reviewer read outside its room. The working directory is `room_dir`, the room's root;
/// a client's own `.claude/` and `.mcp.json` sit in a subfolder of it, so they are reviewed like
/// any other file and never load as project settings or hooks. No instruction file and no
/// auto-memory are read.
pub fn isolate_room_call(cmd: &mut std::process::Command, room_dir: &std::path::Path) {
    cmd.arg("--setting-sources")
        .arg("project")
        .env("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1")
        .env("CLAUDE_CODE_DISABLE_CLAUDE_MDS", "1")
        .current_dir(room_dir);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_is_explicitly_removed_from_the_child_environment() {
        let mut cmd = std::process::Command::new("claude");
        scrub_anthropic_key_env(&mut cmd);
        assert!(cmd
            .get_envs()
            .any(|(k, v)| k == "ANTHROPIC_API_KEY" && v.is_none()));
    }
}
