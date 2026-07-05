//! `camerata-fleet`: reusable governed-fleet build logic.
//!
//! Extracted from the CLI demos so that any consumer (the CLI, the UI, a test
//! harness) can run a governed build pipeline without depending on the binary
//! crate. The CLI re-exports nothing here; callers import this crate directly.
//!
//! # What lives here
//!
//! - Scaffolding helpers: [`governed_role`], [`locate_gateway_bin`],
//!   [`scaffold_crate`], [`run_cargo`], [`tail_lines`], [`NoopChecks`],
//!   [`CargoOutcome`], [`FLEET_DOMAINS`], [`DEFAULT_CORPUS_PATH`].
//! - Stage-task helpers: [`stage_task_for`], [`describe_task_kind`].
//! - The high-level runner: [`build_from_plan`], [`BuildEvent`], [`BuildOutcome`].
//! - Model-tiering: [`tier`] module — [`tier::CapabilityBand`], [`tier::TierMap`],
//!   [`tier::classify_task`], and [`build_from_plan_with_tier_map`].

use std::path::{Path, PathBuf};

use camerata_agent::{prepare_session, HeartbeatFn, GATED_WRITE_TOOL};
use camerata_checks::{runner_for_worktree, runner_for_worktree_with_heartbeat};
use camerata_core::{AgentDriver, CheckOutcome, CheckRunner, FleetCoordinator, FleetStage, Role};
use camerata_gateway::enforced_gate_rules;
use camerata_intake::{Plan, PlanTask, TaskKind};
use camerata_rules::role_from_corpus;
pub use camerata_rules::DEFAULT_CORPUS_PATH;

pub mod gate_probe;
pub mod orchestrator;
pub mod tier;

// ─── Corpus / domain constants ────────────────────────────────────────────────

/// Domains the fleet roles are scoped to in the corpus selection. The code the
/// agents write is plain Rust, so the `rust` family (plus universal `*` rules)
/// is the relevant slice; `agentic` rides along because these ARE agentic runs.
pub const FLEET_DOMAINS: &[&str] = &["rust", "agentic"];

// ─── NoopChecks ───────────────────────────────────────────────────────────────

/// A layer-2 check runner that reports NO structural violations.
///
/// The demos' real layer-2 verification is `cargo build` plus `cargo test` on
/// the finished crate AFTER the fleet completes (a partially-written crate
/// mid-fleet would not build, so per-stage cargo checks would be meaningless).
/// The fleet's bounce-and-revise machinery is still exercised end-to-end by
/// the coordinator tests; here we keep the layer-2 seam a no-op and let the
/// final cargo gates be the judge.
pub struct NoopChecks;

#[async_trait::async_trait]
impl CheckRunner for NoopChecks {
    async fn check(&self, _role: &Role, _worktree: &Path) -> anyhow::Result<CheckOutcome> {
        Ok(CheckOutcome::clean())
    }
}

/// Like the old `layer2_runner`, but also wires `on_activity` into every Rust
/// [`camerata_checks::RustCheckRunner`] sub-runner so cargo subprocess output
/// (fmt/clippy/test) fires heartbeats on the tracked run while the layer-2
/// gate is executing. When `on_activity` is `None` the behaviour is identical
/// to [`layer2_runner`].
///
/// Used exclusively by the `_and_activity` fleet functions, where a
/// `HeartbeatFn` pointing at `RunStore::touch_activity` is already in scope.
fn layer2_runner_with_activity(
    worktree: &Path,
    skip_layer2: bool,
    on_activity: Option<&HeartbeatFn>,
) -> Box<dyn CheckRunner> {
    if skip_layer2 {
        return Box::new(NoopChecks);
    }
    match on_activity {
        Some(cb) => runner_for_worktree_with_heartbeat(worktree, cb.clone()),
        None => runner_for_worktree(worktree),
    }
}

// ─── governed_role ────────────────────────────────────────────────────────────

/// Build a governed role from the real corpus, named `role_name`, and ensure
/// EVERY gateway-enforced gate rule is in the delivered subset so the per-session
/// governance is genuinely active, the same honest blend the live single-agent
/// demo uses. The enforced set comes from [`enforced_gate_rules`], so a rule added
/// to the gateway registry is automatically applied here with no edit.
pub async fn governed_role(role_name: &str) -> anyhow::Result<Role> {
    let corpus = Path::new(DEFAULT_CORPUS_PATH);
    let mut role = role_from_corpus(corpus, role_name, FLEET_DOMAINS, &[]).await?;

    for gate_rule in enforced_gate_rules() {
        if !role.rule_subset.contains(&gate_rule) {
            role.rule_subset.insert(0, gate_rule);
        }
    }
    Ok(role)
}

// ─── locate_gateway_bin ───────────────────────────────────────────────────────

/// Locate the built `camerata-gateway` binary (release preferred, debug
/// fallback).
///
/// This crate lives at `crates/fleet`, so `CARGO_MANIFEST_DIR` is
/// `<workspace_root>/crates/fleet`. Two `.parent()` calls reach the workspace
/// root, then we look in `target/{release,debug}`. The two-parent logic is
/// identical to the original CLI version.
pub fn locate_gateway_bin() -> anyhow::Result<PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| anyhow::anyhow!("cannot locate workspace root from {manifest_dir:?}"))?;

    for profile in ["release", "debug"] {
        let candidate = workspace_root
            .join("target")
            .join(profile)
            .join("camerata-gateway");
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    anyhow::bail!(
        "camerata-gateway binary not found under {}/target/{{release,debug}}. \
         Build it first: `cargo build -p camerata-gateway`.",
        workspace_root.display()
    )
}

// ─── scaffold_crate ───────────────────────────────────────────────────────────

/// Scaffold a fresh cargo library crate at `dir` (the shared worktree).
///
/// Writes a `Cargo.toml` and a placeholder `src/lib.rs`. The placeholder is
/// overwritten by the first agent's governed write; it exists only so the
/// directory is a valid (if empty) crate before the agents run.
pub fn scaffold_crate(dir: &Path, crate_name: &str) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir.join("src"))?;
    let cargo_toml = format!(
        "[package]\nname = \"{crate_name}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\n"
    );
    std::fs::write(dir.join("Cargo.toml"), cargo_toml)?;
    std::fs::write(
        dir.join("src").join("lib.rs"),
        "// placeholder — to be overwritten by the first governed agent\n",
    )?;
    Ok(())
}

// ─── CargoOutcome / run_cargo ─────────────────────────────────────────────────

/// The result of running `cargo <subcommand>` on the produced crate.
pub struct CargoOutcome {
    /// Whether the cargo invocation exited successfully.
    pub success: bool,
    /// Captured stdout from the cargo process.
    pub stdout: String,
    /// Captured stderr from the cargo process.
    pub stderr: String,
}

/// Run `cargo <subcommand>` in `dir` and capture its outcome.
pub async fn run_cargo(dir: &Path, subcommand: &str) -> anyhow::Result<CargoOutcome> {
    let out = tokio::process::Command::new("cargo")
        .arg(subcommand)
        .current_dir(dir)
        .kill_on_drop(true)
        .output()
        .await?;
    Ok(CargoOutcome {
        success: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

// ─── tail_lines ───────────────────────────────────────────────────────────────

/// Return the last `n` lines of `s` as owned strings (for bounded output).
pub fn tail_lines(s: &str, n: usize) -> Vec<String> {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].iter().map(|l| l.to_string()).collect()
}

// ─── stage_task_for / describe_task_kind ──────────────────────────────────────

/// Convert ONE plan task into a precise governed-fleet task instruction.
///
/// The plan's `description` is the engineering intent; this wraps it with the
/// concrete governed-write contract (the agent's ONLY mutation path is the
/// gated tool, written once to the shared `lib.rs`). Earlier stages' writes
/// are visible to later ones because the worktree is shared, so a test stage
/// is told to READ the implementer's file first.
pub fn stage_task_for(task: &PlanTask, lib_path_display: &str, is_first: bool) -> String {
    let tool = GATED_WRITE_TOOL;
    let shared_note = if is_first {
        format!(
            "You are the FIRST agent. OVERWRITE the file at {lib_path_display} with \
             a complete, self-contained Rust library module."
        )
    } else {
        format!(
            "An earlier agent has already written {lib_path_display} in this same \
             crate. FIRST read {lib_path_display} to see what exists, then rewrite \
             it to ADD your contribution while PRESERVING the existing code exactly."
        )
    };

    format!(
        "You are a governed agent in a Product-Owner-mode build fleet. Your ONLY \
         way to write files is the `{tool}` tool; use it exactly once.\n\n\
         {shared_note}\n\n\
         Your task ({kind}): {description}\n\n\
         Hard constraints: the file must be valid Rust that compiles as a library \
         crate on its own; do NOT use `unsafe`; do NOT add external dependencies \
         (the crate has none); derive `Debug`, `Clone`, `PartialEq` on structs. \
         Use `f64` for decimal/money fields and `String` for dates (keep it \
         dependency-free). Call `{tool}` with the path {lib_path_display} and the \
         FULL file content, then report the tool's result.",
        tool = tool,
        shared_note = shared_note,
        kind = task.kind.label(),
        description = task.description,
    )
}

/// A one-liner describing what a task kind contributes, for stage listings.
pub fn describe_task_kind(kind: TaskKind) -> &'static str {
    match kind {
        TaskKind::Database => "persistence/schema",
        TaskKind::Backend => "domain types / API",
        TaskKind::Frontend => "views/screens",
        TaskKind::Test => "tests over the produced code",
    }
}

// ─── BuildEvent / BuildOutcome ────────────────────────────────────────────────

/// Progress events emitted as a governed build runs, for a UI to render.
#[derive(Debug, Clone)]
pub enum BuildEvent {
    /// The crate worktree is being scaffolded.
    Scaffolding,
    /// A fleet stage has started (zero-indexed `index` out of `total`).
    StageStarted {
        /// Zero-based index of this stage.
        index: usize,
        /// Total number of stages in this fleet.
        total: usize,
        /// The role name for this stage.
        role: String,
        /// The task-kind label for this stage.
        kind: String,
    },
    /// The tier/model a stage's spawned agent runs on, and whether it is the
    /// lead/orchestrator (the strongest tier that may delegate). Emitted right after
    /// [`BuildEvent::StageStarted`] so the activity log shows the routing per agent.
    AgentTier {
        /// Zero-based index of this stage.
        index: usize,
        /// The role name for this stage.
        role: String,
        /// The concrete model id this stage's agent runs on.
        model: String,
        /// Whether this stage is the lead/orchestrator (delegate-capable).
        is_lead: bool,
    },
    /// The layer-2 (post-task lint/test) check RESULT for a stage: whether it passed
    /// clean and, if not, the rule ids it left violated. Emitted per stage from the
    /// finished stage report data — observability over what the check decided; it does
    /// not change the bounce behaviour.
    Layer2Result {
        /// Zero-based index of this stage.
        index: usize,
        /// Total number of stages in this fleet.
        total: usize,
        /// Whether the stage ended with NO residual layer-2 violations.
        passed: bool,
        /// The violated rule ids (empty when `passed`).
        violated_rules: Vec<String>,
    },
    /// A bounce-and-revise iteration occurred for a stage: the stage was dirty after
    /// layer 2 and the violated rules were sent back to the agent to revise. Emitted
    /// per dirty stage (the coordinator caps the loop; this records that a revise pass
    /// ran). Observability only — the loop guard / cap is unchanged.
    ReviseIteration {
        /// Zero-based index of this stage.
        index: usize,
        /// The rule ids that were cited back to the agent on the initial bounce.
        violated_rules: Vec<String>,
    },
    /// A fleet stage has finished.
    StageFinished {
        /// Zero-based index of this stage.
        index: usize,
        /// Total number of stages in this fleet.
        total: usize,
        /// Whether this stage ended with no residual layer-2 violations.
        clean: bool,
        /// Whether this stage required a bounce-and-revise pass.
        bounced: bool,
        /// The session id from the initial agent run.
        session_id: String,
    },
    /// The cargo verification step (build and test) is about to run.
    Verifying,
    /// The governed build has finished. Fields mirror [`BuildOutcome`].
    Done {
        /// Whether `cargo build` succeeded.
        compiled: bool,
        /// Whether `cargo test` succeeded.
        tests_passed: bool,
    },
}

/// The result of a governed build from a plan.
#[derive(Debug, Clone)]
pub struct BuildOutcome {
    /// Whether `cargo build` succeeded on the produced crate.
    pub compiled: bool,
    /// Whether `cargo test` succeeded on the produced crate.
    pub tests_passed: bool,
    /// Whether every fleet stage had a non-empty session id (all agents ran).
    pub all_agents_ran: bool,
    /// Whether the final `src/lib.rs` is a real governed write (non-placeholder).
    pub wrote_through_gate: bool,
    /// Total number of bounce-and-revise passes across all fleet stages.
    pub total_bounces: usize,
    /// Whether every stage ended with no residual layer-2 violations.
    pub fleet_clean: bool,
    /// Path to the produced `src/lib.rs` file.
    pub produced_path: PathBuf,
    /// Byte length of the produced file content.
    pub produced_bytes: usize,
}

// ─── build_from_plan ─────────────────────────────────────────────────────────

/// Run the governed fleet to build `plan` into a temp crate worktree under
/// `root`, gated by the Rust gateway at `gateway_bin`. Emits [`BuildEvent`]s
/// via `on_event` as it progresses. Pure plumbing: zero model decisions live
/// here (the agents make the model calls behind the injected drivers).
///
/// The crate name used for the generated worktree is `camerata_app`.
///
/// To pin a specific model for every agent in the fleet, use
/// [`build_from_plan_with_model`]. To raise the bounce-and-revise ceiling, use
/// [`build_from_plan_with_iterations`]. This wrapper passes `None` (CLI default
/// model) and a loop-guard ceiling of `1` bounce-and-revise pass per stage.
pub async fn build_from_plan(
    plan: &Plan,
    root: &Path,
    gateway_bin: &Path,
    on_event: &(dyn Fn(BuildEvent) + Send + Sync),
) -> anyhow::Result<BuildOutcome> {
    build_from_plan_with_model_and_iterations(plan, root, gateway_bin, None, 1, on_event).await
}

/// Run the governed fleet with an explicit `model` id threaded to every
/// `claude -p` agent (loop-guard ceiling `1`). `model = None` means each agent
/// uses the CLI's own default — identical to [`build_from_plan`].
///
/// All agents in the fleet share the same model choice: the fleet is built
/// around a single operator intent, and mixing model tiers mid-fleet creates
/// inconsistent governance context. If per-agent tiering is ever needed, it
/// should be wired at the [`FleetStage`] level.
pub async fn build_from_plan_with_model(
    plan: &Plan,
    root: &Path,
    gateway_bin: &Path,
    model: Option<&str>,
    on_event: &(dyn Fn(BuildEvent) + Send + Sync),
) -> anyhow::Result<BuildOutcome> {
    build_from_plan_with_model_and_iterations(plan, root, gateway_bin, model, 1, on_event).await
}

/// Like [`build_from_plan`], but caps each stage's bounce-and-revise loop at
/// `max_iterations` passes (the loop guard, #29). `1` reproduces the default
/// single-bounce behaviour exactly; higher values let a stuck stage retry the
/// revise pass before its residual violations are surfaced for human review.
pub async fn build_from_plan_with_iterations(
    plan: &Plan,
    root: &Path,
    gateway_bin: &Path,
    max_iterations: usize,
    on_event: &(dyn Fn(BuildEvent) + Send + Sync),
) -> anyhow::Result<BuildOutcome> {
    build_from_plan_with_model_and_iterations(
        plan,
        root,
        gateway_bin,
        None,
        max_iterations,
        on_event,
    )
    .await
}

/// The full governed-fleet build: an explicit `model` for every agent AND a
/// `max_iterations` loop-guard ceiling per stage. The other entry points are thin
/// wrappers over this. All agents share the one model choice.
pub async fn build_from_plan_with_model_and_iterations(
    plan: &Plan,
    root: &Path,
    gateway_bin: &Path,
    model: Option<&str>,
    max_iterations: usize,
    on_event: &(dyn Fn(BuildEvent) + Send + Sync),
) -> anyhow::Result<BuildOutcome> {
    build_from_plan_with_model_iterations_and_layer2(
        plan,
        root,
        gateway_bin,
        model,
        max_iterations,
        false,
        on_event,
    )
    .await
}

/// Like [`build_from_plan_with_model_and_iterations`], but with an explicit
/// `skip_layer2` bootstrap flag.
///
/// `skip_layer2 = false` is identical to [`build_from_plan_with_model_and_iterations`]
/// (the real, language-matched layer-2 runner). `skip_layer2 = true` runs this ONE run
/// with a no-op layer-2 runner ([`NoopChecks`]) so a brownfield repo can land the linters
/// /checkers layer-2 needs without tripping fail-closed "could-not-run". This skips ONLY
/// layer 2 — layer 1 (the deny-before-write gate) is unchanged. See
/// `docs/decisions/2026-06-22_ci_wiring_both_layers_and_layer2_bootstrap_bypass.md`.
pub async fn build_from_plan_with_model_iterations_and_layer2(
    plan: &Plan,
    root: &Path,
    gateway_bin: &Path,
    model: Option<&str>,
    max_iterations: usize,
    skip_layer2: bool,
    on_event: &(dyn Fn(BuildEvent) + Send + Sync),
) -> anyhow::Result<BuildOutcome> {
    build_from_plan_with_model_iterations_layer2_and_activity(
        plan,
        root,
        gateway_bin,
        model,
        max_iterations,
        skip_layer2,
        on_event,
        None,
    )
    .await
}

/// Like [`build_from_plan_with_model_iterations_and_layer2`], but accepts an
/// optional `on_activity` heartbeat callback that is wired into every agent
/// driver via [`ClaudeCliDriver::with_on_activity`]. The callback fires on every
/// stdout line emitted by the agent subprocess, keeping `last_activity_ms` fresh
/// while an agent is actively producing output.
///
/// Pass `None` for identical behaviour to the non-activity variant.
pub async fn build_from_plan_with_model_iterations_layer2_and_activity(
    plan: &Plan,
    root: &Path,
    gateway_bin: &Path,
    model: Option<&str>,
    max_iterations: usize,
    skip_layer2: bool,
    on_event: &(dyn Fn(BuildEvent) + Send + Sync),
    on_activity: Option<HeartbeatFn>,
) -> anyhow::Result<BuildOutcome> {
    let crate_name = "camerata_app";

    // ── Scaffold the shared worktree ─────────────────────────────────────────
    on_event(BuildEvent::Scaffolding);
    let worktree = root.join("crate");
    let _ = std::fs::remove_dir_all(root);
    scaffold_crate(&worktree, crate_name)?;
    let lib_path = worktree.join("src").join("lib.rs");
    let lib_path_display = lib_path.display().to_string();

    let total = plan.tasks.len();

    // ── Build a governed role per plan task ──────────────────────────────────
    let mut roles: Vec<Role> = Vec::with_capacity(total);
    for (i, task) in plan.tasks.iter().enumerate() {
        let role_name = format!("{}-{}", task.role, i + 1);
        let role = governed_role(&role_name).await?;
        roles.push(role);
    }

    // ── Per-session governed drivers (each agent its own session) ────────────
    // prepare_session creates its own TempDir per session (ARCH-RESOURCE-LIFECYCLE-1).
    let mut spawns = Vec::with_capacity(total);
    for role in roles.iter() {
        // Pass the worktree so the gateway is jailed to it (CAMERATA_WORKTREE_ROOT):
        // gated_write refuses any target outside the worktree, in code. No extra read dirs:
        // greenfield fleet builds scaffold into this throwaway worktree and have no other
        // project-repo clones to read across (the multi-repo read scope is for in-project
        // brownfield agents, threaded from the server's AppState::active_repo_dirs()).
        let spawn = prepare_session(gateway_bin, role, Some(&worktree), &[])?;
        spawns.push(spawn);
    }
    let drivers: Vec<_> = spawns
        .iter()
        .map(|spawn| {
            let d = spawn.driver.clone().with_worktree(&worktree);
            // Thread the operator's model choice into every agent. `with_model("")`
            // is a no-op (the driver ignores blank ids), so passing None here via
            // unwrap_or("") is safe.
            let d = match model {
                Some(m) => d.with_model(m),
                None => d,
            };
            // Wire the activity heartbeat so streamed agent output keeps
            // last_activity_ms fresh. `with_on_activity` is a no-op when None.
            match &on_activity {
                Some(cb) => d.with_on_activity(cb.clone()),
                None => d,
            }
        })
        .collect();

    // ── Build the stage list ─────────────────────────────────────────────────
    // Single-model path: every agent runs on the one operator-wide `model` (or the
    // CLI default when `None`); no agent is the lead/orchestrator (no delegation).
    let model_label = model.unwrap_or("default (CLI)").to_string();
    let mut stages: Vec<FleetStage> = Vec::with_capacity(total);
    for (i, task) in plan.tasks.iter().enumerate() {
        on_event(BuildEvent::StageStarted {
            index: i,
            total,
            role: roles[i].name.clone(),
            kind: task.kind.label().to_string(),
        });
        on_event(BuildEvent::AgentTier {
            index: i,
            role: roles[i].name.clone(),
            model: model_label.clone(),
            is_lead: false,
        });
        let stage_task = stage_task_for(task, &lib_path_display, i == 0);
        stages.push(FleetStage::new(roles[i].clone(), stage_task, &drivers[i]));
    }

    // ── Run the governed fleet with the language-matched layer-2 runner ──────
    // `layer2_runner_with_activity` returns a language-matched CheckRunner with the
    // heartbeat baked in for Rust sub-runners (Cargo.toml -> RustCheckRunner::with_heartbeat),
    // or a NoopChecks for an explicit `skip_layer2` bootstrap run. When `on_activity` is
    // None the behaviour degrades to `layer2_runner` (unchanged).
    let checks = layer2_runner_with_activity(&worktree, skip_layer2, on_activity.as_ref());
    let fleet = FleetCoordinator::new(&*checks, &worktree);
    let report = fleet.run_with_iterations(&stages, max_iterations).await?;

    // ── Emit per-stage layer-2 / revise / finished events ────────────────────
    let all_agents_ran = emit_stage_reports(&report, total, on_event);

    // ── Check what the gate actually wrote ───────────────────────────────────
    let produced = std::fs::read_to_string(&lib_path).unwrap_or_default();
    let wrote_through_gate =
        lib_path.exists() && !produced.trim_start().starts_with("// placeholder");

    // ── cargo build + cargo test ──────────────────────────────────────────────
    on_event(BuildEvent::Verifying);
    let build = run_cargo(&worktree, "build").await?;
    let compiled = build.success;

    let test = if compiled {
        Some(run_cargo(&worktree, "test").await?)
    } else {
        None
    };
    let tests_passed = test.as_ref().map(|t| t.success).unwrap_or(false);

    on_event(BuildEvent::Done {
        compiled,
        tests_passed,
    });

    Ok(BuildOutcome {
        compiled,
        tests_passed,
        all_agents_ran,
        wrote_through_gate,
        total_bounces: report.total_bounces(),
        fleet_clean: report.is_clean(),
        produced_path: lib_path,
        produced_bytes: produced.len(),
    })
}

/// Emit the per-stage layer-2 result, bounce-and-revise, and finished events from a
/// completed [`camerata_core::FleetReport`], and report whether every stage ran an
/// agent. Shared by the single-model and tiered build paths so they surface identical
/// observability. PURE w.r.t. the gate: derived entirely from the already-decided
/// report — it records what the check/coordinator decided, changing nothing.
fn emit_stage_reports(
    report: &camerata_core::FleetReport,
    total: usize,
    on_event: &(dyn Fn(BuildEvent) + Send + Sync),
) -> bool {
    let mut all_agents_ran = true;
    for (i, stage) in report.stages.iter().enumerate() {
        let r = &stage.report;
        if r.initial_outcome.session_id.is_empty() {
            all_agents_ran = false;
        }

        // A bounce-and-revise pass ran iff the stage bounced; cite the rules that
        // were sent back (the initial violations the revise pass was asked to fix).
        if r.bounced {
            on_event(BuildEvent::ReviseIteration {
                index: i,
                violated_rules: r.initial_violations.iter().map(|x| x.0.clone()).collect(),
            });
        }

        // The layer-2 result: clean == no residual violations after all passes.
        let passed = r.final_violations.is_empty();
        on_event(BuildEvent::Layer2Result {
            index: i,
            total,
            passed,
            violated_rules: r.final_violations.iter().map(|x| x.0.clone()).collect(),
        });

        on_event(BuildEvent::StageFinished {
            index: i,
            total,
            clean: passed,
            bounced: r.bounced,
            session_id: r.initial_outcome.session_id.clone(),
        });
    }
    all_agents_ran
}

// ─── build_from_plan_with_tier_map ───────────────────────────────────────────

/// Run the governed fleet with PER-STAGE model resolution driven by `tier_map`
/// (ORCH-MODEL-TIERING-1).
///
/// Each [`PlanTask`] is classified by [`tier::classify_task`] into a
/// [`tier::CapabilityBand`]; the band is looked up in `tier_map` to get the
/// concrete model id; that id is threaded into the stage's driver via
/// `with_model(id)`. All stages get an individually-appropriate model rather
/// than a single operator-wide choice.
///
/// The `max_iterations` loop-guard ceiling is passed through unchanged (same
/// semantics as [`build_from_plan_with_model_and_iterations`]).
///
/// This function is ADDITIVE: the existing single-model entry points
/// ([`build_from_plan`], [`build_from_plan_with_model`]) continue to work
/// exactly as before. Callers that do not supply a [`tier::TierMap`] are
/// unaffected.
pub async fn build_from_plan_with_tier_map(
    plan: &Plan,
    root: &Path,
    gateway_bin: &Path,
    tier_map: &tier::TierMap,
    max_iterations: usize,
    on_event: &(dyn Fn(BuildEvent) + Send + Sync),
) -> anyhow::Result<BuildOutcome> {
    build_from_plan_with_tier_map_and_layer2(
        plan,
        root,
        gateway_bin,
        tier_map,
        max_iterations,
        false,
        on_event,
    )
    .await
}

/// Like [`build_from_plan_with_tier_map`], but with an explicit `skip_layer2`
/// bootstrap flag (same semantics as
/// [`build_from_plan_with_model_iterations_and_layer2`]): `false` keeps the real,
/// language-matched layer-2 runner; `true` runs this ONE tiered run with a no-op
/// layer-2 runner so the tool-installing bootstrap run can land. Skips ONLY layer 2;
/// layer 1 is unchanged.
pub async fn build_from_plan_with_tier_map_and_layer2(
    plan: &Plan,
    root: &Path,
    gateway_bin: &Path,
    tier_map: &tier::TierMap,
    max_iterations: usize,
    skip_layer2: bool,
    on_event: &(dyn Fn(BuildEvent) + Send + Sync),
) -> anyhow::Result<BuildOutcome> {
    build_from_plan_with_tier_map_layer2_and_activity(
        plan,
        root,
        gateway_bin,
        tier_map,
        max_iterations,
        skip_layer2,
        on_event,
        None,
        // Back-compat wrapper: vision band off. The live server path calls the deepest
        // function directly and passes the project's real vision_enabled flag.
        false,
        // Back-compat wrapper: no orchestrator-driver factory => the lead runs on the
        // exact current `ClaudeCliDriver` orchestrator path.
        None,
    )
    .await
}

/// Like [`build_from_plan_with_tier_map_and_layer2`], but wires an optional
/// `on_activity` heartbeat into every agent driver so streamed output keeps
/// `last_activity_ms` fresh for the parent tracked run. Pass `None` for
/// identical behaviour to the non-activity variant.
///
/// `orch_factory` is the provider-agnostic LEAD/orchestrator seam (mirrors the
/// child-driver-factory): when `Some`, the LEAD stage's driver is built via
/// [`orchestrator::OrchestratorDriverFactory::build_lead`], so the lead runs on the
/// strongest model's OWN provider (Claude CLI today; native `ApiAgentDriver` when the
/// strongest tier is an OpenRouter model). When `None` (BACK-COMPAT), the lead uses the
/// exact current `ClaudeCliDriver` orchestrator path — every existing caller/test is
/// unchanged. Non-lead stages ALWAYS use the `ClaudeCliDriver` builder either way; only
/// the lead is ever routed through the factory, preserving the orchestrator-only gate.
#[allow(clippy::too_many_arguments)]
pub async fn build_from_plan_with_tier_map_layer2_and_activity(
    plan: &Plan,
    root: &Path,
    gateway_bin: &Path,
    tier_map: &tier::TierMap,
    max_iterations: usize,
    skip_layer2: bool,
    on_event: &(dyn Fn(BuildEvent) + Send + Sync),
    on_activity: Option<HeartbeatFn>,
    vision_enabled: bool,
    orch_factory: Option<orchestrator::SharedOrchestratorDriverFactory>,
) -> anyhow::Result<BuildOutcome> {
    let crate_name = "camerata_app";

    // ── Scaffold the shared worktree ─────────────────────────────────────────
    on_event(BuildEvent::Scaffolding);
    let worktree = root.join("crate");
    let _ = std::fs::remove_dir_all(root);
    scaffold_crate(&worktree, crate_name)?;
    let lib_path = worktree.join("src").join("lib.rs");
    let lib_path_display = lib_path.display().to_string();

    let total = plan.tasks.len();

    // ── Build a governed role per plan task ──────────────────────────────────
    let mut roles: Vec<Role> = Vec::with_capacity(total);
    for (i, task) in plan.tasks.iter().enumerate() {
        let role_name = format!("{}-{}", task.role, i + 1);
        let role = governed_role(&role_name).await?;
        roles.push(role);
    }

    // ── Identify the LEAD/orchestrator stage (the first strongest task) ──────
    // Only this stage gets the governed `delegate` tool (orchestrator mode). All
    // other stages spawn normally — no delegate env, no delegate in --allowedTools
    // — which is the depth-1 guarantee. `None` => no delegation this run.
    let lead_idx = orchestrator::lead_stage_index(&plan.tasks);

    // ── Per-session governed drivers ─────────────────────────────────────────
    // The lead's session carries an orchestrator mcp-config (delegate ON, tier
    // map + gateway bin + worktree + depth=0); every other session is ordinary.
    // prepare_session / prepare_orchestrator_session each create their own TempDir
    // (ARCH-RESOURCE-LIFECYCLE-1); the _dir fields keep the session alive for the run.
    let mut mcp_config_paths: Vec<String> = Vec::with_capacity(total);
    let mut is_orchestrator: Vec<bool> = Vec::with_capacity(total);
    // Hold all session spawns so their _dir TempDirs remain alive for the full run.
    let mut tiered_spawns: Vec<camerata_agent::SessionSpawn> = Vec::with_capacity(total);
    let mut orch_spawns: Vec<orchestrator::OrchestratorSession> = Vec::with_capacity(total);
    for (i, role) in roles.iter().enumerate() {
        if Some(i) == lead_idx {
            let orch = orchestrator::prepare_orchestrator_session(
                gateway_bin,
                role,
                &worktree,
                tier_map,
                vision_enabled,
            )?;
            mcp_config_paths.push(orch.mcp_config.display().to_string());
            is_orchestrator.push(true);
            orch_spawns.push(orch);
        } else {
            // No extra read dirs: greenfield fleet builds scaffold into this throwaway
            // worktree (multi-repo read scope is for in-project brownfield agents).
            let spawn = prepare_session(gateway_bin, role, Some(&worktree), &[])?;
            mcp_config_paths.push(spawn.mcp_config.display().to_string());
            is_orchestrator.push(false);
            tiered_spawns.push(spawn);
        }
    }

    // Resolve the per-stage model id from the tier map BEFORE moving into the
    // closure below (so we have all ids as owned Strings ahead of the borrow).
    let per_stage_models: Vec<String> = plan
        .tasks
        .iter()
        .map(|task| tier_map.model_for_task(task).to_string())
        .collect();

    // Heterogeneous drivers: non-lead stages are always the `ClaudeCliDriver` (boxed,
    // identical behavior incl. the activity heartbeat); the LEAD is built via the injected
    // `orch_factory` when present (provider follows the strongest model), else the same
    // CLI orchestrator driver as before. `FleetStage::new` coerces `&dyn AgentDriver`, so a
    // boxed mix is fine.
    let build_cli_driver = |i: usize| -> Box<dyn AgentDriver> {
        let d = camerata_agent::ClaudeCliDriver::new(mcp_config_paths[i].clone())
            .with_worktree(&worktree)
            .with_model(&per_stage_models[i])
            // Only the lead gets the delegate tool in --allowedTools.
            .as_orchestrator(is_orchestrator[i]);
        // Wire the activity heartbeat so streamed agent output keeps
        // last_activity_ms fresh for the parent tracked run.
        let d = match &on_activity {
            Some(cb) => d.with_on_activity(cb.clone()),
            None => d,
        };
        Box::new(d)
    };

    let drivers: Vec<Box<dyn AgentDriver>> = (0..total)
        .map(|i| -> anyhow::Result<Box<dyn AgentDriver>> {
            // The LEAD is routed through the injected factory when present; every non-lead
            // stage (and the lead when no factory is given) uses the CLI driver. Only the
            // lead can ever reach the factory, so non-lead stages can NEVER carry
            // delegate/fan_out — the orchestrator-only gate is preserved by construction.
            if Some(i) == lead_idx {
                if let Some(factory) = orch_factory.as_ref() {
                    // The lead's orchestrator session is the sole entry in `orch_spawns`.
                    // Build the lead on the strongest model's OWN provider, in orchestrator
                    // mode + gated. The factory upholds the gate.
                    let session = orch_spawns
                        .first()
                        .expect("a lead stage prepared exactly one orchestrator session");
                    let ctx = orchestrator::LeadBuildContext {
                        strongest_model: &per_stage_models[i],
                        session,
                        worktree: &worktree,
                        tier_map,
                        vision_enabled,
                        on_activity: on_activity.clone(),
                    };
                    return factory.build_lead(&ctx);
                }
                // No factory => exact current CLI orchestrator behavior (back-compat).
            }
            Ok(build_cli_driver(i))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    // ── Build the stage list ─────────────────────────────────────────────────
    let mut stages: Vec<FleetStage> = Vec::with_capacity(total);
    for (i, task) in plan.tasks.iter().enumerate() {
        on_event(BuildEvent::StageStarted {
            index: i,
            total,
            role: roles[i].name.clone(),
            kind: task.kind.label().to_string(),
        });
        // Surface the tier/model routing for this agent: the per-stage model resolved
        // from the tier map, and whether it is the lead/orchestrator (delegate-capable).
        on_event(BuildEvent::AgentTier {
            index: i,
            role: roles[i].name.clone(),
            model: per_stage_models[i].clone(),
            is_lead: Some(i) == lead_idx,
        });
        let mut stage_task = stage_task_for(task, &lib_path_display, i == 0);
        // Tell the lead it can delegate (and how escalation works).
        if Some(i) == lead_idx {
            stage_task.push_str(&orchestrator::orchestrator_prompt_suffix(vision_enabled));
        }
        stages.push(FleetStage::new(roles[i].clone(), stage_task, drivers[i].as_ref()));
    }

    // ── Run the governed fleet with the language-matched layer-2 runner ──────
    // `layer2_runner_with_activity` returns a language-matched CheckRunner with the
    // heartbeat baked in for Rust sub-runners so cargo output fires heartbeats during
    // the layer-2 gate, or a NoopChecks for an explicit `skip_layer2` bootstrap run.
    let checks = layer2_runner_with_activity(&worktree, skip_layer2, on_activity.as_ref());
    let fleet = FleetCoordinator::new(&*checks, &worktree);
    let report = fleet.run_with_iterations(&stages, max_iterations).await?;

    // ── Emit per-stage layer-2 / revise / finished events ────────────────────
    let all_agents_ran = emit_stage_reports(&report, total, on_event);

    // ── Check what the gate actually wrote ───────────────────────────────────
    let produced = std::fs::read_to_string(&lib_path).unwrap_or_default();
    let wrote_through_gate =
        lib_path.exists() && !produced.trim_start().starts_with("// placeholder");

    // ── cargo build + cargo test ──────────────────────────────────────────────
    on_event(BuildEvent::Verifying);
    let build = run_cargo(&worktree, "build").await?;
    let compiled = build.success;

    let test = if compiled {
        Some(run_cargo(&worktree, "test").await?)
    } else {
        None
    };
    let tests_passed = test.as_ref().map(|t| t.success).unwrap_or(false);

    on_event(BuildEvent::Done {
        compiled,
        tests_passed,
    });

    Ok(BuildOutcome {
        compiled,
        tests_passed,
        all_agents_ran,
        wrote_through_gate,
        total_bounces: report.total_bounces(),
        fleet_clean: report.is_clean(),
        produced_path: lib_path,
        produced_bytes: produced.len(),
    })
}

// ─── tests (ORCH-NEW-PATH-TESTS-1) ───────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_agent::{HeartbeatFn, GATED_WRITE_TOOL};
    use camerata_core::{Role, RuleId};
    use camerata_intake::PlanTask;

    // ── scaffold_crate ────────────────────────────────────────────────────────

    #[test]
    fn scaffold_crate_writes_valid_cargo_toml_and_placeholder_lib() {
        let dir = std::env::temp_dir().join(format!(
            "camerata-fleet-test-scaffold-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        scaffold_crate(&dir, "my_test_crate").unwrap();

        let toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert!(toml.contains("name = \"my_test_crate\""));
        assert!(toml.contains("edition = \"2021\""));
        assert!(toml.contains("[dependencies]"));

        let lib = std::fs::read_to_string(dir.join("src").join("lib.rs")).unwrap();
        assert!(lib.contains("placeholder"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── layer2_runner (bootstrap layer-2 bypass) ─────────────────────────────

    /// `skip_layer2 = true` selects the no-op layer-2 runner so a bootstrap run can land
    /// the tooling; `false` selects the real, language-matched (fail-closed) runner.
    ///
    /// Asserted behaviorally on a JS worktree whose `package.json` declares NO lint/test
    /// script: the real runner is fail-closed there (returns `Err`, "could-not-run" — the
    /// exact deadlock the bypass exists to break), while the no-op runner returns
    /// `Ok(empty)`. This is token- and network-free: the JS runner bails before any
    /// install step. Confirms the bypass skips layer 2 (and only layer 2).
    #[tokio::test]
    async fn layer2_runner_skips_when_bootstrap_and_runs_real_otherwise() {
        let dir = std::env::temp_dir().join(format!(
            "camerata-fleet-test-layer2-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A JS manifest with no lint/test → the real runner fails closed.
        std::fs::write(dir.join("package.json"), "{ \"name\": \"x\" }").unwrap();

        let role = Role {
            name: "x".into(),
            rule_subset: vec![],
            allowed_paths: vec![],
        };

        // skip_layer2 = false, no heartbeat → real (JS) runner → fail-closed Err (the deadlock).
        let real = layer2_runner_with_activity(&dir, false, None);
        let real_res = real.check(&role, &dir).await;
        assert!(
            real_res.is_err(),
            "the real layer-2 runner must fail closed on a manifest with no lint/test wired"
        );

        // skip_layer2 = true → no-op runner → Ok(empty), so the bootstrap run can proceed.
        let noop = layer2_runner_with_activity(&dir, true, None);
        let noop_res = noop.check(&role, &dir).await;
        assert_eq!(
            noop_res.expect("the bootstrap no-op runner must not error").violated,
            Vec::<RuleId>::new(),
            "the bootstrap no-op runner reports no violations (skips layer 2)"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── layer2_runner_with_activity forwards callback to Rust sub-runners ─────

    /// `layer2_runner_with_activity` with `Some(cb)` and a Rust worktree must
    /// produce a `CombinedCheckRunner` whose language sub-runner is a
    /// `PolyglotCheckRunner` over a `RustCheckRunner::with_heartbeat(cb)`.
    ///
    /// We verify this BEHAVIOURALLY: run `cargo fmt --check` on a cleanly-formatted
    /// minimal crate via the returned runner and assert the heartbeat fires at least
    /// once during the subprocess. This proves the callback threaded all the way
    /// from `layer2_runner_with_activity` → `runner_for_worktree_with_heartbeat` →
    /// `PolyglotCheckRunner::from_detected_with_heartbeat` →
    /// `RustCheckRunner::with_heartbeat` → `subprocess::run_fmt_check(..., Some(cb))`.
    ///
    /// Token- and network-free: all subprocess calls run real cargo against a tiny
    /// scaffolded crate, same as the existing `fmt_real_subprocess` integration test.
    #[tokio::test]
    async fn layer2_runner_with_activity_forwards_heartbeat_to_rust_runner() {
        use std::sync::{Arc, atomic::{AtomicU64, Ordering}};

        let dir = std::env::temp_dir().join(format!(
            "camerata-fleet-test-heartbeat-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();

        // A minimal Rust crate scaffolded in canonical format so fmt passes (and
        // clippy/test won't be reached — fmt passes and exits 0 early enough that
        // the runner is satisfied with a clean result).  We only need fmt to run
        // and emit at least one stdout line so the heartbeat fires.
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"heartbeat_test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ).unwrap();
        std::fs::write(dir.join("src").join("lib.rs"), "// empty\n").unwrap();

        let tick_count = Arc::new(AtomicU64::new(0));
        let tick_count_cb = tick_count.clone();
        let cb: HeartbeatFn = Arc::new(move || {
            tick_count_cb.fetch_add(1, Ordering::Relaxed);
        });

        let runner = layer2_runner_with_activity(&dir, false, Some(&cb));

        let role = Role {
            name: "test".into(),
            rule_subset: vec![],
            allowed_paths: vec![],
        };

        // Run the layer-2 gate. Cargo fmt on a canonical crate returns Ok([]).
        let result = runner.check(&role, &dir).await;

        // Heartbeat must have fired at least once during cargo fmt stdout output.
        let ticks = tick_count.load(Ordering::Relaxed);
        assert!(
            ticks >= 1,
            "layer2_runner_with_activity must fire the heartbeat at least once during \
             cargo fmt; got {ticks} ticks — callback was not forwarded to the Rust runner"
        );

        // And the crate must be clean (no violations on a minimal canonical crate).
        let violations = result.expect("layer2_runner_with_activity must not error on a valid Rust crate").violated;
        assert!(
            violations.is_empty(),
            "a canonical Rust crate must produce no layer-2 violations, got {violations:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── emit_stage_reports (BuildEvent mapping from a finished report) ────────

    /// `emit_stage_reports` derives, per stage, a `ReviseIteration` iff the stage
    /// bounced, a `Layer2Result` (passed/violated rules), and a `StageFinished`. This
    /// is the observability mapping; it reads an already-decided report and changes
    /// nothing about the gate. Built with synthetic reports — no I/O, no tokens.
    #[test]
    fn emit_stage_reports_maps_layer2_revise_and_finished() {
        use camerata_core::{AgentOutcome, FleetReport, RunReport, RuleId, StageReport};
        use std::sync::Mutex;

        fn outcome(session: &str) -> AgentOutcome {
            AgentOutcome {
                session_id: session.to_string(),
                result: "ok".to_string(),
                cost_usd: Some(0.0),
                denials: vec![],
            }
        }

        // Stage 0: clean on first pass (no bounce). Stage 1: bounced, resolved clean.
        // Stage 2: bounced, still dirty (residual RUST-CLIPPY).
        let report = FleetReport {
            stages: vec![
                StageReport {
                    role_name: "Implementer-1".to_string(),
                    report: RunReport {
                        initial_outcome: outcome("s0"),
                        initial_violations: vec![],
                        revised_outcome: None,
                        final_violations: vec![],
                        bounced: false,
                    },
                },
                StageReport {
                    role_name: "Implementer-2".to_string(),
                    report: RunReport {
                        initial_outcome: outcome("s1"),
                        initial_violations: vec![RuleId("RUST-FMT".to_string())],
                        revised_outcome: Some(outcome("s1")),
                        final_violations: vec![],
                        bounced: true,
                    },
                },
                StageReport {
                    role_name: "Implementer-3".to_string(),
                    report: RunReport {
                        initial_outcome: outcome(""), // empty session => agent did not run
                        initial_violations: vec![RuleId("RUST-CLIPPY".to_string())],
                        revised_outcome: Some(outcome("")),
                        final_violations: vec![RuleId("RUST-CLIPPY".to_string())],
                        bounced: true,
                    },
                },
            ],
        };

        let events: Mutex<Vec<BuildEvent>> = Mutex::new(vec![]);
        let all_ran = emit_stage_reports(&report, 3, &|e| events.lock().unwrap().push(e));

        // Stage 2's empty session id means not every agent ran.
        assert!(!all_ran);

        let events = events.into_inner().unwrap();

        // Stage 0 (clean): no ReviseIteration, a passed Layer2Result, a clean StageFinished.
        assert!(!events.iter().any(|e| matches!(
            e,
            BuildEvent::ReviseIteration { index: 0, .. }
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            BuildEvent::Layer2Result { index: 0, passed: true, .. }
        )));

        // Stage 1 (bounced, resolved): a ReviseIteration citing RUST-FMT, then passed.
        assert!(events.iter().any(|e| matches!(
            e,
            BuildEvent::ReviseIteration { index: 1, violated_rules } if violated_rules == &vec!["RUST-FMT".to_string()]
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            BuildEvent::Layer2Result { index: 1, passed: true, .. }
        )));

        // Stage 2 (bounced, residual): a ReviseIteration, then a FAILED Layer2Result
        // carrying the residual rule id, and a non-clean StageFinished.
        assert!(events.iter().any(|e| matches!(
            e,
            BuildEvent::Layer2Result { index: 2, passed: false, violated_rules, .. } if violated_rules == &vec!["RUST-CLIPPY".to_string()]
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            BuildEvent::StageFinished { index: 2, clean: false, bounced: true, .. }
        )));
    }

    // ── stage_task_for ────────────────────────────────────────────────────────

    #[test]
    fn first_stage_task_says_overwrite_and_names_the_tool() {
        let task = PlanTask {
            role: "Implementer".to_string(),
            kind: TaskKind::Backend,
            description: "build the Expense struct".to_string(),
        };
        let s = stage_task_for(&task, "/tmp/x/src/lib.rs", true);
        assert!(s.contains(GATED_WRITE_TOOL));
        assert!(s.contains("OVERWRITE"));
        assert!(s.contains("Expense"));
        assert!(s.contains("/tmp/x/src/lib.rs"));
    }

    #[test]
    fn later_stage_task_says_read_then_preserve() {
        let task = PlanTask {
            role: "Tester".to_string(),
            kind: TaskKind::Test,
            description: "add tests".to_string(),
        };
        let s = stage_task_for(&task, "/tmp/x/src/lib.rs", false);
        assert!(s.contains("FIRST read"));
        assert!(s.contains("PRESERVING"));
    }

    // ── tail_lines ────────────────────────────────────────────────────────────

    #[test]
    fn tail_lines_returns_last_n_lines() {
        let s = "a\nb\nc\nd\ne";
        let got = tail_lines(s, 3);
        assert_eq!(got, vec!["c", "d", "e"]);
    }

    #[test]
    fn tail_lines_with_n_larger_than_total_returns_all() {
        let s = "x\ny";
        let got = tail_lines(s, 10);
        assert_eq!(got, vec!["x", "y"]);
    }

    #[test]
    fn tail_lines_empty_string_returns_empty() {
        let got = tail_lines("", 5);
        assert!(got.is_empty());
    }

    // ── BuildEvent / BuildOutcome smoke tests ─────────────────────────────────

    #[test]
    fn build_event_is_clone_and_debug() {
        let e = BuildEvent::StageStarted {
            index: 0,
            total: 2,
            role: "Implementer".to_string(),
            kind: "Backend".to_string(),
        };
        let cloned = e.clone();
        let _ = format!("{cloned:?}");

        let e2 = BuildEvent::Done {
            compiled: true,
            tests_passed: false,
        };
        let _ = format!("{:?}", e2.clone());

        let e3 = BuildEvent::StageFinished {
            index: 1,
            total: 2,
            clean: true,
            bounced: false,
            session_id: "abc-123".to_string(),
        };
        let _ = format!("{:?}", e3.clone());

        let e4 = BuildEvent::Scaffolding;
        let _ = format!("{:?}", e4.clone());

        let e5 = BuildEvent::Verifying;
        let _ = format!("{:?}", e5.clone());

        let e6 = BuildEvent::AgentTier {
            index: 0,
            role: "Lead-1".to_string(),
            model: "claude-opus-4-8".to_string(),
            is_lead: true,
        };
        let _ = format!("{:?}", e6.clone());

        let e7 = BuildEvent::Layer2Result {
            index: 1,
            total: 2,
            passed: false,
            violated_rules: vec!["RUST-FMT".to_string()],
        };
        let _ = format!("{:?}", e7.clone());

        let e8 = BuildEvent::ReviseIteration {
            index: 1,
            violated_rules: vec!["RUST-FMT".to_string()],
        };
        let _ = format!("{:?}", e8.clone());
    }

    #[test]
    fn build_outcome_is_clone_and_debug() {
        let o = BuildOutcome {
            compiled: true,
            tests_passed: true,
            all_agents_ran: true,
            wrote_through_gate: true,
            total_bounces: 0,
            fleet_clean: true,
            produced_path: PathBuf::from("/tmp/foo/src/lib.rs"),
            produced_bytes: 42,
        };
        let cloned = o.clone();
        assert_eq!(cloned.produced_bytes, 42);
        assert!(cloned.compiled);
        let _ = format!("{cloned:?}");
    }

    // ── build_from_plan_with_model ────────────────────────────────────────────

    /// Verify that `build_from_plan` is a thin wrapper around
    /// `build_from_plan_with_model` — both exist as public APIs with compatible
    /// signatures (both accept the same plan/root/gateway_bin/on_event args;
    /// `_with_model` additionally accepts `Option<&str>`). This is a
    /// compile-time / API-shape test: if the wrappers diverge, this will fail
    /// to compile.
    #[test]
    fn build_from_plan_with_model_accepts_none_and_some() {
        // Verify both signatures are callable (we don't run them — they need a
        // live gateway + corpus). Just confirm the types are correct.
        fn _check_none_compiles(
            plan: &camerata_intake::Plan,
            root: &std::path::Path,
            bin: &std::path::Path,
        ) {
            // Both should accept the same event type.
            let _: std::pin::Pin<Box<dyn std::future::Future<Output = _>>> =
                Box::pin(build_from_plan_with_model(plan, root, bin, None, &|_| {}));
        }
        fn _check_some_compiles(
            plan: &camerata_intake::Plan,
            root: &std::path::Path,
            bin: &std::path::Path,
        ) {
            let _: std::pin::Pin<Box<dyn std::future::Future<Output = _>>> = Box::pin(
                build_from_plan_with_model(plan, root, bin, Some("claude-sonnet-4-6"), &|_| {}),
            );
        }
        // This test proves the API compiles with both None and Some; the _ suffix
        // functions are never called, so no I/O or infra is required.
        let _ = _check_none_compiles as fn(_, _, _);
        let _ = _check_some_compiles as fn(_, _, _);
    }

    // ── build_from_plan_with_tier_map (ORCH-MODEL-TIERING-1) ─────────────────

    /// Compile-time / API-shape test: `build_from_plan_with_tier_map` accepts the
    /// expected types. We don't run it (needs a live gateway + corpus); we just prove
    /// the signature is callable with a default `TierMap`.
    #[test]
    fn build_from_plan_with_tier_map_signature_compiles() {
        fn _check_signature(
            plan: &camerata_intake::Plan,
            root: &std::path::Path,
            bin: &std::path::Path,
        ) {
            let tier_map = crate::tier::TierMap::default();
            let _: std::pin::Pin<Box<dyn std::future::Future<Output = _>>> =
                Box::pin(build_from_plan_with_tier_map(
                    plan,
                    root,
                    bin,
                    &tier_map,
                    1,
                    &|_| {},
                ));
        }
        let _ = _check_signature as fn(_, _, _);
    }

    /// Verify that the tier map resolves per-task models correctly for a
    /// representative plan (Backend -> Opus, Test -> Haiku, Database -> Sonnet,
    /// Frontend -> Sonnet). This is a pure-logic test — no I/O needed.
    #[test]
    fn tier_map_resolves_correct_models_for_mixed_plan() {
        use crate::tier::{CapabilityBand, TierMap, classify_task};
        use camerata_intake::{Plan, PlanTask, TaskKind};

        let tier_map = TierMap::default();
        let plan = Plan {
            app_name: "budget".to_string(),
            summary: "budget app".to_string(),
            tasks: vec![
                PlanTask {
                    role: "Implementer".to_string(),
                    kind: TaskKind::Database,
                    description: "schema".to_string(),
                },
                PlanTask {
                    role: "Implementer".to_string(),
                    kind: TaskKind::Backend,
                    description: "domain types".to_string(),
                },
                PlanTask {
                    role: "Implementer".to_string(),
                    kind: TaskKind::Frontend,
                    description: "list view".to_string(),
                },
                PlanTask {
                    role: "Tester".to_string(),
                    kind: TaskKind::Test,
                    description: "unit tests".to_string(),
                },
            ],
        };

        let expected = [
            (CapabilityBand::Balanced, "claude-sonnet-4-6"),
            (CapabilityBand::Strongest, "claude-opus-4-8"),
            (CapabilityBand::Balanced, "claude-sonnet-4-6"),
            (CapabilityBand::Fast, "claude-haiku-4-5-20251001"),
        ];

        for (task, (expected_band, expected_model)) in plan.tasks.iter().zip(expected.iter()) {
            let band = classify_task(task);
            assert_eq!(
                band, *expected_band,
                "task '{}' ({:?}) wrong band",
                task.description, task.kind
            );
            let model = tier_map.model_for_task(task);
            assert_eq!(
                model, *expected_model,
                "task '{}' ({:?}) wrong model",
                task.description, task.kind
            );
        }
    }

    // ── OrchestratorDriverFactory seam (provider-agnostic LEAD) ───────────────

    use crate::orchestrator::{
        LeadBuildContext, OrchestratorDriverFactory, SharedOrchestratorDriverFactory,
    };
    use camerata_core::{AgentDriver, AgentOutcome};
    use std::sync::{Arc, Mutex};

    /// A no-op driver standing in for whatever the factory would build. Records nothing;
    /// `run` returns immediately so it never spawns a real subprocess.
    struct StubLeadDriver;
    #[async_trait::async_trait]
    impl AgentDriver for StubLeadDriver {
        async fn run(&self, _role: &Role, _task: &str) -> anyhow::Result<AgentOutcome> {
            Ok(AgentOutcome {
                session_id: "stub".into(),
                result: "stub".into(),
                cost_usd: None,
                denials: vec![],
            })
        }
    }

    /// A recording `OrchestratorDriverFactory` double: captures the model + worktree it was
    /// asked to build the LEAD for, and how many times `build_lead` ran.
    struct RecordingOrchestratorFactory {
        calls: Mutex<Vec<(String, std::path::PathBuf, bool)>>,
    }
    impl RecordingOrchestratorFactory {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
            }
        }
    }
    impl OrchestratorDriverFactory for RecordingOrchestratorFactory {
        fn build_lead(&self, ctx: &LeadBuildContext<'_>) -> anyhow::Result<Box<dyn AgentDriver>> {
            self.calls.lock().unwrap().push((
                ctx.strongest_model.to_string(),
                ctx.worktree.to_path_buf(),
                ctx.on_activity.is_some(),
            ));
            Ok(Box::new(StubLeadDriver))
        }
    }

    /// The factory's `build_lead` receives the STRONGEST model id and the shared worktree,
    /// and returns an orchestrator driver the fleet will box and use for the lead stage.
    /// (Pure-logic: drives the factory through a synthesized `LeadBuildContext`, exactly the
    /// shape the build loop constructs — no gateway/cargo/`claude` needed.)
    #[test]
    fn orchestrator_factory_build_lead_receives_strongest_model() {
        let factory = RecordingOrchestratorFactory::new();
        let role = Role {
            name: "Lead".into(),
            rule_subset: vec![RuleId("GOV-1".into())],
            allowed_paths: vec!["crate/".into()],
        };
        let session = orchestrator::prepare_orchestrator_session(
            std::path::Path::new("/bin/camerata-gateway"),
            &role,
            std::path::Path::new("/work/crate"),
            &crate::tier::TierMap::default(),
            false,
        )
        .unwrap();
        let cb: HeartbeatFn = Arc::new(|| {});
        let ctx = LeadBuildContext {
            strongest_model: "claude-opus-4-8",
            session: &session,
            worktree: std::path::Path::new("/work/crate"),
            tier_map: &crate::tier::TierMap::default(),
            vision_enabled: false,
            on_activity: Some(cb),
        };
        let _driver = factory.build_lead(&ctx).unwrap();
        let calls = factory.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "build_lead called exactly once for the lead");
        assert_eq!(calls[0].0, "claude-opus-4-8", "lead built for the strongest model");
        assert_eq!(calls[0].1, std::path::PathBuf::from("/work/crate"));
        assert!(calls[0].2, "the activity heartbeat is threaded to build_lead");
    }

    /// The session carries the lead role's rule subset so the native (non-CLI) lead path can
    /// evaluate writes + seed its per-model child factory under the SAME subset (the contract
    /// the gate depends on).
    #[test]
    fn orchestrator_session_carries_role_rule_subset() {
        let role = Role {
            name: "Lead".into(),
            rule_subset: vec![RuleId("GOV-1".into()), RuleId("SEC-NO-PATH-ESCAPE-1".into())],
            allowed_paths: vec!["crate/".into()],
        };
        let session = orchestrator::prepare_orchestrator_session(
            std::path::Path::new("/bin/camerata-gateway"),
            &role,
            std::path::Path::new("/work/crate"),
            &crate::tier::TierMap::default(),
            false,
        )
        .unwrap();
        assert_eq!(session.role_rule_subset, role.rule_subset);
    }

    /// Compile-time / API-shape test: the deepest tiered build fn accepts the optional
    /// `orch_factory` (BACK-COMPAT: both `None` and `Some` are callable; `None` keeps the
    /// exact current CLI orchestrator behavior). Not run (needs a live gateway + corpus).
    #[test]
    fn build_with_orch_factory_signature_compiles_none_and_some() {
        fn _check(
            plan: &camerata_intake::Plan,
            root: &std::path::Path,
            bin: &std::path::Path,
            factory: SharedOrchestratorDriverFactory,
        ) {
            let tier_map = crate::tier::TierMap::default();
            // None => CLI orchestrator (back-compat).
            let _none: std::pin::Pin<Box<dyn std::future::Future<Output = _>>> =
                Box::pin(build_from_plan_with_tier_map_layer2_and_activity(
                    plan, root, bin, &tier_map, 1, false, &|_| {}, None, false, None,
                ));
            // Some => lead via the factory.
            let _some: std::pin::Pin<Box<dyn std::future::Future<Output = _>>> =
                Box::pin(build_from_plan_with_tier_map_layer2_and_activity(
                    plan, root, bin, &tier_map, 1, false, &|_| {}, None, false, Some(factory),
                ));
        }
        let _ = _check
            as fn(_, _, _, SharedOrchestratorDriverFactory);
    }
}
