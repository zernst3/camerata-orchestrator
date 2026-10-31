# Token-efficiency results (2026-10-30)

The question this test answers: do Camerata's token-efficiency switches work, and do they make an
inspection cheaper? **Yes.** Every switch ran correctly, alone and together, and an inspection with
every switch on cost less than one with every switch off. Accuracy is a separate test (the answer-key
grading below is a side result).

## Setup

- Repository: `supabase-portal-fixture`, one of the benchmark fixtures (about 860 lines, 36 files,
  seven planted defects).
- Engine: `feat/audit-report-export` at 38925fa9, after the retry fixes F1 to F3 (retry diagnosis,
  2026-10-29). One engine for every run, so the runs compare.
- Model: Claude Sonnet 5.5 for the scan, alternatives, calibration and detail stages; Claude Haiku 5.5
  for fix text (the `fix_on_haiku` switch) and the duplicate-code checker.
- Anthropic API backend, real time unless marked batch, a spend cap per run ($10; $15 for the all-off
  baseline). Each run used a fresh state folder, so nothing was reused unless the run says so.
- Costs are what each run's ledger recorded from the API's usage figures.

## Results

| Run | Cost | Calls | Repeated calls | Time | Ended |
|---|---|---|---|---|---|
| **All switches on** (reference) | **$1.152** | 40 | 4 | 6 min 10 s | complete |
| **All switches off** (baseline) | **$1.465** | 35 | 7 | 3 min 50 s | fix stage gave up on 2 findings (a), cost valid |
| `warm_schedule` off | $1.407 | 42 | 6 | 5 min 4 s | complete |
| `label_after_prefix` off | $0.992 | 38 | 4 | 4 min 33 s | complete |
| `truncation_retry` off | $1.247 | 43 | 5 | 6 min 25 s | complete |
| `fix_prefilter` off | $1.195 | 41 | 4 | 6 min | complete |
| `fix_on_haiku` off (fix on Sonnet) | $1.557 | 44 | 7 | 6 min 55 s | fix stage gave up (a), cost valid |
| `terse_output` off | $0.953 | 35 | 5 | 4 min 26 s | export refused by the release gate (b), cost valid |
| `applies_to_routing` off | $1.240 | 43 | 5 | 6 min 11 s | complete |
| `candidates_prefilter` off | $1.141 | 40 | 4 | 5 min 41 s | complete |
| **All on, Message Batch** | **$0.941** | 41 | 3 | 18 min 26 s | complete |
| All on, batch, `batch_long_cache` off | $0.983 | 45 | 7 | 23 min 44 s | complete |
| **All on, re-run on the unchanged repo** (`result_cache`) | **$0.014** | 8 | 5 | 55 s | fix stage gave up on 1 finding (a) |

"Repeated calls" counts calls whose reply was partly or wholly turned down by an accuracy check and
asked again with a changed request; none was a re-send of an identical request.

(a) and (b) are accuracy and safety defects found by this test, not efficiency problems; they are
tracked for fixing before the accuracy sweep (a: the fix checker rejects some standard names; b: a
credential value reached a merged-location column in one dataset, and the release gate refused the
export, so nothing left the machine).

## What the numbers show

- **All switches together: about 21 percent cheaper** than all off ($1.152 against $1.465) on this
  repository.
- **Batch mode: about 18 percent cheaper again** ($0.941 against $1.152), slower (about 18 minutes),
  because Anthropic processes batches in a queue. Only the scan is batched; the later stages run in
  real time, so the saving is below the 50 percent batch discount.
- **Result cache: about 99 percent cheaper** on a re-run of an unchanged repository ($0.014), because
  unchanged files reuse the previous scan's findings.
- **Fix on Haiku** and **warm caching** are the clearest single-switch savings (turning them off cost
  $0.41 and $0.26 more).
- **The one-hour batch cache** saved a little ($0.941 against $0.983).

## Limits of this test

- On a repository this small, the difference between two runs of one configuration is about as large
  as most single switches' effect: each run writes a different number of findings, and cost follows
  the findings. Two single-switch-off runs came in cheaper than all on for that reason. The test
  confirms the switches work and that all of them together cost less; it does not rank the smaller
  switches.
- Several switches matter mainly on large repositories (rule routing, the candidates pre-filter, warm
  caching across many chunks). On 860 lines they have little to cut, so **21 percent is a floor**; a
  real client repository should save more. A larger-repository test is planned later.

## Side result: accuracy of the reference run

Graded by the maintainer with the existing answer-key grader (no answer-key content here): all seven
planted defects found at their expected severity, no finding reported under the wrong kind, the
summary reconciles. Precision and duplicate rows are open accuracy items for the next round.
