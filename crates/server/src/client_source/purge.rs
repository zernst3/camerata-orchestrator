//! The purge: every on-disk or in-memory copy of a repository's code that Camerata made, listed
//! first (the plan, shown to the operator) and then removed (the execution, recorded as proof).
//!
//! Order matters (design, section 5): worktrees are removed BEFORE the checkout (their admin
//! records live inside `<checkout>/.git/worktrees/`), the checkout next, then the copies outside
//! it, and the record is written last by the caller.
//!
//! | Location | Found by | Removed by |
//! |---|---|---|
//! | registered git worktrees | `git worktree list --porcelain` | `git worktree remove --force`, then the directory, then `git worktree prune` |
//! | the checkout | the resolved folder, canonicalised | `remove_dir_all`, then an emptied `<owner>` parent under the workspace root |
//! | server incremental-scan cache | `ScanCacheStore` | `remove_repo` (this repo's slice only) |
//! | CLI scan cache and ledger | `repo_state_key(checkout)` in the CLI state dir | `remove_file` |
//! | Claude Code session files | the ids recorded on the project's inspection entries | `remove_file` / `remove_dir_all` on `<claude dir>/*/<id>.jsonl` and `<id>/` |
//! | in-memory copies | scan transcripts, finished jobs of the project | cleared |
//!
//! Every removal first passes [`ensure_not_real_location_in_test`], and every path is derived
//! from the injected [`StorageRoots`], so no test can reach a real location.

use std::path::{Path, PathBuf};

use super::guard::GuardedTarget;
use super::records::{PurgeOutcome, PurgedItem, PurgedKind};
use super::storage::ensure_not_real_location_in_test;
use crate::scan_cache::state_dir::repo_state_files_in;
use crate::workspace;
use crate::AppState;

/// What one step does when executed.
#[derive(Debug, Clone)]
enum Action {
    /// A registered worktree directory.
    Worktree(PathBuf),
    /// The checkout (and, once it is gone, its `<owner>` folder if that is now empty).
    Checkout(PathBuf),
    /// This repo's slice of the server's incremental-scan cache.
    ServerScanCache,
    /// A single file (a CLI cache or ledger).
    File(PathBuf),
    /// A Claude Code session file.
    Session(PathBuf),
    /// Claude Code's sidecar directory for a session.
    SessionDir(PathBuf),
    /// A session id that was recorded but whose file is not there.
    SessionAbsent,
    /// The scan transcripts held in memory.
    Transcripts,
    /// The finished scan jobs of the project held in memory.
    Jobs,
}

/// One planned removal.
#[derive(Debug, Clone)]
pub struct PurgeStep {
    pub item: PurgedItem,
    action: Action,
}

/// Everything a deletion will remove, in order.
#[derive(Debug, Clone)]
pub struct PurgePlan {
    pub steps: Vec<PurgeStep>,
}

impl PurgePlan {
    /// The items as the preview and the record show them.
    pub fn items(&self) -> Vec<PurgedItem> {
        self.steps.iter().map(|s| s.item.clone()).collect()
    }
}

/// A Claude Code session id as a path component: exactly a 36-character UUID. Anything else
/// (a corrupted record, `../..`) never reaches a path.
pub(crate) fn is_session_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
        && id.matches('-').count() == 4
}

/// `(files, bytes)` under `path` (a file counts as one), not following symbolic links, skipping
/// anything under one of `skip`. Blocking.
pub fn walk_totals(path: &Path, skip: &[PathBuf]) -> (u64, u64) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return (0, 0);
    };
    if !meta.is_dir() {
        return (1, meta.len());
    }
    let Ok(read) = std::fs::read_dir(path) else {
        return (0, 0);
    };
    let mut totals = (0u64, 0u64);
    for entry in read.filter_map(Result::ok) {
        let p = entry.path();
        if skip.iter().any(|s| p.starts_with(s)) {
            continue;
        }
        let (f, b) = walk_totals(&p, skip);
        totals.0 += f;
        totals.1 += b;
    }
    totals
}

/// The registered worktrees that live inside `checkout`, as canonical paths, for the checkout's
/// walk to skip (each is counted by its own step). Compared canonically: git reports worktrees
/// by the spelling it was given and the checkout is canonical, so a raw prefix test misses an
/// alias such as macOS's `/var` for `/private/var` and counts the files twice.
fn worktrees_inside(checkout: &Path, worktrees: &[PathBuf]) -> Vec<PathBuf> {
    let checkout = crate::onboard::finding_context::canonicalize_existing_prefix(checkout);
    worktrees
        .iter()
        .map(|w| crate::onboard::finding_context::canonicalize_existing_prefix(w))
        .filter(|w| w.starts_with(&checkout))
        .collect()
}

/// Session files recorded for deletion: for each id, every `<claude dir>/*/<id>.jsonl` file and
/// `<claude dir>/*/<id>/` directory that exists. Blocking.
fn find_session_paths(claude_dir: &Path, id: &str) -> Vec<(PathBuf, bool)> {
    let mut out = Vec::new();
    let Ok(read) = std::fs::read_dir(claude_dir) else {
        return out;
    };
    for project_dir in read.filter_map(Result::ok).map(|e| e.path()) {
        let Ok(meta) = std::fs::symlink_metadata(&project_dir) else {
            continue;
        };
        // Only real directories: a symlink in here is not Camerata's to follow.
        if !meta.is_dir() {
            continue;
        }
        let file = project_dir.join(format!("{id}.jsonl"));
        if std::fs::symlink_metadata(&file).is_ok_and(|m| m.is_file()) {
            out.push((file, false));
        }
        let sidecar = project_dir.join(id);
        if std::fs::symlink_metadata(&sidecar).is_ok_and(|m| m.is_dir()) {
            out.push((sidecar, true));
        }
    }
    out
}

/// Build the plan for a guarded target. Reads the disk and the stores; removes nothing.
pub async fn plan(state: &AppState, target: &GuardedTarget) -> PurgePlan {
    let mut steps = Vec::new();

    // Worktrees, then the checkout. A worktree that lives inside the checkout is counted by its
    // own step and skipped by the checkout's walk, so no file is counted twice.
    let worktrees = target.worktrees.clone();
    for wt in &worktrees {
        let wt = wt.clone();
        let totals = tokio::task::spawn_blocking({
            let wt = wt.clone();
            move || walk_totals(&wt, &[])
        })
        .await
        .unwrap_or((0, 0));
        steps.push(PurgeStep {
            item: PurgedItem {
                kind: PurgedKind::Worktree,
                path: wt.to_string_lossy().into_owned(),
                files: totals.0,
                bytes: totals.1,
                outcome: PurgeOutcome::Planned,
            },
            action: Action::Worktree(wt),
        });
    }
    let inside = worktrees_inside(&target.path, &worktrees);
    let checkout = target.path.clone();
    let totals = tokio::task::spawn_blocking({
        let checkout = checkout.clone();
        move || walk_totals(&checkout, &inside)
    })
    .await
    .unwrap_or((0, 0));
    steps.push(PurgeStep {
        item: PurgedItem {
            kind: PurgedKind::Checkout,
            path: checkout.to_string_lossy().into_owned(),
            files: totals.0,
            bytes: totals.1,
            outcome: PurgeOutcome::Planned,
        },
        action: Action::Checkout(checkout.clone()),
    });

    // The server's incremental-scan cache.
    let cached = state.scan_cache.peek_repo(&target.project_id, &target.repo);
    steps.push(PurgeStep {
        item: PurgedItem {
            kind: PurgedKind::ServerScanCache,
            path: format!("scan-cache.json#{}/{}", target.project_id, target.repo),
            files: 0,
            bytes: cached.bytes,
            outcome: PurgeOutcome::Planned,
        },
        action: Action::ServerScanCache,
    });

    // The CLI's cache and ledger, keyed by the canonical checkout path (computed now, while the
    // checkout still exists).
    if let Some(cli_dir) = &state.storage_roots.cli_state_dir {
        let [cache, ledger] = repo_state_files_in(cli_dir, &checkout);
        for (kind, path) in [
            (PurgedKind::CliScanCache, cache),
            (PurgedKind::CliLedger, ledger),
        ] {
            let totals = walk_totals(&path, &[]);
            steps.push(PurgeStep {
                item: PurgedItem {
                    kind,
                    path: path.to_string_lossy().into_owned(),
                    files: totals.0,
                    bytes: totals.1,
                    outcome: PurgeOutcome::Planned,
                },
                action: Action::File(path),
            });
        }
    }

    // Claude Code session files for every session recorded on any of the project's inspections.
    if let Some(claude_dir) = &state.storage_roots.claude_projects_dir {
        let mut ids: Vec<String> = state
            .inspections
            .list(&target.project_id)
            .into_iter()
            .flat_map(|e| e.model_sessions)
            .map(|s| s.session_id)
            .collect();
        ids.sort();
        ids.dedup();
        for id in ids.into_iter().filter(|id| is_session_id(id)) {
            let claude_dir = claude_dir.clone();
            let id_for_walk = id.clone();
            let dir_for_walk = claude_dir.clone();
            let found = tokio::task::spawn_blocking(move || {
                find_session_paths(&dir_for_walk, &id_for_walk)
            })
            .await
            .unwrap_or_default();
            if found.is_empty() {
                steps.push(PurgeStep {
                    item: PurgedItem {
                        kind: PurgedKind::ModelSessionFile,
                        path: claude_dir
                            .join("*")
                            .join(format!("{id}.jsonl"))
                            .to_string_lossy()
                            .into_owned(),
                        files: 0,
                        bytes: 0,
                        outcome: PurgeOutcome::Planned,
                    },
                    action: Action::SessionAbsent,
                });
                continue;
            }
            for (path, is_dir) in found {
                let totals = walk_totals(&path, &[]);
                steps.push(PurgeStep {
                    item: PurgedItem {
                        kind: PurgedKind::ModelSessionFile,
                        path: path.to_string_lossy().into_owned(),
                        files: totals.0,
                        bytes: totals.1,
                        outcome: PurgeOutcome::Planned,
                    },
                    action: if is_dir {
                        Action::SessionDir(path)
                    } else {
                        Action::Session(path)
                    },
                });
            }
        }
    }

    // In-memory copies.
    steps.push(PurgeStep {
        item: PurgedItem {
            kind: PurgedKind::InMemory,
            path: "memory:scan transcripts".to_string(),
            files: 0,
            bytes: 0,
            outcome: PurgeOutcome::Planned,
        },
        action: Action::Transcripts,
    });
    steps.push(PurgeStep {
        item: PurgedItem {
            kind: PurgedKind::InMemory,
            path: format!("memory:finished scan jobs of {}", target.project_id),
            files: 0,
            bytes: 0,
            outcome: PurgeOutcome::Planned,
        },
        action: Action::Jobs,
    });

    PurgePlan { steps }
}

/// Remove one file or directory tree, mapping "was not there" to `AlreadyAbsent`.
fn remove_path(path: &Path) -> PurgeOutcome {
    if let Err(blocked) = ensure_not_real_location_in_test(path) {
        return PurgeOutcome::Failed {
            error: blocked.to_string(),
        };
    }
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return PurgeOutcome::AlreadyAbsent,
        Err(e) => {
            return PurgeOutcome::Failed {
                error: e.to_string(),
            }
        }
    };
    let result = if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match result {
        Ok(()) => PurgeOutcome::Removed,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => PurgeOutcome::AlreadyAbsent,
        Err(e) => PurgeOutcome::Failed {
            error: format!("{}: {e}", path.display()),
        },
    }
}

/// Execute `plan` against `target`, returning every item with its real outcome. Every step runs
/// even after a failure (the record must be the truth about what remains), in plan order.
pub async fn execute(state: &AppState, target: &GuardedTarget, plan: PurgePlan) -> Vec<PurgedItem> {
    let mut done = Vec::with_capacity(plan.steps.len());
    for PurgeStep { mut item, action } in plan.steps {
        item.outcome = match action {
            Action::Worktree(path) => {
                // Let git unregister it first (it removes the directory too); whatever it leaves
                // is removed directly. "Already absent" only if it was not there to begin with.
                let existed = std::fs::symlink_metadata(&path).is_ok();
                if existed {
                    let p = path.to_string_lossy().into_owned();
                    let _ = workspace::git(
                        Some(&target.path),
                        &["worktree", "remove", "--force", "--force", &p],
                    )
                    .await;
                }
                match run_blocking(move || remove_path(&path)).await {
                    PurgeOutcome::AlreadyAbsent if existed => PurgeOutcome::Removed,
                    other => other,
                }
            }
            Action::Checkout(path) => {
                // Unregister every worktree record before the checkout (and its .git) goes.
                let _ = workspace::git(Some(&path), &["worktree", "prune"]).await;
                let owner = path.parent().map(Path::to_path_buf);
                let ws_root = state.settings.workspace_root().map(|w| {
                    crate::onboard::finding_context::canonicalize_existing_prefix(Path::new(&w))
                });
                let outcome = run_blocking(move || remove_path(&path)).await;
                if outcome.is_clear() {
                    if let (Some(owner), Some(ws_root)) = (owner, ws_root) {
                        remove_empty_owner_dir(&owner, &ws_root);
                    }
                }
                outcome
            }
            Action::ServerScanCache => {
                state
                    .scan_cache
                    .remove_repo(&target.project_id, &target.repo);
                // The slice is gone either way; "absent" only when there was nothing to take.
                if item.bytes == 0 {
                    PurgeOutcome::AlreadyAbsent
                } else {
                    PurgeOutcome::Removed
                }
            }
            Action::File(path) | Action::Session(path) | Action::SessionDir(path) => {
                run_blocking(move || remove_path(&path)).await
            }
            Action::SessionAbsent => PurgeOutcome::AlreadyAbsent,
            Action::Transcripts => {
                state.transcripts.clear(crate::SCAN_AUDIT_KEY);
                PurgeOutcome::Removed
            }
            Action::Jobs => {
                let n = state.jobs.remove_finished_for_project(&target.project_id);
                if n == 0 {
                    PurgeOutcome::AlreadyAbsent
                } else {
                    PurgeOutcome::Removed
                }
            }
        };
        done.push(item);
    }
    done
}

async fn run_blocking(f: impl FnOnce() -> PurgeOutcome + Send + 'static) -> PurgeOutcome {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| PurgeOutcome::Failed {
            error: format!("removal task failed: {e}"),
        })
}

/// Remove the `<owner>` folder of a checkout once it is EMPTY, and only when it sits strictly
/// inside the workspace root. `remove_dir` fails on a non-empty directory, which is the check.
fn remove_empty_owner_dir(owner: &Path, workspace_root: &Path) {
    let owner = crate::onboard::finding_context::canonicalize_existing_prefix(owner);
    if owner != workspace_root && owner.starts_with(workspace_root) {
        let _ = std::fs::remove_dir(&owner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_uuid_shaped_ids_may_become_path_components() {
        assert!(is_session_id("6f1d2c3a-1111-4222-8333-444455556666"));
        assert!(!is_session_id("../../etc/passwd"));
        assert!(!is_session_id(""));
        assert!(!is_session_id("6f1d2c3a11114222833344445555666677"));
        assert!(!is_session_id("6f1d2c3a-1111-4222-8333-44445555666/"));
        assert!(!is_session_id("zzzzzzzz-1111-4222-8333-444455556666"));
    }

    #[test]
    fn walk_totals_counts_files_and_bytes_and_honours_skips() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/one"), "12345").unwrap();
        std::fs::write(root.join("a/b/two"), "123").unwrap();
        std::fs::create_dir_all(root.join("skipme")).unwrap();
        std::fs::write(root.join("skipme/three"), "1234567890").unwrap();

        assert_eq!(walk_totals(root, &[]), (3, 18));
        assert_eq!(walk_totals(root, &[root.join("skipme")]), (2, 8));
        assert_eq!(walk_totals(&root.join("a/one"), &[]), (1, 5));
        assert_eq!(walk_totals(&root.join("missing"), &[]), (0, 0));
    }

    /// A worktree reported through an alias of the checkout's real path is still recognised as
    /// inside it, so the checkout's walk skips it and no file is counted twice.
    #[cfg(unix)]
    #[test]
    fn worktrees_inside_compares_canonical_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().canonicalize().unwrap().join("real");
        std::fs::create_dir_all(real.join("checkout/.camerata-worktrees/wt")).unwrap();
        std::fs::create_dir_all(real.join("elsewhere/wt")).unwrap();
        let alias = tmp.path().canonicalize().unwrap().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();

        let inside = worktrees_inside(
            &real.join("checkout"),
            &[
                alias.join("checkout/.camerata-worktrees/wt"),
                alias.join("elsewhere/wt"),
            ],
        );

        assert_eq!(inside, vec![real.join("checkout/.camerata-worktrees/wt")]);
        // And the checkout's walk then skips it.
        std::fs::write(real.join("checkout/.camerata-worktrees/wt/f"), "12345").unwrap();
        std::fs::write(real.join("checkout/g"), "123").unwrap();
        assert_eq!(walk_totals(&real.join("checkout"), &inside), (1, 3));
    }

    #[cfg(unix)]
    #[test]
    fn walk_totals_does_not_follow_symbolic_links() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::fs::write(root.join("real/f"), "abcd").unwrap();
        std::fs::create_dir_all(root.join("tree")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("tree/link")).unwrap();
        // The link counts as one entry; the real directory behind it is not walked through it.
        assert_eq!(walk_totals(&root.join("tree"), &[]).0, 1);
    }

    #[test]
    fn session_paths_are_found_in_every_project_folder_by_id_only() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path();
        let id = "6f1d2c3a-1111-4222-8333-444455556666";
        let other = "9d1c0000-0000-4000-8000-000000000000";
        for dir in ["-work-a", "-work-b"] {
            std::fs::create_dir_all(claude.join(dir)).unwrap();
        }
        std::fs::write(claude.join(format!("-work-a/{id}.jsonl")), "x").unwrap();
        std::fs::create_dir_all(claude.join(format!("-work-b/{id}"))).unwrap();
        std::fs::write(claude.join(format!("-work-b/{other}.jsonl")), "x").unwrap();
        std::fs::write(claude.join("loose.jsonl"), "x").unwrap();

        let mut found = find_session_paths(claude, id);
        found.sort();
        assert_eq!(found.len(), 2);
        assert!(found
            .iter()
            .any(|(p, dir)| !dir && p.ends_with(format!("{id}.jsonl"))));
        assert!(found.iter().any(|(p, dir)| *dir && p.ends_with(id)));
        assert!(find_session_paths(claude, "11111111-1111-4111-8111-111111111111").is_empty());
    }

    #[test]
    fn remove_path_maps_absent_removed_and_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("f");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(remove_path(&file), PurgeOutcome::Removed);
        assert_eq!(remove_path(&file), PurgeOutcome::AlreadyAbsent);
        let dir = tmp.path().join("d/e");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("g"), "x").unwrap();
        assert_eq!(remove_path(&tmp.path().join("d")), PurgeOutcome::Removed);
        assert!(!tmp.path().join("d").exists());
    }

    /// The execution-level proof that no test can delete a real location: even handed a path
    /// under the real home directly (bypassing the guard and the injected roots), `remove_path`
    /// refuses. The path does not exist, and nothing is created by asking.
    #[test]
    fn remove_path_refuses_a_real_user_location_from_a_test_process() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        for probe in [
            home.join("camerata-purge-probe-that-does-not-exist"),
            home.join(".claude")
                .join("projects")
                .join("-probe")
                .join("6f1d2c3a-1111-4222-8333-444455556666.jsonl"),
        ] {
            let outcome = remove_path(&probe);
            assert!(
                matches!(&outcome, PurgeOutcome::Failed { error } if error.contains("test process")),
                "{outcome:?}"
            );
            assert!(!probe.exists());
        }
    }

    #[test]
    fn an_owner_folder_is_removed_only_when_empty_and_inside_the_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().canonicalize().unwrap().join("work");
        let owner = ws.join("acme");
        std::fs::create_dir_all(&owner).unwrap();
        // Not empty: stays.
        std::fs::create_dir_all(owner.join("other-repo")).unwrap();
        remove_empty_owner_dir(&owner, &ws);
        assert!(owner.exists());
        // Empty: goes.
        std::fs::remove_dir(owner.join("other-repo")).unwrap();
        remove_empty_owner_dir(&owner, &ws);
        assert!(!owner.exists());
        // The workspace root itself is never removed, even when empty.
        remove_empty_owner_dir(&ws, &ws);
        assert!(ws.exists());
        // Outside the workspace root: untouched.
        let elsewhere = tmp.path().canonicalize().unwrap().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        remove_empty_owner_dir(&elsewhere, &ws);
        assert!(elsewhere.exists());
    }
}
