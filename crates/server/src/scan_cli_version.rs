//! The scan handlers' cli pre-flight: a real cli run launches the local Claude Code, which
//! refuses a model it predates, so every model the run will call is checked against its version
//! before the run starts (the same check `camerata inspect` makes).

use crate::cost_cap::RunControls;
use crate::onboard::SelectedRule;

/// The refusal message for a scan whose Claude Code is too old (or unreadable) for a model it
/// will call; `None` when nothing needs refusing. Only a run that uses the cli backend and
/// reviews with the model is checked. `check` is the version check, passed in so a test does not
/// need a real Claude Code.
#[allow(clippy::too_many_arguments)]
pub(crate) fn cli_version_refusal_for_scan(
    on_cli: bool,
    run_ai_review: bool,
    model: Option<&str>,
    calibration_model: Option<&str>,
    deep: bool,
    selected: &[SelectedRule],
    controls: &RunControls,
    check: impl FnOnce(&[(String, String)]) -> Result<(), String>,
) -> Option<String> {
    if !on_cli || !run_ai_review {
        return None;
    }
    let ids: Vec<&str> = selected.iter().map(|r| r.id.as_str()).collect();
    let models =
        crate::cost_cap::models_the_run_will_use(model, calibration_model, deep, &ids, controls);
    check(&models).err()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cli_scan_is_refused_when_the_claude_code_check_fails() {
        let controls = RunControls::default();
        let refuse = |_: &[(String, String)]| Err("Claude Code is too old".to_string());
        // A real cli scan that reviews with the model is refused.
        let why = cli_version_refusal_for_scan(
            true,
            true,
            Some("claude-fable-5-2"),
            None,
            false,
            &[],
            &controls,
            refuse,
        )
        .expect("refused");
        assert!(why.contains("too old"), "{why}");
        // The api backend, or a scan with no AI review, launches no Claude Code.
        for (on_cli, review) in [(false, true), (true, false)] {
            assert!(cli_version_refusal_for_scan(
                on_cli,
                review,
                Some("claude-fable-5-2"),
                None,
                false,
                &[],
                &controls,
                refuse
            )
            .is_none());
        }
        // The check is handed the models the run will call, with their roles.
        let seen = std::cell::RefCell::new(Vec::new());
        let _ = cli_version_refusal_for_scan(
            true,
            true,
            Some("claude-fable-5-2"),
            None,
            false,
            &[],
            &controls,
            |m: &[(String, String)]| {
                seen.borrow_mut().extend(m.iter().cloned());
                Ok(())
            },
        );
        assert!(
            seen.borrow().iter().any(|(_, m)| m == "claude-fable-5-2"),
            "{:?}",
            seen.borrow()
        );
    }

    /// A cli run with the agent finder on hands the finder's own model to the version check, so
    /// a Claude Code too old for it is refused before the first model call.
    #[test]
    fn the_agent_finders_model_reaches_the_claude_code_version_check() {
        let controls = RunControls {
            scope: Default::default(),
            finder: crate::ai_audit::agent_finder::FinderChoice::Agent,
            finder_model: Some("claude-finder-pick".to_string()),
            ..RunControls::default()
        };
        let seen = std::cell::RefCell::new(Vec::new());
        let why = cli_version_refusal_for_scan(
            true,
            true,
            Some("claude-sonnet-5-5"),
            None,
            false,
            &[],
            &controls,
            |m: &[(String, String)]| {
                seen.borrow_mut().extend(m.iter().cloned());
                Err("Claude Code is too old".to_string())
            },
        );
        assert!(why.is_some_and(|w| w.contains("too old")));
        assert!(
            seen.borrow()
                .iter()
                .any(|(role, m)| m == "claude-finder-pick" && role.contains("the finder")),
            "{:?}",
            seen.borrow()
        );
    }
}
