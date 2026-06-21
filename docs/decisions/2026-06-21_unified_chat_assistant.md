# 2026-06-21 — Unified context-rich chat assistant (replaces mode-switching)

**Status:** Implemented (pw/chat-ui)
**Scope:** `crates/ui/src/chat.rs` only (inline styles; no changes to style.rs or cockpit.rs)

---

## What changed and why

The previous chat panel had four separate modes (Research, Guide, Technical, Project) with
a mode selector. The architect had to know which mode to pick before asking a question,
and a question about a rule while in Technical mode would get no answer because the rules
catalog wasn't in scope.

The new design is a single unified assistant that bundles ALL available context into one
system prompt every turn. The user's prompt determines which part of the grounding the
assistant draws from; no mode selector is needed.

---

## The four context layers

| Layer | Source | When injected |
|---|---|---|
| 1a | `docs/TECHNICAL.md` (compile-time `include_str!`) | Always |
| 1b | `docs/USER_GUIDE.md` (compile-time `include_str!`) | Always |
| 2 | Live corpus catalog (`GET /api/corpus-rules`) | When non-empty (fetched once) |
| 3 | Live UoW snapshot (`GET /api/development/context` or `/api/uow`) | Per turn (tail of prompt) |
| 4 | Focused finding (`FindingContext` prop) | When `rule_id` non-empty (additive) |

Layers 1 and 2 are static within a session and form a stable prefix that Anthropic's
automatic system-prompt caching can retain across turns. Layer 3 is appended last (the
tail) so a fresh UoW snapshot never disturbs the cached prefix. Layer 4 is an additive
lens on top of all other layers, not a mode replacement.

---

## Honesty guardrail

`UNIFIED_NOT_COVERED_PHRASE` ("I don't have that in any of my current context layers.")
is the exact string the assistant must say when none of the four layers cover the question.
It appears in the preamble (before Layer 1), so the model encounters the constraint before
reading any grounding data.

Hard-coded + tested: changing the wording requires updating both the prompt builder
(`unified_system_prompt`) and the tests.

---

## UoW snapshot endpoint

The UI calls `GET /api/development/context` (being built on `pw/server`). As a fallback
while that branch is unmerged, `fetch_uow_snapshot` falls back to `GET /api/uow`, which
returns the same wire shape (`UnitOfWork`, superset of `UowSnapshot`). Both deserialise
into `UowSnapshot` via `#[serde(default)]` — extra fields are silently ignored.

`gate_status` is derived client-side from `gate_provenance`:
- `None` → "no run yet"
- `deny_count > 0` → "gate blocked" (surfaces `rules_fired` in the prompt)
- `deny_count == 0` → "gate passed"

`render_uow_section` caps at 100 stories to keep the prompt bounded.

---

## "What this assistant can see" affordance

An inline strip below the panel header lists the four sources with live status indicators:
- Green dot when the source is loaded/active.
- Gray dot when the source is loading or absent.
- Amber badge when a focused finding is in scope (Layer 4).

Inline styles only (no style.rs edits), per the task constraint.

---

## Public API preserved

No breaking changes to the public surface:

| Symbol | Visibility | Change |
|---|---|---|
| `FindingContext` | `pub` | Fields unchanged |
| `ChatBubble` | `pub fn (component)` | Props unchanged (`finding: Option<FindingContext>`) |
| `ChatBubbleProps` | `pub struct` | Field unchanged |

Removed (were `pub(crate)`, internal only):
- `ChatMode` enum
- `guide_system_prompt`, `technical_system_prompt`, `project_system_prompt`
- `GUIDE_NOT_COVERED_PHRASE`, `PROJECT_NOT_COVERED_PHRASE`
- `ProjectContextResp` (the project context endpoint is still hit by the server; the
  UI no longer fetches it separately since the UoW layer covers the project state)

Added (new `pub(crate)` surface for tests):
- `unified_system_prompt` — the single prompt builder
- `UNIFIED_NOT_COVERED_PHRASE`
- `UowSnapshot`, `GateProvenanceLite`
- `render_uow_section`

---

## Tests (28 total, all pass)

All pure, no live calls.

| Category | Count |
|---|---|
| Compile-time doc constants (`TECHNICAL_DOC`, `USER_GUIDE`) | 2 |
| `UNIFIED_NOT_COVERED_PHRASE` well-formed | 1 |
| `unified_system_prompt` structural shape (layer headers, content) | 13 |
| `render_uow_section` (empty, single, gate states, cap) | 8 |
| `rules_catalog_loaded` helper | 2 |
| Layer ordering tests (1<2<3, 3<4) | 2 |

`cargo check -p camerata-ui`: clean.
`cargo test -p camerata-ui -- chat`: 28/28 pass.

---

## Not done / follow-ups

- **`GET /api/development/context` server endpoint** (pw/server branch): the fallback to
  `/api/uow` works correctly today; the dedicated endpoint adds a purpose-built projection
  (only the fields the chat needs). Once it merges into dev/integration, the fallback path
  is still correct.
- **Per-turn UoW refresh**: `use_resource(fetch_uow_snapshot)` fetches on mount and on
  reactive dependency changes. A "refresh" button or automatic re-fetch on each message
  send would keep the snapshot more current during long sessions. Low priority.
- **Finding wiring in the cockpit**: the `FindingContext` prop and the context signal
  wiring in `cockpit.rs` are unchanged; the cockpit's "Ask AI about this finding" button
  continues to work.
