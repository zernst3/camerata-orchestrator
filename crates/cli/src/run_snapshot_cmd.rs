//! `camerata run snapshot-rules --run <id> --from <rule library folder>` (#560): save the rule
//! snapshot of a run made before snapshots were kept, so it can be exported.
//!
//! The work is the server's shared [`camerata_server::run_store::rule_snapshot::backfill`]: it
//! loads the folder's rule library, refuses unless its digest equals the run's pinned rule digest,
//! and only then writes the snapshot. It never guesses and writes nothing on a refusal. An app
//! inspection made from the run picks the snapshot up at its next export.

use std::path::Path;

use camerata_server::run_store::rule_snapshot::{backfill, Backfilled};

/// Run the backfill and return the line to print, or the refusal to print (exit 1).
pub async fn snapshot_rules(run: &str, from: &Path) -> Result<String, String> {
    match backfill(run, from).await {
        Ok(Backfilled::Written { digest, rules }) => Ok(format!(
            "camerata run snapshot-rules: saved the {rules} rules run {run} ran with ({digest}). \
             Its inspection can be exported now."
        )),
        Ok(Backfilled::AlreadyKept { digest }) => Ok(format!(
            "camerata run snapshot-rules: run {run} already keeps the rules it ran with \
             ({digest}). Nothing was written."
        )),
        Err(refusal) => Err(format!("camerata run snapshot-rules: {refusal}")),
    }
}
