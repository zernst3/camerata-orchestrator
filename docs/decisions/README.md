# Decision records (ADRs)

Each file here captures one design decision in prose: the context, the decision,
and the rationale. This index also maps every major feature to where its design is
written down, so the trail is navigable, not just buried in commit messages.

## Decision records (newest first)

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
  the governed loop with calm security-update recommendations. Also records two open
  strategic questions (per-user economics / tiered pricing; the automate-self
  reflection).
- **2026-06-14_persistence_sqlite_event_sourced_versioning.md** — SQLite now,
  Postgres behind the trait seam at the endgame; an application-level event-sourced
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
| Target audience (small-business middle) + "why not off-the-shelf" | `POSITIONING.md` |
| Two-tier product (enterprise tool / consumer PaaS), BYO-infra | `VISION.md` (sections 19-20) |
| The governance gate + enforcement (the moat) | `POSITIONING.md`; `ENFORCEMENT.md`; `RUST_CORE_VERIFICATION.md` |
| The stack top-to-bottom | `ARCHITECTURE.md` |

## Convention

When a non-trivial design decision is made (especially one Zach raises in
conversation), write it down HERE as a dated record at the moment it is made, with
the context and rationale, not just in a commit message. Update this index. Keep the
user-facing flow in `CONSUMER_UX.md` and the market reasoning in `POSITIONING.md`;
this folder is for the "why we chose X over Y" records.
