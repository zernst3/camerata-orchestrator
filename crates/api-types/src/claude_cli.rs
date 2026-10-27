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
