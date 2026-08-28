# Backend-safety gate + live `/v1/models` — design

Date: 2026-08-27
Status: approved (owner-directed), building
Branch: `feat/audit-report-export`

Two owner-directed changes that harden the CLI-vs-API backend choice for **client
work** and make the API model list live.

## Motivation

The CLI-vs-API backend is an **auth/billing/compliance axis, not a model-quality
axis**. Same model either way; what differs is the *chain*:

- **CLI** = the operator's personal Claude Code subscription (personal account,
  consumer terms, personal usage limit). Not a chain you can name in a client
  contract.
- **API** = commercial, metered, single-party, no-train / ZDR-eligible.

For client work, *every* call that ships the client's private code (audit, the
gov-dev write loop, chat about their code) must ride the API chain. Today
`select_backend(Some("api"), false)` **silently falls back to CLI** when the key
is missing — a silent compliance downgrade: you pick API, forget the key, and the
client's code quietly flows through your personal subscription. Two fixes:

1. **Never silent.** A fallback to CLI must raise a loud, visible warning; an
   API-required project with no key must **hard-block**, not downgrade.
2. **Per-project `cli_active`, default OFF.** A project may only touch the CLI
   transport when this is explicitly ON. OFF (the default) means API-only —
   missing key ⇒ fail. This is the second, independent guard: the project flag AND
   the key-presence check must both pass before a client scan runs.

## Feature B — the backend-safety gate

### Resolver (in `crates/llm/src/llm.rs`)

Replace the `Backend`-returning `select_backend` with a richer resolution that can
express *warn* and *block*, leaving `select_backend` as a thin wrapper for the
existing env path (back-compat) or superseding it:

```rust
pub enum BackendResolution {
    Api,                       // API selected, key present — good
    Cli,                       // CLI, intended (personal use) — quiet
    CliFallbackWarn { message },  // wanted API, no key, but cli_active ON — USE CLI, LOUD warn
    Blocked { message },       // wanted API, no key, cli_active OFF — FAIL, do not run
}

pub fn resolve_backend(app_backend: &str, has_api_key: bool, cli_active: bool) -> BackendResolution
```

Semantics (the crux — `cli_active` is the master switch for whether CLI may be
used **at all** for this project):

| `cli_active` | `app_backend` | key? | resolution |
|---|---|---|---|
| **false** (default) | any | yes | **Api** |
| **false** (default) | any | no  | **Blocked** — "This project is API-only (CLI disabled). Add an Anthropic API key to run." |
| true | `api` | yes | **Api** |
| true | `api` | no  | **CliFallbackWarn** — "Anthropic API key missing — falling back to the Claude CLI (your personal subscription). Do not use for client code." |
| true | `cli` | –   | **Cli** (quiet, intended) |

Key point: `cli_active == false` **forces API-only regardless of `app_backend`** —
even an explicit global `cli` is refused for that project. That is what "double the
checks it is using the API and not the CLI" means: the client project can never use
CLI, not as a fallback and not as an explicit choice.

### Enforcement seams (both must go through the resolver)

- **Audit** — `crates/server/src/onboard.rs` (`Llm::from_env()` at ~678). Resolve
  with the project's `cli_active`; on `Blocked`, abort the scan with the message
  (surfaced to the UI) before any model call; on `CliFallbackWarn`, proceed on CLI
  but attach the warning to the run so the UI shows it.
- **Gov-dev agent** — `crates/server/src/api_agent_driver.rs` (`anthropic_api_backend_key` / driver selection at ~2004). Same resolution; `Blocked` refuses to build the driver / start the run.

### Persistence + API

- `crates/app-core/src/project.rs` `Project`: add `#[serde(default)] pub cli_active: bool` (defaults `false` on load — existing projects become API-only, see Migration).
- `crates/server/src/project.rs` `ProjectStore`: `set_cli_active(project_id, bool)`.
- New endpoint `POST /api/projects/{id}/cli-active { active: bool }` (mirror `set_step_model` shape).

### UI (`crates/ui`)

- Per-project **"Allow Claude CLI (personal subscription)"** toggle, default OFF,
  on the project settings surface. Copy states plainly: OFF = API-only (client-safe);
  ON = permits the personal-subscription CLI (not for client code).
- **Fallback warning**: when a run resolves to `CliFallbackWarn`, a visible,
  non-dismissable-until-acknowledged banner ("running on your personal Claude
  subscription — not a client-safe chain").
- **Block**: when a scan can't start because it resolved to `Blocked`, a clear
  inline error ("API-only project, no Anthropic key") with a link to the key field.

### Migration consequence (flag to owner)

`cli_active` serde-defaults to `false`, so **every existing project loads as
API-only**. If the operator has been running scans on the CLI default with no key,
those scans will now **block** until they either add an Anthropic key or flip
`cli_active` ON for that (personal) project. This is the intended secure-by-default
posture, but it is a live behavior change on existing personal repos — call it out
in the ship note.

## Feature A — live Anthropic `/v1/models`

Today Claude models are **hardcoded** in `crates/llm/src/model_registry.rs`
(`CLAUDE_REGISTRY_MODELS`); OpenRouter already fetches its list live
(`/api/v1/models`, ~line 501). Mirror that for Anthropic:

- When the **API backend is active with a key**, `GET https://api.anthropic.com/v1/models`
  (headers `x-api-key`, `anthropic-version: 2023-06-01`) and use the returned
  `data[].id` / `display_name` as the selectable model list.
- **Pricing caveat**: `/v1/models` returns id + display name + created, **not
  pricing**. Join the hardcoded registry's price map by model-id for the cost
  estimate; fall back to "price unknown" for any id not in the map (still
  selectable, estimate shows unknown).
- **CLI backend keeps the hardcoded registry list** (the CLI/subscription model set
  is not the API `/v1/models` set). So the picker's available models become a
  function of the active backend — the clean answer to per-backend model display.
- This resolves the **Fable-5 availability** question definitively: `claude-fable-5`
  appears in the API picker iff the account/org is actually served it.
- Cache the fetched list (like OpenRouter's registry refresh: on startup + on
  key-save + a manual refresh button); fail soft to the hardcoded list on any
  fetch error so the picker is never empty.
