# UI verification badges for rule provenance

Date: 2026-06-20
Status: Accepted + Implemented
Deciders: Zach (architect)

## Context

`docs/decisions/2026-06-20_rule_provenance_schema.md` added `verification` and
`sources` fields to the server-side `ProposedRule` DTO and threaded them through
`crates/server/src/onboard.rs`. That schema decision explicitly deferred the UI
wiring:

> "The UI-side `ProposedRuleView` silently ignores the new fields today. Wiring
> the actual provenance badge/filter columns into the cockpit table is left to
> the grounding-pass work, when there is grounded data to display."

This decision records the UI implementation.

## Surfaces modified

Three distinct rule-list surfaces in `crates/ui/src/cockpit.rs` now show
verification state:

| Surface | Table component / function | Column added |
|---------|---------------------------|--------------|
| Onboarding proposed-rules | `ProposedRulesTable` / `rule_columns()` | "Provenance" (BadgeVariantMap) |
| Rules window — corpus (Table 2) | `ProposedRulesTableT2` / `corpus_columns()` | "Provenance" (BadgeVariantMap) |
| Rules window — applied (Table 1) | `ProjectRulesTable` / `applied_rule_columns()` | "Provenance" sourced from corpus join |

Both rule detail modals (`RulesDetailModalHost` and `RuleDetailModal`) now render
a `verif-badge` span next to the rule title inside a new `.rule-modal-title-row`
flex wrapper.

## Badge design

Four states, mapped to visual weight in descending trust:

| State | Label | CSS modifier | Rationale |
|-------|-------|--------------|-----------|
| `verified` | ✓ Verified | `verif-badge-verified` (green) | Prominent checkmark — only a human ever sets this; it is the gold standard and must read as significant. |
| `grounded` | Grounded | `verif-badge-grounded` (blue) | Subtler than `verified`; fully usable (cited source). Hover tooltip surfaces the source title(s). |
| `draft` | Draft | `verif-badge-draft` (gray, italic) | De-emphasized; AI-generated rules that are not yet grounded are not in the armed ruleset. |
| `needs_recheck` | Needs re-check | `verif-badge-needs-recheck` (amber) | Distinct warning color; was verified but source drifted. Does NOT wear the checkmark so it is never confused with `verified`. |

The `verified` checkmark is meaningful: only `verified` rules carry it. `grounded`
uses a different color (blue) and no checkmark so the trust levels remain
distinguishable at a glance.

## Hover tooltip for source citation

For `grounded` and `verified` rules the modal badge `title` attribute is
populated by `verif_sources_tooltip(&r.sources)`, which joins source titles
(with `[linter-id]` suffix when a linter is present) with " · ". This surfaces
the authoritative URL/tool without adding visual noise to the table rows.

In the table columns, the browser-native `BadgeVariantMap` tooltip is not
available (chorale renders plain badge text); the tooltip is available only in
the modal detail view. A future enhancement could add a custom cell renderer.

## Data flow

`ProposedRuleView` in `cockpit.rs` now deserializes both new fields:

```rust
#[serde(default = "default_draft")]
verification: String,   // "draft" | "grounded" | "verified" | "needs_recheck"
#[serde(default)]
sources: Vec<RuleSourceView>,
```

`RuleSourceView` mirrors the server's `RuleSourceView` (`url`, `title`,
`linter: Option<String>`). Both are `#[serde(default)]`-safe: any rule response
that omits the fields deserializes as `draft` with empty sources (backward
compatible with pre-schema corpus responses).

## Custom rules

User-authored custom rules created via `CustomRulesPanel` are emitted via
`CustomRuleView::to_proposed()` with `verification: "verified"`. Rationale: the
architect authored and trusts their own rules; there is no external grounding
ladder concept for user directives.

## Pure helper functions (unit-testable)

Two pure `fn`s in `cockpit.rs` are unit-tested without a DOM:

- `verif_badge(verif: &str) -> (&'static str, &'static str)` — maps a
  verification string to `(badge_label, css_modifier)`. Unknown values fall
  back to `("Draft", "draft")` so future extensions don't panic.
- `verif_sources_tooltip(sources: &[RuleSourceView]) -> String` — joins source
  titles with " · " for the hover tooltip.

Eight tests covering all four canonical values, the unknown-value fallback,
empty sources, single source with/without linter, and multi-source joining.

## CSS (style.rs)

New classes added to `GLOBAL_CSS`:

- `.rule-modal-title-row` — flex wrapper for title + badge in the modal.
- `.verif-badge` — base styles (pill shape, uppercase text, cursor:default).
- `.verif-badge-verified` — green background, dark green text.
- `.verif-badge-grounded` — blue background, dark blue text.
- `.verif-badge-draft` — gray/faint background, italic.
- `.verif-badge-needs-recheck` — amber background, dark amber text.

## Files changed

- `crates/ui/src/cockpit.rs` — `RuleSourceView` struct, new fields on
  `ProposedRuleView`, `verif_badge()`, `verif_sources_tooltip()`, `default_draft()`,
  updated `rule_columns()`, `corpus_columns()`, `applied_rule_columns()`,
  badge in both modals, unit tests.
- `crates/ui/src/style.rs` — `GLOBAL_CSS` additions for badge + modal-title-row.
- `docs/decisions/2026-06-20_ui_verification_badges.md` — this file.
