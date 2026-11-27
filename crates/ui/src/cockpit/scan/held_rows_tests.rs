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
use dioxus::dioxus_core::ElementId;
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

fn finding_json(i: usize, held: bool) -> serde_json::Value {
    let (snippet, severity) = if held {
        (format!("held_call_{i}()"), "medium")
    } else {
        (format!("other_call_{i}()"), "critical")
    };
    serde_json::json!({
        "repo": "acme/portal", "path": format!("src/h{i:02}.ts"), "line": 10 + i,
        "rule_id": "AI-RULE-1", "severity": severity, "snippet": snippet, "detail": "d",
        "status": "active", "kind": "defect", "site_key": key_of(i),
    })
}

fn finding(i: usize, held: bool) -> FindingView {
    serde_json::from_value(finding_json(i, held)).expect("a test finding")
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
    door: Signal<Option<Door>>,
    rule_filter: Signal<Option<String>>,
    table_key: Signal<u32>,
}

thread_local! {
    static PROBE: RefCell<Option<Probe>> = const { RefCell::new(None) };
    static SERVER: RefCell<Option<Server>> = const { RefCell::new(None) };
    static WITH_TABLE: Cell<bool> = const { Cell::new(true) };
    /// Every event listener the screen has drawn, by render batch, in order.
    static LISTENERS: RefCell<Vec<Vec<(String, ElementId)>>> = const { RefCell::new(Vec::new()) };
}

/// The export panel over the table, as the results screen mounts them: one provider line for the
/// shared held rows, the shell's refresh counter, and a review server behind the gate read. The
/// table is keyed, so a test can remount it as a triage-view switch does.
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
    let rule_filter = use_signal(|| Option::<String>::None);
    let table_key = use_signal(|| 0u32);
    PROBE.with(|p| {
        *p.borrow_mut() = Some(Probe {
            refresh,
            held,
            request,
            door,
            rule_filter,
            table_key,
        });
    });
    let key = table_key();
    rsx! {
        AuditReportExportPanel { project_id: "p1".to_string(), dispositions }
        if WITH_TABLE.with(Cell::get) {
            for key in [key] {
                FindingsTable {
                    key: "{key}",
                    findings: findings(),
                    repos: vec!["acme/portal".to_string()],
                    descriptions: std::collections::HashMap::new(),
                    triage_view: TriageState::Unresolved,
                    dispositions,
                    order_preset,
                    queue_rule_filter: rule_filter,
                }
            }
        }
    }
}

fn note_listeners(m: &dioxus::dioxus_core::Mutations) {
    use dioxus::dioxus_core::Mutation;
    let batch: Vec<(String, ElementId)> = m
        .edits
        .iter()
        .filter_map(|e| match e {
            Mutation::NewEventListener { name, id } => Some((name.clone(), *id)),
            _ => None,
        })
        .collect();
    if !batch.is_empty() {
        LISTENERS.with(|l| l.borrow_mut().push(batch));
    }
}

fn mount(gate: ExportGateView, with_table: bool) -> (VirtualDom, Server) {
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
    let server = Server {
        gate: Rc::new(RefCell::new(gate)),
        fail: Rc::new(Cell::new(false)),
    };
    SERVER.with(|s| *s.borrow_mut() = Some(server.clone()));
    WITH_TABLE.with(|w| w.set(with_table));
    LISTENERS.with(|l| l.borrow_mut().clear());
    let mut vdom = VirtualDom::new(app);
    note_listeners(&vdom.rebuild_to_vec());
    (vdom, server)
}

/// Let the screen's tasks and effects run. The machine may be busy, so the time given is a floor
/// four times over, not a tight bound.
async fn pump(vdom: &mut VirtualDom, ms: u64) {
    let until = tokio::time::Instant::now() + Duration::from_millis(ms * 4);
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(10), vdom.wait_for_work()).await;
        note_listeners(&vdom.render_immediate_to_vec());
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

fn send(vdom: &VirtualDom, name: &str, id: ElementId, data: Box<dyn std::any::Any>) {
    let platform: Rc<dyn std::any::Any> = Rc::new(dioxus::html::PlatformEventData::new(data));
    vdom.runtime()
        .handle_event(name, dioxus::dioxus_core::Event::new(platform, true), id);
}

/// The listeners drawn in the first render batch after the first one that has any of `name`: for
/// the export line that is its buttons, in document order ("Show them", then "Open Review").
fn line_buttons() -> Vec<ElementId> {
    LISTENERS.with(|l| {
        l.borrow()
            .iter()
            .skip(1)
            .find(|batch| batch.iter().any(|(n, _)| n == "click"))
            .map(|batch| {
                batch
                    .iter()
                    .filter(|(n, _)| n == "click")
                    .map(|(_, id)| *id)
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// Press the real "Show them" button of the export line.
async fn press_show_them(vdom: &mut VirtualDom) {
    let buttons = line_buttons();
    assert!(!buttons.is_empty(), "the export line drew no button");
    send(
        vdom,
        "click",
        buttons[0],
        Box::new(dioxus::html::SerializedMouseData::default()),
    );
    pump(vdom, 150).await;
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

/// Aim the cursor at the finding `key`, arm the pane's keys and press C (Confirm): the reviewer's
/// real decision in the table. The row leaves the table.
async fn confirm_in_the_table(vdom: &mut VirtualDom, request: Signal<Option<String>>, key: String) {
    poke(vdom, || {
        let mut request = request;
        request.set(Some(key));
    });
    pump(vdom, 100).await;
    // The detail pane is the element that listens for both focus and keys. The newest such
    // element is tried first, then older ones, until the decision has landed (the undo banner).
    let candidates: Vec<ElementId> = LISTENERS.with(|l| {
        let all: Vec<(String, ElementId)> = l.borrow().iter().flatten().cloned().collect();
        let mut ids: Vec<ElementId> = all
            .iter()
            .filter(|(n, id)| {
                n == "focusin" && all.iter().any(|(m, other)| m == "keydown" && other == id)
            })
            .map(|(_, id)| *id)
            .collect();
        ids.reverse();
        ids
    });
    assert!(!candidates.is_empty(), "the detail pane draws no listeners");
    for pane in candidates {
        send(
            vdom,
            "focusin",
            pane,
            Box::new(dioxus::html::SerializedFocusData::default()),
        );
        pump(vdom, 50).await;
        let press = dioxus::html::SerializedKeyboardData::new(
            Key::Character("c".to_string()),
            dioxus::html::input_data::keyboard_types::Code::Unidentified,
            dioxus::html::input_data::keyboard_types::Location::Standard,
            false,
            dioxus::html::input_data::keyboard_types::Modifiers::empty(),
            false,
        );
        send(vdom, "keydown", pane, Box::new(press));
        pump(vdom, 100).await;
        if rendered(vdom).contains("camerata-pane-undo") {
            return;
        }
    }
}

// ── Forty held rows: red in the table, one line in the panel ──────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn forty_held_rows_are_red_in_the_table_and_the_panel_is_one_line() {
    let (mut vdom, _) = mount(gate_of(&all_held(), 0), true);
    pump(&mut vdom, 150).await;
    let html = rendered(&vdom);
    assert_eq!(count(&html, "role=\"alert\""), 1, "{html}");
    assert!(
        html.contains("Export blocked: 40 held rows need a decision."),
        "{html}"
    );
    assert!(!html.contains("gate-row"), "no per-row list: {html}");
    assert!(!html.contains(">open</button>"), "no per-row open: {html}");
    assert!(
        !html.contains("before the package can be built"),
        "the old headline is gone"
    );
    assert!(html.contains(">Show them</button>"), "{html}");
    let marked = count(&html, "class=\"held-status\"");
    assert!(marked > 0, "held rows are marked in the table: {html}");
    assert_eq!(
        marked,
        count(&html, "finding-row-held"),
        "every marked cell sits in a row with the red edge"
    );
    assert!(
        html.contains(">Held</"),
        "the column header is drawn: {html}"
    );
    assert!(html.contains("Held: decide before export"), "{html}");
    assert!(
        !html.contains(">Needs a decision<"),
        "the client sheet's words are not the operator's state: {html}"
    );
    assert!(
        html.contains("title=\"evidence unverified"),
        "the gate's reason is the title: {html}"
    );
    assert!(
        !shown_other(&html).is_empty(),
        "unheld rows are in the table too"
    );
}

/// The held column's header is drawn, and the column sorts and filters through chorale's own
/// pipeline (the header click and the filter popover call these same functions): ascending puts
/// held rows first, and the Held filter keeps exactly them.
#[tokio::test(flavor = "current_thread")]
async fn the_held_column_has_a_header_and_sorts_and_filters_held_rows() {
    use chorale_core::{filtered_sorted_rows, set_filter, toggle_sort, FilterValue, SortAction};
    let (mut vdom, _) = mount(gate_of(&all_held(), 0), true);
    pump(&mut vdom, 150).await;
    assert!(
        rendered(&vdom).contains(">Held</"),
        "the column header says Held"
    );

    // The same column, built the way the table builds it, over the same rows and the gate's keys.
    let store: HeldStore = HeldStore::default();
    if let Ok(mut keys) = store.write() {
        *keys = (0..HELD).map(key_of).collect();
    }
    let rows: Vec<(RowId, FindingView)> =
        findings().into_iter().map(|f| (RowId::new(), f)).collect();
    let mut state = finding_table_state(rows, vec!["acme/portal".to_string()], false);
    state.columns.insert(0, held_column(store));
    let snippets = |state: &chorale_core::TableState<FindingView>| -> Vec<String> {
        filtered_sorted_rows(state)
            .iter()
            .map(|f| f.snippet.clone())
            .collect()
    };
    let sorted = toggle_sort(&state, HELD_COLUMN, SortAction::Replace);
    let order = snippets(&sorted);
    assert!(
        order[..HELD].iter().all(|s| s.starts_with("held_call_")),
        "ascending by Held puts every held row first: {order:?}"
    );
    assert!(order[HELD..].iter().all(|s| s.starts_with("other_call_")));
    let filtered = set_filter(
        &state,
        HELD_COLUMN,
        Some(FilterValue::MultiSelect(
            std::iter::once(HELD_WORD.to_string()).collect(),
        )),
    );
    let kept = snippets(&filtered);
    assert_eq!(kept.len(), HELD, "{kept:?}");
    assert!(kept.iter().all(|s| s.starts_with("held_call_")));
}

// ── Show them: exactly the held rows, whatever else filters the table ─────────────────────

#[tokio::test(flavor = "current_thread")]
async fn show_them_filters_the_table_to_exactly_the_held_rows() {
    let (mut vdom, _) = mount(gate_of(&all_held(), 0), true);
    pump(&mut vdom, 150).await;
    let buttons = line_buttons();
    let before = rendered(&vdom);
    assert!(
        !shown_other(&before).is_empty(),
        "before the click the table shows rows the gate does not hold: {before}"
    );
    press_show_them(&mut vdom).await;
    assert!(
        *probe().held.filter.peek(),
        "the click is what asks for the filter ({} buttons)",
        buttons.len()
    );
    let after = rendered(&vdom);
    assert!(
        shown_other(&after).is_empty(),
        "no row the gate does not hold: {:?}",
        shown_other(&after)
    );
    assert!(
        !shown_held(&after).is_empty(),
        "the held rows are shown: {after}"
    );
}

/// A rule, repo or text filter would hide held rows the line counts. Show them clears the others.
#[tokio::test(flavor = "current_thread")]
async fn show_them_clears_the_other_column_filters_that_hide_held_rows() {
    let (mut vdom, _) = mount(gate_of(&all_held(), 0), true);
    pump(&mut vdom, 150).await;
    // The operator has the hub's rule filter on, for a rule no row has.
    poke(&vdom, || {
        let mut rule = probe().rule_filter;
        rule.set(Some("NO-SUCH-RULE".to_string()));
    });
    pump(&mut vdom, 100).await;
    let hidden = rendered(&vdom);
    assert!(
        shown_held(&hidden).is_empty() && shown_other(&hidden).is_empty(),
        "anti-vacuity: the rule filter hides every row: {hidden}"
    );
    assert!(
        hidden.contains("Export blocked: 40 held rows"),
        "the line still counts them"
    );
    press_show_them(&mut vdom).await;
    let after = rendered(&vdom);
    assert!(
        !shown_held(&after).is_empty(),
        "the held rows are visible: {after}"
    );
    assert!(shown_other(&after).is_empty());
    assert_eq!(
        *probe().rule_filter.peek(),
        None,
        "the rule filter was cleared"
    );
}

// ── The filter ends when nothing is held, and does not outlive a remount it should not ────

#[tokio::test(flavor = "current_thread")]
async fn the_filter_is_cleared_when_the_last_held_row_is_decided_and_the_table_shows_every_row() {
    let (mut vdom, server) = mount(gate_of(&[0], 0), true);
    pump(&mut vdom, 150).await;
    press_show_them(&mut vdom).await;
    assert!(*probe().held.filter.peek());
    assert!(
        shown_other(&rendered(&vdom)).is_empty(),
        "filtered to the held row"
    );
    server.gate.borrow_mut().held.clear();
    poke(&vdom, || {
        let mut refresh = probe().refresh;
        refresh += 1;
    });
    pump(&mut vdom, 200).await;
    assert!(
        !*probe().held.filter.peek(),
        "nothing is held, so nobody wants the filter"
    );
    let html = rendered(&vdom);
    assert!(!shown_other(&html).is_empty(), "every row is back: {html}");
    // And a later remount does not bring the filter back.
    poke(&vdom, || {
        let mut key = probe().table_key;
        key += 1;
    });
    pump(&mut vdom, 150).await;
    assert!(
        !shown_other(&rendered(&vdom)).is_empty(),
        "no filter after a remount"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn the_filter_survives_a_remount_while_the_operator_still_wants_it() {
    let (mut vdom, _) = mount(gate_of(&all_held(), 0), true);
    pump(&mut vdom, 150).await;
    press_show_them(&mut vdom).await;
    assert!(shown_other(&rendered(&vdom)).is_empty());
    // A triage-view switch remounts the table.
    poke(&vdom, || {
        let mut key = probe().table_key;
        key += 1;
    });
    pump(&mut vdom, 200).await;
    let html = rendered(&vdom);
    assert!(*probe().held.filter.peek(), "still wanted");
    assert!(
        shown_other(&html).is_empty(),
        "still only held rows: {html}"
    );
    assert!(!shown_held(&html).is_empty(), "{html}");
}

// ── Deciding in the table ─────────────────────────────────────────────────────────────────

/// The reviewer's real decision (C on the pane) takes the row out of the table. When the write is
/// not stored the gate still holds it, so the line must say that row is only in Review and offer
/// the door: the set of rows the table holds is read live, not frozen at mount.
#[tokio::test(flavor = "current_thread")]
async fn a_row_decided_in_the_table_whose_write_was_not_stored_is_counted_as_in_review() {
    let (mut vdom, _) = mount(gate_of(&all_held(), 0), true);
    pump(&mut vdom, 150).await;
    assert!(
        rendered(&vdom).contains("Export blocked: 40 held rows need a decision."),
        "anti-vacuity: every held row is in the table to start with"
    );
    confirm_in_the_table(&mut vdom, probe().request, key_of(0)).await;
    pump(&mut vdom, 200).await;
    let html = rendered(&vdom);
    assert!(
        !html.contains("held_call_0()") || html.contains("data-finding-key"),
        "{html}"
    );
    assert!(
        html.contains("Export blocked: 40 held rows need a decision (1 is in Review)."),
        "{html}"
    );
    assert!(html.contains(">Open Review</button>"), "{html}");
}

#[tokio::test(flavor = "current_thread")]
async fn deciding_a_held_row_removes_its_mark_and_lowers_the_count_once_the_store_has_it() {
    let (mut vdom, server) = mount(gate_of(&all_held(), 0), true);
    pump(&mut vdom, 150).await;
    press_show_them(&mut vdom).await;
    confirm_in_the_table(&mut vdom, probe().request, key_of(0)).await;
    // The store now holds the decision, so the gate no longer holds the row, and the screen reads
    // the gate again (the write-through bumps the shell's refresh counter; the whole-screen test
    // below proves it does).
    server.gate.borrow_mut().held.retain(|h| h.key != key_of(0));
    poke(&vdom, || {
        let mut refresh = probe().refresh;
        refresh += 1;
    });
    // The cursor moves on to the next row, as it does after a decision.
    let next = key_of(1);
    poke(&vdom, || {
        let mut request = probe().request;
        request.set(Some(next.clone()));
    });
    pump(&mut vdom, 200).await;
    let html = rendered(&vdom);
    assert!(
        html.contains("Export blocked: 39 held rows need a decision."),
        "{html}"
    );
    assert!(
        !html.contains("held_call_0()"),
        "the decided row is gone: {html}"
    );
    assert!(shown_other(&html).is_empty(), "{:?}", shown_other(&html));
    assert!(!shown_held(&html).is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn deciding_the_last_held_row_brings_the_export_button_back() {
    let (mut vdom, server) = mount(gate_of(&[0], 0), true);
    pump(&mut vdom, 150).await;
    let blocked = rendered(&vdom);
    assert!(
        blocked.contains("Export blocked: 1 held row needs a decision."),
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
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
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
        html.contains("Export blocked: 8 held rows need a decision (3 are in Review)."),
        "{html}"
    );
    assert!(html.contains(">Show them</button>"), "{html}");
    assert!(html.contains(">Open Review</button>"), "{html}");
    // Press the real Open Review button: the shell is asked for a row only Review holds. The
    // buttons are the line's two, "Show them" and "Open Review"; each is pressed in turn and what
    // each did is read, so the test does not depend on the order they were drawn in.
    let buttons = line_buttons();
    assert_eq!(buttons.len(), 2, "Show them and Open Review");
    let mut asked = None;
    for id in buttons {
        send(
            &vdom,
            "click",
            id,
            Box::new(dioxus::html::SerializedMouseData::default()),
        );
        pump(&mut vdom, 50).await;
        asked = vdom.in_runtime(|| probe().door.peek().clone());
        if asked.is_some() {
            break;
        }
    }
    let Some(Door::Site { key, .. }) = asked else {
        panic!("the click asked for no site door")
    };
    assert!(key.contains("only"), "a row only Review holds: {key}");
}

#[tokio::test(flavor = "current_thread")]
async fn with_no_table_on_screen_every_row_is_in_review() {
    let (mut vdom, _) = mount(gate_of(&all_held(), 0), false);
    pump(&mut vdom, 150).await;
    let html = rendered(&vdom);
    assert!(
        html.contains("Export blocked: 40 held rows need a decision (all are in Review)."),
        "{html}"
    );
    assert!(
        !html.contains(">Show them</button>"),
        "no table to show them in"
    );
    assert!(html.contains(">Open Review</button>"), "{html}");
}

// ── The whole screen: a decision in the table reaches the export line through the store ───

/// The decision store and the review gate of a stub server: a decision posted to the store is
/// remembered, and the gate then no longer holds that row.
#[derive(Clone, Default)]
struct Store {
    decided: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}

struct DecisionPost(Store);

impl wiremock::Respond for DecisionPost {
    fn respond(&self, request: &wiremock::Request) -> wiremock::ResponseTemplate {
        use camerata_api_types::decisions::{Decision, DecisionRecorded, DecisionState, Scope};
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap_or_default();
        let key = body["key"].as_str().unwrap_or_default().to_string();
        if let Ok(mut decided) = self.0.decided.lock() {
            decided.insert(key.clone());
        }
        wiremock::ResponseTemplate::new(200).set_body_json(DecisionRecorded {
            decision: Decision {
                id: "d1".to_string(),
                key,
                state: DecisionState::Confirmed,
                reason: String::new(),
                scope: Scope::Site,
                applies_to: Vec::new(),
                except: Vec::new(),
                inherited_from: None,
                decided_by: "reviewer".to_string(),
                decided_at: "2026-11-19T00:00:00Z".to_string(),
                run_id: "insp-1".to_string(),
                engine_version: "test".to_string(),
                needed_me: None,
                sample_verdict: None,
                confirmed_by_client: None,
                revoked_at: None,
            },
            inherited_count: 0,
        })
    }
}

struct GateGet(Store);

impl wiremock::Respond for GateGet {
    fn respond(&self, _: &wiremock::Request) -> wiremock::ResponseTemplate {
        let decided = self.0.decided.lock().map(|d| d.clone()).unwrap_or_default();
        let mut gate = gate_of(&all_held(), 0);
        gate.held.retain(|h| !decided.contains(&h.key));
        wiremock::ResponseTemplate::new(200).set_body_json(gate)
    }
}

/// The request slot the results screen provides for its viewer: writing a finding key into it is
/// how another surface aims the table's cursor. It is provided inside `ScanResults`, so it is found
/// by asking the scopes the screen created.
fn screen_request(vdom: &VirtualDom) -> Signal<Option<String>> {
    use dioxus::dioxus_core::ScopeId;
    (ScopeId::APP.0..ScopeId::APP.0 + 40)
        .find_map(|id| {
            vdom.in_scope(ScopeId(id), || {
                dioxus::prelude::try_consume_context::<super::super::viewer::ViewerRequest>()
            })
        })
        .map(|r| r.0)
        .expect("the results screen provides its viewer request")
}

fn screen() -> Element {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    use_context_provider(|| Signal::new(None::<ScanReportView>));
    use_context_provider(|| Signal::new(CockpitView::Stories));
    use_context_provider(|| Signal::new(None::<String>));
    use_context_provider(|| Signal::new(0_usize) as crate::loading::LoadingCount);
    let report: ScanReportView = serde_json::from_value(serde_json::json!({
        "repos": ["acme/portal"], "files_scanned": 1, "findings": [],
        "proposed_rules": [], "gated": false
    }))
    .expect("the scan the draft belongs to");
    rsx! { ScanResults { report } }
}

/// The reviewer decides a held row in the table (C on the detail pane). The decision is written
/// through to the store, the screen reads the release gate again, and the export line drops from
/// 40 held rows to 39. Without the refresh after the write the line would stay at 40.
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn a_decision_in_the_table_reaches_the_export_line_through_the_store() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    let store = Store::default();
    let findings_json: Vec<serde_json::Value> = (0..HELD)
        .map(|i| finding_json(i, true))
        .chain((HELD..HELD + OTHER).map(|i| finding_json(i, false)))
        .collect();
    let scan = serde_json::json!({
        "repos": ["acme/portal"], "files_scanned": 1, "findings": findings_json,
        "proposed_rules": [], "gated": false
    });
    Mock::given(method("GET"))
        .and(path("/api/onboard/draft"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "scan": { "repos": ["acme/portal"], "files_scanned": 1, "findings": [],
                      "proposed_rules": [], "gated": false },
            "audit": scan,
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/onboard/draft"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/projects/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "p1", "name": "Acme", "client_name": ""
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/projects/p1/decisions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "decisions": [], "standings": [], "migrated_from_draft": true,
            "latest_run_id": "insp-1"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/projects/p1/decisions"))
        .respond_with(DecisionPost(store.clone()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/projects/p1/review/export-gate"))
        .respond_with(GateGet(store.clone()))
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
    LISTENERS.with(|l| l.borrow_mut().clear());
    let mut vdom = VirtualDom::new(screen);
    note_listeners(&vdom.rebuild_to_vec());
    // Wait for the screen to load its draft, the decisions and the gate.
    let mut html = String::new();
    for _ in 0..40 {
        pump(&mut vdom, 50).await;
        html = rendered(&vdom);
        if html.contains("Export blocked: 40 held rows need a decision") {
            break;
        }
    }
    assert!(
        html.contains("Export blocked: 40 held rows need a decision"),
        "the screen never showed the held rows: {html}"
    );
    // The Queue is drawn once the decisions are read; wait for its detail pane.
    for _ in 0..40 {
        if html.contains("camerata-finding-pane") {
            break;
        }
        pump(&mut vdom, 50).await;
        html = rendered(&vdom);
    }
    assert!(
        html.contains("camerata-finding-pane"),
        "the Queue never drew: {html}"
    );
    let request = screen_request(&vdom);
    confirm_in_the_table(&mut vdom, request, key_of(0)).await;
    for _ in 0..40 {
        pump(&mut vdom, 50).await;
        html = rendered(&vdom);
        if html.contains("Export blocked: 39 held rows need a decision") {
            break;
        }
    }
    assert!(
        store.decided.lock().is_ok_and(|d| d.contains(&key_of(0))),
        "the decision was written to the store (pane drawn: {}, held rows drawn: {}, \
         requests: {:?})",
        html.contains("camerata-finding-pane"),
        shown_held(&html).len(),
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .map(|r| format!("{} {}", r.method, r.url.path()))
            .filter(|r| !r.contains("finding-context") && !r.contains("finding-fix"))
            .collect::<Vec<_>>()
    );
    assert!(
        html.contains("Export blocked: 39 held rows need a decision"),
        "the export line did not follow the stored decision: {html}"
    );
    // The real results screen hosts the table in the container that fills its width and scrolls
    // from the left edge (the rules are pinned in the gallery tests), with the Held column in it.
    assert!(html.contains("class=\"queue-split\""), "{html}");
    assert!(html.contains("queue-split-table"), "{html}");
    assert!(
        html.contains(">Held</"),
        "the real screen's table has the Held column"
    );
}

/// A row that is both critical and held shows the held tint: the held background rule must come
/// after the critical one in the stylesheet (same specificity, so the later rule wins).
#[test]
fn a_critical_held_row_shows_the_held_tint_because_its_rule_comes_last() {
    let css = crate::style::GLOBAL_CSS;
    let critical = css
        .find(".finding-row-critical > td { background")
        .expect("the critical tint rule");
    let held = css
        .find(".finding-row-held > td { background")
        .expect("the held tint rule");
    assert!(
        held > critical,
        "the held tint must follow the critical tint"
    );
}

/// The held pill said "Held: decide before expo" because its column was narrower than the words.
/// The column is wide enough for the whole pill and the pill never wraps or clips mid-word.
#[test]
fn the_held_pill_has_room_for_its_whole_text() {
    // About 7px a character at the pill's 12px bold size, plus its padding and the cell's.
    let needed = HELD_CELL_TEXT.chars().count() as f64 * 7.0 + 16.0 + 16.0;
    assert!(
        HELD_COLUMN_WIDTH >= needed,
        "the column ({HELD_COLUMN_WIDTH}px) must hold {HELD_CELL_TEXT:?} ({needed}px)"
    );
    let css = crate::style::GLOBAL_CSS;
    let rule = css.find(".held-status {").expect("the pill rule");
    let end = css[rule..].find('}').unwrap();
    assert!(css[rule..rule + end].contains("white-space: nowrap"));
}

/// chorale gives the checkbox cell (the first cell) an inline `box-shadow` for its row separator;
/// an inline style beats a plain stylesheet rule, so the held edge only shows if its rule is
/// `!important`. The edge is 7px, wider than the critical row's 4px.
#[test]
fn the_held_edge_rule_beats_the_first_cells_inline_box_shadow() {
    let css = crate::style::GLOBAL_CSS;
    let at = css
        .find(".chorale-root .finding-row-held > td:first-child {")
        .expect("the held edge rule inside a chorale table");
    let end = css[at..].find('}').unwrap();
    let rule = &css[at..at + end];
    assert!(rule.contains("inset 7px 0 0 0"), "{rule}");
    assert!(rule.contains("!important"), "{rule}");
}

/// The held tint is not the critical tint (a flat 6% red): it is a striped, stronger red.
#[test]
fn the_held_tint_differs_from_the_critical_tint() {
    let css = crate::style::GLOBAL_CSS;
    let tint = |sel: &str| {
        let at = css.find(sel).unwrap_or_else(|| panic!("{sel}"));
        let end = css[at..].find('}').unwrap();
        css[at..at + end].to_string()
    };
    let critical = tint(".finding-row-critical > td { background");
    let held = tint(".finding-row-held > td { background");
    assert!(critical.contains("rgba(212, 51, 43, 0.06)"));
    assert!(held.contains("repeating-linear-gradient"), "{held}");
    assert!(!held.contains("0.06"), "{held}");
}

/// Both row edges (critical 4px, held 7px) sit on the checkbox cell, whose inline `box-shadow` is
/// chorale's row separator. Each edge rule is `!important` and re-draws the separator in the
/// colour chorale puts on the row (`--chorale-separator-color`), not a fixed black, so the row
/// keeps its divider (and a selected row its selected divider colour).
#[test]
fn both_row_edges_redraw_the_separator_in_chorales_colour() {
    let css = crate::style::GLOBAL_CSS;
    for sel in [
        ".chorale-root .finding-row-critical > td:first-child {",
        ".chorale-root .finding-row-held > td:first-child {",
    ] {
        let at = css.find(sel).unwrap_or_else(|| panic!("{sel}"));
        let end = css[at..].find('}').unwrap();
        let rule = &css[at..at + end];
        assert!(rule.contains("!important"), "{rule}");
        assert!(
            rule.contains("inset 0 -1px 0 0 var(--chorale-separator-color"),
            "{rule}"
        );
        assert!(!rule.contains("rgba(0, 0, 0, 0.25) !important"), "{rule}");
    }
}

/// The queue and the tree reuse `finding-row-critical` on plain (non-chorale) tables, where a
/// first-cell marker such as the over-budget underline must still win: the plain edge rules carry
/// no `!important`; only the `.chorale-root` variants do.
#[test]
fn the_plain_edge_rules_leave_other_tables_first_cell_markers_alone() {
    let css = crate::style::GLOBAL_CSS;
    for sel in [
        "\n.finding-row-critical > td:first-child {",
        "\n.finding-row-held > td:first-child {",
    ] {
        let at = css.find(sel).unwrap_or_else(|| panic!("{sel}"));
        let end = css[at..].find('}').unwrap();
        let rule = &css[at..at + end];
        assert!(!rule.contains("!important"), "{rule}");
    }
}

// ── A saved draft this build cannot read: decisions still reach the store, the draft is left alone ──

fn note_tours(m: &dioxus::dioxus_core::Mutations, tours: &mut Vec<(String, ElementId)>) {
    use dioxus::dioxus_core::{AttributeValue, Mutation};
    for e in &m.edits {
        if let Mutation::SetAttribute {
            name: "data-tour",
            value: AttributeValue::Text(text),
            id,
            ..
        } = e
        {
            tours.push((text.clone(), *id));
        }
        // The pre-flight's Start (#564) is found by its html id, kept with the markers.
        if let Mutation::SetAttribute {
            name: "id",
            value: AttributeValue::Text(text),
            id,
            ..
        } = e
        {
            if text == super::PREFLIGHT_START_ID {
                tours.push((text.clone(), *id));
            }
        }
    }
}

async fn pump_tours(vdom: &mut VirtualDom, ms: u64, tours: &mut Vec<(String, ElementId)>) {
    let until = tokio::time::Instant::now() + Duration::from_millis(ms * 4);
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(10), vdom.wait_for_work()).await;
        let m = vdom.render_immediate_to_vec();
        note_listeners(&m);
        note_tours(&m, tours);
    }
}

fn screen_with_a_rule() -> Element {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    use_context_provider(|| Signal::new(None::<ScanReportView>));
    use_context_provider(|| Signal::new(CockpitView::Stories));
    use_context_provider(|| Signal::new(None::<String>));
    use_context_provider(|| Signal::new(0_usize) as crate::loading::LoadingCount);
    let report: ScanReportView = serde_json::from_value(serde_json::json!({
        "repos": ["acme/portal"], "files_scanned": 1, "findings": [], "gated": false,
        "proposed_rules": [{
            "id": "R-1", "title": "No raw SQL", "kind": "semantic", "scope": "repo-local",
            "repos": ["acme/portal"], "is_auto_recommended": true, "recommended": true,
            "verification": "grounded"
        }]
    }))
    .expect("a scan with one pre-selected rule");
    rsx! { ScanResults { report } }
}

/// The saved draft has an audit this build cannot read. The screen says so in a banner that
/// stays, writes nothing over the draft, and still writes the reviewer's decisions to the store:
/// the draft's save is the only thing switched off.
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn an_unreadable_draft_switches_off_only_the_draft_save_never_a_decision() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    let store = Store::default();
    // The draft: a scan, and an audit whose findings is a number.
    Mock::given(method("GET"))
        .and(path("/api/onboard/draft"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "scan": { "repos": ["acme/portal"], "files_scanned": 1, "findings": [],
                      "proposed_rules": [], "gated": false },
            "audit": { "repos": ["acme/portal"], "findings": 7 },
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/onboard/draft"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&server)
        .await;
    // Run mode is the Engine finder's control; the first mock mounted wins.
    Mock::given(method("GET"))
        .and(path("/api/projects/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "p1", "name": "Acme", "repos": ["acme/portal"],
            "review": { "finder": "engine" },
            "backend": "api", "client_engagement": true, "client_name": "",
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/projects/p1/decisions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "decisions": [], "standings": [], "migrated_from_draft": true,
            "latest_run_id": "insp-1"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/projects/p1/decisions"))
        .respond_with(DecisionPost(store.clone()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/projects/p1/review/export-gate"))
        .respond_with(GateGet(store.clone()))
        .mount(&server)
        .await;
    // The reviewer runs an inspection on this screen; its report has one finding.
    Mock::given(method("POST"))
        .and(path("/api/onboard/audit/start"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "job_id": "job-9"
        })))
        .mount(&server)
        .await;
    super::preflight_start_tests::mount_open_plan(&server).await;
    let report = serde_json::json!({
        "repos": ["acme/portal"], "files_scanned": 1, "gated": false, "proposed_rules": [],
        "findings": [finding_json(0, true)], "run_id": "20261118T101500Z-ab12cd"
    });
    Mock::given(method("GET"))
        .and(path("/api/onboard/audit/job/job-9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "job": { "status": "done", "done": 1, "total": 1, "findings": [],
                     "report": report, "message": null },
            "idle_ms": null, "cancel_requested": false
        })))
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
    LISTENERS.with(|l| l.borrow_mut().clear());
    let mut tours = Vec::new();
    let mut vdom = VirtualDom::new(screen_with_a_rule);
    let first = vdom.rebuild_to_vec();
    note_listeners(&first);
    note_tours(&first, &mut tours);
    pump_tours(&mut vdom, 200, &mut tours).await;

    // The banner is up, and stays up.
    let html = rendered(&vdom);
    assert!(
        html.contains("could not be read") && html.contains("is not saving this review"),
        "a banner, not only a toast: {html}"
    );

    // Run the inspection, then decide the finding in the table.
    let run = tours
        .iter()
        .rev()
        .find(|(t, _)| t == "run-inspection")
        .map(|(_, id)| *id)
        .expect("the run button");
    send(
        &vdom,
        "click",
        run,
        Box::new(dioxus::html::SerializedMouseData::default()),
    );
    // Start in the pre-flight panel sends the inspection (#564).
    pump_tours(&mut vdom, 400, &mut tours).await;
    let go = tours
        .iter()
        .rev()
        .find(|(t, _)| t == super::PREFLIGHT_START_ID)
        .map(|(_, id)| *id)
        .expect("the pre-flight's Start");
    send(
        &vdom,
        "click",
        go,
        Box::new(dioxus::html::SerializedMouseData::default()),
    );
    let mut html = String::new();
    for _ in 0..60 {
        pump_tours(&mut vdom, 50, &mut tours).await;
        html = rendered(&vdom);
        if html.contains("camerata-finding-pane") {
            break;
        }
    }
    assert!(
        html.contains("camerata-finding-pane"),
        "the Queue never drew: {html}"
    );
    let request = screen_request(&vdom);
    confirm_in_the_table(&mut vdom, request, key_of(0)).await;
    pump_tours(&mut vdom, 100, &mut tours).await;

    assert!(
        store.decided.lock().is_ok_and(|d| d.contains(&key_of(0))),
        "the decision reached the store"
    );
    let draft_posts = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path() == "/api/onboard/draft")
        .count();
    assert_eq!(
        draft_posts, 0,
        "nothing was written over the unreadable draft"
    );
    assert!(
        rendered(&vdom).contains("is not saving this review"),
        "the banner is still there after the decision"
    );
}
