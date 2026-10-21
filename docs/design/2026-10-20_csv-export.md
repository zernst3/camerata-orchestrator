# CSV export in the product package (2026-10-20)

## Status

Design, not yet built. Camerata is in the build phase: no clients, no shipped
deliverables, so there is no backward-compatibility or versioning constraint on the
package layout. We can add CSV cleanly.

## The question this answers

The product-export ZIP today carries three artifacts plus a manifest
(`crates/server/src/lib.rs`, `build_product_zip`):

| File | Format | Audience | Covers |
|------|--------|----------|--------|
| `{stem}.pdf` | PDF | board / buyer | the curated narrative |
| `{stem}-findings.xlsx` | binary (Excel) | engineers | the COMPLETE dataset, 8 worksheets |
| `findings.json` | JSON | tooling / CI | ONLY the "All Findings" rows + provenance |
| `README.txt` | text | everyone | manifest |

There is a gap between the two machine-facing artifacts. `findings.json` is
tool-friendly and plain-text but mirrors only ONE of the xlsx's worksheets ("All
Findings"). The xlsx carries six more data sheets (Deviations, Conventions,
Dependencies, False Positives, Coverage, and per-category views) but is a binary
format that needs Excel or a library to open, does not diff in git, and is awkward to
pipe through `awk`/`pandas`/`csvkit`.

CSV closes that gap: a universal, plain-text, one-table-per-file format that any tool
reads, that diffs cleanly, and that exposes EVERY data sheet, not just All Findings.

## What ships

CSV is one flat table per file, so it mirrors the xlsx worksheets. Not every sheet
earns a CSV:

| xlsx worksheet | CSV? | Why |
|----------------|------|-----|
| Index | NO | pure navigation (hyperlinks to other sheets); meaningless as a flat table |
| All Findings | **yes** | `all-findings.csv` - the complete row set |
| Deviations | **yes** | `deviations.csv` |
| Conventions | **yes** | `conventions.csv` |
| Dependencies | **yes** | `dependencies.csv` |
| False Positives | **yes** | `false-positives.csv` (the reviewer's exclusions, nothing silently dropped) |
| Coverage | **yes** | `coverage.csv` (every rule inspected this run, found or not) |
| per-category sheets (Security, etc.) | NO | redundant: each is a filtered view of All Findings, and the `Category` column already lets a consumer filter. Emitting them as CSV duplicates rows. |

So: **six CSVs**, one per data worksheet, excluding Index (navigation) and the
per-category sheets (filtered duplicates of All Findings).

### Packaging

The CSVs live in a `csv/` subfolder inside the existing ZIP, so the root stays clean:

```
{stem}.pdf
{stem}-findings.xlsx
findings.json
README.txt
csv/
  all-findings.csv
  deviations.csv
  conventions.csv
  dependencies.csv
  false-positives.csv
  coverage.csv
```

Filenames are lowercase-kebab. The README gains a short `csv/` section describing the
set as "the same data as the xlsx worksheets, one plain-text table per file, for
tooling that does not read Excel."

## The load-bearing principle: one row-building pass, no fourth copy of the data

This codebase's hard rule for the export artifacts is that they **never disagree**:
the PDF, the xlsx and `findings.json` are all built from the SAME pass over the scan
(`report_export::partition_rows` and the typed row structs the xlsx sheets already
build from). `findings.json`'s `fix` field is pinned equal to `resolve_fix`'s output
by a test; a cross-artifact equality gate compares the xlsx against `findings.json`.

The CSV writer MUST hook the same row structs the xlsx sheets are built from, not
re-derive rows from the scan. Concretely: the functions in `xlsx_export.rs` that build
each sheet already assemble typed rows (e.g. the `FindingRow` set behind "All
Findings", the dependency rows, the coverage rows). The CSV emitter consumes those
same in-memory rows and serializes them to CSV. It is a second SERIALIZER over shared
data, exactly as `findings.json` is, never a second data model.

This is the single most important constraint. A CSV path that walks the scan
independently would be a fourth source of truth that drifts, which is the defect the
whole "never disagrees" design exists to prevent.

## CSV dialect (RFC 4180, with the choices named)

| Choice | Decision | Why |
|--------|----------|-----|
| Delimiter | comma | it is CSV |
| Quoting | quote any field containing comma, double-quote, CR or LF; escape an embedded `"` by doubling it (`""`) | RFC 4180; findings carry prose with commas and quotes |
| Encoding | UTF-8, **no BOM** | tool-friendly, matches `findings.json`; the xlsx is the Excel-native artifact, CSV targets tooling. (Trade-off: Excel-on-Windows may mis-detect non-ASCII without a BOM. Accepted: a consumer who wants Excel opens the xlsx.) |
| Line endings | **LF** | matches every other text artifact in this repo and is git/Unix friendly. (RFC 4180 nominally says CRLF; LF is universally accepted and we document it.) |
| Header row | yes, column names identical to the xlsx sheet's header row | so a reader who knows the xlsx knows the CSV; All Findings keeps all 26 columns in the same order |
| Newlines inside a cell | preserved inside a quoted field | a multi-line `detail`/`fix` stays one logical row |

Use a vetted CSV crate (`csv`) rather than hand-rolling the quoting; hand-rolled CSV
quoting is a classic source of injection/escaping bugs.

## Build placement

`build_product_zip` (`crates/server/src/lib.rs`) is the single ZIP-assembly point, and
both entry points already route through it:

- `export_product` (the HTTP product-export endpoint), and
- `camerata inspect --export` (the headless CLI, `crates/cli/src/inspect_cmd.rs`).

So adding the CSVs to `build_product_zip`'s inputs means BOTH entry points gain them at
once, with no risk of the two layouts diverging. The signature grows by one argument: a
`&[(String, Vec<u8>)]` of `(csv_filename, csv_bytes)`, written under `csv/` in the
archive. The callers build the CSV bytes via a new `csv_export` module that takes the
same `(report, dispositions, corpus, opts)` the xlsx builder takes.

## Testing

Mirrors the existing cross-artifact discipline:

1. **Parity gate.** `all-findings.csv`, parsed back, equals the xlsx "All Findings"
   rows cell-for-cell on every shared column, and equals the `findings.json` row set.
   One pass, three serializations, asserted identical.
2. **Round-trip / dialect gate.** A finding whose `detail` contains a comma, a
   double-quote and a newline survives write-then-parse as one row with the exact
   original text. This is the escaping test that a hand-rolled writer fails.
3. **Set gate.** The ZIP contains exactly the six expected `csv/*.csv` entries, no
   Index, no per-category duplicates.
4. **e2e.** The sample-report regeneration (`generate_sample_report`) also writes the
   CSVs, so `sample-report/csv/` is a reviewable fixture alongside the PDF and xlsx.

## Open questions for Zach

1. **Six CSVs, or fewer?** The recommendation is all six data sheets. If you only want
   the machine-ingestion case covered, All Findings + Dependencies + Coverage would do,
   and findings.json already covers All Findings. I lean toward all six: the cost is
   one serializer pass and the value is "every sheet is now also plain text."
2. **A `csv/` subfolder, or flat in the ZIP root?** Recommendation: subfolder, to keep
   the root to the four headline artifacts. Flat would put eleven files at the root.
3. **Should the HTTP endpoint also offer a CSVs-only download** (not just inside the
   ZIP)? Recommendation: no for now. The CSVs ride in the package; findings.json
   already serves the single-file machine case. Add a standalone route only if a real
   consumer asks.

## Non-goals

- No change to the xlsx, the PDF, or `findings.json`. CSV is additive.
- No new data. The CSVs expose exactly what the xlsx sheets already contain.
- Not a replacement for the xlsx: the xlsx keeps the Index, hyperlinks, per-category
  sheets, column widths and formatting that a spreadsheet user wants. CSV is the
  plain-text sibling for tooling.

## Relationship to W27

W27 (the PDF rebuild) is reworking the xlsx worksheet set too (per-kind sheets, an
R-id column). The CSV set is defined as "one per data worksheet," so it tracks whatever
worksheet set W27 settles on automatically - the CSV emitter should be written against
the final sheet-row structs, so **sequence this after W27's xlsx changes land** to
avoid building CSVs against a sheet layout that is about to change. If CSV is wanted
sooner, build it against today's sheets and let the set follow the xlsx when W27 moves.
