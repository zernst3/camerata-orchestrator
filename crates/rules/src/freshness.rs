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
}
