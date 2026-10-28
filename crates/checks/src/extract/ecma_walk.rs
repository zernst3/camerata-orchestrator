//! Small helpers over an ECMAScript syntax tree, shared by the W29 Wave D checkers
//! (`state_shape_checker`, `needless_complexity_checker`, `test_assertion_checker`). Each of them
//! reads a `tree-sitter` tree directly rather than an extracted summary, because the facts they
//! state (which block a setter call sits in, which attributes a JSX element carries, whether a
//! test body holds an assertion) are positions in the tree.
//!
//! Every walk here is iterative, so a deeply nested file cannot overflow the stack, and every
//! accessor degrades to `None` or an empty result instead of panicking.

use std::collections::HashMap;

use tree_sitter::{Node, Tree};

use super::ecma::{self, EcmaDialect, ImportBinding};
use super::{lang_for_path, SourceLang};

/// The grammar for a path, when it is an ECMAScript file.
pub(crate) fn dialect_for(path: &str) -> Option<EcmaDialect> {
    match lang_for_path(path)? {
        SourceLang::TypeScript => Some(EcmaDialect::TypeScript),
        SourceLang::Tsx => Some(EcmaDialect::Tsx),
        // `.jsx` and `.js` files hold JSX freely; the JavaScript grammar parses it.
        SourceLang::JavaScript => Some(EcmaDialect::JavaScript),
        SourceLang::Rust | SourceLang::Python => None,
    }
}

/// Parse an ECMAScript file by its path, or `None` for any other file (or one too deeply
/// nested to walk safely).
pub(crate) fn parse_path(path: &str, source: &str) -> Option<Tree> {
    ecma::parse(dialect_for(path)?, source)
}

/// A node's exact source text.
pub(crate) fn text<'s>(node: Node<'_>, source: &'s str) -> &'s str {
    source.get(node.byte_range()).unwrap_or_default()
}

/// 1-based start line.
pub(crate) fn line(node: Node<'_>) -> usize {
    node.start_position().row + 1
}

/// Every node under `node` (itself included), in document order.
pub(crate) fn descendants(node: Node<'_>) -> Vec<Node<'_>> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        out.push(n);
        let mut cursor = n.walk();
        let children: Vec<Node<'_>> = n.children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    out
}

/// The named children of a node, comments left out.
pub(crate) fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|c| c.kind() != "comment")
        .collect()
}

/// A function-like node: a declaration, an expression, an arrow or a method.
pub(crate) fn is_function_like(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "function_declaration"
            | "generator_function_declaration"
            | "function_expression"
            | "function"
            | "arrow_function"
            | "method_definition"
    )
}

/// The name a function-like node is known by: its own name, or the variable it is assigned to
/// (`const Profile = () => ...`).
pub(crate) fn function_name<'s>(node: Node<'_>, source: &'s str) -> Option<&'s str> {
    if let Some(name) = node.child_by_field_name("name") {
        return Some(text(name, source));
    }
    let parent = node.parent()?;
    (parent.kind() == "variable_declarator")
        .then(|| parent.child_by_field_name("name"))
        .flatten()
        .filter(|n| n.kind() == "identifier")
        .map(|n| text(n, source))
}

/// The file's named and default import bindings by local name (`import { a as b }` maps `b`).
pub(crate) fn import_bindings(tree: &Tree, source: &str) -> HashMap<String, ImportBinding> {
    ecma::import_bindings(tree.root_node(), source)
}

/// The local names of the file's namespace imports, with their specifiers
/// (`import * as React from 'react'` gives `("React", "react")`).
pub(crate) fn namespace_imports(tree: &Tree, source: &str) -> Vec<(String, String)> {
    let root = tree.root_node();
    let mut out = Vec::new();
    for statement in named_children(root) {
        if statement.kind() != "import_statement" {
            continue;
        }
        let Some(specifier) = statement
            .child_by_field_name("source")
            .map(|s| unquote(text(s, source)))
        else {
            continue;
        };
        for node in descendants(statement) {
            if node.kind() != "namespace_import" {
                continue;
            }
            if let Some(name) = named_children(node)
                .into_iter()
                .find(|c| c.kind() == "identifier")
            {
                out.push((text(name, source).to_string(), specifier.to_string()));
            }
        }
    }
    out
}

/// A string literal's contents without its quotes.
pub(crate) fn unquote(raw: &str) -> &str {
    raw.trim_matches(|c| c == '"' || c == '\'' || c == '`')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialects_follow_the_extension() {
        assert_eq!(dialect_for("a/b.tsx"), Some(EcmaDialect::Tsx));
        assert_eq!(dialect_for("a/b.jsx"), Some(EcmaDialect::JavaScript));
        assert_eq!(dialect_for("a/b.mts"), Some(EcmaDialect::TypeScript));
        assert_eq!(dialect_for("a/b.py"), None);
    }

    #[test]
    fn bindings_and_namespaces_are_read_by_local_name() {
        let src = "import React, { useState as useS } from 'react';\nimport * as UI from './ui';\n";
        let tree = parse_path("a.tsx", src).expect("parses");
        let bindings = import_bindings(&tree, src);
        assert_eq!(bindings["useS"].origin, "useState");
        assert_eq!(bindings["React"].origin, "default");
        assert_eq!(
            namespace_imports(&tree, src),
            vec![("UI".to_string(), "./ui".to_string())]
        );
    }

    #[test]
    fn function_names_come_from_the_declaration_or_the_variable() {
        let src = "function A() {}\nconst B = () => 1;\n";
        let tree = parse_path("a.ts", src).expect("parses");
        let names: Vec<&str> = descendants(tree.root_node())
            .into_iter()
            .filter(|n| is_function_like(*n))
            .filter_map(|n| function_name(n, src))
            .collect();
        assert_eq!(names, vec!["A", "B"]);
    }
}
