# Scan header: the "X/Y passes" progress counter is frozen at 0/0 and never advances

> **Status: STAGED — not yet filed as a GitHub issue.**
> On the next "GitHub push", create this as a **sub-issue of the Pillar 1 · Onboarding Epic (#1)** using the title + body below.

**Title:** Scan progress header "X/Y passes" stays 0/0 — must update through to completion

---

## Problem

During a scan the header reads:

```
0/0 passes · 229 finding(s) so far
Camerata is auditing your code…   [Stop scan]
```

The **"229 finding(s)"** part updates live, but the **"0/0 passes"** counter stays frozen at `0/0` for the entire scan regardless of actual progress — it never advances and never reaches a completed state.

## Root of it

"Passes" is the AI-audit pass counter (`done/total`). A **deterministic** scan (no LLM, no tokens) runs **zero** AI passes, so `done/total` is legitimately `0/0` — but that makes the header look stuck and conveys no progress, even though there is real progress (the deterministic tool panel shows `4/5 tools`, with per-tool finding counts).

## Expected

The top-line progress indicator must reflect **actual** progress and advance all the way through to completion. Specifically:
- For a **deterministic** scan, derive the header progress from the deterministic stage (e.g. the `N/total tools` already shown in the panel), not the AI-pass counter — or drop the "passes" framing entirely when there are no AI passes.
- For an **AI audit**, the `X/Y passes` counter should advance per completed pass as it does today.
- Either way it must never sit frozen at `0/0` while work is happening, and it should land on a completed state when the scan finishes.

## Notes (implementation pointers)

- The header counter is the audit pass progress (`done`/`total` on the job state). The deterministic per-tool progress is the `DetProgress` (`det_tool_running`/`det_tool_done`) already rendered in the panel.
- Fix: the header should pick its progress source from the active scan stage — deterministic tool progress when there are no AI passes, pass count when auditing — so it advances meaningfully in both modes.

## Parent

Epic **#1 · Pillar 1 · Onboarding (brownfield scan)**. (UX-adjacent to #67 Platform/UX & Docs, but it's the onboarding scan flow, so #1 is the parent.)
