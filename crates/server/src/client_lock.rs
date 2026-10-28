//! The client-engagement lock: client work runs only on the Anthropic commercial API, and a
//! client's own tooling (code that builds or runs the client's repository) is never executed
//! unless the operator opted in for that engagement.
//!
//! # What a client engagement is
//!
//! A flag on the inspection setup: `Project::client_engagement` in the app (on for every new,
//! imported or legacy project until the operator turns it off), and `camerata inspect`'s default
//! (turned off only by `--internal`, the maintainer's own development). It is the one input every
//! decision here takes.
//!
//! # The backend lock (MASTER-SEQUENCE 22a, hardening H2 item 1)
//!
//! A client engagement refuses, before any file is read or any model is called:
//! - the `cli` backend (the operator's personal Claude Code subscription, consumer terms), and
//! - any model that is not a first-party Anthropic API model, which is how an OpenRouter model
//!   (`deepseek/deepseek-chat`, `anthropic/claude-sonnet-4`, `qwen/qwen3-coder:free`) would
//!   reach client code.
//!
//! The check is an allowlist on the model id's SHAPE ([`is_anthropic_api_model`]), not a lookup
//! in the model registry: the registry only knows the OpenRouter catalog once it has been
//! fetched, so a registry-based check would let an OpenRouter id through whenever that cache is
//! empty. Every Anthropic API model id starts with `claude-` and carries no `/` or `:`; every
//! OpenRouter id carries a `/` (vendor prefix).
//!
//! # The tooling lock (hardening H2 item 2)
//!
//! [`client_tooling_allowed`] decides whether a step that EXECUTES the client's code may run.
//! Today two such steps exist: the `cargo clippy` scan preview (it compiles the crate, which runs
//! the repository's `build.rs` scripts and procedural macros and honours its
//! `.cargo/config.toml`, including a `rustc-wrapper`), and the governed dev loop's layer-2 checks
//! (`cargo test`, `npm` scripts, `.camerata/checks.toml` shell commands). Every other inspection
//! step reads the repository as data. See `scan_tools::ScanTool::executes_repo_code`.

use serde::{Deserialize, Serialize};

use crate::llm::ProjectBackend;

/// True when `model` is a first-party Anthropic API model id (`claude-sonnet-5`,
/// `claude-haiku-4-5-20251001`). False for every OpenRouter id (`vendor/model`, `vendor/model:free`)
/// and for an empty id. See the module docs for why this is an allowlist on the id's shape.
pub fn is_anthropic_api_model(model: &str) -> bool {
    let model = model.trim();
    model.starts_with("claude-") && !model.contains('/') && !model.contains(':')
}

/// The reason a client engagement must not run with `backend` and `models` (each a
/// `(role, model id)` pair, e.g. from [`crate::cost_cap::models_the_run_will_use`]), or `None`
/// when it may. A run that is not a client engagement is never refused here.
///
/// The text names what was refused and why; callers append the fix for their own surface (a
/// command-line flag, an app setting).
pub fn refusal(
    client_engagement: bool,
    backend: ProjectBackend,
    models: &[(String, String)],
) -> Option<String> {
    if !client_engagement {
        return None;
    }
    if backend == ProjectBackend::Cli {
        return Some(
            "This is a client engagement, so it runs only on the Anthropic API under Anthropic's \
             commercial terms. The `cli` backend (the Claude Code subscription) is refused for \
             client work. Nothing was read and no model was called."
                .to_string(),
        );
    }
    let refused: Vec<String> = models
        .iter()
        .filter(|(_, model)| !is_anthropic_api_model(model))
        .map(|(role, model)| format!("{role} is set to `{model}`"))
        .collect();
    if refused.is_empty() {
        return None;
    }
    Some(format!(
        "This is a client engagement, so every model must be an Anthropic API model (an id \
         starting with `claude-`). OpenRouter and other providers are refused for client work: {}. \
         Nothing was read and no model was called.",
        refused.join("; ")
    ))
}

/// Every model a scan may use, by role, for the lock to check BEFORE the scan starts: the scan,
/// calibration and fix generation passes, the deep tier when it is on, and the clone judge
/// (always included, because which rules are selected is only known once files have been read,
/// and the lock must decide before that). Each is resolved exactly as the run resolves it,
/// environment overrides included.
pub fn models_a_scan_may_use(
    model: Option<&str>,
    calibration_model: Option<&str>,
    deep: bool,
) -> Vec<(String, String)> {
    crate::cost_cap::models_the_run_will_use(
        model,
        calibration_model,
        deep,
        camerata_checks::clone_rules::RULE_IDS,
    )
}

/// Whether a step that executes the client's own code (builds it, runs its scripts or tests)
/// may run: always outside a client engagement, and inside one only with the operator's
/// explicit opt-in for that engagement.
pub fn client_tooling_allowed(client_engagement: bool, client_tooling_opt_in: bool) -> bool {
    !client_engagement || client_tooling_opt_in
}

/// The client-tooling decision a scan carries into its preview pass, its ledger and its report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ClientTooling {
    /// The run is a client engagement.
    pub client_engagement: bool,
    /// The operator opted in to running the client's own tooling for this engagement.
    pub opt_in: bool,
}

impl ClientTooling {
    /// Not a client engagement: the client's tooling may run (the maintainer's own development,
    /// and every caller that predates the lock).
    pub const INTERNAL: Self = Self {
        client_engagement: false,
        opt_in: false,
    };

    pub fn new(client_engagement: bool, opt_in: bool) -> Self {
        Self {
            client_engagement,
            opt_in,
        }
    }

    /// See [`client_tooling_allowed`].
    pub fn allowed(self) -> bool {
        client_tooling_allowed(self.client_engagement, self.opt_in)
    }
}

/// One model a run used, by role (`the scan`, `calibration`, ...).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRole {
    pub role: String,
    pub model: String,
}

/// What the scan ledger records about the engagement a run was made under: whether it was a
/// client engagement, the backend and provider, every model by role, and the client-tooling
/// decision. Operator-facing; the export carries the backend, provider and models through
/// [`crate::ModelRecord`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngagementRecord {
    pub client_engagement: bool,
    /// `ProjectBackend::provenance_name()`: `api` or `cli`.
    pub backend: String,
    pub provider: String,
    pub models: Vec<ModelRole>,
    /// True when the operator opted in to running the client's own tooling.
    pub client_tooling_opt_in: bool,
    /// True when a step that executes the client's code was allowed to run (not a client
    /// engagement, or opted in).
    pub client_tooling_allowed: bool,
}

impl EngagementRecord {
    /// The record for a scan whose model provenance is `model` (the export's
    /// [`crate::ModelRecord`], with the models that ran already filled in).
    pub fn new(model: &crate::ModelRecord, tooling: ClientTooling) -> Self {
        Self {
            client_engagement: tooling.client_engagement,
            backend: model.backend.clone(),
            provider: model.provider.clone(),
            models: model.models.clone(),
            client_tooling_opt_in: tooling.opt_in,
            client_tooling_allowed: tooling.allowed(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn models(ids: &[(&str, &str)]) -> Vec<(String, String)> {
        ids.iter()
            .map(|(r, m)| (r.to_string(), m.to_string()))
            .collect()
    }

    #[test]
    fn anthropic_api_ids_pass_and_openrouter_ids_do_not() {
        for ok in [
            "claude-sonnet-5",
            "claude-haiku-4-5-20251001",
            " claude-opus-5-5 ",
        ] {
            assert!(is_anthropic_api_model(ok), "{ok}");
        }
        for refused in [
            "deepseek/deepseek-chat",
            "anthropic/claude-sonnet-4",
            "qwen/qwen3-coder:free",
            "claude-sonnet-5:free",
            "gpt-4o",
            "",
        ] {
            assert!(!is_anthropic_api_model(refused), "{refused}");
        }
    }

    #[test]
    fn a_client_engagement_on_the_cli_backend_is_refused() {
        let why = refusal(
            true,
            ProjectBackend::Cli,
            &models(&[("the scan", "claude-sonnet-5")]),
        )
        .expect("cli must be refused for a client engagement");
        assert!(why.contains("`cli` backend"), "{why}");
        assert!(why.contains("Nothing was read"), "{why}");
    }

    #[test]
    fn a_client_engagement_with_an_openrouter_model_is_refused_naming_it() {
        let why = refusal(
            true,
            ProjectBackend::Api,
            &models(&[
                ("the scan", "claude-sonnet-5"),
                ("calibration", "deepseek/deepseek-chat"),
            ]),
        )
        .expect("an OpenRouter model must be refused for a client engagement");
        assert!(
            why.contains("calibration is set to `deepseek/deepseek-chat`"),
            "{why}"
        );
        assert!(!why.contains("the scan is set"), "{why}");
    }

    #[test]
    fn a_client_engagement_on_the_api_with_claude_models_passes() {
        assert_eq!(
            refusal(
                true,
                ProjectBackend::Api,
                &models(&[
                    ("the scan", "claude-sonnet-5"),
                    ("calibration", "claude-opus-5")
                ]),
            ),
            None
        );
    }

    #[test]
    fn internal_development_keeps_the_cli_and_any_model() {
        assert_eq!(
            refusal(
                false,
                ProjectBackend::Cli,
                &models(&[("the scan", "deepseek/deepseek-chat")]),
            ),
            None
        );
    }

    #[test]
    fn client_tooling_runs_outside_an_engagement_or_with_an_opt_in_only() {
        assert!(client_tooling_allowed(false, false));
        assert!(client_tooling_allowed(true, true));
        assert!(!client_tooling_allowed(true, false));
        assert!(ClientTooling::INTERNAL.allowed());
        assert!(!ClientTooling::new(true, false).allowed());
    }

    #[test]
    fn the_engagement_record_states_backend_models_and_the_tooling_decision() {
        let mut model = crate::ModelRecord::anthropic(ProjectBackend::Api);
        model.models = vec![ModelRole {
            role: "the scan".to_string(),
            model: "claude-sonnet-5".to_string(),
        }];
        let r = EngagementRecord::new(&model, ClientTooling::new(true, false));
        assert!(r.client_engagement);
        assert_eq!(r.backend, "api");
        assert_eq!(r.provider, "anthropic");
        assert_eq!(r.models[0].model, "claude-sonnet-5");
        assert!(!r.client_tooling_opt_in);
        assert!(!r.client_tooling_allowed);
    }
}
