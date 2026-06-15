//! The active work-tracker provider, selected from the environment.
//!
//! Default is the in-process `NativeProvider` (no credentials, the demo path). When
//! `CAMERATA_GITHUB_TOKEN` and `CAMERATA_GITHUB_REPO` (`owner/repo`) are set, the real
//! `GithubProvider` over `ReqwestTransport` is wired instead, so the same BFF talks to
//! a real GitHub repo. Everything downstream of the `WorkItemProvider` trait is
//! identical either way; only this selection changes. The hard blocker is supplying a
//! token: with one set, the provider makes real GitHub API calls.

use std::sync::Arc;

use camerata_worktracker::{
    GithubConfig, GithubProvider, NativeProvider, ReqwestTransport, WorkItemProvider,
};

/// The selected provider plus metadata for the `/api/provider` endpoint.
#[derive(Clone)]
pub struct ProviderHandle {
    pub provider: Arc<dyn WorkItemProvider>,
    /// Human-readable label, e.g. "native (in-process)" or "github (owner/repo)".
    pub label: String,
    /// True when a real external tracker is wired (vs the in-process native one).
    pub live: bool,
}

impl ProviderHandle {
    /// The in-process native provider (no credentials).
    pub fn native() -> Self {
        Self {
            provider: Arc::new(NativeProvider::new()),
            label: "native (in-process)".to_string(),
            live: false,
        }
    }

    /// Select the provider from the environment: GitHub when credentials are present,
    /// otherwise native.
    pub fn from_env() -> Self {
        github_from_env().unwrap_or_else(Self::native)
    }
}

/// Build a GitHub provider handle from env vars, or `None` if they are not all set.
///
/// `CAMERATA_GITHUB_TOKEN` = a PAT / installation token with Issues read+write.
/// `CAMERATA_GITHUB_REPO`  = `owner/repo` (or set `CAMERATA_GITHUB_OWNER` + `_REPO`).
fn github_from_env() -> Option<ProviderHandle> {
    let token = non_empty("CAMERATA_GITHUB_TOKEN")?;
    let repo_spec = non_empty("CAMERATA_GITHUB_REPO")?;
    let (owner, repo) = match repo_spec.split_once('/') {
        Some((o, r)) => (o.to_string(), r.to_string()),
        None => (non_empty("CAMERATA_GITHUB_OWNER")?, repo_spec),
    };

    let config = GithubConfig {
        owner: owner.clone(),
        repo: repo.clone(),
        token,
    };
    let transport = match ReqwestTransport::new(config.auth_header()) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[camerata-server] failed to build GitHub transport: {e}; using native");
            return None;
        }
    };
    Some(ProviderHandle {
        provider: Arc::new(GithubProvider::new(config, transport)),
        label: format!("github ({owner}/{repo})"),
        live: true,
    })
}

/// Read an env var, returning `None` if unset or empty.
fn non_empty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_handle_is_not_live() {
        let h = ProviderHandle::native();
        assert!(!h.live);
        assert!(h.label.contains("native"));
    }
}
