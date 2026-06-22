# USER_GUIDE.md refresh — CI security rules, scan-time preview, scan selector, UoW additions

**Date:** 2026-06-22 · **Confined to:** `docs/USER_GUIDE.md` (+ this doc).

Brought `docs/USER_GUIDE.md` current with the features shipped since the 2026-06-21 pass. Every claim
was verified against the shipped code (`crates/server`, `crates/ui/src/cockpit.rs`, `crates/rules`,
`crates/checks`) before being written; the 2026-06-21/2026-06-22 decision docs were the starting
source but the code was the final arbiter.

## Sections added / updated

- **Status block** — rewritten to enumerate the scan-type selector, scan-time deterministic preview,
  the two opt-in CI/CD security rules, the gear popup, AI story authoring, AI-assisted Update-branch,
  the work-item modal + comments + @-mention, and the layer-2 bootstrap bypass. Date bumped to
  2026-06-22.
- **§2 cockpit views** — Governed Development bullet now mentions AI story authoring + the gear popup.
- **§3 step 4 (Audit)** —
  - Added the **scan-type selector** (AI review and/or deterministic scans; both default ON; deterministic-only is fast + token-free; both-false runs both; deep toggle hidden when AI is off).
  - Expanded "two kinds of finding" → **three kinds**, adding the **deterministic preview** tier, plus the Authority-column badge names (green "Rule · enforced", purple "Preview · not enforced until wired", blue "AI · advisory") and the filter + CSV columns.
  - Replaced the old "mechanical rules are NOT run by the scan" note with the **scan-time deterministic preview** (clippy/ruff/eslint/semgrep run by Camerata at scan; preview ≠ gate; graceful note never a false clean; mechanical rules stay out of the LLM review; CodeQL + paid tiers excluded as `layer3_only`) and the **deterministic-scan progress indicator** above the AI drawer.
- **§3 step 6 (CI step)** — added the **both-layers CI-wiring** clarification (canonical check command serves layer-2 and layer-3) and the **opt-in CI security rules** subsection (Semgrep CE vs Pro; CodeQL public-free vs GHAS-paid with the full free-tier limitations).
- **§6 Governed Development** —
  - New **Project settings (gear popup)** subsection (loop guard + default tier-map; project-wide; tier-map also still in Rules view).
  - New **Author a story from a blank UoW with AI** subsection (blank draft → clarification chat with one-question-back → live preview → push to GitHub Issues + auto-link; LLM-text-only, no code gate).
  - Added the **bootstrap run — skip layer-2** toggle to the dev-run section (layer-1 + decisions gate still apply).
  - Expanded "Other controls" with **Open work item** (modal + comments), **@-mention autocomplete** on the comment box, and the **Update branch (AI-assisted)** control (gated conflict resolution, fail-closed).
- **§13 rule types** — new **Opt-in CI security rules** subsection (`opt_in_only` never auto-recommended, no default option; `layer3_only` for CodeQL).
- **Closing one-liner** — folds in the scan-type choice + AI story authoring.

## Sanity-checked (already accurate, left as-is)

- Stepped UoW lifecycle (Intake → Investigating → Decisions Approved → Development → Awaiting QA →
  Signed Off), tiering/delegate, persistence, config-vs-data separation — all still match
  `crates/server/src/uow.rs` and `lib.rs`.
- §7 layer-2 7-language list (Rust, JS/TS, Python, Go, Ruby, Java, C#) — confirmed against
  `crates/checks/src/multilang.rs` (rubocop/bundler, `./mvnw`/`./gradlew`, `dotnet format/build/test`
  all present).

## Flags / could-not-fully-verify

- None blocking. The CI security rules and all UI components (`ProjectSettingsGear`,
  `StoryAuthoringPanel`, `NewAuthoredUowButton`, `DeterministicProgress`, the Update-branch control,
  the `Open work item` modal, `skip_layer2` toggle, scan-type checkboxes, preview badge) were all
  located in the shipped code.
- Minor: the gear-popup decision doc used shorthand lifecycle labels ("Developing → Done"); the
  guide keeps the accurate full lifecycle names that match `UowStage` in code.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
