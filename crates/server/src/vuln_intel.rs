//! Vulnerability intelligence for dependency findings: CISA KEV ("known exploited") and FIRST.org
//! EPSS (exploit probability), plus the provenance stamp that says how current each feed was.
//!
//! This is the ONE implementation of the enrichment. `onboard_audit`, `onboard_audit_start` (the
//! desktop app's scan paths) and `camerata inspect` (the CLI) each run the osv-scanner pass and
//! then call [`enrich_dependency_findings`] once over the rows that pass added.
//!
//! # What it does
//!
//! - **KEV**: a dependency finding whose CVE (the OSV id itself, or any alias of it, so a GHSA or
//!   RUSTSEC advisory matches through its CVE alias) is in the KEV catalog is labelled known
//!   exploited and its severity is floored to `critical`, the top of the scale.
//! - **EPSS**: the finding shows the exploit-probability score and percentile of its CVE (the
//!   highest-scoring one when the advisory has several).
//! - **Stamp**: [`VulnDataStamp`] records, per feed (KEV, EPSS, OSV, end-of-life, NVD), the date the data was
//!   current as of, or plainly that the feed was not checked and why. It rides on
//!   `ScanProvenance::vulnerability_data` into the export's README and `manifest.json`.
//!
//! # Feed rules
//!
//! Deterministic feeds only (two fixed URLs, no scraping). Each feed is fetched once per run and
//! cached with its date. When a fetch fails the run continues: a cache fetched within
//! [`MAX_CACHE_AGE_DAYS`] is used and the stamp says it is a cached copy; otherwise no finding is
//! labelled from that feed and the stamp says the feed was not checked. A stale feed is never
//! presented as current, and a missing one is never silently omitted.
//!
//! A body that downloads but does not parse into a believable feed (a KEV catalog with fewer than
//! [`feeds::KEV_MIN_ENTRIES`] usable rows, an EPSS page whose rows all fail to parse) counts as a
//! failed fetch, never as an empty feed that was "checked".
//!
//! The two fetches run concurrently under one time budget, [`ENRICHMENT_BUDGET`]. A feed that has
//! not answered by then is a failed fetch, so a scan (and the live view of a background audit job,
//! which receives the dependency rows only after enrichment) is never held longer than that.
//!
//! The EPSS cache accumulates per CVE across runs (see [`feeds::EpssCache`]), so a run for
//! another project can use scores an earlier run already fetched. Every date the cache puts in a
//! stamp is a UTC date, and the stamp text says so.

pub(crate) mod feeds;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::dep_audit::DEP_AUDIT_RULE_ID;
use crate::onboard::{CoverageNote, Finding, ScanReport};

pub use feeds::{EpssBatch, EpssScore, FeedState, KevFeed, KEV_MIN_ENTRIES};

/// A cached feed older than this many days counts as not checked. The one staleness constant.
pub const MAX_CACHE_AGE_DAYS: i64 = 7;

/// The one time budget for the whole enrichment's network phase: both feeds, fetched
/// concurrently, must answer within this long or be treated as failed fetches (cache, or not
/// checked). It is longer than one request's own 30 s timeout so a slow-but-working feed is not
/// cut short, and short enough that a scan is never held up for a minute.
pub const ENRICHMENT_BUDGET: Duration = Duration::from_secs(45);

/// The CISA Known Exploited Vulnerabilities catalog (JSON).
pub const KEV_URL: &str =
    "https://www.cisa.gov/sites/default/files/feeds/known_exploited_vulnerabilities.json";

/// The FIRST.org EPSS API, queried by CVE (`?cve=CVE-a,CVE-b`).
pub const EPSS_URL: &str = "https://api.first.org/data/v1/epss";

/// How many CVEs one EPSS request carries (the API's default page size).
pub const EPSS_BATCH: usize = 100;

/// Capture set by `dep_audit::parse_osv_json`: the advisory's CVE ids (its own id when it is a
/// CVE, plus every CVE alias), comma-separated and upper-cased. Enrichment matches on these.
pub const CVE_IDS_CAPTURE: &str = "osv_cve_ids";
/// Capture set on a KEV hit: the CVE that is on the catalog.
pub const KNOWN_EXPLOITED_CAPTURE: &str = "vuln_known_exploited";
/// Capture set on a KEV hit when the catalog carries the date the CVE was added.
pub const KEV_DATE_ADDED_CAPTURE: &str = "vuln_kev_date_added";
/// Captures set when an EPSS score is attached.
pub const EPSS_CVE_CAPTURE: &str = "vuln_epss_cve";
pub const EPSS_SCORE_CAPTURE: &str = "vuln_epss_score";
pub const EPSS_PERCENTILE_CAPTURE: &str = "vuln_epss_percentile";

/// The coverage-note tool name for a vulnerability feed that was not checked.
pub const COVERAGE_TOOL: &str = "vulnerability-intel";

/// Why KEV/EPSS were not checked when osv-scanner ran in no repo.
const REASON_AUDIT_DID_NOT_RUN: &str = "the dependency audit did not run";
/// Why EPSS was not queried when no dependency finding has a CVE id.
const REASON_NO_CVES: &str = "no dependency finding carries a CVE identifier";
/// Why every feed reads not checked on a report that carries no stamp (dependency audit not
/// requested, or a report persisted before stamps existed).
const REASON_NOT_RECORDED: &str = "no vulnerability data was recorded for this inspection";
/// Why every feed reads not checked when the dependency audit is disabled for test isolation.
const REASON_DISABLED: &str = "the dependency audit was disabled for this run";

// ─── the stamp ────────────────────────────────────────────────────────────────

/// One feed's line in the provenance stamp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FeedStamp {
    /// The feed was consulted; its data was current as of `as_of` (`YYYY-MM-DD`). `note`
    /// qualifies it (a cached copy, a repo osv-scanner could not scan, ...).
    Checked {
        as_of: String,
        #[serde(default)]
        note: Option<String>,
    },
    /// The feed was not consulted, so nothing was labelled from it.
    NotChecked { reason: String },
}

impl FeedStamp {
    fn not_checked(reason: &str) -> Self {
        Self::NotChecked {
            reason: reason.to_string(),
        }
    }

    /// The text after the feed's label: `as of 2026-10-27`, `as of 2026-10-27 (cached copy ...)`
    /// or `not checked (the feed could not be fetched)`.
    pub fn text(&self) -> String {
        match self {
            Self::Checked { as_of, note: None } => format!("as of {as_of}"),
            Self::Checked {
                as_of,
                note: Some(note),
            } => format!("as of {as_of} ({note})"),
            Self::NotChecked { reason } => format!("not checked ({reason})"),
        }
    }
}

/// "Vulnerability data as of": the per-feed provenance of a run's dependency findings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VulnDataStamp {
    pub cisa_kev: FeedStamp,
    pub first_epss: FeedStamp,
    pub osv: FeedStamp,
    /// endoflife.date, for the runtime and framework end-of-life findings
    /// (see [`crate::runtime_audit`]). A stamp written before this feed existed reads as not
    /// checked.
    #[serde(default = "not_recorded")]
    pub endoflife: FeedStamp,
    /// The NVD CVE API, for the runtime vulnerability findings.
    #[serde(default = "not_recorded")]
    pub nvd: FeedStamp,
}

fn not_recorded() -> FeedStamp {
    FeedStamp::not_checked(REASON_NOT_RECORDED)
}

impl VulnDataStamp {
    pub(crate) fn all_not_checked(reason: &str) -> Self {
        Self {
            cisa_kev: FeedStamp::not_checked(reason),
            first_epss: FeedStamp::not_checked(reason),
            osv: FeedStamp::not_checked(reason),
            endoflife: FeedStamp::not_checked(reason),
            nvd: FeedStamp::not_checked(reason),
        }
    }
}

/// The stamp's feeds in their fixed order: `(manifest key, human label, stamp)`. A report
/// with no stamp reads as every feed not checked, never as an omitted line.
pub fn stamp_entries(
    stamp: Option<&VulnDataStamp>,
) -> [(&'static str, &'static str, FeedStamp); 5] {
    let stamp = stamp
        .cloned()
        .unwrap_or_else(|| VulnDataStamp::all_not_checked(REASON_NOT_RECORDED));
    [
        ("cisa_kev", "CISA KEV", stamp.cisa_kev),
        ("first_epss", "FIRST EPSS", stamp.first_epss),
        ("osv", "OSV", stamp.osv),
        (
            "end_of_life",
            "End-of-life, endoflife.date",
            stamp.endoflife,
        ),
        ("nvd", "NVD, runtimes", stamp.nvd),
    ]
}

/// The README line prefix every stamp line starts with (the release gate parses it back).
pub const README_PREFIX: &str = "Vulnerability data (";

/// One README stamp line, e.g. `Vulnerability data (CISA KEV) as of 2026-10-27`.
pub fn readme_line(label: &str, stamp: &FeedStamp) -> String {
    format!("{README_PREFIX}{label}) {}", stamp.text())
}

/// The README's stamp block: one line per feed, each ending with a newline.
pub fn readme_lines(stamp: Option<&VulnDataStamp>) -> String {
    stamp_entries(stamp)
        .iter()
        .map(|(_, label, s)| format!("{}\n", readme_line(label, s)))
        .collect()
}

/// The manifest's `inspection.vulnerability_data` object: per feed its `status`, `as_of` (null
/// when not checked) and `stamp`, the exact README line, so the two cannot disagree.
pub fn manifest_json(stamp: Option<&VulnDataStamp>) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for (key, label, s) in stamp_entries(stamp) {
        let (status, as_of) = match &s {
            FeedStamp::Checked { as_of, .. } => ("checked", Some(as_of.clone())),
            FeedStamp::NotChecked { .. } => ("not_checked", None),
        };
        out.insert(
            key.to_string(),
            serde_json::json!({
                "status": status,
                "as_of": as_of,
                "stamp": readme_line(label, &s),
            }),
        );
    }
    serde_json::Value::Object(out)
}

// ─── the network seam ─────────────────────────────────────────────────────────

/// Fetches a feed URL's body. Production uses [`HttpFetcher`]; tests supply fixtures.
#[async_trait::async_trait]
pub trait FeedFetcher: Send + Sync {
    async fn get(&self, url: &str) -> anyhow::Result<String>;
}

/// The production fetcher: plain HTTPS GET with hard timeouts, so an unreachable feed fails fast
/// and the scan continues.
pub struct HttpFetcher {
    client: reqwest::Client,
}

impl HttpFetcher {
    pub fn new() -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("camerata/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self { client })
    }
}

/// A feed answered with a non-success HTTP status. Kept as its own error type so a caller can tell
/// a rate limit (403, 429 or 503, often with `Retry-After`) from an unreachable host and back off.
#[derive(Debug)]
pub struct FetchStatusError {
    pub status: u16,
    /// The `Retry-After` header in seconds, when the feed sent one.
    pub retry_after_secs: Option<u64>,
}

impl std::fmt::Display for FetchStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the feed answered with HTTP status {}", self.status)
    }
}

impl std::error::Error for FetchStatusError {}

#[async_trait::async_trait]
impl FeedFetcher for HttpFetcher {
    async fn get(&self, url: &str) -> anyhow::Result<String> {
        let resp = self.client.get(url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let retry_after_secs = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse().ok());
            return Err(FetchStatusError {
                status: status.as_u16(),
                retry_after_secs,
            }
            .into());
        }
        Ok(resp.text().await?)
    }
}

// ─── the osv-scanner side ─────────────────────────────────────────────────────

/// Whether osv-scanner actually scanned one repo this run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsvRun {
    pub repo: String,
    pub ran: bool,
}

impl OsvRun {
    /// Classify one repo's dep-audit outcome: no coverage note, or the "found no lockfiles" note,
    /// means osv-scanner scanned it; any other note means it did not run.
    pub fn from_note(repo: &str, note: Option<&CoverageNote>) -> Self {
        Self {
            repo: repo.to_string(),
            ran: note.is_none_or(crate::dep_audit::note_means_scanned),
        }
    }
}

fn osv_stamp(runs: &[OsvRun], now: DateTime<Utc>) -> FeedStamp {
    if !runs.iter().any(|r| r.ran) {
        return FeedStamp::not_checked(REASON_AUDIT_DID_NOT_RUN);
    }
    let missed: Vec<&str> = runs
        .iter()
        .filter(|r| !r.ran)
        .map(|r| r.repo.as_str())
        .collect();
    FeedStamp::Checked {
        as_of: now.format("%Y-%m-%d").to_string(),
        note: (!missed.is_empty()).then(|| format!("not checked for {}", missed.join(", "))),
    }
}

// ─── enrichment ───────────────────────────────────────────────────────────────

/// The CVE ids a dependency finding carries (see [`CVE_IDS_CAPTURE`]).
fn finding_cves(f: &Finding) -> Vec<String> {
    f.captures
        .get(CVE_IDS_CAPTURE)
        .map(|ids| {
            ids.split(',')
                .map(|s| s.trim().to_ascii_uppercase())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// A finding KEV and EPSS apply to: a vulnerable dependency, or a vulnerable runtime (both carry
/// their CVE ids under [`CVE_IDS_CAPTURE`]).
fn is_dependency_finding(f: &Finding) -> bool {
    f.rule_id == DEP_AUDIT_RULE_ID || f.rule_id == crate::runtime_audit::VULN_RULE_ID
}

async fn fetch_kev(fetcher: &dyn FeedFetcher) -> anyhow::Result<KevFeed> {
    feeds::parse_kev(&fetcher.get(KEV_URL).await?)
}

async fn fetch_epss(
    fetcher: &dyn FeedFetcher,
    cves: &BTreeSet<String>,
) -> anyhow::Result<EpssBatch> {
    let all: Vec<&str> = cves.iter().map(String::as_str).collect();
    let mut batch = EpssBatch {
        date: None,
        queried: cves.clone(),
        scores: Default::default(),
    };
    for chunk in all.chunks(EPSS_BATCH) {
        let url = format!("{EPSS_URL}?cve={}&limit={EPSS_BATCH}", chunk.join(","));
        let (scores, date) = feeds::parse_epss_page(&fetcher.get(&url).await?)?;
        batch.scores.extend(scores);
        if date > batch.date {
            batch.date = date;
        }
    }
    Ok(batch)
}

/// The stamp note for a cached copy. The date is the UTC date the copy was fetched.
fn cached_note(cached_on: &Option<String>) -> Option<String> {
    cached_on
        .as_ref()
        .map(|d| format!("cached copy fetched {d} UTC; the live feed could not be fetched"))
}

/// The stamp note when FIRST was asked and returned no score at all. It says what happened
/// (nothing was published as of the request date) rather than dating data that never came back.
const NOTE_NO_SCORES: &str = "no scores were published for these CVEs as of the request date (UTC)";

/// The stamp note when FIRST returned scores but none of them carried a score date.
const NOTE_NO_SCORE_DATE: &str =
    "FIRST gave no score date; the date shown is the request date (UTC)";

/// The EPSS line of the stamp. `as_of` is FIRST's own score date whenever it gave one; when it
/// gave none (no rows, or rows without a date) it is the request date, and a note says so.
fn epss_stamp(data: &EpssBatch, cached_on: &Option<String>, now: DateTime<Utc>) -> FeedStamp {
    let request_date = cached_on
        .clone()
        .unwrap_or_else(|| now.format("%Y-%m-%d").to_string());
    let mut notes: Vec<String> = cached_note(cached_on).into_iter().collect();
    let as_of = match &data.date {
        Some(date) => date.clone(),
        None => {
            notes.push(
                if data.scores.is_empty() {
                    NOTE_NO_SCORES
                } else {
                    NOTE_NO_SCORE_DATE
                }
                .to_string(),
            );
            request_date
        }
    };
    FeedStamp::Checked {
        as_of,
        note: (!notes.is_empty()).then(|| notes.join("; ")),
    }
}

/// Run `fetch` under `budget`; running out of time is an ordinary failed fetch.
pub(crate) async fn within_budget<T>(
    budget: Duration,
    fetch: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    match tokio::time::timeout(budget, fetch).await {
        Ok(result) => result,
        Err(_elapsed) => anyhow::bail!("no answer within {} s", budget.as_secs_f64()),
    }
}

/// Apply checked feeds to the dependency findings: label and floor KEV hits, attach EPSS.
///
/// ORDER: this runs BEFORE `severity_bounds::apply_scan_severity_bounds` at every call site, and
/// the bounds have the last word on severity (a `max_severity` ceiling lowers whatever it is
/// given). The "Severity raised from X to critical" sentence below is only true because no rule
/// that governs `DEP-AUDIT-1` findings declares a ceiling. If one ever does, the sentence would
/// contradict the severity the report shows. The tests
/// `no_shipped_ceiling_governs_dependency_findings` and
/// `a_dependency_ceiling_would_overrule_the_raise_so_the_text_depends_on_there_being_none` (and
/// `tests/vuln_intel_call_order.rs` for the order itself) pin that, so the change has to
/// reconcile the text instead of silently shipping a false claim.
fn apply(findings: &mut [Finding], kev: &FeedState<KevFeed>, epss: &FeedState<EpssBatch>) {
    for f in findings.iter_mut().filter(|f| is_dependency_finding(f)) {
        let cves = finding_cves(f);
        if cves.is_empty() {
            continue;
        }
        let mut suffix: Vec<String> = Vec::new();
        if let FeedState::Checked { data, .. } = kev {
            if let Some((cve, added)) = cves
                .iter()
                .find_map(|c| data.entries.get(c).map(|added| (c, added)))
            {
                f.captures
                    .insert(KNOWN_EXPLOITED_CAPTURE.to_string(), cve.clone());
                let since = if added.is_empty() {
                    String::new()
                } else {
                    f.captures
                        .insert(KEV_DATE_ADDED_CAPTURE.to_string(), added.clone());
                    format!(" (added {added})")
                };
                let mut line =
                    format!("Known exploited: {cve} is listed in the CISA KEV catalog{since}.");
                if f.severity != "critical" {
                    line.push_str(&format!(
                        " Severity raised from {} to critical.",
                        f.severity
                    ));
                    f.severity = "critical".to_string();
                }
                suffix.push(line);
            }
        }
        if let FeedState::Checked { data, .. } = epss {
            let best = cves
                .iter()
                .filter_map(|c| data.scores.get(c).map(|s| (c, *s)))
                .max_by(|a, b| a.1.epss.total_cmp(&b.1.epss));
            if let Some((cve, score)) = best {
                f.captures.insert(EPSS_CVE_CAPTURE.to_string(), cve.clone());
                f.captures
                    .insert(EPSS_SCORE_CAPTURE.to_string(), format!("{:.5}", score.epss));
                f.captures.insert(
                    EPSS_PERCENTILE_CAPTURE.to_string(),
                    format!("{:.5}", score.percentile),
                );
                suffix.push(format!(
                    "EPSS ({cve}): {:.2}% probability of exploitation in the next 30 days, \
                     percentile {:.2}.",
                    score.epss * 100.0,
                    score.percentile * 100.0
                ));
            }
        }
        if !suffix.is_empty() {
            let sep = if f.detail.ends_with('.') { " " } else { ". " };
            f.detail = format!("{}{sep}{}", f.detail, suffix.join(" "));
        }
    }
}

/// A coverage note for a feed that could not be consulted (shown in the cockpit's scan
/// coverage section). `None` for a feed that was checked or simply had nothing to look up.
fn unavailable_note(label: &str, state_reason: Option<&str>, effect: &str) -> Option<CoverageNote> {
    let reason = state_reason?;
    Some(CoverageNote {
        tool_failed: false,
        repo: None,
        tool: COVERAGE_TOOL.to_string(),
        message: format!("{label} not checked ({reason}); {effect}"),
    })
}

/// The testable core of [`enrich_dependency_findings`]: enrich `findings` from the feeds behind
/// `fetcher` (cached under `cache_dir`, when one resolves) and return the stamp plus a coverage
/// note per feed that could not be consulted. Both feeds run under [`ENRICHMENT_BUDGET`].
pub async fn enrich_with(
    findings: &mut [Finding],
    osv_runs: &[OsvRun],
    fetcher: &dyn FeedFetcher,
    cache_dir: Option<&Path>,
    now: DateTime<Utc>,
) -> (VulnDataStamp, Vec<CoverageNote>) {
    enrich_with_budget(
        findings,
        osv_runs,
        fetcher,
        cache_dir,
        now,
        ENRICHMENT_BUDGET,
    )
    .await
}

/// [`enrich_with`] with the time budget as a parameter, so a test can exercise the timeout path
/// without waiting 45 s.
async fn enrich_with_budget(
    findings: &mut [Finding],
    osv_runs: &[OsvRun],
    fetcher: &dyn FeedFetcher,
    cache_dir: Option<&Path>,
    now: DateTime<Utc>,
    budget: Duration,
) -> (VulnDataStamp, Vec<CoverageNote>) {
    let osv = osv_stamp(osv_runs, now);
    let osv_ran = !matches!(osv, FeedStamp::NotChecked { .. });
    // A runtime vulnerability is looked up whether or not osv-scanner ran; a dependency finding
    // only counts when it did (without a scan there are none to look up).
    let cves: BTreeSet<String> = findings
        .iter()
        .filter(|f| is_dependency_finding(f))
        .filter(|f| osv_ran || f.rule_id == crate::runtime_audit::VULN_RULE_ID)
        .flat_map(finding_cves)
        .collect();
    if !osv_ran && cves.is_empty() {
        // Nothing was scanned (no dependency audit and no runtime vulnerability either), so
        // there is nothing to look up and no reason to fetch.
        return (
            VulnDataStamp::all_not_checked(REASON_AUDIT_DID_NOT_RUN),
            Vec::new(),
        );
    }

    // The two feeds do not depend on each other, so they are fetched at the same time, each under
    // the same budget (both start together, so it is one shared deadline).
    let (kev_fetched, epss_fetched) =
        tokio::join!(within_budget(budget, fetch_kev(fetcher)), async {
            if cves.is_empty() {
                None
            } else {
                Some(within_budget(budget, fetch_epss(fetcher, &cves)).await)
            }
        });

    let kev_cache = cache_dir.map(|d| d.join("kev.json"));
    let kev = feeds::resolve(kev_fetched, kev_cache.as_deref(), now, |k: &KevFeed| {
        k.entries.len() >= KEV_MIN_ENTRIES
    });

    let epss = match epss_fetched {
        None => FeedState::NotChecked {
            reason: REASON_NO_CVES.to_string(),
        },
        Some(fetched) => {
            let epss_cache = cache_dir.map(|d| d.join("epss.json"));
            // A cached answer is usable only when it covers every CVE this run needs.
            feeds::resolve_epss(fetched, epss_cache.as_deref(), now, &cves)
        }
    };

    apply(findings, &kev, &epss);

    let cisa_kev = match &kev {
        FeedState::Checked { data, cached_on } => FeedStamp::Checked {
            as_of: data.date_released.clone(),
            note: cached_note(cached_on),
        },
        FeedState::NotChecked { reason } => FeedStamp::not_checked(reason),
    };
    let first_epss = match &epss {
        FeedState::Checked { data, cached_on } => epss_stamp(data, cached_on, now),
        FeedState::NotChecked { reason } => FeedStamp::not_checked(reason),
    };

    let mut notes = Vec::new();
    let kev_reason = match &kev {
        FeedState::NotChecked { reason } => Some(reason.as_str()),
        FeedState::Checked { .. } => None,
    };
    notes.extend(unavailable_note(
        "CISA KEV",
        kev_reason,
        "no dependency finding is labelled known exploited",
    ));
    let epss_reason = match &epss {
        FeedState::NotChecked { reason } if reason != REASON_NO_CVES => Some(reason.as_str()),
        _ => None,
    };
    notes.extend(unavailable_note(
        "FIRST EPSS",
        epss_reason,
        "no dependency finding shows an exploit probability",
    ));

    (
        VulnDataStamp {
            cisa_kev,
            first_epss,
            osv,
            // The runtime pass owns these two; `enrich_dependency_findings` carries them over.
            endoflife: not_recorded(),
            nvd: not_recorded(),
        },
        notes,
    )
}

/// Enrich the dependency findings this run added (`report.findings[first_dep_row..]`) with KEV
/// and EPSS, stamp `report.provenance.vulnerability_data`, and record a coverage note for any
/// feed that could not be consulted. Call ONCE per run, after the osv-scanner pass over every
/// repo, with each repo's [`OsvRun`].
///
/// With `CAMERATA_DISABLE_DEP_AUDIT` set (test isolation, see `dep_audit::DISABLE_ENV_VAR`) no
/// feed is fetched and the stamp says the dependency audit was disabled.
pub async fn enrich_dependency_findings(
    report: &mut ScanReport,
    first_dep_row: usize,
    osv_runs: &[OsvRun],
) {
    let disabled = std::env::var(crate::dep_audit::DISABLE_ENV_VAR)
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    // The runtime pass (`runtime_audit`) runs first and leaves its two feeds' stamps here.
    let runtime_stamps = report
        .provenance
        .vulnerability_data
        .take()
        .map(|s| (s.endoflife, s.nvd));
    let carry = |mut stamp: VulnDataStamp| {
        if let Some((endoflife, nvd)) = runtime_stamps.clone() {
            stamp.endoflife = endoflife;
            stamp.nvd = nvd;
        }
        stamp
    };
    if disabled {
        report.provenance.vulnerability_data =
            Some(carry(VulnDataStamp::all_not_checked(REASON_DISABLED)));
        return;
    }
    let first = first_dep_row.min(report.findings.len());
    let cache_dir = crate::app_dirs::camerata_dir().map(|d| d.join("vuln-intel"));
    let (stamp, notes) = match HttpFetcher::new() {
        Ok(fetcher) => {
            enrich_with(
                &mut report.findings[first..],
                osv_runs,
                &fetcher,
                cache_dir.as_deref(),
                Utc::now(),
            )
            .await
        }
        Err(e) => {
            tracing::warn!("could not build the vulnerability feed HTTP client: {e}");
            enrich_with(
                &mut report.findings[first..],
                osv_runs,
                &Unreachable,
                cache_dir.as_deref(),
                Utc::now(),
            )
            .await
        }
    };
    report.provenance.vulnerability_data = Some(carry(stamp));
    report.coverage_notes.extend(notes);
}

/// A fetcher that always fails, used when no HTTP client could be built: the run then falls back
/// to the cache exactly as it would on a network failure.
pub(crate) struct Unreachable;

#[async_trait::async_trait]
impl FeedFetcher for Unreachable {
    async fn get(&self, _url: &str) -> anyhow::Result<String> {
        anyhow::bail!("no HTTP client")
    }
}
