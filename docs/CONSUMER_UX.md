# CONSUMER_UX.md

The design spec for the consumer-mode prototype (VISION section 5, PO mode; section
20, the consumer tier). This is the artifact: a flow a non-technical person walks
through, recordable as a video, that proves a governed, clarification-first app
generator is possible. The differentiator is not the code generation (that is
becoming commodity). It is the **pre-build refinement conversation**, the
**three-artifact model that keeps the project honest over time**, and the **design
polish**. This doc is the bar for all three.

> Scope note: the prototype deploys to the user's OWN Azure account
> (bring-your-own-infra). The managed-cloud PaaS is the funded endgame (VISION
> section 20), not this artifact.

## The one-sentence experience

A person who cannot code fills out a structured project form, refines a living set
of user stories with an AI lead engineer until they are both confident, watches a
governed agent team build it, tests the draft themselves, and ends with a working app
live on their own cloud, never seeing a line of code or a single error message.

## Design principles (the bar: best-in-class, most thorough user-friendly consumer-product design)

The standard here is the most thorough, user-friendly consumer-product design, the
kind that makes powerful machinery feel effortless. Every principle below serves
that: the user should feel guided and capable, never lost or technical.

1. **One decision per screen.** Never show the user two things to think about at
   once. The form is paged, the refinement session works one question-thread at a
   time, and the build is a single calm progress narrative.
2. **Progressive disclosure.** The complexity (governance, agents, worktrees, the
   gate) is real and total underneath, and almost entirely invisible above. The user
   sees intent and outcome, never plumbing.
3. **Calm confidence, not dashboards.** No blinking terminals, no logs, no token
   counters. The surface communicates "this is handled" the way a well-made product
   does. Motion is slow and reassuring, not busy.
4. **The conversation is the magic, so it gets the spotlight.** The refinement
   session is the one moment the product feels smarter than the user expected. It is
   the hero screen and the centerpiece of the video.
5. **No error messages, ever, to the consumer.** When an agent stumbles, the gate
   bounces it and it retries silently (the bounce-and-revise loop). The user sees a
   slightly longer "working" state, never a stack trace.
6. **Honesty about what it is.** The product never pretends the build is instant or
   magic-free. It narrates ("designing the data model", "writing the expense list",
   "checking it against the rules") so the user trusts the process they are watching.

## The three artifact types

Everything the user produces and everything the AI contributes lives in exactly three
artifact types. Understanding these is the key to the whole product.

### 1. The onboarding document

The onboarding document is the grand project plan the user fills out first. It is
strict in structure (every app has roles, entities, actions) and open in content (any
domain). It forces a thorough, well-bounded user story for whatever the user wants to
build, without being budget-specific and without collapsing into a single prompt box
(that is the failure mode of the commodity tools).

The onboarding document is the SEED the first refinement session reads. Once the
first refinement session begins, the onboarding document is **frozen as read-only
origin and history**. It is not the thing the user keeps editing. It is the record of
where the project started.

### 2. User stories (and bug stories)

User stories are the investigation's main output: pre-filled by the AI, but written
in consumer-abstracted prose, NOT real product-owner or engineering user stories.
There are no API contracts, no Gherkin, no technical acceptance criteria. A user
story a normal person understands: who it is for, and a plain bulleted list of what
they want to be able to SEE and DO.

The user can ADD, EDIT, and DELETE user stories at any point. After the onboarding
document is frozen, **user stories (and later, bug stories) become the living source
of truth, exactly the way stories and bug tickets are the source of truth in real
software development.** Every refinement session works from the current story list.
Every build executes against it. The history of edits to the story list is the
history of the project.

Bug stories are the same shape as user stories, produced through the structured bug
form during QA. They describe a problem (where it happened, what the user did, what
they expected, what actually happened) in a form the agents can act on. They feed
into a refinement session, which feeds into a fix build.

### 3. Clarifications and answers

Clarifications are question-and-answer pairs. Both the AI and the user can add,
edit, and delete them. They live alongside the story list throughout the project.
They are the record of every decision that was made in conversation, so no context
is lost between sessions.

### Plus: product suggestions and the confidence score

**Product suggestions** are proactive product-level catches the AI raises that the
user would not have thought of. Example: "You added login. Apps like this usually
also need a place to manage who has access and what they can do. Want me to include a
simple permissions area?" (RBAC, explained plainly.) Each product suggestion
**references a specific user story**, so the user always knows what prompted it.

**The confidence score** is a running percentage. It is the single convergence
signal for the whole refinement loop: how ready is the AI to build this well, right
now? It updates as stories and clarifications are added or edited. It is the honest
signal that makes "skip ahead and build anyway" a legible trade-off rather than a
coin flip.

## Persistence and version history (every edit is saved, nothing is lost)

The artifacts above are not in-memory scratch. Every artifact (the onboarding
document, each user story, each clarification, each suggestion, each refinement
session) is persisted to a database, and every edit by the user OR the AI is written
in real time as a new revision. The store is append-only: an edit never overwrites,
it appends a new version, so the full history of how a story or a spec evolved is
always recoverable, and the audit trail records WHO made each change (user or AI),
WHAT operation (create / edit / remove), and WHEN. This is what lets the product
show version history, undo, and a credible provenance trail, and it is what makes
"stories are the source of truth" real rather than aspirational: the truth lives in
the database, versioned, not in a transient UI buffer.

Engine choice: the prototype and the desktop cockpit use embedded SQLite (zero-ops,
one binary, matches the single-process monolith). The managed-cloud endgame (VISION
section 20) moves the same store behind the same trait seam to managed Postgres. The
version history is implemented at the application level as an event-sourced revision
log, NOT via database-native temporal/system-versioned tables: the revision log is
portable across SQLite and Postgres, and it carries richer intent (actor, operation,
plain-language note) than row-state temporal tables can. See
`crates/persistence` (`ArtifactStore`) and `docs/decisions/`.

## The refinement session: the one repeating primitive

There is exactly one back-and-forth loop, and it is the heart of the product. It is
called the **refinement session**, and it works like this:

> The AI reviews the current artifacts (onboarding document, user stories,
> clarifications) and edits stories, raises product suggestions, and asks clarifying
> questions. The user edits, answers, adds, and deletes stories, clarifications, and
> answers. The confidence score updates. Repeat until the user is happy with the
> confidence percentage.

The user can bypass at any time ("just build it"). The confidence score shows what
they are trading off, but it is their call. The refinement session guides; it never
traps.

The refinement session is not a one-time setup. It is the same primitive, reused in
three contexts across the lifecycle:

**Pre-build refinement.** The onboarding document seeds the first session. The AI
investigates, fills in stories, raises suggestions, and asks questions. The user
refines. Repeat until the user approves and the confidence is where they want it.
This is where the plan is reviewed as both prose and a visual entity-and-action map.
Then the user clicks Build.

**Mid-build refinement.** During execution, builder agents may hit a genuine question
that changes the outcome in a way the plan did not resolve. When that happens,
execution PAUSES and a refinement session opens, scoped to exactly that question.
Once the user answers and the session closes, execution RESUMES. Most builds need
none of these. When one surfaces, it appears calmly, as a quiet question, never as
an error.

**Post-build refinement.** After a build, the user tests the draft. Problems are
filed through the structured bug form, which forces a shape the agents can act on.
Those bug reports become bug stories and feed a new refinement session, which feeds a
fix build. The cycle repeats until the user is satisfied.

## The lifecycle

```
onboarding document
    |
    v
refinement session  <--+
    |                  | (N rounds, until confident)
    +------------------+
    |
    v
execution (build)
    |
    +-- [mid-build escalation -> refinement session -> resume]  (as needed)
    |
    v
post-build refinement  <--+
    |   (QA + structured bug forms -> bug stories -> fix build)
    +---------------------------+
    |                          | (N rounds, until satisfied)
    v                          |
    +-- continue or ---------->+
    |
    v
publish  (draft -> live on the user's own cloud)
    |
    v
ongoing tracked changes  (each change runs the same loop in miniature)
```

The whole product is one primitive (the refinement session) alternating with
execution, both before and after the first build, with the deterministic governance
gate underneath every execution. Publish is the explicit draft-to-live gate. After
publish, the user keeps changing the app: each change runs the same loop in
miniature, and every change is safe because the same governance applies. The story
list carries the history.

## The journey screen-by-screen (the recordable flow)

### 1. Intake: the onboarding document

The form is the constraint that keeps the agents aligned. It is NOT a single prompt
box. It is also NOT budget-specific. It is open-ended enough for any small bespoke
app while forcing a thorough, well-bounded project plan.

Form shape (the strict-but-flexible spine):

- **What is it?** App name plus a one-paragraph plain-language description.
- **Who uses it, and what can each kind of person do?** Roles and their top actions.
  This is the forcing function: "a person of type X wants to be able to Y."
- **What are the things it keeps track of?** Entities, each with a few fields. The
  UI offers types in plain language ("a price", "a date", "yes/no", "a link to
  another thing"), not SQL types.
- **What should a person be able to do with each thing?** CRUD-style features per
  entity, in consumer words (add, see a list, edit, remove, search).
- **Anything important or unusual?** A free-text constraints box for rules,
  must-haves, and look-and-feel wishes.
- **What should it look like?** A style picker, NOT a blank canvas. Camerata ships a
  small curated set of tasteful **color palettes** (Warm Studio, Clean Slate,
  Forest, Midnight, Blossom) and **style examples** the user selects from: button
  shape (rounded, pill, blocky) and font personality (system, serif, geometric,
  monospace). The set is curated so a non-technical user cannot pick a bad-looking
  combination. The user can also **upload inspiration images** the AI interprets for
  styling cues ("I love these warm tones"). Every selection is captured INTO the
  onboarding document (`StylePreferences` on the typed `IntakeForm`), so the build
  honors an explicit look rather than guessing from prose. See `crates/intake/src/appearance.rs`.

The form produces the typed `IntakeForm` the engine already models. Once submitted,
it is the frozen origin. The next thing the user sees is the refinement session.

### 2. The refinement session (THE HERO)

This is the screen the video lingers on. It is the differentiator. Everything else a
prompt-to-code tool does; this, done well, they do not.

The refinement session screen has four elements working together:

- **The user-story list.** The centerpiece. Pre-filled by the AI from the onboarding
  document, written in plain consumer prose. Each story shows who it is for and a
  bulleted list of what they see and do. The user can add, edit, and delete stories
  inline.
- **The confidence score.** Prominent, climbing. The honest signal of readiness. The
  user watches it rise as the session progresses.
- **Product suggestions.** Proactive raises from the AI, each anchored to a specific
  story. Shown in a way that invites the user to act on them (accept, edit, dismiss).
- **The running AI-to-user transcript.** Questions, answers, and reasoning, visible
  as a clean conversation thread. The user answers in free text (primary), with
  guidance chips and suggested replies baked in so they are steered, never stuck.

The user can click "just build it" at any time. The confidence score makes the
trade-off visible.

The session ends when the user is happy with the confidence percentage. At that
point, the plan is shown as BOTH prose (a short plain-language summary of what will
be built) AND a visual entity-and-action map. The user approves, tweaks, or builds
anyway.

#### The shared-design opt-in (a network effect, by consent)

Inside the refinement session the user is offered two independent, opt-in choices
(both default OFF):

- **Share my design to help future apps.** If a user opts in, their design documents
  and stories are contributed (ABSTRACTED, never the business's data or private
  notes) to a corpus Camerata draws on when the NEXT person builds a similar app. The
  shared design includes the **bug stories AND their fixes** (what went wrong and what
  changed to resolve it), so future similar apps inherit hard-won fix knowledge, not
  just app shapes. The copy says plainly: opting in helps Camerata build better, more
  consistent apps for everyone, and only the shape of the app is shared. **The user
  can opt out at any time, and opting out DELETES their shared design from the corpus
  and its search index.** This is a true right-to-be-forgotten, not just a stop on
  future shares: every contribution is stamped with the owning project's id, so a
  withdrawal removes the design and every derived vector row carrying that id.
- **Use historical data to influence my design.** If a user opts in, Camerata seeds
  their refinement from prior consented designs for similar apps (a second person
  building a rental-payment app starts from the shape of one that already exists). The
  copy says plainly: this can speed up your setup and start you from a proven design
  you can still change freely.

The payoff is a flywheel: every shared design makes the corpus richer, which makes
future intakes faster and more consistent, which makes the whole fleet of bespoke
apps more uniform and therefore easier to maintain. Privacy is the hard rule:
contributions are abstracted to the structural shape (entities, capabilities, story
patterns); the description, constraints, look-and-feel, and any field values are
stripped. See `crates/intake/src/sharing.rs` (`SharingPreferences`, `DesignCorpus`,
`abstract_design`).

### 3. Build: governed construction, with the engineer still listening

The user clicks Build once. Underneath: the story list becomes governed agent tasks,
the fleet runs, every write passes the gate, layer-2 checks bounce anything sloppy.
On top: a single, slow, legible progress story.

- A short list of human-readable stages ("Setting up the project", "Building the
  data model", "Creating the screens", "Checking it against the rules"), each
  completing with a quiet check.
- When the gate bounces an agent, the stage simply takes a little longer. The copy
  stays calm. The user never learns a rule was violated.
- **Mid-build escalations appear as quiet questions.** If a builder agent hits a
  question that changes the outcome, execution pauses, the question appears on
  screen, the user answers, and execution resumes. Most builds need none. When one
  appears, it is calm, never alarming.
- No terminal, no logs, no jitter. Time-honest, trust-honest, stress-free.

### 4. QA: the user tests their own app (in draft)

The built app opens in a DRAFT state, and the user is the QA. They click around, try
the things they asked for, and confirm it does what they meant. The product is honest
that this is a draft the user verifies, not a finished thing dropped on them.

### 5. The bug form: structured problem reports

If something is wrong, the user files it through the structured bug form. Like the
intake form, it is strict: it forces the user to describe the problem in a shape the
agents can act on. The four fields:

- **Where were you?** The screen or feature.
- **What did you do?** The action the user took.
- **What did you expect?** What should have happened.
- **What actually happened?** What did happen.

This is not a free-text complaint box. It produces a bug story in the same format as
a user story. The bug story feeds a refinement session (scoped to the fix), which
feeds a targeted build, and the user re-QAs. Iterate until satisfied.

### 6. Publish: out of draft, onto your cloud

Only when the user is satisfied do they Publish. The app leaves DRAFT and goes live,
deployed to their own cloud (BYO-infra for the prototype). "Your app is live" with a
real URL on their own account. The draft-to-publish gate keeps the user in control of
when their app becomes real.

### 7. Ongoing tracked changes

After publishing, the user can keep changing the app. Each change is described,
refined, built, and QA-tested through the same loop in miniature. The story list
grows. Bug stories accumulate. Every change is safe because the same governance
applies to all of them. This is the iteration that makes the prompt-to-code tools
collapse into debt, made durable by the gate.

## The lead engineer's behavior (the intelligence the user feels)

This is the spec for what makes the refinement session feel like a real Staff
Engineer and the bar for the `LeadEngineer` implementation:

- **Checklist-driven.** It maintains an explicit list of what it needs to know,
  works the user through it, and shows progress. The story list is the artifact that
  grows out of this checklist.
- **Confidence-scored.** It tracks and exposes how ready it is to build well. The
  score is a first-class output of every session turn, not an afterthought.
- **Proactively suggestive, with story references.** It raises product-level needs
  the user would not think of (admin access alongside login, soft-delete, audit
  trails, etc.), always explained plainly, always anchored to the specific story that
  prompted the observation.
- **Honest about limits.** It recommends a human architect when the app genuinely
  needs one. It declines to over-promise on apps beyond Camerata's reach, rather than
  confidently building something that will fail QA. This honesty is a trust feature,
  not a failure.
- **Reachable during the build.** It surfaces genuine mid-build questions to the
  user instead of guessing, and it does so calmly, as described in the mid-build
  escalation model.
- **Decides the technical dependencies.** Choosing which external libraries the app
  needs is a LEAD-ENGINEER decision, not the user's. The user never sees a package
  name. The engineer picks them: Camerata's own `rust-chorale` for any tabular
  surface (the default for lists and grids), and, where a capability genuinely needs
  it, a JavaScript library for the generated app's frontend (the all-Rust rule binds
  Camerata's ENGINE, not necessarily every generated target app). It records what it
  chose and why in the plan, so the choice is auditable.

## Maintenance: the standing ops agent (post-publish)

A published app is not done; it is alive, and it needs ongoing operations. Camerata
attaches a **standing, asynchronous maintenance agent** to every published app. It is
the app's whole ops function, not just a package updater. Its remit (open-ended, and
expected to grow):

- **Dependency upgrades.** It scans for newer versions of the app's libraries on a
  regular cadence.
- **Security patching.** It watches for vulnerabilities in what the app depends on.
- **Key and secret rotation.** It rotates credentials, API keys, and secrets on a
  schedule, so they do not go stale or leak value over time.
- **General ops hygiene.** Certificate renewal, backups, health, and the operational
  chores a human ops engineer would own. This list is deliberately not exhaustive;
  the agent owns ops, whatever ops turns out to mean for a given app.

The user-facing contract is calm and honest, never alarming. The maintenance agent
does NOT silently change a live app. When an update matters (especially a security
one), the user gets a **plain-language recommendation** first: "It is a good idea to
bring your app up to date. A part of it has a security fix available." Approving runs
the update through the SAME governed build-and-QA loop as any other change (the gate
applies to maintenance exactly as it applies to features), so "bring it up to date" is
as safe as every other change. Nothing about the app is ever changed outside the gate.

This is a real differentiator: the prompt-to-code tools hand you code and walk away;
the debt and the rot are yours. A governed standing ops agent means a non-technical
owner gets the maintenance a real engineering team would provide, without hiring one.

## Build order for the UI (Dioxus)

1. The screens as static, beautifully-styled Dioxus views with mocked data (intake,
   refinement session, build, QA, bug form, publish/live), to nail the look and the
   motion first. Get the feel right before wiring, the way the best consumer products
   do.
2. Wire intake into the typed `IntakeForm`.
3. Wire the refinement session into the real multi-turn `LeadEngineer` loop: the
   editable story list, the confidence score, the product suggestions with story
   references, and the running clarification transcript.
4. Wire build into the `FleetCoordinator` with a live progress stream (needs
   stream-json from the agent driver) and the mid-build escalation channel.
5. Wire QA and the bug form into the governed fix loop. Wire publish into the
   BYO-infra deploy adapter.

Dogfood rust-chorale for any tabular surfaces (the story list, the generated app's
own admin views), per the family conventions.

## What the video shows and the definition of done

The recording is the whole flow, as a regular person: fill the open-ended onboarding
document, work the refinement session (the AI pre-filling stories in plain prose, the
confidence score climbing, a product suggestion anchored to a specific story), approve
the plan (prose and diagram), watch the calm governed build, test the draft yourself,
optionally file a structured bug and watch it feed a targeted fix, then publish to a
real cloud URL on your own account.

**Definition of done for the prototype:** Zach can screen-record that entire flow,
twice, for two genuinely different apps (one improvised on the spot), and show a
working published end product on his own Azure. Scope-honest: the apps are real but
modest (useful small CRUD-class apps), within what Camerata can build well, NOT a
full enterprise-scale system. The artifact proves the product is possible and that the
consumer-as-Product-Owner refinement loop is real. It does not productionize the
managed PaaS (that is the funded endgame, VISION section 20).

## Resolved UX decisions (Zach, 2026-06-14)

1. **Stories and bug stories are the source of truth after the onboarding document is
   frozen.** The onboarding document is read-only history once the first refinement
   session begins. Everything after that lives in the story list and the
   clarification record. This mirrors the way real software development works:
   tickets and stories, not the original brief, are the source of truth.
2. **The refinement session is the single primitive, reused in all three contexts.**
   Pre-build, mid-build, and post-build all run the same loop: review artifacts,
   edit stories, raise suggestions, ask questions, update confidence, repeat. There
   is no separate "clarify" step and "bug triage" step; it is one primitive with
   three contexts.
3. **The clarification conversation is hybrid, free-text-led.** The user goes back
   and forth in their own words (free text is primary), but guidance is baked in: the
   AI frames each question with a short reason, offers quick-reply chips and suggested
   answers, and steers the user toward a complete story. The model is the kind of
   guided back-and-forth this very project was designed through: open conversation,
   but never aimless.
4. **The plan is shown as both prose and a diagram.** A short plain-language summary
   of what is being built, alongside a visual map of the entities and their actions,
   so the user understands the app from two angles before approving.
5. **The demo video is a montage of two open-ended apps.** Not one canned example.
   Two genuinely different apps, ideally one improvised on the spot, to prove the
   flow works for whatever a person dreams up, not a pre-scripted scenario. The
   open-ended form (not budget-specific, not domain-specific) exists precisely to
   make this possible.

## UI status

The consumer UI prototype lives in `crates/ui` (`camerata-ui`).

**Screens that exist** (one Dioxus view per stop of the journey, in
`crates/ui/src/screens/`):

- **Intake** — the open-ended onboarding document form.
- **Clarify** — the refinement session (the hero screen): editable story list,
  confidence score, product suggestions, clarification transcript.
- **Build** — governed-construction narrative with stage-by-stage progress beats.
- **Qa** — the user tests their own draft app.
- **Bug** — the structured "report a problem" form (a side loop off Qa).
- **Live** — publish / out-of-draft.

**Stack / target.** Dioxus 0.7.9, **desktop** target (chosen over web to avoid
the wasm toolchain for a build-it/run-it/screen-record-it artifact; matches the
family version used by rust-portfolio and rust-chorale). Navigation is a single
`Screen` enum plus one signal. Styling is a hand-written global stylesheet
(`style.rs`); no component kit.

**Run command (desktop app):**

```bash
cargo run -p camerata-ui
```

(Optionally `dx serve` from `crates/ui` if you have the Dioxus CLI and want
hot-reload; the plain `cargo run` needs no extra tooling.)

**Wired vs. mocked.** Everything is **mocked**. All screen content comes from
the static fixtures in `crates/ui/src/data.rs`; the Build screen's progress is a
timed dwell (`tokio::time::sleep` inside `use_future`), not a live fleet stream.
No engine wiring yet: Intake does not yet produce a typed `IntakeForm`, the
refinement session does not call the real multi-turn `LeadEngineer` loop, Build does
not drive the `FleetCoordinator`, and Live does not deploy. The goal of this pass is
the look, the motion, and the flow; wiring follows the build order above.
