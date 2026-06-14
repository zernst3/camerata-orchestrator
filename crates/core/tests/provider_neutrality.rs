//! Provider-neutrality proof: the coordinator, gateway, and check runner govern
//! identically regardless of which concrete agent driver is in use.
//!
//! # What this test proves
//!
//! The POSITIONING.md moat claim states: "Camerata's governance gate is
//! provider-neutral by construction." This file turns that assertion into a
//! demonstrated, tested artifact via three independent proofs:
//!
//! ## Proof 1 -- FleetCoordinator governs a non-Claude fake driver identically
//!
//! A `FakeProviderXDriver` (zero lines shared with `ClaudeCliDriver`) implements
//! `AgentDriver` and returns a canned outcome. A `FleetCoordinator` runs a two-
//! stage pipeline with it, using the same scripted `CheckRunner` the unit tests
//! in `src/fleet.rs` use. The coordinator produces the same `FleetReport` shape,
//! the same bounce-and-revise behavior, and the same `is_clean()` semantics --
//! without knowing or caring that the driver is not Claude.
//!
//! ## Proof 2 -- GenericCliDriver argv contains no Claude-specific flags
//!
//! `GenericCliDriver::build_args` with `program = "llm"` and
//! `task_flag = "--prompt"` produces an argv that contains no
//! `--dangerously-skip-permissions`, no `--strict-mcp-config`, no
//! `--allowedTools` -- none of the Claude CLI flags. The runtime is not hard-
//! wired to Claude.
//!
//! ## Proof 3 -- the gateway's evaluate_call has no provider input at all
//!
//! `camerata_gateway::evaluate_call` accepts `(rule_subset, &ToolCall)`. There
//! is no model parameter, no provider field, no session metadata beyond the role's
//! rule-subset. The same call, with the same rule-subset, always yields the same
//! `Decision` -- independent of which driver produced the tool call. The proof is
//! that the function signature itself has no place to put a provider.

use std::path::Path;
use std::sync::Mutex;

use camerata_agent::GenericCliDriver;
use camerata_core::{
    AgentDriver, AgentOutcome, FleetCoordinator, FleetStage, Role, RuleId, ToolCall,
};
use camerata_gateway::{evaluate_call, gov1_rule};
use serde_json::json;

// ─── shared test helpers ──────────────────────────────────────────────────────

fn backend_role() -> Role {
    Role {
        name: "Backend".to_string(),
        rule_subset: vec![RuleId("GOV-1".to_string())],
        allowed_paths: vec!["crates/".to_string()],
    }
}

fn canned_outcome(label: &str) -> AgentOutcome {
    AgentOutcome {
        session_id: format!("fake-provider-x-{label}"),
        result: format!("ok from {label}"),
        cost_usd: None,
        denials: vec![],
    }
}

// ─── Proof 1: FakeProviderXDriver in a FleetCoordinator ──────────────────────

/// A minimal fake driver with zero shared code with ClaudeCliDriver beyond the
/// trait. Records every (role_name, task) call so the test can assert order and
/// content. Returns a canned outcome so no subprocess is needed.
struct FakeProviderXDriver {
    label: String,
    calls: Mutex<Vec<(String, String)>>,
}

impl FakeProviderXDriver {
    fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            calls: Mutex::new(vec![]),
        }
    }
}

#[async_trait::async_trait]
impl AgentDriver for FakeProviderXDriver {
    async fn run(&self, role: &Role, task: &str) -> anyhow::Result<AgentOutcome> {
        self.calls
            .lock()
            .unwrap()
            .push((role.name.clone(), task.to_string()));
        Ok(canned_outcome(&self.label))
    }
}

/// A scripted check runner: returns the next violation set from a queue.
struct ScriptedChecks {
    queue: Mutex<std::collections::VecDeque<Vec<RuleId>>>,
}

impl ScriptedChecks {
    fn new(seq: Vec<Vec<RuleId>>) -> Self {
        Self {
            queue: Mutex::new(seq.into_iter().collect()),
        }
    }
}

#[async_trait::async_trait]
impl camerata_core::CheckRunner for ScriptedChecks {
    async fn check(&self, _role: &Role, _wt: &Path) -> anyhow::Result<Vec<RuleId>> {
        Ok(self.queue.lock().unwrap().pop_front().unwrap_or_default())
    }
}

/// PROOF 1: A FleetCoordinator driven by FakeProviderXDriver (no Claude code)
/// produces the same FleetReport shape and governance behavior as any other driver.
#[tokio::test]
async fn fleet_coordinator_governs_fake_non_claude_driver_identically() {
    // Two stages, each backed by its own FakeProviderXDriver (neither is Claude).
    let driver_a = FakeProviderXDriver::new("provider-x-stage-a");
    let driver_b = FakeProviderXDriver::new("provider-x-stage-b");

    // Stage A: dirty on first check, clean after bounce. Stage B: clean immediately.
    let checks = ScriptedChecks::new(vec![
        vec![RuleId("GOV-1".to_string())], // stage A initial: violation
        vec![],                            // stage A after bounce: clean
        vec![],                            // stage B: clean immediately
    ]);

    let fleet = FleetCoordinator::new(&checks, "/tmp/provider-neutrality-proof-wt");

    let stages = vec![
        FleetStage::new(backend_role(), "implement the feature", &driver_a),
        FleetStage::new(backend_role(), "add a test", &driver_b),
    ];

    let report = fleet
        .run(&stages)
        .await
        .expect("fleet must succeed with a fake non-Claude driver");

    // Same FleetReport shape as any other driver run.
    assert_eq!(report.stages.len(), 2, "both stages must be reported");
    assert!(
        report.is_clean(),
        "fleet must be clean after the bounce resolved stage A"
    );
    assert_eq!(
        report.total_bounces(),
        1,
        "exactly one bounce across the fleet"
    );

    // Stage A: bounced once and is clean.
    let s_a = &report.stages[0];
    assert_eq!(s_a.role_name, "Backend");
    assert!(s_a.report.bounced, "stage A must have bounced");
    assert!(s_a.is_clean(), "stage A must be clean after bounce");

    // The bounce task cited the violated rule id verbatim.
    let calls_a = driver_a.calls.lock().unwrap();
    assert_eq!(
        calls_a.len(),
        2,
        "stage A's driver ran twice: initial + one bounce"
    );
    assert!(
        calls_a[1].1.contains("GOV-1"),
        "the bounce task must cite GOV-1; got: {:?}",
        calls_a[1].1
    );
    assert!(
        calls_a[1].1.contains("REVISION REQUIRED"),
        "the bounce task must carry the revision instruction"
    );
    drop(calls_a);

    // Stage B: ran exactly once, no bounce.
    let s_b = &report.stages[1];
    assert!(!s_b.report.bounced, "stage B must not bounce");
    assert!(s_b.is_clean(), "stage B must be clean");
    let calls_b = driver_b.calls.lock().unwrap();
    assert_eq!(calls_b.len(), 1, "stage B's driver ran exactly once");
    drop(calls_b);

    // KEY assertion: the coordinator never knew which concrete provider ran.
    // It called `driver.run(role, task)` through `&dyn AgentDriver` and got
    // an `AgentOutcome`. The governance (check, bounce, report) is identical
    // whether the driver behind the reference is Claude, FakeProviderX, or
    // GenericCliDriver.
    assert!(
        report.stages[0]
            .report
            .initial_outcome
            .session_id
            .contains("fake-provider-x"),
        "the outcome carries the fake provider's session id, proving FakeProviderX ran"
    );
}

// ─── Proof 2: GenericCliDriver argv contains no Claude-specific flags ─────────

/// PROOF 2: GenericCliDriver::build_args with program "llm" and task_flag
/// "--prompt" produces an argv with no Claude-specific flags. The runtime is
/// not hard-wired to the Claude CLI binary or any of its governance flags.
#[test]
fn generic_cli_driver_build_args_contains_no_claude_flags() {
    let driver = GenericCliDriver::new("llm", "--prompt", &["--model", "gpt-4o"]);
    let role = backend_role();
    let args = driver.build_args(&role, "implement the feature");

    // The binary is not "claude".
    assert_ne!(
        driver.program, "claude",
        "GenericCliDriver program must not be the Claude binary"
    );

    // None of the Claude-CLI governance flags appear in the argv.
    let claude_flags = [
        "--dangerously-skip-permissions",
        "--strict-mcp-config",
        "--allowedTools",
        "--disallowedTools",
        "--output-format",
        "--mcp-config",
        "--add-dir",
    ];
    for flag in claude_flags {
        assert!(
            !args.iter().any(|a| a == flag),
            "GenericCliDriver argv must not contain Claude-specific flag {flag:?}; \
             got args: {args:?}"
        );
    }

    // The task flag and task ARE present (that is the whole point of this driver).
    let prompt_pos = args
        .iter()
        .position(|a| a == "--prompt")
        .expect("--prompt flag must be present in the argv");
    assert_eq!(
        args[prompt_pos + 1],
        "implement the feature",
        "the task must follow the task_flag"
    );

    // The base_args are present before the task flag.
    assert_eq!(args[0], "--model");
    assert_eq!(args[1], "gpt-4o");
}

// ─── Proof 3: evaluate_call has no provider input ────────────────────────────

/// PROOF 3: camerata_gateway::evaluate_call accepts (rule_subset, &ToolCall).
/// There is no model, no provider, no driver reference in the signature.
/// The same call, same rule-subset, always yields the same Decision regardless
/// of which driver produced the ToolCall.
///
/// This proof is partially structural (the function signature has no provider
/// parameter) and partially behavioral (the same call denies from both the
/// "Claude world" and the "generic provider world" rule-subset identically).
#[test]
fn evaluate_call_has_no_provider_input_and_decides_on_tool_call_alone() {
    let forbidden_write = ToolCall {
        tool: "gated_write".to_string(),
        input: json!({ "path": "crates/forbidden/secret.rs", "content": "x" }),
    };
    let clean_write = ToolCall {
        tool: "gated_write".to_string(),
        input: json!({ "path": "crates/core/src/lib.rs", "content": "x" }),
    };

    // Imagine these rule-subsets come from two different sessions: one backed by
    // a Claude driver, one backed by a GenericCliDriver. The rule-subsets are
    // identical because they are derived from the Role, not the driver. The gate
    // receives no information about which driver is behind the session.
    let claude_backed_subset = vec![gov1_rule()];
    let generic_backed_subset = vec![gov1_rule()];

    // Both subsets deny the forbidden write.
    let d_claude = evaluate_call(&claude_backed_subset, &forbidden_write);
    let d_generic = evaluate_call(&generic_backed_subset, &forbidden_write);

    assert!(
        matches!(d_claude, camerata_core::Decision::Deny { .. }),
        "the forbidden write must be denied for a Claude-backed session"
    );
    assert!(
        matches!(d_generic, camerata_core::Decision::Deny { .. }),
        "the forbidden write must be denied for a generic-provider session"
    );

    // Both subsets allow the clean write.
    let a_claude = evaluate_call(&claude_backed_subset, &clean_write);
    let a_generic = evaluate_call(&generic_backed_subset, &clean_write);

    assert!(
        matches!(a_claude, camerata_core::Decision::Allow),
        "a clean write must be allowed for a Claude-backed session"
    );
    assert!(
        matches!(a_generic, camerata_core::Decision::Allow),
        "a clean write must be allowed for a generic-provider session"
    );

    // Structural proof note (cannot be expressed as a runtime assert, so it is
    // stated here as documentation): `evaluate_call` takes `&[RuleId]` and
    // `&ToolCall`. Inspecting the function signature in crates/gateway/src/lib.rs
    // confirms there is no model parameter, no provider enum, no driver reference.
    // The gate is structurally incapable of discriminating by provider.
}
