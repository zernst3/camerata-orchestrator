# CI-security Part B — scan-time deterministic preview pass

**Date:** 2026-06-22 · **Builds on:** `2026-06-22_ci_security_rules_and_scan_time_preview.md` (the design).

Part A landed the two CI rules + the `opt_in_only` / `layer3_only` schema flags. Part B is the
RUNTIME: at onboarding scan, Camerata runs the selected mechanical rules' own tools itself and folds
the results into triage as **preview findings** — even when the rule is not yet wired into the repo.

## What it does

For each SELECTED mechanical rule that is scan-runnable (mechanical AND NOT `layer3_only`), the scan:

1. Derives the rule's TOOL from its corpus `linter` source (`clippy: …` → clippy, `Ruff: …` → ruff,
   an eslint-family id → eslint, `semgrep` → semgrep).
2. Groups the selected rules by tool and runs each tool ONCE with a Camerata-supplied config that
   enables exactly those rules (clippy `-W clippy::<lint>`, ruff `--select <codes>`,
   eslint `--no-eslintrc --rule '{"id":"error"}' --format sarif`, semgrep `--sarif --config p/ci`).
3. Parses the output — SARIF preferred (semgrep native, eslint via the SARIF formatter), per-tool JSON
   otherwise (ruff `--output-format json`, clippy `--message-format=json` NDJSON) — into the existing
   `Finding` shape (file / line / rule-id / message / severity).
4. Appends the preview findings to the audit report.

This works EVEN IF the rule isn't adopted in the repo yet: you select it, you see findings.

## The `preview` flag

`Finding` gained `preview: bool` (`#[serde(default)]`, back-compatible) + `preview_tool: Option<String>`.
A preview finding is **deterministic but advisory**: a stable tool rule-id (so triage treats it like
the deterministic floor, NOT the AI-advisory bucket — and it stays OUT of the LLM review, saving
tokens), but it is NOT enforcement. Its `status` is `suppressed-baseline` (not `active`) so it never
reads as an enforced gate hit, and its `detail` carries "NOT enforced until wired into CI."

## Preview vs. gate (decoupling)

The repo is the source of truth for the GATE (layer-2/3, authoritative, repo-pinned, no drift). The
SCAN is an advisory preview, so it does NOT need to be repo-sourced. A preview finding is a preview,
not enforcement: the CI story still has to wire the rule for the gate to block on it. A preview uses
Camerata's tool version, which may differ from what the repo eventually pins — preview is indicative,
the gate is authoritative.

## Tools wired

- **semgrep** — SARIF, end-to-end (config pack `p/ci`).
- **ruff** — `--output-format json`, end-to-end.
- **clippy** — `--message-format=json` NDJSON, end-to-end.
- **eslint** — SARIF via `@microsoft/eslint-formatter-sarif`, end-to-end.

Excluded by design: **CodeQL** and the **paid cloud tiers** are `layer3_only` (heavy DB build / not
locally runnable) — they NEVER preview. `split_scannable_rules` filters them out before the pass, and
`group_by_tool` defends against them too.

## Graceful / honesty stance (no false clean)

Mirrors the layer-2 runners' fail-closed posture, adapted for an advisory pass: a missing tool, an
unparseable output, or a mechanical rule whose linter we don't drive end-to-end (golangci-lint,
rubocop, Checkstyle, Roslyn) yields a benign NOTE finding ("Could not preview X — enforces once
wired"), never an empty (clean) result. The note is itself a `preview` finding so it surfaces in the
preview lane, not the enforced lane.

## UI

The triage findings table's **Authority** column now has a THIRD tier: `preview`, rendered as a purple
badge "Preview · not enforced until wired", distinct from the green "Rule · enforced" floor badge and
the blue "AI · advisory" badge. It is filterable (enforced / preview / advisory). The CSV export gained
`preview` + `preview_tool` columns.

## Wiring

`run_scan_tools` runs at all THREE audit entry points: `onboard_audit` (sync), `onboard_audit_start`
(async job; the preview merges into the report the job stores, which `onboard_audit_job` then serves),
via a shared `merge_scan_preview` helper. Mechanical rules stay dropped from the AI scan (unchanged);
the preview pass runs their deterministic tools instead.

## Scoped down (honest)

- Only clippy / ruff / eslint / semgrep are driven end-to-end. golangci-lint, rubocop, Checkstyle,
  PMD, Roslyn, Bandit-via-non-ruff, etc. degrade to a graceful NOTE rather than a silent skip. Adding
  one is mechanical: a `ScanTool` variant + an arm in `run_one_tool` + a parser (reuse `parse_sarif`
  where the tool emits SARIF).
- The preview runs the tool against the repo's working tree with Camerata's installed tool version. It
  does NOT install or pin a tool — that's the gate's job (the repo-pinned toolchain). Hence the "may
  differ from what the repo pins" caveat.
- Severity is normalized conservatively (tool `error` → high, `warning` → medium, else medium). A
  preview is indicative; exact severity is the gate's call once wired.

## Tests (token-free, no real tools)

`linter`-source → tool grouping; SARIF + ruff-JSON + clippy-NDJSON parsing into Findings (sample
fixtures); `layer3_only` excluded from the pass; the `preview` flag round-trips (incl. legacy
back-compat); the graceful no-tool path emits a note, not a clean; the UI Authority labeling
(preview / enforced / advisory). `cargo build --workspace` + `cargo test -p camerata-server
-p camerata-ui` green.
