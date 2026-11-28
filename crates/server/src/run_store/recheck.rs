//! The recheck of a saved, incomplete inspection (`camerata inspect --recheck-saved`): finish a run
//! on a newer build by re-running only what the newer build changed, using every saved model
//! result as it was saved.
//!
//! The run saved its sealed report (`report.json`), its ledger and its stored model replies. The
//! recheck reads them and does exactly two things, and nothing else (no tool preview, no
//! dependency audit, no finder, no fix stage, no group review):
//!
//! 1. The delta: each clone check part that failed because the judge's reply could not be read is
//!    redone from the run's own data ([`crate::onboard::clone_advisory::redo`]): its request is
//!    rebuilt exactly as the scan built it, its stored reply is read with the current reader (no
//!    call), and only with the operator's explicit flag, when no stored reply settles it, is it
//!    asked once more on the model the run started with.
//! 2. The final checks, on the rows as they now stand: the convention accounting, the pipeline
//!    integrity checks and the seal. The integrity failures the first build stored are replaced
//!    with this build's result of the same checks. The release gate and the export follow in the
//!    caller, which writes nothing when the gate refuses.
//!
//! Both builds are recorded in the run, the ledger and "How this was done" ([`RerunRecord`]).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use camerata_api_types::audit::FindingKind;
use camerata_checks::clone_rules::CloneRule;

use super::engine_pin::{
    check_pin_for_rerun, client_start_refusal, EnginePin, RecheckDetail, RedoHow, RedonePart,
    RerunRecord,
};
use super::{
    check_repo_trees, decode_seed, read_record, runs_dir, tree_state, RunOrigin, RunStatus,
    RunStore, REPORT_FILE,
};
use crate::ai_audit::{FailedPass, FailureClass};
use crate::llm::LlmPort;
use crate::onboard::clone_advisory::redo::{self, PlannedBatch, RedoInput, RulePlan};
use crate::onboard::clone_advisory::FAILED_PASS_NAME;
use crate::onboard::{ScanReport, SealedReport, SelectedRule};
use crate::scan_ledger::ConventionsBlock;

/// The most clone check parts `--reask-failed-judge` may ask again unless the flag carries its own
/// count.
pub const DEFAULT_MAX_REASKED_PARTS: usize = 5;

/// Whether the recheck may ask the model again for a clone check part its stored reply does not
/// settle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaskFlag {
    /// Never: a part that cannot be settled from its stored reply refuses the recheck. The
    /// default; the recheck then makes no model call at all.
    Off,
    /// `--reask-failed-judge`: each such part may be asked once, up to `max_parts` of them.
    Allow { max_parts: usize },
}

/// Why a recheck did not run to the end. Nothing was exported in either case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecheckError {
    /// The run, or its saved report, is already complete: there is nothing to finish.
    AlreadyComplete(String),
    /// The recheck was refused, in plain words, naming the cause.
    Refused(String),
}

impl std::fmt::Display for RecheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyComplete(m) | Self::Refused(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for RecheckError {}

fn refused<T>(why: impl Into<String>) -> Result<T, RecheckError> {
    Err(RecheckError::Refused(why.into()))
}

/// What the recheck needs of the command and the build that runs it.
pub struct RecheckInputs<'a> {
    pub run_id: String,
    /// The `(name, folder)` of each repo, as the run named them.
    pub sources: &'a [(String, PathBuf)],
    /// The rules the run selected, derived the way the scan derived them.
    pub selected: &'a [SelectedRule],
    pub corpus: Option<&'a camerata_rules::RuleSet>,
    pub controls: &'a crate::cost_cap::RunControls,
    /// This build, and the rule library it loaded.
    pub pin: EnginePin,
    pub client_engagement: bool,
    /// Where the run's ledger was saved (`args.state_paths()`).
    pub ledger_file: &'a Path,
    pub reask: ReaskFlag,
}

/// A recheck that reached its end: the report to export and the open run to finish.
pub struct Rechecked {
    /// The saved report with the clone check redone and the final checks run again, sealed.
    pub report: SealedReport,
    /// The run, held open. The caller exports, then calls [`finish_run`] and
    /// [`RunStore::commit_recheck`].
    pub store: Arc<RunStore>,
    /// What ran again, to put on the run once the export is written.
    pub record: RerunRecord,
    /// Plain lines for the operator: what was redone and how.
    pub lines: Vec<String>,
}

/// The ledger fields the first build's own pipeline adds or recomputes AFTER it saved the ledger
/// file (`save_ledger`, before the seal) and BEFORE it saved the report (`write_report`, after
/// it), or a recheck records. The seal records the path audit, the content check and the rows it
/// sets aside (`path_hygiene`, `content`, `discarded`), and its site keying rebuilds the review
/// tree, which records the fix merge and the place merge again (`fix_merge`, `place_merge`).
/// `rerun` is the recheck's own record. Read from the order of `run_inspect` and
/// `ScanReport::seal` at build a7e1e1d5; every other field of the two copies must match exactly.
const SEALED_LATER: [&str; 6] = [
    "path_hygiene",
    "content",
    "discarded",
    "rerun",
    "fix_merge",
    "place_merge",
];

fn without_sealed_later(mut v: serde_json::Value) -> serde_json::Value {
    if let Some(o) = v.as_object_mut() {
        for key in SEALED_LATER {
            o.remove(key);
        }
    }
    v
}

fn discarded_tally(ledger: &serde_json::Value) -> u64 {
    ledger["discarded"]["by_reason"].as_object().map_or(0, |m| {
        m.values().filter_map(serde_json::Value::as_u64).sum()
    })
}

/// The saved sealed report of a run, read back as a report, and held to the saved ledger file:
/// both must be there and readable, the report must carry the ledger, the set-aside rows and the
/// convention records the ledger file says the run had (a missing field would read as empty and
/// quietly weaken the checks), and the report's own ledger must be the file's. A field the first
/// build did not write at all reads as its default.
pub fn load_saved_report(run_dir: &Path, ledger_file: &Path) -> Result<ScanReport, RecheckError> {
    let path = run_dir.join(REPORT_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            return refused(format!(
                "the saved report of this run is missing or cannot be read ({}: {e}). A run saves \
                 its sealed report when it ends; without it this command cannot finish the run. \
                 Restore the saved report to the run folder, or start the inspection again.",
                path.display()
            ))
        }
    };
    let unreadable = |what: &str, p: &Path, e: &dyn std::fmt::Display| {
        RecheckError::Refused(format!(
            "the saved {what} of this run could not be read ({}: {e}); it was not changed",
            p.display()
        ))
    };
    let report_v: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| unreadable("report", &path, &e))?;
    let report: ScanReport =
        serde_json::from_value(report_v.clone()).map_err(|e| unreadable("report", &path, &e))?;
    let ledger_bytes = std::fs::read(ledger_file).map_err(|e| {
        RecheckError::Refused(format!(
            "the saved ledger of this run is missing or cannot be read ({}: {e}); there is \
             nothing to recheck without it",
            ledger_file.display()
        ))
    })?;
    let ledger_v: serde_json::Value =
        serde_json::from_slice(&ledger_bytes).map_err(|e| unreadable("ledger", ledger_file, &e))?;
    serde_json::from_value::<crate::scan_ledger::ScanLedger>(ledger_v.clone())
        .map_err(|e| unreadable("ledger", ledger_file, &e))?;
    let missing = |field: &str| {
        RecheckError::Refused(format!(
            "the saved report of this run has no {field} (or an empty one) while the saved ledger \
             says the run had one, so the report is not the one the ledger describes and the \
             checks would run on less than the first build had; it was not changed"
        ))
    };
    if report_v
        .get("ledger")
        .is_none_or(serde_json::Value::is_null)
    {
        return Err(missing("embedded ledger"));
    }
    if discarded_tally(&ledger_v) > 0 && report.discarded.is_empty() {
        return Err(missing("set-aside rows"));
    }
    let records = ledger_v["conventions"]["rules"]
        .as_array()
        .map_or(0, Vec::len);
    if records > 0 && report.conventions.is_empty() {
        return Err(missing("convention records"));
    }
    let embedded = without_sealed_later(serde_json::to_value(&report.ledger).unwrap_or_default());
    let on_file = without_sealed_later(ledger_v);
    if embedded != on_file {
        let differs: Vec<&str> = embedded
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(k, v)| on_file.get(k.as_str()) != Some(*v))
            .map(|(k, _)| k.as_str())
            .collect();
        return refused(format!(
            "the ledger inside the saved report does not match the saved ledger file ({}; \
             differing: {}), so one of them is not from the end of this run; it was not changed",
            ledger_file.display(),
            differs.join(", ")
        ));
    }
    Ok(report)
}

/// The failed parts of a report, by who may redo them.
struct Failures {
    /// Clone check parts whose reply could not be read.
    clone_parts: Vec<FailedPass>,
    /// Failed parts that are not redone here and stay as saved.
    others: Vec<FailedPass>,
}

fn partition_failures(report: &ScanReport) -> Failures {
    let mut out = Failures {
        clone_parts: Vec::new(),
        others: Vec::new(),
    };
    for fp in report.failed_passes.iter().filter(|f| !f.scope_note) {
        if fp.pass == crate::scan_ledger::integrity_pass_name("convention accounting") {
            continue; // replaced by this build's own result of the same check
        }
        if fp.pass == FAILED_PASS_NAME && fp.class == FailureClass::Content {
            out.clone_parts.push(fp.clone());
        } else {
            out.others.push(fp.clone());
        }
    }
    out
}

/// The reasons the run would still be incomplete if every integrity failure and every clone check
/// part were put right: what this command cannot change.
fn reasons_left(report: &ScanReport, failures: &Failures) -> Vec<String> {
    let mut probe = report.clone();
    probe.failed_passes = failures.others.clone();
    probe.incomplete_reasons()
}

fn saved_rows_of<'a>(
    report: &'a ScanReport,
    repo: &'a str,
    rule: CloneRule,
) -> impl Iterator<Item = &'a crate::onboard::Finding> {
    report
        .findings
        .iter()
        .filter(move |f| f.repo == repo && f.rule_id == rule.id())
}

/// One clone check part to redo, planned from the run's data.
struct PartPlan {
    plan: RulePlan,
    files: Vec<(String, String)>,
    /// The repo's folder, where the evidence check reads the files.
    dir: PathBuf,
}

/// The clone rules armed on `repo`: every selected clone rule that applies to it.
fn armed_on<'a>(selected: &'a [SelectedRule], repo: &str) -> HashSet<&'a str> {
    selected
        .iter()
        .filter(|r| r.applies_to(repo))
        .map(|r| r.id.as_str())
        .filter(|id| camerata_checks::clone_rules::RULE_IDS.contains(id))
        .collect()
}

/// Read the repo's files the way the scan read them.
async fn read_repo_files(
    dir: &Path,
    scope: crate::onboard::scope::RepoScope,
) -> Result<Vec<(String, String)>, RecheckError> {
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        crate::onboard::read_local_repo_files(&dir, &scope).map(|e| e.files)
    })
    .await
    .map_err(|e| RecheckError::Refused(format!("reading the repository failed: {e}")))?
    .map_err(|e| RecheckError::Refused(format!("the repository could not be read: {e:#}")))
}

/// The operator's list of what will be asked, before any call goes out.
fn render_asks(asks: &[(&RulePlan, &PlannedBatch)], model: &str) -> String {
    let mut out = format!(
        "Asking {} clone check part(s) again, once each, on {model}, the model this inspection \
         started with. These calls are made on this build and recorded in the run, the ledger \
         and the report:\n",
        asks.len()
    );
    for (plan, batch) in asks {
        out.push_str(&format!(
            "  - clone check for rule {} in {}, {}\n",
            plan.rule.id(),
            plan.repo,
            batch.describe()
        ));
    }
    out
}

/// Finish an incomplete saved run on this build; see the module doc. `llm` is used only for the
/// parts [`ReaskFlag::Allow`] lets be asked again, once each, after `announce` has listed them.
pub async fn recheck_saved_run(
    inputs: &RecheckInputs<'_>,
    llm: Option<&dyn LlmPort>,
    announce: &mut (dyn FnMut(&str) + Send),
) -> Result<Rechecked, RecheckError> {
    let run_id = inputs.run_id.as_str();
    let record = read_record(run_id).map_err(|e| RecheckError::Refused(e.to_string()))?;
    if record.origin == RunOrigin::Cockpit {
        return refused(format!(
            "run {run_id} was started from the cockpit; `camerata inspect --recheck-saved` \
             rechecks runs started from the command line"
        ));
    }
    if record.status == RunStatus::Complete {
        return Err(RecheckError::AlreadyComplete(format!(
            "run {run_id} is already complete: every part is done and the export was written, so \
             there is nothing to recheck"
        )));
    }
    // A client inspection is exported only from a build that can be named exactly.
    if let Some(why) = client_start_refusal(inputs.client_engagement, &inputs.pin.build) {
        return refused(why);
    }
    check_pin_for_rerun(
        record.pin.as_ref(),
        &inputs.pin.build,
        &inputs.pin.corpus_version,
    )
    .map_err(RecheckError::Refused)?;
    let Some(saved_pin) = record.pin.clone() else {
        return refused("this run did not record the build it started on");
    };
    let run_dir = runs_dir().join(run_id);
    let mut report = load_saved_report(&run_dir, inputs.ledger_file)?;
    if report.incomplete_reasons().is_empty() {
        return Err(RecheckError::AlreadyComplete(format!(
            "the saved report of run {run_id} has nothing left unfinished, so there is nothing \
             to recheck"
        )));
    }

    // What this command cannot change is named now, before anything is read or asked: a client
    // export would be refused for it at the end.
    let failures = partition_failures(&report);
    let left = reasons_left(&report, &failures);
    if inputs.client_engagement && !left.is_empty() {
        return refused(format!(
            "this recheck redoes the final checks and a clone check part whose reply could not \
             be read, and the run has more that stays unfinished, so a client export would be \
             refused: {}. This command cannot finish this run. Nothing was asked and no \
             export was written.",
            left.join("; ")
        ));
    }

    // ── The delta: the clone check parts that failed ──────────────────────────────────────────
    let mut parts: Vec<PartPlan> = Vec::new();
    let judge_model = saved_pin.clone_judge_model.trim().to_string();
    if !failures.clone_parts.is_empty() {
        if judge_model.is_empty() {
            return refused(
                "this run did not record the model its clone check used, so the failed clone \
                 check cannot be redone",
            );
        }
        let trees = inputs
            .sources
            .iter()
            .map(|(name, dir)| (name.clone(), tree_state(dir)))
            .collect::<Vec<_>>();
        check_repo_trees(&record, &trees)
            .map_err(|e| RecheckError::Refused(format!("the clone check cannot be redone: {e}")))?;
        let seed = decode_seed(&record.nonce_seed);
        let mut files_by_repo: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for fp in &failures.clone_parts {
            let Some(rule) = redo::rule_of_failure(&fp.reason) else {
                return refused(format!(
                    "the failed clone check part for {} names no clone rule this command knows \
                     ({}); it cannot be redone",
                    fp.repo, fp.reason
                ));
            };
            let Some((_, dir)) = inputs.sources.iter().find(|(name, _)| name == &fp.repo) else {
                return refused(format!(
                    "repository {} of the failed clone check was not given to this command",
                    fp.repo
                ));
            };
            if !files_by_repo.contains_key(&fp.repo) {
                let files = read_repo_files(dir, inputs.controls.scope.for_repo(&fp.repo)).await?;
                files_by_repo.insert(fp.repo.clone(), files);
            }
            let files = files_by_repo[&fp.repo].clone();
            let armed = armed_on(inputs.selected, &fp.repo);
            let plan = crate::prompt_frame::with_seed(Some(seed), async {
                redo::plan_rule(&RedoInput {
                    repo: &fp.repo,
                    files: &files,
                    armed: &armed,
                    rule,
                    judge_model: &judge_model,
                    corpus: inputs.corpus,
                    run_dir: &run_dir,
                })
            })
            .await;
            if plan.batches.is_empty() {
                return refused(format!(
                    "the clone search finds no candidates for rule {} in {} now, though the run \
                     found some: the repository is not the tree the run read. Nothing was asked \
                     and no export was written.",
                    rule.id(),
                    fp.repo
                ));
            }
            // A batch the scan never sent, or batched differently (its verdict cache held some
            // candidates and the run does not record which), has no stored call under its
            // rebuilt key. It is never asked.
            if let Some(batch) = plan.unsent().next() {
                return refused(format!(
                    "the clone check for rule {} in {} cannot be redone: {} has no call stored \
                     under its rebuilt request, so the batch layout of the first build cannot be \
                     reproduced (or that batch was never sent), and this command asks only batches \
                     the first build sent. Nothing was asked and no export was written.",
                    rule.id(),
                    fp.repo,
                    batch.describe()
                ));
            }
            // The saved rows of a part judged in part are tied to its batches by candidate id
            // before anything is redone; rows that cannot be tied refuse rather than risk a
            // second row for a candidate.
            let traces = redo::SavedTraces {
                rows: saved_rows_of(&report, &fp.repo, rule)
                    .filter(|r| !matches!(r.kind, Some(FindingKind::NotAssessed(_))))
                    .collect(),
                discarded: report
                    .discarded
                    .iter()
                    .filter(|f| f.repo == fp.repo && f.rule_id == rule.id())
                    .collect(),
                folded: report
                    .findings
                    .iter()
                    .filter(|f| f.repo == fp.repo)
                    .flat_map(|f| &f.flagged_by)
                    .filter(|b| b.rule_id == rule.id())
                    .map(|b| (b.path.as_str(), b.line))
                    .collect(),
            };
            if let Err(why) = plan.tie_to_saved_rows(&traces) {
                return refused(format!(
                    "the clone check for rule {} in {} was judged in part by build {}, and its \
                     saved rows cannot be tied to its batches ({why}), so finishing it could \
                     count a candidate twice. Nothing was asked and no export was written.",
                    rule.id(),
                    fp.repo,
                    saved_pin.build.short_commit().unwrap_or_default()
                ));
            }
            parts.push(PartPlan {
                plan,
                files,
                dir: dir.clone(),
            });
        }
    }

    // What no stored reply settles may be asked once, with the operator's flag and no other way.
    let unread: Vec<(&RulePlan, &PlannedBatch)> = parts
        .iter()
        .flat_map(|p| p.plan.unread().map(move |b| (&p.plan, b)))
        .collect();
    if !unread.is_empty() {
        let max = match inputs.reask {
            ReaskFlag::Off => {
                let mut out = format!(
                    "{} clone check part(s) cannot be settled from the replies the run stored, \
                     and no model may be asked without --reask-failed-judge. Nothing was sent \
                     and no export was written. The parts:\n",
                    unread.len()
                );
                for (plan, batch) in &unread {
                    out.push_str(&format!(
                        "  - clone check for rule {} in {}, {}\n",
                        plan.rule.id(),
                        plan.repo,
                        batch.describe()
                    ));
                }
                out.push_str(&format!(
                    "With --reask-failed-judge each is asked once on {judge_model}, the model \
                     this inspection started with."
                ));
                return refused(out);
            }
            ReaskFlag::Allow { max_parts } => max_parts,
        };
        if unread.len() > max {
            return refused(format!(
                "{} clone check part(s) would be asked again, more than the {max} allowed. \
                 Nothing was sent and no export was written. Give --reask-failed-judge=<COUNT> \
                 to allow more.",
                unread.len()
            ));
        }
        if llm.is_none() {
            return refused(
                "no model is available to ask the clone check parts again. Nothing was sent and \
                 no export was written.",
            );
        }
        check_spend(inputs, &run_dir, &unread)?;
        announce(&render_asks(&unread, &judge_model));
    }

    // From here the run is opened (its lock held) and every way out finishes it.
    let run_json_before = std::fs::read(run_dir.join("run.json"))
        .map_err(|e| RecheckError::Refused(format!("the run's record could not be read: {e}")))?;
    let store = RunStore::open(run_id).map_err(|e| RecheckError::Refused(e.to_string()))?;
    // A refusal from here on leaves the run's record as it was found.
    store.restore_run_json_on_drop(run_json_before);
    let abandon = |_store: &RunStore, _report: &ScanReport, why: String| RecheckError::Refused(why);

    let mut answers: Vec<Vec<crate::onboard::clone_advisory::prompt::ParsedBatch>> = Vec::new();
    let mut detail = RecheckDetail::default();
    let mut spent: Vec<redo::SpentCall> = Vec::new();
    {
        let journaled = llm.map(|l| super::journal::wrap(l, store.clone()));
        for part in &parts {
            let mut got = Vec::new();
            let mut asked_again = 0usize;
            for batch in &part.plan.batches {
                if let Some(read) = &batch.read {
                    got.push(read.clone());
                    continue;
                }
                let Some(model) = journaled.as_deref() else {
                    return Err(abandon(
                        &store,
                        &report,
                        "no model is available".to_string(),
                    ));
                };
                // A stored reply the journal would hand back as its own is the one that was read
                // and held no verdict: asking would return it again and be no call at all.
                if model.replays(batch.request()) {
                    return Err(abandon(
                        &store,
                        &report,
                        format!(
                            "the stored reply for the clone check of rule {} in {} ({}) names no \
                             verdict for the candidates asked about, and asking again would \
                             return the same stored reply. No export was written.",
                            part.plan.rule.id(),
                            part.plan.repo,
                            batch.describe()
                        ),
                    ));
                }
                let asked_call = crate::prompt_frame::with_seed(
                    Some(decode_seed(&record.nonce_seed)),
                    redo::ask_once(model, part.plan.rule, batch),
                )
                .await;
                spent.extend(asked_call.spent);
                match asked_call.read {
                    Ok(read) => {
                        asked_again += 1;
                        got.push(read);
                    }
                    Err(why) => {
                        return Err(abandon(
                            &store,
                            &report,
                            format!(
                                "the clone check for rule {} in {} ({}) was asked again once \
                                 and still could not be settled: {why}. No export was written.",
                                part.plan.rule.id(),
                                part.plan.repo,
                                batch.describe()
                            ),
                        ))
                    }
                }
            }
            detail.clone_parts.push(RedonePart {
                repo: part.plan.repo.clone(),
                rule: part.plan.rule.id().to_string(),
                candidates: part.plan.found(),
                batches: part.plan.batches.len(),
                asked_again,
                how: if asked_again == 0 {
                    RedoHow::ReadStored
                } else {
                    RedoHow::AskedAgain {
                        model: judge_model.clone(),
                    }
                },
            });
            answers.push(got);
        }
    }

    // ── Join the verdicts the way the scan does, then run the final checks ────────────────────
    let mut lines: Vec<String> = Vec::new();
    for (part, answered) in parts.into_iter().zip(answers) {
        let (repo, rule) = (part.plan.repo.clone(), part.plan.rule);
        let (outcome, counts) = redo::settle(
            part.plan,
            &answered,
            &part.files,
            &judge_model,
            inputs.corpus,
        );
        let rows_in = report.findings.len();
        let removed = {
            let before = report.findings.len();
            report.findings.retain(|f| {
                !(f.repo == repo
                    && f.rule_id == rule.id()
                    && matches!(f.kind, Some(FindingKind::NotAssessed(_))))
            });
            before - report.findings.len()
        };
        let mut fresh = outcome.findings.clone();
        fresh.extend(outcome.not_assessed.iter().cloned());
        let fresh = post_judge_steps(
            &mut report,
            &repo,
            &part.files,
            &part.dir,
            fresh,
            inputs.corpus,
        );
        let added = fresh.len();
        report.findings.extend(fresh);
        let emitted = outcome
            .findings
            .iter()
            .filter(|f| f.rule_id == rule.id())
            .count();
        let fully = counts.coverage_gaps() == 0;
        report.ledger.settle_clone_rule(
            &repo,
            rule.id(),
            fully,
            (!fully).then(|| crate::onboard::clone_advisory::partial_skip_reason(&counts)),
            emitted,
        );
        let mut acc = crate::scan_ledger::StageAccounting::default();
        acc.other
            .push(("not_assessed_row_replaced".to_string(), removed));
        acc.push_added("clone_advisory_findings", added);
        report.ledger.record_stage_for_repo(
            &repo,
            "clone-advisory-recheck",
            rows_in,
            report.findings.len(),
            acc,
        );
        report.failed_passes.retain(|f| {
            !(f.pass == FAILED_PASS_NAME && f.repo == repo && f.reason.starts_with(rule.id()))
        });
        lines.push(format!(
            "clone check for rule {} in {}: {} judged, {} confirmed",
            rule.id(),
            repo,
            counts.judged,
            counts.confirmed
        ));
    }
    add_spend(&mut report, &spent);
    // The scan settles tool causes after its last row-adding stage, over the whole report.
    crate::settle_tool_causes(&mut report, inputs.corpus);
    run_integrity_checks(&mut report);

    let record_of_rerun = RerunRecord::for_recheck(&saved_pin.build, &inputs.pin.build, detail);
    report.ledger.record_rerun(record_of_rerun.clone());
    let sealed = report.seal();
    Ok(Rechecked {
        report: sealed,
        store,
        record: record_of_rerun,
        lines,
    })
}

/// The scan's own deterministic steps after the clone judge, applied to the rows the redone part
/// made, with the scan's own functions: the stack exceptions, the collapse and the grouping (the
/// sub-group ids continue after the saved tree's), the first-inspection record, the corpus
/// severity bounds and the evidence check. The fix stage and the group review are model work and
/// are not run again.
fn post_judge_steps(
    report: &mut ScanReport,
    repo: &str,
    files: &[(String, String)],
    dir: &Path,
    mut rows: Vec<crate::onboard::Finding>,
    corpus: Option<&camerata_rules::RuleSet>,
) -> Vec<crate::onboard::Finding> {
    if let Some(c) = corpus {
        let frameworks = report
            .stacks
            .iter()
            .find(|s| s.repo == repo)
            .map(|s| s.frameworks.clone())
            .unwrap_or_default();
        let rows_in = rows.len();
        let rows_ex = crate::ai_audit::apply_stack_exceptions(rows, &frameworks, c);
        let (excepted, kept) = crate::engine_discard::split_discarded(rows_ex);
        rows = kept;
        report.ledger.record_discarded(&excepted);
        report.discarded.extend(excepted);
        if rows_in != rows.len() {
            let mut acc = crate::scan_ledger::StageAccounting::default();
            acc.other
                .push(("stack_exception_excluded".to_string(), rows_in - rows.len()));
            report.ledger.record_stage_for_repo(
                repo,
                "stack-exception-filter",
                rows_in,
                rows.len(),
                acc,
            );
        }
    }
    let tool_map = crate::review_tree::ToolMap::from_corpus(corpus);
    let rows_in = rows.len();
    let collapse = crate::review_tree::collapse_identical(rows, &tool_map);
    let acc = collapse.accounting();
    let collapsed_out = collapse.rows.len();
    report
        .ledger
        .record_stage_for_repo(repo, "collapse-identical", rows_in, collapsed_out, acc);
    let mut alloc = crate::review_tree::SubgroupAllocator::continuing_after(
        report.tree.as_ref().map_or(&[][..], |t| t.nodes.as_slice()),
    );
    let (mut rows, sub_nodes) =
        crate::review_tree::group_semantic(collapse.rows, files, &tool_map, &mut alloc);
    if let Some(tree) = report.tree.as_mut() {
        tree.nodes.extend(sub_nodes);
    }
    report.ledger.record_stage_for_repo(
        repo,
        "group",
        collapsed_out,
        rows.len(),
        crate::scan_ledger::StageAccounting::default(),
    );
    crate::onboard::audit::record_first_inspection(&mut rows, files, &mut report.ledger);
    crate::severity_bounds::apply_scan_severity_bounds(&mut rows, corpus);
    crate::onboard::evidence_check::verify_and_record(
        &mut report.ledger,
        &mut rows,
        files,
        Some(dir),
    );
    rows
}

/// The spend limit for the asks: with a limit, what the run already spent plus the most each call
/// can cost must fit under it, and every call must be priceable.
fn check_spend(
    inputs: &RecheckInputs<'_>,
    run_dir: &Path,
    asks: &[(&RulePlan, &PlannedBatch)],
) -> Result<(), RecheckError> {
    let Some(limit) = inputs.controls.max_cost_usd else {
        return Ok(());
    };
    let mut total = super::journal::stored_spend(run_dir);
    for (plan, batch) in asks {
        match crate::ai_audit::request_ceiling_usd(batch.request()) {
            Some(c) => total += c,
            None => {
                return refused(format!(
                    "the clone check for rule {} in {} cannot be priced, so the spend limit \
                     cannot be kept. Nothing was sent and no export was written.",
                    plan.rule.id(),
                    plan.repo
                ))
            }
        }
    }
    if total > limit {
        return refused(
            "asking the clone check parts again could go over the spend limit. Nothing was sent \
             and no export was written. Give a higher --max-cost-usd to allow it.",
        );
    }
    Ok(())
}

/// The convention accounting on the rows as they now stand. What the first build stored for it is
/// replaced with this build's result of the same check, so a defect the new build fixed no longer
/// fails the run and one it still has still does. The ledger stage checks are NOT run again: they
/// read stage entries the first build saved, so their result is the one it stored, and a stage it
/// never checked is not a check this recheck adds.
pub fn run_integrity_checks(report: &mut ScanReport) {
    let pass = crate::scan_ledger::integrity_pass_name("convention accounting");
    report.failed_passes.retain(|f| f.pass != pass);
    let repo_label = report.repos.join(", ");
    let mut block =
        report.ledger.conventions().cloned().unwrap_or_else(|| {
            ConventionsBlock::from_records(&report.conventions, &report.findings)
        });
    let lost =
        block.check_finder_rows_kept(&report.conventions, &report.findings, &report.discarded);
    report.ledger.record_conventions(block);
    if let Some(failure) = crate::scan_ledger::convention_accounting_failure(&repo_label, &lost) {
        report.failed_passes.push(failure);
    }
}

/// Put what the asked calls cost on the run's usage and on the ledger's clone judge stage, so a
/// call made on this build is never invisible to either.
fn add_spend(report: &mut ScanReport, spent: &[redo::SpentCall]) {
    use crate::stage_telemetry::{Stage, StageRecord, StageTelemetry, StageUsage};
    if spent.is_empty() {
        return;
    }
    let mut usage = report
        .actual_usage
        .take()
        .unwrap_or(crate::ai_audit::ActualUsage {
            cost_complete: true,
            ..Default::default()
        });
    let mut stage = StageUsage::default();
    for call in spent {
        usage.calls += 1;
        usage.input_tokens += call.input_tokens;
        usage.output_tokens += call.output_tokens;
        usage.cache_read_input_tokens += call.cache_read_input_tokens;
        usage.cache_creation_input_tokens += call.cache_creation_input_tokens;
        stage.calls += 1;
        stage.input_tokens += call.input_tokens;
        stage.output_tokens += call.output_tokens;
        stage.cache_read_input_tokens += call.cache_read_input_tokens;
        stage.cache_creation_input_tokens += call.cache_creation_input_tokens;
        match call.cost_usd {
            Some(c) => {
                usage.cost_usd += c;
                stage.cost_usd += c;
                stage.cost_reported_calls += 1;
            }
            None => usage.cost_complete = false,
        }
        *stage.models.entry(call.model.clone()).or_insert(0) += 1;
    }
    report.actual_usage = Some(usage);
    let mut telemetry: StageTelemetry =
        report.ledger.stage_telemetry().cloned().unwrap_or_default();
    match telemetry
        .stages
        .iter_mut()
        .find(|r| r.stage == Stage::CloneJudge)
    {
        Some(record) => {
            let u = &mut record.usage;
            u.calls += stage.calls;
            u.input_tokens += stage.input_tokens;
            u.output_tokens += stage.output_tokens;
            u.cache_read_input_tokens += stage.cache_read_input_tokens;
            u.cache_creation_input_tokens += stage.cache_creation_input_tokens;
            u.cost_usd += stage.cost_usd;
            u.cost_reported_calls += stage.cost_reported_calls;
            for (m, n) in stage.models {
                *u.models.entry(m).or_insert(0) += n;
            }
        }
        None => telemetry.stages.push(StageRecord {
            stage: Stage::CloneJudge,
            usage: stage,
        }),
    }
    report.ledger.record_stage_telemetry(telemetry);
}

#[cfg(test)]
mod tests;
