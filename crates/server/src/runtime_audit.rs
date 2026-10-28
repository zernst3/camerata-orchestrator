//! Runtime and framework end of life, and known runtime vulnerabilities.
//!
//! The dependency audit (osv-scanner, [`crate::dep_audit`]) sees the packages in a lockfile. It
//! cannot see the runtime those packages run on (Node, Python, PHP, Ruby, Go, Java, .NET, nginx)
//! or tell that a runtime or a framework is past its end of life. This pass reads the versions a
//! repository declares ([`detect`]) and checks them against two public feeds:
//!
//! - **endoflife.date** ([`eol`]): a finding when the declared release line has passed its end of
//!   life, and a low advisory when it ends within [`WARN_DAYS`] days. Runtimes and the major
//!   frameworks (Next.js, React, Django, Rails, Laravel, Spring Boot, Express).
//! - **NVD** ([`nvd`]): the known vulnerabilities of a runtime at an EXACT version, found by CPE.
//!   One finding per runtime version, rated by its highest CVSS score with the dependency audit's
//!   scorer. KEV and EPSS are added afterwards by [`crate::vuln_intel::enrich_dependency_findings`]
//!   exactly as they are for dependency findings (a known-exploited runtime vulnerability is
//!   floored to critical).
//!
//! # Honest about what was not checked
//!
//! - A version that is a range or a moving tag (`>=18`, `lts/*`, `node:latest`, a build argument)
//!   is never guessed. It is listed under Coverage as not checked.
//! - A runtime declared as a release line without a patch version (`node:18`) gets an end-of-life
//!   check but no vulnerability check, because a vulnerability is fixed in a specific patch
//!   release. Coverage says so.
//! - A feed that cannot be fetched is a Coverage line ("runtime end-of-life data unavailable in
//!   this scan", "runtime vulnerability data unavailable in this scan") and a "not checked" stamp.
//!   The pass never prints a clean claim for what it did not check, and a clean result adds no
//!   claim at all: no finding is the only output of a clean runtime.
//!
//! # Feed rules
//!
//! The same as the KEV and EPSS feeds ([`crate::vuln_intel`]): the fetch seam is
//! [`crate::vuln_intel::FeedFetcher`], each answer is cached with its date and used for up to
//! [`crate::vuln_intel::MAX_CACHE_AGE_DAYS`] days when the live fetch fails, and the stamp says when
//! the data was current. NVD's anonymous rate limit is respected ([`nvd::NvdPolicy`]); running into
//! it never fails the scan.
//!
//! # Corpus
//!
//! The two rules that carry these findings are [`EOL_RULE_ID`] and [`VULN_RULE_ID`]. They are
//! emit-only (`inspectable = false`): they are never handed to the AI pass or armed in the scan
//! ledger, and so can never be reported "verified clean". Findings come only from this module.

mod detect;
mod eol;
mod nvd;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use futures::future::join_all;

use crate::onboard::{CoverageNote, Finding, MergedLocation, ScanReport};
use crate::vuln_intel::feeds::{self, FeedState};
use crate::vuln_intel::{
    FeedFetcher, FeedStamp, HttpFetcher, Unreachable, VulnDataStamp, CVE_IDS_CAPTURE,
    ENRICHMENT_BUDGET,
};

pub use detect::{Declaration, Product, Version};
pub use eol::{EolCycle, EolDate, EolFeed};
pub use nvd::{NvdCve, NvdPolicy, NvdResult};

/// Corpus rule for a runtime or framework past its end of life.
pub const EOL_RULE_ID: &str = "SEC-NO-END-OF-LIFE-RUNTIME-1";
/// Corpus rule for a runtime version with known vulnerabilities.
pub const VULN_RULE_ID: &str = "SEC-NO-KNOWN-VULNERABLE-RUNTIME-1";

/// Coverage-note tool name: end-of-life data, and versions that were not a single release.
pub const EOL_NOTE_TOOL: &str = "runtime-eol";
/// Coverage-note tool name: runtime vulnerability data.
pub const VULN_NOTE_TOOL: &str = "runtime-vulnerabilities";
/// Whether a coverage note's tool belongs to this pass (the report writes its own message).
pub fn is_runtime_note_tool(tool: &str) -> bool {
    tool == EOL_NOTE_TOOL || tool == VULN_NOTE_TOOL
}

/// An end of life this many days away or fewer is an advisory.
pub const WARN_DAYS: i64 = 90;

/// How long the NVD lookups may take in all, including the pauses that keep them under the
/// anonymous rate limit.
pub const NVD_BUDGET: Duration = Duration::from_secs(75);

/// The ledger stage name this pass's additions are recorded under.
pub const LEDGER_STAGE: &str = "runtime-audit-merge";

const REASON_NOTHING_DECLARED: &str = "no runtime or framework version was found in the repository";
const REASON_ALL_EXCLUDED: &str = "every version found was excluded (see Coverage)";
const REASON_NO_EXACT_RUNTIME: &str =
    "no runtime version was declared with a patch release, which a vulnerability lookup needs";
const REASON_DISABLED: &str = "the dependency audit was disabled for this run";

/// Capture: the runtime or framework and release line, for example `Node.js 16`.
pub const RUNTIME_CAPTURE: &str = "runtime";
/// Capture: the declaration as written.
pub const DECLARED_CAPTURE: &str = "runtime_declared";
/// Capture: the date the release line stops (or stopped) receiving security fixes.
pub const EOL_DATE_CAPTURE: &str = "runtime_eol_date";

/// The most names listed in one Coverage line before the rest are counted.
const LISTED: usize = 6;

/// Everything one pass produced.
#[derive(Debug)]
pub struct RuntimeAudit {
    pub findings: Vec<Finding>,
    pub notes: Vec<CoverageNote>,
    pub eol: FeedStamp,
    pub nvd: FeedStamp,
}

// ─── wording ─────────────────────────────────────────────────────────────────

/// `a`, `a and b`, `a, b and c`: no serial comma.
fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// At most [`LISTED`] names, then "and N more".
fn join_capped(names: &[String]) -> String {
    if names.len() <= LISTED {
        return join_names(names);
    }
    let shown = &names[..LISTED];
    format!("{} and {} more", shown.join(", "), names.len() - LISTED)
}

fn where_declared(d: &Declaration) -> String {
    format!("{} line {}", d.path, d.line)
}

fn describe(d: &Declaration) -> String {
    format!(
        "{} declared as `{}` in {}",
        d.product.label(),
        d.raw,
        where_declared(d)
    )
}

fn note(tool: &str, message: String) -> CoverageNote {
    CoverageNote {
        tool: tool.to_string(),
        message,
    }
}

fn utc_day(now: DateTime<Utc>) -> NaiveDate {
    now.date_naive()
}

fn day_string(now: DateTime<Utc>) -> String {
    now.format("%Y-%m-%d").to_string()
}

// ─── feed handling ───────────────────────────────────────────────────────────

fn cache_file(cache_dir: Option<&Path>, name: &str) -> Option<PathBuf> {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    cache_dir.map(|d| d.join(format!("{safe}.json")))
}

async fn fetch_eol(fetcher: &dyn FeedFetcher, product: Product) -> anyhow::Result<EolFeed> {
    EolFeed::parse(&fetcher.get(&eol::product_url(product.slug())).await?)
}

/// A stamp for a set of per-item feed states: `Checked` with the oldest date and the notes, or
/// `NotChecked` when none of them could be used.
struct Tally {
    checked: usize,
    missing: Vec<String>,
    cached: Vec<(String, String)>,
    reason: Option<String>,
}

impl Tally {
    fn new() -> Self {
        Self {
            checked: 0,
            missing: Vec::new(),
            cached: Vec::new(),
            reason: None,
        }
    }

    fn add<T>(&mut self, name: &str, state: &FeedState<T>) {
        match state {
            FeedState::Checked { cached_on, .. } => {
                self.checked += 1;
                if let Some(date) = cached_on {
                    self.cached.push((name.to_string(), date.clone()));
                }
            }
            FeedState::NotChecked { reason } => {
                self.missing.push(name.to_string());
                self.reason.get_or_insert_with(|| reason.clone());
            }
        }
    }

    fn stamp(&self, live_date: String) -> FeedStamp {
        if self.checked == 0 {
            return FeedStamp::NotChecked {
                reason: self
                    .reason
                    .clone()
                    .unwrap_or_else(|| feeds::REASON_UNAVAILABLE.to_string()),
            };
        }
        let as_of = self
            .cached
            .iter()
            .map(|(_, d)| d.clone())
            .min()
            .unwrap_or(live_date);
        let mut notes: Vec<String> = Vec::new();
        if !self.cached.is_empty() {
            let names: Vec<String> = self.cached.iter().map(|(n, _)| n.clone()).collect();
            notes.push(format!(
                "cached copy fetched {as_of} UTC for {}; the live feed could not be fetched",
                join_names(&names)
            ));
        }
        if !self.missing.is_empty() {
            notes.push(format!("not checked for {}", join_names(&self.missing)));
        }
        FeedStamp::Checked {
            as_of,
            note: (!notes.is_empty()).then(|| notes.join("; ")),
        }
    }
}

// ─── findings ────────────────────────────────────────────────────────────────

fn merged_locations(
    repo: &str,
    rule_id: &str,
    severity: &str,
    snippet: &str,
    rest: &[&Declaration],
) -> Vec<MergedLocation> {
    rest.iter()
        .map(|d| {
            MergedLocation::site(repo, &d.path, d.line, rule_id, snippet, false)
                .with_rating(severity, snippet)
        })
        .collect()
}

fn also_declared(rest: &[&Declaration]) -> String {
    if rest.is_empty() {
        return String::new();
    }
    let places: Vec<String> = rest.iter().map(|d| where_declared(d)).collect();
    format!(" It is also declared in {}.", join_capped(&places))
}

#[allow(clippy::too_many_arguments)]
fn finding(
    repo: &str,
    rule_id: &str,
    severity: &str,
    primary: &Declaration,
    rest: &[&Declaration],
    snippet: String,
    detail: String,
    fix: String,
    captures: BTreeMap<String, String>,
) -> Finding {
    let mut f = Finding {
        repo: repo.to_string(),
        path: primary.path.clone(),
        line: primary.line,
        rule_id: rule_id.to_string(),
        kind: Some(camerata_api_types::audit::FindingKind::Defect),
        severity: severity.to_string(),
        also_locations: merged_locations(repo, rule_id, severity, &snippet, rest),
        snippet,
        detail,
        fix_specific: Some(fix),
        captures,
        ..Finding::default()
    };
    crate::onboard::redact::redact_finding(&mut f);
    f
}

/// Declarations of one thing in one repo, in a stable order, first one primary.
fn split_primary<'a>(decls: &'a [&'a Declaration]) -> (&'a Declaration, Vec<&'a Declaration>) {
    let mut sorted: Vec<&Declaration> = decls.to_vec();
    sorted.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    let primary = sorted.remove(0);
    (primary, sorted)
}

fn eol_finding(
    repo: &str,
    product: Product,
    cycle: &EolCycle,
    standing: &eol::Standing,
    next: Option<&str>,
    decls: &[&Declaration],
) -> Option<Finding> {
    let (primary, rest) = split_primary(decls);
    let name = format!("{} {}", product.label(), cycle.cycle);
    let next_clause = next
        .map(|n| {
            format!(
                " The nearest release line that still receives security fixes is {} {n}.",
                product.label()
            )
        })
        .unwrap_or_default();
    let declared = format!(
        "It is declared as `{}` in {}.",
        primary.raw,
        where_declared(primary)
    );
    let also = also_declared(&rest);
    let (severity, detail, eol_date) = match standing {
        eol::Standing::Ended { since: Some(date) } => (
            "high",
            format!(
                "{name} has received no security fixes since {date}. {declared}{also}{next_clause}"
            ),
            date.clone(),
        ),
        eol::Standing::Ended { since: None } => (
            "high",
            format!(
                "{name} is past its end of life and no longer receives security fixes. \
                 {declared}{also}{next_clause}"
            ),
            String::new(),
        ),
        eol::Standing::EndsSoon { on, days } => (
            "low",
            format!(
                "Advisory: {name} reaches its end of life on {on}, in {days} days, and will \
                 receive no security fixes after that. {declared}{also}{next_clause}"
            ),
            on.clone(),
        ),
        eol::Standing::Supported => return None,
    };
    let mut captures = BTreeMap::new();
    captures.insert(RUNTIME_CAPTURE.to_string(), name.clone());
    captures.insert(DECLARED_CAPTURE.to_string(), primary.raw.clone());
    if !eol_date.is_empty() {
        captures.insert(EOL_DATE_CAPTURE.to_string(), eol_date);
    }
    let fix = match next {
        Some(n) => format!(
            "Move {} to {} {n} or a later release line and update {}.",
            name,
            product.label(),
            where_declared(primary)
        ),
        None => format!(
            "Move {name} to a release line that still receives security fixes and update {}.",
            where_declared(primary)
        ),
    };
    Some(finding(
        repo,
        EOL_RULE_ID,
        severity,
        primary,
        &rest,
        format!("{name} ({})", primary.raw),
        detail,
        fix,
        captures,
    ))
}

fn vuln_finding(
    repo: &str,
    product: Product,
    version: &str,
    result: &NvdResult,
    decls: &[&Declaration],
) -> Finding {
    let (primary, rest) = split_primary(decls);
    let name = format!("{} {version}", product.label());
    let mut cves = result.cves.clone();
    // Highest score first; unrated last; the id breaks ties so the order is stable.
    cves.sort_by(|a, b| {
        b.score
            .unwrap_or(-1.0)
            .total_cmp(&a.score.unwrap_or(-1.0))
            .then_with(|| a.id.cmp(&b.id))
    });
    let rated = cves.iter().any(|c| c.rated);
    let severity = if rated {
        cves.iter()
            .filter(|c| c.rated)
            .map(|c| c.severity.as_str())
            .max_by_key(|s| severity_rank(s))
            .unwrap_or("medium")
            .to_string()
    } else {
        "medium".to_string()
    };
    let count = cves.len();
    let top: Vec<String> = cves
        .iter()
        .take(3)
        .map(|c| match c.score {
            Some(score) => format!("{} (CVSS {score:.1}, {})", c.id, c.severity),
            None => format!("{} (not yet rated)", c.id),
        })
        .collect();
    // "A, B and C", or "A, B, C and 4 more": never a serial comma.
    let (listed, more) = if count > top.len() {
        (top.join(", "), format!(" and {} more", count - top.len()))
    } else {
        (join_names(&top), String::new())
    };
    let unrated = cves.iter().filter(|c| !c.rated).count();
    let unrated_clause = if rated && unrated > 0 {
        format!(
            " {unrated} of them {} no rating from NVD yet.",
            if unrated == 1 { "has" } else { "have" }
        )
    } else {
        String::new()
    };
    let hold = if rated {
        String::new()
    } else {
        " Severity unknown, held for review: NVD has published no rating for these entries, so \
         medium is shown as a placeholder."
            .to_string()
    };
    let detail = format!(
        "{name} has {count} known {} recorded in the National Vulnerability Database. \
         Highest rated: {}{more}.{unrated_clause} It is declared as `{}` in {}.{}{hold} Upgrade \
         to the latest patch release of this line or move to a supported release line.",
        if count == 1 {
            "vulnerability"
        } else {
            "vulnerabilities"
        },
        listed,
        primary.raw,
        where_declared(primary),
        also_declared(&rest),
    );
    let mut captures = BTreeMap::new();
    captures.insert(RUNTIME_CAPTURE.to_string(), name.clone());
    captures.insert(DECLARED_CAPTURE.to_string(), primary.raw.clone());
    captures.insert(
        CVE_IDS_CAPTURE.to_string(),
        cves.iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>()
            .join(","),
    );
    let fix = format!(
        "Upgrade {name} to the latest patch release of its line, or to a supported release line, \
         and update {}.",
        where_declared(primary)
    );
    let mut f = finding(
        repo,
        VULN_RULE_ID,
        &severity,
        primary,
        &rest,
        format!("{name} ({})", primary.raw),
        detail,
        fix,
        captures,
    );
    if !rated {
        f.needs_review = true;
        f.confidence = Some("needs-review".to_string());
    }
    f
}

fn severity_rank(severity: &str) -> u8 {
    match severity {
        "critical" => 4,
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

// ─── the pass ────────────────────────────────────────────────────────────────

/// One declaration with the repo it was found in.
struct Found {
    repo: String,
    decl: Declaration,
}

/// Run the pass over `sources` (repo label and folder) against the feeds behind `fetcher`.
///
/// `eol_budget` bounds the end-of-life fetches (they run together); [`NVD_BUDGET`] bounds the NVD
/// lookups (they run one after another, `policy.spacing` apart).
pub async fn audit_runtimes(
    sources: &[(String, PathBuf)],
    fetcher: &dyn FeedFetcher,
    cache_dir: Option<&Path>,
    now: DateTime<Utc>,
    policy: &NvdPolicy,
    eol_budget: Duration,
    nvd_budget: Duration,
) -> RuntimeAudit {
    let mut notes: Vec<CoverageNote> = Vec::new();

    // 1. What the repositories declare.
    let mut found: Vec<Found> = Vec::new();
    let mut walk_stopped: Vec<String> = Vec::new();
    for (repo, dir) in sources {
        let detected = detect::detect_declarations(dir);
        if detected.truncated {
            walk_stopped.push(repo.clone());
        }
        found.extend(detected.declarations.into_iter().map(|decl| Found {
            repo: repo.clone(),
            decl,
        }));
    }
    if !walk_stopped.is_empty() {
        notes.push(note(
            EOL_NOTE_TOOL,
            format!(
                "The search for runtime and framework versions stopped at its file limit in {}, \
                 so some version files were not read.",
                join_names(&walk_stopped)
            ),
        ));
    }

    // 2. Versions that are not a single release are listed, never guessed.
    let mut loose: Vec<String> = Vec::new();
    for f in &found {
        if let Version::Loose(why) = &f.decl.version {
            let line = format!("{} ({why})", describe(&f.decl));
            if !loose.contains(&line) {
                loose.push(line);
            }
        }
    }
    if !loose.is_empty() {
        notes.push(note(
            EOL_NOTE_TOOL,
            format!(
                "Not checked, because the declared version is not a single release: {}.",
                join_capped(&loose)
            ),
        ));
    }
    // Read but deliberately not checked (a build stage, a framework that is not a direct
    // dependency): one sentence per reason.
    let mut excluded: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for f in &found {
        if let Version::Excluded(why) = &f.decl.version {
            let line = describe(&f.decl);
            let list = excluded.entry(why.clone()).or_default();
            if !list.contains(&line) {
                list.push(line);
            }
        }
    }
    for (why, list) in &excluded {
        notes.push(note(
            EOL_NOTE_TOOL,
            format!("Not checked, because {why}: {}.", join_capped(list)),
        ));
    }

    // 3. End of life.
    let known: Vec<&Found> = found
        .iter()
        .filter(|f| !matches!(f.decl.version, Version::Loose(_) | Version::Excluded(_)))
        .collect();
    let mut products: Vec<Product> = known.iter().map(|f| f.decl.product).collect();
    products.sort();
    products.dedup();

    let fetched = join_all(products.iter().map(|&p| async move {
        let result = crate::vuln_intel::within_budget(eol_budget, fetch_eol(fetcher, p)).await;
        (p, result)
    }))
    .await;
    let mut eol_tally = Tally::new();
    let mut feeds_by_product: BTreeMap<Product, FeedState<EolFeed>> = BTreeMap::new();
    for (product, result) in fetched {
        let path = cache_file(cache_dir, &format!("eol-{}", product.slug()));
        let state = feeds::resolve(result, path.as_deref(), now, |f: &EolFeed| {
            !f.cycles.is_empty()
        });
        eol_tally.add(product.label(), &state);
        feeds_by_product.insert(product, state);
    }
    let eol_stamp = if products.is_empty() {
        // Versions that were read and then left out are not the same as versions never found.
        let every_one_excluded = !found.is_empty()
            && found
                .iter()
                .all(|f| matches!(f.decl.version, Version::Excluded(_)));
        FeedStamp::NotChecked {
            reason: if every_one_excluded {
                REASON_ALL_EXCLUDED
            } else {
                REASON_NOTHING_DECLARED
            }
            .to_string(),
        }
    } else {
        eol_tally.stamp(day_string(now))
    };
    if !eol_tally.missing.is_empty() {
        notes.push(note(
            EOL_NOTE_TOOL,
            format!(
                "Runtime end-of-life data unavailable in this scan: endoflife.date could not be \
                 reached for {}. This report does not say whether those are still supported.",
                join_names(&eol_tally.missing)
            ),
        ));
    }

    let today = utc_day(now);
    let mut groups: BTreeMap<(String, Product, String), Vec<&Declaration>> = BTreeMap::new();
    let mut too_broad: Vec<String> = Vec::new();
    let mut unlisted: Vec<String> = Vec::new();
    let mut unreadable: Vec<String> = Vec::new();
    for f in &known {
        let (Version::Exact(v) | Version::Release(v)) = &f.decl.version else {
            continue;
        };
        let Some(FeedState::Checked { data, .. }) = feeds_by_product.get(&f.decl.product) else {
            continue;
        };
        match data.cycle_for(v) {
            eol::CycleMatch::Found(cycle) if matches!(cycle.eol, EolDate::Unreadable(_)) => {
                unreadable.push(format!("{} {}", f.decl.product.label(), cycle.cycle));
            }
            eol::CycleMatch::Found(cycle) => groups
                .entry((f.repo.clone(), f.decl.product, cycle.cycle.clone()))
                .or_default()
                .push(&f.decl),
            eol::CycleMatch::TooBroad => too_broad.push(describe(&f.decl)),
            eol::CycleMatch::Unlisted => unlisted.push(describe(&f.decl)),
        }
    }
    if !unreadable.is_empty() {
        unreadable.sort();
        unreadable.dedup();
        notes.push(note(
            EOL_NOTE_TOOL,
            format!(
                "Not checked, because endoflife.date gives no readable end-of-life date for {}.",
                join_names(&unreadable)
            ),
        ));
    }
    if !too_broad.is_empty() {
        too_broad.sort();
        too_broad.dedup();
        notes.push(note(
            EOL_NOTE_TOOL,
            format!(
                "Not checked, because the declared version names several release lines: {}.",
                join_capped(&too_broad)
            ),
        ));
    }
    if !unlisted.is_empty() {
        unlisted.sort();
        unlisted.dedup();
        notes.push(note(
            EOL_NOTE_TOOL,
            format!(
                "Not checked, because endoflife.date does not list the release line: {}.",
                join_capped(&unlisted)
            ),
        ));
    }

    let mut findings: Vec<Finding> = Vec::new();
    for ((repo, product, cycle_name), decls) in &groups {
        let Some(FeedState::Checked { data, .. }) = feeds_by_product.get(product) else {
            continue;
        };
        let Some(cycle) = data.cycles.iter().find(|c| &c.cycle == cycle_name) else {
            continue;
        };
        let standing = eol::standing(&cycle.eol, today, WARN_DAYS);
        if matches!(standing, eol::Standing::Supported) {
            continue;
        }
        let next = data.nearest_supported_above(cycle_name, today, WARN_DAYS);
        findings.extend(eol_finding(repo, *product, cycle, &standing, next, decls));
    }

    // 4. Known vulnerabilities, for runtimes declared with a patch release.
    let mut vuln_groups: BTreeMap<(String, Product, String), Vec<&Declaration>> = BTreeMap::new();
    let mut release_only: Vec<String> = Vec::new();
    for f in &known {
        if f.decl.product.cpe_prefixes().is_empty() {
            continue;
        }
        match &f.decl.version {
            Version::Exact(v) => vuln_groups
                .entry((f.repo.clone(), f.decl.product, v.clone()))
                .or_default()
                .push(&f.decl),
            Version::Release(_) => release_only.push(describe(&f.decl)),
            Version::Loose(_) | Version::Excluded(_) => {}
        }
    }
    if !release_only.is_empty() {
        release_only.sort();
        release_only.dedup();
        notes.push(note(
            VULN_NOTE_TOOL,
            format!(
                "Known vulnerabilities were not checked, because the declared version has no \
                 patch release: {}.",
                join_capped(&release_only)
            ),
        ));
    }

    let mut lookups: Vec<(Product, String, Vec<String>)> = vuln_groups
        .keys()
        .map(|(_, p, v)| (*p, v.clone(), nvd::cpe_names(*p, v)))
        .filter(|(_, _, cpes)| !cpes.is_empty())
        .collect();
    lookups.sort();
    lookups.dedup();

    let mut nvd_tally = Tally::new();
    let mut results: BTreeMap<(Product, String), FeedState<NvdResult>> = BTreeMap::new();
    let deadline = tokio::time::Instant::now() + nvd_budget;
    let mut stop_after_failure = false;
    for (i, (product, version, cpes)) in lookups.iter().enumerate() {
        let fetched = if stop_after_failure {
            Err(anyhow::anyhow!(
                "skipped after an earlier NVD lookup failed"
            ))
        } else {
            if i > 0 {
                tokio::time::sleep(policy.spacing).await;
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            crate::vuln_intel::within_budget(remaining, nvd::lookup_all(fetcher, cpes, policy))
                .await
        };
        if let Err(e) = &fetched {
            // A rate limit that outlasted its retries, a timeout or an unreachable host: do not
            // keep asking.
            tracing::warn!("NVD lookup for {} {version} failed: {e:#}", product.label());
            stop_after_failure = true;
        }
        let path = cache_file(cache_dir, &format!("nvd-{}-{version}", product.slug()));
        let state = feeds::resolve(fetched, path.as_deref(), now, |_: &NvdResult| true);
        nvd_tally.add(&format!("{} {version}", product.label()), &state);
        results.insert((*product, version.clone()), state);
    }
    if !nvd_tally.missing.is_empty() {
        notes.push(note(
            VULN_NOTE_TOOL,
            format!(
                "Runtime vulnerability data unavailable in this scan: the National Vulnerability \
                 Database could not be reached for {}. This report does not say whether those \
                 have known vulnerabilities.",
                join_names(&nvd_tally.missing)
            ),
        ));
    }
    let mut nvd_dates: Vec<String> = Vec::new();
    for ((repo, product, version), decls) in &vuln_groups {
        let Some(FeedState::Checked { data, cached_on }) =
            results.get(&(*product, version.clone()))
        else {
            continue;
        };
        if let Some(d) = cached_on.clone().or_else(|| data.as_of.clone()) {
            nvd_dates.push(d);
        }
        if data.cves.is_empty() {
            continue;
        }
        findings.push(vuln_finding(repo, *product, version, data, decls));
    }
    let nvd_stamp = if lookups.is_empty() {
        FeedStamp::NotChecked {
            reason: if release_only.is_empty() && known.is_empty() {
                REASON_NOTHING_DECLARED.to_string()
            } else {
                REASON_NO_EXACT_RUNTIME.to_string()
            },
        }
    } else {
        match nvd_tally.stamp(day_string(now)) {
            // NVD's own response date, when it gave one, is the "as of" for a live answer.
            FeedStamp::Checked { as_of, note } if nvd_tally.cached.is_empty() => {
                FeedStamp::Checked {
                    as_of: nvd_dates.into_iter().min().unwrap_or(as_of),
                    note,
                }
            }
            other => other,
        }
    };

    findings.sort_by(|a, b| {
        (&a.repo, &a.rule_id, &a.path, a.line, &a.snippet)
            .cmp(&(&b.repo, &b.rule_id, &b.path, b.line, &b.snippet))
    });
    RuntimeAudit {
        findings,
        notes,
        eol: eol_stamp,
        nvd: nvd_stamp,
    }
}

/// Run the pass for a scan: add its findings and Coverage notes to `report`, record its ledger
/// stage and leave its two feed stamps for [`crate::vuln_intel::enrich_dependency_findings`] to
/// carry into the provenance stamp.
///
/// Call it BEFORE `enrich_dependency_findings`, over the same row range: that call adds KEV and
/// EPSS to the vulnerability findings this adds, and the corpus severity bounds run after both.
/// With `CAMERATA_DISABLE_DEP_AUDIT` set (test isolation) nothing is read or fetched.
pub async fn run_runtime_audit(report: &mut ScanReport, sources: &[(String, PathBuf)]) {
    let rows_in = report.findings.len();
    let disabled = std::env::var(crate::dep_audit::DISABLE_ENV_VAR)
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let outcome = if disabled {
        RuntimeAudit {
            findings: Vec::new(),
            notes: Vec::new(),
            eol: FeedStamp::NotChecked {
                reason: REASON_DISABLED.to_string(),
            },
            nvd: FeedStamp::NotChecked {
                reason: REASON_DISABLED.to_string(),
            },
        }
    } else {
        let cache_dir = crate::app_dirs::camerata_dir().map(|d| d.join("vuln-intel"));
        let policy = NvdPolicy::default();
        match HttpFetcher::new() {
            Ok(fetcher) => {
                audit_runtimes(
                    sources,
                    &fetcher,
                    cache_dir.as_deref(),
                    Utc::now(),
                    &policy,
                    ENRICHMENT_BUDGET,
                    NVD_BUDGET,
                )
                .await
            }
            Err(e) => {
                tracing::warn!("could not build the runtime feed HTTP client: {e}");
                audit_runtimes(
                    sources,
                    &Unreachable,
                    cache_dir.as_deref(),
                    Utc::now(),
                    &policy,
                    ENRICHMENT_BUDGET,
                    NVD_BUDGET,
                )
                .await
            }
        }
    };
    report.findings.extend(outcome.findings);
    report.coverage_notes.extend(outcome.notes);
    let mut stamp = report
        .provenance
        .vulnerability_data
        .take()
        .unwrap_or_else(|| {
            VulnDataStamp::all_not_checked("no vulnerability data was recorded for this inspection")
        });
    stamp.endoflife = outcome.eol;
    stamp.nvd = outcome.nvd;
    report.provenance.vulnerability_data = Some(stamp);
    crate::scan_ledger::record_addition_stage(
        &mut report.ledger,
        LEDGER_STAGE,
        rows_in,
        report.findings.len(),
        "runtime_audit_findings",
    );
}
