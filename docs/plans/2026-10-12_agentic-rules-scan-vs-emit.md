# Agentic and process rules: scan target or emitted directive?

Analysis for the owner's question of 2026-10-12: *"analyze those rules and determine if they
should even be a part of the inspection at all; they are development process rules, so it
isn't really something determined by the code itself."* Reached from `SPIRIT-ROBUSTNESS-1`.

Scope: the 23 rules in the `SPIRIT-*` (4), `ORCH-*` (18), and `AGENTIC-*` (1) families, with
a briefer assessment of `PROC-*` (10) and `PROCESS-*` (4). Analysis only; no rule, code, or
test was changed. Every behavioral claim below was verified from the code path on branch
`feat/audit-report-export`, working tree of 2026-10-12, not from field names or doc comments.
Line numbers in `crates/rules/src/lib.rs` drift (concurrent edits); function names will not.

---

## 1. The answer in brief

1. **The owner's instinct is mostly right, with real exceptions.** Of the 23 rules, 15 are
   NOT SCANNABLE (they govern how an agent or team works and leave no reliable trace in the
   repository), 7 are WEAK PROXIES (something in the repo correlates, usually a CI workflow
   file, but the proxy is not the rule), and exactly 1 is genuinely SCANNABLE
   (`SPIRIT-FILE-SIZE-1`). None is NEITHER: every one of the 23 is a valid emitted directive.

2. **The product does NOT currently distinguish the two roles.** Every selected rule is
   scanned, with no exclusion of any kind (`semantic_rule_ids_for_repo`,
   `crates/server/src/onboard.rs:941-950`, whose doc comment says literally "No other
   filter, of any kind"). The emit path (`crates/server/src/arm.rs`) is a separate flow that
   partitions on `enforcement` for file placement only. There is no field, flag, or mechanism
   anywhere that says "emit this rule as a directive but do not scan for it."

3. **This is not an accident; it is the owner's own W4 ruling, applied one notch too
   broadly.** Until Pass-1 W4 (2026-10-09) the prefix check `is_code_auditable_rule`
   (`crates/server/src/onboard/audit.rs:357-359`) kept `ORCH-`/`SPIRIT-`/`PROC-` rules out of
   the semantic pass. W4 removed that exclusion on the owner's instruction ("Every single
   rule should be included in the AI semantic scan"), with the explicit rationale that the
   lexical gate was wrong for rules like `SPIRIT-FILE-SIZE-1` and `PROC-PR-CONCURRENCY-1`,
   which ARE observable (`onboard.rs:914-923`). The W4 reasoning was correct about the
   lexical gate and incomplete about the consequence: it said a trace-free rule "returns
   nothing from the model. That is the CORRECT outcome and it is cheap" (`onboard.rs:925-927`),
   but downstream, "returned nothing" is indistinguishable from "assessed and clean."

4. **The downstream consequence is the real problem, and it is client-facing.** A governance
   rule that runs and finds nothing is marked verified clean by the ledger
   (`RuleLedgerEntry::is_verified_clean`, `crates/server/src/scan_ledger.rs:184`:
   `ran && findings_emitted == 0 && !not_applicable`). It then:
   - gets a Coverage sheet row counted in the G3 summary line "We checked N rules against
     this codebase ... checked and found clean" (`xlsx_export.rs:928-974`, the line itself at
     `xlsx_export.rs:1466-1500`), and
   - becomes a candidate for the PDF's "What's healthy" list, which sorts grounded-citation
     rules FIRST (`report_export.rs:4368-4397`) and renders "N further rules verified clean
     this run" (`crates/server/templates/audit_report.typ:581-596`). 14 of the 23 carry an
     external source URL, so they resolve grounded citations and sort to the top. A client
     PDF can therefore lead its healthy list with, for example, "**ORCH-MODEL-TIERING-1**:
     Model tiering - cheap for bulk, top-tier for decisions," presented as verified clean,
     when no scan of the repository can verify any such thing. That is a false claim in a
     paid deliverable.

5. **Recommendation in one line:** keep all 23 in the EMIT path unchanged, add one per-rule
   corpus field that excludes a rule from the scan's assessment surfaces, set it on the 15
   non-scannable rules (and probably the weak-proxy ones pending a ruling per rule), and give
   the excluded set one honest client-facing line as adopted working agreements rather than
   per-rule checked-clean rows. Detail in section 7.

---

## 2. What the product does with these rules today, traced

### 2.1 Selection: they arm on effectively every unattended scan

- The `agentic` domain (all 18 `ORCH-*`, `AGENTIC-NO-TEST-TAMPER-1`, `PROC-STORY-DOCS-1`) is
  special-cased: always `recommended` and always `is_auto_recommended` regardless of the
  detected stack (`crates/server/src/onboard/propose.rs:771, 784-786`), and `agentic` is one
  of the two `PIPELINE_SELECTED_DOMAINS` selected without any stack match at all
  (`propose.rs:339`). The 4 `SPIRIT-*` rules are domain `universal`, the other pipeline-
  selected domain.
- All 23 carry an arming `verification` value (`policy` or `grounded`; verified against each
  TOML), so none is held back by the draft gate. None is `opt_in_only`.
- An unattended scan selects exactly the `is_auto_recommended` set
  (`curated_rule_selection`, `crates/cli/src/inspect_cmd.rs:155-165`). **Conclusion: all 23
  run on every unattended scan of every repo, regardless of stack.**

### 2.2 Scan: they reach the semantic pass, with no filter

- `semantic_rule_ids_for_repo` (`onboard.rs:941-950`) hands EVERY selected rule that applies
  to the repo to the AI pass. The three historical exclusions (CI-tier enforcement, existing
  gate arm, and the `ORCH-`/`SPIRIT-`/`PROC-` prefix) were all removed by W4; the function's
  doc comment (`onboard.rs:894-940`) records the owner's instruction and the removal history.
- Routing: `rule_scope` classifies by id prefix and maps anything that is not a language
  prefix to `Scope::All`, "audits every file" (`crates/server/src/scan_routing.rs:98-112`).
  All 23 are `Scope::All`, so their routing group always matches files; the `NotAssessed`
  honesty row for zero-matching-files groups (`ai_audit.rs:6556-6584`, skip recorded at
  `ai_audit.rs:~6125-6140`) practically never fires for them.
- They are excluded from exactly one scan stage: the alternatives/recommendation pass, via
  `is_semantic_multi_option_rule`, which requires `is_code_auditable_rule(id)`
  (`onboard/audit.rs:408-415`). So they never produce `Deviation`, `ConventionObserved`, or
  `NoConvention` rows; their only possible finding output is model-reported `Defect` rows.
- `AGENTIC-NO-TEST-TAMPER-1` and the 4 `PROCESS-*` rules are NOT caught by the prefix check
  at all (`AGENTIC-` is not in the list; `"PROCESS-"` does not start with `"PROC-"` because
  the check includes the hyphen). They are instead excluded from the alternatives pass by
  their `architectural` enforcement (`is_ci_enforced`). The prefix list is therefore not even
  a complete census of the governance families; it is a legacy lexical artifact.

### 2.3 Findings: what they can and cannot produce

- No deterministic producer touches any of the 23. The registered architectural checkers
  cover `SUPABASE-*`/`UI-UTC-DATES-1`/etc. (`crates/server/src/onboard/architectural.rs`),
  none of the 23. `mechanical_gate` only inspects `enforcement = "mechanical"` rules.
- `AGENTIC-NO-TEST-TAMPER-1`'s real detector exists and is genuinely deterministic, but it is
  a GOVERNED-PIPELINE guard, not a scan detector: `crates/server/src/test_tamper.rs` inspects
  the agent's unified diff inside Camerata's own dev-implement run (wired in
  `dev_implement_run.rs`; escalation spec consumed at `server/src/lib.rs` run assembly). It
  never runs during a client-repo inspection. Its `verification = "grounded"` and
  `enforcement = "architectural"` describe the pipeline guard, not any scan capability. The
  rule's own `qualifies` text says this explicitly.
- The 4 `PROCESS-*` rules likewise have a real deterministic enforcement point that is not
  the scan: the VCS-action gate over commit/PR/branch metadata at Camerata's own commit
  chokepoint (`crates/checks/src/vcs_action.rs:401-486`). The scan cannot see their subject
  matter at all: the file reader walks the working tree (dotfiles included,
  `crates/server/src/onboard/files.rs:253-330`) and the only git-history access in the whole
  scan is the narrow secret-context enrichment `commits_touching_path`
  (`onboard.rs:2050-2070`). Commit messages and branch names are never read.
- So in a real inspection, each of the 23 runs in the semantic pass, almost always returns
  zero findings (correctly, since there is nothing to find for 15 of them), and can
  occasionally produce a model-reported `Defect` row. For the 9 rules whose only sources are
  internal (`AGENTS.md`, decision docs; counted per TOML: all 4 SPIRIT minus DOC-DECISIONS,
  plus AUTOCALLS-LEDGER, BUDGET-MONITOR, CLEAR-WINNER, CONFLICTING-ROBUSTNESS,
  NO-NATURAL-BREAK, NOVELTY, PRECISION-RECALL), such a row is an uncited-AI finding and
  routes held/informational (`report_export.rs:1977-1982`); for the 14 with an external URL
  it resolves a grounded citation and can ship as a regular finding. A model-reported
  violation of, say, ORCH-REVIEWER-SPLIT-1 against a code chunk would necessarily be a
  hallucination; the grounded-citation path would still dress it in an authoritative
  citation.

### 2.4 Client-facing surfaces: what the client sees today

| Surface | What happens with these 23 rules |
|---|---|
| Coverage sheet, per-rule table | One row each, from `report.provenance.audited_rule_ids` (`xlsx_export.rs:928-974`), rendering 0 findings and a citation-kind label. Reads as checked-and-clean. |
| Coverage sheet, G3 one-line summary | All 23 counted in "We checked N rules against this codebase ... checked and found clean" (`xlsx_export.rs:1466-1500`). For 15 of them the word "checked" is not true in any meaningful sense. |
| "Checks we could not complete" (NotAssessed) | Not populated for them: their `Scope::All` routing group always matches files, and nothing else ever marks them unassessable. |
| "Rules not run this scan" | Not populated for them: the ledger records them as ran (semantic pass) (`report_export.rs:4409-4425` reads `excluded_rules()`, which they never enter). |
| PDF "What's healthy" / "verified clean" | Zero-finding candidates via `is_verified_clean` (`scan_ledger.rs:184`), grounded-citation rules sorted first (`report_export.rs:4380-4397`); rendered at `templates/audit_report.typ:581-596`. 14 of 23 are grounded-citation, so they preferentially occupy the capped 10 slots. |
| Emit (arm) | All 23 emitted on arm: `prose` rules into `AGENTS.md`, the rest into `CONVENTIONS.md` (`arm.rs:141-171`), rendered by `render_rule` (`arm.rs:95-108`). The `architectural` ones additionally produce `.camerata/checks.toml` entries and a generated gates workflow (`arm.rs:212-224`). This path is correct and should not change. |

### 2.5 The scan/emit split as it exists in code

- EMIT partitioning is `arm.rs`'s own string-level `is_ci_tier` (`arm.rs:88-90`) plus the
  `enforcement == "prose"` split (`arm.rs:145-154`). The method
  `EnforcementKind::emits_to_conventions` (`crates/rules/src/lib.rs:117-119`) has zero
  non-test callers; its only call sites are its own unit tests (`lib.rs:3101-3104, 3185`).
  Confirmed by workspace grep on 2026-10-12, consistent with RULES_REFERENCE section 11.1.
- SCAN selection has no per-rule role concept. The only levers that keep a selected rule out
  of the scan today are `opt_in_only` (keeps it out of unattended selection entirely) and a
  human unticking it. Both remove it from EMIT too, because arm installs the project's
  selected rules; there is no way to adopt a rule as a directive without also scanning it.

**Answer to the structural question: the two roles exist as two code paths, but no rule-level
distinction between them exists. Every selected rule is scanned regardless.**

---

## 3. Per-rule verdicts: the 23 in scope

Scannable verdicts: (a) scannable from the repo, (b) not scannable, (c) weak proxy.
Role verdicts: EMIT-ONLY, SCAN-ONLY, BOTH, NEITHER.

| Rule | Scannable | Role | Evidence, one line |
|---|---|---|---|
| SPIRIT-DOC-DECISIONS-1 | (c) | EMIT-ONLY | Presence of decision docs / a conventions file is observable, but the rule's substance is "written the moment it is made"; a repo with a stale decisions folder passes the proxy while violating the rule, and a young repo with no decisions yet fails it while compliant. |
| SPIRIT-FILE-SIZE-1 | (a) | BOTH | File length is a measurable fact of the tree; the auto-generated exemption is decidable from path/content. W4's own counter-example; the owner's premise is wrong for this one. |
| SPIRIT-OPTIMIZE-1 | (c) | BOTH, with a caveat | The accumulated result (N+1 queries, missing indexes, sequential awaits) is observable, but those exact observables are already owned by the specific perf rules (the 2026-05-26 async-efficiency family); as a scan target this rule duplicates them at a mushier altitude. The "from day one, not reactively" clause is pure process. |
| SPIRIT-ROBUSTNESS-1 | (b) | EMIT-ONLY | A tie-breaking policy between two correct implementations at authoring time; the scan sees only the written one, never the rejected alternative, so "chose the more robust of two correct options" is unrecoverable. Flagging terse-but-correct code as a violation would be the proxy, and it is a bad one. The owner's original observation; correct. |
| ORCH-AUTOCALLS-LEDGER-1 | (b) | EMIT-ONLY | The decision ledger and weekly review cadence live in the orchestration harness, not the client repo. |
| ORCH-BUDGET-MONITOR-1 | (b) | EMIT-ONLY | Spend tracking and model-invocation caps are harness runtime behavior; no repo artifact. |
| ORCH-CLEAR-WINNER-1 | (b) | EMIT-ONLY | An in-flight decision procedure for an agent; leaves no trace in the artifact. |
| ORCH-CONFLICTING-ROBUSTNESS-1 | (b) | EMIT-ONLY | An escalation rule about recognizing a tradeoff during a run; nothing to observe afterward. |
| ORCH-CONFORMANCE-1 | (c) | BOTH (weak) | The pipeline-config half is in-repo (a gates workflow, lint config); conformance per its own `qualifies` is "the pipeline configuration names that gate." The waiver-log half and "demonstrably blocked" are not establishable from a tree snapshot. |
| ORCH-CONTEXT-OVERRIDE-1 | (b) | EMIT-ONLY | "Pause and escalate before overriding a rule" is agent behavior mid-run; no repo trace. |
| ORCH-ENV-GATED-QUALITY-1 | (c) | BOTH (weak) | The pre-merge quality bar is visible in CI config (in-repo); the promotion topology through pre-production environments frequently lives in external deploy systems. Partially an outside-repo-facts rule (section 5). |
| ORCH-MODEL-TIERING-1 | (b) | EMIT-ONLY | Which model tier ran which work is a property of how Camerata (or any orchestrator) executes, invisible in the client's code. The owner's own example; correct. |
| ORCH-NEW-PATH-TESTS-1 | (c) | BOTH (weak) | A patch-coverage/diff-coverage gate is visible in CI config when present; "every new path carried tests when it landed" is per-change history the scan does not read. |
| ORCH-NO-NATURAL-BREAK-1 | (b) | EMIT-ONLY | A stop-condition policy for autonomous routines; no repo artifact. |
| ORCH-NOVELTY-1 | (b) | EMIT-ONLY | A confidence-calibration policy for agent decisions; no repo artifact. |
| ORCH-ONE-WAY-DOOR-1 | (b) | EMIT-ONLY | An escalation-routing policy by blast radius; the repo shows outcomes, never whether the routing happened. |
| ORCH-OUTPUT-DIGEST-1 | (b) | EMIT-ONLY | Notification/digest discipline for routines; outside the repo entirely. |
| ORCH-PRECISION-RECALL-1 | (b) | EMIT-ONLY | Detector-tuning policy for autonomous signals; no repo artifact. |
| ORCH-PREREVIEW-1 | (c) | BOTH (weak) | A reviewer-bot CI job is in-repo when present; the required-status/branch-protection half lives in forge settings, outside the repo (section 5). |
| ORCH-REVIEWER-SPLIT-1 | (b) | EMIT-ONLY | Author/reviewer agent separation is orchestration configuration; nothing in the client tree records it. |
| ORCH-TIERED-ESCALATION-1 | (b) | EMIT-ONLY | Soft-flag vs hard-pause policy; harness behavior only. |
| ORCH-TRAINING-CUTOFF-1 | (b) | EMIT-ONLY | A route-to-human policy keyed on decision category; no repo trace. |
| AGENTIC-NO-TEST-TAMPER-1 | (c) | EMIT-ONLY (for the inspection product) | Its grounded deterministic detector is real but is Camerata's own pipeline diff guard (`test_tamper.rs`), which never runs in a client-repo scan; the only scannable facet is an optional layer-4 CI check the rule's own `qualifies` says most repos will not have. Checked: no architectural checker registers this id (`onboard/architectural.rs`). |

Tallies: (a) 1, (b) 15, (c) 7. Roles: EMIT-ONLY 17, BOTH 6 (of which 4 are weak,
CI-config-facet-only), SCAN-ONLY 0, NEITHER 0.

The "BOTH (weak)" rows deserve honesty about how bad the proxy is: for CONFORMANCE,
ENV-GATED-QUALITY, NEW-PATH-TESTS, and PREREVIEW the in-repo observable is "a workflow file
with certain properties exists." A repo can carry a disabled or decorative workflow and pass;
a team can enforce all four through forge settings and external systems and fail the proxy.
A finding built on that proxy, shipped to a client as a defect, would be wrong often enough
to be embarrassing. If these stay scannable, their findings should stay informational-tier,
never action items.

---

## 4. The PROC-* and PROCESS-* families, briefly

### PROC-* (10): a mixed bag, NOT one category

This family genuinely splits, which is itself evidence that a lexical prefix is the wrong
mechanism:

| Rule | Scannable | Role | One line |
|---|---|---|---|
| PROC-PR-CONCURRENCY-1 | (a) | BOTH | The PR template IS a repo file; W4's own named example. |
| PROC-HIDE-DEAD-END-1 | (a) | BOTH | Dead-end affordances are observable in UI code; W4's other named example. |
| PROC-CI-MIGRATION-HYGIENE-1 | (a) | BOTH | CI migration checks are in-repo workflow config. |
| PROC-FEATURE-FLAGS-1 | (c) | BOTH (weak) | The flag mechanism is in code; "no long-lived feature branches" is branch history the scan does not read. |
| PROC-STORY-DOCS-1 | (c) | EMIT-primary | A `docs/<story-id>/` layout is observable, but enforcement is Camerata's own PostStoryHook (per its `qualifies`); domain `agentic`, so it arms everywhere. |
| PROC-MIGRATION-ROLE-GRANTS-1 | (c) | EMIT-primary | Migration SQL is observable, but the owner has already ruled the correct answer depends on whether an admin grant tool exists, possibly in another application. Outside-repo facts (section 5). |
| PROC-AUTO-MERGE-1 | (b) | EMIT-ONLY | Auto-merge is forge configuration (labels, repo settings), not repo content. Outside-repo facts. |
| PROC-PERMISSION-CONFIG-1 | (b) | EMIT-ONLY | A triage policy ("a missing grant is configuration, not a bug"); compliance is how humans route reports. Outside-repo facts. |
| PROC-CITE-CONVENTION-ID-1 | (b) today | EMIT-ONLY (+ pipeline gate) | Commit-message content; the scan reads no commit messages. Enforced forward by the VCS-action gate. |
| PROC-REGRESSION-TEST-1 | (b) | EMIT-ONLY (+ pipeline-enforceable) | "Every bug fix ships with a regression test" is a per-change property requiring diff/history correlation a tree snapshot cannot provide. |

### PROCESS-* (4): already handled correctly, by accident of configuration

All four (`ADO-LINK`, `BRANCH-NAMING`, `COMMIT-DOC`, `CONVENTIONAL-COMMIT`) govern commit/PR/
branch metadata. In principle the data is in the clone's git history; in practice the scan
never reads it (section 2.3), so they are (b)-in-practice. Their real enforcement is the
deterministic VCS-action gate at Camerata's commit chokepoint
(`crates/checks/src/vcs_action.rs:401-486`), which is complete by construction for governed
work. Critically, all four are `opt_in_only = true` and `default = false`, so they never
enter an unattended scan and never pollute coverage. **They are the existence proof that
"emit and gate, do not scan" already works when the configuration happens to produce it. The
same outcome for the 23 currently depends on nothing and happens for none of them.**

---

## 5. The second category: rules whose answer depends on facts outside the repo

Distinct from "governs the authoring process" but with the same consequence (unanswerable
from this repo), per the owner's `PROC-MIGRATION-ROLE-GRANTS-1` ruling:

- `PROC-MIGRATION-ROLE-GRANTS-1` - whether an admin grant tool exists, possibly elsewhere.
- `PROC-PERMISSION-CONFIG-1` - whether the team's triage actually routes grant gaps to
  admins.
- `PROC-AUTO-MERGE-1` - forge repo settings and label conventions.
- `ORCH-PREREVIEW-1` (half) - branch protection / required statuses live in forge settings.
- `ORCH-ENV-GATED-QUALITY-1` (half) - promotion gates and environment topology often live in
  external deploy systems.
- `PROCESS-ADO-LINK-1` (and siblings, partially) - whether the ADO project and work items
  exist is external, though the commit text itself is at least in history.

If a scan-role field is added (section 7), these need a value too; "unanswerable from the
repo" is the shared property whether the cause is process or external systems. A possible
third value of the same field ("requires external facts") would let the report say so
honestly instead of implying checked-clean.

---

## 6. Where the owner's premise is contradicted

He asked to be corrected rather than agreed with, so plainly:

1. **`SPIRIT-FILE-SIZE-1` is a genuine scan rule.** File size is a fact of the tree. It was
   the W4 poster child precisely because the old prefix gate wrongly silenced it.
2. **Three `PROC-*` rules are genuine scan rules** (`PR-CONCURRENCY`, `HIDE-DEAD-END`,
   `CI-MIGRATION-HYGIENE`): their subjects are files in the repo.
3. **Four ORCH rules have a real, if narrow, scannable facet** (CONFORMANCE,
   ENV-GATED-QUALITY, NEW-PATH-TESTS, PREREVIEW): CI workflow configuration is repo content,
   and "does the change pipeline enforce X" is partially answerable from it. Whether that
   facet is worth a client-facing row is a judgment call; the facet itself is real.
4. **"Development process rule" and "not scannable" are not the same set.** The clean
   formulation is not "process rules out of the scan" but "rules whose compliance leaves no
   reliable trace in the artifact out of the scan's ASSESSMENT CLAIMS." That reformulation is
   what rescues both W4 (which was right that the lexical gate over-excluded) and the current
   instinct (which is right that blanket inclusion over-claims).

Also worth stating: the blanket inclusion was not causing false findings in practice as far
as static analysis can tell; the model mostly returns nothing for these rules, exactly as W4
predicted. The damage is quieter and worse for a paid product: false "verified clean" and
inflated "we checked N rules" claims (section 2.4), plus roughly 23 rules of prompt tokens
per chunk spent asking the model questions it cannot answer from the input it is given.

---

## 7. Recommendation

### 7.1 What to change

Add one rule-level corpus field; do not resurrect the prefix gate and do not use a domain
exclusion.

- **New field:** rule-level `scan_role` (or `assessment`), enum, default `"scan"`:
  - `"scan"` (default) - today's behavior, full assessment surface.
  - `"emit-only"` - the rule is selectable, armable, and emitted exactly as today, but is
    excluded from the semantic prompt, from `audited_rule_ids`/Coverage rows, from the G3
    "we checked N rules" count, from "What's healthy"/verified-clean candidacy, and from
    NotAssessed synthesis. It is not a scan rule, so no scan surface may claim anything
    about it.
  - Optionally `"external-facts"` for section 5's category, rendering as "requires
    information outside this repository" rather than silently emit-only; this can be
    deferred.
- **Why a field and not the prefix:** the per-rule table shows every family is mixed
  (`SPIRIT-FILE-SIZE-1` scannable among SPIRIT; `PROC-PR-CONCURRENCY-1` scannable among
  PROC; `PROCESS-` not even matched by the `"PROC-"` check). W4 already demonstrated the
  lexical gate fails in one direction; this analysis shows blanket inclusion fails in the
  other. Per-rule truth needs a per-rule field. The field is also self-documenting in the
  TOML, where the prefix check was invisible action at a distance.
- **Why not a domain exclusion:** domain `agentic` would catch 20 of the 23 but misses the
  4 `SPIRIT-*` (domain `universal`, which must never be blanket-excluded) and wrongly
  sweeps any future scannable rule filed under `agentic/`.
- **Consumption points** (all small): `semantic_rule_ids_for_repo` (`onboard.rs:941-950`)
  or, better, one step earlier where `audited_rule_ids` provenance is recorded, so the
  ledger never records the rule as ran; the Coverage row builder reads
  `audited_rule_ids` and needs no change if provenance is fixed at the source. Per the
  existing anti-inertness discipline, ship it with a corpus gate asserting the field's
  vocabulary and a probe asserting the corpus actually uses it (the `max_severity` lesson:
  a mechanism fully tested against a corpus that never exercises it is inert,
  RULES_REFERENCE section 3.15).

### 7.2 Which rules get it

- `scan_role = "emit-only"` on the 15 (b)-verdict rules in section 3, plus
  `AGENTIC-NO-TEST-TAMPER-1` and `SPIRIT-DOC-DECISIONS-1` (their proxies are too weak to
  ship as client claims), plus `PROC-CITE-CONVENTION-ID-1`, `PROC-REGRESSION-TEST-1`,
  `PROC-PERMISSION-CONFIG-1`, `PROC-AUTO-MERGE-1` (and the 4 `PROCESS-*` for consistency,
  harmless since opt-in keeps them out anyway).
- Keep scannable: `SPIRIT-FILE-SIZE-1`, `PROC-PR-CONCURRENCY-1`, `PROC-HIDE-DEAD-END-1`,
  `PROC-CI-MIGRATION-HYGIENE-1`, `PROC-STORY-DOCS-1`, `PROC-FEATURE-FLAGS-1`,
  `SPIRIT-OPTIMIZE-1` (owner may prefer emit-only given the redundancy caveat).
- The four CI-config ORCH rules (CONFORMANCE, ENV-GATED-QUALITY, NEW-PATH-TESTS, PREREVIEW)
  are the genuine judgment calls; section 8 routes them to the owner.

### 7.3 The client-facing consequence

- The G3 coverage line's "We checked N rules" becomes true.
- "What's healthy" stops listing orchestration policy as verified clean.
- One new honest line (G3-compliant, one sentence) in the Coverage sheet and/or PDF
  methodology: something like "A further 17 adopted rules are working agreements governing
  how future changes are made; they are installed in this project's AGENTS.md and
  CONVENTIONS.md and are not assessable from the current code, so they are not counted
  above." One line, not 17 rows.
- Net effect on findings: approximately none; these rules essentially never produce real
  findings today. The deliverable gets shorter and every remaining claim in it is true.

### 7.4 If the owner prefers zero mechanism

The minimal alternative is `opt_in_only = true` on the 15, which removes them from
unattended scans today with no code change. It is strictly worse: it also removes them from
the recommended set in proposals, so they stop being adopted and emitted by default, which
damages the EMIT half that is working correctly. Not recommended; listed for completeness.

---

## 8. Open questions for the owner

1. **The four CI-config ORCH rules** (CONFORMANCE, ENV-GATED-QUALITY, NEW-PATH-TESTS,
   PREREVIEW): is the "a workflow file with these properties exists" facet worth a scan
   claim, or emit-only? My lean: emit-only for CONFORMANCE and PREREVIEW (the enforcing half
   lives in forge settings), scannable-but-informational for ENV-GATED-QUALITY and
   NEW-PATH-TESTS (the pre-merge bar and a coverage gate genuinely live in the repo).
2. **`SPIRIT-OPTIMIZE-1`:** keep scannable (its observables are real) or emit-only (the
   specific perf rules already own those observables and this duplicates them mushily)?
3. **Does W4's ruling stand as stated?** "Every single rule should be included in the AI
   semantic scan" was given in the context of deterministic rules needing a second pair of
   eyes. Excluding emit-only rules is not in tension with that purpose (there is nothing for
   a second pair of eyes to look at), but it does narrow the literal instruction, so it
   needs his explicit sign-off rather than a quiet reinterpretation.
4. **Should the emitted-working-agreements disclosure line appear in the PDF, the xlsx
   Coverage sheet, or both?** G3 says coverage is client-facing but concise; one sentence in
   both seems right.
5. **The `"external-facts"` third value** (section 5): ship now alongside emit-only, or
   defer? It affects at least 5 rules and the owner has already ruled on the pattern once
   (`PROC-MIGRATION-ROLE-GRANTS-1`).
6. **`is_code_auditable_rule`'s prefix list** (`onboard/audit.rs:357-359`) survives as the
   alternatives-pass gate and does not match `AGENTIC-` or `PROCESS-`. Today that is
   harmless (those rules are excluded from the alternatives pass by `is_ci_enforced`
   instead), but the moment a prose/structured `AGENTIC-*` multi-option rule is authored, it
   would enter the alternatives pass. If `scan_role` lands, that predicate should read the
   field, not the prefix. Flagging rather than deciding.

---

## 9. Incidental findings (no code changed; recorded per the task's constraint)

1. **Lexical near-miss:** `is_code_auditable_rule` excludes `"PROC-"` but not `"PROCESS-"`
   and not `"AGENTIC-"` (`onboard/audit.rs:357-359`). Currently masked by those rules'
   `architectural` enforcement; latent, not live.
2. **Confirmed inert method:** `EnforcementKind::emits_to_conventions`
   (`crates/rules/src/lib.rs:117-119`) has zero non-test callers; the live emit split is
   `arm.rs:88-90` plus the `== "prose"` filter at `arm.rs:145-154`. Matches RULES_REFERENCE
   section 11.1; re-verified by grep on 2026-10-12.
3. **`ScanReport::excluded_mechanical_rules` is vestigial** since W4; nothing populates it
   and its two readers can only render empty (documented in the code's own comment at
   `report_export.rs:4399-4408`). Worth removing when a schema change is next convenient.
4. **The healthy-list preference for grounded citations actively worsens this specific
   problem:** 14 of the 23 governance rules carry external URLs, so the capped 10-slot
   "What's healthy" list preferentially fills with exactly the rules whose "verified clean"
   is least meaningful (`report_export.rs:4392-4397`).
5. **`ModelDeclined` exists in the NotAssessed reason vocabulary and has a client label**
   (`xlsx_export.rs:891`) but no producer was found in the scan pipeline; if it is intended
   as the honest outcome for "the model was shown a rule it could not assess," it is not
   wired to anything that would produce it.
