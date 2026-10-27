//! The Queue's order presets (`WORK_ORDER_REVIEW_SPEED.md` B.4): which way the findings table is
//! cut, as pure data and pure ordering, with no rendering framework.
//!
//! A preset is two things the table needs and nothing else: how the rows are ORDERED before
//! chorale groups them ([`order_findings`], [`order_rows`]), and what they are GROUPED by
//! ([`OrderPreset::grouping`]). The Dioxus layer maps the grouping onto chorale's `group_by`
//! columns and hands the ordered rows to the table; it decides neither.
//!
//! | Preset | Grouping | Rows are ordered by |
//! |---|---|---|
//! | [`Curated`](OrderPreset::Curated) (default) | rule, then file | [`queue_sort_key`]: severity, kind, cheapest deviations, rule, path, line |
//! | [`ByFile`](OrderPreset::ByFile) | file, then rule | file, then rule, then line |
//! | [`ByRule`](OrderPreset::ByRule) | rule, then file | rule, then severity, then path and line |
//! | [`CriticalFirst`](OrderPreset::CriticalFirst) | none (flat) | [`queue_sort_key`], so the worst findings lead the one list |
//!
//! Every preset leads with the finding's enforcement status, as the curated order does, so
//! suppressed findings (baseline debt, waivers) follow the active ones whichever cut is on screen.
//! chorale builds a group at the first row that belongs to it and keeps rows in their given
//! order, so the order of the rows is also the order of the groups.
//!
//! The choice is sticky view state: it is saved in the onboarding draft beside `triage_view`, and
//! an older draft that lacks it reads as [`Curated`](OrderPreset::Curated).

use std::cmp::Ordering;

use crate::triage::{queue_sort_key, severity_rank, FindingView};

/// The sentence the order bar shows for the curated default (design §5), named rather than
/// implied so a curator 300 rows in can tell the judgement from his own re-cut.
pub const CURATED_SENTENCE: &str =
    "Camerata default order: severity, then kind, then cheapest deviations first";

/// How the Queue is cut.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum OrderPreset {
    /// Today's order: grouped by rule then file, severity first.
    #[default]
    Curated,
    /// Grouped by file then rule, rows in line order.
    ByFile,
    /// Grouped by rule then file, rows by severity.
    ByRule,
    /// One flat list, the worst findings first.
    CriticalFirst,
}

/// Read from the draft leniently ([`crate::lenient`]): a preset a newer build wrote, which this
/// one does not know, is the curated order, not a draft that fails to load.
impl crate::lenient::WireEnum for OrderPreset {
    fn from_wire(name: &str) -> Option<Self> {
        Self::from_key(name)
    }
}

/// What the table is grouped by, outermost first. The Dioxus layer maps each name to its
/// column; the one place a preset's grouping is decided is [`OrderPreset::grouping`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grouping {
    /// Rule, then file (the Queue's original grouping).
    RuleThenFile,
    /// File, then rule.
    FileThenRule,
    /// No grouping: one flat list.
    Flat,
}

impl OrderPreset {
    /// Every preset, in the order the select lists them.
    pub const ALL: [OrderPreset; 4] = [
        OrderPreset::Curated,
        OrderPreset::ByFile,
        OrderPreset::ByRule,
        OrderPreset::CriticalFirst,
    ];

    /// The select's option text.
    pub fn label(self) -> &'static str {
        match self {
            OrderPreset::Curated => "Curated",
            OrderPreset::ByFile => "By file",
            OrderPreset::ByRule => "By rule",
            OrderPreset::CriticalFirst => "Critical first",
        }
    }

    /// The stable option value (`<option value>`), also the serialized name.
    pub fn key(self) -> &'static str {
        match self {
            OrderPreset::Curated => "curated",
            OrderPreset::ByFile => "by_file",
            OrderPreset::ByRule => "by_rule",
            OrderPreset::CriticalFirst => "critical_first",
        }
    }

    /// The preset an option value names; `None` for anything else.
    pub fn from_key(key: &str) -> Option<OrderPreset> {
        Self::ALL.into_iter().find(|p| p.key() == key)
    }

    /// What the table is grouped by under this preset.
    pub fn grouping(self) -> Grouping {
        match self {
            OrderPreset::Curated | OrderPreset::ByRule => Grouping::RuleThenFile,
            OrderPreset::ByFile => Grouping::FileThenRule,
            OrderPreset::CriticalFirst => Grouping::Flat,
        }
    }

    /// The sentence the order bar shows while no manual column cut is active.
    pub fn sentence(self) -> String {
        match self {
            OrderPreset::Curated => CURATED_SENTENCE.to_string(),
            OrderPreset::ByFile => {
                "By file: files in path order, then rules, each in line order".to_string()
            }
            OrderPreset::ByRule => {
                "By rule: rules in id order, then files, worst severity first".to_string()
            }
            OrderPreset::CriticalFirst => {
                "Critical first: one flat list, worst severity first, no grouping".to_string()
            }
        }
    }

    /// What a manual column cut sits on top of ("Camerata default order" for the curated one).
    pub fn underneath(self) -> String {
        match self {
            OrderPreset::Curated => "Camerata default order".to_string(),
            other => format!("{} order", other.label()),
        }
    }

    /// The select's tooltip.
    pub fn title(self) -> &'static str {
        match self {
            OrderPreset::Curated => {
                "Severity, then kind, then the cheapest deviations first, grouped by rule then file."
            }
            OrderPreset::ByFile => {
                "Group by file, then rule; rows in line order. Read a file's findings together."
            }
            OrderPreset::ByRule => {
                "Group by rule, then file; rows by severity. Decide one rule at a time."
            }
            OrderPreset::CriticalFirst => {
                "One flat list with the worst findings first and no groups."
            }
        }
    }
}

/// A finding's enforcement rank: active before suppressed, the first term of every order.
fn enforced(f: &FindingView) -> u8 {
    u8::from(f.status != "active")
}

/// Compare two findings under `preset`. Total and stable: ties fall through to the curated key,
/// so two orders that agree on a preset's own key still agree with each other.
pub fn compare(preset: OrderPreset, a: &FindingView, b: &FindingView) -> Ordering {
    let own = match preset {
        // The curated key is severity-led, so the flat cut is the same key ungrouped.
        OrderPreset::Curated | OrderPreset::CriticalFirst => Ordering::Equal,
        OrderPreset::ByFile => (enforced(a), &a.path, &a.rule_id, a.line).cmp(&(
            enforced(b),
            &b.path,
            &b.rule_id,
            b.line,
        )),
        OrderPreset::ByRule => (
            enforced(a),
            &a.rule_id,
            severity_rank(&a.severity),
            &a.path,
            a.line,
        )
            .cmp(&(
                enforced(b),
                &b.rule_id,
                severity_rank(&b.severity),
                &b.path,
                b.line,
            )),
    };
    own.then_with(|| queue_sort_key(a).cmp(&queue_sort_key(b)))
}

/// `findings` in `preset`'s order (a stable sort of a copy).
pub fn order_findings(preset: OrderPreset, findings: &[FindingView]) -> Vec<FindingView> {
    let mut out = findings.to_vec();
    out.sort_by(|a, b| compare(preset, a, b));
    out
}

/// `rows` in `preset`'s order, each kept with its id (the table's `RowId`), so a re-sort
/// reorders the rows the table already holds instead of minting new ones.
pub fn order_rows<T>(
    preset: OrderPreset,
    mut rows: Vec<(T, FindingView)>,
) -> Vec<(T, FindingView)> {
    rows.sort_by(|(_, a), (_, b)| compare(preset, a, b));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::triage::finding_key;

    fn finding(rule: &str, sev: &str, path: &str, line: usize, status: &str) -> FindingView {
        serde_json::from_value(serde_json::json!({
            "repo": "me/api", "path": path, "line": line, "rule_id": rule, "severity": sev,
            "snippet": format!("{rule}-{path}-{line}"), "detail": "d", "status": status,
            "kind": "defect"
        }))
        .expect("fixture")
    }

    fn sample() -> Vec<FindingView> {
        vec![
            finding("R-B", "low", "src/z.rs", 5, "active"),
            finding("R-A", "high", "src/z.rs", 40, "active"),
            finding("R-A", "critical", "src/a.rs", 9, "active"),
            finding("R-B", "critical", "src/a.rs", 2, "active"),
            finding("R-A", "high", "src/a.rs", 1, "active"),
            finding("R-C", "critical", "src/m.rs", 7, "suppressed-baseline"),
        ]
    }

    fn keys(rows: &[FindingView]) -> Vec<String> {
        rows.iter().map(finding_key).collect()
    }

    fn at(rows: &[FindingView]) -> Vec<(String, usize)> {
        rows.iter().map(|f| (f.path.clone(), f.line)).collect()
    }

    #[test]
    fn curated_is_exactly_the_queue_sort_key_order() {
        let mut expected = sample();
        expected.sort_by_key(queue_sort_key);
        assert_eq!(
            keys(&order_findings(OrderPreset::Curated, &sample())),
            keys(&expected)
        );
    }

    #[test]
    fn critical_first_is_the_curated_order_ungrouped() {
        assert_eq!(
            keys(&order_findings(OrderPreset::CriticalFirst, &sample())),
            keys(&order_findings(OrderPreset::Curated, &sample()))
        );
        assert_eq!(OrderPreset::CriticalFirst.grouping(), Grouping::Flat);
        let sevs: Vec<String> = order_findings(OrderPreset::CriticalFirst, &sample())
            .iter()
            .filter(|f| f.status == "active")
            .map(|f| f.severity.clone())
            .collect();
        assert_eq!(sevs, ["critical", "critical", "high", "high", "low"]);
    }

    #[test]
    fn by_file_is_file_then_rule_then_line() {
        let got = order_findings(OrderPreset::ByFile, &sample());
        assert_eq!(
            at(&got),
            [
                ("src/a.rs".to_string(), 1),
                ("src/a.rs".to_string(), 9),
                ("src/a.rs".to_string(), 2),
                ("src/z.rs".to_string(), 40),
                ("src/z.rs".to_string(), 5),
                // The suppressed finding follows every active one.
                ("src/m.rs".to_string(), 7),
            ]
        );
        // Within one file and rule, rows are in line order.
        let rules_in_a: Vec<(&str, usize)> = got
            .iter()
            .filter(|f| f.path == "src/a.rs")
            .map(|f| (f.rule_id.as_str(), f.line))
            .collect();
        assert_eq!(rules_in_a, [("R-A", 1), ("R-A", 9), ("R-B", 2)]);
        assert_eq!(OrderPreset::ByFile.grouping(), Grouping::FileThenRule);
    }

    #[test]
    fn by_rule_is_rule_then_severity_then_place() {
        let got = order_findings(OrderPreset::ByRule, &sample());
        let shape: Vec<(&str, &str, &str, usize)> = got
            .iter()
            .map(|f| {
                (
                    f.rule_id.as_str(),
                    f.severity.as_str(),
                    f.path.as_str(),
                    f.line,
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                ("R-A", "critical", "src/a.rs", 9),
                ("R-A", "high", "src/a.rs", 1),
                ("R-A", "high", "src/z.rs", 40),
                ("R-B", "critical", "src/a.rs", 2),
                ("R-B", "low", "src/z.rs", 5),
                ("R-C", "critical", "src/m.rs", 7),
            ]
        );
        assert_eq!(OrderPreset::ByRule.grouping(), Grouping::RuleThenFile);
    }

    #[test]
    fn every_preset_is_a_permutation_and_is_stable_under_reordering() {
        let rows = sample();
        let mut all: Vec<String> = keys(&rows);
        all.sort();
        for preset in OrderPreset::ALL {
            let once = order_findings(preset, &rows);
            let mut got: Vec<String> = once.iter().map(finding_key).collect();
            got.sort();
            assert_eq!(got, all, "{preset:?} must neither drop nor invent a row");
            // Whatever order the rows arrive in, the same order comes out.
            let mut reversed = rows.clone();
            reversed.reverse();
            assert_eq!(
                keys(&order_findings(preset, &reversed)),
                keys(&once),
                "{preset:?}"
            );
        }
    }

    #[test]
    fn order_rows_keeps_each_id_with_its_finding() {
        let rows: Vec<(usize, FindingView)> = sample().into_iter().enumerate().collect();
        let by_file = order_rows(OrderPreset::ByFile, rows.clone());
        for (id, f) in &by_file {
            assert_eq!(finding_key(f), finding_key(&rows[*id].1));
        }
        let plain: Vec<FindingView> = by_file.into_iter().map(|(_, f)| f).collect();
        assert_eq!(
            keys(&plain),
            keys(&order_findings(OrderPreset::ByFile, &sample()))
        );
    }

    #[test]
    fn the_preset_round_trips_through_its_key_and_through_serde() {
        for preset in OrderPreset::ALL {
            assert_eq!(OrderPreset::from_key(preset.key()), Some(preset));
            let json = serde_json::to_string(&preset).unwrap();
            assert_eq!(
                json,
                format!("\"{}\"", preset.key()),
                "wire name = option value"
            );
            assert_eq!(serde_json::from_str::<OrderPreset>(&json).unwrap(), preset);
        }
        assert_eq!(OrderPreset::from_key("nonsense"), None);
        assert_eq!(OrderPreset::default(), OrderPreset::Curated);
    }

    #[test]
    fn only_the_curated_sentence_is_the_default_label() {
        assert_eq!(OrderPreset::Curated.sentence(), CURATED_SENTENCE);
        for preset in [
            OrderPreset::ByFile,
            OrderPreset::ByRule,
            OrderPreset::CriticalFirst,
        ] {
            assert_ne!(preset.sentence(), CURATED_SENTENCE);
            assert!(preset.sentence().starts_with(preset.label()));
        }
    }

    #[test]
    fn no_em_dashes_in_any_user_facing_text() {
        for preset in OrderPreset::ALL {
            for text in [
                preset.label().to_string(),
                preset.title().to_string(),
                preset.sentence(),
                preset.underneath(),
            ] {
                assert!(!text.contains('\u{2014}'), "{text}");
            }
        }
    }
}
