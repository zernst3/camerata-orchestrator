//! Part A of the corpus freshness routine, end to end and with no real network: an injected
//! fetcher for the judging logic, and a local fake HTTP server (wiremock) for the real transport.
#![allow(clippy::unwrap_used)]

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use camerata_rules::{bare_rule, RuleSet, RuleSource};
use camerata_source_check::{
    render_json, render_text, run_check, source_confidence, AcceptTarget, CheckConfig, DiffOp,
    FetchOutcome, Fetcher, HttpFetcher, LinkState, SourceCheckReport, SourceHashes,
    DEFAULT_CHANGED_WINDOW_DAYS,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------------------------

fn source(url: &str) -> RuleSource {
    RuleSource {
        url: url.to_string(),
        title: "t".to_string(),
        linter: None,
        relevance: None,
    }
}

fn rules(spec: &[(&str, &[&str])]) -> RuleSet {
    let mut set = RuleSet::default();
    for (id, urls) in spec {
        let mut r = bare_rule(id, "demo");
        r.sources = urls.iter().map(|u| source(u)).collect();
        set.push(r);
    }
    set
}

fn cfg(today: &str) -> CheckConfig {
    CheckConfig {
        today: today.to_string(),
        delay: Duration::ZERO,
        max_parallel_hosts: 4,
    }
}

fn ok_page(body: &str) -> FetchOutcome {
    FetchOutcome::Ok {
        final_url: String::new(),
        redirected: false,
        body: Some(body.to_string()),
    }
}

/// Serves canned outcomes and counts how often each page was requested.
struct MapFetcher {
    pages: Mutex<HashMap<String, FetchOutcome>>,
    calls: Mutex<Vec<String>>,
}

impl MapFetcher {
    fn new(pages: &[(&str, FetchOutcome)]) -> Self {
        Self {
            pages: Mutex::new(
                pages
                    .iter()
                    .map(|(u, o)| (u.to_string(), o.clone()))
                    .collect(),
            ),
            calls: Mutex::new(Vec::new()),
        }
    }
    fn set(&self, url: &str, outcome: FetchOutcome) {
        self.pages.lock().unwrap().insert(url.to_string(), outcome);
    }
}

#[async_trait]
impl Fetcher for MapFetcher {
    async fn fetch(&self, url: &str, _want_body: bool) -> FetchOutcome {
        self.calls.lock().unwrap().push(url.to_string());
        self.pages
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .unwrap_or(FetchOutcome::Dead {
                detail: "HTTP 404".to_string(),
            })
    }
}

// ---------------------------------------------------------------------------------------------
// link status through the real transport (local fake server)
// ---------------------------------------------------------------------------------------------

fn http_fetcher(timeout_ms: u64) -> HttpFetcher {
    HttpFetcher::new(
        Duration::from_millis(timeout_ms),
        "camerata-source-check-test",
    )
    .unwrap()
}

#[tokio::test]
async fn http_ok_redirected_dead_and_unreachable_are_told_apart() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/fine"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<h1 id=a>A</h1>text"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/old"))
        .respond_with(ResponseTemplate::new(301).insert_header("Location", "/fine"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/gone"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/removed"))
        .respond_with(ResponseTemplate::new(410))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/down"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/blocked"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/slow"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(1500)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/loop"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", "/loop"))
        .mount(&server)
        .await;

    let f = http_fetcher(400);
    let base = server.uri();

    match f.fetch(&format!("{base}/fine"), true).await {
        FetchOutcome::Ok {
            redirected, body, ..
        } => {
            assert!(!redirected);
            assert!(body.unwrap().contains("text"));
        }
        other => panic!("expected ok, got {other:?}"),
    }
    match f.fetch(&format!("{base}/old"), false).await {
        FetchOutcome::Ok {
            redirected,
            final_url,
            body,
        } => {
            assert!(redirected);
            assert_eq!(final_url, format!("{base}/fine"));
            assert!(body.is_none(), "body is dropped when not wanted");
        }
        other => panic!("expected redirected ok, got {other:?}"),
    }
    for p in ["/gone", "/removed", "/loop"] {
        assert!(
            matches!(
                f.fetch(&format!("{base}{p}"), false).await,
                FetchOutcome::Dead { .. }
            ),
            "{p} should be dead"
        );
    }
    for p in ["/down", "/blocked", "/slow"] {
        assert!(
            matches!(
                f.fetch(&format!("{base}{p}"), false).await,
                FetchOutcome::Unreachable { .. }
            ),
            "{p} should be unreachable, not dead"
        );
    }
}

#[tokio::test]
async fn a_refused_connection_is_unreachable_never_dead() {
    // Bind to learn a free port, then drop the listener so nothing answers there.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let outcome = http_fetcher(500)
        .fetch(&format!("http://127.0.0.1:{port}/x"), false)
        .await;
    assert!(
        matches!(outcome, FetchOutcome::Unreachable { .. }),
        "got {outcome:?}"
    );
}

#[tokio::test]
async fn the_request_carries_the_clear_user_agent() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ua"))
        .and(wiremock::matchers::header(
            "user-agent",
            "camerata-source-check-test",
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let outcome = http_fetcher(500)
        .fetch(&format!("{}/ua", server.uri()), false)
        .await;
    assert!(matches!(outcome, FetchOutcome::Ok { .. }));
}

// ---------------------------------------------------------------------------------------------
// the report: link states, counts, no-source rules
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn report_counts_and_lists_cover_every_state() {
    let set = rules(&[
        ("R-OK", &["https://ok.example/a"]),
        ("R-MOVED", &["https://moved.example/a"]),
        (
            "R-DEAD",
            &["https://dead.example/a", "https://ok.example/a"],
        ),
        ("R-DOWN", &["https://down.example/a"]),
        ("R-NONE", &[]),
        ("R-INTERNAL", &["docs/ENFORCEMENT.md", "AGENTS.md"]),
    ]);
    let fetcher = MapFetcher::new(&[
        ("https://ok.example/a", ok_page("fine")),
        (
            "https://moved.example/a",
            FetchOutcome::Ok {
                final_url: "https://moved.example/b".to_string(),
                redirected: true,
                body: None,
            },
        ),
        (
            "https://dead.example/a",
            FetchOutcome::Dead {
                detail: "HTTP 404".to_string(),
            },
        ),
        (
            "https://down.example/a",
            FetchOutcome::Unreachable {
                detail: "timed out".to_string(),
            },
        ),
    ]);
    let (report, _) = run_check(&set, &fetcher, &SourceHashes::default(), &cfg("2026-10-27")).await;

    let c = &report.counts;
    assert_eq!(c.rules, 6);
    assert_eq!(c.rules_without_source, 1);
    assert_eq!(c.rules_internal_sources_only, 1);
    assert_eq!(c.web_urls, 4, "distinct cited web URLs");
    assert_eq!(c.internal_sources_skipped, 2);
    assert_eq!((c.ok, c.redirected, c.dead, c.unreachable), (1, 1, 1, 1));

    assert_eq!(report.checked_on, "2026-10-27");
    assert_eq!(report.dead.len(), 1);
    assert_eq!(report.dead[0].url, "https://dead.example/a");
    assert_eq!(report.dead[0].rules, vec!["R-DEAD"]);
    assert_eq!(
        report.redirected[0].final_url.as_deref(),
        Some("https://moved.example/b")
    );
    assert_eq!(report.unreachable[0].detail, "timed out");
    // The URL two rules cite is fetched once and lists both.
    let calls = fetcher.calls.lock().unwrap().clone();
    assert_eq!(
        calls
            .iter()
            .filter(|u| *u == "https://ok.example/a")
            .count(),
        1
    );

    assert_eq!(report.rules_without_source.len(), 1);
    assert_eq!(report.rules_without_source[0].id, "R-NONE");
    assert_eq!(report.rules_internal_sources_only[0].id, "R-INTERNAL");
}

#[tokio::test]
async fn the_text_report_leads_with_counts_is_date_stamped_and_lists_the_findings() {
    let set = rules(&[("R-DEAD", &["https://dead.example/a"]), ("R-NONE", &[])]);
    let fetcher = MapFetcher::new(&[(
        "https://dead.example/a",
        FetchOutcome::Dead {
            detail: "HTTP 404".to_string(),
        },
    )]);
    let (report, _) = run_check(&set, &fetcher, &SourceHashes::default(), &cfg("2026-10-27")).await;
    let text = render_text(&report);

    assert!(text.starts_with("CORPUS SOURCE CHECK, 2026-10-27"));
    let counts_at = text.find("Counts").unwrap();
    let dead_at = text.find("DEAD LINKS").unwrap();
    let none_at = text.find("RULES WITH NO SOURCE").unwrap();
    assert!(counts_at < dead_at && dead_at < none_at, "counts first");
    assert!(text.contains("https://dead.example/a"));
    assert!(text.contains("R-NONE"));
    assert!(
        !text.contains('\u{2014}'),
        "no em-dashes in user-facing text"
    );

    let json = render_json(&report).unwrap();
    let back: SourceCheckReport = serde_json::from_str(&json).unwrap();
    assert_eq!(back, report, "the JSON report round-trips");
}

// ---------------------------------------------------------------------------------------------
// sections: hash unchanged / changed (with diff) / baseline / anchor missing / sidecar round trip
// ---------------------------------------------------------------------------------------------

const PAGE_V1: &str = "<h1>Guide</h1><h2 id=idem>Idempotency</h2>\
    <p>Store the key. Reject duplicates within 24 hours. Return the first response.</p>\
    <h2 id=other>Other</h2><p>Unrelated text.</p>";

const PAGE_V2: &str = "<h1>Guide</h1><h2 id=idem>Idempotency</h2>\
    <p>Store the key. Reject duplicates within 72 hours. Return the first response.</p>\
    <h2 id=other>Other</h2><p>Unrelated text, edited.</p>";

#[tokio::test]
async fn section_hash_is_baselined_then_unchanged_then_changed_with_a_diff() {
    let url = "https://docs.example/guide#idem";
    let set = rules(&[("R-IDEM", &[url])]);
    let fetcher = MapFetcher::new(&[("https://docs.example/guide", ok_page(PAGE_V1))]);
    let dir = tempfile::tempdir().unwrap();
    let sidecar = dir.path().join("hashes.json");

    // Run 1: first sight is a baseline, not a change.
    let (r1, s1) = run_check(
        &set,
        &fetcher,
        &SourceHashes::load(&sidecar).unwrap(),
        &cfg("2026-10-01"),
    )
    .await;
    assert_eq!(r1.counts.sections_new_baseline, 1);
    assert!(r1.changed.is_empty());
    s1.save(&sidecar).unwrap();

    // Run 2: same text (even re-wrapped in whitespace): unchanged.
    fetcher.set(
        "https://docs.example/guide",
        ok_page(
            &PAGE_V1
                .replace("<p>", "\n\n  <p>\n")
                .replace("</p>", "\n</p>"),
        ),
    );
    let (r2, s2) = run_check(
        &set,
        &fetcher,
        &SourceHashes::load(&sidecar).unwrap(),
        &cfg("2026-11-01"),
    )
    .await;
    assert_eq!(r2.counts.sections_unchanged, 1);
    assert_eq!(r2.counts.sections_changed, 0);
    assert!(r2.changed.is_empty());
    s2.save(&sidecar).unwrap();

    // Run 3: the cited section's text changed (the sibling section's edit is irrelevant).
    fetcher.set("https://docs.example/guide", ok_page(PAGE_V2));
    let (r3, s3) = run_check(
        &set,
        &fetcher,
        &SourceHashes::load(&sidecar).unwrap(),
        &cfg("2026-12-01"),
    )
    .await;
    assert_eq!(r3.counts.sections_changed, 1);
    let change = &r3.changed[0];
    assert_eq!(change.url, url);
    assert_ne!(change.old_hash, change.new_hash);
    let removed: Vec<_> = change
        .diff
        .iter()
        .filter(|d| d.op == DiffOp::Removed)
        .collect();
    let added: Vec<_> = change
        .diff
        .iter()
        .filter(|d| d.op == DiffOp::Added)
        .collect();
    assert_eq!(removed.len(), 1);
    assert_eq!(added.len(), 1);
    assert!(removed[0].text.contains("24 hours"));
    assert!(added[0].text.contains("72 hours"));
    assert!(render_text(&r3).contains("+ Reject duplicates within 72 hours."));
    assert_eq!(change.first_seen, "2026-12-01");
    let record = &s3.entries[url];
    assert_eq!(
        record.section_hash.as_deref(),
        Some(change.old_hash.as_str()),
        "the accepted baseline is untouched by an unreviewed change"
    );
    assert_eq!(
        record.pending.as_ref().map(|p| p.hash.as_str()),
        Some(change.new_hash.as_str())
    );
}

/// Run one check against the sidecar on disk, save the result, and return the report.
async fn run_and_save(
    set: &RuleSet,
    fetcher: &MapFetcher,
    sidecar: &std::path::Path,
    today: &str,
) -> SourceCheckReport {
    let (report, next) = run_check(
        set,
        fetcher,
        &SourceHashes::load(sidecar).unwrap(),
        &cfg(today),
    )
    .await;
    next.save(sidecar).unwrap();
    report
}

const GUIDE: &str = "https://docs.example/guide";
const CITED: &str = "https://docs.example/guide#idem";

/// A sidecar with the cited section baselined on PAGE_V1, then PAGE_V2 seen once (run on
/// 2026-12-01), so the change is pending.
async fn pending_change_fixture() -> (RuleSet, MapFetcher, tempfile::TempDir, std::path::PathBuf) {
    let set = rules(&[("R-IDEM", &[CITED])]);
    let fetcher = MapFetcher::new(&[(GUIDE, ok_page(PAGE_V1))]);
    let dir = tempfile::tempdir().unwrap();
    let sidecar = dir.path().join("hashes.json");
    run_and_save(&set, &fetcher, &sidecar, "2026-10-01").await;
    fetcher.set(GUIDE, ok_page(PAGE_V2));
    let r = run_and_save(&set, &fetcher, &sidecar, "2026-12-01").await;
    assert_eq!(r.counts.sections_changed, 1);
    (set, fetcher, dir, sidecar)
}

#[tokio::test]
async fn a_change_stays_flagged_on_every_run_until_accepted() {
    let (set, fetcher, _dir, sidecar) = pending_change_fixture().await;
    let baseline = SourceHashes::load(&sidecar).unwrap().entries[CITED]
        .section_hash
        .clone();

    // Two more runs with the page unchanged since the first sighting: still changed, same diff
    // against the ORIGINAL accepted text, and the first-seen date does not move.
    for day in ["2027-01-05", "2027-02-09"] {
        let r = run_and_save(&set, &fetcher, &sidecar, day).await;
        assert_eq!(r.counts.sections_changed, 1, "run on {day}");
        assert_eq!(r.counts.sections_unchanged, 0);
        assert_eq!(r.changed[0].first_seen, "2026-12-01");
        assert!(r.changed[0]
            .diff
            .iter()
            .any(|d| d.op == DiffOp::Removed && d.text.contains("24 hours")));
        assert!(render_text(&r).contains("CHANGED SECTIONS, NOT YET REVIEWED"));
        assert_eq!(
            SourceHashes::load(&sidecar).unwrap().entries[CITED].section_hash,
            baseline,
            "the baseline never moves on its own"
        );
    }
}

#[tokio::test]
async fn a_change_stays_flagged_while_the_page_is_unreachable_or_the_anchor_is_gone() {
    let (set, fetcher, _dir, sidecar) = pending_change_fixture().await;

    fetcher.set(
        GUIDE,
        FetchOutcome::Unreachable {
            detail: "timed out".to_string(),
        },
    );
    let r = run_and_save(&set, &fetcher, &sidecar, "2027-01-05").await;
    assert_eq!(r.counts.unreachable, 1);
    assert_eq!(
        r.changed.len(),
        1,
        "still listed though the page did not answer"
    );

    fetcher.set(GUIDE, ok_page("<h1>Rewritten</h1>"));
    let r = run_and_save(&set, &fetcher, &sidecar, "2027-02-05").await;
    assert_eq!(r.counts.sections_anchor_missing, 1);
    assert_eq!(r.changed.len(), 1);
}

#[tokio::test]
async fn a_further_change_updates_the_pending_text_but_not_the_baseline_or_first_seen() {
    let (set, fetcher, _dir, sidecar) = pending_change_fixture().await;
    let baseline = SourceHashes::load(&sidecar).unwrap().entries[CITED]
        .section_hash
        .clone();
    let v3 = PAGE_V2.replace("72 hours", "7 days");
    fetcher.set(GUIDE, ok_page(&v3));
    let r = run_and_save(&set, &fetcher, &sidecar, "2027-01-05").await;
    assert_eq!(r.changed.len(), 1);
    assert_eq!(r.changed[0].first_seen, "2026-12-01");
    assert!(r.changed[0]
        .diff
        .iter()
        .any(|d| d.op == DiffOp::Added && d.text.contains("7 days")));
    assert_eq!(
        SourceHashes::load(&sidecar).unwrap().entries[CITED].section_hash,
        baseline
    );
}

#[tokio::test]
async fn a_page_that_reverts_to_the_accepted_text_clears_the_flag() {
    let (set, fetcher, _dir, sidecar) = pending_change_fixture().await;
    fetcher.set(GUIDE, ok_page(PAGE_V1));
    let r = run_and_save(&set, &fetcher, &sidecar, "2027-01-05").await;
    assert_eq!(r.counts.sections_unchanged, 1);
    assert!(r.changed.is_empty());
    assert!(SourceHashes::load(&sidecar).unwrap().entries[CITED]
        .pending
        .is_none());
}

#[tokio::test]
async fn accepting_clears_the_flag_records_who_and_when_and_rebaselines() {
    let (set, fetcher, _dir, sidecar) = pending_change_fixture().await;

    let mut loaded = SourceHashes::load(&sidecar).unwrap();
    let done = loaded
        .accept(
            AcceptTarget::Urls(&[CITED.to_string()]),
            "2027-01-02",
            "zach",
        )
        .unwrap();
    assert_eq!(done, vec![CITED.to_string()]);
    loaded.save(&sidecar).unwrap();

    let record = SourceHashes::load(&sidecar).unwrap().entries[CITED].clone();
    let acceptance = record.accepted.expect("who and when are recorded");
    assert_eq!(
        (acceptance.by.as_str(), acceptance.on.as_str()),
        ("zach", "2027-01-02")
    );
    assert!(record.pending.is_none());

    // The page still shows V2, which is now the accepted text: no longer a change.
    let r = run_and_save(&set, &fetcher, &sidecar, "2027-02-01").await;
    assert_eq!(r.counts.sections_unchanged, 1);
    assert!(r.changed.is_empty());

    // A change AFTER acceptance is flagged afresh against the newly accepted text.
    fetcher.set(GUIDE, ok_page(&PAGE_V2.replace("72 hours", "7 days")));
    let r = run_and_save(&set, &fetcher, &sidecar, "2027-03-01").await;
    assert_eq!(r.changed.len(), 1);
    assert_eq!(r.changed[0].first_seen, "2027-03-01");
    assert!(r.changed[0]
        .diff
        .iter()
        .any(|d| d.op == DiffOp::Removed && d.text.contains("72 hours")));
}

#[tokio::test]
async fn accept_all_clears_every_pending_change() {
    let set = rules(&[
        ("R-A", &["https://docs.example/guide#idem"]),
        ("R-B", &["https://other.example/p#sec"]),
    ]);
    let other_v1 = "<h2 id=sec>Sec</h2><p>One. Two.</p>";
    let other_v2 = "<h2 id=sec>Sec</h2><p>One. Three.</p>";
    let fetcher = MapFetcher::new(&[
        (GUIDE, ok_page(PAGE_V1)),
        ("https://other.example/p", ok_page(other_v1)),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let sidecar = dir.path().join("hashes.json");
    run_and_save(&set, &fetcher, &sidecar, "2026-10-01").await;
    fetcher.set(GUIDE, ok_page(PAGE_V2));
    fetcher.set("https://other.example/p", ok_page(other_v2));
    let r = run_and_save(&set, &fetcher, &sidecar, "2026-11-01").await;
    assert_eq!(r.counts.sections_changed, 2);

    let mut loaded = SourceHashes::load(&sidecar).unwrap();
    let done = loaded
        .accept(AcceptTarget::All, "2026-11-05", "zach")
        .unwrap();
    assert_eq!(done.len(), 2);
    loaded.save(&sidecar).unwrap();

    let r = run_and_save(&set, &fetcher, &sidecar, "2026-12-01").await;
    assert_eq!(r.counts.sections_changed, 0);
    assert_eq!(r.counts.sections_unchanged, 2);
}

#[tokio::test]
async fn editing_only_a_sibling_section_does_not_flag_the_cited_one() {
    let url = "https://docs.example/guide#idem";
    let set = rules(&[("R-IDEM", &[url])]);
    let fetcher = MapFetcher::new(&[("https://docs.example/guide", ok_page(PAGE_V1))]);
    let (_, s1) = run_check(&set, &fetcher, &SourceHashes::default(), &cfg("2026-10-01")).await;
    let sibling_only = PAGE_V1.replace("Unrelated text.", "Unrelated text, edited.");
    fetcher.set("https://docs.example/guide", ok_page(&sibling_only));
    let (r2, _) = run_check(&set, &fetcher, &s1, &cfg("2026-11-01")).await;
    assert_eq!(r2.counts.sections_unchanged, 1);
    assert!(r2.changed.is_empty());
}

#[tokio::test]
async fn several_anchors_on_one_page_cost_one_request() {
    let set = rules(&[
        ("R-A", &["https://docs.example/guide#idem"]),
        ("R-B", &["https://docs.example/guide#other"]),
    ]);
    let fetcher = MapFetcher::new(&[("https://docs.example/guide", ok_page(PAGE_V1))]);
    let (report, sidecar) =
        run_check(&set, &fetcher, &SourceHashes::default(), &cfg("2026-10-01")).await;
    assert_eq!(fetcher.calls.lock().unwrap().len(), 1);
    assert_eq!(report.counts.sections_new_baseline, 2);
    assert_eq!(sidecar.entries.len(), 2);
}

#[tokio::test]
async fn a_missing_anchor_is_reported_apart_and_keeps_the_old_baseline() {
    let url = "https://docs.example/guide#idem";
    let set = rules(&[("R-IDEM", &[url])]);
    let fetcher = MapFetcher::new(&[("https://docs.example/guide", ok_page(PAGE_V1))]);
    let (_, s1) = run_check(&set, &fetcher, &SourceHashes::default(), &cfg("2026-10-01")).await;
    let baseline = s1.entries[url].section_hash.clone();
    assert!(baseline.is_some());

    fetcher.set("https://docs.example/guide", ok_page("<h1>Rewritten</h1>"));
    let (r2, s2) = run_check(&set, &fetcher, &s1, &cfg("2026-11-01")).await;
    assert_eq!(r2.counts.sections_anchor_missing, 1);
    assert_eq!(
        r2.counts.sections_changed, 0,
        "a missing anchor is not a change"
    );
    assert_eq!(r2.anchor_missing[0].url, url);
    assert_eq!(s2.entries[url].section_hash, baseline);
}

#[tokio::test]
async fn unreachable_leaves_the_last_known_record_and_dead_keeps_the_section_baseline() {
    let url = "https://docs.example/guide#idem";
    let set = rules(&[("R-IDEM", &[url])]);
    let fetcher = MapFetcher::new(&[("https://docs.example/guide", ok_page(PAGE_V1))]);
    let (_, s1) = run_check(&set, &fetcher, &SourceHashes::default(), &cfg("2026-10-01")).await;

    fetcher.set(
        "https://docs.example/guide",
        FetchOutcome::Unreachable {
            detail: "timed out".to_string(),
        },
    );
    let (r2, s2) = run_check(&set, &fetcher, &s1, &cfg("2026-11-01")).await;
    assert_eq!(r2.counts.unreachable, 1);
    assert_eq!(r2.counts.dead, 0);
    assert_eq!(
        s2.entries[url], s1.entries[url],
        "unreachable changes nothing on record"
    );

    fetcher.set(
        "https://docs.example/guide",
        FetchOutcome::Dead {
            detail: "HTTP 404".to_string(),
        },
    );
    let (_, s3) = run_check(&set, &fetcher, &s2, &cfg("2026-12-01")).await;
    assert_eq!(s3.entries[url].link, LinkState::Dead);
    assert_eq!(s3.entries[url].section_hash, s1.entries[url].section_hash);
}

#[tokio::test]
async fn urls_no_longer_cited_are_dropped_from_the_sidecar() {
    let fetcher = MapFetcher::new(&[
        ("https://a.example/x", ok_page("a")),
        ("https://b.example/x", ok_page("b")),
    ]);
    let (_, s1) = run_check(
        &rules(&[("R", &["https://a.example/x", "https://b.example/x"])]),
        &fetcher,
        &SourceHashes::default(),
        &cfg("2026-10-01"),
    )
    .await;
    assert_eq!(s1.entries.len(), 2);
    let (_, s2) = run_check(
        &rules(&[("R", &["https://a.example/x"])]),
        &fetcher,
        &s1,
        &cfg("2026-11-01"),
    )
    .await;
    assert_eq!(
        s2.entries.keys().collect::<Vec<_>>(),
        vec!["https://a.example/x"]
    );
}

// ---------------------------------------------------------------------------------------------
// politeness
// ---------------------------------------------------------------------------------------------

/// Records the most requests ever in flight at once per host.
struct ConcurrencyProbe {
    in_flight: Mutex<BTreeMap<String, usize>>,
    max_per_host: Mutex<BTreeMap<String, usize>>,
    total: AtomicUsize,
}

#[async_trait]
impl Fetcher for ConcurrencyProbe {
    async fn fetch(&self, url: &str, _want_body: bool) -> FetchOutcome {
        let host = url.split('/').nth(2).unwrap().to_string();
        {
            let mut live = self.in_flight.lock().unwrap();
            let n = live.entry(host.clone()).or_default();
            *n += 1;
            let mut max = self.max_per_host.lock().unwrap();
            let m = max.entry(host.clone()).or_default();
            *m = (*m).max(*n);
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
        *self.in_flight.lock().unwrap().get_mut(&host).unwrap() -= 1;
        self.total.fetch_add(1, Ordering::SeqCst);
        ok_page("x")
    }
}

#[tokio::test]
async fn never_more_than_one_request_in_flight_per_host() {
    let mut spec: Vec<(String, Vec<String>)> = Vec::new();
    for host in ["a.example", "b.example", "c.example"] {
        for n in 0..5 {
            spec.push((
                format!("R-{host}-{n}"),
                vec![format!("https://{host}/page{n}")],
            ));
        }
    }
    let mut set = RuleSet::default();
    for (id, urls) in &spec {
        let mut r = bare_rule(id, "demo");
        r.sources = urls.iter().map(|u| source(u)).collect();
        set.push(r);
    }
    let probe = ConcurrencyProbe {
        in_flight: Mutex::new(BTreeMap::new()),
        max_per_host: Mutex::new(BTreeMap::new()),
        total: AtomicUsize::new(0),
    };
    let mut config = cfg("2026-10-01");
    config.delay = Duration::from_millis(2);
    run_check(&set, &probe, &SourceHashes::default(), &config).await;

    assert_eq!(probe.total.load(Ordering::SeqCst), 15);
    for (host, max) in probe.max_per_host.lock().unwrap().iter() {
        assert_eq!(*max, 1, "{host} saw overlapping requests");
    }
}

#[tokio::test]
async fn the_delay_between_requests_to_one_host_is_honoured() {
    let set = rules(&[(
        "R",
        &[
            "https://slow.example/1",
            "https://slow.example/2",
            "https://slow.example/3",
        ],
    )]);
    let fetcher = MapFetcher::new(&[
        ("https://slow.example/1", ok_page("1")),
        ("https://slow.example/2", ok_page("2")),
        ("https://slow.example/3", ok_page("3")),
    ]);
    let mut config = cfg("2026-10-01");
    config.delay = Duration::from_millis(60);
    let started = std::time::Instant::now();
    run_check(&set, &fetcher, &SourceHashes::default(), &config).await;
    assert!(
        started.elapsed() >= Duration::from_millis(120),
        "two gaps of 60ms between three requests to one host"
    );
}

// ---------------------------------------------------------------------------------------------
// the export seam
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn export_seam_lowers_confidence_for_dead_unreviewed_or_recently_accepted_sources_only() {
    let set = rules(&[
        ("R-FINE", &["https://fine.example/a"]),
        ("R-DEAD", &["https://dead.example/a"]),
        ("R-CHANGED", &["https://docs.example/guide#idem"]),
    ]);
    let fetcher = MapFetcher::new(&[
        ("https://fine.example/a", ok_page("fine")),
        (
            "https://dead.example/a",
            FetchOutcome::Dead {
                detail: "HTTP 404".to_string(),
            },
        ),
        ("https://docs.example/guide", ok_page(PAGE_V1)),
    ]);
    let (_, s1) = run_check(&set, &fetcher, &SourceHashes::default(), &cfg("2026-09-01")).await;
    fetcher.set("https://docs.example/guide", ok_page(PAGE_V2));
    let (_, s2) = run_check(&set, &fetcher, &s1, &cfg("2026-10-20")).await;

    let get = |id: &str, today: &str| {
        source_confidence(
            set.get_by_id(id).unwrap(),
            &s2,
            today,
            DEFAULT_CHANGED_WINDOW_DAYS,
        )
    };

    let fine = get("R-FINE", "2026-10-27");
    assert!(!fine.lowered);
    assert_eq!(fine.line().as_deref(), Some("Source re-checked 2026-10-20"));

    let dead = get("R-DEAD", "2026-10-27");
    assert!(dead.lowered);
    assert!(dead.reasons[0].contains("dead"));

    let changed = get("R-CHANGED", "2026-10-27");
    assert!(changed.lowered);
    assert!(changed.reasons[0].contains("not yet reviewed"));
    // An unreviewed change lowers confidence however old it is: the window does not apply.
    assert!(get("R-CHANGED", "2030-01-01").lowered);

    // Once accepted, the window runs from the accept date, not from when the change was seen.
    let mut accepted = s2.clone();
    accepted
        .accept(AcceptTarget::All, "2026-12-01", "zach")
        .unwrap();
    let after = |today: &str| {
        source_confidence(
            set.get_by_id("R-CHANGED").unwrap(),
            &accepted,
            today,
            DEFAULT_CHANGED_WINDOW_DAYS,
        )
    };
    let soon = after("2027-01-15");
    assert!(soon.lowered, "44 days after accepting");
    assert!(soon.reasons[0].contains("accepted 2026-12-01"));
    assert!(!after("2027-03-15").lowered, "104 days after accepting");

    let mut never_checked = bare_rule("R-UNSEEN", "demo");
    never_checked.sources = vec![source("https://never-checked.example/a")];
    let unseen = source_confidence(
        &never_checked,
        &s2,
        "2026-10-27",
        DEFAULT_CHANGED_WINDOW_DAYS,
    );
    assert!(!unseen.lowered, "no record means no claim either way");
    assert_eq!(unseen.line(), None);
}
