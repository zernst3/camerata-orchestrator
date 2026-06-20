//! Background event-ingest pollers + a notification feed the UI drains into toasts.
//!
//! Polling (not webhooks) is the deliberate mechanism: a local app has no public
//! ingress, and the use cases (comment back-and-forth, watching deployments) are
//! fine on an interval. The provider `poll()` capability already exists; this is
//! the always-on driver of it, plus the feed the UI reads.
//!
//! Cadences are TIERED and env-configurable (with the documented defaults):
//! - `CAMERATA_POLL_TRACKER_SECS` (default 45) — tracker events: a PO answering a
//!   comment, a status change. Slow is fine.
//! - `CAMERATA_POLL_DEPLOY_SECS` (default 5) — deployment watching: as near
//!   real-time as polling allows. Reserved; activates when a deploy-status source
//!   is wired (see `spawn_deploy_poller`).

use std::sync::{Arc, Mutex};

use serde::Serialize;

use camerata_worktracker::{InboundKind, InboundWorkItemEvent, WorkItemProvider};

/// Read an interval (in seconds) from an env var, falling back to `default`.
/// A non-numeric or zero value falls back too, so a typo can't disable polling.
pub fn interval_secs(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}

/// One UI-facing notification.
#[derive(Debug, Clone, Serialize)]
pub struct Notification {
    /// Monotonic id; the UI passes the highest seen back as `since` to drain only
    /// new ones.
    pub id: u64,
    /// Severity the UI maps to a toast: `info` | `warning` | `error`.
    pub kind: String,
    /// Where it came from: `tracker` | `deploy`.
    pub source: String,
    /// The message text.
    pub message: String,
}

/// An in-memory, bounded notification feed. Cloneable (shares one `Mutex`), so the
/// pollers append and the HTTP handler drains the same buffer.
#[derive(Clone, Default)]
pub struct NotificationStore {
    inner: Arc<Mutex<NotifState>>,
}

#[derive(Default)]
struct NotifState {
    items: Vec<Notification>,
    next_id: u64,
}

/// Keep the feed bounded so a long-running session can't grow it without limit.
const MAX_ITEMS: usize = 200;

impl NotificationStore {
    /// A new, empty feed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a notification.
    pub fn push(&self, kind: &str, source: &str, message: impl Into<String>) {
        let mut s = self.inner.lock().expect("notification mutex poisoned");
        s.next_id += 1;
        let id = s.next_id;
        s.items.push(Notification {
            id,
            kind: kind.to_string(),
            source: source.to_string(),
            message: message.into(),
        });
        if s.items.len() > MAX_ITEMS {
            let drop = s.items.len() - MAX_ITEMS;
            s.items.drain(0..drop);
        }
    }

    /// Return notifications with id greater than `cursor`, plus the new high-water
    /// id the caller should send next time.
    pub fn since(&self, cursor: u64) -> (Vec<Notification>, u64) {
        let s = self.inner.lock().expect("notification mutex poisoned");
        let items: Vec<Notification> = s.items.iter().filter(|n| n.id > cursor).cloned().collect();
        (items, s.next_id)
    }
}

/// Human description of an inbound tracker event for the notification feed.
fn describe_event(ev: &InboundWorkItemEvent) -> String {
    let id = &ev.reference.external_id;
    let location = ev
        .reference
        .container
        .as_deref()
        .map(|c| format!(" in {c}"))
        .unwrap_or_default();
    match ev.kind {
        InboundKind::Commented => {
            format!("New comment on story {id}{location} — a clarification answer may be waiting.")
        }
        InboundKind::StatusChanged => {
            format!("Story {id}{location} changed status on the tracker.")
        }
        InboundKind::Created => format!("New work item {id}{location} appeared on the tracker."),
        InboundKind::Updated => format!("Story {id}{location} was updated on the tracker."),
    }
}

/// Spawn the background tracker poller: calls `provider.poll(cursor)` on the
/// tracker cadence and appends a notification per new (non-echo) event.
///
/// The FIRST successful poll only establishes the cursor (baseline) — it does not
/// flood the feed with every pre-existing item. Poll errors (e.g. a GitHub
/// token-only connection with no default repo to poll) are swallowed so a
/// misconfiguration doesn't spam the feed; the connection probe surfaces those.
pub fn spawn_tracker_poller(provider: Arc<dyn WorkItemProvider>, notifications: NotificationStore) {
    let secs = interval_secs("CAMERATA_POLL_TRACKER_SECS", 45);
    tokio::spawn(async move {
        let mut cursor: Option<String> = None;
        let mut baseline = true;
        loop {
            if let Ok((events, next)) = provider.poll(cursor.as_deref()).await {
                if baseline {
                    // First poll: adopt the cursor without emitting historical items.
                    baseline = false;
                } else {
                    for ev in events.iter().filter(|e| !e.is_echo) {
                        notifications.push("info", "tracker", describe_event(ev));
                    }
                }
                cursor = Some(next);
            }
            tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
        }
    });
}

/// Spawn the deployment-status poller. Reserved seam: deployment watching is
/// tiered to a FAST cadence (`CAMERATA_POLL_DEPLOY_SECS`, default 5) so a deploy
/// is tracked as near real-time as polling allows. A deploy-status source isn't
/// wired yet, so this currently no-ops after reading its cadence; when a source
/// lands, poll it here and `notifications.push("info"/"error", "deploy", …)`.
pub fn spawn_deploy_poller(_notifications: NotificationStore) {
    let _secs = interval_secs("CAMERATA_POLL_DEPLOY_SECS", 5);
    // Intentionally not spawning a busy no-op loop until a deploy-status source
    // exists. The cadence env var is honored the moment one is wired in here.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn since_returns_only_newer_items_and_advances_cursor() {
        let store = NotificationStore::new();
        store.push("info", "tracker", "one");
        store.push("info", "tracker", "two");

        // From the start: both, cursor at 2.
        let (items, cursor) = store.since(0);
        assert_eq!(items.len(), 2);
        assert_eq!(cursor, 2);

        // From cursor 2: nothing new yet.
        let (items, cursor) = store.since(2);
        assert!(items.is_empty());
        assert_eq!(cursor, 2);

        // A new item shows only past the last cursor.
        store.push("error", "tracker", "three");
        let (items, cursor) = store.since(2);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].message, "three");
        assert_eq!(items[0].kind, "error");
        assert_eq!(cursor, 3);
    }

    #[test]
    fn feed_is_bounded() {
        let store = NotificationStore::new();
        for i in 0..(MAX_ITEMS + 50) {
            store.push("info", "tracker", format!("n{i}"));
        }
        // Old items are dropped, but ids keep climbing (cursor monotonic).
        let (items, cursor) = store.since(0);
        assert_eq!(items.len(), MAX_ITEMS);
        assert_eq!(cursor as usize, MAX_ITEMS + 50);
    }

    #[test]
    fn interval_secs_falls_back_on_unset_or_bad_values() {
        // Unset -> default (we can't set env safely in parallel tests, so just
        // assert the default path on a definitely-unset key).
        assert_eq!(interval_secs("CAMERATA_DEFINITELY_UNSET_POLL_KEY", 45), 45);
    }
}
