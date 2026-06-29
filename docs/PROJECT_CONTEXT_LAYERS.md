# Soft context layers: product brief, operating principles, project memory

> **Status: DRAFT v0.1 (2026-06-29).** Design proposal. Tracking issue: #112.
>
> Origin: a conversation about why a well-context-loaded human + model pair "just gets it," and what
> repo-independent context Camerata's agents are missing. The rules are Camerata's HARD constraints
> (the skeleton). This doc designs the SOFT context (the muscle): the product brief, agent operating
> principles, and accumulating project memory that let an agent exercise good judgment INSIDE the
> constraints, the way a well-briefed engineer does.

## The gap, stated plainly

A fresh Camerata agent is handed a story + the approved decisions + the rules grounding, then runs a
bounded session and disappears. What it does NOT get, that a well-briefed human (or a model in a rich
conversation) has:

1. **The product's "why" and quality bar.** Who is this for, what does good look like here, what do
   we never compromise on. This is what lets you make a judgment call the spec did not anticipate.
2. **How a good engineer works HERE.** Confirm assumptions, prefer explicit over clever, report
   failures honestly, escalate when genuinely blocked (not to dodge judgment). Operating principles,
   distinct from any rule about the code itself.
3. **Continuity.** A human accrues context across tasks: decisions made, patterns set, gotchas
   learned. Each Camerata agent rediscovers the world from zero.

The rules cover hard, enumerable constraints. They do not, and should not, carry this softer context.
This doc proposes three first-class, per-project layers to carry it, all woven into agent grounding.

---

## Layer 1: Product brief (free-text, captured at onboarding)

**What:** A per-project free-text brief describing the product, its users, and its bar. Captured
during onboarding (a dedicated step) and editable later in project settings.

**Authoring aid (the form is scaffolded, not blank).** The textarea ships with prompts the author
fills in, so a brief is consistent and complete rather than a blank box:

```
## What is this product?
(one paragraph: what it does, for whom)

## Who uses it, and what do they care about most?
(the primary user + their top 1–3 priorities)

## What does "good" look like here? (the quality bar)
(e.g. correctness over speed-to-ship; Apple-tier UI polish; sub-100ms reads)

## Non-negotiables
(things we never compromise on, even under deadline)

## Out of scope / explicit non-goals
(what this product deliberately does NOT try to be)
```

**Shape:**
```rust
// On Project (per-project, persisted with the rest of project config).
pub product_brief: String,   // free-text, the scaffolded sections above; "" until authored
```

**How it reaches the agent:** prepended to the grounding block in every agent task prompt, under a
`## Product context` heading, ABOVE the rules digest. The agent reads the "why" before the "what."

**Why per-project free-text (not structured):** the value is the prose judgment, and projects differ
too much for a rigid schema. The scaffold gives just enough structure for consistency.

---

## Layer 2: Agent operating principles (defaulted, editable)

**What:** A per-project set of operating principles that govern HOW the agent works, seeded with a
DEFAULT set distilled from the standards Zach and Claude have converged on. Surfaced at onboarding
(pre-checked defaults the architect can edit / extend / disable) and in project settings.

**The default set (v1 proposal).** Each is one imperative line the agent is held to:

| id | Principle |
|----|-----------|
| `explicit-over-clever` | Prefer explicit, robust, readable code over terse cleverness; the cost of verbosity is paid by AI, the benefit of context is paid back at debug time. |
| `confirm-irreversible` | Confirm before hard-to-reverse or structural changes; route the decision to a human rather than auto-applying. |
| `report-honestly` | Report outcomes faithfully. If tests fail, say so with the output. Never fake a resolution or paper over a failure. |
| `match-surrounding-style` | Write code that reads like the code around it: match its naming, comment density, and idioms. |
| `escalate-when-blocked` | Stop and escalate when you hit a genuine blocking decision or a rule that calls for it. Do not guess past it; do not escalate to dodge judgment you can make. |
| `test-what-you-change` | Add tests for new behavior; keep existing tests passing; never weaken a test to go green. |
| `performant-by-default` | Reach for the performant pattern by default (no N+1, index the FK + WHERE columns, parallelize independent async). |
| `minimal-blast-radius` | Make the minimal correct change. Do not touch unrelated files or expand scope beyond the story. |

**Shape:**
```rust
pub struct OperatingPrinciple {
    pub id: String,
    pub text: String,          // the imperative line the agent sees
    pub enabled: bool,         // architect can disable a default
}
// On Project:
pub operating_principles: Vec<OperatingPrinciple>,  // seeded with the default set at create
```

Plus a free-text "additional operating principles" box for project-specific ones the defaults miss.

**How it reaches the agent:** the enabled principles render as a `## How to work here` section in the
agent role/system prompt (not the per-task prompt; this is stable across tasks). It complements the
gate: the gate BLOCKS bad writes mechanically; the principles shape judgment where no gate can.

**Relationship to rules:** principles are about the agent's *conduct*, rules are about the *artifact*.
A principle ("escalate when blocked") has no `[[option]]`/enforcement; it is prompt-level guidance. If
a principle hardens into a mechanically-checkable constraint, it graduates into a real rule.

---

## Layer 3: Project memory (accumulating, curated)

**What:** A per-project store of durable, accruing context: decisions made, patterns established,
gotchas learned. The continuity layer, so agent N+1 does not rediscover what agent N learned.

**The hard problem: who writes it, and how does it not rot or explode the context window.** Camerata's
ethos answers the first part: nothing auto-applies. So:

- **Agents PROPOSE memory entries** at run end ("I learned the auth flow assumes X"; "established the
  repository pattern for Y"), exactly as they propose decisions. A proposal is not yet memory.
- **The human curates.** Proposed entries land in a review list (reuse the decisions/NEEDS-YOU
  surface). The architect approves, edits, or discards. Approved entries become durable project
  memory. This keeps it true and tight.
- **Bounded injection.** Memory is capped for grounding: the most recent / highest-signal entries (or
  a rolling human-curated summary) are injected under `## What we have learned on this project`, with
  a hard size budget so it never crowds out the task. Older entries stay queryable but out of the hot
  prompt.

**Shape:**
```rust
pub struct MemoryEntry {
    pub id: String,
    pub kind: MemoryKind,      // Decision | Pattern | Gotcha | Constraint
    pub text: String,          // one fact, curated
    pub proposed_by: String,   // "agent:<run>" | "human"
    pub status: MemoryStatus,  // Proposed | Approved | Archived
    pub created: String,
}
// New store: MemoryStore (mirrors the existing *Store pattern), persisted to project-memory.json.
```

**How it reaches the agent:** approved entries (capped) are woven into grounding under their own
heading, after the product brief, before/with the rules digest.

**Why curated, not auto-accrued:** an auto-growing journal rots (stale entries mislead) and bloats
(every run appends noise). Human curation is the same governance principle as decision approval: the
agent surfaces, the human decides what becomes truth.

---

## How the three compose in grounding

`project_grounding()` (already the single assembly point for agent context) gains, in order:

```
## Product context            <- Layer 1 (product brief)
## What we have learned        <- Layer 3 (approved, capped project memory)
## Project rules               <- existing rules digest
[repo digest ...]              <- existing
```

and the role/system prompt gains:

```
## How to work here            <- Layer 2 (enabled operating principles)
```

One assembly point, size-budgeted, so the soft context informs every run without drowning the task.

---

## Onboarding integration

Onboarding gains two light steps (both skippable, both editable later):

1. **Product brief** step: the scaffolded textarea (Layer 1).
2. **Operating principles** step: the default set pre-checked, with edit/disable + an
   additional-principles box (Layer 2).

Project memory (Layer 3) needs no onboarding step; it accrues from the first run's proposals.

---

## Build phases (proposed)

1. **Operating principles** (cheapest, highest immediate value): the default set + the project field +
   the role-prompt injection + the onboarding step. No new store, no proposal loop.
2. **Product brief**: the project field + scaffolded onboarding/settings textarea + grounding
   injection.
3. **Project memory**: the new `MemoryStore` + the agent proposal-at-run-end + the human curation
   surface + capped grounding injection. The largest piece; the proposal/curation loop reuses the
   decisions/NEEDS-YOU machinery.

## Open questions

- **OPEN-1:** Are operating principles purely prompt-level, or do some warrant graduating into
  mechanically-enforced rules over time? (Lean: prompt-level; graduate case-by-case.)
- **OPEN-2:** Project-memory injection budget + selection: most-recent-N, human-pinned, or a rolling
  AI-summarized digest? (Lean: human-pinned + recent-N, with a summarize-on-overflow pass later.)
- **OPEN-3:** Should the product brief + principles be EXPORTABLE/IMPORTABLE with the project (like the
  ruleset), so a brief can seed a new project? (Lean: yes, they are portable project config.)
- **OPEN-4:** Do agents propose memory entries unprompted, or only when they hit something
  surprising? (Lean: only on signal, to keep proposals high-value, not every-run noise.)

## Why this matters (the thesis)

The rules make an agent SAFE. This soft context makes it GOOD: able to make the judgment call the spec
did not anticipate, the way a well-briefed engineer does. It is the highest-leverage, repo-independent
investment in "the agent just gets it," and it is the differentiator a governed, clarification-first
platform is uniquely positioned to own.
