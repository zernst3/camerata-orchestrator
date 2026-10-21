//! Rust liveness. A `.rs` file is compiled only when a `mod` declaration reaches it from a
//! crate root, so Rust is decided by the `mod` closure, not by the import graph: `use crate::x`
//! has no effect on whether `x.rs` is part of the build, and the import graph carries no `mod`
//! edges at all.
//!
//! Resolution follows `rustc`'s rules. A crate root (`lib.rs`, `main.rs`, a Cargo target) or a
//! `mod.rs` file owns its own directory; any other file `a.rs` owns `a/`. An inline
//! `mod m { .. }` block appends `m` to that directory. `mod x;` loads `<owning>/x.rs`, else
//! `<owning>/x/mod.rs`. A `#[path = ".."]` attribute is relative to the declaring file's
//! directory, or to the inline blocks' directory when the declaration sits inside one.
//!
//! A crate is skipped entirely (nothing is reported in it) when its roots or its declarations
//! are not visible to this pass: no `lib.rs`/`main.rs` in the scan, a `[[bin]]` target or a
//! `src/bin` path in `Cargo.toml` (a bin target may own `#[path]` declarations), or a
//! dependency on `automod`, `mod_use` or `include_dir` (macro-generated declarations).

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use crate::extract::resolver::{directory_of, normalize_join, rust_src_root};
use crate::extract::rust_syn::{mod_decls, ModDecl};

/// Manifest text that makes a crate's declarations or roots invisible to this pass.
const OPAQUE_MANIFEST_MARKERS: &[&str] =
    &["[[bin]]", "src/bin", "automod", "mod_use", "include_dir"];

#[derive(Debug, Default)]
pub(super) struct RustPlan {
    /// `src` directories of the crates this pass can decide.
    checked: BTreeSet<String>,
    /// Crate roots of the checked crates (files that own their directory and are compiled).
    roots: BTreeSet<String>,
    /// For each file with `mod` declarations, the files they resolve to.
    targets: HashMap<String, Vec<String>>,
    /// The reverse of `targets`.
    declarers: HashMap<String, Vec<String>>,
}

fn is_rust(path: &str) -> bool {
    path.ends_with(".rs")
}

fn file_stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.strip_suffix(".rs").unwrap_or(name)
}

impl RustPlan {
    pub(super) fn build(files: &[(String, String)]) -> Self {
        let path_set: HashSet<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        let text_of: HashMap<&str, &str> = files
            .iter()
            .map(|(p, c)| (p.as_str(), c.as_str()))
            .collect();
        let src_roots: BTreeSet<String> = files
            .iter()
            .filter(|(p, _)| is_rust(p))
            .filter_map(|(p, _)| rust_src_root(p))
            .collect();

        let mut plan = Self::default();
        for src_root in src_roots {
            let lib = format!("{src_root}/lib.rs");
            let main = format!("{src_root}/main.rs");
            if !path_set.contains(lib.as_str()) && !path_set.contains(main.as_str()) {
                continue; // roots invisible: today `src/bin/`-only crates are pruned
            }
            let crate_dir = directory_of(&src_root).to_string();
            let manifest_path = join(&crate_dir, "Cargo.toml");
            let manifest = text_of.get(manifest_path.as_str()).copied().unwrap_or("");
            if OPAQUE_MANIFEST_MARKERS.iter().any(|m| manifest.contains(m)) {
                continue;
            }
            let mut roots: Vec<String> = [lib, main, join(&crate_dir, "build.rs")]
                .into_iter()
                .filter(|p| path_set.contains(p.as_str()))
                .collect();
            for (path, _) in files {
                if !is_rust(path) {
                    continue;
                }
                let is_target_root = ["tests", "examples", "benches"].iter().any(|dir| {
                    let base = join(&crate_dir, dir);
                    directory_of(path) == base
                        || (path.ends_with("/main.rs") && directory_of(directory_of(path)) == base)
                }) || {
                    let bin = format!("{src_root}/bin");
                    directory_of(path) == bin
                        || (path.ends_with("/main.rs") && directory_of(directory_of(path)) == bin)
                };
                if is_target_root {
                    roots.push(path.clone());
                }
            }
            roots.extend(cargo_path_roots(manifest, &crate_dir, &path_set));
            plan.checked.insert(src_root);
            plan.roots.extend(roots);
        }

        for (path, content) in files {
            if !is_rust(path) || !content.contains("mod ") {
                continue;
            }
            let is_root = plan.roots.contains(path.as_str());
            let mut resolved: Vec<String> = mod_decls(content)
                .iter()
                .filter_map(|d| resolve_decl(path, is_root, d, &path_set))
                .collect();
            resolved.sort();
            resolved.dedup();
            for target in &resolved {
                plan.declarers
                    .entry(target.clone())
                    .or_default()
                    .push(path.clone());
            }
            if !resolved.is_empty() {
                plan.targets.insert(path.clone(), resolved);
            }
        }
        plan
    }

    /// Whether `path` is a Rust file in a crate this pass can decide (it sits under that
    /// crate's `src/`).
    pub(super) fn is_checked(&self, path: &str) -> bool {
        is_rust(path) && rust_src_root(path).is_some_and(|root| self.checked.contains(&root))
    }

    /// Every file the checked crates compile: the closure of `mod` declarations from the crate
    /// roots plus `extra_roots` (files alive by other evidence).
    pub(super) fn declared(&self, extra_roots: &[String]) -> HashSet<String> {
        let mut seen: HashSet<String> = self.roots.iter().cloned().collect();
        let mut queue: VecDeque<String> = self.roots.iter().cloned().collect();
        for root in extra_roots {
            if is_rust(root) && seen.insert(root.clone()) {
                queue.push_back(root.clone());
            }
        }
        while let Some(file) = queue.pop_front() {
            for target in self.targets.get(&file).into_iter().flatten() {
                if seen.insert(target.clone()) {
                    queue.push_back(target.clone());
                }
            }
        }
        seen
    }

    /// The crate roots of the checked crates.
    pub(super) fn roots(&self) -> impl Iterator<Item = &String> {
        self.roots.iter()
    }

    /// The files whose `mod` statements resolve to `path`.
    pub(super) fn declarers(&self, path: &str) -> &[String] {
        self.declarers.get(path).map_or(&[], Vec::as_slice)
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// `path =` entries of `[lib]`, `[[test]]`, `[[example]]`, `[[bench]]` and `package.build`.
/// A manifest that does not parse names none (the text checks above still apply to it).
fn cargo_path_roots(manifest: &str, crate_dir: &str, path_set: &HashSet<&str>) -> Vec<String> {
    let Ok(doc) = manifest.parse::<toml::Table>() else {
        return Vec::new();
    };
    let mut paths: Vec<String> = Vec::new();
    let path_of = |t: &toml::Value| t.get("path")?.as_str().map(str::to_string);
    if let Some(lib) = doc.get("lib") {
        paths.extend(path_of(lib));
    }
    for section in ["test", "example", "bench"] {
        if let Some(items) = doc.get(section).and_then(|v| v.as_array()) {
            paths.extend(items.iter().filter_map(path_of));
        }
    }
    if let Some(build) = doc
        .get("package")
        .and_then(|p| p.get("build"))
        .and_then(|b| b.as_str())
    {
        paths.push(build.to_string());
    }
    paths
        .iter()
        .map(|p| normalize_join(crate_dir, p))
        .filter(|p| path_set.contains(p.as_str()))
        .collect()
}

/// The file a `mod` declaration in `file` loads, when the scan holds it.
fn resolve_decl(
    file: &str,
    file_is_root: bool,
    decl: &ModDecl,
    path_set: &HashSet<&str>,
) -> Option<String> {
    let dir = directory_of(file);
    let owns_dir = file_is_root || file.ends_with("/mod.rs") || file == "mod.rs";
    let mut owning = if owns_dir {
        dir.to_string()
    } else {
        join(dir, file_stem(file))
    };
    for parent in &decl.inline_parents {
        owning = join(&owning, parent);
    }
    if let Some(attr) = &decl.path_attr {
        let base = if decl.inline_parents.is_empty() {
            dir
        } else {
            owning.as_str()
        };
        let target = normalize_join(base, attr);
        return path_set.contains(target.as_str()).then_some(target);
    }
    let as_file = join(&owning, &format!("{}.rs", decl.name));
    if path_set.contains(as_file.as_str()) {
        return Some(as_file);
    }
    let as_mod = join(&owning, &format!("{}/mod.rs", decl.name));
    path_set.contains(as_mod.as_str()).then_some(as_mod)
}
