//! End-to-end proof of LAYER-2 bounce-and-revise with a REAL check.
//!
//! The unit tests in `src/coordinator.rs` drive the bounce loop with a
//! *scripted* `CheckRunner`. This integration test instead wires the
//! [`Coordinator`] to the REAL [`camerata_checks::FmtCheckRunner`], which shells
//! out to `cargo fmt --check` against an on-disk temp worktree.
//!
//! The fake [`AgentDriver`] models an agent that:
//!   1. on its first run, writes a BADLY-FORMATTED `src/main.rs`, then
//!   2. on the bounce run (recognised by the `RUST-FMT` rule id the coordinator
//!      appends to the task), writes the fmt-CLEAN version.
//!
//! We then assert the full layer-2 contract against the real fmt gate:
//!   - the initial real check finds the `RUST-FMT` violation,
//!   - the coordinator bounces exactly once (re-running the driver with the
//!     violated rule id cited in the task),
//!   - the second real check is clean, and
//!   - `RunReport::final_violations` is empty.
//!
//! That chain is the whole point of the orchestrator's layer-2: a structural
//! defect is caught by a real tool, fed back to the agent, and re-verified.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use camerata_checks::{fmt_rule, FmtCheckRunner};
use camerata_core::{AgentDriver, AgentOutcome, Coordinator, Role, RuleId};

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
        path.push(format!("camerata-core-{tag}-{}-{stamp}", std::process::id()));
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

// ─── fake agent: dirty first, clean on bounce ────────────────────────────────

/// Source bodies the fake agent writes. The badly-formatted one still parses
/// (rustfmt needs valid syntax, not a successful compile); the clean one is
/// rustfmt-canonical.
const BADLY_FORMATTED_MAIN: &str = "fn   main( ){let x=1;println!(\"{}\",x );}\n";
const CLEAN_MAIN: &str = "fn main() {\n    let x = 1;\n    println!(\"{}\", x);\n}\n";

const CARGO_TOML: &str =
    "[package]\nname = \"e2e-probe\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\n";

/// An agent driver that, against a shared worktree, writes the badly-formatted
/// file on its first invocation and the clean file once it sees the bounced
/// rule id in the task. It records every task it received so the test can prove
/// the bounce-back cited the violated rule.
struct ReviseOnBounceDriver {
    worktree: PathBuf,
    tasks: Mutex<Vec<String>>,
    /// The rule id whose presence in the task signals "this is the bounce pass".
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
            // Saw the violated rule id appended to the task — fix the formatting.
            self.write_main(CLEAN_MAIN);
        } else {
            // First pass: emit the defective, unformatted file.
            self.write_main(BADLY_FORMATTED_MAIN);
        }

        Ok(AgentOutcome {
            session_id: format!("e2e-{}", role.name.to_lowercase()),
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

fn backend_role() -> Role {
    Role {
        name: "Backend".to_string(),
        rule_subset: vec![fmt_rule()],
        allowed_paths: vec![".".to_string()],
    }
}

#[tokio::test]
async fn coordinator_bounces_real_fmt_violation_and_resolves() {
    let wt = TempWorktree::new("e2e").expect("create temp worktree");

    // REAL layer-2 gate: shells out to `cargo fmt --check`.
    let checks = FmtCheckRunner;
    // Fake agent that writes dirty first, clean on bounce.
    let driver = ReviseOnBounceDriver::new(wt.path().to_path_buf(), fmt_rule());

    let coord = Coordinator::new(&driver, &checks, wt.path());
    let report = coord
        .run(&backend_role(), "implement the feature")
        .await
        .expect("coordinator run should complete without a seam error");

    // 1. The initial REAL check found the fmt violation.
    assert_eq!(
        report.initial_violations,
        vec![fmt_rule()],
        "the first real cargo-fmt check must report RUST-FMT"
    );

    // 2. The coordinator performed exactly one bounce-and-revise pass.
    assert!(report.bounced, "a violation must trigger the bounce pass");
    assert!(
        report.revised_outcome.is_some(),
        "the revise pass must have produced an outcome"
    );

    // 3. The agent ran exactly twice, and the bounce task cited the rule id.
    let tasks = driver.tasks.lock().unwrap();
    assert_eq!(
        tasks.len(),
        2,
        "the agent must run twice: initial + one revise"
    );
    assert!(
        tasks[1].contains(&fmt_rule().0),
        "the bounce task must cite the violated rule id, got: {:?}",
        tasks[1]
    );
    assert!(
        tasks[1].contains("REVISION REQUIRED"),
        "the bounce task must carry the revision instruction"
    );
    drop(tasks);

    // 4. The second REAL check is clean — the residual set is empty. This is the
    //    end-to-end proof of layer-2 bounce-and-revise against a real tool.
    assert!(
        report.is_clean(),
        "after the agent fixed the formatting, the real check must pass"
    );
    assert!(
        report.final_violations.is_empty(),
        "final_violations must be empty, got {:?}",
        report.final_violations
    );

    // Sanity: the on-disk file really is rustfmt-clean now.
    let final_main =
        fs::read_to_string(wt.path().join("src").join("main.rs")).expect("read final main.rs");
    assert_eq!(final_main, CLEAN_MAIN);
}
