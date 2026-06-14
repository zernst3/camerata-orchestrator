//! Bridge from the Publish/Live screen to the real deploy seam (`camerata-deploy`).
//!
//! Publishing leaves DRAFT and deploys to the user's own cloud (BYO-infra). The
//! default target here is the `LocalDeployTarget` stub, which always succeeds and
//! returns a localhost URL, so the recordable demo works with no cloud account. The
//! Azure Web App target (`camerata_deploy::AzureWebAppTarget`) is the real BYO-infra
//! path; wiring its live execution needs the user's Azure credentials, so it stays
//! behind that seam and is not the default here.

use camerata_deploy::{DeployArtifact, DeployOutcome, DeployTarget, LocalDeployTarget};

/// Publish `app_name` through the deploy seam and return the outcome (status + the
/// live URL). Uses the local target so it always works for the demo; a real
/// deployment swaps in the Azure target behind the same `DeployTarget` trait.
pub async fn publish_app(app_name: &str) -> DeployOutcome {
    let target = LocalDeployTarget::new();
    let artifact = DeployArtifact::new(app_name, String::new());
    match target.deploy(&artifact).await {
        Ok(outcome) => outcome,
        Err(e) => DeployOutcome::failed(format!("publish did not complete: {e}")),
    }
}
