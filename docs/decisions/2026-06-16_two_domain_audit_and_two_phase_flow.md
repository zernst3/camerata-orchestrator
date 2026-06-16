# Two-domain audit (enforced vs advisory) + two-phase brownfield flow

Status: accepted (2026-06-16). Built, with the staged parts called out explicitly below.

## Context

A cold answer-key run (budget-tracker-testrepo) exposed real gaps and clarified the
model. Two truths fell out:

1. The deterministic layer and the AI layer are **different epistemic objects** —
   high-confidence mechanical hits vs lower-confidence semantic findings — so they must
   differ in **authority**, not just appearance.
2. The audit ran one fixed pass and never scanned against the rules the architect picked.

## Decision 1: two domains, mapped to authority

- **Enforced (deterministic rules).** High-confidence, gateable, eligible for auto-fix.
  Secrets / raw-SQL / secret-URLs / secret-files / path-escape. These can block.
- **Advisory (AI investigative).** The agent thinks something's worth a look. Review-only:
  **never auto-blocks work or auto-fixes without a human confirming.** Surfaced, labeled
  ("AI · advisory"), and grouped apart from enforced findings. The over-flag risk (a
  vacuous single-user "vuln", a negligible timing residual) is exactly why advisory
  findings must not carry enforcement authority.

This is the enforcement-vs-convention thesis as a product feature: separate what you can
mechanically enforce from what still needs the architect. The label tells the user; the
authority protects them.

The virtuous loop (staged — see BACKLOG): when an advisory AI finding is verified as real
and generalizable, codify it into a deterministic rule — it graduates from advisory to
enforced. Convention discovers; enforcement locks it in.

## Decision 2: two-phase brownfield flow

- **Phase 1 — DETECT** (`/api/onboard/scan`): fetch + stack-detect + PROPOSE a starter
  ruleset. No code audit yet.
- **Architect picks** rules + alternatives (grouped-by-domain table, click-row modal).
- **Phase 2 — AUDIT** (`/api/onboard/audit`): the deterministic security rules are the
  always-on enforced floor; the AI audit is **parameterized by the selected rules'
  directives** (it checks the code against what the project adopted) and produces advisory
  findings + an investigative pass.

## Precision fixes (this run's evidence, now closed)

- **Deterministic layer was silent on all three Tier-1 plants.** Fixed: whole-file
  matching (`content_match_lines`) so multi-line `format!` SQL is caught; broadened regexes
  (named `{user_id}` args; a `*_KEY`/`*_SECRET`-const long-literal heuristic for
  provider-agnostic keys; templated-URL token params). Tested against the real plant
  shapes + FP guards. STILL heuristics, not AST — they cover the planted cases and near
  neighbours, not "all injection".
- **AI line numbers were wrong** (3/4). Fixed: the digest is now line-numbered (`NNNN| `)
  and the prompt cites those.
- **AI over-flagged.** Fixed: an adversarial-verify (refute) pass drops vacuous/theoretical
  findings and recalibrates severity for the app's context, fail-open on model error.
- **The wrong engine ran the deterministic rules.** Fixed: `audit_repos` ROUTES BY ENGINE
  — a rule with a deterministic gate arm runs only through `audit_files` (real code) and is
  STRIPPED from the LLM prompt; only semantic rules (no arm) reach the model. Fuzzy
  keyword-matching a deterministic rule was the flood; that path is closed.
- **The audit's `claude -p` was a full agent.** Fixed: it now runs as a pure completion
  (`--strict-mcp-config` + `--disallowedTools` for every built-in) — no MCP servers, no
  Task/Explore sub-agents, no filesystem tools. It just reasons over the digest in the
  prompt. (Observed live: `num_turns:1`, ~1.2s for a trivial prompt.)

## Honestly staged (NOT yet built — see BACKLOG.md)

- **Per-row rule selection drives the audit.** Today Phase 2 parameterizes the AI with
  ALL proposed rules' directives, not the per-row picked subset. The modal/table selection
  isn't yet lifted into the audit call.
- **Advisory AI in the DEVELOPMENT path.** The two-domain split should also run during a
  governed dev run (deterministic gate enforces; AI reviews the produced diff as advisory,
  non-blocking). The seam is `ai_audit`; wiring it into the live-fleet completion is staged
  (and gated on live runs, which are opt-in).
- **Live scan feedback.** The scan awaits the AI synchronously; a live prompt/output
  feedback surface needs the transcript store wired to the scan path + a background job.
- **Rule-discovery loop** (advisory → codified deterministic rule).

The single most valuable property here is that this file tells you exactly where the
mechanism ends and the staged work begins — and the code backs every "built" claim with a
test.
