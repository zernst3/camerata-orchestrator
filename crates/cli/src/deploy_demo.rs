//! `deploy-demo` -- BYO-infra publish step, end to end.
//!
//! Demonstrates the full publish lifecycle: the draft-to-live gate, the local
//! dev target (always succeeds in the demo path), and the Azure Web App adapter
//! (plan-only: real execution requires the user's Azure credentials, so the
//! ordered az-CLI commands are shown but not run).
//!
//! Sections:
//!
//!   1. DRAFT GATE   -- show blocked and allowed states
//!   2. LOCAL DEPLOY -- build artifact, deploy via LocalDeployTarget
//!   3. AZURE PLAN   -- AzureWebAppTarget plan (shown, not run)
//!   4. SUMMARY      -- DEPLOY-DEMO: PASS

use camerata_deploy::{
    can_publish, AzureConfig, AzureWebAppTarget, DeployArtifact, DeployTarget, LocalDeployTarget,
    PublishError,
};

// ── main demo entry-point ─────────────────────────────────────────────────────

/// Run the full BYO-infra publish step demonstration in-process.
pub async fn run_deploy_demo() -> anyhow::Result<()> {
    println!("== Camerata DEPLOY-DEMO: BYO-infra publish step ==");
    println!();

    // ── 1. DRAFT GATE ─────────────────────────────────────────────────────────
    println!("── 1. DRAFT-TO-LIVE GATE ──");
    println!("  Publishing needs a successful build AND explicit user confirmation.");
    println!("  Neither condition is automatic; the user owns the draft-to-live decision.");
    println!();

    // No build yet, even with confirmation -- blocked.
    let blocked_no_build = can_publish(false, true);
    println!(
        "  can_publish(executed=false, confirmed=true)  -> {:?}",
        blocked_no_build
    );
    assert_eq!(
        blocked_no_build,
        Err(PublishError::NotYetBuilt),
        "must block when app has not been built"
    );

    // Built, but user has not confirmed -- blocked.
    let blocked_no_confirm = can_publish(true, false);
    println!(
        "  can_publish(executed=true,  confirmed=false) -> {:?}",
        blocked_no_confirm
    );
    assert_eq!(
        blocked_no_confirm,
        Err(PublishError::NotConfirmed),
        "must block when user has not confirmed"
    );

    // Both conditions met -- allowed.
    let allowed = can_publish(true, true);
    println!(
        "  can_publish(executed=true,  confirmed=true)  -> {:?}",
        allowed
    );
    assert!(allowed.is_ok(), "must allow when both conditions are met");

    println!("  GATE CONTRACT: build-executed check takes priority over confirmation check.");
    println!();

    // ── 2. LOCAL DEPLOY ───────────────────────────────────────────────────────
    println!("── 2. LOCAL DEPLOY (dev / test path) ──");

    let artifact = DeployArtifact::new("Pottery Studio Admin", "/tmp/pottery-studio/dist");
    let target = LocalDeployTarget::new();

    println!("  artifact: {} ({})", artifact.app_name, artifact.build_dir);
    println!("  target:   {}", target.name());

    let outcome = target.deploy(&artifact).await?;

    println!("  deploy log:");
    for line in &outcome.log {
        println!("    {line}");
    }

    assert!(
        outcome.is_live(),
        "local target must always return Live status"
    );
    let live_url = outcome
        .url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("expected a URL from the local deploy target"))?;

    println!("  status: {:?}", outcome.status);
    println!("  live URL: {live_url}");
    println!(
        "  NOTE: the local target is the demo path -- no network, no filesystem side-effects."
    );
    println!();

    // ── 3. AZURE (BYO-INFRA) PLAN ─────────────────────────────────────────────
    println!("── 3. AZURE WEB APP PLAN (BYO-infra, plan shown -- not run) ──");

    let config = AzureConfig::new(
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
        "pottery-studio-rg",
        "Pottery Studio Admin",
        "eastus",
    );
    let azure_target = AzureWebAppTarget::new(config);

    println!("  webapp name: {}", azure_target.webapp_name());
    println!("  deploy URL:  {}", azure_target.deploy_url());
    println!();

    let plan = azure_target.deploy_plan(&artifact);
    println!("  deploy plan ({} steps):", plan.len());
    for (i, step) in plan.iter().enumerate() {
        println!("    {}. {step}", i + 1);
    }
    println!();
    println!("  HONEST NOTE: live execution of the plan above requires the user's Azure");
    println!("  credentials (az login / service principal). The plan is shown, not run.");
    println!("  The commands are correct and ready to copy-paste into a terminal or CI job.");
    println!();

    // ── 4. SUMMARY ────────────────────────────────────────────────────────────
    println!("── SUMMARY ──");
    println!(
        "  Draft gate: blocked without build, blocked without confirmation, allowed with both."
    );
    println!("  Local deploy: '{}' live at {live_url}", artifact.app_name);
    println!(
        "  Azure plan: {} steps generated for '{}'.",
        plan.len(),
        azure_target.webapp_name()
    );
    println!("  BYO-infra: swap the target to deploy anywhere (Azure, Fly.io, self-hosted).");
    println!();
    println!("DEPLOY-DEMO: PASS");

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_deploy::{
        can_publish, AzureConfig, AzureWebAppTarget, DeployArtifact, PublishError,
    };

    // ── draft gate logic ──────────────────────────────────────────────────────

    #[test]
    fn gate_blocks_when_not_built() {
        assert_eq!(can_publish(false, true), Err(PublishError::NotYetBuilt));
    }

    #[test]
    fn gate_blocks_when_not_confirmed() {
        assert_eq!(can_publish(true, false), Err(PublishError::NotConfirmed));
    }

    #[test]
    fn gate_allows_when_both_conditions_met() {
        assert!(can_publish(true, true).is_ok());
    }

    #[test]
    fn gate_build_check_takes_priority_over_confirm() {
        // Neither condition met: build check fires first.
        assert_eq!(can_publish(false, false), Err(PublishError::NotYetBuilt));
    }

    // ── azure deploy_plan contains az webapp command ──────────────────────────

    #[test]
    fn azure_deploy_plan_contains_az_webapp_up_command() {
        let config = AzureConfig::new(
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "pottery-studio-rg",
            "Pottery Studio Admin",
            "eastus",
        );
        let target = AzureWebAppTarget::new(config);
        let artifact = DeployArtifact::new("Pottery Studio Admin", "/tmp/pottery/dist");

        let plan = target.deploy_plan(&artifact);

        assert!(
            plan.iter().any(|s| s.contains("az webapp")),
            "plan must contain an 'az webapp' command"
        );
    }

    #[test]
    fn azure_deploy_plan_references_resource_group() {
        let config = AzureConfig::new(
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "pottery-studio-rg",
            "Pottery Studio Admin",
            "eastus",
        );
        let target = AzureWebAppTarget::new(config);
        let artifact = DeployArtifact::new("Pottery Studio Admin", "/tmp/pottery/dist");

        let plan = target.deploy_plan(&artifact);

        assert!(
            plan.iter().any(|s| s.contains("pottery-studio-rg")),
            "plan must reference the resource group"
        );
    }

    // ── local deploy returns live ─────────────────────────────────────────────

    #[tokio::test]
    async fn local_deploy_returns_live_with_url() {
        use camerata_deploy::{DeployTarget, LocalDeployTarget};

        let target = LocalDeployTarget::new();
        let artifact = DeployArtifact::new("My Demo App", "/tmp/build");
        let outcome = target.deploy(&artifact).await.unwrap();

        assert!(outcome.is_live(), "local target must return Live status");
        assert!(outcome.url.is_some(), "local target must return a URL");
    }

    // ── full demo must complete without error ─────────────────────────────────

    #[tokio::test]
    async fn deploy_demo_runs_without_error() {
        run_deploy_demo().await.expect("deploy-demo must not error");
    }
}
