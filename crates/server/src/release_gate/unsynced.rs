//! The release gate refuses to export while the review screen holds a change the decision store
//! has not accepted.
//!
//! The export reads the store. A decision the reviewer made but that never reached it is missing
//! from the package, and an undo whose revoke never landed would export the very decision the
//! reviewer took back. The screen's write-through queue lists what it still holds in the export
//! request, and this check turns each into a problem that names the site.

use camerata_api_types::decisions::{UnsyncedChange, UnsyncedKind};

use super::Problem;
use crate::onboard::Finding;

/// One problem per unsynced change, naming the site (`path line`, or the key itself when no row
/// carries it, so nothing is dropped silently).
pub fn problems(findings: &[Finding], changes: &[UnsyncedChange]) -> Vec<Problem> {
    changes
        .iter()
        .map(|c| {
            let site = findings
                .iter()
                .find(|f| f.site_key == c.key)
                .map(|f| format!("{} line {}", f.path, f.line))
                .unwrap_or_else(|| c.key.clone());
            let message = match c.change {
                UnsyncedKind::Decision => {
                    "a decision made on this site has not reached the decision store, so the \
                     export would not carry it. Wait for it to be stored, or fix what is \
                     refusing it, then export again"
                }
                UnsyncedKind::Undo => {
                    "an undo made on this site has not reached the decision store, which \
                     would still export the decision that was taken back. Wait for it to be \
                     stored, then export again"
                }
            };
            Problem::new("reviewer decisions", site, message, None)
        })
        .collect()
}
