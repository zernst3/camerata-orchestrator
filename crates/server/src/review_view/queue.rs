//! The escalation queue (review UI spec section 6; U5).
//!
//! The queue lists the escalated candidates in global rank order, with the line marked from the
//! project's current settings. Candidates below the line are listed only when the request asks
//! (`show_below`): a view flag that writes nothing. The rate and floor change only through the
//! budget route.

use camerata_api_types::review_view::{
    NeededMeSummary, QueueRow, QueueView, QUEUE_BELOW_PAGE_SIZE,
};

use super::ReviewComposer;

/// The queue entries of one place: the candidates at it (a site alone is a place of one).
pub(super) struct QueueGroup {
    /// The best-ranked candidate of the group (of the escalated ones when any is).
    pub candidate: usize,
    /// Whether any candidate of the group is above the line.
    pub escalated: bool,
}

impl<'a> ReviewComposer<'a> {
    /// The queue's entries in global rank order: one for each PLACE that has a candidate, so the
    /// queue, the lane count and the budget (which counts slots) agree. A place's rows are one
    /// entry whichever of them the group reviewer named.
    pub(super) fn queue_groups(&self) -> Vec<QueueGroup> {
        let mut order: Vec<String> = Vec::new();
        let mut at: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        for (i, c) in self.candidates.iter().enumerate() {
            let id = self
                .place_of(&c.key)
                .map_or_else(|| c.key.clone(), |p| p.id.clone());
            at.entry(id.clone())
                .or_insert_with(|| {
                    order.push(id);
                    Vec::new()
                })
                .push(i);
        }
        order
            .into_iter()
            .filter_map(|id| at.remove(&id))
            .map(|members| {
                let escalated = members.iter().any(|&i| self.candidates[i].escalated);
                let candidate = members
                    .iter()
                    .copied()
                    .find(|&i| self.candidates[i].escalated == escalated)
                    .unwrap_or(members[0]);
                QueueGroup {
                    candidate,
                    escalated,
                }
            })
            .collect()
    }

    fn queue_row(&self, g: &QueueGroup) -> Option<QueueRow> {
        let c = &self.candidates[g.candidate];
        // The row that heads the place shows it; the candidate stays the best-ranked one.
        let shown = self
            .place_of(&c.key)
            .and_then(|p| self.row(&p.carrier))
            .or_else(|| self.row(&c.key))?;
        Some(QueueRow {
            candidate: c.clone(),
            node_path: self.breadcrumb(shown),
            site: self.site_row(shown),
        })
    }

    /// "Needed me: 120 of 140 decided (86%)": of the escalated rows the reviewer has decided,
    /// how many they marked as needing them.
    fn needed_me(&self) -> NeededMeSummary {
        let (mut decided, mut needed) = (0usize, 0usize);
        for g in self.queue_groups().iter().filter(|g| g.escalated) {
            let c = &self.candidates[g.candidate];
            if let Some(d) = self.decision_of(&c.key) {
                decided += 1;
                needed += usize::from(d.needed_me == Some(true));
            }
        }
        NeededMeSummary {
            decided,
            needed_me: needed,
            percent: (needed * 100 + decided / 2)
                .checked_div(decided)
                .map_or(0, |p| u8::try_from(p).unwrap_or(100)),
        }
    }

    /// `GET /api/projects/:id/review/queue?show_below=&below_offset=&below_limit=`.
    pub fn queue(&self, show_below: bool, below_offset: usize, below_limit: usize) -> QueueView {
        let groups = self.queue_groups();
        let escalated: Vec<QueueRow> = groups
            .iter()
            .filter(|g| g.escalated)
            .filter_map(|g| self.queue_row(g))
            .collect();
        let below_all: Vec<&QueueGroup> = groups.iter().filter(|g| !g.escalated).collect();
        let below_total = below_all.len();
        let below = if show_below {
            let limit = below_limit.clamp(1, QUEUE_BELOW_PAGE_SIZE);
            below_all
                .into_iter()
                .skip(below_offset)
                .take(limit)
                .filter_map(|g| self.queue_row(g))
                .collect()
        } else {
            Vec::new()
        };
        QueueView {
            run_id: self.inputs.run_id.to_string(),
            budget: self.budget,
            over_budget_reasons: self.over_budget_reasons(),
            escalated,
            show_below,
            below,
            below_total,
            below_offset,
            needed_me: self.needed_me(),
            group_reviewed: self.group_reviewed(),
        }
    }
}
