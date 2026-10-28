//! Derived state copied by an effect: `useEffect(() => { setX(expr) }, [deps])` where that call
//! is the effect's whole body, `setX` is written nowhere else, and `expr` is a pure expression of
//! the listed dependencies. React's guidance is to compute such a value during render; the effect
//! costs an extra render in which the copy is stale.
//!
//! "Pure" is read narrowly: names, member reads, literals, operators, conditionals, templates,
//! array and object literals, and calls of a short list of array and string methods whose
//! callbacks obey the same limits. Anything else (another call, `new`, `await`, an assignment,
//! `this`, a `.current` read, a name that is not a dependency) leaves the effect alone, and so do
//! two shapes where the extra render can be the point: a copy named as lagging (`prevValue`), and
//! a plain mirror of a name into state that starts from a different value (a CSS transition that
//! needs one render in its initial state).

use std::collections::HashSet;

use tree_sitter::Node;

use super::{component_label, setter_calls, Component, ReactHooks, RULE_STATE_SHAPE};
use crate::arch_checker::{ArchViolation, ViolationGrade, SEVERITY_MEDIUM};
use crate::extract::ecma_walk::{line, named_children, text};

/// Array and string methods that read their receiver and return a new value.
const PURE_METHODS: &[&str] = &[
    "filter",
    "map",
    "slice",
    "concat",
    "join",
    "includes",
    "some",
    "every",
    "find",
    "findIndex",
    "indexOf",
    "trim",
    "toLowerCase",
    "toUpperCase",
    "startsWith",
    "endsWith",
    "toFixed",
];

/// An expression shown in a message is at most this long; a longer one is left out.
const MAX_SHOWN_EXPR: usize = 80;

pub(super) fn check(
    path: &str,
    source: &str,
    component: &Component<'_>,
    hooks: &ReactHooks,
) -> Vec<ArchViolation> {
    component
        .statements
        .iter()
        .filter_map(|statement| derived_effect(path, source, component, hooks, *statement))
        .collect()
}

fn derived_effect(
    path: &str,
    source: &str,
    component: &Component<'_>,
    hooks: &ReactHooks,
    statement: Node<'_>,
) -> Option<ArchViolation> {
    if statement.kind() != "expression_statement" {
        return None;
    }
    let effect = named_children(statement).into_iter().next()?;
    if effect.kind() != "call_expression"
        || !hooks.is_call_of(effect.child_by_field_name("function")?, source, "useEffect")
    {
        return None;
    }
    let [callback, deps] = named_children(effect.child_by_field_name("arguments")?)[..] else {
        return None;
    };
    if deps.kind() != "array" || !takes_no_parameters(callback, source) {
        return None;
    }
    let call = sole_call(callback)?;
    let setter_name = text(call.child_by_field_name("function")?, source);
    let state = component.states.iter().find(|s| s.setter == setter_name)?;
    // The effect is the setter's only writer.
    let calls = setter_calls(component, state, source)?;
    if calls.len() != 1 || calls[0].id() != call.id() {
        return None;
    }
    let [expr] = named_children(call.child_by_field_name("arguments")?)[..] else {
        return None;
    };
    let dependencies = Dependencies::read(deps, source)?;
    if !pure(expr, source, &dependencies, &HashSet::new())
        || deliberately_lagging(&state.value)
        || transition_mirror(expr, state.initial, source)
    {
        return None;
    }

    let shown = text(expr, source);
    let shown = if shown.len() <= MAX_SHOWN_EXPR && !shown.contains('\n') {
        format!(" `{shown}`")
    } else {
        String::new()
    };
    let message = format!(
        "`{value}` (state in {component}) is only ever written by the effect on line {line}, \
         which copies{shown} into it whenever {deps} change. A value computed from props or state \
         belongs in the render: the effect adds a second render in which `{value}` still holds \
         the old value, and the state can drift from its inputs if another writer is added. \
         Remove the state and the effect and compute `{value}` during render (with `useMemo` if \
         the computation is expensive).",
        value = state.value,
        component = component_label(component),
        line = line(effect),
        deps = dependencies.describe(),
    );
    Some(ArchViolation {
        grade: ViolationGrade::Defect,
        rule_id: RULE_STATE_SHAPE.to_string(),
        file: path.to_string(),
        line: line(effect),
        object: Some(state.value.clone()),
        message,
        // The corpus rule's `max_severity = "low"` clamps this at report time; the checker
        // contract forbids emitting `low` directly.
        severity: SEVERITY_MEDIUM,
    })
}

/// A name that says the copy is meant to lag one render behind (`prevValue`, `lastQuery`): the
/// stale render the rule warns about may be the point.
fn deliberately_lagging(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    ["prev", "previous", "last", "old"]
        .iter()
        .any(|p| lower.starts_with(p))
}

/// A plain mirror of one name or property (`setVisible(open)`) into state that starts from a
/// different value: the first render with the old value can be deliberate (a CSS transition
/// that needs one render in its initial state).
fn transition_mirror(expr: Node<'_>, initial: Option<Node<'_>>, source: &str) -> bool {
    let plain = matches!(expr.kind(), "identifier" | "member_expression");
    plain && initial.is_none_or(|i| text(i, source) != text(expr, source))
}

/// An arrow or function expression with an empty parameter list.
fn takes_no_parameters(callback: Node<'_>, source: &str) -> bool {
    if !matches!(callback.kind(), "arrow_function" | "function_expression") {
        return false;
    }
    callback
        .child_by_field_name("parameters")
        .is_some_and(|p| named_children(p).is_empty() && text(p, source).starts_with('('))
}

/// The one call that is the callback's whole body: an expression body, or a block holding one
/// expression statement and nothing else.
fn sole_call(callback: Node<'_>) -> Option<Node<'_>> {
    let body = callback.child_by_field_name("body")?;
    let expression = if body.kind() == "statement_block" {
        let [statement] = named_children(body)[..] else {
            return None;
        };
        if statement.kind() != "expression_statement" {
            return None;
        }
        let [inner] = named_children(statement)[..] else {
            return None;
        };
        inner
    } else {
        body
    };
    (expression.kind() == "call_expression"
        && expression
            .child_by_field_name("function")
            .is_some_and(|f| f.kind() == "identifier"))
    .then_some(expression)
}

/// The dependency array: plain names and member paths only.
struct Dependencies {
    names: HashSet<String>,
    members: HashSet<String>,
    shown: Vec<String>,
}

impl Dependencies {
    fn read(array: Node<'_>, source: &str) -> Option<Self> {
        let items = named_children(array);
        if items.is_empty() {
            return None;
        }
        let mut deps = Self {
            names: HashSet::new(),
            members: HashSet::new(),
            shown: Vec::new(),
        };
        for item in items {
            let raw = text(item, source).to_string();
            match item.kind() {
                "identifier" => {
                    deps.names.insert(raw.clone());
                }
                "member_expression" if is_member_path(item) => {
                    deps.members.insert(raw.clone());
                }
                _ => return None,
            }
            deps.shown.push(format!("`{raw}`"));
        }
        Some(deps)
    }

    fn describe(&self) -> String {
        super::join_words(&self.shown)
    }
}

/// `a.b.c`: a chain of property reads ending in a name.
fn is_member_path(node: Node<'_>) -> bool {
    let mut current = node;
    loop {
        match current.kind() {
            "identifier" => return true,
            "member_expression" => {
                let Some(object) = current.child_by_field_name("object") else {
                    return false;
                };
                current = object;
            }
            _ => return false,
        }
    }
}

/// Whether `node` is a pure expression of the dependencies (module doc). `bound` holds the
/// parameters of an enclosing pure-method callback.
fn pure(node: Node<'_>, source: &str, deps: &Dependencies, bound: &HashSet<String>) -> bool {
    match node.kind() {
        "identifier" | "shorthand_property_identifier" => {
            let name = text(node, source);
            deps.names.contains(name) || bound.contains(name)
        }
        "member_expression" => {
            if deps.members.contains(text(node, source)) {
                return true;
            }
            let property = node.child_by_field_name("property");
            if property.is_some_and(|p| text(p, source) == "current") {
                return false;
            }
            node.child_by_field_name("object")
                .is_some_and(|o| pure(o, source, deps, bound))
        }
        "call_expression" => pure_method_call(node, source, deps, bound),
        "string" | "string_fragment" | "escape_sequence" | "number" | "true" | "false" | "null"
        | "undefined" | "comment" => true,
        "binary_expression"
        | "ternary_expression"
        | "parenthesized_expression"
        | "template_string"
        | "template_substitution"
        | "array"
        | "subscript_expression"
        | "spread_element" => all_children_pure(node, source, deps, bound),
        "unary_expression" => {
            let operator = node
                .child_by_field_name("operator")
                .map_or("", |o| text(o, source));
            operator != "delete" && all_children_pure(node, source, deps, bound)
        }
        "object" => named_children(node)
            .into_iter()
            .all(|member| match member.kind() {
                "pair" => member
                    .child_by_field_name("value")
                    .is_some_and(|v| pure(v, source, deps, bound)),
                _ => pure(member, source, deps, bound),
            }),
        _ => false,
    }
}

fn all_children_pure(
    node: Node<'_>,
    source: &str,
    deps: &Dependencies,
    bound: &HashSet<String>,
) -> bool {
    named_children(node)
        .into_iter()
        .all(|child| pure(child, source, deps, bound))
}

/// `receiver.filter(t => t.done)`: an allowed method on a pure receiver, with pure arguments.
fn pure_method_call(
    node: Node<'_>,
    source: &str,
    deps: &Dependencies,
    bound: &HashSet<String>,
) -> bool {
    let Some(callee) = node.child_by_field_name("function") else {
        return false;
    };
    if callee.kind() != "member_expression" {
        return false;
    }
    let (Some(object), Some(property)) = (
        callee.child_by_field_name("object"),
        callee.child_by_field_name("property"),
    ) else {
        return false;
    };
    if !PURE_METHODS.contains(&text(property, source)) || !pure(object, source, deps, bound) {
        return false;
    }
    let Some(arguments) = node.child_by_field_name("arguments") else {
        return false;
    };
    named_children(arguments).into_iter().all(|arg| {
        if arg.kind() == "arrow_function" {
            pure_callback(arg, source, deps, bound)
        } else {
            pure(arg, source, deps, bound)
        }
    })
}

/// An expression-bodied arrow whose parameters are plain names.
fn pure_callback(
    arrow: Node<'_>,
    source: &str,
    deps: &Dependencies,
    bound: &HashSet<String>,
) -> bool {
    let mut inner = bound.clone();
    if let Some(single) = arrow.child_by_field_name("parameter") {
        if single.kind() != "identifier" {
            return false;
        }
        inner.insert(text(single, source).to_string());
    } else if let Some(params) = arrow.child_by_field_name("parameters") {
        for param in named_children(params) {
            // `(t)` in JavaScript, `(t)` / `(t: Todo)` in TypeScript.
            let name = match param.kind() {
                "identifier" => Some(param),
                "required_parameter" => param
                    .child_by_field_name("pattern")
                    .filter(|p| p.kind() == "identifier"),
                _ => None,
            };
            let Some(name) = name else {
                return false;
            };
            inner.insert(text(name, source).to_string());
        }
    } else {
        return false;
    }
    arrow
        .child_by_field_name("body")
        .is_some_and(|body| body.kind() != "statement_block" && pure(body, source, deps, &inner))
}
