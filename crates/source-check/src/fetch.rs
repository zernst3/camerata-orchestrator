//! The network seam. [`Fetcher`] is the one trait the checker talks to, so every test runs with no
//! network (a local fake server or an injected fetcher); [`HttpFetcher`] is the real transport.
//!
//! Classification, decided here once:
//!
//! - **ok**: a 2xx, reached directly.
//! - **redirected**: a 2xx reached after one or more 3xx hops (the final URL is recorded so the
//!   rule's source can be updated).
//! - **dead**: the server answered and said the page is gone (404, 410), or the redirect chain
//!   never ends.
//! - **unreachable**: anything we could not get an answer about: DNS failure, refused or reset
//!   connection, timeout, and also 401, 403, 429 and 5xx (a bot wall or a server having a bad
//!   hour is not evidence the page is gone). A network failure is NEVER "dead".

use std::time::Duration;

use async_trait::async_trait;
use reqwest::redirect::Policy;
use reqwest::StatusCode;

/// Most redirect hops followed before the chain is called a loop.
const MAX_REDIRECT_HOPS: usize = 8;
/// Largest response body read. A section hash needs the page, not an unbounded download.
const MAX_BODY_BYTES: usize = 5 * 1024 * 1024;

/// What one fetch found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchOutcome {
    /// A 2xx response. `redirected` is true when at least one 3xx hop led here.
    Ok {
        /// The URL that finally answered (equal to the request URL when not redirected).
        final_url: String,
        /// Whether any redirect hop was followed.
        redirected: bool,
        /// The decoded body, present only when the caller asked for it.
        body: Option<String>,
    },
    /// The page is gone: a 404/410 or an endless redirect chain.
    Dead {
        /// What happened, in words ("HTTP 404", "too many redirects").
        detail: String,
    },
    /// No usable answer. Never counted as dead.
    Unreachable {
        /// What happened, in words ("timed out", "HTTP 503", "connection refused").
        detail: String,
    },
}

/// Fetch one page. `want_body` is false when only the link's health matters, so the body is
/// dropped instead of read.
#[async_trait]
pub trait Fetcher: Send + Sync {
    /// Fetch `url` (fragment already stripped by the caller).
    async fn fetch(&self, url: &str, want_body: bool) -> FetchOutcome;
}

/// The real transport: `reqwest` with redirects handled by hand (so a redirect is observable),
/// a request timeout, and a clear user agent.
pub struct HttpFetcher {
    client: reqwest::Client,
}

impl HttpFetcher {
    /// Build a fetcher with `timeout` per request and `user_agent` on every request.
    pub fn new(timeout: Duration, user_agent: &str) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(timeout)
            .user_agent(user_agent)
            .build()?;
        Ok(Self { client })
    }
}

/// Classify a final (non-redirect) status.
fn classify_status(status: StatusCode) -> Result<(), FetchOutcome> {
    if status.is_success() {
        Ok(())
    } else if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
        Err(FetchOutcome::Dead {
            detail: format!("HTTP {}", status.as_u16()),
        })
    } else {
        Err(FetchOutcome::Unreachable {
            detail: format!("HTTP {}", status.as_u16()),
        })
    }
}

/// Read at most [`MAX_BODY_BYTES`] of `response` as lossy UTF-8.
async fn read_capped(mut response: reqwest::Response) -> Result<String, reqwest::Error> {
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        bytes.extend_from_slice(&chunk);
        if bytes.len() >= MAX_BODY_BYTES {
            bytes.truncate(MAX_BODY_BYTES);
            break;
        }
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[async_trait]
impl Fetcher for HttpFetcher {
    async fn fetch(&self, url: &str, want_body: bool) -> FetchOutcome {
        let mut current = url.to_string();
        let mut redirected = false;
        for _ in 0..=MAX_REDIRECT_HOPS {
            let response = match self.client.get(&current).send().await {
                Ok(r) => r,
                Err(e) => {
                    let detail = if e.is_timeout() {
                        "timed out".to_string()
                    } else if e.is_connect() {
                        "connection failed".to_string()
                    } else {
                        format!("request failed: {e}")
                    };
                    return FetchOutcome::Unreachable { detail };
                }
            };
            let status = response.status();
            if status.is_redirection() {
                let next = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|loc| response.url().join(loc).ok());
                match next {
                    Some(next) => {
                        redirected = true;
                        current = next.to_string();
                        continue;
                    }
                    None => {
                        return FetchOutcome::Unreachable {
                            detail: format!("HTTP {} with no usable Location", status.as_u16()),
                        }
                    }
                }
            }
            if let Err(outcome) = classify_status(status) {
                return outcome;
            }
            let body = if want_body {
                match read_capped(response).await {
                    Ok(text) => Some(text),
                    Err(e) => {
                        return FetchOutcome::Unreachable {
                            detail: format!("body read failed: {e}"),
                        }
                    }
                }
            } else {
                None
            };
            return FetchOutcome::Ok {
                final_url: current,
                redirected,
                body,
            };
        }
        FetchOutcome::Dead {
            detail: "too many redirects".to_string(),
        }
    }
}
