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

/// Most toasts to keep on screen at once. A burst (e.g. many story updates in one
/// poll) drops the oldest rather than stacking into a wall — the auto-dismiss timer
/// clears the rest.
const MAX_VISIBLE: usize = 6;

/// Push a toast onto the app-wide stack.
pub fn push_toast(mut list: Signal<Vec<Toast>>, kind: ToastKind, message: impl Into<String>) {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let mut toasts = list.write();
    toasts.push(Toast {
        id,
        kind,
        message: message.into(),
    });
    if toasts.len() > MAX_VISIBLE {
        let drop = toasts.len() - MAX_VISIBLE;
        toasts.drain(0..drop);
    }
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

/// How long a toast stays before auto-dismissing, from `CAMERATA_UI_TOAST_SECS`
/// (default 10s). Applies to ALL toasts so a burst of story updates self-clears
/// rather than piling up; the close X dismisses early.
fn toast_secs() -> u64 {
    std::env::var("CAMERATA_UI_TOAST_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(10)
}

#[component]
fn ToastCard(toast: Toast) -> Element {
    let mut list = use_context::<Signal<Vec<Toast>>>();
    let id = toast.id;
    // EVERY toast auto-dismisses after the configured timer (default 10s), so the
    // user is never bombarded by a stream of updates. Spawned once on mount.
    use_hook(move || {
        let secs = toast_secs();
        spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
            list.write().retain(|t| t.id != id);
        });
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
                title: "Dismiss",
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
    configured: bool,
    ok: Option<bool>,
    #[serde(default)]
    error: Option<String>,
    /// Structured error category for GitHub connections: `no_token`,
    /// `invalid_or_expired`, `insufficient_scope`, or `unreachable`.
    /// Absent for non-GitHub connections or when there is no error.
    #[serde(default)]
    error_category: Option<String>,
    /// Authenticated GitHub login, present when the GitHub probe succeeded.
    #[serde(default)]
    login: Option<String>,
    /// OAuth scopes granted to the GitHub token (classic PATs only; fine-grained
    /// PATs return an empty list).  Captured from the API response for future
    /// scope-diagnostic UI; not yet rendered in toasts.
    #[serde(default)]
    #[allow(dead_code)]
    scopes: Vec<String>,
}

#[derive(Deserialize, Clone)]
struct ConnectionsView {
    connections: Vec<ConnView>,
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
                    if first {
                        // Warn when the GitHub (code host + work tracker) integration
                        // is not configured. Integrations are OPTIONAL, so this is a
                        // warning, not an error — and Claude being present does not
                        // suppress it (the user asked specifically about the code/story
                        // integration).
                        if report
                            .connections
                            .iter()
                            .any(|c| c.id == "github" && !c.configured)
                        {
                            push_toast(
                                toasts,
                                ToastKind::Warning,
                                "No GitHub integration connected (code host + work \
                                 tracker). Set CAMERATA_GITHUB_TOKEN to link real repos \
                                 and stories — optional; Camerata runs on the built-in \
                                 tracker without it.",
                            );
                        }
                    }
                    for c in &report.connections {
                        let prev_ok = prev.get(&c.id).copied();
                        match c.ok {
                            // Newly failing (first probe or a transition to failure).
                            Some(false) if prev_ok != Some(Some(false)) => {
                                // Use the typed error category to produce an
                                // actionable message when available.
                                let detail = match c.error_category.as_deref() {
                                    Some("invalid_or_expired") => {
                                        "token is invalid or expired (HTTP 401). \
                                         Regenerate your CAMERATA_GITHUB_TOKEN."
                                            .to_string()
                                    }
                                    Some("insufficient_scope") => {
                                        "token lacks required scope (HTTP 403). \
                                         Ensure your PAT has repo + read:user scopes."
                                            .to_string()
                                    }
                                    Some("unreachable") => {
                                        "GitHub is unreachable. Check your network or \
                                         GitHub status."
                                            .to_string()
                                    }
                                    _ => c
                                        .error
                                        .clone()
                                        .unwrap_or_else(|| "unreachable".into()),
                                };
                                push_toast(
                                    toasts,
                                    ToastKind::Error,
                                    format!("{} connection failed: {detail}", c.label),
                                );
                            }
                            // Recovered after a failure.
                            Some(true) if matches!(prev_ok, Some(Some(false))) => {
                                let who = c
                                    .login
                                    .as_deref()
                                    .map(|l| format!(" (authenticated as {l})"))
                                    .unwrap_or_default();
                                push_toast(
                                    toasts,
                                    ToastKind::Info,
                                    format!("{} reconnected.{who}", c.label),
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
