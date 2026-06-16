//! Camerata — consumer-mode prototype (Dioxus DESKTOP).
//!
//! A runnable, mocked walkthrough of the full consumer journey described in
//! `docs/CONSUMER_UX.md`: Intake -> Clarify -> Build -> QA -> (Report a problem)
//! -> Publish/Live. No engine wiring yet; the goal of this pass is the look, the
//! motion, and the flow. The design bar is best-in-class consumer: generous
//! whitespace, a restrained palette (near-black text on near-white, one warm
//! accent), a clean system-font stack, large calm type, slow and subtle motion,
//! rounded surfaces, one clear primary action per screen.
//!
//! Run it with:
//!     cargo run -p camerata-ui
//! (or `dx serve` from crates/ui if you have the Dioxus CLI and prefer hot-reload).

mod agent_activity;
mod app_state;
mod build_run;
mod chat;
mod cockpit;
mod data;
mod deploy_run;
mod maintenance_run;
mod routines;
mod screens;
mod style;
mod toast;
mod workspace;

use std::sync::Arc;

use dioxus::prelude::*;

use app_state::AppState;
use camerata_intake::InMemoryDesignCorpus;
use camerata_persistence::SqliteStore;

/// The screens of the consumer journey, plus the simple navigation state. One
/// enum + one signal is the whole router — deliberately minimal, because the flow
/// is mostly linear and the magic is in the transitions, not the addressing.
///
/// The journey is Intake -> Clarify -> Build -> Qa -> Live, with Bug as a side
/// loop off Qa (file a problem, watch it get fixed, land back in Qa). The
/// progress rail collapses Qa + Bug into a single "Try it" stop, since to the
/// user they are one activity: kicking the tires on their draft.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Intake,
    Clarify,
    Build,
    Qa,
    Bug,
    Live,
}

/// Where the embedded BFF binds, and the URL the cockpit fetches from. The desktop
/// shell talks to this local server over HTTP (the same server that runs in the
/// cloud later); the UI never calls the backend crates in-process for cockpit data.
pub const BFF_ADDR: &str = "127.0.0.1:8787";
pub const BFF_URL: &str = "http://127.0.0.1:8787";

fn main() {
    // Auto-load the gitignored .env at the repo root (and any parent), so the
    // GitHub token etc. are available to the embedded BFF without exporting them.
    // Run from the repo dir (`cargo run -p camerata-ui`) so `.env` is found.
    let _ = dotenvy::dotenv();
    dioxus::launch(App);
}

/// The on-disk location of the version-history database, under the per-user data
/// directory (e.g. `~/Library/Application Support/camerata/history.db` on macOS).
///
/// Creates the `camerata` data directory if it does not exist. Returns `None` if
/// the platform data dir can't be resolved or the directory can't be created, in
/// which case the caller falls back to an ephemeral in-memory store.
fn store_path() -> Option<std::path::PathBuf> {
    let dir = dirs::data_dir()?.join("camerata");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("history.db"))
}

/// How the version-history store actually opened, so the UI can be honest with
/// the user when durability is degraded instead of silently dropping history.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PersistenceMode {
    /// On-disk store opened; full version history persists across launches.
    Durable,
    /// The on-disk path was unavailable or failed to open, so we are running on
    /// an in-memory store. Work is saved for this session only and is lost when
    /// the app closes.
    SessionOnly,
    /// No store at all (even the in-memory fallback failed). Edits are not being
    /// persisted; the app still runs so the user is never hard-blocked.
    Unavailable,
}

/// Open the version-history store, preferring the durable on-disk database and
/// falling back so the app always runs. The returned [`PersistenceMode`] lets the
/// UI tell the user the truth when durability is degraded.
async fn open_store() -> (Option<SqliteStore>, PersistenceMode) {
    if let Some(path) = store_path() {
        if let Ok(store) = SqliteStore::open_path(&path).await {
            return (Some(store), PersistenceMode::Durable);
        }
    }
    // On-disk unavailable or failed: fall back to in-memory so the app still
    // runs, but the caller surfaces a quiet banner so history loss is not silent.
    match SqliteStore::open(":memory:").await {
        Ok(store) => (Some(store), PersistenceMode::SessionOnly),
        Err(_) => (None, PersistenceMode::Unavailable),
    }
}

/// The two surfaces this one window can show. Both run on the same engine; they
/// differ in where the human stands (a led wizard vs. a steered control surface).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Edition {
    /// The consumer / small-business app-builder (the guided wizard).
    AppBuilder,
    /// The enterprise / architect cockpit (the dense control surface). Routines live
    /// INSIDE this surface (an architect tool), reached via the cockpit's own nav.
    Cockpit,
}

/// Root. Injects the global stylesheet once and lets the viewer switch between
/// the two surfaces in a single window (so a demo can flip between them).
#[component]
fn App() -> Element {
    // Stand up the BFF once, on its own background Tokio runtime, so the desktop
    // shell talks to the exact same HTTP server that will run in the cloud. If the
    // port is already serving (e.g. a standalone `camerata-server`), this bind fails
    // harmlessly and the cockpit uses the already-running one.
    use_hook(|| {
        std::thread::spawn(|| match tokio::runtime::Runtime::new() {
            Ok(rt) => rt.block_on(async {
                if let Err(e) = camerata_server::serve(BFF_ADDR).await {
                    eprintln!("[camerata-ui] embedded BFF exited: {e}");
                }
            }),
            Err(e) => eprintln!("[camerata-ui] could not start BFF runtime: {e}"),
        });
    });

    let edition = use_signal(|| Edition::AppBuilder);

    // App-wide toast stack, shared via context so any component can push
    // notifications/errors. The ConnectionWatcher below seeds it from the
    // integration health probe.
    let toasts = use_signal(Vec::<toast::Toast>::new);
    use_context_provider(|| toasts);

    rsx! {
        // Global stylesheet, injected as a raw <style> so it works identically on
        // desktop without the asset pipeline. Keeps the whole look in one place.
        style { dangerous_inner_html: style::GLOBAL_CSS }

        div { class: "app-root",
            // Watches connection health and pushes warning/error toasts; renders nothing.
            toast::ConnectionWatcher {}
            // Drains the server-side event-ingest feed (tracker/deploy) into toasts.
            toast::NotificationPoller {}
            EditionSwitcher { edition }
            match edition() {
                Edition::AppBuilder => rsx! { ConsumerApp {} },
                Edition::Cockpit => rsx! { cockpit::CockpitApp {} },
            }
        }
        // The toast stack is a SEPARATE top-layer overlay — a sibling of app-root,
        // position:fixed, pointer-events:none on the layer (so it never blocks the
        // UI behind it) with pointer-events:auto on each toast.
        toast::ToastHost {}
        // The research chat bubble: a floating, always-available AI scratchpad.
        chat::ChatBubble {}
    }
}

/// The segmented control at the very top of the window: flip between the two
/// surfaces. Present in every demo so both editions are reachable at once.
#[component]
fn EditionSwitcher(edition: Signal<Edition>) -> Element {
    let mut edition = edition;
    let app_cls = if edition() == Edition::AppBuilder {
        "edition-tab on"
    } else {
        "edition-tab"
    };
    let cockpit_cls = if edition() == Edition::Cockpit {
        "edition-tab on"
    } else {
        "edition-tab"
    };
    rsx! {
        div { class: "edition-switcher",
            span { class: "edition-brand", "Camerata" }
            div { class: "edition-tabs",
                button {
                    class: "{app_cls}",
                    onclick: move |_| edition.set(Edition::AppBuilder),
                    "App builder"
                }
                button {
                    class: "{cockpit_cls}",
                    onclick: move |_| edition.set(Edition::Cockpit),
                    "Enterprise cockpit"
                }
            }
            span { class: "edition-hint", "one governed engine" }
        }
    }
}

/// The consumer app-builder surface: the guided journey Intake -> Clarify -> Build
/// -> Try it -> Live. Owns the live project, the design corpus, and the durable
/// version-history store, and exposes a Start over control to reset the journey.
#[component]
fn ConsumerApp() -> Element {
    let mut screen = use_signal(|| Screen::Intake);

    // The live consumer project, shared with every screen via context. `None`
    // until the intake screen builds it on submit. Intake writes it; the
    // refinement screen reads and edits it.
    let mut app = use_signal(|| Option::<AppState>::None);
    use_context_provider(|| app);

    // The shared design corpus (the opt-in learning flywheel), app-wide and
    // sharable into async tasks. In-memory for the prototype.
    use_context_provider(|| Arc::new(InMemoryDesignCorpus::new()));

    // Persistence. One SQLite store, opened once and held for the whole session,
    // so versions accumulate. The database lives on disk under the per-user data
    // directory, so the full version history of every project survives across app
    // launches (the user explicitly wanted real-time, version-tracked persistence
    // in a database). If the data dir can't be resolved or opened (rare), we fall
    // back to an ephemeral in-memory store so the app still runs, and surface a
    // quiet banner so the degraded durability is never silent.
    let store = use_resource(open_store);

    // The persistence mode, once the store has resolved, for the honesty banner.
    let persistence_mode = store.read().as_ref().map(|(_, mode)| *mode);

    // Whenever the project has queued revisions (every user/AI edit queues one),
    // drain them and flush to the store. Draining happens synchronously, OUTSIDE
    // the spawned task, so no signal guard is held across an await.
    use_effect(move || {
        let ready = store.read().as_ref().and_then(|(s, _)| s.clone());
        let has_pending = app
            .read()
            .as_ref()
            .map(|s| s.pending_count() > 0)
            .unwrap_or(false);
        if let (Some(store), true) = (ready, has_pending) {
            let pending = app
                .write()
                .as_mut()
                .map(|s| s.take_pending())
                .unwrap_or_default();
            if !pending.is_empty() {
                spawn(async move {
                    let _ = app_state::flush(&store, &pending).await;
                });
            }
        }
    });

    // Start over: clear the live project and return to the first screen. Intake
    // re-seeds fresh on remount, so this is a clean reset of the whole journey.
    let restart = move |_| {
        app.set(None);
        screen.set(Screen::Intake);
    };

    rsx! {
        // If durability is degraded, say so in plain language rather than
        // silently dropping the user's history.
        if let Some(mode) = persistence_mode {
            PersistenceBanner { mode }
        }

        // The progress rail, plus a quiet Start over control on the right so the
        // user can reset the journey from any screen.
        div { class: "rail-row",
            ProgressRail { screen }
            button { class: "btn-restart", onclick: restart, "Start over" }
        }

        div { class: "stage",
            match screen() {
                Screen::Intake => rsx! { screens::intake::IntakeScreen { screen } },
                Screen::Clarify => rsx! { screens::clarify::ClarifyScreen { screen } },
                Screen::Build => rsx! { screens::build::BuildScreen { screen } },
                Screen::Qa => rsx! { screens::qa::QaScreen { screen } },
                Screen::Bug => rsx! { screens::bug::BugScreen { screen } },
                Screen::Live => rsx! { screens::live::LiveScreen { screen } },
            }
        }
    }
}

/// A quiet, plain-language banner shown only when version-history durability is
/// degraded. The default (durable on-disk) path renders nothing. The copy is
/// deliberately calm and free of jargon: it tells the user what is happening to
/// their work, not what failed internally.
#[component]
fn PersistenceBanner(mode: PersistenceMode) -> Element {
    let message = match mode {
        // Durable is the happy path — no banner at all.
        PersistenceMode::Durable => return rsx! {},
        PersistenceMode::SessionOnly => {
            "Heads up: we couldn't open your saved history file, so your work is being \
             kept for this session only. Everything works while the app is open, but \
             changes won't be saved after you close it."
        }
        PersistenceMode::Unavailable => {
            "Heads up: we're unable to save your work right now. You can keep going, but \
             your changes won't be kept after you close the app."
        }
    };
    rsx! {
        div { class: "persist-banner", role: "status",
            span { class: "persist-banner-dot" }
            span { class: "persist-banner-text", "{message}" }
        }
    }
}

/// The journey rail. Calm, slow, never numeric. Five stops; Qa and Bug share the
/// "Try it" stop because to the user they are the same activity (kicking the tires
/// on the draft and reporting anything off).
#[component]
fn ProgressRail(screen: Signal<Screen>) -> Element {
    let steps = [
        (Screen::Intake, "Describe"),
        (Screen::Clarify, "Clarify"),
        (Screen::Build, "Build"),
        (Screen::Qa, "Try it"),
        (Screen::Live, "Live"),
    ];
    let current = screen();
    let order = |s: Screen| match s {
        Screen::Intake => 0,
        Screen::Clarify => 1,
        Screen::Build => 2,
        // Qa and Bug are one stop on the rail.
        Screen::Qa | Screen::Bug => 3,
        Screen::Live => 4,
    };
    let current_order = order(current);

    rsx! {
        nav { class: "rail",
            div { class: "rail-inner",
                for (s , label) in steps {
                    {
                        let o = order(s);
                        let cls = if o < current_order {
                            "rail-step done"
                        } else if o == current_order {
                            "rail-step active"
                        } else {
                            "rail-step"
                        };
                        rsx! {
                            div { class: "{cls}",
                                span { class: "rail-dot" }
                                span { class: "rail-label", "{label}" }
                            }
                        }
                    }
                }
            }
        }
    }
}
