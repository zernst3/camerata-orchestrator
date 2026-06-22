# Scan-type selector + deterministic-scan progress indicator (CI-security Part C)

**Date:** 2026-06-22 · **Decided by:** Zach · **Builds on:**
[[2026-06-22_ci_security_rules_and_scan_time_preview]] ("Scan-type selector" +
"Deterministic-scan progress indicator" sections), Part B (scan-time deterministic preview).

Two onboarding scan-UX additions, on the same audit pipeline + audit UI:

- **(A) a scan-TYPE selector** — at audit-start the user picks WHICH scans to run: the
  **AI architectural review** and/or the **deterministic scans**.
- **(B) a deterministic-scan PROGRESS indicator** — a "Deterministic scan" component above
  the AI agent-activity drawer, showing the deterministic pass's per-tool state.

Definitions: **AI review** = the LLM architectural scan (architectural/structured/prose
rules, plus the deep tier). **Deterministic scans** = the always-on security floor + the
scan-time mechanical preview pass (`run_scan_tools`).

## (A) Scan-type selector — request flags + gating

### Request flags

`AuditReq` (both `/api/onboard/audit` and `/api/onboard/audit/start`) gains two flags, BOTH
`#[serde(default = "default_true")]` so an old/omitting client keeps today's behaviour
(both scans run):

- `run_ai_review: bool` — run the LLM architectural review.
- `run_deterministic: bool` — run the security floor + the scan-time preview.

### Both-false rule: default-both (not a 4xx)

`effective_scan_modes(run_ai_review, run_deterministic)` resolves the pair. If BOTH arrive
false it forces both back to **true** (returns `(true, true, coerced=true)`) — never a no-op
scan. We chose **default-both over a 4xx**: a both-false request is only reachable by a
hand-crafted call (the UI keeps at least one checked and also coerces client-side), so the
scan still doing useful work is friendlier than an error. The UI's checkboxes both default ON
and the hint reads "At least one must be ticked — unticking both runs both."

### Gating (where each pass is skipped)

`audit_repos` gains `run_ai_review: bool` + `run_deterministic: bool`:

- `run_ai_review == false` → the ENTIRE AI review is skipped per repo: no carried findings
  (those are AI results), no `ai_audit::audit_repo` call, no deep tier, **no model calls / no
  tokens**. (`deep_inputs` are not even captured.) A note is added: "AI review deselected —
  deterministic only."
- `run_deterministic == false` → the always-on floor (`audit_files`) is skipped. The handlers
  also skip `merge_scan_preview` (the scan-time preview pass) on the same flag.

The token-free guarantee is tested directly: `deterministic_only_runs_floor_and_skips_ai`
runs a real `audit_repos` over a scratch repo with a hardcoded secret, asserts the floor
flags it AND that `actual_usage.calls == 0` (zero model calls). `deterministic_off_skips_floor`
asserts the floor is skipped when deterministic is off.

### UI

Two labeled checkboxes ("AI architectural review", "Deterministic scans (floor + linters)"),
default both ON, sent with the audit request. Hint copy makes deterministic-only's value
explicit: "fast, no LLM, NO TOKENS." The existing scan-mode (Parallel/Sequential/Background/
Batch) picker is unchanged. The deep-tier toggle is hidden when AI review is off (deep is an
LLM tier).

## (B) Deterministic-scan progress — model + UI

### Job model (server)

The async job (`JobState`) gains a `deterministic: DetProgress` section, separate from the AI
`done`/`total`:

- `DetProgress { tools: Vec<DetToolProgress>, done, total }`.
- `DetToolProgress { tool, status, findings }`, status one of `starting` | `running` | `done`
  (the `det_status` constants), mirroring how the AI passes stream `running` → `done`.

`JobStore` methods drive it: `det_register_tool` (add-if-missing, grows `total`, idempotent —
never resets an in-flight/done status), `det_tool_running`, `det_tool_done(tool, findings)`
(increments `done` exactly once, idempotent on re-finish). `floor` is one tool row; each
preview linter (`clippy`/`ruff`/`eslint`/`semgrep`) is another; `unrouted` collects rules
with no driveable tool.

### Emitters

- The **floor** reports progress from `audit_repos` (running → done with its findings count),
  gated on `run_deterministic`.
- `run_scan_tools` gains a `progress: Option<(&JobStore, &str)>` arg: it pre-registers every
  tool it will drive (accurate denominator), then streams each tool running → done with its
  findings count. `merge_scan_preview` threads the job through.

The poll endpoint (`/api/onboard/audit/job/:id`) returns the section as part of `JobState`.

### Deterministic-only routes through the job path

Live deterministic progress is only pollable on the async **job** path (the sync path holds
one request and returns the final report). So the UI routes a **deterministic-only** scan
(`run_deterministic && !run_ai_review`) through the job path regardless of the picked mode —
that's where the per-tool progress streams. (Mixed/AI scans honour the picked mode as before.)

### UI component

`DeterministicProgress` renders ABOVE the AI agent-activity drawer: an overall done/total bar
plus per-tool rows (glyph + label + `running`/`done` state + findings count). It's the PRIMARY
progress view in deterministic-only mode, where the AI drawer is empty. Styled to match the
existing job-progress bar (`.det-progress*` in `style.rs`). The poll loop surfaces it the
moment any tool registers and clears it on done/failed.

## Defaults summary

| Flag                | Default | Effect when false                                      |
|---------------------|---------|--------------------------------------------------------|
| `run_ai_review`     | true    | skip ALL LLM passes (semantic + deep) — no tokens      |
| `run_deterministic` | true    | skip the floor + the scan-time preview pass            |
| both false          | →both true | never a no-op scan (default-both, not a 4xx)        |

## Files

- `crates/server/src/jobs.rs` — `DetProgress`/`DetToolProgress`/`det_status`, `JobStore`
  det_* methods, tests.
- `crates/server/src/scan_tools.rs` — `run_scan_tools` `progress` arg.
- `crates/server/src/onboard.rs` — `audit_repos` flags + floor gating/progress + AI gating;
  gating tests.
- `crates/server/src/lib.rs` — `AuditReq` flags, `effective_scan_modes`, handler gating,
  `merge_scan_preview` job arg; flag/coercion tests.
- `crates/ui/src/cockpit.rs` — selector checkboxes, `Det*View`, `DeterministicProgress`,
  `poll_job` det signal, request wiring, deterministic-only → job routing; tests.
- `crates/ui/src/style.rs` — `.scan-type-selector` + `.det-progress*`.
