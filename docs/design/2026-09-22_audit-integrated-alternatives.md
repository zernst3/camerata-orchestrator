# Audit-integrated alternative recommendation + disagreement rescan

Date: 2026-09-22
Status: approved (owner-directed), building
Branch: `feat/audit-report-export`
Supersedes: `docs/design/2026-09-21_alternative-advisor.md` (the separate intermediary
"advisor scan" — abandoned; its recommendation logic is folded into the main audit here).

## Rationale for the pivot

The separate advisor pass duplicated effort: to recommend an alternative you must
understand the repo, and understanding the repo IS most of the audit. Folding
recommendation into the main audit removes the duplicate scan AND inherits, for free,
the four integrations the standalone advisor would have re-implemented: the audit
already has a **selectable model** (the `audit` `StepModels` slot), already
**accumulates in the token counter** (`UsageLedger`), already **drives the background
animation**, and already routes through the **`resolve_backend` compliance gate**. No
new integration surface.

## The four changes

### 1. Remove the "must choose an alternative" gate
Today a selected rule with no `default_option` and no `chosen_option` blocks
progression ("No default — you must choose an alternative before arming",
`crates/ui/src/cockpit/rules.rs:1680`, plus any server-side equivalent). REMOVE that
blocking behavior. A rule can proceed to the audit with no chosen alternative; the
scan recommends one. (Keep the *visual* "Needs option" affordance as informational —
it just no longer blocks.)

### 2. The audit prompt feeds ALL alternatives per rule
Scope: the multi-option **semantic** rules (the AI-judged ones with >= 2
`[[option]]`s). Single-option / mechanical / deterministic-floor rules are unchanged.
For each such rule, the AI prompt now includes EVERY option (`id` + `label` +
`directive` + `why`), plus a marker for which is **currently selected** (default or
manual) and is therefore "what we think is right." The AI is free to pick a different
one. Do NOT feed all options to the deterministic floor — this is the semantic tier
only. (See `ai_audit.rs` prompt/prefix builder.)

### 3. One scan, two outputs per multi-option rule
The AI emits, per rule:
- `recommended_option_id` (must be one of that rule's real option ids — VALIDATE;
  drop/flag hallucinations),
- `recommendation_reasoning` (1-3 sentences, grounded in the codebase evidence),
- and the **violations computed AGAINST the recommended option** (the existing
  `Finding` shape, each finding tagged with the `evaluated_option_id` it was judged
  under).

Coherence principle: violations are ALWAYS relative to ONE chosen alternative
(different alternatives define different "correct"). We never score violations
against every alternative (an option the codebase doesn't follow would look like
violations everywhere — noise). Recommend one, report its violations.

### 4. Disagreement -> BATCHED targeted rescan
The operator reviews the recommendations, stages overrides across any number of
rules, then triggers ONE rescan of just the overridden rules with their forced
options. See the UX + endpoints below.

## Data model

- **AI audit finding output** (extend `ai_audit.rs` structured output + prompt): per
  multi-option rule, add `recommended_option_id` + `recommendation_reasoning`; tag
  each `Finding` with `evaluated_option_id: Option<String>` (which option it was
  judged under). Additive `#[serde(default)]`.
- **Recommendation store**: per project/scan, `{ rule_id -> { recommended_option_id,
  reasoning, evaluated_option_id } }`, surfaced to the UI alongside the findings.
- **`resolve_fix` honors the chosen option** (`report_export.rs`): resolve remediation
  for the finding's `evaluated_option_id` / the project's `chosen_option` (via
  `Rule::resolved_option(chosen_option)`), not always the default — so when the
  operator accepts the AI's recommended alternative, the report's Fix text matches it.
- Accepting a recommendation writes `recommended_option_id` into
  `SelectedRule.chosen_option` (`crates/app-core/src/project.rs:120`).

## The review + disagreement UX (owner-directed design)

Keep the violations table as it is today. Add the alternative-review controls:

- **Findings table, per multi-option rule group header:** a compact control line —
  `Evaluated against: <Option label> (AI recommended) · [Why?] · [Change ▾]`. The
  `Why?` opens a modal with `recommendation_reasoning` + confidence. `Change ▾` is a
  dropdown of that rule's alternatives.
- **"Rule alternatives" review panel:** lists EVERY multi-option rule in scope,
  including ones with ZERO violations (so the operator can override those too), each
  with the same recommended-option + Why + Change controls.
- **Staging an override:** picking a different option in EITHER place does NOT rescan
  immediately — it stages a pending override (the rule row gets a "override, pending
  rescan" badge). Overrides accumulate across rules.
- **Sticky rescan action bar:** appears when >= 1 override is pending, showing the
  count — **"Rescan N overridden rules"**. Pressing it fires ONE batched rescan of
  exactly those rules with their forced options; the table + panel update in place;
  the overridden rules' recommendation becomes "your choice" (locked, not the AI's).
- **Accept:** an "Accept alternatives" action persists the final per-rule selections
  (AI's where not overridden, the operator's where overridden) into `chosen_option`.

### Per-rule state machine
`recommended` (AI picked) -> `override_pending` (operator chose different, not yet
rescanned) -> `override_applied` (rescanned; findings reflect the operator's choice).
`accepted` = the current selection (AI's or operator's) is persisted to `chosen_option`.

## Server endpoints
- The **main audit** response carries the per-rule recommendations + reasoning + the
  `evaluated_option_id`-tagged findings (extension of the existing scan result).
- **`POST /api/projects/{id}/rescan-alternatives`** body `{ overrides: [{ rule_id,
  chosen_option_id }, ...] }` — re-runs the AI for ONLY those rules with the forced
  options (through `resolve_backend`, ledger-attached, same as the audit), returns the
  updated findings for those rules (to merge in place), marks them operator-chosen.
  Validate each `chosen_option_id` is a real option on its rule.
- **`POST /api/projects/{id}/accept-alternatives`** body `{}` (all) or `{ rule_ids:
  [...] }` — writes the current selections into `chosen_option`; returns the updated
  ruleset. Mirror an existing per-project mutator handler's shape.

## Compliance / model / observability (inherited from the audit)
The recommendation + the rescan are part of the audit's AI path, so they use the
`audit` `StepModels` model (selectable like everywhere), fold into the `UsageLedger`,
drive the `LoadingGuard` animation, and route through `resolve_backend` (Blocked
refuses; CliFallbackWarn warns) — nothing new to wire. The batched rescan endpoint
must build its `Llm` the same way (`from_env_with_ledger` + gate).

## Testing (thorough — unit + e2e, incl. UI)
- **Unit (server):** all-options prompt assembly; recommendation parse +
  hallucinated-option-id rejection; findings tagged with `evaluated_option_id`;
  `resolve_fix` resolves remediation for the chosen/recommended option (not default);
  the batched rescan re-evaluates only the given rules with the forced options; accept
  writes `chosen_option`; gate paths (Blocked/CliFallbackWarn) on both the audit and
  the rescan endpoint.
- **e2e (server):** a scan over a fixture with >= 2 multi-option rules (one with a
  default, one without) produces recommendations + violations-under-recommended;
  a batched `rescan-alternatives` for 2 overridden rules returns updated findings for
  exactly those; accept persists the choices; the removed gate lets a no-default rule
  proceed.
- **UI:** the group-header Change control + Why modal render; staging N overrides
  shows the sticky "Rescan N" bar with the right count; pressing it calls the batched
  endpoint and updates in place; the review panel lists zero-violation multi-option
  rules and lets them be overridden; the gate no longer blocks proceeding.

## Build phases (tiered — Sonnet implements, Opus orchestrates/verifies)
0. **Red-tree fix** (separate agent, in flight): decouple the 5 remediation tests.
1. **Server**: prompt all-options + selected marker; AI output recommendation +
   reasoning + `evaluated_option_id`; recommendation store; remove the server-side
   gate; `resolve_fix` honors chosen option; the two endpoints; unit + e2e tests.
2. **UI**: findings-table group-header Change/Why controls; the review panel; staged
   overrides + sticky batched-rescan bar; accept; remove the UI gate; unit + UI tests.
3. **Docs + sample**: regenerate the sample if its shape changes; update CLAUDE/docs.
