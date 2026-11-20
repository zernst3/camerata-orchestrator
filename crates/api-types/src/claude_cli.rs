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
/// `tokio::process::Command`.
pub fn scrub_anthropic_key_env(cmd: &mut std::process::Command) {
    cmd.env_remove(ANTHROPIC_API_KEY_ENV);
}

/// The env var that names the `claude` binary every cli call launches (an absolute path).
pub const CLAUDE_PROGRAM_ENV: &str = "CAMERATA_CLAUDE_PROGRAM";

/// The `claude` program every cli call launches: `CAMERATA_CLAUDE_PROGRAM` (surrounding
/// whitespace trimmed) when set and not blank, else `claude` found on PATH. The one resolver; no
/// spawn names the binary itself. It does not judge the value: [`validated_claude_program`] does,
/// and the cli pre-flight calls that before any model call.
pub fn claude_program() -> std::ffi::OsString {
    program_from(std::env::var_os(CLAUDE_PROGRAM_ENV))
}

/// [`claude_program`], refused when `CAMERATA_CLAUDE_PROGRAM` is set to something that is not an
/// absolute path (a relative name would be looked up from whatever directory the run starts in).
pub fn validated_claude_program() -> Result<std::ffi::OsString, String> {
    validated_from(std::env::var_os(CLAUDE_PROGRAM_ENV))
}

fn program_from(env: Option<std::ffi::OsString>) -> std::ffi::OsString {
    match env {
        Some(v) => match v.to_str().map(str::trim) {
            Some("") => std::ffi::OsString::from("claude"),
            Some(trimmed) => std::ffi::OsString::from(trimmed),
            // Not valid UTF-8: kept as it is, there is nothing safe to trim.
            None => v,
        },
        None => std::ffi::OsString::from("claude"),
    }
}

fn validated_from(env: Option<std::ffi::OsString>) -> Result<std::ffi::OsString, String> {
    let set = env
        .as_ref()
        .is_some_and(|v| !v.to_string_lossy().trim().is_empty());
    let program = program_from(env);
    if set && !std::path::Path::new(&program).is_absolute() {
        return Err(format!(
            "{CLAUDE_PROGRAM_ENV} is set to `{}`, which is not an absolute path. Set it to the \
             full path of the `claude` binary, such as /usr/local/bin/claude, or unset it to use \
             `claude` from PATH.",
            program.to_string_lossy()
        ));
    }
    Ok(program)
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

/// Hand a prompt to a `claude` child on its standard input, then close the input so the CLI
/// sees the end of the prompt.
///
/// `claude -p` reads its prompt from stdin when none is given as an argument. The prompt must
/// not ride in the argument list: the OS caps the arguments and environment of one process (about
/// 1 MB on macOS, `ARG_MAX`), and a prompt that carries whole documents is larger, so the spawn
/// fails with "Argument list too long (os error 7)". Stdin has no such cap.
///
/// Run it on its own task (`tokio::spawn`) while the caller reads the child's output: a child
/// that starts answering before it has read everything would otherwise block both sides on full
/// pipes. A child that exits early leaves a broken pipe; that is not this function's error to
/// report (the exit status and stderr already say why), so callers drop the result.
pub async fn write_prompt<W>(mut stdin: W, prompt: Vec<u8>) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    stdin.write_all(&prompt).await?;
    stdin.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_program_is_the_env_value_when_set_else_claude_on_path() {
        assert_eq!(
            program_from(Some("/opt/claude/bin/claude".into())),
            std::ffi::OsString::from("/opt/claude/bin/claude")
        );
        assert_eq!(program_from(None), std::ffi::OsString::from("claude"));
        assert_eq!(
            program_from(Some("  ".into())),
            std::ffi::OsString::from("claude")
        );
        assert_eq!(CLAUDE_PROGRAM_ENV, "CAMERATA_CLAUDE_PROGRAM");
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_from_the_program() {
        assert_eq!(
            program_from(Some("  /opt/claude/bin/claude\n".into())),
            std::ffi::OsString::from("/opt/claude/bin/claude")
        );
    }

    #[test]
    fn a_relative_program_is_refused_with_a_clear_message() {
        let err = validated_from(Some("bin/claude".into())).unwrap_err();
        assert!(err.contains("CAMERATA_CLAUDE_PROGRAM"), "{err}");
        assert!(err.contains("`bin/claude`"), "{err}");
        assert!(err.contains("absolute path"), "{err}");
        assert!(
            !err.contains('\u{2014}') && !err.contains('\u{2013}'),
            "{err}"
        );
        // A bare name is relative too.
        assert!(validated_from(Some("claude".into())).is_err());
        // Absolute, unset and blank pass; the absolute one is trimmed.
        assert_eq!(
            validated_from(Some(" /opt/claude ".into())).unwrap(),
            std::ffi::OsString::from("/opt/claude")
        );
        assert_eq!(
            validated_from(None).unwrap(),
            std::ffi::OsString::from("claude")
        );
        assert_eq!(
            validated_from(Some("   ".into())).unwrap(),
            std::ffi::OsString::from("claude")
        );
    }

    #[test]
    fn the_key_is_explicitly_removed_from_the_child_environment() {
        let mut cmd = std::process::Command::new("claude");
        scrub_anthropic_key_env(&mut cmd);
        assert!(cmd
            .get_envs()
            .any(|(k, v)| k == "ANTHROPIC_API_KEY" && v.is_none()));
    }
}
