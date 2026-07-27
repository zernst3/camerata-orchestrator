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

#let doc_title = if d.cover.project_title != "" {
  d.cover.project_title + ", Camerata Audit Report"
} else {
  "Camerata Audit Report"
}
#set document(title: doc_title)
#set page(
  paper: "us-letter",
  margin: (x: 2.2cm, y: 2cm),
  numbering: "1",
  footer: context [
    #set text(size: 8pt, fill: rgb("#808080"))
    #align(center)[Camerata audit report (advisory, not a certification), page #counter(page).display()]
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
// "critical" cell with 4 findings reads visibly heavier than one with 1. The owner's ruling:
// this is the ONE approved visual addition; the severity x effort matrix stays a plain
// bucketed list (that IS the chart), and no other decorative visual gets added here beyond
// this heat-grid — restrained, engineer-made, not marketing.
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
#let render_site(site) = {
  block(inset: (left: 8pt, top: 4pt, bottom: 4pt))[
    // Item 1: the DEFECT at this object is the primary, bold heading — never the rule's own
    // invariant title (that reads as a clean bill of health out of context).
    #text(size: 11pt, weight: "bold")[#site.headline]
    #v(2pt)
    #text(size: 8.5pt, fill: rgb("#666666"))[*#site.repo* / #raw(breakable(site.path)):#str(site.line)] #severity_chip(site.severity)
    #if site.snippet != "" [
      #block(fill: rgb("#f5f5f5"), inset: 5pt, radius: 2pt, width: 100%)[#raw(breakable(site.snippet, n: 70))]
    ]
    #site.detail
    #if site.fix != "" [
      #v(0.1cm)
      #text(size: 8.5pt)[#text(weight: "bold")[Fix: ]#site.fix]
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
#align(center)[
  #v(2cm)
  #text(size: 22pt, weight: "bold")[Camerata Brownfield Audit Report]
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
  [*Camerata version*], [#or_na(d.cover.camerata_version)],
  [*Audit model*], [#or_na(d.cover.audit_model)],
  [*Calibration model*], [#or_na(d.cover.calibration_model)],
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

#if d.executive_summary.top_do_now.len() > 0 [
  #v(0.3cm)
  #text(weight: "bold")[Top priority items]
  #for line in d.executive_summary.top_do_now [
    - #line
  ]
]

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
    columns: (1.4fr, 2.1cm, 2.1cm, 2.1cm, 2.1cm, auto, 1.2fr),
    stroke: 0.5pt + rgb("#dddddd"),
    inset: 5pt,
    [*Category*], [*Critical*], [*High*], [*Medium*], [*Low*], [*Checked / clean*], [*Status*],
    ..d.scorecard.rows.map(row => (
      [#row.category],
      heat_cell(row.critical, "critical"),
      heat_cell(row.high, "high"),
      heat_cell(row.medium, "medium"),
      heat_cell(row.low, "low"),
      [#str(row.audited_rules)/#str(row.clean_rules)],
      [#chip(row.status, status_kind(row.status))],
    )).flatten()
  )
]

#pagebreak()

// ── 4. Severity × effort matrix ───────────────────────────────────────────
= Severity × effort matrix

#let matrix_cell(title, items) = {
  block(width: 100%, inset: 8pt, stroke: 0.5pt + rgb("#dddddd"), radius: 2pt)[
    #text(weight: "bold")[#title (#str(items.len()))]
    #if items.len() == 0 [
      #text(fill: rgb("#999999"), size: 9pt)[None this run.]
    ] else [
      #let cap = 10
      #let shown = items.slice(0, calc.min(cap, items.len()))
      #for it in shown [
        - #severity_chip(it.severity) #it.repo / #raw(breakable(it.path)):#str(it.line)
      ]
      #if items.len() > cap [
        #text(size: 8pt, fill: rgb("#888888"))[+#str(items.len() - cap) more (see Curated findings)]
      ]
    ]
  ]
}

#grid(
  columns: (1fr, 1fr),
  gutter: 8pt,
  matrix_cell("Do now", d.matrix.do_now),
  matrix_cell("Do next", d.matrix.do_next),
  matrix_cell("Plan", d.matrix.plan),
  matrix_cell("Accepted", d.matrix.accepted),
)

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
    // Item 1: the BOLD, primary heading for each finding is now the per-site defect headline
    // (rendered inside render_site) — the rule id + its own invariant title is demoted here to
    // a small gray SUBTITLE line, kept for registry traceability, not as the headline.
    #block(breakable: false)[
      #block(above: 12pt, below: 2pt)[
        #text(size: 8.5pt, fill: rgb("#888888"))[#group.rule_id: #group.title] #text(size: 8.5pt, fill: rgb("#888888"))[(#str(group.sites.len()) #plural(group.sites.len(), "site", "sites"))]
      ]
      // Item 5: show the citation ONCE. When real external sources exist, the bulleted
      // title+URL list IS the citation — the run-on `citation.label` (a redundant join of
      // those same titles) is dropped. `label` is shown only when there are no external
      // sources to bullet (the advisory/preview case), where it is the sole honesty note.
      #if group.citation.sources.len() > 0 [
        #for s in group.citation.sources [
          #text(size: 8.5pt, fill: rgb("#555555"))[- #s.title #if s.url != "" [(#s.url)]]
        ]
      ] else [
        #text(size: 8.5pt, fill: rgb("#555555"))[#group.citation.label]
      ]
      #if group.sites.len() > 0 [
        #render_site(group.sites.at(0))
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

// ── 8 + 9. Methodology & limitations + Disclaimer (S1: share a page) ────────────────
= Methodology & limitations

#d.methodology.deterministic_note

#d.methodology.ai_tier_note

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
