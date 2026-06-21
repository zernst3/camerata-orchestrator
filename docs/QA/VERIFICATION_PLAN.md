# Camerata Verification Plan — Outputs vs Real-World Ground Truth

**Purpose.** Camerata produces findings (rule violations, SOC-2 gaps, security/threat
results, cost estimates, grounded citations). This document is the standing answer to:
*for each kind of output, what real-world ground truth do I compare it against, and how?*

It is a **living doc** — every new finding type Camerata emits gets a row here describing how
to verify it. It is distinct from a bug list: this tracks **verification methodology**, not defects.

Status legend: ☐ not started · ◐ in progress · ☑ verified-once · ⟳ needs re-verify after change.

---

## 0. Test repositories (ground truth you control)

Camerata scans **local clones** (it reads code on disk), so any repo must be cloned locally first.

**Best ground truth = repos you already know** (you can judge findings yourself):
- `rust-chorale` (Rust, small, yours) — already local
- `rust-portfolio` (Rust, small, yours) — already local
- `budget-mini` / `agora-mini` (small, yours) — already local
- `camerata-ai` (yours) — already local

**Public repos with KNOWN issues** (for precision/recall against a real tool) — clone before scanning:
- Security/SOC-2 lenses: OWASP **NodeGoat** (Node/Express), **RailsGoat** (Rails), **WebGoat** (Java),
  **django.nV** (Django) — intentionally vulnerable, documented flaws to detect.
- Clean baseline (should produce FEW findings): a small canonical TodoMVC implementation in the
  target language.

> NOTE: this is a fresh recommendation, not a list pinned from an earlier session. Start with your
> own small local repos — known ground truth beats unfamiliar code for a first verification pass.

---

## 1. Standard scan findings (rule violations) — Pillar 1

**What it claims:** specific lines violate specific rules.

**Verify against:**
1. **The real linter.** For any rule that maps to a real linter (now recorded in the rule's
   `[[sources]].linter`), run that linter (clippy, eslint, ruff, golangci-lint, rubocop, …) on the
   SAME repo and compare. Overlap = true positives; Camerata-only = check for false positives;
   linter-only = check for false negatives (recall gap).
2. **Seeded repo.** Plant N known violations of a rule, scan, confirm all N are caught (recall) and
   nothing spurious is added (precision).
3. **Sample review.** Manually review a random sample of ~15 findings on a repo you know.

**Metrics to record:** precision (% of findings that are real), recall (% of real violations caught),
line-accuracy (does the cited line match the actual offending line).

Status: ☐

---

## 2. SOC-2 gap analysis (deep tier) — ADVISORY

**What it claims:** which SOC-2 controls the codebase appears to leave gaps in. Explicitly
model-inferred + advisory; a *gap analysis, never a compliance report.*

**Verify against:**
1. **The real control framework.** Map each emitted gap to an actual **SOC-2 Trust Services Criteria
   (TSC)** control (CC-series, plus Availability/Confidentiality/etc.). Does the gap reference a real
   control, or an invented one? (Same "is the citation real" test as rule grounding.)
2. **A readiness checklist.** Compare against a published SOC-2 readiness / auditor control matrix
   (e.g. a Vanta/Drata-style control list, or the AICPA TSC points of focus). Are the gaps it raises
   ones a real readiness review would raise? Are obvious ones missed?
3. **Known-state reference.** Run it on a repo whose posture you know (e.g. one with no audit logging
   vs one with it) and confirm the gap appears/disappears correctly.

**Bar:** directionally useful for a readiness conversation — NOT certificate-accurate. Record where it
hallucinates controls or misses obvious ones; those bound the honest claim you can make about it.

Status: ☐

---

## 3. Deep security findings — ADVISORY

**Verify against:**
1. **A real SAST tool** on the same repo: Semgrep, `gosec`, `bandit`, CodeQL, Brakeman (by language).
   Compare overlap + each-only sets.
2. **Intentionally-vulnerable repos** (NodeGoat/RailsGoat/WebGoat/django.nV) with documented CVEs —
   does it find the known classes (injection, broken auth, secrets, etc.)?

Status: ☐

## 4. Threat model (deep tier) — ADVISORY

**Verify against:** a STRIDE pass on the same system — does Camerata's threat list cover the obvious
entry points / trust boundaries, or miss whole categories? Sanity, not completeness.

Status: ☐

---

## 5. Cost estimate vs actual

**What it claims:** `estimate_audit_cost` predicts the scan's dollar cost before running.

**Verify against:** run the scan; compare the pre-scan estimate to the **actual** `UsageMeter` total
(`cost_usd`, valid when `cost_complete == true`). Record the error %. Do this across the matrix:
standard vs **incremental** vs **full**, and with/without the **deep/SOC-2** tier (the two settings
currently NOT priced — see backlog). Target: estimate within ~±25% of actual.

Status: ☐ (blocked on the estimator fix that prices full-scan + deep tier)

---

## 6. Rule grounding / citations

**What it claims:** each rule's `[[sources]]` cite a real authority / linter rule.

**Verify against:**
1. **Linter-registry validator** (`crates/linter-registry`): does each cited `tool: rule-id` resolve to
   a real rule? Output = `docs/rule-grounding/citation-validation.md` (resolves / doesn't-resolve /
   unsourced). The doesn't-resolve column is the priority review list.
2. **URL spot-check.** Open a random sample of cited URLs; confirm they say what the rule claims.
3. **Risk order** (do these first): demo-set rules → mechanical/deny rules → known-language rules
   (C#, Rust, TS) → unknown-language tail. Promote to `verified` (human-only) as you clear each.

Status: ◐ (grounding wave in progress; validator being built)

---

## 7. Onboarding apply / greenfield scaffold

**Verify against:** after apply, diff the emitted `AGENTS.md` / `CONVENTIONS.md` / CI workflow against
the selected ruleset — every selected rule present, nothing extra. Greenfield: confirm the scaffold is
a valid, buildable repo with governance committed at commit zero.

Status: ☐

## 8. Governed dev loop / gate (Pillar 2)

**Verify against:** `cargo run -p camerata -- live-demo` — confirm the gate actually DENIES a real
agent's forbidden write (deny-before-execute), bounces on Layer-2, and the UoW lifecycle transitions
hold. The verified-flag deny-gate: confirm an agent cannot promote a rule to `verified`.

Status: ☐

---

## How to use this doc

1. Pick a row, pick a test repo from §0.
2. Run Camerata's output + the corresponding real-world tool/framework.
3. Record precision/recall (or estimate error %) + notes inline in the row.
4. Flip the status; for anything that changes upstream later, set ⟳.

When Camerata gains a new finding type, add a row here in the same shape *before* it ships to anyone.
