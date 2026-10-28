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
    let branch = uow
        .branch
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty());
    let project_has_repos = project_repos.is_some_and(|r| !r.is_empty());
    let scoped: Vec<&str> = uow.intake.repos.iter().map(|s| s.repo.as_str()).collect();

    let Some(branch) = branch else {
        if !project_has_repos && scoped.is_empty() {
            return RunTarget::Greenfield;
        }
        return RunTarget::Refused {
            reason: format!(
                "This unit of work has no repository selected, so the run has nothing to work \
                 in. Select its repository under Intake, Repos in scope, then start \
                 development again. (Unit `{story_id}`; no branch is set.)"
            ),
        };
    };

    // Today a run's repository is the one named in the unit's id (`OWNER/REPO#N`).
    let Some((repo, _)) = story_id.rsplit_once('#') else {
        return RunTarget::Refused {
            reason: format!(
                "This unit of work (`{story_id}`) is not linked to a repository issue \
                 (`OWNER/REPO#N`), so the run has no repository to work in. Publish it to an \
                 issue first."
            ),
        };
    };
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
    RunTarget::Repository {
        repo: repo.to_string(),
        branch: branch.to_string(),
        clone,
    }
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
        let mut u = UnitOfWork {
            story_id: "acme/api#7".to_string(),
            branch: branch.map(str::to_string),
            ..Default::default()
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
    fn a_unit_with_scoped_repos_but_no_branch_is_refused_even_without_project_repos() {
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
    fn a_unit_not_linked_to_an_issue_is_refused() {
        let target = plan_run_target(
            "draft-1",
            &uow(Some("b"), &[]),
            Some(&repos(&["acme/api"])),
            |_| None,
            None,
        );
        let RunTarget::Refused { reason } = target else {
            panic!("expected a refusal, got {target:?}");
        };
        assert!(
            reason.contains("not linked to a repository issue"),
            "{reason}"
        );
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
}
