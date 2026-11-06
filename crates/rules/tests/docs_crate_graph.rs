//! The crate graphs and the size claims in the public docs are the real ones.
//!
//! `README.md` carries a compact diagram of the main crates and `PLATFORM.md` carries the full
//! one. Both are generated from `cargo metadata` and drawn as mermaid. A diagram that is only
//! checked by eye rots the first time a crate gains a dependency, so this test reads the
//! workspace `Cargo.toml` files the way cargo does (path dependencies between members) and fails
//! when a drawn arrow is not a real dependency, when a real dependency is missing from the full
//! graph, or when the compact graph no longer implies the same dependencies as the full one.
//!
//! The README also states how many crates the workspace has and floors for the amount of code and
//! the number of tests. The counts must be the real ones and the floors must be true.
//!
//! Only `[dependencies]` edges are drawn (dev-dependencies are test scaffolding), so only those
//! are compared.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel))
        .unwrap_or_else(|e| panic!("cannot read {rel}: {e}"))
}

fn parse_toml(rel: &str) -> toml::Value {
    read(rel)
        .parse::<toml::Value>()
        .unwrap_or_else(|e| panic!("{rel} is not valid TOML: {e}"))
}

/// One workspace member: its directory, its package name and the members it depends on.
struct Member {
    dir: String,
    package: String,
    deps: BTreeSet<String>,
}

/// Every `[dependencies]` table of a manifest: the plain one and any target-specific ones.
fn dependency_tables(manifest: &toml::Value) -> Vec<&toml::value::Table> {
    let mut out = Vec::new();
    if let Some(t) = manifest.get("dependencies").and_then(toml::Value::as_table) {
        out.push(t);
    }
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for cfg in targets.values() {
            if let Some(t) = cfg.get("dependencies").and_then(toml::Value::as_table) {
                out.push(t);
            }
        }
    }
    out
}

fn workspace_members() -> Vec<Member> {
    let root = parse_toml("Cargo.toml");
    let dirs: Vec<String> = root["workspace"]["members"]
        .as_array()
        .expect("workspace.members is a list")
        .iter()
        .map(|v| v.as_str().expect("a member is a path").to_string())
        .collect();
    let packages: BTreeMap<String, String> = dirs
        .iter()
        .map(|dir| {
            let manifest = parse_toml(&format!("{dir}/Cargo.toml"));
            let name = manifest["package"]["name"]
                .as_str()
                .expect("a package has a name")
                .to_string();
            (dir.clone(), name)
        })
        .collect();
    let known: BTreeSet<&String> = packages.values().collect();
    dirs.iter()
        .map(|dir| {
            let manifest = parse_toml(&format!("{dir}/Cargo.toml"));
            let mut deps = BTreeSet::new();
            for table in dependency_tables(&manifest) {
                for (key, spec) in table {
                    // A dependency on a member is a path dependency; `package = "x"` renames it.
                    let Some(spec) = spec.as_table() else {
                        continue;
                    };
                    if !spec.contains_key("path") {
                        continue;
                    }
                    let name = spec
                        .get("package")
                        .and_then(toml::Value::as_str)
                        .unwrap_or(key)
                        .to_string();
                    if known.contains(&name) {
                        deps.insert(name);
                    }
                }
            }
            Member {
                dir: dir.clone(),
                package: packages[dir].clone(),
                deps,
            }
        })
        .collect()
}

/// A mermaid diagram read back from a markdown file: node id to package name, and the arrows.
struct Diagram {
    comment: String,
    nodes: BTreeMap<String, String>,
    edges: BTreeSet<(String, String)>,
}

fn mermaid_blocks(text: &str) -> Vec<Diagram> {
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line.trim() != "```mermaid" {
            continue;
        }
        let mut d = Diagram {
            comment: String::new(),
            nodes: BTreeMap::new(),
            edges: BTreeSet::new(),
        };
        for body in lines.by_ref() {
            let t = body.trim();
            if t == "```" {
                break;
            }
            if let Some(c) = t.strip_prefix("%%") {
                d.comment.push_str(c);
                d.comment.push('\n');
            } else if let Some((from, to)) = t.split_once(" --> ") {
                d.edges
                    .insert((from.trim().to_string(), to.trim().to_string()));
            } else if let Some((id, rest)) = t.split_once("[\"") {
                let label = rest.trim_end_matches("\"]");
                let package = label.split("<br/>").next().unwrap_or(label);
                d.nodes.insert(id.trim().to_string(), package.to_string());
            }
        }
        out.push(d);
    }
    out
}

/// The diagram's arrows as package-name pairs; panics on an arrow naming an undeclared node.
fn package_edges(d: &Diagram, doc: &str) -> BTreeSet<(String, String)> {
    d.edges
        .iter()
        .map(|(a, b)| {
            let name = |id: &String| {
                d.nodes
                    .get(id)
                    .unwrap_or_else(|| panic!("{doc}: an arrow names `{id}`, which is not a node"))
                    .clone()
            };
            (name(a), name(b))
        })
        .collect()
}

fn real_edges(members: &[Member]) -> BTreeSet<(String, String)> {
    members
        .iter()
        .flat_map(|m| m.deps.iter().map(|d| (m.package.clone(), d.clone())))
        .collect()
}

/// Every package reachable from `from` along `edges`.
fn reachable(edges: &BTreeSet<(String, String)>, from: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![from.to_string()];
    while let Some(x) = stack.pop() {
        for (a, b) in edges {
            if *a == x && seen.insert(b.clone()) {
                stack.push(b.clone());
            }
        }
    }
    seen
}

fn only_diagram(doc: &str) -> Diagram {
    let mut blocks = mermaid_blocks(&read(doc));
    assert_eq!(
        blocks.len(),
        1,
        "{doc} must hold exactly one mermaid crate diagram"
    );
    blocks.remove(0)
}

#[test]
fn req_full_crate_graph_is_the_real_dependency_graph() {
    let members = workspace_members();
    let d = only_diagram("PLATFORM.md");
    assert!(
        d.comment.contains("cargo metadata"),
        "PLATFORM.md's diagram must say in a `%%` comment that it was generated from `cargo metadata`"
    );

    let drawn: BTreeSet<String> = d.nodes.values().cloned().collect();
    let real: BTreeSet<String> = members.iter().map(|m| m.package.clone()).collect();
    assert_eq!(
        drawn, real,
        "PLATFORM.md's diagram must draw every workspace package and nothing else"
    );

    let got = package_edges(&d, "PLATFORM.md");
    let want = real_edges(&members);
    let invented: Vec<_> = got.difference(&want).collect();
    let missing: Vec<_> = want.difference(&got).collect();
    assert!(
        invented.is_empty() && missing.is_empty(),
        "PLATFORM.md's crate graph is stale.\n  drawn but not a real dependency: {invented:?}\n  \
         real dependency not drawn: {missing:?}\nRegenerate it from `cargo metadata`."
    );
}

#[test]
fn req_compact_crate_graph_implies_the_same_dependencies_as_the_full_one() {
    let members = workspace_members();
    let d = only_diagram("README.md");
    assert!(
        d.comment.contains("cargo metadata"),
        "README.md's diagram must say in a `%%` comment that it was generated from `cargo metadata`"
    );
    assert!(
        (10..=15).contains(&d.nodes.len()),
        "README.md's diagram is the compact one: 10 to 15 crates, not {}",
        d.nodes.len()
    );

    let real: BTreeSet<String> = members.iter().map(|m| m.package.clone()).collect();
    let shown: BTreeSet<String> = d.nodes.values().cloned().collect();
    assert!(
        shown.is_subset(&real),
        "README.md's diagram names a package that is not in the workspace: {:?}",
        shown.difference(&real).collect::<Vec<_>>()
    );

    let full = real_edges(&members);
    let compact = package_edges(&d, "README.md");
    let invented: Vec<_> = compact.difference(&full).collect();
    assert!(
        invented.is_empty(),
        "README.md's diagram draws an arrow that is not a direct dependency: {invented:?}"
    );

    // The compact graph may drop an arrow another drawn path already implies, but it must
    // still imply exactly the dependencies the full graph has between the crates it shows.
    for from in &shown {
        let want: BTreeSet<String> = reachable(&full, from)
            .intersection(&shown)
            .cloned()
            .collect();
        let got = reachable(&compact, from);
        assert_eq!(
            got, want,
            "README.md's diagram shows `{from}` reaching a different set of the drawn crates than \
             the workspace does. Regenerate it from `cargo metadata`."
        );
    }
}

/// Numbers written directly before one of `nouns`, with the words matched.
fn claims_before(text: &str, nouns: &[&str]) -> Vec<(u64, String)> {
    let toks: Vec<String> = text
        .split_whitespace()
        .flat_map(|raw| {
            raw.split(['-', '/'])
                .map(|p| {
                    p.trim_matches(|c: char| !c.is_alphanumeric() && c != ',')
                        .to_lowercase()
                })
                .collect::<Vec<_>>()
        })
        .filter(|w| !w.is_empty())
        .collect();
    let mut out = Vec::new();
    for i in 1..toks.len() {
        if !nouns.contains(&toks[i].as_str()) {
            continue;
        }
        let digits = toks[i - 1].replace(',', "");
        if let Ok(n) = digits.parse::<u64>() {
            out.push((n, format!("{} {}", toks[i - 1], toks[i])));
        }
    }
    out
}

#[test]
fn req_docs_state_the_real_crate_counts() {
    let members = workspace_members();
    let crates = members
        .iter()
        .filter(|m| m.dir.starts_with("crates/"))
        .count() as u64;
    let tools = members
        .iter()
        .filter(|m| m.dir.starts_with("tools/"))
        .count();
    assert_eq!(
        tools, 1,
        "the docs say \"one maintainer tool\"; the workspace has {tools} under tools/"
    );
    let total = members.len() as u64;

    let sentence = format!("{crates} crates and one maintainer tool");
    let mut problems = Vec::new();
    for doc in ["README.md", "PLATFORM.md"] {
        let text = read(doc);
        if !text.contains(&sentence) {
            problems.push(format!("{doc} must say \"{sentence}\" verbatim"));
        }
        for (n, ctx) in claims_before(&text, &["crates"]) {
            if n != crates {
                problems.push(format!(
                    "{doc}: says \"{ctx}\" but the workspace has {crates} crates"
                ));
            }
        }
        for (n, ctx) in claims_before(&text, &["members", "packages"]) {
            if n != total {
                problems.push(format!(
                    "{doc}: says \"{ctx}\" but the workspace has {total} members"
                ));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "crate counts are stale:\n  {}",
        problems.join("\n  ")
    );
}

/// Every `.rs` file under `crates/` and `tools/`, skipping build output.
fn rust_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for top in ["crates", "tools"] {
        let mut stack = vec![repo_root().join(top)];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    if name != "target" {
                        stack.push(path);
                    }
                } else if kind.is_file() && name.ends_with(".rs") {
                    out.push(path);
                }
            }
        }
    }
    out
}

/// "10,000" for 10000.
fn with_commas(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The floors the README states. Raise them when the code grows past a rounder number; the test
/// fails when the code is smaller than the claim, never when it is larger.
const TESTS_FLOOR: u64 = 10_000;
const LINES_FLOOR: u64 = 500_000;

#[test]
fn req_readme_size_claims_are_floors_the_code_meets() {
    let mut lines = 0u64;
    let mut tests = 0u64;
    for path in rust_files() {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        lines += text.lines().count() as u64;
        tests += text
            .lines()
            .filter(|l| {
                let t = l.trim();
                t == "#[test]" || t.starts_with("#[tokio::test")
            })
            .count() as u64;
    }
    assert!(
        lines >= LINES_FLOOR,
        "the README says over {} lines of Rust; the workspace has {lines}",
        with_commas(LINES_FLOOR)
    );
    assert!(
        tests >= TESTS_FLOOR,
        "the README says {}+ automated tests; the workspace has {tests} test attributes",
        with_commas(TESTS_FLOOR)
    );

    let readme = read("README.md");
    for claim in [
        format!("over {} lines of Rust", with_commas(LINES_FLOOR)),
        format!("{}+ automated tests", with_commas(TESTS_FLOOR)),
    ] {
        assert!(
            readme.contains(&claim),
            "README.md must state \"{claim}\" (the floor this test checks)"
        );
    }
}
