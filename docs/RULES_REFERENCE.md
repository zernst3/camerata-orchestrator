# Camerata Rules Reference

The authoritative functional reference for every field and flag a corpus rule TOML can carry:
what the engine actually DOES with each value, where it is consumed, what happens when it is
absent or malformed, and which other fields it interacts with.

Every claim cites `file.rs:line`. Line numbers were taken from the working tree on 2026-10-11,
branch `feat/audit-report-export`. Two agents were concurrently editing rule `kind` values and
`violates` flags (and `crates/rules/src/lib.rs` itself) at the time of writing, so corpus
COUNTS quoted here are dated to that day and line numbers in actively edited files may drift by
a few lines; function names will not. Where source comments are quoted, any dash characters in
the original have been rendered as hyphens per house style.

Scope note: this document describes the schema and the engine. It deliberately does not
snapshot per-rule classification values that are mid-migration (the supabase/sec/sql rules
moving to `kind = "defect"`, and the architectural rules moving to `kind = "decision"`). The
semantics documented here are what those migrations are converging ON.

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
  - [3.18 `tag` and `layer` (INERT)](#318-tag-and-layer-inert)
  - [3.19 `[[emits]]` (INERT)](#319-emits-inert)
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
  - [10.4 The `Rejected:` convention](#104-the-rejected-convention)
  - [10.5 The self-evidence discipline](#105-the-self-evidence-discipline)
  - [10.6 Corpus-load gates that will reject a rule](#106-corpus-load-gates-that-will-reject-a-rule)
- [11. Open questions, inert fields, and inconsistencies](#11-open-questions-inert-fields-and-inconsistencies)

---

## 1. The pipeline in one page

A rule's fields are consumed at six distinct stages. Knowing which stage reads a field is the
fastest way to predict what changing it does.

1. **Load** (`camerata_rules::load_corpus` / `load_corpus_lenient`,
   `crates/rules/src/lib.rs:1180/1195`). TOML files under `crates/rules/principles/` (override:
   `CAMERATA_CORPUS_PATH`, `lib.rs:1166`) parse into `Rule` structs. The folder path, not the
   in-file `domain` field, decides the rule's domain (`lib.rs:1285-1324`). Unknown field names
   are silently ignored (`deny_unknown_fields` is deliberately NOT set, `lib.rs:391-392`).

2. **Proposal / selection** (`onboard/propose.rs`). `detect_stack` (`propose.rs:271`) maps the
   repo to corpus domains via `domains_for_stack` (`propose.rs:527`); `propose_corpus_rules`
   (`propose.rs:654`) lists EVERY corpus rule but marks a subset `recommended` and a
   narrower subset `is_auto_recommended` (`propose.rs:771, 784-786`). An unattended scan runs
   exactly the `is_auto_recommended` set (`crates/cli/src/inspect_cmd.rs:155-165`). Fields
   consumed here: folder-derived `domain`, `extra_domains`, `verification`, `opt_in_only`,
   `enforcement` (for the placement string), plus `options`/`default_option`/`sources`/
   `decision.*` carried to the UI.

3. **Directive resolution**. Each selected rule becomes a `SelectedRule { id, directive, repos }`
   (`onboard.rs:877-892`). The directive is the chosen option's `directive`, else the
   `default_option`'s, else the rule `summary`/`title`
   (`inspect_cmd.rs:171-181`, `server/src/lib.rs:8663-8676`, `lib.rs:15083-15088`).

4. **Scan passes** (`ai_audit.rs`, `onboard/audit.rs`, `scan_tools.rs`,
   `onboard/architectural.rs`). Deterministic floor checks, external-tool previews
   (`enforcement = "mechanical"` + a routable `[[sources]].linter`, minus `layer3_only`), the
   architectural checkers, the semantic AI pass (EVERY selected code-auditable rule, as of W4,
   `onboard.rs:894-950`), and the alternatives/recommendation pass for multi-option semantic
   rules (`onboard/audit.rs:407-414`, `ai_audit.rs:6565-6718`). Fields consumed: option
   `directive`/`why`/`label`/`violates`, rule `kind`, `stack_exceptions`, `layer3_only`,
   `sources.linter`, `escalation`.

5. **Kind assignment and informational emissions** (`ai_audit.rs:7087-7123`). Model-reported
   rows are `Defect` at parse time (`ai_audit.rs:625-630`); `assign_deviation_kinds`
   reclassifies rows under a decided non-`violates` option as `Deviation`
   (`ai_audit.rs:6239-6295`); `ConventionObserved`, `NoConvention`, and `NotAssessed` rows are
   synthesized (`ai_audit.rs:6311-6482`).

6. **Export** (`report_export.rs`, `xlsx_export.rs`). The severity floor/ceilings
   (`min_severity`, `max_severity`, `deviation_ceiling`) are applied here, at both exporters'
   partitions; `remediation`, `finding_headline`/`finding_detail`, `effort`, and `sources`
   resolve into the client artifacts (PDF, xlsx, `findings.json`). Bucket routing
   (`effective_bucket`, `report_export.rs:2478-2523`) decides whether a row is an action item,
   informational, or held for review.

---

## 2. Loading and parsing

- **Entry points.** `load_corpus` fails the whole load on the first bad file
  (`lib.rs:1180-1188`); `load_corpus_lenient` skips bad files and returns `(RuleSet,
  Vec<(PathBuf, RulesError)>)` (`lib.rs:1195-1212`). Production call sites overwhelmingly use
  the lenient loader and most DISCARD the error list (e.g. `server/src/lib.rs:4257`
  `let (corpus, _errs) = ...`; `propose.rs:661` `let (set, _errs) = ...`). The consequence is
  gotcha 10 in section 9: a value-level typo in a strict enum field makes the whole file
  unparseable, and in production that rule then silently vanishes from every proposal and scan.
  The backstop is the test suite, where corpus-loading tests assert `errors.is_empty()`
  (e.g. `onboard.rs:2159-2160`).

- **Unknown FIELD names never error.** `RuleToml` deliberately does not set
  `serde(deny_unknown_fields)` "so future corpus fields are silently ignored rather than
  breaking the loader" (`lib.rs:391-392`). A misspelled field name (`max_severety = "high"`) is
  therefore not a parse error and not a gate failure; it is simply not there. See gotcha 9.

- **Blank-string scrubbing.** `load_one` filters empty/whitespace-only strings to `None` for
  option `remediation`/`finding_headline`/`finding_detail`/`effort` (`lib.rs:1362-1377`) and
  for rule `max_severity`/`min_severity`/`deviation_ceiling` (`lib.rs:1407-1409`). Authoring
  `remediation = ""` is exactly equivalent to omitting the field.

- **File discovery.** Only `*.toml` files are read, recursively, sorted for deterministic load
  order (`lib.rs:1214-1253`). A rule file sitting directly in the corpus root (no subfolder)
  falls back to domain `"universal"` (`lib.rs:1302-1306`).

---

## 3. Rule-level fields

### 3.1 `id`

- **Type / required.** String, required and non-empty; an empty `id` is a hard
  `MissingField` load error even in lenient mode (that file is skipped) (`lib.rs:1275-1280`).
- **What the engine does with it.** The primary key for everything: `RuleSet::get_by_id`
  indexing (`lib.rs:1020`), finding attribution (`Finding::rule_id`), detector wiring
  (`mechanical_gate::detector_channel`, `mechanical_gate.rs:86-110`, matches gateway arms,
  arch-checker ids, and Semgrep coverage BY ID), suppression/waiver matching, and the
  export-layer corpus join (`report_export::corpus_rule_for`, `report_export.rs:1633-1643`).
- **Prefix semantics are load-bearing.** Three id-prefix checks change behavior by identity:
  - `ORCH-` / `SPIRIT-` / `PROC-` mark a rule governance/process rather than code
    (`onboard/audit.rs:357-359`, `is_code_auditable_rule`). As of W4 this NO LONGER keeps a
    rule out of the semantic prompt (that exclusion was removed; see the long history note at
    `onboard.rs:894-936`); it still excludes the rule from the ALTERNATIVES pass via
    `is_semantic_multi_option_rule` (`onboard/audit.rs:407-414`).
  - `ARCH-OBSERVABILITY-` routes every finding of the rule to the informational bucket and
    caps its severity at low, BY RULE IDENTITY, regardless of model output
    (`server/src/lib.rs:5617, 5639-5641`; consumed at `ai_audit`'s D8 ceiling pass and FIRST
    in `report_export::is_informational`, `report_export.rs:2799-2816`). This is gotcha 7.
  - `AI-` prefixed ids are invented by the model for novel findings (`ai_audit.rs:642`);
    they never resolve in the corpus and ride the P3 citation-class fallback instead.
- **Client-facing?** Yes: rule ids appear in every artifact (PDF citations, xlsx columns,
  `findings.json`).

### 3.2 `title`

- **Type / required.** String, required (serde will fail the file without it,
  `lib.rs:395-396`).
- **What the engine does with it.** Human label everywhere: proposal rows
  (`propose.rs:749`), the xlsx `rule_title` fallback join (`xlsx_export.rs:654-659`), the
  CONVENTIONS/AGENTS emit header (`arm.rs:95-99` renders `### {id} - {title}`), and the
  final fallback for both `summary` (`lib.rs:1327-1337`) and a selected rule's directive when
  no option resolves (`inspect_cmd.rs:171-181`). Reaches every client artifact.

### 3.3 `enforcement`

- **Type / required.** Required enum: `"prose" | "structured" | "mechanical" |
  "architectural"` (`lib.rs:68-75`). An unrecognized value fails the FILE's parse (strict
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
    out of the AI prompt (`onboard.rs:894-950`); the model is a deliberate second pair of
    eyes over deterministic rules too.
  - **Informational stance demotion.** `Structured` + a domain in `STANCE_LAYER_DOMAINS`
    (`universal`, `api-layer`, `fullstack`, `ui`, `javascript`, `integration`) + an
    absence-type finding (`located == false`) at medium-or-below routes the finding to the
    informational appendix (`report_export.rs:2742-2743, 2830-2839`).
  - **Build gates.** `mechanical_rules_missing_detector` only inspects `Mechanical` rules
    (`mechanical_gate.rs:144-157`); the effort-band gate only inspects CI-tier rules
    (`lib.rs:3376-3406`).
  - **Proposal placement string.** Four honest placement descriptions, one per tier
    (`propose.rs:724-737`); the UI "kind" collapses to `mechanical` vs `review`
    (`propose.rs:716-720`).
  - **Emit.** `arm.rs:88-92, 100-105` adds a `_Conformance:_` line only for CI-tier rules.
    `EnforcementKind::emits_to_conventions` (`lib.rs:117-119`) has NO consumer anywhere in
    the workspace as of 2026-10-11 (checked by grep); the CONVENTIONS/AGENTS partitioning is
    done by `arm.rs`'s own string-level `is_ci_tier`, not this method. See section 11.
- **Default / absent.** No default; the field is required.

### 3.4 `domain` (in-file) and the folder-derived domain

- **The folder path is the real domain.** `Rule::domain` is DERIVED from the TOML's parent
  folder relative to the corpus root, components joined with `:` (e.g.
  `rust/dioxus/x.toml` -> `"rust:dioxus"`, `universal/x.toml` -> `"universal"`)
  (`lib.rs:1285-1309`).
- **The in-file `domain` field is nearly inert.** When present it is only COMPARED against the
  derived value; disagreement logs a `tracing::warn!` and the derived value always wins
  (`lib.rs:1314-1324`). `"*"` is normalized to `"universal"` for that comparison only. Moving
  a file between folders changes its domain even if the field says otherwise. Keep the field
  accurate for human readers, but know it decides nothing.
- **What the derived domain gates.** Whether the rule can ever be SUGGESTED for a repo:
  `propose_corpus_rules` matches a repo when its stack's domain set contains the rule's
  `domain` OR any of its `extra_domains` (`propose.rs:685-692`); `domain == "universal"`
  matches every repo (`propose.rs:677`). A rule whose domain NO stack can emit can never be
  suggested, never auto-armed, and therefore never runs on any repo, ever - the A6 defect
  class (94 of 417 rules were in that state). Two build gates now enforce reachability:
  `req_h1_every_corpus_domain_is_emittable_by_some_stack` (`propose.rs:843-876`, checking the
  corpus against `emittable_domains()`, `propose.rs:497`) and
  `req_a6_every_corpus_domain_has_a_detectable_witness_repo` (`propose.rs:1041-1081`, proving
  each domain is reachable end-to-end from a synthetic repo through `detect_stack` ->
  `domains_for_stack`).
  - Some domains are pipeline-selected WITHOUT a stack match at all
    (`PIPELINE_SELECTED_DOMAINS`, `propose.rs:331-355`), and child domains expand to parents
    (`expand_parent_domains`, `propose.rs:478`).
- **Downstream consumers of the domain string.** The stance-layer informational demotion
  (`report_export.rs:2742-2743`); `role_from_corpus` / `select_for_domains` for fleet role
  construction (`lib.rs:1771-1776, 1829-1870`, used by `crates/fleet/src/lib.rs:88` and
  `crates/cli/src/live_demo.rs:58`); and `domain_to_glob` (`lib.rs:1890-1912`), which maps a
  domain to the role's allowed-path glob - an unmapped domain falls back to `**` with a
  warning. **When adding a new stack rulepack you must wire all three: stack detection in
  `propose.rs`, `domains_for_stack`, and `domain_to_glob`** - the Supabase pack originally
  shipped without this wiring and its rules were orphaned (never proposed).
- **`domain == "agentic"` is special-cased:** always `recommended` in proposals regardless of
  stack (`propose.rs:771, 784`).

### 3.5 `extra_domains`

- **Type / default.** `Vec<String>`, absent -> empty (`lib.rs:446-448`). 5 rules used it on
  2026-10-11.
- **What the engine does with it.** A second, explicit domain-match axis in
  `propose_corpus_rules` ONLY: a repo matches when its domain set contains the rule's folder
  domain OR any entry here (`propose.rs:685-692`). Lets a rule whose premise generalizes
  beyond its home folder (e.g. a `supabase:database-functions` rule that is really about any
  Postgres `SECURITY DEFINER` function, carrying `extra_domains = ["sql"]`) arm on a broader
  stack without moving the file out of its corpus family (`lib.rs:711-720`).
- **Not consumed anywhere else.** It does not affect `select_for_domains`, roles,
  `domain_to_glob`, or the stance-layer check - those read only the folder-derived `domain`.

### 3.6 `kind` (the RuleKind taxonomy)

The three-way classification of how a multi-option rule's alternatives relate to the rule's
underlying requirement. Orthogonal to `enforcement` (which classifies HOW a rule is checked,
not what its alternatives MEAN). The source doc comment at `crates/rules/src/lib.rs:132-160`
is authoritative; quoting it (dashes rendered as hyphens):

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

**What each value functionally does, consumer by consumer:**

- **`kind = "defect"`:**
  - `Rule::is_defect_kind()` (`lib.rs:917-919`) makes `is_semantic_multi_option_rule` return
    `false` regardless of option count (`onboard/audit.rs:400-414`), so the rule NEVER enters
    the alternatives/recommendation pass. Its directive is never rewritten; the per-chunk
    violation pass checks the rule's selected/default option directive exactly like a
    single-directive rule. Its findings stay `FindingKind::Defect` (assigned at parse time,
    `ai_audit.rs:625-630`).
  - Consequence: on a `defect`-kind rule, `violates` flags, the `[PURE DECISION]` marker, the
    not-applicable drop, and the no-convention escape are all unreachable at runtime. The
    `violates` flags remain valuable as authored documentation and as the gate signal
    (section 10.4), but nothing executes them.
  - A `defect`-kind rule also never produces `Deviation`, `ConventionObserved`, or
    `NoConvention` rows (all three are keyed off the recommendation records the alternatives
    pass produces, `ai_audit.rs:6239-6448`). `deviation_ceiling` is therefore inert on it
    (gotcha 6).
- **`kind = "decision"`:**
  - Eligible for the alternatives pass (when also non-CI-tier, non-gate-armed,
    code-auditable, and >= 2 options; `onboard/audit.rs:407-414`).
  - The ONLY kind offered the no-convention escape: `build_alternatives_block` appends the
    `[PURE DECISION - "no convention" is a valid answer]` marker exclusively when
    `kind == Some(Decision)` (`ai_audit.rs:2195-2203`), and
    `parse_alternative_recommendations` honors a `no_convention` claim ONLY for a
    `Decision`-kind rule AND only when backed by a validated observed-options breakdown or a
    real evidence citation (`ai_audit.rs:2398-2421`); an unbacked or wrong-kind claim is
    corrected and flagged `hallucinated` (`ai_audit.rs:2502-2517`).
  - Per the source, no option should carry `violates = true`. NOTE: this invariant is stated
    but NOT gated (no `decision_rules_with_violates` corpus gate exists as of 2026-10-11),
    and at runtime a `violates` flag on a decision rule IS live: if the model/operator picks
    that option, `apply_alternative_decision` emits the defect finding exactly as it would
    for a `default_with_exceptions` rule (`ai_audit.rs:6163-6214` branches only on
    `option.violates`, never on the rule's kind). See section 11.
- **`kind = "default_with_exceptions"`:**
  - Eligible for the alternatives pass on the same conditions; NOT offered the no-convention
    escape (an externally-correct default exists, so "no convention" is not an honest
    outcome; `ai_audit.rs:2195-2199`).
  - Expected to mix `violates = false` (legitimate justified alternatives) and
    `violates = true` (degenerate forms) options. A DWE rule with options and ZERO `violates`
    flags is flagged by `suspicious_default_with_exceptions_rules` (`lib.rs:1482-1494`) as
    either misclassified (should be `decision`) or under-authored - this is advisory output
    for review, not a build failure by itself (its doc comment says callers that want it
    enforced assert on it directly).
- **Absent `kind`:**
  - Parses fine (`Option<RuleKind>` with a bare `serde(default)`, `lib.rs:449-459` - the doc
    comment explains an absent kind must NOT silently resolve to a permissive value like
    `Decision`), but the corpus-load gate `rules_missing_kind` (`lib.rs:1431-1439`, asserted
    by `req_a5_1_every_corpus_rule_declares_a_kind`) fails the BUILD on any corpus rule with
    `kind == None`. At runtime, `is_defect_kind()` is conservatively `false` for `None`
    (`lib.rs:908-919`), so an unclassified rule WOULD enter the alternatives pass if it ever
    reached a scan - the gate is what prevents that.
  - A typo'd kind VALUE (`kind = "defct"`) is a different failure: the enum deserialization
    fails, the whole file fails to parse, and the lenient production loader drops the rule
    entirely (gotcha 10) - `rules_missing_kind` cannot catch a rule that never loaded; the
    `errors.is_empty()` assertions in the test suite are what catch it.

### 3.7 `default`

- **Type / default.** Boolean, absent -> `false` (`lib.rs:408-411`).
- **What the engine does with it.** It is a GATE on `[decision].default`, nothing more:
  `load_one` only populates `Rule::default_option` when `default = true` AND
  `[decision].default` names a non-empty option id (`lib.rs:1340-1347`). With
  `default = false` (or absent), any `[decision].default` in the file is silently discarded.
- **Everything that then hangs off `default_option`:**
  - `Rule::resolved_option(chosen)` resolves `chosen_option` first, then `default_option`,
    else `None` (`lib.rs:848-851`). That resolution feeds:
    - the Fix text (`report_export::resolve_fix`, `report_export.rs:2022-2040`),
    - the authored floor headline/detail (`resolve_floor_finding_text`,
      `report_export.rs:2091-2108`),
    - the authored effort band (`resolve_effort`, `report_export.rs:3242-3253`),
    - the active escalation spec (`Rule::selected_escalation`, `lib.rs:875-877`, consumed at
      `server/src/lib.rs:2138-2161` and `test_tamper.rs:169`),
    - the default directive an untouched scan checks
      (`inspect_cmd.rs:171-181`, `server/src/lib.rs:8663-8676`),
    - the alternatives pass's "currently selected" marker and fallback target
      (`onboard/audit.rs:455-458`, `ai_audit.rs:2206-2217, 2353-2358`).
  - The remediation corpus gate `every_rule_with_a_default_option_has_authored_remediation`
    (`lib.rs:2286-2308`) only applies to rules WITH a resolvable default option; a rule with
    no default is deliberately out of scope ("no finding ever resolves a Fix line for such a
    rule until a project explicitly configures a `chosen_option`").
- **The semantics in words.** `default = true` means "this rule ships an adopted default
  option"; `false` means "the architect MUST choose an alternative at onboarding"
  (`lib.rs:408-410, 687-689`). That is why **`default = false` combined with
  `kind = "default_with_exceptions"` is a contradiction** (gotcha 2): the kind asserts "one
  option is correct unless justified" while the field asserts "no default exists". Eleven
  rules carried that contradiction at audit time, and it correlated strongly with wrong
  `violates` flags. There is no build gate for this pair as of 2026-10-11 (section 11).
- **Functional cost of a missing default on a multi-option rule.** Until a project persists a
  `chosen_option`, `resolved_option(None)` is `None`, so findings under the rule ship with NO
  Fix line, NO authored floor text, NO authored effort band, and NO active escalation - every
  one of those joins degrades to its honest-gap fallback.

### 3.8 `verification`

- **Type / default.** Enum `"draft" | "policy" | "grounded" | "verified" | "needs_recheck"`;
  absent -> `Draft` (`lib.rs:245-264`, `serde(default)` at `lib.rs:429-431`).
- **What the engine does with each value.** The ONLY runtime consumer that changes scan
  behavior is `Rule::is_auto_recommended()` (`lib.rs:978-983`):
  `Grounded | Verified | Policy` -> auto-recommended (pre-checked in the proposal AND selected
  by an unattended scan via `curated_rule_selection`, `inspect_cmd.rs:155-165`);
  `Draft | NeedsRecheck` -> listed in the proposal but NOT pre-checked and NOT run unattended.
  - `Policy` was ADDED to the auto-recommended set in Pass-1 W4 (finding A8): provenance
    ("this encodes a project decision, no external authority") is not a statement about
    whether the rule is worth running; excluding it silently disarmed 17 shipped rules on
    every repo, including `SPIRIT-FILE-SIZE-1` (`lib.rs:966-977`).
  - `is_grounded()` / `is_shippable()` still return `false` for `Policy` and `Draft`
    (`lib.rs:297-312`) - those answer the provenance question and feed the citation story,
    not arming.
- **Absent or stale.** Omitting `verification` entirely defaults to `Draft`, which disarms the
  rule EVERYWHERE, FOREVER (a verification value is repo-independent). This is exactly what
  shipped as finding A7: `UI-UTC-DATES-1` omitted the field, defaulted to `Draft`, and never
  armed on any repo while being the only `ui` rule with a shipped deterministic detector
  (`lib.rs:1441-1459`). Gotcha 4.
- **The gate.** `rules_disarmed_by_verification` (`lib.rs:1461-1469`), asserted by
  `req_h1_no_corpus_rule_is_disarmed_by_its_verification_value`, now fails the BUILD on any
  corpus rule whose verification value makes `is_auto_recommended()` false. Practically:
  every shipped rule must be `grounded`, `verified`, or `policy`.
- **`verified` is human-only.** No automated process may ever emit it; `needs_recheck` is what
  the staleness pass demotes a drifted verification to (`lib.rs:232-240`).
- **Client-facing?** The string is carried on proposals (`propose.rs:754`) for the UI; it does
  not reach the PDF/xlsx.

### 3.9 `[[sources]]`

- **Type / default.** Array of `{ url, title, linter? }` tables; absent -> empty
  (`lib.rs:340-351, 433-435`).
- **What the engine does with them:**
  - **Citations.** `resolve_citation` joins the finding's rule id to the corpus and cites
    sources VERBATIM when at least one has an external (`http(s)://`) URL - citation kind
    `"grounded"` (`report_export.rs:1616-1673`). Internal/non-URL sources are filtered out.
    A grounded citation is what keeps an AI-tier finding out of the uncited-AI held/informational
    routing (`is_uncited_ai_finding`, `report_export.rs:1977-1982`) and what ranks the row in
    the provenance-tier sort (`provenance_tier_rank`, `report_export.rs:2708-2714`). Titles
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
- **Absent / malformed.** No sources -> citation kind degrades to `"preview"` (when a
  `preview_tool` produced the row) or `"advisory"` (`report_export.rs:1674-1693`); for an
  AI-tier finding that stays advisory after the class fallback, the row is EXCLUDED from
  `curated_findings` and routed held/informational (`report_export.rs:1964-1982, 2489-2495`).

### 3.10 `[verified]`

- **Type / default.** Optional table `{ by, at, against = [] }`; absent -> `None`
  (`lib.rs:371-382, 437-440`).
- **What the engine does with it.** Provenance record behind `verification = "verified"`: who,
  when, and the exact source/linter versions verified against, for a staleness pass (in
  `camerata-checks`) that demotes to `needs_recheck` on drift (`lib.rs:353-362`). Not consumed
  by the scan or export pipelines; not client-facing.

### 3.11 `opt_in_only`

- **Type / default.** Boolean, absent -> `false` (`lib.rs:413-418`). 13 rules on 2026-10-11
  (e.g. `CICD-CODEQL-SECURITY-SCAN-1`, `CICD-SEMGREP-SECURITY-SCAN-1`).
- **What the engine does with it.** Consumed ONLY in `propose_corpus_rules`: it strips both
  the `recommended` badge and the `is_auto_recommended` pre-check
  (`propose.rs:771, 784-786`), even when the rule is grounded and stack-relevant. The rule
  still appears in the proposal list for a deliberate human opt-in (`lib.rs:885-891`).
- **Functional consequence.** Because unattended scans select exactly the
  `is_auto_recommended` set (`inspect_cmd.rs:155-165`), an `opt_in_only` rule NEVER runs in
  an unattended scan. It can only run when a human ticks it.

### 3.12 `layer3_only`

- **Type / default.** Boolean, absent -> `false` (`lib.rs:420-424`). 6 rules on 2026-10-11.
- **What the engine does with it.** Marks a CI-tier rule too heavy to run at scan time or
  layer 2 (e.g. CodeQL's whole-program DB build). Consumed by `group_by_tool`, which drops
  the rule from the scan-time preview pass (`scan_tools.rs:503`; re-checked defensively in
  `run_scan_tools`, `scan_tools.rs:1196`). It does NOT affect selection, the semantic prompt,
  or export.
- **Honest caveat.** The intended "layer-3 runner" consumer ("Part B", `lib.rs:703-706`) is
  carried-for-the-future; today the flag's only executed effect is the preview exclusion.

### 3.13 `[[stack_exception]]`

- **Type / default.** Array of `{ framework, path_glob, note }` blocks; absent -> empty
  (`lib.rs:441-444, 803-815`). Exactly 1 rule used it on 2026-10-11
  (`fullstack/arch-monolith-first-1.toml`).
- **What the engine does with it.** A finding-SUPPRESSION mechanism for platform-idiomatic
  patterns: `Rule::stack_exception_for(frameworks, path)` returns the first exception whose
  `framework` is present in the DETECTED stack (case-insensitive match against
  `RepoStack::frameworks`) AND whose `path_glob` matches the finding's file path
  (`lib.rs:858-869`; glob semantics: `*` matches ANY run of characters including `/`, so one
  `*` behaves like `**` - `lib.rs:817-843`). `ai_audit::apply_stack_exceptions`
  (`ai_audit.rs:5448+`) drops matching findings, called from the scan pipeline with ledger
  accounting (`onboard.rs:1787-1806`, stage `stack_exception_excluded`).
- **`note` is internal documentation only** - never rendered client-facing (`lib.rs:812-814`).
- **Absent/malformed.** No exceptions -> the rule behaves exactly as before the field existed.
  An exception naming a framework the detector never emits simply never fires (there is no
  gate validating `framework` against the detectable-framework vocabulary; see section 11).

### 3.14 `qualifies` (and the derived `summary`)

- **Type / default.** Optional string (`lib.rs:403-405`). 201 rules carried one on 2026-10-11.
- **What the engine does with it.** Source #1 for the derived `Rule::summary`:
  `qualifies` (non-empty) -> else `[decision].why` (non-empty) -> else `title`
  (`lib.rs:1327-1337`).
- **Where `summary` goes.** It is the FALLBACK directive text when a rule has no resolvable
  option: the arm/emit path (`server/src/lib.rs:8663-8676`), and the rule-drift diff's
  `resolved_directive` (`server/src/lib.rs:15083-15088`); it is also hashed into the
  rule-drift baseline (`server/src/lib.rs:15037`). It does not reach the PDF/xlsx findings
  artifacts.

### 3.15 `max_severity`

- **Type / default.** Optional lowercase severity string (`"critical" | "high" | "medium" |
  "low" | "info"`); absent -> no ceiling (`lib.rs:460-469, 727-736`). Deliberately a string,
  not an enum, matching `Finding::severity`'s vocabulary. 11 rules on 2026-10-11
  (`rules_declaring_max_severity`, `lib.rs:1669-1680`, is the anti-inertness probe).
- **What the engine does with it.** `report_export::clamp_to_rule_max_severity`
  (`report_export.rs:498-515`) clamps a finding's severity DOWN to the ceiling when it ranks
  above it; it never raises. It is applied at BOTH export choke-points, and it has the last
  word in the ordinary chain:
  - PDF/JSON partition: normalize -> floor -> unlocated downgrade -> **max clamp** ->
    deviation clamp (`report_export.rs:3640-3660`).
  - xlsx/`findings.json` partition: normalize -> floor -> **max clamp** -> deviation clamp
    (`xlsx_export.rs:325-342`).
  (The deviation clamp runs after it, but being itself a ceiling it can only go lower; the
  gate at `lib.rs:1563-1573` rejects a `deviation_ceiling` ranking ABOVE `max_severity` as
  unreachable.)
- **Resolution.** The rule is looked up through `corpus_rule_for`
  (`report_export.rs:1633-1643`), so an external-tool finding whose `rule_id` is the TOOL's
  own id still resolves to the grounded corpus rule that may carry the ceiling. Absent
  corpus, unknown rule, or absent field all degrade to a no-op.
- **It clamps EVERYTHING under the rule**, including the anchored `"high"` of a
  `violates`-option finding - the doc at `ai_audit.rs:6151-6154` names `max_severity` as the
  author's escape hatch when that anchor is too aggressive for a specific rule.
- **Malformed value: THE trap (gotcha 1).** The clamp resolves the ceiling through
  `normalize_severity`, whose fall-through maps EVERY unrecognized string to `"low"`
  (`report_export.rs:468-479`). So `max_severity = "critcal"` does not error and does not
  no-op - it becomes the harshest ceiling in the vocabulary and caps every finding of the
  rule at low. The corpus-load gate `rules_with_invalid_severity_ceiling`
  (`lib.rs:1534-1577`, validating against `CEILING_SEVERITY_VOCABULARY`, `lib.rs:1501`)
  exists precisely to fail the build on the typo before it ships.
- **History worth keeping.** The whole mechanism shipped fully implemented and fully
  unit-tested against a corpus in which NOT ONE of 417 rules declared a ceiling - complete
  and completely inert (`lib.rs:1645-1651`). `rules_declaring_max_severity` plus
  `req_e2_the_corpus_actually_declares_severity_ceilings` is the anti-inertness gate written
  in response.

### 3.16 `min_severity`

- **Type / default.** Optional lowercase severity string, same vocabulary; absent -> no floor
  (`lib.rs:470-477, 737-770`). 41 rules on 2026-10-11 (`rules_declaring_min_severity`,
  `lib.rs:1653-1660`, is the anti-inertness probe and is asserted NON-EMPTY by
  `req_w8_the_corpus_actually_declares_severity_floors`).
- **Why it exists.** Roughly two dozen rules state a severity in prose ("Flag as high
  severity"). The owner ruled that phrasing is a FLOOR: "Floor, it's the minimum rating"
  (`lib.rs:740-754`). Before this field, the only floor was `ai_audit`'s D5 pass, which scans
  the MODEL'S prose against phrase tables (`severity_calibration_floor`,
  `ai_audit.rs:1240-1319`) - a floor authored in rule prose was unreadable by the pipeline.
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
     unlocated downgrade (which the xlsx partition does not apply at all).
- **Malformed value (gotcha 1, floor direction).** The same `normalize_severity` fall-through
  maps a typo'd floor to `"low"`, and since `severity_rank` ranks `low`/`info` at the bottom
  together (`report_export.rs:2633-2640`), a typo'd floor does not error and does not no-op -
  it quietly evaporates (the raise never fires). `rules_with_invalid_severity_floor`
  (`lib.rs:1609-1639`) fails the build on the typo, and separately on
  `min_severity` ranking above the same rule's `max_severity` (an unsatisfiable pair whose
  winner would otherwise depend purely on application order). `min_severity == max_severity`
  is legal and pins the rule at one severity.
- **`min_severity = "low"` or `"info"` is inert by construction** (ranks at-or-below
  everything); not an error, but never what the author wants - a rule capping itself at low
  wants `max_severity` (`lib.rs:1603-1607`).

### 3.17 `deviation_ceiling`

- **Type / default.** Optional lowercase severity string; absent -> the pipeline default
  ceiling `"medium"` applies (`lib.rs:478-484, 771-781`;
  `DEVIATION_DEFAULT_CEILING`, `report_export.rs:596`). 36 rules on 2026-10-11
  (`rules_declaring_deviation_ceiling`, `lib.rs:1688-1699`).
- **What the engine does with it.** `clamp_deviation_severity`
  (`report_export.rs:608-628`) clamps DOWN the severity of rows whose kind is
  `FindingKind::Deviation` ONLY - a Defect under the same rule is untouched, and a row with
  no kind is NOT assumed to be a deviation. Applied at both exporters, last in the chain
  (3.15). The rationale: a consistency deviation's cost is forward drift, not current risk,
  so it caps at medium by default; a rule whose decision domain genuinely compounds authors
  `deviation_ceiling = "high"` as the structured escape hatch.
- **Interaction with `max_severity`.** `max_severity` clamps every row of the rule INCLUDING
  deviations, so a `deviation_ceiling` ranking above `max_severity` can never bind; the
  ceiling gate rejects that pair at build time (`lib.rs:1563-1573`).
- **Inert on a `defect`-kind rule (gotcha 6).** Deviation rows only exist for rules that went
  through the alternatives pass and had a genuine option decided
  (`assign_deviation_kinds`, `ai_audit.rs:6239-6295`); a `defect`-kind rule never enters that
  pass, so it emits no deviation rows and this field does nothing on it.
- **Malformed value.** Same `normalize_severity` fall-through (a typo becomes a `"low"`
  deviation ceiling - harsher than the default); gated at build time by
  `rules_with_invalid_severity_ceiling` (`lib.rs:1550-1561`).

### 3.18 `tag` and `layer` (INERT)

Every one of the 417 rule files carries top-level `tag = "..."` and `layer = "..."` keys
(sample values for `layer`: `"language"`, `"library"`, `"universal"`). **Nothing in this
workspace parses either field.** `RuleToml` (`lib.rs:393-485`) declares neither; because
`deny_unknown_fields` is not set they are silently ignored at load; and a workspace-wide
search finds no other code reading the principle TOMLs (the only other TOML parsers are
`checks/src/manifest.rs` for `.camerata/checks.toml`, `checks/src/architecture_config.rs`,
and `server/src/feature_flags.rs`). They are legacy fields from the deprecated camerata-ai
corpus format. They do not gate selection, do not affect emission, do not reach any artifact,
and have no defaults because they have no parse target. This is finding/gotcha 8: two fields
present on 100 percent of the corpus that functionally do not exist. Author them consistently
for human readers or delete them; the engine cannot tell the difference.

### 3.19 `[[emits]]` (INERT)

Two rule files (`universal/spirit-robustness-1.toml`, `rust/rust-domain-2-newtype-ids.toml`)
carry `[[emits]]` blocks with `target =`/`scope =` keys - the old camerata-ai emit-partition
format. Unparsed and inert for the same reason as 3.18. The live emit partitioning is done by
`arm.rs` from `enforcement` (section 3.3).

---

## 4. The `[decision]` block

Parsed into `DecisionToml { question, why, default }`, all optional (`lib.rs:495-505`).

### 4.1 `question`

- Carried to `Rule::decision_question` when non-empty (`lib.rs:1351-1355`).
- **Consumer:** the proposal/rule-detail UI only (`propose.rs:756`,
  `onboard.rs:452`) - "what the architect reads to understand WHAT they are choosing
  between". Not consumed by the scan or export pipelines; not in the PDF/xlsx.

### 4.2 `why`

- Carried to `Rule::decision_why` when non-empty (`lib.rs:1356-1360`).
- **Consumers:** the proposal UI (`propose.rs:757`), and source #2 for the derived `summary`
  when `qualifies` is absent (`lib.rs:1327-1337`) - through which it can become the
  emitted/diffed directive fallback text (section 3.14).

### 4.3 `default`

- The id of the adopted default option. **Honored ONLY when the rule-level `default = true`**
  (`lib.rs:1340-1347`); otherwise silently discarded. See section 3.7 for everything that
  hangs off the resulting `default_option`.
- **Ungated failure mode:** a `default` id that does not match any `[[option]].id` loads fine
  and simply never resolves - `resolved_option` finds nothing, so Fix/effort/floor-text/
  escalation all silently degrade, AND the remediation gate skips the rule (its filter_map
  drops a rule whose default id does not resolve, `lib.rs:2289-2301`). There is no corpus
  gate asserting `default_option` resolves to a real option (section 11).

---

## 5. `[[option]]` fields

Parsed into `OptionToml` (`lib.rs:507-553`) and carried as `RuleOption` (`lib.rs:560-631`).
A rule may have zero options (a pure content/mechanical rule); option-dependent machinery
then never engages.

### 5.1 `id`

- Required string. The stable identity everything resolves by: `[decision].default`
  (section 4.3), the project's persisted `RuleSelection.chosen_option`, the alternatives
  pass's option validation (a model answer naming an id not in the rule's own list is a
  flagged hallucination and falls back to the selected/default option,
  `ai_audit.rs:2456-2501`), `Finding::evaluated_option_id` (set blanket per rule after the
  recommendation is known, `ai_audit.rs:7067-7086`, and read back by `resolve_fix`'s
  resolution order), and the structured kind payloads (`convention_option_id`,
  `observed_options[].option_id`).

### 5.2 `label`

- Required string; the human name. **Client-facing in several places:** the alternatives
  prompt (`ai_audit.rs:2211-2214`), and VERBATIM (corpus text, never model prose) in the
  structured kind payloads that reach `findings.json`/xlsx - `DeviationKind::
  convention_label`, `ConventionObservedKind::convention_label`, `ObservedOptionUsage::label`
  (`api-types/src/audit.rs:154-207`; populated at `ai_audit.rs:6287-6292, 6380-6383,
  2282-2287`). Also the snippet fallback for a violates-option or convention-observed finding
  with no evidence text (`ai_audit.rs:6187-6190, 6358-6364`).

### 5.3 `directive`

- Default empty string (`lib.rs:511-512`). The DETECTION/enforcement recipe, written for an
  agent/model, not a client.
- **Consumers:**
  - The semantic audit prompt: the selected rule list is literally
    `- [RULE-ID] directive` lines (`ai_audit.rs:356`), where the directive is the
    chosen/default option's text (section 1 step 3). When the alternatives pass decides a
    NON-violates option, the rule's entry in `effective_selected` is REWRITTEN to that
    option's directive before the violation passes run (`apply_alternative_decision`,
    `ai_audit.rs:6171-6178`); when it decides a `violates` option, the directive is left
    untouched (section 5.9).
  - The alternatives prompt shows every option's directive so the model can match it against
    the code (`ai_audit.rs:2211-2214`).
  - The emitted CONVENTIONS/AGENTS text (`arm.rs:95-99`) and the rule-drift diff
    (`server/src/lib.rs:15083-15088`).
- **It must NEVER reach a client report.** `resolve_fix` binds ONLY to `remediation` and
  explicitly never falls back to `directive` ("fail-safe here means silently omitting the Fix
  block, not leaking the recipe", `report_export.rs:1991-2008`).

### 5.4 `why`

- Default empty string. The rationale for the option.
- **Consumers:** the alternatives prompt (`ai_audit.rs:2211-2214`), the proposal UI
  (`propose.rs:709`), and - importantly - the DETAIL text of a violates-option defect
  finding, verbatim: `apply_alternative_decision` interpolates `option.why` into the
  finding's `detail` (`ai_audit.rs:6198-6205`), which reaches the client artifacts. Write a
  violating option's `why` as client-readable fault prose. This is also where the
  `Rejected:` convention lives (section 10.4).

### 5.5 `remediation`

- Optional; blank filtered to `None` at load (`lib.rs:1370`). 486 occurrences on 2026-10-11.
- **Contract.** 2 to 4 imperative, client-facing sentences: what to change, WHERE, and how to
  VERIFY it is closed (`lib.rs:515-522`).
- **Consumer.** `report_export::resolve_fix` (`report_export.rs:2022-2040`): joins the
  finding's rule id to the corpus (via `corpus_rule_for`, so external-tool ids ground too),
  resolves the option in this order - (1) `finding.evaluated_option_id`, (2) the project's
  `chosen_option`, (3) the rule's `default_option` - and instantiates placeholder tokens from
  the finding's own `path`/`captures` (`instantiate_remediation`,
  `report_export.rs:2246-2297`). Surfaced as the PDF "Fix:" line, the workbook's
  "Recommended Fix" column, and `findings.json`'s `fix` field - the same join for all three.
- **Placeholders.** `<path>`/`<file>` fill from the finding's path; any other
  `<token>` fills from `captures[token]`, else a readable generic
  (`generic_placeholder_filler`, `report_export.rs:2159-2183`). Tokens are lowercase
  ASCII/digits/hyphens only; a `<...>` span that does not match that shape (a literal HTML
  example, a generic-type snippet) is emitted verbatim (`report_export.rs:2152-2157,
  2268-2276`). `<a:token>` prepends a correctly chosen "a"/"an" to the resolved value
  (`report_export.rs:2234-2291`).
- **Absent.** The Fix block is OMITTED entirely - never backfilled from `directive`
  (section 5.3). And since C5-8, an OPEN action-bucket row with NO fix from any source
  (neither `fix_specific` nor authored remediation), for a rule the corpus RECOGNIZES, is
  re-routed to the `held` bucket with a disclosed reason rather than shipping an action item
  with a blank Fix (`is_action_row_missing_a_fix`, `report_export.rs:2549-2573`; final check
  in `effective_bucket`, `report_export.rs:2500-2509`). So an unauthored remediation on a
  recognized rule does not just lose a Fix line; it can demote the row out of the action
  queue.
- **Gate.** Every rule WITH a default option must carry non-empty remediation on that option
  (`every_rule_with_a_default_option_has_authored_remediation`, `lib.rs:2286-2308`).
  Non-default options are ungated by design (several legitimately offer a "no policy
  adopted" alternative with nothing to fix).

### 5.6 `finding_headline` and `finding_detail`

- Optional pair; blanks filtered to `None` (`lib.rs:1371-1372`). 9 of each on 2026-10-11.
- **Purpose (P4).** A deterministic FLOOR finding's raw `detail` is the gate's own
  enforcement prose ("Deny writing a file whose path marks it as secret-bearing..."), set at
  scan time by `onboard::audit::title_for` - internal text a client should never read. This
  pair is the authored, client-facing replacement, instantiated with the same placeholder
  machinery as `remediation` (`report_export.rs:2065-2108`).
- **Consumer.** `resolve_floor_finding_text` (`report_export.rs:2091-2108`), via
  `client_headline_and_detail` (`report_export.rs:2117-2130`), which both the PDF's curated
  sites and the xlsx headline/detail columns call (`xlsx_export.rs:352-359`) - so the rewrite
  is identical across artifacts.
- **Authored as a PAIR or not at all.** If EITHER half is absent/blank the function returns
  `None` and the row falls back entirely to the pre-P4 derivation (`defect_headline` over
  `finding.detail`); a half-authored pair renders NOTHING of the authored text
  (`report_export.rs:2096-2104`). Convention for `finding_detail`: one plain-language
  sentence first (what this means for a non-technical founder), then 1 to 2 sentences of
  what/where/impact (`lib.rs:592-599`).
- **Never consulted for an AI-tier finding** in practice (invented rule ids never resolve in
  the corpus), but the code degrades to `None` rather than assuming that
  (`report_export.rs:2076-2080`).

### 5.7 `effort`

- Optional; blank filtered to `None` (`lib.rs:1374`). Vocabulary: `"low" | "medium" | "high"`,
  the same three tiers as `Finding::effort` (`lib.rs:537-547`). 141 occurrences on 2026-10-11.
- **Consumer.** `report_export::resolve_effort` (`report_export.rs:3242-3253`): the finding's
  OWN calibrated/default `effort` always wins; the authored band is consulted ONLY when that
  is absent - which is the permanent state for deterministic-floor/architectural/RLS-replay
  findings that never pass through AI calibration (`onboard/architectural.rs` sets
  `effort: None` unconditionally). Without an authored band, such a row renders "not yet
  estimated" forever (`effort_hours_bounds`, `report_export.rs:3263-3270`: low = 2 to 4 h,
  medium = 8 to 16 h, high = 24 to 40 h, None = "not yet estimated").
- **Bucket interaction.** `matrix_bucket` reads effort as EXACTLY `Some("low")`
  (`report_export.rs:2412`): an open high-severity finding is `do_now` only with low effort,
  else `do_next`. Note this reads the FINDING's effort, not the resolved one - and note the
  trap below.
- **Missing estimate never demotes.** Since C6-B4 (amended), a missing estimate does NOT move
  a row out of its action bucket - it renders the honest gap instead
  (`report_export.rs:2510-2522`); `is_action_row_missing_an_estimate`
  (`report_export.rs:2604-2628`) exists as a recognized-rule safety probe.
- **Gate.** Every CI-tier (mechanical/architectural) rule option that carries `remediation`
  must also carry an authored `effort`
  (`every_deterministic_rule_option_with_remediation_has_an_authored_effort_band`,
  `lib.rs:3376-3406`). The gate checks PRESENCE only.
- **Trap: the VALUE is unvalidated.** No gate checks the string is one of
  low/medium/high. A typo (`"Low"`, `"quick"`, `"med"`) parses fine, passes the presence
  gate, and then: `effort_hours_bounds` renders "not yet estimated" (fall-through arm), and
  `matrix_bucket`'s `matches!(effort, Some("low"))` reads it as not-low - silently demoting a
  high-severity row from `do_now` to `do_next`. See section 11.

### 5.8 `escalation`

- Optional inline table `escalation = { condition = "...", severity = "..." }`
  (`lib.rs:534-536, 653-661`). `severity` is `"soft-flag"` (log and continue) or
  `"hard-pause"` (stop the run for human review); ABSENT severity defaults to `HardPause`
  (`lib.rs:634-643`). 1 rule used it on 2026-10-11 (the test-tamper rule).
- **Option-scoped by design:** only the SELECTED (or default) option's spec is active, so a
  rule can offer an escalating option alongside non-escalating ones, and selecting the
  non-escalating option genuinely disables escalation (`Rule::selected_escalation`,
  `lib.rs:871-877`).
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

The single most consequential option flag, and the one the 244-rule audit found wrong 76
times. Absent -> `false` (`lib.rs:548-552`): a genuine, non-degenerate alternative. `true`
means CHOOSING THIS OPTION IS THE DEFECT - the option describes the absence or a degenerate
form of the rule's own requirement, authored as an `[[option]]` only because the corpus
convention pairs correct behavior with the behaviors deliberately not adopted
(`lib.rs:614-630`).

**Exactly what the engine does with it, step by step.** All runtime consumption is inside the
alternatives pass; the single chokepoint is `apply_alternative_decision`
(`ai_audit.rs:6163-6214`), through which BOTH deciding paths route (operator-forced picks at
`ai_audit.rs:6630-6641`, model recommendations at `ai_audit.rs:6689-6702`):

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
     net demotes that one shape to `info` (`ai_audit.rs:6155-6162`).
  3. These findings are SPLICED in after calibration and the P7 not-applicable filter, with
     their own ledger stage `violates_option_emitted` (`ai_audit.rs:7045-7066`) - they never
     re-enter the LLM calibration round-trip.
- **Kind assignment reads it too:** a rule whose DECIDED option is `violates` is skipped by
  `assign_deviation_kinds` (its rows are Defects, never consistency deviations,
  `ai_audit.rs:6262-6266`) and by `conventions_observed_findings` (following the defect is
  never "convention observed", `ai_audit.rs:6336-6338`).
- **It reaches the model's eyes:** `RuleOptionView.violates` is carried into the alternatives
  structures (`onboard/audit.rs:444-453`, `propose.rs:705-711`), though the prompt text does
  not currently render a per-option violates marker (`build_alternatives_block`,
  `ai_audit.rs:2193-2222`, prints id/label/directive/why only).
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
  nineteen security rules were pure decisions with zero flags, so "no RLS policy" reported
  as a convention the codebase chose).

---

## 6. The five finding kinds

Defined in `crates/api-types/src/audit.rs:50-81`; internally tagged on `"kind"`
(snake_case), flattened into every exported row so the discriminator plus payload fields are
siblings. `KindOnWire` guarantees every EXPORTED row carries a `kind` key; an absent kind
exports as the explicit `"unspecified"` tag, never silently as `"defect"`
(`audit.rs:43, 116-145`).

| Kind | Who produces it | Severity behavior | Where it renders |
|---|---|---|---|
| `defect` | Parse-time default for every model-reported fault (`ai_audit.rs:625-630`); every deterministic producer (floor checks `onboard/audit.rs:274, 332`, architectural checkers `onboard/architectural.rs:123`, previews `scan_tools.rs:934`, dep audit `dep_audit.rs:207`); the violates-option emission (`ai_audit.rs:6211`) | Unconstrained apart from floor/ceilings | Findings sheet / action buckets |
| `deviation` | `assign_deviation_kinds` reclassifies every surviving row under a rule whose DECIDED option is genuine (non-violates) (`ai_audit.rs:6239-6295`). Payload: structurally counted numerator, model denominator only when >= numerator, authored option id + label | Capped at `deviation_ceiling` else `"medium"` (section 3.17); floor applies (accusing kind) | Findings sheet; Deviations summary |
| `convention_observed` | One row per rule the scan decided a genuine option for with ZERO trace of violation anywhere in the final row set (`ai_audit.rs:6311-6388`) | Emitted at `info`; informational by kind; never floored | Conventions sheet; informational appendix. Ships in the product (A5.5: the derived decision set) |
| `no_convention` | Exactly ONE row per rule whose recommendation honestly said "no convention adopted" - available only to `decision`-kind rules, with the per-site violation path severed structurally in three layers (`ai_audit.rs:6390-6448, 6675-6687`) | `info`; informational by kind | Conventions sheet; informational appendix |
| `not_assessed` | One honesty row per ARMED rule whose routing group matched zero files (`ai_audit.rs:6450-6482`); closed reason vocabulary `pass_failed / no_files_matched / tool_failed / model_declined` (`audit.rs:211-235`) | `info`; informational by kind | Coverage sheet ("could not complete") |

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
   (`severity_calibration_floor`, `ai_audit.rs:1240-1319`): unauthenticated/full-compromise
   -> critical; authenticated cross-tenant read -> high (critical with an escalating data
   class); RLS-hedge guard -> high. NOT corpus-driven; this is the pass `min_severity` was
   built to replace for authored floors.
4. D6/D7/D8 ceiling rules (`apply_severity_ceiling_rules`, `ai_audit.rs:1586, 1689`):
   self-hedged confirmations capped low + held; CORS clamped to exactly medium
   (bidirectional); `ARCH-OBSERVABILITY-*` forced to exactly low BY RULE ID (gotcha 7).
5. The violates-option splice enters here with its anchored `"high"`
   (`ai_audit.rs:6196, 7045-7066`).

**At export, PDF/JSON partition (`report_export.rs:3640-3660`), per finding:**
1. `normalize_severity` (`report_export.rs:468-479`) - casing fixed; UNRECOGNIZED -> `"low"`.
2. **`min_severity` raise** (`raise_to_rule_min_severity`) - only accusing-kinded, located
   rows.
3. C4-R1 unlocated downgrade (`downgrade_unlocated_above_informational`,
   `report_export.rs:647-653`) - empty path AND line 0 -> `"info"`.
4. **`max_severity` clamp** (`clamp_to_rule_max_severity`) - last word over everything above.
5. **`deviation_ceiling` clamp** (`clamp_deviation_severity`) - Deviation-kinded rows only.

**At export, xlsx/`findings.json` partition (`xlsx_export.rs:325-342`):** identical EXCEPT
step 3 (the unlocated downgrade) is NOT applied - a documented asymmetry
(`report_export.rs:548-550`). See section 11.

Rank order for every comparison: `critical(0) < high(1) < medium(2) < low/info/anything(3)`
(`report_export.rs:2633-2640`); `info` survives normalization as its own tier below low
(`report_export.rs:473-476`) but ranks equal to low in comparisons.

---

## 8. Bucket routing: what reaches the client, and where

`effective_bucket` (`report_export.rs:2478-2523`) is THE single bucket computation, shared by
the PDF matrix, the curated findings, and the xlsx partition. Order of gates:

1. `is_held_for_review` (`report_export.rs:2765-2770`): calibration hedge
   (`needs_review` / `confidence == "needs-review"`) on an OPEN row -> `held`, at ANY
   severity.
2. `is_uncited_ai_finding` (`report_export.rs:1977-1982`): AI-tier with only an advisory
   citation -> `held` at high/critical, `informational` below.
3. `is_informational` (`report_export.rs:2787-2841`), in ITS internal order:
   `ARCH-OBSERVABILITY-*` rule identity FIRST (before the hard invariant; gotcha 7); then the
   hard "critical/high is never informational" invariant; then `info` severity; then
   testing-style with fewer than 3 test files; then absence-type `structured` stance-layer
   rules at medium or below.
4. `matrix_bucket` (`report_export.rs:2397-2420`): dispositions map directly
   (accepted / do_now / plan); open rows: critical -> `do_now`; high + low effort ->
   `do_now`; high otherwise -> `do_next`; everything else -> `plan`.
5. `is_action_row_missing_a_fix` (`report_export.rs:2549-2573`): a recognized-rule action
   row with no fix of any kind -> `held` (section 5.5). A missing ESTIMATE never demotes
   (`report_export.rs:2510-2522`).

Groups then partition into `curated_findings` vs `held_for_review_findings`
(`report_export.rs:4061-4070`; struct fields at `report_export.rs:1280, 1294`). **Rows routed
by rule identity (gotcha 7) or any gate above land under `held_for_review_findings` /
the informational appendix, NOT `curated_findings` - a test (or reader) consuming only
`curated_findings` is structurally blind to them.**

What reaches which artifact, per corpus field: see the per-field sections; summary table:

| Corpus field | PDF | xlsx / findings.json | Prompt-only | Internal-only |
|---|---|---|---|---|
| `title`, `id` | yes | yes | yes | |
| `sources` (url/title/linter) | yes (citations) | yes | | linter also drives tooling |
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
| `tag`, `layer`, `[[emits]]` | | | | NOTHING reads them |

---

## 9. Named gotchas

Every one of these has bitten. Numbers 1 to 7 were named in the audit brief; 8 onward were
found while writing this reference.

1. **`normalize_severity` maps every unrecognized string to `"low"`**
   (`report_export.rs:468-479`). A typo'd `max_severity` becomes the HARSHEST ceiling in the
   vocabulary (caps everything at low); a typo'd `min_severity` silently evaporates (low
   ranks at the bottom, so the raise never fires); a typo'd `deviation_ceiling` becomes a
   low cap. Build gates `rules_with_invalid_severity_ceiling` / `_floor`
   (`lib.rs:1534, 1609`) reject the typo before it ships.

2. **`default = false` with `kind = "default_with_exceptions"` is a contradiction.**
   `default` means "ships an adopted default option" (section 3.7); the kind means "one
   option is correct unless justified". Eleven rules carried both at audit time, and the
   contradiction correlated strongly with wrong `violates` flags. Functionally, the missing
   default also disables Fix/effort/floor-text/escalation resolution until a project chooses
   an option. No build gate exists for the pair (section 11).

3. **A `decision`-kind rule with zero `violates` flags cannot report a violation at all.**
   The only reportable condition is cross-codebase inconsistency (Deviation rows) or the
   no-convention row; whatever option the repo follows is recorded as the convention. That is
   correct for genuine stance decisions and catastrophic for security requirements -
   nineteen security rules were in that state, so "no RLS policy" reported as a convention
   the codebase chose. The fix for a genuine requirement is `kind = "defect"` (plus a
   `violates` flag on the degenerate option for documentation), which is the in-flight
   migration.

4. **`verification` can disarm a rule everywhere, forever.** Omitting it defaults to `draft`
   -> not auto-recommended -> never selected by an unattended scan (`lib.rs:978-983`,
   `inspect_cmd.rs:155-165`). `UI-UTC-DATES-1` shipped that way (finding A7).
   `SPIRIT-FILE-SIZE-1` was DOUBLY disarmed: `verification = "policy"` (pre-W4, not
   auto-recommended) plus the `SPIRIT-` prefix exclusion from the code-audit tier
   (`onboard/audit.rs:357-359`) - both halves since fixed (`policy` now arms,
   `lib.rs:966-983`; the prefix exclusion was removed from the semantic prompt,
   `onboard.rs:914-923`). The gate `rules_disarmed_by_verification` (`lib.rs:1461`) now
   fails the build on any unarmable verification value.

5. **A rule whose `domain` no stack emits can never run, for any repo, ever.**
   `domains_for_stack` (`propose.rs:527`) is the only bridge from a detected repo to corpus
   domains; 94 of 417 rules (23 percent) once sat in unmapped folders. Two build gates now
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
   informational" invariant (`report_export.rs:2799-2816`). Findings of these rules render
   under `held_for_review_findings`/informational, never `curated_findings` - tests reading
   only the latter were structurally blind to them.

8. **`tag` and `layer` (and `[[emits]]`) are parsed by nothing** (sections 3.18/3.19).
   Present on all 417 rules; zero consumers in the workspace. Editing them changes nothing;
   do not mistake them for selection or emission controls.

9. **A misspelled FIELD NAME vanishes silently.** `deny_unknown_fields` is deliberately off
   (`lib.rs:391-392`), so `max_severety = "high"` or `violates = true` typed as
   `voilates = true` parses cleanly and simply is not there. The severity gates catch wrong
   VALUES in correctly spelled fields; nothing catches a wrong field name. When a flag seems
   to do nothing, check the spelling against `RuleToml` (`lib.rs:393-485`) before anything
   else.

10. **A wrong VALUE in a strict-enum field drops the entire rule in production.**
    `enforcement`, `kind`, and `escalation.severity` parse as strict enums; an unknown value
    fails the FILE's parse. Production loaders are `load_corpus_lenient` and mostly discard
    the error list (`server/src/lib.rs:4257`, `propose.rs:661`), so the rule silently
    disappears from every proposal and scan. The backstop is the test suite's
    `errors.is_empty()` assertions (`onboard.rs:2159-2160` and siblings) - run the tests
    after editing TOMLs.

11. **A dangling `[decision].default` id is ungated.** If the default id names no real
    option, the rule loads, resolves no option anywhere (no Fix, no effort band, no floor
    text, no escalation), and the remediation gate SKIPS it rather than failing
    (`lib.rs:2289-2301`). Verify the id matches an `[[option]].id` exactly.

12. **`effort` values are unvalidated.** The presence gate (`lib.rs:3376`) does not check the
    vocabulary; anything other than exactly `"low"`/`"medium"`/`"high"` renders "not yet
    estimated" and reads as not-low in `matrix_bucket` (`report_export.rs:2412`), silently
    demoting a high finding from `do_now` to `do_next`.

13. **`min_severity` never fires on unkinded or unlocated rows** (by design,
    `report_export.rs:561-574`): a legacy persisted row (`kind: None`) or a row with empty
    path and line 0 is never raised. Do not expect a floor to rescue a producer that fails
    to set kinds or locations.

14. **`finding_headline`/`finding_detail` are all-or-nothing** (section 5.6): author both or
    the authored half is ignored entirely.

15. **The in-file `domain` field does not set the domain** (section 3.4): the folder does.
    Moving a file IS a domain change; editing the field is not.

---

## 10. Rule-authoring guide

### 10.1 Worked example: a `defect`-kind rule

Use when there is NO acceptable alternative anywhere - every option other than the correct
one is the absence or a degenerate form of the requirement. Security requirements belong
here.

```toml
id = "SEC-NO-EXAMPLE-SINK-1"
title = "User input never reaches the example sink unescaped"
# tag/layer: kept for human readers; the engine does not parse them (section 3.18).
tag = "universal"
domain = "universal"          # must match the folder; the folder wins (section 3.4)
layer = "universal"
enforcement = "structured"    # structured = human-reviewed; mechanical would promise a detector
kind = "defect"               # REQUIRED on every rule (build gate rules_missing_kind)
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
inconsistency. NO option may carry `violates = true` (stated at `lib.rs:147-150`; currently
unenforced by a gate, but a flag here will fire at runtime if that option is ever decided -
section 3.6 - so treat the invariant as hard).

```toml
id = "ARCH-EXAMPLE-ID-STYLE-1"
title = "Entity identifiers follow one representation project-wide"
tag = "api-layer"
domain = "api-layer"
layer = "universal"
enforcement = "structured"
kind = "decision"
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
# no `default =` here, and rule-level default = false - the pair must agree (gotcha 2 is the
# OPPOSITE mismatch: default_with_exceptions kind + default = false).

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
`lib.rs:1471-1494`).

```toml
id = "ARCH-EXAMPLE-IDEMPOTENCY-1"
title = "Mutating endpoints deduplicate retries server-side"
tag = "api-layer"
domain = "api-layer"
layer = "universal"
enforcement = "structured"
kind = "default_with_exceptions"
default = true                # REQUIRED for this kind to make sense (gotcha 2)
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
  into the defect finding's detail, `ai_audit.rs:6198-6205`) - write the sentence after
  `Rejected:` for a client to read.

This is also the best available MECHANICAL gate signal: a corpus check asserting
`why.starts_with("Rejected:") == violates` for every option of every alternatives-eligible
rule would have caught a large share of the 76 wrong flags. No such gate exists yet
(section 11).

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
| `rules_missing_kind` (`lib.rs:1431`) + `req_a5_1_...` | any rule has no `kind` | an unclassified rule must not silently behave permissively (section 3.6) |
| `rules_disarmed_by_verification` (`lib.rs:1461`) + `req_h1_...` | any rule's `verification` makes it unarmable everywhere | the A7/A8 class: a repo-independent value disarming a rule forever (gotcha 4) |
| `rules_with_invalid_severity_ceiling` (`lib.rs:1534`) | `max_severity`/`deviation_ceiling` outside the vocabulary, or `deviation_ceiling` above `max_severity` | the `normalize_severity` low fall-through makes a typo the harshest cap (gotcha 1) |
| `rules_with_invalid_severity_floor` (`lib.rs:1609`) | `min_severity` outside the vocabulary, or above `max_severity` | a typo'd floor silently evaporates; floor above ceiling is unsatisfiable |
| `req_e2_the_corpus_actually_declares_severity_ceilings` / `req_w8_..._floors` (probes at `lib.rs:1653, 1669`) | the corpus declares ZERO ceilings/floors | anti-inertness: E2 shipped fully tested and fully inert once already |
| `every_rule_with_a_default_option_has_authored_remediation` (`lib.rs:2286`) | a rule's DEFAULT option lacks remediation | a curated finding must always be able to ship a Fix line |
| `every_deterministic_rule_option_with_remediation_has_an_authored_effort_band` (`lib.rs:3376`) | a CI-tier option has remediation but no `effort` | proven deterministic defects must not read "not estimated" forever |
| `suspicious_default_with_exceptions_rules` (`lib.rs:1482`) | ADVISORY, not build-failing by itself: a DWE rule with options and zero `violates` flags | misclassification or under-authoring signal (gotcha 2's sibling) |
| `mechanical_rules_missing_detector` + `every_mechanical_rule_with_no_detector_is_code_auditable` (`mechanical_gate.rs:144, tests`) | a `mechanical` rule has no detector channel AND is not code-auditable | the only genuine phantom: checked by nothing at all |
| `req_h1_every_corpus_domain_is_emittable_by_some_stack` + witness-repo test (`propose.rs:843, 1049`) | a corpus folder/domain no stack can emit | gotcha 5 |
| `errors.is_empty()` assertions on lenient loads (e.g. `onboard.rs:2159`) | any TOML fails to parse | production ignores parse errors; tests are the only net (gotcha 10) |

Checklist when authoring or editing a rule:

1. Pick the `kind` honestly (section 3.6); set `default` consistently with it (gotcha 2).
2. Set `verification` to an arming value (`grounded`/`verified`/`policy`) with real external
   `[[sources]]` when claiming grounded.
3. Make `[decision].default` match an `[[option]].id` character-for-character (gotcha 11).
4. Author `remediation` (default option at minimum) and `effort` (CI-tier) with exact
   vocabulary values.
5. Flag degenerate options `violates = true` and open their `why` with `Rejected:`; never
   flag an option on a `decision`-kind rule.
6. If the prose states a severity, encode it: "at least X" -> `min_severity`; "never more
   than X" -> `max_severity`; "inconsistency here is expensive" -> `deviation_ceiling`.
7. If this is a new stack/folder, wire detection + `domains_for_stack` + `domain_to_glob`
   and add a proposed-rules test (gotcha 5).
8. Run the camerata-rules test suite (and the server corpus tests) before considering the
   edit done - the gates above are the only thing standing between a typo and a silent
   no-op.

---

## 11. Open questions, inert fields, and inconsistencies

Found while writing this reference. Documented here per the no-code-changes constraint;
none are fixed by this document.

1. **Fields with NO consumer anywhere:** rule-level `tag`, `layer`, and the `[[emits]]`
   blocks (sections 3.18/3.19). Also the METHOD `EnforcementKind::emits_to_conventions`
   (`lib.rs:117-119`) has zero callers; the live CONVENTIONS/AGENTS split is `arm.rs`'s own
   string-level `is_ci_tier`. Either wire them or remove them; today they are pure
   documentation wearing a field's clothes.

2. **The "no `violates` on a `decision`-kind rule" invariant is stated but unenforced.**
   There is no `decision_rules_with_violates` gate, and at runtime the flag IS live on a
   decision rule (`apply_alternative_decision` branches only on `option.violates`,
   `ai_audit.rs:6163-6214`). Given the in-flight reclassifications, a gate here would be
   cheap and directly guards the audit's biggest error class.

3. **No gate for the `default = false` + `kind = "default_with_exceptions"` contradiction**
   (gotcha 2), and **no gate that `[decision].default` resolves to a real option id**
   (gotcha 11 - note the remediation gate's filter_map silently SKIPS a rule with a dangling
   default, `lib.rs:2289-2301`, so it hides rather than catches that state).

4. **No vocabulary gate for option `effort`** (gotcha 12): presence is gated, value is not,
   and a bad value silently affects bucket routing via `matrix_bucket`'s exact
   `Some("low")` match (`report_export.rs:2412`).

5. **No mechanical `Rejected:` agreement gate** (section 10.4), despite it being the single
   best-performing authoring convention in the audit.

6. **Cross-artifact severity asymmetry for unlocated rows.** The PDF/JSON partition applies
   the C4-R1 unlocated downgrade; the xlsx/`findings.json` partition does not
   (`report_export.rs:3650` vs `xlsx_export.rs:325-342`; acknowledged at
   `report_export.rs:548-550`). An unlocated above-informational row (e.g. an
   operator-forced `violates` finding, which carries no evidence and anchors at high,
   `ai_audit.rs:6155-6162`) can therefore render `info` in the PDF and `high` in the
   workbook. The floor was made order-independent of this; the base severity was not.

7. **The RUST-DOMAIN-6 taxonomy gap.** The three-way `RuleKind` cannot express a rule with
   SEVERAL equally legitimate options PLUS degenerate options that are genuine faults. That
   shape is not `decision` (which forbids flags) and not `default_with_exceptions` (there is
   no single privileged default). Reading the engine, the mechanics would actually support a
   fourth kind trivially - `apply_alternative_decision` already branches purely on the
   DECIDED option's `violates`, and the no-convention escape is already gated independently
   on kind - so the gap is purely taxonomic: a hypothetical `decision_with_floor` (equal
   alternatives, plus flagged degenerate forms, no-convention escape allowed among the
   genuine options) would need no new pipeline behavior, only a kind value, prompt marker
   policy, and gate updates. Until the owner rules on it, rules of this shape have no honest
   `kind` value: classifying them `decision` forces removing real fault flags, and
   `default_with_exceptions` fabricates a default that does not exist. Documented as an open
   gap, deliberately not papered over.

8. **`stack_exception.framework` is unvalidated** against the detectable-framework
   vocabulary (`detect_frameworks`, `propose.rs:54`): an exception naming a framework the
   detector never emits silently never fires - the same silent-no-op shape as gotcha 9, one
   level down.

9. **Production loads discard parse errors** (gotcha 10). A cheap hardening would be logging
   (or ledgering) the lenient loader's error list at the server call sites that currently
   bind it to `_errs`.
