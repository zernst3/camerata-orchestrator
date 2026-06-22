# Backlog (cross-cutting, forward-looking)

Deferred work and known gaps not yet scheduled. Newest intent at the top. UI-only
follow-ups live in `UI_BACKLOG.md`; this file is for engine + cross-cutting items.

## Live model-list fetch from Anthropic /v1/models (hybrid) (2026-06-22)

The UI's model picker is fed by the hardcoded `MODELS` const in `llm.rs` (id + label + per-Mtok
pricing) via `GET /api/models`. Nice-to-have: when an `ANTHROPIC_API_KEY` is present, fetch the live
model list from Anthropic's `GET /v1/models` so new models appear automatically; fall back to the
const otherwise. **Two hard caveats** (why this is a nice-to-have, not a need): (1) `/v1/models` is
authenticated — useless on the CLI/subscription path with no key; (2) the Models API does NOT return
pricing, so `price_in`/`price_out` (needed for the cost estimate) must STAY a local `id → {price_in,
price_out}` map regardless — auto-pull refreshes the LIST only, never pricing. Models change only a
few times a year, so editing the const on a new release is low-cost. Build = live fetch (key-gated) +
local pricing map + const fallback. Deferred — Zach is on subscription (no key) this week.

## Gemini provider (paid API key only — consumer subscription path dead) (2026-06-22, BLOCKED)

Zach wanted to use his **Gemini Pro subscription** in Camerata the way he uses Claude. **Not
possible:** Google deprecated consumer access to `gemini-cli` on **2026-06-18** (free / AI Pro /
Ultra / individual Code Assist), pushing consumers to the closed-source **Antigravity CLI**.
Same-as-Claude use now requires a **paid `GEMINI_API_KEY`** (paid Gemini project or Vertex;
restriction-scoped keys as of 2026-06-19) — extra cost, not the subscription. Deferred per Zach
("only if I can use my Pro plan now at no extra cost"). Antigravity CLI not researched (closed-
source, no documented headless/JSON/MCP parity).

Groundwork preserved so a paid-key wire-up is fast: provider seam (`Vendor`/`MODELS` in llm.rs);
gate proven reproducible on gemini-cli (`tools.core: []` + MCP-only `includeTools` +
`security.disableYoloMode`/`disableAlwaysAllow`, exclude `run_shell_command`, pin version + leak
test); token meter is provider-agnostic so Gemini usage lights up automatically. Stage A:
`gemini --output-format json` → `{response, stats}` (no cost field → derive from tokens). Full
design + spike findings: `docs/decisions/2026-06-22_gemini_provider_cli.md`. **Revisit IF** Zach
gets a paid Gemini/Vertex key OR Antigravity proves integrable.

## Cost estimate: model the calibration pass + bias HIGH (2026-06-17, PARKED)

**Park until before a customer sees it** — immaterial at single-dollar scans, but the bias
direction is wrong (it reads LOW, and a surprise-bigger-bill is the trust-killer). Two logged
actuals, both UNDER-estimated:
- budget-mini (dense fixture): est ~$0.25 → actual $0.56 (~2.24× under)
- rust-chorale (real, sparser): est ~$1.79 → actual $3.13 (~1.75× under)

The gap narrowed on the larger/less-dense repo, so the agent's density theory holds — BUT a
~1.75× under-read remains even on a real repo, which means a STRUCTURAL under-count on top of
density. Diagnosed cause: the **calibration pass** isn't modeled as its own line — its input is
all findings + context and its output re-emits them, at the calibration model's rate. On a $600
Rivet scan a 1.75× under-read is a ~$450 surprise.

Fix when picked up: estimate the calibration pass explicitly (input ≈ findings + context,
output ≈ findings re-emitted, priced at the calibration model) and add a conservative bias so
the estimate lands ABOVE actual. Keep logging actuals (the actual-vs-estimated readout is the
training data); fit the findings-density prior + calibration line once there are ~5–10 points
across small/large × sparse/dense × model-combo. Cheap, later, not now.

## Re-onboard guard + "add repo to project" + project-config sharing (2026-06-17)

**Gated to the disposition-testing phase** (apply / ignore / accept-as-tech-debt is the next
thing Zach tests). Do NOT build ahead of that phase.

**Re-onboard guard.** Once a repo is onboarded, that fact should be detectable so a second
onboarding warns "this repo is already onboarded" (non-blocking — they can proceed, they just
shouldn't duplicate work unknowingly). The onboarded fact must be read from the REPO at origin,
not from project state: onboarding is "armed" as committed artifacts (CONVENTIONS.md / AGENTS.md,
the CI gate workflow, the gate rule-subset config). The guard = check origin for those armed
artifacts (cheap: a contents API HEAD on the gate workflow / `.camerata/` config). This keeps the
flag authoritative across users + machines, because the enforced config already lives in origin.

**"Add repo to project" as the alternative.** If they don't want to re-onboard, offer adding the
already-onboarded repo to the current project's set (a workspace op, no re-scan) so they reuse the
existing armed config instead of regenerating it.

**Project-config sharing — recommendation: user-sharable export, NOT config-in-every-repo.**
Two deliberate tiers (this is the decision Zach was erring toward, and it's the right one):

1. **Enforced/armed config → lives in each repo, in origin** (CONVENTIONS/AGENTS, CI gate, gate
   rule-subset, baseline). Already gets there via Arm. Each repo owns exactly its own slice → no
   fan-out conflicts, survives in origin, anyone who clones gets it. This is the source of truth
   for *enforcement*.
2. **Project workspace config → user-sharable artifact** (the existing export/import JSON), never
   committed to repos. This is the cross-repo orchestration + pre-arm working state (repo set,
   in-flight rule selections/alternatives/dispositions, audit history). It's a "project file" like
   a `.code-workspace` / Postman collection.

Reject "project config lives in each repo it touches": a project spans N repos but has ONE config,
so replicating it to all N drifts + conflicts (Zach's instinct, correct); picking one "primary"
repo is arbitrary + breaks if that repo leaves; and access-control mismatches (a teammate with
repo A but not B can't reconstruct the project from A). The thing that genuinely belongs in repos
already gets there via Arm; project config is inherently a workspace concern.

Evolution path when multi-user becomes real: a **shared project store** (orchestrator SQLite →
hosted project service, or the Work-Tracker bridge in `WORKTRACKER_INTEGRATION.md`), NOT
config-in-repos. Optional refinement: the project keeps a *manifest* (repo list + a hash/pointer
to each repo's armed config) so it can DETECT drift between project intent and what each repo
actually enforces — without owning/duplicating the enforced config. Promote to a `docs/decisions/`
ADR once Zach confirms the two-tier split.

## Scan execution modes — parallel, then job/streaming (designed 2026-06-16)

Decision + plain-English design in `docs/decisions/2026-06-16_scan_execution_modes.md`.
The audit is sequential + synchronous; at scale (5 huge repos, max model) that's a ~5-hour
single blocking request. Two ceilings: sequential wall-clock (fix = parallel execution) and
synchronous all-or-nothing delivery (fix = a job model with incremental findings + resume).
Build order: **(1) parallel rule-batching + concurrent chunks** (5h → ~25min; the shared
engine), then **(2) the job/streaming delivery layer** when multi-huge-repo is the real case.
Auto-select the mode by scale with a manual override (model-picker philosophy). The job model
is OUR orchestration, backend-agnostic — the CLI does it fine (it's the transcript-store/poll
pattern generalized to findings + progress).

### CRITICAL by impact × confidence, not by category (refinement)

v1 ships "deterministic-floor findings → Critical" (commit cd9211c). That's the RIGHT
starting point and it's NOT a blunt category map: only the **deterministic** hits are
elevated — they're confirmed by regex/AST (high confidence) AND high-impact (live secret,
injection, secret-in-URL). The AI's security-flavoured findings stay high/medium/low with
their `[needs review]` tags, so a low-confidence "maybe auth gap" never screams Critical.

The refinement to guard the tier (so Critical stays rare + trusted, no alert fatigue):
- **Critical = high-impact AND high-confidence** — the top-right corner of (severity × confidence),
  not "the security column." A `[needs review]` semantic finding is never Critical.
- **Not security-exclusive** — a confirmed money-correctness bug (ARCH-EXACT-DECIMALS-1) or a
  data-loss path can be Critical too; severity is blast radius, not the rule's label.
- **Converge on `ORCH-TIERED-ESCALATION-1`'s line**: Critical ≈ hard-guard impact (auth, payments,
  infra, CI) + confirmed. That's the same "stop-the-line" set the gate would hard-pause on.
- A hardcoded *test/sandbox* key or a secret-in-URL on an internal-only endpoint is security-class
  but not stop-the-line — these shouldn't auto-elevate once the impact signal exists.

Also queued from the same fixture verdict (AI audit quality, not execution):
- **Applicability-scoping** — a rule should only fire when the pattern is actually present to
  violate (e.g. don't flag "no cursor pagination" on a repo method with no list endpoint). The
  model already self-labels these "borderline / no endpoint yet" — a prompt instruction to omit
  hypotheticals.
- **Bug-vs-ideal tiering in the AI layer** — deterministic security findings are now Critical
  (done), but the *architectural* findings are still flattened into "high". Tier "actual defect"
  vs "doesn't implement a preferred pattern" so the signal-to-noise holds on big repos.

## Chorale-separable items (flagged 2026-06-16)

Surfaced while reworking the proposed-rules table. These belong in the **rust-chorale**
repo, not here — tracked so they aren't lost.

- **Per-group "select all" (enhancement, not a bug).** When grouping is on, chorale's
  group headers have no select-all checkbox, so the cockpit builds its own per-domain
  "select all in:" chips above the table. A group-header select-all (toggle every row
  under a group) would let consumers drop that workaround. Candidate chorale feature.
- **Row-click → full-screen-modal reopen (SUSPECTED, NOT CONFIRMED).** Symptom: open a
  modal from `on_row_click`, close it, click any row → modal won't reopen. Root-caused
  to the cockpit side: the overlay was mounted as a *sibling of the `Table`* inside the
  same component, and the desktop webview left a ghost node swallowing the next click.
  Fixed cockpit-side by hosting the modal OUTSIDE the table subtree (a shared context
  signal; modal rendered by `ScanResults`). Chorale's `on_row_click` itself is correct —
  it fires `cb.call(row_id)` on every plain click with no open-state gate (verified in
  `components.rs` cell `onclick` + `should_fire_row_click`). **If the bug recurs even
  with the modal relocated**, it's a chorale event-delegation issue (the cell's two
  `signal.set` writes before `cb.call` re-render the table mid-click) worth filing
  against rust-chorale. Until then, nothing to file. NB: a cell-renderer button can't be
  the trigger — `CellRenderer` is `Send + Sync` so it cannot capture a Dioxus signal.

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
