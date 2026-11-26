//! A finished run's two run figures sources, as the app receives them: the saved report
//! (`acme-portal-report.json`, the shape the engine writes; the server's own test parses it as
//! its report type) and the run folder's figures (`acme-portal-figures.json`, the body of
//! `GET /api/inspections/:id/figures`). Neutral names only. Each source carries figures of its
//! own that the other does not, so a panel wired to the wrong source shows the wrong number.
//! The gallery reads these too, so they are always compiled.

/// The saved report: scope, the stage figures on its ledger, and one finding with a path and a
/// code excerpt the panel must never show.
pub const REPORT_JSON: &str = include_str!("run_figures/acme-portal-report.json");

/// The run folder's figures: two sessions, and batches in all three states.
pub const FIGURES_JSON: &str = include_str!("run_figures/acme-portal-figures.json");

/// The saved run's id, as the report names it.
pub const RUN_ID: &str = "20261124T101500Z-a1b2c3";
