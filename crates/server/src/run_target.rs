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

    // The branch becomes a directory under the worktrees root, so it must be a real branch
    // name: a recorded `.` or `..` (from any writer, including data saved before validation
    // existed) is refused here, before any worktree code runs.
    if let Err(why) = camerata_api_types::uow::validate_branch_name(branch) {
        return RunTarget::Refused {
            reason: format!(
                "The branch recorded for this unit of work is unusable. {why} Fix the branch \
                 under Intake, Repos in scope, then start development again. (Unit `{story_id}`.)"
            ),
        };
    }

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
    if let Some(reason) = project_membership_refusal(story_id, project_repos) {
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
    RunTarget::Repository {
        repo: repo.to_string(),
        branch: branch.to_string(),
        clone,
    }
}

/// Why a unit of work may not run (or have a branch recorded) in the active project: its
/// repository, named in its id (`OWNER/REPO#N`), is not one of the project's repositories.
/// `None` when the unit belongs to the project, when the project lists no repositories (it
/// imposes no list) or when the id names no repository (callers refuse that on their own).
/// Repository names compare case-insensitively, as the hosting service treats them.
pub(crate) fn project_membership_refusal(
    story_id: &str,
    project_repos: Option<&[String]>,
) -> Option<String> {
    let project_repos = project_repos.filter(|r| !r.is_empty())?;
    let (repo, _) = story_id.rsplit_once('#')?;
    if project_repos.iter().any(|r| r.eq_ignore_ascii_case(repo)) {
        return None;
    }
    Some(format!(
        "This unit of work belongs to `{repo}`, which is not one of the active project's \
         repositories ({}). Switch to the project that owns it, or add `{repo}` to this \
         project, then try again. (Unit `{story_id}`.)",
        project_repos.join(", ")
    ))
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
        let ok = plan_run_target(
            "Acme/API#7",
            &uow(Some("camerata/fix-7"), &[]),
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

    #[test]
    fn a_recorded_branch_that_escapes_the_worktree_root_is_refused() {
        let clone = git_clone_dir();
        let path = clone.path().display().to_string();
        for bad in [".", "..", "a/../b", "-rf", "x\u{7}y"] {
            let target = plan_run_target(
                "acme/api#7",
                &uow(Some(bad), &["acme/api"]),
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
}
