# Project-Aware Chat Mode (#54)

**Date:** 2026-06-20
**Status:** Implemented (dev1/project-aware-chat)
**Scope:** `crates/ui/src/chat.rs`, `crates/ui/src/style.rs`, `crates/server/src/lib.rs`

---

## What was built

A fourth chat mode ("Project") for the floating chat panel, grounded in the active
project's live state, plus an "Ask about this finding" path that focuses the
conversation on a specific audit finding.

### The four modes, post-PR

| Mode | Grounding | System prompt source |
|---|---|---|
| Research | None | No system prompt |
| Guide | `docs/USER_GUIDE.md` + live corpus catalog | `guide_system_prompt()` |
| Technical | `docs/TECHNICAL.md` | `technical_system_prompt()` |
| **Project** | **Live project state (draft / scan report / ruleset)** | **`project_system_prompt()`** |

---

## Server: `GET /api/projects/active/context`

A new **read-only** endpoint (no AI call) that returns the active project's current
grounding state as a `ProjectContextResponse`. The UI fetches this when the Project tab
is opened and builds the system prompt client-side before sending to the existing
`POST /api/chat`.

This design keeps AI calls on the existing `chat` handler and this endpoint purely
informational, avoiding a second AI endpoint.

### Phase detection

The endpoint detects the project's onboarding phase and populates different fields:

| Phase | Trigger | Fields populated |
|---|---|---|
| `blank` | No draft, no onboarded repos | `project_name`, `repos`, `message` |
| `pre_onboard` | Draft exists, no onboarded repos | + `finding_count`, `findings_summary`, `draft_json` |
| `post_onboard` | At least one repo in `onboarded` | + `ruleset_summary`, `finding_count`, `findings_summary` |

The draft is only injected in `pre_onboard` (to help the architect understand their
in-progress scan). In `post_onboard` the draft is omitted; the live ruleset + findings
summary is used instead (less noisy, more actionable).

### Findings extraction

`extract_findings_from_draft()` looks for `audit.findings` or `scan.findings` inside the
draft blob, caps at 50 findings, and renders each as a compact one-liner:
```
[severity] rule_id in repo/path:line — detail (120 char cap)
```

### Ruleset summary

`build_ruleset_summary()` converts the project's `ProjectRuleset` into a compact
human-readable listing, one rule per line with scope annotation:
```
SEC-NO-HARDCODED-SECRETS-1: repo-local (me/api)
INTEGRATION-API-CONTRACT-1: cross-repo
PROCESS-CONVENTIONAL-COMMIT-1: process (VCS workflow)
CUSTOM-house-style: custom rule (all repos)
```

---

## UI: `project_system_prompt()` in `crates/ui/src/chat.rs`

Assembles the system prompt from the `ProjectContextResp`. Phase-aware:

- **Blank**: minimal prompt explaining no scan data exists yet.
- **PreOnboard**: includes in-progress status, findings so far, and (if the draft has
  proposed rules) a compact rule listing from the draft's `scan.proposed_rules`.
- **PostOnboard**: includes the live ruleset summary + findings from the last audit.

### Not-covered guardrail

Mirrors the Guide mode pattern exactly: `PROJECT_NOT_COVERED_PHRASE` ("That isn't
covered by the current project context.") is required verbatim in the prompt so the
model has a concrete, testable response for out-of-scope questions. The phrase appears
BEFORE the project section headers (tested) so the model encounters the constraint
before reading the grounding data.

---

## "Ask about this finding" path

The `FindingContext` struct carries one specific finding's fields (rule id, severity,
file path + line, snippet, gate detail, repo).

When `FindingContext` is injected via the `ChatBubble` `finding` prop:
1. The panel opens automatically.
2. The mode switches to Project.
3. A `chat-finding-banner` strip appears below the disclaimer showing the rule id and
   file location.
4. The system prompt includes a `=== FOCUSED FINDING ===` section with the full finding
   detail, so the model answers "why was this flagged / how do I fix it?" concretely.

A unique injection key (`rule_id + path + line`) prevents re-injection on unrelated
re-renders. Switching modes or starting a new chat clears the active finding.

The `ChatBubble` component's existing call site (`chat::ChatBubble {}`) continues to
work with the default prop `finding: None` — no callers needed updating.

### Wiring the "Ask" button to a finding

The `ask-finding-btn` CSS class is added for use in the findings table (the cockpit's
audit findings panel). The cockpit can wire a click handler that sets a signal holding
`FindingContext` and passes it to `ChatBubble` as the `finding` prop. This wiring is
NOT done in this PR (it touches the cockpit's findings table, which is complex and
would expand scope). It is documented under Routed items.

---

## CSS additions (`crates/ui/src/style.rs`)

All new CSS is added inside the `GLOBAL_CSS` raw string before `#;`, per house style:

- `.chat-finding-banner` — the in-chat strip showing the focused finding's rule + location.
- `.chat-finding-label`, `.chat-finding-rule`, `.chat-finding-loc` — banner sub-elements.
- `.ask-finding-btn` — the "Ask about this finding" button for findings tables.

---

## Tests

All prompt-assembly logic is unit-tested without live model calls. New tests are in
`crates/ui/src/chat.rs::tests`:

| Test | What it verifies |
|---|---|
| `project_not_covered_phrase_is_well_formed` | Constant is non-empty, no leading space, has letters |
| `project_prompt_post_onboard_includes_project_and_ruleset` | Project name, ONBOARDED label, SELECTED RULESET section, real rule ids |
| `project_prompt_post_onboard_includes_findings_when_present` | Findings section header + finding text appear |
| `project_prompt_pre_onboard_includes_status_and_proposed_rules` | ONBOARDING IN PROGRESS, proposed rules from draft |
| `project_prompt_blank_phase_explains_no_data` | NO SCAN DATA label |
| `project_prompt_contains_not_covered_phrase_post_onboard` | Guardrail present |
| `project_prompt_contains_not_covered_phrase_pre_onboard` | Guardrail present |
| `project_prompt_contains_not_covered_phrase_blank` | Guardrail present |
| `project_prompt_not_covered_guardrail_is_marked_critical` | CRITICAL keyword in prompt |
| `project_prompt_with_finding_includes_focused_finding_section` | FOCUSED FINDING header, rule id, severity, path, gate detail |
| `project_prompt_without_finding_has_no_focused_finding_section` | No spurious injection |
| `project_prompt_with_empty_finding_has_no_focused_finding_section` | Empty `FindingContext::default()` treated as None |
| `project_prompt_with_finding_retains_not_covered_guardrail` | Guardrail survives finding injection |
| `project_prompt_not_covered_phrase_appears_before_project_section` | Ordering: constraint before grounding data |

Build result: `cargo check` clean (no errors, no warnings), `cargo test` 26 UI tests
pass, 219 server tests pass.

---

## Routed items (ROUTE-1: structural changes, do not auto-apply)

### Finding table wiring in the cockpit (not done here)

The cockpit's audit findings table (`crates/ui/src/cockpit.rs`) needs an "Ask" button
per row that:
1. Builds a `FindingContext` from the row's `FindingView`.
2. Passes it to `ChatBubble` via the `finding` prop.

This requires exposing `FindingContext` publicly (it's `pub` already) and adding a
parent-level signal in the cockpit to hold the active finding. The cockpit is large
(5000+ lines) and was explicitly excluded from this feature's scope to avoid parallel
conflicts. The `ask-finding-btn` CSS class is ready; wiring it is the follow-up.

### "Ask about this finding" from the drift/reconcile view

Once the cockpit wiring is done, the same pattern could wire "Ask" from the
reconciliation findings table and the suppression registry view.

---

## How a developer uses it

1. Open the floating chat panel (the `💬` button).
2. Click the **Project** tab.
3. The panel fetches `GET /api/projects/active/context` and builds the system prompt.
4. Ask "what findings did the last audit find?" or "what rules are in my ruleset?" and
   get answers grounded in the actual project data.
5. For a finding-scoped question: when the cockpit wiring is done, click "Ask" on a
   row in the findings table. The panel opens in Project mode with the finding in focus.
