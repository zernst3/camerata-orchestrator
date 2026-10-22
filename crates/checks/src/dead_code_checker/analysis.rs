//! The liveness computation: which candidate files can be reached from the entry points,
//! the files that are alive by definition, and the mention guard, run to a fixed point.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::extract::paths::emitted_js_paths;
use crate::extract::resolver::{build_import_graph, reachable_from_all, ResolvedImport};

use super::conventions::Conventions;
use super::dynamic::dynamic_roots;
use super::emit::{self, DeadFile};
use super::exclusions::{
    has_generated_marker, has_keep_marker, is_config_shaped, is_declaration_file, is_test_file,
};
use super::manifests::{manifest_roots, ManifestDirs, Packages};
use super::mentions::{is_always_mentioned, key_of, MentionIndex};
use super::rust_pass::RustPlan;
use super::{family_of, Family};

/// Whether a file with this path and text is NEVER reported (`G1` to `G3`, `G11`, `G12`).
fn is_excluded(path: &str, content: &str) -> bool {
    is_test_file(path)
        || is_declaration_file(path)
        || is_config_shaped(path)
        || has_generated_marker(content)
        || has_keep_marker(content)
}

/// A script that runs by itself: a shebang line, or a Python `__main__` guard.
fn is_executable(content: &str, family: Family) -> bool {
    if content.starts_with("#!") {
        return true;
    }
    family == Family::Python && has_main_guard(content)
}

fn has_main_guard(content: &str) -> bool {
    content.match_indices("__name__").any(|(i, _)| {
        let tail = &content[i + "__name__".len()..];
        // `tail` may hold multibyte text: floor the window end to a character boundary.
        let window = &tail[..tail.floor_char_boundary(40)];
        window.contains("\"__main__\"") || window.contains("'__main__'")
    })
}

/// Source files no exclusion covers: the files that may be reported. Rust files count only in
/// a crate whose roots and declarations the Rust pass can see.
fn find_candidates(
    files: &[(String, String)],
    emitted: &HashSet<&str>,
    rust: &RustPlan,
) -> Vec<usize> {
    let mut candidates = Vec::new();
    for (i, (path, content)) in files.iter().enumerate() {
        let eligible = match family_of(path) {
            Some(Family::Ecma | Family::Python) => !emitted.contains(path.as_str()),
            Some(Family::Rust) => rust.is_checked(path),
            None => false,
        };
        if eligible && !is_excluded(path, content) {
            candidates.push(i);
        }
    }
    candidates
}

pub(super) fn analyze(files: &[(String, String)]) -> Vec<crate::arch_checker::ArchViolation> {
    let emitted = emitted_js_paths(files);
    let rust = RustPlan::build(files);
    let candidates = find_candidates(files, &emitted, &rust);
    if candidates.is_empty() {
        return Vec::new();
    }
    let is_candidate: HashSet<usize> = candidates.iter().copied().collect();

    // The import graph over EVERY file: tests and configs keep things alive. A committed
    // build-output copy is not an importer, and Rust `use` edges say nothing about whether a
    // file is compiled.
    let graph: Vec<ResolvedImport> = build_import_graph(files)
        .into_iter()
        .filter(|e| {
            !emitted.contains(e.from_file.as_str()) && family_of(&e.from_file) != Some(Family::Rust)
        })
        .collect();

    // Roots: every context file, the Rust crate roots, and each candidate that is alive by
    // itself.
    let mut roots: HashSet<String> = HashSet::new();
    for (i, (path, _)) in files.iter().enumerate() {
        if !is_candidate.contains(&i) && !emitted.contains(path.as_str()) {
            roots.insert(path.clone());
        }
    }
    roots.extend(rust.roots().cloned());
    let packages = Packages::load(files);
    let manifest = manifest_roots(files, &packages);
    let conventions = Conventions::new(files, &packages);
    let dynamic = dynamic_roots(files, &ManifestDirs::load(files));
    let key_of_candidate: HashMap<usize, String> = candidates
        .iter()
        .map(|&i| (i, key_of(&files[i].0)))
        .collect();
    for &i in &candidates {
        let (path, content) = &files[i];
        let family = family_of(path).unwrap_or(Family::Ecma);
        let in_manifest_package = family == Family::Ecma
            && packages
                .nearest(path)
                .is_some_and(|p| manifest.contains_package(&p.dir));
        if is_always_mentioned(&key_of_candidate[&i])
            || is_executable(content, family)
            || manifest.contains(path)
            || dynamic.contains(path)
            || conventions.is_convention_path(path, family)
            || in_manifest_package
        {
            roots.insert(path.clone());
        }
    }

    // The mention guard, to a fixed point: a candidate is alive when its name appears in a file
    // that is itself alive. A mention inside an unreachable file proves nothing.
    let pending: Vec<usize> = candidates
        .iter()
        .copied()
        .filter(|&i| !roots.contains(&files[i].0))
        .collect();
    let keys: Vec<String> = pending
        .iter()
        .map(|i| key_of_candidate[i].clone())
        .collect();
    let index = MentionIndex::build(files, &keys);

    // Alive = reachable over imports from the roots (ECMAScript and Python), plus everything
    // the `mod` closure compiles from the Rust roots.
    let liveness = |roots: &HashSet<String>| -> HashSet<String> {
        let mut live = reachable_from_all(&graph, roots.iter().map(String::as_str));
        let rust_roots: Vec<String> = roots
            .iter()
            .filter(|p| p.ends_with(".rs"))
            .cloned()
            .collect();
        live.extend(rust.declared(&rust_roots));
        live
    };
    let mut live = liveness(&roots);
    loop {
        let mut grew = false;
        for &i in &pending {
            let path = &files[i].0;
            if live.contains(path) || roots.contains(path) {
                continue;
            }
            let mentioned = index
                .mentioners(&key_of_candidate[&i])
                .iter()
                .any(|&m| m != i && live.contains(&files[m].0));
            if mentioned {
                roots.insert(path.clone());
                grew = true;
            }
        }
        if !grew {
            break;
        }
        live = liveness(&roots);
    }

    let dead_paths: BTreeSet<&str> = candidates
        .iter()
        .map(|&i| files[i].0.as_str())
        .filter(|p| !live.contains(*p))
        .collect();
    let mut dead: Vec<DeadFile> = Vec::new();
    for &i in &candidates {
        let (path, _) = &files[i];
        if !dead_paths.contains(path.as_str()) {
            continue;
        }
        let family = family_of(path).unwrap_or(Family::Ecma);
        let key = key_of_candidate[&i].clone();
        // What refers to this file: importers (ECMAScript, Python) or `mod` declarations
        // (Rust). Every one of them is itself unreachable, or the file would be alive.
        let referrers: BTreeSet<String> = if family == Family::Rust {
            rust.declarers(path)
                .iter()
                .filter(|p| dead_paths.contains(p.as_str()))
                .cloned()
                .collect()
        } else {
            graph
                .iter()
                .filter(|e| e.to_file == *path && dead_paths.contains(e.from_file.as_str()))
                .map(|e| e.from_file.clone())
                .collect()
        };
        let mentioners: BTreeSet<String> = index
            .mentioners(&key)
            .iter()
            .filter(|&&m| m != i)
            .map(|&m| files[m].0.clone())
            .filter(|p| !referrers.contains(p))
            .collect();
        dead.push(DeadFile {
            path: path.clone(),
            family,
            key,
            importers: referrers.into_iter().collect(),
            mentioners: mentioners.into_iter().collect(),
        });
    }
    let sources: BTreeSet<String> = files
        .iter()
        .map(|(p, _)| p)
        .filter(|p| family_of(p).is_some() && !emitted.contains(p.as_str()))
        .cloned()
        .collect();
    emit::findings(&dead, &sources)
}
