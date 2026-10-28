# Rule stages: where a rule runs (target design, future phase)

**Status:** target design, NOT scheduled. Not part of Camerata V1 (the version that runs the first
client inspection). Build it when a second rule needs stage control, or when the inspection-scope
control below is next touched, whichever comes first.

## The problem

A rule can be useful in one place and harmful in another. `ARCH-DEAD-CODE-1` is the first example:
in an inspection it reports genuinely unused files, but at the Layer-2 write-time gate it would
bounce an agent for a file the story legitimately leaves unwired (scaffolding, an externally
consumed module), and the cheapest way out of the bounce is pasting `keep-file:` markers.

Today "where does this rule run" is decided in two unrelated places:

- **Inspection-only** (2026-10-21): a code-level checker method,
  `ArchChecker::runs_at_write_time() -> bool` (default `true`; dead-code returns `false`), read
  by the Layer-2 `NativeArchCheckRunner`. Invisible in the corpus and the UI.
- **Leave out of inspection**: a separate, pre-existing control.

Neither is visible where a reader looks for rule behavior (the rule's TOML, the Rules view), and
neither is covered by the corpus version lock.

## The target

One field on the rule itself, read by every runner:

```toml
id = "ARCH-DEAD-CODE-1"
version = 2
stages = ["inspection", "ci"]   # omitted = every stage
```

- **Stages** (initial vocabulary): `inspection` (the scan behind the client deliverable), `ci`
  (the repo-installed gate), `write-time` (the Layer-2 gate on agent work). Extendable if Layer
  3/4 gain their own runners.
- **Default** when omitted: every stage, which is today's behavior for every rule except dead-code.
- **Every runner filters by it**: the scan, the CI gate, and the Layer-2 runner each skip rules
  whose `stages` do not include them. One predicate, `Rule::runs_in(stage)`, in `camerata-rules`.
- **Replaces both existing controls**: `runs_at_write_time()` is deleted (dead-code's TOML says
  `stages = ["inspection", "ci"]`), and the "leave out of inspection" control becomes a `stages`
  value that omits `inspection`. Migrate, don't keep both.
- **Covered by the version lock**: `stages` is part of `Rule::canonical_form`, so changing where a
  rule runs requires a version bump like any other rule change.
- **Visible**: the Rules view shows the stages per rule; the inspection report's coverage section
  can say "this rule runs in inspections only" where that matters.
- **Gates**: an unknown stage name fails the corpus gate; a rule with an empty `stages` list
  fails (a rule that runs nowhere is an orphan).

## Per-file override (related, already exists)

A file containing `keep-file: <reason>` is never reported by dead-code (guard G12). If dead-code
ever moves into development-time checking (Layer 4 was suggested), that marker is the escape hatch.

## Not in scope

Per-repo or per-project stage overrides. The corpus default is enough until a real engagement asks
for more.
