# OpenRouter Provider Safety + Selection — Design

**Date:** 2026-07-28 · **Status:** design, LOCKED (all forks decided), **not built** · **Branch:** feat/audit-report-export

## Context & rationale

Camerata's chatbot and audit models can run on OpenRouter models. When Camerata reads a **client's private repository**, prompts (including their proprietary code) are sent to whichever upstream provider OpenRouter routes to. For a commercial security-audit tool, the **data policy and hosting region of that provider is a trust guarantee, not a convenience**: some OpenRouter providers train on prompts and/or retain them (e.g. DeepSeek first-party, China-hosted); US-hosted providers like DeepInfra / Together / Fireworks do neither (see the 2026-07-28 provider research).

**This reverses the earlier (2026-07) decision to defer provider-selection to OpenRouter's account settings.** That deferral was correct when the ask was generic region-convenience (scope creep). This is a different feature: a **data-safety default that must be enforced in-product**, because it is part of what the contractor sells. New rationale = a trust guarantee for client-code handling.

## Principle: safe-by-default, SERVER-ENFORCED

The non-negotiable: safety is enforced at the **request-building layer**, never only in the UI. The UI toggle sets a persisted flag; the code that builds every OpenRouter request always reads it and injects the provider constraints. A UI bug therefore cannot leak a client's repo to a training/retaining provider. Disabling safety is an intentional, visible, **session-only** act.

## 1. Policy config

```
ProviderPolicy {
  safe_mode: bool,                  // DEFAULT true; enforced server-side every request
  pinned_provider: Option<String>,  // optional, e.g. "deepinfra"
}
```
Persisted with the OpenRouter settings. `safe_mode` defaults to `true` and, per §3, resets to `true` on every app start.

## 2. Request-layer enforcement (the trust core)

At the two OpenRouter request-body construction sites in `crates/llm` (the chat completer request `json!` body, ~`llm.rs:1521`, and the agentic driver, ~`api_agent_driver.rs:671` — confirm exact locations at build), inject OpenRouter's `provider` object from the policy:

- **safe_mode ON** → `data_collection: "deny"` (routes only to no-**retention** providers) **AND** restrict to providers whose live `training == false` flag holds — a **dynamic** allowlist computed from the `/endpoints` data (§6), not a hardcoded list and not the account-wide setting. Result: **no retention AND no training, both enforced per-request from OpenRouter's own live flags.**
- **pinned_provider set** → additionally `provider: { only: ["<pinned>"] }`. In safe mode a pin must itself pass the safe filter (you can only pin a safe provider while safe mode is on).
- **safe_mode OFF** → omit the constraints (allows cheap China-hosted providers for the operator's own QA of Camerata).

The **same** live per-provider `training` / `retainsPrompts` flags drive both the picker's safety badges (§5) and this enforcement — one source of truth, so badge and behavior can never disagree.

> Why not the account-wide "opt out of training" setting: it is fragile (relies on the operator having flipped a toggle in their OpenRouter dashboard, which Camerata cannot prove is on). The dynamic per-request restriction keeps the guarantee entirely inside Camerata.

## 3. Settings — testing-mode toggle

In the credentials panel (next to the OpenRouter key and the "Refresh models" button added in the registry-refresh fix): a **"Data safety"** section.
- Toggle: *"Safe mode — no retention, no training (recommended)"*, **default ON**.
- Turning it OFF pops a confirm dialog: *"This sends prompts to providers that may retain or train on them. Never use with client code."*
- **Testing mode is SESSION-SCOPED: it auto-resets to safe on app restart**, so it can never be silently left on across sessions.

## 4. Always-visible warning (safety OFF)

A persistent, high-contrast, **non-dismissible** banner in the **global app chrome** (the `desktop_chrome` layer), visible on every screen whenever `safe_mode` is OFF:

> ⚠ **TESTING MODE — data safety OFF.** Prompts may be retained or trained on. Do not use with client repositories.

## 5. Model + provider picker

Two-level selection, clean:
- **Model** dropdown unchanged (Claude group + OpenRouter group; grouping in `ui-core/src/models.rs`; pickers in `chat.rs` / `routines.rs` / `cockpit/scan.rs`).
- On selecting an OpenRouter model → a **Provider** sub-selector:
  - Default, named option at the top: **"Auto — cheapest safe"** (route among the safe providers, cheapest first).
  - Then the per-provider list from `GET /api/v1/models/<id>/endpoints`: each row shows **provider name · region flag · `$/1M in` / `$/1M out` · safety badge** (✓ no-train/no-retain, ⚠ retains/trains).
  - **In safe mode, unsafe providers render disabled/greyed** ("requires testing mode") — you literally cannot select a training/retaining provider for real work. In testing mode, all are selectable.

## 6. Data flow

Provider list + prices + policy flags come from a **lazy `/endpoints` fetch** when a model is selected (always fresh, no cache-invalidation), reusing the auth/plumbing from the model-registry refresh. Safe-mode enforcement (§2) reads the same flags.

## Resolved decisions (all four forks)

1. **Safe-mode mechanism:** `data_collection: "deny"` (retention) **+ dynamic restriction to `training == false` providers** from live flags. No hardcoded allowlist, no account-setting dependency. *(Upgrades the initial "account opt-out" idea to a fully in-product guarantee.)*
2. **Testing mode:** session-scoped, auto-resets to safe on restart.
3. **Picker:** full per-provider list with costs, plus an explicit **"Auto — cheapest safe"** option.
4. **Training enforcement:** per-request via the §2 dynamic non-training restriction, not the OpenRouter account setting.

## Build order + effort (~1.5–2 days when built)

1. `ProviderPolicy` config + persistence.
2. **Request-layer injection** in `crates/llm` (the trust core) + tests asserting safe mode ALWAYS injects the constraints and never emits a request that could route to a training/retaining provider. (Runtime routing can't be unit-tested headlessly → include a live smoke-test step.)
3. `/endpoints` fetch + caching (extend the registry refresh).
4. Provider picker UI (Auto-cheapest-safe + per-provider list with region/cost/badge; unsafe disabled in safe mode).
5. Settings toggle (session-scoped) + confirm dialog.
6. Global warning banner in the desktop chrome.

## Open items to verify at build

- Exact OpenRouter request-level `provider` semantics: confirm `data_collection: "deny"` behavior and whether `provider.only` composes with a training filter as intended (the 2026-07-28 research verified the flag semantics from OpenRouter's docs, **not** from a live test). **Smoke-test with a real request and confirm which provider actually served** before this handles client code.
- Whether a request-level *training* filter exists, or non-training enforcement must be via `provider.only` restricted to `training == false` providers (the assumed path in §2).
- Reference safe provider: **DeepInfra** (US, no-train, no-retain, cheapest fully-clean on both DeepSeek V4 Pro and V3.1) per the 2026-07-28 research.

## Reversal note

Supersedes the 2026-07 in-conversation decision to defer provider-selection to OpenRouter's account settings. New rationale: **data-safety-by-default is a product trust guarantee for handling client code**, not the generic region-convenience that was correctly deferred earlier.

## Pass 1 landed (2026-08-06)

Built exactly §1, §2, §6, and the enforcement half of §5's data. **No UI** (picker, settings toggle, warning banner) — that is Pass 2.

### What shipped

- **`ProviderPolicy`** — `crates/llm/src/provider_policy.rs`. `{ safe_mode: bool (default true), pinned_provider: Option<String> }`. `#[serde(default)]` on `safe_mode` so a settings file predating this feature (or `{}`) resolves to `safe_mode: true`, never panics, never silently unsafe.
  - **Persistence**: added to `crates/server/src/settings.rs`'s `Settings` struct as `provider_policy: ProviderPolicy` (`#[serde(default)]`), alongside the existing `chat_model` / `llm_backend` app-level settings — same JSON file (`settings.json` in the per-user data dir), same `SettingsStore::load_or_new` / `.save()` machinery. New accessors: `SettingsStore::provider_policy()` / `set_provider_policy(..)`, mirroring `chat_model()` / `set_chat_model(..)`. Session-reset-to-safe (§3) is NOT implemented here — that's a Pass-2 app-lifecycle concern; the setter just persists whatever it's given.
  - Server-side re-export shim at `crates/server/src/provider_policy.rs` (`pub use camerata_llm::provider_policy::*;`), following the existing `llm.rs` / `credentials.rs` / `model_registry.rs` shim pattern, so `crate::provider_policy::*` resolves unchanged everywhere in `camerata-server`.

- **Per-provider data-policy fetch + cache** — extended `crates/llm/src/model_registry.rs`. Two OpenRouter endpoints, joined:
  1. `GET /api/v1/models/<id>/endpoints` — per-model: which providers currently serve this model + per-model pricing. **Verified live 2026-08-06** against `deepseek/deepseek-chat` and `qwen/qwen3-coder` — does **NOT** carry any data-policy field. Shape: `{"data": {"id", "name", "endpoints": [{"provider_name", "tag", "pricing": {"prompt", "completion"}, ...}]}}`. The `tag` field (e.g. `"deepinfra/fp4"`, `"streamlake"`, `"google-vertex/us-south1"`) is the provider-variant slug; the segment before the first `/` is the provider slug used for the join and for the request-body `provider.only` array.
  2. `GET /api/frontend/v1/all-providers` — per-provider (not per-model): every provider's `dataPolicy` block. **This is the source the design doc flagged as "reconcile the sources" — found and verified live 2026-08-06.** It is an **undocumented-but-public frontend endpoint** (no `/docs/api-reference` page for it as of this writing; no API key required — confirmed via an unauthenticated `curl`). Shape: `{"data": [{"slug", "name", "headquarters", "dataPolicy": {"training": bool, "trainingOpenRouter": bool, "retainsPrompts": bool, "retentionDays"?: number, ...}}]}`. Field names confirmed exactly as documented in the task brief (`training`, `retainsPrompts`, `retentionDays`).
  - Join key: `tag.split('/').next()` (endpoint) against `slug` (all-providers) — NOT a display-name match (verified `"DeepInfra"` provider_name / `"deepinfra"` tag-prefix / `"deepinfra"` slug all line up; also verified the trickier `"Google"` provider_name / `"google-vertex/us-south1"` tag / `"google-vertex"` slug case, where a name-based join would have broken).
  - `ModelRegistry::safe_providers_for(model_id) -> SafeProviders` (sync, cache-only) and `ensure_safe_providers_loaded(api_key, model_id) -> SafeProviders` (async, lazy-fetch-on-miss) added. Fail-closed throughout: a `tag` that's missing, or whose slug isn't in the policy catalog (unknown provider, catalog fetch failed), is recorded as `training: true, retains_prompts: true` — i.e. excluded from the safe set, never defaulted safe.
  - Live-verified sample JSON (trimmed) for both endpoints is embedded directly in `model_registry.rs`'s test module (`SAMPLE_ENDPOINTS_JSON`, `SAMPLE_ALL_PROVIDERS_JSON`) and used to prove the parser doesn't panic on the real shape.

### The exact `provider` JSON emitted, per mode

All four cases live in `crates/llm/src/provider_policy.rs::provider_constraint_for_request` (the ONE function both OpenRouter request-body call sites go through) and are asserted at both the sub-object level (`provider_policy.rs` tests) and the full-request-body level (`llm.rs`'s `build_openrouter_chat_body` tests):

- **safe_mode ON, no pin** (the default): `{"data_collection": "deny", "only": ["deepinfra", "novita", ...]}` — `only` is exactly the live-computed set where `training == false && retains_prompts == false` for this model. No hardcoded list.
- **safe_mode ON, pin = safe provider**: `{"data_collection": "deny", "only": ["<pin>"]}` — narrows to exactly the pin.
- **safe_mode ON, pin = unsafe/unknown provider**: pin is DROPPED; `only` stays the full safe set — same shape as the no-pin case. Never widens, never includes the unsafe pin.
- **safe_mode OFF**: no `provider` key in the request body at all (verified at `chat_body_safe_mode_off_has_no_provider_key_at_all`).

### Fail-closed behavior (the degenerate case)

When `safe_mode` is ON but the safe-provider set can't be determined — either `SafeProviders::Unknown` (endpoints never fetched / fetch failed) or `SafeProviders::Known(vec![])` (fetched, but genuinely zero providers for this model pass the bar) — **`provider_constraint_for_request` returns `Err`, and both call sites propagate that as a hard error BEFORE building the request body or touching the HTTP client.** No HTTP request is sent at all in this case; this is stronger than the design doc's stated floor ("at minimum send `data_collection: deny`") — Pass 1 blocks entirely rather than sending a retention-only-safe/training-unsafe request, because an `only: []` array was judged too fragile a "block everything" signal to rely on at the OpenRouter API level (some provider-routing implementations treat an empty filter as "no filter" — never verified live, deliberately not risked). This is proven end-to-end without any network call via `llm.rs::complete_fails_closed_with_zero_safe_providers_no_network_call`, which seeds the registry's cache directly (`ModelRegistry::seed_provider_endpoints`, a test-only seam) with zero safe providers and asserts `OpenRouterCompleter::complete` errors before any HTTP client is constructed.

### The core invariant test

`crates/llm/src/provider_policy.rs::only_list_is_always_a_subset_of_the_safe_set` — property-style across six safe-set/pin combinations (including an unsafe pin and a bogus pin), asserting the emitted `only` array is always a subset of the input safe set. Paired with `training_or_retaining_provider_never_appears_in_only` and, at the join layer, `model_registry.rs::training_provider_excluded_from_safe_set` / `retaining_only_provider_excluded_from_safe_set` (using DeepSeek's live-verified `training: true` record and StreamLake's live-verified `retainsPrompts: true, training: false` record respectively — proving the bar is `training == false AND retains_prompts == false`, not just one flag).

### How a training/retaining provider is proven excluded

Three layers, each with dedicated tests:
1. **Join layer** (`model_registry.rs`): `join_endpoints_with_policy` + `compute_safe_providers`, using the live-captured DeepSeek (`training: true`) and StreamLake (`retainsPrompts: true`) records — both excluded from the computed safe set (`training_provider_excluded_from_safe_set`, `retaining_only_provider_excluded_from_safe_set`).
2. **Constraint-builder layer** (`provider_policy.rs`): given a safe set that already excludes the unsafe providers, the builder never reintroduces them, INCLUDING when they're pinned (`training_or_retaining_provider_never_appears_in_only`, `unsafe_pin_is_dropped_request_stays_restricted_to_safe_set`).
3. **Full-request-body layer** (`llm.rs`): `chat_body_pinned_unsafe_provider_is_dropped_stays_restricted_to_safe_set` — the exact scenario the task calls out by name, asserted on the actual JSON body that would be posted.
4. **Fail-closed layer**: unknown/malformed provider data resolves to UNSAFE, never safe (`missing_tag_and_unknown_slug_are_excluded_not_panicking`, `empty_policy_catalog_makes_every_endpoint_unsafe`).

### Test counts

- `camerata-llm`: 128 passed, 1 ignored (`complete_fails_closed_when_safe_set_never_loaded_live_smoke_test`, marked `#[ignore]` — hits the live OpenRouter `/endpoints` API, run manually not in CI).
- `camerata-server`: 1251 lib tests + all integration test binaries green (`model_selection_e2e.rs` 29/29, plus every other suite unaffected).
- `cargo check --workspace --all-targets`: clean (only pre-existing, unrelated dead-code warnings in `camerata-ui` / `camerata-gateway`).

### The live smoke-test the operator must run

Pass 1 could not verify (no live OpenRouter API key available in this session):
- That `data_collection: "deny"` + `provider.only` actually compose the way OpenRouter's docs describe when sent together in one request (both were verified independently against docs/behavior description, not a live joint request).
- **Which provider actually serves a real request** when `only` is set to Camerata's computed safe list — i.e., that OpenRouter's routing genuinely never falls through to a provider outside `only`.

**Manual smoke test** (run with a real `OPENROUTER_API_KEY` before this handles client-repository content): call a cheap OpenRouter model (e.g. `deepseek/deepseek-chat`) through the normal chat path with safe_mode ON, capture the response, and cross-check the serving provider — either via the response's provider-identifying fields or via `https://openrouter.ai/activity` in the account dashboard — against the `only` list Camerata computed for that request (log it, or breakpoint `provider_constraint_for_request`'s return value). Confirm the served provider is IN the list. The `crates/llm/src/llm.rs::complete_fails_closed_when_safe_set_never_loaded_live_smoke_test` test (marked `#[ignore]`) exercises the adjacent fail-closed-on-first-call path live; there is no automated test for "OpenRouter honors `only`" since that requires asserting on OpenRouter's own routing behavior, not Camerata's.

### What Pass 2 (UI) builds on

- `ModelRegistry::safe_providers_for` / `provider_endpoints_for` (the richer, full-record sibling — pricing + region + training/retention per provider) are ALREADY the data source for the picker's per-provider list + "Auto — cheapest safe" option + safety badges (§5). No new data plumbing needed.
- `SettingsStore::provider_policy()` / `set_provider_policy(..)` are the read/write seam for the settings-panel toggle + pin picker (§3). Pass 2 adds the confirm-dialog UX and the session-scoped auto-reset-to-safe-on-restart behavior (currently NOT implemented — `set_provider_policy` persists whatever it's given, with no lifecycle hook).
- The global warning banner (§4) needs a way to read "is safe_mode currently off" — `SettingsStore::provider_policy().safe_mode` — no new backend surface required, just an endpoint/websocket to expose it to the `desktop_chrome` layer.
- `ProviderEndpointInfo::region` (from `all-providers`' `headquarters` field) is captured but unused by Pass 1 logic — ready for the picker's region-flag column.
