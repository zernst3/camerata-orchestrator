//! Future-proofing for the in-process agent tool loop.
//!
//! 1. A SOURCE GUARD: the loop's tool executor (an [`super::ApiAgentDriver`], the only thing
//!    that runs `gated_write` and the read tools in-process, and the [`super::ReadJail`] its read
//!    tools run under) is constructed only in the files that construct it today. Another module
//!    that wants an agent goes through `build_agent_driver` or the server's driver factories,
//!    which apply the worktree, the read jail, the run ledger and the spend limit. A new
//!    construction site elsewhere fails this test and has to be reviewed (and added here).
//! 2. A CLOSED TOOL LIST: the tools the loop offers the model are exactly these, for a worker
//!    and for an orchestrator, in both wire shapes. Adding a tool fails this test: a new tool is
//!    a new way for model output to touch the machine, so it is a reviewed change.

use std::path::{Path, PathBuf};

use super::{build_tool_schemas_for, ApiShape};

/// Where the tool executor may be constructed: this module and its own files.
const ALLOWED: &[&str] = &[
    "crates/server/src/api_agent_driver.rs",
    "crates/server/src/api_agent_driver/",
];

/// What constructing (or aliasing, to construct under another name) the executor looks like.
const CONSTRUCTIONS: &[&str] = &[
    "ApiAgentDriver::new",
    "ApiAgentDriver {",
    "ReadJail::new",
    "ApiAgentDriver as ",
    "ReadJail as ",
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "target" || name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            rust_files(&path, out);
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

/// Every `file:line` outside [`ALLOWED`] whose code (comments skipped) constructs the executor.
fn construction_sites_outside(root: &Path, scan: &[&str]) -> Vec<String> {
    let mut files = Vec::new();
    for dir in scan {
        rust_files(&root.join(dir), &mut files);
    }
    let mut hits = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        if ALLOWED
            .iter()
            .any(|a| rel == *a || (a.ends_with('/') && rel.starts_with(a)))
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if CONSTRUCTIONS.iter().any(|c| code.contains(c)) {
                hits.push(format!("{rel}:{}: {}", i + 1, line.trim()));
            }
        }
    }
    hits
}

#[test]
fn the_tool_executor_is_constructed_only_where_it_is_today() {
    let root = workspace_root();
    let hits = construction_sites_outside(&root, &["crates", "tools"]);
    assert!(
        hits.is_empty(),
        "the in-process agent tool executor is constructed outside api_agent_driver; build \
         agents through build_agent_driver or the server driver factories (they apply the \
         worktree, read jail, run ledger and spend limit), or review this site and add it to \
         ALLOWED:\n{}",
        hits.join("\n")
    );
}

/// The guard is not vacuous: it does see the construction sites that exist today.
#[test]
fn the_source_guard_sees_the_construction_sites_it_allows() {
    let root = workspace_root();
    let text = std::fs::read_to_string(root.join(ALLOWED[0])).expect("the driver source");
    assert!(text.contains("ApiAgentDriver::new("));
    assert!(text.contains("ReadJail::new("));
}

fn tool_names(orchestrator: bool, shape: ApiShape) -> Vec<String> {
    build_tool_schemas_for(orchestrator, shape)
        .iter()
        .map(|t| {
            t["function"]["name"]
                .as_str()
                .or_else(|| t["name"].as_str())
                .expect("every tool schema has a name")
                .to_string()
        })
        .collect()
}

#[test]
fn the_tools_offered_to_the_model_are_a_closed_list() {
    const WORKER: &[&str] = &["gated_write", "Read", "Glob", "Grep", "LS"];
    const ORCHESTRATOR: &[&str] = &[
        "gated_write",
        "Read",
        "Glob",
        "Grep",
        "LS",
        "delegate",
        "fan_out",
    ];
    for shape in [ApiShape::OpenRouter, ApiShape::Anthropic] {
        assert_eq!(tool_names(false, shape), WORKER, "worker tools ({shape:?})");
        assert_eq!(
            tool_names(true, shape),
            ORCHESTRATOR,
            "orchestrator tools ({shape:?})"
        );
    }
}

/// Every read tool on the list runs under the read jail: an absolute path outside the
/// worktree is refused by each of them.
#[tokio::test]
async fn every_offered_read_tool_is_jailed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let wt = tmp.path().join("wt");
    std::fs::create_dir_all(&wt).unwrap();
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("f.txt"), "x").unwrap();
    let driver = super::ApiAgentDriver::new(
        std::sync::Arc::new(super::AnthropicNoopCompleter),
        "claude-sonnet-5",
        wt,
    );
    let role = camerata_core::Role {
        name: "guard".to_string(),
        rule_subset: vec![],
        allowed_paths: vec![],
    };
    let target = outside.join("f.txt").display().to_string();
    let dir = outside.display().to_string();
    for name in tool_names(false, ApiShape::Anthropic) {
        if name == "gated_write" {
            continue;
        }
        let inv = super::ToolInvocation {
            id: "guard".to_string(),
            name: name.clone(),
            input: serde_json::json!({
                "file_path": target, "path": dir, "pattern": format!("{dir}/*"),
            }),
        };
        let (text, _) = super::execute_tool(&driver, &role, &inv).await;
        assert!(
            text.starts_with("DENIED"),
            "read tool `{name}` reached outside the jail: {text}"
        );
    }
}
