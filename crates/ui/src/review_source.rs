//! The one data source the review screens read through (review UI spec section 1, rule 1).
//!
//! A host owns signals and I/O and reads here; a view takes props and never fetches. The real
//! HTTP source comes later (`HttpReviewSource`, reqwest against the BFF base URL); the one here is
//! the fixture source the gallery and the tests use. Reads only so far: the work orders that add
//! writes (decisions, edits, settings) extend the trait with them.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use camerata_api_types::review_view::{
    EngineCheckLane, ExportGateView, QueueView, ReviewSettingsView, ReviewShellView, SitesPage,
    TreeView,
};
use camerata_ui_core::test_support::review_fixtures::{dataset_30k, dataset_base, ReviewDataSet};

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
}

impl FixtureReviewSource {
    pub fn new(data: ReviewDataSet) -> Self {
        Self {
            data,
            failure: None,
        }
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
            data: dataset_base(),
            failure: Some(message.to_string()),
        }
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
        self.read(|d| Ok(d.shell.clone()))
    }

    fn tree(&self) -> SourceFuture<'_, TreeView> {
        self.read(|d| Ok(d.tree.clone()))
    }

    fn sites_page<'a>(
        &'a self,
        node: &'a str,
        cursor: Option<&'a str>,
        limit: usize,
    ) -> SourceFuture<'a, SitesPage> {
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
        self.read(move |d| Ok(d.queue(show_below, below_offset, below_limit)))
    }

    fn engine_check(&self) -> SourceFuture<'_, EngineCheckLane> {
        self.read(|d| Ok(d.engine.clone()))
    }

    fn export_gate(&self) -> SourceFuture<'_, ExportGateView> {
        self.read(|d| Ok(d.gate.clone()))
    }

    fn settings(&self) -> SourceFuture<'_, ReviewSettingsView> {
        self.read(|d| Ok(d.settings.clone()))
    }
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
    async fn a_failing_source_fails_every_read_with_its_message() {
        let src = FixtureReviewSource::failing("the server is down");
        assert_eq!(src.shell().await.unwrap_err().message, "the server is down");
        assert!(src.tree().await.is_err());
        assert!(src.queue(false, 0, 10).await.is_err());
        assert!(src.engine_check().await.is_err());
        assert!(src.export_gate().await.is_err());
        assert!(src.settings().await.is_err());
        assert!(src.sites_page("cat:authorization", None, 10).await.is_err());
    }
}
