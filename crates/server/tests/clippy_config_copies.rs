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

/// Lines of `src` that build a default `reqwest` client. `Client::default` is a trait method, so
/// clippy cannot ban it by path; this scan stands in for that ban.
fn default_client_hits(src: &str) -> Vec<String> {
    src.lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.starts_with("//")
                && (l.contains("Client::default") || l.contains("Client :: default"))
        })
        .map(str::to_string)
        .collect()
}

fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn the_default_client_scan_catches_a_planted_use() {
    assert_eq!(
        default_client_hits("let c = reqwest::Client::default();").len(),
        1
    );
    assert_eq!(
        default_client_hits("    let c = Client::default();").len(),
        1
    );
    assert!(default_client_hits("let c = http::build_client();").is_empty());
    assert!(default_client_hits("// reqwest::Client::default is banned").is_empty());
}

#[test]
fn no_source_file_builds_a_default_reqwest_client() {
    let mut files = Vec::new();
    for rel in ["../llm/src", "../server/src"] {
        rust_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join(rel), &mut files);
    }
    assert!(!files.is_empty(), "the scan found no source files");
    let mut found = Vec::new();
    for f in files {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        if name.ends_with("_tests.rs") {
            continue;
        }
        let src = std::fs::read_to_string(&f).unwrap();
        // Everything from the first `#[cfg(test)]` on is test code.
        let non_test = src.split("#[cfg(test)]").next().unwrap();
        for hit in default_client_hits(non_test) {
            found.push(format!("{}: {}", f.display(), hit.trim()));
        }
    }
    assert!(
        found.is_empty(),
        "build the client through the shared builder, not Client::default:\n{}",
        found.join("\n")
    );
}
