//! The deterministic tests ARCH-SINGLE-SOURCE-LOGIC-1 applies to a clone family before the judge
//! sees it: is there a decision in the shared sequence (and not only inside an error handler),
//! and does every copy sit in code that runs (a function, or a module-level statement).

use crate::dead_code_checker::exclusions::is_config_shaped;
use crate::extract::clone::{CodeSpan, Token, TokenClass};
use crate::extract::SourceLang;

use super::context::{is_keyword, is_punct, Ctx, Ids, SpanView};

/// Keywords that open a branch on a condition or a value.
const DECISION_KEYWORDS: &[&str] = &["if", "elif", "match", "switch", "case", "when"];

fn is_open_brace(token: &Token) -> bool {
    is_punct(token, "{") || (token.class == TokenClass::Block && token_is_block(token, "{"))
}

fn is_close_brace(token: &Token) -> bool {
    is_punct(token, "}") || (token.class == TokenClass::Block && token_is_block(token, "}"))
}

fn token_is_block(token: &Token, text: &str) -> bool {
    token.id == crate::extract::clone::token_id(TokenClass::Block, text)
}

/// The index of the brace that closes the one opened at `open`, or the last index when the span
/// ends first.
fn matching_close(tokens: &[Token], open: usize) -> usize {
    let mut depth = 0usize;
    for (i, token) in tokens.iter().enumerate().skip(open) {
        if is_open_brace(token) {
            depth += 1;
        } else if is_close_brace(token) {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return i;
            }
        }
    }
    tokens.len().saturating_sub(1)
}

/// The last index of the `throw`/`raise` statement that starts at `start`: up to a `;` at its own
/// nesting depth, to the close of the enclosing block, and (Python) to the end of its line.
/// Over-extending only removes more tokens from the decision test, which errs toward a false
/// negative.
fn statement_end(tokens: &[Token], start: usize) -> usize {
    let python_raise = tokens.get(start).is_some_and(|t| is_keyword(t, "raise"));
    let mut depth = 0i32;
    for j in start + 1..tokens.len() {
        let token = &tokens[j];
        if is_punct(token, "(") || is_punct(token, "[") || is_open_brace(token) {
            depth += 1;
        } else if is_punct(token, ")") || is_punct(token, "]") || is_close_brace(token) {
            depth -= 1;
            if depth < 0 {
                return j - 1;
            }
        } else if depth == 0 && is_punct(token, ";") {
            return j;
        }
        if python_raise && depth == 0 && token.line > tokens[j - 1].line {
            return j - 1;
        }
    }
    tokens.len().saturating_sub(1)
}

/// Which tokens of a span sit inside a `catch`/`except` clause or a `throw`/`raise` statement.
fn error_regions(tokens: &[Token]) -> Vec<bool> {
    let mut excluded = vec![false; tokens.len()];
    let mut i = 0;
    while i < tokens.len() {
        let token = &tokens[i];
        if is_keyword(token, "catch") || is_keyword(token, "except") {
            // The clause runs from the keyword to the close of its block: skip a parenthesised
            // binding, then take the first block that opens.
            let mut paren = 0i32;
            let mut j = i + 1;
            let mut block = None;
            while j < tokens.len() {
                let t = &tokens[j];
                if is_punct(t, "(") {
                    paren += 1;
                } else if is_punct(t, ")") {
                    paren -= 1;
                } else if paren <= 0 && is_open_brace(t) {
                    block = Some(j);
                    break;
                }
                j += 1;
            }
            let end = block.map_or(tokens.len() - 1, |open| matching_close(tokens, open));
            for flag in &mut excluded[i..=end] {
                *flag = true;
            }
            i = end + 1;
            continue;
        }
        if is_keyword(token, "throw") || is_keyword(token, "raise") {
            let end = statement_end(tokens, i);
            for flag in &mut excluded[i..=end] {
                *flag = true;
            }
            i = end + 1;
            continue;
        }
        i += 1;
    }
    excluded
}

/// Whether the span contains a decision: (a) a comparison, arithmetic or logical operator, a
/// branching keyword or a ternary, AND (b) a literal or a property name; counting only tokens
/// outside `catch`/`except` clauses and `throw`/`raise` statements, so a retry or rethrow
/// skeleton is not a business decision.
pub(super) fn has_decision(ids: &Ids, lang: SourceLang, tokens: &[Token]) -> bool {
    let excluded = error_regions(tokens);
    let mut has_operator = false;
    let mut has_operand = false;
    // Open `?` tokens: (nesting depth, token index).
    let mut questions: Vec<(i32, usize)> = Vec::new();
    let mut depth = 0i32;
    for (i, token) in tokens.iter().enumerate() {
        if excluded[i] {
            continue;
        }
        if is_punct(token, "(") || is_punct(token, "[") || is_open_brace(token) {
            depth += 1;
        } else if is_punct(token, ")") || is_punct(token, "]") || is_close_brace(token) {
            depth -= 1;
            questions.retain(|(d, _)| *d <= depth);
        }
        match token.class {
            TokenClass::BinOp
                if token.id == ids.arith || token.id == ids.cmp || token.id == ids.logic =>
            {
                has_operator = true;
            }
            TokenClass::Keyword if DECISION_KEYWORDS.iter().any(|k| is_keyword(token, k)) => {
                has_operator = true;
            }
            TokenClass::Num | TokenClass::Str | TokenClass::Lit | TokenClass::Prop => {
                has_operand = true;
            }
            _ => {}
        }
        if lang != SourceLang::Rust {
            if is_punct(token, "?") {
                questions.push((depth, i));
            } else if is_punct(token, ":") {
                // `x?: T` (an optional member) has nothing between the two; a ternary does.
                if let Some(pos) = questions.iter().rposition(|(d, _)| *d == depth) {
                    if i - questions[pos].1 > 1 {
                        has_operator = true;
                    }
                    questions.truncate(pos);
                }
            }
        }
        if has_operator && has_operand {
            return true;
        }
    }
    false
}

/// How many distinct token kinds (normalized token ids) the sequence holds. A window of
/// `$ID , $ID , $ID` has two; a tiny vocabulary passes the length test without being logic.
pub(super) fn distinct_kinds(tokens: &[Token]) -> usize {
    let mut ids: Vec<u32> = tokens.iter().map(|t| t.id).collect();
    ids.sort_unstable();
    ids.dedup();
    ids.len()
}

/// The Exact text of the decision-bearing tokens of an Exact-mode span (operators, literals and
/// property names, the branching keywords, and a ternary's `?`), for ranking and for the judge.
/// Each text is capped so one long string cannot dominate the judge input.
pub(super) fn decision_tokens(lang: SourceLang, view: &SpanView) -> Vec<String> {
    const MAX_TEXT: usize = 40;
    let mut out = Vec::new();
    for (offset, token) in view.tokens().iter().enumerate() {
        let wanted = match token.class {
            TokenClass::BinOp
            | TokenClass::Num
            | TokenClass::Str
            | TokenClass::Lit
            | TokenClass::Prop => true,
            TokenClass::Keyword => DECISION_KEYWORDS.iter().any(|k| is_keyword(token, k)),
            TokenClass::Punct => lang != SourceLang::Rust && is_punct(token, "?"),
            _ => false,
        };
        if !wanted {
            continue;
        }
        if let Some(text) = view.text(offset) {
            out.push(text.chars().take(MAX_TEXT).collect());
        }
    }
    out
}

/// Whether the span runs: it is (part of) a function, method or arrow body, or a module-level
/// statement in a file that is not config-shaped. Module-level constant tables are the data
/// guards' business, not this rule's.
pub(super) fn runs_in_a_body(ctx: &mut Ctx<'_>, span: &CodeSpan, view: &SpanView) -> bool {
    let kind = view.item().kind;
    if kind == "function" {
        return true;
    }
    if kind == "statement" && !is_config_shaped(&span.file) {
        return true;
    }
    let (start, end) = (span.start_line as usize, span.end_line as usize);
    ctx.functions(&span.file)
        .iter()
        .any(|f| f.start_line <= start && end <= f.end_line)
}
