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
    MigrationOutcome, NewDecisionReq, Scope, UnsyncedChange, UnsyncedKind,
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
    /// the earlier ones, so an undo revokes them all). Empty when the state is only a snapshot
    /// from the last time the store was reachable.
    ids: Vec<String>,
}

/// How far the screen can trust what it shows of the stored decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Availability {
    /// The store was read this session; the screen shows its decisions.
    Live,
    /// The store could not be reached; the screen shows the decisions as of the last time it
    /// could, with the changes made since over them. Changes are queued.
    LastKnown,
    /// The store could not be reached and there is no earlier state to show. The decision state
    /// is UNKNOWN: it must never be drawn as Unresolved.
    #[default]
    Unknown,
}

/// When and on which inspection a queued change was decided.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Stamp {
    /// The inspection (entry id) the screen was showing; blank when it was not known.
    pub run_id: String,
    /// RFC3339 time of the decision.
    pub at: String,
}

/// What the draft keeps of the write-through queue: the changes the store does not hold, the
/// store's state as last read, and when each queued change was decided.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DraftQueue {
    /// A decided entry is a change to store; an Unresolved entry is an undo to revoke.
    pub pending: HashMap<String, Disposition>,
    /// The stored decisions as of the last time the store was read. `None`: never read.
    pub last_known: Option<HashMap<String, Disposition>>,
    pub stamps: HashMap<String, Stamp>,
}

/// A queued change the store refused. Never dropped: it stays pending and is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub key: String,
    /// The site in words (`path:line`).
    pub site: String,
    pub reason: String,
    /// The refused change was an undo.
    pub undo: bool,
}

impl Refusal {
    /// The error the reviewer reads, naming the site and the reason.
    pub fn text(&self) -> String {
        let what = if self.undo { "undo" } else { "decision" };
        format!(
            "Your {what} on {} was not stored: {}. It stays in your saved review and is not \
             included in an export until it is stored.",
            self.site, self.reason
        )
    }
}

#[derive(Debug, Clone)]
struct RefusedAt {
    /// The disposition that was refused; `None` for an undo.
    wanted: Option<Disposition>,
    site: String,
    reason: String,
}

/// Plans the requests that keep the store equal to the review screen's map. See the module docs.
#[derive(Debug, Clone, Default)]
pub struct DecisionBridge {
    synced: HashMap<String, Synced>,
    running: bool,
    availability: Availability,
    /// The queue as loaded, kept verbatim while the state is Unknown so the draft never loses it.
    held: Option<DraftQueue>,
    stamps: HashMap<String, (Disposition, Stamp)>,
    refused: HashMap<String, RefusedAt>,
    run_id: String,
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
            availability: Availability::Live,
            ..Self::default()
        }
    }

    /// A bridge for a store that could not be reached, over the state it was last read in. The
    /// queued changes are kept; none can be sent.
    pub fn from_last_known(last_known: &HashMap<String, Disposition>, queue: &DraftQueue) -> Self {
        let mut bridge = Self {
            synced: last_known
                .iter()
                .map(|(k, d)| {
                    (
                        k.clone(),
                        Synced {
                            disposition: d.clone(),
                            ids: Vec::new(),
                        },
                    )
                })
                .collect(),
            availability: Availability::LastKnown,
            ..Self::default()
        };
        bridge.restore_stamps(&queue.stamps, &queue.pending);
        bridge
    }

    /// A bridge for a store that could not be reached with nothing earlier to go on. It holds the
    /// draft's queue verbatim so saving the draft cannot lose any of it.
    pub fn unknown(queue: DraftQueue) -> Self {
        Self {
            availability: Availability::Unknown,
            held: Some(queue),
            ..Self::default()
        }
    }

    pub fn availability(&self) -> Availability {
        self.availability
    }

    /// The inspection the screen is showing, as the store reported it at load.
    pub fn set_run_id(&mut self, run_id: &str) {
        self.run_id = run_id.to_string();
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Claim the single run of the sync loop. `false` when one is already running, or when the
    /// store is not reachable (nothing can be sent).
    pub fn begin(&mut self) -> bool {
        if self.availability != Availability::Live {
            return false;
        }
        !std::mem::replace(&mut self.running, true)
    }

    /// Release the claim taken by [`Self::begin`].
    pub fn finish(&mut self) {
        self.running = false;
    }

    /// Stamp every change that is new or newly different since it was last seen with the run and
    /// the time it is decided at. Call it whenever the screen's map changes, before planning:
    /// a change flushed later still carries the moment and the run it was made on.
    pub fn observe(&mut self, desired: &HashMap<String, Disposition>, run_id: &str, now: &str) {
        let pending: HashMap<String, Disposition> = self
            .plan(desired, false)
            .into_iter()
            .filter_map(|op| match op {
                BridgeOp::Post {
                    key, disposition, ..
                } => Some((key, disposition)),
                BridgeOp::Revoke { .. } => None,
            })
            .collect();
        self.stamps.retain(|k, (d, _)| pending.get(k) == Some(d));
        for (key, d) in pending {
            self.stamps.entry(key).or_insert_with(|| {
                (
                    d,
                    Stamp {
                        run_id: run_id.to_string(),
                        at: now.to_string(),
                    },
                )
            });
        }
    }

    /// Put back the stamps a draft carried, for the changes that are still pending.
    pub fn restore_stamps(
        &mut self,
        stamps: &HashMap<String, Stamp>,
        pending: &HashMap<String, Disposition>,
    ) {
        for (key, stamp) in stamps {
            if let Some(d) = pending.get(key).filter(|d| d.state.is_decided()) {
                self.stamps.insert(key.clone(), (d.clone(), stamp.clone()));
            }
        }
    }

    /// The requests that would make the store equal `desired`, in key order, leaving out a
    /// change the store has refused until the screen asks for something different.
    pub fn next_ops(&self, desired: &HashMap<String, Disposition>) -> Vec<BridgeOp> {
        self.plan(desired, true)
    }

    fn plan(&self, desired: &HashMap<String, Disposition>, skip_refused: bool) -> Vec<BridgeOp> {
        let mut ops: Vec<BridgeOp> = Vec::new();
        for (key, d) in desired {
            let Some(state) = state_of(d) else {
                continue;
            };
            if self.synced.get(key).map(|s| &s.disposition) == Some(d) {
                continue;
            }
            if skip_refused
                && self
                    .refused
                    .get(key)
                    .is_some_and(|r| r.wanted.as_ref() == Some(d))
            {
                continue;
            }
            let stamp = self
                .stamps
                .get(key)
                .filter(|(sd, _)| sd == d)
                .map(|(_, s)| s);
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
                    run_id: stamp.map(|s| s.run_id.clone()).unwrap_or_default(),
                    decided_at: stamp.map(|s| s.at.clone()).unwrap_or_default(),
                },
            });
        }
        for (key, synced) in &self.synced {
            if desired.get(key).is_some_and(|d| d.state.is_decided()) {
                continue;
            }
            if skip_refused && self.refused.get(key).is_some_and(|r| r.wanted.is_none()) {
                continue;
            }
            ops.push(BridgeOp::Revoke {
                key: key.clone(),
                decision_ids: synced.ids.clone(),
            });
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
        self.stamps.remove(key);
        self.refused.remove(key);
    }

    /// The store revoked the site's records (or had already).
    pub fn ack_revoke(&mut self, key: &str) {
        self.synced.remove(key);
        self.refused.remove(key);
    }

    /// The store refused the change `op` planned. It stays pending and is remembered, so one
    /// refusal never blocks another key and is not sent again until the screen changes it.
    pub fn nack(&mut self, op: &BridgeOp, site: &str, reason: &str) {
        let (key, wanted) = match op {
            BridgeOp::Post {
                key, disposition, ..
            } => (key, Some(disposition.clone())),
            BridgeOp::Revoke { key, .. } => (key, None),
        };
        self.refused.insert(
            key.clone(),
            RefusedAt {
                wanted,
                site: site.to_string(),
                reason: reason.to_string(),
            },
        );
    }

    /// The refusals that still stand: for a change the screen still asks for.
    pub fn refusals(&self, desired: &HashMap<String, Disposition>) -> Vec<Refusal> {
        let mut out: Vec<Refusal> = self
            .refused
            .iter()
            .filter(|(key, r)| match &r.wanted {
                Some(d) => desired.get(*key) == Some(d),
                None => !desired.get(*key).is_some_and(|d| d.state.is_decided()),
            })
            .map(|(key, r)| Refusal {
                key: key.clone(),
                site: r.site.clone(),
                reason: r.reason.clone(),
                undo: r.wanted.is_none(),
            })
            .collect();
        out.sort_by(|a, b| a.key.cmp(&b.key));
        out
    }

    /// What the draft must keep: every change the store does not yet hold, refused ones
    /// included. A decided entry is a change to post; an Unresolved entry is an undo to revoke.
    pub fn pending(&self, desired: &HashMap<String, Disposition>) -> HashMap<String, Disposition> {
        self.plan(desired, false)
            .into_iter()
            .map(|op| match op {
                BridgeOp::Post {
                    key, disposition, ..
                } => (key, disposition),
                BridgeOp::Revoke { key, .. } => (key, Disposition::default()),
            })
            .collect()
    }

    /// The stored decisions as this bridge last knew them.
    pub fn snapshot(&self) -> HashMap<String, Disposition> {
        self.synced
            .iter()
            .map(|(k, s)| (k.clone(), s.disposition.clone()))
            .collect()
    }

    /// Everything the draft keeps of the queue. While the state is Unknown that is the queue as
    /// it was loaded, untouched.
    pub fn draft_queue(&self, desired: &HashMap<String, Disposition>) -> DraftQueue {
        if self.availability == Availability::Unknown {
            return self.held.clone().unwrap_or_default();
        }
        let pending = self.pending(desired);
        let stamps = self
            .stamps
            .iter()
            .filter(|(k, (d, _))| pending.get(*k) == Some(d))
            .map(|(k, (_, s))| (k.clone(), s.clone()))
            .collect();
        DraftQueue {
            pending,
            last_known: Some(self.snapshot()),
            stamps,
        }
    }

    /// The changes the store does not hold, as the export request lists them. The release gate
    /// refuses an export while there are any.
    pub fn unsynced(&self, desired: &HashMap<String, Disposition>) -> Vec<UnsyncedChange> {
        self.plan(desired, false)
            .into_iter()
            .map(|op| match op {
                BridgeOp::Post { key, .. } => UnsyncedChange {
                    key,
                    change: UnsyncedKind::Decision,
                },
                BridgeOp::Revoke { key, .. } => UnsyncedChange {
                    key,
                    change: UnsyncedKind::Undo,
                },
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
        let mut bridge = DecisionBridge::from_decisions(&[]);
        assert!(bridge.begin());
        assert!(!bridge.begin());
        bridge.finish();
        assert!(bridge.begin());
    }
}
