/**
 * Camerata bundled ESLint flat config — offline, zero-network.
 *
 * This config is bundled inside the Camerata binary assets so the scan-time
 * preview pass can run eslint with a coherent rule set without touching the
 * repo's own eslint config or the npm registry.
 *
 * It is intentionally minimal: only the rules that appear in the Camerata
 * corpus with an `eslint:`, `@typescript-eslint:`, `react-hooks:`, or `jest:`
 * linter source are listed here.  The scan-time pass overrides individual
 * rules via `--rule` anyway, so this config is primarily a stable base that
 * prevents eslint from erroring out on "no config found" AND, for any rule
 * backed by a PLUGIN (as opposed to an eslint core rule), registers that
 * plugin under the namespace its rules are addressed by — `--rule` can only
 * resolve `"react-hooks/exhaustive-deps": "error"` if something has already
 * registered the `react-hooks` plugin here; it does not install plugins on
 * its own.
 *
 * Every plugin import below is wrapped in its own try/catch and dynamically
 * imported, exactly like the TypeScript parser below: a partial or offline
 * `npm install` (see `tool_provisioning::ensure_eslint`) must degrade that ONE
 * plugin's rules gracefully (silently unavailable, `--rule` warns and no-ops
 * rather than erroring the whole config out) rather than taking down every
 * OTHER rule's preview for the whole scan.
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

// Plugin packages, dynamically imported by bare specifier (resolved via this
// file's own node_modules, provisioned by `tool_provisioning::ensure_eslint`).
// Each is independently optional — see the module doc comment above.

let tsEslintPlugin;
try {
  const mod = await import("@typescript-eslint/eslint-plugin");
  tsEslintPlugin = mod.default ?? mod;
} catch {
  tsEslintPlugin = undefined;
}

let reactHooksPlugin;
try {
  const mod = await import("eslint-plugin-react-hooks");
  reactHooksPlugin = mod.default ?? mod;
} catch {
  reactHooksPlugin = undefined;
}

let jestPlugin;
try {
  const mod = await import("eslint-plugin-jest");
  jestPlugin = mod.default ?? mod;
} catch {
  jestPlugin = undefined;
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
    plugins: {
      // Only registered when the corresponding package actually provisioned —
      // an absent plugin here means its rules are simply never previewable
      // this run, surfaced as a graceful CoverageNote, never a crash.
      ...(tsEslintPlugin ? { "@typescript-eslint": tsEslintPlugin } : {}),
      ...(reactHooksPlugin ? { "react-hooks": reactHooksPlugin } : {}),
      ...(jestPlugin ? { jest: jestPlugin } : {}),
    },
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
