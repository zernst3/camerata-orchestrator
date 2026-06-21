//! Provider-agnostic WorkItem layer for the rebuilt Governed Development surface.
//!
//! This sits ON TOP OF the existing `camerata-worktracker` port (its
//! [`CanonicalStory`] model + the GitHub Issues adapter in `github.rs` /
//! `github_issues.rs`). It introduces the normalized [`WorkItem`] DTO the UI
//! consumes, mapped from a `CanonicalStory` + its source repo, and the endpoints
//! that pull work items, project them onto Units of Work (dedup by external ref),
//! refresh one, and comment back to the source.
//!
//! It REPLACES the inline owner/repo "adopt-issue" hack (`/api/stories/adopt-issue`):
//! instead of the UI naming a repo and a number, the architect pulls ALL open issues
//! across the ACTIVE project's repos, then creates a UoW from a chosen work item. The
//! UoW dev controls (run / clarify / sign-off) reuse the EXISTING governed-dev
//! endpoints, keyed by the UoW's story id — the gate is never bypassed.
//!
//! ## Identity
//!
//! A [`WorkItem`] carries a STABLE id of the form `github:OWNER/REPO#NUMBER`. The UoW
//! layer keys by `story_id`; the bridge between the two is [`work_item_id_to_story_id`]
//! / [`story_id_for`], which strips the `github:` provider prefix so the resulting
//! `story_id` is `OWNER/REPO#NUMBER` — exactly the namespaced id the adopt-issue path
//! (and the canonical story spine) already use. This keeps a UoW created from a work
//! item interoperable with the rest of the spine and makes dedup-by-external-ref a
//! pure string identity on `work_item_id`.
//!
//! ## Token / I/O
//!
//! The pull / refresh / comment paths need a GitHub token (`CAMERATA_GITHUB_TOKEN`).
//! The mapping + id functions are pure (no I/O, no token) so they are unit-testable
//! against fixtures. The HTTP calls reuse `github_issues.rs`, which in turn reuses the
//! worktracker's `ReqwestTransport` (correct User-Agent + auth header in one place).

use camerata_worktracker::CanonicalStory;
use serde::{Deserialize, Serialize};

use crate::github_issues::IssueDetail;

/// The provider-agnostic work-item DTO the UI renders. Normalized from any provider;
/// for now mapped from the worktracker's [`CanonicalStory`] + the GitHub adapter.
///
/// The shared contract (server emits, UI consumes):
/// ```json
/// { "id": "github:OWNER/REPO#123", "provider": "github", "repo": "OWNER/REPO",
///   "number": 123, "title": "...", "body": "...", "state": "open",
///   "url": "https://github.com/OWNER/REPO/issues/123", "labels": ["..."] }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItem {
    /// Stable, provider-namespaced id, e.g. `github:OWNER/REPO#123`.
    pub id: String,
    /// The provider this item came from. `"github"` for now.
    pub provider: String,
    /// The source repo as `OWNER/REPO`.
    pub repo: String,
    /// The provider's per-repo item number (GitHub `#N`).
    pub number: u64,
    /// The item title.
    pub title: String,
    /// The item body (markdown). Empty when the source has none.
    pub body: String,
    /// `"open"` or `"closed"`.
    pub state: String,
    /// A human-navigable URL for the item.
    pub url: String,
    /// The item's labels (provider label names).
    #[serde(default)]
    pub labels: Vec<String>,
}

impl WorkItem {
    /// Build the stable work-item id for a GitHub issue: `github:OWNER/REPO#NUMBER`.
    pub fn github_id(repo: &str, number: u64) -> String {
        format!("github:{repo}#{number}")
    }

    /// Map a GitHub [`IssueDetail`] + its source repo into a normalized work item.
    /// Pure: no I/O, no token. `repo` is `OWNER/REPO`.
    pub fn from_github_issue(repo: &str, issue: &IssueDetail) -> Self {
        Self {
            id: Self::github_id(repo, issue.number),
            provider: "github".to_string(),
            repo: repo.to_string(),
            number: issue.number,
            title: issue.title.clone(),
            body: issue.body.clone(),
            state: issue.state.clone(),
            url: issue.url.clone(),
            labels: issue.labels.clone(),
        }
    }

    /// Map a worktracker [`CanonicalStory`] into a work item, given the source repo.
    ///
    /// The repo is the story's source container (GitHub `owner/repo`), taken from the
    /// story's `external_ref.container` when present, else the `repo` argument. The
    /// number is parsed from the external id; the state is derived from the canonical
    /// status. This is the [`CanonicalStory`] → [`WorkItem`] bridge at the API
    /// boundary (we do NOT do the full 95-ref rename now — see the followup note in
    /// the decision doc).
    pub fn from_canonical_story(story: &CanonicalStory) -> Option<Self> {
        let ext = story.external_ref.as_ref()?;
        let repo = ext.container.clone()?;
        let number: u64 = ext.external_id.parse().ok()?;
        let state = if matches!(
            story.status,
            camerata_worktracker::FeatureStatus::Done
                | camerata_worktracker::FeatureStatus::SignedOff
                | camerata_worktracker::FeatureStatus::Rejected
        ) {
            "closed"
        } else {
            "open"
        };
        Some(Self {
            id: Self::github_id(&repo, number),
            provider: "github".to_string(),
            repo,
            number,
            title: story.title.clone(),
            body: story.description.clone(),
            state: state.to_string(),
            url: ext.url.clone(),
            labels: Vec::new(),
        })
    }
}

/// Convert a work-item id (`github:OWNER/REPO#123`) into the UoW story id
/// (`OWNER/REPO#123`) by stripping the `github:` provider prefix.
///
/// Any other provider prefix (`PROVIDER:rest`) is stripped the same way; an id with no
/// recognized prefix is returned unchanged. This is the single bridge between the
/// work-item identity space and the UoW/story-id space, so dedup and run-control wiring
/// agree on one key.
pub fn work_item_id_to_story_id(work_item_id: &str) -> String {
    // Strip a leading `provider:` segment if present. The repo coordinate contains a
    // `/` but never a `:` before the first `#`, so splitting on the FIRST `:` is safe.
    match work_item_id.split_once(':') {
        Some((_provider, rest)) if !rest.is_empty() => rest.to_string(),
        _ => work_item_id.to_string(),
    }
}

/// The story id a UoW uses for this work item. Alias of [`work_item_id_to_story_id`]
/// kept for call-site readability where the intent is "give me the UoW key".
pub fn story_id_for(work_item_id: &str) -> String {
    work_item_id_to_story_id(work_item_id)
}

// ── tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_worktracker::{ExternalRef, FeatureStatus, Provider};

    fn detail(number: u64, state: &str) -> IssueDetail {
        IssueDetail {
            number,
            title: format!("Issue {number}"),
            body: "Body text.".to_string(),
            url: format!("https://github.com/o/r/issues/{number}"),
            state: state.to_string(),
            labels: vec!["bug".to_string(), "camerata:status:intake".to_string()],
        }
    }

    #[test]
    fn github_id_format() {
        assert_eq!(
            WorkItem::github_id("zernst3/camerata-orchestrator", 20),
            "github:zernst3/camerata-orchestrator#20"
        );
    }

    #[test]
    fn from_github_issue_sets_repo_and_all_fields() {
        let wi = WorkItem::from_github_issue("o/r", &detail(20, "open"));
        assert_eq!(wi.id, "github:o/r#20");
        assert_eq!(wi.provider, "github");
        assert_eq!(wi.repo, "o/r", "the repo must be set on the item (#contract)");
        assert_eq!(wi.number, 20);
        assert_eq!(wi.title, "Issue 20");
        assert_eq!(wi.body, "Body text.");
        assert_eq!(wi.state, "open");
        assert_eq!(wi.url, "https://github.com/o/r/issues/20");
        assert_eq!(wi.labels, vec!["bug", "camerata:status:intake"]);
    }

    #[test]
    fn work_item_id_round_trips_to_story_id() {
        // The bridge strips the provider prefix so the UoW key matches the spine id.
        assert_eq!(work_item_id_to_story_id("github:o/r#20"), "o/r#20");
        assert_eq!(story_id_for("github:o/r#20"), "o/r#20");
        // Unknown / prefix-less ids pass through unchanged.
        assert_eq!(work_item_id_to_story_id("o/r#20"), "o/r#20");
    }

    #[test]
    fn from_canonical_story_bridges_via_external_ref() {
        let story = CanonicalStory {
            id: "o/r#20".to_string(),
            external_ref: Some(
                ExternalRef::new(
                    Provider::GitHub,
                    "20",
                    "https://github.com/o/r/issues/20",
                )
                .with_container("o/r"),
            ),
            title: "T".to_string(),
            description: "D".to_string(),
            status: FeatureStatus::Intake,
            created_by: "x".to_string(),
            targets: vec![],
        };
        let wi = WorkItem::from_canonical_story(&story).expect("maps");
        assert_eq!(wi.id, "github:o/r#20");
        assert_eq!(wi.repo, "o/r");
        assert_eq!(wi.number, 20);
        assert_eq!(wi.state, "open");
        assert_eq!(wi.url, "https://github.com/o/r/issues/20");
    }

    #[test]
    fn from_canonical_story_closed_states_map_to_closed() {
        for status in [
            FeatureStatus::Done,
            FeatureStatus::SignedOff,
            FeatureStatus::Rejected,
        ] {
            let story = CanonicalStory {
                id: "o/r#1".to_string(),
                external_ref: Some(
                    ExternalRef::new(Provider::GitHub, "1", "u").with_container("o/r"),
                ),
                title: "T".to_string(),
                description: String::new(),
                status,
                created_by: "x".to_string(),
                targets: vec![],
            };
            assert_eq!(
                WorkItem::from_canonical_story(&story).unwrap().state,
                "closed",
                "{status:?} must map to closed"
            );
        }
    }

    #[test]
    fn from_canonical_story_needs_container_and_numeric_id() {
        // No external_ref → None.
        let mut story = CanonicalStory {
            id: "native-1".to_string(),
            external_ref: None,
            title: "T".to_string(),
            description: String::new(),
            status: FeatureStatus::Intake,
            created_by: "x".to_string(),
            targets: vec![],
        };
        assert!(WorkItem::from_canonical_story(&story).is_none());

        // external_ref without a container → None (cannot form OWNER/REPO).
        story.external_ref = Some(ExternalRef::new(Provider::GitHub, "20", "u"));
        assert!(WorkItem::from_canonical_story(&story).is_none());

        // Non-numeric external id → None.
        story.external_ref =
            Some(ExternalRef::new(Provider::GitHub, "not-a-number", "u").with_container("o/r"));
        assert!(WorkItem::from_canonical_story(&story).is_none());
    }
}
