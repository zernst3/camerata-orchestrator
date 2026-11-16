//! The joints call (design section 1): one more agent call for a repository that was split into
//! two or more pieces. It gets every piece's card, every lead and the list of problems already
//! reported, and reviews only what shows across the parts. Code checks that every lead is answered
//! in one of two ways; a lead with no answer becomes a problem needing a decision, so a lead is
//! never dropped in silence.

use std::collections::{BTreeMap, HashSet};

use sha2::{Digest, Sha256};

use super::super::brief_text::JOINTS_BRIEF;
use super::super::reply::{self, Lead, Problem};
use super::super::room::Room;
use super::super::{AgentRun, AgentStop, Piece, PieceLimits};
use super::piece::{merged_outputs, meter_turns, PieceReport, Shared};
use crate::ai_audit::{AttemptRecord, FailedPass, FailureClass};
use crate::onboard::{Finding, ProposedRule};

const CARDS_PATH: &str = "_camerata/joints/cards.md";
const LEADS_PATH: &str = "_camerata/joints/leads.md";
const PROBLEMS_PATH: &str = "_camerata/joints/problems.md";

/// What the joints agent is given, numbered across the pieces.
#[derive(Debug, Clone)]
pub(super) struct JointsInput {
    pub cards_md: String,
    pub leads_md: String,
    pub problems_md: String,
    /// Every lead with its number across the pieces.
    pub leads: Vec<Lead>,
    /// Every reported problem with its number across the pieces, and its file.
    pub problems: Vec<(u32, String)>,
    /// The highest problem number already used.
    pub max_problem: u32,
}

impl JointsInput {
    /// A digest of everything the agent is given, so a stored joints conversation is replayed
    /// only for the same input.
    pub fn digest(&self) -> String {
        let mut h = Sha256::new();
        for text in [&self.cards_md, &self.leads_md, &self.problems_md] {
            h.update((text.len() as u64).to_le_bytes());
            h.update(text.as_bytes());
        }
        h.finalize()
            .iter()
            .take(6)
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

/// Number the problems and the leads of all pieces from 1, in piece order, and write the three
/// files the joints agent reads.
pub(super) fn gather(reports: &[PieceReport]) -> JointsInput {
    let mut problems: Vec<(u32, String)> = Vec::new();
    let mut problems_md = String::from("# Problems already reported\n\n");
    let mut leads: Vec<Lead> = Vec::new();
    let mut leads_md = String::from("# Leads\n\n");
    let mut cards_md = String::from("# Cards\n");
    let mut next_problem = 1u32;
    let mut next_lead = 1u32;
    for report in reports {
        for (i, p) in report.problems.iter().enumerate() {
            let rule = report
                .findings
                .get(i)
                .map_or_else(|| p.label.clone(), |f| f.rule_id.clone());
            problems_md.push_str(&format!(
                "{next_problem}. {rule} at {}:{} - {}\n",
                p.path, p.line, p.title
            ));
            problems.push((next_problem, p.path.clone()));
            next_problem += 1;
        }
        for lead in &report.leads {
            leads_md.push_str(&format!(
                "{next_lead}. {}:{} - {} (from part {})\n",
                lead.path,
                lead.line,
                lead.what,
                report.index + 1
            ));
            leads.push(Lead {
                n: next_lead,
                ..lead.clone()
            });
            next_lead += 1;
        }
        cards_md.push_str(&format!("\n## Part {}\n", report.index + 1));
        cards_md.push_str(report.card.as_deref().unwrap_or("This part wrote no card."));
        cards_md.push('\n');
    }
    JointsInput {
        cards_md,
        leads_md,
        problems_md,
        leads,
        problems,
        max_problem: next_problem - 1,
    }
}

/// What the joints call produced.
#[derive(Debug, Default)]
pub(super) struct JointsReport {
    pub findings: Vec<Finding>,
    pub proposed: Vec<ProposedRule>,
    /// The problems needing a decision made from leads nothing settled.
    pub unresolved: Vec<Finding>,
    pub failures: Vec<FailedPass>,
    pub notes: Vec<String>,
    pub runs: Vec<AgentRun>,
    pub replayed: bool,
    pub usage: Vec<super::super::TurnUsage>,
}

/// The problem a lead nothing settled becomes: a problem needing a decision, at the lead's place.
fn lead_finding(repo: &str, lead: &Lead, files: &BTreeMap<&str, Vec<&str>>) -> Finding {
    let text = files
        .get(lead.path.as_str())
        .and_then(|lines| lead.line.checked_sub(1).and_then(|i| lines.get(i)))
        .map(|l| l.trim().to_string())
        .unwrap_or_default();
    let title = "A problem seen outside its part was not settled";
    let what = if lead.what.trim().is_empty() {
        "A reviewer noted this place while reading another part.".to_string()
    } else {
        lead.what.trim().to_string()
    };
    let mut finding = Finding {
        repo: repo.to_string(),
        path: lead.path.clone(),
        line: lead.line,
        rule_id: "AI-OPEN-QUESTION".to_string(),
        severity: "medium".to_string(),
        snippet: if text.is_empty() {
            title.to_string()
        } else {
            text
        },
        detail: format!(
            "{title}{}{what} No later step settled it, so it needs a decision.",
            crate::ai_audit::digest_parse::HEADLINE_SEP
        ),
        needs_review: true,
        located: !text_is_empty(files, lead),
        ..Finding::default()
    };
    crate::onboard::redact::redact_finding(&mut finding);
    finding
}

fn text_is_empty(files: &BTreeMap<&str, Vec<&str>>, lead: &Lead) -> bool {
    files
        .get(lead.path.as_str())
        .and_then(|lines| lead.line.checked_sub(1).and_then(|i| lines.get(i)))
        .is_none()
}

/// Turn the leads into problems needing a decision (`numbers`) and return them.
pub(super) fn unresolved_findings(
    repo: &str,
    leads: &[Lead],
    numbers: &[u32],
    files: &[(String, String)],
) -> Vec<Finding> {
    let by_path: BTreeMap<&str, Vec<&str>> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.lines().collect()))
        .collect();
    leads
        .iter()
        .filter(|l| numbers.contains(&l.n))
        .map(|l| lead_finding(repo, l, &by_path))
        .collect()
}

/// Read the joints agent's output into problems, lead outcomes and failures.
#[allow(clippy::too_many_lines)]
pub(super) fn report_of(
    shared: &Shared<'_>,
    input: &JointsInput,
    runs: Vec<AgentRun>,
    adopted: &HashSet<String>,
) -> JointsReport {
    let repo = shared.repo;
    let all_files: Vec<String> = shared.files.iter().map(|(p, _)| p.clone()).collect();
    let outputs = merged_outputs(&runs);
    let mut out = reply::parse_outputs(&outputs, &all_files);
    let mut report = JointsReport {
        usage: runs.iter().flat_map(|r| r.usage.iter().cloned()).collect(),
        ..JointsReport::default()
    };
    let mut failures: Vec<FailedPass> = Vec::new();
    let mut fail = |what: String, class: FailureClass| {
        let mut record = AttemptRecord::new(&shared.options.backend, &shared.options.model);
        record.error_type = Some("joints".to_string());
        record.message = camerata_llm::failure::scrub_message(
            &what,
            camerata_llm::failure::MAX_RECORD_MESSAGE_CHARS,
        );
        failures.push(FailedPass {
            repo: repo.to_string(),
            pass: "agent finder, joints between parts".to_string(),
            reason: what,
            files_not_reviewed: 0,
            class,
            files: Vec::new(),
            attempts: vec![record],
            scope_note: false,
        });
    };
    if let Some(run) = runs.iter().find(|r| !r.stop.is_finished()) {
        let why = match &run.stop {
            AgentStop::TurnLimit(t) => format!("the joints review stopped at its turn limit: {t}"),
            AgentStop::SpendLimit(t) => {
                format!("the joints review stopped at its spend limit: {t}")
            }
            AgentStop::Refused(t) => format!("the model refused the joints review: {t}"),
            AgentStop::Failed(t) => format!("the joints review failed: {t}"),
            AgentStop::Finished => String::new(),
        };
        let class = match &run.stop {
            AgentStop::SpendLimit(_) => FailureClass::SpendLimit,
            AgentStop::Failed(t) => crate::ai_audit::classify_pass_error(&anyhow::anyhow!("{t}")),
            _ => FailureClass::Content,
        };
        fail(why, class);
    }
    let (known, stray): (Vec<_>, Vec<_>) =
        std::mem::take(&mut out.unusable)
            .into_iter()
            .partition(|u| {
                let name = u
                    .file
                    .strip_prefix(super::super::room::OUT_DIR)
                    .unwrap_or(&u.file);
                name.starts_with("problems-") || name == "leads-answers.json"
            });
    for u in &stray {
        report
            .notes
            .push(format!("joints: the file {} {}", u.file, u.reason));
    }
    if !known.is_empty() {
        let names: Vec<String> = known
            .iter()
            .map(|u| format!("{} {}", u.file, u.reason))
            .collect();
        fail(
            format!(
                "the joints review wrote output that could not be used: {}",
                names.join("; ")
            ),
            FailureClass::Content,
        );
    }

    // The joints agent's own problems. Numbers it gave that collide with a reported problem are
    // moved above the highest number in use.
    let mut taken: HashSet<u32> = input.problems.iter().map(|(n, _)| *n).collect();
    let mut highest = input.max_problem;
    let mut renumbered: BTreeMap<u32, u32> = BTreeMap::new();
    let mut problems: Vec<Problem> = Vec::new();
    for mut p in std::mem::take(&mut out.problems) {
        let original = p.n;
        if taken.contains(&p.n) {
            loop {
                highest += 1;
                if !taken.contains(&highest) {
                    break;
                }
            }
            p.n = highest;
        }
        taken.insert(p.n);
        renumbered.insert(original, p.n);
        problems.push(p);
    }
    let mut numbers: Vec<(u32, String)> = input.problems.clone();
    numbers.extend(problems.iter().map(|p| (p.n, p.path.clone())));

    // Problems the joints agent put in files outside the repository are leads again.
    let mut leads = input.leads.clone();
    let mut next_lead = leads.iter().map(|l| l.n).max().unwrap_or(0);
    let mut extra_leads: Vec<Lead> = Vec::new();
    for l in std::mem::take(&mut out.leads) {
        next_lead += 1;
        extra_leads.push(Lead { n: next_lead, ..l });
    }
    leads.extend(extra_leads.iter().cloned());

    let answers_text = out.leads_answers.take().map(|t| {
        // An answer that names a problem by the joints agent's own number is read by the number
        // the problem now has.
        match serde_json::from_str::<serde_json::Value>(&t) {
            Ok(mut v) => {
                let items = if v.is_array() {
                    v.as_array_mut()
                } else {
                    v.get_mut("answers")
                        .and_then(serde_json::Value::as_array_mut)
                };
                if let Some(items) = items {
                    for item in items {
                        if let Some(n) = item
                            .get("new_problem")
                            .and_then(serde_json::Value::as_u64)
                            .and_then(|n| u32::try_from(n).ok())
                        {
                            if let Some(now) = renumbered.get(&n) {
                                item["new_problem"] = serde_json::json!(now);
                            }
                        }
                    }
                }
                v.to_string()
            }
            Err(_) => t,
        }
    });
    let answers = reply::parse_leads_answers(answers_text.as_deref(), &leads, &numbers);
    let mut unresolved: Vec<u32> = answers.unresolved();
    unresolved.retain(|n| *n != 0);
    if let Some(why) = &answers.unusable {
        fail(
            format!("the joints review's answers to the leads could not be used: leads-answers.json {why}"),
            FailureClass::Content,
        );
    }
    // A call that failed or wrote nothing answers no lead, and no lead is dropped.
    report.unresolved = unresolved_findings(repo, &leads, &unresolved, shared.files);

    let cites = BTreeMap::new();
    let (findings, proposed) = reply::to_findings(repo, "joints", &problems, &cites, adopted);
    report.findings = super::rating::apply_agent_rating(findings, &problems);
    report.proposed = proposed;
    report.failures = failures;
    report.runs = runs;
    report
}

/// Run the joints call for a repository with two or more pieces.
pub(super) async fn run_joints(
    shared: &Shared<'_>,
    input: &JointsInput,
    adopted: &HashSet<String>,
) -> Result<Vec<AgentRun>, anyhow::Error> {
    let all_files: Vec<String> = shared.files.iter().map(|(p, _)| p.clone()).collect();
    let piece = Piece {
        key: "joints".to_string(),
        unit: shared.repo.to_string(),
        files: all_files.clone(),
    };
    let built = Room::build(&piece, shared.files, "No checklist applies to this call.\n");
    let room = built
        .room
        .with_extra_files(&[
            (CARDS_PATH.to_string(), input.cards_md.clone()),
            (LEADS_PATH.to_string(), input.leads_md.clone()),
            (PROBLEMS_PATH.to_string(), input.problems_md.clone()),
        ])
        .map_err(|e| anyhow::anyhow!("the joints room could not be built: {e}"))?;
    let limits = PieceLimits::for_piece(all_files.len(), 1.5);
    let numbering = if input.max_problem == 0 {
        "No problem was reported by the parts. Number your own problems from 1.".to_string()
    } else {
        format!(
            "The problems already reported are numbered 1 to {}. Number your own problems from {} upward.",
            input.max_problem,
            input.max_problem + 1
        )
    };
    let brief = format!(
        "{JOINTS_BRIEF}\n\nThe cards are in {CARDS_PATH}, the leads in {LEADS_PATH} and the problems already reported in {PROBLEMS_PATH}. {numbering}"
    );
    let _ = adopted;
    let run = shared.agent.run(&room, &brief, &limits).await?;
    meter_turns(shared, &run);
    Ok(vec![run])
}
