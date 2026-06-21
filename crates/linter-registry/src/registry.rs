//! Canonical linter rule-id registry.
//!
//! All rule lists are curated static data scraped from the authoritative sources
//! cited at the top of each section. The registry covers the rule IDs the
//! camerata corpus actually cites; uncited rules may be absent.

use std::collections::{HashMap, HashSet};

/// Result of validating a (tool, rule_id) citation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CitationStatus {
    /// The (tool, rule_id) pair is in the known-good list for this tool.
    Resolves,
    /// The tool is known but the rule_id was not found in its registry.
    NotFound,
    /// The tool key is not recognised by this registry.
    UnknownTool,
}

impl std::fmt::Display for CitationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CitationStatus::Resolves => write!(f, "resolves"),
            CitationStatus::NotFound => write!(f, "not-found"),
            CitationStatus::UnknownTool => write!(f, "unknown-tool"),
        }
    }
}

/// The global registry of known linter rule IDs.
///
/// Constructed once via [`LinterRegistry::global`] using the bundled static
/// data. All comparisons are case-insensitive.
pub struct LinterRegistry {
    /// tool-key → set of lowercase normalised rule ids
    tools: HashMap<&'static str, HashSet<&'static str>>,
}

impl LinterRegistry {
    /// Validate a (tool, rule_id) citation against the registry.
    ///
    /// Tool key matching is case-insensitive. Rule id matching is
    /// case-insensitive after stripping leading/trailing whitespace.
    pub fn validate(&self, tool: &str, rule_id: &str) -> CitationStatus {
        let tool_key = tool.trim().to_ascii_lowercase();
        let id_normalised = rule_id.trim().to_ascii_lowercase();

        // Try the exact tool key first, then known aliases.
        let canonical_key: Option<&'static str> = self.canonical_key(&tool_key);

        match canonical_key {
            None => CitationStatus::UnknownTool,
            Some(key) => {
                let rules = self.tools.get(key).expect("canonical key must exist in map");
                if rules.contains(id_normalised.as_str()) {
                    CitationStatus::Resolves
                } else {
                    CitationStatus::NotFound
                }
            }
        }
    }

    /// Resolve a normalised (lowercase) tool key to its canonical key in `self.tools`.
    fn canonical_key(&self, normalised: &str) -> Option<&'static str> {
        // Direct match.
        if let Some(&k) = self.tools.keys().find(|&&k| k == normalised) {
            return Some(k);
        }
        // Alias table.
        let alias: Option<&'static str> = match normalised {
            "@typescript-eslint" | "ts-eslint" | "typescript_eslint" => {
                Some("typescript-eslint")
            }
            "react_hooks" | "react-hooks-plugin" => Some("react-hooks"),
            "golangci" | "golang-ci" => Some("golangci-lint"),
            "spotbugs" | "spot-bugs" | "findbugs" => Some("spotbugs"),
            "roslyn-style" | "ide" => Some("roslyn-style"),
            "roslyn" | "ca" | "roslyn-ca" => Some("roslyn"),
            _ => None,
        };
        alias.and_then(|a| self.tools.keys().find(|&&k| k == a).copied())
    }

    /// Build and return the global registry.
    ///
    /// The registry is constructed each time this is called (it is cheap since
    /// all data is static). Callers should store the result rather than
    /// calling this repeatedly in hot paths.
    pub fn global() -> Self {
        let mut tools: HashMap<&'static str, HashSet<&'static str>> = HashMap::new();

        tools.insert("clippy", clippy_rules());
        tools.insert("ruff", ruff_rules());
        tools.insert("eslint", eslint_rules());
        tools.insert("typescript-eslint", typescript_eslint_rules());
        tools.insert("react-hooks", react_hooks_rules());
        tools.insert("golangci-lint", golangci_lint_rules());
        tools.insert("rubocop", rubocop_rules());
        tools.insert("checkstyle", checkstyle_rules());
        tools.insert("spotbugs", spotbugs_rules());
        tools.insert("roslyn", roslyn_ca_rules());
        tools.insert("roslyn-style", roslyn_ide_rules());
        tools.insert("bandit", bandit_rules());
        tools.insert("sqlfluff", sqlfluff_rules());

        LinterRegistry { tools }
    }

    /// Returns the set of all tool keys supported by this registry.
    pub fn tool_keys(&self) -> Vec<&'static str> {
        let mut keys: Vec<&'static str> = self.tools.keys().copied().collect();
        keys.sort_unstable();
        keys
    }
}

// ─── Clippy ─────────────────────────────────────────────────────────────────
// Source: https://rust-lang.github.io/rust-clippy/master/
// Curated subset: the lints cited in the camerata corpus plus a representative
// sample of the most common pedantic and restriction lints. The full Clippy
// registry has ~700 lints; only the corpus-relevant ones are listed here.
fn clippy_rules() -> HashSet<&'static str> {
    [
        // Cited directly in corpus
        "unwrap_used",
        "expect_used",
        "panic",
        // Strict frontier (cited as "natural next tier")
        "unwrap_in_result",
        "indexing_slicing",
        // Common restriction lints (frequently cited in generic references)
        "dbg_macro",
        "print_stdout",
        "print_stderr",
        "todo",
        "unimplemented",
        "unreachable",
        // Pedantic (commonly enabled)
        "clone_on_ref_ptr",
        "empty_enum",
        "enum_glob_use",
        "map_unwrap_or",
        "match_same_arms",
        "missing_docs_in_private_items",
        "missing_errors_doc",
        "must_use_candidate",
        "needless_pass_by_value",
        "option_option",
        "pub_enum_variant_names",
        "redundant_closure_for_method_calls",
        "similar_names",
        "single_match_else",
        "too_many_lines",
        "use_self",
        // Style
        "all",
        "pedantic",
        "nursery",
        "cargo",
        // Correctness (always enabled)
        "absurd_extreme_comparisons",
        "almost_swapped",
        "approx_constant",
        "await_holding_lock",
        "await_holding_refcell_ref",
        "bad_bit_mask",
        "cast_enum_constructor",
        "cast_nan_to_int",
        "cast_ptr_alignment",
        "clone_double_ref",
        "cmp_null",
        "cognitive_complexity",
        "deprecated_semver",
        "derive_hash_xor_eq",
        "derive_ord_xor_partial_ord",
        "double_comparisons",
        "double_must_use",
        "drop_copy",
        "drop_ref",
        "enum_clike_unportable_variant",
        "eq_op",
        "erasing_op",
        "exit",
        "fn_address_comparisons",
        "for_loops_over_fallibles",
        "if_same_then_else",
        "ifs_same_cond",
        "ineffective_bit_mask",
        "infinite_iter",
        "invalid_regex",
        "invisible_characters",
        "iter_next_loop",
        "iterator_step_by_zero",
        "let_unit_value",
        "logic_bug",
        "mem_discriminant_non_enum",
        "mem_replace_with_uninit",
        "min_max",
        "mismatched_target_os",
        "mistyped_literal_suffixes",
        "modulo_one",
        "mut_from_ref",
        "mutable_key_type",
        "never_loop",
        "non_octal_unix_permissions",
        "nonsensical_open_options",
        "not_unsafe_ptr_arg_deref",
        "option_env_unwrap",
        "out_of_bounds_indexing",
        "panicking_unwrap",
        "possible_missing_comma",
        "read_zero_byte_vec",
        "recursive_format_impl",
        "reversed_empty_ranges",
        "self_assignment",
        "serde_api_misuse",
        "size_of_in_element_count",
        "suspicious_arithmetic_impl",
        "suspicious_assignment_formatting",
        "suspicious_else_formatting",
        "suspicious_map",
        "suspicious_op_assign_impl",
        "suspicious_splitn",
        "suspicious_unary_op_formatting",
        "swap_ptr_to_ref",
        "temporary_cstring_as_ptr",
        "transmute_bytes_to_str",
        "transmute_float_to_int",
        "transmute_int_to_bool",
        "transmute_int_to_char",
        "transmute_int_to_float",
        "transmute_null_to_fn",
        "transmute_ptr_to_ref",
        "transmuting_null",
        "type_id_on_box",
        "undropped_manually_drops",
        "uninit_assumed_init",
        "uninit_vec",
        "unit_cmp",
        "unit_hash",
        "unit_return_expecting_ord",
        "unsound_collection_transmute",
        "unused_io_amount",
        "useless_attribute",
        "vec_resize_to_zero",
        "while_immutable_condition",
        "wrong_transmute",
        "zst_offset",
    ]
    .into()
}

// ─── Ruff ────────────────────────────────────────────────────────────────────
// Source: https://docs.astral.sh/ruff/rules/
// Curated subset covering the rule IDs cited in the corpus.
// Note: Bandit (B1xx) rule IDs in the corpus map to Ruff's S-series (S1xx)
// equivalents. The original Bandit B105/B106/B107 map to Ruff S105/S106/S107.
fn ruff_rules() -> HashSet<&'static str> {
    [
        // pycodestyle errors (E series) — cited IDs
        "e722",  // bare-except: https://docs.astral.sh/ruff/rules/bare-except/
        "e501",  // line-too-long
        "e711",  // comparison-to-none
        "e712",  // comparison-to-true
        "e721",  // type-comparison
        "e731",  // lambda-assignment
        "e741",  // ambiguous-variable-name
        // pyflakes (F series)
        "f401",  // unused-import
        "f403",  // undefined-local-with-import-star
        "f811",  // redefinition-of-unused-name
        "f841",  // local-variable-is-assigned-to-but-never-used
        // flake8-bugbear (B series)
        "b006",  // mutable-argument-default
        "b007",  // unused-loop-control-variable
        "b008",  // function-call-in-default-argument
        "b023",  // function-uses-loop-variable
        "b024",  // abstract-base-class-without-abstract-method
        // flake8-blind-except (BLE series) — cited directly
        "ble001", // blind-exception: https://docs.astral.sh/ruff/rules/blind-exception/
        // flake8-bandit (S series) — cited directly
        "s105",  // hardcoded-password-string (Bandit B105 equivalent)
        "s106",  // hardcoded-password-func-arg (Bandit B106 equivalent)
        "s107",  // hardcoded-password-default (Bandit B107 equivalent)
        "s608",  // possible-sql-injection: https://docs.astral.sh/ruff/rules/hardcoded-sql-expression/
        "s611",  // django-raw-sql
        "s101",  // assert
        "s102",  // exec-builtin
        "s110",  // try-except-pass
        "s301",  // suspicious-pickle-usage
        "s324",  // hashlib-insecure-hash-functions
        "s501",  // request-with-no-timeout
        "s506",  // unsafe-yaml-load
        // isort (I series)
        "i001",  // unsorted-imports
        // pep8-naming (N series)
        "n801",  // invalid-class-name
        "n802",  // invalid-function-name
        "n803",  // invalid-argument-name
        "n806",  // non-lowercase-variable-in-function
        // flake8-simplify (sim series)
        "sim108", // if-else-block-instead-of-if-exp
        "sim117", // multiple-with-statements
    ]
    .into()
}

// ─── ESLint core ─────────────────────────────────────────────────────────────
// Source: https://eslint.org/docs/latest/rules/
// Curated subset: the rules cited in the corpus plus a representative sample
// of the most commonly used ESLint core rules.
fn eslint_rules() -> HashSet<&'static str> {
    [
        // Cited directly in corpus
        "no-var",
        "no-restricted-imports",
        "no-restricted-syntax",
        "no-restricted-globals",
        "no-restricted-properties",
        // Other commonly cited ESLint core rules
        "eqeqeq",
        "no-console",
        "no-debugger",
        "no-duplicate-imports",
        "no-else-return",
        "no-empty",
        "no-eval",
        "no-extra-bind",
        "no-extra-semi",
        "no-fallthrough",
        "no-implicit-coercion",
        "no-inner-declarations",
        "no-lone-blocks",
        "no-new",
        "no-new-wrappers",
        "no-param-reassign",
        "no-promise-executor-return",
        "no-return-assign",
        "no-return-await",
        "no-self-compare",
        "no-shadow",
        "no-throw-literal",
        "no-undef",
        "no-undefined",
        "no-unneeded-ternary",
        "no-unreachable",
        "no-unsafe-finally",
        "no-unused-expressions",
        "no-unused-vars",
        "no-use-before-define",
        "no-useless-catch",
        "no-useless-concat",
        "no-useless-constructor",
        "no-useless-escape",
        "no-useless-rename",
        "no-useless-return",
        "no-void",
        "prefer-const",
        "prefer-destructuring",
        "prefer-promise-reject-errors",
        "prefer-rest-params",
        "prefer-spread",
        "prefer-template",
        "radix",
        "require-await",
        "yoda",
    ]
    .into()
}

// ─── TypeScript ESLint ───────────────────────────────────────────────────────
// Source: https://typescript-eslint.io/rules/
// Curated subset: the @typescript-eslint/ rules cited in the corpus.
fn typescript_eslint_rules() -> HashSet<&'static str> {
    [
        // Cited directly in corpus (without @typescript-eslint/ prefix — the
        // tool key identifies the namespace)
        "no-explicit-any",
        "no-floating-promises",
        "no-non-null-assertion",
        // Additional commonly enabled rules
        "await-thenable",
        "ban-ts-comment",
        "ban-types",
        "consistent-type-assertions",
        "consistent-type-imports",
        "explicit-function-return-type",
        "explicit-module-boundary-types",
        "no-array-constructor",
        "no-empty-function",
        "no-empty-interface",
        "no-extra-non-null-assertion",
        "no-for-in-array",
        "no-implied-eval",
        "no-inferrable-types",
        "no-misused-new",
        "no-misused-promises",
        "no-namespace",
        "no-require-imports",
        "no-shadow",
        "no-this-alias",
        "no-unnecessary-boolean-literal-compare",
        "no-unnecessary-condition",
        "no-unnecessary-type-arguments",
        "no-unnecessary-type-assertion",
        "no-unsafe-argument",
        "no-unsafe-assignment",
        "no-unsafe-call",
        "no-unsafe-member-access",
        "no-unsafe-return",
        "no-unused-vars",
        "no-use-before-define",
        "prefer-as-const",
        "prefer-nullish-coalescing",
        "prefer-optional-chain",
        "require-await",
        "restrict-plus-operands",
        "strict-boolean-expressions",
        "switch-exhaustiveness-check",
        "unbound-method",
    ]
    .into()
}

// ─── eslint-plugin-react-hooks ───────────────────────────────────────────────
// Source: https://www.npmjs.com/package/eslint-plugin-react-hooks
// The plugin ships exactly two rules (without the react-hooks/ prefix — the
// tool key identifies the namespace).
fn react_hooks_rules() -> HashSet<&'static str> {
    [
        "rules-of-hooks",    // Enforce Rules of Hooks
        "exhaustive-deps",   // Exhaustive dependencies for useEffect/useMemo/etc.
    ]
    .into()
}

// ─── golangci-lint ───────────────────────────────────────────────────────────
// Source: https://golangci-lint.run/usage/linters/
// Linter names only. Staticcheck SA-codes are not enumerated here; the corpus
// only cites the linter names, not individual SA codes.
fn golangci_lint_rules() -> HashSet<&'static str> {
    [
        // Cited in corpus
        "errcheck",       // Unhandled error returns
        "staticcheck",    // Full staticcheck suite
        // Other commonly enabled golangci-lint linters
        "gofmt",
        "goimports",
        "govet",
        "ineffassign",
        "misspell",
        "revive",
        "unconvert",
        "unused",
        "gosec",
        "gocritic",
        "godot",
        "nolintlint",
        "prealloc",
        "rowserrcheck",
        "sqlclosecheck",
        "typecheck",
    ]
    .into()
}

// ─── RuboCop ─────────────────────────────────────────────────────────────────
// Source: https://docs.rubocop.org/rubocop/
// Curated subset: cops cited in the corpus (the FrozenStringLiteral and
// Brakeman-flagged patterns).
fn rubocop_rules() -> HashSet<&'static str> {
    // NOTE: All cop names stored in lowercase for case-insensitive lookup.
    // RuboCop's canonical cop names use PascalCase namespaces (e.g. Style/FrozenStringLiteralComment)
    // but lookups are normalised to lowercase before comparison.
    // Source: https://docs.rubocop.org/rubocop/
    [
        // Cited in corpus (via "RuboCop with FrozenStringLiteral cop")
        "style/frozenstringliteralcomment",
        // Commonly cited Ruby/Rails cops (lowercase)
        "layout/endalignment",
        "layout/hashalignment",
        "layout/indentationwidth",
        "layout/linelength",
        "layout/spacearoundoperators",
        "metrics/classlength",
        "metrics/cyclomaticcomplexity",
        "metrics/methodlength",
        "naming/methodname",
        "security/eval",
        "security/jsonload",
        "security/marshalload",
        "security/open",
        "security/yamlload",
        "style/documentationmethod",
        "style/guardclause",
        "style/methodcallwithoutargsparentheses",
        "style/preferredhashmethods",
        "style/returnnil",
        "style/symbolproc",
        "rails/activerecordaliases",
        "rails/dynamicfindby",
        "rails/findby",
        "rails/findeach",
        "rails/hasandbelongstomany",
        "rails/httpstatus",
        "rails/inverseof",
        "rails/outputsafety",
        "rails/present",
        "rails/reflectionclassname",
        "rails/safenavigation",
        "rails/savewithbang",
        "rails/skipbeforefilter",
        "rails/whereexists",
    ]
    .into()
}

// ─── Checkstyle ──────────────────────────────────────────────────────────────
// Source: https://checkstyle.sourceforge.io/checks.html (v10.x)
// Curated subset: the Checkstyle check names cited in the corpus.
fn checkstyle_rules() -> HashSet<&'static str> {
    [
        // Cited directly in corpus
        "finallocalvariable",    // FinalLocalVariable
        "finalclass",            // FinalClass
        "visibilitymodifier",    // VisibilityModifier
        "avoidstarimport",       // AvoidStarImport
        "magicnumber",           // MagicNumber
        "annotationusestyle",    // AnnotationUseStyle
        // Also cited generically (no specific check name given):
        // "system.out.println" → Checkstyle/Semgrep code review
        // "constructor injection" → Checkstyle + review
        // Additional commonly cited checks
        "methodlength",
        "parameternumber",
        "cyclomaticcomplexity",
        "missingjavadocmethod",
        "missingjavadoctype",
        "nopathologicallylargemethod",
        "onetoplevelclass",
        "outerclasstypecheck",
        "regexpmultiline",
        "regexpsingleline",
        "throwscount",
        "unusedimports",
        "whitespaceafter",
        "whitespacearound",
    ]
    .into()
}

// ─── SpotBugs ────────────────────────────────────────────────────────────────
// Source: https://spotbugs.readthedocs.io/en/latest/bugDescriptions.html
// Curated subset: patterns cited in the corpus (resource leaks, null, SQL).
fn spotbugs_rules() -> HashSet<&'static str> {
    [
        // Resource leaks — cited in corpus
        "os_open_stream",
        "os_open_stream_exception_path",
        "odr_open_database_resource",
        "odr_open_database_resource_exception_path",
        // Null dereference — cited in corpus
        "np_argument_might_be_null",
        "np_unwritten_field",
        "np_equals_should_handle_null_argument",
        "np_null_on_some_path",
        "np_null_on_some_path_exception",
        // SQL injection
        "sql_nonconstant_string_passed_to_execute",
        "sql_prepared_statement_generated_from_nonconstant_string",
        // Other commonly cited patterns
        "dls_dead_local_store",
        "dm_convert_case",
        "dm_string_ctor",
        "ei_expose_rep",
        "ei_expose_rep2",
        "is_inconsistent_sync",
        "ms_mutable_array",
        "ms_mutable_collection",
        "ms_mutable_hashtable",
        "ms_pkgprotect",
        "ms_should_be_final",
        "pzla_prefer_zero_length_arrays",
        "rr_not_checked",
        "rv_return_value_ignored",
        "se_bad_field",
        "se_transient_field_not_restored",
        "sic_inner_should_be_static",
        "ur_uninit_read",
        "ux_exception_defensive_code",
    ]
    .into()
}

// ─── Roslyn CA (quality rules) ───────────────────────────────────────────────
// Source: https://learn.microsoft.com/dotnet/fundamentals/code-analysis/quality-rules/
// Curated subset: the CA rule IDs cited in the corpus.
fn roslyn_ca_rules() -> HashSet<&'static str> {
    [
        // Cited directly in corpus
        "ca1001",  // Types that own disposable fields should be disposable
        "ca1031",  // Do not catch general exception types
        "ca1068",  // CancellationToken parameters must come last
        "ca1816",  // Call GC.SuppressFinalize correctly
        // Additional commonly cited CA rules
        "ca1000",  // Do not declare static members on generic types
        "ca1002",  // Do not expose generic lists
        "ca1003",  // Use generic event handler instances
        "ca1008",  // Enums should have zero value
        "ca1010",  // Generic interface should also be implemented
        "ca1012",  // Abstract types should not have public constructors
        "ca1014",  // Mark assemblies with CLSCompliantAttribute
        "ca1016",  // Mark assemblies with AssemblyVersionAttribute
        "ca1017",  // Mark assemblies with ComVisibleAttribute
        "ca1018",  // Mark attributes with AttributeUsageAttribute
        "ca1019",  // Define accessors for attribute arguments
        "ca1021",  // Avoid out parameters
        "ca1024",  // Use properties where appropriate
        "ca1027",  // Mark enums with FlagsAttribute
        "ca1028",  // Enum storage should be Int32
        "ca1030",  // Use events where appropriate
        "ca1032",  // Implement standard exception constructors
        "ca1033",  // Interface methods should be callable by child types
        "ca1034",  // Nested types should not be visible
        "ca1036",  // Override methods on comparable types
        "ca1040",  // Avoid empty interfaces
        "ca1041",  // Provide ObsoleteAttribute message
        "ca1043",  // Use integral or string argument for indexers
        "ca1044",  // Properties should not be write only
        "ca1045",  // Do not pass types by reference
        "ca1046",  // Do not overload operator equals on reference types
        "ca1047",  // Do not declare protected members in sealed types
        "ca1050",  // Declare types in namespaces
        "ca1051",  // Do not declare visible instance fields
        "ca1052",  // Static holder types should be static or NotInheritable
        "ca1054",  // URI-like parameters should not be strings
        "ca1055",  // URI-like return values should not be strings
        "ca1056",  // URI-like properties should not be strings
        "ca1058",  // Types should not extend certain base types
        "ca1060",  // Move P/Invokes to NativeMethods class
        "ca1061",  // Do not hide base class methods
        "ca1062",  // Validate arguments of public methods
        "ca1063",  // Implement IDisposable correctly
        "ca1064",  // Exceptions should be public
        "ca1065",  // Do not raise exceptions in unexpected locations
        "ca1066",  // Implement IEquatable when overriding Equals
        "ca1067",  // Override Equals when implementing IEquatable
        "ca1069",  // Enum values should not be duplicated
        "ca1070",  // Do not declare event fields as virtual
        "ca1200",  // Avoid using cref tags with a prefix
        "ca1501",  // Avoid excessive inheritance
        "ca1502",  // Avoid excessive complexity
        "ca1505",  // Avoid unmaintainable code
        "ca1506",  // Avoid excessive class coupling
        "ca1508",  // Avoid dead conditional code
        "ca1509",  // Invalid entry in code metrics rule specification file
        "ca1700",  // Do not name enum values 'Reserved'
        "ca1707",  // Identifiers should not contain underscores
        "ca1708",  // Identifiers should differ by more than case
        "ca1710",  // Identifiers should have correct suffix
        "ca1711",  // Identifiers should not have incorrect suffix
        "ca1712",  // Do not prefix enum values with type name
        "ca1713",  // Events should not have before or after prefix
        "ca1714",  // Flags enums should have plural names
        "ca1715",  // Identifiers should have correct prefix
        "ca1716",  // Identifiers should not match keywords
        "ca1717",  // Only FlagsAttribute enums should have plural names
        "ca1720",  // Identifier contains type name
        "ca1721",  // Property names should not match get methods
        "ca1724",  // Type names should not match namespaces
        "ca1725",  // Parameter names should match base declaration
        "ca1727",  // Use PascalCase for named placeholders
        "ca1800",  // Do not cast unnecessarily
        "ca1801",  // Review unused parameters
        "ca1802",  // Use literals where appropriate
        "ca1805",  // Do not initialize unnecessarily
        "ca1806",  // Do not ignore method results
        "ca1810",  // Initialize reference type static fields inline
        "ca1812",  // Avoid uninstantiated internal classes
        "ca1813",  // Avoid unsealed attributes
        "ca1814",  // Prefer jagged arrays over multidimensional
        "ca1815",  // Override equals and operator equals on value types
        "ca1819",  // Properties should not return arrays
        "ca1820",  // Test for empty strings using string length
        "ca1821",  // Remove empty finalizers
        "ca1822",  // Mark members as static
        "ca1823",  // Avoid unused private fields
        "ca1824",  // Mark assemblies with NeutralResourcesLanguageAttribute
        "ca1825",  // Avoid zero-length array allocations
        "ca1826",  // Do not use Enumerable methods on indexable collections
        "ca1827",  // Do not use Count/LongCount when Any can be used
        "ca1828",  // Do not use CountAsync/LongCountAsync when AnyAsync can be used
        "ca1829",  // Use Length/Count property instead of Enumerable.Count method
        "ca1830",  // Prefer strongly-typed Append and Insert method overloads on StringBuilder
        "ca1831",  // Use AsSpan instead of Range-based indexer for string
        "ca1832",  // Use AsSpan or AsMemory instead of Range-based indexer
        "ca1833",  // Use AsSpan or AsMemory instead of Range-based indexer for getting a Span
        "ca1834",  // Use StringBuilder.Append(char) for single character strings
        "ca1835",  // Prefer the memory-based overloads of ReadAsync/WriteAsync
        "ca1836",  // Prefer IsEmpty over Count when available
        "ca1837",  // Use Environment.ProcessId instead of Process.GetCurrentProcess().Id
        "ca1838",  // Avoid StringBuilder parameters for P/Invokes
        "ca1839",  // Use Environment.ProcessPath instead of Process.GetCurrentProcess().MainModule?.FileName
        "ca1840",  // Use Environment.CurrentManagedThreadId instead of Thread.CurrentThread.ManagedThreadId
        "ca1841",  // Prefer Dictionary Contains methods
        "ca1842",  // Do not use 'WhenAll' with a single task
        "ca1843",  // Do not use 'WaitAll' with a single task
        "ca1844",  // Provide memory-based overloads of ReadAsync and WriteAsync
        "ca1845",  // Use span-based 'string.Concat' and 'AsSpan' instead of 'Substring'
        "ca1846",  // Prefer AsSpan over Substring when span-based overloads are available
        "ca1847",  // Use string.Contains(char) instead of string.Contains(string)
        "ca1848",  // Use the LoggerMessage delegates
        "ca1849",  // Call async methods when in an async method
        "ca1850",  // Prefer static HashData method over ComputeHash
        "ca1851",  // Possible multiple enumerations of IEnumerable collection
        "ca1852",  // Seal internal types
        "ca1853",  // Unnecessary call to 'Dictionary.ContainsKey(key)'
        "ca1854",  // Prefer the IDictionary.TryGetValue(TKey, out TValue) method
        "ca1855",  // Prefer clear over fill
        "ca1856",  // Incorrect usage of ConstantExpected attribute
        "ca1857",  // A constant is expected for the parameter
        "ca1858",  // Use StartsWith instead of IndexOf
        "ca1859",  // Use concrete types when possible for improved performance
        "ca1860",  // Avoid using 'Enumerable.Any()' extension method
        "ca1861",  // Avoid constant arrays as arguments
        "ca1862",  // Use StringComparison method overloads to perform case-insensitive string comparisons
        "ca1863",  // Use CompositeFormat
        "ca1864",  // Prefer the IDictionary.TryAdd(TKey, TValue) method
        "ca1865",  // Use string.Method(char) instead of string.Method(string)
        "ca1866",  // Use string.Method(char) instead of string.Method(string)
        "ca1867",  // Use string.Method(char) instead of string.Method(string)
        "ca1868",  // Unnecessary call to 'Contains' for sets
        "ca1869",  // Cache and reuse 'JsonSerializerOptions' instances
        "ca1870",  // Use a cached SearchValues instance
        "ca2000",  // Dispose objects before losing scope
        "ca2002",  // Do not lock on objects with weak identity
        "ca2007",  // Consider calling ConfigureAwait on the awaited task
        "ca2008",  // Do not create tasks without passing a TaskScheduler
        "ca2009",  // Do not call ToImmutableCollection on an ImmutableCollection value
        "ca2011",  // Avoid infinite recursion
        "ca2012",  // Use ValueTask correctly
        "ca2013",  // Do not use ReferenceEquals with value types
        "ca2014",  // Do not use stackalloc in loops
        "ca2015",  // Do not define finalizers for types derived from MemoryManager<T>
        "ca2016",  // Forward the CancellationToken parameter to methods that take one
        "ca2017",  // Parameter count mismatch
        "ca2018",  // Count argument of Buffer.BlockCopy must specify the number of bytes to copy
        "ca2019",  // ThreadStatic fields should not use inline initialization
        "ca2020",  // Behavior change due to operator precedence
        "ca2100",  // Review SQL queries for security vulnerabilities
        "ca2101",  // Specify marshaling for P/Invoke string arguments
        "ca2109",  // Review visible event handlers
        "ca2119",  // Seal methods that satisfy private interfaces
        "ca2153",  // Do not catch corrupted state exceptions
        "ca2200",  // Rethrow to preserve stack details
        "ca2201",  // Do not raise reserved exception types
        "ca2207",  // Initialize value type static fields inline
        "ca2208",  // Instantiate argument exceptions correctly
        "ca2211",  // Non-constant fields should not be visible
        "ca2213",  // Disposable fields should be disposed
        "ca2214",  // Do not call overridable methods in constructors
        "ca2215",  // Dispose methods should call base class dispose
        "ca2216",  // Disposable types should declare finalizer
        "ca2217",  // Do not mark enums with FlagsAttribute
        "ca2218",  // Override GetHashCode on overriding Equals
        "ca2219",  // Do not raise exceptions in finally clauses
        "ca2224",  // Override Equals on overloading operator equals
        "ca2225",  // Operator overloads have named alternates
        "ca2226",  // Operators should have symmetrical overloads
        "ca2227",  // Collection properties should be read only
        "ca2229",  // Implement serialization constructors
        "ca2231",  // Overload operator equals on overriding ValueType.Equals
        "ca2234",  // Pass System.Uri objects instead of strings
        "ca2235",  // Mark all non-serializable fields
        "ca2237",  // Mark ISerializable types with SerializableAttribute
        "ca2241",  // Provide correct arguments to formatting methods
        "ca2242",  // Test for NaN correctly
        "ca2243",  // Attribute string literals should parse correctly
        "ca2244",  // Do not duplicate indexed element initializations
        "ca2245",  // Do not assign a property to itself
        "ca2246",  // Assigning symbol and its member in the same statement
        "ca2247",  // Argument passed to TaskCompletionSource constructor should be TaskCreationOptions value
        "ca2248",  // Provide correct enum argument to Enum.HasFlag
        "ca2249",  // Consider using string.Contains instead of string.IndexOf
        "ca2250",  // Use ThrowIfCancellationRequested
        "ca2251",  // Use String.Equals over String.Compare
        "ca2252",  // This API requires opting into preview features
        "ca2253",  // Named placeholders should not be numeric values
        "ca2254",  // Template should be a static expression
        "ca2255",  // The ModuleInitializer attribute should not be used in libraries
        "ca2256",  // All members declared in parent interfaces must have an implementation
        "ca2257",  // Members defined on an interface with the DynamicInterfaceCastableImplementationAttribute should be static
        "ca2258",  // Providing a DynamicInterfaceCastableImplementation interface in Visual Basic is unsupported
        "ca2259",  // Ensure ThreadStatic is only used with static fields
        "ca2260",  // Implement generic math interfaces correctly
        "ca2261",  // Do not use ConfigureAwaitOptions.SuppressThrowing with Task.GetAwaiter
        "ca2262",  // Set MaxResponseHeadersLength properly
        "ca3001",  // Review code for SQL injection vulnerabilities
        "ca3002",  // Review code for XSS vulnerabilities
        "ca3003",  // Review code for file path injection vulnerabilities
        "ca3004",  // Review code for information disclosure vulnerabilities
        "ca3005",  // Review code for LDAP injection vulnerabilities
        "ca3006",  // Review code for process command injection vulnerabilities
        "ca3007",  // Review code for open redirect vulnerabilities
        "ca3008",  // Review code for XPath injection vulnerabilities
        "ca3009",  // Review code for XML injection vulnerabilities
        "ca3010",  // Review code for XAML injection vulnerabilities
        "ca3011",  // Review code for DLL injection vulnerabilities
        "ca3012",  // Review code for regex injection vulnerabilities
        "ca5350",  // Do not use weak cryptographic algorithms
        "ca5351",  // Do not use broken cryptographic algorithms
        "ca5358",  // Do not use unsafe cipher modes
        "ca5359",  // Do not disable certificate validation
        "ca5360",  // Do not call dangerous methods in deserialization
        "ca5361",  // Do not disable SChannel use of strong crypto
        "ca5362",  // Potential reference cycle in deserialized object graph
        "ca5363",  // Do not disable request validation
        "ca5364",  // Do not use deprecated security protocols
        "ca5365",  // Do not disable HTTP header checking
        "ca5366",  // Use XmlReader for DataSet.ReadXml
        "ca5367",  // Do not serialize types with pointer fields
        "ca5368",  // Set ViewStateUserKey for classes derived from Page
        "ca5369",  // Use XmlReader for Deserialize
        "ca5370",  // Use XmlReader for validating reader
        "ca5371",  // Use XmlReader for schema read
        "ca5372",  // Use XmlReader for XPathDocument
        "ca5373",  // Do not use obsolete key derivation function
        "ca5374",  // Do not use XslTransform
        "ca5375",  // Do not use account shared access signature
        "ca5376",  // Use SharedAccessProtocol HttpsOnly
        "ca5377",  // Use container level access policy
        "ca5378",  // Do not disable ServicePointManagerSecurityProtocols
        "ca5379",  // Ensure key derivation function algorithm is sufficiently strong
        "ca5380",  // Do not add certificates to root store
        "ca5381",  // Ensure certificates are not added to root store
        "ca5382",  // Use secure cookies in ASP.NET Core
        "ca5383",  // Ensure use secure cookies in ASP.NET Core
        "ca5384",  // Do not use digital signature algorithm (DSA)
        "ca5385",  // Use Rivest–Shamir–Adleman (RSA) algorithm with sufficient key size
        "ca5386",  // Avoid hardcoding SecurityProtocolType value
        "ca5387",  // Do not use weak key derivation function with insufficient iteration count
        "ca5388",  // Ensure sufficient iteration count when using weak key derivation function
        "ca5389",  // Do not add archive item's path to the target file system path
        "ca5390",  // Do not hard-code encryption key
        "ca5391",  // Use antiforgery tokens in ASP.NET Core MVC controllers
        "ca5392",  // Use DefaultDllImportSearchPaths attribute for P/Invokes
        "ca5393",  // Do not use unsafe DllImportSearchPath value
        "ca5394",  // Do not use insecure randomness
        "ca5395",  // Miss HttpVerb attribute for action methods
        "ca5396",  // Set HttpOnly to true for HttpCookie
        "ca5397",  // Do not use deprecated SslProtocols values
        "ca5398",  // Avoid hardcoded SslProtocols values
        "ca5399",  // HttpClients should enable certificate revocation list checks
        "ca5400",  // HttpClients should enable certificate revocation list checks
        "ca5401",  // Do not use CreateEncryptor with non-default IV
        "ca5402",  // Use CreateEncryptor with the default IV
        "ca5403",  // Do not hard-code certificate
    ]
    .into()
}

// ─── Roslyn IDE (style rules) ────────────────────────────────────────────────
// Source: https://learn.microsoft.com/dotnet/fundamentals/code-analysis/style-rules/
// Curated subset: IDE style rules cited in the corpus.
fn roslyn_ide_rules() -> HashSet<&'static str> {
    [
        // Cited in corpus
        "ide0090",  // Simplify 'new' expression (target-typed new)
        // Additional commonly cited IDE style rules
        "ide0001",  // Simplify name
        "ide0002",  // Simplify member access
        "ide0003",  // Remove this or Me qualification
        "ide0004",  // Remove unnecessary cast
        "ide0005",  // Remove unnecessary import
        "ide0007",  // Use var instead of explicit type
        "ide0008",  // Use explicit type instead of var
        "ide0009",  // Add this or Me qualification
        "ide0010",  // Add missing cases to switch statement
        "ide0011",  // Add braces
        "ide0016",  // Use throw expression
        "ide0017",  // Simplify object initialization
        "ide0018",  // Inline variable declaration
        "ide0019",  // Use pattern matching to avoid as followed by a null check
        "ide0020",  // Use pattern matching to avoid is check followed by a cast
        "ide0021",  // Use expression body for constructors
        "ide0022",  // Use expression body for methods
        "ide0023",  // Use expression body for conversion operators
        "ide0024",  // Use expression body for operators
        "ide0025",  // Use expression body for properties
        "ide0026",  // Use expression body for indexers
        "ide0027",  // Use expression body for accessors
        "ide0028",  // Use collection initializers
        "ide0029",  // Use coalesce expression (non-nullable types)
        "ide0030",  // Use coalesce expression (nullable types)
        "ide0031",  // Use null propagation
        "ide0032",  // Use auto property
        "ide0033",  // Use explicitly provided tuple name
        "ide0034",  // Simplify default expression
        "ide0035",  // Remove unreachable code
        "ide0036",  // Order modifiers
        "ide0037",  // Use inferred member name
        "ide0038",  // Use pattern matching to avoid is check followed by a cast
        "ide0039",  // Use local function instead of lambda
        "ide0040",  // Add accessibility modifiers
        "ide0041",  // Use is null check
        "ide0042",  // Deconstruct variable declaration
        "ide0043",  // Invalid format string
        "ide0044",  // Add readonly modifier
        "ide0045",  // Use conditional expression for assignment
        "ide0046",  // Use conditional expression for return
        "ide0047",  // Remove unnecessary parentheses
        "ide0048",  // Add parentheses for clarity
        "ide0049",  // Use language keywords instead of framework type names for type references
        "ide0051",  // Remove unused private member
        "ide0052",  // Remove unread private member
        "ide0053",  // Use expression body for lambdas
        "ide0054",  // Use compound assignment
        "ide0055",  // Fix formatting
        "ide0056",  // Use index operator
        "ide0057",  // Use range operator
        "ide0058",  // Remove unnecessary expression value
        "ide0059",  // Remove unnecessary value assignment
        "ide0060",  // Remove unused parameter
        "ide0061",  // Use expression body for local functions
        "ide0062",  // Make local function static
        "ide0063",  // Use simple using statement
        "ide0064",  // Make struct fields writable
        "ide0065",  // Misplaced using directive
        "ide0066",  // Use switch expression
        "ide0070",  // Use System.HashCode.Combine
        "ide0071",  // Simplify interpolation
        "ide0072",  // Add missing cases to switch expression
        "ide0073",  // File header required
        "ide0074",  // Use coalesce compound assignment
        "ide0075",  // Simplify conditional expression
        "ide0076",  // Invalid global SuppressMessageAttribute
        "ide0077",  // Avoid legacy format target in global SuppressMessageAttribute
        "ide0078",  // Use pattern matching
        "ide0079",  // Remove unnecessary suppression
        "ide0080",  // Remove unnecessary suppression operator
        "ide0081",  // Remove ByVal
        "ide0082",  // Convert typeof to nameof
        "ide0083",  // Use pattern matching (not operator)
        "ide0084",  // Use pattern matching (IsNot operator)
        "ide0100",  // Remove unnecessary equality operator
        "ide0110",  // Remove unnecessary discard
        "ide0120",  // Simplify LINQ expression
        "ide0130",  // Namespace does not match folder structure
        "ide0150",  // Prefer null check over type check
        "ide0160",  // Use block-scoped namespace
        "ide0161",  // Use file-scoped namespace
        "ide0170",  // Simplify property pattern
        "ide0180",  // Use tuple swap
        "ide0200",  // Remove unnecessary lambda expression
        "ide0210",  // Convert to top-level statements
        "ide0211",  // Convert to Program.Main style program
        "ide0220",  // Add explicit cast in foreach loop
        "ide0230",  // Use UTF-8 string literal
        "ide0240",  // Nullable directive is redundant
        "ide0241",  // Nullable directive is unnecessary
        "ide0250",  // Make struct readonly
        "ide0251",  // Make struct member readonly
        "ide0260",  // Use pattern matching
        "ide0270",  // Use coalesce expression
        "ide0280",  // Use nameof
        "ide0290",  // Use primary constructor
        "ide0300",  // Use collection expression for array
        "ide0301",  // Use collection expression for empty
        "ide0302",  // Use collection expression for stack alloc
        "ide0303",  // Use collection expression for Create
        "ide0304",  // Use collection expression for builder
        "ide0305",  // Use collection expression for fluent
        "ide1005",  // Use conditional delegate call
        "ide1006",  // Naming rule violation
        "ide1007",  // Naming rule violation (parameter/local)
    ]
    .into()
}

// ─── Bandit ──────────────────────────────────────────────────────────────────
// Source: https://bandit.readthedocs.io/en/latest/plugins/
// Note: When using Ruff for Python, Bandit's B1xx rules map to Ruff's S1xx.
// This registry lists the original Bandit B-prefixed IDs.
fn bandit_rules() -> HashSet<&'static str> {
    [
        // Cited directly in corpus (hardcoded passwords)
        "b105",  // hardcoded_password_string
        "b106",  // hardcoded_password_funcarg
        "b107",  // hardcoded_password_default
        // SQL injection — cited in corpus
        "b608",  // hardcoded_sql_expressions
        // Other commonly cited Bandit rules
        "b101",  // assert_used
        "b102",  // exec_used
        "b103",  // setting_noqa
        "b104",  // hardcoded_bind_all_interfaces
        "b108",  // probable_insecure_usage_of_temp_file
        "b110",  // try_except_pass
        "b112",  // try_except_continue
        "b201",  // flask_debug_true
        "b301",  // pickle
        "b302",  // marshal
        "b303",  // md5
        "b304",  // ciphers
        "b305",  // cipher_modes
        "b306",  // mktemp_q
        "b307",  // eval
        "b308",  // mark_safe
        "b310",  // urllib_urlopen
        "b311",  // random
        "b312",  // telnetlib
        "b313",  // xml_bad_cElementTree
        "b314",  // xml_bad_ElementTree
        "b315",  // xml_bad_expatreader
        "b316",  // xml_bad_expatbuilder
        "b317",  // xml_bad_sax
        "b318",  // xml_bad_minidom
        "b319",  // xml_bad_pulldom
        "b320",  // xml_bad_etree
        "b321",  // ftp_lib
        "b322",  // input
        "b323",  // unverified_context
        "b324",  // hashlib
        "b325",  // tempnam
        "b401",  // import_telnetlib
        "b402",  // import_ftplib
        "b403",  // import_pickle
        "b404",  // import_subprocess
        "b405",  // import_xml_etree
        "b406",  // import_xml_sax
        "b407",  // import_xml_expat
        "b408",  // import_xml_minidom
        "b409",  // import_xml_pulldom
        "b410",  // import_lxml
        "b411",  // import_xmlrpclib
        "b412",  // import_httpoxy
        "b413",  // import_pycrypto
        "b501",  // request_with_no_timeout
        "b502",  // ssl_with_bad_version
        "b503",  // ssl_with_bad_defaults
        "b504",  // ssl_with_no_version
        "b505",  // weak_cryptographic_key
        "b506",  // yaml_load
        "b507",  // ssh_no_host_key_verification
        "b601",  // paramiko_calls
        "b602",  // subprocess_popen_with_shell_equals_true
        "b603",  // subprocess_without_shell_equals_true
        "b604",  // any_other_function_with_shell_equals_true
        "b605",  // start_process_with_a_shell
        "b606",  // start_process_with_no_shell
        "b607",  // start_process_with_partial_path
        "b609",  // linux_commands_wildcard_injection
        "b610",  // django_extra_used
        "b611",  // django_rawsql_used
        "b701",  // jinja2_autoescape_false
        "b702",  // use_of_mako_templates
        "b703",  // django_mark_safe
    ]
    .into()
}

// ─── SQLFluff ────────────────────────────────────────────────────────────────
// Source: https://docs.sqlfluff.com/en/stable/reference/rules.html
// Representative sample. The corpus does not cite specific sqlfluff IDs yet;
// this covers the most commonly enforced rules for reference.
fn sqlfluff_rules() -> HashSet<&'static str> {
    [
        // Layout rules (L series, pre-1.0 naming still common in IDEs)
        "al01",  // Implicit/explicit aliases for select
        "al02",  // Implicit/explicit aliases for columns
        "al03",  // Table alias not required
        "al04",  // Table aliases should be unique
        "al05",  // Tables should not be aliased if that alias is not used
        "al06",  // Implicit/explicit aliases for subqueries
        "al07",  // Avoid table aliases in from clauses and join conditions
        "am01",  // Order of select wildcards
        "am02",  // Use explicit column references
        "am03",  // Consistent column reference flavour
        "am04",  // SELECT wildcards then simple select targets before calculations and aggregates
        "am05",  // JOIN clause fully qualified
        "am06",  // Select wildcards then simple select targets before calculations and aggregates
        "am07",  // Consistent syntax to express "all columns"
        "cp01",  // Consistent capitalisation of keywords
        "cp02",  // Consistent capitalisation of identifiers
        "cp03",  // Consistent capitalisation of functions
        "cp04",  // Consistent capitalisation of literals
        "cp05",  // Consistent capitalisation of datatypes
        "cv01",  // Use NULL not empty string ''
        "cv02",  // Use COALESCE instead of IFNULL or NVL
        "cv03",  // Do not use SELECT * in subqueries
        "cv04",  // Use consistent syntax to specify join types
        "cv05",  // Operator spacing
        "cv06",  // Semicolon formatting and placement
        "cv07",  // Prefer ANSI over TSQL JOIN syntax
        "cv08",  // LEFT JOIN vs RIGHT JOIN
        "cv09",  // Unnecessary ELSE NULL in a CASE WHEN statement
        "cv10",  // ILIKE in WHERE clause should be avoided
        "cv11",  // Enforce a standard way of referencing exponents
        "jj01",  // Do not use Jinja set blocks
        "lt01",  // Unnecessary trailing whitespace
        "lt02",  // Indentation not consistent with rules
        "lt03",  // Operators should not be at the end of a line
        "lt04",  // Operators should be at the end of a line
        "lt05",  // Line is too long
        "lt06",  // Functions should not be on separate lines
        "lt07",  // With clause closing bracket should be on new line
        "lt08",  // Blank lines between statements
        "lt09",  // Select targets should all be on one line or each on their own
        "lt10",  // Select modifiers (distinct etc) must be on the same line as select
        "lt11",  // Set operators should be surrounded by newlines
        "lt12",  // Files must end with a single trailing newline
        "lt13",  // Files must not begin with newlines or whitespace
        "rf01",  // References cannot reference objects not present in 'from' clause
        "rf02",  // References should be consistent in from clause
        "rf03",  // References should be consistent in output references
        "rf04",  // Keywords should not be used as identifiers
        "rf05",  // Column name contains special character
        "rf06",  // Unnecessary quoted identifier
        "st01",  // Do not use DISTINCT with parentheses
        "st02",  // Unnecessary CASE expression
        "st03",  // Query defines a CTE (with block) but does not use it
        "st04",  // Nested CASE statement in ELSE clause could be flattened
        "st05",  // Use simple CASE expression instead of searched CASE
        "st06",  // Select wildcards then simple select targets before calculations and aggregates
        "st07",  // Prefer specifying join keys instead of using USING
        "st08",  // Select wildcards then simple select targets before calculations and aggregates
        "st09",  // Joins should list the table referenced first
        "ts01",  // Column references should use aliases
        "ts02",  // References should not be used in GROUP BY
        "ts03",  // Do not use GROUP BY ordinals
    ]
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> LinterRegistry {
        LinterRegistry::global()
    }

    // ── Basic fixture: known-real, not-found, unknown-tool ───────────────────

    #[test]
    fn known_real_clippy_lint_resolves() {
        assert_eq!(
            registry().validate("clippy", "unwrap_used"),
            CitationStatus::Resolves
        );
    }

    #[test]
    fn fake_clippy_lint_is_not_found() {
        assert_eq!(
            registry().validate("clippy", "completely_made_up_lint_name"),
            CitationStatus::NotFound
        );
    }

    #[test]
    fn unknown_tool_returns_unknown_tool() {
        assert_eq!(
            registry().validate("blarg", "anything"),
            CitationStatus::UnknownTool
        );
    }

    // ── Tool key case-insensitivity ──────────────────────────────────────────

    #[test]
    fn tool_key_is_case_insensitive() {
        let r = registry();
        assert_eq!(r.validate("Clippy", "unwrap_used"), CitationStatus::Resolves);
        assert_eq!(r.validate("CLIPPY", "unwrap_used"), CitationStatus::Resolves);
        assert_eq!(r.validate("RUFF", "e722"), CitationStatus::Resolves);
    }

    // ── Rule ID case-insensitivity ────────────────────────────────────────────

    #[test]
    fn rule_id_is_case_insensitive() {
        let r = registry();
        assert_eq!(r.validate("ruff", "E722"), CitationStatus::Resolves);
        assert_eq!(r.validate("ruff", "e722"), CitationStatus::Resolves);
        assert_eq!(r.validate("ruff", "BLE001"), CitationStatus::Resolves);
    }

    // ── Every corpus-cited rule resolves ────────────────────────────────────

    #[test]
    fn corpus_cited_clippy_rules_all_resolve() {
        let r = registry();
        let rules = ["unwrap_used", "expect_used"];
        for rule in rules {
            assert_eq!(
                r.validate("clippy", rule),
                CitationStatus::Resolves,
                "clippy::{rule} should resolve"
            );
        }
    }

    #[test]
    fn corpus_cited_ruff_rules_all_resolve() {
        let r = registry();
        // E722 + BLE001 + S608 + S105/S106/S107 (Bandit B105/B106/B107 equivalents)
        for rule in ["e722", "ble001", "s608", "s105", "s106", "s107"] {
            assert_eq!(
                r.validate("ruff", rule),
                CitationStatus::Resolves,
                "ruff {rule} should resolve"
            );
        }
    }

    #[test]
    fn corpus_cited_eslint_rules_all_resolve() {
        let r = registry();
        for rule in [
            "no-var",
            "no-restricted-imports",
            "no-restricted-syntax",
            "no-restricted-globals",
            "no-restricted-properties",
        ] {
            assert_eq!(
                r.validate("eslint", rule),
                CitationStatus::Resolves,
                "eslint {rule} should resolve"
            );
        }
    }

    #[test]
    fn corpus_cited_typescript_eslint_rules_all_resolve() {
        let r = registry();
        for rule in [
            "no-explicit-any",
            "no-floating-promises",
            "no-non-null-assertion",
        ] {
            assert_eq!(
                r.validate("typescript-eslint", rule),
                CitationStatus::Resolves,
                "@typescript-eslint/{rule} should resolve"
            );
        }
    }

    #[test]
    fn corpus_cited_react_hooks_rules_all_resolve() {
        let r = registry();
        for rule in ["rules-of-hooks", "exhaustive-deps"] {
            assert_eq!(
                r.validate("react-hooks", rule),
                CitationStatus::Resolves,
                "react-hooks/{rule} should resolve"
            );
        }
    }

    #[test]
    fn corpus_cited_golangci_lint_linters_resolve() {
        let r = registry();
        for linter in ["errcheck", "staticcheck"] {
            assert_eq!(
                r.validate("golangci-lint", linter),
                CitationStatus::Resolves,
                "golangci-lint {linter} should resolve"
            );
        }
    }

    #[test]
    fn corpus_cited_rubocop_cops_resolve() {
        let r = registry();
        // RuboCop cop names are case-insensitive in the registry.
        // Canonical form is PascalCase (Style/FrozenStringLiteralComment) but
        // lookups are normalised to lowercase, so both forms resolve.
        assert_eq!(
            r.validate("rubocop", "style/frozenStringLiteralComment"),
            CitationStatus::Resolves,
            "mixed-case form should resolve via normalisation"
        );
        assert_eq!(
            r.validate("rubocop", "style/frozenstringliteralcomment"),
            CitationStatus::Resolves,
            "lowercase form should resolve"
        );
        assert_eq!(
            r.validate("rubocop", "Style/FrozenStringLiteralComment"),
            CitationStatus::Resolves,
            "PascalCase canonical form should resolve via normalisation"
        );
    }

    #[test]
    fn corpus_cited_checkstyle_checks_resolve() {
        let r = registry();
        for check in [
            "finallocalvariable",
            "finalclass",
            "visibilitymodifier",
            "avoidstarimport",
            "magicnumber",
            "annotationusestyle",
        ] {
            assert_eq!(
                r.validate("checkstyle", check),
                CitationStatus::Resolves,
                "checkstyle {check} should resolve"
            );
        }
    }

    #[test]
    fn corpus_cited_spotbugs_patterns_resolve() {
        let r = registry();
        for pattern in [
            "os_open_stream",
            "np_argument_might_be_null",
            "sql_nonconstant_string_passed_to_execute",
        ] {
            assert_eq!(
                r.validate("spotbugs", pattern),
                CitationStatus::Resolves,
                "spotbugs {pattern} should resolve"
            );
        }
    }

    #[test]
    fn corpus_cited_roslyn_ca_rules_all_resolve() {
        let r = registry();
        for rule in ["ca1001", "ca1031", "ca1068", "ca1816"] {
            assert_eq!(
                r.validate("roslyn", rule),
                CitationStatus::Resolves,
                "roslyn {rule} should resolve"
            );
        }
    }

    #[test]
    fn corpus_cited_roslyn_ide_rules_all_resolve() {
        let r = registry();
        assert_eq!(
            r.validate("roslyn-style", "ide0090"),
            CitationStatus::Resolves
        );
    }

    #[test]
    fn corpus_cited_bandit_rules_all_resolve() {
        let r = registry();
        for rule in ["b105", "b106", "b107", "b608"] {
            assert_eq!(
                r.validate("bandit", rule),
                CitationStatus::Resolves,
                "bandit {rule} should resolve"
            );
        }
    }

    // ── Tool aliases ─────────────────────────────────────────────────────────

    #[test]
    fn typescript_eslint_aliases_work() {
        let r = registry();
        assert_eq!(
            r.validate("@typescript-eslint", "no-explicit-any"),
            CitationStatus::Resolves
        );
        assert_eq!(
            r.validate("ts-eslint", "no-explicit-any"),
            CitationStatus::Resolves
        );
    }

    #[test]
    fn spotbugs_alias_findbugs_works() {
        let r = registry();
        assert_eq!(
            r.validate("findbugs", "os_open_stream"),
            CitationStatus::Resolves
        );
    }

    #[test]
    fn roslyn_alias_ca_works() {
        let r = registry();
        assert_eq!(r.validate("ca", "ca1031"), CitationStatus::Resolves);
    }

    // ── CitationStatus display ────────────────────────────────────────────────

    #[test]
    fn citation_status_display() {
        assert_eq!(CitationStatus::Resolves.to_string(), "resolves");
        assert_eq!(CitationStatus::NotFound.to_string(), "not-found");
        assert_eq!(CitationStatus::UnknownTool.to_string(), "unknown-tool");
    }

    // ── Tool keys list ────────────────────────────────────────────────────────

    #[test]
    fn tool_keys_includes_all_registered_tools() {
        let r = registry();
        let keys = r.tool_keys();
        for expected in [
            "clippy",
            "ruff",
            "eslint",
            "typescript-eslint",
            "react-hooks",
            "golangci-lint",
            "rubocop",
            "checkstyle",
            "spotbugs",
            "roslyn",
            "roslyn-style",
            "bandit",
            "sqlfluff",
        ] {
            assert!(keys.contains(&expected), "expected tool key {expected} in registry");
        }
    }
}
