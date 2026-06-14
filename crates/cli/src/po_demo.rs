//! `po-demo` — the LIVE Product-Owner-mode pipeline, end to end.
//!
//! This is the second abstraction level (VISION P5 / the "two abstraction levels"
//! subsection) driven all the way through:
//!
//!   1. Take a DELIBERATELY UNDERSPECIFIED [`IntakeForm`] (a minimal budgeting
//!      app: just an `Expense` entity with an `amount` field, vague description).
//!      This stands in for a PO who did not fill in enough detail.
//!   2. Run the MULTI-TURN CLARIFY LOOP over it: the [`ClarifyDriver`] calls
//!      [`ClaudeLeadEngineer`] repeatedly; when the engineer asks questions,
//!      scripted [`SequentialAnswerSource`] answers are folded back into the form
//!      and the engineer is re-evaluated (up to 3 turns). If the live call fails
//!      at any turn, we FALL BACK to [`StubLeadEngineer`] and say so. The
//!      fallback is honest, not a fake success.
//!   3. Hand the plan's tasks to the GOVERNED [`FleetCoordinator`] to build into a
//!      temp worktree — each task becomes one governed `claude -p` agent locked to
//!      the Rust gateway's gated-write tool (identical governance to `build-demo`).
//!   4. `cargo build` + `cargo test` the produced crate.
//!   5. Print a PO-DEMO summary: the plan, where it came from, how many clarify
//!      turns were needed, and whether the governed build compiled + tested.
//!
//! Honesty: we never hand-write the agents' app code. The lead engineer plans;
//! governed agents build; cargo judges. Any imperfection (a stub fallback, a
//! non-compiling governed build) is reported as PARTIAL with the exact reason.

use std::time::Instant;

use camerata_agent::{prepare_session, GATED_WRITE_TOOL};
use camerata_core::{FleetCoordinator, FleetStage, Role};
use camerata_intake::{
    ClarifyDriver, ClarifyOutcome, ClaudeLeadEngineer, IntakeForm, Plan, PlanTask,
    SequentialAnswerSource, StubLeadEngineer, TaskKind,
};

use camerata_checks::RustCheckRunner;

use crate::fleet_support::{
    governed_role, locate_gateway_bin, run_cargo, scaffold_crate, tail_lines, FLEET_DOMAINS,
};

/// Where the plan the governed fleet built came from. Surfaced in the summary so
/// a stub fallback is never mistaken for a live evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanSource {
    /// The live `ClaudeLeadEngineer` resolved (possibly after clarify turns).
    LiveLeadEngineer,
    /// The live call failed or was unresolvable; the deterministic stub produced
    /// the plan.
    StubFallback,
}

impl PlanSource {
    fn label(self) -> &'static str {
        match self {
            PlanSource::LiveLeadEngineer => "LIVE ClaudeLeadEngineer (clarify loop)",
            PlanSource::StubFallback => "StubLeadEngineer (live call failed — fell back)",
        }
    }
}

/// Scripted answers for the PO-demo's clarify loop.
///
/// These are plausible answers to common clarifying questions a lead engineer
/// might ask about an underspecified budgeting app. The `SequentialAnswerSource`
/// returns round 0 on the first clarify turn, round 1 on the second, etc.
/// The loop is capped at 3 turns so the demo always terminates.
fn demo_answer_source() -> SequentialAnswerSource {
    SequentialAnswerSource::new(vec![
        // Turn 1: answer questions about currency, category types, time range.
        vec![
            "USD".to_string(),
            "Food, Transport, Housing, Entertainment, Other".to_string(),
            "monthly view is fine, no date range filtering needed for v1".to_string(),
            "no multi-user support needed; single user only".to_string(),
            "no recurring expense tracking; each expense is a one-off entry".to_string(),
        ],
        // Turn 2: answer follow-up questions if the engineer asks more.
        vec![
            "no budget limits or alerts needed for v1".to_string(),
            "no export/import; just the in-app list view".to_string(),
            "use f64 for amounts; no need for a money type in v1".to_string(),
        ],
        // Turn 3: final round if still unresolved.
        vec![
            "keep it simple; any remaining questions can be deferred to v2".to_string(),
        ],
    ])
}

/// Run the lead engineer over `form` using the multi-turn clarify loop,
/// preferring the live evaluation (with scripted PO answers) and falling back
/// to the deterministic stub on any error or if the live call remains
/// unresolved after the turn cap.
async fn lead_engineer_plan(form: &IntakeForm) -> (Plan, PlanSource, Option<String>) {
    let live = ClaudeLeadEngineer::new();
    let answers = demo_answer_source();
    run_clarify_loop(&live, &answers, form).await
}

/// The clarify-loop body, with the engineer + answer source injected so it is
/// testable without a live `claude -p` call. [`lead_engineer_plan`] is the
/// production wrapper that injects the live [`ClaudeLeadEngineer`] and the
/// scripted [`demo_answer_source`]; tests inject a [`StubLeadEngineer`] (or a
/// custom engineer that drives clarify turns) and a deterministic answer source.
async fn run_clarify_loop(
    engineer: &dyn camerata_intake::LeadEngineer,
    answers: &dyn camerata_intake::AnswerSource,
    form: &IntakeForm,
) -> (Plan, PlanSource, Option<String>) {
    let driver = ClarifyDriver::new(engineer, answers, 3);

    match driver.run(form).await {
        Ok(ClarifyOutcome::Resolved {
            plan,
            clarify_turns,
            ..
        }) => {
            if clarify_turns > 0 {
                println!(
                    "  clarify loop: resolved after {clarify_turns} Q&A turn(s)"
                );
            }
            (plan, PlanSource::LiveLeadEngineer, None)
        }
        Ok(ClarifyOutcome::Unresolved {
            turns_attempted,
            last_questions,
            ..
        }) => {
            let reason = format!(
                "live lead engineer still unresolved after {turns_attempted} clarify \
                 turn(s); last questions: {}",
                last_questions.join(" | "),
            );
            (
                StubLeadEngineer::plan_for(form),
                PlanSource::StubFallback,
                Some(reason),
            )
        }
        Ok(ClarifyOutcome::NeedsArchitect { reason, .. }) => (
            StubLeadEngineer::plan_for(form),
            PlanSource::StubFallback,
            Some(format!("lead engineer recommends a human architect: {reason}")),
        ),
        Ok(ClarifyOutcome::TooComplex { reason, .. }) => (
            StubLeadEngineer::plan_for(form),
            PlanSource::StubFallback,
            Some(format!("lead engineer: request too complex for Camerata alone: {reason}")),
        ),
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
    // Use the underspecified form to exercise the multi-turn clarify loop: it
    // is intentionally sparse (no category field, vague description) so a
    // discerning live lead engineer will ask clarifying questions. The clarify
    // driver will answer with scripted responses and re-evaluate until a plan
    // is produced or the turn cap (3) is exhausted.
    let form = IntakeForm::sample_underspecified_app();
    println!("── 1. INTAKE FORM (the Product Owner's submission — deliberately underspecified) ──");
    print!("{}", form.brief());
    println!();

    // ── 2. Lead engineer evaluates the form → Plan (with clarify loop) ──────
    println!("── 2. LEAD ENGINEER evaluation (multi-turn clarify loop, max 3 turns) ──");
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

    // LAYER-2 DURING THE FLEET: each stage's governed write is structurally
    // checked by the REAL Rust gate (fmt + clippy) before the next stage runs,
    // and a dirty stage bounces-and-revises once citing the violated rule id.
    // This is the same `RustCheckRunner` the fleet integration test exercises;
    // wiring it here (instead of a no-op) means the governed build is genuinely
    // gated per stage, not just judged by the final cargo run. Because each
    // governed agent writes a complete, self-contained `lib.rs`, the crate
    // compiles at each stage boundary, so clippy is meaningful mid-fleet.
    let checks = RustCheckRunner::new();
    let fleet = FleetCoordinator::new(&checks, &worktree);

    println!(
        "  layer-2 gate (per stage):              RustCheckRunner (fmt + clippy, bounce-and-revise once)"
    );
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
        // Layer-2 result for this stage: what the REAL Rust gate found on the
        // first pass, whether it bounced, and whether it ended clean.
        if r.initial_violations.is_empty() {
            println!("    layer-2:    clean on first pass (no fmt/clippy violations)");
        } else {
            let ids: Vec<&str> = r.initial_violations.iter().map(|v| v.0.as_str()).collect();
            println!(
                "    layer-2:    initial violations [{}] → bounced: {} → final clean: {}",
                ids.join(", "),
                yesno(r.bounced),
                yesno(r.final_violations.is_empty()),
            );
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
    println!("  layer-2 gate during fleet:                RustCheckRunner (fmt + clippy)");
    println!("  per-stage layer-2 bounces:                {}", report.total_bounces());
    println!("  fleet ended clean (no residual layer-2):  {}", yesno(report.is_clean()));
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

    #[tokio::test]
    async fn clarify_loop_resolves_with_stub_engineer_no_network() {
        // The StubLeadEngineer returns Ready immediately, so the clarify-loop
        // wiring resolves in 0 turns and reports a LIVE-path plan (here "live"
        // is the injected stub standing in for the real engineer). This proves
        // run_clarify_loop drives the multi-turn loop deterministically without
        // any claude -p call.
        let engineer = StubLeadEngineer::new();
        let answers = camerata_intake::StubAnswerSource::uniform(vec![]);
        let form = IntakeForm::sample_underspecified_app();
        let (plan, source, reason) = run_clarify_loop(&engineer, &answers, &form).await;
        assert_eq!(source, PlanSource::LiveLeadEngineer);
        assert!(reason.is_none());
        assert!(plan.is_buildable());
    }

    #[tokio::test]
    async fn clarify_loop_folds_answers_and_resolves_after_a_turn() {
        // An engineer that asks one question then yields a plan exercises the
        // multi-turn fold: the scripted answer source supplies the answer, the
        // driver folds it into the form, and the second evaluate yields Ready.
        use async_trait::async_trait;
        use camerata_intake::{
            ConfidenceScore, HonestyVerdict, Intake, LeadEngineer, LeadEngineerError,
            LeadEngineerResponse,
        };

        fn minimal_response() -> LeadEngineerResponse {
            LeadEngineerResponse {
                checklist: vec![],
                confidence: ConfidenceScore::new(90),
                suggestions: vec![],
                verdict: HonestyVerdict::Proceed,
                questions: vec![],
            }
        }

        struct OneQuestionEngineer {
            asked: std::sync::atomic::AtomicBool,
        }
        #[async_trait]
        impl LeadEngineer for OneQuestionEngineer {
            async fn evaluate(&self, form: &IntakeForm) -> Result<Intake, LeadEngineerError> {
                if self
                    .asked
                    .swap(true, std::sync::atomic::Ordering::Relaxed)
                {
                    // Second call: the answer must have been folded in.
                    assert_eq!(form.clarifications.len(), 1);
                    Ok(Intake::Ready {
                        plan: StubLeadEngineer::plan_for(form),
                        response: minimal_response(),
                    })
                } else {
                    Ok(Intake::NeedsClarification {
                        questions: vec!["Which currency?".into()],
                        response: LeadEngineerResponse {
                            questions: vec!["Which currency?".into()],
                            confidence: ConfidenceScore::new(40),
                            ..minimal_response()
                        },
                    })
                }
            }
        }

        let engineer = OneQuestionEngineer {
            asked: std::sync::atomic::AtomicBool::new(false),
        };
        let answers = camerata_intake::StubAnswerSource::uniform(vec!["USD".into()]);
        let form = IntakeForm::sample_underspecified_app();
        let (plan, source, reason) = run_clarify_loop(&engineer, &answers, &form).await;
        assert_eq!(source, PlanSource::LiveLeadEngineer);
        assert!(reason.is_none());
        assert!(plan.is_buildable());
    }

    #[tokio::test]
    async fn clarify_loop_falls_back_to_stub_when_turn_cap_exhausted() {
        // An engineer that never stops asking exhausts the 3-turn cap; the demo
        // must fall back to the deterministic stub plan and report the reason,
        // never a faked success.
        use async_trait::async_trait;
        use camerata_intake::{
            ConfidenceScore, HonestyVerdict, Intake, LeadEngineer, LeadEngineerError,
            LeadEngineerResponse,
        };

        struct AlwaysAsksEngineer;
        #[async_trait]
        impl LeadEngineer for AlwaysAsksEngineer {
            async fn evaluate(&self, _form: &IntakeForm) -> Result<Intake, LeadEngineerError> {
                Ok(Intake::NeedsClarification {
                    questions: vec!["Still need info?".into()],
                    response: LeadEngineerResponse {
                        checklist: vec![],
                        confidence: ConfidenceScore::new(30),
                        suggestions: vec![],
                        verdict: HonestyVerdict::Proceed,
                        questions: vec!["Still need info?".into()],
                    },
                })
            }
        }

        let answers = camerata_intake::StubAnswerSource::uniform(vec!["dunno".into()]);
        let form = IntakeForm::sample_underspecified_app();
        let (plan, source, reason) =
            run_clarify_loop(&AlwaysAsksEngineer, &answers, &form).await;
        assert_eq!(source, PlanSource::StubFallback);
        assert!(reason.is_some());
        assert!(plan.is_buildable(), "fallback must still be buildable");
    }

    #[test]
    fn po_demo_uses_a_real_rust_check_runner_for_layer2() {
        // Compile-level guarantee that the demo's layer-2 gate is the REAL
        // RustCheckRunner (fmt + clippy), not a no-op. The fleet-level proof
        // that this runner actually bounces a violation mid-fleet lives in
        // crates/core/tests/fleet_real_check.rs.
        let checks = RustCheckRunner::new();
        let _fleet = FleetCoordinator::new(&checks, std::env::temp_dir());
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
