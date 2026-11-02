//! The reviewer's decisions on sites, and the request and response shapes of the decision routes
//! (review-tree design, sections 3.5 and 3.6; amendment A6 step 2).
//!
//! A decision is what the reviewer ruled on a site: confirmed, ignored, false positive, tech debt
//! or merged into another site. It lives on the server, in one file per project beside the
//! sealed report, never inside it. The server (`camerata_server::decisions`) and the review UI
//! share these types, so a route's real JSON output is the UI's input by construction; the
//! round-trip and wire-shape tests in the server pin that.
//!
//! # Node decisions
//!
//! A decision at a tree node ([`Scope::Node`]) writes the node record plus one per-site record
//! per member, each carrying [`Decision::inherited_from`], so every site holds the decision and
//! its reason in the file. The node record's `key` is the node id and its `applies_to` is every
//! member key at the time of the decision. A per-site record is [`Scope::Site`] with
//! `inherited_from` set; "personally reviewed" is a site-scope record with no `inherited_from`.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// What the reviewer ruled on a site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionState {
    /// A real finding, agreed with.
    Confirmed,
    /// Accepted risk: not fixed, not disputed.
    Ignored,
    /// The tool was wrong.
    FalsePositive,
    /// Real, to be fixed now (pulled into the dev engine).
    TechDebtNow,
    /// Real, scheduled for later (a tracked ticket).
    TechDebtLater,
    /// The same problem as the site with this key. The row is kept and exports with a pointer.
    MergedInto(String),
}

/// Whether a decision was made on one site or at a tree node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Site,
    /// A decision on the tree node with this id (`cat:...`, `rule:...`, `sub:...`).
    Node(String),
}

/// What the Engine Check's reviewer said about an unread row (design 3A.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleVerdict {
    /// A duplicate of the row with this key.
    Duplicate(String),
    FalseAlarm,
    Fine,
}

/// A client's acknowledgement of an Ignored row, set only after delivery (design 5.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientAck {
    pub by: String,
    pub at: String,
    pub note: String,
}

/// One stored decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub id: String,
    /// The site key. On a node record, the node id.
    pub key: String,
    pub state: DecisionState,
    /// Required unless this is a site-scope Confirmed (and always for a node decision).
    pub reason: String,
    pub scope: Scope,
    /// Node record only: every member key the decision covers (one per-site record each). The
    /// members at the time of the decision, less `except`.
    #[serde(default)]
    pub applies_to: Vec<String>,
    /// Node record only: members the decision deliberately left out (unticked outliers, held
    /// rows). They have no per-site record, stay Unresolved, are not "new since the decision"
    /// and are never touched by revoking the node.
    #[serde(default)]
    pub except: Vec<String>,
    /// The node decision this per-site record came from.
    #[serde(default)]
    pub inherited_from: Option<String>,
    pub decided_by: String,
    pub decided_at: String,
    /// The inspection (entry id) the decision was made against.
    pub run_id: String,
    pub engine_version: String,
    /// Escalated sites: whether the reviewer needed to see it (design 4.7).
    #[serde(default)]
    pub needed_me: Option<bool>,
    #[serde(default)]
    pub sample_verdict: Option<SampleVerdict>,
    #[serde(default)]
    pub confirmed_by_client: Option<ClientAck>,
    #[serde(default)]
    pub revoked_at: Option<String>,
}

impl Decision {
    /// A record that has not been revoked.
    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }

    /// Whether this is a node-scope record (the one that holds `applies_to`).
    pub fn is_node_record(&self) -> bool {
        matches!(self.scope, Scope::Node(_))
    }
}

/// Every live site-level record of each site key, oldest first: records not revoked and not
/// node records, and a record a group decision wrote only while its node record is live and
/// lists the key in `applies_to`.
pub fn live_by_key(decisions: &[Decision]) -> HashMap<&str, Vec<&Decision>> {
    let nodes: HashMap<&str, HashSet<&str>> = decisions
        .iter()
        .filter(|d| d.is_active() && d.is_node_record())
        .map(|d| {
            (
                d.id.as_str(),
                d.applies_to.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    let mut by_key: HashMap<&str, Vec<&Decision>> = HashMap::new();
    for d in decisions.iter().filter(|d| d.is_active()) {
        if d.is_node_record() {
            continue;
        }
        if let Some(parent) = &d.inherited_from {
            let covered = nodes
                .get(parent.as_str())
                .is_some_and(|keys| keys.contains(d.key.as_str()));
            if !covered {
                continue;
            }
        }
        by_key.entry(d.key.as_str()).or_default().push(d);
    }
    by_key
}

/// The decision each site key currently has: the LATEST of its live records (see
/// [`live_by_key`]). The server and the UI both work out a site's decision with this function.
pub fn effective_by_key(decisions: &[Decision]) -> HashMap<&str, &Decision> {
    live_by_key(decisions)
        .into_iter()
        .filter_map(|(key, records)| records.last().map(|d| (key, *d)))
        .collect()
}

// ── Requests and responses ────────────────────────────────────────────────────────────────

/// `POST /api/projects/:id/decisions` with one new decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewDecisionReq {
    /// Site scope: the site key is `key`. Node scope: the node id is inside the scope and `key`
    /// is ignored.
    pub scope: Scope,
    #[serde(default)]
    pub key: String,
    pub state: DecisionState,
    #[serde(default)]
    pub reason: String,
    /// Who decided; blank is recorded as `reviewer`.
    #[serde(default)]
    pub decided_by: String,
    #[serde(default)]
    pub needed_me: Option<bool>,
    #[serde(default)]
    pub sample_verdict: Option<SampleVerdict>,
    /// Node scope: site keys to leave out of the decision. Held rows are left out by the server
    /// whether or not they are listed.
    #[serde(default)]
    pub except: Vec<String>,
}

/// One disposition as the UI's onboarding draft held it. The states and buckets are the strings
/// the draft was saved with (`Confirmed`, `Ignored`, `TechDebt`, `FalsePositive`; `Later`, `Now`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftDisposition {
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub bucket: String,
}

/// `POST /api/projects/:id/decisions` with the dispositions the UI draft held, once, on the first
/// load after the upgrade (design 3.6). `migrated_from_draft` must be `true`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrateDraftReq {
    pub migrated_from_draft: bool,
    pub dispositions: BTreeMap<String, DraftDisposition>,
}

/// The body of `POST /api/projects/:id/decisions`: a migration or one new decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PostDecisionsReq {
    Migrate(MigrateDraftReq),
    Decide(NewDecisionReq),
}

/// The reply to one new decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionRecorded {
    pub decision: Decision,
    /// Node decision: how many per-site records it wrote. Zero for a site decision.
    pub inherited_count: usize,
}

/// The reply to a draft migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationOutcome {
    /// True when the project was already migrated: nothing was written.
    pub already_migrated: bool,
    /// How many records the migration wrote.
    pub migrated: usize,
    /// Old keys no current row matches; kept in the store under the old key, for history.
    pub unmatched: Vec<String>,
    /// Entries the draft held that are not decisions (Unresolved, or a state this build does not
    /// know); not migrated.
    pub skipped: usize,
    /// The migrated dispositions by current site key, so the UI can carry on with its session.
    pub dispositions: BTreeMap<String, DraftDisposition>,
}

/// A decision that applies to a site on the current run by an exact key match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedDecision {
    pub decision_id: String,
    pub state: DecisionState,
    pub reason: String,
    pub inherited_from: Option<String>,
}

/// A member of a decided node that was not in the node decision's `applies_to`: it is new since
/// the decision and stays Unresolved ("12 new since you confirmed this group on 11-04").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewSinceDecision {
    pub decision_id: String,
    pub node_id: String,
    pub decided_at: String,
}

/// The prior decision of a site that moved (or whose snippet changed): shown with the prior
/// value, never applied (D10). The site stays Unresolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleDecision {
    pub decision_id: String,
    /// The key the decision was made under.
    pub from_key: String,
    pub state: DecisionState,
    pub reason: String,
    pub decided_at: String,
    pub inherited_from: Option<String>,
}

/// What the decisions say about one site of the current run. A site with nothing to report has
/// no standing and is Unresolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteStanding {
    pub key: String,
    #[serde(default)]
    pub decision: Option<AppliedDecision>,
    #[serde(default)]
    pub new_since_decision: Option<NewSinceDecision>,
    #[serde(default)]
    pub stale: Option<StaleDecision>,
}

impl SiteStanding {
    /// No decision applies: the site is Unresolved (it may still carry a marker).
    pub fn is_unresolved(&self) -> bool {
        self.decision.is_none()
    }
}

/// `GET /api/projects/:id/decisions`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DecisionsResp {
    /// Every record, revoked ones included (history), in the order they were written.
    pub decisions: Vec<Decision>,
    /// The sites of the latest inspection that have a decision, a marker or a stale decision.
    pub standings: Vec<SiteStanding>,
    /// Whether the draft migration has run for this project.
    pub migrated_from_draft: bool,
}

/// `POST /api/projects/:id/decisions/:id/revoke`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RevokeReq {
    #[serde(default)]
    pub by: String,
}

/// The reply to a revoke: the ids revoked (the record itself, and for a node decision every
/// per-site record it wrote).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokeResp {
    pub revoked: Vec<String>,
    pub revoked_at: String,
}

/// The body of every refusal from the decision routes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionError {
    pub ok: bool,
    /// A short machine-readable reason (`reason_required`, `unknown_node`, `not_found`, ...).
    pub code: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Decision {
        Decision {
            id: "dec-000001".into(),
            key: "sub:SEC-1/0".into(),
            state: DecisionState::TechDebtLater,
            reason: "scheduled for the Q1 auth work".into(),
            scope: Scope::Node("sub:SEC-1/0".into()),
            applies_to: vec!["k1".into(), "k2".into()],
            except: vec!["k3".into()],
            inherited_from: None,
            decided_by: "zach".into(),
            decided_at: "2026-11-02T10:00:00Z".into(),
            run_id: "insp-1".into(),
            engine_version: "0.1.0".into(),
            needed_me: None,
            sample_verdict: Some(SampleVerdict::Duplicate("k9".into())),
            confirmed_by_client: None,
            revoked_at: None,
        }
    }

    /// The wire shape of a decision is pinned: any change to a field name or an enum spelling is
    /// a change to what the UI and the stored files read, and must be made on purpose.
    #[test]
    fn the_decision_wire_shape_is_pinned() {
        let got = serde_json::to_value(sample()).unwrap();
        let want = serde_json::json!({
            "id": "dec-000001",
            "key": "sub:SEC-1/0",
            "state": "tech_debt_later",
            "reason": "scheduled for the Q1 auth work",
            "scope": { "node": "sub:SEC-1/0" },
            "applies_to": ["k1", "k2"],
            "except": ["k3"],
            "inherited_from": null,
            "decided_by": "zach",
            "decided_at": "2026-11-02T10:00:00Z",
            "run_id": "insp-1",
            "engine_version": "0.1.0",
            "needed_me": null,
            "sample_verdict": { "duplicate": "k9" },
            "confirmed_by_client": null,
            "revoked_at": null
        });
        assert_eq!(got, want);
        let back: Decision = serde_json::from_value(want).unwrap();
        assert_eq!(back, sample());
    }

    #[test]
    fn the_request_and_standing_wire_shapes_are_pinned() {
        let req = serde_json::json!({
            "scope": "site",
            "key": "k1",
            "state": { "merged_into": "k2" },
            "reason": "same cause"
        });
        let parsed: PostDecisionsReq = serde_json::from_value(req).unwrap();
        assert!(matches!(
            parsed,
            PostDecisionsReq::Decide(NewDecisionReq {
                state: DecisionState::MergedInto(ref t),
                ..
            }) if t == "k2"
        ));

        let migrate = serde_json::json!({
            "migrated_from_draft": true,
            "dispositions": { "old": { "state": "Ignored", "reason": "r", "bucket": "Later" } }
        });
        assert!(matches!(
            serde_json::from_value::<PostDecisionsReq>(migrate).unwrap(),
            PostDecisionsReq::Migrate(_)
        ));

        let standing = SiteStanding {
            key: "k".into(),
            decision: None,
            new_since_decision: Some(NewSinceDecision {
                decision_id: "dec-000002".into(),
                node_id: "rule:SEC-1".into(),
                decided_at: "2026-11-04T09:00:00Z".into(),
            }),
            stale: None,
        };
        assert_eq!(
            serde_json::to_value(&standing).unwrap(),
            serde_json::json!({
                "key": "k",
                "decision": null,
                "new_since_decision": {
                    "decision_id": "dec-000002",
                    "node_id": "rule:SEC-1",
                    "decided_at": "2026-11-04T09:00:00Z"
                },
                "stale": null
            })
        );
        assert!(standing.is_unresolved());
    }

    /// The response and request bodies the routes exchange, pinned field by field. A rename here
    /// is a change to what the UI sends and reads and must be made on purpose.
    #[test]
    fn the_route_bodies_wire_shapes_are_pinned() {
        let new = NewDecisionReq {
            scope: Scope::Node("rule:SEC-1".into()),
            key: String::new(),
            state: DecisionState::Confirmed,
            reason: "pattern reviewed".into(),
            decided_by: "zach".into(),
            needed_me: Some(true),
            sample_verdict: Some(SampleVerdict::FalseAlarm),
            except: vec!["k7".into()],
        };
        assert_eq!(
            serde_json::to_value(&new).unwrap(),
            serde_json::json!({
                "scope": { "node": "rule:SEC-1" },
                "key": "",
                "state": "confirmed",
                "reason": "pattern reviewed",
                "decided_by": "zach",
                "needed_me": true,
                "sample_verdict": "false_alarm",
                "except": ["k7"]
            })
        );

        let mut dispositions = BTreeMap::new();
        dispositions.insert(
            "site".to_string(),
            DraftDisposition {
                state: "Ignored".into(),
                reason: "r".into(),
                bucket: "Later".into(),
            },
        );
        assert_eq!(
            serde_json::to_value(MigrateDraftReq {
                migrated_from_draft: true,
                dispositions: dispositions.clone(),
            })
            .unwrap(),
            serde_json::json!({
                "migrated_from_draft": true,
                "dispositions": { "site": { "state": "Ignored", "reason": "r", "bucket": "Later" } }
            })
        );
        assert_eq!(
            serde_json::to_value(MigrationOutcome {
                already_migrated: false,
                migrated: 2,
                unmatched: vec!["old".into()],
                skipped: 1,
                dispositions,
            })
            .unwrap(),
            serde_json::json!({
                "already_migrated": false,
                "migrated": 2,
                "unmatched": ["old"],
                "skipped": 1,
                "dispositions": { "site": { "state": "Ignored", "reason": "r", "bucket": "Later" } }
            })
        );
        assert_eq!(
            serde_json::to_value(RevokeResp {
                revoked: vec!["dec-000001".into(), "dec-000001.00000".into()],
                revoked_at: "2026-11-05T08:00:00Z".into(),
            })
            .unwrap(),
            serde_json::json!({
                "revoked": ["dec-000001", "dec-000001.00000"],
                "revoked_at": "2026-11-05T08:00:00Z"
            })
        );
        assert_eq!(
            serde_json::to_value(DecisionError {
                ok: false,
                code: "reason_required".into(),
                message: "m".into(),
            })
            .unwrap(),
            serde_json::json!({ "ok": false, "code": "reason_required", "message": "m" })
        );
        assert_eq!(
            serde_json::to_value(DecisionsResp::default()).unwrap(),
            serde_json::json!({
                "decisions": [],
                "standings": [],
                "migrated_from_draft": false
            })
        );
        assert_eq!(
            serde_json::to_value(StaleDecision {
                decision_id: "d".into(),
                from_key: "k".into(),
                state: DecisionState::MergedInto("t".into()),
                reason: "r".into(),
                decided_at: "t0".into(),
                inherited_from: Some("n".into()),
            })
            .unwrap(),
            serde_json::json!({
                "decision_id": "d",
                "from_key": "k",
                "state": { "merged_into": "t" },
                "reason": "r",
                "decided_at": "t0",
                "inherited_from": "n"
            })
        );
        assert_eq!(
            serde_json::to_value(AppliedDecision {
                decision_id: "d".into(),
                state: DecisionState::Ignored,
                reason: "r".into(),
                inherited_from: None,
            })
            .unwrap(),
            serde_json::json!({
                "decision_id": "d",
                "state": "ignored",
                "reason": "r",
                "inherited_from": null
            })
        );
    }

    /// The effective decision of a site is the latest record that is not revoked, and a record a
    /// group decision wrote counts only while its node record is live and lists the key.
    #[test]
    fn the_effective_decision_is_the_latest_live_record() {
        let mut node = sample();
        node.id = "n1".into();
        node.applies_to = vec!["a".into()];
        let inherited = |id: &str, key: &str| Decision {
            id: id.into(),
            key: key.into(),
            scope: Scope::Site,
            applies_to: Vec::new(),
            inherited_from: Some("n1".into()),
            state: DecisionState::Confirmed,
            ..sample()
        };
        let own = |id: &str, key: &str, state: DecisionState| Decision {
            id: id.into(),
            key: key.into(),
            scope: Scope::Site,
            applies_to: Vec::new(),
            inherited_from: None,
            state,
            ..sample()
        };
        let records = vec![
            node.clone(),
            inherited("n1.0", "a"),
            // Not in the node record's applies_to: ignored.
            inherited("n1.1", "b"),
            own("d2", "a", DecisionState::Ignored),
        ];
        let map = effective_by_key(&records);
        assert_eq!(map.len(), 1);
        assert_eq!(map["a"].id, "d2", "the later ruling wins");

        let mut revoked_node = node;
        revoked_node.revoked_at = Some("t".into());
        let dead = [revoked_node, inherited("n1.0", "a")];
        let map = effective_by_key(&dead);
        assert!(
            map.is_empty(),
            "a record of a revoked group decision is dead"
        );
    }
}
