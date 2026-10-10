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
 * its own.  (That `--rule` CAN see a plugin registered in a DIFFERENT config
 * object of this same array is verified behaviour, not an assumption: eslint
 * turns `--rule` into its own `files`-less config object and the flat-config
 * merge unions the plugin namespaces per file.  The failure mode this file's
 * own history exhibited — "could not find plugin '@typescript-eslint'" — was
 * always an ABSENT PACKAGE, never a merge-visibility problem.)
 *
 * Every plugin AND the TypeScript parser below is loaded by BARE SPECIFIER
 * inside its own try/catch.  Two reasons, both load-bearing:
 *
 *  1. A partial or offline `npm install` (see `tool_provisioning::ensure_eslint`)
 *     must degrade that ONE plugin's rules gracefully rather than taking down
 *     every OTHER rule's preview for the whole scan.
 *
 *  2. Bare specifiers are resolved by node walking up from THIS FILE's own
 *     directory, which is the provisioned workspace root that owns
 *     `node_modules/`.  The previous revision hand-built a path instead —
 *     `new URL("../node_modules/@typescript-eslint/parser/dist/index.js",
 *     import.meta.url).pathname` — which was wrong twice over: `../` from a
 *     file AT the workspace root resolves to the workspace's PARENT (the
 *     `tooling/` dir, which has no `node_modules`), and `.pathname`
 *     percent-encodes the space in macOS's `Application Support`, so the
 *     resulting path could not open even at the right level.  The TypeScript
 *     parser therefore never loaded on any platform, and every `.ts`/`.tsx`
 *     file was silently parsed by the JS-only default parser.  A bare
 *     specifier cannot express either mistake.
 */

// The TypeScript parser, so `.ts`/`.tsx`/`.mts`/`.cts` sources parse at all.
// Absent parser => eslint's default JS-only parser => every TS file reports a
// fatal parse error instead of real findings. See note 2 above.
let tsParser;
try {
  const mod = await import("@typescript-eslint/parser");
  tsParser = mod.default ?? mod;
} catch {
  tsParser = undefined;
}

// Plugin packages, each independently optional — see note 1 above.

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
      // this run, surfaced as a graceful CoverageNote, never a crash. The Rust
      // side performs the SAME presence check before it hands a namespaced
      // rule to `--rule` (see `scan_tools::eslint_plugin_package_for_selector`),
      // because an unregistered namespace reaching `--rule` is fatal to the
      // WHOLE run, not just to that rule.
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
