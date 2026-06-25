//! `build-demo` — the LIVE governed FLEET run.
//!
//! Where `live-demo` proves the gateway's deny/allow on ONE governed agent, this
//! proves a governed *fleet*: the [`camerata_core::FleetCoordinator`] sequences
//! TWO real `claude -p` agents against ONE shared worktree, each locked to the
//! Rust gateway's `gated_write` tool, and we then BUILD + TEST the code the fleet
//! produced. The milestone is the FLEET + GATE + COORDINATOR mechanics producing
//! real files through the gate across multiple agents.
//!
//! Pipeline:
//!   1. Scaffold a fresh temp cargo *library* crate (the shared worktree).
//!   2. Build a governed role from the real camerata-ai corpus, ensuring the
//!      gateway's enforced security rules + GOV-1 ride along in the per-session
//!      rule-subset (reusing the same blend the live single-agent demo uses).
//!   3. Stage A — "implementer": a governed agent writes a small, well-specified
//!      Rust function into `src/lib.rs` via the gated write tool.
//!   4. Stage B — "tester": a governed agent READS the implementer's `src/lib.rs`
//!      (the shared worktree IS the inter-agent channel) and rewrites it to add a
//!      `#[test]` for that function, also via the gated write tool.
//!   5. Run `cargo build` + `cargo test` on the produced crate.
//!   6. Print `FLEET-DEMO: PASS` (compiled AND tests passed) or `PARTIAL`
//!      (agents ran + wrote through the gate, but the generated Rust did not
//!      compile / pass first try — honest signal about engine quality).
//!
//! Honesty note: we do NOT hand-write the agents' Rust. The function spec and the
//! test instruction are precise (so first-try success is plausible), but the
//! actual code is whatever the two live agents emit through the gate. If it does
//! not compile or pass, that is reported as PARTIAL, not faked into PASS.

use std::time::Instant;

use camerata_agent::{prepare_session, GATED_WRITE_TOOL};
use camerata_core::{FleetCoordinator, FleetStage};

use crate::fleet_support::{
    governed_role, locate_gateway_bin, run_cargo, scaffold_crate, tail_lines, NoopChecks,
    DEFAULT_CORPUS_PATH, FLEET_DOMAINS,
};

/// Run the full live governed FLEET demo and report PASS / PARTIAL.
pub async fn run_build_demo() -> anyhow::Result<()> {
    let gateway_bin = locate_gateway_bin()?;
    eprintln!("[build-demo] gateway binary: {}", gateway_bin.display());

    // ── Roles (one per fleet stage), both governed from the real corpus ──────
    let implementer_role = governed_role("Implementer").await?;
    let tester_role = governed_role("Tester").await?;

    let impl_ids: Vec<&str> = implementer_role
        .rule_subset
        .iter()
        .map(|r| r.0.as_str())
        .collect();

    // ── Shared worktree: a fresh temp cargo library crate ────────────────────
    let root = std::env::temp_dir().join(format!("camerata-fleet-{}", std::process::id()));
    let worktree = root.join("crate");
    let crate_name = "fleet_demo_crate";
    // Clean any stale dir from a prior run with the same pid (unlikely but safe).
    let _ = std::fs::remove_dir_all(&root);
    scaffold_crate(&worktree, crate_name)?;
    let _ = std::fs::create_dir_all("/tmp/camerata-verify");

    let lib_path = worktree.join("src").join("lib.rs");

    println!("== Camerata LIVE governed FLEET run ==");
    println!("governed tool (agents are locked to this): {GATED_WRITE_TOOL}");
    println!("gateway binary: {}", gateway_bin.display());
    println!("shared worktree (cargo lib crate): {}", worktree.display());
    println!("corpus: {DEFAULT_CORPUS_PATH}");
    println!(
        "implementer role: {} ({} rules over domains {:?})",
        implementer_role.name,
        implementer_role.rule_subset.len(),
        FLEET_DOMAINS,
    );
    println!("delivered rule-subset: {}", impl_ids.join(", "));
    println!();

    // ── Per-session governed drivers (each agent its own session) ────────────
    // Each session gets its own rules.json + mcp-config (per-session delivery),
    // and the driver is bound to the shared worktree (cwd + --add-dir scope).
    // prepare_session creates its own TempDir per session (ARCH-RESOURCE-LIFECYCLE-1).
    let impl_spawn = prepare_session(&gateway_bin, &implementer_role, None, &[])?;
    let impl_driver = impl_spawn.driver.with_worktree(&worktree);

    let tester_spawn = prepare_session(&gateway_bin, &tester_role, None, &[])?;
    let tester_driver = tester_spawn.driver.with_worktree(&worktree);

    eprintln!(
        "[build-demo] implementer session rules={} mcp={}",
        impl_spawn.rules_file.display(),
        impl_spawn.mcp_config.display()
    );
    eprintln!(
        "[build-demo] tester session rules={} mcp={}",
        tester_spawn.rules_file.display(),
        tester_spawn.mcp_config.display()
    );

    // ── The two stage tasks ──────────────────────────────────────────────────
    // The function is WELL-SPECIFIED so first-try compilation is plausible, but
    // the agents write the actual code. We do not hand-write it.
    let impl_task = format!(
        "You are the IMPLEMENTER agent in a two-agent fleet. Your ONLY way to \
         write files is the `{tool}` tool. Use it exactly once.\n\n\
         Write a complete Rust source file to the absolute path {lib} (OVERWRITE \
         whatever is there). The file must contain exactly one public function \
         with this signature and behavior:\n\n\
         /// Returns the sum of all even integers in `nums`.\n\
         pub fn sum_even(nums: &[i64]) -> i64\n\n\
         It must sum the even values of the slice (an empty slice returns 0). Use \
         idiomatic Rust (iterator + filter + sum). Do NOT write any tests, any \
         `mod tests`, or any `#[test]` — the tester agent adds those. Do NOT use \
         `unsafe`. The file must be a valid Rust library module that compiles on \
         its own. Call `{tool}` with the path {lib} and the full file content, \
         then report the tool's result.",
        tool = GATED_WRITE_TOOL,
        lib = lib_path.display(),
    );

    let tester_task = format!(
        "You are the TESTER agent in a two-agent fleet. The IMPLEMENTER agent has \
         already written a Rust library to {lib} in this same crate. First READ \
         {lib} to see the existing `pub fn sum_even(nums: &[i64]) -> i64` the \
         implementer wrote. Your ONLY way to write files is the `{tool}` tool; \
         use it exactly once.\n\n\
         Rewrite {lib} so it contains BOTH (1) the implementer's existing \
         `sum_even` function UNCHANGED, and (2) a new test module appended at the \
         end:\n\n\
         #[cfg(test)]\n\
         mod tests {{\n\
         \x20   use super::*;\n\
         \x20   #[test]\n\
         \x20   fn test_sum_even() {{\n\
         \x20       assert_eq!(sum_even(&[1, 2, 3, 4, 5, 6]), 12);\n\
         \x20       assert_eq!(sum_even(&[]), 0);\n\
         \x20       assert_eq!(sum_even(&[1, 3, 5]), 0);\n\
         \x20   }}\n\
         }}\n\n\
         Preserve the original function body exactly as the implementer wrote it; \
         only APPEND the test module. Call `{tool}` with the path {lib} and the \
         FULL new file content (original function + test module), then report the \
         tool's result.",
        tool = GATED_WRITE_TOOL,
        lib = lib_path.display(),
    );

    // ── Run the governed fleet ───────────────────────────────────────────────
    let checks = NoopChecks;
    let fleet = FleetCoordinator::new(&checks, &worktree);
    let stages = vec![
        FleetStage::new(implementer_role.clone(), impl_task, &impl_driver),
        FleetStage::new(tester_role.clone(), tester_task, &tester_driver),
    ];

    println!("Running governed fleet: 2 live `claude -p` agents through the gate...");
    let t0 = Instant::now();
    let report = fleet.run(&stages).await?;
    let fleet_wall = t0.elapsed();

    // ── Report each stage ────────────────────────────────────────────────────
    let mut both_agents_ran = true;
    let mut wrote_through_gate = lib_path.exists();
    for (i, stage) in report.stages.iter().enumerate() {
        let r = &stage.report;
        println!("── stage {}: {} ──", i + 1, stage.role_name);
        println!("  initial session_id: {}", r.initial_outcome.session_id);
        if let Some(cost) = r.initial_outcome.cost_usd {
            println!("  initial cost_usd:   {cost:.6}");
        }
        println!(
            "  agent said:         {}",
            r.initial_outcome.result.replace('\n', " ")
        );
        if r.initial_outcome.session_id.is_empty() {
            both_agents_ran = false;
        }
        println!();
    }
    println!("fleet wall: {:.2}s", fleet_wall.as_secs_f64());
    println!();

    // The filesystem is the source of truth that the gate actually wrote: after
    // the fleet, src/lib.rs must contain BOTH the function and a #[test].
    let produced = std::fs::read_to_string(&lib_path).unwrap_or_default();
    let has_fn = produced.contains("sum_even");
    let has_test = produced.contains("#[test]");
    wrote_through_gate = wrote_through_gate && has_fn && has_test;

    println!("── produced src/lib.rs ──");
    println!("  path:            {}", lib_path.display());
    println!("  contains fn:     {has_fn} (sum_even)");
    println!("  contains #[test]:{has_test}");
    println!("  bytes:           {}", produced.len());
    println!();

    // ── Build + test the produced crate ──────────────────────────────────────
    println!("Running `cargo build` on the fleet-produced crate...");
    let build = run_cargo(&worktree, "build").await?;
    println!("  cargo build success: {}", build.success);
    if !build.success {
        println!("  --- cargo build stderr (tail) ---");
        for line in tail_lines(&build.stderr, 20) {
            println!("  {line}");
        }
    }
    println!();

    // Only run tests if the build succeeded (cargo test would re-report build
    // errors otherwise, which is noise).
    let test = if build.success {
        println!("Running `cargo test` on the fleet-produced crate...");
        let test = run_cargo(&worktree, "test").await?;
        println!("  cargo test success: {}", test.success);
        println!("  --- cargo test stdout (tail) ---");
        for line in tail_lines(&test.stdout, 12) {
            println!("  {line}");
        }
        if !test.success {
            println!("  --- cargo test stderr (tail) ---");
            for line in tail_lines(&test.stderr, 12) {
                println!("  {line}");
            }
        }
        Some(test)
    } else {
        println!("Skipping `cargo test` (build failed).");
        None
    };
    println!();

    // ── Verdict ──────────────────────────────────────────────────────────────
    let compiled = build.success;
    let tests_passed = test.as_ref().map(|t| t.success).unwrap_or(false);

    println!("── FLEET SUMMARY ──");
    println!(
        "  multiple governed agents ran live:        {}",
        if both_agents_ran { "YES" } else { "NO" }
    );
    println!(
        "  produced code through the gate (fn+test): {}",
        if wrote_through_gate { "YES" } else { "NO" }
    );
    println!(
        "  produced crate compiled:                  {}",
        if compiled { "YES" } else { "NO" }
    );
    println!(
        "  produced crate tests passed:              {}",
        if tests_passed { "YES" } else { "NO" }
    );
    println!();

    if both_agents_ran && wrote_through_gate && compiled && tests_passed {
        println!(
            "FLEET-DEMO: PASS (2 live governed claude agents wrote a compiling, \
             passing crate through the Rust gate)"
        );
        Ok(())
    } else if both_agents_ran && wrote_through_gate {
        println!(
            "FLEET-DEMO: PARTIAL (the governed fleet + gate + coordinator mechanics \
             ran and produced real files through the gate across 2 agents, but the \
             generated Rust did not {} first try — honest engine-quality signal, NOT \
             a harness failure)",
            if !compiled { "compile" } else { "pass tests" }
        );
        Ok(())
    } else {
        eprintln!(
            "FLEET-DEMO: PARTIAL (harness ran but a governed agent did not produce \
             the expected gated write — see stage output above)"
        );
        // Not a hard process failure: the milestone is the mechanics, and the
        // output above is the honest record. Exit non-zero so CI/automation can
        // distinguish this from a full PASS.
        std::process::exit(1);
    }
}
