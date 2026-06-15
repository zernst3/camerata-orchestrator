# Cockpit story-view UX and the work-tracker working set

Date: 2026-06-15
Status: Accepted (design); implementation phased (see Build order). PoC target: GitHub.
Deciders: Zach (architect), Claude (architect)

Companion docs: [`WORKTRACKER_INTEGRATION.md`](../WORKTRACKER_INTEGRATION.md) (the
provider port and the two axes), [`RATIONALE.md`](../RATIONALE.md), [`VISION.md`](../VISION.md).

## Context

Real story trackers are huge and messy. A working Azure DevOps board has hundreds
of work items at every stage of refinement, every status, assigned to many people,
reassigned constantly. The cockpit's left-rail story spine has to coexist with that
reality without becoming a worse copy of the tracker. This note records how the
story-view behaves, how stories enter it, how the Architect interacts with a ticket
(posting an agent's clarifying question as a comment and tagging the right person),
and how all of that stays provider-neutral.

## Decision 1: a governed working set, not a mirror of the board

The cockpit is **not** a second board. The mental model is a git working tree, not
the whole remote: the spine shows only the stories the Architect has explicitly
*adopted* into Camerata to govern, a small deliberate working set. Everything else
stays in the tracker, invisible to the cockpit. We never sync or poll the whole
board. Once a story is adopted we watch only that item (and its comments) for inbound
events; polling and webhooks are scoped to the working set, never the board.

Corollary product boundary, stated explicitly:

> The tracker (ADO / Jira / GitHub) is the backlog and refinement tool. Camerata is
> the governed-execution tool for stories that are already refined enough to build.
> Camerata is not where you groom the backlog.

Under-refined tickets simply are not adopted yet. This resolves most of the
"tickets at various stages of refinement" problem by leaving refinement where it
already lives.

## Decision 2: provider-neutral, two axes, user-configurable; PoC is GitHub

The behavior in this note is identical across **Azure DevOps, Jira, and GitHub**
(scrum/boards and/or issues), selected and configured by the user, behind the one
`WorkItemProvider` port. Nothing in the cockpit knows which backend it is talking to.

There are two independent axes (already established in WORKTRACKER_INTEGRATION):

- **Board axis** (where stories live): ADO Boards work items, Jira issues, GitHub
  Issues / Projects v2 items, or the built-in native tracker.
- **Code-host axis** (where code lives): GitHub repos, ADO Repos. Jira has no code
  host and pairs with one of those.

The two can be the same system or different. GitHub is special because it serves
**both** axes at once (Issues as the board, the repo as the code host), which is why
it is the PoC target: it is the smallest end-to-end loop and it is what Zach uses for
personal projects. ADO must also be supported on both axes (it has Boards and Repos);
it is the higher-friction provider and lands after GitHub.

## Decision 3: how a story enters the working set (intake paths)

Three paths, built in this order:

1. **Explicit adopt by id or URL (MVP).** The Architect pastes a work-item id /
   issue number / URL; Camerata calls `ingest_story` once and it joins the spine.
   Deterministic, scoped, zero polling. This is the primary path.
2. **Scoped picker over a saved query (high-value convenience).** Rather than typing
   ids, the Architect runs a bounded query and picks results to adopt. This is a
   one-time fetch on demand (a search-and-import modal), not continuous polling, and
   it leans on the tracker's own filtering rather than reinventing a board. The
   canonical real-world filter to support first (Zach's ~99% case while actively
   developing) is **"in the current sprint AND assigned to me"** (or a custom
   "Developer" field equal to me). Per provider:
   - ADO: WIQL, `[System.IterationPath] UNDER @currentIteration AND [Assigned To] = @me`
     (or a custom developer field).
   - Jira: JQL, `sprint in openSprints() AND assignee = currentUser()`.
   - GitHub: search, `assignee:@me is:open is:issue` plus a milestone or a Projects v2
     iteration for "sprint."
   Saved queries should be selectable so the Architect picks a named filter rather
   than re-typing it.
3. **Opt-in tag or board column (later).** Only items the team explicitly flags flow
   in, e.g. an ADO tag `camerata`, a Jira label, a GitHub label, or a "Ready for
   Camerata" column. Keeps any auto-intake bounded and opt-in.

All three converge on one rule: **the Principal Architect curates the working set,
a small number of stories at a time.** This is by design, not a limitation. Each
adopted story gets its own curated RuleSet and a governed fleet run, which is not
something to fan out across hundreds of tickets automatically.

## Decision 4: reconciling volume within the adopted set (the spine UX)

- **Group and sort by Camerata's `FeatureStatus`, not the tracker's columns.** The
  spine answers "where is this in OUR governed pipeline" (Investigating, Executing,
  AwaitingClarification, Blocked, AwaitingQa, SignedOff). Tracker states map into
  `FeatureStatus` on ingest, and our status maps back out on `push_status` per the
  per-field `SyncPolicy`.
- **The NEEDS YOU queue is the primary working surface**, the antidote to overload.
  It floats only items needing the Architect's action now (a PO answer returned, a
  diff ready to QA, a blocker). The Architect works the queue; the full spine is
  there when wanted.
- **Each spine item shows the tracker link, the assignee, and the designated
  respondent**, with a one-click jump back to the tracker (provenance plus escape to
  source).
- **Done / signed-off stories archive off the active spine** (behind a filter) so the
  working set stays small on purpose.

## Decision 5: interacting with a ticket (the clarify-bridge)

When an agent's investigation produces a clarifying question:

1. It surfaces in the cockpit (NEEDS YOU / center stage).
2. The Architect **reviews and edits it before anything leaves.** An agent never
   posts to a real work item unsupervised; the Architect controls what goes out.
3. On "Ask the respondent," Camerata calls `post_clarifying_questions`, which writes
   a formatted comment on the work item, mentioning the respondent (see Decision 6).
4. The respondent gets their normal tracker notification, replies in the comment
   thread (their habitat, phone or browser, no Camerata account).
5. Camerata's webhook (Service Hooks / Jira webhooks / GitHub webhooks), with `poll`
   as fallback, pulls the reply back, attaches it as a Provenance source
   (comment id, url, author = the auditable `human_decision`), and unblocks the story.

Privilege split: **product** questions route outward to the respondent via the
tracker; **technical** tradeoffs and the RuleSet stay with the Architect, local. The
respondent can answer and sign off, never execute. The Architect is the gatekeeper.

## Decision 6: "respondent" is just an addressee picker, NOT a PO role

The hard problem: who do we @-mention on the comment? The naive answer ("read the PO
off the ticket") breaks on real teams. Many teams have no PO at all; stories get
reassigned constantly and mentioned to people with no prior history on the ticket. So
we cannot depend on the tracker carrying a clean, reliable "product owner" field.

Decision: there is **no PO role in the product**. The "respondent" is nothing heavier
than the **addressee of a specific question**, a to-field the Architect fills in per
question, defaulting to a smart guess and always overridable. It is chosen at the
moment of asking, not assigned once as a role, so it survives the tracker tossing
assignment around (you pick who actually owns this answer, this time).

The composer, when an agent question is ready:

```
┌─ NEEDS YOU · story CAM-142 (GitHub #142) ─────────────────┐
│  The investigation needs an answer before building:       │
│  ┌─────────────────────────────────────────────────────┐ │
│  │ Should the CSV export include archived members, or  │ │  ← editable
│  │ only currently active ones?                         │ │
│  └─────────────────────────────────────────────────────┘ │
│                                                           │
│  Ask:  [ @maria-pm ▾ ]   ← addressee picker, scoped to    │
│        suggested from issue #142:        this ticket      │
│          • @maria-pm   (assignee)        ← default guess  │
│          • @jdoe       (reporter)                         │
│          • you         (architect)                        │
│          • + type a handle…                              │
│                                                           │
│  Posts as a comment on GitHub #142, @-mentioning your     │
│  pick. They reply in GitHub; the answer returns here.     │
│                                                           │
│        [ Post to GitHub ]        [ Answer it myself ]     │
└───────────────────────────────────────────────────────────┘
```

The picker is pre-populated with the people Camerata pulled off THIS ticket, plus
"you," plus free-type. The default is the best guess; you override. That is the whole
"respondent" concept. It is optionally remembered per story as a convenience (so the
next question on the same story pre-fills the same addressee), but nothing requires a
stable owner.

- **Seed, do not depend.** When a story is adopted, Camerata suggests a respondent,
  ranked from whatever the tracker happens to offer: (1) an explicit custom field if
  the team has one (an ADO "Product Owner" field, a Jira reporter), (2) the current
  assignee, (3) the reporter / created-by, (4) the most recent human commenter. These
  are suggestions the Architect confirms or overrides. None is required.
- **Resolve to a mentionable identity per provider.** GitHub is trivial: the handle
  IS the identity (`@username`), which is another reason it is the PoC target. ADO and
  Jira need a user-search lookup (ADO identity by email/UPN; Jira `accountId`).
- **Fallback ladder, never block.** If no respondent is set, or identity resolution
  fails, post the comment WITHOUT a mention and raise it in NEEDS YOU so the Architect
  pings the person through their normal channel. Mention resolution is best-effort,
  never a gate on getting the question posted.
- **No-PO teams (the common case).** Because the addressee is picked per question,
  the Architect simply asks whoever currently owns the answer (themselves, a rotating
  teammate, a stakeholder), at the moment of asking. There is no "PO" to configure and
  no dependency on the tracker carrying reliable owner metadata.

## Provider capability matrix

| Capability        | GitHub (PoC)                          | Azure DevOps                              | Jira                                  |
|-------------------|---------------------------------------|-------------------------------------------|---------------------------------------|
| Board / story     | Issues (or Projects v2 items)         | Boards work items                         | Issues                                |
| Code host         | the repo (same place)                 | ADO Repos                                 | none (pairs with GitHub/ADO/Bitbucket)|
| Adopt by id       | issue number / URL                    | work-item id / URL                        | issue key / URL                       |
| Scoped query      | search (`assignee:@me`, milestone)    | WIQL (iteration + assigned-to)            | JQL (`openSprints()` + `currentUser()`)|
| Comment           | issue comment                         | work-item comment                         | issue comment                         |
| Mention identity  | `@username` (trivial)                 | identity by email/UPN (lookup)            | `accountId` (lookup)                  |
| Inbound events    | webhooks (+ poll)                     | Service Hooks (+ poll)                    | webhooks (+ poll)                     |

## Build order (PoC first)

1. **GitHub, explicit adopt-by-id + the comment round-trip.** Adopt an issue, post an
   agent question as a comment with an `@username` mention, pull the reply back as
   provenance. Smallest end-to-end proof, and GitHub doubles as the code host.
2. **GitHub scoped picker** for "assignee:@me + open" (the everyday filter).
3. **ADO** on both axes (Boards + Repos), then **Jira** (board only). These add the
   user-identity lookup for mentions and provider-specific query languages.
4. **Mention-identity resolution** is treated as a follow-on after the comment-post
   works, so the round-trip can be demoed even before identity resolution is perfect
   (the fallback ladder covers the gap).

## Open questions

- "Sprint" has no native GitHub concept; for GitHub, map it to a milestone or a
  Projects v2 iteration. Decide the default when neither is configured.
- Whether the scoped picker should persist adopted-from-query stories or re-resolve
  the query each session (leaning: adopt is a copy-in, the spine is canonical).
- Per-provider auth and webhook setup friction (GitHub App vs PAT; ADO Service Hooks)
  is deferred to the Phase 4 live-wiring work and is gated on real credentials.

## Note on enterprise adoption

The PoC targets personal GitHub. ADO and Jira are first-class in the port design
because the enterprise board is where a tool like this would be most useful, but
whether a given employer would permit an external agent tool to read tickets, post
comments, and touch repos is an organizational governance and security question,
separate from the technical design captured here.
