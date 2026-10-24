//! Read-only git probes over a checkout, shared by the status view, the deletion guard, the
//! re-fetch handler and the delta rescan.
//!
//! Every function runs the system `git` through [`crate::workspace::git`] (kill-on-drop, one
//! process, no shell) and never writes to the checkout. Anything that cannot be determined
//! comes back as `None`/`false`/empty rather than an error: callers decide what "unknown" means
//! for them (the deletion guard treats an unreadable branch list as a refusal, the status view as
//! a missing field).

use std::path::{Path, PathBuf};

use serde::Serialize;

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

/// Registered worktree paths of the checkout at `dir`, EXCLUDING the checkout itself (git lists
/// it first). `git worktree list --porcelain` is authoritative: Camerata's own live under
/// `<checkout>/.camerata-worktrees/<branch>`, but a hand-made one may be anywhere.
pub async fn linked_worktrees(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let out = git(Some(dir), &["worktree", "list", "--porcelain"])
        .await
        .map_err(|e| format!("couldn't run git: {e}"))?;
    if !out.status.success() {
        return Err(stderr_of(&out));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .skip(1)
        .map(PathBuf::from)
        .collect())
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
}
