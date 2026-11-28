#![allow(clippy::unwrap_used)]
//! Guard: no code or client text outside `camerata_api_types::package_paths` spells a package
//! path as a literal (ARCH-SHARED-CONSTANT-1). The layout of the client package is a public
//! interface, so every path is written once and every other place names the file through the
//! constant or the `pkg_*!` macro.
//!
//! Scanned: every non-test `.rs` file of every crate under `crates/`, and the typst templates.
//! Not scanned: a `tests` directory, a file named `*tests.rs` or `*e2e*.rs`, comment lines, a
//! `#[cfg(test)] mod x;` declaration (one line) and a `#[cfg(test)] mod x { .. }` block (to its
//! closing brace); the code after either is scanned again. Tests assert paths on purpose. The
//! module that owns the paths is the one exception.

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

/// One forbidden spelling: a name for the report, the pattern, and a prefix that excuses a match.
struct Forbidden {
    name: &'static str,
    pattern: Regex,
    /// A match whose preceding text ends with this is a different file (`all-` before
    /// `findings.json`).
    excused_after: Option<&'static str>,
}

/// The path spellings that must exist only in the paths module.
fn forbidden() -> Vec<Forbidden> {
    let list: [(&str, &str, Option<&str>); 17] = [
        ("findings.json", r"findings\.json", Some("all-")),
        (
            "decisions.json",
            r"(?:^|[^\w])decisions\.json(?:[^l]|$)",
            None,
        ),
        ("inspection.json", r"inspection\.json", None),
        ("tickets.csv", r"tickets\.csv", None),
        ("triage-log.csv", r"triage-log\.csv", None),
        ("all-findings.json", r"all-findings\.json", None),
        ("all-findings.csv", r"all-findings\.csv", None),
        ("manifest.json", r"(?:^|[^-\w/._])manifest\.json", None),
        ("README.txt", r"(?:^|[^-\w/.])README\.txt", None),
        ("a json/ path", r"(?:^|[^-\w/.:])json/", None),
        ("a csv/ path", r"(?:^|[^-\w/.:])csv/", None),
        (
            "a json or csv folder built with join, push or a prefix test",
            r#"(?:join|push|strip_prefix|starts_with)\(\s*"(?:json|csv)"#,
            None,
        ),
        ("a PDF name built from a stem", r"\}\.pdf", None),
        ("a workbook name", r"-findings\.xlsx", None),
        ("a workbook name built from a stem", r"\}\.xlsx", None),
        (
            "a PDF name built from a stem",
            r#"format!\([^)]*\.pdf"#,
            None,
        ),
        ("a dataset folder literal", r#""(?:json|csv)/"#, None),
    ];
    list.into_iter()
        .map(|(name, pattern, excused_after)| Forbidden {
            name,
            pattern: Regex::new(pattern).unwrap(),
            excused_after,
        })
        .collect()
}

/// `line` with its string literals emptied, so braces inside text do not count as code braces.
fn without_strings(line: &str) -> String {
    let mut out = String::new();
    let mut in_string = false;
    let mut escaped = false;
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

/// How the line after a `#[cfg(test)]` attribute opens: `mod x;` is a one-line declaration,
/// `mod x {` an inline block, anything else (an import or a helper) is not a test module.
enum TestItem {
    Declaration,
    InlineBlock,
    Other,
}

fn test_item(next: &str) -> TestItem {
    let next = next.trim_start();
    let next = next
        .strip_prefix("pub(crate) ")
        .or_else(|| next.strip_prefix("pub(super) "))
        .or_else(|| next.strip_prefix("pub "))
        .unwrap_or(next);
    match next.strip_prefix("mod ") {
        Some(rest) if rest.trim_end().ends_with(';') => TestItem::Declaration,
        Some(_) => TestItem::InlineBlock,
        None => TestItem::Other,
    }
}

/// Every offending line number and name in `text`: non-test, non-comment code and text.
/// A `#[cfg(test)] mod x;` is skipped as one item; a `#[cfg(test)] mod x { .. }` block is skipped
/// to its closing brace. Code after either is scanned again.
fn offences(text: &str, forbidden: &[Forbidden]) -> Vec<(usize, &'static str)> {
    let mut found = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut index = 0;
    let mut in_block_comment = false;
    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim_start();
        if in_block_comment {
            if trimmed.contains("*/") {
                in_block_comment = false;
            }
            index += 1;
            continue;
        }
        if trimmed.starts_with("/*") && !trimmed.contains("*/") {
            in_block_comment = true;
            index += 1;
            continue;
        }
        if trimmed.starts_with("#[cfg(test)]") {
            match lines.get(index + 1).map(|next| test_item(next)) {
                Some(TestItem::Declaration) => {
                    index += 2;
                    continue;
                }
                Some(TestItem::InlineBlock) => {
                    let mut depth: i64 = 0;
                    let mut opened = false;
                    index += 1;
                    while index < lines.len() {
                        for c in without_strings(lines[index]).chars() {
                            if c == '{' {
                                depth += 1;
                                opened = true;
                            } else if c == '}' {
                                depth -= 1;
                            }
                        }
                        index += 1;
                        if opened && depth <= 0 {
                            break;
                        }
                    }
                    continue;
                }
                _ => {}
            }
        }
        if !trimmed.starts_with("//") {
            for rule in forbidden {
                let hit = rule.pattern.find_iter(line).any(|m| {
                    !rule
                        .excused_after
                        .is_some_and(|prefix| line[..m.start()].ends_with(prefix))
                });
                if hit {
                    found.push((index + 1, rule.name));
                }
            }
        }
        index += 1;
    }
    found
}

#[test]
fn no_package_path_is_spelled_outside_the_paths_module() {
    let mut files = Vec::new();
    files_under(&repo_crates(), &mut files);
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

fn names(code: &str) -> Vec<&'static str> {
    let rules = forbidden();
    offences(code, &rules).into_iter().map(|(_, n)| n).collect()
}

#[test]
fn the_guard_catches_a_literal_and_ignores_comments() {
    assert_eq!(names("let a = \"findings.json\";"), vec!["findings.json"]);
    assert!(names("// findings.json in a comment").is_empty());
    assert_eq!(names("let b = \"json/x.json\";").len(), 2);
    assert!(names("let ok = \"all-findings\";").is_empty());
    assert!(names("concat!(\"a \", camerata_api_types::pkg_inspection_json!())").is_empty());
}

#[test]
fn the_guard_skips_an_inline_test_block_and_scans_the_code_after_it() {
    let code = "#[cfg(test)]\nmod tests {\n    fn t() { let t = \"findings.json\"; }\n}\nlet after = \"findings.json\";\n";
    let rules = forbidden();
    assert_eq!(offences(code, &rules), vec![(5, "findings.json")]);
}

#[test]
fn a_test_module_declaration_near_the_top_does_not_hide_the_rest_of_the_file() {
    let code = "#[cfg(test)]\nmod x_tests;\nlet a = \"findings.json\";\n";
    let rules = forbidden();
    assert_eq!(offences(code, &rules), vec![(3, "findings.json")]);
    let code = "#[cfg(test)]\nmod x_tests;\n\nfn f() { let p = \"tickets.csv\"; }\n";
    assert_eq!(offences(code, &rules), vec![(4, "tickets.csv")]);
}

#[test]
fn the_bare_names_and_the_old_names_after_a_separator_are_caught() {
    for text in [
        "\"inspection.json\"",
        "\"tickets.csv\"",
        "\"triage-log.csv\"",
        "\"all-findings.json\"",
        "\"all-findings.csv\"",
        "\"a/findings.json\"",
        "\"x-findings.json\"",
        "\"x.findings.json\"",
        "\"x_findings.json\"",
        "\"x/decisions.json\"",
    ] {
        assert!(!names(text).is_empty(), "{text} must be caught");
    }
    assert!(names("\"decisions.jsonl\"").is_empty());
}

#[test]
fn a_folder_built_with_join_or_format_is_caught() {
    for text in [
        "dir.join(\"json\")",
        "path.starts_with(\"csv\")",
        "out.push(\"json\")",
        "format!(\"csv/{name}.csv\")",
    ] {
        assert!(!names(text).is_empty(), "{text} must be caught");
    }
}

#[test]
fn a_pdf_or_workbook_name_built_from_a_stem_is_caught() {
    for text in [
        "format!(\"{stem}.pdf\")",
        "format!(\"{stem}-findings.xlsx\")",
        "let n = format!(\"{}.pdf\", stem);",
    ] {
        assert!(!names(text).is_empty(), "{text} must be caught");
    }
    assert!(names("path.ends_with(\".pdf\")").is_empty());
}
