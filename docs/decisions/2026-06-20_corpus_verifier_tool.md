# 2026-06-20 — `corpus-verifier`: a maintainer-only repo-governance tool

## Status

Accepted.

## Context

The corpus carries a verification ladder (`camerata-rules::Verification`):

```
draft  →  grounded  →  verified  →  needs_recheck
```

`verified` is the strongest assertion the corpus can make about a rule: a human
maintainer has confirmed that the rule's cited source / linter actually says what
the rule claims. By design, **no automated process may ever emit `verified`** —
it is a human governance act.

`docs/QA/VERIFICATION_QUEUE.md` already defines the risk-ordered list a human
walks to promote rules `grounded → verified`. Until now, doing so meant hand-
editing TOML files and committing them ad hoc. We want a repeatable tool that
makes the promotion auditable and hard to do wrong.

The key tension: **who is allowed to write `verified`, and how does it stay
trustworthy?** The shipped app reads `verified` to decide what to ship/demo. If
the app (or any automated runtime path) could write it, the assertion would no
longer mean "a human reviewed this."

## Decision

### 1. Verification is a repo-governance act, not a product feature.

We build a separate tool, `tools/corpus-verifier`, that is the **single writer**
of `verified`. It is **NOT part of the shipped Camerata product**:

- It is excluded from the app deploy (it is a dev/maintainer tool).
- It **must never be a dependency of any app crate** (`camerata-ui`,
  `camerata-server`, `camerata`, ...).
- It may depend on `camerata-rules` only — to read/identify the corpus and to
  round-trip-validate its own edits.

It lives under `tools/`, not `crates/`, to make the repo-vs-app separation
structural rather than conventional.

### 2. The app stays read-only on `verified`.

The cockpit and BFF read the verification status (e.g. to badge or gate rules)
but never set it. This keeps `verified` meaning exactly one thing: a maintainer
confirmed it. Removing the write path from the runtime removes the only way the
assertion could be diluted.

### 3. `verified` is only ever set through a reviewed commit in `main`.

The verify flow is **branch → edit → commit → push → PR into `main`**:

1. `create_branch verify/<rule-id>` (or `verify/self-source-<domain>` for bulk)
   off the current `main`,
2. `apply_verification` edits the rule TOML in place (targeted `toml_edit` change:
   `verification = "verified"` + a `[verified]` table; all other fields, comments,
   and formatting preserved),
3. `commit` with `verify(corpus): mark <id> verified by <by>`,
4. `push`,
5. `gh pr create --base main`.

So every `verified` status traces back to a reviewed commit in the source of
truth. There is no "flip it locally and forget the PR" path in the tool.

### 4. The git/PR layer is behind a `VcsOps` seam.

`VcsOps { create_branch, commit, push, open_pr }` has a real `GitVcs` (shells out
to `git` + `gh`) and a `DryRunVcs` that records the plan instead of executing it.
Tests and `--dry-run` use `DryRunVcs`, so the entire flow is exercisable with no
git and no network. (`--dry-run` still edits the TOML locally so the diff is
inspectable.) During tool development we never run the real path — no live
`verify/*` branch, no real PR.

### 5. Self-sourcing for maintainer-authored meta corpora.

The "meta" domains (`agentic`, `api-layer`, `ui`, `permissions`, `universal`) are
corpora the maintainer designed rather than mirroring an external linter/style
guide. For these, the maintainer IS the authority, so they can be self-sourced:
flipped to `verified` with `against = ["self-sourced: <domain>"]`, batched into a
single branch + single PR (`self-source --all-meta` or `--domain <d>`).

## Consequences

- `verified` is durably tied to a reviewed commit; the corpus's strongest claim
  cannot be set silently or by a runtime path.
- The app deploy never ships this tool, and the lint/dependency boundary keeps it
  out of app crates.
- Tooling for the promotion is repeatable (CLI + thin Dioxus GUI over one CORE)
  and testable offline (DryRun seam).
- Related: `docs/decisions/2026-06-20_verification_mechanics.md` (the ladder and
  staleness/`needs_recheck` mechanics) and `docs/QA/VERIFICATION_QUEUE.md` (the
  risk-ordered queue this tool walks).
