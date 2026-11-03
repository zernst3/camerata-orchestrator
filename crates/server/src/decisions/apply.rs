//! Reading the store: which decisions apply to a run's sites, what to show beside the ones that
//! do not, and the two views other code takes of the same answer (the release gate's question
//! and the export's dispositions).

use std::collections::{BTreeMap, HashMap, HashSet};

use super::{
    effective_by_key, AppliedDecision, Decision, DecisionState, DecisionStore, NewSinceDecision,
    Scope, SiteStanding, StaleDecision,
};
use crate::onboard::Finding;
use crate::release_gate::evidence::ReviewerDecisions;
use crate::report_export::{finding_key, DispositionWire};
use crate::review_tree::{KeyLink, LinkHow, ReviewTree};

/// What the decisions say about the sites of one run. Only a site with something to report has
/// an entry; every other site is Unresolved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppliedDecisions {
    /// Sorted by site key.
    pub sites: Vec<SiteStanding>,
}

impl AppliedDecisions {
    /// The standing of one site, if it has one.
    pub fn get(&self, key: &str) -> Option<&SiteStanding> {
        self.sites
            .binary_search_by(|s| s.key.as_str().cmp(key))
            .ok()
            .map(|i| &self.sites[i])
    }
}

/// Read the moved links a run wrote to `key_links.json` ([`crate::review_tree::RunMatch::
/// write_key_links`]).
pub fn read_key_links(path: &std::path::Path) -> std::io::Result<Vec<KeyLink>> {
    let text = std::fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(std::io::Error::other)
}

/// Apply the store's decisions for `project_id` to `tree`. `links` are the moved sites between
/// the run a decision was made on and this one (what [`crate::review_tree::match_runs`] found,
/// as written to `key_links.json`).
pub fn apply_decisions(
    tree: &ReviewTree,
    store: &DecisionStore,
    project_id: &str,
    links: &[KeyLink],
) -> AppliedDecisions {
    apply_to(tree, &store.active(project_id), links)
}

/// [`apply_decisions`] over records already read from the store (revoked ones are ignored).
///
/// - A decision applies to a site only on an EXACT key match.
/// - A member of a decided node that is not in the node decision's `applies_to` is Unresolved and
///   carries the `new_since_decision` marker, unless a later ruling decides it or it is the moved
///   successor of a site the node decision did cover.
/// - A site that moved shows the prior decision of its old key as stale and stays Unresolved
///   (D10). If the new key has a decision of its own, that one wins and nothing stale is shown.
pub fn apply_to(tree: &ReviewTree, decisions: &[Decision], links: &[KeyLink]) -> AppliedDecisions {
    let by_key = effective_by_key(decisions);
    let current: HashSet<&str> = tree
        .nodes
        .iter()
        .flat_map(|n| n.members.iter().map(String::as_str))
        .collect();
    let mut sites: BTreeMap<String, SiteStanding> = BTreeMap::new();

    for key in &current {
        if let Some(d) = by_key.get(key) {
            sites
                .entry((*key).to_string())
                .or_insert_with(|| blank(key))
                .decision = Some(AppliedDecision {
                decision_id: d.id.clone(),
                state: d.state.clone(),
                reason: d.reason.clone(),
                inherited_from: d.inherited_from.clone(),
            });
        }
    }

    // Members a group decision did not cover.
    let moved_from: HashMap<&str, &str> = links
        .iter()
        .filter(|l| l.how == LinkHow::Moved)
        .map(|l| (l.new_key.as_str(), l.old_key.as_str()))
        .collect();
    let mut newest_marker: HashMap<String, &Decision> = HashMap::new();
    for rec in decisions
        .iter()
        .filter(|d| d.is_active() && d.is_node_record())
    {
        let Scope::Node(node_id) = &rec.scope else {
            continue;
        };
        if tree.node(node_id).is_none() {
            continue;
        }
        let covered: HashSet<&str> = rec.applies_to.iter().map(String::as_str).collect();
        let left_out: HashSet<&str> = rec.except.iter().map(String::as_str).collect();
        for member in tree.batch_members(node_id) {
            // A member the decision left out on purpose is not new: it was seen and set aside.
            if covered.contains(member.as_str())
                || left_out.contains(member.as_str())
                || by_key.contains_key(member.as_str())
            {
                continue;
            }
            // The successor of a covered site is a moved site, shown stale below, not a new one.
            if moved_from
                .get(member.as_str())
                .is_some_and(|old| covered.contains(old))
            {
                continue;
            }
            newest_marker.insert(member, rec);
        }
    }
    for (key, rec) in newest_marker {
        let Scope::Node(node_id) = &rec.scope else {
            continue;
        };
        sites
            .entry(key.clone())
            .or_insert_with(|| blank(&key))
            .new_since_decision = Some(NewSinceDecision {
            decision_id: rec.id.clone(),
            node_id: node_id.clone(),
            decided_at: rec.decided_at.clone(),
        });
    }

    // Moved sites: the prior decision, shown and not applied.
    for link in links.iter().filter(|l| l.how == LinkHow::Moved) {
        let (old, new) = (link.old_key.as_str(), link.new_key.as_str());
        if !current.contains(new) || current.contains(old) || by_key.contains_key(new) {
            continue;
        }
        let Some(d) = by_key.get(old) else {
            continue;
        };
        let entry = sites.entry(new.to_string()).or_insert_with(|| blank(new));
        if entry.stale.is_none() {
            entry.stale = Some(StaleDecision {
                decision_id: d.id.clone(),
                from_key: old.to_string(),
                state: d.state.clone(),
                reason: d.reason.clone(),
                decided_at: d.decided_at.clone(),
                inherited_from: d.inherited_from.clone(),
            });
        }
    }

    AppliedDecisions {
        sites: sites.into_values().collect(),
    }
}

fn blank(key: &str) -> SiteStanding {
    SiteStanding {
        key: key.to_string(),
        decision: None,
        new_since_decision: None,
        stale: None,
    }
}

/// The release gate's question, answered from the store: has the reviewer decided this row? A
/// row is decided when a decision applies to its site key. The gate does not read this yet.
#[derive(Debug, Clone)]
pub struct StoreDecisions {
    decided: HashSet<String>,
}

impl StoreDecisions {
    /// The decided keys of `project_id` as of now.
    pub fn for_project(store: &DecisionStore, project_id: &str) -> Self {
        Self::from_records(&store.active(project_id))
    }

    /// The decided keys of `records` (revoked ones are ignored). A restore of a thrown-out
    /// finding decides nothing; a merge does.
    pub fn from_records(records: &[Decision]) -> Self {
        let decided = effective_by_key(records)
            .keys()
            .map(|k| (*k).to_string())
            .collect();
        Self { decided }
    }
}

impl ReviewerDecisions for StoreDecisions {
    fn has_decision(&self, finding: &Finding) -> bool {
        self.decided.contains(&finding_key(finding))
    }
}

/// The dispositions an export uses, and where they came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffectiveDispositions {
    pub map: HashMap<String, DispositionWire>,
    /// True when the store held a live record for at least one key.
    pub from_store: bool,
    /// Where the request's dispositions and the store's differ (empty unless `from_store`).
    pub disagreements: Vec<DispositionDisagreement>,
}

/// One key on which the request and the store disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispositionDisagreement {
    pub key: String,
    pub request: Option<String>,
    pub store: Option<String>,
}

/// The dispositions an export should use. For one release the export request still carries
/// `dispositions`, for an older client: they are used only while the store holds no live record
/// for the project. Once it does, the decisions come from the store only, a key the store has no
/// record for is undecided, and every key both decided differently is reported.
pub fn effective_dispositions(
    store: &DecisionStore,
    project_id: &str,
    from_request: &HashMap<String, DispositionWire>,
) -> EffectiveDispositions {
    let all = store.active(project_id);
    let from_store = wire_dispositions(&all);
    // Once the store holds a live record, the decisions are the store's alone: a key the store
    // has no record for (or revoked) is undecided, whatever an older client's request says. The
    // request's own dispositions are read only while the store holds nothing for the project.
    let store_keys: HashSet<&str> = effective_by_key(&all).into_keys().collect();
    let map = if all.iter().any(Decision::is_active) {
        from_store.clone()
    } else {
        from_request.clone()
    };

    // A disagreement is a key both sides decided, differently.
    let mut keys: Vec<&String> = from_store.keys().collect();
    keys.sort();
    let disagreements = keys
        .into_iter()
        .filter_map(|key| {
            let theirs = from_request.get(key).and_then(decision_label)?;
            let ours = from_store.get(key).and_then(decision_label);
            (Some(&theirs) != ours.as_ref()).then(|| DispositionDisagreement {
                key: key.clone(),
                request: Some(theirs.0),
                store: ours.map(|(state, _)| state),
            })
        })
        .collect();
    EffectiveDispositions {
        map,
        from_store: !store_keys.is_empty(),
        disagreements,
    }
}

impl EffectiveDispositions {
    /// Log what the request's dispositions would have changed. A disagreement means the UI's copy
    /// and the store's have drifted; the store's is what was used.
    pub fn log_disagreements(&self, project_id: &str) {
        if self.disagreements.is_empty() {
            return;
        }
        let sample: Vec<&str> = self
            .disagreements
            .iter()
            .take(5)
            .map(|d| d.key.as_str())
            .collect();
        tracing::warn!(
            project = %project_id,
            count = self.disagreements.len(),
            first_keys = ?sample,
            "the export request's dispositions disagree with the decision store; \
             the store's decisions were used"
        );
    }
}

/// The dispositions the exporters read, by site key, from decision records (revoked ones are
/// ignored). A merge and a restore have no wire disposition: the overlay gives a merged row its
/// pointer, and a restore decides nothing.
pub fn wire_dispositions(records: &[Decision]) -> HashMap<String, DispositionWire> {
    effective_by_key(records)
        .into_iter()
        .filter_map(|(key, d)| wire_of(d).map(|w| (key.to_string(), w)))
        .collect()
}

/// A disposition as the export reads a decision: its state, and the bucket when it is tech debt.
/// `None` for an entry that is not a decision (Unresolved, empty or unknown).
fn decision_label(w: &DispositionWire) -> Option<(String, String)> {
    if !DECISION_STATES.contains(&w.state.as_str()) {
        return None;
    }
    let bucket = if w.state == "TechDebt" {
        w.bucket.clone()
    } else {
        String::new()
    };
    Some((w.state.clone(), bucket))
}

/// The states `report_export` reads as a decision.
const DECISION_STATES: [&str; 4] = ["Confirmed", "Ignored", "TechDebt", "FalsePositive"];

/// The export's disposition for a stored decision. A merge has none: the export gives a merged
/// row its pointer through the overlay (a later work order).
fn wire_of(d: &Decision) -> Option<DispositionWire> {
    let (state, bucket) = match &d.state {
        DecisionState::Confirmed => ("Confirmed", "Later"),
        DecisionState::Ignored => ("Ignored", "Later"),
        DecisionState::FalsePositive => ("FalsePositive", "Later"),
        DecisionState::TechDebtNow => ("TechDebt", "Now"),
        DecisionState::TechDebtLater => ("TechDebt", "Later"),
        DecisionState::MergedInto(_) | DecisionState::EngineRestored => return None,
    };
    Some(DispositionWire {
        state: state.to_string(),
        reason: d.reason.clone(),
        bucket: bucket.to_string(),
        confirmed_by_client: d.confirmed_by_client.is_some(),
    })
}
