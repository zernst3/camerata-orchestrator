//! GitHub Issue intake (issue #20): adopt a repo's open issues — including the
//! ones Camerata's onboarding emitted — into the canonical story spine.
//!
//! This is a DELIBERATELY thin, direct read path, separate from the full
//! `WorkItemProvider` sync machinery in `camerata-worktracker`. The architect's
//! flow here is one-directional and stateless: "show me the open issues on this
//! repo, let me pick one, pull it onto the spine as a `CanonicalStory`." It reuses
//! the worktracker's `ReqwestTransport` for the HTTP call so the User-Agent /
//! auth-header handling stays in one place, but the parse + story-mapping live
//! here because the shape (a flat list for a picker) is intake-specific.
//!
//! Token handling: every read is gated on `CAMERATA_GITHUB_TOKEN`. With NO token
//! the list endpoint returns an empty list with `ok: false` (never an error, never
//! a panic) so the UI degrades to "Connect GitHub to adopt an issue" instead of
//! breaking. The token value is never logged or echoed.

use camerata_worktracker::{
    CanonicalStory, ExternalRef, FeatureStatus, HttpTransport, Provider, RepoCoord,
    ReqwestTransport,
};
use serde::{Deserialize, Serialize};

/// One open GitHub issue, flattened for the adopt picker. Only the fields the UI
/// renders (and the adopt call needs to echo back) are present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IssueSummary {
    /// The issue number (the per-repo id GitHub shows as `#N`).
    pub number: u64,
    /// The issue title.
    pub title: String,
    /// The issue body (markdown). Empty when the issue has none.
    pub body: String,
    /// The human-navigable URL on github.com.
    pub url: String,
}

/// The minimal GitHub issue shape we read from the list endpoint. The issues API
/// also returns pull requests; they carry a `pull_request` member, which we use to
/// filter them out (a PR is not a story to adopt).
#[derive(Debug, Deserialize)]
struct RawIssue {
    number: u64,
    title: String,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    /// Present ONLY on pull requests. Its mere presence marks the row as a PR.
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
}

/// Parse the GitHub issues-list JSON array into `IssueSummary` rows, dropping any
/// row that is actually a pull request. Pure (no I/O) so it is unit-testable
/// against a fixture without a network call or a token.
pub fn parse_open_issues(json: &str) -> anyhow::Result<Vec<IssueSummary>> {
    let raw: Vec<RawIssue> =
        serde_json::from_str(json).map_err(|e| anyhow::anyhow!("parse_open_issues: {e}"))?;
    Ok(raw
        .into_iter()
        .filter(|i| i.pull_request.is_none())
        .map(|i| IssueSummary {
            number: i.number,
            title: i.title,
            body: i.body.unwrap_or_default(),
            url: i.html_url,
        })
        .collect())
}

/// Fetch the OPEN issues for `owner/repo` via the GitHub REST API, authenticated
/// with the supplied token. Returns the parsed summaries on success.
///
/// `repo` must be `owner/name`; anything else is an error (the caller surfaces it
/// as `ok: false`). The transport carries the required `User-Agent`, so this never
/// 403s for the missing-UA reason.
pub async fn list_open_issues(repo: &str, token: &str) -> anyhow::Result<Vec<IssueSummary>> {
    let coord = RepoCoord::parse(repo)
        .ok_or_else(|| anyhow::anyhow!("repo must be `owner/name`, got `{repo}`"))?;
    let transport = ReqwestTransport::new(format!("Bearer {token}"))?;
    // `state=open` (the default, but explicit) + a generous page size. We do not
    // page here: the adopt picker is for a human eyeballing a list, and 100 open
    // issues is already more than anyone scrolls; if a repo has more, the most
    // recent 100 are the relevant ones to adopt.
    let url = format!(
        "https://api.github.com/repos/{}/{}/issues?state=open&per_page=100",
        coord.owner, coord.repo
    );
    let resp = transport.get(&url).await?;
    if !(200..300).contains(&resp.status) {
        anyhow::bail!("GitHub list issues: HTTP {}", resp.status);
    }
    parse_open_issues(&resp.body)
}

/// Build a `CanonicalStory` from an adopted GitHub issue. The canonical id is
/// namespaced by repo (`<owner>/<repo>#<number>`) so adopting issue #20 from two
/// different repos produces two distinct spine rows instead of colliding on the
/// bare number. The `external_ref` points back at the issue (container = repo,
/// external_id = the number, url = the issue page) so status write-back and
/// clarification bridging can find it later. Pure (no I/O), so it is unit-testable.
pub fn issue_to_story(repo: &str, number: u64, title: &str, body: &str) -> CanonicalStory {
    let url = format!("https://github.com/{repo}/issues/{number}");
    CanonicalStory {
        id: format!("{repo}#{number}"),
        external_ref: Some(
            ExternalRef::new(Provider::GitHub, number.to_string(), url).with_container(repo),
        ),
        title: title.to_string(),
        description: body.to_string(),
        // A freshly-adopted issue enters the spine at Intake — it is on the board,
        // awaiting triage/decomposition. (The richer label-driven inference lives in
        // the full worktracker provider; intake keeps it simple and predictable.)
        status: FeatureStatus::Intake,
        created_by: "github-issue-intake".to_string(),
        // Build targets are assigned during decomposition, not derived from the
        // source issue. The source repo lives on external_ref.container.
        targets: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_open_issues_maps_fields_and_skips_prs() {
        // Two issues and one pull request (the PR carries a `pull_request` member).
        let json = r#"[
            {
                "number": 20,
                "title": "Story intake from GitHub Issues",
                "body": "Adopt a repo's issues into the spine.",
                "html_url": "https://github.com/zernst3/camerata-orchestrator/issues/20"
            },
            {
                "number": 21,
                "title": "No body issue",
                "html_url": "https://github.com/zernst3/camerata-orchestrator/issues/21"
            },
            {
                "number": 22,
                "title": "A pull request, not an issue",
                "html_url": "https://github.com/zernst3/camerata-orchestrator/pull/22",
                "pull_request": { "url": "https://api.github.com/.../pulls/22" }
            }
        ]"#;
        let issues = parse_open_issues(json).expect("parse");
        assert_eq!(issues.len(), 2, "the PR row must be filtered out");
        assert_eq!(issues[0].number, 20);
        assert_eq!(issues[0].title, "Story intake from GitHub Issues");
        assert_eq!(issues[0].body, "Adopt a repo's issues into the spine.");
        assert_eq!(
            issues[0].url,
            "https://github.com/zernst3/camerata-orchestrator/issues/20"
        );
        // Missing body deserializes to an empty string, never a panic.
        assert_eq!(issues[1].body, "");
    }

    #[test]
    fn parse_open_issues_rejects_non_array_json() {
        assert!(parse_open_issues("{\"message\":\"Not Found\"}").is_err());
    }

    #[test]
    fn issue_to_story_namespaces_id_and_links_external_ref() {
        let story = issue_to_story("zernst3/camerata-orchestrator", 20, "Title", "Body");
        assert_eq!(story.id, "zernst3/camerata-orchestrator#20");
        assert_eq!(story.title, "Title");
        assert_eq!(story.description, "Body");
        assert_eq!(story.status, FeatureStatus::Intake);
        let r = story.external_ref.expect("external_ref set");
        assert_eq!(r.provider, Provider::GitHub);
        assert_eq!(r.external_id, "20");
        assert_eq!(
            r.container.as_deref(),
            Some("zernst3/camerata-orchestrator")
        );
        assert_eq!(
            r.url,
            "https://github.com/zernst3/camerata-orchestrator/issues/20"
        );
    }
}
