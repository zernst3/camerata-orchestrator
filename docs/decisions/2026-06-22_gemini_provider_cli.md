# Add Gemini as a provider (CLI transport, seam + gated agents)

**Date:** 2026-06-22 · **Decided by:** Zach (AskUserQuestion: CLI transport + Full scope). Zach has
a Gemini Pro subscription and wants to test switching Camerata to Gemini models.

## STATUS — BACKLOG / BLOCKED (not building, 2026-06-22)

The chosen path (gemini-cli on Zach's **consumer Gemini Pro subscription**) is **no longer
possible**: on **2026-06-18** Google deprecated consumer access to `gemini-cli` (free / AI Pro /
Ultra / individual Code Assist), no grace period, pushing consumers to the closed-source
**Antigravity CLI**. Same-as-Claude Camerata use now requires a **paid `GEMINI_API_KEY`** (paid
Gemini project or Vertex; keys must be restriction-scoped as of 2026-06-19) — i.e. extra cost,
not the subscription. Per Zach's rule ("only proceed if I can use Gemini now on my Pro plan at no
extra cost"), this is **deferred to backlog; do NOT build now.** Antigravity CLI is also NOT being
researched now (still "work", and closed-source / no documented headless-JSON-MCP parity).

**Preserved groundwork** (so a future paid-key wire-up is fast): the provider seam (`Vendor`/`MODELS`
in llm.rs) is ready; the spike proved the gate is reproducible on gemini-cli (`tools.core: []` +
MCP-only `includeTools` + `security.disableYoloMode`/`disableAlwaysAllow`, exclude
`run_shell_command`, pin version + leak test); the token meter ships provider-agnostic so Gemini
usage lights up automatically once wired. Stage A mechanics: `gemini --output-format json` →
`{response, stats}` (no cost field — derive $ from tokens, which the meter already does).

Revisit IF: Zach gets a paid Gemini/Vertex key, OR Antigravity CLI is verified integrable.

## Context — two LLM paths (very different effort)
- **LLM seam** (`Llm::complete`/`complete_streaming`, llm.rs): audit/scan, research chat, story
  authoring, severity calibration. Provider-agnostic `match self.vendor`; only `Anthropic` wired.
  Adding Gemini = a `Vendor::Google` arm + `MODELS` entries. SMALL.
- **Gated code agents** (`claude -p` behind the MCP gateway): dev run, update-branch, PR-resolve,
  investigation. The gate spawns the agent locked to ONLY this server's `gated_write` MCP tool with
  every built-in tool disallowed (`--disallowedTools`). Claude-CLI-shaped. LARGE.

## Decisions
1. **Transport = CLI.** Shell `gemini-cli` with the user's Google login (the direct parallel to how
   Camerata uses the Claude CLI; leverages the Gemini subscription, no API key). Selected via
   `CAMERATA_LLM_BACKEND` + `CAMERATA_LLM_VENDOR=google`.
2. **Scope = full** (seam + gated agents), built in two stages:
   - **Stage A — seam (CLI):** `Vendor::Google` CLI arm in `complete`/`complete_streaming` + Google
     `MODELS` entries + JSON-output parsing. Lives entirely in llm.rs (no conflict with Phase 3a).
   - **Stage B — gated Gemini agents:** re-express the MCP gate for `gemini-cli`
     (prepare_session/render_mcp_config + gateway), keeping it EQUALLY strict.
3. **Access mechanism (unchanged):** there is NO entitlement check. `MODELS` (vendor-tagged) →
   `/api/models` → the UI picker is what's OFFERED; actual access = the CLI is logged in (CLI
   backend) or the key is set (API backend). Adding Gemini = adding `MODELS` rows + the arm; access
   stays implicit (the `gemini` CLI login).

## HARD CONSTRAINT — the gate never weakens
Stage B is **research-gated**. Before building it, verify `gemini-cli` can be locked to MCP-only with
ALL built-in file/shell/agent tools excluded (the equivalent of Claude's `--disallowedTools` +
single-MCP-tool lock). If `gemini-cli` cannot be fully restricted, gating LEAKS (the agent could
write directly, bypassing `gated_write`) — in that case DO NOT ship gated-Gemini; report the
limitation and keep gated agents on Claude. The universal gate (every agent gated, `Task` disallowed,
deny-before-write) is non-negotiable.

## Research spike questions (gemini-cli)
1. Non-interactive single-shot prompt mode (the `claude -p` equivalent) + invocation/flags.
2. Headless auth with a consumer Gemini subscription (Google login) — does it work non-interactive?
3. Structured/JSON output format (to match the generic driver's `{result, cost_usd}` shape).
4. **MCP support:** point it at a custom MCP server (config/flag).
5. **Tool restriction (the gate question):** exclude ALL built-in tools and allow ONLY a specific
   MCP tool (coreTools/excludeTools or equivalent). This determines whether Stage B is viable.

Relates to the LLM-seam (llm.rs `Vendor`/`MODELS`) and the gate architecture
([[camerata_gate_universal_enforcement]]).
