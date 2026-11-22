#![allow(clippy::unwrap_used)]
//! Guard: no code or client text outside `camerata_api_types::package_paths` spells a package
//! path as a literal (ARCH-SHARED-CONSTANT-1). The layout of the client package is a public
//! interface, so every path is written once and every other place names the file through the
//! constant or the `pkg_*!` macro.
//!
//! Scanned: every non-test `.rs` file of every crate under `crates/`, the client text templates
//! and the typst templates. A file's test module (from the first `#[cfg(test)]`, always at the
//! end of the file), a `tests` directory, a file named `*tests.rs` or `*e2e*.rs` and comment
//! lines are not scanned: tests assert paths on purpose. The module that owns the paths is the
//! one exception.

use std::path::{Path, PathBuf};

use regex::Regex;

fn repo_crates() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if path.is_dir() {
            if name == "target" || name == "tests" || name.starts_with('.') {
                continue;
            }
            files_under(&path, out);
        } else if name.ends_with(".rs") || name.ends_with(".typ") {
            out.push(path);
        }
    }
}

fn is_test_file(path: &Path) -> bool {
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    name.ends_with("tests.rs") || name == "tests.rs" || name.contains("e2e")
}

/// The path spellings that must exist only in the paths module.
fn forbidden() -> Vec<(&'static str, Regex)> {
    let list = [
        ("findings.json", r"(?:^|[^-\w/.])findings\.json"),
        ("decisions.json", r"(?:^|[^-\w/.])decisions\.json(?:[^l]|$)"),
        ("manifest.json", r"(?:^|[^-\w/._])manifest\.json"),
        ("README.txt", r"(?:^|[^-\w/.])README\.txt"),
        ("a json/ path", r#"(?:^|[^-\w/.:])json/"#),
        ("a csv/ path", r#"(?:^|[^-\w/.:])csv/"#),
    ];
    list.into_iter()
        .map(|(name, pattern)| (name, Regex::new(pattern).unwrap()))
        .collect()
}

/// True when the line after `index` opens a module (the test module at the end of a file), as
/// opposed to a `#[cfg(test)]` import or helper in the middle of the code.
fn next_is_module(lines: &[&str], index: usize) -> bool {
    lines.get(index + 1).is_some_and(|next| {
        let next = next.trim_start();
        next.starts_with("mod ")
            || next.starts_with("pub mod ")
            || next.starts_with("pub(crate) mod ")
    })
}

/// Every offending `file:line` in `text`, a file's non-test, non-comment code and text.
fn offences(text: &str, forbidden: &[(&'static str, Regex)]) -> Vec<(usize, &'static str)> {
    let mut found = Vec::new();
    let mut in_block_comment = false;
    let lines: Vec<&str> = text.lines().collect();
    for (index, line) in lines.iter().copied().enumerate() {
        if line.trim_start().starts_with("#[cfg(test)]") && next_is_module(&lines, index) {
            break;
        }
        let trimmed = line.trim_start();
        if in_block_comment {
            if trimmed.contains("*/") {
                in_block_comment = false;
            }
            continue;
        }
        if trimmed.starts_with("/*") && !trimmed.contains("*/") {
            in_block_comment = true;
            continue;
        }
        if trimmed.starts_with("//") {
            continue;
        }
        for (name, pattern) in forbidden {
            if pattern.is_match(line) {
                found.push((index + 1, *name));
            }
        }
    }
    found
}

#[test]
fn no_package_path_is_spelled_outside_the_paths_module() {
    let mut files = Vec::new();
    files_under(&repo_crates(), &mut files);
    // Client text templates live beside the code.
    let forbidden = forbidden();
    let mut report = Vec::new();
    let mut scanned = 0;
    for file in files {
        let display = file
            .strip_prefix(repo_crates())
            .unwrap()
            .display()
            .to_string();
        let display = display.replace("../", "");
        if display.ends_with("api-types/src/package_paths.rs") || is_test_file(&file) {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        scanned += 1;
        for (line, name) in offences(&text, &forbidden) {
            report.push(format!("{display}:{line}: spells {name} as a literal"));
        }
    }
    assert!(
        scanned > 100,
        "the scan found only {scanned} files: the crate layout moved"
    );
    assert!(
        report.is_empty(),
        "package paths are named through camerata_api_types::package_paths, never spelled:\n{}",
        report.join("\n")
    );
}

#[test]
fn the_guard_catches_a_literal_and_ignores_comments_and_the_test_module() {
    let forbidden = forbidden();
    let code = "let a = \"findings.json\";\n// findings.json in a comment\nlet b = \"json/x.json\";\nlet ok = \"all-findings\";\n#[cfg(test)]\nmod tests { let t = \"findings.json\"; }\n";
    let hits = offences(code, &forbidden);
    assert_eq!(hits, vec![(1, "findings.json"), (3, "a json/ path")]);
    let prose = "\"the rows of json/inspection.json\"";
    assert!(!offences(prose, &forbidden).is_empty());
    assert!(offences(
        "concat!(\"a \", camerata_api_types::pkg_inspection_json!())",
        &forbidden
    )
    .is_empty());
}
