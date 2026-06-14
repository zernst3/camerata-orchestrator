//! `camerata-deploy`: the BYO-infra publish step.
//!
//! Provides the [`target::DeployTarget`] seam that the publish step calls once
//! the user confirms the draft-to-live transition, a local dev/test stub
//! ([`local::LocalDeployTarget`]), and the Azure Web App adapter shape
//! ([`azure::AzureWebAppTarget`]).
//!
//! The draft-to-live gate ([`gate`]) enforces that an app must have been built
//! at least once AND that the user has explicitly confirmed before any deploy
//! target is invoked. Publishing is never automatic.
//!
//! # Module layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`artifact`] | [`artifact::DeployArtifact`]: the built output handed to a target |
//! | [`outcome`] | [`outcome::DeployStatus`], [`outcome::DeployOutcome`]: result types |
//! | [`target`] | [`target::DeployTarget`]: the async trait every backend implements |
//! | [`gate`] | Draft-to-publish gate: [`gate::can_publish`] guard, [`gate::PublishGate`] |
//! | [`local`] | [`local::LocalDeployTarget`]: stub for dev and tests, always succeeds |
//! | [`azure`] | [`azure::AzureWebAppTarget`]: Azure Web App plan (live execution TODO) |
//! | [`slug`] | [`slug::to_slug`]: URL-safe slug helper used by targets |

pub mod artifact;
pub mod azure;
pub mod gate;
pub mod local;
pub mod outcome;
pub mod slug;
pub mod target;

// Re-export the most-used types so callers can write `camerata_deploy::DeployArtifact`
// without drilling into sub-modules.
pub use artifact::DeployArtifact;
pub use azure::{AzureConfig, AzureWebAppTarget};
pub use gate::{can_publish, PublishError, PublishGate, Published};
pub use local::LocalDeployTarget;
pub use outcome::{DeployOutcome, DeployStatus};
pub use target::DeployTarget;
