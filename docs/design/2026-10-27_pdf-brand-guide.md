# PDF brand guide (W27 PDF rebuild)

Source: the Cantus Works website draft (Zach, 2026-10-27), claude.ai artifact
`RdUkD8YE1nH1Xvys1JaCvL`. It is not the final site, but the final styling will closely match it,
so the W27 PDF follows it. When the site changes, update this file, not the renderer constants
alone.

## Logo

- `docs/design/brand/cantus-logo-mark.svg`: full mark (three open arcs in ink `#0F1826`, one
  horizontal bar in vivid blue `#0C22F2`). Use on the cover and the first-page header.
- `docs/design/brand/cantus-logo-mark-small.svg`: two-arc version with heavier strokes, for small
  sizes (running headers, under about 20 px).
- Wordmark: "CANTUS WORKS", weight 600, letter-spacing 0.14em, uppercase, ink colour, set to the
  right of the mark with a gap of about a third of the mark's width.

## Colour

| Token | Hex | PDF use |
|---|---|---|
| ink | `#0F1826` | headings, body emphasis, logo arcs |
| ink-soft | `#47536A` | body text |
| ink-faint | `#6B7690` | captions, file locations, footers, page numbers |
| accent | `#2B5580` | section eyebrows, "Fix:" label, links |
| accent-deep | `#1B3A56` | table header text |
| vivid | `#0C22F2` | Critical severity, logo bar, one accent rule per page at most |
| vivid-mist | `#E5E8FE` | Critical severity chip fill |
| bg | `#F7F7F7` | page tint behind cards (optional; plain white prints better) |
| bg-blue | `#E9F0FA` | highlighted panels (summary box, "how to read this report") |
| card | `#FFFFFF` | finding cards |
| line | ink at 8% (`#0F1826` 0.08) | card borders, dividers |
| line-strong | ink at 14% | table rules |

Vivid blue is the only saturated colour. Severity other than Critical uses neutral chips (ink-soft
text on ink at 4.5%), so the page is not a rainbow. If High needs separating from Medium in print,
use accent `#2B5580` for High and keep Medium and Low neutral. Check contrast for text on
vivid-mist (vivid on vivid-mist passes AA at 11 pt bold).

## Type

The site uses the Apple system font stack (SF Pro, SF Mono). SF fonts may not be embedded in a
PDF sent to clients, so the PDF needs an embeddable equivalent. **Decided (Zach, 2026-10-27):**
Inter for body and headings, JetBrains Mono for file locations, ids and code. Both are under the
SIL Open Font License (embedding allowed); ship the license files with the fonts.

| Element | Size | Weight | Tracking | Colour |
|---|---|---|---|---|
| Cover title | 30 pt | 600 | -0.03em | ink |
| Section heading (h2) | 20 pt | 600 | -0.024em | ink |
| Sub-heading (h3) | 13 pt | 600 | -0.012em | ink |
| Eyebrow (section label) | 8.5 pt, uppercase | 600 | 0.09em | accent, with a 4 pt vivid dot before it |
| Body | 10 pt, line height 1.6 | 400 | 0 | ink-soft |
| Finding location | 8.5 pt mono | 400 | 0 | ink-faint, on a 3.5% ink tint strip with 4 pt radius |
| Severity chip | 8 pt | 600 | 0.04em | see Colour; pill shape |
| Footer, page number | 8 pt mono | 400 | 0 | ink-faint |

## Finding card (matches the site's report excerpt)

1. Location strip: severity chip, then `path, line N` in mono.
2. Impact paragraph in body style.
3. Divider (line colour), then "**Fix:**" in accent bold followed by the fix in body style.
4. Card footer in mono ink-faint: "Confidence: <level> · reviewed by <reviewer>".
White card, 1 px line border, 8 pt radius, no heavy shadows (print).

## Layout

- 8 pt spacing scale (4, 8, 12, 16, 24, 32, 48).
- Generous margins; one column for prose, the finding card at full text width.
- Every page carries the confidentiality line from the 2026-10-24 decision ("Confidential,
  prepared for <client> on <date>") in the footer, ink-faint mono, plus the page number.
- Voice matches the site: plain sentences, no em-dashes, no hype.
