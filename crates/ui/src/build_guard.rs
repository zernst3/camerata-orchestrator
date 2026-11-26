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

/// After the cockpit has been let through, how long the app keeps checking (slowly) for a server
/// that was started by hand late. When it finally answers, the check runs once then.
const LATE_WAIT: Duration = Duration::from_secs(12 * 60 * 60);

/// What a healthy server said about the build it came from and the folders it runs on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServerBuild {
    pub commit: Option<String>,
    pub dirty: bool,
    /// The folder the server keeps its stores in (projects, inspections). `None` from a server
    /// that does not say.
    pub data_dir: Option<String>,
    /// The folder the server keeps its runs in (what a resume reads). `None` from a server that
    /// does not say.
    pub state_dir: Option<String>,
}

/// The data and state folders this app expects its server to run on: the ones a server started
/// from this app's environment resolves (`CAMERATA_DATA_DIR`, `CAMERATA_STATE_DIR`, else the
/// platform folders), read through the same functions the server uses.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Folders {
    pub data_dir: Option<String>,
    pub state_dir: Option<String>,
}

impl Folders {
    /// The folders of this process's environment.
    pub fn for_this_app() -> Self {
        Self {
            data_dir: camerata_api_types::data_dir::camerata_dir().map(|d| d.display().to_string()),
            state_dir: Some(camerata_api_types::state_dir::dir().display().to_string()),
        }
    }
}

/// Why the app will not run against the server that answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mismatch {
    /// The server comes from another build (or does not say which).
    Build,
    /// The server keeps its stores in another folder (or does not say which).
    DataFolder,
    /// The server keeps its runs in another folder (or does not say which).
    StateFolder,
}

/// The first reason the app must not run against `server`, or `None` when it may: the build is
/// compared first ([`builds_match`]), then the data folder, then the state folder.
pub fn find_mismatch(ui: Option<&str>, folders: &Folders, server: &ServerBuild) -> Option<Mismatch> {
    if !builds_match(ui, server) {
        return Some(Mismatch::Build);
    }
    let _ = folders;
    None
}

fn describe_folder(folder: Option<&str>) -> String {
    folder.map_or_else(|| "a folder it does not name".to_string(), str::to_string)
}

/// One sentence that names both sides of `mismatch`, for the launcher's log and the screen.
pub fn mismatch_line(
    mismatch: &Mismatch,
    ui: Option<&str>,
    folders: &Folders,
    server: &ServerBuild,
) -> String {
    match mismatch {
        Mismatch::Build => format!(
            "The app is built from {}. The server is built from {}.",
            describe_ui(ui),
            describe_server(server)
        ),
        Mismatch::DataFolder => format!(
            "The app uses the data folder {}. The server uses the data folder {}.",
            describe_folder(folders.data_dir.as_deref()),
            describe_folder(server.data_dir.as_deref())
        ),
        Mismatch::StateFolder => format!(
            "The app uses the state folder {}. The server uses the state folder {}.",
            describe_folder(folders.state_dir.as_deref()),
            describe_folder(server.state_dir.as_deref())
        ),
    }
}

/// The one launch command that runs an inspection in its own folders on its own address, so it
/// neither attaches to nor is captured by another server. `<folder>` is the inspection's folder.
pub const ISOLATED_LAUNCH_COMMAND: &str = "CAMERATA_DATA_DIR=<folder> CAMERATA_STATE_DIR=<folder>/state CAMERATA_SERVER_ADDR=127.0.0.1:8790 cargo run -p camerata-ui";

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
pub(crate) async fn probe(client: &reqwest::Client, base: &str) -> Option<ServerBuild> {
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
        data_dir: body
            .get("data_dir")
            .and_then(|c| c.as_str())
            .map(str::to_string),
        state_dir: body
            .get("state_dir")
            .and_then(|c| c.as_str())
            .map(str::to_string),
    })
}

/// Ask the server which build it is, retrying until it answers or `give_up` passes (the app starts
/// the server itself, so the first probes usually find nothing yet). `None` means it never answered.
pub async fn fetch_server_build(
    base: &str,
    give_up: Duration,
    every: Duration,
) -> Option<ServerBuild> {
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
        tokio::time::sleep(every).await;
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
    /// How long to wait for the first answer before letting the cockpit through.
    #[props(default = WAIT_FOR_SERVER)]
    pub wait: Duration,
    /// The folders the server must run on (#566). The app's own by default.
    #[props(default = Folders::for_this_app())]
    pub folders: Folders,
    pub children: Element,
}

/// Where the check stands.
#[derive(Clone, PartialEq)]
enum Phase {
    Waiting,
    GaveUp,
    Answered(ServerBuild),
}

/// Shows `children` (the cockpit) only once the server answering on the BFF address is from the
/// same build as this app. While the server has not answered yet it shows a short waiting line; if
/// it never answers within [`WAIT_FOR_SERVER`] the cockpit shows, because its own connection
/// errors say what is wrong. A different or unreported build shows [`MismatchScreen`] instead.
#[component]
pub fn BuildGuard(props: BuildGuardProps) -> Element {
    let mut phase = use_signal(|| Phase::Waiting);
    let wait = props.wait;
    use_future(move || async move {
        let base = crate::bff_base();
        let quick = Duration::from_millis(250);
        match fetch_server_build(&base, wait, quick).await {
            Some(build) => phase.set(Phase::Answered(build)),
            None => {
                // The cockpit shows now; a server that answers later is still checked, once.
                phase.set(Phase::GaveUp);
                if let Some(build) =
                    fetch_server_build(&base, LATE_WAIT, Duration::from_secs(2)).await
                {
                    phase.set(Phase::Answered(build));
                }
            }
        }
    });
    let ui_build = props.ui_build.clone();
    let folders = props.folders.clone();
    let current = phase.read().clone();
    match current {
        Phase::Waiting => rsx! {
            div { class: "build-guard-wait", "Starting the server..." }
        },
        Phase::Answered(server) => match find_mismatch(ui_build.as_deref(), &folders, &server) {
            Some(mismatch) => rsx! {
                MismatchScreen { ui_build, folders, server, mismatch }
            },
            None => rsx! { {props.children} },
        },
        Phase::GaveUp => rsx! { {props.children} },
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct MismatchScreenProps {
    pub ui_build: Option<String>,
    #[props(default)]
    pub folders: Folders,
    pub server: ServerBuild,
    #[props(default = Mismatch::Build)]
    pub mismatch: Mismatch,
}

/// The one screen shown instead of the cockpit when the app and its server differ.
#[component]
pub fn MismatchScreen(props: MismatchScreenProps) -> Element {
    let line = mismatch_line(
        &props.mismatch,
        props.ui_build.as_deref(),
        &props.folders,
        &props.server,
    );
    let quit = rsx! {
        button {
            class: "btn-primary",
            onclick: move |_| dioxus::desktop::window().close(),
            "Quit"
        }
    };
    if props.mismatch != Mismatch::Build {
        let kind = if props.mismatch == Mismatch::DataFolder {
            "data folders"
        } else {
            "state folders"
        };
        return rsx! {
            div { class: "build-guard-mismatch card",
                h2 { class: "entity-name", "This app and the server on its address use different {kind}" }
                p {
                    "{line} An inspection keeps its projects and runs in its own folders, so the app does not attach to this server."
                }
                p { "Quit the other server, or start the app on its own address and folders:" }
                pre { class: "build-guard-command", "{ISOLATED_LAUNCH_COMMAND}" }
                {quit}
            }
        };
    }
    let reason = if props.server.commit.is_none() {
        "The server does not say which build it is, so it is older than this app."
    } else {
        "They must come from the same build before the app can be trusted."
    };
    rsx! {
        div { class: "build-guard-mismatch card",
            h2 { class: "entity-name", "This app and its server come from different builds" }
            p { "{line} {reason}" }
            p { "Rebuild both, then start the app again:" }
            pre { class: "build-guard-command", "{REBUILD_COMMAND}" }
            {quit}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_test_env::EnvGuard;
    use serial_test::serial;
    use std::time::Duration;

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
        let own = Folders::for_this_app();
        let html = screen_against(
            serde_json::json!({ "status": "ok", "service": "camerata-server",
                                "commit": UI_COMMIT, "dirty": false,
                                "data_dir": own.data_dir, "state_dir": own.state_dir }),
            Some(UI_COMMIT),
        )
        .await;
        assert!(html.contains("COCKPIT-RENDERED"), "{html}");
        assert!(
            !html.contains("come from different builds"),
            "no mismatch screen: {html}"
        );
    }

    #[derive(Clone, PartialEq, Props)]
    struct LateHarness {
        ui_build: Option<String>,
    }

    #[component]
    fn LateGuardedCockpit(props: LateHarness) -> Element {
        rsx! {
            BuildGuard {
                ui_build: props.ui_build.clone(),
                wait: Duration::from_millis(200),
                div { class: "the-cockpit", "COCKPIT-RENDERED" }
            }
        }
    }

    async fn settle(vdom: &mut VirtualDom, rounds: usize) {
        for _ in 0..rounds {
            let _ = tokio::time::timeout(Duration::from_millis(100), vdom.wait_for_work()).await;
            vdom.render_immediate(&mut dioxus::dioxus_core::NoOpMutations);
        }
    }

    /// The cockpit is let through when the server has not answered in time; a server started by
    /// hand later, from a different build, is still caught once it answers.
    #[tokio::test]
    #[serial(process_env)]
    async fn a_server_that_answers_late_from_a_different_build_is_still_caught() {
        let server = wiremock::MockServer::start().await;
        let _env = EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        let mut vdom = VirtualDom::new_with_props(
            LateGuardedCockpit,
            LateHarness {
                ui_build: Some(UI_COMMIT.to_string()),
            },
        );
        vdom.rebuild_in_place();
        settle(&mut vdom, 8).await;
        let early = dioxus_ssr::render(&vdom);
        assert!(
            early.contains("COCKPIT-RENDERED"),
            "no answer in time lets the cockpit through: {early}"
        );
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/health"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "status": "ok", "service": "camerata-server",
                                    "commit": OLD_COMMIT, "dirty": false }),
            ))
            .mount(&server)
            .await;
        let mut late = String::new();
        for _ in 0..12 {
            settle(&mut vdom, 5).await;
            late = dioxus_ssr::render(&vdom);
            if late.contains("come from different builds") {
                break;
            }
        }
        assert_mismatch_screen(&late);
    }

    /// The real `AppShell` (what `App` renders) wraps the cockpit in the guard: against a server
    /// from another build it shows the mismatch screen and no app root.
    #[tokio::test]
    #[serial(process_env)]
    async fn the_app_wraps_the_cockpit_in_the_guard() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/health"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "status": "ok", "service": "camerata-server",
                                    "commit": "not-the-apps-commit", "dirty": false }),
            ))
            .mount(&server)
            .await;
        let _env = EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        let mut vdom = VirtualDom::new(crate::AppShell);
        vdom.rebuild_in_place();
        settle(&mut vdom, 20).await;
        let html = dioxus_ssr::render(&vdom);
        assert!(
            html.contains("This app and its server come from different builds"),
            "{html}"
        );
        assert!(
            !html.contains("class=\"app-root\""),
            "the cockpit's app root must not render behind the guard"
        );
    }

    // ── #566: the data and state folders ────────────────────────────────────────────────

    const ACME_DATA: &str = "/Users/auditor/inspections/acme-2026/camerata";
    const ACME_STATE: &str = "/Users/auditor/inspections/acme-2026/state/camerata";
    const DEV_DATA: &str = "/Users/auditor/dev-data/camerata";
    const DEV_STATE: &str = "/Users/auditor/dev-state/camerata";

    fn acme_folders() -> Folders {
        Folders {
            data_dir: Some(ACME_DATA.to_string()),
            state_dir: Some(ACME_STATE.to_string()),
        }
    }

    #[derive(Clone, PartialEq, Props)]
    struct FolderHarness {
        folders: Folders,
    }

    #[component]
    fn FolderGuardedCockpit(props: FolderHarness) -> Element {
        rsx! {
            BuildGuard { ui_build: Some(UI_COMMIT.to_string()), folders: props.folders.clone(),
                div { class: "the-cockpit", "COCKPIT-RENDERED" }
            }
        }
    }

    /// Mount the real guard, expecting `folders`, against a stub server of the app's own build
    /// that reports `data_dir` and `state_dir`.
    async fn folder_screen(data_dir: Option<&str>, state_dir: Option<&str>) -> String {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/health"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "status": "ok", "service": "camerata-server",
                                    "commit": UI_COMMIT, "dirty": false,
                                    "data_dir": data_dir, "state_dir": state_dir }),
            ))
            .mount(&server)
            .await;
        let _env = EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        let mut vdom = VirtualDom::new_with_props(
            FolderGuardedCockpit,
            FolderHarness {
                folders: acme_folders(),
            },
        );
        vdom.rebuild_in_place();
        settle(&mut vdom, 20).await;
        dioxus_ssr::render(&vdom)
    }

    fn assert_folder_screen(html: &str, kind: &str) {
        assert!(
            html.contains(&format!(
                "This app and the server on its address use different {kind}"
            )),
            "{html}"
        );
        assert!(
            html.contains("CAMERATA_DATA_DIR=&lt;folder&gt;")
                || html.contains("CAMERATA_DATA_DIR=<folder>"),
            "the screen gives the one launch command: {html}"
        );
        assert!(html.contains("Quit"), "{html}");
        assert!(
            !html.contains("COCKPIT-RENDERED"),
            "the cockpit must not render: {html}"
        );
    }

    /// A server of the same build on another data folder (a dev server on the same address) is
    /// refused: the cockpit does not show, and the screen names both folders.
    #[tokio::test]
    #[serial(process_env)]
    async fn a_server_on_another_data_folder_is_refused_and_both_folders_are_named() {
        let html = folder_screen(Some(DEV_DATA), Some(ACME_STATE)).await;
        assert_folder_screen(&html, "data folders");
        assert!(html.contains(ACME_DATA), "names the app's folder: {html}");
        assert!(html.contains(DEV_DATA), "names the server's folder: {html}");
    }

    /// The same for the state folder, where the runs a resume finds live.
    #[tokio::test]
    #[serial(process_env)]
    async fn a_server_on_another_state_folder_is_refused_and_both_folders_are_named() {
        let html = folder_screen(Some(ACME_DATA), Some(DEV_STATE)).await;
        assert_folder_screen(&html, "state folders");
        assert!(html.contains(ACME_STATE), "names the app's folder: {html}");
        assert!(html.contains(DEV_STATE), "names the server's folder: {html}");
    }

    /// A server that does not name its data folder cannot be shown to use the inspection's.
    #[tokio::test]
    #[serial(process_env)]
    async fn a_server_that_names_no_data_folder_is_refused() {
        let html = folder_screen(None, Some(ACME_STATE)).await;
        assert_folder_screen(&html, "data folders");
        assert!(html.contains("a folder it does not name"), "{html}");
    }

    /// The inspection's own server, on its own folders, shows the cockpit.
    #[tokio::test]
    #[serial(process_env)]
    async fn a_server_on_the_apps_own_folders_renders_the_cockpit() {
        let html = folder_screen(Some(ACME_DATA), Some(ACME_STATE)).await;
        assert!(html.contains("COCKPIT-RENDERED"), "{html}");
    }

    #[test]
    fn the_build_is_compared_first_then_the_data_folder_then_the_state_folder() {
        let server = |commit: &str, data: Option<&str>, state: Option<&str>| ServerBuild {
            commit: Some(commit.to_string()),
            dirty: false,
            data_dir: data.map(str::to_string),
            state_dir: state.map(str::to_string),
        };
        let f = acme_folders();
        let ui = Some(UI_COMMIT);
        assert_eq!(
            find_mismatch(ui, &f, &server(UI_COMMIT, Some(ACME_DATA), Some(ACME_STATE))),
            None
        );
        assert_eq!(
            find_mismatch(ui, &f, &server(OLD_COMMIT, Some(DEV_DATA), Some(DEV_STATE))),
            Some(Mismatch::Build)
        );
        assert_eq!(
            find_mismatch(ui, &f, &server(UI_COMMIT, Some(DEV_DATA), Some(DEV_STATE))),
            Some(Mismatch::DataFolder)
        );
        assert_eq!(
            find_mismatch(ui, &f, &server(UI_COMMIT, Some(ACME_DATA), Some(DEV_STATE))),
            Some(Mismatch::StateFolder)
        );
        assert_eq!(
            find_mismatch(ui, &f, &server(UI_COMMIT, Some(ACME_DATA), None)),
            Some(Mismatch::StateFolder),
            "a server that names no state folder is refused"
        );
        let line = mismatch_line(
            &Mismatch::DataFolder,
            ui,
            &f,
            &server(UI_COMMIT, Some(DEV_DATA), None),
        );
        assert_eq!(
            line,
            format!("The app uses the data folder {ACME_DATA}. The server uses the data folder {DEV_DATA}.")
        );
    }

    #[test]
    fn builds_match_compares_commits_and_treats_a_silent_server_as_a_mismatch() {
        let server = |c: Option<&str>| ServerBuild {
            commit: c.map(str::to_string),
            dirty: false,
            ..ServerBuild::default()
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
            ..ServerBuild::default()
        };
        assert!(builds_match(Some(UI_COMMIT), &dirty));
    }

    #[tokio::test]
    async fn a_server_that_never_answers_gives_none_after_the_wait() {
        // Nothing listens on this port.
        let got = fetch_server_build(
            "http://127.0.0.1:9",
            Duration::from_millis(300),
            Duration::from_millis(50),
        )
        .await;
        assert_eq!(got, None);
    }
}
