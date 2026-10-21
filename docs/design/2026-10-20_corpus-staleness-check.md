# Corpus staleness check (2026-10-20)

## Status

Design, not yet built. No urgency: every rule was authored in spring 2026 or later
(the Supabase pack within the last few months), so nothing is stale today. This exists
to PREPARE - a sweep before the first client, and a recurring sweep thereafter - and
to give Camerata a way to nag me when the corpus is overdue for one.

## The problem

Camerata's corpus (~417 rules) encodes "what good looks like" for a stack. "Good"
drifts: an ecosystem moves off a pattern (class components, a deprecated serializer, a
superseded API shape), and a rule that still teaches the old way silently becomes wrong
guidance. Nothing in the system notices the clock. The risk is low now and grows with
every month the corpus sits untouched while the outside world moves.

Two needs:

1. **A sweep** - a periodic pass that checks each rule's described pattern against
   current ecosystem guidance and surfaces candidates that may have aged out.
2. **A staleness nag** - Camerata tracks when the corpus was last confirmed current and
   warns me when that was too long ago, so a sweep never silently lapses.

## The load-bearing distinction: swept vs confirmed

The watermark does NOT advance when a sweep RUNS. It advances when I REVIEW the sweep's
findings and confirm I am confident the corpus is current. A sweep produces candidates;
confirmation is a human act; the watermark records the confirmation.

This matters because an automated "sweep ran, stamp today" would mark the corpus fresh
on the strength of a pass I never looked at - exactly the false-confidence the feature
exists to prevent. Running the sweep and advancing the watermark are two separate acts,
same as "shipping code" and "changing infrastructure" are kept separate elsewhere in
this repo.

## Where the watermark lives

A version-controlled file beside the corpus:

```
crates/rules/CORPUS_FRESHNESS.toml
```

```toml
# When each rule domain was last SWEPT and CONFIRMED current against the ecosystem.
# Advanced only by a human confirmation after reviewing a sweep (see the staleness
# design doc). Running a sweep does NOT touch this file.

warn_after_days = 90   # quarterly default; override per domain below if needed

[confirmed]
# domain = "YYYY-MM-DD"  — the date the domain was last confirmed current
universal  = "2026-10-20"
supabase   = "2026-08-15"
javascript = "2026-05-01"
# ... one line per domain/pack
```

Rationale for this location:

- It travels WITH the corpus. The thing whose freshness we track and the freshness
  record are in one repo, one commit history.
- Every confirmation becomes a git commit - a free, auditable "who confirmed what,
  when" trail, no database needed.
- Camerata already loads the corpus from `crates/rules/principles/`
  (`camerata_rules::corpus_path`); reading a sibling file is trivial and needs no new
  storage.

**Granularity: per-domain, not one corpus-level date.** Ecosystem churn is wildly
uneven - a JavaScript/React pattern ages in a year, a SQL or security-primitive rule in
a decade. A single corpus-level watermark would either nag constantly (paced to the
fastest-moving pack) or lull (paced to the slowest). Per-domain lets the JS pack demand
a quarterly look while `universal`/`sec` ride a yearly one. MVP can start with a single
`default` entry and split per-domain when the first sweep shows which packs move.

## The staleness nag

On corpus load (and as a line in the daily-digest / morning-consolidator routine),
Camerata computes `today - confirmed[domain]` for each domain and, for any that exceed
that domain's `warn_after_days`, emits a warning:

```
CORPUS STALENESS: javascript last confirmed 2026-05-01 (172 days ago, threshold 90).
Due for a staleness sweep.
```

Surfaced in two places:

- A non-fatal banner when the engine loads the corpus (visible to me, never in a client
  artifact).
- A line in the morning digest, so an overdue pack reaches me even when I am not running
  a scan.

It never blocks a scan or an export. Stale guidance is a quality risk to flag, not a
hard gate - and a client engagement must not be stopped because a pack is a week past
its review window.

## The sweep itself

An LLM-driven research pass, one agent per domain/pack (the orchestrator's existing
fan-out). For each rule it takes the rule's `title`, the option `directive`s, and the
`[[sources]]`, and asks: *is this still current best practice as of today, or has the
ecosystem moved?* It returns, per rule, one of:

- CURRENT - no change, cite why it is still the consensus.
- SHIFTED - the ecosystem moved; what replaced it, since when, with a current-guidance
  source.
- UNCERTAIN - route to me; the research was inconclusive.

Output is a review queue (raw, over-inclusive, erring toward showing me more - matching
the loop-grades-the-raw-product philosophy), never an auto-edit of the corpus.

## The confirm-then-watermark flow

1. Run the sweep for a domain (or all). It writes a findings report, touches nothing
   else.
2. I review. For each SHIFTED finding I either:
   - agree it is stale -> author `superseded_since` + `current_recommendation` on that
     rule (the W32 part (b) schema fields - this sweep is precisely the process that
     ever sets them), or revise/retire the rule; or
   - disagree -> leave it, note why.
3. When I am confident the domain is current, I advance its watermark - a single
   command / UI action that sets `confirmed[domain] = today` and commits
   `CORPUS_FRESHNESS.toml`. This is the ONLY thing that advances the watermark.

So W32 part (b), dropped as a standalone chore, re-enters here as the natural output of
a confirmed sweep finding, not a field authored speculatively.

## Recurrence

- **Before the first client: a full sweep of every domain, all confirmed**, so the
  first paid deliverable rests on a corpus I have just verified is current. This is a
  gated pre-client action, tracked in memory.
- **Thereafter: a recurring routine**, monthly or quarterly (quarterly is the default
  `warn_after_days`), that runs the sweep and drops its queue into my review inbox. The
  nag is the backstop if the routine ever lapses.

## Build order (when it is scheduled - after W27)

1. `CORPUS_FRESHNESS.toml` + a `camerata_rules` reader (parse, compute per-domain age).
2. The nag: a staleness check wired into corpus load + the digest routine.
3. The confirm command/action that advances a domain's watermark and commits the file.
4. The sweep agent (reuses the orchestrator fan-out), writing a review queue.
5. The recurring routine that runs step 4 on a schedule.

Steps 1-3 are small and self-contained (the mechanism). Step 4-5 are the heavier agent
work and can follow.

## Open questions for Zach

1. **Default threshold: quarterly (90d)?** Or monthly for fast packs (js/frameworks)
   and quarterly/yearly for slow ones (sec/universal)? Recommendation: 90d default,
   with per-domain overrides authored as the first sweep reveals which packs move.
2. **Where does the recurring sweep run** - a cloud routine (like the existing nightly
   ones) or a local on-demand command I invoke? Recommendation: on-demand to start
   (I control when I have review bandwidth), promote to a scheduled routine once the
   output quality is proven.

## Non-goals

- The sweep never auto-edits the corpus. It produces a review queue; I author the
  changes and advance the watermark.
- The nag never blocks a scan or export.
- Not a per-RULE timestamp. Freshness is tracked per domain; a single rule's currency
  is established by the sweep's finding, not a date field on every rule.
