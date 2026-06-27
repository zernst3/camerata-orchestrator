# Camerata UI Redesign

> **Living doc.** The redesign is **vibe-first**: Gemini mockups (HTML/CSS) set the
> *aesthetic and layout orientation*; Claude translates that into real Dioxus against our
> actual components, content, and data — taking per-screen layout liberty where real content
> demands it. **Gemini owns the look; our architecture wins ties.**
>
> Most of this doc is "translate the vibe." Sections under **Structural changes** are the
> specific layout moves Zach explicitly wants, captured with enough detail to build to.

## The translation contract

- **From the mockup, extract:** palette/tokens, typography scale, spacing rhythm, radius,
  borders/shadows, component treatments, **and layout orientation** (e.g. header-nav → sidebar,
  modal → side panel).
- **Discard:** Gemini's placeholder copy, fake data, framework/markup, and any logic.
- **Replace with:** our real routes, components, `use_signal`/`use_resource` wiring, and the
  content we already built. Where real content doesn't fit the mockup's shape, **adapt the
  layout and flag the deviation** rather than contort real code to a pixel.

## Global vibe (read from the first mockup — finalize exact tokens from Gemini's CSS)

- **Theme:** deep navy gradient background; surfaces a shade lighter with subtle 1px
  translucent borders; soft elevation; generous, airy padding; rounded corners (~14–16px).
- **Accent:** blue (~`#3B82F6`) for checkboxes, selected radios, and **Rule-ID pills**
  (monospace blue text on a translucent-blue fill with a 1px blue border, rounded).
- **Type:** geometric/rounded sans for headers; regular sans for body.
- **Text:** near-white primary; muted blue-grey body (~`#8A9AB5`); **section labels are
  UPPERCASE, letter-spaced, small, muted** (e.g. "RULE DETAIL & CONFIGURATION").
- **Selected row:** translucent blue fill + subtle border/glow (not just a hover tint).
- **Buttons:** translucent dark surface, 1px border, white label, rounded.

> Treat the hexes above as approximate until the mockup CSS lands; pull the real custom
> properties from it and map them to our Tailwind tokens / theme.

---

## Structural changes

### SC-1 — Rules step → master-detail (table beside a persistent detail panel)

**The change.** On the onboarding **"Step 1 — Proposed starter ruleset"** screen, the rule
**detail & configuration no longer pops up as a modal over the page.** Instead it lives in a
**persistent right-hand panel, side-by-side with the table.** Selecting a table row updates the
panel in place. Master (table) left, detail (panel) right.

**Why Zach wants it.** No overlay context-switch; you scan the table and configure the selected
rule at the same time; far better for picking/tuning a whole ruleset in one pass.

**Layout.**
- Two columns inside the step card: **left = rules table** (scrollable master), **right = rule
  detail & configuration** (persistent).
- **Bottom bar (full width):** the **Custom Rules** note + `+ Custom rule (this repo)` and
  `+ Global custom rule` buttons (right-aligned).
- Top: `Repo ruleset:` selector unchanged in spirit.

**Table — structural decisions to settle (liberty is mine; these are the open questions):**
- **Columns:** `Enforce` (checkbox) · `Rule ID` (pill) · `Description`. Decide:
  - Does `Description` show **title + truncated sub-text** (full detail now lives in the panel),
    or title-only to tighten rows? Leaning **title + 1-line clamp**, full text in the panel.
  - Add columns for our real data? Candidates: **enforcement tier** (mechanical/architectural/
    structured/prose), **verification badge** (verified/grounded/draft), **scope** (project/repo).
    Likely surface **tier** as a small chip; keep the row lean, push the rest to the panel.
- **Column sizing:** `Enforce` fixed-narrow; `Rule ID` content-width (pills are uniform-ish);
  `Description` flex-fill. Settle exact widths during build.
- **Row state:** selected row = translucent blue fill + left accent (per vibe); checkbox is
  independent of selection (you can select a row to view it without enforcing it).

**Right panel — content (our real rule fields):**
- Rule **title** + **Rule-ID pill**, **description**.
- **ALTERNATIVE CONFIGURATIONS** (radio list) — maps to our rule **options/alternatives**
  (e.g. `opt-wrap-all` vs `opt-wrap-critical`); the chosen option is what gets enforced.
- Room for the rest of our per-rule data as needed: **enforcement tier**, **verification
  badge**, **opt-in-only** notice, **source/grounding** citation, scope (project/repo).

**Chorale (our table lib) — flexibility this requires:**
- **Selection-driven master-detail**: a controlled "selected row" that an external panel reads.
- **Custom cell renderers**: checkbox cell, pill cell, title+sub cell, tier chip.
- **Flexible column sizing** (fixed / content / flex) + a scroll container.
- These are reasonable table-lib capabilities; **confirm Chorale exposes selection state + custom
  cell render**, and if not, that's a Chorale task, not a workaround in the page.

**Translation notes (vibe → real Dioxus):** keep our existing rules data/handlers; reshape the
*presentation* into the two-column master-detail and move the modal's body into the right panel.
The modal component is retired for this screen (or repurposed as the panel's inner layout).

---

## Interaction patterns (cross-cutting)

### P-1 — Progressive disclosure of necessary-but-noisy text (the "changed" info icon)

**Problem.** Onboarding (and other config-heavy screens) carries a **lot of explanatory text**.
It's all *needed*, but shown inline all at once it's noise that buries the controls.

**Pattern.** Collapse secondary explanatory text behind a small **info icon (ⓘ)**; the user
**reveals it on hover / focus / tap** (tooltip or popover). The text isn't removed — it's moved
out of the default visual field so the controls breathe.

**The "it changed" signal (the part Zach wants).** When a setting change makes an explanation
**newly relevant or changes its content**, the icon **flickers and shifts color** to cue "this
changed — hover to see what it now says." So users aren't told to re-read a wall of text; the UI
points them only at what moved.

**Make it robust (so it's a cue, not a gimmick):**
- **Persist the changed-state until acknowledged.** Pair the transient flicker with a
  **persistent tint / dot** that stays until the user actually hovers/opens it — a flicker alone
  is missable. Clears on view.
- **Accessibility:** hover-only excludes touch + keyboard — also open on **focus and tap**, and
  the changed-state must be conveyed **non-animated too** (color + a dot/badge), gated by
  `prefers-reduced-motion` (no flicker for users who opt out).
- **Reserve it for *secondary* text.** Orientation-critical copy (the step's one-line intent)
  **stays visible**; only the deeper "why / how / implications" detail collapses behind the icon.
  Judgment call per field; err toward keeping one guiding line and hiding the paragraph.

**Where it applies:** per-field help in onboarding, rule/config explanations, the "what this
setting does" paragraphs. Composes with **SC-1**: long rule descriptions live in the detail
panel; this icon handles the smaller inline helper texts that remain.

**Build note:** a single reusable `InfoHint { text, changed: Signal<bool> }`-style component —
parent flips `changed` when the relevant setting mutates; the component owns the flicker/tint/
reduced-motion/acknowledge logic.

---

## Backlog of structural changes (add as mockups arrive)

- **SC-2 — (next):** e.g. in-project nav header → sidebar (per Zach's example) — capture when the
  mockup lands.
