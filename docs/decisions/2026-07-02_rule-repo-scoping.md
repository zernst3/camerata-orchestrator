# ADR: Rule repo-scoping model, adoption, and the in-modal repo picker

**Date:** 2026-07-02
**Status:** Accepted
**Refines:** 2026-06-24 rule selection persistence

## Context

The 2026-06-24 ADR fixed *whether* a rule selection persists across relaunches
(the effect-ordering restore/writeback bug). It did not address *what* a
selection means once persisted: which repos a rule targets, whether an option
pick alone selects a rule, and how a rule's inherent scope decides where it is
emitted.

Three gaps surfaced in use:

1. **Option picks did not always select.** Choosing an option on a rule that was
   not yet in the ruleset did nothing for project-level (process / cross-repo)
   rules. The persist paths only *updated* rules that already lived in one of the
   three ruleset buckets; a fresh `PROCESS-BRANCH-NAMING-1` never reached the
   Applied table.

2. **Two divergent persist paths.** The modal's option pick fired both an
   immediate click handler (`on_option_picked` in `RulesView`) and a
   `chosen_ctx` watcher effect (in `ProjectRulesTable`). A prior fix patched only
   the watcher; the primary click handler still built its ruleset from a captured
   snapshot and only touched already-selected rules. The two paths could disagree
   about what got saved.

3. **Repo scoping was awkward.** A repo-local rule could only be scoped to
   specific repos through a separate per-row "Add to repo..." dropdown in the
   corpus table, away from where the architect actually engages the rule.

Underneath all three sat a latent data hazard: a UI-internal sentinel
(`\u{0}__single_repo__`) used to key a single-repo scan's selection could leak
into a persisted `repos` list and reach the emit path, where `git clone` failed
with `nul byte found in provided data`.

## Decision

### 1. A rule's scope dictates its bucket, and its bucket dictates emission

The corpus rule carries a `scope` field, set by the corpus author. It is
inherent to the rule, not a per-project toggle. `bucket_of`
(`crates/ui-core/src/rules.rs`) maps it:

| Corpus `scope` | Bucket (`SelectionBucket`) | Level | Emitted into repo files? |
|----------------|----------------------------|-------|--------------------------|
| `"cross-repo"` | `CrossRepo`                | project-level | No — gates read it from the project store |
| `"process"`    | `Process`                  | project-level | No — gates read it from the project store |
| anything else  | `Selections`               | repo-local | Yes — emitted into each chosen repo's files, scoped by the selection's `repos` |

Project-level rules (cross-repo / process) apply project-wide. They are never
written into an individual repo's governance files; the gates read them from the
project. Repo-local rules are emitted into exactly the repos named in their
selection's `repos` list.

### 2. Adoption is scope-dependent

- Picking an option on a **project-level** rule adopts it: the target is
  unambiguous (project-wide), so the option pick alone selects the rule and it
  appears in the Applied table.
- Picking an option on a **repo-local** rule does **not** auto-add it. Which repo
  would the option target? Repos are chosen separately (see the picker below).

Project-level adoption scopes the new selection to the project's real repos
(`project.repos`), because a downstream garbage-collection step drops selections
whose `repos` is empty. If the project has no repos, the adopt is skipped rather
than persisting a selection that would immediately be dropped. The adopt-vs-skip
decision is the pure helper `project_level_insert(bucket, project_repos)`.

### 3. One shared transform behind both persist paths

The two divergent persist paths are unified through a single pure transform,
`apply_chosen_option(project, rule_id, option, corpus)`. Both the immediate click
handler and the `chosen_ctx` watcher call it. It updates the option in place when
the rule is present and adopts a not-yet-selected project-level rule into the
right bucket; repo-local rules are never auto-adopted; an idempotent re-pick
reports no change. Integration tests guard the round-trip (server persistence,
the adopt transform, and the Applied-row / search including process rules).

### 4. Repo scoping lives in the rule detail modal

The rule detail modal now shows an "Applies to repos" section:

- **Repo-local rule** (`bucket_of == Selections`): one checkbox per project repo.
  Checking a repo adds it to the rule's selection (creating the selection with
  the chosen or corpus-default option if absent); unchecking removes it, and
  removing the last repo drops the whole selection (repos-empty GC =
  unselecting). Toggles are idempotent. The transform is the pure
  `apply_repo_scope`.
- **Project-level rule** (cross-repo / process): a static "Applies project-wide
  (all repos)" line, no checkboxes. These are never scoped to specific repos.

The older per-row "Add to repo..." dropdown in the corpus table still works; the
in-modal picker is additive. Emit scopes to exactly the chosen repos.

### 5. The single-repo sentinel never persists and never emits

`\u{0}__single_repo__` is a UI-internal HashMap key (deliberately NUL-prefixed so
it can never collide with a real `owner/repo`). It must never reach a persisted
`repos` list or a `git clone`. It is contained at two boundaries:

- **UI save boundary:** `resolve_selection_repos(repos, all_repos)` translates
  the sentinel to the project's single real repo (dropping it when there are no
  repos) as selections leave the internal map. `build_ruleset_json` and the
  arm/emit request builder both run every bucket's repos through it. The internal
  selection map stays keyed by the sentinel (preserving single-repo pre-check
  behavior); only the way out is translated.
- **Server side:** `normalize_repos(repos, project_repos)` replaces any
  NUL-containing entry with the project's real repos (deduped, order-preserving).
  `resolve_project_arm_rules`, `onboard_arm`, and `onboard_apply` all route repos
  through it before the clone loop, and `clone_or_pull` / `apply_local` reject a
  NUL-byte repo with a clear error rather than shelling out to git.

This resolved the `git clone failed: nul byte found in provided data` emit error
and unblocks any project that had already persisted the sentinel.

## Consequences

- Scope is a corpus-authored property, not a per-project setting. Adding a rule to
  the corpus with `scope: "process"` or `scope: "cross-repo"` makes it
  project-level everywhere automatically; anything else is repo-local.
- Project-level rules are guaranteed never to write into repo governance files;
  consumers reading `AGENTS.md` / `CONVENTIONS.md` in a repo will not find them
  there, by design. The gate reads them from the project store.
- Option picks are now a reliable, single-transform selection action, with the
  Applied table reflecting adopted project-level rules deterministically.
- The architect scopes repo-local rules where they engage the rule (the modal),
  with the corpus-table dropdown retained as a second path.
- The sentinel is contained by two independent boundaries (UI and server), so a
  leak on either side is caught; the belt-and-suspenders git-level rejection
  means a future leak fails loudly and legibly rather than as an opaque nul-byte
  error.
