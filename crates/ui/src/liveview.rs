//! The liveview build of the review lanes (review UI spec section 13), behind the
//! `liveview-review` feature.
//!
//! `camerata-ui liveview --project <id>` serves the review shell and its lanes to a browser on
//! 127.0.0.1, using Dioxus's official liveview integration with axum (`dioxus-liveview`, which
//! is on 0.7.9 beside the rest of the Dioxus family). The components run in this process and a
//! thin script in the page sends events over a websocket and applies the edits that come back,
//! so every click in a browser runs the same host closures the desktop app runs.
//!
//! What it mounts, and nothing else: [`ReviewShellHost`] (the lane switcher, the progress strip
//! and the Tree, Escalations, Engine Check and Settings lanes), the export panel (so the
//! release gate's blocked state, its doors and its Export button are there), and the three
//! overlays the lanes open (the code viewer, the stacked view and the command palette). The
//! cockpit's other screens are not linked into this tree. It reads and writes through
//! `HttpReviewSource` and `ReviewWrites` against `CAMERATA_BFF_URL` (or the default BFF address),
//! the same data path the desktop app uses; there is no second one.
//!
//! The one thing that differs from the desktop: native dialogs and the clipboard. A browser
//! driven by a test has no dialog to click, so this build installs a
//! [`RecordingPlatform`](camerata_ui::platform::RecordingPlatform) and serves two routes beside
//! the page for the test to script its answers and read back what the app asked for:
//!
//! - `POST /__platform/script` with a [`Script`] (JSON): answers for the next dialogs and copies;
//! - `GET /__platform/calls`: every call so far, oldest first.
//!
//! Only these routes exist: `/` (the page), `/ws` (the live connection), the two above and
//! `/__health`. Anything else is a 404.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use camerata_ui::platform::{Call, RecordingPlatform, Script};
use camerata_ui_core::tree::export::Door;
use camerata_ui_core::viewer::ViewerCache;
use dioxus::prelude::*;
use dioxus_liveview::{axum_socket, interpreter_glue, LiveViewPool};

use super::palette::{PaletteRoot, PaletteSlot};
use super::review_shell::ReviewShellHost;
use super::scan::{AuditReportExportPanel, RuleHubRequest};
use super::stack::{StackRoot, StackSlot};
use super::tree::{DoorRequest, ShellRefresh};
use super::viewer::{ViewerHubOpen, ViewerRoot, ViewerSlot};
use crate::toast::{Toast, ToastHost};

/// The path of the live connection.
const WS_PATH: &str = "/ws";

/// What `camerata-ui liveview` takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Options {
    /// The project the lanes are about.
    pub project: String,
    /// The port to bind on 127.0.0.1; 0 asks the system for a free one.
    pub port: u16,
}

/// Read `--project <id>` and `--port <n>`.
pub(crate) fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut project = None;
    let mut port = 0u16;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--project" => {
                project = Some(it.next().ok_or("--project needs a project id")?.clone());
            }
            "--port" => {
                port = it
                    .next()
                    .ok_or("--port needs a number")?
                    .parse()
                    .map_err(|e| format!("--port: {e}"))?;
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Options {
        project: project.ok_or("--project <id> is required")?,
        port,
    })
}

/// `camerata-ui liveview`: serve until killed. Prints `liveview listening on http://ADDR` once
/// the port is bound, so a test that asked for port 0 learns the real one. Returns an exit code.
pub(crate) fn run_cli(args: &[String]) -> i32 {
    let options = match parse_args(args) {
        Ok(o) => o,
        Err(message) => {
            eprintln!("camerata-ui liveview: {message}");
            eprintln!("usage: camerata-ui liveview --project <id> [--port <n>]");
            return 2;
        }
    };
    let platform = Arc::new(RecordingPlatform::new());
    if camerata_ui::platform::install(platform.clone()).is_err() {
        eprintln!("camerata-ui liveview: a platform was already installed");
        return 1;
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("camerata-ui liveview: no runtime: {e}");
            return 1;
        }
    };
    runtime.block_on(async move {
        let addr = SocketAddr::from(([127, 0, 0, 1], options.port));
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("camerata-ui liveview: cannot bind {addr}: {e}");
                return 1;
            }
        };
        let bound = listener.local_addr().map_or(addr, |a| a);
        println!("liveview listening on http://{bound}");
        let app = router(options.project, platform);
        match axum::serve(listener, app).await {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("camerata-ui liveview: {e}");
                1
            }
        }
    })
}

#[derive(Clone)]
struct Served {
    project: String,
    pool: LiveViewPool,
    platform: Arc<RecordingPlatform>,
}

/// The whole route table. Nothing else is served.
pub(crate) fn router(project: String, platform: Arc<RecordingPlatform>) -> Router {
    let served = Served {
        project,
        pool: LiveViewPool::new(),
        platform,
    };
    Router::new()
        .route("/", get(page))
        .route(WS_PATH, get(live))
        .route("/__health", get(|| async { "ok" }))
        .route("/__platform/script", post(script))
        .route("/__platform/calls", get(calls))
        .fallback(|| async { StatusCode::NOT_FOUND })
        .with_state(served)
}

/// The page: an empty mount point and the script that connects to `/ws`.
async fn page() -> Html<String> {
    Html(format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>Camerata review (liveview)</title></head>\
         <body><div id=\"main\"></div>{}</body></html>",
        interpreter_glue(WS_PATH)
    ))
}

async fn live(ws: WebSocketUpgrade, State(served): State<Served>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| async move {
        let _ = served
            .pool
            .launch_with_props(axum_socket(socket), root, served.project.clone())
            .await;
    })
}

async fn script(State(served): State<Served>, Json(script): Json<Script>) -> StatusCode {
    served.platform.script(script);
    StatusCode::NO_CONTENT
}

async fn calls(State(served): State<Served>) -> Json<Vec<Call>> {
    Json(served.platform.calls())
}

/// The root the pool launches for each connection.
fn root(project_id: String) -> Element {
    rsx! { LiveviewApp { project_id } }
}

/// The review lanes and what they share, and nothing from the rest of the cockpit.
#[component]
fn LiveviewApp(project_id: String) -> Element {
    crate::loading::provide_loading_context();
    // The toast stack the lanes push into ("Budget is now ...", "Copied the permalink").
    let toasts = use_signal(Vec::<Toast>::new);
    use_context_provider(|| toasts);
    // What the results screen provides to its lanes: the viewer, the stack and the palette (each
    // an overlay the lanes register a link with), and the two signals the export panel and the
    // lanes talk through (a door to a held row, and the counter that re-reads the shell).
    use_context_provider(|| Signal::new(ViewerCache::new()));
    use_context_provider(|| ViewerSlot(Signal::new(None)));
    use_context_provider(|| StackSlot(Signal::new(None)));
    let palette = use_context_provider(|| PaletteSlot(Signal::new(None)));
    // The desktop results view listens for Ctrl or Cmd K at the document (its review bridge) and
    // hands the chord to the palette's link; the lanes' shell registers that link here too.
    use_future(move || async move {
        loop {
            let mut listener = dioxus::document::eval(super::review::REVIEW_BRIDGE_JS);
            while let Ok(msg) = listener.recv::<serde_json::Value>().await {
                if let Some((key, mods, modal)) = super::review::parse_chord_message(&msg) {
                    super::palette::route_chord(Some(palette), &key, mods, modal);
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
    });
    let hub_open = use_signal(|| Option::<RuleHubRequest>::None);
    use_context_provider(|| ViewerHubOpen(hub_open));
    let door = use_signal(|| Option::<Door>::None);
    use_context_provider(|| DoorRequest(door));
    let refresh = use_signal(|| 0u32);
    use_context_provider(|| ShellRefresh(refresh));
    // The export panel posts the stored decisions, not a screen's draft: it starts with none.
    let dispositions = use_signal(HashMap::new);

    rsx! {
        style { dangerous_inner_html: crate::style::GLOBAL_CSS }
        div { class: "app-root", "data-liveview": "review",
            ReviewShellHost { project_id: project_id.clone(),
                div { class: "pane-empty",
                    "The flat view is the Queue of the desktop app. This build serves the review lanes only."
                }
            }
            AuditReportExportPanel { project_id: project_id.clone(), dispositions }
        }
        StackRoot {}
        PaletteRoot {}
        ViewerRoot {}
        ToastHost {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn the_arguments_name_a_project_and_optionally_a_port() {
        assert_eq!(
            parse_args(&args(&["--project", "proj-1", "--port", "8123"])),
            Ok(Options {
                project: "proj-1".into(),
                port: 8123
            })
        );
        assert_eq!(parse_args(&args(&["--project", "p"])).unwrap().port, 0);
        assert!(parse_args(&args(&[])).is_err());
        assert!(parse_args(&args(&["--project", "p", "--bogus"])).is_err());
        assert!(parse_args(&args(&["--project", "p", "--port", "x"])).is_err());
    }
}
