# Adding a rule to Camerata's shipped corpus: a developer guide

> Audience: the Camerata developer (you), adding or changing a rule in `crates/rules/principles/`.
> This is NOT the end-user "how do I wire a mechanical rule in my repo" doc. This answers: when I add
> a rule to the SHIPPED corpus, what do I have to consider BEYOND writing the TOML, because for some
> rule kinds the TOML alone does nothing.

## The one idea to internalize

**The TOML's `enforcement` field is a CLASSIFICATION, not the enforcement.** Writing
`enforcement = "architectural"` does not make Camerata enforce anything. The corpus is data; a rule
only *does* something through one of these channels:

1. **Grounding** (free, automatic): every SELECTED rule's text flows into the agent's prompt. This is
   how prose rules have effect. You get it for free by adding the TOML.
2. **Emission** (free, automatic): structured rules emit into `CONVENTIONS.md` / `AGENTS.md`.
3. **A mechanical gate/check** (NOT free): a rule is only mechanically blocked if a detector for it is
   wired into an enforcement layer (the layer-1 gateway, or a layer-2 check). Adding an "architectural"
   rule to the corpus does NOT create that detector.
4. **Escalation** (partly free): a rule that calls for escalation works agent-driven with zero code
   once you add the field; a deterministic backstop is optional and needs code.

So the real question for any new rule is: **which channel(s) do I need, and which require code?**

---

## What every rule MUST have

```toml
id = "DOMAIN-SHORT-NAME-N"     # stable, traceable, UPPER-KEBAB + trailing -N
title = "One-line human title"
enforcement = "prose" | "structured" | "mechanical" | "architectural"
domain = "agentic"             # also derived from the folder; keep them agreed
```

- **`id` is forever.** It is referenced by selections, grounding, role subsets, code, and the user's
  saved project config. Renaming an id silently breaks existing projects. Don't.
- **`enforcement`** classifies the rule (see channels above). Pick honestly; it drives emission +
  preview, not magic enforcement.

## What a rule CAN have

```toml
default = true                 # ships an adopted default option (else the architect MUST choose)
qualifies = "…"                # the summary / when-it-applies prose (falls back to decision.why, then title)
verification = "draft" | "policy" | "grounded" | "verified"   # the provenance ladder (absent -> draft)
opt_in_only = true             # never auto-recommended / pre-checked at onboarding
layer3_only = true             # CI-tier only; never run at layer-2 or scan time

[decision]
question = "What position does the project take on …?"
default  = "the-default-option-id"
why      = "rationale for the adopted default"

[[option]]
id        = "stable-option-id"   # ALSO forever — referenced by selections + code
label     = "Human label"
directive = "the concrete behavior this option codifies"
why       = "rationale"
# Optional, the escalation hook (see below):
escalation = { condition = "…", severity = "hard-pause" | "soft-flag" }

[[sources]]
url   = "…"
title = "…"
linter = "…"                   # optional, the tool that enforces it externally

[verified]                     # present only when a human confirmed it
# who/when/versions-verified-against, for the staleness pass
```

Notes:
- **Option ids are forever too** (same reason as rule ids).
- **`verification`**: a `draft` rule is grounded-but-unproven. `policy` is the honest label for a rule
  that encodes deliberate project/team policy with NO external authority — any cited source is
  internal (a Camerata doc citing Camerata, e.g.), not a published standard or real linter rule.
  `policy` is NOT grounded: like `draft`, it is kept out of the armed ruleset, but it is labeled
  honestly instead of being mismarked `grounded` just because it cites *something*. Use `policy`
  instead of `grounded` whenever you can't point at a real external authority. `grounded` rules are
  eligible for auto-recommend at onboarding (unless `opt_in_only`). `verified` carries human provenance.

---

## The id conventions that carry meaning

- **`SEC-*` / `ARCH-*`** are the **security floor**: always-on, hard-deny, enforced at layer 1. They
  are distinguished by the id prefix (and `enforced_gate_rules()` / `governed_role` inject them into
  every role's subset). If you add a floor rule, it must have a real layer-1 detector (see below) —
  a floor rule with no detector denies nothing.
- **`GOV-1`** is the governance meta-rule, always in scope.
- Domain prefixes (`AGENTIC-`, `RUST-`, `ORCH-`, `CICD-`, `UI-`, …) group by area + map to the folder.

---

## The decision that actually matters: which channel, and does it need code?

### A. Pure prose / structured rule (the common case) — NO code

The rule guides the agent through grounding (and emits to CONVENTIONS/AGENTS for structured). Write the
TOML, done. The agent reads it; there is no mechanical block. Use this for judgment-shaping guidance
that you do NOT need to hard-enforce.

### B. A GATED rule (mechanically blocked) — NEEDS CODE

If you want a write or a change to be **mechanically blocked**, the corpus TOML is not enough. You must
wire a detector into an enforcement layer:

- **Layer 1 (the gateway, pre-write deny):** add the rule's matcher to the gateway's rule registry
  (`crates/gateway/src/`), where a tool call's path/content is matched and a `Deny { rule, reason }`
  is returned. This is how `SEC-*`/`ARCH-*` actually block.
- **Layer 2 (post-task checks):** add a check to the toolchain runner (`crates/checks/`,
  `runner_for_worktree`) that inspects the worktree after the agent runs and reports a violation that
  bounces the agent.

The `enforcement = "mechanical" | "architectural"` classification documents your intent, but the block
only exists once the detector exists. **A new architectural rule in the corpus with no detector is
advisory-via-grounding only.** Say so honestly in `qualifies` if Camerata does not ship the check.

### C. An ESCALATION rule (pause for a human) — the canonical "TOML plus a little"

This is the example that motivated this guide. An escalation rule pauses the governed run for human
review instead of denying (the agent stops and asks) or instead of merely advising.

**Escalation is OPTION-SCOPED.** It lives on the `[[option]]` that calls for it, not the rule, so a
rule can offer an escalating option alongside non-escalating ones. Selecting a non-escalating option
does NOT escalate.

```toml
[[option]]
id = "escalate-before-doing-X"
label = "Escalate before X"
directive = "…"
why = "…"
escalation = { condition = "the change would do X", severity = "hard-pause" }

[[option]]
id = "allow-X"
label = "Allow X"
directive = "…"
why = "…"
# no `escalation` -> choosing this option does NOT escalate
```

What you get, and what (if anything) you must wire:

1. **Agent-driven enforcement is AUTOMATIC — zero code.** When a project selects an option that
   carries an `escalation`, the implementer agent is grounded with its `condition` (under an
   `## ESCALATION CONDITIONS` section) and given the `raise_escalation` tool. When the agent's work
   meets the condition it calls the tool; the server resolves severity AUTHORITATIVELY from your spec
   (the agent cannot downgrade a hard-pause) and either pauses the run for human review (hard-pause:
   checkpoint + a UoW review escalation + `AwaitingReview`, resolvable Approve/Amend/Reject) or logs
   it and continues (soft-flag). **So a brand-new escalation rule needs only the TOML option field.**

2. **A deterministic backstop is OPTIONAL and needs code.** If the condition is mechanically
   detectable and you want a safety net that does not rely on the agent self-reporting (e.g. the
   test-tamper diff scan), you write the detector + wire it. The canonical example: `test_tamper.rs`'s
   `detect_test_tampering` runs post-task; `test_tamper_escalation(corpus, selections)` resolves the
   rule's active spec (so the backstop is also field-driven, reading YOUR `escalation` spec rather
   than hardcoding option ids); the run branches on severity exactly like the agent-driven path. Only
   the test-tamper rule has a backstop today; most escalation rules are agent-driven only, and that is
   fine.

3. **`severity` is your call:** `hard-pause` (stop and wait for a human, the safe default for
   guard-area conditions) vs `soft-flag` (log a warning and let the run continue). Absent → hard-pause.

**Escalate vs deny:** if a rule should push the agent to FIX something rather than ask a human, it is a
deny/bounce (channel B), not an escalation. No `escalation` field; wire a gate/check. Escalation is
specifically "a human decides."

---

## The checklist for adding a rule

1. **Id + title + domain + enforcement** filled, id is new + stable, domain agrees with the folder.
2. **Decide the channel:**
   - Guidance only → prose/structured, write the TOML, done (channel A).
   - Must be mechanically blocked → write + wire a layer-1/2 detector (channel B). The TOML alone does
     not block.
   - A human should decide when a condition is met → add `escalation` to the relevant option(s)
     (channel C). Agent-driven works immediately; add a backstop detector only if you want one.
3. **If it is a floor rule (`SEC-`/`ARCH-`):** it MUST have a real layer-1 detector, and confirm it is
   injected via `enforced_gate_rules()` / `governed_role`.
4. **Options:** stable ids; if `default = true`, the `[decision].default` points at a real option id.
5. **Honesty in `qualifies`:** if Camerata does NOT ship the mechanical check for this rule, say that
   the coverage is grounding/advisory (don't imply enforcement that isn't there). See the test-tamper
   rule's `qualifies` for the model.
6. **Verification/sources:** set `verification` truthfully; add `[[sources]]` if grounded; only add
   `[verified]` when a human actually confirmed it.
7. **Tests:** a gated rule needs a detector test; an escalation rule's option-scoping is covered by the
   generic `selected_escalation` test, but add a rule-specific one if it has a backstop.

## Where the moving parts live (quick map)

| Concern | Location |
|---|---|
| Rule TOML | `crates/rules/principles/<domain>/<id>.toml` |
| Rule struct / loader / `selected_escalation` | `crates/rules/src/lib.rs` |
| Layer-1 gate (deny) detectors | `crates/gateway/src/` |
| Layer-2 checks | `crates/checks/` (`runner_for_worktree`) |
| `raise_escalation` tool (agent-driven) | `crates/gateway/src/main.rs`, `crates/agent/src/lib.rs` |
| Escalation grounding + sink read + handler | `crates/server/src/dev_implement_run.rs` |
| Test-tamper backstop (the one detector-backed escalation) | `crates/server/src/test_tamper.rs` |
| Floor injection | `enforced_gate_rules()` / `governed_role` (`crates/fleet/src/`) |

## The honest summary

Most rules are TOML-only (grounding does the work). A rule that must be *blocked* needs a detector you
write and wire. A rule that should *pause for a human* needs only an option `escalation` field for the
agent-driven path, plus an optional detector if you want a deterministic backstop. Classifying a rule
`architectural` in the TOML does not enforce it; only a wired check does. When in doubt, write what is
true in `qualifies` so the rule never over-promises.
