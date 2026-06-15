//! GitHub Issues adapter (CODE-HOST axis Tier-1, PAT/installation-token auth,
//! poll-inbound).
//!
//! Outbound: POST/PATCH a status rollup comment (Markdown) + labels on the issue
//! + open/close the issue when the status demands it.
//!
//! Inbound is POLL (GET /repos/{owner}/{repo}/issues?since=cursor&state=all).
//! New and updated issues become Updated events; a new comment is the Commented
//! clarify-bridge answer. Webhooks are an opt-in upgrade for reachable deployments
//! (see design doc section 4.1); the poll is the V1 default for local tools.
//!
//! Status channel: GitHub Issues have NO fixed status-category enum (unlike Jira
//! statusCategory / ADO stateCategory). The load-bearing channel is:
//! - An editable issue COMMENT (Markdown) for the rollup payload.
//! - LABELS to project the status onto the board.
//! - The issue open/closed STATE for SIGNED_OFF/DONE and REJECTED.
//!
//! Status -> (label set, open/closed) mapping:
//! - SIGNED_OFF / DONE     -> close the issue (DELETE all camerata:status:* labels)
//! - REJECTED              -> close + `camerata:rejected` label
//! - Gate FAIL             -> open + `camerata:gate-failed` label
//! - BLOCKED               -> open + `camerata:blocked` label
//! - INTAKE / INVESTIGATING / PLANNED / EXECUTING / GATING / AWAITING_QA
//!                         -> open + `camerata:status:<lowercased-variant>` label
//! - AWAITING_CLARIFICATION -> open + `camerata:status:awaiting_clarification`
//!
//! Auth: `Authorization: Bearer <token>`. Also requires `Accept: application/vnd.github+json`.
//! Pass the token directly (PAT or GitHub App installation token). The GitHub App
//! JWT signing flow is NOT implemented here; produce an installation token externally
//! and supply it as the `token` config field.

use async_trait::async_trait;
use serde::Deserialize;

use crate::{
    CanonicalStory, ExternalRef, FeatureStatus, FeatureStatusReport, GateOutcome, InboundKind,
    InboundWorkItemEvent, PrStatus, Provider, RepoCoord, WorkItemProvider,
};

use super::http::HttpTransport;

// ── Config ────────────────────────────────────────────────────────────────────

/// Connection parameters for a GitHub ACCOUNT (not a single repo).
///
/// Per the credential-delegated-scope decision, the connection carries the token
/// and at most an OPTIONAL default repo; the actual repo for any operation is
/// resolved per-request from the `ExternalRef.container`. One `GithubProvider`
/// therefore serves every repo the token can reach in a single process. The
/// `default_owner`/`default_repo` are a convenience fallback for refs that carry
/// no container (legacy/native-style), never a hard ceiling.
#[derive(Debug, Clone)]
pub struct GithubConfig {
    /// Optional default repository owner, used only when a reference carries no
    /// `container`. `None` means "no default; every ref must name its repo."
    pub owner: Option<String>,
    /// Optional default repository name, paired with `owner`.
    pub repo: Option<String>,
    /// A PAT or GitHub App installation token with Issues read/write scope.
    /// The GitHub App JWT signing flow is NOT handled here; produce an
    /// installation token externally and supply it in this field.
    pub token: String,
}

impl GithubConfig {
    /// Build a token-only connection with no default repo. Every operation must
    /// then resolve its repo from the reference's `container`.
    pub fn from_token(token: impl Into<String>) -> Self {
        Self {
            owner: None,
            repo: None,
            token: token.into(),
        }
    }

    /// Build a connection with a default `owner/repo` fallback for container-less
    /// references. Multi-repo still works: a reference that carries a `container`
    /// overrides this default.
    pub fn with_default_repo(
        token: impl Into<String>,
        owner: impl Into<String>,
        repo: impl Into<String>,
    ) -> Self {
        Self {
            owner: Some(owner.into()),
            repo: Some(repo.into()),
            token: token.into(),
        }
    }

    /// Build the `Authorization: Bearer <token>` header value for this config.
    pub fn auth_header(&self) -> String {
        format!("Bearer {}", self.token)
    }

    /// The default repo coordinate, if both owner and repo are set.
    fn default_coord(&self) -> Option<RepoCoord> {
        match (&self.owner, &self.repo) {
            (Some(o), Some(r)) => Some(RepoCoord {
                owner: o.clone(),
                repo: r.clone(),
            }),
            _ => None,
        }
    }
}

// ── Status mapping ────────────────────────────────────────────────────────────

/// Map a canonical `FeatureStatus` to the set of GitHub labels that express it.
///
/// GitHub Issues have no fixed status-category enum. Labels are the
/// machine-readable channel. The rules (from the design doc section 4.2 and
/// the module docstring):
/// - SIGNED_OFF / DONE     -> no camerata status labels (issue is closed by `status_closes_issue`)
/// - REJECTED              -> `["camerata:rejected"]`
/// - BLOCKED               -> `["camerata:blocked"]`
/// - Gating (implied FAIL handled by caller via `camerata:gate-failed`)
/// - All others            -> `["camerata:status:<lowercased variant name>"]`
///
/// Note: callers responsible for adding `camerata:gate-failed` when a gate FAIL is
/// detected (handled in `push_status`). This function returns the status label set,
/// not gate-result labels.
pub fn status_to_labels(status: FeatureStatus) -> Vec<String> {
    match status {
        FeatureStatus::SignedOff | FeatureStatus::Done => vec![],
        FeatureStatus::Rejected => vec!["camerata:rejected".to_string()],
        FeatureStatus::Blocked => vec!["camerata:blocked".to_string()],
        FeatureStatus::Intake => vec!["camerata:status:intake".to_string()],
        FeatureStatus::Investigating => vec!["camerata:status:investigating".to_string()],
        FeatureStatus::AwaitingClarification => {
            vec!["camerata:status:awaiting_clarification".to_string()]
        }
        FeatureStatus::Planned => vec!["camerata:status:planned".to_string()],
        FeatureStatus::Executing => vec!["camerata:status:executing".to_string()],
        FeatureStatus::Gating => vec!["camerata:status:gating".to_string()],
        FeatureStatus::AwaitingQa => vec!["camerata:status:awaiting_qa".to_string()],
    }
}

/// Whether a status should cause the issue to be closed (PATCH state="closed").
///
/// SIGNED_OFF and DONE close the issue (work is complete). REJECTED also closes
/// the issue (decisively declined). All other statuses keep the issue open.
pub fn status_closes_issue(status: FeatureStatus) -> bool {
    matches!(
        status,
        FeatureStatus::SignedOff | FeatureStatus::Done | FeatureStatus::Rejected
    )
}

// ── Markdown helpers ──────────────────────────────────────────────────────────

/// Wrap a string as a Markdown comment body. GitHub comments accept Markdown
/// directly; this helper is a transparent passthrough that documents the intent
/// and provides a stable surface for future escaping if required.
pub fn markdown_comment(text: &str) -> String {
    text.to_string()
}

/// Build a Markdown comment for clarifying questions. Mentions the Product Owner
/// and renders each question as a bullet list item.
///
/// Format:
/// ```text
/// The Camerata orchestrator has the following clarifying questions for the Product Owner.
/// Please reply directly to this comment.
///
/// - Question one
/// - Question two
/// ```
pub fn clarifying_questions_md(questions: &[String]) -> String {
    let mut md = String::from(
        "The Camerata orchestrator has the following clarifying questions for the \
         Product Owner. Please reply directly to this comment.\n",
    );
    if !questions.is_empty() {
        md.push('\n');
        for q in questions {
            md.push_str(&format!("- {q}\n"));
        }
    }
    md
}

/// Build the Markdown status rollup comment pushed back to a GitHub issue when
/// `push_status` is called. Contains:
/// 1. A PR link checklist (repo, url, open/merged/closed as checkbox state).
/// 2. Per-gate pass/fail rows (rule id + outcome + optional message).
/// 3. Sign-off line (who/when), when present.
/// 4. A full-provenance link (the long trail lives in our store, not here).
///
/// PR checkbox state: `[x]` for merged, `[ ]` for open, `[-]` for closed
/// (closed without merging).
pub fn status_rollup_md(report: &FeatureStatusReport) -> String {
    let mut lines: Vec<String> = Vec::new();

    lines.push(format!("## Camerata status: {:?}", report.status));

    // PR links as a Markdown checklist.
    if !report.pr_links.is_empty() {
        lines.push(String::new());
        lines.push("### Pull requests".to_string());
        lines.push(String::new());
        for pr in &report.pr_links {
            let checkbox = match pr.status {
                PrStatus::Merged => "[x]",
                PrStatus::Open => "[ ]",
                PrStatus::Closed => "[-]",
            };
            lines.push(format!(
                "- {} [{}]({}) ({})",
                checkbox, pr.title, pr.url, pr.repo
            ));
        }
    }

    // Gate results.
    if !report.gate_results.is_empty() {
        lines.push(String::new());
        lines.push("### Gate results".to_string());
        lines.push(String::new());
        for g in &report.gate_results {
            let outcome = match g.result {
                GateOutcome::Pass => "PASS",
                GateOutcome::Fail => "FAIL",
            };
            let msg = g.message.as_deref().unwrap_or("");
            if msg.is_empty() {
                lines.push(format!("- [{}] {}", outcome, g.rule_id));
            } else {
                lines.push(format!("- [{}] {}: {}", outcome, g.rule_id, msg));
            }
        }
    }

    // Sign-off.
    if let Some(sign) = &report.sign_off {
        lines.push(String::new());
        lines.push(format!("**Signed off** by {} at {}", sign.by, sign.at));
    }

    // Provenance link.
    lines.push(String::new());
    lines.push(format!("[Full provenance]({})", report.provenance_url));

    lines.join("\n")
}

// ── URL / path helpers ────────────────────────────────────────────────────────

/// Build the issues-list poll path for `GET /repos/{owner}/{repo}/issues`.
///
/// Always includes `state=all` so closed issues are returned (needed to detect
/// sign-off and rejection). When `cursor` is `Some(ts)`, appends `&since=<ts>`
/// to filter to issues updated at or after that ISO 8601 timestamp.
pub fn issues_since_path(owner: &str, repo: &str, cursor: Option<&str>) -> String {
    match cursor {
        None => format!("/repos/{owner}/{repo}/issues?state=all"),
        Some(ts) => format!("/repos/{owner}/{repo}/issues?state=all&since={ts}"),
    }
}

// ── GitHub API response shapes ────────────────────────────────────────────────

/// Minimal representation of one label on a GitHub issue.
#[derive(Debug, Deserialize)]
struct GithubLabel {
    name: String,
}

/// Minimal representation of a GitHub issue from the issues-list or single-issue
/// endpoints. Only the fields we read are present.
#[derive(Debug, Deserialize)]
struct GithubIssue {
    number: u64,
    title: String,
    #[serde(default)]
    body: Option<String>,
    state: String,
    updated_at: String,
    html_url: String,
    #[serde(default)]
    labels: Vec<GithubLabel>,
    #[serde(default)]
    user: Option<GithubUser>,
}

/// Minimal GitHub user object (we only need the login name).
#[derive(Debug, Deserialize)]
struct GithubUser {
    login: String,
}

/// Response from POST /repos/{owner}/{repo}/issues/{number}/comments.
/// We only read the comment id back.
#[derive(Debug, Deserialize)]
struct GithubCommentCreated {
    id: u64,
}

// ── Status inference from open/closed + labels ────────────────────────────────

/// Derive a best-effort `FeatureStatus` from a GitHub issue's open/closed state
/// and its label set.
///
/// Priority order (first match wins):
/// 1. Closed + `camerata:rejected` label -> Rejected
/// 2. Closed (any other labels) -> Done
/// 3. Open + `camerata:blocked` -> Blocked
/// 4. Open + `camerata:status:<variant>` -> the matching FeatureStatus
/// 5. Open + `camerata:gate-failed` -> Gating (gate failed, still in gating phase)
/// 6. Open, no camerata labels -> Intake (safe default for new/unlabeled issues)
fn infer_status(state: &str, labels: &[GithubLabel]) -> FeatureStatus {
    let label_names: Vec<&str> = labels.iter().map(|l| l.name.as_str()).collect();
    let is_closed = state == "closed";

    if is_closed {
        if label_names.contains(&"camerata:rejected") {
            return FeatureStatus::Rejected;
        }
        return FeatureStatus::Done;
    }

    // Open issue: check labels in priority order.
    if label_names.contains(&"camerata:blocked") {
        return FeatureStatus::Blocked;
    }
    for label in &label_names {
        if let Some(variant) = label.strip_prefix("camerata:status:") {
            return match variant {
                "intake" => FeatureStatus::Intake,
                "investigating" => FeatureStatus::Investigating,
                "awaiting_clarification" => FeatureStatus::AwaitingClarification,
                "planned" => FeatureStatus::Planned,
                "executing" => FeatureStatus::Executing,
                "gating" => FeatureStatus::Gating,
                "awaiting_qa" => FeatureStatus::AwaitingQa,
                _ => FeatureStatus::Investigating, // unknown camerata:status:* -> safe default
            };
        }
    }
    if label_names.contains(&"camerata:gate-failed") {
        return FeatureStatus::Gating;
    }
    // No camerata labels: safe default for unlabeled issues.
    FeatureStatus::Intake
}

// ── Parsers ───────────────────────────────────────────────────────────────────

/// Parse a GitHub issues-list JSON array into `InboundWorkItemEvent`s.
///
/// Each issue becomes one `Updated` event (polling cannot distinguish creates
/// from updates; `Updated` is the conservative choice for reconciliation
/// correctness; the delivery_id dedup table prevents double-processing).
///
/// The issue `number` (integer) is used as the `external_id`; the `updated_at`
/// timestamp drives the next cursor.
pub fn parse_issues(json: &str) -> anyhow::Result<Vec<InboundWorkItemEvent>> {
    let issues: Vec<GithubIssue> =
        serde_json::from_str(json).map_err(|e| anyhow::anyhow!("parse_issues: {e}"))?;

    let events = issues
        .into_iter()
        .map(|issue| {
            let id_str = issue.number.to_string();
            let status = infer_status(&issue.state, &issue.labels);
            InboundWorkItemEvent {
                reference: ExternalRef {
                    provider: Provider::GitHub,
                    external_id: id_str.clone(),
                    // Set by the provider's poll() after parsing (it knows the repo).
                    container: None,
                    url: issue.html_url.clone(),
                    revision: None,
                },
                kind: InboundKind::Updated,
                title: Some(issue.title.clone()),
                description: None,
                status: Some(status),
                body: None,
                delivery_id: format!("github-poll-{id_str}"),
                is_echo: false,
                occurred_at: issue.updated_at.clone(),
            }
        })
        .collect();

    Ok(events)
}

/// Parse a single GitHub issue JSON object into a `CanonicalStory`.
///
/// Maps:
/// - `number`    -> `external_id` (stringified)
/// - `html_url`  -> `ExternalRef.url`
/// - `title`     -> `title`
/// - `body`      -> `description` (Markdown, stored as-is)
/// - `state` + labels -> `status` via `infer_status`
/// - `user.login` -> `created_by`
pub fn parse_issue(json: &str) -> anyhow::Result<CanonicalStory> {
    let issue: GithubIssue =
        serde_json::from_str(json).map_err(|e| anyhow::anyhow!("parse_issue: {e}"))?;

    let status = infer_status(&issue.state, &issue.labels);
    let description = issue.body.unwrap_or_default();
    let created_by = issue
        .user
        .map(|u| u.login)
        .unwrap_or_else(|| "unknown".to_string());
    let id_str = issue.number.to_string();

    Ok(CanonicalStory {
        id: id_str.clone(),
        external_ref: Some(ExternalRef {
            provider: Provider::GitHub,
            external_id: id_str,
            // Set by the provider's ingest_story() after parsing (it knows the repo).
            container: None,
            url: issue.html_url,
            revision: None,
        }),
        title: issue.title,
        description,
        status,
        created_by,
        // Build targets are assigned later (decomposition / adoption), not derived
        // from the source issue. The source repo lives on external_ref.container.
        targets: vec![],
    })
}

// ── GithubProvider ────────────────────────────────────────────────────────────

/// GitHub Issues adapter implementing `WorkItemProvider`. Parameterized over the
/// HTTP transport so it can be unit-tested with `FakeTransport` without network.
pub struct GithubProvider<T: HttpTransport> {
    config: GithubConfig,
    transport: T,
}

impl<T: HttpTransport> GithubProvider<T> {
    /// Construct a new GitHub provider with the given config and transport.
    pub fn new(config: GithubConfig, transport: T) -> Self {
        Self { config, transport }
    }

    /// Resolve the repo coordinate for one operation: the reference's `container`
    /// (`owner/repo`) when present, else the connection's default repo. Errors
    /// when neither is available, so a container-less ref against a default-less
    /// connection fails loudly instead of hitting the wrong repo.
    fn resolve_coord(&self, reference: &ExternalRef) -> anyhow::Result<RepoCoord> {
        if let Some(container) = reference.container.as_deref() {
            return RepoCoord::parse(container).ok_or_else(|| {
                anyhow::anyhow!(
                    "GitHub reference container is not `owner/repo`: {container:?} (item {})",
                    reference.external_id
                )
            });
        }
        self.config.default_coord().ok_or_else(|| {
            anyhow::anyhow!(
                "GitHub reference for item {} has no container and the connection has \
                 no default repo; cannot resolve owner/repo",
                reference.external_id
            )
        })
    }

    fn issue_url(coord: &RepoCoord, number: &str) -> String {
        format!(
            "https://api.github.com/repos/{}/{}/issues/{number}",
            coord.owner, coord.repo
        )
    }

    fn labels_url(coord: &RepoCoord, number: &str) -> String {
        format!(
            "https://api.github.com/repos/{}/{}/issues/{number}/labels",
            coord.owner, coord.repo
        )
    }

    fn comments_url(coord: &RepoCoord, number: &str) -> String {
        format!(
            "https://api.github.com/repos/{}/{}/issues/{number}/comments",
            coord.owner, coord.repo
        )
    }

    fn poll_url(coord: &RepoCoord, cursor: Option<&str>) -> String {
        let path = issues_since_path(&coord.owner, &coord.repo, cursor);
        format!("https://api.github.com{path}")
    }
}

#[async_trait]
impl<T: HttpTransport> WorkItemProvider for GithubProvider<T> {
    fn kind(&self) -> Provider {
        Provider::GitHub
    }

    async fn ingest_story(&self, reference: &ExternalRef) -> anyhow::Result<CanonicalStory> {
        let coord = self.resolve_coord(reference)?;
        let url = Self::issue_url(&coord, &reference.external_id);
        let resp = self.transport.get(&url).await?;
        if resp.status < 200 || resp.status >= 300 {
            anyhow::bail!(
                "GitHub GET issue {} in {coord}: HTTP {} {}",
                reference.external_id,
                resp.status,
                resp.body
            );
        }
        let mut story = parse_issue(&resp.body)?;
        // Stamp the resolved repo onto the story's ref so downstream operations
        // (push_status, clarify) hit the same repo without re-deriving it.
        if let Some(ext) = story.external_ref.as_mut() {
            ext.container = Some(coord.to_string());
        }
        Ok(story)
    }

    async fn push_status(
        &self,
        reference: &ExternalRef,
        report: &FeatureStatusReport,
    ) -> anyhow::Result<()> {
        let number = &reference.external_id;
        let coord = self.resolve_coord(reference)?;

        // Step 1: PATCH the issue state (open or closed).
        let closed = status_closes_issue(report.status);
        let state_value = if closed { "closed" } else { "open" };
        let patch_body = serde_json::to_string(&serde_json::json!({ "state": state_value }))
            .unwrap_or_else(|_| format!(r#"{{"state":"{state_value}"}}"#));
        let issue_url = Self::issue_url(&coord, number);
        let patch_resp = self.transport.post(&issue_url, &patch_body).await?;
        if patch_resp.status >= 300 {
            anyhow::bail!(
                "GitHub PATCH issue {} state: HTTP {} {}",
                number,
                patch_resp.status,
                patch_resp.body
            );
        }

        // Step 2: PUT the label set. GitHub's PUT /labels REPLACES the full label
        // list atomically, which is what we want for a clean status transition.
        let mut labels = status_to_labels(report.status);

        // Add camerata:gate-failed if any gate failed.
        let any_gate_failed = report
            .gate_results
            .iter()
            .any(|g| g.result == GateOutcome::Fail);
        if any_gate_failed {
            labels.push("camerata:gate-failed".to_string());
        }

        let labels_body = serde_json::to_string(&serde_json::json!({ "labels": labels }))
            .unwrap_or_else(|_| r#"{"labels":[]}"#.to_string());
        let labels_url = Self::labels_url(&coord, number);
        let labels_resp = self.transport.put(&labels_url, &labels_body).await?;
        if labels_resp.status >= 300 {
            anyhow::bail!(
                "GitHub PUT labels on issue {}: HTTP {} {}",
                number,
                labels_resp.status,
                labels_resp.body
            );
        }

        // Step 3: POST the status rollup comment (provenance trail).
        let md = status_rollup_md(report);
        let comment_body = serde_json::to_string(&serde_json::json!({ "body": md }))
            .unwrap_or_else(|_| format!(r#"{{"body":"{}"}}"#, md.replace('"', "\\\"")));
        let comments_url = Self::comments_url(&coord, number);
        let comment_resp = self.transport.post(&comments_url, &comment_body).await?;
        if comment_resp.status >= 300 {
            anyhow::bail!(
                "GitHub POST rollup comment on issue {}: HTTP {} {}",
                number,
                comment_resp.status,
                comment_resp.body
            );
        }

        Ok(())
    }

    async fn post_clarifying_questions(
        &self,
        reference: &ExternalRef,
        questions: &[String],
    ) -> anyhow::Result<String> {
        let coord = self.resolve_coord(reference)?;
        let md = clarifying_questions_md(questions);
        let body = serde_json::to_string(&serde_json::json!({ "body": md }))
            .unwrap_or_else(|_| format!(r#"{{"body":"{}"}}"#, md.replace('"', "\\\"")));
        let url = Self::comments_url(&coord, &reference.external_id);
        let resp = self.transport.post(&url, &body).await?;
        if resp.status < 200 || resp.status >= 300 {
            anyhow::bail!(
                "GitHub POST clarifying questions on issue {}: HTTP {} {}",
                reference.external_id,
                resp.status,
                resp.body
            );
        }
        let created: GithubCommentCreated = serde_json::from_str(&resp.body).map_err(|e| {
            anyhow::anyhow!(
                "parse GitHub comment response for {}: {e} (body: {})",
                reference.external_id,
                resp.body
            )
        })?;
        Ok(created.id.to_string())
    }

    async fn poll(
        &self,
        cursor: Option<&str>,
    ) -> anyhow::Result<(Vec<InboundWorkItemEvent>, String)> {
        // `poll` has no per-item reference, so it targets the connection's
        // default repo. Multi-repo polling (iterating a saved working set of
        // repos) is the working-set concern handled above this provider; a
        // default-less connection cannot poll and says so.
        let coord = self.config.default_coord().ok_or_else(|| {
            anyhow::anyhow!(
                "GitHub poll requires a default repo on the connection; none configured"
            )
        })?;
        let url = Self::poll_url(&coord, cursor);
        let resp = self.transport.get(&url).await?;
        if resp.status < 200 || resp.status >= 300 {
            anyhow::bail!("GitHub poll GET in {coord}: HTTP {} {}", resp.status, resp.body);
        }

        let mut events = parse_issues(&resp.body)?;
        // Stamp the polled repo onto every event's reference so each carries its
        // source container for downstream per-item operations.
        for ev in events.iter_mut() {
            ev.reference.container = Some(coord.to_string());
        }

        // Derive the next cursor from the maximum `updated_at` across all events.
        // Never use wall-clock time; derive from the data so the cursor is stable.
        let next_cursor = events
            .iter()
            .map(|e| e.occurred_at.as_str())
            .max()
            .map(|s| s.to_string())
            .unwrap_or_else(|| cursor.unwrap_or("").to_string());

        Ok((events, next_cursor))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::FakeTransport;
    use crate::{FeatureStatusReport, GateOutcome, GateResult, PrLink, PrStatus, SignOff};

    // ── Helper builders ────────────────────────────────────────────────────

    fn test_config() -> GithubConfig {
        GithubConfig::with_default_repo("ghp_testtoken", "myorg", "my-project")
    }

    fn github_ref(number: &str) -> ExternalRef {
        ExternalRef {
            provider: Provider::GitHub,
            external_id: number.to_string(),
            container: None,
            url: format!("https://github.com/myorg/my-project/issues/{number}"),
            revision: None,
        }
    }

    fn make_report(status: FeatureStatus) -> FeatureStatusReport {
        FeatureStatusReport {
            status,
            pr_links: vec![],
            gate_results: vec![],
            sign_off: None,
            provenance_url: "https://camerata.internal/provenance/42".to_string(),
        }
    }

    // ── auth_header ────────────────────────────────────────────────────────

    #[test]
    fn auth_header_is_bearer_token() {
        let cfg = GithubConfig::with_default_repo("ghp_abc123", "o", "r");
        assert_eq!(cfg.auth_header(), "Bearer ghp_abc123");
    }

    // ── status_to_labels ───────────────────────────────────────────────────

    #[test]
    fn status_to_labels_full_mapping() {
        // Statuses that produce no camerata labels (issue is closed instead).
        assert_eq!(
            status_to_labels(FeatureStatus::SignedOff),
            Vec::<String>::new()
        );
        assert_eq!(status_to_labels(FeatureStatus::Done), Vec::<String>::new());

        // Statuses with dedicated labels.
        assert_eq!(
            status_to_labels(FeatureStatus::Rejected),
            vec!["camerata:rejected"]
        );
        assert_eq!(
            status_to_labels(FeatureStatus::Blocked),
            vec!["camerata:blocked"]
        );

        // camerata:status:<lowercased> for in-progress variants.
        assert_eq!(
            status_to_labels(FeatureStatus::Intake),
            vec!["camerata:status:intake"]
        );
        assert_eq!(
            status_to_labels(FeatureStatus::Investigating),
            vec!["camerata:status:investigating"]
        );
        assert_eq!(
            status_to_labels(FeatureStatus::AwaitingClarification),
            vec!["camerata:status:awaiting_clarification"]
        );
        assert_eq!(
            status_to_labels(FeatureStatus::Planned),
            vec!["camerata:status:planned"]
        );
        assert_eq!(
            status_to_labels(FeatureStatus::Executing),
            vec!["camerata:status:executing"]
        );
        assert_eq!(
            status_to_labels(FeatureStatus::Gating),
            vec!["camerata:status:gating"]
        );
        assert_eq!(
            status_to_labels(FeatureStatus::AwaitingQa),
            vec!["camerata:status:awaiting_qa"]
        );
    }

    // ── status_closes_issue ────────────────────────────────────────────────

    #[test]
    fn status_closes_issue_mapping() {
        // SIGNED_OFF, DONE, REJECTED all close the issue.
        assert!(status_closes_issue(FeatureStatus::SignedOff));
        assert!(status_closes_issue(FeatureStatus::Done));
        assert!(status_closes_issue(FeatureStatus::Rejected));

        // All others leave it open.
        assert!(!status_closes_issue(FeatureStatus::Intake));
        assert!(!status_closes_issue(FeatureStatus::Investigating));
        assert!(!status_closes_issue(FeatureStatus::AwaitingClarification));
        assert!(!status_closes_issue(FeatureStatus::Planned));
        assert!(!status_closes_issue(FeatureStatus::Executing));
        assert!(!status_closes_issue(FeatureStatus::Gating));
        assert!(!status_closes_issue(FeatureStatus::AwaitingQa));
        assert!(!status_closes_issue(FeatureStatus::Blocked));
    }

    // ── markdown_comment ───────────────────────────────────────────────────

    #[test]
    fn markdown_comment_is_transparent_passthrough() {
        let text = "Hello, **world**!\n\nSome [link](https://example.com).";
        assert_eq!(markdown_comment(text), text);
    }

    // ── clarifying_questions_md ────────────────────────────────────────────

    #[test]
    fn clarifying_questions_md_mentions_product_owner() {
        let questions = vec!["Q1?".to_string()];
        let md = clarifying_questions_md(&questions);
        assert!(
            md.to_lowercase().contains("product owner"),
            "must mention Product Owner, got: {md}"
        );
    }

    #[test]
    fn clarifying_questions_md_contains_all_questions_as_bullets() {
        let questions = vec![
            "What is the target audience?".to_string(),
            "Are notifications opt-in or opt-out?".to_string(),
        ];
        let md = clarifying_questions_md(&questions);
        assert!(
            md.contains("- What is the target audience?"),
            "must contain first question as bullet: {md}"
        );
        assert!(
            md.contains("- Are notifications opt-in or opt-out?"),
            "must contain second question as bullet: {md}"
        );
    }

    #[test]
    fn clarifying_questions_md_empty_questions_has_intro_only() {
        let md = clarifying_questions_md(&[]);
        assert!(
            md.to_lowercase().contains("product owner"),
            "intro must still mention Product Owner"
        );
        assert!(!md.contains("- "), "no bullets when no questions");
    }

    // ── status_rollup_md ───────────────────────────────────────────────────

    #[test]
    fn status_rollup_md_contains_provenance_url() {
        let report = make_report(FeatureStatus::Executing);
        let md = status_rollup_md(&report);
        assert!(
            md.contains("camerata.internal/provenance"),
            "rollup must contain provenance URL: {md}"
        );
    }

    #[test]
    fn status_rollup_md_contains_pr_links_as_checklist() {
        let report = FeatureStatusReport {
            status: FeatureStatus::Done,
            pr_links: vec![
                PrLink {
                    repo: "org/repo-a".to_string(),
                    url: "https://github.com/org/repo-a/pull/1".to_string(),
                    title: "Add login page".to_string(),
                    status: PrStatus::Merged,
                },
                PrLink {
                    repo: "org/repo-b".to_string(),
                    url: "https://github.com/org/repo-b/pull/5".to_string(),
                    title: "API changes".to_string(),
                    status: PrStatus::Open,
                },
            ],
            gate_results: vec![],
            sign_off: None,
            provenance_url: "https://camerata.internal/provenance/1".to_string(),
        };
        let md = status_rollup_md(&report);

        // Merged PR -> checked checkbox [x]
        assert!(md.contains("[x]"), "merged PR must use [x] checkbox: {md}");
        // Open PR -> unchecked checkbox [ ]
        assert!(md.contains("[ ]"), "open PR must use [ ] checkbox: {md}");
        // Both repos should appear
        assert!(md.contains("org/repo-a"), "must list first repo: {md}");
        assert!(md.contains("org/repo-b"), "must list second repo: {md}");
        // Both URLs should appear
        assert!(
            md.contains("https://github.com/org/repo-a/pull/1"),
            "must include first PR URL: {md}"
        );
        assert!(
            md.contains("https://github.com/org/repo-b/pull/5"),
            "must include second PR URL: {md}"
        );
    }

    #[test]
    fn status_rollup_md_closed_pr_uses_dash_checkbox() {
        let report = FeatureStatusReport {
            status: FeatureStatus::Done,
            pr_links: vec![PrLink {
                repo: "org/repo".to_string(),
                url: "https://github.com/org/repo/pull/99".to_string(),
                title: "Reverted".to_string(),
                status: PrStatus::Closed,
            }],
            gate_results: vec![],
            sign_off: None,
            provenance_url: "https://camerata.internal/provenance/2".to_string(),
        };
        let md = status_rollup_md(&report);
        assert!(md.contains("[-]"), "closed PR must use [-] checkbox: {md}");
    }

    #[test]
    fn status_rollup_md_contains_gate_results() {
        let report = FeatureStatusReport {
            status: FeatureStatus::Gating,
            pr_links: vec![],
            gate_results: vec![
                GateResult {
                    rule_id: "GATE-001".to_string(),
                    result: GateOutcome::Pass,
                    message: Some("All checks passed.".to_string()),
                },
                GateResult {
                    rule_id: "GATE-002".to_string(),
                    result: GateOutcome::Fail,
                    message: None,
                },
            ],
            sign_off: None,
            provenance_url: "https://camerata.internal/provenance/3".to_string(),
        };
        let md = status_rollup_md(&report);
        assert!(
            md.contains("GATE-001"),
            "must contain first gate rule id: {md}"
        );
        assert!(md.contains("PASS"), "must contain PASS outcome: {md}");
        assert!(
            md.contains("GATE-002"),
            "must contain second gate rule id: {md}"
        );
        assert!(md.contains("FAIL"), "must contain FAIL outcome: {md}");
        assert!(
            md.contains("All checks passed"),
            "must contain gate message: {md}"
        );
    }

    #[test]
    fn status_rollup_md_contains_sign_off() {
        let report = FeatureStatusReport {
            status: FeatureStatus::SignedOff,
            pr_links: vec![],
            gate_results: vec![],
            sign_off: Some(SignOff {
                by: "alice".to_string(),
                at: "2026-06-14T10:00:00Z".to_string(),
            }),
            provenance_url: "https://camerata.internal/provenance/4".to_string(),
        };
        let md = status_rollup_md(&report);
        assert!(md.contains("alice"), "must contain approver name: {md}");
        assert!(
            md.contains("2026-06-14"),
            "must contain sign-off timestamp: {md}"
        );
    }

    // Multi-repo PR rollup test (two PrLinks render as two checklist items).
    #[test]
    fn status_rollup_md_multi_repo_rollup_renders_both_prs() {
        let report = FeatureStatusReport {
            status: FeatureStatus::Executing,
            pr_links: vec![
                PrLink {
                    repo: "myorg/backend".to_string(),
                    url: "https://github.com/myorg/backend/pull/7".to_string(),
                    title: "Backend changes".to_string(),
                    status: PrStatus::Open,
                },
                PrLink {
                    repo: "myorg/frontend".to_string(),
                    url: "https://github.com/myorg/frontend/pull/3".to_string(),
                    title: "Frontend changes".to_string(),
                    status: PrStatus::Open,
                },
            ],
            gate_results: vec![],
            sign_off: None,
            provenance_url: "https://camerata.internal/provenance/5".to_string(),
        };
        let md = status_rollup_md(&report);
        assert!(md.contains("myorg/backend"), "must list backend repo: {md}");
        assert!(
            md.contains("myorg/frontend"),
            "must list frontend repo: {md}"
        );
        // Count the number of checklist items (each starts with "- [ ]" or "- [x]" or "- [-]").
        let checklist_count = md
            .lines()
            .filter(|l| l.contains("[ ]") || l.contains("[x]") || l.contains("[-]"))
            .count();
        assert_eq!(
            checklist_count, 2,
            "two PRs must produce two checklist items"
        );
    }

    // ── issues_since_path ──────────────────────────────────────────────────

    #[test]
    fn issues_since_path_none_has_no_since_param() {
        let path = issues_since_path("myorg", "my-project", None);
        assert!(
            path.contains("/repos/myorg/my-project/issues"),
            "must contain repo path: {path}"
        );
        assert!(path.contains("state=all"), "must include state=all: {path}");
        assert!(
            !path.contains("since"),
            "None cursor must not include since param: {path}"
        );
    }

    #[test]
    fn issues_since_path_some_includes_since_param() {
        let ts = "2026-06-01T00:00:00Z";
        let path = issues_since_path("myorg", "my-project", Some(ts));
        assert!(path.contains("state=all"), "must include state=all: {path}");
        assert!(
            path.contains(&format!("since={ts}")),
            "must include since param: {path}"
        );
    }

    // ── parse_issues ───────────────────────────────────────────────────────

    const ISSUES_JSON: &str = r#"[
        {
            "number": 1,
            "title": "Build login page",
            "body": "As a user I want to log in.",
            "state": "open",
            "updated_at": "2026-06-01T10:00:00Z",
            "html_url": "https://github.com/myorg/my-project/issues/1",
            "labels": [{"name": "camerata:status:intake"}],
            "user": {"login": "alice"}
        },
        {
            "number": 2,
            "title": "Dark mode support",
            "body": null,
            "state": "closed",
            "updated_at": "2026-06-02T12:00:00Z",
            "html_url": "https://github.com/myorg/my-project/issues/2",
            "labels": [],
            "user": {"login": "bob"}
        }
    ]"#;

    #[test]
    fn parse_issues_two_issues() {
        let events = parse_issues(ISSUES_JSON).expect("parse must succeed");
        assert_eq!(events.len(), 2);

        let e0 = &events[0];
        assert_eq!(e0.reference.external_id, "1");
        assert_eq!(e0.reference.provider, Provider::GitHub);
        assert_eq!(e0.kind, InboundKind::Updated);
        assert_eq!(e0.status, Some(FeatureStatus::Intake));
        assert_eq!(e0.title, Some("Build login page".to_string()));
        assert_eq!(e0.occurred_at, "2026-06-01T10:00:00Z");
        assert_eq!(e0.delivery_id, "github-poll-1");
        assert!(!e0.is_echo);

        let e1 = &events[1];
        assert_eq!(e1.reference.external_id, "2");
        assert_eq!(e1.status, Some(FeatureStatus::Done)); // closed, no rejected label
        assert_eq!(e1.occurred_at, "2026-06-02T12:00:00Z");
        assert_eq!(e1.delivery_id, "github-poll-2");
    }

    // ── parse_issue ────────────────────────────────────────────────────────

    const SINGLE_ISSUE_JSON: &str = r#"{
        "number": 42,
        "title": "Add CSV export",
        "body": "We need CSV exports for reports.",
        "state": "open",
        "updated_at": "2026-06-01T10:00:00Z",
        "html_url": "https://github.com/myorg/my-project/issues/42",
        "labels": [{"name": "camerata:status:executing"}],
        "user": {"login": "carol"}
    }"#;

    #[test]
    fn parse_issue_maps_to_canonical_story() {
        let story = parse_issue(SINGLE_ISSUE_JSON).expect("parse must succeed");
        assert_eq!(story.id, "42");
        assert_eq!(story.title, "Add CSV export");
        assert_eq!(story.description, "We need CSV exports for reports.");
        assert_eq!(story.status, FeatureStatus::Executing);
        assert_eq!(story.created_by, "carol");
        let ext = story.external_ref.expect("must have external_ref");
        assert_eq!(ext.provider, Provider::GitHub);
        assert_eq!(ext.external_id, "42");
        assert_eq!(ext.url, "https://github.com/myorg/my-project/issues/42");
    }

    #[test]
    fn parse_issue_null_body_is_empty_string() {
        let json = r#"{
            "number": 5,
            "title": "No body issue",
            "body": null,
            "state": "open",
            "updated_at": "2026-06-01T10:00:00Z",
            "html_url": "https://github.com/myorg/my-project/issues/5",
            "labels": [],
            "user": {"login": "dave"}
        }"#;
        let story = parse_issue(json).expect("parse must succeed");
        assert_eq!(story.description, "");
        assert_eq!(story.status, FeatureStatus::Intake); // open, no labels
    }

    #[test]
    fn parse_issue_rejected_closed_with_label() {
        let json = r#"{
            "number": 7,
            "title": "Rejected idea",
            "body": null,
            "state": "closed",
            "updated_at": "2026-06-01T10:00:00Z",
            "html_url": "https://github.com/myorg/my-project/issues/7",
            "labels": [{"name": "camerata:rejected"}],
            "user": {"login": "eve"}
        }"#;
        let story = parse_issue(json).expect("parse must succeed");
        assert_eq!(story.status, FeatureStatus::Rejected);
    }

    #[test]
    fn parse_issue_missing_user_falls_back_to_unknown() {
        let json = r#"{
            "number": 9,
            "title": "Mystery issue",
            "body": null,
            "state": "open",
            "updated_at": "2026-06-01T10:00:00Z",
            "html_url": "https://github.com/myorg/my-project/issues/9",
            "labels": []
        }"#;
        let story = parse_issue(json).expect("parse must succeed");
        assert_eq!(story.created_by, "unknown");
    }

    // ── infer_status ───────────────────────────────────────────────────────

    #[test]
    fn infer_status_label_map_full() {
        let open_with = |name: &str| -> FeatureStatus {
            infer_status(
                "open",
                &[GithubLabel {
                    name: name.to_string(),
                }],
            )
        };

        assert_eq!(open_with("camerata:status:intake"), FeatureStatus::Intake);
        assert_eq!(
            open_with("camerata:status:investigating"),
            FeatureStatus::Investigating
        );
        assert_eq!(
            open_with("camerata:status:awaiting_clarification"),
            FeatureStatus::AwaitingClarification
        );
        assert_eq!(open_with("camerata:status:planned"), FeatureStatus::Planned);
        assert_eq!(
            open_with("camerata:status:executing"),
            FeatureStatus::Executing
        );
        assert_eq!(open_with("camerata:status:gating"), FeatureStatus::Gating);
        assert_eq!(
            open_with("camerata:status:awaiting_qa"),
            FeatureStatus::AwaitingQa
        );
        assert_eq!(open_with("camerata:blocked"), FeatureStatus::Blocked);
        assert_eq!(open_with("camerata:gate-failed"), FeatureStatus::Gating);
    }

    #[test]
    fn infer_status_closed_no_rejected_label_is_done() {
        assert_eq!(infer_status("closed", &[]), FeatureStatus::Done);
    }

    #[test]
    fn infer_status_closed_with_rejected_label_is_rejected() {
        assert_eq!(
            infer_status(
                "closed",
                &[GithubLabel {
                    name: "camerata:rejected".to_string()
                }]
            ),
            FeatureStatus::Rejected
        );
    }

    #[test]
    fn infer_status_open_no_labels_is_intake() {
        assert_eq!(infer_status("open", &[]), FeatureStatus::Intake);
    }

    // ── GithubProvider via FakeTransport ───────────────────────────────────

    #[tokio::test]
    async fn provider_kind_is_github() {
        let transport = FakeTransport::new();
        let provider = GithubProvider::new(test_config(), transport);
        assert_eq!(provider.kind(), Provider::GitHub);
    }

    #[tokio::test]
    async fn ingest_story_calls_get_and_returns_canonical_story() {
        let transport = FakeTransport::new().on("GET", "/issues/42", 200, SINGLE_ISSUE_JSON);
        let provider = GithubProvider::new(test_config(), transport);
        let story = provider.ingest_story(&github_ref("42")).await.unwrap();
        assert_eq!(story.id, "42");
        assert_eq!(story.title, "Add CSV export");
        assert_eq!(story.status, FeatureStatus::Executing);
        assert_eq!(story.created_by, "carol");

        let calls = provider.transport.recorded_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "GET");
        assert!(
            calls[0].1.contains("/issues/42"),
            "GET must target issue 42"
        );
    }

    #[tokio::test]
    async fn push_status_done_closes_issue_and_posts_comment() {
        let transport = FakeTransport::new()
            .on(
                "POST",
                "/issues/42",
                200,
                r#"{"number":42,"state":"closed"}"#,
            )
            .on("PUT", "/labels", 200, "[]")
            .on("POST", "/comments", 201, r#"{"id":100}"#);

        let provider = GithubProvider::new(test_config(), transport);
        let report = FeatureStatusReport {
            status: FeatureStatus::Done,
            pr_links: vec![],
            gate_results: vec![],
            sign_off: None,
            provenance_url: "https://camerata.internal/provenance/42".to_string(),
        };

        provider
            .push_status(&github_ref("42"), &report)
            .await
            .unwrap();

        let calls = provider.transport.recorded_calls();
        let methods: Vec<&str> = calls.iter().map(|(m, _, _)| m.as_str()).collect();
        // POST (PATCH issue state), PUT labels, POST comment.
        assert_eq!(
            methods,
            vec!["POST", "PUT", "POST"],
            "expected state PATCH, labels PUT, rollup comment POST: {methods:?}"
        );

        // PATCH body must request state=closed.
        let patch_body = &calls[0].2;
        let v: serde_json::Value =
            serde_json::from_str(patch_body).expect("patch body must be valid JSON");
        assert_eq!(v["state"], "closed", "DONE must close the issue");
    }

    #[tokio::test]
    async fn push_status_gate_fail_adds_gate_failed_label() {
        let transport = FakeTransport::new()
            .on("POST", "/issues/10", 200, r#"{"number":10,"state":"open"}"#)
            .on("PUT", "/labels", 200, "[]")
            .on("POST", "/comments", 201, r#"{"id":200}"#);

        let provider = GithubProvider::new(test_config(), transport);
        let report = FeatureStatusReport {
            status: FeatureStatus::Gating,
            pr_links: vec![],
            gate_results: vec![GateResult {
                rule_id: "GATE-001".to_string(),
                result: GateOutcome::Fail,
                message: Some("Lint failed.".to_string()),
            }],
            sign_off: None,
            provenance_url: "https://camerata.internal/provenance/10".to_string(),
        };

        provider
            .push_status(&github_ref("10"), &report)
            .await
            .unwrap();

        let calls = provider.transport.recorded_calls();
        // Second call is the PUT labels.
        let (_, labels_url, labels_body) = &calls[1];
        assert!(
            labels_url.contains("/labels"),
            "second call must be to labels endpoint: {labels_url}"
        );
        let v: serde_json::Value =
            serde_json::from_str(labels_body).expect("labels body must be valid JSON");
        let labels: Vec<&str> = v["labels"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|l| l.as_str())
            .collect();
        assert!(
            labels.contains(&"camerata:gate-failed"),
            "gate FAIL must add camerata:gate-failed label: {labels:?}"
        );
    }

    #[tokio::test]
    async fn push_status_rejected_closes_issue_with_rejected_label() {
        let transport = FakeTransport::new()
            .on(
                "POST",
                "/issues/55",
                200,
                r#"{"number":55,"state":"closed"}"#,
            )
            .on("PUT", "/labels", 200, "[]")
            .on("POST", "/comments", 201, r#"{"id":300}"#);

        let provider = GithubProvider::new(test_config(), transport);
        provider
            .push_status(&github_ref("55"), &make_report(FeatureStatus::Rejected))
            .await
            .unwrap();

        let calls = provider.transport.recorded_calls();
        // Patch body -> state=closed.
        let patch_v: serde_json::Value =
            serde_json::from_str(&calls[0].2).expect("patch body JSON");
        assert_eq!(patch_v["state"], "closed", "REJECTED must close the issue");

        // Labels body -> contains camerata:rejected.
        let labels_v: serde_json::Value =
            serde_json::from_str(&calls[1].2).expect("labels body JSON");
        let labels: Vec<&str> = labels_v["labels"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|l| l.as_str())
            .collect();
        assert!(
            labels.contains(&"camerata:rejected"),
            "REJECTED must include camerata:rejected label: {labels:?}"
        );
    }

    #[tokio::test]
    async fn push_status_rollup_comment_contains_provenance_url() {
        let transport = FakeTransport::new()
            .on("POST", "/issues/77", 200, r#"{"number":77,"state":"open"}"#)
            .on("PUT", "/labels", 200, "[]")
            .on("POST", "/comments", 201, r#"{"id":400}"#);

        let provider = GithubProvider::new(test_config(), transport);
        provider
            .push_status(&github_ref("77"), &make_report(FeatureStatus::Executing))
            .await
            .unwrap();

        let calls = provider.transport.recorded_calls();
        let comment_body = &calls[2].2;
        assert!(
            comment_body.contains("camerata.internal/provenance"),
            "rollup comment must contain provenance URL: {comment_body}"
        );
    }

    #[tokio::test]
    async fn post_clarifying_questions_posts_markdown_comment() {
        let response_json = r#"{"id": 99, "html_url": "https://github.com/..."}"#;
        let transport = FakeTransport::new().on("POST", "/comments", 201, response_json);

        let provider = GithubProvider::new(test_config(), transport);
        let questions = vec![
            "Which date format?".to_string(),
            "Are exports opt-in?".to_string(),
        ];
        let comment_id = provider
            .post_clarifying_questions(&github_ref("42"), &questions)
            .await
            .unwrap();

        assert_eq!(comment_id, "99");

        let calls = provider.transport.recorded_calls();
        assert_eq!(calls.len(), 1);
        let (method, url, body) = &calls[0];
        assert_eq!(method, "POST");
        assert!(
            url.contains("/comments"),
            "must post to comments endpoint: {url}"
        );

        // Body must be valid JSON with a "body" field containing the Markdown.
        let v: serde_json::Value = serde_json::from_str(body).expect("body must be valid JSON");
        let md = v["body"].as_str().expect("body.body must be a string");
        assert!(
            md.contains("Which date format?"),
            "comment body must contain first question: {md}"
        );
        assert!(
            md.contains("Are exports opt-in?"),
            "comment body must contain second question: {md}"
        );
        assert!(
            md.to_lowercase().contains("product owner"),
            "comment body must mention Product Owner: {md}"
        );
    }

    #[tokio::test]
    async fn poll_returns_events_and_next_cursor() {
        let transport = FakeTransport::new().on("GET", "/issues", 200, ISSUES_JSON);
        let provider = GithubProvider::new(test_config(), transport);

        let (events, next_cursor) = provider.poll(None).await.unwrap();

        assert_eq!(events.len(), 2, "must return 2 events from poll");
        assert_eq!(events[0].reference.external_id, "1");
        assert_eq!(events[1].reference.external_id, "2");

        // Next cursor is the max updated_at across the two issues.
        assert_eq!(
            next_cursor, "2026-06-02T12:00:00Z",
            "cursor must be the max updated_at timestamp"
        );
    }

    #[tokio::test]
    async fn poll_with_cursor_embeds_since_param_in_url() {
        let transport = FakeTransport::new().on("GET", "/issues", 200, r#"[]"#);
        let provider = GithubProvider::new(test_config(), transport);

        let cursor = "2026-06-01T00:00:00Z";
        let (events, _) = provider.poll(Some(cursor)).await.unwrap();
        assert!(events.is_empty(), "empty response returns no events");

        let calls = provider.transport.recorded_calls();
        let get_url = &calls[0].1;
        assert!(
            get_url.contains("since="),
            "GET URL must embed the since param: {get_url}"
        );
        assert!(
            get_url.contains("2026-06-01"),
            "GET URL must embed the cursor timestamp: {get_url}"
        );
    }

    #[tokio::test]
    async fn poll_empty_result_echoes_cursor() {
        let transport = FakeTransport::new().on("GET", "/issues", 200, r#"[]"#);
        let provider = GithubProvider::new(test_config(), transport);

        let cursor = "2026-06-03T00:00:00Z";
        let (events, next_cursor) = provider.poll(Some(cursor)).await.unwrap();
        assert!(events.is_empty());
        assert_eq!(
            next_cursor, cursor,
            "empty result must echo the incoming cursor"
        );
    }

    #[tokio::test]
    async fn cursor_derived_from_max_updated_at_not_wall_clock() {
        // Two issues: one older, one newer. Cursor must be the max updated_at.
        let transport = FakeTransport::new().on("GET", "/issues", 200, ISSUES_JSON);
        let provider = GithubProvider::new(test_config(), transport);
        let (events, next_cursor) = provider.poll(None).await.unwrap();
        assert_eq!(events.len(), 2);
        // e0 has 2026-06-01, e1 has 2026-06-02; cursor must be the max.
        assert_eq!(next_cursor, "2026-06-02T12:00:00Z");
    }

    #[tokio::test]
    async fn clarify_bridge_round_trip_via_github_provider() {
        // 1. post_clarifying_questions -> POST /comments
        // 2. poll -> GET /issues returns the issue updated (PO commented)
        let comment_response = r#"{"id": 77}"#;
        let poll_response_with_answer = r#"[{
            "number": 42,
            "title": "Build login page",
            "body": "PO added acceptance criteria.",
            "state": "open",
            "updated_at": "2026-06-03T09:00:00Z",
            "html_url": "https://github.com/myorg/my-project/issues/42",
            "labels": [{"name": "camerata:status:investigating"}],
            "user": {"login": "po-user"}
        }]"#;

        let transport = FakeTransport::new()
            .on("POST", "/comments", 201, comment_response)
            .on("GET", "/issues", 200, poll_response_with_answer);

        let provider = GithubProvider::new(test_config(), transport);

        // Step 1: post clarifying questions.
        let questions = vec!["Should exports use UTC?".to_string()];
        let comment_id = provider
            .post_clarifying_questions(&github_ref("42"), &questions)
            .await
            .unwrap();
        assert_eq!(comment_id, "77");

        // Step 2: poll returns the updated issue as an inbound event.
        let (events, _cursor) = provider.poll(None).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].reference.external_id, "42");
        assert_eq!(events[0].kind, InboundKind::Updated);

        // Verify FakeTransport recorded both calls.
        let calls = provider.transport.recorded_calls();
        let post_calls: Vec<_> = calls.iter().filter(|(m, _, _)| m == "POST").collect();
        assert_eq!(post_calls.len(), 1, "must have one POST for the comment");
        let (_, post_url, post_body) = &post_calls[0];
        assert!(
            post_url.contains("/comments"),
            "POST must target the comments endpoint"
        );
        assert!(
            post_body.contains("Should exports use UTC"),
            "POST body must contain the question"
        );
    }

    // ── Multi-repo: one provider/connection serves many repos (Phase A) ─────

    /// A GitHub reference that names its source repo via `container`.
    fn github_ref_in(container: &str, number: &str) -> ExternalRef {
        ExternalRef::new(
            Provider::GitHub,
            number,
            format!("https://github.com/{container}/issues/{number}"),
        )
        .with_container(container)
    }

    #[tokio::test]
    async fn one_token_only_provider_serves_two_repos_via_container() {
        // Scripted responses keyed by the FULL repo path, so a wrong-repo URL
        // would 404 and fail the assertions.
        let issue_a = r#"{"number":1,"title":"A","body":null,"state":"open",
            "updated_at":"2026-06-01T00:00:00Z",
            "html_url":"https://github.com/orgA/repoA/issues/1","labels":[],"user":{"login":"a"}}"#;
        let issue_b = r#"{"number":2,"title":"B","body":null,"state":"open",
            "updated_at":"2026-06-01T00:00:00Z",
            "html_url":"https://github.com/orgB/repoB/issues/2","labels":[],"user":{"login":"b"}}"#;
        let transport = FakeTransport::new()
            .on("GET", "/repos/orgA/repoA/issues/1", 200, issue_a)
            .on("GET", "/repos/orgB/repoB/issues/2", 200, issue_b);

        // Token-only connection: NO default repo. The container selects the repo.
        let provider = GithubProvider::new(GithubConfig::from_token("ghp_x"), transport);

        let story_a = provider
            .ingest_story(&github_ref_in("orgA/repoA", "1"))
            .await
            .expect("repo A ingest");
        let story_b = provider
            .ingest_story(&github_ref_in("orgB/repoB", "2"))
            .await
            .expect("repo B ingest");

        assert_eq!(story_a.title, "A");
        assert_eq!(story_b.title, "B");

        // Each call hit its own repo's URL — proven by the recorded calls.
        let calls = provider.transport.recorded_calls();
        assert!(calls.iter().any(|(_, u, _)| u.contains("/repos/orgA/repoA/issues/1")));
        assert!(calls.iter().any(|(_, u, _)| u.contains("/repos/orgB/repoB/issues/2")));
    }

    #[tokio::test]
    async fn container_overrides_the_default_repo() {
        let issue = r#"{"number":5,"title":"Override","body":null,"state":"open",
            "updated_at":"2026-06-01T00:00:00Z",
            "html_url":"https://github.com/other/repo/issues/5","labels":[],"user":{"login":"x"}}"#;
        let transport = FakeTransport::new().on("GET", "/repos/other/repo/issues/5", 200, issue);
        // Default is myorg/my-project, but the ref names other/repo.
        let provider = GithubProvider::new(test_config(), transport);

        let story = provider
            .ingest_story(&github_ref_in("other/repo", "5"))
            .await
            .expect("ingest");
        assert_eq!(story.title, "Override");
        let calls = provider.transport.recorded_calls();
        assert!(
            calls[0].1.contains("/repos/other/repo/issues/5"),
            "container must override the default repo: {}",
            calls[0].1
        );
    }

    #[tokio::test]
    async fn falls_back_to_default_repo_when_ref_has_no_container() {
        let transport =
            FakeTransport::new().on("GET", "/repos/myorg/my-project/issues/42", 200, SINGLE_ISSUE_JSON);
        let provider = GithubProvider::new(test_config(), transport);

        // github_ref() has container: None -> resolves to the default repo.
        let story = provider.ingest_story(&github_ref("42")).await.expect("ingest");
        assert_eq!(story.id, "42");
        let calls = provider.transport.recorded_calls();
        assert!(calls[0].1.contains("/repos/myorg/my-project/issues/42"));
    }

    #[tokio::test]
    async fn errors_when_no_container_and_no_default_repo() {
        let provider = GithubProvider::new(GithubConfig::from_token("ghp_x"), FakeTransport::new());
        let err = provider
            .ingest_story(&github_ref("99"))
            .await
            .expect_err("must refuse: no repo to resolve");
        let msg = err.to_string();
        assert!(
            msg.contains("no container") && msg.contains("no default repo"),
            "error must explain the missing repo coordinate: {msg}"
        );
    }

    #[tokio::test]
    async fn ingest_stamps_resolved_container_on_returned_story() {
        let transport =
            FakeTransport::new().on("GET", "/repos/orgA/repoA/issues/1", 200,
                r#"{"number":1,"title":"A","body":null,"state":"open",
                "updated_at":"2026-06-01T00:00:00Z",
                "html_url":"https://github.com/orgA/repoA/issues/1","labels":[],"user":{"login":"a"}}"#);
        let provider = GithubProvider::new(GithubConfig::from_token("ghp_x"), transport);

        let story = provider
            .ingest_story(&github_ref_in("orgA/repoA", "1"))
            .await
            .expect("ingest");
        let ext = story.external_ref.expect("has ref");
        assert_eq!(
            ext.container.as_deref(),
            Some("orgA/repoA"),
            "ingest must stamp the resolved repo onto the story ref"
        );
    }

    #[tokio::test]
    async fn invalid_container_is_a_clear_error() {
        let provider = GithubProvider::new(GithubConfig::from_token("ghp_x"), FakeTransport::new());
        let bad = ExternalRef::new(Provider::GitHub, "1", "url").with_container("not-a-repo-spec");
        let err = provider.ingest_story(&bad).await.expect_err("must reject");
        assert!(
            err.to_string().contains("not `owner/repo`"),
            "error must name the malformed container: {err}"
        );
    }

    #[tokio::test]
    async fn poll_requires_a_default_repo_and_stamps_container_on_events() {
        // Token-only connection cannot poll: there is no per-item ref to carry a repo.
        let bare = GithubProvider::new(GithubConfig::from_token("ghp_x"), FakeTransport::new());
        assert!(
            bare.poll(None).await.is_err(),
            "poll without a default repo must error"
        );

        // With a default repo, poll works and stamps the repo on each event.
        let transport = FakeTransport::new().on("GET", "/issues", 200, ISSUES_JSON);
        let provider = GithubProvider::new(test_config(), transport);
        let (events, _) = provider.poll(None).await.expect("poll");
        assert_eq!(events.len(), 2);
        assert!(
            events.iter().all(|e| e.reference.container.as_deref() == Some("myorg/my-project")),
            "every polled event must carry the polled repo as its container"
        );
    }
}
