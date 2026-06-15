//! Injectable HTTP transport seam. Core logic never imports a real HTTP client
//! directly; all provider adapters accept a `T: HttpTransport` so they can be
//! unit-tested via `FakeTransport` without making network calls.

use async_trait::async_trait;

// ── Response ──────────────────────────────────────────────────────────────────

/// One HTTP response: status code plus body text.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    /// HTTP status code (e.g. 200, 201, 400, 404).
    pub status: u16,
    /// Response body as a UTF-8 string.
    pub body: String,
}

// ── Trait ─────────────────────────────────────────────────────────────────────

/// Minimal HTTP transport used by provider adapters. Keeps adapters testable
/// without a real HTTP client: swap in `FakeTransport` under `#[cfg(test)]`.
#[async_trait]
pub trait HttpTransport: Send + Sync {
    /// Perform an HTTP GET.
    async fn get(&self, url: &str) -> anyhow::Result<HttpResponse>;
    /// Perform an HTTP POST with a JSON body.
    async fn post(&self, url: &str, json_body: &str) -> anyhow::Result<HttpResponse>;
    /// Perform an HTTP PUT with a JSON body.
    async fn put(&self, url: &str, json_body: &str) -> anyhow::Result<HttpResponse>;
}

// ── Reqwest transport (live, production path) ─────────────────────────────────

/// Default `User-Agent` sent on every live request. GitHub's REST API **rejects
/// any request without a `User-Agent` header** with `403 Request forbidden by
/// administrative rules`, and Jira/ADO expect one too, so the transport always
/// sends a non-empty identifier. Provider-agnostic on purpose (it names Camerata,
/// not a specific tracker).
pub const DEFAULT_USER_AGENT: &str = concat!("camerata-orchestrator/", env!("CARGO_PKG_VERSION"));

/// A real HTTP transport backed by `reqwest` with rustls-tls. Constructed with
/// a static `Authorization` header value (e.g. `"Basic <base64>"`). Every
/// request carries that header so the transport stays generic (no provider
/// coupling) while the adapter supplies the correct credential string.
///
/// A default [`DEFAULT_USER_AGENT`] is baked into the client, because GitHub
/// refuses User-Agent-less REST requests; without it every live GitHub call 403s
/// before it is ever authenticated.
pub struct ReqwestTransport {
    client: reqwest::Client,
    auth_header: String,
}

impl ReqwestTransport {
    /// Build a transport. `auth_header` is the full value for the
    /// `Authorization` header (e.g. `"Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ=="`).
    ///
    /// The client carries [`DEFAULT_USER_AGENT`] on every request; use
    /// [`ReqwestTransport::with_user_agent`] to override it.
    pub fn new(auth_header: impl Into<String>) -> anyhow::Result<Self> {
        Self::with_user_agent(auth_header, DEFAULT_USER_AGENT)
    }

    /// Build a transport with an explicit `User-Agent`. Exposed so a deployment
    /// can identify itself distinctly (GitHub recommends a real app/user name).
    pub fn with_user_agent(
        auth_header: impl Into<String>,
        user_agent: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .user_agent(user_agent.into())
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build reqwest client: {e}"))?;
        Ok(Self {
            client,
            auth_header: auth_header.into(),
        })
    }
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn get(&self, url: &str) -> anyhow::Result<HttpResponse> {
        let resp = self
            .client
            .get(url)
            .header("Authorization", &self.auth_header)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("GET {url}: {e}"))?;
        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .map_err(|e| anyhow::anyhow!("GET {url} body: {e}"))?;
        Ok(HttpResponse { status, body })
    }

    async fn post(&self, url: &str, json_body: &str) -> anyhow::Result<HttpResponse> {
        let resp = self
            .client
            .post(url)
            .header("Authorization", &self.auth_header)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(json_body.to_string())
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("POST {url}: {e}"))?;
        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .map_err(|e| anyhow::anyhow!("POST {url} body: {e}"))?;
        Ok(HttpResponse { status, body })
    }

    async fn put(&self, url: &str, json_body: &str) -> anyhow::Result<HttpResponse> {
        let resp = self
            .client
            .put(url)
            .header("Authorization", &self.auth_header)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(json_body.to_string())
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("PUT {url}: {e}"))?;
        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .map_err(|e| anyhow::anyhow!("PUT {url} body: {e}"))?;
        Ok(HttpResponse { status, body })
    }
}

// ── FakeTransport (test double) ───────────────────────────────────────────────

/// A scripted HTTP transport for tests. Construct with scripted responses keyed
/// by `(method, url_substring)`. When a call matches, returns that response.
/// Falls back to a 404 when no key matches. Records every call so tests can
/// assert the requests that were issued.
///
/// Matching uses the FIRST registered entry whose `url_substring` appears inside
/// the requested URL (case-sensitive substring, not a regex). Register more
/// specific substrings before less specific ones to avoid false matches.
pub struct FakeTransport {
    // Stored as (method_uppercase, url_substring, response_to_clone).
    scripts: Vec<(String, String, HttpResponse)>,
    /// Recorded calls: (method_uppercase, full_url, body).
    pub calls: std::sync::Mutex<Vec<(String, String, String)>>,
}

impl FakeTransport {
    /// Create an empty fake with no scripted responses. All calls return 404.
    pub fn new() -> Self {
        Self {
            scripts: Vec::new(),
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Register a scripted response. When the call method matches (case-insensitive)
    /// and `url_substring` appears inside the request URL, this response is returned.
    /// Earlier-registered entries win over later ones.
    pub fn on(
        mut self,
        method: impl Into<String>,
        url_substring: impl Into<String>,
        status: u16,
        body: impl Into<String>,
    ) -> Self {
        self.scripts.push((
            method.into().to_uppercase(),
            url_substring.into(),
            HttpResponse {
                status,
                body: body.into(),
            },
        ));
        self
    }

    /// Return all recorded calls as `(method, url, body)` triples.
    pub fn recorded_calls(&self) -> Vec<(String, String, String)> {
        self.calls
            .lock()
            .expect("FakeTransport calls mutex poisoned")
            .clone()
    }

    fn find(&self, method: &str, url: &str) -> HttpResponse {
        let method_up = method.to_uppercase();
        for (m, substr, resp) in &self.scripts {
            if *m == method_up && url.contains(substr.as_str()) {
                return resp.clone();
            }
        }
        HttpResponse {
            status: 404,
            body: format!("FakeTransport: no script for {method} {url}"),
        }
    }

    fn record(&self, method: &str, url: &str, body: &str) {
        self.calls
            .lock()
            .expect("FakeTransport calls mutex poisoned")
            .push((method.to_uppercase(), url.to_string(), body.to_string()));
    }
}

impl Default for FakeTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HttpTransport for FakeTransport {
    async fn get(&self, url: &str) -> anyhow::Result<HttpResponse> {
        self.record("GET", url, "");
        Ok(self.find("GET", url))
    }

    async fn post(&self, url: &str, json_body: &str) -> anyhow::Result<HttpResponse> {
        self.record("POST", url, json_body);
        Ok(self.find("POST", url))
    }

    async fn put(&self, url: &str, json_body: &str) -> anyhow::Result<HttpResponse> {
        self.record("PUT", url, json_body);
        Ok(self.find("PUT", url))
    }
}

// ── Live-transport wire tests ─────────────────────────────────────────────────
//
// These exercise `ReqwestTransport` against a throwaway loopback HTTP server so
// we assert what actually goes ON THE WIRE — the layer `FakeTransport` cannot
// see. They exist because a missing `User-Agent` header (which GitHub 403s on)
// was invisible to the shape-only fake-transport tests and only surfaced against
// real GitHub. This guards that regression.
#[cfg(test)]
mod wire_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Stand up a one-shot loopback server that captures the raw request head of
    /// the first connection and replies `200 {}`. Returns (base_url, JoinHandle
    /// yielding the captured request bytes as a String).
    async fn capture_one_request() -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            // Read until the end of the request headers (CRLFCRLF). A GET has no
            // body, so the head is the whole request.
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let n = socket.read(&mut chunk).await.expect("read");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write response");
            socket.flush().await.expect("flush");
            String::from_utf8_lossy(&buf).into_owned()
        });
        (format!("http://{addr}/"), handle)
    }

    #[tokio::test]
    async fn live_get_sends_the_default_user_agent() {
        let (url, server) = capture_one_request().await;
        let transport = ReqwestTransport::new("Bearer test-token").expect("build transport");

        let resp = transport.get(&url).await.expect("get succeeds");
        assert_eq!(resp.status, 200);

        let request = server.await.expect("server task");
        // Header names are case-insensitive on the wire; lowercase the head.
        let lower = request.to_lowercase();
        assert!(
            lower.contains("user-agent:"),
            "request must carry a User-Agent header (GitHub 403s without it); got:\n{request}"
        );
        assert!(
            request.contains(DEFAULT_USER_AGENT),
            "User-Agent must be DEFAULT_USER_AGENT ({DEFAULT_USER_AGENT}); got:\n{request}"
        );
        // The auth header the adapter supplied must also be present.
        assert!(
            lower.contains("authorization: bearer test-token"),
            "request must carry the supplied Authorization header; got:\n{request}"
        );
    }

    #[tokio::test]
    async fn with_user_agent_overrides_the_default() {
        let (url, server) = capture_one_request().await;
        let transport = ReqwestTransport::with_user_agent("Bearer t", "my-custom-agent/9.9")
            .expect("build transport");

        transport.get(&url).await.expect("get succeeds");

        let request = server.await.expect("server task");
        assert!(
            request.contains("my-custom-agent/9.9"),
            "explicit user agent must be on the wire; got:\n{request}"
        );
    }

    #[test]
    fn default_user_agent_is_nonempty_and_named() {
        // Cheap guard: the const can't be blanked out without tripping this.
        assert!(!DEFAULT_USER_AGENT.is_empty());
        assert!(DEFAULT_USER_AGENT.starts_with("camerata-orchestrator/"));
    }
}
