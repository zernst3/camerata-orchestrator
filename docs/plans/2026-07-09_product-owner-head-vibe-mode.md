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

## Hosting and multi-tenancy (cloud model, staged after local-first)

Vision (Zach, 2026-07-09): the builder chat is eventually a **hosted, authenticated,
multi-tenant service**. Friends log in securely and build their own apps. This is a governed
cloud IDE (prior art: Cloud9, Gitpod, StackBlitz, Replit: per-workspace containers with a
tunneled live preview). The headless / stateless-core design is precisely what enables the
clean service split.

**Service topology:**
- **Chat/PO head** = its own deployed UI service, per-user authenticated sessions.
- **Governed core** = one stateless app service (logic stateless; per-tenant state in a
  backend). Replaces the local single-user stores (projects.json, keychain, SQLite) with a
  multi-tenant, auth-scoped persistence layer (Postgres, tenant-scoped rows).
- **Per-session build environment** = an ISOLATED container per user workspace: the app repo,
  the governed fleet (`claude -p`), and `dx serve` all run there. Isolation is mandatory
  (multi-tenant AI code execution). This is the biggest infra lift.
- **Live preview** = the container's `dx serve` exposed as an authenticated per-session
  preview URL (reverse-proxy / tunnel), rendered in the UI. Device-agnostic (it is a URL).
- **Auth** = an identity layer (Azure Entra External ID / B2C, or similar), per-user tenancy.
- **Secrets** = per-tenant cloud vault (Azure Key Vault) replacing the local keychain. Users
  store their own cloud creds; Camerata deploys their apps to THEIR infra (the existing
  BYO-infra DeployTarget model fits directly).

**The gate is the enabler.** Multi-tenant cloud AI code execution is only safe because of the
deny-before-execute gate + container isolation + egress control. The gate is the moat here
even more than in the enterprise case.

**Remote control (Claude-Code-style):** falls out of the hosted + authenticated model. Hosted
sessions are reachable from any device, so "start a build on desktop, monitor/steer from
phone" is just accessing your session. No separate mechanism needed.

**Mobile:** the preview is an authenticated URL, so it renders on any device. Realistic usage:
from a phone you build mobile-first apps (chat + a small preview pane); from a computer you
have room for chat + a full preview. The simplified chat head (Aesthetic section) is
mobile-friendly by design.

**Staging (important):** build LOCAL-FIRST single-user first (the 2026-07-09 spike is that
foundation), prove the PO head + live-preview + orchestrator on one machine, THEN lift to
cloud (containerize the build env, add auth / tenancy / vault / preview-tunnel). Do not build
multi-tenant cloud before the local loop is proven. "Right now it is only me" means local is
the correct near-term; cloud is the shape to design toward.

## Stack refinement from the spike: Dioxus FULLSTACK (server functions), not wasm + reqwest

The live-preview spike surfaced that itinerary-app used `reqwest` directly in frontend code,
which is native-only and breaks the wasm/web build. The default stack should therefore be
**Dioxus fullstack with server functions**: network / DB I/O goes through server fns on the
backend, keeping the wasm frontend clean. This is both more correct and a Class-A backstop
target (the gate/checks should reject reqwest-in-wasm-frontend for the default stack), which
reinforces the section-1 thesis: the more the default stack pushes I/O through
compiler-checked seams, the higher the safe dial sits.

## Live-preview adapter requirements (from the spike verdict: VIABLE)

Measured: RSX/text edit hot-patches in ~1s; a Rust logic edit incremental-rebuilds in ~4-5s
steady-state (~19s cold). `dx serve` survives compile errors and reports them. The
`crates/preview` adapter (local first, container-hosted later) must:
- Manage `dx serve` as a subprocess (reuse the `crates/ui/src/server_process.rs` lifecycle:
  health, kill-on-exit).
- Parse dx output for build/reload/error status (`Hotreloading:`, `Build completed`, rustc
  errors).
- **Handle the silent-ignore gap:** a syntax-invalid edit is dropped by dx's RSX pre-pass with
  no error line. The adapter needs a post-edit timeout + `cargo check` fallback to detect a
  broken fleet edit instead of serving stale content silently.
- Surface the preview URL (local: localhost; cloud: an authenticated tunnel) and hook the
  click-to-report layer to the running preview.
Full evidence: `docs/spikes/2026-07-09_dioxus-live-preview-spike.md`.

## Deployment: the DevOps abstraction ladder (Zach's big unknown)

Honest current state: `DeployTarget` + `AzureWebAppTarget` exist, but `deploy()` only BUILDS
the command plan (`az group create` + `az webapp up` + show) and returns `Pending`. It does
NOT execute (needs the user's Azure creds wired). No DB provisioning, no GitHub automation,
and the greenfield hook creates a LOCAL-ONLY repo. So deployment is designed, not wired.

"Just say deploy, get a URL" decomposes into five things Camerata must own: (1) a source of
truth (repo), (2) build (Rust release + wasm), (3) provision (compute + DB + networking),
(4) deploy (push artifact, run migrations, inject secrets), (5) return the URL and manage
lifecycle (redeploy, rollback, teardown). The decisions:

**D1. Repo management.** Recommend: Camerata AUTO-CREATES a GitHub repo per app under the
user's account (one-time token, already how Camerata works). Repo = source of truth + CI/CD +
durability + history. Extends today's local-only greenfield to create+push. (Alt: local-only
or BYO-repo; not recommended.)

**D2. Hosting target = the DevOps abstraction dial (three rungs):**
- **Rung 1 (Zach, near-term): BYO-Azure.** Deploy to the USER's Azure; creds stored once in
  the vault; "deploy" runs the (currently unexecuted) plan with their creds and returns their
  URL. WIRE THIS NOW.
- **Rung 2: BYO-any-cloud** via the DeployTarget seam (AWS/GCP/Fly/Render as pluggable
  siblings).
- **Rung 3 (product extreme, "dev-team-as-a-service"): Camerata-managed hosting.** Camerata's
  own platform hosts it; user needs no cloud account, just gets a URL (the Vercel/Railway
  model, on the multi-tenant cloud). Beyond Zach's personal need; the seam keeps it open.

**D3. Provisioning depth.** `az webapp up` (compute only, simplest) vs terraform full stack
(app service + managed Postgres + networking, matching the reference apps' terraform).
Recommend: terraform full stack by default so "deploy" yields a working DATA app, not just
compute. The reference apps' terraform is the template.

**D4. First-deploy vs redeploy.** Recommend: Camerata-direct for the first provision
(terraform apply, Class-C, gated with a cost preview); GitHub Actions on push for redeploys
(Camerata scaffolds the deploy workflow, so redeploy "just happens" on merge).

**D5. Data + migrations.** Managed Postgres via terraform; sqlx migrations run on deploy
behind the Class-C gate (migrations can lose data).

**D6. Runtime secrets.** App secrets (DB URL, API keys) flow from the per-tenant vault into
the app-service config at deploy. Ties to the per-project-secrets gap (section 7).

**D7. URL / domain.** Default to the platform URL (azurewebsites.net); custom domain opt-in
(user provides DNS).

**D8. Lifecycle + cost.** Rollback via CI + versioned artifacts. Teardown via terraform
destroy (Class-C, gated). Infra spend is REAL money (unlike LLM spend), so every
provision/deploy shows a cost preview, and idle auto-sleep is worth considering. Extend the
ORCH-BUDGET rules to cover infra spend.

**Everything in deploy (provision / deploy / migrate / destroy) is Class C** (irreversible /
costs money) and is ALWAYS human-gated with a plan + cost preview. That is how "just deploy"
still keeps you in the loop: one approve, with cost visibility.

## Broader decision inventory (what else needs deciding)

Beyond deployment, the open product/architecture decisions, roughly in priority order:
- **Dial UX:** inferred level vs literal slider (recommend inferred + overridable).
- **Design-approval granularity:** per app / per epic / per story (recommend per epic, fast
  path for trivial changes).
- **Voice:** on-device vs cloud STT/TTS; does the audit trail record the voice transcript?
- **Preview targets:** web-only to start, or desktop/mobile Dioxus targets early?
- **Multi-app management:** a user builds many apps, how is that surfaced (an app gallery)?
- **Stack profiles:** one Rust-fullstack profile now; when do siblings arrive?
- **DB schema ownership:** orchestrator proposes the schema, human reviews (backstopped by the
  gated migration, so schema mistakes are Class-C-caught).
- **Runtime observability of the DEPLOYED app:** capture errors from the live app to feed
  "there's a bug" automatically, or user-reported only? (Auto-capture is a strong later
  feature; the governance trail is the local analog.)
- **Cost controls:** infra budget caps per user/app.
- **The built apps' OWN auth:** does a given bespoke app need end-user logins? A per-app
  decision the orchestrator should raise (Class D, clarify).
- **Rollback strategy:** last-good-artifact redeploy, exposed as a one-word "roll back".

## Decisions resolved 2026-07-10

- **Built apps are web-only, delivered as an adaptable PWA.** The default Rust-fullstack
  template produces a responsive, installable Progressive Web App (manifest + service worker +
  responsive shell) so one codebase adapts to desktop and mobile. Preview targets are
  web-only. This keeps the preview + eventual cloud tunnel simple (one web surface) and gives
  "works on my phone and my laptop" for free.
- **Auto-capture defects before the user reports them (see feedback-loop section).**
- **A built app's own end-user auth is derived + clarified per app** (Class D): the
  orchestrator asks "does this app need end-user logins?" rather than guessing; if yes, the
  scaffold includes an auth module. Confirmed.
- **The design + spike + preview adapter foundation ships as its own PR** (this branch).

## Feedback loop: auto-capture + click-to-report

The loop's input side. Both sources produce ONE structured report that feeds the governed dev
loop (a report becomes a work item the fleet picks up, decompose -> gated implementation ->
hot-reloaded preview).

- **Auto-capture (catch bugs before the user notices).** The scaffolded PWA ships a built-in
  error reporter baked into the template: a wasm panic hook + `window.onerror` +
  `unhandledrejection` + a failed-request interceptor, posting a structured report to Camerata
  (locally in preview; to a per-app capture endpoint once deployed). So runtime errors surface
  to the orchestrator proactively. Auto-capture is a Class-A-adjacent backstop: it catches a
  class of defect mechanically, before it needs a human.
- **Click-to-report (user-initiated).** In the preview, the user clicks an element and
  describes the issue; the report carries the element selector + the current route/state.
  (Depends on the chat head's preview surface, so it lands after the head; the ingest model +
  endpoint are built first and shared with auto-capture.)
- **Shared model:** `DefectReport { app/project id, source: auto|user, kind:
  runtime_error|user_report|..., title, description, context (route, element, stack, console),
  severity, ts }`, in the api-types contract. Ingest endpoint stores it + opens a governed
  work item. Recorded in the governance trail so "what was reported, and what the fleet did
  about it" is auditable.

## Architect-orchestrator design (decided 2026-07-10, from the Fable review)

The orchestrator is the top-tier-model brain that takes intent (new app / change / auto-defect),
decides work WITHIN the dial, drives the existing fleet, and stops at human-review checkpoints.
Decided design:

- **Confidence is mechanical, never LLM self-report (DECIDED: mechanical class + calibration
  loop).** The decision CLASS (A/B/C/D) comes from the action's effect-signature at the gate
  boundary (write to `migrations/`, a terraform call, a spend, a secret-scope change, an
  external POST = C), detected by the same interception the deny-before-execute gate owns.
  Confidence within A/B is a coarse ordinal from checkable features (compiles, inside the
  vetted skeleton, adds an out-of-vetted-set dep, previously redirected). Every autonomous
  decision AND its outcome (human redirected / later defect-linked / survived) is recorded, so
  the dial threshold is tuned against measured override rates. The moat artifact: "measured
  override rate at max dial: X%." New governance-event kind `orchestrator_decision {class,
  confidence, chosen, alternatives, assumption?}`.
- **One spine, three intake policies.** Shared work-item + execution (`start_governed_run`);
  the entry POLICY differs by source. **Class B requires a watcher** (DECIDED: watcher-
  dependent): auto-fix mechanically-verifiable defects only while the human watches the live
  preview; for deployed/idle apps, triage + fingerprint + a next-session digest, never
  hot-patch unwatched.
- **`crates/orchestrator` above the spine** (do not fork `dev_implement_run`), plus a
  **micro-change lane** for Class-B preview edits (gated + `verify_after_edit`, but skips
  decompose/story/L3 ceremony; micro-edits squash into PO-labeled keep-points).
- **`DecisionRecord` gains `approved_by: human | orchestrator{class, confidence}`** — where the
  audit trail proves "what it decided on its own."
- **A living spec per app** (like budget-tracker's SPEC.md), updated after each accepted
  change: the model's memory, contradiction detector, checkpoint-diff source, and the PO's
  "what does my app do now." Highest-leverage single artifact.
- **Design-approval is delta-triggered** (fires on a new noun: schema entity, integration,
  secret, auth, out-of-vetted dep) and shown in PO language (screens + data + assumptions),
  not story splits. Verbs: approve / redirect.
- **Class-D asks only when ambiguity is load-bearing**; else assume-and-declare into an
  assumptions ledger. Cap ~3 batched, customer-phrased questions per round.
- **Keep-point undo** (revertable sentences, not SHAs); undo across a migration is Class C.
- **From-scratch = refuse gracefully in v1** (the vetted skeleton is part of the backstop);
  more vetted skeleton profiles later. Fix the `DISQUALIFYING_SIGNALS` false-positive on
  "feels like a desktop app" once the orchestrator sets `AppTarget` structurally.
- **Dial is inferred + VISIBLE + overridable**, persisted per project (an invisible inferred
  dial is indistinguishable from no dial).

## Usability backlog (Fable senior-PM review, 2026-07-10, ranked)

Fold-in-now items DECIDED (built with the foundation): fingerprint + dedupe `DefectReport`
(before it is baked into every scaffolded app), default-private deploy (single-user lock unless
opted public), chat secret-interceptor (scrub pasted keys from transcript/memory/audit). The
rest are the UX backlog for the head + orchestrator phases:

1. **First 5 minutes are silent** -> render the branded empty skeleton immediately, narrate
   progress in PO language (features appear into a running app). Governed is slower; silence
   reads as broken. Also: one-line consent before auto-creating a GitHub repo (fine for Zach,
   alarming for friends).
2. **"Did it hear me?"** -> build-state indicator on accept + a "changed: X" toast on
   `EditVerdict::Applied`; the orchestrator dedupes an in-flight matching intent instead of
   forking a second run. Never surface rustc output ("I broke something, fixing it").
3. **No brake** -> a first-class user "stop/wait" verb that checkpoints the in-flight run
   (reuse `checkpoint.rs`) and hands control back.
4. **Deploy cost surprises + idle apps** -> always $/month, restated at the C gate with "what
   if I say no"; proactive sleep offers after idle; a standing cost line in the app gallery;
   first-deploy dry-run (creds + quota) before "deploy day"; translate Azure errors; auto-retry
   name collisions.
5. **Close the feedback loop to the user** -> `DefectStatus` exists but nothing tells the user
   their report was understood; restate on receipt, connect fix to report on resolve.
6. **App gallery is the home screen** (status + monthly cost per app) + a one-verb archive
   (tears down infra C-gated, keeps spec + history restorable).
7. **Friends break every assumption at once** (staged): rung-3 managed hosting becomes the only
   viable rung for no-cloud-account users; per-user LLM budget with graceful limits; per-persona
   consequence-first approval copy. Build the cost-ledger + approval copy as if it is coming.

## Open questions (for Zach)
- Dial UX: a literal slider, an inferred level, or a per-request "how sure should you be
  before asking me" phrasing? (Recommend: inferred + overridable.)
- Design-approval granularity: one approval per app, or per story/epic? (Recommend: per
  epic, with a fast-path for trivial changes.)
- Voice: on-device vs. cloud STT/TTS, and does the audit trail record the voice transcript?
- Preview for non-web Dioxus targets (desktop/mobile) later, or web-only to start?
