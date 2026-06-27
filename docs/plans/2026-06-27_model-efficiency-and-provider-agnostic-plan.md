# Model efficiency, discovery, profiles & provider-agnostic runtime — design plan

> **Status: DESIGN (2026-06-27). Plan only; build on go.** Builds on
> `docs/decisions/2026-06-27_provider-agnostic-agent-runtime.md`.

## Goals (from Zach)

1. **Subscription "bucket relief"** — offload work *off* the Claude subscription onto free models
   to conserve quota, targeting the heaviest buckets: **development + routines**. (Value is quota
   relief, not dollars — dev runs on the subscription via the CLI, not per-token billing.)
2. **Maximize prompt-caching efficiency** on the Claude CLI/subscription.
3. A **project-level "model efficiency" setting** that cascades sensible defaults to **every**
   model entry point (auto-saved), with **per-entry override** preserved.
4. **No hard-coded model lists** — except Claude (the CLI has no list-models API, so a curated
   static list); **OpenRouter models discovered via API**, flagged free / tool-use.
5. **Dev tiering: low/mid tiers become model CHAINS** (multiple models, fallback).
6. **Provider agnosticism** throughout.

## 1. Model registry (discovery, not hard-coding)

`ModelInfo { id, provider, display, free, tool_use, context, coding, weight }`. Sources:

- **Claude (subscription / CLI):** a **curated static list** (no list-models API exists for the
  CLI) — `opus-4-8`, `sonnet-4-6`, `haiku-4-5`, each with a relative **weight** (Opus heavy →
  Sonnet mid → Haiku light = relative subscription-quota cost). This is *data* (one const/file),
  not UI-hardcoded; trivial to update.
- **OpenRouter (API):** `GET /api/v1/models` → parse pricing (**free** when prompt+completion
  price = 0), `supported_parameters` (**tool_use** = contains `tools`), `context_length`, and a
  coding-suitability heuristic. Cache the list; refresh on demand.

Model selectors populate **from the registry**, grouped by provider, with badges (FREE · tool-use
✓/✗ · context). Adding a provider = adding a registry source. Requires an OpenRouter key/base-URL
in settings to call the models API.

## 1a. Credentials manager (keychain-backed, UI-set)

App-level credentials, **set from the UI** and stored in the **OS keychain** (macOS Keychain via
the Rust `keyring` crate; Credential Manager / Secret Service on Win/Linux) — **never** in an env
file or app config. Holds at least:
- **OpenRouter API key** (model discovery + the API driver).
- **GitHub token** (PAT — issues / PRs / push). **Unify whatever GitHub auth exists today into
  this manager** so it's set the same way.
- Extensible (future: Anthropic API key, etc.).

Behavior: enter once → backend writes to keychain → backend reads it only at call time → the UI is
**never** sent the full key back, only a **masked** form (first 4 chars + `••••`). Fits Camerata's
posture: the subscription path stores nothing; these are deliberate, user-provided credentials in
the OS vault, never in the repo/app files. **On next launch the user sets both keys (OpenRouter +
GitHub) from one Settings → Credentials area.**

## 2. Model entry points the cascade covers

Every place a model is chosen today:
- **Tier-map:** `strongest` / `balanced` / `fast` (balanced + fast become **chains**, §4).
- **Per-step** (`StepModels`): audit, calibration, story-author, decompose, clarification, chat.
- **L3 reviewer** model (+ enabled).
- *(future)* routine defaults.

## 3. Project "Model Efficiency Profile" + cascade

A project setting `model_profile: MaxEfficiency | Balanced | MaxQuality | Custom`. **"Apply
profile"** computes concrete assignments for **all** entry points (§2) from the profile + the
registry, **writes them to project settings, and auto-saves.**

Concrete profiles:
- **Max efficiency (quota relief):** `strongest`=Opus (subscription CLI); `balanced`=[top free
  coder e.g. `qwen3-coder:free` → Sonnet]; `fast`=[small free → Haiku]; bare-LLM steps=free/Haiku;
  L3=free (or off); routines=free-first. Opus orchestrates; everything offloadable goes free with
  paid fallback.
- **Balanced (subscription-leaning, reliable):** `strongest`=Opus; `balanced`=Sonnet; `fast`=Haiku;
  bare-LLM=Haiku; L3=Sonnet/off; free models off or overflow-only.
- **Max quality:** Opus/Sonnet throughout; no free models.
- **Custom:** never overrides — the user owns every entry.

**Override model:** applying a profile **overwrites all entries** via a **confirm popup that
previews the per-entry `current → new` changes** (plus a count of entries affected) — no blind
overwrite. After apply, any single entry can be overridden manually and sticks until the next apply. (Simple + predictable;
optional refinement: tag each entry profile-default vs user-set so re-apply doesn't clobber
deliberate overrides — listed as an open choice.) Persistence reuses the existing project-settings
+ tier/L3 endpoints — auto-saved on apply and on any override.

## 4. Tier model chains (low + mid)

`fast` and `balanced` become **ordered chains** (`Vec<ModelRef>`, primary → fallbacks);
`strongest` stays single (orchestrator reliability). Runtime fallback at the **request level**:
try primary → on **429 / provider error / malformed-tool-call-after-one-retry** → next in chain.
This captures the free-tier steady slice and **never breaks on burst/throttle** (it falls back to
paid). Profiles set the chains (free-first for Max efficiency).

**Fallback policy:** fall back to the next model on **429** (rate limit), **402** (quota), **5xx**
(provider down), **timeouts / network errors**, and a **malformed tool-call after one retry**. Be
**selective on 400** — fall back only on *capability* 400s (tool-use unsupported, context-length
exceeded → a capable/bigger model), **not** generic bad-request (which would fail on every model
and burn the chain — surface it instead). **Never** fall back on **401/403** (auth — surface
immediately). Cap chain length; if all fail, surface/escalate (`INCOMPLETE` → orchestrator).

## 5. Prompt-caching efficiency (subscription)

Claude Code auto-caches; our job is to keep the cacheable **prefix stable + warm**:
- Generate rules + repo-grounding digest + system prompt **once per UoW/session** and reuse the
  **identical text** for every agent spawn (regenerating per call busts the cache).
- Order prompts **stable-prefix-first** (system + rules + grounding), variable task/diff last.
- Cluster related agent calls back-to-back (fan-out + sequential delegate already do) to stay in
  the cache TTL window.

Small refactor for prefix stability + ordering; no new model. The biggest subscription stretch
alongside tiering.

**Caching is per-model** (each model keeps its own cached prefix; it does **not** cross models or
providers — there's no shared cache between Opus/Sonnet/Haiku, let alone across providers). So the
discipline pays off on whatever **stays on the subscription** (the orchestrator + any retained
Claude tiers); free-model offloaded work generally won't cache — but it's **free anyway**, so
that's not a loss. **Choosing Max efficiency over Balanced trades reliability, not caching value.**

## 6. Provider-agnostic runtime (per the ADR) — the key nuance

- **`Completer`-direct** impls: Anthropic API + OpenRouter API → bare-LLM calls (immediately
  provider-agnostic).
- **Per-tier DRIVER chosen by provider:**
  - Claude tiers → **`ClaudeCliDriver` (subscription, no dollars)** — KEEP it; it *is* the cost
    lever.
  - OpenRouter / other → **native `ApiAgentDriver`** (owns the MCP tool-use loop) — new,
    provider-neutral.
- The fleet picks the driver from the model's provider, so **subscription-Claude and
  free-OpenRouter coexist**; the profile routes each tier to its provider/driver. Layer-1
  invariants unchanged (the driver only changes *who runs the loop*).

**Build it right up front (Zach, 2026-06-27):** **no CLI-as-proxy interim** — build the native
`ApiAgentDriver` properly from the start, so dev workers are **fully model-independent on day
one**. `ClaudeCliDriver` is kept **only** as the **Claude-subscription** provider-path (the cost
lever); the driver is selected by the model's provider. To keep the cockpit **driver-agnostic**,
the `ApiAgentDriver` must (a) emit the **same agent-activity event stream** the CLI driver does,
and (b) **normalize each provider's tool-call format** (Anthropic `tool_use` vs OpenAI-style
`tool_calls`) into Camerata's MCP `gated_write` / tool path. The dev engine (run flow + driver
seam + model resolution) is built as a **reusable engine** so the future routine engine drives the
*same* engine in `Batched` oversight mode (3-phase doc §8) — no fork.

## 7. Bucket-relief routing (the payoff)

Max-efficiency profile → **routines + bare-LLM steps + dev fast/balanced workers run on free
OpenRouter (API driver) with paid fallback**; **Opus orchestrator (and optionally Sonnet) stay on
the subscription CLI**; L3 free/off. Heaviest buckets offloaded off the subscription → **quota
relief**, with Opus reliability + fallback preserved.

## 8. Per-provider rate limiting + OpenRouter caching controls

**Per-provider RPM limiter (Zach — build now):** a shared limiter (token bucket / semaphore)
keyed by provider that caps outbound requests to a provider's RPM (default OpenRouter ≈ 20 RPM,
configurable). Applies to BOTH the `Completer` (bare-LLM) and the `ApiAgentDriver`. Effect: bursty
free-routed work (onboarding's **concurrent audit passes**, fan-out) **self-throttles under the
cap → stays fully free, just slower**, instead of throttling/falling back. The Claude subscription
path is not limited here.

**OpenRouter caching controls (paid-via-OpenRouter) — VERIFY exact API surface before wiring;
implement what's real, flag what isn't:**
- **Sticky routing:** stable **`session_id`** in the payload → subsequent requests hit the same
  provider endpoint (activates caching from request one).
- **Anthropic cache breakpoints (via OpenRouter):** **`cache_control: { type: "ephemeral" }`** on
  the large *static* blocks (repo map / system instructions) — ties into §5 prefix-stability.
- **Response caching (identical-request):** **`X-OpenRouter-Cache: true`** to enable;
  **`X-OpenRouter-Cache-Clear: true`** to bust (stuck loop); read **`X-OpenRouter-Cache-Status`
  (HIT/MISS)** for tracking. *(Confirm these header names against current docs first.)*

## 9. Cost estimation reflects the active models

The estimator prices each call by **the assigned model's registry price** (chunk 2): **free →
$0**, paid → per-token price, subscription → quota-weight (not $). So the estimate tracks whatever
profile is active (Max efficiency → near-$0 on offloaded work).

## Build sequencing (on go)

1. ✅ **Credentials manager** (keychain; OpenRouter + GitHub) — `acb7867`.
2. ✅ **Model registry** (Claude static + OpenRouter discovery + badges) — `bd586b6`.
3a. ✅ **`Completer`-direct** (OpenRouter Completer + provider factory) — `3216a1f`.
3b. **Native `ApiAgentDriver`** (MCP loop via gateway lib, tool-call normalization, event parity,
    driver-by-provider, invariant tests) — *the big one*.
4. **Per-provider RPM limiter** (§8) — built now, for free-model onboarding testing.
5. **Tier chains** (fast/balanced → `Vec`) + request-level **fallback policy** (§4).
6. **Model Efficiency Profile + cascade** (§3) + auto-save + confirm-preview + **cost estimation** (§9).
7. **Caching** (§5 + §8): subscription prefix-stability + OpenRouter cache controls (verified).

## Resolved decisions (2026-06-27, with Zach)

1. **Re-apply behavior:** overwrite-all, via a **confirm popup previewing `current → new`** per
   entry (+ count affected). Per-entry override-protection deferred.
2. **Credentials:** **app-level, keychain-backed, UI-set, masked** — OpenRouter key **+ GitHub
   token** in one unified credentials area (§1a).
3. **Default profile:** **Balanced** (subscription tiering + warm caching; free off/overflow). The
   user opts into **Max efficiency** for the free-offload quota relief. Caching is per-model, so
   Max efficiency trades reliability, not caching value (§5).
4. **Fallback policy:** 429 / 402 / 5xx / timeout / bad-tool-call → fall back; **selective on 400**
   (capability only); **never** on 401/403; cap + escalate (§4).

## Scope notes

- **No CLI-proxy interim (Zach):** the native `ApiAgentDriver` is built **up front** (sequencing
  #3), so **full free-model dev workers are available day one** — no two-wave rollout.
  `ClaudeCliDriver` is kept solely for the **Claude-subscription** path.
- **Credentials are APP-WIDE; the model profile + per-entry model selections are PER-PROJECT.**
  (Keys = your account, shared across all projects; model choices = per project.)
- **Routines reuse the dev engine** (built reusable, driven in `Batched` mode later) — no fork;
  this plan only sets routines' *model defaults*.
- **GitHub auth migration:** fold existing GitHub auth into the keychain credentials manager.
