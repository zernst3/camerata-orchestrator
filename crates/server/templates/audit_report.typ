#let d = json("data.json")

// ── W27 brand tokens (docs/design/2026-10-27_pdf-brand-guide.md) ─────────────────────
// One place for every colour, font and size the report uses. When the brand guide changes,
// change these values and the guide together, never a one-off literal further down.
//
// Vivid blue is the ONLY saturated colour: Critical severity, the logo bar, the eyebrow dots
// and at most one accent rule per page. Every other severity is a neutral chip, so the page is
// not a rainbow. The line/tint tokens are ink mixed into white rather than transparent ink,
// so a printer renders exactly the shade the screen shows.
#let ink = rgb("#0F1826")
#let ink_soft = rgb("#47536A")
#let ink_faint = rgb("#6B7690")
#let accent = rgb("#2B5580")
#let accent_deep = rgb("#1B3A56")
#let vivid = rgb("#0C22F2")
#let vivid_mist = rgb("#E5E8FE")
#let bg_blue = rgb("#E9F0FA")
#let card_fill = white
#let line_c = color.mix((ink, 8%), (white, 92%))
#let line_strong = color.mix((ink, 14%), (white, 86%))
#let tint = color.mix((ink, 3.5%), (white, 96.5%))
#let chip_fill = color.mix((ink, 4.5%), (white, 95.5%))

// The two embedded families. `compile_pdf_from_data` writes the bundled font files next to
// this template and passes `--font-path`, which Typst searches BEFORE system fonts, so the
// deliverable never depends on what happens to be installed on the exporting machine. A
// glyph neither family has (a CJK path, an emoji) still falls back to a system font rather
// than vanishing.
#let sans = "Inter"
#let mono = "JetBrains Mono"

// M6: an absent-or-empty option string both render as the placeholder — a client who
// exports with no options modal input must never see a blank cover field.
// No em/en dashes in the placeholder itself (house no-dash rule) — "N/A", not "\u{2014}".
#let or_na(x) = if x == none or x == "" { "N/A" } else { x }

// C3-1b: `effort`/`confidence` are the CALIBRATION pass's OWN outputs — an absent value here
// means that pass never produced an opinion for THIS finding this run (a failed/timed-out
// pass, or a malformed response), not a legitimate "not applicable" answer the way a blank
// cover-page option field is. "N/A" reads as a settled non-answer rather than an honest gap,
// so these two fields get their own, field-specific placeholders instead of `or_na`.
#let or_not_estimated(x) = if x == none or x == "" { "not estimated this run" } else { x }
#let or_not_evaluated(x) = if x == none or x == "" { "not evaluated this run" } else { x }

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
// does not wrap inside such a run — it overflows the page edge.
//
// C4-R3: this used to insert an invisible zero-width space (U+200B) between chunks as the
// break opportunity — but that is a REAL character that survives into the PDF's text/copy
// layer, so selecting or grepping a path out of the report came back with invisible junk
// spliced into it, silently breaking the exact copy-paste-into-a-terminal workflow a report
// reader needs most. `h(0pt, weak: true)` is pure layout spacing: zero width, so it never
// shows up in copied or extracted text, yet it still gives the line breaker an optional break
// point between the surrounding raw() chunks. Returns CONTENT (one raw() per chunk), never a
// plain string — call sites use the result directly; wrapping it in another raw() call would
// flatten everything back into one unbreakable run and reintroduce the overflow this fixes.
// UTF8-BREAK-1: `s.slice(i, e)` indexes BYTES, and Typst's `str.slice` hard-errors the whole
// compile when an index lands mid-character ("byte index is not a char boundary"). `s.len()`
// is also a byte count. Chunking by byte offset is therefore only safe for pure-ASCII input —
// any path, snippet or package name containing a multi-byte character (an accented letter, a
// CJK character, an emoji) took down the entire PDF export with no deliverable produced. Fix:
// chunk over `.clusters()` (extended grapheme clusters — the "perceived character" unit, so a
// multi-codepoint emoji/ZWJ sequence stays exactly one chunk-unit rather than being split
// across chunks) instead of raw bytes. `n` keeps its original meaning of "roughly how many
// characters before a break", just measured in clusters instead of bytes now.
#let breakable(s, n: 40) = {
  // Guard: a none/empty input must yield empty content, never `none` or a crash. Typst's
  // raw(none) fails the whole compile ("expected string, found none") — so any finding with an
  // empty path/snippet/package would crash the report without this.
  if s == none { return raw("") }
  let clusters = s.clusters()
  let len = clusters.len()
  if len == 0 { return raw("") }
  let i = 0
  let out = ()
  while i < len {
    let e = calc.min(i + n, len)
    out.push(raw(clusters.slice(i, e).join()))
    i = e
  }
  out.intersperse(h(0pt, weak: true)).join()
}

// Fix text (a rule's authored remediation, or the model's codebase-specific fix). Rust has
// already joined the source's hard line wraps (`report_export::fix_text::prepare`), so a
// single newline never reaches here; a blank line ("\n\n") is a paragraph break. A
// `single-backtick span` is set as inline mono (raw), the way the authored text marks code,
// instead of printing the backticks. Anything that is not a well-formed span (an unclosed
// backtick, a run of two or more) stays literal. The regex matches non-empty, backtick-free
// bodies only, and `breakable` keeps a long path from running off the page.
#let fix_runs(s) = {
  let out = ()
  let pos = 0
  for m in s.matches(regex("`[^`]+`")) {
    if m.start > pos { out.push(s.slice(pos, m.start)) }
    out.push(breakable(m.text.slice(1, -1)))
    pos = m.end
  }
  if pos < s.len() { out.push(s.slice(pos)) }
  out.join()
}
#let fix_body(s) = s.split("\n\n").map(fix_runs).join(parbreak())

// Branding (2026-09-13 review, owner's hard constraint): Camerata is the licensable
// APPLICATION; the auditing agency running it is not Camerata's to name. `d.cover.brand` is
// already fully resolved (per-report ReportOptions.brand > CAMERATA_REPORT_BRAND env var > no
// brand at all) by `report_export::build_report_json` — this template just renders it, never
// invents a fallback agency name. Hyphen, not an em/en dash (house no-dash rule, see `or_na`
// above) — "Cantus Works - Codebase Inspection", not "Cantus Works \u{2014} Codebase Inspection".
//
// P6 (2026-09-29): the product is "Codebase Inspection" — this literal must stay in sync with
// `report_export::NEUTRAL_COVER_TITLE` (a Rust const the template cannot import directly; a
// test pins the two together — see `shipped_template_has_no_hardcoded_cover_title`).
#let brand_title = if d.cover.brand != none {
  d.cover.brand + " - Codebase Inspection"
} else {
  "Codebase Inspection Report"
}

#let doc_title = if d.cover.project_title != "" {
  d.cover.project_title + ", " + brand_title
} else {
  brand_title
}
#set document(title: doc_title)

// W27: the logo mark is the bundled brand kit's, so it renders only when the resolved brand IS
// that kit's brand (`d.brand_mark`, decided server-side by `report_export::brand_kit_matches`).
// Any other agency's brand gets the text wordmark alone; no brand gets neither. The template
// never decides which agency a report belongs to.
#let wordmark(size: 9pt) = text(font: sans, size: size, weight: 600, tracking: 0.14em, fill: ink)[#upper(d.cover.brand)]
#let brand_lockup(mark_height, file, size: 9pt) = {
  if d.cover.brand == none { return [] }
  if d.brand_mark {
    box(baseline: 30%, image(file, height: mark_height))
    h(mark_height / 3)
  }
  wordmark(size: size)
}

// P5: `d.review_state` is `"raw"` when NO finding in this scan carries an explicit
// THIS-SESSION auditor disposition (see `report_export::ReviewState`'s doc comment) — a nobody-
// has-looked-at-this-yet export. `is_draft` gates a banner on EVERY page (header, so it survives
// no matter how many pages the report grows to) so a raw export can never be mistaken for a
// signed-off deliverable, matching the exec summary's own engine-voice narrative on page 2.
#let is_draft = d.review_state == "raw"

#set page(
  paper: "us-letter",
  margin: (x: 2.4cm, top: 2.6cm, bottom: 2.6cm),
  numbering: "1",
  header: context [
    #set text(font: sans, size: 8pt, fill: ink_faint)
    // W27: a running header from page 2 on (the cover carries the full mark itself): the small
    // mark and the wordmark on the left, the project title on the right.
    #if counter(page).get().first() > 1 and (d.cover.brand != none or d.cover.project_title != "") [
      #grid(
        columns: (1fr, auto),
        align: (left + horizon, right + horizon),
        brand_lockup(10pt, "brand-mark-small.svg", size: 7.5pt),
        [#d.cover.project_title],
      )
      #v(-4pt)
      #line(length: 100%, stroke: 0.5pt + line_c)
    ]
    #if is_draft [
      #align(center)[
        #box(fill: vivid_mist, stroke: 0.75pt + vivid, inset: (x: 8pt, y: 3pt), radius: 99pt)[
          #text(size: 8pt, weight: 600, tracking: 0.04em, fill: vivid)[DRAFT: not yet reviewed by a human reviewer]
        ]
      ]
    ]
  ],
  // C4-R3: the footer used to echo a bare " · DRAFT" after the page-number line, ON TOP OF the
  // header's full banner above it — on every page, that bare word sits immediately before the
  // NEXT page's "DRAFT: not yet reviewed..." in reading/selection/extraction order, with
  // nothing but a page boundary between them, so a copy-paste or a naive multi-page text
  // extraction read back "...page 1 · DRAFTDRAFT: not yet reviewed...". The header banner
  // above already marks EVERY page unambiguously; render the draft state once per page, not
  // twice.
  //
  // Confidentiality (2026-10-24 decision): every page carries "Confidential, prepared for
  // <client> on <date>" (`d.confidentiality.line`, from `report_export::confidentiality_marking`,
  // the same producer the workbook's sheet footers use), above the advisory line.
  footer: context [
    #set text(font: mono, size: 8pt, fill: ink_faint)
    #line(length: 100%, stroke: 0.5pt + line_c)
    #v(-2pt)
    #align(center)[#d.confidentiality.line]
    #v(-6pt)
    #align(center)[
      #if d.cover.brand != none [#d.cover.brand ]inspection report (advisory, not a certification), page #counter(page).display()
    ]
  ],
)

// Body: Inter 10 pt, ink-soft, line height 1.6 (cap height plus leading, so a baseline-to-
// baseline distance of 1.6em with the default cap-height/baseline text edges).
#set text(font: sans, size: 10pt, fill: ink_soft, lang: "en")
#set par(leading: 0.87em, spacing: 1.2em)
// Weight 600 is the brand's only bold: `*strong*` markup lands on SemiBold, not Bold.
#set strong(delta: 200)
#set heading(numbering: none)
// Section heading (h2 in the brand guide): 20 pt, 600, tracking -0.024em, ink.
#show heading.where(level: 1): it => {
  block(above: 6pt, below: 14pt)[
    #text(size: 20pt, weight: 600, tracking: -0.024em, fill: ink, it.body)
  ]
}
// M8: raw text (paths, shas, snippets) rendered a touch smaller than body copy, and never
// justified (justification widens the invisible breakable() gaps unevenly).
#show raw: set text(font: mono, size: 8.5pt)
#show raw.where(block: true): set par(justify: false)
#show link: set text(fill: accent)

// Eyebrow (section label): 8.5 pt uppercase, 600, tracking 0.09em, accent, after a 4 pt vivid
// dot. Rendered above each section heading by `section` below.
#let eyebrow(label) = block(below: 6pt, sticky: true)[
  #box(baseline: -1.5pt, circle(radius: 2pt, fill: vivid, stroke: none))
  #h(4pt)
  #text(size: 8.5pt, weight: 600, tracking: 0.09em, fill: accent)[#upper(label)]
]

// The words the vocabulary module owns: the section titles, the labels and the sheet names. The
// template spells none of them itself, so the PDF and the workbook name every thing alike.
#let W = d.words
#let sec(i) = W.sections.at(i)

// One report section: the eyebrow, then the level-1 heading the contents page lists. A section
// that is not listed on the contents page (the contents page itself) is still a bookmark in the
// PDF outline, so the outline is always the whole list of sections.
#let section(label, title, outlined: true) = {
  eyebrow(label)
  heading(level: 1, outlined: outlined, bookmarked: true, title)
}

// Sub-heading (h3): 13 pt, 600, tracking -0.012em, ink.
#let subheading(body) = text(size: 13pt, weight: 600, tracking: -0.012em, fill: ink, body)

// A highlighted panel (summary box, handling note): bg-blue, no border.
#let panel(body) = block(fill: bg_blue, inset: 12pt, radius: 8pt, width: 100%, body)

// A white card with a 1 px line border (finding cards, the disclaimer, the sign-off).
#let card(body, breakable: true) = block(
  fill: card_fill,
  stroke: 0.75pt + line_c,
  inset: 12pt,
  radius: 8pt,
  width: 100%,
  breakable: breakable,
  body,
)

// Table chrome: header text accent-deep 600 on a line-strong rule, line-colour row rules.
#let table_stroke = (x, y) => if y == 0 { (bottom: 0.75pt + line_strong) } else { (bottom: 0.5pt + line_c) }
#let th(body) = text(size: 8.5pt, weight: 600, fill: accent_deep, body)

#let chip(label, kind) = {
  // Status chips stay neutral apart from "Action-needed", which takes the accent on the
  // highlight-panel blue (vivid is reserved for Critical severity).
  let bg = if kind == "action" { bg_blue } else { chip_fill }
  let fg = if kind == "action" { accent_deep } else if kind == "attention" { ink_soft } else { ink_faint }
  box(fill: bg, inset: (x: 7pt, y: 3pt), radius: 99pt, [#text(fill: fg, size: 8pt, weight: 600, tracking: 0.04em)[#label]])
}

#let status_kind(status) = {
  if status == "Clean" { "clean" } else if status == "Attention" { "attention" } else { "action" }
}

// S2: severity as a chip, not the lightest gray ink on the page — a "critical" finding's
// severity word must be the most visible thing in its cell, not the least. W27: Critical is
// vivid on vivid-mist; High separates from Medium in print by taking the accent; Medium and
// Low stay neutral.
#let severity_chip(sev) = {
  let bg = if sev == "critical" { vivid_mist } else { chip_fill }
  let fg = if sev == "critical" { vivid } else if sev == "high" { accent } else { ink_soft }
  box(fill: bg, inset: (x: 6pt, y: 2.5pt), radius: 99pt, [#text(fill: fg, size: 8pt, weight: 600, tracking: 0.04em)[#upper(sev)]])
}

// Item 6: the category scorecard as a compact heat-grid. Native Typst primitives only (no
// packages) — a small, hand-picked light-to-dark ramp per severity, indexed by count, so a
// "critical" cell with 4 findings reads visibly heavier than one with 1. The owner's original
// ruling made this the ONE approved visual addition and kept the severity x effort section a
// plain bucketed list. FIX 6 (2026-09-13 review) revisited that second half: the bucketed list
// duplicated page 3's "Three things this week" box without ever delivering the 2-D placement
// its own heading promised, so the owner approved replacing it with a real severity-rows x
// effort-columns `table()` (see `d.priority_grid` below) — still a plain Typst table, not a
// new chart primitive. No OTHER decorative visual gets added beyond this heat-grid and that
// table: restrained, engineer-made, not marketing. W27: the ramps are brand colours mixed into
// white (vivid for Critical, accent for High, ink for Medium and Low), so the grid keeps its
// intensity reading without introducing a second saturated hue.
#let heat_bg(sev, n) = {
  let base = if sev == "critical" { vivid } else if sev == "high" { accent } else { ink }
  let strengths = if sev == "critical" or sev == "high" { (0%, 12%, 30%, 60%, 90%) } else if sev == "medium" { (0%, 8%, 18%, 34%, 55%) } else { (0%, 5%, 12%, 22%, 36%) }
  let idx = if n == 0 { 0 } else if n == 1 { 1 } else if n <= 3 { 2 } else if n <= 6 { 3 } else { 4 }
  let s = strengths.at(idx)
  if s == 0% { tint } else { color.mix((base, s), (white, 100% - s)) }
}

#let heat_cell(n, sev) = {
  let idx = if n == 0 { 0 } else if n == 1 { 1 } else if n <= 3 { 2 } else if n <= 6 { 3 } else { 4 }
  let fg = if idx >= 3 and (sev == "critical" or sev == "high") { white } else { ink }
  align(center)[
    #box(fill: heat_bg(sev, n), inset: (x: 6pt, y: 4pt), radius: 4pt, width: 1.7cm)[
      #align(center)[#text(fill: fg, weight: if n > 0 { 600 } else { 400 })[#str(n)]]
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
// The cover is the first section of the PDF outline. Its heading takes no room on the page.
#place(top + left, hide(heading(level: 1, outlined: false, bookmarked: true, sec(0))))
#brand_lockup(34pt, "brand-mark.svg", size: 10pt)
#v(2.2cm)
#block[
  #set par(leading: 0.3em)
  #text(size: 30pt, weight: 600, tracking: -0.03em, fill: ink)[#brand_title]
]
#if d.cover.project_title != "" [
  #v(0.2cm)
  #text(size: 15pt, fill: ink_soft)[#d.cover.project_title]
]
#if d.cover.client_name != "" [
  #v(0.1cm)
  #text(size: 11pt, fill: ink_faint)[Prepared for #d.cover.client_name]
]
// An internal export of a run that did not complete says so, in red, on the cover. Absent from
// every complete run's data, so a complete run's cover is unchanged.
#if d.cover.at("internal_incomplete", default: none) != none [
  #v(0.3cm)
  #text(size: 11pt, weight: 700, fill: rgb("#b00020"))[#d.cover.internal_incomplete]
]
// The one vivid accent rule on this page.
#v(0.5cm)
#line(length: 1.4cm, stroke: 2.5pt + vivid)
#v(0.6cm)

// P6 (2026-09-29): real code volume, never "0 characters". `d.cover.code_volume` is `none`
// whenever `report_export::build_report_json` has no real line count for this run (see that
// field's doc comment) — the row is OMITTED entirely in that case, never rendered as a zero.
// `d.cover.code_chars` (raw character count) is deliberately NOT used here any more: a bare
// character count reads as noise to a non-engineer reader next to a real line-count figure.
#let code_volume_label(cv) = {
  let base = thousands(cv.lines) + " " + plural(cv.lines, "line", "lines") + " of code"
  let with_languages = if cv.by_language.len() == 0 {
    base
  } else {
    base + " (" + cv.by_language.map(l => l.language + ": " + thousands(l.lines)).join(", ") + ")"
  }
  // A project of several repos also says how large each one is. The key is absent for one repo.
  if not ("per_repo" in cv) or cv.per_repo.len() == 0 {
    with_languages
  } else {
    with_languages + ". By repo: " + cv.per_repo.map(r => r.repo + ": " + thousands(r.lines)).join(", ")
  }
}
#let code_volume_rows = if d.cover.code_volume != none {
  (([*Code volume*], [#code_volume_label(d.cover.code_volume)]),)
} else {
  ()
}

#table(
  columns: (auto, 1fr),
  stroke: (x, y) => (bottom: 0.5pt + line_c),
  inset: (x: 0pt, y: 6pt),
  column-gutter: 16pt,
  [*Repos inspected*], [#d.cover.repos.join(", ")],
  [*Files scanned*], [#thousands(d.cover.files_scanned) (#thousands(d.cover.files_excluded) excluded as noise)],
  ..code_volume_rows.flatten(),
  [*Generated*], [#d.cover.generated_at],
  // P6: `d.cover.prepared_by` is resolved server-side (`resolve_prepared_by`) with a real
  // default name as the final fallback — it is NEVER blank, so this must NEVER render "N/A"
  // (promise 1: "one person reads every finding and signs the report").
  [*Prepared by*], [#d.cover.prepared_by],
)

// Nice-to-have: a cover stat strip so the story starts on page 1, not page 2.
#v(0.3cm)
#text(size: 9.5pt, fill: ink_soft)[
  #d.cover.stats.critical critical · #d.cover.stats.high high · #d.cover.stats.medium medium · #d.cover.stats.accepted accepted · #d.cover.stats.dependency_advisories #plural(d.cover.stats.dependency_advisories, "dependency advisory", "dependency advisories")
]

// H11: an incremental run says so on the first page, with how much it re-audited and how much it
// carried forward. Prints nothing for a full scan.
#if d.cover.incremental_note != none [
  #v(0.3cm)
  #text(size: 9.5pt, fill: ink_soft)[#d.cover.incremental_note]
]

// Nice-to-have: gate the heading when there is nothing under it.
#if d.cover.audited_refs.len() > 0 [
  #v(0.4cm)
  #text(weight: 600, fill: ink)[Inspected git state]
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

// Confidentiality (2026-10-24 decision): the first page's handling note, who the report is
// for and that it is not to be forwarded outside that company without consent.
#v(0.6cm)
#panel[
  #text(size: 8.5pt, weight: 600, tracking: 0.09em, fill: accent)[CONFIDENTIAL]
  #linebreak()
  #text(size: 9pt)[#d.confidentiality.handling_note]
]

#pagebreak()

// ── 2. Contents ──────────────────────────────────────────────────────────────────────────
// One line of Typst, clickable bookmarks in every viewer. The contents page is a bookmark too,
// but not a line of its own list.
#section("Contents", sec(1), outlined: false)
#outline(title: none, depth: 1)

#pagebreak()

// ── 3. Executive summary ─────────────────────────────────────────────────────────────────
#section("Summary", sec(2))

#d.executive_summary.narrative

#if d.executive_summary.is_override [
  #text(size: 8.5pt, style: "italic", fill: ink_faint)[Summary text supplied by the reviewer.]
]

// W6: a failed/timed-out pass must be disclosed, never omitted silently. Rendered here
// (the summary) AND again in the methodology below — the same strings both times,
// see `report_export::failed_pass_disclosures`. Gated on non-empty so a clean run renders
// nothing extra.
#if d.executive_summary.failed_passes.len() > 0 [
  #block(above: 8pt, below: 4pt, stroke: 0.75pt + vivid, inset: 10pt, radius: 8pt, width: 100%)[
    #text(weight: 600, size: 9pt, fill: ink)[Not run this inspection]
    #for note in d.executive_summary.failed_passes.slice(0, calc.min(d.executive_summary.failed_passes.len(), W.failed_passes_cap)) [
      - #note
    ]
    #if d.executive_summary.failed_passes.len() > W.failed_passes_cap [
      #text(size: 9pt, fill: ink_soft)[and #str(d.executive_summary.failed_passes.len() - W.failed_passes_cap) #W.failed_passes_more]
    ]
  ]
]

// ── 4. If you only do three things this week ─────────────────────────────────────────────
// One entry is one fix, so the box never lists one change several times. The total counts each
// fix's effort once.
#v(0.4cm)
#section("Priorities", sec(3))

#if d.three_things.items.len() == 0 [
  No fix is due first this run.
] else [
  #panel[
    #for item in d.three_things.items [
      #block(above: 4pt, below: 10pt)[
        #severity_chip(item.severity) #h(2pt) #text(weight: 600, fill: ink)[#fix_runs(item.headline)]
        #linebreak()
        #text(font: mono, size: 8pt, fill: ink_faint)[#breakable(item.place, n: 60)]
        #if item.closes_line != "" [
          #linebreak()
          #text(size: 8.5pt, fill: ink_soft)[#item.closes_line]
        ]
        #if item.places_line != "" [
          #linebreak()
          #text(size: 8.5pt, fill: ink_soft)[#item.places_line]
        ]
        #linebreak()
        #text(size: 9pt)[Rough estimate: #item.hours_label]
        // FIX 7 (2026-09-13 review): one factual sentence of what the exposure MEANS, when
        // one is authored for this rule — omitted gracefully when it isn't.
        #if item.impact != none [
          #linebreak()
          #text(size: 8.5pt, style: "italic", fill: ink_soft)[#item.impact]
        ]
      ]
    ]
    #line(length: 100%, stroke: 0.5pt + line_strong)
    #v(4pt)
    #text(weight: 600, fill: ink)[#d.three_things.total_hours_label]
  ]
]

#pagebreak(weak: true)

// ── 5. Area scorecard ────────────────────────────────────────────────────────────────────
#section("Scorecard", sec(4))

#if d.scorecard.rows.len() == 0 [
  No problem was found in any inspected area.
] else [
  #table(
    // The severity columns get a FIXED width because `heat_cell`'s box is itself fixed-width.
    // The status column is wide enough for its longest chip, so no word breaks mid-word, and the
    // area name wraps when it has no natural break.
    columns: (1fr, 1.6cm, 1.6cm, 1.6cm, 1.6cm, 4.1cm, 3.2cm),
    stroke: table_stroke,
    inset: (x: 4pt, y: 5pt),
    align: horizon,
    table.header(th[#W.area], th[Critical], th[High], th[Medium], th[Low], th[Rules checked · Rules clean], th[Status]),
    ..d.scorecard.rows.map(row => (
      [#breakable(row.category, n: 18)],
      heat_cell(row.critical, "critical"),
      heat_cell(row.high, "high"),
      heat_cell(row.medium, "medium"),
      heat_cell(row.low, "low"),
      [#str(row.audited_rules) checked · #str(row.clean_rules) clean],
      [#chip(row.status, status_kind(row.status))],
    )).flatten()
  )
  #v(4pt)
  #text(size: 8.5pt, fill: ink_faint)[#d.scorecard.footnote]
]

#v(0.6cm)

// ── 6. Severity by effort matrix ─────────────────────────────────────────────────────────
// Each problem sits in exactly one cell (`d.priority_grid`, built by
// `report_export::build_priority_grid`). A cell counts; it lists no problem. A cell with no
// problem in it is blank: the row and column headers already say which bucket it is.
#section("Prioritization", sec(5))

#let effort_col_label(key) = {
  if key == "low" { "Low effort" }
  else if key == "medium" { "Medium effort" }
  else if key == "high" { "High effort" }
  else { "Not yet estimated" }
}

#let grid_cell_content(cell) = {
  let n = cell.findings.len()
  if n == 0 [] else [
    #text(size: 9pt, weight: 600, fill: ink)[#str(n)] #text(size: 8pt, fill: ink_faint)[#plural(n, "problem", "problems")]
  ]
}

#if d.priority_grid.rows.len() == 0 [
  No problem is due First, Next or Later this run.
] else [
  #table(
    columns: (auto,) + d.priority_grid.columns.map(c => 1fr),
    stroke: table_stroke,
    inset: (x: 5pt, y: 6pt),
    table.header(th[#W.severity], ..d.priority_grid.columns.map(c => th[#effort_col_label(c)])),
    ..d.priority_grid.rows.map(row => (
      [#severity_chip(row.severity)],
      ..row.cells.map(cell => grid_cell_content(cell)),
    )).flatten()
  )
]

#if d.priority_grid.rows.len() > 0 [
  #v(0.3cm)
  #text(size: 8.5pt, fill: ink_faint)[#d.priority_grid.footnote]
]

#if d.priority_grid.accepted_count > 0 or d.priority_grid.informational_count > 0 [
  #v(0.3cm)
  #text(size: 8.5pt, fill: ink_faint)[
    #if d.priority_grid.accepted_count > 0 [
      #str(d.priority_grid.accepted_count) #plural(d.priority_grid.accepted_count, "problem is", "problems are") accepted risk and out of scope for this grid.
    ]
    #if d.priority_grid.informational_count > 0 [
      #str(d.priority_grid.informational_count) further #plural(d.priority_grid.informational_count, "item is", "items are") information only and out of scope for this grid.
    ]
  ]
]

#pagebreak(weak: true)

// ── 7. The most serious problems ─────────────────────────────────────────────────────────
// The places reviewed in person, worst first, at most the cap: one short entry for each place,
// whatever the number of rules at it. Every other place is on the All problems sheet.
#section("Escalations", sec(6))

#if d.escalations.items.len() == 0 [
  No place was sent for review in person in this inspection.
] else [
  #text(size: 9pt, style: "italic", fill: ink_faint)[#d.escalations.intro]
  #v(4pt)
  #for item in d.escalations.items.slice(0, calc.min(d.escalations.items.len(), d.escalations.cap)) [
    #block(breakable: false, above: 6pt, below: 8pt)[
      #severity_chip(lower(item.severity)) #h(2pt) #text(weight: 600, fill: ink)[#fix_runs(item.headline)]
      #linebreak()
      #text(font: mono, size: 8pt, fill: ink_faint)[#breakable(item.place, n: 60)]
      #linebreak()
      #text(size: 8.5pt, fill: ink_soft)[#item.rules_line]
      #if item.what_to_do != "" [
        #linebreak()
        #text(weight: 600, size: 9pt, fill: accent)[#W.what_to_do: ]#text(size: 9pt)[#fix_body(item.what_to_do)]
      ]
      #linebreak()
      #text(font: mono, size: 8pt, fill: ink_faint)[#W.effort: #or_not_estimated(item.effort) · #W.decision_so_far: #item.decision]
    ]
  ]
  #if d.escalations.more_line != "" [
    #text(size: 9pt, fill: ink_soft)[#d.escalations.more_line]
  ]
]

// ── 8. The other fixes ───────────────────────────────────────────────────────────────────
// One line for each fix that is not above, grouped by area, with a cap; the rest are counted.
// Multi-repo observations fold in as short lines at the end. No fix text, no place list.
#v(0.5cm)
#section("Fixes", sec(7))

#if d.other_fixes.groups.len() == 0 [
  No other fix is due this run.
] else [
  #text(size: 9pt, style: "italic", fill: ink_faint)[#d.other_fixes.intro]
  #v(4pt)
  #for group in d.other_fixes.groups [
    #block(breakable: false, above: 8pt, below: 2pt, sticky: true)[
      #text(weight: 600, fill: ink)[#breakable(group.area, n: 40)]
    ]
    #for line in group.lines [
      #block(above: 3pt, below: 3pt)[
        #severity_chip(lower(line.severity)) #h(3pt) #line.name #text(size: 8.5pt, fill: ink_faint)[ · #line.places#if line.effort != "" [ · #line.effort effort] · #line.fix_order]
      ]
    ]
  ]
  #if d.other_fixes.more_line != "" [
    #v(4pt)
    #text(size: 9pt, fill: ink_soft)[#d.other_fixes.more_line]
  ]
]

#if d.patterns_across_services != none [
  #v(0.4cm)
  #subheading[#W.patterns_across_services]
  #v(2pt)
  #text(size: 9pt, style: "italic", fill: ink_faint)[#d.patterns_across_services.note]
  #v(4pt)
  #if d.patterns_across_services.summary_lines.len() == 0 [
    No rule was found violated in one service and clean in another.
  ] else [
    #for line in d.patterns_across_services.summary_lines [
      #block(above: 3pt, below: 3pt)[#line]
    ]
    #if d.patterns_across_services.more_line != "" [
      #text(size: 9pt, fill: ink_soft)[#d.patterns_across_services.more_line]
    ]
  ]
]

#pagebreak(weak: true)

// ── 9. Your conventions ──────────────────────────────────────────────────────────────────
// Counts and short lines: what the code does consistently, where it has no single habit and
// where it departs from itself. Every row is on the Your conventions and Departures sheets.
#section("Conventions", sec(8))

#if not d.conventions.any [
  This inspection derived no conventions and found no departure from one.
] else [
  #if d.conventions.followed_line != "" [#d.conventions.followed_line #linebreak()]
  #if d.conventions.none_adopted_line != "" [#d.conventions.none_adopted_line #linebreak()]
  #if d.conventions.departures_line != "" [#d.conventions.departures_line]
  #for line in d.conventions.departures [
    #block(above: 3pt, below: 3pt)[- #line]
  ]
  #if d.conventions.more_line != "" [
    #text(size: 9pt, fill: ink_soft)[#d.conventions.more_line]
  ]
]

// ── 10. What is healthy ──────────────────────────────────────────────────────────────────
#v(0.6cm)
#section("Strengths", sec(9))

#if d.whats_healthy.rules.len() == 0 and not d.whats_healthy.dependency_clean [
  No rule came back clean this run.
] else [
  #if d.whats_healthy.dependency_clean [
    - No known vulnerable dependencies detected in this scan.
  ]
  #for r in d.whats_healthy.rules [
    - #r.title #text(fill: ink_faint, size: 8.5pt)[(#r.rule_id)]#if r.clean_in.len() > 0 [ #text(fill: ink_faint, size: 8.5pt)[Clean in #r.clean_in.join(", ")#if r.not_applicable_in.len() > 0 [; not applicable in #r.not_applicable_in.join(", ")].]]
  ]
  #if d.whats_healthy.further_clean_count > 0 [
    #text(size: 8.5pt, fill: ink_faint)[#str(d.whats_healthy.further_clean_count) further #plural(d.whats_healthy.further_clean_count, "rule", "rules") verified clean this run.]
  ]
  #text(size: 8.5pt, style: "italic", fill: ink_faint)[
    Verified absent in THIS scan, not a guarantee against future regressions.
  ]
]

// ── 11. Dependencies ─────────────────────────────────────────────────────────────────────
#v(0.6cm)
#section("Dependencies", sec(10))

#if d.dependency_snapshot.clean [
  No known vulnerable dependencies detected.
] else if d.dependency_snapshot.rows.len() == 0 [
  // M11: no rows is not the same as clean. The audit did not run, or left manifests out, so
  // this section cannot say that no dependency is vulnerable.
  Dependencies were not fully checked in this scan, so this report cannot say that none are vulnerable. The notes below say what was not checked.
] else [
  #d.dependency_snapshot.summary_line.
  #for row in d.dependency_snapshot.worst [
    #block(above: 3pt, below: 3pt)[
      #severity_chip(lower(row.severity)) #h(3pt) #breakable(row.package, n: 30) #text(size: 8.5pt, fill: ink_faint)[#row.advisory]
    ]
  ]
  #if d.dependency_snapshot.more_line != "" [
    #text(size: 9pt, fill: ink_soft)[#d.dependency_snapshot.more_line]
  ]
]

#for note in d.dependency_snapshot.coverage_notes [
  #block(above: 4pt, below: 4pt)[
    #text(size: 8.5pt, fill: ink_faint)[Coverage note: #note]
  ]
]

// ── 12. Next steps ───────────────────────────────────────────────────────────────────────
// A short, factual "what happens after this report" paragraph (`report_export::next_steps_note`,
// authored prose, never an LLM call).
#v(0.6cm)
#section("After this report", sec(11))

#d.methodology.next_steps

// ── 13. Where the details are ────────────────────────────────────────────────────────────
// Each question and the exact workbook tab that answers it. The tab names are the vocabulary's,
// the same constants the workbook names its sheets from.
#v(0.6cm)
#section("Details", sec(12))

#d.words.details_intro
#for entry in W.detail_map [
  #block(above: 3pt, below: 3pt)[#entry.question #h(4pt) #text(weight: 600, fill: accent)[#entry.sheet]]
]

// ── 14. How this was done and its limits ─────────────────────────────────────────────────
#v(0.6cm)
#section("How this was done", sec(13))

#d.methodology.deterministic_note

#d.methodology.ai_tier_note

#d.methodology.citation_note

// The engine build, the rule library hash and the calibration model are in manifest.json, not
// here: a reader of this report has no use for them.
Scanned with Camerata v#or_na(d.cover.camerata_version). Inspection model: #or_na(d.cover.audit_model). Model backend: #or_na(d.cover.model_backend).

// P5: this reconciling line must never claim a human "reviewed"/"dispositioned" a finding
// unless `d.review_state == "reviewed"` (at least one finding this session carries an explicit
// auditor disposition — see `report_export::ReviewState`'s doc comment). The two branches also
// each show EVERY number in the partition, so `candidates_reviewed` always reconciles on the page.
#if d.review_state == "reviewed" [
  *#str(d.methodology.candidates_reviewed) #plural(d.methodology.candidates_reviewed, "problem", "problems") reviewed by a human reviewer; #str(d.methodology.excluded_false_positive) ruled out and left out of this report; #str(d.methodology.held_for_review) #plural(d.methodology.held_for_review, "needs", "need") a further decision.*
] else [
  *#str(d.methodology.candidates_reviewed) #plural(d.methodology.candidates_reviewed, "problem", "problems") produced by the engine; #str(d.methodology.held_for_review) #plural(d.methodology.held_for_review, "needs", "need") a human reviewer's decision; #str(d.methodology.excluded_false_positive) ruled out automatically. This export has not yet had a human review.*
]

#d.methodology.severity_scale_note

Not performed in this engagement:
#for item in d.methodology.not_done [
  - #item
]

// W6: a failed/timed-out pass must be disclosed, never omitted silently — same strings as
// the Executive summary's box above (see `report_export::failed_pass_disclosures`), restated
// here for the more technical reader. Gated on non-empty.
#if d.methodology.failed_passes.len() > 0 [
  #v(0.2cm)
  Not run this inspection:
  #for note in d.methodology.failed_passes.slice(0, calc.min(d.methodology.failed_passes.len(), W.failed_passes_cap)) [
    - #note
  ]
  #if d.methodology.failed_passes.len() > W.failed_passes_cap [
    #text(size: 9pt, fill: ink_soft)[and #str(d.methodology.failed_passes.len() - W.failed_passes_cap) #W.failed_passes_more]
  ]
]

// H4: what this scan did not review, by pass and by path (`report_export::coverage_gaps`, the
// same data the workbook's Not checked sheet and findings.json carry). A failed AI pass names how
// many files it did not review; a skipped file is listed by path, the first few only. The rest
// are counted and the Not checked sheet lists them all.
#let gaps = d.coverage_gaps
#if gaps.failed_ai_passes.len() > 0 or gaps.dependency_gaps.len() > 0 or gaps.skipped_files.len() > 0 or gaps.skipped_beyond_cap_total > 0 or gaps.file_limit_notes.len() > 0 or "skipped_summary" in gaps or "evidence_note" in gaps or "scope_limited_note" in gaps [
  #v(0.2cm)
  #card[
    #subheading[Coverage, not reviewed this scan]
    #v(0.1cm)
    #for line in gaps.failed_ai_passes [
      - #line
    ]
    #for line in gaps.dependency_gaps [
      - #line
    ]
    // M5: the file limit as a limit on the run, then the count of every file not read, then each
    // file with its size and reason.
    #for line in gaps.file_limit_notes [
      - #line
    ]
    #if "skipped_summary" in gaps [
      - #gaps.skipped_summary
    ]
    #if "evidence_note" in gaps [
      - #gaps.evidence_note
    ]
    #let shown = calc.min(gaps.skipped_files.len(), W.skipped_files_cap)
    #for skipped in gaps.skipped_files.slice(0, shown) [
      - #skipped.repo, #breakable(skipped.path, n: 50)#if skipped.size != "" [ (#skipped.size)]: #skipped.reason
    ]
    #if gaps.skipped_files.len() > shown [
      - and #str(gaps.skipped_files.len() - shown) #W.skipped_files_more
    ]
    #if "skipped_beyond_cap_note" in gaps [
      - #gaps.skipped_beyond_cap_note
    ]
    #if "scope_limited_note" in gaps [
      - #gaps.scope_limited_note
    ]
  ]
]

// ── 15. Disclaimer ───────────────────────────────────────────────────────────────────────
#v(0.6cm)
#section("Terms", sec(14))

#card[
  #d.disclaimer
]

// P6 (2026-09-29), promise 1 ("one person reads every finding and signs the report"): a
// signature block naming the real preparer. `d.cover.prepared_by` is resolved server-side
// (`report_export::resolve_prepared_by`) with a real default name as its final fallback — it
// is NEVER blank, so this block always names an actual person, never "N/A".
//
// W5 (2026-09-30): gated on `is_draft` (the exact same `d.review_state` signal the draft banner
// reads) so the banner and this block can never disagree: a reviewed run keeps the real
// name+date sign-off below; a raw/unreviewed draft gets an explicit unreviewed-draft label.
#v(0.5cm)
#block(stroke: 0.75pt + line_strong, inset: 12pt, radius: 8pt, width: 100%)[
  #text(size: 9pt)[
    #if is_draft [
      Prepared by: #text(weight: 600, fill: ink)[#d.cover.prepared_by] (unreviewed draft, not signed off)
    ] else [
      Prepared and signed off by: #text(weight: 600, fill: ink)[#d.cover.prepared_by]
      #linebreak()
      #text(fill: ink_faint)[#d.cover.generated_at]
    ]
  ]
]
