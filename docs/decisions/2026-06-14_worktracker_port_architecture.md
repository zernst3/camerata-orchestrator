# The WorkItemProvider port: one seam, canonical spine, per-field sync, two guards

Date: 2026-06-14
Status: Accepted (native + Jira + ADO + GitHub adapters built; live execution pending)
Deciders: Zach (architect), Claude (architect)

## Context

Tier 1 (enterprise governed orchestration) needs a real Product Owner to participate
through whatever tracker their org already uses (Jira, Azure DevOps Boards, GitHub),
without core orchestration knowing or caring which. The full design is in
`docs/WORKTRACKER_INTEGRATION.md` (researched against live provider docs); this record
captures the load-bearing decisions and why.

## Decisions

1. **One port, core depends only on it.** A single `WorkItemProvider` trait
   (`crates/worktracker`) is the seam. Core never imports `jira` / `azure-devops` /
   `github` / `native`. Each adapter maps to and from the canonical shapes and
   implements the trait. Per-provider auth, field mapping, and rate-limit handling
   live inside the adapter. Rejected: per-provider bespoke integrations (re-implements
   intake/status/loop-avoidance N times) and adopting a provider's schema as canonical
   (no provider can model provenance / gate results / sign-off).

2. **Our Story spine is always canonical; the tracker is a mirror, configurable per
   field.** A `SyncPolicy` sets the authoritative side per field (`Ours` | `Tracker`).
   Greenfield/native: all `Ours`. Brownfield: intake fields (title, description,
   status) flip to `Tracker` so the org's board stays process-of-record, while
   provenance, gate results, PR links, and sign-off are ALWAYS `Ours` and never
   configurable. A per-field direction (not a global flag) is the structural
   loop-breaker: a field both sides could edit is impossible by construction.

3. **Two independent loop-avoidance guards.** Guard 1: per-field direction
   (`apply_inbound` only writes tracker-authoritative fields). Guard 2: echo
   suppression via an expected-revision table plus delivery-id dedup
   (`ExpectedEchoTable`, `classify_inbound` returning Duplicate / Echo / Fresh). They
   cover different failure modes (both-sides-own-the-field vs our-own-write-bounces-back),
   so both are needed.

4. **Map to the STABLE category abstraction, never user-renamed names.** Jira
   `statusCategory` (new / indeterminate / done), ADO `stateCategory` (Proposed /
   InProgress / Resolved / Completed / Removed), GitHub labels + open/closed (no fixed
   category). Unknown values route to a safe default rather than throwing. Illegal
   transitions degrade to a comment, never a forced state write.

5. **Build order: native first, then two independent axes.** Native (Phase A) forces
   the canonical shapes correct before any auth/webhook mess. Then the BOARD axis
   (where the PO lives: Jira + ADO Boards, GitHub Issues deprioritized) and the
   CODE-HOST axis (PR + gate writeback: GitHub first) are chosen independently per
   deployment. All four adapters now exist behind the one port; live execution
   (per-provider auth, webhook ingress) is the remaining work.

6. **Inbound default is POLL; webhook is an opt-in upgrade.** The V1 local tool has no
   public ingress, so reconciliation polling (Jira JQL, ADO WIQL, GitHub `since=`) is
   the operating default; a tunnel/relay unlocks webhooks for lower latency later.

## Why all-Rust (the TS design is superseded)

`WORKTRACKER_INTEGRATION.md` specifies the shapes in TypeScript (the pre-pivot
design). The implementation is all-Rust behind the same contract; the design
reasoning holds, only the language moved (consistent with `RUST_CORE_VERIFICATION.md`).

## Consequences

- The async clarify-bridge (post the lead engineer's questions, poll the PO's answer)
  rides the same port and is provider-agnostic.
- Live HTTP sits behind an injectable transport seam, so adapter logic is tested with
  a fake client and the live `reqwest` transport is thin and type-checked.
- The privilege boundary is structural: the PO can answer and sign off via the
  tracker; they can never trigger execution. That is why Tier 1 needs no central
  OAuth, no multi-tenant database, and no hosted compute.
