//! Do the copies agree? Every member's Exact-mode token sequence is compared with the first
//! member's. Structural equality already holds, so the only tokens that can differ are property
//! names, literal values and operator text, and each difference is reported with both lines.

use crate::extract::clone::{exact_diff, CodeSpan, TokenClass};

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
    }
    Some(out)
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
