//! Deleting and re-fetching a client's source code.
//!
//! After an inspection is delivered the client's code should not stay on the operator's machine.
//! This module owns the whole lifecycle (docs/design/2026-10-24_delete-and-refetch-client-
//! source.md): what is on disk now ([`status`]), the proof that it was removed ([`records`]),
//! the guard that decides whether a folder may be deleted at all, the purge of every copy
//! Camerata made, and (in later modules) re-fetching, the delta rescan and the git-object
//! reader the code viewer uses.
//!
//! The split follows the concerns, one file each, so no concern is implemented twice:
//!
//! - [`git`]: read-only git probes over a checkout.
//! - [`records`]: the deletion and re-fetch records and their store.
//! - [`status`]: the derived per-repo source status.
//! - [`handlers`]: the HTTP surface.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

pub mod delete;
pub mod git;
pub mod guard;
pub mod handlers;
pub mod ops;
pub mod purge;
pub mod read;
pub mod records;
pub mod refetch;
pub mod remote;
pub mod rescan;
pub mod resolution;
pub mod status;
pub mod storage;

#[cfg(test)]
mod delete_tests;
#[cfg(test)]
mod guard_tests;
#[cfg(test)]
mod read_tests;
#[cfg(test)]
mod readiness_tests;
#[cfg(test)]
mod refetch_tests;
#[cfg(test)]
mod rescan_tests;
#[cfg(test)]
mod status_tests;
#[cfg(test)]
pub(crate) mod test_support;

pub use records::{DeletionRecord, PurgeOutcome, PurgedItem, PurgedKind, RefetchRecord};
pub use status::RepoSourceStatus;

/// A refusal or failure the endpoints report as `{ "ok": false, "code": "...", "message": "..." }`.
/// `code` is a stable snake_case identifier the UI and tests match on; `message` is one plain
/// sentence for the operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl Refusal {
    /// A refusal because the request is well-formed but the repo is not in a state to allow it.
    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code,
            message: message.into(),
        }
    }

    /// The named thing does not exist.
    pub fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code,
            message: message.into(),
        }
    }

    /// The request itself is malformed.
    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
            message: message.into(),
        }
    }

    /// The JSON body every error response carries.
    pub fn body(&self) -> serde_json::Value {
        serde_json::json!({ "ok": false, "code": self.code, "message": self.message })
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        (self.status, Json(self.body())).into_response()
    }
}
