//! Every place a component is rendered, found through the repository's import graph (the
//! re-export canonicalisation ARCH-ONE-IMPORT-PATH-1 built, [`Model::origin`]), and whether each
//! of them passes a prop with a value that cannot be `undefined`.
//!
//! The question "does EVERY caller pass it" is only answered when every reference to the
//! component is accounted for. A component is left alone when any file uses it other than as a
//! JSX tag (as a value, in a type, through a namespace import, a dynamic import or a computed
//! load), when a file names it without an import the resolver can follow, when a file the check
//! does not parse names it, when a call site spreads props, and when its package declares entry
//! points other code could import it through.

use std::collections::HashSet;

use tree_sitter::{Node, Tree};

use super::optional_props::Candidate;
use crate::dead_code_checker::manifests::Packages;
use crate::extract::ecma_walk::{
    descendants, import_bindings, is_binding_position, line, named_children, namespace_imports,
    text, unquote,
};
use crate::extract::resolver::{directory_of, normalize_join, EcmaResolver};
use crate::extract::{computed_loads, lang_for_path, ComputedLoad};
use crate::one_import_path_checker::Model;

/// Extensions of files that can render a component but are not parsed here: a mention of the
/// component's name in one of them makes its call sites unknowable.
const UNPARSED_TEMPLATE_EXTENSIONS: &[&str] = &[".mdx", ".vue", ".svelte", ".astro"];

/// One import of a file, resolved once.
struct ImportFact {
    /// The local binding (empty for a namespace import or a dynamic load).
    local: String,
    /// The name the module exports it under (`default` for a default import); `None` for a
    /// namespace import or a dynamic load, which can reach any export.
    imported: Option<String>,
    /// The resolved module, when the specifier resolves to a repository file.
    target: Option<String>,
    /// The canonical origin, when the name pins to one origin.
    origin: Option<(String, String)>,
    /// A package from outside the repository (a declared dependency or a Node built-in).
    external: bool,
    type_only: bool,
}

/// One parsed ECMAScript file with its imports resolved once.
pub(super) struct FileFacts<'a> {
    pub(super) path: &'a str,
    pub(super) source: &'a str,
    pub(super) tree: Tree,
    imports: Vec<ImportFact>,
}

impl<'a> FileFacts<'a> {
    fn new(
        path: &'a str,
        source: &'a str,
        tree: Tree,
        resolver: &EcmaResolver<'_>,
        graph: &Model,
        packages: &Packages,
    ) -> Self {
        let fact = |local: String, imported: Option<String>, specifier: &str, type_only: bool| {
            let target = resolver.resolve(path, specifier);
            let origin = match (&target, &imported) {
                (Some(t), Some(name)) => graph.origin(t, name),
                _ => None,
            };
            ImportFact {
                local,
                imported,
                external: target.is_none() && packages.is_external_specifier(specifier),
                target,
                origin,
                type_only,
            }
        };
        let mut imports: Vec<ImportFact> = import_bindings(&tree, source)
            .into_iter()
            .map(|(local, b)| fact(local, Some(b.origin), &b.specifier, b.type_only))
            .collect();
        imports.extend(
            namespace_imports(&tree, source)
                .into_iter()
                .map(|(_, specifier)| specifier)
                .chain(dynamic_specifiers(&tree, source))
                .map(|specifier| fact(String::new(), None, &specifier, false)),
        );
        imports.sort_by(|a, b| a.local.cmp(&b.local));
        Self {
            path,
            source,
            tree,
            imports,
        }
    }
}

/// `import('x')` and `require('x')` with a string argument.
fn dynamic_specifiers(tree: &Tree, source: &str) -> Vec<String> {
    descendants(tree.root_node())
        .into_iter()
        .filter(|n| n.kind() == "call_expression")
        .filter_map(|call| {
            let callee = call.child_by_field_name("function")?;
            let is_load = callee.kind() == "import"
                || (callee.kind() == "identifier" && text(callee, source) == "require");
            if !is_load {
                return None;
            }
            let first = named_children(call.child_by_field_name("arguments")?)
                .into_iter()
                .next()?;
            (first.kind() == "string").then(|| unquote(text(first, source)).to_string())
        })
        .collect()
}

/// One JSX element rendering the component.
pub(super) struct Site<'t> {
    pub(super) file: String,
    pub(super) line: usize,
    pub(super) element: Node<'t>,
    pub(super) source_index: usize,
}

/// Everything the repository-wide guards need.
pub(super) struct Repo<'a> {
    pub(super) parsed: Vec<FileFacts<'a>>,
    /// The contents of files that can name a component but are not parsed: ECMAScript files the
    /// file guard skipped (vendored, generated, minified) and MDX, Vue, Svelte and Astro files.
    unread: Vec<&'a str>,
    /// Every computed load, by the file that makes it.
    computed: Vec<(&'a str, Vec<ComputedLoad>)>,
    packages: Packages,
    graph: Model,
}

impl<'a> Repo<'a> {
    pub(super) fn new(
        files: &'a [(String, String)],
        readable: Vec<(&'a str, &'a str, Tree)>,
        unparsed_code: &HashSet<&str>,
    ) -> Self {
        let resolver = EcmaResolver::new(files);
        let graph = Model::build(files, &resolver);
        let packages = Packages::load(files);
        let parsed = readable
            .into_iter()
            .map(|(path, source, tree)| {
                FileFacts::new(path, source, tree, &resolver, &graph, &packages)
            })
            .collect();
        let unread = files
            .iter()
            .filter(|(path, _)| {
                unparsed_code.contains(path.as_str())
                    || UNPARSED_TEMPLATE_EXTENSIONS
                        .iter()
                        .any(|ext| path.to_ascii_lowercase().ends_with(ext))
            })
            .map(|(_, content)| content.as_str())
            .collect();
        let computed = files
            .iter()
            .filter_map(|(path, content)| {
                let loads = computed_loads(lang_for_path(path)?, content);
                (!loads.is_empty()).then_some((path.as_str(), loads))
            })
            .collect();
        Self {
            parsed,
            unread,
            computed,
            packages,
            graph,
        }
    }

    /// Every JSX element rendering the candidate, or `None` when its call sites cannot all be
    /// known (see the module doc).
    pub(super) fn sites(&self, home: usize, candidate: &Candidate<'_>) -> Option<Vec<Site<'_>>> {
        let home_path = self.parsed[home].path;
        let package = self.packages.nearest(home_path)?;
        if package.declares_entry_points()
            || self.computed_load_reaches(home_path, &package.dir)
            || self.mentioned_where_unread(&candidate.name)
        {
            return None;
        }
        let mut sites = Vec::new();
        for (index, file) in self.parsed.iter().enumerate() {
            let locals: Vec<&str> = if index == home {
                vec![candidate.name.as_str()]
            } else {
                self.imported_locals(file, home_path, candidate)?
            };
            if index != home && locals.is_empty() {
                // Not an importer: any mention of the name must be another symbol.
                if contains_word(file.source, &candidate.name)
                    && !other_mentions_are_accounted(file, &candidate.name)
                {
                    return None;
                }
                continue;
            }
            for node in descendants(file.tree.root_node()) {
                if !is_name_like(node) || !locals.contains(&text(node, file.source)) {
                    continue;
                }
                if index == home && node.id() == candidate.declaration.id() {
                    continue;
                }
                if let Some(element) = jsx_element_named_by(node) {
                    if element.kind() != "jsx_closing_element" {
                        sites.push(Site {
                            file: file.path.to_string(),
                            line: line(element),
                            element,
                            source_index: index,
                        });
                    }
                    continue;
                }
                if index != home && is_binding_position(node) {
                    continue; // the import itself
                }
                if is_local_export(node) {
                    continue;
                }
                return None;
            }
        }
        Some(sites)
    }

    /// The local names under which `file` imports the candidate, or `None` when one of its
    /// imports might reach the candidate without the check being able to tell: a namespace
    /// import or dynamic load of a module that exports it, a type-only import of it, or an
    /// import of one of its exported names that does not resolve to one origin.
    fn imported_locals<'f>(
        &self,
        file: &'f FileFacts<'_>,
        home: &str,
        candidate: &Candidate<'_>,
    ) -> Option<Vec<&'f str>> {
        let mut locals = Vec::new();
        for import in &file.imports {
            if import.external {
                continue;
            }
            let Some(imported) = &import.imported else {
                // A namespace import or a dynamic load: any export of the target is reachable.
                match &import.target {
                    Some(target) if !self.reaches(target, home, &candidate.exported_as) => continue,
                    _ => return None,
                }
            };
            match &import.origin {
                Some((file_path, name))
                    if file_path == home && candidate.exported_as.contains(name) =>
                {
                    if import.type_only {
                        return None;
                    }
                    locals.push(import.local.as_str());
                }
                Some(_) => {}
                None if candidate.exported_as.contains(imported) => return None,
                None => {}
            }
        }
        Some(locals)
    }

    /// Whether `target` is the home module, or re-exports the component.
    fn reaches(&self, target: &str, home: &str, exported_as: &[String]) -> bool {
        target == home
            || exported_as
                .iter()
                .any(|e| self.graph.origin(target, e) == Some((home.to_string(), e.clone())))
    }

    /// A template file the check does not parse, or a code file it skipped, names the component.
    fn mentioned_where_unread(&self, name: &str) -> bool {
        self.unread
            .iter()
            .any(|content| contains_word(content, name))
    }

    /// A computed load (`import.meta.glob`, `require.context`, `import(name)`) in the package
    /// that could reach the component's file.
    fn computed_load_reaches(&self, home: &str, package_dir: &str) -> bool {
        self.computed
            .iter()
            .filter(|(path, _)| is_inside(path, package_dir))
            .any(|(path, loads)| {
                loads.iter().any(|load| {
                    let Some(prefix) = &load.prefix else {
                        return true;
                    };
                    let reach = prefix.rsplit_once('/').map_or("", |(dir, _)| dir);
                    let dir = normalize_join(directory_of(path), reach);
                    is_inside(home, &dir)
                })
            })
    }
}

/// In a file that does not import the component, any mention of its name must be a different
/// symbol (imported from elsewhere, which [`Repo::imported_locals`] has already resolved, or
/// declared there) or a re-export statement.
fn other_mentions_are_accounted(file: &FileFacts<'_>, name: &str) -> bool {
    if file.imports.iter().any(|i| i.local == name) {
        return true;
    }
    let mentions: Vec<Node<'_>> = descendants(file.tree.root_node())
        .into_iter()
        .filter(|n| is_name_like(*n) && text(*n, file.source) == name)
        .collect();
    mentions.iter().any(|n| is_binding_position(*n))
        || mentions
            .iter()
            .all(|n| in_re_export_from(*n) || is_aliased_import_name(*n))
}

/// Identifier-like nodes that can carry a component's name as a value or a tag. Property names
/// (`obj.Button`, `{ Button: x }`) are not references to a binding.
fn is_name_like(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "identifier" | "shorthand_property_identifier" | "type_identifier"
    )
}

/// The JSX opening, self-closing or closing element whose tag name is `node`.
fn jsx_element_named_by(node: Node<'_>) -> Option<Node<'_>> {
    let parent = node.parent()?;
    let is_tag = matches!(
        parent.kind(),
        "jsx_opening_element" | "jsx_self_closing_element" | "jsx_closing_element"
    ) && parent
        .child_by_field_name("name")
        .is_some_and(|n| n.id() == node.id());
    is_tag.then_some(parent)
}

/// `export { Button }` / `export default Button` without a source: the binding is forwarded,
/// and whoever imports it is followed through the graph.
fn is_local_export(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        "export_specifier" => true,
        "export_statement" => parent
            .child_by_field_name("value")
            .is_some_and(|v| v.id() == node.id()),
        _ => false,
    }
}

/// The imported name of an aliased import (`Button` in `import { Button as B }`): the binding
/// is `B`, which the import facts already follow.
fn is_aliased_import_name(node: Node<'_>) -> bool {
    node.parent().is_some_and(|p| {
        p.kind() == "import_specifier"
            && p.child_by_field_name("alias").is_some()
            && p.child_by_field_name("name")
                .is_some_and(|n| n.id() == node.id())
    })
}

/// Inside `export { X } from '...'`.
fn in_re_export_from(node: Node<'_>) -> bool {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "export_statement" {
            return n.child_by_field_name("source").is_some();
        }
        current = n.parent();
    }
    false
}

fn is_inside(path: &str, dir: &str) -> bool {
    dir.is_empty() || path.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
}

/// Whether `name` occurs in `text` as a whole identifier.
fn contains_word(text: &str, name: &str) -> bool {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    text.match_indices(name).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + name.len()..].chars().next();
        !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
    })
}

// ─── the value a call site passes ──────────────────────────────────────────────────────

/// Kinds of attribute expression that can never evaluate to `undefined`.
const DEFINED_KINDS: &[&str] = &[
    "string",
    "template_string",
    "number",
    "true",
    "false",
    "null",
    "object",
    "array",
    "arrow_function",
    "function_expression",
    "function",
    "new_expression",
    "jsx_element",
    "jsx_self_closing_element",
    "jsx_fragment",
    "regex",
];

/// The attributes of a site, or `None` when it spreads props (`{...rest}`), which could pass or
/// override anything.
pub(super) fn attributes<'t>(element: Node<'t>, source: &str) -> Option<Vec<(String, Node<'t>)>> {
    let mut out = Vec::new();
    for child in named_children(element) {
        match child.kind() {
            "jsx_attribute" => {
                let Some(name) = named_children(child).into_iter().next() else {
                    continue;
                };
                out.push((text(name, source).to_string(), child));
            }
            "jsx_expression" => return None,
            _ => {}
        }
    }
    Some(out)
}

/// Whether the attribute passes a value that cannot be `undefined`.
pub(super) fn passes_defined_value(attribute: Node<'_>, root: Node<'_>, source: &str) -> bool {
    let parts = named_children(attribute);
    let Some(value) = parts.get(1) else {
        return true; // `<X disabled />` passes `true`
    };
    let expression = if value.kind() == "jsx_expression" {
        let [inner] = named_children(*value)[..] else {
            return false;
        };
        inner
    } else {
        *value
    };
    if DEFINED_KINDS.contains(&expression.kind()) {
        return true;
    }
    expression.kind() == "identifier"
        && names_a_defined_constant(text(expression, source), root, source)
}

/// The file declares `name` exactly once, as a function or as a `const` holding a value that
/// cannot be `undefined` (or a `useCallback`), and binds it nowhere else (no parameter, no
/// destructuring, no import of the same name).
fn names_a_defined_constant(name: &str, root: Node<'_>, source: &str) -> bool {
    let bindings: Vec<Node<'_>> = descendants(root)
        .into_iter()
        .filter(|n| {
            matches!(
                n.kind(),
                "identifier" | "shorthand_property_identifier_pattern"
            ) && text(*n, source) == name
                && is_binding_position(*n)
        })
        .collect();
    let [binding] = bindings[..] else {
        return false;
    };
    let Some(parent) = binding.parent() else {
        return false;
    };
    match parent.kind() {
        "function_declaration" => true,
        "variable_declarator" => {
            let is_const = parent
                .parent()
                .is_some_and(|d| text(d, source).starts_with("const"));
            let Some(value) = parent.child_by_field_name("value") else {
                return false;
            };
            let is_callback = value.kind() == "call_expression"
                && value
                    .child_by_field_name("function")
                    .is_some_and(|f| text(f, source) == "useCallback");
            is_const && (DEFINED_KINDS.contains(&value.kind()) || is_callback)
        }
        _ => false,
    }
}
