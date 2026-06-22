# Repo-ruleset table: add the 4-modality "Type" column + clarify column language

**Date:** 2026-06-21

## Context

Every rule table in the cockpit gained a 4-modality **Type** column
(prose/structured/mechanical/architectural) with a hover tooltip — except the
repo-ruleset Rules-view table built by `rule_columns()`, which still carried an
older **"Kind"** column whose values were `Mechanical`/`Review` (a coarse
CI-vs-human axis derived from `is_ci_enforced()`, server `onboard.rs:822`). The
word "Mechanical" appearing in "Kind" collided conceptually with the four-modality
Type, and the table lacked the real modality column the others had.

## Decision

In `crates/ui/src/cockpit.rs` `rule_columns()`:

- **Added** a `ColumnId("enf_type"), "Type"` column sourced from `r.enforcement`,
  rendered with `enforcement_badges()`, with a per-cell `title` tooltip wired via
  chorale's `row_cell_renderers` + `enforcement_tooltip()` — identical to the
  pattern in `applied_rule_columns()` / `corpus_columns()`. Placed right after the
  Rule column.
- **Relabeled** the old "Kind" column to **"Enforced by"**, and its badge values
  from `Mechanical`/`Review` to **`Automated (CI)`** / **`Human review`** (the
  underlying `r.kind` values are unchanged; only the displayed labels). This stops
  the "Mechanical" collision and makes the lane axis self-explanatory.
- **Relabeled** the "Gate placement" column header to **"Where enforced"** (value
  unchanged — still the descriptive placement string).

The three columns now read as a clear progression: **Type** (what kind of
conformance check) → **Enforced by** (automated vs human) → **Where enforced**
(the placement).

No docs referenced the old "Kind"/"Gate placement" headers, so no doc change was
needed. `cargo check -p camerata-ui` is green.
