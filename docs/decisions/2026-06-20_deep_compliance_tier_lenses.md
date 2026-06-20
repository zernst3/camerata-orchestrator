# Deep compliance & security tier (the three lenses)

Date: 2026-06-20
Status: Accepted (implemented)
Issues: #55 (the tier), #62 (MVP scope — brings it in-MVP), #56 (external validation — deferred Phase 2)

## What

An ADDITIVE, OPT-IN scan tier layered on top of the existing audit engine. When the audit
request sets `deep`, Camerata runs three analysis LENSES over each repo, AFTER the standard
scan, on the same (selected/Opus) model:

1. **SOC-2 readiness / GAP ANALYSIS** — maps the repo's detectable practices onto SOC-2
   Common-Criteria controls (CC6.1 access, CC6.7 data protection, CC7.2 logging, CC8.1
   change management, …) and reports the GAPS. It is a *gap analysis*, never a "report".
2. **DEEP SECURITY AUDIT** — a deeper-than-floor security read (authorization on write paths,
   sensitive-data handling, secret/credential flow, injection beyond raw-SQL, trust
   boundaries). Reuses the chunked audit engine with a security-focused system prompt; emits
   the standard `Finding` shape so the UI renders it in the familiar findings table.
3. **THREAT MODEL** — derives a structured threat model from the repo map: entry points,
   trust boundaries, data stores, sensitive-data paths, and the threats against them
   (STRIDE-flavored) with mitigations.

## Why

Per #62, mechanically-enforced governance + security is the MVP's whole selling point, and
front-loading capability is itself the pitch. The deep tier is "the governance wedge turned
up," not a different product — so it ships in-MVP as an option available from day one. But it
must never inflate every onboarding's cost: a baseline security pass (deterministic floor +
AI architectural audit) already runs by default, so the deep tier is strictly opt-in.

## How

All new code is additive (ROUTE-1: no new crates, no moved boundaries, no cross-crate API
changes):

- **`crates/server/src/ai_audit.rs`** — the whole tier lives here:
  - Three dedicated system prompts in their own functions: `soc2_gap_system_prompt`,
    `deep_security_system_prompt`, `threat_model_system_prompt`.
  - Structured output types: `DeepLens`, `Soc2Gap`, `Threat`, `DeepLensResult`, `DeepReport`,
    plus `DEEP_ADVISORY_DISCLAIMER`.
  - Robust parsers (`parse_soc2_gaps`, `parse_threats`) that normalize statuses/kinds/severity
    to closed sets and fail soft (malformed model output → empty result, never an error).
  - `run_deep_tier(repo, files, model, mode, …)` runs the three lenses CONCURRENTLY
    (`tokio::join!`). The two prose lenses (SOC-2, threat model) are single whole-repo passes
    (their value is the cross-cutting view); the security lens reuses the chunked engine via
    `run_security_passes` so a large repo is covered chunk-by-chunk, then deduped +
    location-merged like the standard audit.
- **`crates/server/src/onboard.rs`** — `audit_repos` gains a `deep: bool` parameter. When set,
  it captures each repo's whole file set, runs `run_deep_tier` per repo after the standard
  audit, and merges the per-repo results into one tier-level `DeepReport` (every repo's SOC-2
  gaps fold into the single SOC-2 lens, etc.) via `merge_deep_reports`. The result is attached
  to `ScanReport.deep` (a new `Option<DeepReport>`, serde-defaulted). When `deep` is false the
  behavior is byte-for-byte unchanged — the field stays `None`.
- **`crates/server/src/lib.rs`** — `AuditReq` gains `deep: bool` (serde-defaults to false),
  threaded through both `onboard_audit` and `onboard_audit_start` into `audit_repos`.

## Honesty guardrails (load-bearing, from #62)

These are not optional polish; they are the line between an honest tool and a liability:

- Every lens result carries `advisory: true` + `DEEP_ADVISORY_DISCLAIMER`. The disclaimer
  states the output is model-inferred, NOT externally validated, and is a static-code
  analysis, **not a penetration test**. External validation against comparator tools +
  ground-truth corpora is #56 Phase 2 (deferred).
- The SOC-2 lens is a **gap analysis** everywhere — the prompt, the `DeepLens::title`, and the
  types. It never claims certification. A unit test (`soc2_prompt_is_a_gap_analysis_never_a_report`,
  `deep_lens_metadata_is_stable`) locks this in so a future edit can't silently relabel it.
- A true pen test needs a running deployment (post-deploy, out of scope), unchanged from #55.

## Cost

The deep tier reuses the same per-call LLM machinery and the shared `UsageMeter`, so its
spend folds into the report's actual-vs-estimated readout. It is the MOST EXPENSIVE pass
(three extra whole-repo lenses on top of the standard audit, on the strong model) — which is
exactly why it is opt-in and never default. The UI's existing `estimate_audit_cost` (cockpit)
prices the standard audit from `code_chars`; the deep tier adds roughly three more whole-repo
passes, which the cost readout should surface as the priciest option.

## Usage

POST the standard audit request with `"deep": true`:

```json
{ "repos": ["owner/repo"], "rules": [ … ], "model": "claude-opus-4-8", "deep": true }
```

to `/api/onboard/audit` (synchronous) or `/api/onboard/audit/start` (background job). The
response's `deep` field then holds the three lens results; omit `deep` (or send `false`) and
the scan is unchanged.

## Tests

Structural/parse logic is unit-tested with fixtures (no live model): status/kind/severity
normalization, empty-row dropping, garbage tolerance, the honesty-guardrail prompt assertions,
lens metadata stability, and `DeepReport` serialization with the advisory envelope. The live
model calls (`run_deep_tier`, `run_prose_lens`, `run_security_passes`) are thin orchestration
over already-tested primitives and are not exercised against a live model in CI.

## Alternatives considered

- **A separate `/deep` endpoint.** Rejected — it would duplicate repo resolution, model
  selection, transcript wiring, and the incremental/cache plumbing. A `deep` flag on the
  existing `AuditReq` reuses all of it and keeps the handler list additive.
- **Per-repo `DeepReport`s in the response.** Rejected — the lenses keep their identity across
  repos (one SOC-2 lens, one security lens, one threat model), which reads better than three
  lenses × N repos. Merge preserves per-repo summaries + errors.
