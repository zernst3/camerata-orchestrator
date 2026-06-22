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

/// Parse a SINGLE GitHub issue JSON object into an [`IssueSummary`] (the same flat
/// shape the list endpoint produces). Pure (no I/O), so it is unit-testable against a
/// fixture without a network call or a token. Returns an error if the row is actually
/// a pull request (a PR is not a story to refresh).
pub fn parse_single_issue(json: &str) -> anyhow::Result<IssueSummary> {
    let raw: RawIssue =
        serde_json::from_str(json).map_err(|e| anyhow::anyhow!("parse_single_issue: {e}"))?;
    if raw.pull_request.is_some() {
        anyhow::bail!("expected an issue but got a pull request (#{})", raw.number);
    }
    Ok(IssueSummary {
        number: raw.number,
        title: raw.title,
        body: raw.body.unwrap_or_default(),
        url: raw.html_url,
    })
}

/// The raw single-issue shape carries the open/closed `state` (the list adopt path
/// does not need it, but the WorkItem layer does). Read alongside [`parse_single_issue`]
/// when the caller needs the state too.
#[derive(Debug, Deserialize)]
struct RawIssueWithState {
    number: u64,
    title: String,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    state: String,
    #[serde(default)]
    labels: Vec<RawLabel>,
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
}

/// Minimal label shape (only the name is read).
#[derive(Debug, Deserialize)]
struct RawLabel {
    name: String,
}

/// A single open/closed GitHub issue with its `state` and label names, parsed from a
/// single-issue JSON object. The richer shape the WorkItem layer maps from. Pure.
#[derive(Debug, Clone, PartialEq)]
pub struct IssueDetail {
    /// The issue number.
    pub number: u64,
    /// The issue title.
    pub title: String,
    /// The issue body (markdown). Empty when the issue has none.
    pub body: String,
    /// The human-navigable URL on github.com.
    pub url: String,
    /// `"open"` or `"closed"`.
    pub state: String,
    /// The label names on the issue.
    pub labels: Vec<String>,
}

/// Parse a single GitHub issue JSON object into an [`IssueDetail`] (carrying state +
/// labels). Errors if the row is actually a pull request. Pure (no I/O).
pub fn parse_issue_detail(json: &str) -> anyhow::Result<IssueDetail> {
    let raw: RawIssueWithState =
        serde_json::from_str(json).map_err(|e| anyhow::anyhow!("parse_issue_detail: {e}"))?;
    if raw.pull_request.is_some() {
        anyhow::bail!("expected an issue but got a pull request (#{})", raw.number);
    }
    Ok(IssueDetail {
        number: raw.number,
        title: raw.title,
        body: raw.body.unwrap_or_default(),
        url: raw.html_url,
        state: raw.state,
        labels: raw.labels.into_iter().map(|l| l.name).collect(),
    })
}

/// Fetch ONE issue (`owner/repo#number`) via the GitHub REST API, returning the
/// detail shape (state + labels included). Used by the WorkItem refresh path to
/// re-pull a single item. `repo` must be `owner/name`.
pub async fn get_issue_detail(repo: &str, number: u64, token: &str) -> anyhow::Result<IssueDetail> {
    let coord = RepoCoord::parse(repo)
        .ok_or_else(|| anyhow::anyhow!("repo must be `owner/name`, got `{repo}`"))?;
    let transport = ReqwestTransport::new(format!("Bearer {token}"))?;
    let url = format!(
        "https://api.github.com/repos/{}/{}/issues/{number}",
        coord.owner, coord.repo
    );
    let resp = transport.get(&url).await?;
    if !(200..300).contains(&resp.status) {
        anyhow::bail!("GitHub get issue #{number}: HTTP {}", resp.status);
    }
    parse_issue_detail(&resp.body)
}

/// Post a plain markdown comment onto an issue (`owner/repo#number`) via the GitHub
/// REST API. Returns the created comment's `html_url`. Used by the WorkItem comment
/// path to write back onto the source issue. `repo` must be `owner/name`.
///
/// This is the minimal "comment back" primitive, distinct from the worktracker
/// provider's structured `push_status` / `post_clarifying_questions` (which carry
/// status rollups / clarify markers). A free-text comment from the dev surface uses
/// this so it does not get a status-rollup or clarify-marker wrapper.
pub async fn comment_on_issue(
    repo: &str,
    number: u64,
    body: &str,
    token: &str,
) -> anyhow::Result<String> {
    let coord = RepoCoord::parse(repo)
        .ok_or_else(|| anyhow::anyhow!("repo must be `owner/name`, got `{repo}`"))?;
    let transport = ReqwestTransport::new(format!("Bearer {token}"))?;
    let url = format!(
        "https://api.github.com/repos/{}/{}/issues/{number}/comments",
        coord.owner, coord.repo
    );
    let payload = serde_json::to_string(&serde_json::json!({ "body": body }))
        .map_err(|e| anyhow::anyhow!("encode comment body: {e}"))?;
    let resp = transport.post(&url, &payload).await?;
    if !(200..300).contains(&resp.status) {
        anyhow::bail!("GitHub comment on issue #{number}: HTTP {}", resp.status);
    }
    let v: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| anyhow::anyhow!("parse comment response: {e}"))?;
    Ok(v["html_url"].as_str().unwrap_or_default().to_string())
}

/// One comment on a GitHub issue, flattened for the UoW work-item modal. Only the
/// fields the UI renders are present (author login, body markdown, ISO created-at).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IssueComment {
    /// The comment author's login (e.g. `octocat`). Empty when the API omits it.
    pub author: String,
    /// The comment body (markdown). Empty when the comment has none.
    pub body: String,
    /// The ISO-8601 created-at timestamp as GitHub returns it (e.g.
    /// `2026-06-21T12:00:00Z`). Empty when absent. The UI formats it.
    pub created_at: String,
}

/// The minimal GitHub issue-comment shape we read from the comments list endpoint.
/// The `user` member carries the author; we read only its `login`.
#[derive(Debug, Deserialize)]
struct RawComment {
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    user: Option<RawUser>,
}

/// Minimal GitHub user shape (only the login is read).
#[derive(Debug, Deserialize)]
struct RawUser {
    #[serde(default)]
    login: Option<String>,
}

/// Parse the GitHub issue-comments JSON array into [`IssueComment`] rows. Pure (no
/// I/O) so it is unit-testable against a fixture without a network call or a token.
pub fn parse_issue_comments(json: &str) -> anyhow::Result<Vec<IssueComment>> {
    let raw: Vec<RawComment> =
        serde_json::from_str(json).map_err(|e| anyhow::anyhow!("parse_issue_comments: {e}"))?;
    Ok(raw
        .into_iter()
        .map(|c| IssueComment {
            author: c.user.and_then(|u| u.login).unwrap_or_default(),
            body: c.body.unwrap_or_default(),
            created_at: c.created_at.unwrap_or_default(),
        })
        .collect())
}

/// Fetch the COMMENTS on ONE issue (`owner/repo#number`) via the GitHub REST API,
/// returning them oldest-first (GitHub's default order). Used by the UoW work-item
/// modal. `repo` must be `owner/name`.
///
/// Graceful: this is the network primitive; the HTTP-error case bubbles up so the
/// caller can decide. The token-less / error → empty-list degradation is applied at
/// the endpoint layer (mirroring the list path), so this stays a thin, honest read.
pub async fn get_issue_comments(
    repo: &str,
    number: u64,
    token: &str,
) -> anyhow::Result<Vec<IssueComment>> {
    let coord = RepoCoord::parse(repo)
        .ok_or_else(|| anyhow::anyhow!("repo must be `owner/name`, got `{repo}`"))?;
    let transport = ReqwestTransport::new(format!("Bearer {token}"))?;
    let url = format!(
        "https://api.github.com/repos/{}/{}/issues/{number}/comments?per_page=100",
        coord.owner, coord.repo
    );
    let resp = transport.get(&url).await?;
    if !(200..300).contains(&resp.status) {
        anyhow::bail!("GitHub get issue #{number} comments: HTTP {}", resp.status);
    }
    parse_issue_comments(&resp.body)
}

/// Parse the GitHub assignees JSON array into a flat list of login strings (the users
/// who can be assigned to / mentioned on issues in the repo). Pure (no I/O).
pub fn parse_assignees(json: &str) -> anyhow::Result<Vec<String>> {
    let raw: Vec<RawUser> =
        serde_json::from_str(json).map_err(|e| anyhow::anyhow!("parse_assignees: {e}"))?;
    Ok(raw.into_iter().filter_map(|u| u.login).collect())
}

/// Fetch the ASSIGNABLE users for `owner/repo` via the GitHub REST API, returning
/// their logins. These are the users who can be assigned to issues — the practical
/// @-mention set for the comment box. `repo` must be `owner/name`.
pub async fn get_assignees(repo: &str, token: &str) -> anyhow::Result<Vec<String>> {
    let coord = RepoCoord::parse(repo)
        .ok_or_else(|| anyhow::anyhow!("repo must be `owner/name`, got `{repo}`"))?;
    let transport = ReqwestTransport::new(format!("Bearer {token}"))?;
    let url = format!(
        "https://api.github.com/repos/{}/{}/assignees?per_page=100",
        coord.owner, coord.repo
    );
    let resp = transport.get(&url).await?;
    if !(200..300).contains(&resp.status) {
        anyhow::bail!("GitHub get assignees: HTTP {}", resp.status);
    }
    parse_assignees(&resp.body)
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
    fn parse_issue_detail_maps_state_and_labels_and_rejects_prs() {
        let json = r#"{
            "number": 42,
            "title": "Add CSV export",
            "body": "We need CSV exports.",
            "html_url": "https://github.com/o/r/issues/42",
            "state": "open",
            "labels": [{"name":"bug"},{"name":"camerata:status:intake"}]
        }"#;
        let d = parse_issue_detail(json).expect("parse");
        assert_eq!(d.number, 42);
        assert_eq!(d.title, "Add CSV export");
        assert_eq!(d.body, "We need CSV exports.");
        assert_eq!(d.url, "https://github.com/o/r/issues/42");
        assert_eq!(d.state, "open");
        assert_eq!(d.labels, vec!["bug", "camerata:status:intake"]);

        // A PR row is rejected.
        let pr = r#"{
            "number": 7, "title": "PR", "html_url": "https://github.com/o/r/pull/7",
            "state": "open", "pull_request": {"url": "x"}
        }"#;
        assert!(parse_issue_detail(pr).is_err());
    }

    #[test]
    fn parse_single_issue_null_body_and_rejects_prs() {
        let json = r#"{
            "number": 9, "title": "No body", "html_url": "https://github.com/o/r/issues/9"
        }"#;
        let s = parse_single_issue(json).expect("parse");
        assert_eq!(s.number, 9);
        assert_eq!(s.body, "");
        let pr = r#"{"number":1,"title":"x","html_url":"u","pull_request":{}}"#;
        assert!(parse_single_issue(pr).is_err());
    }

    #[test]
    fn parse_issue_comments_maps_author_body_and_date() {
        let json = r#"[
            {
                "body": "First comment.",
                "created_at": "2026-06-21T12:00:00Z",
                "user": { "login": "octocat" }
            },
            {
                "created_at": "2026-06-21T13:00:00Z"
            }
        ]"#;
        let comments = parse_issue_comments(json).expect("parse");
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].author, "octocat");
        assert_eq!(comments[0].body, "First comment.");
        assert_eq!(comments[0].created_at, "2026-06-21T12:00:00Z");
        // Missing user/body deserialize to empty strings, never a panic.
        assert_eq!(comments[1].author, "");
        assert_eq!(comments[1].body, "");
        assert_eq!(comments[1].created_at, "2026-06-21T13:00:00Z");
    }

    #[test]
    fn parse_issue_comments_rejects_non_array_json() {
        assert!(parse_issue_comments("{\"message\":\"Not Found\"}").is_err());
    }

    #[test]
    fn parse_assignees_flattens_to_logins() {
        let json = r#"[
            { "login": "octocat", "id": 1 },
            { "login": "hubot", "id": 2 },
            { "id": 3 }
        ]"#;
        let users = parse_assignees(json).expect("parse");
        // The login-less row is dropped (no handle to mention).
        assert_eq!(users, vec!["octocat", "hubot"]);
    }

    #[test]
    fn parse_assignees_rejects_non_array_json() {
        assert!(parse_assignees("{\"message\":\"Not Found\"}").is_err());
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
