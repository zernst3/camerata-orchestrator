# Spike: Dioxus `dx serve` live-preview de-risking (2026-07-09)

Status: DE-RISKING SPIKE, not production code. Target app: `itinerary-app` (Dioxus 0.7.9
web app). Toolchain: `dx` (Dioxus CLI) 0.7.9, rustc 1.95.0, macOS (Darwin 25.3.0),
`wasm32-unknown-unknown` target already installed.

This spike answers the 5 questions from the design doc (`docs/plans/2026-07-09_product-owner-head-vibe-mode.md`,
section 5) needed to de-risk the "live running-app preview" loop for the Product-Owner
head. All test edits were made to `itinerary-app` and **fully reverted** afterward (see
Cleanup section) — nothing in that repo was left changed.

## TL;DR verdict

**Viable, with one real blind spot to design around.** `dx serve` launches cleanly as a
managed background subprocess, serves a working preview URL, hot-reloads external
(non-editor) file edits with excellent latency for both RSX and Rust-logic changes, and
survives ordinary compile errors while reporting them clearly. The one genuine risk: a
**syntax-invalid** edit is silently swallowed by an earlier RSX-diffing pre-pass with **zero
visible output in default logging** — the preview adapter cannot rely on dx's own log
stream alone to always detect a broken edit; it needs a timeout-based fallback (see
Recommendation).

| Question | Answer |
|---|---|
| Q1: Launch + URL | Yes. `http://127.0.0.1:8080/`, HTTP 200, HTML shell served. Two pre-existing blockers found and worked around (see below). |
| Q2: External-edit hot reload | Yes, confirmed both paths. RSX text change: ~1s (hot-patch, no recompile). Rust logic change: ~4-5s steady-state (real incremental rebuild). |
| Q3: Build/error signals | Three distinct signal classes identified; dx **survives** and **clearly reports** ordinary compile errors (`ERROR`/`WARN` at default verbosity) but **silently ignores** syntax-invalid edits unless `--verbose` is passed. |
| Q4: Visual confirmation | `curl` only ever shows the static shell (client-rendered). Confirmed the live-reload transport is a `WebSocket` embedded in the wasm-bindgen JS glue; recommend driving a real/headless browser for true visual confirmation. |
| Q5: Process management | Clean `SIGTERM` to the `dx serve` parent kills it AND its live `cargo`/`rustc` children within ~2s, and frees the port. No orphans. Matches the existing `server_process.rs` pattern. |

---

## Q1 — Launching `dx serve` as a managed background process

Command used: `nohup dx serve --platform web > <logfile> 2>&1 &`, waiting on the log for a
serving line, then `curl` against the guessed default port.

**Two pre-existing blockers were hit before any preview loop questions could even be
tested** — both worth flagging as findings in their own right, separate from the preview
mechanism itself:

### Blocker 1 — `Dioxus.toml` schema mismatch with the installed CLI version

First `dx serve` invocation failed immediately:

```
ERROR dx serve: Failed to parse Dioxus.toml at ".../Dioxus.toml": TOML parse error at line 11, column 1
   |
11 | [web.resource]
   | ^^^^^^^^^^^^^^
missing field `dev`
```

`itinerary-app`'s checked-in `Dioxus.toml` (authored against an earlier `dx`/Dioxus config
schema) is missing a `[web.resource.dev]` table now required by dx 0.7.9. Worked around
for the spike by adding:

```toml
[web.resource.dev]
style = []
script = []
```

**Implication for the adapter:** the preview runtime must pin (or vendor) a `dx` CLI
version compatible with whatever `Dioxus.toml` schema Camerata's Rust-fullstack scaffolder
emits, or defensively validate/patch the config before spawning `dx serve`.

### Blocker 2 — `itinerary-app`'s web target does not currently compile (pre-existing app bug, unrelated to the preview mechanism)

Once the config parsed, the real build failed with 6 identical errors:

```
39.86s ERROR cannot find module or crate `reqwest` in this scope
...
40.27s  WARN error: could not compile `itinerary-app` (lib) due to 6 previous errors
40.28s ERROR Build failed: cargo build finished with errors for target: itinerary-app [wasm32-unknown-unknown]
```

Root cause: `src/frontend/login.rs` and `src/frontend/timeline.rs` call
`reqwest::Client::new()` unconditionally (the `frontend` module has no
`#[cfg(target_arch = "wasm32")]` gate — only `backend` does, in `src/lib.rs`), but
`Cargo.toml` declares `reqwest` **only** under
`[target.'cfg(not(target_arch = "wasm32"))'.dependencies]` (rustls-tls backend, native-only).
This means **`itinerary-app`'s web/wasm target cannot build at all as currently checked
into the repo** — a real, pre-existing bug in the app, unrelated to anything about the
preview mechanism. Worked around for the spike only, by adding a wasm-compatible `reqwest`
under the wasm32 target block (`features = ["json"]`, no TLS backend needed since the
browser's `fetch()` handles transport):

```toml
[target.'cfg(target_arch = "wasm32")'.dependencies]
reqwest = { version = "0.12", default-features = false, features = ["json"] }
```

This is worth reporting to Zach separately as an actual itinerary-app defect — it's
exactly the class of bug the live-preview loop is meant to catch (see Q3).

### Confirmed working after both fixes

```
$ curl -sv http://127.0.0.1:8080/
< HTTP/1.1 200 OK
< content-type: text/html
...
<!DOCTYPE html>
<html lang="en">
  <head>
    <title>Travel ItineraryTravel Itinerary</title>
    ...
    <div id="main"></div>
    <script type="module" async src="/./wasm/itinerary-app.js"></script>
```

- **URL/port:** `http://127.0.0.1:8080/` — dx's default port; no port was configured in
  `Dioxus.toml`, so the adapter should pass `--port <chosen>` explicitly rather than rely
  on the default (needed anyway to run multiple previews concurrently).
- **Startup timing:** the first full build after both fixes reported `Build completed
  successfully in 19.26s` (dx's own measurement) — a from-scratch wasm32 dependency
  compile (js-sys, web-sys, etc.), though the `target/` dir already had partial warm
  compilation from the two failed prior attempts, so this is not a purely cold number. A
  **later fresh `dx serve` process** (same warm `target/`) rebuilt the whole thing in just
  `5.62s`. No `rustup target add wasm32-unknown-unknown` was needed (already installed).
  `dx` did auto-install `wasm-bindgen-cli@0.2.126` from GitHub on first launch (~1s,
  cached-after-first-run network dependency — worth knowing for a fresh machine).

---

## Q2 — Does an external file edit trigger hot-reload? (the crux question)

All edits in this section were made by a separate Bash/Edit-tool process, **not** by an
editor and **not** by `dx serve` itself — simulating the fleet.

### RSX/text change

Edited `h1 { "Travel Itinerary" }` → `h1 { "Travel Itinerary SPIKE-RSX-EDIT-MARKER" }` in
`src/frontend/timeline.rs`. Result, correctly classified as hot-patchable (**no** rebuild,
**no** `cargo`/`rustc` process spawned — confirmed via `ps aux`):

```
177.27s  INFO Hotreloading: /src/frontend/timeline.rs.tmp.2169.de41133820de
```

Tight bash-timestamped repeat (write via `sed -i ''`, timestamps captured immediately
before/after the write in the same shell invocation to avoid tool round-trip noise):
write completed at epoch `1783632859.686`; log line appeared at internal-clock `379.77s`
(≈ epoch `1783632858.77`, within ~1s of measurement rounding on the process-start
reference). **Latency: ~1 second, effectively near-instant.**

### Rust logic change

Edited the gap-detection threshold `chrono::Duration::hours(6)` → `hours(N)` inside a
`for` loop's `if` condition in `timeline.rs` (real logic, not rsx text). Correctly **not**
hot-patched — dx spawned a real `cargo rustc` + `rustc` build (confirmed live via `ps aux`
showing the `itinerary_app` crate compiling), completing with:

```
251.77s  INFO Build completed in 4.85s
414.11s  INFO Build completed in 4.67s
 76.12s  INFO Build completed in 3.65s   (fresh process, warm target/)
```

Tight-timed trial: write completed at `1783632888.139`; `Build completed` logged at
internal-clock `414.11s` (≈ epoch `1783632893.11`). **End-to-end latency ≈ 5 seconds**
(of which 4.67s is the compile+wasm-bindgen-bundle time dx itself reports; the rest is
watcher debounce). This is the number to use for planning UX around Class-B logic
changes — **not** the ~19s cold-start number, which only applies to the very first build
of a session.

---

## Q3 — Machine-detectable build/reload/error signals

Three distinct classes of signal were observed, at two different log verbosities
(default vs. `--verbose`, which adds `DEBUG`-level lines and per-phase progress bars).

### 1. Fast RSX-only hot-patch (visible by default)
```
INFO Hotreloading: <path>
```
No separate "done" line — presence of this line **is** the completion signal.

### 2. Full rebuild, success (visible by default)
```
INFO Build completed successfully in 5.37s, launching app! 💫   ← only on the FIRST build/launch
INFO Build completed in 3.65s                                   ← every subsequent successful full rebuild
```
The `, launching app! 💫` suffix appears **only once**, on the initial launch; the adapter
should treat `Build completed` (with or without the suffix) as "rebuild finished, reload
now live."

### 3. Full rebuild, real compile error (visible by default) — confirmed dx SURVIVES

Reproduced with the missing-`reqwest`-for-wasm bug from Q1 (a genuine `rustc` E0433):

```
39.86s ERROR cannot find module or crate `reqwest` in this scope
   (×6)
40.27s  WARN error: could not compile `itinerary-app` (lib) due to 6 previous errors
40.27s  WARN Caused by:
40.27s  WARN   process didn't exit successfully: `.../rustc ...` (exit status: 1)
40.28s ERROR Build failed: cargo build finished with errors for target: itinerary-app [wasm32-unknown-unknown]
```

`ps -p <pid>` confirmed the **`dx serve` process itself stayed alive** throughout — it did
not crash or exit; it kept the previous (nonexistent, in this case pre-first-build) state
and simply reported the failure. After fixing the dependency, the next build succeeded
normally. **This class of error is exactly the "does dx survive + report clearly"
question, and the answer is yes.**

### 4. Syntax-invalid edit — THE BLIND SPOT (invisible by default, DEBUG-only even with `--verbose`)

Introduced a deliberate token-level syntax error (`if gap_duration_BROKEN >>> chrono::Duration::hours(N)`)
into a `.rs` file mid-session, while the app was already running successfully.

**In default (non-`--verbose`) logging: absolutely nothing was printed.** No error, no
warning, no rebuild line — for as long as we waited (multiple ~100s+ polls across two
separate trials). The running app kept serving the last-good build with no visible
indication anything was wrong.

Re-ran with `--verbose` to see what was actually happening under the hood:

```
 10.98s DEBUG Failed to canonicalize hotreloaded asset: No such file or directory (os error 2)
 10.98s DEBUG Failed to read rust file while hotreloading: ".../.!79680!timeline.rs"
   (×3 retries)
 10.98s DEBUG Diff rsx returned not parseable
 10.98s DEBUG Diff rsx returned not parseable
 10.98s DEBUG Ignoring file change: /src/frontend/.!79680!timeline.rs dx_src=dev
```

An earlier pre-pass — dx's RSX hot-reload differ — tries to parse the changed file **before**
it ever reaches `rustc`/`cargo`. When the content doesn't parse at all (a raw syntax
error), it logs `Diff rsx returned not parseable` / `Ignoring file change` at `DEBUG`
level only, and **does not fall back to a full `cargo build`** that would surface a normal
rustc diagnostic. This means: **a broken fleet edit that fails at the syntax level is
invisible to a log-scraping adapter unless it runs dx with `--verbose`, and even then the
signal is a DEBUG line, not the familiar `error[EXXXX]` shape.**

**Recovery is real, not a permanent wedge** — confirmed by making a genuinely new
(not merely reverted-to-baseline) valid edit afterward, which correctly triggered a normal
rebuild:
```
 71.92s DEBUG Failed to canonicalize hotreloaded asset: ...   ← benign noise, see below
 73.78s DEBUG Running wasm-bindgen dx_src=bundle
 76.12s  INFO Build completed in 3.65s
```
So dx doesn't get stuck; it just doesn't react/report anything for the one bad revision.

### Benign noise: atomic-rename writes confuse the watcher's path resolution (macOS)

Every hot-reload/rebuild trigger in this spike (both `sed -i ''` and, once, the Edit tool)
showed the *transient* temp-file path from an atomic write-then-rename, not the real
filename — e.g. `Hotreloading: /src/frontend/.!81005!timeline.rs`. This is how BSD
`sed -i ''` (and many editors) write files on macOS; dx's FSEvents-based watcher observes
the temp path directly and sometimes races the rename (`Failed to canonicalize... No such
file or directory`, retried 3×, then falls through to reading the real file's current
content regardless). **Functionally harmless** — the correct content still gets picked up
— but any adapter that string-matches the path in these log lines should not assume it
equals the real source file path, and should treat `Failed to canonicalize`/`Failed to
read rust file` as noise, not errors.

---

## Q4 — Visual confirmation feasibility

`curl` on `/` only ever returns the static HTML shell — confirmed byte-identical across
RSX text edits, since all real rendering happens client-side in the wasm bundle:
```html
<div id="main"></div>
<script type="module" async src="/./wasm/itinerary-app.js"></script>
```

Fetched the JS glue directly and confirmed it embeds a live-reload transport:
```
$ curl -s http://127.0.0.1:8080/./wasm/itinerary-app.js | grep -io websocket
WebSocket
```
This is dx's devtools client, injected into dev builds, which opens a `WebSocket` back to
the dev server to receive hot-patch messages and apply them live in the running DOM
without a full page navigation — this is the actual "did it go live" channel, and it is
**not** observable via plain `curl`.

**Recommendation (not built in this spike):** drive a real or headless browser
(Playwright/Puppeteer, or an embedded WebView) against the served URL, and snapshot/diff
DOM content after each edit — or, better, build the click-to-report layer to run *inside*
that browser context from the start (it needs to anyway, for "click a component, describe
the bug"), so visual confirmation falls out of that work rather than needing a separate
mechanism.

---

## Q5 — Process management

With a rebuild actively in flight (`cargo rustc` + `rustc` children confirmed alive via
`ps aux`, real PIDs captured), sent `kill -TERM` to the `dx serve` parent PID.

```
$ kill -TERM 79154
$ sleep 2
$ ps aux | grep -E "79154|rustc|cargo|wasm-bindgen"        → (no output — all gone)
$ lsof -nP -iTCP:8080 -sTCP:LISTEN                          → (no output — port freed)
```

Plain `SIGTERM` was sufficient in every trial — no orphaned `cargo`/`rustc`/`wasm-bindgen`
processes, no `SIGKILL` escalation needed, port released immediately. This matches the
termination model already implemented in `crates/ui/src/server_process.rs`
(`ServerGuard`: SIGTERM → poll → SIGKILL fallback, plus a detached shell watchdog against
`std::process::exit` skipping Rust destructors) — that exact pattern can be reused for a
`dx serve` child in place of `camerata-server`.

---

## Recommendation for `crates/preview`

1. **Launch:** spawn `dx serve --platform web --port <chosen> --open false --interactive false`
   as a subprocess via `std::process::Command`, following the `ensure_server_running` /
   `ServerGuard` pattern in `crates/ui/src/server_process.rs` verbatim — health-probe
   first to decide reuse-vs-spawn, redirect stdout+stderr to a log file the adapter tails,
   SIGTERM-then-SIGKILL Drop + exit watchdog for lifecycle safety (Q5 confirms this
   pattern works unchanged). Pin an explicit port per preview instance rather than relying
   on dx's default, so multiple app previews can run concurrently.
2. **CLI version pinning:** pin the `dx` CLI version the adapter shells out to against
   whatever `Dioxus.toml` schema Camerata's Rust-fullstack scaffolder emits (Q1, Blocker 1)
   — config schema drift across dx versions is a real, already-observed failure mode.
3. **Log parsing:** classify lines by simple prefix match — `Hotreloading:` → patched
   (~1s), `Build completed` → rebuilt/relaunch-if-first (~5s steady state, longer cold),
   `ERROR`/`Build failed`/`could not compile` → surface the rustc diagnostic verbatim to
   the user, keep the last-good preview alive.
4. **Close the silent-failure gap (the headline risk):** because a syntax-invalid edit
   produces **zero** default-log output and only a `DEBUG`-level "not parseable" line even
   with `--verbose`, do not trust dx's log stream alone for "no news is good news." Run
   `--verbose` for the extra visibility, AND apply the adapter's own timeout: if no
   `Hotreloading:`/`Build completed`/`ERROR` line appears within N seconds of a fleet-driven
   write, run `cargo check` out-of-band against the app to get an authoritative diagnosis
   and report *that* to the user/orchestrator instead of assuming nothing happened.
5. **Visual confirmation / click-to-report:** build the feedback layer as an in-browser
   (or headless-browser-hosted) client from the start — it can piggyback on the same
   devtools `WebSocket` dx already injects (Q4), giving "reload confirmed" and "user can
   click to report" from one integration point rather than two.

### Risks called out

- **Rebuild latency for logic changes** was fast here (~4-5s steady state) because
  `itinerary-app` is small; this will grow with real app size/dependency graph and should
  be monitored, not assumed constant, as Camerata-scaffolded apps get bigger.
- **The silent-failure blind spot (Q3, #4)** is the one must-mitigate risk before relying
  on this loop for a fleet that will, by construction, sometimes emit invalid Rust.
- `itinerary-app`'s web target is currently broken as checked in (Q1, Blocker 2) — a
  separate, real app bug worth reporting to Zach, and a nice illustration that the
  ordinary-compile-error path (Q3, #3) is exactly what the live-preview loop is designed
  to catch and let a user say "that's broken" about.

---

## Cleanup

All test edits were made only to `itinerary-app` and fully reverted:
- `Dioxus.toml` — restored (removed the `[web.resource.dev]` addition)
- `Cargo.toml` — restored (removed the wasm32-target `reqwest` addition)
- `src/frontend/timeline.rs` — restored (all marker text / threshold-value edits reverted)

All three were diffed byte-for-byte against pre-edit backups and confirmed identical. No
`dx`/`cargo`/`rustc`/`wasm-bindgen` processes were left running; port 8080 was confirmed
free. `git -C itinerary-app status` shows the same untracked-files-only state as at the
start of the spike (that repo has no commits yet, so there was no tracked baseline to
diff against — verification was done via manual backup/diff instead).
