//! Import bindings: which local names a file imports and from which module. The composed-variant
//! guards ask "does every copy call the same helper, imported from the same module", and the
//! shared extractor's `imports()` cannot answer for a default import (it records no name), so
//! the ECMAScript dialects are read here directly from the syntax tree.

use tree_sitter::Node;

use crate::extract::ecma::{self, EcmaDialect};
use crate::extract::{python, rust_syn, SourceLang};

/// One imported local name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Binding {
    pub name: String,
    /// The module specifier as written in source.
    pub specifier: String,
}

/// Every import binding of a file. A shape this reader does not model yields no binding, so an
/// unmodelled import can only make a guard fail to drop a candidate, never drop one wrongly.
pub(super) fn import_bindings(lang: SourceLang, source: &str) -> Vec<Binding> {
    match lang {
        SourceLang::TypeScript => ecma_bindings(EcmaDialect::TypeScript, source),
        SourceLang::Tsx => ecma_bindings(EcmaDialect::Tsx, source),
        SourceLang::JavaScript => ecma_bindings(EcmaDialect::JavaScript, source),
        SourceLang::Python => python::imports(source)
            .into_iter()
            .flat_map(|import| {
                import.names.into_iter().map(move |name| Binding {
                    name,
                    specifier: import.specifier.clone(),
                })
            })
            .collect(),
        SourceLang::Rust => rust_syn::imports(source)
            .into_iter()
            .flat_map(|import| {
                import.names.into_iter().map(move |name| Binding {
                    name,
                    specifier: import.specifier.clone(),
                })
            })
            .collect(),
    }
}

fn ecma_bindings(dialect: EcmaDialect, source: &str) -> Vec<Binding> {
    let Some(tree) = ecma::parse(dialect, source) else {
        return Vec::new();
    };
    let root = tree.root_node();
    let mut out = Vec::new();
    let mut cursor = root.walk();
    for statement in root.children(&mut cursor) {
        if statement.kind() != "import_statement" {
            continue;
        }
        let Some(specifier) = specifier_of(statement, source) else {
            continue;
        };
        let mut inner = statement.walk();
        for clause in statement.children(&mut inner) {
            if clause.kind() == "import_clause" {
                collect_clause(clause, source, &specifier, &mut out);
            }
        }
    }
    out
}

fn specifier_of(statement: Node<'_>, source: &str) -> Option<String> {
    let node = statement.child_by_field_name("source")?;
    let text = source.get(node.byte_range())?;
    let trimmed = text.trim_matches(|c| c == '"' || c == '\'' || c == '`');
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// The local names of one `import_clause`: a default name, a namespace name, and the (possibly
/// aliased) named imports.
fn collect_clause(clause: Node<'_>, source: &str, specifier: &str, out: &mut Vec<Binding>) {
    let mut cursor = clause.walk();
    for child in clause.children(&mut cursor) {
        match child.kind() {
            "identifier" => push(out, source, child, specifier),
            "namespace_import" => {
                let mut inner = child.walk();
                for part in child.children(&mut inner) {
                    if part.kind() == "identifier" {
                        push(out, source, part, specifier);
                    }
                }
            }
            "named_imports" => {
                let mut inner = child.walk();
                for part in child.children(&mut inner) {
                    if part.kind() != "import_specifier" {
                        continue;
                    }
                    // `{ a as b }` binds `b`; `{ a }` binds `a`.
                    let local = part
                        .child_by_field_name("alias")
                        .or_else(|| part.child_by_field_name("name"));
                    if let Some(local) = local {
                        push(out, source, local, specifier);
                    }
                }
            }
            _ => {}
        }
    }
}

fn push(out: &mut Vec<Binding>, source: &str, node: Node<'_>, specifier: &str) {
    if let Some(name) = source.get(node.byte_range()) {
        out.push(Binding {
            name: name.to_string(),
            specifier: specifier.to_string(),
        });
    }
}

/// A module specifier made comparable across files: a relative specifier is resolved against the
/// importing file's directory, so `./pricing` in `src/a/x.ts` and `../a/pricing` in `src/b/y.ts`
/// are one module. A specifier that cannot be resolved to a single place (a Rust `self::` or
/// `super::` path, an unresolvable Python relative) is made unique to its file, so it never
/// compares equal to another file's.
pub(super) fn normalize_specifier(lang: SourceLang, file: &str, specifier: &str) -> String {
    match lang {
        SourceLang::TypeScript | SourceLang::Tsx | SourceLang::JavaScript => {
            if specifier.starts_with("./") || specifier.starts_with("../") {
                resolve_relative(file, specifier)
            } else {
                specifier.to_string()
            }
        }
        SourceLang::Python => normalize_python(file, specifier),
        SourceLang::Rust => {
            if specifier.starts_with("self") || specifier.starts_with("super") {
                format!("{file}::{specifier}")
            } else {
                specifier.to_string()
            }
        }
    }
}

fn directory_of(file: &str) -> Vec<&str> {
    let mut parts: Vec<&str> = file.split('/').collect();
    parts.pop();
    parts
}

fn resolve_relative(file: &str, specifier: &str) -> String {
    let mut parts = directory_of(file);
    for segment in specifier.split('/') {
        match segment {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    let mut joined = parts.join("/");
    for extension in [".tsx", ".ts", ".jsx", ".js", ".mjs", ".cjs"] {
        if let Some(stripped) = joined.strip_suffix(extension) {
            joined = stripped.to_string();
            break;
        }
    }
    if let Some(stripped) = joined.strip_suffix("/index") {
        joined = stripped.to_string();
    }
    joined
}

/// A Python specifier: leading dots count levels up from the importing file's package; the rest
/// is a dotted module path.
fn normalize_python(file: &str, specifier: &str) -> String {
    let dots = specifier.chars().take_while(|c| *c == '.').count();
    let rest = &specifier[dots..];
    if dots == 0 {
        return rest.replace('.', "/");
    }
    let mut parts = directory_of(file);
    for _ in 1..dots {
        if parts.pop().is_none() {
            // Walked above the repository root: not a place that can be compared.
            return format!("{file}::{specifier}");
        }
    }
    if !rest.is_empty() {
        parts.extend(rest.split('.'));
    }
    parts.join("/")
}

/// A module imports `name` from `specifier`, resolved (see [`normalize_specifier`]).
pub(super) fn module_of(
    bindings: &[Binding],
    lang: SourceLang,
    file: &str,
    name: &str,
) -> Option<String> {
    bindings
        .iter()
        .find(|b| b.name == name)
        .map(|b| normalize_specifier(lang, file, &b.specifier))
}

/// Whether the file imports the name at all (any module).
pub(super) fn imports_name(bindings: &[Binding], name: &str) -> bool {
    bindings.iter().any(|b| b.name == name)
}
