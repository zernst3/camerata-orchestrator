//! Project READINESS — the derived operational gate (ADR `2026-07-01_project-readiness-gate`).
//!
//! A project is modeled as three facts: portable IDENTITY (`owner/repo`), machine-local
//! MATERIALIZATION (a resolvable local git clone), and READINESS — which is DERIVED from the
//! materialization and never stored. Both the Workspace view and the project's operational state
//! read this single derived value, so they can never disagree.
//!
//! This module owns ONLY the pure classification (given each repo's "did it resolve to a real
//! local clone?" boolean → [`ProjectReadiness`]). It has no I/O: the server adapter checks each
//! repo on disk (via `workspace::repo_resolution`) and feeds the booleans here. Keeping the
//! decision pure (`RUST-HEADLESS-CORE-1`) makes it unit-testable without a filesystem.

use serde::{Deserialize, Serialize};

/// A project's derived readiness, computed from whether each in-scope repo resolves to a real
/// local git clone. Operations (scan / apply / run / design-publish) gate on this single value
/// rather than each discovering failure at call time (the dead-button family, per the ADR).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectReadiness {
    /// Every in-scope repo resolves to a real local git clone. The project is active — all
    /// repo-dependent actions are live.
    ///
    /// A project with NO repos in scope is also `Ready`: there is nothing to link, so nothing
    /// gates it. (This is the "sensible default" for the empty case — an empty project is not
    /// "unlinked", it simply has no repos to materialize, and its non-repo surfaces stay usable.)
    Ready,
    /// NONE of the in-scope repos resolve. The project loads PAUSED behind a single "Link repo"
    /// affordance. This is also the state a freshly-imported project lands in on a new machine:
    /// identity travelled, no local clone did.
    Unlinked,
    /// SOME repos resolve and others do not. Still paused: repo-dependent actions must not look
    /// live while any repo is unresolved, so `Partial` gates the same as `Unlinked`.
    Partial,
}

impl ProjectReadiness {
    /// True when the project is fully materialized and its repo-dependent actions may run.
    /// `Partial` and `Unlinked` both return `false` — the gate is "all repos resolved or none
    /// to resolve", not "at least one resolved".
    pub fn is_ready(self) -> bool {
        matches!(self, ProjectReadiness::Ready)
    }
}

/// Classify a project's readiness from its repos' local resolution.
///
/// `resolved` is one boolean per in-scope repo: `true` when that repo resolves to a real local
/// git clone (the server derives this from `workspace::repo_resolution`, which validates the
/// folder is a git checkout whose `origin` matches the repo). This function is PURE — no I/O.
///
/// - all `true`  → [`ProjectReadiness::Ready`]
/// - all `false` → [`ProjectReadiness::Unlinked`]
/// - mixed       → [`ProjectReadiness::Partial`]
/// - EMPTY list  → [`ProjectReadiness::Ready`] (no repos to materialize → nothing to gate;
///   see the `ProjectReadiness::Ready` doc for the rationale)
pub fn classify_readiness(resolved: &[bool]) -> ProjectReadiness {
    if resolved.is_empty() {
        // No repos in scope: there is nothing to link, so the project is not "paused waiting on
        // a clone". Its non-repo surfaces stay usable → Ready.
        return ProjectReadiness::Ready;
    }
    let any = resolved.iter().any(|&r| r);
    let all = resolved.iter().all(|&r| r);
    match (all, any) {
        (true, _) => ProjectReadiness::Ready,
        (false, true) => ProjectReadiness::Partial,
        (false, false) => ProjectReadiness::Unlinked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_resolved_is_ready() {
        assert_eq!(classify_readiness(&[true, true, true]), ProjectReadiness::Ready);
        assert_eq!(classify_readiness(&[true]), ProjectReadiness::Ready);
    }

    #[test]
    fn none_resolved_is_unlinked() {
        assert_eq!(classify_readiness(&[false, false]), ProjectReadiness::Unlinked);
        assert_eq!(classify_readiness(&[false]), ProjectReadiness::Unlinked);
    }

    #[test]
    fn mixed_is_partial() {
        assert_eq!(classify_readiness(&[true, false]), ProjectReadiness::Partial);
        assert_eq!(classify_readiness(&[false, true, false]), ProjectReadiness::Partial);
    }

    #[test]
    fn empty_repo_list_defaults_to_ready() {
        // A project with no repos has nothing to materialize, so nothing gates it. The chosen
        // sensible default for the empty case is Ready (not Unlinked): an empty project is not
        // "paused waiting on a clone".
        assert_eq!(classify_readiness(&[]), ProjectReadiness::Ready);
    }

    #[test]
    fn is_ready_only_for_ready() {
        assert!(ProjectReadiness::Ready.is_ready());
        assert!(!ProjectReadiness::Partial.is_ready());
        assert!(!ProjectReadiness::Unlinked.is_ready());
    }

    #[test]
    fn readiness_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&ProjectReadiness::Ready).unwrap(),
            "\"ready\""
        );
        assert_eq!(
            serde_json::to_string(&ProjectReadiness::Unlinked).unwrap(),
            "\"unlinked\""
        );
        assert_eq!(
            serde_json::to_string(&ProjectReadiness::Partial).unwrap(),
            "\"partial\""
        );
    }
}
