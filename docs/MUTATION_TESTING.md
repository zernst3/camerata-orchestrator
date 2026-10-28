# Mutation testing

A test suite's size says little about whether it would notice a bug. Mutation testing measures
that directly: `cargo-mutants` makes one small deliberate change to the production code at a time
(flip a comparison, delete a match arm, return a constant), runs the tests and records whether
any test failed. A mutant no test notices is a change the suite would let through.

This page records the 2026-10-28 run over the code that decides what reaches a client: the rule
corpus loader and its build gates (`camerata-rules`), the secret detector and redactor
(`camerata-secrets`) and the server's report export, workbook export, release gate and
integrity gate. `camerata-checks` was covered earlier (97.9%, see
`docs/plans/2026-10-19_MASTER-SEQUENCE.md`).

## How it was run

- `cargo-mutants` 27.1.0, `--jobs 2`, no model calls.
- Each mutant runs its own crate's tests. For `camerata-rules` and `camerata-secrets` that is the
  whole crate (unit and integration tests).
- For `camerata-server` the run was restricted with `--file` to `report_export.rs`,
  `xlsx_export.rs`, `release_gate.rs` with `release_gate/*.rs` and `integrity_gate.rs`. Each
  mutant ran the server's library tests whose names contain `export`, `release_gate` or
  `integrity_gate` (`-C --lib -- -- export release_gate integrity_gate`), about 430 tests. The
  server's 53 integration test binaries were not run per mutant (see "Not covered").
- Per-mutant timeout: 300 s for rules and server, 30 s for secrets (its tests take under a
  second, so a 30 s run is an infinite loop).
- Catch rate is caught / (caught + missed). A timeout is a mutant that hung the tests (an infinite
  loop the tests would have noticed); timeouts are listed separately and left out of the rate.
  An unviable mutant does not compile and says nothing about the tests.

## Results

| Scope | | Mutants | Caught | Missed | Timeout | Unviable | Catch rate |
|---|---|---:|---:|---:|---:|---:|---:|
| `camerata-rules` | before | 558 | 467 | 64 | 0 | 27 | 87.9% |
| | after | 559 | 522 | 9 | 1 | 27 | 98.3% |
| `camerata-secrets` | before | 515 | 356 | 115 | 6 | 38 | 75.6% |
| | after | 521 | 454 | 14 | 15 | 38 | 97.0% |
| server `report_export.rs` | before | 608 | 397 | 192 | 0 | 19 | 67.4% |
| | after | 608 | 578 | 11 | 0 | 19 | 98.1% |
| server release gate | before | 191 | 128 | 39 | 0 | 24 | 76.6% |
| | after | 191 | 165 | 2 | 0 | 24 | 98.8% |
| server `xlsx_export.rs` | before | 353 | 151 | 189 | 1 | 12 | 44.4% |
| | after | 353 | 321 | 18 | 2 | 12 | 94.7% |
| server `integrity_gate.rs` | before | 42 | 31 | 9 | 0 | 2 | 77.5% |
| | after | 42 | 40 | 0 | 0 | 2 | 100% |
| **All** | before | 2,267 | 1,530 | 608 | 7 | 122 | 71.6% |
| | after | 2,274 | 2,080 | 54 | 18 | 122 | 97.5% |

The rules and secrets "after" rows are full reruns on the final code (the mutant totals moved
because the code under test changed: `corpus_path` was split for testing, and the Firebase fix
added a function). The server "after" rows are the "before" run's caught mutants plus every
survivor rerun with `--iterate` against the final tests. "Release gate" is `release_gate.rs` and
`release_gate/*.rs`.

**Tests added.** 143 in all: 28 in `camerata-rules` (`crates/rules/tests/mutation_kills.rs`
and one unit test), 46 in `camerata-secrets` (`detector_edges.rs`, `firebase_spans.rs`,
`twin_boundaries.rs` and two in `json_walk.rs`, under `crates/secrets/src/tests/`) and 69 in
`camerata-server` (a
`mutation_tests` module beside each of the four files). Most of the rules survivors were corpus
build gates (every high-severity rule has a citation and a fix, every security rule has a CWE)
that were only ever run against the clean shipped corpus, so a gate that stopped detecting
anything would still have passed; each now gets a planted offender. One `camerata-server` test is
a row-by-row characterization of the workbook's Index sheet: update it deliberately when the
Index changes.

## A real defect the run found

The secrets crate's Firebase rule keeps a Firebase web `apiKey` readable (Google documents it as
safe to publish) and redacts every other Google API key. A Maps key in its own object followed
within 800 bytes by a Firebase config was classified as the Firebase key: left readable in every
client artifact and dropped from the findings. The bracket scanner started reading forward from
inside the key's own quoted string, so the closing quote was read as an opening one and every
string after it flipped. Fixed in `crates/secrets/src/firebase.rs` (`scan_to`); the failing tests
are in `crates/secrets/src/tests/firebase_spans.rs`. A key the rule no longer recognises stays a
secret, so the fix can only redact more.

## Remaining missed mutants

Every mutant below was checked by hand. All but the workbook's are equivalent: no input changes
what the code produces, so no test can tell them apart from the original. The workbook's change
only colours and sheet view settings, never what a cell says.

### camerata-rules (9)

- `lib.rs` `load_one`, `==` to `!=` and `!=` to `==` on the explicit-domain check: the branch only
  logs a warning; the derived domain wins either way.
- `lib.rs` `severity_rank`, delete the `"high"` arm: the fallback arm returns the same rank (3).
- `lib.rs` `severity_rank`, delete the `"critical"` arm: falls to 3, and every caller only asks
  whether the rank is at least 3.
- `lib.rs` `domain_to_glob`, delete the `"universal"` arm: the fallback returns the same `**`.
- `freshness.rs` `client_rules_last_reviewed`, `<` to `<=`: two equal dates are the same trimmed
  string, so keeping either gives the same answer.
- `freshness.rs` `confirm_in_text`, `+` to `*` in the search start: the header line itself is
  then searched, and it never starts with `<domain> =`.
- `freshness.rs` `confirm_in_text`, `>` to `>=` and `+` to `*` on the blank-line walk-back: the
  walk stops at the header line either way, and the header line is never blank.

### camerata-secrets (14)

- `detect.rs` `twin_context`, `i + 1` to `i * 1`: the line prefix then starts with the newline,
  which every prefix pattern already treats as a boundary.
- `detect.rs` `floor_boundary`, `-=` to `+=`: rounds a window edge forward instead of back by at
  most three bytes; the edge is still a character boundary and windows are approximate by design.
- `detect.rs` `unfooted_body`, `<` to `<=` on the scan loop and `>` to `==` or `>=` on the line
  check: the line check and the body pattern stop the scan at the end of the text either way.
- `detect.rs` `resolve`, `<` to `<=`: no two secret shapes can produce touching value spans
  (every pair of planted values was tried with five separators).
- `firebase.rs` `scan_to`, `<` to `<=`: also reads the value's first byte, a key character with
  no bracket or quote meaning.
- `fixtures.rs` (three): test fixture generators; any deterministic body serves.
- `json.rs` `redact_json_values`, `>` to `>=`: writes back identical text when nothing matched.
- `twins.rs` `byte_class`, delete the digit arm: placeholder words start and end with a letter,
  `_` or `-`, so a digit next to one is a different class either way.
- `twins.rs` `is_closed`, `>` to `>=`: an opener never ends with its own closing character.
- `twins.rs` `is_context_shaped` to `true`: no vendor-shaped value can be a template or a
  publishable key, the only two checks it gates.

### camerata-server, `report_export.rs` (11)

- `classify`, delete the `"FalsePositive"` arm: the arm only adds a debug assertion and falls
  through to the same status-derived disposition.
- `clamp_to_rule_max_severity`, `raise_to_rule_min_severity` and `clamp_deviation_severity`,
  the strict comparison made non-strict: at equality the clamp returns the same severity.
- `is_action_row_missing_an_estimate` (three: returns `false`, `||` to `&&`, delete `!`): its only
  use is a headline branch that cannot be reached, because every row this gate could flag stays
  in an action bucket and the branch needs a held or informational one. The gate is dead code
  since C6-B4 was amended; removing it is a separate decision.
- `build_report_json`, `||` to `&&` in that same unreachable branch, in both places it is built.
- `curated_confidence_rank`, delete the `Some("medium")` arm: the fallback arm has the same value.
- `apply_sites_vs_rows_check`, delete the `line: 0` field: 0 is the default.

### camerata-server, release gate (2)

- `archive.rs` `xlsx_cells`, `tag_end + 1` to `- 1` or `* 1`: the cell body slice then starts one
  or two bytes earlier, inside the cell's own tag, which can never contain `<v>`, `<t` or `</c>`.

### camerata-server, `xlsx_export.rs` (18)

Presentation only, with no effect on any cell's text:

- Row tinting (11): the zebra stripe and the critical-row tint in the findings, deviations and
  conventions sheets (`write_finding_row`, `write_findings_sheet`, `write_deviations_sheet`,
  `write_conventions_sheet`).
- Sheet view settings (7): the Coverage sheet's frozen header row and the extent of the Rules
  applied filter (`write_coverage_sheet`).

### camerata-server, `integrity_gate.rs`

None.

## Not covered

- **Server integration tests.** `crates/server/tests/*.rs` were not run per mutant (each mutant
  would have linked 53 more binaries). A server mutant counted as missed might be caught by one
  of them; every one was triaged and given a library test regardless.
- **PDF compilation with a real Typst.** This environment has no `typst` binary, so the tests
  that compile a real PDF skip themselves. The compile path is covered instead through the
  existing `TYPST_PROGRAM` test seam with a stand-in script
  (`report_export/mutation_tests/built_report.rs`).
- **The rest of `camerata-server`.** Only the four export and gate modules above were in scope.

## Rerunning

```sh
cargo install --locked cargo-mutants
cargo mutants -p camerata-rules --jobs 2 --timeout 300
cargo mutants -p camerata-secrets --jobs 2 --timeout 30
cargo mutants -p camerata-server --file crates/server/src/report_export.rs \
  --jobs 2 --timeout 300 -C --lib -- -- export release_gate integrity_gate
# then the same for xlsx_export.rs, release_gate.rs with 'release_gate/*.rs', integrity_gate.rs
```

`mutants.out*/` is gitignored. After adding tests, `--iterate` reruns only the mutants that were
missed last time.
