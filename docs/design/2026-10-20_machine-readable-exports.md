# Machine-readable exports in the product package: CSV and JSON per dataset (2026-10-20)

## Status

Built (2026-10-21), with all seven Testing gates in place (see the table under
[Testing](#testing-mirrors-the-existing-cross-artifact-discipline)). Camerata is in the build
phase: no clients, no shipped deliverables, so the package layout has no backward-compatibility
constraint. (Supersedes the narrower `2026-10-20_csv-export.md`, which covered CSV only.)

The text below is the original design; where the build differs from it, the section
[Where the build differs from this design](#where-the-build-differs-from-this-design) says so
and is the authority.

**The dataset set must be re-checked when the W27 PDF/xlsx rebuild reshapes the worksheets.**
The set is "one per data worksheet", and the Pass-3 per-kind restructure has already moved it
once (six datasets became eight). `gate2_csv_json_and_xlsx_agree_for_every_dataset_*` pins the
current eight by name and maps each to its worksheet, so a W27 change to the sheet set fails
that gate on purpose; update the dataset list, the sheet mapping and this document together.

## The gap this closes

The product-export ZIP today carries (`crates/server/src/lib.rs`, `build_product_zip`):

| File | Format | Covers |
|------|--------|--------|
| `{stem}.pdf` | PDF | the curated narrative |
| `{stem}-findings.xlsx` | binary (Excel) | the COMPLETE dataset, 8 worksheets |
| `findings.json` | JSON | ONLY the "All Findings" rows, wrapped with provenance + summary |
| `README.txt` | text | manifest |

The xlsx holds data worksheets - **All Findings, Deviations, Conventions, Dependencies,
False Positives, Coverage** (which also carries the not-assessed and rules-applied tables) -
plus per-category views, but it is the ONLY artifact that carries most of them.
`findings.json` mirrors just one (All Findings). So the other datasets a client's tooling
might want (the convention inventory, the dependency list, the reviewer's false-positive
exclusions, the full coverage roster, the deviations, the not-assessed list, the rules-applied
audit trail) exist ONLY inside a binary Excel file. That is the asymmetry to fix.

The fix: emit **every data worksheet in both plain-text machine formats** - CSV (flat,
universal, diffable, awk/pandas/csvkit friendly) and JSON (structured, nested-field
friendly, the shape a web/CI consumer expects) - so no dataset is trapped in the xlsx.

## What ships, and the folder layout

One CSV and one JSON per DATASET: **eight** of them. Index (pure navigation) and the
per-category sheets (filtered duplicates of All Findings - the `Category` column already
filters) get neither.

| Dataset | Rows are | Worksheet it mirrors |
|---------|----------|----------------------|
| `all-findings` | every non-false-positive finding, all kinds | "All Findings" (defect, deviation and kind-less rows only; see below) |
| `deviations` | one per rule with consistency deviations | "Deviations" |
| `conventions` | one per derived or absent convention | "Conventions" |
| `not-assessed` | one per rule armed but not evaluable | "Coverage", section "Checks we could not complete this scan" |
| `dependencies` | one per dependency advisory | "Dependencies" |
| `false-positives` | findings the reviewer excluded, with the reason | "False Positives" |
| `coverage` | one per audited rule, found or clean | "Coverage", the main per-rule table |
| `rules-applied` | one per multi-option rule an alternative was chosen for | "Coverage", section "Rules applied (alternative selection)" |

The original design named six (`all-findings`, `deviations`, `conventions`, `dependencies`,
`false-positives`, `coverage`). The Pass-3 per-kind worksheet restructure added `not-assessed`
(its rows are a coverage disclosure, not a finding) and `rules-applied` (the Coverage sheet's
alternative-selection appendix), and each got the same CSV and JSON pair.

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
  not-assessed.csv
  dependencies.csv
  false-positives.csv
  coverage.csv
  rules-applied.csv
json/
  all-findings.json
  deviations.json
  conventions.json
  not-assessed.json
  dependencies.json
  false-positives.json
  coverage.json
  rules-applied.json
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
CSV" opens one folder; per-dataset folders would be eight folders of two files each and
scatter a format across them. (This is the one place I went against the literal "a
folder for each set" - see open question 1; say the word and I flip it to per-dataset.)

### `findings.json` stays at the root, and why there is a little overlap

`findings.json` keeps its current role: the headline, provenance-WRAPPED view of All
Findings (`{ cover/provenance, summary counts, findings: [...] }`), the thing a CI job
already points at. The new `json/*.json` files are FLAT arrays of row objects - the same
rows, without the wrapper - so:

- `json/all-findings.json` is the flat sibling of the rows inside `findings.json`.
- the provenance/summary wrapper lives ONCE (in `findings.json`), not duplicated into
  eight per-set files.

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
| header row | yes; column names are the JSON keys (stable snake_case), NOT the xlsx sheet's human header text. The CSV is derived from the JSON, so the two agree by construction. |

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

All seven gates exist. The design's numbering is kept; where the built gate is not literally
what was written, the right-hand column says so.

| # | Gate | Where | Notes |
|---|------|-------|-------|
| 1 | Parity: `csv/all-findings.csv` and `json/all-findings.json`, parsed back, equal the `findings.json` rows | `json_all_findings_matches_findings_json_rows` and the CSV-from-JSON derivation (`xlsx_export` tests) | `all-findings` is a SUPERSET of the xlsx "All Findings" sheet (it is unpartitioned), so it is not equal to that sheet's rows; gate 2 accounts for the partition. |
| 2 | Per-set parity: each `csv/X.csv` equals `json/X.json` equals the xlsx worksheet, for ALL EIGHT datasets | `tests/machine_export_dataset_parity_e2e.rs` | Reads the workbook back cell by cell from the real `.xlsx` bytes. CSV vs JSON is row count, header set and every cell; JSON vs xlsx projects each column from a JSON key under an explicit rule, and every JSON key must be rendered or named JSON-only. Runs with no corpus and with the real corpus. |
| 3 | CSV dialect / round-trip: a `detail` with a comma, a `"` and a newline survives as one row | `csv_exports_cover_every_table_and_survive_nasty_cells` (`xlsx_export`); also exercised by gate 2's fixture | |
| 4 | Set gate: exactly the eight `csv/*.csv` and eight `json/*.json`, no Index, no per-category duplicates | `json_exports_cover_every_table_and_parse`, `csv_exports_cover_every_table_and_survive_nasty_cells`; gate 2 asserts the dataset names in both formats | |
| 5 | e2e: `generate_sample_report` also writes the sample package | `tests/generate_sample_report.rs` | The sample ZIP (with `csv/`, `json/`, `manifest.json`) is built through the shipping `product_export_readme` and `build_product_zip`. The generator does NOT write `sample-report/csv/` and `sample-report/json/` as loose folders beside the PDF, as the design asked; the files exist only inside `camerata-sample-audit.zip`. |
| 6 | Manifest integrity: every `files[]` sha256 equals the actual hash; `files[]` covers every member except `manifest.json`; a corrupted byte is caught | coverage clause: `manifest_lists_and_hashes_every_file_except_itself` (`lib.rs`); corruption clause: `tests/product_export_manifest_gates.rs` | Corruption flips one bit of each exported file in turn, re-zips with valid CRCs under the original manifest, and requires the check to flag exactly that file (plus a clean control and a swapped-file case). |
| 7 | Provenance parity: the README's commit per repo / engine version / corpus version / rules-last-reviewed equal the manifest's | `tests/product_export_manifest_gates.rs` | Both render from one `ManifestProvenance` (`readme_lines`, `inspection_json`). Covers the populated case, a generic "every manifest `inspection` value appears in the README" check, and the absent case (null in the manifest, line omitted from the README). |

## Resolved decisions (Zach, 2026-10-20)

1. **Folder grouping: by FORMAT** - nested `csv/` and `json/` subfolders inside the ZIP.
2. **Comprehensive and symmetric** - every data worksheet gets BOTH a CSV and a JSON, as
   many of each as it takes to cover the full dataset (the eight listed above today, and
   any new data sheet W27 adds). The flat `json/all-findings.json` stays alongside the
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

## Where the build differs from this design

Found while building gates 2, 6 and 7. None of these is a regression; each is a place the text
above was wrong or the build stopped short of it.

1. **Eight datasets, not six** (see above).
2. **`all-findings` is not the xlsx "All Findings" sheet.** The JSON/CSV carry every
   non-false-positive row of every kind; the workbook routes defects, deviations and kind-less
   rows to "All Findings", convention rows to "Conventions" and not-assessed rows to Coverage.
   Equality with the sheet was never true after Pass 3; the right invariant (every row lands on
   exactly one sheet) is `req_pass3_every_exported_row_lands_on_exactly_one_sheet`, and gate 2
   checks the same partition across all three formats.
3. **CSV headers are JSON keys, not the sheet's header text** (corrected in the Formats table).
4. **A worksheet is not a dump of the JSON.** It title-cases severities, joins location lists
   with newlines, writes "not counted" for an unknown denominator, and derives "Result" and "How
   we checked it" from counts and citation kinds. Gate 2 therefore compares a projection, not
   raw equality, and fails on a JSON key that is neither projected nor listed as JSON-only.
5. **`all-findings` rows do not share one key set.** The finding kind's payload is flattened
   into the row, so only deviation, convention, no-convention and not-assessed rows carry their
   payload keys. The CSV header is the union of keys (first-seen order); a JSON consumer should
   not assume every row has every key.
6. **An empty dataset has no CSV header.** The header is derived from the rows, so a dataset
   with no rows is an empty CSV with no column names (and `[]` in JSON). Consistent, but a
   consumer cannot learn the columns from an empty export.
7. **Column order is `serde_json`'s `preserve_order`, now requested explicitly.** It had been
   enabled only through feature unification with an unrelated crate.
8. **Manifest provenance is now fully wired, with three deliberate differences from the
   sketch.** Before, `commit`, `corpus_version` and `rules_last_reviewed` existed in the struct
   but no call site filled them, so every package shipped `null` for all three. All three
   callers (`export_product`, `camerata inspect --export`, the `generate_sample_report` test)
   now build `ManifestProvenance` through one function,
   `client_provenance::export_provenance` (`crates/server/src/client_provenance.rs`):
   - **Commit is per repo, as designed.** `inspection.repos` is `[{ name, commit, dirty }]`,
     taken from the scan's recorded `AuditedRef` (the cover's `audited_refs`). The top-level
     `commit` is gone: with a commit per repo it was redundant and wrong for a multi-repo scan.
     `commit` is `null` for a repo the scan could not read as a git checkout. `dirty` is an
     addition the sketch lacked: it is true when the tree had uncommitted changes at scan time,
     in which case `commit` is the base the scanned code was modified from, not an exact
     description of it. The README prints `Commit (<repo>): <sha>` per repo, with a note when
     dirty.
   - **`corpus_version` is `camerata_rules::corpus_version`**, `sha256:` over the sorted
     `(id, version, content hash)` of every rule (exactly the `CORPUS_LOCK.toml` content, with
     its own domain-separation tag), not "a sha256 over the sorted rule files". It is the digest
     of the WHOLE corpus the exporting process loaded, not of the subset the scan audited. When
     the scan and the export run on one build (the CLI always, the server unless rebuilt in
     between) that is the scan's corpus; a server that was rebuilt with a different corpus
     between scan and export would stamp the newer one. A rule with no `version` contributes as
     `-`.
   - **`rules_last_reviewed` is `camerata_rules::client_rules_last_reviewed`**: the OLDEST
     effective review date (`effective_review_date`, the later of a rule's own `last_reviewed`
     and its domain watermark) across the rules the scan audited
     (`provenance.audited_rule_ids`), so the client is never told the rules are fresher than the
     least-reviewed one. It is `None`, and the README omits the line, when any audited rule has
     never been reviewed, when an audited id is not in the loaded corpus (a custom rule has no
     review record), when the scan audited nothing, or when there is no corpus or no readable
     watermark. Today no domain has been confirmed, so every real package carries `null` here;
     that is the honest value, and it becomes a date only once the first confirmed sweep covers
     every domain a scan used.

   **The freshness gate.** This date is the one deliberate exception to "no corpus-freshness
   code in a client-artifact builder" (`no_corpus_freshness_text_reaches_any_client_artifact`).
   The exception is one file, not a loosening: `client_provenance.rs` is the bridge, and the gate
   lets it name exactly `CorpusFreshness`, `corpus_freshness_path` and
   `client_rules_last_reviewed` (`SANCTIONED_BRIDGE_IDENTIFIERS`). The nag formatter, the report
   types, the marker list, the due-soon logic and the watermark file name stay forbidden there
   too. The artifact builders (`report_export.rs`, `xlsx_export.rs`, the export section of
   `lib.rs`) and the CLI's `inspect_cmd.rs` are scanned with NO allowance, so they cannot name
   even the sanctioned function; they receive a finished `ManifestProvenance`.
   `freshness_gate_allows_only_the_sanctioned_date_path` pins that the scanner can still fail
   (the nag, `FreshnessReport`, the markers and `due_soon` are each caught, in the bridge too),
   and `tests/export_provenance.rs` proves the packages carry the date and no maintenance text
   even from a watermark with overdue domains.
9. **README and manifest provenance now come from one struct.** The design asked for this; the
   build had two hand-written renderings (and the sample generator carried a third, stale copy
   of the README text). `ManifestProvenance` is now the single source, and
   `product_export_readme(stem, &provenance)` no longer reads the report cover itself.
10. **No `machine_export` module.** The serializers (`build_json_exports`, `build_csv_exports`)
    live in `xlsx_export.rs` beside the row structs they serialize, and CSV is derived from the
    JSON (`json_array_to_csv`) rather than serialized separately from the row structs. Both
    still consume the one `partition_rows` pass, which is the design's actual constraint.
11. **Built before W27 settled**, which the Sequencing section permitted ("build against
    today's sheets and let the set follow"). The set did follow once already (item 1).

## Non-goals

- No change to the xlsx, the PDF, or the existing `findings.json`. CSV/JSON-per-set is
  additive.
- No new data. The new files expose exactly what the xlsx sheets already contain.
- Not a replacement for the xlsx: it keeps the Index, hyperlinks, per-category sheets and
  formatting a spreadsheet user wants. CSV/JSON are the plain-text siblings for tooling.
