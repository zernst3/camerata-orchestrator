//! Network-free tests: every feed comes from the fixtures under
//! `crates/server/tests/fixtures/vuln_intel/` through [`FixtureFetcher`].

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Duration, TimeZone, Utc};

use super::*;

const KEV_FIXTURE_RAW: &str = include_str!("../../tests/fixtures/vuln_intel/kev.json");
const EPSS_FIXTURE: &str = include_str!("../../tests/fixtures/vuln_intel/epss.json");
const OSV_FIXTURE: &str = include_str!("../../tests/fixtures/vuln_intel/osv_scan.json");

/// The KEV fixture plus enough filler rows to clear the parser's sanity floor
/// ([`KEV_MIN_ENTRIES`]): the real catalog has over a thousand entries, and the parser refuses a
/// body that yields fewer. The two real fixture entries (`CVE-2021-44228`, `CVE-2014-0160`) stay.
fn kev_json() -> &'static str {
    static PADDED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PADDED.get_or_init(|| {
        let mut doc: serde_json::Value = serde_json::from_str(KEV_FIXTURE_RAW).unwrap();
        let rows = doc["vulnerabilities"].as_array_mut().unwrap();
        for i in 0..KEV_MIN_ENTRIES {
            rows.push(serde_json::json!({
                "cveID": format!("CVE-1999-{i:05}"),
                "dateAdded": "2000-01-01",
            }));
        }
        serde_json::to_string(&doc).unwrap()
    })
}

/// Serves the fixture feeds; a `None` feed fails like an unreachable host. Records every URL.
struct FixtureFetcher {
    kev: Option<&'static str>,
    epss: Option<&'static str>,
    calls: Mutex<Vec<String>>,
}

impl FixtureFetcher {
    fn new(kev: Option<&'static str>, epss: Option<&'static str>) -> Self {
        Self {
            kev,
            epss,
            calls: Mutex::new(Vec::new()),
        }
    }

    fn online() -> Self {
        Self::new(Some(kev_json()), Some(EPSS_FIXTURE))
    }

    fn offline() -> Self {
        Self::new(None, None)
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl FeedFetcher for FixtureFetcher {
    async fn get(&self, url: &str) -> anyhow::Result<String> {
        self.calls.lock().unwrap().push(url.to_string());
        let body = if url == KEV_URL {
            self.kev
        } else if url.starts_with(EPSS_URL) {
            self.epss
        } else {
            None
        };
        body.map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("fixture: {url} unreachable"))
    }
}

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 28, 12, 0, 0).unwrap()
}

fn scanned() -> Vec<OsvRun> {
    vec![OsvRun {
        repo: "acme/app".to_string(),
        ran: true,
    }]
}

/// The fixture osv-scanner output, parsed by the real parser: log4j (GHSA aliasing a KEV CVE,
/// MODERATE), semver (GHSA aliasing a CVE not on KEV, with an EPSS score), time (RUSTSEC, no
/// CVE at all).
fn osv_findings() -> Vec<Finding> {
    crate::dep_audit::parse_osv_json("acme/app", Path::new("/r"), OSV_FIXTURE).unwrap()
}

fn by_package<'a>(findings: &'a [Finding], package: &str) -> &'a Finding {
    findings
        .iter()
        .find(|f| f.snippet.starts_with(&format!("{package}@")))
        .unwrap_or_else(|| panic!("no finding for {package}"))
}

async fn enrich(
    findings: &mut [Finding],
    fetcher: &FixtureFetcher,
    cache: Option<&Path>,
) -> (VulnDataStamp, Vec<CoverageNote>) {
    enrich_with(findings, &scanned(), fetcher, cache, now()).await
}

#[tokio::test]
async fn kev_hit_floors_severity_to_critical_and_labels_known_exploited() {
    let mut findings = osv_findings();
    assert_eq!(by_package(&findings, "log4j-core").severity, "medium");

    let (stamp, notes) = enrich(&mut findings, &FixtureFetcher::online(), None).await;

    let log4j = by_package(&findings, "log4j-core");
    assert_eq!(
        log4j.severity, "critical",
        "a KEV hit is floored to the top severity"
    );
    assert_eq!(
        log4j
            .captures
            .get(KNOWN_EXPLOITED_CAPTURE)
            .map(String::as_str),
        Some("CVE-2021-44228")
    );
    assert_eq!(
        log4j
            .captures
            .get(KEV_DATE_ADDED_CAPTURE)
            .map(String::as_str),
        Some("2021-12-10")
    );
    assert!(
        log4j.detail.contains(
            "Known exploited: CVE-2021-44228 is listed in the CISA KEV catalog (added 2021-12-10). \
             Severity raised from medium to critical."
        ),
        "{}",
        log4j.detail
    );
    assert_eq!(
        stamp.cisa_kev,
        FeedStamp::Checked {
            as_of: "2026-10-27".to_string(),
            note: None
        }
    );
    assert!(notes.is_empty(), "every feed was checked: {notes:?}");
}

#[tokio::test]
async fn kev_hit_on_an_already_critical_finding_does_not_claim_a_raise() {
    let mut findings = osv_findings();
    findings
        .iter_mut()
        .find(|f| f.snippet.starts_with("log4j-core@"))
        .unwrap()
        .severity = "critical".to_string();
    enrich(&mut findings, &FixtureFetcher::online(), None).await;
    let log4j = by_package(&findings, "log4j-core");
    assert_eq!(log4j.severity, "critical");
    assert!(log4j.detail.contains("Known exploited"));
    assert!(
        !log4j.detail.contains("Severity raised"),
        "{}",
        log4j.detail
    );
}

#[tokio::test]
async fn kev_miss_leaves_the_finding_unchanged() {
    let mut findings = osv_findings();
    let before = by_package(&findings, "semver").clone();
    let time_before = by_package(&findings, "time").clone();

    // KEV live, EPSS down: semver's CVE is not on KEV, so nothing may change on it.
    let fetcher = FixtureFetcher::new(Some(kev_json()), None);
    let (stamp, _) = enrich(&mut findings, &fetcher, None).await;

    let after = by_package(&findings, "semver");
    assert_eq!(after.severity, before.severity);
    assert_eq!(after.detail, before.detail);
    assert_eq!(after.captures, before.captures);
    // A finding with no CVE at all is never touched either.
    let time_after = by_package(&findings, "time");
    assert_eq!(time_after.detail, time_before.detail);
    assert_eq!(time_after.severity, time_before.severity);
    assert!(matches!(stamp.cisa_kev, FeedStamp::Checked { .. }));
}

#[tokio::test]
async fn ghsa_advisory_matches_kev_through_its_cve_alias() {
    let findings = osv_findings();
    let log4j = by_package(&findings, "log4j-core");
    // The parser records the alias as a structured capture; the advisory id itself is a GHSA.
    assert!(log4j.detail.starts_with("GHSA-jfh8-c2jp-5v3q"));
    assert_eq!(
        log4j.captures.get(CVE_IDS_CAPTURE).map(String::as_str),
        Some("CVE-2021-44228")
    );
    assert!(!by_package(&findings, "time")
        .captures
        .contains_key(CVE_IDS_CAPTURE));

    let mut findings = findings;
    enrich(&mut findings, &FixtureFetcher::online(), None).await;
    assert!(by_package(&findings, "log4j-core")
        .captures
        .contains_key(KNOWN_EXPLOITED_CAPTURE));
}

#[tokio::test]
async fn epss_score_and_percentile_are_attached() {
    let mut findings = osv_findings();
    let fetcher = FixtureFetcher::online();
    let (stamp, _) = enrich(&mut findings, &fetcher, None).await;

    let semver = by_package(&findings, "semver");
    assert_eq!(
        semver.captures.get(EPSS_SCORE_CAPTURE).map(String::as_str),
        Some("0.00238")
    );
    assert_eq!(
        semver
            .captures
            .get(EPSS_PERCENTILE_CAPTURE)
            .map(String::as_str),
        Some("0.61109")
    );
    assert!(
        semver.detail.contains(
            "EPSS (CVE-2022-25883): 0.24% probability of exploitation in the next 30 days, \
             percentile 61.11."
        ),
        "{}",
        semver.detail
    );
    // EPSS only adds information: a non-KEV finding keeps its severity.
    assert_eq!(semver.severity, "medium");
    assert_eq!(
        stamp.first_epss,
        FeedStamp::Checked {
            as_of: "2026-10-27".to_string(),
            note: None
        }
    );
    // One batched EPSS request carrying both CVEs.
    let epss_calls: Vec<String> = fetcher
        .calls()
        .into_iter()
        .filter(|u| u.starts_with(EPSS_URL))
        .collect();
    assert_eq!(epss_calls.len(), 1, "{epss_calls:?}");
    assert!(epss_calls[0].contains("CVE-2021-44228,CVE-2022-25883"));
}

#[tokio::test]
async fn unavailable_feeds_stamp_not_checked_and_label_nothing() {
    let mut findings = osv_findings();
    let before: Vec<(String, String)> = findings
        .iter()
        .map(|f| (f.severity.clone(), f.detail.clone()))
        .collect();

    let (stamp, notes) = enrich(&mut findings, &FixtureFetcher::offline(), None).await;

    let after: Vec<(String, String)> = findings
        .iter()
        .map(|f| (f.severity.clone(), f.detail.clone()))
        .collect();
    assert_eq!(
        before, after,
        "no finding may be labelled from an unchecked feed"
    );
    assert!(findings
        .iter()
        .all(|f| !f.captures.contains_key(KNOWN_EXPLOITED_CAPTURE)
            && !f.captures.contains_key(EPSS_SCORE_CAPTURE)));

    let unavailable = FeedStamp::NotChecked {
        reason: "the feed could not be fetched".to_string(),
    };
    assert_eq!(stamp.cisa_kev, unavailable);
    assert_eq!(stamp.first_epss, unavailable);
    // OSV itself still ran.
    assert!(matches!(stamp.osv, FeedStamp::Checked { .. }));
    assert_eq!(
        readme_line("CISA KEV", &stamp.cisa_kev),
        "Vulnerability data (CISA KEV) not checked (the feed could not be fetched)"
    );
    // And the cockpit's coverage section says so too.
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(notes.iter().all(|n| n.tool == COVERAGE_TOOL));
}

fn write_cached<T: serde::Serialize>(dir: &Path, name: &str, data: &T, fetched: DateTime<Utc>) {
    let record = serde_json::json!({ "fetched_at": fetched.to_rfc3339(), "data": data });
    std::fs::write(dir.join(name), serde_json::to_vec(&record).unwrap()).unwrap();
}

/// Write the cached EPSS file the way a live run does: one entry per CVE, each with its own
/// fetch time.
fn write_epss_cache(
    dir: &Path,
    cves: &[&str],
    scores: &BTreeMap<String, EpssScore>,
    date: &str,
    fetched: DateTime<Utc>,
) {
    let entries = cves
        .iter()
        .map(|cve| {
            let score = scores.get(*cve).copied();
            (
                cve.to_string(),
                feeds::EpssCacheEntry {
                    fetched_at: fetched.to_rfc3339(),
                    date: score.map(|_| date.to_string()),
                    score,
                },
            )
        })
        .collect();
    write_cached(dir, "epss.json", &feeds::EpssCache { entries }, fetched);
}

fn cached_feeds(dir: &Path, fetched: DateTime<Utc>) {
    let kev = feeds::parse_kev(kev_json()).unwrap();
    write_cached(dir, "kev.json", &kev, fetched);
    let (scores, date) = feeds::parse_epss_page(EPSS_FIXTURE).unwrap();
    let cves: Vec<&str> = scores.keys().map(String::as_str).collect();
    write_epss_cache(dir, &cves, &scores, &date.unwrap(), fetched);
}

#[tokio::test]
async fn a_cache_older_than_the_limit_counts_as_not_checked() {
    let dir = tempfile::tempdir().unwrap();
    cached_feeds(dir.path(), now() - Duration::days(MAX_CACHE_AGE_DAYS + 1));
    let mut findings = osv_findings();

    let (stamp, notes) = enrich(&mut findings, &FixtureFetcher::offline(), Some(dir.path())).await;

    let stale = FeedStamp::NotChecked {
        reason: "the feed could not be fetched and the cached copy is older than 7 days"
            .to_string(),
    };
    assert_eq!(stamp.cisa_kev, stale);
    assert_eq!(stamp.first_epss, stale);
    assert_eq!(by_package(&findings, "log4j-core").severity, "medium");
    assert!(findings
        .iter()
        .all(|f| !f.captures.contains_key(KNOWN_EXPLOITED_CAPTURE)));
    assert_eq!(notes.len(), 2);
}

#[tokio::test]
async fn a_fresh_cache_is_used_and_the_stamp_says_it_is_cached() {
    let dir = tempfile::tempdir().unwrap();
    cached_feeds(dir.path(), now() - Duration::days(2));
    let mut findings = osv_findings();

    let (stamp, notes) = enrich(&mut findings, &FixtureFetcher::offline(), Some(dir.path())).await;

    assert_eq!(by_package(&findings, "log4j-core").severity, "critical");
    let cached =
        Some("cached copy fetched 2026-10-26 UTC; the live feed could not be fetched".to_string());
    assert_eq!(
        stamp.cisa_kev,
        FeedStamp::Checked {
            as_of: "2026-10-27".to_string(),
            note: cached.clone()
        }
    );
    assert_eq!(
        stamp.first_epss,
        FeedStamp::Checked {
            as_of: "2026-10-27".to_string(),
            note: cached
        }
    );
    assert!(notes.is_empty());
}

#[tokio::test]
async fn a_live_fetch_writes_the_cache_a_later_offline_run_reads() {
    let dir = tempfile::tempdir().unwrap();
    let mut first = osv_findings();
    enrich(&mut first, &FixtureFetcher::online(), Some(dir.path())).await;
    assert!(dir.path().join("kev.json").exists());
    assert!(dir.path().join("epss.json").exists());

    let mut second = osv_findings();
    let (stamp, _) = enrich(&mut second, &FixtureFetcher::offline(), Some(dir.path())).await;
    assert_eq!(by_package(&second, "log4j-core").severity, "critical");
    assert!(matches!(
        stamp.cisa_kev,
        FeedStamp::Checked { note: Some(_), .. }
    ));
}

#[tokio::test]
async fn an_epss_cache_missing_a_needed_cve_is_not_used() {
    let dir = tempfile::tempdir().unwrap();
    write_epss_cache(
        dir.path(),
        &["CVE-2021-44228"],
        &Default::default(),
        "2026-10-27",
        now() - Duration::days(1),
    );
    let mut findings = osv_findings();
    let (stamp, _) = enrich(&mut findings, &FixtureFetcher::offline(), Some(dir.path())).await;
    assert!(matches!(stamp.first_epss, FeedStamp::NotChecked { .. }));
}

#[tokio::test]
async fn no_feed_is_fetched_when_osv_scanner_did_not_run() {
    let mut findings = osv_findings();
    let fetcher = FixtureFetcher::online();
    let runs = vec![OsvRun {
        repo: "acme/app".to_string(),
        ran: false,
    }];
    let (stamp, notes) = enrich_with(&mut findings, &runs, &fetcher, None, now()).await;
    assert!(fetcher.calls().is_empty());
    assert_eq!(
        stamp,
        VulnDataStamp::all_not_checked("the dependency audit did not run")
    );
    assert!(notes.is_empty());
}

#[tokio::test]
async fn osv_stamp_names_a_repo_it_could_not_scan() {
    let mut findings = Vec::new();
    let runs = vec![
        OsvRun {
            repo: "acme/app".to_string(),
            ran: true,
        },
        OsvRun {
            repo: "acme/worker".to_string(),
            ran: false,
        },
    ];
    let (stamp, _) =
        enrich_with(&mut findings, &runs, &FixtureFetcher::online(), None, now()).await;
    assert_eq!(
        stamp.osv,
        FeedStamp::Checked {
            as_of: "2026-10-28".to_string(),
            note: Some("not checked for acme/worker".to_string())
        }
    );
    // No CVEs to look up: EPSS is plainly not queried, and that is not a coverage gap.
    assert_eq!(
        stamp.first_epss,
        FeedStamp::NotChecked {
            reason: "no dependency finding carries a CVE identifier".to_string()
        }
    );
}

#[tokio::test]
async fn epss_requests_are_batched() {
    let mut findings: Vec<Finding> = (0..(EPSS_BATCH + 5))
        .map(|i| {
            let mut f = Finding {
                rule_id: DEP_AUDIT_RULE_ID.to_string(),
                ..Finding::default()
            };
            f.captures
                .insert(CVE_IDS_CAPTURE.to_string(), format!("CVE-2020-{:05}", i));
            f
        })
        .collect();
    let fetcher = FixtureFetcher::online();
    enrich(&mut findings, &fetcher, None).await;
    let epss_calls = fetcher
        .calls()
        .into_iter()
        .filter(|u| u.starts_with(EPSS_URL))
        .count();
    assert_eq!(epss_calls, 2);
}

#[test]
fn osv_run_classifies_dep_audit_notes() {
    let note = |m: &str| CoverageNote {
        tool: "osv-scanner".to_string(),
        message: m.to_string(),
    };
    assert!(OsvRun::from_note("a", None).ran);
    assert!(
        OsvRun::from_note(
            "a",
            Some(&note(
                "dependency audit (osv-scanner) found no lockfiles in a"
            ))
        )
        .ran
    );
    assert!(
        !OsvRun::from_note(
            "a",
            Some(&note("dependency audit (osv-scanner) did not run: offline"))
        )
        .ran
    );
    assert!(
        !OsvRun::from_note(
            "a",
            Some(&note(
                "dependency audit (osv-scanner) timed out after 60 s for a"
            ))
        )
        .ran
    );
}

#[test]
fn kev_feed_without_a_release_date_is_rejected() {
    assert!(feeds::parse_kev(r#"{"vulnerabilities": []}"#).is_err());
    assert!(feeds::parse_kev(r#"{"dateReleased": "2026-10-27T00:00:00Z"}"#).is_err());
    let kev = feeds::parse_kev(kev_json()).unwrap();
    assert_eq!(kev.date_released, "2026-10-27");
    assert_eq!(kev.entries.len(), 2 + KEV_MIN_ENTRIES);
    assert_eq!(kev.entries["CVE-2021-44228"], "2021-12-10");
}

/// `n` KEV rows in the real catalog's shape, `parseable` of which carry a usable `cveID`.
fn kev_body(n: usize, parseable: bool) -> String {
    let rows: Vec<serde_json::Value> = (0..n)
        .map(|i| {
            if parseable {
                serde_json::json!({"cveID": format!("CVE-2020-{i:05}"), "dateAdded": "2020-01-01"})
            } else {
                // A schema change: the id moved to a field the parser does not read.
                serde_json::json!({"cve": format!("CVE-2020-{i:05}")})
            }
        })
        .collect();
    serde_json::json!({"dateReleased": "2026-10-27", "vulnerabilities": rows}).to_string()
}

#[test]
fn a_kev_body_that_parses_to_nothing_or_too_little_is_a_failed_fetch() {
    // Non-empty array, zero parsed entries: a schema change, not an empty catalog.
    let err = feeds::parse_kev(&kev_body(500, false)).unwrap_err();
    assert!(err.to_string().contains("0 usable entries"), "{err}");
    // A truncated body: a few good rows, far below the floor.
    assert!(feeds::parse_kev(&kev_body(KEV_MIN_ENTRIES - 1, true)).is_err());
    // An empty array is the same failure.
    assert!(feeds::parse_kev(&kev_body(0, true)).is_err());
    // Exactly at the floor is believed.
    assert_eq!(
        feeds::parse_kev(&kev_body(KEV_MIN_ENTRIES, true))
            .unwrap()
            .entries
            .len(),
        KEV_MIN_ENTRIES
    );
}

/// Serves one fixed body for the KEV URL and fails everything else.
struct BodyFetcher(String);

#[async_trait::async_trait]
impl FeedFetcher for BodyFetcher {
    async fn get(&self, url: &str) -> anyhow::Result<String> {
        if url == KEV_URL {
            Ok(self.0.clone())
        } else {
            anyhow::bail!("unreachable")
        }
    }
}

#[tokio::test]
async fn an_unparseable_kev_body_falls_back_to_the_cache_or_reads_not_checked() {
    let broken = BodyFetcher(kev_body(500, false));

    // No cache: not checked, never an empty catalog marked as checked.
    let mut findings = osv_findings();
    let (stamp, notes) = enrich_with(&mut findings, &scanned(), &broken, None, now()).await;
    assert_eq!(
        stamp.cisa_kev,
        FeedStamp::NotChecked {
            reason: "the feed could not be fetched".to_string()
        }
    );
    assert!(notes
        .iter()
        .any(|n| n.message.contains("CISA KEV not checked")));
    assert_eq!(by_package(&findings, "log4j-core").severity, "medium");

    // A fresh cache: used, and the stamp says it is a cached copy.
    let dir = tempfile::tempdir().unwrap();
    cached_feeds(dir.path(), now() - Duration::days(1));
    let mut findings = osv_findings();
    let (stamp, _) = enrich_with(&mut findings, &scanned(), &broken, Some(dir.path()), now()).await;
    assert!(matches!(
        stamp.cisa_kev,
        FeedStamp::Checked { note: Some(_), .. }
    ));
    assert_eq!(by_package(&findings, "log4j-core").severity, "critical");

    // The broken body must not have overwritten the good cache.
    let kept: feeds::Cached<KevFeed> = feeds::read_cache(&dir.path().join("kev.json")).unwrap();
    assert!(kept.data.entries.len() >= KEV_MIN_ENTRIES);
}

#[test]
fn epss_accepts_numbers_as_well_as_strings() {
    let (scores, date) = feeds::parse_epss_page(
        r#"{"status":"OK","data":[{"cve":"cve-2021-1","epss":0.5,"percentile":0.9,"date":"2026-10-27"}]}"#,
    )
    .unwrap();
    assert_eq!(date.as_deref(), Some("2026-10-27"));
    assert_eq!(scores["CVE-2021-1"].epss, 0.5);
    assert!(feeds::parse_epss_page(r#"{"status":"ERROR","data":[]}"#).is_err());
}

#[test]
fn an_epss_page_whose_rows_all_fail_to_parse_is_an_error_but_an_empty_one_is_not() {
    // Rows exist but none parse (a renamed field): a failed fetch, not "no scores published".
    let err = feeds::parse_epss_page(
        r#"{"status":"OK","data":[{"cve":"CVE-2021-1","score":"0.5"},{"cve":"CVE-2021-2"}]}"#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("none had a usable"), "{err}");
    // One good row among bad ones is a usable page.
    let (scores, _) = feeds::parse_epss_page(
        r#"{"status":"OK","data":[{"cve":"CVE-2021-1"},{"cve":"CVE-2021-2","epss":"0.1","percentile":"0.2","date":"2026-10-27"}]}"#,
    )
    .unwrap();
    assert_eq!(scores.len(), 1);
    // FIRST answering with no rows at all is a real answer (no CVE has a score yet).
    let (scores, date) = feeds::parse_epss_page(r#"{"status":"OK","data":[]}"#).unwrap();
    assert!(scores.is_empty() && date.is_none());
}

// ─── provenance: README and manifest ─────────────────────────────────────────

fn sample_stamp() -> VulnDataStamp {
    VulnDataStamp {
        cisa_kev: FeedStamp::Checked {
            as_of: "2026-10-27".to_string(),
            note: None,
        },
        first_epss: FeedStamp::NotChecked {
            reason: "the feed could not be fetched".to_string(),
        },
        osv: FeedStamp::Checked {
            as_of: "2026-10-28".to_string(),
            note: None,
        },
        endoflife: FeedStamp::Checked {
            as_of: "2026-10-28".to_string(),
            note: None,
        },
        nvd: FeedStamp::NotChecked {
            reason: "the feed could not be fetched".to_string(),
        },
    }
}

fn provenance_with(stamp: Option<VulnDataStamp>) -> crate::ManifestProvenance {
    crate::ManifestProvenance {
        repos: vec![crate::ProvenanceRepo {
            name: "acme/app".to_string(),
            commit: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
            dirty: false,
        }],
        generated_at: "2026-10-28T12:00:00Z".to_string(),
        vulnerability_data: stamp,
        ..Default::default()
    }
}

#[test]
fn the_stamp_appears_in_the_readme_and_the_manifest() {
    let provenance = provenance_with(Some(sample_stamp()));
    let readme = crate::product_export_readme("acme-stem", &provenance);
    assert!(
        readme.contains("Vulnerability data (CISA KEV) as of 2026-10-27\n"),
        "{readme}"
    );
    assert!(readme
        .contains("Vulnerability data (FIRST EPSS) not checked (the feed could not be fetched)\n"));
    assert!(readme.contains("Vulnerability data (OSV) as of 2026-10-28\n"));
    assert!(readme.contains("Vulnerability data (End-of-life, endoflife.date) as of 2026-10-28\n"));
    assert!(readme.contains(
        "Vulnerability data (NVD, runtimes) not checked (the feed could not be fetched)\n"
    ));

    let inspection = provenance.inspection_json();
    let data = &inspection["vulnerability_data"];
    assert_eq!(data["cisa_kev"]["status"], "checked");
    assert_eq!(data["cisa_kev"]["as_of"], "2026-10-27");
    assert_eq!(data["first_epss"]["status"], "not_checked");
    assert!(data["first_epss"]["as_of"].is_null());
    assert_eq!(data["osv"]["as_of"], "2026-10-28");

    // The release gate holds the README and the manifest equal.
    let manifest = serde_json::json!({
        "generated_at": provenance.generated_at,
        "inspection": inspection,
    });
    let problems = crate::release_gate::verify_readme_provenance(&readme, &manifest);
    assert!(problems.is_empty(), "{problems:?}");
    // And catches a README that disagrees.
    let tampered = readme.replace(
        "Vulnerability data (CISA KEV) as of 2026-10-27",
        "Vulnerability data (CISA KEV) as of 2026-10-28",
    );
    assert!(!crate::release_gate::verify_readme_provenance(&tampered, &manifest).is_empty());
}

#[test]
fn a_report_without_a_stamp_says_not_checked_rather_than_omitting_it() {
    let provenance = provenance_with(None);
    let readme = provenance.readme_lines();
    for label in ["CISA KEV", "FIRST EPSS", "OSV"] {
        assert!(
            readme.contains(&format!(
                "Vulnerability data ({label}) not checked (no vulnerability data was recorded \
                 for this inspection)"
            )),
            "{readme}"
        );
    }
    assert_eq!(
        provenance.inspection_json()["vulnerability_data"]["osv"]["status"],
        "not_checked"
    );
}

#[test]
fn stamp_text_has_no_em_dashes() {
    let stamps = [
        sample_stamp(),
        VulnDataStamp::all_not_checked(REASON_NOT_RECORDED),
    ];
    for stamp in &stamps {
        assert!(!readme_lines(Some(stamp)).contains('\u{2014}'));
    }
    assert!(!feeds::reason_stale().contains('\u{2014}'));
}

// ─── the time budget and concurrency ─────────────────────────────────────────

/// Wraps [`FixtureFetcher`] with a per-request delay and records how many requests were in
/// flight at once. Time is paused in the tests that use it, so a "slow" feed costs no real time.
struct SlowFetcher {
    delay: std::time::Duration,
    inner: FixtureFetcher,
    in_flight: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
}

impl SlowFetcher {
    fn new(delay: std::time::Duration, inner: FixtureFetcher) -> Self {
        Self {
            delay,
            inner,
            in_flight: Default::default(),
            peak: Default::default(),
        }
    }
}

#[async_trait::async_trait]
impl FeedFetcher for SlowFetcher {
    async fn get(&self, url: &str) -> anyhow::Result<String> {
        use std::sync::atomic::Ordering::SeqCst;
        let now_in_flight = self.in_flight.fetch_add(1, SeqCst) + 1;
        self.peak.fetch_max(now_in_flight, SeqCst);
        tokio::time::sleep(self.delay).await;
        self.in_flight.fetch_sub(1, SeqCst);
        self.inner.get(url).await
    }
}

#[tokio::test(start_paused = true)]
async fn kev_and_epss_are_fetched_concurrently() {
    let fetcher = SlowFetcher::new(std::time::Duration::from_secs(10), FixtureFetcher::online());
    let mut findings = osv_findings();
    let started = tokio::time::Instant::now();

    let (stamp, notes) = enrich_with(&mut findings, &scanned(), &fetcher, None, now()).await;

    assert_eq!(
        fetcher.peak.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "both feeds must be in flight at the same time"
    );
    assert_eq!(
        started.elapsed(),
        std::time::Duration::from_secs(10),
        "concurrent feeds cost one delay, not two"
    );
    assert!(matches!(stamp.cisa_kev, FeedStamp::Checked { .. }));
    assert!(matches!(stamp.first_epss, FeedStamp::Checked { .. }));
    assert!(notes.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_feed_that_outlives_the_budget_reads_not_checked_and_the_scan_is_released_on_time() {
    // Every request takes an hour. The enrichment must give up at the budget, not wait.
    let fetcher = SlowFetcher::new(
        std::time::Duration::from_secs(3600),
        FixtureFetcher::online(),
    );
    let mut findings = osv_findings();
    let started = tokio::time::Instant::now();

    let (stamp, notes) = enrich_with(&mut findings, &scanned(), &fetcher, None, now()).await;

    assert_eq!(
        started.elapsed(),
        ENRICHMENT_BUDGET,
        "the whole enrichment is released at the budget"
    );
    assert_eq!(ENRICHMENT_BUDGET, std::time::Duration::from_secs(45));
    let unavailable = FeedStamp::NotChecked {
        reason: "the feed could not be fetched".to_string(),
    };
    assert_eq!(stamp.cisa_kev, unavailable);
    assert_eq!(stamp.first_epss, unavailable);
    assert_eq!(by_package(&findings, "log4j-core").severity, "medium");
    assert_eq!(notes.len(), 2, "{notes:?}");
}

#[tokio::test(start_paused = true)]
async fn a_timed_out_feed_falls_back_to_a_fresh_cache() {
    let dir = tempfile::tempdir().unwrap();
    cached_feeds(dir.path(), now() - Duration::days(1));
    let fetcher = SlowFetcher::new(
        std::time::Duration::from_secs(3600),
        FixtureFetcher::online(),
    );
    let mut findings = osv_findings();

    let (stamp, _) =
        enrich_with(&mut findings, &scanned(), &fetcher, Some(dir.path()), now()).await;

    assert_eq!(by_package(&findings, "log4j-core").severity, "critical");
    assert!(matches!(
        stamp.cisa_kev,
        FeedStamp::Checked { note: Some(_), .. }
    ));
    assert!(matches!(
        stamp.first_epss,
        FeedStamp::Checked { note: Some(_), .. }
    ));
}

#[tokio::test(start_paused = true)]
async fn one_slow_feed_does_not_discard_the_other() {
    // KEV answers at once; EPSS hangs. KEV labels must still land.
    struct EpssHangs(FixtureFetcher);
    #[async_trait::async_trait]
    impl FeedFetcher for EpssHangs {
        async fn get(&self, url: &str) -> anyhow::Result<String> {
            if url.starts_with(EPSS_URL) {
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            }
            self.0.get(url).await
        }
    }
    let mut findings = osv_findings();
    let (stamp, _) = enrich_with(
        &mut findings,
        &scanned(),
        &EpssHangs(FixtureFetcher::online()),
        None,
        now(),
    )
    .await;
    assert_eq!(by_package(&findings, "log4j-core").severity, "critical");
    assert!(matches!(stamp.cisa_kev, FeedStamp::Checked { .. }));
    assert!(matches!(stamp.first_epss, FeedStamp::NotChecked { .. }));
}

// ─── mixed state: KEV present, EPSS unavailable ──────────────────────────────

#[tokio::test]
async fn kev_present_and_epss_unavailable_labels_kev_attaches_no_epss_and_stamps_both() {
    let mut findings = osv_findings();
    let fetcher = FixtureFetcher::new(Some(kev_json()), None);

    let (stamp, notes) = enrich(&mut findings, &fetcher, None).await;

    // KEV labels land.
    let log4j = by_package(&findings, "log4j-core");
    assert_eq!(log4j.severity, "critical");
    assert_eq!(
        log4j
            .captures
            .get(KNOWN_EXPLOITED_CAPTURE)
            .map(String::as_str),
        Some("CVE-2021-44228")
    );
    // No finding carries any EPSS capture or EPSS sentence.
    for f in &findings {
        assert!(
            !f.captures.contains_key(EPSS_CVE_CAPTURE)
                && !f.captures.contains_key(EPSS_SCORE_CAPTURE)
                && !f.captures.contains_key(EPSS_PERCENTILE_CAPTURE)
                && !f.detail.contains("EPSS"),
            "{f:?}"
        );
    }
    // The two stamp lines say so, in the README and in the manifest, and the gate agrees.
    let provenance = provenance_with(Some(stamp.clone()));
    let readme = crate::product_export_readme("acme-stem", &provenance);
    assert!(
        readme.contains("Vulnerability data (CISA KEV) as of 2026-10-27\n"),
        "{readme}"
    );
    assert!(
        readme.contains(
            "Vulnerability data (FIRST EPSS) not checked (the feed could not be fetched)\n"
        ),
        "{readme}"
    );
    let inspection = provenance.inspection_json();
    assert_eq!(
        inspection["vulnerability_data"]["cisa_kev"]["status"],
        "checked"
    );
    assert_eq!(
        inspection["vulnerability_data"]["first_epss"]["status"],
        "not_checked"
    );
    let manifest = serde_json::json!({
        "generated_at": provenance.generated_at,
        "inspection": inspection,
    });
    assert!(crate::release_gate::verify_readme_provenance(&readme, &manifest).is_empty());
    // Only EPSS is a coverage gap.
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].message.starts_with("FIRST EPSS not checked"));
}

// ─── FIRST returned nothing ──────────────────────────────────────────────────

const EPSS_NO_ROWS: &str = r#"{"status":"OK","total":0,"data":[]}"#;

#[tokio::test]
async fn no_epss_rows_is_stated_plainly_and_no_score_date_is_invented() {
    let mut findings = osv_findings();
    let fetcher = FixtureFetcher::new(Some(kev_json()), Some(EPSS_NO_ROWS));

    let (stamp, notes) = enrich(&mut findings, &fetcher, None).await;

    assert_eq!(
        stamp.first_epss,
        FeedStamp::Checked {
            as_of: "2026-10-28".to_string(),
            note: Some(
                "no scores were published for these CVEs as of the request date (UTC)".to_string()
            ),
        }
    );
    assert_eq!(
        readme_line("FIRST EPSS", &stamp.first_epss),
        "Vulnerability data (FIRST EPSS) as of 2026-10-28 (no scores were published for these \
         CVEs as of the request date (UTC))"
    );
    assert!(findings
        .iter()
        .all(|f| !f.captures.contains_key(EPSS_SCORE_CAPTURE)));
    // The feed answered, so this is not a coverage gap.
    assert!(notes.is_empty(), "{notes:?}");
}

#[tokio::test]
async fn a_cached_no_scores_answer_is_dated_by_when_first_was_asked() {
    let dir = tempfile::tempdir().unwrap();
    let asked = now() - Duration::days(2);
    let fetcher = FixtureFetcher::new(Some(kev_json()), Some(EPSS_NO_ROWS));
    let mut live = osv_findings();
    enrich_with(&mut live, &scanned(), &fetcher, Some(dir.path()), asked).await;

    let mut findings = osv_findings();
    let (stamp, _) = enrich(&mut findings, &FixtureFetcher::offline(), Some(dir.path())).await;
    assert_eq!(
        stamp.first_epss,
        FeedStamp::Checked {
            as_of: "2026-10-26".to_string(),
            note: Some(
                "cached copy fetched 2026-10-26 UTC; the live feed could not be fetched; no \
                 scores were published for these CVEs as of the request date (UTC)"
                    .to_string()
            ),
        },
        "the cached answer is dated by the day FIRST was asked, never by today"
    );
}

// ─── the EPSS cache accumulates across runs ──────────────────────────────────

fn finding_with_cve(cve: &str) -> Finding {
    let mut f = Finding {
        rule_id: DEP_AUDIT_RULE_ID.to_string(),
        severity: "medium".to_string(),
        ..Finding::default()
    };
    f.captures
        .insert(CVE_IDS_CAPTURE.to_string(), cve.to_string());
    f
}

#[tokio::test]
async fn an_offline_run_for_another_project_uses_scores_an_earlier_run_fetched() {
    let dir = tempfile::tempdir().unwrap();

    // Project A, online: scores for the fixture CVEs are fetched and cached.
    let mut project_a = osv_findings();
    enrich_with(
        &mut project_a,
        &scanned(),
        &FixtureFetcher::online(),
        Some(dir.path()),
        now() - Duration::days(1),
    )
    .await;

    // Project B, online, asks about a different CVE: it must ADD to the cache, not replace it.
    let mut project_b = vec![finding_with_cve("CVE-2020-0001")];
    enrich_with(
        &mut project_b,
        &scanned(),
        &FixtureFetcher::online(),
        Some(dir.path()),
        now(),
    )
    .await;

    // Project A again, offline: its scores come from the merged cache.
    let mut again = osv_findings();
    let (stamp, _) = enrich(&mut again, &FixtureFetcher::offline(), Some(dir.path())).await;
    let semver = by_package(&again, "semver");
    assert_eq!(
        semver.captures.get(EPSS_SCORE_CAPTURE).map(String::as_str),
        Some("0.00238")
    );
    // The stamp is dated by the OLDEST entry used: project A's CVEs were fetched yesterday.
    assert_eq!(
        stamp.first_epss,
        FeedStamp::Checked {
            as_of: "2026-10-27".to_string(),
            note: Some(
                "cached copy fetched 2026-10-27 UTC; the live feed could not be fetched"
                    .to_string()
            ),
        }
    );

    // And project B's own CVE is there too.
    let mut b_again = vec![finding_with_cve("CVE-2020-0001")];
    let (stamp_b, _) = enrich(&mut b_again, &FixtureFetcher::offline(), Some(dir.path())).await;
    assert!(matches!(stamp_b.first_epss, FeedStamp::Checked { .. }));
}

#[test]
fn merging_never_refreshes_an_older_entry_and_drops_an_expired_one() {
    let batch = |cve: &str| EpssBatch {
        date: Some("2026-10-28".to_string()),
        queried: [cve.to_string()].into_iter().collect(),
        scores: [(
            cve.to_string(),
            EpssScore {
                epss: 0.5,
                percentile: 0.9,
            },
        )]
        .into_iter()
        .collect(),
    };
    let entry = |fetched: DateTime<Utc>| feeds::EpssCacheEntry {
        fetched_at: fetched.to_rfc3339(),
        date: Some("2026-10-01".to_string()),
        score: None,
    };
    let existing = feeds::EpssCache {
        entries: [
            ("CVE-OLD-1".to_string(), entry(now() - Duration::days(2))),
            (
                "CVE-EXPIRED-1".to_string(),
                entry(now() - Duration::days(MAX_CACHE_AGE_DAYS + 1)),
            ),
        ]
        .into_iter()
        .collect(),
    };

    let merged = feeds::merge_epss_cache(Some(existing), &batch("CVE-NEW-1"), now());

    assert!(!merged.entries.contains_key("CVE-EXPIRED-1"));
    assert_eq!(
        merged.entries["CVE-OLD-1"].fetched_at,
        (now() - Duration::days(2)).to_rfc3339(),
        "an entry keeps its own fetch time, so it is never made to look fresher"
    );
    assert_eq!(merged.entries["CVE-NEW-1"].fetched_at, now().to_rfc3339());
    assert_eq!(merged.entries["CVE-NEW-1"].score.unwrap().epss, 0.5);
}

// ─── the cache file is written atomically ────────────────────────────────────

#[test]
fn write_cache_replaces_the_file_whole_and_leaves_no_temp_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vuln-intel").join("kev.json");
    let first = feeds::parse_kev(kev_json()).unwrap();
    feeds::write_cache(&path, &first, now());
    let mut second = first.clone();
    second.date_released = "2026-10-28".to_string();
    feeds::write_cache(&path, &second, now());

    let read: feeds::Cached<KevFeed> = feeds::read_cache(&path).unwrap();
    assert_eq!(read.data.date_released, "2026-10-28");
    let names: Vec<String> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        vec!["kev.json".to_string()],
        "no temp file may remain"
    );
}

// ─── severity: the KEV floor and the corpus bounds ───────────────────────────

fn ceiling_rule_set(max: &str) -> camerata_rules::RuleSet {
    let mut rule = camerata_rules::bare_rule(DEP_AUDIT_RULE_ID, "dependency-audit");
    rule.max_severity = Some(max.to_string());
    let mut set = camerata_rules::RuleSet::default();
    set.push(rule);
    set
}

/// The pipeline order, run as the three scan paths run it: enrichment, then the corpus bounds.
async fn enrich_then_bound(corpus: Option<&camerata_rules::RuleSet>) -> Vec<Finding> {
    let mut findings = osv_findings();
    enrich(&mut findings, &FixtureFetcher::online(), None).await;
    crate::severity_bounds::apply_scan_severity_bounds(&mut findings, corpus);
    findings
}

#[tokio::test]
async fn the_kev_floor_survives_the_scan_severity_bounds_so_the_raised_text_is_true() {
    // With no corpus (the empty rule set) and with a rule set that bounds some other rule, the
    // dependency finding is still critical after the bounds, so "raised ... to critical" holds.
    let mut other = camerata_rules::RuleSet::default();
    let mut rule = camerata_rules::bare_rule("SOME-OTHER-1", "test");
    rule.max_severity = Some("low".to_string());
    other.push(rule);
    for corpus in [None, Some(&other)] {
        let findings = enrich_then_bound(corpus).await;
        let log4j = by_package(&findings, "log4j-core");
        assert_eq!(log4j.severity, "critical");
        assert!(log4j
            .detail
            .contains("Severity raised from medium to critical"));
    }
}

#[tokio::test]
async fn no_shipped_ceiling_governs_dependency_findings() {
    // The sentence "Severity raised from X to critical" is only true while no rule that governs
    // DEP-AUDIT-1 findings declares a `max_severity`. Prove it against the REAL corpus. If this
    // fails, a ceiling was added: reconcile the enrichment text (see `vuln_intel::apply`)
    // instead of weakening this test.
    let corpus =
        camerata_rules::load_corpus(std::path::Path::new(camerata_rules::DEFAULT_CORPUS_PATH))
            .await
            .expect("the real corpus loads");
    let findings = enrich_then_bound(Some(&corpus)).await;
    let log4j = by_package(&findings, "log4j-core");
    assert_eq!(
        log4j.severity, "critical",
        "a corpus ceiling now lowers KEV-raised dependency findings; the 'raised to critical' \
         text would be false: {}",
        log4j.detail
    );
}

#[tokio::test]
async fn a_dependency_ceiling_would_overrule_the_raise_so_the_text_depends_on_there_being_none() {
    // Pins the ORDER: enrichment runs first and the bounds have the last word. A `max_severity`
    // on the dependency rule therefore lowers a KEV finding, and the detail would still say it
    // was raised to critical. That is why the test above exists; this one shows the hazard so a
    // future author who adds such a ceiling sees exactly what to reconcile.
    let ceiling = ceiling_rule_set("high");
    let findings = enrich_then_bound(Some(&ceiling)).await;
    let log4j = by_package(&findings, "log4j-core");
    assert_eq!(log4j.severity, "high", "the bounds run after the KEV floor");
    assert!(
        log4j.detail.contains("raised from medium to critical"),
        "the enrichment text is written before the bounds can move the severity"
    );
}
