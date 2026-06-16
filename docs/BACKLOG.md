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

## The deterministic engine: reuse vs build (diagnostic answered 2026-06-16)

You asked how the gate evaluates hardcoded-secret / SQL-concat today, to decide
reuse-vs-build. Answer from reading `crates/gateway/src/lib.rs`: they are **regex
heuristics** (compiled `OnceLock<Regex>` per rule), e.g. known token prefixes
(`ghp_`/`sk-`/`AKIA`) plus a new `*_KEY`-const long-literal heuristic for secrets, and a
SQL-keyword-near-interpolation pattern for raw SQL. Deterministic, fast, file-checkable —
so the brownfield scan now REUSES that exact engine (`content_match_lines`), which is why
the Tier-1 plants are caught.

The honest limit (the "loose" concern): regex secret/SQL detection has real false-positive
and false-negative edges the per-write gate rarely exercised. The precision upgrade is to
wrap battle-tested scanners as CheckRunners — **gitleaks/trufflehog** for secrets (they nail
the env-read-vs-hardcoded and name-vs-value distinctions), **semgrep** for AST patterns
(SQL-concat, secret-in-URL). That's the build path if the regexes flood; the wiring is the
same `content_match_lines` seam, swapped for a subprocess. Staged.

## AI output streaming (deferred — you de-prioritized)

The Agent-activity drawer shows the audit's prompt immediately but the OUTPUT only appears
at the end, because `claude -p --output-format json` returns one blob (no incremental
tokens). To show the model's output as it generates, switch the CLI path to
`--output-format stream-json`, read stdout line-by-line, and append each text delta to the
transcript live. Real but a fair lift; you said "nothing to be done, we wait" — captured
here in case that changes.

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
