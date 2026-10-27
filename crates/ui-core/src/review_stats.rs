//! The internal review-time stats (code viewer step 7a): review hours per repo, per group type and
//! per severity, from the saved [`ReviewLog`] joined with the findings. A data view for Zach's
//! pricing, not a client surface.
//!
//! INTERNAL ONLY, like the log it reads. The JSON this module writes ([`ReviewStats::to_json`]) is
//! saved by an explicit "Save stats JSON" button to a place the operator chooses; it is never part
//! of an export package.
//!
//! # What each table means
//!
//! * **Per repo** and **per group type** sum the groups' active time. A group is a rule group
//!   (repo and rule id); its type is the kind of its findings (`defect`, `deviation`,
//!   `unspecified`).
//! * **Per severity** sums the per-decision time of the rows reviewed at that severity, because a
//!   rule group can hold rows of several severities and only a row knows its own. Time spent on a
//!   group that has not yet ended in a decision is in the other two tables and not in this one;
//!   [`ReviewStats::unattributed_ms`] says how much, so the tables can be reconciled.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use crate::review_timer::{short_duration, split_group_key, undecided_by_group, ReviewLog};
use crate::triage::{finding_key, kind_tag, Disposition, FindingView, CALIBRATED_SEVERITIES};

/// One row of a stats table.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct StatLine {
    /// The repo, the group type or the severity.
    pub label: String,
    pub active_ms: u64,
    pub active_hours: f64,
    /// Groups with time recorded (zero in the severity table, which is not by group).
    pub groups: usize,
    pub groups_reviewed: usize,
    /// Rows with a reviewed record.
    pub rows_reviewed: usize,
}

impl StatLine {
    fn new(label: &str) -> Self {
        Self {
            label: label.to_string(),
            ..Self::default()
        }
    }

    fn add_time(&mut self, ms: u64) {
        self.active_ms += ms;
        self.active_hours = hours(self.active_ms);
    }

    /// `3h 10m`.
    pub fn time_text(&self) -> String {
        short_duration(self.active_ms)
    }
}

/// Hours to two decimals.
fn hours(ms: u64) -> f64 {
    ((ms as f64) / 3_600_000.0 * 100.0).round() / 100.0
}

/// The stats.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ReviewStats {
    pub total_active_ms: u64,
    pub total_active_hours: f64,
    /// Active time not yet tied to a decision: `total_active_ms` minus the severity table's total.
    pub unattributed_ms: u64,
    pub by_repo: Vec<StatLine>,
    pub by_group_type: Vec<StatLine>,
    pub by_severity: Vec<StatLine>,
}

/// The file shape: the stats under one distinctive key.
#[derive(Serialize)]
struct StatsDocument<'a> {
    review_stats: &'a ReviewStats,
}

impl ReviewStats {
    /// Join the log with the findings it was recorded against. `findings` is every Queue row and
    /// `dispositions` the current map; group reviewed-ness is taken from them (not from the log's
    /// possibly older flag).
    pub fn compute(
        log: &ReviewLog,
        findings: &[FindingView],
        dispositions: &HashMap<String, Disposition>,
    ) -> Self {
        let mut log = log.clone();
        log.sync_groups(&undecided_by_group(findings, dispositions));

        // Group type: the kind of the group's first finding.
        let mut type_of_group: HashMap<String, &'static str> = HashMap::new();
        let mut by_key: HashMap<String, &FindingView> = HashMap::new();
        for f in findings {
            type_of_group
                .entry(crate::review_timer::group_key_of(f))
                .or_insert_with(|| kind_tag(f));
            by_key.insert(finding_key(f), f);
        }

        let mut repos: BTreeMap<String, StatLine> = BTreeMap::new();
        let mut types: BTreeMap<String, StatLine> = BTreeMap::new();
        for (key, timing) in &log.review_groups {
            let (repo, _rule) = split_group_key(key);
            let repo = if repo.is_empty() { "(no repo)" } else { repo };
            let kind = type_of_group.get(key).copied().unwrap_or("unknown");
            for line in [
                repos
                    .entry(repo.to_string())
                    .or_insert_with(|| StatLine::new(repo)),
                types
                    .entry(kind.to_string())
                    .or_insert_with(|| StatLine::new(kind)),
            ] {
                line.add_time(timing.active_ms);
                line.groups += 1;
                line.groups_reviewed += usize::from(timing.group_reviewed);
            }
        }

        let mut severities: BTreeMap<String, StatLine> = BTreeMap::new();
        let mut decided_ms = 0u64;
        for (row_key, review) in &log.review_rows {
            let Some(f) = by_key.get(row_key) else {
                continue;
            };
            let severity = f.severity.to_lowercase();
            severities
                .entry(severity.clone())
                .or_insert_with(|| StatLine::new(&severity))
                .add_time(review.review_decision_ms);
            decided_ms += review.review_decision_ms;
            if let Some(line) = severities.get_mut(&severity) {
                line.rows_reviewed += 1;
            }
            let repo = if f.repo.is_empty() {
                "(no repo)"
            } else {
                &f.repo
            };
            if let Some(line) = repos.get_mut(repo) {
                line.rows_reviewed += 1;
            }
            if let Some(line) = types.get_mut(kind_tag(f)) {
                line.rows_reviewed += 1;
            }
        }

        let total = log.total_active_ms();
        let mut by_repo: Vec<StatLine> = repos.into_values().collect();
        by_repo.sort_by(|a, b| b.active_ms.cmp(&a.active_ms).then(a.label.cmp(&b.label)));
        let by_group_type: Vec<StatLine> = types.into_values().collect();
        // The calibrated ladder first, then anything else the pipeline authored.
        let mut by_severity: Vec<StatLine> = Vec::new();
        for known in CALIBRATED_SEVERITIES {
            if let Some(line) = severities.remove(known) {
                by_severity.push(line);
            }
        }
        by_severity.extend(severities.into_values());

        Self {
            total_active_ms: total,
            total_active_hours: hours(total),
            unattributed_ms: total.saturating_sub(decided_ms),
            by_repo,
            by_group_type,
            by_severity,
        }
    }

    /// `3h 10m`, the total.
    pub fn total_text(&self) -> String {
        short_duration(self.total_active_ms)
    }

    /// `3h 10m`, the part not yet tied to a decision.
    pub fn unattributed_text(&self) -> String {
        short_duration(self.unattributed_ms)
    }

    /// The stats as a JSON document, for the "Save stats JSON" button.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&StatsDocument { review_stats: self })
            .unwrap_or_else(|_| "{}".to_string())
    }
}

/// The heading of the internal view, and its standing caveat.
pub const STATS_TITLE: &str = "Active review time (internal)";
pub const STATS_NOTE: &str =
    "Internal data for pricing. It is not in any export or report a client receives.";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review_timer::{group_key, Moment, ReviewTimer, RowRef};
    use crate::triage::TriageState;
    use camerata_api_types::review_timing::{GENERIC_KEYS, INTERNAL_ONLY_KEYS};

    fn at(ms: u64) -> Moment {
        Moment::new(ms, 1_700_000_000_000 + ms)
    }

    fn finding(repo: &str, rule: &str, severity: &str, kind: &str, n: usize) -> FindingView {
        let mut v = serde_json::json!({
            "repo": repo, "path": "src/a.rs", "line": n, "rule_id": rule,
            "severity": severity, "snippet": format!("code {n}"), "detail": "d",
            "status": "active", "kind": kind
        });
        if kind == "deviation" {
            v["deviating_sites"] = 2.into();
            v["total_sites"] = 10.into();
            v["convention_label"] = "x".into();
        }
        serde_json::from_value(v).expect("fixture")
    }

    /// Two repos, a defect group and a deviation group, rows of two severities.
    fn scenario() -> (Vec<FindingView>, HashMap<String, Disposition>, ReviewLog) {
        let rows = vec![
            finding("me/api", "R-DEF", "high", "defect", 1),
            finding("me/api", "R-DEF", "low", "defect", 2),
            finding("me/web", "R-DEV", "high", "deviation", 3),
            finding("me/web", "R-DEV", "high", "deviation", 4),
        ];
        let mut timer = ReviewTimer::default();
        let def = group_key("me/api", "R-DEF");
        let dev = group_key("me/web", "R-DEV");
        timer.open_group(&def, at(0));
        timer.decided(&[RowRef::of(&rows[0])], at(60_000)); // 60s on the high defect
        timer.decided(&[RowRef::of(&rows[1])], at(90_000)); // 30s on the low defect
        timer.open_group(&dev, at(100_000)); // 10s of reading the group still counted to def
        timer.decided(&[RowRef::of(&rows[2])], at(130_000)); // 30s, plus the 10s of reading
        timer.activity(at(140_000)); // 10s of reading the next row, not yet in a decision
                                     // Row 4 is not decided: the deviation group is not reviewed.
        let mut disp = HashMap::new();
        for f in &rows[..3] {
            disp.insert(
                finding_key(f),
                Disposition {
                    state: TriageState::Confirmed,
                    ..Disposition::default()
                },
            );
        }
        (rows, disp, timer.log().clone())
    }

    #[test]
    fn hours_are_summed_per_repo_per_group_type_and_per_severity() {
        let (rows, disp, log) = scenario();
        let stats = ReviewStats::compute(&log, &rows, &disp);
        assert_eq!(stats.total_active_ms, 140_000);

        let repo = |label: &str| {
            stats
                .by_repo
                .iter()
                .find(|l| l.label == label)
                .expect(label)
        };
        // def got 0..100s (60+30+10 of reading before opening dev), dev got 40s.
        assert_eq!(repo("me/api").active_ms, 100_000);
        assert_eq!(repo("me/web").active_ms, 40_000);
        assert_eq!(
            (repo("me/api").groups, repo("me/api").groups_reviewed),
            (1, 1)
        );
        assert_eq!(
            (repo("me/web").groups, repo("me/web").groups_reviewed),
            (1, 0)
        );
        assert_eq!(repo("me/api").rows_reviewed, 2);
        // Longest first.
        assert_eq!(stats.by_repo[0].label, "me/api");

        let kind = |label: &str| {
            stats
                .by_group_type
                .iter()
                .find(|l| l.label == label)
                .expect(label)
        };
        assert_eq!(kind("defect").active_ms, 100_000);
        assert_eq!(kind("deviation").active_ms, 40_000);

        let sev = |label: &str| {
            stats
                .by_severity
                .iter()
                .find(|l| l.label == label)
                .expect(label)
        };
        assert_eq!(sev("high").active_ms, 60_000 + 40_000);
        assert_eq!(sev("high").rows_reviewed, 2);
        assert_eq!(sev("low").active_ms, 30_000);
        // The ladder order, not alphabetical.
        let order: Vec<&str> = stats.by_severity.iter().map(|l| l.label.as_str()).collect();
        assert_eq!(order, vec!["high", "low"]);
        // 140s total, 130s tied to a decision, 10s of reading not yet in one.
        assert_eq!(stats.unattributed_ms, 10_000);
    }

    #[test]
    fn the_group_flag_comes_from_the_dispositions_not_a_stale_saved_flag() {
        let (rows, mut disp, log) = scenario();
        // Decide the last row too: the deviation group becomes reviewed with no new log entry.
        disp.insert(
            finding_key(&rows[3]),
            Disposition {
                state: TriageState::Ignored,
                reason: "fixture".to_string(),
                ..Disposition::default()
            },
        );
        let stats = ReviewStats::compute(&log, &rows, &disp);
        let web = stats
            .by_repo
            .iter()
            .find(|l| l.label == "me/web")
            .expect("web");
        assert_eq!(web.groups_reviewed, 1);
    }

    #[test]
    fn an_empty_log_gives_empty_tables_and_a_zero_total() {
        let stats = ReviewStats::compute(&ReviewLog::default(), &[], &HashMap::new());
        assert_eq!(stats, ReviewStats::default());
        assert_eq!(stats.total_text(), "0m");
    }

    #[test]
    fn a_group_whose_findings_are_gone_is_counted_under_unknown_type() {
        let (_, _, log) = scenario();
        let stats = ReviewStats::compute(&log, &[], &HashMap::new());
        assert_eq!(stats.by_group_type.len(), 1);
        assert_eq!(stats.by_group_type[0].label, "unknown");
        assert_eq!(stats.by_group_type[0].active_ms, 140_000);
        assert!(
            stats.by_severity.is_empty(),
            "no row can be joined to a severity"
        );
    }

    /// Every key in the stats JSON is on the deny-list (or generic), and every listed stats key
    /// is really written.
    #[test]
    fn the_stats_json_keys_are_all_on_the_internal_only_deny_list() {
        fn keys(v: &serde_json::Value, out: &mut std::collections::BTreeSet<String>) {
            match v {
                serde_json::Value::Object(map) => {
                    for (k, v) in map {
                        out.insert(k.clone());
                        keys(v, out);
                    }
                }
                serde_json::Value::Array(items) => items.iter().for_each(|i| keys(i, out)),
                _ => {}
            }
        }
        let (rows, disp, log) = scenario();
        let json = ReviewStats::compute(&log, &rows, &disp).to_json();
        let value: serde_json::Value = serde_json::from_str(&json).expect("json");
        let mut written = std::collections::BTreeSet::new();
        keys(&value, &mut written);
        for key in &written {
            assert!(
                INTERNAL_ONLY_KEYS.contains(&key.as_str()) || GENERIC_KEYS.contains(&key.as_str()),
                "the stats JSON writes `{key}`, which is not in camerata_api_types::review_timing"
            );
        }
        for key in [
            "review_stats",
            "total_active_ms",
            "total_active_hours",
            "unattributed_ms",
            "by_group_type",
            "rows_reviewed",
            "groups_reviewed",
            "active_hours",
            "active_ms",
        ] {
            assert!(written.contains(key), "{key} is listed but not written");
        }
    }

    #[test]
    fn the_view_text_has_no_em_dash() {
        assert!(!STATS_TITLE.contains('\u{2014}'));
        assert!(!STATS_NOTE.contains('\u{2014}'));
        // The deny-list carries the phrases the view shows, so a client artifact is checked for
        // them too.
        assert!(camerata_api_types::review_timing::INTERNAL_ONLY_PHRASES
            .iter()
            .any(|p| STATS_TITLE.contains(p)));
    }
}
