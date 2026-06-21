# Decision: Split "wire rules into CI" onboarding story by enforcement tier

**Date:** 2026-06-21
**Status:** Implemented (fix/ci-story-split)

## Context

Camerata's brownfield onboarding previously offered a single "Create CI-rules story"
button per repo that filed one GitHub issue covering all CI-tier rules regardless of
enforcement tier. CI-tier covers two distinct enforcement levels:

- **mechanical** — each rule maps 1:1 to an off-the-shelf linter or analyzer (ESLint
  rule, Semgrep pattern, migration audit script, etc.). Wiring these rules into CI is
  straightforward: enable the cited tool in CI. No bespoke checker needed. A team can
  pick this story up and implement it the same day.

- **architectural** — each rule is deterministic (a PR either violates it or it
  does not) but there is NO off-the-shelf linter. Each needs a bespoke AST transform,
  custom Semgrep rule, or static-analysis pass the team must design, implement, and test
  before wiring into CI. This work requires team refinement and scoping before
  implementation begins and should NOT be bundled with the mechanical story.

Bundling both tiers into a single story caused two problems:

1. The mechanical story (easy, parallelizable) was blocked or deprioritized alongside
   the harder architectural work.
2. Developers picking up the story had no signal that some items were off-the-shelf
   and some needed a design phase.

## Decision

Split the "wire CI rules" affordance into two separate GitHub issues, one per tier,
filed by two separate buttons. The split is driven by the rule's `enforcement` field
("mechanical" | "architectural").

Both issue bodies open with the same preamble that explains the distinction:

> Mechanical and architectural rules are both deterministic CI-tier checks.
> Mechanical rules map to an existing off-the-shelf linter (simple to wire).
> Architectural rules require a custom checker and team refinement before implementing.

The mechanical issue names the linter hint (from the rule's first source with a linter
field) so the developer can go straight to enabling it. The architectural issue flags
explicitly that each checker must be designed by the team and that the story should be
scoped/refined before implementation.

## Implementation

**Server (`crates/server/src/lib.rs`):**

- `CiRulesReq` gains two new fields: `tier: String` ("mechanical" | "architectural")
  and `rules: Vec<CiStoryRule>` (id + title + optional linter).
- `CiStoryRule` is a new struct.
- `onboard_ci_rules` is rewritten to build a tier-scoped story title and body.
  If `rules` is empty it returns `ok: false` with a clear message.

**UI (`crates/ui/src/cockpit.rs`):**

- `CiRuleItem` is a new struct (id, title, enforcement, linter) for carrying rules
  between call sites and the component.
- `first_linter(rule)` — extracts the first non-empty linter hint from a rule's sources.
- `ci_rule_items_from_proposed(rules)` — filters a `Vec<ProposedRuleView>` to CI-tier
  items. Used at the two onboarding call sites where `ScanReportView.proposed_rules`
  is available.
- `ci_rule_items_from_selections(selections, corpus)` — joins `RuleSelectionView`s
  (rule_id only) with the corpus to recover enforcement and title. Used at the
  project Rules panel call site.
- `wire_ci_rules_tier(repo, tier, rules)` — replaces the old `wire_ci_rules`. Posts
  `/api/onboard/ci-rules` with `{repo, tier, rules}` and returns `Result<url, msg>`.
- `CiRulesPanel` gains a `rules: Vec<CiRuleItem>` prop. It splits rules into
  mechanical and architectural lists and renders two buttons per repo (each button
  shown only when its tier has at least one rule).
- All three call sites updated to pass `rules`.

Story titles:
- Mechanical: `Wire mechanical (off-the-shelf linter) rules into CI — {owner/repo}`
- Architectural: `Wire architectural (custom-checker) rules into CI — {owner/repo}`

## Consequences

- The mechanical track is now independently schedulable; a team can wire it in a
  single sprint without waiting for the architectural design phase.
- The architectural story arrives with clear expectations: this is design work first,
  not a configuration task.
- Both stories share the same preamble so any reader landing on either issue has
  full context for the tier they did NOT get.
- `structured` and `prose` rules are never included in CI stories (they are PR-reviewed,
  not CI-enforced) — this was already implicit behavior and is now explicit in the
  filtering helpers.
