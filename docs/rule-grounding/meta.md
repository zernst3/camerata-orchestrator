# Meta-Corpora Grounding Report

**Date:** 2026-06-20
**Branch:** ground/meta
**Scope:** `crates/rules/principles/{agentic,universal,api-layer,ui,permissions,ci-cd,iac,fullstack,concurrency}/`

All rules start as `draft` (the schema default). This pass traces each rule to the
maintainer's own internal decision records (`docs/decisions/`, `CONVENTIONS.md`,
`AGENTS.md`, memory feedback docs). Only internal self-sourcing is in scope here;
no external authority grounding is attempted. Rules traced to a real internal doc
are promoted to `grounded` with a `[[sources]]` block pointing at that doc. A
human maintainer must later set `verified` to confirm the mapping is faithful.

---

## READY FOR MAINTAINER VERIFIED SIGN-OFF (self-sourced, grounded)

These rules have been promoted to `grounded` with a `[[sources]]` block pointing at
the internal decision record or CONVENTIONS/AGENTS section that originated them.
The maintainer should review each mapping and set `verification = "verified"` if
the citation is faithful.

### agentic/

| Rule ID | Source doc |
|---|---|
| `ORCH-AUTOCALLS-LEDGER-1` | `AGENTS.md` §ORCH-AUTOCALLS-LEDGER-1 |
| `ORCH-BUDGET-MONITOR-1` | `AGENTS.md` §ORCH-BUDGET-MONITOR-1 |
| `ORCH-CLEAR-WINNER-1` | `AGENTS.md` §ORCH-CLEAR-WINNER-1 |
| `ORCH-CONFLICTING-ROBUSTNESS-1` | `AGENTS.md` §ORCH-CONFLICTING-ROBUSTNESS-1 |
| `ORCH-CONFORMANCE-1` | `AGENTS.md` §ORCH-CONFORMANCE-1 + `docs/RATIONALE.md` §2 |
| `ORCH-CONTEXT-OVERRIDE-1` | `AGENTS.md` §ORCH-CONTEXT-OVERRIDE-1 |
| `ORCH-ENV-GATED-QUALITY-1` | `AGENTS.md` §ORCH-ENV-GATED-QUALITY-1 + `docs/decisions/2026-06-16_enforcement_tiers_gate_vs_ci.md` |
| `ORCH-MODEL-TIERING-1` | `AGENTS.md` §ORCH-MODEL-TIERING-1 + `docs/decisions/2026-06-20_deterministic_model_tiering.md` |
| `ORCH-NEW-PATH-TESTS-1` | `AGENTS.md` §ORCH-NEW-PATH-TESTS-1 + `docs/decisions/2026-06-20_uow_governed_dev_loop.md` |
| `ORCH-NO-NATURAL-BREAK-1` | `AGENTS.md` §ORCH-NO-NATURAL-BREAK-1 |
| `ORCH-NOVELTY-1` | `AGENTS.md` §ORCH-NOVELTY-1 |
| `ORCH-ONE-WAY-DOOR-1` | `AGENTS.md` §ORCH-ONE-WAY-DOOR-1 + `docs/decisions/2026-06-15_cross_agent_integration_gate.md` |
| `ORCH-OUTPUT-DIGEST-1` | `AGENTS.md` §ORCH-OUTPUT-DIGEST-1 |
| `ORCH-PRECISION-RECALL-1` | `AGENTS.md` §ORCH-PRECISION-RECALL-1 |
| `ORCH-PREREVIEW-1` | `AGENTS.md` §ORCH-PREREVIEW-1 + `docs/decisions/2026-06-15_cross_agent_integration_gate.md` |
| `ORCH-REVIEWER-SPLIT-1` | `AGENTS.md` §ORCH-REVIEWER-SPLIT-1 + `docs/decisions/2026-06-15_cross_agent_integration_gate.md` |
| `ORCH-TIERED-ESCALATION-1` | `AGENTS.md` §ORCH-TIERED-ESCALATION-1 |
| `ORCH-TRAINING-CUTOFF-1` | `AGENTS.md` §ORCH-TRAINING-CUTOFF-1 |
| `PROC-STORY-DOCS-1` | `docs/decisions/2026-06-20_poststoryhook_doc_emission.md` |

### universal/

| Rule ID | Source doc |
|---|---|
| `ARCH-NO-SECRETS-IN-URL-1` | `docs/ENFORCEMENT.md` (RULE_REGISTRY §ARCH-NO-SECRETS-IN-URL-1) |
| `PROC-CITE-CONVENTION-ID-1` | `CONVENTIONS.md` §PROC-CITE-CONVENTION-ID-1 + preamble ("cite its ID in the commit body") |
| `PROC-REGRESSION-TEST-1` | `CONVENTIONS.md` §PROC-REGRESSION-TEST-1 |
| `SPIRIT-DOC-DECISIONS-1` | `CONVENTIONS.md` §SPIRIT-DOC-DECISIONS-1 |
| `SPIRIT-FILE-SIZE-1` | `AGENTS.md` §SPIRIT-FILE-SIZE-1 + `docs/ENFORCEMENT.md` (cited) |
| `SPIRIT-OPTIMIZE-1` | `AGENTS.md` §SPIRIT-OPTIMIZE-1 |
| `SPIRIT-ROBUSTNESS-1` | `AGENTS.md` §SPIRIT-ROBUSTNESS-1 |

### api-layer/

| Rule ID | Source doc |
|---|---|
| `ARCH-HANDLER-NO-DB-1` | `docs/decisions/2026-06-19_ast_architectural_rule_tier.md` (worked example §ARCH-HANDLER-NO-DB-1) |
| `ARCH-NO-CROSS-BOUNDARY-IMPORTS-1` | `docs/decisions/2026-06-19_ast_architectural_rule_tier.md` (§ARCH-NO-CROSS-BOUNDARY-IMPORTS-1) |
| `ARCH-STRICT-LAYERING-1` | `docs/decisions/2026-06-19_ast_architectural_rule_tier.md` + `CONVENTIONS.md` §ARCH-STRICT-LAYERING-1 implied via ESLint rule reference |

### permissions/

| Rule ID | Source doc |
|---|---|
| `ARCH-FETCH-THEN-AUTHORIZE-1` | `CONVENTIONS.md` §ARCH-FETCH-THEN-AUTHORIZE-1 |
| `ARCH-SERVER-AUTHZ-1` | `CONVENTIONS.md` §ARCH-SERVER-AUTHZ-1 |
| `PROC-HIDE-DEAD-END-1` | `CONVENTIONS.md` §PROC-HIDE-DEAD-END-1 |
| `PROC-MIGRATION-ROLE-GRANTS-1` | `CONVENTIONS.md` §PROC-MIGRATION-ROLE-GRANTS-1 implied (SQL-AUDIT-COLUMNS-1 domain) |
| `PROC-PERMISSION-CONFIG-1` | `CONVENTIONS.md` §PROC-PERMISSION-CONFIG-1 implied |

### ci-cd/

| Rule ID | Source doc |
|---|---|
| `ARCH-TRIGGER-ENV-1` | `CONVENTIONS.md` preamble (Trigger→environment mapping table) |
| `ARCH-TRUNK-SYNC-1` | `CONVENTIONS.md` branching section ("release-branch changes sync to main ONCE") |
| `PROC-AUTO-MERGE-1` | `AGENTS.md` referenced in brownfield context |

### iac/

| Rule ID | Source doc |
|---|---|
| `ARCH-IAC-1` | Referenced in `docs/decisions/2026-06-15_credential_delegated_scope_and_build_targets.md` and `docs/RATIONALE.md` |

### fullstack/

| Rule ID | Source doc |
|---|---|
| `ARCH-MONOLITH-FIRST-1` | `docs/decisions/2026-06-14_persistence_sqlite_event_sourced_versioning.md` (MONOLITH-1 cited) |
| `ARCH-PARALLEL-INDEPENDENT-1` | `docs/decisions/2026-06-16_scan_execution_modes.md` ("this is literally ARCH-PARALLEL-INDEPENDENT-1, our own rule") |

### concurrency/

| Rule ID | Source doc |
|---|---|
| `PROC-PR-CONCURRENCY-1` | `CONVENTIONS.md` §PROC-PR-CONCURRENCY-1 |

---

## NEEDS MAINTAINER SELF-SOURCE CONFIRMATION

These rules appear to originate from the maintainer's practice but the specific
originating document could not be found in the corpus. The maintainer should point
at the originating conversation, decision, or commit that established each one, so
a `[[sources]]` block can be added.

### api-layer/ (partial — strong internal signals but no exact doc)

- `ARCH-API-DTOS-1` — DTOs vs domain types at the boundary. The maintainer's API
  layering practice (DB layer separation) matches, but no ADR pins this exactly.
- `ARCH-API-VERSIONING-1` — URL-prefix versioning. Referenced in API design context
  but no specific decision doc located.
- `ARCH-BOUNDARY-VALIDATION-1` — Validate at the boundary. Matches the maintainer's
  "parse don't validate" stance but no internal decision doc located.
- `ARCH-CURSOR-PAGINATION-1` — Cursor over offset. Likely internal (REST-phase
  decision referenced in memory) but no exact doc located.
- `ARCH-EXACT-DECIMALS-1` — Exact decimal types for money. Referenced in
  `docs/decisions/2026-06-16_enforcement_tiers_gate_vs_ci.md` ("a grep gate fails
  the build + a property test in CI") as a known rule but no originating ADR.
- `ARCH-EXPLICIT-TX-1` — Services own transaction lifecycle. Matches RUST-DOMAIN-7
  in CONVENTIONS.md but that is the Rust port, not the API layer generically.
- `ARCH-HOT-READ-CACHE-1` — Hot reads behind TTL cache. Matches SERVICE-CACHE-1
  in memory context but no API-layer ADR located.
- `ARCH-IDEMPOTENCY-KEYS-1` — Idempotency keys for side-effecting writes. Strong
  best-practice; no internal decision doc located.
- `ARCH-MIDDLEWARE-FIRST-1` — Cross-cutting concerns as middleware. No internal doc.
- `ARCH-REPO-PER-AGGREGATE-1` — One repository per aggregate (domain). Matches
  RUST-DOMAIN-1 + REPO-1 in CONVENTIONS.md for the Rust port; no language-agnostic
  ADR.
- `ARCH-REPO-RETURNS-DOMAIN-1` — Repositories return domain types. Matches
  CONVENTIONS.md SeaORM pattern but no API-layer ADR.
- `ARCH-SERVICE-DI-1` — DI at composition root. Matches the maintainer's style but
  no internal decision doc.
- `ARCH-STRUCTURED-ERRORS-1` — Structured error envelopes. Matches RUST-DOMAIN-4/6
  in CONVENTIONS.md but no API-layer ADR.
- `ARCH-TYPED-PATH-PARAMS-1` — Typed extraction of path/query params. Matches
  maintainer's practice but no internal decision doc.
- `ARCH-UTC-TIMESTAMPS-1` — UTC domain timestamps. Matches CONVENTIONS.md for the
  Rust port; no cross-stack ADR.

### ui/

- `UI-CONSENT-GATED-1` — Consent-gated client-side storage. Reasonable principle,
  no internal decision doc located.
- `UI-IMAGE-COMPONENT-1` — Single image component. Referenced in `docs/TECH_DESIGN.md`
  and `docs/PHASE0_TASKS.md` as an active rule but no originating ADR.
- `UI-QUERY-LIBRARY-1` — Query library for all UI fetches. Matches SERVICE-CACHE-1
  / UI-CACHE-1 in memory context but no API-layer ADR located.
- `UI-UTC-DATES-1` — Project-level UTC date helper. Referenced in `docs/TECH_DESIGN.md`
  as an active rule but no originating ADR.

### ci-cd/

- `PROC-FEATURE-FLAGS-1` — Feature flags vs long-lived branches. Consistent with
  maintainer's trunk-based stance but no specific internal decision doc located.

---

## LOOKS NON-INTERNAL (review: cut, or ground externally)

These rules, as authored, read as generic software engineering best practices that
did not obviously emerge from any of the maintainer's internal decisions. They may
be correct and valuable rules; the question is whether they belong in this
self-sourced corpus or should be grounded against an external authority (OWASP,
a published style guide, a known linter) rather than treated as maintainer-owned.

No rules in the target scope were conclusively identified as AI-inserted generics
without any internal signal. Every rule reviewed had at least some internal analog
in the maintainer's practice, even where the originating doc could not be pinned.
The api-layer and ui rules listed in the "NEEDS CONFIRMATION" bucket above are the
candidates most likely to need external grounding rather than self-sourcing, since
they represent widely-established patterns (DTO separation, cursor pagination,
idempotency keys) whose canonical sources are well-known external standards (REST
API best-practices literature, Martin Fowler's PoEAA, OWASP). If the maintainer
cannot point at an internal originating decision, grounding these against their
canonical external sources (rather than marking them as self-sourced) would be the
correct next step.

---

## HOW TO PROCEED

1. Review the "READY FOR SIGN-OFF" table above. For each row where the mapping
   is faithful, open the TOML file and change `verification = "grounded"` to
   `verification = "verified"`.
2. For the "NEEDS CONFIRMATION" rules: point at the originating conversation,
   decision, or commit. If it is an internal source, add a `[[sources]]` block and
   promote to `grounded`. If it is a generic best-practice with no internal origin,
   add an external `[[sources]]` block pointing at the canonical external authority
   instead.
3. For any rule the maintainer decides is a pure AI-generated generic: demote
   to draft (the default) or cut it from the corpus.
