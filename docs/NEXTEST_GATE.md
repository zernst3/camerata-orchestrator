# The nextest gate

Issue #340: the same tests, run faster, mostly on GitHub's runners. Nothing is skipped, retried
or filtered out. Only the runner and the scheduling change.

## What changed

- **Runner.** Tests run under [cargo-nextest](https://nexte.st), one process per test, configured
  in `.config/nextest.toml`. Both profiles (`default`, `ci`) use `retries = 0` and
  `fail-fast = false`, and flag a test as slow every 60 s without ever killing it. The `ci`
  profile also writes JUnit to `target/nextest/ci/junit.xml`.
- **Doctests.** nextest does not run them, so every gate also runs `cargo test --doc`.
- **CI.** `.github/workflows/ci.yml` runs each gate as its own job, in parallel. One `build` job
  compiles the test binaries once (`cargo nextest archive`), and four `test` jobs each run a
  quarter of them from that archive (`--partition count:i/4`). The `govern` job ("Camerata
  governs its own code") is green only when every blocking gate is, so a check required under
  that name means what it meant before. It also prints the 30 slowest tests into the job summary.
- **Timings.** `scripts/nextest-slowest.sh` ranks the tests in one or more JUnit files. Download
  the `junit-partition-*` artifacts of a run and point it at the folder:
  `scripts/nextest-slowest.sh -n 30 path/to/artifacts`.

## Swapping it into the local gate

`scripts/gate-nextest-local.sh` is the local equivalent of the old `cargo test` gate:

1. `cargo fmt --all -- --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo nextest run --workspace --locked -j 3`
4. `cargo test --doc --workspace --locked`

To swap it in, install nextest once (`cargo install cargo-nextest --locked`, or the release
binary) and, in the private pre-push gate, replace the `cargo test --workspace ...` line, and
the fmt and clippy lines if they are there, with a call to `scripts/gate-nextest-local.sh`. Keep
the other steps of that gate (the personal path check, the self-measurement suite, the visual
baselines) as they are: the script does not run them.

`-j 3` (three tests at once) leaves cores for builds on the shared machine. Set
`CAMERATA_NEXTEST_JOBS` to change it, for example `CAMERATA_NEXTEST_JOBS=8` when nothing else
is running. To go back, restore the `cargo test` line; nothing else depends on nextest.

## Measured (Linux, 4 cores, same commit, binaries already built)

| | `cargo test --workspace` | nextest |
|---|---|---|
| test binaries | 190 (+ 30 doctest targets) | 190 |
| tests | 14,300 (14,281 run, 19 ignored) | 14,300 (14,281 run, 19 skipped) |
| doctests | 11 (10 run, 1 ignored) | run separately with `cargo test --doc` |
| wall time | 3,102 s | 1,646 s before the priority override |

The test names match one for one. The suite's floor is one test,
`ai_audit::runner_adoption_tests::a_resumed_30k_site_calibration_resubmits_nothing_already_absorbed`,
which takes about 21 minutes on its own; `.config/nextest.toml` starts it first. Making that
test faster is the largest remaining gain, for both runners.
