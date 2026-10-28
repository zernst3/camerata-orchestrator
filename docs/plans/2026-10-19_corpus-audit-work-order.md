# Corpus audit work order (2026-10-19)

All 417 rules audited against `docs/RULE_AUTHORING.md` in four read-only partitions. This
document is the durable record of what to change; the four agent transcripts hold the full
proposed replacement text and are referenced per batch.

Transcripts (session-local, treat as expiring):
`/private/tmp/claude-501/.../tasks/{a782d1d51cfc29d77,a4ce6d77812ed33d3,aa6563aa397833f5a,aa7532a470ded659f}.output`

## Totals

| partition | files | clean | wording fields | structural |
|---|---|---|---|---|
| supabase + sql + ci-cd | 62 | 15 | ~60 | 17 |
| rust + javascript + universal | 136 | 111 | 18 | 9 |
| go + python + csharp + java + ruby | 141 | 93 | 42 | 13 |
| api-layer + agentic + 8 small | 78 | 31 | 47 | 14 |
| **total** | **417** | **250** | **~167** | **~53** |

`testing/` is the healthiest pack in the corpus (6 of 7 clean, real decision questions,
stative parallel labels, no truncated ids, hand-authored). It is the worked example the rest
should be edited toward.

## Owner rulings, 2026-10-19

1. **`Rejected:` token — strip at render.** Engine one-liner in
   `apply_alternative_decision`; the 71 existing uses become harmless; zero corpus edits. The
   guide already says the token is optional and fault-first prose is mandatory (`9e15c4e`).
2. **Labels — verifiability, not grammar.** LABEL-4 replaces the retracted noun-phrase rule
   (`c97780c`). Finding set shrinks from ~195 to ~15, concentrated in `agentic/`.
3. **Truncated option ids — FIX THEM.** No real client scan has run, so there are no persisted
   selections to migrate. Measured blast radius: 1,101 option ids, **18 hardcoded in Rust**,
   zero persisted selections. Six of the 18 are themselves truncated.
4. **Group 2 (mis-declared layering rules) — FIX.** "Ensure it runs as best as we can make it
   ... go with the class of shape."
5. **Group 3 (A5.7-rostered rules) — HOLD LIFTED.** "You can make the updates as needed."

## STRUCTURAL batches

### Group 1 — `violates` options that describe Camerata, not the client (17, supabase)

Runtime-INERT: verified at `onboard/audit.rs:411` (`is_semantic_multi_option_rule` excludes
`is_defect_kind()`), so a `defect`-kind rule never enters the alternatives pass and these
flags are documentation only. Removing or reshaping changes no engine behaviour.

Each option's `why` is a rejected SCANNER DESIGN ("Flag every X regardless of Y", "Do not
author a separate rule; rely on RULE-Z", "Grep each file independently, no timeline replay").
Rationale already lives, often verbatim, in each rule's `[decision].why`.

`supabase-auth-edge-jwt-1` (BOTH options fail — worst case), `supabase-func-definer-cross-tenant-read-1`,
`supabase-func-dynamic-sql-injection-1`, `supabase-func-privileged-no-authz-1`,
`supabase-func-search-path-1`, `supabase-exposure-matview-1`, `supabase-exposure-schemas-1`,
`supabase-key-service-role-client-1`, `supabase-rls-disabled-not-restored-1`,
`supabase-rls-enabled-1`, `supabase-rls-no-policy-1`, `supabase-rls-permissive-true-1`,
`supabase-rls-policy-disabled-1`, `supabase-storage-object-policy-1`,
`supabase-storage-public-bucket-1`.

Also here: **`supabase-key-service-role-client-1` has NO severity bound** while its prose says
"unconditionally critical" twice. Verified: 0 bounds declared. Add `min_severity = "critical"`.

### Group 2 — layering rules mis-declared `decision` (7) — APPROVED TO FIX

Every one carries `deviation_ceiling = "high"`, the authors' own marker that they mirror
`ARCH-STRICT-LAYERING-1`. But `decision` kind FORBIDS `violates` (build gate), so the
fat-controller option cannot be flagged — and a repo that genuinely puts logic in its
controllers gets `apply_alternative_decision` rewriting its effective directive to that
option, then a `ConventionObserved` row reading *"The codebase consistently follows 'put
business logic and data access directly in handlers' — zero violations were observed"*. The
fault reported as the client's chosen convention.

| rule | degenerate option |
|---|---|
| `go/go-handler-service-repository-1` | `logic-in-handlers` |
| `java/spring/java-spring-thin-controllers-1` | `fat-controllers-inline-logic` |
| `java/spring/java-spring-transactional-service-layer-1` | `transactional-on-controller` |
| `python/django/python-django-fat-model-service-1` | `fat-views`, `fat-serializers` |
| `python/flask/python-flask-service-layer-1` | `logic-in-route-handler` |
| `python/python-service-layer-1` | `logic-in-views` |
| `ruby/rails/ruby-rails-skinny-controllers-1` | `fat-controller` |

Target shape: `kind = "default_with_exceptions"` + `violates = true` on the degenerate option
+ fault-first `why` rewrite. Two rules ALREADY have this shape and are the template:
`csharp/aspnetcore/csharp-aspnetcore-thin-controllers-1` and
`java/spring/java-spring-layered-architecture-1`.

Note `java-spring-thin-controllers-1` and `csharp-aspnetcore-thin-controllers-1` share the
option id `fat-controllers-inline-logic` with OPPOSITE flags under different kinds today.

Consequence to expect: a project already decided onto a degenerate option moves from a
convention row to a guaranteed-high Defect. No option ids change, so no selection breaks.

### Group 3 — A5.7 roster (4) — HOLD LIFTED

Roster at `crates/rules/tests/w14_corpus_schema_gates.rs:279`, 7 entries, asserts equality in
BOTH directions — so every resolution needs a deliberate roster edit. The file already
contains a worked precedent (`ARCH-OBSERVABILITY-SILENT-FAILURE-1`, resolved 2026-10-12 and
removed with a comment block explaining why). Follow that format exactly.

- **`SUPABASE-AUTH-GETSESSION-SERVER-1` → `defect`.** Both options say "flag it" (narrowly vs
  broadly); neither is decidable from the repository. Correct option "Server code establishes
  identity via getClaims()/getUser()", violates option "Server code trusts getSession() for
  identity". Path scoping stays in `qualifies`, where it already is verbatim.
- **`SUPABASE-RLS-INITPLAN-1`** — options are "Recommend the wrap, opt-in only" and "Report it
  as a standard finding". Both describe Camerata's report. Client states are "policies wrap
  auth.*() in a scalar subselect" vs "policies call auth.*() bare"; opt-in placement is
  already carried by the rule's own `opt_in_only = true`.
- **`SUPABASE-AUTH-EMAIL-CONFIRMATION-1`** — its options DO describe client states; the KIND is
  what is wrong (one option is plainly rejected, so it is defect/default_with_exceptions
  shaped).
- **`ARCH-SERVER-CLIENT-PROJECTION-1`** (2 roster entries) — two options' `why` opens
  `Rejected:` with no `violates`. Honest shape is `defect`.

**Explicitly NOT resolved, by prior ruling recorded in the roster itself:**
`ARCH-OBSERVABILITY-CORRELATION-ID-1` stays `decision`; `SEC-NO-RAW-SQL-CONCAT-1 /
escape-then-concatenate` is "classified a clear `decision`" and is the roster's own documented
"genuine finding the 244-rule audit missed". Leave both.

### Group 4 — truncated option ids — APPROVED TO FIX

Machine-truncated at a 48-char slug cap, many cut mid-word, client-visible as
`convention_option_id` / `observed_options[].option_id` in `findings.json` and the xlsx.

- partition 2 measured **187 of 352** at the cap, **106 provably mid-word**
- partition 4 measured **98 of 244**, clean in `testing/`, `process/`, `integration/`
  (hand-authored)
- corpus total: 1,101 option ids; **18 hardcoded in Rust** (the only real blockers)

The 18: `allow-modifying-tests-within-the-same-change-whe`, `allow-silent-discards`,
`codeql-ghas-paid`, `codeql-public-free`, `dependency-arrays-are-trimmed-to-control-when-ef`,
`effect-and-memo-dependency-arrays-list-every-rea`, `enforce-rls-enabled-replayed-end-state`,
`escalate-before-modifying-or-deleting-an-existin`, `explicit-unitofwork-parameter-on-transactional-r`,
`living-central-docs`, `log-or-justify-every-caught-error`, `mechanical-minimum`,
`no-restriction-on-test-edits`, `pyramid-many-unit-fewer-integration-few-e2e`,
`regex-grep-each-migration-independently`, `semgrep-appsec-platform-pro`,
`semgrep-community-edition`, `services-own-the-transaction-lifecycle-repositor`.

**Also fix the generator**, or the next authored rule reproduces the same truncation. Finding
it is part of this batch.

Sub-cases worth care: `orch-autocalls-ledger-1` and `orch-conflicting-robustness-1` each have
siblings disambiguated ONLY by a numeric suffix off an identical truncated prefix
(`…-robust-against-different-f`, `-2`, `-3`), so the id carries no information about which
option it names. `[decision].default` references must stay consistent (ungated against
dangling ids — reference gotcha 11).

## WORDING batches

Full proposed replacement text is in the transcripts. Shape per batch:

1. **supabase directives (35 files)** — scanner recipes → behavioural commitments. The
   mechanics are ALREADY in `qualifies` and the severity bounds, so these are deletion +
   revoicing with nothing needing a new home. **Caveat recorded by the auditor:** the
   `# Severity floor, transcribed from this rule's own prose: "Flag at high severity..."`
   comments quote directive prose these rewrites delete — update each comment to cite the
   bound's rationale directly so the anchors do not dangle.
2. **~26 supabase labels + ~12 `[decision].question`s** — tool voice → client state.
3. **31 `violates`-option `why`s opening on the upside** (go/python/csharp/java/ruby). A
   client's high-severity CORS finding currently opens *"Appropriate only for fully public,
   unauthenticated APIs"*. Worst: `csharp-aspnetcore-cors-explicit-1`,
   `go-gorm-parameterized-queries-1` (SQL injection, detail opens "Easy to code").
4. **`integration/` (3 files)** — directives describe Camerata's own reconciliation ENGINE
   ("run the integration gate's reconciliation engine for this rule") as the client's
   convention.
5. **`process/` (4 files)** — directives embed Camerata's layer numbering, gate names, and UI
   navigation paths ("project Settings → VCS gate → ADO link", "NOT enforced at the layer-2
   code gate").
6. **`universal/` gate-mechanics sentences (W10–W14)** — move verbatim to `qualifies`; good
   enforcement-honesty prose, wrong field.
7. **5 `api-layer` payment directives** — scanner recipes on PCI-adjacent rules.
8. **~15 slogan/proposition labels** (`agentic/`) — the real LABEL-4 failures.
9. **6 `[[sources]].title`s carrying the author's own confidence grading** ("(low authority —
   …)") inside client citations. Move to `relevance`.
10. **13 directives with hard newlines** — `ai_audit.rs:2247` renders `- [{id}] {directive}\n`
    unescaped, so a wrapped directive breaks the one-rule-per-line list the prompt asks the
    model to enumerate.
11. **~37 + ~17 template `[decision].question`s** — `"What position does the project take on:
    <title>?"`. OPERATOR-only, low stakes. **Fix the guide's own orientation snippet first**
    (line ~102 seeds this exact string) or they regrow.

## ENGINE / DOC items found by the audit, outside the corpus

- **`apply_alternative_decision`: strip the `Rejected:` token at interpolation** (ruling 1).
- **`report_export.rs:2042-2050`** — `resolve_fix`'s doc comment still defines `directive` as
  "internal, agent-facing instructions for how Camerata itself finds this defect", the exact
  framing the 2026-10-19 correction repudiates. Behaviour is right; the characterisation is
  stale and will mislead.
- **`RULES_REFERENCE.md` §5.3** — same stale sentence ("written for an agent/model, not a
  client"). An author reading only §5.3 reproduces the original defect.
- **`onboard.rs:1366-1373` and `:1425-1427`** — two comments claim governance/process families
  are dropped from the code audit. `semantic_rule_ids_for_repo` applies no prefix filter;
  `is_code_auditable_rule` now gates only `is_semantic_multi_option_rule`. Stale.
- **Field-interaction rules to ADD to the guide:**
  - `enforcement` × multi-option: `mechanical`/`architectural` ⇒ CI-tier ⇒ the ENTIRE decision
    apparatus is inert (no alternatives pass, no `violates`, no ConventionObserved, no
    NoConvention). The single most consequential interaction and absent from the list. 27
    CI-tier rules carry ≥2 options; 12 carry flags that can never fire.
  - `remediation` × `violates`: on a non-`violates` option, remediation-to-reach-that-state is
    correct (`ui-query-library-1`, `arch-monolith-first-1` both do it properly); on a
    `violates` option the identical syntax ships a Fix telling the client to implement the
    fault. Only the flag distinguishes them. Instance fixed in `d0ea2ea`.
  - `inspectable` scope: working tree vs VCS metadata. The four `process/*` rules declare
    `false` for commit subjects and branch names, which ARE readable from a clone;
    `proc-pr-concurrency-1` declares `true` because its subject is a file. Both defensible
    under current wording, so the wording is underspecified.
- **Two `deviation_ceiling` fields inert on `defect`-kind rules**
  (`javascript-angular-di-constructor-or-inject-1`, `rust-sqlx-transactions-multi-write-1`,
  plus `go-grpc-status-codes-correct-1`).
- **`proc-pr-concurrency-1` is `verification = "grounded"` with one non-http source**, so it
  resolves ZERO citations. `policy` is the honest label.
- **`javascript-angular-subscription-cleanup-1`** flags a state its own conforming option
  permits — manual `.subscribe()` + `ngOnDestroy` unsubscribe is both allowed by the default
  directive and carries `violates = true`.
- **`testing-one-assertion-per-test-1 / strict-one-physical-assert`** — `violates` on an
  over-application, not a fault; its own `why` opens "Guarantees the clearest possible failure
  signal".
- **`proc-permission-config-1`** — `inspectable = false` with `kind = "defect"` and two
  `violates` options: dead machinery, since the rule is removed before arming. One of the two
  declarations is wrong.
