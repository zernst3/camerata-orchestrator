//! Reviewer edits, added findings, merges and splits, and the request and response shapes of
//! their routes (review-tree design section 5, amendment A6 step 2).
//!
//! Every edit is an OVERLAY on the sealed report: it lives in the server's edit store beside the
//! decision store and is applied at read time and at export (`report_export::apply_overlay`). The
//! sealed report never changes. The server (`camerata_server::edits`) and the review UI share
//! these types, so a route's real JSON output is the UI's input by construction.
//!
//! # Fields a reviewer can edit
//!
//! [`EditField::ALL`] is the one list. A test in the UI asserts each field has an edit control
//! and that an edit of each round-trips through the overlay into every export.

use serde::{Deserialize, Serialize};

use crate::audit::FindingKind;
use crate::decisions::Decision;

/// A field of a finding row a reviewer can change before a client sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditField {
    /// The one-line title of the finding.
    Headline,
    /// The explanation under the title.
    Detail,
    /// The fix text for this site.
    Fix,
    /// Whether the fix is labelled a tailored or a standard fix.
    FixType,
    Severity,
    RuleId,
    /// What the row is (defect, deviation, convention, ...). The new value is the kind's tag, or
    /// the kind as JSON when it carries a payload.
    Kind,
    Category,
    /// The sub-group of the review tree the site sits in. A split is one such edit per moved
    /// site, all with the same new sub-group name and the same reason.
    Subgroup,
}

impl EditField {
    /// Every field a reviewer can edit, in the order a form lists them.
    pub const ALL: [EditField; 9] = [
        Self::Headline,
        Self::Detail,
        Self::Fix,
        Self::FixType,
        Self::Severity,
        Self::RuleId,
        Self::Kind,
        Self::Category,
        Self::Subgroup,
    ];

    /// The name serde writes for this field.
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Headline => "headline",
            Self::Detail => "detail",
            Self::Fix => "fix",
            Self::FixType => "fix_type",
            Self::Severity => "severity",
            Self::RuleId => "rule_id",
            Self::Kind => "kind",
            Self::Category => "category",
            Self::Subgroup => "subgroup",
        }
    }
}

/// One stored edit of one field of one site. The store is append-only: two edits on one field are
/// both kept, and the later one applies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edit {
    pub id: String,
    /// The site key of the row.
    pub key: String,
    pub field: EditField,
    /// The value the field had when the edit was made (what a stale edit is shown beside).
    pub old: String,
    pub new: String,
    pub reason: String,
    pub by: String,
    pub at: String,
    /// The inspection (entry id) the edit was made against.
    pub run_id: String,
    /// The fingerprint of the row's rule and snippet when the edit was made. An edit applies only
    /// to a row whose fingerprint is still this.
    pub fingerprint_at_edit: String,
    /// Set on load, never stored as true: the row this edit was made on is gone, moved or changed,
    /// so the edit is shown with its old value and is not applied.
    #[serde(default)]
    pub stale: bool,
    /// When a reviewer discarded this edit (or reapplied it as a new one). A discarded edit is
    /// kept for history and never applied.
    #[serde(default)]
    pub discarded_at: Option<String>,
    /// The stale edit this one reapplies, when it does.
    #[serde(default)]
    pub reapplied_from: Option<String>,
}

impl Edit {
    /// An edit that has not been discarded.
    pub fn is_active(&self) -> bool {
        self.discarded_at.is_none()
    }
}

// ── Requests and responses ────────────────────────────────────────────────────────────────

/// `POST /api/projects/:id/edits`: one edit of one field of one site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewEditReq {
    pub key: String,
    pub field: EditField,
    pub new: String,
    /// Why the reviewer changed it. Required: an edit is the reviewer's claim that the engine's
    /// text was wrong.
    #[serde(default)]
    pub reason: String,
    /// Who edited; blank is recorded as `reviewer`.
    #[serde(default)]
    pub by: String,
    /// The inspection the reviewer was looking at; blank means the latest.
    #[serde(default)]
    pub run_id: String,
}

/// The reply to one edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditRecorded {
    pub edit: Edit,
}

/// `POST /api/projects/:id/edits/split`: move sites to a sub-group of their own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitReq {
    /// The sites to move. Each must be a site of the latest inspection.
    pub keys: Vec<String>,
    /// The name of the sub-group the sites are moved to.
    pub subgroup: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub by: String,
}

/// The reply to a split: one edit per moved site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitRecorded {
    pub edits: Vec<Edit>,
}

/// `POST /api/projects/:id/edits/:edit_id/reapply`: reapply a stale edit to the site it now
/// belongs to, as a new edit with the site's current fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ReapplyReq {
    /// The site to reapply to when it is not the one the stale edit names. Blank means the site
    /// the stale edit's key moved to, when the engine found one.
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub by: String,
}

/// `POST /api/projects/:id/edits/:edit_id/discard`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DiscardReq {
    #[serde(default)]
    pub by: String,
}

/// The reply to a discard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditDiscarded {
    pub edit: Edit,
}

/// A stale edit and where its site went, when the engine found out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleEditView {
    /// The edit, with `stale` set. Its `old` value is what the reviewer saw when they made it.
    pub edit: Edit,
    /// The key the site has now, when it moved (a neighbouring site of the same rule). Blank when
    /// the site is gone.
    #[serde(default)]
    pub current_key: String,
    /// The site's text now, so the reviewer can compare it with `edit.old` and `edit.new`.
    #[serde(default)]
    pub current_value: String,
}

/// `GET /api/projects/:id/edits`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EditsResp {
    /// Every stored edit, discarded ones included, in the order they were written.
    pub edits: Vec<Edit>,
    /// The edits that are not applied to the latest inspection because their site is gone, moved
    /// or changed.
    pub stale: Vec<StaleEditView>,
    /// The newest inspection (entry id), or blank when there is none.
    #[serde(default)]
    pub latest_run_id: String,
}

/// `POST /api/projects/:id/added-findings`: a finding the engine missed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddFindingReq {
    pub repo: String,
    pub path: String,
    pub line: usize,
    pub rule_id: String,
    pub severity: String,
    /// The offending code, quoted. It is checked against the file like any row.
    pub snippet: String,
    pub headline: String,
    pub detail: String,
    #[serde(default)]
    pub fix: String,
    /// What the row is. Required.
    #[serde(default)]
    pub kind: Option<FindingKind>,
    #[serde(default)]
    pub category: Option<String>,
    /// Why the reviewer added it.
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub by: String,
}

/// The reply to an added finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingAdded {
    /// The site key the row has.
    pub key: String,
    /// Whether the evidence check found the snippet near the line. When not, the row is held:
    /// the release gate refuses it until the reviewer decides it.
    pub evidence_verified: bool,
    /// Why the evidence check did not verify it, in words; blank when it did.
    #[serde(default)]
    pub evidence_note: String,
}

/// `POST /api/projects/:id/merge`: merge one site into another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeReq {
    /// The site that is merged away. It stays in the report with a pointer.
    pub key: String,
    /// The site it is merged into.
    pub target: String,
    /// Required.
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub by: String,
}

/// `POST /api/projects/:id/decisions/:decision_id/client-ack`: the client accepted an Ignored
/// finding, recorded after delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientAckReq {
    /// Who at the client acknowledged it.
    pub by: String,
    #[serde(default)]
    pub note: String,
}

/// The reply to a client acknowledgement: the decision as it now stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientAckRecorded {
    pub decision: Decision,
    /// Per-site records a node decision wrote that were acknowledged with it.
    pub also_acknowledged: usize,
}

/// The body of every refusal from the edit routes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditError {
    pub ok: bool,
    /// A short machine-readable reason (`reason_required`, `unknown_site`, `store_corrupt`, ...).
    pub code: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Edit {
        Edit {
            id: "edit-000001".into(),
            key: "acme/app\u{0}src/a.rs\u{0}abc\u{0}0".into(),
            field: EditField::FixType,
            old: "Standard fix".into(),
            new: "Tailored fix".into(),
            reason: "the fix names the right file".into(),
            by: "zach".into(),
            at: "2026-11-04T09:00:00Z".into(),
            run_id: "insp-1".into(),
            fingerprint_at_edit: "61727d97b8fb274a".into(),
            stale: false,
            discarded_at: None,
            reapplied_from: None,
        }
    }

    /// The wire shape of an edit and of every editable field is pinned: a rename is a change to
    /// what the UI reads and the stored files hold, and must be made on purpose.
    #[test]
    fn the_edit_wire_shape_is_pinned() {
        assert_eq!(
            serde_json::to_value(sample()).unwrap(),
            serde_json::json!({
                "id": "edit-000001",
                "key": "acme/app\u{0}src/a.rs\u{0}abc\u{0}0",
                "field": "fix_type",
                "old": "Standard fix",
                "new": "Tailored fix",
                "reason": "the fix names the right file",
                "by": "zach",
                "at": "2026-11-04T09:00:00Z",
                "run_id": "insp-1",
                "fingerprint_at_edit": "61727d97b8fb274a",
                "stale": false,
                "discarded_at": null,
                "reapplied_from": null
            })
        );
        let names: Vec<String> = EditField::ALL
            .iter()
            .map(|f| {
                serde_json::to_value(f)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(
            names,
            [
                "headline", "detail", "fix", "fix_type", "severity", "rule_id", "kind", "category",
                "subgroup"
            ]
        );
        for f in EditField::ALL {
            assert_eq!(serde_json::to_value(f).unwrap(), f.wire_name());
        }
    }

    #[test]
    fn the_route_bodies_wire_shapes_are_pinned() {
        assert_eq!(
            serde_json::to_value(NewEditReq {
                key: "k".into(),
                field: EditField::Severity,
                new: "low".into(),
                reason: "r".into(),
                by: "zach".into(),
                run_id: "insp-1".into(),
            })
            .unwrap(),
            serde_json::json!({
                "key": "k", "field": "severity", "new": "low", "reason": "r",
                "by": "zach", "run_id": "insp-1"
            })
        );
        assert_eq!(
            serde_json::to_value(SplitReq {
                keys: vec!["a".into(), "b".into()],
                subgroup: "test fixtures".into(),
                reason: "different cause".into(),
                by: "zach".into(),
            })
            .unwrap(),
            serde_json::json!({
                "keys": ["a", "b"], "subgroup": "test fixtures",
                "reason": "different cause", "by": "zach"
            })
        );
        assert_eq!(
            serde_json::to_value(MergeReq {
                key: "a".into(),
                target: "b".into(),
                reason: "same cause".into(),
                by: "zach".into(),
            })
            .unwrap(),
            serde_json::json!({
                "key": "a", "target": "b", "reason": "same cause", "by": "zach"
            })
        );
        assert_eq!(
            serde_json::to_value(ClientAckReq {
                by: "cto@acme.test".into(),
                note: "accepted".into(),
            })
            .unwrap(),
            serde_json::json!({ "by": "cto@acme.test", "note": "accepted" })
        );
        assert_eq!(
            serde_json::to_value(FindingAdded {
                key: "k".into(),
                evidence_verified: false,
                evidence_note: "not found".into(),
            })
            .unwrap(),
            serde_json::json!({
                "key": "k", "evidence_verified": false, "evidence_note": "not found"
            })
        );
        assert_eq!(
            serde_json::to_value(EditsResp::default()).unwrap(),
            serde_json::json!({ "edits": [], "stale": [], "latest_run_id": "" })
        );
        assert_eq!(
            serde_json::to_value(EditError {
                ok: false,
                code: "reason_required".into(),
                message: "m".into(),
            })
            .unwrap(),
            serde_json::json!({ "ok": false, "code": "reason_required", "message": "m" })
        );
        let req: AddFindingReq = serde_json::from_value(serde_json::json!({
            "repo": "acme/app", "path": "src/a.rs", "line": 3, "rule_id": "SEC-1",
            "severity": "high", "snippet": "x", "headline": "h", "detail": "d",
            "kind": { "kind": "defect" }
        }))
        .unwrap();
        assert_eq!(req.kind, Some(FindingKind::Defect));
        assert!(req.fix.is_empty() && req.reason.is_empty());
    }
}
