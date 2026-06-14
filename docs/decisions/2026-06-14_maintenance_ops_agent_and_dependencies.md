# Standing maintenance/ops agent + lead-engineer dependency decisions

Date: 2026-06-14
Status: Accepted (requirements); some items forward-looking
Deciders: Zach (PO), Claude (architect)

## Context

Zach added, in rapid succession, a set of related product requirements about a
published app's technical lifecycle: the user picks a look at intake, the lead
engineer owns the technical dependencies, and a standing agent owns the app's
ongoing operations (not just package upgrades).

## Decisions

1. **Intake ships a curated style kit (built).** The intake form offers a small
   curated set of color palettes plus style examples (button shape, font
   personality), and lets the user upload inspiration images for the AI to
   interpret. Every selection is captured into the onboarding document. Implemented
   as `crates/intake/src/appearance.rs` (`StylePreferences`, `SHIPPED_PALETTES`,
   `ButtonStyle`, `FontChoice`, `ImageRef`) and threaded into `IntakeForm.style` +
   `brief()`. The intake UI picker is a later wiring pass.

2. **The lead engineer decides external libraries.** Choosing dependencies is an
   engineering decision, never the user's; the user never sees a package name. The
   engineer picks them and records why in the plan. `rust-chorale` is the default for
   any tabular surface. The all-Rust rule binds Camerata's ENGINE, not necessarily
   every generated TARGET app: where a generated app's frontend genuinely needs a
   JavaScript library, the engineer may choose one.

3. **A published app gets a standing, async maintenance/ops agent.** It is the app's
   ENTIRE ops function, not merely a package updater. Remit (open-ended, expected to
   grow): dependency upgrades, security patching, key/secret rotation, certificate
   renewal, backups, health, and general ops hygiene. The agent never silently
   changes a live app. When an update matters (especially security), the user gets a
   calm, plain-language recommendation first; approving runs it through the SAME
   governed build-and-QA loop as any feature change. Maintenance is governed exactly
   like everything else: nothing changes outside the gate.

## Why this matters

The prompt-to-code tools hand over code and walk away; the rot and the debt become
the owner's problem. A governed standing ops agent gives a non-technical owner the
maintenance a real engineering team would provide. Combined with the clarification
loop and the deterministic gate, "we keep it alive and safe for you, under the same
governance" is a durable differentiator, not a feature bullet.

## Related open strategic questions (NOT decided, captured so they are not lost)

- **Per-user economics / pricing.** If real users run real apps, each app carries
  full-stack infra + database + AI-orchestration overhead + Camerata overhead, all of
  which passes to the end user. A bespoke app could be a meaningful monthly cost.
  Likely shape: tiered plans (e.g. N apps per price tier per month). This is the
  managed-PaaS endgame's business model (VISION section 20) and is explicitly NOT
  being decided now; it is recorded as the open question it is.

- **The automation-of-self reflection.** If the lead engineer eventually handles even
  third-party integrations, it abstracts away work that developers (including Zach) do
  today. The architect's answer: the person who builds and GOVERNS the system that
  builds the apps has moved up a level, not out of one. That is precisely the
  AI-orchestration-architect position this project is the proof of. Recorded as
  strategic context, not an action item.
