# Rule coverage: beyond Rust

> The gate is only as good as the rules it enforces. The corpus is robust for Rust and
> thin for everything else, so corpus coverage across languages and frameworks is a
> first-class product axis, not an afterthought.

## The gap

- Rust has thorough, robust rules. JavaScript / TypeScript and other languages are
  lacking.
- Most API / UI-layer rules are **language-agnostic** and already apply across languages
  (e.g. no hardcoded secrets, no raw SQL built by string concatenation, no secrets in
  URLs, layering boundaries, every UI-gated action maps to a guarded endpoint).
- Some rules are **language-specific** and need per-language authoring.

## The task

1. Populate **JavaScript / TypeScript** rules in the corpus, at least to parity with the
   universal Rust ones, plus the JS/TS-specific ones (typing discipline, async/promise
   handling, module boundaries, no `any`, etc.).
2. Author **framework** rules: React, Redux, Express (and others as needed) — component
   boundaries, state-management discipline, middleware/error-handling conventions.
3. For each rule, record its **enforcement scope**: language-agnostic vs
   language-specific, and **which Layer-2 toolchain** enforces it (eslint / tsc /
   prettier / ...), so the gate knows how to check it per stack.

## Where rules live

The corpus is the sibling [`camerata-ai`](../../camerata-ai) repo (the rule corpus +
conventions engine). New language / framework rules are authored there; the gateway's
enforced arms (Layer 1) and the `CheckRunner` implementations (Layer 2) consume them.

## Dependency on Layer 2

Adding JS/TS rules implies adding JS/TS **Layer-2 toolchains** (eslint / prettier / tsc)
as `CheckRunner` implementations, so a JS/TS rule can actually be enforced mechanically
and not just documented. See [`HARDENING.md`](HARDENING.md) item 4.

## Why this matters for the thesis

"Mechanical beats convention" only holds where a mechanical check exists. A
language/framework with a thin corpus and no Layer-2 toolchain falls back to convention,
which is exactly what the project argues against. Closing the coverage gap is how the
thesis stays true off the Rust happy path.
