# ADR: Adapter ladder (GAP-1), headless-core state lift (GAP-3), CLI HTTP adapter (GAP-7)

**Date:** 2026-07-08
**Status:** Accepted; BUILT (Batch 6 of the Fable 5 audit, one combined PR on
`feat/adapter-ladder-headless-core`).
**Deciders:** Zach (architect), Claude (architect)

Companion docs: [`2026-07-05_escalation-decisions.md`](../plans/2026-07-05_escalation-decisions.md)
(where GAP-1/3/7 were greenlit, Batch 6 of the implementation order), and
[`2026-07-01_ui-core-extraction.md`](../plans/2026-07-01_ui-core-extraction.md) (Phase 2 of the
headless-core UI extraction, which GAP-3 completes for the governed-dev surface).

## Context

The 2026-07-04 Fable 5 audit (`docs/ARCH_AUDIT_2026-07-04_fable5-complete.md`) escalated three
structural gaps, all under ROUTE-1 (they change public crate boundaries, so they route to Zach rather
than auto-applying):

- **GAP-1:** no machine-consumable capability contract. The BFF is the only surface an outside
  caller (an MCP client, a script) can drive, and it has no adapter beyond the Dioxus desktop cockpit
  itself embedding the server in-process. There is no adapter ladder, rung 1 does not exist.
- **GAP-3:** the governed-dev poll / change-detection state lived as three separate process-global
  Dioxus `GlobalSignal`s (`UOW_LAST_SEEN`, `UOW_CHANGED`, `PULLED_WORK_ITEMS`) inside `crates/ui`,
  with the poll and change-detection LOGIC written directly into the view. Not headless, not
  unit-testable without a `VirtualDom`, and a second copy of the same problem the 2026-07-01
  `ui-core` extraction (Phase 0/1) was already solving for the rest of the cockpit.
- **GAP-7:** `crates/cli` was an in-process demo harness (it linked every domain crate and called
  them directly), not an HTTP adapter. It proved nothing about whether the BFF's contract was
  actually drivable from outside the desktop process.

All three were greenlit on 2026-07-05 (see the escalation doc's GAP-1/GAP-3/GAP-7 entries), with
GAP-3 specifically called out to design the state model before building. This ADR records what was
built, as one combined PR rather than three separate branches: the phases are load-bearing
prerequisites of each other (the api-types leaf has to exist before anything can depend on it
without a cycle; the LlmPort rename has to land before the MCP/CLI adapters can be proven against a
stable contract), so splitting them across sequential PRs would have meant repeatedly re-reviewing
partial, non-buildable states.

## Decision

Build the adapter ladder from the leaf up: a pure-serde wire-contract crate first, then the two
things that were tangled with `crates/server` and blocking everything above them (the LLM/provider
stack and the UI's headless-core gap), then the actual adapters (a typed HTTP client, an MCP server,
a real CLI), then sever the desktop app's last in-process shortcut into the server.

### Phase A: `crates/api-types` (`camerata-api-types`), the pure-serde leaf

A new crate holding wire DTOs, with **zero** `camerata-*` dependencies (only `serde`, `serde_json`,
`chrono`, `thiserror`). This is the crate every adapter or consumer imports to speak the BFF's wire
format without pulling in the server, the domain logic, or the LLM stack. Two extractions fed it:

- The pure app-core DTOs (`uow`, `project`, `lifecycle`) moved DOWN one level, out of `app-core` and
  into `api-types`, since they were already free of any behavior.
- `LlmResponse` and the credential / model-registry wire shapes moved OUT of `crates/server`.

Every old call site keeps resolving via `pub use` re-export shims in `app-core` and `server`, so this
phase is a pure move with no consumer-facing break. 10 tests moved in with their types.

### Phase B: `crates/llm` (`camerata-llm`), and the `Completer` -> `LlmPort` rename

Relocated the entire provider stack out of `crates/server`: `rate_limit`, the `credentials` trait
plus its keyring implementation, `model_registry` behavior, `usage_ledger`, and the LLM core itself
(`LlmRequest`, the completion trait, `OpenRouterCompleter`, `build_completer`, the batch path).

The completion trait is renamed **`Completer` -> `LlmPort`** in the move. The rename is deliberate,
not cosmetic: `LlmPort` names it as a hexagonal PORT (the seam an adapter implements), matching the
same vocabulary the adapter ladder is built around. `crates/server` re-exports the relocated items
via shims, so existing `crate::llm::X` / `crate::credentials::X` call sites are unaffected.
`camerata-llm` depends only on `camerata-api-types` (`DEFAULT_MODEL` was already there from Phase A),
so there is no dependency on `app-core` and no cycle. 92 tests landed with the crate.

### Phase C (GAP-3): the headless-core state lift for governed-dev

New module `crates/ui-core/src/govdev.rs`: a TEA-style state machine, `GovDevState` (fields
`last_seen`, `changed`, `pulled`) driven by a `GovDevMsg` enum (`PollObserved`, `PulledLatest`,
`WorkItemsPulled`) through one pure reducer, `apply()`, plus read-only selectors
(`is_changed`, `changed_count`, `pulled_for`, `assignee_label`).

This collapses the three separate `GlobalSignal`s in `crates/ui` into ONE
`GlobalSignal<GovDevState>`. The poll / change-detection logic that used to live in the view moves
into the pure, unit-tested `apply()` transitions; the Dioxus adapter's job shrinks to translating
events into messages and rendering off the selectors. `WorkItem` itself moved to `api-types` so the
core can hold it without a framework dependency. `ui-core` stays renderer-free (no `dioxus` dep,
compiler-enforced), honoring `RUST-HEADLESS-CORE-1` and `RUST-PURE-STATE-TRANSITIONS-1`. This
completes Phase 2 of the 2026-07-01 `ui-core` extraction plan for the governed-dev surface.

### Phase D: `crates/client` (`camerata-client`), the typed HTTP client

A typed async client over the BFF's `/api/*` surface: `bff_base()` resolves the base URL from
`CAMERATA_BFF_URL` (default `http://127.0.0.1:8787`), `Client::new` / `Client::with_base` construct
it, and the verb set is `list_stories`, `get_run`, `list_uows`, `assign_work_item`, `start_run`, with
a typed `ClientError`. Faithful response DTOs (stories / run / uow-list / assign) were added to
`api-types` to back it. This is rung 1's shared plumbing: everything above it (MCP, the CLI) talks to
the BFF through this one client instead of hand-rolling HTTP. 7 tests, using `wiremock` against a
fake BFF.

### Phase E (GAP-1): `crates/mcp` (`camerata-mcp`), the first MCP adapter rung

An `rmcp` 1.7 MCP server over stdio, mirroring the same `rmcp` idiom `crates/gateway` already uses,
exposing 5 first-rung tools: `list_stories`, `get_run`, `list_uows`, `assign_work_item`,
`start_run`. Each tool delegates straight to `camerata-client`, no logic duplicated. This is GAP-1's
actual deliverable: an outside MCP client can now drive Camerata's read + a governed write verb
without being the desktop app. 7 tests plus a live stdio handshake smoke test.

### Phase F (GAP-7): `crates/cli` reworked into a real HTTP adapter

`crates/cli` stopped being an in-process demo harness and became an HTTP adapter over
`camerata-client`, adopting `clap` for argument parsing. All existing demo subcommands are
preserved; new HTTP verbs were added: `stories`, `run`, `uows`, `assign`, `start-run`, plus a global
`--bff-url` flag. 13 new tests. (`camerata-server` stays a `cli` dependency, but ONLY for the `eval`
subcommand, which runs the labeled-corpus harness that lives in `camerata_server::eval`; every
BFF-facing verb goes through the client, never an in-process call into the server.)

### Phase G (GAP-1 severance): the desktop app drops its embedded BFF

The last piece of GAP-1 was severing the cockpit's own shortcut: `crates/ui` no longer calls
`camerata_server::serve(BFF_ADDR)` in-process. A new `crates/ui/src/server_process.rs` spawns
`camerata-server` as a **subprocess** instead:

- Runtime binary resolution (`CAMERATA_SERVER_BIN` env var, then a binary sitting next to the
  running `camerata-ui` executable, then a `target/debug` dev fallback).
- Health-poll readiness against `/api/health` before the UI proceeds.
- Reuse-if-already-healthy (a standalone `camerata-server` left running on the port is adopted
  as-is) else spawn a fresh child, with `reclaim_port` takeover if a stale process is squatting the
  port.
- A detached watchdog kills the child process when the app exits. This exists because the
  workspace forbids `unsafe` and `atexit`-style hooks, so the watchdog is the sanctioned mechanism
  for "don't leak a BFF process after the desktop app closes."

The `camerata-server` crate dependency is DROPPED from `crates/ui/Cargo.toml` entirely; the cockpit
is now, structurally, exactly the kind of BFF client any other adapter is. Several UI mirror structs
were swapped to import from `camerata_api_types` directly instead of hand-duplicating the shape.
12 tests, including a real-subprocess integration test.

**Note on `crates/ui`'s dependency shape:** the cockpit talks to the BFF over raw `reqwest` calls
(`crates/ui/src/cockpit.rs` and friends), not through `camerata-client`. It does not (yet) depend on
`camerata-client`. Folding the cockpit onto the typed client is tracked as a follow-up below, not
claimed as done by this batch.

## The crate DAG (leaf to root, no cycles)

```
camerata-api-types                (pure serde leaf: serde, serde_json, chrono, thiserror; ZERO camerata deps)
        ^
        |-- camerata-llm          (LlmPort + provider stack; depends on api-types only)
        |-- camerata-app-core     (pure domain DTOs re-export from api-types; also depends on
        |                          worktracker, core, liveness, fleet, checks -- unrelated to this batch)
        |-- camerata-ui-core      (renderer-free; depends on api-types only; GovDevState lives here)
        |-- camerata-client       (typed HTTP client; depends on api-types only)

camerata-client
        ^
        |-- camerata-mcp          (MCP adapter; depends on client + api-types)
        |-- camerata-cli          (HTTP-adapter subcommands; depends on client + api-types;
                                    ALSO depends on camerata-server, but ONLY for `eval`)

camerata-server                   (depends on app-core + llm + api-types; re-exports the
                                    relocated DTOs/provider-stack via shims for old call sites)

camerata-ui                       (depends on ui-core + api-types + worktracker; talks to the BFF
                                    over raw reqwest, NOT camerata-client; spawns camerata-server
                                    as a SUBPROCESS -- no crate dependency on it at all)
```

Key invariant preserved throughout: `camerata-app-core` does **not** depend on `camerata-llm`, and
`camerata-llm` does not depend on `camerata-app-core`. Both sit side by side on top of
`camerata-api-types`, which is what let Phase A land ahead of Phase B without forcing a cycle.

## What was built (commits, in order)

1. `1e02803` Phase A: `camerata-api-types` extracted.
2. `9c88bd3` Phase B: `camerata-llm` extracted; `Completer` renamed to `LlmPort`.
3. `206a067` Phase C: `GovDevState` TEA model lands in `ui-core` (GAP-3).
4. `d699322` Phase D: `camerata-client` typed HTTP client.
5. `5a6c3c5` Phase E: `camerata-mcp`, the first MCP adapter rung (GAP-1).
6. `113195f` Phase F: `crates/cli` reworked into an HTTP adapter with clap (GAP-7).
7. `59a6981` Phase G: the desktop app spawns `camerata-server` as a subprocess, drops the
   in-process embed and the crate dependency (GAP-1 severance).

## Verification

Full workspace: **3348 tests pass, 0 fail**, across all crates including the new `api-types`,
`llm`, `client`, `mcp` crates and the `ui-core::govdev` module, plus the pre-existing suites
unaffected by the moves (re-export shims kept every old call site compiling and passing unchanged).

## Consequences

- There is now an actual adapter ladder above the BFF: a typed client (Phase D), an MCP server
  (Phase E), and a real HTTP CLI (Phase F), all built on the same `camerata-client` plumbing rather
  than three independent HTTP implementations.
- The desktop cockpit is no longer privileged: it drives the same BFF over the network that any
  other adapter would, and no longer forces every consumer to link `camerata-server`'s full
  dependency tree just to get a working UI.
- The governed-dev poll/change state is unit-tested without a `VirtualDom`, and the pattern (pure
  `State` + `Msg` + `apply()` in `ui-core`, thin Dioxus adapter) now has a second proof point beyond
  the original `ui-core` extraction's pure-function beachheads.
- `LlmPort` is the name every future provider adapter (and this ADR's own agentic-tool-schema
  follow-up) will be written against.

## Open follow-ups

- **`camerata-secrets` leaf crate.** `camerata-llm::credentials::CredentialStore` still has a
  `GITHUB_TOKEN` constant sitting alongside the LLM provider credentials, which is a smell: a
  GitHub token is not an LLM concern. Carving a `camerata-secrets` leaf out from under both `llm`
  and the VCS-facing crates is a candidate to remove the mixing, not done in this batch.
- **Remaining DTO migration.** `crates/server` and `crates/ui` still each carry some wire DTOs that
  duplicate shapes now available in `api-types` (this batch moved the ones already extracted;
  it did not do a full sweep). Migrating the rest is mechanical, compiler-verified follow-up.
- **Fold the agentic tool-schema path into `LlmPort`.** `crates/server/src/api_agent_driver.rs` has
  a standing `TODO(provider-agnostic-followup)`: the tool-schema / tool-result call path bypasses
  `LlmPort` today and posts the OpenRouter request body directly, because `LlmRequest` doesn't yet
  carry tool schemas. Extending `LlmRequest` to carry them so the agentic path goes through
  `LlmPort` like every bare-LLM call already does is the natural next step post-rename.
- **Packaging must ship `camerata-server` next to `camerata-ui`.** Phase G's binary-resolution
  order depends on finding a `camerata-server` binary next to the running `camerata-ui` executable
  as its primary non-dev path. Whatever packages the desktop app for distribution (installer,
  bundle, whatever ships GAP-1's severance to users) must place both binaries side by side, or the
  exe-sibling resolution falls through to the dev-only `target/debug` fallback, which does not
  exist outside a checked-out workspace.
