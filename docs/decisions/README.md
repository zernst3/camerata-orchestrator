# Decision records (ADRs)

Each file here captures one design decision in prose: the context, the decision,
and the rationale. This index also maps every major feature to where its design is
written down, so the trail is navigable, not just buried in commit messages.

## Decision records (newest first)

- **2026-06-16_enforcement_tiers_gate_vs_ci.md** — two enforcement tiers, mapped by what
  the check needs: the write-time deny-before-execute GATE is the SECURITY tier (decidable
  from one write's path/content, near-zero false positives — secrets, path/secret-file
  guards, SQL-concat, secret-URLs), and CI/integration is the CONSISTENCY/ARCHITECTURE
  tier (mechanical corpus rules whose conformance needs build context — lint, query-plan,
  migration audit, AST). None of the 16 mechanical corpus rules fit the gate (their own
  conformance says "in CI"). New gate rule SEC-NO-SECRET-FILES-1; arm emits
  `.camerata/ci-checks.json` + a governance workflow for mechanical rules.
- **2026-06-16_rule_corpus_is_the_moat.md** — "detect frameworks → suggest rules" is only
  as good as the rule library; generic/wrong rules = a noisy audit = negative value, so
  the per-language/framework corpus is the moat and is eval'd as a SEPARATE axis from the
  scan/fix engine (precision/recall per rule + stack, proposal relevance, directive
  quality). Deterministic corpus optimizes precision; the AI audit de-risks coverage but
  needs the same precision scrutiny (adversarial-refute pass). Harness staged.
- **2026-06-16_fix_through_gate_loop.md** — fixing audited items is a governed dev task
  (not a special path): generate → gate (Layer 1 deny-before-execute) → checks (Layer 2
  fmt/clippy/test, bounce-on-fail) → reviewable diff/PR → verify. `fix.rs::verify` compares
  before/after findings by violation identity and returns resolved/remaining/introduced;
  `clean()` (introduced nothing) is non-negotiable, `complete()` also resolved the target.
  Pipeline wired via start_governed_run; live exec opt-in; auto-verify-at-completion next.
- **2026-06-16_baseline_ratchet_and_suppressions.md** — the make-or-break brownfield
  decision: REPORT every existing violation but ENFORCE on the delta (new/changed code),
  like eslint/ruff/sonar baselines — otherwise onboarding freezes a legacy team and
  nobody adopts. Two homes: inline `// camerata:allow RULE -- reason [, TICKET]` for
  surgical per-line waivers (shows in the PR diff, git-blame who/when, travels through
  refactors) and a central `.camerata/baseline.json` for bulk/legacy/policy. Three
  invariants: reason required + gated (reason-less waiver is itself a violation), indexed
  centrally (one queryable registry), stale ones surfaced (dead directives flagged).
  Content-fingerprinted so touching debt un-baselines it (the ratchet). Waivers carry the
  debt ticket (ignore + create-story = one act). `suppression.rs` built + fully tested;
  wired into the scan; arm-writes-baseline + registry UI next.
- **2026-06-16_ai_native_and_agent_agnostic.md** — Camerata is AI-native: the audit,
  story investigation/decomposition, clarifications, and code-gen are all model work;
  the ONLY deterministic thing is the deny-before-execute enforcement gate (the backstop
  a hallucinating AI can't talk past). Brownfield is two tiers — deterministic mechanical
  (secrets/SQL/path-escape) + an AI architectural audit for the genuine, non-lint
  violations (missing auth, layering breaches, N+1, ...). One vendor-agnostic provider
  seam (`llm.rs`): vendor axis (`CAMERATA_LLM_VENDOR`, anthropic wired; openai/google
  reserved) × transport axis (`CAMERATA_LLM_BACKEND`, cli for local human / api for
  production). Adding a vendor is a new match arm + MODELS entries, not a rewrite. Model
  selectable per call. BUILT (Anthropic CLI + API; AI wired into scan, draft-prompt,
  decomposition, clarify-suggest, research chat); live code-gen default is next.
- **2026-06-16_local_checkout_subsystem.md** — repo CONTENTS live on disk in a local
  working copy (only project pointers persist server-side); the lifecycle is clone/pull
  → fleet edits on a working branch → developer runs/tests LOCALLY → explicit ship
  (push + open PR), nothing auto-merges. Checkouts live under a single VISIBLE workspace
  folder the architect picks (native `rfd` dialog), at `<root>/<owner>/<repo>`, so the
  dev can open and run them normally; the choice persists in `settings.json`. Git is
  shelled out (`tokio::process`), the token injected only into transient clone/fetch/push
  commands and scrubbed from the persisted `origin`. Governance metadata stays
  API-direct-to-PR; this path is exclusively for code work. Server `settings.rs` +
  `workspace.rs` + 5 endpoints; UI **Workspace** cockpit tab. BUILT (46 server tests incl.
  a local git round-trip).
- **2026-06-16_project_container_and_rules_management.md** — the PROJECT is the
  foundational data container (the Azure-resource-group of Camerata): repos + the
  full ruleset (per-repo base selections, cross-repo rules, process rules, custom
  rules) + settings, switchable. Answers "where do non-repo rules persist": the
  cross-repo (API contract) + process (commit-format) rules span repos / are
  account-level, so they CAN'T live in a repo `.camerata/` file — they live at the
  PROJECT level (the project store; the integration + VCS-action gates read them
  there), while repo-local rules are also emitted into each repo. The ruleset is one
  source of truth; an edit is an UPSERT that never clobbers custom rules and produces
  one emit upserting repo files + project config. Adopt camerata-ai's rule features
  (export/import JSON, custom rules, drift) in TWO surfaces: brownfield + a project
  Rules-management screen. Foundation built (Project container + store + ruleset
  export/import + project selector + Rules view); full editor/re-emit phased.
- **2026-06-15_routine_authoring_intent_not_prompt.md** — the routine form was
  treating the user's text as the literal agent prompt; backwards. The user writes
  INTENT (what they want); the lead-engineer AI authors the OPERATIONAL prompt
  (model tiering, directives, governance framing, scope) and the user reviews/edits
  before save. Same intake→clarify / propose→approve shape as the rest of the
  product. Routine gains `intent` + `prompt`; `POST /api/routines/draft-prompt`
  drafts it (deterministic scaffold now, `authored_by: scaffold|claude`; real AI
  authoring activates with Claude). The raw intent is never run as-is.
- **2026-06-15_credential_delegated_scope_and_build_targets.md** — Camerata never
  self-scopes: the connected token/account IS the scope (no `CAMERATA_GITHUB_REPO`
  process pin; mirror the GitHub-MCP model of repo-as-per-call, not repo-as-process).
  And a story has two INDEPENDENT axes: a SOURCE (where tracked — Issues / Projects v2 /
  ADO / Jira, which sit above the repo) and a set of BUILD TARGETS (the repos where code
  lands — zero/one/many). `PrLink` already encodes multi-repo at the output; the gap is
  one field, `CanonicalStory.targets`. Governance sits underneath both axes, unchanged.
  Phased: A repo-per-request (GitHub Issues, multi-repo) → B source/target split in the
  model → C GitHub Projects v2 source → D second board provider. Design only; Phase A
  changes the provider surface (ROUTE-1).
- **2026-06-15_process_rules_and_vcs_action_gate.md** — a FOURTH enforcement point.
  Layers 1/2 and the integration tier all enforce on CODE artifacts (content/diff/tree);
  none sees commit messages or PR titles. A real process rule (`AB#{ticketId}` required
  in the PR title + commit prefix, per Zach's ADO workflow) is metadata, not code, so no
  existing gate can enforce it. Add a `vcs-action` gate at Camerata's commit/PR
  chokepoint — complete by construction because the agent has no `git` (Bash denied), so
  Camerata is the sole committer. New `PROCESS-*` family; per-account custom; gated
  firmly (error, never warn). Formalizes the two-axis rule taxonomy: scope
  (corpus-global / repo-local / cross-repo / process) × enforcement-point (content /
  integration / vcs-action). Design only; not built.
- **2026-06-15_routine_dashboard.md** — a management surface (table) over scheduled,
  governed agent routines: name, schedule/fire time, prompt, permission scope,
  enabled, status + run history, with create/edit/enable/run-now. A routine is a
  scheduled governed run (same engine + gate + provenance), so the dashboard is a
  scheduler + a view, not a new orchestrator. Presupposes the run model (Phase 3).
  Design only; not built.
- **2026-06-15_story_decomposition_by_practice.md** — ingest a parent work item and
  propose the component child stories per the org's CONFIGURABLE practice (Feature ->
  UI/API stories, Story -> tasks, whatever the org runs), grounded in repo context +
  story templates, human-reviewed before it commits. Children link to the parent on
  the spine and each is independently governable; they sync back as child work items.
  An upstream enrichment step before execution; pairs with the clarify-bridge. Design
  only; needs a parent/child spine relation + a decomposition engine.
- **2026-06-15_cross_agent_integration_gate.md** — a THIRD enforcement tier. Layers 1
  and 2 are per-agent, so any invariant spanning agents can drift while each agent's
  gates pass green. The cross-agent integration gate runs once on the assembled tree,
  before the branch is pushed (a pre-PR gate). API contracts are just the obvious
  example; the category is any rule only checkable on >1 agent's output: contract
  conformance, wiring completeness (events/config/DI/migrations, no dangling ends),
  convention coherence (casing/naming/dates/money), and cross-cutting policy (e.g.
  every UI-gated action maps to a guarded endpoint, audit on every write path).
  Principle: prefer compiled contracts (a shared Rust type IS the gate; JS needs an
  explicit derived-contract check). Declared at handoff, enforced at integration. New
  `INTEGRATION-*` rule family. Design only; not built.
- **2026-06-15_brownfield_onboarding_flow.md** — onboarding an EXISTING repo, reframed
  as the instant-value weapon: an existing codebase is pre-loaded with the violations
  the gate catches, so the flow is scan -> propose a starter ruleset -> approve ->
  AUDIT (here are the 12 arch violations + 3 hardcoded secrets already in your code,
  value in 5 min) -> ARM (one governance PR: CONVENTIONS.md/AGENTS.md + an enforced CI
  workflow + the gate's rule-subset config). Audit first (the hook), arm second (the
  close). Never make the user hand-author rules before they see value. Same commitment
  as the greenfield genesis harness, pointed at brownfield. The secret/SQL audit is
  real-now; the architecture audit needs the AST rules. Design only; not built.
- **2026-06-15_cockpit_story_view_ux.md** — how the cockpit's story-view behaves over
  a real (messy) tracker: a governed working SET the Architect adopts into, never a
  mirror of the board (no whole-board polling); provider-neutral across ADO / Jira /
  GitHub on two axes (board + code host), GitHub-first PoC (it serves both axes);
  intake by explicit id then a scoped saved-query picker ("current sprint + assigned
  to me") then opt-in tag/column; spine grouped by Camerata `FeatureStatus` with the
  NEEDS YOU queue as the working surface; the clarify-bridge posts an agent question
  as a tracker comment (review-then-post). Key call: the **respondent is a
  Camerata-side per-story concept, seeded from tracker hints but not dependent on a
  "PO" field**, with a never-block fallback ladder, because real teams often have no
  PO and reassign constantly.
- **2026-06-14_worktracker_port_architecture.md** — the architect surface: one
  `WorkItemProvider` port (core imports no provider); our Story spine is canonical, the tracker is a
  mirror configurable per field (provenance/gate/PR/sign-off always ours); two
  independent loop-avoidance guards (per-field direction + echo suppression); map to
  stable status categories never user names; native-first build order then board /
  code-host axes. Native + Jira + ADO + GitHub adapters built; live execution pending.
- **2026-06-14_refinement_session_primitive.md** — the refinement session as ONE
  back-and-forth primitive reused across three contexts (pre-build, mid-build
  escalation, post-build bugs); user/bug stories as the source of truth; the
  lifecycle as refinement alternating with execution; the `RefinementReviewer` seam.
- **2026-06-14_design_corpus_vector_db.md** — the opt-in shared-design corpus carries
  bug-fix knowledge (not just shapes); a vector DB is the search backend behind the
  `DesignCorpus` seam; opt-out is real deletion keyed by a contribution id; the two
  complementary stores (versioned source of truth vs derived search index).
- **2026-06-14_maintenance_ops_agent_and_dependencies.md** — the lead engineer owns
  external-library choices (chorale default; JS allowed for target apps); a standing
  async maintenance/ops agent owns the whole post-publish ops function (upgrades,
  security patches, key rotation, certs, backups), changing a live app only through
  the governed loop with calm security-update recommendations.
- **2026-06-14_persistence_sqlite_event_sourced_versioning.md** — SQLite now,
  Postgres behind the trait seam later; an application-level event-sourced
  revision log (not DB-native temporal tables) gives persistence + real-time updates
  + full version history with actor/operation provenance.

## Where each feature's design is written down

| Feature | Where the rationale lives |
|---|---|
| Refinement session model + lifecycle | ADR `refinement_session_primitive`; flow in `CONSUMER_UX.md` |
| Open-ended intake / onboarding document | `CONSUMER_UX.md` (Intake section) |
| Style kit (palettes, button/font, image upload) | `CONSUMER_UX.md` (Intake "What should it look like?"); built in `crates/intake/src/appearance.rs` |
| Lead engineer behavior (checklist, confidence, suggestions, honesty) | `CONSUMER_UX.md` (lead engineer's behavior) |
| Versioned persistence | ADR `persistence_sqlite_event_sourced_versioning` |
| Shared design corpus + opt-in/opt-out + bug-fix sharing + vector DB | ADR `design_corpus_vector_db`; `CONSUMER_UX.md` (the shared-design opt-in) |
| Maintenance / ops agent + dependency ownership | ADR `maintenance_ops_agent_and_dependencies`; `CONSUMER_UX.md` (Maintenance section) |
| Why deterministic governance, and the design rationale | `RATIONALE.md` |
| Two interaction surfaces on one engine, BYO-infra | `VISION.md` |
| Cockpit story-view UX + tracker working set + respondent model | ADR `cockpit_story_view_ux`; `WORKTRACKER_INTEGRATION.md` |
| Brownfield onboarding (install governance into an existing repo) | ADR `brownfield_onboarding_flow`; `VISION.md` (onboarding axis) |
| The commanded-violation demo + intent-blind enforcement | `DEMO_COMMANDED_VIOLATION.md`; `RATIONALE.md` |
| The governance gate + enforcement | `RATIONALE.md`; `ENFORCEMENT.md`; `RUST_CORE_VERIFICATION.md` |
| Cross-agent / integration gate (third tier, contract enforcement) | ADR `cross_agent_integration_gate`; `ENFORCEMENT.md` |
| Story decomposition (parent -> component stories by practice) | ADR `story_decomposition_by_practice` |
| Routine dashboard (manage scheduled governed routines) | ADR `routine_dashboard` |
| Project container (repos + ruleset + settings, switchable; non-repo rule persistence) | ADR `project_container_and_rules_management` |
| Local checkout / run-before-push (workspace folder, clone, branch, ship) | ADR `local_checkout_subsystem` |
| Credential-delegated scope + multi-repo + story source/target split | ADR `credential_delegated_scope_and_build_targets`; `WORKTRACKER_INTEGRATION.md` |
| Process rules + the VCS-action gate (`AB#{id}`); the rule taxonomy | ADR `process_rules_and_vcs_action_gate`; `ENFORCEMENT.md` |
| The stack top-to-bottom | `ARCHITECTURE.md` |

## Convention

When a non-trivial design decision is made, write it down HERE as a dated record at
the moment it is made, with the context and rationale, not just in a commit message.
Update this index. Keep the user-facing flow in `CONSUMER_UX.md` and the design
rationale in `RATIONALE.md`; this folder is for the "why we chose X over Y" records.
