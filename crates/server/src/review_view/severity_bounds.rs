//! The rule's own say in a row's severity (the row detail's bounds line).
//!
//! Nothing here recomputes a severity. The bounds come from the corpus rule, and which one moved
//! the row is read back from the adjustment notes the shared chain recorded when it moved it
//! ([`camerata_rules::severity_bounds::classify_adjustment`]).

use camerata_api_types::edits::EditField;
use camerata_api_types::review_view::{AppliedBound, SeverityBoundsView};
use camerata_rules::severity_bounds::{
    classify_adjustment, normalize_severity, BoundStep, BoundedFinding, DEVIATION_DEFAULT_CEILING,
};

use super::ReviewComposer;
use crate::onboard::Finding;
use crate::report_export::corpus_rule_for;
use crate::report_export::overlay::reviewer_edited_field;

impl ReviewComposer<'_> {
    /// The rule's floor and ceilings and the bound that changed `f`'s shown severity, or `None`
    /// when the rule sets none and no bound moved it.
    pub(super) fn severity_bounds(&self, f: &Finding) -> Option<SeverityBoundsView> {
        let rule = corpus_rule_for(&f.rule_id, self.inputs.corpus);
        let authored = |v: Option<&String>| v.map(|s| normalize_severity(s));
        // A rule's minimum is only guidance for the agent finder's rows: it never raises them, so
        // showing it as a floor would say the opposite of what the row got.
        let floor = if f.rule_minimum_is_guidance() {
            None
        } else {
            authored(rule.and_then(|r| r.min_severity.as_ref()))
        };
        let mut view = SeverityBoundsView {
            floor,
            ceiling: authored(rule.and_then(|r| r.max_severity.as_ref())),
            deviation_ceiling: authored(rule.and_then(|r| r.deviation_ceiling.as_ref())),
            applied: None,
            applied_from: None,
        };
        // A severity the reviewer set is theirs: no bound moved what is shown.
        if !reviewer_edited_field(f, EditField::Severity) {
            // The last bound that moved it has the last word on what is shown.
            let last = crate::severity_bounds::recorded_adjustments(f)
                .iter()
                .filter_map(|n| classify_adjustment(n).map(|(step, from)| (step, from.to_string())))
                .next_back();
            if let Some((step, from)) = last {
                view.applied = Some(match step {
                    BoundStep::Floor => AppliedBound::Floor,
                    BoundStep::Ceiling => AppliedBound::Ceiling,
                    BoundStep::DeviationCeiling => AppliedBound::DeviationCeiling,
                    BoundStep::NoLocation => AppliedBound::NoLocation,
                });
                view.applied_from = Some(from);
                if step == BoundStep::DeviationCeiling && view.deviation_ceiling.is_none() {
                    view.deviation_ceiling = Some(DEVIATION_DEFAULT_CEILING.to_string());
                }
            }
        }
        let empty = view.floor.is_none()
            && view.ceiling.is_none()
            && view.deviation_ceiling.is_none()
            && view.applied.is_none();
        (!empty).then_some(view)
    }
}
