//! The scan screen's offer to finish a saved inspection.
//!
//! A cockpit scan is saved as a run as it goes. When the app is closed or crashes in the middle of
//! one (or a part failed and the app was closed), the run stays on disk. On app start and on
//! opening a project this panel lists the project's saved inspections (`GET /api/inspections`) and
//! offers the newest unfinished one: "This inspection was interrupted" (its process is gone) or
//! "This inspection did not finish" (it ended with a part left), with "Run what remains". The plan
//! and estimate preview, the resume request and the Bombe are the ones the incomplete-scan banner
//! uses ([`run_what_remains`]); this panel only adds the way in from a screen that has no report.
//! Zach's rule: any interruption should cost minutes, never the run.

use super::scan::{audit_job_poll, ResumePlanView};
use camerata_ui_core::saved_inspections::{SavedInspection, SavedInspections};
use dioxus::prelude::*;

/// The saved inspections of the open project, newest first. An empty list when the server cannot
/// be reached or answers anything else: the panel then shows nothing rather than a false alarm.
pub(super) async fn fetch_saved_inspections() -> Vec<SavedInspection> {
    let Ok(resp) = reqwest::get(format!("{}/api/inspections", crate::bff_base())).await else {
        return Vec::new();
    };
    if !resp.status().is_success() {
        return Vec::new();
    }
    resp.json::<SavedInspections>()
        .await
        .map(|body| body.inspections)
        .unwrap_or_default()
}

/// Wait until the audit job `job_id` is no longer running (done, failed or cancelled), or until it
/// has vanished (the server restarted). `status` asks the server for the job's status, `None` when
/// the job is unknown; `pause` waits between asks. The Bombe is held by the caller.
pub(super) async fn wait_until_job_ends<S, SF, P, PF>(mut status: S, mut pause: P)
where
    S: FnMut() -> SF,
    SF: std::future::Future<Output = Option<String>>,
    P: FnMut() -> PF,
    PF: std::future::Future<Output = ()>,
{
    let mut misses = 0u32;
    loop {
        pause().await;
        match status().await {
            Some(state) => {
                misses = 0;
                if state != "running" {
                    return;
                }
            }
            None => {
                misses += 1;
                if misses >= 3 {
                    return;
                }
            }
        }
    }
}

/// [`wait_until_job_ends`] against the server's job, polled every second and a half.
pub(super) async fn wait_for_audit_job(job_id: &str) {
    wait_until_job_ends(
        || async { audit_job_poll(job_id).await.map(|js| js.job.status) },
        || tokio::time::sleep(std::time::Duration::from_millis(1500)),
    )
    .await;
}

/// "Start again on this build" for the saved inspection `run_id` (one another build started): the
/// server starts it again from scratch on this build, as a new run, and answers with the job id;
/// or the plain reason it cannot.
pub(super) async fn start_again_on_this_build(run_id: &str) -> Result<String, String> {
    let resp = reqwest::Client::new()
        .post(format!(
            "{}/api/inspections/{run_id}/restart",
            crate::bff_base()
        ))
        .send()
        .await
        .map_err(|e| format!("could not reach the app's server: {e}"))?;
    let ok = resp.status().is_success();
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    if ok {
        body["job_id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "the server did not return a job".to_string())
    } else {
        Err(body["error"]
            .as_str()
            .unwrap_or("the inspection could not be started again")
            .to_string())
    }
}

/// What a row offers in place of a plain "Run what remains" when another build started it: the
/// refusal in plain words and, for a cockpit inspection, "Start again on this build".
fn other_build_block(
    row: &SavedInspection,
    busy: bool,
    on_restart: Option<EventHandler<String>>,
) -> Element {
    let Some(refusal) = row.other_build_refusal() else {
        return rsx! {};
    };
    rsx! {
        p { class: "onboard-gate-b scan-resume-refusal saved-inspection-other-build", "{refusal}" }
        if row.can_start_again_here() {
            button {
                class: "scan-start-again",
                r#type: "button",
                disabled: busy,
                title: "Start this inspection again from the beginning on this build. Nothing from the earlier run is reused.",
                onclick: {
                    let id = row.id.clone();
                    move |_| {
                        if let Some(handler) = on_restart {
                            handler.call(id.clone());
                        }
                    }
                },
                "Start again on this build"
            }
        }
    }
}

/// The offer: the newest unfinished inspection with "Run what remains", and the others listed.
/// Renders nothing when there is nothing to offer or list.
#[component]
pub(super) fn SavedInspectionsPanel(
    saved: Vec<SavedInspection>,
    // The inspection whose plan is shown, and the plan (fetched when "Run what remains" is asked).
    #[props(default)] plan: Option<(String, ResumePlanView)>,
    // True while a resume runs.
    #[props(default)] busy: bool,
    // Why the last resume could not start, in plain words.
    #[props(default)] error: Option<String>,
    #[props(default)] on_plan: Option<EventHandler<String>>,
    #[props(default)] on_resume: Option<EventHandler<String>>,
    // "Start again on this build", for an inspection another build started.
    #[props(default)] on_restart: Option<EventHandler<String>>,
) -> Element {
    let offer = camerata_ui_core::saved_inspections::offer(saved);
    if offer.offered.is_none() && offer.others.is_empty() {
        return rsx! {};
    }
    // The plan block of the inspection `id`, shown under its "Run what remains".
    let plan_block = |id: &str| -> Element {
        match &plan {
            Some((plan_id, plan)) if plan_id == id => rsx! {
                div { class: "scan-resume-plan",
                    pre { class: "scan-resume-plan-text", "{plan.text}" }
                    if let Some(why) = plan.refusal.clone() {
                        p { class: "onboard-gate-b scan-resume-refusal", "Cannot resume now: {why}" }
                    } else {
                        button {
                            class: "scan-resume-confirm",
                            r#type: "button",
                            disabled: busy,
                            onclick: {
                                let id = id.to_string();
                                move |_| {
                                    if let Some(handler) = on_resume {
                                        handler.call(id.clone());
                                    }
                                }
                            },
                            "Start: run only these parts"
                        }
                    }
                }
            },
            _ => rsx! {},
        }
    };
    rsx! {
        div { class: "saved-inspections",
            if let Some(offered) = offer.offered.clone() {
                div { class: "onboard-gate saved-inspection-offer",
                    span { class: "onboard-gate-dot" }
                    div {
                        p { class: "onboard-gate-h", "{offered.headline()}" }
                        p { class: "onboard-gate-b", "{offered.summary()}" }
                        if let Some(line) = offered.started_on_line() {
                            p { class: "onboard-gate-hint saved-inspection-build", "{line}" }
                        }
                        if !offered.reasons.is_empty() {
                            ul { class: "saved-inspection-reasons",
                                for (i , why) in offered.reasons.iter().enumerate() {
                                    li { key: "{i}", "{why}" }
                                }
                            }
                        }
                        if offered.other_build {
                            {other_build_block(&offered, busy, on_restart)}
                        } else {
                            button {
                                class: "scan-run-what-remains",
                                r#type: "button",
                                disabled: busy,
                                title: "Show what would run again and what it should cost. Nothing is sent until you confirm.",
                                onclick: {
                                    let id = offered.id.clone();
                                    move |_| {
                                        if let Some(handler) = on_plan {
                                            handler.call(id.clone());
                                        }
                                    }
                                },
                                "Run what remains"
                            }
                            {plan_block(&offered.id)}
                        }
                        if let Some(why) = error.clone() {
                            p { class: "onboard-gate-b scan-resume-refusal", "{why}" }
                        }
                    }
                }
            }
            if !offer.others.is_empty() {
                div { class: "saved-inspection-others",
                    p { class: "scan-section-sub", "Other saved inspections of this project" }
                    ul { class: "saved-inspection-list",
                        for other in offer.others.iter() {
                            li { key: "{other.id}",
                                span { "{other.list_line()}" }
                                if let Some(line) = other.started_on_line() {
                                    p { class: "onboard-gate-hint saved-inspection-build", "{line}" }
                                }
                                {other_build_block(other, busy, on_restart)}
                                if other.resumable_here {
                                    button {
                                        class: "scan-run-what-remains saved-inspection-other-button",
                                        r#type: "button",
                                        disabled: busy,
                                        onclick: {
                                            let id = other.id.clone();
                                            move |_| {
                                                if let Some(handler) = on_plan {
                                                    handler.call(id.clone());
                                                }
                                            }
                                        },
                                        "Run what remains"
                                    }
                                }
                                if let Some(where_) = other.resume_elsewhere() {
                                    p { class: "onboard-gate-hint", "{where_}" }
                                }
                                {plan_block(&other.id)}
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_ui_core::saved_inspections::SavedStatus;

    fn row(id: &str, started: &str, status: SavedStatus) -> SavedInspection {
        SavedInspection {
            id: id.to_string(),
            started: started.to_string(),
            repos: vec!["acme/one".to_string()],
            status,
            origin: "cockpit".to_string(),
            remaining_parts: 4,
            estimate_usd: 0.25,
            resumable_here: matches!(status, SavedStatus::Interrupted | SavedStatus::Incomplete),
            reasons: Vec::new(),
            started_on_build: Some("aaaaaaaaaaaa".to_string()),
            built_from: None,
            other_build: false,
            build_refusal: None,
        }
    }

    const KEPT_FOLDER: &str = "/Users/auditor/camerata-scan-acme";
    const OTHER_BUILD_REFUSAL: &str = "This inspection was started on build 1a2b3c4d5e6f and \
        cannot be continued on this build (aaaaaaaaaaaa). The build that starts an inspection \
        must finish it. Open build 1a2b3c4d5e6f from the folder \
        /Users/auditor/camerata-scan-acme to run what remains, or start the inspection again on \
        this build.";

    /// A cockpit inspection started on another build, as `GET /api/inspections` lists it.
    fn other_build_json() -> serde_json::Value {
        serde_json::json!({
            "id": "r1", "started": "2026-10-29T10:00:00Z", "repos": ["acme/one"],
            "status": "interrupted", "origin": "cockpit", "remaining_parts": 6,
            "estimate_usd": 0.4, "resumable_here": false, "reasons": [],
            "started_on_build": "1a2b3c4d5e6f", "built_from": KEPT_FOLDER,
            "other_build": true, "build_refusal": OTHER_BUILD_REFUSAL
        })
    }

    fn other_build_row() -> SavedInspection {
        serde_json::from_value(other_build_json()).unwrap()
    }

    fn render(saved: Vec<SavedInspection>, plan: Option<(String, ResumePlanView)>) -> String {
        #[derive(Clone, PartialEq, Props)]
        struct Harness {
            saved: Vec<SavedInspection>,
            plan: Option<(String, ResumePlanView)>,
        }
        fn app(h: Harness) -> Element {
            rsx! {
                SavedInspectionsPanel { saved: h.saved, plan: h.plan }
            }
        }
        let mut vdom = VirtualDom::new_with_props(app, Harness { saved, plan });
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    }

    /// Key test (a), the scan-screen half: a killed cockpit scan shows as interrupted with the
    /// button.
    #[test]
    fn an_interrupted_inspection_is_offered_with_run_what_remains() {
        let html = render(
            vec![row("r1", "2026-10-29T10:00:00Z", SavedStatus::Interrupted)],
            None,
        );
        assert!(html.contains("This inspection was interrupted"), "{html}");
        assert!(html.contains("Run what remains"), "{html}");
        assert!(html.contains("4 parts remain"), "{html}");
        assert!(html.contains("$0.25"), "{html}");
        assert!(!html.contains("did not finish"), "{html}");
    }

    #[test]
    fn an_incomplete_inspection_says_it_did_not_finish() {
        let html = render(
            vec![row("r1", "2026-10-29T10:00:00Z", SavedStatus::Incomplete)],
            None,
        );
        assert!(html.contains("This inspection did not finish"), "{html}");
        assert!(html.contains("Run what remains"), "{html}");
    }

    #[test]
    fn several_saved_inspections_offer_the_newest_and_list_the_others() {
        let html = render(
            vec![
                row("old", "2026-10-27T10:00:00Z", SavedStatus::Incomplete),
                row("new", "2026-10-29T10:00:00Z", SavedStatus::Interrupted),
                row("done", "2026-10-28T10:00:00Z", SavedStatus::Complete),
            ],
            None,
        );
        assert!(html.contains("This inspection was interrupted"), "{html}");
        assert!(
            html.contains("Other saved inspections of this project"),
            "{html}"
        );
        assert!(
            html.contains("2026-10-27T10:00:00Z on acme/one: did not finish"),
            "{html}"
        );
        assert!(
            !html.contains("2026-10-28T10:00:00Z"),
            "a complete inspection is not listed: {html}"
        );
        // One main button for the offered one, one small one for the listed one.
        assert_eq!(html.matches("Run what remains").count(), 2, "{html}");
    }

    /// Key test (b), the screen half: a running inspection is shown as running and has no button.
    #[test]
    fn a_running_inspection_is_never_offered_for_resuming() {
        let html = render(
            vec![row("live", "2026-10-29T10:00:00Z", SavedStatus::Live)],
            None,
        );
        assert!(html.contains("running now"), "{html}");
        assert!(!html.contains("Run what remains"), "{html}");
        assert!(!html.contains("This inspection was interrupted"), "{html}");
    }

    #[test]
    fn nothing_saved_renders_nothing() {
        assert_eq!(render(Vec::new(), None).trim(), "");
        let html = render(
            vec![row("done", "2026-10-29T10:00:00Z", SavedStatus::Complete)],
            None,
        );
        assert_eq!(html.trim(), "");
    }

    #[test]
    fn the_plan_and_its_estimate_show_under_the_offer_and_a_refusal_has_no_start_button() {
        let saved = vec![row("r1", "2026-10-29T10:00:00Z", SavedStatus::Interrupted)];
        let html = render(
            saved.clone(),
            Some((
                "r1".to_string(),
                ResumePlanView {
                    text: "Resume: what remains\n  - calibration (was running)\n".to_string(),
                    refusal: None,
                    ..ResumePlanView::default()
                },
            )),
        );
        assert!(html.contains("what remains"), "{html}");
        assert!(html.contains("scan-resume-confirm"), "{html}");
        let html = render(
            saved,
            Some((
                "r1".to_string(),
                ResumePlanView {
                    text: "Resume: what remains\n".to_string(),
                    refusal: Some("the repo moved on".to_string()),
                    ..ResumePlanView::default()
                },
            )),
        );
        assert!(
            html.contains("Cannot resume now: the repo moved on"),
            "{html}"
        );
        assert!(!html.contains("scan-resume-confirm"), "{html}");
    }

    #[test]
    fn a_command_line_inspection_is_listed_with_where_to_resume_it() {
        let mut cli = row("cli-1", "2026-10-29T10:00:00Z", SavedStatus::Interrupted);
        cli.origin = "cli".to_string();
        cli.resumable_here = false;
        let html = render(vec![cli], None);
        assert!(html.contains("camerata inspect --resume cli-1"), "{html}");
        assert!(!html.contains("Run what remains"), "{html}");
    }

    #[test]
    fn every_unfinished_row_says_which_build_started_it() {
        let html = render(
            vec![
                row("new", "2026-10-29T10:00:00Z", SavedStatus::Interrupted),
                row("old", "2026-10-27T10:00:00Z", SavedStatus::Incomplete),
            ],
            None,
        );
        assert_eq!(
            html.matches("Started on build aaaaaaaaaaaa").count(),
            2,
            "the offer and the listed row: {html}"
        );
    }

    /// An inspection another build started shows the refusal (naming the kept folder) in place of
    /// "Run what remains", with "Start again on this build".
    #[test]
    fn an_inspection_another_build_started_shows_the_refusal_and_start_again() {
        let html = render(vec![other_build_row()], None);
        assert!(html.contains("Started on build 1a2b3c4d5e6f"), "{html}");
        assert!(html.contains("must finish it"), "{html}");
        assert!(html.contains(KEPT_FOLDER), "{html}");
        assert!(html.contains("Start again on this build"), "{html}");
        assert!(!html.contains("Run what remains"), "{html}");
        assert!(!html.contains("not sent again"), "{html}");
        assert!(!html.contains("camerata inspect --resume"), "{html}");
    }

    thread_local! {
        static PANEL_ROWS: std::cell::RefCell<Vec<SavedInspection>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    /// The real panel, its "Start again on this build" wired to the same request the scan screen
    /// sends ([`start_again_on_this_build`]).
    fn panel_with_start_again() -> Element {
        let saved = PANEL_ROWS.with(|r| r.borrow().clone());
        rsx! {
            SavedInspectionsPanel {
                saved,
                on_plan: EventHandler::new(|id: String| {
                    spawn(async move {
                        let _ = super::super::scan::resume_plan(&id).await;
                    });
                }),
                on_resume: EventHandler::new(|id: String| {
                    spawn(async move {
                        let _ = super::super::scan::resume_start(&id).await;
                    });
                }),
                on_restart: EventHandler::new(|id: String| {
                    spawn(async move {
                        let _ = start_again_on_this_build(&id).await;
                    });
                }),
            }
        }
    }

    /// A real click on "Start again on this build" asks the server to start the inspection again
    /// (`POST /api/inspections/r1/restart`) and asks for no plan and no resume.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn clicking_start_again_on_this_build_starts_the_inspection_again() {
        use dioxus::dioxus_core::{ElementId, Mutation};
        use std::any::Any;
        use std::rc::Rc;
        use std::time::Duration;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/inspections/r1/restart"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"job_id": "j2"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));

        PANEL_ROWS.with(|r| *r.borrow_mut() = vec![other_build_row()]);
        let mut vdom = VirtualDom::new(panel_with_start_again);
        let edits = vdom.rebuild_to_vec();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("must finish it"), "{html}");
        assert!(!html.contains("Run what remains"), "{html}");
        let clicks: Vec<ElementId> = edits
            .edits
            .iter()
            .filter_map(|e| match e {
                Mutation::NewEventListener { name, id } if name == "click" => Some(*id),
                _ => None,
            })
            .collect();
        assert_eq!(clicks.len(), 1, "only Start again is clickable: {html}");
        let data: Box<dyn Any> = Box::new(dioxus::html::SerializedMouseData::default());
        let platform: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(data));
        vdom.runtime().handle_event(
            "click",
            dioxus::dioxus_core::Event::new(platform, true),
            clicks[0],
        );
        let until = tokio::time::Instant::now() + Duration::from_millis(500);
        while tokio::time::Instant::now() < until {
            let _ = tokio::time::timeout(Duration::from_millis(10), vdom.wait_for_work()).await;
            let _ = vdom.render_immediate_to_vec();
        }
        let requests = server.received_requests().await.unwrap_or_default();
        let paths: Vec<String> = requests.iter().map(|r| r.url.path().to_string()).collect();
        assert_eq!(paths, vec!["/api/inspections/r1/restart".to_string()]);
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn the_list_is_fetched_from_the_inspections_endpoint() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/inspections"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "project_id": "p1",
                "inspections": [{
                    "id": "r1", "started": "2026-10-29T10:00:00Z", "repos": ["acme/one"],
                    "status": "interrupted", "origin": "cockpit", "remaining_parts": 2,
                    "estimate_usd": 0.1, "resumable_here": true, "reasons": []
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        let listed = fetch_saved_inspections().await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].status, SavedStatus::Interrupted);
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn an_unreachable_server_lists_nothing() {
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", "http://127.0.0.1:9");
        assert!(fetch_saved_inspections().await.is_empty());
    }

    #[tokio::test]
    async fn waiting_for_a_job_ends_when_it_is_done_failed_or_gone() {
        use std::cell::Cell;
        let asks = Cell::new(0u32);
        wait_until_job_ends(
            || {
                asks.set(asks.get() + 1);
                let n = asks.get();
                async move { Some(if n < 3 { "running" } else { "done" }.to_string()) }
            },
            || async {},
        )
        .await;
        assert_eq!(asks.get(), 3, "kept asking while it ran");

        let asks = Cell::new(0u32);
        wait_until_job_ends(
            || {
                asks.set(asks.get() + 1);
                async { None }
            },
            || async {},
        )
        .await;
        assert_eq!(
            asks.get(),
            3,
            "gave up after three misses (the server restarted)"
        );
    }

    /// The scan screen itself, on open: the interrupted inspection of the open project is offered
    /// with "Run what remains" before anything else, and once a scan has run the offer gives way
    /// to its results.
    mod on_the_scan_screen {
        use super::super::super::scan::{OnboardView, ScanReportView};
        use super::*;
        use std::time::Duration;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn screen_without_a_scan() -> Element {
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            use_context_provider(|| Signal::new(None::<String>));
            use_context_provider(crate::loading::LoadingCount::default);
            use_context_provider(|| Signal::new(crate::cockpit::CockpitView::Onboard));
            use_context_provider(|| Signal::new(None::<ScanReportView>));
            rsx! { OnboardView { connection: None } }
        }

        async fn pump(vdom: &mut VirtualDom, ms: u64) {
            let until = tokio::time::Instant::now() + Duration::from_millis(ms);
            while tokio::time::Instant::now() < until {
                let _ = tokio::time::timeout(Duration::from_millis(10), vdom.wait_for_work()).await;
                let _ = vdom.render_immediate_to_vec();
            }
        }

        #[tokio::test(flavor = "current_thread")]
        #[serial_test::serial(process_env)]
        async fn an_interrupted_inspection_is_offered_when_the_screen_opens() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/inspections"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "project_id": "p1",
                    "inspections": [{
                        "id": "r1", "started": "2026-10-29T10:00:00Z", "repos": ["acme/one"],
                        "status": "interrupted", "origin": "cockpit", "remaining_parts": 6,
                        "estimate_usd": 0.4, "resumable_here": true, "reasons": []
                    }]
                })))
                .mount(&server)
                .await;
            let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
            let mut vdom = VirtualDom::new(screen_without_a_scan);
            vdom.rebuild_in_place();
            pump(&mut vdom, 700).await;
            let html = dioxus_ssr::render(&vdom);
            assert!(html.contains("This inspection was interrupted"), "{html}");
            assert!(html.contains("Run what remains"), "{html}");
            assert!(html.contains("6 parts remain"), "{html}");
        }

        /// The scan screen itself, for an inspection another build started: the refusal and
        /// "Start again on this build", never "Run what remains".
        #[tokio::test(flavor = "current_thread")]
        #[serial_test::serial(process_env)]
        async fn an_inspection_another_build_started_is_offered_to_start_again_on_the_screen() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/inspections"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "project_id": "p1",
                    "inspections": [other_build_json()]
                })))
                .mount(&server)
                .await;
            let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
            let mut vdom = VirtualDom::new(screen_without_a_scan);
            vdom.rebuild_in_place();
            pump(&mut vdom, 700).await;
            let html = dioxus_ssr::render(&vdom);
            assert!(html.contains("This inspection was interrupted"), "{html}");
            assert!(html.contains("Started on build 1a2b3c4d5e6f"), "{html}");
            assert!(html.contains("must finish it"), "{html}");
            assert!(html.contains("Start again on this build"), "{html}");
            assert!(!html.contains("Run what remains"), "{html}");
        }
    }
}
