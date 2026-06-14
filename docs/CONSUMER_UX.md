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

## Design principles (the Apple bar)

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

### 2. Clarify: the back-and-forth with the lead engineer (THE HERO)

On submit, the AI lead engineer reads the whole form and does what a good engineer
does with a vague ticket: it finds the gaps, contradictions, and unstated
assumptions, and asks. This is the screen the video lingers on.

UX of the conversation:
- It feels like chatting with a calm senior engineer, not filling more forms. One
  focused question at a time, with a short reason ("I ask because it changes how we
  store the data").
- Questions are specific and reveal expertise the user lacks: "When two people edit
  the same record at once, who wins?" "Should a deleted item disappear, or just be
  hidden?" "Do amounts ever go negative?"
- The user answers in plain language. The lead engineer folds each answer back into
  the spec and may ask a follow-up. A small, honest "Refining the plan…" beat
  between turns.
- A visible, shrinking sense of "things still to pin down", so the user feels
  progress toward a confident plan rather than an endless quiz. Cap the turns.
- It ends with a plain-language **plan summary** the user approves: "Here is what I
  am going to build for you," in their words, with the entities, the actions, and the
  decisions made during the conversation. Approve, or send it back with a tweak.

This pre-build alignment is the feature the prompt-to-code tools do not have and the
SDD tools bury behind markdown. Showing it on screen, in plain language, is the
whole pitch.

### 3. Build: governed construction as a calm narrative

The user clicks Build once. Underneath: the plan becomes governed agent tasks, the
fleet runs, every write passes the gate, layer-2 checks bounce anything sloppy. On
top: a single, slow, legible progress story.

- A short list of human-readable stages ("Setting up the project", "Building the
  data model", "Creating the expense list screen", "Double-checking it against the
  rules"), each completing with a quiet check.
- When the gate bounces an agent, the stage simply takes a little longer; the copy
  stays calm ("Tidying up the code"). The user never learns a rule was violated.
- No terminal, no logs, no percentage-jitter. Time-honest (it takes minutes), trust
  -honest (it narrates), stress-free (it cannot fail in a way the user sees).

### 4. Live: the app on the user's own cloud

The payoff. The governed, tested app deploys to the user's Azure (BYO-infra for the
prototype), and the user lands on:
- "Your app is live" with a real URL on their own account, and a one-line note that
  it is theirs, on their cloud.
- A button to open it, and a quiet "Change something" path that loops back to a
  smaller clarify conversation (the iteration the consumer tools collapse under, made
  safe by the same governance).

## What the video shows (the artifact)

The recording is: fill the form as a regular person, have the clarify conversation
(the lead engineer catching things the user did not think of), approve the plain
-language plan, watch the calm build, and open a working app on a real cloud URL.
The point being demonstrated is not "AI writes code." It is "a non-technical person
was interviewed into a correct spec and got a dependable app, governed end to end."

## Build order for the UI (Dioxus)

1. The four screens as static, beautifully-styled Dioxus views (intake, clarify,
   build, live) with mocked data, to nail the look and the motion first (Apple bar:
   get the feel right before wiring).
2. Wire intake -> the typed `IntakeForm`.
3. Wire clarify -> the real multi-turn `LeadEngineer` loop (the engine piece landing
   now).
4. Wire build -> the `FleetCoordinator` with a live progress stream (needs stream
   -json from the agent driver for real-time stage updates; see PO_MODE remaining
   work).
5. Wire live -> the Azure deploy adapter.

Dogfood chorale for any tabular surfaces (the generated app's own admin lists), per
the family conventions.

## Open UX questions for Zach

- How much of the clarify conversation is free-text chat vs guided choices? (Pure
  chat feels smarter; guided choices are more reliable and more on-brand for "strict
  but open".) A hybrid (chat with occasional quick-reply chips) is the likely answer.
- Does the user see the plan as prose, as a visual map of entities/actions, or both?
- For the video: one polished example app end to end, or a short montage of two very
  different apps from the same flow (to prove open-endedness)?
