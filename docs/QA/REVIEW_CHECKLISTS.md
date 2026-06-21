# Review Checklists

Two checklists for human review, derived from `docs/rule-grounding/meta.md` and the
citation-validation report.

---

## Checklist A: META SELF-SOURCE — Rules needing maintainer confirmation

Source: `docs/rule-grounding/meta.md` §§ "NEEDS MAINTAINER SELF-SOURCE CONFIRMATION" and
"LOOKS NON-INTERNAL". These rules were promoted to `grounded` but the originating internal
document could not be pinned. The maintainer must either point at the source or reclassify.

### A1. NEEDS MAINTAINER SELF-SOURCE CONFIRMATION

For each item: either locate the originating decision/commit and add a `[[sources]]` block
pointing at it, OR confirm the rule is best grounded against an external authority (and add
that `[[sources]]` block instead).

#### api-layer/ (15 rules — strong internal signal but no exact doc)

- [ ] **`ARCH-API-DTOS-1`** — DTOs vs domain types at the boundary. Maintainer's API layering
  practice matches, but no ADR pins this. Point at the decision or ground against PoEAA / REST
  spec.
- [ ] **`ARCH-API-VERSIONING-1`** — URL-prefix versioning. Referenced in API design context but
  no specific decision doc located.
- [ ] **`ARCH-BOUNDARY-VALIDATION-1`** — Validate at the boundary. Matches "parse don't validate"
  stance but no internal doc. Consider grounding against Alexis King's "Parse, don't validate"
  (2019).
- [ ] **`ARCH-CURSOR-PAGINATION-1`** — Cursor over offset. Likely referenced in REST-phase
  decision (memory) but no exact doc located.
- [ ] **`ARCH-EXACT-DECIMALS-1`** — Exact decimal types for money. Referenced in
  `docs/decisions/2026-06-16_enforcement_tiers_gate_vs_ci.md` as a known rule but no originating
  ADR.
- [ ] **`ARCH-EXPLICIT-TX-1`** — Services own transaction lifecycle. Matches `RUST-DOMAIN-7` in
  CONVENTIONS.md but that is the Rust port, not the API layer generically. Add a cross-reference
  or write a language-agnostic ADR.
- [ ] **`ARCH-HOT-READ-CACHE-1`** — Hot reads behind TTL cache. Matches `SERVICE-CACHE-1` in
  memory context but no API-layer ADR located.
- [ ] **`ARCH-IDEMPOTENCY-KEYS-1`** — Idempotency keys for side-effecting writes. Strong
  best-practice; no internal decision doc located. Ground against Stripe's idempotency-key pattern
  or RFC 9110 §9.2.2 if no internal origin.
- [ ] **`ARCH-MIDDLEWARE-FIRST-1`** — Cross-cutting concerns as middleware. No internal doc.
- [ ] **`ARCH-REPO-PER-AGGREGATE-1`** — One repository per aggregate. Matches `RUST-DOMAIN-1` +
  `REPO-1` in CONVENTIONS.md for the Rust port; no language-agnostic ADR.
- [ ] **`ARCH-REPO-RETURNS-DOMAIN-1`** — Repositories return domain types. Matches CONVENTIONS.md
  SeaORM pattern but no API-layer ADR.
- [ ] **`ARCH-SERVICE-DI-1`** — DI at composition root. Matches maintainer's style but no
  internal decision doc.
- [ ] **`ARCH-STRUCTURED-ERRORS-1`** — Structured error envelopes. Matches `RUST-DOMAIN-4/6` in
  CONVENTIONS.md but no API-layer ADR.
- [ ] **`ARCH-TYPED-PATH-PARAMS-1`** — Typed extraction of path/query params. Matches maintainer's
  practice but no internal doc.
- [ ] **`ARCH-UTC-TIMESTAMPS-1`** — UTC domain timestamps. Matches CONVENTIONS.md for the Rust
  port; no cross-stack ADR.

#### ui/ (4 rules)

- [ ] **`UI-CONSENT-GATED-1`** — Consent-gated client-side storage. No internal decision doc
  located. Ground against GDPR Article 25 / W3C Storage API spec if no internal origin.
- [ ] **`UI-IMAGE-COMPONENT-1`** — Single image component. Referenced in `docs/TECH_DESIGN.md`
  and `docs/PHASE0_TASKS.md` as an active rule but no originating ADR.
- [ ] **`UI-QUERY-LIBRARY-1`** — Query library for all UI fetches. Matches `SERVICE-CACHE-1` /
  `UI-CACHE-1` in memory context but no API-layer ADR.
- [ ] **`UI-UTC-DATES-1`** — Project-level UTC date helper. Referenced in `docs/TECH_DESIGN.md`
  as an active rule but no originating ADR.

#### ci-cd/ (1 rule)

- [ ] **`PROC-FEATURE-FLAGS-1`** — Feature flags vs long-lived branches. Consistent with
  maintainer's trunk-based stance but no specific internal decision doc located.

---

### A2. LOOKS NON-INTERNAL (review: cut, or ground externally)

The meta.md grounding pass did not conclusively identify any rule as AI-inserted generic without
internal signal in the target scope. Every rule reviewed had at least some internal analog in the
maintainer's practice. The items in A1 above are the candidates MOST LIKELY to need external
grounding rather than self-sourcing, because they represent widely-established patterns whose
canonical sources are well-known external standards.

**Action for each A1 item:** decide which applies —
1. **Internal origin found** — add `[[sources]]` block pointing at the internal doc;
   keep `verification = "grounded"` (maintainer sign-off still needed).
2. **External authority is the true source** — replace the internal-signal `[[sources]]` with
   an external URL (PoEAA, OWASP, RFC, etc.); keep `verification = "grounded"`.
3. **Neither** — no defensible source; demote to `draft` or cut the rule.

---

## Checklist B: UNGROUNDED rules (verification = draft / no verification field)

These 36 rules have no `verification` field (the schema default is `draft`), meaning they were
never promoted from the initial draft state. They are candidates to ground-later or cut.

Grouped by family. For each: decide (a) ground it (find/confirm a source, add `[[sources]]`
block, add `verification = "grounded"`), or (b) cut it (delete the TOML file if the rule is
redundant or too generic to be actionable).

### api-layer/ (15 draft rules)

These overlap with the A1 self-source list — they are both ungrounded AND missing a confirmed
source. Priority: decide self-source vs. external grounding first (Checklist A), then add the
`verification` field.

- [ ] `ARCH-API-DTOS-1` — mechanical enforcement; high blast-radius if applied without grounding.
- [ ] `ARCH-API-VERSIONING-1`
- [ ] `ARCH-BOUNDARY-VALIDATION-1`
- [ ] `ARCH-CURSOR-PAGINATION-1`
- [ ] `ARCH-EXACT-DECIMALS-1` — mechanical enforcement; high blast-radius.
- [ ] `ARCH-EXPLICIT-TX-1`
- [ ] `ARCH-HOT-READ-CACHE-1`
- [ ] `ARCH-IDEMPOTENCY-KEYS-1`
- [ ] `ARCH-MIDDLEWARE-FIRST-1`
- [ ] `ARCH-REPO-PER-AGGREGATE-1`
- [ ] `ARCH-REPO-RETURNS-DOMAIN-1`
- [ ] `ARCH-SERVICE-DI-1`
- [ ] `ARCH-STRUCTURED-ERRORS-1` — mechanical enforcement; high blast-radius.
- [ ] `ARCH-TYPED-PATH-PARAMS-1`
- [ ] `ARCH-UTC-TIMESTAMPS-1`

### ci-cd/ (1 draft rule)

- [ ] `PROC-FEATURE-FLAGS-1`

### go/ (5 draft rules)

- [ ] `GO-HANDLER-SERVICE-REPOSITORY-1` — likely redundant with Java/Python analogues; confirm
  or merge.
- [ ] `GO-WEB-MIDDLEWARE-CROSS-CUTTING-1`
- [ ] `GO-WEB-REQUEST-BINDING-VALIDATION-1`
- [ ] `GO-WEB-STRUCTURED-ERROR-RESPONSES-1`
- [ ] `GO-WEB-THIN-HANDLERS-DELEGATION-1`

### javascript/ (1 draft rule)

- [ ] `JAVASCRIPT-ANGULAR-SMART-PRESENTATIONAL-PATTERN-1` — pattern exists in angular.dev style
  guide; easy to ground externally.

### rust/ (9 draft rules)

These are Rust-port-specific rules from the maintainer's CONVENTIONS.md / memory; they likely
have an internal source that was not linked. Check CONVENTIONS.md and the Rust port ADRs first.

- [ ] `RUST-DIOXUS-1` — Dioxus file structure. Check `docs/decisions/2026-06-09_dioxus_ui_phase_decisions.md`.
- [ ] `RUST-DIOXUS-10` — Auth `_can` flags in Dioxus. Check same ADR (decision Z10).
- [ ] `RUST-DIOXUS-12` — SVG icons inline. Check same ADR.
- [ ] `RUST-DIOXUS-13` — Forms newtype validation. Check same ADR.
- [ ] `RUST-DIOXUS-14` — Primitives first. Check same ADR.
- [ ] `RUST-DOMAIN-7` — Explicit unit-of-work. Check `docs/decisions/2026-05-31_rust_port_unit_of_work_pattern.md`
  (RUST-DOMAIN-7 is named there).
- [ ] `RUST-HEADLESS-CORE-1` — Headless core + adapter crates. Check Rust port architecture ADRs.
- [ ] `RUST-MAPPER-1` — Mappers in own crate. Check `docs/decisions/2026-05-26_rust_port_domain_phase_architecture.md`
  (MAPPER-1 named there).
- [ ] `RUST-PURE-STATE-TRANSITIONS-1` — Pure state transitions. Check same domain-phase ADR.

### sql/ (1 draft rule)

- [ ] `SQL-AUDIT-COLUMNS-1` — Audit columns (`created_at`, `updated_at`, etc.). Referenced in
  CI-hygiene ADR (`docs/decisions/2026-05-09_ci_hygiene_additions.md`); likely groundable there.

### ui/ (4 draft rules)

- [ ] `UI-CONSENT-GATED-1`
- [ ] `UI-IMAGE-COMPONENT-1` — Note: citation-validation shows ESLint citations resolve; just add
  `verification = "grounded"` once source is confirmed.
- [ ] `UI-QUERY-LIBRARY-1`
- [ ] `UI-UTC-DATES-1` — Note: citation-validation shows ESLint citations resolve; just add
  `verification = "grounded"` once source is confirmed.

---

## Quick-action summary

| Priority | Action | Count |
|---|---|---|
| 1 | Mechanical draft rules — ground or cut immediately (applied as CI gates) | 4 (`ARCH-API-DTOS-1`, `ARCH-EXACT-DECIMALS-1`, `ARCH-STRUCTURED-ERRORS-1`, plus `UI-IMAGE-COMPONENT-1` and `UI-UTC-DATES-1` which resolve in citation-validation) |
| 2 | Self-source A1 items — locate internal originating doc | 20 |
| 3 | Ground easy Rust draft rules against existing ADRs | 9 |
| 4 | Ground or cut remaining draft rules | 12 |

---

*Sources: `docs/rule-grounding/meta.md`, `docs/rule-grounding/citation-validation.md`,
`crates/rules/principles/**/*.toml`.*
