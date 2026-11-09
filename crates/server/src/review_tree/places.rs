//! The Place (D2, rulings R-6a to R-6d): one review row for each exact (repo, path, line).
//!
//! Rows of different rule families never share a sub-group, so one committed secret that six
//! rules flag at the same line is six rows under six rule nodes. The place is the key the
//! escalation slot (R-1), the All problems sheet and the `same_line` duplicate link already agree
//! on; this module makes it the key of the review row too.
//!
//! # The rule
//!
//! * A place is the exact (repo, path, line) of a PROBLEM row: a defect, a departure or a row with
//!   no kind. A convention, a not-assessed statement, a row the engine threw out and a suppressed
//!   row are never carried and never carry. Line 0 (the whole file) is a place of its own.
//! * The row at the place with the highest `primary_rank` is its carrier; the others are carried
//!   and listed on it as "Also matches". Nothing merges across lines.
//! * No row is removed, hidden or re-severed. The carrying is a ledger block
//!   ([`PlaceMergeBlock`]), never a `merged_into` decision record: the reviewer's place decision
//!   (`place:<id>`) is what the decision log shows.
//!
//! Like the Fixes, the places are worked out ONCE from the rows and the tree alone, never from a
//! decision, so a place id is the same before, during and after the review. Only places with two
//! or more rows are stored.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{is_thrown_out_id, ReviewTree};
use crate::ai_audit::primary_rank;
use crate::onboard::Finding;

/// The id prefix of a node-scope decision on a place (`place:<id>`).
pub const PLACE_PREFIX: &str = "place:";

/// The ledger stage name of the carrying.
pub const PLACE_MERGE_STAGE: &str = "place-merge";

/// One place with two or more problem rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaceNode {
    /// Twelve hex digits of the repo, path and line: the same for the same place on every run,
    /// whichever row carries it.
    pub id: String,
    pub repo: String,
    pub path: String,
    /// 1-based; 0 is the whole file.
    pub line: usize,
    /// The site key of the row that heads the place.
    pub carrier: String,
    /// Every site key at the place, the carrier first, then by descending `primary_rank`.
    pub members: Vec<String>,
    /// Every rule id of the members, sorted.
    pub rules: Vec<String>,
    /// The Fixes the members are in, sorted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fix_ids: Vec<String>,
}

impl PlaceNode {
    /// The node-scope decision id of this place.
    pub fn node_id(&self) -> String {
        format!("{PLACE_PREFIX}{}", self.id)
    }

    /// The site keys the place carries (every member but the carrier).
    pub fn carried(&self) -> impl Iterator<Item = &str> {
        self.members.iter().skip(1).map(String::as_str)
    }
}

/// The carrying as the ledger records it: problem rows in equal places out plus rows carried.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaceMergeBlock {
    pub stage: String,
    pub rows_in: usize,
    pub places_out: usize,
    pub carried: usize,
}

impl PlaceMergeBlock {
    /// Rows in equals places out plus rows carried.
    pub fn is_integral(&self) -> bool {
        self.rows_in == self.places_out + self.carried
    }

    /// The block for the problem rows of `findings` and the multi-row `places` among them.
    pub fn of(findings: &[Finding], places: &[PlaceNode]) -> Self {
        let rows: Vec<&Finding> = findings.iter().filter(|f| is_place_row(f)).collect();
        let distinct: BTreeSet<(&str, &str, usize)> = rows
            .iter()
            .map(|f| (f.repo.as_str(), f.path.as_str(), f.line))
            .collect();
        Self {
            stage: PLACE_MERGE_STAGE.to_string(),
            rows_in: rows.len(),
            places_out: distinct.len(),
            carried: places.iter().map(|p| p.members.len() - 1).sum(),
        }
    }
}

/// The id of the place (`repo`, `path`, `line`).
pub fn place_id_of(repo: &str, path: &str, line: usize) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(format!("{repo}\n{path}\n{line}").as_bytes())
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Whether a row says something is wrong at its place (a defect, a departure, or a row with no
/// kind), as opposed to an observation.
fn is_problem(f: &Finding) -> bool {
    use camerata_api_types::audit::FindingKind;
    matches!(
        f.kind,
        None | Some(FindingKind::Defect) | Some(FindingKind::Deviation(_))
    )
}

/// Whether the engine's own state makes `f` a row that can carry or be carried: a problem, with a
/// site key, active, not thrown out.
pub fn is_place_row(f: &Finding) -> bool {
    is_problem(f)
        && !f.site_key.is_empty()
        && !f.status.starts_with("suppressed")
        && !crate::engine_discard::is_discarded(f)
        && !f.group_path.iter().any(|g| is_thrown_out_id(g))
}

/// The places with two or more problem rows of `findings`, sorted by (repo, path, line). Pure and
/// deterministic. Run it after the Fixes: each place lists the Fixes of its rows.
pub fn merge_places(tree: &ReviewTree, findings: &[Finding]) -> Vec<PlaceNode> {
    merge_places_where(tree, findings, &|_| false)
}

/// [`merge_places`] with one more way to leave a row out: `excluded` says a row is not a problem
/// (an export that knows the reviewer's false positives passes them here).
pub fn merge_places_where(
    tree: &ReviewTree,
    findings: &[Finding],
    excluded: &dyn Fn(&Finding) -> bool,
) -> Vec<PlaceNode> {
    let n = findings.len();
    let mut at: BTreeMap<(&str, &str, usize), Vec<usize>> = BTreeMap::new();
    for (i, f) in findings.iter().enumerate() {
        if is_place_row(f) && !excluded(f) {
            at.entry((f.repo.as_str(), f.path.as_str(), f.line))
                .or_default()
                .push(i);
        }
    }
    at.into_iter()
        .filter(|(_, rows)| rows.len() > 1)
        .map(|((repo, path, line), mut rows)| {
            // Best first; the earliest row on a tie (`primary_rank` ends on that).
            rows.sort_by_key(|&i| std::cmp::Reverse(primary_rank(&findings[i], n, i)));
            let rules: BTreeSet<String> =
                rows.iter().map(|&i| findings[i].rule_id.clone()).collect();
            let fix_ids: BTreeSet<String> = rows
                .iter()
                .filter_map(|&i| findings[i].group_path.get(2))
                .filter_map(|sub| tree.fix_of(sub))
                .map(|fx| fx.id.clone())
                .collect();
            let members: Vec<String> = rows.iter().map(|&i| findings[i].site_key.clone()).collect();
            PlaceNode {
                id: place_id_of(repo, path, line),
                repo: repo.to_string(),
                path: path.to_string(),
                line,
                carrier: members[0].clone(),
                members,
                rules: rules.into_iter().collect(),
                fix_ids: fix_ids.into_iter().collect(),
            }
        })
        .collect()
}

impl ReviewTree {
    /// Work the places out again from `findings` and store them. Returns what the ledger records.
    pub fn refresh_places(&mut self, findings: &[Finding]) -> PlaceMergeBlock {
        self.places = merge_places(self, findings);
        PlaceMergeBlock::of(findings, &self.places)
    }

    /// The carrying as the ledger records it: the problem rows in, the stored places out.
    pub fn place_merge_block(&self, findings: &[Finding]) -> PlaceMergeBlock {
        PlaceMergeBlock::of(findings, &self.places)
    }

    /// The place with this id, or `place:<id>`.
    pub fn place(&self, id: &str) -> Option<&PlaceNode> {
        let bare = id.strip_prefix(PLACE_PREFIX).unwrap_or(id);
        self.places.iter().find(|p| p.id == bare)
    }

    /// The stored place a site key is a member of.
    pub fn place_of_key(&self, key: &str) -> Option<&PlaceNode> {
        self.places
            .iter()
            .find(|p| p.members.iter().any(|m| m == key))
    }
}

/// The places of a report as an export reads them: the tree's stored places when it has any, else
/// worked out here from the rows (a report stored before places existed).
pub fn places_for(tree: Option<&ReviewTree>, findings: &[Finding]) -> Vec<PlaceNode> {
    let Some(tree) = tree else {
        return merge_places(&ReviewTree::default(), findings);
    };
    if tree.places.is_empty() {
        merge_places(tree, findings)
    } else {
        tree.places.clone()
    }
}
