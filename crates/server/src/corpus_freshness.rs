//! Corpus freshness, server side: the one startup log line and the `corpus_freshness` object in
//! `GET /api/updates/check` (design: `docs/design/2026-10-20_corpus-staleness-check.md`,
//! "Amendment 2026-10-21", part 4).
//!
//! The rules crate owns the logic (which domains are overdue, due soon, never confirmed). This
//! module only reads the watermark file, picks "today", and shapes the result for the two
//! operator surfaces. Both are NON-FATAL by construction: a missing or malformed watermark file
//! yields "unknown" (`None` / JSON `null`), never an error, and nothing here is on a scan or
//! export path.
//!
//! # Operator-only
//!
//! Freshness is the operator's maintenance concern. Nothing in this module may reach a client
//! artifact (PDF, xlsx, CSV/JSON, README); `tests::no_corpus_freshness_text_reaches_any_client_
//! artifact` in `lib.rs` is the gate. The client-visible "rules last reviewed" date in the
//! export manifest is a separate, deliberate field: it is produced by `client_provenance`, the
//! one file the gate lets name the sanctioned date function, and not by anything in this module.

use std::path::Path;

use camerata_rules::{CorpusFreshness, FreshnessReport, RuleSet};
use chrono::Datelike;

/// Today (UTC) as the `(year, month, day)` triple the rules crate's pure date math takes.
pub(crate) fn today_utc() -> (i64, u32, u32) {
    let today = chrono::Utc::now().date_naive();
    (today.year() as i64, today.month(), today.day())
}

/// Read the watermark file at `path` and report on it as of `today` for `corpus`.
///
/// `None` means freshness is UNKNOWN, not "fine": the file is absent (a binary running away from
/// its source tree has no watermark to read, and claiming "every domain is unconfirmed" there
/// would be a false statement about a corpus nobody asked it about), or it does not parse (a
/// warning is logged; the nag must never take the server down).
pub(crate) fn load_report(
    path: &Path,
    corpus: &RuleSet,
    today: (i64, u32, u32),
) -> Option<FreshnessReport> {
    let freshness = match CorpusFreshness::load(path) {
        Ok(Some(f)) => f,
        Ok(None) => return None,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "corpus watermark file does not parse; freshness unknown"
            );
            return None;
        }
    };
    Some(freshness.report(today, &corpus.top_level_domains()))
}

/// The `corpus_freshness` value of `GET /api/updates/check`:
/// `{ overdue: [...], due_soon: [...], never_confirmed_count }`, or `null` when freshness is
/// unknown (see [`load_report`]) or no corpus is loaded.
pub(crate) fn updates_value(
    corpus: Option<&RuleSet>,
    path: &Path,
    today: (i64, u32, u32),
) -> serde_json::Value {
    corpus
        .and_then(|c| load_report(path, c, today))
        .map(|r| report_json(&r))
        .unwrap_or(serde_json::Value::Null)
}

/// Shape a [`FreshnessReport`] for the cockpit. Domain names are listed for the two groups that
/// need action; for "never confirmed" only the count is sent, because before the first sweep
/// that is every domain and the banner treats it as a quiet note, not a list.
pub(crate) fn report_json(report: &FreshnessReport) -> serde_json::Value {
    serde_json::json!({
        "overdue": report.overdue.iter().map(|o| serde_json::json!({
            "domain": o.domain,
            "last_confirmed": o.last_confirmed,
            "days_since": o.days_since,
            "threshold_days": o.threshold_days,
        })).collect::<Vec<_>>(),
        "due_soon": report.due_soon.iter().map(|d| serde_json::json!({
            "domain": d.domain,
            "last_confirmed": d.last_confirmed,
            "days_since": d.days_since,
            "threshold_days": d.threshold_days,
            "days_remaining": d.days_remaining,
        })).collect::<Vec<_>>(),
        "never_confirmed_count": report.never_confirmed.len(),
    })
}

/// The single startup log line, or `None` when the corpus is fully fresh or freshness is
/// unknown. Pure given its inputs, so the text is tested without a logger.
pub(crate) fn startup_log_line(
    corpus: &RuleSet,
    path: &Path,
    today: (i64, u32, u32),
) -> Option<String> {
    load_report(path, corpus, today).and_then(|r| r.summary_line())
}

/// Log the freshness summary once, at server startup. Non-fatal: it loads the corpus itself,
/// logs at most one `warn!` line, and returns; any failure along the way is silence, because a
/// maintenance nag must never be the reason the server does not start. Spawned by `serve`, so it
/// never delays binding the port.
pub(crate) async fn log_at_startup() {
    let corpus_path = camerata_rules::corpus_path();
    if !corpus_path.exists() {
        return;
    }
    let (corpus, _errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    if let Some(line) = startup_log_line(
        &corpus,
        &camerata_rules::corpus_freshness_path(),
        today_utc(),
    ) {
        tracing::warn!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus_of(domains: &[&str]) -> RuleSet {
        let mut set = RuleSet::default();
        for (i, d) in domains.iter().enumerate() {
            set.push(camerata_rules::bare_rule(&format!("FIX-{i}-1"), d));
        }
        set
    }

    /// Write `body` to a temp watermark file and return (dir guard, path).
    fn watermark(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("CORPUS_FRESHNESS.toml");
        std::fs::write(&path, body).expect("write");
        (dir, path)
    }

    // 2026-04-01: go (confirmed 2026-01-01) is exactly 90 days old, so it is due today;
    // javascript (2025-06-01, window 30) is long overdue; rust is never confirmed.
    const MIXED: &str = "warn_after_days = 90\n\
        [warn_after_days_overrides]\njavascript = 30\n\
        [confirmed]\ngo = \"2026-01-01\"\njavascript = \"2025-06-01\"\n";
    const TODAY: (i64, u32, u32) = (2026, 4, 1);

    #[test]
    fn the_json_has_the_spec_shape_overdue_due_soon_and_a_never_confirmed_count() {
        let (_d, path) = watermark(MIXED);
        let corpus = corpus_of(&["go", "javascript", "rust:seaorm", "rust"]);
        let v = updates_value(Some(&corpus), &path, TODAY);

        let overdue = v["overdue"].as_array().expect("overdue array");
        assert_eq!(overdue.len(), 1);
        assert_eq!(overdue[0]["domain"], "javascript");
        assert_eq!(overdue[0]["last_confirmed"], "2025-06-01");
        assert_eq!(overdue[0]["threshold_days"], 30);
        assert!(overdue[0]["days_since"].as_i64().expect("number") > 30);

        let due_soon = v["due_soon"].as_array().expect("due_soon array");
        assert_eq!(due_soon.len(), 1);
        assert_eq!(due_soon[0]["domain"], "go");
        assert_eq!(due_soon[0]["days_remaining"], 0);
        assert_eq!(due_soon[0]["threshold_days"], 90);

        // `rust` and `rust:seaorm` are one pack: ONE never-confirmed domain, as a count.
        assert_eq!(v["never_confirmed_count"], 1);
        assert!(
            v.get("never_confirmed").is_none(),
            "only the count is sent, not the list: {v}"
        );
    }

    #[test]
    fn a_fresh_corpus_yields_empty_arrays_and_a_zero_count_not_null() {
        let (_d, path) = watermark("[confirmed]\ngo = \"2026-03-30\"\n");
        let v = updates_value(Some(&corpus_of(&["go"])), &path, TODAY);
        assert_eq!(v["overdue"], serde_json::json!([]));
        assert_eq!(v["due_soon"], serde_json::json!([]));
        assert_eq!(v["never_confirmed_count"], 0);
    }

    #[test]
    fn unknown_freshness_is_null_never_a_false_all_clear_or_all_unconfirmed() {
        let corpus = corpus_of(&["go"]);
        let dir = tempfile::tempdir().expect("temp dir");

        // No watermark file at all.
        let absent = dir.path().join("nope.toml");
        assert!(updates_value(Some(&corpus), &absent, TODAY).is_null());

        // A watermark that does not parse.
        let (_d, bad) = watermark("this is [not toml");
        assert!(updates_value(Some(&corpus), &bad, TODAY).is_null());

        // No corpus loaded.
        let (_d2, good) = watermark(MIXED);
        assert!(updates_value(None, &good, TODAY).is_null());
    }

    #[test]
    fn the_startup_log_line_summarises_the_same_three_groups_on_one_line() {
        let (_d, path) = watermark(MIXED);
        let corpus = corpus_of(&["go", "javascript", "rust"]);
        let line = startup_log_line(&corpus, &path, TODAY).expect("something to say");
        assert!(!line.contains('\n'), "one line: {line:?}");
        assert!(line.contains("1 overdue (javascript)"), "{line}");
        assert!(line.contains("1 due soon (go in 0d)"), "{line}");
        assert!(line.contains("1 never confirmed"), "{line}");
    }

    #[test]
    fn the_startup_log_line_is_silent_when_fresh_or_unknown() {
        let corpus = corpus_of(&["go"]);
        let (_d, fresh) = watermark("[confirmed]\ngo = \"2026-03-30\"\n");
        assert_eq!(startup_log_line(&corpus, &fresh, TODAY), None);

        let dir = tempfile::tempdir().expect("temp dir");
        let absent = dir.path().join("nope.toml");
        assert_eq!(startup_log_line(&corpus, &absent, TODAY), None);
    }

    #[test]
    fn today_is_a_plausible_calendar_date() {
        let (y, m, d) = today_utc();
        assert!(y >= 2026, "{y}");
        assert!(camerata_rules::parse_ymd(&format!("{y:04}-{m:02}-{d:02}")).is_some());
    }
}
