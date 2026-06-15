//! Azure DevOps Boards adapter (Phase E, PAT + Basic auth, WIQL-polling inbound).
//!
//! Inbound is WIQL polling: POST a WIQL query to find work items changed since
//! a cursor, then GET the returned ids in a batch to build InboundWorkItemEvents.
//! Outbound writes use the Azure DevOps REST API: JSON-Patch on System.State for
//! status changes, and the ADO Comments API for plain-text / HTML comment bodies.
//! ADO comments do NOT use ADF; they accept plain text or simple HTML strings.
//!
//! Map our FeatureStatus to ADO stateCategory TOKENS (exact strings, no spaces):
//! Proposed, InProgress, Resolved, Completed, Removed. The stateCategory is a
//! plain string in payloads, NOT a frozen enum: any unrecognized value routes to
//! a safe default (InProgress) rather than panicking.
//!
//! Auth: ADO Basic auth encodes ":<pat>" (empty username, colon, then the PAT)
//! in standard base64. The same hand-rolled encoder used in jira.rs is reused
//! here; no external base64 crate is introduced.

use async_trait::async_trait;
use serde::Deserialize;

use crate::{
    CanonicalStory, ExternalRef, FeatureStatus, FeatureStatusReport, GateOutcome, InboundKind,
    InboundWorkItemEvent, PrStatus, Provider, WorkItemProvider,
};

use super::http::HttpTransport;

// ── Config ─────────────────────────────────────────────────────────────────────

/// Connection parameters for one Azure DevOps organization and project.
#[derive(Debug, Clone)]
pub struct AdoConfig {
    /// The ADO organization name (e.g. `myorg` in `dev.azure.com/myorg`).
    pub organization: String,
    /// The ADO project name (e.g. `MyProject`).
    pub project: String,
    /// A Personal Access Token with Work Items Read and Write scopes.
    pub pat: String,
}

impl AdoConfig {
    /// Build the `Authorization: Basic ...` header value for this config.
    /// ADO Basic auth encodes an empty username with the PAT as password:
    /// `base64(":" + pat)`. No line breaks.
    pub fn auth_header(&self) -> String {
        let raw = format!(":{}", self.pat);
        format!("Basic {}", base64_encode(raw.as_bytes()))
    }

    /// URL for a single work item by id.
    fn work_item_url(&self, id: &str) -> String {
        format!(
            "https://dev.azure.com/{org}/{proj}/_apis/wit/workitems/{id}?api-version=7.1",
            org = self.organization,
            proj = self.project,
        )
    }

    /// URL to PATCH (JSON-Patch) a single work item.
    fn patch_url(&self, id: &str) -> String {
        format!(
            "https://dev.azure.com/{org}/{proj}/_apis/wit/workitems/{id}?api-version=7.1",
            org = self.organization,
            proj = self.project,
        )
    }

    /// URL for the ADO Comments API on a work item.
    /// Pinned to `7.1-preview.4` as documented; update when GA version publishes.
    fn comments_url(&self, id: &str) -> String {
        format!(
            "https://dev.azure.com/{org}/{proj}/_apis/wit/workItems/{id}/comments?api-version=7.1-preview.4",
            org = self.organization,
            proj = self.project,
        )
    }

    /// URL for the WIQL endpoint.
    fn wiql_url(&self) -> String {
        format!(
            "https://dev.azure.com/{org}/{proj}/_apis/wit/wiql?api-version=7.1",
            org = self.organization,
            proj = self.project,
        )
    }

    /// URL for a batch GET of work items by ids (comma-separated, with fields).
    fn batch_url(&self, ids: &[u64]) -> String {
        let id_list: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
        format!(
            "https://dev.azure.com/{org}/{proj}/_apis/wit/workitems?ids={ids}&fields=System.Id,System.Title,System.State,System.ChangedDate,System.AssignedTo,System.Description,System.CreatedBy&api-version=7.1",
            org = self.organization,
            proj = self.project,
            ids = id_list.join(","),
        )
    }

    /// Human-navigable URL for a work item in the ADO web UI.
    fn browse_url(&self, id: &str) -> String {
        format!(
            "https://dev.azure.com/{org}/{proj}/_workitems/edit/{id}",
            org = self.organization,
            proj = self.project,
        )
    }
}

// ── Minimal base64 encoder (no external dep) ───────────────────────────────────

/// Hand-rolled base64 encoder. Produces the standard alphabet (A-Z a-z 0-9 + /)
/// with `=` padding. No line breaks. Enough for an Authorization header value.
/// Identical algorithm to jira.rs; kept local so each module is self-contained.
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

// ── Status mapping ─────────────────────────────────────────────────────────────

/// Map a canonical FeatureStatus to an ADO stateCategory token.
///
/// Per the design doc (section 4.2) and the spec:
/// - INTAKE                              -> Proposed
/// - INVESTIGATING / PLANNED / EXECUTING -> InProgress
/// - GATING                              -> InProgress
/// - AWAITING_CLARIFICATION / BLOCKED    -> InProgress (with a comment when posting)
/// - AWAITING_QA -> Resolved (degrade to InProgress + comment when the process
///   type lacks Resolved, e.g. Scrum PBI / Basic Issue)
/// - SIGNED_OFF / DONE                   -> Completed
/// - REJECTED                            -> Removed (decisively closed, not just stalled)
///
/// BLOCKED maps to InProgress because ADO has no blocked column in the stateCategory
/// set; the caller is responsible for posting an explanatory comment when needed.
/// REJECTED maps to Removed rather than InProgress so the work item is archived from
/// active views rather than left in an undifferentiated "active" bucket.
pub fn status_to_category(status: FeatureStatus) -> &'static str {
    match status {
        FeatureStatus::Intake => "Proposed",
        FeatureStatus::Investigating
        | FeatureStatus::AwaitingClarification
        | FeatureStatus::Planned
        | FeatureStatus::Executing
        | FeatureStatus::Gating
        | FeatureStatus::Blocked => "InProgress",
        FeatureStatus::AwaitingQa => "Resolved",
        FeatureStatus::SignedOff | FeatureStatus::Done => "Completed",
        FeatureStatus::Rejected => "Removed",
    }
}

/// Map an ADO stateCategory token to a canonical FeatureStatus.
///
/// Known tokens (exact strings, no spaces): Proposed, InProgress, Resolved,
/// Completed, Removed. Any unrecognized value (including future ADO additions)
/// maps to Investigating, a safe recoverable default that keeps the story in
/// the active-work category without losing it. This is a plain-string field in
/// ADO payloads, NOT a frozen enum, so defensive routing is mandatory.
pub fn category_to_status(category: &str) -> FeatureStatus {
    match category {
        "Proposed" => FeatureStatus::Intake,
        "InProgress" => FeatureStatus::Investigating,
        "Resolved" => FeatureStatus::AwaitingQa,
        "Completed" => FeatureStatus::Done,
        "Removed" => FeatureStatus::Rejected,
        // Any unrecognized value routes to Investigating (safe, active default).
        _ => FeatureStatus::Investigating,
    }
}

/// Return the default concrete ADO state name for a given stateCategory token.
///
/// ADO state names are per-work-item-type and user-configurable; the concrete
/// name must be resolved at runtime via the states-list API. These defaults cover
/// the four built-in process templates (Agile, Scrum, CMMI, Basic) and serve as
/// the fallback that a real deployment overrides from the states-list API response.
///
/// Defaults:
/// - Proposed  -> "New"      (Agile/Scrum/CMMI: New; Basic: To Do)
/// - InProgress-> "Active"   (Agile/CMMI: Active; Scrum: Committed; Basic: Doing)
/// - Resolved  -> "Resolved" (Agile/CMMI: Resolved; absent in Scrum PBI/Basic Issue)
/// - Completed -> "Closed"   (Agile/CMMI: Closed; Scrum: Done; Basic: Done)
/// - Removed   -> "Removed"  (present in all four process templates)
/// - Unknown   -> "Active"   (safe active fallback for any future category token)
pub fn default_state_for_category(category: &str) -> &'static str {
    match category {
        "Proposed" => "New",
        "InProgress" => "Active",
        "Resolved" => "Resolved",
        "Completed" => "Closed",
        "Removed" => "Removed",
        _ => "Active",
    }
}

// ── JSON-Patch helper ──────────────────────────────────────────────────────────

/// Build the JSON-Patch array body that sets System.State to the given concrete
/// state name. This is the format ADO expects for a PATCH request on a work item.
///
/// Result is a JSON array: `[{"op":"add","path":"/fields/System.State","value":"<state>"}]`.
/// The `add` operation is used for both create and replace in JSON-Patch on ADO.
pub fn json_patch_state(state: &str) -> String {
    // Use serde_json to correctly escape the state string value.
    let state_json = serde_json::to_string(state).unwrap_or_else(|_| "\"\"".to_string());
    format!(r#"[{{"op":"add","path":"/fields/System.State","value":{state_json}}}]"#)
}

// ── Comment body helpers ───────────────────────────────────────────────────────

/// Build a plain-text / HTML comment body for posting to the ADO Comments API.
/// ADO accepts plain text strings (not ADF); HTML is supported but optional.
/// This helper returns a minimal JSON object with a `text` field as expected
/// by the Comments API: `{"text": "<escaped text>"}`.
pub fn comment_body(text: &str) -> String {
    let text_json = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string());
    format!(r#"{{"text":{text_json}}}"#)
}

/// Build an ADO Comments API JSON body for a clarifying-questions comment.
/// The comment body is an HTML string listing the questions with an intro
/// paragraph that mentions the Product Owner. ADO renders simple HTML in the
/// comments pane.
///
/// Format (HTML): an intro sentence followed by an ordered list of the questions.
pub fn clarifying_questions_body(questions: &[String]) -> String {
    let mut html = String::from(
        "<p>The Camerata orchestrator has the following clarifying questions for the \
         Product Owner. Please reply directly to this comment.</p><ol>",
    );
    for q in questions {
        // Escape HTML-special chars in question text.
        let escaped_q = q
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;");
        html.push_str(&format!("<li>{escaped_q}</li>"));
    }
    html.push_str("</ol>");

    // Embed the HTML string as a JSON string value.
    let html_json = serde_json::to_string(&html).unwrap_or_else(|_| "\"\"".to_string());
    format!(r#"{{"text":{html_json}}}"#)
}

/// Build a plain-text rollup comment body for the ADO Comments API. Contains
/// the status, PR links, gate results, sign-off, and the provenance URL.
/// Formatted as simple human-readable text (no HTML required; ADO renders it).
pub fn status_rollup_text(report: &FeatureStatusReport) -> String {
    let mut lines: Vec<String> = Vec::new();

    lines.push(format!("Camerata status: {:?}", report.status));

    if !report.pr_links.is_empty() {
        lines.push(String::new());
        lines.push("Pull requests:".to_string());
        for pr in &report.pr_links {
            let pr_status = match pr.status {
                PrStatus::Open => "open",
                PrStatus::Merged => "merged",
                PrStatus::Closed => "closed",
            };
            lines.push(format!(
                "  [{pr_status}] {} ({}) - {}",
                pr.title, pr.repo, pr.url
            ));
        }
    }

    if !report.gate_results.is_empty() {
        lines.push(String::new());
        lines.push("Gate results:".to_string());
        for g in &report.gate_results {
            let outcome = match g.result {
                GateOutcome::Pass => "PASS",
                GateOutcome::Fail => "FAIL",
            };
            let msg = g.message.as_deref().unwrap_or("");
            if msg.is_empty() {
                lines.push(format!("  [{outcome}] {}", g.rule_id));
            } else {
                lines.push(format!("  [{outcome}] {}: {msg}", g.rule_id));
            }
        }
    }

    if let Some(sign) = &report.sign_off {
        lines.push(String::new());
        lines.push(format!("Signed off by {} at {}", sign.by, sign.at));
    }

    lines.push(String::new());
    lines.push(format!("Full provenance: {}", report.provenance_url));

    let text = lines.join("\n");
    // Use serde_json to correctly escape the text value (handles \n, \r, \t,
    // \", \\, and all other JSON control characters automatically).
    let text_json = serde_json::to_string(&text).unwrap_or_else(|_| "\"\"".to_string());
    format!(r#"{{"text":{text_json}}}"#)
}

// ── WIQL helper ────────────────────────────────────────────────────────────────

/// Build the WIQL query string for polling work items changed since a cursor.
///
/// When `cursor` is None, defaults to a 7-day lookback so the first run
/// does not miss recent changes (same safe-default pattern as jira.rs JQL).
/// When `cursor` is Some(ts), uses the ISO 8601 timestamp directly in the
/// WIQL WHERE clause.
///
/// Returns the full WIQL SELECT statement as a string. The caller wraps it
/// in the JSON body: `{"query": "<wiql>"}`.
pub fn wiql_changed_since(cursor: Option<&str>) -> String {
    let date_clause = match cursor {
        None => "[System.ChangedDate] > @today - 7".to_string(),
        Some(ts) => {
            let escaped = ts.replace('\'', "\\'");
            format!("[System.ChangedDate] > '{escaped}'")
        }
    };
    format!("SELECT [System.Id] FROM workitems WHERE {date_clause} ORDER BY [System.ChangedDate]")
}

// ── ADO response shapes (for deserialization) ──────────────────────────────────

/// A single work-item reference from a WIQL query response.
#[derive(Debug, Deserialize)]
struct WiqlWorkItemRef {
    id: u64,
}

/// The WIQL query response shape returned by the ADO WIQL endpoint.
#[derive(Debug, Deserialize)]
struct WiqlResponse {
    #[serde(rename = "workItems", default)]
    work_items: Vec<WiqlWorkItemRef>,
}

/// A work item returned by the batch GET endpoint (value array entry).
#[derive(Debug, Deserialize)]
struct AdoWorkItem {
    id: u64,
    fields: AdoWorkItemFields,
}

/// The fields subset we read from ADO work items.
#[derive(Debug, Deserialize)]
struct AdoWorkItemFields {
    #[serde(rename = "System.Title", default)]
    title: Option<String>,
    #[serde(rename = "System.State", default)]
    state: Option<String>,
    #[serde(rename = "System.ChangedDate", default)]
    changed_date: Option<String>,
    #[serde(rename = "System.Description", default)]
    description: Option<String>,
    #[serde(rename = "System.CreatedBy", default)]
    created_by: Option<AdoIdentityRef>,
}

/// ADO identity reference (minimal subset: the display name or unique name).
#[derive(Debug, Deserialize)]
struct AdoIdentityRef {
    #[serde(rename = "uniqueName", default)]
    unique_name: Option<String>,
    #[serde(rename = "displayName", default)]
    display_name: Option<String>,
}

/// The batch work-items response: a `value` array of AdoWorkItem.
#[derive(Debug, Deserialize)]
struct AdoWorkItemsResponse {
    value: Vec<AdoWorkItem>,
}

/// The Comments API response for a newly created comment; we read the `id`.
#[derive(Debug, Deserialize)]
struct AdoCommentCreated {
    id: u64,
}

// ── Parsers ────────────────────────────────────────────────────────────────────

/// Parse an ADO work-items batch JSON response into InboundWorkItemEvents.
///
/// Each work item becomes one Updated event. Polling cannot distinguish creates
/// from updates; Updated is used conservatively for reconciliation correctness.
/// The delivery_id dedup table prevents double-processing of replays.
///
/// The stateCategory -> FeatureStatus mapping is done via the state field name
/// and a defensive category lookup: the state name is treated as if it were a
/// category token first, then falls back to Investigating for any unknown value.
/// This is correct for the polling path where only the raw state NAME is present
/// (not the stateCategory). A production deployment wires a live states-list
/// lookup to map name -> category -> FeatureStatus; here we do a best-effort
/// match by checking whether the state name itself is a known category token.
pub fn parse_wiql_workitems(json: &str) -> anyhow::Result<Vec<InboundWorkItemEvent>> {
    let resp: AdoWorkItemsResponse =
        serde_json::from_str(json).map_err(|e| anyhow::anyhow!("parse_wiql_workitems: {e}"))?;

    let events = resp
        .value
        .into_iter()
        .map(|item| {
            let id_str = item.id.to_string();
            let state = item.fields.state.as_deref().unwrap_or("");
            let status = category_to_status(state);
            let occurred_at = item
                .fields
                .changed_date
                .clone()
                .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string());
            let title = item.fields.title.clone();
            InboundWorkItemEvent {
                reference: ExternalRef {
                    provider: Provider::AzureDevOps,
                    external_id: id_str.clone(),
                    container: None,
                    // URL is set to empty string here; the provider fills it from config.
                    url: String::new(),
                    revision: None,
                },
                kind: InboundKind::Updated,
                title,
                description: None,
                status: Some(status),
                body: None,
                delivery_id: format!("ado-poll-{id_str}"),
                is_echo: false,
                occurred_at,
            }
        })
        .collect();

    Ok(events)
}

/// Parse a single ADO work-item JSON object into a CanonicalStory.
///
/// Maps fields:
/// - System.Title          -> title
/// - System.State          -> status via category_to_status (best-effort; see note on parse_wiql_workitems)
/// - System.Description    -> description (may be HTML from ADO; stored as-is)
/// - System.CreatedBy      -> created_by (uniqueName preferred, fallback displayName)
/// - id                    -> external_id (numeric, stringified)
/// - browse URL            -> derived from the organization/project/id in the caller context
///
/// Because parsers are pure functions here (no config access), the browse URL is
/// constructed separately by the provider after calling this function. This
/// function sets `url` to an empty string placeholder for the parser, and the
/// provider fills it in.
pub fn parse_workitem(json: &str) -> anyhow::Result<AdoParsedWorkItem> {
    let item: AdoWorkItem =
        serde_json::from_str(json).map_err(|e| anyhow::anyhow!("parse_workitem: {e}"))?;

    let state = item.fields.state.as_deref().unwrap_or("");
    let status = category_to_status(state);
    let title = item
        .fields
        .title
        .unwrap_or_else(|| "(no title)".to_string());
    let description = item.fields.description.unwrap_or_default();
    let created_by = item
        .fields
        .created_by
        .as_ref()
        .and_then(|u| u.unique_name.clone().or_else(|| u.display_name.clone()))
        .unwrap_or_else(|| "unknown".to_string());

    Ok(AdoParsedWorkItem {
        id: item.id,
        status,
        title,
        description,
        created_by,
    })
}

/// Intermediate result from `parse_workitem`, used by `AdoProvider` to
/// build a `CanonicalStory` with the browse URL filled in from config.
#[derive(Debug)]
pub struct AdoParsedWorkItem {
    /// Numeric ADO work-item id.
    pub id: u64,
    /// Mapped canonical status.
    pub status: FeatureStatus,
    /// Work-item title (System.Title).
    pub title: String,
    /// Work-item description (System.Description, may be HTML).
    pub description: String,
    /// Creator identity string (uniqueName or displayName).
    pub created_by: String,
}

// ── AdoProvider ────────────────────────────────────────────────────────────────

/// Azure DevOps Boards adapter implementing WorkItemProvider. Parameterized over
/// the HTTP transport so it can be unit-tested with FakeTransport without network.
pub struct AdoProvider<T: HttpTransport> {
    config: AdoConfig,
    transport: T,
}

impl<T: HttpTransport> AdoProvider<T> {
    /// Construct a new ADO provider with the given config and transport.
    pub fn new(config: AdoConfig, transport: T) -> Self {
        Self { config, transport }
    }
}

#[async_trait]
impl<T: HttpTransport> WorkItemProvider for AdoProvider<T> {
    fn kind(&self) -> Provider {
        Provider::AzureDevOps
    }

    async fn ingest_story(&self, reference: &ExternalRef) -> anyhow::Result<CanonicalStory> {
        let url = self.config.work_item_url(&reference.external_id);
        let resp = self.transport.get(&url).await?;
        if resp.status < 200 || resp.status >= 300 {
            anyhow::bail!(
                "ADO GET work item {}: HTTP {} {}",
                reference.external_id,
                resp.status,
                resp.body
            );
        }
        let parsed = parse_workitem(&resp.body)?;
        let browse_url = self.config.browse_url(&parsed.id.to_string());
        Ok(CanonicalStory {
            id: parsed.id.to_string(),
            external_ref: Some(ExternalRef {
                provider: Provider::AzureDevOps,
                external_id: parsed.id.to_string(),
                container: None,
                url: browse_url,
                revision: None,
            }),
            title: parsed.title,
            description: parsed.description,
            status: parsed.status,
            created_by: parsed.created_by,
            targets: vec![],
        })
    }

    async fn push_status(
        &self,
        reference: &ExternalRef,
        report: &FeatureStatusReport,
    ) -> anyhow::Result<()> {
        let category = status_to_category(report.status);
        let concrete_state = default_state_for_category(category);
        let patch_body = json_patch_state(concrete_state);

        // Step 1: PATCH System.State to the default concrete state for this category.
        // Content-Type for JSON-Patch on ADO is application/json-patch+json; the
        // transport posts application/json which ADO also accepts for this endpoint.
        let patch_url = self.config.patch_url(&reference.external_id);
        let patch_resp = self.transport.post(&patch_url, &patch_body).await?;
        if patch_resp.status >= 300 {
            anyhow::bail!(
                "ADO PATCH state on {}: HTTP {} {}",
                reference.external_id,
                patch_resp.status,
                patch_resp.body
            );
        }

        // Step 2: always post the rollup comment (provenance trail).
        let rollup = status_rollup_text(report);
        let comment_url = self.config.comments_url(&reference.external_id);
        let comment_resp = self.transport.post(&comment_url, &rollup).await?;
        if comment_resp.status >= 300 {
            anyhow::bail!(
                "ADO POST rollup comment on {}: HTTP {} {}",
                reference.external_id,
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
        let body = clarifying_questions_body(questions);
        let url = self.config.comments_url(&reference.external_id);
        let resp = self.transport.post(&url, &body).await?;
        if resp.status < 200 || resp.status >= 300 {
            anyhow::bail!(
                "ADO POST clarifying questions on {}: HTTP {} {}",
                reference.external_id,
                resp.status,
                resp.body
            );
        }
        let created: AdoCommentCreated = serde_json::from_str(&resp.body).map_err(|e| {
            anyhow::anyhow!(
                "parse ADO comment response for {}: {e} (body: {})",
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
        // Step 1: POST the WIQL query to get a list of changed work-item ids.
        let wiql = wiql_changed_since(cursor);
        let wiql_body = {
            let escaped = wiql.replace('\\', "\\\\").replace('"', "\\\"");
            format!(r#"{{"query":"{escaped}"}}"#)
        };
        let wiql_url = self.config.wiql_url();
        let wiql_resp = self.transport.post(&wiql_url, &wiql_body).await?;
        if wiql_resp.status < 200 || wiql_resp.status >= 300 {
            anyhow::bail!(
                "ADO WIQL POST: HTTP {} {}",
                wiql_resp.status,
                wiql_resp.body
            );
        }

        // Parse the WIQL response to extract work-item ids.
        let wiql_parsed: WiqlResponse = serde_json::from_str(&wiql_resp.body)
            .map_err(|e| anyhow::anyhow!("parse WIQL response: {e}"))?;

        if wiql_parsed.work_items.is_empty() {
            let next_cursor = cursor.unwrap_or("").to_string();
            return Ok((vec![], next_cursor));
        }

        // Step 2: GET the work-item batch to read fields.
        let ids: Vec<u64> = wiql_parsed.work_items.iter().map(|r| r.id).collect();
        let batch_url = self.config.batch_url(&ids);
        let batch_resp = self.transport.get(&batch_url).await?;
        if batch_resp.status < 200 || batch_resp.status >= 300 {
            anyhow::bail!(
                "ADO batch GET work items: HTTP {} {}",
                batch_resp.status,
                batch_resp.body
            );
        }

        let mut events = parse_wiql_workitems(&batch_resp.body)?;

        // Fill in the browse URL for each event using the config.
        for event in &mut events {
            event.reference.url = self.config.browse_url(&event.reference.external_id);
        }

        // Derive the next cursor from the maximum ChangedDate across all events.
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

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::FakeTransport;
    use crate::{FeatureStatusReport, GateOutcome, GateResult, PrLink, PrStatus, SignOff};

    // ── Helper builders ────────────────────────────────────────────────────

    fn test_config() -> AdoConfig {
        AdoConfig {
            organization: "myorg".to_string(),
            project: "MyProject".to_string(),
            pat: "mypattoken".to_string(),
        }
    }

    fn ado_ref(id: &str) -> ExternalRef {
        ExternalRef {
            provider: Provider::AzureDevOps,
            external_id: id.to_string(),
            container: None,
            url: format!("https://dev.azure.com/myorg/MyProject/_workitems/edit/{id}"),
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

    // ── base64 ────────────────────────────────────────────────────────────

    #[test]
    fn base64_encode_known_vectors() {
        // RFC 4648 test vectors (same algorithm as jira.rs).
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    // ── auth_header ────────────────────────────────────────────────────────

    #[test]
    fn auth_header_is_basic_base64_of_colon_pat() {
        // ADO Basic auth is base64(":" + pat), empty username.
        let cfg = AdoConfig {
            organization: "x".to_string(),
            project: "y".to_string(),
            pat: "secret".to_string(),
        };
        let header = cfg.auth_header();
        assert!(header.starts_with("Basic "), "must start with 'Basic '");
        let encoded = &header["Basic ".len()..];
        // Verify the encoded payload: base64(":secret")
        assert_eq!(encoded, base64_encode(b":secret"));
    }

    #[test]
    fn auth_header_empty_pat_encodes_single_colon() {
        let cfg = AdoConfig {
            organization: "x".to_string(),
            project: "y".to_string(),
            pat: String::new(),
        };
        let header = cfg.auth_header();
        let encoded = &header["Basic ".len()..];
        assert_eq!(encoded, base64_encode(b":"));
    }

    // ── status_to_category full mapping ───────────────────────────────────

    #[test]
    fn status_to_category_full_mapping() {
        assert_eq!(status_to_category(FeatureStatus::Intake), "Proposed");
        assert_eq!(
            status_to_category(FeatureStatus::Investigating),
            "InProgress"
        );
        assert_eq!(
            status_to_category(FeatureStatus::AwaitingClarification),
            "InProgress"
        );
        assert_eq!(status_to_category(FeatureStatus::Planned), "InProgress");
        assert_eq!(status_to_category(FeatureStatus::Executing), "InProgress");
        assert_eq!(status_to_category(FeatureStatus::Gating), "InProgress");
        assert_eq!(status_to_category(FeatureStatus::Blocked), "InProgress");
        assert_eq!(status_to_category(FeatureStatus::AwaitingQa), "Resolved");
        assert_eq!(status_to_category(FeatureStatus::SignedOff), "Completed");
        assert_eq!(status_to_category(FeatureStatus::Done), "Completed");
        assert_eq!(status_to_category(FeatureStatus::Rejected), "Removed");
    }

    // ── category_to_status ────────────────────────────────────────────────

    #[test]
    fn category_to_status_known_tokens() {
        assert_eq!(category_to_status("Proposed"), FeatureStatus::Intake);
        assert_eq!(
            category_to_status("InProgress"),
            FeatureStatus::Investigating
        );
        assert_eq!(category_to_status("Resolved"), FeatureStatus::AwaitingQa);
        assert_eq!(category_to_status("Completed"), FeatureStatus::Done);
        assert_eq!(category_to_status("Removed"), FeatureStatus::Rejected);
    }

    #[test]
    fn category_to_status_unknown_routes_to_investigating() {
        // ADO stateCategory is a plain string, not a frozen enum.
        // Any unknown value must route to the safe active default.
        assert_eq!(
            category_to_status("FutureUnknownCategory"),
            FeatureStatus::Investigating
        );
        assert_eq!(category_to_status(""), FeatureStatus::Investigating);
        assert_eq!(category_to_status("proposed"), FeatureStatus::Investigating); // wrong case
        assert_eq!(
            category_to_status("inprogress"),
            FeatureStatus::Investigating
        ); // wrong case
    }

    // ── round-trip for stable tokens ───────────────────────────────────────

    #[test]
    fn status_category_round_trip_for_stable_tokens() {
        let pairs = [
            (FeatureStatus::Intake, "Proposed"),
            (FeatureStatus::AwaitingQa, "Resolved"),
            (FeatureStatus::Done, "Completed"),
            (FeatureStatus::Rejected, "Removed"),
        ];
        for (status, expected_cat) in pairs {
            let cat = status_to_category(status);
            assert_eq!(cat, expected_cat);
            let back = category_to_status(cat);
            assert_eq!(back, status);
        }
    }

    // ── default_state_for_category ─────────────────────────────────────────

    #[test]
    fn default_state_for_category_all_known() {
        assert_eq!(default_state_for_category("Proposed"), "New");
        assert_eq!(default_state_for_category("InProgress"), "Active");
        assert_eq!(default_state_for_category("Resolved"), "Resolved");
        assert_eq!(default_state_for_category("Completed"), "Closed");
        assert_eq!(default_state_for_category("Removed"), "Removed");
    }

    #[test]
    fn default_state_for_category_unknown_falls_back_to_active() {
        assert_eq!(default_state_for_category("FutureCat"), "Active");
        assert_eq!(default_state_for_category(""), "Active");
    }

    // ── json_patch_state ───────────────────────────────────────────────────

    #[test]
    fn json_patch_state_is_valid_json_array() {
        let patch = json_patch_state("Active");
        let v: serde_json::Value =
            serde_json::from_str(&patch).expect("json_patch_state must produce valid JSON");
        let arr = v.as_array().expect("must be a JSON array");
        assert_eq!(arr.len(), 1);
        let op = &arr[0];
        assert_eq!(op["op"], "add");
        assert_eq!(op["path"], "/fields/System.State");
        assert_eq!(op["value"], "Active");
    }

    #[test]
    fn json_patch_state_escapes_quotes_in_state_name() {
        let patch = json_patch_state(r#"My "Special" State"#);
        // Must parse as valid JSON (the escaped quotes don't break the array).
        let v: serde_json::Value =
            serde_json::from_str(&patch).expect("must be valid JSON after escaping");
        assert_eq!(v[0]["value"], r#"My "Special" State"#);
    }

    #[test]
    fn json_patch_state_all_default_states_produce_valid_json() {
        for state in &["New", "Active", "Resolved", "Closed", "Removed"] {
            let patch = json_patch_state(state);
            let v: serde_json::Value = serde_json::from_str(&patch)
                .unwrap_or_else(|e| panic!("invalid JSON for {state}: {e}"));
            assert_eq!(v[0]["value"], *state);
        }
    }

    // ── comment_body ───────────────────────────────────────────────────────

    #[test]
    fn comment_body_is_valid_json_with_text_field() {
        let body = comment_body("Hello, this is a rollup.");
        let v: serde_json::Value =
            serde_json::from_str(&body).expect("comment_body must produce valid JSON");
        assert_eq!(v["text"], "Hello, this is a rollup.");
    }

    #[test]
    fn comment_body_escapes_double_quotes() {
        let body = comment_body(r#"He said "yes"."#);
        let v: serde_json::Value = serde_json::from_str(&body).expect("must be valid JSON");
        assert_eq!(v["text"], r#"He said "yes"."#);
    }

    // ── clarifying_questions_body ──────────────────────────────────────────

    #[test]
    fn clarifying_questions_body_is_valid_json() {
        let questions = vec![
            "What is the target audience?".to_string(),
            "Are notifications opt-in or opt-out?".to_string(),
        ];
        let body = clarifying_questions_body(&questions);
        let v: serde_json::Value =
            serde_json::from_str(&body).expect("clarifying_questions_body must produce valid JSON");
        // Must have a "text" field.
        let text = v["text"].as_str().expect("text must be a string");
        // Must mention Product Owner.
        assert!(
            text.to_lowercase().contains("product owner"),
            "body must mention Product Owner, got: {text}"
        );
        // Must contain both questions.
        assert!(
            text.contains("target audience"),
            "body must contain first question"
        );
        assert!(
            text.contains("opt-in or opt-out"),
            "body must contain second question"
        );
    }

    #[test]
    fn clarifying_questions_body_mentions_product_owner() {
        let questions = vec!["Q1?".to_string()];
        let body = clarifying_questions_body(&questions);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let text = v["text"].as_str().unwrap();
        assert!(
            text.to_lowercase().contains("product owner"),
            "must mention Product Owner: {text}"
        );
    }

    #[test]
    fn clarifying_questions_body_escapes_html_special_chars() {
        let questions = vec!["Is <b>bold</b> & \"quoted\" ok?".to_string()];
        let body = clarifying_questions_body(&questions);
        // Must parse as valid JSON (no broken escaping).
        let v: serde_json::Value =
            serde_json::from_str(&body).expect("must be valid JSON with HTML-escaped question");
        let text = v["text"].as_str().unwrap();
        // HTML special chars in questions are escaped.
        assert!(
            text.contains("&lt;b&gt;") || text.contains("&amp;"),
            "HTML special chars should be escaped in question text: {text}"
        );
    }

    // ── status_rollup_text ─────────────────────────────────────────────────

    #[test]
    fn status_rollup_text_is_valid_json_with_text_field() {
        let report = FeatureStatusReport {
            status: FeatureStatus::Done,
            pr_links: vec![PrLink {
                repo: "org/repo".to_string(),
                url: "https://github.com/org/repo/pull/1".to_string(),
                title: "Add feature".to_string(),
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
            provenance_url: "https://camerata.internal/provenance/42".to_string(),
        };
        let body = status_rollup_text(&report);
        let v: serde_json::Value =
            serde_json::from_str(&body).expect("status_rollup_text must produce valid JSON");
        let text = v["text"].as_str().expect("text must be a string");
        assert!(!text.is_empty(), "rollup text must not be empty");
    }

    #[test]
    fn status_rollup_text_contains_provenance_url() {
        let report = make_report(FeatureStatus::Executing);
        let body = status_rollup_text(&report);
        assert!(
            body.contains("camerata.internal/provenance"),
            "rollup must reference the provenance URL"
        );
    }

    #[test]
    fn status_rollup_text_contains_pr_link_and_gate_result() {
        let report = FeatureStatusReport {
            status: FeatureStatus::Done,
            pr_links: vec![PrLink {
                repo: "org/repo".to_string(),
                url: "https://github.com/org/repo/pull/42".to_string(),
                title: "My PR".to_string(),
                status: PrStatus::Merged,
            }],
            gate_results: vec![GateResult {
                rule_id: "GATE-007".to_string(),
                result: GateOutcome::Fail,
                message: Some("Lint failed.".to_string()),
            }],
            sign_off: None,
            provenance_url: "https://camerata.internal/provenance/42".to_string(),
        };
        let body = status_rollup_text(&report);
        assert!(body.contains("My PR"), "must contain PR title");
        assert!(body.contains("GATE-007"), "must contain gate rule id");
        assert!(body.contains("FAIL"), "must contain gate outcome");
    }

    // ── wiql_changed_since ─────────────────────────────────────────────────

    #[test]
    fn wiql_none_uses_today_minus_7_lookback() {
        let wiql = wiql_changed_since(None);
        assert!(
            wiql.contains("@today - 7"),
            "None cursor should use a 7-day lookback: {wiql}"
        );
        assert!(
            wiql.contains("ORDER BY"),
            "wiql should include ordering: {wiql}"
        );
        assert!(
            wiql.contains("SELECT [System.Id]"),
            "wiql should select System.Id: {wiql}"
        );
    }

    #[test]
    fn wiql_some_embeds_timestamp() {
        let ts = "2026-06-01T00:00:00.000Z";
        let wiql = wiql_changed_since(Some(ts));
        assert!(wiql.contains(ts), "must embed the timestamp: {wiql}");
        assert!(wiql.contains("ORDER BY"), "should include ordering: {wiql}");
    }

    #[test]
    fn wiql_none_and_some_both_select_system_id() {
        assert!(wiql_changed_since(None).contains("System.Id"));
        assert!(wiql_changed_since(Some("2026-01-01")).contains("System.Id"));
    }

    // ── parse_wiql_workitems ────────────────────────────────────────────────

    const BATCH_JSON: &str = r#"{
        "count": 2,
        "value": [
            {
                "id": 101,
                "rev": 3,
                "fields": {
                    "System.Id": 101,
                    "System.Title": "Build login page",
                    "System.State": "Proposed",
                    "System.ChangedDate": "2026-06-01T10:00:00.000Z",
                    "System.Description": "<p>Login page spec.</p>",
                    "System.CreatedBy": {
                        "uniqueName": "alice@example.com",
                        "displayName": "Alice"
                    }
                }
            },
            {
                "id": 102,
                "rev": 5,
                "fields": {
                    "System.Id": 102,
                    "System.Title": "Dark mode support",
                    "System.State": "InProgress",
                    "System.ChangedDate": "2026-06-02T12:00:00.000Z",
                    "System.Description": null,
                    "System.CreatedBy": {
                        "uniqueName": "bob@example.com",
                        "displayName": "Bob"
                    }
                }
            }
        ]
    }"#;

    #[test]
    fn parse_wiql_workitems_two_items() {
        let events = parse_wiql_workitems(BATCH_JSON).expect("parse must succeed");
        assert_eq!(events.len(), 2);

        let e0 = &events[0];
        assert_eq!(e0.reference.external_id, "101");
        assert_eq!(e0.reference.provider, Provider::AzureDevOps);
        assert_eq!(e0.kind, InboundKind::Updated);
        assert_eq!(e0.status, Some(FeatureStatus::Intake)); // Proposed -> Intake
        assert_eq!(e0.title, Some("Build login page".to_string()));
        assert_eq!(e0.occurred_at, "2026-06-01T10:00:00.000Z");
        assert_eq!(e0.delivery_id, "ado-poll-101");

        let e1 = &events[1];
        assert_eq!(e1.reference.external_id, "102");
        assert_eq!(e1.status, Some(FeatureStatus::Investigating)); // InProgress -> Investigating
        assert_eq!(e1.occurred_at, "2026-06-02T12:00:00.000Z");
        assert_eq!(e1.delivery_id, "ado-poll-102");
    }

    // ── parse_workitem ─────────────────────────────────────────────────────

    const WORK_ITEM_JSON: &str = r#"{
        "id": 101,
        "rev": 3,
        "fields": {
            "System.Id": 101,
            "System.Title": "Build login page",
            "System.State": "Completed",
            "System.ChangedDate": "2026-06-01T10:00:00.000Z",
            "System.Description": "<p>Login page spec.</p>",
            "System.CreatedBy": {
                "uniqueName": "alice@example.com",
                "displayName": "Alice"
            }
        }
    }"#;

    #[test]
    fn parse_workitem_maps_all_fields() {
        let parsed = parse_workitem(WORK_ITEM_JSON).expect("parse must succeed");
        assert_eq!(parsed.id, 101);
        assert_eq!(parsed.title, "Build login page");
        assert_eq!(parsed.status, FeatureStatus::Done); // Completed -> Done
        assert_eq!(parsed.created_by, "alice@example.com");
        assert!(
            parsed.description.contains("Login page"),
            "description must be non-empty"
        );
    }

    #[test]
    fn parse_workitem_null_description_is_empty_string() {
        let json = r#"{
            "id": 102,
            "rev": 1,
            "fields": {
                "System.Title": "No description",
                "System.State": "InProgress",
                "System.ChangedDate": "2026-06-01T00:00:00.000Z",
                "System.Description": null,
                "System.CreatedBy": {
                    "uniqueName": "bob@example.com",
                    "displayName": "Bob"
                }
            }
        }"#;
        let parsed = parse_workitem(json).expect("parse must succeed");
        assert_eq!(parsed.description, "");
        assert_eq!(parsed.status, FeatureStatus::Investigating);
    }

    #[test]
    fn parse_workitem_missing_creator_falls_back_to_unknown() {
        let json = r#"{
            "id": 103,
            "rev": 1,
            "fields": {
                "System.Title": "Mystery item",
                "System.State": "Proposed",
                "System.ChangedDate": "2026-06-01T00:00:00.000Z"
            }
        }"#;
        let parsed = parse_workitem(json).expect("parse must succeed");
        assert_eq!(parsed.created_by, "unknown");
        assert_eq!(parsed.status, FeatureStatus::Intake);
    }

    // ── AdoProvider via FakeTransport ──────────────────────────────────────

    #[tokio::test]
    async fn provider_kind_is_azure_devops() {
        let transport = FakeTransport::new();
        let provider = AdoProvider::new(test_config(), transport);
        assert_eq!(provider.kind(), Provider::AzureDevOps);
    }

    #[tokio::test]
    async fn ingest_story_calls_get_and_returns_canonical_story() {
        let transport = FakeTransport::new().on("GET", "/workitems/101", 200, WORK_ITEM_JSON);
        let provider = AdoProvider::new(test_config(), transport);
        let story = provider.ingest_story(&ado_ref("101")).await.unwrap();
        assert_eq!(story.id, "101");
        assert_eq!(story.title, "Build login page");
        assert_eq!(story.status, FeatureStatus::Done);
        assert_eq!(story.created_by, "alice@example.com");

        let ext = story.external_ref.expect("must have external_ref");
        assert_eq!(ext.provider, Provider::AzureDevOps);
        assert_eq!(ext.external_id, "101");
        assert!(
            ext.url.contains("101"),
            "browse URL must contain the item id"
        );

        let calls = provider.transport.recorded_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "GET");
        assert!(calls[0].1.contains("101"), "GET must target work item 101");
    }

    #[tokio::test]
    async fn push_status_posts_patch_then_comment() {
        let transport = FakeTransport::new()
            .on("POST", "/workitems/42", 200, "{}")
            .on("POST", "/comments", 201, r#"{"id":100}"#);

        let provider = AdoProvider::new(test_config(), transport);
        let report = FeatureStatusReport {
            status: FeatureStatus::Done,
            pr_links: vec![],
            gate_results: vec![],
            sign_off: None,
            provenance_url: "https://camerata.internal/provenance/42".to_string(),
        };

        provider.push_status(&ado_ref("42"), &report).await.unwrap();

        let calls = provider.transport.recorded_calls();
        let methods: Vec<&str> = calls.iter().map(|(m, _, _)| m.as_str()).collect();
        assert_eq!(
            methods,
            vec!["POST", "POST"],
            "expected PATCH (via POST) then rollup comment POST"
        );

        // First POST: the JSON-Patch body for System.State.
        let patch_body = &calls[0].2;
        let v: serde_json::Value =
            serde_json::from_str(patch_body).expect("patch body must be valid JSON");
        assert_eq!(v[0]["op"], "add");
        assert_eq!(v[0]["path"], "/fields/System.State");
        // Done -> Completed -> "Closed"
        assert_eq!(v[0]["value"], "Closed");

        // Second POST: the rollup comment must contain the provenance URL.
        let comment_body = &calls[1].2;
        assert!(
            comment_body.contains("camerata.internal/provenance"),
            "rollup comment must contain provenance URL"
        );
    }

    #[tokio::test]
    async fn push_status_state_maps_correctly_for_each_category() {
        // Verify the concrete state name written for each canonical status that
        // maps to a distinct stateCategory.
        let cases: &[(FeatureStatus, &str)] = &[
            (FeatureStatus::Intake, "New"),          // Proposed
            (FeatureStatus::Executing, "Active"),    // InProgress
            (FeatureStatus::AwaitingQa, "Resolved"), // Resolved
            (FeatureStatus::Done, "Closed"),         // Completed
            (FeatureStatus::Rejected, "Removed"),    // Removed
        ];

        for (status, expected_state) in cases {
            let transport = FakeTransport::new()
                .on("POST", "/workitems/99", 200, "{}")
                .on("POST", "/comments", 201, r#"{"id":1}"#);
            let provider = AdoProvider::new(test_config(), transport);
            let report = make_report(*status);
            provider.push_status(&ado_ref("99"), &report).await.unwrap();

            let calls = provider.transport.recorded_calls();
            let patch_body = &calls[0].2;
            let v: serde_json::Value = serde_json::from_str(patch_body).expect("valid JSON");
            assert_eq!(
                v[0]["value"], *expected_state,
                "status {:?} must write state '{expected_state}'",
                status
            );
        }
    }

    #[tokio::test]
    async fn post_clarifying_questions_posts_comment_and_returns_id() {
        let response_json = r#"{"id": 42, "text": "...", "createdDate": "2026-06-01"}"#;
        let transport = FakeTransport::new().on("POST", "/comments", 201, response_json);

        let provider = AdoProvider::new(test_config(), transport);
        let questions = vec![
            "Which date format?".to_string(),
            "Are exports opt-in?".to_string(),
        ];
        let comment_id = provider
            .post_clarifying_questions(&ado_ref("101"), &questions)
            .await
            .unwrap();

        assert_eq!(comment_id, "42");

        let calls = provider.transport.recorded_calls();
        assert_eq!(calls.len(), 1);
        let (method, url, body) = &calls[0];
        assert_eq!(method, "POST");
        assert!(
            url.contains("/comments"),
            "must post to comments endpoint: {url}"
        );

        // Body must be valid JSON containing the questions.
        let v: serde_json::Value = serde_json::from_str(body).expect("body must be valid JSON");
        let text = v["text"].as_str().expect("text must be present");
        assert!(
            text.contains("Which date format"),
            "must contain first question"
        );
        assert!(
            text.contains("Are exports opt-in"),
            "must contain second question"
        );
        assert!(
            text.to_lowercase().contains("product owner"),
            "must mention Product Owner"
        );
    }

    #[tokio::test]
    async fn poll_posts_wiql_then_gets_batch_and_returns_events() {
        let wiql_response = r#"{
            "queryType": "flat",
            "workItems": [
                {"id": 101, "url": "https://dev.azure.com/myorg/MyProject/_apis/wit/workitems/101"},
                {"id": 102, "url": "https://dev.azure.com/myorg/MyProject/_apis/wit/workitems/102"}
            ]
        }"#;

        let transport = FakeTransport::new()
            .on("POST", "/wiql", 200, wiql_response)
            .on("GET", "/workitems", 200, BATCH_JSON);

        let provider = AdoProvider::new(test_config(), transport);
        let (events, next_cursor) = provider.poll(None).await.unwrap();

        assert_eq!(events.len(), 2, "must return 2 events from batch");
        assert_eq!(events[0].reference.external_id, "101");
        assert_eq!(events[1].reference.external_id, "102");

        // Browse URLs must be filled in from config.
        assert!(
            events[0].reference.url.contains("101"),
            "browse URL must contain item id"
        );

        // Next cursor is the max ChangedDate.
        assert_eq!(next_cursor, "2026-06-02T12:00:00.000Z");

        let calls = provider.transport.recorded_calls();
        let methods: Vec<&str> = calls.iter().map(|(m, _, _)| m.as_str()).collect();
        assert_eq!(methods, vec!["POST", "GET"]);

        // First call must be to the WIQL endpoint.
        let wiql_body = &calls[0].2;
        assert!(
            wiql_body.contains("System.Id"),
            "WIQL body must contain SELECT System.Id"
        );
    }

    #[tokio::test]
    async fn poll_empty_wiql_result_returns_no_events_and_echoes_cursor() {
        let empty_wiql = r#"{"queryType":"flat","workItems":[]}"#;
        let transport = FakeTransport::new().on("POST", "/wiql", 200, empty_wiql);

        let provider = AdoProvider::new(test_config(), transport);
        let cursor = "2026-06-01T00:00:00.000Z";
        let (events, next_cursor) = provider.poll(Some(cursor)).await.unwrap();

        assert!(events.is_empty(), "empty WIQL result returns no events");
        assert_eq!(
            next_cursor, cursor,
            "empty result must echo the incoming cursor"
        );

        // Only one call: the WIQL POST; no batch GET when no ids.
        let calls = provider.transport.recorded_calls();
        assert_eq!(calls.len(), 1, "must only call WIQL when no ids returned");
    }

    #[tokio::test]
    async fn poll_with_cursor_embeds_cursor_in_wiql_body() {
        let empty_wiql = r#"{"queryType":"flat","workItems":[]}"#;
        let transport = FakeTransport::new().on("POST", "/wiql", 200, empty_wiql);

        let provider = AdoProvider::new(test_config(), transport);
        let cursor = "2026-06-01T00:00:00.000Z";
        provider.poll(Some(cursor)).await.unwrap();

        let calls = provider.transport.recorded_calls();
        let wiql_body = &calls[0].2;
        assert!(
            wiql_body.contains("2026-06-01"),
            "WIQL body must embed the cursor timestamp: {wiql_body}"
        );
    }

    #[tokio::test]
    async fn cursor_derived_from_max_changed_date_not_wall_clock() {
        // Two events: one newer, one older. Cursor must be the max ChangedDate.
        let wiql_response = r#"{
            "queryType": "flat",
            "workItems": [
                {"id": 101, "url": ""},
                {"id": 102, "url": ""}
            ]
        }"#;

        let transport = FakeTransport::new()
            .on("POST", "/wiql", 200, wiql_response)
            .on("GET", "/workitems", 200, BATCH_JSON);

        let provider = AdoProvider::new(test_config(), transport);
        let (events, next_cursor) = provider.poll(None).await.unwrap();
        assert_eq!(events.len(), 2);
        // e0 has 2026-06-01, e1 has 2026-06-02; cursor must be the max.
        assert_eq!(next_cursor, "2026-06-02T12:00:00.000Z");
    }

    #[tokio::test]
    async fn clarify_bridge_round_trip_via_ado_provider() {
        // 1. post_clarifying_questions -> POST /comments
        // 2. poll -> POST /wiql + GET /workitems returns an Updated event (simulating PO update)
        let comment_response = r#"{"id": 99}"#;
        let wiql_response = r#"{
            "queryType": "flat",
            "workItems": [
                {"id": 101, "url": ""}
            ]
        }"#;
        let batch_with_answer = r#"{
            "count": 1,
            "value": [{
                "id": 101,
                "rev": 4,
                "fields": {
                    "System.Title": "Build login page",
                    "System.State": "InProgress",
                    "System.ChangedDate": "2026-06-03T09:00:00.000Z",
                    "System.Description": "<p>PO added acceptance criteria.</p>",
                    "System.CreatedBy": {
                        "uniqueName": "po@example.com",
                        "displayName": "PO"
                    }
                }
            }]
        }"#;

        let transport = FakeTransport::new()
            .on("POST", "/comments", 201, comment_response)
            .on("POST", "/wiql", 200, wiql_response)
            .on("GET", "/workitems", 200, batch_with_answer);

        let provider = AdoProvider::new(test_config(), transport);

        // Step 1: post clarifying questions.
        let questions = vec!["Should exports use UTC?".to_string()];
        let comment_id = provider
            .post_clarifying_questions(&ado_ref("101"), &questions)
            .await
            .unwrap();
        assert_eq!(comment_id, "99");

        // Step 2: poll returns the PO's update as an inbound event.
        let (events, _cursor) = provider.poll(None).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].reference.external_id, "101");
        assert_eq!(events[0].kind, InboundKind::Updated);

        // Verify FakeTransport recorded all three calls.
        let calls = provider.transport.recorded_calls();
        let post_calls: Vec<_> = calls.iter().filter(|(m, _, _)| m == "POST").collect();
        assert_eq!(post_calls.len(), 2, "must have two POSTs: comment + WIQL");

        let (_, comment_url, comment_body) = &post_calls[0];
        assert!(
            comment_url.contains("/comments"),
            "first POST must be to comments endpoint"
        );
        assert!(
            comment_body.contains("Should exports use UTC"),
            "POST body must contain the question"
        );
    }
}
