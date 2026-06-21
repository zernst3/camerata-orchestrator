# Option Why Rewrite: csharp Corpus

**Date:** 2026-06-21
**Branch:** why/csharp
**Scope:** `crates/rules/principles/csharp/`

## What changed

Rewrote 4 vacuous `[[option]].why` placeholder values in the csharp rule corpus. Each placeholder contained
"A defensible alternative the project considered." with no actual trade-off reasoning, which left the option
record content-free for curators and reviewers.

## Files touched

All 4 rewrites are in `crates/rules/principles/csharp/aspnetcore/`:

| File | Option id | Rewrite summary |
|------|-----------|-----------------|
| `csharp-aspnetcore-options-pattern-1.toml` | `raw-iconfiguration-injection` | Explains the simplicity appeal of direct IConfiguration injection and its cost: untyped reads, invisible constructor requirements, silent null on missing keys at runtime instead of startup. |
| `csharp-aspnetcore-middleware-ordering-1.toml` | `ad-hoc-middleware-order` | Explains that ad-hoc ordering feels flexible but produces correctness accidents: a single out-of-order addition silently enables auth bypass or broken CORS preflight. |
| `csharp-aspnetcore-thin-controllers-1.toml` | `fat-controllers-inline-logic` | Explains the low-ceremony appeal of inlining logic and its costs: HTTP-only reachability, inability to unit-test without framework infrastructure, and single-responsibility violation. |
| `csharp-aspnetcore-minimal-api-vs-controllers-1.toml` | `mixed-minimal-and-controller` | Explains the flexibility appeal of mixing styles and its cost: two programming models coexist for identical work, doubling the mental model surface with no added capability. |

## Tests

`cargo test -p camerata-rules`: 54 passed, 0 failed.

## How each rewrite was derived

Each new `why` was pulled directly from the rule's `[decision].why` paragraph, which already discussed
the option's trade-off in full. The rewrite extracts the option-specific reasoning, adds 1-2 sentences
on the appeal of choosing this option, and writes it at the option level as a concrete trade-off
statement (what adopting it means, what it costs, when it might be right).

No facts were invented; all content is grounded in the existing decision rationale or implied by the
option's own directive.
