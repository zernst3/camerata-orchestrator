//! One piece end to end: build the room and the checklist, admit it with the meter, run the
//! agent, read the output, compute coverage from the trace, send unread files again once, check
//! the checklist and ask once more for the rules that have no usable answer, and turn the problems
//! into findings.
//!
//! The reading of a finished conversation ([`interpret`]) is a pure function of the recorded
//! runs, so a resume replays a stored piece through exactly the code a fresh piece goes through.

use std::collections::{BTreeMap, HashSet};

use super::super::checklist::{
    CheckOutcome, Checklist, ChecklistFile, Citation, Effective, EffectiveAnswer,
};
use super::super::coverage::{coverage_of, PieceCoverage};
use super::super::reply::{self, Lead, PieceOutput, Problem, Unusable};
use super::super::room::{NotServed, Room};
use super::super::{
    AgentRun, AgentStop, FollowUp, Piece, PieceAgent, PieceLimits, SEND_AGAIN_MESSAGE,
};
use super::{FinderOptions, PartialRule};
use crate::ai_audit::{AttemptRecord, FailedPass, FailureClass, UsageMeter};
use crate::onboard::{Finding, ProposedRule};
use crate::scan_routing::AuditRule;
use crate::stage_telemetry::Stage;

/// Everything the pieces of one repository share.
pub(super) struct Shared<'a> {
    pub agent: &'a dyn PieceAgent,
    pub repo: &'a str,
    /// The repository's model-facing files: redacted, in path order.
    pub files: &'a [(String, String)],
    pub selected: &'a [AuditRule],
    pub corpus: &'a camerata_rules::RuleSet,
    pub options: &'a FinderOptions,
    pub meter: Option<&'a UsageMeter>,
    /// How many pieces (and halves of refused pieces) the repository is reviewed in.
    pub n_pieces: usize,
}

/// The recorded conversation of a finished piece: what a resume replays.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct StoredPiece {
    pub runs: Vec<AgentRun>,
}

/// What one piece produced.
#[derive(Debug, Default)]
pub(super) struct PieceReport {
    pub index: usize,
    pub problems: Vec<Problem>,
    /// One finding for each problem, in the same order (the agent's own rating already applied).
    pub findings: Vec<Finding>,
    pub proposed: Vec<ProposedRule>,
    pub leads: Vec<Lead>,
    pub card: Option<String>,
    pub coverage: Option<PieceCoverage>,
    /// The checklist answers after the checks, for the conventions.
    pub effective: Vec<EffectiveAnswer>,
    pub partial_rules: Vec<PartialRule>,
    /// Rules this piece answered "not applicable".
    pub not_applicable: Vec<String>,
    /// Rules whose answer in this piece stood as "not a problem here".
    pub not_a_problem_here: Vec<String>,
    /// One line for each "not a problem here" answer (rule, place, reason), for the operator's
    /// own list. Never a client row.
    pub not_a_problem_lines: Vec<String>,
    pub failures: Vec<FailedPass>,
    /// Operator-only remarks (stray files).
    pub notes: Vec<String>,
    pub runs: Vec<AgentRun>,
    /// True when the model refused the piece.
    pub refused: bool,
    pub replayed: bool,
    /// The rule ids on the piece's checklist.
    pub checklist_rules: Vec<String>,
    /// The cost of the piece's turns, for the checkpoint line.
    pub usage: Vec<super::super::TurnUsage>,
    /// Every rule an accepted `problems` answer or a citation names: each of them has a problem.
    pub rules_with_problems: Vec<String>,
}

pub(super) fn lines_of(files: &[(String, String)], paths: &[String]) -> usize {
    let wanted: HashSet<&str> = paths.iter().map(String::as_str).collect();
    files
        .iter()
        .filter(|(p, _)| wanted.contains(p.as_str()))
        .map(|(_, t)| t.lines().count())
        .sum()
}

/// The estimate a piece's spend ceiling is four times (design section 7): about $1.00 for each
/// 1,000 lines plus $0.80 for the piece.
pub(super) fn estimate_usd(lines: usize) -> f64 {
    0.8 + lines as f64 / 1000.0
}

fn is_known_output(file: &str) -> bool {
    let name = file
        .strip_prefix(super::super::room::OUT_DIR)
        .unwrap_or(file);
    name == "checklist.json"
        || name == "leads.json"
        || name == "unread.json"
        // Any `problems*.json` is one the agent meant as a problems file, so a stray name
        // (`problems_2.json`, `problems-2a.json`) is repaired and then a failed part. A
        // `problems.md` or `problems-notes.txt` is not a problems output.
        || (name.starts_with("problems") && name.ends_with(".json"))
}

/// A file named like a problems file (a stray name too: the reader takes one that parses as a
/// problems list, and the others are refused, so both are joined across steps the same way).
fn is_problems_file(key: &str) -> bool {
    let name = key.strip_prefix(super::super::room::OUT_DIR).unwrap_or(key);
    name.starts_with("problems") && name.ends_with(".json")
}

/// Whether `text` is a problems file the reader accepts. `joined` is true for the engine's own
/// joined text, which is not held to the per-file cap.
fn problems_text_is_usable(key: &str, text: &str, joined: bool) -> bool {
    let one: BTreeMap<String, String> = [(key.to_string(), text.to_string())].into();
    let exempt: HashSet<String> = if joined {
        [key.to_string()].into()
    } else {
        HashSet::new()
    };
    reply::parse_outputs_joined(&one, &[], &exempt)
        .unusable
        .is_empty()
}

/// The items of a problems file's text (a list, or an object holding `problems`).
fn problem_items(text: &str) -> Option<Vec<serde_json::Value>> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    v.as_array()
        .or_else(|| v.get("problems").and_then(serde_json::Value::as_array))
        .cloned()
}

fn item_number(item: &serde_json::Value) -> Option<u64> {
    item.get("n").and_then(serde_json::Value::as_u64)
}

/// The place an item names: its path and line.
fn item_place(item: &serde_json::Value) -> (Option<String>, Option<u64>) {
    (
        item.get("path")
            .and_then(serde_json::Value::as_str)
            .map(|p| p.trim().trim_start_matches("./").to_string()),
        item.get("line").and_then(serde_json::Value::as_u64),
    )
}

/// A problem number an earlier step used at one place that a later step used again at another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Clash {
    /// The problems file both versions are of.
    pub file: String,
    pub n: u64,
    /// Where the earlier step put the problem.
    pub path: String,
    pub line: Option<u64>,
}

/// Join a later version of a problems file onto an earlier one. A number only the earlier version
/// holds stays. A number both hold at the same path and line is a correction: the later wins. A
/// number both hold at a different place is a clash: both stay in the text, which the reader then
/// refuses ("problem number used twice") so the agent is asked to write the file again; the
/// clashes are returned with the earlier place. `None` when either version cannot be used (the
/// later one then simply replaces the earlier). `earlier_joined` is true when the earlier version
/// is itself the engine's joined text.
fn join_problem_versions(
    key: &str,
    earlier: &str,
    earlier_joined: bool,
    later: &str,
) -> Option<(String, Vec<Clash>)> {
    if !problems_text_is_usable(key, earlier, earlier_joined)
        || !problems_text_is_usable(key, later, false)
    {
        return None;
    }
    let later_items = problem_items(later)?;
    let mut clashes = Vec::new();
    let mut joined: Vec<serde_json::Value> = Vec::new();
    for old in problem_items(earlier)? {
        let Some(n) = item_number(&old) else {
            continue;
        };
        let reused = later_items
            .iter()
            .filter(|new| item_number(new) == Some(n))
            .collect::<Vec<_>>();
        // Dropped only when the later version corrects it, at the same place.
        if reused.iter().any(|new| item_place(new) == item_place(&old)) {
            continue;
        }
        if !reused.is_empty() {
            let (path, line) = item_place(&old);
            clashes.push(Clash {
                file: key.to_string(),
                n,
                path: path.unwrap_or_default(),
                line,
            });
        }
        joined.push(old);
    }
    joined.extend(later_items);
    Some((serde_json::Value::Array(joined).to_string(), clashes))
}

/// What joining the outputs of every run gave.
struct Joined {
    outputs: BTreeMap<String, String>,
    /// The problems files where a later version joined onto a base that held a problem the later
    /// version did not (those problems were kept), in path order.
    kept: Vec<String>,
    /// The problems files whose text is the engine's own join (exempt from the per-file cap).
    joined: HashSet<String>,
    clashes: Vec<Clash>,
}

/// The outputs of every run joined, and the problems files whose earlier problems were kept.
///
/// A file that is not a problems file: a later run replaces an earlier one. A problems file keeps
/// a join base, the LAST USABLE version joined so far; a later usable version is joined onto it
/// (see [`join_problem_versions`]), so a step that wrote broken text in between does not lose the
/// problems the base held. A later version that is not usable is the file's text as it stands
/// (the reader refuses it, and the agent is asked to write it again), but it never becomes the
/// base.
fn join_outputs(runs: &[AgentRun]) -> Joined {
    let mut result = Joined {
        outputs: BTreeMap::new(),
        kept: Vec::new(),
        joined: HashSet::new(),
        clashes: Vec::new(),
    };
    let mut base: BTreeMap<String, String> = BTreeMap::new();
    for run in runs {
        for (k, v) in &run.outputs {
            if !is_problems_file(k) || !problems_text_is_usable(k, v, false) {
                result.joined.remove(k);
                result.outputs.insert(k.clone(), v.clone());
                continue;
            }
            let Some(earlier) = base.get(k) else {
                base.insert(k.clone(), v.clone());
                result.joined.remove(k);
                result.outputs.insert(k.clone(), v.clone());
                continue;
            };
            let earlier_joined =
                result.joined.contains(k) && result.outputs.get(k) == Some(earlier);
            let Some((text, clashes)) = join_problem_versions(k, earlier, earlier_joined, v) else {
                base.insert(k.clone(), v.clone());
                result.joined.remove(k);
                result.outputs.insert(k.clone(), v.clone());
                continue;
            };
            let later_numbers: HashSet<u64> = problem_items(v)
                .unwrap_or_default()
                .iter()
                .filter_map(item_number)
                .collect();
            let held_only_earlier = problem_items(earlier)
                .unwrap_or_default()
                .iter()
                .filter_map(item_number)
                .any(|n| !later_numbers.contains(&n));
            if held_only_earlier && !result.kept.contains(k) {
                result.kept.push(k.clone());
            }
            result.clashes.retain(|c| &c.file != k);
            result.clashes.extend(clashes);
            result.joined.insert(k.clone());
            if problems_text_is_usable(k, &text, true) {
                base.insert(k.clone(), text.clone());
            }
            result.outputs.insert(k.clone(), text);
        }
    }
    result.kept.sort();
    result
}

/// The problems files a later step wrote again where the problems only an earlier version held
/// were kept, in path order.
pub(super) fn rewritten_problem_files(runs: &[AgentRun]) -> Vec<String> {
    join_outputs(runs).kept
}

/// The outputs of every run, later runs replacing earlier ones that wrote the same file (a
/// problems file is joined, not replaced), and the files whose text is the engine's own join.
pub(super) fn merged_outputs(runs: &[AgentRun]) -> (BTreeMap<String, String>, HashSet<String>) {
    let joined = join_outputs(runs);
    (joined.outputs, joined.joined)
}

/// The number clashes between steps, see [`Clash`].
fn number_clashes(runs: &[AgentRun]) -> Vec<Clash> {
    join_outputs(runs).clashes
}

/// The checklist answers of every run joined: an answer a later run gives for a rule replaces the
/// earlier one, the others stay, and the citations of all runs are kept. A follow-up that
/// rewrites `checklist.json` with only the rules it was asked about loses nothing.
fn merged_checklist(runs: &[AgentRun]) -> Option<ChecklistFile> {
    let mut joined: Option<ChecklistFile> = None;
    for run in runs {
        for (key, text) in &run.outputs {
            if !key.ends_with("checklist.json") {
                continue;
            }
            let one: BTreeMap<String, String> = [(key.clone(), text.clone())].into();
            let Some(file) = reply::parse_outputs(&one, &[]).checklist else {
                continue;
            };
            match joined.as_mut() {
                None => joined = Some(file),
                Some(prev) => {
                    for answer in file.answers {
                        prev.answers
                            .retain(|a| !a.rule.eq_ignore_ascii_case(&answer.rule));
                        prev.answers.push(answer);
                    }
                    for c in file.citations {
                        if !prev.citations.contains(&c) {
                            prev.citations.push(c);
                        }
                    }
                }
            }
        }
    }
    joined
}

/// The reading of a conversation so far.
pub(super) struct View {
    pub output: PieceOutput,
    pub known_unusable: Vec<Unusable>,
    pub stray: Vec<Unusable>,
    pub outcome: CheckOutcome,
    pub coverage: PieceCoverage,
    /// Unread piece files the room can serve again.
    pub unread_sendable: Vec<String>,
    pub citations: Vec<Citation>,
    /// Problem numbers an earlier step used that a later step used again at another place.
    pub clashes: Vec<Clash>,
}

impl View {
    fn needs_repair(&self) -> bool {
        !self.known_unusable.is_empty() || self.output.problems_files == 0
    }

    fn needs_rules_follow_up(&self) -> bool {
        !self.outcome.missing.is_empty() || !self.outcome.not_completed.is_empty()
    }
}

/// Read what the conversation has produced so far.
pub(super) fn interpret(
    room: &Room,
    not_served: &[NotServed],
    checklist: &Checklist,
    piece: &Piece,
    runs: &[AgentRun],
) -> View {
    let (outputs, joined_files) = merged_outputs(runs);
    let mut output = reply::parse_outputs_joined(&outputs, room.piece_files(), &joined_files);
    let (known_unusable, stray): (Vec<Unusable>, Vec<Unusable>) =
        std::mem::take(&mut output.unusable)
            .into_iter()
            .partition(|u| is_known_output(&u.file));
    // A checklist file that is unusable now may have had a good version in an earlier step.
    let joined = merged_checklist(runs);
    let file = joined.clone().or_else(|| output.checklist.clone());
    output.checklist = file.clone();
    // A step that did not end `Finished` reviewed nothing: its reads are not evidence.
    let trace: Vec<super::super::ToolEvent> = runs
        .iter()
        .filter(|r| r.stop.is_finished())
        .flat_map(|r| r.trace.iter().cloned())
        .collect();
    let shown: Vec<String> = runs
        .iter()
        .filter(|r| r.stop.is_finished())
        .flat_map(|r| r.shown_in_full.iter().cloned())
        .collect();
    let outcome = checklist.check_with_shown(
        &file.clone().unwrap_or_default(),
        &output.problems,
        &trace,
        &room.piece_line_counts(),
        &shown,
    );
    let coverage = coverage_of(
        &piece.key,
        runs,
        &room.piece_line_counts(),
        not_served,
        room.context_files(),
        output.unread.as_deref(),
    );
    let unserved: HashSet<&str> = not_served.iter().map(|n| n.path.as_str()).collect();
    let unread_sendable: Vec<String> = coverage
        .still_unread
        .iter()
        .filter(|p| !unserved.contains(p.as_str()))
        .cloned()
        .collect();
    // A problem the agent cited, or named under a rule's `problems` answer, breaks that rule.
    let mut citations = outcome.valid_citations.clone();
    for answer in &outcome.effective {
        if let Effective::Problems(numbers) = &answer.state {
            for &n in numbers {
                let c = Citation {
                    problem: n,
                    rule: answer.rule.to_ascii_uppercase(),
                };
                if !citations.contains(&c) {
                    citations.push(c);
                }
            }
        }
    }
    View {
        output,
        known_unusable,
        stray,
        outcome,
        coverage,
        unread_sendable,
        citations,
        clashes: number_clashes(runs),
    }
}

fn repair_message(view: &View) -> String {
    let mut text = String::from(
        "Some of your output files could not be used. Write each one again in full.\n",
    );
    text.push_str(&super::super::brief_text::repair_reminder());
    for u in &view.known_unusable {
        let clashes: Vec<&Clash> = view.clashes.iter().filter(|c| c.file == u.file).collect();
        if clashes.is_empty() {
            text.push_str(&format!("- {} {}\n", u.file, u.reason));
            continue;
        }
        for c in clashes {
            let at = c
                .line
                .map_or_else(|| c.path.clone(), |l| format!("{}:{l}", c.path));
            text.push_str(&format!(
                "- {}: an earlier step used problem number {} at {at}; keep that problem as it \
                 was and renumber the new one, then write the file again in full.\n",
                u.file, c.n
            ));
        }
    }
    if view.output.problems_files == 0 {
        text.push_str(
            "- You wrote no problems file. Write _camerata/out/problems-1.json; if you found no \
             problem, it holds an empty list.\n",
        );
    }
    text
}

fn rules_message(view: &View) -> String {
    let mut text = String::from(
        "Your checklist answers are not complete. Write _camerata/out/checklist.json again with \
         your answers for the rules below (answers you gave before stay).\n",
    );
    if !view.outcome.missing.is_empty() {
        text.push_str(&format!(
            "These rules have no answer: {}.\n",
            view.outcome.missing.join(", ")
        ));
    }
    if !view.outcome.not_completed.is_empty() {
        text.push_str(&format!(
            "These rules have an answer that could not be accepted: {}. A `clean` answer needs up \
             to three places you read, each a file of this part and a line; a `not_a_problem_here` \
             answer needs its one place the same way; a `problems` answer needs the numbers of \
             problems you wrote.\n",
            view.outcome.not_completed.join(", ")
        ));
    }
    text.push_str("Answer each rule `problems`, `clean`, `not_a_problem_here` (with its one place and one sentence of reason) or `not_applicable`, as before.");
    text
}

/// Record the turns of a run with the meter (and the health of the provider), unless the agent
/// already records its own turns and failures there (then each is counted once, by the agent).
pub(super) fn meter_turns(shared: &Shared<'_>, run: &AgentRun) {
    let Some(m) = shared.meter else { return };
    if shared.agent.meters_its_own_turns() {
        return;
    }
    for usage in &run.usage {
        m.record_stage(Stage::Finder, &response_of(shared.options, usage));
    }
    if let AgentStop::Failed(why) = &run.stop {
        m.record_failed(Stage::Finder, &anyhow::anyhow!("{why}"));
    }
}

/// The usage of one turn in the shape the meter counts.
pub(super) fn response_of(
    options: &FinderOptions,
    usage: &super::super::TurnUsage,
) -> crate::llm::LlmResponse {
    let input =
        usage.input_tokens + usage.cache_read_input_tokens + usage.cache_creation_input_tokens;
    let cost = usage.cost_usd.or_else(|| {
        crate::llm::usage_cost_usd(
            &options.model,
            &crate::llm::CallUsage {
                input: Some(input),
                output: Some(usage.output_tokens),
                cache_read: usage.cache_read_input_tokens,
                cache_creation: usage.cache_creation_input_tokens,
                cache_creation_1h: None,
            },
            crate::llm::PriceBasis::realtime(),
        )
    });
    crate::llm::LlmResponse {
        text: String::new(),
        model: options.model.clone(),
        backend: options.backend.clone(),
        cost_usd: cost,
        input_tokens: Some(input),
        output_tokens: Some(usage.output_tokens),
        cache_read_input_tokens: usage.cache_read_input_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
        or_cache_discount: None,
        truncated: false,
        reported_model: None,
        cli_cost_usd: None,
    }
}

fn attempt(shared: &Shared<'_>, kind: &str, message: &str) -> AttemptRecord {
    let mut record = AttemptRecord::new(&shared.options.backend, &shared.options.model);
    record.error_type = Some(kind.to_string());
    record.message = camerata_llm::failure::scrub_message(
        message,
        camerata_llm::failure::MAX_RECORD_MESSAGE_CHARS,
    );
    record
}

fn pass_name(shared: &Shared<'_>, index: usize, what: &str) -> String {
    let base = format!("agent finder, part {} of {}", index + 1, shared.n_pieces);
    if what.is_empty() {
        base
    } else {
        format!("{base} ({what})")
    }
}

/// The rules of the checklist that cover at least one of `files`.
fn rules_covering(shared: &Shared<'_>, checklist: &Checklist, files: &[String]) -> Vec<String> {
    let wanted: HashSet<&str> = files.iter().map(String::as_str).collect();
    let in_files: Vec<(String, String)> = shared
        .files
        .iter()
        .filter(|(p, _)| wanted.contains(p.as_str()))
        .cloned()
        .collect();
    checklist
        .rules
        .iter()
        .filter(|r| {
            shared
                .selected
                .iter()
                .find(|s| s.id.eq_ignore_ascii_case(&r.id))
                .is_some_and(|s| {
                    !crate::scan_routing::files_for_rule(
                        s,
                        shared.options.applies_to_routing,
                        &in_files,
                    )
                    .is_empty()
                })
        })
        .map(|r| r.id.clone())
        .collect()
}

fn stop_class(stop: &AgentStop) -> FailureClass {
    match stop {
        AgentStop::Finished | AgentStop::TurnLimit(_) | AgentStop::Refused(_) => {
            FailureClass::Content
        }
        AgentStop::SpendLimit(_) => FailureClass::SpendLimit,
        AgentStop::Failed(why) => crate::ai_audit::classify_pass_error(&anyhow::anyhow!("{why}")),
    }
}

fn stop_reason(stop: &AgentStop) -> String {
    match stop {
        AgentStop::Finished => String::new(),
        AgentStop::TurnLimit(why) => format!("the review stopped at its turn limit: {why}"),
        AgentStop::SpendLimit(why) => format!("the review stopped at its spend limit: {why}"),
        AgentStop::Refused(why) => format!("the model refused to review this part: {why}"),
        AgentStop::Failed(why) => format!("the review failed: {why}"),
    }
}

/// Read the finished conversation into the piece's report: the findings, the checks and every
/// failed part.
pub(super) fn report_of(
    shared: &Shared<'_>,
    index: usize,
    piece: &Piece,
    room: &Room,
    not_served: &[NotServed],
    checklist: &Checklist,
    runs: Vec<AgentRun>,
) -> PieceReport {
    let view = interpret(room, not_served, checklist, piece, &runs);
    let mut report = PieceReport {
        index,
        checklist_rules: checklist.rules.iter().map(|r| r.id.clone()).collect(),
        usage: runs.iter().flat_map(|r| r.usage.iter().cloned()).collect(),
        ..PieceReport::default()
    };
    for u in &view.stray {
        report
            .notes
            .push(format!("{}: the file {} {}", piece.key, u.file, u.reason));
    }
    for n in &view.output.notes {
        report.notes.push(format!("{}: {n}", piece.key));
    }
    // What the agent judged not a problem here: shown to the reviewer, never a client row.
    for n in &view.outcome.not_a_problem_notes {
        let at = n
            .place
            .line
            .map_or_else(|| n.place.path.clone(), |l| format!("{}:{l}", n.place.path));
        report
            .not_a_problem_lines
            .push(format!("{}: {} at {at}: {}", piece.key, n.rule, n.reason));
    }
    for n in &view.outcome.not_applicable_notes {
        report.not_a_problem_lines.push(format!(
            "{}: {} not applicable: {}",
            piece.key, n.rule, n.reason
        ));
    }
    for file in rewritten_problem_files(&runs) {
        report.notes.push(format!(
            "{}: a later step wrote {file} again; the problems only the earlier version held were kept",
            piece.key
        ));
    }

    // The findings: whatever problems were usable, even when the piece failed.
    let adopted: HashSet<String> = checklist
        .rule_ids()
        .iter()
        .map(|r| r.to_ascii_uppercase())
        .collect();
    let cites = reply::citation_map(checklist, &view.citations);
    let (findings, proposed) = reply::to_findings(
        shared.repo,
        &piece.key,
        &view.output.problems,
        &cites,
        &adopted,
    );
    report.findings = super::rating::apply_agent_rating(findings, &view.output.problems);
    report.proposed = proposed;
    let mut with_problems: Vec<String> = view
        .citations
        .iter()
        .map(|c| c.rule.trim().to_ascii_uppercase())
        .collect();
    with_problems.sort();
    with_problems.dedup();
    report.rules_with_problems = with_problems;
    report.problems = view.output.problems.clone();
    report.leads = view.output.leads.clone();
    report.card = view.output.card.clone();
    report.effective = view.outcome.effective.clone();
    report.not_applicable = view
        .outcome
        .effective
        .iter()
        .filter(|e| matches!(e.state, Effective::NotApplicable))
        .map(|e| e.rule.clone())
        .collect();
    report.not_a_problem_here = view
        .outcome
        .effective
        .iter()
        .filter(|e| matches!(e.state, Effective::NotAProblemHere))
        .map(|e| e.rule.clone())
        .collect();

    // Failed parts.
    let stopped = runs.iter().find(|r| !r.stop.is_finished());
    let unserved_files: Vec<String> = view.coverage.not_served.keys().cloned().collect();
    let unread_files: Vec<String> = view.unread_sendable.clone();
    let mut failures: Vec<FailedPass> = Vec::new();
    let mut partial: Vec<(String, String)> = Vec::new();
    if let Some(run) = stopped {
        report.refused = matches!(run.stop, AgentStop::Refused(_));
        let kind = match run.stop {
            AgentStop::Refused(_) => "refused",
            AgentStop::TurnLimit(_) => "turn_limit",
            AgentStop::SpendLimit(_) => "spend_limit",
            _ => "failed",
        };
        // The main review died: the whole part is the failed part. A later step died (send
        // again, the repair or rules turn): only what that step was to cover is.
        let main_review_failed = runs.first().is_some_and(|r| !r.stop.is_finished());
        let files = if main_review_failed {
            piece.files.clone()
        } else {
            view.coverage.still_unread.clone()
        };
        failures.push(FailedPass {
            repo: shared.repo.to_string(),
            pass: pass_name(shared, index, ""),
            reason: stop_reason(&run.stop),
            files_not_reviewed: files.len(),
            class: stop_class(&run.stop),
            files,
            attempts: vec![attempt(shared, kind, &stop_reason(&run.stop))],
            scope_note: false,
        });
        let covered_by = if main_review_failed {
            piece.files.clone()
        } else {
            view.unread_sendable.clone()
        };
        for rule in rules_covering(shared, checklist, &covered_by) {
            partial.push((
                rule,
                "the review of this part did not finish and files of it were not reviewed"
                    .to_string(),
            ));
        }
        for rule in view
            .outcome
            .missing
            .iter()
            .chain(&view.outcome.not_completed)
        {
            partial.push((
                rule.clone(),
                "the review of this part did not finish before this rule was answered".to_string(),
            ));
        }
    } else {
        if !view.known_unusable.is_empty() {
            let names: Vec<String> = view
                .known_unusable
                .iter()
                .map(|u| format!("{} {}", u.file, u.reason))
                .collect();
            failures.push(content_failure(
                shared,
                index,
                "output not usable",
                format!(
                    "the review wrote output that could not be used, even after one request to \
                     write it again: {}",
                    names.join("; ")
                ),
                Vec::new(),
            ));
        } else if view.output.problems_files == 0 {
            failures.push(content_failure(
                shared,
                index,
                "output not usable",
                "the review wrote no problems file, even after one request to write one"
                    .to_string(),
                Vec::new(),
            ));
        }
        if !unread_files.is_empty() {
            let mut failure = content_failure(
                shared,
                index,
                "files not reviewed",
                format!(
                    "{} file(s) of this part were not read in full and were not shown to the \
                     model in full either, so they were not reviewed: {}",
                    unread_files.len(),
                    unread_files.join(", ")
                ),
                unread_files.clone(),
            );
            failure.files_not_reviewed = unread_files.len();
            failures.push(failure);
            for rule in rules_covering(shared, checklist, &unread_files) {
                partial.push((
                    rule,
                    format!(
                        "{} file(s) of this part were not reviewed",
                        unread_files.len()
                    ),
                ));
            }
        }
        if !view.outcome.missing.is_empty() {
            failures.push(content_failure(
                shared,
                index,
                "rules not answered",
                format!(
                    "no answer was given for {} even after one request for it",
                    view.outcome.missing.join(", ")
                ),
                rules_files(shared, checklist, &view.outcome.missing, &piece.files),
            ));
            for rule in &view.outcome.missing {
                partial.push((rule.clone(), "no answer was given".to_string()));
            }
        }
        if !view.outcome.not_completed.is_empty() {
            failures.push(content_failure(
                shared,
                index,
                "rules not completed",
                format!(
                    "the answer for {} could not be accepted (a `clean` answer named no place \
                     that was read; a `problems` answer named no problem that exists)",
                    view.outcome.not_completed.join(", ")
                ),
                rules_files(shared, checklist, &view.outcome.not_completed, &piece.files),
            ));
            for rule in &view.outcome.not_completed {
                partial.push((rule.clone(), "the answer could not be accepted".to_string()));
            }
        }
    }
    if !unserved_files.is_empty() {
        let reasons: Vec<String> = view
            .coverage
            .not_served
            .iter()
            .map(|(p, why)| format!("{p} ({why})"))
            .collect();
        let mut failure = content_failure(
            shared,
            index,
            "files not served",
            format!(
                "the review room could not serve {} file(s) of this part: {}",
                unserved_files.len(),
                reasons.join("; ")
            ),
            unserved_files.clone(),
        );
        failure.files_not_reviewed = unserved_files.len();
        failures.push(failure);
        for rule in rules_covering(shared, checklist, &unserved_files) {
            partial.push((
                rule,
                "a file of this part could not be put in front of the review".to_string(),
            ));
        }
    }
    partial.sort();
    partial.dedup();
    report.partial_rules = partial
        .into_iter()
        .map(|(rule_id, reason)| PartialRule {
            unit: piece.unit.clone(),
            rule_id,
            reason,
        })
        .collect();
    report.failures = failures;
    report.coverage = Some(view.coverage);
    report.runs = runs;
    report
}

fn rules_files(
    shared: &Shared<'_>,
    checklist: &Checklist,
    rules: &[String],
    piece_files: &[String],
) -> Vec<String> {
    let wanted: HashSet<String> = rules.iter().map(|r| r.to_ascii_uppercase()).collect();
    let mut out: Vec<String> = Vec::new();
    for rule in checklist
        .rules
        .iter()
        .filter(|r| wanted.contains(&r.id.to_ascii_uppercase()))
    {
        let scope: Vec<(String, String)> = shared
            .files
            .iter()
            .filter(|(p, _)| piece_files.contains(p))
            .cloned()
            .collect();
        if let Some(s) = shared
            .selected
            .iter()
            .find(|s| s.id.eq_ignore_ascii_case(&rule.id))
        {
            for (p, _) in
                crate::scan_routing::files_for_rule(s, shared.options.applies_to_routing, &scope)
            {
                if !out.contains(p) {
                    out.push(p.clone());
                }
            }
        }
    }
    out.sort();
    out
}

fn content_failure(
    shared: &Shared<'_>,
    index: usize,
    what: &str,
    reason: String,
    files: Vec<String>,
) -> FailedPass {
    FailedPass {
        repo: shared.repo.to_string(),
        pass: pass_name(shared, index, what),
        attempts: vec![attempt(shared, "content", &reason)],
        reason,
        files_not_reviewed: 0,
        class: FailureClass::Content,
        files,
        scope_note: false,
    }
}

/// A piece that could not even start (nothing was sent, nothing was paid).
pub(super) fn unstarted_report(
    shared: &Shared<'_>,
    index: usize,
    piece: &Piece,
    checklist: &Checklist,
    class: FailureClass,
    reason: String,
    attempts: Vec<AttemptRecord>,
) -> PieceReport {
    let mut report = PieceReport {
        index,
        checklist_rules: checklist.rules.iter().map(|r| r.id.clone()).collect(),
        ..PieceReport::default()
    };
    report.failures.push(FailedPass {
        repo: shared.repo.to_string(),
        pass: pass_name(shared, index, ""),
        reason,
        files_not_reviewed: piece.files.len(),
        class,
        files: piece.files.clone(),
        attempts,
        scope_note: false,
    });
    for rule in rules_covering(shared, checklist, &piece.files) {
        report.partial_rules.push(PartialRule {
            unit: piece.unit.clone(),
            rule_id: rule,
            reason: "this part was not reviewed".to_string(),
        });
    }
    // Nothing was reviewed: every file of the piece is on the record as not reviewed.
    let mut sorted = piece.files.clone();
    sorted.sort();
    report.coverage = Some(PieceCoverage {
        piece_key: piece.key.clone(),
        files: sorted
            .iter()
            .map(|p| (p.clone(), super::super::coverage::FileState::NotReviewed))
            .collect(),
        still_unread: sorted,
        ..PieceCoverage::default()
    });
    report
}

/// The failed part for a piece the spend limit (or an outage stop) did not admit.
pub(super) fn not_run_report(
    shared: &Shared<'_>,
    index: usize,
    piece: &Piece,
    checklist: &Checklist,
    outage: bool,
) -> PieceReport {
    let cause = shared
        .meter
        .map_or(crate::cost_cap::CAUSE_LIMIT_REACHED, |m| m.stop_cause());
    let reason = if outage {
        "NOT RUN: the run stopped because the model provider could not be reached before this part was reviewed. This inspection is incomplete.".to_string()
    } else {
        format!("NOT RUN: {cause} before this part was reviewed. This inspection is incomplete.")
    };
    let class = if outage {
        FailureClass::OutageStop
    } else {
        FailureClass::SpendLimit
    };
    unstarted_report(shared, index, piece, checklist, class, reason, Vec::new())
}

/// Run the whole conversation of one piece with the agent: the first run, then the repair turn,
/// the send again turn and the missing rules turn, each at most once.
pub(super) async fn converse(
    shared: &Shared<'_>,
    index: usize,
    piece: &Piece,
    room: &Room,
    not_served: &[NotServed],
    checklist: &Checklist,
    limits: &PieceLimits,
) -> Result<Vec<AgentRun>, anyhow::Error> {
    let mut runs: Vec<AgentRun> = Vec::new();
    let first = shared
        .agent
        .run(room, super::super::brief_text::PIECE_BRIEF, limits)
        .await?;
    meter_turns(shared, &first);
    runs.push(first);
    let (mut repaired, mut sent, mut asked) = (false, false, false);
    // The newest conversation the agent holds: a successful follow-up releases the one it
    // continued from, so only this one is let go of when the piece ends.
    let mut live = runs[0].conversation.clone();
    loop {
        let last = runs.last().expect("one run");
        if !last.stop.is_finished() {
            break;
        }
        let view = interpret(room, not_served, checklist, piece, &runs);
        let turn = if !repaired && view.needs_repair() {
            repaired = true;
            FollowUp::text(repair_message(&view))
        } else if !sent && !view.unread_sendable.is_empty() {
            sent = true;
            FollowUp::with_files(SEND_AGAIN_MESSAGE, &view.unread_sendable)
        } else if !asked && view.needs_rules_follow_up() {
            asked = true;
            FollowUp::text(rules_message(&view))
        } else {
            break;
        };
        let prior = runs.last().expect("one run").clone();
        match shared.agent.follow_up(room, &prior, &turn, limits).await {
            Ok(run) => {
                meter_turns(shared, &run);
                live = run.conversation.clone();
                runs.push(run);
            }
            // Nothing was sent: the step simply did not happen; its gaps stay disclosed.
            Err(e) => {
                if let Some(m) = shared.meter {
                    m.record_failed(Stage::Finder, &e);
                }
                runs.push(AgentRun::empty(AgentStop::Failed(e.to_string())));
                break;
            }
        }
    }
    shared.agent.release(&live);
    let _ = index;
    Ok(runs)
}
