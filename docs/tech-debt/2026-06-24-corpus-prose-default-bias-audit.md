# Tech debt: corpus-wide prose/default-bias audit (neutralize implied defaults)

> **Status: STAGED — not yet filed as a GitHub issue.**
> On the next "GitHub push", create this as a **sub-issue of the Tech Debt Epic (#70)** using the title + body below.

**Title:** Re-scan the rule corpus for default + prose bias (unwarranted defaults; implied defaults in wording)

---

## Problem

The rule corpus encodes the author's own past architectural preferences as defaults — sometimes explicitly (`default = true`), sometimes only in the *prose*. A decision rule is supposed to present options at equal weight and let the team choose; many rules silently tilt the choice. `RUST-DIOXUS-9` was the first one caught and rebalanced by hand (server functions ↔ REST API); this issue tracks doing it **systematically across the whole corpus**.

## Two checks to run on every rule

**Check 1 — Rules that shouldn't have a default but do.**
Rules with `default = true` / `[decision].default = "..."` set on a decision that is a genuine, context-dependent choice where no option should be privileged. These should drop the adopted default (`default = false`, remove `[decision].default`).

- **Worked example — `ARCH-MONOLITH-FIRST-1`** (`crates/rules/principles/fullstack/arch-monolith-first-1.toml`): ships `default = true` + `[decision].default = "monolith-first"`. Monolith-vs-services is a textbook context-dependent call (team size, deploy boundaries, scaling shape) — it should not be pre-adopted.

**Check 2 — Rules that claim no default but the wording implies one.**
Even with `default = false`, the prose tilts the choice. The pattern (observed across multiple rules):
- the historically-preferred option gets a **long, thorough `directive` + a thorough, positively-framed `why`**;
- the alternatives get **brief directives** and **`why`s that are mostly negatives** — framed as reasons *against* relative to the favored option, not as the option's own genuine trade-off.

`ARCH-MONOLITH-FIRST-1` exhibits this too (thorough monolith-first arm; terse, negatively-framed alternatives), so it fails both checks.

## Fix (per rule, mirror the RUST-DIOXUS-9 rebalance)

1. Drop unwarranted defaults (`default = false`; remove `[decision].default`).
2. Neutralize the **title** and `[decision].why` so they describe the choice, not advocate one side.
3. Rebalance each `[[option]]`: comparable thoroughness across directives; each `why` states that option's **own** legitimate trade-off (when it's the right call), not just negatives relative to the favored one.
4. Keep option **ids** stable (baseline-compatible); reword labels/directives/whys only.

## Audit approach

- Mechanizable starting signal: flag rules where one option's `directive`+`why` length is disproportionately larger than its siblings, and/or alternative `why`s are dominated by negative framing — that surfaces the likely-biased rules for human rebalancing.
- Then a human (or governed pass) rebalances each flagged rule. This is prose/judgment work, so it routes through review, not auto-apply.

## Scope

Whole `crates/rules/principles/` corpus. Precedent already merged: `RUST-DIOXUS-9` (commit on `chore/rust-dioxus-9-rebalance`, now in local main). Parent: **Tech Debt Epic #70**.
