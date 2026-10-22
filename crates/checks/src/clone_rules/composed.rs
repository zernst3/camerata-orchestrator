//! The composed-variant guards: a family whose copies all call the same imported helper is the
//! helper's call site plus its options, not a second implementation of the helper. The guard
//! fires only when that call DOMINATES the shared sequence (W29 Wave C review, D2): a decision
//! written around an unrelated imported call (`render(`, `auth(`, `useState(`, `t(`) is still a
//! decision written in two places.

use crate::extract::clone::{CodeSpan, Token, TokenClass};
use crate::extract::SourceLang;

use super::bindings::{imports_name, module_of};
use super::context::{is_punct, Ctx, SpanView};
use super::decision::is_decision_token;

/// The shared imported call dominates the sequence when its tokens cover at least this share of
/// it, as a percentage.
const DOMINANT_PERCENT: usize = 50;

/// Offsets inside a span where a free identifier is called: an identifier token directly
/// followed by `(`.
fn call_offsets(tokens: &[Token]) -> Vec<usize> {
    (0..tokens.len().saturating_sub(1))
        .filter(|&i| tokens[i].class == TokenClass::Ident && is_punct(&tokens[i + 1], "("))
        .collect()
}

/// The callee text at `offset`, from the source.
fn callee_at<'a>(
    ctx: &Ctx<'a>,
    span: &CodeSpan,
    view: &SpanView,
    offset: usize,
) -> Option<&'a str> {
    ctx.token_text(&span.file, view.tokens().get(offset)?)
}

/// The index of the `)` that closes the `(` at `open`; the last index when the span ends first
/// (which over-extends the call, and so errs toward dropping the candidate).
fn matching_paren(tokens: &[Token], open: usize) -> usize {
    let mut depth = 0usize;
    for (i, token) in tokens.iter().enumerate().skip(open) {
        if is_punct(token, "(") {
            depth += 1;
        } else if is_punct(token, ")") {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return i;
            }
        }
    }
    tokens.len().saturating_sub(1)
}

/// Whether the call whose callee sits at `offset` dominates the sequence: its tokens cover at
/// least [`DOMINANT_PERCENT`] of it, OR every decision-bearing token of the sequence lies inside
/// the call's argument list (the decision then belongs to the call's options).
fn call_dominates(lang: SourceLang, tokens: &[Token], offset: usize) -> bool {
    let end = matching_paren(tokens, offset + 1);
    let covered = end + 1 - offset;
    if covered * 100 >= tokens.len() * DOMINANT_PERCENT {
        return true;
    }
    tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| is_decision_token(lang, token))
        .all(|(i, _)| i > offset + 1 && i < end)
}

/// Whether the shared sequence contains a call to a helper that every member file imports from
/// the same module (`composed_variant`, ARCH-SINGLE-SOURCE-LOGIC-1): the same callee name at the
/// same position in every copy, imported in each copy's file from one resolved module, AND that
/// call dominates the sequence ([`call_dominates`]).
pub(super) fn calls_one_shared_helper(ctx: &mut Ctx<'_>, members: &[CodeSpan]) -> bool {
    let Some(first) = members.first() else {
        return false;
    };
    let mut views = Vec::with_capacity(members.len());
    for member in members {
        match ctx.structural_view(member) {
            Some(view) => views.push(view),
            None => return false,
        }
    }
    for offset in call_offsets(views[0].tokens()) {
        let Some(name) = callee_at(ctx, first, &views[0], offset) else {
            continue;
        };
        let mut module: Option<String> = None;
        let mut every_member = true;
        for (member, view) in members.iter().zip(&views) {
            let is_call = view
                .tokens()
                .get(offset)
                .is_some_and(|t| t.class == TokenClass::Ident)
                && view
                    .tokens()
                    .get(offset + 1)
                    .is_some_and(|t| is_punct(t, "("));
            let same_name = callee_at(ctx, member, view, offset) == Some(name);
            let Some(lang) = ctx.lang(&member.file) else {
                every_member = false;
                break;
            };
            let bindings = ctx.bindings(&member.file);
            let resolved = module_of(&bindings, lang, &member.file, name);
            match (is_call && same_name, resolved) {
                (true, Some(found)) => match &module {
                    None => module = Some(found),
                    Some(existing) if *existing == found => {}
                    Some(_) => {
                        every_member = false;
                        break;
                    }
                },
                _ => {
                    every_member = false;
                    break;
                }
            }
        }
        if every_member && module.is_some() {
            let dominant = ctx
                .lang(&first.file)
                .is_some_and(|lang| call_dominates(lang, views[0].tokens(), offset));
            if dominant {
                return true;
            }
        }
    }
    false
}

/// Whether any member calls the function another member declares, having imported it: a wrapper
/// over the other function rather than a copy of it (ARCH-NAMING-ONE-CONCEPT-1's adapter guard).
/// `named` pairs each member span with the function name it declares.
pub(super) fn wraps_another_member(ctx: &mut Ctx<'_>, named: &[(&CodeSpan, Option<&str>)]) -> bool {
    for (member, _) in named {
        let Some(view) = ctx.structural_view(member) else {
            continue;
        };
        let bindings = ctx.bindings(&member.file);
        for offset in call_offsets(view.tokens()) {
            let Some(callee) = callee_at(ctx, member, &view, offset) else {
                continue;
            };
            let calls_a_sibling = named
                .iter()
                .any(|(other, name)| other.file != member.file && *name == Some(callee));
            if calls_a_sibling && imports_name(&bindings, callee) {
                return true;
            }
        }
    }
    false
}
