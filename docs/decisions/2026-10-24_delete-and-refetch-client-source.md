# ADR: Delete and re-fetch client source

**Date:** 2026-10-24
**Status:** Accepted; BUILT on `feat/audit-report-export` (see the build order in the design).
**Deciders:** Zach (architect), Claude (architect and builder)

Design: [`docs/design/2026-10-24_delete-and-refetch-client-source.md`](../design/2026-10-24_delete-and-refetch-client-source.md).
Binding decisions: the private decision log, the 2026-10-22 entries "Delete client source after
delivery" and its re-fetch revision. Reference: `docs/TECHNICAL.md` section 7.

## Context

An inspection reads a client's code, and Camerata copies it in places the operator does not see: the
checkout, UoW worktrees, the server's incremental-scan cache, the CLI's cache directory, and the Claude
Code session files that persist every prompt of a CLI-backend call. A client who asks "is our code
still on your laptop" had no honest answer, and the product depended on the checkout in places it did
not need to (exports 404ed after a restart because the last scan lived only in memory).

## Decision

1. **The inspection is the durable artifact; the source is a cache.** Every completed inspection is
   persisted (`InspectionStore`, one file per project). `get_last_scan` falls back to it after a
   restart. Findings, the review screen, exports and package regeneration read only that. The package
   delivered from an inspection is recorded on it (hash and manifest).
2. **One button per repo deletes the source and everything derived from it**, confirmed by typing the
   repo's name. It removes the worktrees, the checkout, the repo's slice of the scan cache, the CLI
   cache and ledger, and exactly the Claude Code session files the inspection recorded (each model call
   is tagged `--session-id`, the ids are stored on the inspection entry, and they are listed in the
   record), then writes a deletion record: the proof handed to the client.
3. **Kept on purpose:** the finding excerpts (in the review draft and the inspection). They are the
   deliverable. The record states this.
4. **The deletion is guarded, not trusted.** It refuses in a fixed order of thirteen checks (including
   the remote-match guard: the folder's `origin` must be exactly this repo on `github.com`), and
   refuses uncommitted work and commits GitHub does not have unless the operator ticks "Delete
   uncommitted work too". A scan or another source operation in flight is refused, not raced.
5. **Deleted is not unlinked.** A repo whose newest record is a deletion does not pause the project.
   Controls that need the source are disabled with a stated reason and the server answers 409
   `source_deleted`; code views say "source deleted on <date>, re-fetch to view" and otherwise read
   the inspected commit through git objects, never a missing folder.
6. **Re-fetch brings the code back with full history** (never `--depth`), the latest or exactly the
   inspected commit. **Rescan changes** inspects only what changed since the inspection, through a
   synthetic prior manifest, so a monthly retest costs the change, not the repository.
7. **Tests can never delete anything real.** Every location outside the checkout is injected
   (`StorageRoots`, empty by default; only `from_env()` installs real ones), and a backstop
   (`ensure_not_real_location_in_test`, the shape of `RealToolingProtected`) refuses a test binary
   under the real home, data, cache or `~/.claude` directories.

## Defects fixed on the way

D1 ruff wrote `.ruff_cache/` into the client's checkout (now `--no-cache`). D2 semgrep sent anonymous
metrics to its vendor (now `--metrics=off`). D3 `GET .../suppressions` ran `git pull` on every read,
moving a checkout past the inspected commit just by opening a screen (pull removed). Also fixed:
`parse_owner_repo` accepted lookalike hosts (`notgithub.com/acme/api` parsed as `acme/api`), which
mattered once deletion trusted it; the last scan was lost on restart.

## As built, where it differs from the design

- The end-to-end tests are in `crates/server/src/client_source/e2e_tests.rs`, not `crates/server/
  tests/source_deletion_e2e.rs`: the fixture, the sandboxed roots and the job runner are crate-internal
  on purpose (nothing outside the crate can aim a deletion at anything), and `AppState`'s stores are
  private. They use the real HTTP router.
- The purge-completeness test writes the CLI cache and ledger with the shared key functions
  (`scan_cache::state_dir`) instead of running `camerata inspect`: the CLI crate depends on the server,
  so the reverse would need new dev-dependencies and a lockfile change. The key function is the single
  implementation both use. The scan in that test uses a fake `claude` with AI review on (not floor
  only) so the recorded session ids are real.
- `client_source::KNOWN_STORAGE_LOCATIONS(state)` is `storage::known_storage_locations(&roots)`: it
  takes the roots, which is what the test injects.
- `delete-preview` also returns `item_view` and `total_summary` (worded by the record's own formatter),
  and the `deleted` status carries a `summary` line, so the UI never re-implements the wording.
- The `exports_never_touch_...` source-grep allows one read: Typst's compiled PDF in its own temp
  directory.
- Synchronous scan handlers (`/api/onboard/scan`, `/audit`) and the alternatives rescan now register
  as running scans (the design only named the job path), because they hold the checkout open for the
  whole request.
- "Rescan changes" hands its job to the Onboard review's existing poller. Results show where an audit
  job's results always show, which needs the onboarding draft or an in-session scan to be present.

## Consequences

- Operators get a real answer for the client, and a checkable record.
- A new store that can hold client code must be named in `storage::DATA_DIR_ENTRIES` with a treatment;
  a test fails otherwise. A new export that reads the repository fails `exports_never_touch_...`.
- The code viewer (built after this) reads through `client_source::read` and gets the deleted and
  newer-source cases for free.
