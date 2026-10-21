//! Package manifests as sources of liveness: the entry points they name (`R2`), the
//! public-surface rule for a package anyone can deep-import (`R4`), the invisible-entry rule
//! for an entry the scan cannot see (`R7`), and the skip for packages whose importers are
//! single-file components the graph cannot read (`G19`).
//!
//! Every manifest is parsed leniently: a malformed one yields nothing, never an error.

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::extract::resolver::{directory_of, normalize_join, resolve_python_module, EcmaResolver};

use super::roots::{is_inside, RootSet};

/// One parsed `package.json`.
#[derive(Debug)]
pub(super) struct Package {
    /// Directory of the manifest (`""` for the repository root).
    pub(super) dir: String,
    pub(super) path: String,
    json: Map<String, Value>,
}

/// Every parseable `package.json` in the scan, deepest first.
#[derive(Debug, Default)]
pub(super) struct Packages {
    list: Vec<Package>,
}

impl Packages {
    pub(super) fn load(files: &[(String, String)]) -> Self {
        let mut list: Vec<Package> = files
            .iter()
            .filter(|(p, _)| p == "package.json" || p.ends_with("/package.json"))
            .filter_map(|(p, c)| {
                let Value::Object(json) = serde_json::from_str::<Value>(c).ok()? else {
                    return None;
                };
                Some(Package {
                    dir: directory_of(p).to_string(),
                    path: p.clone(),
                    json,
                })
            })
            .collect();
        list.sort_by(|a, b| {
            b.dir
                .len()
                .cmp(&a.dir.len())
                .then_with(|| a.dir.cmp(&b.dir))
        });
        Self { list }
    }

    /// The package whose folder is the nearest ancestor of `path`.
    pub(super) fn nearest(&self, path: &str) -> Option<&Package> {
        self.list.iter().find(|p| is_inside(path, &p.dir))
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &Package> {
        self.list.iter()
    }
}

/// Directories holding a `package.json`, `Cargo.toml` or `pyproject.toml`, parsed or not:
/// the root of "the package this file belongs to" for loads and tool folders.
#[derive(Debug, Default)]
pub(super) struct ManifestDirs {
    /// Deepest first.
    dirs: Vec<String>,
}

impl ManifestDirs {
    pub(super) fn load(files: &[(String, String)]) -> Self {
        let mut dirs: Vec<String> = files
            .iter()
            .filter(|(p, _)| {
                matches!(
                    p.rsplit('/').next().unwrap_or(p),
                    "package.json" | "Cargo.toml" | "pyproject.toml"
                )
            })
            .map(|(p, _)| directory_of(p).to_string())
            .collect();
        dirs.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        dirs.dedup();
        Self { dirs }
    }

    /// The nearest manifest directory above `path` (`""` when no manifest encloses it).
    pub(super) fn nearest(&self, path: &str) -> &str {
        self.dirs
            .iter()
            .find(|d| is_inside(path, d))
            .map_or("", String::as_str)
    }
}

impl Package {
    fn is_private(&self) -> bool {
        self.json.get("private") == Some(&Value::Bool(true))
    }

    fn has_exports(&self) -> bool {
        self.json.contains_key("exports")
    }

    pub(super) fn depends_on(&self, matches: impl Fn(&str) -> bool) -> bool {
        [
            "dependencies",
            "devDependencies",
            "peerDependencies",
            "optionalDependencies",
        ]
        .iter()
        .filter_map(|section| self.json.get(*section)?.as_object())
        .any(|deps| deps.keys().any(|k| matches(k)))
    }
}

/// Every string a manifest uses as a file path: `main`, `module`, `browser`, `types`,
/// `typings`, `bin` (a string or a map of paths) and every string leaf of `exports`.
fn entry_values(json: &Map<String, Value>) -> Vec<String> {
    fn leaves(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::String(s) => out.push(s.clone()),
            Value::Array(items) => items.iter().for_each(|v| leaves(v, out)),
            Value::Object(map) => map.values().for_each(|v| leaves(v, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for field in [
        "main", "module", "browser", "types", "typings", "bin", "exports",
    ] {
        if let Some(value) = json.get(field) {
            leaves(value, &mut out);
        }
    }
    out.retain(|v| !v.is_empty());
    out
}

/// Dependencies that bring single-file components (or MDX) whose `import` lines the graph
/// cannot read, with the extension of the files that hold them. A name ending in `/` is a
/// scope prefix.
const SFC_DEPENDENCIES: &[(&str, &str)] = &[
    ("vue", ".vue"),
    ("nuxt", ".vue"),
    ("svelte", ".svelte"),
    ("@sveltejs/kit", ".svelte"),
    ("astro", ".astro"),
    ("@mdx-js/", ".mdx"),
];

fn dependency_matches(pattern: &str, dependency: &str) -> bool {
    if pattern.ends_with('/') {
        dependency.starts_with(pattern)
    } else {
        dependency == pattern
    }
}

/// Everything the manifests make alive.
pub(super) fn manifest_roots(files: &[(String, String)], packages: &Packages) -> RootSet {
    let mut roots = RootSet::default();
    let resolver = EcmaResolver::new(files);
    for package in packages.iter() {
        for value in entry_values(&package.json) {
            entry_root(&mut roots, files, &resolver, package, &value);
        }
        // R4: a package nobody marked private, with no `exports` map, is deep-importable by
        // consumers outside this repository, so every file in it is public API.
        if !package.is_private() && !package.has_exports() {
            roots.insert_package(package.dir.clone());
        }
        // G19: importers inside single-file components are invisible to the graph. Without
        // those files in the scan, nothing in the package can be proved dead.
        let blind = SFC_DEPENDENCIES.iter().any(|(dep, ext)| {
            package.depends_on(|k| dependency_matches(dep, k))
                && !files
                    .iter()
                    .any(|(p, _)| p.ends_with(ext) && is_inside(p, &package.dir))
        });
        if blind {
            roots.insert_package(package.dir.clone());
        }
    }
    python_entry_roots(&mut roots, files);
    roots
}

/// One manifest path value. A value that resolves to a file roots that file; a pattern
/// (`./src/*.ts`) roots the folder it names; a value naming nothing the scan can see (a `bin/`
/// script the scan prunes, a `dist/` build output) could import anything, so the whole package
/// subtree is rooted (`R7`).
fn entry_root(
    roots: &mut RootSet,
    files: &[(String, String)],
    resolver: &EcmaResolver<'_>,
    package: &Package,
    value: &str,
) {
    let rel = value.trim_start_matches("./");
    if let Some(star) = rel.find('*') {
        let head = &rel[..star];
        let dir_part = head.rfind('/').map_or("", |i| &head[..i]);
        let dir = normalize_join(&package.dir, dir_part);
        let present = files
            .iter()
            .any(|(p, _)| is_inside(p, &dir) && !dir.is_empty());
        if present {
            roots.insert_dir(dir);
        } else {
            roots.insert_dir(package.dir.clone());
        }
        return;
    }
    match resolver.resolve(&package.path, &format!("./{rel}")) {
        Some(file) => roots.insert_file(file),
        None => roots.insert_dir(package.dir.clone()),
    }
}

/// Python entry points: `[project.scripts]`, `[project.gui-scripts]`,
/// `[project.entry-points.*]` and `[tool.poetry.scripts]` values, each `pkg.mod:attr`.
fn python_entry_roots(roots: &mut RootSet, files: &[(String, String)]) {
    let path_set: HashSet<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    for (path, content) in files {
        if !(path == "pyproject.toml" || path.ends_with("/pyproject.toml")) {
            continue;
        }
        let Ok(doc) = content.parse::<toml::Table>() else {
            continue;
        };
        let mut targets: Vec<String> = Vec::new();
        let table_at = |keys: &[&str]| -> Option<&toml::Table> {
            let mut cur = &doc;
            for key in keys {
                cur = cur.get(*key)?.as_table()?;
            }
            Some(cur)
        };
        let strings = |t: &toml::Table, out: &mut Vec<String>| {
            out.extend(t.values().filter_map(|v| v.as_str().map(str::to_string)));
        };
        for keys in [
            &["project", "scripts"][..],
            &["project", "gui-scripts"],
            &["tool", "poetry", "scripts"],
        ] {
            if let Some(t) = table_at(keys) {
                strings(t, &mut targets);
            }
        }
        for keys in [
            &["project", "entry-points"][..],
            &["tool", "poetry", "plugins"],
        ] {
            if let Some(groups) = table_at(keys) {
                for group in groups.values().filter_map(|g| g.as_table()) {
                    strings(group, &mut targets);
                }
            }
        }
        for target in targets {
            let module = target.split(':').next().unwrap_or("").trim();
            if module.is_empty() {
                continue;
            }
            if let Some(file) = resolve_python_module(module, &path_set) {
                roots.insert_file(file);
            }
        }
    }
}
