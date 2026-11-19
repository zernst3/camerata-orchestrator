//! The help text of the Inspection screen (work order C): what Checks to run, Run mode, Full
//! inspection and the Spend limit do, how the estimate is built, and how each finder runs. The
//! screen keeps one short line where an operator must act; everything longer is here, behind a
//! "?" button.

use camerata_api_types::project::{ProjectBackend, ProjectFinder};

use super::HelpTopic;

/// The ids of this area's topics.
pub const INSPECT_TOPIC_ID: &str = "inspect";
pub const CHECKS_TOPIC_ID: &str = "checks-to-run";
pub const RUN_MODE_TOPIC_ID: &str = "run-mode";
pub const FULL_INSPECTION_TOPIC_ID: &str = "full-inspection";
pub const SPEND_LIMIT_TOPIC_ID: &str = "spend-limit";
pub const ESTIMATE_TOPIC_ID: &str = "estimate";
pub const BACKEND_TOPIC_ID: &str = "backend";

fn topic(id: &'static str, title: &'static str, paragraphs: &[&str]) -> HelpTopic {
    HelpTopic {
        id,
        title,
        body: paragraphs.join("\n\n"),
    }
}

/// "This run": what pressing Inspect does, for the finder in effect.
pub fn inspect_topic(finder: ProjectFinder) -> HelpTopic {
    let how = match finder {
        ProjectFinder::Agent => {
            "The Agent finder reviews the code in pieces, each a live conversation, then makes \
             joints calls across the pieces. The finder stage is not batched. Group review, fix \
             and duplicate check run as their own stages and are not part of the finder's \
             estimate."
        }
        ProjectFinder::Engine => {
            "The Engine finder checks the code rule by rule: the model checks it against only \
             the rules you ticked and also flags anything else worth a look, as advice. The \
             deterministic security rules for secrets, raw SQL and secret URLs run as the enforced \
             floor, unless Checks to run is set to AI review only."
        }
    };
    topic(
        INSPECT_TOPIC_ID,
        "This run",
        &[
            "The inspection checks the code against the rules you ticked above. When it ends \
             you review what it found and export the result.",
            how,
            "Every inspection runs on the server as a saved job. You can leave this screen and \
             come back. Run what remains can finish a run that stopped.",
        ],
    )
}

/// "Checks to run".
pub fn checks_topic() -> HelpTopic {
    topic(
        CHECKS_TOPIC_ID,
        "Checks to run",
        &[
            "All checks runs the deterministic checks and the AI review. This is the default.",
            "Deterministic checks only runs the always-on security floor and the mechanical \
             linters on your machine. It calls no model, so it costs nothing and is fast. The AI \
             review is skipped entirely. Use it for a quick check.",
            "AI review only runs the model against your ticked rules and skips the security \
             floor and the linters.",
        ],
    )
}

/// "Run mode" (Engine finder only).
pub fn run_mode_topic() -> HelpTopic {
    topic(
        RUN_MODE_TOPIC_ID,
        "Run mode",
        &[
            "Run mode decides how the Engine finder calls the model. It does not change what is \
             checked.",
            "Default runs the inspection as a Message Batch on the API backend, at half price, \
             with results when the whole batch is done. On the CLI backend it runs in real \
             time. Parallel runs rule batches at the same time in real time. Sequential runs \
             them one after another in real time, which is slower and gentlest on rate limits. \
             Large inspection is the same as Default. It is what the screen picks for a big or \
             multi-repo codebase.",
            "Batch always uses the Anthropic Message Batches API for a flat 50% discount on all \
             tokens. It needs the API backend and an Anthropic API key, added in the app or set as \
             ANTHROPIC_API_KEY. Results arrive asynchronously, in seconds to minutes and up to \
             24 hours on very large codebases.",
            "Whatever the mode, the inspection is saved on the server as a job, so you can leave \
             and watch findings arrive. Run what remains can finish it.",
        ],
    )
}

/// "Full inspection".
pub fn full_inspection_topic() -> HelpTopic {
    topic(
        FULL_INSPECTION_TOPIC_ID,
        "Full inspection",
        &[
            "The Agent finder always inspects every file, so the box is ticked and locked for \
             it.",
            "For the Engine finder, a second inspection by default reuses results for files that \
             did not change, so a small edit costs a fraction of a full run and the first \
             inspection of a project is always full. Tick Full inspection to inspect every file \
             again, for example after you change your rule selection or to refresh every \
             finding.",
        ],
    )
}

/// "Spend limit".
pub fn spend_limit_topic() -> HelpTopic {
    topic(
        SPEND_LIMIT_TOPIC_ID,
        "Spend limit",
        &[
            "Optional. The inspection stops between stages once its spend reaches this amount. \
             It says what did not run and marks the result incomplete. Blank means no limit.",
            "Work already started finishes, so a run can end about one stage over the limit. The \
             live cost panel shows the spend against the limit. Approve and continue lets you \
             go on from a pause.",
        ],
    )
}

/// "The cost estimate", for the finder in effect (and, for the Engine finder, the backend).
pub fn estimate_topic(finder: ProjectFinder, backend: ProjectBackend) -> HelpTopic {
    match finder {
        ProjectFinder::Agent => topic(
            ESTIMATE_TOPIC_ID,
            "The cost estimate",
            &[
                "The figure is for the finder stage only. Group review, fix and duplicate check \
                 are extra and are not in it. It is an estimate, not a quote.",
                "It is built from the size of each repository: its pieces and its lines of code, \
                 at fixed rates taken from earlier runs. Those rates are not priced for the Finder \
                 model you pick and do not change with it, so a model that costs more or less \
                 than the one the rates came from makes the real cost differ. A repository cut into two or more pieces also gets one joints \
                 call, which looks across the pieces.",
                "When the sizes cannot be read, the screen says the estimate is not available \
                 instead of showing a figure it cannot stand behind.",
            ],
        ),
        ProjectFinder::Engine => {
            let backend_line = match backend {
                ProjectBackend::Cli => {
                    "It is priced for the CLI backend. Each call is a separate subprocess with \
                     its own fresh prompt cache, so there is no cross-call caching discount to \
                     expect. Switching to the API backend, with a key configured, usually costs \
                     less for the same inspection."
                }
                _ => {
                    "Prompt caching across batches keeps the input side down, so the real bill \
                     can land below this figure."
                }
            };
            topic(
                ESTIMATE_TOPIC_ID,
                "The cost estimate",
                &[
                    "The figure is approximate. Output tokens, the problems found, dominate an \
                     inspection that finds a lot.",
                    backend_line,
                    "The deterministic security floor runs free. An incremental inspection bills \
                     only the files that changed since the last one, so it usually costs well \
                     below a whole-repository figure. Tick Full inspection to inspect everything. \
                     The deep tier, when it is on, adds about three extra whole-repository \
                     passes and dominates the figure.",
                ],
            )
        }
    }
}

/// "Backend": where this project's AI calls run, and what each choice needs.
pub fn backend_topic() -> HelpTopic {
    topic(
        BACKEND_TOPIC_ID,
        "Backend",
        &[
            "The backend decides where every AI call this project makes runs: the inspection, \
             alternative recommendations, rescans and the governed dev loop.",
            "CLI runs on your Claude Code subscription and needs no key. API runs on the \
             Anthropic API and needs a saved Anthropic API key. It is the compliant, metered \
             chain for client code.",
            "If the API is chosen and no key is saved, the AI steps on this project do not run \
             until one is saved. The deterministic checks still run.",
        ],
    )
}

/// Every topic this file owns (the Engine and Agent forms of the two that differ).
pub fn topics() -> Vec<HelpTopic> {
    let mut all = vec![
        checks_topic(),
        run_mode_topic(),
        full_inspection_topic(),
        spend_limit_topic(),
        backend_topic(),
    ];
    for finder in [ProjectFinder::Agent, ProjectFinder::Engine] {
        all.push(inspect_topic(finder));
        for backend in [ProjectBackend::Cli, ProjectBackend::Api] {
            all.push(estimate_topic(finder, backend));
        }
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_topic_has_a_title_a_body_and_the_button_name_the_standard_asks_for() {
        for t in topics() {
            assert!(!t.title.is_empty() && !t.body.is_empty(), "{}", t.id);
        }
        assert_eq!(checks_topic().button_label(), "About Checks to run");
        assert_eq!(run_mode_topic().button_label(), "About Run mode");
        assert_eq!(
            full_inspection_topic().button_label(),
            "About Full inspection"
        );
        assert_eq!(spend_limit_topic().button_label(), "About Spend limit");
        assert_eq!(
            inspect_topic(ProjectFinder::Agent).button_label(),
            "About This run"
        );
    }

    #[test]
    fn no_topic_has_a_dash_a_serial_comma_or_the_retired_verb() {
        for t in topics() {
            let text = format!("{} {}", t.title, t.body);
            assert!(!text.contains(['\u{2013}', '\u{2014}']), "{text}");
            assert!(!text.contains(", and "), "{text}");
            assert!(!text.to_lowercase().contains("audit"), "{text}");
        }
    }

    #[test]
    fn the_agent_topics_describe_the_agent_finder_and_never_the_engines_passes_or_batches() {
        let run = inspect_topic(ProjectFinder::Agent).body;
        assert!(
            run.contains("in pieces") && run.contains("joints calls"),
            "{run}"
        );
        assert!(run.contains("not batched"), "{run}");
        let est = estimate_topic(ProjectFinder::Agent, ProjectBackend::Cli).body;
        assert!(
            est.contains("finder stage only") && est.contains("extra"),
            "{est}"
        );
        assert!(!est.contains("tokens") && !est.contains("passes"), "{est}");
        // The estimate does not price the picked model, and the text says so.
        assert!(
            est.contains("not priced for the Finder model you pick"),
            "{est}"
        );
    }

    #[test]
    fn the_floor_sentence_matches_checks_to_run_and_no_range_is_promised() {
        let engine = inspect_topic(ProjectFinder::Engine).body;
        assert!(!engine.contains("always run"), "{engine}");
        assert!(
            engine.contains("unless Checks to run is set to AI review only"),
            "{engine}"
        );
        let est = estimate_topic(ProjectFinder::Agent, ProjectBackend::Cli).body;
        assert!(!est.contains("0.6") && !est.contains("most"), "{est}");
    }

    #[test]
    fn the_engine_estimate_text_follows_the_backend() {
        let cli = estimate_topic(ProjectFinder::Engine, ProjectBackend::Cli).body;
        let api = estimate_topic(ProjectFinder::Engine, ProjectBackend::Api).body;
        assert!(cli.contains("CLI backend") && !api.contains("CLI backend"));
        assert!(api.contains("Prompt caching"));
    }
}
