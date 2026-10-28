# Spike + work order: every token-saving measure for Anthropic API usage (2026-10-25)
For the Camerata development session. Part 1 is an investigation of the code; Part 2 is building the gaps it finds. Zach's goal: client inspections run on the Anthropic API at the lowest cost that does not reduce recall. Recall comes first: no saving is accepted if a fixture's recall drops.

## Evidence to start from
- Latest sweep (2026-10-16, real-time mode): output tokens were about 60% of cost; input about 480K fixed per run plus about 310 tokens per counted line, meaning each line is sent roughly 25 to 30 times across rule groups.
- Cache economics look inverted: faktura wrote 661K tokens to cache but read only 80K back; hearth wrote 1.34M and read 80K. Cache writes cost more than plain input, so caching that is written and not re-read costs money instead of saving it.
- The hearth smoke run logged "rule-routing: 3% input reduction" (routing barely reduces input) and several "fix-specific generation gave up after 2 batched retries plus one single-finding retry" (paid tokens with no output kept).

## Part 1: spike (read the code, measure, report; no behaviour changes)
For each item: EXISTS (file:line, and measured effect) / PARTIAL / MISSING, plus the expected saving on a 200K-line repo. Write the table to `_workspace/brownfield-audits/SPIKE_TOKEN_EFFICIENCY_RESULTS.md`.
1. **Message Batches API** for inspections: supported? default for client runs? how partial batch failures are retried (per request, not per batch)?
2. **Prompt caching**: where `cache_control` breakpoints are placed; whether the cached prefix is byte-identical across calls (system prompt, rule text, shared repo context first; variable content last); measured hit ratio per stage; whether the 1-hour cache TTL is used where batches outlive the 5-minute TTL; whether anything is cached that is used once (cost without benefit).
3. **Request ordering for cache reuse**: are calls that share a prefix sent together (same rule group across many files, or same file across many rule groups, whichever shares the longer prefix)?
4. **Rule routing**: is each file sent only to rules whose domain, stack and file type can apply? Why is the measured reduction only 3%? Can rule groups be merged so each file is sent once per pass instead of 25 to 30 times?
5. **Deterministic pre-filtering**: which rules could be decided or pre-narrowed by tree-sitter, the existing checkers, linters or Semgrep so the model sees candidates (snippets plus metadata) instead of whole files?
6. **Context size**: whole files vs enclosing blocks/AST slices; duplicate files (monorepo copies) sent more than once; generated or vendored content leaking past the filter.
7. **Output tokens** (the largest cost): structured, terse JSON in the scan stage; no prose, headlines or fix text generated for candidates that may be dropped or merged; `max_tokens` set per stage; narrative and fix_specific generated once, after calibration and dedup, only for surviving findings.
8. **Wasted retries**: retry policy for malformed output (fix the schema/prompt rather than retrying blind); cap on retries; logging of tokens spent on failed attempts.
9. **Result caching across runs**: results keyed by (file content hash, rule id and version, prompt version, model) so a re-run, a retest or a Govern month re-uses unchanged results and only sends changed files. This is also what makes the delta re-inspection cheap.
10. **Model tiering**: which stages could use a smaller model without recall loss (routing, classification, dedup suggestions), measured on the fixtures; Sonnet stays the judge. (Zach measured Haiku+Sonnet at about 80% of the findings for the scan stage itself: not acceptable there.)
11. **Extended thinking**: where it is enabled, its budget, and whether those stages need it.
12. **Calibration pass input**: does it send findings plus minimal context, or re-send code?
13. **Pre-flight estimate and budget cap**: token counting before a run, an estimated cost shown before starting, a per-run budget cap that stops loudly when exceeded.
14. **Cost telemetry**: tokens and cost per stage and per 1K counted lines printed every run and written to the export manifest; a regression check that fails when cost per 1K lines rises more than a set percentage on a fixture without an explaining change.

## Part 2: build the gaps
Priority order unless the spike shows otherwise: (a) batch default for client runs; (b) fix cache placement and TTL so writes are re-read, or stop caching single-use content; (c) defer prose and fix generation until after calibration and dedup; (d) result caching by content hash; (e) better rule routing / merged rule groups; (f) deterministic pre-filtering for rules that allow it; (g) telemetry, pre-flight estimate, budget cap and the cost regression check.
Protected (needs Zach's go): anything changing calibration, dedup/merge, severity, the export format, or the rule corpus.
Acceptance: on faktura, tutorly, lumen, hearth and supabase-portal, recall and decoy results unchanged or better; cost per 1K counted lines and total cost reported before and after each change; the results file updated with measured savings.
