# The corpus serves two products, and they share one namespace

Written 2026-10-20 from Zach's observation: *"Some rules are process rules for Camerata to
use, process rules that I would stick to when developing, for example, but they are not rules
for a repo in particular. That is where the confusion might come from."*

That is the root cause of the confusion, and it is broader than the 14 supabase options it was
raised about. **Analysis only — nothing changed.**

## The two products

The corpus is read by two different consumers that have been treated as one:

1. **The client inspection.** Rules that can be true or false of a scanned repository.
2. **Camerata's own governance.** Rules about how Camerata operates, and how Zach works when
   building it. Several are literally his own working principles: `SPIRIT-DOC-DECISIONS-1`
   ("document decisions immediately"), `SPIRIT-ROBUSTNESS-1` ("robustness over terseness"),
   `SPIRIT-OPTIMIZE-1` ("optimization by default") all appear verbatim in his personal
   operating notes.

Nothing in the corpus marks which product a rule belongs to. `inspectable` comes closest, but
it answers a different question — "can a scan see this?" rather than "is this even ABOUT the
client?"

## What the data actually says — the hypothesis, refined

Measured against the controlled 2026-10-18 hearth export (237 findings):

**13 governance/process rows reached the client.** But they do NOT split along family lines,
which is the part that refines the hypothesis. Four produced genuinely good client findings:

| rule | finding |
|---|---|
| `PROC-CI-MIGRATION-HYGIENE-1` ×7 | "CREATE TRIGGER has no idempotent guard for retry" |
| `ORCH-ENV-GATED-QUALITY-1` ×2 | "CI pipeline never runs the test or lint scripts before merge" |
| `ORCH-NEW-PATH-TESTS-1` | "Numerous existing routes carry no tests" |
| `SPIRIT-OPTIMIZE-1` | "Reminder emails are sent one-at-a-time in a sequential loop instead of in parallel" |

Every one of those is checkable, specific, and worth a client's money. **So "exclude the
governance families" would be the wrong fix** — it would delete real coverage.

And `inspectable` is already carrying most of the load correctly: 14 of 18 `ORCH-*` rules are
`inspectable = false` and never scanned. `ORCH-MODEL-TIERING-1`, `ORCH-BUDGET-MONITOR-1`,
`ORCH-AUTOCALLS-LEDGER-1` and friends are about running an agent fleet, and they are already
out.

## The real test

A rule belongs in the inspection if and only if **it can be true or false of the client's
repository.** Not "is it a process rule" — process rules about the client's own working
practice pass this test fine (a PR template is a file; a CI job is a file).

- "CI never runs tests before merge" → checkable in their repo. **In.**
- "Reminder emails are sent sequentially" → checkable. **In.**
- "Model tiering: cheap model for bulk, top-tier for decisions" → not a property of their
  repository at all. **Out**, and already out.

## The two rules that DO misfire, and why it is not what it looks like

`ORCH-CONFORMANCE-1` and `SPIRIT-FILE-SIZE-1` each shipped one row reading:

> Structure and consistency observations: ORCH-CONFORMANCE-1 was flagged as a debatable
> structural or stylistic preference in 1 place across the codebase —
> `.github/workflows/ci.yml:9`. These are informational consistency notes, not confirmed
> defects; review at your own discretion.

They do not misfire because they are process rules. They misfire because **that row is not a
finding.** Three distinct defects in one line:

1. It names a Camerata rule id to the client, who has no idea what `ORCH-CONFORMANCE-1` is.
2. It says nothing about what is actually wrong — a reader cannot act on it.
3. It carries `kind = defect` while its own text says "not confirmed defects".

This is a `W25`/`W26` shape (client-facing text and fix quality), not a scoping question, and
it would be just as wrong on a `SUPABASE-*` rule.

## How this explains the 14 supabase options

The same conflation, one level down. Those options are **Camerata-about CONTENT inside
client-about RULES**:

> "Flag every SECURITY DEFINER function, regardless of grants or body contents"
> "Grep each migration file independently"
> "Skip reporting on buckets with safe-sounding names"

The RULE is correctly client-facing — a SECURITY DEFINER function without an auth check is a
real client fault. Only the rejected OPTION is about Camerata: it records a scanner design
that was considered and rejected.

So the resolution follows from the framing rather than from taste: **that content is
governance, not inspection, so the option does not belong in the client's decision set at
all.** Delete it and keep the rationale in `[decision].why`, where a rejected design choice
is exactly what belongs. The revoicing agent reached the same conclusion by instinct ("there
is no bad client state to name") and could not justify it; this is the justification.

## What this suggests, for Zach to decide

1. **Do not exclude the governance families.** Four of them produce real findings and
   `inspectable` already excludes the ones that should be excluded.
2. **Consider making the axis explicit.** `inspectable` currently conflates "a scan cannot see
   this" with "this is not about the client". Those are different reasons to skip, and the
   second one is a product boundary rather than a detection limit. A separate declaration —
   or simply documenting that `inspectable = false` means EITHER — would stop this question
   recurring.
3. **The 14 options get deleted, not reworded**, with their rationale preserved in
   `[decision].why`. That is a corpus edit and needs the go-ahead.
4. **Fix the "debatable structural or stylistic preference" row shape separately.** It is a
   client-text defect that happens to have surfaced on two governance rules, and it would be
   equally wrong anywhere else.

## One thing worth noticing about the second product

Camerata's self-governance rules are good, and they are the only written record of how Zach
works. They are not waste — they are what makes the orchestrator governable by its own
standard. The problem is only that they share a namespace with the client-facing corpus, so
every pass over "the rules" has to re-derive which kind it is looking at. That is the cost
being paid repeatedly, and it is the thing worth fixing.

---

## RESOLVED (2026-10-20, Fable-verified from source): the 14, and the shape

The "14 supabase options" question is settled. A Fable agent verified every claim below
against source; two corrections to my earlier analysis are folded in.

### What is wrong with them
Each of the 14 is a 2-option `kind = "defect"` rule. The first option names what the
CLIENT'S code should do (correct, client-facing). The second, `violates = true`, names **a
Camerata detection/corpus design that was considered and rejected** — "flag every definer
function regardless of grants", "grep each migration independently", "do not author a storage
rule, rely on another", "report both as one finding type". An option is meant to answer "what
does this repo do"; this one answers "what should the scanner do". Category error.

Live consequence: **none today.** Defect-kind rules are excluded from the alternatives pass
(`onboard/audit.rs:413`, `!is_defect_kind()`), so the option text reaches neither the scan
prompt nor the client; `arm.rs::render_rule` emits only the selected option's directive.
Correction to my own wording: the `violates` flag is not globally inert — it has a live
consumer in `apply_alternative_decision` (`ai_audit.rs:6445`) — it is UNREACHABLE for these
rules because they never enter that pass. Same practical result, different reason. (Residual:
`Rule::resolved_option` has no violates guard, so nothing structural stops a future path from
arming one; none does today.)

### Are there others? No.
TRUE total of the shape corpus-wide: **14, all in supabase/.** Zero elsewhere. All 22
non-supabase keyword suspects were false positives — genuine client states that happen to open
"rely on" (JAVA-RESOURCE-MANAGEMENT-1's "rely on GC to close resources"), or the separate
label-voice defect (SQL-MONEY-FLOAT-1's correct option with a scanner-voiced label). This is
exactly why the sweep needed reading, not grep.

### The fix, per rule
Two schema-valid moves (single-option defect rules are a proven shape — 5 ship today; the
`[decision]` block stays because it populates `default_option`; `suspicious_default_with_
exceptions_rules` only touches `default_with_exceptions`, so a defect rule trips nothing):

- **(a) Rewrite the violates option to the bad CLIENT state** — 6 rules, where a concrete
  detectable fault state exists distinct from "didn't follow the default":
  AUTH-EDGE-JWT-1, FUNC-DEFINER-CROSS-TENANT-READ-1, FUNC-DYNAMIC-SQL-INJECTION-1,
  FUNC-PRIVILEGED-NO-AUTHZ-1, FUNC-SEARCH-PATH-1, KEY-SERVICE-ROLE-CLIENT-1.
- **(b) Delete the violates option** (single-option defect), rationale to `[decision].why` or
  a TOML comment — 8 rules, where the default option already fully defines the required state,
  and for PERMISSIVE-TRUE / PUBLIC-BUCKET the read-side call is deliberately needs_review that
  a violates option would wrongly assert: EXPOSURE-MATVIEW-1, EXPOSURE-SCHEMAS-1,
  RLS-DISABLED-NOT-RESTORED-1, RLS-ENABLED-1, RLS-PERMISSIVE-TRUE-1, RLS-POLICY-DISABLED-1,
  STORAGE-OBJECT-POLICY-1, STORAGE-PUBLIC-BUCKET-1.

Needs Zach's go-ahead (corpus is protected). Not yet executed.

### Scope correction that outranks the 14: the directive-voice defect is ~15 rules and IS client-facing
Distinct from the 14. A NON-violates (selected-able) option's `directive` is emitted verbatim
into the client's CONVENTIONS.md (`report_export.rs:2073`), and ~15 rules OUTSIDE supabase/
have directives that open in scanner voice ("Scan column declarations... Flag as high
severity"). Verified on sql-money-float-1, arch-no-pan-storage-1,
arch-webhook-signature-verification-1, sec-no-sensitive-data-in-logs-1,
arch-observability-silent-failure-1. Full set: the SQL directive family (money-float,
timestamptz, fk-cascade-financial, enum-text-constraint, soft-delete-partial-index,
privileged-write-audit), the api-layer payment family (no-pan-storage,
payment-reconciliation, refund-authorization, webhook-signature-verification), the
ARCH-OBSERVABILITY trio, sec-no-sensitive-data-in-logs-1, javascript-public-env-prefix-secret-1.

The supabase revoicing pass (`12e8f69`) fixed this inside supabase/ only. The Phase-3 wording
batch list names "5 api-layer payment directives" and "universal gate-mechanics sentences" but
does NOT clearly cover the SQL directive family or the javascript one. **The wording batch is
undersized; widen it to this full ~15 before the graded sweep, because unlike the 14 this
ships to clients today.**
