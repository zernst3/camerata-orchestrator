//! One site row (review UI spec sections 5 and 8): the same row serves the tree, the queue, the
//! Engine Check card and the detail pane.

use camerata_api_types::decisions::Decision;
use camerata_api_types::edits::{Edit, EditField};
use camerata_api_types::review::EscalationState;
use camerata_api_types::review_view::{
    BreadcrumbItem, DecisionView, DuplicateView, EditView, EscalationStatus, FlaggedByView, Marker,
    MergedIntoView, NewSinceDecisionView, SiteRow, Stage1View, Stage2View,
};

use super::ReviewComposer;
use crate::onboard::Finding;
use crate::report_export::overlay::{
    current_value, merged_target, PROVENANCE_CAPTURE, REVIEWER_EDITED_CAPTURE, REVIEWER_PROVENANCE,
};

fn decision_view(d: &Decision) -> DecisionView {
    DecisionView {
        id: d.id.clone(),
        state: d.state.clone(),
        reason: d.reason.clone(),
        scope: d.scope.clone(),
        inherited_from: d.inherited_from.clone(),
        decided_by: d.decided_by.clone(),
        decided_at: d.decided_at.clone(),
        needed_me: d.needed_me,
        confirmed_by_client: d.confirmed_by_client.clone(),
    }
}

impl<'a> ReviewComposer<'a> {
    /// The view of a live decision record, by id.
    pub(super) fn decision_by_id(&self, id: &str) -> Option<DecisionView> {
        self.records.get(id).map(|d| decision_view(d))
    }

    /// The decision that applies to a site now, as the row shows it.
    pub(super) fn decision_of(&self, key: &str) -> Option<DecisionView> {
        let applied = self.standings.get(key)?.decision.as_ref()?;
        self.decision_by_id(&applied.decision_id)
    }

    /// Category, rule and sub-group of `f`, as a breadcrumb of node ids and labels.
    pub(super) fn breadcrumb(&self, f: &Finding) -> Vec<BreadcrumbItem> {
        f.group_path
            .iter()
            .map(|id| BreadcrumbItem {
                id: id.clone(),
                label: self
                    .tree
                    .and_then(|t| self.node_at.get(id.as_str()).map(|i| &t.nodes[*i]))
                    .map_or_else(|| id.clone(), |n| n.label.clone()),
            })
            .collect()
    }

    /// Where `f` stands against the escalation line under the current settings.
    pub(super) fn escalation_status(&self, f: &Finding) -> EscalationStatus {
        match self.candidate(&f.site_key) {
            None => EscalationStatus::NotCandidate,
            Some(c) if !c.escalated => EscalationStatus::BelowLine {
                rank: c.global_rank,
            },
            Some(c) => match c.over_budget_reason {
                Some(reason) => EscalationStatus::OverBudget {
                    reason,
                    rank: c.global_rank,
                },
                None => EscalationStatus::Escalated {
                    reason: c.reason,
                    rank: c.global_rank,
                },
            },
        }
    }

    /// Stage 1's answer about `f`.
    pub(super) fn stage1_view(&self, f: &Finding) -> Stage1View {
        let record = crate::ai_audit::stage1_of(f);
        // Stage 1 said what the row said before a reviewer's severity edit replaced it.
        let severity = self
            .edits_by_key
            .get(f.site_key.as_str())
            .and_then(|edits| {
                edits.iter().find(|e| {
                    e.field == EditField::Severity && !self.stale_edits.contains_key(e.id.as_str())
                })
            })
            .map_or_else(|| f.severity.clone(), |e| e.old.clone());
        Stage1View {
            severity,
            real: record
                .as_ref()
                .map(|r| r.real.as_str().to_string())
                .unwrap_or_default(),
            effort: f.effort.clone(),
            confidence: f.confidence.clone(),
            reason: record.map(|r| r.reason).unwrap_or_default(),
        }
    }

    /// What the group reviewer said about `f`, when it said anything.
    pub(super) fn stage2_view(&self, f: &Finding) -> Option<Stage2View> {
        let candidate = self.candidate(&f.site_key);
        if f.review.is_empty() && candidate.is_none() {
            return None;
        }
        let r = &f.review;
        Some(Stage2View {
            state: match candidate {
                None => EscalationState::NotACandidate,
                Some(c) => c.state(),
            },
            reason: candidate.map(|c| c.reason),
            why: candidate.map(|c| c.why.clone()).unwrap_or_default(),
            stage2_rank: candidate.map(|c| c.stage2_rank),
            global_rank: candidate.map(|c| c.global_rank),
            over_budget_reason: candidate.and_then(|c| c.over_budget_reason),
            outlier_reason: r.outlier_reason,
            outlier_why: r.outlier_why.clone(),
            subgroup_label: r.subgroup_label.clone(),
            subgroup_cause: r.subgroup_cause.clone(),
            disagreements: r.stage2_disagreement.clone(),
        })
    }

    fn edit_view(&self, e: &Edit, f: &Finding) -> EditView {
        let stale = self.stale_edits.get(e.id.as_str());
        EditView {
            id: e.id.clone(),
            field: e.field,
            old: e.old.clone(),
            new: e.new.clone(),
            reason: e.reason.clone(),
            by: e.by.clone(),
            at: e.at.clone(),
            stale: stale.is_some(),
            current_value: stale
                .map_or_else(|| current_value(f, e.field), |s| s.current_value.clone()),
            reapplied_from: e.reapplied_from.clone(),
        }
    }

    /// The markers of `f`, in precedence order.
    fn markers_of(&self, f: &Finding, has_stale_edit: bool) -> Vec<Marker> {
        let key = f.site_key.as_str();
        let standing = self.standings.get(key);
        let candidate = self.candidate(key);
        let mut out = Vec::new();
        if self.hold_of(key).is_some() {
            out.push(Marker::Held);
        }
        match candidate {
            Some(c) if c.escalated => {
                out.push(Marker::Escalated);
                if c.over_budget_reason.is_some() {
                    out.push(Marker::OverBudget);
                }
            }
            Some(_) => out.push(Marker::Candidate),
            None => {}
        }
        if self.outliers.contains(key) || f.review.outlier_reason.is_some() {
            out.push(Marker::Outlier);
        }
        if standing.is_some_and(|s| s.new_since_decision.is_some()) {
            out.push(Marker::NewSinceDecision);
        }
        if standing.is_some_and(|s| s.stale.is_some()) || has_stale_edit {
            out.push(Marker::Stale);
        }
        if self.inputs.moved.contains_key(key) {
            out.push(Marker::Moved);
        }
        if f.possible_duplicate_of.is_some() {
            out.push(Marker::PossibleDuplicate);
        }
        if f.captures.contains_key(REVIEWER_EDITED_CAPTURE) {
            out.push(Marker::Edited);
        }
        out
    }

    /// The flagged-by entries of `f`.
    pub(super) fn flagged_by_views(f: &Finding) -> Vec<FlaggedByView> {
        f.flagged_by
            .iter()
            .map(|x| FlaggedByView {
                rule_id: x.rule_id.clone(),
                tool: x.tool.clone(),
                site_key: x.site_key.clone(),
                path: x.path.clone(),
                line: x.line,
            })
            .collect()
    }

    /// The possible duplicate `f` points at, resolved to where it is.
    pub(super) fn duplicate_view(&self, f: &Finding) -> Option<DuplicateView> {
        let link = f.possible_duplicate_of.as_ref()?;
        let target = self.row(&link.key);
        Some(DuplicateView {
            key: link.key.clone(),
            hint: link.hint.clone(),
            rule_id: target.map(|t| t.rule_id.clone()).unwrap_or_default(),
            path: target.map(|t| t.path.clone()).unwrap_or_default(),
            line: target.map_or(0, |t| t.line),
        })
    }

    /// One site row.
    pub(super) fn site_row(&self, f: &Finding) -> SiteRow {
        let key = f.site_key.as_str();
        let standing = self.standings.get(key);
        let edits: Vec<EditView> = self
            .edits_by_key
            .get(key)
            .map(|list| list.iter().map(|e| self.edit_view(e, f)).collect())
            .unwrap_or_default();
        let has_stale_edit = edits.iter().any(|e| e.stale);

        SiteRow {
            site_key: f.site_key.clone(),
            repo: f.repo.clone(),
            path: f.path.clone(),
            line: f.line,
            rule_id: f.rule_id.clone(),
            severity: f.severity.clone(),
            kind: f.kind.clone(),
            category: f.category.clone(),
            headline: current_value(f, EditField::Headline),
            detail: current_value(f, EditField::Detail),
            fix: current_value(f, EditField::Fix),
            fix_type: current_value(f, EditField::FixType),
            snippet: f.snippet.clone(),
            group_path: f.group_path.clone(),
            group_primary: f.group_role.is_primary(),
            stage1: self.stage1_view(f),
            stage2: self.stage2_view(f),
            markers: self.markers_of(f, has_stale_edit),
            decision: self.decision_of(key),
            edits,
            stale_decision: standing
                .and_then(|s| s.stale.as_ref())
                .and_then(|s| self.decision_by_id(&s.decision_id)),
            new_since_decision: standing
                .and_then(|s| s.new_since_decision.as_ref())
                .map(|n| NewSinceDecisionView {
                    decision_id: n.decision_id.clone(),
                    node_id: n.node_id.clone(),
                    decided_at: n.decided_at.clone(),
                    covered: self
                        .records
                        .get(n.decision_id.as_str())
                        .map_or(0, |d| d.applies_to.len()),
                }),
            moved_from: self.inputs.moved.get(key).cloned(),
            flagged_by: Self::flagged_by_views(f),
            possible_duplicate_of: self.duplicate_view(f),
            merged_into: merged_target(f).map(|target| MergedIntoView {
                key: target.to_string(),
                label: self
                    .row(target)
                    .map_or_else(String::new, |t| format!("{}:{}", t.path, t.line)),
            }),
            held: self.hold_of(key),
            review_level: self.inputs.reviewed.review_level(f).cloned(),
            added_by_reviewer: f
                .captures
                .get(PROVENANCE_CAPTURE)
                .is_some_and(|v| v == REVIEWER_PROVENANCE),
            thrown_out_reason: crate::engine_discard::reason_of(f).map(str::to_string),
            review_flags: crate::review_flags::flags_of(f),
        }
    }
}
