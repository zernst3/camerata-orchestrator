# A new finding class: real but not client-fixable (2026-10-21)

## Status

PROPOSAL, pending Zach's sign-off on the direction (a new structured axis vs prose). Surfaced by
W24's platform-owned-table case; generalized because it is a whole CLASS, not one rule's quirk.

## The class

A finding that is REAL and correctly detected, but whose fix is **not in the client's own
first-party code**. The client cannot resolve it by editing their repo. Examples:

- **platform-managed**: RLS/policy on a platform-owned table (Supabase `auth.*`, `storage.*`) —
  the platform owns it; the client cannot change it.
- **external-dependency**: a vulnerability in a third-party package. (Note: often FIXABLE by
  upgrading/pinning — so this reason's realistic action differs from platform-managed.)
- **generated**: a defect in generated output — fix the generator/source, not the artifact.
- **vendored**: copied-in third-party code — fix upstream.

"External" is NOT monolithic: platform-managed is truly unfixable (accept/escalate), while an
external dependency is usually fixable by upgrade. The field must carry the REASON, not a bare
boolean, because the realistic action differs per reason.

## Three constraints that pull against each other

1. **Never drop it** — it is a real observation; silently hiding it is the dishonesty the
   "never silently drop" discipline forbids. It informs risk acceptance, vendor choice, escalation.
2. **Never mis-rank it** — ranking an unfixable finding beside an actionable critical spends the
   client's alarm and remediation budget on something they cannot act on
   (`hide_dead_end_affordances`, `permission_gap_is_not_a_bug`).
3. **Never lie about its severity** — it may be genuinely high-risk. Re-route it by OWNERSHIP;
   do NOT fake-cap its severity. This supersedes W24's original "platform-owned-table severity
   anchor" idea, which distorted severity to compensate for not having this axis.

## Decision: a STRUCTURED field, not prose

Prose-only cannot route a PDF section, cannot be excluded from the remediation budget, cannot be
filtered in the machine exports, cannot be gated, and drifts (the reason `kind`/`disposition`/
`not_assessed` are all structured, and why W28 field-agreement exists). So: structure it.

### The model — a new orthogonal axis `ownership`

Mirrors the existing `not_assessed` structured-reason pattern (a structured reason that routes a
finding out of the action tiers to its own destination). NOT a new `kind` (an unfixable finding
is still a defect — orthogonal) and NOT merely a `disposition` (disposition is the auditor's
triage call; ownership is a PROPERTY of where the finding lives, known to the detector).

- `ownership`: `client-owned` (default — the current behavior for every finding today) |
  `platform-managed` | `external-dependency` | `generated` | `vendored`.
- A required **realistic client action** string per non-client-owned finding ("Accept the risk or
  escalate to Supabase; you cannot change RLS on `auth.*`." / "Upgrade `<pkg>` when a patched
  version ships; none is available today.").
- **Set by the detector** (the RLS checker knows the schema; the dependency checker knows the
  package), flowing through `Finding` to findings.json / xlsx / csv / manifest.

### Presentation
- Its own PDF section, **"Findings outside your direct control"**, separate from do-now/do-next/plan.
- **Excluded from the remediation budget/estimate** (do not price a fix the client cannot make).
- True severity preserved and shown (honest).
- A machine field in every export (filterable), so a client's tooling can separate "my work" from
  "not my work."

### Gate
A non-`client-owned` finding MUST carry a reason AND a realistic action — never a bare "you can't
fix this." (Same discipline as W26's "no high+ row without a fix": here it is "no unfixable row
without a reason + a client action.")

## Open sub-decisions for Zach
1. **Axis name**: `ownership` vs `actionability` vs `locus`. (Recommend `ownership`.)
2. **Does `external-dependency` even belong here**, given it is usually fixable by upgrade? Option:
   keep it client-owned (an upgrade IS a fix) and reserve this axis for the truly-unfixable
   (platform-managed / generated / vendored-without-upstream-access). Leaning: include it but with
   the "upgrade when available" action, because a dependency with NO patched version is genuinely
   not-yet-fixable and belongs in the same section.
3. **Build scope**: the axis + the RLS platform-schema detector (the W24 instance) first, then
   extend to the dependency/generated reasons; or all reasons at once.

## Build outline (on sign-off)
1. `Finding.ownership: Ownership` + `Finding.unfixable_action: Option<String>` (or fold the action
   into the existing calibration/rationale surface).
2. The detector sets it — start with the RLS checker tagging `auth.*`/`storage.*` tables
   `platform-managed`.
3. Export routing: PDF section, budget exclusion, the field in findings.json/xlsx/csv/manifest.
4. The gate (reason + action required).
5. Tests: positive (a platform-managed finding routes to the section, keeps severity, leaves the
   budget), safe-twin (a public-schema RLS finding stays client-owned and actionable).
