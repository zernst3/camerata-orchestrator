# Per-project backend (discard the global env-var backend)

Date: 2026-09-22
Status: approved (owner-directed), building
Branch: `feat/audit-report-export`
Supersedes the backend-selection half of `docs/design/2026-08-27_backend-safety-and-live-models.md`
(the `cli_active` flag + the global `CAMERATA_LLM_BACKEND`-driven gate). The `/v1/models`
half of that doc still stands.

## Why

The prior model had THREE overlapping backend controls: the `CAMERATA_LLM_BACKEND` env var,
its settings-store override (the global "Claude backend" toggle), and the per-project
`cli_active` boolean. `cli_active` (default false) was made to force API-only even over an
explicit global `CLI` choice, so a user who set the app to CLI still got "API-only (CLI
disabled), no key" and a blocked scan. That is contradictory and confusing.

Owner directive (2026-09-22): discard the env variable entirely. There are exactly TWO
settings and no others:
1. a **per-project backend** setting, and
2. a **single global backend** setting whose ONLY job is the chatbox (which lives outside any
   project's sphere).

## Target model

### Per-project backend — the source of truth for all project-scoped AI
`Project.backend: ProjectBackend { Cli, Api }` (replaces `Project.cli_active: bool`).
- **Default: `Cli`** (the zero-setup subscription path — a scan "just works" out of the box).
- Governs EVERY project-scoped model call: the brownfield scan, the alternative
  recommendation pass, the disagreement rescan, and the governed dev loop.
- `Cli` -> the Claude Code subscription (no key needed).
- `Api` -> the Anthropic API; needs a key. If the key is missing, the AI does not run and the
  "AI review did not run" banner shows (the deterministic floor still runs) — the behavior we
  just shipped in `1451c0b`.
- For CLIENT work: set that project's backend to `Api` (metered, single-party, no-train,
  nameable in a contract). That is the whole client-safety story now, expressed per project.

### Global backend — chatbox only
One persisted global setting (`settings.chat_backend: ProjectBackend`, default `Cli`) used
ONLY by the project-less chat assistant. Nothing project-related reads it. (The existing global
"Claude backend" toggle is repurposed to this; relabel it "Chat backend".)

### Removed
- The `CAMERATA_LLM_BACKEND` env var and its boot-time hydration (`lib.rs` `set_var`). No env
  drives backend selection anymore.
- `Project.cli_active`.
- The `CliFallbackWarn` resolution path (a per-project explicit choice needs no silent
  fallback). `resolve_backend` collapses to: Api+key -> Api; Api+no-key -> Blocked; Cli -> Cli.
- The global-backend-applies-to-project-work behavior.

## Resolution (trivial now)
- Project scan/dev: from `project.backend`. `Api && !has_key -> Blocked{message}`;
  `Api && has_key -> Api`; `Cli -> Cli`.
- Chat: same three-way against `settings.chat_backend`.
No env reads, no cross-setting override, no fallback.

## Migration
Existing projects carry `cli_active: bool`. On load, map ALL existing projects to
`backend = Cli` (the new default) — nobody deliberately set API-only yet (the feature is days
old), and Cli is what unblocks the owner's dogfooding. Implement as a serde-compatible
migration: accept an absent/legacy `cli_active` and yield `backend = Cli`; drop `cli_active`.

## Endpoints
- Replace `POST /api/projects/{id}/cli-active` with `POST /api/projects/{id}/backend`
  body `{ "backend": "cli" | "api" }` -> updated project.
- The global setting: repurpose the existing `POST /api/settings/llm-backend` as the CHAT
  backend (or add `POST /api/settings/chat-backend`); `GET /api/settings` returns
  `chat_backend` + `api_key_present`. Remove the env-precedence logic from `effective_llm_backend`.

## Seams to rewire (all project-scoped resolution -> `project.backend`)
- `resolve_backend_for_project` (`lib.rs`) -> read `project.backend`, not
  `effective_llm_backend` + `cli_active`.
- The audit (`onboard::audit_repos`), the alternative recommendation + rescan (`ai_audit`,
  the `rescan-alternatives` endpoint), and the gov-dev driver (`api_agent_driver` — currently
  reads `CAMERATA_LLM_BACKEND`) must all resolve from the project's backend.
- The chat path resolves from `settings.chat_backend`.
- `Llm::from_env` / `select_backend`'s env read: the project paths must NOT use it; build the
  `Llm` with the resolved per-project (or chat) backend explicitly.

## UI
- Project settings: a per-project **Backend** control (CLI / API), replacing the "Allow Claude
  CLI" checkbox. Clear copy: CLI = your Claude subscription (no key); API = Anthropic API
  (needs a key; the compliant metered chain for client code).
- Settings: relabel the global "Claude backend" toggle to **"Chat backend"** and clarify it
  affects ONLY the assistant/chatbox, not project scans.

## Testing (unit + e2e + UI)
- Resolution: per-project Api+no-key -> Blocked (banner), Api+key -> Api, Cli -> Cli; chat uses
  its own global setting independently.
- The scan/recommendation/rescan/dev seams all read `project.backend` (a project set to Api
  with no key blocks with the banner + the floor still runs; a project set to Cli runs on CLI).
- No code path reads `CAMERATA_LLM_BACKEND` anymore (grep-guard test).
- Migration: a project persisted with the old `cli_active` loads as `backend = Cli`.
- UI: the per-project backend control POSTs + reflects; the Settings toggle is chat-scoped.

## Build phases (tiered — Sonnet implements, Opus verifies)
1. Core + server: `Project.backend` (+ migration), `resolve_backend` collapse, rewire the four
   project seams + chat, drop the env var + hydration + `cli_active` + `CliFallbackWarn`,
   endpoints, tests.
2. UI: per-project Backend control, Settings "Chat backend" relabel, tests.
3. Docs: README / USER_GUIDE / ARCHITECTURE + this decision.
