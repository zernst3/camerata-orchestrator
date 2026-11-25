//! The agent finder's runner: one repository into problems, returning exactly what
//! `ai_audit::audit_repo` returns, so everything after the call is unchanged. It plans the pieces,
//! runs each one (build the room and the checklist, admit it with the meter, run the agent, send
//! unread files again once, ask once for the rules with no usable answer), runs the joints call
//! for a repository with two or more pieces, derives the conventions from the checklist answers,
//! rates the problems, and records every finished piece for a resume.
//!
//! Coverage is read from the tool trace only ([`super::coverage`]); nothing the model says about
//! its own work changes a state.

mod activity;
mod conventions;
mod joints;
mod piece;
mod plan;
mod rating;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap, HashSet};

use futures::StreamExt;

use super::checklist;
use super::coverage::PieceCoverage;
use super::pieces::{plan_pieces, units_for, TARGET_BYTES};
use super::reply::{
    Problem, FINDER_PIECE_CAPTURE, FINDER_PROBLEM_CAPTURE, FINDER_RULES_CAPTURE,
    FINDER_SURE_CAPTURE,
};
use super::room::Room;
use super::{Piece, PieceAgent};
use crate::ai_audit::{FailedPass, FailureClass, RuleRecommendation, UsageMeter};
use crate::checkpoint::AbsorbedLine;
use crate::cost_cap::RunControls;
use crate::onboard::{Finding, ProposedRule};
use crate::scan_ledger::{ScanLedger, StageAccounting, StageSample};
use crate::scan_routing::AuditRule;
use crate::stage_telemetry::Stage;
pub use activity::FinderActivity;
use camerata_api_types::audit::RuleConvention;
use piece::{PieceReport, Shared, StoredPiece};
pub use plan::{piece_figures, PieceFigures};

/// Settings of one finder run that are not on [`RunControls`].
#[derive(Debug, Clone, PartialEq)]
pub struct FinderOptions {
    /// The finder's model (resolved: the explicit pick, else [`super::DEFAULT_FINDER_MODEL`]).
    pub model: String,
    /// Internal runs only: split every unit into at least this many pieces.
    pub force_pieces: Option<usize>,
    /// Whether rules' declared `applies_to` scopes route files to rules.
    pub applies_to_routing: bool,
    /// Pieces in flight at once (3 on the API, 1 on the command line backend).
    pub pieces_in_flight: usize,
    /// The project's chosen option for each multi-option rule (upper-cased rule id to option id),
    /// the way the alternatives stage is given it. Empty when the project chose none.
    pub chosen_options: HashMap<String, String>,
    /// `api` or `cli`: the label the turns are metered under.
    pub backend: String,
}

impl FinderOptions {
    /// Options for `model` with every other setting at its default (one piece in flight).
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            force_pieces: None,
            applies_to_routing: true,
            pieces_in_flight: 1,
            chosen_options: HashMap::new(),
            backend: "api".to_string(),
        }
    }
}

/// A rule the finder could not complete for a unit, so the unit's answer for it is partial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialRule {
    pub unit: String,
    pub rule_id: String,
    pub reason: String,
}

/// What the finder returns beyond the five values `audit_repo` returns.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FinderExtras {
    /// One coverage record for each piece.
    pub coverage: Vec<PieceCoverage>,
    /// Rules recorded as not completed for a unit.
    pub partial_rules: Vec<PartialRule>,
    /// Rules every piece that was asked answered "not applicable" (and none was left partial).
    pub not_applicable_rules: Vec<String>,
    /// Rules every piece that was asked answered "not a problem here" or "not applicable" (at
    /// least one the former, none left partial): the model looked and judged it harmless, so the
    /// rule is never recorded as verified clean.
    pub not_a_problem_rules: Vec<String>,
    /// One line for each "not a problem here" answer (piece, rule, place, reason). Operator only.
    pub not_a_problem_notes: Vec<String>,
    /// Operator-only remarks: stray files an agent wrote, listed places that could not be checked.
    pub notes: Vec<String>,
    /// How many problems the agents wrote, and how many of them cite no rule. The share with no
    /// rule drives the needs a decision count, so every run records it.
    pub problems_total: usize,
    pub problems_without_rule: usize,
    /// Every rule an accepted `problems` answer or a citation names, whichever rule the merged
    /// row of its problem ended up under. A rule here has a problem and never reads clean.
    pub rules_with_problems: Vec<String>,
    /// One convention record for each rule with options the finder judged for this repo.
    pub conventions: Vec<RuleConvention>,
}

impl FinderExtras {
    /// Queue the "judged not applicable" fact for the rules every piece answered so. Call it
    /// BEFORE the rules of the repository are recorded in the ledger (it is consumed by
    /// `record_rule`).
    pub fn note_before_rules(&self, ledger: &mut ScanLedger, repo: &str) {
        for rule in &self.not_applicable_rules {
            ledger.note_not_applicable_rule(repo, rule.to_ascii_uppercase());
        }
        for rule in &self.not_a_problem_rules {
            ledger.note_not_a_problem_rule(repo, rule.to_ascii_uppercase());
        }
    }

    /// Count one for each rule in [`Self::rules_with_problems`] that the rows' own rule ids left at
    /// zero (a problem cited to two rules is one row under the first; the second has a problem
    /// too). Call it on the per-rule counts BEFORE the rules are recorded.
    pub fn add_rules_with_problems(&self, counts: &mut HashMap<String, usize>) {
        for rule in &self.rules_with_problems {
            let n = counts.entry(rule.clone()).or_insert(0);
            if *n == 0 {
                *n = 1;
            }
        }
    }

    /// Record the coverage block and mark the rules left partial. Call it AFTER the rules of the
    /// repository are recorded (a partial mark needs the rule's entry).
    pub fn note_after_rules(&self, ledger: &mut ScanLedger, repo: &str) {
        ledger.record_finder_coverage(repo, self.coverage.iter().map(finder_coverage_record));
        for p in &self.partial_rules {
            ledger.note_partial_rule(&p.rule_id, repo, p.reason.clone());
        }
    }
}

/// One piece's coverage in the shape the ledger keeps.
fn finder_coverage_record(c: &PieceCoverage) -> crate::scan_ledger::FinderPieceCoverage {
    use super::coverage::FileState;
    let mut r = crate::scan_ledger::FinderPieceCoverage {
        piece: c.piece_key.clone(),
        files_owed: c.files.len(),
        context_files_read: c.context_read.len(),
        agent_list_matched: c.claim_matched,
        ..Default::default()
    };
    for (path, state) in &c.files {
        match state {
            FileState::ReadInFull => r.read_in_full += 1,
            FileState::ReadInPart(_) => r.read_in_part += 1,
            FileState::SentAgain => r.sent_again += 1,
            FileState::NotReviewed => r.not_reviewed += 1,
        }
        if matches!(state, FileState::NotReviewed | FileState::ReadInPart(_)) {
            r.not_reviewed_files.push(path.clone());
        }
    }
    r
}

/// The same five values as `ai_audit::audit_repo` plus [`FinderExtras`].
pub type FinderResult = (
    Vec<Finding>,
    Vec<ProposedRule>,
    Vec<RuleRecommendation>,
    Vec<FailedPass>,
    Vec<StageSample>,
    FinderExtras,
);

/// Refuse a combination of finder settings before anything is read or spent. `incremental` is true
/// when the run would re-inspect only what changed since an earlier inspection; `internal` is
/// true for the maintainer's own (`--internal`) runs. The messages are for the operator at the
/// command line.
pub fn refuse_unsupported_finder_settings(
    controls: &RunControls,
    incremental: bool,
    internal: bool,
) -> Result<(), String> {
    use super::FinderChoice;
    if controls.finder_force_pieces.is_some() && !internal {
        return Err(
            "--finder-force-pieces is for internal runs only. Pass --internal if this is your own \
             development and not client work."
                .to_string(),
        );
    }
    if controls.finder_force_pieces.is_some() && controls.finder != FinderChoice::Agent {
        return Err("--finder-force-pieces needs --finder agent.".to_string());
    }
    if controls.finder != FinderChoice::Agent {
        return Ok(());
    }
    if controls.consensus {
        return Err(
            "--finder agent cannot be combined with --consensus. The agent finder reviews each part \
             once; the consensus option repeats the scan passes it replaces. Pass --finder engine \
             to use --consensus."
                .to_string(),
        );
    }
    if incremental {
        return Err(
            "--finder agent needs a full inspection; it cannot re-inspect only the files that \
             changed. Pass --full to inspect everything. Pass --finder engine to re-inspect \
             only the changed files."
                .to_string(),
        );
    }
    Ok(())
}

/// Outside review Part 1 item 4: a rule file that did not parse never reaches the agent finder's
/// checklist, so an agent finder run, client or internal, stops before any spend when one failed
/// to load (a run with a rule silently missing would read as clean for that rule, and graded
/// evidence must not come from a broken rule set). The message names each rule id and the file
/// with the parser's reason, for the operator. The same failures are also recorded in the run's
/// ledger, coverage notes and integrity gate by `record_corpus_load_failures`. `ai_review` is
/// whether the AI review (and so the finder) runs at all: a deterministic-only scan is not stopped.
pub fn refuse_unloadable_rules(
    controls: &RunControls,
    ai_review: bool,
    failures: &[crate::CorpusLoadFailure],
) -> Result<(), String> {
    if controls.finder != super::FinderChoice::Agent || !ai_review || failures.is_empty() {
        return Ok(());
    }
    let lines: Vec<String> = failures
        .iter()
        .map(|f| format!("{} ({}: {})", f.rule_id, f.path.display(), f.error))
        .collect();
    Err(format!(
        "{} rule file(s) did not load, so the agent finder would run without those rules and \
         report nothing for them: {}. Fix the rule files and run again. Nothing was spent.",
        failures.len(),
        lines.join("; ")
    ))
}

/// One unit of work: a piece to run, a piece to replay or a piece that cannot be tried again.
enum Work {
    Fresh(Piece),
    Stored(Piece, StoredPiece),
    /// A single file the model refused on an earlier attempt and again after the split.
    RefusedFinal(Piece, AbsorbedLine),
}

impl Work {
    fn piece(&self) -> &Piece {
        match self {
            Work::Fresh(p) | Work::Stored(p, _) | Work::RefusedFinal(p, _) => p,
        }
    }
}

fn refused_key(key: &str) -> String {
    format!("{key}#refused")
}

/// The marker of a piece that ended on a stop that repeats (see `piece::splits_on_resume`).
fn split_key(key: &str) -> String {
    format!("{key}#split")
}

/// Expand a planned piece against the stored lines of an earlier attempt: a finished piece is
/// replayed, a refused piece is run in two halves (by path order), and anything else runs.
/// The stored line of each refused piece that was split into halves goes to `split_parents`: its
/// cost was paid, so the resume replays it as resumed.
fn expand(
    piece: Piece,
    lines: &HashMap<String, AbsorbedLine>,
    out: &mut Vec<Work>,
    split_parents: &mut Vec<AbsorbedLine>,
    halves: &mut HashSet<String>,
) {
    if let Some(line) = lines.get(&piece.key) {
        if let Ok(stored) = serde_json::from_str::<StoredPiece>(&line.text) {
            out.push(Work::Stored(piece, stored));
            return;
        }
    }
    if let Some(line) = lines.get(&refused_key(&piece.key)) {
        if piece.files.len() >= 2 {
            split_parents.push(line.clone());
            let mid = piece.files.len() / 2;
            for (suffix, files) in [("a", &piece.files[..mid]), ("b", &piece.files[mid..])] {
                halves.insert(format!("{}/{suffix}", piece.key));
                expand(
                    Piece {
                        key: format!("{}/{suffix}", piece.key),
                        unit: piece.unit.clone(),
                        files: files.to_vec(),
                    },
                    lines,
                    out,
                    split_parents,
                    halves,
                );
            }
        } else {
            out.push(Work::RefusedFinal(piece, line.clone()));
        }
        return;
    }
    if let Some(line) = lines.get(&split_key(&piece.key)) {
        if piece.files.len() >= 2 {
            split_parents.push(line.clone());
            let mid = piece.files.len() / 2;
            for (suffix, files) in [("a", &piece.files[..mid]), ("b", &piece.files[mid..])] {
                halves.insert(format!("{}/{suffix}", piece.key));
                expand(
                    Piece {
                        key: format!("{}/{suffix}", piece.key),
                        unit: piece.unit.clone(),
                        files: files.to_vec(),
                    },
                    lines,
                    out,
                    split_parents,
                    halves,
                );
            }
            return;
        }
        // One file cannot be halved: it runs again whole and, if it ends the same way, stays a
        // named failed part.
    }
    out.push(Work::Fresh(piece));
}

/// The checkpoint line of a finished piece (or of a refused one, under its `#refused` key).
fn checkpoint_line(
    options: &FinderOptions,
    key: String,
    usage: &[super::TurnUsage],
    text: String,
) -> AbsorbedLine {
    let cost: Option<f64> = usage
        .iter()
        .map(|u| u.cost_usd)
        .try_fold(0.0, |acc, c| c.map(|c| acc + c));
    AbsorbedLine {
        key,
        model: options.model.clone(),
        backend: options.backend.clone(),
        cost_usd: cost,
        input_tokens: Some(usage.iter().map(|u| u.input_tokens).sum()),
        output_tokens: Some(usage.iter().map(|u| u.output_tokens).sum()),
        cache_read_input_tokens: usage.iter().map(|u| u.cache_read_input_tokens).sum(),
        cache_creation_input_tokens: usage.iter().map(|u| u.cache_creation_input_tokens).sum(),
        text,
        members: Vec::new(),
        batch_id: None,
    }
}

/// The turn usage a stored checkpoint line recorded.
fn line_usage(line: &AbsorbedLine) -> super::TurnUsage {
    super::TurnUsage {
        input_tokens: line.input_tokens.unwrap_or(0),
        output_tokens: line.output_tokens.unwrap_or(0),
        cache_read_input_tokens: line.cache_read_input_tokens,
        cache_creation_input_tokens: line.cache_creation_input_tokens,
        cost_usd: line.cost_usd,
    }
}

fn store(shared: &Shared<'_>, line: &AbsorbedLine) {
    let Some(cp) = crate::checkpoint::stage_checkpoint(&format!("finder_{}", shared.repo)) else {
        return;
    };
    if let Err(e) = cp.record_absorbed(line) {
        if let Some(m) = shared.meter {
            m.record_note(format!(
                "The agent finder's checkpoint could not be written ({e}); a resume will review \
                 the part again."
            ));
        }
    }
}

/// Run one unit of work to its report.
async fn run_work(shared: &Shared<'_>, index: usize, work: Work) -> PieceReport {
    let piece = work.piece().clone();
    let pairs: Vec<(String, String)> = shared
        .files
        .iter()
        .filter(|(p, _)| piece.files.contains(p))
        .cloned()
        .collect();
    let list = checklist::build(
        shared.selected,
        shared.corpus,
        &pairs,
        shared.options.applies_to_routing,
    );
    let built = Room::build(&piece, shared.files, &list.render());
    let room = built.room;
    let not_served = built.not_served;
    match work {
        Work::Stored(_, stored) => {
            if let Some(m) = shared.meter {
                for run in &stored.runs {
                    for usage in &run.usage {
                        m.record_resumed(Stage::Finder, &piece::response_of(shared.options, usage));
                    }
                }
            }
            if let Some(last) = stored.runs.last() {
                shared.agent.release(&last.conversation);
            }
            let mut report = piece::report_of(
                shared,
                index,
                &piece,
                &room,
                &not_served,
                &list,
                stored.runs,
            );
            report.replayed = true;
            report
        }
        Work::RefusedFinal(_, line) => {
            if let Some(m) = shared.meter {
                let usage = line_usage(&line);
                m.record_resumed(Stage::Finder, &piece::response_of(shared.options, &usage));
            }
            let mut report = piece::unstarted_report(
                shared,
                index,
                &piece,
                &list,
                FailureClass::Content,
                "the model refused to review this file on an earlier attempt and again when the \
                 file was reviewed on its own, so it was not tried again"
                    .to_string(),
                Vec::new(),
            );
            report.refused = true;
            report.replayed = true;
            report
        }
        Work::Fresh(_) => {
            let _permit = match shared.meter {
                Some(m) => match if m.projection_stops_piece() {
                    // The projected total is over the approved amount: the spend limit's own
                    // stop, with its own reason (see `crate::cost_checkpoint`).
                    Err(crate::ai_audit::CapReached)
                } else {
                    m.admit().await
                } {
                    Ok(p) => Some(p),
                    Err(crate::ai_audit::CapReached) => {
                        m.record_not_run(crate::cost_cap::NotRunPass {
                            repo: shared.repo.to_string(),
                            group: "agent finder".to_string(),
                            chunk: index + 1,
                            n_chunks: shared.n_pieces,
                            rule_batch: 1,
                            n_rule_batches: 1,
                            rule_ids: list.rule_ids().iter().map(|r| (*r).to_string()).collect(),
                            files: piece.files.clone(),
                            class: FailureClass::SpendLimit,
                        });
                        return piece::not_run_report(
                            shared,
                            index,
                            &piece,
                            &list,
                            m.outage_stopped(),
                        );
                    }
                },
                None => None,
            };
            let lines: usize = piece::lines_of(shared.files, &piece.files);
            let read_pages = piece::read_pages_of(shared.files, &piece.files);
            let limits = super::PieceLimits::for_piece_reading(
                piece.files.len(),
                read_pages,
                piece::estimate_usd(lines),
            );
            let runs =
                match piece::converse(shared, index, &piece, &room, &not_served, &list, &limits)
                    .await
                {
                    Ok(runs) => runs,
                    Err(e) => {
                        if let Some(m) = shared.meter {
                            m.record_failed(Stage::Finder, &e);
                        }
                        return piece::unstarted_report(
                            shared,
                            index,
                            &piece,
                            &list,
                            crate::ai_audit::classify_pass_error(&e),
                            e.to_string(),
                            camerata_llm::failure::attempts_of(&e),
                        );
                    }
                };
            let report = piece::report_of(shared, index, &piece, &room, &not_served, &list, runs);
            if report.failures.is_empty() {
                if let Some(m) = shared.meter {
                    let cost: f64 = report.usage.iter().filter_map(|u| u.cost_usd).sum();
                    if shared.halves.contains(&piece.key) {
                        m.half_finished();
                    }
                    m.piece_finished(Some(cost), report.problems.len());
                }
                if let Ok(text) = serde_json::to_string(&StoredPiece {
                    runs: report.runs.clone(),
                }) {
                    store(
                        shared,
                        &checkpoint_line(shared.options, piece.key.clone(), &report.usage, text),
                    );
                }
            } else {
                // Its turns bought nothing that is kept: a resume starts the piece again.
                if let Some(m) = shared.meter {
                    for usage in &report.usage {
                        m.record_wasted(Stage::Finder, &piece::response_of(shared.options, usage));
                    }
                }
                if report.split_on_resume {
                    store(
                        shared,
                        &checkpoint_line(
                            shared.options,
                            split_key(&piece.key),
                            &report.usage,
                            String::from("split"),
                        ),
                    );
                }
                if report.refused {
                    store(
                        shared,
                        &checkpoint_line(
                            shared.options,
                            refused_key(&piece.key),
                            &report.usage,
                            String::from("refused"),
                        ),
                    );
                }
            }
            report
        }
    }
}

/// Whether a finder row keyed to `rule` is not a departure from the rule's standard, so it does
/// not belong under the rule (convention model design, A1). A rule with no record keeps its
/// rows, and so does a rule whose standard is its own named defect.
fn leaves_rule(f: &Finding, rule: &str, records: &[RuleConvention]) -> bool {
    let Some(c) = records
        .iter()
        .find(|c| c.rule_id.eq_ignore_ascii_case(rule))
    else {
        return false;
    };
    if let Some(id) = &c.standard.option_id {
        if c.options.iter().any(|o| &o.option_id == id && o.violates) {
            return false;
        }
    }
    crate::conventions::row_status(c, &c.standard, f)
        != camerata_api_types::audit::PlaceStatus::Departs
}

/// Move each agent problem that is not a departure from the standard of the rule it is keyed to
/// off that rule: to the next rule it was cited to that it belongs under, else to the `AI-` id of
/// its own label. A problem is never thrown out. `findings` and `problems` are in the same order.
fn rekey_non_departing(
    findings: &mut [Finding],
    problems: &[Problem],
    records: &[RuleConvention],
    adopted: &HashSet<String>,
) {
    if findings.len() != problems.len() || records.is_empty() {
        return;
    }
    for (f, p) in findings.iter_mut().zip(problems) {
        if !f.captures.contains_key(FINDER_PROBLEM_CAPTURE)
            || !leaves_rule(f, &f.rule_id.to_ascii_uppercase(), records)
        {
            continue;
        }
        let next = f
            .captures
            .get(FINDER_RULES_CAPTURE)
            .into_iter()
            .flat_map(|c| c.split(','))
            .map(|r| r.trim().to_ascii_uppercase())
            .find(|r| adopted.contains(r) && !leaves_rule(f, r, records));
        match next {
            Some(rule) => {
                f.also_matches.retain(|r| *r != rule);
                f.rule_id = rule;
            }
            None => {
                let label = crate::ai_audit::digest_parse::normalize_model_rule_label(&p.label);
                f.rule_id = format!(
                    "AI-{}",
                    if label.is_empty() {
                        "ARCHITECTURE"
                    } else {
                        &label
                    }
                );
            }
        }
        let keep: Vec<String> = f
            .also_matches
            .iter()
            .filter(|r| !leaves_rule(f, &r.to_ascii_uppercase(), records))
            .cloned()
            .collect();
        f.also_matches = keep;
    }
}

/// The listed places of the problems as rows (`findings` and `problems` are in the same order).
/// A place that checks out against its file is a sibling row of its problem. A place that cannot
/// be checked (no such line, a blank one) is a problem needing a decision, and when the findings
/// and problems do not line up every listed place is one, so no place vanishes.
fn places_as_rows(
    repo: &str,
    findings: &[Finding],
    problems: &[Problem],
    files: &[(String, String)],
    out: &mut Vec<Finding>,
) {
    let by_path: HashMap<&str, Vec<&str>> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.lines().collect()))
        .collect();
    let aligned = findings.len() == problems.len();
    for (i, p) in problems.iter().enumerate() {
        for place in &p.places {
            let at = checklist::Place {
                path: place.path.clone(),
                line: Some(place.line),
            };
            let checked = if aligned {
                conventions::checked_place(&by_path, &at)
            } else {
                None
            };
            match checked {
                Some((path, line, text)) => {
                    let mut snippet = text.trim().to_string();
                    crate::onboard::redact::redact_string(&mut snippet);
                    add_sibling(out, &findings[i], repo, &path, line, snippet);
                }
                None => {
                    let held = conventions::held_problem_place_row(repo, &p.title, place);
                    let held_already = out.iter().any(|f| {
                        f.rule_id == held.rule_id
                            && (&f.path, f.line) == (&held.path, held.line)
                            && f.detail == held.detail
                    });
                    if !held_already {
                        out.push(held);
                    }
                }
            }
        }
    }
}

/// Add the sibling row of `primary` at another place, unless that place is the problem's own or
/// was already added for it.
fn add_sibling(
    siblings: &mut Vec<Finding>,
    primary: &Finding,
    repo: &str,
    path: &str,
    line: usize,
    snippet: String,
) {
    if (primary.path.as_str(), primary.line) == (path, line) {
        return;
    }
    let same_problem = |f: &Finding| {
        [FINDER_PIECE_CAPTURE, FINDER_PROBLEM_CAPTURE]
            .iter()
            .all(|k| f.captures.get(*k) == primary.captures.get(*k))
    };
    if siblings
        .iter()
        .any(|f| same_problem(f) && (f.path.as_str(), f.line) == (path, line))
    {
        return;
    }
    siblings.push(crate::review_tree::sibling_row(
        primary, repo, path, line, &snippet,
    ));
}

/// Find the problems of one repository with the agent finder.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub async fn find_repo(
    agent: &dyn PieceAgent,
    repo: &str,
    files: &[(String, String)],
    selected: &[AuditRule],
    corpus: &camerata_rules::RuleSet,
    controls: &RunControls,
    options: &FinderOptions,
    meter: Option<&UsageMeter>,
) -> anyhow::Result<FinderResult> {
    let _ = controls;
    if files.is_empty() {
        return Ok((
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            FinderExtras::default(),
        ));
    }
    // Redact BEFORE any code reaches a model, in one canonical order.
    let mut model_files = crate::onboard::redact::files_for_model(files);
    model_files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut selected: Vec<AuditRule> = selected.to_vec();
    selected.sort_by(|a, b| a.id.cmp(&b.id));
    let adopted_all: HashSet<String> = selected.iter().map(|r| r.id.to_ascii_uppercase()).collect();

    // Plan, then read what an earlier attempt kept.
    let units = units_for(repo, &model_files);
    let planned = plan_pieces(&units, &model_files, TARGET_BYTES, options.force_pieces);
    let stored_lines: HashMap<String, AbsorbedLine> =
        crate::checkpoint::stage_checkpoint(&format!("finder_{repo}"))
            .map(|cp| {
                cp.absorbed()
                    .into_iter()
                    .map(|l| (l.key.clone(), l))
                    .collect()
            })
            .unwrap_or_default();
    let mut works: Vec<Work> = Vec::new();
    let mut split_parents: Vec<AbsorbedLine> = Vec::new();
    let mut halves: HashSet<String> = HashSet::new();
    for p in planned {
        expand(
            p,
            &stored_lines,
            &mut works,
            &mut split_parents,
            &mut halves,
        );
    }
    if let Some(m) = meter {
        for line in &split_parents {
            m.record_resumed(
                Stage::Finder,
                &piece::response_of(options, &line_usage(line)),
            );
        }
    }
    if let Some(m) = meter {
        let held = works
            .iter()
            .filter(|w| !matches!(w, Work::Fresh(_)))
            .count();
        m.begin_pieces(works.len(), held);
        // The halves among the works, and the ones a stored attempt holds.
        let is_half = |w: &Work| halves.contains(&w.piece().key);
        m.begin_halves(
            works.iter().filter(|w| is_half(w)).count(),
            works
                .iter()
                .filter(|w| is_half(w) && !matches!(w, Work::Fresh(_)))
                .count(),
        );
    }
    let shared = Shared {
        agent,
        repo,
        files: &model_files,
        selected: &selected,
        corpus,
        options,
        meter,
        n_pieces: works.len(),
        halves,
        activity: FINDER_ACTIVITY.try_with(Clone::clone).ok(),
    };

    // Run the pieces, a few at a time, keeping their order.
    let in_flight = options.pieces_in_flight.max(1);
    let shared_ref = &shared;
    let mut reports: Vec<PieceReport> = futures::stream::iter(works.into_iter().enumerate())
        .map(|(i, w)| Box::pin(run_work(shared_ref, i, w)))
        .buffered(in_flight)
        .collect()
        .await;
    reports.sort_by_key(|r| r.index);

    let mut failed: Vec<FailedPass> = Vec::new();
    let mut extras = FinderExtras::default();
    for r in &mut reports {
        failed.append(&mut r.failures);
        extras.notes.append(&mut r.notes);
        extras
            .rules_with_problems
            .append(&mut r.rules_with_problems);
    }
    extras.rules_with_problems.sort();
    extras.rules_with_problems.dedup();

    // The joints between the parts, and every lead answered.
    let input = joints::gather(&reports);
    let mut joints_findings: Vec<Finding> = Vec::new();
    let mut joints_proposed: Vec<ProposedRule> = Vec::new();
    let mut joints_problems: Vec<Problem> = Vec::new();
    let unresolved: Vec<Finding>;
    let any_run = reports.iter().any(|r| !r.runs.is_empty());
    // The joints call waits while any part failed: the run is incomplete already, the leads
    // still become problems needing a decision, and the resume pays for the call once.
    let any_part_failed = !failed.is_empty();
    if reports.len() >= 2 && any_run && !any_part_failed {
        let key = format!("joints:{}", input.digest());
        let stored = stored_lines
            .get(&key)
            .and_then(|l| serde_json::from_str::<StoredPiece>(&l.text).ok());
        let (runs, replayed) = match stored {
            Some(s) => {
                if let Some(m) = meter {
                    for run in &s.runs {
                        for usage in &run.usage {
                            m.record_resumed(Stage::Finder, &piece::response_of(options, usage));
                        }
                    }
                }
                if let Some(last) = s.runs.last() {
                    agent.release(&last.conversation);
                }
                (Ok(s.runs), true)
            }
            None => {
                let admitted = match meter {
                    Some(m) => m.admit().await.map(Some),
                    None => Ok(None),
                };
                match admitted {
                    Err(crate::ai_audit::CapReached) => {
                        let outage = meter.is_some_and(UsageMeter::outage_stopped);
                        let cause =
                            meter.map_or(crate::cost_cap::CAUSE_LIMIT_REACHED, |m| m.stop_cause());
                        failed.push(FailedPass {
                            repo: repo.to_string(),
                            pass: "agent finder, joints between parts".to_string(),
                            reason: if outage {
                                format!(
                                    "NOT RUN: the run stopped because {} before the joints between the parts were reviewed. This inspection is incomplete.",
                                    meter.map_or(crate::run_health::STOP_PHRASE_UNREACHABLE, |m| m.stop_phrase())
                                )
                            } else {
                                format!("NOT RUN: {cause} before the joints between the parts were reviewed. This inspection is incomplete.")
                            },
                            files_not_reviewed: 0,
                            class: if outage {
                                FailureClass::OutageStop
                            } else {
                                FailureClass::SpendLimit
                            },
                            files: Vec::new(),
                            attempts: Vec::new(),
                            scope_note: false,
                        });
                        (Ok(Vec::new()), false)
                    }
                    Ok(_permit) => (
                        joints::run_joints(&shared, &input, &adopted_all).await,
                        false,
                    ),
                }
            }
        };
        match runs {
            Ok(runs) if runs.is_empty() => {
                unresolved = joints::unresolved_findings(
                    repo,
                    &input.leads,
                    &input.leads.iter().map(|l| l.n).collect::<Vec<_>>(),
                    &model_files,
                );
            }
            Ok(runs) => {
                let mut jr = joints::report_of(&shared, &input, runs, &adopted_all);
                jr.replayed = replayed;
                let clean = jr.failures.is_empty();
                failed.append(&mut jr.failures);
                extras.notes.append(&mut jr.notes);
                joints_findings = std::mem::take(&mut jr.findings);
                joints_proposed = std::mem::take(&mut jr.proposed);
                joints_problems = std::mem::take(&mut jr.problems);
                unresolved = std::mem::take(&mut jr.unresolved);
                if !replayed {
                    if clean {
                        if let Ok(text) = serde_json::to_string(&StoredPiece {
                            runs: jr.runs.clone(),
                        }) {
                            store(&shared, &checkpoint_line(options, key, &jr.usage, text));
                        }
                    } else if let Some(m) = meter {
                        for usage in &jr.usage {
                            m.record_wasted(Stage::Finder, &piece::response_of(options, usage));
                        }
                    }
                }
            }
            Err(e) => {
                if let Some(m) = meter {
                    m.record_failed(Stage::Finder, &e);
                }
                let mut record =
                    crate::ai_audit::AttemptRecord::new(&options.backend, &options.model);
                record.message = camerata_llm::failure::scrub_message(
                    &e.to_string(),
                    camerata_llm::failure::MAX_RECORD_MESSAGE_CHARS,
                );
                failed.push(FailedPass {
                    repo: repo.to_string(),
                    pass: "agent finder, joints between parts".to_string(),
                    reason: e.to_string(),
                    files_not_reviewed: 0,
                    class: crate::ai_audit::classify_pass_error(&e),
                    files: Vec::new(),
                    attempts: camerata_llm::failure::attempts_of(&e),
                    scope_note: false,
                });
                unresolved = joints::unresolved_findings(
                    repo,
                    &input.leads,
                    &input.leads.iter().map(|l| l.n).collect::<Vec<_>>(),
                    &model_files,
                );
            }
        }
    } else {
        if reports.len() >= 2 && any_run && any_part_failed {
            extras.notes.push(format!(
                "{repo}: the joints between the parts were not reviewed because a part failed; \
                 the leads became problems needing a decision, and a resume pays for the joints call once"
            ));
        }
        // A repository that is one piece makes no joints call. A lead it still holds (a place
        // outside every file it was given) is never dropped.
        unresolved = joints::unresolved_findings(
            repo,
            &input.leads,
            &input.leads.iter().map(|l| l.n).collect::<Vec<_>>(),
            &model_files,
        );
    }

    // Conventions from the answers.
    let alternatives = crate::onboard::build_rule_alternatives(
        corpus,
        selected.iter().map(|r| r.id.as_str()),
        &options.chosen_options,
    );
    let answered: Vec<usize> = reports
        .iter()
        .enumerate()
        .filter(|(_, r)| !r.effective.is_empty())
        .map(|(i, _)| i)
        .collect();
    let piece_answers: Vec<Vec<checklist::EffectiveAnswer>> = answered
        .iter()
        .map(|&i| reports[i].effective.clone())
        .collect();
    // The problems that have a row: only those can carry the places of a rule's answer.
    let problem_rows: HashSet<(usize, u32)> = answered
        .iter()
        .enumerate()
        .flat_map(|(at, &i)| {
            reports[i]
                .findings
                .iter()
                // A problem the engine threw out (a placeholder) is no report of the rule.
                .filter(|f| !crate::engine_discard::is_discarded(f))
                .filter_map(|f| f.captures.get(FINDER_PROBLEM_CAPTURE)?.parse().ok())
                .map(move |n: u32| (at, n))
        })
        .collect();
    let conv = conventions::conventions_from_answers(
        repo,
        &alternatives,
        &piece_answers,
        &model_files,
        &selected,
        corpus,
        &problem_rows,
    );
    extras.notes.extend(conv.notes.iter().cloned());
    if let Some(note) = agent.trace_note() {
        extras.notes.push(format!("{repo}: {note}"));
    }

    // A problem that is not a departure from the standard of the rule it was cited to is never
    // thrown out: it moves to the next rule it was cited to that it belongs under, else to its
    // own label. It stays listed on the rule's conforming places.
    for r in &mut reports {
        rekey_non_departing(&mut r.findings, &r.problems, &conv.records, &adopted_all);
    }
    rekey_non_departing(
        &mut joints_findings,
        &joints_problems,
        &conv.records,
        &adopted_all,
    );

    // Every further place of a problem is a sibling row of it, from the model's own `places` (a
    // cited rule's option places are census only). No producer writes
    // `also_locations`; each place is a copy of the problem at its own path, line and snippet,
    // in the same sub-group, so the places share one fix.
    let mut siblings: Vec<Finding> = Vec::new();
    for r in &reports {
        places_as_rows(repo, &r.findings, &r.problems, &model_files, &mut siblings);
    }
    places_as_rows(
        repo,
        &joints_findings,
        &joints_problems,
        &model_files,
        &mut siblings,
    );

    // Every problem so far, in piece order.
    let mut rows: Vec<Finding> = Vec::new();
    let mut proposed: Vec<ProposedRule> = Vec::new();
    for r in &mut reports {
        rows.append(&mut r.findings);
        proposed.append(&mut r.proposed);
    }
    extras.problems_total = rows.len() + joints_findings.len();
    rows.append(&mut joints_findings);
    proposed.append(&mut joints_proposed);
    extras.problems_without_rule = rows
        .iter()
        .filter(|f| !adopted_all.contains(&f.rule_id.to_ascii_uppercase()))
        .count();
    rows.extend(siblings);
    rows.extend(unresolved);
    let (departures, held_places, place_notes) =
        conventions::departure_and_held_rows(&conv.records, &rows, corpus);
    extras.notes.extend(place_notes);
    rows.extend(departures);
    rows.extend(held_places);
    // The deterministic backstop: a reviewer-addressed phrase in the code is a row of its own, so
    // the plant is seen even when the model obeyed it and reported nothing (the same checker the
    // floor runs when the rule is armed; the combined set collapses the same place later).
    let planted = crate::onboard::audit_architectural(
        repo,
        &model_files,
        &HashSet::from([
            camerata_checks::ai_reviewer_injection_checker::RULE_AI_REVIEWER_INSTRUCTIONS,
        ]),
    );
    for row in planted {
        let already = rows.iter().any(|f| {
            f.rule_id.eq_ignore_ascii_case(&row.rule_id)
                && (f.path.as_str(), f.line) == (row.path.as_str(), row.line)
        });
        if !already {
            rows.push(row);
        }
    }
    let mut seen = HashSet::new();
    proposed.retain(|p| seen.insert(p.id.clone()));

    let mut stage_samples: Vec<StageSample> = Vec::new();
    stage_samples.push(StageSample {
        stage: "agent-finder".to_string(),
        rows_in: rows.len(),
        rows_out: rows.len(),
        accounting: StageAccounting::default(),
        model: Some(options.model.clone()),
    });

    // Anchor, set aside what the parser threw out, same-rule same-place dedup and merge.
    crate::ai_audit::resolve_finding_lines(&mut rows, &model_files);
    let (thrown_out, kept) = crate::engine_discard::split_discarded(rows);
    let mut parked = thrown_out;
    let mut rows = kept;
    {
        let rows_in = rows.len();
        crate::ai_audit::dedup_findings_keep_max_severity(&mut rows);
        let rows_after_dedup = rows.len();
        crate::review_tree::mark_located(&mut rows, &model_files);
        let collapse =
            crate::review_tree::collapse_identical(rows, &crate::review_tree::ToolMap::builtin());
        let mut acc = collapse.accounting();
        acc.deduped = rows_in.saturating_sub(rows_after_dedup);
        rows = collapse.rows;
        stage_samples.push(StageSample {
            stage: "ai-location-merge".to_string(),
            rows_in,
            rows_out: rows.len(),
            accounting: acc,
            model: None,
        });
    }

    // The wording floors and ceilings, which run after the agent's own rating.
    rows = crate::ai_audit::apply_severity_ceiling_rules(
        crate::ai_audit::apply_severity_calibration_rules(rows),
    );

    // A rule with no convention emits no per-site problems: any stray problem under it is set
    // aside, and exactly one information row stands for the rule.
    let recs = conv.recommendations;
    let no_convention: HashSet<String> = recs
        .iter()
        .filter(|r| r.no_convention)
        .map(|r| r.rule_id.to_ascii_uppercase())
        .collect();
    let mut dropped_no_convention = 0usize;
    if !no_convention.is_empty() {
        for f in &mut rows {
            let bare = f.rule_id.trim_start_matches("AI-").to_ascii_uppercase();
            // An agent problem is never set aside (it was moved off the rule above).
            if no_convention.contains(&bare)
                && !f.captures.contains_key(FINDER_PROBLEM_CAPTURE)
                && !crate::engine_discard::is_discarded(f)
            {
                crate::engine_discard::mark(f, crate::engine_discard::NO_CONVENTION);
                dropped_no_convention += 1;
            }
        }
    }
    let (thrown_out, kept) = crate::engine_discard::split_discarded(std::mem::take(&mut rows));
    parked.extend(thrown_out);
    let mut verified = kept;
    if dropped_no_convention > 0 {
        let mut acc = StageAccounting::default();
        acc.other.push((
            "no_convention_rule_filtered".to_string(),
            dropped_no_convention,
        ));
        stage_samples.push(StageSample {
            stage: "agent-finder-no-convention-filter".to_string(),
            rows_in: verified.len() + dropped_no_convention,
            rows_out: verified.len(),
            accounting: acc,
            model: None,
        });
    }
    if !conv.violates.is_empty() {
        let rows_in = verified.len();
        verified.extend(conv.violates);
        let mut acc = StageAccounting::default();
        acc.push_added("violates_option_emitted", verified.len() - rows_in);
        stage_samples.push(StageSample {
            stage: "violates-option-emission".to_string(),
            rows_in,
            rows_out: verified.len(),
            accounting: acc,
            model: None,
        });
    }
    // Tag every problem under a rule judged against one option with that option.
    if !recs.is_empty() {
        let by_rule: HashMap<&str, &str> = recs
            .iter()
            .filter(|r| !r.no_convention)
            .map(|r| (r.rule_id.as_str(), r.recommended_option_id.as_str()))
            .collect();
        for f in &mut verified {
            if let Some(id) = by_rule.get(f.rule_id.to_ascii_uppercase().as_str()) {
                f.evaluated_option_id = Some((*id).to_string());
            }
        }
    }
    // Kinds, then the three information rows, as the engine writes them.
    let mut records = conv.records;
    crate::conventions::tag(&mut verified, &mut records);
    extras.conventions = records;
    let mut skipped_rules: Vec<(String, String)> = Vec::new();
    {
        let plan =
            crate::scan_routing::plan_routes(&selected, &model_files, options.applies_to_routing);
        let _ =
            crate::ai_audit::routing::runnable_units(&model_files, &plan, None, &mut skipped_rules);
    }
    for (stage, new_rows) in [
        (
            "convention-observed-emission",
            crate::ai_audit::conventions_observed_findings(repo, &verified, &recs, &alternatives),
        ),
        (
            "no-convention-emission",
            crate::ai_audit::no_convention_findings(repo, &recs),
        ),
        (
            "not-assessed-emission",
            crate::ai_audit::not_assessed_findings(repo, &skipped_rules),
        ),
    ] {
        if new_rows.is_empty() {
            continue;
        }
        let rows_in = verified.len();
        verified.extend(new_rows);
        let mut acc = StageAccounting::default();
        acc.push_added(stage, verified.len() - rows_in);
        stage_samples.push(StageSample {
            stage: stage.to_string(),
            rows_in,
            rows_out: verified.len(),
            accounting: acc,
            model: None,
        });
    }
    verified.extend(parked);

    // The extras: coverage, partial rules, rules every piece found not applicable.
    let mut rule_pieces: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
    for r in &reports {
        for rule in &r.checklist_rules {
            let entry = rule_pieces.entry(rule.to_ascii_uppercase()).or_default();
            entry.0 += 1;
            if r.not_applicable
                .iter()
                .any(|n| n.eq_ignore_ascii_case(rule))
            {
                entry.1 += 1;
            }
            if r.not_a_problem_here
                .iter()
                .any(|n| n.eq_ignore_ascii_case(rule))
            {
                entry.2 += 1;
            }
        }
    }
    for r in &mut reports {
        if let Some(c) = r.coverage.take() {
            extras.coverage.push(c);
        }
        extras.partial_rules.append(&mut r.partial_rules);
        extras
            .not_a_problem_notes
            .append(&mut r.not_a_problem_lines);
    }
    let partial_ids: HashSet<String> = extras
        .partial_rules
        .iter()
        .map(|p| p.rule_id.to_ascii_uppercase())
        .collect();
    extras.not_applicable_rules = rule_pieces
        .iter()
        .filter(|(rule, (asked, na, _))| *asked > 0 && asked == na && !partial_ids.contains(*rule))
        .map(|(rule, _)| rule.clone())
        .collect();
    extras.not_a_problem_rules = rule_pieces
        .into_iter()
        .filter(|(rule, (asked, na, nph))| {
            *asked > 0 && *nph > 0 && asked == &(na + nph) && !partial_ids.contains(rule)
        })
        .map(|(rule, _)| rule)
        .collect();
    Ok((verified, proposed, recs, failed, stage_samples, extras))
}

tokio::task_local! {
    static FINDER_ACTIVITY: FinderActivity;
}

/// Run `fut` with the finder's model calls shown in Agent activity when `activity` is `Some`,
/// carried with the future like the finder setup: never global.
pub async fn with_finder_activity<F: std::future::Future>(
    activity: Option<FinderActivity>,
    fut: F,
) -> F::Output {
    match activity {
        Some(a) => FINDER_ACTIVITY.scope(a, fut).await,
        None => fut.await,
    }
}

tokio::task_local! {
    static AGENT_OVERRIDE: std::sync::Arc<dyn PieceAgent>;
}

/// Run `fut` with `agent` standing in for the run's piece agent. The one test seam of the finder,
/// carried with the future the way the run's checkpoint folder is
/// ([`crate::checkpoint::with_run_checkpoints`]): never global, so two runs in one process cannot
/// see each other's agent. Production never sets it.
pub async fn with_agent_override<F: std::future::Future>(
    agent: std::sync::Arc<dyn PieceAgent>,
    fut: F,
) -> F::Output {
    AGENT_OVERRIDE.scope(agent, fut).await
}

/// Whether a test agent stands in for the run's piece agent (see [`with_agent_override`]).
pub fn agent_override_is_set() -> bool {
    #[cfg(feature = "test-support")]
    {
        if AGENT_FACTORY_OVERRIDE.try_with(|_| ()).is_ok() {
            return true;
        }
    }
    AGENT_OVERRIDE.try_with(|_| ()).is_ok()
}

/// Builds a test agent from the run's shared meter (`test-support` only).
#[cfg(feature = "test-support")]
pub type AgentFactoryOverride = std::sync::Arc<
    dyn Fn(std::sync::Arc<UsageMeter>) -> std::sync::Arc<dyn PieceAgent> + Send + Sync,
>;

#[cfg(feature = "test-support")]
tokio::task_local! {
    static AGENT_FACTORY_OVERRIDE: AgentFactoryOverride;
}

/// Run `fut` with a test agent that is built, once for each repository, from the run's own
/// shared meter: the seam that lets a test put the real API transport over a scripted wire, with
/// every turn counted on the run's meter. Carried with the future like [`with_agent_override`].
#[cfg(feature = "test-support")]
pub async fn with_agent_factory_override<F: std::future::Future>(
    factory: AgentFactoryOverride,
    fut: F,
) -> F::Output {
    AGENT_FACTORY_OVERRIDE.scope(factory, fut).await
}

pub use super::cli_agent::EngagementKind;

/// Everything the command line decides about the run's finder agent, as one value built once
/// before the audit starts and carried into it ([`with_finder_setup`]). The command line never
/// builds a finder agent itself: the run builds it, on the run's own shared meter, from this.
#[derive(Clone, PartialEq, Eq)]
pub struct FinderSetup {
    /// The run's folder for the command line transport's rooms (never inside the client's repo).
    pub rooms_dir: std::path::PathBuf,
    /// The Anthropic key from the credential store. `None` when there is none.
    pub api_key: Option<String>,
    /// The kind of engagement the run is for.
    pub engagement: EngagementKind,
}

impl std::fmt::Debug for FinderSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FinderSetup")
            .field("rooms_dir", &self.rooms_dir)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("engagement", &self.engagement)
            .finish()
    }
}

/// The finder setup for a run: the rooms folder (an engine-owned folder, never inside the
/// client's repository), the saved Anthropic key and the kind of engagement, built once here and
/// carried into the audit. The run builds the agent from it on its own shared meter. `Err` before
/// anything is spent when the API backend has no key. The ONE place a [`FinderSetup`] is built
/// outside tests: the command line and the server's scan path both come through
/// [`finder_setup_for_run`], which calls this.
pub fn finder_setup(
    backend: crate::llm::Backend,
    api_key: Option<String>,
    rooms_dir: std::path::PathBuf,
    client_engagement: bool,
) -> Result<FinderSetup, String> {
    let api_key = api_key
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty());
    if backend == crate::llm::Backend::Api && api_key.is_none() {
        return Err("the API backend needs an Anthropic key".to_string());
    }
    Ok(FinderSetup {
        rooms_dir,
        api_key,
        engagement: if client_engagement {
            EngagementKind::Client
        } else {
            EngagementKind::Internal
        },
    })
}

/// The setup a run carries, or `None` when it runs the engine finder. Only a run whose controls
/// choose the agent finder, whose AI review is not blocked, and that has no test agent standing
/// in gets one. The rooms folder is `finder-rooms` inside the run's own folder (`run_dir`), else
/// inside the per-user state folder. The key comes from the saved credentials, never the
/// environment of the caller's choosing.
pub fn finder_setup_for_run(
    controls: &RunControls,
    ai_review_blocked: bool,
    backend: crate::llm::Backend,
    creds: &dyn crate::credentials::CredentialStore,
    run_dir: Option<&std::path::Path>,
    client_engagement: bool,
) -> Result<Option<FinderSetup>, String> {
    if controls.finder != super::FinderChoice::Agent || ai_review_blocked || agent_override_is_set()
    {
        return Ok(None);
    }
    let rooms_dir = run_dir
        .map_or_else(
            crate::scan_cache::state_dir::dir,
            std::path::Path::to_path_buf,
        )
        .join("finder-rooms");
    finder_setup(
        backend,
        crate::credentials::anthropic_api_key(creds),
        rooms_dir,
        client_engagement,
    )
    .map(Some)
}

/// Run `fut` with `setup` as the run's finder setup when there is one, else as it is.
pub async fn with_optional_finder_setup<F: std::future::Future>(
    setup: Option<FinderSetup>,
    fut: F,
) -> F::Output {
    match setup {
        Some(setup) => with_finder_setup(setup, fut).await,
        None => fut.await,
    }
}

tokio::task_local! {
    static FINDER_SETUP: std::sync::Arc<FinderSetup>;
}

/// Run `fut` with `setup` as the run's finder setup, carried with the future like the test seam
/// [`with_agent_override`] and the run's checkpoint folder: never global.
pub async fn with_finder_setup<F: std::future::Future>(setup: FinderSetup, fut: F) -> F::Output {
    FINDER_SETUP.scope(std::sync::Arc::new(setup), fut).await
}

/// What the run needs to build its piece agent, resolved from the setup.
pub(crate) struct AgentBuild {
    pub backend: crate::llm::Backend,
    pub api_key: Option<String>,
    pub work_dir: std::path::PathBuf,
    pub engagement: EngagementKind,
}

/// Resolve the agent build from the run's setup. With no setup the command line backend is an
/// error (its rooms never go under the system temp folder), the key falls back to the one the
/// run's model client holds, and the engagement kind is the strict one (client).
pub(crate) fn resolve_agent_build(
    setup: Option<&FinderSetup>,
    llm_key: Option<&str>,
    backend: crate::llm::Backend,
) -> anyhow::Result<AgentBuild> {
    let work_dir = match (setup, backend) {
        (Some(s), _) => s.rooms_dir.clone(),
        (None, crate::llm::Backend::Cli) => anyhow::bail!(
            "the command line backend needs a finder setup naming a folder for its rooms; none \
             was given, so nothing was run"
        ),
        (None, crate::llm::Backend::Api) => std::path::PathBuf::new(),
    };
    Ok(AgentBuild {
        backend,
        api_key: setup
            .and_then(|s| s.api_key.clone())
            .or_else(|| llm_key.map(str::to_string)),
        work_dir,
        engagement: setup.map_or(EngagementKind::Client, |s| s.engagement),
    })
}

/// Find the problems of one repository for the scan loop: build the run's piece agent (the test
/// override, else the factory [`super::piece_agent_for`] from the run's finder setup, on the
/// run's shared meter) and call [`find_repo`]. The model, the force-pieces setting and the pieces
/// in flight come from the run's controls and backend, never from the environment.
#[allow(clippy::too_many_arguments)]
pub async fn find_repo_for_run(
    llm: &dyn crate::llm::LlmPort,
    repo: &str,
    files: &[(String, String)],
    selected: &[AuditRule],
    corpus: Option<&camerata_rules::RuleSet>,
    controls: &RunControls,
    chosen_options: &HashMap<String, String>,
    backend_resolution: &crate::llm::BackendResolution,
    meter: Option<&std::sync::Arc<UsageMeter>>,
) -> anyhow::Result<FinderResult> {
    find_repo_for_run_with(
        llm,
        repo,
        files,
        selected,
        corpus,
        controls,
        chosen_options,
        backend_resolution,
        meter,
        &|controls, build, meter| {
            super::piece_agent_for(
                controls,
                build.backend,
                build.api_key.as_deref(),
                &build.work_dir,
                meter,
                build.engagement,
            )
        },
    )
    .await
}

/// The agent factory [`find_repo_for_run_with`] calls once: the run's controls, what the setup
/// resolved to and the run's shared meter.
pub(crate) type AgentFactory<'a> = dyn Fn(&RunControls, &AgentBuild, std::sync::Arc<UsageMeter>) -> anyhow::Result<Box<dyn PieceAgent>>
    + Sync
    + 'a;

/// [`find_repo_for_run`] with the factory a test can replace.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn find_repo_for_run_with(
    llm: &dyn crate::llm::LlmPort,
    repo: &str,
    files: &[(String, String)],
    selected: &[AuditRule],
    corpus: Option<&camerata_rules::RuleSet>,
    controls: &RunControls,
    chosen_options: &HashMap<String, String>,
    backend_resolution: &crate::llm::BackendResolution,
    meter: Option<&std::sync::Arc<UsageMeter>>,
    factory: &AgentFactory<'_>,
) -> anyhow::Result<FinderResult> {
    let backend = match backend_resolution {
        crate::llm::BackendResolution::Api => crate::llm::Backend::Api,
        crate::llm::BackendResolution::Cli | crate::llm::BackendResolution::Blocked { .. } => {
            crate::llm::Backend::Cli
        }
    };
    let options = FinderOptions {
        model: super::finder_model_for(controls),
        force_pieces: controls.finder_force_pieces,
        applies_to_routing: controls.efficiency.applies_to_routing,
        pieces_in_flight: super::pieces_in_flight_for(backend),
        chosen_options: chosen_options.clone(),
        backend: match backend {
            crate::llm::Backend::Api => "api".to_string(),
            crate::llm::Backend::Cli => "cli".to_string(),
        },
    };
    let empty = camerata_rules::RuleSet::default();
    let corpus = corpus.unwrap_or(&empty);
    if let Ok(agent) = AGENT_OVERRIDE.try_with(std::sync::Arc::clone) {
        return find_repo(
            &*agent,
            repo,
            files,
            selected,
            corpus,
            controls,
            &options,
            meter.map(|m| &**m),
        )
        .await;
    }
    #[cfg(feature = "test-support")]
    {
        if let Ok(factory) = AGENT_FACTORY_OVERRIDE.try_with(std::sync::Arc::clone) {
            let shared_meter = meter.map_or_else(
                || std::sync::Arc::new(UsageMeter::with_controls(controls.clone())),
                std::sync::Arc::clone,
            );
            let agent = factory(shared_meter);
            return find_repo(
                &*agent,
                repo,
                files,
                selected,
                corpus,
                controls,
                &options,
                meter.map(|m| &**m),
            )
            .await;
        }
    }
    let setup = FINDER_SETUP.try_with(std::sync::Arc::clone).ok();
    let llm_key = llm
        .as_any()
        .downcast_ref::<crate::llm::Llm>()
        .and_then(crate::llm::Llm::api_key);
    let build = resolve_agent_build(setup.as_deref(), llm_key, backend)?;
    // The agent is built with the run's shared meter (an agent with no meter refuses to send).
    let shared_meter = meter.map_or_else(
        || std::sync::Arc::new(UsageMeter::with_controls(controls.clone())),
        std::sync::Arc::clone,
    );
    let agent = factory(controls, &build, shared_meter)?;
    find_repo(
        &*agent,
        repo,
        files,
        selected,
        corpus,
        controls,
        &options,
        meter.map(|m| &**m),
    )
    .await
}

/// The captures every agent problem carries, for tests that look for one.
#[allow(dead_code)]
pub(crate) const PIECE_CAPTURE: &str = FINDER_PIECE_CAPTURE;
#[allow(dead_code)]
pub(crate) const SURE_CAPTURE: &str = FINDER_SURE_CAPTURE;
