//! Local dev/test deploy stub.
//!
//! [`LocalDeployTarget`] never makes a network call. It records the deploy
//! steps into the [`DeployOutcome`] log and returns `Live` with a
//! `http://localhost:8080/<slug>` URL. This is the target used in tests and
//! in the prototype demo.

use async_trait::async_trait;

use crate::artifact::DeployArtifact;
use crate::outcome::{DeployOutcome, DeployStatus};
use crate::slug::to_slug;
use crate::target::DeployTarget;

/// Stub deploy target for local development and automated tests.
///
/// `deploy` always succeeds: it records the steps it would take into the
/// outcome log and returns [`DeployStatus::Live`] with a localhost URL
/// derived from the app name. No network access, no filesystem side-effects.
#[derive(Debug, Clone, Default)]
pub struct LocalDeployTarget;

impl LocalDeployTarget {
    /// Construct a new `LocalDeployTarget`.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl DeployTarget for LocalDeployTarget {
    fn name(&self) -> &str {
        "local"
    }

    /// Simulate a deploy by recording step descriptions and returning `Live`.
    ///
    /// The returned [`DeployOutcome::log`] documents what a real deploy would
    /// do so the caller can surface it in the UI. The URL is always
    /// `http://localhost:8080/<slug>` where `<slug>` is derived from
    /// `artifact.app_name`.
    async fn deploy(&self, artifact: &DeployArtifact) -> anyhow::Result<DeployOutcome> {
        let slug = to_slug(&artifact.app_name);
        let url = format!("http://localhost:8080/{slug}");

        let log = vec![
            format!("[local] starting deploy of '{}'", artifact.app_name),
            format!("[local] reading build output from '{}'", artifact.build_dir),
            format!("[local] binding to http://localhost:8080/{slug}"),
            format!("[local] serving '{}' at {url}", artifact.app_name),
        ];

        Ok(DeployOutcome {
            status: DeployStatus::Live,
            url: Some(url),
            log,
            message: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn deploy_returns_live_with_localhost_url() {
        let target = LocalDeployTarget::new();
        let artifact = DeployArtifact::new("Pottery Studio Admin", "/tmp/pottery/dist");
        let outcome = target.deploy(&artifact).await.unwrap();

        assert!(outcome.is_live(), "local target must always return Live");
        let url = outcome.url.as_deref().unwrap();
        assert!(
            url.starts_with("http://localhost:8080/"),
            "url must be a localhost url, got: {url}"
        );
        assert!(
            url.contains("pottery"),
            "url must contain the app name slug, got: {url}"
        );
    }

    #[tokio::test]
    async fn deploy_log_is_non_empty() {
        let target = LocalDeployTarget::new();
        let artifact = DeployArtifact::new("My App", "/tmp/my-app/dist");
        let outcome = target.deploy(&artifact).await.unwrap();
        assert!(
            !outcome.log.is_empty(),
            "log must contain at least one entry"
        );
    }

    #[tokio::test]
    async fn deploy_url_contains_slug_not_raw_name() {
        let target = LocalDeployTarget::new();
        // App name has spaces and caps.
        let artifact = DeployArtifact::new("Rent Tracker Pro", "/tmp/build");
        let outcome = target.deploy(&artifact).await.unwrap();
        let url = outcome.url.as_deref().unwrap();
        // The slug must be lowercase with hyphens.
        assert_eq!(url, "http://localhost:8080/rent-tracker-pro");
    }

    #[test]
    fn name_is_local() {
        assert_eq!(LocalDeployTarget::new().name(), "local");
    }
}
