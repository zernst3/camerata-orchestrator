# JS-Core Rule Grounding Report

**Date:** 2026-06-20
**Family:** js-core
**Scope:** `crates/rules/principles/javascript/*.toml` + `javascript/{react,redux,typescript}/`

## Summary

- Grounded: 11 / 11 rules
- Ungrounded: 0
- Demoted (mechanical → prose): 2

## Demoted Rules

| Rule ID | Reason |
|---------|--------|
| `JAVASCRIPT-REDUX-SERIALIZABLE-STATE-1` | Enforcer is Redux Toolkit's runtime serializableCheck middleware, not a static ESLint/linter rule. No ESLint rule enforces serializable state at lint time. Demoted mechanical → prose; runtime source documented. |
| `JAVASCRIPT-TYPESCRIPT-STRICT-MODE-1` | Enforcer is the TypeScript compiler itself (`tsconfig strict: true` + `tsc --noEmit`). No ESLint/typescript-eslint rule enforces the presence of `"strict": true` in tsconfig. Demoted mechanical → prose; TypeScript compiler docs sourced. |

## Rule Grounding Table

| Rule ID | Verification | Source URL | Linter Rule | Status |
|---------|-------------|------------|-------------|--------|
| `JAVASCRIPT-CONST-DEFAULT-1` | grounded | https://eslint.org/docs/latest/rules/prefer-const | `eslint: prefer-const` | grounded |
| `JAVASCRIPT-NO-VAR-1` | grounded | https://eslint.org/docs/latest/rules/no-var | `eslint: no-var` | grounded |
| `JAVASCRIPT-STRICT-EQUALITY-1` | grounded | https://eslint.org/docs/latest/rules/eqeqeq | `eslint: eqeqeq` | grounded |
| `JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1` | grounded | https://react.dev/reference/eslint-plugin-react-hooks/lints/exhaustive-deps | `react-hooks: exhaustive-deps` | grounded |
| `JAVASCRIPT-REACT-FUNCTION-COMPONENTS-1` | grounded | https://react.dev/reference/react/Component | (none — enforcement = structured) | grounded |
| `JAVASCRIPT-REACT-KEYED-LISTS-1` | grounded | https://react.dev/learn/rendering-lists#keeping-list-items-in-order-with-key | `react: no-array-index-key` | grounded |
| `JAVASCRIPT-REACT-RULES-OF-HOOKS-1` | grounded | https://react.dev/reference/eslint-plugin-react-hooks/lints/rules-of-hooks | `react-hooks: rules-of-hooks` | grounded |
| `JAVASCRIPT-REDUX-NO-EFFECTS-IN-REDUCERS-1` | grounded | https://redux.js.org/style-guide/#reducers-must-not-have-side-effects | (none — enforcement = structured) | grounded |
| `JAVASCRIPT-REDUX-NO-STATE-MUTATION-1` | grounded | https://redux.js.org/style-guide/#do-not-mutate-state | (none — enforcement = structured) | grounded |
| `JAVASCRIPT-REDUX-SERIALIZABLE-STATE-1` | grounded | https://redux.js.org/style-guide/#do-not-put-non-serializable-values-in-state-or-actions | (none — runtime RTK middleware, not a linter) | demoted |
| `JAVASCRIPT-REDUX-TOOLKIT-DEFAULT-1` | grounded | https://redux.js.org/style-guide/#use-redux-toolkit-for-writing-redux-logic | (none — enforcement = structured) | grounded |
| `JAVASCRIPT-TYPESCRIPT-NO-EXPLICIT-ANY-1` | grounded | https://typescript-eslint.io/rules/no-explicit-any | `@typescript-eslint: no-explicit-any` | grounded |
| `JAVASCRIPT-TYPESCRIPT-NO-FLOATING-PROMISES-1` | grounded | https://typescript-eslint.io/rules/no-floating-promises | `@typescript-eslint: no-floating-promises` | grounded |
| `JAVASCRIPT-TYPESCRIPT-NO-NON-NULL-ASSERTION-1` | grounded | https://typescript-eslint.io/rules/no-non-null-assertion | `@typescript-eslint: no-non-null-assertion` | grounded |
| `JAVASCRIPT-TYPESCRIPT-STRICT-MODE-1` | grounded | https://www.typescriptlang.org/tsconfig#strict | (none — TypeScript compiler config, not a linter rule) | demoted |

## Authorities Consulted

- **ESLint core rules:** https://eslint.org/docs/latest/rules/
- **typescript-eslint rules:** https://typescript-eslint.io/rules/
- **React official docs + eslint-plugin-react-hooks:** https://react.dev/reference/eslint-plugin-react-hooks/
- **React rendering docs:** https://react.dev/learn/rendering-lists
- **React Component reference:** https://react.dev/reference/react/Component
- **eslint-plugin-react (jsx-eslint):** https://github.com/jsx-eslint/eslint-plugin-react
- **Redux Style Guide:** https://redux.js.org/style-guide/
- **Redux Toolkit Serializability Middleware:** https://redux-toolkit.js.org/api/serializabilityMiddleware
- **TypeScript tsconfig reference:** https://www.typescriptlang.org/tsconfig

## Demotion Rationale

### JAVASCRIPT-REDUX-SERIALIZABLE-STATE-1
The original `enforcement = "mechanical"` claim asserted that Redux Toolkit's `serializableCheck` middleware acts as a lint gate. While RTK's `configureStore` does include this middleware by default, it is a **runtime** development check (logs a console error) rather than a static analysis / linter rule. No ESLint rule exists that enforces serializable-only state at the source-code level. Demoted to `prose` and the qualifies field updated to reflect this accurately.

### JAVASCRIPT-TYPESCRIPT-STRICT-MODE-1
The original `enforcement = "mechanical"` claim asserted that `tsconfig "strict": true` + `tsc --noEmit` constitutes a linter rule. The TypeScript compiler is a type-checker, not an ESLint plugin, and no ESLint/typescript-eslint rule enforces that `strict` must be enabled in tsconfig. The enforcement is via the compiler itself as a build/CI gate. Demoted to `prose` with the TypeScript tsconfig reference as the authoritative source.
