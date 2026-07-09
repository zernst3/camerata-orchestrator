//! Unit of Work (UoW) — pure, serde-only wire/domain shapes.
//!
//! These are the pure, serde-only leaf types of a story's dev-side projection: the DEV
//! status badge, the AI development history, the per-phase (Intake / Investigation /
//! Development) state shapes, the branch/repo scope, the gate provenance, the sign-off,
//! and the cockpit metadata. They have NO dependency on any transport framework and NO
//! dependency on any other camerata-* crate.
//!
//! Relocated here (Phase A of the DTO extraction) from `camerata_app_core::uow`, which
//! re-exports every name below so `camerata_app_core::uow::X` call sites resolve
//! unchanged. The aggregate root `UnitOfWork` and the `UowStore` (Arc<Mutex> + JSON
//! persistence + artifact-store integration) STAY in the `camerata-server` adapter:
//! `UnitOfWork` embeds an evidence record that transitively needs the adapter's onboard
//! (filesystem/audit) engine, which must never enter this leaf crate.

use serde::{Deserialize, Serialize};

/// The dev lifecycle status for a story's Unit of Work. Shown ALONGSIDE the
/// story's own tracker status — they are orthogonal: a story can be "Planned"
/// (product) while its UoW is "In Progress" (dev already started).
#[derive(Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum DevStatus {
    /// Dev work has not started for this story.
    #[default]
    New,
    /// Dev work is actively in progress.
    InProgress,
    /// Dev work is complete (code shipped / PR merged / ready for QA).
    Done,
}

impl DevStatus {
    /// Parse from the wire string the API accepts (`"new"`, `"in_progress"`, `"done"`).
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "new" => Some(Self::New),
            "in_progress" => Some(Self::InProgress),
            "done" => Some(Self::Done),
            _ => None,
        }
    }

    /// A short display label for the UI.
    pub fn label(self) -> &'static str {
        match self {
            Self::New => "New",
            Self::InProgress => "In progress",
            Self::Done => "Done",
        }
    }
}

/// A single entry in the AI development history for a UoW. Appended by the
/// governed run (Pillar 2) when it takes an action on this story's behalf; also
/// appendable via the API for manual notes.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    /// RFC 3339 timestamp of the action.
    pub ts: String,
    /// A short kind tag: `"run"`, `"note"`, `"gate_deny"`, `"gate_allow"`, etc.
    pub kind: String,
    /// Human-readable description of what happened.
    pub text: String,
}

/// A child node proposed by the AI during design-mode authoring. When the architect
/// accepts a proposal, each entry is materialized into a new draft UoW via
/// `POST /api/designs/:id/nodes`.
#[derive(Clone, Serialize, Deserialize, Debug, Default, PartialEq, Eq)]
pub struct ProposedChild {
    /// The schema work type for this proposed node (e.g. "Feature", "Story").
    #[serde(default)]
    pub node_type: String,
    /// The proposed issue title.
    #[serde(default)]
    pub title: String,
    /// The proposed issue body (markdown).
    #[serde(default)]
    pub body: String,
}

/// A file attached to a UnitOfWork. The content is stored inline (base64 for binary;
/// raw text for HTML/plain text), travels with the UoW in the JSON store and in project
/// exports, and is embedded into the published GitHub issue body as a collapsed
/// `<details>` block.
///
/// Design note: GitHub's REST API has no "attach file to issue" endpoint (web-only CDN).
/// The decided approach (low-risk, no external deps) is to embed the content directly
/// in the issue body. For HTML mockups this means the full HTML in a `<details>` block;
/// for other types a code-fence snippet. The gist/CDN paths are deferred for Zach to
/// confirm.
#[derive(Clone, Serialize, Deserialize, Debug, Default, PartialEq, Eq)]
pub struct UowAttachment {
    /// A short name identifying this attachment (e.g. `"mockup.html"`, `"arch-diagram.mmd"`).
    pub name: String,
    /// The MIME type (e.g. `"text/html"`, `"text/x-mermaid"`, `"text/plain"`).
    #[serde(default = "default_attachment_mime")]
    pub mime: String,
    /// The attachment content as a UTF-8 string. Binary files must be base64-encoded
    /// before being stored here; the mime type signals to readers how to decode.
    pub content: String,
}

fn default_attachment_mime() -> String {
    "text/plain".to_string()
}

/// The durable gate provenance persisted onto a UoW after a governed run finishes.
///
/// `crate::run::RunProvenance` is the live, derived-on-read summary of a run; this
/// is the FROZEN copy stamped onto the UoW so the governed-development record survives
/// even if the in-memory run is gone (the `RunStore` is in-memory, the UoW persists).
/// It is the honest accounting an architect reviews at QA before signing off.
///
/// # Invariant (BUG-10)
///
/// `total_bounces == deny_count` is an INVARIANT: both fields mean the same count.
/// `total_bounces` uses the architect-facing "bounce" vocabulary in the UI; `deny_count`
/// uses the gate-facing vocabulary in code. They are kept as two fields for API
/// back-compat but MUST be set to identical values. A `debug_assert` fires in
/// `record_gate_provenance` when this invariant is violated. Future callers should
/// prefer the `new` constructor which enforces it at construction time.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct GateProvenance {
    /// The run this provenance came from.
    pub run_id: String,
    /// "scripted" (token-free, real-gate verdicts) or "live".
    pub mode: String,
    /// How many gate verdicts allowed a write.
    pub allow_count: usize,
    /// How many gate verdicts denied a write.
    pub deny_count: usize,
    /// Total bounces the gate sent back (== `deny_count`; named for the architect-
    /// facing vocabulary). Must always equal `deny_count` — see type-level doc.
    pub total_bounces: usize,
    /// The distinct rule ids that actually fired a denial, in first-seen order.
    #[serde(default)]
    pub rules_fired: Vec<String>,
    /// RFC 3339 timestamp of when this provenance was stamped onto the UoW.
    pub recorded: String,
}

impl GateProvenance {
    /// Canonical constructor that enforces the `total_bounces == deny_count` invariant
    /// (BUG-10). Prefer this over struct literals in new code.
    pub fn new(
        run_id: impl Into<String>,
        mode: impl Into<String>,
        allow_count: usize,
        deny_count: usize,
        rules_fired: Vec<String>,
        recorded: impl Into<String>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            mode: mode.into(),
            allow_count,
            deny_count,
            // total_bounces is identical to deny_count by definition; the two fields
            // exist for back-compat vocabulary reasons only.
            total_bounces: deny_count,
            rules_fired,
            recorded: recorded.into(),
        }
    }
}

/// One message in a story-authoring clarification chat. `role` is `"user"` or
/// `"ai"`; `text` is the message body. Persisted on the UoW so the back-and-forth
/// survives sessions until the story is published.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct AuthorChatMessage {
    /// `"user"` (the requirements author) or `"ai"` (the drafting assistant).
    pub role: String,
    /// The message body.
    pub text: String,
}

/// The transient AI story-authoring state carried by a DRAFT UoW (one created via
/// `POST /api/uow/blank` with `work_item = None`). It records the requirements
/// prompt, the clarification chat transcript, and the current AI-drafted issue
/// (title + body). It is preserved on the struct after publish for the record.
///
/// All fields default, so a legacy `uow.json` written before this field existed
/// deserializes with an empty/absent authoring state (back-compat).
#[derive(Clone, Default, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct AuthoringState {
    /// The first user message: the free-text requirements that seed the draft.
    #[serde(default)]
    pub requirements_prompt: String,
    /// The full clarification chat transcript (user + ai turns), in order.
    #[serde(default)]
    pub chat: Vec<AuthorChatMessage>,
    /// The current AI-drafted issue title.
    #[serde(default)]
    pub draft_title: String,
    /// The current AI-drafted issue body (GitHub-flavoured markdown).
    #[serde(default)]
    pub draft_body: String,
}

/// One turn in a per-phase agent chat transcript (investigation or development).
/// `role` is `"user"` or `"agent"`; `text` is the message body. Persisted on the UoW
/// so the back-and-forth refinement session survives sessions (3-phase doc §7).
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct ChatTurn {
    /// `"user"` (the architect) or `"agent"` (the gated working agent).
    pub role: String,
    /// The message body.
    pub text: String,
}

/// The branch mode for one in-scope repo (R6): either work off an EXISTING branch in
/// that repo, or create a NEW UoW-specific branch from a chosen base. Both are
/// first-class options (3-phase doc §3, fleet doc R6).
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum BranchMode {
    /// Work off an existing branch in this repo.
    Existing {
        /// The existing branch name to work off.
        #[serde(default)]
        branch_name: String,
    },
    /// Create a new UoW-specific branch from a chosen base.
    NewFromBase {
        /// The base branch to create the new branch from (e.g. `"main"`).
        #[serde(default)]
        base: String,
        /// The new branch name to create. May be empty when the fleet derives it.
        #[serde(default)]
        new_name: String,
    },
}

impl Default for BranchMode {
    fn default() -> Self {
        Self::NewFromBase {
            base: String::new(),
            new_name: String::new(),
        }
    }
}

/// One in-scope repo for a story, with its branch mode (R6). Out-of-scope repos are not
/// mounted into the agents' read grounding — this set is the token-cost / correctness
/// control the orchestrator's fan-out is bounded to (fleet doc R6).
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct RepoScope {
    /// `OWNER/REPO` for this in-scope repo.
    pub repo: String,
    /// The branch mode for this repo: existing branch vs. new-from-base.
    #[serde(default)]
    pub branch: BranchMode,
}

/// The Intake-phase state for a UoW (3-phase doc §3 / §7). Free-text context for the
/// next agent and the per-story repo/branch scope (R6). All fields default so a legacy
/// `uow.json` written before this field existed deserializes with an empty intake state.
#[derive(Clone, Default, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct IntakeState {
    /// Free-text context for the investigation agent — extra context the story doesn't capture.
    #[serde(default)]
    pub context: String,
    /// The in-scope repos for this story, each with its branch mode (R6). Empty until the
    /// architect selects repos in the Intake scoping UI.
    #[serde(default)]
    pub repos: Vec<RepoScope>,
}

/// The Investigation & Refinement-phase state for a UoW (3-phase doc §4 / §7). Holds the
/// free-text refinement chat transcript and the prose interface contract (R3.g). All
/// fields default for back-compat.
#[derive(Clone, Default, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct InvestigationState {
    /// The investigation/refinement agent chat transcript (user + agent turns), in order.
    #[serde(default)]
    pub chat: Vec<ChatTurn>,
    /// The prose interface contract (R3.g). Free-form prose written into the story; the
    /// cross-repo integration gate reads and checks the assembled code against it. Empty
    /// when no contract has been settled.
    #[serde(default)]
    pub contract: String,
    /// `true` when the architect (or orchestrator) has determined the work crosses a
    /// contract boundary, so a contract is REQUIRED before development (R3.g / §4.6).
    #[serde(default)]
    pub crosses_boundary: bool,
}

/// The Development-phase state for a UoW (3-phase doc §5 / §7). Holds the dev-agent chat
/// transcript (clarification back-and-forth, bug-fix chat). Dev-run output + layer-2
/// results already live in `history` / `gate_provenance`. All fields default for back-compat.
#[derive(Clone, Default, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct DevelopmentState {
    /// The development agent chat transcript (user + agent turns), in order.
    #[serde(default)]
    pub chat: Vec<ChatTurn>,
}

/// Which of the three cockpit phases the user last selected to view. Navigation is FREE —
/// this is informational view state, never drives control flow (3-phase doc §2 / §7).
#[derive(Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum PhaseTab {
    /// Intake phase.
    #[default]
    Intake,
    /// Investigation & Refinement phase.
    Investigation,
    /// Development phase.
    Development,
}

impl PhaseTab {
    /// Parse from the wire string (`"intake"`, `"investigation"`, `"development"`).
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "intake" => Some(Self::Intake),
            "investigation" => Some(Self::Investigation),
            "development" => Some(Self::Development),
            _ => None,
        }
    }
}

/// Per-UoW metadata for the 3-phase cockpit shell (3-phase doc §7 `meta`). The viewed
/// phase, the per-phase finished flags, and the done/archived flag. All fields default
/// so a legacy `uow.json` loads with an empty meta (back-compat). This is the durable
/// home for the soft Finish/Reopen structure the architect uses to separate "what I'm
/// doing now" from "what I've settled" (§2).
#[derive(Clone, Default, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct UowMeta {
    /// Which phase the architect last selected to view.
    #[serde(default)]
    pub viewed_phase: PhaseTab,
    /// `true` when Intake has been Finished (greyed read-only until Reopened).
    #[serde(default)]
    pub intake_finished: bool,
    /// `true` when Investigation & Refinement has been Finished.
    #[serde(default)]
    pub investigation_finished: bool,
    /// `true` when Development has been Finished.
    #[serde(default)]
    pub development_finished: bool,
    /// `true` when the whole UoW is Done (read-only + archived). Never deletes the UoW —
    /// deletion is a separate explicit act (§5.8).
    #[serde(default)]
    pub done: bool,
}

/// An architect's explicit sign-off on a story's governed run (issue #21). Recorded
/// only by the deliberate sign-off action — Camerata never signs work off on its own.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct SignOff {
    /// RFC 3339 timestamp of when the sign-off was recorded.
    pub ts: String,
    /// Who signed off (the architect's handle/name).
    pub by: String,
    /// The run that was signed off (the provenance the architect reviewed).
    pub run_id: String,
    /// An optional note the architect attached to the sign-off.
    #[serde(default)]
    pub note: Option<String>,
}

// ── `GET /api/uows` list view (Phase D: `camerata-client`) ─────────────────────

/// One row of `GET /api/uows`'s response (mirrors the server's private `UowView` in
/// `crates/server/src/lib.rs`): a UoW with the [`crate::workitems::WorkItem`] it
/// references (when resolved) and its lifecycle stage.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct UowListItem {
    /// The UoW id (its story id, e.g. `OWNER/REPO#123`, or `draft-<token>` for an
    /// AI-authoring draft).
    pub id: String,
    /// The work item this UoW references, when it maps to one. `None` for
    /// native/legacy stories with no external ref, and for a draft not yet published.
    #[serde(default)]
    pub work_item: Option<crate::workitems::WorkItem>,
    /// The lifecycle stage as a snake_case wire string (`"intake"`, `"development"`, …).
    #[serde(default)]
    pub stage: String,
    /// `true` when this is a blank/authoring DRAFT UoW (authoring state, no work item yet).
    #[serde(default)]
    pub authoring: bool,
}

/// The response body of `GET /api/uows`: `{ "uows": [...] }` (mirrors
/// `crates/server/src/lib.rs::uows_list`'s `Json(serde_json::json!({ "uows": views }))`).
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct UowListResponse {
    #[serde(default)]
    pub uows: Vec<UowListItem>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bug10_gate_provenance_new_enforces_invariant() {
        let p = GateProvenance::new("run-x", "scripted", 5, 3, vec![], "2026-06-20T00:00:00Z");
        assert_eq!(
            p.total_bounces, p.deny_count,
            "GateProvenance::new must set total_bounces == deny_count"
        );
        assert_eq!(p.deny_count, 3);
        assert_eq!(p.total_bounces, 3);
    }

    #[test]
    fn branch_mode_serializes_tagged() {
        let existing = serde_json::to_value(BranchMode::Existing {
            branch_name: "b".into(),
        })
        .unwrap();
        assert_eq!(existing["mode"], "existing");
        assert_eq!(existing["branch_name"], "b");

        let new = serde_json::to_value(BranchMode::NewFromBase {
            base: "main".into(),
            new_name: "n".into(),
        })
        .unwrap();
        assert_eq!(new["mode"], "new_from_base");
        assert_eq!(new["base"], "main");
        assert_eq!(new["new_name"], "n");
    }

    #[test]
    fn phase_tab_from_wire_round_trips() {
        assert_eq!(PhaseTab::from_wire("intake"), Some(PhaseTab::Intake));
        assert_eq!(
            PhaseTab::from_wire("investigation"),
            Some(PhaseTab::Investigation)
        );
        assert_eq!(
            PhaseTab::from_wire("development"),
            Some(PhaseTab::Development)
        );
        assert_eq!(PhaseTab::from_wire("bogus"), None);
    }
}
