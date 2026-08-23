# camerata-check

The Layer-3 CI-parity distributable. See
`docs/design/2026-07-26_architectural-executor-feasibility.md` §2.4 for the gap this closes:
the native architectural checkers (`camerata-checks::arch_checker::all_checkers`) run inside
Camerata (the brownfield scan and the Layer-2 governed-dev loop), but the generated client-side
`.github/workflows/camerata-gates.yml` runs in the **client's own** GitHub Actions, where no
Camerata binary exists. `camerata-check` is that missing distributable: a small, standalone
binary a client's CI can invoke directly, with **no** dependency on `camerata-server` — no HTTP,
no database, no GitHub client, none of that is available (or needed) on a bare CI runner.

It runs the exact same checker registry, `RepoView` construction, and pruned file-walk that the
Camerata-hosted scan and Layer-2 gate already use (see `src/lib.rs`'s module doc for exactly
which functions are reused, not duplicated).

## Build

```sh
cargo build --release -p camerata-check
# binary at target/release/camerata-check
```

There is no crates.io / release-binary publishing pipeline yet (release-ops follow-up, out of
scope for this pass) — vendor the built binary into your CI image, or build it as a CI step from
a pinned commit of this repo.

## CLI surface

```
camerata-check [PATH] [--format human|json] [--config PATH] [--rule-id ID]... [--strict]
```

- `PATH` — repo (or worktree) root to scan. Defaults to `.`.
- `--format human|json` — output format. Defaults to `human`.
- `--config PATH` — use this file as `.camerata/architecture.toml` instead of (or in addition
  to — this wins) whatever `PATH/.camerata/architecture.toml` the walk finds. Useful when the
  boundary map lives outside the directory being scanned (e.g. a monorepo subdir CI job).
- `--rule-id ID` — repeatable. Restrict to these corpus rule ids. Omit to run every registered
  checker. A rule id no registered checker answers at all (deterministic or advisory — e.g. one
  of the two Group E rules, `ARCH-STRUCTURED-ERRORS-1` / `ARCH-EXACT-DECIMALS-1`, deliberately
  left AI-review-only, see `docs/design/2026-07-27_ast-extractor-layer.md` §4) is reported back
  in `unmatched_rule_ids` rather than silently producing nothing.
- `--strict` — also fail the build on needs-review-grade findings (a checker whose findings are
  `advisory_coexisting` by the rule's own design, e.g. `ARCH-RESOURCE-LIFECYCLE-1`'s spawn
  facet; or a finding whose message carries the `[needs review` marker, e.g.
  `ARCH-HANDLER-NO-DB-1`'s attribute/name-fallback tiers). Off by default — a CI gate should
  only hard-fail on deterministic, hard verdicts; that split is the whole point of this system.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Clean (or `--strict` is off and only needs-review findings exist). |
| `1` | At least one deterministic violation found (or, with `--strict`, any violation at all). |
| `2` | The run itself failed — e.g. an explicit `--config` path that doesn't exist. |

## JSON schema

```json
{
  "repo": "path/as/given",
  "checkers_run": 8,
  "unmatched_rule_ids": [],
  "violations": [
    {
      "rule_id": "SUPABASE-RLS-ENABLED-1",
      "file": "supabase/migrations/20240301000000_init.sql",
      "line": 5,
      "object": "public.profiles",
      "message": "...",
      "severity": "critical",
      "needs_review": false
    }
  ],
  "deterministic_count": 1,
  "needs_review_count": 0,
  "clean": false
}
```

Field names/types are covered by a dedicated test (`json_schema_is_stable`, `src/lib.rs`) — a
downstream CI step parsing this output can rely on the shape not changing silently.

## D3 (config-aware degradation) parity

No special-casing is needed for this — it falls out of reusing the checkers verbatim. A repo
with no `.camerata/architecture.toml` (or a malformed one) gets the exact "unconfigured, stays
silent" behavior for config-gated checkers (`ImportBoundaryChecker`, `HandlerNoDbChecker`,
`StrictLayeringCallChecker`) that it would get inside Camerata's own scan or Layer-2 gate,
because this binary calls the same `ArchChecker::check` methods over the same `RepoView` shape.

## CI invocation

Drop a step into `.github/workflows/camerata-gates.yml` (or any workflow) after building /
restoring the binary:

```yaml
- name: Native architectural checks (camerata-check)
  run: |
    camerata-check . --format json | tee camerata-check-results.json
    camerata-check . # human-readable output + real exit code for the gate
```

Or, as a `.camerata/checks.toml` manifest entry (so Layer 2 and this CI step stay in the same
single source of truth for any rule `camerata-check` already answers):

```toml
[[check]]
id       = "SUPABASE-RLS-ENABLED-1"
name     = "Supabase RLS (native camerata-check)"
tool     = "camerata-check"
version  = "<pinned release/build>"
command  = "camerata-check . --rule-id SUPABASE-RLS-ENABLED-1"
severity = "critical"
in_loop  = true
```

`crates/server/src/lib.rs`'s `ci_story_body_architectural` (the auto-filed "wire architectural
rules into CI" GitHub issue) now tells teams to check `camerata-check` first before building a
bespoke checker, and shows this same manifest shape.
