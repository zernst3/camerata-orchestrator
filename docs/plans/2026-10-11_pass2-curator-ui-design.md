# Pass 2 - Curator UI design for the five-kind finding taxonomy (2026-10-11)

Design document only. No code in this pass. Builds on:

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
