//! Loading the decision store into the review screen, and writing the screen's changes through
//! to it (review-tree design 3.5 and 3.6; the write-through bridge in [`crate::decisions`]).
//!
//! The logic lives here, behind [`DecisionTransport`], so the load and the sync loop are tested
//! with a fake store; the UI supplies the HTTP transport and nothing else.
//!
//! # Rules
//!
//! - A load that cannot read the store never draws a decision as Unresolved. With an earlier
//!   state to show it is [`Availability::LastKnown`]; without one it is [`Availability::Unknown`].
//!   Both carry a banner. Changes made meanwhile, undos included, are queued in the draft.
//! - The old draft's dispositions are migrated only when the server accepts them; a refusal
//!   (no scan rows yet, or the server down) leaves the draft as it was.
//! - The sync loop is per key. A change the store refuses is remembered and shown, and the next
//!   key goes on. Only an unreachable store ends the pass.

use std::cell::RefCell;
use std::collections::HashMap;

use camerata_api_types::decisions::{DecisionsResp, MigrateDraftReq, NewDecisionReq};

use crate::decisions::{
    dispositions_from_decisions, migration_request, overlay_pending, Availability, BridgeOp,
    DecisionBridge, DraftQueue, Refusal,
};
use crate::triage::Disposition;

/// Shown while the stored decisions are the last ones read, not verified.
pub const LAST_KNOWN_BANNER: &str =
    "Decisions not loaded: the decision store could not be reached. \
You are seeing your decisions as of the last time it could be, plus what you have changed since. \
Changes are saved here and stored when it is reachable. An export waits until they are.";

/// Shown when there is nothing earlier to show.
pub const UNKNOWN_BANNER: &str = "Decisions not loaded: the decision store could not be reached, \
so what you have decided on these findings is unknown. It is not shown as Unresolved. Retry once \
the server is reachable.";

/// Why a request to the store did not succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpError {
    /// The store could not be asked (no connection, a server error). Nothing was decided about
    /// the change; it stays queued.
    Unreachable(String),
    /// The store answered and said no. `message` is its reason.
    Refused { status: u16, message: String },
}

/// The store, as the UI reaches it.
#[allow(async_fn_in_trait)]
pub trait DecisionTransport {
    /// `GET .../decisions`; `None` when it could not be read.
    async fn fetch(&self) -> Option<DecisionsResp>;
    /// `POST .../decisions` with the old draft's dispositions.
    async fn migrate(&self, req: &MigrateDraftReq) -> Result<(), OpError>;
    /// `POST .../decisions` with one site decision; returns the new record's id.
    async fn post(&self, req: &NewDecisionReq) -> Result<String, OpError>;
    /// `POST .../decisions/:id/revoke`.
    async fn revoke(&self, decision_id: &str) -> Result<(), OpError>;
}

/// What a load made of the store.
pub struct Loaded {
    /// What the screen shows. Meaningful only when `bridge.availability()` is not Unknown.
    pub shown: HashMap<String, Disposition>,
    pub bridge: DecisionBridge,
    /// The banner to draw, when the state is not Live.
    pub banner: Option<&'static str>,
}

impl Loaded {
    /// How the decision on `key` reads on this screen: never Unresolved when it is unknown.
    pub fn state_of(&self, key: &str) -> StateView {
        match self.bridge.availability() {
            Availability::Unknown => StateView::Unknown,
            _ => match self.shown.get(key) {
                Some(d) if d.state.is_decided() => StateView::Decided(d.clone()),
                _ => StateView::Unresolved,
            },
        }
    }
}

/// The decision state of a site as the screen may draw it.
#[derive(Debug, Clone, PartialEq)]
pub enum StateView {
    Decided(Disposition),
    Unresolved,
    /// The store could not be read and there is nothing earlier: not Unresolved.
    Unknown,
}

/// Read the store (migrating an old draft's dispositions first when the server has not), and
/// build what the screen shows from it and the draft's queue.
pub async fn load_decisions<T: DecisionTransport>(t: &T, queue: &DraftQueue) -> Loaded {
    let Some(mut got) = t.fetch().await else {
        return unreachable_store(queue);
    };
    let mut migrated_now = false;
    if !got.migrated_from_draft {
        let req = migration_request(&queue.pending).unwrap_or(MigrateDraftReq {
            migrated_from_draft: true,
            dispositions: Default::default(),
        });
        // A refusal (no scan rows yet) or a failure leaves the draft exactly as it was.
        if t.migrate(&req).await.is_err() {
            return unreachable_store(queue);
        }
        match t.fetch().await {
            Some(again) => {
                got = again;
                migrated_now = true;
            }
            None => return unreachable_store(queue),
        }
    }
    let stored = dispositions_from_decisions(&got.decisions);
    // Right after a migration the draft's entries were the old ones, now in the store; otherwise
    // they are changes waiting to be stored.
    let pending = if migrated_now {
        HashMap::new()
    } else {
        queue.pending.clone()
    };
    let mut bridge = DecisionBridge::from_decisions(&got.decisions);
    bridge.restore_stamps(&queue.stamps, &pending);
    bridge.set_run_id(&got.latest_run_id);
    Loaded {
        shown: overlay_pending(stored, &pending),
        bridge,
        banner: None,
    }
}

fn unreachable_store(queue: &DraftQueue) -> Loaded {
    match &queue.last_known {
        Some(last_known) => Loaded {
            shown: overlay_pending(last_known.clone(), &queue.pending),
            bridge: DecisionBridge::from_last_known(last_known, queue),
            banner: Some(LAST_KNOWN_BANNER),
        },
        None => Loaded {
            shown: queue
                .pending
                .iter()
                .filter(|(_, d)| d.state.is_decided())
                .map(|(k, d)| (k.clone(), d.clone()))
                .collect(),
            bridge: DecisionBridge::unknown(queue.clone()),
            banner: Some(UNKNOWN_BANNER),
        },
    }
}

/// What one run of the sync loop did.
#[derive(Debug, Default, PartialEq)]
pub struct SyncOutcome {
    /// At least one change was stored (or revoked): the draft's queue shrank.
    pub stored_any: bool,
    /// The store could not be reached; the pass ended there and the rest stays queued.
    pub unreachable: Option<String>,
    /// Changes the store has refused that the screen still asks for, one per key.
    pub refused: Vec<Refusal>,
}

/// Make the requests the bridge plans until the store equals the screen, re-reading the screen
/// after every pass. `site_of` names a key in words for an error. The borrow of `bridge` is never
/// held across a request.
pub async fn sync_decisions<T, D, S>(
    bridge: &RefCell<DecisionBridge>,
    t: &T,
    desired: D,
    site_of: S,
) -> SyncOutcome
where
    T: DecisionTransport,
    D: Fn() -> HashMap<String, Disposition>,
    S: Fn(&str) -> String,
{
    let mut out = SyncOutcome::default();
    if !bridge.borrow_mut().begin() {
        return out;
    }
    'passes: loop {
        let want = desired();
        let ops = bridge.borrow().next_ops(&want);
        if ops.is_empty() {
            break;
        }
        for op in ops {
            let result = match &op {
                BridgeOp::Post { request, .. } => t.post(request.as_ref()).await.map(Some),
                BridgeOp::Revoke { decision_ids, .. } => {
                    let mut last = Ok(None);
                    for id in decision_ids {
                        match t.revoke(id).await {
                            Ok(()) => {}
                            // Already gone, or already revoked: as good as revoked.
                            Err(OpError::Refused {
                                status: 404 | 409, ..
                            }) => {}
                            Err(e) => {
                                last = Err(e);
                                break;
                            }
                        }
                    }
                    last
                }
            };
            match (result, &op) {
                (
                    Ok(Some(id)),
                    BridgeOp::Post {
                        key, disposition, ..
                    },
                ) => {
                    bridge.borrow_mut().ack_post(key, disposition.clone(), id);
                    out.stored_any = true;
                }
                (Ok(_), BridgeOp::Revoke { key, .. }) => {
                    bridge.borrow_mut().ack_revoke(key);
                    out.stored_any = true;
                }
                (Ok(None), BridgeOp::Post { .. }) => {}
                (Err(OpError::Refused { message, .. }), op) => {
                    let key = match op {
                        BridgeOp::Post { key, .. } | BridgeOp::Revoke { key, .. } => key,
                    };
                    bridge.borrow_mut().nack(op, &site_of(key), &message);
                }
                (Err(OpError::Unreachable(e)), _) => {
                    out.unreachable = Some(e);
                    break 'passes;
                }
            }
        }
    }
    let mut b = bridge.borrow_mut();
    b.finish();
    out.refused = b.refusals(&desired());
    out
}

#[cfg(test)]
mod tests;
