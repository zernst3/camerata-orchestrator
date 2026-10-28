//! Exclusive boolean flags: several `useState` booleans of one component where every place that
//! sets one to `true` sets the others to `false` in the same block. Together they store one
//! status in several variables.
//!
//! A flag qualifies only when every call of its setter passes a single `true` or `false`
//! literal; a pair qualifies only when each `true` call of either sits in a statement list that
//! also sets the other to `false` (and never to `true`), when each of the two is turned on
//! somewhere, and when they do not both start `true`. A `false` call may sit anywhere: it cannot
//! make two flags true at once.

use std::collections::BTreeSet;

use tree_sitter::Node;

use super::{component_label, join_words, setter_calls, Component, RULE_STATE_SHAPE};
use crate::arch_checker::{ArchViolation, ViolationGrade, SEVERITY_MEDIUM};
use crate::extract::ecma_walk::{line, named_children};

/// One boolean flag and the literal calls of its setter.
struct Flag {
    value: String,
    line: usize,
    initial: bool,
    /// `(value set, the statement list holding the call when it is a statement of one)`.
    calls: Vec<(bool, Option<usize>)>,
    /// Lines of the calls that set it to `true`.
    on_lines: Vec<usize>,
}

pub(super) fn check(path: &str, source: &str, component: &Component<'_>) -> Vec<ArchViolation> {
    let flags: Vec<Flag> = component
        .states
        .iter()
        .filter_map(|state| {
            let initial = bool_literal(state.initial?)?;
            let calls = setter_calls(component, state, source)?;
            let mut read = Vec::new();
            let mut on_lines = Vec::new();
            for call in calls {
                let args = named_children(call.child_by_field_name("arguments")?);
                let [arg] = args[..] else {
                    return None;
                };
                let value = bool_literal(arg)?;
                if value {
                    on_lines.push(line(call));
                }
                read.push((value, statement_list(call)));
            }
            Some(Flag {
                value: state.value.clone(),
                line: state.line,
                initial,
                calls: read,
                on_lines,
            })
        })
        .collect();

    // Greedy maximal groups of pairwise exclusive flags, in declaration order.
    let mut used: BTreeSet<usize> = BTreeSet::new();
    let mut out = Vec::new();
    for i in 0..flags.len() {
        if used.contains(&i) {
            continue;
        }
        let mut group = vec![i];
        for j in (i + 1)..flags.len() {
            if !used.contains(&j) && group.iter().all(|&k| exclusive(&flags[k], &flags[j])) {
                group.push(j);
            }
        }
        if group.len() < 2 {
            continue;
        }
        used.extend(group.iter().copied());
        let members: Vec<&Flag> = group.iter().map(|&k| &flags[k]).collect();
        out.push(violation(path, component, &members));
    }
    out
}

/// `true` or `false` written as a literal.
fn bool_literal(node: Node<'_>) -> Option<bool> {
    match node.kind() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// The statement list a call is a direct statement of (a block, a `case`, the program), or the
/// arrow whose expression body it is. Identified by the list node's start byte.
fn statement_list(call: Node<'_>) -> Option<usize> {
    let parent = call.parent()?;
    if parent.kind() == "arrow_function" {
        let is_body = parent
            .child_by_field_name("body")
            .is_some_and(|b| b.id() == call.id());
        return is_body.then(|| parent.start_byte());
    }
    if parent.kind() != "expression_statement" {
        return None;
    }
    let list = parent.parent()?;
    matches!(
        list.kind(),
        "statement_block" | "switch_case" | "switch_default" | "program"
    )
    .then(|| list.start_byte())
}

/// Whether two flags are exclusive by construction (see the module doc).
fn exclusive(a: &Flag, b: &Flag) -> bool {
    if a.initial && b.initial {
        return false;
    }
    let turned_on = |x: &Flag| x.calls.iter().any(|(v, _)| *v);
    turned_on(a) && turned_on(b) && paired(a, b) && paired(b, a)
}

/// Every `true` call of `on` is a statement of a list that sets `off` to `false` and never to
/// `true`.
fn paired(on: &Flag, off: &Flag) -> bool {
    on.calls.iter().filter(|(v, _)| *v).all(|(_, list)| {
        let Some(list) = list else {
            return false;
        };
        let in_list = |want: bool| {
            off.calls
                .iter()
                .any(|(v, l)| *v == want && *l == Some(*list))
        };
        in_list(false) && !in_list(true)
    })
}

fn violation(path: &str, component: &Component<'_>, members: &[&Flag]) -> ArchViolation {
    let names: Vec<String> = members.iter().map(|f| format!("`{}`", f.value)).collect();
    let mut on_lines: Vec<usize> = members
        .iter()
        .flat_map(|f| f.on_lines.iter().copied())
        .collect();
    on_lines.sort_unstable();
    on_lines.dedup();
    let on_lines: Vec<String> = on_lines.iter().map(usize::to_string).collect();
    let declared: Vec<String> = members.iter().map(|f| f.line.to_string()).collect();
    let first = members.first().map_or(0, |f| f.line);
    let message = format!(
        "{} (boolean state in {}, declared on lines {}) are written as if they were one status \
         stored in {} variables: every place that sets one of them to `true` sets the others to \
         `false` in the same block (lines {}). Two flags that must never be true together can still be set that way \
         by the next change that forgets one setter, and every reader has to know the rule that \
         links them. Keep one state variable for the status, for example a union such as \
         `'idle' | 'loading' | 'error'`, and derive each boolean from it during render.",
        join_words(&names),
        component_label(component),
        join_words(&declared),
        members.len(),
        join_words(&on_lines),
    );
    ArchViolation {
        grade: ViolationGrade::Defect,
        rule_id: RULE_STATE_SHAPE.to_string(),
        file: path.to_string(),
        line: first,
        object: Some(
            members
                .iter()
                .map(|f| f.value.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        ),
        message,
        // The corpus rule's `max_severity = "low"` clamps this at report time; the checker
        // contract forbids emitting `low` directly.
        severity: SEVERITY_MEDIUM,
    }
}
