//! Read-only git probes over a checkout, shared by the status view, the deletion guard, the
//! re-fetch handler and the delta rescan.
//!
//! Every function runs the system `git` through [`crate::workspace::git`] (kill-on-drop, one
//! process, no shell) and never writes to the checkout (the one exception is
//! [`linked_worktrees`], which first prunes stale worktree registrations: administrative records
//! of folders that no longer exist, never a working tree). Anything that cannot be determined
//! comes back as `None`/`false`/empty rather than an error: callers decide what "unknown" means
//! for them (the deletion guard treats an unreadable branch list as a refusal, the status view as
//! a missing field).

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::onboard::finding_context::canonicalize_existing_prefix;
use crate::workspace::{git, stderr_of};

/// Untracked paths Camerata itself creates inside a checkout. They are scratch, not work: a
/// checkout that ran governed development has `.camerata-worktrees/` and
/// `.camerata-shared-target/` showing as untracked, and counting them would make every such
/// checkout read as "has uncommitted changes".
const CAMERATA_SCRATCH: [&str; 5] = [
    ".camerata-worktrees/",
    ".camerata-shared-target/",
    ".ruff_cache/",
    ".camerata-scan-cache.json",
    ".camerata-scan-ledger.json",
];

/// A local branch with commits that are not on any remote.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Unpushed {
    pub branch: String,
    pub ahead: u64,
}

/// Whether `s` is a full 40-character lowercase-or-uppercase hex object name. Every sha that
/// reaches a git command line from outside this crate passes this first.
pub fn is_full_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// stdout of a successful `git <args>` in `dir`, trimmed; `None` when git fails or prints nothing.
pub async fn read(dir: &Path, args: &[&str]) -> Option<String> {
    let out = git(Some(dir), args).await.ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// `git rev-parse HEAD`.
pub async fn head_sha(dir: &Path) -> Option<String> {
    read(dir, &["rev-parse", "HEAD"]).await
}

/// The checked-out branch; `None` on a detached HEAD.
pub async fn head_branch(dir: &Path) -> Option<String> {
    read(dir, &["symbolic-ref", "--short", "-q", "HEAD"]).await
}

/// Whether `sha` names a commit that exists in this clone.
pub async fn commit_exists(dir: &Path, sha: &str) -> bool {
    if !is_full_sha(sha) {
        return false;
    }
    let spec = format!("{sha}^{{commit}}");
    git(Some(dir), &["cat-file", "-e", &spec])
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `git rev-list --count <from>..<to>`; `None` when either end is not a commit here.
pub async fn commits_between(dir: &Path, from: &str, to: &str) -> Option<u64> {
    if !is_full_sha(from) || !(to == "HEAD" || is_full_sha(to)) {
        return None;
    }
    let range = format!("{from}..{to}");
    read(dir, &["rev-list", "--count", &range])
        .await
        .and_then(|s| s.parse().ok())
}

/// How many entries `git status --porcelain` shows, not counting Camerata's own scratch paths.
pub async fn dirty_file_count(dir: &Path) -> u64 {
    let Ok(out) = git(Some(dir), &["status", "--porcelain"]).await else {
        return 0;
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| {
            // `XY path`, or `XY old -> new` for a rename.
            let path = line.get(3..).unwrap_or("").trim();
            !CAMERATA_SCRATCH.iter().any(|s| path.starts_with(s))
        })
        .count() as u64
}

/// Number of `git stash` entries. A stash is uncommitted work that lives only in this clone.
pub async fn stash_count(dir: &Path) -> u64 {
    read(dir, &["stash", "list"])
        .await
        .map(|s| s.lines().count() as u64)
        .unwrap_or(0)
}

/// Local branches (and a detached HEAD) holding commits that exist on no remote-tracking ref.
///
/// For a branch with an `origin/<branch>` counterpart the count is `origin/<b>..<b>`; for a
/// branch without one it is every commit reachable from the branch and from no remote ref at all
/// (a local branch that merely points at a commit GitHub already has holds nothing to lose). The
/// remote-tracking refs are whatever the last fetch left, so a stale view can only OVER-report.
pub async fn unpushed(dir: &Path) -> Vec<Unpushed> {
    let mut found = Vec::new();
    let branches = read(
        dir,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )
    .await
    .unwrap_or_default();
    for branch in branches.lines().map(str::trim).filter(|b| !b.is_empty()) {
        let local = format!("refs/heads/{branch}");
        let remote = format!("refs/remotes/origin/{branch}");
        let has_remote = git(Some(dir), &["rev-parse", "--verify", "--quiet", &remote])
            .await
            .map(|o| o.status.success())
            .unwrap_or(false);
        let count = if has_remote {
            let range = format!("{remote}..{local}");
            read(dir, &["rev-list", "--count", &range]).await
        } else {
            read(dir, &["rev-list", "--count", &local, "--not", "--remotes"]).await
        }
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
        if count > 0 {
            found.push(Unpushed {
                branch: branch.to_string(),
                ahead: count,
            });
        }
    }
    // Commits made on a detached HEAD belong to no branch at all.
    if head_branch(dir).await.is_none() {
        let ahead = read(
            dir,
            &[
                "rev-list",
                "--count",
                "HEAD",
                "--not",
                "--remotes",
                "--branches",
            ],
        )
        .await
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
        if ahead > 0 {
            found.push(Unpushed {
                branch: "HEAD (detached)".to_string(),
                ahead,
            });
        }
    }
    found
}

/// One entry of `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListedWorktree {
    pub path: PathBuf,
    /// Git annotated the entry `prunable`: the registration points at a folder that is no
    /// longer that worktree.
    pub prunable: bool,
}

/// Parse `git worktree list --porcelain`: blocks separated by a blank line, each starting with
/// `worktree <path>` and optionally carrying a `prunable <reason>` line. Pure.
pub(crate) fn parse_worktree_list(porcelain: &str) -> Vec<ListedWorktree> {
    let mut out: Vec<ListedWorktree> = Vec::new();
    for line in porcelain.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            out.push(ListedWorktree {
                path: PathBuf::from(path),
                prunable: false,
            });
        } else if line == "prunable" || line.starts_with("prunable ") {
            if let Some(last) = out.last_mut() {
                last.prunable = true;
            }
        }
    }
    out
}

/// The canonical path of the git directory shared by every worktree of the clone `dir` belongs
/// to (`git rev-parse --git-common-dir`): `<clone>/.git` for the main checkout AND for each of
/// its linked worktrees. `None` when `dir` is not a repository.
pub async fn common_dir(dir: &Path) -> Option<PathBuf> {
    let raw = read(dir, &["rev-parse", "--git-common-dir"]).await?;
    let path = PathBuf::from(raw);
    let absolute = if path.is_absolute() {
        path
    } else {
        dir.join(path)
    };
    Some(canonicalize_existing_prefix(&absolute))
}

/// Registered worktree paths of the checkout at `dir`, EXCLUDING `dir` itself. `git worktree
/// list --porcelain` is authoritative: Camerata's own live under
/// `<checkout>/.camerata-worktrees/<branch>`, but a hand-made one may be anywhere.
///
/// Three safeguards keep a worktree that is not really one out of the answer, because every path
/// returned is later deleted:
///
/// - stale registrations are pruned first (this removes only administrative records of
///   worktrees whose folder is gone; no working tree is touched), and any entry git still
///   flags `prunable` is dropped;
/// - the asked folder is excluded by its canonical path, not by position (git lists the MAIN
///   worktree first, which is not `dir` when `dir` is itself a linked worktree);
/// - a listed folder that exists must carry a `.git` file pointing into THIS clone's
///   `worktrees/` area, else the whole answer is an error: a folder reused for something else
///   is not ours to remove.
pub async fn linked_worktrees(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let _ = git(Some(dir), &["worktree", "prune"]).await;
    let out = git(Some(dir), &["worktree", "list", "--porcelain"])
        .await
        .map_err(|e| format!("couldn't run git: {e}"))?;
    if !out.status.success() {
        return Err(stderr_of(&out));
    }
    let Some(common) = common_dir(dir).await else {
        return Err("couldn't find the clone's git directory".to_string());
    };
    let me = canonicalize_existing_prefix(dir);
    let mut found = Vec::new();
    for listed in parse_worktree_list(&String::from_utf8_lossy(&out.stdout)) {
        if listed.prunable || canonicalize_existing_prefix(&listed.path) == me {
            continue;
        }
        // Nothing on disk, nothing to remove (a locked registration for a missing folder).
        if std::fs::symlink_metadata(&listed.path).is_err() {
            continue;
        }
        verify_belongs_to(&listed.path, &common)?;
        found.push(listed.path);
    }
    Ok(found)
}

/// `Ok` when the existing folder `wt` is a worktree of the clone whose shared git directory is
/// `common`: its `.git` is a file whose `gitdir:` line points under `<common>/worktrees/`, or
/// (the main checkout, listed from a linked worktree) its `.git` directory IS `common`.
fn verify_belongs_to(wt: &Path, common: &Path) -> Result<(), String> {
    let not_ours = |detail: &str| {
        format!(
            "{} is not a worktree of this clone ({detail})",
            wt.display()
        )
    };
    let dot_git = wt.join(".git");
    let Ok(meta) = std::fs::symlink_metadata(&dot_git) else {
        return Err(not_ours("it has no .git"));
    };
    if meta.is_dir() {
        return (canonicalize_existing_prefix(&dot_git) == common)
            .then_some(())
            .ok_or_else(|| not_ours("its .git is another repository's"));
    }
    let text = std::fs::read_to_string(&dot_git).map_err(|e| not_ours(&e.to_string()))?;
    let Some(target) = text
        .lines()
        .find_map(|l| l.strip_prefix("gitdir:"))
        .map(str::trim)
    else {
        return Err(not_ours("its .git file has no gitdir line"));
    };
    let target = Path::new(target);
    let target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        wt.join(target)
    };
    if canonicalize_existing_prefix(&target).starts_with(common.join("worktrees")) {
        Ok(())
    } else {
        Err(not_ours(&format!(
            "its .git file points to {}",
            target.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_git::{bare_remote, clone_as, commit_all, git as sync_git, write_file};
    use tempfile::TempDir;

    /// A clone of a local bare remote, so `origin/main` is a real remote-tracking ref.
    fn fixture() -> (TempDir, PathBuf, String) {
        let tmp = TempDir::new().unwrap();
        let (_bare, url, sha) = bare_remote(tmp.path(), &[("a.txt", "one\n")]);
        let clone = tmp.path().join("clone");
        clone_as(&url, &clone, "https://github.com/acme/app.git");
        (tmp, clone, sha)
    }

    #[test]
    fn full_sha_validation_is_strict() {
        assert!(is_full_sha(&"a".repeat(40)));
        assert!(is_full_sha(&"A1".repeat(20)));
        assert!(!is_full_sha(&"a".repeat(39)));
        assert!(!is_full_sha(&"a".repeat(41)));
        assert!(!is_full_sha(&format!("{}-", "a".repeat(39))));
        assert!(!is_full_sha("--upload-pack=evil"));
    }

    #[tokio::test]
    async fn a_clean_clone_has_nothing_dirty_nothing_stashed_nothing_unpushed() {
        let (_tmp, clone, sha) = fixture();
        assert_eq!(head_sha(&clone).await.as_deref(), Some(sha.as_str()));
        assert_eq!(head_branch(&clone).await.as_deref(), Some("main"));
        assert_eq!(dirty_file_count(&clone).await, 0);
        assert_eq!(stash_count(&clone).await, 0);
        assert!(unpushed(&clone).await.is_empty());
    }

    #[tokio::test]
    async fn dirty_count_sees_edits_and_untracked_files_but_not_camerata_scratch() {
        let (_tmp, clone, _) = fixture();
        write_file(&clone, "a.txt", "changed\n");
        write_file(&clone, "new.txt", "new\n");
        write_file(&clone, ".camerata-worktrees/x/file", "scratch\n");
        write_file(&clone, ".camerata-shared-target/debug/x", "scratch\n");
        write_file(&clone, ".ruff_cache/CACHEDIR.TAG", "scratch\n");
        assert_eq!(dirty_file_count(&clone).await, 2);
    }

    #[tokio::test]
    async fn stashed_work_is_counted() {
        let (_tmp, clone, _) = fixture();
        write_file(&clone, "a.txt", "work in progress\n");
        sync_git(&clone, &["stash", "push", "-q"]);
        assert_eq!(dirty_file_count(&clone).await, 0);
        assert_eq!(stash_count(&clone).await, 1);
    }

    #[tokio::test]
    async fn local_commits_not_on_origin_are_unpushed_with_their_count() {
        let (_tmp, clone, _) = fixture();
        write_file(&clone, "b.txt", "b\n");
        commit_all(&clone, "local one");
        write_file(&clone, "c.txt", "c\n");
        commit_all(&clone, "local two");
        assert_eq!(
            unpushed(&clone).await,
            vec![Unpushed {
                branch: "main".to_string(),
                ahead: 2
            }]
        );
    }

    #[tokio::test]
    async fn a_branch_without_an_upstream_is_unpushed_only_when_it_holds_unique_commits() {
        let (_tmp, clone, _) = fixture();
        // A new branch at the same commit as origin/main holds nothing to lose.
        sync_git(&clone, &["branch", "idle"]);
        assert!(unpushed(&clone).await.is_empty());
        // Give a branch its own commit and it is unpushed work.
        sync_git(&clone, &["checkout", "-q", "-b", "wip"]);
        write_file(&clone, "w.txt", "w\n");
        commit_all(&clone, "wip");
        sync_git(&clone, &["checkout", "-q", "main"]);
        assert_eq!(
            unpushed(&clone).await,
            vec![Unpushed {
                branch: "wip".to_string(),
                ahead: 1
            }]
        );
    }

    #[tokio::test]
    async fn a_commit_made_on_a_detached_head_counts_as_unpushed() {
        let (_tmp, clone, sha) = fixture();
        sync_git(&clone, &["checkout", "-q", "--detach", &sha]);
        write_file(&clone, "d.txt", "d\n");
        commit_all(&clone, "detached work");
        let found = unpushed(&clone).await;
        assert_eq!(
            found,
            vec![Unpushed {
                branch: "HEAD (detached)".to_string(),
                ahead: 1
            }]
        );
        assert_eq!(head_branch(&clone).await, None);
    }

    #[tokio::test]
    async fn commit_existence_and_distance() {
        let (tmp, clone, first) = fixture();
        assert!(commit_exists(&clone, &first).await);
        assert!(!commit_exists(&clone, &"0".repeat(40)).await);
        assert!(!commit_exists(&clone, "not-a-sha").await);
        write_file(&clone, "b.txt", "b\n");
        let second = commit_all(&clone, "second");
        assert_eq!(commits_between(&clone, &first, "HEAD").await, Some(1));
        assert_eq!(commits_between(&clone, &first, &second).await, Some(1));
        assert_eq!(commits_between(&clone, &second, "HEAD").await, Some(0));
        assert_eq!(commits_between(&clone, &"0".repeat(40), "HEAD").await, None);
        assert_eq!(commits_between(&clone, "HEAD", "HEAD").await, None);
        drop(tmp);
    }

    #[tokio::test]
    async fn linked_worktrees_excludes_the_checkout_and_lists_the_others() {
        let (tmp, clone, _) = fixture();
        assert!(linked_worktrees(&clone).await.unwrap().is_empty());
        let wt = tmp.path().join("elsewhere-wt");
        sync_git(
            &clone,
            &["worktree", "add", "-q", "-b", "side", &wt.to_string_lossy()],
        );
        let listed = linked_worktrees(&clone).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].canonicalize().unwrap(),
            wt.canonicalize().unwrap()
        );
    }

    /// Git lists the MAIN worktree first, whichever worktree it is asked from. Excluding "the
    /// first entry" therefore excluded the wrong folder when asked from a linked worktree, and
    /// listed the folder itself as one of its own worktrees.
    #[tokio::test]
    async fn linked_worktrees_excludes_the_asked_folder_even_when_git_lists_the_main_first() {
        let (tmp, clone, _) = fixture();
        let wt = tmp.path().join("elsewhere-wt");
        sync_git(
            &clone,
            &["worktree", "add", "-q", "-b", "side", &wt.to_string_lossy()],
        );

        let from_linked = linked_worktrees(&wt).await.unwrap();

        assert_eq!(from_linked.len(), 1, "{from_linked:?}");
        assert_eq!(
            from_linked[0].canonicalize().unwrap(),
            clone.canonicalize().unwrap(),
            "the other worktree is the main checkout, not the folder asked about"
        );
    }

    /// A worktree whose folder was removed by hand stays registered until pruned. Listing prunes
    /// first, so a registration for a vanished folder never appears (and its path, reused later
    /// for something else, is never mistaken for a worktree).
    #[tokio::test]
    async fn a_stale_registration_is_pruned_before_listing() {
        let (tmp, clone, _) = fixture();
        let wt = tmp.path().join("gone-wt");
        sync_git(
            &clone,
            &["worktree", "add", "-q", "-b", "side", &wt.to_string_lossy()],
        );
        std::fs::remove_dir_all(&wt).unwrap();
        std::fs::create_dir_all(wt.join("precious")).unwrap();
        assert!(
            sync_git(&clone, &["worktree", "list", "--porcelain"]).contains("prunable"),
            "git itself still lists the stale registration"
        );

        assert!(linked_worktrees(&clone).await.unwrap().is_empty());
        assert!(
            !sync_git(&clone, &["worktree", "list", "--porcelain"]).contains("gone-wt"),
            "the registration is gone"
        );
        assert!(
            wt.join("precious").exists(),
            "the reused folder is untouched"
        );
    }

    #[test]
    fn the_listing_parser_reads_paths_and_the_prunable_flag() {
        let porcelain = "worktree /x/main\nHEAD aaa\nbranch refs/heads/main\n\n\
                         worktree /x/live\nHEAD bbb\nbranch refs/heads/live\n\n\
                         worktree /x/stale\nHEAD ccc\ndetached\nprunable gitdir file points to non-existent location\n";
        let listed = parse_worktree_list(porcelain);
        assert_eq!(
            listed,
            vec![
                ListedWorktree {
                    path: PathBuf::from("/x/main"),
                    prunable: false
                },
                ListedWorktree {
                    path: PathBuf::from("/x/live"),
                    prunable: false
                },
                ListedWorktree {
                    path: PathBuf::from("/x/stale"),
                    prunable: true
                },
            ]
        );
    }

    /// A registration prune left behind (a locked one, say) that git still flags `prunable`
    /// is never returned either.
    #[test]
    fn prunable_entries_are_dropped_from_the_returned_list() {
        let porcelain =
            "worktree /x/main\nHEAD aaa\n\nworktree /x/stale\nHEAD ccc\nprunable gone\n";
        let kept: Vec<PathBuf> = parse_worktree_list(porcelain)
            .into_iter()
            .filter(|w| !w.prunable)
            .map(|w| w.path)
            .collect();
        assert_eq!(kept, vec![PathBuf::from("/x/main")]);
    }

    /// A folder that exists at a registered path but whose `.git` file points into SOME OTHER
    /// repository's admin area is not this clone's worktree; asking to delete it is an error.
    #[tokio::test]
    async fn a_listed_folder_whose_git_file_points_elsewhere_is_an_error() {
        let (tmp, clone, _) = fixture();
        let wt = tmp.path().join("reused-wt");
        sync_git(
            &clone,
            &["worktree", "add", "-q", "-b", "side", &wt.to_string_lossy()],
        );
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}/other/.git/worktrees/x\n", tmp.path().display()),
        )
        .unwrap();

        let err = linked_worktrees(&clone).await.unwrap_err();

        assert!(err.contains("not a worktree of this clone"), "{err}");
        assert!(wt.exists());
    }

    #[tokio::test]
    async fn the_common_dir_of_a_worktree_is_the_main_clones_git_dir() {
        let (tmp, clone, _) = fixture();
        let wt = tmp.path().join("elsewhere-wt");
        sync_git(
            &clone,
            &["worktree", "add", "-q", "-b", "side", &wt.to_string_lossy()],
        );
        let expected = clone.join(".git").canonicalize().unwrap();
        assert_eq!(common_dir(&clone).await.unwrap(), expected);
        assert_eq!(common_dir(&wt).await.unwrap(), expected);
    }
}
