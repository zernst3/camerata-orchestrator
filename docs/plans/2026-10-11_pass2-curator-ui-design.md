# Pass 2 - Curator UI design for the five-kind finding taxonomy (2026-10-11)

> **STATUS, 2026-10-15: PARTLY BUILT.** This was a design document with no code when it was
> written. Steps 1, 2 and 3 of section 11 are shipped, so is the manual-sort requirement
> section 5 mentions in passing, and so is the docked detail pane of section 2 — with the
> `C`/`I`/`T`/`F` keystroke step 3 deferred to it, its Q5 scoping, and the five-second undo.
> The full built-versus-designed inventory, with file:line, is section 14 at the bottom; read
> it before treating any paragraph here as a description of the product.
>
> **Section 13's chorale dependency check is RETRACTED.** Both of its headline claims were
> wrong, and both were discovered at build time rather than by review. See the retraction
> note in that section before relying on any chorale capability named anywhere in this
> document.

Design document, written before any code existed. Builds on:

- `crates/api-types/src/audit.rs` - `FindingKind` and its payloads (the data model this designs for)
- `crates/server/src/onboard.rs` - `Finding` (the wire row: flattened `kind`, `also_locations`, `confidence`, `effort`, `evaluated_option_id`, `captures`)
- `crates/server/src/report_export.rs` - `site_count`, `curated_findings` / `held_for_review_findings`, `rules_not_run`, `failed_passes`
- `crates/ui-core/src/triage.rs` - `TriageModel` / `TriageState` / `FindingView` (the current triage machinery)
- `crates/ui/src/cockpit/scan.rs` - `ScanResults`, `FindingsTable`, `RuleAlternativesPanel`, the finding detail modal (the current surface)

Design goal, in the owner's words: make it as easy as possible for the curator to review
everything and decide. Hundreds of accept/reject/adopt decisions in one sitting. This is the
daily working tool, not a dashboard.

---

## 1. The recommendation

**Keep one screen. Do not keep one table.**

The "one table that flips between views" premise does not survive contact with the five kinds,
and the data model itself says why: `FindingKind::is_informational()` is already a structural
split. `Defect` and `Deviation` are accusations with a site, a severity, and a
dispose-this-row decision. `ConventionObserved`, `NoConvention`, and `NotAssessed` have no
site to dispose, no meaningful severity (they ship at `info` by construction), and decisions
that are per-RULE, not per-row. Forcing all five into one grid means either a
lowest-common-denominator action bar (every button dead for most rows, violating the standing
hide-dead-end-affordances rule) or a per-row action soup.

So the recommendation is **one Review workbench with a kind switcher as the primary axis**,
containing exactly two shapes of surface:

1. **The Queue** - one table for the two accusing kinds (`Defect` + `Deviation` + any
   `unspecified` row). This IS the current `FindingsTable` and `TriageModel`, extended, not
   replaced. The four-disposition flow (Unresolved / Ignored / Tech debt / False positive)
   survives intact and gains a fifth disposition, Confirmed (section 4).
2. **Three compact panels** for the informational kinds - Conventions, No convention,
   Not assessed. Lists, not grids. Each has exactly the one decision its kind needs, and
   `NotAssessed` has none at all.

This honors the spirit of "one table that flips between views": the curator still works one
screen top to bottom, and the switcher is still the primary gesture. What changes is that
flipping the view also flips the action vocabulary, because the decisions genuinely differ.
Putting a `ConventionObserved` row into the triage queue would be the UI-level version of the
`primary_rank` bug W7 fixed in the merge layer: a compliment competing with an accusation.

Why not five fully separate tabs (one per kind)? Because `Defect` and `Deviation` share the
same decision shape (is this real, do we act) and the curator should burn down ONE actionable
queue, not two. Splitting them doubles the "am I done yet" bookkeeping for zero decision
value. The kind distinction inside the queue is carried by a badge column, group headers, and
the ranking model (section 5) instead.

---

## 2. The workbench layout

```
+---------------------------------------------------------------------------+
| Scan summary strip (unchanged: repos / files / cost / provenance)         |
+---------------------------------------------------------------------------+
| REVIEW WORKBENCH                                                          |
|                                                                           |
| [ Queue 212 ] [ Conventions 38 ] [ No convention 11 ] [ Not assessed 7 ]  |
|   ^ primary kind switcher; counts live. "Queue" = Defect + Deviation      |
|     (+ a red "3 unspecified" chip on Queue if any kind-less rows exist)   |
|                                                                           |
| -- when Queue is active ------------------------------------------------ |
| [ Unresolved 180 ] [ Confirmed 12 ] [ Ignored 9 ] [ Tech debt 8 ] [ FP 3 ]|
|   ^ the existing disposition sub-tabs, plus Confirmed                     |
|                                                                           |
| +-------------------------------------------+---------------------------+|
| | QUEUE TABLE (chorale, grouped by rule)    | DETAIL PANE (docked)      ||
| |                                           |                           ||
| | v SQL-INJECTION-1 · DEFECT · critical (3) | SUPABASE-API-VERSIONING-1 ||
| |   [ ] handlers.rs:88   crit  2 sites      | DEVIATION                 ||
| |   [ ] handlers.rs:214  crit               | "2 of 47 endpoints do not ||
| |   [ ] export.rs:31     crit               |  use URI versioning"      ||
| |                                           |                           ||
| | v API-VERSIONING-1 · DEVIATION            | Convention: URI-versioned ||
| |   "2 of 47 endpoints deviate from         |  endpoints (versioned)    ||
| |    URI-versioned endpoints"               | Severity: medium          ||
| |   [Confirm all 2] [FP all 2]              |  (consistency; capped)    ||
| |   [ ] routes.rs:12    med                 | Evidence / snippet        ||
| |   [ ] routes.rs:40    med                 | Sites (2): ...            ||
| |                                           | Fix: ...                  ||
| | > NO-RAW-SQL-1 · DEFECT · high (4)        |                           ||
| | ...                                       | [C]onfirm [I]gnore        ||
| |                                           | [T]ech debt [F]alse pos   ||
| +-------------------------------------------+---------------------------+|
| | Toolbar: Confirm | Ignore... | Tech debt | False positive... | CSV    ||
| +----------------------------------------------------------------------+ |
|                                                                           |
| v Coverage (curator drawer, collapsed by default) ........... section 7   |
+---------------------------------------------------------------------------+
```

Notes on the sketch:

- **The docked detail pane replaces the row-click modal for the Queue.** At 400+ rows the
  modal's open/decide/close cycle is the single biggest per-decision time cost in the current
  UI. A docked pane plus arrow-key row focus turns review into cursor-down, read, one
  keystroke. chorale pulled master/detail forward into v0.2.x; this is its exact use case
  (verify the installed chorale version exposes it; if not, that hook is the one piece of
  enabling work worth asking chorale for, alongside the group-header hook in section 9).
  The modal remains for the informational panels, where volume is low.
- **Group headers carry the Deviation sentence.** A `Deviation` row alone carries numerator,
  denominator, and the convention label as structured fields, so the rule group header
  renders the owner's canonical sentence verbatim from data: "N of M sites deviate from
  <convention_label>". No prose parsing, per the module doc in `audit.rs`.
- **Group headers carry rule-level bulk actions** for deviations (Confirm all / FP all),
  because the natural unit of a deviation decision is the rule, not the site: if the curator
  believes the convention is real, all N sites are confirmed together far more often than not.

---

## 3. What each kind IS to the curator, and the decision it needs

| Kind | Volume | The question the curator answers | Decision set | Fastest gesture |
|---|---|---|---|---|
| `Defect` | high | "Is this real, and what do we do about it?" | Confirm / Ignore(reason) / Tech debt(later-now) / False positive(reason) | focused row + single key (C/I/T/F); bulk via selection + toolbar |
| `Deviation` | medium | "Is the convention real, and are these really exceptions?" | same five dispositions per site, PLUS rule-level bulk on the group header | group-header [Confirm all N] one click; per-site keys for the mixed cases |
| `ConventionObserved` | medium-high | "Does this belong in the client's derived convention set (AGENTS.md)?" | Adopt / Skip, per rule | checklist, ALL CHECKED by default, uncheck to skip, one "Adopt N conventions" commit |
| `NoConvention` | low (1 row/rule by A5.3) | "Do I recommend an option going forward, or report the split as-is?" | Recommend <option> / Leave as reported | per-row select over `observed_options` (counts shown); default = leave |
| `NotAssessed` | low | none - it is an honesty row | NONE. Read-only. | none |
| `unspecified` | should be zero | "which emission path is broken?" | none in the UI; it is a pipeline bug surfaced loudly | red chip on the Queue tab + banner; rows render in the Queue, held, undisposable except FP |

Rationale for the sharp edges:

- **Confirmed is a new, explicit disposition.** Today the Queue's end state is "nothing
  Unresolved", reachable only via Ignore or Tech debt - there is no way to say "yes, this is
  real, it ships in the report" except by leaving the row Unresolved, which the Process gate
  then blocks on. That was tolerable when every row was an onboarding violation to be
  waived or ticketed; it is wrong for an audit deliverable where "confirmed, ships" is the
  MAJORITY outcome. Confirm is a local disposition like False positive: it needs no reason,
  commits nothing at Process (the report export reads it), and it is the one-keystroke
  default gesture. This is the accept in "accept/reject/adopt".
- **Deviations get rule-level bulk because of their shape.** Every deviating row for a rule
  carries the same group-level numbers (`DeviationKind` doc), so the group IS the decision
  unit; the per-site gestures exist for the minority case where one of the N "exceptions" is
  actually a false positive while the rest are real.
- **Conventions default to adopted.** A5.5 says the derived set ships in the product; the
  audit already did the deriving. Opt-out (uncheck the wrong ones) is O(errors); opt-in is
  O(rules). With 30-80 convention rows per scan, opt-out is the only defensible default.
  The commit is still one explicit act ("Adopt N conventions"), so nothing lands silently.
- **NotAssessed gets no buttons.** There is no per-rule re-run mechanism in the product
  today, and the standing rule is to hide affordances the user cannot act on. A disabled
  "re-run" button would be exactly the 403-button bug. If a re-run mechanism lands later,
  the affordance appears with it. The rows group by `NotAssessedReason` (a closed vocabulary,
  so grouping is structural) with `reason_detail` on expand.
- **`unspecified` is treated as an alarm, not a kind.** The backfill rule in `audit.rs` says
  an absent kind is a gap to surface, never to paper over. The UI's job is to make the gap
  impossible to miss (red chip + banner naming the count) and impossible to quietly
  disposition away as if it were understood - False positive (with reason) is the only exit,
  because "the pipeline mislabeled this" is literally what FP means.

Reason capture moves out of the toolbar. The two permanently visible textareas (ignore
reason, FP reason) become an on-demand inline prompt that appears when the action is invoked
(keystroke or button), pre-focused, Enter to commit, Esc to cancel. Same require-reason
invariant, enforced at the same point; two fewer always-rendered inputs.

---

## 4. The Queue in detail

Columns, revised (current column set in `finding_columns`, scan.rs:1524):

| Column | Status | Why |
|---|---|---|
| Kind | NEW, badge | `DEFECT` (red family) vs `DEVIATION` (blue family) vs `UNSPECIFIED` (red outline). The single most load-bearing new pixel. Filterable. |
| Severity | kept, re-rendered | Deviation severities render with a visually distinct treatment (outline badge + "consistency" in the tooltip) so a medium deviation is never confusable with a medium defect. Same words, different claim. |
| Authority | kept, narrowed | enforced / preview / advisory is orthogonal to kind (trust tier of the producer, not the nature of the statement) and still earns its place, but at chip width, not 220px with long labels. Long label moves to tooltip. |
| Finding type (rule id) | kept | group key; +N `also_matches` chip stays. |
| Sites | NEW, narrow | `site_count` (1 + `also_locations.len()`). Renders only when > 1, as "x3". Sortable. See section 6. |
| Confidence | REPLACES "Needs review" column | the structured `confidence` field ("high" / "needs-review") as a narrow chip; the verbose inline reason moves to the detail pane. Recovers ~300px. |
| Enforcement (status) | kept | active vs baseline/waived is still the ratchet. |
| File / Line / Snippet | kept | unchanged. |
| Bucket | kept, Tech debt view only | unchanged. |

Group headers (rule level), the other load-bearing surface:

```
v SUPABASE-API-VERSIONING-1 · DEVIATION · medium                    2 sites
  2 of 47 assessed endpoints deviate from "URI-versioned endpoints"
  Evaluated against: URI-versioned endpoints (AI recommended) · Why? · Change
  [Confirm all 2]  [False positive all 2]
```

- Line 2 is the structured sentence (`deviating_sites` / `assessed_sites` /
  `convention_label`). When `assessed_sites` is `None` the renderer shows the numerator
  alone ("2 endpoints deviate from ..."), never a fabricated denominator, exactly per the
  field's doc.
- Line 3 is the current `RuleAlternativesPanel` row, relocated. See section 9 for what that
  removes.
- Defect group headers are plainer: `rule-id · DEFECT · worst-severity · (n)`, plus the same
  Evaluated-against line when the rule is multi-option.

---

## 5. The ranking model

The hard problem as stated: severity means different things per kind, deviations cap at
medium by default, and a naive severity sort mixes incomparable things.

Two decisions dissolve most of it:

**First, the UI never re-derives or re-weights severity.** Comparability is manufactured
upstream, deliberately, by the pipeline: `max_severity` ceilings (E2/W5), the coming
`min_severity` floors (W8), and `deviation_ceiling`. By the time a row reaches the UI its
severity is the calibrated, authored-bounds-respecting figure. A UI that second-guesses that
would be a second calibration layer - the exact mid-layer accretion the Fable review called
out. The UI's honesty obligation is PRESENTATION: make a deviation's medium visibly a
different claim than a defect's medium (the badge treatment above), not a different number.

**Second, informational kinds never enter the Queue, so the worst incomparabilities
(a convention "severity" vs anything) structurally cannot meet.**

What remains is ordering defects and deviations in one list. The sort key, lexicographic:

1. `status` - active before suppressed (unchanged from today).
2. `severity` - critical > high > medium > low, as calibrated upstream.
3. `kind` - Defect before Deviation within a severity band. Same justification as W7's
   `kind_actionability_rank` in the merge layer: an accusation of objective fault outranks an
   accusation of inconsistency at equal severity. The UI mirrors the pipeline's own rank
   rather than inventing a second one.
4. Within Deviations: ascending deviation ratio (`deviating_sites / assessed_sites`;
   unknown denominator sorts after known). A 2-of-47 deviation is a near-unanimous
   convention with two clear outliers - the fastest, highest-confidence decisions in the
   whole queue - while a 19-of-47 is drifting toward no-convention and needs thought.
   Cheapest decisions first is the right order for a human burning down hundreds of rows.
5. Tie-break: rule id, then path, then line.

Groups order by their best row; rows order within groups by the same key. The default sort
is a curated order, and the column headers still allow any manual re-sort chorale supports -
the default is opinionated, the table is not locked.

Why not interleave by "real cost" (forward drift vs current risk)? Because that judgment is
already encoded where it belongs: the authored `deviation_ceiling` lets a rule declare its
inconsistency genuinely high-cost, and such a deviation then ranks in the high band by rule 2
with no UI special-casing at all. The escape hatch and the ranking compose for free.

---

## 6. Merged findings (one row, several places)

The shipped bug (C5) was an absorbed site reaching NO artifact. W7 fixed the data:
`also_locations` reaches exporters verbatim and `site_count` is on every row. The UI design
obligation is the same honesty with the same discipline: never double the row count, never
bury the sites.

- **In the row:** a narrow Sites column rendering "x3" when `site_count > 1`. Nothing when 1.
  Sortable, so "show me the multi-site rows" is one click.
- **In the group header:** deviation numerators are already site-based (computed structurally
  from rows plus absorbed sites, per `DeviationKind`), so the header sentence needs no
  additional merge handling - the numbers are already honest.
- **In the detail pane:** a "Sites" section listing the primary site first, then every
  `also_locations` entry as its own line (`path:line` + its original `rule_id` + snippet),
  with `consequence: true` entries visually subordinated under an "also affects" sub-label
  (that is exactly what the field's doc says it means). This is the full disclosure surface.
- **Dispositions apply to the whole row**, all sites together. If the curator believes one
  absorbed site is wrongly merged, that is a pipeline merge error, and the honest gesture is
  FP-with-reason on the row (the reason names the bad site) - not a UI-side row split, which
  would create rows the pipeline cannot account for and break conservation.

Rejected alternative: expandable site sub-rows inline in the table. It doubles visual row
count for the minority of merged rows, interacts badly with the rule-to-file grouping already
in place, and the detail pane gives the same information at zero table cost.

---

## 7. Coverage: one source, two renderings

The owner's constraint is already two-sided: client-facing coverage must be a concise
one-line-per-tool summary (G3, shipped and tested in W7), but the curator may need more.
These are different questions. The client asks "can I trust the scan?"; the curator asks
"what exactly can I not vouch for, and why?"

**Two renderings, one data source. Neither is a new page.**

1. **Client rendering: unchanged and not duplicated in the workbench.** The concise
   per-tool lines ship via `failed_passes` through the existing exporters. The workbench
   shows them only inside the export panel's preview, where the curator is checking what the
   client will see.
2. **Curator rendering: a Coverage drawer at the bottom of the workbench,** collapsed by
   default. Contents, all from existing structured data, no new pipeline fields:

```
v Coverage - 417 rules armed
  Assessed clean (convention observed)   38 rules
  Assessed with deviations               12 rules · 31 sites
  Defects found                          24 rules · 181 sites
  No convention                          11 rules
  Not assessed                            7 rules    [-> Not assessed panel]
     tool_failed 3 · no_files_matched 2 · pass_failed 1 · model_declined 1
  Rules not run (scope/stack)            94 rules    [expand: id + reason]
  Tool notes                             eslint - ... (the per-tool lines, verbatim)
```

  The rollup's rows link into the panels that hold the detail (Not assessed, Conventions).
  This drawer is diagnostic and internal; nothing in it alters what exports. It answers the
  question the curator currently answers by cross-reading the ledger, the coverage notes box,
  and the xlsx Coverage sheet.

The existing `scan-coverage-notes` box (scan.rs:3798) folds into this drawer and disappears
as a separate surface.

---

## 8. Interconnection: the rule modal is the hub

Conventions, deviations, defects, and coverage all reference the same rules and files. The
temptation is n-squared cross-links. Most would be noise. The navigation paths the curator
will actually use:

1. **Rule -> everything about that rule.** This is THE join, and it gets the investment: the
   existing rule detail modal gains a "this rule in this scan" strip - kind outcome, counts,
   the convention label if any, the evaluated-against option, and jump links into the Queue
   (filtered to the rule) or the relevant panel. Reachable from every surface that shows a
   rule id (Queue rows, group headers, Conventions list, No convention list, Not assessed
   list, Coverage drawer). One hub instead of many point-to-point links.
2. **Deviation -> its convention.** The group header names it; clicking the convention label
   opens the rule hub. Note the data honestly: conforming sites are not in the data (only
   counts), so there is no "show me the 45 conforming endpoints" link to offer. Do not
   fabricate one.
3. **File -> everything in that file.** Already served by the existing File column text
   filter. A filter, not a hyperlink graph. Sufficient.

Paths deliberately NOT built: convention -> "sibling deviations in other rules",
coverage line -> tool logs, finding -> finding similarity links. Each is a page of plumbing
for a navigation the curator has never needed in practice; the hub covers the real cases.

One cross-surface rule worth stating: **a rule appears in exactly one kind surface per
scan.** The pipeline guarantees it (a rule emits deviations OR a convention row OR one
no-convention row OR not-assessed), and the UI should assert it rather than defend against
it - if a rule ever appears in two, that is a pipeline bug to surface, not to tolerate.

---

## 9. What to remove

Removal is the design work. In order of conviction:

1. **The `RuleAlternativesPanel` as a standalone panel** (scan.rs:2909). Its own doc comment
   admits it exists only because chorale's grouped table "exposes no group-header
   content-injection hook" - it is a workaround rendered as a feature, duplicating per-rule
   context away from the rows it describes. The Evaluated-against line, Why?, Change select,
   and the staged-override/rescan flow move into the rule group headers and the rule hub
   modal. The sticky rescan bar survives as-is (it is cross-rule state and genuinely global).
   Enabling work: a group-header content hook in chorale - worth requesting; it also unlocks
   the deviation sentence and bulk actions (section 4), so it pays for itself three times.
2. **The two always-visible reason textareas** in the Queue toolbar. Replaced by on-demand
   prompts (section 3). An empty textarea the curator is not using is noise 95% of the time.
3. **The "Needs review" 300px column with inline prose reason.** Superseded by the
   structured `confidence` chip; the reason text lives in the detail pane. The
   `split_needs_review` prose-parsing path in the UI should be retired entirely once the
   structured field is universal - a UI parsing model prose violates the same principle the
   backend just spent a whole pass eliminating ("structured identity, never prose").
4. **Severity on informational surfaces.** Conventions, No convention, and Not assessed
   panels render NO severity anywhere. The kinds ship at `info` by construction; a severity
   badge there is a dead affordance implying a ranking decision that does not exist.
5. **The findings-key legend** ("Security findings" red swatch / "Architectural findings")
   above the table (scan.rs:1846). Superseded by the Kind badge column, which says more,
   per row, in less space. The red full-row critical highlight stays.
6. **The row-click modal for Queue rows,** replaced by the docked detail pane (section 2).
   The modal pattern stays for low-volume surfaces.
7. **"What's healthy" as a ledger-driven list, eventually.** The Conventions panel IS
   "what's healthy", stated precisely (which convention, which option, adopted or not)
   instead of as a rule-emitted/not-emitted inference. This intersects open ruling 7 in the
   tracker (never-falsely-healthy direction) - flagged there, not unilaterally changed here.

Kept deliberately: the four-disposition flow and Process semantics (ignores -> baseline,
debt -> tickets, FP -> no-op), the CSV export (lossless flat escape hatch), the Authority
column (narrowed), active-before-suppressed ordering, the auto-saved draft machinery
(dispositions already persist in `OnboardingDraft`; Confirmed rides the same map for free).

---

## 10. Enabling work (UI data plumbing, stated so it is not discovered mid-build)

- `FindingView` (ui-core/triage.rs) must mirror the new wire fields the same way it mirrors
  `confidence`/`effort` today, with the same round-trip contract tests: the flattened
  `kind` (as `Option<FindingKind>` - the api-types crate is already a UI dependency-safe
  home), `also_locations`, and `site_count` if the server adds it to the live scan row (it
  exists on export rows today; the UI can compute `1 + also_locations.len()` identically,
  which is the better choice - one source of truth, no new wire field).
- `finding_key` must stay stable across the kind backfill (it keys on repo/rule/path/line/
  snippet, none of which the taxonomy touches - verified, no change needed).
- chorale asks: group-header content hook (section 9.1), master/detail docked pane
  (section 2). Both were already on chorale's own roadmap trajectory.

---

## 11. What I would build first

The smallest slice that makes triage materially faster, in one sentence: **get the
informational rows out of the accusing queue and put the deviation sentence where the
decision happens.**

Concretely, in order:

1. Mirror `kind` into `FindingView`; add the Kind badge column and the kind-aware default
   sort (section 5). Route informational kinds out of the Queue into three minimal list
   panels (even ugly ones). This alone removes roughly a third of the rows from the
   400-row sitting and ends compliment-vs-accusation interleaving.
2. The deviation group-header sentence from structured fields, plus `Confirm all N` /
   `FP all N` on deviation group headers. This converts the most common deviation decision
   from N gestures to one.
3. The Confirmed disposition (fifth state in `TriageModel`, one keystroke, no reason),
   with the Process gate updated to treat Confirmed as resolved.

Everything else - docked detail pane, panel polish, coverage drawer, rule hub,
RuleAlternativesPanel relocation - layers on top without rework.

---

## 12. Open questions for Zach

Each of these is a product judgment, not a design detail I am ducking.

1. **Do majority conventions behind Deviation rows enter the adoptable convention set?**
   A rule with 45-of-47 conformance has derived a convention just as surely as a
   zero-violation rule; only `ConventionObserved` rows (zero violations) carry the kind
   today. My recommendation: yes - the Conventions panel lists them too, labeled "observed
   with N exceptions", adoptable once their deviations are dispositioned. But that widens
   A5.5's shipped meaning of "the derived decision set", so it is yours to call.
2. **What does Confirmed mean downstream?** I have designed it as report-export-readable
   local state (like FP) with no Process-time backend commit. If you want "confirmed"
   to leave a durable per-finding record beyond the draft and the export (for engagement
   history across scans), that is a backend surface Pass 2 should not invent silently.
3. **Is a NoConvention option pick a client-facing recommendation or an internal note?**
   If picking an option there feeds the derived decision set (AGENTS.md) as a
   "recommended going forward", it is a governance act and should say so loudly; if it is
   curator scratch state, it should ship nowhere. The two UIs differ (commit button and
   wording), so I need the intent before building either.
4. **Default-adopt for conventions: confirm the opt-out default.** I argued it from A5.5
   and volume; it does mean a convention you never looked at can land in a client's
   AGENTS.md behind one commit click. If that is too hot, the alternative is
   default-adopt only for rules whose option the audit evaluated with high confidence,
   opt-in for the rest - more honest, measurably slower.
5. **Single-key dispositions (C/I/T/F on the focused row): acceptable, or too easy to
   fat-finger at 1am?** Mitigations if wanted: keys act only with the detail pane focused,
   plus a 5-second undo toast. I would build the keys; your hands do this work, your call.

---

## Appendix: decision-flow summary, one line per kind

```
Defect              -> per-row:  Confirm | Ignore(reason) | TechDebt(later/now) | FalsePositive(reason)
Deviation           -> per-rule: Confirm all / FP all;  per-row: same five as Defect
ConventionObserved  -> per-rule: Adopt (default) | Skip      -> one "Adopt N" commit
NoConvention        -> per-rule: Recommend <option> | Leave as reported (default)
NotAssessed         -> none (read-only, grouped by structured reason)
unspecified         -> alarm surface; FP(reason) is the only exit; fix is in the pipeline
```

---

## 13. Zach's rulings on the open questions (2026-10-12)

**Q4 DECIDED — convention adoption is opt-out, and thoroughly gated.**
> "Yes it should be opt out by default, make sure it is thoroughly gated and not an accidental click."

Pre-checked by default. The commit dialog must name the destination repository, the adopted
count, and **how many were adopted without being opened**, and must require the repository name
typed rather than a checkbox. It offers three exits, not two: Cancel, Review the unopened set,
Commit. Opt-out is fast, so the gate has to be honest about its own cost.

**Q1 DECIDED, AGAINST MY DESIGN — a convention with live deviations IS adoptable.**
> "I could decide that 'this is the correct convention even though there are inconsistencies
> that exist'. The remediation of those inconsistencies is a separate process. But if I say
> 'this is the rule and convention' then I mean it, and that can be adopted."

I had designed adoption as blocked until the deviations were dispositioned. That was wrong, and
it inverts the same causality A5.2 already settled: **adopting a convention states what the
standard is going forward; whether the code complies today is separate work with a separate
owner.** Gating one on the other conflates "what is our rule" with "are we currently compliant."

It also fails worst where it matters most: the repo with the most drift is the one that most
needs the convention written down, and the blocked design was least able to write it.

So majority conventions behind Deviation rows DO enter the adoptable set, labelled "observed
with N exceptions", with a jump into the Queue. The count is context, never a gate.

**Four tabs stay.** I floated collapsing `No convention` and `Not assessed` into a shared
"Observations" panel to reduce the switcher to two primary surfaces. Zach: *"I think it is
fine as is."* Dropped; the kind switcher keeps one tab per kind.

### Q2, Q3, Q5 DECIDED (2026-10-12) -- Zach: "good to go with your recommendations"

- **Q2 `Confirmed` stays LOCAL for Pass 2.** Report-export-readable draft state, like False
  Positive, with no Process-time backend commit. A durable per-finding record across
  engagements is a real backend surface and Pass 2 must not invent it as a side effect of a
  UI build. Revisit if cross-scan engagement history is ever wanted.
- **Q3 a `NoConvention` option pick IS client-facing.** It feeds the derived decision set, so
  it is a governance act and must say so in the UI. It therefore **inherits Q4's gating**: the
  same typed-destination confirm, the same honest count of what was not opened.
- **Q5 single-key dispositions SHIP.** `C` / `I` / `T` / `F` on the focused row, scoped so the
  keys only fire while the detail pane holds focus, plus a five-second undo toast. At 400 rows
  the keystroke is the whole speed gain; the toast removes the 1am risk.

### Dependency check (2026-10-12): chorale 0.2.3 already has everything

> **RETRACTED, 2026-10-15. Both headline claims in this block are WRONG.** They were made by
> matching symbol names against the design's asks, not by reading the installed source. Steps
> 2 and 4 each discovered the error at build time, and each routed around it in Camerata. The
> corrected reading is recorded immediately below; the original text is kept so the mistake is
> legible rather than quietly overwritten.
>
> - **`detail_renderer` / `set_detail_column_enabled` / `detail_column_enabled` are NOT a
>   docked pane.** `detail_renderer` is an EXPANDABLE INLINE ROW DETAIL: `chorale-dioxus`
>   0.2.3 renders it in `detail_panel_tr` (`components.rs:1998`) as
>   `<tr class="chorale-detail-panel"><td colspan=N>…</td></tr>` immediately BENEATH its
>   parent row, with a chevron column to expand it. `set_detail_column_enabled` /
>   `detail_column_enabled` just keep that chevron column in chorale's own keyboard
>   navigation. An inline panel pushes every row below it down the page, so
>   cursor-down-read-decide would reflow the table under the cursor on every move — the
>   opposite of what §2 asks for. **Built in Camerata** (`FindingDetailPane`, step 4 below).
> - **`render_grouped_row` / `group_header_tr` / `group_header_class` are NOT a group-header
>   content hook.** The first two are private `fn`s in `chorale-dioxus` 0.2.3; the third is a
>   CSS class string. Step 2 found this and routed around it by grouping on a hidden column
>   whose cell value IS the header text.
>
> What chorale 0.2.3 genuinely does provide, and what step 4 used instead, is
> **`chorale_core::visible_grouped_view`** — public, and post-filter, post-sort AND
> post-collapse. Expressing row focus as a position in that list is what makes "focus survives
> a re-sort" and "focus survives a collapse" one rule rather than two features.
>
> The remaining two lines below (grouping/expand/collapse, selection) were checked and are
> correct.

The design named the group-header content hook as "the one chorale ask" and flagged
master/detail as needing verification. Both are present in the installed registry version
(`chorale-core` / `chorale-dioxus` `0.2.3`), so nothing needs requesting upstream and the
fallback-to-modal risk is closed:

- `detail_renderer`, `set_detail_column_enabled`, `detail_column_enabled` -- the docked pane
- `render_grouped_row`, `group_header_tr`, `group_header_class` -- custom group-header content
- `set_grouping`, `expand_all_groups`, `collapse_all_groups`, `group_selection_state`
- `select_all_filtered`, `select_all_visible_page`, `selection_count`, `set_selection`

### Nothing remains open

All five of section 12's questions are decided above. For the record, these were the last
three to close, each decided in line with the recommendation stated with it:

- **Q2** what `Confirmed` means downstream. Decided: local, export-readable draft state.
- **Q3** whether a `NoConvention` option pick is client-facing. Decided: client-facing, and
  it inherits Q4's gating.
- **Q5** single-key dispositions. Decided: build them, scoped to the focused detail pane,
  with a five-second undo toast.

Layout mockups for all four screens were produced and reviewed on 2026-10-12.

---

## 14. Build status (recorded 2026-10-14)

Established by reading the tree at `feat/audit-report-export`, not from the commit messages.
Every line reference is to the current tree.

### BUILT - section 11 step 1, in full

Commits `01f74a6`, `a26996a`, `62249c9`.

| Design item | Where it landed |
|---|---|
| `kind` mirrored into `FindingView` | `crates/ui-core/src/triage.rs:131-143`, `Option<FindingKind>` with `#[serde(default, flatten)]`. Accessors `kind_tag` `:211`, `is_unspecified_kind` `:217`, `unspecified_kind_count` `:222` |
| Kind badge column (section 4) | Column def `crates/ui/src/cockpit/scan.rs:1570-1581` (sortable, multi-select filter over `defect`/`deviation`/`unspecified`); chip renderer `:1997-2023`; styles `crates/ui/src/style.rs:2425-2431`, with the dashed red outline for `unspecified` the design asked for |
| Kind-aware default sort (section 5) | `queue_sort_key`, `crates/ui-core/src/triage.rs:417-428`, applied in `TriageModel::visible` `:528`. Order is exactly as designed: status, severity, kind rank (`:371-382`), ascending deviation ratio with unknown denominator last (`:394-405`), then rule/path/line |
| Three informational kinds routed out of the Queue (section 1) | `kind_surface` `crates/ui-core/src/triage.rs:196-206`; `KindSurface` `:154-164`. Panels: `ConventionsPanel` `scan.rs:3576-3609`, `NoConventionPanel` `:3618-3663`, `NotAssessedPanel` `:3671-3718` |
| The kind switcher (section 2) | Inline in `ScanResults`, `scan.rs:4913-4942`, driven by the `kind_view` signal at `:3988`. Not a named component |
| The `unspecified` alarm chip (section 3) | `scan.rs:4931-4937`, count at `:4079` |
| One-rule-one-surface invariant (section 8) | Beyond what step 1 scoped: `rules_on_multiple_surfaces` `triage.rs:264-278` plus `KindSurfaceConflictBanner` `scan.rs:3537-3562`, mounted at `:4893`. The design said the UI "should assert it rather than defend against it"; it does |

### BUILT - the manual-sort requirement (section 5's closing sentence)

Commits `fc621e2`, `a4a5612`, `c3f8330`. The design mentioned this only in passing ("the
default is opinionated, the table is not locked"); it shipped as its own increment, tagged
`req_pass2_8`, outside the section 11 step numbering.

- `QueueOrderBar` component `crates/ui/src/cockpit/scan.rs:1826-1852`, rendered above the
  table at `:2288-2292`.
- Nine sortable columns: repo, kind, severity, authority, type, needs_review, status, file,
  line. Snippet and bucket are deliberately not sortable.
- Visible indicator in two layers: chorale's own header arrows, plus the bar's sentence -
  `DEFAULT_ORDER_LABEL` (`:1744-1745`) when untouched, "Your sort: {Header} ..." when manual
  (`:1784-1787`).
- Explicit "Default order" control at `:1842-1849`, rendered ONLY in the manual state (its
  absence is itself the indicator), calling `handle.clear_sort()`.
- Calibrated severity sort value `severity_sort_value`
  (`crates/ui-core/src/triage.rs:343-345`), so a header sort on Severity orders
  critical/high/medium/low rather than alphabetically, while the rendered cell keeps the
  authored word.

### BUILT - a live bug this work exposed and fixed

The Process gate was counting informational rows. The disposition tally at
`crates/ui/src/cockpit/scan.rs:4082-4097` looped the RAW finding set; `a26996a` changed the
loop header at `:4088` to iterate `queue_findings` (defined `:4076`). Before that change,
every `ConventionObserved` / `NoConvention` / `NotAssessed` row counted as Unresolved
permanently, because those rows have no disposition and no triage table in which to acquire
one. The gate at `:5029` disables Process on `n_unresolved > 0`, so **a row saying "your
codebase does this correctly" blocked sign-off**, and the hint at `:5024-5025` told the
curator to resolve rows he had no way to resolve. The same narrowing was applied to the rows
Process commits (`findings_for_process`, `:5021`) and the block is additionally scoped to the
Queue surface (`:5016`).

Test coverage, with a caveat worth recording: `req_pass2_2_informational_rows_cannot_block_the_process_gate`
(`crates/ui-core/src/triage.rs:1235-1246`) covers the PURE derivation in ui-core. The Dioxus
`ScanResults` tally and gate condition are not themselves exercised, because `ScanResults`
sits behind `use_resource` network resources and is not SSR-testable; the render tests target
`FindingsTable` and the panels directly.

### BUILT - section 11 step 2

Commits `b35f144`, `d24a00d`. (This section read "NOT BUILT" until 2026-10-14; it was written
before those commits landed and was not revised with them.)

The deviation sentence is structured values in core (`DeviationHeadline` / `rule_group_label`
/ `deviation_groups`, `crates/ui-core/src/triage.rs`) and reaches the real group header by
being the value the table GROUPS BY (`RULE_GROUP_COLUMN`), so chorale renders it. The
rule-level bulk action is `DeviationBulkBar` above the table, not content inside the group
header `<tr>`: the 2026-10-12 dependency check recorded `render_grouped_row` /
`group_header_tr` / `group_header_class` as a content-injection hook and they are not one
(the first two are private in `chorale-dioxus` 0.2.3, the third is a CSS class string), which
is recorded at the component. `False positive all N` shipped there; `Confirm all N` was
deliberately withheld for want of an honest disposition, and step 3 below adds it.

Still open from section 9: `RuleAlternativesPanel` remains a standalone panel above the table,
so section 9.1's relocation has not happened.

### BUILT - section 11 step 3 (2026-10-14), except the keystroke

`TriageState` now has FIVE variants, `Confirmed` second (`crates/ui-core/src/triage.rs`),
and the disposition reaches the export rather than stopping at the UI.

| Design item | Where it landed |
|---|---|
| The fifth disposition | `TriageState::Confirmed` + `label()` + `TriageState::all()` (the single source of the sub-tab set and order) + `TriageState::is_decided()`, `crates/ui-core/src/triage.rs` |
| The per-row transition | `TriageModel::confirm(&[FindingView])` — no reason parameter, deliberately; a thin wrapper over `move_to` |
| `Confirm all N` (step 2's withheld half) | `TriageModel::bulk_confirm(&DeviationGroup)` (thin wrapper over `confirm`, refuses only an empty group) + the button in `DeviationBulkBar`, `crates/ui/src/cockpit/scan.rs` |
| The tally | `TriageCounts` struct replaces the 4-tuple; `ScanResults`' duplicate inline loop is gone, so there is now exactly ONE tally and it is the unit-tested one |
| The Process gate | `TriageCounts::outstanding()` / `has_committable_work()` / `can_process()`, rendered by the new props-only `ProcessGate` component. Confirmed is resolved, so it does not block; it commits nothing, so it does not unblock either, and the gate now SAYS so instead of leaving a disabled button unexplained |
| The export | `camerata_server::report_export::Disposition::Confirmed` — `ReviewState` flips to `Reviewed`, `matrix_bucket` shares `Unresolved`'s severity-derived arm, the row counts as still open, and `disposition_label` reads "Confirmed by the reviewer (recommended: X)". `xlsx_export`'s two counting sites and its legend follow, because the workbook must reconcile with the PDF |

Local only, per Q2. Nothing is committed at Process and there is no cross-scan record.

**The `C`/`I`/`T`/`F` keystrokes are DEFERRED to the docked-detail-pane step, deliberately.**
Q5 ships them "scoped so the keys only fire while the detail pane holds focus, plus a
five-second undo toast". The pane does not exist yet (the Queue's detail surface is still the
row-click modal), so the scoping condition the ruling attaches the keys to cannot be
satisfied. The only thing buildable today is a document-level handler firing on any `c`
anywhere on the page, which at 400 rows is the exact fat-finger risk the scoping clause
exists to prevent. The keys belong to the pane step, where their focus owner, the row cursor
and the undo toast all arrive together. The rationale is repeated at the Confirm button in
`scan.rs` so it is not rediscovered as an oversight.

Still not built from this step's neighbourhood: the undo toast (it arrives with the keys).

### BUILT - the DOCKED DETAIL PANE (2026-10-15), with the keystroke and the undo

Commits `9f19f30`, `6300913`, `6a8f2c2`, `97b93af`. Section 2's pane, section 6's
merged-site disclosure, and the Q5 ruling in full — the keys, their scoping, and the
five-second undo that step 3 deferred to this step.

| Design item | Where it landed |
|---|---|
| `also_locations` mirrored (section 10's undone half) | `MergedSiteView` + the `FindingView` field + `site_count` (`1 + also_locations.len()`, computed here per §10's own ruling), `crates/ui-core/src/triage.rs`. Round-trip contract tests, same as `kind`/`confidence`/`effort` |
| The docked pane | `FindingDetailPane` + `DetailPaneRow` + `PaneSiteRow` + `UndoBanner`, `crates/ui/src/cockpit/scan.rs`. Props-only, like `ProcessGate` and `DeviationBulkBar`, so SSR can render it in every state. Mounted in a `.queue-split` beside the table, sticky, with its own scroll context |
| Arrow-key row focus | `RowCursor` (core) over the keys of `chorale_core::visible_grouped_view`'s data rows. Pure and non-mutating — the cursor is RESOLVED during render against the current frame's row list, so resolution is a read and cannot loop |
| Single-key dispositions, SCOPED | `pane_command(focus, key, mods)` (core). The scope gate is the first thing it does and is unconditional: anything but `PaneFocus::Cursor` returns `None` for every key. `ctrl`/`alt`/`meta` disarm too (Cmd-C is copy over a snippet the pane is showing); shift does not |
| The armed indicator | `pane_keys_armed(focus)`, DEFINED as `pane_command(focus, "c", …).is_some()` rather than as a second read of the enum — so a pane claiming "armed" while the handler refuses is unreachable |
| `I` / `F` | `PaneCommand::PromptFor`, never `Dispose`. See the ruling below |
| The five-second undo | `UNDO_WINDOW_MS`, `UndoEntry`, `TriageModel::dispose_with_undo` / `undo` -> `UndoOutcome` (core). Timestamps are caller-supplied `u64` ms against a MONOTONIC `Instant`, so the window is tested without sleeping and an NTP step cannot move it |
| Section 9.6's removal | A Queue row click now FOCUSES the pane instead of opening the finding modal. The modal is one deliberate click away, from the pane's `Full detail` button; it keeps the on-demand enclosing-code-block fetch, far too expensive to re-run on every cursor-down. It never served the informational panels, so section 2's "the modal remains for the informational panels" is unaffected |
| Section 6's disclosure | The pane's `Sites (N)` list: primary first, then every absorbed site with its own original rule id and snippet, `consequence: true` entries subordinated under an `also affects` sub-label. Rendered only when N > 1 |

**The reason-carrying keys PROMPT, they do not apply.** `I` and `F` open the on-demand reason
prompt (pre-focused, Enter commits, Esc cancels) — which is what section 3 already specified
for the buttons. Apply-then-collect was rejected on a concrete failure rather than on taste:
it parks a row in `Ignored` with an empty reason for as long as the curator takes to type one,
and the curator's next gesture in this UI is cursor-down. A reasonless `Ignored` row is a
baseline waiver with no audit trail, which is the hole the require-reason invariant and
`bulk_false_positive`'s refusal both exist to close. Keys-only-for-the-reasonless was rejected
because it drops two of the four keys the ruling names. The prompt also COMPOSES with the
scoping instead of fighting it: opening it moves focus to `PaneFocus::ReasonPrompt`, so the
keys disarm for exactly as long as the curator is typing.

**Focus never asserts, it always moves.** Every path that wants the keys armed calls
`focus_detail_pane()` (a DOM focus against the shared `DETAIL_PANE_ID`) and lets the pane's
own `focusin` set the state. Writing `PaneFocus::Cursor` directly — which the first cut of the
row-click handler did — would let the pane report its keys armed while real focus sat in the
table and the keydown handler, which lives on the pane, received nothing.

**What happens to focus on a re-sort and on a collapse.** The cursor is anchored to the row's
`finding_key`, never to its index, with the index kept only as the fallback for when that key
is gone:

- **Re-sort** permutes the visible list without removing anything, so the SAME finding stays
  focused at its new position. An index-anchored cursor would silently re-point at whatever
  row landed under it, and the pane would start describing a different finding with no gesture
  from the curator — the worst failure for a surface whose whole job is "read this, press a
  key".
- **Collapse, filter, or the disposition itself** removes the focused row, and focus clamps to
  the row that took the vacated index (or the last row, when the tail went). For the
  disposition case that IS the burn-down gesture. Focus is never left on a row that is not on
  screen, and an emptied view clears it rather than keeping a stale key.

### BUILT - the RULE HUB (section 8), 2026-10-15

Commit `5029289`. `rule_hub` / `RuleHubView` / `ConformingSites` in core, `RuleHubStrip` /
`RuleHubModal` / `RuleHubRequest` in `scan.rs`. One join, scoped per `(repo, rule)`, reachable
from the three informational panels, the docked detail pane and the deviation bulk bar. The
conforming count is two integers with nothing to click, and the three paths section 8 refuses
are asserted absent.

### BUILT - the REMOVALS SWEEP (section 9), 2026-10-15

Commits `7e11ac8`, `06fb0b2`, `3eaac42`, `cc0b26c`, `3de74ef`, `3c832d4`, over `ed50b74`'s
`confidence_tag`. Taken in the order the risk allowed rather than the order of conviction, and
each one proved its replacement covered the affordance BEFORE deleting it.

| § | Item | Outcome |
|---|---|---|
| 9.1 | `RuleAlternativesPanel` as a standalone panel | **PARTLY REMOVED.** See the block below - the panel is still here, and the reason is a capability the hub provably cannot reach |
| 9.2 | The two always-visible reason textareas | **REMOVED.** `QueueReasonPrompt`, raised by the gesture, heading from core's `bulk_reason_heading` (names the disposition AND the count). The bulk-FP bar's reason source changed with it: it raised its own prompt instead of reading the shared textarea |
| 9.3 | The 300px "Needs review" prose column | **REMOVED.** Header now `Confidence`, 130px, cell value `confidence_tag`, filter `confidence_tags()`, chip by chorale badge. The hedging sentence relocated to the detail pane, asserted in both directions |
| 9.4 | Severity on informational surfaces | **ALREADY SATISFIED**, now PINNED with a `critical`/`high`/`medium` fixture that would leak one, across all three panels plus the hub strip |
| 9.5 | The findings-key legend | **REMOVED.** Superseded by the Severity + Authority badges, which also fix the legend's own false claim (it called the red row "deterministic security"; the predicate is `severity == "critical"` and always was). Red full-row highlight stays, now as the named `finding_row_class` so "it stays" is assertable |
| 9.6 | The row-click modal for Queue rows | **ALREADY REMOVED** by the detail-pane step; verified, not re-removed |
| 9.7 | "What's healthy" as a ledger-driven list | **LEFT ALONE**, per its own "eventually" and the open owner ruling it sits against |

**`split_needs_review` is NOT fully retired from the UI, and section 9.3's own condition says
why** - it asks for retirement "once the structured field is universal", and `confidence` is
`Option<String>` with legacy rows carrying only the prose marker. The line is drawn where the
principle is: prose must not DECIDE. Retired from the Queue column, where a regex drove a cell
value, a filter vocabulary and a sort key. Kept in `DetailPaneRow::from_finding` and the
finding modal, which only split already-written text so a human reader sees the marker in bold.
Deleting those two would drop a legacy row's only disclosure of its hedge.

### 9.1 IS BLOCKED, and the blocker is structural

The Evaluated-against line and the Why? ANSWER did move into `RuleHubStrip`, built by the same
pure `alternative_controls_row` the panel uses so the two cannot disagree, and read-only so
section 8's one-navigation invariant survives. The rest cannot follow:

- **The group headers take no content.** Already established in section 13's retraction.
  `render_grouped_row` / `group_header_tr` are private in `chorale-dioxus` 0.2.3 and
  `group_header_class` is a CSS class string. A `<select>` cannot be a grouped-on cell value.
- **The hub cannot reach a rule with no findings.** `rule_hub` returns `None` for it, by its
  own design ("there is no '0 findings' hub - an empty strip is a dead affordance"). But the
  panel exists to list every multi-option rule the audit evaluated *including those with zero
  violations*, and a clean rule is the one whose option choice is least visible. Deleting the
  panel would leave those rules with no Queue row, no panel row and no hub.

So the Change select, the staged-override map and the rescan/accept flow stay on the panel; the
sticky rescan bar survives as section 9.1 says it should; and the hub names the panel rather
than leaving a curator who just read "Override, pending rescan" to hunt for the control.
Finishing 9.1 needs a surface enumerating the rules the scan found nothing for - a new thing to
build, not a relocation, and section 9 does not describe one.
`req_pass2_13_the_hub_cannot_reach_a_clean_multi_option_rule_so_the_panel_stays` pins it.

### NOT BUILT - the later layers, as expected

Coverage drawer (section 7). Section 4's Sites COLUMN still does not exist (the data it needs
is mirrored; only the column is missing) and the Authority column has not been narrowed. The
convention commit dialog (Q4's typed-destination gate) is the next step.

### PARTIAL, and worth knowing before the next step starts

- ~~**Section 10's enabling work is half done.**~~ **CLOSED 2026-10-15** by the detail-pane
  step: `also_locations` is mirrored as `MergedSiteView` with round-trip contract tests, and
  `site_count` is computed in core. Section 6 is built; section 4's Sites COLUMN is still
  unbuilt, but is no longer blocked on data.
- **Section 4's column table is two rows done of four changed.** Kind badge shipped; the
  Confidence column replaced "Needs review" (9.3). The Sites column does not exist and the
  Authority column has not been narrowed.
