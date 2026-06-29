//! Hermetic end-to-end WALK of the governed development pipeline.
//!
//! THE CLAIM under test: a single work item can be threaded across the governed-dev
//! pipeline — project setup + onboard, UoW creation, lifecycle progression to
//! Development through the real gate seams, the orchestration/delegation invariants,
//! reconcile-back, and the Camerata-commits step — and the GATE INVARIANTS hold the
//! whole way through. This is the value of an END-TO-END WALK: the seams that other
//! suites prove in isolation are here exercised TOGETHER, in order, on ONE scenario.
//!
//! What this suite OWNS vs. defers:
//!   - `uow_lifecycle_e2e.rs` owns the exhaustive per-transition state-machine net; here
//!     we only assert the run REACHES Development LEGITIMATELY (decision gate + R3.g
//!     contract precondition satisfied via the real `ensure_development_gate`).
//!   - `vcs_action_gate_e2e.rs` owns the VCS-action gate's config→rules→decision pipeline;
//!     here we reference the SAME gate seams in the end-to-end context (children cannot
//!     run git/commit; Camerata is the sole committer) without copying its assertions.
//!   - The HEART of this suite is the orchestration/delegation invariants, asserted
//!     against the REAL gateway/fleet seams.
//!
//! HERMETIC: NO real LLM, NO network, NO `claude`/process spawn. The provider is the
//! in-process native `AppState`; the lifecycle runs through `UowStore` + the real
//! `ensure_development_gate`; delegation uses a NO-SPAWN `ChildDriverFactory` test double
//! (so a `delegate`/`fan_out` exercises the real gating + framing without spending a
//! token). Where a flow would require AI, we assert the STRUCTURE/gate around it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;

use camerata_agent::{
    allowed_tools_for_role, allowed_tools_for_role_with_mode, DELEGATE_TOOL, FAN_OUT_TOOL,
    GATED_WRITE_TOOL,
};
use camerata_core::{AgentDriver, AgentOutcome, Decision, Role, RuleId, ToolCall};
use camerata_gateway::delegate::{
    child_role, ChildDriverFactory, DelegateError, DelegateModels, OrchestratorConfig,
};
use camerata_gateway::fan_out::{run_fan_out, FanOutEntry, FanOutError};
use camerata_gateway::{enforced_gate_rules, evaluate_call, GovernedGateway};
use camerata_server::lifecycle::UowStage;
use camerata_server::{ensure_development_gate, AppState};

use camerata_worktracker::investigation::DecisionRecord;

// ════════════════════════════════════════════════════════════════════════════════════
// Shared fixtures
// ════════════════════════════════════════════════════════════════════════════════════

/// An approved decision for `story` (so the no-code-first gate can open).
fn approved(story: &str, slug: &str) -> DecisionRecord {
    DecisionRecord::ai_proposed(
        story,
        format!("{story}/decision/{slug}"),
        "Decision",
        "Question?",
        "Rationale",
        vec![],
        Utc::now(),
    )
    .approve(Utc::now())
}

/// The per-tier model map the orchestrator delegates to (vision off — the common case).
fn models() -> DelegateModels {
    DelegateModels {
        fast: "claude-haiku-4-5-20251001".to_string(),
        balanced: "claude-sonnet-4-6".to_string(),
        strongest: "claude-opus-4-8".to_string(),
        vision: String::new(),
    }
}

/// A NO-SPAWN child driver that records the role + tools it was handed and returns a fixed
/// marker. This keeps the delegation walk token-free (no `claude` process) while still
/// exercising the REAL gating: the gateway builds it via the factory, frames the subtask,
/// and records gate decisions identically to the live path.
struct RecordingChildDriver {
    /// The role the gateway handed this child (so the test can assert it is gated_write-only,
    /// non-orchestrator, worktree-jailed).
    seen_role: Arc<std::sync::Mutex<Option<Role>>>,
}

#[async_trait]
impl AgentDriver for RecordingChildDriver {
    async fn run(&self, role: &Role, _task: &str) -> anyhow::Result<AgentOutcome> {
        *self.seen_role.lock().unwrap() = Some(role.clone());
        Ok(AgentOutcome {
            session_id: "recording-child".to_string(),
            result: "child did the subtask".to_string(),
            cost_usd: None,
            denials: vec![],
        })
    }
}

/// A `ChildDriverFactory` test double: records every (model, worktree) it is asked to build
/// and returns a [`RecordingChildDriver`]. Implementing the public trait here is the hermetic
/// substitute for the server's real provider-routing factory — same gate contract, no spawn.
#[derive(Clone, Default)]
struct RecordingFactory {
    models: Arc<std::sync::Mutex<Vec<String>>>,
    worktrees: Arc<std::sync::Mutex<Vec<PathBuf>>>,
    last_role: Arc<std::sync::Mutex<Option<Role>>>,
}

impl ChildDriverFactory for RecordingFactory {
    fn build_child(
        &self,
        model: &str,
        worktree: &Path,
        _read_dirs: &[PathBuf],
    ) -> std::io::Result<Box<dyn AgentDriver>> {
        self.models.lock().unwrap().push(model.to_string());
        self.worktrees.lock().unwrap().push(worktree.to_path_buf());
        Ok(Box::new(RecordingChildDriver {
            seen_role: self.last_role.clone(),
        }))
    }
}

/// An ORCHESTRATOR-mode config (depth 0, max 1) wired to a no-spawn factory.
fn orchestrator_cfg(factory: Arc<RecordingFactory>) -> OrchestratorConfig {
    OrchestratorConfig {
        models: models(),
        worktree_root: PathBuf::from("/work/governed-run"),
        gateway_bin: PathBuf::from("/bin/camerata-gateway"),
        depth: 0,
        max_depth: 1,
        child_driver_factory: Some(factory),
    }
}

/// A CHILD's config: depth ALREADY at the cap (1/1), modelling a depth-1 child that tries to
/// delegate/fan-out further. The structural guarantee is that a child never even registers the
/// tool; the depth guard is the belt-and-suspenders this exercises.
fn child_cfg(factory: Arc<RecordingFactory>) -> OrchestratorConfig {
    OrchestratorConfig {
        depth: 1,
        ..orchestrator_cfg(factory)
    }
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 1 — Project setup + onboard, then create a UoW from a work item.
// ════════════════════════════════════════════════════════════════════════════════════

#[test]
fn scope1_project_setup_onboard_repo_and_create_uow_from_workitem() {
    let state = AppState::seeded();
    let repo = "o/r".to_string();

    // Project setup: an active project covering the work item's repo.
    let project = state
        .projects()
        .create("GovernedRun", vec![repo.clone()])
        .expect("create the active project");

    // Onboard the repo (the apply step marks it onboarded on the project).
    state
        .projects()
        .update(&project.id, |p| p.mark_onboarded(&[repo.clone()]))
        .expect("mark the repo onboarded");
    let onboarded = state
        .projects()
        .get(&project.id)
        .expect("project still exists")
        .onboarded;
    assert!(
        onboarded.contains(&repo),
        "the repo is recorded as onboarded on the project"
    );

    // Create a UoW from a work item (the handler's get_or_create, project-scoped dedup).
    let story_id = "o/r#7";
    let first = state.uow().get_or_create(story_id);
    assert_eq!(first.story_id, story_id);
    assert_eq!(first.stage, UowStage::Intake, "a new UoW starts at Intake");
    // The UoW is visible in the project's view (so a second from-workitem call dedups).
    let visible = state
        .uow()
        .list_for_project(&project.id, &project.repos)
        .iter()
        .any(|u| u.story_id == story_id);
    assert!(visible, "the new UoW is in the project's view");
    // A second creation of the same work item never duplicates.
    let _ = state.uow().get_or_create(story_id);
    let count = state
        .uow()
        .list()
        .iter()
        .filter(|u| u.story_id == story_id)
        .count();
    assert_eq!(count, 1, "the same work item never creates a duplicate UoW");
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 2 — Stage progression to Development through the REAL `ensure_development_gate`
//   seam: the run REACHES Development LEGITIMATELY (decision gate + R3.g contract).
// ════════════════════════════════════════════════════════════════════════════════════

#[test]
fn scope2_run_reaches_development_only_after_gates_are_satisfied() {
    let state = AppState::seeded();
    let story_id = "o/r#7";

    // No decisions yet: the gate BLOCKS, the stage does NOT advance to Development.
    let blocked = ensure_development_gate(&state, story_id);
    assert!(blocked.is_err(), "no approved decisions => gate blocks");
    assert_ne!(
        state.uow().get_or_create(story_id).stage,
        UowStage::Development,
        "a blocked run never reaches Development"
    );

    // This story crosses a contract boundary with NO contract: still blocked, now on R3.g.
    state.uow().set_decisions(story_id, vec![approved(story_id, "a")]);
    state.uow().set_contract(story_id, "", true);
    let r3g = ensure_development_gate(&state, story_id).unwrap_err();
    assert!(
        r3g.contains("contract") && r3g.contains("R3.g"),
        "a boundary-crossing story with no contract is blocked on R3.g: {r3g}"
    );

    // Write the contract: BOTH preconditions satisfied -> the gate OPENS and the lifecycle
    // is driven legitimately to Development through the real transition seams.
    state
        .uow()
        .set_contract(story_id, "GET /widgets returns {id, name}.", true);
    assert!(
        ensure_development_gate(&state, story_id).is_ok(),
        "decisions approved + contract written => the gate opens"
    );
    assert_eq!(
        state.uow().get_or_create(story_id).stage,
        UowStage::Development,
        "the run reaches Development legitimately (not forced) through the gate"
    );
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 3 — THE HEART: orchestration/delegation invariants, against the REAL gateway/fleet
//   seams, exercised together in the governed-run context.
// ════════════════════════════════════════════════════════════════════════════════════

#[test]
fn scope3a_delegate_and_fan_out_are_orchestrator_only() {
    // Only the orchestrator/lead role carries `delegate` + `fan_out`; every non-orchestrator
    // agent (and every delegate child) gets `gated_write` ONLY. This is the structural
    // depth-1 guarantee: a child's tool surface cannot include the delegation tools.
    let role = Role {
        name: "anything".to_string(),
        rule_subset: vec![RuleId("GOV-1".to_string())],
        allowed_paths: vec!["/work/governed-run".to_string()],
    };

    // Non-orchestrator (the child surface): gated_write present, delegate/fan_out ABSENT.
    let child_tools = allowed_tools_for_role(&role);
    assert!(child_tools.iter().any(|t| t == GATED_WRITE_TOOL));
    assert!(
        !child_tools.iter().any(|t| t == DELEGATE_TOOL),
        "a non-orchestrator agent must never get `delegate`"
    );
    assert!(
        !child_tools.iter().any(|t| t == FAN_OUT_TOOL),
        "a non-orchestrator agent must never get `fan_out`"
    );

    // Orchestrator surface: gated_write + delegate + fan_out.
    let lead_tools = allowed_tools_for_role_with_mode(&role, true);
    assert!(lead_tools.iter().any(|t| t == GATED_WRITE_TOOL));
    assert!(
        lead_tools.iter().any(|t| t == DELEGATE_TOOL),
        "only the orchestrator gets `delegate`"
    );
    assert!(
        lead_tools.iter().any(|t| t == FAN_OUT_TOOL),
        "only the orchestrator gets `fan_out`"
    );
}

#[tokio::test]
async fn scope3b_a_child_cannot_delegate_depth_over_1_is_refused() {
    // A depth-1 child (already at the cap) is refused a further delegation BEFORE any spawn —
    // the belt-and-suspenders depth guard over the structural depth-1 guarantee.
    let factory = Arc::new(RecordingFactory::default());
    let child = child_cfg(factory.clone());
    assert!(
        !child.may_delegate(),
        "a depth-1 child (max_depth 1) may not delegate further"
    );

    let err = camerata_gateway::delegate::run_delegated(
        &child,
        vec![RuleId("GOV-1".to_string())],
        "do x",
        "fast",
    )
    .await
    .unwrap_err();
    assert_eq!(
        err,
        DelegateError::DepthExceeded {
            depth: 1,
            max_depth: 1
        },
        "depth>1 delegation is refused"
    );

    // And fan_out from a depth-capped child is likewise refused before any spawn.
    let fan_err = run_fan_out(
        &child,
        vec![RuleId("GOV-1".to_string())],
        vec![FanOutEntry {
            repo: "o/r".to_string(),
            domain: "backend".to_string(),
            subtask: "do x".to_string(),
        }],
    )
    .await
    .unwrap_err();
    assert_eq!(
        fan_err,
        FanOutError::DepthExceeded {
            depth: 1,
            max_depth: 1
        }
    );

    // No spawn happened on either refused path (token-free): the factory was never called.
    assert!(
        factory.models.lock().unwrap().is_empty(),
        "a refused delegation/fan-out spawns nothing"
    );
}

#[tokio::test]
async fn scope3c_delegate_child_receives_gated_write_only_jailed_non_orchestrator() {
    // The orchestrator delegates ONE subtask. The gateway builds the child via the factory
    // and hands it a role that is gated_write-only, worktree-jailed, and non-orchestrator.
    let factory = Arc::new(RecordingFactory::default());
    let cfg = orchestrator_cfg(factory.clone());

    let out = camerata_gateway::delegate::run_delegated(
        &cfg,
        vec![RuleId("GOV-1".to_string())],
        "implement the widget endpoint",
        "balanced",
    )
    .await
    .expect("delegate returns the child's framed output");
    assert!(out.contains("child did the subtask"), "the child ran: {out}");

    // The factory was asked for EXACTLY the balanced model (per-tier resolution).
    assert_eq!(
        factory.models.lock().unwrap().clone(),
        vec!["claude-sonnet-4-6".to_string()]
    );
    // The child was jailed to the orchestrator's shared worktree root.
    assert_eq!(
        factory.worktrees.lock().unwrap().clone(),
        vec![PathBuf::from("/work/governed-run")]
    );

    // The role handed to the child: gated_write ONLY, worktree-jailed, NOT orchestrator.
    let role = factory
        .last_role
        .lock()
        .unwrap()
        .clone()
        .expect("the child received a role");
    let tools = allowed_tools_for_role(&role);
    assert!(tools.iter().any(|t| t == GATED_WRITE_TOOL));
    for forbidden in [
        DELEGATE_TOOL,
        FAN_OUT_TOOL,
        // The raw built-in write/exec/spawn tools are on the disallow list for EVERY agent;
        // they are never in any allowed surface. A child surface is gated_write + read-only.
        "Task",
        "Bash",
        "Write",
        "Edit",
        "MultiEdit",
        "NotebookEdit",
    ] {
        assert!(
            !tools.iter().any(|t| t == forbidden),
            "a delegate child must NOT be granted `{forbidden}`: {tools:?}"
        );
    }
    assert_eq!(
        role.allowed_paths,
        vec!["/work/governed-run".to_string()],
        "the child is jailed to the shared worktree"
    );
}

#[test]
fn scope3d_child_role_is_worktree_jailed_and_not_delegate_capable() {
    // The `child_role` constructor (the role every gated child is born with) carries the
    // orchestrator's rule subset, is jailed to the worktree, and yields a tool surface
    // with NO delegate/fan_out and NONE of the raw write/exec/spawn built-ins.
    let role = child_role(
        vec![RuleId("GOV-1".to_string())],
        Path::new("/work/governed-run"),
    );
    assert_eq!(role.allowed_paths, vec!["/work/governed-run".to_string()]);
    let tools = allowed_tools_for_role(&role);
    assert!(tools.iter().any(|t| t == GATED_WRITE_TOOL));
    assert!(!tools.iter().any(|t| t == DELEGATE_TOOL));
    assert!(!tools.iter().any(|t| t == FAN_OUT_TOOL));
}

#[test]
fn scope3e_camerata_is_the_sole_committer_children_cannot_embed_git_state_mutation() {
    // Camerata manages all git state; an agent cannot run git/commit (no Bash) and cannot
    // smuggle a state-mutating git command into a written file — the gateway gate denies it.
    // This is the SAME gate seam `vcs_action_gate_e2e` references, asserted here as the
    // "sole committer" invariant of the governed run.
    let subset = vec![RuleId("SEC-NO-GIT-STATE-MUTATION-1".to_string())];

    // A gated write whose content embeds `git reset --hard` is DENIED. (`evaluate_call`'s
    // abstract gate recognises the bare `write` tool — the same seam the MCP `gated_write`
    // call funnels into; see `is_write_tool`.)
    let smuggle = ToolCall {
        tool: "write".to_string(),
        input: serde_json::json!({
            "path": "scripts/reset.sh",
            "content": "#!/bin/sh\ngit reset --hard HEAD~1\n",
        }),
    };
    match evaluate_call(&subset, &smuggle) {
        Decision::Deny { rule, .. } => {
            assert_eq!(rule.0, "SEC-NO-GIT-STATE-MUTATION-1");
        }
        Decision::Allow => panic!("embedding a state-mutating git command must be denied"),
    }

    // A benign write is allowed (the gate is permissive about calls, not blanket-deny).
    let benign = ToolCall {
        tool: "write".to_string(),
        input: serde_json::json!({
            "path": "src/widget.rs",
            "content": "pub fn widget() {}\n",
        }),
    };
    assert!(matches!(evaluate_call(&subset, &benign), Decision::Allow));

    // And the child's tool surface has no Bash at all, so it cannot run `git commit` directly:
    // committing is structurally reserved to Camerata.
    let role = child_role(subset.clone(), Path::new("/work/governed-run"));
    let tools = allowed_tools_for_role(&role);
    assert!(!tools.iter().any(|t| t == "Bash"), "children have no shell");
}

#[test]
fn scope3f_security_floor_is_always_active_and_not_selection_gated() {
    // The SEC-*/ARCH-* security floor is the deterministic, always-on layer: every rule with a
    // real enforcement arm rides along in `enforced_gate_rules()`, and the brownfield audit
    // floor (`onboard::AUDIT_RULES`) is a fixed SEC/ARCH set — neither is selection-gated, so
    // it cannot be turned off by a project's rule selection.
    let enforced = enforced_gate_rules();
    let enforced_ids: Vec<&str> = enforced.iter().map(|r| r.0.as_str()).collect();
    for floor in [
        "SEC-NO-HARDCODED-SECRETS-1",
        "SEC-NO-RAW-SQL-CONCAT-1",
        "SEC-NO-PRIVATE-KEY-1",
        "SEC-NO-GIT-STATE-MUTATION-1",
    ] {
        assert!(
            enforced_ids.contains(&floor),
            "the security floor rule `{floor}` is always enforced (not selection-gated): {enforced_ids:?}"
        );
    }

    // The brownfield audit floor is a fixed SEC/ARCH set, applied regardless of selection.
    for floor in [
        "SEC-NO-HARDCODED-SECRETS-1",
        "SEC-NO-RAW-SQL-CONCAT-1",
        "ARCH-NO-SECRETS-IN-URL-1",
        "SEC-NO-PRIVATE-KEY-1",
    ] {
        assert!(
            camerata_server::onboard::AUDIT_RULES.contains(&floor),
            "the brownfield audit floor always includes `{floor}` (cannot be turned off)"
        );
    }

    // Proof the floor FIRES with no rule selection at all: a real hardcoded secret is found by
    // the deterministic audit even though the project selected zero rules.
    let files = vec![(
        "src/config.rs".to_string(),
        "pub const AWS_SECRET_ACCESS_KEY: &str = \"AKIAIOSFODNN7EXAMPLEKEYDATA1234567890ABC\";\n"
            .to_string(),
    )];
    let findings = camerata_server::onboard::audit_files("o/r", &files);
    assert!(
        findings.iter().any(|f| f.rule_id == "SEC-NO-HARDCODED-SECRETS-1"),
        "the security floor fires on a real secret with NO rule selection: {findings:?}"
    );
}

#[test]
fn scope3g_unknown_session_fails_closed_at_the_gate() {
    // A governed run binds every spawned agent's session to a role at spawn. A call from an
    // UNBOUND session fails CLOSED (deny) — the gate never vouches for an un-spawned agent.
    let gw = GovernedGateway::new();
    let call = ToolCall {
        tool: GATED_WRITE_TOOL.to_string(),
        input: serde_json::json!({ "path": "src/x.rs", "content": "fn x() {}" }),
    };
    let decision = gw.try_evaluate(
        &camerata_core::SessionId("never-bound".to_string()),
        &call,
    );
    assert!(
        decision.is_err(),
        "an unbound session is a gate error (fail-closed), not a silent allow"
    );
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 4 — Reconcile the repo back into the project: project state mirrors what was
//   emitted into the repo's gate config.
// ════════════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn scope4_reconcile_local_mirrors_emitted_gate_config_into_project_state() {
    use camerata_server::arm::{arm_files_for_repo, ArmRule};
    use camerata_server::reconcile::{adopt_from_applied, reconcile_repos_local};

    let repo_spec = "o/r".to_string();
    let repo_dir = tempfile::tempdir().unwrap();

    // EMIT the governance arm-files into the repo (the apply step) — a corpus rule + a custom.
    let base = ArmRule {
        id: "RULE-A".to_string(),
        title: "A".to_string(),
        directive: "Do A.".to_string(),
        option: None,
        enforcement: "prose".to_string(),
        scope: "repo-local".to_string(),
        conformance: None,
        repos: vec![repo_spec.clone()],
    };
    let custom = camerata_server::project::CustomRule {
        name: "house-style".to_string(),
        body: "Prefer explicit.".to_string(),
        domain: "*".to_string(),
        repos: vec![repo_spec.clone()],
    };
    let files = arm_files_for_repo(&[&base], &[&custom]);
    for (rel, content) in &files {
        let path = repo_dir.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
    }

    // RECONCILE from the local working copy (no token needed — the local reader is first).
    let sources = vec![(repo_spec.clone(), Some(repo_dir.path().to_path_buf()))];
    let applied = reconcile_repos_local(&sources, "").await;
    let applied_ids: Vec<&str> = applied.iter().map(|a| a.id.as_str()).collect();
    assert!(
        applied_ids.contains(&"RULE-A"),
        "reconcile reads the emitted base rule back from the repo: {applied_ids:?}"
    );
    assert!(
        applied_ids.contains(&"CUSTOM-house-style"),
        "reconcile reads the emitted custom rule back from the repo: {applied_ids:?}"
    );

    // ADOPT into project-state shape: the project now MIRRORS what was emitted in the repo.
    let (selections, customs) = adopt_from_applied(&applied);
    assert!(
        selections.iter().any(|s| s.rule_id == "RULE-A" && s.repos.contains(&repo_spec)),
        "the base selection mirrors the emit, scoped to the repo"
    );
    let house = customs
        .iter()
        .find(|c| c.name == "house-style")
        .expect("the custom rule is rebuilt from the round-tripped body");
    assert_eq!(house.body, "Prefer explicit.", "the custom body round-trips");
    assert!(house.repos.contains(&repo_spec), "the custom scoping round-trips");
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 5 — The Camerata-commits step: Camerata is the ONLY writer. We assert the gate
//   posture around the commit, in the end-to-end context — children never commit; the
//   sole-committer invariant is the gate's, not an agent's.
// ════════════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn scope5_fan_out_workers_are_partitioned_and_camerata_remains_sole_committer() {
    // A multi-repo UoW fans out across two repos. Each worker is write-isolated to its OWN
    // partition (no two workers share a repo), is gated_write-only, and CANNOT commit — only
    // Camerata commits, after the gated workers write. We exercise the real `run_fan_out`
    // through the no-spawn factory.
    let factory = Arc::new(RecordingFactory::default());
    let cfg = orchestrator_cfg(factory.clone());

    // Partition collision is refused (write-isolation invariant): two entries, same repo.
    let collide = run_fan_out(
        &cfg,
        vec![RuleId("GOV-1".to_string())],
        vec![
            FanOutEntry {
                repo: "shared".to_string(),
                domain: "backend".to_string(),
                subtask: "x".to_string(),
            },
            FanOutEntry {
                repo: "shared".to_string(),
                domain: "frontend".to_string(),
                subtask: "y".to_string(),
            },
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(collide, FanOutError::DuplicateRepo("shared".to_string()));

    // A valid fan-out across two DISTINCT repos: both workers run, each jailed to its repo.
    let results = run_fan_out(
        &cfg,
        vec![RuleId("GOV-1".to_string())],
        vec![
            FanOutEntry {
                repo: "api".to_string(),
                domain: "backend".to_string(),
                subtask: "implement endpoint".to_string(),
            },
            FanOutEntry {
                repo: "web".to_string(),
                domain: "frontend".to_string(),
                subtask: "wire the view".to_string(),
            },
        ],
    )
    .await
    .expect("a valid two-repo fan-out runs");
    assert_eq!(results.len(), 2, "both workers produced a result");
    assert!(!results.iter().any(|r| r.incomplete), "no worker stalled");

    // Each worker was jailed to its OWN partition under the shared worktree root.
    let jails = factory.worktrees.lock().unwrap().clone();
    assert!(jails.contains(&PathBuf::from("/work/governed-run/api")));
    assert!(jails.contains(&PathBuf::from("/work/governed-run/web")));

    // The role the LAST worker received is gated_write-only, non-orchestrator (no fan_out):
    // a fan-out worker is a depth-1 child, so it can never fan out or commit further.
    let role = factory
        .last_role
        .lock()
        .unwrap()
        .clone()
        .expect("a worker received a role");
    let tools = allowed_tools_for_role(&role);
    assert!(tools.iter().any(|t| t == GATED_WRITE_TOOL));
    assert!(!tools.iter().any(|t| t == FAN_OUT_TOOL));
    assert!(!tools.iter().any(|t| t == DELEGATE_TOOL));
    assert!(!tools.iter().any(|t| t == "Bash"), "a worker has no shell -> cannot git/commit");
}
