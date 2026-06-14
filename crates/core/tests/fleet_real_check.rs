#![allow(clippy::unwrap_used)]
//! End-to-end proof that LAYER-2 fires INSIDE THE FLEET against a REAL check.
//!
//! The unit tests in `src/fleet.rs` drive the per-stage bounce loop with a
//! *scripted* `CheckRunner`. The integration test in `tests/coordinator_real_check.rs`
//! proves the bounce loop against the REAL [`camerata_checks::FmtCheckRunner`]
//! for the *single*-role [`camerata_core::Coordinator`].
//!
//! This test closes the remaining gap: it proves the SAME real layer-2 gate
//! fires and bounces a stage *while a multi-stage [`FleetCoordinator`] is
//! running*, using the REAL [`camerata_checks::RustCheckRunner`] (fmt + clippy)
//! against an on-disk temp crate. That is the exact wiring `po-demo`/`build-demo`
//! stub out with a no-op check runner — here it is exercised for real.
//!
//! Topology:
//!   - Stage 1 ("Implementer"): a fake agent that, on its FIRST run, writes a
//!     BADLY-FORMATTED `src/main.rs`, and on the BOUNCE run (recognised by the
//!     `RUST-FMT` rule id the fleet appends to the task) writes the fmt-CLEAN
//!     version. Its output is the substrate stage 2 sees on the shared worktree.
//!   - Stage 2 ("Tester"): a fake agent that writes a clean, clippy-clean helper
//!     module. It never introduces a violation, so it must run exactly once.
//!
//! Assertions (the whole point):
//!   - stage 1 `initial_violations == [RUST-FMT]` (the REAL fmt gate caught it
//!     DURING the fleet),
//!   - stage 1 bounced exactly once and is clean afterwards (final empty),
//!   - stage 2 STILL RAN (a violation in an earlier stage does not abort the
//!     pipeline) and is itself clean,
//!   - the aggregate [`FleetReport`] reflects all of it (one bounce total, clean
//!     overall, stages in order).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use camerata_checks::{fmt_rule, RustCheckRunner};
use camerata_core::{AgentDriver, AgentOutcome, FleetCoordinator, FleetStage, Role, RuleId};

// ─── self-cleaning temp worktree (no new crate dependency) ───────────────────

struct TempWorktree {
    path: PathBuf,
}

impl TempWorktree {
    fn new(tag: &str) -> std::io::Result<Self> {
        let mut path = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        path.push(format!(
            "camerata-fleet-{tag}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempWorktree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

// ─── source bodies (all clippy-clean; only formatting differs) ───────────────
//
// The badly-formatted main still PARSES (rustfmt needs valid syntax, not a
// successful compile) and is clippy-clean — so the initial real check reports
// ONLY RUST-FMT, not a spurious RUST-CLIPPY. The clean version is
// rustfmt-canonical AND clippy-clean.

const BADLY_FORMATTED_MAIN: &str = "fn   main( ){let x=1;println!(\"{}\",x );}\n";
const CLEAN_MAIN: &str = "fn main() {\n    let x = 1;\n    println!(\"{}\", x);\n}\n";

/// The helper module stage 2 lands. Rustfmt-canonical and clippy-clean so the
/// tester stage never introduces a violation of its own.
const TESTER_HELPER: &str =
    "pub fn double(n: i32) -> i32 {\n    n * 2\n}\n\nfn main() {\n    let _ = double(21);\n}\n";

const CARGO_TOML: &str =
    "[package]\nname = \"fleet-probe\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\n";

// ─── fake stage 1: dirty first, clean on bounce ──────────────────────────────

/// Stage-1 agent: writes the badly-formatted file on its first invocation and
/// the clean file once it sees the bounced rule id in the task. Records every
/// task it received so the test can prove the bounce-back cited the violated
/// rule id.
struct ReviseOnBounceDriver {
    worktree: PathBuf,
    tasks: Mutex<Vec<String>>,
    bounce_marker: String,
}

impl ReviseOnBounceDriver {
    fn new(worktree: PathBuf, bounce_marker: RuleId) -> Self {
        Self {
            worktree,
            tasks: Mutex::new(vec![]),
            bounce_marker: bounce_marker.0,
        }
    }

    fn scaffold_manifest(&self) {
        let manifest = self.worktree.join("Cargo.toml");
        fs::write(manifest, CARGO_TOML).expect("write Cargo.toml");
    }

    fn write_main(&self, body: &str) {
        let main = self.worktree.join("src").join("main.rs");
        if let Some(parent) = main.parent() {
            fs::create_dir_all(parent).expect("create src dir");
        }
        fs::write(main, body).expect("write main.rs");
    }
}

#[async_trait::async_trait]
impl AgentDriver for ReviseOnBounceDriver {
    async fn run(&self, role: &Role, task: &str) -> anyhow::Result<AgentOutcome> {
        self.tasks.lock().unwrap().push(task.to_string());

        // The crate manifest is part of "the agent's output" on the first pass.
        self.scaffold_manifest();

        let is_bounce = task.contains(&self.bounce_marker);
        if is_bounce {
            self.write_main(CLEAN_MAIN);
        } else {
            self.write_main(BADLY_FORMATTED_MAIN);
        }

        Ok(AgentOutcome {
            session_id: format!("fleet-{}", role.name.to_lowercase()),
            result: if is_bounce {
                "revised".to_string()
            } else {
                "initial".to_string()
            },
            cost_usd: Some(0.0),
            denials: vec![],
        })
    }
}

// ─── fake stage 2: clean helper, never bounces ───────────────────────────────

/// Stage-2 agent: overwrites `src/main.rs` on the SHARED worktree with a clean,
/// clippy-clean helper. It introduces no violation, so the fleet must run it
/// exactly once. It also proves the shared-worktree channel: it reads the
/// substrate stage 1 left (the existing crate) and lands its own output beside
/// it.
struct CleanTesterDriver {
    worktree: PathBuf,
    runs: Mutex<usize>,
}

impl CleanTesterDriver {
    fn new(worktree: PathBuf) -> Self {
        Self {
            worktree,
            runs: Mutex::new(0),
        }
    }
}

#[async_trait::async_trait]
impl AgentDriver for CleanTesterDriver {
    async fn run(&self, role: &Role, _task: &str) -> anyhow::Result<AgentOutcome> {
        *self.runs.lock().unwrap() += 1;

        // Prove the shared-worktree channel: stage 1's manifest must be present.
        let manifest = self.worktree.join("Cargo.toml");
        assert!(
            manifest.exists(),
            "stage 2 must see stage 1's output on the shared worktree"
        );

        let main = self.worktree.join("src").join("main.rs");
        fs::write(main, TESTER_HELPER).expect("write helper main.rs");

        Ok(AgentOutcome {
            session_id: format!("fleet-{}", role.name.to_lowercase()),
            result: "tested".to_string(),
            cost_usd: Some(0.0),
            denials: vec![],
        })
    }
}

fn role(name: &str) -> Role {
    Role {
        name: name.to_string(),
        rule_subset: vec![fmt_rule()],
        allowed_paths: vec![".".to_string()],
    }
}

#[tokio::test]
async fn fleet_bounces_real_layer2_violation_in_a_stage_and_later_stages_still_run() {
    let wt = TempWorktree::new("e2e").expect("create temp worktree");

    // REAL layer-2 gate shared across the fleet: fmt + clippy.
    let checks = RustCheckRunner::new();

    // Stage 1 driver: dirty first, clean on bounce. Stage 2 driver: clean once.
    let implementer = ReviseOnBounceDriver::new(wt.path().to_path_buf(), fmt_rule());
    let tester = CleanTesterDriver::new(wt.path().to_path_buf());

    let fleet = FleetCoordinator::new(&checks, wt.path());
    let stages = vec![
        FleetStage::new(role("Implementer"), "implement the feature", &implementer),
        FleetStage::new(role("Tester"), "add a tested helper", &tester),
    ];

    let report = fleet
        .run(&stages)
        .await
        .expect("fleet run should complete without a seam error");

    // ── Stage 1: the REAL layer-2 gate caught the fmt violation DURING the
    //    fleet, the stage bounced exactly once, and ended clean. ──────────────
    let stage1 = &report.stages[0];
    assert_eq!(stage1.role_name, "Implementer");
    assert_eq!(
        stage1.report.initial_violations,
        vec![fmt_rule()],
        "stage 1's first REAL check must report exactly RUST-FMT, got {:?}",
        stage1.report.initial_violations
    );
    assert!(
        stage1.report.bounced,
        "stage 1 must bounce once on the real fmt violation"
    );
    assert!(
        stage1.report.revised_outcome.is_some(),
        "stage 1's bounce must have produced a revise outcome"
    );
    assert!(
        stage1.report.final_violations.is_empty(),
        "stage 1 must be clean after the fix, got {:?}",
        stage1.report.final_violations
    );
    assert!(stage1.is_clean(), "stage 1 must end clean");

    // The bounce cited the violated rule id verbatim, and the agent ran twice.
    let s1_tasks = implementer.tasks.lock().unwrap();
    assert_eq!(
        s1_tasks.len(),
        2,
        "stage 1 agent must run twice (initial + one revise)"
    );
    assert!(
        s1_tasks[1].contains(&fmt_rule().0),
        "the bounce task must cite RUST-FMT, got: {:?}",
        s1_tasks[1]
    );
    assert!(
        s1_tasks[1].contains("REVISION REQUIRED"),
        "the bounce task must carry the revision instruction"
    );
    drop(s1_tasks);

    // ── Stage 2: STILL RAN despite stage 1's earlier violation, ran exactly
    //    once (no bounce of its own), and is clean. ───────────────────────────
    let stage2 = &report.stages[1];
    assert_eq!(stage2.role_name, "Tester");
    assert_eq!(
        *tester.runs.lock().unwrap(),
        1,
        "stage 2 must run exactly once: it never introduced a violation"
    );
    assert!(
        stage2.report.initial_violations.is_empty(),
        "stage 2's first real check must be clean, got {:?}",
        stage2.report.initial_violations
    );
    assert!(!stage2.report.bounced, "stage 2 must not bounce");
    assert!(stage2.is_clean(), "stage 2 must end clean");

    // ── Aggregate FleetReport reflects all of it. ────────────────────────────
    assert_eq!(report.stages.len(), 2, "both stages must be reported");
    assert!(report.is_clean(), "the whole fleet must end clean");
    assert_eq!(
        report.total_bounces(),
        1,
        "exactly one bounce across the fleet (stage 1 only)"
    );

    // Sanity: the on-disk file really is the tester's clean output now (stage 2
    // ran last against the shared worktree).
    let final_main =
        fs::read_to_string(wt.path().join("src").join("main.rs")).expect("read final main.rs");
    assert_eq!(final_main, TESTER_HELPER);
}
