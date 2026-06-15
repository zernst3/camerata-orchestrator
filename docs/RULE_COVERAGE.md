# Rule coverage: beyond Rust

> The gate is only as good as the rules it enforces. The corpus is robust for Rust and
> thin for everything else, so corpus coverage across languages and frameworks is a
> first-class product axis, not an afterthought.

## The unit of language support is the CheckRunner, not the rule

The instinct is to count rules: "Rust has N, JS has few, author the rest." That mis-prices
the work. Authoring a rule is cheap — it's prose plus an ID. A rule only *governs* anything
once a **deterministic Layer-2 `CheckRunner`** can mechanically decide pass/fail for it in
that language. So a language is "supported" when it has a CheckRunner that actually enforces
its rules, not when the rules are written down. **Roadmap milestones are "shipped a
deterministic CheckRunner for language X," not "authored X's rules."** A drawer full of JS
rules with no JS checker is convention with extra steps — the exact thing the project argues
against.

## Two tiers of rule, with very different porting costs

- **Commodity / carry-over rules (cheap).** The textual and security rules port almost for
  free because they're language-agnostic and a generic checker (regex / secret-scanner /
  simple lint) already decides them: no hardcoded secrets, no secrets in URLs, no raw SQL by
  string concatenation, audit-field presence, etc. These are real value but they are the
  *commodity* space — every linter ships them.
- **Differentiated / architectural rules (expensive — the moat).** The layering and
  boundary rules are the differentiator: "`db.*` only inside `repositories/`," "UI-gated
  action maps to a guarded endpoint," component/module boundaries, state-management
  discipline. These are **NOT language-agnostic to enforce.** Deciding them requires
  language-aware analysis — an AST / parser / type-resolver for each target language. The
  Rust versions lean on Rust's structure; the JS/TS versions need an eslint AST rule, the
  Python versions an `ast`-module check, and so on. **Porting the moat costs more than "most
  rules carry over" implies**, because the part that carries over is the commodity part and
  the part that defines the product does not.

## Sequencing rule, by checker availability, not popularity

Order new-language work by *"where can I get a real, deterministic checker for the
**architectural** rules,"* not by language popularity. A language with a mature AST-lint
ecosystem (JS/TS via eslint custom rules) lets the moat travel; a popular language where the
architectural checks would mean hand-rolling a parser is a worse early bet even if more users
ask for it. The architectural-checker availability is the gating constraint on the whole
language.

## The task

1. Populate **JavaScript / TypeScript** rules in the corpus — but treat shipping the JS/TS
   `CheckRunner` (eslint + tsc + prettier, including **custom eslint AST rules for the
   architectural boundaries**) as the actual milestone. Universal Rust-parity rules and the
   JS/TS-specific ones (typing discipline, async/promise handling, no `any`) are the input;
   the deterministic checker is the deliverable.
2. Author **framework** rules: React, Redux, Express — component boundaries,
   state-management discipline, middleware/error-handling conventions. These are
   architectural (moat) rules, so each needs an AST-level eslint check, not just prose.
3. For each rule, record its **tier** (commodity carry-over vs differentiated architectural),
   its **enforcement scope** (language-agnostic vs language-specific), and **which Layer-2
   toolchain** enforces it (eslint / tsc / prettier / custom AST rule), so the gate knows how
   to check it per stack and so the moat-porting cost is visible, not hidden.

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
