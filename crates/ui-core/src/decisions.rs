//! The review UI's side of the server-side decision store (review-tree design 3.5 and 3.6):
//! pure conversions between the triage table's dispositions and the shared decision types.
//!
//! The onboarding draft used to hold the reviewer's dispositions. They now live on the server;
//! the draft's copy is migrated once and then blanked. Until the review tree screen reads the
//! decisions directly, the triage table still works from a `key -> Disposition` map, and this
//! module builds that map from the stored decisions and builds the migration request from an old
//! draft's map.
//!
//! # The write-through bridge
//!
//! Every change the current review screen makes to its map must reach the store as it happens
//! ([`DecisionBridge`]): a changed or new disposition posts a site decision; a return to
//! Unresolved (undo, revert) revokes the site's live records. The bridge only PLANS: it compares
//! the map with what the store is known to hold and names the requests to make. The caller
//! performs them and reports each result back. A request that fails is not acknowledged, so the
//! change is still pending: [`DecisionBridge::pending`] is exactly what the draft must keep
//! (an Unresolved entry means "revoke"), and on the next load [`overlay_pending`] puts it back
//! over what the store holds, so the same plan is made again. No I/O here.

use std::collections::{BTreeMap, HashMap};

use camerata_api_types::decisions::{
    effective_by_key, live_by_key, Decision, DecisionState, DraftDisposition, MigrateDraftReq,
    MigrationOutcome, NewDecisionReq, Scope,
};

use crate::triage::{Disposition, TechDebtBucket, TriageState};

/// The triage-table disposition a stored decision means. A merge has no triage state of its own
/// (the merge screen is a later work order), so it maps to none.
pub fn disposition_of(d: &Decision) -> Option<Disposition> {
    let (state, bucket) = match &d.state {
        DecisionState::Confirmed => (TriageState::Confirmed, TechDebtBucket::Later),
        DecisionState::Ignored => (TriageState::Ignored, TechDebtBucket::Later),
        DecisionState::FalsePositive => (TriageState::FalsePositive, TechDebtBucket::Later),
        DecisionState::TechDebtNow => (TriageState::TechDebt, TechDebtBucket::Now),
        DecisionState::TechDebtLater => (TriageState::TechDebt, TechDebtBucket::Later),
        DecisionState::MergedInto(_) => return None,
    };
    Some(Disposition {
        state,
        reason: d.reason.clone(),
        bucket,
    })
}

/// The `key -> Disposition` map the triage table shows, from the project's stored decisions
/// (every record, revoked ones included: the effective decision of each site is worked out
/// here the way the server works it out).
pub fn dispositions_from_decisions(decisions: &[Decision]) -> HashMap<String, Disposition> {
    effective_by_key(decisions)
        .into_iter()
        .filter_map(|(key, d)| disposition_of(d).map(|disp| (key.to_string(), disp)))
        .collect()
}

/// The migration request for an old draft's dispositions, or `None` when the draft held none.
/// An entry still Unresolved is a decision nobody made and is left out.
pub fn migration_request(dispositions: &HashMap<String, Disposition>) -> Option<MigrateDraftReq> {
    let carried: BTreeMap<String, DraftDisposition> = dispositions
        .iter()
        .filter(|(_, d)| d.state.is_decided())
        .map(|(key, d)| {
            (
                key.clone(),
                DraftDisposition {
                    state: wire_name(&d.state),
                    reason: d.reason.clone(),
                    bucket: wire_name(&d.bucket),
                },
            )
        })
        .collect();
    if carried.is_empty() {
        None
    } else {
        Some(MigrateDraftReq {
            migrated_from_draft: true,
            dispositions: carried,
        })
    }
}

/// The dispositions a migration produced, by current site key, as the triage table holds them.
/// An entry whose state this build does not know is left out.
pub fn dispositions_from_migration(outcome: &MigrationOutcome) -> HashMap<String, Disposition> {
    outcome
        .dispositions
        .iter()
        .filter_map(|(key, d)| {
            let state: TriageState = from_wire_name(&d.state)?;
            let bucket: TechDebtBucket = from_wire_name(&d.bucket).unwrap_or(TechDebtBucket::Later);
            Some((
                key.clone(),
                Disposition {
                    state,
                    reason: d.reason.clone(),
                    bucket,
                },
            ))
        })
        .collect()
}

/// The name serde writes for a unit variant, which is what the draft was saved with.
fn wire_name<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn from_wire_name<T: serde::de::DeserializeOwned>(name: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(name.to_string())).ok()
}

/// The decision state a triage-table disposition means; `None` for Unresolved.
pub fn state_of(d: &Disposition) -> Option<DecisionState> {
    match d.state {
        TriageState::Unresolved => None,
        TriageState::Confirmed => Some(DecisionState::Confirmed),
        TriageState::Ignored => Some(DecisionState::Ignored),
        TriageState::FalsePositive => Some(DecisionState::FalsePositive),
        TriageState::TechDebt => Some(match d.bucket {
            TechDebtBucket::Now => DecisionState::TechDebtNow,
            TechDebtBucket::Later => DecisionState::TechDebtLater,
        }),
    }
}

/// One request the bridge wants made.
#[derive(Debug, Clone, PartialEq)]
pub enum BridgeOp {
    /// `POST .../decisions` with a site-scope decision for `key`.
    Post {
        key: String,
        disposition: Disposition,
        request: NewDecisionReq,
    },
    /// `POST .../decisions/:id/revoke` for each of the site's live records.
    Revoke {
        key: String,
        decision_ids: Vec<String>,
    },
}

/// What the store is known to hold for one site.
#[derive(Debug, Clone)]
struct Synced {
    disposition: Disposition,
    /// Every live site-level record of the site, oldest first (a later ruling does not remove
    /// the earlier ones, so an undo revokes them all).
    ids: Vec<String>,
}

/// Plans the requests that keep the store equal to the review screen's map. See the module docs.
#[derive(Debug, Clone, Default)]
pub struct DecisionBridge {
    synced: HashMap<String, Synced>,
    running: bool,
}

impl DecisionBridge {
    /// A bridge for a store holding `decisions` (all records, revoked ones included). A site
    /// whose live decision the table cannot show (a merge) is left alone: the bridge never posts
    /// over it or revokes it unless the screen sets a disposition for that site.
    pub fn from_decisions(decisions: &[Decision]) -> Self {
        let synced = live_by_key(decisions)
            .into_iter()
            .filter_map(|(key, records)| {
                let disposition = disposition_of(records.last()?)?;
                Some((
                    key.to_string(),
                    Synced {
                        disposition,
                        ids: records.iter().map(|d| d.id.clone()).collect(),
                    },
                ))
            })
            .collect();
        Self {
            synced,
            running: false,
        }
    }

    /// Claim the single run of the sync loop. `false` when one is already running.
    pub fn begin(&mut self) -> bool {
        !std::mem::replace(&mut self.running, true)
    }

    /// Release the claim taken by [`Self::begin`].
    pub fn finish(&mut self) {
        self.running = false;
    }

    /// The requests that would make the store equal `desired`, in key order.
    pub fn next_ops(&self, desired: &HashMap<String, Disposition>) -> Vec<BridgeOp> {
        let mut ops: Vec<BridgeOp> = Vec::new();
        for (key, d) in desired {
            let Some(state) = state_of(d) else {
                continue;
            };
            if self.synced.get(key).map(|s| &s.disposition) != Some(d) {
                ops.push(BridgeOp::Post {
                    key: key.clone(),
                    disposition: d.clone(),
                    request: NewDecisionReq {
                        scope: Scope::Site,
                        key: key.clone(),
                        state,
                        reason: d.reason.trim().to_string(),
                        decided_by: String::new(),
                        needed_me: None,
                        sample_verdict: None,
                        except: Vec::new(),
                    },
                });
            }
        }
        for (key, synced) in &self.synced {
            if !desired.get(key).is_some_and(|d| d.state.is_decided()) {
                ops.push(BridgeOp::Revoke {
                    key: key.clone(),
                    decision_ids: synced.ids.clone(),
                });
            }
        }
        ops.sort_by(|a, b| op_key(a).cmp(op_key(b)));
        ops
    }

    /// The store accepted the post for `key`; `decision_id` is the new record.
    pub fn ack_post(&mut self, key: &str, disposition: Disposition, decision_id: String) {
        let entry = self.synced.entry(key.to_string()).or_insert(Synced {
            disposition: disposition.clone(),
            ids: Vec::new(),
        });
        entry.disposition = disposition;
        entry.ids.push(decision_id);
    }

    /// The store revoked the site's records (or had already).
    pub fn ack_revoke(&mut self, key: &str) {
        self.synced.remove(key);
    }

    /// What the draft must keep: every change the store does not yet hold. A decided entry is a
    /// change to post; an Unresolved entry is a revert to revoke.
    pub fn pending(&self, desired: &HashMap<String, Disposition>) -> HashMap<String, Disposition> {
        self.next_ops(desired)
            .into_iter()
            .map(|op| match op {
                BridgeOp::Post {
                    key, disposition, ..
                } => (key, disposition),
                BridgeOp::Revoke { key, .. } => (key, Disposition::default()),
            })
            .collect()
    }
}

fn op_key(op: &BridgeOp) -> &str {
    match op {
        BridgeOp::Post { key, .. } | BridgeOp::Revoke { key, .. } => key,
    }
}

/// The map the review screen shows after a load: what the store holds with the draft's pending
/// changes over it (an Unresolved entry removes the site).
pub fn overlay_pending(
    mut stored: HashMap<String, Disposition>,
    pending: &HashMap<String, Disposition>,
) -> HashMap<String, Disposition> {
    for (key, d) in pending {
        if d.state.is_decided() {
            stored.insert(key.clone(), d.clone());
        } else {
            stored.remove(key);
        }
    }
    stored
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_api_types::decisions::Scope;

    fn decision(id: &str, key: &str, state: DecisionState, reason: &str) -> Decision {
        Decision {
            id: id.into(),
            key: key.into(),
            state,
            reason: reason.into(),
            scope: Scope::Site,
            applies_to: Vec::new(),
            except: Vec::new(),
            inherited_from: None,
            decided_by: "zach".into(),
            decided_at: "2026-11-04T09:00:00Z".into(),
            run_id: "insp-1".into(),
            engine_version: "v".into(),
            needed_me: None,
            sample_verdict: None,
            confirmed_by_client: None,
            revoked_at: None,
        }
    }

    #[test]
    fn stored_decisions_become_the_triage_tables_map() {
        let mut revoked = decision("d3", "k3", DecisionState::Ignored, "gone");
        revoked.revoked_at = Some("2026-11-05T09:00:00Z".into());
        let map = dispositions_from_decisions(&[
            decision("d1", "k1", DecisionState::TechDebtNow, "fix now"),
            decision("d2", "k2", DecisionState::Confirmed, ""),
            revoked,
            decision(
                "d4",
                "k4",
                DecisionState::MergedInto("k1".into()),
                "same cause",
            ),
            // The later ruling on a site is the one shown.
            decision("d5", "k2", DecisionState::FalsePositive, "generated"),
        ]);
        assert_eq!(
            map.len(),
            2,
            "revoked and merged records have no table entry"
        );
        assert_eq!(map["k1"].state, TriageState::TechDebt);
        assert_eq!(map["k1"].bucket, TechDebtBucket::Now);
        assert_eq!(map["k2"].state, TriageState::FalsePositive);
        assert_eq!(map["k2"].reason, "generated");
    }

    #[test]
    fn an_old_drafts_dispositions_become_the_migration_request() {
        let mut draft = HashMap::new();
        draft.insert(
            "old1".to_string(),
            Disposition {
                state: TriageState::TechDebt,
                reason: "later".into(),
                bucket: TechDebtBucket::Now,
            },
        );
        draft.insert("old2".to_string(), Disposition::default());
        let req = migration_request(&draft).expect("one decided entry");
        assert!(req.migrated_from_draft);
        assert_eq!(
            req.dispositions.len(),
            1,
            "an Unresolved entry is not carried"
        );
        let d = &req.dispositions["old1"];
        assert_eq!(
            (d.state.as_str(), d.reason.as_str(), d.bucket.as_str()),
            ("TechDebt", "later", "Now")
        );
        assert!(migration_request(&HashMap::new()).is_none());
        let mut only_unresolved = HashMap::new();
        only_unresolved.insert("k".to_string(), Disposition::default());
        assert!(migration_request(&only_unresolved).is_none());
    }

    #[test]
    fn the_migration_outcome_carries_the_session_on() {
        let mut dispositions = BTreeMap::new();
        dispositions.insert(
            "site-key".to_string(),
            DraftDisposition {
                state: "Ignored".into(),
                reason: "fixture".into(),
                bucket: "Later".into(),
            },
        );
        dispositions.insert(
            "mystery".to_string(),
            DraftDisposition {
                state: "FromTheFuture".into(),
                reason: String::new(),
                bucket: String::new(),
            },
        );
        let map = dispositions_from_migration(&MigrationOutcome {
            already_migrated: false,
            migrated: 2,
            unmatched: Vec::new(),
            skipped: 0,
            dispositions,
        });
        assert_eq!(map.len(), 1);
        assert_eq!(map["site-key"].state, TriageState::Ignored);
        assert_eq!(map["site-key"].reason, "fixture");
    }

    fn disp(state: TriageState, reason: &str) -> Disposition {
        Disposition {
            state,
            reason: reason.into(),
            bucket: TechDebtBucket::Later,
        }
    }

    fn map(entries: &[(&str, Disposition)]) -> HashMap<String, Disposition> {
        entries
            .iter()
            .map(|(k, d)| ((*k).to_string(), d.clone()))
            .collect()
    }

    #[test]
    fn a_new_or_changed_disposition_is_a_post_and_an_unchanged_one_is_not() {
        let stored = [decision("d1", "k1", DecisionState::Ignored, "fixture")];
        let bridge = DecisionBridge::from_decisions(&stored);
        let same = map(&[("k1", disp(TriageState::Ignored, "fixture"))]);
        assert!(
            bridge.next_ops(&same).is_empty(),
            "the store already holds it"
        );

        let desired = map(&[
            // A reason edit is a change.
            ("k1", disp(TriageState::Ignored, "fixture, generated")),
            ("k2", disp(TriageState::Confirmed, "")),
            (
                "k3",
                Disposition {
                    state: TriageState::TechDebt,
                    reason: String::new(),
                    bucket: TechDebtBucket::Now,
                },
            ),
            // Unresolved with nothing stored is nothing.
            ("k4", Disposition::default()),
        ]);
        let ops = bridge.next_ops(&desired);
        let keys: Vec<&str> = ops
            .iter()
            .map(|o| match o {
                BridgeOp::Post { key, .. } | BridgeOp::Revoke { key, .. } => key.as_str(),
            })
            .collect();
        assert_eq!(keys, vec!["k1", "k2", "k3"]);
        let BridgeOp::Post { request, .. } = &ops[2] else {
            panic!("a post")
        };
        assert_eq!(request.scope, Scope::Site);
        assert_eq!(request.state, DecisionState::TechDebtNow);
        assert_eq!(request.key, "k3");
        let BridgeOp::Post { request, .. } = &ops[0] else {
            panic!("a post")
        };
        assert_eq!(request.reason, "fixture, generated");
    }

    #[test]
    fn going_back_to_unresolved_revokes_every_live_record_of_the_site() {
        let mut inherited = decision("n1.00000", "k1", DecisionState::Confirmed, "group");
        inherited.inherited_from = Some("n1".into());
        let mut node = decision("n1", "rule:X", DecisionState::Confirmed, "group");
        node.scope = Scope::Node("rule:X".into());
        node.applies_to = vec!["k1".into()];
        let later = decision("d2", "k1", DecisionState::Ignored, "own ruling");
        let bridge = DecisionBridge::from_decisions(&[node, inherited, later]);

        for desired in [HashMap::new(), map(&[("k1", Disposition::default())])] {
            assert_eq!(
                bridge.next_ops(&desired),
                vec![BridgeOp::Revoke {
                    key: "k1".into(),
                    decision_ids: vec!["n1.00000".into(), "d2".into()],
                }]
            );
        }
    }

    #[test]
    fn a_site_the_table_cannot_show_is_left_alone() {
        let merged = decision("d1", "k1", DecisionState::MergedInto("k2".into()), "same");
        let bridge = DecisionBridge::from_decisions(&[merged]);
        assert!(bridge.next_ops(&HashMap::new()).is_empty());
    }

    #[test]
    fn a_change_that_was_not_stored_stays_pending_and_comes_back_on_the_next_load() {
        let stored = [
            decision("d1", "k1", DecisionState::Ignored, "fixture"),
            decision("d2", "k2", DecisionState::Confirmed, ""),
        ];
        let mut bridge = DecisionBridge::from_decisions(&stored);
        // The screen: k1 undone, k2 untouched, k3 newly ignored.
        let desired = map(&[
            ("k2", disp(TriageState::Confirmed, "")),
            ("k3", disp(TriageState::Ignored, "dead code")),
        ]);
        let pending = bridge.pending(&desired);
        assert_eq!(pending.len(), 2, "only what the store lacks: {pending:?}");
        assert_eq!(pending["k3"].reason, "dead code");
        assert_eq!(pending["k1"].state, TriageState::Unresolved, "a revert");

        // The k3 post succeeds, the k1 revoke fails: only the revert is still pending.
        bridge.ack_post("k3", pending["k3"].clone(), "d3".into());
        let pending = bridge.pending(&desired);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending["k1"].state, TriageState::Unresolved);

        // Restart: the store still has k1; the draft's pending revert goes back over it.
        let shown = overlay_pending(dispositions_from_decisions(&stored), &pending);
        assert!(!shown.contains_key("k1"));
        assert!(shown.contains_key("k2"));
        // And the same plan is made again.
        let again = DecisionBridge::from_decisions(&stored);
        assert!(matches!(
            again.next_ops(&shown).as_slice(),
            [BridgeOp::Revoke { key, .. }] if key == "k1"
        ));
    }

    #[test]
    fn only_one_sync_loop_runs_at_a_time() {
        let mut bridge = DecisionBridge::default();
        assert!(bridge.begin());
        assert!(!bridge.begin());
        bridge.finish();
        assert!(bridge.begin());
    }
}
