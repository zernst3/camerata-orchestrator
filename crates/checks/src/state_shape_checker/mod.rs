//! `StateShapeChecker`: the deterministic edge of `UI-STATE-SHAPE-1` ("one status, one
//! variable"). The design, the traps and the safe twins are in
//! `docs/design/2026-10-21_w29-detector-implementation.md` ("Wave D and 22e: the specs",
//! UI-STATE-SHAPE-1).
//!
//! Registered in [`crate::arch_checker::all_checkers`] together with the corpus rule
//! `crates/rules/principles/ui/ui-state-shape-1.toml` (`ui` domain).
//!
//! # What it proves
//!
//! Two shapes inside one React function component or custom hook, each a fact a reader can
//! verify by looking at the lines the finding names:
//!
//! 1. [`flags`]: boolean state variables that are pairwise exclusive by construction: every call
//!    of their setters passes a `true` or `false` literal, and every place that sets one to `true`
//!    sets the others to `false` in the same block. They are one status stored in several
//!    variables.
//! 2. [`derived`]: a state variable whose only writer is an effect that copies a pure expression
//!    of its dependencies into it. The value belongs in the render.
//!
//! # Judgment stays with the semantic pass
//!
//! The rule is wider than these two shapes (a status split across a boolean and a string, a
//! value stored twice in different forms). That part is a judgment, so the checker opts into
//! [`ArchChecker::advisory_coexisting`] and the rule id stays in the semantic prompt, while
//! [`ArchChecker::defects_need_review`] is `false`: the two proven shapes are facts.
//!
//! # The law: false-negative, never false-positive
//!
//! A setter that is passed, aliased, shadowed or called with anything but what the shape needs
//! makes the state unreadable and it is skipped. `useState` and `useEffect` must come from React
//! (or Preact's hooks); test, story, generated, vendored and minified files are never read.
//!
//! # Severity
//!
//! Always [`SEVERITY_MEDIUM`]: the checker contract forbids emitting `low`. The corpus rule
//! declares `max_severity = "low"`, which the report export clamps down to.

mod derived;
mod flags;

#[cfg(test)]
mod tests;

use std::collections::HashSet;

use tree_sitter::{Node, Tree};

use crate::arch_checker::{ArchChecker, ArchViolation, RepoView};
use crate::extract::clone::classify_path;
use crate::extract::ecma_walk::{
    descendants, function_name, import_bindings, is_function_like, named_children,
    namespace_imports, parse_path, text,
};
use crate::extract::paths::emitted_js_paths;
use crate::extract::SourceLang;

pub const RULE_STATE_SHAPE: &str = "UI-STATE-SHAPE-1";

const RULE_IDS: &[&str] = &[RULE_STATE_SHAPE];

const INTEREST_GLOBS: &[&str] = &["**/*.jsx", "**/*.tsx", "**/*.js", "**/*.ts"];

/// Modules whose `useState` and `useEffect` have React's semantics.
const REACT_MODULES: &[&str] = &["react", "preact/hooks", "preact/compat"];

pub struct StateShapeChecker;

impl ArchChecker for StateShapeChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        RULE_IDS
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        let emitted = emitted_js_paths(repo.files);
        let mut out = Vec::new();
        for (path, source) in repo.files {
            let readable = matches!(
                classify_path(path, &emitted),
                Ok(SourceLang::TypeScript | SourceLang::Tsx | SourceLang::JavaScript)
            );
            if !readable {
                continue;
            }
            let Some(tree) = parse_path(path, source) else {
                continue;
            };
            let hooks = ReactHooks::read(&tree, source);
            if hooks.is_empty() {
                continue;
            }
            for component in components(&tree, source, &hooks) {
                out.extend(flags::check(path, source, &component));
                out.extend(derived::check(path, source, &component, &hooks));
            }
        }
        out.sort_by(|a, b| {
            (a.file.as_str(), a.line, a.message.as_str()).cmp(&(
                b.file.as_str(),
                b.line,
                b.message.as_str(),
            ))
        });
        out
    }

    /// The rule's judgment half (any status shape the two proofs do not cover) stays in the
    /// semantic prompt.
    fn advisory_coexisting(&self) -> bool {
        true
    }

    /// The two proven shapes are facts: CI may fail on them.
    fn defects_need_review(&self) -> bool {
        false
    }
}

/// The local names under which a file reaches React's `useState` and `useEffect`.
pub(super) struct ReactHooks {
    use_state: HashSet<String>,
    use_effect: HashSet<String>,
    /// Default and namespace imports of a React module (`React.useState`).
    namespaces: HashSet<String>,
}

impl ReactHooks {
    fn read(tree: &Tree, source: &str) -> Self {
        let mut hooks = Self {
            use_state: HashSet::new(),
            use_effect: HashSet::new(),
            namespaces: HashSet::new(),
        };
        for (local, binding) in import_bindings(tree, source) {
            if !REACT_MODULES.contains(&binding.specifier.as_str()) || binding.type_only {
                continue;
            }
            match binding.origin.as_str() {
                "useState" => {
                    hooks.use_state.insert(local);
                }
                "useEffect" => {
                    hooks.use_effect.insert(local);
                }
                "default" => {
                    hooks.namespaces.insert(local);
                }
                _ => {}
            }
        }
        for (local, specifier) in namespace_imports(tree, source) {
            if REACT_MODULES.contains(&specifier.as_str()) {
                hooks.namespaces.insert(local);
            }
        }
        hooks
    }

    fn is_empty(&self) -> bool {
        self.use_state.is_empty() && self.namespaces.is_empty()
    }

    /// Whether a callee is `hook` (`useState`, or `React.useState`).
    fn is_call_of(&self, callee: Node<'_>, source: &str, hook: &str) -> bool {
        let locals = if hook == "useState" {
            &self.use_state
        } else {
            &self.use_effect
        };
        match callee.kind() {
            "identifier" => locals.contains(text(callee, source)),
            "member_expression" => {
                let object = callee.child_by_field_name("object");
                let property = callee.child_by_field_name("property");
                matches!((object, property), (Some(o), Some(p))
                    if o.kind() == "identifier"
                        && self.namespaces.contains(text(o, source))
                        && text(p, source) == hook)
            }
            _ => false,
        }
    }
}

/// One `const [value, setValue] = useState(initial)` at the top level of a component.
pub(super) struct StateVar<'t> {
    pub(super) value: String,
    pub(super) setter: String,
    /// The setter's binding identifier (its declaration).
    pub(super) setter_node: Node<'t>,
    /// The single argument of `useState`, when it has exactly one.
    pub(super) initial: Option<Node<'t>>,
    pub(super) line: usize,
}

/// A function that declares state at its top level: a component or a custom hook.
pub(super) struct Component<'t> {
    pub(super) node: Node<'t>,
    pub(super) name: String,
    /// The direct statements of its body.
    pub(super) statements: Vec<Node<'t>>,
    pub(super) states: Vec<StateVar<'t>>,
}

fn components<'t>(tree: &'t Tree, source: &str, hooks: &ReactHooks) -> Vec<Component<'t>> {
    descendants(tree.root_node())
        .into_iter()
        .filter(|n| is_function_like(*n))
        .filter_map(|node| {
            let body = node.child_by_field_name("body")?;
            if body.kind() != "statement_block" {
                return None;
            }
            let statements = named_children(body);
            let states: Vec<StateVar<'t>> = statements
                .iter()
                .filter(|s| s.kind() == "lexical_declaration")
                .flat_map(|s| named_children(*s))
                .filter_map(|d| state_var(d, source, hooks))
                .collect();
            if states.is_empty() {
                return None;
            }
            Some(Component {
                node,
                name: function_name(node, source).unwrap_or_default().to_string(),
                statements,
                states,
            })
        })
        .collect()
}

fn state_var<'t>(declarator: Node<'t>, source: &str, hooks: &ReactHooks) -> Option<StateVar<'t>> {
    if declarator.kind() != "variable_declarator" {
        return None;
    }
    let pattern = declarator.child_by_field_name("name")?;
    let call = declarator.child_by_field_name("value")?;
    if pattern.kind() != "array_pattern" || call.kind() != "call_expression" {
        return None;
    }
    if !hooks.is_call_of(call.child_by_field_name("function")?, source, "useState") {
        return None;
    }
    let [value, setter] = named_children(pattern)[..] else {
        return None;
    };
    if value.kind() != "identifier" || setter.kind() != "identifier" {
        return None;
    }
    let args = named_children(call.child_by_field_name("arguments")?);
    Some(StateVar {
        value: text(value, source).to_string(),
        setter: text(setter, source).to_string(),
        setter_node: setter,
        initial: match args[..] {
            [only] => Some(only),
            _ => None,
        },
        line: declarator.start_position().row + 1,
    })
}

/// Every call of a state's setter inside the component, or `None` when the setter is used in any
/// other way (passed, aliased, re-declared): its writers are then unknown.
pub(super) fn setter_calls<'t>(
    component: &Component<'t>,
    state: &StateVar<'t>,
    source: &str,
) -> Option<Vec<Node<'t>>> {
    let mut calls = Vec::new();
    for node in descendants(component.node) {
        if node.kind() != "identifier" || text(node, source) != state.setter {
            continue;
        }
        if node.id() == state.setter_node.id() {
            continue;
        }
        let parent = node.parent()?;
        let is_callee = parent.kind() == "call_expression"
            && parent
                .child_by_field_name("function")
                .is_some_and(|f| f.id() == node.id());
        if !is_callee {
            return None;
        }
        calls.push(parent);
    }
    Some(calls)
}

/// The display name of a component in a message.
pub(super) fn component_label(component: &Component<'_>) -> String {
    if component.name.is_empty() {
        "this component".to_string()
    } else {
        format!("`{}`", component.name)
    }
}

/// `a`, `a and b`, `a, b and c`.
pub(super) fn join_words(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}
