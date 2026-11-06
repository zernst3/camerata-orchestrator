//! The network seam. [`Fetcher`] is the one trait the checker talks to, so every test runs with no
//! network (a local fake server or an injected fetcher); [`HttpFetcher`] is the real transport.
//!
//! Classification, decided here once:
//!
//! - **ok**: a 2xx, reached directly.
//! - **redirected**: a 2xx reached after one or more 3xx hops (the final URL is recorded so the
//!   rule's source can be updated).
//! - **dead**: the server answered and said the page is gone (404, 410).
//! - **refused**: the [`NetworkPolicy`] would not let us talk to that address (loopback, private,
//!   link-local, a non-web scheme). Not a verdict on the page; nothing was requested.
//! - **unreachable**: anything we could not get a verdict about: DNS failure, refused or reset
//!   connection, timeout, a redirect chain that never ends (login, consent and bot-wall loops look
//!   exactly like this), and also 401, 403, 429 and 5xx. A network failure is NEVER "dead".

use std::error::Error as StdError;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::redirect::Policy;
use reqwest::{StatusCode, Url};

/// Most redirect hops followed before the chain is called a loop.
const MAX_REDIRECT_HOPS: usize = 8;
/// Default largest response body read. A section hash needs the page, not an unbounded download.
pub const DEFAULT_MAX_BODY_BYTES: usize = 5 * 1024 * 1024;

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
        /// True when the body was cut at the size limit. A truncated body must never be hashed or
        /// searched for an anchor: the missing tail could hold the section.
        truncated: bool,
    },
    /// The page is gone: a 404 or 410.
    Dead {
        /// What happened, in words ("HTTP 404").
        detail: String,
    },
    /// The network policy refused the address or scheme; nothing was requested.
    Refused {
        /// Why, in words ("address 10.0.0.5 is not a public address").
        detail: String,
    },
    /// No usable answer. Never counted as dead.
    Unreachable {
        /// What happened, in words ("timed out", "HTTP 503", "too many redirects").
        detail: String,
    },
}

/// Fetch one page. `want_body` is false when only the link's health matters, so the body is
/// dropped instead of read.
// Written in the form `#[async_trait]` expands to, so implementations keep using the
// macro unchanged. Declaring it with the macro adds a bare `#[must_use]` to each method,
// which clippy (Rust 1.99+) reports as `double_must_use`: the boxed future is already
// `#[must_use]`.
pub trait Fetcher: Send + Sync {
    /// Fetch `url` (fragment already stripped by the caller).
    fn fetch<'life0, 'life1, 'async_trait>(
        &'life0 self,
        url: &'life1 str,
        want_body: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = FetchOutcome> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait;
}

// ---------------------------------------------------------------------------------------------
// SSRF guard
// ---------------------------------------------------------------------------------------------

/// Which addresses the fetcher may talk to. The corpus is trusted text, but a redirect is chosen
/// by a third party, so every hop (and every address a name resolves to) is checked: a public
/// page must not be able to bounce us to a service on the operator's own network.
///
/// The default is [`NetworkPolicy::strict`]. Tests that talk to a loopback fake server inject
/// [`NetworkPolicy::allow_loopback`]; that is the only relaxation there is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NetworkPolicy {
    loopback_ok: bool,
}

impl NetworkPolicy {
    /// Public internet only: http(s), no loopback, private, link-local or unique-local
    /// addresses, no `.localhost` / `.local` / single-label names.
    pub const fn strict() -> Self {
        Self { loopback_ok: false }
    }

    /// Like [`strict`](Self::strict) but literal loopback addresses (127.0.0.0/8, ::1) are
    /// allowed, so a test can use a fake server on 127.0.0.1. Names such as `localhost` are
    /// still refused, as are all other private ranges.
    pub const fn allow_loopback() -> Self {
        Self { loopback_ok: true }
    }

    /// Whether `ip` is an address this policy lets us connect to.
    pub fn allows_ip(&self, ip: IpAddr) -> bool {
        let ip = canonical_ip(ip);
        if self.loopback_ok && ip.is_loopback() {
            return true;
        }
        match ip {
            IpAddr::V4(v4) => is_public_v4(v4),
            IpAddr::V6(v6) => is_public_v6(v6),
        }
    }

    /// Check a URL before requesting it: scheme, then host (a literal IP against
    /// [`allows_ip`](Self::allows_ip), a name against the local-name rules). Names are checked
    /// again after DNS resolution by the guarded resolver.
    pub fn check_url(&self, url: &Url) -> Result<(), String> {
        match url.scheme() {
            "http" | "https" => {}
            other => return Err(format!("scheme '{other}' is not allowed")),
        }
        let host = url.host_str().ok_or("URL has no host")?;
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        if let Ok(ip) = bare.parse::<IpAddr>() {
            return if self.allows_ip(ip) {
                Ok(())
            } else {
                Err(format!("address {ip} is not a public address"))
            };
        }
        let name = host.trim_end_matches('.').to_ascii_lowercase();
        if name == "localhost"
            || name.ends_with(".localhost")
            || name.ends_with(".local")
            || !name.contains('.')
        {
            return Err(format!("host '{name}' is a local name"));
        }
        Ok(())
    }
}

/// Map an IPv4-mapped IPv6 address (::ffff:a.b.c.d) to its IPv4 form so it cannot dodge the
/// IPv4 rules.
fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        v4 => v4,
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || a == 0
        || a >= 240 // reserved
        || (a == 100 && (64..=127).contains(&b)) // carrier-grade NAT
        || (a == 192 && b == 0 && c == 0) // IETF protocol assignments
        || (a == 198 && (18..=19).contains(&b))) // benchmarking
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let seg = ip.segments();
    // NAT64 (64:ff9b::/96) embeds an IPv4 address in the last 32 bits.
    if seg[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        let [a, b] = [seg[6].to_be_bytes(), seg[7].to_be_bytes()];
        return is_public_v4(Ipv4Addr::new(a[0], a[1], b[0], b[1]));
    }
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (seg[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
        || (seg[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
        || (seg[0] & 0xffc0) == 0xfec0) // deprecated site-local fec0::/10
}

/// Raised by the resolver when a name resolves to an address the policy forbids.
#[derive(Debug)]
struct RefusedAddress {
    host: String,
    ip: IpAddr,
}

impl fmt::Display for RefusedAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} resolves to {}, which is not a public address",
            self.host, self.ip
        )
    }
}

impl StdError for RefusedAddress {}

/// Resolve `host` and refuse the whole answer if ANY returned address is forbidden (a name with a
/// public and a private record is exactly the rebinding shape to avoid).
async fn resolve_checked(
    host: &str,
    policy: NetworkPolicy,
) -> Result<Vec<SocketAddr>, Box<dyn StdError + Send + Sync>> {
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, 0)).await?.collect();
    if let Some(bad) = addrs.iter().find(|a| !policy.allows_ip(a.ip())) {
        return Err(Box::new(RefusedAddress {
            host: host.to_string(),
            ip: bad.ip(),
        }));
    }
    Ok(addrs)
}

/// The DNS resolver handed to reqwest: every connection's addresses are checked after
/// resolution, on every redirect hop.
struct GuardedResolver {
    policy: NetworkPolicy,
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let policy = self.policy;
        Box::pin(async move {
            let addrs = resolve_checked(name.as_str(), policy).await?;
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

/// The refusal reason if `error`'s cause chain contains a [`RefusedAddress`].
fn refusal_in(error: &(dyn StdError + 'static)) -> Option<String> {
    let mut current: Option<&(dyn StdError + 'static)> = Some(error);
    while let Some(e) = current {
        if let Some(refused) = e.downcast_ref::<RefusedAddress>() {
            return Some(refused.to_string());
        }
        current = e.source();
    }
    None
}

// ---------------------------------------------------------------------------------------------
// the real transport
// ---------------------------------------------------------------------------------------------

/// The real transport: `reqwest` with redirects handled by hand (so a redirect is observable and
/// every hop is policy-checked), a guarded DNS resolver, a request timeout, and a clear user
/// agent.
pub struct HttpFetcher {
    client: reqwest::Client,
    policy: NetworkPolicy,
    max_body_bytes: usize,
}

impl HttpFetcher {
    /// Build a fetcher with `timeout` per request and `user_agent` on every request, under the
    /// [`NetworkPolicy::strict`] policy.
    pub fn new(timeout: Duration, user_agent: &str) -> anyhow::Result<Self> {
        Self::with_policy(timeout, user_agent, NetworkPolicy::strict())
    }

    /// Like [`new`](Self::new) with an explicit policy (tests inject
    /// [`NetworkPolicy::allow_loopback`]).
    pub fn with_policy(
        timeout: Duration,
        user_agent: &str,
        policy: NetworkPolicy,
    ) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(timeout)
            .user_agent(user_agent)
            .dns_resolver(Arc::new(GuardedResolver { policy }))
            .build()?;
        Ok(Self {
            client,
            policy,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        })
    }

    /// Cut response bodies at `bytes` (default [`DEFAULT_MAX_BODY_BYTES`]).
    #[must_use]
    pub fn with_max_body_bytes(mut self, bytes: usize) -> Self {
        self.max_body_bytes = bytes;
        self
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

/// Read at most `limit` bytes of `response` as lossy UTF-8. The flag is true when more than
/// `limit` bytes were on offer, i.e. the text is cut.
async fn read_capped(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<(String, bool), reqwest::Error> {
    let mut bytes: Vec<u8> = Vec::new();
    let mut truncated = false;
    while let Some(chunk) = response.chunk().await? {
        bytes.extend_from_slice(&chunk);
        if bytes.len() > limit {
            bytes.truncate(limit);
            truncated = true;
            break;
        }
    }
    Ok((String::from_utf8_lossy(&bytes).into_owned(), truncated))
}

#[async_trait]
impl Fetcher for HttpFetcher {
    async fn fetch(&self, url: &str, want_body: bool) -> FetchOutcome {
        let mut current = url.to_string();
        let mut redirected = false;
        for _ in 0..=MAX_REDIRECT_HOPS {
            let parsed = match Url::parse(&current) {
                Ok(u) => u,
                Err(_) => {
                    return FetchOutcome::Dead {
                        detail: "malformed URL".to_string(),
                    }
                }
            };
            if let Err(detail) = self.policy.check_url(&parsed) {
                return FetchOutcome::Refused { detail };
            }
            let response = match self.client.get(parsed).send().await {
                Ok(r) => r,
                Err(e) => {
                    if let Some(detail) = refusal_in(&e) {
                        return FetchOutcome::Refused { detail };
                    }
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
            let (body, truncated) = if want_body {
                match read_capped(response, self.max_body_bytes).await {
                    Ok((text, truncated)) => (Some(text), truncated),
                    Err(e) => {
                        return FetchOutcome::Unreachable {
                            detail: format!("body read failed: {e}"),
                        }
                    }
                }
            } else {
                (None, false)
            };
            return FetchOutcome::Ok {
                final_url: current,
                redirected,
                body,
                truncated,
            };
        }
        // Login, consent and bot-wall pages loop like this; that is not evidence the page is gone.
        FetchOutcome::Unreachable {
            detail: "too many redirects".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().expect("ip")
    }

    #[test]
    fn strict_policy_refuses_every_non_public_range() {
        let p = NetworkPolicy::strict();
        for bad in [
            "127.0.0.1",
            "127.8.8.8",
            "0.0.0.0",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "::",
            "fe80::1",
            "fc00::1",
            "fd12:3456::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:1",
        ] {
            assert!(!p.allows_ip(ip(bad)), "{bad} should be refused");
        }
        for good in ["8.8.8.8", "93.184.216.34", "172.32.0.1", "2606:4700::1111"] {
            assert!(p.allows_ip(ip(good)), "{good} should be allowed");
        }
    }

    #[test]
    fn allow_loopback_relaxes_only_loopback() {
        let p = NetworkPolicy::allow_loopback();
        assert!(p.allows_ip(ip("127.0.0.1")));
        assert!(p.allows_ip(ip("::1")));
        assert!(!p.allows_ip(ip("10.0.0.1")));
        assert!(!p.allows_ip(ip("169.254.169.254")));
    }

    #[test]
    fn urls_are_checked_by_scheme_literal_ip_and_local_name() {
        let strict = NetworkPolicy::strict();
        let check = |p: &NetworkPolicy, u: &str| p.check_url(&Url::parse(u).expect("url"));
        assert!(check(&strict, "https://example.com/a").is_ok());
        assert!(check(&strict, "http://93.184.216.34/").is_ok());
        for bad in [
            "file:///etc/passwd",
            "ftp://example.com/x",
            "http://localhost/",
            "http://LOCALHOST:8080/",
            "http://app.localhost/",
            "http://printer.local/",
            "http://intranet/",
            "http://127.0.0.1:9/",
            "http://[::1]/",
            "http://10.0.0.5/",
            "http://169.254.169.254/latest/meta-data",
            "http://2130706433/", // 127.0.0.1 as a decimal integer
        ] {
            assert!(check(&strict, bad).is_err(), "{bad} should be refused");
        }
        let lo = NetworkPolicy::allow_loopback();
        assert!(check(&lo, "http://127.0.0.1:9/").is_ok());
        assert!(
            check(&lo, "http://localhost:9/").is_err(),
            "names stay refused"
        );
    }

    #[tokio::test]
    async fn a_name_that_resolves_to_a_forbidden_address_is_refused_after_dns() {
        // `localhost` always resolves to loopback; the post-resolution check must refuse it even
        // though this function does not look at the name.
        let err = resolve_checked("localhost", NetworkPolicy::strict())
            .await
            .expect_err("loopback must be refused");
        assert!(refusal_in(err.as_ref()).is_some(), "{err}");
        assert!(
            resolve_checked("localhost", NetworkPolicy::allow_loopback())
                .await
                .is_ok()
        );
    }
}
