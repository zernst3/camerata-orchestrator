//! Network-free tests: every feed comes from the fixtures under
//! `crates/server/tests/fixtures/vuln_intel/` through [`FixtureFetcher`].

use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Duration, TimeZone, Utc};

use super::*;

const KEV_FIXTURE: &str = include_str!("../../tests/fixtures/vuln_intel/kev.json");
const EPSS_FIXTURE: &str = include_str!("../../tests/fixtures/vuln_intel/epss.json");
const OSV_FIXTURE: &str = include_str!("../../tests/fixtures/vuln_intel/osv_scan.json");

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
        Self::new(Some(KEV_FIXTURE), Some(EPSS_FIXTURE))
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
    let fetcher = FixtureFetcher::new(Some(KEV_FIXTURE), None);
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

fn cached_feeds(dir: &Path, fetched: DateTime<Utc>) {
    let kev = feeds::parse_kev(KEV_FIXTURE).unwrap();
    write_cached(dir, "kev.json", &kev, fetched);
    let (scores, date) = feeds::parse_epss_page(EPSS_FIXTURE).unwrap();
    let epss = EpssBatch {
        date,
        queried: scores.keys().cloned().collect(),
        scores,
    };
    write_cached(dir, "epss.json", &epss, fetched);
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
        Some("cached copy fetched 2026-10-26; the live feed could not be fetched".to_string());
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
    let epss = EpssBatch {
        date: Some("2026-10-27".to_string()),
        queried: ["CVE-2021-44228".to_string()].into_iter().collect(),
        scores: Default::default(),
    };
    write_cached(dir.path(), "epss.json", &epss, now() - Duration::days(1));
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
    let kev = feeds::parse_kev(KEV_FIXTURE).unwrap();
    assert_eq!(kev.date_released, "2026-10-27");
    assert_eq!(kev.entries.len(), 2);
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
