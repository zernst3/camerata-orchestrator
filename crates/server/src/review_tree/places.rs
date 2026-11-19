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
        let distinct: BTreeSet<(&str, &str, usize, &str)> = rows
            .iter()
            .map(|f| place_key(&f.repo, &f.path, f.line, &f.rule_id))
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
        && !crate::finding_status::is_suppressed(&f.status)
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
    // The same key `places_of` uses: a whole-file problem (line 0) is a place of its own rule.
    let mut at: BTreeMap<(&str, &str, usize, &str), Vec<usize>> = BTreeMap::new();
    for (i, f) in findings.iter().enumerate() {
        if is_place_row(f) && !excluded(f) {
            at.entry(place_key(&f.repo, &f.path, f.line, &f.rule_id))
                .or_default()
                .push(i);
        }
    }
    at.into_iter()
        .filter(|(_, rows)| rows.len() > 1)
        .map(|((repo, path, line, rule), mut rows)| {
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
                // A whole-file place is told apart from the others in its file by its rule.
                id: if line == 0 {
                    place_id_of(repo, &format!("{path}\n{rule}"), line)
                } else {
                    place_id_of(repo, path, line)
                },
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

    /// Whether the stored places are not the ones `findings` give today. A tree sealed before the
    /// per-rule key for a whole-file place stores one place for a file whatever the rules that
    /// flag it; the places are worked out from the rows and the tree alone, so a stored list that
    /// differs from the fresh one is old and is worked out again on load.
    pub fn places_are_stale(&self, findings: &[Finding]) -> bool {
        merge_places(self, findings) != self.places
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

// ── The one place rule for counting (single counts, work order A) ─────────────────────────

/// What a row must tell the place rule: where it is, how bad, and which bucket it is in. The
/// workbook's rows and the PDF's matrix refs both implement it, so one function groups both.
pub trait PlaceRow {
    fn place_repo(&self) -> &str;
    fn place_path(&self) -> &str;
    fn place_line(&self) -> usize;
    /// The rule of the row. It is part of the place key only for a whole-file problem (line 0).
    fn place_rule_id(&self) -> &str;
    fn place_severity(&self) -> &str;
    /// `"do_now"` | `"do_next"` | `"plan"` | `"accepted"` | `"informational"` | `"held"`.
    fn place_bucket(&self) -> &str;
}

/// The fix order a place is recommended in: the earliest of its rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FixOrder {
    First,
    Next,
    Later,
}

/// The one class of a place. The order of the variants is the FAIL CLOSED precedence: a place with
/// any held row needs a decision whatever else is there, then a place with any row to act on is
/// for action, then information only, and a place whose every row is accepted is accepted risk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceClass {
    NeedsDecision,
    ForAction(FixOrder),
    InformationOnly,
    AcceptedRisk,
}

/// One place: the row that heads it, every row at it (the head first) and its class.
#[derive(Debug)]
pub struct Place<'a, T> {
    pub head: &'a T,
    pub members: Vec<&'a T>,
    pub class: PlaceClass,
}

fn severity_order(sev: &str) -> u8 {
    camerata_rules::severity_bounds::severity_rank(sev)
}

/// The class of one row's bucket. An unknown bucket needs a decision: a row the rule does not
/// know is never counted as settled.
fn class_of_bucket(bucket: &str) -> PlaceClass {
    match bucket {
        "do_now" => PlaceClass::ForAction(FixOrder::First),
        "do_next" => PlaceClass::ForAction(FixOrder::Next),
        "plan" => PlaceClass::ForAction(FixOrder::Later),
        "informational" => PlaceClass::InformationOnly,
        "accepted" => PlaceClass::AcceptedRisk,
        _ => PlaceClass::NeedsDecision,
    }
}

fn precedence(c: PlaceClass) -> (u8, u8) {
    match c {
        PlaceClass::NeedsDecision => (0, 0),
        PlaceClass::ForAction(FixOrder::First) => (1, 0),
        PlaceClass::ForAction(FixOrder::Next) => (1, 1),
        PlaceClass::ForAction(FixOrder::Later) => (1, 2),
        PlaceClass::InformationOnly => (2, 0),
        PlaceClass::AcceptedRisk => (3, 0),
    }
}

/// The class of a place from the buckets of all its rows (fail closed, see [`PlaceClass`]).
pub fn class_of_place<'b>(buckets: impl IntoIterator<Item = &'b str>) -> PlaceClass {
    buckets
        .into_iter()
        .map(class_of_bucket)
        .min_by_key(|c| precedence(*c))
        .unwrap_or(PlaceClass::NeedsDecision)
}

/// The one class of a Fix, and so the one order it is done in. The variants are in print and
/// plan order: First, Next, Later, then the classes with nothing to act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FixClass {
    /// The earliest order among the Fix's places that are for action.
    Order(FixOrder),
    /// No place for action, and at least one place needs a decision.
    NeedsDecision,
    /// No place for action and none needs a decision, at least one is information only.
    InformationOnly,
    /// Every place is accepted risk.
    AcceptedRisk,
}

/// THE Fix order rule, and the only place an order is derived from places: a Fix's order is the
/// earliest order among its places that are for action ([`class_of_place`] gives each place's
/// class). A Fix with no place for action has the class of its places instead (needs a decision,
/// then information only, then accepted risk) and never an order. Every surface (the PDF, the
/// workbook, the tickets, the counts) takes a Fix's order from here.
pub fn class_of_fix(place_classes: impl IntoIterator<Item = PlaceClass>) -> FixClass {
    place_classes
        .into_iter()
        .map(FixClass::from)
        .min()
        // A Fix with no place at all is one nobody has settled: it needs a decision.
        .unwrap_or(FixClass::NeedsDecision)
}

impl From<PlaceClass> for FixClass {
    /// A place's class as the Fix class it would give a Fix of that one place.
    fn from(c: PlaceClass) -> Self {
        match c {
            PlaceClass::ForAction(order) => Self::Order(order),
            PlaceClass::NeedsDecision => Self::NeedsDecision,
            PlaceClass::InformationOnly => Self::InformationOnly,
            PlaceClass::AcceptedRisk => Self::AcceptedRisk,
        }
    }
}

/// The key of a place: (repo, path, line), and for a whole-file problem (line 0) the rule id as
/// well, so the whole-file problems of different rules in one file are separate places. A
/// whole-file row and a line row never share a place.
pub fn place_key<'a>(
    repo: &'a str,
    path: &'a str,
    line: usize,
    rule_id: &'a str,
) -> (&'a str, &'a str, usize, &'a str) {
    (repo, path, line, if line == 0 { rule_id } else { "" })
}

/// THE place rule: one place for each [`place_key`] of the problem rows given, in the order the
/// places first appear. The head is picked by a total order that never depends on the order the
/// rows arrive in: the worst severity, then the earliest fix order (a held row ranks first), then
/// the stable key (repo, path, line, rule id). The caller passes problem rows only (it knows what
/// a problem is: a defect or a departure that is not ruled out); this function never drops a row.
pub fn places_of<'a, T: PlaceRow>(rows: impl IntoIterator<Item = &'a T>) -> Vec<Place<'a, T>> {
    type Key<'k> = (&'k str, &'k str, usize, &'k str);
    let mut order: Vec<Key<'a>> = Vec::new();
    let mut at: std::collections::HashMap<Key<'a>, Vec<&'a T>> = std::collections::HashMap::new();
    for r in rows {
        let key = place_key(
            r.place_repo(),
            r.place_path(),
            r.place_line(),
            r.place_rule_id(),
        );
        at.entry(key)
            .or_insert_with(|| {
                order.push(key);
                Vec::new()
            })
            .push(r);
    }
    let rank = |m: &&'a T| {
        (
            severity_order(m.place_severity()),
            precedence(class_of_bucket(m.place_bucket())),
            m.place_repo(),
            m.place_path(),
            m.place_line(),
            m.place_rule_id(),
        )
    };
    order
        .into_iter()
        .filter_map(|k| at.remove(&k))
        .map(|mut members| {
            members.sort_by(|a, b| rank(a).cmp(&rank(b)));
            let head = members[0];
            let class = class_of_place(members.iter().map(|m| m.place_bucket()));
            Place {
                head,
                members,
                class,
            }
        })
        .collect()
}
