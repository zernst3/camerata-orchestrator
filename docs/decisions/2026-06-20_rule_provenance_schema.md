# Rule provenance + verification-status schema

Date: 2026-06-20
Status: Accepted + Implemented (schema only; grounding pass is a follow-up)
Deciders: Zach (architect)

## Problem

The corpus rules in `crates/rules/principles/` were authored by AI. A rule's TOML
says *what* to enforce and *why*, but nothing records whether the rule actually
corresponds to a real, authoritative external standard (a published style guide,
a security standard) or an established linter rule, versus being something the AI
merely designed. Without that provenance we cannot honestly tell a draft,
unverified rule apart from one backed by, e.g., the Google Java Style Guide or
`golangci-lint: errcheck`. This schema is the foundation for a later grounding
pass that will map every rule to its authoritative source.

## The grounding ladder

A new `verification` field on each rule places it on a three-rung ladder
(strictness increasing):

- **`draft`** — AI-generated / designed, NOT yet checked against any external
  authority. There may be no source at all. **Not shippable** — draft rules are
  kept out of the demo'd / armed ruleset.
- **`grounded`** — mapped to a cited authoritative source or a real, established
  linter rule: a URL + identifier is present in `sources`. Machine-grounded; an
  automated grounding pass MAY emit this rung.
- **`verified`** — a human (the maintainer) has confirmed the grounding is
  correct. **Only a human ever sets this.** No automated process may emit
  `verified`. It is the strongest assertion the corpus can make about a rule.

`verified` implies `grounded` (a human confirms an already-grounded rule), so
`is_grounded()` is true for both `grounded` and `verified`.

### Human-only `verified`

The hard rule: automation can ground (find and cite a source), but only a human
can verify (confirm the citation is correct and the mapping is faithful). This
keeps the strongest provenance claim trustworthy. A grounding pass that finds a
plausible source promotes a rule `draft -> grounded`, never to `verified`.

## Sources shape

A new `sources` field is a list of `[[sources]]` array-of-tables blocks:

```toml
verification = "grounded"

[[sources]]
url = "https://google.github.io/styleguide/javaguide.html#s4.8.3.1-for-each"
title = "Google Java Style Guide — Enhanced for statement"
linter = "Checkstyle: FinalLocalVariable"

[[sources]]
url = "https://errcheck.dev"
title = "errcheck — unchecked errors"
# linter omitted: this is a doc-only source no single tool enforces
```

`RuleSource { url, title, linter: Option<String> }`. `linter` carries the
enforcing tool + rule id when the source is a real linter rule (e.g.
`"golangci-lint: errcheck"`, `"Checkstyle: FinalLocalVariable"`); it is `None`
for a style-guide / documentation-only source.

## Backward compatibility

Both fields are additive with serde defaults:

- `verification` defaults (via `#[derive(Default)]` on the enum, variant `Draft`)
  to `draft`.
- `sources` defaults to an empty vec.

Every existing rule TOML (none of which carry the new fields) therefore loads as
`draft` with no sources. The full-corpus load test stays green. No corpus file
was mass-edited; defaults cover them. Round-trip is proven by unit tests in
`crates/rules/src/lib.rs`.

## Accessors

On `camerata_rules::Rule`:

- `verification(&self) -> Verification`
- `is_grounded(&self) -> bool` — `Grounded` or `Verified`
- `is_shippable(&self) -> bool` — true only for `Grounded`/`Verified`; the hook a
  later step uses to keep `draft` rules out of the armed ruleset.

## DTO threading

The two fields are threaded additively through the most direct DTO,
`crates/server/src/onboard.rs::ProposedRule` (`verification: String`, plus a
`sources: Vec<RuleSourceView>` mirror of `RuleSource`), so the UI can later
badge/filter rules by provenance. The corpus->DTO mapping populates them from the
real rule; the hardcoded process/security/AI-discovered `ProposedRule` literals
emit `draft` + empty sources (they are AI-designed and not yet grounded).

Deferred (follow-up): the UI-side `ProposedRuleView`
(`crates/ui/src/cockpit.rs`) deserializes server JSON and silently ignores the
new fields today. Wiring the actual provenance badge/filter columns into the
cockpit table is left to the grounding-pass work, when there is grounded data to
display.

## Files

- `crates/rules/src/lib.rs` — `Verification` enum, `RuleSource` struct, fields on
  `Rule` + `RuleToml`, accessors, tests.
- `crates/server/src/onboard.rs` — `RuleSourceView`, fields on `ProposedRule`,
  mapping.
- `crates/server/src/ai_audit.rs` — AI-discovered `ProposedRule` literal emits
  `draft`.
