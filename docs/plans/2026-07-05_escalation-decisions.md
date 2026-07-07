# Escalation decisions — 2026-07-05

Every item the audit blitz escalated (held back because it needs a human call). Each has THE QUESTION,
options, and a recommendation. Detail for any finding is in `docs/ARCH_AUDIT_2026-07-04_fable5-complete.md`.

## APPROVED 2026-07-05 — ALL 21 greenlit

Batch 1 (GAP-2, LIFECYCLE-10, GATE-F7) landed on fix/gate-hardening.
**Status: Batch 2 (LIFECYCLE-1/2/3/4) landed on `fix/lifecycle-provenance`.**
**Status: Batch 3 (LIFECYCLE-5/9/12) landed on `fix/lifecycle-loop`.**
**Status: Batch 3b (LIFECYCLE-7/6) landed on `fix/lifecycle-liveness`.**
**Status: CheckRunner full-diagnostics landed on `fix/checkrunner-diagnostics` — completes LIFECYCLE-5 for open-weight models: `CheckRunner` returns `CheckOutcome { violated, diagnostics }`; Layer-2 bounce feeds the full toolchain output (16 KiB tail-capped) back at the prompt tail.**
**Status: Batch 4 ROUTES (ROUTES-9/5/7/8) landed on `fix/routes-correctness`. Server correctness: no per-request `set_var` (backend/key read from stores; fixes a flaky credential test); project-scoped + latest deep-report export; correct HTTP status codes (404/400/409, not all 500; body shape unchanged); read GETs use a non-creating UoW getter (no junk records). ADR: `docs/decisions/2026-07-05_routes-correctness.md`.**
**Status: Prompt cache-layering + kernel v2 landed on `fix/prompt-cache-layering`. Formalized the 3-layer geological assembly (`camerata_app_core::LayeredPrompt`): Layer 1 (kernel + role) top, Layer 2 (grounding) middle, Layer 3 (story + LIFECYCLE-5 error tail) bottom; a byte-stable static prefix with a unit test proving it is identical across differing Layer-3 input; a provider-neutral cache-activation abstraction (Anthropic `cache_control` breakpoints at the Layer 1/2 and end-of-Layer-2 boundaries via the grounding terminator marker; DeepSeek/GLM automatic); per-call cache-hit-ratio logging (`LlmResponse::cache_hit_ratio`); and kernel v2 (a mandatory stack-neutral `<Reasoning>` block + state-machine phase framing for the fast/balanced tier addenda). Rollout 6-9 (the remaining prompt rewrites + emitted AGENTS.md/CONVENTIONS.md preamble + audit lenses) is a noted follow-up.**

Zach approved every item. Recommendation [Rec] taken for all EXCEPT:
- **GAP-6: BUILD it now, do NOT defer** (build the remaining 3 integration-gate categories).
- **LIFECYCLE-7:** also update the stall-auto-cancel DEFAULT shown in the UI.
- **GAP-3:** approved, but design the state model + share it BEFORE building (see the design doc).

Governing principle (standing): **never defer hardening/refining/correctness of Camerata; only defer
net-new features outside MVP spirit.** So GAP-1/3/6/7/8 are all build-now (structural ones design-first).

GAP-4 (phase chat wired) landed on fix/gap4-chat: the Investigation and Development phase chats are
now live, project-grounded LLM conversations (reuse `POST /api/chat`), not stubs. See
`docs/decisions/2026-07-05_govdev-phase-chat.md`.

GAP-8 landed on fix/gap8-routine-scope: `Routine.scope` is now a structured, enforced `RoutineScope`
(rule subset + tool allowlist + write jail) that resolves onto the same gateway registration dev runs
use (`resolve_scope_registration`); serde back-compat for legacy string scopes; live execution still
latent (seam tested). ADR: docs/decisions/2026-07-05_routine-structured-scope.md.

Implementation order: Batch 1 P0 gate (GAP-2, LIFECYCLE-10, GATE-F7) -> Batch 2 lifecycle provenance
bundle (LIFECYCLE-1/2/3/4) -> Batch 3 lifecycle loop/concurrency (LIFECYCLE-5/7+6/9/12) -> Batch 4
ROUTES (5/7/8/9) -> Batch 5 GAP features (4/6/8) -> Batch 6 structural (GAP-3 LlmPort+state, GAP-1
api-types+MCP, GAP-7 CLI). Prompt phase-2 (cache-layering + rollout 6-9) rides with Batch 3's LIFECYCLE-5.

## P0 — gate / moat + provenance integrity

### GAP-2 — commit/PR gate configured but NEVER enforced
Broken: `checks/src/vcs_action.rs` + a bypass endpoint exist, but no commit/PR chokepoint calls the gate.
Q: Wire it in, at which chokepoints, and hard-block or warn?
- A) Enforce at all server commit/PR chokepoints (workspace commit_all/commit_merge, pr.rs open, dev_implement_run commit), HARD-BLOCK on violation, keep the bypass endpoint for explicit overrides. **[Rec]**
- B) Warn-only. C) Leave unenforced + mark the settings panel "not enforced".
Answer: ____

### LIFECYCLE-10 — process-global env var cross-contaminates concurrent runs' gate provenance
Broken: the per-run gate-events sink path is set via `std::env::set_var` (process-global); concurrent runs read each other's sink.
Q: Thread the sink path per-run instead?
- A) Pass the sink path per-spawn via the child `Command::env` (gateway interface unchanged). **[Rec]**
- B) Serialize live runs (one at a time). 
Answer: ____

### GATE-F7 — test-scope Waive lets the agent disable 3 floor rules by filename
Broken: naming a file test/fixture/`examples/` waives SQL-concat, disabled-TLS, unsafe-deserialization at the gate.
Q: Keep, tighten, or remove?
- A) Tighten: keep for true test files, DROP the `examples/` waive (shipped code), require explicit `camerata:allow` for TLS + deserialization. **[Rec]**
- B) Keep as-is. C) Remove entirely (test code must comply or waive explicitly).
Answer: ____

## P1 — run-engine behavioral / semantic

### LIFECYCLE-1 — Cancel is a no-op; the executor still commits + pushes after Stop
Q: Confirm cancel semantics.
- A) Add `is_cancelled` checks between steps and immediately before commit + before push; on cancel, stop BEFORE any git mutation; make `set_status` refuse to mutate a done/cancelled run. **[Rec]**
Answer: ____ (mostly a confirm; anything to change?)

### LIFECYCLE-2 — provenance/stage advances on FAILED and CANCELLED runs
Broken: `stamp_provenance_when_done` fires on any `run.done`, so a failed/cancelled run still advances Development->AwaitingQa + attaches SOC-2 evidence.
Q: What happens on a non-success terminal?
- A) Advance stage + attach QA evidence ONLY on success. For a FAILED run, still freeze gate provenance (honest record of what the gate saw) but do NOT advance stage / attach QA evidence. CANCELLED = freeze nothing, advance nothing. **[Rec]**
- B) Advance only on success; freeze nothing on failure either.
Answer: ____

### LIFECYCLE-3 — provenance watcher gives up after ~5 min
Q: Replace the 5-min poll with:
- A) A completion signal (runner signals done -> stamp then); share the "on-completion" path with LIFECYCLE-2/4. **[Rec]**
- B) A much longer / adaptive poll.
Answer: ____

### LIFECYCLE-4 — resume path spawns no provenance watcher
Q: Confirm: once 2/3 are decided, mirror the fixed watcher spawn into `resume_governed_run`. (No standalone decision; rides on 2/3.) **[Rec: yes, bundle]**
Answer: ____

### LIFECYCLE-5 — bounce loop re-runs the IDENTICAL prompt (open-weight linchpin)
Broken: Layer-2 failure reasons + compiler/gate errors are dropped; the retry is a blind re-run.
Q: Confirm the feedback fix + how much error context.
- A) Feed the violated rule ids + the FULL stack-agnostic toolchain error output back, appended at the tail of the next prompt (cache-friendly, per the prefix-cache design). **[Rec]**
- B) Summarized errors only.
Answer: ____

### LIFECYCLE-7 (+ LIFECYCLE-6) — no liveness heartbeat; stall-enforcement is dead code
Q: Two parts. (a) Wire an activity heartbeat on dev-implement + pr-resolve so a healthy long run is not "stalled". (b) Turn on the dead stall-CANCEL enforcement?
- A) Wire the heartbeat [Rec]; AND enable auto-cancel for AUTONOMOUS/routine runs only (not interactive), generous threshold. **[Rec]**
- B) Heartbeat only; leave stall-cancel off (alert-only).
Answer: ____

### LIFECYCLE-9 — no single-flight guard; concurrent runs share a worktree
Q: Concurrency policy for a second run on a story that already has an active run + sign-off safety.
- A) REJECT (409) the second run; require `run.done` before a sign-off can tear down the worktree. **[Rec]**
- B) QUEUE the second run instead of rejecting.
Answer: ____

### LIFECYCLE-12 — reject-after-bounce leaves committed snapshot commits on the branch
Broken: "Reject the agent's work" only discards uncommitted changes; prior snapshot commits survive + can be pushed.
Q: How aggressive is "reject"?
- A) `git reset --hard` to the checkpoint base_commit (fully reverts; matches the UI's "revert the agent's work" promise; destructive). **[Rec]**
- B) Leave commits, just stop (keep current behavior but fix the misleading UI text).
Answer: ____

## P2 — feature / structural

### GAP-4 — govdev phase-chat panels are canned-string stubs
Q: Wire the Investigation/Development chat tabs to the real LLM plumbing now, or disable them until built?
- A) Wire to the existing chat/escalation LLM plumbing (seams exist). **[Rec]**
- B) Replace with a disabled affordance + "coming soon" until a dedicated build.
Answer: ____

### GAP-6 — integration gate: 1 of 4 categories built
Q: Build the remaining 3 (wiring/convention/cross-cutting) now, or defer + mark the ADR "partial"?
- A) Defer + mark partial; revisit when live multi-agent runs are common. **[Rec]**
- B) Build now.
Answer: ____

### ROUTES-5 — GET deep-report returns an arbitrary job (cross-project leak risk)
Q: Confirm the fix.
- A) Add project_id + completion timestamp to `JobState`; return the latest report for THAT project. **[Rec]**
Answer: ____ (confirm?)

### ROUTES-7 — AppError maps everything (incl. not-found) to HTTP 500 (~40 sites)
Q: Do the 4xx pass?
- A) Add NotFound/BadRequest variants; use 404/400 where handler docs promise 4xx (~40 mechanical sites). **[Rec]**
- B) Leave as-is (UI tolerates it).
Answer: ____

### ROUTES-8 — side-effectful GETs persist junk on a typo'd id (~25 sites)
Q: Confirm the fix.
- A) Add a non-creating store getter; switch read paths to it; keep `get_or_create` for writes (~25 sites). **[Rec]**
Answer: ____ (confirm?)

### ROUTES-9 — runtime `set_var` in request handlers races concurrent getenv (UB)
Q: Confirm the fix.
- A) Read the effective backend/key from `AppState`/settings store instead of the process env (also aligns with the cache/kernel "read from state" direction). **[Rec]**
Answer: ____ (confirm?)

### GAP-1 — no machine-consumable capability contract; adapter ladder has no rung 1 (ROUTE-1, structural)
Q: Greenlight the direction now, or design together first?
- A) Greenlight: extract a `camerata-api-types` contract crate + a first thin MCP adapter (a few read verbs + 1-2 governed write verbs), sequenced after the lifecycle/gate fixes. **[Rec]**
- B) Design the shape together before any build.
Answer: ____

### GAP-3 — headless-core state lift (#116 Ph2) + LlmPort trait (#117 D2) unbuilt (ROUTE-1, structural)
Q: Greenlight the state-lift + LlmPort refactor?
- A) Greenlight in principle; design the state model together, then execute (it unblocks the adapter ladder). **[Rec]**
- B) Defer.
Answer: ____

### GAP-7 — CLI is a demo harness, not an HTTP adapter
Q: Build a real HTTP-client CLI (cheap proof of adapter-readiness), or just fix the architecture diagram?
- A) Build a small HTTP-client CLI (validates GAP-1 cheaply). **[Rec]**
- B) Fix the diagram + defer the CLI.
Answer: ____

### GAP-8 — routine "permission scope" is decorative prose, not enforced
Q: Replace `scope: String` with a structured, enforced scope (rule-subset + tool allowlist + write jail)?
- A) Yes, as a PREREQUISITE for live routine execution (latent until then). **[Rec]**
- B) Defer entirely.
Answer: ____
