# Dogfood disk-safety: shared Cargo target + hard disk guard + terminal teardown

**Date:** 2026-06-22  **Decided by:** Zach (locked decision, all three mitigations required).

## Incident

Camerata developing itself (a large Rust workspace) with several concurrent Units of Work
produced one `target/` directory per UoW worktree because `CARGO_TARGET_DIR` was not set.
Each worktree's `cargo build / clippy / test` wrote ~5 GB of artifacts into
`<clone>/.camerata-worktrees/<branch>/target/`. With four active UoWs this multiplied to
~115 GB across worktrees, filling the disk to 131 MB free and corrupting subsequent builds.

The root cause is that `CARGO_TARGET_DIR` was never threaded into the subprocess layer,
the agent driver, or the check runners. Every worker operated in isolation without a shared
artifact store.

## Decision 1: single shared CARGO_TARGET_DIR (collapses the multiplier)

**Location:** `<clone>/.camerata-shared-target`

A sibling of `.camerata-worktrees/` inside the shared clone. Per-repo (each clone gets
its own) because different repos cannot share a cargo target. Outside every worktree root
so it never appears in any worktree's `git status`. Lives inside the Camerata-managed clone
directory and is cleaned up with it; do NOT add to the user's `.gitignore`.

**Helper functions** in `crates/server/src/workspace.rs`:
- `shared_target_dir(clone: &Path) -> PathBuf` — returns `<clone>/.camerata-shared-target`
- `ensure_shared_target_dir(clone: &Path) -> PathBuf` — creates the dir (best-effort)

**Derivation from a worktree path:** because the canonical layout is
`<clone>/.camerata-worktrees/<branch-seg>`, the clone root is `worktree.parent().parent()`.
The `derive_shared_target_dir(worktree)` helper (duplicated in `camerata-checks` and
`camerata-agent`) encodes this and falls back to `None` for out-of-band worktrees.

**Where `CARGO_TARGET_DIR` is injected:**
1. `crates/checks/src/subprocess.rs` — `run_fmt_check`, `run_clippy`, `run_test` all
   accept `target_dir: Option<&Path>` and call `.env("CARGO_TARGET_DIR", td)` when `Some`.
2. `crates/checks/src/lib.rs` — each `CheckRunner` derives the target dir from the worktree
   and passes it to the subprocess functions. Also runs the disk-headroom preflight guard.
3. `crates/agent/src/generic.rs` (`GenericCliDriver::build_command`) — injects
   `CARGO_TARGET_DIR` when `self.worktree` is set and the derivation succeeds.
4. `crates/agent/src/lib.rs` (`ClaudeCliDriver::build_command`) — same injection.

**Concurrency tradeoff:** Cargo file-locks `target/` during a build, so concurrent builds
on the same repo SERIALIZE at the lock rather than running in parallel. This is the accepted
tradeoff: correctness (no interleaved artifacts, no half-built target corruption) over
parallelism. Camerata's serial-by-default UoW execution means this rarely matters in
practice; even when it does, waiting at the lock is far better than filling the disk.

## Decision 2: hard disk preflight guard (the absolute backstop)

**Location:** `crates/server/src/workspace.rs`

Functions:
- `available_disk_bytes(path) -> Option<u64>` — single `fs2::available_space` (statvfs) call
- `has_headroom(available, min) -> bool` — pure decision function (unit-testable in isolation)
- `disk_headroom_threshold_bytes() -> u64` — reads `CAMERATA_MIN_DISK_HEADROOM_GB` env var;
  defaults to `MIN_DISK_HEADROOM_BYTES = 10 * 1024 * 1024 * 1024` (10 GiB)
- `ensure_disk_headroom(path, min_bytes) -> anyhow::Result<()>` — returns `Ok(())` when
  space is sufficient OR when the query fails (fail-open for cross-platform safety); returns
  a descriptive error naming free/required amounts and suggesting remediation when below threshold

**Call sites:**
- Top of `ensure_uow_worktree` in `workspace.rs` — refuses to create another worktree when disk is low
- Top of each `CheckRunner::check` in `lib.rs` — refuses to start a cargo build when disk is low

The check is cheap (one `statvfs` syscall) so it runs before every worktree creation and
every build without meaningful overhead.

**Disk-space crate:** `fs2 = "0.4"` added to `[workspace.dependencies]` in the root
`Cargo.toml` and declared in `camerata-server`, `camerata-checks` crates. No shell-out
to `df` — the `fs2` call is a single kernel syscall.

**Threshold override:** set `CAMERATA_MIN_DISK_HEADROOM_GB=5` (integer, GiB) to lower the
threshold for a constrained machine. Default 10 GiB is sized for a worst-case Rust workspace
where a single `cargo build` can consume 5-8 GB of new artifacts.

**Error message format:**
```
insufficient disk headroom: 0.1 GB free, need >= 10 GB; reclaim space
(remove stale worktrees under .camerata-worktrees/ or .camerata-shared-target/)
before starting more work
```

## Decision 3: terminal-state teardown + startup sweep (hygiene)

**On-sign-off teardown** was already in place at `lib.rs` around line 1244. No change needed
there.

**Startup sweep** extended (was: prune-only; now: two passes):

Pass 1 — Terminal-state sweep (new): for every UoW in `SignedOff` state that has a branch,
call `remove_uow_worktree`. This reclaims worktrees that leaked through crashes between
sign-off and the existing on-sign-off teardown. Conservative: only `SignedOff` UoWs (the
lifecycle's sole terminal stage as of this decision); branches are left intact for the PR.
If future lifecycle stages add `Abandoned` or `Failed` variants, extend the filter here.

Pass 2 — Admin-record prune (unchanged): `git worktree prune` across all repo clones.

Both passes are best-effort + non-fatal. A missing clone, unresolvable repo, or git error
is silently swallowed — the guard (Decision 2) is the hard backstop, not these hygiene steps.

**Limitation:** the startup sweep does not remove worktrees for UoWs in non-terminal stages
whose worktree dirs happened to leak (e.g. the dir was removed out-of-band but the admin
record survives). Pass 2's `git worktree prune` handles the admin-record half of that case;
the dir is already gone so there is nothing to reclaim. A future improvement could periodically
sweep `.camerata-worktrees/` against the active-UoW set and remove orphans — tracked as
a future enhancement if the guards above prove insufficient.

## Per-repo target location

```
<workspace_root>/<owner>/<repo>/              ← shared clone
  .git/
  .camerata-worktrees/
    camerata__story-7/                         ← UoW A worktree
    camerata__story-8/                         ← UoW B worktree
  .camerata-shared-target/                     ← ONE shared target (NEW)
    debug/
    release/
    ...
```

The shared target is invisible to all worktree-scoped `git status` calls. It is outside
the `.camerata-worktrees/` subtree and outside the clone's tracked files, so no `.gitignore`
entry is needed. Camerata manages the entire clone directory and may remove it on
de-onboarding.
