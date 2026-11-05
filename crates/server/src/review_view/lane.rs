//! The Engine Check lane (review UI spec section 7; design A2, A2b, A9 and A10).
//!
//! The lane is internal quality control: it never appears in any client output. The sample is
//! drawn by [`crate::engine_check::draw_sample`], which depends on the run and the rows only, so
//! the same run always shows the same cards in the same order.

use camerata_api_types::decisions::DecisionState;
use camerata_api_types::review_view::{
    CategoryAllocation, CorrectionRef, EngineCheckCard, EngineCheckLane, OutlierView,
    PlacementView, SampleStratum, StratumRate, SMALL_CATEGORY_DRAWN,
};

use super::ReviewComposer;
use crate::engine_check::{draw_sample, Sample};

/// A rate as a percent of verdicts, with the half-width of its 95 percent Wilson score interval
/// (design A14), both in percentage points to one decimal place; `None` before the first verdict.
/// The normal approximation reads 0 at a rate of 0 or 100 percent, which is certainty from a
/// handful of verdicts; the Wilson half-width stays wide there and shrinks as verdicts come in.
fn rate_and_margin(wrong: usize, verdicts: usize) -> (Option<f64>, Option<f64>) {
    if verdicts == 0 {
        return (None, None);
    }
    const Z: f64 = 1.96;
    let n = verdicts as f64;
    let p = wrong as f64 / n;
    let margin = Z / (1.0 + Z * Z / n) * (p * (1.0 - p) / n + Z * Z / (4.0 * n * n)).sqrt();
    let round = |x: f64| (x * 1000.0).round() / 10.0;
    (Some(round(p)), Some(round(margin)))
}

/// [`rate_and_margin`], for the tests.
#[cfg(test)]
pub(super) fn rate_and_margin_for_tests(
    wrong: usize,
    verdicts: usize,
) -> (Option<f64>, Option<f64>) {
    rate_and_margin(wrong, verdicts)
}

impl<'a> ReviewComposer<'a> {
    /// The run's sample under the project's current `sample_size_per_run`.
    pub(super) fn sample(&self) -> Sample {
        draw_sample(
            self.report,
            self.inputs.settings.sample_size_per_run,
            self.inputs.run_id,
        )
    }

    fn verdict_counts<'k>(&self, keys: impl Iterator<Item = &'k String>) -> (usize, usize) {
        let (mut verdicts, mut wrong) = (0, 0);
        for key in keys {
            // A No carried from an earlier run stands for the hold, but it is not an answer given
            // on this run, so it is not in this run's rate.
            if let Some(v) = self
                .verdicts
                .get(key.as_str())
                .filter(|v| v.carried_from.is_none())
            {
                verdicts += 1;
                wrong += usize::from(!v.right && !self.confirms_throw_out(v));
            }
        }
        (verdicts, wrong)
    }

    /// Whether a No is the reviewer confirming the engine was right to throw the row out, with
    /// the row still thrown out. Either it names a false-positive decision on its own key (the
    /// Engine Check card's Confirm), or it names nothing and the row's effective decision is a
    /// false positive made strictly after the No (the detail's Confirm, which posts only the
    /// decision; the same test as the gate's `decided_since`, so the card, the hold and the rate
    /// agree). The rate of "real problems among thrown-out findings" counts a throw-out the
    /// reviewer upheld as an answer that agrees with the engine. A No on a kept row that names a
    /// false positive says the row was not real, and still counts as wrong.
    fn confirms_throw_out(&self, v: &camerata_api_types::review_view::EngineCheckVerdict) -> bool {
        if !self
            .row(&v.key)
            .is_some_and(|f| crate::engine_discard::reason_of(f).is_some())
        {
            return false;
        }
        match &v.correction {
            Some(CorrectionRef::Decision(id)) => self
                .records
                .get(id.as_str())
                .is_some_and(|d| d.state == DecisionState::FalsePositive && d.key == v.key),
            Some(CorrectionRef::Edit(_)) => false,
            None => self.decision_of(&v.key).is_some_and(|d| {
                d.state == DecisionState::FalsePositive
                    && match (
                        chrono::DateTime::parse_from_rfc3339(&d.decided_at),
                        chrono::DateTime::parse_from_rfc3339(&v.at),
                    ) {
                        (Ok(decided), Ok(no)) => decided > no,
                        _ => false,
                    }
            }),
        }
    }

    /// `GET /api/projects/:id/review/engine-check`.
    pub fn engine_check(&self) -> EngineCheckLane {
        let setting = self.inputs.settings.sample_size_per_run;
        let sample = self.sample();

        let mut cards = Vec::new();
        for (_, key, stratum) in sample.cards() {
            let Some(f) = self.row(&key) else { continue };
            let node = f
                .group_path
                .get(2)
                .and_then(|id| self.node_at.get(id.as_str()))
                .map(|i| self.node_ref(*i));
            let mut outliers: Vec<OutlierView> = Vec::new();
            if let Some(tree) = self.tree {
                for n in &tree.nodes {
                    for o in n.outliers.iter().filter(|o| o.key == key) {
                        outliers.push(OutlierView {
                            reason: o.reason,
                            why: o.why.clone(),
                        });
                    }
                }
            }
            if let Some(reason) = f.review.outlier_reason {
                if !outliers.iter().any(|o| o.reason == reason) {
                    outliers.push(OutlierView {
                        reason,
                        why: f.review.outlier_why.clone(),
                    });
                }
            }
            cards.push(EngineCheckCard {
                site: self.site_row(f),
                stratum,
                stage1: self.stage1_view(f),
                placement: PlacementView {
                    node_path: self.breadcrumb(f),
                    subgroup_label: if f.review.subgroup_label.is_empty() {
                        node.map(|n| n.label.clone()).unwrap_or_default()
                    } else {
                        f.review.subgroup_label.clone()
                    },
                    cause: if f.review.subgroup_cause.is_empty() {
                        node.and_then(|n| n.cause.clone()).unwrap_or_default()
                    } else {
                        f.review.subgroup_cause.clone()
                    },
                    summary: node.and_then(|n| n.summary.clone()),
                },
                escalation: self.escalation_status(f),
                flagged_by: Self::flagged_by_views(f),
                duplicate_link: self.duplicate_view(f),
                outliers,
                review_flags: crate::review_flags::flags_of(f),
                verdict: self.verdicts.get(key.as_str()).map(|v| (*v).clone()),
            });
        }

        let allocation: Vec<CategoryAllocation> = sample
            .unread
            .iter()
            .map(|(category, unread)| {
                let drawn_keys = sample
                    .kept
                    .iter()
                    .find(|(c, _)| c == category)
                    .map(|(_, k)| k.as_slice())
                    .unwrap_or_default();
                let (verdicts, wrong) = self.verdict_counts(drawn_keys.iter());
                let (rate, margin) = rate_and_margin(wrong, verdicts);
                CategoryAllocation {
                    category: category.clone(),
                    drawn: drawn_keys.len(),
                    unread: *unread,
                    verdicts,
                    wrong,
                    rate_percent: rate,
                    margin_percent: margin,
                    indicative_only: drawn_keys.len() < SMALL_CATEGORY_DRAWN,
                }
            })
            .collect();

        let kept_keys: Vec<&String> = sample.kept.iter().flat_map(|(_, k)| k).collect();
        let strata = [
            (SampleStratum::Kept, kept_keys),
            (SampleStratum::ThrownOut, sample.thrown_out.iter().collect()),
        ]
        .into_iter()
        .map(|(stratum, keys)| {
            let (verdicts, wrong) = self.verdict_counts(keys.iter().copied());
            let (rate, margin) = rate_and_margin(wrong, verdicts);
            StratumRate {
                stratum,
                drawn: keys.len(),
                verdicts,
                wrong,
                rate_percent: rate,
                margin_percent: margin,
            }
        })
        .collect();

        EngineCheckLane {
            run_id: self.inputs.run_id.to_string(),
            setting,
            off: setting == 0,
            drawn: sample.drawn(),
            unread_total: sample.unread_total,
            thrown_out_total: sample.thrown_out_total,
            allocation,
            strata,
            cards,
        }
    }
}
