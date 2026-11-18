//! The Engine Check lane (review UI spec section 7; design A2, A2b, A9 and A10).
//!
//! The lane is internal quality control: it never appears in any client output. The sample is
//! drawn by [`crate::engine_check::draw_sample`], which depends on the run and the rows only, so
//! the same run always shows the same cards in the same order.

use std::collections::{HashMap, HashSet};

use camerata_api_types::decisions::{effective_by_key, DecisionState};
use camerata_api_types::review_view::{
    CategoryAllocation, CorrectionRef, EngineCheckCard, EngineCheckError, EngineCheckLane,
    OutlierView, PlacementView, SampleStratum, StratumRate, SMALL_CATEGORY_DRAWN,
};

use super::ReviewComposer;
use crate::engine_check::{
    draw_order, draw_ruled_out_flagged, draw_sample, ruled_out_want, Sample,
};

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

/// The second look: the cards it shows, the rows drawn new this time, and how many rows were ruled
/// out while flagged.
pub(super) struct SecondLook {
    pub(super) cards: Vec<String>,
    pub(super) added: Vec<String>,
    pub(super) total: usize,
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

    /// The rows the second look drew now that the store has not yet recorded. The route records
    /// them, so a drawn row stays drawn.
    pub fn second_look_added(&self) -> Vec<String> {
        self.second_look().added.clone()
    }

    /// The second look at rows the reviewer ruled out while the engine flagged them "possible
    /// false positive", worked out once per composer.
    ///
    /// - The rows already drawn on this run (`second_look_drawn`, kept in the Engine Check store)
    ///   stay drawn whatever the decisions, the flag or the sample size do afterwards, and so does
    ///   any row with a standing second-look No: nothing is ever taken out, so a card the
    ///   reviewer answered cannot vanish.
    /// - New rows are drawn by [`draw_ruled_out_flagged`] over the rows ruled out while flagged,
    ///   in the order they were ruled out, and recorded. With a pool of 15 or fewer the cards are
    ///   the first three rulings (an online draw cannot know the pool to come); this is accepted.
    /// - It never touches [`Self::sample`]. A row the first sample already holds is left out of
    ///   the new draws and carries one card only, the first sample's (one card per row, one
    ///   question); this is accepted.
    /// - It does not depend on the sample size: it guards the reviewer's own rulings, not engine
    ///   sampling, so a sample size of 0 leaves it on.
    pub(super) fn second_look(&self) -> &SecondLook {
        self.second_look.get_or_init(|| {
            let first = self.sample();
            let in_first: HashSet<String> = first.cards().into_iter().map(|(_, k, _)| k).collect();
            let records = self.inputs.reviewed.records();
            let effective = effective_by_key(records);
            let position: HashMap<&str, usize> = records
                .iter()
                .enumerate()
                .map(|(i, d)| (d.id.as_str(), i))
                .collect();
            // Ruled out while flagged, in the order the rulings were written.
            let mut ruled_out: Vec<(usize, &str)> = self
                .report
                .findings
                .iter()
                .filter(|f| !in_first.contains(&f.site_key))
                .filter(|f| self.possible_false_positive(f).is_some())
                .filter_map(|f| {
                    let d = effective.get(f.site_key.as_str())?;
                    (d.state == DecisionState::FalsePositive).then(|| {
                        (
                            position.get(d.id.as_str()).copied().unwrap_or(0),
                            f.site_key.as_str(),
                        )
                    })
                })
                .collect();
            ruled_out.sort_unstable();
            let pool: Vec<String> = ruled_out.into_iter().map(|(_, k)| k.to_string()).collect();

            // One de-duped set: the rows already drawn, then any row with a standing No (carried
            // from an earlier run or not), which always has its card even when the store holds no
            // draw for it. A card is never taken away.
            let mut standing: Vec<String> = self
                .verdicts
                .iter()
                .filter(|(_, v)| !v.right && v.wrong == Some(EngineCheckError::RuledOutRealProblem))
                .map(|(k, _)| (*k).to_string())
                .collect();
            standing.sort();
            let mut seen = HashSet::new();
            let mut cards: Vec<String> = self
                .inputs
                .second_look_drawn
                .iter()
                .cloned()
                .chain(standing)
                .filter(|k| {
                    !in_first.contains(k) && self.row(k).is_some() && seen.insert(k.clone())
                })
                .collect();
            // Only rows still ruled out count toward the share, so rows since kept as problems
            // never use it up; replacements are added, nothing is removed.
            let in_pool: HashSet<&str> = pool.iter().map(String::as_str).collect();
            let mut live = cards
                .iter()
                .filter(|k| in_pool.contains(k.as_str()))
                .count();
            let want = ruled_out_want(pool.len());
            let mut added = Vec::new();
            // The online draw first, then the rest of the pool in draw order.
            let mut candidates = draw_ruled_out_flagged(self.inputs.run_id, &pool);
            let mut rest: Vec<&String> = pool.iter().filter(|k| !candidates.contains(k)).collect();
            rest.sort_by_key(|k| (draw_order(self.inputs.run_id, k), (*k).clone()));
            candidates.extend(rest.into_iter().cloned());
            for key in candidates {
                if live >= want {
                    break;
                }
                if seen.insert(key.clone()) {
                    added.push(key.clone());
                    cards.push(key);
                    live += 1;
                }
            }
            SecondLook {
                cards,
                added,
                total: pool.len(),
            }
        })
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
        let still_thrown_out = self
            .row(&v.key)
            .is_some_and(|f| crate::engine_discard::reason_of(f).is_some());
        if !still_thrown_out {
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

        let second = self.second_look();
        let (second_look, ruled_out_flagged_total) = (&second.cards, second.total);
        let mut cards = Vec::new();
        let drawn_cards = sample.cards().into_iter().chain(
            second_look
                .iter()
                .map(|k| (String::new(), k.clone(), SampleStratum::RuledOutFlagged)),
        );
        for (_, key, stratum) in drawn_cards {
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
        let mut strata_keys = vec![
            (SampleStratum::Kept, kept_keys),
            (SampleStratum::ThrownOut, sample.thrown_out.iter().collect()),
        ];
        // The second look has a rate of its own, once it has drawn a row.
        if !second_look.is_empty() {
            strata_keys.push((SampleStratum::RuledOutFlagged, second_look.iter().collect()));
        }
        let strata = strata_keys
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
            // Off only when there is nothing to show: a second look keeps the lane on.
            off: setting == 0 && second_look.is_empty(),
            drawn: sample.drawn(),
            unread_total: sample.unread_total,
            thrown_out_total: sample.thrown_out_total,
            ruled_out_flagged_total,
            allocation,
            strata,
            cards,
        }
    }
}
