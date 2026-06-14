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

use std::path::{Path, PathBuf};

use camerata_agent::{prepare_session, GATED_WRITE_TOOL};
use camerata_checks::RustCheckRunner;
use camerata_core::{CheckRunner, FleetCoordinator, FleetStage, Role, RuleId};
use camerata_gateway::{gov1_rule, sec_no_hardcoded_secrets_1_rule};
use camerata_intake::{Plan, PlanTask, TaskKind};
use camerata_rules::role_from_corpus;
pub use camerata_rules::DEFAULT_CORPUS_PATH;

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
    async fn check(&self, _role: &Role, _worktree: &Path) -> anyhow::Result<Vec<RuleId>> {
        Ok(vec![])
    }
}

// ─── governed_role ────────────────────────────────────────────────────────────

/// Build a governed role from the real corpus, named `role_name`, and ensure
/// the gateway-enforced gate rules (GOV-1 plus the hardcoded-secret rule) are
/// in the delivered subset so the per-session governance is genuinely active,
/// the same honest blend the live single-agent demo uses.
pub async fn governed_role(role_name: &str) -> anyhow::Result<Role> {
    let corpus = Path::new(DEFAULT_CORPUS_PATH);
    let mut role = role_from_corpus(corpus, role_name, FLEET_DOMAINS, &[]).await?;

    for gate_rule in [sec_no_hardcoded_secrets_1_rule(), gov1_rule()] {
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
pub async fn build_from_plan(
    plan: &Plan,
    root: &Path,
    gateway_bin: &Path,
    on_event: &(dyn Fn(BuildEvent) + Send + Sync),
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
    let mut spawns = Vec::with_capacity(total);
    for (i, role) in roles.iter().enumerate() {
        let session_dir = root.join(format!("session-{}", i + 1));
        let spawn = prepare_session(&session_dir, gateway_bin, role)?;
        spawns.push(spawn);
    }
    let drivers: Vec<_> = spawns
        .iter()
        .map(|spawn| spawn.driver.clone().with_worktree(&worktree))
        .collect();

    // ── Build the stage list ─────────────────────────────────────────────────
    let mut stages: Vec<FleetStage> = Vec::with_capacity(total);
    for (i, task) in plan.tasks.iter().enumerate() {
        on_event(BuildEvent::StageStarted {
            index: i,
            total,
            role: roles[i].name.clone(),
            kind: task.kind.label().to_string(),
        });
        let stage_task = stage_task_for(task, &lib_path_display, i == 0);
        stages.push(FleetStage::new(roles[i].clone(), stage_task, &drivers[i]));
    }

    // ── Run the governed fleet with the REAL RustCheckRunner ─────────────────
    let checks = RustCheckRunner::new();
    let fleet = FleetCoordinator::new(&checks, &worktree);
    let report = fleet.run(&stages).await?;

    // ── Emit per-stage finished events ───────────────────────────────────────
    let mut all_agents_ran = true;
    for (i, stage) in report.stages.iter().enumerate() {
        let r = &stage.report;
        if r.initial_outcome.session_id.is_empty() {
            all_agents_ran = false;
        }
        on_event(BuildEvent::StageFinished {
            index: i,
            total,
            clean: r.final_violations.is_empty(),
            bounced: r.bounced,
            session_id: r.initial_outcome.session_id.clone(),
        });
    }

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
    use camerata_agent::GATED_WRITE_TOOL;
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
}
