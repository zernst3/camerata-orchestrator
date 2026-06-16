# The rule corpus is the moat — eval it as a separate axis

Status: accepted (2026-06-16) as a principle + plan; eval harness staged.

## Context

"Detect frameworks → suggest rules" is only as good as the rule library the suggestions
come from. Generic or wrong rules = a noisy audit = negative value: a user who turns
Camerata on and sees junk findings never turns it on again. So the per-language /
framework rule corpus is the actual moat, and its quality is a SEPARATE thing to measure
from the scan/fix engine — the engine can be flawless and the audit still be garbage if
the rules are weak.

## Decision: treat corpus quality as its own eval axis

The scan/fix engine and the corpus are tested independently:

- **Engine** (already): deterministic audit arms, the AI-audit JSON parsing, suppression
  classification, fix verification — pure, unit-tested.
- **Corpus** (this axis): the quality of the rules + proposals themselves, measured on a
  labelled set of repos, not by the engine's unit tests.

What the corpus eval measures, per rule / per stack:
- **Precision** — of the findings a rule produces on a labelled repo, what fraction are
  real (not false positives). Low precision is the noise that kills adoption.
- **Recall** — of the known violations of a kind, what fraction the corpus catches.
- **Proposal relevance** — for a detected stack, are the suggested rules the ones a
  competent lead would pick (not generic filler).
- **Directive quality** — is each rule's adopted directive specific and actionable, or
  vague boilerplate.

## Two tiers, two quality strategies

- **Deterministic corpus** (mechanical arms + the proposed-rules library): precision is
  the priority; a false-positive mechanical rule is pure noise. Curate conservatively;
  measure on labelled repos.
- **AI architectural audit** (`ai_audit.rs`): partly de-risks the corpus problem because
  it reasons about the specific code rather than relying only on a fixed library — but it
  trades determinism for judgment, so its findings need the same precision scrutiny (the
  adversarial-verify pattern: a second pass that tries to REFUTE each finding before it's
  shown) and the require-evidence prompt already pushes it to cite real code.

## Status / next

Principle codified; the harness is staged work: a labelled fixture set (repos with known
violations), a runner that scores precision/recall per rule + stack, and a regression
gate so corpus edits can't silently lower quality. Until then, corpus changes are
reviewed by hand and the AI audit carries the architectural tier.
