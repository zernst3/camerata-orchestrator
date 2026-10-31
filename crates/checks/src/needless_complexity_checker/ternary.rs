//! Negated ternaries: `!x ? a : b`, which reads as a double negative and is `x ? b : a` with
//! its branches swapped.
//!
//! Only the plainest shape is reported: a one-line conditional whose test is `!` applied to a
//! name, a member access, a subscript or a call; whose branches are not conditionals and not
//! both boolean literals; and which is not itself a branch of a conditional chain. A comparison
//! (`a !== b ? ...`), a double negation, a chain, a multi-line guard-first conditional and a
//! TypeScript non-null assertion (`x! ? ...`) are never reported.

use tree_sitter::{Node, Tree};

use super::RULE_NEEDLESS_COMPLEXITY;
use crate::arch_checker::{ArchViolation, ViolationGrade, SEVERITY_MEDIUM};
use crate::extract::ecma_walk::{descendants, line, text};

/// A conditional shown in a message is at most this long; a longer one is described instead.
const MAX_SHOWN: usize = 100;

pub(super) fn check(path: &str, source: &str, tree: &Tree) -> Vec<ArchViolation> {
    descendants(tree.root_node())
        .into_iter()
        .filter(|n| n.kind() == "ternary_expression")
        .filter_map(|n| negated(path, source, n))
        .collect()
}

fn negated(path: &str, source: &str, node: Node<'_>) -> Option<ArchViolation> {
    let condition = node.child_by_field_name("condition")?;
    let consequence = node.child_by_field_name("consequence")?;
    let alternative = node.child_by_field_name("alternative")?;
    if condition.kind() != "unary_expression"
        || condition
            .child_by_field_name("operator")
            .is_none_or(|o| text(o, source) != "!")
    {
        return None;
    }
    let operand = condition.child_by_field_name("argument")?;
    if !matches!(
        operand.kind(),
        "identifier" | "member_expression" | "subscript_expression" | "call_expression"
    ) {
        return None;
    }
    if [consequence, alternative]
        .iter()
        .any(|b| unparenthesized(*b).kind() == "ternary_expression")
        || (is_bool(consequence) && is_bool(alternative))
        || in_chain(node)
        || node.start_position().row != node.end_position().row
    {
        return None;
    }

    let whole = text(node, source);
    let swapped = format!(
        "{} ? {} : {}",
        text(operand, source),
        text(alternative, source),
        text(consequence, source)
    );
    let example = if whole.len() <= MAX_SHOWN && swapped.len() <= MAX_SHOWN {
        format!("`{whole}` is `{swapped}` written backwards")
    } else {
        format!(
            "This conditional on `!{}` is one on `{}` written backwards",
            text(operand, source),
            text(operand, source)
        )
    };
    let message = format!(
        "{example}: the condition is negated only to put the branches in the other order. A \
         reader has to undo the `!` to see which branch runs when the condition holds. Drop the \
         `!` and swap the two branches."
    );
    Some(ArchViolation {
        grade: ViolationGrade::Defect,
        rule_id: RULE_NEEDLESS_COMPLEXITY.to_string(),
        file: path.to_string(),
        line: line(node),
        object: Some(text(operand, source).to_string()),
        message,
        // The corpus rule's `max_severity = "low"` clamps this at report time; the checker
        // contract forbids emitting `low` directly.
        severity: SEVERITY_MEDIUM,
    })
}

/// The expression inside any number of parentheses.
fn unparenthesized(node: Node<'_>) -> Node<'_> {
    let mut current = node;
    while current.kind() == "parenthesized_expression" {
        match current.named_child(0) {
            Some(inner) => current = inner,
            None => break,
        }
    }
    current
}

fn is_bool(node: Node<'_>) -> bool {
    matches!(node.kind(), "true" | "false")
}

/// Whether the conditional is a branch of another conditional (through parentheses).
fn in_chain(node: Node<'_>) -> bool {
    let mut current = node;
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "parenthesized_expression" => current = parent,
            "ternary_expression" => return true,
            _ => return false,
        }
    }
    false
}
