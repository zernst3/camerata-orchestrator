//! The fixed child environment every semgrep process the engine spawns carries.
//!
//! Semgrep contacts its vendor to check for a newer version on every run, even with
//! `--metrics=off`. A client scan must make no outbound vendor call, so the version check is
//! always off. This is a fixed rule, not a setting: there is no toggle and nothing reads the
//! user's environment. The value is set on the child process only, never on the engine's own
//! process environment.

use std::process::Command;

/// Name of the semgrep environment variable that gates the vendor version check.
pub const SEMGREP_VERSION_CHECK_ENV: &str = "SEMGREP_ENABLE_VERSION_CHECK";

/// The value that turns the version check off.
pub const SEMGREP_VERSION_CHECK_OFF: &str = "0";

/// Apply the fixed semgrep child environment to `cmd`. Every code path that can run semgrep
/// must call this on its command before spawning. For a `tokio::process::Command`, pass
/// `cmd.as_std_mut()`.
pub fn apply_semgrep_env(cmd: &mut Command) {
    cmd.env(SEMGREP_VERSION_CHECK_ENV, SEMGREP_VERSION_CHECK_OFF);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(cmd: &Command, key: &str) -> Option<String> {
        cmd.get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new(key))
            .and_then(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
    }

    #[test]
    fn helper_sets_version_check_off_on_the_child_command() {
        let mut cmd = Command::new("semgrep");
        apply_semgrep_env(&mut cmd);
        assert_eq!(
            env_of(&cmd, SEMGREP_VERSION_CHECK_ENV).as_deref(),
            Some(SEMGREP_VERSION_CHECK_OFF)
        );
    }
}
