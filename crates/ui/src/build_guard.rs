//! The app never runs against a server from a different build.
//!
//! `cargo run -p camerata-ui` rebuilds only the app, and the app starts (or attaches to) the
//! `camerata-server` binary it finds. After a pull that can pair new app code with an old server
//! with nothing to show it. Both binaries record the git commit they were built from
//! (`CAMERATA_GIT_COMMIT`, written by a build script); the server reports it on `/api/health` and
//! [`BuildGuard`] compares it with the app's own before showing the cockpit.

use std::time::{Duration, Instant};

use dioxus::prelude::*;

/// The commit this app was built from, recorded by `build.rs` (`CAMERATA_GIT_COMMIT`). `None` for
/// a build outside a git checkout: an honest absence, never a placeholder.
pub const UI_COMMIT: Option<&str> = option_env!("CAMERATA_GIT_COMMIT");

/// The build command that makes both binaries agree.
pub const REBUILD_COMMAND: &str = "cargo build -p camerata-server -p camerata-ui";

/// How long the app waits for the server to answer before it stops waiting and shows the cockpit
/// (whose own connection errors then say what is wrong).
const WAIT_FOR_SERVER: Duration = Duration::from_secs(45);

/// What a healthy server said about the build it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerBuild {
    pub commit: Option<String>,
    pub dirty: bool,
}

/// Compare the app's build with the server's: `true` when they agree. Commits are compared; the
/// dirty flag is shown but not compared, because an edit to one crate rebuilds only that crate and
/// would trip the guard on every ordinary edit. A server that reports no commit never matches an
/// app that has one (an old server), and neither side knowing its commit (both built outside git)
/// is not a mismatch: there is nothing to compare.
pub fn builds_match(ui: Option<&str>, server: &ServerBuild) -> bool {
    match (ui, server.commit.as_deref()) {
        (None, None) => true,
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// One probe of `GET {base}/api/health`: `Some` when a camerata server answered healthy.
async fn probe(client: &reqwest::Client, base: &str) -> Option<ServerBuild> {
    let url = format!("{}/api/health", base.trim_end_matches('/'));
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body = resp.json::<serde_json::Value>().await.ok()?;
    if body.get("service").and_then(|s| s.as_str()) != Some("camerata-server") {
        return None;
    }
    Some(ServerBuild {
        commit: body
            .get("commit")
            .and_then(|c| c.as_str())
            .map(str::to_string),
        dirty: body.get("dirty").and_then(|d| d.as_bool()).unwrap_or(false),
    })
}

/// Ask the server which build it is, retrying until it answers or `give_up` passes (the app starts
/// the server itself, so the first probes usually find nothing yet). `None` means it never answered.
pub async fn fetch_server_build(base: &str, give_up: Duration) -> Option<ServerBuild> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(750))
        .build()
        .unwrap_or_default();
    let started = Instant::now();
    loop {
        if let Some(build) = probe(&client, base).await {
            return Some(build);
        }
        if started.elapsed() >= give_up {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// The first eight characters of a commit, the way `git log --oneline` shortens it.
fn short(commit: &str) -> &str {
    commit.get(..8).unwrap_or(commit)
}

fn describe_ui(ui: Option<&str>) -> String {
    ui.map_or_else(
        || "an unknown build".to_string(),
        |c| format!("commit {}", short(c)),
    )
}

fn describe_server(server: &ServerBuild) -> String {
    let base = server.commit.as_deref().map_or_else(
        || "an unknown build".to_string(),
        |c| format!("commit {}", short(c)),
    );
    if server.dirty {
        format!("{base}, with uncommitted changes")
    } else {
        base
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct BuildGuardProps {
    /// The commit this app was built from (`None` for a build outside a git checkout).
    pub ui_build: Option<String>,
    pub children: Element,
}

/// Shows `children` (the cockpit) only once the server answering on the BFF address is from the
/// same build as this app. While the server has not answered yet it shows a short waiting line; if
/// it never answers within [`WAIT_FOR_SERVER`] the cockpit shows, because its own connection
/// errors say what is wrong. A different or unreported build shows [`MismatchScreen`] instead.
#[component]
pub fn BuildGuard(props: BuildGuardProps) -> Element {
    let check =
        use_resource(|| async { fetch_server_build(&crate::bff_base(), WAIT_FOR_SERVER).await });
    let ui_build = props.ui_build.clone();
    let verdict = check.read().clone();
    match verdict {
        None => rsx! {
            div { class: "build-guard-wait", "Starting the server..." }
        },
        Some(Some(server)) if !builds_match(ui_build.as_deref(), &server) => rsx! {
            MismatchScreen { ui_build, server }
        },
        Some(_) => rsx! { {props.children} },
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct MismatchScreenProps {
    pub ui_build: Option<String>,
    pub server: ServerBuild,
}

/// The one screen shown instead of the cockpit when the app and its server differ.
#[component]
pub fn MismatchScreen(props: MismatchScreenProps) -> Element {
    let ui = describe_ui(props.ui_build.as_deref());
    let server = describe_server(&props.server);
    let reason = if props.server.commit.is_none() {
        "The server does not say which build it is, so it is older than this app."
    } else {
        "They must come from the same build before the app can be trusted."
    };
    rsx! {
        div { class: "build-guard-mismatch card",
            h2 { class: "entity-name", "This app and its server come from different builds" }
            p { "The app is built from {ui}. The server is built from {server}. {reason}" }
            p { "Rebuild both, then start the app again:" }
            pre { class: "build-guard-command", "{REBUILD_COMMAND}" }
            button {
                class: "btn-primary",
                onclick: move |_| dioxus::desktop::window().close(),
                "Quit"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_test_env::EnvGuard;
    use serial_test::serial;

    const UI_COMMIT: &str = "1111111111111111111111111111111111111111";
    const OLD_COMMIT: &str = "2222222222222222222222222222222222222222";
    const COMMAND: &str = "cargo build -p camerata-server -p camerata-ui";

    #[derive(Clone, PartialEq, Props)]
    struct Harness {
        ui_build: Option<String>,
    }

    #[component]
    fn GuardedCockpit(props: Harness) -> Element {
        rsx! {
            BuildGuard { ui_build: props.ui_build.clone(),
                div { class: "the-cockpit", "COCKPIT-RENDERED" }
            }
        }
    }

    /// Mount the real guard against a stub server answering `/api/health` with `health`, let the
    /// check settle, and return the markup the operator sees.
    async fn screen_against(health: serde_json::Value, ui_build: Option<&str>) -> String {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/health"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(health))
            .mount(&server)
            .await;
        let _env = EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        let mut vdom = VirtualDom::new_with_props(
            GuardedCockpit,
            Harness {
                ui_build: ui_build.map(str::to_string),
            },
        );
        vdom.rebuild_in_place();
        for _ in 0..20 {
            let _ =
                tokio::time::timeout(std::time::Duration::from_millis(100), vdom.wait_for_work())
                    .await;
            vdom.render_immediate(&mut dioxus::dioxus_core::NoOpMutations);
        }
        dioxus_ssr::render(&vdom)
    }

    fn assert_mismatch_screen(html: &str) {
        assert!(
            html.contains("This app and its server come from different builds"),
            "{html}"
        );
        assert!(html.contains(COMMAND), "{html}");
        assert!(html.contains("Quit"), "{html}");
        assert!(
            !html.contains("COCKPIT-RENDERED"),
            "the cockpit must not render: {html}"
        );
    }

    #[tokio::test]
    #[serial(process_env)]
    async fn a_server_from_a_different_build_shows_the_mismatch_screen_not_the_cockpit() {
        let html = screen_against(
            serde_json::json!({ "status": "ok", "service": "camerata-server",
                                "commit": OLD_COMMIT, "dirty": false }),
            Some(UI_COMMIT),
        )
        .await;
        assert_mismatch_screen(&html);
        assert!(html.contains("11111111"), "names the app build: {html}");
        assert!(html.contains("22222222"), "names the server build: {html}");
    }

    #[tokio::test]
    #[serial(process_env)]
    async fn a_server_that_reports_no_build_is_a_mismatch() {
        let html = screen_against(
            serde_json::json!({ "status": "ok", "service": "camerata-server" }),
            Some(UI_COMMIT),
        )
        .await;
        assert_mismatch_screen(&html);
        assert!(
            html.contains("does not say which build it is"),
            "explains the old server: {html}"
        );
    }

    #[tokio::test]
    #[serial(process_env)]
    async fn a_null_build_from_the_server_is_a_mismatch() {
        let html = screen_against(
            serde_json::json!({ "status": "ok", "service": "camerata-server",
                                "commit": null, "dirty": false }),
            Some(UI_COMMIT),
        )
        .await;
        assert_mismatch_screen(&html);
    }

    #[tokio::test]
    #[serial(process_env)]
    async fn a_matching_build_renders_the_cockpit() {
        let html = screen_against(
            serde_json::json!({ "status": "ok", "service": "camerata-server",
                                "commit": UI_COMMIT, "dirty": false }),
            Some(UI_COMMIT),
        )
        .await;
        assert!(html.contains("COCKPIT-RENDERED"), "{html}");
        assert!(
            !html.contains("come from different builds"),
            "no mismatch screen: {html}"
        );
    }

    #[test]
    fn builds_match_compares_commits_and_treats_a_silent_server_as_a_mismatch() {
        let server = |c: Option<&str>| ServerBuild {
            commit: c.map(str::to_string),
            dirty: false,
        };
        assert!(builds_match(Some(UI_COMMIT), &server(Some(UI_COMMIT))));
        assert!(!builds_match(Some(UI_COMMIT), &server(Some(OLD_COMMIT))));
        assert!(!builds_match(Some(UI_COMMIT), &server(None)));
        assert!(!builds_match(None, &server(Some(OLD_COMMIT))));
        assert!(builds_match(None, &server(None)));
        // The dirty flag is information, not part of the comparison.
        let dirty = ServerBuild {
            commit: Some(UI_COMMIT.to_string()),
            dirty: true,
        };
        assert!(builds_match(Some(UI_COMMIT), &dirty));
    }

    #[tokio::test]
    async fn a_server_that_never_answers_gives_none_after_the_wait() {
        // Nothing listens on this port.
        let got = fetch_server_build("http://127.0.0.1:9", Duration::from_millis(300)).await;
        assert_eq!(got, None);
    }
}
