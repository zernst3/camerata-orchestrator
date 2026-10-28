//! The ONE secret-shape detector and redactor.
//!
//! The governing law (docs/design/2026-10-23_secret-redaction-and-release-gate.md): a value that
//! matches a secret shape never appears in full in anything a client receives, and never reaches a
//! model when we can avoid it. This leaf crate owns the shapes ([`SecretShape`]), the safe twins
//! that stay readable ([`Twin`]), the redacted form (`prefix…tail (N chars)`), and the JSON-tree
//! walkers the release gate uses. It depends on no other camerata crate, so the gateway SEC arms,
//! the scan floor, the chat scrubber, the clone judge and the export release gate all share it.
//!
//! Source rule: no secret-shaped literal appears in this crate. Test values are built at runtime
//! (see [`fixtures`]); published documentation keys are stored as digests ([`published`]).

#![forbid(unsafe_code)]

mod detect;
mod firebase;
pub mod fixtures;
mod json;
pub mod published;
mod redact;
mod shapes;
mod twins;

pub use detect::{
    detect_all, detect_for_rule, detect_secrets, detect_shapes, first_secret, is_key_body_line,
    match_lines, match_lines_for_rule, SecretMatch,
};
pub use json::{find_secret_hits_in_json, find_secrets_in_json, redact_json_values, JsonHit};
pub use redact::{redact, redact_str, Redacted, Redaction};
pub use shapes::{is_supabase_dsn, shapes_for_rule, SecretShape, URL_SECRET_PARAMS};
pub use twins::{Twin, PLACEHOLDER_WORDS};

#[cfg(test)]
mod tests;
