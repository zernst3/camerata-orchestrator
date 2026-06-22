# CI security rules — Part A (schema flags + the two rules + propose logic)

**Date:** 2026-06-22 · **Implements:** Part A of
[2026-06-22_ci_security_rules_and_scan_time_preview.md](./2026-06-22_ci_security_rules_and_scan_time_preview.md).

Part A delivers the two opt-in CI/CD security rules, the two schema flags they need, and the
propose-logic change that keeps them un-recommended. Part B (the scan-time deterministic preview
that actually runs the tools) is a separate follow-up and is intentionally NOT built here. The
`layer3_only` flag is carried now but consumed later (by the runners + Part B).

## Schema flags (new) — `crates/rules/src/lib.rs`

Two rule-level booleans were added to `RuleToml` (with `#[serde(default)]`, default `false`) and to
the public `Rule`, threaded through the `load_one` (`RuleToml` → `Rule`) conversion, each with an
accessor:

- **`opt_in_only`** (`Rule::is_opt_in_only()`) — a grounded rule that must NEVER be
  auto-recommended / pre-checked in the onboarding proposal, even when it is `grounded`/`verified`
  and stack-relevant. It still appears in the proposal list so the architect can deliberately opt
  in; it is just never pre-ticked.
- **`layer3_only`** (`Rule::is_layer3_only()`) — a CI-tier rule that must never run at layer-2 or at
  scan time (too heavy / not locally runnable). Carried now; consumed by the runners + the
  scan-time preview (Part B).

Both default to `false`, so every existing corpus TOML loads unchanged. The test helpers
(`make_rule`, `parse_rule`) were updated to construct the two new fields.

## Propose logic — `crates/server/src/onboard.rs`

In `propose_corpus_rules`, the auto-recommend (pre-checked) computation now ANDs in
`!r.is_opt_in_only()`:

```rust
is_auto_recommended: (is_suggested || r.domain == "agentic")
    && r.is_auto_recommended()
    && !r.is_opt_in_only(),
```

So an `opt_in_only` rule is never pre-ticked regardless of grounding/stack. It still appears in the
proposal list (its `recommended` flag is unchanged), so the architect can opt in deliberately.

## The two rules — `crates/rules/principles/ci-cd/`

Both: `enforcement = mechanical`, `domain = "ci-cd"`, `opt_in_only = true`,
`verification = grounded`, rule-level `default = false` and NO `[decision].default` (selecting forces
a conscious tier choice — the amber "must choose" state). They exist to GENERATE CI stories (a
DevOps engineer wires the tool); the option directives describe wiring the tool, not constraining the
agent's code.

### `CICD-SEMGREP-SECURITY-SCAN-1`
"Run the Semgrep security suite (CI + scan preview)". `layer3_only = false` (Semgrep CE is
single-file and lightweight enough to run at scan preview + layer-2 + CI). Sources cite
semgrep.dev, the CE getting-started docs, pricing, and Pro-vs-OSS, with `linter = "semgrep"`. Two
options:

- **`semgrep-community-edition`** — free, LGPL-2.1 OSS CLI; runs on ANY repo (public or private),
  single-file (no whole-program build), ~3,000 community rules; SARIF output; runs at scan preview +
  layer-2 + CI.
- **`semgrep-appsec-platform-pro`** — paid Team/Enterprise; cross-file taint analysis, ~20,000 Pro
  rules, managed platform; CI / platform tier (per-seat cost).

### `CICD-CODEQL-SECURITY-SCAN-1`
"Run the CodeQL security suite in CI (layer-3 only)". `layer3_only = true` (whole-program DB build
is too heavy for scan/in-loop). Sources cite codeql.github.com, GitHub code-scanning docs, the GHAS
overview, and the CodeQL CLI license, with `linter = "codeql"`. Two options:

- **`codeql-public-free`** — free ONLY on public/open-source repos; the directive states the full
  limitations: private/closed-source requires GitHub Advanced Security (paid, per active committer);
  heavy whole-program DB build → CI / layer-3 ONLY, never scan or in-loop.
- **`codeql-ghas-paid`** — GitHub Advanced Security for private repos, billed per active committer;
  same CI / layer-3-only placement.

## Tests
New tests in `crates/rules/src/lib.rs`: flags default `false`; flags parse when set;
`is_auto_recommended() && !is_opt_in_only()` is false for an opt-in grounded rule; and a corpus-load
test asserting both new TOMLs load with the expected flags/enforcement/domain/options and no default.
`cargo build --workspace -j2` and `cargo test -p camerata-rules -p camerata-server` are green.
