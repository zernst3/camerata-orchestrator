//! camerata orchestrator binary.
//!
//! Subcommands:
//! - `acceptance` — run the in-process, no-network planted-violation acceptance
//!   scenario and print the gate's verdict. Exit 0 if the gate denied the
//!   planted violation and allowed the control write; exit 1 otherwise.

use camerata::acceptance::{run_acceptance, AcceptanceResult};
use camerata_core::Decision;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Auto-load the gitignored .env (so the live commands pick up the token).
    let _ = dotenvy::dotenv();
    let cmd = std::env::args().nth(1).unwrap_or_default();
    match cmd.as_str() {
        "acceptance" => run_acceptance_cmd().await,
        "gate-probe" => run_gate_probe_cmd().await,
        "live-demo" => camerata::live_demo::run_live_demo().await,
        "build-demo" => camerata::build_demo::run_build_demo().await,
        "po-demo" => camerata::po_demo::run_po_demo().await,
        "worktracker-demo" => camerata::worktracker_demo::run_worktracker_demo().await,
        "maintenance-demo" => camerata::maintenance_demo::run_maintenance_demo().await,
        "deploy-demo" => camerata::deploy_demo::run_deploy_demo().await,
        "worktracker-live" => camerata::worktracker_live::run_worktracker_live().await,
        "projects-live" => camerata::projects_live::run_projects_live().await,
        "" | "help" | "--help" | "-h" => {
            println!("camerata orchestrator");
            println!("usage:");
            println!(
                "  camerata acceptance        run the in-process planted-violation acceptance scenario"
            );
            println!(
                "  camerata gate-probe        #14 end-to-end gate-loop GO/NO-GO on a story (both layers, no claude)"
            );
            println!("  camerata live-demo         spawn a REAL claude -p twice; prove gateway deny + allow live");
            println!("  camerata build-demo        run a LIVE governed FLEET (2 agents) that writes + builds + tests a crate");
            println!("  camerata po-demo           run PO-MODE end to end: intake form -> lead engineer -> governed fleet -> cargo");
            println!("  camerata worktracker-demo  Tier-1 enterprise flow: PO on their board, governed, provenance written back");
            println!("  camerata maintenance-demo  Tier-2 standing ops agent: scan, approve, key rotation");
            println!("  camerata deploy-demo       BYO-infra publish step: gate, local deploy, Azure plan");
            println!("  camerata worktracker-live  LIVE GitHub round-trip (needs CAMERATA_GITHUB_* env; see docs/GITHUB_SETUP.md)");
            println!("  camerata projects-live     LIVE GitHub Projects v2 board listing across repos (needs CAMERATA_GITHUB_PROJECT_* env)");
            Ok(())
        }
        other => {
            eprintln!("unknown subcommand: {other}");
            std::process::exit(2);
        }
    }
}

async fn run_acceptance_cmd() -> anyhow::Result<()> {
    let result: AcceptanceResult = run_acceptance().await?;

    println!("== Camerata planted-violation acceptance run ==");
    println!(
        "agent session (fake/echo driver): {}",
        result.agent_session_id
    );
    println!("role allowedTools: {}", result.allowed_tools.join(" "));

    match &result.planted_violation_decision {
        Decision::Deny { rule, reason } => {
            println!("planted forbidden write -> DENIED [{}]: {}", rule.0, reason);
        }
        Decision::Allow => {
            println!("planted forbidden write -> ALLOWED  (UNEXPECTED — gate not wired)");
        }
    }
    match &result.planted_path_escape_decision {
        Decision::Deny { rule, reason } => {
            println!("planted `..` traversal  -> DENIED [{}]: {}", rule.0, reason);
        }
        Decision::Allow => {
            println!(
                "planted `..` traversal  -> ALLOWED  (UNEXPECTED — path-escape rule not wired)"
            );
        }
    }
    match &result.planted_secret_decision {
        Decision::Deny { rule, reason } => {
            println!("planted secret literal  -> DENIED [{}]: {}", rule.0, reason);
        }
        Decision::Allow => {
            println!("planted secret literal  -> ALLOWED  (UNEXPECTED — secrets rule not wired)");
        }
    }
    match &result.clean_control_decision {
        Decision::Allow => println!("clean control write     -> ALLOWED  (expected)"),
        Decision::Deny { rule, reason } => {
            println!(
                "clean control write     -> DENIED [{}]: {}  (UNEXPECTED)",
                rule.0, reason
            );
        }
    }

    if result.passed() {
        println!("ACCEPTANCE: PASS");
        Ok(())
    } else {
        eprintln!("ACCEPTANCE: FAIL");
        std::process::exit(1);
    }
}

/// #14 — the end-to-end gate-loop GO/NO-GO on a story. Runs BOTH gate layers in-process (no
/// model) and prints a single verdict; exit 1 on NO-GO.
async fn run_gate_probe_cmd() -> anyhow::Result<()> {
    use camerata::gate_probe::run_gate_probe;
    let r = run_gate_probe().await?;

    println!("== Camerata gate-loop probe (#14) — end-to-end, no claude ==");
    println!("story: {}", r.story);
    println!();
    println!(
        "LAYER 1 — deny-before-execute (real gateway): {}/{} floor rules enforced",
        r.layer1_denied_count(),
        r.layer1_total()
    );
    for c in &r.layer1 {
        let verdict = if c.denied {
            "DENIED"
        } else {
            "ALLOWED (NO-GO)"
        };
        println!("  {:<44} -> {verdict}  {}", c.label, c.detail);
    }
    println!(
        "  clean control write{}-> {}",
        " ".repeat(26),
        if r.layer1_clean_allowed {
            "ALLOWED (expected)"
        } else {
            "DENIED (NO-GO — deny-all)"
        }
    );
    println!();
    println!("LAYER 2 — bounce-and-revise (real coordinator):");
    println!(
        "  agent passes: {} (initial + revise), bounced: {}, revise clean: {}",
        r.agent_passes, r.layer2_bounced, r.layer2_clean
    );
    println!();

    if r.go() {
        println!(
            "GATE PROBE: GO  — the loop denies before execute, bounces on violation, and resolves."
        );
        Ok(())
    } else {
        eprintln!("GATE PROBE: NO-GO");
        std::process::exit(1);
    }
}
