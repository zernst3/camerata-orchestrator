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

## Dependency findings — the fixability split (folded in 2026-10-21)

**We already run a dependabot-class scan.** `crates/server/src/dep_audit.rs` runs
**`osv-scanner`** (Google's OSV: npm, cargo, pip, go, maven, …) over the repo's lockfiles,
emits a `DEP-AUDIT-1` defect per known-vulnerable package, and routes them into the inspection
output already — the Dependencies sheet, `dependency_snapshot`, the `dependency_advisories`
reconciliation count, and findings.json. So the SCAN exists and is wired; it is not unique to us
(OSV is the shared public DB behind Dependabot too), but it is correctly part of the product.

**The gap (and the hook for this class):** `parse_osv_json` does NOT extract the **fixed/patched
version** from OSV's `affected[].ranges[].events[].fixed`. So today a dep finding says "pkg@ver
has CVE-X" but not "upgrade to >= Y." Extracting the fixed version is the enhancement that makes
the dependency finding ACTIONABLE and, crucially, lets `ownership` be **data-driven**:

- **A patched version exists** (OSV reports a `fixed`) → the CVE IS fixable → `ownership =
  client-owned`, action = **"upgrade `<pkg>` to >= `<fixed>`"**. This is a normal actionable
  finding, in the action tiers and the remediation budget.
- **No patched version exists** (no `fixed` event for a range covering the installed version) →
  genuinely not-yet-fixable → `ownership = external-dependency` (THIS class), action = **"no
  patch is available yet; monitor the advisory, accept the risk, or replace the dependency."**
  Routed to the "outside your direct control" section, excluded from the budget, true severity
  kept.

So "external dependency" is not one bucket — it is split deterministically by whether OSV carries
a fix. That resolves open sub-decision #2 below: include dependency findings in this class ONLY
when no fix is available; a fixable CVE stays client-owned with an upgrade action.

**Merely-outdated (no CVE)** — packages behind latest with no advisory — is a DIFFERENT scan
(`cargo-outdated`/`npm outdated`) and is deliberately OUT of scope for the inspection: it is
noise (everything is eventually "a minor behind"), it is a maintenance-dashboard concern rather
than a security/quality inspection finding, and flagging it would spend client attention on
non-findings. (A future, low-priority option: flag only EOL or severely-stale MAJOR versions,
never every minor. Noted, not planned.)

### Dependency-findings build (added to the build outline)
- Extend `parse_osv_json` to pull the smallest `fixed` version >= the installed version from the
  matching `affected` range; carry it on the finding.
- Derive `ownership` from it: `fixed` present → client-owned + "upgrade to >= `<fixed>`";
  absent → `external-dependency` + "no patch available".
- Tests: positive (a CVE WITH a fix → client-owned, upgrade action, in the budget), and the
  twin (a CVE with NO fix → external-dependency, in the "outside your control" section, out of
  the budget). This is the dependency arm of the class's positive/safe-twin pair.

## Open sub-decisions for Zach
1. **Axis name**: `ownership` vs `actionability` vs `locus`. (Recommend `ownership`.)
2. ~~Does `external-dependency` belong here?~~ **RESOLVED (2026-10-21):** split it by OSV fix
   data — a CVE WITH a patched version is `client-owned` (action: upgrade); a CVE with NO patch
   is `external-dependency` (this class). See the "Dependency findings" section above.
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

## Implementation status (2026-10-21) — W33

**BUILT and tested (green):**
- The axis: `camerata_api_types::audit::Ownership` (+ `tag()`, `is_client_fixable()`).
- Pure derivation: `camerata_server::ownership::ownership_of(rule_id, captures)` +
  `ownership_action(ownership)` — one source of truth, 6 unit tests.
- **Dependency arm (the live payoff):** `dep_audit::parse_osv_json` extracts OSV's smallest
  `fixed` version into `captures[osv_fixed_version]`, enriches the detail, sets an actionable
  `fix_specific` ("Upgrade <pkg> to >= X") when patched. `ownership_of` splits client-owned
  (patch exists) from external-dependency (none). Surfaced in the Dependencies sheet
  ("Fixed in" + "Recommended action" columns) and dependencies.json/.csv.

**KEY FINDING — the W24 motivating case was already handled.** The RLS checker's `is_platform_owned`
(`crates/checks/src/supabase/rls_checker.rs`) already gates severity so it NEVER fires a false
"RLS disabled" finding on a platform-owned table (`auth.*`/`storage.*`) the repo didn't create —
their baseline is platform-managed-enabled. So the platform-owned-table case is a NON-issue by
non-emission (the correct behavior — don't surface a non-problem), not a real finding needing
routing. The genuinely-live "real but not fixable" case is therefore the **no-patch dependency
CVE**, which is built.

**Dormant-but-supported:** the `PlatformManaged`/`Generated`/`Vendored` branches of
`ownership_of` + `ownership_action` are correct, tested infrastructure, ready if a genuine
non-dependency platform-managed REAL finding ever needs surfacing (a detector would set
`captures[platform_schema]`). No such finding arises today, so no dedicated PDF "outside your
control" section is populated yet — building an empty section now would be speculative. Revisit
if/when such a finding class appears.

**Budget exclusion:** dependency findings already live in their own lane (the Dependencies sheet /
`dependency_snapshot`), outside the curated action tiers and the remediation budget — so the
"route unfixable findings out of the budget" intent is already satisfied for the live case.
