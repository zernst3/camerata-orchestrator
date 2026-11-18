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
//! Place alone cannot tell the same flaw written twice from two different flaws that sit near
//! each other (round 4 measured 31 pairs in one planted problem against 156 across different
//! ones). So the places are only CANDIDATES ([`DupIndex::candidates`]); the scan asks a model, once
//! per run, whether each candidate pair is the same flaw ([`check`]), and stores the answers
//! ([`check::DuplicateChecks`], on the scan ledger and in the run folder). [`DupIndex::new`] keeps
//! only the candidate pairs the model answered `same`. The review view and the internal exports
//! both call it, beside [`crate::possible_false_positive::FpIndex`], and make no model call: they
//! read the stored answers. With no answers (the check did not run, or failed) no hint is shown.

use std::collections::{BTreeMap, HashMap};

use crate::onboard::Finding;
use crate::review_tree::{FixIndex, ReviewTree};

pub mod check;

use check::DuplicateChecks;

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
    /// The model's one-line reason for calling the two write-ups the same flaw: empty when the
    /// link was not checked by a model.
    pub reason: String,
}

/// One pair of rows at the same place (a candidate for the model to judge): in one file, lines
/// within [`NEAR_LINES`], different causes. `a` is the row that comes first in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidatePair {
    pub key_a: String,
    pub key_b: String,
    pub fix_a: String,
    pub fix_b: String,
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
    /// Index the rows of one report: the rows a review or an export reads. Only the candidate
    /// pairs `checks` holds the model's answer `same` for are linked; with no answers there is no
    /// link.
    pub fn new(tree: Option<&ReviewTree>, findings: &[Finding], checks: &DuplicateChecks) -> Self {
        let (fix_of, place_of) = lookups(tree, findings);
        Self::build(findings, &fix_of, &place_of, &|a, b| {
            checks.reason_if_same(&a.site_key, &b.site_key)
        })
    }

    /// The candidate pairs of one report, exactly the places [`Self::new`] would link before the
    /// model is asked: the pairs the duplicate check puts to the model.
    pub fn candidates(tree: Option<&ReviewTree>, findings: &[Finding]) -> Vec<CandidatePair> {
        let (fix_of, place_of) = lookups(tree, findings);
        let mut out = Vec::new();
        for_each_pair(findings, &fix_of, &place_of, &mut |a, b, _| {
            out.push(CandidatePair {
                key_a: a.f.site_key.clone(),
                key_b: b.f.site_key.clone(),
                fix_a: a.fix_id.clone(),
                fix_b: b.fix_id.clone(),
            });
        });
        out
    }

    /// Index `findings` with EVERY candidate pair linked (no model answer asked for), the Fix of
    /// each row given by `fix_of` (empty for a row in none) and the carried place it shares with
    /// other rows by `place_of`. The candidate tests read it.
    #[cfg(test)]
    pub(crate) fn with_fix_ids(
        findings: &[Finding],
        fix_of: impl Fn(&Finding) -> String,
        place_of: impl Fn(&Finding) -> Option<String>,
    ) -> Self {
        Self::build(findings, &fix_of, &place_of, &|_, _| Some(String::new()))
    }

    /// Link the candidate pairs `keep` answers `Some(reason)` for.
    fn build(
        findings: &[Finding],
        fix_of: &dyn Fn(&Finding) -> String,
        place_of: &dyn Fn(&Finding) -> Option<String>,
        keep: &dyn Fn(&Finding, &Finding) -> Option<String>,
    ) -> Self {
        let mut by_key: HashMap<String, Vec<DupLink>> = HashMap::new();
        for_each_pair(findings, fix_of, place_of, &mut |a, b, distance| {
            let Some(reason) = keep(a.f, b.f) else { return };
            by_key
                .entry(a.f.site_key.clone())
                .or_default()
                .push(link_to(b, distance, a.fix_id == b.fix_id, &reason));
            by_key
                .entry(b.f.site_key.clone())
                .or_default()
                .push(link_to(a, distance, a.fix_id == b.fix_id, &reason));
        });
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

/// The Fix id of a row and the carried place it shares with other rows, read from the review tree.
#[allow(clippy::type_complexity)]
fn lookups<'a>(
    tree: Option<&'a ReviewTree>,
    findings: &[Finding],
) -> (
    impl Fn(&Finding) -> String + 'a,
    impl Fn(&Finding) -> Option<String> + 'a,
) {
    let fixes = FixIndex::new(tree, findings);
    // A rule carried by another row at the same place is that row's, not another cause.
    let place_of: HashMap<&str, &str> = tree
        .into_iter()
        .flat_map(|t| &t.places)
        .flat_map(|p| p.members.iter().map(move |m| (m.as_str(), p.id.as_str())))
        .collect();
    (
        move |f: &Finding| fixes.fix_id_of(f),
        move |f: &Finding| place_of.get(f.site_key.as_str()).map(|p| (*p).to_string()),
    )
}

/// Call `visit` for every candidate pair of rows: two problem rows in one file, lines within
/// [`NEAR_LINES`], of different causes (a place shared by a carried rule is one cause).
fn for_each_pair(
    findings: &[Finding],
    fix_of: &dyn Fn(&Finding) -> String,
    place_of: &dyn Fn(&Finding) -> Option<String>,
    visit: &mut dyn FnMut(&Entry<'_>, &Entry<'_>, usize),
) {
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
                visit(a, b, distance);
            }
        }
    }
}

fn link_to(other: &Entry<'_>, distance: usize, same_fix: bool, reason: &str) -> DupLink {
    DupLink {
        fix_id: other.fix_id.clone(),
        key: other.f.site_key.clone(),
        rule_id: other.f.rule_id.clone(),
        line: other.f.line,
        group_id: other.f.group_path.get(2).cloned().unwrap_or_default(),
        distance,
        same_fix,
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests;
