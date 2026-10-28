//! JavaScript and TypeScript tests (Jest, Vitest, Mocha, Jasmine, `node:test`, Bun): a call to
//! `it`, `test` or `specify` (or their `.only` / `.concurrent` forms) in a test file, whose first
//! argument is a string or template and whose last argument is a function with no parameters.
//!
//! A test is skipped, never reported, when its callback takes a parameter (AVA's `t`, Mocha's
//! `done`, Playwright fixtures can all assert), when it sits in a skipped `describe`, when `it` or
//! `test` is declared in the file or imported from anywhere but a test framework, and when any
//! shared hook (`beforeEach`, `afterEach`, `beforeAll`, `afterAll`, `before`, `after`) asserts.

use std::collections::{HashMap, HashSet};

use tree_sitter::{Node, Tree};

use super::words::{is_support_module, message, names_an_assertion, HELPER_DEPTH};
use super::Finding;
use crate::extract::ecma_walk::{
    descendants, import_bindings, is_binding_position, is_function_like, line, named_children,
    namespace_imports, parse_path, text,
};

/// Test frameworks whose `it` / `test` declare a test.
const TEST_MODULES: &[&str] = &[
    "vitest",
    "@jest/globals",
    "node:test",
    "bun:test",
    "mocha",
    "@playwright/test",
];

/// Modules whose every export is an assertion function or object.
const ASSERTION_MODULES: &[&str] = &[
    "assert",
    "assert/strict",
    "node:assert",
    "node:assert/strict",
    "chai",
    "expect",
    "should",
    "power-assert",
    "uvu/assert",
];

/// Names that declare a test.
const TEST_NAMES: &[&str] = &["it", "test", "specify"];

/// Modifiers of a test declaration that still run the test.
const RUNNING_MODIFIERS: &[&str] = &["only", "concurrent"];

/// Hooks whose assertions apply to every test of the file.
const HOOKS: &[&str] = &[
    "beforeEach",
    "afterEach",
    "beforeAll",
    "afterAll",
    "before",
    "after",
];

/// Suites whose tests do not run.
const SKIPPED_SUITES: &[&str] = &[
    "xdescribe",
    "describe.skip",
    "describe.todo",
    "context.skip",
    "suite.skip",
    "xcontext",
];

/// Calls that check something without an assertion word in their name: Testing Library's
/// `waitFor` (retries until its callback stops throwing), Jasmine and Jest's `fail`, Mocha's
/// `done`.
const ASSERTING_CALLS: &[&str] = &["waitFor", "fail", "done"];

pub(super) fn check(path: &str, source: &str) -> Vec<Finding> {
    let Some(tree) = parse_path(path, source) else {
        return Vec::new();
    };
    let file = File::read(&tree, source);
    let Some(test_names) = file.test_names() else {
        return Vec::new();
    };
    let root = tree.root_node();
    let nodes = descendants(root);
    let hooks_assert = nodes.iter().any(|n| {
        n.kind() == "call_expression"
            && n.child_by_field_name("function")
                .is_some_and(|f| f.kind() == "identifier" && HOOKS.contains(&text(f, source)))
            && file.asserts(*n, HELPER_DEPTH)
    });
    if hooks_assert {
        return Vec::new();
    }
    nodes
        .into_iter()
        .filter_map(|n| test_without_assertion(&file, &test_names, n))
        .collect()
}

/// What the file binds: its imports, its function declarations, and which names it declares.
struct File<'t, 's> {
    source: &'s str,
    /// Local name → module specifier, for named, default and namespace imports.
    imports: HashMap<String, String>,
    /// Local name → the original exported name, for named imports.
    imported_as: HashMap<String, String>,
    /// Function bodies by the name the file declares them under.
    functions: HashMap<String, Node<'t>>,
    /// Every name the file declares (outside imports).
    declared: HashSet<String>,
}

impl<'t, 's> File<'t, 's> {
    fn read(tree: &'t Tree, source: &'s str) -> Self {
        let mut imports = HashMap::new();
        let mut imported_as = HashMap::new();
        for (local, binding) in import_bindings(tree, source) {
            imported_as.insert(local.clone(), binding.origin);
            imports.insert(local, binding.specifier);
        }
        for (local, specifier) in namespace_imports(tree, source) {
            imports.insert(local, specifier);
        }
        let root = tree.root_node();
        let mut functions = HashMap::new();
        let mut declared = HashSet::new();
        for node in descendants(root) {
            if node.kind() != "identifier" || !is_binding_position(node) {
                continue;
            }
            let name = text(node, source);
            if imports.contains_key(name) && in_import(node) {
                continue;
            }
            declared.insert(name.to_string());
            let Some(parent) = node.parent() else {
                continue;
            };
            let function = match parent.kind() {
                "function_declaration" => Some(parent),
                "variable_declarator" => parent
                    .child_by_field_name("value")
                    .filter(|v| is_function_like(*v)),
                _ => None,
            };
            if let Some(body) = function.and_then(|f| f.child_by_field_name("body")) {
                functions.insert(name.to_string(), body);
            }
        }
        Self {
            source,
            imports,
            imported_as,
            functions,
            declared,
        }
    }

    /// The names that declare a test in this file, or `None` when the file declares or imports
    /// one of them from somewhere other than a test framework (a custom `test` may assert).
    fn test_names(&self) -> Option<HashSet<&'static str>> {
        let mut names = HashSet::new();
        for name in TEST_NAMES {
            if self.declared.contains(*name) {
                return None;
            }
            if let Some(specifier) = self.imports.get(*name) {
                if !TEST_MODULES.contains(&specifier.as_str()) {
                    return None;
                }
            }
            names.insert(*name);
        }
        Some(names)
    }

    /// Whether `node`'s subtree holds an assertion (see the module doc of the checker),
    /// following same-file helpers to `depth`.
    fn asserts(&self, node: Node<'_>, depth: usize) -> bool {
        descendants(node).into_iter().any(|n| match n.kind() {
            "throw_statement" => true,
            // `x.should.equal(1)`, `sinon.assert.calledOnce(spy)`.
            "member_expression" => n
                .child_by_field_name("property")
                .is_some_and(|p| names_an_assertion(text(p, self.source))),
            "call_expression" => self.call_asserts(n, depth),
            _ => false,
        })
    }

    fn call_asserts(&self, call: Node<'_>, depth: usize) -> bool {
        let Some(callee) = call.child_by_field_name("function") else {
            return false;
        };
        let name = match callee.kind() {
            "identifier" => text(callee, self.source),
            "member_expression" => callee
                .child_by_field_name("property")
                .map_or("", |p| text(p, self.source)),
            _ => "",
        };
        if names_an_assertion(name) || ASSERTING_CALLS.contains(&name) || is_throwing_query(name) {
            return true;
        }
        let Some(root) = chain_root(callee) else {
            return false;
        };
        let root_name = text(root, self.source);
        // `assert.equal(...)`, `should.exist(...)` with a global or required assertion object.
        if names_an_assertion(root_name) {
            return true;
        }
        if let Some(specifier) = self.imports.get(root_name) {
            let original = self.imported_as.get(root_name).map_or("", String::as_str);
            return ASSERTION_MODULES.contains(&specifier.as_str())
                || names_an_assertion(original)
                || (!is_package(specifier) && is_support_module(specifier));
        }
        if callee.kind() == "identifier" && depth > 0 {
            if let Some(body) = self.functions.get(root_name) {
                return self.asserts(*body, depth - 1);
            }
        }
        false
    }
}

/// Testing Library queries that throw when nothing matches.
fn is_throwing_query(name: &str) -> bool {
    ["getBy", "getAllBy", "findBy", "findAllBy"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// A bare package specifier (`react`, `@testing-library/react`), as opposed to a relative path
/// or an alias into the repository (`./helpers`, `@/test-utils`, `~/support`).
fn is_package(specifier: &str) -> bool {
    !specifier.starts_with(['.', '/', '~', '#']) && !specifier.starts_with("@/")
}

/// The leftmost identifier of a callee chain: `expect` in `expect(x).not.toBe`, `h` in `h.check`.
fn chain_root(callee: Node<'_>) -> Option<Node<'_>> {
    let mut current = callee;
    loop {
        current = match current.kind() {
            "identifier" => return Some(current),
            "member_expression" | "subscript_expression" => {
                current.child_by_field_name("object")?
            }
            "call_expression" => current.child_by_field_name("function")?,
            "parenthesized_expression" | "await_expression" | "non_null_expression" => {
                current.named_child(0)?
            }
            _ => return None,
        };
    }
}

fn in_import(node: Node<'_>) -> bool {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "import_statement" {
            return true;
        }
        current = n.parent();
    }
    false
}

/// The test a call declares, when it is a running test with a parameterless callback and no
/// assertion.
fn test_without_assertion(
    file: &File<'_, '_>,
    test_names: &HashSet<&str>,
    call: Node<'_>,
) -> Option<Finding> {
    if call.kind() != "call_expression" {
        return None;
    }
    let callee = call.child_by_field_name("function")?;
    let declared_name = match callee.kind() {
        "identifier" => text(callee, file.source),
        "member_expression" => {
            let object = callee.child_by_field_name("object")?;
            let property = text(callee.child_by_field_name("property")?, file.source);
            if object.kind() != "identifier" || !RUNNING_MODIFIERS.contains(&property) {
                return None;
            }
            text(object, file.source)
        }
        _ => return None,
    };
    if !test_names.contains(declared_name) {
        return None;
    }
    let args = named_children(call.child_by_field_name("arguments")?);
    let (title, callback) = (args.first()?, args.last()?);
    if args.len() < 2
        || !matches!(title.kind(), "string" | "template_string")
        || !matches!(callback.kind(), "arrow_function" | "function_expression")
        || callback.child_by_field_name("parameter").is_some()
        || callback
            .child_by_field_name("parameters")
            .is_none_or(|p| !named_children(p).is_empty())
        || in_skipped_suite(call, file.source)
    {
        return None;
    }
    let body = callback.child_by_field_name("body")?;
    if file.asserts(body, HELPER_DEPTH) {
        return None;
    }
    let empty = body.kind() == "statement_block" && named_children(body).is_empty();
    let title = text(*title, file.source);
    let subject = format!("The test {} (line {})", short(title), line(call));
    Some(Finding {
        line: line(call),
        name: title
            .trim_matches(|c| c == '"' || c == '\'' || c == '`')
            .to_string(),
        message: message(&subject, empty),
    })
}

/// A test title as quoted in a message, cut at 80 characters.
fn short(title: &str) -> String {
    let trimmed = title.trim_matches(|c| c == '"' || c == '\'' || c == '`');
    let cut: String = trimmed.chars().take(80).collect();
    if cut.len() < trimmed.len() {
        format!("\"{cut}...\"")
    } else {
        format!("\"{cut}\"")
    }
}

/// Inside `describe.skip(...)`, `xdescribe(...)` and the like.
fn in_skipped_suite(call: Node<'_>, source: &str) -> bool {
    let mut current = call.parent();
    while let Some(n) = current {
        if n.kind() == "call_expression" {
            if let Some(callee) = n.child_by_field_name("function") {
                if SKIPPED_SUITES.contains(&text(callee, source)) {
                    return true;
                }
            }
        }
        current = n.parent();
    }
    false
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn a_framework_import_keeps_the_test_names_and_a_local_one_drops_them() {
        let read = |src: &str| {
            let tree = parse_path("a.test.ts", src).expect("parses");
            File::read(&tree, src).test_names().is_some()
        };
        assert!(read("import { it, test } from 'vitest';\n"));
        assert!(read("const x = 1;\n"));
        assert!(!read("import { test } from './fixtures';\n"));
        assert!(!read("function it(name, fn) { fn(); }\n"));
    }
}
