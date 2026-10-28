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

/// How one in-scope repo stands against the local disk, for the readiness gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoLink {
    /// A real local git clone whose `origin` matches the repo.
    Resolved,
    /// The source was INTENTIONALLY removed from this machine (client source deletion) and has
    /// not been fetched back. There is nothing to link, so it does not pause the project: the
    /// delivered inspection (findings, review, exports) must stay usable with no code on disk.
    /// Only the operations that need the code are disabled, with an explanation.
    Deleted,
    /// No usable clone and no deletion on record: the project still needs a repo linked.
    Unresolved,
}

impl RepoLink {
    /// Whether the readiness gate counts this repo as "nothing left to link".
    fn is_settled(self) -> bool {
        matches!(self, RepoLink::Resolved | RepoLink::Deleted)
    }
}

/// `true` is [`RepoLink::Resolved`], `false` is [`RepoLink::Unresolved`]: the original
/// per-repo boolean.
impl From<bool> for RepoLink {
    fn from(resolved: bool) -> Self {
        if resolved {
            RepoLink::Resolved
        } else {
            RepoLink::Unresolved
        }
    }
}

/// Classify a project's readiness from how each in-scope repo stands against the disk.
///
/// A repo whose source was deleted on purpose counts like a resolved one: there is nothing to
/// link. This function is PURE — no I/O.
///
/// - all settled   → [`ProjectReadiness::Ready`]
/// - none settled  → [`ProjectReadiness::Unlinked`]
/// - mixed         → [`ProjectReadiness::Partial`]
/// - EMPTY list    → [`ProjectReadiness::Ready`] (no repos to materialize → nothing to gate;
///   see the `ProjectReadiness::Ready` doc for the rationale)
pub fn classify_links(links: &[RepoLink]) -> ProjectReadiness {
    if links.is_empty() {
        // No repos in scope: there is nothing to link, so the project is not "paused waiting on
        // a clone". Its non-repo surfaces stay usable → Ready.
        return ProjectReadiness::Ready;
    }
    let any = links.iter().any(|l| l.is_settled());
    let all = links.iter().all(|l| l.is_settled());
    match (all, any) {
        (true, _) => ProjectReadiness::Ready,
        (false, true) => ProjectReadiness::Partial,
        (false, false) => ProjectReadiness::Unlinked,
    }
}

/// Classify from one boolean per in-scope repo: `true` when that repo resolves to a real local
/// git clone (the server derives this from `workspace::repo_resolution`, which validates the
/// folder is a git checkout whose `origin` matches the repo). The original form, kept as a thin
/// wrapper over [`classify_links`] (`true` = `Resolved`, `false` = `Unresolved`).
pub fn classify_readiness(resolved: &[bool]) -> ProjectReadiness {
    let links: Vec<RepoLink> = resolved.iter().copied().map(RepoLink::from).collect();
    classify_links(&links)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_resolved_is_ready() {
        assert_eq!(
            classify_readiness(&[true, true, true]),
            ProjectReadiness::Ready
        );
        assert_eq!(classify_readiness(&[true]), ProjectReadiness::Ready);
    }

    #[test]
    fn none_resolved_is_unlinked() {
        assert_eq!(
            classify_readiness(&[false, false]),
            ProjectReadiness::Unlinked
        );
        assert_eq!(classify_readiness(&[false]), ProjectReadiness::Unlinked);
    }

    #[test]
    fn mixed_is_partial() {
        assert_eq!(
            classify_readiness(&[true, false]),
            ProjectReadiness::Partial
        );
        assert_eq!(
            classify_readiness(&[false, true, false]),
            ProjectReadiness::Partial
        );
    }

    #[test]
    fn empty_repo_list_defaults_to_ready() {
        // A project with no repos has nothing to materialize, so nothing gates it. The chosen
        // sensible default for the empty case is Ready (not Unlinked): an empty project is not
        // "paused waiting on a clone".
        assert_eq!(classify_readiness(&[]), ProjectReadiness::Ready);
    }

    /// Deleted client source must not pause the project: the delivered inspection has to stay
    /// usable with no code on disk. A deleted repo is "nothing left to link".
    #[test]
    fn classify_readiness_treats_deleted_as_not_gating() {
        use RepoLink::{Deleted, Resolved, Unresolved};
        assert_eq!(classify_links(&[Deleted]), ProjectReadiness::Ready);
        assert_eq!(
            classify_links(&[Deleted, Resolved]),
            ProjectReadiness::Ready
        );
        assert_eq!(classify_links(&[Deleted, Deleted]), ProjectReadiness::Ready);
        // An UNRESOLVED repo still gates, deleted siblings or not.
        assert_eq!(
            classify_links(&[Deleted, Unresolved]),
            ProjectReadiness::Partial
        );
        assert_eq!(
            classify_links(&[Unresolved, Unresolved]),
            ProjectReadiness::Unlinked
        );
        assert_eq!(
            classify_links(&[Resolved, Unresolved]),
            ProjectReadiness::Partial
        );
        assert_eq!(classify_links(&[]), ProjectReadiness::Ready);
    }

    /// The original `&[bool]` form is unchanged.
    #[test]
    fn classify_readiness_bool_wrapper_unchanged() {
        assert_eq!(classify_readiness(&[true, true]), ProjectReadiness::Ready);
        assert_eq!(
            classify_readiness(&[false, false]),
            ProjectReadiness::Unlinked
        );
        assert_eq!(
            classify_readiness(&[true, false]),
            ProjectReadiness::Partial
        );
        assert_eq!(classify_readiness(&[]), ProjectReadiness::Ready);
        assert_eq!(RepoLink::from(true), RepoLink::Resolved);
        assert_eq!(RepoLink::from(false), RepoLink::Unresolved);
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
