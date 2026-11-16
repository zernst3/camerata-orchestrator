//! The read tools (`Read`, `Glob`, `Grep`, `LS`) are jailed to the worktree plus the read folders
//! the run declared. Synthetic layout under a temp folder:
//!
//! ```text
//! <tmp>/wt/src/lib.rs        inside the jail
//! <tmp>/wt/link -> ../outside  a symlink that leaves the jail
//! <tmp>/outside/secret.txt   outside the jail
//! <tmp>/sibling/api.rs       a declared read folder
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use super::*;

const SECRET: &str = "synthetic-outside-secret-body";

struct Layout {
    _tmp: tempfile::TempDir,
    wt: PathBuf,
    outside: PathBuf,
    sibling: PathBuf,
}

fn layout() -> Layout {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let wt = root.join("wt");
    let outside = root.join("outside");
    let sibling = root.join("sibling");
    std::fs::create_dir_all(wt.join("src")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir_all(&sibling).unwrap();
    std::fs::write(wt.join("src/lib.rs"), "pub fn inside() {}\n").unwrap();
    std::fs::write(outside.join("secret.txt"), SECRET).unwrap();
    std::fs::write(sibling.join("api.rs"), "pub fn sibling_api() {}\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, wt.join("link")).unwrap();
    Layout {
        _tmp: tmp,
        wt,
        outside,
        sibling,
    }
}

fn driver(l: &Layout) -> ApiAgentDriver {
    ApiAgentDriver::new(Arc::new(AnthropicNoopCompleter), "claude-sonnet-5")
        .with_worktree(l.wt.clone())
}

async fn call(d: &ApiAgentDriver, tool: &str, input: Value) -> String {
    let role = camerata_core::Role {
        name: "t".to_string(),
        rule_subset: vec![],
        allowed_paths: vec![],
    };
    let inv = ToolInvocation {
        id: "c".to_string(),
        name: tool.to_string(),
        input,
    };
    execute_tool(d, &role, &inv).await.0
}

fn assert_refused(out: &str) {
    assert!(out.starts_with("DENIED"), "expected a refusal, got: {out}");
    assert!(!out.contains(SECRET), "the secret leaked: {out}");
}

#[tokio::test]
async fn an_absolute_path_outside_the_jail_is_refused() {
    let l = layout();
    let out = call(
        &driver(&l),
        "Read",
        json!({"file_path": l.outside.join("secret.txt").display().to_string()}),
    )
    .await;
    assert_refused(&out);
}

#[tokio::test]
async fn a_dot_dot_escape_is_refused() {
    let l = layout();
    let out = call(
        &driver(&l),
        "Read",
        json!({"file_path": "../outside/secret.txt"}),
    )
    .await;
    assert_refused(&out);
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_that_leaves_the_jail_is_refused() {
    let l = layout();
    let out = call(
        &driver(&l),
        "Read",
        json!({"file_path": "link/secret.txt"}),
    )
    .await;
    assert_refused(&out);
}

#[tokio::test]
async fn ls_and_grep_outside_the_jail_are_refused() {
    let l = layout();
    let d = driver(&l);
    assert_refused(&call(&d, "LS", json!({"path": "/"})).await);
    assert_refused(
        &call(
            &d,
            "Grep",
            json!({"pattern": "synthetic", "path": l.outside.display().to_string()}),
        )
        .await,
    );
}

#[cfg(unix)]
#[tokio::test]
async fn grep_does_not_follow_a_symlink_out_of_the_jail() {
    let l = layout();
    let out = call(&driver(&l), "Grep", json!({"pattern": "synthetic"})).await;
    assert!(!out.contains(SECRET), "{out}");
    assert!(!out.contains("secret.txt"), "{out}");
}

#[tokio::test]
async fn a_glob_with_an_absolute_pattern_outside_the_jail_is_refused() {
    let l = layout();
    let out = call(
        &driver(&l),
        "Glob",
        json!({"pattern": format!("{}/*", l.outside.display())}),
    )
    .await;
    assert_refused(&out);
}

#[cfg(unix)]
#[tokio::test]
async fn a_relative_glob_drops_matches_that_resolve_outside_the_jail() {
    let l = layout();
    let out = call(&driver(&l), "Glob", json!({"pattern": "link/*"})).await;
    assert!(!out.contains("secret.txt"), "{out}");
}

#[tokio::test]
async fn a_path_inside_the_jail_still_works() {
    let l = layout();
    let d = driver(&l);
    let read = call(&d, "Read", json!({"file_path": "src/lib.rs"})).await;
    assert!(read.contains("pub fn inside"), "{read}");
    let abs = call(
        &d,
        "Read",
        json!({"file_path": l.wt.join("src/lib.rs").display().to_string()}),
    )
    .await;
    assert!(abs.contains("pub fn inside"), "{abs}");
    let ls = call(&d, "LS", json!({"path": "src"})).await;
    assert!(ls.contains("lib.rs"), "{ls}");
    let glob = call(&d, "Glob", json!({"pattern": "src/*.rs"})).await;
    assert!(glob.contains("lib.rs"), "{glob}");
    let grep = call(&d, "Grep", json!({"pattern": "inside"})).await;
    assert!(grep.contains("lib.rs"), "{grep}");
}

#[tokio::test]
async fn a_declared_read_folder_is_readable_and_others_are_not() {
    let l = layout();
    let d = driver(&l).with_read_roots(vec![l.sibling.clone()]);
    let ok = call(
        &d,
        "Read",
        json!({"file_path": l.sibling.join("api.rs").display().to_string()}),
    )
    .await;
    assert!(ok.contains("sibling_api"), "{ok}");
    let no = call(
        &d,
        "Read",
        json!({"file_path": l.outside.join("secret.txt").display().to_string()}),
    )
    .await;
    assert_refused(&no);
}

#[tokio::test]
async fn the_refusal_names_the_path_and_says_what_is_allowed() {
    let l = layout();
    let out = call(
        &driver(&l),
        "Read",
        json!({"file_path": "../outside/secret.txt"}),
    )
    .await;
    assert!(out.contains("outside/secret.txt"), "{out}");
    assert!(out.contains("worktree"), "{out}");
}

#[tokio::test]
async fn the_jail_is_the_only_reading_the_loop_does_end_to_end() {
    // A scripted model asks for an outside file; the tool result it gets back is the refusal.
    let l = layout();
    let completer = Arc::new(OneShotRead {
        path: l.outside.join("secret.txt").display().to_string(),
        seen: std::sync::Mutex::new(Vec::new()),
    });
    let d = ApiAgentDriver::new(completer.clone(), "stub").with_worktree(l.wt.clone());
    let role = camerata_core::Role {
        name: "t".to_string(),
        rule_subset: vec![],
        allowed_paths: vec![],
    };
    use camerata_core::AgentDriver as _;
    let _ = d.run(&role, "go").await.unwrap();
    let seen = completer.seen.lock().unwrap().join("\n");
    assert!(seen.contains("DENIED"), "{seen}");
    assert!(!seen.contains(SECRET), "{seen}");
    let _ = Path::new("");
}

struct OneShotRead {
    path: String,
    seen: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl LlmPort for OneShotRead {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        // The fallback path squashes history into a text prompt, so a tool result is visible
        // in `prompt` only when it has string content.
        self.seen.lock().unwrap().push(req.prompt.clone());
        let n = self.seen.lock().unwrap().len();
        let text = if n == 1 {
            json!({"message": {"role": "assistant", "content": null, "tool_calls": [{
                "id": "c1", "type": "function",
                "function": {"name": "Read",
                    "arguments": json!({"file_path": self.path}).to_string()}
            }]}})
            .to_string()
        } else {
            json!({"message": {"role": "assistant", "content": "done"}}).to_string()
        };
        Ok(LlmResponse {
            reported_model: None,
            cli_cost_usd: None,
            text,
            model: "stub".into(),
            backend: "stub".into(),
            cost_usd: Some(0.0),
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        })
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        self.complete(req).await
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
