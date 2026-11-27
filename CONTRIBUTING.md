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

CI runs the tests under cargo-nextest, split across parallel jobs, plus `cargo test --doc` for
the doctests; `scripts/gate-nextest-local.sh` is the local equivalent (see
[`docs/NEXTEST_GATE.md`](docs/NEXTEST_GATE.md)). Either runner runs the same tests.

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
  its message says which number to change. The same test reads every other markdown file in the
  repository for the headline shape "N rules across M domains", so any doc that quotes the
  headline must quote the real one. The long technical docs the README links are held to the
  same number wherever they state a whole-corpus count of 100 or more; say "every rule" where a
  figure adds nothing. Write the count as plain digits in the shape "N rules across M domains".
- The authoring workflow is in [`docs/RULE_AUTHORING.md`](docs/RULE_AUTHORING.md).

## Changing the crates

The README carries a compact crate diagram and `PLATFORM.md` the full one. Both are generated
from `cargo metadata`, and `crates/rules/tests/docs_crate_graph.rs` fails when a drawn arrow is
not a real dependency, a real dependency is missing from the full graph or the crate count in
either doc is stale. The same test holds the README's "over 500,000 lines" and "10,000+ tests"
to the code, so they can only be floors the code meets. Regenerate the diagram when you add a
crate or a dependency between workspace crates.

## Keeping the build for a long inspection

The build that starts an inspection must finish it, and the rule library is read from the
checkout folder at run time, so a copied binary alone is not a kept build. Before a multi-day
inspection, commit everything, pass the full gate, then run `scripts/keep-scan-build.sh <name>`
(add `--dry-run` first to see the steps). It refuses on a dirty tree, creates the local tag
`scan-build/<name>`, adds a worktree at `camerata-scan-<name>` next to the main checkout, and
builds server, app and CLI there. Start the inspection from that folder and do not pull, edit or
rebuild in it until the inspection is complete. The script never pushes a tag or a branch and
never deletes anything; the tag stays as the record after the folder is removed by hand.

Client inspections start only from that kept folder: the build records whether it has uncommitted
changes, a client inspection will not start on a dirty build, and the build flag is refreshed
whenever a source file under `crates/` changes. The app uses one fixed port, so only one build's
app can be open at a time, and the running inspection owns it: do not open another build's app
until the inspection is complete. If the script fails after creating the tag, it says so and
leaves the tag alone; remove it yourself when you are sure.
