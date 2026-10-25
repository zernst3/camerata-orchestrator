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
//! - [`grants`]: the shared per-`allow` model, path helpers and message plumbing.
//! - [`grant_rules`]: rules 1, 2, 3 and 6 (open, test mode, auth only, recursive wildcard).
//! - [`rules_checker`]: [`rules_checker::FirebaseRulesChecker`], which runs the rules-file rules
//!   and owns the degradation contract.
//!
//! The governing law is the resolver's: false-negative, never false-positive. A client reads
//! each finding as a fact they can check by opening the named file at the named line. Where a
//! rule cannot state such a fact deterministically the finding is review-graded (held, not
//! asserted) or left to the advisory tier.

pub mod config;
pub mod grant_rules;
pub mod grants;
pub mod json_lite;
pub mod rtdb;
pub mod rules_checker;
pub mod rules_model;
pub mod rules_parse;

pub use rules_checker::FirebaseRulesChecker;

/// Rule 1: an unconditional grant.
pub const RULE_OPEN: &str = "FIREBASE-RULES-OPEN-1";
/// Rule 2: the console's test-mode scaffold.
pub const RULE_TEST_MODE: &str = "FIREBASE-RULES-TEST-MODE-1";
/// Rule 3: a grant on the sole condition that the caller is signed in.
pub const RULE_AUTH_ONLY: &str = "FIREBASE-RULES-AUTH-ONLY-1";
/// Rule 4: update ownership checked on the incoming document only.
pub const RULE_OWNERSHIP: &str = "FIREBASE-RULES-OWNERSHIP-1";
/// Rule 5: a write grant that never reads the incoming document.
pub const RULE_FIELD_VALIDATION: &str = "FIREBASE-RULES-FIELD-VALIDATION-1";
/// Rule 6: a broad wildcard grant that overrides narrower rules.
pub const RULE_RECURSIVE_WILDCARD: &str = "FIREBASE-RULES-RECURSIVE-WILDCARD-1";
/// Rule 7: Cloud Storage rules that are open to the public or to any signed-in user.
pub const RULE_STORAGE_OPEN: &str = "FIREBASE-STORAGE-RULES-OPEN-1";
/// Rule 8: Realtime Database rules that are open.
pub const RULE_RTDB_OPEN: &str = "FIREBASE-RTDB-RULES-OPEN-1";
/// Rule 9: a role the holder can write themselves.
pub const RULE_ADMIN_CLAIMS: &str = "FIREBASE-ADMIN-CLAIMS-1";

#[cfg(test)]
mod tests;
