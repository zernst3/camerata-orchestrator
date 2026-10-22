//! The deterministic facts a candidate carries. Every sentence a finding states about a
//! candidate (where it is, how long, whether the copies agree and what differs) comes from these
//! fields and nowhere else; the only non-deterministic clause in a finding is the judge's concept.

use crate::extract::clone::{CloneFamily, CodeSpan};

/// The three rules this producer serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CloneRule {
    /// `ARCH-SINGLE-SOURCE-LOGIC-1`: one application decision written in several places.
    SingleSourceLogic,
    /// `ARCH-NAMING-ONE-CONCEPT-1`: one helper under several names.
    NamingOneConcept,
    /// `UI-SHARED-PRIMITIVE-1`: one UI mechanism hand-written in several components.
    SharedPrimitive,
}

impl CloneRule {
    /// Every rule, in the order the producer builds them.
    pub const ALL: [CloneRule; 3] = [
        CloneRule::SingleSourceLogic,
        CloneRule::NamingOneConcept,
        CloneRule::SharedPrimitive,
    ];

    /// The corpus rule id.
    pub fn id(self) -> &'static str {
        match self {
            CloneRule::SingleSourceLogic => super::RULE_SINGLE_SOURCE,
            CloneRule::NamingOneConcept => super::RULE_NAMING,
            CloneRule::SharedPrimitive => super::RULE_SHARED_PRIMITIVE,
        }
    }

    /// The rule for a corpus id, matched exactly (ids are upper case in the corpus).
    pub fn from_id(id: &str) -> Option<CloneRule> {
        CloneRule::ALL.into_iter().find(|rule| rule.id() == id)
    }

    /// The closed verdict vocabulary the judge may answer with for this rule. Only the FIRST
    /// word is an affirmative; every other word, `unsure` included, can never ship a finding.
    pub fn verdict_words(self) -> &'static [&'static str] {
        match self {
            CloneRule::SingleSourceLogic => {
                &["same_decision", "different", "boilerplate", "unsure"]
            }
            CloneRule::NamingOneConcept => {
                &["same_concept", "different_concept", "adapter", "unsure"]
            }
            CloneRule::SharedPrimitive => &["same_mechanism", "different", "unsure"],
        }
    }

    /// The one verdict word that ships a finding.
    pub fn affirmative_word(self) -> &'static str {
        self.verdict_words()[0]
    }
}

/// One place a candidate lives: a path and the lines of its span.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Site {
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
}

impl Site {
    pub(super) fn of(span: &CodeSpan) -> Site {
        Site {
            path: span.file.clone(),
            start_line: span.start_line,
            end_line: span.end_line,
        }
    }

    /// `41-52`, or `41` for a one-line span.
    pub fn lines(&self) -> String {
        if self.start_line == self.end_line {
            self.start_line.to_string()
        } else {
            format!("{}-{}", self.start_line, self.end_line)
        }
    }
}

/// One position at which a copy differs from the first copy once local names are set aside: a
/// property name, a literal value or an operator. Both lines are real source lines, so a client
/// can check the claim in one look.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedDisagreement {
    /// The first copy (members are sorted by path).
    pub path_a: String,
    /// The copy that differs from it.
    pub path_b: String,
    pub line_a: u32,
    pub line_b: u32,
    /// The token text in the first copy (`0.2`, `??`, `total`).
    pub a: String,
    /// The token text in the differing copy.
    pub b: String,
}

/// A declared function name and where it is (ARCH-NAMING-ONE-CONCEPT-1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameSite {
    /// The declared name; `None` for an anonymous or default-exported function.
    pub name: Option<String>,
    pub site: Site,
}

/// One cloned function pair inside a module overlap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModulePair {
    /// The function in the first file of the overlap, and the one in the second.
    pub a: NameSite,
    pub b: NameSite,
    pub token_len: u32,
    /// Whether the two bodies agree token for token once local names are set aside.
    pub agree: bool,
}

/// Two files linked by two or more cloned function pairs (ARCH-NAMING-ONE-CONCEPT-1, shape b).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleOverlap {
    /// The two paths, in sorted order.
    pub files: [String; 2],
    pub pairs: Vec<ModulePair>,
}

/// What only one rule knows about a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleFacts {
    /// ARCH-SINGLE-SOURCE-LOGIC-1.
    Logic {
        /// The Exact text of the operator, literal and property tokens of the shared sequence,
        /// in order (for ranking and for the judge).
        decision_tokens: Vec<String>,
    },
    /// ARCH-NAMING-ONE-CONCEPT-1.
    Naming {
        /// Every member's declared name and place.
        names: Vec<NameSite>,
        /// Present when the candidate is a roll-up of several cloned pairs between two files.
        module: Option<ModuleOverlap>,
    },
    /// UI-SHARED-PRIMITIVE-1.
    Primitive {
        /// Which mechanism marks fired in the shared sequence (`onClick`, `useEffect`,
        /// `addEventListener`, ...), first occurrence order.
        mechanism_marks: Vec<String>,
        /// The JSX tag names of the shared sequence, in order, without repeats.
        jsx_tags: Vec<String>,
        /// The `use*` hooks the shared sequence calls, in order, without repeats.
        hooks: Vec<String>,
        /// The string literals that differ between copies (labels, class names, event names):
        /// the difference the structural match set aside, named so a client sees it.
        differing_strings: Vec<NamedDisagreement>,
    },
}

/// The deterministic facts a finding states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateFacts {
    /// Tokens in the shared sequence (every member has exactly this many).
    pub token_len: u32,
    /// Every place the candidate lives, sorted by path then line.
    pub sites: Vec<Site>,
    /// Whether the Exact-mode token sequences of all members are identical.
    pub agree: bool,
    /// Where the copies differ from the first copy, deduplicated per differing pair of texts.
    pub disagreements: Vec<NamedDisagreement>,
    pub rule: RuleFacts,
}

/// One candidate: a clone family that passed a rule's deterministic filters. It is never a
/// finding; only an affirmative judge verdict promotes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub rule: CloneRule,
    /// `<RULE>:<family key hex>:<token_len>`; stable across scans of the same tree.
    pub id: String,
    /// The family this candidate came from (for a module roll-up, the longest pair's family).
    pub family: CloneFamily,
    pub facts: CandidateFacts,
    /// 0 is judged first (the rule's deterministic ranking).
    pub rank: u32,
}

impl Candidate {
    /// How many places the candidate lives in.
    pub fn member_count(&self) -> usize {
        self.facts.sites.len()
    }
}

/// What the index looked like, for the ledger and the transcript.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexSummary {
    /// Files the detector tokenized and indexed.
    pub files_read: u32,
    /// Files the detector skipped (tests, generated, vendored, minified, data, unparsable).
    pub files_skipped: u32,
    /// Families the detector found, before any rule filter.
    pub families: u32,
    /// A cap fired: the index is partial and an absence proves nothing.
    pub truncated: bool,
    /// Files UI-SHARED-PRIMITIVE-1 could read: indexed TypeScript, TSX or JavaScript files in a
    /// package that depends on a React-family library. Zero means the rule has nothing in scope
    /// in this repo, so "nothing found" says nothing. Counted only when that rule is armed.
    pub ui_files: u32,
}

/// Every candidate of every armed rule.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CandidateSet {
    /// One entry per ARMED rule, ranked, at most [`super::CLONE_JUDGE_MAX_PER_RULE`] long. An
    /// armed rule with nothing to judge has an empty list (it ran; it found nothing).
    pub by_rule: std::collections::BTreeMap<CloneRule, Vec<Candidate>>,
    /// Candidates found beyond the cap, per armed rule: counted and disclosed, never judged.
    pub over_cap: std::collections::BTreeMap<CloneRule, u32>,
    pub index: IndexSummary,
}

impl CandidateSet {
    /// How many candidates a rule found in all: the judged ones plus those over the cap.
    pub fn found(&self, rule: CloneRule) -> u32 {
        let judged = self
            .by_rule
            .get(&rule)
            .map_or(0, |c| u32::try_from(c.len()).unwrap_or(u32::MAX));
        judged.saturating_add(self.over_cap.get(&rule).copied().unwrap_or(0))
    }
}
