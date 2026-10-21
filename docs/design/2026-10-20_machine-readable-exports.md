# Machine-readable exports in the product package: CSV and JSON per dataset (2026-10-20)

## Status

Design, not yet built. Camerata is in the build phase: no clients, no shipped
deliverables, so the package layout has no backward-compatibility constraint. (Supersedes
the narrower `2026-10-20_csv-export.md`, which covered CSV only.)

## The gap this closes

The product-export ZIP today carries (`crates/server/src/lib.rs`, `build_product_zip`):

| File | Format | Covers |
|------|--------|--------|
| `{stem}.pdf` | PDF | the curated narrative |
| `{stem}-findings.xlsx` | binary (Excel) | the COMPLETE dataset, 8 worksheets |
| `findings.json` | JSON | ONLY the "All Findings" rows, wrapped with provenance + summary |
| `README.txt` | text | manifest |

The xlsx holds seven data worksheets - **All Findings, Deviations, Conventions,
Dependencies, False Positives, Coverage**, plus per-category views - but it is the ONLY
artifact that carries six of them. `findings.json` mirrors just one (All Findings). So
five datasets a client's tooling might want (the convention inventory, the dependency
list, the reviewer's false-positive exclusions, the full coverage roster, the
deviations) exist ONLY inside a binary Excel file. That is the asymmetry to fix.

The fix: emit **every data worksheet in both plain-text machine formats** - CSV (flat,
universal, diffable, awk/pandas/csvkit friendly) and JSON (structured, nested-field
friendly, the shape a web/CI consumer expects) - so no dataset is trapped in the xlsx.

## What ships, and the folder layout

One CSV and one JSON per DATA worksheet. Index (pure navigation) and the per-category
sheets (filtered duplicates of All Findings - the `Category` column already filters) get
neither.

```
{stem}.pdf
{stem}-findings.xlsx
findings.json            # KEPT as-is: the headline All-Findings view, wrapped with
                         # provenance (repos, SHA, generated-at) + summary counts
README.txt
csv/
  all-findings.csv
  deviations.csv
  conventions.csv
  dependencies.csv
  false-positives.csv
  coverage.csv
json/
  all-findings.json
  deviations.json
  conventions.json
  dependencies.json
  false-positives.json
  coverage.json
```

Grouping choice: **by format** (`csv/` and `json/`), not by dataset
(`all-findings/`, `deviations/`, ...). A consumer who wants "all the JSON" or "all the
CSV" opens one folder; per-dataset folders would be six folders of two files each and
scatter a format across them. (This is the one place I went against the literal "a
folder for each set" - see open question 1; say the word and I flip it to per-dataset.)

### `findings.json` stays at the root, and why there is a little overlap

`findings.json` keeps its current role: the headline, provenance-WRAPPED view of All
Findings (`{ cover/provenance, summary counts, findings: [...] }`), the thing a CI job
already points at. The new `json/*.json` files are FLAT arrays of row objects - the same
rows, without the wrapper - so:

- `json/all-findings.json` is the flat sibling of the rows inside `findings.json`.
- the provenance/summary wrapper lives ONCE (in `findings.json`), not duplicated into
  six per-set files.

That means All Findings appears in three shapes (wrapped `findings.json`, flat
`json/all-findings.json`, `csv/all-findings.csv`). That is intentional and friendly:
same data, the shape each consumer wants. The README names all three plainly.

## The load-bearing principle: one row-building pass, N serializers

This codebase's hard rule for the export artifacts is that they **never disagree**: the
PDF, the xlsx, and `findings.json` are all built from the SAME pass over the scan
(`report_export::partition_rows` and the typed per-sheet row structs the xlsx builds
from). `findings.json`'s `fix` field is pinned equal to `resolve_fix`'s output by a
test; a cross-artifact equality gate compares the xlsx against `findings.json`.

Every new CSV and JSON file MUST be another SERIALIZER over those same in-memory row
structs - never an independent walk of the scan. Concretely: the functions in
`xlsx_export.rs` that build each sheet already assemble typed rows; the CSV and JSON
emitters consume those exact rows. This is the single most important constraint - a path
that re-derives rows would be a new source of truth that drifts, which is the whole
defect the "never disagrees" design prevents. One pass, many serializations, all
asserted identical.

## Formats, with the choices named

**CSV** (RFC 4180, via the `csv` crate, never hand-rolled quoting):

| Choice | Decision |
|--------|----------|
| delimiter | comma |
| quoting | quote any field with comma / `"` / CR / LF; escape `"` by doubling |
| encoding | UTF-8, no BOM (tool-friendly, matches `findings.json`; Excel users open the xlsx) |
| line endings | LF (matches every other text artifact here; documented) |
| header row | yes, column names identical to the xlsx sheet's header row |

**JSON** (`serde_json`, pretty-printed for diffability, same as `findings.json`):

- Each `json/*.json` is an array of objects, one object per row, keys = the sheet's
  column identifiers (stable snake_case, not the human header text).
- Field values typed, not stringified: a severity is a string, a line number an
  integer, a sites count an integer - the structure CSV flattens to text is preserved.
- UTF-8, LF, trailing newline.

## Build placement

`build_product_zip` (`crates/server/src/lib.rs`) is the single ZIP-assembly point, and
both entry points route through it: `export_product` (HTTP) and
`camerata inspect --export` (headless CLI, `crates/cli/src/inspect_cmd.rs`). Adding the
folders there means BOTH gain them at once with no divergence risk. The signature grows
by one argument: a list of `(archive_path, bytes)` for the `csv/*` and `json/*` entries,
produced by a new `machine_export` module that takes the same
`(report, dispositions, corpus, opts)` the xlsx builder takes.

## Testing (mirrors the existing cross-artifact discipline)

1. **Parity gate.** `csv/all-findings.csv` and `json/all-findings.json`, parsed back,
   each equal the xlsx "All Findings" rows and the `findings.json` row set - one pass,
   now four serializations, asserted identical on every shared column.
2. **Per-set parity.** Each `csv/X.csv` equals `json/X.json` equals the xlsx `X` sheet,
   for all six datasets.
3. **CSV dialect / round-trip.** A finding whose `detail` has a comma, a `"`, and a
   newline survives write-then-parse as one row, text intact (the escaping test a
   hand-rolled writer fails).
4. **Set gate.** The ZIP contains exactly the six `csv/*.csv` and six `json/*.json`
   entries - no Index, no per-category duplicates.
5. **e2e.** `generate_sample_report` also writes `sample-report/csv/` and
   `sample-report/json/`, reviewable fixtures beside the PDF and xlsx.

## Resolved decisions (Zach, 2026-10-20)

1. **Folder grouping: by FORMAT** - nested `csv/` and `json/` subfolders inside the ZIP.
2. **Comprehensive and symmetric** - every data worksheet gets BOTH a CSV and a JSON, as
   many of each as it takes to cover the full dataset (the six named here today, and any
   new data sheet W27 adds). The flat `json/all-findings.json` stays alongside the
   wrapped `findings.json`; symmetry wins over avoiding the small overlap.
3. **Inside the product ZIP, not a separate download.** No standalone CSV/JSON route;
   the files ride in the package next to the PDF and xlsx.

## Sequencing

Build AFTER W27's xlsx worksheet changes land (W27 reworks the sheet set - per-kind
sheets, an R-id column). The CSV/JSON set is defined as "one per data worksheet," so it
tracks whatever W27 settles on; building it first would target a layout about to change.
If wanted sooner, build against today's sheets and let the set follow when W27 moves.

## Non-goals

- No change to the xlsx, the PDF, or the existing `findings.json`. CSV/JSON-per-set is
  additive.
- No new data. The new files expose exactly what the xlsx sheets already contain.
- Not a replacement for the xlsx: it keeps the Index, hyperlinks, per-category sheets and
  formatting a spreadsheet user wants. CSV/JSON are the plain-text siblings for tooling.
