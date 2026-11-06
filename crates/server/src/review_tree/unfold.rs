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

/// The identity of a site for "is this the same site": the engine's own site key (repo, path and
/// the fingerprint of rule id plus snippet, so two unlocated sites of one rule in one file are
/// told apart by their snippets) and the line.
type SiteIdentity = (String, usize);

fn identity_of_row(f: &Finding) -> SiteIdentity {
    (
        camerata_api_types::site_key::site_key(&f.repo, &f.path, &f.rule_id, &f.snippet, 0),
        f.line,
    )
}

fn identity_of_site(l: &MergedLocation) -> SiteIdentity {
    (
        camerata_api_types::site_key::site_key(&l.repo, &l.path, &l.rule_id, &l.snippet, 0),
        l.line,
    )
}

/// Promote every absorbed site to a row. Returns how many rows were added. The survivor keeps its
/// own place and loses only its `also_locations`; each promoted row follows its survivor in the
/// list, in the survivor's `group_path`.
///
/// A site never appears twice: one that two rows both absorbed, or that a row lists twice, is
/// promoted once, and one that is already a row of the same rule at the same place is not
/// promoted again (it is that row). Nothing is lost by that: if the duplicate was rated higher
/// than the row that already stands for the place, that row takes the higher rating and records
/// why.
pub fn unfold_also_locations(findings: &mut Vec<Finding>) -> usize {
    if findings.iter().all(|f| f.also_locations.is_empty()) {
        return 0;
    }
    /// The row that stands for a site: an original row, or the j-th row promoted after row i.
    #[derive(Clone, Copy)]
    enum Stands {
        Original(usize),
        Promoted(usize, usize),
    }
    let mut originals = std::mem::take(findings);
    let sites: Vec<Vec<MergedLocation>> = originals
        .iter_mut()
        .map(|f| std::mem::take(&mut f.also_locations))
        .collect();
    let mut stands: std::collections::HashMap<SiteIdentity, Stands> =
        std::collections::HashMap::new();
    for (i, f) in originals.iter().enumerate() {
        stands
            .entry(identity_of_row(f))
            .or_insert(Stands::Original(i));
    }
    let mut promoted: Vec<Vec<Finding>> = vec![Vec::new(); originals.len()];
    let mut raises: Vec<(Stands, MergedLocation)> = Vec::new();
    let mut added = 0;
    for (i, row_sites) in sites.iter().enumerate() {
        for site in row_sites {
            let id = identity_of_site(site);
            if let Some(&at) = stands.get(&id) {
                raises.push((at, site.clone()));
                continue;
            }
            promoted[i].push(promote(&originals[i], site));
            stands.insert(id, Stands::Promoted(i, promoted[i].len() - 1));
            added += 1;
        }
    }
    for (at, site) in raises {
        let row = match at {
            Stands::Original(i) => &mut originals[i],
            Stands::Promoted(i, j) => &mut promoted[i][j],
        };
        // A located duplicate makes the row that stands for the place a located one.
        row.located |= !site.consequence;
        if !site.severity.is_empty()
            && crate::ai_audit::severity_rank(&site.severity)
                > crate::ai_audit::severity_rank(&row.severity)
        {
            let from = row.severity.clone();
            crate::ai_audit::record_severity_adjustment(
                row,
                &from,
                &site.severity,
                &format!(
                    "{} was absorbed at this place into another row at a higher rating",
                    site.rule_id
                ),
            );
            row.severity.clone_from(&site.severity);
        }
    }
    let mut result: Vec<Finding> = Vec::with_capacity(originals.len() + added);
    for (row, extra) in originals.into_iter().zip(promoted) {
        result.push(row);
        result.extend(extra);
    }
    *findings = result;
    added
}
