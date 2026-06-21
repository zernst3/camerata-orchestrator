//! corpus-verifier CLI — the maintainer command-line surface over the CORE.
//!
//! Subcommands:
//!   - `list`         — print the risk-ordered grounded queue.
//!   - `verify`       — verify ONE rule (branch + edit + commit + push + PR).
//!   - `self-source`  — bulk self-source the maintainer-authored meta rules
//!     into ONE branch + ONE PR.
//!
//! Every flow that touches git/PR can run `--dry-run`, which uses the
//! [`corpus_verifier::DryRunVcs`] seam: it still edits the TOML locally (so you
//! can inspect the diff), but records the git/PR plan instead of executing it.
//! Pass no `--dry-run` to use the real `git` + `gh` path.

use anyhow::Result;
use clap::{Parser, Subcommand};

use corpus_verifier::{
    corpus_dir, list_grounded, self_source, today, verify_one, DryRunVcs, GitVcs, VcsOps,
};

#[derive(Parser)]
#[command(
    name = "corpus-verifier",
    about = "MAINTAINER-ONLY: promote corpus rules grounded -> verified via branch + PR into main",
    long_about = "Repo-governance tool. NOT part of the shipped Camerata product. Writes `verified` \
ONLY through a reviewed commit in main (branch + PR). The app stays read-only on verification."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the risk-ordered list of grounded rules to verify.
    List,

    /// Verify a single rule: branch + edit + commit + push + PR into main.
    Verify {
        /// The rule id to verify (e.g. RUST-NO-UNWRAP-1).
        rule_id: String,
        /// Who is verifying (human identifier).
        #[arg(long)]
        by: String,
        /// Version anchor(s) verified against. Repeatable. If omitted, prefilled
        /// from the rule's [[sources]].
        #[arg(long)]
        against: Vec<String>,
        /// Plan only: edit the TOML and record the git/PR plan without running it.
        #[arg(long)]
        dry_run: bool,
    },

    /// Bulk self-source the maintainer-authored meta rules into ONE branch + PR.
    SelfSource {
        /// Restrict to a single meta domain (e.g. agentic, api-layer, permissions, ui, universal).
        #[arg(long)]
        domain: Option<String>,
        /// Cover ALL meta domains (mutually informative with --domain; --domain wins if both set).
        #[arg(long)]
        all_meta: bool,
        /// Who is verifying (human identifier).
        #[arg(long)]
        by: String,
        /// Plan only: edit the TOML and record the git/PR plan without running it.
        #[arg(long)]
        dry_run: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let dir = corpus_dir();

    match cli.command {
        Command::List => {
            let rows = list_grounded(&dir).await?;
            if rows.is_empty() {
                println!("No grounded rules. Nothing to verify.");
                return Ok(());
            }
            println!("Grounded queue ({} rules, risk-ordered):\n", rows.len());
            println!("{:<42}  {:<20}  {:<14}  source", "RULE ID", "DOMAIN", "ENFORCEMENT");
            for r in &rows {
                println!(
                    "{:<42}  {:<20}  {:<14}  {}",
                    r.id,
                    r.domain,
                    r.enforcement,
                    r.primary_source.as_deref().unwrap_or("(none)")
                );
            }
        }

        Command::Verify {
            rule_id,
            by,
            against,
            dry_run,
        } => {
            let at = today();
            let prefilled = against.is_empty();
            let outcome = run_verify(&dir, &rule_id, &by, &at, against, dry_run).await?;
            if prefilled {
                println!("--against omitted; prefilled from the rule's sources:");
                for a in &outcome.against {
                    println!("  - {a}");
                }
            }
            print_outcome(dry_run, &outcome.branch, &outcome.pr_url);
        }

        Command::SelfSource {
            domain,
            all_meta,
            by,
            dry_run,
        } => {
            let at = today();
            // --domain takes precedence; otherwise require --all-meta to bulk all.
            let scope = match (&domain, all_meta) {
                (Some(d), _) => Some(d.clone()),
                (None, true) => None,
                (None, false) => {
                    anyhow::bail!("specify --domain <d> or --all-meta");
                }
            };
            let outcome = run_self_source(&dir, scope.as_deref(), &by, &at, dry_run).await?;
            print_outcome(dry_run, &outcome.branch, &outcome.pr_url);
        }
    }

    Ok(())
}

async fn run_verify(
    dir: &std::path::Path,
    rule_id: &str,
    by: &str,
    at: &str,
    against: Vec<String>,
    dry_run: bool,
) -> Result<corpus_verifier::VerifyOutcome> {
    if dry_run {
        let vcs = DryRunVcs::new();
        let outcome = verify_one(dir, rule_id, by, at, against, &vcs).await?;
        print_plan(&vcs);
        Ok(outcome)
    } else {
        let vcs: GitVcs = GitVcs::new();
        verify_one(dir, rule_id, by, at, against, &vcs).await
    }
}

async fn run_self_source(
    dir: &std::path::Path,
    domain: Option<&str>,
    by: &str,
    at: &str,
    dry_run: bool,
) -> Result<corpus_verifier::VerifyOutcome> {
    if dry_run {
        let vcs = DryRunVcs::new();
        let outcome = self_source(dir, domain, by, at, &vcs).await?;
        print_plan(&vcs);
        Ok(outcome)
    } else {
        let vcs: GitVcs = GitVcs::new();
        self_source(dir, domain, by, at, &vcs).await
    }
}

fn print_plan(vcs: &DryRunVcs) {
    println!("\n[DRY RUN] git/PR plan (no branch/push/PR executed):");
    for line in vcs.plan() {
        println!("  {line}");
    }
}

fn print_outcome(dry_run: bool, branch: &str, pr_url: &str) {
    if dry_run {
        println!("\n[DRY RUN] would open PR: {pr_url}");
        println!("[DRY RUN] branch: {branch}");
        println!("[DRY RUN] (the TOML was edited locally so you can inspect the diff)");
    } else {
        println!("\nBranch: {branch}");
        println!("PR: {pr_url}");
    }
}

// VcsOps is referenced in the bounds of GitVcs/DryRunVcs; keep the import used.
const _: fn() = || {
    fn _assert_impls<T: VcsOps>() {}
    _assert_impls::<GitVcs>();
    _assert_impls::<DryRunVcs>();
};
