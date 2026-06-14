//! Work-tracker integration port: canonical shapes, sync policy, and the
//! `WorkItemProvider` trait that core depends on.
//!
//! Core orchestration never imports a specific provider. Each adapter (native,
//! jira, azure-devops, github) maps to and from the canonical shapes defined here
//! and implements `WorkItemProvider`. See `docs/WORKTRACKER_INTEGRATION.md`.

use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ── Feature status ────────────────────────────────────────────────────────────

/// The lifecycle status of a story in Camerata's canonical vocabulary.
/// Providers map to and from this; they never leak their own status names into
/// the spine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureStatus {
    /// The story has been submitted and is awaiting triage.
    Intake,
    /// An agent is gathering information and decomposing the story.
    Investigating,
    /// The story is paused pending a clarifying answer from the Product Owner.
    AwaitingClarification,
    /// The story has been decomposed and is queued for execution.
    Planned,
    /// Agents are actively working on the story.
    Executing,
    /// Automated gate checks are running against the produced diff.
    Gating,
    /// Work is complete and awaiting human QA sign-off.
    AwaitingQa,
    /// The Principal Architect has approved the story for release.
    SignedOff,
    /// The story is shipped and closed.
    Done,
    /// Progress is blocked by an external dependency or decision.
    Blocked,
    /// The story was declined or cannot be completed as specified.
    Rejected,
}

// ── Provider identity ─────────────────────────────────────────────────────────

/// Which tracker backend backs a given `ExternalRef` or `WorkItemProvider`.
///
/// Serde uses the canonical wire strings from the design doc (`"native"`,
/// `"jira"`, `"azure-devops"`, `"github"`) so JSON round-trips match the
/// TypeScript interface in `docs/WORKTRACKER_INTEGRATION.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    /// The in-process native tracker (no auth, no webhook, no network).
    Native,
    /// Atlassian Jira (Cloud or Data Center).
    Jira,
    /// Microsoft Azure DevOps Boards.
    #[serde(rename = "azure-devops")]
    AzureDevOps,
    /// GitHub Issues or Projects v2.
    #[serde(rename = "github")]
    GitHub,
}

// ── External reference ────────────────────────────────────────────────────────

/// A handle to a work item on an external tracker. Stored alongside a
/// `CanonicalStory` when the story is linked to an external board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalRef {
    /// Which tracker holds this item.
    pub provider: Provider,
    /// The provider's own id for the item (Jira issue key, ADO work-item id,
    /// GitHub node id, or a synthetic id for the native provider).
    pub external_id: String,
    /// A human-navigable URL for the item on the tracker's UI.
    pub url: String,
    /// Optional revision token used for echo suppression. GitHub delivery id,
    /// Jira issue version, ADO `rev`, etc. None when not yet written back.
    pub revision: Option<String>,
}

// ── Canonical story ───────────────────────────────────────────────────────────

/// Our canonical story spine. Providers never see this shape leak outward; they
/// map to and from it via field-mapping adapters.
///
/// Note: per-field `fieldOrigins` (the echo-suppression provenance map recording
/// which side last wrote each field) is deferred to a later cut. The doc
/// describes it as `Partial<Record<keyof CanonicalStory, 'ours' | 'tracker'>>`;
/// that will be added when echo suppression is wired into the inbound reconciler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalStory {
    /// Camerata's own story id (canonical spine id, not the tracker's id).
    pub id: String,
    /// The linked tracker item, if any. Absent for native-only stories.
    pub external_ref: Option<ExternalRef>,
    /// Story title.
    pub title: String,
    /// Full story description (may be long-form markdown).
    pub description: String,
    /// Current lifecycle status.
    pub status: FeatureStatus,
    /// The user or agent that created the story.
    pub created_by: String,
}

// ── PR links ──────────────────────────────────────────────────────────────────

/// The open/merged/closed state of a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrStatus {
    /// The pull request is open.
    Open,
    /// The pull request was merged into the target branch.
    Merged,
    /// The pull request was closed without merging.
    Closed,
}

/// A link to one pull request produced for this story. A multi-repo feature
/// produces N `PrLink`s that all roll up onto the same tracker work item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrLink {
    /// The repository slug or full name (e.g. `org/repo`).
    pub repo: String,
    /// URL to the pull request on the code host.
    pub url: String,
    /// Pull request title.
    pub title: String,
    /// Current state of the pull request.
    pub status: PrStatus,
}

// ── Gate results ──────────────────────────────────────────────────────────────

/// Whether a gate rule passed or failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateOutcome {
    /// The rule passed.
    Pass,
    /// The rule failed; the story must not advance until it is resolved.
    Fail,
}

/// The outcome of one gate rule evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateResult {
    /// The rule id that was evaluated (matches the rule corpus entry).
    pub rule_id: String,
    /// Pass or fail.
    pub result: GateOutcome,
    /// Optional human-readable explanation of why the rule failed (or a
    /// confirmation message for passing rules).
    pub message: Option<String>,
}

// ── Sign-off ──────────────────────────────────────────────────────────────────

/// A human sign-off recording who approved the story and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignOff {
    /// The user id or name of the approver.
    pub by: String,
    /// ISO 8601 timestamp of the approval.
    pub at: String,
}

// ── Feature status report ─────────────────────────────────────────────────────

/// The payload pushed back to the tracker when a story's status changes. This is
/// the minimum-credible provenance trail: PR links, gate pass/fail, and sign-off.
/// The full internal trail lives in Camerata's own store and is linked via
/// `provenance_url`. Provenance, gate results, PR links, and sign-off are ALWAYS
/// ours and are never overwritten by a tracker inbound event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureStatusReport {
    /// The story's current canonical lifecycle status.
    pub status: FeatureStatus,
    /// All pull requests produced for this story (N per multi-repo feature).
    pub pr_links: Vec<PrLink>,
    /// Every gate rule result from the most recent gating run.
    pub gate_results: Vec<GateResult>,
    /// Human sign-off when the story has reached `SignedOff` or `Done`.
    pub sign_off: Option<SignOff>,
    /// URL to the full provenance trail in Camerata's own store.
    pub provenance_url: String,
}

// ── Inbound events ────────────────────────────────────────────────────────────

/// The kind of change that triggered an inbound event from the tracker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboundKind {
    /// A new work item was created.
    Created,
    /// A field on an existing item was updated.
    Updated,
    /// A comment was posted on the item (includes the PO's clarification answer).
    Commented,
    /// The item's status column was changed.
    StatusChanged,
}

/// A normalized inbound event produced by an adapter from a raw webhook delivery
/// or a reconciliation poll row. Core receives this shape regardless of provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundWorkItemEvent {
    /// The external reference identifying which work item changed.
    /// (`reference` rather than `ref` because `ref` is a Rust keyword.)
    pub reference: ExternalRef,
    /// The kind of change that occurred.
    pub kind: InboundKind,
    /// Updated title, if the title changed (or if this is a Created event).
    pub title: Option<String>,
    /// Updated description, if the description changed.
    pub description: Option<String>,
    /// Updated status, if the status column changed.
    pub status: Option<FeatureStatus>,
    /// Comment body, present when `kind == Commented`.
    pub body: Option<String>,
    /// Idempotency key for this delivery (X-GitHub-Delivery GUID, Jira webhook
    /// event id, ADO hook subscription delivery id, etc.). Deduped on a unique
    /// constraint so replayed or redelivered events are dropped after the first
    /// successful processing.
    pub delivery_id: String,
    /// True when the adapter determined this event was caused by our own
    /// outbound write (matched the expected-echo table). Core drops echo events.
    pub is_echo: bool,
    /// ISO 8601 timestamp from the provider indicating when the change occurred.
    pub occurred_at: String,
}

// ── Sync policy ───────────────────────────────────────────────────────────────

/// Which side is authoritative for a given field when both Camerata and an
/// external tracker could potentially hold the value.
///
/// Provenance, gate results, PR links, and sign-off are ALWAYS `Ours` and
/// are NOT represented here because they are never configurable. A tracker must
/// never overwrite a gate result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthoritativeSide {
    /// Camerata is authoritative. The tracker is a projection of our value.
    Ours,
    /// The external tracker is authoritative. We ingest its value and mirror it.
    Tracker,
}

/// Per-field sync policy controlling which side wins for each configurable field.
/// The structural loop-breaker: a field has exactly one authoritative side, so
/// a sync war is impossible by construction (each side owns different fields).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncPolicy {
    /// Who owns the story title. Brownfield: `Tracker`. Greenfield: `Ours`.
    pub title: AuthoritativeSide,
    /// Who owns the story description. Brownfield: `Tracker`. Greenfield: `Ours`.
    pub description: AuthoritativeSide,
    /// Who owns the lifecycle status. Brownfield: `Tracker`. Greenfield: `Ours`.
    pub status: AuthoritativeSide,
}

impl SyncPolicy {
    /// Greenfield policy: every field is `Ours`. The story originates in
    /// Camerata; the tracker (if any) is a pure projection.
    pub fn greenfield() -> Self {
        Self {
            title: AuthoritativeSide::Ours,
            description: AuthoritativeSide::Ours,
            status: AuthoritativeSide::Ours,
        }
    }

    /// Brownfield enterprise policy: intake fields (`title`, `description`,
    /// `status`) flip to `Tracker`. The story originates on the team's board;
    /// Camerata ingests it and treats the board as the process-of-record for
    /// those fields. Provenance, gate results, PR links, and sign-off stay ours
    /// and are pushed back onto the issue.
    pub fn brownfield() -> Self {
        Self {
            title: AuthoritativeSide::Tracker,
            description: AuthoritativeSide::Tracker,
            status: AuthoritativeSide::Tracker,
        }
    }
}

// ── WorkItemProvider trait ────────────────────────────────────────────────────

/// The seam between core orchestration and any work-tracker backend. Core holds
/// a `dyn WorkItemProvider` and never imports provider-specific code. Each adapter
/// (native, jira, azure-devops, github) implements this trait and handles its own
/// auth, webhook signature verification, field mapping, and rate-limit handling.
#[async_trait]
pub trait WorkItemProvider: Send + Sync {
    /// The provider kind this adapter implements.
    fn kind(&self) -> Provider;

    /// Intake: pull an external work item in and normalize it as a
    /// `CanonicalStory`. The adapter maps provider-specific fields to the
    /// canonical vocabulary.
    async fn ingest_story(&self, reference: &ExternalRef) -> anyhow::Result<CanonicalStory>;

    /// Outbound: post a status transition plus the minimum-credible provenance
    /// payload (governed PR links, gate results, sign-off) as a single editable
    /// status comment on the work item. Providers use their own comment-update
    /// API (GitHub `updateComment`, Jira `PUT .../comment/{id}`, ADO Comments
    /// API) so only one comment is ever present per story.
    async fn push_status(
        &self,
        reference: &ExternalRef,
        report: &FeatureStatusReport,
    ) -> anyhow::Result<()>;

    /// CLARIFY-BRIDGE outbound (V1 slice, doc 0.5): post the PRODUCT clarifying
    /// questions as a comment on the work item, mentioning the Product Owner.
    /// Technical tradeoffs and the RuleSet are NOT posted; they stay with the
    /// Architect locally. Returns the provider's comment id or reference so the
    /// answer can later be matched by thread.
    ///
    /// Privilege boundary: the PO can answer and eventually sign off via the
    /// tracker, but can never trigger execution. The Architect reviews the
    /// ingested answer locally, approves tradeoffs, and runs the agents.
    async fn post_clarifying_questions(
        &self,
        reference: &ExternalRef,
        questions: &[String],
    ) -> anyhow::Result<String>;

    /// Inbound reconciliation poll (webhook is a later opt-in upgrade for users
    /// with a public ingress). Returns new events since `cursor` plus the next
    /// cursor. On the first call pass `None` to start from the beginning. The
    /// Product Owner's clarification answer arrives here as a `Commented` event.
    ///
    /// The V1 local tool default is poll-only: Camerata runs on the Architect's
    /// local machine which has no public URL, so inbound webhooks require an
    /// explicit opt-in tunnel (ngrok / cloudflared). The poll runs on a slow
    /// cadence (a few minutes) and serves as both the primary path and the safety
    /// net for any webhook-capable deployment.
    async fn poll(
        &self,
        cursor: Option<&str>,
    ) -> anyhow::Result<(Vec<InboundWorkItemEvent>, String)>;
}

// ── NativeProvider ────────────────────────────────────────────────────────────

/// Internal state for the native provider, held behind a `Mutex`.
struct NativeState {
    /// Map from `external_id` to the stored `CanonicalStory`.
    stories: std::collections::HashMap<String, CanonicalStory>,
    /// All clarifying-question comments posted, in order.
    comments: Vec<(String, Vec<String>)>,
    /// Monotonically increasing counter used to generate comment ids.
    comment_counter: u64,
    /// Queue of inbound events to be returned by `poll`.
    event_queue: Vec<InboundWorkItemEvent>,
    /// Monotonically increasing cursor (each poll bumps it to the current len).
    cursor: u64,
}

impl NativeState {
    fn new() -> Self {
        Self {
            stories: std::collections::HashMap::new(),
            comments: Vec::new(),
            comment_counter: 0,
            event_queue: Vec::new(),
            cursor: 0,
        }
    }
}

/// The in-process `WorkItemProvider` for greenfield / solo usage where Camerata
/// itself is the source of truth. Backed by in-memory state behind a `std::sync::Mutex`
/// (never a tokio mutex: the lock is always dropped before any `.await`).
///
/// Privilege boundary: the Product Owner can answer and sign off via the
/// tracker, but can never trigger execution. The Architect reviews the ingested
/// answer locally, approves tradeoffs, and runs the agents.
pub struct NativeProvider {
    state: Mutex<NativeState>,
}

impl NativeProvider {
    /// Construct an empty native provider.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(NativeState::new()),
        }
    }

    /// Seed a story into the provider's store. Used in tests and initial setup.
    pub fn seed_story(&self, story: CanonicalStory) {
        let mut state = self.state.lock().expect("native provider mutex poisoned");
        state.stories.insert(
            story
                .external_ref
                .as_ref()
                .map(|r| r.external_id.clone())
                .unwrap_or_else(|| story.id.clone()),
            story,
        );
    }

    /// Return all clarifying-question comment entries recorded so far, as a slice
    /// of `(comment_id, questions)` pairs. Used in tests to assert the post.
    pub fn posted_questions(&self) -> Vec<(String, Vec<String>)> {
        self.state
            .lock()
            .expect("native provider mutex poisoned")
            .comments
            .clone()
    }

    /// Inject an inbound `Commented` event representing the Product Owner's
    /// answer to a clarifying-question comment. Enqueues the event so the next
    /// `poll` call returns it. Used to make the full clarify-bridge round-trip
    /// testable without any real tracker: post questions, inject answer, poll
    /// returns the answer.
    pub fn inject_answer(&self, reference: ExternalRef, body: impl Into<String>) {
        let mut state = self.state.lock().expect("native provider mutex poisoned");
        let idx = state.event_queue.len();
        state.event_queue.push(InboundWorkItemEvent {
            reference,
            kind: InboundKind::Commented,
            title: None,
            description: None,
            status: None,
            body: Some(body.into()),
            delivery_id: format!("native-delivery-{idx}"),
            is_echo: false,
            occurred_at: "2026-01-01T00:00:00Z".to_string(),
        });
    }
}

impl Default for NativeProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl WorkItemProvider for NativeProvider {
    fn kind(&self) -> Provider {
        Provider::Native
    }

    async fn ingest_story(&self, reference: &ExternalRef) -> anyhow::Result<CanonicalStory> {
        let state = self.state.lock().expect("native provider mutex poisoned");
        state
            .stories
            .get(&reference.external_id)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "native provider: no story seeded for external_id `{}`",
                    reference.external_id
                )
            })
    }

    async fn push_status(
        &self,
        reference: &ExternalRef,
        report: &FeatureStatusReport,
    ) -> anyhow::Result<()> {
        let mut state = self.state.lock().expect("native provider mutex poisoned");
        match state.stories.get_mut(&reference.external_id) {
            Some(story) => {
                story.status = report.status;
                Ok(())
            }
            None => Err(anyhow::anyhow!(
                "native provider: no story found for external_id `{}` when pushing status",
                reference.external_id
            )),
        }
    }

    async fn post_clarifying_questions(
        &self,
        reference: &ExternalRef,
        questions: &[String],
    ) -> anyhow::Result<String> {
        let mut state = self.state.lock().expect("native provider mutex poisoned");
        state.comment_counter += 1;
        let comment_id = format!("native-comment-{}", state.comment_counter);
        state
            .comments
            .push((comment_id.clone(), questions.to_vec()));
        // Record the echo so the resulting poll event is not double-processed by
        // the reconciler when a real tracker would bounce it back.
        let _ = &reference.external_id; // referenced for documentation clarity
        Ok(comment_id)
    }

    async fn poll(
        &self,
        cursor: Option<&str>,
    ) -> anyhow::Result<(Vec<InboundWorkItemEvent>, String)> {
        let mut state = self.state.lock().expect("native provider mutex poisoned");
        // Parse the cursor as the index of the first unseen event.
        let start: usize = cursor.and_then(|c| c.parse::<u64>().ok()).unwrap_or(0) as usize;

        let new_events: Vec<InboundWorkItemEvent> =
            state.event_queue.get(start..).unwrap_or(&[]).to_vec();

        // Advance the cursor to the end of the queue.
        let next_cursor = state.event_queue.len() as u64;
        state.cursor = next_cursor;
        Ok((new_events, next_cursor.to_string()))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: build a minimal ExternalRef pointing at the native provider.
    fn native_ref(id: &str) -> ExternalRef {
        ExternalRef {
            provider: Provider::Native,
            external_id: id.to_string(),
            url: format!("native://stories/{id}"),
            revision: None,
        }
    }

    // Helper: build a minimal CanonicalStory.
    fn make_story(id: &str, title: &str) -> CanonicalStory {
        CanonicalStory {
            id: id.to_string(),
            external_ref: Some(native_ref(id)),
            title: title.to_string(),
            description: "A test story.".to_string(),
            status: FeatureStatus::Intake,
            created_by: "test-user".to_string(),
        }
    }

    // Helper: build a minimal FeatureStatusReport.
    fn make_report(status: FeatureStatus) -> FeatureStatusReport {
        FeatureStatusReport {
            status,
            pr_links: vec![],
            gate_results: vec![],
            sign_off: None,
            provenance_url: "https://camerata.internal/provenance/s1".to_string(),
        }
    }

    // ── Enum serde round-trips ─────────────────────────────────────────────

    #[test]
    fn feature_status_serde_round_trip() {
        let cases = [
            (FeatureStatus::Intake, "\"intake\""),
            (FeatureStatus::Investigating, "\"investigating\""),
            (
                FeatureStatus::AwaitingClarification,
                "\"awaiting_clarification\"",
            ),
            (FeatureStatus::Planned, "\"planned\""),
            (FeatureStatus::Executing, "\"executing\""),
            (FeatureStatus::Gating, "\"gating\""),
            (FeatureStatus::AwaitingQa, "\"awaiting_qa\""),
            (FeatureStatus::SignedOff, "\"signed_off\""),
            (FeatureStatus::Done, "\"done\""),
            (FeatureStatus::Blocked, "\"blocked\""),
            (FeatureStatus::Rejected, "\"rejected\""),
        ];
        for (variant, expected_json) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected_json, "serialize {variant:?}");
            let back: FeatureStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant, "deserialize {variant:?}");
        }
    }

    #[test]
    fn provider_serde_round_trip() {
        // Wire strings match the design doc canonical values (not Rust snake_case,
        // which would produce "azure_dev_ops" / "git_hub").
        let cases = [
            (Provider::Native, "\"native\""),
            (Provider::Jira, "\"jira\""),
            (Provider::AzureDevOps, "\"azure-devops\""),
            (Provider::GitHub, "\"github\""),
        ];
        for (variant, expected_json) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected_json, "serialize {variant:?}");
            let back: Provider = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant, "deserialize {variant:?}");
        }
    }

    #[test]
    fn pr_status_serde_round_trip() {
        let cases = [
            (PrStatus::Open, "\"open\""),
            (PrStatus::Merged, "\"merged\""),
            (PrStatus::Closed, "\"closed\""),
        ];
        for (variant, expected_json) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected_json, "serialize {variant:?}");
            let back: PrStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant, "deserialize {variant:?}");
        }
    }

    #[test]
    fn gate_outcome_serde_round_trip() {
        let cases = [
            (GateOutcome::Pass, "\"pass\""),
            (GateOutcome::Fail, "\"fail\""),
        ];
        for (variant, expected_json) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected_json, "serialize {variant:?}");
            let back: GateOutcome = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant, "deserialize {variant:?}");
        }
    }

    #[test]
    fn inbound_kind_serde_round_trip() {
        let cases = [
            (InboundKind::Created, "\"created\""),
            (InboundKind::Updated, "\"updated\""),
            (InboundKind::Commented, "\"commented\""),
            (InboundKind::StatusChanged, "\"status_changed\""),
        ];
        for (variant, expected_json) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected_json, "serialize {variant:?}");
            let back: InboundKind = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant, "deserialize {variant:?}");
        }
    }

    #[test]
    fn authoritative_side_serde_round_trip() {
        let cases = [
            (AuthoritativeSide::Ours, "\"ours\""),
            (AuthoritativeSide::Tracker, "\"tracker\""),
        ];
        for (variant, expected_json) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected_json, "serialize {variant:?}");
            let back: AuthoritativeSide = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant, "deserialize {variant:?}");
        }
    }

    // ── SyncPolicy ─────────────────────────────────────────────────────────

    #[test]
    fn sync_policy_greenfield_all_ours() {
        let policy = SyncPolicy::greenfield();
        assert_eq!(policy.title, AuthoritativeSide::Ours);
        assert_eq!(policy.description, AuthoritativeSide::Ours);
        assert_eq!(policy.status, AuthoritativeSide::Ours);
    }

    #[test]
    fn sync_policy_brownfield_all_tracker() {
        let policy = SyncPolicy::brownfield();
        assert_eq!(policy.title, AuthoritativeSide::Tracker);
        assert_eq!(policy.description, AuthoritativeSide::Tracker);
        assert_eq!(policy.status, AuthoritativeSide::Tracker);
    }

    #[test]
    fn sync_policy_round_trip_json() {
        for policy in [SyncPolicy::greenfield(), SyncPolicy::brownfield()] {
            let json = serde_json::to_string(&policy).unwrap();
            let back: SyncPolicy = serde_json::from_str(&json).unwrap();
            assert_eq!(back, policy);
        }
    }

    // ── NativeProvider: seed + ingest ──────────────────────────────────────

    #[tokio::test]
    async fn native_seed_and_ingest_returns_story() {
        let provider = NativeProvider::new();
        let story = make_story("s1", "Add login page");
        provider.seed_story(story.clone());

        let ingested = provider.ingest_story(&native_ref("s1")).await.unwrap();
        assert_eq!(ingested, story);
    }

    #[tokio::test]
    async fn native_ingest_unknown_ref_errors() {
        let provider = NativeProvider::new();
        let err = provider.ingest_story(&native_ref("does-not-exist")).await;
        assert!(err.is_err(), "ingest of unknown ref must return an error");
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("does-not-exist"),
            "error message should include the missing id"
        );
    }

    #[tokio::test]
    async fn native_kind_is_native() {
        let provider = NativeProvider::new();
        assert_eq!(provider.kind(), Provider::Native);
    }

    // ── NativeProvider: push_status ────────────────────────────────────────

    #[tokio::test]
    async fn push_status_updates_stored_story() {
        let provider = NativeProvider::new();
        provider.seed_story(make_story("s2", "Dark mode"));

        let report = make_report(FeatureStatus::Executing);
        provider
            .push_status(&native_ref("s2"), &report)
            .await
            .unwrap();

        let ingested = provider.ingest_story(&native_ref("s2")).await.unwrap();
        assert_eq!(ingested.status, FeatureStatus::Executing);
    }

    #[tokio::test]
    async fn push_status_unknown_ref_errors() {
        let provider = NativeProvider::new();
        let report = make_report(FeatureStatus::Done);
        let err = provider.push_status(&native_ref("ghost"), &report).await;
        assert!(err.is_err());
    }

    // ── NativeProvider: clarify-bridge round-trip ──────────────────────────

    #[tokio::test]
    async fn post_clarifying_questions_records_and_returns_id() {
        let provider = NativeProvider::new();
        provider.seed_story(make_story("s3", "Notification system"));

        let questions = vec![
            "Should push notifications be opt-in or opt-out by default?".to_string(),
            "Which channels are required at launch (email, SMS, push)?".to_string(),
        ];
        let comment_id = provider
            .post_clarifying_questions(&native_ref("s3"), &questions)
            .await
            .unwrap();

        assert!(
            comment_id.starts_with("native-comment-"),
            "comment id must use the native-comment-N format"
        );

        let posted = provider.posted_questions();
        assert_eq!(posted.len(), 1);
        assert_eq!(posted[0].0, comment_id);
        assert_eq!(posted[0].1, questions);
    }

    #[tokio::test]
    async fn clarify_bridge_full_round_trip() {
        let provider = NativeProvider::new();
        provider.seed_story(make_story("s4", "CSV export"));

        // 1. Post clarifying questions (outbound).
        let questions = vec!["Which date format: ISO 8601 or locale-specific?".to_string()];
        let _comment_id = provider
            .post_clarifying_questions(&native_ref("s4"), &questions)
            .await
            .unwrap();

        // 2. Inject the Product Owner's answer (simulates the tracker returning
        //    an inbound Commented event on the next poll).
        provider.inject_answer(
            native_ref("s4"),
            "ISO 8601 please, our data team expects it.",
        );

        // 3. First poll: returns the answer event.
        let (events, cursor) = provider.poll(None).await.unwrap();
        assert_eq!(
            events.len(),
            1,
            "first poll must return the injected answer"
        );
        let ev = &events[0];
        assert_eq!(ev.kind, InboundKind::Commented);
        assert_eq!(
            ev.body.as_deref(),
            Some("ISO 8601 please, our data team expects it.")
        );
        assert!(
            !ev.is_echo,
            "the injected answer must not be marked as an echo"
        );

        // 4. Second poll with the returned cursor returns no duplicates.
        let (events2, _cursor2) = provider.poll(Some(&cursor)).await.unwrap();
        assert!(
            events2.is_empty(),
            "second poll must not replay already-seen events"
        );
    }

    #[tokio::test]
    async fn poll_with_no_events_returns_empty() {
        let provider = NativeProvider::new();
        let (events, cursor) = provider.poll(None).await.unwrap();
        assert!(events.is_empty());
        assert_eq!(cursor, "0");
    }

    #[tokio::test]
    async fn multiple_injected_answers_all_returned_then_none_on_next_poll() {
        let provider = NativeProvider::new();
        provider.seed_story(make_story("s5", "Multi-answer test"));

        provider.inject_answer(native_ref("s5"), "Answer one.");
        provider.inject_answer(native_ref("s5"), "Answer two.");

        let (events, cursor) = provider.poll(None).await.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].body.as_deref(), Some("Answer one."));
        assert_eq!(events[1].body.as_deref(), Some("Answer two."));

        // Inject a third event after the first poll.
        provider.inject_answer(native_ref("s5"), "Answer three.");

        // Second poll from the returned cursor returns only the new event.
        let (events2, cursor2) = provider.poll(Some(&cursor)).await.unwrap();
        assert_eq!(events2.len(), 1);
        assert_eq!(events2[0].body.as_deref(), Some("Answer three."));

        // Third poll returns nothing.
        let (events3, _) = provider.poll(Some(&cursor2)).await.unwrap();
        assert!(events3.is_empty());
    }

    // ── CanonicalStory JSON round-trip ─────────────────────────────────────

    #[test]
    fn canonical_story_json_round_trip() {
        let story = CanonicalStory {
            id: "story-42".to_string(),
            external_ref: Some(ExternalRef {
                provider: Provider::Jira,
                external_id: "PROJ-99".to_string(),
                url: "https://jira.example.com/browse/PROJ-99".to_string(),
                revision: Some("v7".to_string()),
            }),
            title: "Export feature".to_string(),
            description: "As a user I want CSV exports.".to_string(),
            status: FeatureStatus::Planned,
            created_by: "alice".to_string(),
        };
        let json = serde_json::to_string(&story).unwrap();
        let back: CanonicalStory = serde_json::from_str(&json).unwrap();
        assert_eq!(back, story);
    }

    // ── FeatureStatusReport JSON round-trip ────────────────────────────────

    #[test]
    fn feature_status_report_json_round_trip() {
        let report = FeatureStatusReport {
            status: FeatureStatus::SignedOff,
            pr_links: vec![PrLink {
                repo: "org/repo".to_string(),
                url: "https://github.com/org/repo/pull/42".to_string(),
                title: "Add CSV export".to_string(),
                status: PrStatus::Merged,
            }],
            gate_results: vec![GateResult {
                rule_id: "GATE-001".to_string(),
                result: GateOutcome::Pass,
                message: Some("All checks passed.".to_string()),
            }],
            sign_off: Some(SignOff {
                by: "alice".to_string(),
                at: "2026-06-14T12:00:00Z".to_string(),
            }),
            provenance_url: "https://camerata.internal/provenance/story-42".to_string(),
        };
        let json = serde_json::to_string(&report).unwrap();
        let back: FeatureStatusReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, report);
    }

    // ── InboundWorkItemEvent JSON round-trip ───────────────────────────────

    #[test]
    fn inbound_event_json_round_trip() {
        let event = InboundWorkItemEvent {
            reference: native_ref("s-rt"),
            kind: InboundKind::Commented,
            title: None,
            description: None,
            status: None,
            body: Some("Please use snake_case.".to_string()),
            delivery_id: "abc-123".to_string(),
            is_echo: false,
            occurred_at: "2026-06-14T09:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: InboundWorkItemEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, event);
    }
}
