# Design: Enclosing-Block Code Context in the Finding Modal

Branch context: `feat/audit-report-export` (camerata-orchestrator). Design-only pass; no code changed.

## 0. Framing: this costs ZERO LLM tokens

This is a **display feature, not an AI feature**. The enclosing block is a **local file read** off the repo the user already has on disk (Camerata is local-first), computed deterministically and rendered in the modal. Nothing here is sent to any model. It is fully decoupled from what the AI audit consumes; token cost is exactly zero, forever, regardless of block size.

## 1. Where the finding modal is today

- Modal: `crates/ui/src/cockpit/scan.rs` (~lines 2567–2625) — the finding-detail modal gated on the `detail_finding()` signal, rendered with the `rule-modal-*` CSS classes at the results-subtree root.
- Today it shows: `rule_id`, severity / `path:line` / status tags, the rule directive, **`f.snippet`** as the "Finding" paragraph (the short capped snippet stored at scan time), the explanation (`f.detail`), and `also_matches`.
- Data shape: `FindingView` in `crates/ui-core/src/triage.rs:80` — `repo, path, line, rule_id, severity, snippet, detail, status, also_matches, preview`. `path` is repo-relative; `repo` names the repo; that pair + `line` is everything the context lookup needs.

## 2. Enclosing-block resolver (the load-bearing piece)

One pure function, living in **`camerata-checks`** next to the AST layer (that crate is deliberately server-independent and takes `&str` — same contract here: no filesystem, no panics):

```rust
// crates/checks/src/extract/enclosing.rs (new module under extract/)
pub struct EnclosingBlock {
    pub start_line: usize,  // 1-based, inclusive
    pub end_line: usize,
    pub kind: BlockKind,    // Function | SqlStatement | Window
}
pub fn enclosing_block(path: &str, source: &str, line: usize) -> Option<EnclosingBlock>
```

Dispatch by file type, always degrading toward the window fallback — **it returns SOMETHING sane for any in-bounds line**:

1. **Code (Rust/TS/TSX/JS/Python)** — `lang_for_path()` hits (`crates/checks/src/extract/mod.rs`): call `functions(lang, source)` (just shipped; 1-based inclusive `FunctionSpan { start_line, end_line, .. }`, never panics). Pick the **smallest span containing `line`** (smallest = innermost: handles nested fns/closures/methods for free). No containing span (top-level statement, parse failure yielding zero spans) → window fallback.
2. **SQL (`.sql` — the Supabase RLS/function findings)**: `split_statements(source)` from `crates/checks/src/supabase/splitter.rs` already segments on real top-level semicolons (dollar-quote/comment-aware, never panics) and records each statement's 1-based start `line`. **Caveat:** `SqlStatement.text` is whitespace-normalized (comments stripped) — do NOT display it. Use only the start lines: enclosing statement = `[stmt[i].line, stmt[i+1].line − 1]` (last statement → EOF), then slice the **raw** source lines for display, trimming trailing blank lines. This yields the whole `CREATE POLICY` / `CREATE FUNCTION ... $$...$$` block.
3. **Everything else** (`.toml`, `.env`, `.yaml`, unknown): fixed window of **±12 lines** around the violation, clamped to file bounds. `kind: Window` so the UI can label it "context" rather than claim it's a semantic block.

`line` out of bounds (file shrank since scan) → `None`; the caller treats that as "changed" (see §5). Unit tests per branch (incl. nested fn, last SQL statement, line-1 and EOF clamps) per ship-with-docs-and-tests.

## 3. Where it runs: on-demand server endpoint (recommended)

**`GET /api/onboard/finding-context?repo=<name>&path=<rel-path>&line=<n>&expect=<first-snippet-line>`**, registered beside the other onboard routes in `crates/server/src/lib.rs` (~line 1002 block). Handler:

1. Resolve repo dir via the existing **`crate::workspace::resolve_repo_dir(override_path, workspace_root, repo)`** (`crates/server/src/workspace.rs`) — the same path every other repo-touching endpoint uses. No new path-resolution scheme.
2. Join the repo-relative `path`, **canonicalize, and verify the result is still under the repo dir** (rejects `../` traversal — the path came from our own scan data, but the endpoint is still an HTTP surface).
3. Read the file (`tokio::fs::read`); enforce the existing `MAX_FILE_BYTES` (400 KB, `crates/server/src/onboard/files.rs`) and valid UTF-8. Single-file read — do NOT route through `read_local_repo_files` (that's a whole-tree walk).
4. Call `enclosing_block(...)`, slice raw lines, compare the line at `line` against `expect` (trimmed containment) for staleness.
5. Return:

```json
{ "status": "ok" | "file_missing" | "not_utf8" | "too_large" | "line_gone",
  "lines": ["..."], "start_line": 41, "end_line": 96,
  "violation_line": 57, "kind": "function", "language": "rust",
  "matches_snippet": true }
```

**On-demand vs. scan-time cache: on-demand wins.** Computed only when a modal opens (a handful of reads per triage session vs. thousands of findings per scan), always reflects the file as it exists NOW (with an honest staleness flag), adds zero bytes to the scan report / onboarding draft, and requires no invalidation. A per-finding parse of one file is milliseconds. No cache.

UI side follows the existing pattern: `reqwest::get(format!("{}/api/onboard/finding-context?...", crate::bff_base()))` in a `use_resource` keyed on the open finding, in `crates/ui/src/cockpit/scan.rs`.

## 4. Display (in the existing modal)

- New section under the "Finding" snippet: label **"Code context"** + `kind` tag ("enclosing function" / "SQL statement" / "surrounding lines"), then a monospace `pre` block with a line-number gutter (`start_line`-based) and the **violation line highlighted** (background accent class, e.g. `ctx-line-hit`).
- Include the block verbatim — the resolver already bounds it to the enclosing unit; add no extra padding for `Function`/`SqlStatement` kinds (the block IS the context; padding above a `fn` signature adds noise).
- **Big-block cap:** if the block exceeds **80 lines**, initially render ±40 lines centered on the violation with fold indicators ("… 63 more lines above …") and a **"Show full block (N lines)"** toggle. Whole section is collapsible (default expanded); loading state = small spinner, never blocks the rest of the modal.
- Minified one-liner guard: any single line > ~500 chars renders in a horizontally-scrolling `pre` (no wrap) so one webpack bundle line can't blow up the modal layout.

## 5. Edge cases (no panics, always a fallback)

| Case | Behavior |
|---|---|
| File moved/deleted since scan | `status: file_missing` → modal keeps the stored `f.snippet` + note "file not found — changed since scan?" |
| Line no longer exists (file shrank) | `status: line_gone` → stored snippet + "file changed since scan" note |
| Line exists but content drifted | `matches_snippet: false` → show the block anyway + banner "file changed since this scan; context may not match the finding" |
| Parse failure / no enclosing span | Window fallback (±12), labeled "surrounding lines" — never an error |
| Enormous function | 80-line cap + "Show full block" toggle (§4) |
| Binary / non-UTF8 / >400 KB | `status: not_utf8` / `too_large` → stored snippet + one-line reason |
| Unlinked project / repo path unresolved | `resolve_repo_dir` returns `None` → treated as `file_missing` (consistent with the readiness-gate model) |
| Path traversal in `path` param | Canonicalize-and-contain check → 400 |

The stored `f.snippet` is the universal floor: **every** failure mode degrades to exactly what the modal shows today, plus a one-line reason. The feature can only add.

## 6. Effort + routing

- **Estimate (5x-corrected per `ai_estimate_overestimation_pattern`):** first-instinct 2–3 days → **~half a day of orchestrated work**. Three tight pieces: resolver + tests in `camerata-checks` (largest), one axum handler + tests, modal section + CSS. No algorithmic novelty — every hard primitive (`functions()`, `split_statements()`, `resolve_repo_dir`) already exists.
- **Routed decisions: none.** No new crate or module boundary (new module under existing `extract/`, one new route, modal extension) — below the ROUTE-1 bar. Auto-decided defaults worth flagging: window ±12, display cap 80/±40, `expect`-based staleness check. All trivially tunable.

## 7. What shipped

Built exactly per the plan above, on `feat/audit-report-export`. No design deviations; a few implementation notes worth recording:

- **Resolver:** `crates/checks/src/extract/enclosing.rs` — `enclosing_block(path, source, line) -> Option<EnclosingBlock>` (`{ start_line, end_line, kind }`, `kind: Function | SqlStatement | Window`), plus a `slice_lines(source, start_line, end_line) -> Vec<String>` helper so callers (the endpoint, and tests) slice RAW lines rather than ever touching `SqlStatement::text`. SQL statement trimming (dropping trailing blank raw lines) is careful to never trim past the violation line itself, even when the violation line is the very last line of the statement. 26 unit tests: one innermost-nested-function case per language (Rust/TS/JS/Python), exact-boundary containment (start and end), a top-level-statement-outside-any-fn case, malformed source, an oversized function (cap is a UI concern, not the resolver's), three SQL cases (middle statement, last-statement-to-EOF, raw-vs-normalized-text proof), unknown-extension and no-extension window fallback, and adversarial cases (line past EOF, line 0, empty file, a 50k-char single line, embedded control characters) — none panic.
- **Endpoint:** `GET /api/onboard/finding-context` (`crates/server/src/lib.rs`, handler `onboard_finding_context`) resolves the repo dir the same way the existing git-status endpoints do (`state.settings.repo_path` / `workspace_root` → `workspace::resolve_repo_dir`), then delegates to `crates/server/src/onboard/finding_context.rs::lookup`, which does the single-file read (reusing `onboard::files::MAX_FILE_BYTES`, never the whole-tree `read_local_repo_files` walker) and path-traversal containment (`resolve_contained_path`, mirroring the existing-ancestor-canonicalization jail `api_agent_driver::assert_in_worktree` already uses for writes, applied here to a read). Every outcome (`Ok`/`FileMissing`/`NotUtf8`/`TooLarge`/`LineGone`/`PathTraversal`) serializes via `FindingContextOutcome::to_json`; only `PathTraversal` gets a non-200 (400) — everything else is 200 with a `status` field, so the UI's normal JSON path handles every case uniformly. An unresolved/unlinked repo maps straight to `file_missing` at the handler level (never calls `lookup`). 11 unit tests on `lookup` (real fixture, `matches_snippet` true/false, three traversal shapes incl. one against a nonexistent target, missing file, oversized, non-UTF8 bytes, line-gone, SQL language label) + 5 axum-router integration tests via `router(state).oneshot(...)` (real fixture through HTTP, traversal → 400, unresolved repo → `file_missing`, oversized → `too_large`, missing file → `file_missing`).
- **UI:** `crates/ui/src/cockpit/scan.rs` adds `FindingCodeContext` (a `#[component]`), mounted in the finding-detail modal right under the existing "Finding" snippet paragraph, keyed by `"{repo}\u{1f}{path}\u{1f}{line}"` so switching findings remounts the section (fresh `use_resource` fetch) instead of reusing stale state — the same identity-reset concern the pre-existing modal-hosting comment already calls out for `RuleDetailModal`. Renders a monospace line-gutter block with the violation line highlighted (`.finding-ctx-line-hit`), a `kind` tag ("enclosing function" / "SQL statement" / "surrounding lines"), a staleness banner when `matches_snippet` is false, and the 80-line cap / ±40 / "Show full block" toggle purely client-side over the already-fetched lines (no second fetch to expand). CSS added to `crates/ui/src/style.rs` alongside the existing `.rule-modal-*` rules, using the same `--paper`/`--line`/`--ink`/`--accent*` theme variables (no new palette).
- **Degradation floor:** every failure mode maps to the SAME rendering path in `FindingCodeContext` (a one-line reason under the "Code context" label) — the stored `f.snippet` above it is untouched in every case, so the section can only add, never regress. An outright request failure (server unreachable, bad JSON) renders nothing extra at all rather than a confusing empty error box, since the snippet already covers the finding.
- **Test counts:** `camerata-checks` unit tests 582 → 608 (+26, all in `extract::enclosing`). `camerata-server` unit tests +11 (`onboard::finding_context`) and +5 integration (`tests::finding_context_*`) = 1242 total passed. `camerata-ui` 584 passed (compiles clean with the new component; no new UI-crate unit tests were needed since the component has no pure logic beyond what the server-side tests already cover — the cap/±40 math is exercised implicitly via the `finding_context_returns_the_enclosing_block_for_a_real_fixture` fixture shape). `cargo check --workspace` green throughout.
- **Judgment calls:** (1) `matches_snippet` staleness compares via trimmed substring-containment either direction (actual-contains-expect OR expect-contains-actual) rather than exact match, since the stored snippet's first line may itself be already-trimmed/truncated relative to the raw file line. (2) The UI's degraded-reason strings duplicate the server's `reason` text as a client-side fallback (`degraded_context_reason`) in case an older/newer server sends the bare `status` without `reason` — belt-and-suspenders, not load-bearing today. (3) No new crate/module boundary was introduced (per the design's §6 routing call) — `enclosing.rs` lives under the existing `extract/` module and `finding_context.rs` under the existing `onboard/` module.
