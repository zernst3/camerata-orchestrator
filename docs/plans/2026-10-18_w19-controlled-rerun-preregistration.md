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

## 8. What this run does NOT test

- Whether the restored findings are CORRECT. Recall only.
- Any of the other four repos.
- The convention-synthesiser defect, which is read from source and is model-independent.
- W21 paths, also deterministic.

Those proceed in parallel and do not depend on this result.
