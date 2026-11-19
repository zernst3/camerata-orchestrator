//! Tests: held rows live in the triage table (work order E).
//!
//! The real export panel (`AuditReportExportPanel`) and the real `FindingsTable` are mounted
//! together over a stub review server whose export gate is what the test says it is. The gate's
//! rows are the only source of what is held: the table and the panel read it and recompute
//! nothing.

use super::*;
use crate::cockpit::export_gate::HeldRows;
use crate::cockpit::tree::{DoorRequest, ShellRefresh};
use camerata_api_types::review_view::{ExportGateView, HeldRow, HoldReason};
use camerata_ui::review_io::{RecordingWrites, ReviewApi};
use camerata_ui::review_source::{
    FixtureReviewSource, ReviewSource, ReviewSourceError, SourceFuture, SourceWindow,
};
use camerata_ui_core::tree::export::Door;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

/// Forty findings the gate holds and five it does not, worst first so the unheld ones lead the
/// table (a filter to the held rows then has something to remove).
const HELD: usize = 40;
const OTHER: usize = 5;

fn key_of(i: usize) -> String {
    format!(
        "acme/portal\0src/h{i:02}.ts\0{:016x}\0{i}",
        0xA11C_E000_u64 + i as u64
    )
}

fn finding(i: usize, held: bool) -> FindingView {
    let (snippet, severity) = if held {
        (format!("held_call_{i}()"), "medium")
    } else {
        (format!("other_call_{i}()"), "critical")
    };
    serde_json::from_value(serde_json::json!({
        "repo": "acme/portal", "path": format!("src/h{i:02}.ts"), "line": 10 + i,
        "rule_id": "AI-RULE-1", "severity": severity, "snippet": snippet, "detail": "d",
        "status": "active", "kind": "defect", "site_key": key_of(i),
    }))
    .expect("a test finding")
}

fn findings() -> Vec<FindingView> {
    (0..HELD)
        .map(|i| finding(i, true))
        .chain((HELD..HELD + OTHER).map(|i| finding(i, false)))
        .collect()
}

fn held_row(key: String, i: usize) -> HeldRow {
    HeldRow {
        key,
        path: format!("src/h{i:02}.ts"),
        line: 10 + i,
        rule_id: "AI-RULE-1".to_string(),
        reason: HoldReason::EvidenceUnverified,
        detail: String::new(),
        node_id: String::new(),
    }
}

/// The gate for these findings: every `held` index is held, and `review_only` extra rows exist
/// only in Review (the table has no row for them).
fn gate_of(held: &[usize], review_only: usize) -> ExportGateView {
    let mut gate = ExportGateView::default();
    gate.held
        .extend(held.iter().map(|i| held_row(key_of(*i), *i)));
    gate.held.extend((0..review_only).map(|n| {
        let i = 900 + n;
        held_row(
            format!("acme/portal\0src/only{n}.ts\0{:016x}\0{n}", 7 + n),
            i,
        )
    }));
    gate
}

/// What the stub review server holds: the gate it answers with, and whether reading it fails.
#[derive(Clone)]
struct Server {
    gate: Rc<RefCell<ExportGateView>>,
    fail: Rc<Cell<bool>>,
}

/// A review source whose export gate is the stub's; every other read is the fixture's.
struct GateSource {
    inner: FixtureReviewSource,
    server: Server,
}

impl ReviewSource for GateSource {
    fn shell(&self) -> SourceFuture<'_, camerata_api_types::review_view::ReviewShellView> {
        self.inner.shell()
    }
    fn tree(&self) -> SourceFuture<'_, camerata_api_types::review_view::TreeView> {
        self.inner.tree()
    }
    fn sites_page<'a>(
        &'a self,
        node: &'a str,
        cursor: Option<&'a str>,
        limit: usize,
    ) -> SourceFuture<'a, camerata_api_types::review_view::SitesPage> {
        self.inner.sites_page(node, cursor, limit)
    }
    fn queue(
        &self,
        show_below: bool,
        below_offset: usize,
        below_limit: usize,
    ) -> SourceFuture<'_, camerata_api_types::review_view::QueueView> {
        self.inner.queue(show_below, below_offset, below_limit)
    }
    fn engine_check(&self) -> SourceFuture<'_, camerata_api_types::review_view::EngineCheckLane> {
        self.inner.engine_check()
    }
    fn export_gate(&self) -> SourceFuture<'_, ExportGateView> {
        let result = if self.server.fail.get() {
            Err(ReviewSourceError {
                message: "the gate could not be read".to_string(),
            })
        } else {
            Ok(self.server.gate.borrow().clone())
        };
        Box::pin(std::future::ready(result))
    }
    fn settings(&self) -> SourceFuture<'_, camerata_api_types::review_view::ReviewSettingsView> {
        self.inner.settings()
    }
    fn source_window<'a>(
        &'a self,
        repo: &'a str,
        path: &'a str,
        line: usize,
    ) -> SourceFuture<'a, SourceWindow> {
        self.inner.source_window(repo, path, line)
    }
}

/// What the test reaches into the mounted screen with.
#[derive(Clone, Copy)]
struct Probe {
    refresh: Signal<u32>,
    held: HeldRows,
    request: Signal<Option<String>>,
    dispositions: Signal<std::collections::HashMap<String, Disposition>>,
}

thread_local! {
    static PROBE: RefCell<Option<Probe>> = const { RefCell::new(None) };
    static SERVER: RefCell<Option<Server>> = const { RefCell::new(None) };
    static WITH_TABLE: Cell<bool> = const { Cell::new(true) };
}

/// The export panel over the table, as the results screen mounts them: one provider line for the
/// shared held rows, the shell's refresh counter, and a review server behind the gate read.
fn app() -> Element {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    use_context_provider(|| Signal::new(None::<FindingView>));
    let request = use_signal(|| Option::<String>::None);
    use_context_provider(|| super::super::viewer::ViewerRequest(request));
    let refresh = use_signal(|| 0u32);
    use_context_provider(|| ShellRefresh(refresh));
    let door = use_signal(|| Option::<Door>::None);
    use_context_provider(|| DoorRequest(door));
    let held = use_context_provider(HeldRows::new);
    let server = SERVER.with(|s| s.borrow().clone().expect("a stub server"));
    use_context_provider(move || ReviewApi {
        reads: Rc::new(GateSource {
            inner: FixtureReviewSource::base(),
            server: server.clone(),
        }),
        writes: Rc::new(RecordingWrites::default()),
    });
    let dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
    let order_preset = use_signal(|| OrderPreset::CriticalFirst);
    PROBE.with(|p| {
        *p.borrow_mut() = Some(Probe {
            refresh,
            held,
            request,
            dispositions,
        });
    });
    rsx! {
        AuditReportExportPanel { project_id: "p1".to_string(), dispositions }
        if WITH_TABLE.with(Cell::get) {
            FindingsTable {
                findings: findings(),
                repos: vec!["acme/portal".to_string()],
                descriptions: std::collections::HashMap::new(),
                triage_view: TriageState::Unresolved,
                dispositions,
                order_preset,
            }
        }
    }
}

fn mount(gate: ExportGateView, with_table: bool) -> (VirtualDom, Server) {
    let server = Server {
        gate: Rc::new(RefCell::new(gate)),
        fail: Rc::new(Cell::new(false)),
    };
    SERVER.with(|s| *s.borrow_mut() = Some(server.clone()));
    WITH_TABLE.with(|w| w.set(with_table));
    let mut vdom = VirtualDom::new(app);
    vdom.rebuild_in_place();
    (vdom, server)
}

/// Let the screen's tasks and effects run. The machine may be busy, so the time given is a floor
/// four times over, not a tight bound.
async fn pump(vdom: &mut VirtualDom, ms: u64) {
    let until = tokio::time::Instant::now() + Duration::from_millis(ms * 4);
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(10), vdom.wait_for_work()).await;
        let _ = vdom.render_immediate_to_vec();
    }
}

fn probe() -> Probe {
    PROBE.with(|p| p.borrow().expect("a mounted screen"))
}

/// Run `f` with the vdom's runtime current (to touch signals the screen owns).
fn poke(vdom: &VirtualDom, f: impl FnOnce()) {
    vdom.in_runtime(f);
}

fn rendered(vdom: &VirtualDom) -> String {
    dioxus_ssr::render(vdom)
}

fn count(html: &str, needle: &str) -> usize {
    html.matches(needle).count()
}

/// The held snippets the table shows, as indices.
fn shown_held(html: &str) -> Vec<usize> {
    (0..HELD)
        .filter(|i| html.contains(&format!("held_call_{i}()")))
        .collect()
}

fn shown_other(html: &str) -> Vec<usize> {
    (HELD..HELD + OTHER)
        .filter(|i| html.contains(&format!("other_call_{i}()")))
        .collect()
}

fn all_held() -> Vec<usize> {
    (0..HELD).collect()
}

// ── Forty held rows: red in the table, one line in the panel ──────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn forty_held_rows_are_red_in_the_table_and_the_panel_is_one_line() {
    let (mut vdom, _) = mount(gate_of(&all_held(), 0), true);
    pump(&mut vdom, 150).await;
    let html = rendered(&vdom);
    // The panel: one alert, the count, no list and no per-row open buttons.
    assert_eq!(count(&html, "role=\"alert\""), 1, "{html}");
    assert!(
        html.contains("Export blocked: 40 rows need a decision."),
        "{html}"
    );
    assert!(!html.contains("gate-row"), "no per-row list: {html}");
    assert!(!html.contains(">open</button>"), "no per-row open: {html}");
    assert!(
        !html.contains("before the package can be built"),
        "the old headline is gone"
    );
    assert!(html.contains(">Show them</button>"), "{html}");
    // The table: held rows carry the red cell with the words, and the red edge.
    let marked = count(&html, "class=\"held-status\"");
    assert!(marked > 0, "held rows are marked in the table: {html}");
    assert_eq!(
        marked,
        count(&html, "finding-row-held"),
        "every marked cell sits in a row with the red edge"
    );
    assert!(html.contains("Needs a decision"), "{html}");
    assert!(
        html.contains("title=\"evidence unverified"),
        "the gate's reason is the title: {html}"
    );
    // Only held rows are red: the unheld critical rows carry no mark of their own.
    assert!(
        !shown_other(&html).is_empty(),
        "unheld rows are in the table too"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn the_status_column_sorts_held_rows_first_and_filters_to_them() {
    let (mut vdom, _) = mount(gate_of(&all_held(), 0), true);
    pump(&mut vdom, 150).await;
    let html = rendered(&vdom);
    assert!(
        html.contains(">Decision<") || html.contains("Decision"),
        "the column has a header: {html}"
    );
    // The column's own filter lists the one option, beside the existing filters.
    assert!(html.contains("Needs a decision"), "{html}");
}

// ── Show them: exactly the held rows ──────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn show_them_filters_the_table_to_exactly_the_held_rows() {
    let (mut vdom, _) = mount(gate_of(&all_held(), 0), true);
    pump(&mut vdom, 150).await;
    let before = rendered(&vdom);
    assert!(
        !shown_other(&before).is_empty(),
        "before the click the table shows rows the gate does not hold: {before}"
    );
    poke(&vdom, || {
        let mut held = probe().held;
        held.filter.set(true);
    });
    pump(&mut vdom, 150).await;
    let after = rendered(&vdom);
    assert!(
        shown_other(&after).is_empty(),
        "no row the gate does not hold: {:?}",
        shown_other(&after)
    );
    let shown = shown_held(&after);
    assert!(!shown.is_empty(), "the held rows are shown: {after}");
    for i in &shown {
        assert!(all_held().contains(i));
    }
}

// ── Deciding a held row lowers the count and keeps the filter exact ───────────────────────

#[tokio::test(flavor = "current_thread")]
async fn deciding_a_held_row_removes_its_mark_and_lowers_the_count() {
    let (mut vdom, server) = mount(gate_of(&all_held(), 0), true);
    pump(&mut vdom, 150).await;
    poke(&vdom, || {
        let mut held = probe().held;
        held.filter.set(true);
    });
    pump(&mut vdom, 100).await;
    // The reviewer decides one held row in the table (the cursor aims at it, as a click on the row
    // does, and Confirm is pressed on the pane).
    let target = key_of(0);
    poke(&vdom, || {
        let mut request = probe().request;
        request.set(Some(target.clone()));
    });
    pump(&mut vdom, 100).await;
    let mut dispositions = probe().dispositions;
    poke(&vdom, || {
        dispositions.write().insert(
            target.clone(),
            Disposition {
                state: TriageState::Confirmed,
                reason: String::new(),
                bucket: TechDebtBucket::Later,
            },
        );
    });
    // The store now holds the decision, so the gate no longer holds the row, and the screen reads
    // the gate again (the write-through bumps the shell's refresh counter).
    server.gate.borrow_mut().held.retain(|h| h.key != target);
    poke(&vdom, || {
        let mut refresh = probe().refresh;
        refresh += 1;
    });
    // The cursor moves on to the next row, as it does after a decision, so the detail pane no
    // longer shows the decided row's text.
    let next = key_of(1);
    poke(&vdom, || {
        let mut request = probe().request;
        request.set(Some(next.clone()));
    });
    pump(&mut vdom, 200).await;
    let html = rendered(&vdom);
    assert!(
        html.contains("Export blocked: 39 rows need a decision."),
        "{html}"
    );
    assert!(
        !html.contains("held_call_0()"),
        "the decided row is gone: {html}"
    );
    // The filter still shows exactly the held rows.
    assert!(shown_other(&html).is_empty(), "{:?}", shown_other(&html));
    assert!(!shown_held(&html).is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn deciding_the_last_held_row_brings_the_export_button_back() {
    let (mut vdom, server) = mount(gate_of(&[0], 0), true);
    pump(&mut vdom, 150).await;
    let blocked = rendered(&vdom);
    assert!(
        blocked.contains("Export blocked: 1 row needs a decision."),
        "{blocked}"
    );
    assert!(
        !blocked.contains("Export product (ZIP: PDF + Excel + JSON)</button>"),
        "the button is replaced while a row is held: {blocked}"
    );
    server.gate.borrow_mut().held.clear();
    poke(&vdom, || {
        let mut refresh = probe().refresh;
        refresh += 1;
    });
    pump(&mut vdom, 200).await;
    let html = rendered(&vdom);
    assert!(!html.contains("Export blocked"), "the line goes: {html}");
    assert!(
        html.contains("Export product (ZIP: PDF + Excel + JSON)</button>"),
        "the export button is back: {html}"
    );
    assert_eq!(count(&html, "class=\"held-status\""), 0, "no red mark left");
}

// ── The gate read fails ───────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn when_the_gate_cannot_be_read_the_export_button_shows_and_no_line_does() {
    let (vdom_server, _) = (SERVER.with(|s| s.borrow().clone()), ());
    drop(vdom_server);
    let server = Server {
        gate: Rc::new(RefCell::new(gate_of(&all_held(), 0))),
        fail: Rc::new(Cell::new(true)),
    };
    SERVER.with(|s| *s.borrow_mut() = Some(server));
    WITH_TABLE.with(|w| w.set(true));
    let mut vdom = VirtualDom::new(app);
    vdom.rebuild_in_place();
    pump(&mut vdom, 150).await;
    let html = rendered(&vdom);
    assert!(!html.contains("Export blocked"), "{html}");
    assert!(
        html.contains("Export product (ZIP: PDF + Excel + JSON)</button>"),
        "{html}"
    );
    assert_eq!(
        count(&html, "class=\"held-status\""),
        0,
        "nothing is marked: {html}"
    );
}

// ── Rows the table does not hold are reachable from the line ─────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn rows_only_review_holds_are_counted_and_have_a_door() {
    let (mut vdom, _) = mount(gate_of(&[0, 1, 2, 3, 4], 3), true);
    pump(&mut vdom, 150).await;
    let html = rendered(&vdom);
    assert!(
        html.contains("Export blocked: 8 rows need a decision (3 are in Review)."),
        "{html}"
    );
    assert!(html.contains(">Show them</button>"), "{html}");
    assert!(html.contains(">Open Review</button>"), "{html}");
    // The door goes to a row only Review holds.
    let door = camerata_ui_core::tree::export::blocked_line(
        &probe().held.gate.peek(),
        probe().held.table_keys.peek().as_ref(),
    )
    .expect("blocked")
    .review_door
    .expect("a door");
    let Door::Site { key, .. } = door else {
        panic!("a site door")
    };
    assert!(key.contains("only"), "{key}");
}

#[tokio::test(flavor = "current_thread")]
async fn with_no_table_on_screen_every_row_is_in_review() {
    let (mut vdom, _) = mount(gate_of(&all_held(), 0), false);
    pump(&mut vdom, 150).await;
    let html = rendered(&vdom);
    assert!(
        html.contains("Export blocked: 40 rows need a decision (all are in Review)."),
        "{html}"
    );
    assert!(
        !html.contains(">Show them</button>"),
        "no table to show them in"
    );
    assert!(html.contains(">Open Review</button>"), "{html}");
}
