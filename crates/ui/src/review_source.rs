//! The one data source the review screens read through (review UI spec section 1, rule 1).
//!
//! A host owns signals and I/O and reads here; a view takes props and never fetches. The real
//! HTTP source comes later (`HttpReviewSource`, reqwest against the BFF base URL); the one here is
//! the fixture source the gallery and the tests use. Reads only: every write is
//! [`crate::review_io::ReviewWrites`], which the fixture source implements too, so one fixture
//! serves a host.

use std::cell::RefCell;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

use camerata_api_types::decisions::{Decision, DecisionRecorded, NewDecisionReq, RevokeResp};
use camerata_api_types::edits::{EditRecorded, NewEditReq};
use camerata_api_types::review::EscalationBudget;
use camerata_api_types::review_view::{
    EngineCheckLane, EngineCheckVerdict, EngineCheckVerdictReq, EscalationBudgetOutcome,
    EscalationBudgetReq, ExportGateView, QueueView, ReviewSettingsView, ReviewShellView,
    SampleSizeResp, SitesPage, TreeView,
};
use camerata_ui_core::test_support::review_fixtures::{dataset_30k, dataset_base, ReviewDataSet};

use crate::review_io::{RecordingWrites, ReviewWrites};

/// Why a read failed, in words a screen can show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSourceError {
    pub message: String,
}

impl fmt::Display for ReviewSourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ReviewSourceError {}

/// A read in flight. Boxed, and not required to be `Send`, so the trait can be used as
/// `&dyn ReviewSource` and a host can hold it in a Dioxus task.
pub type SourceFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ReviewSourceError>> + 'a>>;

/// Everything the review screens read.
pub trait ReviewSource {
    /// The shell: counts, lanes, budget, export gate (spec section 4).
    fn shell(&self) -> SourceFuture<'_, ReviewShellView>;
    /// The tree: nodes, counts and markers, never sites (spec U1).
    fn tree(&self) -> SourceFuture<'_, TreeView>;
    /// One node's sites, at most 500 at a time, resuming after `cursor`.
    fn sites_page<'a>(
        &'a self,
        node: &'a str,
        cursor: Option<&'a str>,
        limit: usize,
    ) -> SourceFuture<'a, SitesPage>;
    /// The escalation queue; `show_below` is a view flag that writes nothing (spec U5).
    fn queue(
        &self,
        show_below: bool,
        below_offset: usize,
        below_limit: usize,
    ) -> SourceFuture<'_, QueueView>;
    /// The Engine Check lane (spec section 7).
    fn engine_check(&self) -> SourceFuture<'_, EngineCheckLane>;
    /// What stops the export (spec section 10).
    fn export_gate(&self) -> SourceFuture<'_, ExportGateView>;
    /// The review settings (spec section 9).
    fn settings(&self) -> SourceFuture<'_, ReviewSettingsView>;
}

/// A source over a seeded fixture data set, or one that fails every read.
pub struct FixtureReviewSource {
    data: ReviewDataSet,
    failure: Option<String>,
    /// The settings as the lane last wrote them: a write shows on the next read, as it does on
    /// the server.
    settings: RefCell<ReviewSettingsView>,
    /// Every call made, by name, in order. A test reads it to see what a screen asked for.
    calls: RefCell<Vec<String>>,
    /// The decision and edit writes, recorded.
    pub recorded: RecordingWrites,
}

impl FixtureReviewSource {
    pub fn new(data: ReviewDataSet) -> Self {
        Self {
            settings: RefCell::new(data.settings.clone()),
            data,
            failure: None,
            calls: RefCell::new(Vec::new()),
            recorded: RecordingWrites::default(),
        }
    }

    /// Every call made so far, by name (`apply_budget`, `settings`, `set_stage_model:fix`).
    pub fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }

    fn log(&self, call: impl Into<String>) {
        self.calls.borrow_mut().push(call.into());
    }

    /// The 600-site working set.
    pub fn base() -> Self {
        Self::new(dataset_base())
    }

    /// The 30,000-site set, generated now.
    pub fn scale_30k() -> Self {
        Self::new(dataset_30k())
    }

    /// A source whose every read fails with `message` (the error states).
    pub fn failing(message: &str) -> Self {
        Self {
            failure: Some(message.to_string()),
            ..Self::new(dataset_base())
        }
    }

    /// A write: refused with the failure message when the source fails, else `f` against the
    /// settings. The call is logged first, so a refused call still shows.
    fn write<'a, T: 'a>(
        &'a self,
        call: impl Into<String>,
        f: impl FnOnce(&mut ReviewSettingsView) -> Result<T, ReviewSourceError>,
    ) -> SourceFuture<'a, T> {
        self.log(call);
        let result = match &self.failure {
            Some(message) => Err(ReviewSourceError {
                message: message.clone(),
            }),
            None => f(&mut self.settings.borrow_mut()),
        };
        Box::pin(std::future::ready(result))
    }

    fn read<'a, T: 'a>(
        &'a self,
        f: impl FnOnce(&'a ReviewDataSet) -> Result<T, ReviewSourceError>,
    ) -> SourceFuture<'a, T> {
        let result = match &self.failure {
            Some(message) => Err(ReviewSourceError {
                message: message.clone(),
            }),
            None => f(&self.data),
        };
        Box::pin(std::future::ready(result))
    }
}

impl ReviewSource for FixtureReviewSource {
    fn shell(&self) -> SourceFuture<'_, ReviewShellView> {
        self.log("shell");
        self.read(|d| Ok(d.shell.clone()))
    }

    fn tree(&self) -> SourceFuture<'_, TreeView> {
        self.log("tree");
        self.read(|d| Ok(d.tree.clone()))
    }

    fn sites_page<'a>(
        &'a self,
        node: &'a str,
        cursor: Option<&'a str>,
        limit: usize,
    ) -> SourceFuture<'a, SitesPage> {
        self.log("sites_page");
        self.read(move |d| {
            d.sites_page(node, cursor, limit)
                .ok_or_else(|| ReviewSourceError {
                    message: format!("no such node: {node}"),
                })
        })
    }

    fn queue(
        &self,
        show_below: bool,
        below_offset: usize,
        below_limit: usize,
    ) -> SourceFuture<'_, QueueView> {
        self.log("queue");
        self.read(move |d| Ok(d.queue(show_below, below_offset, below_limit)))
    }

    fn engine_check(&self) -> SourceFuture<'_, EngineCheckLane> {
        self.log("engine_check");
        self.read(|d| Ok(d.engine.clone()))
    }

    fn export_gate(&self) -> SourceFuture<'_, ExportGateView> {
        self.log("export_gate");
        self.read(|d| Ok(d.gate.clone()))
    }

    fn settings(&self) -> SourceFuture<'_, ReviewSettingsView> {
        self.log("settings");
        let result = match &self.failure {
            Some(message) => Err(ReviewSourceError {
                message: message.clone(),
            }),
            None => Ok(self.settings.borrow().clone()),
        };
        Box::pin(std::future::ready(result))
    }
}

impl ReviewWrites for FixtureReviewSource {
    // The decision and edit writes answer with made-up records and keep a log (see
    // `RecordingWrites`); the settings writes change the settings this source reads back.
    fn post_decision(&self, req: NewDecisionReq) -> SourceFuture<'_, DecisionRecorded> {
        self.log("post_decision");
        self.recorded.post_decision(req)
    }

    fn revoke_decision<'a>(&'a self, decision_id: &'a str) -> SourceFuture<'a, RevokeResp> {
        self.log("revoke_decision");
        self.recorded.revoke_decision(decision_id)
    }

    fn post_edit(&self, req: NewEditReq) -> SourceFuture<'_, EditRecorded> {
        self.log("post_edit");
        self.recorded.post_edit(req)
    }

    fn preview_budget(
        &self,
        req: EscalationBudgetReq,
    ) -> SourceFuture<'_, EscalationBudgetOutcome> {
        self.write("preview_budget", move |s| budget_outcome(s, &req, false))
    }

    fn apply_budget(&self, req: EscalationBudgetReq) -> SourceFuture<'_, EscalationBudgetOutcome> {
        self.write("apply_budget", move |s| {
            let outcome = budget_outcome(s, &req, true)?;
            s.escalation.rate_per_kloc = req.rate_per_kloc;
            s.escalation.floor = req.floor;
            s.escalation.over_budget_reasons = req.over_budget_reasons;
            s.escalation.budget = outcome.after;
            Ok(outcome)
        })
    }

    fn set_sample_size(&self, sample_size_per_run: usize) -> SourceFuture<'_, SampleSizeResp> {
        self.write("set_sample_size", move |s| {
            if sample_size_per_run > s.limits.sample_max {
                return Err(refused(format!(
                    "The sample size must be between 0 and {}.",
                    s.limits.sample_max
                )));
            }
            s.engine_check.sample_size_per_run = sample_size_per_run;
            Ok(SampleSizeResp {
                sample_size_per_run,
                lane_off: sample_size_per_run == 0,
            })
        })
    }

    fn post_engine_verdict<'a>(
        &'a self,
        key: &'a str,
        req: EngineCheckVerdictReq,
    ) -> SourceFuture<'a, EngineCheckVerdict> {
        self.log("post_engine_verdict");
        self.recorded.post_engine_verdict(key, req)
    }

    fn set_needed_me<'a>(
        &'a self,
        decision_id: &'a str,
        needed_me: bool,
    ) -> SourceFuture<'a, Decision> {
        self.log("set_needed_me");
        self.recorded.set_needed_me(decision_id, needed_me)
    }

    fn set_stage_model<'a>(&'a self, step: &'a str, model: &'a str) -> SourceFuture<'a, ()> {
        self.write(format!("set_stage_model:{step}"), move |s| {
            let slot = match step {
                "audit" => &mut s.stage_models.scan,
                "calibration" => &mut s.stage_models.calibration,
                "group_review" => &mut s.stage_models.group_review,
                "fix" => &mut s.stage_models.fix,
                other => return Err(refused(format!("unknown step: {other}"))),
            };
            if model.trim().is_empty() && step != "fix" {
                return Err(refused("model must not be empty".to_string()));
            }
            *slot = model.trim().to_string();
            Ok(())
        })
    }
}

fn refused(message: String) -> ReviewSourceError {
    ReviewSourceError { message }
}

/// What a budget request would do to the fixture's settings: the line before and after, and how
/// many candidates cross it. Refuses what the server refuses (outside the limits). The crossing
/// counts are a stand-in over the fixture's candidate count, deterministic and plausible.
fn budget_outcome(
    settings: &ReviewSettingsView,
    req: &EscalationBudgetReq,
    applied: bool,
) -> Result<EscalationBudgetOutcome, ReviewSourceError> {
    let limits = settings.limits;
    if !req.rate_per_kloc.is_finite()
        || req.rate_per_kloc < 0.0
        || req.rate_per_kloc > limits.rate_max
    {
        return Err(refused(format!(
            "The rate must be between 0 and {} escalations per 1,000 lines.",
            limits.rate_max
        )));
    }
    if req.floor > limits.floor_max {
        return Err(refused(format!(
            "The floor must be between 0 and {}.",
            limits.floor_max
        )));
    }
    let before = settings.escalation.budget;
    let mut after = EscalationBudget::compute(
        req.rate_per_kloc,
        req.floor,
        settings.escalation.counted_lines,
    );
    after.candidates = before.candidates;
    after.escalated = before.candidates.min(after.budget).max(before.over_budget);
    after.over_budget = after.escalated.saturating_sub(after.budget);
    Ok(EscalationBudgetOutcome {
        applied,
        before,
        after,
        moved_above: after.escalated.saturating_sub(before.escalated),
        moved_below: before.escalated.saturating_sub(after.escalated),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_api_types::review_view::SITES_PAGE_SIZE;

    /// The trait can be held as a trait object, which is how a host will hold it.
    fn as_dyn(s: &FixtureReviewSource) -> &dyn ReviewSource {
        s
    }

    #[tokio::test]
    async fn the_fixture_source_serves_every_view_the_screens_read() {
        let src = FixtureReviewSource::base();
        let src = as_dyn(&src);
        let shell = src.shell().await.unwrap();
        let tree = src.tree().await.unwrap();
        assert_eq!(tree.run_id, shell.run_id);
        assert!(!tree.nodes.is_empty());
        let queue = src.queue(false, 0, 100).await.unwrap();
        assert_eq!(queue.escalated.len(), shell.budget.escalated);
        assert_eq!(
            src.engine_check().await.unwrap().cards.len(),
            shell.lanes.engine_check
        );
        assert_eq!(src.export_gate().await.unwrap(), shell.export_gate);
        assert!(src.settings().await.unwrap().limits.rate_max > 0.0);
    }

    #[tokio::test]
    async fn sites_come_a_node_at_a_time_and_an_unknown_node_is_an_error() {
        let src = FixtureReviewSource::base();
        let tree = src.tree().await.unwrap();
        let node = &tree.nodes[0].id;
        let page = src.sites_page(node, None, 10_000).await.unwrap();
        assert!(page.rows.len() <= SITES_PAGE_SIZE);
        assert_eq!(page.node, *node);
        let err = src.sites_page("cat:none", None, 10).await.unwrap_err();
        assert!(err.to_string().contains("no such node"), "{err}");
    }

    #[tokio::test]
    async fn the_thirty_thousand_site_source_pages_a_big_node_five_hundred_at_a_time() {
        let src = FixtureReviewSource::scale_30k();
        let shell = src.shell().await.unwrap();
        assert_eq!(shell.counts.total, 30_000);
        let tree = src.tree().await.unwrap();
        let biggest = tree.nodes.iter().max_by_key(|n| n.counts.sites).unwrap();
        let first = src.sites_page(&biggest.id, None, 500).await.unwrap();
        assert_eq!(first.rows.len(), 500);
        let cursor = first.next_cursor.clone().unwrap();
        let second = src
            .sites_page(&biggest.id, Some(&cursor), 500)
            .await
            .unwrap();
        assert_eq!(second.offset, 500);
        assert_ne!(first.rows[0].site_key, second.rows[0].site_key);
    }

    #[tokio::test]
    async fn the_settings_writes_show_on_the_next_read_and_are_logged_by_name() {
        use camerata_api_types::review_view::EscalationBudgetReq;
        let src = FixtureReviewSource::base();
        let before = src.settings().await.unwrap();
        let req = EscalationBudgetReq {
            rate_per_kloc: 3.0,
            floor: 40,
            over_budget_reasons: Vec::new(),
        };
        let preview = src.preview_budget(req.clone()).await.unwrap();
        assert!(!preview.applied);
        assert_eq!(
            src.settings().await.unwrap(),
            before,
            "a preview writes nothing"
        );
        let applied = src.apply_budget(req).await.unwrap();
        assert!(applied.applied);
        let after = src.settings().await.unwrap();
        assert_eq!(after.escalation.rate_per_kloc, 3.0);
        assert_eq!(after.escalation.floor, 40);
        assert_eq!(after.escalation.budget, applied.after);
        assert!(after.escalation.over_budget_reasons.is_empty());
        src.set_sample_size(0).await.unwrap();
        src.set_stage_model("group_review", "claude-opus-5-5")
            .await
            .unwrap();
        let after = src.settings().await.unwrap();
        assert_eq!(after.engine_check.sample_size_per_run, 0);
        assert_eq!(after.stage_models.group_review, "claude-opus-5-5");
        assert!(src.set_stage_model("nonsense", "x").await.is_err());
        assert!(src.set_sample_size(1001).await.is_err());
        assert_eq!(
            src.calls(),
            [
                "settings",
                "preview_budget",
                "settings",
                "apply_budget",
                "settings",
                "set_sample_size",
                "set_stage_model:group_review",
                "settings",
                "set_stage_model:nonsense",
                "set_sample_size",
            ]
        );
    }

    #[tokio::test]
    async fn a_failing_source_fails_every_read_with_its_message() {
        let src = FixtureReviewSource::failing("the server is down");
        assert_eq!(src.shell().await.unwrap_err().message, "the server is down");
        assert!(src.tree().await.is_err());
        assert!(src.queue(false, 0, 10).await.is_err());
        assert!(src.engine_check().await.is_err());
        assert!(src.export_gate().await.is_err());
        assert!(src.settings().await.is_err());
        assert!(src.set_sample_size(5).await.is_err());
        assert!(src.set_stage_model("fix", "x").await.is_err());
        assert!(src
            .apply_budget(camerata_api_types::review_view::EscalationBudgetReq {
                rate_per_kloc: 1.0,
                floor: 20,
                over_budget_reasons: Vec::new(),
            })
            .await
            .is_err());
        assert!(src.sites_page("cat:authorization", None, 10).await.is_err());
    }
}
