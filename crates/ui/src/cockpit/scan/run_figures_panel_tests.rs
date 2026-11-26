//! The Run figures panel, mounted for real over a stub server: the saved acme report gives the
//! report's figures, the stub serves the run folder's figures, the operator switches views and
//! clicks Copy. Each source carries figures the other does not, so a panel wired to the wrong
//! source shows the wrong number.

use std::cell::RefCell;
use std::time::Duration;

use dioxus::dioxus_core::{AttributeValue, ElementId, Mutation, Mutations};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use camerata_ui_core::test_support::run_figures::{FIGURES_JSON, REPORT_JSON, RUN_ID};

thread_local! {
    static COPIED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static RUN: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn saved_report() -> ScanReportView {
    serde_json::from_str(REPORT_JSON).expect("the saved acme report reads as the screen's report")
}

/// The panel as the scan screen mounts it for the finished acme run, with a clipboard that
/// keeps what it is given.
fn finished_run() -> Element {
    let report = saved_report();
    let run_id = RUN.with(|r| r.borrow().clone());
    let copier = EventHandler::new(|text: String| COPIED.with(|c| c.borrow_mut().push(text)));
    rsx! {
        RunFiguresSection {
            report: report.run_figures_report(),
            run_id,
            copier,
        }
    }
}

#[derive(Default)]
struct Marks(Vec<(String, ElementId)>);

impl Marks {
    fn take(&mut self, m: &Mutations) {
        for e in &m.edits {
            if let Mutation::SetAttribute {
                name: "data-test",
                value: AttributeValue::Text(text),
                id,
                ..
            } = e
            {
                self.0.push((text.clone(), *id));
            }
        }
    }

    fn click(&self, vdom: &VirtualDom, mark: &str) {
        let id = self
            .0
            .iter()
            .rev()
            .find(|(t, _)| t == mark)
            .map(|(_, id)| *id)
            .unwrap_or_else(|| panic!("no element marked {mark}: {:?}", self.0));
        let data: Box<dyn std::any::Any> = Box::new(dioxus::html::SerializedMouseData::default());
        let platform: std::rc::Rc<dyn std::any::Any> =
            std::rc::Rc::new(dioxus::html::PlatformEventData::new(data));
        vdom.runtime()
            .handle_event("click", dioxus::dioxus_core::Event::new(platform, true), id);
    }
}

async fn pump(vdom: &mut VirtualDom, marks: &mut Marks, ms: u64) {
    let until = tokio::time::Instant::now() + Duration::from_millis(ms);
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(10), vdom.wait_for_work()).await;
        marks.take(&vdom.render_immediate_to_vec());
    }
}

/// The words on screen, markup removed and whitespace collapsed.
fn visible(vdom: &VirtualDom) -> String {
    let html = dioxus_ssr::render(vdom);
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The table's rows as cell lists, for the view on screen.
fn rows(vdom: &VirtualDom) -> Vec<Vec<String>> {
    let html = dioxus_ssr::render(vdom);
    let body = html
        .split_once("<tbody")
        .and_then(|(_, b)| b.split_once("</tbody>"))
        .map(|(b, _)| b.to_string())
        .unwrap_or_default();
    body.split("<tr")
        .skip(1)
        .map(|row| {
            row.split("<td")
                .skip(1)
                .map(|cell| {
                    let inner = cell.split_once('>').map_or("", |(_, r)| r);
                    inner.split("</td>").next().unwrap_or("").trim().to_string()
                })
                .collect()
        })
        .collect()
}

async fn serve(figures: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/inspections/{RUN_ID}/figures")))
        .respond_with(figures)
        .mount(&server)
        .await;
    server
}

async fn mount(run_id: Option<&str>) -> (VirtualDom, Marks) {
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
    COPIED.with(|c| c.borrow_mut().clear());
    RUN.with(|r| *r.borrow_mut() = run_id.map(str::to_string));
    let mut vdom = VirtualDom::new(finished_run);
    let mut marks = Marks::default();
    marks.take(&vdom.rebuild_to_vec());
    pump(&mut vdom, &mut marks, 400).await;
    (vdom, marks)
}

fn copied() -> Vec<String> {
    COPIED.with(|c| c.borrow().clone())
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn the_operator_reads_the_figures_of_both_sources_and_copies_them_one_per_line() {
    let server = serve(ResponseTemplate::new(200).set_body_string(FIGURES_JSON)).await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let (mut vdom, mut marks) = mount(Some(RUN_ID)).await;

    // The Summary view: scope and tokens from the report, time from the run folder.
    let summary = rows(&vdom);
    let get = |figure: &str| {
        summary
            .iter()
            .find(|r| r[0] == figure)
            .map(|r| r[1].clone())
            .unwrap_or_else(|| panic!("no {figure} row in {summary:?}"))
    };
    assert_eq!(get("Lines in scope"), "48,213");
    assert_eq!(get("Files in scope"), "326");
    assert_eq!(get("Languages in scope"), "3");
    assert_eq!(get("Coverage"), "95.4% (311 of 326 files read)");
    assert_eq!(get("Wall clock"), "1h 02m 03s");
    assert_eq!(get("Sessions"), "2");
    assert_eq!(get("Tokens in"), "1,507,405");
    assert_eq!(get("Tokens cached"), "754,322");
    assert_eq!(get("Model calls"), "166");

    // Copy: every figure, one per line, starting with the heading.
    marks.click(&vdom, "run-figures-copy");
    pump(&mut vdom, &mut marks, 100).await;
    let copied = copied();
    assert_eq!(copied.len(), 1, "one click, one copy");
    let text = &copied[0];
    assert!(text.starts_with("Run figures (operator only, never part of the client package)\n"));
    for line in [
        "Lines in scope: 48,213",
        "Wall clock: 1h 02m 03s",
        "Sessions: 2",
        "Tokens in: 1,507,405",
        "Tokens out: 150,227",
        "Tokens cached: 754,322",
        "Scan stage, wall clock: 12m 30s",
        "Fix stage, busy time: not recorded",
        "Reviewer role, model calls: 37",
        "Model claude-haiku-5, stages: Calibration and Fix",
        "Scan batches, processing: 1",
        "Calibration batches, submitted: 2",
        "Fix batches, ended: 5",
    ] {
        assert!(
            text.lines().any(|l| l == line),
            "`{line}` not in the copy:\n{text}"
        );
    }
    assert_eq!(text.lines().count(), 63, "{text}");
    assert!(visible(&vdom).contains("Copied the run figures."));

    // Nothing of the client's repository beyond counts is on screen or in the copy.
    let screen = visible(&vdom);
    for leak in [
        "acme",
        "src/",
        "ledger_secret",
        "select",
        "Rust",
        "TypeScript",
        RUN_ID,
    ] {
        assert!(!screen.contains(leak), "`{leak}` on screen");
        assert!(!text.contains(leak), "`{leak}` in the copy");
    }
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn the_view_buttons_switch_the_one_table_and_batches_show_all_three_states() {
    let server = serve(ResponseTemplate::new(200).set_body_string(FIGURES_JSON)).await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let (mut vdom, mut marks) = mount(Some(RUN_ID)).await;

    let html = dioxus_ssr::render(&vdom);
    assert_eq!(html.matches("<table").count(), 1, "one table per panel");

    marks.click(&vdom, "run-figures-view-batches");
    pump(&mut vdom, &mut marks, 100).await;
    let html = dioxus_ssr::render(&vdom);
    assert_eq!(html.matches("<table").count(), 1, "still one table");
    assert!(html.contains("data-view=\"batches\""), "{html}");
    assert_eq!(
        rows(&vdom),
        vec![
            vec!["Scan", "0", "1", "4", "10m 40s"],
            vec!["Calibration", "2", "0", "3", "not recorded"],
            vec!["Fix", "0", "0", "5", "not recorded"],
        ]
    );
    let screen = visible(&vdom);
    for header in ["Submitted", "Processing", "Ended"] {
        assert!(screen.contains(header), "{header}: {screen}");
    }

    marks.click(&vdom, "run-figures-view-stages");
    pump(&mut vdom, &mut marks, 100).await;
    let stages = rows(&vdom);
    assert_eq!(
        stages[0],
        vec![
            "Scan",
            "12m 30s",
            "40m 01s",
            "120",
            "1,234,567",
            "98,765",
            "654,321"
        ]
    );
    assert_eq!(stages[2][0..3], ["Fix", "not recorded", "not recorded"]);

    marks.click(&vdom, "run-figures-view-roles");
    pump(&mut vdom, &mut marks, 100).await;
    let roles: Vec<String> = rows(&vdom).into_iter().map(|r| r[0].clone()).collect();
    assert_eq!(roles, ["Finder", "Reviewer", "Writer"]);

    marks.click(&vdom, "run-figures-view-models");
    pump(&mut vdom, &mut marks, 100).await;
    assert_eq!(
        rows(&vdom),
        vec![
            vec!["claude-haiku-5", "16", "Calibration and Fix"],
            vec!["claude-sonnet-5", "150", "Scan and Calibration"],
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn a_run_folder_the_server_cannot_read_leaves_time_and_batches_not_recorded() {
    let server = serve(ResponseTemplate::new(404).set_body_json(json!({
        "error": "no saved run with this id"
    })))
    .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let (mut vdom, mut marks) = mount(Some(RUN_ID)).await;

    let summary = rows(&vdom);
    let get = |figure: &str| summary.iter().find(|r| r[0] == figure).unwrap()[1].clone();
    assert_eq!(get("Wall clock"), "not recorded");
    assert_eq!(get("Sessions"), "not recorded");
    assert_eq!(get("Tokens in"), "1,507,405", "the report's figures stand");

    marks.click(&vdom, "run-figures-view-batches");
    pump(&mut vdom, &mut marks, 100).await;
    assert_eq!(rows(&vdom), vec![vec!["Batch status: not recorded"]]);

    marks.click(&vdom, "run-figures-copy");
    pump(&mut vdom, &mut marks, 100).await;
    let all = copied();
    assert_eq!(all.len(), 1, "the click copied once");
    let text = &all[0];
    assert!(
        text.lines().any(|l| l == "Wall clock: not recorded"),
        "{text}"
    );
    assert!(
        text.lines().any(|l| l == "Batch status: not recorded"),
        "{text}"
    );
    assert!(!text.contains("Wall clock: 0"), "{text}");
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn a_run_that_was_not_saved_asks_the_server_nothing() {
    let server = serve(ResponseTemplate::new(200).set_body_string(FIGURES_JSON)).await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let (vdom, _marks) = mount(None).await;
    let summary = rows(&vdom);
    let wall = summary.iter().find(|r| r[0] == "Wall clock").unwrap();
    assert_eq!(wall[1], "not recorded");
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "no run id, no request"
    );
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn the_panels_words_are_plain_with_no_long_dashes() {
    let server = serve(ResponseTemplate::new(200).set_body_string(FIGURES_JSON)).await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let (vdom, _marks) = mount(Some(RUN_ID)).await;
    let screen = visible(&vdom);
    assert!(screen.contains("Run figures"));
    assert!(screen.contains("Operator only. Never part of the client package."));
    assert!(
        !screen.contains('\u{2014}') && !screen.contains('\u{2013}'),
        "{screen}"
    );
    assert!(!screen.to_lowercase().contains("scan "), "{screen}");
}
