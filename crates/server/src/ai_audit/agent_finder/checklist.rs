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

/// The four answers a rule can get.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerKind {
    Problems,
    Clean,
    NotApplicable,
    /// The letter of the rule is not met at one place, and the agent judges it is not a problem
    /// here. One place and one sentence of reason; it makes no row.
    NotAProblemHere,
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
    /// For `Clean`: up to three places checked. For `NotAProblemHere`: the one place.
    pub places: Vec<Place>,
    /// For `NotAProblemHere`: the one sentence of reason.
    pub reason: Option<String>,
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
    /// Not applicable. Only the agent's own `not_applicable` answer gives this state; a check
    /// never changes an answer into it.
    NotApplicable,
    /// Every answer the agent gave for the rule was `not_a_problem_here`: it looked, found the
    /// letter of the rule unmet at a place it judged harmless, and reported no problem. Not clean
    /// (nothing was shown to comply), not a problem, and not "not applicable" (the rule's concern
    /// is present). Rules in this state make no row and count as assessed; the place and the
    /// reason are in [`CheckOutcome::not_a_problem_notes`].
    NotAProblemHere,
    /// The rule was not completed: the agent's answer did not hold up (a `problems` answer whose
    /// numbers resolve to nothing, a `clean` answer with no valid place). The text says why. A
    /// failed part a resume retries, never a "not applicable" and never a clean.
    NotCompleted(String),
}

impl Effective {
    /// How bad the state is when one rule has two answers: problems over not completed over clean
    /// over not a problem here over not applicable.
    fn rank(&self) -> u8 {
        match self {
            Self::Problems(_) => 4,
            Self::NotCompleted(_) => 3,
            Self::Clean(_) => 2,
            Self::NotAProblemHere => 1,
            Self::NotApplicable => 0,
        }
    }
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

/// One `not_a_problem_here` answer, kept for the reviewer. Never a client row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotAProblemNote {
    pub rule: String,
    pub place: Place,
    pub reason: String,
}

/// The result of the checks on one piece's answers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CheckOutcome {
    /// Rules on the checklist with no answer.
    pub missing: Vec<String>,
    pub bad_problem_refs: Vec<BadRef>,
    /// Rules answered `clean` with no valid checked place; recorded as not completed.
    pub downgraded_clean: Vec<String>,
    /// Rules whose state after the checks is not completed (a downgraded `clean` answer or a
    /// `problems` answer that resolves to no problem), in checklist order.
    pub not_completed: Vec<String>,
    /// Citations to a rule that is not on the checklist (the problem stays uncited).
    pub foreign_citations: Vec<Citation>,
    /// Citations to a rule on the checklist about a problem that exists.
    pub valid_citations: Vec<Citation>,
    /// One answer for each checklist rule that has one, after the checks.
    pub effective: Vec<EffectiveAnswer>,
    /// Every `not_a_problem_here` answer given, whichever answer stood for its rule.
    pub not_a_problem_notes: Vec<NotAProblemNote>,
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

/// A rule's directive with its severity orders turned into guidance. The engine no longer holds
/// an agent's row to a rule's minimum, so the text must not say it does: "Report at high
/// severity when" reads "Usually high when", "Flag as high severity" reads "Usually high",
/// "emit a high-severity finding" reads "emit a finding (usually high)", "Severity is High."
/// reads "Usually high.".
fn as_severity_guidance(directive: &str) -> String {
    static PATTERNS: std::sync::OnceLock<Vec<(regex::Regex, &'static str)>> =
        std::sync::OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            (r"(?i)\breport at (\w+) severity when\b", "Usually $1 when"),
            (r"(?i)\bflag (?:it )?as (\w+) severity\b", "Usually $1"),
            (
                r"(?i)\b(emit|report) an? (\w+)[- ]severity finding\b",
                "$1 a finding (usually $2)",
            ),
            (r"\bSeverity is (\w+)\.", "Usually $1."),
        ]
        .into_iter()
        .filter_map(|(p, r)| regex::Regex::new(p).ok().map(|re| (re, r)))
        .collect()
    });
    let mut text = directive.to_string();
    for (re, with) in patterns {
        text = re.replace_all(&text, *with).into_owned();
    }
    text
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
             checked, each a file and line you read), `not_a_problem_here` (the letter of the rule \
             is not met at one place, and you judge it is not a problem here: give that one place \
             as `place`, a file and line, and one sentence as `reason`; this reports nothing to the \
             client, it tells the reviewer what you saw), or `not_applicable` when nothing in this \
             part is what the rule is about. Give one place and one sentence of reason, no more. \
             For a rule with options, also say how many places follow each option, and name up to \
             ten places for each.\n\
             3. Add. Add a problem only if you would report it to this client on your own \
             judgment. Rate it yourself: the severity a rule shows is a guide, not a rule. If a \
             rule asks for something this code needs and does not have, that is a problem: write \
             it. A problem you add goes into a new problems file with that rule's id as its \
             `label`.\n\n\
             `checklist.json` looks like this:\n\n\
             {\"citations\": [{\"n\": 1, \"rule\": \"RULE-ID\"}], \"answers\": [{\"rule\": \"RULE-ID\", \
             \"answer\": \"clean\", \"places\": [{\"path\": \"src/a.rs\", \"line\": 10}], \"options\": \
             [{\"id\": \"option-id\", \"count\": 3, \"places\": [\"src/a.rs:10\"]}]}, \
             {\"rule\": \"OTHER-ID\", \"answer\": \"not_a_problem_here\", \"place\": {\"path\": \
             \"src/b.rs\", \"line\": 7}, \"reason\": \"One sentence.\"}]}\n\n\
             # Rules\n",
        );
        for rule in &self.rules {
            out.push_str(&format!("\n## {}: {}\n", rule.id, rule.title));
            out.push_str(&format!(
                "Directive: {}\n",
                as_severity_guidance(&rule.directive)
            ));
            match (&rule.min_severity, &rule.max_severity) {
                (Some(lo), Some(hi)) => {
                    out.push_str(&format!("Suggested severity: {lo}, at most {hi}\n"));
                }
                (Some(lo), None) => out.push_str(&format!("Suggested severity: {lo}\n")),
                (None, Some(hi)) => out.push_str(&format!("Suggested severity: at most {hi}\n")),
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
    /// says about its own work is believed.
    ///
    /// - A `clean` answer needs a place that is a file of the piece (`piece_files`, with line
    ///   counts: not the checklist, the tree or a context file) at a line the trace shows a Read
    ///   returned. Otherwise the rule is not completed.
    /// - A `problems` answer needs problems that exist. When none of its numbers resolves (the
    ///   problem moved to the leads because its path is outside the piece, say) the rule is not
    ///   completed, never "not applicable".
    /// - When a rule has several answers the worst one stands: problems over not completed over
    ///   clean over not applicable.
    /// - A citation needs a listed rule.
    pub fn check(
        &self,
        answers: &ChecklistFile,
        problems: &[Problem],
        trace: &[ToolEvent],
        piece_files: &[(String, usize)],
    ) -> CheckOutcome {
        self.check_with_shown(answers, problems, trace, piece_files, &[])
    }

    /// [`Self::check`] where `shown_in_full` are the files a completed step showed the model in
    /// full ([`crate::ai_audit::agent_finder::AgentRun::shown_in_full`]). Every line of such a
    /// file counts as read, so a `clean` place in it holds up.
    pub fn check_with_shown(
        &self,
        answers: &ChecklistFile,
        problems: &[Problem],
        trace: &[ToolEvent],
        piece_files: &[(String, usize)],
        shown_in_full: &[String],
    ) -> CheckOutcome {
        let numbers: Vec<u32> = problems.iter().map(|p| p.n).collect();
        let line_counts: BTreeMap<String, usize> = piece_files
            .iter()
            .map(|(p, n)| (plain_path(p), *n))
            .collect();
        let mut returned: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
        for e in trace {
            if let ToolEvent::Read {
                path,
                first_line,
                lines_returned,
                ..
            } = e
            {
                if *lines_returned > 0 {
                    let first = (*first_line).max(1);
                    returned
                        .entry(plain_path(path))
                        .or_default()
                        .push((first, first + lines_returned - 1));
                }
            }
        }
        for shown in shown_in_full {
            let path = plain_path(shown);
            if let Some(&lines) = line_counts.get(&path) {
                returned.entry(path).or_default().push((1, lines.max(1)));
            }
        }
        // A place is valid when it names a file of the piece and a line a Read returned (or a
        // file a completed step showed in full).
        let place_is_valid = |p: &Place| -> bool {
            let path = plain_path(&p.path);
            let (Some(&lines), Some(line)) = (line_counts.get(&path), p.line) else {
                return false;
            };
            line >= 1
                && line <= lines
                && returned
                    .get(&path)
                    .is_some_and(|ranges| ranges.iter().any(|&(a, b)| a <= line && line <= b))
        };
        let listed: std::collections::BTreeSet<String> = self
            .rules
            .iter()
            .map(|r| r.id.to_ascii_uppercase())
            .collect();
        let mut out = CheckOutcome::default();
        for rule in &self.rules {
            let given: Vec<&RuleAnswer> = answers
                .answers
                .iter()
                .filter(|a| a.rule.eq_ignore_ascii_case(&rule.id))
                .collect();
            if given.is_empty() {
                out.missing.push(rule.id.clone());
                continue;
            }
            let mut states: Vec<(Effective, &RuleAnswer)> = Vec::new();
            for answer in given {
                let state = match answer.kind {
                    AnswerKind::NotApplicable => Effective::NotApplicable,
                    AnswerKind::NotAProblemHere => {
                        if let (Some(place), Some(reason)) =
                            (answer.places.first(), answer.reason.as_ref())
                        {
                            out.not_a_problem_notes.push(NotAProblemNote {
                                rule: rule.id.clone(),
                                place: place.clone(),
                                reason: reason.trim().to_string(),
                            });
                        }
                        Effective::NotAProblemHere
                    }
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
                            Effective::NotCompleted(
                                "the answer named no problem that exists".to_string(),
                            )
                        } else {
                            Effective::Problems(real)
                        }
                    }
                    AnswerKind::Clean => {
                        let valid: Vec<Place> = answer
                            .places
                            .iter()
                            .filter(|p| place_is_valid(p))
                            .cloned()
                            .collect();
                        if valid.is_empty() {
                            out.downgraded_clean.push(rule.id.clone());
                            Effective::NotCompleted(
                                "no place to check was named in a file of the part at a line that was read"
                                    .to_string(),
                            )
                        } else {
                            Effective::Clean(valid)
                        }
                    }
                };
                states.push((state, answer));
            }
            // The worst answer stands; the first of equal rank, with problem numbers and places
            // of equal-rank answers joined.
            let worst = states.iter().map(|(s, _)| s.rank()).max().unwrap_or(0);
            let mut chosen: Option<(Effective, &RuleAnswer)> = None;
            for (state, answer) in states {
                if state.rank() != worst {
                    continue;
                }
                chosen = Some(match chosen.take() {
                    None => (state, answer),
                    Some((Effective::Problems(mut a), first)) => {
                        if let Effective::Problems(b) = state {
                            for n in b {
                                if !a.contains(&n) {
                                    a.push(n);
                                }
                            }
                        }
                        (Effective::Problems(a), first)
                    }
                    Some((Effective::Clean(mut a), first)) => {
                        if let Effective::Clean(b) = state {
                            for p in b {
                                if !a.contains(&p) {
                                    a.push(p);
                                }
                            }
                        }
                        (Effective::Clean(a), first)
                    }
                    Some(kept) => kept,
                });
            }
            let Some((state, answer)) = chosen else {
                continue;
            };
            // A downgraded clean that lost to a worse answer is not a downgrade of the record.
            if !matches!(state, Effective::NotCompleted(_)) {
                out.downgraded_clean.retain(|r| r != &rule.id);
            } else {
                out.not_completed.push(rule.id.clone());
            }
            out.downgraded_clean.dedup();
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
