# Camerata enforcement model — the checks, the layers, and the single source of truth

> What gets checked, by what, when, and whether you can turn it off. The plain-language
> reference for users **and** the canonical model for the build. Companion to the governed-dev
> design (`docs/plans/2026-06-25_*`).

## The one-sentence story

**You define your rules once (the SSOT). Camerata enforces them at every checkpoint between
writing code and merging it — locally before you push, and remotely in CI — from the *same*
rules. Above all of that, the lead orchestrator guarantees the multi-repo pieces fit together
(contracts). The catch: the layers consume the SSOT **in different ways and at different times**,
so they *can* drift — see "Why the layers drift" below.**

![Camerata enforcement model — rule sources feeding the check layers](enforcement-model.svg)

## Two ideas first (so the rest stays simple)

1. **Verification vs. integration are different jobs.**
   - **Verification** — "is this code good?" — done by the **check layers** below, **per repo**.
   - **Integration** — "do the repos fit together?" — done by the **orchestrator** (contracts),
     **across repos**, always on.
   Hold these apart and the rest stops being confusing. Contracts are deliberately **not** a
   "layer."
2. **Every check is one of two kinds.**
   - **Deterministic** — code checking code (security gate, lint/build, CI). Fast, free, exact.
   - **Judgment** — reasoning over code (an AI reviewer, or you). Catches what deterministic
     checks can't (architecture, intent, taste). Costs tokens (AI) or time (you).

## The four check layers (verification)

| # | Layer | Checks for | Kind | When | Scope | Optional? | Fed by |
|---|-------|-----------|------|------|-------|-----------|--------|
| **1** | **Security** | dangerous/forbidden writes (deny-before-write) | Deterministic | **before** the write lands | per-repo (worktree jail) | **No** — core gate | gateway ruleset |
| **2** | **Mechanical** | lint · structure · build · format | Deterministic | after a task, **local** | per-repo | **No** — fail-closed | the **wired set** — mechanical (wire it) + architectural (define per spec, *then* wire); that set is the SSOT |
| **3** | **Code review** | architecture · quality · rule-spirit · **meets the story** | **Judgment** — AI reviewer (model selectable) **or you** | after a task, **local** | per-repo | **Opt-in** (token cost) | the **story** (reqs/contract/integrations) + **ALL rules as prose** + the diff — **not** other agents' contexts |
| **4** | **Origin** | CI · required reviews · deploy gates | Deterministic + maybe judgment + human | **remote**, on PR | per-repo | **user-owned** (Camerata generates, you maintain) | L2's checks mirrored to CI **+ origin extras + optional user-built AI reviewer (≠ L3)** |

**What L3 sees — and doesn't.** L3 reads **the story (requirements, contract, integrations) +
the selected rules + the diff**, and judges the code against **both the rules and the story's
intent** — exactly as a human reviewer reads the ticket before the diff. It does **not** see
**any other agent's context** (the investigation, developer, or orchestrator transcripts). That
isolation — **from the other agents, not from the story** — is what keeps it from rubber-stamping
the implementer's own rationalizations. Spec-grounded, implementer-blind.

> **Numbering note (reconciled).** This is the canonical **stage** model:
> **L1** Security · **L2** Mechanical · **L3** Code review (AI) · **L4** Origin/CI. The docs
> (TECHNICAL.md, USER_GUIDE.md, ENFORCEMENT.md, README.md) are now reconciled to it: every prose
> reference that means **CI** reads **L4**, and **L3** is reserved for the AI code reviewer. The
> ONLY remaining legacy is the **code** flag name `layer3_only` (`Rule::is_layer3_only()`), which
> means **CI-only (L4)** despite its name; it is intentionally NOT renamed to avoid a corpus-wide
> code migration, so a reader who greps the source for "layer3" finds the CI tier, not the AI
> reviewer. (Separately, `crates/ui/src/chat.rs` uses "Layer 3" for an unrelated chat-grounding
> concept, and `crates/server` uses "Layer 3 — API contract" for the run-liveness subsystem —
> neither is an enforcement stage.)

## Rule types & the wiring cost (the part that's easy to miss)

"Fed by rules" hides a real distinction. There are **two rule types**, and **neither runs in
L2/L4 until you wire it in** — they do not run automatically:

- **Mechanical rules** — the deterministic check logic already exists (each maps to a **named
  linter rule**). But it is **dormant until wired** into the pipeline (`.camerata/checks.toml` +
  generated CI). **One step: wire it in.** ("Out of the box" only means the *checker* exists —
  not that it runs.)
- **Architectural rules** — machine-decidable in principle, but **no off-the-shelf checker
  exists**. **Two steps: (1) define/build the check per the *application's own specs* (via a
  story), then (2) wire it in.** The definition step is the extra cost mechanical rules don't
  have.

**Both require wiring; architectural also requires definition first.** And critically: **the
rules you actually wire in — not the whole corpus — are the SSOT for L2 and L4.** Wire a subset
and that subset is what L2 enforces locally and L4 enforces in CI.

The contrast that drives everything: **L3 needs neither step.** It reads **any** rule as prose
the moment it exists — a brand-new architectural rule is enforceable by L3 immediately, while
L2/L4 can't touch it until it's defined and wired. That lag is not a bug — it's the cost of
turning judgment into a deterministic gate — but it *is* where confusion (and drift) live.

## Why the layers drift (and from what)

The SSOT binds L2/L3/L4 together, but **in different ways, so they drift for different reasons:**

- **L2 ↔ L3** — a new architectural rule works in L3 (prose) immediately, but L2 can't enforce
  it until a story mechanizes it. They're out of step *by design* until the dev work lands.
- **L2 ↔ L4** — Camerata generates origin CI from L2, but **you maintain origin.** Let it lapse
  and local and remote diverge.
- **L4 alone** — origin can carry **extra checks beyond the SSOT** and **its own AI reviewer,
  which is a different thing from L3**: L3 ships *in* Camerata; an L4 reviewer is **entirely
  user-built — Camerata does nothing to set it up.** L3 is best thought of as the cheap *local
  preview* of whatever origin also enforces (shift-left), not a guarantee origin matches it.

## Integration — the orchestrator's job (not a layer)

- **Contract existence** — for boundary-crossing work the orchestrator **requires a prose
  contract before development** and pushes back without one. **Non-negotiable.**
- **Contract coherence** — the orchestrator validates that the assembled multi-repo result
  honors the contract, as its integration duty. **Non-negotiable, and independent of L3.**
- If L3 *is* enabled it adds an **independent** architectural review on top (its rule-list can
  include contract-related expectations) — **additive**, never the thing contracts depend on.

## What you actually choose (the entire decision surface)

**Per project (settings):**
- Your **rules** (the SSOT) — drive L2 / L3 / L4.
- **AI code reviewer (L3): on / off**, and **which model** runs it.

**Per unit of work:**
- **Repos + branches in scope** (Intake).
- Whether *this* story needs a contract — the orchestrator decides and asks.

Everything else runs automatically.

## How the UI should show it — one pipeline, not four concepts

Don't make users hold a four-layer taxonomy. Show **one escalating pipeline** with live ticks,
cheapest-first:

```
◉ security ✓ · checks ✓ · AI review ✓ (or "you ▸") · ⤴ pushed · CI ✓        [ Integration ✓ ]
```

- A failure stops the line and bounces with the reason.
- "AI review" shows the selected model, or **"you"** when L3 is off.
- "CI" is your origin pipeline; it deep-links to the PR.
- A separate always-present **Integration** badge (contracts) shows on multi-repo stories.

The four-layer model lives in this doc; **the user sees a single gate that escalates from
free-and-fast to smart-and-careful, then hands off to their own CI.**

## Simplifications adopted (the answer to "how do we make this less confusing")

1. **Layers are internal; users see one pipeline.** (above)
2. **The SSOT collapses L2≈L4** into one idea: "your rules, enforced before push and again in CI."
3. **Two kinds only** — deterministic vs. judgment — instead of enumerating mechanisms.
4. **Integration ≠ verification.** Contracts are the orchestrator's promise, kept out of the
   layer taxonomy so "is the contract L3?" never has to be asked.
5. **One real toggle** — the AI reviewer (on/off + model). Everything else is automatic or
   rule-driven.
