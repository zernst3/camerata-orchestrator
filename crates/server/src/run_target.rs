//! Where a governed development run writes its code.
//!
//! A live development run works in one of two places:
//!
//! - the unit of work's REAL repository (brownfield): a worktree of the local clone on the
//!   unit's branch, which the console records via `POST /api/uow/:id/branch` when the
//!   architect starts development; or
//! - a freshly scaffolded app in a temporary directory (greenfield), which is only correct
//!   for a project that has no repositories at all.
//!
//! Before this module, any unit whose branch was unset (every single-repository unit started
//! from the console) fell through to the greenfield scaffolder without a word. [`plan_run_target`]
//! makes the choice explicit: a unit that should work in a repository but cannot is
//! [`RunTarget::Refused`] with a reason the console shows, never silently scaffolded.
//!
//! The planner is pure (it only reads the unit, the project's repos, the configured paths and
//! whether the clone exists on disk), so the HTTP pre-flight and the run dispatch share it and
//! it is unit-tested without git or network.

use std::path::{Path, PathBuf};

use crate::uow::UnitOfWork;

/// The outcome of [`plan_run_target`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunTarget {
    /// Work in the unit's real repository: `clone` is the local clone of `repo`; the run
    /// creates (or reuses) the unit's worktree on `branch` off it.
    Repository {
        repo: String,
        branch: String,
        clone: PathBuf,
    },
    /// The project has no repositories and the unit names none, so the run scaffolds a new
    /// app in a temporary directory. The run announces this in its events.
    Greenfield,
    /// The unit should work in a repository but cannot; `reason` says why and what to do.
    Refused { reason: String },
}

/// The run event text announcing a greenfield run, so it is never silent.
pub(crate) const GREENFIELD_NOTICE: &str = "No repository: this project has no repositories \
     and this unit of work names none, so the run scaffolds a new app in a temporary directory.";

/// Decide where a development run for `story_id` writes.
///
/// `project_repos` are the active project's repositories (`None` = no active project).
/// `override_path` is the configured local path for a repo, if any; `workspace_root` is the
/// configured workspace root. Pure apart from checking whether the resolved clone is a git
/// repository on disk.
pub(crate) fn plan_run_target(
    story_id: &str,
    uow: &UnitOfWork,
    project_repos: Option<&[String]>,
    override_path: impl Fn(&str) -> Option<String>,
    workspace_root: Option<&str>,
) -> RunTarget {
    let recorded = recorded_branch(uow);
    let project_has_repos = project_repos.is_some_and(|r| !r.is_empty());
    let scoped: Vec<&str> = uow.intake.repos.iter().map(|s| s.repo.as_str()).collect();

    // No branch and no Intake scope: nothing says where to work.
    if recorded.is_none() && scoped.is_empty() {
        if !project_has_repos {
            return RunTarget::Greenfield;
        }
        return RunTarget::Refused {
            reason: format!(
                "This unit of work has no repository selected, so the run has nothing to work                  in. Select its repository under Intake, Repos in scope, then start                  development again. (Unit `{story_id}`; no branch is set.)"
            ),
        };
    }

    // The run's repository is the unit's own (`UnitOfWork::repo`): an issue's repository for a
    // GitHub-sourced unit, the one it was created with or scoped to for a native unit. It is
    // never read out of the unit's key.
    let Some(repo) = uow.repo() else {
        return RunTarget::Refused {
            reason: format!(
                "This unit of work (`{story_id}`) names no valid OWNER/REPO repository, so the \
                 run has no repository to work in. Select its repository under Intake, Repos \
                 in scope, or publish it to an issue, then start development again."
            ),
        };
    };
    if let Some(reason) = project_membership_refusal(story_id, Some(repo), project_repos) {
        return RunTarget::Refused { reason };
    }
    if !scoped.is_empty() && !scoped.contains(&repo) {
        return RunTarget::Refused {
            reason: format!(
                "Runs work in the issue's repository `{repo}`, which is not among this unit's \
                 Repos in scope ({}). Add `{repo}` to Repos in scope under Intake.",
                scoped.join(", ")
            ),
        };
    }

    let override_path = override_path(repo);
    let Some(clone) =
        crate::workspace::resolve_repo_dir(override_path.as_deref(), workspace_root, repo)
    else {
        return RunTarget::Refused {
            reason: format!(
                "No local clone of `{repo}` is configured. Set a workspace root, or a local \
                 path for `{repo}`, in Settings, then start development again."
            ),
        };
    };
    if !crate::workspace::is_git_repo(&clone) {
        return RunTarget::Refused {
            reason: format!(
                "The local path for `{repo}` ({}) is not a git clone. Clone the repository \
                 there, or point Settings at the existing clone, then start development again.",
                clone.display()
            ),
        };
    }

    // ONE derivation of the branch. With an Intake scope, the scope names it (it always has an
    // entry for `repo` here, checked above), so a scope changed after a branch was recorded
    // wins and the run records the change ([`record_run_branch`]). With no scope, the recorded
    // branch is the only source (the console's Begin Development and the orchestrator chat
    // record one). The result becomes a directory name, so it is validated.
    let Some(branch) = scope_branch_for_repo(uow, story_id, repo).or_else(|| recorded.clone())
    else {
        return RunTarget::Refused {
            reason: format!(
                "This unit of work has no branch for `{repo}`. Select the repository under                  Intake, Repos in scope, then start development again. (Unit `{story_id}`.)"
            ),
        };
    };
    if let Err(why) = camerata_api_types::uow::validate_branch_name(&branch) {
        return RunTarget::Refused {
            reason: format!(
                "The branch for this unit of work is unusable. {why} Fix the branch under                  Intake, Repos in scope, then start development again. (Unit `{story_id}`.)"
            ),
        };
    }

    RunTarget::Repository {
        repo: repo.to_string(),
        branch,
        clone,
    }
}

/// The branch recorded on the unit (`uow.branch`), trimmed; `None` when unset or blank.
fn recorded_branch(uow: &UnitOfWork) -> Option<String> {
    uow.branch
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .map(str::to_string)
}

/// The branch the Intake scope names for `repo`, derived with the shared
/// [`BranchMode::branch_for_story`]; `None` when `repo` is not in scope. The one place a
/// scope turns into a branch: the run target, multi-repo worktree setup and Ship all come
/// through here (directly or via [`unit_branch_for_repo`]).
pub(crate) fn scope_branch_for_repo(
    uow: &UnitOfWork,
    story_id: &str,
    repo: &str,
) -> Option<String> {
    uow.intake
        .repos
        .iter()
        .find(|s| s.repo == repo)
        .map(|s| s.branch.branch_for_story(story_id))
}

/// The branch multi-repo worktree setup and Ship use for `repo`. For the unit's own
/// repository ([`UnitOfWork::repo`]) that is the unit's recorded branch, the one the run
/// derived from the scope and committed to (so Ship pushes the branch that holds the work),
/// falling back to the scope when nothing is recorded yet. For any other in-scope repository
/// it is the scope's branch. `None` when `repo` is neither.
pub(crate) fn unit_branch_for_repo(uow: &UnitOfWork, story_id: &str, repo: &str) -> Option<String> {
    if uow.repo() == Some(repo) {
        if let Some(recorded) = recorded_branch(uow) {
            return Some(recorded);
        }
    }
    scope_branch_for_repo(uow, story_id, repo)
}

/// Record `branch` as the unit's branch when it differs from the recorded one, and say so in
/// the unit's history: a scope changed after a branch was recorded overwrites it on the next
/// run, never silently. A no-op when the value is unchanged.
pub(crate) fn record_run_branch(store: &crate::uow::UowStore, story_id: &str, branch: &str) {
    let previous = recorded_branch(&store.get_or_create(story_id));
    if previous.as_deref() == Some(branch) {
        return;
    }
    store.set_branch(story_id, Some(branch.to_string()));
    let text = match previous {
        Some(old) => format!(
            "Branch changed from `{old}` to `{branch}`: the Intake scope names a different              branch for this repository, so the run works on the new one."
        ),
        None => format!("Branch `{branch}` recorded from the Intake scope for this run."),
    };
    store.append_history(story_id, "branch", &text);
}

/// Is `repo` allowed in the active project? True when the project lists no repositories (it
/// imposes no list) or when `repo` is one of them. Names compare case-insensitively, as the
/// hosting service treats them. The ONE membership test: the run, the branch record, the
/// Intake scope and the worktree resolver all go through it.
pub(crate) fn repo_in_project(repo: &str, project_repos: Option<&[String]>) -> bool {
    match project_repos.filter(|r| !r.is_empty()) {
        None => true,
        Some(list) => list.iter().any(|r| r.eq_ignore_ascii_case(repo)),
    }
}

/// The refusal text when `repo` is not in the active project, or `None` when it is. `subject`
/// says what is being refused (it opens the sentence).
pub(crate) fn repo_membership_refusal(
    subject: &str,
    repo: &str,
    story_id: &str,
    project_repos: Option<&[String]>,
) -> Option<String> {
    if repo_in_project(repo, project_repos) {
        return None;
    }
    let list = project_repos.unwrap_or_default().join(", ");
    Some(format!(
        "{subject} `{repo}`, which is not one of the active project's repositories ({list}). \
         Switch to the project that owns it, or add `{repo}` to this project, then try again. \
         (Unit `{story_id}`.)"
    ))
}

/// Why a unit of work may not run (or have a branch recorded) in the active project: its
/// repository (`repo`, the unit's own [`UnitOfWork::repo`]) is not one of the project's
/// repositories. `None` when the unit belongs to the project, when the project lists no
/// repositories or when the unit names no repository (callers refuse that on their own).
pub(crate) fn project_membership_refusal(
    story_id: &str,
    repo: Option<&str>,
    project_repos: Option<&[String]>,
) -> Option<String> {
    repo_membership_refusal(
        "This unit of work belongs to",
        repo?,
        story_id,
        project_repos,
    )
}

/// The same check for a repository in the unit's Intake scope (multi-repo work): a scope
/// naming a repository outside the active project is refused.
pub(crate) fn scope_membership_refusal(
    story_id: &str,
    repo: &str,
    project_repos: Option<&[String]>,
) -> Option<String> {
    repo_membership_refusal(
        "This unit's Repos in scope include",
        repo,
        story_id,
        project_repos,
    )
}

/// [`plan_run_target`] against the live state: the active project and the configured paths.
pub(crate) fn plan_run_target_for_state(
    state: &crate::AppState,
    story_id: &str,
    uow: &UnitOfWork,
) -> RunTarget {
    let project = state.projects.active();
    let workspace_root = state.settings.workspace_root();
    plan_run_target(
        story_id,
        uow,
        project.as_ref().map(|p| p.repos.as_slice()),
        |repo| state.settings.repo_path(repo),
        workspace_root.as_deref(),
    )
}

/// Create (or reuse) the unit's worktree for a [`RunTarget::Repository`]. A failure is
/// reported as a refusal reason, never a fall-through to the greenfield scaffolder.
pub(crate) async fn ensure_target_worktree(clone: &Path, branch: &str) -> Result<PathBuf, String> {
    crate::workspace::ensure_uow_worktree(clone, branch)
        .await
        .map_err(|e| {
            format!(
                "Could not prepare the worktree for branch `{branch}` in {}: {e}",
                clone.display()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uow::{BranchMode, RepoScope};

    fn uow(branch: Option<&str>, scoped: &[&str]) -> UnitOfWork {
        // Keyed like an issue unit, so it carries `acme/api` as its own repository field.
        let mut u = UnitOfWork {
            branch: branch.map(str::to_string),
            ..UnitOfWork::keyed("acme/api#7")
        };
        u.intake.repos = scoped
            .iter()
            .map(|r| RepoScope {
                repo: r.to_string(),
                branch: BranchMode::default(),
            })
            .collect();
        u
    }

    fn git_clone_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        dir
    }

    fn repos(r: &[&str]) -> Vec<String> {
        r.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_unit_with_a_branch_and_a_clone_targets_its_repository() {
        let clone = git_clone_dir();
        let path = clone.path().display().to_string();
        let target = plan_run_target(
            "acme/api#7",
            &uow(Some("camerata/acme/api#7"), &["acme/api"]),
            Some(&repos(&["acme/api"])),
            |r| (r == "acme/api").then(|| path.clone()),
            None,
        );
        assert_eq!(
            target,
            RunTarget::Repository {
                repo: "acme/api".to_string(),
                branch: "camerata/acme/api#7".to_string(),
                clone: clone.path().to_path_buf(),
            }
        );
    }

    #[test]
    fn a_unit_without_a_branch_in_a_project_with_repos_is_refused_not_scaffolded() {
        let target = plan_run_target(
            "acme/api#7",
            &uow(None, &[]),
            Some(&repos(&["acme/api"])),
            |_| None,
            None,
        );
        let RunTarget::Refused { reason } = target else {
            panic!("expected a refusal, got {target:?}");
        };
        assert!(reason.contains("no repository selected"), "{reason}");
        assert!(reason.contains("Repos in scope"), "{reason}");
    }

    #[test]
    fn a_scoped_unit_whose_repository_has_no_configured_clone_is_refused() {
        let target = plan_run_target(
            "acme/api#7",
            &uow(None, &["acme/api"]),
            None,
            |_| None,
            None,
        );
        assert!(matches!(target, RunTarget::Refused { .. }), "{target:?}");
    }

    #[test]
    fn a_project_without_repos_and_a_unit_without_one_is_greenfield() {
        assert_eq!(
            plan_run_target("draft-1", &uow(None, &[]), Some(&[]), |_| None, None),
            RunTarget::Greenfield
        );
        assert_eq!(
            plan_run_target("draft-1", &uow(None, &[]), None, |_| None, None),
            RunTarget::Greenfield
        );
    }

    #[test]
    fn a_branch_without_a_configured_clone_is_refused_with_the_settings_hint() {
        let target = plan_run_target(
            "acme/api#7",
            &uow(Some("b"), &[]),
            Some(&repos(&["acme/api"])),
            |_| None,
            None,
        );
        let RunTarget::Refused { reason } = target else {
            panic!("expected a refusal, got {target:?}");
        };
        assert!(reason.contains("No local clone of `acme/api`"), "{reason}");
    }

    #[test]
    fn a_configured_path_that_is_not_a_git_clone_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let target = plan_run_target(
            "acme/api#7",
            &uow(Some("b"), &[]),
            Some(&repos(&["acme/api"])),
            |_| Some(dir.path().display().to_string()),
            None,
        );
        let RunTarget::Refused { reason } = target else {
            panic!("expected a refusal, got {target:?}");
        };
        assert!(reason.contains("is not a git clone"), "{reason}");
    }

    #[test]
    fn the_issue_repository_must_be_in_scope_when_a_scope_is_set() {
        let clone = git_clone_dir();
        let path = clone.path().display().to_string();
        let target = plan_run_target(
            "acme/api#7",
            &uow(Some("b"), &["acme/web"]),
            Some(&repos(&["acme/api", "acme/web"])),
            |_| Some(path.clone()),
            None,
        );
        let RunTarget::Refused { reason } = target else {
            panic!("expected a refusal, got {target:?}");
        };
        assert!(
            reason.contains("not among this unit's Repos in scope"),
            "{reason}"
        );
    }

    #[test]
    fn an_issue_repository_outside_the_active_project_is_refused() {
        let clone = git_clone_dir();
        let path = clone.path().display().to_string();
        // The unit's id names `acme/api`, but the active project only has other repositories.
        let target = plan_run_target(
            "acme/api#7",
            &uow(Some("camerata/fix-7"), &[]),
            Some(&repos(&["other/web", "other/db"])),
            |_| Some(path.clone()),
            None,
        );
        let RunTarget::Refused { reason } = target else {
            panic!("expected a refusal, got {target:?}");
        };
        assert!(
            reason.contains("not one of the active project's repositories"),
            "{reason}"
        );
        assert!(
            reason.contains("`acme/api`") && reason.contains("other/web"),
            "{reason}"
        );

        // A repository of the project (any letter case, as GitHub treats them) is fine.
        let mut upper = uow(Some("camerata/fix-7"), &[]);
        upper.repo = Some("Acme/API".to_string());
        let ok = plan_run_target(
            "Acme/API#7",
            &upper,
            Some(&repos(&["acme/api"])),
            |_| Some(path.clone()),
            None,
        );
        assert!(matches!(ok, RunTarget::Repository { .. }), "{ok:?}");

        // A project with no repositories imposes no list.
        let none = plan_run_target(
            "acme/api#7",
            &uow(Some("camerata/fix-7"), &[]),
            Some(&[]),
            |_| Some(path.clone()),
            None,
        );
        assert!(matches!(none, RunTarget::Repository { .. }), "{none:?}");
    }

    fn existing(branch: &str) -> BranchMode {
        BranchMode::Existing {
            branch_name: branch.to_string(),
        }
    }

    #[test]
    fn the_scope_names_the_branch_and_wins_over_a_stale_recorded_one() {
        let clone = git_clone_dir();
        let path = clone.path().display().to_string();
        let mut u = uow(Some("feature/old"), &["acme/api"]);
        u.intake.repos[0].branch = existing("feature/new");
        let target = plan_run_target(
            "acme/api#7",
            &u,
            Some(&repos(&["acme/api"])),
            |_| Some(path.clone()),
            None,
        );
        assert_eq!(
            target,
            RunTarget::Repository {
                repo: "acme/api".to_string(),
                branch: "feature/new".to_string(),
                clone: clone.path().to_path_buf(),
            }
        );
    }

    #[test]
    fn a_scoped_unit_with_no_recorded_branch_derives_one_from_its_scope() {
        let clone = git_clone_dir();
        let path = clone.path().display().to_string();
        let target = plan_run_target(
            "acme/api#7",
            &uow(None, &["acme/api"]),
            Some(&repos(&["acme/api"])),
            |_| Some(path.clone()),
            None,
        );
        assert_eq!(
            target,
            RunTarget::Repository {
                repo: "acme/api".to_string(),
                branch: "camerata/acme/api#7".to_string(),
                clone: clone.path().to_path_buf(),
            }
        );
    }

    #[test]
    fn a_scope_that_names_an_escaping_branch_is_refused_whatever_is_recorded() {
        let clone = git_clone_dir();
        let path = clone.path().display().to_string();
        for bad in [".", "..", "a/../b"] {
            let mut u = uow(Some("feature/ok"), &["acme/api"]);
            u.intake.repos[0].branch = existing(bad);
            let target = plan_run_target(
                "acme/api#7",
                &u,
                Some(&repos(&["acme/api"])),
                |_| Some(path.clone()),
                None,
            );
            let RunTarget::Refused { reason } = target else {
                panic!("scope branch {bad:?} must be refused, got {target:?}");
            };
            assert!(reason.contains("not a valid branch name"), "{reason}");
        }
    }

    #[test]
    fn ship_and_multi_repo_setup_read_the_same_branch_the_run_works_on() {
        let mut u = uow(Some("feature/new"), &["acme/api", "acme/web"]);
        u.intake.repos[0].branch = existing("feature/new");
        u.intake.repos[1].branch = BranchMode::NewFromBase {
            base: "main".to_string(),
            new_name: "camerata/web-7".to_string(),
        };
        let story = "acme/api#7";
        assert_eq!(
            unit_branch_for_repo(&u, story, "acme/api").as_deref(),
            Some("feature/new")
        );
        assert_eq!(
            unit_branch_for_repo(&u, story, "acme/web").as_deref(),
            Some("camerata/web-7")
        );
        assert_eq!(unit_branch_for_repo(&u, story, "acme/other"), None);

        // Between a scope change and the next run, the branch that holds the work is the
        // recorded one: Ship pushes it, rather than a branch that has no commits yet.
        let mut stale = u.clone();
        stale.branch = Some("feature/old".to_string());
        assert_eq!(
            unit_branch_for_repo(&stale, story, "acme/api").as_deref(),
            Some("feature/old")
        );
        // The unit's own repository with nothing recorded derives from the scope.
        let mut fresh = u.clone();
        fresh.branch = None;
        assert_eq!(
            unit_branch_for_repo(&fresh, story, "acme/api").as_deref(),
            Some("feature/new")
        );
    }

    #[test]
    fn recording_a_changed_branch_overwrites_it_and_says_so_in_the_history() {
        let store = crate::uow::UowStore::new();
        let story = "acme/api#7";
        store.set_branch(story, Some("feature/old".to_string()));

        record_run_branch(&store, story, "feature/new");
        let u = store.get_or_create(story);
        assert_eq!(u.branch.as_deref(), Some("feature/new"));
        let entry = u
            .history
            .iter()
            .find(|h| h.kind == "branch")
            .expect("the change is recorded in the unit's history");
        assert!(
            entry.text.contains("feature/old") && entry.text.contains("feature/new"),
            "{}",
            entry.text
        );
        assert!(!entry.text.contains('\u{2014}'));

        // Recording the value that is already there changes and logs nothing.
        record_run_branch(&store, story, "feature/new");
        assert_eq!(
            store
                .get_or_create(story)
                .history
                .iter()
                .filter(|h| h.kind == "branch")
                .count(),
            1
        );

        // A first record (nothing before it) is logged too.
        let first = "acme/api#8";
        record_run_branch(&store, first, "camerata/acme/api#8");
        let u = store.get_or_create(first);
        assert_eq!(u.branch.as_deref(), Some("camerata/acme/api#8"));
        assert!(u.history.iter().any(|h| h.kind == "branch"));
    }

    #[test]
    fn a_recorded_branch_that_escapes_the_worktree_root_is_refused() {
        let clone = git_clone_dir();
        let path = clone.path().display().to_string();
        for bad in [".", "..", "a/../b", "-rf", "x\u{7}y"] {
            let target = plan_run_target(
                "acme/api#7",
                &uow(Some(bad), &[]),
                Some(&repos(&["acme/api"])),
                |_| Some(path.clone()),
                None,
            );
            let RunTarget::Refused { reason } = target else {
                panic!("branch {bad:?} must be refused, got {target:?}");
            };
            assert!(reason.contains("not a valid branch name"), "{reason}");
        }
    }

    #[test]
    fn a_unit_that_names_no_repository_is_refused() {
        let draft = UnitOfWork {
            branch: Some("b".to_string()),
            ..UnitOfWork::keyed("draft-1")
        };
        let target = plan_run_target(
            "draft-1",
            &draft,
            Some(&repos(&["acme/api"])),
            |_| None,
            None,
        );
        let RunTarget::Refused { reason } = target else {
            panic!("expected a refusal, got {target:?}");
        };
        assert!(reason.contains("names no valid OWNER/REPO"), "{reason}");
    }

    /// The run's repository is the unit's own field, never read out of the story id passed in
    /// beside it: a key that merely looks like `OWNER/REPO#N` for another repository, or like
    /// nothing at all, changes nothing.
    #[test]
    fn the_repository_comes_from_the_unit_not_from_the_story_id() {
        let clone = git_clone_dir();
        let path = clone.path().display().to_string();
        let native = UnitOfWork {
            story_id: "uow-3".to_string(),
            repo: Some("acme/api".to_string()),
            native_number: Some(3),
            branch: Some("camerata/uow-3".to_string()),
            ..Default::default()
        };
        for key in ["uow-3", "evil/other#9", "no-separator", "a#b#c"] {
            let target = plan_run_target(
                key,
                &native,
                Some(&repos(&["acme/api"])),
                |r| (r == "acme/api").then(|| path.clone()),
                None,
            );
            assert_eq!(
                target,
                RunTarget::Repository {
                    repo: "acme/api".to_string(),
                    branch: "camerata/uow-3".to_string(),
                    clone: clone.path().to_path_buf(),
                },
                "key {key:?}"
            );
        }
    }

    /// A native unit's repository gets the same project check as an issue's.
    #[test]
    fn a_native_unit_in_a_repository_outside_the_project_is_refused() {
        let native = UnitOfWork {
            story_id: "uow-3".to_string(),
            repo: Some("acme/api".to_string()),
            native_number: Some(3),
            branch: Some("camerata/uow-3".to_string()),
            ..Default::default()
        };
        let RunTarget::Refused { reason } = plan_run_target(
            "uow-3",
            &native,
            Some(&repos(&["other/web"])),
            |_| None,
            None,
        ) else {
            panic!("expected a refusal");
        };
        assert!(
            reason.contains("not one of the active project's repositories"),
            "{reason}"
        );
    }

    /// A repository that is not `OWNER/REPO` (a hand-edited record, a stale scope) is no
    /// repository: the run is refused before any path is built from it.
    #[test]
    fn a_unit_whose_repository_would_escape_the_workspace_is_refused() {
        let clone = git_clone_dir();
        let path = clone.path().display().to_string();
        for bad in ["../../other", "..", "a/b/c", "acme/a pi", "/etc"] {
            let native = UnitOfWork {
                story_id: "uow-3".to_string(),
                repo: Some(bad.to_string()),
                native_number: Some(3),
                branch: Some("camerata/uow-3".to_string()),
                ..Default::default()
            };
            let target = plan_run_target("uow-3", &native, None, |_| Some(path.clone()), None);
            assert!(
                matches!(target, RunTarget::Refused { .. }),
                "repo {bad:?} must not be run in: {target:?}"
            );
        }
    }

    #[test]
    fn the_greenfield_notice_and_refusals_carry_no_em_dash() {
        assert!(!GREENFIELD_NOTICE.contains('\u{2014}'));
        let RunTarget::Refused { reason } = plan_run_target(
            "acme/api#7",
            &uow(None, &["acme/api"]),
            None,
            |_| None,
            None,
        ) else {
            panic!("expected a refusal");
        };
        assert!(!reason.contains('\u{2014}'));
    }
}

/// The console flow end to end at the HTTP layer: `POST /api/uow/:id/branch` (what the
/// console calls when the architect starts development) makes `POST /api/stories/:id/run`
/// work in the real repository; without it the run is refused with the reason.
///
/// These tests turn live mode on but never let an agent run: the refusal returns before any
/// run exists, and the brownfield test asserts synchronously right after the response (the
/// worktree is created BEFORE the agent task is spawned), so the test runtime is dropped
/// before the spawned agent task can reach `claude`.
#[cfg(test)]
mod http_tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::AppState;

    const STORY: &str = "acme/api#7";
    const STORY_SEG: &str = "acme%2Fapi%237";

    fn approve_a_decision(state: &AppState) {
        use camerata_worktracker::investigation::DecisionRecord;
        let now = chrono::Utc::now();
        let d = DecisionRecord::ai_proposed(
            STORY,
            format!("{STORY}/decision/1"),
            "T",
            "Q?",
            "R",
            vec![],
            now,
        )
        .approve(now);
        state.uow.set_decisions(STORY, vec![d]);
    }

    fn state_with_project() -> AppState {
        let state = AppState::seeded();
        state
            .projects
            .create("Acme", vec!["acme/api".to_string()])
            .unwrap();
        approve_a_decision(&state);
        state
    }

    async fn post(
        state: &AppState,
        uri: &str,
        body: serde_json::Value,
    ) -> axum::response::Response {
        crate::router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn a_run_for_a_unit_with_no_repository_selected_is_refused_with_the_reason() {
        let _live = camerata_test_env::EnvGuard::set("CAMERATA_LIVE_BUILD", "1");
        let state = state_with_project();
        let stage_before = state.uow.get_or_create(STORY).stage;

        let resp = post(
            &state,
            &format!("/api/stories/{STORY_SEG}/run"),
            serde_json::json!({}),
        )
        .await;

        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let json: serde_json::Value =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        let reason = json["reason"].as_str().unwrap_or_default();
        assert!(reason.contains("no repository selected"), "{reason}");
        assert!(reason.contains("Repos in scope"), "{reason}");
        assert!(
            state.runs.active_run_for_story(STORY).is_none(),
            "a refused start creates no run (and so no greenfield scaffold)"
        );
        assert_eq!(
            state.uow.get_or_create(STORY).stage,
            stage_before,
            "the refusal runs before the development gate, so the stage is untouched"
        );
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn setting_the_branch_makes_the_run_work_in_the_real_repository() {
        let _live = camerata_test_env::EnvGuard::set("CAMERATA_LIVE_BUILD", "1");
        let _token = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN");
        let clone = tempfile::tempdir().unwrap();
        crate::test_git::init_repo(clone.path(), &[("README.md", "fixture\n")]);
        let state = state_with_project();
        state
            .settings
            .set_repo_path("acme/api", Some(clone.path().display().to_string()));

        // The console's Begin Development: record the branch, then start the run.
        let resp = post(
            &state,
            &format!("/api/uow/{STORY_SEG}/branch"),
            serde_json::json!({ "branch": "camerata/fix-7" }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let resp = post(
            &state,
            &format!("/api/stories/{STORY_SEG}/run"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);

        // Brownfield: the unit's worktree exists on its branch in the REAL clone, and the run
        // carries no greenfield notice.
        let worktrees = crate::test_git::git(clone.path(), &["worktree", "list"]);
        assert!(worktrees.contains("[camerata/fix-7]"), "{worktrees}");
        let run = state
            .runs
            .active_run_for_story(STORY)
            .expect("the run started");
        assert!(
            !run.events
                .iter()
                .any(|e| e.detail == super::GREENFIELD_NOTICE),
            "a unit with a repository must not be scaffolded"
        );
    }

    /// A caller that skips the HTTP pre-flight (the orchestrator chat calls
    /// `start_governed_run` directly) still never scaffolds: the run fails with the reason.
    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn a_direct_governed_run_without_a_repository_fails_with_the_reason() {
        let _live = camerata_test_env::EnvGuard::set("CAMERATA_LIVE_BUILD", "1");
        let state = state_with_project();

        let (run_id, mode) = crate::start_governed_run(
            &state,
            STORY,
            None,
            None,
            false,
            crate::run::RunKind::Watched,
        )
        .await;

        assert_eq!(mode, "live");
        let run = state.runs.get(&run_id).unwrap();
        let reason = run.failure_reason.unwrap_or_default();
        assert!(reason.contains("no repository selected"), "{reason}");
        assert!(state
            .uow
            .get_or_create(STORY)
            .history
            .iter()
            .any(|h| h.text.starts_with("Development run refused: ")));
    }

    #[tokio::test]
    async fn the_branch_endpoint_refuses_names_that_escape_the_worktree_root() {
        let state = state_with_project();
        for bad in [".", "..", "a/../b", "-rf"] {
            let resp = post(
                &state,
                &format!("/api/uow/{STORY_SEG}/branch"),
                serde_json::json!({ "branch": bad }),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{bad:?}");
            assert_eq!(
                state.uow.get_or_create(STORY).branch,
                None,
                "{bad:?} must not be recorded"
            );
        }
        let ok = post(
            &state,
            &format!("/api/uow/{STORY_SEG}/branch"),
            serde_json::json!({ "branch": "camerata/fix-7" }),
        )
        .await;
        assert_eq!(ok.status(), StatusCode::OK);
        let cleared = post(
            &state,
            &format!("/api/uow/{STORY_SEG}/branch"),
            serde_json::json!({ "branch": null }),
        )
        .await;
        assert_eq!(cleared.status(), StatusCode::OK);
        assert_eq!(state.uow.get_or_create(STORY).branch, None);
    }

    #[tokio::test]
    async fn the_intake_scope_refuses_branch_names_that_escape_the_worktree_root() {
        let state = state_with_project();
        for (mode, field) in [("existing", "branch_name"), ("new_from_base", "new_name")] {
            for bad in [".", "..", "a/../b"] {
                let resp = post(
                    &state,
                    &format!("/api/uow/{STORY_SEG}/intake/repos"),
                    serde_json::json!({ "repos": [
                        { "repo": "acme/api", "branch": { "mode": mode, field: bad } }
                    ]}),
                )
                .await;
                assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{mode} {bad:?}");
                assert!(
                    state.uow.get_or_create(STORY).intake.repos.is_empty(),
                    "{mode} {bad:?} must not be recorded"
                );
            }
        }
        // A blank name (derived later) and an ordinary name are fine.
        let resp = post(
            &state,
            &format!("/api/uow/{STORY_SEG}/intake/repos"),
            serde_json::json!({ "repos": [
                { "repo": "acme/api", "branch": { "mode": "existing", "branch_name": "feature/x" } }
            ]}),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// A branch recorded before validation existed (or by any other writer) must still never
    /// reach the worktree code: the run is refused and the shared clone and the worktrees
    /// root are untouched.
    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn a_recorded_escaping_branch_is_refused_and_destroys_nothing() {
        let _live = camerata_test_env::EnvGuard::set("CAMERATA_LIVE_BUILD", "1");
        let _token = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN");
        let clone = tempfile::tempdir().unwrap();
        crate::test_git::init_repo(clone.path(), &[("README.md", "fixture\n")]);
        let other_unit = clone
            .path()
            .join(".camerata-worktrees")
            .join("camerata__other");
        std::fs::create_dir_all(&other_unit).unwrap();
        std::fs::write(other_unit.join("work.txt"), "uncommitted").unwrap();
        let state = state_with_project();
        state
            .settings
            .set_repo_path("acme/api", Some(clone.path().display().to_string()));

        for bad in [".", ".."] {
            state.uow.set_branch(STORY, Some(bad.to_string()));
            let resp = post(
                &state,
                &format!("/api/stories/{STORY_SEG}/run"),
                serde_json::json!({}),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::CONFLICT, "{bad:?}");
            assert!(other_unit.join("work.txt").exists(), "{bad:?} wiped work");
            assert!(clone.path().join("README.md").exists());
            assert!(state.runs.active_run_for_story(STORY).is_none());
        }
    }

    /// A story id from ANOTHER project must neither get a branch recorded nor start a run in
    /// this project: the unit's repository is not one of the active project's repositories.
    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn a_unit_from_another_project_gets_no_branch_and_no_run() {
        let _live = camerata_test_env::EnvGuard::set("CAMERATA_LIVE_BUILD", "1");
        let clone = tempfile::tempdir().unwrap();
        crate::test_git::init_repo(clone.path(), &[("README.md", "fixture\n")]);
        let state = state_with_project();
        state
            .settings
            .set_repo_path("other/web", Some(clone.path().display().to_string()));
        let other = "other/web#3";
        let other_seg = "other%2Fweb%233";
        state
            .uow
            .set_branch(other, Some("camerata/other-3".to_string()));

        let resp = post(
            &state,
            &format!("/api/stories/{other_seg}/run"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let json: serde_json::Value =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        let reason = json["reason"].as_str().unwrap_or_default();
        assert!(
            reason.contains("not one of the active project's repositories"),
            "{reason}"
        );
        assert!(state.runs.active_run_for_story(other).is_none());
        let worktrees = crate::test_git::git(clone.path(), &["worktree", "list"]);
        assert!(!worktrees.contains("camerata/other-3"), "{worktrees}");

        // Setting a branch for it is refused too, and records nothing.
        let fresh = "other/web#9";
        let resp = post(
            &state,
            "/api/uow/other%2Fweb%239/branch",
            serde_json::json!({ "branch": "camerata/other-9" }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert_eq!(state.uow.get_or_create(fresh).branch, None);

        // A unit of the project's own repository is still accepted.
        let resp = post(
            &state,
            &format!("/api/uow/{STORY_SEG}/branch"),
            serde_json::json!({ "branch": "camerata/fix-7" }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// One source of truth for the branch: after a branch was recorded, a changed Intake
    /// scope makes the NEXT run re-derive it, work on the new branch, overwrite the recorded
    /// value and say so in the unit's history.
    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn changing_the_scope_after_a_branch_was_recorded_switches_the_next_run() {
        let _live = camerata_test_env::EnvGuard::set("CAMERATA_LIVE_BUILD", "1");
        let _token = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN");
        let clone = tempfile::tempdir().unwrap();
        crate::test_git::init_repo(clone.path(), &[("README.md", "fixture\n")]);
        let state = state_with_project();
        state
            .settings
            .set_repo_path("acme/api", Some(clone.path().display().to_string()));

        // An earlier run recorded feature/a; the architect then changes the scope to feature/b.
        state.uow.set_branch(STORY, Some("feature/a".to_string()));
        let resp = post(
            &state,
            &format!("/api/uow/{STORY_SEG}/intake/repos"),
            serde_json::json!({ "repos": [
                { "repo": "acme/api", "branch": { "mode": "existing", "branch_name": "feature/b" } }
            ]}),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = post(
            &state,
            &format!("/api/stories/{STORY_SEG}/run"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);

        let worktrees = crate::test_git::git(clone.path(), &["worktree", "list"]);
        assert!(worktrees.contains("[feature/b]"), "{worktrees}");
        assert!(!worktrees.contains("[feature/a]"), "{worktrees}");
        let uow = state.uow.get_or_create(STORY);
        assert_eq!(uow.branch.as_deref(), Some("feature/b"));
        assert!(
            uow.history.iter().any(|h| h.kind == "branch"
                && h.text.contains("feature/a")
                && h.text.contains("feature/b")),
            "the switch is recorded in the history: {:?}",
            uow.history
        );
        // Ship and multi-repo setup read the value the run recorded.
        assert_eq!(
            super::unit_branch_for_repo(&uow, STORY, "acme/api").as_deref(),
            Some("feature/b")
        );
    }

    /// The Intake scope gets the same project check as the run and the branch record: a repo
    /// of another project cannot be scoped into this unit, and nothing is recorded.
    #[tokio::test]
    async fn the_intake_scope_refuses_a_repo_outside_the_active_project() {
        let state = state_with_project();
        let resp = post(
            &state,
            &format!("/api/uow/{STORY_SEG}/intake/repos"),
            serde_json::json!({ "repos": [
                { "repo": "acme/api", "branch": { "mode": "existing", "branch_name": "feature/x" } },
                { "repo": "other/web", "branch": { "mode": "existing", "branch_name": "feature/x" } }
            ]}),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let json: serde_json::Value =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        let reason = json["reason"].as_str().unwrap_or_default();
        assert!(
            reason.contains("`other/web`")
                && reason.contains("not one of the active project's repositories"),
            "{reason}"
        );
        assert!(
            state.uow.get_or_create(STORY).intake.repos.is_empty(),
            "a refused scope records nothing"
        );

        // Repos of the project are accepted (any letter case).
        let resp = post(
            &state,
            &format!("/api/uow/{STORY_SEG}/intake/repos"),
            serde_json::json!({ "repos": [
                { "repo": "acme/api", "branch": { "mode": "existing", "branch_name": "feature/x" } }
            ]}),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// Multi-repo setup resolves a worktree per scoped repo; a scope that already holds a repo
    /// outside the project (saved before the check existed) gets no worktree for it.
    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn multi_repo_setup_makes_no_worktree_for_a_repo_outside_the_project() {
        let _live = camerata_test_env::EnvGuard::set("CAMERATA_LIVE_BUILD", "1");
        let _token = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN");
        let own = tempfile::tempdir().unwrap();
        crate::test_git::init_repo(own.path(), &[("README.md", "fixture\n")]);
        let foreign = tempfile::tempdir().unwrap();
        crate::test_git::init_repo(foreign.path(), &[("README.md", "fixture\n")]);
        let state = state_with_project();
        state
            .settings
            .set_repo_path("acme/api", Some(own.path().display().to_string()));
        state
            .settings
            .set_repo_path("other/web", Some(foreign.path().display().to_string()));
        let scope = |repo: &str| crate::uow::RepoScope {
            repo: repo.to_string(),
            branch: crate::uow::BranchMode::Existing {
                branch_name: "feature/x".to_string(),
            },
        };
        state
            .uow
            .set_intake_repos(STORY, vec![scope("acme/api"), scope("other/web")])
            .unwrap();

        let resp = post(
            &state,
            &format!("/api/stories/{STORY_SEG}/run"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);

        let own_trees = crate::test_git::git(own.path(), &["worktree", "list"]);
        assert!(own_trees.contains("[feature/x]"), "{own_trees}");
        let foreign_trees = crate::test_git::git(foreign.path(), &["worktree", "list"]);
        assert!(
            !foreign_trees.contains("feature/x"),
            "no worktree in a repo outside the project: {foreign_trees}"
        );
        assert!(!foreign.path().join(".camerata-worktrees").exists());
    }
}
