//! Connection health probe: reports whether the optional integrations are
//! configured and, for the ones we can cheaply check, whether they actually work.
//!
//! Integrations are OPTIONAL. Camerata runs on the built-in (native) tracker with
//! no credentials. So:
//! - nothing configured  -> the UI shows a WARNING ("no integration detected"),
//!   not an error: running unconnected is a legitimate choice.
//! - configured but failing (a 401/403/5xx from GitHub) -> the UI shows an ERROR,
//!   because the user asked for a connection that isn't working.
//!
//! Two integration families today, both served by the one GitHub token (code host
//! = repos; work tracker = issues/projects), plus the Claude agent (CLI vs API).

use serde::Serialize;

use camerata_worktracker::{HttpTransport, ReqwestTransport};

/// One integration's status as the UI renders it.
#[derive(Debug, Serialize)]
pub struct Connection {
    /// Stable id (`github`, `claude`).
    pub id: &'static str,
    /// Human label.
    pub label: String,
    /// What this integration provides (`code+story`, `agent`).
    pub role: &'static str,
    /// True when the user has supplied credentials for it.
    pub configured: bool,
    /// When we actively checked reachability: `Some(true)` ok, `Some(false)`
    /// failing, `None` not checked (configured-only, no live probe).
    pub ok: Option<bool>,
    /// The failure detail when `ok == Some(false)` (e.g. `HTTP 403`).
    pub error: Option<String>,
    /// For the agent: how Claude is reached (`cli`, `api-key`), if configured.
    pub mode: Option<String>,
}

/// The full connections report.
#[derive(Debug, Serialize)]
pub struct ConnectionsReport {
    /// Each integration's status.
    pub connections: Vec<Connection>,
    /// True when at least one integration is configured.
    pub any_configured: bool,
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// Probe all integrations. Does a single cheap GitHub reachability call when a
/// token is present (GET `/rate_limit`), so a bad/forbidden token surfaces as a
/// real error rather than a silent "configured".
pub async fn probe() -> ConnectionsReport {
    let mut connections = Vec::new();

    // ── GitHub: code host (repos) + work tracker (issues/projects), one token. ──
    let (configured, ok, error) = match env_nonempty("CAMERATA_GITHUB_TOKEN") {
        None => (false, None, None),
        Some(token) => match ReqwestTransport::new(format!("Bearer {token}")) {
            Ok(transport) => match transport.get("https://api.github.com/rate_limit").await {
                Ok(resp) if (200..300).contains(&resp.status) => (true, Some(true), None),
                Ok(resp) => (
                    true,
                    Some(false),
                    Some(format!("HTTP {} from GitHub", resp.status)),
                ),
                Err(e) => (true, Some(false), Some(e.to_string())),
            },
            Err(e) => (true, Some(false), Some(e.to_string())),
        },
    };
    connections.push(Connection {
        id: "github",
        label: "GitHub — repos + issues/projects".to_string(),
        role: "code+story",
        configured,
        ok,
        error,
        mode: None,
    });

    // ── Claude agent: CLI on PATH, else an API key (informational). ──
    let claude_cli = claude_on_path();
    let api_key = env_nonempty("ANTHROPIC_API_KEY").is_some();
    let (a_configured, a_mode) = if claude_cli {
        (true, Some("cli".to_string()))
    } else if api_key {
        // The current agent driver shells out to the `claude` CLI; a bare API key
        // with no CLI is reported but is not yet a usable driver.
        (true, Some("api-key (no claude CLI found)".to_string()))
    } else {
        (false, None)
    };
    connections.push(Connection {
        id: "claude",
        label: "Claude — governed agents".to_string(),
        role: "agent",
        configured: a_configured,
        ok: None,
        error: None,
        mode: a_mode,
    });

    let any_configured = connections.iter().any(|c| c.configured);
    ConnectionsReport {
        connections,
        any_configured,
    }
}

/// Whether a `claude` executable is resolvable on PATH (no subprocess spawned).
fn claude_on_path() -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join("claude");
        candidate.is_file()
    })
}
