//! Corpus staleness tracking (design: `docs/design/2026-10-20_corpus-staleness-check.md`).
//!
//! The corpus encodes "what good looks like", and "good" drifts as an ecosystem moves off a
//! pattern. This module reads a version-controlled watermark file,
//! `crates/rules/CORPUS_FRESHNESS.toml`, recording when each rule DOMAIN was last CONFIRMED
//! current (by a human, after reviewing a sweep - running a sweep never advances it), and
//! computes which domains are overdue for another look.
//!
//! All date math here is PURE and dependency-free: a caller passes TODAY in as a `(year, month,
//! day)` triple (from `chrono::Utc::now()` at the edge), and every function is a total function
//! over its inputs - no clock read, no `chrono`, so the whole module is trivially testable and
//! never flaky.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use thiserror::Error;

/// Days from the civil date `(y, m, d)` to the Unix epoch (1970-01-01 = 0), by Howard Hinnant's
/// `days_from_civil` algorithm. Valid for any proleptic-Gregorian date; `m` in 1..=12, `d` in
/// 1..=31. Negative for dates before 1970. Pure.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    // Shift so March is month 0; Jan/Feb belong to the previous year.
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64; // year of era, [0, 399]
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // day of era, [0, 146096]
    era * 146097 + doe - 719468
}

/// Parse a `"YYYY-MM-DD"` string into `(year, month, day)`. Returns `None` on any shape error -
/// a malformed date in the watermark file must never panic the nag; it is simply skipped (and
/// surfaced as unparseable by [`CorpusFreshness::overdue`]'s caller if it cares).
fn parse_ymd(s: &str) -> Option<(i64, u32, u32)> {
    let mut parts = s.trim().split('-');
    let y = parts.next()?.parse::<i64>().ok()?;
    let m = parts.next()?.parse::<u32>().ok()?;
    let d = parts.next()?.parse::<u32>().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some((y, m, d))
}

/// The watermark file: `crates/rules/CORPUS_FRESHNESS.toml`. Advanced ONLY by a human
/// confirmation after reviewing a sweep (see the design doc); running a sweep never touches it.
#[derive(Debug, Clone, Deserialize)]
pub struct CorpusFreshness {
    /// Default staleness window in days (quarterly = 90). A domain confirmed longer ago than
    /// this is overdue unless it has a per-domain override below.
    #[serde(default = "default_warn_after_days")]
    pub warn_after_days: u64,
    /// Per-domain override of `warn_after_days` - a fast-moving pack (javascript) may want a
    /// tighter window than a slow one (sec/universal).
    #[serde(default)]
    pub warn_after_days_overrides: BTreeMap<String, u64>,
    /// domain -> last-confirmed-current date, `"YYYY-MM-DD"`.
    #[serde(default)]
    pub confirmed: BTreeMap<String, String>,
}

fn default_warn_after_days() -> u64 {
    90
}

/// The bundled watermark path, `crates/rules/CORPUS_FRESHNESS.toml`, resolved from the crate's
/// manifest dir (the sibling of `principles/`). Mirrors [`crate::corpus_path`].
pub fn corpus_freshness_path() -> std::path::PathBuf {
    std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/CORPUS_FRESHNESS.toml"))
}

/// A domain whose last confirmation is older than its staleness window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverdueDomain {
    pub domain: String,
    /// The recorded last-confirmed date, verbatim from the file.
    pub last_confirmed: String,
    /// Whole days from `last_confirmed` to the `today` passed in. Never negative here (a
    /// future-dated confirmation is treated as not-overdue and excluded).
    pub days_since: i64,
    /// The window that was exceeded (the per-domain override, else `warn_after_days`).
    pub threshold_days: u64,
}

impl CorpusFreshness {
    /// Parse the TOML at `path`. A missing file is NOT an error at this layer - callers decide
    /// whether absence means "never configured" (nag) or "not using the feature" (silent); it
    /// is returned as `Ok(None)` so neither interpretation is forced here.
    pub fn load(path: &Path) -> Result<Option<Self>, toml::de::Error> {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Ok(None);
        };
        toml::from_str(&text).map(Some)
    }

    /// The staleness window for `domain`: its override if present, else the global default.
    pub fn threshold_for(&self, domain: &str) -> u64 {
        self.warn_after_days_overrides
            .get(domain)
            .copied()
            .unwrap_or(self.warn_after_days)
    }

    /// Every confirmed domain whose age (as of `today`) exceeds its window, sorted most-overdue
    /// first. A domain with an unparseable date is skipped here (see [`Self::unparseable`]); a
    /// future-dated confirmation (negative age) is not overdue and is excluded.
    pub fn overdue(&self, today: (i64, u32, u32)) -> Vec<OverdueDomain> {
        let today_days = days_from_civil(today.0, today.1, today.2);
        let mut out: Vec<OverdueDomain> = self
            .confirmed
            .iter()
            .filter_map(|(domain, date)| {
                let (y, m, d) = parse_ymd(date)?;
                let days_since = today_days - days_from_civil(y, m, d);
                let threshold = self.threshold_for(domain);
                if days_since > threshold as i64 {
                    Some(OverdueDomain {
                        domain: domain.clone(),
                        last_confirmed: date.clone(),
                        days_since,
                        threshold_days: threshold,
                    })
                } else {
                    None
                }
            })
            .collect();
        out.sort_by(|a, b| b.days_since.cmp(&a.days_since).then(a.domain.cmp(&b.domain)));
        out
    }

    /// Corpus domains that have NO watermark at all - present in the live corpus but absent
    /// from `confirmed`, i.e. never swept-and-confirmed. This is the honest "we have never
    /// verified this pack" signal, distinct from "confirmed but now overdue" ([`Self::overdue`]).
    /// A freshly-seeded file (empty `confirmed`) reports every corpus domain here until the
    /// first confirmed sweep populates it. `all_domains` is the set of top-level domains the
    /// loaded corpus actually contains. Sorted.
    pub fn never_confirmed(&self, all_domains: &[String]) -> Vec<String> {
        let mut out: Vec<String> = all_domains
            .iter()
            .filter(|d| !self.confirmed.contains_key(*d))
            .cloned()
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Domains whose recorded date does not parse as `YYYY-MM-DD` - a data error in the file to
    /// surface loudly rather than silently treat as fresh-or-stale. Sorted.
    pub fn unparseable(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .confirmed
            .iter()
            .filter(|(_, date)| parse_ymd(date).is_none())
            .map(|(domain, _)| domain.clone())
            .collect();
        out.sort();
        out
    }
}

/// Advance (or insert) one domain's confirmation date inside the TEXT of a
/// `CORPUS_FRESHNESS.toml`, returning the new text. A pure text transform, not a
/// re-serialization, so every comment (the swept-vs-confirmed contract header, the override
/// examples) is preserved verbatim - re-serializing a `toml::Value` would strip them all.
///
/// Rules: if a `<domain> = "..."` line already exists under `[confirmed]`, its date is replaced;
/// otherwise a new line is appended at the end of the `[confirmed]` table (kept sorted among the
/// existing entries for a stable diff). `date` is written as given; the caller passes a
/// validated `YYYY-MM-DD`. Returns an error only if the file has no `[confirmed]` table at all.
pub fn confirm_in_text(text: &str, domain: &str, date: &str) -> Result<String, FreshnessEditError> {
    let lines: Vec<&str> = text.lines().collect();
    // Find the [confirmed] header.
    let header = lines
        .iter()
        .position(|l| l.trim() == "[confirmed]")
        .ok_or(FreshnessEditError::NoConfirmedTable)?;

    // The confirmed table runs from just after the header to the next table header (a line
    // starting with '[') or EOF.
    let body_end = lines[header + 1..]
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .map(|rel| header + 1 + rel)
        .unwrap_or(lines.len());

    let key_prefix = format!("{domain} =");
    let new_entry = format!("{domain} = \"{date}\"");

    let mut out: Vec<String> = lines.iter().map(|s| s.to_string()).collect();

    // Existing entry (ignoring leading whitespace)?
    if let Some(idx) = (header + 1..body_end)
        .find(|&i| lines[i].trim_start().starts_with(&key_prefix))
    {
        out[idx] = new_entry;
    } else {
        // Insert before the first trailing blank lines of the table body, so the entry sits
        // with its siblings rather than after a gap.
        let mut insert_at = body_end;
        while insert_at > header + 1 && lines[insert_at - 1].trim().is_empty() {
            insert_at -= 1;
        }
        out.insert(insert_at, new_entry);
    }

    let mut joined = out.join("\n");
    if text.ends_with('\n') {
        joined.push('\n');
    }
    Ok(joined)
}

/// Advance one domain's confirmation date in the watermark file on disk (read, transform via
/// [`confirm_in_text`], write). The caller is responsible for committing the file - this does no
/// git. This is the ONLY function that advances a watermark, and it is invoked by the human
/// confirm step after a sweep is reviewed, never by a sweep run.
pub fn confirm_domain(path: &Path, domain: &str, date: &str) -> Result<(), FreshnessEditError> {
    let text = std::fs::read_to_string(path).map_err(FreshnessEditError::Io)?;
    let updated = confirm_in_text(&text, domain, date)?;
    std::fs::write(path, updated).map_err(FreshnessEditError::Io)?;
    Ok(())
}

/// Format the human-facing staleness nag for a set of overdue and never-confirmed domains, or
/// `None` when the corpus is fully fresh (nothing to say). Non-fatal text a caller prints in a
/// load banner or the daily digest - it never blocks a scan or export.
pub fn format_staleness_nag(overdue: &[OverdueDomain], never_confirmed: &[String]) -> Option<String> {
    if overdue.is_empty() && never_confirmed.is_empty() {
        return None;
    }
    let mut lines = vec!["CORPUS STALENESS:".to_string()];
    for o in overdue {
        lines.push(format!(
            "  {} last confirmed {} ({} days ago, threshold {}). Due for a staleness sweep.",
            o.domain, o.last_confirmed, o.days_since, o.threshold_days
        ));
    }
    if !never_confirmed.is_empty() {
        lines.push(format!(
            "  never confirmed (no sweep yet): {}.",
            never_confirmed.join(", ")
        ));
    }
    Some(lines.join("\n"))
}

/// Error from editing the watermark file in place.
#[derive(Debug, Error)]
pub enum FreshnessEditError {
    #[error("CORPUS_FRESHNESS.toml has no [confirmed] table to write into")]
    NoConfirmedTable,
    #[error("reading or writing the watermark file: {0}")]
    Io(#[source] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_from_civil_matches_known_anchors() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(days_from_civil(2000, 1, 1), 10957);
        // A one-year span is 365 days (2001 is not a leap year).
        assert_eq!(days_from_civil(2001, 1, 1) - days_from_civil(2000, 1, 1), 366); // 2000 is leap
        assert_eq!(days_from_civil(2002, 1, 1) - days_from_civil(2001, 1, 1), 365);
    }

    fn fixture() -> CorpusFreshness {
        let toml = r#"
            warn_after_days = 90
            [warn_after_days_overrides]
            javascript = 30
            [confirmed]
            universal = "2026-10-01"
            supabase = "2026-08-15"
            javascript = "2026-10-05"
            go = "2026-07-01"
        "#;
        toml::from_str(toml).unwrap()
    }

    #[test]
    fn overdue_uses_default_window_and_per_domain_override() {
        let f = fixture();
        // As of 2026-10-20:
        //   universal  19 days  (<= 90)            -> not overdue
        //   supabase   66 days  (<= 90)            -> not overdue
        //   javascript 15 days  (<= 30 override)   -> not overdue
        //   go         111 days (> 90)             -> OVERDUE
        let overdue = f.overdue((2026, 10, 20));
        assert_eq!(overdue.len(), 1);
        assert_eq!(overdue[0].domain, "go");
        assert_eq!(overdue[0].days_since, 111);
        assert_eq!(overdue[0].threshold_days, 90);
    }

    #[test]
    fn per_domain_override_makes_a_fast_pack_overdue_sooner() {
        let f = fixture();
        // 40 days after javascript's confirmation: past its 30-day override, still inside the
        // 90-day default every other pack uses.
        let overdue = f.overdue((2026, 11, 14));
        let js = overdue.iter().find(|o| o.domain == "javascript").expect("js overdue");
        assert_eq!(js.days_since, 40);
        assert_eq!(js.threshold_days, 30);
    }

    #[test]
    fn overdue_sorted_most_overdue_first() {
        let f = fixture();
        let overdue = f.overdue((2027, 1, 1)); // everything stale by now
        let days: Vec<i64> = overdue.iter().map(|o| o.days_since).collect();
        let mut sorted = days.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(days, sorted, "most-overdue must come first");
    }

    #[test]
    fn future_dated_confirmation_is_not_overdue() {
        let f = fixture();
        // Before any confirmation date: negative age, never flagged.
        assert!(f.overdue((2026, 1, 1)).is_empty());
    }

    #[test]
    fn unparseable_date_is_reported_not_panicked() {
        let toml = r#"
            [confirmed]
            universal = "2026-10-01"
            broken = "last tuesday"
            alsobad = "2026-13-40"
        "#;
        let f: CorpusFreshness = toml::from_str(toml).unwrap();
        assert_eq!(f.unparseable(), vec!["alsobad".to_string(), "broken".to_string()]);
        // The bad rows are skipped by overdue(), not crashed on.
        let _ = f.overdue((2030, 1, 1));
    }

    #[test]
    fn never_confirmed_reports_corpus_domains_absent_from_the_watermark() {
        let f = fixture(); // confirmed: universal, supabase, javascript, go
        let corpus_domains = vec![
            "universal".to_string(),
            "supabase".to_string(),
            "javascript".to_string(),
            "go".to_string(),
            "rust".to_string(), // present in corpus, never confirmed
            "python".to_string(),
        ];
        assert_eq!(
            f.never_confirmed(&corpus_domains),
            vec!["python".to_string(), "rust".to_string()]
        );
    }

    #[test]
    fn empty_confirmed_reports_every_domain_as_never_confirmed() {
        let f: CorpusFreshness = toml::from_str("warn_after_days = 90").unwrap();
        let domains = vec!["rust".to_string(), "go".to_string()];
        assert_eq!(f.never_confirmed(&domains), vec!["go".to_string(), "rust".to_string()]);
        assert!(f.overdue((2030, 1, 1)).is_empty(), "nothing confirmed -> nothing 'overdue'");
    }

    #[test]
    fn missing_file_is_ok_none() {
        let got = CorpusFreshness::load(Path::new("/nonexistent/CORPUS_FRESHNESS.toml")).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn default_window_is_quarterly() {
        let f: CorpusFreshness = toml::from_str("[confirmed]\nx = \"2026-01-01\"").unwrap();
        assert_eq!(f.warn_after_days, 90);
        assert_eq!(f.threshold_for("anything"), 90);
    }

    const WATERMARK: &str = "# header comment, must survive\nwarn_after_days = 90\n\n[warn_after_days_overrides]\n\n[confirmed]\ngo = \"2026-07-01\"\nrust = \"2026-08-01\"\n";

    #[test]
    fn confirm_updates_an_existing_domain_in_place_preserving_comments() {
        let out = confirm_in_text(WATERMARK, "go", "2026-10-20").unwrap();
        assert!(out.contains("# header comment, must survive"), "header comment lost");
        assert!(out.contains("go = \"2026-10-20\""), "go not advanced");
        assert!(!out.contains("go = \"2026-07-01\""), "old go date left behind");
        assert!(out.contains("rust = \"2026-08-01\""), "sibling clobbered");
        // Still parses, and go is now the confirmed value.
        let f: CorpusFreshness = toml::from_str(&out).unwrap();
        assert_eq!(f.confirmed.get("go").unwrap(), "2026-10-20");
    }

    #[test]
    fn confirm_inserts_a_new_domain_under_confirmed() {
        let out = confirm_in_text(WATERMARK, "python", "2026-10-20").unwrap();
        let f: CorpusFreshness = toml::from_str(&out).unwrap();
        assert_eq!(f.confirmed.get("python").unwrap(), "2026-10-20");
        assert_eq!(f.confirmed.get("go").unwrap(), "2026-07-01"); // untouched
        assert_eq!(f.confirmed.len(), 3);
    }

    #[test]
    fn confirm_into_an_empty_confirmed_table_at_eof() {
        // The bundled file's shape: [confirmed] is the last table and starts empty.
        let text = "warn_after_days = 90\n\n[confirmed]\n";
        let out = confirm_in_text(text, "go", "2026-10-20").unwrap();
        let f: CorpusFreshness = toml::from_str(&out).unwrap();
        assert_eq!(f.confirmed.get("go").unwrap(), "2026-10-20");
        assert_eq!(f.confirmed.len(), 1);
    }

    #[test]
    fn confirm_errors_without_a_confirmed_table() {
        let err = confirm_in_text("warn_after_days = 90\n", "go", "2026-10-20");
        assert!(matches!(err, Err(FreshnessEditError::NoConfirmedTable)));
    }

    #[test]
    fn nag_is_none_when_fresh_and_text_when_not() {
        assert!(format_staleness_nag(&[], &[]).is_none());
        let overdue = vec![OverdueDomain {
            domain: "go".to_string(),
            last_confirmed: "2026-07-01".to_string(),
            days_since: 111,
            threshold_days: 90,
        }];
        let msg = format_staleness_nag(&overdue, &["rust".to_string()]).unwrap();
        assert!(msg.contains("go last confirmed 2026-07-01 (111 days ago"));
        assert!(msg.contains("never confirmed (no sweep yet): rust"));
    }
}
