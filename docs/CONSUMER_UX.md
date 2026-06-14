# CONSUMER_UX.md

The design spec for the consumer-mode prototype (VISION section 5, PO mode; section
20, the consumer tier). This is the artifact: a flow a non-technical person walks
through, recordable as a video, that proves a governed, clarification-first app
generator is possible. The differentiator is not the code generation (that is
becoming commodity). It is the **pre-build clarification conversation** and the
**design polish**. This doc is the bar for both.

> Scope note: the prototype deploys to the user's OWN Azure account
> (bring-your-own-infra). The managed-cloud PaaS is the funded endgame (VISION
> section 20), not this artifact.

## The one-sentence experience

A person who cannot code answers a structured form, has a short back-and-forth with
an AI lead engineer who pins down what they actually meant, watches a governed agent
team build it, and ends with a working app live on their own cloud, never seeing a
line of code or a single error message.

## Design principles (the bar: best-in-class user-friendly design)

The standard here is the most thorough, user-friendly consumer-product design, the
kind that makes powerful machinery feel effortless. Every principle below serves
that: the user should feel guided and capable, never lost or technical.

1. **One decision per screen.** Never show the user two things to think about at
   once. The form is paged, the clarification is one question-thread at a time, the
   build is a single calm progress narrative.
2. **Progressive disclosure.** The complexity (governance, agents, worktrees, the
   gate) is real and total underneath, and almost entirely invisible above. The user
   sees intent and outcome, never plumbing.
3. **Calm confidence, not dashboards.** No blinking terminals, no logs, no token
   counters. The surface communicates "this is handled" the way a well-made product
   does. Motion is slow and reassuring, not busy.
4. **The conversation is the magic, so it gets the spotlight.** The clarify step is
   the one moment the product feels smarter than the user expected. It is the hero
   screen and the centerpiece of the video.
5. **No error messages, ever, to the consumer.** When an agent stumbles, the gate
   bounces it and it retries silently (the bounce-and-revise loop). The user sees a
   slightly longer "working" state, never a stack trace.
6. **Honesty about what it is.** The product never pretends the build is instant or
   magic-free. It narrates ("designing the data model", "writing the expense list",
   "checking it against the rules") so the user trusts the process they are watching.

## The journey (the recordable flow)

### 1. Intake: a strict, open-ended user-story form

The form is the constraint that keeps the agents aligned. It is NOT a single prompt
box (that is the failure mode of the consumer tools). It is also NOT
budget-specific; it is open-ended enough for any small bespoke app, while forcing a
thorough, well-bounded user story.

Form shape (the strict-but-flexible spine):
- **What is it?** App name + a one-paragraph plain-language description.
- **Who uses it, and what can each kind of person do?** Roles + their top actions.
  This is the user-story forcing function: "As a [role], I want to [action]."
- **What are the things it keeps track of?** Entities, each with a few fields. The
  UI offers types in plain language ("a price", "a date", "yes/no", "a link to
  another thing"), not SQL types.
- **What should a person be able to do with each thing?** CRUD-ish features per
  entity, in consumer words (add / see a list / edit / remove / search).
- **Anything important or unusual?** A free-text constraints box for the leeway and
  creativity: rules, must-haves, look-and-feel wishes.

The form is strict in structure (every app has roles, entities, actions) and open in
content (any domain). It produces the typed `IntakeForm` the engine already models.

### 2. Clarify: the lead engineer's checklist (THE HERO)

On submit, the AI lead engineer reads the whole form and does what a good engineer
does with a vague ticket: it finds the gaps, contradictions, and unstated
assumptions, and works through them with the user. This is the screen the video
lingers on, and it is the differentiator.

The conversation IS a checklist: a running list of the things the lead engineer
needs pinned down before it feels comfortable building. Each answered item ticks
off; the user can see how close the engineer is to "ready."

- **Hybrid, free-text-led.** The user answers in their own words (free text is
  primary), with guidance baked in: each question carries a short reason, and the UI
  offers quick-reply chips and suggested answers so the user is steered, never stuck.
- **A confidence score, tracked throughout and visible.** It climbs as the checklist
  fills. It is the honest signal of "how well can I build this for you right now,"
  and it is what makes the trade-off of skipping ahead legible.
- **Product-level suggestions the user did not think of.** The lead engineer
  proactively raises things a Product Owner would miss, in plain language. Example:
  "You added login. Apps like this usually also need a place for an admin to manage
  users and decide who can do what. Want me to include a simple users-and-permissions
  area?" Written this way, the user understands it even though they never thought of
  RBAC. This is the engineer earning its title.
- **Honest about its own limits.** The lead engineer knows what Camerata can and
  cannot build well. If the app genuinely needs a human architect in the loop, it
  says so. If the request is beyond what Camerata can do well on its own (too large or
  too novel), it says that plainly rather than confidently building something
  fragile. This honesty is a trust feature, not a failure.
- **Bypassable at any time.** The user can say "just build it" whenever they want.
  The confidence score shows what they are trading off (a lower-confidence build), but
  it is their call. The checklist guides; it never traps.
- **Ends with the plan: prose AND a diagram.** A plain-language summary of what will
  be built, alongside a visual map of the entities and their actions, with the
  product suggestions and the decisions folded in. The user approves, tweaks, or
  builds anyway.

This pre-build alignment is the feature the prompt-to-code tools do not have and the
SDD tools bury behind markdown. Showing it on screen, in plain language, is the pitch.

### 3. Build: governed construction, with the engineer still listening

The user clicks Build once. Underneath: the plan becomes governed agent tasks, the
fleet runs, every write passes the gate, layer-2 checks bounce anything sloppy. On
top: a single, slow, legible progress story.

- A short list of human-readable stages ("Setting up the project", "Building the
  data model", "Creating the screens", "Checking it against the rules"), each
  completing with a quiet check.
- When the gate bounces an agent, the stage simply takes a little longer; the copy
  stays calm. The user never learns a rule was violated.
- **The engineer can still ask.** If a real question surfaces mid-build that changes
  the outcome, the lead engineer raises it to the user instead of guessing ("For the
  export, did you want a spreadsheet or a PDF?"). Most builds need none; when one is
  needed, it appears calmly, never as an error.
- No terminal, no logs, no jitter. Time-honest, trust-honest, stress-free.

### 4. QA: the user tests their own app (in draft)

The built app opens in a DRAFT state, and the user is the QA. They click around, try
the things they asked for, and confirm it does what they meant. The product is honest
that this is a draft the user verifies, not a finished thing dropped on them.

### 5. Report a problem: the structured bug form

If something is wrong, the user files it through a bug form. Like the intake form, it
is strict: it forces the user to describe the problem in a shape the agents can act
on (what they did, what they expected, what actually happened, on which screen or
feature), not a vague "it's broken." The report goes back through the governed build
loop, the agents fix it under the gate, and the user re-QAs. Iterate until happy.

### 6. Publish: out of draft, onto your cloud

Only when the user is satisfied do they Publish. The app leaves DRAFT and goes live,
deployed to their own cloud (BYO-infra for the prototype). "Your app is live" with a
real URL on their own account. The draft-to-publish gate keeps the user in control of
when their app becomes real.

### 7. Change it: tracked changes

After publishing, the user can keep changing the app. Each change is tracked and runs
the same loop in miniature (describe -> clarify -> governed build -> QA -> publish).
The app has a history, and every change is safe because the same governance applies to
all of them. This is the iteration that makes the prompt-to-code tools collapse into
debt, made durable by the gate.

## The lead engineer's behavior (the intelligence the user feels)

This is the spec for what makes the conversation feel like a real Staff Engineer, and
the bar for the `LeadEngineer` implementation:

- **Checklist-driven.** It maintains an explicit list of what it needs to know, works
  the user through it, and shows progress.
- **Confidence-scored.** It tracks and exposes how ready it is to build well.
- **Proactively suggestive.** It raises product-level needs the user would not think
  of (admin/RBAC alongside login, soft-delete, audit, etc.), always explained plainly.
- **Honest about limits.** It recommends a human architect when warranted and declines
  to over-promise on apps beyond Camerata's reach, rather than building something that
  will fail QA.
- **Reachable during the build.** It surfaces genuine mid-build questions instead of
  guessing.

## What the video shows (the artifact) and the definition of done

The recording is the whole flow, as a regular person: fill the open-ended form, work
the clarify checklist (the engineer catching things you did not think of, suggesting
the admin page, the confidence score climbing), approve the plan (prose + diagram),
watch the calm governed build, QA the draft yourself, optionally file a structured bug
and watch it get fixed, then publish to a real cloud URL on your own account.

**Definition of done for the prototype:** Zach can screen-record that entire flow,
twice, for two genuinely different apps (one improvised on the spot), and show a
working published end product on his own Azure. Scope-honest: the apps are real but
modest (a useful small CRUD-class app), within what Camerata can build well, NOT a
full Salesforce-scale system. The artifact proves the *product is possible* and that
the consumer-as-Product-Owner clarify loop is real; it does not productionize the
managed PaaS (that is the funded endgame, VISION section 20).

## Build order for the UI (Dioxus)

1. The screens as static, beautifully-styled Dioxus views with mocked data (intake,
   clarify-checklist, build, QA, bug form, publish/live), to nail the look and the
   motion first (get the feel right before wiring, the way the best consumer products
   do).
2. Wire intake -> the typed `IntakeForm`.
3. Wire clarify -> the real multi-turn `LeadEngineer` loop with the checklist,
   confidence score, and product suggestions.
4. Wire build -> the `FleetCoordinator` with a live progress stream (needs stream-json
   from the agent driver) and the mid-build question channel.
5. Wire QA / bug form -> the governed fix loop; publish -> the BYO-infra deploy.
5. Wire live -> the Azure deploy adapter.

Dogfood chorale for any tabular surfaces (the generated app's own admin lists), per
the family conventions.

## Resolved UX decisions (Zach, 2026-06-14)

1. **The clarify conversation is HYBRID, free-text-led.** The user goes back and
   forth in their own words (free text is primary), but guidance is baked in: the
   engineer frames each question with a short reason, offers quick-reply chips and
   suggested answers, and steers the user toward a complete story. The model is the
   kind of guided back-and-forth this very project was designed through: open
   conversation, but never aimless.
2. **The plan is shown as BOTH prose AND diagrams.** A short plain-language summary
   of what is being built, alongside a visual map of the entities and their actions,
   so the user understands the app from two angles before approving.
3. **The demo video is a MONTAGE OF TWO open-ended apps.** Not one canned example.
   Two genuinely different apps, ideally one improvised on the spot, to prove the
   flow works for whatever a person dreams up, not a pre-scripted budgeting demo.
   The open-ended form (not budget-specific) exists precisely to make this possible.
