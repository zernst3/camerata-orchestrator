//! The stable key reader client (issue #574).
//!
//! macOS ties a Keychain "Always Allow" to the exact hash of the binary that asks, and the
//! engine is rebuilt constantly, so every build asked again. The Anthropic key is therefore
//! read by a tiny separate program, `camerata-key-reader` (`crates/key-reader`), whose hash
//! changes only when its own code does. The engine runs it, reads the key from its stdout
//! PIPE, and never passes the key through an env var, an argument, a file or a log line.
//!
//! Outcomes: the helper gave a key ([`ReaderOutcome::Key`]); the entry does not exist
//! ([`ReaderOutcome::NotFound`], exit 2: same as no key configured); no helper exists, so the caller
//! uses the direct keychain read it always had ([`ReaderOutcome::Missing`]); or the helper
//! ran and failed, was denied, or hung ([`ReaderOutcome::Failed`]), which is a closed failure:
//! the direct read is NOT tried (it would hit the same locked keychain and could hang on a
//! dialog), the caller sees a keychain error, and with no env fallback the existing "no API
//! key is configured" message results. The child gets a hard timeout and is killed on it.

#[cfg(target_os = "macos")]
use std::io::Read;
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
#[cfg(target_os = "macos")]
use std::sync::mpsc;
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::Instant;

use camerata_api_types::credentials::{CredentialError, ANTHROPIC_API_KEY};

/// File name of the helper binary.
pub const READER_BIN: &str = "camerata-key-reader";
/// How long the helper may run before it is killed and the read fails closed. The one-time
/// Always Allow dialog cannot be answered within this, so the owner approves the installed
/// reader once by running it directly (see docs/TECHNICAL.md); after that it returns at once.
pub const READER_TIMEOUT: Duration = Duration::from_secs(5);
/// The helper's stdout is read up to this many bytes, no more.
#[cfg(target_os = "macos")]
const MAX_KEY_BYTES: u64 = 8192;

/// Where to look for the helper and how long to let it run.
#[derive(Clone, Debug)]
pub struct KeyReaderConfig {
    /// Tried in order; the first that exists is run.
    pub candidates: Vec<PathBuf>,
    pub timeout: Duration,
    /// Set once a run failed: the failure is remembered for the life of the process.
    failed: std::sync::Arc<std::sync::Mutex<Option<&'static str>>>,
}

impl KeyReaderConfig {
    /// Production lookup: the documented stable install path
    /// `$HOME/.camerata/bin/camerata-key-reader` first, then next to the running binary.
    pub fn production() -> Self {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(PathBuf::from));
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from);
        Self::production_with(exe_dir, home)
    }

    pub fn production_with(exe_dir: Option<PathBuf>, home: Option<PathBuf>) -> Self {
        // The stable install path first: one copy, one hash, one approval for every build and
        // worktree. A copy beside the binary is only a fallback (each worktree's own copy can
        // have its own hash and so its own prompt).
        let mut candidates = Vec::new();
        if let Some(home) = home {
            candidates.push(home.join(".camerata").join("bin").join(READER_BIN));
        }
        if let Some(dir) = exe_dir {
            candidates.push(dir.join(READER_BIN));
        }
        Self::new(candidates, READER_TIMEOUT)
    }

    pub fn new(candidates: Vec<PathBuf>, timeout: Duration) -> Self {
        Self {
            candidates,
            timeout,
            failed: Default::default(),
        }
    }
}

/// What running the helper came to. `Key` holds the key itself and is redacted in `Debug`.
#[derive(PartialEq, Eq)]
pub enum ReaderOutcome {
    Key(String),
    /// No usable helper on disk (or not macOS): use the direct read.
    Missing,
    /// The helper ran and said there is no such entry (exit 2): same as "no key configured".
    NotFound,
    /// The helper ran and failed, printed nothing, or hung: fail closed. The text is a fixed
    /// description, never helper output.
    Failed(&'static str),
}

impl std::fmt::Debug for ReaderOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Key(_) => f.write_str("Key(<redacted>)"),
            Self::Missing => f.write_str("Missing"),
            Self::NotFound => f.write_str("NotFound"),
            Self::Failed(w) => write!(f, "Failed({w:?})"),
        }
    }
}

/// The helper's exit status for "no such keychain entry" (see `crates/key-reader`).
#[cfg(target_os = "macos")]
const EXIT_NOT_FOUND: i32 = 2;

/// The current user's uid, from `/usr/bin/id -u` (std has no uid call and `unsafe` is forbidden).
#[cfg(target_os = "macos")]
fn current_uid() -> Option<u32> {
    static UID: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *UID.get_or_init(|| {
        let out = Command::new("/usr/bin/id").arg("-u").output().ok()?;
        String::from_utf8(out.stdout).ok()?.trim().parse().ok()
    })
}

/// A reader is run only by absolute path, only when it is a regular file owned by the current
/// user and writable by nobody else (no group or world write bit).
#[cfg(target_os = "macos")]
fn safe_to_run(path: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    if !path.is_absolute() {
        return false;
    }
    match (std::fs::metadata(path), current_uid()) {
        (Ok(m), Some(uid)) => m.is_file() && m.uid() == uid && m.mode() & 0o022 == 0,
        _ => false,
    }
}

/// Run the helper and read its stdout pipe. `log` receives fixed diagnostic lines only.
/// Off macOS this is always `Missing` (Linux keeps its Secret Service direct read).
#[cfg(not(target_os = "macos"))]
pub fn run_reader(_cfg: &KeyReaderConfig, _log: &dyn Fn(&str)) -> ReaderOutcome {
    ReaderOutcome::Missing
}

#[cfg(target_os = "macos")]
pub fn run_reader(cfg: &KeyReaderConfig, log: &dyn Fn(&str)) -> ReaderOutcome {
    // A failed run is remembered for the life of the process: no re-spawn, no second wait,
    // and no second dialog.
    let memo = || cfg.failed.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(why) = *memo() {
        return ReaderOutcome::Failed(why);
    }
    let outcome = run_reader_once(cfg, log);
    if let ReaderOutcome::Failed(why) = &outcome {
        *memo() = Some(why);
    }
    outcome
}

#[cfg(target_os = "macos")]
fn run_reader_once(cfg: &KeyReaderConfig, log: &dyn Fn(&str)) -> ReaderOutcome {
    let mut found = None;
    for p in cfg.candidates.iter().filter(|p| p.exists()) {
        if safe_to_run(p) {
            found = Some(p);
            break;
        }
        log("a key reader was ignored: it must be an absolute path, owned by you, not group or world writable");
    }
    let Some(path) = found else {
        return ReaderOutcome::Missing;
    };
    let mut child = match Command::new(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ReaderOutcome::Missing,
        Err(_) => {
            log("key reader could not be started");
            return ReaderOutcome::Failed("the key reader could not be started");
        }
    };
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return ReaderOutcome::Failed("the key reader gave no pipe");
    };
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        // One byte past the cap, so hitting the cap is detectable (never a truncated key).
        let _ = (&mut stdout).take(MAX_KEY_BYTES + 1).read_to_end(&mut buf);
        let _ = tx.send(buf);
    });

    let deadline = Instant::now() + cfg.timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() >= deadline => {
                // Hung (a locked keychain can wait on a dialog): kill it, fail closed.
                let _ = child.kill();
                let _ = child.wait();
                log("key reader timed out and was killed");
                return ReaderOutcome::Failed("the key reader timed out");
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return ReaderOutcome::Failed("the key reader could not be waited on");
            }
        }
    };
    if status.code() == Some(EXIT_NOT_FOUND) {
        return ReaderOutcome::NotFound;
    }
    if !status.success() {
        log("key reader exited with a failure");
        return ReaderOutcome::Failed("the key reader failed");
    }
    let bytes = rx
        .recv_timeout(Duration::from_millis(500))
        .unwrap_or_default();
    if bytes.len() as u64 > MAX_KEY_BYTES {
        log("key reader output was too long");
        return ReaderOutcome::Failed("the key reader output was too long");
    }
    match String::from_utf8(bytes) {
        Ok(s) if !s.trim().is_empty() => {
            let t = s.trim();
            // Copy only when there is whitespace to cut.
            if t.len() == s.len() {
                ReaderOutcome::Key(s)
            } else {
                ReaderOutcome::Key(t.to_string())
            }
        }
        _ => {
            log("key reader returned no key");
            ReaderOutcome::Failed("the key reader returned no key")
        }
    }
}

/// The one read rule: for the Anthropic key, try the helper first; `Missing` falls back to
/// `direct` (the read the app always had); `Failed` is a keychain error and `direct` is not
/// called. Every other credential goes straight to `direct`.
pub fn get_with_reader(
    name: &str,
    cfg: &KeyReaderConfig,
    log: &dyn Fn(&str),
    direct: impl FnOnce() -> Result<Option<String>, CredentialError>,
) -> Result<Option<String>, CredentialError> {
    if name != ANTHROPIC_API_KEY {
        return direct();
    }
    match run_reader(cfg, log) {
        ReaderOutcome::Key(k) => Ok(Some(k)),
        ReaderOutcome::Missing => direct(),
        ReaderOutcome::NotFound => Ok(None),
        ReaderOutcome::Failed(why) => Err(CredentialError::Keychain(why.to_string())),
    }
}

#[cfg(all(test, target_os = "macos"))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Serialises the tests: writing an executable and spawning it from another thread races
    /// (ETXTBSY), so one fake helper is created and run at a time.
    static LOCK: Mutex<()> = Mutex::new(());
    static N: AtomicUsize = AtomicUsize::new(0);

    const KEY: &str = "sk-ant-FAKE-pipe-key-0123456789";

    fn dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "camerata-keyreader-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Write a fake helper script and return a config that points at it.
    fn fake(script: &str, timeout_ms: u64) -> (KeyReaderConfig, PathBuf) {
        let d = dir();
        let p = d.join(READER_BIN);
        std::fs::write(&p, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        (
            KeyReaderConfig::new(vec![p], Duration::from_millis(timeout_ms)),
            d,
        )
    }

    fn logs() -> (std::sync::Arc<Mutex<Vec<String>>>, impl Fn(&str)) {
        let v = std::sync::Arc::new(Mutex::new(Vec::new()));
        let v2 = v.clone();
        (v, move |m: &str| v2.lock().unwrap().push(m.to_string()))
    }

    fn pid_alive(pid: &str) -> bool {
        Command::new("kill")
            .args(["-0", pid])
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// 1. The key arrives over the pipe, and never through argv, env, a file or a log.
    #[test]
    fn the_key_comes_over_the_pipe_and_never_through_argv_env_files_or_logs() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // The fake records its own argv and env into a file, then prints the key.
        let (cfg, d) = fake(
            &format!(
                "echo \"argc=$#\" > \"$(dirname \"$0\")/seen.txt\"\nenv >> \"$(dirname \"$0\")/seen.txt\"\nprintf %s '{KEY}'"
            ),
            5000,
        );
        let (captured, log) = logs();
        let out = run_reader(&cfg, &log);
        assert_eq!(
            out,
            ReaderOutcome::Key(KEY.to_string()),
            "the key must come back"
        );
        let seen = std::fs::read_to_string(d.join("seen.txt")).unwrap();
        assert!(seen.starts_with("argc=0\n"), "no argv: {seen}");
        assert!(
            !seen.contains(KEY),
            "the key must not be in the child's env"
        );
        assert!(
            !captured.lock().unwrap().join("\n").contains(KEY),
            "the key must not be logged"
        );
        assert!(
            !std::env::vars().any(|(_, v)| v.contains(KEY)),
            "the key must not be put in this process's env either"
        );
        // Nothing the engine wrote in the helper's folder holds the key either.
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.file_name().is_some_and(|n| n != READER_BIN) {
                assert!(!std::fs::read_to_string(&p).unwrap().contains(KEY));
            }
        }
    }

    /// 1b. A helper that prints the key and THEN fails must not leak it through the error.
    #[test]
    fn a_failed_helper_never_leaks_what_it_printed() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (cfg, _d) = fake(&format!("printf %s '{KEY}'\nexit 3"), 5000);
        let (captured, log) = logs();
        let r = get_with_reader(ANTHROPIC_API_KEY, &cfg, &log, || panic!("no direct read"));
        let err = r.unwrap_err().to_string();
        assert!(!err.contains(KEY), "{err}");
        assert!(!captured.lock().unwrap().join("\n").contains(KEY));
    }

    /// 2. Non-zero exit, empty output, and a hang all fail closed (never the direct read).
    #[test]
    fn non_zero_exit_and_empty_output_fail_closed_without_the_direct_read() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        for script in ["exit 1", "exit 0", "printf '  \\n'"] {
            let (cfg, _d) = fake(script, 5000);
            let r = get_with_reader(ANTHROPIC_API_KEY, &cfg, &|_| {}, || {
                panic!("the direct read must not run after a helper failure")
            });
            assert!(
                matches!(r, Err(CredentialError::Keychain(_))),
                "{script:?} must fail closed, got {r:?}"
            );
        }
    }

    #[test]
    fn a_hung_helper_is_killed_at_the_timeout() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (cfg, d) = fake(
            "echo $$ > \"$(dirname \"$0\")/pid.txt\"\nexec sleep 30",
            2000,
        );
        let start = Instant::now();
        let out = run_reader(&cfg, &|_| {});
        assert!(
            matches!(out, ReaderOutcome::Failed(_)),
            "a hang must fail closed, got {out:?}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(6),
            "must return within the timeout, took {:?}",
            start.elapsed()
        );
        let pid = std::fs::read_to_string(d.join("pid.txt")).unwrap();
        let pid = pid.trim();
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !pid_alive(pid),
            "the hung helper (pid {pid}) must be killed"
        );
    }

    /// 3. No helper: the direct read runs (through its seam, never the real keychain).
    #[test]
    fn a_missing_helper_falls_back_to_the_direct_read() {
        let cfg = KeyReaderConfig::new(
            vec![std::env::temp_dir().join("camerata-no-such-reader-574")],
            Duration::from_secs(1),
        );
        let r = get_with_reader(ANTHROPIC_API_KEY, &cfg, &|_| {}, || {
            Ok(Some("direct-fake".to_string()))
        });
        assert_eq!(r.unwrap().as_deref(), Some("direct-fake"));
    }

    /// The helper reads only the Anthropic account: other credentials never spawn it.
    #[test]
    fn other_credentials_skip_the_helper() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (cfg, d) = fake(
            &format!("touch \"$(dirname \"$0\")/ran\"\nprintf %s '{KEY}'"),
            5000,
        );
        let r = get_with_reader("github_token", &cfg, &|_| {}, || Ok(Some("gh".to_string())));
        assert_eq!(r.unwrap().as_deref(), Some("gh"));
        assert!(!Path::new(&d.join("ran")).exists(), "helper must not run");
    }

    #[test]
    fn the_stable_install_path_is_tried_first_then_the_beside_binary_copy() {
        let c = KeyReaderConfig::production_with(
            Some(PathBuf::from("/x/target/debug")),
            Some(PathBuf::from("/home/u")),
        );
        assert_eq!(
            c.candidates,
            vec![
                PathBuf::from("/home/u/.camerata/bin/camerata-key-reader"),
                PathBuf::from("/x/target/debug/camerata-key-reader"),
            ]
        );
        assert_eq!(c.timeout, READER_TIMEOUT);
    }

    /// Exit 2 is "no such entry": same as no key configured, and not a failure.
    #[test]
    fn exit_two_means_no_entry_and_is_ok_none() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (cfg, _d) = fake("exit 2", 5000);
        let r = get_with_reader(ANTHROPIC_API_KEY, &cfg, &|_| {}, || {
            panic!("no direct read after a definite not-found")
        });
        assert_eq!(r.unwrap(), None);
    }

    /// A failed run is remembered: the next reads do not spawn the helper again.
    #[test]
    fn a_failure_is_remembered_and_the_helper_is_not_respawned() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (cfg, d) = fake("echo x >> \"$(dirname \"$0\")/runs.txt\"\nexit 1", 5000);
        for _ in 0..3 {
            let r = get_with_reader(ANTHROPIC_API_KEY, &cfg, &|_| {}, || panic!("no direct"));
            assert!(matches!(r, Err(CredentialError::Keychain(_))));
        }
        let runs = std::fs::read_to_string(d.join("runs.txt")).unwrap();
        assert_eq!(runs.lines().count(), 1, "spawned more than once: {runs}");
    }

    #[test]
    fn the_key_is_redacted_in_debug_output() {
        let shown = format!("{:?}", ReaderOutcome::Key(KEY.to_string()));
        assert!(!shown.contains(KEY), "{shown}");
    }

    #[test]
    fn output_that_hits_the_cap_is_a_failure_not_a_truncated_key() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (cfg, _d) = fake("head -c 9000 /dev/zero | tr '\\0' 'a'", 5000);
        let out = run_reader(&cfg, &|_| {});
        assert!(matches!(out, ReaderOutcome::Failed(_)), "{out:?}");
    }

    /// A reader that others can write, or that is not an absolute path, is never run.
    #[test]
    fn an_unsafe_reader_is_treated_as_missing_and_logged() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (cfg, d) = fake(
            &format!("touch \"$(dirname \"$0\")/ran\"\nprintf %s '{KEY}'"),
            5000,
        );
        let p = d.join(READER_BIN);
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o775)).unwrap();
        let (captured, log) = logs();
        assert_eq!(run_reader(&cfg, &log), ReaderOutcome::Missing);
        assert!(
            !d.join("ran").exists(),
            "a group-writable reader must not run"
        );
        assert!(
            !captured.lock().unwrap().is_empty(),
            "an operator log line is expected"
        );
        let rel = KeyReaderConfig::new(vec![PathBuf::from("camerata-key-reader")], READER_TIMEOUT);
        assert_eq!(run_reader(&rel, &|_| {}), ReaderOutcome::Missing);
    }
}
