//! Jira Cloud adapter (Phase D, API-token + Basic auth, JQL-polling inbound).
//!
//! Inbound is JQL polling: the API-token + Basic auth model CANNOT register
//! webhooks (Jira gates webhook registration to Connect / OAuth 2.0 app
//! identities), so polling is the only inbound path here. Outbound writes use
//! the Jira REST API v3: transitions API for status changes, and ADF
//! (Atlassian Document Format) for all comments.
//!
//! Map to `statusCategory` KEYS (`new`, `indeterminate`, `done`), never to
//! user-renamed status names. The status category set is fixed in Jira; only the
//! human-facing names change. The sentinel category ("undefined", id 1) is
//! treated as unmapped.

use async_trait::async_trait;
use serde::Deserialize;

use crate::{
    CanonicalStory, ExternalRef, FeatureStatus, FeatureStatusReport, GateOutcome, InboundKind,
    InboundWorkItemEvent, PrStatus, Provider, WorkItemProvider,
};

use super::http::HttpTransport;

// ── Config ────────────────────────────────────────────────────────────────────

/// Connection parameters for one Jira instance.
#[derive(Debug, Clone)]
pub struct JiraConfig {
    /// Base URL of the Jira instance, e.g. `https://myorg.atlassian.net`.
    /// No trailing slash.
    pub base_url: String,
    /// The Atlassian account email associated with the API token.
    pub email: String,
    /// The API token (from `id.atlassian.com/manage-profile/security/api-tokens`).
    pub api_token: String,
}

impl JiraConfig {
    /// Build the `Authorization: Basic ...` header value for this config.
    /// Encodes `email:api_token` in URL-safe base64 (no line breaks).
    pub fn auth_header(&self) -> String {
        let raw = format!("{}:{}", self.email, self.api_token);
        format!("Basic {}", base64_encode(raw.as_bytes()))
    }
}

// ── Minimal base64 encoder (no external dep) ──────────────────────────────────

/// Hand-rolled base64 encoder. Produces the standard alphabet (A-Z a-z 0-9 + /)
/// with `=` padding. No line breaks. Enough for an Authorization header value.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let combined = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((combined >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((combined >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((combined >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(combined & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

// ── Status mapping ────────────────────────────────────────────────────────────

/// Map a canonical `FeatureStatus` to the Jira `statusCategory` key.
///
/// Per the design doc (section 4.2):
/// - INTAKE -> `new`
/// - INVESTIGATING / PLANNED / EXECUTING / GATING / AWAITING_CLARIFICATION / AWAITING_QA / BLOCKED / REJECTED -> `indeterminate`
/// - SIGNED_OFF / DONE -> `done`
///
/// BLOCKED and REJECTED map to `indeterminate` because Jira has no dedicated
/// category for them. The adapter degrades to posting a comment when the desired
/// state cannot be expressed via a legal transition.
pub fn status_to_category(status: FeatureStatus) -> &'static str {
    match status {
        FeatureStatus::Intake => "new",
        FeatureStatus::Investigating
        | FeatureStatus::AwaitingClarification
        | FeatureStatus::Planned
        | FeatureStatus::Executing
        | FeatureStatus::Gating
        | FeatureStatus::AwaitingQa
        | FeatureStatus::Blocked
        | FeatureStatus::Rejected => "indeterminate",
        FeatureStatus::SignedOff | FeatureStatus::Done => "done",
    }
}

/// Map a Jira `statusCategory` key to a canonical `FeatureStatus`.
///
/// Known keys: `new`, `indeterminate`, `done`. The sentinel `undefined` (id 1)
/// and any unknown key map to `Investigating` (a safe, recoverable default that
/// keeps the story in the active-work category without losing it).
pub fn category_to_status(category: &str) -> FeatureStatus {
    match category {
        "new" => FeatureStatus::Intake,
        "indeterminate" => FeatureStatus::Investigating,
        "done" => FeatureStatus::Done,
        // Sentinel "undefined" (id 1) or any future unknown key: safe default.
        _ => FeatureStatus::Investigating,
    }
}

// ── ADF helpers ───────────────────────────────────────────────────────────────

/// Build an ADF (Atlassian Document Format) JSON string for a single-paragraph
/// plain-text comment. ADF version 1, minimal: `doc -> paragraph -> text`.
pub fn adf_comment(text: &str) -> String {
    // Escape any double-quotes or backslashes in the text to produce valid JSON.
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        r#"{{"version":1,"type":"doc","content":[{{"type":"paragraph","content":[{{"type":"text","text":"{escaped}"}}]}}]}}"#
    )
}

/// Build an ADF JSON string for a clarifying-questions comment. Format:
/// an intro paragraph followed by a bullet list (bulletList -> listItem ->
/// paragraph -> text) of the questions.
pub fn adf_clarifying_questions(questions: &[String]) -> String {
    // Intro paragraph.
    let intro_node = r#"{"type":"paragraph","content":[{"type":"text","text":"The Camerata orchestrator has the following clarifying questions for the Product Owner. Please reply directly to this comment."}]}"#;

    // Each question becomes a listItem.
    let list_items: Vec<String> = questions
        .iter()
        .map(|q| {
            let escaped = q.replace('\\', "\\\\").replace('"', "\\\"");
            format!(
                r#"{{"type":"listItem","content":[{{"type":"paragraph","content":[{{"type":"text","text":"{escaped}"}}]}}]}}"#
            )
        })
        .collect();

    let bullet_list = format!(
        r#"{{"type":"bulletList","content":[{}]}}"#,
        list_items.join(",")
    );

    format!(r#"{{"version":1,"type":"doc","content":[{intro_node},{bullet_list}]}}"#)
}

/// Build the ADF JSON for the status rollup comment pushed back to the tracker
/// when `push_status` is called. Contains the PR links checklist, per-gate
/// pass/fail rows, and the sign-off line (when present).
pub fn status_rollup_adf(report: &FeatureStatusReport) -> String {
    let mut content_nodes: Vec<String> = Vec::new();

    // Header paragraph.
    let status_str = format!("{:?}", report.status);
    let header_escaped = format!("Camerata status: {status_str}");
    let header_escaped = header_escaped.replace('"', "\\\"");
    content_nodes.push(format!(
        r#"{{"type":"paragraph","content":[{{"type":"text","text":"{header_escaped}"}}]}}"#
    ));

    // PR links section.
    if !report.pr_links.is_empty() {
        let header_pr = r#"{"type":"paragraph","content":[{"type":"text","text":"Pull requests:","marks":[{"type":"strong"}]}]}"#;
        content_nodes.push(header_pr.to_string());

        let pr_items: Vec<String> = report
            .pr_links
            .iter()
            .map(|pr| {
                let pr_status = match pr.status {
                    PrStatus::Open => "open",
                    PrStatus::Merged => "merged",
                    PrStatus::Closed => "closed",
                };
                let title_escaped = pr.title.replace('"', "\\\"");
                let url_escaped = pr.url.replace('"', "\\\"");
                let repo_escaped = pr.repo.replace('"', "\\\"");
                let line = format!("[{pr_status}] {title_escaped} ({repo_escaped})");
                let line_escaped = line.replace('"', "\\\"");
                format!(
                    r#"{{"type":"listItem","content":[{{"type":"paragraph","content":[{{"type":"text","text":"{line_escaped}","marks":[{{"type":"link","attrs":{{"href":"{url_escaped}"}}}}]}}]}}]}}"#
                )
            })
            .collect();

        content_nodes.push(format!(
            r#"{{"type":"bulletList","content":[{}]}}"#,
            pr_items.join(",")
        ));
    }

    // Gate results section.
    if !report.gate_results.is_empty() {
        let header_gates = r#"{"type":"paragraph","content":[{"type":"text","text":"Gate results:","marks":[{"type":"strong"}]}]}"#;
        content_nodes.push(header_gates.to_string());

        let gate_items: Vec<String> = report
            .gate_results
            .iter()
            .map(|g| {
                let outcome = match g.result {
                    GateOutcome::Pass => "PASS",
                    GateOutcome::Fail => "FAIL",
                };
                let rule_escaped = g.rule_id.replace('"', "\\\"");
                let msg = g
                    .message
                    .as_deref()
                    .unwrap_or("")
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"");
                let line = if msg.is_empty() {
                    format!("[{outcome}] {rule_escaped}")
                } else {
                    format!("[{outcome}] {rule_escaped}: {msg}")
                };
                let line_escaped = line.replace('"', "\\\"");
                format!(
                    r#"{{"type":"listItem","content":[{{"type":"paragraph","content":[{{"type":"text","text":"{line_escaped}"}}]}}]}}"#
                )
            })
            .collect();

        content_nodes.push(format!(
            r#"{{"type":"bulletList","content":[{}]}}"#,
            gate_items.join(",")
        ));
    }

    // Sign-off line.
    if let Some(sign) = &report.sign_off {
        let by_escaped = sign.by.replace('"', "\\\"");
        let at_escaped = sign.at.replace('"', "\\\"");
        let line = format!("Signed off by {by_escaped} at {at_escaped}");
        let line_escaped = line.replace('"', "\\\"");
        content_nodes.push(format!(
            r#"{{"type":"paragraph","content":[{{"type":"text","text":"{line_escaped}"}}]}}"#
        ));
    }

    // Provenance link paragraph.
    let prov_escaped = report.provenance_url.replace('"', "\\\"");
    let prov_line = format!("Full provenance: {prov_escaped}");
    let prov_line_escaped = prov_line.replace('"', "\\\"");
    content_nodes.push(format!(
        r#"{{"type":"paragraph","content":[{{"type":"text","text":"{prov_line_escaped}"}}]}}"#
    ));

    format!(
        r#"{{"version":1,"type":"doc","content":[{}]}}"#,
        content_nodes.join(",")
    )
}

// ── JQL builder ───────────────────────────────────────────────────────────────

/// Build a JQL string for the poll query. When `cursor` is `None`, defaults to
/// `updated >= -7d ORDER BY updated ASC` (a safe 7-day lookback for first run).
/// When `cursor` is `Some(ts)`, uses `updated >= "ts" ORDER BY updated ASC`.
/// The cursor is an ISO 8601 datetime string extracted from the max `updated`
/// field in the previous search response.
pub fn jql_updated_since(cursor: Option<&str>) -> String {
    match cursor {
        None => "updated >= -7d ORDER BY updated ASC".to_string(),
        Some(ts) => {
            let escaped = ts.replace('"', "\\\"");
            format!(r#"updated >= "{escaped}" ORDER BY updated ASC"#)
        }
    }
}

// ── Transition helpers ────────────────────────────────────────────────────────

/// Minimal representation of one Jira issue transition returned by the
/// `GET /rest/api/3/issue/{key}/transitions` endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JiraTransition {
    /// The transition id (used in the POST body to execute it).
    pub id: String,
    /// The `statusCategory` key of the target status (e.g. `"done"`).
    pub to_category: String,
}

/// Pick a legal transition that targets the desired `statusCategory` key.
/// Returns the transition `id` when one is found, or `None` when no legal
/// transition leads to that category. The caller MUST degrade to a comment
/// when `None` is returned; forcing an illegal transition is forbidden.
///
/// When multiple transitions target the same category, the first one wins.
pub fn pick_transition(available: &[JiraTransition], target_category: &str) -> Option<String> {
    available
        .iter()
        .find(|t| t.to_category == target_category)
        .map(|t| t.id.clone())
}

// ── Jira response shapes (for deserialization) ────────────────────────────────

/// Subset of the Jira issue JSON needed for `parse_issue` and `parse_search_results`.
#[derive(Debug, Deserialize)]
struct JiraIssue {
    key: String,
    #[serde(rename = "self")]
    self_url: String,
    fields: JiraFields,
}

#[derive(Debug, Deserialize)]
struct JiraFields {
    summary: String,
    #[serde(default)]
    description: Option<serde_json::Value>,
    status: JiraStatus,
    #[serde(default)]
    updated: Option<String>,
    #[serde(default)]
    creator: Option<JiraUser>,
}

#[derive(Debug, Deserialize)]
struct JiraStatus {
    #[serde(rename = "statusCategory")]
    status_category: JiraStatusCategory,
}

#[derive(Debug, Deserialize)]
struct JiraStatusCategory {
    key: String,
}

#[derive(Debug, Deserialize)]
struct JiraUser {
    #[serde(rename = "emailAddress", default)]
    email_address: Option<String>,
    #[serde(rename = "displayName", default)]
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JiraSearchResponse {
    issues: Vec<JiraIssue>,
}

#[derive(Debug, Deserialize)]
struct JiraTransitionRaw {
    id: String,
    to: JiraTransitionTarget,
}

#[derive(Debug, Deserialize)]
struct JiraTransitionTarget {
    #[serde(rename = "statusCategory")]
    status_category: JiraStatusCategory,
}

#[derive(Debug, Deserialize)]
struct JiraTransitionsResponse {
    transitions: Vec<JiraTransitionRaw>,
}

#[derive(Debug, Deserialize)]
struct JiraCommentCreated {
    id: String,
}

// ── Parsers ───────────────────────────────────────────────────────────────────

/// Parse a Jira `GET /rest/api/3/search` response into `InboundWorkItemEvent`s.
/// Each issue becomes one `Updated` event (the polling path cannot distinguish
/// creates from updates; we conservatively use `Updated` for reconciliation
/// correctness; the delivery_id dedup table prevents double-processing).
pub fn parse_search_results(json: &str) -> anyhow::Result<Vec<InboundWorkItemEvent>> {
    let resp: JiraSearchResponse =
        serde_json::from_str(json).map_err(|e| anyhow::anyhow!("parse_search_results: {e}"))?;

    let events = resp
        .issues
        .into_iter()
        .map(|issue| {
            let category = &issue.fields.status.status_category.key;
            let status = category_to_status(category);
            let occurred_at = issue
                .fields
                .updated
                .clone()
                .unwrap_or_else(|| "1970-01-01T00:00:00.000+0000".to_string());
            InboundWorkItemEvent {
                reference: ExternalRef {
                    provider: Provider::Jira,
                    external_id: issue.key.clone(),
                    url: issue.self_url.clone(),
                    revision: None,
                },
                kind: InboundKind::Updated,
                title: Some(issue.fields.summary.clone()),
                description: None,
                status: Some(status),
                body: None,
                delivery_id: format!("jira-poll-{}", issue.key),
                is_echo: false,
                occurred_at,
            }
        })
        .collect();

    Ok(events)
}

/// Parse a single Jira issue JSON object into a `CanonicalStory`. Maps
/// `fields.summary` to `title`, `fields.status.statusCategory.key` to status,
/// and uses the issue key as `external_id`.
pub fn parse_issue(json: &str) -> anyhow::Result<CanonicalStory> {
    let issue: JiraIssue =
        serde_json::from_str(json).map_err(|e| anyhow::anyhow!("parse_issue: {e}"))?;

    let category = &issue.fields.status.status_category.key;
    let status = category_to_status(category);

    // Extract description text from the ADF `description` field if present.
    // ADF is deeply nested; we do a best-effort extraction of the raw JSON string.
    // A full ADF-to-text extractor is out of scope for this slice; the raw JSON
    // serves as an addressable description until a richer extractor is added.
    let description = match &issue.fields.description {
        Some(v) => v.to_string(),
        None => String::new(),
    };

    let created_by = issue
        .fields
        .creator
        .as_ref()
        .and_then(|u| u.email_address.clone().or_else(|| u.display_name.clone()))
        .unwrap_or_else(|| "unknown".to_string());

    // Derive the human-navigable browse URL from the REST API self URL.
    // The self URL is `https://<host>/rest/api/3/issue/<id>`; the browse URL is
    // `https://<host>/browse/<key>`. Extract the origin by splitting on "/rest/".
    let browse_url = issue
        .self_url
        .split_once("/rest/")
        .map(|(origin, _)| format!("{}/browse/{}", origin, issue.key))
        .unwrap_or_else(|| issue.self_url.clone());

    Ok(CanonicalStory {
        id: issue.key.clone(),
        external_ref: Some(ExternalRef {
            provider: Provider::Jira,
            external_id: issue.key.clone(),
            url: browse_url,
            revision: None,
        }),
        title: issue.fields.summary,
        description,
        status,
        created_by,
    })
}

// ── JiraProvider ──────────────────────────────────────────────────────────────

/// Jira adapter implementing `WorkItemProvider`. Parameterized over the HTTP
/// transport so it can be unit-tested with `FakeTransport` without network.
pub struct JiraProvider<T: HttpTransport> {
    config: JiraConfig,
    transport: T,
}

impl<T: HttpTransport> JiraProvider<T> {
    /// Construct a new Jira provider with the given config and transport.
    pub fn new(config: JiraConfig, transport: T) -> Self {
        Self { config, transport }
    }

    fn issue_url(&self, key: &str) -> String {
        format!("{}/rest/api/3/issue/{}", self.config.base_url, key)
    }

    fn transitions_url(&self, key: &str) -> String {
        format!(
            "{}/rest/api/3/issue/{}/transitions",
            self.config.base_url, key
        )
    }

    fn comment_url(&self, key: &str) -> String {
        format!("{}/rest/api/3/issue/{}/comment", self.config.base_url, key)
    }

    fn search_url(&self, jql: &str) -> String {
        // Use query-param JQL (GET form) for simplicity; URL-encode the JQL string.
        let encoded = url_encode(jql);
        format!(
            "{}/rest/api/3/search?jql={}&fields=summary,status,updated,creator",
            self.config.base_url, encoded
        )
    }
}

/// Minimal URL-percent-encoder for the characters that break JQL in a query
/// string. Encodes space, `"`, `+`, `&`, `=`, `#`, `%`, plus control chars.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b'>'
            | b'<'
            | b'='
            | b'!'
            | b',' => out.push(b as char),
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}

#[async_trait]
impl<T: HttpTransport> WorkItemProvider for JiraProvider<T> {
    fn kind(&self) -> Provider {
        Provider::Jira
    }

    async fn ingest_story(&self, reference: &ExternalRef) -> anyhow::Result<CanonicalStory> {
        let url = self.issue_url(&reference.external_id);
        let resp = self.transport.get(&url).await?;
        if resp.status < 200 || resp.status >= 300 {
            anyhow::bail!(
                "Jira GET issue {}: HTTP {} {}",
                reference.external_id,
                resp.status,
                resp.body
            );
        }
        parse_issue(&resp.body)
    }

    async fn push_status(
        &self,
        reference: &ExternalRef,
        report: &FeatureStatusReport,
    ) -> anyhow::Result<()> {
        let target_category = status_to_category(report.status);

        // Step 1: fetch available transitions.
        let trans_url = self.transitions_url(&reference.external_id);
        let trans_resp = self.transport.get(&trans_url).await?;
        if trans_resp.status < 200 || trans_resp.status >= 300 {
            anyhow::bail!(
                "Jira GET transitions {}: HTTP {} {}",
                reference.external_id,
                trans_resp.status,
                trans_resp.body
            );
        }

        let raw: JiraTransitionsResponse = serde_json::from_str(&trans_resp.body)
            .map_err(|e| anyhow::anyhow!("parse transitions for {}: {e}", reference.external_id))?;

        let available: Vec<JiraTransition> = raw
            .transitions
            .into_iter()
            .map(|t| JiraTransition {
                id: t.id,
                to_category: t.to.status_category.key,
            })
            .collect();

        // Step 2: pick a legal transition; degrade to a comment if none exists.
        if let Some(transition_id) = pick_transition(&available, target_category) {
            let body = format!(r#"{{"transition":{{"id":"{transition_id}"}}}}"#);
            let post_url = self.transitions_url(&reference.external_id);
            let post_resp = self.transport.post(&post_url, &body).await?;
            if post_resp.status >= 300 {
                anyhow::bail!(
                    "Jira POST transition {} on {}: HTTP {} {}",
                    transition_id,
                    reference.external_id,
                    post_resp.status,
                    post_resp.body
                );
            }
        } else {
            // No legal transition: degrade to a comment noting the intended status.
            let note = format!(
                "Camerata intended to transition this issue to category '{}' (status: {:?}), \
                 but no legal transition was available. Please update the status manually.",
                target_category, report.status
            );
            let comment_adf = adf_comment(&note);
            let comment_body = format!(r#"{{"body":{comment_adf}}}"#);
            let comment_url = self.comment_url(&reference.external_id);
            let post_resp = self.transport.post(&comment_url, &comment_body).await?;
            if post_resp.status >= 300 {
                anyhow::bail!(
                    "Jira POST degrade-comment on {}: HTTP {} {}",
                    reference.external_id,
                    post_resp.status,
                    post_resp.body
                );
            }
        }

        // Step 3: always post the rollup status comment (provenance trail).
        let rollup_adf = status_rollup_adf(report);
        let rollup_body = format!(r#"{{"body":{rollup_adf}}}"#);
        let comment_url = self.comment_url(&reference.external_id);
        let rollup_resp = self.transport.post(&comment_url, &rollup_body).await?;
        if rollup_resp.status >= 300 {
            anyhow::bail!(
                "Jira POST rollup comment on {}: HTTP {} {}",
                reference.external_id,
                rollup_resp.status,
                rollup_resp.body
            );
        }

        Ok(())
    }

    async fn post_clarifying_questions(
        &self,
        reference: &ExternalRef,
        questions: &[String],
    ) -> anyhow::Result<String> {
        let comment_adf = adf_clarifying_questions(questions);
        let body = format!(r#"{{"body":{comment_adf}}}"#);
        let url = self.comment_url(&reference.external_id);
        let resp = self.transport.post(&url, &body).await?;
        if resp.status < 200 || resp.status >= 300 {
            anyhow::bail!(
                "Jira POST clarifying questions on {}: HTTP {} {}",
                reference.external_id,
                resp.status,
                resp.body
            );
        }
        // Parse the comment id from the response.
        let created: JiraCommentCreated = serde_json::from_str(&resp.body).map_err(|e| {
            anyhow::anyhow!(
                "parse comment response for {}: {e} (body: {})",
                reference.external_id,
                resp.body
            )
        })?;
        Ok(created.id)
    }

    async fn poll(
        &self,
        cursor: Option<&str>,
    ) -> anyhow::Result<(Vec<InboundWorkItemEvent>, String)> {
        let jql = jql_updated_since(cursor);
        let url = self.search_url(&jql);
        let resp = self.transport.get(&url).await?;
        if resp.status < 200 || resp.status >= 300 {
            anyhow::bail!("Jira poll GET: HTTP {} {}", resp.status, resp.body);
        }

        let events = parse_search_results(&resp.body)?;

        // Derive the next cursor from the maximum `updated` value in the results.
        // If no results, echo the incoming cursor (or the default lookback string).
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

    fn test_config() -> JiraConfig {
        JiraConfig {
            base_url: "https://example.atlassian.net".to_string(),
            email: "user@example.com".to_string(),
            api_token: "mytoken".to_string(),
        }
    }

    fn jira_ref(key: &str) -> ExternalRef {
        ExternalRef {
            provider: Provider::Jira,
            external_id: key.to_string(),
            url: format!("https://example.atlassian.net/browse/{key}"),
            revision: None,
        }
    }

    fn make_report(status: FeatureStatus) -> FeatureStatusReport {
        FeatureStatusReport {
            status,
            pr_links: vec![],
            gate_results: vec![],
            sign_off: None,
            provenance_url: "https://camerata.internal/provenance/PROJ-1".to_string(),
        }
    }

    // ── base64 ─────────────────────────────────────────────────────────────

    #[test]
    fn base64_encode_known_vectors() {
        // RFC 4648 test vectors.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn auth_header_is_basic_base64() {
        let cfg = JiraConfig {
            base_url: "https://x.atlassian.net".to_string(),
            email: "alice@example.com".to_string(),
            api_token: "secret".to_string(),
        };
        let header = cfg.auth_header();
        assert!(header.starts_with("Basic "), "must start with 'Basic '");
        // Verify the encoded payload: base64("alice@example.com:secret")
        let encoded = &header["Basic ".len()..];
        assert_eq!(encoded, base64_encode(b"alice@example.com:secret"));
    }

    // ── status_to_category ─────────────────────────────────────────────────

    #[test]
    fn status_to_category_full_mapping() {
        assert_eq!(status_to_category(FeatureStatus::Intake), "new");
        assert_eq!(
            status_to_category(FeatureStatus::Investigating),
            "indeterminate"
        );
        assert_eq!(
            status_to_category(FeatureStatus::AwaitingClarification),
            "indeterminate"
        );
        assert_eq!(status_to_category(FeatureStatus::Planned), "indeterminate");
        assert_eq!(
            status_to_category(FeatureStatus::Executing),
            "indeterminate"
        );
        assert_eq!(status_to_category(FeatureStatus::Gating), "indeterminate");
        assert_eq!(
            status_to_category(FeatureStatus::AwaitingQa),
            "indeterminate"
        );
        assert_eq!(status_to_category(FeatureStatus::SignedOff), "done");
        assert_eq!(status_to_category(FeatureStatus::Done), "done");
        assert_eq!(status_to_category(FeatureStatus::Blocked), "indeterminate");
        assert_eq!(status_to_category(FeatureStatus::Rejected), "indeterminate");
    }

    // ── category_to_status ─────────────────────────────────────────────────

    #[test]
    fn category_to_status_known_keys() {
        assert_eq!(category_to_status("new"), FeatureStatus::Intake);
        assert_eq!(
            category_to_status("indeterminate"),
            FeatureStatus::Investigating
        );
        assert_eq!(category_to_status("done"), FeatureStatus::Done);
    }

    #[test]
    fn category_to_status_unknown_sentinel_is_investigating() {
        // "undefined" is the sentinel (id 1); any other unknown key is also safe.
        assert_eq!(
            category_to_status("undefined"),
            FeatureStatus::Investigating
        );
        assert_eq!(
            category_to_status("totally_new_future_key"),
            FeatureStatus::Investigating
        );
        assert_eq!(category_to_status(""), FeatureStatus::Investigating);
    }

    // ── round-trip ─────────────────────────────────────────────────────────

    #[test]
    fn status_category_round_trip_for_stable_keys() {
        // For keys that have a 1:1 canonical mapping, the round-trip must hold.
        let pairs = [
            (FeatureStatus::Intake, "new"),
            (FeatureStatus::Done, "done"),
        ];
        for (status, expected_cat) in pairs {
            let cat = status_to_category(status);
            assert_eq!(cat, expected_cat);
            let back = category_to_status(cat);
            assert_eq!(back, status);
        }
    }

    // ── adf_comment ────────────────────────────────────────────────────────

    #[test]
    fn adf_comment_is_valid_json_with_text() {
        let text = "Hello, PO. Please review this.";
        let adf = adf_comment(text);
        let v: serde_json::Value =
            serde_json::from_str(&adf).expect("adf_comment must produce valid JSON");
        assert_eq!(v["version"], 1);
        assert_eq!(v["type"], "doc");
        let content = &v["content"][0];
        assert_eq!(content["type"], "paragraph");
        let text_node = &content["content"][0];
        assert_eq!(text_node["type"], "text");
        assert_eq!(text_node["text"], text);
    }

    #[test]
    fn adf_comment_escapes_double_quotes() {
        let text = r#"He said "yes" and it worked."#;
        let adf = adf_comment(text);
        let v: serde_json::Value =
            serde_json::from_str(&adf).expect("must be valid JSON after escaping");
        let text_back = v["content"][0]["content"][0]["text"]
            .as_str()
            .expect("text node");
        assert_eq!(text_back, text);
    }

    // ── adf_clarifying_questions ───────────────────────────────────────────

    #[test]
    fn adf_clarifying_questions_is_valid_json_with_all_questions() {
        let questions = vec![
            "What is the target audience?".to_string(),
            "Are notifications opt-in or opt-out?".to_string(),
        ];
        let adf = adf_clarifying_questions(&questions);
        let v: serde_json::Value = serde_json::from_str(&adf).expect("must be valid JSON");
        assert_eq!(v["version"], 1);
        assert_eq!(v["type"], "doc");

        // First child: intro paragraph.
        let intro = &v["content"][0];
        assert_eq!(intro["type"], "paragraph");

        // Second child: bulletList.
        let bullet_list = &v["content"][1];
        assert_eq!(bullet_list["type"], "bulletList");
        let items = bullet_list["content"].as_array().expect("content array");
        assert_eq!(items.len(), questions.len());
        for (i, q) in questions.iter().enumerate() {
            let text = items[i]["content"][0]["content"][0]["text"]
                .as_str()
                .expect("list item text");
            assert_eq!(text, q.as_str());
        }
    }

    #[test]
    fn adf_clarifying_questions_intro_mentions_product_owner() {
        let questions = vec!["Q1".to_string()];
        let adf = adf_clarifying_questions(&questions);
        let v: serde_json::Value = serde_json::from_str(&adf).unwrap();
        let intro_text = v["content"][0]["content"][0]["text"]
            .as_str()
            .expect("intro text");
        assert!(
            intro_text.to_lowercase().contains("product owner"),
            "intro must mention Product Owner, got: {intro_text}"
        );
    }

    // ── jql_updated_since ──────────────────────────────────────────────────

    #[test]
    fn jql_none_uses_lookback() {
        let jql = jql_updated_since(None);
        assert!(
            jql.contains("-7d"),
            "None cursor should use a relative lookback: {jql}"
        );
        assert!(jql.contains("ORDER BY"), "should include ordering: {jql}");
    }

    #[test]
    fn jql_some_embeds_timestamp() {
        let ts = "2026-06-01T00:00:00.000+0000";
        let jql = jql_updated_since(Some(ts));
        assert!(jql.contains(ts), "must embed the timestamp: {jql}");
        assert!(jql.contains("ORDER BY"), "should include ordering: {jql}");
    }

    // ── pick_transition ────────────────────────────────────────────────────

    #[test]
    fn pick_transition_returns_matching_id() {
        let transitions = vec![
            JiraTransition {
                id: "11".to_string(),
                to_category: "new".to_string(),
            },
            JiraTransition {
                id: "21".to_string(),
                to_category: "indeterminate".to_string(),
            },
            JiraTransition {
                id: "31".to_string(),
                to_category: "done".to_string(),
            },
        ];
        assert_eq!(
            pick_transition(&transitions, "done"),
            Some("31".to_string())
        );
        assert_eq!(pick_transition(&transitions, "new"), Some("11".to_string()));
        assert_eq!(
            pick_transition(&transitions, "indeterminate"),
            Some("21".to_string())
        );
    }

    #[test]
    fn pick_transition_returns_none_when_no_match() {
        // When no legal transition leads to the desired category, the caller
        // MUST degrade to posting a comment (never force an illegal transition).
        let transitions = vec![JiraTransition {
            id: "21".to_string(),
            to_category: "indeterminate".to_string(),
        }];
        assert_eq!(pick_transition(&transitions, "done"), None);
        assert_eq!(pick_transition([].as_slice(), "done"), None);
    }

    #[test]
    fn pick_transition_picks_first_when_multiple_match() {
        let transitions = vec![
            JiraTransition {
                id: "10".to_string(),
                to_category: "done".to_string(),
            },
            JiraTransition {
                id: "20".to_string(),
                to_category: "done".to_string(),
            },
        ];
        assert_eq!(
            pick_transition(&transitions, "done"),
            Some("10".to_string())
        );
    }

    // ── parse_search_results ───────────────────────────────────────────────

    const SEARCH_JSON: &str = r#"{
        "expand": "schema,names",
        "startAt": 0,
        "maxResults": 50,
        "total": 2,
        "issues": [
            {
                "expand": "operations",
                "id": "10001",
                "self": "https://example.atlassian.net/rest/api/3/issue/10001",
                "key": "PROJ-1",
                "fields": {
                    "summary": "Build login page",
                    "status": {
                        "statusCategory": {
                            "id": 2,
                            "key": "new",
                            "name": "To Do"
                        }
                    },
                    "updated": "2026-06-01T10:00:00.000+0000",
                    "creator": {
                        "emailAddress": "alice@example.com",
                        "displayName": "Alice"
                    }
                }
            },
            {
                "expand": "operations",
                "id": "10002",
                "self": "https://example.atlassian.net/rest/api/3/issue/10002",
                "key": "PROJ-2",
                "fields": {
                    "summary": "Dark mode support",
                    "status": {
                        "statusCategory": {
                            "id": 4,
                            "key": "indeterminate",
                            "name": "In Progress"
                        }
                    },
                    "updated": "2026-06-02T12:00:00.000+0000",
                    "creator": {
                        "emailAddress": "bob@example.com",
                        "displayName": "Bob"
                    }
                }
            }
        ]
    }"#;

    #[test]
    fn parse_search_results_two_issues() {
        let events = parse_search_results(SEARCH_JSON).expect("parse must succeed");
        assert_eq!(events.len(), 2);

        let e0 = &events[0];
        assert_eq!(e0.reference.external_id, "PROJ-1");
        assert_eq!(e0.reference.provider, Provider::Jira);
        assert_eq!(e0.kind, InboundKind::Updated);
        assert_eq!(e0.status, Some(FeatureStatus::Intake));
        assert_eq!(e0.title, Some("Build login page".to_string()));
        assert_eq!(e0.occurred_at, "2026-06-01T10:00:00.000+0000");

        let e1 = &events[1];
        assert_eq!(e1.reference.external_id, "PROJ-2");
        assert_eq!(e1.status, Some(FeatureStatus::Investigating));
        assert_eq!(e1.occurred_at, "2026-06-02T12:00:00.000+0000");
    }

    // ── parse_issue ────────────────────────────────────────────────────────

    const ISSUE_JSON: &str = r#"{
        "expand": "renderedFields",
        "id": "10001",
        "self": "https://example.atlassian.net/rest/api/3/issue/10001",
        "key": "PROJ-1",
        "fields": {
            "summary": "Build login page",
            "description": {
                "version": 1,
                "type": "doc",
                "content": [
                    {"type": "paragraph", "content": [{"type": "text", "text": "We need a login page."}]}
                ]
            },
            "status": {
                "statusCategory": {
                    "id": 4,
                    "key": "indeterminate",
                    "name": "In Progress"
                }
            },
            "updated": "2026-06-01T10:00:00.000+0000",
            "creator": {
                "emailAddress": "alice@example.com",
                "displayName": "Alice"
            }
        }
    }"#;

    #[test]
    fn parse_issue_maps_to_canonical_story() {
        let story = parse_issue(ISSUE_JSON).expect("parse must succeed");
        assert_eq!(story.id, "PROJ-1");
        assert_eq!(story.title, "Build login page");
        assert_eq!(story.status, FeatureStatus::Investigating);
        assert_eq!(story.created_by, "alice@example.com");
        let ext = story.external_ref.expect("must have external_ref");
        assert_eq!(ext.provider, Provider::Jira);
        assert_eq!(ext.external_id, "PROJ-1");
        assert!(
            ext.url.contains("PROJ-1"),
            "url must reference the issue key"
        );
        // Description should be non-empty (raw ADF JSON or text).
        assert!(
            !story.description.is_empty(),
            "description must not be empty"
        );
    }

    #[test]
    fn parse_issue_null_description_is_empty_string() {
        let json = r#"{
            "id": "10002",
            "self": "https://example.atlassian.net/rest/api/3/issue/10002",
            "key": "PROJ-2",
            "fields": {
                "summary": "No description issue",
                "description": null,
                "status": {"statusCategory": {"id": 3, "key": "done", "name": "Done"}},
                "updated": "2026-06-01T10:00:00.000+0000",
                "creator": {"emailAddress": "bob@example.com", "displayName": "Bob"}
            }
        }"#;
        let story = parse_issue(json).expect("parse must succeed");
        assert_eq!(story.description, "");
        assert_eq!(story.status, FeatureStatus::Done);
    }

    // ── status_rollup_adf ─────────────────────────────────────────────────

    #[test]
    fn status_rollup_adf_is_valid_json() {
        let report = FeatureStatusReport {
            status: FeatureStatus::Done,
            pr_links: vec![PrLink {
                repo: "org/repo".to_string(),
                url: "https://github.com/org/repo/pull/1".to_string(),
                title: "Add login page".to_string(),
                status: PrStatus::Merged,
            }],
            gate_results: vec![GateResult {
                rule_id: "GATE-001".to_string(),
                result: GateOutcome::Pass,
                message: Some("All checks passed.".to_string()),
            }],
            sign_off: Some(SignOff {
                by: "alice".to_string(),
                at: "2026-06-01T10:00:00Z".to_string(),
            }),
            provenance_url: "https://camerata.internal/provenance/PROJ-1".to_string(),
        };
        let adf = status_rollup_adf(&report);
        let v: serde_json::Value =
            serde_json::from_str(&adf).expect("status_rollup_adf must produce valid JSON");
        assert_eq!(v["version"], 1);
        assert_eq!(v["type"], "doc");
        // Content is non-empty.
        assert!(
            v["content"]
                .as_array()
                .map(|a| !a.is_empty())
                .unwrap_or(false),
            "content array must not be empty"
        );
    }

    #[test]
    fn status_rollup_adf_contains_provenance_url() {
        let report = make_report(FeatureStatus::Executing);
        let adf = status_rollup_adf(&report);
        assert!(
            adf.contains("camerata.internal/provenance"),
            "rollup ADF must reference the provenance URL"
        );
    }

    // ── JiraProvider via FakeTransport ─────────────────────────────────────

    #[tokio::test]
    async fn provider_kind_is_jira() {
        let transport = FakeTransport::new();
        let provider = JiraProvider::new(test_config(), transport);
        assert_eq!(provider.kind(), Provider::Jira);
    }

    #[tokio::test]
    async fn ingest_story_calls_get_and_returns_canonical_story() {
        let transport = FakeTransport::new().on("GET", "/issue/PROJ-1", 200, ISSUE_JSON);
        let provider = JiraProvider::new(test_config(), transport);
        let story = provider.ingest_story(&jira_ref("PROJ-1")).await.unwrap();
        assert_eq!(story.id, "PROJ-1");
        assert_eq!(story.title, "Build login page");
        assert_eq!(story.status, FeatureStatus::Investigating);

        let calls = provider.transport.recorded_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "GET");
        assert!(calls[0].1.contains("PROJ-1"), "GET must target PROJ-1");
    }

    #[tokio::test]
    async fn push_status_with_legal_transition_posts_transition_then_comment() {
        let transitions_json = r#"{
            "transitions": [
                {"id": "31", "to": {"statusCategory": {"key": "done"}}},
                {"id": "11", "to": {"statusCategory": {"key": "new"}}}
            ]
        }"#;
        let transport = FakeTransport::new()
            .on("GET", "/transitions", 200, transitions_json)
            .on("POST", "/transitions", 201, "{}")
            .on("POST", "/comment", 201, r#"{"id":"100"}"#);

        let provider = JiraProvider::new(test_config(), transport);
        let report = FeatureStatusReport {
            status: FeatureStatus::Done,
            pr_links: vec![],
            gate_results: vec![],
            sign_off: None,
            provenance_url: "https://camerata.internal/provenance/PROJ-1".to_string(),
        };

        provider
            .push_status(&jira_ref("PROJ-1"), &report)
            .await
            .unwrap();

        let calls = provider.transport.recorded_calls();
        // Expect: GET transitions, POST transition, POST rollup comment.
        let methods: Vec<&str> = calls.iter().map(|(m, _, _)| m.as_str()).collect();
        assert_eq!(
            methods,
            vec!["GET", "POST", "POST"],
            "expected GET transitions, POST transition, POST rollup comment"
        );

        // The first POST should target the transitions URL and contain the transition id.
        let transition_body = &calls[1].2;
        assert!(
            transition_body.contains("31"),
            "transition body must contain the transition id '31'"
        );
    }

    #[tokio::test]
    async fn push_status_without_legal_transition_degrades_to_comment() {
        // No transition into "done" available.
        let transitions_json = r#"{
            "transitions": [
                {"id": "11", "to": {"statusCategory": {"key": "new"}}},
                {"id": "21", "to": {"statusCategory": {"key": "indeterminate"}}}
            ]
        }"#;
        let transport = FakeTransport::new()
            .on("GET", "/transitions", 200, transitions_json)
            // Two comment POSTs expected: degrade comment + rollup.
            .on("POST", "/comment", 201, r#"{"id":"200"}"#);

        let provider = JiraProvider::new(test_config(), transport);
        let report = FeatureStatusReport {
            status: FeatureStatus::Done,
            pr_links: vec![],
            gate_results: vec![],
            sign_off: None,
            provenance_url: "https://camerata.internal/provenance/PROJ-1".to_string(),
        };

        provider
            .push_status(&jira_ref("PROJ-1"), &report)
            .await
            .unwrap();

        let calls = provider.transport.recorded_calls();
        let posts: Vec<_> = calls.iter().filter(|(m, _, _)| m == "POST").collect();
        // Must have at least one POST to /comment (the degrade comment).
        assert!(
            !posts.is_empty(),
            "must POST a degrade comment when no transition is available"
        );
        // The degrade comment must say something about the intended status/category.
        let degrade_body = &posts[0].2;
        assert!(
            degrade_body.contains("done") || degrade_body.contains("Done"),
            "degrade comment must mention the target category: {degrade_body}"
        );
    }

    #[tokio::test]
    async fn post_clarifying_questions_posts_adf_comment() {
        let response_json = r#"{"id": "comment-42", "self": "https://..."}"#;
        let transport = FakeTransport::new().on("POST", "/comment", 201, response_json);

        let provider = JiraProvider::new(test_config(), transport);
        let questions = vec![
            "Which date format?".to_string(),
            "Are exports opt-in?".to_string(),
        ];
        let comment_id = provider
            .post_clarifying_questions(&jira_ref("PROJ-1"), &questions)
            .await
            .unwrap();

        assert_eq!(comment_id, "comment-42");

        let calls = provider.transport.recorded_calls();
        assert_eq!(calls.len(), 1);
        let (method, url, body) = &calls[0];
        assert_eq!(method, "POST");
        assert!(url.contains("/comment"), "must post to comment endpoint");

        // The body must be valid JSON containing the ADF doc with the questions.
        let v: serde_json::Value = serde_json::from_str(body).expect("body must be valid JSON");
        // Outer wrapper has a "body" key holding the ADF doc.
        let adf_doc = &v["body"];
        assert_eq!(adf_doc["version"], 1);
        assert_eq!(adf_doc["type"], "doc");
        // The raw body string must contain the question text.
        assert!(
            body.contains("Which date format"),
            "ADF body must contain first question"
        );
        assert!(
            body.contains("Are exports opt-in"),
            "ADF body must contain second question"
        );
    }

    #[tokio::test]
    async fn poll_returns_events_and_next_cursor() {
        let transport = FakeTransport::new().on("GET", "/search", 200, SEARCH_JSON);
        let provider = JiraProvider::new(test_config(), transport);

        let (events, next_cursor) = provider.poll(None).await.unwrap();

        assert_eq!(events.len(), 2, "must return 2 events from search results");
        assert_eq!(events[0].reference.external_id, "PROJ-1");
        assert_eq!(events[1].reference.external_id, "PROJ-2");

        // Next cursor is the max updated across the two issues.
        assert_eq!(
            next_cursor, "2026-06-02T12:00:00.000+0000",
            "cursor must be the max updated timestamp"
        );
    }

    #[tokio::test]
    async fn poll_with_cursor_embeds_cursor_in_jql_url() {
        let transport = FakeTransport::new().on("GET", "/search", 200, r#"{"issues":[]}"#);
        let provider = JiraProvider::new(test_config(), transport);

        let cursor = "2026-06-01T00:00:00.000+0000";
        let (events, _) = provider.poll(Some(cursor)).await.unwrap();
        assert!(events.is_empty(), "empty search result returns no events");

        let calls = provider.transport.recorded_calls();
        let get_url = &calls[0].1;
        // The cursor timestamp must appear (URL-encoded) in the search URL.
        assert!(
            get_url.contains("2026"),
            "GET URL must embed the cursor year: {get_url}"
        );
    }

    #[tokio::test]
    async fn clarify_bridge_round_trip_via_jira_provider() {
        // 1. post_clarifying_questions -> POST /comment
        // 2. poll -> GET /search returns a comment-containing update (simulated as an Updated event)
        let comment_response = r#"{"id": "comment-99"}"#;
        let poll_response_with_answer = r#"{
            "issues": [{
                "id": "10001",
                "self": "https://example.atlassian.net/rest/api/3/issue/10001",
                "key": "PROJ-1",
                "fields": {
                    "summary": "Build login page",
                    "status": {"statusCategory": {"id": 4, "key": "indeterminate", "name": "In Progress"}},
                    "updated": "2026-06-03T09:00:00.000+0000",
                    "creator": {"emailAddress": "po@example.com", "displayName": "PO"}
                }
            }]
        }"#;

        let transport = FakeTransport::new()
            .on("POST", "/comment", 201, comment_response)
            .on("GET", "/search", 200, poll_response_with_answer);

        let provider = JiraProvider::new(test_config(), transport);

        // Step 1: post clarifying questions.
        let questions = vec!["Should exports use UTC?".to_string()];
        let comment_id = provider
            .post_clarifying_questions(&jira_ref("PROJ-1"), &questions)
            .await
            .unwrap();
        assert_eq!(comment_id, "comment-99");

        // Step 2: poll returns the PO's update as an inbound event.
        let (events, _cursor) = provider.poll(None).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].reference.external_id, "PROJ-1");

        // Verify FakeTransport recorded both calls.
        let calls = provider.transport.recorded_calls();
        let post_calls: Vec<_> = calls.iter().filter(|(m, _, _)| m == "POST").collect();
        assert_eq!(post_calls.len(), 1, "must have one POST for the comment");
        let (_, post_url, post_body) = &post_calls[0];
        assert!(
            post_url.contains("/comment"),
            "POST must target the comment endpoint"
        );
        assert!(
            post_body.contains("Should exports use UTC"),
            "POST body must contain the question"
        );
    }
}
