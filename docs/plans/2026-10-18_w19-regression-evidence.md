# W19 — the measured regression inventory, and the gate design that follows from it

Status: evidence established 2026-10-18. Supersedes the counts in section 2A of
`2026-10-16_comprehensive-remediation-design.md`, which were taken from grader prose rather
than from a direct export diff.

## 1. Method

Both exports are of the SAME fixture at the SAME fixture commit (hearth `a2b3859`), so every
difference is attributable to the engine:

| | export | engine | findings |
|---|---|---|---|
| baseline | `_runs/sweep4-verify-2026-10-08-1948/hearth/hearth.zip` | `96ed204` | 122 |
| current | `_runs/sweep5b-2026-10-16/hearth.zip` | `50d03a7` | 160 |

Note the direction: the regressed engine emits **38 MORE rows**. Volume went up while recall
went down, which is exactly why no count-based check caught it.

## 2. The finding that reframes everything: semantic rule ids are not stable

21 rule ids fired in the baseline and are absent now. **Nine of those 21 are not regressions.**
The semantic tier invents its rule id per run, so the same defect reappears under a new name.
Four baseline findings sit at an identical `(path, line)` in both exports under different ids:

| path:line | baseline rule id | current rule id |
|---|---|---|
| `app/api/admin/users/route.ts:4` | `AI-UNAUTHENTICATED-PII-ADMIN-ENDPOINT` | `AI-UNAUTHED-ADMIN-DATA-EXPOSURE` |
| `app/api/bookings/route.ts:22` | `AI-CLIENT-CONTROLLED-PRICE` | `AI-CLIENT-CONTROLLED-PAYMENT-AMOUNT` |
| `app/api/messages/route.ts:5` | `AI-OVERLY-PERMISSIVE-MESSAGES-SELECT-POLICY` | `AI-UNAUTHED-MESSAGE-READ` |
| `components/SearchHighlight.tsx:7` | `AI-XSS-SEARCH-HIGHLIGHT` | `AI-XSS-VIA-DANGEROUSLYSETINNERHTML` |

**Consequence for the gate: it may never key on `rule_id` for the semantic tier.** A
rule-id-keyed gate would have reported nine false regressions on this very pair of runs, and a
gate that cries wolf gets disabled — the reasoning already written into
`integrity_gate.rs:226` about why `Architectural` is not a required tier applies verbatim here.

Measuring the semantic tier by LOCATION instead, of 10 baseline semantic findings:

- 5 still have an accusing row at the exact line
- 4 still have an accusing row somewhere in the same file
- **1 is genuinely lost**: `AI-IDOR-RECEIPT-ACCESS`, CRITICAL, `app/api/receipts/[id]/route.ts:14`
  — no accusing row anywhere in that file now.

So semantic recall lost ONE finding, not nine. It happens to be a critical IDOR, so it still
matters, but the tier is not broken and must not be treated as broken.

## 3. CORRECTED — the table below is WRONG. See §3A.

**Retracted same day.** The table in this section was built by diffing two exports, which is
one run against one run. A third export existed and I had not looked at it. Reading it reduces
the real regression from 12 rules to 6 and changes what W19 is. The original table is kept
below strikethrough-style for the record; §3A supersedes it.

## 3. The real regression: 12 deterministic rules  *(SUPERSEDED — see §3A)*

Stable ids, genuinely silent at `50d03a7`:

| rule_id | sev | rows | caught |
|---|---|---|---|
| `SEC-NO-SECRET-FILE-1` | critical | 1 | committed `.env.production` |
| `SUPABASE-AUTH-ANON-SIGNIN-1` | critical | 3 | anonymous sign-ins vs vetted-user policies |
| `SUPABASE-AUTH-EDGE-JWT-1` | critical | 2 | `verify_jwt=false` on a scheduled function |
| `ARCH-EXACT-DECIMALS-1` | high | 1 | `payments.amount_cents` as float8 |
| `ARCH-TOKEN-REFRESH-ROTATION-1` | high | 1 | 30-day JWT expiry |
| `JAVASCRIPT-PUBLIC-ENV-PREFIX-SECRET-1` | high | 1 | Stripe secret under `NEXT_PUBLIC_` |
| `SUPABASE-AUTH-REDIRECT-ALLOWLIST-1` | high | 1 | wildcard `https://*.vercel.app` |
| `SUPABASE-AUTH-PASSWORD-POLICY-1` | medium | 1 | no `password_requirements` |
| `ARCH-NO-SECRETS-IN-URL-1` | low | 3 | secret in query string |
| `ARCH-OBSERVABILITY-SILENT-FAILURE-1` | low | 13 | discarded query errors |
| `JAVASCRIPT-TESTING-NO-DISABLED-TESTS-1` | low | 2 | no ESLint config for jest rules |
| `ARCH-MONOLITH-FIRST-1` | low | 1 | structural observation |

## 3A. CORRECTED inventory — 6 reproducible misses, not 12

### The third run

`_runs/sweep5-2026-10-16/hearth.zip` was produced by the SAME engine `50d03a7` as the current
export, on the SAME fixture commit `a2b3859`, **3h28m earlier**. Every commit after `50d03a7`
is docs-only. So it is a true engine-fixed, fixture-fixed replicate, and §3's two-run diff was
measuring a single sample as though it were a trend.

Re-measuring all 12 across three runs, and counting a rule as present if it appears directly
OR is absorbed into a merge carrier (`also_matches` / `also_locations`):

| rule | base `96ed204` | mid `50d03a7` | curr `50d03a7` | verdict |
|---|---|---|---|---|
| `ARCH-EXACT-DECIMALS-1` | 1 | 0 | 0 | **reproducible miss** |
| `ARCH-TOKEN-REFRESH-ROTATION-1` | 1 | 0 | 0 | **reproducible miss** |
| `JAVASCRIPT-PUBLIC-ENV-PREFIX-SECRET-1` | 1 | 0 | 0 | **reproducible miss** |
| `ARCH-NO-SECRETS-IN-URL-1` | 3 | 0 | 0 | **reproducible miss** |
| `JAVASCRIPT-TESTING-NO-DISABLED-TESTS-1` | 2 | 0 | 0 | **reproducible miss** |
| `ARCH-MONOLITH-FIRST-1` | 1 | 0 | 0 | **reproducible miss** |
| `SUPABASE-AUTH-ANON-SIGNIN-1` | 3 | 1 | 0 | run variance |
| `SUPABASE-AUTH-EDGE-JWT-1` | 2 | 1+2 | 0 | run variance |
| `SUPABASE-AUTH-REDIRECT-ALLOWLIST-1` | 1 | 1 | 0 | run variance |
| `SUPABASE-AUTH-PASSWORD-POLICY-1` | 1 | 0+1 | 0 | run variance |
| `SEC-NO-SECRET-FILE-1` | 1 | 1+1 | 0+2 | not lost, absorbed |
| `ARCH-OBSERVABILITY-SILENT-FAILURE-1` | 13 | 0 | 0+1 | absorbed, but see note |

**6 reproducible misses. 4 run variance. 2 absorbed.**

Three of the six are HIGH and are the ones that matter: money stored as float, a 30-day token
lifetime, and a Stripe secret under a client-exposing prefix.

Note on `ARCH-OBSERVABILITY-SILENT-FAILURE-1`: "absorbed" describes its PRESENCE, not its
coverage. It fell from 13 rows to a single absorbed location. Whether 12 sites merged into one
carrier or 12 sites vanished is not answerable from the export alone and needs the stage
ledger. Do not record it as healthy on the strength of the word "absorbed".

### The config-block iterator does not exist

Section 2A of the remediation design says *"a config-block iterator that previously skipped the
first block now reports nothing on any block."* There is no such iterator. The only code that
reads `supabase/config.toml` is `parse_exposed_schemas`
(`crates/checks/src/supabase/config.rs:22`), a single keyed lookup with no loop and no index
arithmetic. The sentence is grader prose describing an LLM enumerating repeated TOML tables
with varying recall across runs. It was never a loop, and no revert can fix it.

### The dominant finding: the engine ate its own output

Digest size across the three runs, from each export's own `provenance`:

| run | files scanned | files excluded | code_chars |
|---|---|---|---|
| base `96ed204` | 87 | 0 | 98,522 |
| mid `50d03a7` | 88 | 0 | 103,307 |
| curr `50d03a7` | 88 | **0** | **277,543** |

**+174,236 chars in ZERO additional files.** The growth is `.camerata-scan-cache.json`, which
the engine writes into the scanned repo root (`crates/cli/src/inspect_cmd.rs:189`) and then
collects because the walker runs `hidden(false)` and the file is in no noise list
(`files_excluded = 0` proves nothing filtered it). It is 212,195 bytes on disk now. It grew
between runs because the mid run wrote a 105-finding cache where the earlier smoke run had
written a 19-finding one.

So roughly **63% of the single audit digest in the final run was the engine's own prior JSON.**
Proof it was inside the model's window rather than merely on disk: the curr export emits
`SEC-NO-DISABLED-TLS-1`, CRITICAL, located at `.camerata-scan-cache.json:1778`. Verified: mid
has 0 findings located in the cache, curr has 1.

Cross-repo control, which is what elevates this above coincidence: lumen and tutorly are
byte-identical between the two engines (54 files / 51,423 chars and 45 files / 43,524 chars in
both), carry no cache row, and lost one class each. Hearth is the only repo scanned twice on
Oct 16, therefore the only one carrying a fat cache, and the only one with a large regression
cluster.

Causation cannot be proven the way a code path can. But it is the only measurable input
difference, it tracks the damage across repos, and it is independently a defect already
scheduled as W20.

### What this does to W19 and W20

1. **W20 moves ahead of W19.** It is a one-line noise-list fix that removes 63% of the digest
   noise and a bogus CRITICAL at queue position #1. Measuring a regression while the engine is
   eating its own output measures the contamination, not the engine. W19 cannot be measured
   honestly until W20 lands.
2. **W19 shrinks to 6 rules** and stops being "restore the regressed classes". Three of the six
   are HIGH and worth real work; three are LOW.
3. **A naive revert of the `kind`/`violates` work would be actively harmful.** It would put the
   four defect-kind auth rules back into the alternatives pass, where
   `apply_alternative_decision`'s `!option.violates` branch rewrites the project's effective
   directive to the rule's own degenerate option — the defect `2f23ab3` fixed. The mid run
   proves that machinery is working: every auth row there carries `kind: defect`.
4. **`SUPABASE-AUTH-PASSWORD-POLICY-1`** is the one auth rule that misses in both `50d03a7`
   runs and deserves its own investigation rather than a family story that does not hold for
   the other three.

### The gate design in §6 is no longer sufficient

§6.3 and §6.4 survive. **§6.5 does not.** A gate that fails when a class fired at >= medium on
a prior export and is zero now would fire on **4 of these 12 right now**, for rules that are
provably alive on the same engine. Two runs of one engine on one fixture differ by whole rule
families.

That is the real headline: **this is a reliability problem wearing a regression costume.** The
revision needed:

- **Deterministic / ExternalTool / Architectural tiers** — ids are stable AND behaviour is
  reproducible. Single-baseline floor is sound here. This is where the 6 real misses live.
- **Semantic tier** — neither id-stable (§2) nor run-stable (§3A). A single-baseline floor is
  invalid. Either exclude it from the failing gate and report it advisory-only, or require
  N-run agreement (best-of or median over k runs) before calling anything a regression.

Deciding between advisory-only and N-run needs a cost conversation: N-run multiplies sweep cost
by N. Advisory-only is free and catches less. Open for Zach.

## 4. The second failure mode: inversion, not silence

Separately, ~25 rules NEWLY appear at `severity=info`, `kind=convention_observed`,
`bucket=informational`, with the headline template:

> "The codebase consistently follows \"&lt;TEXT&gt;\" for &lt;RULE-ID&gt; — zero violations were observed"

`CICD-NO-SECRET-LOGGING-1` is the proof this is a regression and not new coverage: it was a
HIGH do-now finding on `96ed204` (a CI step printing the environment with secrets in scope) and
is a "zero violations" convention row on `50d03a7`, same repo, same commit.

**And `<TEXT>` is the rule's instruction to the scanner**, shipped to the client as a
description of their own engineering convention:

- `SUPABASE-AUTH-GETSESSION-SERVER-1` → "The codebase consistently follows *\"Flag getSession()
  calls in server-execution-context files; ambiguous paths get needs_review\"*"
- `SUPABASE-AUTH-EMAIL-CONFIRMATION-1` → "The codebase consistently follows *\"Flag
  enable_confirmations = false in a config shown to be pushed to a remote project; name the
  local-dev carve-out explicitly\"*"

This is in the shipped PDF.

## 5. Why the existing integrity gate passed on all five repos

Not a bug in the gate; a category it cannot reach. `integrity_gate::evaluate` is documented
pure over a single `ScanLedger` (`integrity_gate.rs:312-315`), and its five families all ask
*did the machinery run*. All 12 regressed rules **ran**. They ran and emitted nothing, or ran
and emitted an inverted row. Nothing in a single run's ledger distinguishes "ran and correctly
found nothing" from "ran and wrongly found nothing".

A regression gate is therefore necessarily CROSS-RUN. It cannot be a sixth predicate over the
same input; it needs a baseline as an additional input.

## 6. Gate design

### 6.1 Keep `evaluate` pure

The baseline arrives as DATA on `IntegrityGateOptions`, not as a file read inside `evaluate`.
The purity contract at `integrity_gate.rs:313` ("Given the same ledger it returns the same
verdict, so the gate itself can never be the flaky thing that blocks a run") is load-bearing
and must survive. Loading the manifest is the caller's job.

### 6.2 Key per tier, because one key does not fit

| tier | key | why |
|---|---|---|
| Deterministic, ExternalTool, Architectural | `rule_id` | stable, authored, in the corpus |
| Semantic | `path` | §2 proves `rule_id` is LLM-generated per run; line numbers drift, file does not |

### 6.3 The floor is an ACCUSING row, not any row

A class is "still detected" only if it produces a row whose `kind` is `defect` or `deviation`.
A `convention_observed`, `no_convention` or `not_assessed` row does NOT discharge the floor.

**This single rule is what makes the gate catch §4.** Had the gate counted any row,
`CICD-NO-SECRET-LOGGING-1` would have passed it — the rule is present in the export, at
position and with text, just inverted. Counting rows would have certified the regression.

### 6.4 Severity: fail at medium and above, report below

`>= medium` regressions are failures (`class_regression`). `< medium` regressions are recorded
in an advisory list that is rendered but does not fail, matching the existing `justified_skips`
idiom of "reported, never a failure" (`integrity_gate.rs:276`).

Rationale: four of the twelve are LOW, and one of those lost 13 rows, so dropping them entirely
would discard real signal. But low-severity volume legitimately moves between runs, and a gate
that fails on it would be disabled within a month. Advisory keeps the signal and the gate.

### 6.5 The baseline is a high-water mark, not the last export

Per Zach's Q3 ruling — *"the gate asserts no class regresses from whichever prior export last
detected it"* — the manifest records, per `(repo, tier, key)`, the most recent export that
detected it and the max severity it reached. A class detected three runs ago but not in the
immediately preceding run is still floored. A single-export baseline would let a regression
become permanent simply by surviving one more run, which is how this one lasted eight days.

`96ed204` is the seed, used as a floor ONLY for classes it actually detected (it had a dark
semantic tier and a broken eslint path, so it is not a valid ceiling).

## 7. Open — needs deciding during implementation

- Where the manifest lives and whether it is committed. It is test data that must survive a
  clean checkout, which argues for in-repo; it is also machine-written, which argues against.
  Leaning in-repo under `crates/server/tests/fixtures/`, written by an explicit
  `--update-baseline` invocation so it is never silently rewritten by a normal run.
- Whether `ARCH-MONOLITH-FIRST-1` belongs in the floor at all. Its baseline row reads as a
  debatable structural observation, and §3's table should not be assumed to be 12 rules all of
  which SHOULD fire. Any rule whose baseline behaviour was itself wrong must be excluded with
  the reason recorded, not restored.
