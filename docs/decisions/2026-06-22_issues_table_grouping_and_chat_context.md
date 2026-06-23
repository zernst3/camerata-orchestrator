# 2026-06-22 -- Issues Table Grouping and Chat Context

## What was built

### Feature 1: GitHub Issues table grouped by Epic -> child

**Backend (`crates/server/src/github_issues.rs`, `crates/server/src/workitems.rs`, `crates/server/src/lib.rs`)**

`IssueSummary` gained a `parent_number: Option<u64>` field (`#[serde(default)]` for back-compat). `RawIssue` gained a corresponding `parent: Option<RawIssueParent>` deserialization field. `parse_open_issues` maps `i.parent.map(|p| p.number)` onto `IssueSummary::parent_number`. The `WorkItem` DTO gained the same `parent_number` field, and `workitems_pull` carries it through from `IssueSummary`.

**GitHub API approach**: Reading the `parent` field directly off the issue object in the list response. GitHub's sub-issues feature (released 2024) adds a `parent` member to each sub-issue's REST representation: `{ "parent": { "number": N, "title": "...", ... } }`. Reading this field is zero additional API calls -- the existing `GET /repos/{owner}/{repo}/issues?state=open&per_page=100` call returns all the data needed. The `RawIssueParent` struct reads only `number` (the only field needed for grouping); extra fields in the GitHub response are ignored by serde.

**Why not the sub_issues endpoint**: `GET /repos/{owner}/{repo}/issues/{number}/sub_issues` would require N extra calls (one per potential Epic), creating an N+1 pattern banned by DB-NPLUSONE-1's spirit and costly against GitHub's rate limit. The `parent` field on the issue itself is strictly cheaper: zero extra calls.

**Frontend (`crates/ui/src/cockpit.rs`)**

`WorkItemRow` wraps `WorkItem` and adds a computed `parent_label: String`. `build_work_item_rows()` computes labels by indexing the item list: Epics get `"#N: <title>"`, children get `"#N: <parent title>"`, standalones get `"Standalone"`. `work_item_columns()` was updated to operate over `WorkItemRow`, adding a `parent` column as the grouping key. `WorkItemTable` calls `handle.set_grouping(vec![ColumnId("parent")])` via `use_hook` -- the same Chorale idiom the custom-rules table uses for domain grouping.

### Feature 2: Pulled issues in chatbot context

**Approach**: `ChatBubble` (in `crates/ui/src/chat.rs`) received a new `pulled_issues_section: Option<String>` prop. The caller in `main.rs` invokes `cockpit::pulled_issues_chat_section()`, which reads the app-lifetime `PULLED_WORK_ITEMS` GlobalSignal and calls `render_pulled_issues_for_chat()` to produce the text. `unified_system_prompt` gained a `pulled_issues_section: Option<&str>` parameter and injects it as "=== LAYER 3b ===" between the UoW snapshot and the optional focused finding.

**Why Layer 3b, not a new server endpoint**: Pulled issues exist only in the UI's `PULLED_WORK_ITEMS` GlobalSignal -- they are not persisted server-side (the pull is manual, stateless). Adding a new server endpoint would require a server-side cache or a per-request refetch (an extra GitHub API call on every chat turn). The prop-passing approach reads from already-held memory at zero cost and keeps the server context endpoint clean.

**Which endpoint**: No new server endpoint. The existing `GET /api/development/context` is unchanged. Issue context is passed through the UI layer directly.

**Circular-dependency avoidance**: `chat.rs` imports from `cockpit.rs` (ChatBubble, FindingContext). Adding a cockpit import to chat.rs would create a cycle. The `pulled_issues_section` prop keeps `chat.rs` free of WorkItem knowledge -- the caller (main.rs, which imports both modules) bridges them.

## Decisions

- `parent` field on REST issue payload preferred over `/sub_issues` endpoint: zero N+1, zero extra rate-limit cost, same data.
- `#[serde(default)]` on `parent_number` throughout ensures all serialized states (existing stored `WorkItem` JSON) round-trip without error.
- `WorkItemRow` wrapper (not a field on `WorkItem`) keeps the grouping label a VIEW concern, not a stored/serialized data concern.
- `pulled_issues_section` as a prop (not a new server context endpoint) keeps the server stateless and the chat module free of WorkItem dependencies.
- `pulled_issues_chat_section()` is `pub(crate)` on cockpit.rs so main.rs can bridge the two modules without exposing `PULLED_WORK_ITEMS` itself.
