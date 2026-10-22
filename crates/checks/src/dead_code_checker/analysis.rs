//! The liveness computation: which candidate files can be reached from the entry points,
//! the files that are alive by definition, and the mention guard, run to a fixed point.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use crate::extract::paths::emitted_js_paths;
use crate::extract::resolver::{build_import_graph, ResolvedImport};

use super::conventions::Conventions;
use super::dynamic::dynamic_roots;
use super::emit::{self, DeadFile};
use super::exclusions::{
    has_generated_marker, has_keep_marker, is_config_shaped, is_declaration_file,
    is_generated_path, is_test_file,
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
        || is_generated_path(path)
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
    // the `mod` closure compiles from the Rust roots, plus every candidate named in a live file.
    // One worklist: a file is expanded once, when it becomes live, along its import edges, its
    // `mod` declarations and the candidates its text names, so the cost is linear however long
    // a chain of mentions is.
    let mut edges: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in &graph {
        edges
            .entry(edge.from_file.as_str())
            .or_default()
            .push(edge.to_file.as_str());
    }
    // path -> the pending candidates its text names.
    let mut names_candidates: HashMap<&str, Vec<&str>> = HashMap::new();
    for &i in &pending {
        for &m in index.mentioners(&key_of_candidate[&i]) {
            if m != i {
                names_candidates
                    .entry(files[m].0.as_str())
                    .or_default()
                    .push(files[i].0.as_str());
            }
        }
    }
    let mut live: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    for root in &roots {
        if live.insert(root.clone()) {
            queue.push_back(root.clone());
        }
    }
    while let Some(file) = queue.pop_front() {
        let imported = edges.get(file.as_str()).into_iter().flatten().copied();
        let declared = rust.targets_of(&file).iter().map(String::as_str);
        let named = names_candidates
            .get(file.as_str())
            .into_iter()
            .flatten()
            .copied();
        for next in imported.chain(declared).chain(named) {
            if !live.contains(next) {
                live.insert(next.to_string());
                queue.push_back(next.to_string());
            }
        }
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
    let live_assets = live_assets_under_dead_files(files, &emitted, &dead_paths, &live);
    emit::findings(&dead, &sources, &live_assets)
}

/// Non-source files (stylesheets, data, images, templates) in a folder that holds a dead file
/// and that something live still names. The name test is the mention guard's, applied to the
/// asset: a generic or very short name counts as named, a name found in a live file counts, a
/// name found only in unreachable files (or nowhere) does not.
fn live_assets_under_dead_files(
    files: &[(String, String)],
    emitted: &HashSet<&str>,
    dead_paths: &BTreeSet<&str>,
    live: &HashSet<String>,
) -> BTreeSet<String> {
    let mut dead_dirs: HashSet<&str> = HashSet::new();
    for path in dead_paths {
        let mut dir = *path;
        while let Some(i) = dir.rfind('/') {
            dir = &dir[..i];
            dead_dirs.insert(dir);
        }
    }
    let assets: Vec<usize> = files
        .iter()
        .enumerate()
        .filter(|(_, (p, _))| family_of(p).is_none() && !emitted.contains(p.as_str()))
        .filter(|(_, (p, _))| {
            let mut dir = p.as_str();
            while let Some(i) = dir.rfind('/') {
                dir = &dir[..i];
                if dead_dirs.contains(dir) {
                    return true;
                }
            }
            false
        })
        .map(|(i, _)| i)
        .collect();
    let keys: Vec<String> = assets.iter().map(|&i| key_of(&files[i].0)).collect();
    let index = MentionIndex::build(files, &keys);
    assets
        .iter()
        .zip(&keys)
        .filter(|(&i, key)| {
            is_always_mentioned(key)
                || index
                    .mentioners(key)
                    .iter()
                    .any(|&m| m != i && live.contains(&files[m].0))
        })
        .map(|(&i, _)| files[i].0.clone())
        .collect()
}
