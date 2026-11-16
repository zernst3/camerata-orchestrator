//! Every read tool call is logged: the path (relative to the jail), the bytes returned and
//! whether it was allowed or refused. The log is shared by handle with child agents and leads,
//! and each read is also written to the run's governance log when one is wired.

use std::sync::Arc;

use serde_json::{json, Value};

use super::*;

fn role() -> camerata_core::Role {
    camerata_core::Role {
        name: "t".to_string(),
        rule_subset: vec![],
        allowed_paths: vec![],
    }
}

fn inv(tool: &str, input: Value) -> ToolInvocation {
    ToolInvocation {
        id: "c".to_string(),
        name: tool.to_string(),
        input,
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    wt: std::path::PathBuf,
    outside: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let wt = root.join("wt");
    std::fs::create_dir_all(wt.join("src")).unwrap();
    std::fs::write(wt.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    std::fs::write(wt.join("README.md"), "# readme\n").unwrap();
    let outside = root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "synthetic secret").unwrap();
    Fixture {
        _tmp: tmp,
        wt,
        outside,
    }
}

fn driver(f: &Fixture) -> ApiAgentDriver {
    ApiAgentDriver::new(Arc::new(StubOnly), "stub").with_worktree(f.wt.clone())
}

struct StubOnly;

#[async_trait::async_trait]
impl LlmPort for StubOnly {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        anyhow::bail!("not used")
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

#[tokio::test]
async fn three_reads_give_three_log_entries_with_paths_and_bytes() {
    let f = fixture();
    let d = driver(&f);
    let r = role();
    let (read, _) = execute_tool(&d, &r, &inv("Read", json!({"file_path": "src/lib.rs"}))).await;
    let _ = execute_tool(&d, &r, &inv("LS", json!({"path": "src"}))).await;
    let _ = execute_tool(&d, &r, &inv("Grep", json!({"pattern": "readme"}))).await;

    let entries = d.read_log().entries();
    assert_eq!(entries.len(), 3, "{entries:?}");
    assert_eq!(entries[0].tool, "Read");
    assert_eq!(entries[0].path, "src/lib.rs", "relative to the jail");
    assert_eq!(entries[0].bytes, read.len());
    assert!(entries[0].allowed);
    assert_eq!(entries[1].tool, "LS");
    assert_eq!(entries[1].path, "src");
    assert_eq!(entries[2].tool, "Grep");
    assert!(entries.iter().all(|e| e.allowed));
}

#[tokio::test]
async fn an_absolute_path_inside_the_jail_is_logged_relative() {
    let f = fixture();
    let d = driver(&f);
    let abs = f.wt.join("src/lib.rs").display().to_string();
    let _ = execute_tool(&d, &role(), &inv("Read", json!({"file_path": abs}))).await;
    assert_eq!(d.read_log().entries()[0].path, "src/lib.rs");
}

#[tokio::test]
async fn a_refused_read_is_logged_as_refused() {
    let f = fixture();
    let d = driver(&f);
    let asked = f.outside.join("secret.txt").display().to_string();
    let _ = execute_tool(&d, &role(), &inv("Read", json!({"file_path": asked.clone()}))).await;
    let entries = d.read_log().entries();
    assert_eq!(entries.len(), 1);
    assert!(!entries[0].allowed, "{entries:?}");
    assert_eq!(entries[0].path, asked, "a refused path is logged as asked");
}

#[tokio::test]
async fn a_child_driver_sharing_the_handle_logs_into_the_parent_log() {
    let f = fixture();
    let parent = driver(&f);
    let child_controls = DriverControls {
        read_log: Some(parent.read_log()),
        ..DriverControls::default()
    };
    let child = apply_controls(driver(&f), &child_controls).with_agent_label("delegate-child");
    let _ = execute_tool(&child, &role(), &inv("Read", json!({"file_path": "README.md"}))).await;
    let _ = execute_tool(&parent, &role(), &inv("Read", json!({"file_path": "src/lib.rs"}))).await;
    let entries = parent.read_log().entries();
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_eq!(entries[0].agent, "delegate-child");
    assert_eq!(entries[0].path, "README.md");
    assert_eq!(entries[1].agent, "agent");
}

#[tokio::test]
async fn the_factories_hand_their_read_log_to_every_child_and_lead() {
    let log = ReadLog::default();
    let controls = DriverControls {
        read_log: Some(log.clone()),
        ..DriverControls::default()
    };
    let child = child_controls_from(&controls);
    assert!(
        child.read_log.as_ref().unwrap().same_log(&log),
        "the child writes into the run's log, not a new one"
    );
}

#[tokio::test]
async fn a_read_is_recorded_in_the_governance_log_with_path_bytes_and_outcome() {
    let f = fixture();
    let log = Arc::new(
        camerata_persistence::GovernanceLog::open_in_memory()
            .await
            .unwrap(),
    );
    let d = driver(&f).with_governance(log.clone(), "run-read-1");
    let (text, _) = execute_tool(&d, &role(), &inv("Read", json!({"file_path": "src/lib.rs"}))).await;
    let _ = execute_tool(
        &d,
        &role(),
        &inv(
            "Read",
            json!({"file_path": f.outside.join("secret.txt").display().to_string()}),
        ),
    )
    .await;

    let events = log.by_run("run-read-1").await.unwrap();
    let reads: Vec<_> = events.iter().filter(|e| e.kind == "read").collect();
    assert_eq!(reads.len(), 2, "{events:?}");
    let ok = reads[0].detail.clone().unwrap_or_default();
    assert!(ok.contains("src/lib.rs"), "{ok}");
    assert!(ok.contains(&format!("\"bytes\":{}", text.len())), "{ok}");
    assert!(ok.contains("\"allowed\":true"), "{ok}");
    assert_eq!(reads[1].severity, "warn", "a refused read is a warning");
    assert!(reads[1].detail.clone().unwrap_or_default().contains("\"allowed\":false"));
}
