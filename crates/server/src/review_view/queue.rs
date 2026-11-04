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

impl<'a> ReviewComposer<'a> {
    fn queue_row(&self, c: &camerata_api_types::review::EscalationCandidate) -> Option<QueueRow> {
        let f = self.row(&c.key)?;
        Some(QueueRow {
            candidate: c.clone(),
            node_path: self.breadcrumb(f),
            site: self.site_row(f),
        })
    }

    /// "Needed me: 120 of 140 decided (86%)": of the escalated rows the reviewer has decided,
    /// how many they marked as needing them.
    fn needed_me(&self) -> NeededMeSummary {
        let (mut decided, mut needed) = (0usize, 0usize);
        for c in self.candidates.iter().filter(|c| c.escalated) {
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
        let escalated: Vec<QueueRow> = self
            .candidates
            .iter()
            .filter(|c| c.escalated)
            .filter_map(|c| self.queue_row(c))
            .collect();
        let below_all: Vec<&_> = self.candidates.iter().filter(|c| !c.escalated).collect();
        let below_total = below_all.len();
        let below = if show_below {
            let limit = below_limit.clamp(1, QUEUE_BELOW_PAGE_SIZE);
            below_all
                .into_iter()
                .skip(below_offset)
                .take(limit)
                .filter_map(|c| self.queue_row(c))
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
