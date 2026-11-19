//! The words and rules of the Inspection screen's "This run" block (settings overhaul, work order
//! C): the Checks to run options and the request flags each sends, the Full inspection state for
//! each finder, the Run mode options (Engine finder only), the Agent finder's estimate and the
//! sentence that says it is not available. Pure functions with no rendering framework, so the
//! screen only chooses pixels and a test needs no browser.

use camerata_api_types::project::ProjectFinder;

/// What the "Checks to run" select offers: which of the two kinds of check a run makes. One
/// select, so there is no "untick both" case, and it sends the same two request flags the two
/// checkboxes it replaces did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksToRun {
    /// The security floor, the mechanical linters and the model review. The default.
    All,
    /// The model review only.
    AiOnly,
    /// The security floor and the mechanical linters only. Calls no model, so it costs nothing.
    DeterministicOnly,
}

impl ChecksToRun {
    /// The options in the order the select shows them.
    pub const OPTIONS: [ChecksToRun; 3] = [
        ChecksToRun::All,
        ChecksToRun::AiOnly,
        ChecksToRun::DeterministicOnly,
    ];

    /// The select's option value.
    pub fn value(self) -> &'static str {
        match self {
            ChecksToRun::All => "all",
            ChecksToRun::AiOnly => "ai",
            ChecksToRun::DeterministicOnly => "deterministic",
        }
    }

    /// What a person reads in the select.
    pub fn label(self) -> &'static str {
        match self {
            ChecksToRun::All => "All checks",
            ChecksToRun::AiOnly => "AI review only",
            ChecksToRun::DeterministicOnly => "Deterministic checks only",
        }
    }

    /// The choice a select value names, or the default for anything else.
    pub fn from_value(value: &str) -> ChecksToRun {
        ChecksToRun::OPTIONS
            .into_iter()
            .find(|c| c.value() == value)
            .unwrap_or(ChecksToRun::All)
    }

    /// The two request flags, `(run_ai_review, run_deterministic)`.
    pub fn flags(self) -> (bool, bool) {
        match self {
            ChecksToRun::All => (true, true),
            ChecksToRun::AiOnly => (true, false),
            ChecksToRun::DeterministicOnly => (false, true),
        }
    }

    /// Whether the run makes any model call.
    pub fn uses_a_model(self) -> bool {
        self.flags().0
    }

    /// The one short line beside the select where an operator needs it to act. The long
    /// explanation is the "Checks to run" help topic.
    pub fn inline_line(self) -> Option<&'static str> {
        match self {
            ChecksToRun::DeterministicOnly => Some("Calls no model and costs nothing."),
            ChecksToRun::AiOnly => Some("The security floor and the linters are skipped."),
            ChecksToRun::All => None,
        }
    }
}

/// The estimate line for a run that calls no model, for either finder.
pub const DETERMINISTIC_ONLY_ESTIMATE: &str = "$0, no model calls";

/// How the "Full inspection" control looks for the finder in effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FullInspectionState {
    /// Whether the box shows ticked.
    pub checked: bool,
    /// Whether the operator cannot change it.
    pub locked: bool,
}

/// The one line shown beside the locked box for the Agent finder.
pub const AGENT_FULL_INSPECTION_LINE: &str = "The Agent finder always inspects every file.";

/// The Full inspection box: the Agent finder always inspects every file, so it is ticked and
/// locked; the Engine finder keeps the operator's own tick, free to change.
pub fn full_inspection(finder: ProjectFinder, ticked: bool) -> FullInspectionState {
    match finder {
        ProjectFinder::Agent => FullInspectionState {
            checked: true,
            locked: true,
        },
        ProjectFinder::Engine => FullInspectionState {
            checked: ticked,
            locked: false,
        },
    }
}

/// The request's `incremental` flag. Always false for the Agent finder, whatever the box held
/// before the finder changed, because the Agent finder refuses an incremental run.
pub fn incremental_flag(finder: ProjectFinder, ticked: bool) -> bool {
    !full_inspection(finder, ticked).checked
}

/// The Run mode options (Engine finder only): the value the request carries and what a person
/// reads. Every mode runs as a saved job; the mode decides how the model is called.
pub const RUN_MODES: [(&str, &str); 5] = [
    (
        "auto",
        "Default (batch on the API backend, else parallel)",
    ),
    ("parallel", "Parallel (real time)"),
    ("sequential", "Sequential (real time, slower, gentlest)"),
    ("job", "Large inspection (same as Default)"),
    ("batch", "Batch (half price, API key required)"),
];

/// The Engine finder's one line under "Run mode".
pub const RUN_MODE_INLINE_LINE: &str = "Every inspection is saved, so Run what remains can finish it.";

/// The sentence when the Agent finder has no estimate. Always followed by the reason.
pub const AGENT_ESTIMATE_UNAVAILABLE: &str = "Estimate not available for the Agent finder";

/// "Estimate not available for the Agent finder. <reason>".
pub fn agent_estimate_unavailable(reason: &str) -> String {
    let reason = reason.trim();
    if reason.is_empty() {
        format!("{AGENT_ESTIMATE_UNAVAILABLE}.")
    } else {
        format!("{AGENT_ESTIMATE_UNAVAILABLE}. {reason}")
    }
}

/// One repository's Agent finder figures, as the preflight route sends them.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRepoFigure {
    pub repo: String,
    pub pieces: usize,
    pub joints_calls: usize,
    pub expected_usd: f64,
    pub low_usd: f64,
    pub high_usd: f64,
}

/// The Agent finder's estimate as the screen shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentEstimateText {
    /// "Estimated cost of the finder stage".
    pub label: &'static str,
    /// "about $4.20".
    pub value: String,
    /// "most likely between $2.52 and $7.56".
    pub range: String,
    /// "3 pieces, 1 joints call".
    pub calls: String,
    /// The one line that says the other stages are extra.
    pub extra: &'static str,
    /// One line for each repository, only when there are several.
    pub repo_lines: Vec<String>,
}

/// The label over the Agent finder's figure.
pub const AGENT_ESTIMATE_LABEL: &str = "Estimated cost of the finder stage";

/// The line that keeps the figure honest: it is the finder stage only.
pub const AGENT_ESTIMATE_EXTRA: &str = "Other stages are extra.";

/// What the estimate block shows for the Agent finder.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEstimateView {
    Figure(AgentEstimateText),
    /// The whole sentence, reason included. No dollar figure.
    Unavailable(String),
}

fn money(usd: f64) -> String {
    if usd < 0.01 {
        "<$0.01".to_string()
    } else {
        format!("${usd:.2}")
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The Agent finder's estimate from the repositories whose figures came back. `unreadable` names
/// the repositories that could not be read, each with its reason; they are not in the figure and
/// the lines say so. With no figure at all the answer is [`AgentEstimateView::Unavailable`].
pub fn agent_estimate_view(
    figures: &[AgentRepoFigure],
    unreadable: &[(String, String)],
) -> AgentEstimateView {
    if figures.is_empty() {
        let reason = match unreadable.first() {
            Some((repo, why)) => format!("{repo} could not be read: {why}"),
            None => "No repository size came back.".to_string(),
        };
        return AgentEstimateView::Unavailable(agent_estimate_unavailable(&reason));
    }
    let expected: f64 = figures.iter().map(|f| f.expected_usd).sum();
    let low: f64 = figures.iter().map(|f| f.low_usd).sum();
    let high: f64 = figures.iter().map(|f| f.high_usd).sum();
    let pieces: usize = figures.iter().map(|f| f.pieces).sum();
    let joints: usize = figures.iter().map(|f| f.joints_calls).sum();
    let mut repo_lines = Vec::new();
    if figures.len() + unreadable.len() > 1 {
        for f in figures {
            repo_lines.push(super::agent_finder_repo_line(
                &f.repo,
                f.pieces,
                f.expected_usd,
                f.low_usd,
                f.high_usd,
            ));
        }
        for (repo, why) in unreadable {
            repo_lines.push(super::unreadable_repo_line(repo, why));
        }
    }
    AgentEstimateView::Figure(AgentEstimateText {
        label: AGENT_ESTIMATE_LABEL,
        value: if expected < 0.01 {
            "less than $0.01".to_string()
        } else {
            format!("about {}", money(expected))
        },
        range: format!("most likely between {} and {}", money(low), money(high)),
        calls: format!(
            "{}, {}",
            plural(pieces, "piece", "pieces"),
            plural(joints, "joints call", "joints calls")
        ),
        extra: AGENT_ESTIMATE_EXTRA,
        repo_lines,
    })
}

/// The Engine finder's scope line under its estimate: what the figure is priced over.
pub fn engine_scope_line(incremental: bool) -> &'static str {
    if incremental {
        "Priced over every file. An incremental inspection usually costs less."
    } else {
        "Priced over every file."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn figure(repo: &str, pieces: usize, joints: usize, usd: f64) -> AgentRepoFigure {
        AgentRepoFigure {
            repo: repo.to_string(),
            pieces,
            joints_calls: joints,
            expected_usd: usd,
            low_usd: usd * 0.6,
            high_usd: usd * 1.8,
        }
    }

    #[test]
    fn the_three_checks_send_the_same_flag_pairs_the_two_checkboxes_did() {
        assert_eq!(ChecksToRun::All.flags(), (true, true));
        assert_eq!(ChecksToRun::AiOnly.flags(), (true, false));
        assert_eq!(ChecksToRun::DeterministicOnly.flags(), (false, true));
        // Never the pair that was coerced back to both.
        for c in ChecksToRun::OPTIONS {
            assert_ne!(c.flags(), (false, false), "{c:?}");
            assert_eq!(ChecksToRun::from_value(c.value()), c);
        }
        assert_eq!(ChecksToRun::from_value("anything else"), ChecksToRun::All);
        assert!(!ChecksToRun::DeterministicOnly.uses_a_model());
        assert!(ChecksToRun::All.uses_a_model());
    }

    #[test]
    fn full_inspection_is_locked_on_for_the_agent_finder_and_free_for_the_engine() {
        let agent = full_inspection(ProjectFinder::Agent, false);
        assert!(agent.checked && agent.locked);
        let engine_off = full_inspection(ProjectFinder::Engine, false);
        assert!(!engine_off.checked && !engine_off.locked);
        let engine_on = full_inspection(ProjectFinder::Engine, true);
        assert!(engine_on.checked && !engine_on.locked);
    }

    #[test]
    fn the_request_is_never_incremental_for_the_agent_finder_whatever_the_box_holds() {
        assert!(!incremental_flag(ProjectFinder::Agent, false));
        assert!(!incremental_flag(ProjectFinder::Agent, true));
        assert!(incremental_flag(ProjectFinder::Engine, false));
        assert!(!incremental_flag(ProjectFinder::Engine, true));
    }

    #[test]
    fn the_agent_estimate_says_the_finder_stage_and_that_the_others_are_extra() {
        let AgentEstimateView::Figure(text) = agent_estimate_view(
            &[figure("acme/api", 3, 1, 4.2)],
            &[],
        ) else {
            panic!("a figure was given");
        };
        assert_eq!(text.label, "Estimated cost of the finder stage");
        assert_eq!(text.value, "about $4.20");
        assert_eq!(text.range, "most likely between $2.52 and $7.56");
        assert_eq!(text.calls, "3 pieces, 1 joints call");
        assert_eq!(text.extra, "Other stages are extra.");
        assert!(text.repo_lines.is_empty(), "one repository needs no list");
        let all = format!("{} {} {} {}", text.value, text.range, text.calls, text.extra);
        for engine_word in ["token", "pass", "rule"] {
            assert!(!all.contains(engine_word), "{engine_word}: {all}");
        }
    }

    #[test]
    fn several_repositories_add_up_and_each_unreadable_one_is_named() {
        let AgentEstimateView::Figure(text) = agent_estimate_view(
            &[figure("acme/api", 1, 0, 2.0), figure("acme/web", 2, 1, 3.0)],
            &[("acme/old".to_string(), "no local folder set".to_string())],
        ) else {
            panic!("figures were given");
        };
        assert_eq!(text.value, "about $5.00");
        assert_eq!(text.calls, "3 pieces, 1 joints call");
        assert_eq!(text.repo_lines.len(), 3, "{:?}", text.repo_lines);
        assert!(text.repo_lines[2].contains("acme/old"));
        assert!(text.repo_lines[2].contains("not in this estimate"));
    }

    #[test]
    fn with_no_figure_the_sentence_is_all_there_is_and_no_dollar_figure_is_made_up() {
        let AgentEstimateView::Unavailable(text) = agent_estimate_view(&[], &[]) else {
            panic!("no figure");
        };
        assert!(text.starts_with("Estimate not available for the Agent finder"));
        assert!(!text.contains('$'), "{text}");
        let AgentEstimateView::Unavailable(why) = agent_estimate_view(
            &[],
            &[("acme/api".to_string(), "no local folder set".to_string())],
        ) else {
            panic!("no figure");
        };
        assert!(why.contains("acme/api") && why.contains("no local folder set"), "{why}");
        assert_eq!(
            agent_estimate_unavailable(""),
            "Estimate not available for the Agent finder."
        );
    }

    #[test]
    fn deterministic_only_costs_nothing_and_says_so() {
        assert_eq!(DETERMINISTIC_ONLY_ESTIMATE, "$0, no model calls");
        assert!(ChecksToRun::DeterministicOnly.inline_line().is_some());
        assert!(ChecksToRun::All.inline_line().is_none());
    }

    #[test]
    fn no_word_here_has_a_dash_a_serial_comma_or_the_retired_verb() {
        let mut words: Vec<String> = vec![
            AGENT_FULL_INSPECTION_LINE.to_string(),
            RUN_MODE_INLINE_LINE.to_string(),
            AGENT_ESTIMATE_UNAVAILABLE.to_string(),
            AGENT_ESTIMATE_LABEL.to_string(),
            AGENT_ESTIMATE_EXTRA.to_string(),
            DETERMINISTIC_ONLY_ESTIMATE.to_string(),
            engine_scope_line(true).to_string(),
            engine_scope_line(false).to_string(),
        ];
        for c in ChecksToRun::OPTIONS {
            words.push(c.label().to_string());
            words.extend(c.inline_line().map(str::to_string));
        }
        words.extend(RUN_MODES.iter().map(|(_, l)| (*l).to_string()));
        for w in words {
            assert!(!w.contains(['\u{2013}', '\u{2014}']), "{w}");
            assert!(!w.contains(", and "), "{w}");
            assert!(!w.to_lowercase().contains("audit"), "{w}");
        }
    }
}
