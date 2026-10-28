# Mutation testing

Mutation testing changes the code in small ways (a `>` becomes `>=`, a function returns an empty
list) and checks that some test fails. A change no test notices is a "survivor": a place where the
tests would not catch a real bug.

## Runs so far

| Crate | Date | Result | What was done |
|---|---|---|---|
| camerata-checks | earlier | published rate (see README) | the crate the README's mutation figure refers to |
| camerata-rules | 2026-10-28 | 64 survivors | `crates/rules/tests/mutation_kills.rs` kills the real ones |
| camerata-secrets | 2026-10-28 | survivors in `detect.rs` and `firebase.rs` (count not recorded) | `crates/secrets/src/tests/detector_edges.rs` and `firebase_spans.rs` |
| report export | not yet run | | open item in the repo hardening work order |
| integrity gate | not yet run | | open item in the repo hardening work order |

The raw cargo-mutants output (`mutants.out/`) is not kept in the repository, so the camerata-secrets
count and the caught, timeout and unviable totals for both 2026-10-28 runs were not recorded. The
next run should copy those totals into this table.

## camerata-rules

Most of the 64 survivors were corpus build gates (a high or critical rule needs a citation and a
fix, a SEC rule needs a CWE, CWE codes must be well formed, severity bounds are declared). Every
test ran those gates over the clean shipped corpus and asserted an empty offender list, which a
gate replaced by "return nothing" also passes. `mutation_kills.rs` hands each gate a planted
offender beside a twin that must not be flagged and asserts the exact offender list. It also covers
the remaining survivors: `RuleKind` display, `is_defect_kind`, `Filter::And`, role derivation, the
canonical form's control-character escaping, the freshness date math and the watermark editor.

## camerata-secrets

Several `detect.rs` survivors changed only where a PEM match ends (whether the footer is part of
it), never the redacted text, so the redaction tests could not see them. `detector_edges.rs` pins
those spans, the 16-character first-line floor, the redaction-marker rescan, the JWT payload decode
cap and `first_secret`.

The `firebase.rs` survivors led to a real fix: a Google key in its own object followed by a
Firebase config was misread as the public Firebase web key, left readable in client artifacts and
dropped from the findings. It is now reported. `firebase_spans.rs` holds the failing-first cases and
tests for the bracket matcher's comment, string, division and nesting handling.

## How to run

```
cargo install cargo-mutants
cargo mutants -p camerata-rules
cargo mutants -p camerata-secrets
```

Survivors judged equivalent (a change that cannot alter behaviour) should be listed here with the
reason, so the next run does not chase them again.
