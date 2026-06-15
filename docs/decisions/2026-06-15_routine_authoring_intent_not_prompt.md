# Routines are authored from INTENT; the AI writes the operational prompt

Date: 2026-06-15
Status: Accepted. Model + UI reshaped; live AI authoring is Claude-gated.
Deciders: Zach (architect), Claude (architect)

Companion docs: [`routine_dashboard`](2026-06-15_routine_dashboard.md),
[`CONSUMER_UX.md`](../CONSUMER_UX.md) (intake → clarify → lead engineer),
[`brownfield_onboarding_flow`](2026-06-15_brownfield_onboarding_flow.md) (propose → approve).

## The correction

The routine form treated the text the user types as the **literal prompt** sent to the
agent. That is backwards. The user should describe **what they want the routine to do**
(intent, in plain language). The **lead-engineer AI** then authors the actual operational
prompt — model tiering, directive structure, efficiency rules, scope, output contract —
and the user **reviews/edits** it before it is saved and run.

This is exactly how Zach operates with Claude here: he states intent; the AI fleshes out
the implementation detail. Camerata should mirror that, because it is already the whole
philosophy of the product everywhere else:

- **Intake → clarify → lead engineer** turns a consumer's description into a real spec.
- **Brownfield** proposes a ruleset the architect approves; the user does not author rules.
- **Decomposition** proposes child stories the architect reviews before commit.

Routine authoring was the one place still asking the human to write the machine's input
directly. This ADR aligns it: **describe intent; the AI authors; you approve.**

## Decision

A routine carries two distinct fields:

- **`intent`** — the user's plain-language description ("nightly, scan dependencies for
  advisories and open governed PRs for safe upgrades"). This is what the user writes.
- **`prompt`** — the OPERATIONAL prompt the agent actually runs, **authored by the
  lead-engineer AI from the intent** and then human-reviewed. It encodes the things the
  user should not have to hand-write: model tiering, directive phrasing, the governance
  framing, scope, and the output contract.

Flow: **describe intent → AI drafts the operational prompt → review/edit → save.** The
draft step is an AI step (it needs Claude), the same Claude connection the governed fleet
uses. The "model tiering, etc." judgment is precisely what the AI owns; a human picking
the model per task is the anti-pattern this removes.

## Honest current state

- Model + UI are reshaped: the form captures **intent**; a "Draft operational prompt" step
  produces the reviewable `prompt`; both are stored; the table shows the intent.
- The draft step has a **deterministic scaffold** fallback so the flow is usable with no
  Claude connected (it wraps the intent with the standard governance/scope framing and
  marks model tiering as the lead engineer's call). The **real AI authoring** (the lead
  engineer writing a fully-specified prompt with chosen model tiers) activates when Claude
  is connected — that is the build behind `POST /api/routines/draft-prompt`, flagged
  `authored_by: scaffold | claude`.
- Never silently send the raw intent as the prompt: it is always passed through the draft
  step (scaffold or AI) and surfaced for review first.

## Why this matters beyond routines

The same intent→author→approve shape is the reusable primitive: a user describes an
outcome, the lead-engineer AI authors the machine-facing artifact (a prompt, a ruleset, a
decomposition), and the human approves. Routine authoring is just the latest surface to
adopt it; the draft-prompt authoring step should be factored so other "describe, don't
hand-write" surfaces can reuse it.
