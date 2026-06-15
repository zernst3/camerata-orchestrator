# Decision records (ADRs)

Each file here captures one design decision in prose: the context, the decision,
and the rationale. This index also maps every major feature to where its design is
written down, so the trail is navigable, not just buried in commit messages.

## Decision records (newest first)

- **2026-06-15_cross_agent_integration_gate.md** — a THIRD enforcement tier. Layers 1
  and 2 are per-agent, so the seam BETWEEN agents (the API contract the UI agent
  assumes vs what the API agent built) can drift while each agent's gates pass green.
  The cross-agent integration gate runs once on the assembled tree, before the branch
  is pushed (a pre-PR gate), checking API-contract conformance, shared-schema/type
  consistency, interface conformance. Principle: prefer compiled contracts (a shared
  Rust type IS the gate; JS needs an explicit derived-contract check). Declared at
  handoff, enforced at integration. New `INTEGRATION-*` rule family. Design only; not
  built.
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
| The stack top-to-bottom | `ARCHITECTURE.md` |

## Convention

When a non-trivial design decision is made, write it down HERE as a dated record at
the moment it is made, with the context and rationale, not just in a commit message.
Update this index. Keep the user-facing flow in `CONSUMER_UX.md` and the design
rationale in `RATIONALE.md`; this folder is for the "why we chose X over Y" records.
