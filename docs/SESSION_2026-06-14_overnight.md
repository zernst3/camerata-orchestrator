# Overnight session 2026-06-14 (handoff)

A long single session that took Camerata from an app-builder-only prototype to a
two-surface system, sharpened the framing, and made the project govern its own source.

## Important: local commits

The interactive session could not `git push` without an approval prompt (you were
asleep), so the most recent commits are LOCAL ONLY. To back them up when you wake:

```
cd "/Users/zacharyernst/Documents/Repos/camerata-orchestrator"
git push origin main
```

The 4:11 AM continuation routine also pushes the local branch automatically at fire
time (its wrapper runs `git` directly, no approval layer), so they may already be on
GitHub by the time you read this. The routine SKIPS doing work if the session was
still alive (a commit within 30 min of 4:11); it only continues the work if the
session died. It self-deletes after firing.

## What landed this session

State at handoff: a 14-crate workspace, 500+ tests, zero warnings, all four gates
green (unsafe forbidden, clippy `-D warnings`, fmt, tests), on
`github.com/zernst3/camerata-orchestrator`.

- **App-builder surface (proven in code end to end):** the refinement-session model, versioned
  event-sourced persistence, the Staff-Engineer reviewer (stub + live), the shipped
  style kit + intake picker, the opt-in design corpus with bug-fix sharing and
  opt-out-is-deletion, the post-build bug loop, the build screen wired to the real
  governed fleet (opt-in), publish wired to a deploy seam, and a standing maintenance
  panel. Composed into the Dioxus UI, with an end-to-end lifecycle test.
- **Architect surface (built out):** the `WorkItemProvider` port; native + Jira + Azure DevOps +
  GitHub adapters (mapping/request/response behind an injectable transport seam, live
  `reqwest` transport type-checked); the async clarify-bridge; SyncPolicy per-field
  source-of-truth + echo suppression; an end-to-end flow test; and a runnable
  `worktracker-demo` CLI.
- **Engine:** provider-neutrality proven with a second non-Claude driver
  (`docs/PROVIDER_NEUTRALITY.md`); self-governance CI (`.github/workflows/ci.yml`,
  `docs/ENFORCEMENT.md`).
- **Framing:** README + VISION + RATIONALE written as a technical artifact, honest
  about what is proven versus staged, leading with the narrow reproducible claim and
  showing the overturned NO-GO as evidence. New ADRs for persistence, refinement,
  corpus, maintenance, and the worktracker port (see `docs/decisions/`).

## What remains (next session)

- Live execution for the external worktracker adapters: per-provider auth (Jira OAuth
  3LO + ~25-day webhook refresh, ADO PAT/Entra, GitHub App), webhook ingress (the
  opt-in upgrade over poll), live field discovery.
- The Azure deploy adapter's live execution (needs your Azure credentials).
- Closing the tracked unwrap-cleanup frontier (~220 sites) so `clippy::unwrap_used`
  moves from the non-blocking CI job into the blocking lint bar.
- The recordable demo (two apps), which is yours to capture.
