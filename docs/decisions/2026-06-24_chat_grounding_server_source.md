# 2026-06-24: Chat Grounding Server-Source Fix

## Problem
`active_project_context` read scan results only from the UI-round-tripped onboarding draft (`extract_scan_results_from_draft`). The draft is saved async/fire-and-forget and gated on a `draft_loaded` flag, so when chat queries, the server-side draft often has no findings yet (timing race; permanent if the user switches tabs first).

## Decision
Hold the last completed scan report per project on the server (`AppState::last_scan: Arc<Mutex<HashMap<String, ScanReport>>>`), written the instant any scan handler finishes (both sync `onboard_audit` and async job paths). Read it in `active_project_context` with the draft as first-priority fallback.

## Consequences
- Scan results are immediately available to chat after any scan completes, regardless of UI state or draft timing.
- Draft still takes precedence when present (preserves triage/disposition context).
- In-memory only (v1); survives process restart is a v2 concern.
- Fail-soft on lock poisoning: treats as empty, never panics the handler.
