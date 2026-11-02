//! The loader step that undoes the old absorbing merge (design 3.2).
//!
//! Before grouping stopped absorbing, a merge folded the sites of the rows it merged into the
//! survivor's `also_locations`. A report stored (or a row carried forward) from that time still
//! has them there. [`unfold_also_locations`] promotes each absorbed site back to a row of its own
//! with the survivor's detail and fix, in the survivor's sub-group, and empties the survivor's
//! list, so the site count is the row count. It is idempotent: a second call finds nothing to do.

use crate::ai_audit::{finding_headline, HEADLINE_SEP};
use crate::onboard::{Finding, MergedLocation};

use super::types::GroupRole;

/// The row an absorbed site becomes: the site's own place, rule, snippet and (when it recorded
/// them) severity and headline, with everything else taken from the survivor it was folded into.
fn promote(primary: &Finding, site: &MergedLocation) -> Finding {
    let mut row = primary.clone();
    row.repo.clone_from(&site.repo);
    row.path.clone_from(&site.path);
    row.line = site.line;
    row.rule_id.clone_from(&site.rule_id);
    row.snippet.clone_from(&site.snippet);
    if !site.severity.is_empty() {
        row.severity.clone_from(&site.severity);
    }
    // The site's own title leads, so the headline the row shows is the one it carried when it
    // was absorbed; the survivor's explanation follows.
    if !site.headline.is_empty() && site.headline != finding_headline(primary) {
        row.detail = format!("{}{HEADLINE_SEP}{}", site.headline, primary.detail);
    }
    row.located = !site.consequence;
    row.also_matches = Vec::new();
    row.also_locations = Vec::new();
    row.flagged_by = Vec::new();
    row.possible_duplicate_of = None;
    row.site_key = String::new();
    row.group_role = GroupRole::Member;
    row
}

/// Promote every absorbed site to a row. Returns how many rows were added. The survivor keeps its
/// own place and loses only its `also_locations`; each promoted row follows its survivor in the
/// list, in the survivor's `group_path`.
pub fn unfold_also_locations(findings: &mut Vec<Finding>) -> usize {
    if findings.iter().all(|f| f.also_locations.is_empty()) {
        return 0;
    }
    let mut out: Vec<Finding> = Vec::with_capacity(findings.len());
    let mut added = 0;
    for mut f in std::mem::take(findings) {
        let sites = std::mem::take(&mut f.also_locations);
        let promoted: Vec<Finding> = sites.iter().map(|s| promote(&f, s)).collect();
        added += promoted.len();
        out.push(f);
        out.extend(promoted);
    }
    *findings = out;
    added
}
