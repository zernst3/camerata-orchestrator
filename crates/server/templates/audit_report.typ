#let d = json("data.json")

// M6: an absent-or-empty option string both render as the placeholder — a client who
// exports with no options modal input must never see a blank cover field.
// No em/en dashes in the placeholder itself (house no-dash rule) — "N/A", not "\u{2014}".
#let or_na(x) = if x == none or x == "" { "N/A" } else { x }

#let plural(n, singular, plural_form) = if n == 1 { singular } else { plural_form }

// S13: thousands separators so "486203 characters" on the cover reads as "486,203".
#let thousands(n) = {
  let s = str(n)
  let len = s.len()
  let parts = ()
  let i = len
  while i > 3 {
    parts.push(s.slice(i - 3, i))
    i -= 3
  }
  parts.push(s.slice(0, i))
  parts.rev().join(",")
}

// M8: a long unbroken raw string (a deep node_modules path, a one-line minified snippet, a
// scoped package name) has no natural break character, and Typst's raw/monospace rendering
// does not wrap inside such a run — it overflows the page edge. Insert an invisible
// zero-width-space break opportunity every `n` characters so it wraps instead, with zero
// visual change to the text itself.
#let breakable(s, n: 40) = {
  let len = s.len()
  let i = 0
  let out = ()
  while i < len {
    let e = calc.min(i + n, len)
    out.push(s.slice(i, e))
    i = e
  }
  out.join("\u{200B}")
}

// Branding (2026-09-13 review, owner's hard constraint): Camerata is the licensable
// APPLICATION; the auditing agency running it is not Camerata's to name. `d.cover.brand` is
// already fully resolved (per-report ReportOptions.brand > CAMERATA_REPORT_BRAND env var > no
// brand at all) by `report_export::build_report_json` — this template just renders it, never
// invents a fallback agency name. Hyphen, not an em/en dash (house no-dash rule, see `or_na`
// above) — "Cantus Works - Codebase Audit", not "Cantus Works \u{2014} Codebase Audit".
#let brand_title = if d.cover.brand != none {
  d.cover.brand + " - Codebase Audit"
} else {
  "Codebase Audit Report"
}

#let doc_title = if d.cover.project_title != "" {
  d.cover.project_title + ", " + brand_title
} else {
  brand_title
}
#set document(title: doc_title)
#set page(
  paper: "us-letter",
  margin: (x: 2.2cm, y: 2cm),
  numbering: "1",
  footer: context [
    #set text(size: 8pt, fill: rgb("#808080"))
    #align(center)[
      #if d.cover.brand != none [#d.cover.brand ]audit report (advisory, not a certification), page #counter(page).display()
    ]
  ],
)
#set text(size: 10.5pt)
#set heading(numbering: none)
#show heading.where(level: 1): it => {
  v(0.6cm)
  block(text(size: 15pt, weight: "bold", it.body))
  v(0.2cm)
  line(length: 100%, stroke: 0.5pt + rgb("#cccccc"))
  v(0.3cm)
}
// M8: raw text (paths, shas, snippets) rendered a touch smaller than body copy, and never
// justified (justification widens the invisible breakable() gaps unevenly).
#show raw: set text(size: 8.5pt)
#show raw.where(block: true): set par(justify: false)

#let chip(label, kind) = {
  let bg = if kind == "clean" { rgb("#e6f4ea") } else if kind == "attention" { rgb("#fff4e0") } else { rgb("#fdeaea") }
  let fg = if kind == "clean" { rgb("#1e7d34") } else if kind == "attention" { rgb("#b06a00") } else { rgb("#c0392b") }
  box(fill: bg, inset: (x: 6pt, y: 3pt), radius: 3pt, [#text(fill: fg, size: 8.5pt, weight: "bold")[#label]])
}

#let status_kind(status) = {
  if status == "Clean" { "clean" } else if status == "Attention" { "attention" } else { "action" }
}

// S2: severity as a colored chip, not the lightest gray ink on the page — a "critical"
// finding's severity word must be the most visible thing in its cell, not the least.
#let severity_chip(sev) = {
  let bg = if sev == "critical" { rgb("#fdeaea") } else if sev == "high" { rgb("#fff0e6") } else if sev == "medium" { rgb("#fff4e0") } else { rgb("#eeeeee") }
  let fg = if sev == "critical" { rgb("#c0392b") } else if sev == "high" { rgb("#c0392b") } else if sev == "medium" { rgb("#b06a00") } else { rgb("#555555") }
  box(fill: bg, inset: (x: 5pt, y: 2pt), radius: 3pt, [#text(fill: fg, size: 8pt, weight: "bold")[#upper(sev)]])
}

// Nice-to-have: effort/confidence as small neutral chips instead of plain inline text.
#let mini_chip(label) = box(fill: rgb("#eeeeee"), inset: (x: 5pt, y: 2pt), radius: 2pt, [#text(size: 8pt, fill: rgb("#444444"))[#label]])

// Item 6: the category scorecard as a compact heat-grid. Native Typst primitives only (no
// packages) — a small, hand-picked light-to-dark palette per severity, indexed by count, so a
// "critical" cell with 4 findings reads visibly heavier than one with 1. The owner's original
// ruling made this the ONE approved visual addition and kept the severity x effort section a
// plain bucketed list. FIX 6 (2026-09-13 review) revisited that second half: the bucketed list
// duplicated page 3's "Three things this week" box without ever delivering the 2-D placement
// its own heading promised, so the owner approved replacing it with a real severity-rows x
// effort-columns `table()` (see `d.priority_grid` below) — still a plain Typst table, not a
// new chart primitive. No OTHER decorative visual gets added beyond this heat-grid and that
// table: restrained, engineer-made, not marketing.
#let heat_bg(sev, n) = {
  let palette = if sev == "critical" {
    (rgb("#fdeaea"), rgb("#f8b8b0"), rgb("#f2887a"), rgb("#e2574a"), rgb("#c0392b"))
  } else if sev == "high" {
    (rgb("#fff0e6"), rgb("#ffd2ab"), rgb("#ffb570"), rgb("#f2953f"), rgb("#d9720d"))
  } else if sev == "medium" {
    (rgb("#fff9e6"), rgb("#ffedb0"), rgb("#ffe07a"), rgb("#f7cf4a"), rgb("#e0b400"))
  } else {
    (rgb("#f7f7f7"), rgb("#eaeaea"), rgb("#d8d8d8"), rgb("#c2c2c2"), rgb("#a8a8a8"))
  }
  let idx = if n == 0 { 0 } else if n == 1 { 1 } else if n <= 3 { 2 } else if n <= 6 { 3 } else { 4 }
  palette.at(idx)
}

#let heat_cell(n, sev) = {
  let idx = if n == 0 { 0 } else if n == 1 { 1 } else if n <= 3 { 2 } else if n <= 6 { 3 } else { 4 }
  let fg = if idx >= 3 { white } else { rgb("#333333") }
  align(center)[
    #box(fill: heat_bg(sev, n), inset: (x: 6pt, y: 4pt), radius: 2pt, width: 1.7cm)[
      #align(center)[#text(fill: fg, weight: if n > 0 { "bold" } else { "regular" })[#str(n)]]
    ]
  ]
}

// One curated-finding site row — factored out so the FIRST site of a group can be kept
// together with its heading (S4) while later sites are free to break across pages.
//
// FIX 8 (2026-09-13 review): "the plain-English headline leads; the engineer-normative rule
// title renders smaller/grey BENEATH it." `site.headline` was already the primary bold
// heading (Item 1, above) — but for a group's FIRST site, the once-per-group `rule_id: title`
// subtitle used to render ABOVE it (see the curated-findings loop below), so the reading order
// put the engineer-facing rule id first after all. `after_headline` lets the caller inject
// that subtitle (plus the citation) directly UNDER this site's headline instead, so it always
// reads headline-first regardless of which site in the group is rendering.
#let render_site(site, after_headline: none) = {
  block(inset: (left: 8pt, top: 4pt, bottom: 4pt))[
    // Item 1: the DEFECT at this object is the primary, bold heading — never the rule's own
    // invariant title (that reads as a clean bill of health out of context).
    #text(size: 11pt, weight: "bold")[#site.headline]
    #if after_headline != none [
      #v(2pt)
      #after_headline
    ]
    #v(2pt)
    #text(size: 8.5pt, fill: rgb("#666666"))[*#site.repo* / #raw(breakable(site.path)):#str(site.line)] #severity_chip(site.severity)
    #if site.snippet != "" [
      #block(fill: rgb("#f5f5f5"), inset: 5pt, radius: 2pt, width: 100%)[#raw(breakable(site.snippet, n: 70))]
    ]
    #site.detail
    // FIX 1 contract (report_export.rs's `CuratedSiteJson.fix`): `Option<String>` serializes
    // to JSON `null`, which Typst's `json()` parses as `none` — NOT an empty string. The
    // check must be `!= none`; a `!= ""` check (the earlier bug here) would let a `none` value
    // fall into the branch and render a bare "Fix:" label with nothing after it. `none` means
    // no remediation is authored for this rule yet: render NOTHING, not an empty Fix block.
    #if site.fix != none [
      #v(0.1cm)
      #text(size: 8.5pt)[#text(weight: "bold")[Fix: ]#site.fix]
      // `fix_for_this_finding` is always `none` as of this pass (see the field's own doc
      // comment in report_export.rs) but the branch is wired so a future finding-specific
      // remediation sentence renders correctly without touching this template again: a
      // SINGLE labeled line UNDER the authored Fix block, never in place of it.
      #if site.fix_for_this_finding != none [
        #v(0.05cm)
        #text(size: 8.5pt)[#text(weight: "bold")[For this finding: ]#site.fix_for_this_finding]
      ]
    ]
    #v(0.1cm)
    #text(size: 8.5pt)[
      #mini_chip("Effort: " + or_na(site.effort)) #mini_chip("Confidence: " + or_na(site.confidence)) #text(weight: "bold")[#site.disposition]
    ]
    #if site.also_matches.len() > 0 [
      #text(size: 8pt, fill: rgb("#888888"))[Also violates: #site.also_matches.join(", ")]
    ]
  ]
}

// ── 1. Cover ─────────────────────────────────────────────────────────────
// Branding (2026-09-13 review): the cover title is `brand_title` (computed above from the
// resolved `d.cover.brand`, never a hardcoded agency or "Camerata" literal), and the old
// scan-category qualifier ("Brown" + "field") is gone from it entirely. The two model-name
// rows that used to sit in this table (which model audited, which model calibrated) plus the
// Camerata-version row have moved off the cover into Methodology further down (see "Scanned
// with Camerata v..." there): Camerata is the auditing INSTRUMENT, legitimately named where
// methodology is discussed, not on the client-facing cover.
#align(center)[
  #v(2cm)
  #text(size: 22pt, weight: "bold")[#brand_title]
  #if d.cover.project_title != "" [
    #v(0.4cm)
    #text(size: 14pt)[#d.cover.project_title]
  ]
  #v(0.2cm)
  #if d.cover.client_name != "" [#text(size: 12pt, fill: rgb("#666666"))[Prepared for #d.cover.client_name]]
  #v(1cm)
]

#table(
  columns: (auto, 1fr),
  stroke: none,
  inset: 4pt,
  [*Repos audited*], [#d.cover.repos.join(", ")],
  [*Files scanned*], [#thousands(d.cover.files_scanned) (#thousands(d.cover.files_excluded) excluded as noise)],
  [*Code volume*], [#thousands(d.cover.code_chars) characters],
  [*Generated*], [#d.cover.generated_at],
  [*Prepared by*], [#or_na(d.cover.prepared_by)],
)

// Nice-to-have: a cover stat strip so the story starts on page 1, not page 2.
#v(0.2cm)
#text(size: 9.5pt, fill: rgb("#444444"))[
  #d.cover.stats.critical critical · #d.cover.stats.high high · #d.cover.stats.medium medium · #d.cover.stats.accepted accepted · #d.cover.stats.dependency_advisories #plural(d.cover.stats.dependency_advisories, "dependency advisory", "dependency advisories")
]

// Nice-to-have: gate the heading when there is nothing under it.
#if d.cover.audited_refs.len() > 0 [
  #v(0.4cm)
  #text(weight: "bold")[Audited git state]
  #for r in d.cover.audited_refs [
    #block(above: 2pt, below: 2pt)[
      - *#r.repo*: #if r.sha == none and r.branch == none [
          (no git metadata available)
        ] else [
          #if r.sha != none [commit #raw(r.short_sha)] else [no commit recorded], #if r.branch != none [branch #raw(r.branch)] else [unknown branch]
        ]#if r.dirty [ (working tree had uncommitted changes at scan time)]
    ]
  ]
]

#pagebreak()

// ── ToC (nice-to-have): one line of Typst, clickable bookmarks in every viewer ───────
#outline(title: [Contents], depth: 1)

#pagebreak()

// ── 2. Executive summary ──────────────────────────────────────────────────
= Executive summary

#d.executive_summary.narrative

#if d.executive_summary.is_override [
  #text(size: 8.5pt, style: "italic", fill: rgb("#666666"))[Summary text supplied by the auditor.]
]

// FIX 4 (2026-09-13 review, "page 3 states the top-3 three times"): the executive summary used
// to ALSO restate the top do-now findings as its own bulleted "priority items" list right
// here — on top of the narrative's own blast-radius lead sentence (dropped, see
// `report_export::default_narrative`'s doc comment) and the "Three things this week" box
// immediately below. The exec summary is now curation + counts only; the box below is the ONE
// place that names the top items.

// ── Item 7: "If you only do three things this week" ───────────────────────
// A buyer pricing remediation reads "these two criticals are about four hours of work total"
// as the sentence that converts anxiety into a purchase order — a half-page box, not a whole
// new page, right after the executive summary.
= If you only do three things this week

#if d.three_things.items.len() == 0 [
  No do-now items this run.
] else [
  #block(stroke: 0.6pt + rgb("#c0392b"), inset: 10pt, radius: 3pt)[
    #for item in d.three_things.items [
      #block(above: 4pt, below: 8pt)[
        #severity_chip(item.severity) *#item.headline*
        #text(size: 8.5pt, fill: rgb("#666666"))[
          #item.repo / #raw(breakable(item.path)):#str(item.line) (#item.rule_id)
        ]
        #v(1pt)
        #text(size: 9pt)[Rough estimate: #item.hours_label]
        // FIX 7 (2026-09-13 review): one factual sentence of what the exposure MEANS, when
        // one is authored for this rule (`report_export::business_impact_for_rule`) — omitted
        // gracefully (no blank line, no "impact unknown" filler) when it isn't.
        #if item.impact != none [
          #v(1pt)
          #text(size: 8.5pt, style: "italic", fill: rgb("#555555"))[#item.impact]
        ]
      ]
    ]
    #line(length: 100%, stroke: 0.4pt + rgb("#dddddd"))
    #v(4pt)
    #text(weight: "bold")[#d.three_things.total_hours_label]
  ]
]

#pagebreak()

// ── 3. Category scorecard ─────────────────────────────────────────────────
= Category scorecard

#if d.scorecard.rows.len() == 0 [
  No findings were produced by any audited category.
] else [
  #table(
    // Item 6: the critical/high/medium/low columns get a FIXED width (not "auto") because
    // heat_cell's box is itself fixed-width — an "auto" column sized against a 100%-relative
    // child creates a circular sizing problem that corrupts the whole table's row layout.
    // FIX 5 (2026-09-13 review): the "checked/clean" column is now spelled out in words
    // ("N checked, M clean") rather than a bare "N/M" fraction that reads as a grade — widened
    // from `auto` to a fixed width to keep that longer text from cramping the table.
    columns: (1.3fr, 2.1cm, 2.1cm, 2.1cm, 2.1cm, 2.9cm, 1.1fr),
    stroke: 0.5pt + rgb("#dddddd"),
    inset: 5pt,
    [*Category*], [*Critical*], [*High*], [*Medium*], [*Low*], [*Rules checked · Rules clean*], [*Status*],
    ..d.scorecard.rows.map(row => (
      [#row.category],
      heat_cell(row.critical, "critical"),
      heat_cell(row.high, "high"),
      heat_cell(row.medium, "medium"),
      heat_cell(row.low, "low"),
      [#str(row.audited_rules) checked · #str(row.clean_rules) clean],
      [#chip(row.status, status_kind(row.status))],
    )).flatten()
  )
]

#pagebreak()

// ── 4. Severity × effort matrix ───────────────────────────────────────────
// FIX 6 (2026-09-13 review, "build the real 2-D grid"): this used to be 4 unrelated boxes
// (Do now / Do next / Plan / Accepted) under a heading that promised a 2-D placement it never
// delivered — and it duplicated page 3's "Three things this week" box. `d.priority_grid`
// (`report_export::build_priority_grid`) is the ACTUAL severity-rows x effort-columns grid:
// each open finding sits in exactly one cell. Accepted and Informational findings are
// deliberately out of scope (a footnote, not a row/column) — see the struct's doc comment.
= Severity × effort matrix

#let effort_col_label(key) = {
  if key == "low" { "Low effort" }
  else if key == "medium" { "Medium effort" }
  else if key == "high" { "High effort" }
  else { "Not yet estimated" }
}

#let grid_cell_content(cell) = {
  if cell.findings.len() == 0 [
    #text(fill: rgb("#bbbbbb"), size: 8pt)[None]
  ] else [
    #for f in cell.findings [
      #block(above: 2pt, below: 4pt)[
        #text(size: 8pt)[#f.headline]
        #linebreak()
        #text(size: 7.5pt, fill: rgb("#888888"))[#f.repo / #raw(breakable(f.path, n: 24)):#str(f.line) (#f.rule_id)]
      ]
    ]
  ]
}

#if d.priority_grid.rows.len() == 0 [
  No do-now, do-next, or plan findings this run.
] else [
  #table(
    columns: (auto,) + d.priority_grid.columns.map(c => 1fr),
    stroke: 0.5pt + rgb("#dddddd"),
    inset: 6pt,
    [*Severity*],
    ..d.priority_grid.columns.map(c => [*#effort_col_label(c)*]),
    ..d.priority_grid.rows.map(row => (
      [#severity_chip(row.severity)],
      ..row.cells.map(cell => grid_cell_content(cell)),
    )).flatten()
  )
]

#if d.priority_grid.accepted_count > 0 or d.priority_grid.informational_count > 0 [
  #v(0.3cm)
  #text(size: 8.5pt, fill: rgb("#888888"))[
    #if d.priority_grid.accepted_count > 0 [
      #str(d.priority_grid.accepted_count) #plural(d.priority_grid.accepted_count, "finding is", "findings are") accepted risk and out of scope for this grid (see Curated findings).
    ]
    #if d.priority_grid.informational_count > 0 [
      #str(d.priority_grid.informational_count) further #plural(d.priority_grid.informational_count, "item is", "items are") informational and out of scope for this grid.
    ]
  ]
]

#pagebreak()

// ── 5. Curated findings ───────────────────────────────────────────────────
= Curated findings

#if d.curated_findings.len() == 0 [
  No open code findings survived triage.
] else [
  #for group in d.curated_findings [
    // S4: keep the group heading + citation + FIRST site together (orphan control) — later
    // sites in the same group are free to break across pages normally.
    //
    // FIX 8 (2026-09-13 review): the BOLD, primary heading for each finding is the per-site
    // defect headline (`render_site`'s own `site.headline`, Item 1) and now LEADS the block
    // outright — the rule id + its own invariant title, plus the citation, render as a small
    // gray SUBTITLE directly BENEATH that headline (via `render_site`'s `after_headline`),
    // kept for registry traceability, never as the thing a reader sees first.
    #block(breakable: false)[
      #block(above: 12pt, below: 2pt)[
        #if group.sites.len() > 0 [
          #render_site(group.sites.at(0), after_headline: [
            #text(size: 8.5pt, fill: rgb("#888888"))[#group.rule_id: #group.title] #text(size: 8.5pt, fill: rgb("#888888"))[(#str(group.sites.len()) #plural(group.sites.len(), "site", "sites"))]
            // Item 5: show the citation ONCE. When real external sources exist, the bulleted
            // title+URL list IS the citation — the run-on `citation.label` (a redundant join
            // of those same titles) is dropped. `label` is shown only when there are no
            // external sources to bullet (the advisory/preview case), where it is the sole
            // honesty note.
            #if group.citation.sources.len() > 0 [
              #for s in group.citation.sources [
                #text(size: 8.5pt, fill: rgb("#555555"))[- #s.title #if s.url != "" [(#s.url)]]
              ]
            ] else [
              #text(size: 8.5pt, fill: rgb("#555555"))[#group.citation.label]
            ]
          ])
        ]
      ]
    ]
    #for site in group.sites.slice(1) [
      #render_site(site)
    ]
  ]
]

#pagebreak()

// ── 6 + 7. What's healthy + Dependency snapshot (S1: share a page — each was ~85-90% ──
// blank on its own) ─────────────────────────────────────────────────────────────────
= What's healthy

#if d.whats_healthy.rules.len() == 0 and not d.whats_healthy.dependency_clean [
  No zero-finding rules to report this run.
] else [
  #if d.whats_healthy.dependency_clean [
    - No known vulnerable dependencies detected in this scan.
  ]
  #for r in d.whats_healthy.rules [
    - *#r.rule_id*: #r.title #if r.citation.kind == "grounded" [#text(fill: rgb("#888888"), size: 8.5pt)[(#r.citation.label)]]
  ]
  #if d.whats_healthy.further_clean_count > 0 [
    #text(size: 8.5pt, fill: rgb("#888888"))[#str(d.whats_healthy.further_clean_count) further #plural(d.whats_healthy.further_clean_count, "rule", "rules") verified clean this run.]
  ]
  #text(size: 8.5pt, style: "italic", fill: rgb("#666666"))[
    Verified absent in THIS scan, not a guarantee against future regressions.
  ]
]

= Dependency & CVE snapshot

#if d.dependency_snapshot.clean [
  No known vulnerable dependencies detected.
] else [
  #table(
    columns: (1fr, auto, 2fr, auto),
    stroke: 0.5pt + rgb("#dddddd"),
    inset: 5pt,
    [*Package*], [*Repo*], [*Advisory*], [*Severity*],
    ..d.dependency_snapshot.rows.map(row => (
      [#raw(breakable(row.package, n: 24))],
      [#row.repo],
      [#row.advisory],
      [#severity_chip(row.severity)],
    )).flatten()
  )
]

#for note in d.dependency_snapshot.coverage_notes [
  #block(above: 4pt, below: 4pt)[
    #text(size: 8.5pt, fill: rgb("#888888"))[Coverage note: #note]
  ]
]

#pagebreak()

// ── Next steps (FIX 3, 2026-09-13 review) ───────────────────────────────────────────
// A short, FACTUAL "what happens after this report" paragraph, opening the ladder before the
// reader gets to Methodology's more technical detail. `d.methodology.next_steps` is authored
// prose (`report_export::NEXT_STEPS_NOTE`), never an LLM call, same pattern as the
// deterministic/ai-tier notes below.
= Next steps

#d.methodology.next_steps

// ── 8 + 9. Methodology & limitations + Disclaimer (S1: share a page) ────────────────
= Methodology & limitations

#d.methodology.deterministic_note

#d.methodology.ai_tier_note

// Branding (2026-09-13 review): Audit model / Calibration model / Camerata version used to sit
// on the cover; they now render here instead. Camerata is the auditing INSTRUMENT, legitimately
// named where methodology is discussed, never on the client-facing cover (see `brand_title`
// above) and never a substitute for the agency's own resolved brand.
Scanned with Camerata v#or_na(d.cover.camerata_version). Audit model: #or_na(d.cover.audit_model). Calibration model: #or_na(d.cover.calibration_model).

*#str(d.methodology.candidates_reviewed) candidate #plural(d.methodology.candidates_reviewed, "finding", "findings") reviewed; #str(d.methodology.excluded_false_positive) dispositioned as false positives by the auditor and excluded.*

#d.methodology.severity_scale_note

Not performed in this engagement:
#for item in d.methodology.not_done [
  - #item
]

= Disclaimer

#block(stroke: 0.5pt + rgb("#cccccc"), inset: 10pt, radius: 2pt)[
  #d.disclaimer
]
