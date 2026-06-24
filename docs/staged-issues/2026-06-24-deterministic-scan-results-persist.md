# Deterministic scan: per-tool results panel must persist after the scan completes

> **Status: STAGED — not yet filed as a GitHub issue.**
> On the next "GitHub push", create this as a **sub-issue of the Pillar 1 · Onboarding Epic (#1)** using the title + body below.

**Title:** Persist the deterministic-scan per-tool results panel after the scan finishes

---

## Problem

During a deterministic scan, the **"Deterministic scan"** panel shows a per-tool breakdown with finding counts — e.g.:

```
Deterministic scan                      4/5 tools
✓ Security floor    229 finding(s)
✓ clippy             83 finding(s)
✓ semgrep            28 finding(s)
✓ Unrouted rules      6 finding(s)
· dep-audit          starting
```

Once the scan **completes**, this per-tool panel **disappears** — only the aggregate findings / triage tables remain. The per-tool summary (which tool ran, how many it produced, done/skipped state) is useful context and should persist after completion, not be cleared on the transition to triage.

## Expected

- After the deterministic scan finishes, the per-tool results panel stays visible (in place, or as a collapsible "scan summary"), showing each tool + its final finding count + done/skipped state.
- Ideally it also re-appears when the project's onboarding view is revisited (ties into the scan-results persistence work: the completed scan should be reconstructable, not only live-streamed).

## Notes (implementation pointers)

- The per-tool data is the job's `DetProgress` (the `det_tool_running` / `det_tool_done` counts in `crates/server/src/jobs.rs`, rendered in the deterministic-scan panel in `crates/ui/src/cockpit/scan.rs`). Today the panel is bound to the live in-progress job and is dropped when the job completes / the view switches to the findings tables.
- Fix: retain the completed `DetProgress` summary and keep rendering it after completion (and persist/restore it with the rest of the scan state — related to the server-authoritative `last_scan` work landed for the chat grounding).

## Parent

Epic **#1 · Pillar 1 · Onboarding (brownfield scan)**.
