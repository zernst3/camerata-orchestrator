//! `po-demo` — the LIVE Product-Owner-mode pipeline, end to end.
//!
//! This is the second abstraction level (VISION P5 / the "two abstraction levels"
//! subsection) driven all the way through:
//!
//!   1. Take a SAMPLE filled [`IntakeForm`] (a tiny budgeting app: an `Expense`
//!      entity with a few fields + a list view). This stands in for what a
//!      Product Owner submits.
//!   2. Run the [`LeadEngineer`] over it to produce a [`Plan`]. We try the REAL
//!      [`ClaudeLeadEngineer`] first; if the live call fails (offline, CLI
//!      missing, unparseable output) we FALL BACK to [`StubLeadEngineer`] and say
//!      so. The fallback is honest, not a fake success.
//!   3. Hand the plan's tasks to the GOVERNED [`FleetCoordinator`] to build into a
//!      temp worktree — each task becomes one governed `claude -p` agent locked to
//!      the Rust gateway's gated-write tool (identical governance to `build-demo`).
//!   4. `cargo build` + `cargo test` the produced crate.
//!   5. Print a PO-DEMO summary: the plan, where it came from, and whether the
//!      governed build compiled + tested.
//!
//! Honesty: we never hand-write the agents' app code. The lead engineer plans;
//! governed agents build; cargo judges. Any imperfection (a stub fallback, a
//! non-compiling governed build) is reported as PARTIAL with the exact reason.

use std::time::Instant;

use camerata_agent::{prepare_session, GATED_WRITE_TOOL};
use camerata_core::{FleetCoordinator, FleetStage, Role};
use camerata_intake::{
    ClaudeLeadEngineer, IntakeForm, LeadEngineer, Plan, PlanTask, StubLeadEngineer, TaskKind,
};

use crate::fleet_support::{
    governed_role, locate_gateway_bin, run_cargo, scaffold_crate, tail_lines, NoopChecks,
    FLEET_DOMAINS,
};

/// Where the plan the governed fleet built came from. Surfaced in the summary so
/// a stub fallback is never mistaken for a live evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanSource {
    /// The live `ClaudeLeadEngineer` produced the plan.
    LiveLeadEngineer,
    /// The live call failed; the deterministic stub produced the plan.
    StubFallback,
}

impl PlanSource {
    fn label(self) -> &'static str {
        match self {
            PlanSource::LiveLeadEngineer => "LIVE ClaudeLeadEngineer",
            PlanSource::StubFallback => "StubLeadEngineer (live call failed — fell back)",
        }
    }
}

/// Run the lead engineer over `form`, preferring the live evaluation and falling
/// back to the deterministic stub on any failure. Returns the plan, where it came
/// from, and (if live failed) why.
async fn lead_engineer_plan(form: &IntakeForm) -> (Plan, PlanSource, Option<String>) {
    let live = ClaudeLeadEngineer::new();
    match live.evaluate(form).await {
        Ok(intake) => {
            if let Some(plan) = intake.plan() {
                return (plan.clone(), PlanSource::LiveLeadEngineer, None);
            }
            // The lead engineer asked for clarification rather than planning. V1's
            // multi-turn clarify loop is not built yet, so we fall back to the
            // stub to keep the pipeline flowing, and record what happened.
            let reason = format!(
                "lead engineer returned {} clarifying question(s) (multi-turn \
                 clarify loop not yet implemented)",
                intake.questions().len()
            );
            (
                StubLeadEngineer::plan_for(form),
                PlanSource::StubFallback,
                Some(reason),
            )
        }
        Err(e) => (
            StubLeadEngineer::plan_for(form),
            PlanSource::StubFallback,
            Some(e.to_string()),
        ),
    }
}

/// Convert ONE plan task into a precise governed-fleet task instruction.
///
/// The plan's `description` is the engineering intent; here we wrap it with the
/// concrete governed-write contract (the agent's ONLY mutation path is the gated
/// tool, written once to the shared `lib.rs`). Earlier stages' writes are visible
/// to later ones because the worktree is shared — so a test stage is told to READ
/// the implementer's file first.
fn stage_task_for(task: &PlanTask, lib_path_display: &str, is_first: bool) -> String {
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

/// Run the full PO-mode pipeline and report PASS / PARTIAL.
pub async fn run_po_demo() -> anyhow::Result<()> {
    println!("== Camerata PO-MODE pipeline (intake → lead engineer → governed fleet → cargo) ==");
    println!();

    // ── 1. The sample Product-Owner intake form ──────────────────────────────
    let form = IntakeForm::sample_budgeting_app();
    println!("── 1. INTAKE FORM (the Product Owner's submission) ──");
    print!("{}", form.brief());
    println!();

    // ── 2. Lead engineer evaluates the form → Plan ───────────────────────────
    println!("── 2. LEAD ENGINEER evaluation ──");
    println!("  attempting LIVE ClaudeLeadEngineer ({}) ...", camerata_intake::engine::DEFAULT_LEAD_ENGINEER_MODEL);
    let t_le = Instant::now();
    let (plan, plan_source, fallback_reason) = lead_engineer_plan(&form).await;
    let le_wall = t_le.elapsed();
    println!("  plan source: {}", plan_source.label());
    if let Some(reason) = &fallback_reason {
        println!("  live-call note: {reason}");
    }
    println!("  lead-engineer wall: {:.2}s", le_wall.as_secs_f64());
    println!();
    print!("{}", plan.render());
    println!();

    // The plan must be buildable to continue (the stub guarantees this, so this
    // is defensive against a future live plan that arrives empty).
    if !plan.is_buildable() {
        eprintln!("PO-DEMO: PARTIAL (lead engineer produced an empty plan — nothing to build)");
        std::process::exit(1);
    }

    // ── 3. Map the plan onto a GOVERNED fleet over a temp worktree ────────────
    let gateway_bin = locate_gateway_bin()?;
    eprintln!("[po-demo] gateway binary: {}", gateway_bin.display());

    let root = std::env::temp_dir().join(format!("camerata-po-{}", std::process::id()));
    let worktree = root.join("crate");
    let crate_name = "po_demo_crate";
    let _ = std::fs::remove_dir_all(&root);
    scaffold_crate(&worktree, crate_name)?;
    let lib_path = worktree.join("src").join("lib.rs");
    let lib_path_display = lib_path.display().to_string();

    println!("── 3. GOVERNED FLEET BUILD ──");
    println!("  governed tool (agents locked to this): {GATED_WRITE_TOOL}");
    println!("  shared worktree (cargo lib crate):     {}", worktree.display());
    println!("  corpus domains:                        {FLEET_DOMAINS:?}");
    println!("  fleet stages (one governed agent each): {}", plan.tasks.len());

    // Build a governed role + per-session driver for each plan task, then a
    // FleetStage. We must hold the roles + session-spawns + drivers alive for the
    // duration of the fleet run, so collect them into owned vectors first.
    let mut roles: Vec<Role> = Vec::with_capacity(plan.tasks.len());
    for (i, task) in plan.tasks.iter().enumerate() {
        // Per-aggregate role naming: use the plan's role name, de-duplicated by
        // stage index so two tasks with the same role name get distinct sessions.
        let role_name = format!("{}-{}", task.role, i + 1);
        let role = governed_role(&role_name).await?;
        roles.push(role);
    }

    // Per-session governed drivers (each agent its own session: own rules.json +
    // mcp-config, all bound to the shared worktree).
    let mut spawns = Vec::with_capacity(plan.tasks.len());
    for (i, role) in roles.iter().enumerate() {
        let session_dir = root.join(format!("session-{}", i + 1));
        let spawn = prepare_session(&session_dir, &gateway_bin, role)?;
        spawns.push(spawn);
    }
    let drivers: Vec<_> = spawns
        .iter()
        .map(|spawn| spawn.driver.clone().with_worktree(&worktree))
        .collect();

    // Build the stage list. First task overwrites; later tasks read-then-extend.
    let mut stages: Vec<FleetStage> = Vec::with_capacity(plan.tasks.len());
    for (i, task) in plan.tasks.iter().enumerate() {
        let stage_task = stage_task_for(task, &lib_path_display, i == 0);
        stages.push(FleetStage::new(roles[i].clone(), stage_task, &drivers[i]));
        println!(
            "    stage {}: role={} kind={} ({})",
            i + 1,
            roles[i].name,
            task.kind.label(),
            describe_task_kind(task.kind),
        );
    }
    println!();

    let checks = NoopChecks;
    let fleet = FleetCoordinator::new(&checks, &worktree);

    println!(
        "  Running governed fleet: {} live `claude -p` agent(s) through the gate ...",
        stages.len()
    );
    let t0 = Instant::now();
    let report = fleet.run(&stages).await?;
    let fleet_wall = t0.elapsed();
    println!("  fleet wall: {:.2}s", fleet_wall.as_secs_f64());
    println!();

    let mut all_agents_ran = true;
    for (i, stage) in report.stages.iter().enumerate() {
        let r = &stage.report;
        println!("  ── stage {}: {} ──", i + 1, stage.role_name);
        println!("    session_id: {}", r.initial_outcome.session_id);
        if let Some(cost) = r.initial_outcome.cost_usd {
            println!("    cost_usd:   {cost:.6}");
        }
        println!(
            "    agent said: {}",
            tail_lines(&r.initial_outcome.result.replace('\n', " "), 1)
                .first()
                .cloned()
                .unwrap_or_default()
        );
        if r.initial_outcome.session_id.is_empty() {
            all_agents_ran = false;
        }
    }
    println!();

    // The filesystem is the source of truth that the gate actually wrote.
    let produced = std::fs::read_to_string(&lib_path).unwrap_or_default();
    let wrote_through_gate = lib_path.exists() && !produced.trim_start().starts_with("// placeholder");
    println!("  ── produced src/lib.rs ──");
    println!("    path:  {}", lib_path.display());
    println!("    bytes: {}", produced.len());
    println!("    wrote through gate (non-placeholder): {wrote_through_gate}");
    println!();

    // ── 4. cargo build + test the produced crate ─────────────────────────────
    println!("── 4. VERIFY (cargo build + test on the governed-built crate) ──");
    let build = run_cargo(&worktree, "build").await?;
    println!("  cargo build success: {}", build.success);
    if !build.success {
        println!("  --- cargo build stderr (tail) ---");
        for line in tail_lines(&build.stderr, 20) {
            println!("  {line}");
        }
    }

    let test = if build.success {
        let test = run_cargo(&worktree, "test").await?;
        println!("  cargo test success:  {}", test.success);
        println!("  --- cargo test stdout (tail) ---");
        for line in tail_lines(&test.stdout, 10) {
            println!("  {line}");
        }
        Some(test)
    } else {
        println!("  (skipping cargo test — build failed)");
        None
    };
    println!();

    // ── 5. PO-DEMO summary ────────────────────────────────────────────────────
    let compiled = build.success;
    let tests_passed = test.as_ref().map(|t| t.success).unwrap_or(false);

    println!("── PO-DEMO SUMMARY ──");
    println!("  intake form:                              {} ({} entity, {} view)", form.app_name, form.entities.len(), form.views.len());
    println!("  plan source:                              {}", plan_source.label());
    println!("  plan tasks (governed fleet stages):       {}", plan.task_count());
    println!("  all governed agents ran live:             {}", yesno(all_agents_ran));
    println!("  produced code through the gate:           {}", yesno(wrote_through_gate));
    println!("  governed build compiled:                  {}", yesno(compiled));
    println!("  governed build tests passed:              {}", yesno(tests_passed));
    println!();

    if all_agents_ran && wrote_through_gate && compiled && tests_passed {
        println!(
            "PO-DEMO: PASS (a Product-Owner form was evaluated by the lead engineer \
             into a plan, and the governed fleet built it into a compiling, passing \
             crate through the Rust gate)"
        );
        Ok(())
    } else if all_agents_ran && wrote_through_gate {
        println!(
            "PO-DEMO: PARTIAL (the full PO pipeline ran — form → {src} → governed \
             fleet → cargo — and produced real files through the gate, but the \
             governed build did not {what} first try. Honest engine-quality signal, \
             NOT a harness failure.)",
            src = plan_source.label(),
            what = if !compiled { "compile" } else { "pass tests" },
        );
        Ok(())
    } else {
        eprintln!(
            "PO-DEMO: PARTIAL (pipeline ran but a governed agent did not produce the \
             expected gated write — see stage output above)"
        );
        std::process::exit(1);
    }
}

/// A one-liner describing what a task kind contributes, for the stage list.
fn describe_task_kind(kind: TaskKind) -> &'static str {
    match kind {
        TaskKind::Database => "persistence/schema",
        TaskKind::Backend => "domain types / API",
        TaskKind::Frontend => "views/screens",
        TaskKind::Test => "tests over the produced code",
    }
}

/// "YES"/"NO" for the summary table.
fn yesno(b: bool) -> &'static str {
    if b {
        "YES"
    } else {
        "NO"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_intake::{Plan, PlanTask, TaskKind};

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

    #[test]
    fn plan_source_labels_distinguish_live_from_fallback() {
        assert!(PlanSource::LiveLeadEngineer.label().contains("LIVE"));
        assert!(PlanSource::StubFallback.label().contains("fell back"));
    }

    #[test]
    fn buildable_plan_maps_to_one_stage_per_task() {
        // A sanity check on the count contract the fleet wiring relies on.
        let plan = Plan {
            app_name: "x".to_string(),
            summary: "s".to_string(),
            tasks: vec![
                PlanTask {
                    role: "A".to_string(),
                    kind: TaskKind::Backend,
                    description: "d1".to_string(),
                },
                PlanTask {
                    role: "B".to_string(),
                    kind: TaskKind::Test,
                    description: "d2".to_string(),
                },
            ],
        };
        assert!(plan.is_buildable());
        assert_eq!(plan.task_count(), 2);
    }
}
