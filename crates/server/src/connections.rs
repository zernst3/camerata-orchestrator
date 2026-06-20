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
//!
//! ## GitHub probe
//!
//! When a token is present, we call `GET https://api.github.com/user`.  That
//! endpoint is cheap (one round-trip), returns the authenticated user's `login`
//! in the JSON body, and (for PATs / OAuth tokens) echoes the granted scopes in
//! the `X-OAuth-Scopes` response header.  We classify the outcome into one of
//! four error categories:
//!
//! - `NoToken`           — `CAMERATA_GITHUB_TOKEN` is absent or empty.
//! - `InvalidOrExpired`  — GitHub returned 401: the token is wrong or has expired.
//! - `InsufficientScope` — GitHub returned 403: the token exists but lacks the
//!                         permissions Camerata needs (read:user + repo).
//! - `Unreachable`       — Any other non-2xx or a transport error.
//!
//! We NEVER log or return the token value; only the derived `login` + `scopes` are
//! surfaced.

use serde::Serialize;

// ── Error category ─────────────────────────────────────────────────────────────

/// Categorised failure modes for a GitHub token probe.  Knowing which category
/// applies lets the UI/operator show an actionable message rather than a generic
/// "connection failed".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubTokenError {
    /// No token is configured (`CAMERATA_GITHUB_TOKEN` absent or empty).
    NoToken,
    /// The token exists but GitHub rejected it with HTTP 401 (wrong or expired).
    InvalidOrExpired,
    /// The token authenticated (not 401) but GitHub returned 403 — the token
    /// lacks required scope (typically `repo` or `read:user`).
    InsufficientScope,
    /// Any other non-2xx HTTP status or transport-level failure (DNS, TLS, etc.).
    Unreachable,
}

impl std::fmt::Display for GitHubTokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitHubTokenError::NoToken => {
                write!(f, "no token — set CAMERATA_GITHUB_TOKEN to connect")
            }
            GitHubTokenError::InvalidOrExpired => {
                write!(f, "token is invalid or has expired (HTTP 401 from GitHub)")
            }
            GitHubTokenError::InsufficientScope => write!(
                f,
                "token lacks required scope (HTTP 403 from GitHub); \
                 ensure the PAT has repo + read:user scopes"
            ),
            GitHubTokenError::Unreachable => write!(f, "GitHub is unreachable"),
        }
    }
}

// ── Probe outcome ──────────────────────────────────────────────────────────────

/// The structured outcome of a single GitHub token probe.
///
/// This is the internal result type; it is shaped into the public
/// [`Connection`] by [`probe()`].
#[derive(Debug, Clone)]
pub(crate) struct GitHubProbeOutcome {
    /// The authenticated GitHub login, when the probe succeeded.
    pub login: Option<String>,
    /// OAuth scopes granted to the token, parsed from the `X-OAuth-Scopes`
    /// response header, when the probe succeeded.  Empty for fine-grained PATs
    /// (which GitHub does not report scopes for in the same header).
    pub scopes: Vec<String>,
    /// Error category when the probe did not succeed.  `None` means success.
    pub error: Option<GitHubTokenError>,
}

// ── Public surface ─────────────────────────────────────────────────────────────

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
    /// The failure detail when `ok == Some(false)` (e.g. `"token is invalid or
    /// has expired (HTTP 401 from GitHub)"`).
    pub error: Option<String>,
    /// Structured error category for the GitHub integration, when failing.
    /// Absent for non-GitHub connections or when there is no error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_category: Option<GitHubTokenError>,
    /// For the agent: how Claude is reached (`cli`, `api-key`), if configured.
    pub mode: Option<String>,
    /// Authenticated GitHub login, present when the GitHub probe succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    /// OAuth scopes granted to the GitHub token, when the probe succeeded and
    /// the token is a classic PAT (fine-grained PATs omit the scope header).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
}

/// The full connections report.
#[derive(Debug, Serialize)]
pub struct ConnectionsReport {
    /// Each integration's status.
    pub connections: Vec<Connection>,
    /// True when at least one integration is configured.
    pub any_configured: bool,
}

// ── GitHub probe ───────────────────────────────────────────────────────────────

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// Call `GET https://api.github.com/user` with the given token, return a
/// structured [`GitHubProbeOutcome`].
///
/// Uses `reqwest` directly (rather than the shared `ReqwestTransport`) so we
/// can read the `X-OAuth-Scopes` response header, which the transport's
/// `HttpResponse` does not expose.
///
/// NEVER logs or returns the token value.
pub(crate) async fn probe_github_token(token: &str) -> GitHubProbeOutcome {
    use camerata_worktracker::http::DEFAULT_USER_AGENT;

    let client = match reqwest::Client::builder()
        .use_rustls_tls()
        .user_agent(DEFAULT_USER_AGENT)
        .build()
    {
        Ok(c) => c,
        Err(_) => {
            return GitHubProbeOutcome {
                login: None,
                scopes: vec![],
                error: Some(GitHubTokenError::Unreachable),
            };
        }
    };

    let result = client
        .get("https://api.github.com/user")
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await;

    match result {
        Err(_) => GitHubProbeOutcome {
            login: None,
            scopes: vec![],
            error: Some(GitHubTokenError::Unreachable),
        },
        Ok(resp) => {
            let status = resp.status().as_u16();

            // Parse scopes from the response header BEFORE consuming the body.
            // The `X-OAuth-Scopes` header is present for classic PATs; fine-grained
            // PATs and GitHub App tokens omit it.
            let scopes = parse_oauth_scopes_header(resp.headers());

            match status {
                200..=299 => {
                    // Success: parse the login from the JSON body.
                    let login = resp
                        .json::<serde_json::Value>()
                        .await
                        .ok()
                        .and_then(|v| v["login"].as_str().map(|s| s.to_string()));

                    GitHubProbeOutcome {
                        login,
                        scopes,
                        error: None,
                    }
                }
                401 => GitHubProbeOutcome {
                    login: None,
                    scopes,
                    error: Some(GitHubTokenError::InvalidOrExpired),
                },
                403 => GitHubProbeOutcome {
                    login: None,
                    scopes,
                    error: Some(GitHubTokenError::InsufficientScope),
                },
                _ => GitHubProbeOutcome {
                    login: None,
                    scopes,
                    error: Some(GitHubTokenError::Unreachable),
                },
            }
        }
    }
}

/// Parse `X-OAuth-Scopes` header value (comma-separated scope names) into a
/// sorted, deduplicated `Vec<String>`.  Returns an empty vec when the header is
/// absent (fine-grained PATs, GitHub App tokens) or malformed.
pub(crate) fn parse_oauth_scopes_header(headers: &reqwest::header::HeaderMap) -> Vec<String> {
    let Some(value) = headers.get("x-oauth-scopes") else {
        return vec![];
    };
    let Ok(s) = value.to_str() else {
        return vec![];
    };
    let mut scopes: Vec<String> = s
        .split(',')
        .map(|part| part.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    scopes.sort();
    scopes.dedup();
    scopes
}

// ── Main probe ─────────────────────────────────────────────────────────────────

/// Probe all integrations. Does a single cheap GitHub reachability call when a
/// token is present (`GET /user`), surfacing the authenticated login, granted
/// scopes, and a typed error category when the token is wrong/expired/missing scope.
pub async fn probe() -> ConnectionsReport {
    let mut connections = Vec::new();

    // ── GitHub: code host (repos) + work tracker (issues/projects), one token. ──
    match env_nonempty("CAMERATA_GITHUB_TOKEN") {
        None => {
            connections.push(Connection {
                id: "github",
                label: "GitHub — repos + issues/projects".to_string(),
                role: "code+story",
                configured: false,
                ok: None,
                error: None,
                error_category: None,
                mode: None,
                login: None,
                scopes: vec![],
            });
        }
        Some(token) => {
            let outcome = probe_github_token(&token).await;
            let (ok, error_msg, error_category) = match &outcome.error {
                None => (Some(true), None, None),
                Some(cat) => (Some(false), Some(cat.to_string()), Some(cat.clone())),
            };
            connections.push(Connection {
                id: "github",
                label: "GitHub — repos + issues/projects".to_string(),
                role: "code+story",
                configured: true,
                ok,
                error: error_msg,
                error_category,
                mode: None,
                login: outcome.login,
                scopes: outcome.scopes,
            });
        }
    }

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
        error_category: None,
        mode: a_mode,
        login: None,
        scopes: vec![],
    });

    let any_configured = connections.iter().any(|c| c.configured);
    ConnectionsReport {
        connections,
        any_configured,
    }
}

// ── Path helper ────────────────────────────────────────────────────────────────

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

// ── Unit tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: build a minimal HeaderMap with one header value.
    fn headers_with(name: &'static str, value: &str) -> reqwest::header::HeaderMap {
        use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
        let mut map = HeaderMap::new();
        map.insert(
            HeaderName::from_static(name),
            HeaderValue::from_str(value).expect("valid header value"),
        );
        map
    }

    // ── GitHubTokenError Display ───────────────────────────────────────────

    #[test]
    fn error_display_no_token_mentions_env_var() {
        let s = GitHubTokenError::NoToken.to_string();
        assert!(
            s.contains("CAMERATA_GITHUB_TOKEN"),
            "NoToken display must mention the env var: {s}"
        );
    }

    #[test]
    fn error_display_invalid_mentions_401() {
        let s = GitHubTokenError::InvalidOrExpired.to_string();
        assert!(
            s.contains("401"),
            "InvalidOrExpired display must mention HTTP 401: {s}"
        );
    }

    #[test]
    fn error_display_insufficient_scope_mentions_403_and_scopes() {
        let s = GitHubTokenError::InsufficientScope.to_string();
        assert!(
            s.contains("403"),
            "InsufficientScope display must mention HTTP 403: {s}"
        );
        assert!(
            s.contains("scope") || s.contains("scopes"),
            "InsufficientScope display must mention scopes: {s}"
        );
    }

    #[test]
    fn error_display_unreachable_mentions_unreachable() {
        let s = GitHubTokenError::Unreachable.to_string();
        assert!(
            s.to_lowercase().contains("unreachable") || s.to_lowercase().contains("github"),
            "Unreachable display must mention unreachable or GitHub: {s}"
        );
    }

    // ── GitHubTokenError Serialize ─────────────────────────────────────────

    #[test]
    fn error_serializes_to_snake_case_strings() {
        assert_eq!(
            serde_json::to_string(&GitHubTokenError::NoToken).unwrap(),
            r#""no_token""#
        );
        assert_eq!(
            serde_json::to_string(&GitHubTokenError::InvalidOrExpired).unwrap(),
            r#""invalid_or_expired""#
        );
        assert_eq!(
            serde_json::to_string(&GitHubTokenError::InsufficientScope).unwrap(),
            r#""insufficient_scope""#
        );
        assert_eq!(
            serde_json::to_string(&GitHubTokenError::Unreachable).unwrap(),
            r#""unreachable""#
        );
    }

    // ── parse_oauth_scopes_header ──────────────────────────────────────────

    #[test]
    fn parse_scopes_empty_headers_returns_empty_vec() {
        let headers = reqwest::header::HeaderMap::new();
        assert_eq!(parse_oauth_scopes_header(&headers), Vec::<String>::new());
    }

    #[test]
    fn parse_scopes_typical_pat_value() {
        let headers = headers_with("x-oauth-scopes", "repo, read:user, gist");
        let scopes = parse_oauth_scopes_header(&headers);
        // Sorted + deduped.
        assert_eq!(scopes, vec!["gist", "read:user", "repo"]);
    }

    #[test]
    fn parse_scopes_single_scope() {
        let headers = headers_with("x-oauth-scopes", "repo");
        assert_eq!(parse_oauth_scopes_header(&headers), vec!["repo"]);
    }

    #[test]
    fn parse_scopes_trims_whitespace() {
        let headers = headers_with("x-oauth-scopes", "  repo ,  read:user  ");
        let scopes = parse_oauth_scopes_header(&headers);
        assert!(
            scopes.contains(&"repo".to_string()),
            "must include repo: {scopes:?}"
        );
        assert!(
            scopes.contains(&"read:user".to_string()),
            "must include read:user: {scopes:?}"
        );
    }

    #[test]
    fn parse_scopes_deduplicates() {
        let headers = headers_with("x-oauth-scopes", "repo, repo, read:user");
        let scopes = parse_oauth_scopes_header(&headers);
        assert_eq!(
            scopes.iter().filter(|s| s.as_str() == "repo").count(),
            1,
            "duplicate repo scope must be deduplicated: {scopes:?}"
        );
    }

    #[test]
    fn parse_scopes_empty_string_returns_empty_vec() {
        let headers = headers_with("x-oauth-scopes", "");
        assert_eq!(parse_oauth_scopes_header(&headers), Vec::<String>::new());
    }

    // ── Connection serialization ───────────────────────────────────────────

    /// A successful connection must not emit `error`, `error_category`, or the
    /// `scopes`/`login` fields when they are absent.
    #[test]
    fn connection_ok_serializes_without_error_fields() {
        let conn = Connection {
            id: "github",
            label: "GitHub — repos + issues/projects".to_string(),
            role: "code+story",
            configured: true,
            ok: Some(true),
            error: None,
            error_category: None,
            mode: None,
            login: Some("octocat".to_string()),
            scopes: vec!["repo".to_string(), "read:user".to_string()],
        };
        let v = serde_json::to_value(&conn).unwrap();
        assert!(
            v.get("error").is_none() || v["error"].is_null(),
            "ok connection must not emit error field: {v}"
        );
        assert!(
            v.get("error_category").is_none(),
            "ok connection must not emit error_category field: {v}"
        );
        assert_eq!(v["login"], "octocat");
        assert_eq!(v["scopes"], serde_json::json!(["repo", "read:user"]));
    }

    /// A failing connection must emit `error_category` as a snake_case string.
    #[test]
    fn connection_failing_serializes_error_category() {
        let conn = Connection {
            id: "github",
            label: "GitHub — repos + issues/projects".to_string(),
            role: "code+story",
            configured: true,
            ok: Some(false),
            error: Some("token is invalid or has expired (HTTP 401 from GitHub)".to_string()),
            error_category: Some(GitHubTokenError::InvalidOrExpired),
            mode: None,
            login: None,
            scopes: vec![],
        };
        let v = serde_json::to_value(&conn).unwrap();
        assert_eq!(v["error_category"], "invalid_or_expired");
        assert!(v["error"].as_str().unwrap().contains("401"));
        // login and scopes must not appear when absent/empty.
        assert!(
            v.get("login").is_none(),
            "absent login must be omitted: {v}"
        );
        assert!(
            v.get("scopes").is_none(),
            "empty scopes must be omitted: {v}"
        );
    }

    // ── Status-shaping from probe outcomes ─────────────────────────────────
    //
    // These tests exercise the logic that turns a GitHubProbeOutcome into the
    // Connection shape exposed to the UI.  No live GitHub call is made; we
    // construct the outcome directly and verify the resulting Connection fields.

    fn shape_connection(outcome: GitHubProbeOutcome) -> Connection {
        // Replicate the shaping logic from probe() in isolation.
        let (ok, error_msg, error_category) = match &outcome.error {
            None => (Some(true), None, None),
            Some(cat) => (Some(false), Some(cat.to_string()), Some(cat.clone())),
        };
        Connection {
            id: "github",
            label: "GitHub — repos + issues/projects".to_string(),
            role: "code+story",
            configured: true,
            ok,
            error: error_msg,
            error_category,
            mode: None,
            login: outcome.login,
            scopes: outcome.scopes,
        }
    }

    #[test]
    fn shaping_success_outcome_produces_ok_connection() {
        let outcome = GitHubProbeOutcome {
            login: Some("alice".to_string()),
            scopes: vec!["repo".to_string(), "read:user".to_string()],
            error: None,
        };
        let conn = shape_connection(outcome);
        assert_eq!(conn.ok, Some(true));
        assert!(conn.error.is_none());
        assert!(conn.error_category.is_none());
        assert_eq!(conn.login.as_deref(), Some("alice"));
        assert_eq!(conn.scopes, vec!["repo", "read:user"]);
    }

    #[test]
    fn shaping_invalid_token_outcome_produces_401_error() {
        let outcome = GitHubProbeOutcome {
            login: None,
            scopes: vec![],
            error: Some(GitHubTokenError::InvalidOrExpired),
        };
        let conn = shape_connection(outcome);
        assert_eq!(conn.ok, Some(false));
        assert!(conn.error.as_deref().unwrap().contains("401"));
        assert_eq!(
            conn.error_category,
            Some(GitHubTokenError::InvalidOrExpired)
        );
        assert!(conn.login.is_none());
    }

    #[test]
    fn shaping_insufficient_scope_outcome_produces_403_error() {
        let outcome = GitHubProbeOutcome {
            login: None,
            scopes: vec!["public_repo".to_string()],
            error: Some(GitHubTokenError::InsufficientScope),
        };
        let conn = shape_connection(outcome);
        assert_eq!(conn.ok, Some(false));
        assert_eq!(
            conn.error_category,
            Some(GitHubTokenError::InsufficientScope)
        );
        let err = conn.error.as_deref().unwrap();
        assert!(err.contains("403"), "error message must mention 403: {err}");
    }

    #[test]
    fn shaping_unreachable_outcome_produces_error() {
        let outcome = GitHubProbeOutcome {
            login: None,
            scopes: vec![],
            error: Some(GitHubTokenError::Unreachable),
        };
        let conn = shape_connection(outcome);
        assert_eq!(conn.ok, Some(false));
        assert_eq!(conn.error_category, Some(GitHubTokenError::Unreachable));
    }

    #[test]
    fn shaping_success_with_no_scopes_fine_grained_pat() {
        // Fine-grained PATs do not return X-OAuth-Scopes; scopes is empty but
        // the connection is still OK.
        let outcome = GitHubProbeOutcome {
            login: Some("bot-user".to_string()),
            scopes: vec![],
            error: None,
        };
        let conn = shape_connection(outcome);
        assert_eq!(conn.ok, Some(true));
        assert!(conn.error.is_none());
        assert_eq!(conn.login.as_deref(), Some("bot-user"));
        assert!(conn.scopes.is_empty(), "fine-grained PAT has empty scopes");
    }

    // ── Claude path ────────────────────────────────────────────────────────

    #[test]
    fn claude_on_path_returns_bool() {
        // Just assert it doesn't panic; the result depends on the host environment.
        let _ = super::claude_on_path();
    }
}
