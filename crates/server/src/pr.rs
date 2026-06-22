//! GitHub pull-request read/write for the per-UoW PR lifecycle (Decision 2).
//!
//! This mirrors the parse/fetch split established in [`crate::github_issues`]: every
//! shape that comes back from GitHub is parsed by a pure `parse_*(json) -> Result<…>`
//! function (fixture-testable, NO network), paired with an async `fetch`-style function
//! that does the HTTP call via the worktracker's [`ReqwestTransport`] and then hands the
//! body to the parser. The parsers are the testable heart; the async wrappers are thin.
//!
//! Token handling matches the issue-intake module: every read is gated on the token by
//! the CALLER (the endpoint layer degrades to an empty/graceful payload when there is no
//! token). These functions take the token directly; with a bad repo / HTTP error they
//! return an `Err` for the caller to fold into the graceful path — they never panic.
//!
//! Discovery + store (the "works even if the PR was made directly in GitHub" requirement)
//! lives in [`resolve_pr_for_uow`]: a STORED `pr_number` always wins; otherwise a
//! head-branch search backfills it and STORES it on the UoW.

use camerata_worktracker::{HttpTransport, ReqwestTransport};
use serde::{Deserialize, Serialize};

use crate::uow::{UnitOfWork, UowStore};

const API: &str = "https://api.github.com";

/// Split `owner/repo` into its two parts, erroring on a malformed coordinate. GitHub's
/// `head=` PR filter needs the owner separately (`head={owner}:{branch}`).
fn split_repo(repo: &str) -> anyhow::Result<(&str, &str)> {
    repo.split_once('/')
        .filter(|(o, n)| !o.is_empty() && !n.is_empty())
        .ok_or_else(|| anyhow::anyhow!("repo must be `owner/repo`, got `{repo}`"))
}

// ── PR state ───────────────────────────────────────────────────────────────────

/// The high-level state of a pull request, normalized from GitHub's `state` + `merged`
/// fields. GitHub reports `state` as `open`/`closed`; a merged PR is `closed` with
/// `merged: true`, so we promote that to its own [`PrState::Merged`] variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    /// The PR is open.
    Open,
    /// The PR was closed without merging.
    Closed,
    /// The PR was merged.
    Merged,
}

impl PrState {
    fn from_github(state: &str, merged: bool) -> Self {
        if merged {
            Self::Merged
        } else if state == "open" {
            Self::Open
        } else {
            Self::Closed
        }
    }
}

/// A pull request flattened to the fields the console renders + the lifecycle needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrInfo {
    /// The PR number (`#N`).
    pub number: u64,
    /// Open / closed / merged (merged promoted out of closed).
    pub state: PrState,
    /// The human-navigable URL on github.com.
    pub url: String,
    /// The head branch name (the branch the PR is FROM).
    pub head_branch: String,
    /// The head commit SHA — used to query CI checks.
    pub head_sha: String,
    /// The base branch name (the branch the PR merges INTO).
    pub base_branch: String,
    /// The PR title.
    pub title: String,
    /// GitHub's mergeability flag (`null` while GitHub is still computing it).
    pub mergeable: Option<bool>,
}

/// The raw GitHub pull-request shape (only the members we read).
#[derive(Debug, Deserialize)]
struct RawPr {
    number: u64,
    state: String,
    #[serde(default)]
    merged: bool,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    mergeable: Option<bool>,
    head: RawPrRef,
    base: RawPrRef,
}

/// The `head` / `base` sub-object on a PR. `ref` is the branch name; `sha` the commit.
#[derive(Debug, Deserialize)]
struct RawPrRef {
    #[serde(default, rename = "ref")]
    r#ref: String,
    #[serde(default)]
    sha: String,
}

/// Parse a SINGLE GitHub pull-request JSON object into [`PrInfo`]. Pure (no I/O), so it
/// is unit-testable against a fixture without a network call or a token.
pub fn parse_pr(json: &str) -> anyhow::Result<PrInfo> {
    let raw: RawPr = serde_json::from_str(json).map_err(|e| anyhow::anyhow!("parse_pr: {e}"))?;
    Ok(PrInfo {
        number: raw.number,
        state: PrState::from_github(&raw.state, raw.merged),
        url: raw.html_url,
        head_branch: raw.head.r#ref,
        head_sha: raw.head.sha,
        base_branch: raw.base.r#ref,
        title: raw.title.unwrap_or_default(),
        mergeable: raw.mergeable,
    })
}

/// Parse the GitHub pulls-LIST JSON array (e.g. the `?head=…` discovery query) into the
/// FIRST pull request's number, if any. Used by [`find_pr_by_head`]'s parse step. Pure.
///
/// The discovery query is filtered to a single head branch, so at most one PR is
/// relevant; we take the first. Returns `None` for an empty array (no PR for that head).
pub fn parse_first_pr_number(json: &str) -> anyhow::Result<Option<u64>> {
    let raw: Vec<RawPr> =
        serde_json::from_str(json).map_err(|e| anyhow::anyhow!("parse_first_pr_number: {e}"))?;
    Ok(raw.first().map(|p| p.number))
}

// ── PR comments (issue comments + review comments) ───────────────────────────────

/// One comment on a PR, normalized across the two GitHub comment endpoints (the
/// conversation/issue comments AND the inline review comments). Only the fields the
/// console renders / the resolve run feeds to the agent are present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrComment {
    /// The comment author's login. Empty when the API omits it.
    pub author: String,
    /// The comment body (markdown). Empty when the comment has none.
    pub body: String,
    /// The ISO-8601 created-at timestamp as GitHub returns it. Empty when absent.
    pub created_at: String,
    /// `true` for an inline review comment (tied to a file/line), `false` for a
    /// conversation comment. The resolve run prioritizes review comments (actionable
    /// code feedback) but the console shows both.
    pub review: bool,
}

/// The minimal GitHub comment shape shared by both comment endpoints.
#[derive(Debug, Deserialize)]
struct RawPrComment {
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    user: Option<RawUser>,
}

#[derive(Debug, Deserialize)]
struct RawUser {
    #[serde(default)]
    login: Option<String>,
}

/// Parse a GitHub comments JSON array (either endpoint) into [`PrComment`] rows, marking
/// each with `review`. Pure (no I/O), unit-testable from a fixture.
pub fn parse_pr_comments(json: &str, review: bool) -> anyhow::Result<Vec<PrComment>> {
    let raw: Vec<RawPrComment> =
        serde_json::from_str(json).map_err(|e| anyhow::anyhow!("parse_pr_comments: {e}"))?;
    Ok(raw
        .into_iter()
        .map(|c| PrComment {
            author: c.user.and_then(|u| u.login).unwrap_or_default(),
            body: c.body.unwrap_or_default(),
            created_at: c.created_at.unwrap_or_default(),
            review,
        })
        .collect())
}

// ── PR CI / check status ─────────────────────────────────────────────────────────

/// A summary of a PR head commit's CI status, collapsing the modern check-runs API and
/// the legacy commit-status API into one pass/fail/pending tally + the failing names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrChecks {
    /// Count of checks that passed (success).
    pub passed: usize,
    /// Count of checks that failed (failure / timed_out / cancelled / action_required,
    /// or a legacy `failure`/`error` state).
    pub failed: usize,
    /// Count of checks still pending (queued / in_progress, or legacy `pending`).
    pub pending: usize,
    /// The names of the checks that FAILED — fed verbatim to the resolve agent so it
    /// knows which checks to fix.
    pub failing: Vec<String>,
}

impl PrChecks {
    /// `true` when there are no checks at all (the head commit has no CI configured, or
    /// none have reported yet). The console renders this distinctly from "all passed".
    pub fn is_empty(&self) -> bool {
        self.passed == 0 && self.failed == 0 && self.pending == 0
    }
}

/// The modern check-runs API shape (`commits/{sha}/check-runs`).
#[derive(Debug, Deserialize)]
struct RawCheckRuns {
    #[serde(default)]
    check_runs: Vec<RawCheckRun>,
}

#[derive(Debug, Deserialize)]
struct RawCheckRun {
    #[serde(default)]
    name: String,
    /// `queued` / `in_progress` / `completed`.
    #[serde(default)]
    status: String,
    /// Set when `status == completed`: `success` / `failure` / `neutral` / `cancelled`
    /// / `timed_out` / `action_required` / `skipped`.
    #[serde(default)]
    conclusion: Option<String>,
}

/// The legacy combined-status API shape (`commits/{sha}/status`).
#[derive(Debug, Deserialize)]
struct RawCombinedStatus {
    #[serde(default)]
    statuses: Vec<RawStatus>,
}

#[derive(Debug, Deserialize)]
struct RawStatus {
    #[serde(default)]
    context: String,
    /// `success` / `failure` / `error` / `pending`.
    #[serde(default)]
    state: String,
}

/// Parse the modern check-runs JSON + the legacy combined-status JSON into one
/// [`PrChecks`] summary. Either input may be empty (`""` skips that source). Pure.
///
/// Conclusion mapping (check-runs): `success`/`skipped`/`neutral` → passed; `failure`/
/// `timed_out`/`cancelled`/`action_required` → failed (name recorded); not-yet-completed
/// (`queued`/`in_progress`) → pending. Legacy statuses: `success` → passed; `failure`/
/// `error` → failed (context recorded); `pending` → pending.
pub fn parse_pr_checks(check_runs_json: &str, status_json: &str) -> anyhow::Result<PrChecks> {
    let mut out = PrChecks::default();

    if !check_runs_json.trim().is_empty() {
        let raw: RawCheckRuns = serde_json::from_str(check_runs_json)
            .map_err(|e| anyhow::anyhow!("parse_pr_checks (check-runs): {e}"))?;
        for run in raw.check_runs {
            if run.status != "completed" {
                out.pending += 1;
                continue;
            }
            match run.conclusion.as_deref() {
                Some("success") | Some("skipped") | Some("neutral") => out.passed += 1,
                Some(_) => {
                    out.failed += 1;
                    out.failing.push(run.name);
                }
                // Completed but no conclusion reported — count as pending (indeterminate).
                None => out.pending += 1,
            }
        }
    }

    if !status_json.trim().is_empty() {
        let raw: RawCombinedStatus = serde_json::from_str(status_json)
            .map_err(|e| anyhow::anyhow!("parse_pr_checks (status): {e}"))?;
        for st in raw.statuses {
            match st.state.as_str() {
                "success" => out.passed += 1,
                "failure" | "error" => {
                    out.failed += 1;
                    out.failing.push(st.context);
                }
                "pending" => out.pending += 1,
                _ => {}
            }
        }
    }

    Ok(out)
}

// ── async fetch wrappers (thin; the parsers above are the testable heart) ─────────

/// Build an authenticated transport for `token`.
fn transport(token: &str) -> anyhow::Result<ReqwestTransport> {
    ReqwestTransport::new(format!("Bearer {token}"))
}

/// Fetch ONE pull request (`owner/repo#number`) and return its [`PrInfo`].
pub async fn get_pr(repo: &str, number: u64, token: &str) -> anyhow::Result<PrInfo> {
    let t = transport(token)?;
    let resp = t.get(&format!("{API}/repos/{repo}/pulls/{number}")).await?;
    if !(200..300).contains(&resp.status) {
        anyhow::bail!("GitHub get PR #{number}: HTTP {}", resp.status);
    }
    parse_pr(&resp.body)
}

/// Find the PR whose HEAD is `branch`, regardless of state (so a merged/closed PR — or
/// one opened directly in GitHub — is still found). Returns its number, or `None` when no
/// PR exists for that head. Mirrors `open_pr`'s discovery query but with `state=all`.
pub async fn find_pr_by_head(repo: &str, branch: &str, token: &str) -> anyhow::Result<Option<u64>> {
    let (owner, _name) = split_repo(repo)?;
    let t = transport(token)?;
    let resp = t
        .get(&format!(
            "{API}/repos/{repo}/pulls?head={owner}:{branch}&state=all"
        ))
        .await?;
    if !(200..300).contains(&resp.status) {
        anyhow::bail!("GitHub find PR by head `{branch}`: HTTP {}", resp.status);
    }
    parse_first_pr_number(&resp.body)
}

/// Fetch BOTH the conversation/issue comments AND the inline review comments for a PR,
/// normalized + concatenated (issue comments first, review comments second). A failure to
/// read EITHER endpoint bubbles up; the endpoint layer folds that into the graceful path.
pub async fn list_pr_comments(
    repo: &str,
    number: u64,
    token: &str,
) -> anyhow::Result<Vec<PrComment>> {
    let t = transport(token)?;
    // Conversation comments live on the ISSUE comments endpoint (a PR is an issue).
    let issue = t
        .get(&format!(
            "{API}/repos/{repo}/issues/{number}/comments?per_page=100"
        ))
        .await?;
    if !(200..300).contains(&issue.status) {
        anyhow::bail!("GitHub PR #{number} issue comments: HTTP {}", issue.status);
    }
    let mut comments = parse_pr_comments(&issue.body, false)?;
    // Inline review comments live on the PULLS comments endpoint.
    let review = t
        .get(&format!(
            "{API}/repos/{repo}/pulls/{number}/comments?per_page=100"
        ))
        .await?;
    if !(200..300).contains(&review.status) {
        anyhow::bail!("GitHub PR #{number} review comments: HTTP {}", review.status);
    }
    comments.extend(parse_pr_comments(&review.body, true)?);
    Ok(comments)
}

/// Fetch the CI status for a PR's head commit: the modern check-runs API plus the legacy
/// combined-status API, summarized into [`PrChecks`]. The legacy call is best-effort — a
/// failure there does not fail the whole read (older/newer repos use one or the other).
pub async fn pr_checks(repo: &str, head_sha: &str, token: &str) -> anyhow::Result<PrChecks> {
    let t = transport(token)?;
    let runs = t
        .get(&format!(
            "{API}/repos/{repo}/commits/{head_sha}/check-runs"
        ))
        .await?;
    let runs_body = if (200..300).contains(&runs.status) {
        runs.body
    } else {
        String::new()
    };
    // Legacy combined status (best-effort).
    let status_body = match t
        .get(&format!("{API}/repos/{repo}/commits/{head_sha}/status"))
        .await
    {
        Ok(r) if (200..300).contains(&r.status) => r.body,
        _ => String::new(),
    };
    parse_pr_checks(&runs_body, &status_body)
}

/// Post a plain markdown comment onto a PR. A PR comment IS an issue comment on the PR
/// number, so this mirrors [`crate::github_issues::comment_on_issue`]. Returns the
/// created comment's `html_url`.
pub async fn post_pr_comment(
    repo: &str,
    number: u64,
    body: &str,
    token: &str,
) -> anyhow::Result<String> {
    crate::github_issues::comment_on_issue(repo, number, body, token).await
}

// ── discovery + store (idempotent) ───────────────────────────────────────────────

/// The action [`resolve_pr_for_uow`] should take, decided purely from the UoW's stored
/// state. Extracted so the precedence logic (stored number wins; else head-branch search;
/// else nothing) is unit-testable WITHOUT a network call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrResolution {
    /// A PR number is already stored — fetch + use it (the stored number always wins).
    UseStored(u64),
    /// No stored number, but the UoW has a branch — search PRs by this head and backfill.
    SearchHead(String),
    /// No stored number and no branch — there is nothing to resolve.
    None,
}

/// Decide how to resolve a UoW's PR from its stored state alone (pure, no I/O). The stored
/// `pr_number` ALWAYS wins; otherwise a non-empty branch triggers a head-branch search;
/// otherwise there is nothing to resolve.
pub fn pr_resolution_plan(uow: &UnitOfWork) -> PrResolution {
    if let Some(number) = uow.pr_number {
        return PrResolution::UseStored(number);
    }
    match uow.branch.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        Some(branch) => PrResolution::SearchHead(branch.to_string()),
        None => PrResolution::None,
    }
}

/// Resolve the PR for a UoW, idempotently, and STORE the resolved number on the UoW.
///
/// Order of precedence:
/// 1. If `uow.pr_number` is already set → fetch + return it (the stored number ALWAYS
///    wins; this is the durable link, robust to a PR's head being renamed/reused).
/// 2. Else, if the UoW has a branch, search PRs by head = the branch (state=all, so a
///    PR opened directly in GitHub or already merged is found). If found, STORE the
///    number + url on the UoW (the backfill — this is the "works even if the PR was made
///    directly in GitHub" requirement) and return it.
/// 3. Else `None` (no stored number, no branch, or no PR exists yet).
///
/// `store` is the [`UowStore`] used for the backfill flush; `story_id` keys it. Any HTTP
/// error returns `None` (the caller's graceful path), never a panic.
pub async fn resolve_pr_for_uow(
    store: &UowStore,
    story_id: &str,
    uow: &UnitOfWork,
    repo: &str,
    token: &str,
) -> Option<PrInfo> {
    match pr_resolution_plan(uow) {
        // 1. Stored number wins.
        PrResolution::UseStored(number) => get_pr(repo, number, token).await.ok(),
        // 2. Head-branch search backfills (and STORES) — the "PR made in GitHub" case.
        PrResolution::SearchHead(branch) => {
            let number = find_pr_by_head(repo, &branch, token).await.ok().flatten()?;
            let info = get_pr(repo, number, token).await.ok()?;
            // STORE so a subsequent resolve takes the stored-number fast path.
            store.set_pr(story_id, Some(info.number), Some(info.url.clone()));
            Some(info)
        }
        PrResolution::None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pr_maps_state_url_branches_and_sha() {
        let json = r#"{
            "number": 7,
            "state": "open",
            "merged": false,
            "mergeable": true,
            "html_url": "https://github.com/o/r/pull/7",
            "title": "Add the thing",
            "head": { "ref": "camerata/story-7", "sha": "abc123" },
            "base": { "ref": "dev/integration", "sha": "def456" }
        }"#;
        let pr = parse_pr(json).expect("parse");
        assert_eq!(pr.number, 7);
        assert_eq!(pr.state, PrState::Open);
        assert_eq!(pr.url, "https://github.com/o/r/pull/7");
        assert_eq!(pr.head_branch, "camerata/story-7");
        assert_eq!(pr.head_sha, "abc123");
        assert_eq!(pr.base_branch, "dev/integration");
        assert_eq!(pr.title, "Add the thing");
        assert_eq!(pr.mergeable, Some(true));
    }

    #[test]
    fn parse_pr_promotes_merged_out_of_closed() {
        // GitHub reports a merged PR as state=closed + merged=true.
        let json = r#"{
            "number": 9, "state": "closed", "merged": true,
            "html_url": "u", "head": {"ref":"b","sha":"s"}, "base": {"ref":"main","sha":"x"}
        }"#;
        assert_eq!(parse_pr(json).unwrap().state, PrState::Merged);

        let closed = r#"{
            "number": 10, "state": "closed", "merged": false,
            "html_url": "u", "head": {"ref":"b","sha":"s"}, "base": {"ref":"main","sha":"x"}
        }"#;
        assert_eq!(parse_pr(closed).unwrap().state, PrState::Closed);
    }

    #[test]
    fn parse_pr_tolerates_null_mergeable_and_missing_title() {
        let json = r#"{
            "number": 1, "state": "open", "merged": false, "mergeable": null,
            "html_url": "u", "head": {"ref":"b","sha":"s"}, "base": {"ref":"main","sha":"x"}
        }"#;
        let pr = parse_pr(json).unwrap();
        assert_eq!(pr.mergeable, None);
        assert_eq!(pr.title, "");
    }

    #[test]
    fn parse_first_pr_number_finds_pr_made_in_github() {
        // The number is unknown locally; discovery (head search) returns the array and
        // we pull the first PR's number — the "PR made directly in GitHub" case.
        let json = r#"[
            {
                "number": 42, "state": "open", "merged": false, "html_url": "u",
                "head": {"ref":"camerata/story-7","sha":"s"}, "base": {"ref":"main","sha":"x"}
            }
        ]"#;
        assert_eq!(parse_first_pr_number(json).unwrap(), Some(42));
        // Empty array → no PR for that head.
        assert_eq!(parse_first_pr_number("[]").unwrap(), None);
    }

    #[test]
    fn parse_pr_comments_marks_review_flag_and_handles_missing_fields() {
        let issue = r#"[
            { "body": "Looks good overall.", "created_at": "2026-06-22T10:00:00Z", "user": {"login":"alice"} },
            { "created_at": "2026-06-22T11:00:00Z" }
        ]"#;
        let parsed = parse_pr_comments(issue, false).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].author, "alice");
        assert_eq!(parsed[0].body, "Looks good overall.");
        assert!(!parsed[0].review);
        // Missing user/body → empty strings, never a panic.
        assert_eq!(parsed[1].author, "");
        assert_eq!(parsed[1].body, "");

        let review = r#"[ { "body": "Fix this off-by-one.", "user": {"login":"bob"} } ]"#;
        let r = parse_pr_comments(review, true).unwrap();
        assert!(r[0].review);
        assert_eq!(r[0].body, "Fix this off-by-one.");
    }

    #[test]
    fn parse_pr_checks_summarizes_passed_failed_pending_with_names() {
        // A realistic check-runs payload: one success, one failure (named), one pending.
        let check_runs = r#"{
            "total_count": 3,
            "check_runs": [
                { "name": "build", "status": "completed", "conclusion": "success" },
                { "name": "clippy", "status": "completed", "conclusion": "failure" },
                { "name": "deploy-preview", "status": "in_progress", "conclusion": null }
            ]
        }"#;
        let checks = parse_pr_checks(check_runs, "").unwrap();
        assert_eq!(checks.passed, 1);
        assert_eq!(checks.failed, 1);
        assert_eq!(checks.pending, 1);
        assert_eq!(checks.failing, vec!["clippy"]);
        assert!(!checks.is_empty());
    }

    #[test]
    fn parse_pr_checks_merges_legacy_combined_status() {
        let status = r#"{
            "state": "failure",
            "statuses": [
                { "context": "ci/circleci", "state": "success" },
                { "context": "ci/legacy-lint", "state": "failure" },
                { "context": "ci/error-one", "state": "error" },
                { "context": "ci/queued", "state": "pending" }
            ]
        }"#;
        let checks = parse_pr_checks("", status).unwrap();
        assert_eq!(checks.passed, 1);
        assert_eq!(checks.failed, 2);
        assert_eq!(checks.pending, 1);
        assert!(checks.failing.contains(&"ci/legacy-lint".to_string()));
        assert!(checks.failing.contains(&"ci/error-one".to_string()));
    }

    #[test]
    fn parse_pr_checks_empty_when_no_sources() {
        let checks = parse_pr_checks("", "").unwrap();
        assert!(checks.is_empty());
        assert_eq!(checks.failing.len(), 0);
    }

    #[test]
    fn pr_checks_is_empty_distinguishes_no_checks_from_all_passed() {
        let none = PrChecks::default();
        assert!(none.is_empty());
        let passed = PrChecks { passed: 2, ..Default::default() };
        assert!(!passed.is_empty());
    }

    // ── discovery + store (the precedence logic, fixture-tested without network) ──

    #[test]
    fn resolution_plan_stored_number_always_wins() {
        let uow = UnitOfWork {
            story_id: "o/r#7".to_string(),
            branch: Some("camerata/story-7".to_string()),
            pr_number: Some(99),
            ..Default::default()
        };
        // Even WITH a branch present, the stored number wins (never re-searches).
        assert_eq!(pr_resolution_plan(&uow), PrResolution::UseStored(99));
    }

    #[test]
    fn resolution_plan_searches_head_when_no_stored_number() {
        // No stored number but a branch → the head-branch search path: this is the
        // "PR made directly in GitHub, number unknown locally" case (discover then store).
        let uow = UnitOfWork {
            story_id: "o/r#7".to_string(),
            branch: Some("camerata/story-7".to_string()),
            pr_number: None,
            ..Default::default()
        };
        assert_eq!(
            pr_resolution_plan(&uow),
            PrResolution::SearchHead("camerata/story-7".to_string())
        );
    }

    #[test]
    fn resolution_plan_none_when_no_number_and_no_branch() {
        let uow = UnitOfWork {
            story_id: "o/r#7".to_string(),
            branch: None,
            pr_number: None,
            ..Default::default()
        };
        assert_eq!(pr_resolution_plan(&uow), PrResolution::None);
        // A blank/whitespace branch is treated the same as no branch.
        let blank = UnitOfWork {
            story_id: "o/r#7".to_string(),
            branch: Some("   ".to_string()),
            pr_number: None,
            ..Default::default()
        };
        assert_eq!(pr_resolution_plan(&blank), PrResolution::None);
    }

    #[test]
    fn backfill_stores_discovered_number_so_next_resolve_uses_stored() {
        // Simulate the head-search backfill side-effect deterministically (no network):
        // a UoW with a branch but no stored number FIRST plans a SearchHead; after the
        // discovered number is stored (as resolve_pr_for_uow does), a re-read plans
        // UseStored — proving the backfill makes discovery idempotent + stored-wins.
        let store = UowStore::new();
        store.set_branch("o/r#7", Some("camerata/story-7".to_string()));
        let before = store.get_or_create("o/r#7");
        assert_eq!(
            pr_resolution_plan(&before),
            PrResolution::SearchHead("camerata/story-7".to_string()),
            "with no stored number the plan must head-search (the PR-made-in-GitHub path)"
        );
        // The backfill that resolve_pr_for_uow performs after find_pr_by_head returns 42.
        store.set_pr("o/r#7", Some(42), Some("https://github.com/o/r/pull/42".to_string()));
        let after = store.get_or_create("o/r#7");
        assert_eq!(
            pr_resolution_plan(&after),
            PrResolution::UseStored(42),
            "after backfill the stored number must win — discovery is idempotent"
        );
        assert_eq!(after.pr_url.as_deref(), Some("https://github.com/o/r/pull/42"));
    }
}
