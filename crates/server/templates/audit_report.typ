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

// One report section: the eyebrow, then the level-1 heading the contents page lists.
#let section(label, title) = {
  eyebrow(label)
  heading(level: 1, title)
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

// The card's location strip: the severity chip, then `repo / path, line N` in mono, ink-faint,
// on a 3.5% ink tint with a 4 pt radius (the site's report-excerpt shape).
#let location_strip(sev, repo, path, line) = block(fill: tint, radius: 4pt, inset: (x: 6pt, y: 4pt), width: 100%, below: 8pt)[
  #severity_chip(sev) #h(4pt) #text(font: mono, size: 8.5pt, fill: ink_faint)[#repo / #breakable(path), line #str(line)]
]

// One curated-finding site, as a finding card — factored out so the FIRST site of a group can
// be kept together with its heading (S4) while later sites are free to break across pages.
//
// FIX 8 (2026-09-13 review): "the plain-English headline leads; the engineer-normative rule
// title renders smaller/grey BENEATH it." `site.headline` was already the primary bold
// heading (Item 1, above) — but for a group's FIRST site, the once-per-group `rule_id: title`
// subtitle used to render ABOVE it (see the curated-findings loop below), so the reading order
// put the engineer-facing rule id first after all. `after_headline` lets the caller inject
// that subtitle (plus the citation) directly UNDER this site's headline instead, so it always
// reads headline-first regardless of which site in the group is rendering.
//
// W27 card order (brand guide, "Finding card"): location strip, headline, impact, a divider,
// the "Fix:" line in accent, then the card footer in mono ink-faint.
#let render_site(site, after_headline: none) = {
  block(above: 10pt, below: 0pt, card[
    #location_strip(site.severity, site.repo, site.path, site.line)
    // Item 1: the DEFECT at this object is the primary, bold heading — never the rule's own
    // invariant title (that reads as a clean bill of health out of context).
    #subheading[#site.headline]
    #if after_headline != none [
      #v(2pt)
      #after_headline
    ]
    #if site.snippet != "" [
      #block(fill: tint, inset: 6pt, radius: 4pt, width: 100%)[#breakable(site.snippet, n: 70)]
    ]
    #site.detail
    // FIX 1 contract (report_export.rs's `CuratedSiteJson.fix`): `Option<String>` serializes
    // to JSON `null`, which Typst's `json()` parses as `none` — NOT an empty string. Every
    // check below must be `!= none`; a `!= ""` check would let a `none` value fall into the
    // branch and render a bare "Fix:" label with nothing after it.
    //
    // P2 (2026-09-29): `fix_for_this_finding` (the codebase-specific fix — see that field's
    // doc comment in report_export.rs) is now the PRIMARY "Fix:" line whenever it's present;
    // the rule's generic authored `fix` demotes to a secondary "General guidance:" line
    // underneath it. When fix-generation never ran or gave up (`fix_for_this_finding ==
    // none` — a deterministic-only scan, or a needs-review "fix not generated" finding), the
    // generic `fix` — if any — is still shown, as the ONLY "Fix:" line, exactly like before
    // this pass. Nothing renders when BOTH are `none`.
    #if site.fix_for_this_finding != none or site.fix != none [
      #line(length: 100%, stroke: 0.5pt + line_c)
    ]
    #if site.fix_for_this_finding != none [
      #text(weight: 600, fill: accent)[Fix: ]#fix_body(site.fix_for_this_finding)
      #if site.fix != none [
        #v(0.05cm)
        #text(size: 8.5pt, fill: ink_faint)[General guidance: #fix_body(site.fix)]
      ]
    ] else if site.fix != none [
      #text(weight: 600, fill: accent)[Fix: ]#fix_body(site.fix)
    ]
    #v(2pt)
    #text(font: mono, size: 8pt, fill: ink_faint)[Effort: #or_not_estimated(site.effort) · Confidence: #or_not_evaluated(site.confidence) · #site.disposition]
    // C4-P4 (residual defect 2): human-readable TITLES, never raw rule ids — see
    // `report_export::also_matches_titles`'s doc comment.
    #if site.also_matches_titles.len() > 0 [
      #linebreak()
      #text(size: 8pt, fill: ink_faint)[Also violates: #site.also_matches_titles.join(", ")]
    ]
    // M2/M8: when a rule's ceiling, floor or the unlocated safety net moved this row's
    // severity, say what it was and why. Prints nothing for a row no bound moved.
    #if site.severity_adjustments.len() > 0 [
      #for note in site.severity_adjustments [
        #linebreak()
        #text(size: 8pt, fill: ink_faint)[#note]
      ]
    ]
    // H6: every site a merge folded into this row, so a row that stands for several places
    // says so. The first ten are named; the rest are counted and point to the workbook, which
    // lists them all.
    #if site.also_sites.len() > 0 [
      #linebreak()
      #text(size: 8pt, fill: ink_faint)[Also found at #str(site.also_sites.len()) other #plural(site.also_sites.len(), "site", "sites"):]
      #for also in site.also_sites.slice(0, calc.min(site.also_sites.len(), 10)) [
        #linebreak()
        #text(font: mono, size: 8pt, fill: ink_faint)[#also.repo / #breakable(also.path, n: 60), line #str(also.line)]
        #if also.title != none [#text(size: 8pt, fill: ink_faint)[(#also.title)]] else if also.headline != none [#text(size: 8pt, fill: ink_faint)[(#also.headline)]]
        #if also.severity != none [#text(size: 8pt, fill: ink_faint)[(rated #also.severity)]]
        #if also.consequence [#text(size: 8pt, fill: ink_faint)[(also affected)]]
      ]
      #if site.also_sites.len() > 10 [
        #linebreak()
        #text(size: 8pt, fill: ink_faint)[and #str(site.also_sites.len() - 10) more, listed in full in the workbook and findings.json]
      ]
    ]
    // Members of a merge flagged at this row's own place are the same site, not another one.
    #if site.also_flagged_here.len() > 0 [
      #for note in site.also_flagged_here [
        #linebreak()
        #text(size: 8pt, fill: ink_faint)[#note]
      ]
    ]
    // A credential another repo of the project also flags under the same name. Display only:
    // each service's row stays its own.
    #if site.same_credential_note != "" [
      #linebreak()
      #text(size: 8pt, fill: ink_faint)[#site.same_credential_note]
    ]
  ])
}

// ── 1. Cover ─────────────────────────────────────────────────────────────
// Branding (2026-09-13 review): the cover title is `brand_title` (computed above from the
// resolved `d.cover.brand`, never a hardcoded agency or "Camerata" literal), and the old
// scan-category qualifier ("Brown" + "field") is gone from it entirely. The two model-name
// rows that used to sit in this table (which model audited, which model calibrated) plus the
// Camerata-version row have moved off the cover into Methodology further down (see "Scanned
// with Camerata v..." there): Camerata is the auditing INSTRUMENT, legitimately named where
// methodology is discussed, not on the client-facing cover.
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

// ── ToC (nice-to-have): one line of Typst, clickable bookmarks in every viewer ───────
#eyebrow("Contents")
#outline(title: none, depth: 1)

#pagebreak()

// ── 2. Executive summary ──────────────────────────────────────────────────
#section("Summary", [Executive summary])

#d.executive_summary.narrative

#if d.executive_summary.is_override [
  #text(size: 8.5pt, style: "italic", fill: ink_faint)[Summary text supplied by the reviewer.]
]

// W6: a failed/timed-out pass must be disclosed, never omitted silently. Rendered here
// (the summary) AND again in Methodology & limitations below — the same strings both times,
// see `report_export::failed_pass_disclosures`. Gated on non-empty so a clean run renders
// nothing extra.
#if d.executive_summary.failed_passes.len() > 0 [
  #block(above: 8pt, below: 4pt, stroke: 0.75pt + vivid, inset: 10pt, radius: 8pt, width: 100%)[
    #text(weight: 600, size: 9pt, fill: ink)[Not run this inspection]
    #for note in d.executive_summary.failed_passes [
      - #note
    ]
  ]
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
#v(0.4cm)
#section("Priorities", [If you only do three things this week])

#if d.three_things.items.len() == 0 [
  No do-now items this run.
] else [
  #panel[
    #for item in d.three_things.items [
      #block(above: 4pt, below: 10pt)[
        #severity_chip(item.severity) #h(2pt) #text(weight: 600, fill: ink)[#item.headline]
        #linebreak()
        #text(font: mono, size: 8pt, fill: ink_faint)[
          #item.repo / #breakable(item.path), line #str(item.line) (#item.rule_id)
        ]
        #linebreak()
        #text(size: 9pt)[Rough estimate: #item.hours_label]
        // FIX 7 (2026-09-13 review): one factual sentence of what the exposure MEANS, when
        // one is authored for this rule (`report_export::business_impact_for_rule`) — omitted
        // gracefully (no blank line, no "impact unknown" filler) when it isn't.
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

#pagebreak()

// ── 3. Category scorecard ─────────────────────────────────────────────────
#section("Scorecard", [Category scorecard])

#if d.scorecard.rows.len() == 0 [
  No findings were produced by any inspected category.
] else [
  #table(
    // Item 6: the critical/high/medium/low columns get a FIXED width (not "auto") because
    // heat_cell's box is itself fixed-width — an "auto" column sized against a 100%-relative
    // child creates a circular sizing problem that corrupts the whole table's row layout.
    // FIX 5 (2026-09-13 review): the "checked/clean" column is now spelled out in words
    // ("N checked, M clean") rather than a bare "N/M" fraction that reads as a grade — widened
    // from `auto` to a fixed width to keep that longer text from cramping the table.
    columns: (1.3fr, 2.1cm, 2.1cm, 2.1cm, 2.1cm, 2.9cm, 1.1fr),
    stroke: table_stroke,
    inset: (x: 4pt, y: 6pt),
    align: horizon,
    table.header(th[Category], th[Critical], th[High], th[Medium], th[Low], th[Rules checked · Rules clean], th[Status]),
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
#section("Prioritization", [Severity × effort matrix])

#let effort_col_label(key) = {
  if key == "low" { "Low effort" }
  else if key == "medium" { "Medium effort" }
  else if key == "high" { "High effort" }
  else { "Not yet estimated" }
}

// C4-R3: a cell with no findings in it used to render the literal word "None" — read by a
// non-engineer client as something having GONE WRONG (a stringified absent value), not as the
// intended "nothing landed in this severity x effort bucket" empty state. An empty cell now
// renders as blank — the row/column headers already say which bucket it is, so a blank cell at
// their intersection reads as "zero" without needing a word for it (and this house style never
// uses an em/en dash as a placeholder either, see `or_na` above).
#let grid_cell_content(cell) = {
  if cell.findings.len() == 0 [] else [
    #for f in cell.findings [
      #block(above: 2pt, below: 6pt)[
        #text(size: 8pt, fill: ink)[#f.headline]
        #linebreak()
        #text(font: mono, size: 7.5pt, fill: ink_faint)[#f.repo / #breakable(f.path, n: 24), line #str(f.line) (#f.rule_id)]
      ]
    ]
  ]
}

#if d.priority_grid.rows.len() == 0 [
  No do-now, do-next, or plan findings this run.
] else [
  #table(
    columns: (auto,) + d.priority_grid.columns.map(c => 1fr),
    stroke: table_stroke,
    inset: (x: 5pt, y: 6pt),
    table.header(th[Severity], ..d.priority_grid.columns.map(c => th[#effort_col_label(c)])),
    ..d.priority_grid.rows.map(row => (
      [#severity_chip(row.severity)],
      ..row.cells.map(cell => grid_cell_content(cell)),
    )).flatten()
  )
]

#if d.priority_grid.accepted_count > 0 or d.priority_grid.informational_count > 0 [
  #v(0.3cm)
  #text(size: 8.5pt, fill: ink_faint)[
    #if d.priority_grid.accepted_count > 0 [
      #str(d.priority_grid.accepted_count) #plural(d.priority_grid.accepted_count, "finding is", "findings are") accepted risk and out of scope for this grid (see Curated findings).
    ]
    #if d.priority_grid.informational_count > 0 [
      #str(d.priority_grid.informational_count) further #plural(d.priority_grid.informational_count, "item is", "items are") informational and out of scope for this grid.
    ]
  ]
]

#pagebreak()

// One curated-findings GROUP (one rule, its citation/title header, and every site it covers) —
// factored out so "Curated findings" and the separate "Held for review" section (C4-P4 residual
// defect 5) render identically instead of maintaining two copies of this block.
//
// S4: keep the group heading + citation + FIRST site together (orphan control) — later sites in
// the same group are free to break across pages normally.
//
// FIX 8 (2026-09-13 review): the BOLD, primary heading for each finding is the per-site defect
// headline (`render_site`'s own `site.headline`, Item 1) and now LEADS the block outright — the
// rule id + its own invariant title, plus the citation, render as a small gray SUBTITLE directly
// BENEATH that headline (via `render_site`'s `after_headline`), kept for registry traceability,
// never as the thing a reader sees first.
#let render_curated_group(group) = {
  block(breakable: false)[
    #block(above: 12pt, below: 2pt)[
      #if group.sites.len() > 0 [
        #render_site(group.sites.at(0), after_headline: [
          #text(size: 8.5pt, fill: ink_faint)[#group.rule_id: #group.title] #text(size: 8.5pt, fill: ink_faint)[(#str(group.sites.len()) #plural(group.sites.len(), "site", "sites"))]
          // Item 5: show the citation ONCE. When real external sources exist, the bulleted
          // title+URL list IS the citation — the run-on `citation.label` (a redundant join of
          // those same titles) is dropped. `label` is shown only when there are no external
          // sources to bullet (the advisory/preview case), where it is the sole honesty note.
          #if group.citation.sources.len() > 0 [
            // H5: a keyword-matched standard is labelled as related, never as a rule check.
            #if group.citation.kind == "related" [
              #text(size: 8.5pt, fill: rgb("#555555"))[Related standard (matched by keyword, not a rule check):]
            ]
            #for s in group.citation.sources [
              #text(size: 8.5pt, fill: ink_faint)[- #s.title #if s.url != "" [(#s.url)]]
            ]
          ] else [
            #text(size: 8.5pt, fill: ink_faint)[#group.citation.label]
          ]
        ])
      ]
    ]
  ]
  for site in group.sites.slice(1) [
    #render_site(site)
  ]
}

// ── 5. Curated findings ───────────────────────────────────────────────────
// C4-P4 (residual defect 5): `d.curated_findings` is already ordered by the server (severity
// desc, then bucket do_now -> do_next -> plan -> accepted, then confidence) — see
// `report_export::curated_group_sort_key` — never re-sorted (alphabetically or otherwise) here.
#section("Findings", [Curated findings])

#if d.curated_findings.len() == 0 [
  No open code findings survived triage.
] else [
  #for group in d.curated_findings [
    #render_curated_group(group)
  ]
]

#pagebreak()

// ── Held for review ─────────────────────────────────────────────────────────
// C4-P4 (residual defect 5) / C5-4: a group whose sites are ALL informational (the severity-
// capped, <= medium conventions-to-consider appendix) or ALL held (a calibration hedge at ANY
// severity, including high/critical — see report_export::is_held_for_review) never renders under
// "Curated findings" — that heading promises action items. Both route here, matching the
// executive summary's own "held for a human reviewer's judgment call" framing. `render_site`
// renders each site's own real severity via `severity_chip` and its own `disposition` label
// (computed server-side, distinct wording for "informational" vs "held") — no bucket-specific
// branching needed here. Same row-level content, own heading, own page, strictly AFTER every
// curated finding above.
#section("Review", [Held for review])

#if d.held_for_review_findings.len() == 0 [
  No findings held for a reviewer's judgment call this run.
] else [
  #text(size: 9pt, style: "italic", fill: ink_faint)[
    Held for a human reviewer's judgment call — not an open action item, but worth a second \
    look.
  ]
  #v(4pt)
  #for group in d.held_for_review_findings [
    #render_curated_group(group)
  ]
]

// ── Patterns across services ──────────────────────────────────────────────────────────
// Present only for a project of more than one repo. An observation after the findings: no
// severity glyph, no chip, in no total (`report_export::PatternsAcrossServicesJson`). A cell
// that is not applicable or not reviewed is printed as such, never as clean.
#if d.patterns_across_services != none [
  #pagebreak()
  #section("Observations", [Patterns across services])
  #text(size: 9pt, style: "italic", fill: ink_faint)[#d.patterns_across_services.note]
  #v(6pt)
  #if d.patterns_across_services.rows.len() == 0 [
    No rule was found violated in one service and clean in another.
  ] else [
    #for row in d.patterns_across_services.rows [
      #block(breakable: false, below: 10pt)[
        #text(weight: 600, fill: ink)[#row.title]
        #v(2pt)
        #text(size: 9.5pt)[#row.statement]
        #v(3pt)
        #table(
          columns: (auto, 1fr),
          stroke: table_stroke,
          inset: (x: 5pt, y: 4pt),
          table.header(th[Service], th[Result]),
          ..row.cells.map(cell => (
            [#cell.repo],
            [#cell.label],
          )).flatten()
        )
      ]
    ]
    #for theme in d.patterns_across_services.themes [
      #block(below: 6pt)[
        #text(weight: 600, fill: ink)[#theme.name]: #theme.note
      ]
    ]
  ]
]

#pagebreak()

// ── 6 + 7. What's healthy + Dependency snapshot (S1: share a page — each was ~85-90% ──
// blank on its own) ─────────────────────────────────────────────────────────────────
#section("Strengths", [What's healthy])

#if d.whats_healthy.rules.len() == 0 and not d.whats_healthy.dependency_clean [
  No zero-finding rules to report this run.
] else [
  #if d.whats_healthy.dependency_clean [
    - No known vulnerable dependencies detected in this scan.
  ]
  #for r in d.whats_healthy.rules [
    - *#r.rule_id*: #r.title #if r.citation.kind == "grounded" [#text(fill: ink_faint, size: 8.5pt)[(#r.citation.label)]]#if r.clean_in.len() > 0 [ #text(fill: ink_faint, size: 8.5pt)[Clean in #r.clean_in.join(", ")#if r.not_applicable_in.len() > 0 [; not applicable in #r.not_applicable_in.join(", ")].]]
  ]
  #if d.whats_healthy.further_clean_count > 0 [
    #text(size: 8.5pt, fill: ink_faint)[#str(d.whats_healthy.further_clean_count) further #plural(d.whats_healthy.further_clean_count, "rule", "rules") verified clean this run.]
  ]
  #text(size: 8.5pt, style: "italic", fill: ink_faint)[
    Verified absent in THIS scan, not a guarantee against future regressions.
  ]
]

#v(0.6cm)
#section("Dependencies", [Dependency & CVE snapshot])

#if d.dependency_snapshot.clean [
  No known vulnerable dependencies detected.
] else if d.dependency_snapshot.rows.len() == 0 [
  // M11: no rows is not the same as clean. The audit did not run, or left manifests out, so
  // this section cannot say that no dependency is vulnerable.
  Dependencies were not fully checked in this scan, so this report cannot say that none are vulnerable. The notes below say what was not checked.
] else [
  #table(
    columns: (1fr, auto, 2fr, auto),
    stroke: table_stroke,
    inset: (x: 5pt, y: 6pt),
    table.header(th[Package], th[Repo], th[Advisory], th[Severity]),
    ..d.dependency_snapshot.rows.map(row => (
      [#breakable(row.package, n: 24)],
      [#row.repo],
      [#row.advisory],
      [#severity_chip(row.severity)],
    )).flatten()
  )
]

#for note in d.dependency_snapshot.coverage_notes [
  #block(above: 4pt, below: 4pt)[
    #text(size: 8.5pt, fill: ink_faint)[Coverage note: #note]
  ]
]

#pagebreak()

// ── Next steps (FIX 3, 2026-09-13 review) ───────────────────────────────────────────
// A short, FACTUAL "what happens after this report" paragraph, opening the ladder before the
// reader gets to Methodology's more technical detail. `d.methodology.next_steps` is authored
// prose (`report_export::next_steps_note`, which also computes and states the run's real
// numeric fix rate per promise 2), never an LLM call, same pattern as the deterministic/ai-tier
// notes below.
#section("After this report", [Next steps])

#d.methodology.next_steps

// ── 8 + 9. Methodology & limitations + Disclaimer (S1: share a page) ────────────────
#v(0.6cm)
#section("How this was done", [Methodology & limitations])

#d.methodology.deterministic_note

#d.methodology.ai_tier_note

#d.methodology.citation_note

// Branding (2026-09-13 review): Audit model / Calibration model / Camerata version used to sit
// on the cover; they now render here instead. Camerata is the inspecting INSTRUMENT,
// legitimately named where methodology is discussed, never on the client-facing cover (see
// `brand_title` above) and never a substitute for the agency's own resolved brand.
Scanned with Camerata v#or_na(d.cover.camerata_version). Inspection model: #or_na(d.cover.audit_model). Calibration model: #or_na(d.cover.calibration_model). Model backend: #or_na(d.cover.model_backend).

// H4: the exact engine build and rule library behind this report, the same values the README,
// manifest.json and findings.json state. Each line renders only when the value was recorded.
#if d.cover.engine_commit != none [Engine commit: #raw(d.cover.engine_commit). ]#if d.cover.corpus_version != none [Rule library lock hash: #raw(d.cover.corpus_version).]

// P5: this reconciling line must never claim a human "reviewed"/"dispositioned" a finding
// unless `d.review_state == "reviewed"` (at least one finding this session carries an explicit
// auditor disposition — see `report_export::ReviewState`'s doc comment). The two branches also
// each show EVERY number in the partition (curated total is implied by the do-now/next/plan/
// accepted breakdown above; this line adds the two counts that used to be invisible: held for
// review and false positives), so `candidates_reviewed` always reconciles on the page.
#if d.review_state == "reviewed" [
  *#str(d.methodology.candidates_reviewed) candidate #plural(d.methodology.candidates_reviewed, "finding", "findings") reviewed by a human reviewer; #str(d.methodology.excluded_false_positive) dispositioned as false positives and excluded; #str(d.methodology.held_for_review) held for further review.*
] else [
  *#str(d.methodology.candidates_reviewed) candidate #plural(d.methodology.candidates_reviewed, "finding", "findings") produced by the engine; #str(d.methodology.held_for_review) held for a human reviewer's judgment call; #str(d.methodology.excluded_false_positive) auto-excluded as likely false positives. This export has not yet had a human triage pass.*
]

#d.methodology.severity_scale_note

Not performed in this engagement:
#for item in d.methodology.not_done [
  - #item
]

// W6: a failed/timed-out pass must be disclosed, never omitted silently — same strings as
// the Executive summary's box above (see `report_export::failed_pass_disclosures`), restated
// here for the more technical Methodology reader. Gated on non-empty.
#if d.methodology.failed_passes.len() > 0 [
  #v(0.2cm)
  Not run this inspection:
  #for note in d.methodology.failed_passes [
    - #note
  ]
]

// H4: what this scan did not review, by pass and by path (`report_export::coverage_gaps`, the
// same data the workbook's Coverage sheet and findings.json carry). A failed AI pass names how
// many files it did not review; a skipped file is listed by path. The PDF lists the first 100
// skipped files and points to the full list, which itself holds the first 1,000 per reason; the
// rest are counted ("and K more").
#let gaps = d.coverage_gaps
#if gaps.failed_ai_passes.len() > 0 or gaps.dependency_gaps.len() > 0 or gaps.skipped_files.len() > 0 or gaps.skipped_beyond_cap_total > 0 or "scope_limited_note" in gaps [
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
    #let shown = calc.min(gaps.skipped_files.len(), 100)
    #for skipped in gaps.skipped_files.slice(0, shown) [
      - #skipped.repo, #raw(skipped.path): #skipped.reason
    ]
    #if gaps.skipped_files.len() > shown [
      - and #str(gaps.skipped_files.len() - shown) more skipped files, listed in full on the workbook's Coverage sheet and in findings.json
    ]
    #if "skipped_beyond_cap_note" in gaps [
      - #gaps.skipped_beyond_cap_note
    ]
    #if "scope_limited_note" in gaps [
      - #gaps.scope_limited_note
    ]
  ]
]

#v(0.6cm)
#section("Terms", [Disclaimer])

#card[
  #d.disclaimer
]

// P6 (2026-09-29), promise 1 ("one person reads every finding and signs the report"): a
// signature block naming the real preparer. `d.cover.prepared_by` is resolved server-side
// (`report_export::resolve_prepared_by`) with a real default name as its final fallback — it
// is NEVER blank, so this block always names an actual person, never "N/A".
//
// W5 (2026-09-30): this block used to render the same NAME-plus-DATE attestation no matter
// what, including on a raw (nobody-has-looked-at-this-yet) export — contradicting the DRAFT
// banner above and reading to a client as a completed human sign-off that never happened. Now
// gated on `is_draft` (the exact same `d.review_state` signal the draft banner reads, defined
// once near the top of this file) so the banner and this block can never disagree again: a
// reviewed run keeps the real name+date sign-off below; a raw/unreviewed draft gets no
// name+date attestation at all, just an explicit unreviewed-draft label.
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
