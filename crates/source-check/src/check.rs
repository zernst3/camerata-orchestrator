//! The check itself: collect every rule's `[[sources]]`, fetch each distinct page politely, judge
//! each cited URL, compare cited sections against the sidecar, and produce a [`SourceCheckReport`]
//! plus the sidecar to save. Nothing here writes to disk or edits a rule; the caller decides.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Duration;

use camerata_rules::RuleSet;
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};

use crate::diff::{diff_sections, DiffLine};
use crate::fetch::{FetchOutcome, Fetcher};
use crate::section::{extract_section, hash_text, split_fragment, SectionLookup};
use crate::sidecar::{
    cap_excerpt, LinkState, PendingChange, SourceHashes, SourceRecord, EXCERPT_CAP_CHARS,
};

/// Politeness and clock settings for one run.
#[derive(Debug, Clone)]
pub struct CheckConfig {
    /// The run date, `YYYY-MM-DD`, stamped on the report and the sidecar.
    pub today: String,
    /// Pause between two requests to the SAME host. Zero in tests.
    pub delay: Duration,
    /// How many different hosts are worked at once. Each host still gets one request at a time.
    pub max_parallel_hosts: usize,
}

/// A rule named in a report list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleRef {
    /// The rule id, e.g. `SEC-NO-HARDCODED-SECRETS-1`.
    pub id: String,
    /// The rule's corpus domain.
    pub domain: String,
    /// The rule's title.
    pub title: String,
}

/// A cited URL whose link state needs attention (dead, unreachable, redirected).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkFinding {
    /// The URL exactly as the rule cites it.
    pub url: String,
    /// Its state.
    pub state: LinkState,
    /// What happened ("HTTP 404", "timed out"); empty for a plain redirect.
    pub detail: String,
    /// Where a redirect ended up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_url: Option<String>,
    /// The rules that cite this URL, sorted.
    pub rules: Vec<String>,
}

/// A cited section whose text differs from the last ACCEPTED text and has not been reviewed. It
/// is listed on every run until it is accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionChange {
    /// The cited URL, anchor included.
    pub url: String,
    /// The rules that cite it, sorted.
    pub rules: Vec<String>,
    /// Date (YYYY-MM-DD) of the first run that saw the section differ.
    pub first_seen: String,
    /// Hash of the last accepted text.
    pub old_hash: String,
    /// Hash of the newest text seen.
    pub new_hash: String,
    /// What was removed and added, in reading order.
    pub diff: Vec<DiffLine>,
    /// True when the section is longer than the stored excerpt, so a change past the excerpt
    /// shows in the hash but not in the diff.
    pub beyond_excerpt: bool,
}

/// A cited anchor the fetched page does not contain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingAnchor {
    /// The cited URL, anchor included.
    pub url: String,
    /// The rules that cite it, sorted.
    pub rules: Vec<String>,
}

/// The numbers at the top of the report. URL counts are per distinct cited URL.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    /// Rules in the corpus.
    pub rules: usize,
    /// Rules with an empty `[[sources]]` list.
    pub rules_without_source: usize,
    /// Rules that have sources but none is a web URL (all are internal documents).
    pub rules_internal_sources_only: usize,
    /// Distinct cited web URLs.
    pub web_urls: usize,
    /// Cited sources that are not web URLs (repo documents) and were not fetched.
    pub internal_sources_skipped: usize,
    /// URLs that answered directly.
    pub ok: usize,
    /// URLs that answered after a redirect.
    pub redirected: usize,
    /// URLs the server says are gone.
    pub dead: usize,
    /// URLs that could not be reached this run (not a verdict on the page).
    pub unreachable: usize,
    /// Cited URLs carrying an anchor whose section was looked up.
    pub sections_checked: usize,
    /// Sections whose text matches the last run.
    pub sections_unchanged: usize,
    /// Sections whose text differs from the last accepted text and are not yet reviewed
    /// (including ones carried over from earlier runs, or whose page did not answer this run).
    pub sections_changed: usize,
    /// Sections seen for the first time (a baseline was recorded, nothing to compare).
    pub sections_new_baseline: usize,
    /// Anchors the fetched page did not contain.
    pub sections_anchor_missing: usize,
}

/// The full, date-stamped result of one run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCheckReport {
    /// The run date, `YYYY-MM-DD`.
    pub checked_on: String,
    /// The headline numbers.
    pub counts: Counts,
    /// URLs the server says are gone.
    pub dead: Vec<LinkFinding>,
    /// Sections changed since the last accepted text and not yet reviewed.
    pub changed: Vec<SectionChange>,
    /// Rules with no `[[sources]]` at all (ungrounded).
    pub rules_without_source: Vec<RuleRef>,
    /// Rules whose only sources are internal documents.
    pub rules_internal_sources_only: Vec<RuleRef>,
    /// URLs that moved; the rule's source should be updated to `final_url`.
    pub redirected: Vec<LinkFinding>,
    /// URLs that could not be reached this run.
    pub unreachable: Vec<LinkFinding>,
    /// Cited anchors missing from the fetched page (may be a JavaScript-rendered page).
    pub anchor_missing: Vec<MissingAnchor>,
}

/// Whether a cited source is a fetchable web URL.
fn is_web_url(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Every cited web URL mapped to the sorted rule ids citing it, plus the rule lists and the
/// internal-source count.
struct Collected {
    urls: BTreeMap<String, BTreeSet<String>>,
    without_source: Vec<RuleRef>,
    internal_only: Vec<RuleRef>,
    internal_skipped: usize,
    rule_count: usize,
}

fn collect(rules: &RuleSet) -> Collected {
    let mut c = Collected {
        urls: BTreeMap::new(),
        without_source: Vec::new(),
        internal_only: Vec::new(),
        internal_skipped: 0,
        rule_count: 0,
    };
    for rule in rules.iter() {
        c.rule_count += 1;
        let rule_ref = || RuleRef {
            id: rule.id.0.clone(),
            domain: rule.domain.clone(),
            title: rule.title.clone(),
        };
        if rule.sources.is_empty() {
            c.without_source.push(rule_ref());
            continue;
        }
        let mut any_web = false;
        for source in &rule.sources {
            let url = source.url.trim();
            if is_web_url(url) {
                any_web = true;
                c.urls
                    .entry(url.to_string())
                    .or_default()
                    .insert(rule.id.0.clone());
            } else {
                c.internal_skipped += 1;
            }
        }
        if !any_web {
            c.internal_only.push(rule_ref());
        }
    }
    c.without_source.sort_by(|a, b| a.id.cmp(&b.id));
    c.internal_only.sort_by(|a, b| a.id.cmp(&b.id));
    c
}

/// Host (with port) a page URL lives on, the unit of politeness.
fn host_key(page_url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(page_url).ok()?;
    let host = parsed.host_str()?;
    Some(match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

/// Fetch every distinct page once. Pages on one host are fetched one at a time with `delay`
/// between them; different hosts run side by side, up to `max_parallel_hosts`.
async fn fetch_pages(
    fetcher: &dyn Fetcher,
    pages: &BTreeMap<String, bool>,
    cfg: &CheckConfig,
) -> HashMap<String, FetchOutcome> {
    let mut by_host: BTreeMap<String, Vec<(String, bool)>> = BTreeMap::new();
    let mut outcomes = HashMap::new();
    for (page, &want_body) in pages {
        match host_key(page) {
            Some(host) => by_host
                .entry(host)
                .or_default()
                .push((page.clone(), want_body)),
            None => {
                outcomes.insert(
                    page.clone(),
                    FetchOutcome::Dead {
                        detail: "malformed URL".to_string(),
                    },
                );
            }
        }
    }
    let delay = cfg.delay;
    let results: Vec<Vec<(String, FetchOutcome)>> = stream::iter(by_host.into_values())
        .map(|queue| async move {
            let mut done = Vec::with_capacity(queue.len());
            for (n, (page, want_body)) in queue.into_iter().enumerate() {
                if n > 0 && !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                let outcome = fetcher.fetch(&page, want_body).await;
                done.push((page, outcome));
            }
            done
        })
        .buffer_unordered(cfg.max_parallel_hosts.max(1))
        .collect()
        .await;
    outcomes.extend(results.into_iter().flatten());
    outcomes
}

/// Judge one cited URL against its fetched page and the previous record, updating the report and
/// the new sidecar entry.
struct Judge<'a> {
    prev: &'a SourceHashes,
    today: &'a str,
    report: SourceCheckReport,
    next: BTreeMap<String, SourceRecord>,
}

impl Judge<'_> {
    fn judge(&mut self, url: &str, rules: &BTreeSet<String>, outcome: &FetchOutcome) {
        let rules: Vec<String> = rules.iter().cloned().collect();
        let prev = self.prev.entries.get(url);
        let (_, fragment) = split_fragment(url);
        let counts = &mut self.report.counts;
        match outcome {
            FetchOutcome::Unreachable { detail } => {
                counts.unreachable += 1;
                self.report.unreachable.push(LinkFinding {
                    url: url.to_string(),
                    state: LinkState::Unreachable,
                    detail: detail.clone(),
                    final_url: None,
                    rules,
                });
                // Unknown this run: keep whatever the last definitive check recorded.
                if let Some(prev) = prev {
                    self.next.insert(url.to_string(), prev.clone());
                }
            }
            FetchOutcome::Dead { detail } => {
                counts.dead += 1;
                self.report.dead.push(LinkFinding {
                    url: url.to_string(),
                    state: LinkState::Dead,
                    detail: detail.clone(),
                    final_url: None,
                    rules,
                });
                // A page that is down today keeps its section baseline for when it returns.
                let mut record = blank_record(self.today, LinkState::Dead);
                carry_section(&mut record, prev);
                self.next.insert(url.to_string(), record);
            }
            FetchOutcome::Ok {
                final_url,
                redirected,
                body,
            } => {
                let mut record = blank_record(
                    self.today,
                    if *redirected {
                        LinkState::Redirected
                    } else {
                        LinkState::Ok
                    },
                );
                if *redirected {
                    counts.redirected += 1;
                    record.redirected_to = Some(final_url.clone());
                    self.report.redirected.push(LinkFinding {
                        url: url.to_string(),
                        state: LinkState::Redirected,
                        detail: String::new(),
                        final_url: Some(final_url.clone()),
                        rules: rules.clone(),
                    });
                } else {
                    counts.ok += 1;
                }
                carry_section(&mut record, prev);
                if let (Some(anchor), Some(body)) = (fragment, body) {
                    self.judge_section(url, anchor, body, &rules, prev, &mut record);
                }
                self.next.insert(url.to_string(), record);
            }
        }
    }

    fn judge_section(
        &mut self,
        url: &str,
        anchor: &str,
        body: &str,
        rules: &[String],
        prev: Option<&SourceRecord>,
        record: &mut SourceRecord,
    ) {
        let counts = &mut self.report.counts;
        counts.sections_checked += 1;
        let SectionLookup::Found(text) = extract_section(body, anchor) else {
            counts.sections_anchor_missing += 1;
            self.report.anchor_missing.push(MissingAnchor {
                url: url.to_string(),
                rules: rules.to_vec(),
            });
            return; // the old baseline stays in `record`
        };
        let new_hash = hash_text(&text);
        // `record` already carries the accepted baseline (and any pending change) from `prev`.
        match record.section_hash.clone() {
            None => {
                counts.sections_new_baseline += 1;
                record.section_hash = Some(new_hash);
                record.section_excerpt = Some(cap_excerpt(&text));
            }
            Some(baseline) if baseline == new_hash => {
                // Same as the accepted text, including a page that reverted a pending change.
                counts.sections_unchanged += 1;
                record.pending = None;
            }
            Some(_) => {
                // Differs from the accepted baseline: record it as pending and leave the
                // baseline alone. `run_check` lists it on the report.
                let first_seen = prev
                    .and_then(|p| p.pending.as_ref())
                    .map_or_else(|| self.today.to_string(), |p| p.first_seen.clone());
                record.pending = Some(PendingChange {
                    hash: new_hash,
                    excerpt: cap_excerpt(&text),
                    first_seen,
                });
            }
        }
    }
}

fn blank_record(today: &str, link: LinkState) -> SourceRecord {
    SourceRecord {
        last_checked: today.to_string(),
        link,
        redirected_to: None,
        section_hash: None,
        section_excerpt: None,
        pending: None,
        accepted: None,
    }
}

/// Carry the section review state (accepted baseline, pending change, acceptance) from the
/// previous record, so a link problem or a missing anchor never erases it.
fn carry_section(record: &mut SourceRecord, prev: Option<&SourceRecord>) {
    if let Some(prev) = prev {
        record.section_hash.clone_from(&prev.section_hash);
        record.section_excerpt.clone_from(&prev.section_excerpt);
        record.pending.clone_from(&prev.pending);
        record.accepted.clone_from(&prev.accepted);
    }
}

/// Run the check. Returns the report and the sidecar as it should be saved; the caller decides
/// whether to save it. URLs no longer cited by any rule are dropped from the new sidecar.
pub async fn run_check(
    rules: &RuleSet,
    fetcher: &dyn Fetcher,
    prev: &SourceHashes,
    cfg: &CheckConfig,
) -> (SourceCheckReport, SourceHashes) {
    let collected = collect(rules);

    // One fetch per distinct page (the URL without its fragment); the body is kept only when some
    // cited URL on that page carries an anchor.
    let mut pages: BTreeMap<String, bool> = BTreeMap::new();
    for url in collected.urls.keys() {
        let (page, fragment) = split_fragment(url);
        let want_body = pages.entry(page.to_string()).or_insert(false);
        *want_body |= fragment.is_some();
    }
    let outcomes = fetch_pages(fetcher, &pages, cfg).await;

    let mut judge = Judge {
        prev,
        today: &cfg.today,
        report: SourceCheckReport {
            checked_on: cfg.today.clone(),
            counts: Counts {
                rules: collected.rule_count,
                rules_without_source: collected.without_source.len(),
                rules_internal_sources_only: collected.internal_only.len(),
                web_urls: collected.urls.len(),
                internal_sources_skipped: collected.internal_skipped,
                ..Counts::default()
            },
            rules_without_source: collected.without_source.clone(),
            rules_internal_sources_only: collected.internal_only.clone(),
            ..SourceCheckReport::default()
        },
        next: BTreeMap::new(),
    };
    for (url, citing) in &collected.urls {
        let (page, _) = split_fragment(url);
        let outcome = outcomes
            .get(page)
            .cloned()
            .unwrap_or(FetchOutcome::Unreachable {
                detail: "not fetched".to_string(),
            });
        judge.judge(url, citing, &outcome);
    }

    let Judge {
        mut report, next, ..
    } = judge;
    // Every section with an unreviewed change is listed, whether or not the page answered this
    // run: a change stays on the report until someone accepts it.
    for (url, record) in &next {
        let Some(pending) = &record.pending else {
            continue;
        };
        let citing = collected.urls.get(url).map(|r| r.iter().cloned().collect());
        report.counts.sections_changed += 1;
        report.changed.push(SectionChange {
            url: url.clone(),
            rules: citing.unwrap_or_default(),
            first_seen: pending.first_seen.clone(),
            old_hash: record.section_hash.clone().unwrap_or_default(),
            new_hash: pending.hash.clone(),
            diff: diff_sections(
                record.section_excerpt.as_deref().unwrap_or_default(),
                &pending.excerpt,
            ),
            beyond_excerpt: pending.excerpt.chars().count() >= EXCERPT_CAP_CHARS,
        });
    }
    report.dead.sort_by(|a, b| a.url.cmp(&b.url));
    report.redirected.sort_by(|a, b| a.url.cmp(&b.url));
    report.unreachable.sort_by(|a, b| a.url.cmp(&b.url));
    report.changed.sort_by(|a, b| a.url.cmp(&b.url));
    report.anchor_missing.sort_by(|a, b| a.url.cmp(&b.url));
    let sidecar = SourceHashes {
        schema_version: prev.schema_version,
        entries: next,
    };
    (report, sidecar)
}
