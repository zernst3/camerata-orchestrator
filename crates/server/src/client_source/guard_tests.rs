//! The remote-match guard, one refusal at a time. Every refusal test also asserts the checkout
//! is still on disk: a refusal that deleted anything would be worse than no guard.

use std::path::Path;

use super::guard::{guard, Intent};
use super::test_support::{fixture, Fx, REPO};
use crate::test_git::{commit_all, git, write_file};

async fn code(fx: &Fx, intent: Intent<'_>) -> &'static str {
    match guard(&fx.state, &fx.project_id, REPO, intent).await {
        Ok(_) => "ok",
        Err(refusal) => refusal.code,
    }
}

fn delete_intent(include_uncommitted: bool) -> Intent<'static> {
    Intent::Delete {
        confirm: REPO,
        include_uncommitted,
    }
}

fn set_origin(path: &Path, url: &str) {
    git(path, &["remote", "set-url", "origin", url]);
}

fn point_repo_at(fx: &Fx, path: &Path) {
    fx.state
        .settings
        .set_repo_path(REPO, Some(path.to_string_lossy().into_owned()));
}

#[tokio::test]
async fn guard_accepts_matching_https_origin() {
    let fx = fixture();
    assert_eq!(code(&fx, delete_intent(false)).await, "ok");
}

#[tokio::test]
async fn guard_accepts_matching_ssh_origin() {
    let fx = fixture();
    set_origin(&fx.checkout, "git@github.com:acme/app.git");
    assert_eq!(code(&fx, delete_intent(false)).await, "ok");
}

#[tokio::test]
async fn guard_accepts_case_different_origin() {
    let fx = fixture();
    set_origin(&fx.checkout, "https://github.com/Acme/App.git");
    assert_eq!(code(&fx, delete_intent(false)).await, "ok");
}

#[tokio::test]
async fn guard_accepts_trailing_slash_and_dot_git_variants() {
    let fx = fixture();
    for url in [
        "https://github.com/acme/app/",
        "https://github.com/acme/app.git/",
        "https://github.com/acme/app",
        "ssh://git@github.com/acme/app.git",
    ] {
        set_origin(&fx.checkout, url);
        assert_eq!(code(&fx, delete_intent(false)).await, "ok", "{url}");
    }
}

#[tokio::test]
async fn guard_refuses_repo_not_in_project() {
    let fx = fixture();
    let refusal = guard(&fx.state, &fx.project_id, "acme/nope", Intent::Preview)
        .await
        .err()
        .expect("refused");
    assert_eq!(refusal.code, "repo_not_in_project");
    let refusal = guard(&fx.state, "no-such-project", REPO, Intent::Preview)
        .await
        .err()
        .expect("refused");
    assert_eq!(refusal.code, "unknown_project");
    assert!(fx.checkout.exists());
}

#[tokio::test]
async fn guard_refuses_when_no_path_is_recorded() {
    let fx = fixture();
    fx.state.settings.set_workspace_root(None);
    assert_eq!(code(&fx, delete_intent(false)).await, "no_path");
    assert!(fx.checkout.exists());
}

#[tokio::test]
async fn guard_refuses_missing_directory_as_nothing_to_delete() {
    let fx = fixture();
    // A path that does not exist.
    point_repo_at(&fx, &fx.root.join("work/acme/never-cloned"));
    assert_eq!(code(&fx, delete_intent(false)).await, "not_a_directory");
    // A path that is a FILE.
    let file = fx.root.join("work/acme/a-file");
    std::fs::write(&file, "x").unwrap();
    point_repo_at(&fx, &file);
    assert_eq!(code(&fx, delete_intent(false)).await, "not_a_directory");
    assert!(fx.checkout.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn guard_refuses_symlinked_path() {
    let fx = fixture();
    // The LAST component is a link to the real checkout.
    let link = fx.root.join("work/acme/app-link");
    std::os::unix::fs::symlink(&fx.checkout, &link).unwrap();
    point_repo_at(&fx, &link);
    assert_eq!(code(&fx, delete_intent(false)).await, "symlinked_path");

    // A MIDDLE component is a link: <root>/via/app, where <root>/via -> <root>/work/acme.
    let via = fx.root.join("via");
    std::os::unix::fs::symlink(fx.root.join("work/acme"), &via).unwrap();
    point_repo_at(&fx, &via.join("app"));
    assert_eq!(code(&fx, delete_intent(false)).await, "symlinked_path");

    // The real checkout behind both links was not touched, and is still deletable by its real path.
    assert!(fx.checkout.join(".git").exists());
    point_repo_at(&fx, &fx.checkout);
    assert_eq!(code(&fx, delete_intent(false)).await, "ok");
}

#[tokio::test]
async fn guard_refuses_protected_paths_root_home_workspace_root_and_data_dir() {
    let fx = fixture();
    let roots = fx.roots();
    let home = roots.home.clone().unwrap();
    let data = roots.data_dir.clone().unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    let work = fx.root.join("work");
    let cases: Vec<(&str, std::path::PathBuf)> = vec![
        ("the filesystem root", "/".into()),
        ("a shallow system folder", "/usr/bin".into()),
        ("the home directory", home.clone()),
        ("an ancestor of home", home.parent().unwrap().to_path_buf()),
        ("the workspace root", work.clone()),
        ("Camerata's data folder", data.clone()),
        ("a folder inside Camerata's data folder", {
            let inner = data.join("inspections");
            std::fs::create_dir_all(&inner).unwrap();
            inner
        }),
    ];
    for (what, path) in cases {
        point_repo_at(&fx, &path);
        assert_eq!(
            code(&fx, delete_intent(true)).await,
            "protected_path",
            "{what}: {}",
            path.display()
        );
    }
    assert!(fx.checkout.join(".git").exists());
    assert!(home.exists() && data.exists() && work.exists());
}

/// The Camerata repository itself (the running program lives inside it) and the repo's PARENT
/// folder (which holds other checkouts) are never a checkout to delete.
#[tokio::test]
async fn guard_refuses_the_camerata_repo_itself_and_a_repos_parent_folder() {
    let fx = fixture();
    // The program is target/debug/camerata inside a repository folder.
    let camerata_repo = fx.root.join("camerata-orchestrator");
    let exe_dir = camerata_repo.join("target").join("debug");
    std::fs::create_dir_all(&exe_dir).unwrap();
    let mut roots = fx.roots();
    roots.exe_dir = Some(exe_dir);
    let state = fx.state.clone().with_storage_roots(roots);
    state
        .settings
        .set_repo_path(REPO, Some(camerata_repo.to_string_lossy().into_owned()));
    let refusal = guard(&state, &fx.project_id, REPO, Intent::Preview)
        .await
        .err()
        .expect("refused");
    assert_eq!(refusal.code, "protected_path");

    // The parent of this repo's checkout holds the project's other repo too.
    fx.add_repo("acme/lib");
    state.settings.set_repo_path(
        REPO,
        Some(fx.root.join("work/acme").to_string_lossy().into_owned()),
    );
    let refusal = guard(&state, &fx.project_id, REPO, Intent::Preview)
        .await
        .err()
        .expect("refused");
    assert_eq!(refusal.code, "protected_path");
    assert!(
        refusal.message.contains("another repository"),
        "{}",
        refusal.message
    );
    assert!(fx.checkout.exists());
}

/// The backstop: even with NO roots configured (so nothing tells the guard where home is), a
/// test process is refused the REAL home directory. Nothing is created or removed by asking.
#[tokio::test]
async fn guard_refuses_the_real_home_even_when_no_roots_are_configured() {
    let fx = fixture();
    let Some(real_home) = dirs::home_dir() else {
        return;
    };
    let bare = fx.state.clone().with_storage_roots(Default::default());
    bare.settings
        .set_repo_path(REPO, Some(real_home.to_string_lossy().into_owned()));
    let refusal = guard(&bare, &fx.project_id, REPO, Intent::Preview)
        .await
        .err()
        .expect("the real home must be refused");
    assert!(
        matches!(refusal.code, "protected_path" | "symlinked_path"),
        "{refusal}"
    );
}

#[test]
fn a_relative_or_dotdot_path_is_protected() {
    let fx = fixture();
    for p in ["work/acme/app", "./work/acme/app", "/a/b/../c/d/e"] {
        assert!(
            super::guard::protected_reason(&fx.state, Path::new(p), Path::new(p), None).is_some(),
            "{p}"
        );
    }
}

#[tokio::test]
async fn guard_refuses_non_git_folder() {
    let fx = fixture();
    let plain = fx.root.join("work/acme/plain");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("f"), "x").unwrap();
    point_repo_at(&fx, &plain);
    assert_eq!(code(&fx, delete_intent(false)).await, "not_a_git_repo");
    assert!(plain.join("f").exists());
}

#[tokio::test]
async fn guard_refuses_folder_without_origin() {
    let fx = fixture();
    git(&fx.checkout, &["remote", "remove", "origin"]);
    assert_eq!(code(&fx, delete_intent(false)).await, "no_origin");
    assert!(fx.checkout.exists());
}

#[tokio::test]
async fn guard_refuses_non_github_origin() {
    let fx = fixture();
    for url in [
        "https://gitlab.com/acme/app.git",
        "https://notgithub.com/acme/app.git",
        "https://github.com.evil.example/acme/app.git",
    ] {
        set_origin(&fx.checkout, url);
        assert_eq!(
            code(&fx, delete_intent(false)).await,
            "origin_not_github",
            "{url}"
        );
    }
    assert!(fx.checkout.exists());
}

#[tokio::test]
async fn guard_refuses_origin_mismatch_prefix_lookalike() {
    let fx = fixture();
    // A clone of acme/app-2 sits where acme/app is expected: a prefix lookalike.
    set_origin(&fx.checkout, "https://github.com/acme/app-2.git");
    assert_eq!(code(&fx, delete_intent(false)).await, "origin_mismatch");
    // And the other direction.
    set_origin(&fx.checkout, "https://github.com/acme/ap.git");
    assert_eq!(code(&fx, delete_intent(false)).await, "origin_mismatch");
    assert!(fx.checkout.exists());
}

#[tokio::test]
async fn guard_refuses_origin_mismatch_different_owner() {
    let fx = fixture();
    set_origin(&fx.checkout, "https://github.com/evil/app.git");
    let refusal = guard(&fx.state, &fx.project_id, REPO, delete_intent(false))
        .await
        .err()
        .expect("refused");
    assert_eq!(refusal.code, "origin_mismatch");
    assert!(refusal.message.contains("evil/app"), "{}", refusal.message);
    assert!(fx.checkout.exists());
}

#[tokio::test]
async fn guard_refuses_confirmation_mismatch_including_case_and_whitespace() {
    let fx = fixture();
    for typed in [
        "ACME/APP",
        "acme/app ",
        " acme/app",
        "acme/app\n",
        "acme/",
        "",
    ] {
        let got = code(
            &fx,
            Intent::Delete {
                confirm: typed,
                include_uncommitted: false,
            },
        )
        .await;
        assert_eq!(got, "confirmation_mismatch", "typed {typed:?}");
    }
    assert!(fx.checkout.exists());
}

#[tokio::test]
async fn guard_refuses_dirty_tree_without_opt_in() {
    let fx = fixture();
    write_file(&fx.checkout, "src/main.rs", "fn main() { /* edited */ }\n");
    let refusal = guard(&fx.state, &fx.project_id, REPO, delete_intent(false))
        .await
        .err()
        .expect("refused");
    assert_eq!(refusal.code, "dirty_tree");
    assert!(refusal.message.contains("Delete uncommitted work too"));
    assert!(fx.checkout.exists());
}

#[tokio::test]
async fn guard_refuses_a_stash_as_uncommitted_work() {
    let fx = fixture();
    write_file(&fx.checkout, "src/main.rs", "fn main() { /* wip */ }\n");
    git(&fx.checkout, &["stash", "push", "-q"]);
    assert_eq!(code(&fx, delete_intent(false)).await, "dirty_tree");
}

#[tokio::test]
async fn guard_refuses_unpushed_commits_without_opt_in() {
    let fx = fixture();
    write_file(&fx.checkout, "new.rs", "// remediation\n");
    commit_all(&fx.checkout, "remediation not pushed");
    let refusal = guard(&fx.state, &fx.project_id, REPO, delete_intent(false))
        .await
        .err()
        .expect("refused");
    assert_eq!(refusal.code, "unpushed_commits");
    assert!(refusal.message.contains("main: 1"), "{}", refusal.message);
    assert!(fx.checkout.exists());
}

#[tokio::test]
async fn guard_refuses_branch_with_no_upstream_as_unpushed() {
    let fx = fixture();
    git(&fx.checkout, &["checkout", "-q", "-b", "camerata/work"]);
    write_file(&fx.checkout, "work.rs", "// governed dev\n");
    commit_all(&fx.checkout, "work on a branch GitHub has never seen");
    git(&fx.checkout, &["checkout", "-q", "main"]);
    let refusal = guard(&fx.state, &fx.project_id, REPO, delete_intent(false))
        .await
        .err()
        .expect("refused");
    assert_eq!(refusal.code, "unpushed_commits");
    assert!(
        refusal.message.contains("camerata/work"),
        "{}",
        refusal.message
    );
}

#[tokio::test]
async fn guard_accepts_dirty_and_unpushed_with_opt_in() {
    let fx = fixture();
    write_file(&fx.checkout, "src/main.rs", "fn main() { /* edited */ }\n");
    write_file(&fx.checkout, "new.rs", "// remediation\n");
    git(&fx.checkout, &["add", "new.rs"]);
    git(&fx.checkout, &["commit", "-q", "-m", "unpushed"]);
    write_file(
        &fx.checkout,
        "src/main.rs",
        "fn main() { /* edited again */ }\n",
    );
    git(&fx.checkout, &["stash", "push", "-q"]);
    write_file(&fx.checkout, "src/main.rs", "fn main() { /* dirty */ }\n");

    let target = guard(&fx.state, &fx.project_id, REPO, delete_intent(true))
        .await
        .expect("accepted with the tick");
    assert!(target.dirty_files >= 1);
    assert_eq!(target.stashes, 1);
    assert_eq!(target.unpushed.len(), 1);
}

#[tokio::test]
async fn guard_refuses_while_scan_job_is_running() {
    let fx = fixture();
    let job = fx.state.jobs.create("audit", Some(fx.project_id.clone()));
    assert_eq!(code(&fx, delete_intent(false)).await, "scan_in_progress");
    // Finished: allowed again.
    fx.state
        .jobs
        .finish(&job, crate::onboard::ScanReport::gated(&[REPO.to_string()]));
    assert_eq!(code(&fx, delete_intent(false)).await, "ok");
    // A scan running inside a request counts too.
    let scan = fx.state.source_ops.begin_scan(&fx.project_id);
    assert_eq!(code(&fx, delete_intent(false)).await, "scan_in_progress");
    drop(scan);
    // And so does another exclusive operation on the same repo (a re-fetch, another delete).
    let busy = fx.state.source_ops.try_begin(&fx.project_id, REPO).unwrap();
    assert_eq!(code(&fx, delete_intent(false)).await, "scan_in_progress");
    drop(busy);
    assert_eq!(code(&fx, delete_intent(false)).await, "ok");
    assert!(fx.checkout.exists());
}

/// A governed, investigation, update-branch or PR-resolve run that has not finished is working
/// in the repo's worktrees; deleting the checkout under it would strand it (and its writes would
/// recreate the folder). Refused the same way a running scan is.
#[tokio::test]
async fn guard_refuses_while_a_governed_run_is_active_on_the_repo() {
    let fx = fixture();
    let run = fx
        .state
        .runs
        .create("acme/app#7", "live", crate::run::RunKind::Watched);

    for intent in [delete_intent(false), Intent::Preview] {
        let refusal = guard(&fx.state, &fx.project_id, REPO, intent)
            .await
            .err()
            .expect("an active run blocks deletion");
        assert_eq!(refusal.code, "governed_run_in_progress");
        assert_eq!(
            refusal.message,
            "A governed run is in progress on acme/app; wait for it to finish or stop it first."
        );
    }
    assert!(fx.checkout.exists());

    // Finished (here: cancelled) it no longer blocks.
    fx.state.runs.cancel(&run);
    assert_eq!(code(&fx, delete_intent(false)).await, "ok");
}

#[tokio::test]
async fn a_paused_run_still_counts_and_a_marked_done_run_does_not() {
    let fx = fixture();
    let run = fx
        .state
        .runs
        .create("acme/app#8", "live", crate::run::RunKind::Watched);
    fx.state.runs.set_status(
        &run,
        camerata_app_core::run::RunStatus::AwaitingClarification,
        false,
    );
    assert_eq!(
        code(&fx, delete_intent(false)).await,
        "governed_run_in_progress",
        "a run paused on a question is still open"
    );
    fx.state.runs.mark_done(&run);
    assert_eq!(code(&fx, delete_intent(false)).await, "ok");
}

/// A run is attributed to a repo by its story id (`owner/repo#n`) and by the repos a UoW put in
/// scope at intake; a run on another repo, or on a story that resolves to no repo, is no reason
/// to keep this one.
#[tokio::test]
async fn a_run_blocks_only_the_repos_it_is_working_on() {
    let fx = fixture();
    // Another repo's story id.
    fx.state
        .runs
        .create("acme/lib#1", "live", crate::run::RunKind::Watched);
    // A story that names no repo at all.
    fx.state
        .runs
        .create("CAM-99", "live", crate::run::RunKind::Watched);
    assert_eq!(code(&fx, delete_intent(false)).await, "ok");

    // A multi-repo UoW whose story id names a different repo but whose intake scope includes ours.
    fx.state
        .uow
        .set_intake_repos(
            "acme/other#3",
            vec![crate::uow::RepoScope {
                repo: REPO.to_string(),
                branch: crate::uow::BranchMode::NewFromBase {
                    base: "main".into(),
                    new_name: String::new(),
                },
            }],
        )
        .unwrap();
    fx.state
        .runs
        .create("acme/other#3", "live", crate::run::RunKind::Watched);
    assert_eq!(
        code(&fx, delete_intent(false)).await,
        "governed_run_in_progress"
    );
}

#[tokio::test]
async fn guard_refuses_worktree_in_protected_location() {
    let fx = fixture();
    // A hand-made worktree inside Camerata's own data folder.
    let evil = fx.root.join("data").join("evil-wt");
    git(
        &fx.checkout,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "evil",
            &evil.to_string_lossy(),
        ],
    );
    assert_eq!(
        code(&fx, delete_intent(true)).await,
        "worktree_outside_refused"
    );
    assert!(evil.exists() && fx.checkout.exists());
}

/// H2: the path override aimed at a LINKED worktree of the client's clone. Its `.git` is a
/// file, the origin matches, and the purge used to skip the wrong worktree-list entry, so the
/// main clone (with all the client code) survived while the status flipped to deleted.
#[tokio::test]
async fn guard_refuses_a_linked_worktree_as_the_target() {
    let fx = fixture();
    let linked = fx.root.join("elsewhere").join("linked");
    git(
        &fx.checkout,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            &linked.to_string_lossy(),
        ],
    );
    assert!(
        linked.join(".git").is_file(),
        "a linked worktree has a .git FILE"
    );
    point_repo_at(&fx, &linked);

    for intent in [delete_intent(true), Intent::Preview] {
        let refusal = guard(&fx.state, &fx.project_id, REPO, intent)
            .await
            .err()
            .expect("a linked worktree is never the target");
        assert_eq!(refusal.code, "linked_worktree");
        assert!(
            refusal.message.contains("is a worktree of the clone at")
                && refusal.message.contains(
                    &fx.checkout
                        .canonicalize()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                )
                && refusal.message.contains("delete that clone instead"),
            "{}",
            refusal.message
        );
    }
    assert!(linked.exists() && fx.checkout.exists());
}

/// H3: the registered worktree's folder was reused by a worktree of a DIFFERENT repository.
/// Git still lists the registration (the `.git` file exists), but it is not this clone's
/// worktree and must never be removed with it.
#[tokio::test]
async fn guard_refuses_when_a_registered_folder_now_belongs_to_another_repository() {
    let fx = fixture();
    let wt = fx.root.join("scratch").join("wt");
    git(
        &fx.checkout,
        &["worktree", "add", "-q", "-b", "side", &wt.to_string_lossy()],
    );
    std::fs::write(
        wt.join(".git"),
        format!(
            "gitdir: {}/someone-else/.git/worktrees/wt\n",
            fx.root.display()
        ),
    )
    .unwrap();
    write_file(&wt, "precious.txt", "not this clone's\n");

    assert_eq!(
        code(&fx, delete_intent(true)).await,
        "worktree_outside_refused"
    );
    assert!(wt.join("precious.txt").exists() && fx.checkout.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn a_worktree_behind_a_symlink_is_refused_by_the_worktree_check() {
    let fx = fixture();
    let real = fx.root.join("elsewhere");
    std::fs::create_dir_all(real.join("wt")).unwrap();
    let via = fx.root.join("via-elsewhere");
    std::os::unix::fs::symlink(&real, &via).unwrap();

    // The check itself, on a path that goes through a link...
    let reason = super::guard::worktree_refusal(&fx.state, &via.join("wt"))
        .expect("a worktree path behind a symbolic link is refused");
    assert!(reason.contains("symbolic link"), "{reason}");
    // ...and on one that does not.
    assert_eq!(
        super::guard::worktree_refusal(&fx.state, &real.join("wt")),
        None
    );

    // Git itself registers worktrees by their real path, even when added through a link, so a
    // registered worktree never trips the check by accident.
    git(
        &fx.checkout,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            &via.join("wt3").to_string_lossy(),
        ],
    );
    assert_eq!(code(&fx, delete_intent(true)).await, "ok");
}

#[tokio::test]
async fn guard_runs_checks_in_documented_order_and_reports_the_first() {
    let fx = fixture();
    // A scenario that fails EVERYTHING it can at once.
    // R4: reached through a symlink; R6..: not git; plus a scan running and a wrong confirmation.
    let plain = fx.root.join("work/acme/plain");
    std::fs::create_dir_all(&plain).unwrap();
    let link = fx.root.join("work/acme/plain-link");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&plain, &link).unwrap();
    let _scan = fx.state.source_ops.begin_scan(&fx.project_id);
    let wrong = Intent::Delete {
        confirm: "wrong",
        include_uncommitted: false,
    };

    #[cfg(unix)]
    {
        point_repo_at(&fx, &link);
        assert_eq!(code(&fx, wrong).await, "symlinked_path", "R4 first");
    }
    point_repo_at(&fx, &plain);
    assert_eq!(code(&fx, wrong).await, "not_a_git_repo", "then R6");

    git(&plain, &["init", "-q", "-b", "main"]);
    assert_eq!(code(&fx, wrong).await, "no_origin", "then R7");
    git(
        &plain,
        &["remote", "add", "origin", "https://gitlab.com/acme/app.git"],
    );
    assert_eq!(code(&fx, wrong).await, "origin_not_github");
    set_origin(&plain, "https://github.com/acme/other.git");
    assert_eq!(code(&fx, wrong).await, "origin_mismatch", "then R8");
    set_origin(&plain, "https://github.com/acme/app.git");
    assert_eq!(code(&fx, wrong).await, "confirmation_mismatch", "then R9");

    // Right confirmation, but dirty AND unpushed AND a scan running: dirty (R10) comes first.
    write_file(&plain, "a.txt", "dirty\n");
    commit_all(&plain, "unpushed one");
    write_file(&plain, "b.txt", "dirty\n");
    assert_eq!(
        code(&fx, delete_intent(false)).await,
        "dirty_tree",
        "then R10"
    );
    git(&plain, &["add", "-A"]);
    git(&plain, &["commit", "-q", "-m", "unpushed two"]);
    assert_eq!(
        code(&fx, delete_intent(false)).await,
        "unpushed_commits",
        "then R11"
    );
    assert_eq!(
        code(&fx, delete_intent(true)).await,
        "scan_in_progress",
        "then R12"
    );
}
