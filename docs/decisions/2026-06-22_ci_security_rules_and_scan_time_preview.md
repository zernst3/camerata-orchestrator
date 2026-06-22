# CI security rules (Semgrep/CodeQL) + scan-time deterministic preview

**Date:** 2026-06-22 · **Decided by:** Zach (full design settled over several turns).

## The two rules — CI/CD domain, opt-in only, mandatory tier choice

Two new CI/CD-domain rules that exist ONLY to generate CI stories (a DevOps engineer implements
them). They are NOT agent directives.

- **`CICD-SEMGREP-SECURITY-SCAN-1`** — options: **Community Edition (free, LGPL-2.1 OSS CLI, runs
  on any repo incl. private, single-file)** | **AppSec Platform / Pro (paid, cross-file, Pro rules)**.
- **`CICD-CODEQL-SECURITY-SCAN-1`** — options: **public-repo (free)** | **GitHub Advanced Security
  (paid, per-committer, for private)**. The free option's directive MUST carry the full limitations:
  free only on public/OSS repos; private requires GHAS (paid); heavy whole-program DB build → CI /
  layer-3 ONLY, never scan or in-loop.

Both: `enforcement = mechanical`, `opt_in_only = true` (NEVER auto-recommended / pre-checked), and
**NO default option** — selecting forces a conscious tier choice (the amber "must choose" state).

## Schema flags (new)

- **`opt_in_only`** — a grounded rule that is never auto-recommended/pre-checked (manual opt-in).
- **`layer3_only`** — a CI-tier rule that must NEVER run at layer-2 or scan (CodeQL: too heavy).
- **scan-runnable + tool invocation** — metadata letting the scan run the tool with a
  Camerata-supplied config for the rule (Semgrep CE; the repo linters via their rule-select flags).

Propose logic (`onboard.rs`) must respect `opt_in_only` (exclude from auto-recommend).

## Scan-time deterministic PREVIEW (decoupled from the gate)

The key reconciliation: **scan-time and gate-time are different.** The repo is the source of truth
for the **gate** (layer-2/3, authoritative, repo-pinned, no drift). The **scan is an advisory
preview** — so it does NOT need to be repo-sourced.

- At onboarding scan, for EACH selected deterministic (mechanical) rule that is scan-runnable,
  **Camerata runs the tool itself with a supplied config for that rule** (clippy `-W`, ruff
  `--select`, eslint `--rule`/config, golangci-lint, rubocop, Semgrep CE `--config`), parses the
  output (SARIF where supported), and folds the findings into triage as **deterministic preview
  findings** (stable rule-ids), labeled **"preview — found by Camerata; NOT enforced until wired."**
- This works EVEN IF the rule isn't in the repo yet — you select it, you see findings. A scan
  finding is a preview, not enforcement; the CI story still must wire it for the gate to enforce it.
- Mechanical/CI rules STAY OUT of the AI/LLM review (they already are) — deterministic tools, no
  tokens. The AI review only handles judgment rules (architectural/structured/prose). This REDUCES
  token usage.
- Honest caveat: a preview uses Camerata's tool version, which may differ slightly from what the
  repo eventually pins — preview is indicative, the gate is authoritative. Graceful when a tool
  isn't installed ("couldn't preview X — enforces once wired"), never a false clean.

### The one exception
Tools that can't practically run at scan/loop — **CodeQL** (heavy DB build) and the **paid cloud
tiers** — are **story-only** (`layer3_only`, not scan-runnable). Not a licensing/repo issue at
scan; a "too heavy / not locally runnable" one.

## Gate (layer-2/3) — unchanged principle
Repo-pinned, authoritative, enforces ONLY rules wired into the repo. The bootstrap chicken-and-egg
(installing the tooling) is handled by the existing `skip_layer2` bootstrap bypass on the dev run.

## Positioning note (internalized)
These rules are the "wrap the mature engine, don't rebuild it" pattern: Camerata composes
CodeQL/Semgrep as deterministic check sources and enforces their findings at the points they don't
cover (the gate, the agent loop). The moat is the enforcement stance/point (deterministic,
deny-before-execute, in-loop, no model in the trust path, provider-neutral), not the integration.

## Scan-type selector (queued — build right after Part B)

At audit-start the user picks WHICH scans to run: **AI architectural review** (the LLM scan of
architectural/structured/prose rules) and/or **Deterministic scans** (the security floor + the new
tool-based mechanical preview). Either or both; at least one; default both (today's behavior).
Deterministic-only is fast/free/no-tokens (and makes QA of the tool pass much easier); AI-only is
just the judgment review. The audit entry points (`onboard_audit`/`_start`/`_job`) gate each pass by
the selection; the cockpit audit UI gets the toggle. Sequenced after Part B because it edits the
same audit pipeline + audit UI.

## Deterministic-scan progress indicator (queued — build with/after Part B)

Today only the AI agents show progress during a scan; the deterministic scan (floor + the new
tool pass) has no visible state. Add a **"Deterministic scan" progress component above the AI
agent-activity drawer**: the deterministic pass emits progress (per tool: starting → running →
done, with a findings count; plus overall done/total), and the component renders it. Matters most
in deterministic-only mode (no AI progress to watch). Sequenced with Part B/C — same scan UI +
audit pipeline; the deterministic pass must emit the progress events.

Relates to [[camerata_layer2_uses_repo_pinned_toolchain]], the universal gate, and the CI-wiring
both-layers decision.
