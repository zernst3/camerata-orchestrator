//! `EffectCleanupChecker`: the deterministic edge of `JAVASCRIPT-REACT-EFFECT-CLEANUP-1` ("an
//! effect that starts something returns the function that stops it").
//!
//! Registered in [`crate::arch_checker::all_checkers`] together with the corpus rule
//! `crates/rules/principles/javascript/react/javascript-react-effect-cleanup-1.toml`.
//!
//! # What it proves
//!
//! A `useEffect` or `useLayoutEffect` (imported from React or Preact's hooks) whose callback is a
//! synchronous function with a block body that, in its own body, starts long-lived work and has
//! no `return` that hands React a value. Long-lived work is one of:
//!
//! - `setInterval(...)` (also `window.setInterval`, `globalThis.setInterval`);
//! - `<target>.addEventListener(...)`;
//! - `<source>.subscribe(...)` (a Supabase realtime channel, an RxJS observable, a store);
//! - the auth and snapshot listeners `onAuthStateChange`, `onAuthStateChanged`,
//!   `onIdTokenChanged` and `onSnapshot`, which return their own unsubscribe handle;
//! - `new WebSocket(...)` and `new EventSource(...)`.
//!
//! React runs the effect again on every dependency change and twice on mount in development, and
//! it can only stop what the effect started by calling the function the effect returns. With no
//! return, every run adds another interval, listener or connection that outlives the component.
//!
//! # The law: false-negative, never false-positive
//!
//! Each of these stays quiet, because the tree alone cannot show the work leaks:
//!
//! - any `return <value>` in the callback's own body (a cleanup function, a returned unsubscribe
//!   handle, anything): the checker does not judge whether the cleanup is complete;
//! - an expression-bodied arrow (`useEffect(() => store.subscribe(fn))`), which returns the
//!   subscription's own result;
//! - an `async` callback (it returns a promise, a different fault the semantic read owns);
//! - work started only inside a nested function (a `.then` callback, a handler);
//! - the matching stop call (`clearInterval`, `removeEventListener`, `unsubscribe`,
//!   `removeChannel`, `close`) anywhere in the callback, nested functions included;
//! - a handle stored on a member (`timer.current = setInterval(...)`), which another effect may
//!   clear;
//! - `addEventListener` with `{ once: true }`, or on an object the callback itself created with
//!   `new` or `document.createElement`;
//! - `setTimeout`: a one-shot timer is often harmless, so it is left to the semantic read;
//! - a `useEffect` that does not come from React, and test, story, generated, vendored and
//!   minified files.
//!
//! # Judgment stays with the semantic pass
//!
//! The rule is wider than these shapes (a one-shot timeout that sets state after unmount, an
//! observer, a third-party widget, a fetch with no abort). So the checker opts into
//! [`ArchChecker::advisory_coexisting`] and the rule id stays in the semantic prompt, while
//! [`ArchChecker::defects_need_review`] is `false`: the proven shape is a fact.
//!
//! # Severity
//!
//! Always [`SEVERITY_MEDIUM`], matching the corpus rule's `min_severity`/`max_severity`.

#[cfg(test)]
mod tests;

use std::collections::HashSet;

use tree_sitter::{Node, Tree};

use crate::arch_checker::{ArchChecker, ArchViolation, RepoView, ViolationGrade, SEVERITY_MEDIUM};
use crate::extract::clone::classify_path;
use crate::extract::ecma_walk::{
    function_name, import_bindings, is_function_like, line, named_children, namespace_imports,
    parse_path, text,
};
use crate::extract::paths::emitted_js_paths;
use crate::extract::SourceLang;

pub const RULE_EFFECT_CLEANUP: &str = "JAVASCRIPT-REACT-EFFECT-CLEANUP-1";

const RULE_IDS: &[&str] = &[RULE_EFFECT_CLEANUP];

const INTEREST_GLOBS: &[&str] = &[
    "**/*.jsx", "**/*.tsx", "**/*.js", "**/*.ts", "**/*.mjs", "**/*.mts",
];

/// Modules whose `useEffect` and `useLayoutEffect` have React's cleanup contract.
const REACT_MODULES: &[&str] = &["react", "preact/hooks", "preact/compat"];

/// The effect hooks whose callback may return a cleanup function.
const EFFECT_HOOKS: &[&str] = &["useEffect", "useLayoutEffect"];

/// Method names that register a listener or subscription and return or require a stop.
const SUBSCRIBE_METHODS: &[&str] = &[
    "subscribe",
    "onAuthStateChange",
    "onAuthStateChanged",
    "onIdTokenChanged",
    "onSnapshot",
];

/// Constructors that open a connection the effect must close.
const CONNECTION_CONSTRUCTORS: &[&str] = &["WebSocket", "EventSource"];

/// Calls that stop long-lived work. Seeing one anywhere in the callback means the author handled
/// the lifetime some other way, so the effect is not reported.
const STOP_CALLS: &[&str] = &[
    "clearInterval",
    "removeEventListener",
    "unsubscribe",
    "removeChannel",
    "removeAllChannels",
    "close",
    "abort",
];

pub struct EffectCleanupChecker;

impl ArchChecker for EffectCleanupChecker {
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
            // The text test is a cheap pre-filter; the import check below is the real one.
            if !readable || !(source.contains("useEffect") || source.contains("useLayoutEffect")) {
                continue;
            }
            let Some(tree) = parse_path(path, source) else {
                continue;
            };
            let hooks = EffectHooks::read(&tree, source);
            if hooks.is_empty() {
                continue;
            }
            out.extend(violations_in_tree(path, source, &tree, &hooks));
        }
        out.sort_by(|a, b| (a.file.as_str(), a.line).cmp(&(b.file.as_str(), b.line)));
        out
    }

    /// The rule's judgment half (one-shot timers, observers, widgets, fetches) stays in the
    /// semantic prompt.
    fn advisory_coexisting(&self) -> bool {
        true
    }

    /// The proven shape is a fact: CI may fail on it.
    fn defects_need_review(&self) -> bool {
        false
    }
}

/// The local names under which a file reaches React's effect hooks.
struct EffectHooks {
    locals: HashSet<String>,
    /// Default and namespace imports of a React module (`React.useEffect`).
    namespaces: HashSet<String>,
}

impl EffectHooks {
    fn read(tree: &Tree, source: &str) -> Self {
        let mut hooks = Self {
            locals: HashSet::new(),
            namespaces: HashSet::new(),
        };
        for (local, binding) in import_bindings(tree, source) {
            if !REACT_MODULES.contains(&binding.specifier.as_str()) || binding.type_only {
                continue;
            }
            if EFFECT_HOOKS.contains(&binding.origin.as_str()) {
                hooks.locals.insert(local);
            } else if binding.origin == "default" {
                hooks.namespaces.insert(local);
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
        self.locals.is_empty() && self.namespaces.is_empty()
    }

    /// Whether `callee` is an effect hook (`useEffect`, an alias of it, or `React.useEffect`).
    fn is_effect(&self, callee: Node<'_>, source: &str) -> bool {
        match callee.kind() {
            "identifier" => self.locals.contains(text(callee, source)),
            "member_expression" => {
                let object = callee.child_by_field_name("object");
                let property = callee.child_by_field_name("property");
                matches!((object, property), (Some(o), Some(p))
                    if o.kind() == "identifier"
                        && self.namespaces.contains(text(o, source))
                        && EFFECT_HOOKS.contains(&text(p, source)))
            }
            _ => false,
        }
    }
}

fn violations_in_tree(
    path: &str,
    source: &str,
    tree: &Tree,
    hooks: &EffectHooks,
) -> Vec<ArchViolation> {
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
        if node.kind() != "call_expression" {
            continue;
        }
        let Some(callee) = node.child_by_field_name("function") else {
            continue;
        };
        if !hooks.is_effect(callee, source) {
            continue;
        }
        if let Some(v) = check_effect(path, source, node) {
            out.push(v);
        }
    }
    out
}

/// One long-lived start found in an effect body.
struct Start {
    name: String,
    line: usize,
}

fn check_effect(path: &str, source: &str, call: Node<'_>) -> Option<ArchViolation> {
    let args = named_children(call.child_by_field_name("arguments")?);
    let callback = *args.first()?;
    if !matches!(
        callback.kind(),
        "arrow_function" | "function_expression" | "function"
    ) {
        return None;
    }
    if is_async(callback) {
        return None;
    }
    let body = callback.child_by_field_name("body")?;
    if body.kind() != "statement_block" {
        return None;
    }

    // Anything that stops work, anywhere in the callback, means the lifetime is handled.
    if subtree(body).any(|n| is_stop_call(n, source)) {
        return None;
    }

    let mut starts = Vec::new();
    let mut created_locally: HashSet<String> = HashSet::new();
    for node in own_body(body) {
        match node.kind() {
            "return_statement" if named_children(node).iter().any(|c| !is_nothing(*c, source)) => {
                return None;
            }
            "variable_declarator" => {
                if let (Some(name), Some(value)) = (
                    node.child_by_field_name("name"),
                    node.child_by_field_name("value"),
                ) {
                    if name.kind() == "identifier" && is_local_object(value, source) {
                        created_locally.insert(text(name, source).to_string());
                    }
                }
            }
            "call_expression" => {
                if let Some(start) = start_call(node, source, &created_locally) {
                    starts.push(start);
                }
            }
            "new_expression" => {
                let ctor = node.child_by_field_name("constructor");
                if let Some(c) = ctor.filter(|c| c.kind() == "identifier") {
                    let name = text(c, source);
                    if CONNECTION_CONSTRUCTORS.contains(&name) && !stored_on_member(node) {
                        starts.push(Start {
                            name: name.to_string(),
                            line: line(node),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    let first = starts.into_iter().min_by_key(|s| s.line)?;
    let owner = enclosing_function_name(call, source)
        .map(|n| format!("`{n}`"))
        .unwrap_or_else(|| "this module".to_string());
    Some(ArchViolation {
        grade: ViolationGrade::Defect,
        rule_id: RULE_EFFECT_CLEANUP.to_string(),
        file: path.to_string(),
        line: line(call),
        object: Some(first.name.clone()),
        message: format!(
            "The effect at line {effect_line} in {owner} starts `{name}` (line {start_line}) and \
             returns no cleanup function, so React cannot stop it: every re-run of the effect and \
             every remount adds another one, and it keeps running after the component unmounts. \
             Return a function that stops it (for example `clearInterval`, \
             `removeEventListener`, `unsubscribe` or `removeChannel`).",
            effect_line = line(call),
            name = first.name,
            start_line = first.line,
        ),
        severity: SEVERITY_MEDIUM,
    })
}

/// Every node under `node`, itself included (nested functions too).
fn subtree(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let mut stack = vec![node];
    std::iter::from_fn(move || {
        let n = stack.pop()?;
        let mut cursor = n.walk();
        let children: Vec<Node<'_>> = n.children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
        Some(n)
    })
}

/// The nodes of a function body that run when the function itself runs: every descendant except
/// the insides of nested functions (and of nested classes, whose methods are functions).
fn own_body(body: Node<'_>) -> Vec<Node<'_>> {
    let mut out = Vec::new();
    let mut stack = vec![body];
    while let Some(n) = stack.pop() {
        out.push(n);
        if n.id() != body.id() && (is_function_like(n) || n.kind() == "class_body") {
            continue;
        }
        let mut cursor = n.walk();
        let children: Vec<Node<'_>> = n.children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    out
}

/// `async () => {}` and `async function () {}` both open with an anonymous `async` token.
fn is_async(function: Node<'_>) -> bool {
    function.child(0).is_some_and(|c| c.kind() == "async")
}

/// A return argument that hands React nothing to call: `undefined`, `null`, `void 0`, `false`.
fn is_nothing(node: Node<'_>, source: &str) -> bool {
    match node.kind() {
        "undefined" | "null" | "false" | "true" | "number" | "string" => true,
        "identifier" => text(node, source) == "undefined",
        "unary_expression" => text(node, source).starts_with("void"),
        _ => false,
    }
}

/// The method or function name a call invokes (`clearInterval`, `x.y.unsubscribe` -> `unsubscribe`).
fn called_name<'s>(call: Node<'_>, source: &'s str) -> Option<&'s str> {
    let callee = call.child_by_field_name("function")?;
    match callee.kind() {
        "identifier" => Some(text(callee, source)),
        "member_expression" => callee
            .child_by_field_name("property")
            .map(|p| text(p, source)),
        _ => None,
    }
}

fn is_stop_call(node: Node<'_>, source: &str) -> bool {
    node.kind() == "call_expression"
        && called_name(node, source).is_some_and(|name| STOP_CALLS.contains(&name))
}

/// A value an effect creates for itself and nobody else holds: `new Image()`,
/// `document.createElement('script')`. Listeners on it go away with it.
fn is_local_object(value: Node<'_>, source: &str) -> bool {
    match value.kind() {
        "new_expression" => !value
            .child_by_field_name("constructor")
            .is_some_and(|c| CONNECTION_CONSTRUCTORS.contains(&text(c, source))),
        "call_expression" => value
            .child_by_field_name("function")
            .is_some_and(|f| text(f, source) == "document.createElement"),
        _ => false,
    }
}

/// Whether a call's or constructor's result is assigned to a member (`ref.current = ...`), which
/// another effect or handler may stop.
fn stored_on_member(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    parent.kind() == "assignment_expression"
        && parent
            .child_by_field_name("left")
            .is_some_and(|l| l.kind() == "member_expression")
        && parent
            .child_by_field_name("right")
            .is_some_and(|r| r.id() == node.id())
}

fn start_call(call: Node<'_>, source: &str, created_locally: &HashSet<String>) -> Option<Start> {
    let callee = call.child_by_field_name("function")?;
    let name = called_name(call, source)?;
    let starts = match name {
        "setInterval" => match callee.kind() {
            "identifier" => true,
            "member_expression" => callee
                .child_by_field_name("object")
                .is_some_and(|o| matches!(text(o, source), "window" | "globalThis" | "self")),
            _ => false,
        },
        "addEventListener" => {
            let receiver = callee.child_by_field_name("object");
            let local = receiver.is_some_and(|r| {
                r.kind() == "identifier" && created_locally.contains(text(r, source))
            });
            !local && !has_once_option(call, source)
        }
        _ => callee.kind() == "member_expression" && SUBSCRIBE_METHODS.contains(&name),
    };
    if !starts || stored_on_member(call) {
        return None;
    }
    Some(Start {
        name: name.to_string(),
        line: line(call),
    })
}

/// `addEventListener(type, fn, { once: true })`.
fn has_once_option(call: Node<'_>, source: &str) -> bool {
    let Some(args) = call.child_by_field_name("arguments") else {
        return false;
    };
    named_children(args).get(2).is_some_and(|options| {
        options.kind() == "object"
            && named_children(*options).iter().any(|pair| {
                pair.kind() == "pair"
                    && pair
                        .child_by_field_name("key")
                        .is_some_and(|k| text(k, source) == "once")
                    && pair
                        .child_by_field_name("value")
                        .is_some_and(|v| text(v, source) == "true")
            })
    })
}

/// The component or hook the effect is declared in.
fn enclosing_function_name<'s>(node: Node<'_>, source: &'s str) -> Option<&'s str> {
    let mut current = node.parent();
    while let Some(n) = current {
        if is_function_like(n) {
            if let Some(name) = function_name(n, source) {
                return Some(name);
            }
        }
        current = n.parent();
    }
    None
}
