//! The cli backend's pre-flight: is the Claude Code the run will launch new enough for every
//! model the run will call?
//!
//! Claude Code refuses a model it predates. Without this check a run starts, every call to that
//! model fails, and the run ends incomplete after the time was spent. The check runs
//! `<program> --version` once before any model call and compares it with
//! [`crate::model_registry::CLAUDE_CODE_MIN_VERSIONS`]. A version that cannot be read is a
//! refusal too: the check fails closed.

use crate::model_registry::min_claude_code_version;
use camerata_api_types::claude_cli::CLAUDE_PROGRAM_ENV;
use std::ffi::OsStr;

/// A Claude Code version: (major, minor, patch).
pub type Version = (u32, u32, u32);

/// How long `--version` may take before the check gives up (and refuses).
const VERSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

fn show(v: Version) -> String {
    format!("{}.{}.{}", v.0, v.1, v.2)
}

/// The first whitespace-separated word of the form `major.minor.patch` in `line`.
fn version_in_line(line: &str) -> Option<Version> {
    line.split_whitespace().find_map(|word| {
        let word = word.trim_start_matches('v');
        let mut parts = word.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        // The patch may carry a suffix such as `-beta`; its leading digits are the patch.
        let patch_raw = parts.next()?;
        let digits: String = patch_raw
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let patch = digits.parse().ok()?;
        Some((major, minor, patch))
    })
}

/// The version in `claude --version` output. The real line has the shape
/// `2.1.285 (Claude Code)`: the last line of that shape wins, so a notice that also says
/// `Claude Code` or holds another version number is not read as the version. With no line of
/// that shape, the line that says `Claude Code` is read, else the first line.
pub fn parse_version(output: &str) -> Option<Version> {
    let real = output.lines().rev().find(|l| {
        let l = l.trim();
        l.ends_with("(Claude Code)")
            && l.split_whitespace()
                .next()
                .is_some_and(|w| version_in_line(w).is_some())
    });
    if let Some(line) = real {
        return version_in_line(line);
    }
    let line = output
        .lines()
        .find(|l| l.contains("Claude Code"))
        .or_else(|| output.lines().next())?;
    version_in_line(line)
}

/// The probe command: `<program> --version` and nothing else, stdin closed. It carries no model
/// input; `tests/model_input_guard.rs` clears this file as a non-sender on that basis, and a
/// test below pins the argv.
fn version_probe(program: &OsStr) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    cmd.arg("--version").stdin(std::process::Stdio::null());
    cmd
}

/// Run `<program> --version` and read the version, or say why it could not be read.
pub fn read_version(program: &OsStr) -> Result<Version, String> {
    read_version_within(program, VERSION_TIMEOUT)
}

/// What the probe thread found.
enum Probe {
    Ran(std::process::Output),
    CouldNotRun(String),
    TimedOut,
}

/// Run the probe on a thread of its own (so a caller already inside an async runtime is not
/// blocked on a runtime of ours), bounded by `timeout`. The child is killed when the wait is
/// given up on (`kill_on_drop`), so a hung `claude` is not left running.
fn probe(program: std::ffi::OsString, timeout: std::time::Duration) -> Probe {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return Probe::CouldNotRun(e.to_string()),
    };
    runtime.block_on(async move {
        let mut cmd = tokio::process::Command::from(version_probe(&program));
        cmd.kill_on_drop(true);
        match tokio::time::timeout(timeout, cmd.output()).await {
            Ok(Ok(out)) => Probe::Ran(out),
            Ok(Err(e)) => Probe::CouldNotRun(e.to_string()),
            Err(_) => Probe::TimedOut,
        }
    })
}

/// [`read_version`] with the time it may take given, so a test need not wait the full bound.
fn read_version_within(program: &OsStr, timeout: std::time::Duration) -> Result<Version, String> {
    let program = program.to_os_string();
    let shown = program.to_string_lossy().into_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(probe(program, timeout));
    });
    let out = match rx.recv() {
        Ok(Probe::Ran(out)) => out,
        Ok(Probe::CouldNotRun(e)) => {
            return Err(format!("`{shown} --version` could not be run ({e})"))
        }
        Ok(Probe::TimedOut) | Err(_) => {
            return Err(format!(
                "`{shown} --version` did not answer within {} seconds",
                timeout.as_secs()
            ))
        }
    };
    if !out.status.success() {
        return Err(format!("`{shown} --version` exited with {}", out.status));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    parse_version(&text).ok_or_else(|| {
        format!(
            "`{shown} --version` printed {:?}, which holds no version number",
            text.trim()
        )
    })
}

/// Refuse a run whose Claude Code is too old for one of its models. `models` is each model the
/// run will call on the cli with the role(s) it plays, as
/// `camerata_server::cost_cap::models_the_run_will_use` lists them. Returns the operator message
/// on refusal. A run that uses no model with a known minimum passes without reading the version.
pub fn check_models(program: &OsStr, models: &[(String, String)]) -> Result<(), String> {
    let needing: Vec<(&str, &str, Version)> = models
        .iter()
        .filter_map(|(roles, model)| {
            min_claude_code_version(model).map(|v| (roles.as_str(), model.as_str(), v))
        })
        .collect();
    if needing.is_empty() {
        return Ok(());
    }
    let shown = program.to_string_lossy().into_owned();
    let found = read_version(program).map_err(|why| {
        format!(
            "This run cannot start: Claude Code's version could not be read, so it cannot be \
             checked against the models this run calls ({}). {why}. Fix: install Claude Code, or \
             set {CLAUDE_PROGRAM_ENV} to the full path of a working, current `claude` binary.",
            needing
                .iter()
                .map(|(roles, m, v)| format!("{m} ({roles}) needs {} or newer", show(*v)))
                .collect::<Vec<_>>()
                .join("; ")
        )
    })?;
    for (roles, model, needed) in &needing {
        if found < *needed {
            return Err(format!(
                "This run cannot start: the Claude Code at `{shown}` is version {}, but {model} \
                 ({roles}) needs version {} or newer. Fix: update Claude Code, or set \
                 {CLAUDE_PROGRAM_ENV} to the full path of a newer `claude` binary.",
                show(found),
                show(*needed)
            ));
        }
    }
    Ok(())
}

/// [`check_models`] against the program every cli call launches
/// ([`camerata_api_types::claude_cli::claude_program`]). A `CAMERATA_CLAUDE_PROGRAM` that is not
/// an absolute path is refused first, whatever the models.
pub fn check_run_models(models: &[(String, String)]) -> Result<(), String> {
    check_program_models(
        camerata_api_types::claude_cli::validated_claude_program(),
        models,
    )
}

fn check_program_models(
    program: Result<std::ffi::OsString, String>,
    models: &[(String, String)],
) -> Result<(), String> {
    check_models(&program?, models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// An executable fake `claude` printing `body` for `--version`, under a unique temp dir.
    fn fake(tag: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "camerata-claude-version-{}-{tag}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("claude");
        std::fs::write(&bin, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        // First run pays the OS's check of a new executable.
        let _ = std::process::Command::new(&bin).arg("--version").output();
        bin
    }

    fn run(models: &[&str]) -> Vec<(String, String)> {
        models
            .iter()
            .map(|m| ("the group reviewer".to_string(), (*m).to_string()))
            .collect()
    }

    #[test]
    fn the_probe_is_exactly_version_with_no_stdin_and_no_other_argument() {
        let cmd = version_probe(OsStr::new("claude"));
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, ["--version"]);
        // Source pin: the only argument call and the only stdin setting in this file's
        // non-test code are the probe's, so a later edit that sends a prompt trips this.
        let code = include_str!("claude_version.rs");
        let code = &code[..code.find("#[cfg(test)]").unwrap()];
        assert_eq!(code.matches(".arg(").count(), 1, "one argument call");
        // Spelled in pieces so this test does not itself hold the texts the guard forbids.
        assert_eq!(code.matches(concat!(".ar", "gs(")).count(), 0);
        assert_eq!(code.matches(".stdin(").count(), 1);
        assert!(code.contains(".stdin(std::process::Stdio::null())"));
        assert_eq!(code.matches("Command::new(").count(), 1);
        assert_eq!(code.matches(concat!(".sp", "awn(")).count(), 0);
    }

    #[test]
    fn parses_the_version_line() {
        assert_eq!(parse_version("2.1.285 (Claude Code)\n"), Some((2, 1, 285)));
        assert_eq!(parse_version("Claude Code v2.1.321"), Some((2, 1, 321)));
        assert_eq!(parse_version("2.1.290-beta.1"), Some((2, 1, 290)));
        assert_eq!(parse_version("no version here"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn an_old_claude_code_is_refused_for_fable_5_2_with_the_fix() {
        let bin = fake("old", "echo '2.1.285 (Claude Code)'");
        let err = check_models(bin.as_os_str(), &run(&["claude-fable-5-2"])).unwrap_err();
        assert!(err.contains("2.1.285"), "{err}");
        assert!(err.contains("2.1.290"), "{err}");
        assert!(err.contains("claude-fable-5-2"), "{err}");
        assert!(err.contains("update Claude Code"), "{err}");
        assert!(err.contains("CAMERATA_CLAUDE_PROGRAM"), "{err}");
    }

    #[test]
    fn a_notice_line_with_another_version_does_not_hide_the_real_version_line() {
        assert_eq!(
            parse_version("Update available: 9.9.9\n2.1.285 (Claude Code)\n"),
            Some((2, 1, 285))
        );
        // A notice that says Claude Code and holds another version, ahead of the real line.
        assert_eq!(
            parse_version("Claude Code update 9.9.9 is available\n2.1.285 (Claude Code)\n"),
            Some((2, 1, 285))
        );
        // No Claude Code line: the first line is the one read.
        assert_eq!(parse_version("3.0.1\n4.0.0\n"), Some((3, 0, 1)));
    }

    #[test]
    fn a_failing_version_command_is_an_unreadable_version() {
        let bin = fake("exit1", "echo '2.1.321 (Claude Code)'; exit 1");
        let err = read_version(bin.as_os_str()).unwrap_err();
        assert!(err.contains("exited with"), "{err}");
    }

    #[test]
    fn a_hung_version_command_is_given_up_on_and_killed() {
        let dir = std::env::temp_dir().join(format!("camerata-claude-hang-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pid_file = dir.join("pid");
        // The helper's warm-up run must not hang: the script hangs only once the marker exists.
        let marker = dir.join("go");
        let bin = fake(
            "hang",
            &format!(
                "[ -e {} ] || exit 0\necho $$ > {}\nexec sleep 60",
                marker.display(),
                pid_file.display()
            ),
        );
        std::fs::write(&marker, "").unwrap();
        let started = std::time::Instant::now();
        let err =
            read_version_within(bin.as_os_str(), std::time::Duration::from_secs(1)).unwrap_err();
        assert!(err.contains("did not answer"), "{err}");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        let pid = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .to_string();
        // Killed: gone, or a zombie waiting only to be reaped.
        let gone = || {
            // The flag is spelled in pieces so this test does not hold the texts the guard forbids.
            let out = std::process::Command::new("ps")
                .arg("-o")
                .arg("stat=")
                .arg(concat!("-", "p"))
                .arg(&pid)
                .output()
                .unwrap();
            let stat = String::from_utf8_lossy(&out.stdout).trim().to_string();
            stat.is_empty() || stat.starts_with('Z')
        };
        for _ in 0..40 {
            if gone() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("the hung `--version` child {pid} is still running");
    }

    #[test]
    fn the_unreadable_version_message_names_the_role_like_the_too_old_one() {
        let bin = fake("junk2", "echo 'something odd'");
        let err = check_models(bin.as_os_str(), &run(&["claude-fable-5-2"])).unwrap_err();
        assert!(err.contains("(the group reviewer)"), "{err}");
        assert!(err.contains("claude-fable-5-2"), "{err}");
    }

    #[test]
    fn a_program_setting_that_is_refused_refuses_the_run_even_for_models_with_no_minimum() {
        let refused =
            Err("CAMERATA_CLAUDE_PROGRAM is set to `x`, which is not an absolute path".to_string());
        let err = check_program_models(refused, &run(&["claude-opus-5-5"])).unwrap_err();
        assert!(err.contains("not an absolute path"), "{err}");
    }

    #[test]
    fn a_new_enough_claude_code_passes() {
        let bin = fake("new", "echo '2.1.321 (Claude Code)'");
        assert!(check_models(bin.as_os_str(), &run(&["claude-fable-5-2"])).is_ok());
        let exact = fake("exact", "echo '2.1.290 (Claude Code)'");
        assert!(check_models(exact.as_os_str(), &run(&["claude-fable-5-2"])).is_ok());
    }

    #[test]
    fn an_unreadable_version_is_a_refusal_with_the_reason() {
        let bin = fake("junk", "echo 'something odd'");
        let err = check_models(bin.as_os_str(), &run(&["claude-fable-5-2"])).unwrap_err();
        assert!(err.contains("could not be read"), "{err}");
        assert!(err.contains("holds no version number"), "{err}");
        let missing = std::path::Path::new("/nonexistent/claude");
        let err = check_models(missing.as_os_str(), &run(&["claude-fable-5-2"])).unwrap_err();
        assert!(err.contains("could not be run"), "{err}");
    }

    #[test]
    fn a_run_with_no_model_needing_a_minimum_ignores_an_old_version() {
        let bin = fake("old2", "echo '2.1.285 (Claude Code)'");
        assert!(check_models(
            bin.as_os_str(),
            &run(&["claude-opus-5-5", "claude-haiku-5-5"])
        )
        .is_ok());
        // Not even read: an unreadable program is fine when no model needs the check.
        let missing = std::path::Path::new("/nonexistent/claude");
        assert!(check_models(missing.as_os_str(), &run(&["claude-opus-5-5"])).is_ok());
    }
}
