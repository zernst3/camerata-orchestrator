//! The site detail: what each state draws, what the reviewer's gestures report, and what each
//! write posts. The gallery pages are checked in `gallery/detail_states`.

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use camerata_api_types::decisions::{DecisionState, Scope};
use camerata_api_types::edits::EditField;
use camerata_api_types::review_view::{HoldReason, Marker, SiteRow};
use camerata_ui_core::edits::{
    AckDraft, AddDraft, DetailContext, DetailKey, EditDraft, MergeDraft, Snippet, SplitChoice,
    SplitDraft, SPLIT_NEW_VALUE,
};
use camerata_ui_core::test_support::review_fixtures::dataset_base;
use dioxus::dioxus_core::{ElementId, Mutation, Mutations};
use dioxus::prelude::*;

use super::*;
use crate::style::GLOBAL_CSS;
use crate::test_harness;
use camerata_ui::review_io::{RecordingWrites, Written};

fn rows() -> Vec<SiteRow> {
    dataset_base().rows
}

fn find(pred: impl Fn(&SiteRow) -> bool) -> SiteRow {
    rows().into_iter().find(|r| pred(r)).expect("a fixture row")
}

fn plain() -> SiteRow {
    find(|r| {
        r.markers.is_empty()
            && r.decision.is_none()
            && r.edits.is_empty()
            && r.category.is_some()
            && !r.fix.is_empty()
            && r.stage2.is_some()
    })
}

fn ctx() -> DetailContext {
    DetailContext {
        crumbs: vec![
            "Authorization".into(),
            "SEC-RLS-1".into(),
            "sub 3: service-role in client".into(),
        ],
        rule_ids: vec!["SEC-RLS-1".into(), "SEC-KEY-2".into()],
        subgroups: vec!["sub 1: policies".into(), "sub 2: grants".into()],
        node_decision: Some(DecisionState::Confirmed),
        pool: rows().iter().take(30).map(PoolSite::of).collect(),
    }
}

use camerata_ui_core::edits::PoolSite;

#[derive(Clone, PartialEq, Props)]
struct Case {
    row: SiteRow,
    ctx: DetailContext,
    panel: DetailPanel,
    ticked: bool,
}

#[component]
fn Harness(props: Case) -> Element {
    rsx! {
        SiteDetailView {
            row: props.row.clone(),
            ctx: props.ctx.clone(),
            position: "7 of 12",
            panel: props.panel.clone(),
            ticked: props.ticked,
        }
    }
}

fn page(row: SiteRow, panel: DetailPanel) -> String {
    test_harness::render_props(
        Harness,
        Case {
            row,
            ctx: ctx(),
            panel,
            ticked: false,
        },
    )
    .replace(GLOBAL_CSS, "")
}

fn mode(mode: DetailMode) -> DetailPanel {
    DetailPanel {
        mode,
        ..DetailPanel::default()
    }
}

fn clean(html: &str) {
    assert!(!html.contains('\u{2014}'), "an em dash in the page");
    assert!(!html.contains('\0'), "a raw NUL in the page");
}

// ── What each state draws ─────────────────────────────────────────────────────────────────

#[test]
fn a_plain_site_draws_every_line_of_spec_section_8_with_an_edit_for_each_field() {
    let row = plain();
    let html = page(row.clone(), DetailPanel::default());
    clean(&html);
    for label in [
        "Headline",
        "Severity",
        "Detail",
        "Fix",
        "Fix type",
        "Rule",
        "Kind",
        "Category",
        "Sub-group",
        "Where",
        "Stage 1",
        "Decision",
    ] {
        assert!(
            html.contains(&format!(">{label}</span>")),
            "no {label} line:\n{html}"
        );
    }
    // One edit control per editable field of the site: nine, the node summary being WO10's.
    assert_eq!(html.matches("site-detail-edit").count(), 9, "{html}");
    for field in camerata_ui_core::edits::SITE_FIELDS {
        let title = format!("Edit {}", field_label(field).to_lowercase());
        assert!(html.contains(&title), "no `{title}`");
    }
    // The head: rule, kind, position, and the two doors to the viewer and the stack.
    assert!(html.contains(&row.rule_id));
    assert!(html.contains("7 of 12"));
    assert!(html.contains("site-detail-code") && html.contains("site-detail-stack"));
    // The crumb and the review level.
    assert!(
        html.contains("Authorization &#62; SEC-RLS-1 &#62; sub 3: service-role in client"),
        "{}",
        &html[html.find("site-detail-crumb").unwrap_or(0)..][..300.min(html.len())]
    );
    // The decision buttons keep their labels, and the new ones are there.
    for label in [
        "Confirmed",
        "Ignored",
        "Tech debt",
        "False positive",
        "Merge into",
        "Add finding",
    ] {
        assert!(html.contains(label), "no {label} button");
    }
    // A plain site has no stale, moved, held, client or duplicate lines.
    for absent in [
        "mark-stale",
        "mark-moved",
        "site-detail-held",
        "site-detail-client",
        "Possible duplicate",
    ] {
        assert!(!html.contains(absent), "{absent} on a plain site");
    }
}

#[test]
fn the_pane_with_nothing_focused_says_so() {
    let html = test_harness::render_fragment(|()| rsx! { SiteDetailEmpty {} }, ());
    assert!(
        html.contains("pane-empty") && html.contains("Select a site"),
        "{html}"
    );
}

#[test]
fn an_edited_field_shows_the_mark_and_its_hover() {
    let row = find(|r| r.edits.iter().any(|e| !e.stale));
    let html = page(row.clone(), DetailPanel::default());
    assert!(html.contains("mark-edited"), "{html}");
    let e = row.edits.iter().find(|e| !e.stale).unwrap();
    assert!(
        html.contains("Was: ") && html.contains(&e.reason.replace('\'', "&#39;")),
        "the hover names the old value and the reason"
    );
}

#[test]
fn a_stale_edit_shows_the_current_text_a_mark_and_reapply_and_discard() {
    let row = find(|r| r.edits.iter().any(|e| e.stale));
    let stale = row.edits.iter().find(|e| e.stale).unwrap().clone();
    let html = page(row.clone(), DetailPanel::default());
    clean(&html);
    assert!(html.contains("mark-stale"), "{html}");
    assert!(
        html.contains(&format!(
            "Your edit (stale): {}",
            stale.new.replace('\'', "&#39;")
        )) || html.contains(&format!("Your edit (stale): {}", stale.new)),
        "{html}"
    );
    assert!(html.contains("Reapply") && html.contains("Discard"));
    // The field itself still shows the engine's text; the stale edit applies nothing.
    let value = current_value_of(&row, stale.field);
    assert!(
        html.contains(&value.replace('\'', "&#39;")) || value.is_empty() || html.contains(&value)
    );
}

fn current_value_of(row: &SiteRow, field: EditField) -> String {
    camerata_ui_core::edits::current_value(row, field, &ctx())
}

#[test]
fn a_stale_decision_reads_unresolved_with_what_it_was_and_offers_both_choices() {
    let row = find(|r| r.stale_decision.is_some() && r.decision.is_none());
    let html = page(row, DetailPanel::default());
    clean(&html);
    assert!(html.contains("Unresolved \u{b7} stale: was "), "{html}");
    assert!(html.contains("dec-stale"));
    // Two buttons beside the decision (a row with a stale edit as well has two more).
    assert!(html.matches("site-detail-line-action").count() >= 2);
    assert!(html.contains("Reapply") && html.contains("Discard"));
}

#[test]
fn a_moved_row_says_where_it_was_in_the_head() {
    let row = find(|r| r.moved_from.as_ref().is_some_and(|m| m.line.is_some()));
    let line = row.moved_from.as_ref().and_then(|m| m.line).unwrap();
    let html = page(row, DetailPanel::default());
    assert!(html.contains("mark-moved"), "{html}");
    assert!(html.contains(&format!("moved, was line {line}")), "{html}");
}

#[test]
fn a_new_since_decision_row_says_so_in_the_chip_and_the_decision_line() {
    let row = find(|r| r.new_since_decision.is_some() && r.decision.is_none());
    let html = page(row, DetailPanel::default());
    assert!(html.contains("mark-new"));
    assert!(
        html.contains("new since you confirmed this group on "),
        "{html}"
    );
}

#[test]
fn a_held_row_says_why_in_red() {
    let row = find(|r| r.held == Some(HoldReason::EvidenceUnverified));
    let html = page(row, DetailPanel::default());
    assert!(
        html.contains("Held from export: evidence unverified. Decide it to release."),
        "{html}"
    );
    assert!(html.contains("site-detail-held"));
    assert!(html.contains("mark-held"));
}

#[test]
fn a_merged_row_reads_merged_into_its_target_and_is_still_there() {
    let row = find(|r| {
        r.decision
            .as_ref()
            .is_some_and(|d| matches!(d.state, DecisionState::MergedInto(_)))
            && r.merged_into.is_some()
    });
    let target = row.merged_into.as_ref().unwrap().label.clone();
    let html = page(row.clone(), DetailPanel::default());
    assert!(html.contains(&format!("Merged into {target}")), "{html}");
    assert!(html.contains(&row.headline.replace('\'', "&#39;")) || html.contains(&row.headline));
}

#[test]
fn an_added_row_carries_the_edit_mark() {
    let row = find(|r| r.added_by_reviewer);
    let html = page(row, DetailPanel::default());
    assert!(html.contains("mark-edited"), "{html}");
}

#[test]
fn the_client_line_belongs_to_ignored_rows_only() {
    let all = rows();
    let mut seen_needed = 0;
    let mut seen_acked = 0;
    let mut checked_other = 0;
    for row in all.iter().filter(|r| {
        r.decision
            .as_ref()
            .is_some_and(|d| d.state == DecisionState::Ignored)
    }) {
        let html = page(row.clone(), DetailPanel::default());
        assert!(
            html.contains("site-detail-client"),
            "an Ignored row has a client line"
        );
        let acked = row
            .decision
            .as_ref()
            .and_then(|d| d.confirmed_by_client.as_ref())
            .is_some();
        if acked {
            seen_acked += 1;
            assert!(html.contains("Accepted risk"), "{html}");
            assert!(!html.contains("record acknowledgement"));
        } else {
            seen_needed += 1;
            assert!(html.contains("Needs client confirmation"));
            assert!(html.contains("record acknowledgement"));
        }
        if seen_needed > 3 && seen_acked > 3 {
            break;
        }
    }
    assert!(seen_needed > 0 && seen_acked > 0);
    for row in all
        .iter()
        .filter(|r| {
            r.decision
                .as_ref()
                .is_none_or(|d| d.state != DecisionState::Ignored)
        })
        .take(40)
    {
        checked_other += 1;
        let html = page(row.clone(), DetailPanel::default());
        assert!(!html.contains("site-detail-client"), "{}", row.site_key);
        assert!(!html.contains("record acknowledgement"));
    }
    assert!(checked_other > 0);
}

#[test]
fn client_ack_only_on_ignored_rows() {
    // The button, the form and the post all follow the one judgment in ui-core.
    let ignored = find(|r| {
        r.decision
            .as_ref()
            .is_some_and(|d| d.state == DecisionState::Ignored && d.confirmed_by_client.is_none())
    });
    let confirmed = find(|r| {
        r.decision
            .as_ref()
            .is_some_and(|d| d.state == DecisionState::Confirmed)
    });
    let form = mode(DetailMode::Ack(AckDraft {
        by: String::new(),
        note: String::new(),
        error: None,
    }));
    let html = page(ignored, form.clone());
    assert!(html.contains("site-detail-ack-form"), "{html}");
    assert!(html.contains("Who at the client acknowledged"));
    // A form left open on a row that cannot be acknowledged still draws nothing for it: the
    // lane resets the form when another row is focused, and the commit would refuse anyway.
    let html = page(confirmed.clone(), DetailPanel::default());
    assert!(!html.contains("site-detail-client"));
    assert!(camerata_ui_core::edits::client_ack_request(&confirmed, "x", "").is_err());
}

// ── The forms ─────────────────────────────────────────────────────────────────────────────

#[test]
fn an_open_edit_swaps_the_value_for_its_control_a_required_reason_and_commit() {
    let row = plain();
    for field in camerata_ui_core::edits::SITE_FIELDS {
        let m = camerata_ui_core::edits::reduce(
            DetailMode::Viewing,
            &DetailAction::BeginEdit(field),
            &row,
            &ctx(),
        );
        let html = page(row.clone(), mode(m));
        clean(&html);
        assert!(html.contains(DETAIL_FORM_ID), "{field:?}: {html}");
        if field == EditField::Subgroup {
            assert!(html.contains("Move this site to a sub-group"));
            assert!(html.contains("New sub-group"));
            continue;
        }
        assert!(html.contains("(required)"), "{field:?}");
        assert!(html.contains("Commit"), "{field:?}");
        assert!(html.contains("Cancel"));
        let control = match field {
            EditField::Headline => "<input",
            EditField::Detail | EditField::Fix => "<textarea",
            EditField::RuleId => "<datalist",
            _ => "<select",
        };
        assert!(html.contains(control), "{field:?} needs a {control}");
    }
}

#[test]
fn an_open_edit_starts_on_the_rows_current_value() {
    let row = plain();
    let text_fields = [
        (EditField::Headline, row.headline.clone(), "value=\""),
        (EditField::Detail, row.detail.clone(), "<textarea"),
        (EditField::Fix, row.fix.clone(), "<textarea"),
    ];
    for (field, value, marker) in text_fields {
        let m = camerata_ui_core::edits::reduce(
            DetailMode::Viewing,
            &DetailAction::BeginEdit(field),
            &row,
            &ctx(),
        );
        let html = page(row.clone(), mode(m));
        let form = &html[html.find(DETAIL_FORM_ID).unwrap()..];
        let escaped = value.replace('\'', "&#39;");
        if marker == "<textarea" {
            let area = &form[form.find("<textarea").unwrap()..];
            let inner = &area[area.find('>').unwrap() + 1..];
            assert!(
                inner.starts_with(&escaped),
                "{field:?}: {}",
                &inner[..60.min(inner.len())]
            );
        } else {
            assert!(form.contains(&format!("value=\"{escaped}\"")), "{field:?}");
        }
    }
}

#[test]
fn an_edit_form_shows_its_error_and_the_value_being_typed() {
    let row = plain();
    let draft = EditDraft {
        field: EditField::Headline,
        value: "My better headline".into(),
        reason: String::new(),
        error: Some("An edit needs a reason. Say what was wrong.".into()),
    };
    let html = page(row, mode(DetailMode::Edit(draft)));
    assert!(html.contains("My better headline"));
    assert!(html.contains("An edit needs a reason. Say what was wrong."));
    assert!(html.contains("site-detail-error"));
}

#[test]
fn the_merge_picker_lists_the_possible_duplicate_first_and_marks_the_pick() {
    let mut row = plain();
    let pool = ctx().pool;
    let other = pool.iter().find(|p| p.key != row.site_key).unwrap().clone();
    row.possible_duplicate_of = Some(camerata_api_types::review_view::DuplicateView {
        key: other.key.clone(),
        hint: "same line, different family".into(),
        rule_id: other.rule_id.clone(),
        path: other.path.clone(),
        line: other.line,
    });
    let draft = MergeDraft {
        query: String::new(),
        target: Some(other.key.clone()),
        reason: String::new(),
        error: None,
    };
    let html = page(row, mode(DetailMode::Merge(draft)));
    clean(&html);
    assert!(html.contains("site-detail-merge-form"));
    assert!(html.contains("the row stays in the tree"));
    let list = html.find("site-detail-candidates").unwrap();
    let dup = html[list..]
        .find("<span class=\"mark-dup\">possible duplicate")
        .unwrap();
    let first_row = html[list..].find("<li").unwrap();
    assert!(first_row < dup, "the linked duplicate is a row of the list");
    assert_eq!(
        html[list..].matches("<li").count().min(2),
        2,
        "and the search results follow it"
    );
    assert!(
        html[list..][first_row..].starts_with("<li class=\"site-detail-candidate picked\""),
        "the linked duplicate is listed first and already picked: {}",
        &html[list..][first_row..][..200]
    );
    assert!(html.contains("site-detail-candidate picked"), "{html}");
    assert!(html.contains("Merge \u{2192}"));
    assert!(html.contains(&format!("{}:{}", other.path, other.line)));
}

#[test]
fn the_split_picker_offers_the_rules_sub_groups_and_a_new_one_with_a_label() {
    let row = plain();
    let existing = mode(DetailMode::Split(SplitDraft {
        choice: SplitChoice::Existing("sub 1: policies".into()),
        new_label: String::new(),
        reason: String::new(),
        error: None,
    }));
    let html = page(row.clone(), existing);
    assert!(html.contains("sub 1: policies") && html.contains("sub 2: grants"));
    assert!(html.contains("New sub-group"));
    assert!(!html.contains("Label of the new sub-group"));
    let new = mode(DetailMode::Split(SplitDraft {
        choice: SplitChoice::New,
        new_label: "test fixtures".into(),
        reason: "r".into(),
        error: Some("A new sub-group needs a label.".into()),
    }));
    let html = page(row, new);
    assert!(html.contains("Label of the new sub-group"));
    assert!(html.contains("test fixtures"));
    assert!(html.contains("A new sub-group needs a label."));
}

#[test]
fn the_split_picker_values_are_indexes_and_new_selects_the_label_input() {
    let row = plain();
    let draft = |choice, new_label: &str| {
        mode(DetailMode::Split(SplitDraft {
            choice,
            new_label: new_label.into(),
            reason: String::new(),
            error: None,
        }))
    };
    let html = page(
        row.clone(),
        draft(SplitChoice::Existing("sub 2: grants".into()), ""),
    );
    assert!(html.contains(r#"<option value="0""#), "{html}");
    assert!(html.contains(r#"<option value="1" selected"#), "{html}");
    assert!(
        html.contains(&format!(r#"<option value="{SPLIT_NEW_VALUE}""#)),
        "{html}"
    );
    assert!(!html.contains("Label of the new sub-group"));
    // The select never carries a raw NUL, whichever choice is current.
    let new = page(row, draft(SplitChoice::New, ""));
    assert!(!new.contains('\u{0}'), "a raw NUL in the markup");
    assert!(
        new.contains(&format!(r#"<option value="{SPLIT_NEW_VALUE}" selected"#)),
        "{new}"
    );
    assert!(new.contains("Label of the new sub-group"));
}

#[test]
fn the_add_form_has_every_field_the_snippet_read_only_and_the_gates_reason() {
    let row = plain();
    let mut draft = AddDraft::from_row(&row);
    draft.snippet = Snippet::Loaded("const k = import.meta.env.VITE_KEY;".into());
    draft.error = Some("The quoted code was not found within 3 lines of line 40.".into());
    let html = page(row.clone(), mode(DetailMode::Add(Box::new(draft))));
    clean(&html);
    assert!(html.contains("site-detail-add-form"));
    for label in [
        "Repo", "File", "Line", "Rule", "Kind", "Severity", "Headline", "Detail", "Fix",
    ] {
        assert!(
            html.contains(&format!("<label>{label}</label>")),
            "no {label}"
        );
    }
    assert!(html.contains("Load code"));
    assert!(html.contains("const k = import.meta.env.VITE_KEY;"));
    assert!(html.contains("(read-only)"));
    assert!(html.contains("Why you are adding it (required)"));
    // The gate's reason, in the form.
    assert!(html.contains("The quoted code was not found within 3 lines of line 40."));
    assert!(html.contains(&row.path));
    // Not loaded yet, loading, and failed each say so.
    for (snippet, words) in [
        (Snippet::NotLoaded, "when you press Load code"),
        (Snippet::Loading, "Reading the code at that line"),
        (Snippet::Failed("file not found".into()), "file not found"),
    ] {
        let mut d = AddDraft::from_row(&row);
        d.snippet = snippet;
        let html = page(row.clone(), mode(DetailMode::Add(Box::new(d))));
        assert!(html.contains(words), "{words}");
    }
}

#[test]
fn the_pane_waits_for_a_field_letter_and_says_which_ones() {
    let html = page(plain(), mode(DetailMode::PickField));
    assert!(html.contains("Edit which field?"), "{html}");
    assert!(html.contains("S severity") && html.contains("U sub-group"));
}

#[test]
fn the_undo_bar_and_the_notice_are_drawn_above_the_fields() {
    let panel = DetailPanel {
        mode: DetailMode::Viewing,
        undo: Some(DetailBanner {
            message: "Headline edited.".into(),
            button: "Undo edit",
            seconds_left: 4,
        }),
        notice: Some(DetailNotice {
            warn: true,
            text: "Finding added, but the evidence check held it: not found.".into(),
        }),
    };
    let html = page(plain(), panel);
    clean(&html);
    assert!(html.contains("Headline edited.") && html.contains("Undo edit") && html.contains("4s"));
    assert!(html.contains("site-detail-notice warn"));
    let bar = html.find("site-detail-undo").unwrap();
    let headline = html.find("site-detail-edit").unwrap();
    assert!(bar < headline, "the bar sits above the first field");
}

#[test]
fn an_outlier_offers_the_batch_tick_and_a_checked_box_when_ticked() {
    let row = find(camerata_ui_core::tree::is_outlier);
    let html = test_harness::render_props(
        Harness,
        Case {
            row,
            ctx: ctx(),
            panel: DetailPanel::default(),
            ticked: true,
        },
    );
    assert!(html.contains("Include this outlier in the next batch decision"));
    assert!(html.contains("checked"));
}

#[test]
fn every_row_of_the_fixture_renders_with_no_dash_and_no_nul() {
    for row in rows().iter().step_by(7) {
        clean(&page(row.clone(), DetailPanel::default()));
    }
}

#[test]
fn nothing_the_server_returned_about_a_site_is_dropped() {
    let row = find(|r| !r.flagged_by.is_empty());
    let html = page(row.clone(), DetailPanel::default());
    for f in &row.flagged_by {
        assert!(
            html.contains(&format!("({}:{})", f.path, f.line)),
            "flagged by"
        );
    }
    assert!(html.contains(&row.repo));
    let staged = find(|r| {
        r.stage2
            .as_ref()
            .is_some_and(|s| s.outlier_reason.is_some())
    });
    assert!(page(staged, DetailPanel::default()).contains("Stage 2"));
    let thrown = find(|r| r.thrown_out_reason.is_some());
    let html = page(thrown.clone(), DetailPanel::default());
    assert!(
        html.contains(&thrown.thrown_out_reason.unwrap().replace('\'', "&#39;"))
            || html.contains("Thrown out")
    );
    let mut flagged = plain();
    flagged.review_flags = vec!["a count the data contradicts".to_string()];
    let html = page(flagged, DetailPanel::default());
    assert!(html.contains("Wording flags") && html.contains("a count the data contradicts"));
}

// ── The reviewer's gestures, with real events ─────────────────────────────────────────────

thread_local! {
    static EVENTS: RefCell<Vec<DetailEvent>> = const { RefCell::new(Vec::new()) };
}

fn recording_harness() -> Element {
    let row = plain();
    rsx! {
        SiteDetailView {
            row,
            ctx: ctx(),
            position: "1 of 1",
            panel: DetailPanel::default(),
            ticked: false,
            on_event: Callback::new(|e: DetailEvent| EVENTS.with(|l| l.borrow_mut().push(e))),
        }
    }
}

fn recording_harness_with_form() -> Element {
    let row = plain();
    let m = camerata_ui_core::edits::reduce(
        DetailMode::Viewing,
        &DetailAction::BeginEdit(EditField::Headline),
        &row,
        &ctx(),
    );
    rsx! {
        SiteDetailView {
            row,
            ctx: ctx(),
            position: "1 of 1",
            panel: mode(m),
            ticked: false,
            on_event: Callback::new(|e: DetailEvent| EVENTS.with(|l| l.borrow_mut().push(e))),
        }
    }
}

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
    fn nth(&self, name: &str, n: usize) -> ElementId {
        self.0
            .iter()
            .filter(|(l, _)| l == name)
            .nth(n)
            .map(|(_, id)| *id)
            .unwrap_or_else(|| panic!("no {name} listener #{n} among {}", self.0.len()))
    }
}

fn send(vdom: &VirtualDom, name: &str, id: ElementId, data: Box<dyn Any>) {
    let platform: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(data));
    vdom.runtime()
        .handle_event(name, dioxus::dioxus_core::Event::new(platform, true), id);
}

fn click(vdom: &VirtualDom, id: ElementId) {
    send(
        vdom,
        "click",
        id,
        Box::new(dioxus::html::SerializedMouseData::default()),
    );
}

async fn settle(vdom: &mut VirtualDom, seen: &mut Listeners) {
    for _ in 0..4 {
        let _ = tokio::time::timeout(Duration::from_millis(10), vdom.wait_for_work()).await;
        seen.take(&vdom.render_immediate_to_vec());
    }
}

#[tokio::test]
async fn o_and_b_open_the_viewer_and_stack_from_the_detail() {
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
    let mut vdom = VirtualDom::new(recording_harness);
    let mut seen = Listeners::default();
    seen.take(&vdom.rebuild_to_vec());
    settle(&mut vdom, &mut seen).await;
    EVENTS.with(|l| l.borrow_mut().clear());
    // Every button of the pane: the Code and Stack doors report the viewer and the stack.
    let clicks = seen.0.iter().filter(|(n, _)| n == "click").count();
    for n in 0..clicks {
        click(&vdom, seen.nth("click", n));
    }
    let events = EVENTS.with(|l| l.borrow().clone());
    assert_eq!(
        events
            .iter()
            .filter(|e| **e == DetailEvent::OpenCode)
            .count(),
        1,
        "the Code button reports the viewer to the lane: {events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| **e == DetailEvent::OpenStack)
            .count(),
        1,
        "the Stack button reports the stacked view to the lane: {events:?}"
    );
    // The keys that reach them are the lane's: O and B on a site resolve to the same commands.
    use camerata_ui_core::tree::keys::{tree_command, CursorKind, TreeCommand};
    use camerata_ui_core::triage::{KeyMods, PaneFocus};
    for (key, want) in [("o", TreeCommand::OpenCode), ("b", TreeCommand::OpenStack)] {
        assert_eq!(
            tree_command(PaneFocus::Cursor, key, KeyMods::none(), CursorKind::Site),
            Some(want)
        );
    }
}

#[tokio::test]
async fn an_edit_button_opens_that_fields_form_and_enter_commits_while_esc_cancels() {
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
    let mut vdom = VirtualDom::new(recording_harness);
    let mut seen = Listeners::default();
    seen.take(&vdom.rebuild_to_vec());
    settle(&mut vdom, &mut seen).await;
    EVENTS.with(|l| l.borrow_mut().clear());
    // Click every button: the doors, the nine edit buttons, the decisions, merge and add.
    let clicks = seen.0.iter().filter(|(n, _)| n == "click").count();
    for n in 0..clicks {
        click(&vdom, seen.nth("click", n));
    }
    let events = EVENTS.with(|l| l.borrow().clone());
    for field in camerata_ui_core::edits::SITE_FIELDS {
        assert!(
            events.contains(&DetailEvent::Form(DetailAction::BeginEdit(field))),
            "no edit button for {field:?}: {events:?}"
        );
    }
    for want in [
        DetailEvent::OpenCode,
        DetailEvent::OpenStack,
        DetailEvent::Form(DetailAction::BeginMerge),
        DetailEvent::Form(DetailAction::BeginAdd),
        DetailEvent::Decide(TriageState::Confirmed),
        DetailEvent::Decide(TriageState::Ignored),
        DetailEvent::Decide(TriageState::TechDebt),
        DetailEvent::Decide(TriageState::FalsePositive),
    ] {
        assert!(events.contains(&want), "no {want:?}: {events:?}");
    }

    // With the form open: Enter in the field commits, Esc cancels.
    let mut vdom = VirtualDom::new(recording_harness_with_form);
    let mut seen = Listeners::default();
    seen.take(&vdom.rebuild_to_vec());
    settle(&mut vdom, &mut seen).await;
    EVENTS.with(|l| l.borrow_mut().clear());
    let key = |pressed: dioxus::html::Key| {
        Box::new(dioxus::html::SerializedKeyboardData::new(
            pressed,
            dioxus::html::Code::Unidentified,
            dioxus::html::Location::Standard,
            false,
            dioxus::html::Modifiers::empty(),
            false,
        )) as Box<dyn Any>
    };
    // The first keydown listener is the value field (an input), the second the reason.
    send(&vdom, "keydown", seen.nth("keydown", 0), key(Key::Enter));
    send(&vdom, "keydown", seen.nth("keydown", 0), key(Key::Escape));
    send(&vdom, "keydown", seen.nth("keydown", 1), key(Key::Enter));
    assert_eq!(
        EVENTS.with(|l| l.borrow().clone()),
        vec![
            DetailEvent::Commit,
            DetailEvent::Cancel,
            DetailEvent::Commit
        ]
    );
    // A letter typed into the field is the field's: it reports nothing.
    EVENTS.with(|l| l.borrow_mut().clear());
    send(
        &vdom,
        "keydown",
        seen.nth("keydown", 0),
        key(Key::Character("c".into())),
    );
    assert!(EVENTS.with(|l| l.borrow().is_empty()));
}

// ── The writes ────────────────────────────────────────────────────────────────────────────

fn fill_edit(row: &SiteRow, reason: &str) -> DetailMode {
    let m = camerata_ui_core::edits::reduce(
        DetailMode::Viewing,
        &DetailAction::BeginEdit(EditField::Headline),
        row,
        &ctx(),
    );
    let m = camerata_ui_core::edits::reduce(
        m,
        &DetailAction::Text("A clearer headline".into()),
        row,
        &ctx(),
    );
    camerata_ui_core::edits::reduce(m, &DetailAction::Reason(reason.into()), row, &ctx())
}

#[tokio::test]
async fn edit_requires_a_reason_and_posts_to_the_edits_route() {
    let row = plain();
    let writes = RecordingWrites::default();
    // No reason: nothing is built, so nothing is posted.
    let refused = commit(&fill_edit(&row, "  "), &row, &ctx(), "insp-1");
    assert_eq!(
        refused,
        Err("An edit needs a reason. Say what was wrong.".to_string())
    );
    assert!(writes.log.borrow().is_empty());
    // With one: one post to the edits route, keyed by the site, with the field and the reason.
    let write = commit(
        &fill_edit(&row, "it named the wrong table"),
        &row,
        &ctx(),
        "insp-1",
    )
    .unwrap();
    let posted = run_write(&writes, write.clone()).await.unwrap();
    let log = writes.log.borrow();
    assert_eq!(log.len(), 1);
    let Written::Edit(req) = &log[0] else {
        panic!("not an edit: {:?}", log[0]);
    };
    assert_eq!(req.key, row.site_key);
    assert_eq!(req.field, EditField::Headline);
    assert_eq!(req.new, "A clearer headline");
    assert_eq!(req.reason, "it named the wrong table");
    assert_eq!(req.run_id, "insp-1");
    // The undo bar offers "Undo edit" for the edit it made, and the undo posts a discard.
    let outcome = after_write(&write, &posted);
    let (action, message) = outcome.undo.clone().expect("an undo bar");
    assert_eq!(message, "Headline edited.");
    let Posted::Edit(edit) = posted else {
        panic!("an edit");
    };
    assert_eq!(action, UndoAction::DiscardEdit(edit.id));
    let stamped = outcome.stamp(1_000).unwrap();
    assert_eq!(stamped.button(), "Undo edit");
    assert_eq!(stamped.seconds_left(1_000), Some(5));
}

#[tokio::test]
async fn a_refused_edit_comes_back_in_the_servers_words_and_the_form_keeps_what_was_typed() {
    let row = plain();
    let writes = RecordingWrites::default();
    *writes.refuse.borrow_mut() = Some("The edit file is read only.".to_string());
    let write = commit(&fill_edit(&row, "why"), &row, &ctx(), "insp-1").unwrap();
    let err = run_write(&writes, write).await.unwrap_err();
    assert_eq!(err, "The edit file is read only.");
    let shown = fill_edit(&row, "why").with_error(err);
    let html = page(row, mode(shown));
    assert!(html.contains("The edit file is read only."));
    assert!(html.contains("A clearer headline"));
}

#[tokio::test]
async fn stale_reapply_posts_a_new_record_and_discard_revokes() {
    // A stale decision.
    let row = find(|r| {
        r.stale_decision
            .as_ref()
            .is_some_and(|d| !matches!(d.state, DecisionState::MergedInto(_)))
            && r.decision.is_none()
    });
    let stale = row.stale_decision.clone().unwrap();
    let writes = RecordingWrites::default();
    let outcome = reapply_stale_decision(&writes, &row, "insp-2")
        .await
        .unwrap();
    {
        let log = writes.log.borrow();
        let Written::Decision(req) = &log[0] else {
            panic!("a new decision record: {:?}", log[0]);
        };
        assert_eq!(req.scope, Scope::Site);
        assert_eq!(req.key, row.site_key);
        assert_eq!(req.state, stale.state);
        assert_eq!(req.reason, stale.reason);
        assert_eq!(req.run_id, "insp-2");
        assert_eq!(log.len(), 1, "one new record, nothing revoked");
    }
    let (action, message) = outcome.undo.unwrap();
    assert!(matches!(action, UndoAction::RevokeDecision(_)));
    assert!(message.ends_with("reapplied."), "{message}");

    // Discard revokes the OLD record by its id and applies nothing.
    let writes = RecordingWrites::default();
    let outcome = discard_stale_decision(&writes, &row).await.unwrap();
    assert_eq!(
        *writes.log.borrow(),
        vec![Written::Revoke(stale.id.clone())]
    );
    assert!(outcome.undo.is_none());
    assert!(outcome.notice.unwrap().text.contains("stays Unresolved"));

    // A stale merge cannot be reapplied: its target belongs to the earlier run.
    let mut merge = row.clone();
    if let Some(d) = merge.stale_decision.as_mut() {
        d.state = DecisionState::MergedInto("old".into());
    }
    let writes = RecordingWrites::default();
    let err = reapply_stale_decision(&writes, &merge, "insp-2")
        .await
        .unwrap_err();
    assert!(err.contains("stale merge"), "{err}");
    assert!(writes.log.borrow().is_empty());

    // A stale edit: reapply asks the server for a new edit; discard discards it.
    let writes = RecordingWrites::default();
    let outcome = reapply_stale_edit(&writes, "edit-000004").await.unwrap();
    assert_eq!(
        *writes.log.borrow(),
        vec![Written::ReapplyEdit("edit-000004".into())]
    );
    assert!(matches!(
        outcome.undo,
        Some((UndoAction::DiscardEdit(_), _))
    ));
    let writes = RecordingWrites::default();
    discard_stale_edit(&writes, "edit-000004").await.unwrap();
    assert_eq!(
        *writes.log.borrow(),
        vec![Written::DiscardEdit("edit-000004".into())]
    );
    // And discarding a decision that is not stale is refused in words.
    let err = discard_stale_decision(&writes, &plain()).await.unwrap_err();
    assert!(err.contains("no stale decision"));
}

#[tokio::test]
async fn merge_keeps_the_row() {
    let row = plain();
    let target = ctx()
        .pool
        .into_iter()
        .find(|p| p.key != row.site_key)
        .unwrap();
    let mut context = ctx();
    context.pool.push(PoolSite::of(&row));
    let m = camerata_ui_core::edits::reduce(
        DetailMode::Viewing,
        &DetailAction::BeginMerge,
        &row,
        &context,
    );
    let m =
        camerata_ui_core::edits::reduce(m, &DetailAction::Pick(target.key.clone()), &row, &context);
    let m = camerata_ui_core::edits::reduce(
        m,
        &DetailAction::Reason("same helper".into()),
        &row,
        &context,
    );
    let write = commit(&m, &row, &context, "insp-1").unwrap();
    let writes = RecordingWrites::default();
    let posted = run_write(&writes, write.clone()).await.unwrap();
    // A decision on the site, MergedInto the target. Not an edit and not a removal.
    let log = writes.log.borrow();
    assert_eq!(log.len(), 1);
    let Written::Decision(req) = &log[0] else {
        panic!("a decision: {:?}", log[0]);
    };
    assert_eq!(req.key, row.site_key);
    assert_eq!(req.state, DecisionState::MergedInto(target.key.clone()));
    assert_eq!(req.reason, "same helper");
    let outcome = after_write(&write, &posted);
    let (action, message) = outcome.undo.unwrap();
    assert!(matches!(action, UndoAction::RevokeDecision(_)));
    assert!(message.contains("The row stays in the tree."), "{message}");
}

#[tokio::test]
async fn add_refused_shows_the_gates_reason() {
    let row = plain();
    let mut draft = AddDraft::from_row(&row);
    draft.headline = "Key in a public variable".into();
    draft.detail = "It ships to the browser.".into();
    draft.reason = "missed".into();
    draft.snippet = Snippet::Loaded("const k = 1;".into());
    let write = commit(&DetailMode::Add(Box::new(draft.clone())), &row, &ctx(), "r").unwrap();

    // The server refuses the form (an incomplete finding): its words land in the form.
    let writes = RecordingWrites::default();
    *writes.refuse.borrow_mut() = Some("a finding the reviewer adds needs a rule id".into());
    let err = run_write(&writes, write.clone()).await.unwrap_err();
    let shown = DetailMode::Add(Box::new(draft.clone())).with_error(err);
    let html = page(row.clone(), mode(shown));
    assert!(
        html.contains("a finding the reviewer adds needs a rule id"),
        "{html}"
    );

    // The evidence check did not find the code at that line: the row is in and held, and the
    // pane says why in the check's own words.
    let held = Posted::Added(FindingAdded {
        key: "k".into(),
        evidence_verified: false,
        evidence_note: "The quoted code was not found within 3 lines of line 40.".into(),
    });
    let outcome = after_write(&write, &held);
    let notice = outcome.notice.expect("a notice");
    assert!(notice.warn);
    assert!(
        notice
            .text
            .contains("was not found within 3 lines of line 40."),
        "{}",
        notice.text
    );
    assert!(notice.text.contains("until you decide it"));
    let html = page(
        row.clone(),
        DetailPanel {
            notice: Some(notice),
            ..DetailPanel::default()
        },
    );
    assert!(html.contains("was not found within 3 lines of line 40."));
    assert!(html.contains("site-detail-notice warn"));

    // Found: a plain confirmation, no warning.
    let ok = Posted::Added(FindingAdded {
        key: "k".into(),
        evidence_verified: true,
        evidence_note: String::new(),
    });
    assert!(!after_write(&write, &ok).notice.unwrap().warn);
    // An accepted add reached the add route with the form's fields.
    let writes = RecordingWrites::default();
    run_write(&writes, write).await.unwrap();
    let Written::AddFinding(req) = &writes.log.borrow()[0] else {
        panic!("an add");
    };
    assert_eq!(req.path, row.path);
    assert_eq!(req.snippet, "const k = 1;");
}

#[tokio::test]
async fn the_clients_acknowledgement_posts_on_the_ignored_decision() {
    let row = find(|r| {
        r.decision
            .as_ref()
            .is_some_and(|d| d.state == DecisionState::Ignored && d.confirmed_by_client.is_none())
    });
    let decision_id = row.decision.as_ref().unwrap().id.clone();
    let m =
        camerata_ui_core::edits::reduce(DetailMode::Viewing, &DetailAction::BeginAck, &row, &ctx());
    let m = camerata_ui_core::edits::reduce(
        m,
        &DetailAction::Text("cto@acme.test".into()),
        &row,
        &ctx(),
    );
    let write = commit(&m, &row, &ctx(), "r").unwrap();
    let writes = RecordingWrites::default();
    let posted = run_write(&writes, write.clone()).await.unwrap();
    assert!(matches!(
        &writes.log.borrow()[0],
        Written::ClientAck(id, req) if *id == decision_id && req.by == "cto@acme.test"
    ));
    assert_eq!(posted, Posted::Acked);
    assert!(after_write(&write, &posted)
        .notice
        .unwrap()
        .text
        .contains("acknowledgement"));
}

#[tokio::test]
async fn a_split_posts_a_sub_group_edit_and_the_undo_names_where_the_site_went() {
    let row = plain();
    let m = camerata_ui_core::edits::reduce(
        DetailMode::Viewing,
        &DetailAction::BeginSplit,
        &row,
        &ctx(),
    );
    let m =
        camerata_ui_core::edits::reduce(m, &DetailAction::Split(SplitChoice::New), &row, &ctx());
    let m = camerata_ui_core::edits::reduce(
        m,
        &DetailAction::Text("test fixtures".into()),
        &row,
        &ctx(),
    );
    let m = camerata_ui_core::edits::reduce(
        m,
        &DetailAction::Reason("different cause".into()),
        &row,
        &ctx(),
    );
    let write = commit(&m, &row, &ctx(), "insp-1").unwrap();
    let writes = RecordingWrites::default();
    let posted = run_write(&writes, write.clone()).await.unwrap();
    let Written::Edit(req) = &writes.log.borrow()[0] else {
        panic!("an edit");
    };
    assert_eq!(req.field, EditField::Subgroup);
    assert_eq!(req.new, "test fixtures");
    let (_, message) = after_write(&write, &posted).undo.unwrap();
    assert!(
        message.contains("\u{201c}test fixtures\u{201d}"),
        "{message}"
    );
}

#[test]
fn the_chord_and_the_forms_follow_the_pure_reducer() {
    // The keys the tree forwards open the same forms the buttons open.
    let row = plain();
    for (key, want) in [
        (DetailKey::Merge, "Merge"),
        (DetailKey::Add, "Add"),
        (DetailKey::Split, "Split"),
    ] {
        let action = match key {
            DetailKey::Merge => DetailAction::BeginMerge,
            DetailKey::Add => DetailAction::BeginAdd,
            DetailKey::Split => DetailAction::BeginSplit,
            DetailKey::Edit => DetailAction::PickField,
        };
        let m = camerata_ui_core::edits::reduce(DetailMode::Viewing, &action, &row, &ctx());
        assert!(format!("{m:?}").starts_with(want), "{key:?}: {m:?}");
    }
    let _ = Marker::Edited;
}
