//! Post-story documentation hook (PROC-STORY-DOCS-1).
//!
//! When a governed story reaches sign-off, Camerata emits durable, deterministic
//! draft documentation into the workspace repo so the architect can review and
//! refine it during the PR process — no LLM call required by default.
//!
//! # Architecture
//!
//! [`PostStoryHook`] is the trait. A value implementing it is optionally attached to
//! the [`crate::server::UowStore`] (via the builder API) and called inside
//! `UowStore::sign_off` once the sign-off is persisted.
//!
//! [`StoryDocEmitter`] is the provided implementation. Its behaviour is governed by
//! [`DocConvention`], which maps to the `PROC-STORY-DOCS-1` option chosen for the
//! project:
//!
//! | Convention            | Behaviour                                        |
//! |-----------------------|--------------------------------------------------|
//! | `PerStoryDocs`        | Emit two DRAFT files per story (default).        |
//! | Any other variant     | No-op (return empty vec).                        |
//!
//! The emitted files are:
//!
//! - `docs/<story-id>/technical/<story-id>-dev.md` — developer-facing rationale,
//!   design decisions, and implementation notes.
//! - `docs/<story-id>/user/<story-id>-guide.md` — user-facing change summary and
//!   usage guidance.
//!
//! Both files are assembled deterministically from the story's decision records and
//! the run summary. This keeps the emitter hermetic and unit-testable: no LLM call,
//! no network IO, only filesystem writes and pure string assembly.
//!
//! An optional LLM-polish pass is NOT included but is easy to add behind a flag:
//! just add a `lm_polish: Option<Box<dyn LlmPolish>>` to [`StoryDocEmitter`] and
//! call it after assembly if present. The default path stays deterministic.
//!
//! # File writability contract
//!
//! Parent directories are created when they do not exist (using `create_dir_all`).
//! Idempotent: re-emitting the same story id overwrites the previous drafts, so a
//! hook fired more than once (e.g. re-sign-off after a revision) produces the same
//! result.
//!
//! # PROC-STORY-DOCS-1 conventions honored
//!
//! - Path key is the story id tracked by the dev console and the UoW: the agent
//!   places docs deterministically without additional configuration.
//! - Technical vs user-facing content is split across two separate files to keep
//!   developer context out of the user guide.
//! - Files are DRAFT: the PR review step is where the architect refines them.

use std::path::{Path, PathBuf};

use camerata_worktracker::investigation::{DecisionOutcome, DecisionRecord};

// ── DocConvention ─────────────────────────────────────────────────────────────

/// Maps to the chosen `PROC-STORY-DOCS-1` alternative.
///
/// Only `PerStoryDocs` (the default) causes the emitter to write files; all
/// other variants are treated as explicit no-ops so a project that chose a
/// different documentation strategy is not surprised by file creation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DocConvention {
    /// The default: emit `docs/<story-id>/technical/` and `docs/<story-id>/user/`.
    #[default]
    PerStoryDocs,
    /// Living central docs — appends to TECHNICAL.md + USER_GUIDE.md. Not
    /// implemented here; the emitter no-ops for this variant.
    LivingCentralDocs,
    /// ADR per change — emit one ADR file per story. Not implemented here; the
    /// emitter no-ops for this variant.
    AdrPerChange,
    /// Mechanical minimum — no in-repo docs beyond the commit/PR record. The
    /// emitter always no-ops for this variant.
    MechanicalMinimum,
}

impl DocConvention {
    /// Parse from the `chosen_option` wire string used in the project ruleset
    /// (`RuleSelection.chosen_option`). Returns `None` for unknown strings so the
    /// caller can fall back to the default.
    pub fn from_option_id(s: &str) -> Option<Self> {
        match s {
            "per-story-docs" => Some(Self::PerStoryDocs),
            "living-central-docs" => Some(Self::LivingCentralDocs),
            "adr-per-change" => Some(Self::AdrPerChange),
            "mechanical-minimum" => Some(Self::MechanicalMinimum),
            _ => None,
        }
    }

    /// The canonical option id string for use in serialization and debugging.
    pub fn as_option_id(&self) -> &'static str {
        match self {
            Self::PerStoryDocs => "per-story-docs",
            Self::LivingCentralDocs => "living-central-docs",
            Self::AdrPerChange => "adr-per-change",
            Self::MechanicalMinimum => "mechanical-minimum",
        }
    }
}

// ── StoryCompletion ───────────────────────────────────────────────────────────

/// All the context a [`PostStoryHook`] receives at story sign-off time.
///
/// This is a snapshot: the hook must not call back into the `UowStore` or any
/// other live state — everything it needs is here.
#[derive(Debug, Clone)]
pub struct StoryCompletion {
    /// The story id (e.g. `"CAM-42"`). Becomes the directory key under `docs/`.
    pub story_id: String,
    /// The frozen decision records for this story at sign-off time. Used to
    /// assemble the technical rationale section of the developer doc.
    pub decisions: Vec<DecisionRecord>,
    /// A plain-text summary of the run: gate verdicts, outcome, run id. Used in
    /// both generated files.
    pub run_summary: String,
    /// The ABSOLUTE path to the root of the workspace (the repo being governed).
    /// Docs are written under `<workspace_root>/docs/<story_id>/`.
    pub workspace_root: PathBuf,
    /// RFC 3339 timestamp of the sign-off event. Stamped into the file headers.
    pub signed_off_at: String,
}

// ── PostStoryHook trait ───────────────────────────────────────────────────────

/// Object-safe hook called once after a story's UoW reaches the `SignedOff`
/// stage. Implementors decide what (if anything) to do.
///
/// # Object safety + send + sync
///
/// The trait is object-safe (no generic methods) and is bounded `Send + Sync` so
/// it can be stored in an `Arc<dyn PostStoryHook + Send + Sync>` on the `UowStore`,
/// which is `Clone + Send + Sync`.
///
/// # Contract
///
/// - MUST be idempotent: calling `emit` twice with the same `completion` must
///   produce the same output. File-writing implementations satisfy this by
///   overwriting.
/// - MUST NOT call back into the `UowStore` or mutate any shared state beyond
///   the filesystem path declared by `completion.workspace_root`.
/// - Any I/O error is returned as `anyhow::Error`; the caller logs it and
///   continues — a doc-write failure must never block sign-off.
pub trait PostStoryHook: Send + Sync {
    /// Emit documentation (or perform any other post-story action) for the
    /// completed story. Returns the list of files written (empty for a no-op).
    fn emit(&self, completion: &StoryCompletion) -> anyhow::Result<Vec<PathBuf>>;
}

// ── StoryDocEmitter ───────────────────────────────────────────────────────────

/// Emits two deterministic DRAFT markdown docs per story when the project's
/// doc convention is [`DocConvention::PerStoryDocs`] (the PROC-STORY-DOCS-1 default).
///
/// For any other convention, `emit` is a no-op that returns `Ok(vec![])`.
///
/// # Determinism guarantee
///
/// Content is assembled entirely from `StoryCompletion` fields: story id,
/// decision records, run summary, and timestamp. No LLM call, no network IO,
/// no random values. Tests can assert on exact output or structural properties
/// (section presence, path correctness) without stubs or mocks.
#[derive(Debug, Clone)]
pub struct StoryDocEmitter {
    /// The doc convention to honour. Defaults to [`DocConvention::PerStoryDocs`].
    convention: DocConvention,
}

impl StoryDocEmitter {
    /// Create an emitter for the given convention. Pass
    /// [`DocConvention::PerStoryDocs`] (the default) to get active emission.
    pub fn new(convention: DocConvention) -> Self {
        Self { convention }
    }

    /// Create an emitter using the default convention (`PerStoryDocs`).
    pub fn default_convention() -> Self {
        Self {
            convention: DocConvention::default(),
        }
    }

    // ── path helpers ─────────────────────────────────────────────────────────

    /// Absolute path for the technical doc: `<root>/docs/<id>/technical/<id>-dev.md`.
    pub fn technical_path(workspace_root: &Path, story_id: &str) -> PathBuf {
        workspace_root
            .join("docs")
            .join(story_id)
            .join("technical")
            .join(format!("{story_id}-dev.md"))
    }

    /// Absolute path for the user guide: `<root>/docs/<id>/user/<id>-guide.md`.
    pub fn user_path(workspace_root: &Path, story_id: &str) -> PathBuf {
        workspace_root
            .join("docs")
            .join(story_id)
            .join("user")
            .join(format!("{story_id}-guide.md"))
    }

    // ── content assembly ──────────────────────────────────────────────────────

    /// Assemble the technical (developer-facing) DRAFT document.
    ///
    /// Sections: YAML front-matter header, run summary, decisions table, and a
    /// "Next steps" stub the architect can expand.
    pub fn assemble_technical(completion: &StoryCompletion) -> String {
        let mut out = String::with_capacity(2048);

        // YAML-style front-matter (not parsed by any processor; purely informational).
        out.push_str("---\n");
        out.push_str(&format!("story_id: {}\n", completion.story_id));
        out.push_str("status: DRAFT\n");
        out.push_str(&format!("signed_off_at: {}\n", completion.signed_off_at));
        out.push_str("audience: developer\n");
        out.push_str("---\n\n");

        out.push_str(&format!(
            "# {} — Technical Notes (DRAFT)\n\n",
            completion.story_id
        ));
        out.push_str(
            "> **DRAFT** — This file was emitted automatically at sign-off. \
             Edit it in the PR to add implementation detail, architecture notes, \
             and lessons learned.\n\n",
        );

        // Run summary section.
        out.push_str("## Governed run summary\n\n");
        if completion.run_summary.is_empty() {
            out.push_str("_(No run summary recorded.)_\n\n");
        } else {
            out.push_str(&completion.run_summary);
            out.push_str("\n\n");
        }

        // Decisions section.
        out.push_str("## Design decisions\n\n");
        if completion.decisions.is_empty() {
            out.push_str("_(No decision records were captured for this story.)_\n\n");
        } else {
            out.push_str(
                "The following decisions were surfaced during investigation and \
                 approved before development started.\n\n",
            );
            for (i, d) in completion.decisions.iter().enumerate() {
                let state_badge = match &d.outcome {
                    DecisionOutcome::Approved => "Approved",
                    DecisionOutcome::Pending => "Pending",
                    DecisionOutcome::Rejected { .. } => "Rejected",
                };
                out.push_str(&format!(
                    "### Decision {}: {}\n\n",
                    i + 1,
                    d.label
                ));
                out.push_str(&format!("**State:** {state_badge}\n\n"));
                out.push_str(&format!("**Question:** {}\n\n", d.question));
                out.push_str(&format!("**Rationale:** {}\n\n", d.rationale));
                if !d.alternatives_considered.is_empty() {
                    out.push_str("**Alternatives considered:**\n\n");
                    for alt in &d.alternatives_considered {
                        out.push_str(&format!("- {alt}\n"));
                    }
                    out.push('\n');
                }
            }
        }

        // Placeholder next-steps section.
        out.push_str("## Implementation notes\n\n");
        out.push_str(
            "_(Fill in: key code paths, notable tradeoffs in the implementation, \
             test coverage notes, known limitations.)_\n\n",
        );

        out.push_str("## Follow-on work\n\n");
        out.push_str("_(Fill in: any items punted from this story's scope.)_\n");

        out
    }

    /// Assemble the user guide (user-facing) DRAFT document.
    ///
    /// Sections: header, overview blurb, what changed, how to use, migration steps.
    pub fn assemble_user_guide(completion: &StoryCompletion) -> String {
        let mut out = String::with_capacity(1024);

        // YAML-style front-matter.
        out.push_str("---\n");
        out.push_str(&format!("story_id: {}\n", completion.story_id));
        out.push_str("status: DRAFT\n");
        out.push_str(&format!("signed_off_at: {}\n", completion.signed_off_at));
        out.push_str("audience: user\n");
        out.push_str("---\n\n");

        out.push_str(&format!(
            "# {} — User Guide (DRAFT)\n\n",
            completion.story_id
        ));
        out.push_str(
            "> **DRAFT** — This file was emitted automatically at sign-off. \
             Edit it in the PR to describe the feature from the user's perspective, \
             remove or expand sections as needed.\n\n",
        );

        // Summary section: high-level what-changed.
        out.push_str("## What changed\n\n");
        out.push_str(&format!(
            "Story **{}** shipped changes to the product. \
             This section should describe, in plain language, what is new or \
             different from the user's point of view.\n\n",
            completion.story_id
        ));

        // Key capabilities derived from approved decisions.
        let approved: Vec<&DecisionRecord> = completion
            .decisions
            .iter()
            .filter(|d| matches!(d.outcome, DecisionOutcome::Approved))
            .collect();
        if !approved.is_empty() {
            out.push_str("Key architectural choices that affect the user experience:\n\n");
            for d in &approved {
                out.push_str(&format!("- **{}**: {}\n", d.label, d.rationale));
            }
            out.push('\n');
        }

        // How-to section.
        out.push_str("## How to use this feature\n\n");
        out.push_str("_(Fill in: step-by-step instructions from the user's perspective.)_\n\n");

        // Migration steps.
        out.push_str("## Migration steps\n\n");
        out.push_str(
            "_(Fill in: any changes required to existing data, configuration, or \
             workflows. If no migration is needed, delete this section.)_\n\n",
        );

        // Known limitations.
        out.push_str("## Known limitations\n\n");
        out.push_str(
            "_(Fill in: caveats, edge cases, or behaviours the user should be aware of.)_\n",
        );

        out
    }
}

impl PostStoryHook for StoryDocEmitter {
    /// Emit draft docs for the story if the convention is [`DocConvention::PerStoryDocs`].
    ///
    /// Returns `Ok(vec![technical_path, user_path])` when files were written.
    /// Returns `Ok(vec![])` for any other convention.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a required directory cannot be created or a file cannot
    /// be written. The caller (the sign-off path) treats this as non-fatal: the
    /// sign-off is already persisted by the time the hook fires.
    fn emit(&self, completion: &StoryCompletion) -> anyhow::Result<Vec<PathBuf>> {
        if self.convention != DocConvention::PerStoryDocs {
            // Explicit no-op for non-default conventions (PROC-STORY-DOCS-1).
            return Ok(vec![]);
        }

        let tech_path = Self::technical_path(&completion.workspace_root, &completion.story_id);
        let user_path = Self::user_path(&completion.workspace_root, &completion.story_id);

        // Ensure parent directories exist.
        if let Some(dir) = tech_path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| {
                anyhow::anyhow!(
                    "failed to create technical docs directory {}: {e}",
                    dir.display()
                )
            })?;
        }
        if let Some(dir) = user_path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| {
                anyhow::anyhow!(
                    "failed to create user docs directory {}: {e}",
                    dir.display()
                )
            })?;
        }

        // Assemble and write the technical doc.
        let tech_content = Self::assemble_technical(completion);
        std::fs::write(&tech_path, &tech_content).map_err(|e| {
            anyhow::anyhow!(
                "failed to write technical doc {}: {e}",
                tech_path.display()
            )
        })?;

        // Assemble and write the user guide.
        let user_content = Self::assemble_user_guide(completion);
        std::fs::write(&user_path, &user_content).map_err(|e| {
            anyhow::anyhow!(
                "failed to write user guide {}: {e}",
                user_path.display()
            )
        })?;

        Ok(vec![tech_path, user_path])
    }
}

// ── tests (ORCH-NEW-PATH-TESTS-1) ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use camerata_worktracker::investigation::{DecisionOutcome, DecisionRecord, RevisionProvenance, RevisionActor};
    use chrono::Utc;

    fn approved_decision(story: &str, slug: &str, label: &str, rationale: &str) -> DecisionRecord {
        DecisionRecord {
            artifact_id: format!("{story}/decision/{slug}"),
            story_id: story.to_string(),
            label: label.to_string(),
            question: format!("Is {label} the right approach?"),
            rationale: rationale.to_string(),
            alternatives_considered: vec!["Alternative A".to_string()],
            outcome: DecisionOutcome::Approved,
            provenance: RevisionProvenance::new(RevisionActor::User, Utc::now()),
        }
    }

    fn pending_decision(story: &str, slug: &str) -> DecisionRecord {
        DecisionRecord {
            artifact_id: format!("{story}/decision/{slug}"),
            story_id: story.to_string(),
            label: "Pending choice".to_string(),
            question: "Which approach?".to_string(),
            rationale: "TBD".to_string(),
            alternatives_considered: vec![],
            outcome: DecisionOutcome::Pending,
            provenance: RevisionProvenance::new(RevisionActor::Ai, Utc::now()),
        }
    }

    fn completion_in(dir: &std::path::Path, story_id: &str) -> StoryCompletion {
        StoryCompletion {
            story_id: story_id.to_string(),
            decisions: vec![
                approved_decision(story_id, "auth", "JWT authentication", "Stateless, works across services"),
                pending_decision(story_id, "pagination"),
            ],
            run_summary: format!("Run run-1 completed: 1 allow, 2 deny, 2 bounces."),
            workspace_root: dir.to_path_buf(),
            signed_off_at: "2026-06-20T12:00:00Z".to_string(),
        }
    }

    // ── DocConvention ─────────────────────────────────────────────────────────

    #[test]
    fn doc_convention_default_is_per_story_docs() {
        assert_eq!(DocConvention::default(), DocConvention::PerStoryDocs);
    }

    #[test]
    fn doc_convention_from_option_id_round_trips_all_variants() {
        for (id, expected) in [
            ("per-story-docs", DocConvention::PerStoryDocs),
            ("living-central-docs", DocConvention::LivingCentralDocs),
            ("adr-per-change", DocConvention::AdrPerChange),
            ("mechanical-minimum", DocConvention::MechanicalMinimum),
        ] {
            let parsed = DocConvention::from_option_id(id).expect("must parse");
            assert_eq!(parsed, expected, "option id '{id}'");
            assert_eq!(parsed.as_option_id(), id, "round-trip for '{id}'");
        }
    }

    #[test]
    fn doc_convention_from_option_id_returns_none_for_unknown() {
        assert!(DocConvention::from_option_id("unknown-id").is_none());
        assert!(DocConvention::from_option_id("").is_none());
    }

    // ── Path helpers ──────────────────────────────────────────────────────────

    #[test]
    fn technical_path_is_under_docs_technical_dir() {
        let root = PathBuf::from("/workspace/my-repo");
        let path = StoryDocEmitter::technical_path(&root, "CAM-42");
        assert_eq!(
            path,
            PathBuf::from("/workspace/my-repo/docs/CAM-42/technical/CAM-42-dev.md")
        );
    }

    #[test]
    fn user_path_is_under_docs_user_dir() {
        let root = PathBuf::from("/workspace/my-repo");
        let path = StoryDocEmitter::user_path(&root, "CAM-42");
        assert_eq!(
            path,
            PathBuf::from("/workspace/my-repo/docs/CAM-42/user/CAM-42-guide.md")
        );
    }

    #[test]
    fn paths_are_keyed_by_story_id() {
        let root = PathBuf::from("/repo");
        let path_a = StoryDocEmitter::technical_path(&root, "CAM-1");
        let path_b = StoryDocEmitter::technical_path(&root, "CAM-2");
        assert_ne!(path_a, path_b, "paths must differ by story id");
        assert!(path_a.to_string_lossy().contains("CAM-1"));
        assert!(path_b.to_string_lossy().contains("CAM-2"));
    }

    // ── Content assembly ──────────────────────────────────────────────────────

    #[test]
    fn technical_doc_contains_story_id_and_decisions() {
        let dir = tempfile::tempdir().unwrap();
        let c = completion_in(dir.path(), "CAM-10");
        let content = StoryDocEmitter::assemble_technical(&c);
        assert!(content.contains("CAM-10"), "story id must appear in technical doc");
        assert!(content.contains("JWT authentication"), "decision label must appear");
        assert!(content.contains("Stateless, works across services"), "rationale must appear");
        assert!(content.contains("Governed run summary"), "run summary section must be present");
        assert!(content.contains("DRAFT"), "file must be marked DRAFT");
    }

    #[test]
    fn technical_doc_run_summary_section_contains_summary() {
        let dir = tempfile::tempdir().unwrap();
        let c = completion_in(dir.path(), "CAM-11");
        let content = StoryDocEmitter::assemble_technical(&c);
        assert!(
            content.contains("Run run-1 completed"),
            "run summary text must appear in the technical doc"
        );
    }

    #[test]
    fn technical_doc_lists_all_decisions_including_pending() {
        let dir = tempfile::tempdir().unwrap();
        let c = completion_in(dir.path(), "CAM-12");
        let content = StoryDocEmitter::assemble_technical(&c);
        // One approved, one pending — both must appear.
        assert!(content.contains("Approved"), "approved state badge");
        assert!(content.contains("Pending"), "pending state badge");
        assert!(content.contains("Pending choice"), "pending decision label");
    }

    #[test]
    fn technical_doc_alternatives_considered_are_included() {
        let dir = tempfile::tempdir().unwrap();
        let c = completion_in(dir.path(), "CAM-13");
        let content = StoryDocEmitter::assemble_technical(&c);
        assert!(content.contains("Alternative A"), "alternatives_considered must appear");
    }

    #[test]
    fn user_guide_contains_story_id_and_approved_decisions() {
        let dir = tempfile::tempdir().unwrap();
        let c = completion_in(dir.path(), "CAM-20");
        let content = StoryDocEmitter::assemble_user_guide(&c);
        assert!(content.contains("CAM-20"), "story id must appear in user guide");
        // Approved decision rationale is summarised in the user guide.
        assert!(content.contains("Stateless, works across services"), "approved rationale in guide");
        assert!(content.contains("DRAFT"), "file must be marked DRAFT");
        assert!(content.contains("What changed"), "what-changed section must be present");
    }

    #[test]
    fn user_guide_only_mentions_approved_decisions_not_pending() {
        let dir = tempfile::tempdir().unwrap();
        let c = completion_in(dir.path(), "CAM-21");
        let content = StoryDocEmitter::assemble_user_guide(&c);
        // Only approved decisions are surfaced to the user guide.
        assert!(!content.contains("Pending choice"), "pending decisions must NOT appear in user guide");
        assert!(content.contains("JWT authentication"), "approved decision label appears");
    }

    #[test]
    fn empty_decisions_produces_valid_documents() {
        let dir = tempfile::tempdir().unwrap();
        let c = StoryCompletion {
            story_id: "CAM-0".to_string(),
            decisions: vec![],
            run_summary: "No events.".to_string(),
            workspace_root: dir.path().to_path_buf(),
            signed_off_at: "2026-06-20T00:00:00Z".to_string(),
        };
        let tech = StoryDocEmitter::assemble_technical(&c);
        let guide = StoryDocEmitter::assemble_user_guide(&c);
        // Must not panic, and must produce a non-empty document.
        assert!(!tech.is_empty());
        assert!(!guide.is_empty());
        assert!(tech.contains("No decision records"), "empty-decisions message in technical doc");
    }

    // ── PostStoryHook::emit — per-story-docs (active path) ───────────────────

    #[test]
    fn emit_per_story_docs_creates_both_files() {
        let dir = tempfile::tempdir().unwrap();
        let emitter = StoryDocEmitter::new(DocConvention::PerStoryDocs);
        let c = completion_in(dir.path(), "CAM-30");
        let written = emitter.emit(&c).unwrap();
        assert_eq!(written.len(), 2, "exactly two files must be written");
        let tech = StoryDocEmitter::technical_path(dir.path(), "CAM-30");
        let user = StoryDocEmitter::user_path(dir.path(), "CAM-30");
        assert!(tech.exists(), "technical doc must exist on disk");
        assert!(user.exists(), "user guide must exist on disk");
        assert!(written.contains(&tech));
        assert!(written.contains(&user));
    }

    #[test]
    fn emit_per_story_docs_content_includes_decisions() {
        let dir = tempfile::tempdir().unwrap();
        let emitter = StoryDocEmitter::new(DocConvention::PerStoryDocs);
        let c = completion_in(dir.path(), "CAM-31");
        emitter.emit(&c).unwrap();
        let tech_content = std::fs::read_to_string(
            StoryDocEmitter::technical_path(dir.path(), "CAM-31")
        ).unwrap();
        assert!(tech_content.contains("JWT authentication"));
        assert!(tech_content.contains("Stateless, works across services"));
    }

    #[test]
    fn emit_is_idempotent_re_emit_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let emitter = StoryDocEmitter::new(DocConvention::PerStoryDocs);
        let c = completion_in(dir.path(), "CAM-32");
        let first = emitter.emit(&c).unwrap();
        let second = emitter.emit(&c).unwrap();
        // Both calls succeed and return the same paths.
        assert_eq!(first, second, "re-emit must return the same paths");
        // Files still exist (overwritten, not errored).
        assert!(
            StoryDocEmitter::technical_path(dir.path(), "CAM-32").exists()
        );
    }

    #[test]
    fn emit_creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let emitter = StoryDocEmitter::new(DocConvention::PerStoryDocs);
        let c = completion_in(dir.path(), "PROJ-999");
        // The parent dirs do not exist yet — emit must create them.
        let tech = StoryDocEmitter::technical_path(dir.path(), "PROJ-999");
        assert!(!tech.parent().unwrap().exists(), "setup: dir must not exist yet");
        emitter.emit(&c).unwrap();
        assert!(tech.parent().unwrap().exists(), "emit must create the parent directory");
    }

    // ── PostStoryHook::emit — no-op variants ─────────────────────────────────

    #[test]
    fn emit_living_central_docs_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let emitter = StoryDocEmitter::new(DocConvention::LivingCentralDocs);
        let c = completion_in(dir.path(), "CAM-40");
        let written = emitter.emit(&c).unwrap();
        assert!(written.is_empty(), "living-central-docs must be a no-op");
        // No files created.
        assert!(
            !StoryDocEmitter::technical_path(dir.path(), "CAM-40").exists()
        );
    }

    #[test]
    fn emit_adr_per_change_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let emitter = StoryDocEmitter::new(DocConvention::AdrPerChange);
        let c = completion_in(dir.path(), "CAM-41");
        let written = emitter.emit(&c).unwrap();
        assert!(written.is_empty(), "adr-per-change must be a no-op");
    }

    #[test]
    fn emit_mechanical_minimum_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let emitter = StoryDocEmitter::new(DocConvention::MechanicalMinimum);
        let c = completion_in(dir.path(), "CAM-42");
        let written = emitter.emit(&c).unwrap();
        assert!(written.is_empty(), "mechanical-minimum must be a no-op");
    }

    // ── Default-convention shortcut ───────────────────────────────────────────

    #[test]
    fn default_convention_emitter_behaves_as_per_story_docs() {
        let dir = tempfile::tempdir().unwrap();
        let emitter = StoryDocEmitter::default_convention();
        let c = completion_in(dir.path(), "CAM-50");
        let written = emitter.emit(&c).unwrap();
        assert_eq!(written.len(), 2, "default convention emits two files");
    }

    // ── Trait object usability ────────────────────────────────────────────────

    #[test]
    fn story_doc_emitter_is_usable_as_trait_object() {
        let dir = tempfile::tempdir().unwrap();
        let hook: Box<dyn PostStoryHook> = Box::new(StoryDocEmitter::default_convention());
        let c = completion_in(dir.path(), "CAM-60");
        let written = hook.emit(&c).unwrap();
        assert_eq!(written.len(), 2);
    }
}
