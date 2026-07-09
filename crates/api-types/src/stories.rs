//! `CanonicalStory` — the CLIENT-side deserialization mirror of the story spine's
//! canonical shape, added for Phase D of the DTO extraction (`camerata-client`, the
//! typed HTTP client over the BFF's `/api/*` routes).
//!
//! The real, behavior-carrying definition lives in `camerata_worktracker::{CanonicalStory,
//! RepoTarget, ExternalRef, FeatureStatus, Provider}` (`crates/worktracker/src/lib.rs`),
//! which that crate's provider adapters construct and the server's `GET /api/stories`
//! handler (`crates/server/src/lib.rs::stories`) serializes verbatim (it returns
//! `Json<Vec<CanonicalStory>>` from `camerata_worktracker` directly — there is no
//! separate server-side wire shape to mirror here, unlike `workitems::WorkItem`).
//!
//! This module re-declares the SAME field-for-field shape as a pure-serde mirror so
//! `camerata-api-types` (which must have zero `camerata-*` dependencies — see the crate
//! doc) can type the client response without pulling in `camerata-worktracker` (a much
//! heavier crate: provider adapters, HTTP transports, Jira/ADO/GitHub clients). Every
//! field carries `#[serde(default)]` for resilience, matching the `workitems::WorkItem`
//! client-mirror convention.

use serde::{Deserialize, Serialize};

/// Mirrors `camerata_worktracker::FeatureStatus`. Wire strings are `snake_case`
/// (`"intake"`, `"awaiting_qa"`, `"signed_off"`, etc.) — see the source enum for the
/// full doc on each variant's meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureStatus {
    Intake,
    Investigating,
    AwaitingClarification,
    Planned,
    Executing,
    Gating,
    AwaitingQa,
    SignedOff,
    Done,
    Blocked,
    Rejected,
}

/// Mirrors `camerata_worktracker::Provider`. Wire strings are the canonical tokens from
/// the worktracker design doc: `"native"`, `"jira"`, `"azure-devops"`, `"github"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Native,
    Jira,
    #[serde(rename = "azure-devops")]
    AzureDevOps,
    #[serde(rename = "github")]
    GitHub,
}

/// Mirrors `camerata_worktracker::ExternalRef`: a handle to a work item on an external
/// tracker, carried alongside a [`CanonicalStory`] when it is linked to an external board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalRef {
    pub provider: Provider,
    pub external_id: String,
    #[serde(default)]
    pub container: Option<String>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub revision: Option<String>,
}

/// Mirrors `camerata_worktracker::RepoTarget`: one build target for a story.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoTarget {
    pub repo: String,
    #[serde(default)]
    pub role: Option<String>,
}

/// Mirrors `camerata_worktracker::CanonicalStory`, the shape `GET /api/stories`
/// (`crates/server/src/lib.rs::stories`) returns as `Json<Vec<CanonicalStory>>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalStory {
    pub id: String,
    #[serde(default)]
    pub external_ref: Option<ExternalRef>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub status: FeatureStatus,
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub targets: Vec<RepoTarget>,
}
