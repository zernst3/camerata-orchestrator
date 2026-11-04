//! Tests: order preset followup tests.

use super::*;
use dioxus::dioxus_core::{ElementId, Mutation, Mutations};
use dioxus::html::{
    Code, Key, Location, Modifiers, SerializedFocusData, SerializedFormData, SerializedKeyboardData,
};
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

fn finding(rule: &str, sev: &str, path: &str, line: usize) -> FindingView {
    serde_json::from_value(serde_json::json!({
        "repo": "me/api", "path": path, "line": line, "rule_id": rule, "severity": sev,
        "snippet": format!("snip-{rule}-{line}"), "detail": "d", "status": "active",
        "kind": "defect"
    }))
    .expect("fixture")
}

fn sorts() -> Vec<Vec<chorale_core::SortState>> {
    use chorale_core::{SortDirection, SortState};
    vec![
        vec![],
        vec![SortState::new(ColumnId("severity"), SortDirection::Desc)],
        vec![
            SortState::new(ColumnId("file"), SortDirection::Asc),
            SortState::new(ColumnId("kind"), SortDirection::Desc),
        ],
        vec![SortState::new(
            ColumnId("no-such-column"),
            SortDirection::Asc,
        )],
    ]
}

// ── Q2: no em-dash anywhere in the order text ───────────────────────────────────────

#[test]
fn no_order_text_contains_an_em_dash() {
    let cols = finding_columns(vec!["me/api".to_string()], false);
    let dash = '\u{2014}';
    for preset in OrderPreset::ALL {
        for sort in sorts() {
            let text = order_indicator_text(preset, &sort, &cols);
            assert!(!text.contains(dash), "{preset:?} {sort:?}: {text}");
            assert!(!queue_order(preset, &sort, &cols).text.contains(dash));
        }
    }
    // And the rendered bar: its sentence, its select, its tooltips.
    #[component]
    fn Bar(preset: OrderPreset, sort: Vec<chorale_core::SortState>) -> Element {
        let cols = finding_columns(vec!["me/api".to_string()], false);
        rsx! {
            QueueOrderBar {
                order: queue_order(preset, &sort, &cols),
                on_reset: Callback::new(|()| {}),
                preset,
                on_preset: Callback::new(|_: OrderPreset| {}),
            }
        }
    }
    for preset in OrderPreset::ALL {
        for sort in sorts() {
            let mut vdom = VirtualDom::new_with_props(Bar, BarProps { preset, sort });
            vdom.rebuild_in_place();
            let html = dioxus_ssr::render(&vdom);
            assert!(!html.contains(dash), "{preset:?}: {html}");
        }
    }
}

// ── event plumbing (the same idiom the viewer's host tests use) ─────────────────────

#[derive(Default)]
struct Listeners(Vec<(String, ElementId)>);

impl Listeners {
    fn take(&mut self, m: &Mutations) {
        for e in &m.edits {
            if let Mutation::NewEventListener { name, id } = e {
                self.0.push((name.clone(), *id));
            }
        }
    }
    fn last(&self, name: &str) -> ElementId {
        self.0
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, id)| *id)
            .unwrap_or_else(|| panic!("no {name} listener among {:?}", self.0))
    }
}

fn send(vdom: &VirtualDom, name: &str, id: ElementId, data: Box<dyn Any>) {
    let platform: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(data));
    vdom.runtime()
        .handle_event(name, dioxus::dioxus_core::Event::new(platform, true), id);
}

async fn settle(vdom: &mut VirtualDom, seen: &mut Listeners, ms: u64) {
    let until = tokio::time::Instant::now() + Duration::from_millis(ms);
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(10), vdom.wait_for_work()).await;
        seen.take(&vdom.render_immediate_to_vec());
    }
}

// ── Q3: the select hands the keyboard back to the pane ─────────────────────────────

thread_local! {
    static PICKED: RefCell<Vec<OrderPreset>> = const { RefCell::new(Vec::new()) };
}

#[test]
fn choosing_a_preset_in_the_select_returns_keyboard_focus_to_the_pane() {
    fn bar() -> Element {
        let cols = finding_columns(vec!["me/api".to_string()], false);
        rsx! {
            QueueOrderBar {
                order: queue_order(OrderPreset::Curated, &[], &cols),
                on_reset: Callback::new(|()| {}),
                on_preset: Callback::new(|p: OrderPreset| PICKED.with(|l| l.borrow_mut().push(p))),
            }
        }
    }
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
    let mut vdom = VirtualDom::new(bar);
    let mut seen = Listeners::default();
    seen.take(&vdom.rebuild_to_vec());
    PICKED.with(|l| l.borrow_mut().clear());
    super::super::viewer::FOCUS_LOG.with(|l| l.borrow_mut().clear());

    let select = seen.last("change");
    let data = SerializedFormData::new("by_file".to_string(), Vec::new());
    send(&vdom, "change", select, Box::new(data));

    assert_eq!(
        PICKED.with(|l| l.borrow().clone()),
        vec![OrderPreset::ByFile]
    );
    assert_eq!(
        super::super::viewer::FOCUS_LOG.with(|l| l.borrow().clone()),
        vec![DETAIL_PANE_ID.to_string()],
        "focus goes to the pane, through the one focus call"
    );
    // A value that names no preset changes nothing and takes no focus.
    PICKED.with(|l| l.borrow_mut().clear());
    super::super::viewer::FOCUS_LOG.with(|l| l.borrow_mut().clear());
    let junk = SerializedFormData::new("nonsense".to_string(), Vec::new());
    send(&vdom, "change", select, Box::new(junk));
    assert!(PICKED.with(|l| l.borrow().is_empty()));
    assert!(super::super::viewer::FOCUS_LOG.with(|l| l.borrow().is_empty()));
}

// ── Q4: the cursor's row is scrolled back into view after a re-cut ─────────────────

#[test]
fn the_scroll_target_puts_the_cursor_row_in_the_upper_third_and_none_when_hidden() {
    let findings: Vec<FindingView> = (1..=60)
        .map(|n| finding("R-A", "high", &format!("src/f{n:02}.rs"), n))
        .collect();
    let rows: Vec<_> = findings.iter().map(|f| (RowId::new(), f.clone())).collect();
    let mut state = finding_table_state(rows, vec!["me/api".to_string()], false);
    state.row_height = 30.0;
    state.viewport_height = 600.0;
    // Flat: row N is at index N-1.
    let key = finding_key(&findings[49]);
    let top = cursor_scroll_top(&state, &key).expect("a visible row");
    assert_eq!(
        top,
        camerata_ui_core::viewer::scroll_top_for_line(50, 30, 600.0, 60)
    );
    assert!(top > 0.0, "a deep row needs a scroll");
    // The first row needs none.
    assert_eq!(
        cursor_scroll_top(&state, &finding_key(&findings[0])),
        Some(0.0)
    );
    // Grouped and collapsed, no row is on screen to scroll to.
    let state = chorale_core::collapse_all_groups(&chorale_core::set_grouping(
        &state,
        group_columns(OrderPreset::ByFile),
    ));
    assert_eq!(cursor_scroll_top(&state, &key), None);
    // Opened again by `reach_finding`'s toggles, it is there.
    let plan = super::super::viewer::plan_reach(&state, &key);
    let super::super::viewer::Reach::NeedsExpand(groups) = plan else {
        panic!("expected NeedsExpand");
    };
    let mut open = state;
    for g in &groups {
        open = chorale_core::toggle_group(&open, g);
    }
    assert!(cursor_scroll_top(&open, &key).is_some());
}

#[test]
fn the_scroll_script_targets_the_queue_tables_own_container() {
    let js = scroll_table_js(1234.5);
    assert!(js.contains("1234.5"), "{js}");
    assert!(js.contains(".queue-split-table"), "{js}");
    assert!(js.contains("chorale-scroll-"), "{js}");
    assert_eq!(js.matches("requestAnimationFrame").count(), 2, "{js}");
}

// ── M1: a decided row, then a re-cut ────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct Probe {
    preset: Signal<OrderPreset>,
    request: Signal<Option<String>>,
}

thread_local! {
    static PROBE: RefCell<Option<Probe>> = const { RefCell::new(None) };
}

fn rows() -> Vec<FindingView> {
    vec![
        finding("R-A", "high", "src/z.rs", 40),
        finding("R-B", "critical", "src/a.rs", 2),
        finding("R-A", "critical", "src/a.rs", 9),
        finding("R-A", "high", "src/a.rs", 1),
        finding("R-0", "low", "src/m.rs", 3),
    ]
}

fn table() -> Element {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    use_context_provider(|| Signal::new(None::<FindingView>));
    let request = use_signal(|| Option::<String>::None);
    use_context_provider(|| super::super::viewer::ViewerRequest(request));
    let dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
    let preset = use_signal(OrderPreset::default);
    PROBE.with(|p| *p.borrow_mut() = Some(Probe { preset, request }));
    rsx! {
        FindingsTable {
            findings: rows(),
            repos: vec!["me/api".to_string()],
            descriptions: std::collections::HashMap::new(),
            triage_view: TriageState::Unresolved,
            dispositions,
            order_preset: preset,
        }
    }
}

fn key_on_pane(html: &str) -> String {
    let attr = "data-finding-key=\"";
    let i = html.find(attr).expect("the pane is rendered") + attr.len();
    html[i..i + html[i..].find('"').unwrap()].to_string()
}

/// The failure mode: decide the focused row (it leaves the table, so the STORED cursor key is
/// stale), then re-cut. The pane must still show a finding, the one that took its place,
/// with its group open; before the fix every group stayed shut and the pane went blank.
#[tokio::test(flavor = "current_thread")]
async fn deciding_a_row_then_switching_preset_keeps_the_pane_on_the_neighbour() {
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
    let mut vdom = VirtualDom::new(table);
    let mut seen = Listeners::default();
    seen.take(&vdom.rebuild_to_vec());
    settle(&mut vdom, &mut seen, 50).await;
    let probe = PROBE.with(|p| p.borrow().expect("probe"));

    // Aim the cursor at one finding; the pane shows it.
    let target = rows()[2].clone();
    let aimed = super::super::viewer::dom_key(&finding_key(&target));
    vdom.in_runtime(|| {
        let mut request = probe.request;
        request.set(Some(finding_key(&target)));
    });
    settle(&mut vdom, &mut seen, 100).await;
    assert_eq!(key_on_pane(&dioxus_ssr::render(&vdom)), aimed);

    // Arm the pane's keys the way a click into it does, and press C (Confirm).
    let pane = seen.last("focusin");
    send(
        &vdom,
        "focusin",
        pane,
        Box::new(SerializedFocusData::default()),
    );
    settle(&mut vdom, &mut seen, 50).await;
    let press = SerializedKeyboardData::new(
        Key::Character("c".to_string()),
        Code::Unidentified,
        Location::Standard,
        false,
        Modifiers::empty(),
        false,
    );
    // The pane's focus and key handlers are on the same element.
    send(&vdom, "keydown", pane, Box::new(press));
    settle(&mut vdom, &mut seen, 100).await;
    let after = key_on_pane(&dioxus_ssr::render(&vdom));
    assert!(!after.is_empty(), "the cursor moved to a neighbour");
    assert_ne!(after, aimed, "the decided row left the table");

    // Now re-cut. The pane stays on a real finding, the same one.
    vdom.in_runtime(|| {
        let mut p = probe.preset;
        p.set(OrderPreset::ByFile);
    });
    settle(&mut vdom, &mut seen, 150).await;
    let html = dioxus_ssr::render(&vdom);
    assert_eq!(
        key_on_pane(&html),
        after,
        "the pane still shows the neighbour after the re-cut: {html}"
    );
    assert!(!html.contains(&aimed), "and the decided row is gone");
}

// ── L3: a value a newer build wrote ──────────────────────────────────────────────────

#[test]
fn an_unknown_view_setting_in_the_draft_does_not_lose_the_review() {
    let scan = serde_json::json!({
        "files_scanned": 0, "findings": [], "proposed_rules": [], "gated": false
    });
    let key = "me/api\u{0}R-A\u{0}src/a.rs\u{0}9\u{0}snip".to_string();
    let confirmed = serde_json::to_value(Disposition {
        state: TriageState::Confirmed,
        ..Disposition::default()
    })
    .unwrap();
    for (triage_view, order_preset) in [
        ("Archived", "by_rule_then_nothing"),
        ("", ""),
        ("TechDebt", "by_file"),
    ] {
        let draft: OnboardingDraft = serde_json::from_value(serde_json::json!({
            "scan": scan,
            "triage_view": triage_view,
            "order_preset": order_preset,
            "dispositions": { key.clone(): confirmed.clone() },
        }))
        .unwrap_or_else(|e| panic!("{triage_view}/{order_preset}: {e}"));
        assert_eq!(draft.dispositions.len(), 1, "the dispositions survive");
        let known_view = matches!(triage_view, "TechDebt");
        assert_eq!(
            draft.triage_view,
            if known_view {
                TriageState::TechDebt
            } else {
                TriageState::Unresolved
            }
        );
        assert_eq!(
            draft.order_preset,
            if order_preset == "by_file" {
                OrderPreset::ByFile
            } else {
                OrderPreset::Curated
            }
        );
    }
    // A value of an unexpected shape too.
    let draft: OnboardingDraft = serde_json::from_value(serde_json::json!({
        "scan": scan,
        "triage_view": { "future": [1, 2] },
        "order_preset": 7,
    }))
    .expect("a draft with odd view settings still loads");
    assert_eq!(draft.triage_view, TriageState::Unresolved);
    assert_eq!(draft.order_preset, OrderPreset::Curated);
}

// ── L2: a flat list of thousands of rows ─────────────────────────────────────────────

/// Critical first mounts every row into one view (page size 5000). chorale windows the rows
/// it renders by the scroll position, so the DOM holds a viewport's worth, not the list; this
/// measures that on 3,000 rows and records the render time (printed, not asserted: a slow
/// machine must not fail a build).
#[test]
fn a_flat_list_of_3000_rows_renders_a_window_not_the_list() {
    fn big() -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        use_context_provider(|| Signal::new(None::<FindingView>));
        let dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
        let preset = use_signal(|| OrderPreset::CriticalFirst);
        let findings: Vec<FindingView> = (0..3000)
            .map(|n| finding("R-A", "high", &format!("src/f{n:04}.rs"), n + 1))
            .collect();
        rsx! {
            FindingsTable {
                findings,
                repos: vec!["me/api".to_string()],
                descriptions: std::collections::HashMap::new(),
                triage_view: TriageState::Unresolved,
                dispositions,
                order_preset: preset,
            }
        }
    }
    let started = std::time::Instant::now();
    let mut vdom = VirtualDom::new(big);
    vdom.rebuild_in_place();
    let built = started.elapsed();
    let html = dioxus_ssr::render(&vdom);
    let total = started.elapsed();
    let rendered = html.matches("data-chorale-index").count();
    println!("L2: 3000 flat rows: build {built:?}, build+render {total:?}, {rendered} row elements, {} bytes", html.len());
    assert!(rendered > 0, "rows are rendered");
    assert!(
        rendered < 500,
        "the DOM holds a window of the 3000 rows, not all of them: {rendered}"
    );
}
