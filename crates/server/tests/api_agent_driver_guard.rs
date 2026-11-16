//! Source guard for the in-process tool loop (`crates/server/src/api_agent_driver.rs`).
//!
//! That loop reads and writes the disk and calls the model directly. Today it is reached from
//! two places only: the governed dev run (`dev_implement_run.rs`) and the greenfield fleet
//! (`live_fleet.rs`). An inspection (`ai_audit`, `camerata inspect`) and the agent finder never
//! use it; they go through the model port, behind the spend meter and the journal.
//!
//! This test makes that fact mechanical instead of habitual: it reads the source of every crate
//! and fails when a file outside the known users names the loop's constructors, or when an
//! inspection or finder file names the driver trait at all. A NEW user of the loop has to be
//! added to the list below, on purpose, in a change that someone reads.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

/// Names that build or hand out the loop's tool executor.
const LOOP_NAMES: &[&str] = &[
    "ApiAgentDriver",
    "build_agent_driver",
    "build_agent_driver_with",
    "ServerChildDriverFactory",
    "ServerOrchestratorDriverFactory",
];

/// The files (or folders, when the entry ends in `/`) that may name the loop.
const KNOWN_USERS: &[&str] = &[
    // The loop itself and its test modules.
    "crates/server/src/api_agent_driver.rs",
    "crates/server/src/api_agent_driver/",
    // The governed dev run.
    "crates/server/src/dev_implement_run.rs",
    // The greenfield fleet.
    "crates/server/src/live_fleet.rs",
    // Routing tests that build the drivers to check the provider follows the model.
    "crates/server/tests/model_selection_e2e.rs",
    // This guard.
    "crates/server/tests/api_agent_driver_guard.rs",
];

/// Places that must never name the driver trait: the inspection pipeline and the agent finder.
const NEVER_THE_DRIVER: &[&str] = &[
    "crates/server/src/ai_audit.rs",
    "crates/server/src/ai_audit/",
    "crates/server/src/agent_finder",
    "crates/server/src/review_agent",
];

fn is_listed(file: &str, list: &[&str]) -> bool {
    list.iter().any(|entry| {
        if entry.ends_with('/') {
            file.starts_with(entry)
        } else {
            file == *entry
                || file.starts_with(&format!("{entry}/"))
                || file.starts_with(&format!("{entry}."))
        }
    })
}

/// A file's source without its comment lines.
fn code_of(source: &str) -> String {
    source
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `word` as a whole identifier somewhere in `code`.
fn names(code: &str, word: &str) -> bool {
    let bytes = code.as_bytes();
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut from = 0;
    while let Some(at) = code[from..].find(word) {
        let start = from + at;
        let end = start + word.len();
        let before_ok = start == 0 || !ident(bytes[start - 1]);
        let after_ok = end >= bytes.len() || !ident(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

/// Every violation in `files` (`(relative path, source)`), as readable lines.
fn violations(files: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for (path, source) in files {
        let code = code_of(source);
        if !is_listed(path, KNOWN_USERS) {
            for name in LOOP_NAMES {
                if names(&code, name) {
                    out.push(format!(
                        "{path} names `{name}`; add it to KNOWN_USERS only if it is meant to use the loop"
                    ));
                }
            }
        }
        if is_listed(path, NEVER_THE_DRIVER) && names(&code, "AgentDriver") {
            out.push(format!(
                "{path} names `AgentDriver`; an inspection or the agent finder must not use a driver"
            ));
        }
    }
    out
}

fn rust_files(root: &Path) -> Vec<(String, String)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if name == "target" || name.starts_with('.') {
                    continue;
                }
                walk(&path, root, out);
            } else if name.ends_with(".rs") {
                if let Ok(src) = std::fs::read_to_string(&path) {
                    let rel = path
                        .strip_prefix(root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    out.push((rel, src));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(&root.join("crates"), root, &mut out);
    out
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

#[test]
fn only_the_known_users_name_the_in_process_loop() {
    let files = rust_files(&workspace_root());
    assert!(files.len() > 100, "the scan found the workspace's sources");
    let found = violations(&files);
    assert!(
        found.is_empty(),
        "the in-process tool loop is named outside its known users:\n{}",
        found.join("\n")
    );
}

#[test]
fn a_planted_new_user_of_the_loop_is_caught() {
    let planted = vec![(
        "crates/server/src/routes/new_feature.rs".to_string(),
        "fn go() { let d = ApiAgentDriver::new(c, \"m\", wt); }".to_string(),
    )];
    let found = violations(&planted);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("new_feature.rs"), "{found:?}");
}

#[test]
fn a_planted_use_of_the_driver_trait_in_the_inspection_is_caught() {
    let planted = vec![(
        "crates/server/src/ai_audit/stage.rs".to_string(),
        "use camerata_core::AgentDriver;".to_string(),
    )];
    let found = violations(&planted);
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
fn comments_and_longer_names_do_not_trip_the_guard() {
    let files = [
        (
            "crates/llm/src/llm.rs".to_string(),
            "/// used by `ApiAgentDriver`\nfn x() { let y = MyApiAgentDriverish; }".to_string(),
        ),
        (
            "crates/server/src/ai_audit/stage.rs".to_string(),
            "fn x() { let y = ApiAgentDriverLike; }".to_string(),
        ),
    ];
    assert!(violations(&files[..1]).is_empty());
    // A longer identifier that merely contains the name is not the name.
    assert!(
        violations(&files[1..]).is_empty(),
        "{:?}",
        violations(&files[1..])
    );
}
