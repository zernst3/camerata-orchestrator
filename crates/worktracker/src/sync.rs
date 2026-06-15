//! Loop-avoidance engine for two-way tracker sync.
//!
//! Two independent guards prevent sync loops when Camerata mirrors an enterprise board:
//!
//! 1. Per-field direction (`apply_inbound`): each field in `SyncPolicy` has exactly
//!    one `AuthoritativeSide`. Only fields owned by `Tracker` are ever updated from
//!    an inbound event. Fields owned by `Ours` are never touched, even if the event
//!    carries a new value. Provenance, gate results, PR links, and sign-off are not
//!    modeled on `CanonicalStory` at all and are always `Ours` by construction.
//!
//! 2. Echo suppression (`ExpectedEchoTable`): every time Camerata writes to an
//!    external tracker it records the expected revision (delivery id, Jira version,
//!    ADO rev) in an in-memory table. When the resulting inbound event arrives the
//!    adapter matches it against the table; if it matches the event is marked as an
//!    echo and `apply_inbound` short-circuits immediately. Delivery-id deduplication
//!    is a second, independent layer that catches redelivered or replayed webhook
//!    payloads.

use crate::{AuthoritativeSide, CanonicalStory, ExternalRef, InboundWorkItemEvent, SyncPolicy};
use std::collections::HashSet;

// ── Guard 1: per-field direction ──────────────────────────────────────────────

/// Return the names of every field whose authoritative side is `Tracker`.
/// These are the only fields that `apply_inbound` is permitted to update.
/// Useful for callers that want to know upfront which fields could change.
pub fn updatable_fields(policy: &SyncPolicy) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if policy.title == AuthoritativeSide::Tracker {
        fields.push("title");
    }
    if policy.description == AuthoritativeSide::Tracker {
        fields.push("description");
    }
    if policy.status == AuthoritativeSide::Tracker {
        fields.push("status");
    }
    fields
}

/// Apply a normalized inbound event to a `CanonicalStory` under the constraints
/// of `policy`.
///
/// Rules:
/// - If `event.is_echo` is `true`, nothing is applied and an empty Vec is returned.
///   Echo guard short-circuits before any field check.
/// - For each of the three syncable fields (`title`, `description`, `status`):
///   if `policy.<field>` is `AuthoritativeSide::Tracker` AND the event carries a
///   new value, the story field is updated and the field name is appended to the
///   returned Vec.
/// - Fields whose authoritative side is `Ours` are NEVER updated from inbound,
///   even when the event carries a value.
/// - Provenance, gate results, PR links, and sign-off are not modeled on
///   `CanonicalStory` and are always `Ours` by construction; they are not
///   checked here.
///
/// Returns the list of field names that were actually updated (empty when the
/// event is an echo or when no tracker-authoritative field carried a new value).
pub fn apply_inbound(
    policy: &SyncPolicy,
    story: &mut CanonicalStory,
    event: &InboundWorkItemEvent,
) -> Vec<&'static str> {
    // Guard 1a: echo short-circuit. If the event is an echo of our own write,
    // apply nothing.
    if event.is_echo {
        return Vec::new();
    }

    let mut applied = Vec::new();

    // title
    if policy.title == AuthoritativeSide::Tracker {
        if let Some(new_title) = &event.title {
            story.title = new_title.clone();
            applied.push("title");
        }
    }

    // description
    if policy.description == AuthoritativeSide::Tracker {
        if let Some(new_desc) = &event.description {
            story.description = new_desc.clone();
            applied.push("description");
        }
    }

    // status
    if policy.status == AuthoritativeSide::Tracker {
        if let Some(new_status) = event.status {
            story.status = new_status;
            applied.push("status");
        }
    }

    applied
}

// ── Guard 2: echo suppression via expected-revision table ─────────────────────

/// A record of one outbound write, kept until the corresponding inbound echo
/// arrives and is consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedEcho {
    /// The external item that was written.
    pub reference: ExternalRef,
    /// The revision marker we expect to see on the bounced-back inbound event.
    /// This is the GitHub delivery id, Jira issue version, ADO `rev`, etc.,
    /// that the provider will use to identify the write we just made.
    pub expected_revision: String,
    /// ISO 8601 timestamp of when the write was recorded.
    pub written_at: String,
}

/// In-memory expected-echo table plus delivery-id dedup set.
///
/// Matching key for echo detection: `reference.external_id` AND the event's
/// revision marker. The revision marker is taken from `event.reference.revision`
/// when present; otherwise `event.delivery_id` is used as the fallback. This
/// covers all three providers:
/// - GitHub: delivery id in `X-GitHub-Delivery` appears both as the
///   `delivery_id` and, after writeback, in the revision of the resulting event.
/// - Jira: the incremented `version` field on the issue appears as `revision`.
/// - ADO: the `rev` integer appears as `revision`.
///
/// Delivery-id dedup is independent and catches redeliveries or replayed poll
/// rows that were already processed, regardless of whether they are echoes.
#[derive(Debug, Default)]
pub struct ExpectedEchoTable {
    /// Pending expected echoes, removed on consumption.
    pending: Vec<ExpectedEcho>,
    /// Delivery ids of events already processed. Populated by `record_delivery`.
    seen_deliveries: HashSet<String>,
}

impl ExpectedEchoTable {
    /// Construct an empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that Camerata just wrote to `reference` and expects the resulting
    /// inbound event to carry `expected_revision` as its revision marker.
    pub fn record_write(
        &mut self,
        reference: ExternalRef,
        expected_revision: impl Into<String>,
        written_at: impl Into<String>,
    ) {
        self.pending.push(ExpectedEcho {
            reference,
            expected_revision: expected_revision.into(),
            written_at: written_at.into(),
        });
    }

    /// Returns `true` if `event` matches a recorded expected write.
    ///
    /// Matching: `reference.external_id` must equal the expected entry's
    /// `reference.external_id`, AND the event's revision marker must equal
    /// `expected_revision`. The revision marker is `event.reference.revision`
    /// when `Some`, else `event.delivery_id`.
    ///
    /// Does NOT consume the entry; use `mark_and_consume` on the real ingest
    /// path so each write produces exactly one suppressed echo.
    pub fn is_echo(&self, event: &InboundWorkItemEvent) -> bool {
        let revision_marker = event
            .reference
            .revision
            .as_deref()
            .unwrap_or(&event.delivery_id);

        self.pending.iter().any(|entry| {
            entry.reference.external_id == event.reference.external_id
                && entry.expected_revision == revision_marker
        })
    }

    /// Like `is_echo`, but REMOVES the matched entry so a single write produces
    /// a single suppressed echo. Returns `true` if an entry was found and
    /// consumed, `false` otherwise.
    ///
    /// Use this on the real ingest path; `is_echo` is for read-only inspection.
    pub fn mark_and_consume(&mut self, event: &InboundWorkItemEvent) -> bool {
        let revision_marker = event
            .reference
            .revision
            .as_deref()
            .unwrap_or(&event.delivery_id);

        let pos = self.pending.iter().position(|entry| {
            entry.reference.external_id == event.reference.external_id
                && entry.expected_revision == revision_marker
        });

        match pos {
            Some(idx) => {
                self.pending.remove(idx);
                true
            }
            None => false,
        }
    }

    /// Record that a delivery with `delivery_id` has been processed. Subsequent
    /// calls to `seen_delivery` with the same id return `true`, and
    /// `classify_inbound` returns `Duplicate`.
    pub fn record_delivery(&mut self, delivery_id: impl Into<String>) {
        self.seen_deliveries.insert(delivery_id.into());
    }

    /// Returns `true` if this `delivery_id` has already been processed.
    /// GitHub guarantees duplicate deliveries on retry; this dedup layer
    /// is mandatory on the ingest path.
    pub fn seen_delivery(&self, delivery_id: &str) -> bool {
        self.seen_deliveries.contains(delivery_id)
    }

    /// Classify an inbound event as `Duplicate`, `Echo`, or `Fresh`, then
    /// update internal state accordingly.
    ///
    /// - `Duplicate`: `delivery_id` was already seen. No state change.
    /// - `Echo`: the event matches a recorded expected write (consumed via
    ///   `mark_and_consume`). The delivery id is also recorded as seen.
    /// - `Fresh`: not a duplicate and not an echo. The delivery id is recorded
    ///   as seen.
    ///
    /// This is the one-call front door an ingest loop uses before routing to
    /// `apply_inbound`.
    pub fn classify_inbound(&mut self, event: &InboundWorkItemEvent) -> InboundDisposition {
        // Layer 1: delivery-id dedup. Independent of echo status.
        if self.seen_delivery(&event.delivery_id) {
            return InboundDisposition::Duplicate;
        }

        // Layer 2: echo suppression.
        if self.mark_and_consume(event) {
            self.record_delivery(&event.delivery_id);
            return InboundDisposition::Echo;
        }

        // Fresh event: record the delivery and let the caller proceed.
        self.record_delivery(&event.delivery_id);
        InboundDisposition::Fresh
    }
}

/// Classification of an inbound event, produced by `ExpectedEchoTable::classify_inbound`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundDisposition {
    /// The delivery id was already seen. Drop the event; it is a replay.
    Duplicate,
    /// The event matched a recorded expected write. Drop the event; it is our
    /// own write bouncing back.
    Echo,
    /// A genuinely new external event. Route to `apply_inbound`.
    Fresh,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExternalRef, FeatureStatus, InboundKind, Provider};

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn make_ref(id: &str) -> ExternalRef {
        ExternalRef {
            provider: Provider::GitHub,
            external_id: id.to_string(),
            container: Some("org/repo".to_string()),
            url: format!("https://github.com/org/repo/issues/{id}"),
            revision: None,
        }
    }

    fn make_ref_with_revision(id: &str, revision: &str) -> ExternalRef {
        ExternalRef {
            provider: Provider::GitHub,
            external_id: id.to_string(),
            container: Some("org/repo".to_string()),
            url: format!("https://github.com/org/repo/issues/{id}"),
            revision: Some(revision.to_string()),
        }
    }

    fn make_story(id: &str) -> CanonicalStory {
        CanonicalStory {
            id: id.to_string(),
            external_ref: Some(make_ref(id)),
            title: "Original title".to_string(),
            description: "Original description.".to_string(),
            status: FeatureStatus::Intake,
            created_by: "test-user".to_string(),
        }
    }

    /// Build a basic inbound event with all three syncable fields populated.
    fn make_full_event(reference: ExternalRef, delivery_id: &str) -> InboundWorkItemEvent {
        InboundWorkItemEvent {
            reference,
            kind: InboundKind::Updated,
            title: Some("New title from tracker".to_string()),
            description: Some("New description from tracker.".to_string()),
            status: Some(FeatureStatus::Executing),
            body: None,
            delivery_id: delivery_id.to_string(),
            is_echo: false,
            occurred_at: "2026-06-14T10:00:00Z".to_string(),
        }
    }

    /// Build an event with only a status change.
    fn make_status_event(
        reference: ExternalRef,
        delivery_id: &str,
        status: FeatureStatus,
    ) -> InboundWorkItemEvent {
        InboundWorkItemEvent {
            reference,
            kind: InboundKind::StatusChanged,
            title: None,
            description: None,
            status: Some(status),
            body: None,
            delivery_id: delivery_id.to_string(),
            is_echo: false,
            occurred_at: "2026-06-14T10:00:00Z".to_string(),
        }
    }

    // ── updatable_fields ─────────────────────────────────────────────────────

    #[test]
    fn updatable_fields_greenfield_is_empty() {
        let policy = SyncPolicy::greenfield();
        assert!(
            updatable_fields(&policy).is_empty(),
            "greenfield: no fields are tracker-authoritative"
        );
    }

    #[test]
    fn updatable_fields_brownfield_has_all_three() {
        let policy = SyncPolicy::brownfield();
        let fields = updatable_fields(&policy);
        assert!(
            fields.contains(&"title"),
            "brownfield: title must be listed"
        );
        assert!(
            fields.contains(&"description"),
            "brownfield: description must be listed"
        );
        assert!(
            fields.contains(&"status"),
            "brownfield: status must be listed"
        );
        assert_eq!(fields.len(), 3, "brownfield: exactly three fields");
    }

    #[test]
    fn updatable_fields_status_only_policy() {
        let policy = SyncPolicy {
            title: AuthoritativeSide::Ours,
            description: AuthoritativeSide::Ours,
            status: AuthoritativeSide::Tracker,
        };
        let fields = updatable_fields(&policy);
        assert_eq!(fields, vec!["status"]);
    }

    // ── apply_inbound: greenfield (all Ours) ─────────────────────────────────

    #[test]
    fn apply_inbound_greenfield_updates_nothing() {
        let policy = SyncPolicy::greenfield();
        let mut story = make_story("1");
        let event = make_full_event(make_ref("1"), "delivery-001");

        let applied = apply_inbound(&policy, &mut story, &event);

        assert!(
            applied.is_empty(),
            "greenfield: apply_inbound must update nothing"
        );
        assert_eq!(story.title, "Original title", "title unchanged");
        assert_eq!(
            story.description, "Original description.",
            "description unchanged"
        );
        assert_eq!(story.status, FeatureStatus::Intake, "status unchanged");
    }

    // ── apply_inbound: brownfield (all Tracker) ───────────────────────────────

    #[test]
    fn apply_inbound_brownfield_updates_all_three_when_event_has_all() {
        let policy = SyncPolicy::brownfield();
        let mut story = make_story("2");
        let event = make_full_event(make_ref("2"), "delivery-002");

        let applied = apply_inbound(&policy, &mut story, &event);

        assert!(applied.contains(&"title"), "title must be in applied list");
        assert!(
            applied.contains(&"description"),
            "description must be in applied list"
        );
        assert!(
            applied.contains(&"status"),
            "status must be in applied list"
        );
        assert_eq!(applied.len(), 3, "exactly three fields applied");
        assert_eq!(story.title, "New title from tracker");
        assert_eq!(story.description, "New description from tracker.");
        assert_eq!(story.status, FeatureStatus::Executing);
    }

    #[test]
    fn apply_inbound_brownfield_only_status_when_event_has_only_status() {
        let policy = SyncPolicy::brownfield();
        let mut story = make_story("3");
        let event = make_status_event(make_ref("3"), "delivery-003", FeatureStatus::Done);

        let applied = apply_inbound(&policy, &mut story, &event);

        assert_eq!(applied, vec!["status"], "only status should be applied");
        assert_eq!(story.title, "Original title", "title unchanged");
        assert_eq!(
            story.description, "Original description.",
            "description unchanged"
        );
        assert_eq!(story.status, FeatureStatus::Done, "status updated");
    }

    // ── apply_inbound: echo short-circuit ────────────────────────────────────

    #[test]
    fn apply_inbound_echo_event_applies_nothing_even_with_brownfield_policy() {
        let policy = SyncPolicy::brownfield();
        let mut story = make_story("4");
        let mut event = make_full_event(make_ref("4"), "delivery-004");
        event.is_echo = true;

        let applied = apply_inbound(&policy, &mut story, &event);

        assert!(
            applied.is_empty(),
            "echo: apply_inbound must return empty Vec"
        );
        assert_eq!(story.title, "Original title", "echo: title must not change");
        assert_eq!(
            story.description, "Original description.",
            "echo: description must not change"
        );
        assert_eq!(
            story.status,
            FeatureStatus::Intake,
            "echo: status must not change"
        );
    }

    // ── apply_inbound: mixed policy (status Tracker, title Ours) ─────────────

    #[test]
    fn apply_inbound_mixed_policy_only_tracker_fields_updated() {
        // status is Tracker, title + description are Ours.
        let policy = SyncPolicy {
            title: AuthoritativeSide::Ours,
            description: AuthoritativeSide::Ours,
            status: AuthoritativeSide::Tracker,
        };
        let mut story = make_story("5");
        // Event carries title + description + status.
        let event = make_full_event(make_ref("5"), "delivery-005");

        let applied = apply_inbound(&policy, &mut story, &event);

        assert_eq!(applied, vec!["status"], "only status must be applied");
        assert_eq!(
            story.title, "Original title",
            "Ours title must not be overwritten"
        );
        assert_eq!(
            story.description, "Original description.",
            "Ours description must not be overwritten"
        );
        assert_eq!(
            story.status,
            FeatureStatus::Executing,
            "Tracker status updated"
        );
    }

    // ── ExpectedEchoTable: is_echo / mark_and_consume ─────────────────────────

    #[test]
    fn echo_table_is_echo_matches_expected_revision_via_event_revision() {
        let mut table = ExpectedEchoTable::new();
        table.record_write(make_ref("10"), "rev-abc", "2026-06-14T10:00:00Z");

        // Event whose reference.revision matches.
        let event = InboundWorkItemEvent {
            reference: make_ref_with_revision("10", "rev-abc"),
            kind: InboundKind::Updated,
            title: None,
            description: None,
            status: None,
            body: None,
            delivery_id: "d-100".to_string(),
            is_echo: false,
            occurred_at: "2026-06-14T10:01:00Z".to_string(),
        };

        assert!(
            table.is_echo(&event),
            "matching revision must be detected as echo"
        );
    }

    #[test]
    fn echo_table_is_echo_falls_back_to_delivery_id_when_no_revision() {
        let mut table = ExpectedEchoTable::new();
        // Record using the delivery id as the expected revision (GitHub pattern).
        table.record_write(make_ref("11"), "d-github-xyz", "2026-06-14T10:00:00Z");

        let event = InboundWorkItemEvent {
            reference: make_ref("11"), // no revision field
            kind: InboundKind::Updated,
            title: None,
            description: None,
            status: None,
            body: None,
            delivery_id: "d-github-xyz".to_string(), // used as fallback
            is_echo: false,
            occurred_at: "2026-06-14T10:01:00Z".to_string(),
        };

        assert!(
            table.is_echo(&event),
            "delivery_id fallback must be detected as echo"
        );
    }

    #[test]
    fn echo_table_is_echo_false_for_non_matching_revision() {
        let mut table = ExpectedEchoTable::new();
        table.record_write(make_ref("12"), "rev-expected", "2026-06-14T10:00:00Z");

        let event = InboundWorkItemEvent {
            reference: make_ref_with_revision("12", "rev-different"),
            kind: InboundKind::Updated,
            title: None,
            description: None,
            status: None,
            body: None,
            delivery_id: "d-200".to_string(),
            is_echo: false,
            occurred_at: "2026-06-14T10:01:00Z".to_string(),
        };

        assert!(
            !table.is_echo(&event),
            "non-matching revision must not be detected as echo"
        );
    }

    #[test]
    fn echo_table_mark_and_consume_removes_entry_on_first_call() {
        let mut table = ExpectedEchoTable::new();
        table.record_write(make_ref("13"), "rev-consume", "2026-06-14T10:00:00Z");

        let event = InboundWorkItemEvent {
            reference: make_ref_with_revision("13", "rev-consume"),
            kind: InboundKind::Updated,
            title: None,
            description: None,
            status: None,
            body: None,
            delivery_id: "d-300".to_string(),
            is_echo: false,
            occurred_at: "2026-06-14T10:01:00Z".to_string(),
        };

        // First call: matches and consumes.
        assert!(
            table.mark_and_consume(&event),
            "first call must match and return true"
        );
        // Second call: entry is gone.
        assert!(
            !table.mark_and_consume(&event),
            "second call must return false (entry consumed)"
        );
        // is_echo is also false now.
        assert!(
            !table.is_echo(&event),
            "is_echo must be false after consumption"
        );
    }

    // ── ExpectedEchoTable: delivery-id dedup ─────────────────────────────────

    #[test]
    fn delivery_dedup_seen_after_record() {
        let mut table = ExpectedEchoTable::new();
        assert!(
            !table.seen_delivery("d-dup-1"),
            "unseen delivery must return false"
        );
        table.record_delivery("d-dup-1");
        assert!(
            table.seen_delivery("d-dup-1"),
            "recorded delivery must return true"
        );
        assert!(
            !table.seen_delivery("d-dup-2"),
            "other delivery must still be false"
        );
    }

    // ── classify_inbound ─────────────────────────────────────────────────────

    fn make_simple_event(reference: ExternalRef, delivery_id: &str) -> InboundWorkItemEvent {
        InboundWorkItemEvent {
            reference,
            kind: InboundKind::Updated,
            title: None,
            description: None,
            status: None,
            body: None,
            delivery_id: delivery_id.to_string(),
            is_echo: false,
            occurred_at: "2026-06-14T11:00:00Z".to_string(),
        }
    }

    #[test]
    fn classify_inbound_fresh_on_first_delivery() {
        let mut table = ExpectedEchoTable::new();
        let event = make_simple_event(make_ref("20"), "fresh-001");

        assert_eq!(
            table.classify_inbound(&event),
            InboundDisposition::Fresh,
            "first delivery must be Fresh"
        );
    }

    #[test]
    fn classify_inbound_fresh_then_duplicate() {
        let mut table = ExpectedEchoTable::new();
        let event = make_simple_event(make_ref("21"), "dup-001");

        let first = table.classify_inbound(&event);
        assert_eq!(first, InboundDisposition::Fresh);

        let second = table.classify_inbound(&event);
        assert_eq!(
            second,
            InboundDisposition::Duplicate,
            "replay must be Duplicate"
        );
    }

    #[test]
    fn classify_inbound_echo_when_write_recorded() {
        let mut table = ExpectedEchoTable::new();
        // Simulate Camerata writing to the tracker and recording the echo.
        table.record_write(make_ref("22"), "rev-echo-001", "2026-06-14T11:00:00Z");

        let event = InboundWorkItemEvent {
            reference: make_ref_with_revision("22", "rev-echo-001"),
            kind: InboundKind::Updated,
            title: None,
            description: None,
            status: None,
            body: None,
            delivery_id: "echo-delivery-001".to_string(),
            is_echo: false,
            occurred_at: "2026-06-14T11:01:00Z".to_string(),
        };

        assert_eq!(
            table.classify_inbound(&event),
            InboundDisposition::Echo,
            "event matching a recorded write must be Echo"
        );
    }

    #[test]
    fn classify_inbound_echo_consumed_then_duplicate_if_redelivered() {
        let mut table = ExpectedEchoTable::new();
        table.record_write(make_ref("23"), "rev-once", "2026-06-14T11:00:00Z");

        let event = InboundWorkItemEvent {
            reference: make_ref_with_revision("23", "rev-once"),
            kind: InboundKind::Updated,
            title: None,
            description: None,
            status: None,
            body: None,
            delivery_id: "redelivery-001".to_string(),
            is_echo: false,
            occurred_at: "2026-06-14T11:01:00Z".to_string(),
        };

        // First delivery: Echo (consumes the entry).
        assert_eq!(table.classify_inbound(&event), InboundDisposition::Echo);
        // GitHub redelivers the same event: now Duplicate (delivery id seen).
        assert_eq!(
            table.classify_inbound(&event),
            InboundDisposition::Duplicate
        );
    }

    #[test]
    fn classify_inbound_different_refs_independent() {
        let mut table = ExpectedEchoTable::new();
        // Two independent writes to two different items.
        table.record_write(make_ref("30"), "rev-30", "2026-06-14T12:00:00Z");
        table.record_write(make_ref("31"), "rev-31", "2026-06-14T12:00:00Z");

        let event_30 = InboundWorkItemEvent {
            reference: make_ref_with_revision("30", "rev-30"),
            kind: InboundKind::Updated,
            title: None,
            description: None,
            status: None,
            body: None,
            delivery_id: "d-30".to_string(),
            is_echo: false,
            occurred_at: "2026-06-14T12:01:00Z".to_string(),
        };
        let event_31 = InboundWorkItemEvent {
            reference: make_ref_with_revision("31", "rev-31"),
            kind: InboundKind::Updated,
            title: None,
            description: None,
            status: None,
            body: None,
            delivery_id: "d-31".to_string(),
            is_echo: false,
            occurred_at: "2026-06-14T12:01:00Z".to_string(),
        };
        let event_32_fresh = InboundWorkItemEvent {
            reference: make_ref("32"),
            kind: InboundKind::Created,
            title: None,
            description: None,
            status: None,
            body: None,
            delivery_id: "d-32".to_string(),
            is_echo: false,
            occurred_at: "2026-06-14T12:02:00Z".to_string(),
        };

        assert_eq!(table.classify_inbound(&event_30), InboundDisposition::Echo);
        assert_eq!(table.classify_inbound(&event_31), InboundDisposition::Echo);
        assert_eq!(
            table.classify_inbound(&event_32_fresh),
            InboundDisposition::Fresh
        );
    }
}
