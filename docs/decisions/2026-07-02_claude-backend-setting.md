# ADR: In-app Claude backend setting (CLI vs API) with keychain-backed Anthropic key

**Date:** 2026-07-02
**Status:** Accepted (shipped)
**Refines:** `2026-06-27` provider-agnostic agent runtime

## Context

Camerata's 2026-06-27 provider-agnostic runtime ADR established two distinct execution
paths for Claude:

1. **CLI path** (`ClaudeCliDriver`) — spawns the `claude -p` subprocess; uses the
   operator's logged-in Claude Code subscription; no API key required.
2. **API path** (`ApiAgentDriver`) — calls the Anthropic Messages API directly; requires
   `ANTHROPIC_API_KEY`; the provider-agnostic target architecture.

The toggle between these paths was previously an env-only knob (`CAMERATA_LLM_BACKEND`),
meaning the user had to edit a `.env` file and restart. That is an ops burden for a
setting that changes how every AI action in the app behaves.

A second friction point: `ANTHROPIC_API_KEY` itself was env-only, so even users who
wanted the API path had to drop out of the app to configure it. The OpenRouter key
already had an in-app keychain-backed credential row; there was no reason the Anthropic
key should not.

## Decision

### 1. Persisted setting: `llm_backend`

`Settings` gains a `llm_backend: Option<String>` field (serialized as `"cli"` or `"api"`;
absent means "not chosen here"). The setter validates the value and clamps anything
outside `{"cli", "api"}` to `None`, so invalid values cannot persist.

Effective precedence (highest to lowest): stored setting > `CAMERATA_LLM_BACKEND` env
var > default `"cli"`.

### 2. Startup hydration bridge

After settings load in `lib.rs`, the stored backend is written into the env var
(`std::env::set_var("CAMERATA_LLM_BACKEND", b)`). The two existing backend-selection
sites (`Llm::from_env` and `api_agent_driver::anthropic_api_backend_key`) continue to
read the env var unchanged. The hydration bridge is the only change to the boot path; all
downstream logic is untouched.

`POST /api/settings/llm-backend` validates (400 on anything other than `"cli"` or `"api"`),
persists, and calls `set_var` so the change takes effect immediately for subsequent requests
with no restart.

### 3. Anthropic key in the keychain

`anthropic_api_key` is added to `ALL_CREDENTIALS` in `crates/server/src/credentials.rs`,
making it a first-class keychain credential alongside `openrouter_api_key` and
`github_token`. On startup (in `from_env`) and on save (in `set_credential`, scoped to
this key only), the keychain value is hydrated into `ANTHROPIC_API_KEY`. The env var
remains supported as a back-compat fallback; the keychain wins when both are present.

`anthropic_api_key_present` returns `true` when the key is present in EITHER the keychain
or the env var; the credential store is threaded through its two call sites.

### 4. UI: Claude backend toggle

The Settings panel gains a `ModelBackendSettings` component with a `CLI / API` segmented
control labeled "Claude backend" (not "Model backend" — this toggle is specifically about
Claude's two access paths, not provider choice; provider-agnostic model selection is the
separate 2026-06-27 runtime). The label names Claude and Anthropic explicitly so the
distinction is clear.

When `API` is selected:
- An `anthropic_api_key` `CredentialRow` is revealed inline, reusing the OpenRouter key UX.
- If no key is present the server falls back to CLI silently; an inline warning is shown
  until a key is saved. The warning-visibility logic is pure (in `camerata_ui_core::llm_backend`)
  so it is unit-testable without a Dioxus runtime.

On credential save the component re-fetches both the credentials list and the backend
settings so the key badge and the warning both update in the same render.

## What this is NOT

This toggle is not provider selection. The 2026-06-27 runtime ADR describes the
`AgentDriver` seam that eventually allows any API-speaking provider to back a tier. The
CLI/API toggle here selects between Claude's two access modes specifically: the local
subprocess (subscription) vs the direct HTTP API (key). Provider choice at the driver
level is the next layer.

## Consequences

- The backend is configurable without touching `.env` or restarting the server.
- The Anthropic key is stored once in the OS keychain, like other credentials, and
  propagated automatically.
- No existing env-driven setups break: the env var still works; the stored setting simply
  takes precedence.
- Pure logic in `camerata-ui-core` (the `LlmBackend` enum, `show_api_key_warning`) keeps
  the toggle's decision logic unit-testable with no Dioxus dependency.

## Files touched

- `crates/server/src/settings.rs` — `llm_backend` field + getter/setter + validation
- `crates/server/src/lib.rs` — startup hydration + `get_settings` endpoint + `POST /api/settings/llm-backend`
- `crates/server/src/credentials.rs` — `ANTHROPIC_API_KEY` const + `ALL_CREDENTIALS` entry
- `crates/ui-core/src/llm_backend.rs` — `LlmBackend` enum + `show_api_key_warning` pure logic
- `crates/ui/src/credentials.rs` — `ModelBackendSettings` component + `CredentialRow` reveal
