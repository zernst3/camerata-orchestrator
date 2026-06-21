# corpus-verifier

A **MAINTAINER-ONLY** repo-governance tool that promotes Camerata corpus rules
from `grounded` to `verified`.

## What it is

`verified` is the strongest assertion the corpus can make about a rule: a human
maintainer has confirmed the rule's grounding (its cited source / linter) is
accurate. The verification ladder lives in `camerata-rules`:

```
draft  →  grounded  →  verified  →  needs_recheck (drifted)
```

`corpus-verifier` is the **single writer** of `verified`. It:

1. locates the rule's `.toml` under `crates/rules/principles/`,
2. edits it in place — sets `verification = "verified"` and writes a `[verified]`
   table (`by` / `at` / `against`), preserving every other field, comment, and
   the existing formatting (via `toml_edit`),
3. commits the edit on a `verify/<rule-id>` branch and opens a PR into `main`.

Because the only path to `verified` is a **branch + PR into main**, every
`verified` status traces back to a reviewed commit in the source of truth.

## Why it is repo tooling, NOT part of the product

The shipped Camerata app (`camerata-ui`, `camerata-server`, ...) is **read-only**
on verification: it reads the `verified` status to decide what to ship/demo, but
it never writes it. Verification is a maintainer governance act, not a runtime
product feature, so it lives in a separate tool that:

- is **excluded from the app deploy**, and
- **must never be a dependency of any app crate.**

It is allowed to depend on `camerata-rules` only — to *read* and *identify* the
corpus (and to round-trip-validate its own edits).

See `docs/decisions/2026-06-20_corpus_verifier_tool.md` for the full rationale.

## Layout

```
tools/corpus-verifier/
├── src/
│   ├── lib.rs        # CORE: locate_rule, apply_verification, list_grounded,
│   │                 #       self_source_targets, the VcsOps seam, verify flows
│   ├── main.rs       # CLI  (binary: corpus-verifier)
│   └── bin/gui.rs    # GUI  (binary: corpus-verifier-gui — thin Dioxus 0.7 desktop)
└── README.md
```

The CLI and GUI are two thin surfaces over the same CORE in `lib.rs`.

## The VcsOps seam

All git/PR operations go through the `VcsOps` trait — the flow code never shells
out directly. Two implementations:

- `GitVcs` — the real path: shells out to `git` (branch / commit / push) and
  `gh pr create --base main`.
- `DryRunVcs` — records the planned operations instead of running them. Used by
  every test and by `--dry-run`, so the verify path is fully exercisable with **no
  git and no network**.

`--dry-run` still edits the TOML locally (so you can inspect the diff) but records
the git/PR plan rather than executing it.

## Usage

```bash
# List the risk-ordered grounded queue (mechanical first, then known languages).
cargo run -p corpus-verifier --bin corpus-verifier -- list

# Verify ONE rule. --against is prefilled from the rule's [[sources]] if omitted.
cargo run -p corpus-verifier --bin corpus-verifier -- \
    verify RUST-NO-UNWRAP-1 --by zach --against "clippy 1.83"

# Dry-run first (no branch / push / PR; edits TOML locally to inspect the diff):
cargo run -p corpus-verifier --bin corpus-verifier -- \
    verify RUST-NO-UNWRAP-1 --by zach --dry-run

# Bulk self-source the maintainer-authored meta rules into ONE branch + ONE PR.
# (meta domains: agentic, api-layer, ui, permissions, universal)
cargo run -p corpus-verifier --bin corpus-verifier -- \
    self-source --all-meta --by zach
cargo run -p corpus-verifier --bin corpus-verifier -- \
    self-source --domain agentic --by zach --dry-run

# The thin desktop GUI (lists the queue, prefilled verify form, bulk view).
cargo run -p corpus-verifier --bin corpus-verifier-gui
```

`self-source` marks meta rules verified with `against = ["self-sourced: <domain>"]`
because the maintainer IS the authority for the corpora they authored.

## Tests

```bash
cargo test -p corpus-verifier
```

The CORE is unit-tested with temp-dir fixtures: `apply_verification` round-trips
through `camerata-rules` (the edited rule reads back as `Verified` with its
provenance), is idempotent, replaces a stale `against`, and preserves comments.
The verify and self-source flows run through `DryRunVcs` so no real git/network is
touched.
