//! Project readiness gate: pure logic for mapping the BFF's `/readiness` response into
//! view state. No rendering-framework dependency (RUST-HEADLESS-CORE-1); unit-tested here.
//!
//! A project is modeled as three facts (see `docs/decisions/2026-07-01_project-readiness-gate.md`):
//! identity (portable), local materialization (machine-local), and **readiness** — derived from
//! whether each repo resolves to a local clone. Repo-dependent actions gate on readiness so an
//! `Unlinked` / `Partial` project loads PAUSED behind a single "link repo" affordance rather than
//! each action failing independently (the dead-end-affordance principle).

/// The derived operational state of a project, mirroring the server's `readiness` string.
/// Anything the client doesn't recognise collapses to `Unlinked` (fail-safe: gate rather than
/// let actions fire into a guaranteed failure).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectReadiness {
    /// Every repo resolves to a local clone. Actions are live.
    Ready,
    /// No repo resolves. The project is paused; the resolve modal shows on entry.
    Unlinked,
    /// Some repos resolve, others don't. Paused until the rest are linked.
    Partial,
}

impl ProjectReadiness {
    /// Parse the server's `readiness` string. Unknown values fail safe to `Unlinked` so the
    /// gate errs toward pausing rather than releasing actions on an unrecognised state.
    pub fn parse(s: &str) -> Self {
        match s {
            "ready" => ProjectReadiness::Ready,
            "partial" => ProjectReadiness::Partial,
            // "unlinked" and anything unrecognised.
            _ => ProjectReadiness::Unlinked,
        }
    }

    /// Whether the project is operationally ready (all repos resolved).
    pub fn is_ready(self) -> bool {
        matches!(self, ProjectReadiness::Ready)
    }

    /// Whether the project is paused — i.e. NOT ready, so repo-dependent actions must be gated
    /// and the resolve modal / paused banner shown.
    pub fn is_paused(self) -> bool {
        !self.is_ready()
    }
}

/// One repo's local-resolution status, mirroring an entry of the server's `repos` array. Only the
/// fields the gate needs are modeled; extra fields on the wire are ignored by the adapter's parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoResolution {
    /// The repo identity, `owner/repo`.
    pub repo: String,
    /// Whether this repo resolves to a valid local clone.
    pub resolved: bool,
    /// The resolved (or expected) local path, when known. Empty when unresolved.
    pub path: String,
    /// A human-readable reason for the current state (e.g. "no local match", "origin mismatch").
    pub reason: String,
    /// The source was deleted from this machine on purpose (client source deletion) and has not
    /// been fetched back. Such a repo has nothing to link, so it is not "unresolved".
    pub source_deleted: bool,
    /// RFC3339 time of the deletion, when `source_deleted`.
    pub deleted_at: Option<String>,
}

/// The repos that still need resolving (not resolved AND not deliberately deleted) — the ones the
/// modal offers to clone-or-link. A repo whose source was deleted is excluded: the project keeps
/// working without it, and the way back is "Re-fetch source", not "Link repo". Order is
/// preserved from the input.
pub fn unresolved_repos(repos: &[RepoResolution]) -> Vec<&RepoResolution> {
    repos
        .iter()
        .filter(|r| !r.resolved && !r.source_deleted)
        .collect()
}

/// The scan-and-write operations that cannot run while a repo's source is deleted, and why.
/// `None` when every repo's source is available (or never was deleted).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanBlock {
    /// `(repo, deleted_at)` for each repo whose source is deleted, in input order.
    pub repos: Vec<(String, String)>,
}

/// Which repos block scanning, if any. Pure.
pub fn scan_block(repos: &[RepoResolution]) -> Option<ScanBlock> {
    let deleted: Vec<(String, String)> = repos
        .iter()
        .filter(|r| r.source_deleted)
        .map(|r| (r.repo.clone(), r.deleted_at.clone().unwrap_or_default()))
        .collect();
    (!deleted.is_empty()).then_some(ScanBlock { repos: deleted })
}

/// The explanation shown beside every control a deleted source disables, for example
/// "Source for acme/app was deleted on 24 Oct 2026. Re-fetch source from the Workspace tab to
/// rescan." Several repos are listed in one sentence.
pub fn scan_block_text(block: &ScanBlock) -> String {
    let named: Vec<String> = block
        .repos
        .iter()
        .map(|(repo, at)| {
            if at.is_empty() {
                repo.clone()
            } else {
                format!("{repo} ({})", crate::source::format_date(at))
            }
        })
        .collect();
    match block.repos.as_slice() {
        [(repo, at)] if !at.is_empty() => format!(
            "Source for {repo} was deleted on {}. Re-fetch source from the Workspace tab to rescan.",
            crate::source::format_date(at)
        ),
        [(repo, _)] => format!(
            "Source for {repo} was deleted. Re-fetch source from the Workspace tab to rescan."
        ),
        _ => format!(
            "Source was deleted for {}. Re-fetch source from the Workspace tab to rescan.",
            named.join(", ")
        ),
    }
}

/// The banner headline for a paused project, given its readiness. `Ready` has no banner (returns
/// `None`); the caller renders nothing in that case.
pub fn paused_banner_text(readiness: ProjectReadiness, unresolved_count: usize) -> Option<String> {
    match readiness {
        ProjectReadiness::Ready => None,
        ProjectReadiness::Unlinked => {
            Some("Paused — link this project's repo before you can run it.".to_string())
        }
        ProjectReadiness::Partial => {
            let plural = if unresolved_count == 1 {
                "repo"
            } else {
                "repos"
            };
            Some(format!(
                "Paused — {unresolved_count} {plural} still need a local clone before this project can run."
            ))
        }
    }
}

/// The modal's per-repo prompt line (the ADR wording), for one unresolved repo.
pub fn resolve_prompt_text(repo: &str) -> String {
    format!(
        "This project is coupled with `{repo}`, but no local match exists. Clone it now, or select a local clone you already have?"
    )
}

/// Whether a repo-dependent primary action (start run / scan / apply / design-publish) should be
/// GATED (disabled) given the project's readiness. Actions are gated whenever the project is not
/// fully ready, so no action fires into a guaranteed 404 / no-op.
pub fn action_gated(readiness: ProjectReadiness) -> bool {
    readiness.is_paused()
}

/// Whether the resolve modal should auto-open on project entry: only when the project is paused.
pub fn should_open_resolve_modal(readiness: ProjectReadiness) -> bool {
    readiness.is_paused()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(repo: &str, resolved: bool) -> RepoResolution {
        RepoResolution {
            repo: repo.to_string(),
            resolved,
            path: if resolved {
                format!("/ws/{repo}")
            } else {
                String::new()
            },
            reason: if resolved {
                "ok".to_string()
            } else {
                "no local match".to_string()
            },
            source_deleted: false,
            deleted_at: None,
        }
    }

    fn deleted(repo: &str, at: &str) -> RepoResolution {
        RepoResolution {
            source_deleted: true,
            deleted_at: Some(at.to_string()),
            reason: "source deleted".to_string(),
            ..res(repo, false)
        }
    }

    #[test]
    fn unresolved_repos_excludes_deleted() {
        let repos = vec![
            res("a/one", false),
            deleted("a/two", "2026-10-24T17:40:19Z"),
            res("a/three", true),
        ];
        let un = unresolved_repos(&repos);
        assert_eq!(un.len(), 1);
        assert_eq!(un[0].repo, "a/one");
        // A project whose only unresolved repo was deleted has nothing for the modal to offer.
        assert!(unresolved_repos(&[deleted("a/two", "2026-10-24T17:40:19Z")]).is_empty());
    }

    #[test]
    fn scan_block_lists_deleted_repos_with_dates() {
        let repos = vec![
            res("a/one", true),
            deleted("a/two", "2026-10-24T17:40:19Z"),
            deleted("a/four", "2026-11-03T09:00:00Z"),
        ];
        let block = scan_block(&repos).expect("blocked");
        assert_eq!(
            block.repos,
            vec![
                ("a/two".to_string(), "2026-10-24T17:40:19Z".to_string()),
                ("a/four".to_string(), "2026-11-03T09:00:00Z".to_string()),
            ]
        );
        assert_eq!(
            scan_block_text(&ScanBlock {
                repos: vec![("acme/app".to_string(), "2026-10-24T17:40:19Z".to_string())]
            }),
            "Source for acme/app was deleted on 24 Oct 2026. Re-fetch source from the Workspace tab to rescan."
        );
        assert_eq!(
            scan_block_text(&block),
            "Source was deleted for a/two (24 Oct 2026), a/four (3 Nov 2026). Re-fetch source from the Workspace tab to rescan."
        );
    }

    #[test]
    fn scan_block_none_when_all_present() {
        assert_eq!(scan_block(&[res("a/one", true), res("a/two", false)]), None);
        assert_eq!(scan_block(&[]), None);
        // A missing date still reads as a sentence.
        let undated = ScanBlock {
            repos: vec![("a/b".to_string(), String::new())],
        };
        assert_eq!(
            scan_block_text(&undated),
            "Source for a/b was deleted. Re-fetch source from the Workspace tab to rescan."
        );
    }

    #[test]
    fn parse_maps_known_strings() {
        assert_eq!(ProjectReadiness::parse("ready"), ProjectReadiness::Ready);
        assert_eq!(
            ProjectReadiness::parse("partial"),
            ProjectReadiness::Partial
        );
        assert_eq!(
            ProjectReadiness::parse("unlinked"),
            ProjectReadiness::Unlinked
        );
    }

    #[test]
    fn parse_unknown_fails_safe_to_unlinked() {
        assert_eq!(ProjectReadiness::parse("wat"), ProjectReadiness::Unlinked);
        assert_eq!(ProjectReadiness::parse(""), ProjectReadiness::Unlinked);
    }

    #[test]
    fn is_ready_and_is_paused_are_complementary() {
        assert!(ProjectReadiness::Ready.is_ready());
        assert!(!ProjectReadiness::Ready.is_paused());
        assert!(ProjectReadiness::Unlinked.is_paused());
        assert!(!ProjectReadiness::Unlinked.is_ready());
        assert!(ProjectReadiness::Partial.is_paused());
    }

    #[test]
    fn unresolved_repos_filters_and_preserves_order() {
        let repos = vec![
            res("a/one", false),
            res("a/two", true),
            res("a/three", false),
        ];
        let un = unresolved_repos(&repos);
        assert_eq!(un.len(), 2);
        assert_eq!(un[0].repo, "a/one");
        assert_eq!(un[1].repo, "a/three");
    }

    #[test]
    fn ready_has_no_banner() {
        assert_eq!(paused_banner_text(ProjectReadiness::Ready, 0), None);
    }

    #[test]
    fn unlinked_banner_text() {
        let t = paused_banner_text(ProjectReadiness::Unlinked, 1).expect("banner");
        assert!(t.contains("Paused"));
        assert!(t.to_lowercase().contains("link"));
    }

    #[test]
    fn partial_banner_pluralizes() {
        let one = paused_banner_text(ProjectReadiness::Partial, 1).expect("banner");
        assert!(one.contains("1 repo "), "singular: {one}");
        let many = paused_banner_text(ProjectReadiness::Partial, 3).expect("banner");
        assert!(many.contains("3 repos "), "plural: {many}");
    }

    #[test]
    fn resolve_prompt_names_the_repo_and_offers_both_paths() {
        let t = resolve_prompt_text("owner/repo");
        assert!(t.contains("owner/repo"));
        assert!(t.contains("Clone it now"));
        assert!(t.contains("select a local clone"));
    }

    #[test]
    fn action_gated_when_paused_only() {
        assert!(!action_gated(ProjectReadiness::Ready));
        assert!(action_gated(ProjectReadiness::Unlinked));
        assert!(action_gated(ProjectReadiness::Partial));
    }

    #[test]
    fn modal_opens_when_paused_only() {
        assert!(!should_open_resolve_modal(ProjectReadiness::Ready));
        assert!(should_open_resolve_modal(ProjectReadiness::Unlinked));
        assert!(should_open_resolve_modal(ProjectReadiness::Partial));
    }

    // ── Bug regression: ResolveModal early-return guard (Bug 2) ─────────────────────────────────
    //
    // `ResolveModal` early-returns `rsx! {}` when `unresolved().is_empty()` so a Ready project
    // never shows the empty modal even if `modal_open` is still true after the last resolve.
    // This test confirms the load-bearing condition: a fully-resolved repo list yields an empty
    // unresolved set, which triggers the early-return.
    #[test]
    fn ready_project_has_empty_unresolved_repos() {
        // All repos resolved — this is the post-link state that triggers the empty-modal bug.
        let repos = vec![res("a/one", true), res("a/two", true)];
        let unresolved = unresolved_repos(&repos);
        assert!(
            unresolved.is_empty(),
            "a fully-resolved repo list must produce an empty unresolved set (ResolveModal early-return guard)"
        );
    }
}
