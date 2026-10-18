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

## 3. The real regression: 12 deterministic rules

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
