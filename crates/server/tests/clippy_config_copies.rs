//! A crate-level `clippy.toml` REPLACES the workspace one (clippy does not merge them), so
//! `crates/llm/clippy.toml` and `crates/server/clippy.toml` each carry a full copy of the root
//! `clippy.toml` plus the `reqwest` ban block. This gate keeps the copies from drifting: a copy
//! equals the root file once its leading comment and its `reqwest::` ban lines are removed, and
//! both copies carry the same ban lines.

#![allow(clippy::unwrap_used)]

use std::path::Path;

/// The lines of `copy` that are `reqwest` bans, and `copy` with its leading comment block and
/// those lines removed. Fails when the lines left over are not exactly `root`.
fn check_copy(root: &str, copy: &str) -> Result<Vec<String>, String> {
    let mut bans = Vec::new();
    let mut kept = Vec::new();
    let mut in_header = true;
    for line in copy.lines() {
        if in_header && line.starts_with('#') {
            continue;
        }
        in_header = false;
        if line.trim_start().starts_with("{ path = \"reqwest::") {
            bans.push(line.to_string());
        } else {
            kept.push(line);
        }
    }
    let shared = kept.join("\n");
    if shared.trim_end() == root.trim_end() {
        Ok(bans)
    } else {
        Err(format!(
            "the copy minus its header and reqwest bans differs from the root:\n--- root\n{root}\n--- copy\n{shared}"
        ))
    }
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).unwrap()
}

#[test]
fn each_crate_clippy_toml_is_the_root_plus_exactly_the_ban_block() {
    let root = read("../../clippy.toml");
    let llm = check_copy(&root, &read("../llm/clippy.toml")).unwrap();
    let server = check_copy(&root, &read("../server/clippy.toml")).unwrap();
    assert_eq!(llm, server, "the two copies must carry the same ban block");
    // `Client::default` is a trait method (`Default::default`), which clippy cannot ban by a
    // type path, so only the two inherent entry points are listed.
    for banned in ["reqwest::Client::new", "reqwest::get"] {
        assert!(
            llm.iter().any(|l| l.contains(&format!("\"{banned}\""))),
            "the ban block must ban {banned}: {llm:?}"
        );
    }
}

const ROOT: &str = "allow-unwrap-in-tests = true\n\ndisallowed-methods = [\n    { path = \"a::b\", reason = \"x\" },\n]\n";

fn copy_with(extra: &str, root_edit: Option<(&str, &str)>) -> String {
    let mut body = ROOT.to_string();
    if let Some((from, to)) = root_edit {
        body = body.replace(from, to);
    }
    format!(
        "# header comment\n# second line\n{}",
        body.replace("]\n", &format!("{extra}]\n"))
    )
}

#[test]
fn a_faithful_copy_passes_and_returns_its_ban_lines() {
    let copy = copy_with("    { path = \"reqwest::get\", reason = \"r\" },\n", None);
    assert_eq!(
        check_copy(ROOT, &copy).unwrap(),
        vec!["    { path = \"reqwest::get\", reason = \"r\" },".to_string()]
    );
}

#[test]
fn a_copy_whose_shared_part_drifted_from_the_root_fails() {
    let copy = copy_with(
        "    { path = \"reqwest::get\", reason = \"r\" },\n",
        Some(("a::b", "a::c")),
    );
    assert!(check_copy(ROOT, &copy).is_err());
}

#[test]
fn a_copy_with_an_extra_non_reqwest_line_fails() {
    let copy = copy_with(
        "    { path = \"std::mem::forget\", reason = \"r\" },\n",
        None,
    );
    assert!(check_copy(ROOT, &copy).is_err());
}
