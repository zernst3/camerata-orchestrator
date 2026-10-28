//! Runtime vulnerabilities from the NVD CVE API 2.0, looked up by CPE name.
//!
//! One request asks for one runtime at one exact version, for example
//! `cpe:2.3:a:nodejs:node.js:18.17.0:*:*:*:*:*:*:*`. NVD matches the CPE against each CVE's
//! configuration, including version ranges, and returns the CVEs that list it as vulnerable.
//!
//! The only text sent is a product name and a version string; no source code leaves the machine.
//!
//! # Rate limit
//!
//! The anonymous limit is five requests per rolling 30 seconds. [`NvdPolicy`] spaces the requests
//! and backs off on a rate-limit answer (403, 429 or 503), honouring `Retry-After`. Running out of
//! retries is an ordinary failed fetch: the scan continues and the report says the data was
//! unavailable. It never fails the scan.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::detect::Product;
use crate::vuln_intel::{FeedFetcher, FetchStatusError};

/// The NVD CVE API 2.0 endpoint.
pub const NVD_API: &str = "https://services.nvd.nist.gov/rest/json/cves/2.0";

/// The most pages one lookup reads. NVD returns up to 2,000 CVEs per page; no runtime release
/// has anywhere near that many, so a lookup that runs past this is not believed.
const MAX_PAGES: usize = 5;

/// How requests are spaced and retried.
#[derive(Debug, Clone, Copy)]
pub struct NvdPolicy {
    /// The pause before every request after the first, to stay under the anonymous limit.
    pub spacing: Duration,
    /// The first back-off after a rate-limit answer; it doubles on each retry.
    pub backoff: Duration,
    /// The longest a single `Retry-After` is honoured.
    pub max_wait: Duration,
    /// Retries after the first attempt.
    pub retries: u32,
}

impl Default for NvdPolicy {
    fn default() -> Self {
        Self {
            // Five requests per 30 seconds is one per six seconds.
            spacing: Duration::from_secs(6),
            backoff: Duration::from_secs(8),
            max_wait: Duration::from_secs(30),
            retries: 2,
        }
    }
}

impl NvdPolicy {
    /// No waiting at all, for tests that run against fixtures.
    pub fn instant() -> Self {
        Self {
            spacing: Duration::ZERO,
            backoff: Duration::ZERO,
            max_wait: Duration::ZERO,
            retries: 2,
        }
    }
}

/// One CVE as the audit reads it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NvdCve {
    pub id: String,
    /// `critical` | `high` | `medium` | `low` | `info`, derived from the CVSS score with the same
    /// scorer the dependency audit uses. `medium` as a placeholder when `rated` is false.
    pub severity: String,
    pub score: Option<f64>,
    /// False when NVD has published no usable CVSS rating (an entry still awaiting analysis).
    pub rated: bool,
}

/// What one lookup returned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NvdResult {
    /// The date (`YYYY-MM-DD`) of NVD's own response timestamp, when it gave one.
    pub as_of: Option<String>,
    pub cves: Vec<NvdCve>,
}

/// The CPE 2.3 names for a runtime at an exact version (one per vendor NVD lists it under), or
/// none for a product NVD is not asked about or a version that is not plain dotted numbers.
pub fn cpe_names(product: Product, version: &str) -> Vec<String> {
    if version.is_empty() || !version.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Vec::new();
    }
    // .NET Core 1.x to 3.x are listed under their own product; 5 and later are plain ".NET".
    let core = product == Product::DotNet
        && version
            .split('.')
            .next()
            .and_then(|major| major.parse::<u32>().ok())
            .is_some_and(|major| major < 5);
    if core {
        return vec![format!(
            "cpe:2.3:a:microsoft:.net_core:{version}:*:*:*:*:*:*:*"
        )];
    }
    product
        .cpe_prefixes()
        .iter()
        .map(|prefix| format!("{prefix}:{version}:*:*:*:*:*:*:*"))
        .collect()
}

/// The request URL for one page of one CPE.
pub fn query_url(cpe: &str, start_index: usize) -> String {
    format!(
        "{NVD_API}?cpeName={cpe}&isVulnerable&noRejected&resultsPerPage=2000&startIndex={start_index}"
    )
}

struct Page {
    total_results: usize,
    as_of: Option<String>,
    cves: Vec<NvdCve>,
}

/// The CVSS-derived severity of one CVE record, scored with the dependency audit's scorer.
///
/// Order: a CVSS 3.1 then 3.0 metric (scored locally from its vector, falling back to NVD's own
/// base score), then CVSS 4.0 and 2.0 (NVD's base score, or a v2 vector scored locally). Within a
/// version, NVD's own "Primary" analysis comes first. `None` when no metric gives a score.
fn rate(cve: &serde_json::Value) -> Option<f64> {
    let metrics = cve.get("metrics")?;
    for key in [
        "cvssMetricV31",
        "cvssMetricV30",
        "cvssMetricV40",
        "cvssMetricV2",
    ] {
        let Some(entries) = metrics.get(key).and_then(|m| m.as_array()) else {
            continue;
        };
        let mut ordered: Vec<&serde_json::Value> = entries.iter().collect();
        ordered.sort_by_key(|e| e.get("type").and_then(|t| t.as_str()) != Some("Primary"));
        for entry in ordered {
            let Some(data) = entry.get("cvssData") else {
                continue;
            };
            let vector = data.get("vectorString").and_then(|v| v.as_str());
            let from_vector = vector.and_then(|v| match key {
                "cvssMetricV31" | "cvssMetricV30" => crate::dep_audit::cvss_v3_base_score(v),
                "cvssMetricV2" => crate::dep_audit::cvss_v2_base_score(v),
                _ => None,
            });
            if let Some(score) = from_vector.or_else(|| data.get("baseScore")?.as_f64()) {
                return Some(score);
            }
        }
    }
    None
}

fn parse_page(json: &str) -> anyhow::Result<Page> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    let rows = v
        .get("vulnerabilities")
        .and_then(|a| a.as_array())
        .ok_or_else(|| anyhow::anyhow!("NVD response has no vulnerabilities list"))?;
    let total_results = v
        .get("totalResults")
        .and_then(|t| t.as_u64())
        .map_or(rows.len(), |t| t as usize);
    let as_of = v
        .get("timestamp")
        .and_then(|t| t.as_str())
        .and_then(|t| t.get(..10))
        .filter(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").is_ok())
        .map(str::to_string);
    let mut cves = Vec::new();
    for row in rows {
        let Some(cve) = row.get("cve") else { continue };
        let Some(id) = cve.get("id").and_then(|i| i.as_str()) else {
            continue;
        };
        let score = rate(cve);
        cves.push(NvdCve {
            id: id.trim().to_ascii_uppercase(),
            severity: score
                .map_or("medium", crate::dep_audit::cvss_score_to_severity)
                .to_string(),
            score,
            rated: score.is_some(),
        });
    }
    if !rows.is_empty() && cves.is_empty() {
        anyhow::bail!(
            "NVD response carried {} rows but none with a CVE id",
            rows.len()
        );
    }
    Ok(Page {
        total_results,
        as_of,
        cves,
    })
}

/// True for the answers NVD gives when a client is going too fast.
fn is_rate_limited(err: &anyhow::Error) -> Option<Option<u64>> {
    let status = err.downcast_ref::<FetchStatusError>()?;
    matches!(status.status, 403 | 429 | 503).then_some(status.retry_after_secs)
}

/// One request with the back-off policy applied.
async fn get_with_backoff(
    fetcher: &dyn FeedFetcher,
    url: &str,
    policy: &NvdPolicy,
) -> anyhow::Result<String> {
    let mut wait = policy.backoff;
    let mut attempt = 0;
    loop {
        match fetcher.get(url).await {
            Ok(body) => return Ok(body),
            Err(e) => match is_rate_limited(&e) {
                Some(retry_after) if attempt < policy.retries => {
                    attempt += 1;
                    let pause = retry_after
                        .map_or(wait, Duration::from_secs)
                        .min(policy.max_wait);
                    tracing::warn!("NVD rate limit; waiting {pause:?} (retry {attempt})");
                    tokio::time::sleep(pause).await;
                    wait = wait.saturating_mul(2);
                }
                _ => return Err(e),
            },
        }
    }
}

/// Look up every CVE NVD lists against `cpe`, reading every page. An error means the lookup is
/// not complete, so nothing from it is used.
pub async fn lookup(
    fetcher: &dyn FeedFetcher,
    cpe: &str,
    policy: &NvdPolicy,
) -> anyhow::Result<NvdResult> {
    let mut cves: Vec<NvdCve> = Vec::new();
    let mut as_of: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let body = get_with_backoff(fetcher, &query_url(cpe, cves.len()), policy).await?;
        let page = parse_page(&body)?;
        let returned = page.cves.len();
        as_of = as_of.or(page.as_of);
        cves.extend(page.cves);
        if cves.len() >= page.total_results {
            cves.sort_by(|a, b| a.id.cmp(&b.id));
            cves.dedup_by(|a, b| a.id == b.id);
            return Ok(NvdResult { as_of, cves });
        }
        if returned == 0 {
            anyhow::bail!("NVD reported more results than it returned");
        }
    }
    anyhow::bail!("NVD lookup for {cpe} ran past {MAX_PAGES} pages")
}

/// [`lookup`] for each of a runtime's CPE names, `policy.spacing` apart, merged into one result
/// (a CVE listed under two vendors counts once). If any lookup fails the whole answer is
/// incomplete and is an error.
pub async fn lookup_all(
    fetcher: &dyn FeedFetcher,
    cpes: &[String],
    policy: &NvdPolicy,
) -> anyhow::Result<NvdResult> {
    let mut cves: Vec<NvdCve> = Vec::new();
    let mut as_of: Option<String> = None;
    for (i, cpe) in cpes.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(policy.spacing).await;
        }
        let one = lookup(fetcher, cpe, policy).await?;
        as_of = match (as_of, one.as_of) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        cves.extend(one.cves);
    }
    cves.sort_by(|a, b| a.id.cmp(&b.id));
    cves.dedup_by(|a, b| a.id == b.id);
    Ok(NvdResult { as_of, cves })
}
