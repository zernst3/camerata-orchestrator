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
    reqwest::get(format!("{}/api/connections", crate::bff_base()))
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
    reqwest::get(format!(
        "{}/api/notifications?since={since}",
        crate::bff_base()
    ))
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
                                    _ => c.error.clone().unwrap_or_else(|| "unreachable".into()),
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── Tier 2: network-helper tests (wiremock) ─────────────────────────────────
    // These point a reqwest helper at a fake BFF via the CAMERATA_BFF_URL seam
    // (crate::bff_base) and assert the request it issues + that the JSON body parses
    // into the expected value. CAMERATA_BFF_URL is process-global, so each test sets
    // it, calls the helper, then removes it (don't run these concurrently with other
    // bff_base() readers).

    // fetch_connections GETs /api/connections and deserializes the report. Asserts
    // both the path it hits AND that a representative connections payload parses into
    // the typed ConnectionsView (id/label/configured/ok/error_category/login).
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_connections_gets_endpoint_and_parses_report() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/connections"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "connections": [
                    {
                        "id": "github",
                        "label": "GitHub",
                        "configured": true,
                        "ok": false,
                        "error_category": "invalid_or_expired"
                    },
                    {
                        "id": "claude",
                        "label": "Claude",
                        "configured": true,
                        "ok": true,
                        "login": "octocat"
                    }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let report = super::fetch_connections().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let report = report.expect("the /api/connections payload parses into ConnectionsView");
        assert_eq!(report.connections.len(), 2);
        let gh = &report.connections[0];
        assert_eq!(gh.id, "github");
        assert_eq!(gh.label, "GitHub");
        assert!(gh.configured);
        assert_eq!(gh.ok, Some(false));
        assert_eq!(gh.error_category.as_deref(), Some("invalid_or_expired"));
        let claude = &report.connections[1];
        assert_eq!(claude.ok, Some(true));
        assert_eq!(claude.login.as_deref(), Some("octocat"));
    }

    // fetch_connections returns None when the server errors (non-200 / unparseable),
    // so the watcher loop simply skips that probe rather than panicking.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_connections_returns_none_on_server_error() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/connections"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let report = super::fetch_connections().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(report.is_none(), "a 500 yields None, not a panic");
    }

    // fetch_notifications GETs /api/notifications with the `since` cursor as a query
    // param and parses the feed (notifications + cursor high-water mark). Asserts the
    // path, the exact `since` query value, and that the body deserializes.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_notifications_passes_since_cursor_and_parses_feed() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/notifications"))
            .and(query_param("since", "42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "notifications": [
                    { "kind": "error", "message": "a story failed" },
                    { "kind": "info", "message": "a story moved" }
                ],
                "cursor": 99
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let feed = super::fetch_notifications(42).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let feed = feed.expect("the /api/notifications payload parses into NotificationsFeed");
        assert_eq!(feed.cursor, 99);
        assert_eq!(feed.notifications.len(), 2);
        assert_eq!(feed.notifications[0].kind, "error");
        assert_eq!(feed.notifications[0].message, "a story failed");
        assert_eq!(feed.notifications[1].kind, "info");
    }

    // ── Tier 1: render tests (dioxus-ssr) ───────────────────────────────────────
    // Render a component headlessly (VirtualDom + dioxus-ssr) and assert its static
    // STRUCTURE. Components here read a Signal<Vec<Toast>> from context, so the
    // harness root MUST provide it before rendering, else use_context panics.

    // ToastCard renders the severity label, the message, and the dismiss button for a
    // given toast. Verifies the Warning kind maps to the "HEADS UP" label and that the
    // message text + close affordance are present.
    #[test]
    fn toast_card_renders_label_message_and_dismiss() {
        fn harness() -> Element {
            use_context_provider(|| Signal::new(Vec::<Toast>::new()));
            rsx! {
                ToastCard {
                    toast: Toast {
                        id: 1,
                        kind: ToastKind::Warning,
                        message: "nothing connected".to_string(),
                    },
                }
            }
        }

        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);

        assert!(
            html.contains("HEADS UP"),
            "Warning renders the HEADS UP label; html=\n{html}"
        );
        assert!(
            html.contains("nothing connected"),
            "the message text renders; html=\n{html}"
        );
        assert!(
            html.contains("toast-close"),
            "the dismiss button renders; html=\n{html}"
        );
    }

    // ToastCard maps the Error kind to the "ERROR" label + the `toast error` class.
    #[test]
    fn toast_card_error_kind_renders_error_label_and_class() {
        fn harness() -> Element {
            use_context_provider(|| Signal::new(Vec::<Toast>::new()));
            rsx! {
                ToastCard {
                    toast: Toast {
                        id: 2,
                        kind: ToastKind::Error,
                        message: "GitHub connection failed".to_string(),
                    },
                }
            }
        }

        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);

        assert!(
            html.contains("ERROR"),
            "Error renders the ERROR label; html=\n{html}"
        );
        assert!(
            html.contains("toast error"),
            "Error renders the `toast error` class; html=\n{html}"
        );
        assert!(
            html.contains("GitHub connection failed"),
            "the message text renders; html=\n{html}"
        );
    }

    // ToastHost reads the app-wide Signal<Vec<Toast>> from context and renders one
    // card per toast inside the `toast-host` container. Seed the context with two
    // toasts and assert both messages render under the host.
    #[test]
    fn toast_host_renders_a_card_per_toast() {
        fn harness() -> Element {
            use_context_provider(|| {
                Signal::new(vec![
                    Toast {
                        id: 1,
                        kind: ToastKind::Info,
                        message: "first toast".to_string(),
                    },
                    Toast {
                        id: 2,
                        kind: ToastKind::Error,
                        message: "second toast".to_string(),
                    },
                ])
            });
            rsx! { ToastHost {} }
        }

        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);

        assert!(
            html.contains("toast-host"),
            "the host container renders; html=\n{html}"
        );
        assert!(
            html.contains("first toast") && html.contains("second toast"),
            "one card renders per toast in the signal; html=\n{html}"
        );
        assert!(
            html.contains("INFO") && html.contains("ERROR"),
            "each card renders its own severity label; html=\n{html}"
        );
    }
}
