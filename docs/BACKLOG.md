# Backlog (cross-cutting, forward-looking)

Deferred work and known gaps not yet scheduled. Newest intent at the top. UI-only
follow-ups live in `UI_BACKLOG.md`; this file is for engine + cross-cutting items.

## Scan / AI feedback modal (deferred 2026-06-16 — user is mid-testing)

When a user presses **Scan**, the brownfield audit awaits a real `claude -p` call
synchronously (~20-60s on a large repo), so the UI just shows "scanning" with no
visibility into whether the AI is actually working. Confirmed it IS working (a live
`claude -p` process with the audit prompt was observed), but the user can't *see* that,
so they can't trust it.

**Build:** a feedback modal that shows the AI's prompt + output in real time during a
scan (and during any AI step). The plumbing is half-there: `transcript.rs` + the
**Agent-activity drawer** already render an agent's generated prompt + output, but
they're only wired to the *run* path (`execute_run`), not the *scan* path. The AI-audit
call (`ai_audit::audit_repo`) should register its prompt/output into the transcript store
(or a per-scan channel) and the UI polls it, the same way the drawer does. That is the
natural home for the "see the AI's output" modal.

## Findings from the budget-tracker-testrepo run (2026-06-16) — MOSTLY FIXED

The deterministic-silence + AI-precision items below were FIXED 2026-06-16 (see ADR
`two_domain_audit_and_two_phase_flow`): whole-file matching + broadened regexes (the 3
Tier-1 plants now caught), line-numbered AI digest, adversarial-verify pass, two-domain
authority split, two-phase flow. The STILL-STAGED items moved to "Staged after the
2026-06-16 overhaul" below. Original evidence kept for reference:

- **Deterministic Layer-1 was completely silent on all three Tier-1 plants.** Three
  confirmed root causes:
  1. **The audit runs line-by-line.** `onboard::audit_content` passes ONE line at a time
     to each rule arm, so a multi-line construct is invisible. The planted raw-SQL
     `format!` has `format!(` on one line and `SELECT … WHERE user_id = '{user_id}'` on
     the next — never seen together. Fix: audit whole-file content (the write-time gate
     already sees whole content, so the two paths currently DISAGREE).
  2. **Regex format gaps.** SQL-concat matches empty `{}` but the plant uses named
     `{user_id}`/`{year}`. Secrets matches known provider prefixes (`ghp_`/`sk-`/`AKIA`/
     `AIza`/PEM) but the plant is a bare 32-char key (no prefix). Secret-in-URL needs a
     literal `https?://` but the plant templates the base (`"{base}?…&token={token}"`).
     Fix: named-arg `{\w+}` for SQL; a "long literal assigned to a `*_KEY`/`*_TOKEN`/
     `*_SECRET`-named const" heuristic for arbitrary keys; templated-URL token params.
- **AI-audit line numbers are unreliable.** The digest (`ai_audit::build_digest`)
  concatenates files WITHOUT line numbers, so the model estimates line numbers by
  counting and drifts (3 of 4 findings cited wrong lines). Fix: inject real line numbers
  into the digest.
- **AI-audit precision needs an adversarial-verify pass.** On the testbed: 2 solid, 1
  real-but-over-severity (single-user authz), 1 over-flagged (a theoretical timing
  residual). Wire the `fix::verify`-style skeptic pass (try to REFUTE each finding) before
  showing AI findings, and calibrate severity for app context (single-user, etc.).

## Staged after the 2026-06-16 audit overhaul

These are the parts the ADR `two_domain_audit_and_two_phase_flow` explicitly left staged:

- **Per-row selection drives the audit.** Phase 2 currently parameterizes the AI with ALL
  proposed rules' directives, not the per-row picked subset. Lift the rules-table /
  modal selection into the `/api/onboard/audit` call.
- **Advisory AI in the DEVELOPMENT path.** The two-domain split (deterministic enforces /
  AI advises, non-blocking) should also run during a governed dev run: after the fleet
  produces a diff, run `ai_audit` over it as advisory warnings, never blocking the build.
  Seam exists (`ai_audit`); wiring into the live-fleet completion is gated on live runs
  (opt-in `CAMERATA_LIVE_BUILD=1`).
- **Live scan feedback.** The scan awaits the AI synchronously (no progress surface). A
  live prompt/output feed needs the scan's AI calls registered into the transcript store
  (which already backs the Agent-activity drawer) + a background job the UI polls.
- **The discover→codify loop.** When an advisory AI finding is verified real + generalizable,
  offer to codify it into a deterministic rule (advisory → enforced). "Convention discovers;
  enforcement locks it in."

## Two-phase audit workflow — BUILT 2026-06-16

Detect (Phase 1, `/api/onboard/scan`) → pick rules → audit against selected (Phase 2,
`/api/onboard/audit`, AI parameterized). See the per-row-selection staged item above for
the one remaining gap.

## Local checkout used in place of cloning (deferred 2026-06-16)

The "Browse for a local repo folder" picker derives `owner/repo` from the folder's git
remote, but the workspace still CLONES into `<workspace>/<owner>/<repo>` rather than using
the developer's existing local checkout in place. Consider a "use this local checkout
directly" mode.
