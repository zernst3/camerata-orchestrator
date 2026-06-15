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
- **Human-reviewed before commit (review-then-create).** The architect sees the
  proposed children, edits/adds/removes, and approves before anything is created. Same
  posture as the clarify-bridge: an agent never writes work items unsupervised.
- **Parent/child lives on the spine.** Children link to the parent on the canonical
  spine (a parent_id on the Story), and each child is independently adoptable and
  governable. Children can sync back to the tracker as child work items (ADO child
  links, GitHub task lists / sub-issues, Jira sub-tasks) via the `WorkItemProvider`.

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
and would need the parent/child relation added.

## Open questions

- Where the practice config lives (per-org config file? a Camerata setting? read from
  the tracker's own hierarchy where it exists, e.g. ADO's process template?).
- How much repo context the decomposer needs to propose good children without
  over-reading (the same convention-extraction problem as brownfield onboarding).
- Whether decomposition and brownfield onboarding share the repo-review machinery
  (both read repos to ground their output).
