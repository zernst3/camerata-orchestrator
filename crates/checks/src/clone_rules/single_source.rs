//! ARCH-SINGLE-SOURCE-LOGIC-1: one application decision written in several places.
//!
//! A candidate is a clone family that is long enough, spans files, is not a framework pattern,
//! contains a decision outside any error handler, runs in every place it appears, and is not
//! merely the call sites of one shared helper. Whether it is one application decision is the
//! judge's question; whether the copies agree is decided here, by token comparison.

use std::cmp::Ordering;

use crate::extract::clone::CloneFamily;

use super::compare::compare_members;
use super::composed::calls_one_shared_helper;
use super::context::Ctx;
use super::decision::{decision_tokens, distinct_kinds, has_decision, runs_in_a_body};
use super::facts::{Candidate, CandidateFacts, CloneRule, RuleFacts, Site};
use super::{MIN_DISTINCT_KINDS, SINGLE_SOURCE_MIN_TOKENS};

/// Build the candidate for a family, or `None` when any deterministic filter rejects it.
pub(super) fn build(ctx: &mut Ctx<'_>, family: &CloneFamily) -> Option<Candidate> {
    if family.token_len < SINGLE_SOURCE_MIN_TOKENS
        || family.distinct_files < 2
        || family.house_pattern
    {
        return None;
    }
    let first = family.members.first()?;
    let lang = ctx.lang(&first.file)?;
    let structural = ctx.structural_view(first)?;
    if distinct_kinds(structural.tokens()) < MIN_DISTINCT_KINDS {
        return None;
    }
    if !has_decision(&ctx.ids, lang, structural.tokens()) {
        return None;
    }
    for member in &family.members {
        let view = ctx.structural_view(member)?;
        if !runs_in_a_body(ctx, member, &view) {
            return None;
        }
    }
    if calls_one_shared_helper(ctx, &family.members) {
        return None;
    }
    let disagreements = compare_members(ctx, &family.members)?;
    let exact = ctx.exact_view(first)?;
    let decision = decision_tokens(lang, &exact);
    let mut member_tokens: Vec<String> = Vec::new();
    for member in &family.members {
        let view = ctx.exact_view(member)?;
        member_tokens.extend(decision_tokens(lang, &view));
    }
    member_tokens.sort_unstable();
    let mut sites: Vec<Site> = family.members.iter().map(Site::of).collect();
    sites.sort();
    Some(Candidate {
        rule: CloneRule::SingleSourceLogic,
        id: super::candidate_id(CloneRule::SingleSourceLogic, family),
        family: family.clone(),
        facts: CandidateFacts {
            token_len: family.token_len,
            sites,
            agree: disagreements.is_empty(),
            disagreements,
            rule: RuleFacts::Logic {
                decision_tokens: decision,
                member_decision_tokens: member_tokens,
            },
        },
        rank: 0,
    })
}

fn decision_count(c: &Candidate) -> u64 {
    match &c.facts.rule {
        RuleFacts::Logic {
            decision_tokens, ..
        } => decision_tokens.len() as u64,
        _ => 0,
    }
}

/// The ranking: copies that disagree first, then more places, then denser in decision tokens,
/// then longer, then by path (and id, so the order is total).
pub(super) fn compare(a: &Candidate, b: &Candidate) -> Ordering {
    a.facts
        .agree
        .cmp(&b.facts.agree)
        .then_with(|| b.member_count().cmp(&a.member_count()))
        .then_with(|| {
            // decisions / token_len, compared by cross-multiplication (no floating point).
            let left = decision_count(b) * u64::from(a.facts.token_len);
            let right = decision_count(a) * u64::from(b.facts.token_len);
            left.cmp(&right)
        })
        .then_with(|| b.facts.token_len.cmp(&a.facts.token_len))
        .then_with(|| first_path(a).cmp(first_path(b)))
        .then_with(|| a.id.cmp(&b.id))
}

pub(super) fn first_path(c: &Candidate) -> &str {
    c.facts.sites.first().map_or("", |s| s.path.as_str())
}
