//! The Queue's progress strip (work order B.7): where the review stands, as one line of text.
//!
//! `Undecided 187 of 612 · Confirmed 310 · Ignored 71 · FP 29 · Tech debt 15`
//!
//! The numbers come from [`TriageCounts`] (what [`TriageModel::counts`] returns over the Queue's
//! rows, already unit-tested); this module only chooses the words and their order and does no
//! counting of its own. `Undecided` is [`TriageCounts::outstanding`] and `of N` is
//! [`TriageCounts::total`], the same two numbers the Process gate and the sub-tabs read, so the
//! strip can never disagree with them.
//!
//! The estimated time left is NOT computed here either. It comes from the review timer
//! (`crate::review_timer::eta`, which shows nothing until 20 decisions carry measured time) and is
//! handed to the component that draws this line through its `eta` prop, so this module still
//! counts and words only.
//!
//! [`TriageModel::counts`]: crate::triage::TriageModel::counts

use crate::triage::{TriageCounts, TriageState};

/// The decided states in the order the strip lists them, after `Undecided`. A test pins that this
/// plus `Unresolved` is exactly [`TriageState::all`], so a sixth disposition fails there instead of
/// being missing from the strip.
pub const STRIP_ORDER: [TriageState; 4] = [
    TriageState::Confirmed,
    TriageState::Ignored,
    TriageState::FalsePositive,
    TriageState::TechDebt,
];

/// The word the strip uses for a state: the state's own label, except for two overrides.
/// `Unresolved` is "Undecided" because that is what the number means (rows nobody has decided
/// yet), and `FalsePositive` is "FP" to keep the line short.
pub fn strip_label(state: TriageState) -> &'static str {
    match state {
        TriageState::Unresolved => "Undecided",
        TriageState::FalsePositive => "FP",
        other => other.label(),
    }
}

/// What `FP` stands for, for the tooltip.
pub const FP_MEANING: &str = "FP = false positive";

/// The strip's text for a tally.
pub fn progress_text(counts: TriageCounts) -> String {
    let mut text = format!(
        "{} {} of {}",
        strip_label(TriageState::Unresolved),
        counts.outstanding(),
        counts.total()
    );
    for state in STRIP_ORDER {
        text.push_str(&format!(
            " \u{b7} {} {}",
            strip_label(state),
            counts.of(state)
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::triage::{FindingView, TriageModel};

    fn finding(n: usize) -> FindingView {
        serde_json::from_value(serde_json::json!({
            "repo": "me/api", "path": "src/a.rs", "line": n, "rule_id": "R-1",
            "severity": "high", "snippet": format!("code {n}"), "detail": "d",
            "status": "active", "kind": "defect"
        }))
        .expect("fixture")
    }

    #[test]
    fn the_strip_reads_like_the_work_order() {
        let counts = TriageCounts {
            unresolved: 187,
            confirmed: 310,
            ignored: 71,
            false_positive: 29,
            tech_debt: 15,
        };
        assert_eq!(
            progress_text(counts),
            "Undecided 187 of 612 \u{b7} Confirmed 310 \u{b7} Ignored 71 \u{b7} FP 29 \u{b7} Tech debt 15"
        );
    }

    #[test]
    fn the_order_covers_every_disposition_exactly_once() {
        let mut listed: Vec<&str> = std::iter::once(TriageState::Unresolved)
            .chain(STRIP_ORDER)
            .map(TriageState::label)
            .collect();
        let mut all: Vec<&str> = TriageState::all()
            .into_iter()
            .map(TriageState::label)
            .collect();
        listed.sort_unstable();
        all.sort_unstable();
        assert_eq!(listed, all, "a new disposition must be placed in the strip");
    }

    #[test]
    fn the_text_has_no_em_dash_and_an_empty_queue_reads_zero_of_zero() {
        let text = progress_text(TriageCounts::default());
        assert_eq!(
            text,
            "Undecided 0 of 0 \u{b7} Confirmed 0 \u{b7} Ignored 0 \u{b7} FP 0 \u{b7} Tech debt 0"
        );
        assert!(!text.contains('\u{2014}'));
    }

    /// B.7 check 1: the strip's numbers ARE `TriageCounts` for every table (the strip is the same
    /// whichever triage table is showing), after decisions and after an undo.
    #[test]
    fn the_strip_equals_triage_counts_for_every_table_after_decisions_and_an_undo() {
        let rows: Vec<FindingView> = (1..=10).map(finding).collect();
        let mut model = TriageModel::new();
        let read = |model: &mut TriageModel| -> TriageCounts {
            let mut seen: Option<TriageCounts> = None;
            for table in TriageState::all() {
                model.triage_view = table;
                let counts = model.counts(&rows);
                // The same tally whichever table is showing.
                assert_eq!(*seen.get_or_insert(counts), counts, "{table:?}");
                let text = progress_text(counts);
                assert!(text.starts_with(&format!(
                    "Undecided {} of {}",
                    counts.outstanding(),
                    rows.len()
                )));
                for state in STRIP_ORDER {
                    assert!(
                        text.contains(&format!("{} {}", strip_label(state), counts.of(state))),
                        "{text}"
                    );
                }
            }
            seen.unwrap()
        };
        assert_eq!(read(&mut model).outstanding(), 10);
        model.dispose_with_undo(&rows[0..3], TriageState::Confirmed, "", 0);
        model.dispose_with_undo(&rows[3..5], TriageState::Ignored, "fixture", 0);
        let mut last = model
            .dispose_with_undo(&rows[5..6], TriageState::FalsePositive, "misread", 0)
            .unwrap();
        model.dispose_with_undo(&rows[6..7], TriageState::TechDebt, "", 0);
        let counts = read(&mut model);
        assert_eq!(
            (
                counts.unresolved,
                counts.confirmed,
                counts.ignored,
                counts.false_positive,
                counts.tech_debt
            ),
            (3, 3, 2, 1, 1)
        );
        assert_eq!(
            progress_text(counts),
            "Undecided 3 of 10 \u{b7} Confirmed 3 \u{b7} Ignored 2 \u{b7} FP 1 \u{b7} Tech debt 1"
        );
        // Undoing the last decision (the tech-debt one was after it, so undo the FP entry's
        // rows by hand through the entry it returned) changes the strip with it.
        model.undo(&mut last, 100);
        let counts = read(&mut model);
        assert_eq!(counts.false_positive, 0);
        assert_eq!(counts.unresolved, 4);
    }
}
