# AI-native engine, agent-agnostic provider seam

Status: accepted (2026-06-16); Anthropic wired, other vendors reserved.

## Context

Camerata is an AI-native product: the audit, story investigation/decomposition,
clarification authoring, routine-prompt authoring, and code generation are all model
work. Early implementation had the governance skeleton real but the AI mostly stubbed
(deterministic scaffolds, a mechanical-only brownfield scan, live code-gen behind an
opt-in flag). Two questions had to be answered to make it actually AI-driven:

1. What stays deterministic, given the "deterministic governance" thesis?
2. How do models get called, and by which vendor?

## Decision 1: AI discovers and generates; only enforcement is deterministic

Two different jobs touch "rules," with opposite correct answers:

- **Discovery / generation / understanding** → **AI.** Reading a codebase to find
  genuine architectural violations, decomposing work, asking the right clarifying
  questions, writing code. This is most of the product.
- **Enforcement** → **deterministic.** The single deny-before-execute gate that blocks
  a write before it touches disk. It must be mechanical: it is the backstop a
  hallucinating or jailbroken AI cannot talk past. If enforcement were AI, Camerata
  would be "an AI that advises about rules," the thing it exists to beat.

So **AI finds and writes; the deterministic gate is the seatbelt.** Caveat: some genuine
violations can't be reduced to a mechanical gate (e.g. "god object"). Those are enforced
either by turning them into a compiled contract / AST check, or by an AI-assisted
integration review (pre-PR). The deny-before-execute layer is the deterministic core;
outer layers may be AI-assisted.

### Brownfield is two tiers, not a linting exercise

- **Tier 1 (deterministic):** secrets / raw SQL / path-escape — precise, line-level,
  the same arms the gate enforces. Determinism is correct here; AI would be worse.
- **Tier 2 (AI):** the genuine violations that need a model to read the code — missing
  auth on a write path, services bypassing the repository layer, N+1, cross-boundary
  imports, inconsistent money/date handling, god objects. `ai_audit.rs`. AI discovers;
  the architect approves; approved rules become gate config (mechanical where possible)
  or AI-assisted integration checks.

## Decision 2: one vendor-agnostic provider seam; two transports; Anthropic wired

`llm.rs` is the single seam every AI step calls. The request/response shapes
(`LlmRequest` / `LlmResponse`) are vendor-neutral on purpose.

Two axes:
- **Vendor** (`CAMERATA_LLM_VENDOR`, default `anthropic`). Camerata *happens* to ship
  with Anthropic; the end state is vendor-neutral — a user picks Anthropic, OpenAI,
  Google, or others. `OpenAi` / `Google` are reserved enum arms that return a clear
  "not wired yet — add an arm here" rather than being absent or silently falling back.
- **Transport** (`CAMERATA_LLM_BACKEND`, default `cli`). For a vendor offering both:
  `cli` shells the vendor's CLI (the LOCAL path: a human's own login, no key — the
  CLI's terms fit a single human driving the app); `api` calls the HTTP API with a key
  (the PRODUCTION / multi-user path, which can't ride one person's CLI session).
  Anthropic offers both; other vendors are API-only.

**Adding a vendor is a new match arm in `Llm::complete` plus its `MODELS` entries — not a
rewrite.** The model is selectable per call (`CAMERATA_LLM_MODEL` default; the research
chat and any future per-step config override it). The active provider is surfaced
honestly in the UI as `vendor/transport` (e.g. `anthropic/cli`).

Local testing uses the CLI; production uses the API. Both ship out of the box.

## Surface

- `llm.rs` — `Llm` (vendor + transport + model), `Vendor`, `LlmRequest/LlmResponse`,
  `MODELS` (vendor-tagged). `POST /api/chat`, `GET /api/models`.
- `ai_audit.rs` — Tier-2 brownfield audit, merged into the scan's findings/proposals.
- AI wired into: brownfield scan, routine draft-prompt, story decomposition
  (`propose_ai`), clarify-suggest, the research chat. Code-gen live path is next.
- `transcript.rs` + the Agent-activity drawer surface the generated prompts/outputs so
  the AI work is inspectable, not hidden.

## Status / next

Anthropic CLI + API both work. Remaining: make the live AI code-gen fleet the default
(behind a capability check), wire OpenAI/Google vendor arms when wanted, and let each
AI step carry its own model choice (per-step config, not just the global default).
