//! ARCH-NAMING-ONE-CONCEPT-1: one helper under several names.
//!
//! A candidate is a clone family that is, in every member, essentially a whole top-level function
//! (at least 90% of its tokens), whose declared names carry at least two different concepts, and
//! in which no member is a wrapper over another. Two files linked by two or more such pairs are
//! rolled up into one module candidate that names every overlapping function. Whether the names
//! say the same concept is the judge's question.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use crate::extract::clone::CloneFamily;

use super::compare::compare_members;
use super::composed::wraps_another_member;
use super::context::Ctx;
use super::decision::distinct_kinds;
use super::facts::{
    Candidate, CandidateFacts, CloneRule, ModuleOverlap, ModulePair, NameSite, RuleFacts, Site,
};
use super::names::has_two_different_concepts;
use super::{fnv64, MIN_DISTINCT_KINDS, NAMING_MIN_TOKENS};

/// The share of a function's tokens a member must cover, as a percentage.
const WHOLE_BODY_PERCENT: u64 = 90;

/// Build the function-level candidate for a family, or `None` when any filter rejects it.
pub(super) fn build(ctx: &mut Ctx<'_>, family: &CloneFamily) -> Option<Candidate> {
    if family.token_len < NAMING_MIN_TOKENS || family.distinct_files < 2 || family.house_pattern {
        return None;
    }
    let first = family.members.first()?;
    if distinct_kinds(ctx.structural_view(first)?.tokens()) < MIN_DISTINCT_KINDS {
        return None;
    }
    let mut names: Vec<NameSite> = Vec::with_capacity(family.members.len());
    for member in &family.members {
        let view = ctx.structural_view(member)?;
        let item = view.item();
        // The member must be (nearly) a whole function: a shared middle with different prologues
        // is a logic overlap, not a renamed copy.
        let whole = item.kind == "function"
            && u64::from(member.token_len) * 100 >= WHOLE_BODY_PERCENT * item.tokens.len() as u64;
        if !whole {
            return None;
        }
        names.push(NameSite {
            name: item.name.clone(),
            site: Site::of(member),
        });
    }
    if !has_two_different_concepts(names.iter().filter_map(|n| n.name.as_deref())) {
        return None;
    }
    let named: Vec<_> = family
        .members
        .iter()
        .zip(&names)
        .map(|(span, n)| (span, n.name.as_deref()))
        .collect();
    if wraps_another_member(ctx, &named) {
        return None;
    }
    let disagreements = compare_members(ctx, &family.members)?;
    let mut sites: Vec<Site> = family.members.iter().map(Site::of).collect();
    sites.sort();
    names.sort_by(|a, b| a.site.cmp(&b.site));
    Some(Candidate {
        rule: CloneRule::NamingOneConcept,
        id: super::candidate_id(CloneRule::NamingOneConcept, family),
        family: family.clone(),
        facts: CandidateFacts {
            token_len: family.token_len,
            sites,
            agree: disagreements.is_empty(),
            disagreements,
            rule: RuleFacts::Naming {
                names,
                module: None,
            },
        },
        rank: 0,
    })
}

/// The two paths a two-file candidate links, in order; `None` for any other shape.
fn file_pair(c: &Candidate) -> Option<(String, String)> {
    let mut files: Vec<&str> = c.facts.sites.iter().map(|s| s.path.as_str()).collect();
    files.dedup();
    match files.as_slice() {
        [a, b] => Some(((*a).to_string(), (*b).to_string())),
        _ => None,
    }
}

/// Roll up every pair of files linked by two or more function candidates into one module
/// candidate and remove the function candidates it absorbed. A candidate that spans more than
/// two files is never absorbed (a third file would be lost); it stays a function candidate.
pub(super) fn roll_up(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut links: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for (i, c) in candidates.iter().enumerate() {
        if let Some(pair) = file_pair(c) {
            links.entry(pair).or_default().push(i);
        }
    }
    let mut absorbed = vec![false; candidates.len()];
    let mut modules: Vec<Candidate> = Vec::new();
    for ((file_a, file_b), indexes) in links {
        if indexes.len() < 2 {
            continue;
        }
        if let Some(module) = module_candidate(&file_a, &file_b, &indexes, &candidates) {
            for &i in &indexes {
                absorbed[i] = true;
            }
            modules.push(module);
        }
    }
    let mut out: Vec<Candidate> = candidates
        .into_iter()
        .zip(absorbed)
        .filter_map(|(c, gone)| (!gone).then_some(c))
        .collect();
    out.extend(modules);
    out
}

fn module_candidate(
    file_a: &str,
    file_b: &str,
    indexes: &[usize],
    candidates: &[Candidate],
) -> Option<Candidate> {
    let mut pairs: Vec<ModulePair> = Vec::new();
    let mut sites: Vec<Site> = Vec::new();
    let mut disagreements = Vec::new();
    let mut keys: Vec<u64> = Vec::new();
    let mut total_len = 0u32;
    let mut primary: Option<&Candidate> = None;
    for &i in indexes {
        let c = candidates.get(i)?;
        let RuleFacts::Naming { names, .. } = &c.facts.rule else {
            return None;
        };
        let a = names.iter().find(|n| n.site.path == file_a)?.clone();
        let b = names.iter().find(|n| n.site.path == file_b)?.clone();
        pairs.push(ModulePair {
            a,
            b,
            token_len: c.facts.token_len,
            agree: c.facts.agree,
        });
        sites.extend(c.facts.sites.iter().cloned());
        disagreements.extend(c.facts.disagreements.iter().cloned());
        keys.push(c.family.key);
        total_len = total_len.saturating_add(c.facts.token_len);
        if primary.is_none_or(|p| c.facts.token_len > p.facts.token_len) {
            primary = Some(c);
        }
    }
    sites.sort();
    sites.dedup();
    pairs.sort_by(|x, y| x.a.site.cmp(&y.a.site));
    keys.sort_unstable();
    let mut material: Vec<u8> = Vec::new();
    material.extend_from_slice(file_a.as_bytes());
    material.push(0);
    material.extend_from_slice(file_b.as_bytes());
    for key in &keys {
        material.extend_from_slice(&key.to_le_bytes());
    }
    let key = fnv64(&material);
    let agree = pairs.iter().all(|p| p.agree);
    let names: Vec<NameSite> = pairs
        .iter()
        .flat_map(|p| [p.a.clone(), p.b.clone()])
        .collect();
    Some(Candidate {
        rule: CloneRule::NamingOneConcept,
        id: format!("{}:{key:016x}:{total_len}", super::RULE_NAMING),
        family: primary?.family.clone(),
        facts: CandidateFacts {
            token_len: total_len,
            sites,
            agree,
            disagreements,
            rule: RuleFacts::Naming {
                names,
                module: Some(ModuleOverlap {
                    files: [file_a.to_string(), file_b.to_string()],
                    pairs,
                }),
            },
        },
        rank: 0,
    })
}

fn is_module(c: &Candidate) -> bool {
    matches!(
        &c.facts.rule,
        RuleFacts::Naming {
            module: Some(_),
            ..
        }
    )
}

/// The ranking: module roll-ups first (most sites per call), then copies that disagree, then more
/// places, then longer, then by path (and id, so the order is total).
pub(super) fn compare(a: &Candidate, b: &Candidate) -> Ordering {
    is_module(b)
        .cmp(&is_module(a))
        .then_with(|| a.facts.agree.cmp(&b.facts.agree))
        .then_with(|| b.member_count().cmp(&a.member_count()))
        .then_with(|| b.facts.token_len.cmp(&a.facts.token_len))
        .then_with(|| super::single_source::first_path(a).cmp(super::single_source::first_path(b)))
        .then_with(|| a.id.cmp(&b.id))
}
