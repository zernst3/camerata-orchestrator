//! Needless optional props: a TSX component prop declared optional (`size?: Size`) that every
//! place rendering the component passes with a value that cannot be `undefined`. The component
//! keeps handling a missing value nobody produces.
//!
//! This module finds the CANDIDATES in one file (a component, its props type, its optional
//! members); [`super::call_sites`] decides from the whole repository whether every call site is
//! known and passes the prop. The guards on the component itself are here: it must start with a
//! capital, render JSX, take exactly one parameter typed by an inline object type or by a
//! non-exported interface or object type alias that nothing else in the file names, and have no
//! type parameters. A member with a default in the parameter's destructuring, and `children`, are
//! never candidates.

use tree_sitter::{Node, Tree};

use crate::extract::ecma_walk::{descendants, has_jsx, line, named_children, text};

/// One optional member of a component's props type.
#[derive(Debug, Clone)]
pub(super) struct OptionalMember {
    pub(super) name: String,
    pub(super) line: usize,
}

/// A component whose props type has at least one optional member that could be needless.
pub(super) struct Candidate<'t> {
    pub(super) name: String,
    /// The names other modules import it by (`default` for a default export).
    pub(super) exported_as: Vec<String>,
    /// The identifier that declares it.
    pub(super) declaration: Node<'t>,
    pub(super) members: Vec<OptionalMember>,
}

pub(super) fn candidates<'t>(tree: &'t Tree, source: &str) -> Vec<Candidate<'t>> {
    let root = tree.root_node();
    let statements = named_children(root);
    let mut out = Vec::new();
    for statement in &statements {
        let (declaration, default_export, exported) = match statement.kind() {
            "export_statement" => {
                let Some(declaration) = statement.child_by_field_name("declaration") else {
                    continue;
                };
                (declaration, has_default_keyword(*statement), true)
            }
            _ => (*statement, false, false),
        };
        for (name_node, function) in declared_functions(declaration) {
            let name = text(name_node, source);
            if !name.starts_with(|c: char| c.is_ascii_uppercase()) {
                continue;
            }
            let Some(members) = optional_members(function, root, &statements, source) else {
                continue;
            };
            if members.is_empty() {
                continue;
            }
            let mut exported_as = Vec::new();
            if exported {
                exported_as.push(if default_export {
                    "default".to_string()
                } else {
                    name.to_string()
                });
            }
            exported_as.extend(later_exports(&statements, name, source));
            out.push(Candidate {
                name: name.to_string(),
                exported_as,
                declaration: name_node,
                members,
            });
        }
    }
    out
}

fn has_default_keyword(statement: Node<'_>) -> bool {
    let mut cursor = statement.walk();
    let found = statement
        .children(&mut cursor)
        .any(|c| c.kind() == "default");
    found
}

/// `function X(...)` or `const X = (...) => ...` / `const X = function (...) {...}`: the name
/// node and the function node.
fn declared_functions(declaration: Node<'_>) -> Vec<(Node<'_>, Node<'_>)> {
    match declaration.kind() {
        "function_declaration" => declaration
            .child_by_field_name("name")
            .map(|n| vec![(n, declaration)])
            .unwrap_or_default(),
        "lexical_declaration" => named_children(declaration)
            .into_iter()
            .filter(|d| {
                d.kind() == "variable_declarator" && d.child_by_field_name("type").is_none()
            })
            .filter_map(|d| {
                let name = d.child_by_field_name("name")?;
                let value = d.child_by_field_name("value")?;
                (name.kind() == "identifier"
                    && matches!(value.kind(), "arrow_function" | "function_expression"))
                .then_some((name, value))
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The optional members of the component's props type, or `None` when the function is not a
/// component this check can read (see the module doc).
fn optional_members(
    function: Node<'_>,
    root: Node<'_>,
    statements: &[Node<'_>],
    source: &str,
) -> Option<Vec<OptionalMember>> {
    if function.child_by_field_name("type_parameters").is_some()
        || !has_jsx(function.child_by_field_name("body")?)
    {
        return None;
    }
    let [param] = named_children(function.child_by_field_name("parameters")?)[..] else {
        return None;
    };
    if param.kind() != "required_parameter" {
        return None;
    }
    let pattern = param.child_by_field_name("pattern")?;
    let annotation = param.child_by_field_name("type")?;
    let [props_type] = named_children(annotation)[..] else {
        return None;
    };
    let body = match props_type.kind() {
        "object_type" => props_type,
        "type_identifier" => named_type_body(text(props_type, source), root, statements, source)?,
        _ => return None,
    };
    let defaults = destructured_defaults(pattern, source);
    Some(
        named_children(body)
            .into_iter()
            .filter(|m| m.kind() == "property_signature" && is_optional(*m))
            .filter_map(|m| {
                let name = m.child_by_field_name("name")?;
                if name.kind() != "property_identifier" {
                    return None;
                }
                let name = text(name, source);
                (name != "children" && !defaults.iter().any(|d| d == name)).then(|| {
                    OptionalMember {
                        name: name.to_string(),
                        line: line(m),
                    }
                })
            })
            .collect(),
    )
}

/// The member list of a same-file `interface P {...}` or `type P = {...}` that is not exported,
/// has no type parameters, and is named nowhere else in the file but the one annotation.
fn named_type_body<'t>(
    name: &str,
    root: Node<'t>,
    statements: &[Node<'t>],
    source: &str,
) -> Option<Node<'t>> {
    let uses = descendants(root)
        .into_iter()
        .filter(|n| n.kind() == "type_identifier" && text(*n, source) == name)
        .count();
    if uses != 2 {
        return None;
    }
    statements.iter().find_map(|s| {
        let declared = s
            .child_by_field_name("name")
            .is_some_and(|n| text(n, source) == name);
        if !declared || s.child_by_field_name("type_parameters").is_some() {
            return None;
        }
        match s.kind() {
            "interface_declaration" => s.child_by_field_name("body"),
            "type_alias_declaration" => s
                .child_by_field_name("value")
                .filter(|v| v.kind() == "object_type"),
            _ => None,
        }
    })
}

/// A `?` token directly in the member: `size?: Size`.
fn is_optional(member: Node<'_>) -> bool {
    let mut cursor = member.walk();
    let found = member.children(&mut cursor).any(|c| c.kind() == "?");
    found
}

/// Names given a default in an object destructuring parameter: `{ size = 'md' }` or
/// `{ size: s = 'md' }`.
fn destructured_defaults(pattern: Node<'_>, source: &str) -> Vec<String> {
    if pattern.kind() != "object_pattern" {
        return Vec::new();
    }
    named_children(pattern)
        .into_iter()
        .filter_map(|p| match p.kind() {
            "object_assignment_pattern" => p
                .child_by_field_name("left")
                .map(|l| text(l, source).to_string()),
            "pair_pattern" => {
                let value = p.child_by_field_name("value")?;
                (value.kind() == "assignment_pattern")
                    .then(|| p.child_by_field_name("key"))
                    .flatten()
                    .map(|k| text(k, source).to_string())
            }
            _ => None,
        })
        .collect()
}

/// `export { X }`, `export { X as Y }` and `export default X` after the declaration.
fn later_exports(statements: &[Node<'_>], name: &str, source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for statement in statements {
        if statement.kind() != "export_statement"
            || statement.child_by_field_name("source").is_some()
        {
            continue;
        }
        if let Some(value) = statement.child_by_field_name("value") {
            if has_default_keyword(*statement)
                && value.kind() == "identifier"
                && text(value, source) == name
            {
                out.push("default".to_string());
            }
            continue;
        }
        for node in descendants(*statement) {
            if node.kind() != "export_specifier" {
                continue;
            }
            let Some(local) = node.child_by_field_name("name") else {
                continue;
            };
            if text(local, source) == name {
                let exported = node.child_by_field_name("alias").unwrap_or(local);
                out.push(text(exported, source).to_string());
            }
        }
    }
    out
}
