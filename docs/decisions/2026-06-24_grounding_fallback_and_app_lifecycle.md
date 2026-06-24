# 2026-06-24 — Grounding Fallback and App Lifecycle

## Context

Two related bugs caused the chat assistant to show "Scan results (none yet)" after a
completed scan:

1. **Root cause (Fix B)**: Closing the macOS app window did NOT exit the process. The
   Dioxus desktop default (`WindowCloseBehaviour::WindowHides`) keeps the process alive,
   so the embedded Axum BFF kept holding `:8787`. The next `cargo run -p camerata-ui`
   built fresh code but its embedded server could not bind the port and silently fell back
   to the stale server — so all rebuilds were talking to old code.

2. **Fix A**: Even with the lifecycle fixed, `active_project_context` only looked up scan
   results under the ACTIVE project's key in `last_scan`. If the active project changed
   between scan submission and scan completion (or no project was active at submission
   time), the per-project key was empty and the fallback chain bottomed out at "none yet."

## Decisions

### Fix B — window close fully terminates the process

`crates/ui/src/main.rs`: the Dioxus `Config` now explicitly sets:

```rust
.with_close_behaviour(WindowCloseBehaviour::WindowCloses)
.with_exits_when_last_window_closes(true)
```

`WindowCloseBehaviour::WindowCloses` overrides the macOS default (`WindowHides`) so the
OS window close button actually closes the window. `with_exits_when_last_window_closes(true)`
(which is the framework default but is now explicit) causes the process to exit when the
last window closes. Process exit drops all non-detached threads, including the background
BFF thread, releasing the `:8787` bind.

The embedded BFF bind-failure error message is now loud and unmistakable: if `serve()` fails
with "address already in use," a boxed warning is printed to stderr identifying the stale
server and instructing the user to quit all Camerata instances before relaunching. The
previous message (`"embedded BFF exited: {e}"`) was easy to miss.

### Fix A — project-agnostic `recent_scan` fallback

`crates/server/src/lib.rs`:

- Added `recent_scan: Arc<Mutex<Option<ScanReport>>>` to `AppState` — the most recently
  completed scan, regardless of project.
- `set_last_scan` (the canonical write path for both the synchronous `onboard_audit` handler
  and via the accessor) now also writes `recent_scan`.
- The async job path (which bypasses `set_last_scan` and writes `last_scan` directly) now
  also writes `recent_scan` via its captured `Arc`.
- Added `get_recent_scan() -> Option<ScanReport>` — a fail-soft, clone-on-read accessor.
- All three branches in `active_project_context` (PostOnboard, PreOnboard-with-draft, Blank)
  now use a three-tier fallback chain:
  1. Draft extract (`extract_scan_results_from_draft`)
  2. Per-project `get_last_scan(&project.id)`
  3. Project-agnostic `get_recent_scan()`

The `recent_scan` fallback is intentionally last-resort: it can surface results from a
different project than the one currently active. This is the right trade-off because the
alternative (showing "none yet" when a scan just completed) is worse.

## Tests added (788 total, +4 new)

- `set_last_scan_populates_recent_scan` — round-trip: `set_last_scan` → `get_recent_scan` is Some.
- `recent_scan_tracks_most_recent_write` — second write wins; per-project entries stay independent.
- `active_project_context_falls_back_to_recent_scan` — active project has no per-project entry; recent_scan surfaces the scan.
- `per_project_last_scan_wins_over_recent_scan` — per-project entry takes precedence when both are present.
