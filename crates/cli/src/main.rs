//! camerata orchestrator binary.
//!
//! Two families of subcommand:
//! - The original in-process demo/eval harness (`acceptance`, `gate-probe`, `eval`,
//!   `*-demo`, `*-live`) — each calls a backend crate directly in-process (no HTTP).
//! - The HTTP-adapter subcommands (`stories`, `run`, `uows`, `assign`, `start-run`;
//!   Phase F, GAP-7) — each is a thin delegation to `camerata-client`'s typed HTTP
//!   client, a real round trip to the running BFF's `/api/*` routes (`src/http_cmd.rs`
//!   holds the handlers). This is the SAME client the MCP adapter (`crates/mcp`) and
//!   the Dioxus cockpit (`crates/ui`) use — the CLI is just another adapter over the
//!   one capability contract.
//!
//! `acceptance` in particular: run the in-process, no-network planted-violation
//! acceptance scenario and print the gate's verdict. Exit 0 if the gate denied the
//! planted violation and allowed the control write; exit 1 otherwise.

use camerata::acceptance::{run_acceptance, AcceptanceResult};
use camerata_client::{Client, ClientError};
use camerata_core::Decision;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "camerata", about = "camerata orchestrator", long_about = None)]
struct Cli {
    /// Override the BFF base URL for the HTTP-adapter subcommands (stories, run, uows,
    /// assign, start-run). Falls back to `CAMERATA_BFF_URL`, then the embedded BFF's
    /// default `http://127.0.0.1:8787`. Ignored by the in-process demo/eval commands.
    #[arg(long, global = true, value_name = "URL")]
    bff_url: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the in-process planted-violation acceptance scenario.
    Acceptance,
    /// #14 end-to-end gate-loop GO/NO-GO on a story (both layers, no claude).
    GateProbe,
    /// #11 precision/recall eval of the deterministic audit floor (no model, no network).
    Eval,
    /// Spawn a REAL claude -p twice; prove gateway deny + allow live.
    LiveDemo,
    /// Run a LIVE governed FLEET (2 agents) that writes + builds + tests a crate.
    BuildDemo,
    /// Run PO-MODE end to end: intake form -> lead engineer -> governed fleet -> cargo.
    PoDemo,
    /// Tier-1 enterprise flow: PO on their board, governed, provenance written back.
    WorktrackerDemo,
    /// Tier-2 standing ops agent: scan, approve, key rotation.
    MaintenanceDemo,
    /// BYO-infra publish step: gate, local deploy, Azure plan.
    DeployDemo,
    /// LIVE GitHub round-trip (needs CAMERATA_GITHUB_* env; see docs/GITHUB_SETUP.md).
    WorktrackerLive,
    /// LIVE GitHub Projects v2 board listing across repos (needs CAMERATA_GITHUB_PROJECT_* env).
    ProjectsLive,

    /// List canonical stories for the active project via the BFF (GET /api/stories).
    Stories,
    /// Get the current state of a governed run by id via the BFF (GET /api/runs/:id).
    Run {
        /// The run id, e.g. as returned by `start-run`.
        run_id: String,
    },
    /// List every Unit of Work for the active project via the BFF (GET /api/uows).
    Uows,
    /// Assign a tracker work item to a login via the BFF (POST /api/workitems/assign).
    Assign {
        /// Stable cross-provider work-item id, e.g. "github:owner/repo#123".
        #[arg(long = "work-item")]
        work_item: String,
        /// The tracker login to assign the item to.
        #[arg(long)]
        assignee: String,
    },
    /// Start a governed run for a story via the BFF (POST /api/stories/:id/run).
    StartRun {
        /// The canonical story id to run, e.g. "owner/repo#123" (as listed by `stories`).
        story_id: String,
        /// Optional single-model override for the run.
        #[arg(long)]
        model: Option<String>,
        /// Skip the layer-2 post-task check gate.
        #[arg(long)]
        skip_layer2: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Auto-load the gitignored .env (so the live commands pick up the token).
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    let bff_url = cli.bff_url;

    match cli.command {
        Command::Acceptance => run_acceptance_cmd().await,
        Command::GateProbe => run_gate_probe_cmd().await,
        Command::Eval => camerata::eval_cmd::run_eval_cmd(),
        Command::LiveDemo => camerata::live_demo::run_live_demo().await,
        Command::BuildDemo => camerata::build_demo::run_build_demo().await,
        Command::PoDemo => camerata::po_demo::run_po_demo().await,
        Command::WorktrackerDemo => camerata::worktracker_demo::run_worktracker_demo().await,
        Command::MaintenanceDemo => camerata::maintenance_demo::run_maintenance_demo().await,
        Command::DeployDemo => camerata::deploy_demo::run_deploy_demo().await,
        Command::WorktrackerLive => camerata::worktracker_live::run_worktracker_live().await,
        Command::ProjectsLive => camerata::projects_live::run_projects_live().await,

        Command::Stories => {
            let client = make_client(bff_url);
            print_result(camerata::http_cmd::handle_stories(&client).await)
        }
        Command::Run { run_id } => {
            let client = make_client(bff_url);
            print_result(camerata::http_cmd::handle_run(&client, &run_id).await)
        }
        Command::Uows => {
            let client = make_client(bff_url);
            print_result(camerata::http_cmd::handle_uows(&client).await)
        }
        Command::Assign {
            work_item,
            assignee,
        } => {
            let client = make_client(bff_url);
            print_result(camerata::http_cmd::handle_assign(&client, work_item, assignee).await)
        }
        Command::StartRun {
            story_id,
            model,
            skip_layer2,
        } => {
            let client = make_client(bff_url);
            print_result(
                camerata::http_cmd::handle_start_run(&client, &story_id, model, skip_layer2)
                    .await,
            )
        }
    }
}

/// Build the `camerata-client::Client` the HTTP-adapter subcommands share: `--bff-url`
/// wins when set, else [`Client::new`] resolves `CAMERATA_BFF_URL` / the embedded
/// default (see `camerata_client::bff_base`).
fn make_client(bff_url: Option<String>) -> Client {
    match bff_url {
        Some(url) => Client::with_base(url),
        None => Client::new(),
    }
}

/// Shape an HTTP-adapter handler's result the same way for every subcommand: pretty
/// JSON to stdout on success, a clear diagnostic to stderr + non-zero exit on a
/// [`ClientError`] — never a panic.
fn print_result(result: Result<String, ClientError>) -> anyhow::Result<()> {
    match result {
        Ok(json) => {
            println!("{json}");
            Ok(())
        }
        Err(e) => {
            eprintln!("camerata: {e}");
            std::process::exit(1);
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

#[cfg(test)]
mod cli_parse_tests {
    use super::*;

    /// Every preserved demo/eval subcommand must still parse with no extra args
    /// (standard clap-derive `try_parse_from` pattern).
    #[test]
    fn preserved_demo_subcommands_parse() {
        for name in [
            "acceptance",
            "gate-probe",
            "eval",
            "live-demo",
            "build-demo",
            "po-demo",
            "worktracker-demo",
            "maintenance-demo",
            "deploy-demo",
            "worktracker-live",
            "projects-live",
        ] {
            let cli = Cli::try_parse_from(["camerata", name])
                .unwrap_or_else(|e| panic!("`{name}` must parse: {e}"));
            assert!(cli.bff_url.is_none());
        }
    }

    #[test]
    fn stories_and_uows_take_no_args() {
        assert!(Cli::try_parse_from(["camerata", "stories"]).is_ok());
        assert!(Cli::try_parse_from(["camerata", "uows"]).is_ok());
    }

    #[test]
    fn run_requires_a_run_id_positional() {
        let cli = Cli::try_parse_from(["camerata", "run", "run-42"]).expect("must parse");
        match cli.command {
            Command::Run { run_id } => assert_eq!(run_id, "run-42"),
            other => panic!("expected Command::Run, got a different variant: {other:?}"),
        }
        assert!(Cli::try_parse_from(["camerata", "run"]).is_err());
    }

    #[test]
    fn assign_parses_its_two_required_flags() {
        let cli = Cli::try_parse_from([
            "camerata",
            "assign",
            "--work-item",
            "github:owner/repo#1",
            "--assignee",
            "octocat",
        ])
        .expect("must parse");
        match cli.command {
            Command::Assign {
                work_item,
                assignee,
            } => {
                assert_eq!(work_item, "github:owner/repo#1");
                assert_eq!(assignee, "octocat");
            }
            other => panic!("expected Command::Assign, got a different variant: {other:?}"),
        }
        // Missing `--assignee` must fail to parse.
        assert!(Cli::try_parse_from([
            "camerata",
            "assign",
            "--work-item",
            "github:owner/repo#1"
        ])
        .is_err());
    }

    #[test]
    fn start_run_parses_positional_and_optional_flags() {
        let cli = Cli::try_parse_from([
            "camerata",
            "start-run",
            "owner/repo#1",
            "--model",
            "claude-sonnet-4-6",
            "--skip-layer2",
        ])
        .expect("must parse");
        match cli.command {
            Command::StartRun {
                story_id,
                model,
                skip_layer2,
            } => {
                assert_eq!(story_id, "owner/repo#1");
                assert_eq!(model.as_deref(), Some("claude-sonnet-4-6"));
                assert!(skip_layer2);
            }
            other => panic!("expected Command::StartRun, got a different variant: {other:?}"),
        }

        // Without the optional flags: model is None, skip_layer2 defaults false.
        let cli =
            Cli::try_parse_from(["camerata", "start-run", "owner/repo#1"]).expect("must parse");
        match cli.command {
            Command::StartRun {
                model, skip_layer2, ..
            } => {
                assert!(model.is_none());
                assert!(!skip_layer2);
            }
            other => panic!("expected Command::StartRun, got a different variant: {other:?}"),
        }
    }

    /// The global `--bff-url` flag must parse regardless of which subcommand it
    /// precedes (clap `global = true` semantics).
    #[test]
    fn global_bff_url_flag_parses_before_the_subcommand() {
        let cli = Cli::try_parse_from([
            "camerata",
            "--bff-url",
            "http://example.test:9999",
            "stories",
        ])
        .expect("must parse");
        assert_eq!(cli.bff_url.as_deref(), Some("http://example.test:9999"));
    }

    /// No subcommand at all must fail to parse (clap requires one; there is no more
    /// implicit "print usage and exit 0" fallback for the bare invocation).
    #[test]
    fn missing_subcommand_fails_to_parse() {
        assert!(Cli::try_parse_from(["camerata"]).is_err());
    }
}
