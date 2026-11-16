//! The loop offers a closed list of tools. A new tool must not appear silently: this test names
//! them all, and each read tool must go through the read jail and the read log.

use serde_json::json;

use super::*;

#[test]
fn a_worker_is_offered_exactly_these_tools() {
    let names: Vec<String> = build_tool_schemas(false)
        .iter()
        .filter_map(|s| s["function"]["name"].as_str().map(str::to_string))
        .collect();
    assert_eq!(names, ["gated_write", "Read", "Glob", "Grep", "LS"]);
}

#[test]
fn an_orchestrator_is_offered_those_plus_delegate_and_fan_out() {
    let names: Vec<String> = build_tool_schemas(true)
        .iter()
        .filter_map(|s| s["function"]["name"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        names,
        [
            "gated_write",
            "Read",
            "Glob",
            "Grep",
            "LS",
            "delegate",
            "fan_out"
        ]
    );
}

#[test]
fn both_wire_shapes_offer_the_same_closed_list() {
    for orchestrator in [false, true] {
        let openai: Vec<String> = build_tool_schemas_for(orchestrator, ApiShape::OpenRouter)
            .iter()
            .filter_map(|s| s["function"]["name"].as_str().map(str::to_string))
            .collect();
        let anthropic: Vec<String> = build_tool_schemas_for(orchestrator, ApiShape::Anthropic)
            .iter()
            .filter_map(|s| s["name"].as_str().map(str::to_string))
            .collect();
        assert_eq!(openai, anthropic);
    }
}

#[tokio::test]
async fn every_read_tool_in_the_list_is_jailed_and_logged() {
    let tmp = tempfile::tempdir().unwrap();
    let driver = ApiAgentDriver::new(
        Arc::new(AnthropicNoopCompleter),
        "claude-sonnet-5",
        tmp.path(),
    );
    let role = camerata_core::Role {
        name: "t".into(),
        rule_subset: vec![],
        allowed_paths: vec![],
    };
    let reads = ["Read", "Glob", "Grep", "LS"];
    // Every tool in the list that is not the write tool or an orchestrator tool is a read tool.
    let offered: Vec<String> = build_tool_schemas(false)
        .iter()
        .filter_map(|s| s["function"]["name"].as_str().map(str::to_string))
        .filter(|n| n != "gated_write")
        .collect();
    assert_eq!(offered, reads, "a new read tool must be added to this test");
    for tool in reads {
        let input = serde_json::json!({
            "file_path": "/etc/hosts", "path": "/etc", "pattern": "/etc/*"
        });
        let inv = ToolInvocation {
            id: "c".into(),
            name: tool.into(),
            input,
        };
        let (out, _) = execute_tool(&driver, &role, &inv).await;
        assert!(
            out.starts_with("DENIED"),
            "{tool} read outside the jail: {out}"
        );
    }
    let entries = driver.read_log().entries();
    assert_eq!(entries.len(), reads.len(), "{entries:?}");
    assert!(entries.iter().all(|e| !e.allowed));
}

#[tokio::test]
async fn a_tool_that_is_not_in_the_list_is_denied() {
    let tmp = tempfile::tempdir().unwrap();
    let driver = ApiAgentDriver::new(
        Arc::new(AnthropicNoopCompleter),
        "claude-sonnet-5",
        tmp.path(),
    );
    let role = camerata_core::Role {
        name: "t".into(),
        rule_subset: vec![],
        allowed_paths: vec![],
    };
    for tool in [
        "WebFetch",
        "Bash",
        "Edit",
        "Write",
        "NotebookEdit",
        "delegate",
    ] {
        let inv = ToolInvocation {
            id: "c".into(),
            name: tool.into(),
            input: serde_json::json!({}),
        };
        let (out, denial) = execute_tool(&driver, &role, &inv).await;
        assert!(out.starts_with("DENIED"), "{tool}: {out}");
        assert!(denial.is_some(), "{tool}");
    }
}

#[test]
fn the_delegate_schema_asks_for_exactly_what_the_code_reads() {
    let schemas = build_tool_schemas(true);
    let delegate = schemas
        .iter()
        .find(|s| s["function"]["name"] == "delegate")
        .expect("an orchestrator is offered delegate");
    let params = &delegate["function"]["parameters"];
    let mut properties: Vec<&str> = params["properties"]
        .as_object()
        .expect("properties")
        .keys()
        .map(String::as_str)
        .collect();
    properties.sort_unstable();
    // `execute_delegate` reads `subtask` and `tier`; nothing else may be offered.
    assert_eq!(properties, ["subtask", "tier"]);
    assert_eq!(params["required"], json!(["subtask"]));
    assert_eq!(params["additionalProperties"], json!(false), "a closed schema");
}
