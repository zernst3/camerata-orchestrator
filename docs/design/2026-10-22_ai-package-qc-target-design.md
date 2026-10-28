# AI package QC: reading the deliverable after generation (target design, V2)

**Status:** target design, NOT V1. V1 ships the deterministic export release gate (secret
redaction check first, plus the existing export invariants run at export time). Revisit after the
graded accuracy tests show what the deterministic gate misses.

## Why a second layer

The deterministic gate proves the package is internally consistent: formats agree, hashes match,
provenance matches, nothing is missing, no secret shape survives. It cannot judge whether the
package reads correctly to a client. An AI reviewer reads the finished package the way the client
will, right after generation, before release.

## What it would check (things only a reader catches)

- **Narrative vs data.** The PDF's prose (executive summary, "three things this week", counts in
  sentences) agrees with the tables and the workbook. "Five critical issues" when the table lists
  four is a defect no format check sees.
- **Rendering.** Page images through a vision model: text overflowing a cell or page, clipped
  tables, overlapping elements, empty pages, a chart with no data, garbled characters.
- **Client voice.** No pipeline jargon, internal ids, tool names or retry narration in client
  text (W25 checks this at the source; the reviewer catches what slips through rendering).
- **Coherence.** Contradictions across sections (a rule listed as healthy and as violated),
  fixes that do not match their finding, severities whose explanation argues a different level.
- **Actionability.** Each fix names a place and a change a developer can act on; flag vague or
  placeholder fix text.
- **Completeness to a reader.** Sections that render but say nothing, truncated sentences, a
  finding cited in the summary but absent from the appendix.

## Shape (when built)

- Runs after the deterministic gate passes, on the exact bytes to be released.
- Structured output: a list of issues with location and evidence; ANY issue holds the release
  for operator review (it does not auto-fix; the reviewer is advisory, the operator decides).
- Deterministic where possible: counts and cross-references are extracted and compared in code
  first; the model judges only what code cannot (prose meaning, visual layout).
- Never sees raw secrets (the package is already redacted, and the release gate ran first).
- Model tiering: a fast tier for text checks; a vision-capable tier only for page renders.
  Cost bounded per export and stamped in provenance.
- Overlaps with the 19f grader (which grades the package during the graded sweep). Reuse its
  rubric where they agree; the grader measures, this reviewer gates.
