# Verifying Camerata's SOC-2 Gap Analysis

How to check the deep-tier **SOC-2 gap analysis** against real-world ground truth. This is the focused,
standalone reference; it complements [VERIFICATION_PLAN.md](VERIFICATION_PLAN.md) §2.

## What the feature is (and isn't)

When you run an onboarding scan with the **deep-tier checkbox ON**, Camerata runs three model-inferred lenses
(SOC-2 gap, deep security, threat model) over the repo and attaches the result to the scan report's deep-tier
section. The SOC-2 output is a **gap analysis** — a list of SOC-2 controls the codebase appears to leave gaps
in. It is **advisory / model-inferred**, explicitly **a gap analysis, never a compliance report or
certification.** So you are verifying **directional correctness**, not certificate accuracy.

> The Markdown export of this report is queued (product wave); today it is screen-only in the cockpit.

## Test repos (in `~/Documents/Repos/sample apps/`)

Real, small, shipped apps to scan: **umami** (Next/React/TS), **linkding** & **healthchecks** (Django),
**node-express-realworld** (Express), **spring-petclinic** (Java/Spring), **miniflux** (Go). `spring-petclinic`
is a good known-thin-security baseline; `umami`/`healthchecks` have real auth + data + secrets surface.

## The four checks (cheapest first)

### 1. Are the controls real?
For each gap, confirm it maps to an **actual SOC-2 Trust Services Criteria (TSC) control** — the Common Criteria
**CC1–CC9** plus the Availability / Confidentiality / Processing-Integrity / Privacy categories. Cross-reference
a published control list:
- AICPA **Trust Services Criteria** (the authoritative source; "points of focus" per criterion).
- Any vendor's free SOC-2 control checklist (Vanta / Drata / Secureframe publish these) for a faster lookup.

A gap that cites an **invented or mis-numbered control** is the same failure mode as a fabricated rule citation —
flag it.

### 2. Would a real readiness review raise these?
Compare the gap set against a SOC-2 **readiness checklist**. Does it surface the real categories?
- Encryption in transit / at rest
- Access control & authentication (least privilege, MFA posture)
- Audit logging / monitoring
- Change management (PR review, CI gates)
- Secrets handling (no hardcoded credentials, vaulting)
- Dependency / vulnerability management

Just as important: **what does it MISS** that an obvious readiness review would catch?

### 3. Known-state reference
Scan a repo whose posture you know and confirm the gap behaves correctly.
- `spring-petclinic` is a demo with deliberately thin security — the gaps it raises (no real authn hardening,
  no audit logging) should be plausible and present.
- A/B test: a repo **with** a given control vs **without** it — the corresponding gap should disappear/appear.

### 4. Cross-tool on the security-overlapping gaps
There is no "SOC-2 linter," but the security-relevant gaps (injection, secrets, weak auth) should **overlap**
with a real SAST run on the same repo:
- `semgrep` (multi-language), `gosec` (Go: miniflux), `bandit` (Python: linkding/healthchecks),
  `npm audit` / `eslint-plugin-security` (JS: umami/realworld).
Compare overlap + each-tool-only sets.

## What to record

For each scan, note: **invented/mis-numbered controls** (check 1 failures), **obvious misses** (check 2),
known-state correctness (check 3), and SAST overlap (check 4). Those four numbers **bound the honest claim** you
can make about the feature — e.g. "directionally useful for a readiness conversation; not audit-grade."

## The honest framing

SOC-2 output is `advisory: true` by construction. Verifying it is about establishing it's **useful**, not
**authoritative**. Keep that line in any external description.
