#![allow(clippy::unwrap_used)]
//! Source guard (#560): no export or review path reads the live rule library.
//!
//! Only the inspection is pinned to an engine build. The rules a client package carries, and the
//! rules a review screen shows, are the ones the inspection ran with: its run's rule snapshot,
//! read through `crate::export_rules` (the app) or `run_store::rule_snapshot` (the command line).
//! This test makes that mechanical: it reads the export and review sources and fails when any of
//! them names the live library's loader or its location.
//!
//! Scanned: every non-test line of the export and review modules below, and the bodies of the
//! named route functions in files that also hold inspection code. Not scanned: comment lines, a
//! `*tests.rs` file, a `tests` folder, and a `#[cfg(test)]` item (to its closing brace or `;`).
//! A new export or review path joins a list here, on purpose, in a change someone reads.

use std::path::{Path, PathBuf};

/// The spellings of the live library: its loaders and where it lives.
const LIVE_LIBRARY: &[&str] = &[
    "corpus_path(",
    "load_corpus_lenient(",
    "load_corpus(",
    "load_corpus_for_scan(",
    "split_scannable_rules",
    "DEFAULT_CORPUS_PATH",
    "CAMERATA_CORPUS_PATH",
];

/// Whole modules (a file, or a folder when it ends in `/`) that are export or review code only.
const EXPORT_AND_REVIEW_MODULES: &[&str] = &[
    "crates/server/src/export_rules.rs",
    "crates/server/src/run_store/rule_snapshot.rs",
    "crates/server/src/inspections/rule_snapshots.rs",
    "crates/server/src/report_export.rs",
    "crates/server/src/report_export/",
    "crates/server/src/xlsx_export.rs",
    "crates/server/src/xlsx_export/",
    "crates/server/src/release_gate.rs",
    "crates/server/src/release_gate/",
    "crates/server/src/review_view.rs",
    "crates/server/src/review_view/",
    "crates/server/src/rewrite_advisory.rs",
    "crates/server/src/routes/review_routes.rs",
    "crates/server/src/routes/rewrite_advisory_route.rs",
];

/// Export and review functions that live beside inspection code: (file, function names).
const EXPORT_AND_REVIEW_FUNCTIONS: &[(&str, &[&str])] = &[
    (
        "crates/server/src/lib.rs",
        &[
            "export_audit_report",
            "export_product",
            "export_triage_log",
            "gated_release",
            "prepare_export_options",
            "reviewed_for_export",
        ],
    ),
    (
        "crates/server/src/routes/onboarding.rs",
        &["onboard_finding_fix"],
    ),
    (
        "crates/cli/src/inspect_cmd.rs",
        &[
            "release_product_export",
            "build_product_export_bytes",
            "write_triage_log_beside_zip",
        ],
    ),
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn rs_files(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_file() {
        out.push(path.to_path_buf());
        return;
    }
    for entry in std::fs::read_dir(path).unwrap() {
        let p = entry.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        if p.is_dir() {
            if name != "tests" {
                rs_files(&p, out);
            }
        } else if name.ends_with(".rs") && !name.ends_with("tests.rs") {
            out.push(p);
        }
    }
}

/// `line` with its string literals emptied, so a brace inside text is not a code brace.
fn code_part(line: &str) -> String {
    let mut out = String::new();
    let (mut in_string, mut escaped) = (false, false);
    for c in line.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
        } else {
            out.push(c);
        }
    }
    out
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

/// The non-test, non-comment lines of `text`, with their line numbers. A `#[cfg(test)]` item is
/// skipped to its `;` or to the brace that closes it; the code after it is kept.
fn non_test_lines(text: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.trim() == "#[cfg(test)]" {
            // Skip the item: to a `;` at depth 0, or to the brace closing its first `{`.
            let mut depth = 0i32;
            let mut opened = false;
            i += 1;
            while i < lines.len() {
                let code = code_part(lines[i]);
                for c in code.chars() {
                    match c {
                        '{' => {
                            depth += 1;
                            opened = true;
                        }
                        '}' => depth -= 1,
                        _ => {}
                    }
                }
                let done = (opened && depth <= 0) || (!opened && code.trim_end().ends_with(';'));
                i += 1;
                if done {
                    break;
                }
            }
            continue;
        }
        if !is_comment(line) {
            out.push((i + 1, line.to_string()));
        }
        i += 1;
    }
    out
}

/// The lines of the body of `fn name` in `text` (signature through its closing brace), or `None`
/// when the file defines no such function.
fn function_lines(text: &str, name: &str) -> Option<Vec<(usize, String)>> {
    let lines: Vec<&str> = text.lines().collect();
    let needle = format!("fn {name}(");
    let needle_generic = format!("fn {name}<");
    let start = lines.iter().position(|l| {
        !is_comment(l) && (code_part(l).contains(&needle) || code_part(l).contains(&needle_generic))
    })?;
    let mut out = Vec::new();
    let (mut depth, mut opened) = (0i32, false);
    for (offset, line) in lines[start..].iter().enumerate() {
        if !is_comment(line) {
            out.push((start + offset + 1, line.to_string()));
            for c in code_part(line).chars() {
                match c {
                    '{' => {
                        depth += 1;
                        opened = true;
                    }
                    '}' => depth -= 1,
                    _ => {}
                }
            }
        }
        if opened && depth <= 0 {
            break;
        }
    }
    Some(out)
}

/// The files `text` (the source of `file`) declares as test-only modules: a `#[cfg(test)]`, an
/// optional `#[path = ".."]`, then `mod name;`.
fn test_module_files(file: &Path, text: &str) -> Vec<PathBuf> {
    let dir = file.parent().unwrap();
    let stem = file.file_stem().unwrap().to_string_lossy().to_string();
    let children = if stem == "mod" || stem == "lib" || stem == "main" {
        dir.to_path_buf()
    } else {
        dir.join(&stem)
    };
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if *line != "#[cfg(test)]" {
            continue;
        }
        let mut j = i + 1;
        let mut path_attr = None;
        if let Some(rest) = lines.get(j).and_then(|l| l.strip_prefix("#[path = \"")) {
            path_attr = rest.strip_suffix("\"]").map(str::to_string);
            j += 1;
        }
        let Some(decl) = lines.get(j) else { continue };
        let decl = decl
            .trim_start_matches("pub(crate) ")
            .trim_start_matches("pub(super) ")
            .trim_start_matches("pub ");
        let Some(name) = decl.strip_prefix("mod ").and_then(|r| r.strip_suffix(';')) else {
            continue;
        };
        match path_attr {
            Some(p) => out.push(dir.join(p)),
            None => {
                out.push(children.join(format!("{name}.rs")));
                out.push(children.join(name));
            }
        }
    }
    out
}

fn offences(file: &str, lines: &[(usize, String)]) -> Vec<String> {
    lines
        .iter()
        .flat_map(|(n, line)| {
            LIVE_LIBRARY
                .iter()
                .filter(|token| line.contains(*token))
                .map(move |token| format!("{file}:{n}: {token}"))
        })
        .collect()
}

/// Every live-library spelling in the named functions' bodies.
fn function_offences(root: &Path, list: &[(&str, &[&str])]) -> Vec<String> {
    let mut hits = Vec::new();
    for (file, functions) in list {
        let text = std::fs::read_to_string(root.join(file)).unwrap();
        for name in *functions {
            let body = function_lines(&text, name)
                .unwrap_or_else(|| panic!("{file} has no fn {name}; update this guard's list"));
            hits.extend(offences(file, &body));
        }
    }
    hits
}

#[test]
fn export_paths_never_read_the_live_rule_library() {
    let root = repo_root();
    let mut hits = Vec::new();
    for module in EXPORT_AND_REVIEW_MODULES {
        let path = root.join(module.trim_end_matches('/'));
        assert!(path.exists(), "{module} is listed but does not exist");
        let mut files = Vec::new();
        rs_files(&path, &mut files);
        // A file a scanned file (or its parent module) declares under `#[cfg(test)]` is a test.
        let mut test_files: Vec<PathBuf> = Vec::new();
        let parent_module = path.with_extension("rs");
        for declaring in files.iter().chain(std::iter::once(&parent_module)) {
            if let Ok(text) = std::fs::read_to_string(declaring) {
                test_files.extend(test_module_files(declaring, &text));
            }
        }
        for file in files {
            if test_files.iter().any(|t| file == *t || file.starts_with(t)) {
                continue;
            }
            let text = std::fs::read_to_string(&file).unwrap();
            let rel = file.strip_prefix(&root).unwrap().display().to_string();
            hits.extend(offences(&rel, &non_test_lines(&text)));
        }
    }
    hits.extend(function_offences(&root, EXPORT_AND_REVIEW_FUNCTIONS));
    // A positive control through the same scan: the inspection's own loader is found.
    assert!(
        !function_offences(
            &root,
            &[(
                "crates/server/src/routes/scan.rs",
                &["split_scannable_rules"]
            )]
        )
        .is_empty(),
        "the function scan finds a live load where there is one"
    );
    assert!(
        hits.is_empty(),
        "an export or review path reads the live rule library; read the inspection's rules \
         through export_rules (the app) or run_store::rule_snapshot (the command line):\n  {}",
        hits.join("\n  ")
    );
}

#[test]
fn the_guard_catches_a_live_load_and_skips_comments_and_test_items() {
    let text = "\
fn export_thing() {
    // camerata_rules::corpus_path() in a comment is fine
    let x = \"{\";
    let corpus = camerata_rules::corpus_path();
}
#[cfg(test)]
mod tests {
    fn t() { camerata_rules::corpus_path(); }
}
#[cfg(test)]
mod more_tests;
fn after() { camerata_rules::load_corpus_lenient(&p); }
";
    let body = function_lines(text, "export_thing").unwrap();
    let hits = offences("f.rs", &body);
    assert_eq!(hits, vec!["f.rs:4: corpus_path(".to_string()], "{hits:?}");
    let all = offences("f.rs", &non_test_lines(text));
    assert_eq!(
        all,
        vec![
            "f.rs:4: corpus_path(".to_string(),
            "f.rs:12: load_corpus_lenient(".to_string()
        ],
        "{all:?}"
    );
    assert!(function_lines(text, "missing").is_none());

    let decls = "#[cfg(test)]\nmod gate;\n#[cfg(test)]\n#[path = \"other_tests.rs\"]\nmod other;\nmod real;\n";
    let files = test_module_files(Path::new("/r/report_export.rs"), decls);
    assert!(
        files.contains(&PathBuf::from("/r/report_export/gate.rs")),
        "{files:?}"
    );
    assert!(
        files.contains(&PathBuf::from("/r/other_tests.rs")),
        "{files:?}"
    );
    assert!(!files.iter().any(|f| f.ends_with("real.rs")), "{files:?}");
}
