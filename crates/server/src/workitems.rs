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

use camerata_worktracker::{CanonicalStory, ExternalRef, Provider};
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
    /// The parent issue number when this item is a GitHub sub-issue (Epic → child).
    /// Carried through from [`IssueSummary::parent_number`] on the pull path.
    /// `None` for top-level issues, standalone issues, and items fetched via the
    /// single-issue refresh path (which does not carry the parent field).
    #[serde(default)]
    pub parent_number: Option<u64>,
    /// The logins of the users assigned to the item. Empty when unassigned. Populated on
    /// the single-issue refresh path (`from_github_issue`); the bulk pull path and the
    /// canonical-story bridge leave it empty (the fields are not on those sources).
    #[serde(default)]
    pub assignees: Vec<String>,
    /// The item's last-updated ISO-8601 timestamp as the provider returns it. Empty when
    /// absent. The UI's update-poll captures this as the per-UoW last-seen baseline and
    /// flags the item CHANGED when a later poll reports a newer value.
    #[serde(default)]
    pub updated_at: String,
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
            parent_number: None,
            assignees: issue.assignees.clone(),
            updated_at: issue.updated_at.clone(),
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
        let state = state_of(story);
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
            parent_number: None,
            // The canonical story spine does not carry assignees or the updated-at
            // timestamp; they populate on the single-issue refresh path (Pull latest).
            assignees: Vec::new(),
            updated_at: String::new(),
        })
    }
}

impl WorkItem {
    /// Build the stable work-item id for a NATIVE unit: `native:<unit key>`. The id bridge
    /// ([`work_item_id_to_story_id`]) strips the prefix back to the unit key.
    pub fn native_id(story_id: &str) -> String {
        format!("native:{story_id}")
    }

    /// Map a NATIVE unit's spine story (a [`CanonicalStory`] with no tracker link) into a work
    /// item, so the console lists it like any other: provider `native`, the unit's repository
    /// (empty when it names none yet), its native number, and no URL (there is no board).
    pub fn from_native_story(story: &CanonicalStory, repo: Option<&str>, number: u64) -> Self {
        Self {
            id: Self::native_id(&story.id),
            provider: "native".to_string(),
            repo: repo.unwrap_or_default().to_string(),
            number,
            title: story.title.clone(),
            body: story.description.clone(),
            state: state_of(story).to_string(),
            url: String::new(),
            labels: Vec::new(),
            parent_number: None,
            assignees: Vec::new(),
            updated_at: String::new(),
        }
    }
}

/// `"closed"` for a story whose canonical status is terminal, else `"open"`.
fn state_of(story: &CanonicalStory) -> &'static str {
    if matches!(
        story.status,
        camerata_worktracker::FeatureStatus::Done
            | camerata_worktracker::FeatureStatus::SignedOff
            | camerata_worktracker::FeatureStatus::Rejected
    ) {
        "closed"
    } else {
        "open"
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

// ── GitHub issue keys (the adapter's own parse) ─────────────────────────────────

/// Parse a GitHub issue key `OWNER/REPO#NUMBER` into its repository and issue number.
///
/// A unit of work created from a GitHub issue has been keyed by this string since before units
/// carried their own repository and tracker link, so existing records still have it as their
/// key. This adapter is the ONLY place the `#` in such a key is interpreted: everything else
/// reads the unit's own fields ([`crate::uow::UnitOfWork::repo`],
/// [`crate::uow::UnitOfWork::external_ref`]), which the store fills from this parse when it
/// loads or creates a GitHub-keyed unit. `None` when the key is not this shape (a native unit
/// `uow-<token>`, a draft `draft-<token>`, a demo story id).
pub fn parse_github_key(key: &str) -> Option<(String, u64)> {
    let (repo, number) = key.rsplit_once('#')?;
    camerata_worktracker::RepoCoord::parse(repo)?;
    let number: u64 = number.parse().ok()?;
    Some((repo.to_string(), number))
}

/// The tracker link for GitHub issue `number` in `repo` (`OWNER/REPO`): provider GitHub, the
/// issue number as the external id, the repository as the container, and the issue's URL.
pub fn github_issue_ref(repo: &str, number: u64) -> ExternalRef {
    ExternalRef::new(
        Provider::GitHub,
        number.to_string(),
        format!("https://github.com/{repo}/issues/{number}"),
    )
    .with_container(repo)
}

/// The tracker link for a unit key, when the key is a GitHub issue key (see
/// [`parse_github_key`]). Used to give legacy GitHub-keyed units their link.
pub fn github_issue_ref_for_key(key: &str) -> Option<ExternalRef> {
    parse_github_key(key).map(|(repo, number)| github_issue_ref(&repo, number))
}

/// Parse a GitHub work-item id (`github:OWNER/REPO#NUMBER`) into `(repo, number)`, with a
/// reason when the provider is not `github` or the shape is malformed.
pub fn parse_github_work_item_id(work_item_id: &str) -> Result<(String, u64), String> {
    let rest = work_item_id.strip_prefix("github:").ok_or_else(|| {
        format!("work_item_id must be `github:OWNER/REPO#NUMBER`, got `{work_item_id}`")
    })?;
    let (repo, num) = rest
        .rsplit_once('#')
        .ok_or_else(|| format!("work_item_id missing `#NUMBER`: `{work_item_id}`"))?;
    if camerata_worktracker::RepoCoord::parse(repo).is_none() {
        return Err(format!("work_item_id repo is not `owner/repo`: `{repo}`"));
    }
    let number: u64 = num
        .parse()
        .map_err(|_| format!("work_item_id number is not a u64: `{num}`"))?;
    Ok((repo.to_string(), number))
}

// ── tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_worktracker::FeatureStatus;

    fn detail(number: u64, state: &str) -> IssueDetail {
        IssueDetail {
            number,
            title: format!("Issue {number}"),
            body: "Body text.".to_string(),
            url: format!("https://github.com/o/r/issues/{number}"),
            state: state.to_string(),
            labels: vec!["bug".to_string(), "camerata:status:intake".to_string()],
            assignees: vec!["octocat".to_string()],
            updated_at: "2026-07-05T12:00:00Z".to_string(),
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
        assert_eq!(
            wi.repo, "o/r",
            "the repo must be set on the item (#contract)"
        );
        assert_eq!(wi.number, 20);
        assert_eq!(wi.title, "Issue 20");
        assert_eq!(wi.body, "Body text.");
        assert_eq!(wi.state, "open");
        assert_eq!(wi.url, "https://github.com/o/r/issues/20");
        assert_eq!(wi.labels, vec!["bug", "camerata:status:intake"]);
        assert_eq!(
            wi.assignees,
            vec!["octocat"],
            "assignees thread onto the item"
        );
        assert_eq!(
            wi.updated_at, "2026-07-05T12:00:00Z",
            "updated_at threads onto the item"
        );
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
                ExternalRef::new(Provider::GitHub, "20", "https://github.com/o/r/issues/20")
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

    #[test]
    fn parse_github_key_reads_repo_and_number() {
        assert_eq!(
            parse_github_key("acme/api#42"),
            Some(("acme/api".to_string(), 42))
        );
        let link = github_issue_ref_for_key("acme/api#42").unwrap();
        assert_eq!(link.provider, Provider::GitHub);
        assert_eq!(link.external_id, "42");
        assert_eq!(link.container.as_deref(), Some("acme/api"));
        assert_eq!(link.url, "https://github.com/acme/api/issues/42");
    }

    #[test]
    fn parse_github_key_rejects_keys_that_are_not_issue_keys() {
        for key in [
            "uow-18c2f-0",
            "draft-abc",
            "story-42",
            "not-a-story-id",
            "acme/api#",
            "acme/api#x",
            "noslash#3",
            "a/b/c#3",
        ] {
            assert_eq!(parse_github_key(key), None, "{key}");
        }
    }

    #[test]
    fn parse_github_work_item_id_explains_each_malformed_shape() {
        assert_eq!(
            parse_github_work_item_id("github:o/r#20"),
            Ok(("o/r".to_string(), 20))
        );
        assert!(parse_github_work_item_id("jira:X-1")
            .unwrap_err()
            .contains("must be"));
        assert!(parse_github_work_item_id("github:o/r")
            .unwrap_err()
            .contains("missing"));
        assert!(parse_github_work_item_id("github:nope#2")
            .unwrap_err()
            .contains("owner/repo"));
        assert!(parse_github_work_item_id("github:o/r#x")
            .unwrap_err()
            .contains("u64"));
    }

    /// Unit keys are opaque outside this adapter: no source in the workspace splits a key on
    /// `#` to find a repository or an issue number, with any of `split`, `rsplit`, `splitn`,
    /// `rsplitn`, `split_once`, `rsplit_once`, `find` or `rfind`, in code or in tests. Every
    /// other site reads the unit's own fields (`UnitOfWork::repo`, `external_ref`).
    ///
    /// Scans every crate's `src` and `tests` folders except two whose `#` is not a unit key
    /// (`checks`: comment markers in scanned source; `source-check`: URL fragments). This file
    /// is the GitHub adapter and is exempt.
    #[test]
    fn unit_keys_are_split_on_hash_only_in_the_github_adapter() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut files = Vec::new();
        for krate in std::fs::read_dir(&crates).unwrap() {
            let krate = krate.unwrap().path();
            let name = krate.file_name().unwrap().to_string_lossy().into_owned();
            if matches!(name.as_str(), "checks" | "source-check") {
                continue;
            }
            walk(&krate.join("src"), &mut files);
            walk(&krate.join("tests"), &mut files);
        }
        assert!(
            files.len() > 100,
            "the scan must actually see the workspace, saw {} files",
            files.len()
        );
        let mut patterns: Vec<String> = Vec::new();
        for method in [
            "split",
            "rsplit",
            "split_once",
            "rsplit_once",
            "split_terminator",
            "find",
            "rfind",
        ] {
            for quote in ['\'', '"'] {
                patterns.push(format!("{method}({quote}#{quote}"));
            }
        }
        for method in ["splitn", "rsplitn"] {
            for n in 1..=4 {
                for quote in ['\'', '"'] {
                    patterns.push(format!("{method}({n}, {quote}#{quote}"));
                }
            }
        }
        let mut offenders: Vec<String> = Vec::new();
        for file in files
            .iter()
            .filter(|f| !f.ends_with("server/src/workitems.rs"))
        {
            let text = std::fs::read_to_string(file).unwrap();
            for (n, line) in text.lines().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                if patterns.iter().any(|p| line.contains(p.as_str())) {
                    offenders.push(format!("{}:{}", file.display(), n + 1));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "split unit keys on `#` only in workitems.rs; found at: {offenders:#?}"
        );
    }

    #[test]
    fn from_native_story_lists_a_native_unit_with_no_board() {
        let story = CanonicalStory {
            id: "uow-1a-0".to_string(),
            external_ref: None,
            title: "Add rate limiting".to_string(),
            description: "Limit the login endpoint.".to_string(),
            status: FeatureStatus::Intake,
            created_by: "camerata".to_string(),
            targets: vec![],
        };
        let wi = WorkItem::from_native_story(&story, Some("acme/api"), 3);
        assert_eq!(wi.id, "native:uow-1a-0");
        assert_eq!(work_item_id_to_story_id(&wi.id), "uow-1a-0");
        assert_eq!(wi.provider, "native");
        assert_eq!(wi.repo, "acme/api");
        assert_eq!(wi.number, 3);
        assert_eq!(wi.title, "Add rate limiting");
        assert_eq!(wi.state, "open");
        assert!(wi.url.is_empty(), "a native unit has no board URL");
    }
}
