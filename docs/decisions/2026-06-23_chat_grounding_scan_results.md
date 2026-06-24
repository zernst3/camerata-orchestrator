# Chat grounding: inject scan results (2026-06-23)

## Problem

The in-app chat assistant had no visibility into the active project's scan
findings. When asked "Do you have access to the results table from this scan?"
the bot correctly said "I don't have access to an actual scan results table."
The grounding endpoint (`GET /api/projects/active/context`) had a comment
claiming it supplied "draft / scan report / ruleset summary," but the scan
findings were either absent or buried in the opaque `draft_json` blob — not
in a queryable, structured form.

As a result, the bot could not answer:
- "What are my critical findings?"
- "How many SQL-related violations do I have?"
- "Which rule fires the most?"
- "Which file has the most findings?"

## Decision

Add a new `scan_results_section: Option<String>` field to
`ProjectContextResponse`. The server populates it by calling
`render_scan_results_for_chat` on the findings extracted from the
onboarding draft. The UI fetches the context endpoint and injects the
section as **Layer 3c** in the `unified_system_prompt`.

## What is in Layer 3c

The section is compact but queryable. It contains:

1. **Totals**: total finding count, counts per severity
   (critical / high / medium / low), counts by status (active / suppressed),
   and a preview vs floor split (preview = advisory, not yet wired into the
   CI gate; floor = enforced).
2. **By-rule breakdown**: top 20 rules by finding count, sorted descending.
   Lets the model answer "what is the most common rule?" or "how many
   ARCH-LAYERING-1 violations?"
3. **Capped finding list** (top 40, severity-ordered): one line per finding
   with `severity · rule_id · repo/path:line · status [preview]`, plus the
   gate-authored `detail` (capped at 120 chars).
4. **Coverage notes**: tools that were skipped or unavailable during the scan
   (e.g., "eslint not installed; JS rules skipped").

## Secret safety

`Finding.snippet` is **never** included in the chat context. Snippets are
the raw offending source lines and may contain hardcoded credentials or
other secret-shaped values. Only the rule id, file location, severity,
status, and the gate-authored `detail` field are surfaced. The `detail`
field is prose produced by the gate engine — it describes the violation
without quoting the raw value.

This is enforced in `render_scan_results_for_chat` and tested:
- `render_scan_results_never_leaks_snippet` (server test): constructs a
  finding whose `snippet` contains a split credential literal and asserts
  the output does not contain it.
- `extract_scan_results_from_draft_scan_findings` (server test): verifies
  the JSON extraction path also does not include the snippet.

## Architecture

```
scan produces findings
  -> DraftStore holds the onboarding draft (UI-owned JSON blob)
     draft["scan"]["findings"] = Vec<Finding> (serialized)
     draft["scan"]["coverage_notes"] = Vec<CoverageNote>
  -> GET /api/projects/active/context
       calls extract_scan_results_from_draft(draft)
         -> deserializes findings as Vec<Finding>
         -> calls render_scan_results_for_chat(&findings, &coverage_notes)
         -> returns None when no findings (fail-soft)
       populates scan_results_section: Option<String> on ProjectContextResponse
  -> UI fetches context endpoint (fetch_project_scan_section in chat.rs)
       pulls scan_results_section
       passes as scan_results_section: Option<&str> to unified_system_prompt
  -> unified_system_prompt
       Layer 3c: injects section when Some and non-empty
       section absent = silent (no error; model does not hallucinate scan data)
```

## Fail-soft behavior

- No active project: `scan_results_section` is `None`.
- Active project, no scan yet: `scan_results_section` is `None`.
- Scan run but no findings: `scan_results_section` is `None`.
- Endpoint unreachable: `fetch_project_scan_section` returns `None`;
  the chat still works (Layer 3c is simply absent).
- `draft["scan"]` deserialization fails (shape mismatch): falls back to
  `draft["audit"]["findings"]`; on total failure, returns `None`.

## Key functions

| Function | Crate | Purpose |
|---|---|---|
| `render_scan_results_for_chat` | `camerata-server` | Pure, testable. Takes `&[Finding]` + `&[CoverageNote]`. Returns compact prompt section. Never includes snippet. |
| `extract_scan_results_from_draft` | `camerata-server` | JSON path. Deserializes findings from the UI-owned draft blob; calls render. |
| `fetch_project_scan_section` | `camerata-ui` | Fetches `/api/projects/active/context` and extracts `scan_results_section`. |
| `unified_system_prompt` | `camerata-ui` | Accepts new `scan_results_section: Option<&str>` as Layer 3c. |

## Tests added

Server (`cargo test -p camerata-server`):
- `render_scan_results_empty_report_returns_empty_string`
- `render_scan_results_counts_by_severity`
- `render_scan_results_counts_by_status`
- `render_scan_results_preview_vs_floor`
- `render_scan_results_by_rule_breakdown`
- `render_scan_results_capped_at_40`
- `render_scan_results_never_leaks_snippet`
- `render_scan_results_includes_coverage_notes`
- `extract_scan_results_from_draft_none_on_empty`
- `extract_scan_results_from_draft_none_on_empty_findings`
- `extract_scan_results_from_draft_scan_findings`
- `extract_scan_results_from_draft_audit_fallback`

UI (`cargo test -p camerata-ui`):
- `unified_prompt_layer3c_present_when_scan_results_supplied`
- `unified_prompt_layer3c_absent_when_none`
- `unified_prompt_layer3c_absent_when_whitespace_only`
- `layer3c_appears_after_layer3b_and_before_layer4`
- `unified_prompt_preamble_mentions_scan_findings`

## Back-compat

Additive. No existing endpoint signatures changed. The new
`scan_results_section` field on `ProjectContextResponse` is
`#[serde(skip_serializing_if = "Option::is_none")]` — absent from the
JSON when `None`, so old UI versions that do not read this field are
unaffected.
