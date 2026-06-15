//! App-wide toast notifications + a connection watcher.
//!
//! A single `Signal<Vec<Toast>>` is provided at the app root; any component pushes
//! to it via [`push_toast`], and [`ToastHost`] renders the stack (top-right).
//! Info/warning toasts auto-dismiss; errors persist until dismissed, since a
//! failing connection is something the user should act on.
//!
//! [`ConnectionWatcher`] probes `/api/connections` on startup and on a slow
//! cadence: a WARNING when nothing is configured (running unconnected is a valid
//! choice, not an error), an ERROR when a configured integration is failing
//! (401/403/5xx), and an INFO when one recovers. This is the real-time-ish health
//! signal; ingesting tracker events (a PO answering a comment) into toasts is the
//! next build (see docs/UI_BACKLOG.md) and rides the same provider `poll()`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use dioxus::prelude::*;
use serde::Deserialize;

/// Toast severity.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    /// Neutral information (e.g. a connection recovered).
    Info,
    /// Something to be aware of, but not an error (e.g. nothing connected).
    Warning,
    /// A failure the user should act on (e.g. a 403 from GitHub).
    Error,
}

/// One toast.
#[derive(Clone, PartialEq)]
pub struct Toast {
    /// Unique id (for keying + dismissal).
    pub id: u64,
    /// Severity.
    pub kind: ToastKind,
    /// The message text.
    pub message: String,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Push a toast onto the app-wide stack.
pub fn push_toast(mut list: Signal<Vec<Toast>>, kind: ToastKind, message: impl Into<String>) {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    list.write().push(Toast {
        id,
        kind,
        message: message.into(),
    });
}

/// Renders the toast stack. Reads the app-wide `Signal<Vec<Toast>>` from context.
#[component]
pub fn ToastHost() -> Element {
    let list = use_context::<Signal<Vec<Toast>>>();
    rsx! {
        div { class: "toast-host",
            for t in list.read().iter().cloned() {
                ToastCard { key: "{t.id}", toast: t }
            }
        }
    }
}

#[component]
fn ToastCard(toast: Toast) -> Element {
    let mut list = use_context::<Signal<Vec<Toast>>>();
    let id = toast.id;
    // Info/warning auto-dismiss; errors stay until dismissed. Spawned once on mount.
    let auto_dismiss = !matches!(toast.kind, ToastKind::Error);
    use_hook(move || {
        if auto_dismiss {
            spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(9)).await;
                list.write().retain(|t| t.id != id);
            });
        }
    });

    let (cls, label) = match toast.kind {
        ToastKind::Info => ("toast info", "INFO"),
        ToastKind::Warning => ("toast warning", "HEADS UP"),
        ToastKind::Error => ("toast error", "ERROR"),
    };
    rsx! {
        div { class: "{cls}",
            span { class: "toast-label", "{label}" }
            span { class: "toast-msg", "{toast.message}" }
            button {
                class: "toast-close",
                onclick: move |_| {
                    list.write().retain(|t| t.id != id);
                },
                "\u{00d7}"
            }
        }
    }
}

// ── Connection watcher ─────────────────────────────────────────────────────────

/// One integration's status as `/api/connections` reports it. Extra server-side
/// fields (role) are ignored by serde.
#[derive(Deserialize, Clone)]
struct ConnView {
    id: String,
    label: String,
    #[allow(dead_code)]
    configured: bool,
    ok: Option<bool>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize, Clone)]
struct ConnectionsView {
    connections: Vec<ConnView>,
    any_configured: bool,
}

async fn fetch_connections() -> Option<ConnectionsView> {
    reqwest::get(format!("{}/api/connections", crate::BFF_URL))
        .await
        .ok()?
        .json::<ConnectionsView>()
        .await
        .ok()
}

// ── Notification poller ────────────────────────────────────────────────────────

/// One notification from the BFF feed. Extra fields (id, source) are ignored.
#[derive(Deserialize, Clone)]
struct NotificationView {
    kind: String,
    message: String,
}

#[derive(Deserialize, Clone)]
struct NotificationsFeed {
    notifications: Vec<NotificationView>,
    cursor: u64,
}

async fn fetch_notifications(since: u64) -> Option<NotificationsFeed> {
    reqwest::get(format!("{}/api/notifications?since={since}", crate::BFF_URL))
        .await
        .ok()?
        .json::<NotificationsFeed>()
        .await
        .ok()
}

/// Invisible component: drains the BFF notification feed on a cadence and pushes
/// each new event as a toast. The feed is fed by the server-side event-ingest
/// pollers (tracker events now; deploy status when wired). The UI drain cadence is
/// `CAMERATA_UI_NOTIFY_SECS` (default 5s) so server-ingested events surface
/// quickly. Mount once near the app root.
#[component]
pub fn NotificationPoller() -> Element {
    let toasts = use_context::<Signal<Vec<Toast>>>();
    use_hook(move || {
        let secs = std::env::var("CAMERATA_UI_NOTIFY_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(5);
        spawn(async move {
            // Start from the current high-water mark so we only toast NEW events,
            // not whatever was already in the feed when the app launched.
            let mut cursor = match fetch_notifications(0).await {
                Some(feed) => feed.cursor,
                None => 0,
            };
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                if let Some(feed) = fetch_notifications(cursor).await {
                    for n in &feed.notifications {
                        let kind = match n.kind.as_str() {
                            "error" => ToastKind::Error,
                            "warning" => ToastKind::Warning,
                            _ => ToastKind::Info,
                        };
                        push_toast(toasts, kind, n.message.clone());
                    }
                    cursor = feed.cursor;
                }
            }
        });
    });
    rsx! {}
}

/// Invisible component: probes connection health on startup and on a slow cadence,
/// pushing toasts on the initial state and on any transition. Mount it once near
/// the app root.
#[component]
pub fn ConnectionWatcher() -> Element {
    let toasts = use_context::<Signal<Vec<Toast>>>();
    use_hook(move || {
        spawn(async move {
            // Previous `ok` per integration id, to toast only on transitions.
            let mut prev: HashMap<String, Option<bool>> = HashMap::new();
            let mut first = true;
            loop {
                if let Some(report) = fetch_connections().await {
                    if first && !report.any_configured {
                        push_toast(
                            toasts,
                            ToastKind::Warning,
                            "No integrations connected. Camerata is running on the built-in \
                             tracker — connect GitHub (and Claude) to link real repos, \
                             stories, and agents. See docs/USER_GUIDE.md.",
                        );
                    }
                    for c in &report.connections {
                        let prev_ok = prev.get(&c.id).copied();
                        match c.ok {
                            // Newly failing (first probe or a transition to failure).
                            Some(false) if prev_ok != Some(Some(false)) => {
                                let detail = c.error.clone().unwrap_or_else(|| "unreachable".into());
                                push_toast(
                                    toasts,
                                    ToastKind::Error,
                                    format!("{} connection failed: {detail}", c.label),
                                );
                            }
                            // Recovered after a failure.
                            Some(true) if matches!(prev_ok, Some(Some(false))) => {
                                push_toast(
                                    toasts,
                                    ToastKind::Info,
                                    format!("{} reconnected.", c.label),
                                );
                            }
                            _ => {}
                        }
                        prev.insert(c.id.clone(), c.ok);
                    }
                    first = false;
                }
                // Slow re-check so a connection that breaks (token revoked, GitHub
                // down) surfaces without the user re-launching.
                tokio::time::sleep(std::time::Duration::from_secs(45)).await;
            }
        });
    });
    rsx! {}
}
