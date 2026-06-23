# Decision: Bound the dep-audit provisioning path to prevent indefinite hangs

**Date:** 2026-06-23
**Status:** Applied

## Root cause

`ensure_osv_scanner` → `download_osv_scanner` built a `reqwest::Client` with
**no connect timeout and no total timeout**.  On a slow, blocked, or
no-network environment the `client.get(url).send().await` future never resolves,
hanging the caller indefinitely.

Because `run_dep_audit` is called unconditionally inside `audit_repos` (the
always-on dep-audit floor), **every onboarding scan** and every test that
exercises `audit_repos` inherited this hang.  The two tests confirmed hanging
(>60 s with no resolution):

- `onboard::tests::deterministic_off_skips_floor`
- `onboard::tests::deterministic_only_runs_floor_and_skips_ai`

This is a concrete instance of the "bound the externals" principle: any async
call that touches the network, a subprocess, or any other external resource
must carry an explicit deadline.  The provisioning chain violated this for
two paths: the reqwest download and the `go install` subprocess.

## Fixes applied

### 1. reqwest connect + total timeout (`tool_provisioning.rs`)

```rust
reqwest::Client::builder()
    .connect_timeout(Duration::from_secs(5))
    .timeout(Duration::from_secs(30))
    .build()
```

- `connect_timeout(5s)`: aborts if the TCP handshake does not complete within
  5 seconds (fast failure on blocked/unroutable hosts).
- `timeout(30s)`: caps the total round-trip (connect + headers + body stream)
  at 30 seconds.  A 30-second download failure is actionable; a 30-minute hang
  is not.
- On either timeout `send()` or `bytes()` returns an `Err` which is mapped to
  `ProvisionError::InstallFailed`, which the scan converts to a `CoverageNote`.
  The scan continues without dep-audit findings.

### 2. `go install` subprocess timeout (`tool_provisioning.rs`)

```rust
tokio::time::timeout(Duration::from_secs(60), Command::new("go").args(["install", ...]).output())
```

`go install` compiles the scanner from source and downloads Go modules over
the network.  It is bounded at 60 seconds — generous for a real network but
guaranteed to resolve rather than hang a CI runner.

The `go env GOPATH` prefix query is bounded at 10 seconds (it is a local
metadata query and should be nearly instant).

### 3. `osv-scanner` subprocess timeout (`dep_audit.rs`)

```rust
tokio::time::timeout(Duration::from_secs(120), Command::new(&bin_str).args([...]).output())
```

Once provisioned, the scanner subprocess itself is capped at 120 seconds.
On very large repos with hundreds of lockfiles the tool can be slow, but an
unbounded subprocess blocks the scan indefinitely.

### 4. `CAMERATA_DISABLE_DEP_AUDIT` env-var skip (`dep_audit.rs`)

A new escape hatch: when the env var `CAMERATA_DISABLE_DEP_AUDIT` is set to
any non-empty value, `run_dep_audit_with_tooling` (and therefore `run_dep_audit`)
returns immediately with an empty findings list and no coverage note — no
provisioning, no network, no subprocess.

**Purpose: test isolation only.**  Onboarding scan tests test *scan logic*, not
dep-audit specifically.  Setting this variable in those tests prevents an
unrelated live download from making them slow, flaky, or broken in CI
environments with no outbound network access.

The two previously-hanging tests now set this variable at the top of their
test body via `std::env::set_var("CAMERATA_DISABLE_DEP_AUDIT", "1")`.

## Caching is preserved (cache-before-download)

The provisioning resolution chain in `ensure_osv_scanner` already checks in
order:

1. PATH probe (`interpreter_available("osv-scanner")`) — fast, no network.
2. Cached binary probe (`osv_scanner_is_provisioned(&bin)`) — fast, filesystem.
3. Only if both miss: download (now bounded).
4. `go install` fallback (now bounded).

A second scan on a machine where step 1 or 2 succeeds never reaches the
download path.  The timeouts only activate when step 3 or 4 is actually
reached.

## Fail-soft preserved

All three timeout/error cases map to `ProvisionError` (provisioning) or a
directly returned `CoverageNote` (subprocess timeout).  In all cases:

- `run_dep_audit_with_tooling` returns `(Vec::new(), Some(CoverageNote {...}))`.
- `audit_repos` pushes the note into `dep_audit_coverage_notes` and continues.
- The scan report carries a coverage note explaining why dep-audit did not run.
- No panic, no hang, no silent clean result.

## Principle

> Every async call that touches an external resource (network, subprocess,
> filesystem-over-network, DNS) must carry an explicit deadline.  An unbounded
> wait is not a performance issue — it is a correctness issue.  The caller
> can always convert a timeout error into a graceful degradation; it cannot
> recover from a future that never resolves.
