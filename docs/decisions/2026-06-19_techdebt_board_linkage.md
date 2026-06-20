# Tech-Debt Board Linkage (ROUTE-1)

**Status:** ROUTED — structural/cross-crate surface change. Do not implement without Zach sign-off.

**Issue:** #41 (part 3 of 3)

**Date:** 2026-06-19

---

## Problem

The current onboarding flow creates one GitHub issue per repo for accepted tech
debt. This is the pragmatic first delivery (Pillar 1). Some teams track work in
a board system instead of or in addition to GitHub Issues: GitHub Projects,
Jira, Azure DevOps (ADO), Linear, etc. "Save to a board" means creating a work
item in the team's preferred tracker, not a GitHub issue.

---

## Proposed Design

### 1. Integrations Panel (UI)

The onboarding cockpit gains an "Integrations" panel (a new section in the
triage/action step) where a project configures its board target:

- **Type:** dropdown — GitHub Projects / Jira / Azure DevOps / None (default)
- **Connection:** credentials or a pre-configured `ProjectIntegration` record
- **Project/Board reference:** the board URL or id to receive work items

The project's `Project` struct (in `crates/server/src/project.rs`) gains a
`board` field:

```rust
pub struct Project {
    // ... existing fields ...
    /// Optional board integration this project routes tech-debt work items to.
    #[serde(default)]
    pub board: Option<BoardIntegration>,
}

pub struct BoardIntegration {
    /// Provider discriminant: "github_projects" | "jira" | "ado" | "linear"
    pub provider: String,
    /// Board/project reference (URL, numeric id, or "<org>/<project>").
    pub board_ref: String,
    /// Opaque auth credential name (looked up from the credential store).
    pub credential: String,
}
```

This change touches the `Project` struct and its serde round-trip. It is
additive (existing `Project` records deserialize cleanly via `#[serde(default)]`),
but it changes a cross-crate type used by `camerata-server` and `camerata-ui`.

### 2. WorkItemProvider Trait (new cross-crate surface)

A new `WorkItemProvider` trait lives in `camerata-core` (or a new
`camerata-integrations` crate):

```rust
/// Create a work item in an external tracker. One implementation per
/// provider: GitHubProjectsProvider, JiraProvider, AdoProvider, etc.
#[async_trait]
pub trait WorkItemProvider: Send + Sync {
    /// Create a work item. Returns the URL of the created item.
    async fn create_work_item(&self, item: WorkItem) -> anyhow::Result<String>;
}

pub struct WorkItem {
    pub title: String,
    pub body: String,
    pub repo: String,
    /// Machine-readable CSV of the findings, for boards that support
    /// structured attachments (Jira, ADO). GitHub Projects receives it
    /// inline in the body (same as the GitHub Issues path today).
    pub csv: String,
}
```

Adding a trait to `camerata-core` changes the crate's public surface and
affects every downstream consumer. This is the primary reason this is ROUTE-1.

### 3. Provider Dispatch in the Ticket Handler

`onboard_ticket` in `crates/server/src/lib.rs` currently always calls
`create_tech_debt_ticket` (which creates a GitHub issue). After this change it
would:

1. Resolve the project's `board` field.
2. If `board` is `None` or `provider == "github_issues"`, use the existing
   `create_tech_debt_ticket` path.
3. Otherwise, instantiate the appropriate `WorkItemProvider` and call
   `create_work_item`.

This is a behavior change in the handler but not a new API surface on the HTTP
layer.

---

## Why This Is ROUTE-1

Per `CONVENTIONS.md` `ROUTE-1`:

> Structural or topology changes — new/moved crate or module boundaries, public
> trait/API surface — ROUTE to Zach; never auto-apply.

This design requires at least one of:
- A new `WorkItemProvider` trait in `camerata-core` (public trait = cross-crate
  surface).
- A new `camerata-integrations` crate (new crate boundary).
- A `board` field on the `Project` struct (cross-crate type change, even if
  serde-additive).

Any of those triggers ROUTE-1. The decision tree is clear: design, sketch, stop.

---

## What Was Implemented in This PR (Non-Structural)

Parts 1 and 2 of issue #41 are fully additive within `camerata-server`:

- `tech_debt_csv(findings: &[Finding]) -> String` — pure CSV generator with
  RFC 4180 escaping.
- `tech_debt_issue_body` updated to embed a per-repo fenced `csv` block.
- 11 new unit tests: regression lock on repo isolation + CSV escaping + body
  embedding.

These require no new crate, no new trait, and no cross-crate type change.

---

## Open Questions for Zach

**Z1. New crate or extend core?**
`WorkItemProvider` is conceptually separate from `camerata-core`'s scan/audit
primitives. A `camerata-integrations` crate keeps the boundary clean but adds
build graph complexity. Alternatively, `camerata-core` takes the trait (it
already has `Finding`, `ScanReport`, etc.).

**Z2. GitHub Projects specifics.**
GitHub Projects v2 has a GraphQL API, not the Issues REST API used today.
"Link a Project board" means creating a ProjectV2Item, which requires the
`project_id` and an existing issue or PR as the content node. Should Camerata
create a GitHub Issue AND link it to the Project (two-step), or create a
Project draft item (no issue)?

**Z3. Credential model.**
Jira and ADO require per-org credentials (API token + base URL for Jira; PAT
for ADO). How should Camerata store these: alongside `CAMERATA_GITHUB_TOKEN`
as env vars, or in the project's encrypted settings? The current credential
system is flat env vars; a per-provider store may be needed.

**Z4. Priority.**
The GitHub Issues path now embeds a per-repo CSV block (part 2, shipped). Is
board linkage needed before a broader user-facing release, or is it a post-v1
feature?
