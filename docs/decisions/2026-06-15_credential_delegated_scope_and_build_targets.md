# Credential-delegated scope + story-source / build-target separation

Date: 2026-06-15
Status: Accepted (design); NOT built. PoC target: GitHub (Issues + Projects v2).
Deciders: Zach (architect), Claude (architect)

Companion docs: [`WORKTRACKER_INTEGRATION.md`](../WORKTRACKER_INTEGRATION.md),
[`PROVIDER_NEUTRALITY.md`](../PROVIDER_NEUTRALITY.md),
[`brownfield_onboarding_flow`](2026-06-15_brownfield_onboarding_flow.md) ("multi-repo is
normal; a feature can span repos"),
[`cross_agent_integration_gate`](2026-06-15_cross_agent_integration_gate.md).

## Context: Camerata is currently scoping itself, which it has no business doing

Today the BFF reads a single `CAMERATA_GITHUB_REPO` env var
([`crates/server/src/provider.rs`](../../crates/server/src/provider.rs)), splits it into
one `owner/repo`, and constructs one `GithubProvider` bound to that repo for the life of
the process. Two consequences, both wrong for the real product:

1. **The app self-scopes below the credential.** A token that can reach 200 repos is
   pinned to one. Compare the GitHub MCP server every Claude agent on this machine
   already uses: it takes `owner/repo` as a *per-call parameter*, so one token + one
   session reaches everything the token allows. Camerata bakes the repo into the
   *process*. Same token scope, strictly worse selection model.
2. **It conflates two independent axes.** GitHub Issues happen to live inside a repo, so
   "the tracker" and "the repo" look like one thing. Projects v2, ADO Boards, and Jira
   break that immediately: those boards are org/account/site-scoped and a single story
   routinely targets several repos, or none yet.

## Decision 1: Camerata delegates scope to the credential; it never self-scopes

**The credential is the scope.** Camerata surfaces exactly what the connected
token/account can reach: no more (it does not re-implement GitHub/ADO/Jira's permission
model) and no less (it does not pin itself below what the credential grants). Access
control is the platform's job; Camerata's job is to *govern the build* on top of whatever
the credential exposes.

Concretely:

- A **Connection** is an authenticated link to a provider account (a GitHub token, later
  an ADO org PAT, a Jira site token). It carries credentials and a base URL, **not** a
  fixed repo or board. The user registers connections; Camerata enumerates from them.
- The per-process `CAMERATA_GITHUB_REPO` binding is retired as the selection mechanism.
  It may remain as an optional *default filter* for convenience, never as a hard ceiling.
- `WorkItemProvider`'s methods already take `&ExternalRef` per call rather than reading a
  constructor-bound repo, so the trait is mostly already shaped for this. The repo/board
  coordinate becomes a per-request dimension carried on the reference, and the GitHub
  provider holds the connection (token + base URL) without a fixed repo.

## Decision 2: a story has a SOURCE and a set of BUILD TARGETS, and they are independent

A story is **not** "an issue in a repo." It is a unit of work that:

- **lives in a story source** — where it is tracked: GitHub Issues, GitHub Projects v2,
  ADO Boards, Jira. (Projects/ADO/Jira are inherently *above* the repo.)
- **targets a set of build targets** — the repos where code will land. Zero (not scoped
  yet), one, or many.

These two facts are independent. A story on a GitHub Project board can target three repos;
an Issue-sourced story targets the one repo it was filed in but may grow to span more.

The canonical model already half-encodes this and just needs the missing half:

- `PrLink` already states "a multi-repo feature produces N `PrLink`s that all roll up onto
  the same tracker work item" — the build-target axis exists at the *output* (PR) level.
- `CanonicalStory` carries `external_ref` (the source) but has **no build-target field**.
  The gap is exactly one field: the set of repos a story targets, distinct from where it
  is tracked. Add it (`targets: Vec<RepoTarget>`), and the split is real in the type
  system, not just in prose.

Governance sits *underneath* both axes and is unchanged by this. The gate, the worktree
jail, and the CheckRunner govern whatever repo a story resolves to; they do not care how
many repos or trackers exist. Multi-repo is purely a selection/orchestration concern above
the security boundary — which is why this is safe to add without touching the cage.

## Provider priority

GitHub Projects v2 is the first board-spanning-repos source to build, because it is the
easiest to demo with a real account and GitHub is already the code-host PoC. ADO and Jira
follow the same `WorkItemProvider` seam (stubs already exist:
`crates/worktracker/src/{azure_devops,jira}.rs`); they are deferred until there is a real
account to test against. The `Provider::GitHub` enum variant is already documented as
"Issues or Projects v2," so Projects is an additional *source* behind the same provider
kind, not a new kind.

## Phased build (each phase ships green)

- **Phase A — repo as a per-request dimension (GitHub Issues, multi-repo).** Make
  `GithubProvider` hold the connection, not a fixed repo; carry the repo coordinate on the
  reference; retire the `CAMERATA_GITHUB_REPO` hard binding. After this, one launch + one
  token works across every repo the token can reach. *(Changes the public provider surface
  → ROUTE-1; ratify before building.)*
- **Phase B — story-source / build-target split in the model.** Add
  `RepoTarget` + `CanonicalStory.targets`; thread it through the native provider, the
  demos, and the cockpit. The story view shows source-vs-targets as distinct.
- **Phase C — GitHub Projects v2 as a story source.** A board-level source that lists
  stories spanning repos, each story carrying its own targets. This is the demoable
  headline.
- **Phase D — second board provider (ADO or Jira)** once an account exists, to prove the
  source abstraction holds beyond GitHub.

## The single-repo live call still comes first

Before any of the above, the one-repo `worktracker-live` round-trip is run with a real
token to prove the GitHub transport actually works end to end (real ingest, real comment).
It is throwaway-cheap and de-risks the plumbing the refactor depends on. See
[`GITHUB_SETUP.md`](../GITHUB_SETUP.md).

## Honest current state

Not built. Today: one provider, one repo, bound from env at process start. The trait is
already per-`ExternalRef` (the hard part is mostly done); the connection model, the
per-request repo coordinate, the `targets` field, and the Projects source are the build.

## Open questions

- Connection storage: where registered connections live (the SQLite event store already in
  the persistence crate?) and how tokens are kept at rest (OS keychain vs encrypted row).
- Default-filter ergonomics: a session that *can* see 200 repos still wants a sensible
  default working set in the cockpit without re-pinning the process. A saved per-user
  working set, not a hard scope.
- Projects v2 is a GraphQL API (Issues is REST). The GitHub provider grows a GraphQL path
  for the board source while keeping REST for issue/PR ops; how cleanly those cohabit
  behind one `Connection`.
