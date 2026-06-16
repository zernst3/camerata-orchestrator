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

## Honest current state / phasing

- **This foundation:** the `Project` container + project store (the home for
  cross-repo/process/custom rules — the persistence the question identified) +
  ruleset export/import JSON + a project selector. The gates will read project-level
  rules from the store.
- **Phased:** the full rules-management screen UI, the custom-rule editor, the
  single-emit upsert wiring into arm (re-emit on edit), and drift detection. The
  emit format itself (AGENTS.md/CONVENTIONS.md + gate config) is already built in
  `arm`; the rules view re-uses it.
