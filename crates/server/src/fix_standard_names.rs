//! Names a fix may use without them being in a finding's evidence (defect D3): the standard
//! JavaScript, Web and Node API, and the rule names of the ESLint rules and plugins a scan
//! commonly cites.
//!
//! The fix self-check refuses a name found nowhere in the finding's evidence or the repository,
//! to stop the model inventing a helper or a column. A name that belongs to the language or the
//! platform (`Number.isSafeInteger`, `crypto.randomUUID`) or to a linter (`react-hooks/rules-of-hooks`)
//! exists in no repository and in every one, so asking for it rejected sound fixes. These are
//! EXACT lists, not prefixes: `Number.toCents` and `react-hooks/hook-order-checker` are
//! inventions and stay refused.

use std::collections::HashSet;
use std::sync::OnceLock;

/// `(namespace, members)`: `namespace.member` is a standard name. A namespace written with a
/// further dot (`crypto.subtle`) lists the members of that nested object.
const STANDARD_MEMBERS: &[(&str, &[&str])] = &[
    (
        "Number",
        &[
            "isSafeInteger",
            "isInteger",
            "isFinite",
            "isNaN",
            "parseInt",
            "parseFloat",
            "MAX_SAFE_INTEGER",
            "MIN_SAFE_INTEGER",
            "MAX_VALUE",
            "MIN_VALUE",
            "EPSILON",
        ],
    ),
    (
        "Math",
        &[
            "floor", "ceil", "round", "trunc", "max", "min", "abs", "random", "pow", "sqrt",
            "sign", "log", "exp",
        ],
    ),
    ("JSON", &["parse", "stringify"]),
    (
        "Object",
        &[
            "keys",
            "values",
            "entries",
            "assign",
            "freeze",
            "fromEntries",
            "hasOwn",
            "create",
            "defineProperty",
            "getPrototypeOf",
            "getOwnPropertyNames",
            "isFrozen",
        ],
    ),
    ("Array", &["isArray", "from", "of"]),
    ("String", &["raw", "fromCharCode", "fromCodePoint"]),
    (
        "Promise",
        &["all", "allSettled", "race", "any", "resolve", "reject"],
    ),
    ("Date", &["now", "parse", "UTC"]),
    ("Intl", &["NumberFormat", "DateTimeFormat", "Collator"]),
    ("Symbol", &["for", "iterator", "asyncIterator"]),
    ("BigInt", &["asUintN", "asIntN"]),
    ("Error", &["captureStackTrace"]),
    ("Uint8Array", &["from", "of"]),
    (
        "Buffer",
        &["from", "alloc", "concat", "isBuffer", "byteLength"],
    ),
    (
        "crypto",
        &[
            "randomUUID",
            "getRandomValues",
            "subtle",
            "timingSafeEqual",
            "randomBytes",
            "randomInt",
            "createHash",
            "createHmac",
            "createCipheriv",
            "createDecipheriv",
            "scryptSync",
            "scrypt",
            "pbkdf2Sync",
            "pbkdf2",
            "webcrypto",
        ],
    ),
    (
        "crypto.subtle",
        &[
            "digest",
            "encrypt",
            "decrypt",
            "sign",
            "verify",
            "importKey",
            "exportKey",
            "generateKey",
            "deriveBits",
            "deriveKey",
        ],
    ),
    (
        "window",
        &[
            "location",
            "localStorage",
            "sessionStorage",
            "addEventListener",
            "removeEventListener",
            "open",
            "fetch",
            "crypto",
            "origin",
            "postMessage",
        ],
    ),
    (
        "document",
        &[
            "cookie",
            "getElementById",
            "querySelector",
            "querySelectorAll",
            "createElement",
            "addEventListener",
            "referrer",
            "body",
        ],
    ),
    (
        "location",
        &[
            "href", "origin", "hostname", "host", "pathname", "search", "hash", "assign",
            "replace", "reload",
        ],
    ),
    (
        "navigator",
        &["userAgent", "sendBeacon", "clipboard", "language"],
    ),
    ("URL", &["createObjectURL", "revokeObjectURL", "canParse"]),
    ("performance", &["now", "mark", "measure"]),
    ("AbortSignal", &["timeout", "abort", "any"]),
    (
        "process",
        &[
            "env", "exit", "argv", "nextTick", "cwd", "platform", "hrtime", "version",
        ],
    ),
    (
        "console",
        &[
            "log", "error", "warn", "info", "debug", "table", "trace", "assert",
        ],
    ),
];

/// Names that are standard on their own: Web and platform classes and functions a fix writes
/// bare, and the members of the namespaces above when imported by name
/// (`import { randomUUID } from "crypto"`).
const STANDARD_BARE: &[&str] = &[
    "URLSearchParams",
    "AbortController",
    "AbortSignal",
    "TextEncoder",
    "TextDecoder",
    "FormData",
    "ReadableStream",
    "WritableStream",
    "TransformStream",
    "WeakMap",
    "WeakSet",
    "WeakRef",
    "ArrayBuffer",
    "SharedArrayBuffer",
    "Uint8Array",
    "Uint16Array",
    "Uint32Array",
    "Int32Array",
    "Float64Array",
    "BigInt64Array",
    "DataView",
    "structuredClone",
    "queueMicrotask",
    "requestAnimationFrame",
    "cancelAnimationFrame",
    "requestIdleCallback",
    "globalThis",
    "isSafeInteger",
    "isInteger",
    "randomUUID",
    "getRandomValues",
    "timingSafeEqual",
    "randomBytes",
    "randomInt",
    "createHash",
    "createHmac",
    "createCipheriv",
    "createDecipheriv",
    "scryptSync",
    "pbkdf2Sync",
    "readFileSync",
    "writeFileSync",
    "existsSync",
    "setImmediate",
    "clearInterval",
];

/// Prefixes a global object may be reached through (`globalThis.crypto.randomUUID`).
const GLOBAL_PREFIXES: &[&str] = &["globalThis.", "window.", "self.", "global."];

/// ESLint core rules.
const CORE_RULES: &[&str] = &[
    "no-console",
    "no-eval",
    "no-implied-eval",
    "no-new-func",
    "eqeqeq",
    "no-var",
    "prefer-const",
    "no-unused-vars",
    "no-undef",
    "no-empty",
    "no-debugger",
    "no-alert",
    "no-throw-literal",
    "no-param-reassign",
    "no-shadow",
    "no-restricted-syntax",
    "no-restricted-imports",
    "no-restricted-globals",
    "no-restricted-properties",
    "no-unsafe-optional-chaining",
    "no-prototype-builtins",
    "no-useless-escape",
    "no-redeclare",
    "no-fallthrough",
    "no-case-declarations",
    "no-return-await",
    "no-await-in-loop",
    "no-constant-condition",
    "no-nested-ternary",
    "no-script-url",
    "no-process-env",
    "no-sync",
    "no-magic-numbers",
    "no-proto",
    "no-extend-native",
    "no-new-wrappers",
    "no-self-compare",
    "no-unreachable",
    "no-unsafe-finally",
    "no-empty-function",
    "no-duplicate-imports",
    "no-void",
    "no-eq-null",
    "require-await",
    "array-callback-return",
    "consistent-return",
    "default-case",
    "dot-notation",
    "guard-for-in",
    "curly",
    "complexity",
    "max-depth",
    "max-lines",
    "max-params",
    "max-lines-per-function",
    "prefer-template",
    "prefer-promise-reject-errors",
    "radix",
];

/// ESLint plugin rules, written `plugin/rule` (a scoped plugin keeps its `@scope/`).
const PLUGIN_RULES: &[&str] = &[
    "react-hooks/rules-of-hooks",
    "react-hooks/exhaustive-deps",
    "react/jsx-key",
    "react/no-danger",
    "react/no-danger-with-children",
    "react/jsx-no-target-blank",
    "react/jsx-no-script-url",
    "react/no-unescaped-entities",
    "react/react-in-jsx-scope",
    "react/prop-types",
    "react/display-name",
    "react/no-array-index-key",
    "react/jsx-no-bind",
    "react/no-unstable-nested-components",
    "jsx-a11y/alt-text",
    "jsx-a11y/anchor-is-valid",
    "jsx-a11y/click-events-have-key-events",
    "jsx-a11y/no-static-element-interactions",
    "jsx-a11y/label-has-associated-control",
    "import/no-cycle",
    "import/no-default-export",
    "import/no-extraneous-dependencies",
    "import/no-unresolved",
    "import/order",
    "import/no-restricted-paths",
    "security/detect-object-injection",
    "security/detect-eval-with-expression",
    "security/detect-non-literal-regexp",
    "security/detect-unsafe-regex",
    "security/detect-child-process",
    "security/detect-non-literal-fs-filename",
    "security/detect-possible-timing-attacks",
    "security/detect-buffer-noassert",
    "security/detect-no-csrf-before-method-override",
    "@next/next/no-img-element",
    "@next/next/no-html-link-for-pages",
    "@next/next/no-sync-scripts",
    "@typescript-eslint/no-non-null-assertion",
    "@typescript-eslint/no-explicit-any",
    "@typescript-eslint/no-unused-vars",
    "@typescript-eslint/no-floating-promises",
    "@typescript-eslint/no-misused-promises",
    "@typescript-eslint/no-unsafe-assignment",
    "@typescript-eslint/no-unsafe-member-access",
    "@typescript-eslint/no-unsafe-call",
    "@typescript-eslint/no-unsafe-return",
    "@typescript-eslint/no-unsafe-argument",
    "@typescript-eslint/ban-ts-comment",
    "@typescript-eslint/consistent-type-imports",
    "@typescript-eslint/no-var-requires",
    "@typescript-eslint/no-require-imports",
    "@typescript-eslint/no-implied-eval",
    "@typescript-eslint/no-unnecessary-type-assertion",
    "@typescript-eslint/await-thenable",
    "@typescript-eslint/require-await",
    "@typescript-eslint/restrict-template-expressions",
    "@typescript-eslint/strict-boolean-expressions",
    "@typescript-eslint/prefer-nullish-coalescing",
    "@typescript-eslint/prefer-optional-chain",
    "@typescript-eslint/no-non-null-asserted-optional-chain",
    "@typescript-eslint/no-empty-function",
    "@typescript-eslint/no-inferrable-types",
    "@typescript-eslint/no-namespace",
    "@typescript-eslint/no-this-alias",
    "@typescript-eslint/switch-exhaustiveness-check",
];

fn dotted_names() -> &'static HashSet<String> {
    static NAMES: OnceLock<HashSet<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names = HashSet::new();
        for (namespace, members) in STANDARD_MEMBERS {
            names.insert((*namespace).to_string());
            for member in *members {
                names.insert(format!("{namespace}.{member}"));
            }
        }
        names
    })
}

/// Environment variables the operating system or Node defines, not the application
/// (`process.env.TZ` pins a test's timezone, D7). An application's own variable
/// (`process.env.CHARGE_SIGNING_SECRET`) is not here and stays a name to ground.
const PLATFORM_ENV_VARS: &[&str] = &[
    "TZ",
    "NODE_ENV",
    "NODE_OPTIONS",
    "NODE_PATH",
    "NODE_DEBUG",
    "NODE_NO_WARNINGS",
    "NODE_TLS_REJECT_UNAUTHORIZED",
    "UV_THREADPOOL_SIZE",
    "PATH",
    "HOME",
    "USER",
    "SHELL",
    "PWD",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "CI",
    "NO_COLOR",
    "FORCE_COLOR",
];

fn is_platform_env_var(word: &str) -> bool {
    word.strip_prefix("process.env.")
        .is_some_and(|name| PLATFORM_ENV_VARS.contains(&name))
}

/// Whether `word` is a standard JavaScript, Web or Node name: a bare standard name
/// (`URLSearchParams`, `randomUUID`), or `namespace.member` from the lists above, optionally
/// reached through `globalThis.` or `window.`, or a platform environment variable
/// (`process.env.TZ`). A trailing call or dot is the caller's to trim.
pub fn is_standard_js_name(word: &str) -> bool {
    if is_platform_env_var(word) {
        return true;
    }
    let word = GLOBAL_PREFIXES
        .iter()
        .find_map(|p| word.strip_prefix(p))
        .unwrap_or(word);
    STANDARD_BARE.contains(&word) || dotted_names().contains(word)
}

/// Whether `name` is a rule of ESLint core or of a plugin a scan commonly cites
/// (`react-hooks/rules-of-hooks`, `@typescript-eslint/no-non-null-assertion`, `eqeqeq`).
pub fn is_known_eslint_rule(name: &str) -> bool {
    CORE_RULES.contains(&name) || PLUGIN_RULES.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_names_are_recognised_bare_dotted_and_through_a_global() {
        for name in [
            "Number.isSafeInteger",
            "crypto.randomUUID",
            "crypto.subtle.digest",
            "globalThis.crypto.randomUUID",
            "window.location.href",
            "URLSearchParams",
            "randomUUID",
            "Math.max",
        ] {
            assert!(is_standard_js_name(name), "{name}");
        }
    }

    #[test]
    fn platform_environment_variables_are_standard_and_the_applications_own_are_not() {
        for name in ["process.env.TZ", "process.env.NODE_ENV", "process.env.CI"] {
            assert!(is_standard_js_name(name), "{name}");
        }
        for name in [
            "process.env.CHARGE_SIGNING_SECRET",
            "process.env.TZX",
            "env.TZ",
        ] {
            assert!(!is_standard_js_name(name), "{name}");
        }
    }

    #[test]
    fn an_invented_member_of_a_standard_namespace_is_not_standard() {
        for name in [
            "Number.toCents",
            "crypto.makeToken",
            "JSON.parseSafely",
            "process.env.SECRET",
            "window.myApp",
        ] {
            assert!(!is_standard_js_name(name), "{name}");
        }
    }

    #[test]
    fn eslint_rules_are_exact() {
        assert!(is_known_eslint_rule("react-hooks/rules-of-hooks"));
        assert!(is_known_eslint_rule(
            "@typescript-eslint/no-non-null-assertion"
        ));
        assert!(is_known_eslint_rule("no-console"));
        assert!(!is_known_eslint_rule("react-hooks/hook-order-checker"));
        assert!(!is_known_eslint_rule("no-magic-cents"));
    }
}
