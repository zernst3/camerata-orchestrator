#![allow(clippy::unwrap_used)]
//! The build script's lists of workspace crates to watch (`build.rs`) must equal each package's
//! normal path-dependency graph, so a new workspace dependency cannot be missing from the list
//! (an edit in it would leave the dirty flag stale) and an unrelated crate is not watched (it
//! would force a rebuild).

#[allow(dead_code)]
#[path = "../build.rs"]
mod build_identity;

use std::collections::{BTreeMap, BTreeSet};

fn closure(root: &str) -> BTreeSet<String> {
    let out = std::process::Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    // name -> (folder name under crates/, normal path dependencies' names)
    let mut graph: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for p in meta["packages"].as_array().unwrap() {
        let manifest = p["manifest_path"].as_str().unwrap();
        let folder = std::path::Path::new(manifest)
            .parent()
            .and_then(|d| d.file_name())
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let deps = p["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| d["kind"].is_null() && d["path"].is_string())
            .map(|d| d["name"].as_str().unwrap().to_string())
            .collect();
        graph.insert(p["name"].as_str().unwrap().to_string(), (folder, deps));
    }
    let mut seen = BTreeSet::new();
    let mut stack = vec![root.to_string()];
    while let Some(name) = stack.pop() {
        let Some((folder, deps)) = graph.get(&name) else {
            continue;
        };
        if seen.insert(folder.clone()) {
            stack.extend(deps.iter().cloned());
        }
    }
    seen
}

fn listed(pkg: &str) -> BTreeSet<String> {
    build_identity::watched_crates(pkg)
        .unwrap()
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

#[test]
fn the_server_watch_list_is_its_workspace_dependency_graph() {
    assert_eq!(listed("camerata-server"), closure("camerata-server"));
}

#[test]
fn the_app_watch_list_is_its_workspace_dependency_graph() {
    assert_eq!(listed("camerata-ui"), closure("camerata-ui"));
}
