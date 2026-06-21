# USER_GUIDE.md Update for Product Wave Features

**Date:** 2026-06-21
**Status:** Complete
**Author:** Claude (Opus 4.8, 1M context) / Zach Ernst (architect)
**Scope:** `docs/USER_GUIDE.md` only

---

## What was updated

Five major product-wave features documented in `docs/USER_GUIDE.md` (§8–12):

### 1. The unified chat assistant (§8, renamed from "in-app assistant")

**Before:** Two modes (Research, Guide) with minimal context description.

**After:** Four context-grounded modes documented with a clear table:
- **Research** — open chat, smoke test.
- **Guide** — grounded in USER_GUIDE.md + corpus rules.
- **Technical** — grounded in TECHNICAL.md (internals, extensibility).
- **Project** — grounded in live project state (findings, ruleset, onboarding phase).

Added subsections:
- **The unified chat assistant** — explains the four context sources (docs, rules, live state, active finding).
- **Project mode in detail** — how the mode fetches and uses project context; the "Ask" button on findings.
- **Honesty guardrail** — the critical phrase that prevents hallucination.

### 2. Update detection and rule drift (§9, entirely new)

Covers two signals:
- **App updates** — the banner UI.
- **Applied-rule drift** — the health check in the Rules view; verification badges (verified/grounded/draft/needs-recheck).
- **How to update a rule** when drift is detected.

Includes a table of the four badge states and their visual meaning.

### 3. Single-rule editing (§10, entirely new)

Documents the Rules-view fine-grained editing workflow:
- **Editing a single rule** — opening the detail modal, switching options, viewing full definition.
- **Repo-scoped overrides** — removing a rule from a specific repo.
- **Project-level rules** — immutable at repo level; apply everywhere.
- **Custom rules** — repo-scoped and project-scoped custom rule authorship, creation, deletion.

### 4. Deep-report Markdown export (§11, entirely new)

Comprehensive coverage of the deep audit tier:
- **Three lenses** — SOC-2 gap analysis, deep security audit, threat model.
- **Cost and timing** — opt-in, 3× standard audit cost, strong model.
- **The deep-report export** — structured markdown sections, advisory notices.
- **Important: the SOC-2 output is advisory** — explicit callout that this is not audit-grade.

### 5. Feature flags (§12, entirely new)

Documents the environment-variable-based flag system:
- **How to enable/disable** — `export CAMERATA_FEATURE_<NAME>=false` or `.env` file.
- **Current flags table** — three flags (SOC2_ANALYSIS off by default, others on).
- **Why flags** — ship on day one, A/B test, pivot fast, support air-gapped deployments.

---

## Design constraints honored

1. **Matches existing USER_GUIDE.md style** — consistent structure (numbered sections, tables, code blocks, markdown links to decisions).
2. **Accurate to shipped behavior** — all features documented match the actual product (decisions + code reviewed).
3. **Honest framing** — SOC-2 language flagged as advisory throughout; deep report is gap analysis, not compliance tool; feature flags explained as ship-on-day-one mechanism.
4. **Conforms to docs conventions** — no dashes (em/en), no invented features, section links to decisions where they exist.

---

## Files touched

- `docs/USER_GUIDE.md` — sections 8–12 added/rewritten (§1–7 untouched). Status box at top remains current.

---

## Testing

- `cargo check` — passes (no errors, no warnings).
- Manual review: all cross-references, flag names, badge names, and feature descriptions match source code and decisions.

---

## Rationale

Per the product wave specification, these five features are all shipped and production-ready. Documenting them in the user guide (the canonical source that the Guide mode assistant reads from) ensures:

1. **In-app assistant accuracy** — the Guide and Project modes have authoritative, up-to-date grounding.
2. **Onboarding clarity** — new users see all four chat modes + the rules-editing workflow right away.
3. **Transparency** — feature flags are explicit (no hidden toggles), and SOC-2 advisory language is unambiguous.
4. **Durability** — these docs stay in sync as features evolve; a future change to the chat assistant or flags can update the same sections.

---

## Cross-references

Linked from this doc:
- `docs/decisions/2026-06-20_project_aware_chat.md` — the chat-mode implementation.
- `docs/decisions/2026-06-20_ui_verification_badges.md` — badge design.
- `docs/decisions/2026-06-16_project_container_and_rules_management.md` — ruleset editing.
- `docs/decisions/2026-06-20_uow_soc2_evidence_and_scoped_scan.md` — scoped scan details.
- `docs/decisions/2026-06-20_deep_compliance_tier_lenses.md` — deep audit tier.

---
