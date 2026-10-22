//! The candidate producer for the three clone-based rules (W29 Wave C):
//! `ARCH-SINGLE-SOURCE-LOGIC-1`, `ARCH-NAMING-ONE-CONCEPT-1` and `UI-SHARED-PRIMITIVE-1`.
//!
//! Spec: `docs/design/2026-10-21_w29-detector-implementation.md`, the three rule sections and
//! "Wiring the H rules: the advisory seam" (and the Wave C build notes for every place the build
//! decided something the spec left open).
//!
//! # Why this is not an `ArchChecker`
//!
//! A registered checker's `check()` output becomes a `Defect` finding with no judgment in
//! between, and the write-time runner and the `camerata-check` CI binary, which have no model,
//! run every registered checker and would read an empty `check()` as clean. Whether a repeated
//! block is one application decision, one concept or one UI mechanism is a judgment that
//! deterministic code cannot make, so this module produces CANDIDATES only. The server's
//! advisory seam (`onboard::clone_advisory`) puts a closed, one-question model judgment between a
//! candidate and a finding; only an affirmative verdict ships. Everything a finding then states
//! about a candidate (where it is, how long, whether the copies agree and what differs) is a
//! fact this module computed by token comparison.
//!
//! # The funnel
//!
//! [`candidates`] builds the shared clone index once ([`crate::extract::clone::detect`]), then
//! runs each armed rule's deterministic filters over every family: token-length threshold,
//! distinct files, token-kind diversity, house-pattern exclusion, the rule's own structural test
//! (a decision, a whole function under different names, a UI mechanism), the composed-variant
//! guards, and the exact-mode agreement comparison. Survivors are ranked per rule and capped at
//! [`CLONE_JUDGE_MAX_PER_RULE`]; the overflow is counted, never judged, never shipped.

use std::collections::{BTreeMap, HashSet};

use crate::extract::clone::{detect, CloneFamily, CloneOptions, SkipReason};

mod bindings;
mod compare;
mod composed;
mod context;
mod decision;
mod facts;
mod judge;
mod names;
mod naming;
mod primitive;
mod single_source;

#[cfg(test)]
mod tests;

pub use facts::{
    Candidate, CandidateFacts, CandidateSet, CloneRule, IndexSummary, ModuleOverlap, ModulePair,
    NameSite, NamedDisagreement, RuleFacts, Site,
};
pub use judge::{judge_input, JudgeDisagreement, JudgeInput, JudgeMember, JudgeNameSite};

use context::Ctx;

pub const RULE_SINGLE_SOURCE: &str = "ARCH-SINGLE-SOURCE-LOGIC-1";
pub const RULE_NAMING: &str = "ARCH-NAMING-ONE-CONCEPT-1";
pub const RULE_SHARED_PRIMITIVE: &str = "UI-SHARED-PRIMITIVE-1";
/// Every rule id this producer serves.
pub const RULE_IDS: &[&str] = &[RULE_SINGLE_SOURCE, RULE_NAMING, RULE_SHARED_PRIMITIVE];

/// The files the producer reads (the language set of the extractor layer).
pub const INTEREST_GLOBS: &[&str] = &[
    "**/*.ts", "**/*.tsx", "**/*.js", "**/*.jsx", "**/*.mjs", "**/*.cjs", "**/*.py", "**/*.rs",
];

/// At most this many candidates per rule are handed to the judge in one scan. A constant, not a
/// configuration knob, so the cost of a scan is bounded by construction.
pub const CLONE_JUDGE_MAX_PER_RULE: usize = 160;

/// ARCH-SINGLE-SOURCE-LOGIC-1 needs a shared sequence at least this long (Structural tokens).
pub(crate) const SINGLE_SOURCE_MIN_TOKENS: u32 = 60;
/// ARCH-NAMING-ONE-CONCEPT-1 needs a shared sequence at least this long.
pub(crate) const NAMING_MIN_TOKENS: u32 = 60;
/// UI-SHARED-PRIMITIVE-1 sits at the detector's floor: JSX is punctuation-dense.
pub(crate) const PRIMITIVE_MIN_TOKENS: u32 = 40;
/// UI-SHARED-PRIMITIVE-1 reports at most this many places for one mechanism.
pub(crate) const PRIMITIVE_MAX_MEMBERS: usize = 40;
/// A shared sequence must hold at least this many distinct token kinds.
pub(crate) const MIN_DISTINCT_KINDS: usize = 8;

/// 64-bit FNV-1a, the same hash the detector keys families with.
pub(crate) fn fnv64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes.iter().fold(OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(PRIME)
    })
}

/// `<RULE>:<family key hex>:<token_len>`: stable across scans of the same tree.
pub(crate) fn candidate_id(rule: CloneRule, family: &CloneFamily) -> String {
    format!("{}:{:016x}:{}", rule.id(), family.key, family.token_len)
}

fn skip_count(index: &crate::extract::clone::CloneIndex) -> u32 {
    let skipped = index
        .skipped
        .iter()
        // A file that is not source at all (a manifest, a README) was never a candidate for
        // indexing; only count what the detector chose not to read.
        .filter(|(_, reason)| *reason != SkipReason::NotSource)
        .count();
    u32::try_from(skipped).unwrap_or(u32::MAX)
}

/// Build the candidates of every armed rule over `files`. Deterministic; never panics; an unarmed
/// rule costs nothing (the index is not even built when no clone rule is armed).
pub fn candidates(files: &[(String, String)], armed: &HashSet<&str>) -> CandidateSet {
    let rules: Vec<CloneRule> = CloneRule::ALL
        .into_iter()
        .filter(|rule| armed.contains(rule.id()))
        .collect();
    let mut set = CandidateSet::default();
    if rules.is_empty() {
        return set;
    }
    let index = detect(files, &CloneOptions::default());
    set.index = IndexSummary {
        files_read: index.files_read,
        files_skipped: skip_count(&index),
        families: u32::try_from(index.families.len()).unwrap_or(u32::MAX),
        truncated: index.truncated,
    };
    let mut ctx = Ctx::new(files);
    // Families that share a first member are processed together, so a file's tokens are read
    // while they are still cached. The order affects time only: the output is ranked.
    let mut order: Vec<&CloneFamily> = index.families.iter().collect();
    order.sort_by(|a, b| {
        let key = |f: &CloneFamily| {
            f.members
                .first()
                .map(|m| m.file.clone())
                .unwrap_or_default()
        };
        key(a).cmp(&key(b)).then(a.key.cmp(&b.key))
    });
    let mut built: BTreeMap<CloneRule, Vec<Candidate>> = BTreeMap::new();
    for rule in &rules {
        built.insert(*rule, Vec::new());
    }
    for family in order {
        for rule in &rules {
            let candidate = match rule {
                CloneRule::SingleSourceLogic => single_source::build(&mut ctx, family),
                CloneRule::NamingOneConcept => naming::build(&mut ctx, family),
                CloneRule::SharedPrimitive => primitive::build(&mut ctx, family),
            };
            if let Some(candidate) = candidate {
                built.entry(*rule).or_default().push(candidate);
            }
        }
    }
    for (rule, list) in built {
        let (kept, over) = rank_and_cap(rule, list, CLONE_JUDGE_MAX_PER_RULE);
        set.over_cap.insert(rule, over);
        set.by_rule.insert(rule, kept);
    }
    set
}

/// Rank a rule's candidates and keep the first `max`; the count of the rest is returned, not
/// dropped silently. Each kept candidate's `rank` is its position.
fn rank_and_cap(rule: CloneRule, list: Vec<Candidate>, max: usize) -> (Vec<Candidate>, u32) {
    let ranked = rank(rule, list);
    let found = ranked.len();
    let kept: Vec<Candidate> = ranked
        .into_iter()
        .take(max)
        .enumerate()
        .map(|(position, mut candidate)| {
            candidate.rank = u32::try_from(position).unwrap_or(u32::MAX);
            candidate
        })
        .collect();
    let over = u32::try_from(found.saturating_sub(kept.len())).unwrap_or(u32::MAX);
    (kept, over)
}

/// The rule's deterministic ranking (best first). The naming rule rolls module overlaps up first.
fn rank(rule: CloneRule, list: Vec<Candidate>) -> Vec<Candidate> {
    let mut list = match rule {
        CloneRule::NamingOneConcept => naming::roll_up(list),
        _ => list,
    };
    match rule {
        CloneRule::SingleSourceLogic => list.sort_by(single_source::compare),
        CloneRule::NamingOneConcept => list.sort_by(naming::compare),
        CloneRule::SharedPrimitive => list.sort_by(primitive::compare),
    }
    list
}
