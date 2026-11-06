//! The `detail/*` gallery states (review UI spec section 12): the site detail alone, under the
//! shell, from the review fixtures and with the app's real stylesheet. Each state is the pane a
//! site opens in every lane, so the page is the pane at the width it has beside the tree.

use camerata_api_types::review_view::SiteRow;
use camerata_ui_core::edits::{
    context_for, reduce, AddDraft, DetailAction, DetailContext, DetailMode, Snippet,
};
use camerata_ui_core::test_support::review_fixtures::{
    fixture, Body, DetailBody, DetailForm, StateFixture,
};
use camerata_ui_core::tree::shell::Lane;
use camerata_ui_core::tree::state::{TreeIndex, TreeState};
use dioxus::prelude::*;

use crate::cockpit::review_shell::ReviewShell;
use crate::cockpit::site_detail::code_bridge::InspectedViewer;
use crate::cockpit::site_detail::{
    DetailBanner, DetailNotice, DetailPanel, SiteDetailEmpty, SiteDetailView,
};
use crate::test_harness;

#[derive(Clone, PartialEq, Props)]
pub(super) struct PageProps {
    pub id: String,
}

/// The tree's view of the site's surroundings: the sub-group it sits in, loaded, so the pickers
/// list real sites and the rule select lists the run's rules.
fn context_of(row: &SiteRow) -> DetailContext {
    let ds = camerata_ui_core::test_support::review_fixtures::dataset_base();
    let index = TreeIndex::new(&ds.tree);
    let node = row.group_path.get(2).cloned().unwrap_or_default();
    let mut state = TreeState::default();
    if let Some(page) = ds.sites_page(&node, None, 500) {
        state.apply_page(&page);
    }
    context_for(&index, &state, &node, row)
}

/// The mode a fixture's form stands for, with enough typed in it to read the page.
pub(super) fn mode_of(form: Option<&DetailForm>, row: &SiteRow, ctx: &DetailContext) -> DetailMode {
    match form {
        None => DetailMode::Viewing,
        Some(DetailForm::Edit(field)) => reduce(
            DetailMode::Viewing,
            &DetailAction::BeginEdit(*field),
            row,
            ctx,
        ),
        Some(DetailForm::MergePicker) => {
            let m = reduce(DetailMode::Viewing, &DetailAction::BeginMerge, row, ctx);
            reduce(m, &DetailAction::Reason(String::new()), row, ctx)
        }
        Some(DetailForm::SplitPicker) => {
            reduce(DetailMode::Viewing, &DetailAction::BeginSplit, row, ctx)
        }
        Some(DetailForm::AddForm) => {
            let mut draft = AddDraft::from_row(row);
            draft.snippet =
                Snippet::Loaded(row.snippet.lines().next().unwrap_or_default().to_string());
            DetailMode::Add(Box::new(draft))
        }
        Some(DetailForm::AddRefused(message)) => {
            let mut draft = AddDraft::from_row(row);
            draft.headline = "Second copy of the key".to_string();
            draft.detail = "The same key is read again in a second file.".to_string();
            draft.reason = "the engine reported only the first copy".to_string();
            draft.snippet =
                Snippet::Loaded(row.snippet.lines().next().unwrap_or_default().to_string());
            DetailMode::Add(Box::new(draft)).with_error(message.clone())
        }
    }
}

/// The undo bar a state shows: `detail/edited-field` follows an edit, five seconds ago or less.
fn panel_of(id: &str, mode: DetailMode, row: &SiteRow) -> DetailPanel {
    let undo = (id == "detail/edited-field").then(|| DetailBanner {
        message: row.edits.iter().rfind(|e| !e.stale).map_or_else(
            || "Edited.".to_string(),
            |e| camerata_ui_core::edits::edit_done_message(e.field),
        ),
        button: "Undo edit",
        seconds_left: 4,
    });
    let notice = (id == "detail/added").then(|| DetailNotice {
        warn: false,
        text: camerata_ui_core::edits::added_message(
            &camerata_ui_core::edits::AddedOutcome::Verified,
        ),
    });
    DetailPanel { mode, undo, notice }
}

#[component]
pub(super) fn DetailPage(props: PageProps) -> Element {
    let Some(StateFixture {
        shell,
        body: Body::Detail(body),
    }) = fixture(&props.id)
    else {
        return rsx! { p { "no detail fixture for {props.id}" } };
    };
    let DetailBody {
        site, form, viewer, ..
    } = *body;
    let viewer_pane = match (viewer, site.as_ref()) {
        (true, Some(row)) => rsx! {
            InspectedViewer {
                row: row.clone(),
                audited_shas: shell.ready().map(|s| s.audited_shas.clone()).unwrap_or_default(),
            }
        },
        _ => rsx! {},
    };
    let pane = match site {
        None => rsx! { SiteDetailEmpty {} },
        Some(row) => {
            let ctx = context_of(&row);
            let mode = mode_of(form.as_ref(), &row, &ctx);
            let panel = panel_of(&props.id, mode, &row);
            let ticked = camerata_ui_core::tree::is_outlier(&row);
            rsx! {
                SiteDetailView {
                    position: "1 of 12",
                    ticked,
                    panel,
                    ctx,
                    row,
                }
            }
        }
    };
    rsx! {
        ReviewShell { status: (&shell).into(), active: Lane::Tree,
            {viewer_pane}
            div { class: "site-detail-page", {pane} }
        }
    }
}

pub(super) fn render(id: &str) -> String {
    test_harness::render_props(DetailPage, PageProps { id: id.to_string() })
}

macro_rules! states {
    ($($name:ident => $id:literal),* $(,)?) => {
        $(fn $name() -> String { render($id) })*
        /// Every state this file renders, with its renderer.
        pub(super) fn states() -> Vec<super::BuiltRenderer> {
            vec![$(($id, $name as fn() -> String)),*]
        }
    };
}

states! {
    empty => "detail/empty",
    plain => "detail/plain",
    escalated => "detail/escalated",
    outlier => "detail/outlier",
    edited_field => "detail/edited-field",
    stale_edit => "detail/stale-edit",
    stale_decision => "detail/stale-decision",
    moved => "detail/moved",
    new_since_decision => "detail/new-since-decision",
    held => "detail/held",
    merged => "detail/merged",
    added => "detail/added",
    ignored_needs_ack => "detail/ignored-needs-ack",
    ignored_acked => "detail/ignored-acked",
    edit_open => "detail/edit-open",
    merge_picker => "detail/merge-picker",
    split_picker => "detail/split-picker",
    add_form => "detail/add-form",
    add_refused => "detail/add-refused",
    possible_duplicate => "detail/possible-duplicate",
    merge_picker_duplicate_first => "detail/merge-picker-duplicate-first",
    over_budget => "detail/over-budget",
    viewer_commit => "detail/viewer-commit",
    thrown_out => "detail/thrown-out",
}

#[cfg(test)]
mod tests;
