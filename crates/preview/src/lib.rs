//! camerata-preview: a local-first adapter that manages a Dioxus `dx serve` process and
//! exposes its build/reload status. This turns the de-risking spike in
//! `docs/spikes/2026-07-09_dioxus-live-preview-spike.md` into a real, testable adapter.
//!
//! Three layers, in dependency order:
//! - [`parser`] -- pure: classify one dx stdout/stderr line into a [`parser::PreviewEvent`].
//! - [`status`] -- pure: fold a [`parser::PreviewEvent`] stream into a [`status::PreviewStatus`].
//! - [`process`] -- the process manager: [`process::PreviewServer`] spawns `dx serve`,
//!   streams its output through the two pure layers above, and owns the subprocess lifecycle.
//! - [`verify`] -- closes the spike's headline risk (a syntax-invalid edit is silently
//!   dropped with zero dx-log output): [`verify::verify_after_edit`] and its pure decision
//!   core [`verify::decide_edit_verdict`].
//!
//! Local-first by design: this crate only ever spawns a subprocess on the same machine.
//! Cloud tunneling (sharing a preview URL off-box) is a later phase per the design doc
//! (`docs/plans/2026-07-09_product-owner-head-vibe-mode.md`) and is deliberately out of
//! scope here.

pub mod parser;
pub mod process;
pub mod status;
pub mod verify;

pub use parser::{parse_dx_line, PreviewEvent};
pub use process::{dx_bin, PreviewLaunchConfig, PreviewServer};
pub use status::{fold, fold_all, PreviewStatus};
pub use verify::{decide_edit_verdict, verify_after_edit, CargoCheckResult, EditVerdict};
