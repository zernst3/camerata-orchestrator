//! Azure Web App deploy adapter.
//!
//! This module provides the SHAPE of a real Azure deploy: the configuration
//! types, the pure command-plan builder, and the `DeployTarget` implementation.
//! The plan-building functions are fully testable without any network access.
//!
//! # What is wired vs. what is not
//!
//! `deploy_plan` returns the ordered `az`-CLI command strings that a live
//! deploy would run. That plan is pure Rust: no subprocess, no HTTP call, no
//! credential lookup. The [`DeployTarget`] implementation puts that plan into
//! the outcome log and returns a `Pending` (not `Live`) status, with an
//! explicit message explaining that live `az`-CLI execution is the remaining
//! step. Wiring it requires the user's Azure credentials and is left as the
//! next implementation task.
//!
//! The pure plan-building functions (`deploy_plan`, `webapp_name`,
//! `deploy_url`) are the right place to review correctness and add tests.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::artifact::DeployArtifact;
use crate::outcome::{DeployOutcome, DeployStatus};
use crate::slug::to_slug;
use crate::target::DeployTarget;

/// Azure subscription and resource group coordinates for a single Web App.
///
/// All four fields are required. The `region` value must match an Azure
/// location string (e.g. `"eastus"`, `"westeurope"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AzureConfig {
    /// Azure subscription id (a UUID).
    pub subscription_id: String,
    /// The resource group that will contain (or already contains) the Web App.
    pub resource_group: String,
    /// The human-readable app name; used to derive the Azure Web App name.
    pub app_name: String,
    /// Azure region string, e.g. `"eastus"`.
    pub region: String,
}

impl AzureConfig {
    /// Construct an `AzureConfig`.
    pub fn new(
        subscription_id: impl Into<String>,
        resource_group: impl Into<String>,
        app_name: impl Into<String>,
        region: impl Into<String>,
    ) -> Self {
        Self {
            subscription_id: subscription_id.into(),
            resource_group: resource_group.into(),
            app_name: app_name.into(),
            region: region.into(),
        }
    }
}

/// Azure Web App deploy target.
///
/// Holds an [`AzureConfig`] and provides pure plan-building methods that are
/// testable without credentials. The [`DeployTarget`] implementation returns a
/// `Pending` outcome with the plan in the log: live `az`-CLI execution is the
/// remaining step that requires real Azure credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AzureWebAppTarget {
    /// Azure configuration for this target.
    pub config: AzureConfig,
}

impl AzureWebAppTarget {
    /// Construct a target from the given config.
    pub fn new(config: AzureConfig) -> Self {
        Self { config }
    }

    /// The Azure Web App name derived from `config.app_name`.
    ///
    /// Azure Web App names must be globally unique and URL-safe. The name is
    /// a slug of `config.app_name` so it is safe to use in a subdomain.
    pub fn webapp_name(&self) -> String {
        to_slug(&self.config.app_name)
    }

    /// The public URL the deployed app will be reachable at once live.
    ///
    /// Format: `https://<webapp-name>.azurewebsites.net`.
    pub fn deploy_url(&self) -> String {
        format!("https://{}.azurewebsites.net", self.webapp_name())
    }

    /// Build the ordered list of `az`-CLI command strings a live deploy would
    /// run. Returns plain strings; callers that drive a subprocess would pass
    /// each string to the shell. The list is the deploy plan and is included
    /// verbatim in the [`DeployOutcome`] log.
    ///
    /// Steps:
    /// 1. Create (or confirm) the resource group.
    /// 2. Deploy the app using `az webapp up`.
    /// 3. Confirm the site is reachable.
    pub fn deploy_plan(&self, artifact: &DeployArtifact) -> Vec<String> {
        let name = self.webapp_name();
        let rg = &self.config.resource_group;
        let region = &self.config.region;
        let url = self.deploy_url();

        vec![
            format!(
                "az group create --name {rg} --location {region} \
                 --subscription {}",
                self.config.subscription_id
            ),
            format!(
                "az webapp up --name {name} --resource-group {rg} \
                 --location {region} --src-path {} --subscription {}",
                artifact.build_dir, self.config.subscription_id
            ),
            format!("az webapp show --name {name} --resource-group {rg} --query defaultHostName"),
            format!("# Expected live URL: {url}"),
        ]
    }
}

#[async_trait]
impl DeployTarget for AzureWebAppTarget {
    fn name(&self) -> &str {
        "azure-web-app"
    }

    /// Build the deploy plan and return it in the outcome log.
    ///
    /// Status is always [`DeployStatus::Pending`] because live `az`-CLI
    /// execution is not wired yet: this implementation builds the correct plan
    /// (the commands are real and correct) but does not run them. Wiring the
    /// live execution path requires the user's Azure credentials and is the
    /// remaining step documented in the log.
    ///
    /// The outcome is never [`DeployStatus::Live`] from this implementation.
    async fn deploy(&self, artifact: &DeployArtifact) -> anyhow::Result<DeployOutcome> {
        let plan = self.deploy_plan(artifact);
        let message = format!(
            "Deploy plan built for '{}' targeting Azure Web App '{}' in resource \
             group '{}'. Live az-CLI execution is the remaining step and requires \
             the user's Azure credentials. Run the commands in the log to complete \
             the deploy.",
            artifact.app_name,
            self.webapp_name(),
            self.config.resource_group,
        );

        Ok(DeployOutcome {
            status: DeployStatus::Pending,
            url: None,
            log: plan,
            message: Some(message),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config() -> AzureConfig {
        AzureConfig::new(
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "my-resource-group",
            "Pottery Studio Admin",
            "eastus",
        )
    }

    fn sample_artifact() -> DeployArtifact {
        DeployArtifact::new("Pottery Studio Admin", "/tmp/pottery/dist")
    }

    // --- pure plan-building helpers ---

    #[test]
    fn webapp_name_is_a_slug_of_app_name() {
        let target = AzureWebAppTarget::new(sample_config());
        let name = target.webapp_name();
        // Must be lowercase, alphanumeric + hyphens only.
        assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
        assert_eq!(name, name.to_lowercase());
        assert!(
            name.contains("pottery"),
            "slug must be derived from the app name"
        );
    }

    #[test]
    fn deploy_url_contains_webapp_name_and_azurewebsites() {
        let target = AzureWebAppTarget::new(sample_config());
        let url = target.deploy_url();
        assert!(
            url.ends_with(".azurewebsites.net"),
            "deploy url must end with .azurewebsites.net, got: {url}"
        );
        assert!(
            url.contains(&target.webapp_name()),
            "deploy url must contain the webapp name"
        );
        assert!(url.starts_with("https://"), "deploy url must use https");
    }

    #[test]
    fn deploy_plan_contains_az_webapp_command() {
        let target = AzureWebAppTarget::new(sample_config());
        let plan = target.deploy_plan(&sample_artifact());

        let has_webapp_up = plan.iter().any(|line| line.contains("az webapp up"));
        assert!(has_webapp_up, "plan must include an 'az webapp up' command");

        let has_app_name = plan.iter().any(|line| line.contains(&target.webapp_name()));
        assert!(has_app_name, "plan must reference the derived webapp name");

        let has_rg = plan.iter().any(|line| line.contains("my-resource-group"));
        assert!(has_rg, "plan must reference the resource group");
    }

    #[test]
    fn deploy_plan_starts_with_resource_group_create() {
        let target = AzureWebAppTarget::new(sample_config());
        let plan = target.deploy_plan(&sample_artifact());
        assert!(!plan.is_empty(), "plan must not be empty");
        assert!(
            plan[0].contains("az group create"),
            "first step must create the resource group"
        );
    }

    // --- DeployTarget implementation ---

    #[tokio::test]
    async fn deploy_returns_the_plan_in_log() {
        let target = AzureWebAppTarget::new(sample_config());
        let artifact = sample_artifact();
        let outcome = target.deploy(&artifact).await.unwrap();

        let expected_plan = target.deploy_plan(&artifact);
        assert_eq!(
            outcome.log, expected_plan,
            "outcome log must equal the deploy plan"
        );
    }

    #[tokio::test]
    async fn deploy_does_not_return_live() {
        let target = AzureWebAppTarget::new(sample_config());
        let outcome = target.deploy(&sample_artifact()).await.unwrap();

        assert!(
            !outcome.is_live(),
            "azure target must not claim Live without credentials"
        );
        assert!(
            outcome.url.is_none(),
            "url must be None until live execution is wired"
        );
    }

    #[tokio::test]
    async fn deploy_message_explains_missing_execution() {
        let target = AzureWebAppTarget::new(sample_config());
        let outcome = target.deploy(&sample_artifact()).await.unwrap();

        let msg = outcome.message.as_deref().unwrap_or("");
        assert!(
            msg.contains("az-CLI") || msg.contains("credentials") || msg.contains("remaining"),
            "message must explain that live execution is the remaining step, got: {msg}"
        );
    }

    #[test]
    fn name_is_azure_web_app() {
        assert_eq!(
            AzureWebAppTarget::new(sample_config()).name(),
            "azure-web-app"
        );
    }

    // --- serde round-trips ---

    #[test]
    fn azure_config_round_trip_json() {
        let config = sample_config();
        let json = serde_json::to_string(&config).unwrap();
        let back: AzureConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, config);
    }
}
