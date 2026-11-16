//! The words a finding's `status` can carry, in one place.
//!
//! `active`: an open problem. A row found by an outside tool in preview is `active` too; the
//! row's `preview` field (the Source column in the workbook) says it came from a tool.
//! `carried-baseline`: matched to a record in a baseline Cantus wrote.
//! `waived-inline`: matched to a `camerata:allow` waiver comment.
//! The old words `suppressed-baseline` and `suppressed-inline` are read from sealed reports
//! made before the change and mapped here; nothing writes them any more.

use crate::onboard::Finding;

pub const ACTIVE: &str = "active";
pub const CARRIED_BASELINE: &str = "carried-baseline";
pub const WAIVED_INLINE: &str = "waived-inline";

const LEGACY_BASELINE: &str = "suppressed-baseline";
const LEGACY_INLINE: &str = "suppressed-inline";

/// Whether the status says a real baseline record carried the row (either word).
pub fn is_carried_baseline(status: &str) -> bool {
    status == CARRIED_BASELINE || status == LEGACY_BASELINE
}

/// Whether the status says an inline waiver covers the row (either word).
pub fn is_waived_inline(status: &str) -> bool {
    status == WAIVED_INLINE || status == LEGACY_INLINE
}

/// Whether the engine treats the row as not open: carried, waived or the self-reference mark.
pub fn is_suppressed(status: &str) -> bool {
    is_carried_baseline(status) || is_waived_inline(status) || status.starts_with("suppressed")
}

/// The current word for a stored status. A preview row never carries a baseline word: those
/// rows were stamped `suppressed-baseline` only to keep them out of the enforced tally.
pub fn canonical(status: &str, preview: bool) -> String {
    if is_carried_baseline(status) {
        if preview {
            ACTIVE
        } else {
            CARRIED_BASELINE
        }
    } else if is_waived_inline(status) {
        WAIVED_INLINE
    } else {
        return status.to_string();
    }
    .to_string()
}

/// Map one row's stored status to the current word, in place.
pub fn normalize(f: &mut Finding) {
    f.status = canonical(&f.status, f.preview);
}

/// The `ScanReport.findings` reader: the read shim for sealed reports that hold the old words.
pub fn de_findings<'de, D>(d: D) -> Result<Vec<Finding>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let mut rows: Vec<Finding> = serde::Deserialize::deserialize(d)?;
    rows.iter_mut().for_each(normalize);
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_words_map_and_a_preview_row_is_active() {
        assert_eq!(canonical("suppressed-baseline", false), "carried-baseline");
        assert_eq!(canonical("suppressed-baseline", true), "active");
        assert_eq!(canonical("suppressed-inline", true), "waived-inline");
        assert_eq!(canonical("active", true), "active");
        assert_eq!(
            canonical("suppressed-self-reference", false),
            "suppressed-self-reference"
        );
        assert!(is_suppressed("suppressed-self-reference"));
        assert!(!is_suppressed("active"));
    }
}
