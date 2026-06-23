# Decision: Dependency-vulnerability audit as always-on onboarding floor

**Date:** 2026-06-23
**Status:** Accepted
**Scope:** `crates/server/src/dep_audit.rs`, `tool_provisioning.rs`, `onboard.rs`

---

## Context

Camerata's onboarding scan has two tiers:

1. **Always-on deterministic floor** (`audit_files`) — content rules for hardcoded
   secrets, raw-SQL concatenation, secret-bearing file paths. These are pure functions
   over file content; they run unconditionally and produce ENFORCED findings.

2. **Opt-in SAST preview** (`run_scan_tools`) — semgrep, eslint, ruff, clippy driven
   against the architect's selected rules. Advisory-but-deterministic; labeled "not
   enforced until wired."

Neither tier scanned for **known-vulnerable dependencies** (CVEs in the project's
lockfiles). A repo could have a `RUSTSEC-XXXX-XXXX` in `Cargo.lock` or a `GHSA-…`
in `package-lock.json` and the onboarding scan would say nothing.

---

## Decision

Add dependency-vulnerability scanning as an **always-on part of the floor** (not
opt-in, not gated on rule selection) using **osv-scanner** (Google's open-source
multi-ecosystem scanner).

### Why osv-scanner

- **Multi-ecosystem in one binary.** A single `osv-scanner -r .` invocation
  discovers and scans Cargo.lock, package-lock.json, go.sum, poetry.lock, Gemfile.lock,
  pom.xml, and every other supported lockfile in the repo tree. No per-ecosystem
  configuration required.
- **OSV database.** Backed by a public, well-maintained vulnerability database that
  aggregates RUSTSEC, GitHub Advisory DB, NVD/CVE, and others. The advisory IDs are
  stable and cross-referenced with aliases (CVE-…, GHSA-…).
- **No false positives from code heuristics.** osv-scanner only fires on packages
  that appear in lockfiles AND have a recorded advisory with an unambiguous affected
  version range. There is no "best-effort pattern match" ambiguity.
- **Prebuilt release binaries.** No runtime dependency (no Python venv, no npm
  workspace) for the common case (linux/darwin amd64/arm64). Fall through to `go
  install` when the prebuilt isn't available.

### Why always-on (not opt-in like SAST)

Knowing your current CVE exposure is prerequisite-level information — it is not a
stylistic or architectural opinion that a team needs to debate. Every repo that ships
software should know whether any of its transitive dependencies have a known exploit.
The SAST rules (semgrep, eslint patterns) carry architectural opinions; dep-auditing
does not. It belongs on the floor.

### Placement: floor findings, not SAST preview findings

Dep-audit findings carry `rule_id = "DEP-AUDIT-1"` (stable umbrella) with the
advisory ID in `detail`. They are NOT `preview = true` (preview is for SAST tools
run during the tool-preview pass). They are NOT added to `AUDIT_RULES` (that list
drives the regex-based content rules). They are a third category: a **provisioned-tool
floor check** that runs via `crate::dep_audit::run_dep_audit` in `audit_repos`,
after `audit_files`, for every repo on every scan.

### Network-required + fail-soft

osv-scanner queries the OSV database over the network. This is accepted: the
onboarding scan already requires network for GitHub tarball download and (optionally)
the LLM audit. If the network is unavailable, or osv-scanner cannot be provisioned
(unsupported platform, no Go toolchain, download failure), the pass emits a
`CoverageNote` and the scan continues. A missing dep-audit is surfaced honestly in
the UI's "Scan coverage" section — never silently swallowed as a clean result.

### Not a Layer-1 gate arm

The dep-audit is not added to the gateway's `RULE_REGISTRY` or `lookup_arm`. It
does not block writes at the content gate. It is an onboarding-time advisory scan
that tells the architect "here is your current exposure." Blocking CI on a specific
CVE is a separate workflow (e.g. `cargo deny`, `npm audit --audit-level=high`).

---

## Finding shape

| Field | Value |
|---|---|
| `rule_id` | `DEP-AUDIT-1` (stable umbrella; advisory ID in `detail`) |
| `path` | Lockfile path relative to the repo root (e.g. `Cargo.lock`) |
| `line` | `0` (no per-CVE line number in a lockfile) |
| `severity` | Mapped from `database_specific.severity` word or CVSS base score |
| `snippet` | `<pkg-name>@<version>` (the affected coordinate) |
| `detail` | `<advisory-id> (also: <aliases>): <summary> (affects <pkg>@<version>)` |
| `preview` | `false` (floor-level, not a preview finding) |
| `status` | `active` (suppression classification applies like any other finding) |

---

## Severity mapping

| Source | Rule |
|---|---|
| `database_specific.severity` (word) | CRITICAL/HIGH/MEDIUM/MODERATE/LOW mapped directly |
| `severity[type=CVSS_V3].score` (numeric) | >=9.0 critical, >=7.0 high, >=4.0 medium, <4.0 low |
| No severity info | `medium` (conservative default) |

---

## Provisioning chain

Pinned version: **v1.9.2**

1. `osv-scanner` on PATH — use it immediately (zero disk).
2. Cached binary at `<data_dir>/camerata/tooling/osv-scanner/osv-scanner` — use it.
3. Download prebuilt release binary from GitHub releases (darwin/linux x amd64/arm64).
4. `go install github.com/google/osv-scanner/cmd/osv-scanner@v1.9.2` if Go is present.
5. None work — return `ProvisionError::InstallFailed`, caller emits `CoverageNote`.

---

## Alternatives considered

- **`cargo audit` only:** Rust-only. Agora is multi-ecosystem (Node, Python, Rust).
- **`npm audit` only:** Node-only, same problem.
- **Snyk / Dependabot:** Cloud-only; requires API tokens; not appropriate for a
  local-first scan tool.
- **Opt-in SAST-style:** Rejected. CVE exposure is factual, not a design opinion;
  the team should always know their exposure at onboarding time.
