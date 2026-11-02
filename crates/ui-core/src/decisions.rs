//! The review UI's side of the server-side decision store (review-tree design 3.5 and 3.6):
//! pure conversions between the triage table's dispositions and the shared decision types.
//!
//! The onboarding draft used to hold the reviewer's dispositions. They now live on the server;
//! the draft's copy is migrated once and then blanked. Until the review tree screen reads the
//! decisions directly, the triage table still works from a `key -> Disposition` map, and this
//! module builds that map from the stored decisions and builds the migration request from an old
//! draft's map. No I/O here.

use std::collections::{BTreeMap, HashMap};

use camerata_api_types::decisions::{
    effective_by_key, Decision, DecisionState, DraftDisposition, MigrateDraftReq, MigrationOutcome,
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
}
