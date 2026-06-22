# Test-when-back checklist (scan modes + UI sweep)

Things built while you were away that need your eyes / a live run to verify. Grouped by
risk — the starred (★) ones change model behavior or have untested runtime paths.

## Scan execution modes

- [ ] **Parallel mode (default)** — scan budget-mini in `Parallel`. Expect multiple pass
      agents working concurrently in the Agent-activity drawer, and a much faster finish than
      the old single call (~155s → the slowest ~40s batch).
- [ ] **Sequential mode** — same scan in `Sequential`; one agent at a time, slower. Findings
      should match Parallel (same coverage, just serial).
- [ ] **★ Background job mode** — pick `Background job`. Expect: returns immediately, a progress
      bar climbs (`done/total passes`), findings count ticks up, then the full Findings table
      lands on completion.
- [ ] **★ Resume across navigation** — start a Background job, switch to another cockpit view
      (Stories / Rules / Workspace), then return to Onboard. The progress should re-attach and
      the report should still land. (This is the one with untested runtime wiring — `poll_job`
      + the app-scope `active_audit_job`/`onboard_scan` contexts.)
- [ ] **Auto-select** — after a scan, the mode picker should pre-pick by scale (multi-repo or
      >150 files → Background job; else Parallel) with a "✓ auto-selected for this scan's size"
      note. Overriding it should clear the note.

## ★ Cache-aware prompt order (watch for drift)

- [ ] The audit prompt now leads with the repo-map + code digest and trails with the rules
      (was rules-first). **Eyeball the findings vs a prior run** — same violations should still
      surface, ideally with equal-or-better rule-following. If quality drifts, that's the
      reorder; flag it. (Cost savings depend on the backend's prompt caching.)

## Severity + findings UX

- [ ] Deterministic security findings (hardcoded secret / SQL concat / secret-in-URL) show as
      **Critical**, sorted to the very top, above the architectural `high` findings.
- [ ] **Click a findings row** → modal shows the violated rule's full directive + the complete,
      untruncated explanation.
- [ ] **Model picker** + **Scan-mode picker** sit side by side, styled consistently.

## UI consistency sweep (cosmetic)

- [ ] Buttons share one corner radius (8px) everywhere; primary buttons (`Audit`, `Arm`,
      `Scan repos`) look identical; disabled buttons (while auditing/arming) look disabled.
- [ ] In the findings toolbar, the primary + secondary buttons are vertically aligned (the old
      14px margin misalignment is gone).
- [ ] Findings **Severity** badges (Critical/High/Medium/Low) and the **authority**
      (Rule·enforced green / AI·advisory yellow) + **scope** (Repo-local green / Cross-repo
      yellow / Process gray) badges render as distinct colors, not all gray.

## ★ CI security rules + scan-time deterministic preview (NEW — needs QA)

Built 2026-06-22 (Semgrep/CodeQL CI rules + the scan runs deterministic CI rules as a preview).
No GitHub issue filed for this yet — banked here as a QA item.

- [ ] **The two CI/CD rules exist + are opt-in only.** `CICD-SEMGREP-SECURITY-SCAN-1` and
      `CICD-CODEQL-SECURITY-SCAN-1` appear in the CI/CD domain but are **never auto-recommended**
      (never pre-checked at onboarding), even on a relevant stack.
- [ ] **Mandatory tier choice (no default).** Selecting either forces a tier pick (free vs paid) —
      amber "must choose" until you pick. The **CodeQL free option spells out its limitations**
      (public-repo only; private → GHAS paid; heavy → CI/layer-3 only).
- [ ] **★ Scan-time deterministic PREVIEW.** Select a deterministic CI rule (e.g. a clippy/ruff/
      eslint rule, or Semgrep CE) whose tool is NOT yet wired into the repo, run the onboarding
      scan, and confirm it **still produces findings** (Camerata runs the tool with a supplied
      config). Findings are labeled **"preview — not enforced until wired"**, distinct from the
      authoritative gate.
- [ ] **CodeQL does NOT run at scan or layer-2.** Selecting the CodeQL rule produces a CI story
      only — no scan preview, no in-loop bounce (too heavy / licensed).
- [ ] **Mechanical rules stay OUT of the AI review.** Confirm token usage doesn't rise from these
      rules — they run as deterministic tools, not via the LLM scan.
- [ ] **Preview ≠ gate.** A rule that previewed at scan is still NOT enforced at layer-2/3 until
      the CI story wires it into the repo (then the repo is the pinned source of truth).
- [ ] **Graceful when a tool is missing.** If the local tool can't run, the scan notes
      "couldn't preview X — enforces once wired" rather than a false clean.

- [ ] **★ Scan-type selector.** At audit-start, pick **AI review** and/or **Deterministic scans**.
      Deterministic-only should run fast with **no LLM/token usage** (floor + tool preview only);
      AI-only should skip the linters; both = today's behavior. (Great for QA'ing the tool pass.)

- [ ] **★ Deterministic-scan progress indicator.** During a scan, a "Deterministic scan" progress
      component sits **above** the AI agent-activity drawer, showing the floor + tool pass state
      (per-tool starting/running/done + findings count, overall done/total). In deterministic-only
      mode it's the primary progress view.

## ★ Author a story from a blank UoW with AI (NEW — needs QA)

- [ ] **Create a UoW with no story.** A "new / author a story" action makes a blank UoW (no work
      item linked yet).
- [ ] **Prompt requirements → AI drafts a story** (title + body), with a **back-and-forth
      clarification chat** that refines it.
- [ ] **Push to board.** Pick a project repo; the drafted story is created as a **GitHub Issue**.
- [ ] **Auto-link.** The UoW links to the new issue and behaves like a normal story-linked UoW
      (dev runs, etc.).
- [ ] Graceful without a GitHub token (can draft, but push needs the token).

## Known v1 limits (by design — not bugs)

- Jobs are in-memory (don't survive an app restart — correct, since the work can't either).
- Resume re-attaches within the app session; a server restart ends the job (poll gives up).
- Live job findings are a raw preview; the table switches to the authoritative report on done.
