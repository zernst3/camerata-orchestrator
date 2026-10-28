//! Do the copies agree? Every member's Exact-mode token sequence is compared with the first
//! member's. Structural equality already holds, so the only tokens that can differ are property
//! names, literal values, operator text and callee names, and each difference is reported with
//! both lines. A free identifier that is NOT a callee collapses in Exact mode (a rename of a
//! variable is not a difference), so that claim is checked here: the names must map one to one
//! between the two copies, or the place where they do not is reported as a difference.

use std::collections::HashMap;

use crate::extract::clone::{exact_diff, CodeSpan, TokenClass, TEXT_IDENT};

use super::context::Ctx;
use super::decision::decision_test_mask;
use super::facts::NamedDisagreement;

/// The differences between the first member and each other member. `None` when a member's tokens
/// cannot be resolved (the candidate then states nothing it cannot prove, so it is not built).
pub(super) fn compare_members(
    ctx: &mut Ctx<'_>,
    members: &[CodeSpan],
) -> Option<Vec<NamedDisagreement>> {
    let first = members.first()?;
    let first_view = ctx.exact_view(first)?;
    let lang = ctx.lang(&first.file)?;
    let in_test = decision_test_mask(lang, first_view.tokens());
    let mut out: Vec<NamedDisagreement> = Vec::new();
    for other in members.iter().skip(1) {
        let other_view = ctx.exact_view(other)?;
        let mut seen: Vec<(String, String)> = Vec::new();
        for d in exact_diff(first_view.item(), first, other_view.item(), other) {
            // The same pair of texts repeated down a block is one difference, named once at its
            // first position.
            let pair = (d.a.clone(), d.b.clone());
            if seen.contains(&pair) {
                continue;
            }
            seen.push(pair);
            let position = d.position as usize;
            let decision = first_view
                .tokens()
                .get(position)
                .is_some_and(|t| match t.class {
                    TokenClass::BinOp | TokenClass::Num => true,
                    TokenClass::Prop => in_test.get(position).copied().unwrap_or(false),
                    _ => false,
                });
            out.push(NamedDisagreement {
                path_a: first.file.clone(),
                path_b: other.file.clone(),
                line_a: d.line_a,
                line_b: d.line_b,
                a: d.a,
                b: d.b,
                decision,
            });
        }
        out.extend(inconsistent_renames(
            ctx,
            (first, &first_view),
            (other, &other_view),
            &mut seen,
        ));
    }
    Some(out)
}

/// The places where a free identifier of the first copy and the one at the same position of the
/// other copy are not a one-to-one renaming: `base` mapped to `amount` everywhere else but to
/// `order` here. Identifiers that Exact mode keeps (callees) were compared by `exact_diff`.
fn inconsistent_renames(
    ctx: &Ctx<'_>,
    (first, first_view): (&CodeSpan, &super::context::SpanView),
    (other, other_view): (&CodeSpan, &super::context::SpanView),
    seen: &mut Vec<(String, String)>,
) -> Vec<NamedDisagreement> {
    let mut forward: HashMap<&str, &str> = HashMap::new();
    let mut backward: HashMap<&str, &str> = HashMap::new();
    let mut out = Vec::new();
    let tokens = first_view.tokens().iter().zip(other_view.tokens());
    for (offset, (x, y)) in tokens.enumerate() {
        let collapsed = |view: &super::context::SpanView| view.text(offset) == Some(TEXT_IDENT);
        if x.class != TokenClass::Ident
            || y.class != TokenClass::Ident
            || !collapsed(first_view)
            || !collapsed(other_view)
        {
            continue;
        }
        let (Some(name_a), Some(name_b)) = (
            ctx.token_text(&first.file, x),
            ctx.token_text(&other.file, y),
        ) else {
            continue;
        };
        let one_to_one = *forward.entry(name_a).or_insert(name_b) == name_b
            && *backward.entry(name_b).or_insert(name_a) == name_a;
        let pair = (name_a.to_string(), name_b.to_string());
        if one_to_one || seen.contains(&pair) {
            continue;
        }
        seen.push(pair);
        out.push(NamedDisagreement {
            path_a: first.file.clone(),
            path_b: other.file.clone(),
            line_a: x.line,
            line_b: y.line,
            a: name_a.to_string(),
            b: name_b.to_string(),
            decision: false,
        });
    }
    out
}

/// The disagreements that sit on string tokens (labels, class names, event names): the
/// difference UI-SHARED-PRIMITIVE-1's structural match sets aside and names in its message.
pub(super) fn string_differences(
    ctx: &mut Ctx<'_>,
    members: &[CodeSpan],
) -> Option<Vec<NamedDisagreement>> {
    let first = members.first()?;
    let first_view = ctx.exact_view(first)?;
    let mut out: Vec<NamedDisagreement> = Vec::new();
    for other in members.iter().skip(1) {
        let other_view = ctx.exact_view(other)?;
        let mut seen: Vec<(String, String)> = Vec::new();
        for d in exact_diff(first_view.item(), first, other_view.item(), other) {
            let on_string = first_view
                .tokens()
                .get(d.position as usize)
                .is_some_and(|t| t.class == TokenClass::Str);
            let pair = (d.a.clone(), d.b.clone());
            if !on_string || seen.contains(&pair) {
                continue;
            }
            seen.push(pair);
            out.push(NamedDisagreement {
                path_a: first.file.clone(),
                path_b: other.file.clone(),
                line_a: d.line_a,
                line_b: d.line_b,
                a: d.a,
                b: d.b,
                decision: false,
            });
        }
    }
    Some(out)
}
