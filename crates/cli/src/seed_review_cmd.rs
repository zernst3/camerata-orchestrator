//! `camerata seed-review <fixture dir> --data-dir <dir> [--variant moved] [--force]`: write a demo
//! review project into a data directory, so the liveview build and the desktop smoke checklist
//! (review UI spec sections 13 and 14) open a realistic project without a paid scan.
//!
//! The work is [`camerata_server::seed`]: every record goes through the server's own stores and
//! routes, with no model call and no network. This module is the command's arguments and its
//! report. A data directory with anything in it is refused unless `--force` is passed, and
//! nothing is ever deleted.

use std::path::PathBuf;

use camerata_server::seed::{self, SeedOptions, Seeded, Variant};
use clap::ValueEnum;

/// Which version of the demo project to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum VariantArg {
    /// One inspection and the reviewer's work on it.
    Base,
    /// A second inspection after the code moved: a stale decision, a stale edit, a new site.
    Moved,
    /// The base project with every held row released: the release gate refuses nothing.
    Clean,
}

impl From<VariantArg> for Variant {
    fn from(v: VariantArg) -> Self {
        match v {
            VariantArg::Base => Variant::Base,
            VariantArg::Moved => Variant::Moved,
            VariantArg::Clean => Variant::Clean,
        }
    }
}

/// Arguments of `camerata seed-review`.
#[derive(Debug, Clone)]
pub struct SeedReviewArgs {
    /// The fixture folder (`run.json`, `overlays.json`, `moved.json`).
    pub fixture: PathBuf,
    /// The data directory to seed. The stores are written under `<data dir>/camerata`, the place
    /// `CAMERATA_DATA_DIR=<data dir>` makes the server look.
    pub data_dir: PathBuf,
    pub variant: VariantArg,
    /// Seed a data directory that is not empty, adding the project beside what is there.
    pub force: bool,
}

/// Seed, and describe what was written.
///
/// # Errors
/// A message when the data directory is not empty (and `force` is not set), the fixture cannot be
/// read, or a store refuses a write.
pub async fn run_seed_review(args: &SeedReviewArgs) -> Result<String, String> {
    let seeded = seed::seed_review(
        &args.fixture,
        &args.data_dir,
        &SeedOptions {
            variant: args.variant.into(),
            force: args.force,
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(report(args, &seeded))
}

fn report(args: &SeedReviewArgs, seeded: &Seeded) -> String {
    let c = &seeded.shell.counts;
    let mut out = String::new();
    out.push_str(&format!(
        "Seeded project {} into {}\n",
        seeded.project_id,
        seeded.store_dir.display()
    ));
    out.push_str(&format!("  inspections: {}\n", seeded.run_ids.join(", ")));
    out.push_str(&format!(
        "  sites {} | unresolved {} | confirmed {} | ignored {} | false positive {} | tech debt {} | merged {} | held {}\n",
        c.total, c.unresolved, c.confirmed, c.ignored, c.false_positive, c.tech_debt, c.merged, c.held
    ));
    let s = &seeded.shell;
    out.push_str(&format!(
        "  line: {} escalated of {} candidates ({} over budget) | Engine Check cards {} | stale decisions {} | stale edits {} | moved {} | new since decision {}\n",
        s.budget.escalated,
        s.budget.candidates,
        s.budget.over_budget,
        s.lanes.engine_check,
        s.stale.stale_decisions,
        s.stale.stale_edits,
        s.stale.moved,
        s.stale.new_since_decision,
    ));
    out.push_str("  wrote:\n");
    for file in seed::written_files(&seeded.store_dir) {
        out.push_str(&format!("    {}\n", file.display()));
    }
    out.push_str(&format!(
        "Open it with: CAMERATA_DATA_DIR={} cargo run -p camerata-server\n",
        args.data_dir.display()
    ));
    out
}
