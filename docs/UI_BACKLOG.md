# UI backlog (forward-looking)

Current, post-clean-slate UI follow-ups. Newest intent at the top.

- **chorale integration — DONE 2026-06-15 (where it earns its keep).** `chorale-core`
  + `chorale-dioxus` `0.2.2` from crates.io now back the brownfield **audit-findings**
  table (sort by severity/type, filter by type/location, resize, virtualized) and the
  **proposed-rules** table (chorale selection checkboxes = accept/reject into the
  starter set). Built with the explicit `ColumnDef` API (not `#[derive(TableRow)]`,
  which doesn't support the severity/kind **badges** these tables rely on). chorale
  injects its own stylesheet (and has a `Theme` prop — relevant to the dark-mode item).
  Remaining: route the routines table onto the same primitive for consistency, and wire
  rule-selection → "arm" (governance PR).

- **Where chorale (the headless table) earns its keep — decided 2026-06-15.** Do NOT
  justify chorale on the routines table (3 rows; a virtualized grid behind it reads as
  hammer-thumbtack and undercuts the right-sizing judgment). Wire it into the cockpit
  surfaces where data genuinely scales:
  - **Brownfield audit findings** — a real audit can surface hundreds/thousands of
    findings. This is the prime home: group + sort by finding TYPE and severity,
    group + sort proposed RULES with **selection** (checkboxes to accept/reject into the
    approved set), filtering. Virtualization is real value here, not decoration.
  - **Gate activity log** — the run/verdict stream grows; same primitive.
  Then the routines table (and other small fixed lists) ride the SAME primitive for
  consistency (one table abstraction across the cockpit), explicitly NOT because they
  "need virtualization." chorale is headless, so small-N surfaces use the table state
  (columns/sort/selection) without lighting up the virtualization path. The defensible
  line to keep: "chorale where data can scale (audit findings, activity logs); the same
  primitive for the small lists for consistency; a plain table for a true one-off."
  Build order: the audit-findings table is the design to nail (grouping/sorting/selection
  over a large finding set) and pairs with the brownfield scan engine.

- **Dark mode** (requested 2026-06-15; NOT a priority). The whole look is driven by
  CSS custom properties in `crates/ui/src/style.rs` (`--ink`, `--ink-soft`,
  `--ink-faint`, `--surface`, `--line`, `--accent`, plus the warm onboarding-gate
  colors). A dark theme is mostly a second `:root`/`[data-theme="dark"]` variable
  block plus a toggle in the edition switcher — no per-component rewrites, since
  components already reference the variables. The few hardcoded colors (the
  onboarding gate `#fff7ed`/`#f0c89a`/`#8a4f1d`, the conn-ok/conn-warn greens/oranges)
  need theme-aware values.

- **Projects v2 board as a cockpit view.** The engine + CLI (`projects-live`) are
  built and proven; surface the board listing as a view (likely under "Onboard a
  repo" or a new "Boards" tab) so cross-repo stories adopt from the UI, not just the
  CLI.

- **Brownfield scan/audit/arm engine.** The Onboard view + flow exist and are
  connection-gated; the repo-scanning engine (scan → propose ruleset → audit existing
  code → generate the governance PR) is the backend build behind the "Scan repo"
  button. See `decisions/2026-06-15_brownfield_onboarding_flow.md`.

- **Wire the VCS-action gate into the live commit/PR path.** The deterministic core
  (`camerata-checks::vcs_action`, `PROCESS-*`) is built and tested; it plugs into
  Camerata's commit/PR step when the live-build path is hardened.

- **Real-time / notifications (POLLING, tiered cadence — webhooks NOT a priority).**
  Decision (Zach, 2026-06-15): interval polling is the mechanism; websockets/webhooks
  are an opt-in upgrade we are not prioritizing — the use cases are comment back-and-forth
  and watching deployments, which polling handles fine. The provider `poll()` capability
  already exists (GitHub adapter implements inbound reconciliation); what's NOT built is
  an always-on server-side poller that ingests tracker events and emits notifications.
  Build it with **tiered cadences**:
  - **Comments / clarifications (PO answers):** slow poll (~30–60s) is plenty.
  - **Deployment watching:** as near-real-time as polling allows — a fast cadence
    (single-digit seconds) while a deploy is in flight, backing off when idle.
  Shape: a background tokio task in `serve()` calls `provider.poll(cursor)` (and a
  deploy-status poll) on its cadence, applies inbound events to the store, and exposes a
  notifications feed the UI drains into toasts (the toast host + `push_toast` are already
  in place; `ConnectionWatcher` is the same pattern at 45s for connection health). The
  current `ConnectionWatcher` is the connection-health slice of this; the event-ingest
  slice is the build.
