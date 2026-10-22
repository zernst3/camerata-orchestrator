//! UI-SHARED-PRIMITIVE-1: one UI mechanism hand-written in several components.
//!
//! A candidate is a clone family, in React-family packages, whose every copy sits in a UI unit (a
//! JSX element, the callback of a React hook, or the body of a custom hook), whose shared
//! sequence has a mechanism (a conditional, an event handler, a hook call, a listener, a ref, a
//! timer or the document), and which is not merely a use of one imported component. Whether the
//! copies are one mechanism is the judge's question.

use std::cmp::Ordering;

use tree_sitter::Node;

use crate::extract::clone::{CloneFamily, CodeSpan, Token, TokenClass};
use crate::extract::SourceLang;

use super::bindings::imports_name;
use super::compare::{compare_members, string_differences};
use super::context::{is_keyword, is_punct, Ctx, SpanView};
use super::decision::distinct_kinds;
use super::facts::{Candidate, CandidateFacts, CloneRule, RuleFacts, Site};
use super::{MIN_DISTINCT_KINDS, PRIMITIVE_MAX_MEMBERS, PRIMITIVE_MIN_TOKENS};

/// React hooks whose callback is a UI unit.
const UI_HOOKS: &[&str] = &[
    "useEffect",
    "useLayoutEffect",
    "useCallback",
    "useMemo",
    "useImperativeHandle",
];

/// Tokens outside a single imported element below which the span is a use of that component.
const COMPOSED_OUTSIDE_TOKENS: usize = 40;

fn is_hook_name(name: &str) -> bool {
    name.strip_prefix("use")
        .and_then(|rest| rest.chars().next())
        .is_some_and(|c| c.is_ascii_uppercase())
}

fn is_jsx(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "jsx_element" | "jsx_self_closing_element" | "jsx_fragment"
    )
}

fn text<'s>(node: Node<'_>, source: &'s str) -> &'s str {
    source.get(node.byte_range()).unwrap_or_default()
}

/// The declared name of a function-like node, when it has one.
fn function_name<'s>(node: Node<'_>, source: &'s str) -> Option<&'s str> {
    match node.kind() {
        "function_declaration"
        | "generator_function_declaration"
        | "function_expression"
        | "method_definition" => node.child_by_field_name("name").map(|n| text(n, source)),
        "variable_declarator" => {
            let value = node.child_by_field_name("value")?;
            matches!(
                value.kind(),
                "arrow_function" | "function_expression" | "function"
            )
            .then(|| node.child_by_field_name("name").map(|n| text(n, source)))
            .flatten()
        }
        _ => None,
    }
}

/// Whether any node below `node` is JSX. Iterative, so a deeply nested tree cannot overflow the
/// stack.
fn subtree_has_jsx(node: Node<'_>) -> bool {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if is_jsx(n) {
            return true;
        }
        let mut cursor = n.walk();
        stack.extend(n.children(&mut cursor));
    }
    false
}

/// A function-like node: its body is a component when it renders JSX.
fn is_function_like(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "function_declaration"
            | "generator_function_declaration"
            | "function_expression"
            | "arrow_function"
            | "method_definition"
    )
}

/// Whether the node, or one of its ancestors, is a UI unit: a JSX node, the callback of a React
/// hook, the body of a custom hook, or the body of a component (a function that renders JSX; a
/// copy-pasted component is the plainest case of this rule).
fn in_ui_unit(node: Node<'_>, source: &str) -> bool {
    let mut current = Some(node);
    while let Some(n) = current {
        if is_jsx(n) || (is_function_like(n) && subtree_has_jsx(n)) {
            return true;
        }
        if n.kind() == "call_expression" {
            if let Some(callee) = n.child_by_field_name("function") {
                let callee = text(callee, source);
                let name = callee.rsplit('.').next().unwrap_or(callee);
                if UI_HOOKS.contains(&name) {
                    return true;
                }
            }
        }
        if function_name(n, source).is_some_and(is_hook_name) {
            return true;
        }
        current = n.parent();
    }
    false
}

/// The first and last byte of a span's tokens.
fn byte_range(tokens: &[Token]) -> Option<(usize, usize)> {
    let first = tokens.first()?;
    let last = tokens.last()?;
    Some((first.raw.start as usize, last.raw.end as usize))
}

/// Whether the span lies in a UI unit of its file.
fn span_in_ui_unit(ctx: &mut Ctx<'_>, member: &CodeSpan, view: &SpanView) -> bool {
    let Some((start, end)) = byte_range(view.tokens()) else {
        return false;
    };
    let (Some(tree), Some(source)) = (ctx.tree(&member.file), ctx.source(&member.file)) else {
        return false;
    };
    let root = tree.root_node();
    root.descendant_for_byte_range(start, end)
        .is_some_and(|node| in_ui_unit(node, source))
}

/// Every outermost JSX node lying wholly inside the byte range.
fn outermost_jsx<'t>(node: Node<'t>, start: usize, end: usize, out: &mut Vec<Node<'t>>) {
    if node.end_byte() <= start || node.start_byte() >= end {
        return;
    }
    if is_jsx(node) && node.start_byte() >= start && node.end_byte() <= end {
        out.push(node);
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        outermost_jsx(child, start, end, out);
    }
}

/// The tag of a JSX element node: `Modal`, `Foo.Bar` (the head `Foo`), `div`.
fn element_tag<'s>(node: Node<'_>, source: &'s str) -> Option<&'s str> {
    let opening = match node.kind() {
        "jsx_element" => node.child_by_field_name("open_tag")?,
        "jsx_self_closing_element" => node,
        _ => return None,
    };
    let name = text(opening.child_by_field_name("name")?, source);
    Some(name.split('.').next().unwrap_or(name))
}

/// Whether the span is one use of an imported component: a single outermost JSX element whose tag
/// the file imports, with fewer than 40 tokens around it.
fn span_is_a_use_of_an_imported_component(
    ctx: &mut Ctx<'_>,
    member: &CodeSpan,
    view: &SpanView,
) -> bool {
    let Some((start, end)) = byte_range(view.tokens()) else {
        return false;
    };
    let (Some(tree), Some(source)) = (ctx.tree(&member.file), ctx.source(&member.file)) else {
        return false;
    };
    let mut found = Vec::new();
    outermost_jsx(tree.root_node(), start, end, &mut found);
    let [element] = found.as_slice() else {
        return false;
    };
    let Some(tag) = element_tag(*element, source) else {
        return false;
    };
    let bindings = ctx.bindings(&member.file);
    if !imports_name(&bindings, tag) {
        return false;
    }
    let (from, to) = (element.start_byte(), element.end_byte());
    let inside = view
        .tokens()
        .iter()
        .filter(|t| t.raw.start as usize >= from && t.raw.end as usize <= to)
        .count();
    view.tokens().len().saturating_sub(inside) < COMPOSED_OUTSIDE_TOKENS
}

/// The mechanism marks, JSX tags and hook calls of one copy, in first-occurrence order.
#[derive(Default)]
struct Reading {
    marks: Vec<String>,
    jsx_tags: Vec<String>,
    hooks: Vec<String>,
}

fn push_unique(list: &mut Vec<String>, value: &str) {
    if !list.iter().any(|existing| existing == value) {
        list.push(value.to_string());
    }
}

/// The marks of one copy of the shared sequence. A mark that depends on a name (a property, a
/// callee) is read from the source of THIS copy, because the Structural stream the family was
/// found in collapses those names.
fn read_member(ctx: &Ctx<'_>, member: &CodeSpan, view: &SpanView, lang: SourceLang) -> Reading {
    let tokens = view.tokens();
    let mut reading = Reading::default();
    let source_text = |t: &Token| ctx.token_text(&member.file, t).unwrap_or_default();
    for (i, token) in tokens.iter().enumerate() {
        let next = tokens.get(i + 1);
        let prev = i.checked_sub(1).and_then(|p| tokens.get(p));
        match token.class {
            TokenClass::JsxTag => push_unique(&mut reading.jsx_tags, source_text(token)),
            TokenClass::JsxAttr => {
                let name = source_text(token);
                let handler = name
                    .strip_prefix("on")
                    .and_then(|rest| rest.chars().next())
                    .is_some_and(|c| c.is_ascii_uppercase());
                if handler {
                    push_unique(&mut reading.marks, name);
                }
            }
            TokenClass::Ident => {
                let name = source_text(token);
                // `function useThing(` declares; it does not call.
                let is_call = next.is_some_and(|n| is_punct(n, "("))
                    && !prev.is_some_and(|p| is_keyword(p, "function"));
                if is_call && is_hook_name(name) {
                    push_unique(&mut reading.marks, name);
                    push_unique(&mut reading.hooks, name);
                } else if is_call && (name == "setTimeout" || name == "requestAnimationFrame") {
                    push_unique(&mut reading.marks, name);
                } else if (name == "document" || name == "window")
                    && next.is_some_and(|n| is_punct(n, "."))
                {
                    push_unique(&mut reading.marks, &format!("{name}."));
                }
            }
            TokenClass::Prop => {
                let name = source_text(token);
                if name == "addEventListener" || name == "removeEventListener" {
                    push_unique(&mut reading.marks, name);
                } else if name == "current"
                    && prev.is_some_and(|p| is_punct(p, "."))
                    && i >= 2
                    && tokens.get(i - 2).is_some_and(|owner| {
                        let owner = source_text(owner);
                        owner == "ref" || owner.ends_with("Ref")
                    })
                {
                    push_unique(&mut reading.marks, "ref.current");
                }
            }
            TokenClass::BinOp if source_text(token) == "&&" => {
                push_unique(&mut reading.marks, "conditional");
            }
            TokenClass::Keyword if is_keyword(token, "if") => {
                push_unique(&mut reading.marks, "conditional");
            }
            // A ternary: a `?` with a `:` after it. (`x?: T` is a type; the tokenizer keeps
            // optional chaining as `?.`.)
            TokenClass::Punct
                if lang != SourceLang::Rust
                    && is_punct(token, "?")
                    && tokens[i + 1..].iter().any(|t| is_punct(t, ":"))
                    && next.is_some_and(|n| !is_punct(n, ":")) =>
            {
                push_unique(&mut reading.marks, "conditional");
            }
            _ => {}
        }
    }
    reading
}

/// Build the candidate for a family, or `None` when any deterministic filter rejects it.
pub(super) fn build(ctx: &mut Ctx<'_>, family: &CloneFamily) -> Option<Candidate> {
    if family.token_len < PRIMITIVE_MIN_TOKENS {
        return None;
    }
    // Scope: ECMAScript files in a package that depends on a React-family library. A copy in a
    // package outside that scope is not listed; what remains must still span two files.
    let mut scoped = family.clone();
    scoped.members.retain(|member| {
        matches!(
            ctx.lang(&member.file),
            Some(SourceLang::TypeScript | SourceLang::Tsx | SourceLang::JavaScript)
        ) && ctx.in_react_package(&member.file)
    });
    let mut files: Vec<&str> = scoped.members.iter().map(|m| m.file.as_str()).collect();
    files.dedup();
    scoped.distinct_files = u32::try_from(files.len()).unwrap_or(u32::MAX);
    let family = &scoped;
    if family.distinct_files < 2 || family.members.len() > PRIMITIVE_MAX_MEMBERS {
        return None;
    }
    let first = family.members.first()?;
    if distinct_kinds(ctx.structural_view(first)?.tokens()) < MIN_DISTINCT_KINDS {
        return None;
    }
    let mut readings: Vec<Reading> = Vec::with_capacity(family.members.len());
    let mut composed_everywhere = true;
    for member in &family.members {
        let view = ctx.structural_view(member)?;
        if !span_in_ui_unit(ctx, member, &view) {
            return None;
        }
        if !span_is_a_use_of_an_imported_component(ctx, member, &view) {
            composed_everywhere = false;
        }
        let lang = ctx.lang(&member.file)?;
        readings.push(read_member(ctx, member, &view, lang));
    }
    if composed_everywhere {
        return None;
    }
    // A mechanism must fire in EVERY copy: the marks the copies share.
    let mut marks: Vec<String> = readings.first()?.marks.clone();
    for reading in readings.iter().skip(1) {
        marks.retain(|m| reading.marks.contains(m));
    }
    if marks.is_empty() {
        return None;
    }
    let reading = readings.into_iter().next()?;
    let disagreements = compare_members(ctx, &family.members)?;
    let differing_strings = string_differences(ctx, &family.members)?;
    let mut sites: Vec<Site> = family.members.iter().map(Site::of).collect();
    sites.sort();
    Some(Candidate {
        rule: CloneRule::SharedPrimitive,
        id: super::candidate_id(CloneRule::SharedPrimitive, family),
        family: family.clone(),
        facts: CandidateFacts {
            token_len: family.token_len,
            sites,
            agree: disagreements.is_empty(),
            disagreements,
            rule: RuleFacts::Primitive {
                mechanism_marks: marks,
                jsx_tags: reading.jsx_tags,
                hooks: reading.hooks,
                differing_strings,
            },
        },
        rank: 0,
    })
}

fn mark_count(c: &Candidate) -> usize {
    match &c.facts.rule {
        RuleFacts::Primitive {
            mechanism_marks, ..
        } => mechanism_marks.len(),
        _ => 0,
    }
}

/// The ranking: more places, then more mechanism marks, then longer, then by path (and id, so
/// the order is total).
pub(super) fn compare(a: &Candidate, b: &Candidate) -> Ordering {
    b.member_count()
        .cmp(&a.member_count())
        .then_with(|| mark_count(b).cmp(&mark_count(a)))
        .then_with(|| b.facts.token_len.cmp(&a.facts.token_len))
        .then_with(|| super::single_source::first_path(a).cmp(super::single_source::first_path(b)))
        .then_with(|| a.id.cmp(&b.id))
}
