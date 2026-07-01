# Backend headless-core extraction (#117)

Backend twin of #116. Applies `RUST-HEADLESS-CORE-1` (structure) + `RUST-PURE-STATE-TRANSITIONS-1`
(state in → state out, side effects at the edges) to the server side.

**Question answered:** is the core stateless, with state on the adapter? **Yes, mostly** — but the
transition logic isn't yet promoted out of the Axum crate, and it's interleaved with the stores.

## Verified current state (2026-07-01)

The 8 app-orchestration modules live in `crates/server/src/`, all **zero axum coupling**:

| module        | ~lines | owns store?                         | reaches outside the move set |
|---------------|-------:|-------------------------------------|------------------------------|
| `uow.rs`      | 3904   | yes — `UowStore` (Arc<Mutex>, L601) | `crate::lifecycle` (in set)  |
| `project.rs`  | 1848   | yes (1 Arc<Mutex>)                  | **`crate::llm`**, `model_tier` |
| `routine.rs`  | 1221   | yes (1)                             | —                            |
| `run.rs`      |  966   | yes (3)                             | **`crate::transcript`** (`TranscriptStore`) |
| `escalation.rs`| 968   | yes (1)                             | `crate::routine` (in set)    |
| `lifecycle.rs`|  582   | no                                  | —                            |
| `schedule.rs` |  427   | no (pure: `is_due`/`next_fire`)     | —                            |
| `checkpoint.rs`|  225   | yes (2)                             | —                            |

Total ~11.1k lines. None import a concrete store from *elsewhere* except via the entanglements
below — they define their **own** stores inline. Stores are reached by handlers as params, not
owned by the core, which is why the issue calls the backend "built right."

## The real shape of the work: a per-module SPLIT, not a bulk move

Each module interleaves three things in one file:
1. **Domain types** (e.g. `UnitOfWork`) — move to the core.
2. **Pure transition fns / methods** (state in → state out) — move to the core.
3. **The store** (e.g. `UowStore { mem: Arc<Mutex<…>> }`) + persistence — **stay in the adapter**.

So the extraction is: split each of the 8 files, promote (1)+(2), leave (3) in `crates/server`.
`schedule.rs` and `lifecycle.rs` are already pure (trivial). The rest need the split.

## Blocking decisions (ROUTE-1 — Zach's call)

### D1 — Crate boundary: new `camerata-app-core` vs. fold into `camerata-core`
- **New `camerata-app-core`** (recommended): keeps `camerata-core` as the pure governance/domain
  kernel and puts app-orchestration state-machines in a sibling. Clean layering, mirrors
  `camerata-ui-core`. Cost: one new crate + wiring.
- **Fold into `camerata-core`**: fewer crates; risks bloating the kernel with app-flow concerns
  and mixing two abstraction levels.

### D2 — The `project → llm` entanglement
`llm.rs` is **2340 lines, 42 axum refs, depends on `credentials`** — a firm *adapter* concern that
cannot move to a headless core. `project.rs` touches it. Options:
- **(a)** Inject llm behind a trait the adapter implements (`project` core takes `&dyn LlmPort`).
  Cleanest; matches the state-as-param pattern already used for stores.
- **(b)** Keep `project.rs`'s llm-touching functions in the adapter; move only the pure parts.
- **(c)** Exclude `project` from phase 1; ship the other 7 first.

### D3 — `run → transcript` and `project → model_tier`
- `transcript.rs` (190 lines, 0 axum, stateless-ish + `TranscriptStore`) — small; move its domain
  types with the set, leave `TranscriptStore` in the adapter (same split rule). Low risk.
- `model_tier.rs` (148 lines, 0 axum, depends on `crate::project`) — circular with project; moves
  with the set.

## Proposed phasing (once D1–D2 are decided)

1. Stand up the target crate (per D1). Move the two already-pure modules first: `schedule`,
   `lifecycle` (+ their transition tests) — proves the wiring with near-zero risk. (Mirrors the
   #116 "beachhead" cadence.)
2. Split + move the clean stateful ones: `checkpoint`, `routine`, `escalation`, `uow` — domain
   types + transitions to core, `*Store` stays in adapter, re-export to keep handler call sites
   unchanged.
3. Handle `run` (resolve `transcript` split) and `project` (resolve llm per D2) last.
4. Leave `crates/server` a thin Axum adapter over the core. Coverage-preservation rule from #116
   applies: transition tests move with the logic, 1:1.

## Non-goals
- No behavior change, no store relocation to a DB, no transport change. Pure structural promotion.
- The `gateway` + `fleet` crates already prove standalone reuse; not touched here.
