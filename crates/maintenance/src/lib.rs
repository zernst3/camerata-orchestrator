//! camerata-maintenance: the standing post-publish ops agent.
//!
//! This crate owns the ENTIRE post-publish operations function for a Camerata
//! app: dependency upgrades, security patches, key and secret rotation,
//! certificate renewal, backups, health, and general ops hygiene.
//!
//! The governing contract:
//! - The agent NEVER silently changes a live app.
//! - When an update matters (especially a security one), the user gets a calm,
//!   plain-language recommendation first.
//! - Approving a recommendation runs it through the SAME governed build-and-QA
//!   loop as any feature change. Nothing changes outside that gate.
//!
//! See `docs/decisions/2026-06-14_maintenance_ops_agent_and_dependencies.md`
//! and the "Maintenance" section of `docs/CONSUMER_UX.md`.
//!
//! ## Module layout
//!
//! - [`finding`]: [`MaintenanceFinding`], [`MaintenanceKind`], [`Severity`].
//! - [`scan`]: [`MaintenanceScan`], the [`MaintenanceScanner`] trait, and
//!   [`StubScanner`].
//! - [`warning`]: [`security_warning`] (user-facing calm copy).
//! - [`plan`]: [`MaintenancePlan`], [`ApprovalDecision`] (the approval gate).
//! - [`rotation`]: [`KeyRotation`], [`due_rotations`] (rotation schedule).

pub mod finding;
pub mod plan;
pub mod rotation;
pub mod scan;
pub mod warning;

// Flat re-exports so callers can `use camerata_maintenance::*` without
// drilling into submodules.
pub use finding::{MaintenanceFinding, MaintenanceKind, Severity};
pub use plan::{ApprovalDecision, MaintenancePlan};
pub use rotation::{due_rotations, KeyRotation};
pub use scan::{MaintenanceScan, MaintenanceScanner, StubScanner};
pub use warning::security_warning;
