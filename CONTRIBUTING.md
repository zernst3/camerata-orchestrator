# Contributing

## Branches

`feat/audit-report-export` is the working branch. Day-to-day work lands there.

`main` is the public default branch. At the end of every work session, `feat/audit-report-export`
is merged to `main` through a pull request, using a normal merge commit (not a squash or a
rebase). That keeps `main` from trailing the working branch by more than a day, so what a visitor
sees on the public repository is never stale.

## Before you open a pull request

The same gates run on every merge:

```
cargo fmt --all -- --check
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --locked -- -D warnings
RUSTFLAGS="-D warnings" cargo test --workspace --locked
```

## Changing the rule corpus

- Editing a rule means bumping its `version` and regenerating the lock with
  `cargo run -p camerata -- corpus lock`. The command refuses to write if a rule changed without
  a bump. Commit `crates/rules/CORPUS_LOCK.toml` together with the rule change.
- The README and `docs/OVERVIEW.md` state the rule and domain counts. A test in
  `crates/rules/tests/readme_rule_count.rs` fails when either doc disagrees with the corpus, and
  its message says which number to change. The same test reads every other markdown file in the
  repository for the headline shape "N rules across M domains", so any doc that quotes the
  headline must quote the real one. Write the count as plain digits (for example
  "437 rules across 22 domains").
- The authoring workflow is in [`docs/RULE_AUTHORING.md`](docs/RULE_AUTHORING.md).
