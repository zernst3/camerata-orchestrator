#let d = json("data.json")

#let or_dash(x) = if x == none { "\u{2014}" } else { x }

#set document(title: d.cover.project_title + " -- Camerata Audit Report")
#set page(
  paper: "us-letter",
  margin: (x: 2.2cm, y: 2cm),
  numbering: "1",
  footer: context [
    #set text(size: 8pt, fill: rgb("#808080"))
    #align(center)[Camerata audit report -- advisory, not a certification -- page #counter(page).display()]
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

#let chip(label, kind) = {
  let bg = if kind == "clean" { rgb("#e6f4ea") } else if kind == "attention" { rgb("#fff4e0") } else { rgb("#fdeaea") }
  let fg = if kind == "clean" { rgb("#1e7d34") } else if kind == "attention" { rgb("#b06a00") } else { rgb("#c0392b") }
  box(fill: bg, inset: (x: 6pt, y: 3pt), radius: 3pt, [#text(fill: fg, size: 8.5pt, weight: "bold")[#label]])
}

#let status_kind(status) = {
  if status == "Clean" { "clean" } else if status == "Attention" { "attention" } else { "action" }
}

// ── 1. Cover ─────────────────────────────────────────────────────────────
#align(center)[
  #v(2cm)
  #text(size: 22pt, weight: "bold")[Camerata Brownfield Audit Report]
  #v(0.4cm)
  #text(size: 14pt)[#or_dash(d.cover.project_title)]
  #v(0.2cm)
  #if d.cover.client_name != "" [#text(size: 12pt, fill: rgb("#666666"))[Prepared for #d.cover.client_name]]
  #v(1cm)
]

#table(
  columns: (auto, 1fr),
  stroke: none,
  inset: 4pt,
  [*Repos audited*], [#d.cover.repos.join(", ")],
  [*Files scanned*], [#str(d.cover.files_scanned) (#str(d.cover.files_excluded) excluded as noise)],
  [*Code volume*], [#str(d.cover.code_chars) characters],
  [*Camerata version*], [#or_dash(d.cover.camerata_version)],
  [*Audit model*], [#or_dash(d.cover.audit_model)],
  [*Calibration model*], [#or_dash(d.cover.calibration_model)],
  [*Generated*], [#d.cover.generated_at],
  [*Prepared by*], [#or_dash(d.cover.prepared_by)],
)

#v(0.4cm)
#text(weight: "bold")[Audited git state]
#for r in d.cover.audited_refs [
  - #r.repo --- #if r.sha != none [commit `#r.short_sha`] else [(no git metadata)] on #if r.branch != none [`#r.branch`] else [unknown branch]#if r.dirty [ (working tree had uncommitted changes at scan time)]
]

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

#pagebreak()

// ── 3. Category scorecard ─────────────────────────────────────────────────
= Category scorecard

#if d.scorecard.rows.len() == 0 [
  No findings were produced by any audited category.
] else [
  #table(
    columns: (1.6fr, auto, auto, auto, auto, auto, 1.2fr),
    stroke: 0.5pt + rgb("#dddddd"),
    inset: 5pt,
    [*Category*], [*Critical*], [*High*], [*Medium*], [*Low*], [*Checked / clean*], [*Status*],
    ..d.scorecard.rows.map(row => (
      [#row.category],
      [#str(row.critical)],
      [#str(row.high)],
      [#str(row.medium)],
      [#str(row.low)],
      [#str(row.clean_rules)/#str(row.audited_rules)],
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
      #text(fill: rgb("#999999"), size: 9pt)[none]
    ] else [
      #for it in items [
        - #it.rule_id --- #it.path:#str(it.line) #text(fill: rgb("#999999"))[(#it.severity)]
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
    #block(above: 12pt, below: 6pt)[
      #text(size: 12.5pt, weight: "bold")[#group.rule_id --- #group.title] (#str(group.sites.len()) site(s))
    ]
    #text(size: 8.5pt, fill: rgb("#555555"))[#group.citation.label]
    #if group.citation.sources.len() > 0 [
      #for s in group.citation.sources [
        - #s.title #if s.url != "" [(#s.url)]
      ]
    ]
    #for site in group.sites [
      #block(inset: (left: 8pt, top: 4pt, bottom: 4pt))[
        *#site.repo* / `#site.path`:#str(site.line) --- #text(fill: rgb("#999999"))[#site.severity]
        #if site.snippet != "" [
          #block(fill: rgb("#f5f5f5"), inset: 5pt, radius: 2pt)[#raw(site.snippet)]
        ]
        #site.detail
        #v(0.1cm)
        #text(size: 8.5pt)[
          Effort: #or_dash(site.effort) · Confidence: #or_dash(site.confidence) · #text(weight: "bold")[#site.disposition]
        ]
        #if site.also_matches.len() > 0 [
          #text(size: 8pt, fill: rgb("#888888"))[Also violates: #site.also_matches.join(", ")]
        ]
      ]
    ]
  ]
]

#pagebreak()

// ── 6. What's healthy ──────────────────────────────────────────────────────
= What's healthy

#if d.whats_healthy.rules.len() == 0 and not d.whats_healthy.dependency_clean [
  No zero-finding rules to report this run.
] else [
  #if d.whats_healthy.dependency_clean [
    - No known vulnerable dependencies detected in this scan.
  ]
  #for r in d.whats_healthy.rules [
    - *#r.rule_id* --- #r.title #text(fill: rgb("#888888"), size: 8.5pt)[(#r.citation.label)]
  ]
]

#text(size: 8.5pt, style: "italic", fill: rgb("#666666"))[
  Verified absent in THIS scan -- not a guarantee against future regressions.
]

#pagebreak()

// ── 7. Dependency / CVE snapshot ──────────────────────────────────────────
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
      [`#row.package`],
      [#row.repo],
      [#row.advisory],
      [#row.severity],
    )).flatten()
  )
]

#for note in d.dependency_snapshot.coverage_notes [
  #text(size: 8.5pt, fill: rgb("#888888"))[Coverage note: #note]
]

#pagebreak()

// ── 8. Methodology & limitations ──────────────────────────────────────────
= Methodology & limitations

#d.methodology.deterministic_note

#d.methodology.ai_tier_note

*#str(d.methodology.candidates_reviewed) candidate finding(s) reviewed; #str(d.methodology.excluded_false_positive) dispositioned as false positives by the auditor and excluded.*

#d.methodology.severity_scale_note

Not performed in this engagement:
#for item in d.methodology.not_done [
  - #item
]

#pagebreak()

// ── 9. Disclaimer ──────────────────────────────────────────────────────────
= Disclaimer

#block(stroke: 0.5pt + rgb("#cccccc"), inset: 10pt, radius: 2pt)[
  #d.disclaimer
]
