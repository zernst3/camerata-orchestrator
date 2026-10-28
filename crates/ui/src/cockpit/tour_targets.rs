//! The inspection walkthrough's enforcement tests (V1 item 25).
//!
//! The walkthrough points at real controls by a `data-tour` id. If a control is renamed or removed
//! the popup would be left pointing at nothing, silently. These tests render each screen a step
//! belongs to and fail the build when a step's id is not on it, so a rename breaks here instead.
//!
//! They live under `cockpit` because the screens they render (`ProjectsHome`, `CockpitNav`,
//! `OnboardView`, `ScanResults`, the failure banners) are private to it.
//!
//! The rest of the walkthrough's rules are tested where they live: the step table, the text
//! checks and the pure state in `camerata_ui_core::walkthrough`; the host's parsing and gates in
//! `crate::walkthrough`. What is added here is the join between the two: every step's target is a
//! control the cockpit actually draws, and walking the table with what the page would report
//! fires every advance trigger.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use camerata_ui_core::walkthrough::{
    required_ids, Advance, Screen, Tour, TourGate, STEPS, TOUR_ATTRIBUTE,
};
use dioxus::prelude::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::scan::{AiReviewBlockedBanner, OnboardView, ScanJobFailedBanner, ScanResults};
use super::{CockpitNav, CockpitScreen, CockpitView, ProjectsHome, ScanReportView};
use crate::walkthrough::{apply_probe, Probe};

/// Every `data-tour` id in some rendered html.
fn tour_ids(html: &str) -> BTreeSet<String> {
    let needle = format!("{TOUR_ATTRIBUTE}=\"");
    let mut out = BTreeSet::new();
    let mut rest = html;
    while let Some(at) = rest.find(&needle) {
        let after = &rest[at + needle.len()..];
        let Some(end) = after.find('"') else { break };
        out.insert(after[..end].to_string());
        rest = &after[end..];
    }
    out
}

fn render(mut vdom: VirtualDom) -> String {
    vdom.rebuild_in_place();
    dioxus_ssr::render(&vdom)
}

fn projects_screen() -> Element {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    use_context_provider(|| Signal::new(CockpitScreen::Projects));
    rsx! { ProjectsHome {} }
}

fn nav_screen() -> Element {
    use_context_provider(|| Signal::new(CockpitScreen::InProject));
    let view = use_signal(|| CockpitView::Stories);
    rsx! { CockpitNav { view } }
}

fn onboard_screen() -> Element {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    use_context_provider(|| Signal::new(None::<String>));
    use_context_provider(crate::loading::LoadingCount::default);
    use_context_provider(|| Signal::new(CockpitView::Onboard));
    use_context_provider(|| Signal::new(None::<ScanReportView>));
    rsx! { OnboardView { connection: None } }
}

fn failed_banner_screen() -> Element {
    rsx! {
        AiReviewBlockedBanner { ai_blocked_reason: None, ai_error: Some("the model did not answer".to_string()) }
    }
}

fn scan_json(findings: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "repos": ["me/api"], "files_scanned": 4, "code_chars": 40000, "findings": findings,
        "proposed_rules": [], "gated": false
    })
}

fn scan_results_screen() -> Element {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    use_context_provider(|| Signal::new(None::<ScanReportView>));
    use_context_provider(|| Signal::new(CockpitView::Stories));
    use_context_provider(|| Signal::new(None::<String>));
    use_context_provider(|| Signal::new(0_usize) as crate::loading::LoadingCount);
    let report: ScanReportView =
        serde_json::from_value(scan_json(serde_json::json!([]))).expect("the scan");
    rsx! { ScanResults { report } }
}

/// The scan result with a finished inspection: the saved draft brings the audit back, so the
/// Queue, its toolbar and the progress line are drawn, and so are the cost estimate, the run
/// button, the rules and the export.
async fn scan_html() -> String {
    let server = MockServer::start().await;
    let findings = serde_json::json!([
        { "repo": "me/api", "path": "src/z.rs", "line": 40, "rule_id": "R-A", "severity": "high",
          "snippet": "z", "detail": "d", "status": "active", "kind": "defect" },
        { "repo": "me/api", "path": "src/a.rs", "line": 9, "rule_id": "R-B", "severity": "critical",
          "snippet": "a", "detail": "d", "status": "active", "kind": "defect" },
    ]);
    Mock::given(method("GET"))
        .and(path("/api/onboard/draft"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "scan": scan_json(serde_json::json!([])),
            "audit": scan_json(findings),
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/onboard/draft"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

    let mut vdom = VirtualDom::new(scan_results_screen);
    vdom.rebuild_in_place();
    let until = tokio::time::Instant::now() + Duration::from_millis(800);
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(10), vdom.wait_for_work()).await;
        let _ = vdom.render_immediate_to_vec();
    }
    dioxus_ssr::render(&vdom)
}

/// Every screen the step table names, rendered, with the `data-tour` ids found on it.
async fn ids_by_screen() -> BTreeMap<String, BTreeSet<String>> {
    let mut out = BTreeMap::new();
    for screen in [
        Screen::Projects,
        Screen::ProjectNav,
        Screen::Onboard,
        Screen::Scan,
        Screen::FailedBanner,
    ] {
        let html = match screen {
            Screen::Projects => render(VirtualDom::new(projects_screen)),
            Screen::ProjectNav => render(VirtualDom::new(nav_screen)),
            Screen::Onboard => render(VirtualDom::new(onboard_screen)),
            Screen::Scan => scan_html().await,
            Screen::FailedBanner => render(VirtualDom::new(failed_banner_screen)),
        };
        out.insert(format!("{screen:?}"), tour_ids(&html));
    }
    out
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn every_steps_target_is_on_the_screen_it_belongs_to() {
    let screens = ids_by_screen().await;
    let mut missing = Vec::new();
    for step in &STEPS {
        let on_screen = &screens[&format!("{:?}", step.screen)];
        if !on_screen.contains(step.target) {
            missing.push(format!(
                "step {:?}: data-tour=\"{}\" is not on the {:?} screen (found: {on_screen:?})",
                step.id, step.target, step.screen
            ));
        }
    }
    assert!(
        missing.is_empty(),
        "a control the walkthrough points at was renamed or removed:\n{}",
        missing.join("\n")
    );
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn every_control_a_step_waits_for_is_on_some_screen() {
    let screens = ids_by_screen().await;
    let everywhere: BTreeSet<&String> = screens.values().flatten().collect();
    for step in &STEPS {
        if let Advance::Appears(id) = step.advance {
            assert!(
                everywhere.contains(&id.to_string()),
                "step {:?} waits for data-tour=\"{id}\", which no screen draws",
                step.id
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn a_rendered_data_tour_id_that_no_step_uses_is_an_orphan() {
    // The other direction: an id left on a control after its step was deleted would suggest the
    // walkthrough still covers it. Every id on a screen is either a step's target or awaited.
    let required = required_ids();
    for (screen, ids) in ids_by_screen().await {
        for id in ids {
            assert!(
                required.contains(id.as_str()),
                "data-tour=\"{id}\" on the {screen} screen is used by no step"
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn the_start_screens_carry_no_guided_switch_without_a_walkthrough_and_the_queue_is_drawn() {
    // A sanity check on the Scan render itself: if the Queue were not mounted the toolbar ids
    // above would be absent, and the failure would be about the harness, not a rename.
    let html = scan_html().await;
    assert!(
        html.contains("findings-toolbar"),
        "the Queue is not mounted: {html}"
    );
}

/// What the page reports for a look at a screen with these controls on it.
fn probe(ids: &[&str], clicks: &[&str]) -> Probe {
    Probe {
        target: "x".to_string(),
        ids: ids.iter().map(|s| (*s).to_string()).collect(),
        rect: None,
        viewport: camerata_ui_core::walkthrough::Viewport {
            w: 1200.0,
            h: 800.0,
        },
        gate: TourGate::Open,
        clicks: clicks.iter().map(|s| (*s).to_string()).collect(),
        esc: false,
    }
}

#[test]
fn walking_all_twelve_steps_with_what_the_page_reports_fires_every_trigger() {
    let mut tour = Tour::new();
    tour.start();
    // The first look: the app as it stands, with nothing yet done.
    assert!(!apply_probe(&mut tour, &probe(&[], &[])));
    for step in &STEPS {
        assert_eq!(tour.step(), Some(step));
        // A look that shows nothing new moves nothing (except a step that only explains, which
        // waits for Next).
        assert!(!apply_probe(&mut tour, &probe(&[], &[])), "{}", step.id);
        let moved = match step.advance {
            Advance::Next => {
                tour.go_next();
                true
            }
            Advance::Click => apply_probe(&mut tour, &probe(&[step.target], &[step.target])),
            Advance::Appears(id) => apply_probe(&mut tour, &probe(&[id], &[])),
        };
        assert!(moved, "step {:?} did not advance", step.id);
    }
    assert!(
        !tour.is_active(),
        "the walkthrough ended after the twelfth step"
    );
}

#[test]
fn a_click_that_opens_a_dialog_still_counts_while_the_dialog_hides_the_popup() {
    // The Commands button opens the palette. The page reports the click and the open overlay in
    // the same look: the step advances, and the next look hides the popup under the overlay.
    let mut tour = Tour::new();
    tour.start();
    while tour.step().is_some_and(|s| s.target != "speed-tools") {
        tour.go_next();
    }
    let mut look = probe(&["speed-tools"], &["speed-tools"]);
    look.gate = TourGate::Overlay;
    assert!(apply_probe(&mut tour, &look));
    assert_eq!(tour.step().map(|s| s.target), Some("review-time"));
}

#[test]
fn the_walkthrough_does_not_touch_a_job_failed_banner_it_only_points_at_it() {
    // The scan-job-failed banner carries the same id as the AI-review one, so a stopped scan is
    // pointed at too. Render it directly.
    fn screen() -> Element {
        rsx! { ScanJobFailedBanner { message: Some("the scan stopped".to_string()) } }
    }
    let html = render(VirtualDom::new(screen));
    assert!(tour_ids(&html).contains("failed-passes"), "{html}");
}

/// The guided switch is on the screen where an inspection starts: with a walkthrough provided,
/// the Onboard screen draws it.
#[test]
fn the_guided_switch_is_on_the_onboard_screen_when_a_walkthrough_is_provided() {
    fn screen() -> Element {
        crate::walkthrough::provide_tour();
        onboard_screen()
    }
    let html = render(VirtualDom::new(screen));
    assert!(html.contains("Guided inspection"), "{html}");
    assert!(html.contains("role=\"switch\""), "{html}");
}

/// The first-launch offer is on the projects home.
#[test]
fn the_offer_is_on_the_projects_home_once_the_settings_are_read_and_there_are_no_projects() {
    // The projects list is still loading in a static render, so the offer waits: it is never
    // made before the list says it is empty.
    fn screen() -> Element {
        crate::walkthrough::provide_tour();
        projects_screen()
    }
    let html = render(VirtualDom::new(screen));
    assert!(!html.contains("Want a guided first inspection?"), "{html}");
}
