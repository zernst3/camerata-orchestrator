# Mechanical-enforcement linter triage

Date: 2026-06-21

## Problem

The `mechanical` enforcement tier is supposed to mean exactly one thing: a
**real, off-the-shelf linter rule** (a tool with a citable rule-id) enforces
the commitment and fails the build on violation. A scan found 14 rules marked
`enforcement = "mechanical"` whose `[[sources]]` carried **no `linter = "..."`
line** (or whose cited linter did not resolve). A mechanical rule with no real
linter behind it is a false promise: it claims tool-level enforcement that does
not exist.

## Invariant

`enforcement = "mechanical"` MUST map to a real off-the-shelf linter rule cited
as `linter = "tool: rule-id"`. If it cannot, the rule is reclassified into the
honest tier:

- **architectural** — deterministically checkable, but no off-the-shelf rule
  exists; needs a bespoke/AST/custom static-analysis CI gate.
- **structured** — a process / pipeline-topology commitment, codified and
  enforced by the project's own orchestration/CI config, not a third-party
  linter.
- **prose** — judgment, reviewed by humans.

## Resolution

Re-scan confirmed exactly the 14 rules in the known starting set; no others.

| Rule id | Old enforcement | Resolution | Linter cited / reason for reclassification |
|---|---|---|---|
| UI-IMAGE-COMPONENT-1 | mechanical | linter added (kept mechanical) | `eslint-plugin-next: @next/next/no-img-element` — real off-the-shelf rule bans raw `<img>`. (The project-specific "ban next/image too, force a wrapper" layer is noted in `qualifies` as a config extension, not the off-the-shelf rule.) |
| CSHARP-NULLABLE-REFERENCE-TYPES-1 | mechanical | linter added (kept mechanical) | `Roslyn (csc): CS8600/CS8602/CS8618 nullable warnings + <Nullable>enable</Nullable> + TreatWarningsAsErrors` — enforced by the C# compiler. |
| CSHARP-NO-HARDCODED-SECRETS-1 | mechanical | linter added (kept mechanical) | `SonarC#: csharpsquid:S2068` ("Credentials should not be hard-coded"); gitleaks is an equivalent off-the-shelf gate. |
| ARCH-STRICT-LAYERING-1 | mechanical | -> architectural | Boundary is "DB **calls** (db.select/insert) forbidden outside the repository layer" — a call-site-and-layer predicate. `import/no-restricted-paths` blocks imports between dirs, not the DB-client call, so it does not cover this off the shelf. Needs a bespoke `no-restricted-syntax`/AST gate. |
| ARCH-API-DTOS-1 | mechanical | -> architectural | "Controllers must not import domain types" — which modules are "domain" vs "controller" is project-specific; generic rule-ids (no-restricted-imports, import/no-restricted-paths, dependency-cruiser) carry no DTO/domain semantics until zones are wired in. Custom configured check. |
| ARCH-EXACT-DECIMALS-1 | mechanical | -> architectural | Deciding which type/field/column "requires exact arithmetic" is domain knowledge no generic linter carries; needs a custom float-ban gate over an annotated surface + a precision property test. |
| ARCH-STRUCTURED-ERRORS-1 | mechanical | -> architectural | Validating every error body against the project's envelope schema needs a custom contract test; no generic linter knows the envelope shape. |
| ARCH-SERVER-AUTHZ-1 | mechanical | -> architectural | Blocking client-side permission introspection needs a `no-restricted-imports`/`no-restricted-syntax` rule wired to the project's specific permission-helper names; the generic rule-id carries no permission semantics until configured. Bespoke check. |
| UI-UTC-DATES-1 | mechanical | -> architectural | `no-restricted-syntax` is a generic AST-matcher with no built-in rule-id for "ban implicit-timezone date formatting"; the patterns and the helper exemption are hand-written per project. Bespoke AST gate. |
| PYTHON-TESTING-FILE-NAMING-1 | mechanical | -> architectural | pytest discovery **silently skips** misnamed files rather than failing the build, and flake8-pytest-style (Ruff `PT`) has no file-naming rule (astral-sh/ruff#8145, #8796). A hard gate needs a custom glob/collection-count check. |
| ORCH-CONFORMANCE-1 | mechanical | -> structured | Process commitment about the change pipeline; enforced by the orchestration system (Camerata's own governance gate + waiver log), not a third-party linter rule-id. |
| ORCH-NEW-PATH-TESTS-1 | mechanical | -> structured | Per-diff patch-coverage / mutation gate; linters check source shape, not coverage-of-a-diff. Enforced by the change pipeline + coverage tooling. |
| ORCH-PREREVIEW-1 | mechanical | -> structured | Pipeline-topology commitment (AI reviewer gating the human queue); enforced by orchestration + branch-protection config, no third-party linter rule-id. |
| ORCH-ENV-GATED-QUALITY-1 | mechanical | -> structured | Deployment-pipeline-topology commitment (pre-merge bar + automated promotion gate); enforced by CI/CD config, no third-party linter rule-id. |

## Counts

- kept mechanical, real linter added: **3** (UI-IMAGE-COMPONENT-1, CSHARP-NULLABLE-REFERENCE-TYPES-1, CSHARP-NO-HARDCODED-SECRETS-1)
- -> architectural (deterministic, needs custom checker): **7** (ARCH-STRICT-LAYERING-1, ARCH-API-DTOS-1, ARCH-EXACT-DECIMALS-1, ARCH-STRUCTURED-ERRORS-1, ARCH-SERVER-AUTHZ-1, UI-UTC-DATES-1, PYTHON-TESTING-FILE-NAMING-1)
- -> structured (process / pipeline, not linter-checkable): **4** (ORCH-CONFORMANCE-1, ORCH-NEW-PATH-TESTS-1, ORCH-PREREVIEW-1, ORCH-ENV-GATED-QUALITY-1)

## Honesty note

No linter rule-id was invented. Each kept-mechanical citation was verified to
exist (`@next/next/no-img-element`, Roslyn CS86xx nullable diagnostics,
`csharpsquid:S2068`). Where existence was unconfirmed or the rule did not
honestly cover the commitment's semantics, the rule was reclassified rather
than cited.

## Verification

`cargo test -p camerata-rules` — green (54 unit tests + 1 doctest).
