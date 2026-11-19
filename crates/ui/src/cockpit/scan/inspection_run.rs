//! The Inspection screen's "This run" block and its estimate (settings overhaul, work order C).
//!
//! Props-only components, so the screen and the gallery draw the same markup. Every judgment
//! (which options exist, what each sends, the estimate's words) is a pure function in
//! `camerata_ui_core::scan::inspection_run`; the long explanations are help topics in
//! `camerata_ui_core::help::inspection`. This file chooses markup, and each control keeps at most
//! one short line where the operator needs it to act.

use camerata_api_types::project::{ProjectBackend, ProjectFinder};
use camerata_ui_core::help::inspection as help_text;
use camerata_ui_core::scan::inspection_run::{
    full_inspection, ChecksToRun, AGENT_FULL_INSPECTION_LINE, RUN_MODES, RUN_MODE_INLINE_LINE,
};
use dioxus::prelude::*;

use crate::cockpit::help::HelpButton;
use crate::cockpit::{ProjectView, ScanReportView};

use super::{AuditModelsResp, RepoSizeView};

/// The open project as the Inspection settings last left it, so the run block and the estimate
/// follow a change of finder or backend without a remount. `None` until a setting changes: the
/// screen then reads the project it fetched itself.
#[derive(Clone, Copy)]
pub(crate) struct LiveProject(pub Signal<Option<ProjectView>>);

/// The html ids the tests and the walkthrough find the controls by.
pub(crate) const RUN_BLOCK_ID: &str = "inspection-run";
pub(crate) const CHECKS_ID: &str = "inspection-checks";
pub(crate) const RUN_MODE_ID: &str = "inspection-run-mode";
pub(crate) const FULL_ID: &str = "inspection-full";
pub(crate) const SPEND_LIMIT_ID: &str = "inspection-spend-limit";
pub(crate) const ESTIMATE_ID: &str = "inspection-estimate";

/// The controls of one run, drawn from props.
#[derive(Props, Clone, PartialEq)]
pub(crate) struct RunControlsProps {
    pub finder: ProjectFinder,
    pub checks: ChecksToRun,
    pub on_checks: EventHandler<ChecksToRun>,
    /// Run mode (Engine finder only): the value and the one the screen recommends.
    pub run_mode: String,
    pub recommended_mode: String,
    pub on_run_mode: EventHandler<String>,
    /// What the Full inspection box holds before the finder forces it.
    pub full_ticked: bool,
    pub on_full: EventHandler<bool>,
    pub spend_limit: String,
    pub on_spend_limit: EventHandler<String>,
    /// The deep compliance tier: shown only when its feature flag is on and the run uses a model.
    pub deep_available: bool,
    pub deep: bool,
    pub on_deep: EventHandler<bool>,
    /// A run is in flight: every control is locked.
    pub busy: bool,
}

#[component]
pub(crate) fn InspectionRunControls(props: RunControlsProps) -> Element {
    let full = full_inspection(props.finder, props.full_ticked);
    let engine = props.finder == ProjectFinder::Engine;
    let checks_value = props.checks.value();
    let on_checks = props.on_checks;
    let on_mode = props.on_run_mode;
    let on_full = props.on_full;
    let on_spend = props.on_spend_limit;
    let on_deep = props.on_deep;
    rsx! {
        div { id: RUN_BLOCK_ID, class: "audit-run",
            p { class: "audit-run-h",
                strong { "This run" }
                HelpButton { topic: help_text::inspect_topic(props.finder) }
            }
            // Checks to run: one select, the same two request flags as before.
            div { class: "audit-model-row",
                label { class: "audit-model-label", r#for: CHECKS_ID, "Checks to run" }
                div { class: "audit-run-control",
                    select {
                        id: CHECKS_ID,
                        class: "audit-model-select",
                        disabled: props.busy,
                        value: "{checks_value}",
                        onchange: move |e| on_checks.call(ChecksToRun::from_value(&e.value())),
                        for option in ChecksToRun::OPTIONS {
                            option { value: option.value(), "{option.label()}" }
                        }
                    }
                    HelpButton { topic: help_text::checks_topic() }
                }
                if let Some(line) = props.checks.inline_line() {
                    span { class: "audit-model-hint", "{line}" }
                }
            }
            // Run mode: the Engine finder's way of calling the model. The Agent finder has none.
            if engine {
                div { class: "audit-model-row",
                    label { class: "audit-model-label", r#for: RUN_MODE_ID, "Run mode" }
                    div { class: "audit-run-control",
                        select {
                            id: RUN_MODE_ID,
                            class: "audit-model-select",
                            disabled: props.busy,
                            value: "{props.run_mode}",
                            onchange: move |e| on_mode.call(e.value()),
                            for (value , label) in RUN_MODES {
                                option { value, "{label}" }
                            }
                        }
                        if props.run_mode == props.recommended_mode {
                            span { class: "audit-mode-rec", "\u{2713} picked for this inspection's size" }
                        }
                        HelpButton { topic: help_text::run_mode_topic() }
                    }
                    span { class: "audit-model-hint", "{RUN_MODE_INLINE_LINE}" }
                }
            }
            // Full inspection: ticked and locked for the Agent finder.
            div { class: "audit-model-row",
                label { class: "audit-model-label", r#for: FULL_ID, "Full inspection" }
                div { class: "audit-run-control",
                    label { class: "audit-thorough-toggle",
                        input {
                            id: FULL_ID,
                            r#type: "checkbox",
                            checked: full.checked,
                            disabled: props.busy || full.locked,
                            onchange: move |e| on_full.call(e.checked()),
                        }
                        span { "Inspect every file" }
                    }
                    HelpButton { topic: help_text::full_inspection_topic() }
                }
                if full.locked {
                    span { class: "audit-model-hint", "{AGENT_FULL_INSPECTION_LINE}" }
                }
            }
            // Spend limit.
            div {
                class: "audit-model-row",
                "data-tour": camerata_ui_core::walkthrough::target::SPEND_LIMIT,
                label { class: "audit-model-label", r#for: SPEND_LIMIT_ID, "Spend limit (USD)" }
                div { class: "audit-run-control",
                    input {
                        id: SPEND_LIMIT_ID,
                        r#type: "text",
                        placeholder: "no limit",
                        value: "{props.spend_limit}",
                        disabled: props.busy,
                        oninput: move |e| on_spend.call(e.value()),
                    }
                    HelpButton { topic: help_text::spend_limit_topic() }
                }
            }
            // Deep compliance and security: opt-in, advisory and the most expensive tier.
            if props.deep_available {
                div { class: "audit-model-row",
                    label { class: "audit-model-label", "Deep compliance and security (opt-in)" }
                    label { class: "audit-thorough-toggle",
                        input {
                            r#type: "checkbox",
                            checked: props.deep,
                            disabled: props.busy,
                            onchange: move |e| on_deep.call(e.checked()),
                        }
                        span { "Run SOC-2 gap analysis, deep security review and threat model" }
                    }
                    span { class: "audit-model-hint deep-tier-warning",
                        "Advisory only, not a SOC-2 report and not a penetration test. Three extra whole-repo passes make it the most expensive tier."
                    }
                }
            }
        }
    }
}

/// What the estimate block shows. The screen decides which; this file only draws it.
#[derive(Clone, PartialEq)]
pub(crate) enum EstimateBlock {
    /// The Engine finder's figure.
    Engine {
        dollars: String,
        meta: String,
        repo_lines: Vec<String>,
        scope_line: &'static str,
        deep_on: bool,
        finder_backend: (ProjectFinder, ProjectBackend),
    },
    /// The Agent finder's figure.
    Agent {
        text: camerata_ui_core::scan::inspection_run::AgentEstimateText,
        finder_backend: (ProjectFinder, ProjectBackend),
    },
    /// The Agent finder has no figure: the sentence says so and why.
    AgentUnavailable {
        sentence: String,
        finder_backend: (ProjectFinder, ProjectBackend),
    },
    /// Deterministic checks only: no model is called.
    DeterministicOnly,
}

/// The estimate, before a run. After a run the screen shows the actual cost instead.
#[component]
pub(crate) fn InspectionEstimate(block: EstimateBlock) -> Element {
    rsx! {
        div { id: ESTIMATE_ID, class: "audit-cost",
            match block {
                EstimateBlock::Engine { dollars, meta, repo_lines, scope_line, deep_on, finder_backend } => rsx! {
                    div { class: "audit-cost-main",
                        span { class: "audit-cost-label", "Estimated cost" }
                        span { class: "audit-cost-val", "{dollars}" }
                        span { class: "audit-cost-meta", "{meta}" }
                        HelpButton { topic: help_text::estimate_topic(finder_backend.0, finder_backend.1) }
                    }
                    if !repo_lines.is_empty() {
                        ul { class: "audit-cost-repos",
                            for line in repo_lines.iter() {
                                li { key: "{line}", "{line}" }
                            }
                        }
                    }
                    p { class: "audit-cost-note",
                        "{scope_line}"
                        if deep_on {
                            span { class: "audit-cost-deep-note", " The deep tier is on and included." }
                        }
                    }
                },
                EstimateBlock::Agent { text, finder_backend } => rsx! {
                    div { class: "audit-cost-main",
                        span { class: "audit-cost-label", "{text.label}" }
                        span { class: "audit-cost-val", "{text.value}" }
                        span { class: "audit-cost-meta", "{text.calls}" }
                        HelpButton { topic: help_text::estimate_topic(finder_backend.0, finder_backend.1) }
                    }
                    p { class: "audit-cost-range", "{text.range}" }
                    if !text.repo_lines.is_empty() {
                        ul { class: "audit-cost-repos",
                            for line in text.repo_lines.iter() {
                                li { key: "{line}", "{line}" }
                            }
                        }
                    }
                    p { class: "audit-cost-note", "{text.extra}" }
                },
                EstimateBlock::AgentUnavailable { sentence, finder_backend } => rsx! {
                    div { class: "audit-cost-main",
                        span { class: "audit-cost-unavailable", role: "status", "{sentence}" }
                        HelpButton { topic: help_text::estimate_topic(finder_backend.0, finder_backend.1) }
                    }
                },
                EstimateBlock::DeterministicOnly => rsx! {
                    div { class: "audit-cost-main",
                        span { class: "audit-cost-label", "Estimated cost" }
                        span { class: "audit-cost-val", "{camerata_ui_core::scan::inspection_run::DETERMINISTIC_ONLY_ESTIMATE}" }
                    }
                },
            }
        }
    }
}

/// An estimate ready to show, and the short form "est. was ..." uses once the real cost is in.
#[derive(Clone, PartialEq)]
pub(crate) struct EstimateShown {
    pub block: EstimateBlock,
    pub short: String,
}

impl EstimateShown {
    /// Deterministic checks only: nothing is called, nothing is spent.
    pub fn deterministic() -> Self {
        Self {
            block: EstimateBlock::DeterministicOnly,
            short: "$0".to_string(),
        }
    }
}

/// The repository sizes as the screen holds them: not back yet (`None`), back but the request
/// failed (`Some(None)`), or back (`Some(Some(..))`).
pub(in crate::cockpit) type SizesState = Option<Option<Vec<RepoSizeView>>>;

/// The Agent finder's estimate from the repository sizes. Never the Engine's figure: with no
/// sizes the answer is the sentence that the estimate is not available, with the reason.
pub(in crate::cockpit) fn agent_estimate_block(
    sizes: &SizesState,
    backend: ProjectBackend,
) -> EstimateShown {
    use camerata_ui_core::scan::inspection_run::{
        agent_estimate_unavailable, agent_estimate_view, AgentEstimateView, AgentRepoFigure,
    };
    let finder_backend = (ProjectFinder::Agent, backend);
    let unavailable = |reason: &str| EstimateShown {
        block: EstimateBlock::AgentUnavailable {
            sentence: agent_estimate_unavailable(reason),
            finder_backend,
        },
        short: "not available".to_string(),
    };
    let Some(answer) = sizes else {
        return unavailable("The repository sizes are still loading.");
    };
    let Some(sizes) = answer else {
        return unavailable("The repository sizes could not be read.");
    };
    let mut figures = Vec::new();
    let mut unreadable = Vec::new();
    for z in sizes {
        match (&z.error, &z.agent_finder) {
            (None, Some(a)) => figures.push(AgentRepoFigure {
                repo: z.repo.clone(),
                pieces: a.pieces,
                joints_calls: a.joints_calls,
                expected_usd: a.expected_usd,
                low_usd: a.low_usd,
                high_usd: a.high_usd,
            }),
            (Some(why), _) => unreadable.push((z.repo.clone(), why.clone())),
            (None, None) => {
                unreadable.push((z.repo.clone(), "no figure came back for it".to_string()))
            }
        }
    }
    match agent_estimate_view(&figures, &unreadable) {
        AgentEstimateView::Figure(text) => EstimateShown {
            short: text.value.clone(),
            block: EstimateBlock::Agent {
                text,
                finder_backend,
            },
        },
        AgentEstimateView::Unavailable(sentence) => EstimateShown {
            block: EstimateBlock::AgentUnavailable {
                sentence,
                finder_backend,
            },
            short: "not available".to_string(),
        },
    }
}

/// The Engine finder's estimate: the one run estimate the command line's pre-flight prints from
/// (`camerata_ui_core::scan::cockpit_run_estimate`), over the inputs the screen has. The models
/// priced are the project's own (Inspection settings), the ones the run will use.
#[allow(clippy::too_many_arguments)]
pub(in crate::cockpit) fn engine_estimate_block(
    report: &ScanReportView,
    models: Option<&AuditModelsResp>,
    project: Option<&ProjectView>,
    sizes: &SizesState,
    repo_selection: &std::collections::HashMap<String, Vec<String>>,
    selected: usize,
    run_mode: &str,
    backend: ProjectBackend,
    incremental: bool,
    deep_on: bool,
) -> EstimateShown {
    use camerata_ui_core::scan::{
        cockpit_run_estimate, estimate_mode, repo_estimate_line, unknown_rules_repo_line,
        unreadable_repo_line, PricedModel, RunFlags, RunRepoInput,
    };
    let price = |id: &str, fallback: (f64, f64)| {
        models
            .and_then(|m| {
                m.models
                    .iter()
                    .find(|o| o.id == id)
                    .map(|o| (o.price_in, o.price_out))
            })
            .unwrap_or(fallback)
    };
    let stored = |chosen: Option<&str>, default: &str| -> String {
        chosen
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .unwrap_or(default)
            .to_string()
    };
    // The models the run uses: the project's Scan model and Calibration model, else the
    // built-in defaults the route stores for a new project.
    let scan_id = stored(
        project.map(|p| p.step_models.audit.as_str()),
        camerata_api_types::project::DEFAULT_MODEL,
    );
    let calibration_id = stored(
        project.map(|p| p.step_models.calibration.as_str()),
        camerata_api_types::project::STAGE1_DEFAULT_MODEL,
    );
    let (a_in, a_out) = price(&scan_id, (3.0, 15.0));
    let (c_in, c_out) = price(&calibration_id, (a_in, a_out));
    // The fix stage runs on its default model (the server's `fix_on_haiku` switch is on by
    // default); priced when the model list prices it.
    let fix_model_id = camerata_api_types::project::FIX_STAGE_HAIKU_MODEL;
    let fix_price = models.and_then(|m| {
        m.models
            .iter()
            .find(|o| o.id == fix_model_id)
            .map(|o| (o.price_in, o.price_out))
    });
    // The group reviewer runs on the project's group review model, else its built-in default.
    let group_review_id = stored(
        project.map(|p| p.review.group_review_model.as_str()),
        camerata_api_types::project::STAGE2_DEFAULT_MODEL,
    );
    let group_review_price = models.and_then(|m| {
        m.models
            .iter()
            .find(|o| o.id == group_review_id)
            .map(|o| (o.price_in, o.price_out))
    });
    let multi_option_selected = report
        .proposed_rules
        .iter()
        .filter(|r| r.options.len() >= 2)
        .count();
    let est_mode = estimate_mode(run_mode, backend).to_string();
    fn priced(id: &str, price_in: f64, price_out: f64) -> PricedModel<'_> {
        PricedModel {
            id,
            price_in,
            price_out,
        }
    }
    let flags = RunFlags {
        deep: deep_on,
        backend,
        terse_output: true,
    };
    let run_estimate = |inputs: &[RunRepoInput]| {
        cockpit_run_estimate(
            inputs,
            &est_mode,
            priced(&scan_id, a_in, a_out),
            priced(&calibration_id, c_in, c_out),
            group_review_price.map(|(i, o)| priced(&group_review_id, i, o)),
            fix_price.map(|(i, o)| priced(fix_model_id, i, o)),
            flags,
        )
    };
    // Several repos: one estimate per repo, summed (each repo is chunked and calibrated on its
    // own). One repo, or sizes not back yet: the one-stream estimate.
    let all_sizes: Option<Vec<RepoSizeView>> = sizes.clone().flatten();
    let multi_repo = report.repos.len() > 1;
    let sized: Option<Vec<RepoSizeView>> = if multi_repo { all_sizes } else { None };
    let mut repo_lines: Vec<String> = Vec::new();
    let (tokens, dollars, passes) = match sized {
        Some(sizes) => {
            // A repo is priced only when its rule choice is known: a guessed rule count would
            // put a made-up figure in front of a spend decision.
            let inputs: Vec<RunRepoInput> = sizes
                .iter()
                .filter(|z| z.error.is_none())
                .filter_map(|z| {
                    let ids = repo_selection.get(&z.repo)?;
                    Some(RunRepoInput {
                        repo: z.repo.clone(),
                        code_chars: z.code_chars,
                        files: z.files,
                        selected: ids.len(),
                        multi_option_selected: report
                            .proposed_rules
                            .iter()
                            .filter(|r| r.options.len() >= 2 && ids.contains(&r.id))
                            .count(),
                        routing_groups: 1,
                    })
                })
                .collect();
            let est = run_estimate(&inputs);
            for z in sizes.iter() {
                match (&z.error, est.rows.iter().find(|r| r.repo == z.repo)) {
                    (Some(why), _) => repo_lines.push(unreadable_repo_line(&z.repo, why)),
                    (None, Some(row)) => {
                        repo_lines.push(repo_estimate_line(&z.repo, z.files, z.code_chars, row))
                    }
                    (None, None) => repo_lines.push(unknown_rules_repo_line(&z.repo)),
                }
            }
            (est.tokens, est.dollars, est.passes)
        }
        None => {
            let est = run_estimate(&[RunRepoInput {
                repo: report.repos.first().cloned().unwrap_or_default(),
                code_chars: report.code_chars,
                files: report.files_scanned,
                selected,
                multi_option_selected,
                routing_groups: 1,
            }]);
            (est.tokens, est.dollars, est.passes)
        }
    };
    let dollar_str = if dollars < 0.01 {
        "<$0.01".to_string()
    } else {
        format!("~${dollars:.2}")
    };
    EstimateShown {
        short: dollar_str.clone(),
        block: EstimateBlock::Engine {
            dollars: dollar_str,
            meta: format!(
                "~{} tokens \u{b7} {passes} pass(es) \u{b7} {selected} rule(s)",
                camerata_ui_core::scan::human_tokens(tokens)
            ),
            repo_lines,
            scope_line: camerata_ui_core::scan::inspection_run::engine_scope_line(incremental),
            deep_on,
            finder_backend: (ProjectFinder::Engine, backend),
        },
    }
}
