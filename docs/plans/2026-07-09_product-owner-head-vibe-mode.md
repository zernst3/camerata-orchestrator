# Design: The Product-Owner Head + Confidence-Calibrated Vibe Mode

Status: DESIGN (pre-implementation). Author: Zach + Claude, 2026-07-09.

## Context and vision

Camerata today governs enterprise-scale development: issues, onboarding, decomposition,
design management, the deny-before-execute gate, Layer-2 checks, the integration gate, and
the governance audit trail. That substrate is verified (the gate denies at the real MCP
boundary; RUST-* rules + clippy enforce robustness; every governed action is now recorded in
`governance_events`).

The next head sits one abstraction level higher. For bespoke apps (the budget and itinerary
repos are the references), the enterprise ceremony is not warranted, but the underlying
resilience should still apply for free. The user wants to operate as **Product Owner /
Customer**, not architect: "I want an app that does XYZ", "make X do Y", "there's a bug when
I input $0". A higher orchestrator takes the architect seat; the human reviews rather than
authors. The output is a real, running app the user can see and click, with changes applied
live, and a one-word path to deploy.

The thesis that makes this safe (and different from Lovable / Bolt / v0): **governance is
what makes vibe coding trustworthy.** You watch it work AND you have an auditable record it
was built safely. Nobody else combines the governance + clarification + design polish.

### Locked decisions (Zach, 2026-07-09)
1. **Default stack = Rust fullstack** (Dioxus fullstack + Axum + SQLx + Terraform + CI).
   Rationale below: Rust's compiler/linter is an extra correctness backstop that interpreted
   stacks lack, so it raises the confidence ceiling of autonomous generation. Fullstack
   TypeScript is out.
2. **One head, not two modes.** A single chat agent (voice in and out) with one **abstraction
   dial**. Human review is ALWAYS in the loop, at every dial setting.
3. **The orchestrator is the smartest tier** (Fable 5 / Mythos). It reviews the rule corpus
   against the user's requirements and makes the option decisions itself, so the human never
   walks 100+ rules by hand. Human reviews the result, does not author it.
4. A **safe secret store** the orchestrator references by name (never pasted in chat).

---

## 1. The core principle: the dial is a CONFIDENCE THRESHOLD, not a freedom slider

The central question is "how far up can the dial go and still hold ~90% confidence, without
sliding to 70 or 50?" The answer is a reframe: **the dial does not trade confidence for
autonomy. It only automates the decisions that have a real backstop, so confidence stays
pinned by backstop coverage, not by dial position.**

Every decision the orchestrator faces is classified by its **backstop** (what catches it if
it is wrong):

| Class | Decision kind | Backstop | Dial behavior |
|---|---|---|---|
| **A. Mechanically verified** | rule-option selection, implementation, refactors, deps, scaffolding conformance | compiler + gate + Layer-2 checks + integration gate (deterministic) | Auto-decided even at high dial. A wrong pick is a suboptimal-but-SAFE pick, never an unsafe one. |
| **B. Cheaply reversible + visible** | UI/layout, copy, default values, flows | the live preview (you see it and say "change it") | Auto-decided at high dial; the preview is the loop that catches it cheaply. |
| **C. Irreversible / high blast radius** | destructive data migration, prod deploy, spending money, external side effects, secret scope, deletion | NONE that judges "was this the right call" | **Hard floor: always human-gated. The dial cannot override this.** |
| **D. Genuine intent ambiguity** | requirement unclear; a wrong guess wastes real work | clarification only | **Always clarify (clarification-first). The dial cannot override this.** |

The dial sets **how aggressively the orchestrator auto-decides A and B** (max dial: auto-decide
everything in A and B it is at least threshold-confident in; low dial: surface more of them
for you). **C and D never move, at any dial setting.**

Consequence, and the direct answer to the question: **you can turn the dial all the way up and
still hold ~90%, because "all the way up" is defined as "auto-decide everything a guardrail or
the live preview can catch, and only those."** The confidence ceiling is a property of your
backstop coverage, not the dial. Trust does not fall to 70/50 because the dial physically
cannot automate the un-backstopped classes (C, D).

### Why Rust fullstack is not aesthetic, it raises the ceiling
Improving confidence-at-max-dial == improving backstop coverage. Rust's compiler + borrow
checker + clippy move whole categories of decision out of "unverified" and into **Class A**
(mechanically caught): type errors, null/None handling, lifetimes, exhaustiveness, data-race
freedom. In fullstack TypeScript those are runtime-or-never. So Rust literally **enlarges the
safely-automatable set**, which means the dial can sit higher at the same confidence. The
language default is a lever on the trust ceiling.

### Confidence as first-class, recorded data
The orchestrator attaches a **confidence estimate + the backstop class** to each autonomous
decision, and writes it to the existing `governance_events` trail. That gives a reviewable
record of "what it decided on its own, how confident, and why it was safe." A per-session
**confidence budget**: if too many low-confidence Class-B decisions accumulate, the
orchestrator proactively raises a design-review checkpoint mid-run rather than pressing on.

---

## 2. The head (chat + voice, one dial)

A single conversational adapter over the governed core, built as another head on the headless
core / adapter ladder we just shipped (it drives the same verbs the MCP rung exposes). It is
deliberately thin:
- Free-text + voice in, voice + visual out (free-text-agent-first: structure emerges from the
  conversation, the user is never forced into a form).
- One **abstraction dial** (the confidence threshold of section 1). The head can also infer
  intent: talking in Camerata nouns ("pull issue #123, investigate") keeps the dial low
  (you are the architect); talking in outcomes ("an app that does XYZ") turns it up (you are
  the customer).
- Human-review affordances are always present: nothing in Class C/D executes without an
  explicit approve.

## 3. The Architect orchestrator (the smartest tier)

A new top-level orchestrator that takes the architect seat. Runs on the strongest model. Its
loop, all backstopped:
1. **Ingest requirements** (from the head) and clarify ambiguity (Class D) using the existing
   LeadEngineer / intake clarification machinery.
2. **Autonomous rule review.** Read the rule corpus, select the option for each rule against
   the requirements, and record the choices. This is the "don't make me walk 100 rules" ask,
   and it is **one of the safest things to automate**: rule options are pre-vetted, and the
   gate enforces whatever is chosen, so the worst case is a suboptimal-but-safe selection
   (Class A). Surface the selected set as a compact digest for human review, not 100 prompts.
3. **Propose the architecture + scaffold + story breakdown** (reuse `decompose` + design
   management). Present at a **design-approval checkpoint** (see section 6).
4. **Drive the governed fleet** to implement (the existing `dev_implement_run` + gate +
   checks + integration gate).
5. **Surface the live preview** (section 5), capture feedback, loop.

## 4. Rust-fullstack scaffolder (NEW capability)

The missing backbone. Today there is a `greenfield` onboarding hook but no generator that
spins up a new app from a sentence. Build an opinionated scaffolder that emits a new repo:
- **Stack:** Dioxus fullstack (server functions) + Axum + SQLx/Postgres + Terraform + GitHub
  Actions CI + the `camerata-governance.yml` marker. The budget-tracker repo
  (Dockerfile + infra + ci + SPEC.md) and itinerary-app (Dioxus + terraform) are the
  templates to distill.
- The scaffold ships **already governed**: the CONVENTIONS/AGENTS files and the rule set the
  orchestrator selected are baked in from commit one, so Class-A backstops are live before the
  first feature.
- Per the stack-generalized-capabilities rule: the scaffolder's orchestration is generic; the
  **Rust-fullstack template is the first pluggable stack profile** (siblings can come later).

## 5. The live running-app loop (the differentiator, and the risk)

See it -> click -> "that's a bug" -> orchestrator fixes -> the running app updates -> "deploy".
Because the apps are Dioxus, the hard part is largely solved for us:
- **Preview runtime:** wrap `dx serve` (Dioxus hot-reload) as a per-app preview process. This
  is the same toolchain Camerata's own UI uses, so we own it.
- **Feedback capture:** an element-to-report layer in the preview (click a component, describe
  the bug) that emits a structured report into the governed dev loop.
- **The loop:** report -> orchestrator (Class-A/B change) -> gate + checks -> hot reload ->
  you review live. Reversibility of Class-B changes is what keeps the dial safely high here.
- Per stack-generalized rule: the loop logic is generic; the **preview runtime is the
  per-stack pluggable piece** (a sibling of the linters/extractors). Build Dioxus first.

## 6. Human review is never removed

Three always-on checkpoints, independent of the dial:
1. **Design-approval** (after the orchestrator proposes architecture + rule selections +
   story breakdown): approve, or redirect. This is where your architecture oversight lives
   without you authoring it. Important: the guardrails cover security + conventions +
   integration, but NOT "is this the right architecture for the goal", so this checkpoint
   stays valuable and must be lightweight (approve / redirect, not re-author).
2. **Live-preview approve/redirect** (Class-B changes): you see it, you keep or change it.
3. **Irreversible-action gate** (Class C): destructive migration, prod deploy, spend, secret
   scope. Explicit human approve, always, at any dial.

## 7. Secrets (NEW capability)

Today the credential store is a **fixed allowlist** (`ALL_CREDENTIALS` + `validate_name`),
global scope, keychain-backed (`KeyringCredentialStore`). For bespoke apps the orchestrator
needs arbitrary per-project secrets (Azure key, Stripe key, a DB URL) it can reference but
never see in chat. Extend the store:
- Allow **arbitrary named secrets scoped per project** (not just the fixed LLM/GitHub
  allowlist), still keychain-backed, still masked over the wire (only `masked()` crosses HTTP).
- The orchestrator references secrets **by name**; raw values are injected at deploy/runtime,
  never surfaced to the model or the chat transcript.
- This composes with the existing gate: `SEC-NO-HARDCODED-SECRETS-1` and the secret-in-URL /
  secret-file rules already deny a secret being written into code, so a leaked value is
  caught by a Class-A backstop.

## 8. Deploy ("just handle it")

The `DeployTarget` seam exists with a working Azure plan generator (`az webapp` + resource
group). Fill in: credential-store-driven `terraform apply` + the Azure deploy, behind the
Class-C human gate. "Deploy" becomes a one-word command that runs the vetted plan with the
per-project secrets injected.

---

## What exists vs. what is new (honest reuse map)

| Piece | Status |
|---|---|
| Governed core, gate, checks, integration gate, audit trail | EXISTS, verified |
| Headless core + adapter ladder + MCP verbs (head plugs in here) | EXISTS (just shipped) |
| Intake / LeadEngineer clarification, decompose, design mgmt | EXISTS |
| Rule corpus + option selection | EXISTS (selection is manual today) |
| Credential store (keychain) | EXISTS, but fixed allowlist + global scope |
| Deploy seam + Azure plan | EXISTS (partial), needs apply + creds wiring |
| Greenfield onboarding hook | EXISTS (not a full scaffolder) |
| **Confidence-calibrated dial + decision-class routing** | NEW |
| **Architect orchestrator (autonomous rule review + arch proposal)** | NEW |
| **Rust-fullstack scaffolder** | NEW |
| **Live-preview runtime + feedback loop (Dioxus)** | NEW (biggest unknown) |
| **Arbitrary per-project secrets** | NEW (extend existing store) |
| **The chat/voice head** | NEW (thin) |

## Build sequence (spike the risk first)

1. **Spike the Dioxus live-preview loop** end to end on ONE existing app (budget or
   itinerary): `dx serve` preview + element-to-feedback + one governed round-trip that
   hot-reloads. This is the differentiator and the biggest unknown; prove the magic first.
2. **Decision-class router + confidence recording** into `governance_events` (the dial's
   engine), tested against the existing gate/checks so Class-A/B/C/D routing is real.
3. **Architect orchestrator**: autonomous rule review (digest for approval) + arch proposal +
   design-approval checkpoint, driving the existing fleet.
4. **Rust-fullstack scaffolder** (distill budget-tracker + itinerary into a stack profile).
5. **Per-project secrets** + **deploy apply**.
6. **The chat/voice head** wrapping it all (thin, last).

## Aesthetic and UI identity (decisions marked 2026-07-09)

The chat/PO head is a SECOND head that lives **alongside** the current cockpit UI, not a
replacement. Both are heads on the same governed core / adapter ladder; the enterprise
cockpit stays for architect-mode work.

- **Aesthetic carries over.** The visual identity is shared, deliberately: same look, and
  specifically the **Bombe machine background** from the current UI carries into the chat
  head. It should feel like the same product, one tier simpler.
- **Radically simpler surface.** The chat head is essentially a chat app (voice + text) with
  only a few buttons and a couple of small menus (for example project selection). No dense
  panels, no rule tables, no fleet dashboards. Chat-first; complexity stays behind the head.
- **Design goal: the iPhone move.** Take a powerful, complex system and present a drastically
  simplified surface while keeping the identity intact, the way the iPhone simplified the
  smartphone UI. The power is undiminished underneath; the surface is calm.

These are small decisions but they are load-bearing for the "you are the customer, not the
operator" feel, so they are marked now to guide the head's implementation.

## Open questions (for Zach)
- Dial UX: a literal slider, an inferred level, or a per-request "how sure should you be
  before asking me" phrasing? (Recommend: inferred + overridable.)
- Design-approval granularity: one approval per app, or per story/epic? (Recommend: per
  epic, with a fast-path for trivial changes.)
- Voice: on-device vs. cloud STT/TTS, and does the audit trail record the voice transcript?
- Preview for non-web Dioxus targets (desktop/mobile) later, or web-only to start?
