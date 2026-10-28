# Camerata Rules Reference

The authoritative functional reference for every field and flag a corpus rule TOML can carry:
what the engine actually DOES with each value, where it is consumed, what happens when it is
absent or malformed, and which other fields it interacts with.

Every claim cites `file.rs:line`. **Line numbers were re-verified against the working tree on
2026-10-14**, branch `feat/audit-report-export`. `crates/rules/src/lib.rs` grew by roughly 300
lines between the first edition of this document (2026-10-11) and this one, so every citation
into that file was remapped; a citation that still looks wrong should be resolved by the
FUNCTION or FIELD NAME named next to it, which is the stable anchor. Where source comments are
quoted, any dash characters in the original have been rendered as hyphens per house style.

Corpus counts quoted here were measured on 2026-10-14 against the rule files as they stood that day. Where a count is
pinned by a test, the test is named; where it is not, it is a measurement and will drift.

Scope note: this document describes the schema and the engine. It deliberately does not
snapshot the per-rule classification values of rules still on the owner's A5.7 reclassification
hold (`docs/plans/2026-10-09_rule-kind-classification.md`). The `kind` distribution on
2026-10-14 was `decision` 154, `defect` 152, `default_with_exceptions` 110,
`decision_with_floor` 1. The authoritative count is always the TOMLs:
`git grep -h '^kind = ' -- crates/rules/principles | sort | uniq -c`.

---

## Table of contents

- [1. The pipeline in one page](#1-the-pipeline-in-one-page)
- [2. Loading and parsing](#2-loading-and-parsing)
- [3. Rule-level fields](#3-rule-level-fields)
  - [3.1 `id`](#31-id)
  - [3.2 `title`](#32-title)
  - [3.3 `enforcement`](#33-enforcement)
  - [3.4 `domain` (in-file) and the folder-derived domain](#34-domain-in-file-and-the-folder-derived-domain)
  - [3.5 `extra_domains`](#35-extra_domains)
  - [3.6 `kind` (the RuleKind taxonomy)](#36-kind-the-rulekind-taxonomy)
  - [3.6a `inspectable`](#36a-inspectable)
  - [3.7 `default`](#37-default)
  - [3.8 `verification`](#38-verification)
  - [3.9 `[[sources]]`](#39-sources)
  - [3.10 `[verified]`](#310-verified)
  - [3.11 `opt_in_only`](#311-opt_in_only)
  - [3.12 `layer3_only`](#312-layer3_only)
  - [3.13 `[[stack_exception]]`](#313-stack_exception)
  - [3.14 `qualifies` (and the derived `summary`)](#314-qualifies-and-the-derived-summary)
  - [3.15 `max_severity`](#315-max_severity)
  - [3.16 `min_severity`](#316-min_severity)
  - [3.17 `deviation_ceiling`](#317-deviation_ceiling)
  - [3.18 `layer` (INERT) and `tag` (DELETED)](#318-layer-inert-and-tag-deleted)
  - [3.19 `[[emits]]` (INERT)](#319-emits-inert)
  - [3.20 `version`](#320-version)
  - [3.21 `last_reviewed`](#321-last_reviewed)
  - [3.22 `superseded_since` and `current_recommendation`](#322-superseded_since-and-current_recommendation)
  - [3.23 The rule content hash and `CORPUS_LOCK.toml`](#323-the-rule-content-hash-and-corpus_locktoml)
- [4. The `[decision]` block](#4-the-decision-block)
- [5. `[[option]]` fields](#5-option-fields)
  - [5.1 `id`](#51-id)
  - [5.2 `label`](#52-label)
  - [5.3 `directive`](#53-directive)
  - [5.4 `why`](#54-why)
  - [5.5 `remediation`](#55-remediation)
  - [5.6 `finding_headline` and `finding_detail`](#56-finding_headline-and-finding_detail)
  - [5.7 `effort`](#57-effort)
  - [5.8 `escalation`](#58-escalation)
  - [5.9 `violates`](#59-violates)
- [6. The five finding kinds](#6-the-five-finding-kinds)
- [7. The severity pipeline, in order](#7-the-severity-pipeline-in-order)
- [8. Bucket routing: what reaches the client, and where](#8-bucket-routing-what-reaches-the-client-and-where)
- [9. Named gotchas](#9-named-gotchas)
- [10. Rule-authoring guide](#10-rule-authoring-guide)
  - [10.1 Worked example: a `defect`-kind rule](#101-worked-example-a-defect-kind-rule)
  - [10.2 Worked example: a `decision`-kind rule](#102-worked-example-a-decision-kind-rule)
  - [10.3 Worked example: a `default_with_exceptions`-kind rule](#103-worked-example-a-default_with_exceptions-kind-rule)
  - [10.3a Worked example: a `decision_with_floor`-kind rule](#103a-worked-example-a-decision_with_floor-kind-rule)
  - [10.4 The `Rejected:` convention](#104-the-rejected-convention)
  - [10.5 The self-evidence discipline](#105-the-self-evidence-discipline)
  - [10.6 Corpus-load gates that will reject a rule](#106-corpus-load-gates-that-will-reject-a-rule)
- [11. Open questions, inert fields, and inconsistencies](#11-open-questions-inert-fields-and-inconsistencies)

---

## 1. The pipeline in one page

A rule's fields are consumed at seven distinct stages. Knowing which stage reads a field is the
fastest way to predict what changing it does.

1. **Load** (`camerata_rules::load_corpus` / `load_corpus_lenient`,
   `crates/rules/src/lib.rs:1474/1489`). TOML files under `crates/rules/principles/` (override:
   `CAMERATA_CORPUS_PATH`, `lib.rs:1460-1465`) parse into `Rule` structs. The folder path, not
   the in-file `domain` field, decides the rule's domain (`lib.rs:1576-1618`). Serde itself
   still ignores unknown field names (`deny_unknown_fields` is deliberately NOT set), but since
   W14 a separate corpus-load gate, `corpus_files_with_unknown_fields` (`lib.rs:2208`), fails
   the BUILD on any key outside the declared vocabulary, across seven nested vocabularies.
   See section 2.

2. **Proposal / selection** (`onboard/propose.rs`). `detect_stack` (`propose.rs:271`) maps the
   repo to corpus domains via `domains_for_stack` (`propose.rs:527`); `propose_corpus_rules`
   (`propose.rs:654`) lists EVERY corpus rule but marks a subset `recommended` and a
   narrower subset `is_auto_recommended` (`propose.rs:771, 784-786`). An unattended scan runs
   exactly the `is_auto_recommended` set (`crates/cli/src/inspect_cmd.rs:155-165`). Fields
   consumed here: folder-derived `domain`, `extra_domains`, `verification`, `opt_in_only`,
   `enforcement` (for the placement string), plus `options`/`default_option`/`sources`/
   `decision.*` carried to the UI. **`inspectable` is NOT read here** - an emit-only rule is
   proposed, recommended and armed exactly like any other.

2a. **Scan-set narrowing** (W16). `inspectable_rules_for_scan`
   (`crates/server/src/onboard.rs:944-961`) drops every selected rule whose
   `Rule::is_inspectable()` is `false`, and `audit_repos` SHADOWS its own `selected` binding
   with the result (`onboard.rs:1254-1255`) so the exclusion is total for everything
   downstream. Everything that follows in this list therefore operates on the narrowed set;
   the EMIT path (stage 7) does not. See section 3.6a.

3. **Directive resolution**. Each selected rule becomes a `SelectedRule { id, directive, repos }`
   (`onboard.rs:877-892`). The directive is the chosen option's `directive`, else the
   `default_option`'s, else the rule `summary`/`title`
   (`inspect_cmd.rs:171-181`, `server/src/lib.rs:8663-8676`, `server/src/lib.rs:15083-15088`).

4. **Scan passes** (`ai_audit.rs`, `onboard/audit.rs`, `scan_tools.rs`,
   `onboard/architectural.rs`). Deterministic floor checks, external-tool previews
   (`enforcement = "mechanical"` + a routable `[[sources]].linter`, minus `layer3_only`), the
   architectural checkers, the semantic AI pass (EVERY selected code-auditable rule, as of W4,
   `onboard.rs:975-1048`), and the alternatives/recommendation pass for multi-option semantic
   rules (`onboard/audit.rs:407-414`, `ai_audit.rs:6681-6835`). Fields consumed: option
   `directive`/`why`/`label`/`violates`, rule `kind`, `stack_exceptions`, `layer3_only`,
   `sources.linter`, `escalation`.

5. **Kind assignment and informational emissions** (`ai_audit.rs:7206-7230`). Model-reported
   rows are `Defect` at parse time (`ai_audit.rs:626-630`); `assign_deviation_kinds`
   reclassifies rows under a decided non-`violates` option as `Deviation`
   (`ai_audit.rs:6355-6425`); `ConventionObserved`, `NoConvention`, and `NotAssessed` rows are
   synthesized (`ai_audit.rs:6427-6602`).

6. **Export** (`report_export.rs`, `xlsx_export.rs`). The severity floor/ceilings
   (`min_severity`, `max_severity`, `deviation_ceiling`) are applied here, at both exporters'
   partitions; `remediation`, `finding_headline`/`finding_detail`, `effort`, and `sources`
   resolve into the client artifacts (PDF, xlsx, `findings.json`). Bucket routing
   (`effective_bucket`, `report_export.rs:2607-2652`) decides whether a row is an action item,
   informational, or held for review.

---

## 2. Loading and parsing

- **Entry points.** `load_corpus` fails the whole load on the first bad file
  (`lib.rs:1474-1487`); `load_corpus_lenient` skips bad files and returns `(RuleSet,
  Vec<(PathBuf, RulesError)>)` (`lib.rs:1489-1507`).

- **A parse error is no longer always thrown away (W13), but only on the SCAN path.** This is
  the single most important correction to the first edition of this document, which said flatly
  that production discards the error list. Precisely:
  - **The scan path routes it.** `load_corpus_for_scan` (`server/src/lib.rs:5232`) replaces the
    old `load_corpus_lenient(..).await.0` idiom; `split_scannable_rules` /
    `split_scannable_rules_from` (`server/src/lib.rs:5123, 5142`) now return a 5-tuple whose
    fifth element is `Vec<CorpusLoadFailure>` (struct at `server/src/lib.rs:5184`), and
    `record_corpus_load_failures` (`server/src/lib.rs:5283-5308`) sends each failure to four
    places: a ledger entry via `ScanLedger::record_corpus_load_failure`
    (`scan_ledger.rs:494-520`) under the new `RuleTier::Unloaded` (`scan_ledger.rs:73`, label
    `"Rule files that failed to load"` at `scan_ledger.rs:106`); the integrity gate; ONE
    client-facing coverage note; and a `tracing::error!` carrying the path and the parser
    message. Wired at all three scan entry points: `server/src/lib.rs:5413/5497` (sync
    `onboard_audit`), `server/src/lib.rs:6433/6590` (`onboard_audit_start`), and
    `crates/cli/src/inspect_cmd.rs:374/432` (headless `camerata inspect`).
  - **The integrity gate fails on it.** `SkipClass::CorpusLoadFailed`
    (`integrity_gate.rs:95`) is deliberately NOT in `is_justified()`
    (`integrity_gate.rs:102-104`) - it can never be excused under any profile. Two trigger
    sites both emit the code `"corpus_load_failure"`: a rule that RAN but carries
    `CORPUS_LOAD_FAILED_NOTE` in its coverage note (`integrity_gate.rs:403-421`), and a rule
    that did not run and classifies as `CorpusLoadFailed` (`integrity_gate.rs:438-452`, routed
    away from `unjustified_skip` on purpose).
  - **The client sees one line, with no internals.** `tool_coverage_disclosures`
    (`report_export.rs:1349`, third arm at `report_export.rs:1367-1374`) renders exactly:
    "N rules in the rule library could not be loaded this scan and did not run; anything they
    would have reported is not reflected in this report." One line per scan, not per failure,
    and deliberately carrying neither the file path nor the parser's message.
  - **The proposal and alternatives paths still discard it.** `propose_corpus_rules` still
    binds `let (set, _errs) = ...` (`propose.rs:661`), as does the alternatives rescan handler
    (`server/src/lib.rs:4275`, in `rescan_alternatives_handler` at `server/src/lib.rs:4244`),
    and roughly eight further server call sites. So a rule whose file will not parse still
    vanishes silently from a PROPOSAL; it no longer vanishes silently from a SCAN. The
    remaining backstop for the proposal path is the test suite's `errors.is_empty()`
    assertions. See gotcha 10.

- **Unknown FIELD names no longer pass unnoticed (W14).** Serde's behaviour is unchanged:
  `RuleToml` still does not set `serde(deny_unknown_fields)`, "so future corpus fields are
  silently ignored rather than breaking the loader" (`lib.rs:556-558`), and the attribute is
  deliberately staying off for three recorded reasons (`lib.rs:2038-2062`): the gate below is
  strictly stronger, it keeps the check a BUILD failure rather than a runtime refusal, and
  fixture corpora under `crates/server/**` legitimately carry keys `RuleToml` does not declare.
  What changed is that `corpus_files_with_unknown_fields` (`lib.rs:2208-2290`) now walks the
  raw TOML and reports every key outside the declared vocabulary, at seven levels: rule level
  (`RULE_LEVEL_FIELDS`, `lib.rs:2070-2097`), `[[option]]` (`lib.rs:2101-2112`), `[decision]`
  (`lib.rs:2115`), `[[sources]]` (`lib.rs:2149`), `[verified]` (`lib.rs:2152`),
  `[[stack_exception]]` (`lib.rs:2155`), and an option's `escalation` inline table
  (`lib.rs:2158`). The build gate is `req_w14_no_corpus_rule_carries_an_unknown_field`
  (`crates/rules/tests/w14_corpus_schema_gates.rs:435`), with a fire proof at five nesting
  levels (`:455`). Measured: zero offenders over every rule file; 865 before `layer`, `[[emits]]` and
  `sources.qualifies` were declared as accepted-but-unconsumed (`lib.rs:2196-2199`). A file
  that will not parse as TOML at all is skipped quietly here (`lib.rs:2217-2219`); that is the
  other gate's finding. Gotcha 9 is therefore now a much narrower hazard than it was.

- **Blank-string scrubbing.** `load_one` filters empty/whitespace-only strings to `None` for
  option `remediation`/`finding_headline`/`finding_detail`/`effort` (`lib.rs:1664-1668`) and
  for rule `max_severity`/`min_severity`/`deviation_ceiling` (`lib.rs:1702-1704`) and
`last_reviewed` (section 3.21). Authoring
  `remediation = ""` is exactly equivalent to omitting the field.

- **File discovery.** Only `*.toml` files are read, recursively, sorted for deterministic load
  order (`lib.rs:1509-1555`). A rule file sitting directly in the corpus root (no subfolder)
  falls back to domain `"universal"` (`lib.rs:1596-1600`).

---

## 3. Rule-level fields

### 3.1 `id`

- **Type / required.** String, required and non-empty; an empty `id` is a hard
  `MissingField` load error even in lenient mode (that file is skipped) (`lib.rs:1569-1574`).
- **What the engine does with it.** The primary key for everything: `RuleSet::get_by_id`
  indexing (`lib.rs:1312`), finding attribution (`Finding::rule_id`), detector wiring
  (`mechanical_gate::detector_channel`, `mechanical_gate.rs:86-110`, matches gateway arms,
  arch-checker ids, and Semgrep coverage BY ID), suppression/waiver matching, and the
  export-layer corpus join (`report_export::corpus_rule_for`, `report_export.rs:1649-1659`).
- **Prefix semantics are load-bearing.** Three id-prefix checks change behavior by identity:
  - `ORCH-` / `SPIRIT-` / `PROC-` mark a rule governance/process rather than code
    (`onboard/audit.rs:357-359`, `is_code_auditable_rule`). As of W4 this NO LONGER keeps a
    rule out of the semantic prompt (that exclusion was removed; see the long history note at
    `onboard.rs:975-1026`); it still excludes the rule from the ALTERNATIVES pass via
    `is_semantic_multi_option_rule` (`onboard/audit.rs:407-414`).
  - `ARCH-OBSERVABILITY-` routes every finding of the rule to the informational bucket and
    caps its severity at low, BY RULE IDENTITY, regardless of model output
    (`server/src/lib.rs:5617, 5639-5641`; consumed at `ai_audit`'s D8 ceiling pass and FIRST
    in `report_export::is_informational`, `report_export.rs:2928-2945`). This is gotcha 7.
  - `AI-` prefixed ids are invented by the model for novel findings (`ai_audit.rs:642`);
    they never resolve in the corpus and ride the P3 citation-class fallback instead.
- **Client-facing?** Yes: rule ids appear in every artifact (PDF citations, xlsx columns,
  `findings.json`).

### 3.2 `title`

- **Type / required.** String, required (serde will fail the file without it,
  `lib.rs:562`).
- **What the engine does with it.** Human label everywhere: proposal rows
  (`propose.rs:749`), the xlsx `rule_title` fallback join (`xlsx_export.rs:654-659`), the
  CONVENTIONS/AGENTS emit header (`arm.rs:95-99` renders `### {id} - {title}`), and the
  final fallback for both `summary` (`lib.rs:1620-1633`) and a selected rule's directive when
  no option resolves (`inspect_cmd.rs:171-181`). Reaches every client artifact.

### 3.3 `enforcement`

- **Type / required.** Required enum: `"prose" | "structured" | "mechanical" |
  "architectural"` (`lib.rs:70-77`). An unrecognized value fails the FILE's parse (strict
  enum), which in production lenient loads silently drops the rule (gotcha 10).
- **What the engine does with it, per consumer:**
  - **Scan-time preview.** Only `Mechanical` rules are grouped to an external preview tool;
    `Architectural` is explicitly skipped ("no off-the-shelf linter") and `layer3_only` is
    skipped (`scan_tools.rs:487-520`, the check at `scan_tools.rs:503`).
  - **Alternatives-pass eligibility.** `is_ci_enforced()` (`Mechanical` or `Architectural`,
    `lib.rs:108-113`) EXCLUDES a rule from the alternatives pass regardless of option count
    (`onboard/audit.rs:407-414`): a deterministic detector answers one behavior, so a wording
    choice is moot.
  - **Semantic prompt.** NOT consulted. Since W4, CI-tier enforcement does NOT keep a rule
    out of the AI prompt (`onboard.rs:975-1048`); the model is a deliberate second pair of
    eyes over deterministic rules too.
  - **Informational stance demotion.** `Structured` + a domain in `STANCE_LAYER_DOMAINS`
    (`universal`, `api-layer`, `fullstack`, `ui`, `javascript`, `integration`) + an
    absence-type finding (`located == false`) at medium-or-below routes the finding to the
    informational appendix (`report_export.rs:2871-2872, 2959-2968`).
  - **Build gates.** `mechanical_rules_missing_detector` only inspects `Mechanical` rules
    (`mechanical_gate.rs:144-157`); the effort-band gate only inspects CI-tier rules
    (`lib.rs:4270-4300`).
  - **Proposal placement string.** Four honest placement descriptions, one per tier
    (`propose.rs:724-737`); the UI "kind" collapses to `mechanical` vs `review`
    (`propose.rs:716-720`).
  - **Emit.** `arm.rs:88-92, 100-105` adds a `_Conformance:_` line only for CI-tier rules.
    `EnforcementKind::emits_to_conventions` (`lib.rs:117-119`) has NO PRODUCTION consumer
    anywhere in the workspace as of 2026-10-14 (re-checked by grep; its only callers are five
    assertions in `lib.rs`'s own test module at `lib.rs:3492-3495, 3576`). The live
    CONVENTIONS/AGENTS partitioning is done by `arm.rs`'s own string-level `is_ci_tier`
    (`arm.rs:88`), not by this method. See section 11.
- **Default / absent.** No default; the field is required.

### 3.4 `domain` (in-file) and the folder-derived domain

- **The folder path is the real domain.** `Rule::domain` is DERIVED from the TOML's parent
  folder relative to the corpus root, components joined with `:` (e.g.
  `rust/dioxus/x.toml` -> `"rust:dioxus"`, `universal/x.toml` -> `"universal"`)
  (`lib.rs:1576-1603`).
- **The in-file `domain` field is nearly inert.** When present it is only COMPARED against the
  derived value; disagreement logs a `tracing::warn!` and the derived value always wins
  (`lib.rs:1605-1618`). `"*"` is normalized to `"universal"` for that comparison only. Moving
  a file between folders changes its domain even if the field says otherwise. Keep the field
  accurate for human readers, but know it decides nothing.
- **What the derived domain gates.** Whether the rule can ever be SUGGESTED for a repo:
  `propose_corpus_rules` matches a repo when its stack's domain set contains the rule's
  `domain` OR any of its `extra_domains` (`propose.rs:685-692`); `domain == "universal"`
  matches every repo (`propose.rs:677`). A rule whose domain NO stack can emit can never be
  suggested, never auto-armed, and therefore never runs on any repo, ever - the A6 defect
  class (94 rules, 23 percent of the corpus at the time, were in that state). Two build gates now enforce reachability:
  `req_h1_every_corpus_domain_is_emittable_by_some_stack` (`propose.rs:843-876`, checking the
  corpus against `emittable_domains()`, `propose.rs:497`) and
  `req_a6_every_corpus_domain_has_a_detectable_witness_repo` (`propose.rs:1041-1081`, proving
  each domain is reachable end-to-end from a synthetic repo through `detect_stack` ->
  `domains_for_stack`).
  - Some domains are pipeline-selected WITHOUT a stack match at all
    (`PIPELINE_SELECTED_DOMAINS`, `propose.rs:331-355`), and child domains expand to parents
    (`expand_parent_domains`, `propose.rs:478`).
- **Downstream consumers of the domain string.** The stance-layer informational demotion
  (`report_export.rs:2871-2872`); `role_from_corpus` / `select_for_domains` for fleet role
  construction (`lib.rs:2721-2767, 2663-2719`, used by `crates/fleet/src/lib.rs:88` and
  `crates/cli/src/live_demo.rs:58`); and `domain_to_glob` (`lib.rs:2782-2814`), which maps a
  domain to the role's allowed-path glob - an unmapped domain falls back to `**` with a
  warning. **When adding a new stack rulepack you must wire all three: stack detection in
  `propose.rs`, `domains_for_stack`, and `domain_to_glob`** - the Supabase pack originally
  shipped without this wiring and its rules were orphaned (never proposed).
- **`domain == "agentic"` is special-cased:** always `recommended` in proposals regardless of
  stack (`propose.rs:771, 784`).

### 3.5 `extra_domains`

- **Type / default.** `Vec<String>`, absent -> empty (`lib.rs:612-614`). 5 rules used it on
  2026-10-11.
- **What the engine does with it.** A second, explicit domain-match axis in
  `propose_corpus_rules` ONLY: a repo matches when its domain set contains the rule's folder
  domain OR any entry here (`propose.rs:685-692`). Lets a rule whose premise generalizes
  beyond its home folder (e.g. a `supabase:database-functions` rule that is really about any
  Postgres `SECURITY DEFINER` function, carrying `extra_domains = ["sql"]`) arm on a broader
  stack without moving the file out of its corpus family (`lib.rs:936-944`).
- **Not consumed anywhere else.** It does not affect `select_for_domains`, roles,
  `domain_to_glob`, or the stance-layer check - those read only the folder-derived `domain`.

### 3.6 `kind` (the RuleKind taxonomy)

The **four-way** classification of how a multi-option rule's alternatives relate to the rule's
underlying requirement. Orthogonal to `enforcement` (which classifies HOW a rule is checked,
not what its alternatives MEAN). The enum is at `crates/rules/src/lib.rs:167`, with
`#[serde(rename_all = "snake_case")]` as the only renaming, so the TOML spellings are exactly
`"defect"` / `"decision"` / `"default_with_exceptions"` / `"decision_with_floor"`
(restated in `RuleKind::as_str`, `lib.rs:279-286`). `RuleKind::ALL` (`lib.rs:271-276`) is a
`[RuleKind; 4]`, so adding a variant without extending it is a compile error. The source doc
comment at `crates/rules/src/lib.rs:132-164` is authoritative; quoting it (dashes rendered as
hyphens):

> This is the direct fix for the shipped defect (A5, `ai_audit.rs` ~5885/~5921-5929): the
> alternatives pass used to pick whichever option the repo follows and REPLACE the rule's
> effective directive with it, so a repo that correctly follows the degenerate/absent form of
> a requirement reported ZERO findings - "consistently incorrect" read as "compliant" because
> the engine had no way to tell a genuine architectural alternative apart from the defect
> itself. `RuleKind` is that missing distinction:
>
> - `RuleKind::Defect` - there is NO acceptable alternative anywhere; every option other
>   than the correct one describes the absence or a degenerate form of the requirement
>   (see `RuleOption::violates`). A `defect`-kind rule NEVER enters the alternatives pass -
>   it behaves exactly like a single-directive rule, because there is nothing to decide among.
> - `RuleKind::Decision` - the alternatives genuinely hold EQUAL weight (e.g. API
>   versioning scheme, ID representation, package-by-feature vs. package-by-layer). The only
>   reportable condition is INCONSISTENCY across the codebase, never "the repo picked option
>   B" by itself. No option on a `decision`-kind rule should carry `violates = true`.
> - `RuleKind::DefaultWithExceptions` - one option is correct UNLESS justified; deviating
>   is a finding even when done consistently. Some non-default options may be legitimate,
>   justified alternatives (`violates = false`); others may be the degenerate/absent form of
>   the requirement wearing the same `[[option]]` shape (`violates = true`) - both can coexist
>   on the same rule.

The fourth variant, `RuleKind::DecisionWithFloor` (`lib.rs:263`, doc comment `lib.rs:179-262`),
was added by W17 for the shape the three-way taxonomy had no slot for: SEVERAL options hold
genuinely equal weight (no privileged default; the choice follows from project context) AND the
rule separately names shapes that are faults under every one of those options. Classifying such
a rule `decision` forces deleting real fault flags; classifying it `default_with_exceptions`
fabricates a default that does not exist. The first edition of this document recorded that gap
as open question 7; it is now closed.

**Two exhaustive predicates are the taxonomy's public API**, and both deliberately have NO
catch-all `_` arm, so a fifth variant could not inherit an answer by accident:

| Kind | `permits_violates_flag()` (`lib.rs:308-317`) | `offers_no_convention_escape()` (`lib.rs:332-339`) |
|---|---|---|
| `Defect` | true (documentation + gate signal only; never executed) | false |
| `Decision` | **false** | **true** |
| `DefaultWithExceptions` | true | false |
| `DecisionWithFloor` | true | **false** |

The matrix above is pinned as a test, `req_w17_the_four_kinds_pipeline_contract`
(`crates/rules/tests/w17_decision_with_floor_kind.rs:303`), including a
`matrix.len() == RuleKind::ALL.len()` check at `:309-313`.

**Read the `DecisionWithFloor` row carefully: the asymmetry is deliberate, not an oversight.**
The new kind permits `violates` flags but is DENIED the no-convention escape, even though its
genuine options are equal-weight exactly like a `decision` rule's. Granting it the escape would
let a repo split between one legitimate option and the rule's own named fault answer "no
convention adopted" - and the fault would be buried inside an informational row that asserts
nothing is wrong. A `decision` rule has no named fault to bury, which is why it keeps the
escape. The doc comment states the same reasoning at `ai_audit.rs:2302-2310`.

**Where the predicates are consumed.** `offers_no_convention_escape()` has exactly two
production call sites, deliberately so the prompt and the parser cannot drift apart: the prompt
marker in `build_alternatives_block` (`ai_audit.rs:2311`) and the claim-honoring conjunction in
`parse_alternative_recommendations` (`ai_audit.rs:2516-2520`). `permits_violates_flag()` has
exactly one: the scope test of corpus-load GATE 1,
`options_with_rejected_prose_but_no_violates_flag` (`lib.rs:1923`), where a kind that permits
the flag stays IN scope and an absent kind deliberately also stays in scope.

**Membership.** `RUST-DOMAIN-6` is the sole `decision_with_floor` rule in the corpus
(`crates/rules/principles/rust/rust-domain-6-shared-repository-error.toml:6`). That is asserted
as an EQUALITY, not a lower bound: `req_w17_decision_with_floor_has_exactly_one_member`
(`crates/rules/tests/w17_decision_with_floor_kind.rs:212`, roster constant at `:53`) fails the
build if a second rule is added without recording the self-evidence. The candidate sweep that
produced the population of one is itself pinned by
`req_w17_the_decision_with_floor_candidate_sweep_still_holds` (`:239`), which asserts the 17
near-miss `decision` rules stay `decision` and 8 examined-and-rejected rules stay out.

**`RuleKind` never reaches a client artifact.** It is not on `RuleOptionView`
(`onboard.rs:383`), not in `camerata-api-types`, and not in the UI; `RuleAlternatives`
(`ai_audit.rs:45`, `kind` field at `:65`) is not even `Serialize`. The only downstream trace of
a kind decision is the `no_convention` boolean on `RuleRecommendation` (`ai_audit.rs:145`) and
the `FindingKind::NoConvention` row it produces.

**What each value functionally does, consumer by consumer:**

- **`kind = "defect"`:**
  - `Rule::is_defect_kind()` (`lib.rs:1185-1187`) makes `is_semantic_multi_option_rule` return
    `false` regardless of option count (`onboard/audit.rs:400-414`), so the rule NEVER enters
    the alternatives/recommendation pass. Its directive is never rewritten; the per-chunk
    violation pass checks the rule's selected/default option directive exactly like a
    single-directive rule. Its findings stay `FindingKind::Defect` (assigned at parse time,
    `ai_audit.rs:626-630`).
  - Consequence: on a `defect`-kind rule, `violates` flags, the `[PURE DECISION]` marker, the
    not-applicable drop, and the no-convention escape are all unreachable at runtime. The
    `violates` flags remain valuable as authored documentation and as the gate signal
    (section 10.4), but nothing executes them.
  - A `defect`-kind rule also never produces `Deviation`, `ConventionObserved`, or
    `NoConvention` rows (all three are keyed off the recommendation records the alternatives
    pass produces, `ai_audit.rs:6355-6568`). `deviation_ceiling` is therefore inert on it
    (gotcha 6).
- **`kind = "decision"`:**
  - Eligible for the alternatives pass (when also non-CI-tier, non-gate-armed,
    code-auditable, and >= 2 options; `onboard/audit.rs:407-414`).
  - The ONLY kind offered the no-convention escape: `build_alternatives_block` appends the
    `[PURE DECISION - "no convention" is a valid answer]` marker exclusively when
    `kind == Some(Decision)` (`ai_audit.rs:2302-2311`), and
    `parse_alternative_recommendations` honors a `no_convention` claim ONLY for a
    `Decision`-kind rule AND only when backed by a validated observed-options breakdown or a
    real evidence citation (`ai_audit.rs:2509-2536`); an unbacked or wrong-kind claim is
    corrected and flagged `hallucinated` (`ai_audit.rs:2616-2655`).
  - **No option may carry `violates = true`, and this is now GATED.** The first edition of this
    document recorded the invariant as stated-but-unenforced; W14 closed that.
    `decision_kind_rules_with_violates_flag` (`lib.rs:2007-2023`) collects every offending rule
    as `"<rule id>: [<option ids>]"`, and
    `req_w14_no_decision_kind_rule_carries_a_violates_flag`
    (`crates/rules/tests/w14_corpus_schema_gates.rs:358`, fire proof at `:380`) fails the
    build. A vacuity guard, `decision_kind_rule_count` (`lib.rs:2031`), keeps the gate from
    passing trivially if the corpus ever held zero decision rules.
  - The reason the gate is worth having is that the flag is LIVE at runtime regardless of kind:
    `apply_alternative_decision` (`ai_audit.rs:6279-6330`) branches purely on `option.violates`
    and never consults `kind`, so a stray flag on an equal-weight rule silently produces a
    guaranteed high-severity `Defect`.
- **`kind = "default_with_exceptions"`:**
  - Eligible for the alternatives pass on the same conditions; NOT offered the no-convention
    escape (an externally-correct default exists, so "no convention" is not an honest
    outcome; `ai_audit.rs:2302-2311`).
  - Expected to mix `violates = false` (legitimate justified alternatives) and
    `violates = true` (degenerate forms) options. A DWE rule with options and ZERO `violates`
    flags is flagged by `suspicious_default_with_exceptions_rules` (`lib.rs:1837-1849`) as
    either misclassified (should be `decision`) or under-authored - this is advisory output
    for review, not a build failure by itself (its doc comment says callers that want it
    enforced assert on it directly). `DecisionWithFloor` is deliberately NOT swept by this
    gate (`lib.rs:1828-1835`): its premise is that no single option is privileged, so the
    "exceptions" reasoning does not apply.
- **`kind = "decision_with_floor"`:**
  - Eligible for the alternatives pass on the same conditions as the other two multi-option
    kinds. Pipeline-wise it behaves exactly like `default_with_exceptions`: flags permitted,
    no-convention escape withheld. **It required no new pipeline behaviour at all** - only a
    variant, two predicate arms, gate scoping, and the classification itself. That is why the
    first edition could predict the mechanics would support it trivially.
  - The honest authoring shape: at least two unflagged options that are genuinely equal
    alternatives, at least one `violates = true` option naming the fault, and
    `default = false` (there is no adopted default to name). `RUST-DOMAIN-6` satisfies all
    three, asserted by `req_w17_rust_domain_6_is_decision_with_floor`
    (`crates/rules/tests/w17_decision_with_floor_kind.rs:138`).
  - Consequence worth stating plainly: on this kind, a repo that is split between two
    legitimate options reports DEVIATIONS (there is no no-convention exit), and a repo that
    sits on the named fault reports a `Defect`. Both outcomes are intended.
- **Absent `kind`:**
  - Parses fine (`Option<RuleKind>` with a bare `serde(default)`, `lib.rs:615-625` - the doc
    comment explains an absent kind must NOT silently resolve to a permissive value like
    `Decision`), but the corpus-load gate `rules_missing_kind` (`lib.rs:1726-1734`, asserted
    by `req_a5_1_every_corpus_rule_declares_a_kind`) fails the BUILD on any corpus rule with
    `kind == None`. At runtime, `is_defect_kind()` is conservatively `false` for `None`
    (`lib.rs:1176-1187`), so an unclassified rule WOULD enter the alternatives pass if it ever
    reached a scan - the gate is what prevents that.
  - A typo'd kind VALUE (`kind = "defct"`) is a different failure: the enum deserialization
    fails, the whole file fails to parse, and the lenient production loader drops the rule
    entirely (gotcha 10) - `rules_missing_kind` cannot catch a rule that never loaded. On the
    SCAN path W13 now records that file as a corpus-load failure (section 2); on the proposal
    path the `errors.is_empty()` assertions in the test suite are still the only net.

### 3.6a `inspectable`

The answer to one question: **can a scan of a repository say anything true about this rule at
all?** Added by W16. It is the only field that separates the EMIT role of a rule from its SCAN
role.

- **Type / required.** `Option<bool>` on both `RuleToml` (`lib.rs:634-635`) and `Rule`
  (`lib.rs:994`), with a bare `#[serde(default)]`. The pairing is the same lenient-parse /
  strict-gate split `kind` uses: a TOML lacking the key still PARSES (fixture corpora in other
  crates legitimately omit it), and a separate build gate refuses it for the real corpus. There
  is deliberately no value-level default resolving `None` to `true` at the parse layer
  (`lib.rs:631-633`).
  - **Required in the sense that matters:** `rules_missing_inspectable` (`lib.rs:1756-1764`),
    asserted by `req_w16_every_corpus_rule_declares_inspectable`
    (`crates/rules/tests/w16_inspectable_gate.rs:286`), fails the BUILD on any corpus rule that
    omits it. The failure message states the reason both directions are unsafe: guessing `true`
    "ships an unassessable rule as 'verified clean' in a paid deliverable", guessing `false`
    "silently stops a real check from running". A fire proof at `:303` confirms the gate trips.
  - **At runtime an absent value resolves to `true`.** `Rule::is_inspectable()`
    (`lib.rs:1200-1202`) is `self.inspectable.unwrap_or(true)` - the fail-safe direction is to
    keep looking, not to go quiet. Pinned at `w16_inspectable_gate.rs:327-332`.
  - The key is in `RULE_LEVEL_FIELDS` (`lib.rs:2089`), so the unknown-field gate accepts it.

- **The single consumer.** `inspectable_rules_for_scan`
  (`crates/server/src/onboard.rs:944-961`, doc at `:894-943`; the `is_inspectable()` call is
  `:956`) drops every emit-only rule from the selected set, and `audit_repos` SHADOWS its own
  `selected` binding with the result (`onboard.rs:1254-1255`). That shadow is the whole
  mechanism: one line, and everything computed afterwards inherits the narrowing. There is no
  second read of the flag anywhere in the engine.

- **What the narrowing reaches, because it happens BEFORE arming rather than after:**
  `rules_fingerprint` (`onboard.rs:1261-1263`), `provenance.audited_rule_ids`
  (`onboard.rs:1268`, stamped at `:2090`), `repo_selected_ids` (`onboard.rs:1389-1393`, so the
  architectural checkers never arm either), and the semantic prompt set, whose own doc notes it
  does NOT re-filter (`onboard.rs:977-981`). Excluding before arming rather than suppressing
  findings afterwards is deliberate: a rule that armed and was then suppressed still pollutes
  the ledger's accounting, which is the same defect shape W16 closed.

- **Where an emit-only rule still appears, deliberately:**
  - `AGENTS.md`, `CONVENTIONS.md` and `.camerata/rules.json`, verbatim. The emit path
    (`crate::arm`) does not read the field at all - `arm_files_for_repo` (`arm.rs:141`) never
    mentions it. Pinned end to end by
    `crates/server/tests/w16_emit_only_rules_e2e.rs:307-386`.
  - The onboarding PROPOSAL, still recommended and still auto-recommended
    (`w16_emit_only_rules_e2e.rs:181-194`). `inspectable` is not a selection control.

- **Where it does NOT appear, each established from the code path:**
  - **The scan ledger: no entry at all.** Every `record_rule` call in `audit_repos` iterates a
    set derived from the post-filter `selected`; the only calls keyed off something else loop
    the hardcoded 8-rule `AUDIT_RULES` content floor (`onboard.rs:70-83`), none of which is
    emit-only. Asserted at `w16_emit_only_rules_e2e.rs:259-262` - "not a clean one, not an
    excluded one".
  - **"Verified clean": impossible by construction.** `RuleLedgerEntry::verified_clean()`
    (`scan_ledger.rs:183-185`) is `ran && findings_emitted == 0 && !not_applicable`; with no
    ledger entry there is nothing to evaluate. Both paths into the PDF's "What's healthy" are
    closed: the ledger path (`report_export.rs:4375-4376` via
    `ScanLedger::healthy_rule_ids`, `scan_ledger.rs:638-648`) and the pre-ledger
    `audited_rule_ids` fallback (`report_export.rs:4378`), which was already filtered upstream.
    Asserted at `w16_emit_only_rules_e2e.rs:263-267` and, through the real export, `:291-295`.
  - **The Coverage sheet and the "we checked N rules" line.** `build_coverage_rows`
    (`xlsx_export.rs:928`) maps directly over `audited_rule_ids` (`:933-935`), so
    `coverage_rows.len()` IS that count; `coverage_summary_line` (`xlsx_export.rs:1466`) reads
    `coverage_rows.len()` at `:1467`. The PDF's per-category "Rules checked" column is the same
    fact, scoped (`report_export.rs:4319-4323`, rendered at
    `crates/server/templates/audit_report.typ:426`).
  - **"Rules not run this scan": also no.** `ScanLedger::excluded_rules`
    (`scan_ledger.rs:666`) filters `!r.ran` over ledger entries, and there is no entry.
    Asserted at `w16_emit_only_rules_e2e.rs:268-271` - "it was never a scan rule".
  - **The integrity gate: no, derivatively.** `integrity_gate::evaluate`
    (`integrity_gate.rs:334`) reads only the `ScanLedger`, so a rule with no entry is outside
    its universe. **This one is an inference from the ledger answer plus the function's input
    type; no test asserts it directly.**

- **Population.** 26 rules were `inspectable = false` on 2026-10-14, and every rule declares the
  field. By folder: `agentic/` 16, `process/` 4, `universal/` 4, `ci-cd/` 1, `permissions/` 1.
  Note the spread - `PROC-*` ids appear in three different directories and the `SPIRIT-*` and
  `ORCH-*` families are MIXED, which is exactly why a lexical prefix rule could not have done
  this job and the flag had to be per-rule. The population is pinned **by id list**, not by a
  number: `EMIT_ONLY_RULES` (`crates/rules/tests/w16_inspectable_gate.rs:52`, 26 `(id, reason)`
  entries) and `req_w16_the_emit_only_population_is_pinned` (`:366`). Its complement
  `DELIBERATELY_SCANNABLE` (`:182`, 11 ids including `SPIRIT-FILE-SIZE-1`,
  `PROC-PR-CONCURRENCY-1` and `PROC-HIDE-DEAD-END-1`) is pinned at `:393`, and the two are
  asserted disjoint at `:409`. There is no `assert_eq!(.., 26)` and no assertion on the total
  rule count anywhere; the nearest numeric guard is `files.len() >= 400`
  (`crates/rules/tests/w17_tag_field_deleted.rs:95-98`).

- **The client-visible effect, stated honestly.** W16's own write-up reports the "rules
  checked" count on a TypeScript stack dropping from 167 to 145 (a drop of 22). **The mechanism is certain and
  the exact arithmetic is not reproducible from static reading**: a delta of 22 is not 26, so
  that stack selected only 22 of the 26 emit-only rules, and which 22 depends on the proposal
  pipeline running against that repo (`ALWAYS_ON_DOMAINS`, `propose.rs:329`, and
  `PIPELINE_SELECTED_DOMAINS`, `propose.rs:339`, together cover `agentic` + `universal` +
  `process` = 24 unconditionally, leaving `ci-cd` and `permissions` stack-dependent). Treat 167
  and 145 as one measured run, not as a derivable constant.

- **Authoring guidance.** Set `inspectable = false` when nothing in a file tree could confirm
  or refute the rule - it governs how an agent or a team WORKS, not what the artifact looks
  like. Set it `true` whenever the repo carries a real trace, even a partial one; the rule then
  behaves exactly as it did before this field existed. If you are unsure, `true` is the
  reversible choice: an over-scanned rule produces a finding someone can dispute, while an
  under-scanned one produces silence nobody notices.

### 3.7 `default`

- **Type / default.** Boolean, absent -> `false` (`lib.rs:574-577`).
- **What the engine does with it.** It is a GATE on `[decision].default`, nothing more:
  `load_one` only populates `Rule::default_option` when `default = true` AND
  `[decision].default` names a non-empty option id (`lib.rs:1634-1644`). With
  `default = false` (or absent), any `[decision].default` in the file is silently discarded.
- **Everything that then hangs off `default_option`:**
  - `Rule::resolved_option(chosen)` resolves `chosen_option` first, then `default_option`,
    else `None` (`lib.rs:1116-1124`). That resolution feeds:
    - the Fix text (`report_export::resolve_fix`, `report_export.rs:2038-2060`),
    - the authored floor headline/detail (`resolve_floor_finding_text`,
      `report_export.rs:2107-2131`),
    - the authored effort band (`resolve_effort`, `report_export.rs:3371-3382`),
    - the active escalation spec (`Rule::selected_escalation`, `lib.rs:1143-1147`, consumed at
      `server/src/lib.rs:2138-2161` and `test_tamper.rs:169`),
    - the default directive an untouched scan checks
      (`inspect_cmd.rs:171-181`, `server/src/lib.rs:8663-8676`),
    - the alternatives pass's "currently selected" marker and fallback target
      (`onboard/audit.rs:455-458`, `ai_audit.rs:2317-2322, 2469-2475`).
  - The remediation corpus gate `every_rule_with_a_default_option_has_authored_remediation`
    (`lib.rs:3180-3202`) only applies to rules WITH a resolvable default option; a rule with
    no default is deliberately out of scope ("no finding ever resolves a Fix line for such a
    rule until a project explicitly configures a `chosen_option`").
- **The semantics in words.** `default = true` means "this rule ships an adopted default
  option"; `false` means "the architect MUST choose an alternative at onboarding"
  (`lib.rs:574-576, 910-912`). **`default` is orthogonal to `kind`:** it governs onboarding
  adoption, not option semantics, so `default = false` on a `default_with_exceptions` rule is
  a legitimate authoring choice, not a contradiction. The first edition of this document
  claimed otherwise and recommended a gate for it; both were retracted. `default = false` is
  typically paired with `opt_in_only = true` for a rule whose right answer depends on
  project-specific infrastructure the architect must consciously choose (`lib.rs:3446`). See
  gotcha 2 for the full retraction and for the heuristic that survived it.
- **Functional cost of a missing default on a multi-option rule.** Until a project persists a
  `chosen_option`, `resolved_option(None)` is `None`, so findings under the rule ship with NO
  Fix line, NO authored floor text, NO authored effort band, and NO active escalation - every
  one of those joins degrades to its honest-gap fallback.

### 3.8 `verification`

- **Type / default.** Enum `"draft" | "policy" | "grounded" | "verified" | "needs_recheck"`;
  absent -> `Draft` (`lib.rs:395-430`, `serde(default)` at `lib.rs:594-597`).
- **What the engine does with each value.** The ONLY runtime consumer that changes scan
  behavior is `Rule::is_auto_recommended()` (`lib.rs:1270-1276`):
  `Grounded | Verified | Policy` -> auto-recommended (pre-checked in the proposal AND selected
  by an unattended scan via `curated_rule_selection`, `inspect_cmd.rs:155-165`);
  `Draft | NeedsRecheck` -> listed in the proposal but NOT pre-checked and NOT run unattended.
  - `Policy` was ADDED to the auto-recommended set in Pass-1 W4 (finding A8): provenance
    ("this encodes a project decision, no external authority") is not a statement about
    whether the rule is worth running; excluding it silently disarmed 17 shipped rules on
    every repo, including `SPIRIT-FILE-SIZE-1` (`lib.rs:1258-1269`). 25 corpus rules carry
    `policy` and all 25 arm.
  - **The source comment describing `Policy` was itself wrong until W17, and believing it did
    real damage.** `Verification::Policy`'s doc comment (`lib.rs:399-421`) used to read "kept
    out of the armed ruleset like `draft`, but labeled honestly rather than as `grounded`" -
    a claim that had been FALSE since the A8 fix. It is corrected in place and now says, in
    the source's own words: "NOT grounded ... but it DOES arm." Two sibling comments carrying
    the same falsified claim were corrected with it: the enum-level verification-ladder doc
    (`lib.rs:364-376`, explicit retraction at `:372-374`) and `Verification::is_shippable`
    (`lib.rs:474-478`). If you encounter an older copy of either the source or this reference,
    resolve it against `is_auto_recommended`'s BODY (`lib.rs:1270-1276`), which reads
    `Grounded | Verified | Policy`. Pinned by
    `crates/rules/tests/w17_policy_arms_the_rule.rs`.
  - `is_grounded()` / `is_shippable()` still return `false` for `Policy` and `Draft`
    (`lib.rs:467-480`) - those answer the provenance question and feed the citation story,
    not arming. What `policy` costs a rule is a weaker CITATION story, not a weaker arming
    story.
- **Absent or stale.** Omitting `verification` entirely defaults to `Draft`, which disarms the
  rule EVERYWHERE, FOREVER (a verification value is repo-independent). This is exactly what
  shipped as finding A7: `UI-UTC-DATES-1` omitted the field, defaulted to `Draft`, and never
  armed on any repo while being the only `ui` rule with a shipped deterministic detector
  (`lib.rs:1789-1806`). Gotcha 4.
- **The gate.** `rules_disarmed_by_verification` (`lib.rs:1807-1819`), asserted by
  `req_h1_no_corpus_rule_is_disarmed_by_its_verification_value`, now fails the BUILD on any
  corpus rule whose verification value makes `is_auto_recommended()` false. Practically:
  every shipped rule must be `grounded`, `verified`, or `policy`.
- **`verified` is human-only.** No automated process may ever emit it; `needs_recheck` is what
  the staleness pass demotes a drifted verification to (`lib.rs:424-429`).
- **Client-facing?** The string is carried on proposals (`propose.rs:754`) for the UI; it does
  not reach the PDF/xlsx.

### 3.9 `[[sources]]`

- **Type / default.** Array of `{ url, title, linter?, relevance? }` tables; absent -> empty
  (`lib.rs:507-523, 598-601`).
- **What the engine does with them:**
  - **Citations.** `resolve_citation` joins the finding's rule id to the corpus and cites
    sources VERBATIM when at least one has an external (`http(s)://`) URL - citation kind
    `"grounded"` (`report_export.rs:1661-1700`). Internal/non-URL sources are filtered out.
    A grounded citation is what keeps an AI-tier finding out of the uncited-AI held/informational
    routing (`is_uncited_ai_finding`, `report_export.rs:1993-2010`) and what ranks the row in
    the provenance-tier sort (`provenance_tier_rank`, `report_export.rs:2837-2843`). Titles
    and URLs reach the PDF, xlsx, and `findings.json`.
  - **Preview-tool wiring.** `sources[].linter` is how a mechanical rule gets a scan-time
    preview tool: `tool_for_rule` maps the first routable `linter` string through
    `tool_for_linter` (clippy/ruff/eslint-family/semgrep), with a fallback that claims Semgrep
    coverage by rule id even with no `linter` source (`scan_tools.rs:118-128, 148-192`).
    `selector_for_linter` derives the exact `--rule`/`--select` argument from the text after
    the colon (`scan_tools.rs:210+`). A `linter` string that is a prose description rather
    than a real rule id (e.g. `jest: testMatch default glob`) is deliberately not routed
    (`looks_like_lint_rule_id`, `scan_tools.rs:140-144`).
  - **Detector-channel accounting.** A linter source that `tool_for_linter` recognizes is
    channel 4 of `detector_channel` (`mechanical_gate.rs:58-110`), which decides whether a
    `mechanical` rule counts as having a wired detector for the build gate and the scan
    ledger.
  - **Grounding status.** Sources are what justify `verification = "grounded"`; the
    `verification` field, not the sources list, is what the engine reads for arming
    (section 3.8).
- **`relevance` is per-source provenance, and it is the ONLY place a per-source note goes.**
  Optional string on each `[[sources]]` block: what that source actually states, and how it
  bears on the rule (e.g. *"cmd/go documents the //go:build directive: custom tags specified
  via -tags flag at build/test time..."*). 19 sources across the 7 `go/testing/` rules carry
  one. **Parsed and reachable, but deliberately consumed by nothing today** - it is
  author/reviewer-facing provenance for re-verifying a citation without re-reading the source,
  not deliverable text. It does NOT reach the PDF, xlsx, `findings.json`, the proposal
  `RuleSourceView`, or `Rule::summary`, and it must never be concatenated into any of them.
  Added by W18; see the "artifact reach" note at the end of this bullet list.
- **`qualifies` inside a `[[sources]]` block is a real trap, it bit 10 rules, and it is now a
  build failure.** `SOURCE_FIELDS` (`lib.rs:2149`) still accepts a `qualifies` key at source
  level so that the gate rejecting it can give a placement message rather than a spelling one
  - but `RuleSource` does not carry it, so the text is parsed and discarded. Two shapes
  existed:
  - Ten `go/` rules had their authored *rule-level* `qualifies` written one level too deep;
    the rules loaded cleanly and their `summary` silently fell back to `[decision].why`
    (section 3.14). W17 fixed them by moving the text to rule level.
  - Nineteen more, across 7 `go/testing/` rules that already had a correct rule-level
    `qualifies`, were per-source annotations wearing a rule-level field's name. W18 migrated
    all 19 to `relevance`. The corpus now has ZERO nested `qualifies`.

  Three gates exist, narrowest first:
  `corpus_rules_losing_qualifies_text_to_sources` (build-failing; fires only when the
  misplacement actually COSTS the rule its authored summary),
  `corpus_rules_with_qualifies_misplaced_into_sources` (advisory roster; the only one that
  reports, per occurrence, whether the rule has a rule-level `qualifies`), and
  `corpus_rules_with_qualifies_nested_in_sources` (W18, build-failing on ANY nested
  occurrence - the general shape, which until W18 had no guard; the 19 lived in the gap
  between the other two). **Author `qualifies` at rule level and `relevance` at source level,
  always.**
- **Absent / malformed.** No sources -> citation kind degrades to `"preview"` (when a
  `preview_tool` produced the row) or `"advisory"` (`report_export.rs:1701-1720`); for an
  AI-tier finding that stays advisory after the class fallback, the row is EXCLUDED from
  `curated_findings` and routed held/informational (`report_export.rs:1993-2010, 2615-2621`).

### 3.10 `[verified]`

- **Type / default.** Optional table `{ by, at, against = [] }`; absent -> `None`
  (`lib.rs:538-558, 602-606`).
- **What the engine does with it.** Provenance record behind `verification = "verified"`: who,
  when, and the exact source/linter versions verified against, for a staleness pass (in
  `camerata-checks`) that demotes to `needs_recheck` on drift (`lib.rs:525-537`). Not consumed
  by the scan or export pipelines; not client-facing.

### 3.11 `opt_in_only`

- **Type / default.** Boolean, absent -> `false` (`lib.rs:578-584`). 13 rules on 2026-10-11
  (e.g. `CICD-CODEQL-SECURITY-SCAN-1`, `CICD-SEMGREP-SECURITY-SCAN-1`).
- **What the engine does with it.** Consumed ONLY in `propose_corpus_rules`: it strips both
  the `recommended` badge and the `is_auto_recommended` pre-check
  (`propose.rs:771, 784-786`), even when the rule is grounded and stack-relevant. The rule
  still appears in the proposal list for a deliberate human opt-in (`lib.rs:1151-1161`).
- **Functional consequence.** Because unattended scans select exactly the
  `is_auto_recommended` set (`inspect_cmd.rs:155-165`), an `opt_in_only` rule NEVER runs in
  an unattended scan. It can only run when a human ticks it.

### 3.12 `layer3_only`

- **Type / default.** Boolean, absent -> `false` (`lib.rs:585-590`). 6 rules on 2026-10-11.
- **What the engine does with it.** Marks a CI-tier rule too heavy to run at scan time or
  layer 2 (e.g. CodeQL's whole-program DB build). Consumed by `group_by_tool`, which drops
  the rule from the scan-time preview pass (`scan_tools.rs:503`; re-checked defensively in
  `run_scan_tools`, `scan_tools.rs:1196`). It does NOT affect selection, the semantic prompt,
  or export.
- **Honest caveat.** The intended "layer-3 runner" consumer ("Part B", `lib.rs:928-930`) is
  carried-for-the-future; today the flag's only executed effect is the preview exclusion.

### 3.13 `[[stack_exception]]`

- **Type / default.** Array of `{ framework, path_glob, note }` blocks; absent -> empty
  (`lib.rs:607-610, 1051-1082`). Exactly 1 rule used it on 2026-10-11
  (`fullstack/arch-monolith-first-1.toml`).
- **What the engine does with it.** A finding-SUPPRESSION mechanism for platform-idiomatic
  patterns: `Rule::stack_exception_for(frameworks, path)` returns the first exception whose
  `framework` is present in the DETECTED stack (case-insensitive match against
  `RepoStack::frameworks`) AND whose `path_glob` matches the finding's file path
  (`lib.rs:1126-1141`; glob semantics: `*` matches ANY run of characters including `/`, so one
  `*` behaves like `**` - `lib.rs:1091-1114`). `ai_audit::apply_stack_exceptions`
  (`ai_audit.rs:5564+`) drops matching findings, called from the scan pipeline with ledger
  accounting (`onboard.rs:1888-1903`, stage `stack_exception_excluded`).
- **`note` is internal documentation only** - never rendered client-facing (`lib.rs:1080-1082`).
- **Absent/malformed.** No exceptions -> the rule behaves exactly as before the field existed.
  An exception naming a framework the detector never emits simply never fires (there is no
  gate validating `framework` against the detectable-framework vocabulary; see section 11).

### 3.14 `qualifies` (and the derived `summary`)

- **Type / default.** Optional string (`lib.rs:568-571`), at RULE level. On 2026-10-11, 201 of the rules carried one.
- **What the engine does with it.** Source #1 for the derived `Rule::summary`:
  `qualifies` (non-empty) -> else `[decision].why` (non-empty) -> else `title`
  (`lib.rs:1620-1633`).
- **Author it at rule level or it is discarded.** A `qualifies` key written inside a
  `[[sources]]` block parses, passes the unknown-field gate, and is thrown away, so the rule
  falls through to `[decision].why` with nothing to signal the loss. Ten `go/` rules shipped
  in that state. See section 3.9 for the three gates that now catch it - since W18 ANY nested
  `qualifies` fails the build, whether or not it costs the rule its summary. If what you are
  writing is about the SOURCE rather than the rule, the field is `[[sources]].relevance`.
- **Where `summary` goes.** It is the FALLBACK directive text when a rule has no resolvable
  option: the arm/emit path (`server/src/lib.rs:8663-8676`), and the rule-drift diff's
  `resolved_directive` (`server/src/lib.rs:15083-15088`); it is also hashed into the
  rule-drift baseline (`server/src/lib.rs:15037`). It does not reach the PDF/xlsx findings
  artifacts.

### 3.15 `max_severity`

- **Type / default.** Optional lowercase severity string (`"critical" | "high" | "medium" |
  "low" | "info"`); absent -> no ceiling (`lib.rs:636-645, 995-1004`). Deliberately a string,
  not an enum, matching `Finding::severity`'s vocabulary. 11 rules on 2026-10-11
  (`rules_declaring_max_severity`, `lib.rs:2561-2578`, is the anti-inertness probe).
- **What the engine does with it.** `report_export::clamp_to_rule_max_severity`
  (`report_export.rs:498-515`) clamps a finding's severity DOWN to the ceiling when it ranks
  above it; it never raises. It is applied at BOTH export choke-points, and it has the last
  word in the ordinary chain:
  - PDF/JSON partition: normalize -> floor -> unlocated downgrade -> **max clamp** ->
    deviation clamp (`report_export.rs:3772-3789`).
  - xlsx/`findings.json` partition: normalize -> floor -> **max clamp** -> deviation clamp
    (`xlsx_export.rs:325-342`).
  (The deviation clamp runs after it, but being itself a ceiling it can only go lower; the
  gate at `lib.rs:2457-2465` rejects a `deviation_ceiling` ranking ABOVE `max_severity` as
  unreachable.)
- **Resolution.** The rule is looked up through `corpus_rule_for`
  (`report_export.rs:1649-1659`), so an external-tool finding whose `rule_id` is the TOOL's
  own id still resolves to the grounded corpus rule that may carry the ceiling. Absent
  corpus, unknown rule, or absent field all degrade to a no-op.
- **It clamps EVERYTHING under the rule**, including the anchored `"high"` of a
  `violates`-option finding - the doc at `ai_audit.rs:6265-6270` names `max_severity` as the
  author's escape hatch when that anchor is too aggressive for a specific rule.
- **Malformed value: THE trap (gotcha 1).** The clamp resolves the ceiling through
  `normalize_severity`, whose fall-through maps EVERY unrecognized string to `"low"`
  (`report_export.rs:468-479`). So `max_severity = "critcal"` does not error and does not
  no-op - it becomes the harshest ceiling in the vocabulary and caps every finding of the
  rule at low. The corpus-load gate `rules_with_invalid_severity_ceiling`
  (`lib.rs:2426-2470`, validating against `CEILING_SEVERITY_VOCABULARY`, `lib.rs:2393`)
  exists precisely to fail the build on the typo before it ships.
- **History worth keeping.** The whole mechanism shipped fully implemented and fully
  unit-tested against a corpus in which NOT ONE rule declared a ceiling - complete
  and completely inert (`lib.rs:2553-2559`). `rules_declaring_max_severity` plus
  `req_e2_the_corpus_actually_declares_severity_ceilings` is the anti-inertness gate written
  in response.

### 3.16 `min_severity`

- **Type / default.** Optional lowercase severity string, same vocabulary; absent -> no floor
  (`lib.rs:646-653, 1005-1038`). 41 rules on 2026-10-11 (`rules_declaring_min_severity`,
  `lib.rs:2545-2559`, is the anti-inertness probe and is asserted NON-EMPTY by
  `req_w8_the_corpus_actually_declares_severity_floors`).
- **Why it exists.** Roughly two dozen rules state a severity in prose ("Flag as high
  severity"). The owner ruled that phrasing is a FLOOR: "Floor, it's the minimum rating"
  (`lib.rs:1009-1027`). Before this field, the only floor was `ai_audit`'s D5 pass, which scans
  the MODEL'S prose against phrase tables (`severity_calibration_floor`,
  `ai_audit.rs:1346-1430`) - a floor authored in rule prose was unreadable by the pipeline.
- **What the engine does with it.** `report_export::raise_to_rule_min_severity`
  (`report_export.rs:561-587`) raises a finding's severity UP to the floor when it ranks
  below; never lowers. Applied FIRST in both exporters' chains (see 3.15), so `max_severity`,
  the deviation ceiling, and the PDF's unlocated downgrade all still get the last word.
- **Two hard preconditions (`report_export.rs:531-574`):**
  1. The finding's `kind` must be PRESENT and ACCUSING (`Defect` or `Deviation`). An
     informational kind is never raised ("always ship at info" by W3 design), and an ABSENT
     kind is excluded too under the honesty rule that nothing silently defaults to defect.
     Consequence: a legacy row with `kind: None` can never be floored.
  2. The finding must have a real location (`path` non-empty or `line != 0`); an unlocated
     row is never raised, which makes the floor order-independent with respect to the C4-R1
     unlocated downgrade.
- **Malformed value (gotcha 1, floor direction).** The same `normalize_severity` fall-through
  maps a typo'd floor to `"low"`, and since `severity_rank` ranks `low`/`info` at the bottom
  together (`report_export.rs:2762-2769`), a typo'd floor does not error and does not no-op -
  it quietly evaporates (the raise never fires). `rules_with_invalid_severity_floor`
  (`lib.rs:2501-2543`) fails the build on the typo, and separately on
  `min_severity` ranking above the same rule's `max_severity` (an unsatisfiable pair whose
  winner would otherwise depend purely on application order). `min_severity == max_severity`
  is legal and pins the rule at one severity.
- **`min_severity = "low"` or `"info"` is inert by construction** (ranks at-or-below
  everything); not an error, but never what the author wants - a rule capping itself at low
  wants `max_severity` (`lib.rs:2495-2499`).

### 3.17 `deviation_ceiling`

- **Type / default.** Optional lowercase severity string; absent -> the pipeline default
  ceiling `"medium"` applies (`lib.rs:654-660, 1039-1049`;
  `DEVIATION_DEFAULT_CEILING`, `report_export.rs:596`). 36 rules on 2026-10-11
  (`rules_declaring_deviation_ceiling`, `lib.rs:2580-2600`).
- **What the engine does with it.** `clamp_deviation_severity`
  (`report_export.rs:608-628`) clamps DOWN the severity of rows whose kind is
  `FindingKind::Deviation` ONLY - a Defect under the same rule is untouched, and a row with
  no kind is NOT assumed to be a deviation. Applied at both exporters, last in the chain
  (3.15). The rationale: a consistency deviation's cost is forward drift, not current risk,
  so it caps at medium by default; a rule whose decision domain genuinely compounds authors
  `deviation_ceiling = "high"` as the structured escape hatch.
- **Interaction with `max_severity`.** `max_severity` clamps every row of the rule INCLUDING
  deviations, so a `deviation_ceiling` ranking above `max_severity` can never bind; the
  ceiling gate rejects that pair at build time (`lib.rs:2457-2465`).
- **Inert on a `defect`-kind rule (gotcha 6).** Deviation rows only exist for rules that went
  through the alternatives pass and had a genuine option decided
  (`assign_deviation_kinds`, `ai_audit.rs:6355-6425`); a `defect`-kind rule never enters that
  pass, so it emits no deviation rows and this field does nothing on it.
- **Malformed value.** Same `normalize_severity` fall-through (a typo becomes a `"low"`
  deviation ceiling - harsher than the default); gated at build time by
  `rules_with_invalid_severity_ceiling` (`lib.rs:2442-2455`).

### 3.18 `layer` (INERT) and `tag` (DELETED)

The first edition of this document said both fields were present on every rule and parsed
by nothing. **Half of that is no longer true, and the two halves were resolved differently and
on purpose.**

**`tag` is GONE.** W17 deleted it from every rule file and from the schema (commit `23c81e6`).
The owner ruled it dead weight on three measured grounds, recorded at `lib.rs:661-672`: it held
three distinct values across the whole corpus (`"stack"` 388, `"universal"` 28, `"process"` 1 - 93
percent one value); it agreed with the folder-derived domain on only 21 of them; and the 28
`"universal"` rules were exactly the `testing/` and `universal/` folders, already derivable
from the path. Because `tag` is deliberately ABSENT from `RULE_LEVEL_FIELDS`
(`lib.rs:2065-2068`, with a "do not re-add it" guard comment), a reappearance is now a BUILD
FAILURE through the unknown-field gate rather than a silently accepted key. Verified by
`req_w17_no_corpus_rule_carries_a_tag_field`
(`crates/rules/tests/w17_tag_field_deleted.rs:85`, which parses the TOML rather than
line-matching, so a `tag` nested in `[[sources]]` could not hide) and by a reappearance fire
proof at `:187`.

**`layer` STAYS, and is still read by nothing.** It is now DECLARED on `RuleToml`
(`lib.rs:695-697`) as `#[serde(default)] #[allow(dead_code)] layer: Option<String>` and listed
in `RULE_LEVEL_FIELDS` (`lib.rs:2095`), purely so the unknown-field gate tolerates it. The
`#[allow(dead_code)]` is the compiler's own testimony that nothing reads it: **the `Rule`
domain struct has no `layer` field at all**, so the value is parsed and dropped on the floor at
the `RuleToml` -> `Rule` conversion (`lib.rs:1690-1704`, which copies `kind`, `inspectable` and
the three severity bounds, and not `layer`). A workspace-wide grep for `.layer` on any rule
object returns nothing; every hit is an unrelated `layer` field on run/gate telemetry.

It was kept rather than deleted because it is **not the same shape as `tag`**: it carries
information that is NOT derivable from the folder path, which makes it the more plausible
latent feature of the two, and the owner has not ruled on it. Distribution on 2026-10-11,
pinned as a histogram by `req_w17_layer_is_untouched_on_every_corpus_rule`
(`crates/rules/tests/w17_tag_field_deleted.rs:124`): `universal` 131, `framework` 113,
`language` 90, `platform` 35, `library` 34, `architecture` 7, `testing` 7. Eight folder
families hold more than one `layer` value, so the field genuinely sub-divides a domain rather
than restating it. It is also already internally inconsistent in at least two places, which is
what a hand-authored unread field drifts into: `go:testing` is `layer = "testing"` while every
other `<lang>:testing` folder is `layer = "language"`, and `JAVASCRIPT-PUBLIC-ENV-PREFIX-SECRET-1`
is `layer = "universal"` while its three `javascript/` siblings are `layer = "language"`
(`lib.rs:674-694`). **Anything built on `layer` must normalize it first.**

Practical consequence for an author: keep `layer` accurate for a human reader, but do not
expect it to change any behaviour, and never add a `tag`.

### 3.19 `[[emits]]` (INERT)

Two rule files (`universal/spirit-robustness-1.toml`, `rust/rust-domain-2-newtype-ids.toml`)
carry `[[emits]]` blocks with `target =`/`scope =` keys - the old camerata-ai emit-partition
format. Like `layer`, these are now DECLARED on `RuleToml` as accepted-but-unconsumed
(`lib.rs:699-708`), typed as a raw `Vec<toml::Value>` deliberately: declaring a concrete shape
would assert a contract this workspace does not honour, and would make a future key inside the
block a parse error for a block nothing reads. The live emit partitioning is done by `arm.rs`
from `enforcement` (section 3.3).

### 3.20 `version`

`version = 1` (an integer, REQUIRED on every corpus rule, placed beside `id`). The rule's
revision number: bumped by the author whenever anything about the rule changes (a directive, an
option, a source, a flag, the title).

Same lenient-parse / strict-gate split as `kind` and `inspectable`. `RuleToml::version` is
`Option<u32>` with a bare `#[serde(default)]`, so a TOML that omits it still parses (fixture
corpora in other crates do not declare it), and the BUILD is what refuses it:
`rules_missing_version` (absent) and `rules_with_invalid_version` (`0`), asserted empty against
the real corpus by `req_every_corpus_rule_declares_a_version_of_at_least_one`
(`crates/rules/tests/rule_version_review_fields.rs`). There is deliberately no default: a
version that silently defaulted to `1` would let a rule enter the lock without anyone deciding
what its revision is. A negative or non-integer value fails the TOML parse itself.

It reaches no client artifact.

### 3.21 `last_reviewed`

`last_reviewed = "2026-09-01"` (a quoted `YYYY-MM-DD` string, optional). The date this ONE rule
was individually confirmed current, between full domain sweeps. Absent means "no individual
review"; a blank string is scrubbed to absent at load.

A rule's EFFECTIVE review date is the later of its own `last_reviewed` and its top-level
domain's `[confirmed]` watermark in `CORPUS_FRESHNESS.toml` (`effective_review_date`, which
ignores an unparseable date on either side so a typo can only make a rule look less fresh).

Gate: `rules_with_invalid_last_reviewed(set, today)` fails the build on a value that is not a
real calendar date (the parser checks days-in-month, so `2026-02-30` and a non-leap
`2026-02-29` are rejected rather than rolled into March) or that is later than today. It
takes `today` as an argument so the gate is a pure function; the real-corpus test reads the
clock. A review is not a change to the rule, so it never forces a `version` bump. It reaches
no client artifact.

### 3.22 `superseded_since` and `current_recommendation`

Both optional strings, authored together or not at all. `superseded_since = "2021"` is the
year the ecosystem moved off the pattern the rule describes; `current_recommendation` is a
pointer (URL or short phrase) to what replaced it. They were added by W32 part (a) and stay
unused by every rule until a CONFIRMED sweep finding sets them (the sweep is the process that
decides a pattern aged out; see `docs/design/2026-10-20_corpus-staleness-check.md`).

Gates: `rules_with_malformed_credibility_metadata` (year is exactly four digits) and
`rules_with_unpaired_supersession` (setting exactly one of the two fails the build; a
whitespace-only half counts as absent). A year with no replacement is half a claim, and a
replacement with no year cannot say how stale the rule is.

### 3.23 The rule content hash and `CORPUS_LOCK.toml`

`version` is only worth having if a change to a rule cannot ship without it. Two pieces make
that mechanical.

**`Rule::content_hash()`** (`crates/rules/src/lock.rs`) is `sha256:<64 lowercase hex>` over a
canonical serialization of the WHOLE rule: every field, every option (including `directive`,
`why`, `remediation`, `violates`, `escalation`), every source, `[verified]`, every
`[[stack_exception]]`, the severity bounds, and the supersession fields. It EXCLUDES `version`
and `last_reviewed`, so bumping a version or recording a review does not move it. The
serialization is canonical JSON with keys sorted at every level by the module's own renderer,
no insignificant whitespace, hand-written string escaping (identical to `json.dumps(...,
ensure_ascii=False)`), and every `None`/empty/`false` field omitted. The preimage is prefixed
`camerata-rule-hash/v1\n`; that tag is the format version. Nothing is delegated to a library
that does not promise stability, so the same rule hashes identically across builds, platforms
and dependency upgrades. `the_canonical_form_and_hash_match_the_independent_oracle`
(`crates/rules/tests/corpus_lock.rs`) pins the bytes against a Python oracle. `Rule::
canonical_form()` destructures `Rule` with no `..`, so adding a field to `Rule` is a compile
error until its hash treatment is decided. Not covered: `layer` and `[[emits]]`, which are
parsed but not carried onto `Rule`; comments and formatting.

**`crates/rules/CORPUS_LOCK.toml`** records `"<ID>" = { version = N, hash = "sha256:..." }` for
every rule, sorted by id. It is GENERATED, never hand-edited, and the lock never moves on its
own. The gate (`check_lock`, asserted by `req_the_bundled_lock_matches_the_bundled_corpus`)
fails, naming the rule and the command to run, when:

| Disagreement | Message |
|---|---|
| same `version`, different hash | `rule changed but version is still N; bump version, then run camerata corpus lock` |
| `version` above the lock's | `run camerata corpus lock` (the lock must be regenerated, even for a bump that changed nothing) |
| `version` below the lock's | `versions only move forward` |
| rule missing from the lock | `run camerata corpus lock` |
| lock names a rule that no longer exists | `run camerata corpus lock` |
| rule has no `version`, or two rules share an id | cannot be locked |

`camerata corpus lock` (`crates/cli/src/corpus_cmd.rs`) is the only writer. It loads the
bundled corpus strictly (a file that fails to parse is an error, never a silent removal),
refuses when any rule changed without a bump, went backwards, has no version or has a
duplicate id (writing nothing), and otherwise records new rules, bumps and removals. It also
refuses to regenerate over a lock it cannot parse. Workflow for editing a rule: edit it, bump
`version`, run `camerata corpus lock`, commit the rule and the lock together.

**The same hash is the applied-rule drift baseline.** `RuleSelection.applied_hash` (a project's
record of the rule as it was last accepted) is `Rule::content_hash()`, stored with the rule's
`version` in `applied_version` by the drift "Update this rule" action (`accept_rule_drift`,
the only writer). `GET /api/updates/check` and `GET /api/projects/:id/rule-drift` compare it to
the corpus: a different `sha256:` hash is `changed: true`; a stored hash WITHOUT the `sha256:`
prefix was written by an older build (a non-cryptographic fingerprint over id, title, summary and
enforcement only, which could not see an option change and was not stable across builds), is not
comparable, and is reported as `changed: null` + `legacy_baseline: true` until the next accept
re-baselines it. Each entry also carries `applied_version` and `current_version` when known.

---

## 4. The `[decision]` block

Parsed into `DecisionToml { question, why, default }`, all optional (`lib.rs:720-730`).

### 4.1 `question`

- Carried to `Rule::decision_question` when non-empty (`lib.rs:1645-1649`).
- **Consumer:** the proposal/rule-detail UI only (`propose.rs:756`,
  `onboard.rs:452`) - "what the architect reads to understand WHAT they are choosing
  between". Not consumed by the scan or export pipelines; not in the PDF/xlsx.

### 4.2 `why`

- Carried to `Rule::decision_why` when non-empty (`lib.rs:1650-1654`).
- **Consumers:** the proposal UI (`propose.rs:757`), and source #2 for the derived `summary`
  when `qualifies` is absent (`lib.rs:1620-1633`) - through which it can become the
  emitted/diffed directive fallback text (section 3.14).

### 4.3 `default`

- The id of the adopted default option. **Honored ONLY when the rule-level `default = true`**
  (`lib.rs:1634-1644`); otherwise silently discarded. See section 3.7 for everything that
  hangs off the resulting `default_option`.
- **Ungated failure mode:** a `default` id that does not match any `[[option]].id` loads fine
  and simply never resolves - `resolved_option` finds nothing, so Fix/effort/floor-text/
  escalation all silently degrade, AND the remediation gate skips the rule (its filter_map
  drops a rule whose default id does not resolve, `lib.rs:3183-3195`). There is no corpus
  gate asserting `default_option` resolves to a real option (section 11).

---

## 5. `[[option]]` fields

Parsed into `OptionToml` (`lib.rs:732-783`) and carried as `RuleOption` (`lib.rs:785-858`).
A rule may have zero options (a pure content/mechanical rule); option-dependent machinery
then never engages.

### 5.1 `id`

- Required string. The stable identity everything resolves by: `[decision].default`
  (section 4.3), the project's persisted `RuleSelection.chosen_option`, the alternatives
  pass's option validation (a model answer naming an id not in the rule's own list is a
  flagged hallucination and falls back to the selected/default option,
  `ai_audit.rs:2560-2615`), `Finding::evaluated_option_id` (set blanket per rule after the
  recommendation is known, `ai_audit.rs:7190-7205`, and read back by `resolve_fix`'s
  resolution order), and the structured kind payloads (`convention_option_id`,
  `observed_options[].option_id`).

### 5.2 `label`

- Required string; the human name. **Client-facing in several places:** the alternatives
  prompt (`ai_audit.rs:2318-2327`), and VERBATIM (corpus text, never model prose) in the
  structured kind payloads that reach `findings.json`/xlsx - `DeviationKind::
  convention_label`, `ConventionObservedKind::convention_label`, `ObservedOptionUsage::label`
  (`api-types/src/audit.rs:154-207`; populated at `ai_audit.rs:6403-6410, 6494-6500,
  2529-2535`). Also the snippet fallback for a violates-option or convention-observed finding
  with no evidence text (`ai_audit.rs:6304-6308, 6485-6492`).

### 5.3 `directive`

- Default empty string (`lib.rs:735-736`). The rule's behavioural requirement, in ONE voice
  for three readers: the scanning model (as an adopted rule to check code against), the
  client's own agents, and the client's engineers.

  **Correction, 2026-10-20.** This read "The DETECTION/enforcement recipe, written for an
  agent/model, not a client", which is wrong in both halves and is the sentence
  `RULE_AUTHORING.md`'s 2026-10-19 correction identifies as the misreading that let detector
  voice into the whole `supabase/` pack. `directive` DOES reach the client —
  `arm.rs::render_rule` emits it verbatim into their `AGENTS.md`/`CONVENTIONS.md` — and that
  is the product rather than a leak. And a scanner-recipe directive is wrong for the model
  too: the scan prompt introduces the list as "the rules the project has ADOPTED", so
  "Regex-scan for X / Emit a high-severity finding" tells a language model to impersonate a
  regex engine instead of telling it what the code must be. There is no reader for whom the
  recipe phrasing is correct. See `RULE_AUTHORING.md`, "The MULTI-AUDIENCE fields".
- **Consumers:**
  - The semantic audit prompt: the selected rule list is literally
    `- [RULE-ID] directive` lines (`ai_audit.rs:356`), where the directive is the
    chosen/default option's text (section 1 step 3). When the alternatives pass decides a
    NON-violates option, the rule's entry in `effective_selected` is REWRITTEN to that
    option's directive before the violation passes run (`apply_alternative_decision`,
    `ai_audit.rs:6287-6294`); when it decides a `violates` option, the directive is left
    untouched (section 5.9).
  - The alternatives prompt shows every option's directive so the model can match it against
    the code (`ai_audit.rs:2318-2327`).
  - The emitted CONVENTIONS/AGENTS text (`arm.rs:95-99`) and the rule-drift diff
    (`server/src/lib.rs:15083-15088`).
- **It must NEVER reach a client report.** `resolve_fix` binds ONLY to `remediation` and
  explicitly never falls back to `directive` ("fail-safe here means silently omitting the Fix
  block, not leaking the recipe", `report_export.rs:2020-2036`).

### 5.4 `why`

- Default empty string. The rationale for the option.
- **Consumers:** the alternatives prompt (`ai_audit.rs:2318-2327`), the proposal UI
  (`propose.rs:709`), and - importantly - the DETAIL text of a violates-option defect
  finding, verbatim: `apply_alternative_decision` interpolates `option.why` into the
  finding's `detail` (`ai_audit.rs:6314-6321`), which reaches the client artifacts. Write a
  violating option's `why` as client-readable fault prose. This is also where the
  `Rejected:` convention lives (section 10.4).

### 5.5 `remediation`

- Optional; blank filtered to `None` at load (`lib.rs:1664`). 486 occurrences on 2026-10-11.
- **Contract.** 2 to 4 imperative, client-facing sentences: what to change, WHERE, and how to
  VERIFY it is closed (`lib.rs:739-746`).
- **Consumer.** `report_export::resolve_fix` (`report_export.rs:2038-2060`): joins the
  finding's rule id to the corpus (via `corpus_rule_for`, so external-tool ids ground too),
  resolves the option in this order - (1) `finding.evaluated_option_id`, (2) the project's
  `chosen_option`, (3) the rule's `default_option` - and instantiates placeholder tokens from
  the finding's own `path`/`captures` (`instantiate_remediation`,
  `report_export.rs:2262-2313`). Surfaced as the PDF "Fix:" line, the workbook's
  "Recommended Fix" column, and `findings.json`'s `fix` field - the same join for all three.
- **Placeholders.** `<path>`/`<file>` fill from the finding's path; any other
  `<token>` fills from `captures[token]`, else a readable generic
  (`generic_placeholder_filler`, `report_export.rs:2175-2199`). Tokens are lowercase
  ASCII/digits/hyphens only; a `<...>` span that does not match that shape (a literal HTML
  example, a generic-type snippet) is emitted verbatim (`report_export.rs:2168-2173,
  2284-2292`). `<a:token>` prepends a correctly chosen "a"/"an" to the resolved value
  (`report_export.rs:2250-2307`).
- **Absent.** The Fix block is OMITTED entirely - never backfilled from `directive`
  (section 5.3). And since C5-8, an OPEN action-bucket row with NO fix from any source
  (neither `fix_specific` nor authored remediation), for a rule the corpus RECOGNIZES, is
  re-routed to the `held` bucket with a disclosed reason rather than shipping an action item
  with a blank Fix (`is_action_row_missing_a_fix`, `report_export.rs:2678-2702`; final check
  in `effective_bucket`, `report_export.rs:2629-2638`). So an unauthored remediation on a
  recognized rule does not just lose a Fix line; it can demote the row out of the action
  queue.
- **Gate.** Every rule WITH a default option must carry non-empty remediation on that option
  (`every_rule_with_a_default_option_has_authored_remediation`, `lib.rs:3180-3202`).
  Non-default options are ungated by design (several legitimately offer a "no policy
  adopted" alternative with nothing to fix).

### 5.6 `finding_headline` and `finding_detail`

- Optional pair; blanks filtered to `None` (`lib.rs:1665-1666`). 9 of each on 2026-10-11.
- **Purpose (P4).** A deterministic FLOOR finding's raw `detail` is the gate's own
  enforcement prose ("Deny writing a file whose path marks it as secret-bearing..."), set at
  scan time by `onboard::audit::title_for` - internal text a client should never read. This
  pair is the authored, client-facing replacement, instantiated with the same placeholder
  machinery as `remediation` (`report_export.rs:2083-2131`).
- **Consumer.** `resolve_floor_finding_text` (`report_export.rs:2107-2131`), via
  `client_headline_and_detail` (`report_export.rs:2133-2150`), which both the PDF's curated
  sites and the xlsx headline/detail columns call (`xlsx_export.rs:352-359`) - so the rewrite
  is identical across artifacts.
- **Authored as a PAIR or not at all.** If EITHER half is absent/blank the function returns
  `None` and the row falls back entirely to the pre-P4 derivation (`defect_headline` over
  `finding.detail`); a half-authored pair renders NOTHING of the authored text
  (`report_export.rs:2117-2127`). Convention for `finding_detail`: one plain-language
  sentence first (what this means for a non-technical founder), then 1 to 2 sentences of
  what/where/impact (`lib.rs:816-823`).
- **Never consulted for an AI-tier finding** in practice (invented rule ids never resolve in
  the corpus), but the code degrades to `None` rather than assuming that
  (`report_export.rs:2112-2118`).

### 5.7 `effort`

- Optional; blank filtered to `None` (`lib.rs:1668`). Vocabulary: `"low" | "medium" | "high"`,
  the same three tiers as `Finding::effort` (`lib.rs:761-771`). 141 occurrences on 2026-10-11.
- **Consumer.** `report_export::resolve_effort` (`report_export.rs:3371-3382`): the finding's
  OWN calibrated/default `effort` always wins; the authored band is consulted ONLY when that
  is absent - which is the permanent state for deterministic-floor/architectural/RLS-replay
  findings that never pass through AI calibration (`onboard/architectural.rs` sets
  `effort: None` unconditionally). Without an authored band, such a row renders "not yet
  estimated" forever (`effort_hours_bounds`, `report_export.rs:3392-3399`: low = 2 to 4 h,
  medium = 8 to 16 h, high = 24 to 40 h, None = "not yet estimated").
- **Bucket interaction.** `matrix_bucket` reads effort as EXACTLY `Some("low")`
  (`report_export.rs:2541`): an open high-severity finding is `do_now` only with low effort,
  else `do_next`. Note this reads the FINDING's effort, not the resolved one - and note the
  trap below.
- **Missing estimate never demotes.** Since C6-B4 (amended), a missing estimate does NOT move
  a row out of its action bucket - it renders the honest gap instead
  (`report_export.rs:2639-2651`); `is_action_row_missing_an_estimate`
  (`report_export.rs:2733-2757`) exists as a recognized-rule safety probe.
- **Gate.** Every CI-tier (mechanical/architectural) rule option that carries `remediation`
  must also carry an authored `effort`
  (`every_deterministic_rule_option_with_remediation_has_an_authored_effort_band`,
  `lib.rs:4270-4300`). The gate checks PRESENCE only.
- **Trap: the VALUE is unvalidated.** No gate checks the string is one of
  low/medium/high. A typo (`"Low"`, `"quick"`, `"med"`) parses fine, passes the presence
  gate, and then: `effort_hours_bounds` renders "not yet estimated" (fall-through arm), and
  `matrix_bucket`'s `matches!(effort, Some("low"))` reads it as not-low - silently demoting a
  high-severity row from `do_now` to `do_next`. See section 11.

### 5.8 `escalation`

- Optional inline table `escalation = { condition = "...", severity = "..." }`
  (`lib.rs:758-760, 878-887`). `severity` is `"soft-flag"` (log and continue) or
  `"hard-pause"` (stop the run for human review); ABSENT severity defaults to `HardPause`
  (`lib.rs:860-876`). 1 rule used it on 2026-10-11 (the test-tamper rule).
- **Option-scoped by design:** only the SELECTED (or default) option's spec is active, so a
  rule can offer an escalating option alongside non-escalating ones, and selecting the
  non-escalating option genuinely disables escalation (`Rule::selected_escalation`,
  `lib.rs:1137-1147`).
- **Consumers.** The dev-implement run assembly (`server/src/lib.rs:2138-2161`) computes, per
  project selection: `escalations_in_scope` (every selected rule whose selected option
  escalates - the governed agent is grounded with these conditions and self-escalates), and
  the test-tamper rule's spec specifically, driving its deterministic detector backstop
  (`test_tamper.rs:169`). A met `HardPause` condition pauses the run (AwaitingReview +
  checkpoint/resume); `SoftFlag` logs and continues.
- **Not consumed by the scan/report pipeline at all** - this is a governance-run mechanism,
  not an audit one.
- **Malformed.** An unknown `severity` string fails the file's parse (strict enum; gotcha
  10). A missing `condition` likewise (required field).

### 5.9 `violates`

The single most consequential option flag, and the one an earlier option audit found wrong 76
times. Absent -> `false` (`lib.rs:772-776`): a genuine, non-degenerate alternative. `true`
means CHOOSING THIS OPTION IS THE DEFECT - the option describes the absence or a degenerate
form of the rule's own requirement, authored as an `[[option]]` only because the corpus
convention pairs correct behavior with the behaviors deliberately not adopted
(`lib.rs:839-854`).

**Exactly what the engine does with it, step by step.** All runtime consumption is inside the
alternatives pass; the single chokepoint is `apply_alternative_decision`
(`ai_audit.rs:6279-6330`), through which BOTH deciding paths route (operator-forced picks at
`ai_audit.rs:6712-6760`, model recommendations at `ai_audit.rs:6786-6820`):

- **`violates == false` on the decided option:** the rule's entry in the effective directive
  set is REWRITTEN to this option's `directive`; the per-chunk violation pass then judges
  conformance against the newly adopted directive. No finding is built here.
- **`violates == true` on the decided option:**
  1. The directive is LEFT UNTOUCHED - the chunk pass keeps checking the rule's
     pre-existing (selected/default) directive, so other sites following the defect can
     still be independently flagged.
  2. A GUARANTEED-floor `Finding` is built: `rule_id` = the rule's own citation,
     `severity` anchored at `"high"` ("a confirmed, evidence-backed determination that the
     repo follows the rule's own named defect, not a hedge" - cap it per rule with
     `max_severity` if too aggressive), `kind = FindingKind::Defect`,
     `evaluated_option_id` = the violating option's id (so `resolve_fix` renders THAT
     option's authored remediation), `detail` interpolating the option's `label` and `why`,
     and `path`/`line`/`located` parsed from the recommendation's evidence citation. An
     operator-forced pick carries no evidence, so `located = false` and the PDF's C4-R1
     net demotes that one shape to `info` (`ai_audit.rs:6271-6278`).
  3. These findings are SPLICED in after calibration and the P7 not-applicable filter, with
     their own ledger stage `violates_option_emitted` (`ai_audit.rs:7160-7178`) - they never
     re-enter the LLM calibration round-trip.
- **Kind assignment reads it too:** a rule whose DECIDED option is `violates` is skipped by
  `assign_deviation_kinds` (its rows are Defects, never consistency deviations,
  `ai_audit.rs:6376-6382`) and by `conventions_observed_findings` (following the defect is
  never "convention observed", `ai_audit.rs:6450-6456`).
- **It reaches the model's eyes:** `RuleOptionView.violates` is carried into the alternatives
  structures (`onboard/audit.rs:444-453`, `propose.rs:705-711`), though the prompt text does
  not currently render a per-option violates marker (`build_alternatives_block`,
  `ai_audit.rs:2299-2338`, prints id/label/directive/why only).
- **Where it is INERT:**
  - On a `defect`-kind rule (never enters the alternatives pass) - the flags are
    documentation plus gate signal only.
  - On a CI-enforced, gate-armed, or governance (`ORCH-/SPIRIT-/PROC-`) rule, or a rule with
    fewer than 2 options - same reason (`is_semantic_multi_option_rule`,
    `onboard/audit.rs:407-414`).
  - When the option is never DECIDED for a scan (the flag fires only on the decided option).
- **The three audit-era failure modes it guards against** (and which this reference exists to
  prevent recurring): a flag missing on the truly degenerate option (the GDPR rule silent on
  its own clearest violations), a flag set on the SAFE option (the CORS rule flagging the
  allowlist while ignoring the wildcard), and flags on a `decision`-kind rule (gotcha 3:
  eight `prose`/`structured` security rules were pure decisions with zero flags, so the
  vulnerability was adopted as the project's effective directive and reported as a convention
  the codebase chose. That third mode is now a BUILD FAILURE, not just a convention:
  `decision_kind_rules_with_violates_flag` (`lib.rs:2007-2023`). See gotcha 3, which also
  carries the correction to the "nineteen" figure this document originally printed.)

---

## 6. The five finding kinds

Defined in `crates/api-types/src/audit.rs:50-81`; internally tagged on `"kind"`
(snake_case), flattened into every exported row so the discriminator plus payload fields are
siblings. `KindOnWire` guarantees every EXPORTED row carries a `kind` key; an absent kind
exports as the explicit `"unspecified"` tag, never silently as `"defect"`
(`audit.rs:43, 116-145`).

| Kind | Who produces it | Severity behavior | Where it renders |
|---|---|---|---|
| `defect` | Parse-time default for every model-reported fault (`ai_audit.rs:626-630`); every deterministic producer (floor checks `onboard/audit.rs:274, 332`, architectural checkers `onboard/architectural.rs:123`, previews `scan_tools.rs:934`, dep audit `dep_audit.rs:207`); the violates-option emission (`ai_audit.rs:6327`) | Unconstrained apart from floor/ceilings | Findings sheet / action buckets |
| `deviation` | `assign_deviation_kinds` reclassifies every surviving row under a rule whose DECIDED option is genuine (non-violates) (`ai_audit.rs:6355-6425`). Payload: structurally counted numerator, model denominator only when >= numerator, authored option id + label | Capped at `deviation_ceiling` else `"medium"` (section 3.17); floor applies (accusing kind) | Findings sheet; Deviations summary |
| `convention_observed` | One row per rule the scan decided a genuine option for with ZERO trace of violation anywhere in the final row set (`ai_audit.rs:6427-6513`) | Emitted at `info`; informational by kind; never floored | Conventions sheet; informational appendix. Ships in the product (A5.5: the derived decision set) |
| `no_convention` | Exactly ONE row per rule whose recommendation honestly said "no convention adopted" - available only to `decision`-kind rules, with the per-site violation path severed structurally in three layers (`ai_audit.rs:6515-6568, 6791-6806`) | `info`; informational by kind | Conventions sheet; informational appendix |
| `not_assessed` | One honesty row per ARMED rule whose routing group matched zero files (`ai_audit.rs:6570-6602`); closed reason vocabulary `pass_failed / no_files_matched / tool_failed / model_declined` (`audit.rs:211-235`) | `info`; informational by kind | Coverage sheet ("could not complete") |

**How a rule's configuration determines the kind a row gets:**

- `kind = "defect"` rule -> all rows stay `Defect`.
- `decision` / `default_with_exceptions` rule, decided option genuine -> rows become
  `Deviation`; zero rows -> one `ConventionObserved`.
- Decided option `violates = true` -> the guaranteed finding is `Defect`, and sibling rows
  under the rule STAY `Defect` (deviation reclassification skips the rule).
- `decision`-kind rule, honest no-majority answer -> one `NoConvention`, zero per-site rows.
- Armed but zero files matched the routing group -> one `NotAssessed`.
- `kind.is_informational()` (`audit.rs:106-113`) is the structural split the export layer
  uses: informational kinds always ship at `info` severity and live outside the action tiers.

**Routing consequences:** the xlsx `kind_sheet` partition (`xlsx_export.rs:638-649`) sends
`None | Defect | Deviation` to the Findings sheet (an UNKINDED row deliberately lands with
the accusing rows - "visible in the wrong tab", never "quietly filed as an observation"),
`ConventionObserved | NoConvention` to Conventions, `NotAssessed` to Coverage. The
`min_severity` floor and `clamp_deviation_severity` are both kind-gated (sections 3.16,
3.17).

---

## 7. The severity pipeline, in order

A finding's exported severity is the result of an ordered chain. Corpus fields enter at the
marked points.

**Upstream, inside `ai_audit` (before export):**
1. The model's own severity, or the deterministic producer's authored severity
   (`onboard/audit.rs:12` for floor rules).
2. Calibration verdicts (`apply_verdicts`, `ai_audit.rs:946`), with the upward-raise
   exploit-precondition guard.
3. D5 deterministic floor - prose-scanning phrase tables over the finding's own text
   (`severity_calibration_floor`, `ai_audit.rs:1346-1430`): unauthenticated/full-compromise
   -> critical; authenticated cross-tenant read -> high (critical with an escalating data
   class); RLS-hedge guard -> high. NOT corpus-driven; this is the pass `min_severity` was
   built to replace for authored floors.
   - **W8-Q4: the phrase tables match WORDS, not bare substrings.** Every table now routes
     through `contains_phrase_as_word` (`ai_audit.rs:1242-1281`), which requires a word
     boundary on both sides, where a word char is `is_alphanumeric() || '_'`
     (`ai_audit.rs:1219`) - a hyphen is deliberately NOT a word char, so `"cross-tenant"` and
     `"defense-in-depth"` still match. A closed inflection set is admitted on the trailing
     side, `PHRASE_INFLECTION_SUFFIXES = ["s", "es", "ed", "ing", "able", "ible"]`
     (`ai_audit.rs:1202`), plus a bare `d` only for phrases of length >= 4 ending in `e`
     (`phrase_admits_bare_d_suffix`, `ai_audit.rs:1211`), which keeps `see` -> `seed` out
     while keeping `retrieve` -> `retrieved` in. `"y"`, `"en"` and `"er"` are deliberately
     excluded, so `"seen"` is conservatively NOT the read verb `see`
     (pinned, `ai_audit.rs:10266`).
   - **Before this, the floor fired on fragments of unrelated English.** `already` contained
     `read`, `allowlist` contained `list`, and `urls` contained `rls` - so a finding saying
     "the record was already returned" or "signed urls expire" could be floored as a
     cross-tenant read or an RLS hedge. Those three exact pairs, plus `review`/`view`,
     `seed`/`see`, `ready`/`read`, `listener`/`list`, `passwordless`/`password`,
     `each`/`"ach transfer"` and `preview`/`view`, are asserted as must-NOT-match in
     `req_w8_q4_contains_phrase_as_word_unit_contract` (`ai_audit.rs:10285`, table at
     `:10287-10299`), with a must-STILL-match table for the inflections at `:10301-10316`.
4. D6/D7/D8 ceiling rules (`apply_severity_ceiling_rules`, `ai_audit.rs:1692, 1795`):
   self-hedged confirmations capped low + held; CORS clamped to exactly medium
   (bidirectional); `ARCH-OBSERVABILITY-*` forced to exactly low BY RULE ID (gotcha 7).
5. The violates-option splice enters here with its anchored `"high"`
   (`ai_audit.rs:6312, 7160-7178`).

**The bound chain, applied identically by the live scan, the PDF/JSON partition and the
xlsx/`findings.json` partition (`severity_bounds::bounded_severity`), per finding:**
1. `normalize_severity` (`report_export.rs:468-479`) - casing fixed; UNRECOGNIZED -> `"low"`.
2. **`min_severity` raise** (`raise_to_rule_min_severity`) - only accusing-kinded, located
   rows.
3. C4-R1 unlocated downgrade (`downgrade_unlocated_above_informational`,
   `report_export.rs:647-653`) - empty path AND line 0 -> `"info"`.
4. **`max_severity` clamp** (`clamp_to_rule_max_severity`) - last word over everything above.
5. **`deviation_ceiling` clamp** (`clamp_deviation_severity`) - Deviation-kinded rows only.

Until 2026-10-21 step 3 ran in the PDF partition only; it now runs in the shared chain, so
the live scan, the PDF, the workbook, the CSV/JSON exports and `findings.json` all agree on an
unlocated row (it is `info` everywhere). See `severity_bounds.rs` and section 11.

Rank order for every comparison: `critical(0) < high(1) < medium(2) < low/info/anything(3)`
(`report_export.rs:2762-2769`); `info` survives normalization as its own tier below low
(`report_export.rs:473-476`) but ranks equal to low in comparisons.

---

## 8. Bucket routing: what reaches the client, and where

`effective_bucket` (`report_export.rs:2607-2652`) is THE single bucket computation, shared by
the PDF matrix, the curated findings, and the xlsx partition. Order of gates:

1. `is_held_for_review` (`report_export.rs:2894-2899`): calibration hedge
   (`needs_review` / `confidence == "needs-review"`) on an OPEN row -> `held`, at ANY
   severity.
2. `is_uncited_ai_finding` (`report_export.rs:1993-2010`): AI-tier with only an advisory
   citation -> `held` at high/critical, `informational` below.
3. `is_informational` (`report_export.rs:2916-2970`), in ITS internal order:
   `ARCH-OBSERVABILITY-*` rule identity FIRST (before the hard invariant; gotcha 7); then the
   hard "critical/high is never informational" invariant; then `info` severity; then
   testing-style with fewer than 3 test files; then absence-type `structured` stance-layer
   rules at medium or below.
4. `matrix_bucket` (`report_export.rs:2526-2545`): dispositions map directly
   (accepted / do_now / plan); open rows: critical -> `do_now`; high + low effort ->
   `do_now`; high otherwise -> `do_next`; everything else -> `plan`.
5. `is_action_row_missing_a_fix` (`report_export.rs:2678-2702`): a recognized-rule action
   row with no fix of any kind -> `held` (section 5.5). A missing ESTIMATE never demotes
   (`report_export.rs:2639-2651`).

Groups then partition into `curated_findings` vs `held_for_review_findings`
(`report_export.rs:4190-4198`; struct fields at `report_export.rs:1296, 1310`). **Rows routed
by rule identity (gotcha 7) or any gate above land under `held_for_review_findings` /
the informational appendix, NOT `curated_findings` - a test (or reader) consuming only
`curated_findings` is structurally blind to them.**

What reaches which artifact, per corpus field: see the per-field sections; summary table:

| Corpus field | PDF | xlsx / findings.json | Prompt-only | Internal-only |
|---|---|---|---|---|
| `title`, `id` | yes | yes | yes | |
| `sources` (url/title/linter) | yes (citations) | yes | | linter also drives tooling |
| `sources.relevance` | NEVER | NEVER | NEVER | parsed, reachable, consumed by nothing - author-facing provenance (section 3.9) |
| option `label` | yes (via kind payloads, snippets) | yes | yes | |
| option `why` | yes (violates-finding detail) | yes | yes | |
| option `directive` | NEVER | NEVER | yes | emit/diff |
| option `remediation` | yes (Fix) | yes | | |
| option `finding_headline`/`finding_detail` | yes | yes | | |
| option `effort` | yes (hours label) | yes | | |
| `max_severity`/`min_severity`/`deviation_ceiling` | effect only | effect only | | |
| `kind`, `violates` | effect only | effect + kind column | kind: the PURE DECISION marker | |
| `qualifies`/`decision.*` | | | | UI/proposals, summary fallback |
| `verification`, `opt_in_only`, `layer3_only`, `stack_exception`, `extra_domains` | effect only (what ran at all) | effect only | | |
| `inspectable` | effect only (whether the rule was scanned at all) | effect only; also decides whether the rule appears on the Coverage sheet | | emit path ignores it |
| `layer`, `[[emits]]` | | | | declared, parsed, DISCARDED - nothing reads them |
| `tag` | | | | DELETED from the schema and from every rule |

---

## 9. Named gotchas

Every one of these has bitten. Numbers 1 to 7 were named in the audit brief; 8 to 15 were
found while writing the first edition of this reference; 16 to 18 were added on 2026-10-14.
Three of the originals changed substantively in that revision and are worth re-reading even
if you know them: **2 is RETRACTED** (it was not a contradiction), **3's figure and its
headline example were both wrong** and are corrected, and **8, 9 and 10 are each now half
closed** by gates or by W13's failure routing.

1. **`normalize_severity` maps every unrecognized string to `"low"`**
   (`report_export.rs:468-479`). A typo'd `max_severity` becomes the HARSHEST ceiling in the
   vocabulary (caps everything at low); a typo'd `min_severity` silently evaporates (low
   ranks at the bottom, so the raise never fires); a typo'd `deviation_ceiling` becomes a
   low cap. Build gates `rules_with_invalid_severity_ceiling` / `_floor`
   (`lib.rs:2426, 2501`) reject the typo before it ships.

2. **`default = false` with `kind = "default_with_exceptions"` is NOT a contradiction.**
   The first edition of this document asserted it was, and recommended a build gate for it.
   **Both were wrong and are retracted** (owner's correction of 2026-10-11,
   `docs/plans/2026-10-09_audit-findings-tracker.md`). `default` governs ONBOARDING ADOPTION
   ("does this rule ship an adopted default option, or must the architect choose?") while
   `kind` governs OPTION SEMANTICS. They are orthogonal. `default = false` is a first-class
   documented authoring choice, typically paired with `opt_in_only = true` for a rule whose
   right answer depends on project-specific infrastructure (`lib.rs:3446`). The proposed gate
   would have failed the build on rules authored exactly as `docs/RULE_AUTHORING.md`
   prescribes - 13 rules declare `opt_in_only = true`, and `INTEGRATION-API-CONTRACT-1` and
   `INTEGRATION-AUTH-SEAM-1` were named false positives of the framing.
   **What survives is a HEURISTIC, not a rule:** of the 11 rules flagged on that basis, 7
   were genuinely misclassified and moved to `decision`, and 1 had a genuinely wrong
   `default`. So the correlation was real and predictive while the explanation was false -
   which is exactly why the signal belongs in a review report and never in a build failure.
   The one functional consequence that IS true stands on its own: a rule with no resolvable
   default option resolves no Fix, no authored floor text, no effort band and no escalation
   until a project persists a `chosen_option` (section 3.7).

3. **A `decision`-kind rule can never report a violation at all - and it is now FORBIDDEN
   from trying.** A decision rule's only reportable conditions are cross-codebase
   inconsistency (Deviation rows) and the no-convention row; whatever option the repo follows
   is recorded as the convention. That is correct for genuine stance decisions and
   catastrophic for security requirements - **eight** security rules were genuinely in that
   state, among them `SUPABASE-STORAGE-KEY-SANITIZATION-1`, where the measured outcome was no
   finding, the vulnerability adopted as the project's effective directive, and a
   `ConventionObserved` row asserting the codebase consistently follows that "convention".
   **Correction worth carrying:** the first edition of this document said nineteen and
   illustrated it with "no RLS policy". Both were wrong, and the audit tracker retracted them
   on 2026-10-12. `SUPABASE-RLS-NO-POLICY-1` is `architectural` and
   `SUPABASE-KEY-SERVICE-ROLE-CLIENT-1` is `mechanical`, and `is_ci_enforced`
   (`lib.rs:108-113`) already excludes both tiers from the alternatives pass - so neither
   could ever have been silenced this way, and several of the 11 had live deterministic
   detectors emitting the whole time. Only the `prose`/`structured` tier is exposed. The fix
   for a genuine requirement is
   `kind = "defect"` or, when several options are genuinely equal AND a named fault exists,
   `kind = "decision_with_floor"` (section 3.6). Since W14 you cannot paper over it by adding
   a flag in place: `decision_kind_rules_with_violates_flag` (`lib.rs:2007-2023`) fails the
   build on a `decision`-kind rule carrying any `violates` option.

4. **`verification` can disarm a rule everywhere, forever.** Omitting it defaults to `draft`
   -> not auto-recommended -> never selected by an unattended scan (`lib.rs:1270-1276`,
   `inspect_cmd.rs:155-165`). `UI-UTC-DATES-1` shipped that way (finding A7).
   `SPIRIT-FILE-SIZE-1` was DOUBLY disarmed: `verification = "policy"` (pre-W4, not
   auto-recommended) plus the `SPIRIT-` prefix exclusion from the code-audit tier
   (`onboard/audit.rs:357-359`) - both halves since fixed (`policy` now arms,
   `lib.rs:1258-1276`; the prefix exclusion was removed from the semantic prompt,
   `onboard.rs:1003-1016`). The gate `rules_disarmed_by_verification` (`lib.rs:1807`) now
   fails the build on any unarmable verification value.

5. **A rule whose `domain` no stack emits can never run, for any repo, ever.**
   `domains_for_stack` (`propose.rs:527`) is the only bridge from a detected repo to corpus
   domains; 94 rules (23 percent of the corpus then) once sat in unmapped folders. Two build gates now
   enforce reachability (`req_h1_every_corpus_domain_is_emittable_by_some_stack`,
   `propose.rs:843`; the witness-repo test at `propose.rs:1049`). Adding a stack rulepack
   requires wiring detection, `domains_for_stack`, AND `domain_to_glob` (section 3.4).

6. **`deviation_ceiling` is inert on a `defect`-kind rule.** Defect-kind rules never enter
   the alternatives pass, so they emit no Deviation rows, and `clamp_deviation_severity`
   only touches Deviation-kinded rows (`report_export.rs:608-628`). An authored ceiling
   there states an intent nothing will ever execute.

7. **`ARCH-OBSERVABILITY-*` routes out of every action bucket BY RULE IDENTITY.**
   `is_observability_convention_class` (`server/src/lib.rs:5617, 5639-5641`) is a prefix
   check consumed in two places: the D8 ceiling pass forces the severity string to exactly
   low, and `is_informational` checks it FIRST, ahead of its own "critical/high is never
   informational" invariant (`report_export.rs:2928-2945`). Findings of these rules render
   under `held_for_review_findings`/informational, never `curated_findings` - tests reading
   only the latter were structurally blind to them.

8. **HALF RESOLVED. `tag` is deleted; `layer` and `[[emits]]` are declared but still read by
   nothing** (section 3.18/3.19). `tag` is gone from every rule file and from the schema, and
   re-adding it is now a build failure. `layer` is on every rule, is declared on
   `RuleToml` with `#[allow(dead_code)]`, has no field on the `Rule` domain struct at all, and
   is dropped on the floor at the `RuleToml -> Rule` conversion. Editing `layer` still changes
   nothing; do not mistake it for a selection or emission control. If you ever DO wire it,
   normalize it first - it is already internally inconsistent in at least two folders.

9. **A misspelled FIELD NAME still vanishes at parse time, but no longer ships.**
   `deny_unknown_fields` is deliberately still off (`lib.rs:556-558`, reasons at
   `lib.rs:2038-2062`), so serde accepts `max_severety = "high"` or `voilates = true` and the
   field simply is not there. What changed in W14 is that
   `corpus_files_with_unknown_fields` (`lib.rs:2208-2290`) now fails the BUILD on any such
   key, across seven nested vocabularies, and the fire proof exercises five of them
   (`crates/rules/tests/w14_corpus_schema_gates.rs:455`). The hazard that remains is narrow
   but real: a typo in a FIXTURE corpus outside `crates/rules/principles/` is still silent,
   and so is a correctly-spelled key in the WRONG place - `qualifies` inside `[[sources]]` is
   in the accepted vocabulary at both levels and means nothing at one of them (section 3.9).

10. **A wrong VALUE in a strict-enum field still drops the entire rule - but a SCAN now says
    so.** `enforcement`, `kind`, and `escalation.severity` parse as strict enums; an unknown
    value fails the FILE's parse. Since W13 the scan path records that file
    (`record_corpus_load_failures`, `server/src/lib.rs:5283-5308`), fails
    `camerata integrity-gate` under `corpus_load_failure`, and discloses one line to the
    client (section 2). The PROPOSAL path still discards the error list
    (`propose.rs:661`, `server/src/lib.rs:4275`), so the rule still disappears silently from
    a proposal. The backstop there is the test suite's `errors.is_empty()` assertions
    (`onboard.rs:2257` and siblings) - run the tests after editing TOMLs.

11. **A dangling `[decision].default` id is ungated.** If the default id names no real
    option, the rule loads, resolves no option anywhere (no Fix, no effort band, no floor
    text, no escalation), and the remediation gate SKIPS it rather than failing
    (`lib.rs:3183-3195`). Verify the id matches an `[[option]].id` exactly.

12. **`effort` values are unvalidated.** The presence gate (`lib.rs:4270`) does not check the
    vocabulary; anything other than exactly `"low"`/`"medium"`/`"high"` renders "not yet
    estimated" and reads as not-low in `matrix_bucket` (`report_export.rs:2541`), silently
    demoting a high finding from `do_now` to `do_next`.

13. **`min_severity` never fires on unkinded or unlocated rows** (by design,
    `report_export.rs:561-574`): a legacy persisted row (`kind: None`) or a row with empty
    path and line 0 is never raised. Do not expect a floor to rescue a producer that fails
    to set kinds or locations.

14. **`finding_headline`/`finding_detail` are all-or-nothing** (section 5.6): author both or
    the authored half is ignored entirely.

15. **The in-file `domain` field does not set the domain** (section 3.4): the folder does.
    Moving a file IS a domain change; editing the field is not.

16. **`inspectable = false` makes a rule invisible to every scan surface, including the ones
    that look like coverage honesty.** It is not "scan it and suppress the findings" - the
    rule is removed before arming, so it has no ledger entry at all, and therefore appears
    neither in "verified clean", nor in the "we checked N rules" count, nor in "rules not run
    this scan". That last one surprises people: a rule excluded from the scan by design is
    not reported as excluded, because it was never a scan rule. Section 3.6a.

17. **`qualifies` is accepted inside `[[sources]]` and means nothing there - but it now fails
    the build.** `SOURCE_FIELDS` (`lib.rs:2149`) still lists it, so the unknown-field gate
    stays quiet (deliberately: the problem is placement, not spelling, and the dedicated gate
    says so), but `RuleSource` does not carry it and the text is discarded - the rule's
    summary then silently falls back to `[decision].why`. Ten `go/` rules shipped that way and
    19 more per-source annotations sat in the same key harmlessly-but-uselessly. Three gates
    now catch it (`lib.rs:2312` advisory, `lib.rs:2369` and W18's
    `corpus_rules_with_qualifies_nested_in_sources` build-failing, the last on ANY
    occurrence). The per-source content has its own field, `relevance`. Section 3.9.

18. **A `decision_with_floor` rule has no "no convention" exit, by design.** Its genuine
    options are equal-weight exactly like a `decision` rule's, so the absence of the escape
    reads as an inconsistency until you see why: the kind also names a FAULT, and letting a
    split repo answer "no convention adopted" would file that fault inside a row asserting
    nothing is wrong. A repo split across two legitimate options under this kind therefore
    reports Deviations, not a `NoConvention` row. Section 3.6.

---

## 10. Rule-authoring guide

### 10.1 Worked example: a `defect`-kind rule

Use when there is NO acceptable alternative anywhere - every option other than the correct
one is the absence or a degenerate form of the requirement. Security requirements belong
here.

```toml
id = "SEC-NO-EXAMPLE-SINK-1"
title = "User input never reaches the example sink unescaped"
domain = "universal"          # must match the folder; the folder wins (section 3.4)
# `layer` is parsed and then discarded; `tag` is DELETED and re-adding it fails the build
# (section 3.18). Author `layer` for human readers only.
layer = "universal"
enforcement = "structured"    # structured = human-reviewed; mechanical would promise a detector
kind = "defect"               # REQUIRED on every rule (build gate rules_missing_kind)
inspectable = true            # REQUIRED on every rule (build gate rules_missing_inspectable);
                              # true = the scan can assess this from the repo (section 3.6a)
default = true                # ships an adopted default option
verification = "grounded"     # grounded/verified/policy arm; draft/needs_recheck do NOT (gotcha 4)
# A floor, because the rule's own prose says "at least high" (W5-Q2: floor = minimum rating):
min_severity = "high"

[[sources]]                   # at least one external URL makes citations "grounded"
url = "https://cwe.mitre.org/data/definitions/79.html"
title = "CWE-79: Improper Neutralization of Input During Web Page Generation"

[decision]
question = "Does user-controlled input ever reach the example sink without escaping?"
default = "always-escape-before-the-sink"   # must exactly match an [[option]].id (gotcha 11)
why = """Client-readable rationale for the requirement. Also the summary fallback."""

[[option]]
id = "always-escape-before-the-sink"
label = "Escape all user-controlled input before the sink"
directive = "Detection recipe the MODEL reads: where to look, what counts as a violation. Never shown to a client."
why = "The natural fit: the sink executes whatever reaches it, so escaping at the boundary is the only safe shape."
remediation = """Route every user-controlled value through the escaping helper before it
reaches <path>'s sink. Verify by sending a probe payload and confirming it renders as inert
text."""
effort = "low"                # exactly low|medium|high (gotcha 12)

[[option]]
id = "raw-input-reaches-the-sink"
label = "User input reaches the sink unescaped"
# On a defect-kind rule this flag is DOCUMENTATION + gate signal: the rule never enters the
# alternatives pass, so nothing executes it (section 3.6). Keep it anyway - it records which
# option is the fault, and the DWE gate reads it if the kind ever changes.
violates = true
directive = "The degenerate form: input flows to the sink with no escaping anywhere."
why = "Rejected: this is the failure mode the rule exists to prevent - any visitor-controlled value executes in the sink's context."
remediation = """Move to the escaping helper at every sink call site. Verify with a probe
payload."""
```

Pipeline effect: never enters the alternatives pass; the chunk pass checks the default
option's directive; every finding is a `Defect`; `min_severity` raises any under-called row
to high at export.

### 10.2 Worked example: a `decision`-kind rule

Use when the alternatives genuinely hold equal weight and the ONLY reportable condition is
inconsistency. NO option may carry `violates = true` (stated at `lib.rs:172-174`; currently
unenforced by a gate, but a flag here will fire at runtime if that option is ever decided -
section 3.6 - so treat the invariant as hard).

```toml
id = "ARCH-EXAMPLE-ID-STYLE-1"
title = "Entity identifiers follow one representation project-wide"
domain = "api-layer"
layer = "universal"
enforcement = "structured"
kind = "decision"
inspectable = true
default = false               # honest: no externally-correct default exists; the architect
                              # (or the scan's recommendation pass) picks one.
verification = "grounded"
# Inconsistency under this rule compounds hard on future features? Raise the deviation cap:
deviation_ceiling = "high"    # default is "medium" when absent (section 3.17)

[[sources]]
url = "https://example.org/style-guide#identifiers"
title = "Example Style Guide - identifier representation"

[decision]
question = "How are entity identifiers typed: newtypes, bare UUIDs, or a generic phantom-typed ID?"
why = """All three are legitimate; the right one follows from how much the project values
compile-time distinction against representational simplicity."""
# no `default =` here, and rule-level default = false - the pair must agree WITH EACH OTHER.
# (`default` is orthogonal to `kind`; see gotcha 2 for the retracted "contradiction" claim.)

[[option]]
id = "newtype-per-entity"
label = "A distinct newtype per entity"
directive = "Every entity id is a wrapper type; a bare UUID never crosses a function boundary."
why = "A legitimate choice when compile-time ID safety outweighs wrapper overhead."
remediation = """Standardize on newtypes: wrap each bare UUID parameter and convert at the
persistence and HTTP boundaries. Verify by compiling with a deliberately swapped ID argument
and confirming a type error."""
effort = "medium"

[[option]]
id = "bare-uuid-everywhere"
label = "A bare UUID everywhere"
directive = "Entity ids are plain UUIDs with no wrapper types anywhere."
why = "A legitimate choice when representational simplicity and boundary interop dominate."
remediation = """Standardize on bare UUIDs: unwrap existing newtypes and delete the wrapper
types. Verify the wrappers no longer appear anywhere in the module tree."""
effort = "medium"
```

Pipeline effect: enters the alternatives pass with the
`[PURE DECISION - "no convention" is a valid answer]` marker; the decided option's directive
replaces the rule's directive for the violation passes; per-site rows become `Deviation`
(capped at `deviation_ceiling`); a clean repo gets one `ConventionObserved`; an honestly
split repo gets exactly one `NoConvention` row and zero per-site violations.

Note the remediation self-evidence: both options say "standardize on / move to" - legitimate
targets, no flags (section 10.5).

### 10.3 Worked example: a `default_with_exceptions`-kind rule

Use when ONE option is correct unless justified: deviating is a finding even when done
consistently. May mix legitimate justified alternatives (`violates = false`) with degenerate
forms (`violates = true`) on the same rule - and per the DWE gate, at least one option
SHOULD carry `violates = true` (`suspicious_default_with_exceptions_rules`,
`lib.rs:1820-1849`).

```toml
id = "ARCH-EXAMPLE-IDEMPOTENCY-1"
title = "Mutating endpoints deduplicate retries server-side"
domain = "api-layer"
layer = "universal"
enforcement = "structured"
kind = "default_with_exceptions"
inspectable = true
default = true                # this rule DOES ship an adopted default, so say so. Not required
                              # by the kind - see gotcha 2 - but true here.
verification = "grounded"

[[sources]]
url = "https://datatracker.ietf.org/doc/draft-ietf-httpapi-idempotency-key-header/"
title = "The Idempotency-Key HTTP Header Field"

[decision]
question = "How does the API deduplicate retried mutations?"
default = "idempotency-key-header"
why = """A retried POST must not double-charge or double-create; a server-side key check is
the correct default."""

[[option]]
id = "idempotency-key-header"
label = "Idempotency-Key header checked server-side"
directive = "Every mutating endpoint accepts an Idempotency-Key and replays the stored result on a duplicate."
why = "The natural fit for any API whose clients retry."
remediation = """Add the key check in the mutation middleware and store responses keyed by
(key, endpoint). Verify by replaying a request with the same key and confirming a single
side effect."""
effort = "medium"

[[option]]
id = "natural-idempotency-by-design"
label = "Mutations are naturally idempotent (PUT-by-id only)"
# A LEGITIMATE justified exception: no flag.
directive = "Every mutation is an idempotent PUT keyed by a client-supplied id; no dedup table is needed."
why = "Acceptable when the API genuinely exposes no non-idempotent mutation; the constraint itself is the mechanism."
remediation = """If a non-idempotent endpoint ever appears, move to the Idempotency-Key
option. Verify the route table still contains only PUT-by-id mutations."""
effort = "low"

[[option]]
id = "the-api-does-no-server-side-deduplication-at-all"
label = "No server-side deduplication at all"
violates = true               # the degenerate/absent form wearing the [[option]] shape
directive = "Retried mutations re-execute their side effects; nothing deduplicates."
why = "Rejected: a client retry (or a proxy replay) double-executes the side effect - the exact failure mode this rule guards against."
remediation = """Introduce the Idempotency-Key check on every mutating endpoint, starting
with payment and creation routes. Verify by replaying a captured request and confirming a
single side effect."""
effort = "high"
```

Pipeline effect: enters the alternatives pass WITHOUT the no-convention escape. If the scan
decides `natural-idempotency-by-design`, the directive is rewritten to it and per-site rows
become Deviations against it. If the scan decides the `violates` option, the directive stays
at the default, and a guaranteed high-severity `Defect` finding is emitted citing this rule,
carrying THAT option's remediation (section 5.9).

### 10.3a Worked example: a `decision_with_floor`-kind rule

Use when SEVERAL options hold genuinely equal weight - no privileged default, the choice
follows from project context - AND the rule separately names one or more shapes that are
faults under EVERY one of those options. The test for reaching for this kind rather than
`decision`: could you write a `Rejected:` sentence for one of the options without lying? If
yes, it is not a pure decision. The test against `default_with_exceptions`: is one of the
genuine options actually the right answer absent a reason to deviate? If no, there is no
default to name.

`RUST-DOMAIN-6` is the only member of this kind in the shipped corpus, and the membership is
asserted as an equality (section 3.6), so adding a second is a deliberate classification act
with recorded self-evidence, not a drive-by edit.

```toml
id = "ARCH-EXAMPLE-ERROR-SEAM-1"
title = "Repository errors cross the service boundary through one declared seam"
domain = "api-layer"
layer = "universal"
enforcement = "structured"
kind = "decision_with_floor"
inspectable = true
# No adopted default: the right seam depends on how many crates share the repository layer.
default = false
verification = "grounded"
deviation_ceiling = "high"    # drift between seams compounds across every call site

[[sources]]
url = "https://example.org/style-guide#error-boundaries"
title = "Example Style Guide - error boundaries"

[decision]
question = "How does a repository error reach the service layer: one shared error type, or a per-module error type mapped at the boundary?"
why = """Both are legitimate and the choice follows from how many consumers the repository
layer has. What is NOT legitimate is erasing the error entirely."""
# no `default =` here, deliberately: neither genuine option is privileged.

[[option]]
id = "one-shared-repository-error"
label = "A single shared repository error type"
directive = "Every repository returns the one shared error enum; services match on its variants."
why = "A legitimate choice when the repository layer has few consumers and the variant set stays small."
remediation = """Consolidate the per-module error types into the shared enum and update each
service's match arms. Verify by confirming no module declares its own repository error type."""
effort = "medium"

[[option]]
id = "per-module-error-mapped-at-the-boundary"
label = "A per-module error type mapped at the service boundary"
directive = "Each module declares its own error type and maps it to the service error at the boundary."
why = "A legitimate choice when modules evolve independently and a shared enum would couple them."
remediation = """Introduce a per-module error type and an explicit boundary mapping for each
module. Verify by confirming each module's error type is private to it."""
effort = "medium"

[[option]]
id = "errors-are-stringified-at-the-repository"
label = "Repository errors are flattened to strings"
# THE FLOOR. A fault under BOTH genuine options above, which is exactly the shape the
# three-way taxonomy could not express.
violates = true
directive = "Repository failures are converted to opaque strings before leaving the repository."
why = "Rejected: the caller cannot distinguish a not-found from a connection failure, so every recovery decision above the repository becomes a substring match."
remediation = """Replace the stringified returns with a typed error at the repository boundary,
starting with the paths a service branches on. Verify by confirming no service inspects an
error's text to decide control flow."""
effort = "high"
```

Pipeline effect: enters the alternatives pass WITHOUT the `[PURE DECISION]` marker, so the
model cannot answer "no convention adopted" (section 3.6, gotcha 18). If the scan decides
either genuine option, that option's directive replaces the rule's and per-site rows become
`Deviation`s capped at `high` by the authored `deviation_ceiling`. If the scan decides the
flagged option, the directive is left untouched and a guaranteed `Defect` anchored at high is
emitted carrying that option's remediation. A repo split between the two genuine options
reports deviations rather than a `NoConvention` row - that is the point of the kind.

### 10.4 The `Rejected:` convention

All 17 flagged `supabase/` rules open the violating option's `why` with the literal token
`Rejected:` - and that folder scored **17 of 17 correct** in the audit while other folders
scored as low as 59 percent. The template is why: an author forced to begin the option's
rationale with `Rejected:` cannot write it as a neutral alternative by accident, and a
reviewer (or a mechanical gate) can check flag/prose agreement with a string match.

**Recommended convention, stated as rules:**

- Every `violates = true` option's `why` BEGINS with `Rejected:` followed by the concrete
  failure mode.
- No `violates = false` option's `why` contains `Rejected:`.
- Remember the `why` text is client-visible when the option is decided (it is interpolated
  into the defect finding's detail, `ai_audit.rs:6314-6321`) - write the sentence after
  `Rejected:` for a client to read.

**This is now a BUILD GATE, not just a convention (W14).** The first edition of this document
proposed it; `options_with_rejected_prose_but_no_violates_flag` (`lib.rs:1916-1939`, token
constant `REJECTED_PROSE_TOKEN` at `lib.rs:1869`, matcher `why_opens_with_rejected` at
`lib.rs:1875-1877`, which trims leading whitespace so inline and `"""`-block strings behave
identically) implements it, and
`req_w14_every_rejected_option_is_flagged_violates`
(`crates/rules/tests/w14_corpus_schema_gates.rs:129`, fire proof at `:165`) fails the build.

Two scoping details matter:

- **The gate runs only on kinds that PERMIT the flag** (`lib.rs:1923`, via
  `RuleKind::permits_violates_flag`). A `decision`-kind rule is out of scope here, because
  GATE 2 already forbids it from carrying the flag at all and the two gates would otherwise
  contradict each other on the same option (`lib.rs:1889-1894`). An ABSENT kind deliberately
  stays in scope.
- **The decision-kind case routes to an ADVISORY gate instead.** A `decision` rule whose
  option opens with `Rejected:` is telling you the KIND is probably wrong, not that the flag
  is missing. `decision_kind_rules_with_rejected_prose` (`lib.rs:1963-1977`) reports those
  without failing the build, and the population is pinned to an equality roster
  (`DECISION_KIND_RULES_WITH_REJECTED_PROSE`,
  `crates/rules/tests/w14_corpus_schema_gates.rs:318`) so it cannot grow silently while the
  owner's A5.7 classification hold is open.

Note the gate checks ONE direction only: `Rejected:` prose implies the flag. The converse (a
flagged option whose `why` does not open with `Rejected:`) is still a convention, not a gate.

### 10.5 The self-evidence discipline

The audit's most reliable classifier was the rule's own authored text. When flagging or
reviewing an option, read its `remediation` and `why` and apply:

- **Remediation direction.** A remediation that says "move/migrate/switch **to** this
  option" marks a legitimate TARGET - no flag. One that says "consolidate **into** another
  option" / "introduce the <other option>" marks a FAULT being escaped - flag.
- **Blessing vocabulary** (no flag): "the natural fit", "a legitimate choice",
  "Acceptable when", "appropriate for".
- **Fault vocabulary** (flag): "Rejected:", "Forbidden", "the failure mode this rule guards
  against", "defeats", "never acceptable".
- If the vocabulary and the flag disagree, one of them is wrong - and in the audit it was
  the flag about three times out of four.

### 10.6 Corpus-load gates that will reject a rule

All in `crates/rules/src/lib.rs` unless noted; these are `#[cfg(test)]`-exercised build/CI
gates, never runtime refusals (a scan must never refuse to run over corpus state).

| Gate | Fails the build when | Why it exists |
|---|---|---|
| `rules_missing_kind` (`lib.rs:1726`) + `req_a5_1_...` | any rule has no `kind` | an unclassified rule must not silently behave permissively (section 3.6) |
| `rules_missing_inspectable` (`lib.rs:1756`) + `req_w16_every_corpus_rule_declares_inspectable` | any rule has no `inspectable` | guessing `true` ships an unassessable rule as "verified clean"; guessing `false` silently stops a real check (section 3.6a) |
| `rules_missing_version` / `rules_with_invalid_version` + `req_every_corpus_rule_declares_a_version_of_at_least_one` (`crates/rules/tests/rule_version_review_fields.rs`) | any rule has no `version`, or `version = 0` | the revision number the lock gate keys on; no default, so a bump is always a deliberate act (section 3.20) |
| `rules_with_invalid_last_reviewed` + `req_every_authored_last_reviewed_is_a_real_past_or_present_date` (same file) | `last_reviewed` is not a real `YYYY-MM-DD` date, or is in the future | a future or impossible review date would make a rule look fresher than anyone has verified (section 3.21) |
| `rules_with_unpaired_supersession` + `req_no_corpus_rule_has_half_a_supersession_claim` (same file) | exactly one of `superseded_since` / `current_recommendation` is set | a year with no replacement, or a replacement with no year, is half a claim (section 3.22) |
| `check_lock` + `req_the_bundled_lock_matches_the_bundled_corpus` (`crates/rules/tests/corpus_lock.rs`) | a rule's content changed without a `version` bump, or `CORPUS_LOCK.toml` is stale, missing a rule, or names a removed one | a version nobody is forced to bump rots; the lock makes the bump mechanical (section 3.23) |
| `options_with_rejected_prose_but_no_violates_flag` (`lib.rs:1916`) + `req_w14_every_rejected_option_is_flagged_violates` | an option's `why` opens with `Rejected:` but carries no `violates = true`, on any kind that permits the flag | the audit's single best-performing authoring signal, made mechanical (section 10.4) |
| `decision_kind_rules_with_violates_flag` (`lib.rs:2007`) + `req_w14_no_decision_kind_rule_carries_a_violates_flag` | a `decision`-kind rule carries any `violates` option | the flag is live at runtime regardless of kind, so a stray one manufactures a high-severity defect on an equal-weight rule (gotcha 3) |
| `corpus_files_with_unknown_fields` (`lib.rs:2208`) + `req_w14_no_corpus_rule_carries_an_unknown_field` | any TOML key outside the declared vocabulary, at seven nesting levels | `deny_unknown_fields` is off by design; this is the stronger replacement (gotcha 9) |
| `corpus_rules_losing_qualifies_text_to_sources` (`lib.rs:2369`) | a rule's authored `qualifies` sits inside `[[sources]]` AND the rule therefore loses its summary | ten `go/` rules shipped with a discarded summary (gotcha 17) |
| `corpus_rules_with_qualifies_nested_in_sources` + `req_w18_no_corpus_rule_authors_qualifies_inside_a_sources_block` | ANY `[[sources]]` block authors `qualifies`, summary-losing or not | the general shape, which the two narrower gates left a gap in: 19 annotations sat in it as a passing state until W18 gave them `relevance` (gotcha 17) |
| `req_w17_decision_with_floor_has_exactly_one_member` (`crates/rules/tests/w17_decision_with_floor_kind.rs:212`) | the `decision_with_floor` population differs from the approved roster | adding a rule to the fourth kind is a classification decision with recorded self-evidence, not a drive-by (section 3.6) |
| `req_w17_no_corpus_rule_carries_a_tag_field` (`crates/rules/tests/w17_tag_field_deleted.rs:85`) | any rule re-adds `tag` | the field was deleted on measured grounds; the unknown-field gate also rejects it (section 3.18) |
| `rules_disarmed_by_verification` (`lib.rs:1807`) + `req_h1_...` | any rule's `verification` makes it unarmable everywhere | the A7/A8 class: a repo-independent value disarming a rule forever (gotcha 4) |
| `rules_with_invalid_severity_ceiling` (`lib.rs:2426`) | `max_severity`/`deviation_ceiling` outside the vocabulary, or `deviation_ceiling` above `max_severity` | the `normalize_severity` low fall-through makes a typo the harshest cap (gotcha 1) |
| `rules_with_invalid_severity_floor` (`lib.rs:2501`) | `min_severity` outside the vocabulary, or above `max_severity` | a typo'd floor silently evaporates; floor above ceiling is unsatisfiable |
| `req_e2_the_corpus_actually_declares_severity_ceilings` / `req_w8_..._floors` (probes at `lib.rs:2545, 2561`) | the corpus declares ZERO ceilings/floors | anti-inertness: E2 shipped fully tested and fully inert once already |
| `every_rule_with_a_default_option_has_authored_remediation` (`lib.rs:3180`) | a rule's DEFAULT option lacks remediation | a curated finding must always be able to ship a Fix line |
| `every_deterministic_rule_option_with_remediation_has_an_authored_effort_band` (`lib.rs:4270`) | a CI-tier option has remediation but no `effort` | proven deterministic defects must not read "not estimated" forever |
| `suspicious_default_with_exceptions_rules` (`lib.rs:1837`) | ADVISORY, not build-failing by itself: a DWE rule with options and zero `violates` flags | misclassification or under-authoring signal. `decision_with_floor` is deliberately out of scope |
| `mechanical_rules_missing_detector` + `every_mechanical_rule_with_no_detector_is_code_auditable` (`mechanical_gate.rs:144, tests`) | a `mechanical` rule has no detector channel AND is not code-auditable | the only genuine phantom: checked by nothing at all |
| `req_h1_every_corpus_domain_is_emittable_by_some_stack` + witness-repo test (`propose.rs:843, 1049`) | a corpus folder/domain no stack can emit | gotcha 5 |
| `errors.is_empty()` assertions on lenient loads (e.g. `onboard.rs:2257`) | any TOML fails to parse | production ignores parse errors; tests are the only net (gotcha 10) |

Checklist when authoring or editing a rule:

1. Pick the `kind` honestly (section 3.6) from the FOUR values, declare `inspectable`
   (section 3.6a), and give the rule a `version` (section 3.20; `1` for a new rule). Set `default` to whether the rule genuinely ships an adopted default -
   that question is independent of `kind` (gotcha 2).
2. Set `verification` to an arming value (`grounded`/`verified`/`policy`) with real external
   `[[sources]]` when claiming grounded.
3. Make `[decision].default` match an `[[option]].id` character-for-character (gotcha 11).
4. Author `remediation` (default option at minimum) and `effort` (CI-tier) with exact
   vocabulary values.
5. Flag degenerate options `violates = true` and open their `why` with `Rejected:` - the
   gate enforces that pairing. Never flag an option on a `decision`-kind rule; if the rule
   genuinely has both equal-weight options and a named fault, its kind is
   `decision_with_floor`, not `decision`.
6. If the prose states a severity, encode it: "at least X" -> `min_severity`; "never more
   than X" -> `max_severity`; "inconsistency here is expensive" -> `deviation_ceiling`.
7. If this is a new stack/folder, wire detection + `domains_for_stack` + `domain_to_glob`
   and add a proposed-rules test (gotcha 5).
8. After any edit to an existing rule: bump its `version` (section 3.20), then run
   `camerata corpus lock` and commit the lock with the rule (section 3.23).
9. Run the camerata-rules test suite (and the server corpus tests) before considering the
   edit done - the gates above are the only thing standing between a typo and a silent
   no-op.

---

## 11. Open questions, inert fields, and inconsistencies

Found while writing this reference. Documented here per the no-code-changes constraint;
none are fixed by this document.

### Closed since the first edition (2026-10-11), kept for the record

- **~~Item 2: the "no `violates` on a `decision`-kind rule" invariant is unenforced.~~**
  CLOSED by W14's `decision_kind_rules_with_violates_flag` (`lib.rs:2007-2023`), with a
  vacuity guard. Gotcha 3.
- **~~Item 5: no mechanical `Rejected:` agreement gate.~~** CLOSED by W14's
  `options_with_rejected_prose_but_no_violates_flag` (`lib.rs:1916-1939`), one direction only
  (prose implies flag). Section 10.4.
- **~~Item 7: the RUST-DOMAIN-6 taxonomy gap.~~** CLOSED by W17's fourth `RuleKind`,
  `decision_with_floor` (`lib.rs:263`). The first edition's prediction held exactly: it
  needed no new pipeline behaviour, only a variant, two exhaustive predicate arms, gate
  scoping and the classification. **One prediction was wrong and is worth noting**: the first
  edition assumed the new kind would be allowed the no-convention escape "among the genuine
  options". It is NOT - granting it would let a split repo bury a named fault inside an
  informational row (section 3.6, gotcha 18).
- **~~Item 3, first half: no gate for the `default = false` + `default_with_exceptions`
  contradiction.~~** WITHDRAWN, not implemented. There is no contradiction; the claim and
  the proposed gate were both retracted. See gotcha 2.
- **Partially closed: item 9, production loads discard parse errors.** W13 routes them on the
  SCAN path (ledger, integrity gate, one client line, structured log). The proposal and
  alternatives paths still discard. See the item below and section 2.
- **Partially closed: item 1, fields with no consumer.** `tag` is deleted; `layer` and
  `[[emits]]` are now at least DECLARED as accepted-but-unconsumed rather than invisible to
  the schema. Nothing reads them. See the item below.

### Still open

Renumbered from the first edition; the closed items keep their original numbers in the
block above so an old cross-reference still resolves.

1. **Fields still read by NOTHING:** rule-level `layer` and the `[[emits]]` blocks
   (sections 3.18/3.19). They are now declared on `RuleToml` with `#[allow(dead_code)]`, so
   they are documented rather than invisible, but the `Rule` domain struct has no `layer`
   field at all and the value is dropped at conversion. Also the METHOD
   `EnforcementKind::emits_to_conventions` (`lib.rs:117-119`) has zero PRODUCTION callers
   (only five assertions in the crate's own test module); the live CONVENTIONS/AGENTS split
   is `arm.rs`'s own string-level `is_ci_tier` (`arm.rs:88`). Either wire them or remove
   them. If `layer` is ever wired, normalize it first: `go:testing` and
   `JAVASCRIPT-PUBLIC-ENV-PREFIX-SECRET-1` already disagree with their siblings.

2. **No gate that `[decision].default` resolves to a real option id**
   (gotcha 11 - note the remediation gate's filter_map silently SKIPS a rule with a dangling
   default, `lib.rs:3183-3195`, so it hides rather than catches that state).

3. **No vocabulary gate for option `effort`** (gotcha 12): presence is gated, value is not,
   and a bad value silently affects bucket routing via `matrix_bucket`'s exact
   `Some("low")` match (`report_export.rs:2541`).

4. **The unknown-field gate cannot catch a correctly-spelled key in the WRONG place.**
   `qualifies` is in both `RULE_LEVEL_FIELDS` and `SOURCE_FIELDS`, and means nothing at the
   second. THAT one case is now fully gated: three targeted gates cover it (`lib.rs:2312`
   advisory, `lib.rs:2369`, and W18's `corpus_rules_with_qualifies_nested_in_sources`, which
   fails the build on any occurrence). What remains open is the GENERAL shape - a vocabulary
   that is per-level yet overlapping gets a hand-written gate per overlap, and nothing
   enumerates the overlaps. Today `qualifies` is the only one. Gotcha 17.

5. **Unlocated rows are `info` on every surface (fixed 2026-10-21).** This was an open
   cross-artifact asymmetry: the PDF/JSON partition applied the C4-R1 unlocated downgrade and
   the live scan and xlsx/`findings.json` partition did not, so an unlocated
   above-informational row (e.g. an operator-forced `violates` finding, which carries no
   evidence and anchors at high, `ai_audit.rs:6271-6278`) rendered `info` in the PDF and `high`
   elsewhere. C4-R1's rationale is about the finding (nothing concrete was established, so it
   is not actionable at its stated severity), not about one artifact's presentation, so it
   moved into the shared chain (`severity_bounds::bounded_severity`: floor, C4-R1, ceilings).
   Consequence to know: such a row is also `info` in the live triage view from the moment it
   is emitted. `live_and_export_severities_agree_for_unlocated_findings` pins the agreement.

6. **`inspectable`'s exclusion from the integrity gate is an inference, not a tested fact.**
   An emit-only rule has no ledger entry, and `integrity_gate::evaluate`
   (`integrity_gate.rs:334`) reads only the ledger, so it cannot be in the gate's universe.
   That reasoning is sound but no test asserts it directly, unlike every other
   `inspectable` consequence, each of which is pinned in
   `crates/server/tests/w16_emit_only_rules_e2e.rs`. A one-line assertion would close it.

7. **`stack_exception.framework` is unvalidated** against the detectable-framework
   vocabulary (`detect_frameworks`, `propose.rs:54`): an exception naming a framework the
   detector never emits silently never fires - the same silent-no-op shape as gotcha 9, one
   level down.

8. **The PROPOSAL path still discards parse errors** (gotcha 10, section 2). W13 fixed the
   scan path only, and says so in its own test-module comment
   (`server/src/lib.rs:27363-27371`). `propose_corpus_rules` (`propose.rs:661`),
   `rescan_alternatives_handler` (`server/src/lib.rs:4275`) and roughly eight further server
   call sites still bind `_errs`, so a rule whose file will not parse is still silently
   absent from a proposal with nothing recorded. The cheap hardening is the same one W13
   applied one layer over: route the lenient loader's error list rather than dropping it.

9. **The 167 to 145 drop in the "rules checked" figure is a measurement, not a derivation.** The
    mechanism is certain (section 3.6a) but the delta of 22 does not equal the 26 emit-only
    rules, and reproducing which 22 a given stack selects requires running the proposal
    pipeline. Nothing in the test suite pins a "rules checked" number against a known
    fixture repo, so there is no regression guard on the count itself - only on each rule's
    individual exclusion. Quote the figure as one observed run or not at all.
