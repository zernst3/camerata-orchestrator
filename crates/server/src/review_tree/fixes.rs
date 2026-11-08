//! The Fix (item #1, design R-4): a set of sub-groups whose places are one change.
//!
//! A sub-group never crosses a rule family, so one committed secret can sit in five sub-groups
//! (a hardcoded secret, a vendor token, a service-role client, a public env prefix, an AI secret
//! exposure) and a sixth for its file. The place is the only key that joins them. A Fix is worked
//! out ONCE, from the rows and the tree alone: never from a decision, so a Fix id is the same
//! before, during and after the review.
//!
//! # The merge key
//!
//! A sub-group's place set is the (repo, file, line) of every row of it that says something is
//! wrong. Two sub-groups merge when their place sets are equal or one is contained in the other:
//! the smaller folds into the smallest set that contains it, and a chain folds into its largest
//! set. Overlap never merges, so a 50-place sub-group that shares one place with another cannot
//! swallow it and nothing chains through a shared place. A sub-group with two containing sets that
//! do not contain each other stays its own Fix (fail closed toward duplicates). A dependency
//! sub-group and a sub-group of observations (conventions, rules that could not be completed)
//! never merge.
//!
//! A whole-file place (line 0) of a file contains every line place of that file, but only when the
//! sub-group of the whole file is at least as severe as the worst row of the sub-group of lines: a
//! critical "committed `.env.production`" absorbs line 6 of it, a low "license header" never
//! absorbs a critical secret.
//!
//! Nothing here removes, hides or changes a row. The merge is a ledger entry
//! ([`FixMergeBlock`]), not a decision record.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use super::{is_thrown_out_id, NodeLevel, ReviewTree};
use crate::onboard::Finding;

/// The id prefix of a node-scope decision on a Fix (`fix:<id>`).
pub const FIX_PREFIX: &str = "fix:";

/// The ledger stage name of the merge.
pub const FIX_MERGE_STAGE: &str = "fix-merge";

/// One place: a file and line of a repo (line 0 is the whole file).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FixPlace {
    pub repo: String,
    pub path: String,
    pub line: usize,
}

/// A place two Fixes both list without one containing the other (a true partial overlap).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixShare {
    /// The other Fix.
    pub fix: String,
    /// How many places the two have in common.
    pub places: usize,
}

/// One Fix: the sub-groups that are one change, and everything it covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixNode {
    /// Twelve hex digits, the same for the same members on every run.
    pub id: String,
    /// The sub-group node ids, sorted. The first is the Fix's anchor; the rest were folded in.
    pub members: Vec<String>,
    /// The union of the members' places, sorted.
    pub places: Vec<FixPlace>,
    /// Every rule id of the members' rows, sorted.
    pub rules: Vec<String>,
    /// The Fixes this one shares a place with and does not contain.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shares: Vec<FixShare>,
}

impl FixNode {
    /// The node-scope decision id of this Fix.
    pub fn node_id(&self) -> String {
        format!("{FIX_PREFIX}{}", self.id)
    }
}

/// The merge as the ledger records it: sub-groups in equals Fixes out plus those folded.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixMergeBlock {
    pub stage: String,
    pub sub_groups_in: usize,
    pub fixes_out: usize,
    /// Each folded sub-group and the Fix it was folded into.
    pub folded: Vec<(String, String)>,
}

impl FixMergeBlock {
    /// Sub-groups in equals Fixes out plus those folded.
    pub fn is_integral(&self) -> bool {
        self.sub_groups_in == self.fixes_out + self.folded.len()
    }

    /// The block for `fixes` over `sub_groups_in` sub-groups.
    pub fn of(fixes: &[FixNode], sub_groups_in: usize) -> Self {
        Self {
            stage: FIX_MERGE_STAGE.to_string(),
            sub_groups_in,
            fixes_out: fixes.len(),
            folded: fixes
                .iter()
                .flat_map(|fx| {
                    fx.members
                        .iter()
                        .skip(1)
                        .map(|m| (m.clone(), fx.id.clone()))
                })
                .collect(),
        }
    }
}

/// The id of a Fix of `members` (sub-group ids, in any order).
pub fn fix_id_of(members: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let mut sorted: Vec<&str> = members.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    Sha256::digest(sorted.join("\n").as_bytes())
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// One sub-group as the merge sees it.
struct Candidate {
    id: String,
    places: BTreeSet<FixPlace>,
    /// Rank of the worst row (0 is critical); smaller is worse.
    worst: u8,
    mergeable: bool,
    rules: BTreeSet<String>,
}

impl Candidate {
    fn whole_files(&self) -> BTreeSet<(&str, &str)> {
        self.places
            .iter()
            .filter(|p| p.line == 0)
            .map(|p| (p.repo.as_str(), p.path.as_str()))
            .collect()
    }

    /// Whether `self` contains every place of `other`, a whole-file place counting for the lines
    /// of its file when `self` is at least as severe as `other`'s worst row.
    fn covers(&self, other: &Self) -> bool {
        let files = self.whole_files();
        other.places.iter().all(|p| {
            self.places.contains(p)
                || (p.line > 0
                    && self.worst <= other.worst
                    && files.contains(&(p.repo.as_str(), p.path.as_str())))
        })
    }
}

fn severity_rank(severity: &str) -> u8 {
    match severity.trim().to_ascii_lowercase().as_str() {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        _ => 3,
    }
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

fn place_of(f: &Finding) -> FixPlace {
    FixPlace {
        repo: f.repo.clone(),
        path: f.path.clone(),
        line: f.line,
    }
}

/// The Fixes of `findings` over the sub-groups of `tree`, sorted by their first member. Pure and
/// deterministic. Every sub-group of the tree is in exactly one Fix, a sub-group with no rows
/// included.
pub fn merge_fixes(tree: &ReviewTree, findings: &[Finding]) -> Vec<FixNode> {
    let mut rows_of: HashMap<&str, Vec<&Finding>> = HashMap::new();
    for f in findings {
        if let Some(sub) = f.group_path.get(2) {
            rows_of.entry(sub.as_str()).or_default().push(f);
        }
    }
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut all_places: HashMap<String, BTreeSet<FixPlace>> = HashMap::new();
    let mut subs: Vec<&str> = tree
        .nodes
        .iter()
        .filter(|n| n.level == NodeLevel::Subgroup && !is_thrown_out_id(&n.id))
        .map(|n| n.id.as_str())
        .collect();
    subs.sort_unstable();
    subs.dedup();
    for id in subs {
        let rows = rows_of.get(id).map(Vec::as_slice).unwrap_or_default();
        let is_dependency = rows
            .iter()
            .any(|f| f.rule_id == crate::dep_audit::DEP_AUDIT_RULE_ID);
        let problems: Vec<&&Finding> = rows.iter().filter(|f| is_problem(f)).collect();
        let places: BTreeSet<FixPlace> = problems.iter().map(|f| place_of(f)).collect();
        all_places.insert(
            id.to_string(),
            rows.iter().map(|f| place_of(f)).collect::<BTreeSet<_>>(),
        );
        candidates.push(Candidate {
            id: id.to_string(),
            worst: problems
                .iter()
                .map(|f| severity_rank(&f.severity))
                .min()
                .unwrap_or(3),
            mergeable: !is_dependency && !places.is_empty(),
            places,
            rules: rows.iter().map(|f| f.rule_id.clone()).collect(),
        });
    }

    // Sub-groups with exactly the same places are one class from the start.
    let mut class_of: Vec<usize> = vec![0; candidates.len()];
    let mut classes: Vec<Vec<usize>> = Vec::new();
    let mut by_places: BTreeMap<&BTreeSet<FixPlace>, usize> = BTreeMap::new();
    for (i, c) in candidates.iter().enumerate() {
        if !c.mergeable {
            class_of[i] = classes.len();
            classes.push(vec![i]);
            continue;
        }
        let at = *by_places.entry(&c.places).or_insert(classes.len());
        if at == classes.len() {
            classes.push(Vec::new());
        }
        classes[at].push(i);
        class_of[i] = at;
    }
    // A class's stand-in is its first sub-group, with the worst severity of all of them.
    let stand_in = |class: usize| -> Candidate {
        let first = &candidates[classes[class][0]];
        Candidate {
            id: first.id.clone(),
            places: first.places.clone(),
            worst: classes[class]
                .iter()
                .map(|&i| candidates[i].worst)
                .min()
                .unwrap_or(3),
            mergeable: first.mergeable,
            rules: BTreeSet::new(),
        }
    };
    let reps: Vec<Candidate> = (0..classes.len()).map(stand_in).collect();

    // Each class folds into the smallest class that contains it, when there is exactly one.
    let mut parent: Vec<Option<usize>> = vec![None; classes.len()];
    for s in 0..classes.len() {
        if !reps[s].mergeable {
            continue;
        }
        let containers: Vec<usize> = (0..classes.len())
            .filter(|&t| {
                t != s && reps[t].mergeable && reps[t].covers(&reps[s]) && !reps[s].covers(&reps[t])
            })
            .collect();
        let minimal: Vec<usize> = containers
            .iter()
            .copied()
            .filter(|&t| {
                !containers
                    .iter()
                    .any(|&u| u != t && reps[t].covers(&reps[u]) && !reps[u].covers(&reps[t]))
            })
            .collect();
        if let [only] = minimal.as_slice() {
            parent[s] = Some(*only);
        }
    }
    let root_of = |start: usize| -> usize {
        let mut at = start;
        let mut seen = BTreeSet::new();
        while let Some(next) = parent[at] {
            if !seen.insert(at) {
                break;
            }
            at = next;
        }
        at
    };

    let mut by_root: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for class in 0..classes.len() {
        by_root.entry(root_of(class)).or_default().push(class);
    }
    let mut fixes: Vec<FixNode> = by_root
        .into_values()
        .map(|group| {
            let mut members: Vec<String> = group
                .iter()
                .flat_map(|&class| classes[class].iter().map(|&i| candidates[i].id.clone()))
                .collect();
            members.sort();
            let places: BTreeSet<FixPlace> = members
                .iter()
                .flat_map(|m| all_places.get(m).into_iter().flatten().cloned())
                .collect();
            let rules: BTreeSet<String> = group
                .iter()
                .flat_map(|&class| classes[class].iter())
                .flat_map(|&i| candidates[i].rules.iter().cloned())
                .collect();
            FixNode {
                id: fix_id_of(&members),
                members,
                places: places.into_iter().collect(),
                rules: rules.into_iter().collect(),
                shares: Vec::new(),
            }
        })
        .collect();
    fixes.sort_by(|a, b| a.members.cmp(&b.members));

    // Fixes that list one place between them and do not contain each other say so, both ways.
    let mut at_place: BTreeMap<&FixPlace, Vec<usize>> = BTreeMap::new();
    for (i, fx) in fixes.iter().enumerate() {
        for p in &fx.places {
            at_place.entry(p).or_default().push(i);
        }
    }
    let mut shared: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for list in at_place.values() {
        for (n, &a) in list.iter().enumerate() {
            for &b in &list[n + 1..] {
                *shared.entry((a, b)).or_default() += 1;
            }
        }
    }
    for ((a, b), places) in shared {
        let (ida, idb) = (fixes[a].id.clone(), fixes[b].id.clone());
        fixes[a].shares.push(FixShare { fix: idb, places });
        fixes[b].shares.push(FixShare { fix: ida, places });
    }
    fixes
}

impl ReviewTree {
    /// Work the Fixes out again from `findings` and store them. Returns what the ledger records.
    pub fn refresh_fixes(&mut self, findings: &[Finding]) -> FixMergeBlock {
        self.fixes = merge_fixes(self, findings);
        self.fix_merge_block()
    }

    /// The merge as the ledger records it: the sub-groups of the tree in, the stored Fixes out.
    pub fn fix_merge_block(&self) -> FixMergeBlock {
        let sub_groups = self
            .nodes
            .iter()
            .filter(|n| n.level == NodeLevel::Subgroup && !is_thrown_out_id(&n.id))
            .count();
        FixMergeBlock::of(&self.fixes, sub_groups)
    }

    /// The Fix a sub-group is in.
    pub fn fix_of(&self, subgroup_id: &str) -> Option<&FixNode> {
        self.fixes
            .iter()
            .find(|f| f.members.iter().any(|m| m == subgroup_id))
    }

    /// The Fix with this id, or `fix:<id>`.
    pub fn fix(&self, id: &str) -> Option<&FixNode> {
        let bare = id.strip_prefix(FIX_PREFIX).unwrap_or(id);
        self.fixes.iter().find(|f| f.id == bare)
    }

    /// Whether a decision can be made at this id: a tree node or a Fix.
    pub fn is_decidable(&self, id: &str) -> bool {
        match id.strip_prefix(FIX_PREFIX) {
            Some(bare) => self.fixes.iter().any(|f| f.id == bare),
            None => self.node(id).is_some(),
        }
    }
}

/// Which Fix each sub-group is in, as an export reads it: the tree's stored Fixes when they cover
/// every sub-group, else worked out here from the rows (a report stored before Fixes existed).
#[derive(Debug, Clone, Default)]
pub struct FixIndex {
    by_subgroup: HashMap<String, String>,
    fixes: Vec<FixNode>,
}

impl FixIndex {
    pub fn new(tree: Option<&ReviewTree>, findings: &[Finding]) -> Self {
        let Some(tree) = tree else {
            return Self::default();
        };
        let covers_all = tree
            .nodes
            .iter()
            .filter(|n| n.level == NodeLevel::Subgroup && !is_thrown_out_id(&n.id))
            .all(|n| tree.fix_of(&n.id).is_some());
        let fixes = if covers_all && !tree.fixes.is_empty() {
            tree.fixes.clone()
        } else {
            merge_fixes(tree, findings)
        };
        let by_subgroup = fixes
            .iter()
            .flat_map(|fx| fx.members.iter().map(|m| (m.clone(), fx.id.clone())))
            .collect();
        Self { by_subgroup, fixes }
    }

    /// The Fix id of a row, or empty for a row in no sub-group.
    pub fn fix_id_of(&self, f: &Finding) -> String {
        f.group_path
            .get(2)
            .and_then(|sub| self.by_subgroup.get(sub))
            .cloned()
            .unwrap_or_default()
    }

    /// The Fix id of a sub-group.
    pub fn fix_of_subgroup(&self, subgroup_id: &str) -> Option<&str> {
        self.by_subgroup.get(subgroup_id).map(String::as_str)
    }

    pub fn fixes(&self) -> &[FixNode] {
        &self.fixes
    }
}
