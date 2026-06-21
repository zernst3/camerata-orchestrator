# 2026-06-21: Option why rewrite

## Context

Every rule TOML has a `[decision].why` paragraph that discusses every option's trade-off at the decision level. The individual `[[option]].why` fields were left as content-free placeholders:

- `"A defensible alternative the project considered."`
- `"A defensible position on this decision; with no default, the project must choose deliberately at curation time."`

These placeholders conveyed no option-specific information and forced readers to re-derive the trade-off from the decision-level paragraph.

## What changed

All placeholder `[[option]].why` values across these directories were rewritten with 1-3 sentences each:

| Directory      | Files touched | Options rewritten |
|----------------|---------------|-------------------|
| agentic/       | 17            | 43                |
| universal/     | 6             | 13                |
| api-layer/     | 14            | 39                |
| ui/            | 4             | 12                |
| permissions/   | 4             | 7                 |
| ci-cd/         | 4             | 11                |
| iac/           | 1             | 1                 |
| fullstack/     | 1             | 3                 |
| concurrency/   | 1             | 1                 |
| **Total**      | **52**        | **130**           |

Files already containing substantive option-specific trade-off content were left untouched.

## Rules applied

1. Pulled option-specific reasoning directly from the `[decision].why` paragraph when it covered that option.
2. Derived a faithful trade-off from the option's directive when `[decision].why` did not explicitly address it.
3. No em-dashes; commas, colons, semicolons, and parentheses used instead.
4. No other TOML fields (id, title, directive, domain, enforcement, verification, sources, decision) were modified.
5. `cargo test -p camerata-rules` green throughout (54 unit tests + 1 doc test).
