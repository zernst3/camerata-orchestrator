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
manifest.json            # the package index + provenance + integrity (see below)
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

## `manifest.json` - the package index, provenance, and integrity (Zach, 2026-10-20)

One `manifest.json` at the ZIP root. It answers three questions a recipient asks: what
is each file, can I trust it is intact, and what exactly was inspected to produce it.

```json
{
  "manifest_schema_version": "1.0",
  "generated_at": "2026-10-20T14:32:00Z",
  "inspection": {
    "repos": [
      { "name": "harbor-member-portal", "commit": "a1b2c3d4e5f6..." }
    ],
    "engine_version": "camerata 0.3.1",
    "corpus_version": "sha256:9f86d0...",
    "rules_last_reviewed": "2026-10-18"
  },
  "files": [
    {
      "path": "camerata-sample-audit.pdf",
      "purpose": "Curated narrative report for a decision-maker.",
      "sha256": "e3b0c4...",
      "schema_version": null
    },
    {
      "path": "findings.json",
      "purpose": "All-Findings rows wrapped with provenance + summary counts.",
      "sha256": "a948904...",
      "schema_version": "findings/1.0"
    },
    { "path": "csv/deviations.csv", "purpose": "...", "sha256": "...", "schema_version": "deviations/1.0" }
    // ... one entry per file in the ZIP, except manifest.json itself
  ]
}
```

Field notes:

- **`files[]`** lists every file in the ZIP EXCEPT `manifest.json` (a file cannot hash
  itself). `purpose` is one plain sentence; `sha256` lets a recipient verify the file
  arrived intact; `schema_version` is per data-contract (`findings/1.0`,
  `deviations/1.0`, ...) and `null` for formats that have no schema (PDF, xlsx, README).
- **`inspection.repos[].commit`** is the exact SHA scanned, per repo (a scan can span
  several). This is the "what state of the code did these findings come from" anchor.
- **`engine_version`** is Camerata's own version (`env!("CARGO_PKG_VERSION")`), so a
  finding can always be traced to the engine that produced it.
- **`corpus_version`** is a deterministic content digest of the rule corpus (a sha256
  over the sorted rule files), so two packages built from the same rules carry the same
  corpus id even across machines. Not a hand-maintained version string.
- **`rules_last_reviewed`** is the staleness watermark made CLIENT-VISIBLE - this is the
  field surfacing where the client can see it ("rules last reviewed on 2026-10-18").
  Sourced from `CORPUS_FRESHNESS.toml` (see the staleness-check design). Because that
  watermark is per-domain, the client-facing value is the OLDEST confirmed date among
  the domains that actually fired in THIS scan - the honest weakest-link date, never a
  flattering newest. Until the staleness feature ships, this field is `null` and the
  README omits the line rather than print a fabricated date.

### Build order and the self-reference problem

The manifest hashes every other file, so it is built LAST: compute the PDF, xlsx,
`findings.json`, every CSV and JSON as bytes; then build the manifest over those bytes;
then add `manifest.json`; then finalize the ZIP. `manifest.json` never appears in its
own `files[]`. This lives in `build_product_zip` (`crates/server/src/lib.rs`), the
single assembly point, so both the HTTP export and `camerata inspect --export` get an
identical manifest.

### The README mirrors the new provenance for the human reader

`product_export_readme` today carries file purposes, "Repos inspected", "Generated",
and the disclaimer - but NOT the commit SHA, engine version, corpus version, or
rules-last-reviewed date. Extend the README prose to print those same provenance lines
(commit per repo, engine version, corpus version, "Rules last reviewed on YYYY-MM-DD")
so a human reading the text file sees what the manifest encodes for a machine. The
manifest is the source of truth; the README is its human rendering - built from the
same provenance struct so they cannot drift.

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
6. **Manifest integrity gate.** Every `files[]` entry's `sha256` equals the actual
   hash of that entry in the ZIP; `files[]` covers every ZIP member except
   `manifest.json`; `manifest.json` is absent from its own `files[]`. A deliberately
   corrupted byte makes the recomputed hash mismatch - the test asserts that is caught.
7. **Provenance-parity gate.** The README's printed commit / engine version / corpus
   version / rules-last-reviewed equal the manifest's, since both render the same
   provenance struct (they cannot drift).

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

## Execution plan (turnkey, verified against the tree 2026-10-20)

Green, committable increments. Facts confirmed by reading the code:

- `xlsx_export::partition_rows` is the single source: `(Vec<FindingRow>, Vec<DepRow>)`.
  `FindingRow` ALREADY derives `Serialize` and IS the exact `findings.json` row shape
  (`build_findings_export` filters it to non-FP). The other sheets build from `DepRow`,
  `CoverageRow` + `RulesAppliedRow` (Coverage), `ConventionRow` (Conventions),
  `NotAssessedRow` (part of the not-assessed/deviations surface), `DeviationGroupRow`
  (Deviations) - all simple (String/usize/bool/u8) but NOT yet `Serialize`.
- There is NO `csv` crate dependency yet - add `csv = "1"` to the server crate.

1. **Field contract first (the quality gate).** For each row struct, decide the EXPORTED
   keys: stable snake_case, matching the sheet's visible columns, with `#[serde(skip)]` on
   internal-only fields (`ConventionRow::rank`, `FindingRow::citation_kind` already skipped).
   Do NOT blanket-derive and leak sort keys. This is the step that makes the JSON a contract
   rather than a struct dump.
2. **`machine_export` module** (server): `fn sheet_json(rows) -> Vec<u8>` (serde_json pretty)
   and `fn sheet_csv(rows) -> Vec<u8>` (the `csv` crate's `Writer::serialize`, never
   hand-rolled). One pair per row type. Parity test: All-Findings JSON == `findings.json`
   rows; CSV round-trips a comma/quote/newline cell.
3. **Add `Serialize` to the five remaining row structs** with the field contract from (1).
4. **`manifest.json` builder**: walk the produced `(path, bytes)` list, sha256 each, assemble
   the `inspection` provenance (commit per repo, `env!("CARGO_PKG_VERSION")`, corpus content
   digest, `rules_last_reviewed` from `CorpusFreshness` oldest-relevant-domain). Build LAST.
5. **Wire `build_product_zip`** (`crates/server/src/lib.rs`): add a `&[(String, Vec<u8>)]`
   param for the `csv/*`, `json/*`, and `manifest.json` entries; both `export_product` (HTTP)
   and `inspect_cmd` (CLI) call it, so both gain the files at once. Extend
   `product_export_readme` to print the new provenance lines.
6. **Sample regen** (`generate_sample_report`): write `sample-report/csv/`, `json/`,
   `manifest.json` so they are reviewable fixtures.
7. The seven gates in the Testing section.

Note: W27 reworks the xlsx sheet set (per-kind sheets, R-id column). If W27 lands first, the
CSV/JSON set tracks its final sheets for free (one-per-data-sheet); if exports land first, the
set follows when W27 moves. Either order is fine; do NOT block one on the other.

## Non-goals

- No change to the xlsx, the PDF, or the existing `findings.json`. CSV/JSON-per-set is
  additive.
- No new data. The new files expose exactly what the xlsx sheets already contain.
- Not a replacement for the xlsx: it keeps the Index, hyperlinks, per-category sheets and
  formatting a spreadsheet user wants. CSV/JSON are the plain-text siblings for tooling.
