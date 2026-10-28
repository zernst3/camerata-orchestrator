# W19 controlled re-run — pre-registered experiment

Written BEFORE the run. The point of pre-registering is that this week has already produced
two wrong counts from me, both because the interpretation was chosen after seeing the data.
The prediction table in §5 is committed before the scan starts and is not editable afterward;
if the result does not fit it, the result wins.

## 1. The question

Four rules fired on engine `96ed204` and emitted nothing on `50d03a7`, twice:

| rule | sev | baseline rows | what it catches |
|---|---|---|---|
| `ARCH-EXACT-DECIMALS-1` | high | 1 | `payments.amount_cents` stored as float8 |
| `ARCH-TOKEN-REFRESH-ROTATION-1` | high | 1 | JWT expiry 2592000s = 30 days |
| `JAVASCRIPT-PUBLIC-ENV-PREFIX-SECRET-1` | high | 1 | Stripe secret under `NEXT_PUBLIC_` |
| `ARCH-NO-SECRETS-IN-URL-1` | low | 3 | secret in a URL query string |

All four show `ran: true, findings_emitted: 0` in the `50d03a7` ledger. They were armed,
prompted, and the model returned nothing. Nothing downstream dropped them.

**Is this an engine regression, or an artefact of the measurement?**

## 2. The three candidate explanations

- **H1 — cache contamination.** The `50d03a7` run's digest was 277,543 chars, of which
  ~174,236 was `.camerata-scan-cache.json`, the engine's own prior output. Real source fell
  from ~100% of the audit prompt to ~35%.
- **H2 — unrecorded model.** `provenance.audit_model` is `claude-sonnet-5` on the baseline and
  `None` on both `50d03a7` runs. `None` means NOT RECORDED, not "different": the resolution
  chain falls through to `CAMERATA_AUDIT_MODEL` then the client default, and only an explicit
  caller pick is written to provenance. So the model may or may not have differed. It cannot be
  asserted either way, which is itself the problem.
- **H3 — prompt dilution.** Rule routing went from 130 rules to 147 in the same prompt
  (`3116ac7`, corpus reachability). `scan_routing.rs` itself is unchanged between the revisions.

## 3. Design, and its deliberate limitation

One run of hearth at HEAD with the cache removed and the model pinned.

| variable | baseline | broken runs | this run |
|---|---|---|---|
| engine | `96ed204` | `50d03a7` | HEAD (`bced2d3`, includes W20) |
| fixture commit | `a2b3859` | `a2b3859` | `a2b3859` |
| cache in digest | absent | **174,236 chars** | absent (deleted + W20 excludes it) |
| model | `claude-sonnet-5` | unrecorded | **pinned `claude-sonnet-5`** |
| rules routed | 130 | 147 | 147 |

**This controls H1 and H2 together and does not control H3.** That is a choice, not an
oversight. A 2x2 separating cache from model is four runs and roughly $64, and the two branches
below lead to the same next action regardless of which of H1/H2 was responsible, because both
are controlled from here on (W20 shipped; pinning becomes standard). If the rules do NOT
return, separating them becomes worth paying for, and H3 moves to the front.

## 4. Measure from the LEDGER, not the export

**`findings_emitted` in `.camerata-scan-ledger.json`, per rule.** Not the export.

This is the lesson of the day. Today's second wrong count came from reading the export: three
rules that DID detect were absorbed into another rule's row by the merge stage, and
`also_matches` records the absorbed rule's TITLE rather than its id, so an id-keyed search over
the export finds nothing and reports a detection miss that never happened. The ledger records
detection-time truth, before merge.

## 5. Pre-registered prediction table

Primary outcome = how many of the four show `findings_emitted >= 1`.

| result | reading | next action |
|---|---|---|
| **4 of 4** | No detection regression. The measurement was contaminated. | W19's restoration scope becomes ZERO. W19 reduces to the regression gate alone. |
| **2-3 of 4** | Contamination was real but is not the whole story. | Keep W19 open for the specific rules still silent; investigate each individually, not as a family. |
| **0-1 of 4** | A real recall problem that survives a clean input. | H3 moves to front. Pay for the 2x2, and look at prompt construction rather than file discovery. |

Partial credit note: `ARCH-NO-SECRETS-IN-URL-1` had 3 rows at baseline. **>= 1 row counts as
returned** for the primary outcome; the row count is recorded but a drop from 3 to 1 is not
scored as a miss here, because per-rule row counts are visibly noisy on this fixture.

## 6. Validity checks — if these fail the run is VOID, not negative

Checked before the primary outcome is read:

1. `code_chars` drops from 277,543 to roughly 100,000. Confirms the cache left the digest.
2. `files_scanned` is 87, not 88.
3. `provenance.audit_model` is recorded as `claude-sonnet-5`. If it is `None` again the pin did
   not take and H2 is still uncontrolled.
4. **No finding is located in `.camerata-scan-cache.json`.** The `50d03a7` run emitted
   `SEC-NO-DISABLED-TLS-1` CRITICAL at `.camerata-scan-cache.json:1778` at queue position #1.
   Its absence is the direct proof W20 works in production, not just in a unit test.

## 7. Variance control

The four `SUPABASE-AUTH-*` rules (`ANON-SIGNIN`, `EDGE-JWT`, `REDIRECT-ALLOWLIST`,
`PASSWORD-POLICY`) are the noise gauge: they fired in the `50d03a7` mid run and not in the
`50d03a7` late run, same engine, same fixture, 3h28m apart. They are NOT part of the primary
outcome.

They calibrate it. If the four target rules return AND the supabase four also return, the
reading is clean. If the targets return but the supabase four stay absent, run-to-run variance
is still large enough that a single run cannot settle anything, and the 4-of-4 cell above
should be read as "promising" rather than "settled" — which would itself be the decisive
argument for N-run agreement in the regression gate rather than advisory-only.

---

# RESULT (run 2026-10-18 15:39 to 16:51, engine `1e3280a`, exit 0)

## Validity checks: 4 of 4 PASS

| check | expected | actual | |
|---|---|---|---|
| `code_chars` | ~100,000 | **98,522** | PASS |
| `files_scanned` | 87 | **87** | PASS |
| `audit_model` | recorded | **`claude-sonnet-5`** | PASS |
| findings inside the cache | 0 | **0** | PASS |

`code_chars` came back at **98,522, which is the baseline's figure to the character**, at the
baseline's exact file count. The input is now identical in volume to what `96ed204` saw, so
this is as clean a control as the fixture can provide. The bogus `SEC-NO-DISABLED-TLS-1`
CRITICAL that sat at queue position #1 is gone. **W20 is confirmed in production, not just in
a unit test.**

## Primary outcome: 4 of 4 returned. Variance control: 4 of 4 present.

| rule | baseline | contaminated | controlled |
|---|---|---|---|
| `ARCH-EXACT-DECIMALS-1` | 1 | 0 | **5** |
| `ARCH-TOKEN-REFRESH-ROTATION-1` | 1 | 0 | **1** |
| `JAVASCRIPT-PUBLIC-ENV-PREFIX-SECRET-1` | 1 | 0 | **1** |
| `ARCH-NO-SECRETS-IN-URL-1` | 3 | 0 | **2** |

All four variance controls returned as well (`ANON-SIGNIN` 2, `EDGE-JWT` 3,
`REDIRECT-ALLOWLIST` 1, `PASSWORD-POLICY` 1), which is the pre-registered condition for a
CLEAN reading rather than a lucky draw.

## Every one of the twelve now emits

| | rules emitting | findings emitted |
|---|---|---|
| contaminated | 83 | 189 |
| controlled | **106** | **348** |

+23 rules and +159 findings from removing one file. `ARCH-OBSERVABILITY-SILENT-FAILURE-1` went
13 → 1 → **12**. `SUPABASE-AUTH-EDGE-JWT-1` found **3** sites where the baseline found 2, which
is all three `verify_jwt = false` blocks — the "config-block iterator that skipped the first
block" was the baseline being WORSE, not the current engine being broken.

## Verdict, per the pre-registered table: the 4-of-4 cell

**There was never a detection regression. W19's restoration scope is ZERO.**

The engine at HEAD out-detects the `96ed204` baseline on this fixture once it stops auditing
its own output. The entire "hearth lost five working detector classes" finding — which set the
priority of the whole remediation and made W19 the first item of Wave 1 — was the engine
reading a 212KB cache file it had written into the repo on a previous run.

W19 reduces to the regression gate alone, and the gate's design changes too: the thing it most
needed to catch was an INPUT defect, which no output-diffing floor would have identified as
such. It would have reported twelve rule regressions and sent someone to read twelve rules.

## Scope of the damage: hearth only

| repo | files | code_chars | cache-located findings |
|---|---|---|---|
| hearth | 88 | 277,543 | **1** |
| faktura | 38 | 25,753 | 0 |
| lumen | 54 | 51,423 | 0 |
| tutorly | 45 | 43,524 | 0 |

Hearth was the only repo scanned twice on 2026-10-16 and therefore the only one carrying a fat
cache. So:

- **Hearth's 30-of-46 grade is INVALID.** It graded a run in which 63% of the audit prompt was
  the engine's own JSON. It cannot be cited as evidence about the engine, and the remediation
  design's section 1 reading built on it ("hearth's misses are overwhelmingly regressions")
  falls with it.
- **The other four repos' grades stand**, and so does everything the graders found that was
  observed across all five: the kind/honesty defects in 2B and the presentation defects in 2C.
  Those are NOT contamination artefacts and Waves 2 and 3 are unaffected.

## Honest limits

- **Recall, not correctness.** 348 emitted findings is not 348 good findings. Whether the
  restored rows are right is a separate question this run does not answer.
- **One repo, one run.** The variance controls make a fluke unlikely but not impossible.
- **H3 (prompt dilution, 130 → 147 rules) was never controlled** and is now moot for this
  question, since the rules returned with it still in place. It is not disproven, just not
  needed as an explanation.

## 8. What this run does NOT test

- Whether the restored findings are CORRECT. Recall only.
- Any of the other four repos.
- The convention-synthesiser defect, which is read from source and is model-independent.
- W21 paths, also deterministic.

Those proceed in parallel and do not depend on this result.
