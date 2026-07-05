//! The cross-agent INTEGRATION GATE — the third enforcement tier (GAP-6 / R3.e).
//!
//! Where Layer 1 (the MCP gateway) evaluates ONE write and Layer 2 (the
//! [`crate::multilang`] check runners) evaluate ONE agent's worktree, this tier
//! evaluates the ASSEMBLED tree: the combined outputs of every role agent, run
//! once before the branch ships. It catches the seam breaks no per-agent gate can
//! see — the API agent exposes `POST /members/export`, the UI agent calls
//! `POST /members/csv`, and each diff is individually clean.
//!
//! # Architecture (stack-generalized)
//!
//! ```text
//!   per-repo source ──[ Extractor (per-stack) ]──▶ produced/consumed (neutral vocab)
//!                                                          │
//!                          assemble across all repos ──────┤
//!                                                          ▼
//!                                          [ reconcile engine (100% generic) ]
//!                                                          │
//!                           waivers + review-tier split ───┤
//!                                                          ▼
//!                                                    GateVerdict
//! ```
//!
//! NOTHING about a particular stack lives in the engine. The ONLY stack-aware
//! code is the pluggable [`extractor::Extractor`], selected off the SAME
//! [`crate::multilang::WorktreeLanguage`] detection the Layer-2 linters use. A
//! shared compiled type across a Rust boundary is not a different mechanism: it
//! is simply the case where the extractor emits matching records and the engine
//! finds zero drift.
//!
//! # Determinism (the ADR's hard line)
//!
//! Every verdict is a deterministic comparison of neutral records; no model is
//! ever consulted. Where a seam cannot be made deterministic on a stack (no
//! extractor, or an undeterminable guard status), it is REVIEW-TIER: routed to
//! human QA and honestly labeled, NEVER rendered green.
//!
//! See `docs/decisions/2026-07-05_integration-gate-generic-engine.md` and
//! `docs/decisions/2026-06-15_cross_agent_integration_gate.md`.

pub mod engine;
pub mod extractor;
pub mod gate;
pub mod vocab;

pub use engine::{reconcile, AssembledTree, Finding, FindingLevelKind, SeamRule};
pub use extractor::{
    extractor_endpoint_identity, select_extractors, uncovered_seams, Extractor, Seam,
};
pub use gate::{run_gate, GateRepo, GateVerdict, GateWaiver, ReviewItem};
pub use vocab::{
    normalize_field, normalize_path, ArtifactKind, Consumed, Produced, RepoArtifacts, Shape,
};
