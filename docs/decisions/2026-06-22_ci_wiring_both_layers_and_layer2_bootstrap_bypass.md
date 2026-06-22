# CI-wiring covers both layers + a one-time layer-2 bootstrap bypass

**Date:** 2026-06-22 · **Decided by:** Zach

## 1. CI-wiring wires the repo's CANONICAL check command (→ both layers), not just CI

Layer 2 (Camerata's in-loop post-task check during a governed run) and layer 3 (the repo's own
CI on every PR) run the **same checks** — the repo's lint/test commands — they differ only in
where/when. So the CI-wiring story now instructs wiring each check into the repo's **canonical
check command** (the lint/test command layer 2 runs) **and** the CI workflow. One wiring covers
both: layer 2 picks it up automatically (it runs the repo's lint/test), layer 3 runs the same
command on every PR (catching non-Camerata changes too).

Done in `onboard_ci_rules` (both the mechanical and architectural story bodies + the shared
preamble). Previously the wording was CI-only ("wire it into the CI workflow"), which on a repo
with no pre-existing lint script could produce a CI-only step layer 2 never invokes.

## 2. The chicken-and-egg: a one-time layer-2 bootstrap bypass

Layer 2 is fail-closed: a repo with a manifest but no lint/test wired → "could-not-run" (a hard
failure), not a silent pass. That is correct governance, but it creates a **bootstrap deadlock**:
the very dev run that would *install* the linters/checkers fails layer 2 *because the tools aren't
there yet* — with no way to land the tooling through Camerata.

**Decision — an explicit, one-time layer-2 bypass for the bootstrapping run:**
- The development run gains an explicit, default-OFF **"bootstrap run — skip layer-2 checks"**
  option, surfaced with a clear explanation that it's for installing the tooling layer-2 needs.
- **Layer 1 (deny-before-write) STILL applies** — the bypass skips ONLY the post-task lint/test
  bounce (layer 2), never the security gate. You never bypass the gate.
- It is deliberate and visible (the architect knowingly enables it for the tool-installing run),
  not a silent or sticky default. Intended as the bootstrap escape hatch, then turned back off.

Implementation: `start_run` accepts a `skip_layer2: bool` (bootstrap) flag; when set, the fleet
runs that one run with a no-op layer-2 runner (no bounce). The UI exposes it as a labeled toggle
on the development run control. Resolves the deadlock without weakening the gate or making
fail-closed the silent-pass default.

Relates to [[camerata_layer2_uses_repo_pinned_toolchain]] and the universal-gate principle
(layer 1 is never bypassed).
