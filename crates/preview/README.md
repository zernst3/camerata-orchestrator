# camerata-preview

A local-first adapter that manages a Dioxus `dx serve` process and exposes its build/reload
status. This is the first production piece of the live-preview loop for Camerata's
Product-Owner head, turning the de-risking spike in
[`docs/spikes/2026-07-09_dioxus-live-preview-spike.md`](../../docs/spikes/2026-07-09_dioxus-live-preview-spike.md)
into a real, unit-tested adapter.

**Local-first, no cloud.** This crate only ever spawns `dx serve` on the same machine and
serves on `127.0.0.1`. Sharing a preview URL off-box (a cloud tunnel) is a later phase per the
design doc (`docs/plans/2026-07-09_product-owner-head-vibe-mode.md`) and is out of scope here.

## Layers

```
parser   -- pure: one dx stdout/stderr line -> Option<PreviewEvent>
status   -- pure: a PreviewEvent stream folded -> PreviewStatus (Starting -> Serving ->
            Building -> Serving/BuildFailed)
process  -- PreviewServer: spawns `dx serve`, tails its output through parser+status,
            owns the subprocess lifecycle (SIGTERM-then-SIGKILL on Drop + an exit watchdog,
            mirroring crates/ui/src/server_process.rs)
verify   -- closes the spike's headline risk: verify_after_edit() + its pure decision core
            decide_edit_verdict()
```

## The silent-ignore gap

The spike's one real finding: a **syntax-invalid** edit is silently dropped by dx's RSX
hot-reload differ before it ever reaches `rustc` -- with **zero** output at default log
verbosity, and only a `DEBUG`-level "not parseable" line even with `--verbose` (which dx does
not treat as a build trigger either way). Trusting "no dx event arrived" as "the edit is fine"
is therefore unsafe.

[`verify::verify_after_edit`] mitigates this: it waits a bounded timeout for a decisive
`Hotreload`/`BuildOk`/`BuildFailed` event on a `PreviewServer`'s event stream; if nothing
decisive arrives, it falls back to running `cargo check` in the app directory as an
authoritative diagnosis and reports that instead of assuming nothing happened. The pure
decision core, [`verify::decide_edit_verdict`], is unit-tested directly against
mocked `(event-or-timeout, cargo-check-result)` inputs -- no real `dx`/`cargo` subprocess
required for the bulk of the test suite.

## Usage

```rust,ignore
use camerata_preview::{PreviewLaunchConfig, PreviewServer};

let cfg = PreviewLaunchConfig::new("/path/to/dioxus-app", 8123);
let server = PreviewServer::spawn(cfg)?;

println!("preview at {}", server.url());     // http://127.0.0.1:8123/ -- known immediately,
                                               // not dependent on parsing dx's own output
println!("{:?}", server.status());            // PreviewStatus::Serving { .. } / Building / ...

// After a fleet-driven edit to a file under the app dir:
let mut events = server.subscribe_events();
let verdict = camerata_preview::verify_after_edit(
    server.app_dir(),
    &mut events,
    std::time::Duration::from_secs(8),
).await;
```

## Binary resolution

`dx_bin()` resolves the CLI to spawn: the `CAMERATA_DX_BIN` env override if set and non-blank,
else bare `dx` (resolved via `PATH`). Per the spike (Q1, Blocker 1), the `dx` CLI version
matters -- its `Dioxus.toml` schema can drift across versions, so a pinned/compatible `dx` is
the caller's responsibility (this crate doesn't vendor or version-check it).

## Testing

`cargo test -p camerata-preview` runs entirely offline: the parser and status-fold tests are
pure, and `verify`'s pure decision core is tested with mocked inputs. **No test spawns a real
`dx serve`** (too slow/flaky for CI) -- `process::tests::spawns_a_real_dx_serve_and_reports_a_url`
is the one real-process smoke check, gated `#[ignore]`; run it manually with
`cargo test -p camerata-preview -- --ignored spawns_a_real_dx_serve` against a real Dioxus app
(the `dx` CLI must be installed).
