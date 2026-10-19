# Adding a rule to Camerata's shipped corpus: a developer guide

> Audience: the Camerata developer (you), adding or changing a rule in `crates/rules/principles/`.
> This is NOT the end-user "how do I wire a mechanical rule in my repo" doc. This answers: when I add
> a rule to the SHIPPED corpus, what do I have to consider BEYOND writing the TOML, because for some
> rule kinds the TOML alone does nothing.

> **Field reference lives elsewhere.** `docs/RULES_REFERENCE.md` is the authoritative,
> field-by-field reference for the rule schema: every field, every flag, every accepted value.
> This document is the *workflow* guide and deliberately does not restate the schema, so there
> is exactly one place to change when a field is added. The snippets below are orientation,
> not a specification; where they disagree with `docs/RULES_REFERENCE.md`, that file is right.
>
> **Scope change, 2026-10-19.** This guide now ALSO owns the per-field AUTHORING STANDARD:
> who reads each value, what voice it must be written in, and what makes a value wrong. The
> reference stays descriptive (what the engine does); this document is prescriptive (what you
> write). The split was forced by a shipped defect: the reference correctly recorded that
> option `label` is client-facing, this guide specified it as `label = "Human label"`, and
> ~28 labels went to paying clients in the DETECTOR's voice (see "Who reads what you write"
> below). A fact recorded in the reference is not a standard until it is stated here as one.

## The one idea to internalize

**The TOML's `enforcement` field is a CLASSIFICATION, not the enforcement.** Writing
`enforcement = "architectural"` does not make Camerata enforce anything. The corpus is data; a rule
only *does* something through one of these channels:

1. **Grounding** (free, automatic): every SELECTED rule's text flows into the agent's prompt. This is
   how prose rules have effect. You get it for free by adding the TOML.
2. **Emission** (free, automatic): structured rules emit into `CONVENTIONS.md` / `AGENTS.md`.
   **These files go into the CLIENT's repository and are the product** — a codebase whose
   conventions are written down so the client's own future development, much of it
   agent-driven, stays consistent with what they already have. So the text you author here is
   read by their engineers and their agents, not only by ours. Write it accordingly; see
   "Reaching the client is the POINT, not the risk".
3. **A mechanical gate/check** (NOT free): a rule is only mechanically blocked if a detector for it is
   wired into an enforcement layer (the layer-1 gateway, or a layer-2 check). Adding an "architectural"
   rule to the corpus does NOT create that detector.
4. **Escalation** (partly free): a rule that calls for escalation works agent-driven with zero code
   once you add the field; a deterministic backstop is optional and needs code.

So the real question for any new rule is: **which channel(s) do I need, and which require code?**

---

## What every rule MUST have

```toml
id = "DOMAIN-SHORT-NAME-N"     # stable, traceable, UPPER-KEBAB + trailing -N
title = "One-line human title"
enforcement = "prose" | "structured" | "mechanical" | "architectural"
domain = "agentic"             # also derived from the folder; keep them agreed
kind = "…"                     # REQUIRED since 2026-10-09; FOUR values; see docs/RULES_REFERENCE.md
inspectable = true | false     # REQUIRED since 2026-10-12 (W16); see below
```

- **`id` is forever.** It is referenced by selections, grounding, role subsets, code, and the user's
  saved project config. Renaming an id silently breaks existing projects. Don't.
- **`enforcement`** classifies the rule (see channels above). Pick honestly; it drives emission +
  preview, not magic enforcement.
- **`kind` is mandatory and the build enforces it.** A rule with no `kind` fails the corpus-load
  gate `rules_missing_kind` in `crates/rules/src/lib.rs`, which is asserted empty against the real
  corpus by `req_a5_1_every_corpus_rule_declares_a_kind`. There is deliberately **no permissive
  default** on an absent field: the field decides whether this rule can ever accuse a codebase of a
  fault, so defaulting it would let a classification nobody made produce a finding. For which value
  to pick, and what each one makes the engine do, read `docs/RULES_REFERENCE.md` - that is the
  single source of truth for the taxonomy and should not be restated here.
- **`inspectable` is mandatory too, and the build enforces it.** It answers one question: can a
  scan of a repository say anything true about this rule at all? `false` means EMITTED, NEVER
  SCANNED - the rule still ships into the client's `AGENTS.md`/`CONVENTIONS.md` as a directive
  governing future work, but it is removed from the scan before arming, so it never claims a
  verified-clean result on a codebase it cannot be checked against. Like `kind`, there is
  deliberately no default: `rules_missing_inspectable` in `crates/rules/src/lib.rs` fails the build
  on an absent value, because guessing `true` would ship an unassessable rule as "verified clean"
  in a paid deliverable and guessing `false` would silently stop a real check from running. 26 of
  417 rules are currently `false`. If you are unsure, `true` is the reversible choice. Semantics
  and the full consumer list: `docs/RULES_REFERENCE.md` section 3.6a.
- **Do not add a `tag` field.** It was deleted from all 417 rules and from the schema on
  2026-10-12, and the unknown-field gate now fails the build if it reappears. `layer` is still
  authored on every rule and is still read by nothing; keep it accurate for human readers and do
  not expect it to do anything.
- **Write `qualifies` at RULE level, never inside a `[[sources]]` block.** A `qualifies` key inside
  `[[sources]]` parses, passes the unknown-field gate, and is discarded, after which the rule's
  summary silently falls back to `[decision].why`. Ten `go/` rules shipped that way before two
  gates were added for it.
- **More corpus-load gates will fail your build.** If a severity bound is malformed:
  `rules_with_invalid_severity_ceiling` and `rules_with_invalid_severity_floor`. Both exist because
  `report_export::normalize_severity` maps every unrecognised severity string to `low`, so a typo in
  a bound does not error and does not no-op; it silently becomes the harshest possible ceiling, or
  silently evaporates as a floor. Since 2026-10-12 three more will too:
  `corpus_files_with_unknown_fields` (any misspelled or undeclared key, at seven nesting levels),
  `options_with_rejected_prose_but_no_violates_flag` (an option whose `why` opens with `Rejected:`
  must carry `violates = true`), and `decision_kind_rules_with_violates_flag` (a `decision`-kind
  rule may carry no `violates` option at all). Again: the fields and their accepted values are
  documented in `docs/RULES_REFERENCE.md`.

## What a rule CAN have

```toml
default = true                 # ships an adopted default option (else the architect MUST choose)
qualifies = "…"                # the summary / when-it-applies prose (falls back to decision.why, then title)
verification = "draft" | "policy" | "grounded" | "verified" | "needs_recheck"  # absent -> draft
opt_in_only = true             # never auto-recommended / pre-checked at onboarding
layer3_only = true             # CI-tier only; never run at layer-2 or scan time

[decision]
question = "What position does the project take on …?"
default  = "the-default-option-id"
why      = "rationale for the adopted default"

[[option]]
id        = "stable-option-id"   # ALSO forever — referenced by selections + code; client-visible data
label     = "A state the client's code is in"  # CLIENT-FACING VERBATIM — see "The client-facing
                                 # writing standard" below. "Human label" was this guide's entire
                                 # spec for the field until 2026-10-19, and it shipped ~28
                                 # detector-voice labels to clients. Never again.
directive = "the concrete behavior this option codifies"  # model prompt + emitted CONVENTIONS.md
why       = "rationale"          # client-facing when violates = true (defect detail)
# Optional, the escalation hook (see below):
escalation = { condition = "…", severity = "hard-pause" | "soft-flag" }

[[sources]]
url   = "…"
title = "…"
linter = "…"                   # optional, the tool that enforces it externally

[verified]                     # present only when a human confirmed it
# who/when/versions-verified-against, for the staleness pass
```

Notes:
- **Option ids are forever too** (same reason as rule ids).
- **`verification`** answers a PROVENANCE question and nothing else. `draft` is unreviewed AI output.
  `policy` is the honest label for a rule that encodes deliberate project or team policy with NO
  external authority: any cited source is internal (a Camerata doc citing Camerata, say), not a
  published standard or a real linter rule. Use `policy` rather than `grounded` whenever you cannot
  point at a real external authority; inventing a citation to reach `grounded` is worse than the
  honest label. `verified` carries human provenance and no agent may set it. `needs_recheck` was
  `verified` until its cited source drifted.

  **Correction, 2026-10-09 (finding A8).** An earlier version of this guide said `policy` "is kept
  out of the armed ruleset, like `draft`". That was true and it was a defect. `policy` says nothing
  about whether a rule is correct or checkable, so using it as an arming switch conflated provenance
  with armability and silently disarmed 17 shipped rules, among them every `PROC-*` governance rule
  the product is explicitly about. `Verification::Policy` is now auto-recommended
  (`is_auto_recommended()` matches `Grounded | Verified | Policy`), while `is_grounded()` and
  `is_shippable()` still return false for it, because the provenance answer has not changed. `draft`
  and `needs_recheck` still disarm, deliberately: unreviewed AI output and known-stale human
  confirmation both want an explicit opt-in.

  **A rule that cannot arm for ANY stack is now a build failure.** The gate is
  `rules_disarmed_by_verification`, asserted empty against the real corpus by
  `req_h1_no_corpus_rule_is_disarmed_by_its_verification_value`. Omitting `verification` entirely is
  the trap that caused this: the field defaults to `draft`, so four rules with shipped deterministic
  detectors never armed anywhere. Set it explicitly. See `docs/TECHNICAL.md` section 5a, "The
  verification ladder", for the full table of which statuses arm and which ground.

---

## Who reads what you write: audience before wording

**A field's audience is a property of the engine, not a stylistic choice.** Every value you
author lands somewhere specific - a paid client artifact, a model prompt, the client's own
repo files, the operator's UI, or nowhere - and the engine composes client-facing sentences
AROUND your text verbatim. Authoring in the wrong voice is therefore not a tone problem; it is
a factual error in a deliverable.

**The defect that proved it (2026-10-19).** The entire `supabase/` rulepack authored option
labels in the tool's voice. `conventions_observed_findings` (`ai_audit.rs`) then rendered, in
client artifacts, sentences like: *"The codebase consistently follows "Flag getSession() calls
in server-execution-context files; ambiguous paths get needs_review""* - an instruction to a
scanner, including Camerata's internal `needs_review` vocabulary, presented to a client as a
description of their own engineering practice. Roughly 28 of 35 such labels sat in
`crates/rules/principles/supabase/`; `sql/` carries 3 more files in the same voice.

### Reaching the client is the POINT, not the risk

Read the paragraph above and it is easy to draw exactly the wrong conclusion: that
client-facing text is a hazard and the safe move is to keep corpus prose away from the client.
That reading is wrong, and this section exists to forbid it before it takes hold.

**Emitting derived conventions into the client's repository IS the product.** The client is
buying a codebase whose conventions are written down and enforceable, so that their own future
development — much of it agent-driven — stays consistent with what they already have. A
directive the client never receives would be a defect, not a safeguard. There is nothing to
contain here.

**The defect is only ever VOICE.** Never reach, never audience, never volume. A field is wrong
when its text is addressed to the wrong reader, and the repair is always to re-voice it, never
to withhold it.

This distinction is easy to lose and was in fact lost once during this work: the
`directive`-reaches-`CONVENTIONS.md` behaviour was initially written up as a "leak", with a
proposal to stop emitting it. That proposal would have removed the deliverable to protect it.
The correction, and its full reasoning, is recorded at `directive`'s own entry below.

### The corollary that does the real work: one voice, not one voice per audience

A field with several audiences does NOT get several voices. It gets the one voice that serves
all of them, and if no such voice exists the field is being asked to do two jobs and the rule
is mis-shaped.

`directive` is the worked case. It is simultaneously the scanning model's rule list
(`ai_audit.rs:2247` renders `- [RULE-ID] directive`, introduced at `ai_audit.rs:389` as "the
rules the project has ADOPTED"), the client's agents' governance text, and the client's
engineers' reading. A scanner recipe fails ALL THREE — including the model, which is told to
behave like a regex engine instead of being told what the code must do. So "tool voice is fine
in the prompt" is false, and the W15 investigation proved it independently: a directive
rewritten to such text makes the conformance pass ask the wrong question and correctly find
nothing.

The practical test, applicable to every multi-audience field: **write the value, then read it
as each audience in turn.** If it only works for one of them, it is wrong for the field — not
wrong for the other readers.

Five audiences, in decreasing order of how expensive a wrong word is:

1. **CLIENT-ARTIFACT** - verbatim text in a paid deliverable (PDF, xlsx, `findings.json`).
2. **CLIENT-REPO** - verbatim text emitted into the client repository's `AGENTS.md` /
   `CONVENTIONS.md` (`arm.rs::render_rule`: `### {id} - {title}` then the directive). Read by
   the client's own developers and agents as "conventions this project follows".
3. **MODEL** - prompt text (scan directive list, alternatives block, escalation grounding).
4. **OPERATOR** - the onboarding/cockpit UI (proposal rows, rule-detail modal).
5. **ENGINE / NOBODY** - consumed as data (ids, flags, bounds) or parsed and read by nothing.

### The audience table

Audience column derived from `docs/RULES_REFERENCE.md`'s per-field consumption and its
"What reaches which artifact" table (section 8); cited section in the last column. "Absent"
states the engine's actual behavior on an omitted/blank value (blank strings are scrubbed to
absent at load for `remediation`/`finding_headline`/`finding_detail`/`effort` and the three
severity bounds - REF section 2).

| Field | Audience | Absent / blank | REF |
|---|---|---|---|
| `id` | CLIENT-ARTIFACT (cited in every artifact) + ENGINE (primary key; prefixes change behavior) | hard load error; file skipped | 3.1 |
| `title` | CLIENT-ARTIFACT + CLIENT-REPO (emit header; directive of last resort) | parse failure (required) | 3.2 |
| `enforcement` | ENGINE + OPERATOR (placement string) | required; unknown value drops the whole FILE on the lenient path | 3.3 |
| `domain` (in-file) | NOBODY (folder-derived domain wins; mismatch only warns) | folder decides regardless | 3.4 |
| `extra_domains` | ENGINE (proposal match only) | empty; folder domain alone matches | 3.5 |
| `kind` | ENGINE (never reaches any client artifact) | build gate `rules_missing_kind` | 3.6 |
| `inspectable` | ENGINE (scan-set narrowing only) | build gate; runtime fallback `true` | 3.6a |
| `default` | ENGINE (gate on `[decision].default`) | `false`: architect must choose | 3.7 |
| `qualifies` (-> `summary`) | OPERATOR + CLIENT-REPO/MODEL as FALLBACK directive when no option resolves | summary falls back to `[decision].why`, then `title` | 3.14 |
| `verification` | ENGINE (arming) + OPERATOR (badge string; never PDF/xlsx) | defaults `draft` = disarmed everywhere = build gate | 3.8 |
| `[[sources]]` `url`/`title` | CLIENT-ARTIFACT (citations VERBATIM) ; `linter` ENGINE (tool routing) | citation degrades; an uncited AI-tier row routes held/informational | 3.9 |
| `[[sources]].relevance` | NOBODY (author/reviewer provenance; must never be concatenated into a deliverable) | - | 3.9 |
| `[verified]` | NOBODY (human provenance for the staleness pass) | none | 3.10 |
| `opt_in_only` | ENGINE (strips recommended + auto-recommended) | `false` | 3.11 |
| `layer3_only` | ENGINE (preview-pass exclusion) | `false` | 3.12 |
| `[[stack_exception]]` | ENGINE (finding suppression; `note` is internal-only) | none; unknown `framework` silently never fires | 3.13 |
| `max_severity` | ENGINE (export clamp) | no ceiling | 3.15 |
| `min_severity` | ENGINE (export raise; accusing + located rows only) | no floor | 3.16 |
| `deviation_ceiling` | ENGINE (Deviation rows only) | pipeline default `"medium"` | 3.17 |
| `[decision].question` | OPERATOR only | nothing shown | 4.1 |
| `[decision].why` | OPERATOR + summary fallback #2 (can become the emitted directive) | - | 4.2 |
| `[decision].default` | ENGINE | dangling id: loads fine, resolves nothing, remediation gate SKIPS the rule (gotcha 11) | 4.3 |
| `layer`, `[[emits]]` | NOBODY (declared, parsed, discarded). Never add `tag` (deleted; build failure) | - | 3.18/3.19 |
| option `id` | ENGINE (identity) + CLIENT-ARTIFACT as data (the flattened kind payloads: `convention_option_id`, `observed_options[].option_id` in `findings.json`/xlsx; `evaluated_option_id` itself stays engine-side) | required | 5.1, 6 |
| option `label` | CLIENT-ARTIFACT VERBATIM (kind payloads, "consistently follows" sentence, snippet fallback) + MODEL | required | 5.2 |
| option `directive` | MODEL (scan + alternatives prompts) + CLIENT-REPO (emitted convention text). NEVER the report artifacts | empty string; summary/title fall in | 5.3 |
| option `why` | MODEL + OPERATOR + CLIENT-ARTIFACT when a `violates` option is decided (interpolated into the defect detail) | empty string | 5.4 |
| option `remediation` | CLIENT-ARTIFACT (the Fix line, all three artifacts) | no Fix; since C5-8 an open action row with no fix on a recognized rule is DEMOTED to `held` | 5.5 |
| `finding_headline`/`finding_detail` | CLIENT-ARTIFACT (floor-finding rewrite) | pair-or-nothing: a half-authored pair renders NONE of it | 5.6 |
| option `effort` | CLIENT-ARTIFACT (hours label) + ENGINE (`do_now` routing reads exactly `"low"`) | "not yet estimated"; never demotes the row | 5.7 |
| option `escalation` | MODEL (agent grounding, `## ESCALATION CONDITIONS`) + OPERATOR (review) | no escalation; absent `severity` = `hard-pause` | 5.8 |
| option `violates` | ENGINE (alternatives chokepoint; guaranteed high-severity Defect when decided) | `false` | 5.9 |

---

## The client-facing writing standard

The engine does not quote your text in isolation; it builds sentences around it. Two frames
are load-bearing and are quoted here from the source so the test is checkable, not vibes
(line numbers drift; anchor on the function names):

- `conventions_observed_findings` (`ai_audit.rs`): *"The codebase consistently follows
  "{label}" for {rule_id} - zero violations were observed this scan. ..."*
- `apply_alternative_decision` (`ai_audit.rs`): *"This codebase was judged to follow
  "{label}" for {rule_id} - {why}. This option is the absence/degenerate form of the rule's
  own requirement ..."*

### `label`: the sentence tests

Every label is a noun phrase naming a STATE THE CLIENT'S CODE IS IN. Three tests, in order:

- **LABEL-1 (the frame test, primary).** A non-`violates` option's label must complete
  **"The codebase consistently follows ___"** and remain a true, client-voiced statement
  about their code. This is not a heuristic - it is the literal sentence the engine ships.
- **LABEL-2 (the violates frame).** A `violates` option's label must complete
  **"This codebase was judged to follow ___"** - it names the bad state itself, never the
  act of flagging it. "User input reaches the sink unescaped" passes; "Flag unescaped input
  as critical" fails.
- **LABEL-3 (the decidability test).** A scan must be able to DECIDE the option by looking
  at the repository alone. If two labels differ only in what Camerata would do (scope,
  severity, report placement), nothing in the client's code distinguishes them, the
  alternatives pass's "which option does this repo follow?" question has no groundable
  answer, and whichever the model picks ships as the client's "convention". That is the
  rule-shape defect, not a wording defect - see the next section.

Advisory lexical screen (a reviewer aid, not a gate): a label containing `needs_review`,
`finding`, `severity`, `opt-in`, `pre-checked`, `report`, `scan`, or verb-form `flag` is
almost certainly in the tool's voice. It IS only advisory: `ci-cd/proc-feature-flags-1.toml`'s
label `"Flag defaults to on"` is a correct client-code state (noun "flag"), which is exactly
why the sentence tests gate and the word list does not.

Right/wrong, from the shipped corpus:

| Right (passes LABEL-1/2) | Wrong (tool voice) |
|---|---|
| `"A single shared repository error type"` (`rust/rust-domain-6-shared-repository-error.toml`) | `"Flag getSession() calls in server-execution-context files; ambiguous paths get needs_review"` (`supabase/auth/supabase-auth-getsession-server-1.toml`) |
| `"Parameterized queries / a query builder - external values are always bound, never concatenated"` (`universal/`) | `"Flag true-predicate policies targeting anon/public; high for writes, medium+needs_review for reads"` (`supabase/rls/supabase-rls-permissive-true-1.toml`) |
| `"organize packages by domain/business-capability, not by generic layer"` (`go/`) | `"Recommend the scalar-subselect wrap for auth.*() calls in policies, opt-in only"` (`supabase/rls/supabase-rls-initplan-1.toml`) |

On a `decision`-kind rule, the labels should additionally read as PARALLEL ANSWERS to
`[decision].question` - the question asks what the client's code does; each label is one
possible answer.

Scope note: the two frames render only for rules that enter the alternatives pass (multi-
option, non-CI-tier, non-`ORCH-/SPIRIT-/PROC-`, non-`defect`-kind). Labels on every other
rule still reach the proposal UI and prompts, so the tests apply to ALL labels; the frames
are just where a violation becomes a client-visible false statement.

### `why`: two voices on one field

- On a NON-violates option, `why` is read by the model and the operator only: state why this
  alternative is legitimate ("A legitimate choice when...", "The natural fit because...").
- On a `violates` option, `why` is CLIENT-ARTIFACT: `apply_alternative_decision` interpolates
  it verbatim into the defect finding's detail, directly after the label. **It must be
  client-readable FAULT-FIRST prose**: open on the concrete failure mode, in terms of their
  system's behaviour, with no Camerata vocabulary. The commonest defect in this field is
  opening on the upside ("Simpler to code, but…", "Works but…", "Appropriate only for…"),
  which is the correct voice for a NON-violates `why` and produces, here, a client's
  high-severity security finding whose detail opens by calling the state *appropriate*.

  **Correction, 2026-10-19.** An earlier version of this bullet said the `why` "must open with
  the literal token `Rejected:` (build gate)". **That is wrong in both halves and the error was
  mine, not the corpus's.** `RULES_REFERENCE.md` §10.4 is explicit: the gate
  (`options_with_rejected_prose_but_no_violates_flag`) checks ONE direction only — `Rejected:`
  prose implies the flag. The converse, a flagged option whose `why` lacks the token, is
  **not** gated and never has been. This guide's own field-interaction §1 states it correctly;
  only this bullet was wrong, and it was wrong in the section an author actually reads for the
  field.

  The measured corpus position, which is why this matters: **71 of 377 `violates` options open
  with the token, 18%.** It is not a corpus convention at all — it is one authoring lineage's
  habit. `supabase` is 32/32; `rust` 0/48, `csharp` 0/38, `python` 0/32, `go` 0/30, `java`
  0/26, `agentic` 0/21 and `ruby` 0/18 are all zero. An author trusting the retracted sentence
  would have concluded that 82% of the corpus violates a build gate, and a bulk prefix pass
  would have committed 306 more options to a token this guide elsewhere calls a wart that
  corrupts the client sentence.

  **So: `Rejected:` is OPTIONAL. Fault-first prose is MANDATORY.** The token is one way to get
  fault-first and not the only way; where it is already present, leave it. Whether to keep,
  strip, or gate it corpus-wide is an open decision recorded for the owner — do not bulk-add
  it, and do not treat its absence as a finding.

### `remediation`: the Fix line

2 to 4 imperative, client-facing sentences: what to change, WHERE (use `<path>`/`<file>` and
`captures`-driven `<token>` placeholders - lowercase ASCII/digits/hyphens only, anything else
renders verbatim), and how to VERIFY the fix is closed. It is the only source of the Fix line;
`directive` is never a fallback (fail-safe is omitting the block, not leaking the recipe).
Wrongness test: a remediation addressed to Camerata ("re-run the scan", "mark as reviewed")
instead of to the client's engineer is wrong; so is one with no verification step. Remember
the absence cost: on a recognized rule, an open action row with no fix from any source demotes
to `held` - an unauthored remediation can silently empty the action queue.

### `finding_headline` / `finding_detail`: the floor rewrite

Client-facing replacement for a deterministic floor finding's internal enforcement prose.
Author BOTH or neither (a half pair renders nothing). `finding_detail` convention: one
plain-language sentence a non-technical founder understands first, then 1-2 sentences of
what/where/impact. Same placeholder machinery as `remediation`.

### `title`, `[[sources]].title`, and the ids

- `title` reaches every artifact AND is the directive of last resort (no resolvable option,
  no summary): one sentence stating the required state of the code, client voice, imperative
  enough to survive being read as a directive in `CONVENTIONS.md`.
- `[[sources]].title` is cited VERBATIM in the client citation block: it must be the real,
  checkable title of the external source, optionally with a clause on what it establishes -
  never an internal note (that is `relevance`, which reaches nobody).
- Rule `id` and option `id` both appear in client artifacts (ids in citations/columns; option
  ids as data fields in `findings.json`). Option ids are lowercase-kebab noun phrases naming
  the code state (`"one-shared-repository-error"`), subject to LABEL-3 like the label they
  identify - `"flag-getsession-in-server-paths"` is tool voice in id form. Both are FOREVER;
  wrongness includes any rename, and any rule-id prefix chosen without checking the
  identity-driven behaviors (`SEC-`/`ARCH-` floor injection, `ARCH-OBSERVABILITY-` severity
  cap + informational routing, `ORCH-/SPIRIT-/PROC-` alternatives exclusion - REF 3.1).

---

## Options describe the client's code, never Camerata

**The invariant, first-class: every `[[option]]` must describe a state the CLIENT'S side
could be in - their code, or for process/agentic rules their adopted working practice. No
option may describe something CAMERATA does: a detector scope, a report placement, a severity
policy. If no option survives that test, the rule is not a `decision`; it is a defect rule
with a detector-tuning knob wearing a decision's clothes.**

(The "working practice" half matters: an escalation option like "Escalate before X" describes
the project's own adopted process - the client's side - and is legitimate. "Flag X at high
severity" describes Camerata's report and never is.)

Why this is structural and not stylistic, three ways:

1. **The alternatives pass asks "which option does this repo follow?"** An option like
   `"Flag every getSession() call ... without path scoping"` is not an answer a repository
   can give: no file in the client's tree distinguishes it from the scoped variant. The
   model's "decision" between them is ungroundable noise (LABEL-3).
2. **Whatever is decided ships as the client's convention.** The decided option's label and
   id land verbatim in `ConventionObserved`/`Deviation` payloads and the "consistently
   follows" sentence. A Camerata-policy option therefore gets reported as the CLIENT's
   engineering practice - the exact shipped defect.
3. **Every Camerata-behavior statement already has a dedicated field.** Detector scope and
   FP carve-outs -> `qualifies` (and the detection recipe -> `directive`); report placement
   -> `opt_in_only`; severity policy -> `min_severity`/`max_severity`/`deviation_ceiling`;
   "run it or not" -> `inspectable`/`layer3_only`. An option is never the place.

The two shipped counterexamples, both `kind = "decision"`, both already pinned on the W14
advisory roster (`DECISION_KIND_RULES_WITH_REJECTED_PROSE`,
`crates/rules/tests/w14_corpus_schema_gates.rs`), whose own comments name this sub-shape
("the options describe DETECTOR SCOPE ... not the scanned repo's own behaviour"):

- `SUPABASE-AUTH-GETSESSION-SERVER-1`: options "flag it narrowly" vs "flag it broadly".
  Neither is a state of the client's code; there is no decision for a client to make. The
  honest shape is `kind = "defect"`, one correct option ("Server code establishes identity
  via getClaims()/getUser()"), one `violates = true` option ("Server code trusts
  getSession() for identity"), with the path-scoping/needs_review policy in `qualifies`.
- `SUPABASE-RLS-INITPLAN-1`: options "recommend the wrap, opt-in only" vs "report it as a
  standard finding". Both describe Camerata's report. The code states are "policies wrap
  auth.*() in a scalar subselect" vs "policies call auth.*() bare"; opt-in placement is
  already carried by the rule's own `opt_in_only = true`.

**Corollary for `kind`:** strip the Camerata-voice options and look at what remains. Two or
more genuine code states of equal weight -> `decision`. One correct state plus justified
exceptions -> `default_with_exceptions`. Only one acceptable state -> `defect`. Equal states
plus a named fault under all of them -> `decision_with_floor`.

**Do not drive-by fix the two pinned rules.** They sit on the owner's A5.7 reclassification
hold, the roster test fails on silent shrinkage, and re-shaping them changes option ids that
projects may have persisted. Author no NEW rule in this shape; route the existing two through
the A5.7 ruling.

---

## The MULTI-AUDIENCE fields: the remaining standards

Renamed 2026-10-19. This section was headed "Model- and operator-facing fields", which was
wrong in a way that mattered: `directive` and `qualifies` both reach the CLIENT's repository,
and filing them under a model/operator heading is exactly the misreading that let the detector
voice survive in `directive` for the whole `supabase/` pack. Every field below has more than
one reader. See "one voice, not one voice per audience" above.

- **`directive` has THREE audiences and ONE correct voice.** It is the scanning model's rule
  list, the client's agents', and the client's engineers'.

  **Correction, 2026-10-19.** An earlier draft of this section said the directive is "the
  detection/enforcement recipe in the scan and alternatives prompts - tool voice is correct
  THERE". That is wrong, and it is the same misreading that let the detector voice in at all.
  Tool voice is not correct for the model either. The scan prompt introduces this list as
  *"the rules the project has ADOPTED"* (`ai_audit.rs:389`) and renders each entry as
  `- [RULE-ID] directive` (`ai_audit.rs:2247`), so a directive reading "Regex-scan for
  `getSession(` ... Emit a high-severity finding" instructs a language model to impersonate a
  regex engine instead of telling it what the code must do. The W15 investigation found that
  independently: when `apply_alternative_decision` rewrites a project's effective directive to
  such text, the conformance pass is asked the wrong question and correctly finds nothing.

  So there is no audience for whom the scanner recipe is the right text, and emitting the
  directive to the client is not a leak to be plugged - it is the PRODUCT. Deriving a
  repository's conventions and shipping them as governance the client's own agents will follow
  is the point of the whole pipeline. The defect is only ever the voice. One well-written
  directive serves all three readers:

  > Server-side identity is established via `getClaims()` or `getUser()`; never trust
  > `getSession()` in a server execution path. `getSession()` does not revalidate the token,
  > so a forged cookie can impersonate any user.

  The model can check code against that, the client's agent can follow it, and their engineer
  can read it. Nothing is lost by dropping the scanner phrasing and the scan improves.

  The selected option's directive is emitted verbatim under
  `### {id} - {title}` into the client's `CONVENTIONS.md`/`AGENTS.md` as a convention the
  project follows. Write it as the behavioral commitment an engineer or agent working in
  that repo can follow ("Establish server-side identity via getClaims() or getUser(); never
  trust getSession() in a server execution path"), with WHERE-to-look specifics as needed.
  Scanner mechanics that mean nothing to the client's agent - severity assignments,
  `needs_review` routing, canned finding sentences - belong in `qualifies` or the severity
  bounds, not here. (Several `supabase/` directives currently embed full finding templates;
  that is the same voice defect one field over, and the corpus audit covers it.)
- **`qualifies` must survive two readings:** the operator's "when does this rule apply"
  summary, and - when no option ever resolves - the emitted/diffed fallback DIRECTIVE. State
  scope, carve-outs, and enforcement honesty ("coverage is grounding/advisory; Camerata
  ships no mechanical check") in prose that still works if a client's agent reads it as the
  rule's text. Always at RULE level; nested in `[[sources]]` it is discarded and now fails
  the build (REF 3.9).
- **`[decision].question`** is asked OF the client's code, TO the architect: "How does the
  API deduplicate retried mutations?" - and the options must read as its answers. A question
  about Camerata ("...and is this ever pre-checked automatically?") is the option-shape
  defect surfacing one field earlier.
- **`escalation.condition`** grounds the governed agent: write an observable predicate over
  the change in flight ("the change would modify or delete an existing test assertion"),
  not a value judgment ("the change is risky"). The agent must be able to self-test it; the
  server resolves `severity` authoritatively, so the condition carries no severity prose.

---

## Field interactions an author must respect

Stated here as authoring rules; mechanics and citations in the reference.

1. **`kind` x `violates`.** No `violates = true` option on a `decision`-kind rule (build
   gate). A `why` opening `Rejected:` requires `violates = true` on any kind that permits
   the flag (build gate); on a `decision`-kind rule the same prose is an advisory signal
   that the KIND is wrong, pinned to a roster. Converse convention: every `violates` option's
   `why` opens with `Rejected:`.
2. **Severity bounds order.** `min_severity <= max_severity` and
   `deviation_ceiling <= max_severity`, both build-gated; exact vocabulary
   `critical|high|medium|low|info` (a typo is NOT a no-op - gotcha 1 - the gates catch it).
   `min_severity = "low"|"info"` is inert; if you meant "cap at low", the field is
   `max_severity`. `deviation_ceiling` is INERT on a `defect`-kind rule (no Deviation rows
   exist to clamp). If the rule's prose names a severity, transcribe it: "at least X" ->
   `min_severity`, "never more than X" -> `max_severity`.
3. **`inspectable = false` is emit-only, totally.** The rule still emits to the client repo
   and still proposes/arms in the UI, but it has NO scan surface at all - no ledger entry,
   not in "verified clean", not in "rules checked", not even in "rules not run". Never set
   it `false` to quiet a noisy rule; that is what severity bounds and `opt_in_only` are for.
4. **`opt_in_only` x `default`.** `opt_in_only = true` means no unattended scan ever runs
   the rule; it pairs naturally with `default = false` (architect consciously chooses), and
   `default = false` on a `default_with_exceptions` rule is legitimate, NOT a contradiction
   (gotcha 2's retraction). But `default = false` costs real output: no Fix line, no floor
   text, no effort band, no escalation until a project persists a `chosen_option`.
5. **`default = true` obligations.** `[decision].default` must match an `[[option]].id`
   character-for-character (a dangling id is UNGATED and silently degrades everything that
   resolves through the default - gotcha 11), and that option must carry `remediation`
   (build gate). CI-tier options with `remediation` must also carry `effort`, value exactly
   `low|medium|high` (presence is gated, the VALUE is not - gotcha 12).
6. **`verification` must arm.** `grounded`/`verified`/`policy` only (build gate); `grounded`
   requires at least one real external `[[sources]]` URL, and `policy` is the honest label
   when there is none. Omitting the field is the A7 trap (defaults `draft`, disarmed
   everywhere).
7. **Per-source notes go in `relevance`,** rule applicability in rule-level `qualifies`;
   `qualifies` inside `[[sources]]` fails the build.
8. **New folder/stack = wiring, not just TOML:** stack detection, `domains_for_stack`, and
   `domain_to_glob`, plus a proposed-rules test - or every rule in the pack is orphaned.

---

## The id conventions that carry meaning

- **`SEC-*` / `ARCH-*`** are the **security floor**: always-on, hard-deny, enforced at layer 1. They
  are distinguished by the id prefix (and `enforced_gate_rules()` / `governed_role` inject them into
  every role's subset). If you add a floor rule, it must have a real layer-1 detector (see below) —
  a floor rule with no detector denies nothing.
- **`GOV-1`** is the governance meta-rule, always in scope.
- Domain prefixes (`AGENTIC-`, `RUST-`, `ORCH-`, `CICD-`, `UI-`, …) group by area + map to the folder.

---

## The decision that actually matters: which channel, and does it need code?

### A. Pure prose / structured rule (the common case) — NO code

The rule guides the agent through grounding (and emits to CONVENTIONS/AGENTS for structured). Write the
TOML, done. The agent reads it; there is no mechanical block. Use this for judgment-shaping guidance
that you do NOT need to hard-enforce.

### B. A GATED rule (mechanically blocked) — NEEDS CODE

If you want a write or a change to be **mechanically blocked**, the corpus TOML is not enough. You must
wire a detector into an enforcement layer:

- **Layer 1 (the gateway, pre-write deny):** add the rule's matcher to the gateway's rule registry
  (`crates/gateway/src/`), where a tool call's path/content is matched and a `Deny { rule, reason }`
  is returned. This is how `SEC-*`/`ARCH-*` actually block.
- **Layer 2 (post-task checks):** add a check to the toolchain runner (`crates/checks/`,
  `runner_for_worktree`) that inspects the worktree after the agent runs and reports a violation that
  bounces the agent.

The `enforcement = "mechanical" | "architectural"` classification documents your intent, but the block
only exists once the detector exists. **A new architectural rule in the corpus with no detector is
advisory-via-grounding only.** Say so honestly in `qualifies` if Camerata does not ship the check.

### C. An ESCALATION rule (pause for a human) — the canonical "TOML plus a little"

This is the example that motivated this guide. An escalation rule pauses the governed run for human
review instead of denying (the agent stops and asks) or instead of merely advising.

**Escalation is OPTION-SCOPED.** It lives on the `[[option]]` that calls for it, not the rule, so a
rule can offer an escalating option alongside non-escalating ones. Selecting a non-escalating option
does NOT escalate.

```toml
[[option]]
id = "escalate-before-doing-X"
label = "Escalate before X"
directive = "…"
why = "…"
escalation = { condition = "the change would do X", severity = "hard-pause" }

[[option]]
id = "allow-X"
label = "Allow X"
directive = "…"
why = "…"
# no `escalation` -> choosing this option does NOT escalate
```

What you get, and what (if anything) you must wire:

1. **Agent-driven enforcement is AUTOMATIC — zero code.** When a project selects an option that
   carries an `escalation`, the implementer agent is grounded with its `condition` (under an
   `## ESCALATION CONDITIONS` section) and given the `raise_escalation` tool. When the agent's work
   meets the condition it calls the tool; the server resolves severity AUTHORITATIVELY from your spec
   (the agent cannot downgrade a hard-pause) and either pauses the run for human review (hard-pause:
   checkpoint + a UoW review escalation + `AwaitingReview`, resolvable Approve/Amend/Reject) or logs
   it and continues (soft-flag). **So a brand-new escalation rule needs only the TOML option field.**

2. **A deterministic backstop is OPTIONAL and needs code.** If the condition is mechanically
   detectable and you want a safety net that does not rely on the agent self-reporting (e.g. the
   test-tamper diff scan), you write the detector + wire it. The canonical example: `test_tamper.rs`'s
   `detect_test_tampering` runs post-task; `test_tamper_escalation(corpus, selections)` resolves the
   rule's active spec (so the backstop is also field-driven, reading YOUR `escalation` spec rather
   than hardcoding option ids); the run branches on severity exactly like the agent-driven path. Only
   the test-tamper rule has a backstop today; most escalation rules are agent-driven only, and that is
   fine.

3. **`severity` is your call:** `hard-pause` (stop and wait for a human, the safe default for
   guard-area conditions) vs `soft-flag` (log a warning and let the run continue). Absent → hard-pause.

**Escalate vs deny:** if a rule should push the agent to FIX something rather than ask a human, it is a
deny/bounce (channel B), not an escalation. No `escalation` field; wire a gate/check. Escalation is
specifically "a human decides."

---

## The checklist for adding a rule

1. **Id + title + domain + enforcement + `kind` + `inspectable`** filled, id is new + stable,
   domain agrees with the folder. The build fails without either `kind` or `inspectable`; pick
   `kind` from the four values documented in `docs/RULES_REFERENCE.md`, and set `inspectable`
   honestly (can a scan of a repo assess this rule at all?).
1b. **If the rule's prose names a severity, author it as a structured bound** rather than leaving it
   in prose the pipeline cannot read. The corpus went through a transcription pass for exactly this
   reason: the bounds had been authored all along, in sentences no code could parse. Which field,
   and which direction a named severity binds in, is in `docs/RULES_REFERENCE.md`.
2. **Decide the channel:**
   - Guidance only → prose/structured, write the TOML, done (channel A).
   - Must be mechanically blocked → write + wire a layer-1/2 detector (channel B). The TOML alone does
     not block.
   - A human should decide when a condition is met → add `escalation` to the relevant option(s)
     (channel C). Agent-driven works immediately; add a backstop detector only if you want one.
3. **If it is a floor rule (`SEC-`/`ARCH-`):** it MUST have a real layer-1 detector, and confirm it is
   injected via `enforced_gate_rules()` / `governed_role`.
4. **Options:** stable ids; if `default = true`, the `[decision].default` points at a real option id.
4b. **Every option passes the sentence tests.** Non-violates: "The codebase consistently
   follows ___". Violates: "This codebase was judged to follow ___". And LABEL-3: a scan of
   the repo alone can decide the option. An option describing what CAMERATA does (scope,
   placement, severity) is a rule-shape defect - see "Options describe the client's code,
   never Camerata". Check the voice of every client-facing field against the audience table.
5. **Honesty in `qualifies`:** if Camerata does NOT ship the mechanical check for this rule, say that
   the coverage is grounding/advisory (don't imply enforcement that isn't there). See the test-tamper
   rule's `qualifies` for the model.
6. **Verification/sources:** set `verification` truthfully; add `[[sources]]` if grounded; only add
   `[verified]` when a human actually confirmed it.
7. **Tests:** a gated rule needs a detector test; an escalation rule's option-scoping is covered by the
   generic `selected_escalation` test, but add a rule-specific one if it has a backstop.

## Where the moving parts live (quick map)

| Concern | Location |
|---|---|
| Rule TOML | `crates/rules/principles/<domain>/<id>.toml` |
| Rule struct / loader / `selected_escalation` | `crates/rules/src/lib.rs` |
| Layer-1 gate (deny) detectors | `crates/gateway/src/` |
| Layer-2 checks | `crates/checks/` (`runner_for_worktree`) |
| `raise_escalation` tool (agent-driven) | `crates/gateway/src/main.rs`, `crates/agent/src/lib.rs` |
| Escalation grounding + sink read + handler | `crates/server/src/dev_implement_run.rs` |
| Test-tamper backstop (the one detector-backed escalation) | `crates/server/src/test_tamper.rs` |
| Floor injection | `enforced_gate_rules()` / `governed_role` (`crates/fleet/src/`) |

## The honest summary

Most rules are TOML-only (grounding does the work). A rule that must be *blocked* needs a detector you
write and wire. A rule that should *pause for a human* needs only an option `escalation` field for the
agent-driven path, plus an optional detector if you want a deterministic backstop. Classifying a rule
`architectural` in the TOML does not enforce it; only a wired check does. When in doubt, write what is
true in `qualifies` so the rule never over-promises.
