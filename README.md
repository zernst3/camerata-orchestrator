# Camerata Orchestrator

[![CI](https://github.com/zernst3/camerata-orchestrator/actions/workflows/ci.yml/badge.svg)](https://github.com/zernst3/camerata-orchestrator/actions/workflows/ci.yml)

## What Camerata does for an inspection

Camerata inspects a codebase. It reads the code against a library of written, versioned rules and produces a report of findings. Every finding names the rule it was raised under, and rules point to the standard they rest on (a published style guide, a security reference or a vendor's own documentation), and the few without an outside standard say why. Any rule that can raise a high or critical finding must carry a citation and a fix, and a corpus test enforces that. In the desktop app a person reviews findings in a triage queue (confirm, ignore, false positive or tech debt) before export. The headless `camerata inspect` export carries every finding untriaged and says so: the report is stamped as an unreviewed draft.

Two kinds of check run. A deterministic scan answers the questions that have a provable answer: hardcoded secrets, SQL built by string concatenation, file paths that escape their directory, plus the project's own linters. An AI pass handles the judgment calls, such as a write path with no authorization check or a service that bypasses the repository layer. No AI-raised candidate becomes a finding until it has passed severity calibration and de-duplication, and anything the engine is unsure of is held for a person to decide.

The rules are plain TOML files in [`crates/rules/principles/`](crates/rules/principles/), so you can read exactly what an inspection checks. The library holds 440 rules across 22 domains, each one versioned and locked by content hash so no rule can change silently. [`docs/RULES_REFERENCE.md`](docs/RULES_REFERENCE.md) documents the format and [`docs/RULE_AUTHORING.md`](docs/RULE_AUTHORING.md) the authoring workflow.

- **Generate a sample report:** the real report pipeline builds the PDF, the workbook and `findings.json` from a hand-built fixture (not a scan of a real codebase). `cargo test -p camerata-server --test generate_sample_report -- --ignored` writes them to `sample-report/` and needs Typst 0.15 on `PATH`.
- **Data handling:** [`DATA_HANDLING.md`](DATA_HANDLING.md) covers what leaves your machine during an inspection and how to delete what stays.

**The honest boundary.** What this repository can show you is the engine's behavior on fixture repositories and the test suite that pins it. It does not claim, and the repo holds no published measurement of, how precise or complete the AI pass is on a large real-world codebase, so treat results at that scale as unproven until you have run it on your own code and reviewed the findings yourself.

Camerata also has a governed agent-development platform built on the same rules. That material, including what in it is still staged, is in [`PLATFORM.md`](PLATFORM.md).

## How an inspection runs

Point Camerata at one or more existing repositories (in the desktop app, or one checkout at a time with `camerata inspect`):

- **Per-repo stack detection** (languages and frameworks) and a **per-repo proposed ruleset** drawn from a corpus of **440 rules across 22 domains** covering many language and framework stacks (Rust/Axum/SeaORM/Dioxus/sqlx/Tokio, Python/Django/Flask/FastAPI, Go/GORM/gRPC, JS/TS/React/Vue/Angular/Next/Nest, Java/Spring, C#/ASP.NET/EF Core, Ruby/Rails, SQL, Supabase, fullstack, plus the always-on security floor, the process rules and the agentic governance rules). Each repo is scanned against its own selected rules plus an always-on deterministic security floor. The domain-to-stack mapping is declarative data with a computable union, and a build gate (`req_h1_every_corpus_domain_is_emittable_by_some_stack`) fails the build if a corpus folder exists that no stack could ever select from, so a rule cannot sit in the corpus permanently unreachable.
- **A two-tier audit:** a deterministic mechanical scan (hardcoded secrets, raw-SQL concatenation, path escapes, plus clippy/ruff/eslint/semgrep/osv preview) and an AI architectural audit (missing auth on a write path, a service bypassing the repository layer, N+1, cross-boundary imports and the like), with severity/authority calibration, cross-rule dedup and snippet-to-line resolution.
- **Alternative recommendation, built into that same audit:** for every rule that carries more than one adopted alternative, the audit also picks the option it judges best fitting for this codebase (with reasoning you can inspect) and assesses the codebase against that option, not against every option at once. There is one hard exception, and it is load-bearing: if the option the repo actually follows is the one the rule itself names as its own degenerate or absent form (`violates = true` on that option), the rule's directive is **not** rewritten to match, and a finding is emitted instead. Without that guard, a codebase that is consistently incorrect reads as compliant, which is the single worst failure this engine has had. A rule with no default no longer blocks you from proceeding, it is just a hint that the audit will pick for you. Disagree with any recommendation and you can override it and rescan just the rules you changed, then accept the final selections into the ruleset.

In the desktop app the findings then go to triage, and onboarding can turn the triaged result into governance files for the repository; that half is described in [`PLATFORM.md`](PLATFORM.md#onboarding-from-inspection-to-governance). Walked screen by screen in [`docs/USER_GUIDE.md`](docs/USER_GUIDE.md); mechanics in [`docs/TECHNICAL.md`](docs/TECHNICAL.md).

### What a finding IS: five kinds, not a yes/no

A finding is not a yes/no. Every exported row carries an explicit `kind`, and the five kinds
are different statements about the codebase, not five severities:

| Kind | What it says | Ships at |
|---|---|---|
| `defect` | An objective fault at a site. | Its calibrated severity, bounded by the rule's authored floor and ceiling. |
| `deviation` | A consistency violation against the convention this codebase itself predominantly follows: "2 of your 47 assessed endpoints do not use URI versioning." Numerator, denominator and the observed convention are structured fields, never prose to parse. | Capped at `medium` by default, because the cost of inconsistency is forward drift on future features rather than present risk. A rule may author `deviation_ceiling = "high"` where the domain genuinely compounds. |
| `convention_observed` | The codebase consistently follows a convention and there are zero violations. This ships in the product: the inspection derives the decision set, governance later enforces it. | `info`, one row per rule. |
| `no_convention` | No convention has been adopted; the codebase is inconsistent with no clear majority. | `info`, exactly one row per rule, never N. |
| `not_assessed` | The rule was armed but could not be evaluated (tool failed, no files matched, the model declined). An honesty row, structurally distinguishable from "assessed and clean". | `info`. |

A row whose producer never assigned a kind exports as the explicit tag `unspecified`. It is
deliberately **never** rendered as `defect`: an unmapped emission path has to be visible as a gap
rather than silently counted as an accusation. The taxonomy is defined once, in
`crates/api-types/src/audit.rs`, and the same discriminator reaches the PDF, the workbook and
`findings.json`. Mechanics in [`docs/TECHNICAL.md`](docs/TECHNICAL.md) §6a.

## Run an inspection headless

`camerata inspect` runs the whole inspection pipeline in-process with no server and no UI, and
writes the same product-export ZIP the cockpit's export button produces, with no triage decisions applied. This costs real model
tokens, so the two commands below are a pair: run the gate before you pay for anything.

```
cargo run -p camerata -- inspect --repo <PATH> --export <OUT.zip>
                                            # the ZIP holds: the PDF summary, findings.xlsx,
                                            # findings.json, README.txt. Also writes
                                            # <repo>/.camerata-scan-ledger.json.
cargo run -p camerata -- integrity-gate --ledger <PATH>/.camerata-scan-ledger.json
                                            # exit 0 = every required engine ran, every skip was
                                            # justified, every pipeline stage reconciles
                                            # exit 1 = the scan is degraded; do not grade it
                                            # exit 2 = the GATE could not run (ledger missing or
                                            #          unreadable) - deliberately NOT a pass
./scripts/self-measurement.sh               # the in-repo self-measurement gate: seconds,
                                            # deterministic, no network, no model calls
```

The workbook is partitioned by finding kind rather than being one flat sheet: **Index**,
**All Findings** (defects, deviations and any unclassified row), **Deviations** (one row per rule,
at the grain the deviation numbers are actually computed), **Conventions** (what this codebase
already does well and consistently), the per-area sheets, **Dependencies**, **False Positives**
and **Coverage** (what was checked, what could not be and which rules did not run). The PDF is the
summary and deliberately does not carry every row; `findings.json` carries the whole non-false-positive
row set, every kind included. Exit code 2 on the gate is the point of the design: a wrapper that read
"the gate could not run" as "the gate passed" would be the same class of bug as a scan that reads
"the tool did not run" as "the rule is clean".

## By the numbers

I architected Camerata and orchestrated its build: supervised AI agents wrote the code, working under the same deterministic governance rules Camerata itself enforces. Every architectural decision, review gate and scope call is mine. In roughly four and a half months:

| | |
|---|---|
| Rust | about 475,000 lines across 29 crates and one maintainer tool, including tests |
| Tests | 8,500+ automated tests; test code is roughly 45% of all code |
| Test quality | mutation testing on the checks crate: the suite catches 97.9% of deliberately injected faults |
| Rule library | 440 rules across 22 domains, each one versioned and locked by content hash so no rule can change silently |
| Commits | 1,900+, and every push to the main branches runs formatting, zero-warning lints and the full test suite in CI |

What I am proudest of is the discipline behind those numbers, not the numbers themselves:

- **False negative, never false positive.** A finding a client cannot verify is worse than a missed one. Every deterministic detector ships with a positive test and a "safe twin" test that proves it stays quiet on the look-alike case.
- **Deterministic first, AI second.** Wherever a question has a provable answer, code answers it. AI review covers the judgment calls, and no AI-raised candidate becomes a finding without passing through calibration, deduplication and per-rule severity bounds; anything the engine is unsure of is held for human review rather than shipped.
- **Nothing sensitive leaves the building.** Client secrets are redacted the moment a finding is created and before any code reaches a model, and a release gate scans every string of every file in a deliverable before it exists. If one secret survives, the export fails.

## Read in this order

1. [`docs/OVERVIEW.md`](docs/OVERVIEW.md): the high-level map of the whole system (crates, the inspection pipeline, the core concepts) in one sitting.
2. [`docs/USER_GUIDE.md`](docs/USER_GUIDE.md): the flows and features, how to use them.
3. [`docs/TECHNICAL.md`](docs/TECHNICAL.md): how it works under the hood (the gate, the scan pipeline, agent grounding, persistence).
4. [`PLATFORM.md`](PLATFORM.md): governed development, the governance gate, the architecture and what is still staged.
5. [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md): the all-Rust stack, top to bottom.
6. [`docs/decisions/`](docs/decisions/): the design-decision records.
7. [`docs/VISION.md`](docs/VISION.md): the north star and where the architecture leads.

## Family

- camerata-ai: the conventions engine the corpus format originates from. The corpus is vendored into this repo at `crates/rules/principles/` (440 TOML rules as of 2026-10-28), so the workspace is self-contained; override with `CAMERATA_CORPUS_PATH`. Field-by-field reference for the rule schema: [`docs/RULES_REFERENCE.md`](docs/RULES_REFERENCE.md); the authoring workflow is [`docs/RULE_AUTHORING.md`](docs/RULE_AUTHORING.md).
- rust-chorale: the headless, virtualized Dioxus / Leptos table library used for tabular surfaces.
- this repo: the conductor that leads the ensemble.

## License

Source-available under the [PolyForm Noncommercial License 1.0.0](LICENSE). The code is fully readable, and noncommercial use (study, evaluation, personal and research projects) is permitted. Commercial and competing use is reserved by the copyright holder, Zachary Ernst. This is a deliberate choice over a permissive license: a license can be loosened later but never tightened, so it starts reserved.
