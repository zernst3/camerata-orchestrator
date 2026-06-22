//! `po-demo` — the LIVE Product-Owner-mode pipeline, end to end.
//!
//! This is the second abstraction level (VISION P5 / the "two abstraction
//! levels" subsection) driven all the way through:
//!
//!   1. Take a DELIBERATELY UNDERSPECIFIED [`IntakeForm`] (a minimal budgeting
//!      app: just an `Expense` entity with an `amount` field, vague
//!      description). This stands in for a PO who did not fill in enough detail.
//!   2. Run the MULTI-TURN CLARIFY LOOP over it: the [`ClarifyDriver`] calls
//!      [`ClaudeLeadEngineer`] repeatedly; when the engineer asks questions,
//!      scripted [`SequentialAnswerSource`] answers are folded back into the
//!      form and the engineer is re-evaluated (up to 3 turns). If the live call
//!      fails at any turn, we FALL BACK to [`StubLeadEngineer`] and say so.
//!      The fallback is honest, not a fake success.
//!   3. Hand the plan's tasks to the GOVERNED [`camerata_fleet::build_from_plan`]
//!      runner, which builds a governed fleet over a temp worktree: each task
//!      becomes one governed `claude -p` agent locked to the Rust gateway's
//!      gated-write tool (identical governance to `build-demo`).
//!   4. `cargo build` + `cargo test` the produced crate (done inside
//!      `build_from_plan`).
//!   5. Print a PO-DEMO summary: the plan, where it came from, how many clarify
//!      turns were needed, and whether the governed build compiled + tested.
//!
//! Honesty: we never hand-write the agents' app code. The lead engineer plans;
//! governed agents build; cargo judges. Any imperfection (a stub fallback, a
//! non-compiling governed build) is reported as PARTIAL with the exact reason.

use std::time::Instant;

use camerata_fleet::{describe_task_kind, locate_gateway_bin, BuildEvent, FLEET_DOMAINS};
use camerata_intake::{
    ClarifyDriver, ClarifyOutcome, ClaudeLeadEngineer, IntakeForm, Plan, SequentialAnswerSource,
    StubLeadEngineer, TaskKind,
};

/// Where the plan the governed fleet built came from. Surfaced in the summary
/// so a stub fallback is never mistaken for a live evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanSource {
    /// The live `ClaudeLeadEngineer` resolved (possibly after clarify turns).
    LiveLeadEngineer,
    /// The live call failed or was unresolvable; the deterministic stub
    /// produced the plan.
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
/// might ask about an underspecified budgeting app. The
/// `SequentialAnswerSource` returns round 0 on the first clarify turn, round
/// 1 on the second, etc. The loop is capped at 3 turns so the demo always
/// terminates.
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
        vec!["keep it simple; any remaining questions can be deferred to v2".to_string()],
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
/// custom engineer that drives clarify turns) and a deterministic answer
/// source.
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
                println!("  clarify loop: resolved after {clarify_turns} Q&A turn(s)");
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
            Some(format!(
                "lead engineer recommends a human architect: {reason}"
            )),
        ),
        Ok(ClarifyOutcome::TooComplex { reason, .. }) => (
            StubLeadEngineer::plan_for(form),
            PlanSource::StubFallback,
            Some(format!(
                "lead engineer: request too complex for Camerata alone: {reason}"
            )),
        ),
        Err(e) => (
            StubLeadEngineer::plan_for(form),
            PlanSource::StubFallback,
            Some(e.to_string()),
        ),
    }
}

/// Run the full PO-mode pipeline and report PASS / PARTIAL.
pub async fn run_po_demo() -> anyhow::Result<()> {
    println!(
        "== Camerata PO-MODE pipeline (intake -> lead engineer -> governed fleet -> cargo) =="
    );
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

    // ── 2. Lead engineer evaluates the form -> Plan (with clarify loop) ──────
    println!("── 2. LEAD ENGINEER evaluation (multi-turn clarify loop, max 3 turns) ──");
    println!(
        "  attempting LIVE ClaudeLeadEngineer ({}) ...",
        camerata_intake::engine::DEFAULT_LEAD_ENGINEER_MODEL
    );
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

    // The plan must be buildable to continue (the stub guarantees this, so
    // this is defensive against a future live plan that arrives empty).
    if !plan.is_buildable() {
        eprintln!("PO-DEMO: PARTIAL (lead engineer produced an empty plan — nothing to build)");
        std::process::exit(1);
    }

    // ── 3. Map the plan onto a GOVERNED fleet over a temp worktree ────────────
    let gateway_bin = locate_gateway_bin()?;
    eprintln!("[po-demo] gateway binary: {}", gateway_bin.display());

    let root = std::env::temp_dir().join(format!("camerata-po-{}", std::process::id()));

    println!("── 3. GOVERNED FLEET BUILD ──");
    println!("  corpus domains:                        {FLEET_DOMAINS:?}");
    println!(
        "  fleet stages (one governed agent each): {}",
        plan.tasks.len()
    );
    println!();

    // ── 4. Run the governed build via camerata_fleet::build_from_plan ─────────
    let t0 = Instant::now();
    let outcome =
        camerata_fleet::build_from_plan(&plan, &root, &gateway_bin, &|event| match event {
            BuildEvent::Scaffolding => {
                println!("  [fleet] scaffolding worktree ...");
            }
            BuildEvent::StageStarted {
                index,
                total,
                role,
                kind,
            } => {
                println!(
                    "    stage {}/{}: role={} kind={} ({})",
                    index + 1,
                    total,
                    role,
                    kind,
                    describe_task_kind(match kind.as_str() {
                        "database" => TaskKind::Database,
                        "backend" => TaskKind::Backend,
                        "frontend" => TaskKind::Frontend,
                        _ => TaskKind::Test,
                    }),
                );
            }
            BuildEvent::AgentTier {
                index,
                role,
                model,
                is_lead,
            } => {
                println!(
                    "    stage {}: {} -> {}{}",
                    index + 1,
                    role,
                    model,
                    if is_lead { " (lead/orchestrator)" } else { "" },
                );
            }
            BuildEvent::Layer2Result {
                index,
                total,
                passed,
                violated_rules,
            } => {
                println!(
                    "    stage {}/{} layer-2: {}{}",
                    index + 1,
                    total,
                    if passed { "passed" } else { "FAILED" },
                    if violated_rules.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", violated_rules.join(", "))
                    },
                );
            }
            BuildEvent::ReviseIteration {
                index,
                violated_rules,
            } => {
                println!(
                    "    stage {}: bounce-and-revise [{}]",
                    index + 1,
                    violated_rules.join(", "),
                );
            }
            BuildEvent::StageFinished {
                index,
                total,
                clean,
                bounced,
                session_id,
            } => {
                println!(
                    "    stage {}/{} done: session={} clean={} bounced={}",
                    index + 1,
                    total,
                    session_id,
                    clean,
                    bounced,
                );
            }
            BuildEvent::Verifying => {
                println!("── 4. VERIFY (cargo build + test on the governed-built crate) ──");
            }
            BuildEvent::Done {
                compiled,
                tests_passed,
            } => {
                println!("  cargo build success: {compiled}");
                println!("  cargo test success:  {tests_passed}");
            }
        })
        .await?;
    let fleet_wall = t0.elapsed();
    println!("  fleet wall: {:.2}s", fleet_wall.as_secs_f64());
    println!();

    println!("  ── produced src/lib.rs ──");
    println!("    path:  {}", outcome.produced_path.display());
    println!("    bytes: {}", outcome.produced_bytes);
    println!(
        "    wrote through gate (non-placeholder): {}",
        outcome.wrote_through_gate
    );
    println!();

    // ── 5. PO-DEMO summary ────────────────────────────────────────────────────
    println!("── PO-DEMO SUMMARY ──");
    println!(
        "  intake form:                              {} ({} entity, {} view)",
        form.app_name,
        form.entities.len(),
        form.views.len()
    );
    println!(
        "  plan source:                              {}",
        plan_source.label()
    );
    println!(
        "  plan tasks (governed fleet stages):       {}",
        plan.task_count()
    );
    println!("  layer-2 gate during fleet:                language-matched runner (Rust: fmt+clippy+test; JS/Py/Go: lint+test)");
    println!(
        "  per-stage layer-2 bounces:                {}",
        outcome.total_bounces
    );
    println!(
        "  fleet ended clean (no residual layer-2):  {}",
        yesno(outcome.fleet_clean)
    );
    println!(
        "  all governed agents ran live:             {}",
        yesno(outcome.all_agents_ran)
    );
    println!(
        "  produced code through the gate:           {}",
        yesno(outcome.wrote_through_gate)
    );
    println!(
        "  governed build compiled:                  {}",
        yesno(outcome.compiled)
    );
    println!(
        "  governed build tests passed:              {}",
        yesno(outcome.tests_passed)
    );
    println!();

    if outcome.all_agents_ran
        && outcome.wrote_through_gate
        && outcome.compiled
        && outcome.tests_passed
    {
        println!(
            "PO-DEMO: PASS (a Product-Owner form was evaluated by the lead engineer \
             into a plan, and the governed fleet built it into a compiling, passing \
             crate through the Rust gate)"
        );
        Ok(())
    } else if outcome.all_agents_ran && outcome.wrote_through_gate {
        println!(
            "PO-DEMO: PARTIAL (the full PO pipeline ran — form -> {src} -> governed \
             fleet -> cargo — and produced real files through the gate, but the \
             governed build did not {what} first try. Honest engine-quality signal, \
             NOT a harness failure.)",
            src = plan_source.label(),
            what = if !outcome.compiled {
                "compile"
            } else {
                "pass tests"
            },
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
    use camerata_agent::GATED_WRITE_TOOL;
    use camerata_fleet::stage_task_for;
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
                if self.asked.swap(true, std::sync::atomic::Ordering::Relaxed) {
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
        // An engineer that never stops asking exhausts the 3-turn cap; the
        // demo must fall back to the deterministic stub plan and report the
        // reason, never a faked success.
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
        let (plan, source, reason) = run_clarify_loop(&AlwaysAsksEngineer, &answers, &form).await;
        assert_eq!(source, PlanSource::StubFallback);
        assert!(reason.is_some());
        assert!(plan.is_buildable(), "fallback must still be buildable");
    }

    #[test]
    fn po_demo_uses_a_language_matched_check_runner_for_layer2() {
        // Compile-level guarantee that the demo's layer-2 gate is the REAL,
        // language-matched runner (via runner_for_worktree), not a no-op. A
        // Cargo.toml worktree resolves to the Rust runner (fmt + clippy + test);
        // a package.json / go.mod / pyproject tree resolves to its language's
        // runner. The fleet-level proof that this runner actually bounces a
        // violation mid-fleet lives in crates/core/tests/fleet_real_check.rs.
        use camerata_checks::{detect_language, runner_for_worktree, WorktreeLanguage};
        use camerata_core::FleetCoordinator;

        let dir = std::env::temp_dir().join(format!(
            "po-demo-layer2-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        assert_eq!(detect_language(&dir), WorktreeLanguage::Rust);

        let checks = runner_for_worktree(&dir);
        let _fleet = FleetCoordinator::new(&*checks, &dir);
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
