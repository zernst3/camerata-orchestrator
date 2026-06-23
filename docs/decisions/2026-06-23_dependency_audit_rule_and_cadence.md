# Decision: Dependency-audit rule (CICD-DEPENDENCY-AUDIT-1) and cadence-as-decision

**Date:** 2026-06-23
**Status:** Implemented (`feat/dep-audit-rule`)
**Files changed:**
- `crates/rules/principles/ci-cd/cicd-dependency-audit-1.toml` (new)
- `crates/server/src/lib.rs` (enriched mechanical CI-integration story + 10 new tests)

---

## Context

Ongoing dependency-vulnerability scanning was previously unaddressed as a selectable
Camerata rule. The always-on onboarding floor (separate agent, separate files) handles
the base security posture; this rule is the opt-in complement: teams that want explicit
dependency scanning choose it deliberately.

The key architectural tension: when and how often should the scan run? That is a cadence
decision, and Camerata does NOT build a scheduling engine. The cadence belongs to the
project, implemented by the developer in CI.

---

## Decisions

### 1. opt-in, never auto-recommended

`CICD-DEPENDENCY-AUDIT-1` sets `default = false` and `opt_in_only = true`. It must
never be pre-checked in the onboarding proposal, even though it is `grounded` against a
real tool (osv-scanner). Selecting it requires a conscious choice, including a cadence
decision. Pattern matches CICD-SEMGREP-SECURITY-SCAN-1 and CICD-CODEQL-SECURITY-SCAN-1.

### 2. Cadence is a project decision, not a Camerata engine

Camerata does not build a scheduling engine or cron runner. The cadence (where + how
often the scan runs) is:

1. A **decision** the architect makes on the rule (one of four options in the `[decision]`
   block).
2. Carried into the **CI-integration story body** as concrete implementation guidance.
3. **Implemented by the developer** when wiring CI.

Same pattern as all other architectural rules: Camerata ships the rule + the story HOW-TO;
the developer wires it.

### 3. Tool: osv-scanner (Apache-2.0, Google)

osv-scanner is the tool of record for this rule. It queries the OSV database (the
authoritative open-source vulnerability database), reads lock files without a full build,
and exits non-zero on any finding. It is fast enough to run locally (not layer3-only) and
in CI. Apache-2.0 license, no per-seat cost.

### 4. Four cadence options

| Option id | Description | Recommended? |
|-----------|-------------|-------------|
| `dep-audit-weekly-ci` | Scheduled weekly CI job (cron) | YES — recommended default |
| `dep-audit-per-pr` | Run in CI on every pull_request / push | High-friction alternative |
| `dep-audit-every-pass` | `in_loop = true`: in-loop dev gate AND CI | Highest coverage |
| `dep-audit-manual` | `workflow_dispatch` only — on-demand | Early-stage projects |

Weekly is recommended because it catches newly-disclosed CVEs affecting already-merged
dependencies (the class per-PR scans miss entirely) with low per-PR friction.

### 5. Version-pinned osv-scanner in `.camerata/checks.toml`

Every cadence option's directive includes a `[[check]]` manifest entry with `tool`,
`version`, and `install` fields. The version placeholder `<x.y.z>` must be filled with
an exact pinned version. This closes the L2/L3 drift gap (SSOT decision 2026-06-22).

The `in_loop` flag differs by cadence:
- Weekly, per-PR, manual: `in_loop = false` (CI-only)
- Every-pass: `in_loop = true` (runs at Layer 2 in-loop dev gate too)

### 6. CI-integration story enrichment

When `CICD-DEPENDENCY-AUDIT-1` is among the armed rules in the mechanical CI-integration
story, `ci_story_body_mechanical` now injects a "Dependency vulnerability scanning —
cadence" section after the manifest examples. The section:

- States explicitly that Camerata does not build a scheduling engine.
- Lists all four cadence options with GitHub Actions YAML snippets.
- Shows the osv-scanner invocation (`osv-scanner -r .`).
- Tells the developer to set `in_loop` to match the chosen cadence.
- Includes a per-rule implementation checklist.

When `CICD-DEPENDENCY-AUDIT-1` is NOT among the rules, the section is absent — no
spurious cadence noise for unrelated mechanical rules.

---

## Test coverage added

10 new unit tests in `crates/server/src/lib.rs::tests`:

| Test | What it asserts |
|------|-----------------|
| `dep_audit_armed_mechanical_body_contains_cadence_section` | "cadence" appears when rule is armed |
| `dep_audit_armed_mechanical_body_mentions_osv_scanner_command` | `osv-scanner -r .` is present |
| `dep_audit_armed_mechanical_body_lists_all_four_cadence_options` | All four cadence options described |
| `dep_audit_armed_mechanical_body_states_developer_implements_cadence` | "developer" / "project decision" present |
| `dep_audit_armed_mechanical_body_references_checks_toml_for_version_pin` | `.camerata/checks.toml` + osv-scanner named |
| `dep_audit_absent_mechanical_body_has_no_cadence_section` | Section absent when rule not armed |
| `dep_audit_mixed_rules_cadence_section_present` | Section present when dep-audit is one of several rules |
| `dep_audit_cadence_section_recommends_weekly_as_default` | "recommended" or "standard" present |
| `dep_audit_cadence_section_explains_cron_schedule_example` | `cron:` YAML present |
| `dep_audit_cadence_section_mentions_camerata_does_not_schedule` | "scheduling engine" / "does not build" present |

Total server test count after this change: 670 (up from 648).

---

## Consequences

- Teams can explicitly opt in to dependency scanning as a Camerata-governed rule.
- The cadence decision is documented at the rule level and carried into the story body,
  so the developer who picks up the CI story has concrete implementation guidance without
  additional hand-holding.
- Camerata's architecture is unchanged: no scheduling engine added, no cron runner, no
  cadence state. The developer owns the cadence; Camerata owns the rule and the story.
- The rule loads cleanly via the existing corpus parser (verified: `cargo test -p camerata-rules`).
