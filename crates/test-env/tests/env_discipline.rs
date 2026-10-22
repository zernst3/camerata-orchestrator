//! Mechanical enforcement of the process-env test discipline described in
//! `camerata_test_env`'s crate docs. A convention nobody checks decays, and this one failed
//! exactly that way once (a flaky full-workspace run traced to an unguarded
//! `remove_var("ANTHROPIC_API_KEY")`), so the two checkable halves are checked here:
//!
//! 1. NO second serial key. `#[serial(a)]` and `#[serial(b)]` do not exclude each other, so
//!    any key other than `process_env` can race `process_env` tests on a shared variable.
//!    A bare `#[serial]` (the implicit default key) is the same hazard and is rejected too.
//! 2. A test that uses `EnvGuard` carries `serial(process_env)`. A guard without the key
//!    restores state on drop but still races other tests while it is held.
//!
//! The raw `std::env::set_var` / `remove_var` ban is enforced by clippy (`clippy.toml`
//! `disallowed-methods`), not here. The reader half (a test that merely exercises code
//! reading a mutated variable) cannot be checked textually and stays a review obligation.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

/// The one permitted key. Mirrors `camerata_test_env::SERIAL_KEY`.
const KEY: &str = camerata_test_env::SERIAL_KEY;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Every `.rs` file under `crates/` and `tools/`, skipping build output and the embedded
/// scaffold template tree (data, not workspace code).
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if name == "target" || name == "templates" || name.starts_with('.') {
                continue;
            }
            rust_files(&path, out);
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

fn all_workspace_sources() -> Vec<PathBuf> {
    let root = workspace_root();
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);
    rust_files(&root.join("tools"), &mut files);
    files.sort();
    files
}

/// A line that is a `serial` ATTRIBUTE (not prose in a comment or doc string).
fn serial_attribute(line: &str) -> Option<&str> {
    // `#[serial]`, `#[serial(..)]`, `#[serial_test::serial(..)]`, `#[serial_test::serial]`;
    // `file_serial` / `parallel` and friends are different attributes.
    let t = line.trim_start().strip_prefix("#[")?;
    let t = t.strip_prefix("serial_test::").unwrap_or(t);
    let rest = t.strip_prefix("serial")?;
    // `serial` must end the identifier (`serial_test`, `serialize` are not it).
    if rest.starts_with(|c: char| c.is_alphanumeric() || c == '_' || c == ':') {
        return None;
    }
    Some(rest)
}

#[test]
fn no_serial_key_other_than_process_env_exists() {
    let own = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/env_discipline.rs");
    let mut violations = Vec::new();
    for file in all_workspace_sources() {
        if file == own {
            continue;
        }
        let text = fs::read_to_string(&file).unwrap();
        for (n, line) in text.lines().enumerate() {
            let Some(rest) = serial_attribute(line) else {
                continue;
            };
            let allowed = rest.starts_with(&format!("({KEY})"));
            if !allowed {
                violations.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "every serial_test attribute must be exactly `serial({KEY})` (two keys do not \
         exclude each other, so a second key re-opens the env race). Offenders:\n{}",
        violations.join("\n")
    );
}

#[test]
fn every_test_using_env_guard_carries_the_shared_serial_key() {
    let own = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/env_discipline.rs");
    // This crate's own unit tests exercise EnvGuard on variable names private to each test,
    // so they need (and have) no shared key.
    let own_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut violations = Vec::new();
    for file in all_workspace_sources() {
        if file == own || file.starts_with(&own_src) {
            continue;
        }
        let text = fs::read_to_string(&file).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            let trimmed = lines[i].trim_start();
            let is_fn = trimmed.starts_with("fn ")
                || trimmed.starts_with("async fn ")
                || trimmed.starts_with("pub fn ")
                || trimmed.starts_with("pub async fn ");
            if !is_fn {
                i += 1;
                continue;
            }
            // Attributes directly above the fn.
            let mut attrs = Vec::new();
            let mut a = i;
            while a > 0 && lines[a - 1].trim_start().starts_with("#[") {
                a -= 1;
                attrs.push(lines[a]);
            }
            let is_test = attrs.iter().any(|l| {
                let t = l.trim();
                t == "#[test]" || t.starts_with("#[tokio::test")
            });
            // The fn body ends at the first line that is exactly the closing brace at the
            // fn's own indentation (the workspace is rustfmt-clean, so this is reliable).
            let indent = lines[i].len() - trimmed.len();
            let closer = format!("{}}}", " ".repeat(indent));
            let end = (i + 1..lines.len())
                .find(|&j| lines[j] == closer)
                .unwrap_or(lines.len() - 1);
            if is_test
                && lines[i..=end].iter().any(|l| l.contains("EnvGuard::"))
                && !attrs.iter().any(|l| l.contains(&format!("serial({KEY})")))
            {
                let name = trimmed
                    .split("fn ")
                    .nth(1)
                    .and_then(|s| s.split('(').next())
                    .unwrap_or("?");
                violations.push(format!("{}:{}: fn {name}", file.display(), i + 1));
            }
            i += 1;
        }
    }
    assert!(
        violations.is_empty(),
        "tests that use EnvGuard must also be `#[serial_test::serial({KEY})]`. Offenders:\n{}",
        violations.join("\n")
    );
}

#[test]
fn the_scanner_recognises_serial_attributes() {
    assert_eq!(
        serial_attribute("    #[serial_test::serial(process_env)]"),
        Some("(process_env)]")
    );
    assert_eq!(serial_attribute("#[serial]"), Some("]"));
    assert_eq!(
        serial_attribute("#[serial_test::serial(bff_env)]"),
        Some("(bff_env)]")
    );
    // Prose that merely mentions a key is not an attribute.
    assert_eq!(serial_attribute("// #[serial(bff_env)] is retired"), None);
    assert_eq!(serial_attribute("//! serial_test::serial(bff_env)"), None);
    assert_eq!(serial_attribute("#[cfg(test)]"), None);
    assert_eq!(serial_attribute("#[serialize_this]"), None);
}
