//! The two vulnerability-intelligence feeds (CISA KEV and FIRST.org EPSS): their parsers, their
//! on-disk cache, and how a fetch result and a cache combine into one [`FeedState`].
//!
//! Everything here is pure or local-file I/O. The network sits behind
//! [`super::FeedFetcher`], so the tests drive these functions with fixture feeds and no network.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use super::MAX_CACHE_AGE_DAYS;

/// The CISA Known Exploited Vulnerabilities catalog, reduced to what enrichment reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KevFeed {
    /// The catalog's own `dateReleased`, as `YYYY-MM-DD`. This is the "as of" date the stamp
    /// prints.
    pub date_released: String,
    /// Upper-cased CVE id to the date CISA added it to the catalog (`dateAdded`, may be empty
    /// when the entry carries none).
    pub entries: BTreeMap<String, String>,
}

/// One EPSS score: the probability of exploitation in the next 30 days and its percentile, both
/// in `0.0..=1.0` as FIRST publishes them.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EpssScore {
    pub epss: f64,
    pub percentile: f64,
}

/// One run's EPSS lookup: every CVE that was asked about, and the scores FIRST returned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EpssBatch {
    /// The score date FIRST reported (`YYYY-MM-DD`), the latest across the returned rows.
    /// `None` when FIRST returned no rows (none of the CVEs has a published score yet).
    pub date: Option<String>,
    /// Every CVE id that was queried, so a CVE with no published score is known to have been
    /// checked rather than skipped.
    pub queried: BTreeSet<String>,
    /// Upper-cased CVE id to its score.
    pub scores: BTreeMap<String, EpssScore>,
}

/// A feed as one run sees it: checked (live, or from a fresh cache) or plainly not checked.
#[derive(Debug, Clone, PartialEq)]
pub enum FeedState<T> {
    Checked {
        data: T,
        /// `Some(date)` when the data came from the cache because the live fetch failed: the
        /// date (`YYYY-MM-DD`) the cached copy was fetched.
        cached_on: Option<String>,
    },
    NotChecked {
        reason: String,
    },
}

/// The cache record written next to each feed: the parsed data and when it was fetched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cached<T> {
    /// RFC3339 instant the live fetch succeeded.
    pub fetched_at: String,
    pub data: T,
}

/// The reason printed when a feed could not be fetched and no usable cache exists.
pub const REASON_UNAVAILABLE: &str = "the feed could not be fetched";
/// The reason printed when a feed could not be fetched and the cache is too old to use.
pub fn reason_stale() -> String {
    format!(
        "the feed could not be fetched and the cached copy is older than {MAX_CACHE_AGE_DAYS} days"
    )
}

/// The `YYYY-MM-DD` prefix of an ISO date or RFC3339 instant, when it is a real calendar date.
fn iso_date(raw: &str) -> Option<String> {
    let head = raw.trim().get(..10)?;
    NaiveDate::parse_from_str(head, "%Y-%m-%d").ok()?;
    Some(head.to_string())
}

/// Parse the CISA KEV JSON feed. A feed without a parseable `dateReleased` or without a
/// `vulnerabilities` array is an error: a catalog we cannot date is never presented as current.
pub fn parse_kev(json: &str) -> anyhow::Result<KevFeed> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    let date_released = v
        .get("dateReleased")
        .and_then(|d| d.as_str())
        .and_then(iso_date)
        .ok_or_else(|| anyhow::anyhow!("KEV feed has no parseable dateReleased"))?;
    let vulns = v
        .get("vulnerabilities")
        .and_then(|a| a.as_array())
        .ok_or_else(|| anyhow::anyhow!("KEV feed has no vulnerabilities array"))?;
    let entries = vulns
        .iter()
        .filter_map(|e| {
            let cve = e.get("cveID")?.as_str()?.trim().to_ascii_uppercase();
            if cve.is_empty() {
                return None;
            }
            let added = e
                .get("dateAdded")
                .and_then(|d| d.as_str())
                .and_then(iso_date)
                .unwrap_or_default();
            Some((cve, added))
        })
        .collect();
    Ok(KevFeed {
        date_released,
        entries,
    })
}

/// A number FIRST may publish as a JSON string (`"0.944010000"`) or a JSON number.
fn flexible_f64(v: Option<&serde_json::Value>) -> Option<f64> {
    match v? {
        serde_json::Value::String(s) => s.trim().parse().ok(),
        serde_json::Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// Parse one page of the EPSS API response (`{"status": "OK", "data": [{"cve", "epss",
/// "percentile", "date"}]}`) into `(scores, latest date)`. A response that is not `OK` or has no
/// `data` array is an error.
pub fn parse_epss_page(
    json: &str,
) -> anyhow::Result<(BTreeMap<String, EpssScore>, Option<String>)> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    if let Some(status) = v.get("status").and_then(|s| s.as_str()) {
        if !status.eq_ignore_ascii_case("ok") {
            anyhow::bail!("EPSS API returned status {status}");
        }
    }
    let rows = v
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| anyhow::anyhow!("EPSS response has no data array"))?;
    let mut scores = BTreeMap::new();
    let mut latest: Option<String> = None;
    for row in rows {
        let Some(cve) = row.get("cve").and_then(|c| c.as_str()) else {
            continue;
        };
        let (Some(epss), Some(percentile)) = (
            flexible_f64(row.get("epss")),
            flexible_f64(row.get("percentile")),
        ) else {
            continue;
        };
        if let Some(date) = row.get("date").and_then(|d| d.as_str()).and_then(iso_date) {
            if latest.as_ref().is_none_or(|l| date > *l) {
                latest = Some(date);
            }
        }
        scores.insert(
            cve.trim().to_ascii_uppercase(),
            EpssScore { epss, percentile },
        );
    }
    Ok((scores, latest))
}

/// The cache file's age check: usable only when it was fetched within [`MAX_CACHE_AGE_DAYS`]
/// of `now` (and not in the future, which only a broken clock produces).
fn cache_is_fresh(fetched_at: &str, now: DateTime<Utc>) -> bool {
    let Ok(at) = DateTime::parse_from_rfc3339(fetched_at) else {
        return false;
    };
    let age = now.signed_duration_since(at.with_timezone(&Utc));
    age >= chrono::Duration::zero() && age <= chrono::Duration::days(MAX_CACHE_AGE_DAYS)
}

/// Read a cache record, `None` when absent or unreadable (an unreadable cache is no cache).
pub fn read_cache<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<Cached<T>> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Write a cache record. A failure is logged and otherwise ignored: the run already has the
/// live data, and a missing cache only matters on a later run whose fetch also fails.
pub fn write_cache<T: Serialize>(path: &Path, data: &T, now: DateTime<Utc>) {
    let record = Cached {
        fetched_at: now.to_rfc3339(),
        data,
    };
    let result = path
        .parent()
        .map(std::fs::create_dir_all)
        .unwrap_or(Ok(()))
        .and_then(|_| {
            let bytes = serde_json::to_vec(&record).map_err(std::io::Error::other)?;
            std::fs::write(path, bytes)
        });
    if let Err(e) = result {
        tracing::warn!(path = %path.display(), "could not write vulnerability feed cache: {e}");
    }
}

/// Combine a live fetch with the cache: a successful fetch is used (and cached); a failed one
/// falls back to a cache fetched within [`MAX_CACHE_AGE_DAYS`] that `usable` accepts; otherwise
/// the feed is not checked. `cache_path` is `None` when no data dir resolves (no caching at all).
pub fn resolve<T: Clone + Serialize + for<'de> Deserialize<'de>>(
    fetched: anyhow::Result<T>,
    cache_path: Option<&Path>,
    now: DateTime<Utc>,
    usable: impl Fn(&T) -> bool,
) -> FeedState<T> {
    match fetched {
        Ok(data) => {
            if let Some(path) = cache_path {
                write_cache(path, &data, now);
            }
            FeedState::Checked {
                data,
                cached_on: None,
            }
        }
        Err(e) => {
            tracing::warn!("vulnerability feed fetch failed: {e:#}");
            let Some(cached) = cache_path.and_then(read_cache::<T>) else {
                return FeedState::NotChecked {
                    reason: REASON_UNAVAILABLE.to_string(),
                };
            };
            if !cache_is_fresh(&cached.fetched_at, now) {
                return FeedState::NotChecked {
                    reason: reason_stale(),
                };
            }
            if !usable(&cached.data) {
                return FeedState::NotChecked {
                    reason: REASON_UNAVAILABLE.to_string(),
                };
            }
            FeedState::Checked {
                cached_on: iso_date(&cached.fetched_at),
                data: cached.data,
            }
        }
    }
}
