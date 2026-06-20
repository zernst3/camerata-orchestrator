# Evidence PR Wiring (issue #53)

**Date:** 2026-06-20
**Branch:** dev3/evidence-pr-wiring
**Status:** Implemented

## What

Wire the already-built SOC-2 evidence record and scoped security scan into the governed run
completion path and the architect sign-off gate. Specifically:

1. **Evidence assembly on run completion.** When a governed run reaches `done`, the
   `stamp_provenance_when_done` watcher now also calls `assemble_evidence_for_run`, which
   builds a `UowEvidenceRecord` from the run's gate events, provenance, and a scoped
   deterministic audit over the changed files. The record is attached to the UoW via
   `UowStore::attach_evidence` so it survives session restarts.

2. **Critical-finding sign-off gate.** A `Critical`-severity scoped-scan finding in the
   evidence record BLOCKS the `AwaitingQa -> SignedOff` sign-off transition. The block is
   enforced in the `sign_off_run` HTTP handler (not in the pure lifecycle state machine,
   because it requires reading the UoW's persisted evidence record). The architect must
   supply an explicit non-empty `waive_reason` in the sign-off request to proceed. A
   reason-less waive is rejected with HTTP 409 (CONFLICT). When a waiver is accepted, the
   reason is appended to the sign-off note so it is durable in the UoW history.

3. **PR comment posting.** When the sign-off request includes `pr_number` and `pr_repo`,
   the rendered evidence markdown (`render_pr_markdown`) is posted as a GitHub PR comment
   via the arm.rs `post_pr_comment` primitive. Graceful degradation: no token, no PR
   number, or a GitHub API error all result in `Ok(None)` — the sign-off is never blocked
   by a PR comment failure.

## Why

The evidence module (`crates/server/src/evidence.rs`) was already complete with
`UowEvidenceRecord`, `render_pr_markdown`, and `scoped_audit` — but it was an island: no
code called it from the governed run or the sign-off path. This wiring closes the loop
between the gate's runtime verdicts and the durable, PR-injectable audit trail.

The critical-finding gate is intentional product behaviour: a security finding at critical
severity means a known exploitable defect would be shipped. Forcing an explicit waiver
prevents accidental oversight while keeping the sign-off process unblocked for teams that
make a deliberate, documented trade-off.

## How

### Files changed (additive only)

- **`crates/server/src/uow.rs`**
  - Added `evidence: Option<UowEvidenceRecord>` field to `UnitOfWork` (serde default =
    `None`; backwards-compatible with persisted UoW JSON).
  - Added `impl UnitOfWork { fn is_sign_off_blocked(&self) -> bool }` — readable without
    locking the store.
  - Added `UowStore::attach_evidence(story_id, record) -> UnitOfWork` — stores the record,
    appends a history entry (`kind = "evidence"`), and flushes to disk. Does NOT change
    the lifecycle stage.

- **`crates/server/src/arm.rs`**
  - Added `post_pr_comment(owner, repo, pr_number, body, token) -> anyhow::Result<Option<String>>` —
    posts a markdown comment to a GitHub PR via the Issues API. Returns `Ok(None)` on
    graceful degradation (no token, transport error, GitHub non-2xx). Never returns `Err`
    in the current implementation; all failure modes degrade gracefully.

- **`crates/server/src/lib.rs`**
  - `stamp_provenance_when_done` now calls `assemble_evidence_for_run` after stamping
    provenance, and attaches the result via `UowStore::attach_evidence`.
  - Added `assemble_evidence_for_run(run, prov, story_id) -> UowEvidenceRecord` (private
    helper) — builds the full evidence record from a completed run.
  - `SignOffReq` gains `waive_reason: Option<String>` and `pr_number: Option<u64>` and
    `pr_repo: Option<String>` (all serde default = `None`; backwards-compatible).
  - `sign_off_run` handler updated to enforce the critical-finding gate, fold the waiver
    into the note, and attempt PR comment posting. Returns `Response` instead of
    `Result<Json<UnitOfWork>>` to allow the 409 branch.

### Changed file boundaries

- Only `uow.rs`, `arm.rs`, `lib.rs` were modified. No other files touched.
- No new crates, no moved module boundaries, no cross-crate public API breaks.

## Design decisions

### Where the sign-off gate lives (handler vs. lifecycle)

The pure lifecycle state machine (`lifecycle.rs`) does not enforce the critical-finding
gate. That machine is clock-free and I/O-free; adding an evidence-record read would break
its purity and its unit-testability without a store. The gate check belongs in the HTTP
handler (`sign_off_run`), which already reads the UoW store. This follows the existing
pattern: the decision-approval gate is also enforced at the handler level (in
`ensure_development_gate`), not in the pure state machine.

### Evidence assembly: "changed paths" for the scoped scan

For the scripted path, the gate events' `detail` fields contain the fictional target paths
(e.g. `"Backend wrote the repository method; clean."`) — not real file paths. The scoped
scan receives these as empty-content files. The deterministic floor only fires on actual
file content, so no findings are produced and `has_critical = false` for a clean scripted
run. This is correct: the scripted path exercises gate logic, not real source files.

For a live fleet run, the gate events contain the real paths the agent wrote. The scoped
scan still receives empty content because resolving the workspace root is not available
in the provenance-watcher context. A follow-up TODO is marked in the code to read actual
file content for live runs when the workspace root is threaded through.

### Waiver recording

The waiver reason is folded into the sign-off note as `[WAIVER] <reason>` so it is
durable in the UoW's `sign_off.note` field (persisted to disk) and in the AI development
history entry. No separate waiver table is needed at this scope.

### PR comment: PR number source

The PR number is supplied by the caller (the architect or the UI) in the sign-off request.
The alternative — looking up the PR from the UoW's branch via the GitHub API — would add
latency and a network call to every sign-off, including offline ones. The caller-supplied
approach degrades gracefully: if no PR number is supplied, no comment is posted.

## Usage

### Sign off without a critical finding (normal path)

```http
POST /api/runs/{run_id}/sign-off
Content-Type: application/json

{ "by": "zach", "note": "LGTM" }
```

Returns 200 with the updated UoW.

### Sign off with a critical finding (waiver required)

```http
POST /api/runs/{run_id}/sign-off
Content-Type: application/json

{ "by": "zach", "waive_reason": "Pre-existing debt; tracked in GH #99. Shipping under exception." }
```

Returns 200 with the updated UoW. The note field on the sign-off record will contain
`[WAIVER] Pre-existing debt; tracked in GH #99. Shipping under exception.`.

### Sign off WITHOUT a waiver when a critical finding is present

```http
POST /api/runs/{run_id}/sign-off
Content-Type: application/json

{ "by": "zach" }
```

Returns **409 CONFLICT**:
```json
{ "error": "sign-off blocked by critical security finding", "blocked": true, "reason": "..." }
```

### Sign off with PR comment

```http
POST /api/runs/{run_id}/sign-off
Content-Type: application/json

{
  "by": "zach",
  "pr_number": 123,
  "pr_repo": "owner/my-repo"
}
```

Posts `render_pr_markdown(evidence)` as a comment on PR #123. Requires
`CAMERATA_GITHUB_TOKEN` with Issues-write scope. On failure, the sign-off still succeeds.

## Follow-ups

- **Live-path scoped scan**: thread the workspace root through `stamp_provenance_when_done`
  so the scoped scan reads actual file content for live fleet runs (marked TODO in code).
- **PR number auto-discovery**: optionally look up the PR number from the UoW's branch
  when `pr_number` is not supplied, to remove the caller burden.
- **Evidence-in-PR-description**: optionally insert the evidence markdown into the PR
  DESCRIPTION (not just a comment) for visibility without scrolling to comments.
