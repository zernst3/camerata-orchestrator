//! The rules as the agent's checklist (design section 3): which rules a piece owes an answer to,
//! the text of `checklist.md`, the checks that run in code on the answers, and the way the
//! answers of a unit's pieces add up to the unit's conventions.

use std::collections::BTreeMap;

use super::reply::Problem;
use super::ToolEvent;
use crate::scan_routing::AuditRule;

/// One option of a rule that offers several.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChecklistOption {
    pub id: String,
    pub label: String,
}

/// One checklist line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChecklistRule {
    pub id: String,
    pub title: String,
    /// The project's chosen option's directive, else the rule's default.
    pub directive: String,
    pub min_severity: Option<String>,
    pub max_severity: Option<String>,
    /// Empty for a rule with no options.
    pub options: Vec<ChecklistOption>,
}

/// The rules one piece owes an answer to, sorted by id.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Checklist {
    pub rules: Vec<ChecklistRule>,
}

/// A place the agent says it checked: a file and, when given, a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub path: String,
    pub line: Option<usize>,
}

/// The three answers a rule can get.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerKind {
    Problems,
    Clean,
    NotApplicable,
}

/// How many places follow one option of a rule, and up to ten of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionCount {
    pub id: String,
    pub count: usize,
    pub places: Vec<Place>,
}

/// The agent's answer for one rule, as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleAnswer {
    pub rule: String,
    pub kind: AnswerKind,
    /// For `Problems`: the numbers of the problems that break the rule.
    pub problems: Vec<u32>,
    /// For `Clean`: up to three places checked.
    pub places: Vec<Place>,
    pub options: Vec<OptionCount>,
}

/// A problem the agent says breaks a rule on the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citation {
    pub problem: u32,
    pub rule: String,
}

/// `checklist.json` as parsed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChecklistFile {
    pub citations: Vec<Citation>,
    pub answers: Vec<RuleAnswer>,
}

/// An answer after the checks: what the record may say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effective {
    Problems(Vec<u32>),
    Clean(Vec<Place>),
    /// Not applicable; the reason is set when the check changed the answer.
    NotApplicable(Option<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveAnswer {
    pub rule: String,
    pub state: Effective,
    pub options: Vec<OptionCount>,
}

/// A `problems` answer that names a problem that does not exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadRef {
    pub rule: String,
    pub problem: u32,
}

/// The result of the checks on one piece's answers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CheckOutcome {
    /// Rules on the checklist with no answer.
    pub missing: Vec<String>,
    pub bad_problem_refs: Vec<BadRef>,
    /// Rules answered `clean` with no valid checked place; recorded as not applicable.
    pub downgraded_clean: Vec<String>,
    /// Citations to a rule that is not on the checklist (the problem stays uncited).
    pub foreign_citations: Vec<Citation>,
    /// Citations to a rule on the checklist about a problem that exists.
    pub valid_citations: Vec<Citation>,
    /// One answer for each checklist rule that has one, after the checks.
    pub effective: Vec<EffectiveAnswer>,
}

/// A unit's picture of one rule that offers options, from adding its pieces' option counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitConvention {
    pub rule: String,
    pub counts: BTreeMap<String, usize>,
    pub total: usize,
    /// The option with at least two thirds of the counted places, if any.
    pub convention: Option<String>,
}

/// Build the checklist: the rules with at least one of `piece_files` in scope, sorted by id.
/// `rules` are the rules the AI tier checks for the repository (the directive is the project's
/// chosen option, else the rule's default). Title, severity range and options come from `corpus`.
pub fn build(
    rules: &[AuditRule],
    corpus: &camerata_rules::RuleSet,
    piece_files: &[(String, String)],
    applies_to_routing: bool,
) -> Checklist {
    let mut sorted: Vec<&AuditRule> = rules.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    sorted.dedup_by(|a, b| a.id == b.id);
    let mut out = Vec::new();
    for rule in sorted {
        if crate::scan_routing::files_for_rule(rule, applies_to_routing, piece_files).is_empty() {
            continue;
        }
        let corpus_rule = corpus
            .get_by_id(&rule.id)
            .or_else(|| corpus.get_by_id(&rule.id.to_ascii_uppercase()));
        let options = corpus_rule
            .filter(|r| r.options.len() >= 2)
            .map(|r| {
                r.options
                    .iter()
                    .map(|o| ChecklistOption {
                        id: o.id.clone(),
                        label: o.label.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.push(ChecklistRule {
            id: rule.id.clone(),
            title: corpus_rule.map_or_else(|| rule.id.clone(), |r| r.title.clone()),
            directive: rule.directive.clone(),
            min_severity: corpus_rule.and_then(|r| r.min_severity.clone()),
            max_severity: corpus_rule.and_then(|r| r.max_severity.clone()),
            options,
        });
    }
    Checklist { rules: out }
}

/// A path the way the room names it.
fn plain_path(path: &str) -> String {
    path.trim_start_matches("./").to_string()
}

impl Checklist {
    /// The ids on the list, in order.
    pub fn rule_ids(&self) -> Vec<&str> {
        self.rules.iter().map(|r| r.id.as_str()).collect()
    }

    /// The text of `_camerata/checklist.md`: the three instructions, then the rules.
    pub fn render(&self) -> String {
        let mut out = String::from(
            "# Checklist\n\n\
             You have written down everything you found. Now go through the rules below. Do these \
             three things, then write `_camerata/out/checklist.json`.\n\n\
             1. Cite. For each problem you already reported that breaks a rule on this list, \
             record the problem's number and the rule id under `citations`. A problem that breaks \
             no rule stays as it is.\n\
             2. Answer every rule. Give one answer for each rule, no exceptions, under `answers`: \
             `problems` (with the numbers of your problems), `clean` (with up to three places you \
             checked, each a file and line you read), or `not_applicable` when nothing in this part \
             is what the rule is about. For a rule with options, also say how many places follow \
             each option, and name up to ten places for each.\n\
             3. Add. A problem that a rule turns up and that you did not report goes into a new \
             problems file with that rule's id as its `label`.\n\n\
             `checklist.json` looks like this:\n\n\
             {\"citations\": [{\"n\": 1, \"rule\": \"RULE-ID\"}], \"answers\": [{\"rule\": \"RULE-ID\", \
             \"answer\": \"clean\", \"places\": [{\"path\": \"src/a.rs\", \"line\": 10}], \"options\": \
             [{\"id\": \"option-id\", \"count\": 3, \"places\": [\"src/a.rs:10\"]}]}]}\n\n\
             # Rules\n",
        );
        for rule in &self.rules {
            out.push_str(&format!("\n## {}: {}\n", rule.id, rule.title));
            out.push_str(&format!("Directive: {}\n", rule.directive));
            match (&rule.min_severity, &rule.max_severity) {
                (Some(lo), Some(hi)) => {
                    out.push_str(&format!("Severity: at least {lo}, at most {hi}\n"));
                }
                (Some(lo), None) => out.push_str(&format!("Severity: at least {lo}\n")),
                (None, Some(hi)) => out.push_str(&format!("Severity: at most {hi}\n")),
                (None, None) => {}
            }
            if !rule.options.is_empty() {
                out.push_str("Options:\n");
                for o in &rule.options {
                    out.push_str(&format!("- {}: {}\n", o.id, o.label));
                }
            }
        }
        out
    }

    /// Check the agent's answers against the problems it wrote and the trace. Nothing the model
    /// says about its own work is believed: a `clean` answer needs a place the trace shows was
    /// read, a `problems` answer needs problems that exist, and a citation needs a listed rule.
    pub fn check(
        &self,
        answers: &ChecklistFile,
        problems: &[Problem],
        trace: &[ToolEvent],
    ) -> CheckOutcome {
        let numbers: Vec<u32> = problems.iter().map(|p| p.n).collect();
        let read_paths: std::collections::BTreeSet<String> = trace
            .iter()
            .filter_map(|e| match e {
                ToolEvent::Read {
                    path,
                    lines_returned,
                    ..
                } if *lines_returned > 0 => Some(plain_path(path)),
                _ => None,
            })
            .collect();
        let listed: std::collections::BTreeSet<String> = self
            .rules
            .iter()
            .map(|r| r.id.to_ascii_uppercase())
            .collect();
        let mut out = CheckOutcome::default();
        for rule in &self.rules {
            let Some(answer) = answers
                .answers
                .iter()
                .find(|a| a.rule.eq_ignore_ascii_case(&rule.id))
            else {
                out.missing.push(rule.id.clone());
                continue;
            };
            let state = match answer.kind {
                AnswerKind::NotApplicable => Effective::NotApplicable(None),
                AnswerKind::Problems => {
                    let mut real = Vec::new();
                    for &n in &answer.problems {
                        if numbers.contains(&n) {
                            real.push(n);
                        } else {
                            out.bad_problem_refs.push(BadRef {
                                rule: rule.id.clone(),
                                problem: n,
                            });
                        }
                    }
                    if real.is_empty() {
                        Effective::NotApplicable(Some(
                            "the answer named no problem that exists".to_string(),
                        ))
                    } else {
                        Effective::Problems(real)
                    }
                }
                AnswerKind::Clean => {
                    let valid: Vec<Place> = answer
                        .places
                        .iter()
                        .filter(|p| read_paths.contains(&plain_path(&p.path)))
                        .cloned()
                        .collect();
                    if valid.is_empty() {
                        out.downgraded_clean.push(rule.id.clone());
                        Effective::NotApplicable(Some("no place to check was named".to_string()))
                    } else {
                        Effective::Clean(valid)
                    }
                }
            };
            out.effective.push(EffectiveAnswer {
                rule: rule.id.clone(),
                state,
                options: answer.options.clone(),
            });
        }
        for c in &answers.citations {
            if !listed.contains(&c.rule.to_ascii_uppercase()) {
                out.foreign_citations.push(c.clone());
            } else if numbers.contains(&c.problem) {
                out.valid_citations.push(c.clone());
            }
        }
        out
    }
}

/// Add the option counts of a unit's pieces and apply the two thirds rule: an option with at
/// least two thirds of the counted places is the unit's convention. Rules with no option rows
/// have no entry. Sorted by rule id.
pub fn combine_for_unit(piece_answers: &[Vec<EffectiveAnswer>]) -> Vec<UnitConvention> {
    let mut by_rule: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for answers in piece_answers {
        for a in answers {
            if a.options.is_empty() {
                continue;
            }
            let counts = by_rule.entry(a.rule.to_ascii_uppercase()).or_default();
            for o in &a.options {
                *counts.entry(o.id.clone()).or_insert(0) += o.count;
            }
        }
    }
    by_rule
        .into_iter()
        .map(|(rule, counts)| {
            let total: usize = counts.values().sum();
            let convention = counts
                .iter()
                .find(|(_, &n)| total > 0 && n * 3 >= total * 2)
                .map(|(id, _)| id.clone());
            UnitConvention {
                rule,
                counts,
                total,
                convention,
            }
        })
        .collect()
}
