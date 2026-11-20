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
pub async fn write_prompt<W>(mut stdin: W, prompt: Vec<u8>) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    stdin.write_all(&prompt).await?;
    stdin.shutdown().await
}

/// A [`write_prompt`] running on its own task, so a child that answers before it has read
/// everything cannot block both sides on full pipes.
///
/// Dropping it aborts the task, so a call that is killed, times out or is dropped leaves no
/// writer behind. [`PromptWriter::finish`] reports how the write ended: a child that exits early
/// leaves a broken pipe, which is its exit status's to explain and is not an error here; any other
/// failure means the prompt did not reach the child whole, and the answer to a truncated prompt
/// must not be taken as the answer.
pub struct PromptWriter {
    handle: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
}

impl PromptWriter {
    /// Start writing `prompt` to `stdin` on a new task. Needs a running tokio runtime.
    pub fn start<W>(stdin: W, prompt: Vec<u8>) -> Self
    where
        W: tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        Self {
            handle: Some(tokio::spawn(write_prompt(stdin, prompt))),
        }
    }

    /// Wait for the write to end, for at most [`PROMPT_WRITE_GRACE`]. Call it once the child has
    /// exited: a writer still unfinished then is stuck (something else, such as a grandchild the
    /// child left running, holds the pipe open) and is aborted rather than waited on forever.
    ///
    /// `Ok` when it completed, the child closed its input early (broken pipe) or it was aborted
    /// here; `Err` for any other write failure.
    pub async fn finish(mut self) -> std::io::Result<()> {
        let Some(mut handle) = self.handle.take() else {
            return Ok(());
        };
        match tokio::time::timeout(PROMPT_WRITE_GRACE, &mut handle).await {
            Ok(Ok(Err(e))) if e.kind() != std::io::ErrorKind::BrokenPipe => Err(e),
            // Completed, broken pipe, or the task was cancelled (nothing was left to finish).
            Ok(_) => Ok(()),
            Err(_) => {
                handle.abort();
                Ok(())
            }
        }
    }
}

/// How long [`PromptWriter::finish`] waits for a writer that is still running after the child has
/// exited.
pub const PROMPT_WRITE_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

impl Drop for PromptWriter {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

/// The engine-owned folder the per-call system prompt files live in:
/// `<Camerata state dir>/tmp/system-prompts/` ([`crate::state_dir::dir`]), never `$TMPDIR`.
///
/// Each call's file is client data (the chat's carries project layers), so it must not sit in a
/// shared temp folder, and a quit that skips destructors (`process::exit`, SIGKILL, a crash) must
/// not leave it behind for good: [`remove_live_system_prompts`] runs from the shutdown handler and
/// [`sweep_leftover_system_prompts`] at server start.
pub fn system_prompt_root() -> std::path::PathBuf {
    crate::state_dir::dir().join("tmp").join("system-prompts")
}

/// The directories of the system prompt files whose calls are in flight in this process.
fn live_system_prompts() -> &'static std::sync::Mutex<std::collections::HashSet<std::path::PathBuf>>
{
    static LIVE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashSet<std::path::PathBuf>>,
    > = std::sync::OnceLock::new();
    LIVE.get_or_init(Default::default)
}

/// Create `dir` and its missing parents, private to the owner where the platform has modes.
fn create_private_dir_all(dir: &std::path::Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// A private copy of a system prompt on disk, for `claude --system-prompt-file`.
///
/// A system prompt can be as large as the prompt (the chat's carries the whole manuals), and
/// `--system-prompt <text>` puts it in the argument list, which the OS caps. The file is created
/// in its own private directory (mode 0700) under [`system_prompt_root`], is readable by its owner
/// alone (0600), is registered as live, and is removed with its directory when this value is
/// dropped: on success, error, timeout or kill, whichever way the call ends. A quit that runs no
/// destructors is covered by [`remove_live_system_prompts`] (shutdown) and
/// [`sweep_leftover_system_prompts`] (start). It is a file and not stdin because the flag REPLACES
/// the Claude Code scaffolding, which folding the text into the prompt would not do.
pub struct SystemPromptFile {
    dir: std::path::PathBuf,
    file: std::path::PathBuf,
}

impl SystemPromptFile {
    /// Write `text` to a new private file under [`system_prompt_root`].
    pub fn create(text: &str) -> std::io::Result<Self> {
        Self::create_in(&system_prompt_root(), text)
    }

    /// As [`Self::create`], under `root`.
    pub fn create_in(root: &std::path::Path, text: &str) -> std::io::Result<Self> {
        use std::io::Write;
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        create_private_dir_all(root)?;
        // `<pid>-<n>-<nanos>`: the leading pid is what the start-up sweep reads to tell a dead
        // process's leftovers from a live one's.
        let dir = root.join(format!(
            "{}-{}-{nanos}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        create_private_dir_all(&dir)?;
        let file = dir.join("system-prompt.txt");
        // Owned (and so registered and removed on drop) from here on, a failed write included.
        let this = Self { dir, file };
        if let Ok(mut live) = live_system_prompts().lock() {
            live.insert(this.dir.clone());
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options
            .open(&this.file)
            .and_then(|mut f| f.write_all(text.as_bytes()))?;
        Ok(this)
    }

    /// The file's path, for `--system-prompt-file`.
    pub fn path(&self) -> &std::path::Path {
        &self.file
    }
}

impl Drop for SystemPromptFile {
    fn drop(&mut self) {
        if let Ok(mut live) = live_system_prompts().lock() {
            live.remove(&self.dir);
        }
        let _ = std::fs::remove_file(&self.file);
        let _ = std::fs::remove_dir(&self.dir);
    }
}

/// Remove the system prompt files of every call in flight in this process. For the shutdown
/// handler, which ends the process with `process::exit` (no destructors run).
pub fn remove_live_system_prompts() {
    if let Ok(mut live) = live_system_prompts().lock() {
        for dir in live.drain() {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// Whether a process with this id exists (`kill -0`). Without a way to ask, it is assumed to.
fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stderr(std::process::Stdio::null())
            .status()
            .map_or(true, |s| s.success())
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// At start, remove the system prompt directories under `root` left by a process that is gone
/// (a SIGKILL, a crash, a power cut), and any of this process's own that are not in flight.
/// A directory of ANOTHER live process (the CLI running beside the server) is left alone. Returns
/// how many were removed.
pub fn sweep_leftover_system_prompts_in(root: &std::path::Path) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let own = std::process::id();
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let pid = entry
            .file_name()
            .to_str()
            .and_then(|n| n.split('-').next())
            .and_then(|p| p.parse::<u32>().ok());
        let stale = match pid {
            Some(pid) if pid == own => !live_system_prompts()
                .lock()
                .map_or(false, |live| live.contains(&path)),
            Some(pid) => !process_alive(pid),
            // Not a name this module made: not ours to judge.
            None => false,
        };
        if stale && std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// [`sweep_leftover_system_prompts_in`] over [`system_prompt_root`].
pub fn sweep_leftover_system_prompts() -> usize {
    sweep_leftover_system_prompts_in(&system_prompt_root())
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

    /// A writer that fails every write with `kind`.
    struct FailingWriter(std::io::ErrorKind);

    impl tokio::io::AsyncWrite for FailingWriter {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Ready(Err(std::io::Error::from(self.0)))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// A writer that never accepts a byte: a child that does not read its stdin.
    struct StalledWriter;

    impl tokio::io::AsyncWrite for StalledWriter {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Pending
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Pending
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Pending
        }
    }

    #[tokio::test]
    async fn a_write_that_fails_for_any_reason_but_a_broken_pipe_is_an_error() {
        let failed = PromptWriter::start(
            FailingWriter(std::io::ErrorKind::PermissionDenied),
            b"prompt".to_vec(),
        )
        .finish()
        .await;
        assert_eq!(
            failed.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }

    #[tokio::test]
    async fn a_broken_pipe_is_the_childs_exit_status_to_explain_not_a_write_error() {
        let result = PromptWriter::start(
            FailingWriter(std::io::ErrorKind::BrokenPipe),
            b"prompt".to_vec(),
        )
        .finish()
        .await;
        assert!(result.is_ok(), "{result:?}");
    }

    #[tokio::test]
    async fn a_complete_write_delivers_every_byte_and_closes_the_input() {
        let (mut reader, writer) = tokio::io::duplex(64);
        let sent: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let w = PromptWriter::start(writer, sent.clone());
        let mut got = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut got)
            .await
            .unwrap();
        assert!(w.finish().await.is_ok());
        assert_eq!(got, sent);
    }

    /// A call that is killed or dropped leaves no writer behind.
    #[tokio::test]
    async fn dropping_the_writer_aborts_a_write_that_is_stuck() {
        let writer = PromptWriter::start(StalledWriter, b"prompt".to_vec());
        let handle = writer.handle.as_ref().unwrap().abort_handle();
        drop(writer);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !handle.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the stuck writer task was aborted");
    }

    /// The live-directory registry is process-wide, and the shutdown cleanup empties it: the tests
    /// that create files hold this so one's cleanup never removes another's file mid-test.
    fn registry_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A fresh private root for one test, removed when it drops.
    struct Root(std::path::PathBuf);
    impl Root {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "camerata-api-types-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&p);
            Self(p)
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_system_prompt_file_is_private_holds_the_text_and_goes_when_dropped() {
        let _registry = registry_lock();
        use std::os::unix::fs::PermissionsExt;
        let root = Root::new("private");
        let text = "line one\nline two\n".repeat(100_000);
        let file = SystemPromptFile::create_in(&root.0, &text).unwrap();
        let path = file.path().to_path_buf();
        let dir = path.parent().unwrap().to_path_buf();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&root.0), 0o700);
        drop(file);
        assert!(!path.exists(), "the file is removed on drop");
        assert!(!dir.exists(), "so is its private directory");
    }

    #[test]
    fn the_root_is_the_engine_state_folder_and_not_the_temp_folder() {
        let root = system_prompt_root();
        assert!(root.ends_with("tmp/system-prompts"), "{root:?}");
        assert!(root.starts_with(crate::state_dir::dir()), "{root:?}");
    }

    /// A process that is gone leaves its directory behind; the next start removes it, and leaves
    /// a live process's and this one's in-flight directories alone.
    #[cfg(unix)]
    #[test]
    fn a_leftover_directory_of_a_dead_process_is_removed_at_start() {
        let _registry = registry_lock();
        let root = Root::new("sweep");
        std::fs::create_dir_all(&root.0).unwrap();
        // A pid that is certainly gone: a child that has exited and been reaped.
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        let leftover = root.0.join(format!("{dead}-0-1"));
        std::fs::create_dir_all(&leftover).unwrap();
        std::fs::write(leftover.join("system-prompt.txt"), "client data").unwrap();
        // Another live process's (the parent shell of the test run is alive).
        let live_other = root
            .0
            .join(format!("{}-0-1", std::os::unix::process::parent_id()));
        std::fs::create_dir_all(&live_other).unwrap();
        // This process, in flight.
        let in_flight = SystemPromptFile::create_in(&root.0, "now").unwrap();
        // This process, but no longer in flight (a previous life with the same pid).
        let own_stale = root.0.join(format!("{}-999-1", std::process::id()));
        std::fs::create_dir_all(&own_stale).unwrap();
        // Not one of ours.
        let foreign = root.0.join("notes");
        std::fs::create_dir_all(&foreign).unwrap();

        let removed = sweep_leftover_system_prompts_in(&root.0);
        assert_eq!(removed, 2);
        assert!(!leftover.exists(), "the dead process's directory is gone");
        assert!(!own_stale.exists());
        assert!(live_other.exists(), "a live process's is left alone");
        assert!(
            in_flight.path().exists(),
            "an in-flight call's is left alone"
        );
        assert!(
            foreign.exists(),
            "a name this module did not make is left alone"
        );
    }

    /// The shutdown handler ends the process without destructors: it removes what is in flight.
    #[test]
    fn the_shutdown_cleanup_removes_a_live_registered_directory() {
        let _registry = registry_lock();
        let root = Root::new("shutdown");
        let file = SystemPromptFile::create_in(&root.0, "client data").unwrap();
        let path = file.path().to_path_buf();
        assert!(path.exists());
        remove_live_system_prompts();
        assert!(!path.exists(), "removed by the shutdown cleanup");
        drop(file); // a later drop is a harmless no-op
    }
}
