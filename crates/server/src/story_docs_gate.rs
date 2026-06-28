//! Selection + option gating for the post-story documentation hook
//! (PROC-STORY-DOCS-1).
//!
//! The post-story hook (`PostStoryHook` / `StoryDocEmitter`, wired into
//! [`crate::uow::UowStore::sign_off`]) emits per-story documentation at sign-off.
//! Like the test-tamper guard (see [`crate::test_tamper::test_tamper_guard_active`]),
//! the hook must only act when the project actually opted into it — otherwise a
//! hook attached for one project would emit docs for a project that selected a
//! different documentation strategy (or no per-story docs at all).
//!
//! This mirrors the test-tamper guard's two-condition shape, both required:
//!   1. the rule `PROC-STORY-DOCS-1` is **selected** (active) in the ruleset, AND
//!   2. the **chosen option** maps to a recognised documentation convention.
//!
//! The function is pure — it inspects a slice of [`crate::project::RuleSelection`]
//! and returns the [`DocConvention`] to honour, or `None` when the rule is not
//! selected. A selection with no explicit option falls to the rule's default
//! (`per-story-docs`), exactly as the corpus default declares.
//!
//! Note the division of labour: this helper enforces **selection** (is the rule
//! on at all?). The chosen **option** is honoured downstream by
//! [`camerata_agent::post_story_hook::StoryDocEmitter`], which no-ops for every
//! convention except `PerStoryDocs`. Returning the convention here lets the caller
//! build the emitter with the right convention AND skip attaching the hook
//! entirely when the rule is deselected.

use camerata_agent::post_story_hook::DocConvention;

/// The rule id this gate governs.
pub const STORY_DOCS_RULE_ID: &str = "PROC-STORY-DOCS-1";

/// Whether the post-story documentation hook should run for this project, and
/// under which [`DocConvention`], derived from its ruleset selections.
///
/// Returns:
///   - `Some(convention)` when `PROC-STORY-DOCS-1` is selected. With no explicit
///     option the rule's default (`per-story-docs`) is returned; an explicit,
///     recognised option maps to its convention; an unrecognised option also
///     falls back to the default (the safe, documented behaviour — see
///     [`DocConvention::from_option_id`]).
///   - `None` when the rule is NOT selected at all — the hook must not run, so the
///     caller should not attach it.
///
/// This is the selection half of the gate; the option half (no-op for any
/// convention other than `PerStoryDocs`) lives in the emitter.
pub fn story_docs_convention(
    selections: &[crate::project::RuleSelection],
) -> Option<DocConvention> {
    selections
        .iter()
        .find(|s| s.rule_id == STORY_DOCS_RULE_ID)
        .map(|s| match s.chosen_option.as_deref() {
            // No explicit option -> the corpus default (`per-story-docs`).
            None => DocConvention::default(),
            // An explicit option maps to its convention; an unrecognised string
            // falls back to the default rather than silently disabling emission.
            Some(opt) => DocConvention::from_option_id(opt).unwrap_or_default(),
        })
}

/// Whether the post-story hook should fire at all (selection check only).
///
/// `true` exactly when `PROC-STORY-DOCS-1` is selected. Equivalent to
/// `story_docs_convention(selections).is_some()`; provided as a readable predicate
/// for call sites that only need the yes/no (mirrors the test-tamper guard's
/// boolean shape).
pub fn story_docs_hook_active(selections: &[crate::project::RuleSelection]) -> bool {
    story_docs_convention(selections).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::RuleSelection;

    fn sel(rule_id: &str, opt: Option<&str>) -> RuleSelection {
        RuleSelection {
            rule_id: rule_id.to_string(),
            chosen_option: opt.map(String::from),
            repos: vec![],
        }
    }

    #[test]
    fn inactive_when_rule_not_selected() {
        assert!(!story_docs_hook_active(&[sel("SOME-OTHER-RULE", None)]));
        assert!(!story_docs_hook_active(&[]));
        assert_eq!(story_docs_convention(&[sel("SOME-OTHER-RULE", None)]), None);
        assert_eq!(story_docs_convention(&[]), None);
    }

    #[test]
    fn selected_no_option_uses_default_convention() {
        assert!(story_docs_hook_active(&[sel(STORY_DOCS_RULE_ID, None)]));
        assert_eq!(
            story_docs_convention(&[sel(STORY_DOCS_RULE_ID, None)]),
            Some(DocConvention::PerStoryDocs)
        );
    }

    #[test]
    fn selected_with_explicit_option_maps_convention() {
        assert_eq!(
            story_docs_convention(&[sel(STORY_DOCS_RULE_ID, Some("per-story-docs"))]),
            Some(DocConvention::PerStoryDocs)
        );
        assert_eq!(
            story_docs_convention(&[sel(STORY_DOCS_RULE_ID, Some("mechanical-minimum"))]),
            Some(DocConvention::MechanicalMinimum)
        );
        assert_eq!(
            story_docs_convention(&[sel(STORY_DOCS_RULE_ID, Some("living-central-docs"))]),
            Some(DocConvention::LivingCentralDocs)
        );
        assert_eq!(
            story_docs_convention(&[sel(STORY_DOCS_RULE_ID, Some("adr-per-change"))]),
            Some(DocConvention::AdrPerChange)
        );
    }

    #[test]
    fn selected_with_unknown_option_falls_back_to_default() {
        // Selected but with an unrecognised option string: the rule is still ON
        // (selection holds), so we return the default convention rather than None.
        assert!(story_docs_hook_active(&[sel(STORY_DOCS_RULE_ID, Some("bogus-option"))]));
        assert_eq!(
            story_docs_convention(&[sel(STORY_DOCS_RULE_ID, Some("bogus-option"))]),
            Some(DocConvention::PerStoryDocs)
        );
    }

    #[test]
    fn mechanical_minimum_is_selected_but_emitter_no_ops() {
        // Selection is TRUE (hook may be attached), but the convention is the
        // explicit no-op one. This documents the division of labour: selection
        // gating lives here; the option no-op lives in StoryDocEmitter::emit.
        let conv = story_docs_convention(&[sel(STORY_DOCS_RULE_ID, Some("mechanical-minimum"))]);
        assert_eq!(conv, Some(DocConvention::MechanicalMinimum));
        assert_ne!(conv, Some(DocConvention::PerStoryDocs));
    }
}
