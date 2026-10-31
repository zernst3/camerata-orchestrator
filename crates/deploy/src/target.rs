//! The `DeployTarget` seam: every deploy backend implements this trait.
//!
//! The publish step calls [`DeployTarget::deploy`] ONLY after the user has
//! explicitly confirmed the transition from draft to live (see
//! [`crate::gate::can_publish`]). A target implementation never auto-publishes
//! on behalf of the system.

use crate::artifact::DeployArtifact;
use crate::outcome::DeployOutcome;

/// BYO-infra deploy seam. The publish step calls [`DeployTarget::deploy`]
/// exactly once per publish, and only after the user confirms the
/// draft-to-live transition. Implementations decide where and how the
/// artifact is deployed; the caller is responsible for the gate.
///
/// Built-in implementations:
/// - [`crate::local::LocalDeployTarget`] (dev / test stub, always succeeds)
/// - [`crate::azure::AzureWebAppTarget`] (Azure Web App shape; plan-only for now)
// Written in the form `#[async_trait]` expands to, so implementations keep using the
// macro unchanged. Declaring it with the macro adds a bare `#[must_use]` to each method,
// which clippy (Rust 1.99+) reports as `double_must_use`: the boxed future is already
// `#[must_use]`.
pub trait DeployTarget: Send + Sync {
    /// A short, human-readable label for this target (e.g. `"local"`,
    /// `"azure-web-app"`). Used in logs and diagnostic messages.
    fn name(&self) -> &str;

    /// Deploy `artifact` to this target and return a [`DeployOutcome`].
    ///
    /// Returning `Err` means the deploy machinery itself failed (e.g. the
    /// subprocess could not be spawned). Returning `Ok(DeployOutcome)` with a
    /// [`DeployStatus::Failed`][crate::outcome::DeployStatus::Failed] status
    /// means the deploy was attempted but the app did not go live.
    fn deploy<'life0, 'life1, 'async_trait>(
        &'life0 self,
        artifact: &'life1 DeployArtifact,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<DeployOutcome>> + Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait;
}
