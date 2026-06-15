# Story decomposition: split a parent story into component stories by practice

Date: 2026-06-15
Status: Accepted (design); NOT built.
Deciders: Zach (architect), Claude (architect)

Companion docs: [`WORKTRACKER_INTEGRATION.md`](../WORKTRACKER_INTEGRATION.md),
[`VISION.md`](../VISION.md), ADR [`cockpit_story_view_ux`](2026-06-15_cockpit_story_view_ux.md).

## Context (a real workflow this should own)

On a real team you rarely get a clean, build-ready story. You get a feature with
just enough information, and a human turns it into the component stories the work
actually needs. Zach's current flow at work is exactly this: take a feature, feed an
AI agent the context of the applicable repos plus the team's story templates, and have
it generate the child stories (typically a UI story and an API story, sometimes more).
Camerata should do this natively: take a parent and produce the component stories,
where "what splits into what" is configurable to the org's practice.

## Decision: a configurable decomposition step, human-reviewed before it commits

Camerata ingests a parent work item and proposes a set of child stories, governed by a
**decomposition practice** the org configures.

- **The practice is data, not hardcoded.** A practice declares parent-type to
  child-types mappings: `Feature -> [UI story, API story, ...]`, `User Story -> [tasks]`,
  or whatever the org runs. Levels and names are the org's. A team that goes
  feature -> story -> task configures two levels; a team that goes feature -> UI/API
  configures one. Camerata does not impose a hierarchy.
- **Templates per child type.** Each child type carries a story template (the skeleton
  shape: title pattern, sections, acceptance-criteria scaffold) so generated children
  match how the team writes stories.
- **Repo context informs the split.** The decomposer reads the applicable repos (the
  code-host axis) to ground the children: which components/repos a feature touches
  drives which child stories exist (a feature touching the API repo and the UI repo
  yields an API child and a UI child). Multi-repo is normal.
- **Edit in Camerata, THEN push back (review-then-create).** The architect sees the
  proposed children and edits / adds / removes them inside Camerata first. Nothing is
  written to the tracker until they approve. An agent never writes work items
  unsupervised.
- **Children are pushed back to the tracker AS those story types.** The approved
  children are created as real work items in the integrated service (GitHub, ADO,
  Jira) of the correct TYPE per the practice: a UI story becomes a real UI-typed story
  / issue, an API story an API-typed one, a task a task. They are not Camerata-only
  artifacts; they land in the board the team actually uses.
- **The write-back carries the relationship metadata.** Each created child sets the
  correct parent / child / related links in the tracker's own model, so the hierarchy
  is real on their board, not just in our spine: ADO work-item link types
  (Parent/Child, Related), GitHub sub-issues / task-list links / "relates to", Jira
  sub-tasks and issue links. Getting these relationships right is a first-class
  requirement of the write-back, not an afterthought.
- **Parent/child also lives on our spine.** Children carry a `parent_id` on the
  canonical Story and each is independently adoptable and governable; the spine and the
  tracker mirror the same hierarchy per the existing `SyncPolicy`.

## Where it sits in the workflow

Decomposition is an upstream enrichment step, before execution: intake / adopt a
parent -> DECOMPOSE into children -> adopt + govern each child through the normal
loop. It pairs with the clarify-bridge (both are pre-execution refinement of a story
into something buildable).

## Honest current state

Design only; not built. Prerequisites that do not exist yet: a parent/child field on
the canonical Story spine, a decomposition engine (the agent step that reads repo
context + templates and proposes children), the practice-config model, and the
child-write-back per provider. The spine itself (`StoryStore`) is new as of Phase 1
and would need the parent/child relation added. Notably, the `WorkItemProvider` trait
today has `ingest_story` / `push_status` / `post_clarifying_questions` / `poll` but NO
"create a child work item of type X with these relationship links" capability; the
write-back described here is a contract extension, not just a `push_status` call.

## Open questions

- Where the practice config lives (per-org config file? a Camerata setting? read from
  the tracker's own hierarchy where it exists, e.g. ADO's process template?).
- How much repo context the decomposer needs to propose good children without
  over-reading (the same convention-extraction problem as brownfield onboarding).
- Whether decomposition and brownfield onboarding share the repo-review machinery
  (both read repos to ground their output).
