//! THE PLACE VIEWER: the code viewer opened on a bare place (repo, path, line), not on a finding.
//!
//! The convention model lists the places that FOLLOW a rule's standard (design 2026-11-20, section
//! 8), and a conforming place is no finding: it has no row, no key and no cursor. The finding
//! viewer ([`super::viewer::CodeViewerHost`]) is built around a table row, so it cannot open one.
//! This host reuses everything below the row: the same file request, the same props-only
//! [`CodeViewer`], the same line scroll. It has no disposition keys, no history and no popover,
//! because a place carries nothing to decide.

use std::sync::Arc;

use camerata_ui_core::source::short_sha;
use camerata_ui_core::triage::KeyMods;
use camerata_ui_core::viewer::{page_request, scroll_top_for_line, CODE_LINE_H_PX};
use dioxus::prelude::*;

use super::scan::PlaceOpen;
use super::viewer::{fetch_source_file, CodeViewer, FileStateView, ViewerFinding, SCROLL_ID};

/// What the viewer assumes of its viewport until the DOM says otherwise.
const VIEWPORT_PX: f64 = 900.0;

/// The `data-finding-key` of a place: it has no finding key, so the attribute names the place.
pub(super) fn place_key(place: &PlaceOpen) -> String {
    format!("place|{}|{}|{}", place.repo, place.path, place.line)
}

/// The viewer, open on `place`. `sha` is the commit the inspection recorded for the repo (empty
/// when it recorded none).
#[component]
pub(super) fn PlaceViewer(
    place: PlaceOpen,
    sha: String,
    #[props(default)] sha_conflict: bool,
    project_id: String,
    on_close: Callback<()>,
) -> Element {
    let mut around = use_signal(|| Option::<usize>::None);
    let mut scroll_top = use_signal(|| {
        scroll_top_for_line(place.line, CODE_LINE_H_PX, VIEWPORT_PX, usize::MAX / 64)
    });
    let mut viewport = use_signal(|| VIEWPORT_PX);

    let file_res = use_resource({
        let (place, sha, project_id) = (place.clone(), sha.clone(), project_id.clone());
        move || {
            let (place, sha, project_id) = (place.clone(), sha.clone(), project_id.clone());
            let wanted = around().unwrap_or(place.line);
            async move {
                fetch_source_file(
                    &place.repo,
                    &place.path,
                    Some(&sha),
                    Some(wanted),
                    Some(&project_id),
                )
                .await
                .map(Arc::new)
            }
        }
    });
    let file_state = match file_res.read().clone() {
        Some(Some(v)) => FileStateView::Loaded(v),
        Some(None) => FileStateView::Failed,
        None => FileStateView::Loading,
    };
    let total_lines = match &file_state {
        FileStateView::Loaded(v) => v.total(),
        _ => 0,
    };
    let finding = ViewerFinding {
        repo: place.repo.clone(),
        path: place.path.clone(),
        line: place.line,
        sha7: short_sha(&sha).to_string(),
        sha_conflict,
        ..ViewerFinding::default()
    };

    let on_scroll = {
        let file_state = file_state.clone();
        move |(top, client_h): (f64, f64)| {
            scroll_top.set(top);
            if client_h > 50.0 && (client_h - *viewport.peek()).abs() > 1.0 {
                viewport.set(client_h);
            }
            // A file delivered as a window: ask for a new one near an edge.
            let FileStateView::Loaded(v) = &file_state else {
                return;
            };
            let Some(w) = v.window.as_ref() else {
                return;
            };
            if let Some(centre) = page_request(w, top, *viewport.peek(), CODE_LINE_H_PX) {
                if *around.peek() != Some(centre) {
                    around.set(Some(centre));
                }
            }
        }
    };
    // The scroll container just mounted: aim it at the place, refined by the real height.
    let line = place.line;
    let on_scroll_ready = move |()| {
        set_dom_scroll(*scroll_top.peek());
        spawn(async move {
            let probe =
                format!("var e=document.getElementById('{SCROLL_ID}');return e?e.clientHeight:0;");
            let Ok(value) = dioxus::document::eval(&probe).await else {
                return;
            };
            let Some(h) = value.as_f64().filter(|h| *h > 100.0) else {
                return;
            };
            viewport.set(h);
            let top = scroll_top_for_line(line, CODE_LINE_H_PX, h, total_lines);
            scroll_top.set(top);
            set_dom_scroll(top);
        });
    };
    rsx! {
        CodeViewer {
            finding,
            finding_key: place_key(&place),
            file: file_state,
            scroll_top: scroll_top(),
            viewport_px: viewport(),
            on_key: move |(key, _): (String, KeyMods)| {
                if key == "Escape" {
                    on_close.call(());
                }
            },
            on_close,
            on_scroll,
            on_scroll_ready,
        }
    }
}

/// Set the scroll container's `scrollTop`.
fn set_dom_scroll(px: f64) {
    dioxus::document::eval(&format!(
        "var e=document.getElementById('{SCROLL_ID}');if(e)e.scrollTop={px};"
    ));
}
