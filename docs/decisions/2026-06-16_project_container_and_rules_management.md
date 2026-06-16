# The Project container + the single-source-of-truth ruleset

Date: 2026-06-16
Status: Accepted (design). Foundation building; full rules-management UI phased.
Deciders: Zach (architect), Claude (architect)

Companion docs:
[`brownfield_onboarding_flow`](2026-06-15_brownfield_onboarding_flow.md),
[`credential_delegated_scope_and_build_targets`](2026-06-15_credential_delegated_scope_and_build_targets.md),
[`process_rules_and_vcs_action_gate`](2026-06-15_process_rules_and_vcs_action_gate.md),
[`cross_agent_integration_gate`](2026-06-15_cross_agent_integration_gate.md),
camerata-ai `src/emit.rs` (the emit/selection/custom/lock reference).

## 1. The Project is the foundational data container

A **Project** is the top-level containment — the Azure-resource-group of Camerata.
Everything hangs off it:

- the **repos** in scope (the credential reaches them; the project groups them),
- the **ruleset**: per-repo rule selections (with chosen alternatives), the
  cross-repo rules, the process rules, and the architect's **custom rules**,
- per-repo and project **settings**.

The user **switches between projects**; all of the above is scoped to the active
one. Stories, runs, routines, onboarding, and the rules view all read the active
project. (This generalizes the per-process single-context model we have today.)

## 2. Where rules persist (two homes, one source of truth)

| Rule kind | Persisted in | Read by |
|---|---|---|
| repo-local (content, selected architectural) | each repo's `.camerata/rules.json` + `AGENTS.md`/`CONVENTIONS.md` | the in-repo CI gate + the consuming agent |
| **cross-repo** (API contracts) | **the project store** | the integration gate |
| **process** (commit format, `AB#{id}`) | **the project store** | the VCS-action gate |
| custom rules | the project store (emitted into repos per their domain) | both |
| the selection source-of-truth (rule → chosen option) | the project store | emit |

This answers the open question: the **non-repo rules have no home in any repo** — by
construction they span repos or are account-level, so they live at the **project
level** (the engine's gates read them from the project store), NOT in a `.camerata/`
file. Today the orchestrator has `camerata-persistence` (SQLite); the project store
rides it.

## 3. The ruleset is one source of truth; edits are an upsert

The project's ruleset is the single source of truth. Editing it — in the brownfield
flow OR the standalone rules-management screen — produces **one emit** that upserts:

- the **repo files** (`.camerata/rules.json` + AGENTS.md/CONVENTIONS.md) for each repo
  the rule is bound to, and
- the **project config** (the cross-repo + process rules the gates read).

Critically, the upsert **never clobbers custom rules**. Modifying an alternative,
deleting a base rule, or adding a base rule changes only the base set; the
architect's custom rules (camerata-ai's `CustomRule { name, body, domain }`) are
carried through untouched. This is the thing that "never made it into camerata-ai":
the emit is an upsert over a set that distinguishes base from custom, not a
full overwrite.

## 4. Adopt camerata-ai's rule features in TWO surfaces

camerata-ai already has the rule machinery: export/import the ruleset as JSON
(`selections_json`), custom rules, and drift detection (the lock + `outdated`).
Adopt these in:

- **Brownfield** — the initial scan → select → arm flow (built); export/import the
  proposed/approved ruleset as JSON.
- **A project Rules-management screen** (new, post-brownfield) — see and manage every
  rule currently applied to the project: repo-local (per repo), cross-repo, process,
  and custom. Edit alternatives, add/delete base rules, add custom rules, import a
  ruleset, export the current one. Saving runs the single emit (section 3).

So the rules view is the ongoing control surface over the same project ruleset the
brownfield flow first populates.

## Reconciliation: the repos are ground truth; the bank rehydrates the source

The Rules screen (post-brownfield) must show what is **actually applied** — read from
each repo's emitted `.camerata/rules.json` gate config — not what the project store
assumes. But the emitted files are **lossy** (the adopted directive only), so each
applied rule is **rehydrated by id**:

- a base rule id → the **corpus** source rule (its alternatives + context), so the
  architect sees the full rule and which alternative is chosen, not just the directive;
- a `CUSTOM-*` id → the **project** (its source is the project; see below);
- an id in neither → **drift** (applied in the repo but not in the bank), surfaced.

For the chosen alternative to survive the round trip, the gate config records
`{ id, option }` per rule (not just the id). The reconcile reads the repos (gated on
the token) and rehydrates; `/api/projects/:id/reconcile` returns the applied rules.

## Custom rules: no source, never dropped by an upsert

Custom rules are user-authored: they have **no corpus source** and live only in the
project store (the source of truth). The hard invariant:

> **An upsert/emit must never inadvertently delete a custom rule.** The engine must
> KNOW the custom rule exists in the emit and carry it forward. A custom rule changes
> ONLY when the user explicitly edits it, and leaves ONLY when the user explicitly
> deletes it.

Mechanism: **the emit is always built from the project's full ruleset (base +
custom)**, and `arm` ALWAYS writes the project's custom rules into each repo (as
`### CUSTOM-{name}` in AGENTS.md and as a `CUSTOM-{name}` entry in the gate config,
so reconcile sees them). Re-emitting base rules therefore cannot drop custom — they
are included every time. `Project::merge_custom` (import/edit) replaces a custom rule
by name and never drops an untouched one; `Project::remove_custom` is the only path
that removes one. Tests lock both: a base upsert keeps all custom; `merge_custom`
edits the named one, keeps the untouched one, adds the new one; arm emits the custom
rule into AGENTS.md + the gate config. Custom rules are creatable in both surfaces
(brownfield + the Rules screen — the editor UI is phased).

## Honest current state / phasing

- **This foundation:** the `Project` container + project store (the home for
  cross-repo/process/custom rules — the persistence the question identified) +
  ruleset export/import JSON + a project selector. The gates will read project-level
  rules from the store.
- **Phased:** the full rules-management screen UI, the custom-rule editor, the
  single-emit upsert wiring into arm (re-emit on edit), and drift detection. The
  emit format itself (AGENTS.md/CONVENTIONS.md + gate config) is already built in
  `arm`; the rules view re-uses it.
