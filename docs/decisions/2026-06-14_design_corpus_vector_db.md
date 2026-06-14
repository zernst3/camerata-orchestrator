# Shared design corpus: bug-fix knowledge + a vector DB for search

Date: 2026-06-14
Status: Accepted (direction); search backend forward-looking
Deciders: Zach (PO), Claude (architect)

## Context

Zach extended the opt-in shared-design corpus: the shared documents should include
bug stories AND the fixes (so future similar bugs benefit), and asked whether all
these documents should have a version stored in a vector database for quick search
and use.

## Decisions

1. **Shared designs carry fix knowledge (built).** A `ResolvedBug { symptom, fix }`
   travels with a `DesignReference` (`resolved_bugs`). The project accumulates them
   via `Project::record_fix` as post-build bug sessions are resolved, and they are
   attached (abstracted, no private data) when the user contributes their design. So
   the corpus carries "what went wrong and what fixed it," not just app shapes; the
   next builder of a similar app inherits the remedy. See
   `crates/intake/src/sharing.rs`.

2. **Yes, a vector database is the right search backend (direction).** The
   `DesignCorpus` trait is already the seam for this: `similar(form)` is the
   semantic-retrieval call. The in-memory implementation uses naive token overlap to
   prove the seam; the production implementation embeds the abstracted documents
   (designs, stories, resolved bugs) and stores the vectors in a vector DB
   (pgvector on a shared Postgres, or a dedicated store like
   Qdrant / LanceDB), then retrieves by semantic similarity. Swapping it in is a new
   `impl DesignCorpus`, no caller changes.

## The two stores, and why both

These are complementary, not redundant:

- **Source of truth: the versioned SQLite/Postgres `ArtifactStore`** (the
  event-sourced revision log). It owns the authoritative, per-project, versioned
  documents. This is where "the truth lives, versioned" (the 2026-06-14 persistence
  decision).
- **Search index: the vector DB.** A DERIVED structure over the ABSTRACTED, consented,
  cross-project corpus. It exists for fast semantic retrieval ("find designs and bug
  fixes like this one"), not as a system of record. It is rebuildable from the
  consented abstractions at any time.

Keeping them separate matters: the source of truth must be exact, versioned, and
private-by-default; the search index is fuzzy, cross-tenant, and consent-gated. They
have different correctness, privacy, and durability requirements, so they are
different stores behind different seams (`ArtifactStore` vs `DesignCorpus`).

## Opt-out is deletion, keyed by id (right to be forgotten)

The user can opt out at ANY time, and opting out must DELETE their data from the
corpus and its vector index, not merely stop future shares. This requires an id
reference from the contribution to the index rows:

- Every `DesignReference` carries an `id` (the owning project's id), and that id is
  stamped on EVERY derived row in the search index (each design, story, and bug-fix
  vector). It is the foreign key from "a user's contribution" to "the rows that
  represent it."
- `DesignCorpus::withdraw(id)` is the opt-out path: it removes the design and, in the
  vector-DB implementation, every row `WHERE owner_id = id`. `contribute` upserts by
  the same id, so re-sharing replaces rather than duplicating, and there is never a
  stale copy a withdrawal could miss.
- `AppState::withdraw_from_corpus` calls it when the user toggles sharing off, so the
  UI opt-out actually deletes.

This is why the contribution id is load-bearing, not cosmetic: without it there is no
way to find and delete a specific user's vectors, and "opt out" would be a lie.

## Privacy (unchanged, reaffirmed)

Only abstracted shapes ever enter the corpus or its vector index: `abstract_design`
strips description, constraints, look-and-feel, field values, and story motivations.
Resolved bugs are shared as plain-language symptom/fix, abstracted the same way.
Nothing enters the cross-project index without the user's explicit opt-in, and the
user can withdraw it all at any time by id.
