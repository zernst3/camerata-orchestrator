# Decision: Enrich CI-gate story bodies with self-sufficient SSOT HOW-TO

**Date:** 2026-06-23
**Status:** Implemented (`feat/ci-gate-story-howto`)
**Files changed:** `crates/server/src/lib.rs`

---

## Context

After the tier-split decision (2026-06-21) and the check-manifest SSOT decision
(2026-06-22), Camerata's onboarding flow emits two GitHub issues — one for
mechanical rules, one for architectural rules — when the architect clicks the
"Wire CI rules" buttons. The issue bodies were thin: they named the rules and
gave a one-line note about the implementation approach. This was not enough.

A developer or AI agent picking up the story still needed to know:

1. That `.camerata/checks.toml` is the single source of truth (SSOT) and that
   editing it is the ONLY step that wires a check into BOTH Layer 2 and Layer 3.
2. That tool versions must be pinned (`tool` + `version` + `install` fields) or
   the SSOT breaks: L2 and L3 can run different tool versions and disagree.
3. That `.camerata/checks.toml` is gate-protected by `SEC-NO-CAMERATA-CONFIG-1`
   and an agent cannot edit it; the manifest edit is always a human/operator commit.
4. For architectural rules specifically: there is no off-the-shelf linter, so a
   bespoke checker must be designed and scoped before implementation. The
   dependency-cruiser worked example for API layering is the canonical reference.

Without this content in the story, a developer follows the old "wire into CI
separately" mental model and bypasses the manifest entirely, which re-introduces
L2/L3 drift.

---

## Decision

Enrich both tier story bodies to be self-sufficient at the SSOT level. The story
is the implementation guide; it must carry enough context that the developer or
agent implements it correctly with no additional hand-holding.

---

## What the enriched bodies teach

### Shared preamble (both tiers)

Extracted into `ci_story_ssot_preamble()`:

- `.camerata/checks.toml` is the SSOT. One entry there enforces the check at
  BOTH Layer 2 (in-loop dev gate) and Layer 3 (generated CI workflow).
- Parity is structural, not by convention: both the Layer-2 runner and the
  Layer-3 workflow generator call the same shared functions over the manifest.
- Full `[[check]]` schema with annotated field reference table, including the
  three optional pinning fields (`tool`, `version`, `install`).
- Why pinning is required: without it, L2 and L3 can run different linter
  versions on the same ruleset and produce different results.
- Gate protection: `SEC-NO-CAMERATA-CONFIG-1` blocks agent writes to `.camerata/`.
  The manifest edit is always a human/operator commit.
- How to regenerate the CI workflow after editing the manifest
  (`POST /api/projects/active/generate-ci-workflow`).

### Mechanical tier additions

Built by `ci_story_body_mechanical()`:

- Names the off-the-shelf linter for each rule (from the `linter` hint in
  `CiStoryRule`).
- Shows a per-rule annotated `[[check]]` entry template with placeholder pinned
  version, so the implementation is: fill in the real version, commit, done.
- Emphasises that no separate L2 vs L3 wiring step exists; the manifest is it.

### Architectural tier additions

Built by `ci_story_body_architectural()`:

- Explicit: there is NO off-the-shelf linter. Each rule needs a bespoke checker
  designed by the team (script, custom Semgrep rule, AST pass,
  dependency-cruiser config, etc.).
- Four-step HOW-TO:
  1. Design the deterministic checker (options enumerated; exit-code contract stated).
  2. Add the manifest entry with pinned `tool` + `version` + `install`.
  3. Regenerate the CI workflow.
  4. Verify at both Layer 2 and Layer 3.
- Worked example for API layering (`ARCH-API-LAYERING-1`): a
  `dependency-cruiser` config that asserts the service-to-repository import
  boundary; the exact `depcruise` invocation is shown.
- Local-script variant: manifest entry without `tool`/`version`/`install` for
  repo-owned scripts that need no external binary.
- Scoping guidance: scope each rule as its own sub-task; do not block the
  mechanical story on this work.

---

## Implementation

Two helper functions extracted from `onboard_ci_rules` into standalone fns:

- `ci_story_ssot_preamble() -> &'static str` — shared preamble (static str;
  no per-request fields).
- `ci_story_body_mechanical(repo, rules) -> String` — full mechanical body.
- `ci_story_body_architectural(repo, rules) -> String` — full architectural body.

`onboard_ci_rules` delegates to these helpers; the match arms are now trivial.

---

## Test coverage added

26 new unit tests in `crates/server/src/lib.rs::tests`. They assert structural
landmarks in the body text without byte-exact string matching, so minor prose
edits do not break them:

| Test group | What it asserts |
|------------|-----------------|
| SSOT file reference | `.camerata/checks.toml` appears in both tier bodies |
| Both-layers mention | `Layer 2` and `Layer 3` appear in both tier bodies |
| Parity guarantee | "Parity is structural" or "parity" appears in both bodies |
| Schema fields | All 5 required fields + 3 pinning fields appear in both bodies |
| Exact version pinning | "exact" or "pin" appears in both bodies |
| Gate protection | `SEC-NO-CAMERATA-CONFIG-1` or "agents cannot" appears in both bodies |
| Linter hint | Linter name (e.g. "eslint") appears in mechanical body for rules that carry one |
| Rule IDs | All rule ids from the fixture appear in the respective body |
| Custom checker | "bespoke checker" or "custom checker" appears in the architectural body |
| Worked example | `dependency-cruiser` and `ARCH-API-LAYERING-1` appear in architectural body |
| Workflow regen | `generate-ci-workflow` appears in architectural body |
| Scoping guidance | "mechanical CI story" / "mechanical story" appears in architectural body |
| Repo name | Repo name propagated into both bodies |

Total server test count after this change: 648 (up from 622).

---

## Consequences

- Any developer or AI agent picking up either story has the complete SSOT
  implementation model in the issue body, with no extra hand-holding required.
- The architectural story is now unambiguous: it teaches the four-step
  design-then-register flow with a concrete worked example.
- The helper functions are unit-testable without GitHub tokens or HTTP servers.
- The story bodies are longer (~3-4x) but thoroughness is the stated requirement:
  correctness of implementation is more important than issue brevity.
