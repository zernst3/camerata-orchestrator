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
    let cmd = std::env::args().nth(1).unwrap_or_default();
    match cmd.as_str() {
        "acceptance" => run_acceptance_cmd().await,
        "live-demo" => camerata::live_demo::run_live_demo().await,
        "build-demo" => camerata::build_demo::run_build_demo().await,
        "po-demo" => camerata::po_demo::run_po_demo().await,
        "worktracker-demo" => camerata::worktracker_demo::run_worktracker_demo().await,
        "" | "help" | "--help" | "-h" => {
            println!("camerata orchestrator");
            println!("usage:");
            println!(
                "  camerata acceptance        run the in-process planted-violation acceptance scenario"
            );
            println!("  camerata live-demo         spawn a REAL claude -p twice; prove gateway deny + allow live");
            println!("  camerata build-demo        run a LIVE governed FLEET (2 agents) that writes + builds + tests a crate");
            println!("  camerata po-demo           run PO-MODE end to end: intake form -> lead engineer -> governed fleet -> cargo");
            println!("  camerata worktracker-demo  Tier-1 enterprise flow: PO on their board, governed, provenance written back");
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
