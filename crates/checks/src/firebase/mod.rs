//! Firebase / Firestore coverage (MASTER-SEQUENCE item 24; design:
//! `docs/design/2026-10-25_firebase-rules.md`).
//!
//! # Module layout
//!
//! - [`json_lite`]: a lenient JSON reader that keeps line numbers (`firebase.json`, Realtime
//!   Database rules).
//! - [`rules_parse`]: the Firestore / Storage Security Rules reader. Hardened like
//!   `supabase/splitter.rs`, with one difference the false-negative law requires: an input it
//!   cannot account for degrades the file to NOT ASSESSED, never to clean.
//! - [`rules_model`]: condition normalisation, helper inlining and the exact-shape recognisers.
//! - [`rtdb`]: Realtime Database rules JSON, with `.read`/`.write` cascade.
//! - [`config`]: which files are assessed as the deployed rules (`firebase.json` resolution).
//!
//! The governing law is the resolver's: false-negative, never false-positive. A client reads
//! each finding as a fact they can check by opening the named file at the named line. Where a
//! rule cannot state such a fact deterministically the finding is review-graded (held, not
//! asserted) or left to the advisory tier.

pub mod config;
pub mod json_lite;
pub mod rtdb;
pub mod rules_model;
pub mod rules_parse;

#[cfg(test)]
mod tests;
