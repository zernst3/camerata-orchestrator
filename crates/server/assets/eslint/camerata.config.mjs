/**
 * Camerata bundled ESLint flat config — offline, zero-network.
 *
 * This config is bundled inside the Camerata binary assets so the scan-time
 * preview pass can run eslint with a coherent rule set without touching the
 * repo's own eslint config or the npm registry.
 *
 * It is intentionally minimal: only the rules that appear in the Camerata
 * corpus with an `eslint:` or `@typescript-eslint:` linter source are listed
 * here.  The scan-time pass overrides individual rules via `--rule` anyway, so
 * this config is primarily a stable base that prevents eslint from erroring out
 * on "no config found".
 *
 * For TypeScript targets the TypeScript parser is referenced by the local
 * node_modules path that Camerata provisions.  If the parser is absent, eslint
 * falls back to the default (JS-only) parser and the TS-specific rules are
 * silently ignored — graceful degradation, not a hard failure.
 */

// eslint-disable-next-line no-undef
const tsParserPath = new URL("../node_modules/@typescript-eslint/parser/dist/index.js", import.meta.url).pathname;

let tsParser;
try {
  // Dynamic import so the config is valid even when the TS parser is absent.
  const mod = await import(tsParserPath);
  tsParser = mod.default ?? mod;
} catch {
  tsParser = undefined;
}

/** @type {import('eslint').Linter.FlatConfig[]} */
const config = [
  {
    // Apply to all JS/TS source files; exclude typical non-source paths.
    files: ["**/*.{js,mjs,cjs,jsx,ts,tsx,mts,cts}"],
    ignores: [
      "node_modules/**",
      "**/node_modules/**",
      "dist/**",
      "build/**",
      ".next/**",
      "coverage/**",
    ],
    ...(tsParser ? { languageOptions: { parser: tsParser } } : {}),
    rules: {
      // ── Security baseline ───────────────────────────────────────────────────
      // These rules correspond to the corpus entries that map to the eslint
      // linter source.  They are set to "warn" here so the base pass is
      // non-blocking; the scan-time `--rule` override sets them to "error".

      // Disallow == / != (prefer === / !==)
      eqeqeq: ["warn", "always"],
      // Disallow eval()
      "no-eval": "warn",
      // Disallow implied eval (setTimeout("code", …))
      "no-implied-eval": "warn",
      // Disallow var (prefer const/let)
      "no-var": "warn",
      // Prefer const where let is not reassigned
      "prefer-const": "warn",
      // Disallow console (surfaces in production code reviews)
      "no-console": "warn",
      // No unused variables
      "no-unused-vars": ["warn", { argsIgnorePattern: "^_" }],
      // Require error objects to be thrown, not strings
      "no-throw-literal": "warn",
      // Disallow dangling commas in ES3 targets (off for modern JS)
      // "comma-dangle": "off",
      // Disallow prototype builtins called directly (e.g. obj.hasOwnProperty)
      "no-prototype-builtins": "warn",
      // Disallow assignment in conditions (a common logic bug)
      "no-cond-assign": ["warn", "always"],
      // Disallow duplicate case labels in switch
      "no-duplicate-case": "error",
      // Disallow empty block statements without a comment
      "no-empty": ["warn", { allowEmptyCatch: true }],
      // Require error handling in callbacks (node style)
      // "handle-callback-err": "warn",  // node-specific, off by default
    },
  },
];

export default config;
