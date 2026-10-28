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

/// The fewest usable rows a KEV catalog may yield and still be believed. The real catalog has
/// well over a thousand entries, so a body that parses to fewer than this is a schema change or
/// a truncated download, never "the catalog is empty". It is treated as a failed fetch (cache or
/// not checked), because an empty "checked" catalog would stamp KEV as current while labelling
/// nothing. One constant: the parser and the cache both use it.
pub const KEV_MIN_ENTRIES: usize = 100;

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
/// So is one that yields fewer than [`KEV_MIN_ENTRIES`] usable rows (a non-empty array whose rows
/// no longer parse, or a truncated body): the caller then falls back to the cache or reports the
/// feed as not checked, never an empty catalog marked as checked.
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
    let entries: BTreeMap<String, String> = vulns
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
    if entries.len() < KEV_MIN_ENTRIES {
        anyhow::bail!(
            "KEV feed yielded {} usable entries from {} rows; fewer than {KEV_MIN_ENTRIES} means \
             a schema change or a truncated body, so it is not trusted",
            entries.len(),
            vulns.len()
        );
    }
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
/// "percentile", "date"}]}`) into `(scores, latest date)`. A response that is not `OK`, has no
/// `data` array, or has rows none of which parse (a schema change) is an error. A genuinely empty
/// `data` array is not: it is FIRST saying none of the CVEs has a published score.
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
    if !rows.is_empty() && scores.is_empty() {
        anyhow::bail!(
            "EPSS response carried {} rows but none had a usable cve, epss and percentile",
            rows.len()
        );
    }
    Ok((scores, latest))
}

/// Whether an instant fetched at `fetched_at` is usable at `now`: within [`MAX_CACHE_AGE_DAYS`]
/// (and not in the future, which only a broken clock produces).
fn cache_is_fresh(fetched_at: &str, now: DateTime<Utc>) -> bool {
    let Ok(at) = DateTime::parse_from_rfc3339(fetched_at) else {
        return false;
    };
    let age = now.signed_duration_since(at.with_timezone(&Utc));
    age >= chrono::Duration::zero() && age <= chrono::Duration::days(MAX_CACHE_AGE_DAYS)
}

/// The UTC calendar date (`YYYY-MM-DD`) of an RFC3339 instant. Every stamp date that comes from
/// a fetch time is a UTC date, whatever offset the instant was written with.
fn utc_date(fetched_at: &str) -> Option<String> {
    let at = DateTime::parse_from_rfc3339(fetched_at).ok()?;
    Some(at.with_timezone(&Utc).format("%Y-%m-%d").to_string())
}

/// Read a cache record, `None` when absent or unreadable (an unreadable cache is no cache).
pub fn read_cache<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<Cached<T>> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Write a cache record atomically: the bytes go to a temp file in the same directory, then are
/// renamed over `path`, so a crash or a concurrent run never leaves a half-written cache that
/// the next run would read as garbage. A failure is logged and otherwise ignored: the run
/// already has the live data, and a missing cache only matters on a later run whose fetch also
/// fails.
pub fn write_cache<T: Serialize>(path: &Path, data: &T, now: DateTime<Utc>) {
    let record = Cached {
        fetched_at: now.to_rfc3339(),
        data,
    };
    let result = (|| -> std::io::Result<()> {
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(dir)?;
        let bytes = serde_json::to_vec(&record).map_err(std::io::Error::other)?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
        let written = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
        if written.is_err() {
            // Best effort: do not leave the temp file behind on a failed write or rename.
            let _ = std::fs::remove_file(&tmp);
        }
        written
    })();
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
                cached_on: utc_date(&cached.fetched_at),
                data: cached.data,
            }
        }
    }
}

// ─── the EPSS cache ───────────────────────────────────────────────────────────

/// What the cache remembers about one CVE: when FIRST was asked, and what it answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EpssCacheEntry {
    /// RFC3339 instant FIRST was asked about this CVE.
    pub fetched_at: String,
    /// The score date FIRST reported; `None` when it published no score for the CVE.
    pub date: Option<String>,
    /// `None` records "asked, FIRST had no score", which is an answer worth keeping.
    pub score: Option<EpssScore>,
}

/// The EPSS cache file: per CVE, so runs for different projects ADD to it instead of replacing
/// one another. Each entry ages on its own clock, so merging a new run in never makes an older
/// score look fresher than it is.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EpssCache {
    pub entries: BTreeMap<String, EpssCacheEntry>,
}

/// The cache after a successful live fetch: the entries still within [`MAX_CACHE_AGE_DAYS`] from
/// `existing` (queried by earlier runs, possibly for other projects), overlaid with every CVE
/// this run asked about.
pub fn merge_epss_cache(
    existing: Option<EpssCache>,
    batch: &EpssBatch,
    now: DateTime<Utc>,
) -> EpssCache {
    let mut cache = existing.unwrap_or_default();
    cache
        .entries
        .retain(|_, entry| cache_is_fresh(&entry.fetched_at, now));
    let fetched_at = now.to_rfc3339();
    for cve in &batch.queried {
        let score = batch.scores.get(cve).copied();
        cache.entries.insert(
            cve.clone(),
            EpssCacheEntry {
                fetched_at: fetched_at.clone(),
                date: score.and(batch.date.clone()),
                score,
            },
        );
    }
    cache
}

/// [`resolve`] for EPSS. A live fetch is used and MERGED into the cache; a failed fetch falls
/// back to the cache only when it holds a fresh entry for every CVE in `needed`. The returned
/// `cached_on` is the UTC date of the OLDEST entry used, so the stamp never overstates freshness.
pub fn resolve_epss(
    fetched: anyhow::Result<EpssBatch>,
    cache_path: Option<&Path>,
    now: DateTime<Utc>,
    needed: &BTreeSet<String>,
) -> FeedState<EpssBatch> {
    match fetched {
        Ok(batch) => {
            if let Some(path) = cache_path {
                let existing = read_cache::<EpssCache>(path).map(|c| c.data);
                write_cache(path, &merge_epss_cache(existing, &batch, now), now);
            }
            FeedState::Checked {
                data: batch,
                cached_on: None,
            }
        }
        Err(e) => {
            tracing::warn!("vulnerability feed fetch failed: {e:#}");
            let unavailable = || FeedState::NotChecked {
                reason: REASON_UNAVAILABLE.to_string(),
            };
            let Some(cache) = cache_path.and_then(read_cache::<EpssCache>) else {
                return unavailable();
            };
            let mut entries = Vec::with_capacity(needed.len());
            for cve in needed {
                match cache.data.entries.get(cve) {
                    Some(entry) => entries.push((cve, entry)),
                    // The cache does not cover this run's CVEs, so it cannot stand in.
                    None => return unavailable(),
                }
            }
            if entries
                .iter()
                .any(|(_, entry)| !cache_is_fresh(&entry.fetched_at, now))
            {
                return FeedState::NotChecked {
                    reason: reason_stale(),
                };
            }
            let cached_on = entries
                .iter()
                .filter_map(|(_, entry)| utc_date(&entry.fetched_at))
                .min();
            let date = entries
                .iter()
                .filter_map(|(_, entry)| entry.date.clone())
                .max();
            let scores = entries
                .iter()
                .filter_map(|(cve, entry)| entry.score.map(|s| ((*cve).clone(), s)))
                .collect();
            FeedState::Checked {
                data: EpssBatch {
                    date,
                    queried: needed.clone(),
                    scores,
                },
                cached_on,
            }
        }
    }
}
