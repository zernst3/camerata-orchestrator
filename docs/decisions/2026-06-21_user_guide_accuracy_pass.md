# 2026-06-21 — USER_GUIDE.md accuracy pass

`docs/USER_GUIDE.md` is both the user-facing guide and the in-app chatbot's grounding
(`crates/ui/src/chat.rs` `include_str!`s it). A comprehensive section-by-section accuracy
pass was done against the shipped code. Every claim below was verified against source before
changing. This file records every change and every unresolved flag.

## Sources of truth consulted

- `crates/ui/src/cockpit.rs` — cockpit nav + Governed Development surface (IssueManagement /
  WorkItem table / WorkItemDetail / CreateOrOpenUow / UowDevControls / GateSelfCheck / CI step).
- `crates/server/src/workitems.rs`, `crates/server/src/lib.rs` (routes) — WorkItem/UoW endpoints.
- `crates/ui/src/chat.rs` — chatbot grounding layers + "what this assistant can see" strip.
- `crates/checks/src/multilang.rs` — cross-language layer-2 runners (lockfile-pinned, fail-closed).
- `crates/server/src/arm.rs` — emit-time file routing (prose → AGENTS.md, rest → CONVENTIONS.md).
- `crates/server/src/feature_flags.rs` + `.camerata/features.toml` — feature-flag model.
- `crates/gateway/src/lib.rs` — gate `RULE_REGISTRY` (6 enforced rules) + `enforced_gate_rules()`.
- `crates/fleet/src/gate_probe.rs` — gate self-check probe semantics.
- `crates/rules/principles/**` — corpus: enforcement + verification tiers, mechanical linter cites.
- `crates/server/src/ai_audit.rs` — scan modes + thorough calibration + deep tier.

## Changes made

1. **§2 cockpit views** — added the **Docs** nav item (sixth view; shipped). Rewrote the
   Governed Development blurb to drop "adopt stories / story control surface" and describe the
   shipped pull-work-items → create-UoW → governed-dev flow. (Verified nav labels in cockpit.rs:
   "Onboard repos", "Governed Development", "Rules", "Routines", "Repository Workspace", "Docs".)

2. **§6 Governed Development — full rewrite (the biggest fix).** The old section described a
   "story spine" with clickable stage tabs (Intake / Investigation / Plan / Status / QA & sign-off).
   That surface was rebuilt. Replaced with the shipped surface:
   - **Issue Management** panel: GitHub Issues only (Jira/ADO/GitHub-Projects are planned, not
     shipped); **manual "Pull work items"** (no auto-poll); pulls **all open issues across every
     project repo** into a **WorkItem table with a Repo column**; click a row to read it.
   - **Create Unit of Work from this issue** — deduped by external ref ("Open Unit of Work" if one
     exists).
   - **UoW dev controls** (verified exact labels): "Run this work (governed)"; the lifecycle strip
     (Intake → Investigating → Decisions approved → Development → Awaiting QA → Signed off) with the
     two architect transitions "Begin investigation" / "Approve decisions"; "Ask the team" clarify
     loop with "Suggest questions (AI)"; "Add comment to issue" / "Post comment"; "Pull latest work
     item" (re-pull one item, no cache); "✓ Sign off this run".
   - **Gate self-check**: "Run gate self-check", GO/NO-GO verdict, "6/6 floor rules enforced".
   - Retired the terms CanonicalStory / "adopt a story" / "adopt-issue" / "story spine" from the
     section; switched to WorkItem / Unit of Work (UoW).

3. **§3 step 4 (scan modes)** — added the **fourth** shipped mode the doc omitted: **Batch (50% off
   — async, API key required)** (Anthropic Message Batches API). Tightened the labels for
   Sequential and Background job to match the UI selector. Noted Camerata auto-selects a recommended
   mode by codebase size.

4. **§3 step 6 (CI step)** — was described as filing one "wire mechanical rules" story. Shipped
   reality files **two** stories per repo: "Create mechanical-rules CI story" and "Create
   architectural-rules CI story", each with a preamble explaining both are deterministic CI-tier
   (mechanical = off-the-shelf linter, architectural = bespoke custom checker + refinement).

5. **§7 (the four gates)** — updated the Layer-2 row + added a paragraph: Layer 2 is now
   **cross-language / polyglot** (runs checks for every language in the repo: Rust/JS-TS/Python/Go),
   uses the **repo's lockfile-pinned toolchain** (== the repo's CI), and is **fail-closed** (missing
   toolchain / undefined check / failed install → "not verified", never a clean pass). The old row
   implied Rust-only `fmt`/`clippy`/`test`.

6. **§8 (in-app assistant)** — corrected the "what this assistant can see" strip. It lists
   **Technical reference (TECHNICAL.md)** and **User guide (USER_GUIDE.md)** as separate rows (not a
   merged "Canonical docs"), plus **Governance rules catalog** (`GET /api/corpus-rules`) and
   **Development state** (`GET /api/uow`, not a "development-context endpoint"). The active finding is
   a conditional fifth row ("Focused finding"), shown only when you "Ask" about a finding.

7. **§11 (deep report / SOC-2)** — added a note that the SOC-2 lens is behind the `soc2` feature
   flag, which ships OFF (cross-ref §12), so it runs only if re-enabled.

8. **§12 (feature flags)** — substantial correction. Shipped reality:
   - There is **one** runtime flag: `soc2` (env override `CAMERATA_FEATURE_SOC2`). The doc's other
     two flags (`CAMERATA_FEATURE_DEEP_SECURITY`, `CAMERATA_FEATURE_THREAT_MODEL`) **do not exist**
     as flags. Removed them.
   - Flag name corrected: env var is `CAMERATA_FEATURE_SOC2`, not `CAMERATA_FEATURE_SOC2_ANALYSIS`.
   - Config source is **`.camerata/features.toml`** + per-flag env override, **not** a `.env` file.
   - Model is **default-true opt-out**, BUT the repo's checked-in `.camerata/features.toml` sets
     `soc2 = false`, so **SOC-2 ships OFF**. Documented both facts and that when `soc2` is off, only
     the SOC-2 lens is skipped (deep-security + threat-model still run, and are NOT separately gated).

9. **Whole-loop one-liner** — replaced "adopt stories and run governed work" with "pull work items,
   create a Unit of Work from one, run governed work".

## Verified accurate, left unchanged

- **§13 (rule types)** — verified and accurate: prose/structured/mechanical/architectural;
  emit routing (prose → AGENTS.md, rest → CONVENTIONS.md, per `arm.rs`); "six gate rules, only
  ARCH-NO-SECRETS-IN-URL-1 also in corpus as structured" (confirmed: gateway `RULE_REGISTRY` = GOV-1
  + SEC-NO-HARDCODED-SECRETS-1 + SEC-NO-RAW-SQL-CONCAT-1 + ARCH-NO-SECRETS-IN-URL-1 +
  SEC-NO-PATH-ESCAPE-1 + SEC-NO-SECRET-FILES-1; only ARCH-NO-SECRETS-IN-URL-1 is in the corpus, as
  `structured`/`grounded`); every mechanical rule cites a real linter (all 57 mechanical TOMLs carry
  a `linter` line); verification ladder draft/grounded/verified with **0 verified** and 305 grounded
  (confirmed); grounded = shippable baseline.
- **§0–§1, §5** — local-first storage, export = path-free single-project JSON, import = upsert with
  same-name overwrite warning, per-repo local-path resolution + continuous broken-path health check,
  `camerata/onboard-governance` branch, `CAMERATA_GITHUB_TOKEN` / `CAMERATA_LIVE_BUILD` — all verified.
- **§3 step 5** — Apply button "Add rules to repo(s) (branch + push)" + separate "Open governance
  PR" button; commits governance files only; branch force-pushed — verified.
- **§3 step 4** — thorough-calibration ~3× the calibration portion, multi-vote conservative
  consensus, never drops findings — verified. Deep tier ~3× / most expensive — verified.
- **§8 honesty guardrail** — single context-rich assistant, no mode selector (chat.rs comment
  "No mode selector"), says-so-verbatim when uncovered (`UNIFIED_NOT_COVERED_PHRASE`) — verified.

## Unresolved flags

- **None blocking.** All claims were verifiable against code. Two notes for awareness (no doc change
  needed, NOT code changes since this pass is confined to the doc):
  - `crates/ui/src/cockpit.rs:4001` still contains a stale in-code hint string ("…or adopt a tracker
    issue to bring real stories into the spine") using retired terminology. The user guide no longer
    uses that wording; the code string is out of scope for this doc-only pass. Flagging for a future
    code-side cleanup.
  - `workitems.rs` still maps via the worktracker `CanonicalStory` model internally (a deliberate
    bridge, per its module doc); this is internal and correctly hidden from the user guide, which now
    uses WorkItem / UoW exclusively.
