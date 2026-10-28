# Alternative Advisor — AI-recommended rule alternatives

Date: 2026-09-21
Status: approved (owner-directed), design — build pending go-ahead
Branch: TBD (likely continues on the audit-report line or a fresh feature branch)

## Motivation

Many rules carry multiple `[[option]]` alternatives; the project's `chosen_option`
selects one (`Rule.resolved_option` falls back to `default_option`). Rules with NO
default already surface in the UI as **"Needs option — you must choose before
arming"** (the `needs_option` filter, `crates/ui/src/cockpit/rules.rs:3726`).
Choosing the right alternative for each is manual and requires the operator to know
the codebase's existing conventions.

The **Alternative Advisor** is a lightweight AI pass that reads the codebase and
recommends the best-fitting alternative for each multi-option rule, with reasoning,
so the operator can accept them in one click (or override per rule).

## The principle that keeps it lightweight + non-overlapping with the audit

The audit asks: **"does the code VIOLATE this rule?"**
The advisor asks: **"which alternative matches the pattern this codebase already
uses (or should use)?"**

Different question, different inputs, different output — no shared work with the
audit engine (`ai_audit.rs`). E.g. for an API-versioning rule, the advisor inspects
how existing routes are already versioned and picks the matching option; it does not
scan for violations.

## Decisions (owner-approved 2026-09-21)

- **Scope:** ALL selected rules with **≥2 options** (not only the no-default "must
  choose" set). The advisor confirms-or-overrides defaults based on actual code, so
  it adds value everywhere a choice is meaningful. The no-default rules are the
  mandatory subset (highlight them).
- **Trigger:** MANUAL. A **"Suggest alternatives"** button the operator presses after
  the initial scan (fully decoupled; no surprise cost). A SEPARATE **"Accept
  recommended alternatives"** button applies them. Per-rule override always available.

## Architecture

### Input / evidence (lightweight, no re-walk)
- Operate over the multi-option rules in the project's selected/proposed set.
- Reuse the **initial scan's already-collected repo map / file digest** (in memory /
  cached from the stack+propose pass) plus a small **targeted evidence slice per
  rule's domain** (the domain→glob mapping already exists). No full audit digest, no
  violation analysis, no second repo walk.

### The model call
- **One batched structured LLM call** (or a few, grouped by domain) →
  `{ rule_id → { recommended_option_id, reasoning, confidence } }`. Bounded to the
  multi-option rule set; cheap.
- Prompt per rule: the rule's `title` + `summary` + each option's `id`/`label`/`why`
  + the targeted codebase evidence. Ask which option best fits + a 1-3 sentence
  rationale grounded in the evidence. `recommended_option_id` MUST be one of the
  rule's real option ids (validate; drop/flag any hallucinated id).
- **COMPLIANCE — through the backend gate:** this is a NEW model-calling path over
  client code, so it MUST resolve through `crate::llm::resolve_backend` with the
  project's `cli_active` exactly like the audit + gov-dev seams (see
  `docs/design/2026-08-27_backend-safety-and-live-models.md`). `Blocked` refuses to
  run; `CliFallbackWarn` warns. Do not add a transport that bypasses the gate.

### App-consistent model + observability (owner requirements 2026-09-21)
The advisor must behave like every other AI step in the app — NOT a special-cased path:
1. **Model selectable the same way as the rest of the app.** Add a NEW per-step slot
   to `StepModels` (`crates/app-core/src/project.rs:300-310`), e.g. `alternative_advisor`,
   serde-default to `DEFAULT_MODEL`, mutated via the existing `ProjectStore::set_step_model`
   path and shown in the SAME per-step model picker UI as audit/calibration/etc. The
   advisor resolves its model from the project's `StepModels`, exactly like the audit
   resolves its own. (Tier the DEFAULT to Sonnet-class, but it is user-selectable.)
2. **Background animation.** The advisor's in-flight state must drive the centralized
   AI-in-flight animation (the `LoadingGuard` in `crates/ui/src/loading.rs` we
   centralized earlier this session) so the background animates during the pass, like
   every other AI call. Do not add a separate/ad-hoc spinner.
3. **Token cost counter.** Build the advisor's `Llm` via `from_env_with_ledger` with
   the process-global `UsageLedger` attached, so its usage folds into the cumulative
   cockpit token/cost meter — same chokepoint as the audit + chat.

### Data model + persistence
- A recommendation record per rule: `{ recommended_option_id: String, reasoning:
  String, confidence: Option<String> }`. Stored per project (transient scan-result
  store keyed by project, surfaced to the UI; persistence optional — recommendations
  can be re-run, so a lightweight store or in-memory-with-refresh is fine).
- **Accept** = write each `recommended_option_id` into the matching `SelectedRule.
  chosen_option` (`crates/app-core/src/project.rs:120`) in the project's
  `ProjectRuleset`. "Accept recommended alternatives" applies ALL; per-rule accept
  applies one. Override = the operator picks a different option (existing UI).

### Endpoints (mirror existing per-project mutators)
- `POST /api/projects/{id}/suggest-alternatives` → runs the advisor, returns the
  recommendations (+ any `Blocked`/warn state from the gate).
- `POST /api/projects/{id}/accept-alternatives` (all, or `{ rule_ids: [...] }`) →
  writes `chosen_option`s, returns the updated ruleset.

### UI (`crates/ui`)
- An intermediary panel/section after the initial scan: each multi-option rule with
  its **recommended alternative pre-highlighted** (mandatory no-default rules
  flagged), a per-rule **"Why" modal** showing the reasoning + confidence, the
  **"Suggest alternatives"** trigger, the **"Accept recommended alternatives"** bulk
  button, and per-rule accept/override.
- Consistent with the existing rules-table + rule-modal patterns
  (`cockpit/rules.rs`).

## Phases (proposed)
1. **Core pass + data model** (server): the advisor engine (evidence assembly from
   the cached repo map + domain slice; batched structured call through
   `resolve_backend`; option-id validation), the recommendation store, the two
   endpoints, tests (incl. hallucinated-option-id rejection + the gate paths).
2. **UI**: the intermediary panel, recommended-highlight, Why modal, Suggest +
   Accept buttons, per-rule override; tests.
3. **Docs + e2e** per the ship-with-docs-and-tests bar.

## Non-overlap guardrails (must hold)
- No call into the audit engine (`ai_audit.rs`) or its digest.
- No second repo walk — reuse the initial scan's collected files/repo map.
- Recommendations never auto-apply — the operator presses Accept.
- Never emit a `recommended_option_id` that isn't a real option on that rule.
