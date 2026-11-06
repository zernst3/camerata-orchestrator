//! Deterministic fixtures for every named state of the review gallery (review UI spec section 12).
//!
//! Everything is seeded: the same seed gives byte-identical data. The 30,000-site set is
//! generated on demand ([`dataset_30k`]) and never stored. The fixtures carry data and the plain
//! selectors a screen needs (which node is open, which card is selected); they invent no view
//! model of their own, so a screen's props are built from `camerata_api_types::review_view`
//! structs only.
//!
//! - [`dataset`]: the seeded generator and the [`ReviewDataSet`] every view is read from.
//! - [`states`]: one [`StateFixture`] per named state, by id (`tree/markers`).

pub mod dataset;
pub mod states;

use camerata_api_types::edits::EditField;
use camerata_api_types::review_view::{
    EngineCheckLane, EscalationBudgetOutcome, EscalationBudgetReq, ExportGateView, QueueView,
    ReviewSettingsView, ReviewShellView, SiteRow, SitesPage, TreeView,
};
use std::collections::BTreeMap;

pub use dataset::{build, markers_for, EngineMode, Params, ReviewDataSet};
pub use states::{fixture, FIXTURE_IDS};

/// The seed every named state is built from.
pub const SEED: u64 = 0x5EED_2026_1103;

/// The base working set: 600 sites, every marker present.
pub fn dataset_base() -> ReviewDataSet {
    build(&Params::base(SEED))
}

/// The scale set: 30,000 sites. Built on demand, never stored.
pub fn dataset_30k() -> ReviewDataSet {
    build(&Params::scale_30k(SEED))
}

/// A view that may still be loading or may have failed.
#[derive(Debug, Clone, PartialEq)]
pub enum Loaded<T> {
    Loading,
    Failed(String),
    Ready(T),
}

impl<T> Loaded<T> {
    pub fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(v) => Some(v),
            _ => None,
        }
    }
}

impl<T: Clone> From<&Loaded<T>> for crate::tree::ViewStatus<T> {
    /// A fixture's view as the status a screen draws.
    fn from(l: &Loaded<T>) -> Self {
        match l {
            Loaded::Loading => Self::Loading,
            Loaded::Failed(m) => Self::Failed(m.clone()),
            Loaded::Ready(v) => Self::Ready(v.clone()),
        }
    }
}

/// Everything a gallery page needs for one named state: the shell above, then the body.
#[derive(Debug, Clone, PartialEq)]
pub struct StateFixture {
    pub shell: Loaded<ReviewShellView>,
    pub body: Body,
}

/// The screen under the shell.
#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    /// The shell alone (the `shell/*` states).
    ShellOnly,
    Tree(TreeBody),
    Queue(QueueBody),
    Engine(EngineBody),
    Detail(Box<DetailBody>),
    Settings(SettingsBody),
    Export(ExportBody),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TreeBody {
    pub tree: Loaded<TreeView>,
    /// Node ids that are open, ancestors included.
    pub open_nodes: Vec<String>,
    /// The first page of sites of each open sub-group, by node id.
    pub pages: BTreeMap<String, SitesPage>,
    pub selected_site: Option<String>,
    /// The node whose batch prompt is open.
    pub prompt_node: Option<String>,
    /// The window is narrow, so the right pane collapses under the table (spec U6).
    pub narrow: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueueBody {
    pub queue: Loaded<QueueView>,
    pub selected_site: Option<String>,
    /// The toast after the rate or floor changed.
    pub rate_toast: Option<EscalationBudgetOutcome>,
    /// A form open in the site detail beside the table.
    pub form: Option<DetailForm>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EngineBody {
    pub lane: Loaded<EngineCheckLane>,
    /// The index into the lane's cards of the card in view.
    pub selected_card: Option<usize>,
    /// The reviewer pressed No and the list of what was wrong is open.
    pub wrong_list_open: bool,
}

/// A form open in the site detail.
#[derive(Debug, Clone, PartialEq)]
pub enum DetailForm {
    Edit(EditField),
    MergePicker,
    SplitPicker,
    AddForm,
    /// The server refused the added finding, with its message.
    AddRefused(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct DetailBody {
    pub site: Option<SiteRow>,
    pub form: Option<DetailForm>,
    /// The other sites of the same sub-group, for the merge and split pickers.
    pub siblings: Vec<SiteRow>,
    /// The code viewer is open on the site, at the commit the run inspected.
    pub viewer: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettingsBody {
    pub view: Loaded<ReviewSettingsView>,
    /// The escalation formula as edited and not yet applied.
    pub draft: Option<EscalationBudgetReq>,
    /// What applying the draft would do, from the preview.
    pub preview: Option<EscalationBudgetOutcome>,
    pub saving: bool,
    pub save_error: Option<String>,
    /// A run is in flight, so the settings cannot change.
    pub run_in_flight: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExportBody {
    pub gate: ExportGateView,
}

#[cfg(test)]
mod tests;
