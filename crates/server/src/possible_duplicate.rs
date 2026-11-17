//! The "Possible duplicate" hint: two causes with a row at the same place (an operator review aid).
//!
//! # What it marks
//!
//! Recall comes before cutting duplicates, so a problem found twice under two causes can reach
//! review. Two different causes (different Fix ids) that each have a problem row in one file, at
//! lines within [`NEAR_LINES`] of each other, are marked so the operator finds the pair and rules
//! the extra one out. A residual sub-group ("No shared cause found") counts each place as its own
//! cause, as the counts do, so two residual rows near each other pair even though they share a Fix.
//!
//! # What it never does
//!
//! The hint never hides, drops, merges, lowers or reorders anything a client sees, changes no
//! count, severity, kind, escalation or decision, and never holds an export. A client package
//! carries none of it. A hinted row stays a problem until the operator rules on it.
//!
//! # Where it is worked out
//!
//! Once per report, here, from data the run already holds, with no model call: [`DupIndex::new`].
//! The review view and the internal exports both call it, beside
//! [`crate::possible_false_positive::FpIndex`].

use std::collections::{BTreeMap, HashMap};

use crate::onboard::Finding;
use crate::review_tree::{FixIndex, ReviewTree};

/// Two rows are at the same place when they are in one file and their lines are this close.
pub const NEAR_LINES: usize = 3;

/// One other cause that has a row near a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DupLink {
    /// The other cause's Fix id.
    pub fix_id: String,
    /// The site key of the other cause's row nearest the row.
    pub key: String,
    /// That row's rule id.
    pub rule_id: String,
    /// That row's line.
    pub line: usize,
    /// That row's sub-group node id.
    pub group_id: String,
    /// How many lines apart the two rows are.
    pub distance: usize,
    /// Whether the other cause has the same Fix id as the row (two places of one rule's residual
    /// sub-group, each its own cause), so the Fix id alone does not tell them apart.
    pub same_fix: bool,
}

/// For each row (by site key), the other causes with a row at the same place.
#[derive(Debug, Default)]
pub struct DupIndex {
    by_key: HashMap<String, Vec<DupLink>>,
}

/// A row as the index sees it.
struct Entry<'a> {
    f: &'a Finding,
    fix_id: String,
    /// What makes the row's cause: its Fix id, and for a residual sub-group its place too.
    cause: (String, Option<usize>),
    /// The place node the row is carried at, when it shares its place with another row.
    place: Option<String>,
}

/// Whether the row says something is wrong at its place (a defect, a departure or no kind), as
/// the Fixes count them, and sits at a line of a file.
fn is_problem_at_a_line(f: &Finding) -> bool {
    use camerata_api_types::audit::FindingKind;
    // A row the engine threw out is in no package, and a row the reviewer merged into another is
    // already ruled on: neither is a candidate for the hint.
    !crate::engine_discard::is_discarded(f)
        && crate::report_export::overlay::merged_target(f).is_none()
        && f.line > 0
        && !f.path.is_empty()
        && !f.site_key.is_empty()
        && matches!(
            f.kind,
            None | Some(FindingKind::Defect) | Some(FindingKind::Deviation(_))
        )
}

/// Whether the row's sub-group is a rule's residual one (ordinal 0).
fn is_residual(f: &Finding) -> bool {
    f.group_path.get(2).is_some_and(|s| s.ends_with("/0"))
}

impl DupIndex {
    /// Index the rows of one report: the rows a review or an export reads.
    pub fn new(tree: Option<&ReviewTree>, findings: &[Finding]) -> Self {
        let fixes = FixIndex::new(tree, findings);
        // A rule carried by another row at the same place is that row's, not another cause.
        let place_of: HashMap<&str, &str> = tree
            .into_iter()
            .flat_map(|t| &t.places)
            .flat_map(|p| p.members.iter().map(move |m| (m.as_str(), p.id.as_str())))
            .collect();
        Self::with_fix_ids(
            findings,
            |f| fixes.fix_id_of(f),
            |f| place_of.get(f.site_key.as_str()).map(|p| (*p).to_string()),
        )
    }

    /// Index `findings`, the Fix of each row given by `fix_of` (empty for a row in none) and the
    /// carried place it shares with other rows by `place_of`.
    pub(crate) fn with_fix_ids(
        findings: &[Finding],
        fix_of: impl Fn(&Finding) -> String,
        place_of: impl Fn(&Finding) -> Option<String>,
    ) -> Self {
        let mut files: BTreeMap<(&str, &str), Vec<Entry<'_>>> = BTreeMap::new();
        for f in findings.iter().filter(|f| is_problem_at_a_line(f)) {
            let fix_id = fix_of(f);
            if fix_id.is_empty() {
                continue;
            }
            let cause = (fix_id.clone(), is_residual(f).then_some(f.line));
            files
                .entry((f.repo.as_str(), f.path.as_str()))
                .or_default()
                .push(Entry {
                    f,
                    fix_id,
                    cause,
                    place: place_of(f),
                });
        }
        let mut by_key: HashMap<String, Vec<DupLink>> = HashMap::new();
        for entries in files.values_mut() {
            entries.sort_by(|a, b| (a.f.line, &a.f.site_key).cmp(&(b.f.line, &b.f.site_key)));
            for (i, a) in entries.iter().enumerate() {
                for b in &entries[i + 1..] {
                    let distance = b.f.line - a.f.line;
                    if distance > NEAR_LINES {
                        break;
                    }
                    if a.cause == b.cause || (a.place.is_some() && a.place == b.place) {
                        continue;
                    }
                    by_key
                        .entry(a.f.site_key.clone())
                        .or_default()
                        .push(link_to(b, distance, a.fix_id == b.fix_id));
                    by_key
                        .entry(b.f.site_key.clone())
                        .or_default()
                        .push(link_to(a, distance, a.fix_id == b.fix_id));
                }
            }
        }
        // Each other cause once (its nearest row), nearest first, then by Fix id.
        for links in by_key.values_mut() {
            links.sort_by(|a, b| {
                (a.fix_id.as_str(), a.distance, a.key.as_str()).cmp(&(
                    b.fix_id.as_str(),
                    b.distance,
                    b.key.as_str(),
                ))
            });
            links.dedup_by(|b, a| a.fix_id == b.fix_id);
            links.sort_by(|a, b| {
                (a.distance, a.fix_id.as_str()).cmp(&(b.distance, b.fix_id.as_str()))
            });
        }
        Self { by_key }
    }

    /// The other causes with a row at the same place as `f`: empty when there is none.
    pub fn of(&self, f: &Finding) -> &[DupLink] {
        self.by_key.get(&f.site_key).map_or(&[], Vec::as_slice)
    }

    /// The other causes as the internal exports name them, in the order [`Self::of`] lists them:
    /// the Fix id, and when it is the row's own Fix (two places of a residual sub-group) the Fix id
    /// and the place, like `ab12 line 14`, so the cell never reads as a row duplicating itself.
    pub fn fix_ids_of(&self, f: &Finding) -> Vec<String> {
        self.of(f)
            .iter()
            .map(|l| {
                if l.same_fix {
                    format!("{} line {}", l.fix_id, l.line)
                } else {
                    l.fix_id.clone()
                }
            })
            .collect()
    }
}

fn link_to(other: &Entry<'_>, distance: usize, same_fix: bool) -> DupLink {
    DupLink {
        fix_id: other.fix_id.clone(),
        key: other.f.site_key.clone(),
        rule_id: other.f.rule_id.clone(),
        line: other.f.line,
        group_id: other.f.group_path.get(2).cloned().unwrap_or_default(),
        distance,
        same_fix,
    }
}

#[cfg(test)]
mod tests;
