//! The review gallery's state registry (review spec section 12, design A6).
//!
//! One entry per named state, `screen/state`. A state whose screen is not built yet renders a
//! placeholder page naming the work order that owns it, and `placeholders()` lists them so none is
//! forgotten: the work order that builds a screen replaces its entries' `Placeholder` with `Built`.
//!
//! # Screenshotting every page (agents, U6: 1440 and 1000 wide, no third width)
//!
//! 1. `cargo run -p camerata-ui -- gallery <out dir>` writes `<out>/<screen>/<state>.html`
//!    and `<out>/index.html`.
//! 2. Serve it: `python3 -m http.server 8099 --directory <out dir>` (background), then open
//!    `http://127.0.0.1:8099/index.html` in the in-app browser.
//! 3. For each link, `resize_window` to 1440 x 900, screenshot, `resize_window` to 1000 x 900,
//!    screenshot; reset the window with preset `desktop` when done. Compare against spec sections
//!    2 to 10; the grouping check is done on `tree/markers` and `tree/30k`.

use std::fmt::Write as _;
use std::path::Path;

use dioxus::prelude::*;

use crate::test_harness;

mod detail_states;
mod engine_states;
mod export_states;
mod queue_states;
mod shell_states;
mod tree_states;

/// How a state draws itself.
#[derive(Clone, Copy)]
pub enum StateKind {
    /// The screen does not exist yet; `owner` is the work order that builds it.
    Placeholder { owner: &'static str },
    /// Renders the real screen from fixtures to a full HTML document.
    Built(fn() -> String),
}

/// One named gallery state.
#[derive(Clone, Copy)]
pub struct GalleryState {
    pub screen: &'static str,
    pub state: &'static str,
    pub kind: StateKind,
}

impl GalleryState {
    /// `screen/state`, the name the spec uses.
    pub fn id(&self) -> String {
        format!("{}/{}", self.screen, self.state)
    }

    pub fn is_placeholder(&self) -> bool {
        matches!(self.kind, StateKind::Placeholder { .. })
    }

    /// The full HTML document for this state.
    pub fn render(&self) -> String {
        match self.kind {
            StateKind::Placeholder { owner } => test_harness::render_props(
                NotBuilt,
                NotBuiltProps {
                    screen: self.screen.to_string(),
                    state: self.state.to_string(),
                    owner: owner.to_string(),
                },
            ),
            StateKind::Built(render) => render(),
        }
    }
}

/// The review spec's section 12 states, grouped by screen, with the work order that builds the
/// screen. When a work order builds a screen it replaces its screen's `Placeholder` entries in
/// `registry()` with `Built` renderers (in a `gallery/<screen>_states.rs` file) and deletes the
/// matching lines from `EXPECTED_PLACEHOLDERS` in `gallery/tests.rs`.
const SPEC: &[(&str, &str, &[&str])] = &[
    (
        "shell",
        "WO10",
        &["empty", "loading", "error", "held-count"],
    ),
    (
        "tree",
        "WO10",
        &[
            "empty",
            "loading",
            "error",
            "30k",
            "markers",
            "node-prompt",
            "node-decided",
            "not-group-reviewed",
            "narrow",
        ],
    ),
    (
        "queue",
        "WO11",
        &[
            "empty",
            "loading",
            "error",
            "at-budget",
            "over-budget",
            "line-lowered",
            "rate-changed-toast",
            "30k",
            "detail-form-open",
            "merge-picker-empty-pool",
        ],
    ),
    (
        "engine",
        "WO11",
        &[
            "lane-off",
            "loading",
            "card-pending",
            "no-list-open",
            "no-pending-correction",
            "no-corrected",
            "all-verdicts",
            "small-category",
            "thrown-out-held",
            "thrown-out-confirmed",
            "floor-binds",
        ],
    ),
    (
        "detail",
        "WO11",
        &[
            "empty",
            "plain",
            "escalated",
            "outlier",
            "edited-field",
            "stale-edit",
            "stale-decision",
            "moved",
            "new-since-decision",
            "held",
            "merged",
            "added",
            "ignored-needs-ack",
            "ignored-acked",
            "edit-open",
            "merge-picker",
            "split-picker",
            "add-form",
            "add-refused",
            "possible-duplicate",
            "merge-picker-duplicate-first",
            "over-budget",
            "viewer-commit",
            "thrown-out",
        ],
    ),
    (
        "settings",
        "WO11b",
        &[
            "defaults",
            "rate-changed",
            "invalid",
            "saving",
            "error",
            "run-in-flight",
        ],
    ),
    (
        "export",
        "WO10",
        &["clear", "blocked", "one-line-two-reasons"],
    ),
];

/// A state id and the function that renders its page.
pub(crate) type BuiltRenderer = (&'static str, fn() -> String);

/// The renderer of every state a work order has built, by `screen/state`. A work order that
/// builds a screen adds its `gallery/<screen>_states.rs` here.
fn built_renderers() -> Vec<BuiltRenderer> {
    let mut all = Vec::new();
    all.extend(shell_states::states());
    all.extend(tree_states::states());
    all.extend(queue_states::states());
    all.extend(engine_states::states());
    all.extend(detail_states::states());
    all.extend(export_states::states());
    all.extend(settings_states::states());
    all
}

/// Every named state, in spec order. A state with a renderer is `Built`; the rest are
/// placeholders naming the work order that owns them.
pub fn registry() -> Vec<GalleryState> {
    let built = built_renderers();
    SPEC.iter()
        .flat_map(|(screen, owner, states)| states.iter().map(|state| (*screen, *owner, *state)))
        .map(|(screen, owner, state)| {
            let id = format!("{screen}/{state}");
            let kind = built
                .iter()
                .find(|(built_id, _)| *built_id == id)
                .map_or(StateKind::Placeholder { owner }, |(_, render)| {
                    StateKind::Built(*render)
                });
            GalleryState {
                screen,
                state,
                kind,
            }
        })
        .collect()
}

#[derive(Clone, PartialEq, Props)]
struct NotBuiltProps {
    screen: String,
    state: String,
    owner: String,
}

/// The page a state shows until its screen exists.
#[component]
fn NotBuilt(props: NotBuiltProps) -> Element {
    rsx! {
        div { class: "gallery-not-built", style: "padding: 48px; font-family: var(--font-mono, monospace);",
            p { "not built: {props.screen}/{props.state}, owned by {props.owner}" }
        }
    }
}

/// What `write_gallery` produced.
#[derive(Debug, PartialEq, Eq)]
pub struct GalleryReport {
    pub pages: usize,
    pub placeholders: usize,
}

/// Write `<out>/<screen>/<state>.html` for every state and `<out>/index.html`.
pub fn write_gallery(out: &Path) -> std::io::Result<GalleryReport> {
    let states = registry();
    for s in &states {
        let dir = out.join(s.screen);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(format!("{}.html", s.state)), s.render())?;
    }
    let report = GalleryReport {
        pages: states.len(),
        placeholders: states.iter().filter(|s| s.is_placeholder()).count(),
    };
    std::fs::write(
        out.join("index.html"),
        index_html(&states, report.placeholders),
    )?;
    Ok(report)
}

/// `index.html`: the placeholder count first, then every page linked, grouped by screen.
fn index_html(states: &[GalleryState], placeholders: usize) -> String {
    let mut body = String::new();
    let _ = write!(
        body,
        "<h1>Review gallery</h1>\n<p>{} states, {placeholders} placeholders (screens not built yet).</p>\n\
         <p>Screenshot each page at 1440 and 1000 wide.</p>\n",
        states.len()
    );
    let mut screens: Vec<&str> = Vec::new();
    for s in states {
        if !screens.contains(&s.screen) {
            screens.push(s.screen);
        }
    }
    for screen in screens {
        let _ = writeln!(body, "<h2>{screen}</h2>\n<ul>");
        for s in states.iter().filter(|s| s.screen == screen) {
            let note = if s.is_placeholder() {
                " (placeholder)"
            } else {
                ""
            };
            let _ = writeln!(
                body,
                "<li id=\"{}\"><a href=\"{screen}/{}.html\">{}</a>{note}</li>",
                s.id(),
                s.state,
                s.state
            );
        }
        body.push_str("</ul>\n");
    }
    test_harness::document("Review gallery", &body)
}

/// Which of the app's two jobs a command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Launch {
    /// No subcommand: open the cockpit window, exactly as before.
    App,
    /// `gallery <out dir>`: write the static pages. Carries the arguments after `gallery`.
    Gallery(Vec<String>),
}

/// Route the arguments after the program name. Pure, so it is tested without a window.
pub fn route(args: &[String]) -> Launch {
    match args.first().map(String::as_str) {
        Some("gallery") => Launch::Gallery(args[1..].to_vec()),
        _ => Launch::App,
    }
}

/// The `gallery` subcommand: `gallery <out dir>`. Returns the process exit code.
pub fn run_cli(args: &[String]) -> i32 {
    let Some(out) = args.first() else {
        eprintln!("usage: cargo run -p camerata-ui -- gallery <out dir>");
        return 2;
    };
    match write_gallery(Path::new(out)) {
        Ok(report) => {
            println!(
                "gallery: wrote {} pages ({} placeholders) and index.html to {out}",
                report.pages, report.placeholders
            );
            0
        }
        Err(e) => {
            eprintln!("gallery: could not write to {out}: {e}");
            1
        }
    }
}

pub(crate) mod settings_states;

#[cfg(test)]
mod tests;
