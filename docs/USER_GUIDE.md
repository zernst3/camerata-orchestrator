# Camerata — end-to-end user guide

This is the "I have a repo new to Camerata — where do I start, and how do I take it all
the way through?" walkthrough, and the canonical source the **in-app assistant** (the chat bubble) answers from. Keep it accurate to what's actually shipped — an
assistant that describes a feature that doesn't exist undercuts the whole point.

> Status (updated 2026-06-27): the brownfield **onboarding flow is built and live** — per-repo
> stack detection + rule selection, **custom rules** (per-repo + project-global), an optional code
> audit you can scope with a **scan-type selector** (AI review and/or deterministic scans), four scan
> modes, an opt-in **thorough-calibration** consensus pass, a **scan-time deterministic preview** that
> runs your selected mechanical linters during the scan, a three-table finding triage, and an **Apply**
> step that writes governance onto a local branch and pushes it (no PR until you ask). Two opt-in
> **CI/CD security rules** (Semgrep, CodeQL) are available but **never auto-recommended or pre-checked**.
> Re-onboarding an already-onboarded repo is **blocked**. Projects are **exportable/importable**, repos
> resolve to **local folders** with a health check, and onboarding state **auto-saves**. Governed
> Development is now a **3-phase cockpit** (Intake · Investigation & Refinement · Development) with
> free navigation between phases, **Finish/Reopen** controls per phase, **per-story repo/branch
> scoping** (Intake, with the repos-in-scope selector at the top of the page), **prose contract settling**
> and a **contract precondition** (no development for cross-boundary work without a contract), and a
> **per-repo Ship panel** with a "Ship all repos" chain button. UoWs reach a **Done/archive** terminal
> state. Settings is a **single consolidated nav item** (no settings button on the Governed Development
> page): cross-project credentials (OpenRouter key + GitHub token, keychain-backed, masked) and Bombe
> animation controls on one side; per-project **model configuration** on the other. Model tiering is
> **domain-aware**: Strongest / Balanced / Fast fleet bands, an optional project-wide **Designer
> (vision) band** for visual work, **suggested profiles** (Balanced / Max Efficiency / Max Quality /
> Custom), and individually selectable **helper-agent models** (audit, calibration, research chat,
> story authoring, decomposition, escalation, clarification). L3 AI code review has its own model
> selector (defaults to Balanced). The **Rules page** is rules-only (model config moved to Settings).
> The **Rules view** has an **"Emit rules locally"** button that regenerates governance files directly
> into local repo clones. Emit is **local-only by default**, with three **cascading** toggles (each
> requires the previous): **Save emits on a new branch → Push to GitHub → Open a PR**. The UI theme
> is **Bletchley industrial amber** with a background **Bombe machine** that is an **AI-activity
> indicator**: it powers up (lights up, rotors spin) only while genuine AI / heavy work runs (chat
> turns, authoring, investigation, live runs, scans, audits) and powers down to a dim idle otherwise;
> the rotor knobs freeze in place between runs and resume from there. Trivial fetches never animate
> it. Settings exposes a global ON/OFF and a Play/Pause preview. The **Intake page** renders the whole
> story inline (no separate modal) with a large **"Context for the investigation agent"** field that
> is editable and deletable after it is added. The **Rules page** groups rule tables by domain,
> collapsed by default; clicking a row opens a rule-detail modal. **Blank UoWs you author with AI**,
> an **AI-assisted Update-branch** control, a work-item modal with comments + @-mention autocomplete,
> a one-time **layer-2 bootstrap bypass** for installing tooling, a one-click **Gate self-check**
> (go/no-go), **multiple concurrent UoWs** (each runs in its own isolated git worktree), **PR
> lifecycle buttons** per UoW (push, open PR, pull PR info, resolve with agent), and **structured
> clarifications** that auto-save at pause points. Dev runs and onboarding scans show **run liveness**:
> an amber **stall warning** appears when a run makes no progress for the watched threshold, and a
> **Stop button** is always available to cancel any running dev run or scan at any time (the run ends
> in a **Cancelled** state). Scan findings now include a **Test badge** for test-scope violations, a
> separate **Scan coverage** section (tools that didn't run), and scan tools (Semgrep/ESLint)
> **auto-install on first use**. The in-app assistant retains **conversation context** across messages
> and is grounded on your active project and pulled issues. A persistent **token usage meter** tracks
> 5-hour and session-wide spend. The **check manifest** (`.camerata/checks.toml`) is the single
> source of truth for custom deterministic gates: one entry wires a check into BOTH the in-loop dev
> gate and the generated CI workflow. The runtime is **provider-agnostic**: a native in-process
> `ApiAgentDriver` owns the MCP tool-use loop for any provider; the `ClaudeCliDriver` remains for
> the Claude subscription path. Credentials are an **OpenRouter API key** (for API-path models) and
> a **GitHub token** (for repo operations).

---

## 0. Credentials (the only setup)

Credentials live in **Settings** (the dedicated nav item, not the old gear popup). They are stored
in the system keychain and shown masked in the UI. Two credentials, both optional depending on which
paths you use:

1. **GitHub token**: for push/PR operations and Issues read/write. One token serves every repo it
   can reach. You can also set it via environment variable for scripted launches:
   ```bash
   export CAMERATA_GITHUB_TOKEN=github_pat_xxx
   ```
2. **Model provider**: two paths, pick one:
   - **Claude CLI (subscription):** the `claude` CLI on your PATH, logged in. Camerata drives it as
     a subprocess (`ClaudeCliDriver`). Set `CAMERATA_LIVE_BUILD=1` to activate the live fleet.
   - **API path (OpenRouter or Anthropic API):** set an **OpenRouter API key** in Settings (or
     `ANTHROPIC_API_KEY` for direct Anthropic). The `ApiAgentDriver` owns the MCP tool-use loop
     in-process and works with any provider the model registry discovers (Claude, and every
     OpenRouter-listed model flagged for tool use).

Launch:
```bash
cargo run -p camerata-ui
```
The desktop app opens with its embedded server; the topbar shows the live connection status and
which provider path is active.

### Claude backend

Camerata can call Claude in two ways, selected in **Settings → Claude backend**:

| Option | How it works | API key needed? |
|--------|-------------|-----------------|
| **CLI** (default) | Spawns the `claude` CLI using your logged-in Claude Code subscription | No |
| **API** | Calls the Anthropic Messages API directly | Yes, enter it below |

When you select **API**, an **Anthropic API Key** input appears. Enter your key once; it is stored in the OS keychain (never in files or the repo) and hydrated into the server process automatically. If you select **API** but have not yet saved a key, the server falls back to CLI, and an inline warning stays visible until a key is present. The CLI path requires an active Claude Code subscription on the machine running Camerata; the API path works without one but consumes Anthropic API credits.

**Local-first:** Camerata stores only configs + pointers (JSON in your OS app-data dir —
`projects.json`, `settings.json`, `onboarding-draft.json`); your repo code lives on your own
machine. Each repo a project references must resolve to a local git checkout (see §5).

---

## 1. Projects (the container for everything)

A **project** holds its repos, ruleset, and onboarded state. From the **Projects** home you can:
- **Create** a project (just a name to start).
- **Open** one (the cockpit's four+1 views only appear inside a project).
- **Export** the open project — a single, **path-free** `camerata-project-<name>.json` containing
  only that project's repos (`owner/repo`), ruleset, and which repos are onboarded. It does **not**
  include local paths or your workspace settings.
- **Import** a project config — it **upserts** into your local projects. If you already have a
  project with the same name, Camerata **warns before overwriting**. Your own workspace settings are
  never touched. After import the repos have no local paths yet — resolve them in the Rules view (§5).

**Project config vs. project data:** the exported JSON contains only the project's transferable
config (repos, ruleset, onboarded state, tier map). Units of Work, the story spine, onboarding
drafts, and local repo paths are **local to each developer** and are never included in an export.
UoWs represent in-progress dev-lifecycle state (branch, stage, run history, decisions, sign-off) and
are machine-local — transferring them would produce overlapping work across developers. When you
import a project on a new machine you start with no UoWs; they accumulate as you work.

---

## 2. The cockpit views

Inside a project the nav shows: **Onboard repos · Governed Development · Rules · Routines ·
Repository Workspace · Settings · Docs**.
- **Onboard repos** — bring a repo under governance (§3).
- **Governed Development** — the work control surface: pull work items from a tracker (or author a new
  story from a blank UoW with AI), create a Unit of Work (UoW) from one, then run governed development
  on it with the human↔AI clarify loop, comment back, and sign off (§6). There is no settings button
  on this page; all configuration lives in Settings (below).
- **Rules** — manage the project's ruleset after onboarding + the repo-path health check (§4, §5).
  Rule tables are grouped by domain, collapsed by default; clicking a row opens a rule-detail modal.
  This page is rules-only: model configuration has moved to Settings.
- **Routines** — schedule governed runs (templates, an in-app auto-fire scheduler, run history, and
  blocked-run review). Full walkthrough in §18.
- **Repository Workspace** — the local clones: clone status, branch, and ship (push + PR) for dev work.
- **Settings**: all configuration in one place, split into two clearly-labeled scopes (see §17). The
  Soft-context group also holds the **Work hierarchy** builder (define your work-item types and nesting).
- **Docs** — the in-app documentation viewer (this guide and the technical reference).

---

## 3. Onboarding a repo (the main flow)

Open **Onboard repos**. The flow: **detect stack → pick rules → scan the repo for violations (audit)
→ triage findings → apply governance → (optional: wire CI) → onboarded.** The **violation scan is a
core part of onboarding**, not an afterthought: it is where Camerata reads your existing code and
surfaces what already violates the rules (existing hardcoded secrets, SQL injection, disabled TLS,
architectural problems, and more), so you start governed development from a known baseline. You
*can* skip it and just apply the governance files, but running it is strongly recommended.
Onboarding state **auto-saves** continuously, so you can quit and reopen without re-scanning (a
fresh scan starts a new session; a crash mid-scan just re-runs the scan).

1. **Point at the repo(s)** — **browse to each repo's local folder.** Onboarding is **local-first**:
   it reads the code on disk, so a repo must already be cloned locally. Camerata derives the
   `owner/repo` from the folder's git origin (used later only for push / PR) and **records the local
   path**, which makes it a workspace repo immediately — "add a repo to onboard" and "add a repo to
   the workspace" are the same act. **No GitHub connection is needed to onboard**; a token is only
   used later, to push the governance branch and open a PR.

   **Already-onboarded guard:** if you point at a repo this project has already onboarded, Camerata
   **refuses with an error** rather than silently re-running. Onboarding is a one-time act per repo;
   to change a repo's rules after the fact, edit them in the **Rules** view (§4), don't re-onboard.
2. **Scan + propose per-repo rules** — Camerata reads each repo's **local working tree** (it never
   downloads code from GitHub) and detects its stack: languages from extensions, frameworks from
   manifests, **IaC** (Terraform, Terragrunt, Bicep, Pulumi, CloudFormation) and **CI/CD** (GitHub
   Actions, GitLab CI, CircleCI, Azure Pipelines, Travis, Bitbucket, Drone, Jenkins). It proposes a
   starter ruleset **per repo**.

   **The scan follows your `.gitignore`** (gitignore-aware walking). For git repos, Camerata walks
   the working tree with ripgrep's gitignore engine, which honours `.gitignore` at the root and in
   subdirectories, `.git/info/exclude`, and your global gitignore. A file that is gitignored is
   **skipped** — no findings are generated for it. A file that is **committed to the repo** (not
   gitignored) is **still scanned**, even if it has a sensitive name like `.env`. This is deliberate:
   a committed `.env` with real credentials is an actual leak because it is version-controlled and
   visible to anyone with repo access. The scan is enforcing what git sees, not what the filename
   suggests. For directories without `.git`, Camerata falls back to its built-in noise denylist
   (`node_modules`, `target`, build/cache/generated dirs, lockfiles, …).
3. **Pick rules** — each repo has its **own** recommended-rule table; a repo single-select switches
   which repo you're editing. Selection is **per repo**: a rule ticked for repo A is bound to A only.
   **Project-level rules** apply to every repo. Click any rule to read its decision question, the
   options, the default, and each option's rationale, and to choose an alternative. **Option choices
   are also per repo** — adopting an alternative for a rule while viewing one repo does not change
   another repo's choice. A recommended rule that still needs an alternative chosen is **highlighted
   amber and blocks Audit / Add-rules** until you pick one (or deselect it).

   **Opt-in CI security rules are never pre-checked.** The two security-scan rules
   (`CICD-SEMGREP-SECURITY-SCAN-1`, `CICD-CODEQL-SECURITY-SCAN-1`) appear in the list as
   **"Available"** (no recommended badge, no pre-checked checkbox). You opt in deliberately. See §3
   step 6 for details.

4. **Scan the repo for violations + triage (recommended core step)** — scan the existing code to
   surface what already violates your rules. This is the heart of brownfield onboarding: you can skip
   it and apply governance straight away, but it is where Camerata finds the existing security and
   architectural problems in your repo, so running it is strongly recommended. Each repo
   is scanned only against **its own selected rules** plus the always-on **deterministic security
   floor** (hardcoded secrets, raw-SQL concatenation, secrets in URLs, private-key blocks, vendor
   credential tokens, secret-bearing file paths, TLS verification disabled — ranked Critical, free +
   instant).

   **Scan-type selector — what to run.** At audit-start two checkboxes decide which passes run, both
   **on by default** (today's behavior):
   - **AI architectural review** — the LLM scan of architectural/structured/prose rules (and the deep
     tier). Unticking it skips **every** model call: **no tokens** are spent.
   - **Deterministic scans (floor + linters)** — the always-on security floor **plus** the scan-time
     mechanical preview (below). Local, **no LLM, no tokens**.

   You need at least one ticked; if you untick both, Camerata runs both rather than nothing. **Pick
   deterministic-only** for a fast, free pass (it's also the cleanest way to sanity-check the linter
   findings); **pick AI-only** for just the judgment review. The deep-report toggle is hidden when AI
   review is off (the deep tier is itself an LLM pass). The scan-mode picker below (Parallel /
   Sequential / Background / Batch) is unchanged.

   **Three kinds of finding, and the difference matters:**
   - **Deterministic floor** (`SEC-NO-HARDCODED-SECRETS-1`, `SEC-NO-RAW-SQL-CONCAT-1`,
     `ARCH-NO-SECRETS-IN-URL-1`, `SEC-NO-PRIVATE-KEY-1`, `SEC-NO-VENDOR-TOKEN-1`,
     `SEC-NO-SECRET-FILE-1`, `SEC-NO-DISABLED-TLS-1`) — pure regex/logic, no LLM. **Repeatable**
     (same code → same result, same rule-id, same line), and these are the exact checks the layer-1
     gate enforces on new writes. Treat their rule-ids as **stable/canonical**. In the triage table
     they carry a green **"Rule · enforced"** badge.
   - **Deterministic preview** — findings from the **scan-time preview** (below): your selected
     mechanical rules' own linters, run by Camerata during the scan. **Deterministic** (stable
     tool rule-ids) but **advisory** — they are **not enforced until the CI story wires them**. They
     carry a purple **"Preview · not enforced until wired"** badge.
   - **AI-suggested (architectural)** — model-inferred issues regex can't catch (layering violations,
     N+1, missing auth on writes, god objects, GET-with-side-effects). These are **advisory**, and the
     model **invents the rule-id** per finding, so the id, severity, and exact set can **vary run to
     run**. Read them as "the model flagged this pattern," not as a fixed rule. They carry a blue
     **"AI · advisory"** badge. The calibration pass recalibrates severity and flags low-confidence ones
     but never drops any — you make the final call.

   The triage table's **Authority** column shows these three tiers and is **filterable** (enforced /
   preview / advisory); the CSV export carries the `preview` / `preview_tool` columns.

   **Test badge, Needs-review state, and Self-referential findings.** Three extra flags appear in the
   finding table:
   - **Test badge** (yellow) — the finding is in test code (a test file path, or a line inside a
     `#[cfg(test)]` block). Test-scope violations are down-ranked to low severity. The nuance: a real
     secret in production code in the same file stays Critical even if the file also contains a test
     block — classification is per-finding-by-line, not per-file.
   - **Needs review** (orange) — the calibration pass flagged this finding as uncertain; read the
     reason and decide yourself.
   - **Self-referential** (gray badge, status `suppressed-self-reference`) — Camerata's own rule
     descriptions contain the very patterns it looks for. When the scanner reads a governed repo's
     `CONVENTIONS.md` or `AGENTS.md` and encounters a rule directive such as "Do not set
     `verify=False` in TLS configuration," it would otherwise flag that line as a TLS-verification
     finding. Findings like this are marked Self-referential and do NOT count toward the active
     violation tally. They are visible in the report so you can confirm they are noise — but they do
     not drive the gate, and they do not contribute to the CI gate count.

     **The safety property:** this suppression applies ONLY when BOTH the file is a Camerata-emitted
     governance artifact (`AGENTS.md`/`CONVENTIONS.md` with the Camerata header, a `.camerata/` file,
     or a corpus TOML) AND the matched snippet is traceable to a rule's description text. A real
     credential pasted into `CONVENTIONS.md` that is not part of any rule description still flags as
     `active`. Ordinary source files (`app/config.py`, `src/main.rs`, etc.) are never suppressed
     by this mechanism regardless of what text appears in them.

   **Scan coverage section.** Below the violations table, a separate **"Scan coverage"** section
   lists tools that didn't run (missing binary, unrouted rule, etc.) as informational notes. These
   are **not violations** — they tell you where coverage has a gap, so you know what the scan did
   and didn't check.

   **Scan tools auto-install on first use.** Camerata auto-provisions Semgrep and ESLint into its
   own cache directory (`~/Library/Application Support/camerata/tooling/` on macOS;
   `~/.local/share/camerata/tooling/` on Linux) the first time a scan needs them. The bundled
   Semgrep ruleset runs fully offline after the one-time install. If `python3` or `npm` is not on
   your PATH, Camerata degrades gracefully — you get a coverage note explaining the gap, and the
   rest of the scan continues unaffected.

   Pick the **model** and the **scan mode** (four options; Camerata auto-selects a recommended one by
   the codebase's size):
   - **Parallel** (default) — runs rule-batches concurrently; fastest. Wall-clock is the slowest
     batch, not the sum of every call.
   - **Sequential (slower, gentlest)** — one chunk at a time, all rules together; slower but gentlest
     on rate limits (a fallback when Parallel hits throttling).
   - **Background job (walk away)** — the Parallel scan, but it runs server-side and detached: you get
     a progress view and can walk away while findings stream in. Best for huge / multi-repo scans
     where a foreground scan would tie up the page. (Parallel and Sequential run in the foreground and
     block until they finish; Background job is the same work, just detached.)
   - **Batch (50% off — async, API key required)** — submits all passes as a single **Anthropic
     Message Batch**, for a flat **50% discount on all scan tokens**. Requires `ANTHROPIC_API_KEY` and
     the `api` backend; results arrive asynchronously (seconds to minutes, up to 24h on very large
     scans). Best when latency is acceptable in exchange for cost.

   **Thorough calibration (opt-in checkbox).** Off by default. When ticked, the calibration pass
   that recalibrates AI-suggested severities runs as a **multi-vote consensus** instead of a single
   pass: each finding is judged several times and the **conservative** verdict wins on disagreement
   (a finding two votes call real and one calls noise stays). It catches more borderline cases and
   reduces flaky severity, **at the cost of more AI calls** — so the **pre-scan cost estimate rises
   (~3× the calibration portion)** the moment you tick it. Leave it off for a quick pass; turn it on
   when you want the audit's severities to be trustworthy enough to act on directly. It never drops
   findings either way — it only re-ranks and flags.
   Findings land in three tables you switch between: **Unresolved · Ignored · Tech debt** — select and
   **Ignore (with reason)** or **Save as tech debt**, re-bucket freely. A **Needs-review** column shows
   the calibration pass's flag + reason (and the Test badge for test-scope findings). In the Tech-debt
   table mark items **resolve later** or **resolve now**, then **Process**: ignores become baseline
   waivers, and **every tech-debt item is filed as a GitHub issue** (the story). Resolve-now issues
   are titled for pickup by the dev engine (the actual dev work is Pillar 2; onboarding only *writes
   the story*). There is no separate "fix the findings" button — fixing a finding is just its
   resolve-now story flowing into the dev layer. Triage/Process is **not required** to finish
   onboarding.

   **Scan-time deterministic preview.** For each **mechanical** rule you selected that maps to a tool
   Camerata can drive (clippy, ruff, eslint, semgrep), the deterministic scan **runs that tool itself**
   with a Camerata-supplied config and folds the results into triage as **preview findings** — even if
   the rule isn't wired into the repo yet. You select it, you see findings. A preview is **indicative,
   not enforcement**: it uses Camerata's tool version (which may differ from what the repo eventually
   pins), and the CI story still has to wire the rule for the gate to block on it. Honest stance: a
   missing tool, an unparseable result, or a linter Camerata doesn't drive end-to-end
   (golangci-lint, rubocop, Checkstyle, Roslyn, …) yields a note in the **Scan coverage** section,
   never a false clean. Mechanical rules **stay out of the AI/LLM review** — a deterministic tool
   runs them instead, which saves tokens. **Architectural rules are never attempted in the preview**;
   they need a bespoke checker and are covered by the AI review instead.

   **Excluded from the preview by design:** **CodeQL** and the **paid cloud tiers** (`layer3_only` —
   too heavy a whole-program build to run locally) never preview; they are CI-story-only (step 6).
   The scan header still shows how many rules were excluded from the AI scan for being mechanical.

   **Deterministic-scan progress.** A **"Deterministic scan" progress indicator** renders **above the
   AI agent-activity drawer**, with an overall done/total bar plus a per-tool row (the floor and each
   preview linter: starting → running → done, with a findings count). It's the primary progress view
   in deterministic-only mode, where the AI drawer is empty.

   **Stop button and stall warning during the scan.** A **Stop** button is always visible while the
   scan is running — you do not have to wait for a stall to stop it. Clicking Stop ends the scan in a
   **Cancelled** state. Separately, if the scan makes no progress for approximately 2 minutes, an
   amber **"No progress — possible stall"** warning appears above the progress indicator. The warning
   is informational; the scan continues until you click Stop or it finishes normally.

5. **Add rules to repo(s)** — writes the governance files onto a `camerata/onboard-governance` branch
   in each repo's **local clone AND pushes that branch to origin — no pull request is opened.** Each
   repo's local clone is resolved from its recorded path (or, as a fallback, a workspace folder). The
   branch is Camerata-managed and regenerated each run (force-pushed), so re-applying is safe. Edit
   the working copy freely, then click **Open governance PR** (a separate, optional button) when ready
   — **Camerata never opens a PR automatically.** **Applying marks the repo onboarded.**

   The files written on apply:

   | File | What it contains |
   |---|---|
   | `AGENTS.md` | Prose rules — the agent's in-context directives. |
   | `CONVENTIONS.md` | Structured, mechanical, and architectural rules — conventions + CI conformance notes. |
   | `.camerata/rules.json` | Armed rule ids — the gate config read by Layer 1. |
   | `.camerata/baseline.json` | Accepted pre-existing debt (the full active-finding set at apply time). |
   | **`.camerata/checks.toml`** | **The SSOT check manifest** read by both Layer 2 (dev-loop runner) and Layer 4 (CI). Each applied CI-tier rule becomes one `[[check]]` entry; mechanical rules carry a concrete command, architectural rules carry a TODO placeholder for the team to fill in. This is the single file you edit to add, remove, or change a custom gate check — one edit covers both the dev loop and CI. |
   | **`.github/workflows/camerata-gates.yml`** | **The generated CI workflow** (a real, runnable workflow file, not a placeholder). It is derived directly from `.camerata/checks.toml`, so it is always consistent with the manifest. Regenerate it any time by clicking **Regenerate CI workflow** in the Rules view. |

   **Apply scaffolds the CI layer; it does not turn CI enforcement on for you.** The two files above
   (the workflow and the `checks.toml` manifest) are *generated and committed to the branch*, and
   onboarding files wiring stories (step 6) for what's left. But the CI layer (Layer 4) is **not
   enforced automatically on apply**: mechanical rules still need their linter provisioned and their
   manifest command filled in where missing, and architectural rules carry only a commented TODO
   placeholder until the team writes the bespoke checker. The workflow runs on your CI only once your
   team reviews it, provisions the linters, and merges it. The *file* is generated; *enforcement* is
   opt-in and manually wired.

   The apply loop is now closed end-to-end: apply writes `.camerata/checks.toml` → Layer 2 reads it →
   Layer 4 is generated from it, all from the same file. See §14 for the full SSOT picture.

6. **Wire CI rules (two separate stories)** — the final step files **two GitHub-issue stories** per
   repo, one per CI enforcement tier:
   - **Create mechanical-rules CI story** — wire the selected **mechanical** rules into that repo's CI
     as enforced lint gates. Mechanical rules map to an existing off-the-shelf linter, so the
     implementation is straightforward: fill in a manifest entry for each rule and you're done.
   - **Create architectural-rules CI story** — wire the selected **architectural** rules into CI.
     Architectural rules need a **custom checker** (no off-the-shelf linter expresses them, e.g.
     "handlers never touch the DB directly") plus team design and scoping before implementing.

   Each story body is **self-sufficient**: it carries a full explanation of the `.camerata/checks.toml`
   single source of truth, the manifest schema with all fields, and (for architectural rules) a
   step-by-step how-to with a worked example. A developer picking up either story has everything they
   need with no additional hand-holding. See §14 for the full SSOT picture.

   Both stories are filed separately so the mechanical track (easy, done in a single sprint) is never
   blocked on the architectural design phase.

   **CI-wiring covers both gate layers, not just CI.** Each story instructs wiring the check into
   `.camerata/checks.toml`, which the Layer-2 dev-loop runner AND the Layer-4 CI workflow both read.
   One entry serves both; there is no "wire it twice" step.

   **Optional CI security rules (opt-in, never auto-recommended).** Two CI/CD-domain rules can
   generate their own security-scan CI stories. They are **never pre-checked and never badged as
   recommended** during onboarding — you opt in deliberately:
   - **`CICD-SEMGREP-SECURITY-SCAN-1`** — **Community Edition** (free, LGPL-2.1 OSS CLI; runs on any
     repo public or private; single-file, light enough for the scan preview, layer-2, and CI) **vs.**
     **AppSec Platform / Pro** (paid, cross-file taint analysis + the Pro rule set; CI / platform
     tier).
   - **`CICD-CODEQL-SECURITY-SCAN-1`** — **public-repo (free)** **vs.** **GitHub Advanced Security
     (paid, per active committer, for private repos)**. CodeQL's free entitlement is **public/open-
     source repos only**; private code requires the paid GHAS license. Either way its whole-program
     database build is heavy, so CodeQL is **CI / layer-4 ONLY** — it never runs at the scan preview
     or in the dev loop (which is also why CodeQL never appears in the scan-time preview).

   Both rules have **no default option** — selecting one immediately shows the amber "must choose"
   state until you pick a tier explicitly.

**Greenfield (a new repo):** name → pick starter ruleset → scaffold the repo with the rules baked in
from commit zero.

Design rationale: [`decisions/2026-06-15_brownfield_onboarding_flow.md`](decisions/2026-06-15_brownfield_onboarding_flow.md).

---

## 4. The Rules view (manage the ruleset)

The Rules page is **rules-only**: model configuration lives in Settings (§17), not here.

Two tables:
- **Project rules** — the rules the project has selected, in one table, **filterable by repo** (a repo
  single-select), with project-level rules shown too. Click a rule to switch its chosen option; remove
  a rule from a repo. Edits persist to the project's ruleset.
- **All rules** — the full corpus (**350+ rules across many language and framework stacks**: Rust,
  Python, Go, JS/TS and its frameworks, Java/Spring, C#/ASP.NET, Ruby/Rails, SQL, fullstack, plus the
  always-on security floor and the agentic governance rules), viewable even when unassigned. Each row
  shows **which repos it's applied to**, with **"Add to repo"** (add a rule to any repo it's not yet
  on — directly here) and a jump to the project-rules table for editing.

**Rule tables are grouped by domain** (derived from the rule's folder in the corpus), collapsed by
default. Clicking a group header expands it; clicking a row opens a **rule-detail modal** showing the
decision question, all options and their rationale, the sources the rule is grounded in, and the
enforcement kind. Tables use the dark Bletchley amber theme.

The Rules view also hosts emit, reconcile, suppressions, custom rules, import/export, and the
repo-path **health check** (§5).

### Scope decides where a rule lives (and where it's emitted)

Every corpus rule carries a **scope**, set by the rule's author. It is inherent to the rule, not a per-project switch. Scope sorts each rule into one of two levels:

- **Repo-local rules** are emitted straight into each chosen repo's governance files (`AGENTS.md`, `CONVENTIONS.md`, and the checks manifest), scoped to exactly the repos you pick for them.
- **Project-level rules** (process rules like branch/commit format, and cross-repo API contracts) apply **project-wide** and are **never written into an individual repo's files**. The gates read them from the project itself. They apply everywhere in the project, always. As of 2026-07-05 (GAP-2 fix) the commit and PR gates are **enforced at server chokepoints**: a human-initiated commit or PR whose metadata violates a configured process rule is **hard-blocked** before git is touched. Previously these rules were configurable but had no enforcement path.

**Adopting a rule by picking an option:** click a rule and choose an option. For a **project-level** rule the target is unambiguous (the whole project), so the pick alone selects it and it appears in the **Project rules** table. For a **repo-local** rule the pick does **not** select it on its own (there would be no way to know which repo you meant), so you choose repos separately (see §10).

### Emit rules locally (with optional escalation to a branch / push / PR)

The **"Emit rules locally"** button in the Rules view regenerates `AGENTS.md`, `CONVENTIONS.md`,
`.camerata/checks.toml`, and `.github/workflows/camerata-gates.yml` from the current ruleset.
Emit is **local-only by default**: it writes the governance files straight into each repo's local
clone with no commit. Three **cascading** toggles escalate from there, each one requiring the
previous to be checked:

1. **Save emits on a new branch**: commit the emitted files onto the managed governance branch.
2. **Push to GitHub**: push that branch to origin.
3. **Open a PR**: open a pull request for it.

Unchecking a level clears the deeper ones, so a request can never push without a branch or PR
without a push. **Push and Open-a-PR require a connected GitHub token**; local-only and branch-only
emits do not. Custom rules are always carried through. (There is no longer a separate
"Emit ruleset to repos (re-emit)" PR button; this single button covers local writes through PRs.)

Click it after editing rules (adding/removing rules, switching options, adding custom rules) to
bring the governance files up to date. It is always safe to re-run (it regenerates from the current
ruleset each time). It requires at least one repo to have a resolved local path; if no local
workspace is configured it reports an error rather than silently doing nothing.

### Reconcile with repos

The **"Reconcile with repos"** button reads what is **actually applied in the repos** and pulls it
back into the project. It reads each repo's emitted gate config (the **local working copy first**,
falling back to the **GitHub governance branch** for repos without a local clone) and matches every
rule id back to its source in the rule-bank, so you see each rule's alternatives and context, not
just the adopted directive. Reconcile then **adopts** the repos' rules into project state: base
selections are mirrored and **custom rules are merged in**, so after reconciling the project
reflects what the repos really enforce (including custom rules). Rules found in the repos that are
not in the corpus are flagged as **"not in rule-bank"** (drift).

**Opt-in rules are not pre-checked.** Rules with the `opt_in_only` flag (currently the two security-
scan rules) appear in rule tables as **"Available"** only — no pre-checked checkbox, no recommended
badge. You opt in deliberately by checking them.

**Custom rules.** Beyond the built-in corpus you can author your own rules. A custom rule is a
**free-text directive**, not a corpus-shaped rule: it carries a short **name**, a **directive body**,
and a **repo multiselect** ("Applies to:") that scopes which of the project's repos it targets.
**Leaving every repo unchecked applies it to all repos** (a project-level custom rule). Adding a name
that already exists edits it.

It has none of the corpus decision/options/enforcement-kind shape, so there is no alternative to
choose and no amber needs-choice gate; it simply emits as a `### CUSTOM-{name}` block alongside the
selected rules in each repo it applies to. You can **create, edit, and delete** custom rules from the
Rules view; deleting one removes it from any repo it was on.

**In practice a custom rule is always a prose or structured rule** — an advisory directive the agent
follows and a human reviews. It can never be `mechanical` or `architectural`, because Camerata has no
off-the-shelf linter mapping or bespoke checker for a rule you just invented. To make a custom rule
deterministically enforced, you would open a story / development task to build that enforcement (a
linter mapping or a custom AST checker); until then it is guidance, not a gate. (See §13.)

### Suppressions registry (read-only)

The Rules view hosts a **central suppressions registry**: an informational, **read-only** listing of
every finding the team has waived across the project's repos (inline `// camerata:allow` waivers and
baseline entries). It is **hidden by default**: nothing is fetched until you press **Refresh**,
which **fast-forward-pulls each repo first and then lists** what is waived. The table has a **Repo**
column (alongside Rule, Source, Location, Reason, Accepted by, Status) and an internal scroll so a
long list stays capped. A waiver is flagged **"stale"** when it no longer matches any live finding
(a dead waiver that is safe to remove).

### Export / import the ruleset

The ruleset JSON is the source of truth and can move between projects or machines:

- **Export** shows the full ruleset JSON and a **"Save JSON…"** button to download it to disk.
- **Import** updates the **project only** (the repos are unchanged until you emit). Paste a ruleset
  JSON; an **"Apply imported rules to:"** repo multiselect lets you re-scope the imported rules
  (leave it empty to keep the scoping baked into the JSON). Custom rules are preserved. Because the
  import does not touch the repos, you must **"Emit rules locally"** afterward to apply it (the UI
  warns you of exactly that on a successful import).

---

## 5. Repo paths (resolution + health)

Each repo resolves to a **local folder** — repos can live anywhere; the path is machine-local and is
**never exported**. The Rules view runs a continuous **health check**: any repo that doesn't point at
a valid local git checkout (common right after importing a project) is flagged with a warning + a
per-repo **Resolve…** button to browse to its folder. A repo is "broken" when its path is unset,
missing, or not a git checkout whose origin matches `owner/repo`.

---

## 6. Governed Development (work items → Units of Work → governed dev)

The **Governed Development** view is built around two objects:
- A **WorkItem** is the requirement/story pulled from a tracker (the normalized model). Today the
  provider is **GitHub Issues**; the WorkItem model is provider-agnostic, and Jira / Azure DevOps /
  GitHub Projects are **planned per-provider adapters, not yet shipped**.
- A **Unit of Work (UoW)** is the dev lifecycle that references a WorkItem.

### Project settings

All project-level configuration lives in the **Settings** nav item (§17), not on the Governed
Development page. The settings that affect runs are:

- **Loop guard** — the maximum number of revise iterations a governed run may take before it stops.
- **Model configuration**: the full tier-map, suggested profile, helper-agent models, Designer band
  toggle, and L3 AI code review config (see §17).
- **Stall thresholds** — two numeric fields (in seconds) that control how long a run can be idle
  before Camerata considers it stalled:
  - **Watched (interactive)** — default 120 s. Applies to dev runs you are actively watching. On
    stall, an amber warning appears in the run panel; the run keeps going and **you decide** what to
    do (alert-only, never auto-cancelled).
  - **Routine (autonomous)** — default **1800 s (30 min)**, a deliberately generous grace period.
    Applies to walk-away autonomous runs (scheduled routines). Because no one is watching them,
    autonomous runs **auto-cancel on stall by default**: on stall the run transitions to **Failed**
    with the stall reason recorded (idle time + threshold). A background sweep enforces this; watched
    runs are never swept. Both values must be positive integers greater than zero; saving zero is
    blocked.

These are project defaults, not per-UoW knobs. Per-run model overrides stay on the UoW card; they
default from the project tier-map and override only that one run.

### Issue Management — pull work items

At the top of the view, an **Issue Management** panel shows the GitHub connection status (`● GitHub
connected`, or a no-token notice). Click **Pull work items** to do a **manual** pull (there is **no
auto-poll**) that pulls **all open issues across every repo in the active project** into a
**WorkItem table with a Repo column**. Click any row to read the full work item (title, body, state,
labels, and an **Open issue ↗** link to the source).

### Create a Unit of Work from a work item

From a work item's detail, click **Create Unit of Work from this issue**. This is **deduped by
external reference**: if a UoW already exists for that item the button reads **Open Unit of Work**
and selects the existing one instead of making a duplicate.

### Author a story from a blank UoW with AI

You don't have to start from an existing issue. The left nav's **New authored story** button creates a
**blank draft UoW** and opens an **authoring panel**:
- A **clarification chat** — describe what you want; the AI drafts an issue title + body and, when the
  requirements are ambiguous, **asks one clarifying question** back (as a structured question with
  options and benefits/drawbacks — see Structured clarifications below). Keep chatting to refine it.
- A **live draft preview** of the title and body as they take shape.
- A **target-repo picker** (the active project's repos) and a **Push to board & link** button.

Clicking **Push to board & link** opens the story as a **GitHub issue** in the chosen repo and **links**
the draft UoW to it; the draft then becomes a normal linked UoW and the standard dev controls take
over. This path is **LLM text generation only** — it drafts an issue, it does **not** write code — so
the governance gate isn't involved here (same class as the chat assistant); the gate stays on the
governed dev run after the UoW is linked. Pushing to the board needs a GitHub token; without a token
(or with Claude unavailable) the chat still saves your turns and tells you AI drafting is unavailable
rather than failing.

### Multiple concurrent Units of Work

You can run **multiple Units of Work at once** — even on the same repo. Each UoW operates in its own
**isolated git worktree** (a separate working directory off the repo's shared `.git` object store),
keyed by its branch. Worktrees live at `<clone>/.camerata-worktrees/<branch-name>`.

What this means in practice: two UoWs on the same repo can run development, update-branch, and ship
operations simultaneously without git checkout conflicts. The gate and all governance rules are
unchanged — worktrees change WHERE the agent works, not WHETHER it's gated. Two UoWs editing the
same lines still produce a normal merge conflict at PR/merge time (expected, resolved at merge).

Worktrees are cleaned up automatically on sign-off. A startup sweep reclaims any that leaked through
crashes. A **disk headroom guard** (default: requires 10 GB free; override with
`CAMERATA_MIN_DISK_HEADROOM_GB`) refuses to create a new worktree if disk space is low — this
protects against the `target/` multiplier when developing a large Rust project with several concurrent
UoWs.

### Structured clarifications (auto-saved, resumable)

Whenever Camerata or the AI needs input from you — during story authoring, investigation, and other
lifecycle phases — it presents a **structured question**: multiple options each with a short
benefit/drawback description, an "Other" free-text escape, and optional multi-select. This mirrors
the `AskUserQuestion` design: you pick from concrete options rather than typing free-text into a chat
box.

Everything is **auto-saved**: open questions and your answers survive a restart. You can close the
app, come back later, and resume exactly where you left off. The cross-UoW **"Needs you" queue** in
the Governed Development view lists every open question across all stories, so you never miss a
waiting pause point.

The investigation phase can **pause mid-run** when the agent raises a question, park the run at
"Awaiting clarification," and **resume** (re-spawn the gated agent with the Q+A in context) once you
answer. The **development** phase can likewise pause — for a human-review *escalation* — and resume
from a checkpoint (see the next section).

### Human review escalations (a run pauses, you decide, it resumes)

Beyond clarifying questions, a governed **development** run can pause for a **review escalation** — a
decision only you should make. This happens when the agent's work meets the escalation condition of a
rule you selected (for example, modifying an existing test, which `AGENTIC-NO-TEST-TAMPER-1` calls for
review on). Instead of failing, the run **pauses**: it checkpoints its state, leaves the worktree
intact, and parks at **"Needs your review."**

The paused run shows up in the same **NEEDS YOU** queue. Open it and you get a review panel:

- **What happened** — the rule, what specifically met its condition, and the agent's reasoning.
- A **chat** with the lead-engineer assistant to discuss it (chatting never decides anything).
- Three decisions:
  - **Approve & resume** — the change is fine; the agent resumes from where it stopped and continues.
  - **Amend & resume** — give a free-text correction; the agent resumes with your directive applied.
  - **Reject & revert** — the change is discarded and the run stops cleanly.

Approve and Amend **re-spawn the agent from the checkpoint** (its partial work is still on disk), so it
continues rather than restarting. A toast notifies you when a run pauses while you are on another tab.

Which rules escalate — and whether a condition **hard-pauses** (stops for you) or **soft-flags** (logs
and continues) — is driven entirely by the rule corpus: any rule whose selected option carries an
escalation condition triggers this, not just test-tamper. The agent self-reports when its work meets a
condition; a deterministic backstop catches the test-tamper case mechanically. See
`docs/ESCALATION_RESUME_DESIGN.md` (the as-built design) and `docs/RULE_AUTHORING.md` (how to add an
escalating rule).

### The 3-phase Development cockpit

The Governed Development surface for a selected UoW is a **three-phase cockpit** with free
navigation between phases. The three phases are:

> **Intake → Investigation & Refinement → Development**

Unlike the old strict-sequence lifecycle strip, you can navigate between phases freely. Status
within each phase is **informational** — the cockpit does not lock you into the current phase.
The underlying `UowStage` server state (Intake / Investigating / DecisionsApproved / Development /
AwaitingQa / SignedOff) still gates the actual AI runs, but the UI surfacing is free-nav.

Each phase has a **Finish / Reopen** control that marks it done (a visible tick) and persists that
flag so it survives a restart. Finishing a phase is informational — it does not advance the stage
automatically, and you can always reopen.

#### Phase 1: Intake

The Intake phase renders the whole story inline (no separate modal). The page layout from top to
bottom:

- **Update-branch notice**: any pending upstream branch update is shown at the very top.
- **Repos in scope**: the repos-in-scope selector sits directly under the update-branch notice.
  Set which repos the story touches and which branch each targets. This affects the **per-repo Ship
  panel** in Development (only in-scope repos appear); out-of-scope repos are not listed.
- **Context for the investigation agent**: a large free-text field. Add context the story itself
  doesn't capture; the agent receives it at investigation time. The field is **editable and
  deletable** after it is added (it is not locked once saved).

A **▶ Begin investigation** button (with a model select) starts the investigation run from Intake.
The model defaults to the project's strongest tier. Clicking it transitions the UoW to
**Investigating** and starts a single gated investigation agent.

Without `CAMERATA_LIVE_BUILD=1` the run completes with a placeholder note; with it set and `claude`
connected, a real gated investigation agent runs.

#### Phase 2: Investigation & Refinement

The Investigation phase hosts the investigation agent's output and the prose interface contract.

**Investigation chat and decisions.** The investigation/refinement agent chat transcript is shown
here. Decisions the agent surfaced are listed and can be approved or rejected. When all decision
records are approved, **Approve decisions** advances the UoW to **Decisions Approved** — the gate
state the server requires before development can start.

**Settling the prose contract (cross-repo work).** If the story's work **crosses a contract
boundary** (e.g. two repos need to agree on an API shape), mark **"this work crosses a contract
boundary"** and write the agreed interface contract in the contract text field. This prose contract
is the R3.g artifact — the single authoritative reference that:

- The development agent uses as its primary spec.
- The **integration gate** checks after multi-repo fan-out assembly.

**Contract precondition.** For a story marked as crossing a boundary, the server **blocks
development** until the contract field is non-empty. Attempting to start a dev run without a
contract returns a 400 with an explanatory message. The precondition only applies when
`crosses_boundary = true`; single-repo stories are unaffected.

A **Finish Investigation** button marks the phase done. **Reopen Investigation** is always available.

#### Phase 3: Development

The Development phase is the main run-and-ship surface.

**Run development (governed).** Three per-tier model selects appear — **Strongest**, **Balanced**,
and **Fast** — each defaulting from the project tier map and overridable for this run only. Click
**▶ Run development (governed)** to start the build.

**Brownfield vs. greenfield.** When the UoW's repo has a local clone (the normal case), the agent
edits the existing codebase in-place on the UoW's branch in its isolated worktree. When there is no
local clone, the runner scaffolds a new app from the plan in a throwaway temp directory (greenfield).
Camerata picks the path automatically.

**How the tiered run works:** the **Strongest-tier agent is the orchestrator and lead.** It does the
complex, one-way-door work itself. For well-scoped subtasks it can use the governed `delegate` tool
to hand the task to the Balanced or Fast tier. The gate is universal: every child is spawned gated
(`gated_write` only, `Task` disallowed), delegation is only one level deep, and escalation is
parent-driven (a child returns `INCOMPLETE:` and the orchestrator re-handles it).

**For multi-repo stories**, the lead can use the `fan_out` tool to dispatch **concurrent per-repo
workers**, each write-isolated to its own repo directory. Workers assemble their outputs and the
**integration gate** checks that the assembled outputs are consistent with the settled contract.
Fan-out is depth-limited (workers cannot fan out or delegate further). The integration gate's live
LLM check path is implemented but the wiring into the dev run orchestration path is **in progress
(#105-followup)** — the contract precondition and the synchronous gate seam are live.

Without `CAMERATA_LIVE_BUILD=1` the run is token-free/scripted and gate enforcement is still real.

**Bootstrap run — skip layer-2 checks (the chicken-and-egg escape hatch).** The development-run
control carries a **default-OFF** "bootstrap run — skip layer-2 checks" toggle. Layer 2 is
fail-closed: a repo with a manifest but no lint/test wired fails as "could-not-run," which is
correct governance but creates a deadlock — the very run that would install the linters fails layer
2 because the tools aren't there yet. Enabling this toggle skips only the post-task layer-2
lint/test bounce for that one tool-installing run. **Layer 1 (deny-before-write) and the decisions
gate still apply.** It is deliberate and visible, never silent or sticky.

**Per-repo Ship panel.** Below the run controls, a Ship panel shows one row per in-scope repo (set
in Intake). Each row has its own push, PR, and comment controls. A **"Ship all repos →"** chain
button runs push → open PR → comment across all in-scope repos in sequence. The per-repo breakdown
reflects the repos that will be touched by the story's fan-out work.

**Done / archive.** A **"Mark Done (archive)"** button transitions the UoW to an archived state
(read-only in the cockpit). The UoW is never deleted — reopening Development from the archived
state is always available. Done UoWs remain visible in the left nav (with a done indicator) and in
the history.

**Run liveness: stall warnings and the Stop button.** Camerata watches for lack of progress, not
elapsed time. A legitimately long build that keeps emitting output is never flagged; a process that
goes silent is. A **■ Stop** button is always visible while a run is in progress (you do not have
to wait for a stall). If a dev run produces no progress for the project's configured Watched
threshold (default 120 s), an amber banner appears. For autonomous routines, a stall auto-cancels
the run to **Failed** (with the stall reason as the diagnostic); for watched runs, you remain in
control. See "Stall thresholds" in Project settings for the configurable values.

**Failed vs. Cancelled.** Failed (with reason) = the run stopped due to an error or stall. Cancelled =
you explicitly stopped it. Both are terminal; only Failed carries a diagnostic reason.

**Other controls on every UoW card:**

- **PR lifecycle** — a dedicated panel on each UoW card for the push/PR/feedback loop:
  - **Push & open PR** — pushes the UoW branch and opens a PR with a **user-selected base branch**
    (picker in the console). The resulting PR number + URL are stored on the UoW.
  - **Pull PR info** — fetches current PR state, CI check status (passed/failed/pending), and
    comments. Camerata first checks its stored PR number; if none is stored, it searches by head
    branch — so a PR opened directly in GitHub is found automatically and its number is backfilled.
  - **Resolve with agent (gated)** — feeds open review comments and failing CI check names to a
    gated agent that edits the worktree to address the feedback. Same gate as the dev run; the
    agent cannot commit or push itself.
  - **Comment** — posts a comment on the PR/issue from the console.

- **Open work item** — opens the full work-item modal right from inside the UoW (next to the
  retained **Open issue ↗** link). The modal shows the title, body, and a **Comments** section that
  fetches and renders every comment on the source issue (author + timestamp + body). The
  create/open-UoW affordance is hidden here (you're already in the UoW).
- **Add comment to issue** — write a comment posted back onto the source issue via the tracker
  adapter. The comment box has **GitHub-style @-mention autocomplete**: type `@` and a dropdown of the
  repo's assignable users appears; click one to insert `@login`. This replaces the old "Ask the team"
  clarify panel, which has been removed. (The mention set is GitHub's repo **assignees** — the
  practical mention set, provider-specific; a per-provider user search is the future generalization.)
- **Update branch (AI-assisted)** — Camerata's equivalent of GitHub's PR "Update branch": pick a source
  branch (grouped **Local** / **Origin**) and merge it **into** this UoW's branch. A clean merge commits
  automatically; on a conflict a **single gated agent** resolves the conflict markers and stages the
  files, and the server completes the merge commit. The agent runs behind the same gate as every
  Camerata agent (gated tools only, no `git`, can't spawn sub-agents) — it never commits or pushes
  itself. It's **fail-closed**: with live build off, conflicts abort the merge with an honest "needs the
  AI resolver" message (a clean merge still succeeds); and any path left conflicted aborts the whole
  merge (`git merge --abort`), so a model claiming success without resolving is caught.
- **Pull latest work item** — re-pull just this one item from the tracker (a full refresh, no cache).
- **Sign off this run** — review the run's diff + gate results (rules in force, deny/allow tallies,
  total bounces) and **✓ Sign off this run**; provenance is written back. Sign-off also triggers
  worktree cleanup.

### Gate self-check (GO / NO-GO)

The view hosts a one-click **Run gate self-check** that proves the gate loop is actually wired
*before* you trust it with a work item. It runs the deterministic end-to-end probe (no model call, no
tokens): it plants one violation for **every enforced gate rule** (the security floor), confirms
**Layer 1 denies each one** before it can touch disk, confirms a clean write is **allowed** (the gate
isn't deny-all), and confirms **Layer 2 bounces once on a planted violation and resolves on the
revise pass**. It reports a single **GO / NO-GO** verdict with the floor count (e.g. "6/6 floor rules
enforced"). GO means deny-before-execute + bounce-and-revise are both live. The same probe runs in CI
and as `camerata gate-probe` on the CLI.

> **Note on enforcement transparency:** today the gate blocks and bounces the agent without a
> human-visible audit log of each denial. A visible enforcement record (showing which rule fired, on
> which file, during which run) is planned but not yet built.

---

## 6a. L3 AI code review (opt-in per project)

The **L3 agentic code reviewer** is an optional AI reviewer that runs **in parallel with Layer-2**
after each dev-run iteration. It is the third enforcement stage in the canonical model:
**L1** Security · **L2** Mechanical · **L3 AI code review** · **L4** Origin/CI.

### Enabling it

In **Settings** (§17), the per-project **Model configuration** section includes an **L3 AI code
review** subsection with:
- An **enable/disable toggle** — off by default (opt-in per project; a project without L3 configured
  never runs the reviewer).
- A **model selector** (empty falls back to the project's Balanced tier model.

The setting is per-project, not per-UoW. Enabling it applies to every subsequent dev run in that
project.

### What it does

After the dev agent finishes each iteration and Layer-2 checks run, the L3 reviewer:

1. Runs `git diff HEAD` in the agent's worktree.
2. Sends the **story + selected rules + diff** to the configured model.
3. Returns a PASS or BOUNCE verdict.

A BOUNCE feeds the bounce reasons back to the dev agent on the next iteration — same
bounce-and-revise mechanism as Layer-2. A PASS is recorded as a `layer-3` gate event in the run log.

If the diff is empty (nothing changed), the L3 review is skipped (logged: "Layer-3 skipped: no diff
to review").

### Isolation: blind to other agents

The reviewer sees **only** the story, the rules, and the diff — it is deliberately blind to the
investigation notes, orchestrator transcripts, and developer reasoning. This isolation prevents
rubber-stamping: the reviewer is spec-grounded and implementer-blind.

### When to use it

- **Cross-cutting rules that are hard to lint** — prose and structured rules that the agent should
  follow but that Layer-2 cannot mechanically verify. The L3 reviewer can catch a rule violation
  that regex/lint cannot.
- **Spec conformance checking** — verifying that the diff actually implements what the story says,
  not just that it compiles.
- **High-stakes stories** — when the cost of a missed rule violation is high enough to justify
  additional AI inference per iteration.

The tradeoff: L3 adds one model call per dev iteration. At the Balanced tier this is modest; at the
Strongest tier it is more expensive. Match the model to the risk.

---

## 6b. Design Canvas (co-design a work tree, publish as a batch)

The **Design Canvas** is where you co-design a hierarchy of work with an AI, then publish the whole tree into GitHub as a batch of linked issues. Where §6 authors a single story, the Design Canvas authors a **design**: a top node (usually an Epic or Initiative) plus the tree of work it decomposes into. Everything stays a draft until you publish, so you can iterate freely with nothing on the board.

### Opening the canvas

Open the Design Canvas from the cockpit. With an active project selected, the empty state lists that project's **saved designs**. Each row shows the design title, a **status badge** (draft is neutral, published is green, archived is muted), a "Type · N nodes" meta line, and when it was last updated. Click a row to open its tree. If no project is active you get a hint to pick one first.

### Starting a new design

The **New design** action is front and centre. Pick the **root node type** (Epic by default) and create it. A blank root node opens with its own authoring chat.

### Designing the tree with the AI

Talk the design through in the node's chat, the same author loop as story authoring. The AI proposes **child nodes at the types your project's hierarchy allows** (it drafts against your saved hierarchy schema, not a fixed Epic to Story shape). Each proposed child shows as a "NodeType: Title" heading with its drafted body rendered below. Accept the proposals to **materialize** them: each becomes a draft node linked under its parent, and the tree renders immediately in the relationship table with the hierarchy in place. The **+ Add child** button offers only the child types your schema permits for the selected node (a leaf type shows no button).

### Mockups per node

A node can carry a UI **mockup**. Open the mockup panel and describe the UI you want, or **leave the box blank**: a blank prompt still generates a mockup grounded in the node's own story plus its parent's context. The generated HTML previews live in a sandboxed frame and is saved on the node.

### Auto-save, archive, and delete

Designs are **saved automatically** on every change; the open design shows a subtle "Saved" indicator. In the header you can **Archive / Unarchive** a design (it moves between the `draft` and `archived` badges). Back in the list, a two-step inline delete (trash, then "Confirm?") removes an entire design (root plus every descendant). Design status (draft/published/archived) is separate from a story's development status: publishing a design is not the same as a story finishing a dev run.

### Publishing

When the tree is ready, **Publish all** walks it top-down and creates one GitHub issue per node, wiring parent/child as sub-issues (up to 8 deep) and applying a `type:<name>` label per node. Publishing is fail-soft per node: you get the created issue numbers plus any warnings, and the design is marked **published**. The result lands in the same grouped issues table as the rest of your work. (GitHub is the only publish target today.)

---

## 7. The rules that govern it

Five enforcement points, of which four are fully deterministic (binary pass/fail, no LLM judgement)
and one (L3) is AI-driven:

| Point | Enforces on | Example |
|---|---|---|
| **L1** (MCP tool gate) | one write's file content, before it executes | no hardcoded secret reaches disk |
| **L2** (CheckRunner) | one task's diff, after | the repo's own format/lint/test (e.g. `cargo fmt`/`clippy`/`test`, `ruff`/`pytest`, `npm run lint`/`test`, `gofmt`/`go vet`/`go test`, `bundle exec rubocop`/`rspec`, `./mvnw verify`/`./gradlew check`, `dotnet format`/`build`/`test`) |
| **L3** (AI code reviewer, opt-in) | diff + story + rules, per iteration | rule violation or spec non-conformance the agent missed (see §6a) |
| **Integration gate** | the assembled per-repo outputs vs. the prose contract | API contract between repo A and repo B agrees; live check in progress (#105-followup) |
| **VCS-action gate** | commit/PR/branch metadata | the PR title + commit subject carry the ticket id — **enforced at server chokepoints as of 2026-07-05**: a non-compliant human-initiated commit or PR open is hard-blocked before git is touched; machine-generated commits use an auditable bypass |

> **Layer numbering note — reconciled.** The canonical stages are **L1** Security · **L2** Mechanical ·
> **L3** AI code review · **L4** Origin/CI, and this guide is now reconciled to them: every prose
> reference that means CI reads **L4**, and **L3** is the AI reviewer. The one remaining legacy is
> the `layer3_only` rule-corpus flag in **code**, which means "CI-only (L4)" despite its name; it is
> intentionally NOT renamed to avoid a code migration, so grepping the source for "layer3" finds the
> CI tier. The canonical model in `ENFORCEMENT_MODEL.md` is the reference.

**Layer 2 is cross-language and polyglot — across all seven supported languages.** It is no longer
Rust-only: for each worktree it runs the checks for every **supported** language present — **Rust,
JavaScript/TypeScript, Python, Go, Ruby, Java, and C#** — using the **repo's own pinned toolchain**
(the same tool versions the repo's CI uses, taken from the repo's lockfile/wrapper/SDK pin, never
baked into Camerata). It is **fail-closed**: if a toolchain is missing, a check isn't defined, or dep
install fails, the task is treated as **not verified** (an error), never a clean pass. So code is
pre-linted at dev time across all seven languages.

> Each language uses its own pinned tooling: Rust via `cargo`, JS/TS via the lockfile-detected
> package manager (`npm`/`pnpm`/`yarn`), Python in an isolated venv from `requirements.txt`/
> `pyproject.toml`, Go via `go.mod`, **Ruby** via `Gemfile.lock` + bundler (`bundle exec rubocop` +
> `rspec`/`rake test`), **Java** via the repo's `./mvnw verify` / `./gradlew check` wrapper, and
> **C#** via `dotnet format`/`build`/`test` (SDK pinned by `global.json`). This matches the seven
> languages the rule corpus ships rules for — the layer-2 language gap is closed.

Rule scopes: **corpus-global**, **repo-local** (from onboarding), **cross-repo** (contracts),
**process** (workflow conventions). The agent has no `git`, so Camerata is the sole committer.

---

## 8. The in-app assistant

The floating chat bubble is a single, context-rich assistant. There are no modes to pick. Every turn it is grounded in all of its sources at once, and your prompt decides which it leans on: ask "how do I onboard a repo" and it draws on the docs; ask "what did my last audit find" and it draws on the live project state; ask "where are we at" and it draws on the development state across every Unit of Work.

**Conversation context is retained** across messages — you can ask follow-up questions in the same session and the assistant remembers what was said earlier in the thread. The assistant is also **grounded on your active project** (its repos, ruleset, onboarded state) and on **any issues you've pulled in** via the Issue Management panel.

### What the assistant can see

A **"What this assistant can see"** strip at the top of the panel lists its context sources and whether each is currently loaded:

1. **Technical reference** (`docs/TECHNICAL.md`), baked in at compile time: how Camerata works.
2. **User guide** (`docs/USER_GUIDE.md`), baked in at compile time: flows, how-to steps, feature descriptions.
3. **Governance rules catalog** (the live corpus from `GET /api/corpus-rules`, fetched once per session): every rule with its domain, scope, and alternatives.
4. **Development state**, fetched from `GET /api/uow` and refreshed each turn: every Unit of Work with its lifecycle stage, gate/bounce status, and sign-off state, so a "where are we at" question returns a real cross-project status report.

A fifth row, **Focused finding**, appears only when you click **"Ask"** on a specific audit finding; it injects that finding's rule-id, path, and line additively so the assistant can zoom into one violation without losing the rest of the context.

The technical reference, user guide, and rules catalog form a stable prefix that is cached for cheap reuse; the development snapshot refreshes each turn.

### Honesty guardrail

The assistant answers **only** from those sources. When a question is covered by none of them, it says so verbatim rather than guessing, so it never invents features or facts that do not exist. That is the same honesty line drawn everywhere else in Camerata.

---

## 9. Update detection and rule drift

Camerata surfaces two update signals:

### App updates

When a new version of Camerata is available, a **banner appears at the top of the UI** with the release notes and an "Update" button. This is a one-click reminder, not an auto-update.

### External drift signals (distinct from verification)

Camerata watches for changes that originate OUTSIDE your ruleset. These are separate from the
verification ladder below: they concern a rule's presence and version relative to the corpus and your
local clones, not whether its verification is still trustworthy.

- **Repo path health check** (issue #33): the Rules view shows, per repo, whether its recorded local
  path still resolves to a git checkout, and lets you re-resolve a broken one.
- **Update detection**: an app-update banner appears when a newer Camerata release is available, and the
  Rules view can surface when a rule you have already APPLIED to a repo has since changed upstream in the
  corpus, so you can review and re-apply the new version.

### Verification badges

Every rule carries a provenance badge. These are **read-only in the app**: the `verified` tier is set only
by the maintainer-side verifier tool, never from the product.

| Badge | Meaning |
|---|---|
| ✓ **Verified** (green) | A human explicitly approved this rule, via the maintainer verifier tool. The gold standard. |
| **Grounded** (blue) | Rule is cited in a corpus source (linter, doc, framework best practice). The cited source(s) are listed in the rule's **detail panel**: open the rule and read the **Sources** section (a rule may cite several). |
| **Draft** (gray, italic) | AI-generated rule, not yet grounded. Advisory only, and never auto-recommended during onboarding. |

Re-verification is a deliberate maintainer act, done with the repo-side verifier tool (which lands as a
reviewed PR), not from inside the app.

> **Planned (not yet built):** detecting when a rule you have *applied* has since changed in the corpus
> (because you updated Camerata to a version carrying a newer version of that rule), then offering a per-rule
> "update to current version" diff so you decide whether to take it. It is informational and never
> auto-updates an applied rule. Tracked in [#66](https://github.com/zernst3/camerata-orchestrator/issues/66).

---

## 10. Single-rule editing (project and repo scope)

The **Rules view** (§4) allows fine-grained rule editing at three scopes:

### Editing a single rule

Click any rule in the **Project rules** table to open its **detail modal**. You can:
- **Switch its chosen option** — if the rule has multiple alternatives (e.g., "no hardcoded secrets" vs. "secrets vaulted"), select a different one. The choice is per-repo.
- **Add custom sub-options** — for rules that support local overrides, you can tack on a custom directive (e.g., "allow X in this repo only").
- **View the rule's full definition** — the decision question, all available options + rationale, the sources it's grounded in, and the enforcement kind.

### Repo-scoped overrides (choose repos right in the modal)

A repo-local rule applies to **all the repos you scope it to**. You choose those repos **right in the rule detail modal**: open the rule and use the **"Applies to repos"** section, which shows **one checkbox per project repo**.

- **Check a repo** to add the rule there (if the rule wasn't selected yet, checking a repo selects it with its default option).
- **Uncheck a repo** to remove it. Unchecking the **last** repo drops the rule entirely (removing every repo is the same as unselecting the rule).

The older per-row **"Add to repo..."** dropdown in the **All rules** table still works; the in-modal picker is simply a second, more direct path. To carve out a single repo you can also add a custom rule that applies only to that repo (a rule you author locally).

### Project-level rules (always apply everywhere)

Some rules are **project-level** by their scope and apply to **every repo** in the project:
- Examples: **process** rules like commit format (`AB#{id}`) and branch naming, and **cross-repo** API contracts.
- They apply project-wide and are **never emitted into an individual repo's governance files**. The gates read them from the project itself. The commit and PR variants are enforced at server chokepoints (see §7).
- In the rule detail modal they show a static **"Applies project-wide (all repos)"** line instead of the per-repo checkboxes.
- You edit them only in the **Project rules** table, and the change flows to all repos on the next emit.

### Custom rules

Beyond the corpus you can author **two kinds of custom rules**:
- **Repo-scoped** — applies to a single repo; lives in its `.camerata/AGENTS.md`.
- **Project-scoped** — applies to every repo; lives in the project store (cross-repo rules read it).

Both custom types flow through the Rules view editor and the emission system. A custom rule is a free-text directive (name + body + scope) and carries **no enforcement kind**, so it is **prose or structured in practice** — never mechanical or architectural unless you open a development task to build a linter mapping or custom checker for it (see §13). Deleting a custom rule removes it from any repo it was on. Editing one changes only that rule.

---

## 11. Deep-report Markdown export

When you run an **onboarding audit** with the **"Deep report"** checkbox enabled, Camerata runs three advanced analysis lenses over each repo:

1. **SOC-2 gap analysis** — maps detectable practices onto SOC-2 Common-Criteria controls and reports **gaps** (what controls appear to lack implementation). This is a **gap analysis, never a compliance report or certification** — it is advisory and model-inferred. **Note:** the SOC-2 lens is behind the `soc2` feature flag, which ships **OFF** (§12); it runs only if you re-enable the flag.
2. **Deep security audit** — a layer deeper than the deterministic floor: authorization on write paths, sensitive-data handling, secret flows, trust boundaries. Findings flow into the same triage + tech-debt workflow as the standard audit.
3. **Threat model** — a structured STRIDE-flavored view: entry points, trust boundaries, data stores, sensitive-data paths, and threats + mitigations.

### Cost and timing

The deep tier is the **most expensive audit option** (~3× the standard audit cost, run on the strong model). It is **strictly opt-in** because of the cost; baseline audits (deterministic floor + AI architectural scan) run by default and are much cheaper.

### The deep-report export

After the audit, the **deep report is rendered as structured Markdown**. You can **export it** (download as `.md`) from the findings screen. The markdown includes:

- **Advisory notices** — prominent labels stating the output is model-inferred, a static-code analysis (not a pen test), and not externally validated.
- **Scoped-scan table** — deterministic security findings from the changed files (hardcoded secrets, raw SQL, etc.) — the same floor that gates are enforced on.
- **SOC-2 gap summary** — controls it appears the codebase leaves gaps in, with severity rankings.
- **Deep security findings** — architectural + authorization issues, trust-boundary violations, sensitive-data handling gaps.
- **Threat model** — entry points, data stores, trust boundaries, and the threats that apply to each.
- **SOC-2 control index** — a deduped cross-reference of all Trust Services Criteria controls touched by the scan.

The export is markdown, so you can **paste it into a GitHub issue, a team wiki, or a compliance readiness doc** for review and discussion.

### Important: the SOC-2 output is advisory

The SOC-2 lens produces a **gap analysis** — a conversation-starter about what controls may be underimplemented, not an audit-grade report. For true SOC-2 compliance work, pair Camerata's gap analysis with a real readiness review and, if aiming for certification, an accredited assessor. Camerata is a **governance tool**, not a compliance tool.

---

## 12. Feature flags (opt-out)

Camerata uses **feature flags** to gate features that are optional or under evaluation without code
branching. Every flag defaults **ON** (opt-out model): a flag absent from all sources is enabled. A
flag is turned off only by setting it to `false` explicitly.

### Setting flags

Flags are read from a **`.camerata/features.toml`** file at startup, and can be overridden per-flag by
an environment variable. Only an explicit `false` disables a flag; an absent or any other value leaves
it at its configured default.

```toml
# .camerata/features.toml
soc2 = false
```

```bash
export CAMERATA_FEATURE_SOC2=false   # env override; disables the flag
cargo run -p camerata-ui
```

### Current flags

There is currently **one** runtime flag:

| Flag | Env override | Default | Shipped value | What it controls |
|---|---|---|---|---|
| `soc2` | `CAMERATA_FEATURE_SOC2` | ON (opt-out) | **OFF** (the repo's `.camerata/features.toml` sets `soc2 = false`) | The SOC-2 gap-analysis lens in the deep audit tier (§11). |

So although the flag *defaults* on under the opt-out model, **SOC-2 is shipped OFF**: the checked-in
`.camerata/features.toml` sets `soc2 = false`, so the SOC-2 lens does not run unless you re-enable it.
When `soc2` is false, only the SOC-2 lens is skipped — the deep-security and threat-model lenses of the
deep tier still run, and the deep report is valid with an empty SOC-2 section. (Deep-security and
threat-model are part of the deep tier and are not separately flag-gated today.)

### Why flags?

Flags let us:
- **Ship features on day one** without inflating defaults (e.g., SOC-2 analysis is optional until validated).
- **A/B test** features with real users before committing to permanent defaults.
- **Kill or pivot** features fast if they don't land (no messy deprecation dance).
- **Support air-gapped or offline deployments** that opt out of certain AI passes.

---

## 13. Understanding rule types

Every rule in Camerata's corpus carries an `enforcement` field that answers one question: **how objectively can conformance be checked?** That single property decides where the rule is written and how it's enforced.

![Camerata enforcement model — rule sources feeding the check layers](enforcement-model.svg)

The diagram above is the canonical **stage** model — L1 Security · L2 Mechanical · L3 AI code review · L4 Origin/CI — and how rules feed each layer (full write-up: [`ENFORCEMENT_MODEL.md`](ENFORCEMENT_MODEL.md)). The table just below is a **different, related axis**: the rule *enforcement modalities* (how objectively a rule can be checked), which determine *what kind of wiring* a rule needs.

> **Numbering note — reconciled.** This section and §14 now follow the canonical stage model above:
> any reference that means **CI** reads **L4**, and the AI code reviewer (§6a) is the actual **L3**.
> The one remaining legacy is the `layer3_only` rule flag in **code**, which means "CI-only (L4)"
> despite its name; it is intentionally not renamed to avoid a code migration, so grepping the source
> for "layer3" finds the CI tier. See §7 for the canonical enforcement table.

### The four-layer model

| Rule tier | Blocks the agent's write (gate) | In-loop dev checks | Your repo's CI | Scan report |
|---|---|---|---|---|
| **Deterministic floor** (built-in secret/key/SQL/TLS checks) | Yes | No | No dedicated job | Yes |
| **Mechanical** (off-the-shelf linter, e.g. ESLint rule, Clippy lint) | Some | Yes (built-ins + your manifest checks) | Yes (generated) | Yes |
| **Architectural** (you build a custom checker, e.g. API layering via dependency-cruiser) | No | Once you register your checker in `.camerata/checks.toml` | Yes, once you build it | AI review only |
| **Prose / Structured** (advisory) | No | No | No | AI review |

**Security gate rules** are not a corpus category you author into. They are a small, hardwired set of rule-ids built directly into the MCP gate (see §7). They run before any write touches disk, require no build, and are always on. You do not select them; they cannot be turned off per-repo. The audit surfaces them as "deterministic floor" findings because they are the exact same checks the gate enforces on new writes. There are currently seven gate rules (`SEC-NO-HARDCODED-SECRETS-1`, `SEC-NO-RAW-SQL-CONCAT-1`, `ARCH-NO-SECRETS-IN-URL-1`, `SEC-NO-PRIVATE-KEY-1`, `SEC-NO-VENDOR-TOKEN-1`, `SEC-NO-SECRET-FILE-1`, `SEC-NO-DISABLED-TLS-1`).

The remaining four are the corpus enforcement modalities, from most human-judgment to most automated:

| Bucket | What it means for you | Where it's enforced |
|---|---|---|
| **Mechanical** | An existing linter catches it. Every mechanical rule maps to a real, named linter rule in a per-language tool (clippy, ruff, eslint, golangci-lint, etc.). | Local layer-2 check runner (fast, in the dev loop, across all detected languages) **and** CI (authoritative backstop). |
| **Architectural** | Machine-decidable but needs a custom AST check — no off-the-shelf linter expresses it (e.g. "handlers never touch the DB directly"). | Custom CI check (or agent directive as a fallback while the checker is being built). |
| **Structured** | A concrete design contract with a clear conform/violate answer — but not lint-able. Examples: "repositories return domain types," "API version lives in the URL prefix," "cursor not offset pagination." A human can verify it objectively; a linter cannot. | PR review (human, binary yes/no). |
| **Prose** | A principle or idiom where a human must judge conformance: "interfaces are small and cohesive," "optimization by default," "errors are wrapped with context." Reasonable engineers may weigh these differently on the margin. | PR review (human judgment). |

### Opt-in CI security rules (never auto-recommended)

A few `mechanical` rules carry an **`opt_in_only`** flag: they are grounded against a real tool but are
**never pre-checked** during onboarding — you opt in deliberately. The two security-scan rules
(`CICD-SEMGREP-SECURITY-SCAN-1`, `CICD-CODEQL-SECURITY-SCAN-1`, §3 step 6) are the current examples;
they exist to generate a security-scan **CI story** for a DevOps engineer, not to constrain the agent's
code, and they have **no default option** so selecting one forces a conscious tier choice. CodeQL also
carries **`layer3_only`** — its whole-program database build is too heavy to run at the scan preview or
in the dev loop, so it is CI-only (L4 in the canonical model) and never appears in the scan-time
preview. Note: the `layer3_only` flag name is a legacy artefact that predates the L3 AI reviewer;
it means "CI / L4 only", not "only at L3". See §7 for the canonical stage numbering.

### The objectivity spectrum

One way to read the four modalities is as a single spectrum: how far can the conformance check shift from human to machine?

| Modality | Conformance test | Written to | Enforced by |
|---|---|---|---|
| prose | human judgment / matter of degree | `AGENTS.md` | PR review |
| structured | human, binary contract | `CONVENTIONS.md` | PR review |
| mechanical | existing linter | `CONVENTIONS.md` + CI | local check runner + CI |
| architectural | bespoke AST check | `CONVENTIONS.md` + CI | custom check |

### Prose vs. structured: the line that matters most

Both prose and structured rules carry the same TOML shape and both live outside CI — they are directives the agent follows and that engineers review. The difference is one of judgment:

- **Prose** rules require a human to *judge* conformance. They live in `AGENTS.md` because they are principles the agent reads and applies by spirit.
- **Structured** rules require a human to *verify* conformance against a clear binary contract. They live in `CONVENTIONS.md` because they are concrete, citable conventions.

"The API version lives in the URL prefix" is structured: you can check any endpoint and give a definite yes/no. "Interfaces are small and cohesive" is prose: engineers weigh it and the answer is a matter of degree.

### Where rules are written

`arm.rs` routes rules to files at emit time: `prose` → `AGENTS.md`, everything else (`structured`, `mechanical`, `architectural`) → `CONVENTIONS.md`. That routing is the live source of truth (see `crates/server/src/arm.rs`). The format string at the top of each generated file explains this for anyone reading the repo without Camerata open.

### Custom rules are always prose or structured

The four modalities above describe **corpus** rules. A rule *you* author (a custom rule, §10) is a
free-text directive with no `enforcement` field, so in practice it is only ever **prose** or
**structured** — an advisory directive the agent follows and a human reviews. A custom rule cannot be
`mechanical` or `architectural` on its own: those require an existing linter mapping or a bespoke
checker, which Camerata can't conjure for a rule it has never seen. To enforce a custom rule
deterministically, open a development task to build that linter mapping or custom check — then it
graduates out of the advisory tier.

### Rule provenance badges

Every rule also carries a `verification` badge (shown in the Rules view):

| Badge | Meaning |
|---|---|
| **Verified** | A human explicitly checked the rule and its cited sources. No agent may set this. |
| **Grounded** | The onboarding agents found and cited at least one real source (linter rule id, language spec, framework best practice). |
| **Draft** | AI-generated, not yet grounded. Advisory only; never auto-recommended during onboarding. |

The mechanical rules in the current corpus are grounded (each maps to a real, named linter rule that was validated to exist). None are verified yet — that is a deliberate human-only step the maintainer keeps. Grounded is the baseline for a shippable rule; verified is the gold standard you can cite to an auditor.

---

## 14. Wiring CI gates: the check manifest (SSOT)

When you click the onboarding buttons to create CI stories (§3 step 6), the stories teach a single
wiring model: **one entry in `.camerata/checks.toml` enforces a check at BOTH the in-loop dev gate
(Layer 2) and the generated CI workflow (Layer 4).** There is no "wire it twice" step.

### Why a single source of truth matters

Without a shared definition, Layer 2 and Layer 4 can drift: a custom linter you add to CI never
reaches the dev loop, so the agent gets no early feedback and can produce code that passes locally
but fails in CI. The manifest eliminates this structurally — both consumers read the same file.

### The manifest schema

```toml
# .camerata/checks.toml
[[check]]
id       = "ARCH-API-LAYERING-1"         # stable rule id; used as the violation id on nonzero exit
name     = "API layering"                 # short human label shown in bounce-back messages
command  = "scripts/check_layering.sh"   # shell command, runs from the repo root (sh -c)
severity = "high"                         # "high" | "medium" | "low" — informational; all severities block
in_loop  = true                           # true = run at Layer 2 AND Layer 4; false = CI-only
```

All five fields are required. A missing field is a parse error, not a silent misconfiguration. A
missing manifest is never fatal — the built-in language runners (cargo/clippy/eslint/ruff/etc.)
are always unaffected.

**`in_loop` decides when the check runs:**

| `in_loop` | Runs at | Use when |
|---|---|---|
| `true` | Layer 2 (dev loop) AND Layer 4 (CI) | Check is fast (under 30s), needs no external secrets or services. The agent gets early feedback. |
| `false` | Layer 4 (CI) only | Check needs secrets, an external service, or takes too long to run mid-loop. CI is always the authoritative backstop. |

**The manifest is agent-protected.** The gate rule `SEC-NO-CAMERATA-CONFIG-1` blocks any agent write
to the `.camerata/` directory. Editing the manifest is always a human/operator commit. This prevents
the canonical gate-weakening attack: an agent cannot disable the rules that govern it.

### Mechanical vs. architectural: two different paths

The onboarding flow files **two separate CI stories** — one per enforcement tier — because the
implementation work is fundamentally different:

**Mechanical CI story** ("Wire mechanical rules"): each selected mechanical rule maps to an
off-the-shelf linter. The story shows a per-rule manifest entry template. The implementation is:
fill in the correct command + pinned version, commit the manifest, done. No custom checker design
needed.

**Architectural CI story** ("Wire architectural rules"): there is NO off-the-shelf linter for
architectural rules (e.g. "service layer never touches the DB directly"). The story walks through
a four-step process:
1. Design a deterministic checker (options: a shell script, a custom Semgrep rule, an AST pass,
   a dependency-cruiser config — anything that exits 0 for clean and nonzero for a violation).
2. Add the manifest entry with pinned tool + version + install command.
3. Regenerate the CI workflow.
4. Verify the check at both Layer 2 and Layer 4.

The story includes a worked example for API-layering enforcement via `dependency-cruiser`, as this
is the most common architectural check pattern.

Scope each architectural rule as its own sub-task; do not block the mechanical story on this design
phase.

### What apply writes — the closed loop

When you click **Add rules to repo(s)** (§3 step 5), the apply step is now the upstream source for
everything Layer 2 and Layer 4 consume. The loop that was previously open is now closed:

```
Apply (Camerata UI)
  └─ writes  .camerata/checks.toml   (the SSOT manifest)
                 |
       +---------+---------+
       |                   |
  Layer 2               Layer 4
  (dev loop)             (CI)
  ManifestCheckRunner    camerata-gates.yml
  reads checks.toml      generated from checks.toml
```

Before this was wired, the apply step wrote a `.camerata/ci-checks.json` file and a placeholder
`camerata-governance.yml` that nothing in the runtime consumed. Those are gone. Now apply emits the
real `.camerata/checks.toml` (the exact format `load_manifest` parses) and the real
`.github/workflows/camerata-gates.yml` (identical to what the Regenerate CI workflow button
produces). There is no "wire it twice" step and no divergence between what the apply step wrote and
what the runtime executes.

**`.camerata/checks.toml` is the single file you edit to manage custom gate checks.** One change
there is automatically reflected in both Layer 2 (next dev run) and Layer 4 (next CI run after
commit). The `camerata-gates.yml` workflow is regenerated from it on demand — it is never the
authoritative source, only the derived output.

### Regenerating the CI workflow

After editing `.camerata/checks.toml`, regenerate the CI workflow by clicking the **Regenerate CI
workflow** button in the Rules view (or calling `POST /api/projects/active/generate-ci-workflow`).
The workflow is derived entirely from the manifest, so regenerating it is always safe to repeat.

---

## 15. Tool-version pinning and the drift error

Even with a single manifest definition, a check can still disagree between Layer 2 and Layer 4 if
the two environments run **different versions of the same tool**. For example, a `dependency-cruiser`
rule that was valid under version 5.x may emit different findings under 6.x. The manifest solves
this with three optional pinning fields:

```toml
[[check]]
id       = "DEP-CRUISER-LAYERING-1"
name     = "dependency-cruiser layering"
tool     = "dependency-cruiser"                        # the binary name
version  = "6.3.0"                                     # EXACT version — no ranges or carets
install  = "npm install -g dependency-cruiser@6.3.0"   # the exact install command
command  = "depcruise --config .dependency-cruiser.cjs src"
severity = "high"
in_loop  = true
```

**`version` must be an exact version string, not a range (`^6.3.0` or `>=6`) .** Ranges allow the
two environments to land on different patch releases and disagree. Pinning an exact version is what
makes the SSOT property hold end-to-end.

### What happens at Layer 4 (CI)

The generated CI workflow emits a **dedicated install step immediately before the check step**:

```yaml
- name: "install dependency-cruiser (6.3.0)"
  run: npm install -g dependency-cruiser@6.3.0

- name: "dependency-cruiser layering (DEP-CRUISER-LAYERING-1)"
  run: depcruise --config .dependency-cruiser.cjs src
```

CI always installs the pinned version, so the check runs against the exact tool version you declared.

### What happens at Layer 2 (your machine)

Layer 2 does NOT install tools — installing in the dev loop is too heavy. Instead, before running a
pinned check, Camerata verifies that your local tool version matches the pinned version. If it does,
the check runs. If it doesn't, you get a clear error like:

```
local dependency-cruiser is 5.1.0 but manifest pins 6.3.0 —
install the pinned version: npm install -g dependency-cruiser@6.3.0
```

The check is **skipped** (not silently run on the wrong version) until you reconcile. This is a
violation, not a warning — a warning would still let the loop complete "green" on the wrong tool,
which defeats the whole point.

**To fix a drift error:** run the `install` command shown in the error message (it's always the exact
command from your manifest's `install` field), then re-run your dev loop. This protects you from the
"passes locally, fails in CI" class of surprises.

**Checks without pinning fields** run normally at both layers with no version check — you get the
system-installed version at each environment. Add pinning when your check's output is sensitive to
the exact tool version.

---

## 16. Token usage meter

A compact, persistent **usage meter** is pinned to the right of the cockpit nav row. It shows your
cumulative token and dollar spend for the current session: `<tokens> tok · $<cost> · <calls> calls`.
Click it to expand a by-model breakdown table.

When the provider is rate-limiting requests, the meter swaps to an amber pulsing **"Rate-limited —
retrying"** badge instead of the normal readout. This clears automatically when the next request
succeeds.

The meter accumulates spend across ALL model calls — the audit, calibration, research chat, story
authoring, decomposition, clarification, and fleet runs — not just the last audit. It is purely
observational: it does not change model selection, retry behavior, or the gate.

---

## 17. Settings

Settings is a **single consolidated nav item** inside a project. It is split into two clearly-labeled
scopes.

### Cross-project settings (apply to all projects)

- **OpenRouter API key**: for API-path model access. Stored in the system keychain; shown masked.
- **GitHub token**: for push, PR, and Issues operations. Stored in the system keychain; shown masked.
- **Bombe animation**: a global ON/OFF toggle and a Play/Pause preview control. ON by default;
  turning it off stops the background animation entirely. The preview lets you see the Bombe in motion
  (and pause it) without waiting for a real run to start.

### Per-project model configuration

Model configuration is the other half of the Settings page, scoped to the open project.

#### Suggested profiles

A **suggested model level** (a profile) cascades an opinionated set of model choices across every
fleet band and helper step in one click. Four profiles are available:

| Profile | Intent |
|---|---|
| **Balanced** | A sensible default: mid-tier models everywhere, reasonable cost. |
| **Max Efficiency** | Faster and cheaper models throughout; best for high-volume or low-stakes work. |
| **Max Quality** | Strongest models everywhere; best for complex or high-stakes stories. |
| **Custom** | You have manually overridden one or more entries; the profile stays at Custom until you re-apply a preset. |

Applying a profile sets all entries at once. Any single entry remains overridable afterward.

#### Fleet model bands

The governed development fleet uses a three-band logic ladder:

| Band | Role | Who runs here |
|---|---|---|
| **Strongest** | Lead / orchestrator | Drives the whole run; handles complex, one-way-door decisions; re-handles escalations. |
| **Balanced** | Mid engineer | Subtasks delegated by the orchestrator (`delegate` tool). |
| **Fast** | Quick engineer | Well-scoped, low-risk subtasks. |

Each band has a **primary** model and an optional **fallback chain** (tried in order when the primary
is unavailable or rate-limited).

**Escalation routing:** when a child returns `INCOMPLETE:`, the orchestrator re-handles the work
itself (Strongest tier). There is no separate escalation tier.

#### Designer (vision) band

An optional, project-wide **Designer** band sits orthogonal to the logic ladder. It is a real fleet
agent the lead/orchestrator can hand visual / UI work to (reachable internally as a gated `"vision"`
delegate tier), and it intercepts visual work before the ladder applies:

- Only **vision-capable (multimodal)** models are listed in the Designer selector.
- The designer agent receives the existing in-code layout and styling as context, then produces an
  **HTML/Tailwind mockup** as an intermediate representation (IR).
- A logic-tier agent then translates that IR into Dioxus `rsx!` markup. The vision model never
  writes Rust directly.
- Routing is domain-first: any task classified as visual work goes to the Designer; the Strongest /
  Balanced / Fast ladder handles everything else.

**When the Designer is reachable (the gate).** The band is available to a run ONLY when both hold:
the project's **Designer toggle is ON** in Settings **and** a **vision-capable model is configured**
for it. The toggle controls availability, not just configuration: with the toggle off (or no vision
model set), the orchestrator simply cannot route to the Designer, and any attempt to hand off visual
work is declined the same way an unknown tier would be (no error, no half-configured run). When the
band does run, the Designer agent is governed exactly like every other agent in the fleet: its only
write path is the gate, it is jailed to the single shared worktree, and it cannot delegate further.

> The **in-hierarchy designer agent** (above) is distinct from a planned, separate **designer
> module** (an interactive mockup tool for end users). They do not overlap.

#### Helper-agent models

Each one-shot helper step has its own model selector with an info icon explaining what the step
does and where it runs:

| Step | When it runs |
|---|---|
| Audit | The onboarding code audit scan |
| Calibration | The severity-calibration pass during the audit |
| Research chat | The in-app assistant |
| Story authoring | Drafting an issue from a blank UoW |
| Decomposition | Breaking a story into sub-tasks |
| Escalation | Translating escalation decisions into prose |
| Clarification | Generating structured clarification questions |

All steps default to `claude-sonnet-4-6` when a project is created. A change to one project never
touches another project's step models.

For steps where you also pick a model per run (audit, calibration, research chat), your per-run
choice wins over the project default for that run only.

#### L3 AI code review

An opt-in, per-project agentic reviewer that runs in parallel with Layer-2 after each dev-run
iteration. Toggle it on here; select the model (defaults to the Balanced tier when left blank).
See §6a for the full description of what it does and when to use it.

### Soft context: product brief, operating principles, project memory

Below model configuration, the **Soft context** group holds the per-project context that helps the
agents exercise good judgment *inside* the rules. The rules are the hard constraints; this is the
softer "why / how / what we have learned" a well-briefed engineer carries. All three are woven into
every agent's grounding and **travel with the project export**, so they transfer to another user on
import.

- **Product brief** — free text describing what the product is, who it is for, the quality bar, and
  the non-negotiables. The agents read it *above* the rules, so they grasp the why before the what. A
  scaffold prompts the sections to fill in.
- **Operating principles** — how a good engineer works on this project (conduct, not the code): prefer
  explicit over clever, confirm irreversible changes, report honestly, escalate when blocked, and so
  on. New projects are seeded with a default set you can toggle off or extend with your own; only the
  *enabled* ones reach the agents.
- **Project memory** — the accumulating, **human-curated** learnings (decisions, patterns, gotchas,
  constraints) that carry across runs, so the next agent does not rediscover what the last one learned.
  **Agents propose, you curate:** after a run, the agent's proposed learnings appear here as
  **Proposed** (with a "N to review" badge); you Approve, Archive, or Delete them, or add your own.
  Only **Approved** entries (the most recent, capped) feed the agents' grounding.

See `docs/PROJECT_CONTEXT_LAYERS.md` for the design.

### Work hierarchy (define your work-item types)

Also in the Soft-context group is the **Work hierarchy** builder: define, per project, the work-item
types your team uses and which types may nest under which. It is Camerata's own, portable, per-project
answer to a fixed type system: freetext types (so custom ones slot in), and it encodes the parent-child
RELATIONSHIP rules that a flat label cannot.

- A **palette** of built-in types (Initiative, Epic, Feature, Story, Defect, Task, Bug), each with a
  hover explaining the term, plus an input to add your own **custom** types.
- **Drag a type onto another type's "children" zone** to allow that nesting. A parent may allow several
  child types (a Feature can parent a Story and a Defect), and a child type may sit under several
  parents. Cycles are prevented, so the graph stays a sane tree.
- Mark which types may be a **design root** (the top of a work tree), then **Save** to persist the whole
  graph. It travels with the project export.

New projects start seeded with the common ladder (Initiative, then Epic, then Feature, then Story or
Defect; Story then Task or Bug); edit it freely. This is the first shipped piece of a larger Design
page: the schema itself is live today; the AI-assisted decomposition of an epic into drafted child
stories, and publishing that tree to your tracker, are planned (see
`docs/plans/2026-06-30_epic-design-page.md`).

---

## 18. Routines (scheduled governed runs)

A **routine** is a governed run on a schedule: give it a name, a schedule, an operational prompt, and a
permission scope, and Camerata fires it for you. Routines live under the **Routines** nav item. The
scheduler runs *inside the app*, so routines fire while Camerata is open (not as a background OS
service).

### Creating a routine

Open **Routines** and either start from a **template** or fill the form fresh.

- **Templates** — two presets to start from: **Bug Triage Dashboard** (daily 09:00, read-only) and
  **Security Scan and Patch** (daily 04:00, write-gated). "Use this template" prefills the form; you
  still write the intent and press Save.
- **Name** — what you will recognize it by.
- **Permissions (scope)** — the cap on what an unattended run may do:
  - **Read-only** — analyze and report; write nothing.
  - **Write (gated)** — edit on a working branch, every write through the governance gate, no push.
  - **Write and open PR** — the above, plus push and open a PR. Nothing auto-merges.
- **Project** — assign it to a project (for grouping, and so it travels with that project's export) or
  leave it **Global**. Either way the scheduler fires it; the assignment is about organization, not
  execution.
- **Model** — the model the routine uses, from your model catalog.
- **Schedule** — a structured picker: **One-off** (a single date and time), **Daily**, **Weekly** (pick
  the weekdays), or **Monthly** (pick the day of month), each with a time. A live preview shows the
  resulting schedule. Anything left unscheduled is **manual**: it never auto-fires, and you run it by
  hand.
- **Intent** — describe, in plain words, WHAT you want the routine to do. This is never run verbatim.
- **Draft operational prompt** — press it and the lead-engineer AI authors a concrete **operational
  prompt** from your intent plus scope (falling back to a deterministic scaffold if the model is
  unavailable). The prompt is editable; it is what a run would actually execute.

Press **Add routine** to save. A locally-created routine is **provisioned** and ready.

### The dashboard

The Routines view shows a status strip you can click to filter: **Total, Enabled, Running, Blocked,
Due (under 24h)**. Rows are grouped by project (Global last), each showing its schedule, next fire, and
last run. Per routine:

- **Start / Stop** — arm or disarm auto-firing (the `enabled` state). Provisioning never auto-starts a
  routine; you press Start.
- **Run now** — fire it immediately (a manual run).
- **Set up** — appears on an **imported** routine, which arrives un-provisioned (so a Start cannot
  silently do nothing on a machine where the routine does not really exist). Press it to provision,
  then Start.
- **Run history** — the recent runs (capped at 20), each with its gate summary.

### Blocked runs and review

When a run's governance gate denies something, the routine lands in **Blocked (needs review)** and
raises a human-review escalation (at most one open per routine). Its row expands into a review panel
where you can **chat with the lead engineer** about what was denied (chatting never unblocks) and then
**Authorize and unblock**. This is the same review flow as the interactive escalations in section 6.

### What a fire does today (current behavior)

Be aware of the current execution model so nothing surprises you: a scheduled or manual fire exercises
the **governance gate against a fixed set of representative calls** to demonstrate deny and allow
enforcement, rather than running your operational prompt as a live multi-agent build. The gate verdicts
are real and the blocked-run review flow is real, but the routine's authored prompt, scope, and model
are not yet driving a live build at fire time (so a fire currently returns the same representative
summary). Live prompt execution is planned; the scheduling, prompt authoring, dashboard, run history,
and review surfaces are what is shipped today.

### Portability

Routines assigned to a project travel with that project's **export**. On import they arrive
**un-provisioned and stopped**, so nothing fires on the new machine until you press **Set up** and then
**Start**.

---

## The whole loop, in one line

**Create/open a project → configure credentials in Settings (OpenRouter key + GitHub token,
keychain-backed) + set model configuration (profile, fleet bands, Designer band, helper steps, L3
review) → onboard each repo (browse to its local folder → scan the local code, choosing AI review
and/or deterministic scans → pick per-repo rules (opt-in rules are never pre-checked) → Add rules
to repo(s): local branch+push → optionally audit + triage (check Test badge + Scan coverage
section) + wire CI via two separate stories (mechanical and architectural)) → manage the ruleset
in the Rules view (rules-only; Re-emit rules to bring local clones up to date; rules grouped by
domain, collapsed by default, click to expand) → in Governed Development, pull work items (or
author a new story from a blank UoW with AI), create a Unit of Work from one → 3-phase cockpit
(Intake: inline story view, set repos-in-scope at top, add/edit/delete context for the
investigation agent → Investigation: run investigation agent, approve decisions, settle prose
contract for cross-boundary work → Development: run three-tier orchestrator-led build with optional
Designer band for visual work, use per-repo Ship panel, mark Done/archive) → use PR lifecycle
buttons to push, open PR, pull CI status, and resolve feedback with a gated agent → sign off.**

Onboarding is local-first (no GitHub needed); connect GitHub and a model provider for the push/PR
and the AI audit + governed dev. Export/import a project (config only; UoWs stay local) to move it
between machines; resolve local repo paths on the receiving side. Multiple UoWs can run
concurrently, each in its own isolated git worktree. Wire custom checks via `.camerata/checks.toml`
(the SSOT for both the dev loop and CI); pin exact tool versions to prevent drift. Watch the token
usage meter in the nav for cumulative spend. Use the chat bubble to ask data-driven questions about
your active project; it retains context across messages.
