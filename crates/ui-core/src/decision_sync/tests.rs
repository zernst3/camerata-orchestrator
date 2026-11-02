//! The load and the sync loop, against a fake store.

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::task::{Context, Poll, Waker};

use camerata_api_types::decisions::{
    Decision, DecisionState, DecisionsResp, MigrateDraftReq, NewDecisionReq, Scope, UnsyncedKind,
};

use super::*;
use crate::decisions::{dispositions_from_decisions, Stamp};
use crate::triage::{TechDebtBucket, TriageState};

/// Run a future whose awaits all complete at once (the fake store never waits).
fn block_on<F: Future>(f: F) -> F::Output {
    let mut f = Box::pin(f);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
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

fn record(id: &str, key: &str, state: DecisionState, reason: &str) -> Decision {
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
        decided_at: "2026-11-02T10:00:00Z".into(),
        run_id: "insp-1".into(),
        engine_version: "v".into(),
        needed_me: None,
        sample_verdict: None,
        confirmed_by_client: None,
        revoked_at: None,
    }
}

/// A fake store. `reachable` switches the whole thing off; `refuse` names keys whose posts it
/// refuses; `refuse_revoke` names record ids whose revokes it refuses; everything it accepts is
/// recorded.
struct FakeStore {
    reachable: bool,
    migrated: RefCell<bool>,
    migrate_refuses: bool,
    refuse: HashMap<String, (u16, String)>,
    refuse_revoke: HashMap<String, (u16, String)>,
    decisions: RefCell<Vec<Decision>>,
    posted: RefCell<Vec<NewDecisionReq>>,
    revoked: RefCell<Vec<String>>,
    migrations: RefCell<usize>,
}

impl FakeStore {
    fn new(decisions: Vec<Decision>) -> Self {
        Self {
            reachable: true,
            migrated: RefCell::new(true),
            migrate_refuses: false,
            refuse: HashMap::new(),
            refuse_revoke: HashMap::new(),
            decisions: RefCell::new(decisions),
            posted: RefCell::new(Vec::new()),
            revoked: RefCell::new(Vec::new()),
            migrations: RefCell::new(0),
        }
    }
}

impl DecisionTransport for FakeStore {
    async fn fetch(&self) -> Option<DecisionsResp> {
        self.reachable.then(|| DecisionsResp {
            decisions: self.decisions.borrow().clone(),
            standings: Vec::new(),
            migrated_from_draft: *self.migrated.borrow(),
            latest_run_id: "insp-2".into(),
        })
    }
    async fn migrate(&self, _req: &MigrateDraftReq) -> Result<(), OpError> {
        if !self.reachable {
            return Err(OpError::Unreachable("down".into()));
        }
        if self.migrate_refuses {
            return Err(OpError::Refused {
                status: 409,
                message: "no scan rows yet".into(),
            });
        }
        *self.migrations.borrow_mut() += 1;
        *self.migrated.borrow_mut() = true;
        Ok(())
    }
    async fn post(&self, req: &NewDecisionReq) -> Result<String, OpError> {
        if !self.reachable {
            return Err(OpError::Unreachable("down".into()));
        }
        if let Some((status, message)) = self.refuse.get(&req.key) {
            return Err(OpError::Refused {
                status: *status,
                message: message.clone(),
            });
        }
        self.posted.borrow_mut().push(req.clone());
        let id = format!("d{}", self.decisions.borrow().len() + 1);
        self.decisions
            .borrow_mut()
            .push(record(&id, &req.key, req.state.clone(), &req.reason));
        Ok(id)
    }
    async fn revoke(&self, id: &str) -> Result<(), OpError> {
        if !self.reachable {
            return Err(OpError::Unreachable("down".into()));
        }
        if let Some((status, message)) = self.refuse_revoke.get(id) {
            return Err(OpError::Refused {
                status: *status,
                message: message.clone(),
            });
        }
        self.revoked.borrow_mut().push(id.to_string());
        for d in self
            .decisions
            .borrow_mut()
            .iter_mut()
            .filter(|d| d.id == id)
        {
            d.revoked_at = Some("t".into());
        }
        Ok(())
    }
}

fn site(key: &str) -> String {
    format!("src/{key}.rs:10")
}

fn sync(
    bridge: &RefCell<DecisionBridge>,
    store: &FakeStore,
    screen: &HashMap<String, Disposition>,
) -> SyncOutcome {
    block_on(sync_decisions(bridge, store, || screen.clone(), site))
}

// ── offline at load ───────────────────────────────────────────────────────────────────────

#[test]
fn an_offline_load_renders_unknown_not_unresolved() {
    let mut store = FakeStore::new(vec![record("d1", "k1", DecisionState::Ignored, "fixture")]);
    store.reachable = false;

    // Nothing earlier to show: the state is unknown, for a site that IS decided in the store
    // and for one that is not, and the banner says why.
    let loaded = block_on(load_decisions(&store, &DraftQueue::default()));
    assert_eq!(loaded.bridge.availability(), Availability::Unknown);
    assert_eq!(loaded.state_of("k1"), StateView::Unknown);
    assert_eq!(loaded.state_of("k2"), StateView::Unknown);
    assert!(loaded.banner.unwrap().starts_with("Decisions not loaded"));
    // Nothing can be sent while it is unknown.
    assert!(!loaded.bridge.clone().begin());

    // With an earlier state, the screen shows that, labelled; an undecided site is Unresolved
    // only as of that state.
    let queue = DraftQueue {
        last_known: Some(map(&[("k1", disp(TriageState::Ignored, "fixture"))])),
        ..DraftQueue::default()
    };
    let loaded = block_on(load_decisions(&store, &queue));
    assert_eq!(loaded.bridge.availability(), Availability::LastKnown);
    assert!(matches!(loaded.state_of("k1"), StateView::Decided(_)));
    assert!(loaded.banner.unwrap().starts_with("Decisions not loaded"));

    // Online, there is no banner.
    let online = FakeStore::new(vec![record("d1", "k1", DecisionState::Ignored, "fixture")]);
    let loaded = block_on(load_decisions(&online, &DraftQueue::default()));
    assert_eq!(loaded.bridge.availability(), Availability::Live);
    assert!(loaded.banner.is_none());
    assert_eq!(loaded.state_of("k2"), StateView::Unresolved);
}

#[test]
fn an_unknown_state_keeps_the_drafts_queue_untouched() {
    let mut store = FakeStore::new(Vec::new());
    store.reachable = false;
    let queue = DraftQueue {
        pending: map(&[("k3", disp(TriageState::Confirmed, ""))]),
        last_known: None,
        stamps: HashMap::from([(
            "k3".to_string(),
            Stamp {
                run_id: "insp-1".into(),
                at: "2026-11-02T10:00:00Z".into(),
            },
        )]),
    };
    let loaded = block_on(load_decisions(&store, &queue));
    // Whatever the screen does, saving the draft writes the queue back as it was loaded.
    assert_eq!(loaded.bridge.draft_queue(&HashMap::new()), queue);
}

#[test]
fn an_offline_undo_is_queued_and_revokes_on_the_next_online_load() {
    let stored = vec![
        record("d1", "k1", DecisionState::Ignored, "fixture"),
        record("d2", "k2", DecisionState::Confirmed, ""),
    ];
    // A session that saw the store, then lost it. The draft holds the last known state.
    let last_known = dispositions_from_decisions(&stored);
    let mut store = FakeStore::new(stored);
    store.reachable = false;
    let queue = DraftQueue {
        last_known: Some(last_known),
        ..DraftQueue::default()
    };
    let loaded = block_on(load_decisions(&store, &queue));
    assert_eq!(loaded.bridge.availability(), Availability::LastKnown);
    let mut screen = loaded.shown.clone();
    assert!(screen.contains_key("k1"));

    // The reviewer undoes k1 while offline. The sync loop sends nothing, and the undo is queued.
    screen.remove("k1");
    let bridge = RefCell::new(loaded.bridge);
    let out = sync(&bridge, &store, &screen);
    assert!(!out.stored_any);
    assert!(store.revoked.borrow().is_empty());
    let saved = bridge.borrow().draft_queue(&screen);
    assert_eq!(saved.pending.len(), 1);
    assert_eq!(saved.pending["k1"].state, TriageState::Unresolved);
    assert_eq!(
        bridge.borrow().unsynced(&screen)[0].change,
        UnsyncedKind::Undo
    );

    // The next load is online: the undo goes back over the store's state and is revoked.
    store.reachable = true;
    let loaded = block_on(load_decisions(&store, &saved));
    assert_eq!(loaded.bridge.availability(), Availability::Live);
    assert!(!loaded.shown.contains_key("k1"), "still undone on screen");
    assert!(loaded.shown.contains_key("k2"));
    let screen = loaded.shown.clone();
    let bridge = RefCell::new(loaded.bridge);
    let out = sync(&bridge, &store, &screen);
    assert!(out.stored_any);
    assert_eq!(*store.revoked.borrow(), vec!["d1".to_string()]);
    assert!(bridge.borrow().unsynced(&screen).is_empty());
}

#[test]
fn a_load_with_no_rows_leaves_the_draft_untouched_and_stores_nothing() {
    // The server refuses to migrate until the project has scan rows.
    let mut store = FakeStore::new(Vec::new());
    *store.migrated.borrow_mut() = false;
    store.migrate_refuses = true;
    let legacy = map(&[("old\0key", disp(TriageState::Ignored, "fixture"))]);
    let queue = DraftQueue {
        pending: legacy.clone(),
        ..DraftQueue::default()
    };
    let loaded = block_on(load_decisions(&store, &queue));
    assert_eq!(*store.migrations.borrow(), 0);
    assert!(store.decisions.borrow().is_empty() && store.posted.borrow().is_empty());
    assert_ne!(loaded.bridge.availability(), Availability::Live);
    assert!(loaded.banner.is_some());
    // The draft goes back out exactly as it came in.
    assert_eq!(loaded.bridge.draft_queue(&HashMap::new()).pending, legacy);

    // Once rows exist the same load migrates, and the draft's old entries are consumed.
    store.migrate_refuses = false;
    let loaded = block_on(load_decisions(&store, &queue));
    assert_eq!(*store.migrations.borrow(), 1);
    assert_eq!(loaded.bridge.availability(), Availability::Live);
    assert!(
        !loaded.shown.contains_key("old\0key"),
        "the server holds them now"
    );
}

// ── the sync loop ─────────────────────────────────────────────────────────────────────────

#[test]
fn a_refused_key_does_not_block_the_next_key_and_the_error_is_shown() {
    let mut store = FakeStore::new(Vec::new());
    store.refuse.insert(
        "k1".to_string(),
        (422, "ignoring a finding needs a reason".to_string()),
    );
    let bridge = RefCell::new(DecisionBridge::from_decisions(&[]));
    let screen = map(&[
        ("k1", disp(TriageState::Ignored, "")),
        ("k2", disp(TriageState::Confirmed, "")),
        ("k3", disp(TriageState::FalsePositive, "generated")),
    ]);
    let out = sync(&bridge, &store, &screen);

    // k2 and k3, which come after the refused k1, were stored.
    let posted: Vec<String> = store
        .posted
        .borrow()
        .iter()
        .map(|r| r.key.clone())
        .collect();
    assert_eq!(posted, vec!["k2".to_string(), "k3".to_string()]);
    assert!(out.stored_any);
    assert!(out.unreachable.is_none());

    // The refusal is shown, naming the site and the store's reason.
    assert_eq!(out.refused.len(), 1);
    let text = out.refused[0].text();
    assert!(text.contains("src/k1.rs:10"), "{text}");
    assert!(text.contains("ignoring a finding needs a reason"), "{text}");
    assert!(text.contains("decision"));

    // It is not dropped: still pending (so the draft keeps it) and unsynced (so an export waits).
    {
        let b = bridge.borrow();
        assert_eq!(b.draft_queue(&screen).pending.len(), 1);
        assert!(b.draft_queue(&screen).pending.contains_key("k1"));
        assert_eq!(b.unsynced(&screen)[0].key, "k1");
    }

    // It is not sent again unchanged, and is retried the moment the screen changes it.
    let before = store.posted.borrow().len();
    sync(&bridge, &store, &screen);
    assert_eq!(store.posted.borrow().len(), before);
    store.refuse.clear();
    let mut fixed = screen.clone();
    fixed.insert("k1".to_string(), disp(TriageState::Ignored, "dead code"));
    let out = sync(&bridge, &store, &fixed);
    assert!(out.refused.is_empty());
    assert_eq!(store.posted.borrow().len(), before + 1);
    assert!(bridge.borrow().unsynced(&fixed).is_empty());
}

#[test]
fn a_refused_undo_is_shown_as_an_undo_and_does_not_block_the_next_key() {
    let mut store = FakeStore::new(vec![
        record("d1", "k1", DecisionState::Ignored, "fixture"),
        record("d2", "k2", DecisionState::Confirmed, ""),
    ]);
    store
        .refuse_revoke
        .insert("d1".to_string(), (500, "disk full".to_string()));
    let bridge = RefCell::new(DecisionBridge::from_decisions(&store.decisions.borrow()));
    let screen = HashMap::new();
    let out = sync(&bridge, &store, &screen);
    // k1's revoke is refused; k2, after it, was revoked.
    assert_eq!(*store.revoked.borrow(), vec!["d2".to_string()]);
    assert_eq!(out.refused.len(), 1);
    assert!(out.refused[0].text().contains("undo"));
    assert!(out.refused[0].text().contains("disk full"));
    assert_eq!(bridge.borrow().unsynced(&screen).len(), 1);
}

#[test]
fn an_unreachable_store_ends_the_pass_and_keeps_every_change_queued() {
    let mut store = FakeStore::new(Vec::new());
    store.reachable = false;
    let bridge = RefCell::new(DecisionBridge::from_decisions(&[]));
    let screen = map(&[
        ("k1", disp(TriageState::Ignored, "r")),
        ("k2", disp(TriageState::Confirmed, "")),
    ]);
    let out = sync(&bridge, &store, &screen);
    assert!(out.unreachable.is_some());
    assert!(!out.stored_any);
    assert_eq!(bridge.borrow().draft_queue(&screen).pending.len(), 2);
    // A later pass picks them up.
    store.reachable = true;
    let out = sync(&bridge, &store, &screen);
    assert!(out.stored_any);
    assert!(bridge.borrow().unsynced(&screen).is_empty());
}

#[test]
fn a_revoke_the_store_no_longer_finds_counts_as_done() {
    let mut store = FakeStore::new(vec![record("d1", "k1", DecisionState::Ignored, "r")]);
    store
        .refuse_revoke
        .insert("d1".to_string(), (404, "no such decision".to_string()));
    let bridge = RefCell::new(DecisionBridge::from_decisions(&store.decisions.borrow()));
    let screen = HashMap::new();
    let out = sync(&bridge, &store, &screen);
    assert!(out.refused.is_empty());
    assert!(bridge.borrow().unsynced(&screen).is_empty());
}

// ── stamps ────────────────────────────────────────────────────────────────────────────────

#[test]
fn a_change_made_on_run_a_and_flushed_after_run_b_carries_run_a() {
    let store = FakeStore::new(Vec::new());
    let mut bridge = DecisionBridge::from_decisions(&[]);
    let mut screen = map(&[("k1", disp(TriageState::Ignored, "dead code"))]);
    // Decided on run A, at 10:00; nothing flushes it yet.
    bridge.observe(&screen, "insp-A", "2026-11-02T10:00:00Z");
    // Run B arrives, and the reviewer decides another site on it.
    screen.insert("k2".to_string(), disp(TriageState::Confirmed, ""));
    bridge.observe(&screen, "insp-B", "2026-11-03T09:00:00Z");
    // Observing again changes nothing: a stamp is taken once per change.
    bridge.observe(&screen, "insp-B", "2026-11-03T09:05:00Z");

    let bridge = RefCell::new(bridge);
    sync(&bridge, &store, &screen);
    let posted = store.posted.borrow();
    let by_key = |k: &str| posted.iter().find(|r| r.key == k).unwrap();
    assert_eq!(by_key("k1").run_id, "insp-A");
    assert_eq!(by_key("k1").decided_at, "2026-11-02T10:00:00Z");
    assert_eq!(by_key("k2").run_id, "insp-B");
    assert_eq!(by_key("k2").decided_at, "2026-11-03T09:00:00Z");
}

#[test]
fn a_changed_reason_is_stamped_again_and_stamps_survive_the_draft() {
    let mut bridge = DecisionBridge::from_decisions(&[]);
    let mut screen = map(&[("k1", disp(TriageState::Ignored, "first"))]);
    bridge.observe(&screen, "insp-A", "2026-11-02T10:00:00Z");
    screen.insert("k1".to_string(), disp(TriageState::Ignored, "second"));
    bridge.observe(&screen, "insp-B", "2026-11-03T09:00:00Z");

    // Saved to the draft and loaded on the next start: the stamp comes back with its change.
    let queue = bridge.draft_queue(&screen);
    assert_eq!(queue.stamps["k1"].run_id, "insp-B");
    let store = FakeStore::new(Vec::new());
    let loaded = block_on(load_decisions(&store, &queue));
    let screen = loaded.shown.clone();
    let bridge = RefCell::new(loaded.bridge);
    sync(&bridge, &store, &screen);
    let posted = store.posted.borrow();
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0].run_id, "insp-B");
    assert_eq!(posted[0].reason, "second");
    // The bridge learned which run the screen is on from the load.
    assert_eq!(bridge.borrow().run_id(), "insp-2");
}
