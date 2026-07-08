//! Unit of Work (UoW) — framework-agnostic domain types (`RUST-HEADLESS-CORE-1`).
//!
//! Every pure serde-only leaf type that used to live in this module (the DEV status
//! badge, the AI development history, the per-phase (Intake / Investigation /
//! Development) state shapes, the branch/repo scope, the gate provenance, the sign-off,
//! and the cockpit metadata) was relocated to `camerata_api_types::uow` (Phase A of the
//! DTO extraction) — a pure-serde leaf crate with NO dependency on any other camerata-*
//! crate. Re-exported below so every existing `crate::uow::X` /
//! `camerata_app_core::uow::X` call site keeps resolving unchanged.
//!
//! The aggregate root [`crate`]-external `UnitOfWork` and the `UowStore` (Arc<Mutex> +
//! JSON persistence + artifact-store integration) STAY in the `camerata-server` adapter:
//! `UnitOfWork` embeds an evidence record that transitively needs the adapter's onboard
//! (filesystem/audit) engine, which must never enter this core. The adapter re-exports
//! every type below so `crate::uow::X` call sites resolve unchanged.

pub use camerata_api_types::uow::{
    AuthorChatMessage, AuthoringState, BranchMode, ChatTurn, DevStatus, DevelopmentState,
    GateProvenance, HistoryEntry, IntakeState, InvestigationState, PhaseTab, ProposedChild,
    RepoScope, SignOff, UowAttachment, UowMeta,
};
