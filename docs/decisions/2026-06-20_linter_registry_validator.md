# 2026-06-20: Linter Registry Validator

## Context

Rule sources in the camerata corpus cite real linter rule IDs in their `qualifies`
fields (e.g. `clippy::unwrap_used`, `Ruff E722`, `@typescript-eslint/no-explicit-any`).
The grounding pass that maps each rule to an authoritative source needs to answer:
**does this rule ID actually exist in the named tool?**

Previously there was no mechanical check for this. A human reviewer doing a
grounding-verification pass had to look up every cited ID manually, creating a
bottleneck and leaving room for hallucinated or stale rule IDs to slip through.

## Decision

Create `crates/linter-registry` — a new crate that ships a curated static
registry of canonical linter rule IDs and exposes:

1. **`validate_citation(tool, rule_id) -> CitationStatus`** — the primary API.
   Returns one of `Resolves | NotFound | UnknownTool`.

2. **`generate_report(corpus_dir, output_path, registry)`** — scans all rule
   TOML files under `corpus_dir`, extracts linter citations from each rule's
   `qualifies` field using heuristic pattern matching, validates each citation
   against the registry, and writes a 3-column Markdown report to
   `docs/rule-grounding/citation-validation.md`.

3. **Unit tests** — a small fixture set proving the three status paths (real id
   resolves; fake id is NotFound; unknown tool is UnknownTool) plus per-tool
   coverage tests for every rule ID the corpus actually cites.

## Supported tools

| Tool key | Description | Source |
|---|---|---|
| `clippy` | Rust Clippy lints | https://rust-lang.github.io/rust-clippy/master/ |
| `ruff` | Ruff Python linter (E/W/B/S/BLE) | https://docs.astral.sh/ruff/rules/ |
| `eslint` | ESLint core rules | https://eslint.org/docs/latest/rules/ |
| `typescript-eslint` | @typescript-eslint/ rules | https://typescript-eslint.io/rules/ |
| `react-hooks` | eslint-plugin-react-hooks | https://www.npmjs.com/package/eslint-plugin-react-hooks |
| `golangci-lint` | golangci-lint linter names | https://golangci-lint.run/usage/linters/ |
| `rubocop` | RuboCop cop names (case-insensitive) | https://docs.rubocop.org/rubocop/ |
| `checkstyle` | Checkstyle check names | https://checkstyle.sourceforge.io/checks.html |
| `spotbugs` | SpotBugs bug pattern IDs | https://spotbugs.readthedocs.io/en/latest/bugDescriptions.html |
| `roslyn` | Roslyn CA quality rules | https://learn.microsoft.com/dotnet/fundamentals/code-analysis/quality-rules/ |
| `roslyn-style` | Roslyn IDE style rules | https://learn.microsoft.com/dotnet/fundamentals/code-analysis/style-rules/ |
| `bandit` | Bandit Python security tool | https://bandit.readthedocs.io/en/latest/plugins/ |
| `sqlfluff` | SQLFluff SQL linter rules | https://docs.sqlfluff.com/en/stable/reference/rules.html |

Tool-key aliases are supported (`@typescript-eslint`, `ts-eslint`, `findbugs`,
`ca`, `roslyn-ca`, etc.).

## Coverage policy

The registry is curated, not exhaustive. It covers the rule IDs the corpus
**actually cites** plus a representative sample of each tool's most commonly
enabled rules. Coverage gaps are documented in `crates/linter-registry/src/lib.rs`
under "Coverage gaps (intentional)". Re-run the example binary after grounding
more rules to extend coverage as needed.

## Bandit vs Ruff B/S mapping

The corpus cites both `Bandit B105/B106/B107` and `Ruff S608` for the same
underlying checks. The Bandit tool uses `B`-prefixed IDs; Ruff's port of Bandit
maps them to `S`-prefixed IDs (S105/S106/S107, S608). Both are registered:
`bandit` tool for `B`-prefixed IDs, `ruff` tool for the equivalent `S`-prefixed IDs.

## RuboCop case-insensitivity

RuboCop cop names use PascalCase namespaces (e.g. `Style/FrozenStringLiteralComment`).
The registry normalises all IDs to lowercase before comparison so callers can
pass either form. The test suite exercises all three forms (mixed-case, lowercase,
PascalCase).

## Report baseline (mech/linter-validator, 2026-06-20)

Running `cargo run -p camerata-linter-registry --example generate-report` against
the current corpus on this branch:

- **285** rules scanned
- **22** resolves (citations found and validated)
- **0** not-found (no hallucinated or stale rule IDs detected)
- **263** unsourced (rules with no linter citation in their `qualifies` field)

The high unsourced count is expected: the grounding pass has not run on this
branch yet. The tool will be re-run after grounding integrates to confirm the
not-found count stays at zero.

## Alternatives considered

**Live tool query** — invoke the actual linter to check whether a rule ID exists.
Rejected: adds runtime dependencies on N linters being installed; brittle in CI;
the registry-as-static-data approach is deterministic and fast.

**Pull from tool registries at build time (build.rs)** — fetch and embed the
full rule list at compile time. Rejected: network dependency in build; the
curated approach is more reliable and the corpus coverage surface is bounded.

**Separate crate per tool** — one crate per linter. Rejected: the corpus needs
cross-tool validation in one pass; a monolithic registry is simpler.
