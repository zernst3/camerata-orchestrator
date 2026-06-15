# Demo: the commanded violation (intent-blind enforcement)

> The single most convincing demo of the gate, and the one to lead with in front of a
> skeptic. Captured 2026-06-15.

## Why a commanded violation beats a spontaneous one

The skeptic's objection to any AI-governance demo is "you staged it." If the agent
spontaneously writes a hardcoded secret and the gate catches it, the skeptic wonders:
did it really do that on its own, or was the setup rigged so it would get caught?
There is ambiguity about whether the violation was even real.

Commanding the violation defeats that objection by staging it openly. You explicitly
instruct the bad thing, on the record, in the prompt the audience watched you type,
and the gate stops it anyway. The violation is intended and unambiguous, and it still
cannot happen. That is not theater you have to defend; it is a proof the audience
watched you set up.

## The arc (do not cut it off at "denied")

1. You prompt, e.g.: "put the database query directly in the service layer." This
   violates a layering rule.
2. The agent, obeying you, generates a write that puts a DB call into a service-layer
   file.
3. The write hits the MCP tool boundary. **Layer 1 denies it before it executes**,
   before a byte reaches disk. The agent gets the denial back as a tool error.
4. The agent is now boxed: you said "service layer," the gate says no. A good agent
   resolves it the only way it can: it puts the query in the repository/data layer,
   has the service call that, and reports the constraint back: "I could not put the
   query directly in the service layer; rule ARCH-X forbids it, so I routed it through
   the repository layer instead."

That last beat is the kicker. The gate did not just stop the wrong thing, it forced
the agent into the correct architecture even though the human ordered the wrong one,
and the agent transparently said why. "Stopped, then self-corrected, then explained
the rule it hit" converts a skeptic in a way a bare denial never will.

## The principle (and the interview line)

**The gate is intent-blind.** It does not care whether a violation came from the model
being lazy, the model hallucinating, or you explicitly ordering it. It evaluates the
write, not the why. That is the entire difference between enforcement and convention:

- Convention (prompting, system prompts, the model's good manners) is intent-sensitive
  and probabilistic. You can talk a model into almost anything.
- The gate is intent-blind and deterministic. You cannot talk it out of the rule.

This demo is that sentence made physical.

## Three things to get right so it does not backfire on stage

1. **The rule must be deterministically checkable.** "Hardcoded secret literal"
   (`SEC-NO-HARDCODED-SECRETS-1`) and "raw SQL string-concat"
   (`SEC-NO-RAW-SQL-CONCAT-1`) are bulletproof regex checks. A layering rule ("DB
   query in the service layer") is fuzzier: its check needs an airtight way to
   recognize "service-layer path + DB-access pattern" so the agent cannot slip a
   phrasing past it. For the safest high-stakes demo, lead with the secret or the
   SQL-concat violation. For the most impressive one, use the layering rule, but
   harden its check first.
2. **The model must actually attempt the write, so the gate is visibly the hero.** If
   the model self-censors ("I notice this violates the architecture, are you sure?"),
   that is the convention layer doing the work and the skeptic credits the model, not
   the gate. Design the moment so the model dutifully tries to comply with the bad
   instruction and Layer 1 is the thing that stops it.
3. **Show the recovery, not just the stop.** The full arc (ordered it, tried it, gate
   denied it, agent re-routed to the compliant design and named the rule) is what
   converts. Do not end at "denied."

## What is demoable today vs what needs building

- **Demoable now (on the real gate):** the secret and SQL-concat commanded violations.
  The deterministic checks exist and are enforced, and `cargo run -p camerata -- live-demo`
  already drives a real `claude -p` agent into a denied write. The commanded framing
  just changes the prompt; because the gate is intent-blind, the verdict is identical.
  A live `claude -p` run spends tokens.
- **Needs building first:** the layering version. `ARCH-STRICT-LAYERING-1` is the
  canonical example but is NOT yet an enforced gateway arm (it is AST-level and must be
  made deterministic and hardened per caveat 1). Also worth confirming the agent loop
  continues after the Layer-1 denial so the re-route-and-explain beat actually renders
  (the current live-demo shows the stop; the full recovery arc is a slightly richer
  run).

## Suggested order

Lead with `SEC-NO-HARDCODED-SECRETS-1` (the cleanest, most universally legible: "I am
ordering you to hardcode this API key" -> denied -> agent moves it to config and says
why). Keep the layering version as the aspirational v2 once its check is hardened.
