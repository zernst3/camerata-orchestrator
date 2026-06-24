# Tech debt: migrate UI off ad-hoc fetch-and-store to a query layer (UI-QUERY-LIBRARY-1 → Option 1)

> **Status: STAGED — not yet filed as a GitHub issue.**
> On the next "GitHub push", create this as a **sub-issue of the Tech Debt Epic (#70)** using the title + body below.

**Title:** Migrate UI off ad-hoc fetch-and-store to a thin query layer (UI-QUERY-LIBRARY-1 → Option 1)

---

## Decision

Camerata adopts **`UI-QUERY-LIBRARY-1` → Option 1** ("all UI data fetches go through a query library") going forward, replacing the current **Option 2** (ad-hoc per-component `use_resource` + a fresh `reqwest::Client` at each call site). New UI data reads go through a shared query layer; existing call sites migrate over time (this issue).

## Why — the evidence (ad-hoc scan of `crates/ui/src`, 2026-06-24)

| Signal | Count | Meaning |
|---|---|---|
| `reqwest::Client::new()` | 76 | every call site builds its own client/pool — no shared infra |
| same endpoint, many readers | `projects/` ×16, `models` ×10, `active/context` ×5, `corpus-rules` ×5, `runs/` ×5 | shared data fetched independently — no de-dup, drifts out of sync |
| mutating verbs (post/put/delete/patch) | 75 | 75 sites must hand-invalidate/refetch after writes |
| polling loops | 49 | hand-rolled status polling instead of a refetch policy |
| `use_resource` fetch sites | 49 | the per-component fetch surface to migrate |

**The decisive evidence is the bug history**, not the counts. The chat-grounding "none yet" (stale read from the wrong source), scan results not propagating after completion (no invalidation on scan finish), and the rule-selection race are textbook Option-2 failure modes. A query layer's "one source of truth per key + invalidate-on-mutation" removes that entire class of bug by construction.

## Honest caveats (shape the approach)

- **Value is correctness/consistency, not scale.** Camerata is a single-user desktop cockpit — no thundering herd. Justify on fewer bugs + maintainability, not perf.
- **Dioxus's query ecosystem is immature** (the port's Z3 decision picked built-in `use_resource` for that reason). So Option 1 here is a **thin home-grown layer**, not a heavy third-party dep: one shared `reqwest::Client`, a keyed async cache with stale-while-revalidate, and invalidate-by-key on mutation. Fits the "build the primitive" style (chorale-adjacent).

## Approach (incremental — NOT big-bang)

1. **Free win:** collapse the 76 `reqwest::Client::new()` into a single shared client.
2. **Build the thin query layer** (shared client + keyed cache + SWR + invalidate-by-key).
3. **Migrate highest-fanout reads first:** `projects/active/context`, `projects`, `models`, `corpus-rules`, `runs` status, `development/context`, and the polled endpoints. One-off form submits can stay simple.
4. **Governance loop:** once Camerata's `UI-QUERY-LIBRARY-1` disposition is flipped to Option 1, Camerata's own scan flags the remaining ad-hoc sites as the retroactive worklist. (Best run after the AI re-scan — see #77 re-scan, deferred for token budget.)

## Scope

~49 `use_resource` read sites + ~75 mutation sites, migrated incrementally. Parent: **Tech Debt Epic #70**.
