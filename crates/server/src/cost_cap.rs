//! The run-wide spend cap (`camerata inspect --max-cost-usd`) and the controls that travel with a
//! run.
//!
//! # What the cap is
//!
//! A guard against a runaway bill (a 200K-line repo with a mis-set rule set), not a saving.
//! It must therefore fail LOUDLY: a run that stops early is an incomplete run, and an
//! incomplete run says so everywhere it can (the failed-pass list, the operator ledger, the
//! export manifest, the exit code). It never drops or invents a finding.
//!
//! # How it stops
//!
//! * **Between passes, never mid-response.** Before a (chunk, rule-batch) pass starts, the run
//!   asks [`crate::ai_audit::UsageMeter::admit`]. A pass that was admitted always finishes, its
//!   truncation retry included. A pass that was refused is recorded as [`NotRunPass`] and
//!   reported as NOT RUN.
//! * **Later stages** (alternative recommendation, calibration, fix generation, the clone judge,
//!   the deep tier) check the cap before they start
//!   ([`crate::ai_audit::UsageMeter::stage_blocked`]) and are recorded as [`NotRunStage`]. A
//!   stage is blocked once spend reaches the cap AND once the cap has already stopped anything,
//!   so nothing starts after the cap has bitten. Inside calibration, fix generation and the
//!   deep tier each batch or call is admitted separately
//!   ([`crate::ai_audit::UsageMeter::admit_for_stage`]); the remainder of a stage that is
//!   stopped part way is recorded as `partial`, and its findings pass through unchanged.
//! * **Message Batches** cannot be cancelled cheaply once submitted, so the cap is checked
//!   BEFORE a submission, against a projection (see
//!   [`crate::ai_audit::UsageMeter::refuse_batch_submission`]). A batch that was submitted runs
//!   to completion.
//!
//! # The overshoot bound
//!
//! Passes run concurrently, so the money a pass will cost is not known when the next one is
//! admitted. A pass (or a calibration or fix batch, or a deep-tier call) is admitted only while
//! `spent + in_flight * reserve < cap`, where `reserve` is the most any single CALL has cost so
//! far. Until one call has reported a cost there is nothing to reserve against, so exactly one
//! pass is admitted and the others wait for it.
//!
//! If every call costs about the same, the final total is at most **one pass's cost over the
//! cap**: the pass that crosses the line. Three things make it larger, and they are the real
//! bound, not a footnote:
//!
//! * the reserve is per CALL and a pass can be two calls (a reply cut off at the output limit
//!   is retried once at a higher limit, inside the same admitted pass), so a retried pass can
//!   cost about twice the reserve;
//! * a call larger than every call before it adds the difference;
//! * thorough calibration runs three calls per batch under one admission.
//!
//! The first pass of a run is always admitted (nothing has been spent), so a cap smaller than
//! one pass still lets that one pass finish.
//!
//! # What the cap needs from every call
//!
//! A cost. With a cap set, every model the run will call is checked for a list price before
//! anything runs ([`RunControls::refuse_if_cap_unenforceable`]), and every stage's request names
//! its model explicitly ([`crate::ai_audit::stage_model`]) so the model that was checked is the
//! model that is called. A request sent with no model is refused by the transport (it has no
//! default of its own), so a model no check saw can never run. Should a call's cost still be
//! unknowable, the cap is no longer enforceable and admits nothing more; the disclosures then
//! say the cap could not be enforced, not that it was reached
//! ([`CostCapRecord::cause`]).

use serde::{Deserialize, Serialize};

use crate::efficiency::EfficiencyConfig;
pub use camerata_llm::failure::FailureClass;

/// Everything about HOW a run executes that is not WHAT it scans: which efficiency measures are
/// on and how much it may spend. Built once per run, carried on the run's
/// [`crate::ai_audit::UsageMeter`] (which every model-calling stage already receives), so no
/// stage signature grows.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RunControls {
    pub efficiency: EfficiencyConfig,
    /// `--max-cost-usd`. `None` = no cap.
    pub max_cost_usd: Option<f64>,
    /// The pre-flight estimate of the whole run, when the caller computed one. Used ONLY to
    /// decide whether a non-cancellable batch may be submitted (a real-time run is capped on
    /// actual spend alone).
    pub estimate_hint_usd: Option<f64>,
    /// The fix stage's explicit model (the project's fix step model, or
    /// `camerata inspect --fix-model`). `None` or blank = not set: the fix stage then follows the
    /// `fix_on_haiku` measure. See [`crate::ai_audit::resolve_fix_model`].
    pub fix_model: Option<String>,
    /// The clone judge's model: the open project's fast (Haiku) band in the cockpit, or
    /// `camerata inspect --clone-judge-model`. `None` or blank = the built-in default
    /// ([`crate::onboard::clone_advisory::resolve_clone_judge_model`]).
    pub clone_judge_model: Option<String>,
    /// H5: the three-pass consensus option (`crate::consensus`). Off by default: about three
    /// times the AI spend, and a finding only one pass in three reports is dropped.
    pub consensus: bool,
}

impl RunControls {
    /// Defaults plus the environment's efficiency switches; no cap.
    pub fn from_env() -> Self {
        Self {
            efficiency: EfficiencyConfig::from_env_or_default(),
            ..Self::default()
        }
    }

    /// A positive, finite cap, or `Err` with the reason. Zero and negative caps are refused:
    /// a cap of zero would still let the first pass run (see the module docs), which would
    /// read as "spend nothing" and do the opposite.
    pub fn validate_cap(cap: f64) -> Result<f64, String> {
        if cap.is_finite() && cap > 0.0 {
            Ok(cap)
        } else {
            Err(format!(
                "--max-cost-usd must be a positive dollar amount (got {cap})"
            ))
        }
    }

    /// The model this run's clone judge calls: the explicit pick, else the built-in fast default.
    pub fn clone_judge_model_for(&self) -> String {
        crate::onboard::clone_advisory::resolve_clone_judge_model(self.clone_judge_model.as_deref())
    }

    /// The model this run's fix stage calls, given the run's scan and calibration picks:
    /// [`crate::ai_audit::resolve_fix_model`] over this run's explicit fix model and its
    /// `fix_on_haiku` switch.
    pub fn fix_model_for<'a>(
        &'a self,
        model: Option<&'a str>,
        calibration_model: Option<&'a str>,
    ) -> Option<&'a str> {
        crate::ai_audit::resolve_fix_model(
            self.fix_model.as_deref(),
            self.efficiency.fix_on_haiku,
            model,
            calibration_model,
        )
    }
}

/// Every model a run will call, with the role it plays, resolved exactly as the stages resolve
/// them: the scan model, the calibration model, the fix model, the deep tier's model (only when
/// the deep tier is on) and the clone judge (only when a clone rule is selected). Roles that
/// resolve to the same model appear once, with their roles joined.
pub fn models_the_run_will_use(
    model: Option<&str>,
    calibration_model: Option<&str>,
    deep: bool,
    selected_rule_ids: &[&str],
    controls: &RunControls,
) -> Vec<(String, String)> {
    // The SAME resolutions the stages apply to their requests (`crate::ai_audit::stage_model`,
    // `resolve_calibration_model`), so the models listed, priced and printed here are the models
    // the run calls. The alternatives recommendation, the detail step and the truncation retries
    // run on the scan model.
    let scan = crate::ai_audit::resolve_audit_model(model);
    let calibration = crate::ai_audit::resolve_calibration_model(calibration_model, &scan);
    let fix =
        crate::ai_audit::resolve_audit_model(controls.fix_model_for(model, calibration_model));
    let mut roles: Vec<(&str, String)> = vec![
        ("the scan", scan.clone()),
        ("calibration", calibration),
        ("fix generation", fix),
    ];
    if deep {
        roles.push(("the deep tier", scan));
    }
    if selected_rule_ids
        .iter()
        .any(|id| camerata_checks::clone_rules::RULE_IDS.contains(id))
    {
        roles.push(("the clone judge", controls.clone_judge_model_for()));
    }
    let mut out: Vec<(String, String)> = Vec::new();
    for (role, model) in roles {
        match out.iter_mut().find(|(_, m)| *m == model) {
            Some((roles, _)) => {
                roles.push_str(", ");
                roles.push_str(role);
            }
            None => out.push((role.to_string(), model)),
        }
    }
    out
}

impl RunControls {
    /// With a spend limit set, every model the run will use must have a list price, or its calls
    /// cannot be costed and the limit would silently count nothing for them. Returns the message
    /// that refuses the run (naming each unpriced model), or `None` when the run may start. With
    /// no limit set this is always `None`: nothing changes for an uncapped run.
    pub fn refuse_if_cap_unenforceable(&self, models: &[(String, String)]) -> Option<String> {
        self.max_cost_usd?;
        let unpriced: Vec<String> = models
            .iter()
            .filter(|(_, model)| !crate::llm::is_priced(model))
            .map(|(roles, model)| format!("`{model}` (used for {roles})"))
            .collect();
        if unpriced.is_empty() {
            return None;
        }
        Some(format!(
            "The spend limit cannot be enforced: {} {} no price in the model registry, so what \
             the calls cost cannot be counted against the limit. Nothing was run. Choose a priced \
             model, or remove the spend limit.",
            unpriced.join(" and "),
            if unpriced.len() == 1 { "has" } else { "have" }
        ))
    }
}

/// One (chunk, rule-batch) scan pass that did not run because the cap was reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotRunPass {
    pub repo: String,
    /// The routing group label (`pass[all]`, `pass[rust]`, `resolution`).
    pub group: String,
    /// 1-based position.
    pub chunk: usize,
    pub n_chunks: usize,
    /// 1-based position.
    pub rule_batch: usize,
    pub n_rule_batches: usize,
    /// The rules that were in this rule-batch and so were not checked against this chunk.
    pub rule_ids: Vec<String>,
    /// The files of the chunk that was not checked (repo-relative paths). A file named here was
    /// not reviewed by this pass, so the incremental cache must not record it as audited.
    #[serde(default)]
    pub files: Vec<String>,
    /// Why this pass did not run: the spend limit, or an outage stop. A ledger written before
    /// the class existed holds only spend-limit refusals, so that is the default.
    #[serde(default = "FailureClass::spend_limit")]
    pub class: FailureClass,
}

/// A stage that did not run, or did not finish, because of the cap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotRunStage {
    pub repo: String,
    /// The name the failed-pass list shows (the same names the stages use when they fail).
    pub stage: String,
    /// True when the stage started and the cap stopped it part way (the remainder did not
    /// run); false when it never started.
    #[serde(default)]
    pub partial: bool,
    /// Why this stage did not run; see [`NotRunPass::class`].
    #[serde(default = "FailureClass::spend_limit")]
    pub class: FailureClass,
}

/// The failed-pass disclosures for passes and stages that did not run, grouped one entry per
/// (repo, group, chunk). `cause` completes "NOT RUN: <cause> before ..." and is the only thing
/// that differs between the spend limit and an outage stop.
pub(crate) fn not_run_failed_passes(
    passes_in: &[NotRunPass],
    stages_in: &[NotRunStage],
    cause: &str,
) -> Vec<crate::ai_audit::FailedPass> {
    let mut out: Vec<crate::ai_audit::FailedPass> = Vec::new();
    // One entry per (repo, group, chunk), listing its rule-batches, so a 200-chunk scan is
    // not 1,600 notes while every (chunk, rule-batch) pair is still named.
    // Passes are recorded in the order they were refused, which depends on scheduling;
    // the disclosure is in position order so it reads the same on every run.
    let mut passes = passes_in.to_vec();
    passes.sort_by(|a, b| {
        (&a.repo, &a.group, a.chunk, a.rule_batch).cmp(&(&b.repo, &b.group, b.chunk, b.rule_batch))
    });
    let mut i = 0;
    while i < passes.len() {
        let first = &passes[i];
        let mut batches = vec![first.rule_batch];
        let mut rules = first.rule_ids.len();
        let mut j = i + 1;
        while j < passes.len()
            && passes[j].repo == first.repo
            && passes[j].group == first.group
            && passes[j].chunk == first.chunk
        {
            batches.push(passes[j].rule_batch);
            rules += passes[j].rule_ids.len();
            j += 1;
        }
        let listed = batches
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        out.push(crate::ai_audit::FailedPass {
            repo: first.repo.clone(),
            pass: format!(
                "scan pass {} chunk {}/{}",
                first.group, first.chunk, first.n_chunks
            ),
            reason: format!(
                "NOT RUN: {cause} before rule-batch {listed} \
                 of {} ran, so {rules} rule check(s) were not made against this part of the \
                 code. This inspection is incomplete.",
                first.n_rule_batches
            ),
            // The files of this chunk were not checked by this pass. Counted so the
            // incremental cache and the completeness check treat the repo as unreviewed.
            files_not_reviewed: first.files.len(),
            class: first.class,
            files: first.files.clone(),
            attempts: Vec::new(),
            scope_note: false,
        });
        i = j;
    }
    for stage in stages_in {
        out.push(crate::ai_audit::FailedPass {
            repo: stage.repo.clone(),
            pass: stage.stage.clone(),
            reason: if stage.partial {
                format!(
                    "NOT RUN (in part): {cause} while this stage was running, so the rest of \
                     it did not run. This inspection is incomplete."
                )
            } else {
                format!(
                    "NOT RUN: {cause} before this stage started. This inspection is incomplete."
                )
            },
            files_not_reviewed: 0,
            class: stage.class,
            files: Vec::new(),
            attempts: Vec::new(),
            scope_note: false,
        });
    }
    out
}

/// How much of a rule's checking the cap left undone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleCoverage {
    /// Every pass that carries the rule ran.
    Complete,
    /// Some passes that carry the rule ran and some did not: it was checked on part of the
    /// code only. Never "clean".
    Partial,
    /// No pass that carries the rule ran.
    NotRun,
}

/// What the cap did, as the operator ledger records it. Dollar figures live HERE and in the CLI
/// output only, never in the client export.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CostCapRecord {
    pub cap_usd: f64,
    /// Actual spend when the report was built.
    pub spent_usd: f64,
    pub not_run_passes: Vec<NotRunPass>,
    pub not_run_stages: Vec<NotRunStage>,
    /// Plain-language operator notes (a refused batch submission and its projection).
    pub notes: Vec<String>,
    /// The repos the cap left unfinished: any repo with a pass or stage it refused, and any
    /// repo the run reached after the cap had already stopped it (such a repo is not read at
    /// all). In the order the run met them. A reader can tell which repos the inspection did
    /// not finish without reading the pass list.
    #[serde(default)]
    pub repos_not_finished: Vec<String>,
    /// True when a call's cost could not be determined (no reported figure, tokens that could
    /// not be priced) while the limit was set. The limit could then no longer be enforced, so
    /// everything after was refused; the limit was NOT reached. Every disclosure of what did not
    /// run says which of the two happened (see [`Self::cause`]).
    #[serde(default)]
    pub cost_unknown: bool,
}

/// The cause the spend limit's disclosures name when a call's cost was unknown. No dollar
/// figure: the text reaches the client report.
pub const CAUSE_COST_UNKNOWN: &str = "the spend limit set for this run could not be enforced \
                                     (a call's cost could not be determined, so the run stopped \
                                     instead of running past the limit unseen)";

/// The cause the spend limit's disclosures name when actual spend reached the limit.
pub const CAUSE_LIMIT_REACHED: &str = "the spend limit set for this run was reached";

/// [`CostCapRecord::cause`] for a call site that has the meter's flag rather than the record.
pub fn cause_for(cost_unknown: bool) -> &'static str {
    if cost_unknown {
        CAUSE_COST_UNKNOWN
    } else {
        CAUSE_LIMIT_REACHED
    }
}

impl CostCapRecord {
    /// Why the limit stopped work, as every disclosure completes "NOT RUN: <cause> ...": the
    /// limit was reached, or it could no longer be enforced because a call's cost was unknown.
    /// Telling an operator the limit "was reached" when a few cents of a $10 limit were spent
    /// sends them looking for a spend that never happened.
    pub fn cause(&self) -> &'static str {
        if self.cost_unknown {
            CAUSE_COST_UNKNOWN
        } else {
            CAUSE_LIMIT_REACHED
        }
    }

    /// True when the cap actually stopped something. A cap that was set but never reached
    /// leaves the run complete.
    pub fn stopped_the_run(&self) -> bool {
        !self.not_run_passes.is_empty()
            || !self.not_run_stages.is_empty()
            || !self.repos_not_finished.is_empty()
    }

    /// The repos the cap left unfinished: those named in [`Self::repos_not_finished`] plus any
    /// repo with a pass or stage that did not run, sorted.
    pub fn unfinished_repos(&self) -> Vec<String> {
        let mut repos: std::collections::BTreeSet<String> =
            self.repos_not_finished.iter().cloned().collect();
        repos.extend(self.not_run_passes.iter().map(|p| p.repo.clone()));
        repos.extend(self.not_run_stages.iter().map(|s| s.repo.clone()));
        repos.into_iter().collect()
    }

    /// The files whose scan pass did not run, as `(repo, path)`. These were not reviewed.
    pub fn unreviewed_files(&self) -> std::collections::BTreeSet<(String, String)> {
        self.not_run_passes
            .iter()
            .flat_map(|p| p.files.iter().map(|f| (p.repo.clone(), f.clone())))
            .collect()
    }

    /// The failed-pass disclosures for everything the cap stopped. The text carries NO dollar
    /// figure (it reaches the client report); the amounts are in the ledger and the CLI output.
    pub fn failed_passes(&self) -> Vec<crate::ai_audit::FailedPass> {
        not_run_failed_passes(&self.not_run_passes, &self.not_run_stages, self.cause())
    }

    /// How much of `rule_id`'s checking on `repo` the cap left undone. A rule is `NotRun` only
    /// when every chunk of its routing group was refused; a rule refused on some chunks, or
    /// only in the resolution round, is `Partial`.
    pub fn rule_coverage(&self, repo: &str, rule_id: &str) -> RuleCoverage {
        let mine: Vec<&NotRunPass> = self
            .not_run_passes
            .iter()
            .filter(|p| p.repo == repo && p.rule_ids.iter().any(|r| r == rule_id))
            .collect();
        if mine.is_empty() {
            return RuleCoverage::Complete;
        }
        let main = mine.iter().filter(|p| p.group != "resolution");
        let refused_chunks: std::collections::BTreeSet<(&str, usize)> =
            main.clone().map(|p| (p.group.as_str(), p.chunk)).collect();
        let n_chunks = main.map(|p| p.n_chunks).max();
        match n_chunks {
            Some(n) if refused_chunks.len() >= n => RuleCoverage::NotRun,
            _ => RuleCoverage::Partial,
        }
    }
}

/// The mutable half of the cap, owned by the [`crate::ai_audit::UsageMeter`].
#[derive(Debug, Default)]
pub(crate) struct BudgetState {
    pub in_flight: u64,
    pub not_run_passes: Vec<NotRunPass>,
    pub not_run_stages: Vec<NotRunStage>,
    pub notes: Vec<String>,
    /// Repos the run reached after the cap had already stopped it (see
    /// [`CostCapRecord::repos_not_finished`]).
    pub repos_not_finished: Vec<String>,
    /// Repos the run reached after an outage stop (see [`crate::run_health::OutageRecord`]).
    pub outage_repos: Vec<String>,
    /// A call's cost could not be determined (no reported dollar figure and no way to price its
    /// tokens) while a cap was set. The cap can no longer be enforced, so nothing further is
    /// admitted: refusing a pass that might have fit beats running past the limit unseen.
    pub cost_unknown: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass(chunk: usize, batch: usize, rules: &[&str]) -> NotRunPass {
        NotRunPass {
            files: Vec::new(),
            repo: "acme/api".into(),
            group: "pass[all]".into(),
            chunk,
            n_chunks: 3,
            rule_batch: batch,
            n_rule_batches: 4,
            rule_ids: rules.iter().map(|r| r.to_string()).collect(),
            class: camerata_llm::failure::FailureClass::SpendLimit,
        }
    }

    fn controls_with_cap(cap: Option<f64>) -> RunControls {
        RunControls {
            max_cost_usd: cap,
            ..RunControls::default()
        }
    }

    #[test]
    fn a_cap_with_an_unpriced_model_refuses_to_start_and_names_the_model() {
        let models = vec![
            (
                "the scan, calibration".to_string(),
                "claude-sonnet-5".to_string(),
            ),
            (
                "fix generation".to_string(),
                "some-free-model:free".to_string(),
            ),
        ];
        let why = controls_with_cap(Some(5.0))
            .refuse_if_cap_unenforceable(&models)
            .expect("an unpriced model must stop a capped run");
        assert!(
            why.contains("`some-free-model:free`") && why.contains("fix generation"),
            "{why}"
        );
        assert!(
            why.contains("cannot be enforced") && why.contains("Nothing was run"),
            "{why}"
        );
        assert!(
            !why.contains("claude-sonnet-5"),
            "the priced model is not blamed: {why}"
        );
    }

    #[test]
    fn without_a_cap_an_unpriced_model_changes_nothing_and_priced_models_pass_with_one() {
        let unpriced = vec![("the scan".to_string(), "some-free-model:free".to_string())];
        assert!(controls_with_cap(None)
            .refuse_if_cap_unenforceable(&unpriced)
            .is_none());
        let priced = vec![("the scan".to_string(), "claude-sonnet-5".to_string())];
        assert!(controls_with_cap(Some(5.0))
            .refuse_if_cap_unenforceable(&priced)
            .is_none());
    }

    /// The pre-rank-10 controls: the fix stage follows the calibration model, else the scan model.
    fn fix_follows_calibration() -> RunControls {
        let mut controls = RunControls::default();
        controls.efficiency.fix_on_haiku = false;
        controls
    }

    #[test]
    fn the_models_a_run_uses_are_listed_by_role_and_deduplicated() {
        let off = fix_follows_calibration();
        let all = models_the_run_will_use(Some("claude-sonnet-5"), None, false, &["ARCH-1"], &off);
        assert_eq!(all.len(), 1, "one model fills every role: {all:?}");
        assert!(all[0].0.contains("the scan") && all[0].0.contains("calibration"));
        assert!(all[0].0.contains("fix generation"));
        assert!(!all[0].0.contains("clone judge") && !all[0].0.contains("deep"));

        let split = models_the_run_will_use(
            Some("claude-sonnet-5"),
            Some("claude-opus-5"),
            true,
            &[camerata_checks::clone_rules::RULE_SINGLE_SOURCE],
            &off,
        );
        let ids: Vec<&str> = split.iter().map(|(_, m)| m.as_str()).collect();
        assert!(
            ids.contains(&"claude-sonnet-5") && ids.contains(&"claude-opus-5"),
            "{ids:?}"
        );
        let text = format!("{split:?}");
        assert!(
            text.contains("the deep tier") && text.contains("the clone judge"),
            "{text}"
        );
    }

    /// The clone judge is listed (and so priced and lock-checked) on the model the run will call:
    /// the project's fast band or `--clone-judge-model`, else the built-in Haiku 5.5. The older
    /// Haiku stays selectable and priced.
    #[test]
    fn the_clone_judge_role_is_listed_on_the_model_the_judge_will_call() {
        let judge_of = |controls: &RunControls| {
            models_the_run_will_use(
                Some("claude-sonnet-5-5"),
                None,
                false,
                &[camerata_checks::clone_rules::RULE_SINGLE_SOURCE],
                controls,
            )
            .into_iter()
            .find(|(roles, _)| roles.contains("the clone judge"))
            .map(|(_, model)| model)
            .unwrap()
        };
        assert_eq!(judge_of(&RunControls::default()), "claude-haiku-5-5");
        let picked = RunControls {
            clone_judge_model: Some("claude-haiku-4-5-20251001".to_string()),
            ..RunControls::default()
        };
        assert_eq!(judge_of(&picked), "claude-haiku-4-5-20251001");
        assert!(crate::llm::is_priced("claude-haiku-4-5-20251001"));
        let blank = RunControls {
            clone_judge_model: Some("  ".to_string()),
            ..RunControls::default()
        };
        assert_eq!(judge_of(&blank), "claude-haiku-5-5");
    }

    /// Rank 10: the spend-limit check lists the model the fix stage will actually call, in the
    /// same precedence as the call itself: an explicit fix model, else Haiku with the switch on.
    #[test]
    fn the_fix_role_is_listed_on_the_model_the_fix_stage_will_call() {
        let fix_model_of = |controls: &RunControls| {
            models_the_run_will_use(Some("claude-sonnet-5"), None, false, &[], controls)
                .into_iter()
                .find(|(roles, _)| roles.contains("fix generation"))
                .map(|(_, model)| model)
                .unwrap()
        };
        assert_eq!(
            fix_model_of(&RunControls::default()),
            crate::ai_audit::FIX_STAGE_HAIKU_MODEL
        );
        let explicit = RunControls {
            fix_model: Some("claude-opus-5".to_string()),
            ..RunControls::default()
        };
        assert_eq!(fix_model_of(&explicit), "claude-opus-5");
        assert_eq!(fix_model_of(&fix_follows_calibration()), "claude-sonnet-5");

        // With a spend limit, the default run is refused exactly when Haiku has no list price
        // (until the model registry carries it), and the refusal names the fix role.
        let capped = controls_with_cap(Some(5.0));
        let models = models_the_run_will_use(Some("claude-sonnet-5"), None, false, &[], &capped);
        let haiku_priced = crate::llm::models()
            .iter()
            .any(|m| m.id == crate::ai_audit::FIX_STAGE_HAIKU_MODEL);
        match capped.refuse_if_cap_unenforceable(&models) {
            Some(why) => {
                assert!(!haiku_priced, "a priced model must not refuse: {why}");
                assert!(
                    why.contains(crate::ai_audit::FIX_STAGE_HAIKU_MODEL)
                        && why.contains("fix generation"),
                    "{why}"
                );
            }
            None => assert!(
                haiku_priced,
                "an unpriced fix model must refuse a capped run"
            ),
        }
    }

    #[test]
    fn a_non_positive_or_non_finite_cap_is_refused() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(RunControls::validate_cap(bad).is_err(), "{bad}");
        }
        assert_eq!(RunControls::validate_cap(2.5).unwrap(), 2.5);
    }

    #[test]
    fn failed_passes_name_every_pair_once_per_chunk_and_carry_no_dollars() {
        let record = CostCapRecord {
            repos_not_finished: Vec::new(),
            cost_unknown: false,
            cap_usd: 12.34,
            spent_usd: 13.57,
            not_run_passes: vec![
                pass(2, 3, &["A-1", "A-2"]),
                pass(2, 4, &["A-3"]),
                pass(3, 1, &["A-4"]),
            ],
            not_run_stages: vec![NotRunStage {
                repo: "acme/api".into(),
                stage: "calibration".into(),
                partial: false,
                class: camerata_llm::failure::FailureClass::SpendLimit,
            }],
            notes: vec![],
        };
        let failed = record.failed_passes();
        assert_eq!(failed.len(), 3, "2 chunks + 1 stage: {failed:?}");
        assert!(failed[0].pass.contains("chunk 2/3"));
        assert!(
            failed[0].reason.contains("rule-batch 3, 4 of 4"),
            "{}",
            failed[0].reason
        );
        assert!(
            failed[0].reason.contains("3 rule check"),
            "{}",
            failed[0].reason
        );
        assert!(failed[1].reason.contains("rule-batch 1 of 4"));
        assert_eq!(failed[2].pass, "calibration");
        for fp in &failed {
            assert!(fp.reason.starts_with("NOT RUN"), "{}", fp.reason);
            assert!(fp.reason.contains("incomplete"), "{}", fp.reason);
            assert!(!fp.reason.contains('$') && !fp.reason.contains("12.34"));
        }
    }

    #[test]
    fn a_stage_stopped_part_way_says_so() {
        let record = CostCapRecord {
            cap_usd: 1.0,
            spent_usd: 2.0,
            not_run_stages: vec![NotRunStage {
                repo: "r".into(),
                stage: "calibration".into(),
                partial: true,
                class: camerata_llm::failure::FailureClass::SpendLimit,
            }],
            ..CostCapRecord::default()
        };
        let failed = record.failed_passes();
        assert!(
            failed[0].reason.contains("NOT RUN (in part)"),
            "{}",
            failed[0].reason
        );
        assert!(record.stopped_the_run());
    }

    #[test]
    fn rule_coverage_separates_not_run_from_partly_run() {
        // 3 chunks in the group; the rule's batch was refused on chunks 2 and 3 only.
        let mut record = CostCapRecord {
            cap_usd: 1.0,
            spent_usd: 2.0,
            not_run_passes: vec![pass(2, 1, &["A-1"]), pass(3, 1, &["A-1", "B-1"])],
            ..CostCapRecord::default()
        };
        assert_eq!(
            record.rule_coverage("acme/api", "A-1"),
            RuleCoverage::Partial
        );
        assert_eq!(
            record.rule_coverage("acme/api", "Z-9"),
            RuleCoverage::Complete
        );
        assert_eq!(
            record.rule_coverage("other/repo", "A-1"),
            RuleCoverage::Complete
        );
        // Refused on all three chunks: not run at all.
        record.not_run_passes.push(pass(1, 1, &["A-1"]));
        assert_eq!(
            record.rule_coverage("acme/api", "A-1"),
            RuleCoverage::NotRun
        );
        assert_eq!(
            record.rule_coverage("acme/api", "B-1"),
            RuleCoverage::Partial
        );
        // Refused only in the resolution round: partial.
        let mut resolution = pass(1, 1, &["C-1"]);
        resolution.group = "resolution".into();
        record.not_run_passes.push(resolution);
        assert_eq!(
            record.rule_coverage("acme/api", "C-1"),
            RuleCoverage::Partial
        );
    }

    /// A pass refused because a call's cost could not be priced was not refused because the
    /// limit was REACHED (the run may have spent cents against a $10 limit). The disclosure
    /// says what actually happened, so an operator reading the failed-pass list does not go
    /// looking for a spend that never occurred; it still names no dollar figure.
    #[test]
    fn a_pass_refused_because_a_cost_was_unknown_does_not_say_the_limit_was_reached() {
        let record = CostCapRecord {
            cap_usd: 10.0,
            spent_usd: 0.0095,
            not_run_passes: vec![pass(1, 1, &["A-1"])],
            not_run_stages: vec![NotRunStage {
                repo: "acme/shop".into(),
                stage: "fix-specific generation".into(),
                partial: false,
                class: FailureClass::SpendLimit,
            }],
            notes: Vec::new(),
            repos_not_finished: Vec::new(),
            cost_unknown: true,
        };
        assert!(record.stopped_the_run());
        let failed = record.failed_passes();
        assert_eq!(failed.len(), 2);
        for f in &failed {
            assert!(
                !f.reason.contains("was reached"),
                "the limit was not reached: {}",
                f.reason
            );
            assert!(
                f.reason.contains("could not be enforced") && f.reason.contains("cost"),
                "{}",
                f.reason
            );
            assert!(!f.reason.contains('$'), "{}", f.reason);
        }
        // The plain reached case is unchanged.
        let reached = CostCapRecord {
            cost_unknown: false,
            ..record
        };
        assert!(reached.failed_passes()[0].reason.contains("was reached"));
    }

    #[test]
    fn a_cap_that_stopped_nothing_leaves_the_run_complete() {
        let record = CostCapRecord {
            cap_usd: 5.0,
            spent_usd: 1.0,
            ..CostCapRecord::default()
        };
        assert!(!record.stopped_the_run());
        assert!(record.failed_passes().is_empty());
    }
}
