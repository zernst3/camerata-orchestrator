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
