//! The `DeployTarget` seam: every deploy backend implements this trait.
//!
//! The publish step calls [`DeployTarget::deploy`] ONLY after the user has
//! explicitly confirmed the transition from draft to live (see
//! [`crate::gate::can_publish`]). A target implementation never auto-publishes
//! on behalf of the system.

use async_trait::async_trait;

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
#[async_trait]
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
    async fn deploy(&self, artifact: &DeployArtifact) -> anyhow::Result<DeployOutcome>;
}
