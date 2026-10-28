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

CI also runs `scripts/check-personal-paths.sh`, which fails when a tracked file names a personal
home directory or the private fixture folder. Write example paths as `/Users/auditor/...`. Run
the script before you push.

## Running the benchmark tests locally

Some tests grade the scanner against private fixture repositories that are not part of this
repository, and one `#[ignore]`d test serves a local Dioxus app. They read the fixtures from the
folder named by the `CAMERATA_FIXTURES_DIR` environment variable:

```
export CAMERATA_FIXTURES_DIR=/path/to/your/fixtures
cargo test -p camerata-server --test benchmark_supabase_fixture
```

- Variable unset: the tests print `skipping: ...` and pass. That is the normal state on CI and
  on a fresh clone.
- Variable set but wrong (the folder is missing or does not hold the expected fixture): the
  tests panic with a message that says what is missing. A mistyped path never passes silently.
  Fix the variable or unset it.

`CAMERATA_BENCHMARK_FIXTURE` points at the benchmark repository itself and wins over
`CAMERATA_FIXTURES_DIR`. `CAMERATA_SWEEP_EXPORTS` names the sweep exports directory for
`release_gate_acceptance`, which is `#[ignore]`d and run by hand with `-- --ignored`.

## Changing the rule corpus

- Editing a rule means bumping its `version` and regenerating the lock with
  `cargo run -p camerata -- corpus lock`. The command refuses to write if a rule changed without
  a bump. Commit `crates/rules/CORPUS_LOCK.toml` together with the rule change.
- The README and `docs/OVERVIEW.md` state the rule and domain counts. A test in
  `crates/rules/tests/readme_rule_count.rs` fails when either doc disagrees with the corpus, and
  its message says which number to change. Write the count as plain digits (for example
  "437 rules across 22 domains").
- The authoring workflow is in [`docs/RULE_AUTHORING.md`](docs/RULE_AUTHORING.md).
