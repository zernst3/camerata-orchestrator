# ADR: Deep discovery scan (bug + gap) as an onboarding option, and re-onboarding (re-scan)

**Date:** 2026-07-04
**Status:** Accepted (design); implementation to follow
**Motivated by:** the 2026-07-04 Fable 5 audit (`docs/ARCH_AUDIT_2026-07-04_fable5-complete.md`)

## Context

On 2026-07-04 we pointed Fable 5 at Camerata's own repo and had it read the code to find latent bugs
and functionality gaps. It surfaced 68 defects + 10 gaps that the existing onboarding audit does not
find. That is not an accident of model strength: the existing onboarding runs two tiers, and neither
does what Fable did.

- **Tier 1, mechanical scan:** deterministic pattern matching (secrets, raw SQL, path escapes,
  clippy/ruff/semgrep/osv). Finds only pre-codified patterns.
- **Tier 2, AI architectural audit:** an LLM checks the code for CONFORMANCE to the project's SELECTED
  rules (missing auth on a write path, a service bypassing the repo layer, N+1). It answers "does the
  code violate a rule we already wrote?"

What Fable did is a **third, categorically different thing**: open-ended discovery. It abstracted the
defect class from the code (a silent-failure pattern, a stale-snapshot reactivity bug, a state-machine
transition that advances on failure, a dead client/server contract) and reasoned about whole-subsystem
behavior and about the repo's stated INTENT (README/vision) to find gaps. There is no pre-written rule
to match: the finding IS the discovery.

This is Camerata's own thesis ("**L3 discovers, L2 enforces**", `2026-07-02_l3-completeness-check.md`)
applied at repo scope. Camerata exists to hold OTHER repos to a standard; the very act we just
performed (a strong model reading a repo to find flaws and gaps) should therefore be a first-class
onboarding capability, not a thing we do by hand once.

## Decision

### 1. Add an opt-in "Deep Discovery Scan" onboarding mode (third tier)

A new, opt-in scan mode alongside the mechanical and rule-based tiers. It performs open-ended discovery
of two things:
- **Latent correctness bugs** the rule-based audit cannot name in advance: silent failures, dropped
  async results / races, client/server contract mismatches, state-machine violations, dead affordances,
  stale-snapshot reactivity, non-idempotent side effects.
- **Functionality gaps vs the repo's own stated intent**: what the README / docs / vision say the
  system should do vs what the code actually does (missing capability, half-built stub, a promised
  surface with no implementation).

**Name (decided 2026-07-05): "Bug and Gap Discovery Scan".** (Chosen over "Deep Discovery Scan" to avoid
colliding with the existing "deep-tier" SOC-2 deep-report export.)

### 2. Engine: bounded, read-only, subsystem-partitioned reasoning agents

The scan MUST use the pattern that worked on 2026-07-04 and MUST NOT repeat the failure that preceded it:
- **Partition the repo into subsystems** (by crate / top-level dir / language boundary) and spawn ONE
  read-only reasoning agent per subsystem. Bounded, predictable agent count = number of subsystems.
- **No recursive sub-agent spawning.** (The first Fable pass let general-purpose agents spawn their own
  sub-agents, turning 3 into 20+ and exhausting the budget in minutes. The successful pass used agents
  that structurally could not spawn.) The scan's workers must be read-only and non-spawning by
  construction.
- **Ground each agent** in (a) the repo's intent docs (README/vision/docs) so gap analysis works, and
  (b) the relevant code. Gap analysis is worthless without the intent baseline.
- Each agent returns **ranked findings** with a fixed schema: title, location (file:line), what's wrong,
  why it's real (repro / broken invariant / impact), severity, confidence, suggested fix.
- A synthesis step **dedups + records cross-confirmations** (a finding found by two agents is
  higher-trust) and produces one ranked report.

Reuse Camerata's existing fleet + model-tiering + grounding plumbing (`fleet/orchestrator.rs`,
`grounding.rs`) rather than a bespoke path.

### 3. Model tiering: suggest the strongest available, opt-in with a cost estimate

Discovery depth scales with model capability, so the scan **suggests the highest tier available** (the
Strongest band, or an explicit "Deepest available" that maps to Fable when present). Because that is
expensive, it is **opt-in**: the user selects it in the onboarding scan-mode selector, sees a cost +
time estimate first (like the existing deep-tier), and can decline. Default off.

### 4. Output disposition: reuse the triage pipeline

Findings flow into the SAME disposition pipeline onboarding already has: the three triage tables
(Unresolved / Ignored / Tech debt), then Process turns dispositions into durable artifacts (baseline
waivers + GitHub issues). A discovered bug becomes a tracked story, carrying its severity/confidence.
Optionally also emit a markdown report (like deep-report). The scan NEVER auto-fixes (consistent with
onboarding's "emit stories, never do the development work" principle).

### 5. Re-onboarding (re-scan): a point-in-time re-run on an existing project

A **"Re-scan" / "Re-onboard"** action on an already-onboarded project: re-run any subset of the
onboarding scan tiers (mechanical / rule-based / deep discovery) with the same options, WITHOUT redoing
project setup (repos, ruleset, credentials are already configured). It is a point-in-time snapshot of
the current code. Called "re-onboarding" for simplicity even though it is really a re-scan.
- Stored as a **timestamped snapshot per project**.
- **Future enhancement:** diff a re-scan against the prior snapshot ("what's new / fixed / regressed
  since last scan") — high value, but v1 can ship snapshot-only.

### 6. Findings presentation: category-first consolidation (added 2026-07-05)

Three scan tiers (mechanical, rule-based, deep discovery) will produce HUNDREDS of rows. Today the grid
groups by domain/rule inside collapsible drop-downs; with three tiers that is still an unnavigable wall.
Add ONE abstraction higher: a **finding CATEGORY** as the top-level grouping.

**The taxonomy (fixed + closed, so categorization is repeatable):**
- **Security** — vulnerabilities, secrets, auth/authz, injection, path escape, TLS, unsafe deserialization.
- **Architecture** — layering/boundary violations, coupling, DI, contract drift, module structure.
- **Correctness (Bugs)** — logic errors, races, silent failures, contract mismatches, state-machine
  violations, dead affordances.
- **Functionality Gaps** — missing or half-built capabilities vs the repo's stated intent.
- **Performance** — N+1, hot-path allocations, missing indexes, needless work.
- **Compliance** — SOC-2 / audit gaps, licensing, evidence.
- **Maintainability / Debt** — dead code, duplication, stale TODOs, complexity (optional; may fold into
  Architecture).
- **Other** — catch-all; should stay near-empty (a large Other means the taxonomy needs a new category).

**The critical rule: category is INTRINSIC, decoupled from source tier.** A finding is grouped by WHAT it
is, not by which scan found it. A security flaw surfaced by the tier-3 deep scan is a **Security** row,
sitting with the other Security findings, even though the dedicated security scan missed it. Each finding
therefore carries two orthogonal fields:
- `category` — the intrinsic type; the TOP-LEVEL grouping key (assigned by the consolidator).
- `source` — which tier/lens produced it (mechanical / rule-audit / deep-discovery + lens); a visible
  badge + a filter, NEVER the grouping key.

**The grid:** Category (new level 0: collapsible, shows count + max severity + a source-mix badge) → the
EXISTING breakdown (domain / repo / rule drop-downs, unchanged) underneath → individual findings.
Provenance stays visible via the per-row Source badge, and Source becomes a FILTER, so you can still slice
by tier when you want without it dictating the grouping.

**The consolidator (the make-or-break component: it must categorize + dedup correctly).** A governed step
that runs AFTER all tiers emit raw findings:
1. NORMALIZE every finding to one shape (category, source, severity, confidence, repo, file:line, title,
   detail, rule_id?).
2. CATEGORIZE into the fixed taxonomy by intrinsic nature. Deterministic where the source implies it (a
   mechanical secret-scan hit is Security by construction); LLM judgment only for ambiguous ones, against
   WRITTEN category definitions so it is repeatable.
3. DEDUP across tiers: the same issue found by two tiers is ONE row listing both sources. Merge ONLY
   same-issue-same-spot (same file + overlapping line + same semantic defect). NEVER merge across
   different lines or on a "same issue?" guess: a wrong merge HIDES a real finding (the worst failure);
   when unsure, keep separate and flag `[needs review]`.
4. CROSS-CONFIRM: a finding independently surfaced by 2+ tiers/lenses gets a confidence boost + a
   "confirmed" badge (high-trust, like the audit's cross-agent confirmations).
5. RANK within each category by severity x confidence.

**Consolidator reliability:** it is itself a governed agent — the read-only governance kernel, the fixed
taxonomy with definitions, and a STRICT output contract (one object per finding: category, source(s),
severity, confidence, file:line, dedup-group-id, cross_confirmed_by). Dedup uses normalized file+line + a
conservative similarity threshold biased toward keeping separate. Mis-categorization or wrong merges
destroy trust, so this component gets the strongest model and its own eval set.

## Consequences

- Onboarding gains a third, opt-in tier that is reasoning-heavy and cost-gated; the two existing tiers
  are unchanged and remain the default.
- Camerata can now hold user repos to the same discovery standard it just held itself to.
- The discovery→enforcement loop closes: recurring discovered classes can later be promoted to L2 rules
  (the same "L3 discovers, L2 enforces" escalation, now sourced from user-repo scans).
- Re-onboarding makes the audit repeatable and trend-able.

## Honest limits

- Discovery is **probabilistic, not proof** (a blind reasoning pass); it complements, never replaces,
  the deterministic mechanical tier and the rule-based audit.
- It is **expensive** (strongest-model, per-subsystem fan-out); opt-in + cost-estimate are load-bearing,
  not optional polish.
- Quality depends on the intent docs: a repo with no README/vision yields weaker gap analysis (the scan
  should say so rather than invent gaps).
- v1 is snapshot-only; the re-scan DIFF is deferred.

### 7. Scheduled onboarding-scan routine template (all tiers selectable) + per-iteration slice (added 2026-07-05)

A routine TEMPLATE (Camerata's scheduled-routine feature) that re-runs the ENTIRE onboarding scan flow on
a cadence, exposing the SAME selectable options as interactive onboarding, plus a new per-iteration SLICE
scope. It is re-onboarding (§5) on a schedule, feeding the same consolidator (§6) + triage pipeline. It is
a template: sensible defaults the user overrides however they like.

- **Runs the full onboarding flow each iteration**, with the identical onboarding selector: which TIERS
  run (mechanical / rule-based audit / Bug-and-Gap discovery, any subset), the DISPOSITION (triage-first),
  and the MODEL tier. Default template = all three tiers, triage-first, strongest model for discovery.
- **NEW: per-iteration SLICE scope.** Instead of the whole codebase, a run can scan a configurable SLICE:
  a path / glob / subsystem / language-layer selector (e.g. "API controllers only", "crates/server",
  "src/auth/**"). The slice can be **fixed** (same slice every run, e.g. security-triage the API layer
  nightly) or **rotating** (advance through slices across iterations to cover the whole codebase over time
  at lower per-run cost). Full-codebase-every-run stays the default/recommended; the slice is the
  cost/cadence lever.
- **Fully customizable combinations**, e.g.: "full onboarding every iteration", "security triage only",
  "discovery scan of the API-controllers slice per iteration", or any tier x disposition x model x slice.
- **Guardrailed against snowballing:** bounded agent count (= subsystems in scope, no recursion), a per-run
  cost cap + estimate, and a cadence floor, so even a full-repo run cannot run away.
- Output each run: consolidator → category-grouped findings (§6) → triage (never auto-fix).

Reconciling with the "full deep scan every run" principle (§decision 3): that is the DEFAULT and the rule
for a given run's SCOPE (within a run, do not auto-skip already-scanned code, since new references change
its behavior). The slice option is a DELIBERATE user choice to narrow the per-iteration scope for
cost/cadence; within the chosen slice, the scan is still full and deep.

## Resolved decisions (2026-07-05)

1. **Name:** "Bug and Gap Discovery Scan" (see §1).
2. **Disposition:** triage-first (findings land in the triage tables; the user promotes real ones to
   issues). No auto-filing.
3. **Scan scope:** FULL deep scan of all repos every run, never incremental/skip-already-scanned (see §7).
   The snapshot DIFF stays a v2 feature layered on top of full-scan snapshots.
4. **Guardrails:** bounded + capped (agent count = subsystems, no recursion; cost cap; cadence floor) to
   prevent snowballing (see §2, §7).
5. **Category taxonomy:** confirmed as the top-level grouping (the §6 set). Maintainability/Debt stays its
   own category; the user can manually re-bucket a finding (like triage), with the consolidator's call as
   the default.
