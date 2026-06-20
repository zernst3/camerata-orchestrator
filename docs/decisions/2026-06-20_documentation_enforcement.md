# Documentation Enforcement

**Date:** 2026-06-20
**Issue:** #64
**Branch:** dev1/doc-enforcement

## Summary

Two complementary enforcement layers that together ensure every governed change carries durable, navigable documentation:

- **PROCESS-COMMIT-DOC-1** (mechanical, in `crates/checks/src/vcs_action.rs`): gates commit bodies and PR bodies at the VCS-action chokepoint before Camerata performs the action.
- **PROC-STORY-DOCS-1** (agentic corpus prose rule, in `crates/rules/principles/agentic/proc-story-docs-1.toml`): establishes the project-wide convention for where and how per-story documentation is written.

---

## Why Two Layers?

The commit/PR record and the in-repo story docs serve different purposes and need different enforcement mechanisms:

| Concern | Layer | Mechanism |
|---|---|---|
| Commit body is non-trivial and keyed to a story | Mechanical gate (PROCESS-COMMIT-DOC-1) | Deterministic string check at VCS action time; no LLM; hard refusal |
| Durable per-story documentation exists in the repo | Agentic prose rule (PROC-STORY-DOCS-1) | Agent reads the rule at story start; decides where to place docs; no regex can enforce this |

A mechanical gate alone cannot tell the agent WHERE to put richer documentation. A prose rule alone cannot prevent a bare subject-only commit from landing. The two layers are orthogonal slices of the same enforcement concern.

---

## Layer 1: PROCESS-COMMIT-DOC-1

### What it enforces

Applies to: `VcsTarget::CommitBody` (everything after the first line of a commit message) and `VcsTarget::PrBody` (the PR description).

Both must satisfy the `SubstantiveWithStoryId` matcher, which checks two conditions AND-ed together:

1. **Substantive body**: at least `min_non_blank_chars` non-whitespace characters are present. The default is 20, long enough to rule out a one-word placeholder.
2. **Story-id reference**: the body contains a reference of the form `<prefix><separator><digits>`. The default (prefix `""`, separator `'#'`) accepts a bare GitHub-style `#42` reference.

Both thresholds are configurable at rule construction time so teams using Azure Boards (`AB#42`), Jira (`PROJ-42`), or other trackers can adapt the rule.

### New vocabulary

**`VcsTarget::CommitBody`**: a new target variant added alongside the existing `CommitMessage` and `CommitSubject`. Extracts everything after the first `\n` in the commit message. For a subject-only commit (no newline), returns `""` (not `None`) so body-presence rules fire rather than silently skipping.

**`Matcher::SubstantiveWithStoryId`**: a compound matcher that encodes both the length floor and the story-id pattern as a single predicate. This keeps the rule as one named `ProcessRule` (one violation message, one rule id) rather than two separate rules that could partially fail in confusing combinations.

**`fn is_substantive(text, min)`**: counts non-whitespace chars across all lines; returns true if the total meets the minimum.

**`fn contains_story_id(text, prefix, separator)`**: scans for `prefix + separator + <digit+>`. When prefix is `""`, the token is just the separator character (e.g. `'#'` matching `#42`). Scans past invalid occurrences so a non-digit match does not mask a later valid one.

### Constructor signature

```rust
ProcessRule::commit_documentation(
    min_non_blank_chars: usize,  // default: 20
    story_id_prefix: &str,       // default: "" (bare #42)
    story_id_separator: char,    // default: '#'
) -> ProcessRule
```

### Rustdoc

The constructor is documented with:
- the convention it enforces
- what each parameter controls
- the two targets it applies to
- why branch actions are not gated

### Tests added (all in `crates/checks/src/vcs_action.rs`)

| Test | What it verifies |
|---|---|
| `doc_rule_passes_commit_with_substantive_body_and_story_id` | The happy path: a commit with a full body and `#42` passes |
| `doc_rule_fails_commit_with_subject_only` | Subject-only commit fails on `CommitBody` |
| `doc_rule_fails_commit_body_without_story_id` | Long body without a story ref fails |
| `doc_rule_fails_commit_body_too_short_even_with_story_id` | Body with only `#42` (below 20 chars) fails |
| `doc_rule_passes_pr_with_substantive_body_and_story_id` | PR with body + `#99` passes |
| `doc_rule_fails_pr_with_empty_body` | PR with empty body fails on `PrBody` |
| `doc_rule_branch_action_not_gated` | Branch action is not in scope; no violations |
| `doc_rule_custom_prefix_and_separator_ado_style` | `AB#42` variant; bare `#42` does NOT satisfy this rule |
| `doc_rule_custom_prefix_and_separator_jira_style` | `PROJ-42` Jira variant |
| `is_substantive_counts_non_whitespace_chars` | Unit test for the helper |
| `contains_story_id_bare_hash_reference` | Unit tests for the bare `#` case |
| `contains_story_id_ado_style` | Unit tests for `AB#` case |
| `contains_story_id_jira_style` | Unit tests for `PROJ-` case |
| `extract_commit_body_returns_text_after_first_newline` | New `CommitBody` target extracts correctly |
| `extract_commit_body_returns_empty_string_for_subject_only` | `CommitBody` on a subject-only commit yields `""`, not `None` |

---

## Layer 2: PROC-STORY-DOCS-1

### What it enforces

Decision question: "How does the project capture durable, in-repo documentation for a change (beyond the commit/PR record)?"

Adopted default: **per-story-docs**

Directive: when a governed agent completes a story, emit two files:

```
docs/<story-id>/technical/<story-id>-dev.md   (developer-facing)
docs/<story-id>/user/<story-id>-guide.md      (user-facing)
```

The story id is the key already tracked by the dev console and unit of work, so the agent has a deterministic drop location without additional routing configuration.

### Why the default was chosen

The per-story layout:
- Keys off the story id the agent already knows (no secondary lookup)
- Separates technical rationale (design decisions, routing notes, testable assertions) from user-facing documentation (what the change enables, how to use it, migration steps)
- Produces files the agent can place deterministically without guessing
- Scales linearly: each story is self-contained

### Alternatives considered (and why not adopted)

| Option | Directive | Why not the default |
|---|---|---|
| living-central-docs | Append a dated section to docs/TECHNICAL.md + docs/USER_GUIDE.md per story | Collapses all stories into two growing files; per-story attribution becomes difficult as story count grows |
| adr-per-change | docs/decisions/<date>_<slug>.md per story | Architecture-biased; no dedicated user-facing artifact; conflates technical and user content |
| mechanical-minimum | Rely on commit/PR record + generated CHANGELOG; no separate docs files | Lowest overhead but leaves the repo without searchable narrative; user-facing documentation is absent |

### TOML file

Path: `crates/rules/principles/agentic/proc-story-docs-1.toml`

Fields follow the exact corpus format:
- `id = "PROC-STORY-DOCS-1"`
- `domain = "agentic"`
- `layer = "universal"`
- `enforcement = "prose"`
- `default = true`
- One `[decision]` block with `question`, `default`, `why`
- Four `[[option]]` blocks (per-story-docs, living-central-docs, adr-per-change, mechanical-minimum)

---

## Routed Items (ROUTE-1: not auto-applied)

The following wiring items require structural decisions that go through Zach before implementation:

### 1. How the dev engine honors PROC-STORY-DOCS-1 when running a governed story

The dev engine's story execution loop (currently in `crates/agent/src/` / `crates/server/src/`) would need to:

1. Read the project's selected option for PROC-STORY-DOCS-1 from the codified ruleset.
2. At story completion, invoke a `StoryDocEmitter` (new struct; would live in `crates/agent/`) to place the correct files based on the selected option.
3. For the `per-story-docs` default: create `docs/<story-id>/technical/<story-id>-dev.md` and `docs/<story-id>/user/<story-id>-guide.md` with LLM-generated content (the story summary from the run log + decisions made).

**Routed because**: this requires a new public API on the agent execution path and a new `StoryDocEmitter` abstraction. Both cross module boundaries. ROUTE-1 applies.

**Suggested design sketch for Zach's review**: a `PostStoryHook` trait in `crates/agent/src/hooks.rs` (new file) that the dev engine calls after a story completes; `StoryDocEmitter` implements it. The hook receives the completed `StoryRun` (with story id, decisions log, and run output) and emits files according to the codified option.

### 2. How to register PROCESS-COMMIT-DOC-1 as a selectable process rule in the project onboarding UI

Currently, process rules (`ProcessRule` in `crates/checks/src/vcs_action.rs`) are constructed by callers (e.g. the dev engine's commit path) as concrete instances. There is no registry connecting a human-readable corpus TOML to a `ProcessRule` constructor.

To make PROCESS-COMMIT-DOC-1 selectable in the project's rule-configuration UI (i.e., let architects configure `min_non_blank_chars` and the story-id pattern at onboarding), the server/UI layer would need:

1. A serializable `ProcessRuleConfig` enum (or struct) with one variant per process rule.
2. A `build_process_rules(config: &[ProcessRuleConfig]) -> Vec<ProcessRule>` function.
3. Storage of the chosen config in the project's `settings.json`.

**Routed because**: this requires a new cross-crate public API (a config type the server, persistence, and checks crates all reference) and a structural change to how the server wires process rules into the commit gate. ROUTE-1 applies.

**Suggested scope for Zach's review**: start with `ProcessRuleConfig` in `crates/checks/src/vcs_action.rs` (already accessible to server) to avoid a new crate; the server reads it from settings and calls `build_process_rules`.
