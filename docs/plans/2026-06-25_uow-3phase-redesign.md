# UoW Governed-Development redesign — the 3-phase refinement-session model

> **Status: DESIGN v1 — FINAL (2026-06-25). All clarifications resolved.** No code yet —
> tokens reserved; ready to build when scheduled. Routines are a **separate follow-on story**
> (§8) that reuses these components.
> This supersedes the current 6-stage gated lifecycle in the Governed Development console.

## 1. The reframe (why this exists)

The current console models a UoW as a **gated 6-stage pipeline** (Intake → Investigating →
Decisions Approved → Development → Awaiting QA → Signed Off) that auto-advances on click and
blocks development behind a "decisions approved" gate. That was overbuilt.

**The real shape of the work is a refinement session.** The architect (you) is the
**principal architect / lead engineer**; the investigation and development agents are the
**senior developer**. The phases are where you organize your thoughts *as the delegator* —
converse, answer clarifying/approval dialogs, add context — then say *go*. Each phase is a
UI wrapper over exactly what you do with Claude Code by hand, split into investigation vs.
development with buttons for the instructions you'd normally just type.

There are **three phases: Intake · Investigation & Refinement · Development.** That's it.

Two ideas make it coherent:

- **Phases are freely navigable views, decoupled from execution.** Selecting a phase only
  changes the screen — it never auto-advances and never runs anything. Soft structure comes
  from a deliberate **Finish [phase]** button (greys the phase read-only, keeps the history),
  with a deliberate **Reopen [phase]** to re-enable it. Nothing is *enforced* in order; the
  Finish/Reopen acts are how you separate "what I'm doing now" from "what I've settled."
- **The UoW is one accumulating transcript.** Story + comments + free-text + investigation
  findings/dialogs/chat + development output/clarifications/bug-fix chat + artifacts all live
  in **one UoW state**, appended in real time. Every agent and the assistant read it live.

> **Future (out of scope here):** voice mode + text-to-speech so the refinement session is
> literally conversational. Designed-for but not built in this story.

## 2. Persistent top bar (visible in every phase)

| Control | Behavior |
|---|---|
| **UoW status** | **Informational display only** — shows the UoW's current status for the developer to see. Not a control, separate from the phase selector. |
| **Pull latest work item** | Re-pull this issue from the tracker (full refresh, no cache). |
| **Lifecycle phase selector** | Free navigation between **Intake · Investigation & Refinement · Development**. Selecting one shows that screen; it never runs anything and never advances. A *Finished* phase shows greyed/read-only with its history until *Reopened*. |

**Removed:** the **Gate self-check** box ("Run gate self-check"). This removes the self-check
**UI affordance only** — the **Layer-1 deny-before-execute gate still wraps every
development/bug-fix agent write**, unchanged (§9).

## 3. Phase 1 — Intake

"What is this work, and what do I want the senior-dev agent to know."

- **See the story + comments inline, in full** — title, body, and the comment thread rendered
  right here.
- **Add a comment to the story** — posts back to the tracker (@-mention autocomplete reused).
- **Free-text context for the next agent** — extra context for the **investigation** agent;
  saved into the UoW state.
- **Select the repos + branches this story touches (scoping).** A project can hold multiple
  repos (FE, BE, services, …). The user picks **which repos are in scope** and **which branch
  per repo** — if the project has FE + BE + Services but the story doesn't touch Services, pick
  only FE + BE. This is a **context-cost control**: out-of-scope repos are **not** mounted into
  the agents' read grounding, so they aren't bloated with irrelevant context (wasted tokens).
  Only in-scope repos drive the per-repo story branches and the orchestrator's fan-out (see the
  fleet doc R3/R6).
- **Per in-scope repo, choose the branch mode** — either **work off an existing branch** in
  that repo, or **create a new UoW-specific branch** from a chosen base. Both are first-class
  options (a fresh per-UoW branch is the common case; working off an existing branch is fully
  supported).
- **Update the branch — per selected repo.** The AI-assisted "Update branch" control runs
  **for each in-scope repo**: pull a chosen source branch into *that* repo's story branch
  (clean merge commits, conflicts resolved by a gated agent). Each selected repo can pull from
  a different source branch.
- **Finish intake** — greys the above read-only, keeps the history visible. **Reopen intake**
  re-enables it.

## 4. Phase 2 — Investigation & Refinement

The agent **investigates** the code + requirements, then the **refinement session** begins —
a back-and-forth with the senior-dev agent that accumulates context and resolves
clarifications/approvals. No auto-advance, no separate blocking gate stage. (The name carries
both halves: investigate first, then refine.)

1. **Begin investigation** → live **agent activity** (Bombe + streamed run events): the agent
   reads the story + the full repo and works out the lay of the land.
2. When the run finishes → a **findings readout** is spawned into the screen.
3. **Clarification / approval dialogs** — the agent formats what it needs **clarified and/or
   approved** as structured dialogs (single- or multi-select recommendations **+ "Other"**
   free-text), exactly like the dialogs Claude opens for you. Your answers ARE the "approved
   decisions," appended to the UoW context.
4. **Free-text chat** runs alongside — a real back-and-forth for any extra context unrelated
   to the dialogs. This is where you organize your thoughts as the delegator.
5. **Comment on the story (board-visible)** — comments you make during refinement post back to
   the tracker, so the **product team can see them on their board**. Refinement isn't a closed
   loop; it can pull in the people who own the requirement. (Same board-comment capability as
   Intake §3, surfaced here because commenting *is* part of refining.)
6. **Settle the contract — when the story's work crosses a contract boundary.** If the work
   changes **both sides of a shared interface** (an API + its caller, a service + its consumer,
   a shared schema/type + its users), a **contract must be settled here before Development.**
   The contract is **free-form prose written into the story** — "whatever the story calls for";
   no formal schema, no protocol-specific document (REST, GraphQL, gRPC, a shared schema — all
   just prose). The **refinement agent may draft it** (so you needn't enumerate every field);
   you review/approve it like any other decision. It lives in the UoW state and is what the
   **cross-repo integration gate reads and checks the assembled code against** — a **new
   agent-driven, cross-repo check, NOT a per-repo mechanical rule** (per-repo rules can't span
   repos; fleet doc R3.e/R3.g). **Required only when contracts are in scope** — a frontend-only
   bug fix, a single-repo refactor, or any change confined to one side needs **no contract**. A
   boundary-crossing story that reaches Development with **no contract → the orchestrator
   refuses and pushes back** to this phase.
7. **Finish investigation & refinement / Reopen** — same lock/unlock as Intake.

In essence this is a **refinement session for one story** — investigate, clarify, decide,
discuss, and refine the requirement (including back to the board). A *batch* refinement
routine over many stories is a future routine (§8); the essence of the session is what's in
scope here.

**Agent context boundary (critical, per §7):** the investigation/refinement agent is
**stateless** and sees **only** (a) this UoW's full transcript (story + comments + everything
above) and (b) **full read access to the entire project repo code** — and is **blind to other
UoWs and other stories.** (The "create a new story" agent's blindness last night is the bug
this prevents: every working agent gets the whole repo, scoped to this UoW only.)

## 5. Phase 3 — Development

"Go build it," plus test + ship.

1. **Begin Development** — the button is **always available**, regardless of whether
   Intake/Investigation ran; straight-to-build is allowed. The **one precondition** is the
   contract gate (§4.6): if the orchestrator determines the work **crosses a contract boundary**
   and **no contract exists** in the UoW, its first act is to **refuse and push back** to
   Investigation & Refinement (where the contract can be drafted). Work that **doesn't cross a
   boundary** — a frontend-only fix, a single-repo change — proceeds straight to build with no
   contract needed.
2. Once begun → **agent activity** shown.
3. **Clarifications pause the run** — on an ambiguity the agent spawns a dialog and **pauses
   the story**; you answer (single/multi-select or "Other") and can **chat back and forth**.
   Answering resumes the work.
4. When done → the agent **outputs what it did**, so you can **go test the branch** yourself.
5. **Bug-fix loop** — find a bug, **free-text it right here**, and a gated agent attempts the
   fix (a follow-up gated run on the same branch; Development is re-runnable).
6. **Layer-2 runs automatically at the end of the development cycle** and **bounces failures
   back to the agent** to fix — **no button**. (The existing in-loop layer-2 bounce, kept.)
   **Layer-3 — the agentic code reviewer (opt-in, model-selectable)** runs **with / parallel
   to** Layer-2: an AI reviewer that sees **the story (requirements · contract · integrations)
   + the selected rules + the diff**, and checks the code against **both the rules and the
   story's intent** — like a human reviewer. Its isolation is from the **other agents**: it is
   **blind to the investigation/developer/orchestrator contexts**, so it can't rubber-stamp the
   implementer's reasoning. It enforces **architectural** quality, not just mechanical checks.
   It's a **project setting (on/off + which model)**; **when off, you are the reviewer.** Both
   L2 (mechanical) and L3 (agentic) bounce back to the agent.
   **Contract coherence is separate and always on** (the orchestrator's integration duty, fleet
   doc R3.e/R7) — it does not depend on L3. At **Ship**, the work hands off to **Layer-4** (the
   repo's GitHub PR/CI). The full four-layer model + the rules-as-SSOT story:
   `docs/ENFORCEMENT_MODEL.md`.
7. **Ship** — a state-aware panel (design for the architect's nod), **per in-scope repo**:
   - A **base-branch picker** (select before pushing).
   - `[ Push branch ]` (always enabled) · `[ Open PR ]` (enabled after push) · `[ Comment
     link on story ]` (enabled after PR) — each gated on the prior step's completion.
   - `[ Ship all → ]` — runs push → open-PR → comment in sequence with the chosen base,
     halting on first failure. Inline state: `pushed ✓ · PR #42 ✓ · commented ✓`.
   - **Multi-repo:** a story that touches N repos shows **one Ship row per repo** (each ships
     its own branch/PR onto its own base); a top-level `[ Ship all repos → ]` chains them. The
     fan-out guarantees each repo's branch is already coherent before this point (fleet doc
     R3.e–f).
8. **Done** — a deliberate action meaning "the developer considers this done." It greys the
   **entire UoW read-only** and **archives** it. A done/archived UoW is **never deleted** —
   deletion is a separate, explicit act (the trash + confirm we already built).

## 6. Overarching principles

1. **One UoW state holds everything**, updated in real time — all chat, dialogs, answers,
   findings, run output, and artifacts.
2. **Two chats, two scopes (kept separate, by design — §10):**
   - **Floating assistant** — the **broader** context: the active project, pulled issues, dev
     state across *all* UoWs, scan, rules. A read-only helper.
   - **Per-phase agent chat** — the **narrower, working** agent: confined to **this UoW +
     full repo**, blind to other UoWs/stories (§4). It does the work.
3. **Components are as reusable as possible** so **routines reuse them** in a *batched*
   posture (§8), not a rewrite.
4. **The refinement-session premise:** architect/lead-engineer ↔ senior-developer agents. The
   UI structures the thoughts-and-delegation loop you already run by hand.

## 7. State / data model (the unified transcript)

The UoW state is the single source of truth:

- `intake`: story snapshot, comment thread, free-text-for-investigation, **repo/branch scope**
  (the in-scope repos, and per repo the **branch mode** — existing branch vs. new-from-base).
- `investigation`: findings readout, the clarification/approval dialogs (Q + chosen
  answer/option/other), the free-text chat transcript, and **the contract artifact** (present
  only when the work crosses a contract boundary — §4.6).
- `development`: dev-run output(s), the clarification dialogs, the bug-fix chat, layer-2
  results/bounces.
- `artifacts`: branch, PR number/URL, ship state (`pushed`/`pr`/`commented`).
- `meta`: status (informational), viewed-phase, per-phase finished/reopened flags,
  done/archived flag.

**Every working-agent turn is stateless** and grounded on **(this UoW's full transcript) +
(full repo read access)** — and **nothing else** (no other UoWs, no other stories). The
floating assistant is grounded more broadly (§6.2). Repo + rules grounding is the work
already shipped; this story tightens the *scope* to one UoW for the working agents.

## 8. Routines — a separate follow-on story (captured here for reuse)

Routines are **not** "less oversight." They are the **same 3-phase flow run over a backlog of
UoWs, asynchronously, with human-interaction points collected into a triage backlog** instead
of blocking inline — and **escalation rules always still apply.**

Per-UoW state machine inside a routine:

- **Not yet investigated** → investigate → emit findings + clarification dialogs → park as
  **"awaiting clarification."**
- **Awaiting clarification** → wait for the human (answered in a batch, e.g. next morning).
- **Clarified (dialogs answered)** → develop.
- **Development raised an escalation** → **stop + log** → park as **"escalated."**

The human opens the routine and works the **local triage backlog**: read findings, clarify
many UoWs in one sitting, address escalations. The routine continues on its next scheduled
run. Example: a **bug-triage** routine pulls 10 bugs, investigates all, reports findings +
clarifying questions; you clear all 10 one morning; the routine develops them that night,
escalating where needed.

**Governance is never the thing that's "less."** Two hard rules:
- **Any development work done inside a routine of any kind goes through this full 3-phase
  process** — the Layer-1 gate, the clarification/escalation flow, and the automatic Layer-2
  bounce all apply. A routine may carry an explicit **"bypass full process" selection, but it
  is OFF by default.** A routine that does no development work is simply unaffected by this
  design.
- **Routines are free-text prompts today**; structured **routine templates** (forms whose
  inputs generate the prompt — bug-triage being one envisioned template) come later. Either
  way the *batched* posture is only about **where the human-interaction points go** (a triage
  backlog vs. inline), never about skipping governance.

**Reuse implication for this story's components:** every human-interaction component
(`ClarificationQA`, `AgentChat`, escalation surfacing, `AgentActivity`) must support an
**`oversight` mode**:
- **`Interactive`** (the dev console): dialogs/escalations **block inline**, answered now.
- **`Batched`** (routines): dialogs/escalations are **emitted to a backlog/queue**,
  non-blocking, answered later. Escalation policy is always active in both.

Designing the components around this single prop now is what makes routines a reuse.

## 9. Reusable component inventory

| Component | Used by | `Batched` (routine) behavior |
|---|---|---|
| `PhaseShell` (top bar + free phase nav + Finish/Reopen) | all phases | routine engine walks UoWs; no manual nav |
| `AgentActivityStream` (Bombe + live events) | investigation, development, bug-fix, update-branch | rendered to the routine run log |
| `ClarificationQA` (single/multi-select + Other) | investigation, development | **emit to triage backlog** instead of blocking |
| `AgentChat` (back-and-forth gated agent, this-UoW+repo scope) | investigation, development | suppressed/auto per policy; transcript still recorded |
| `ContextLog` (UoW transcript renderer) | all phases + floating assistant | identical |
| `BranchOps` (update-branch) | intake, development | applied per routine merge policy |
| `ShipControls` (base-picker · push · PR · comment · ship-all) | development | gated by routine ship policy (auto or hold) |
| `EscalationSurface` | development (+ routines) | **always** active; routine logs + parks "escalated" |

## 10. Removed vs. preserved

**Removed (this surface):**
- The **Gate self-check** UI box (enforcement stays — §2).
- The **Decisions-Approved** stage as a *blocking gate* — replaced by **clarification/approval
  dialogs** inside investigation/development (§4.3). Decisions/approvals still exist; they're
  just dialogs, not a pipeline gate.
- **Awaiting QA** and **Signed Off** as stages — replaced by **automatic end-of-cycle Layer-2
  bounce** (§5.6) and the **Done** action (§5.8).
- Auto-advance on click.
- The earlier draft's manual "Run final tests" button (Layer-2 is automatic — §5.6).

**Preserved (unchanged enforcement):**
- **Layer-1 deny-before-execute gate** on every development/bug-fix agent write.
- **Layer-2** auto-bounce at the end of the dev cycle.
- **Per-UoW isolated worktrees**, the **PR lifecycle**, **multi-repo full-read grounding**
  (now **scoped per story** to the selected repos — §3), and the **Stop/cancel** control.
- **One clean branch per touched repo** as the ship-time invariant (fleet doc R3.f) — Ship is
  per-repo (§5.7).

## 11. Migration (existing UoWs)

- `Intake` → Intake phase.
- `Investigating` / `DecisionsApproved` → Investigation & Refinement phase (prior decisions
  surface as context, not a gate).
- `Development` / `AwaitingQa` → Development phase.
- `SignedOff` → Development phase, status = Done/archived.
No data lost; the old stage maps to a viewed-phase + status.

## 12. Resolved decisions (from the 2026-06-25 clarification round)

1. Gate self-check: remove the **UI button only**; Layer-1 enforcement stays. ✓
2. Top-bar UoW status: **informational display only**, not a control. ✓
3. Free navigation, with deliberate **Finish/Reopen** per phase; Development is re-runnable;
   bug-fix = a gated dev run on the same branch; Update-branch lives in Intake. ✓
4. Working agents are **stateless**, scoped to **this UoW + full repo**, blind to other
   UoWs/stories. ✓
5. **Decisions/approvals stay** — as clarification/approval **dialogs** (single/multi + other),
   not a blocking stage. ✓
6. **Layer-2 automatic** at end of dev cycle, bounces back — **no button.** ✓
7. **Done = read-only + archived** (developer's call), never deleted; delete is separate. ✓
8. **Ship:** the §5.7 panel — base-branch picker + per-step buttons (each gated on the prior
   step's completion) + a `Ship all →` chain that runs push → PR → comment. **Approved.** ✓
9. **Routines** = batched-oversight reuse over a triage backlog; **development work in any
   routine still runs the full governed 3-phase flow** (per-routine bypass is opt-in, OFF by
   default); escalation always applies. Free-text now, templates later. Separate follow-on
   story (§8). ✓
10. **Two chats**, different scopes (floating = broad; per-phase agent = this-UoW + repo). ✓

## 13. Acceptance criteria

- The console shows **3 freely-navigable phases**; selecting one never runs or advances.
- **Finish/Reopen** locks/unlocks each phase (read-only history preserved).
- Each phase's spec (§3–§5) works; every chat/dialog/answer/output **persists in the UoW state
  in real time**; the floating assistant reads the full state live; the **per-phase agent is
  scoped to this UoW + full repo and nothing else**.
- **Begin Development is reachable from a fresh UoW** with no prior phases.
- **Per-story repo/branch scoping** works at Intake: the user selects in-scope repos + a branch
  per repo (**existing branch or new-from-base**); out-of-scope repos are not grounded (no
  context bloat); "Update branch" runs per selected repo; Ship is per-repo; a multi-repo story
  ends as **one coherent branch per repo**.
- **Contract gate (scoped):** when — and **only when** — the story's work crosses a contract
  boundary, a **contract artifact must be settled in Investigation & Refinement before
  Development**; the orchestrator **refuses + pushes back** if it's missing; the refinement
  agent may draft it; the integration gate validates the assembly against it. Single-side work
  (e.g. a frontend-only bug fix) requires no contract.
- **Layer-2 runs automatically** at the end of the dev cycle and bounces; **no self-check
  button**; the **Layer-1 gate is provably intact** (gateway jail tests pass).
- **Done** greys the whole UoW read-only + archives (never deletes).
- The §9 components carry the **`oversight: Interactive | Batched`** prop so routines reuse
  them without a rewrite.
- **Development work inside any routine** runs the same governed 3-phase flow (Layer-1 gate +
  automatic Layer-2 bounce); the per-routine **bypass is opt-in and OFF by default**;
  non-development routines are unaffected.
- Existing UoWs migrate (§11) with no data loss.
