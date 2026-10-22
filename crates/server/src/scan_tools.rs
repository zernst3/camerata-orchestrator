//! Scan-time deterministic PREVIEW pass (CI-security Part B).
//!
//! At onboarding scan, for each SELECTED mechanical rule that can run locally,
//! Camerata runs the rule's underlying tool ITSELF with a Camerata-supplied
//! config that enables exactly those rules, parses the output, and folds the
//! findings into triage as **preview findings**. This works EVEN IF the rule is
//! not yet wired into the repo's gate — you select it, you see findings.
//!
//! # Why preview is decoupled from the gate
//!
//! The repo is the source of truth for the GATE (layer-2/3, authoritative,
//! repo-pinned, no drift). The SCAN is an advisory preview — so it does NOT need
//! to be repo-sourced. A preview finding is NOT enforcement: the CI story still
//! must wire the rule for the gate to block on it. See
//! `docs/decisions/2026-06-22_ci_security_rules_and_scan_time_preview.md` and
//! `docs/decisions/2026-06-22_ci_scan_preview_partB.md`.
//!
//! # Deterministic, not AI
//!
//! These findings carry STABLE rule-ids (the tool's own ids), so triage treats
//! them like the deterministic floor — NOT the AI-advisory bucket. They stay OUT
//! of the LLM review entirely (no tokens). The mechanical/CI rules are already
//! dropped from the AI scan; this pass runs the deterministic tool for them.
//!
//! # The one exception
//!
//! `layer3_only` rules (CodeQL — heavy whole-program DB build) and the paid cloud
//! tiers are story-only: they NEVER preview. The caller excludes them before
//! calling [`run_scan_tools`]; this module also defends against them.
//!
//! # Honesty stance (no false clean)
//!
//! Mirrors the layer-2 runners' fail-closed posture, adapted for an ADVISORY
//! pass: a missing tool or an unrunnable rule must NEVER be reported as a clean
//! preview. Instead the pass emits a benign NOTE finding ("could not preview X —
//! enforces once wired"). A preview uses Camerata's tool version, which may differ
//! from what the repo eventually pins — the preview is indicative, the gate is
//! authoritative.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use camerata_agent::{spawn_mtime_probe, HeartbeatFn, MTIME_PROBE_INTERVAL};

use crate::onboard::{CoverageNote, Finding, SelectedRule};
use crate::tool_provisioning;
use camerata_rules::Rule;

/// The deterministic tools the scan preview can drive. Each maps to a known
/// invocation + output parser. Tools we don't fully wire degrade gracefully (a
/// NOTE finding), they don't silently vanish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ScanTool {
    /// Rust — `cargo clippy --message-format=json` with `-W <lint>` per rule.
    Clippy,
    /// Python — `ruff check --output-format json --select <code>`.
    Ruff,
    /// JS/TS — `eslint` with a SARIF formatter, rules forced on via `--rule`.
    Eslint,
    /// Polyglot — `semgrep --sarif --config <config>`.
    Semgrep,
}

impl ScanTool {
    /// The lowercase tool name carried on a preview finding's `preview_tool`.
    pub fn name(self) -> &'static str {
        match self {
            ScanTool::Clippy => "clippy",
            ScanTool::Ruff => "ruff",
            ScanTool::Eslint => "eslint",
            ScanTool::Semgrep => "semgrep",
        }
    }
}

/// Derive the scan tool a rule's findings would come from, by inspecting its
/// grounding sources' `linter` field (the corpus's tool+rule provenance), falling
/// back to [`crate::mechanical_gate::semgrep_covered_rule_ids`] when no source names
/// one.
///
/// Recognized prefixes (case-insensitive on the tool token):
/// - `clippy: ...` / `clippy::...`            -> [`ScanTool::Clippy`]
/// - `Ruff: ...` / a bare `RUF...`/`S...` code -> [`ScanTool::Ruff`]
/// - `semgrep` / `semgrep: ...`               -> [`ScanTool::Semgrep`]
/// - any `eslint`/`@typescript-eslint`/`@angular-eslint`/`eslint-plugin-*`/`vue/`
///   style id                                  -> [`ScanTool::Eslint`]
///
/// Returns `None` when no source maps to a scan tool we drive (e.g. Checkstyle,
/// RuboCop, golangci-lint, Roslyn — not wired end-to-end here; the caller emits a
/// graceful NOTE for these).
///
/// # W3 wiring-gap fix: the `semgrep_covered_rule_ids` fallback
///
/// `mechanical_gate::detector_channel` (the build-time "does this mechanical rule have
/// a real detector" gate, and the scan-time ledger's speculative pre-pass recording in
/// `onboard::audit_repos`) both treat a rule id present in
/// [`crate::mechanical_gate::semgrep_covered_rule_ids`] as "covered by Semgrep" —
/// that set is derived from the bundled `security.yml`/`taint-security.yml` rule ids
/// via `semgrep_floor_category`, INDEPENDENTLY of whether the corpus rule's own
/// `[[sources]]` names `linter = "semgrep"`. `SEC-NO-RAW-SQL-CONCAT-1` and
/// `SEC-NO-COMMAND-INJECTION-1` (among others) are claimed-covered that way but carry
/// NO `linter` source at all. Before this fallback, that meant the build gate and the
/// ledger's speculative pre-pass entry both believed Semgrep covered these (very
/// commonly selected, default-on) rules, while THIS function — the one `group_by_tool`
/// actually uses to decide whether to drive Semgrep at scan time — had no way to know
/// that and routed them to `ungrouped` ("no scan-runnable tool wired") instead. The
/// practical effect: Semgrep (and the whole `taint-security.yml` commodity layer) never
/// actually ran in EITHER the headless or the server/UI scan path, for any repo,
/// ever — `ensure_semgrep` was unreachable except via the opt-in-only
/// `CICD-SEMGREP-SECURITY-SCAN-1` rule nobody selects by default. Because the failure
/// mode was "unrouted" (a routing gap) rather than a hard tool failure, the ledger's
/// reconciliation (`reconcile_external_tool_ledger`) correctly treats a routing gap as
/// "not evidence the tool itself didn't run" and so never corrected the speculative
/// `ran = true, findings_emitted = 0` entry — a silent false "verified clean" on every
/// scan. This fallback makes the two "is this covered by Semgrep" definitions agree:
/// one `semgrep_covered_rule_ids`, consulted everywhere.
pub fn tool_for_rule(rule: &Rule) -> Option<ScanTool> {
    rule.sources
        .iter()
        .filter_map(|s| s.linter.as_deref())
        .find_map(tool_for_linter)
        .or_else(|| {
            crate::mechanical_gate::semgrep_covered_rule_ids()
                .contains(rule.id.0.as_str())
                .then_some(ScanTool::Semgrep)
        })
}

/// Whether `s` is shaped like a real lint rule identifier (`kebab-case`, optionally
/// `scope/kebab-case` or `scope/kebab_case`) rather than a prose description of a tool
/// CONVENTION. A handful of corpus `[[sources]].linter` annotations describe how a
/// tool behaves by DEFAULT rather than naming an actual rule — e.g. `jest:
/// jest.useFakeTimers() API`, `jest: testMatch default glob`, `vitest: include default
/// glob` (see `crates/rules/principles/javascript/testing/*.toml`). Handing one of
/// those to `eslint --rule` as if it were a real id would either silently no-op or
/// error; this keeps them out of both the routing decision ([`tool_for_linter`]) and
/// the generated `--rule` arguments ([`selector_for_linter`]), while leaving every
/// genuine rule id (hyphens, underscores, scope slashes, `@scope` prefixes) untouched.
fn looks_like_lint_rule_id(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/'))
}

/// Map a single `linter` source string to a scan tool. Pure; the core of the
/// linter-source -> tool grouping that the tests pin.
pub fn tool_for_linter(linter: &str) -> Option<ScanTool> {
    let trimmed = linter.trim();
    let lower = trimmed.to_ascii_lowercase();
    // The "tool token" is the bit before the first `:` or `::` separator.
    let token = lower.split([':'].as_ref()).next().unwrap_or(&lower).trim();

    if token == "semgrep" {
        return Some(ScanTool::Semgrep);
    }
    if token == "clippy" || token.starts_with("clippy::") {
        return Some(ScanTool::Clippy);
    }
    if token == "ruff" {
        return Some(ScanTool::Ruff);
    }
    // eslint family: bare `eslint`, scoped plugins (`@typescript-eslint`,
    // `@angular-eslint`), `eslint-plugin-*`, the `vue/` rule namespace enforced via
    // eslint-plugin-vue, and a few named plugin scopes the corpus grounds specific
    // rules against today (`react-hooks` — eslint-plugin-react-hooks; `jest` —
    // eslint-plugin-jest).
    let is_eslint_family = token == "eslint"
        || token.starts_with("eslint-")
        || token.starts_with("@typescript-eslint")
        || token.starts_with("@angular-eslint")
        || token.starts_with("vue/")
        || token == "react-hooks"
        || token == "jest";
    if !is_eslint_family {
        return None;
    }
    // No-colon forms (`@angular-eslint/prefer-inject`) are already fully-qualified
    // rule ids; trust them as before — there is no "after the colon" to validate.
    let Some((_, after)) = trimmed.split_once(':') else {
        return Some(ScanTool::Eslint);
    };
    // A colon-separated source (`token: after`) must have an `after` shaped like a
    // real rule id. Several `jest:` sources in the corpus describe a CONVENTION the
    // tool enforces by its own defaults, not a lint rule — those must never be
    // claimed as eslint-routable (see `looks_like_lint_rule_id`'s doc comment).
    if looks_like_lint_rule_id(after.trim()) {
        Some(ScanTool::Eslint)
    } else {
        None
    }
}

/// The tool-specific rule SELECTOR token derived from a `linter` source, used to
/// build the tool's `--select`/`-W`/`--rule` config so the preview enables exactly
/// the selected rules. Returns the bit AFTER the tool token (the rule id), trimmed.
///
/// Examples:
/// - `"Ruff: S608"`            -> `"S608"`
/// - `"clippy: unwrap_used"`   -> `"unwrap_used"`
/// - `"eslint: eqeqeq"`        -> `"eqeqeq"`
/// - `"@typescript-eslint: no-explicit-any"` -> `"@typescript-eslint/no-explicit-any"`
/// - `"react-hooks: exhaustive-deps"` -> `"react-hooks/exhaustive-deps"`
/// - `"jest: no-conditional-expect"` -> `"jest/no-conditional-expect"`
/// - `"eslint-plugin-vue: vue/component-api-style"` -> `"vue/component-api-style"`
///   (the corpus already wrote the rule in fully-qualified form after the colon —
///   NOT re-prefixed to `vue/vue/component-api-style`)
/// - `"semgrep"`               -> `None` (semgrep selects by config pack, not id)
/// - `"jest: jest.useFakeTimers() API"` -> `None` (a convention, not a rule id)
pub fn selector_for_linter(linter: &str) -> Option<String> {
    let trimmed = linter.trim();
    let tool = tool_for_linter(trimmed)?;
    if tool == ScanTool::Semgrep {
        return None;
    }
    // Split on the first `:` (the corpus convention is `Tool: rule-id`).
    let after = trimmed
        .split_once(':')
        .map(|(_, after)| after.trim())
        .unwrap_or("");
    if after.is_empty() {
        // No `:` separator — the whole token IS the rule id for some eslint
        // plugins recorded as `@angular-eslint/prefer-inject` with no colon.
        if tool == ScanTool::Eslint {
            return Some(trimmed.to_string());
        }
        return None;
    }
    if tool == ScanTool::Eslint && !looks_like_lint_rule_id(after) {
        // `tool_for_linter` already filters most of these at the routing stage, but
        // this guard is defensive — a rule id this function is ever asked about must
        // never be handed to `eslint --rule` unless it is actually shaped like one.
        return None;
    }
    // eslint scoped plugins record `@typescript-eslint: no-explicit-any`; the real
    // eslint rule id is `@typescript-eslint/no-explicit-any`. Likewise `react-hooks:
    // exhaustive-deps` -> `react-hooks/exhaustive-deps` and `jest:
    // no-conditional-expect` -> `jest/no-conditional-expect`.
    let lower = trimmed.to_ascii_lowercase();
    if tool == ScanTool::Eslint
        && (lower.starts_with("@typescript-eslint")
            || lower.starts_with("@angular-eslint")
            || lower.starts_with("eslint-plugin")
            || lower.starts_with("react-hooks:")
            || lower.starts_with("jest:"))
    {
        let scope = trimmed.split(':').next().unwrap_or("").trim();
        // eslint-plugin-foo: rule  ->  foo/rule ; @scope: rule -> @scope/rule
        let scope = scope.strip_prefix("eslint-plugin-").unwrap_or(scope);
        // Guard against double-prefixing when the corpus already wrote the rule in
        // fully-qualified `scope/rule` form after the colon (the convention used by
        // the Vue sources: `eslint-plugin-vue: vue/component-api-style`) — keep it
        // as-is rather than producing `vue/vue/component-api-style`.
        if after.starts_with(&format!("{scope}/")) {
            return Some(after.to_string());
        }
        return Some(format!("{scope}/{after}"));
    }
    Some(after.to_string())
}

/// A note that the preview could not be produced for a tool — surfaced as a
/// benign info-severity preview finding rather than swallowed (never a false
/// clean). Pulled out so the caller and tests share one shape.
/// The npm PACKAGE that must be present in the provisioned eslint workspace for
/// `selector`'s plugin namespace to resolve, or `None` when `selector` is a CORE
/// eslint rule (`eqeqeq`, `no-var`) that needs nothing beyond eslint itself.
///
/// # Why this check exists (W6 / C11)
///
/// `--rule '{"@typescript-eslint/no-explicit-any": "error"}'` is not a per-rule
/// risk: an unregistered namespace is a FATAL CONFIG ERROR that aborts the entire
/// eslint invocation ("A configuration object specifies rule X, but could not find
/// plugin Y"), writing nothing to stdout. One unprovisioned plugin therefore takes
/// down the preview for every OTHER selected rule in the same run, including the
/// plain core rules that would have worked. Partitioning the selectors up front
/// (see [`partition_eslint_selectors`]) turns that into a disclosed gap for the one
/// affected rule instead of a silent zero for all of them.
///
/// The package name follows eslint's own plugin-naming convention
/// (`naming.normalizePackageName`), so it is derived rather than table-driven — a
/// corpus rule citing a plugin Camerata has never heard of still gets the right
/// answer instead of being mistaken for a core rule:
///
/// - `eqeqeq`                             → `None` (core)
/// - `react-hooks/exhaustive-deps`        → `eslint-plugin-react-hooks`
/// - `@typescript-eslint/no-explicit-any` → `@typescript-eslint/eslint-plugin`
/// - `@scope/foo/some-rule`               → `@scope/eslint-plugin-foo`
pub fn eslint_plugin_package_for_selector(selector: &str) -> Option<String> {
    let parts: Vec<&str> = selector.split('/').filter(|s| !s.is_empty()).collect();
    if selector.starts_with('@') {
        match parts.len() {
            // `@scope` alone is not a rule id at all; nothing to resolve.
            0 | 1 => None,
            // `@scope/rule` — the namespace IS the scope, package `@scope/eslint-plugin`.
            2 => Some(format!("{}/eslint-plugin", parts[0])),
            // `@scope/plugin/rule` — package `@scope/eslint-plugin-<plugin>`.
            _ => Some(format!("{}/eslint-plugin-{}", parts[0], parts[1])),
        }
    } else if parts.len() >= 2 {
        Some(format!("eslint-plugin-{}", parts[0]))
    } else {
        // No namespace separator: a core eslint rule.
        None
    }
}

/// `true` when `package` is installed in the provisioned eslint workspace's
/// `node_modules`. Probes `package.json` rather than the directory so a
/// half-removed package (empty dir left behind by an interrupted `npm install`)
/// reads as ABSENT — the conservative direction, since a present-but-broken
/// package is the fatal case we are avoiding.
fn eslint_package_present(workspace: &Path, package: &str) -> bool {
    workspace
        .join("node_modules")
        .join(package)
        .join("package.json")
        .exists()
}

/// Split eslint rule selectors into the ones that can safely be handed to
/// `--rule` for this provisioned workspace, and the ones whose plugin package is
/// missing — returned as `(runnable, unresolvable)` where each `unresolvable`
/// entry is `(selector, missing_package)`.
///
/// See [`eslint_plugin_package_for_selector`] for why an unresolvable selector
/// must be withheld rather than attempted.
pub fn partition_eslint_selectors(
    selectors: &[String],
    workspace: &Path,
) -> (Vec<String>, Vec<(String, String)>) {
    let mut runnable = Vec::new();
    let mut unresolvable = Vec::new();
    for sel in selectors {
        match eslint_plugin_package_for_selector(sel) {
            None => runnable.push(sel.clone()),
            Some(pkg) if eslint_package_present(workspace, &pkg) => runnable.push(sel.clone()),
            Some(pkg) => unresolvable.push((sel.clone(), pkg)),
        }
    }
    (runnable, unresolvable)
}

/// The exact `--format` value Camerata passes to eslint: one of eslint's own
/// BUILT-IN formatter names, shipped inside the `eslint` package.
///
/// This is a named constant rather than an inline string so the C11 regression
/// test can assert the invariant directly: the formatter must never again be an
/// npm PACKAGE name, because eslint resolves a non-built-in `--format` relative
/// to its cwd — the scanned repo — where Camerata's tooling packages do not
/// exist. See [`parse_eslint_json`]'s doc comment for the post-mortem.
pub const ESLINT_FORMATTER: &str = "json";

/// Build the full eslint argument vector for one scan invocation: the bundled
/// flat config, the built-in JSON formatter, one `--rule` override per runnable
/// selector, and `.` as the target.
///
/// Pure and separated from [`run_one_tool`]'s I/O purely so the invariant "we
/// never pass an npm formatter package" is assertable in a unit test instead of
/// only observable by running a real eslint against a real provisioned workspace.
pub fn eslint_args(config_path: &str, runnable_selectors: &[String]) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "--config".to_string(),
        config_path.to_string(),
        "--format".to_string(),
        ESLINT_FORMATTER.to_string(),
        // The bundled config carries a "warn"-level baseline so eslint always has a coherent
        // base; a selected rule is passed below at "error". `--quiet` drops every warning at the
        // source, so the baseline cannot reach the output (see also
        // [`keep_selected_eslint_findings`], which enforces the same on what is parsed).
        "--quiet".to_string(),
    ];
    for sel in runnable_selectors {
        args.push("--rule".to_string());
        args.push(format!("{{\"{sel}\": \"error\"}}"));
    }
    args.push(".".to_string());
    args
}

/// Keep only the eslint findings whose rule id is one of the `selected` selectors (the
/// runnable set handed to `--rule`).
///
/// # Why this exists
///
/// Camerata's bundled eslint config enables a baseline of core rules (`no-console`,
/// `eqeqeq`, `no-var`, `prefer-const`, ...). Every message eslint printed used to become a
/// preview [`Finding`], so a scan that selected only the React Hooks rules still shipped
/// `no-console` and `eqeqeq` rows into `report.findings` and from there into every client
/// artifact, and `eqeqeq` / `no-var` / `prefer-const` even grounded to
/// `JAVASCRIPT-STRICT-EQUALITY-1`, `JAVASCRIPT-NO-VAR-1` and `JAVASCRIPT-CONST-DEFAULT-1`
/// with their citations, selected or not. The scan reports what the project selected.
pub fn keep_selected_eslint_findings(findings: Vec<Finding>, selected: &[String]) -> Vec<Finding> {
    findings
        .into_iter()
        .filter(|f| selected.iter().any(|s| s == &f.rule_id))
        .collect()
}

pub fn note_finding(repo: &str, tool: &str, message: impl Into<String>) -> Finding {
    let message = message.into();
    Finding {
        repo: repo.to_string(),
        path: "(scan preview)".to_string(),
        line: 0,
        rule_id: format!("PREVIEW-NOTE-{}", tool.to_ascii_uppercase()),
        severity: "info".to_string(),
        snippet: String::new(),
        detail: message.clone(),
        // Info notes are not enforced — keep them out of the active/enforced set.
        status: "suppressed-baseline".to_string(),
        preview: true,
        preview_tool: Some(tool.to_string()),
        // A5.6: "the tool could not run" is an honesty row, structurally distinguishable
        // from both a fault and "assessed and clean".
        kind: Some(camerata_api_types::audit::FindingKind::NotAssessed(
            camerata_api_types::audit::NotAssessedKind {
                reason: camerata_api_types::audit::NotAssessedReason::ToolFailed,
                reason_detail: message,
            },
        )),
        ..Finding::default()
    }
}

// ─── stack-aware language gating ─────────────────────────────────────────────

/// Derive the set of languages PRESENT in a file list by mapping each file
/// extension to a normalised language label. The label strings match what
/// [`crate::onboard::propose::lang_for_ext`] returns; they are case-sensitive
/// (e.g. `"Rust"`, `"Python"`, `"JavaScript"`, `"TypeScript"`).
///
/// Pure; used by the stack-gating predicate so the test suite can drive it
/// without touching the filesystem.
pub fn languages_from_files(files: &[(String, String)]) -> HashSet<String> {
    files
        .iter()
        .filter_map(|(path, _)| crate::onboard::propose::lang_for_ext(path))
        .map(|l| l.to_string())
        .collect()
}

/// Return `true` if `tool` should run given the set of languages PRESENT in the
/// repo being scanned. A tool whose required language is absent is **omitted**
/// from the run (and from the pre-declared tool count) — it must NOT appear as a
/// passing "✓ 0" on a stack that has no such files.
///
/// Language membership rules (each tool gates on at least one language):
/// - `Clippy`  → Rust present
/// - `Ruff`    → Python present
/// - `Eslint`  → JavaScript OR TypeScript present
/// - `Semgrep` → any semgrep-supported language present (Python, JS, TS, Go,
///   Java, Ruby, Rust, C#, PHP, C, C++); passes if present_languages
///   is empty (unknown / can't derive → run conservatively).
///
/// When `present_languages` is `None`, all tools pass (backward-compat: callers
/// that haven't threaded language info through yet don't regress).
pub fn tool_languages_present(tool: ScanTool, present: Option<&HashSet<String>>) -> bool {
    let Some(langs) = present else {
        return true; // no language info → don't gate
    };
    // If we couldn't derive any languages (e.g. empty repo or all binary files)
    // be conservative: let all tools through rather than silently omitting them.
    if langs.is_empty() {
        return true;
    }
    match tool {
        ScanTool::Clippy => langs.contains("Rust"),
        ScanTool::Ruff => langs.contains("Python"),
        ScanTool::Eslint => langs.contains("JavaScript") || langs.contains("TypeScript"),
        ScanTool::Semgrep => {
            // Semgrep supports a broad polyglot set; gate on any recognized language
            // from that set being present. If the language list is non-empty but
            // contains NONE of these, the repo is e.g. pure SQL/Kotlin/Swift — do
            // not run semgrep (would produce a misleading "✓ 0").
            const SEMGREP_LANGS: &[&str] = &[
                "Python",
                "JavaScript",
                "TypeScript",
                "Go",
                "Java",
                "Ruby",
                "Rust",
                "C#",
                "PHP",
                "C",
                "C++",
            ];
            SEMGREP_LANGS.iter().any(|l| langs.contains(*l))
        }
    }
}

/// Group the SELECTED mechanical rules by the scan tool that would produce their
/// findings, dropping `layer3_only` rules (CodeQL / paid tiers never preview) and
/// any rule whose tool we can't derive. Returns `(by_tool, ungrouped)` where
/// `ungrouped` holds rule ids we recognized as mechanical but couldn't route to a
/// driven tool (the caller emits a graceful note for these).
///
/// Pure over the corpus: takes a `lookup` resolving a rule id to its corpus
/// [`Rule`] (the real caller passes `|id| set.get_by_id(id)`), not I/O. Taking a
/// closure rather than the `RuleSet` lets the unit tests drive this with
/// hand-built [`Rule`]s (whose fields are public) without the private `RuleSet`
/// constructor.
///
/// `present_languages` — when `Some`, tools whose required language is absent from
/// the set are omitted entirely (stack-gating: a Rust-only repo won't have eslint
/// or ruff in the output, even if a JS rule was selected). When `None`, no gating
/// (all tools pass; backward-compat).
pub fn group_by_tool<'a, 'r>(
    selected: &'a [SelectedRule],
    lookup: &(dyn Fn(&str) -> Option<&'r Rule> + Send + Sync),
    present_languages: Option<&HashSet<String>>,
) -> (
    BTreeMap<ScanTool, Vec<&'a SelectedRule>>,
    Vec<&'a SelectedRule>,
) {
    let mut by_tool: BTreeMap<ScanTool, Vec<&SelectedRule>> = BTreeMap::new();
    let mut ungrouped: Vec<&SelectedRule> = Vec::new();

    for sr in selected {
        let Some(rule) = lookup(&sr.id) else {
            // Not in the corpus — can't derive a tool; let the caller note it.
            ungrouped.push(sr);
            continue;
        };
        // Architectural rules have no off-the-shelf linter (they need a custom AST checker),
        // so the preview cannot run them. They remain covered by the AI review (advisory).
        // Only MECHANICAL rules attempt the preview.
        if rule.enforcement != camerata_rules::EnforcementKind::Mechanical || rule.is_layer3_only()
        {
            continue;
        }
        match tool_for_rule(rule) {
            Some(tool) if tool_languages_present(tool, present_languages) => {
                by_tool.entry(tool).or_default().push(sr)
            }
            Some(_tool) => {
                // Tool's language is absent from the repo — silently omit (no note,
                // no false-clean). The stack gates, regardless of rule selection.
            }
            None => ungrouped.push(sr),
        }
    }

    (by_tool, ungrouped)
}

/// Derive the distinct tool-name strings that `run_scan_tools` WOULD register on the
/// job for the given rule selection, WITHOUT running any tool.  Used by the pre-declaration
/// step in `onboard_audit_start` so the job can show the correct "N" before any tool
/// executes.
///
/// `present_languages` must be the SAME set passed to `run_scan_tools` so the
/// pre-declared "N" matches the stack-gated tools that actually run. When `None`,
/// no stack-gating is applied (backward-compat).
///
/// Returns a `Vec<String>` of tool names in stable order (sorted, then "unrouted" last
/// when applicable).  The result mirrors exactly what `run_scan_tools` would call
/// `det_register_tool` with, so the pre-declared total always matches what the live
/// pass fills in.
pub fn preview_tool_ids_for_rules<'r>(
    selected: &[SelectedRule],
    lookup: &(dyn Fn(&str) -> Option<&'r Rule> + Send + Sync),
    present_languages: Option<&HashSet<String>>,
) -> Vec<String> {
    let (by_tool, ungrouped) = group_by_tool(selected, lookup, present_languages);
    let mut names: Vec<String> = by_tool.keys().map(|t| t.name().to_string()).collect();
    if !ungrouped.is_empty() {
        names.push("unrouted".to_string());
    }
    names
}

// ─── output parsers (pure, fixture-tested) ───────────────────────────────────

/// Severity normalized to the `Finding.severity` vocabulary (`high`/`medium`/
/// `low`/`info`), from a tool's own severity string. Conservative default:
/// `medium` (a preview is advisory; don't over- or under-state it).
/// W24 — normalise a tool's raw level, then cap it by what that tool's severity MEANS.
///
/// `norm_severity` maps "error" to `high`. For Semgrep that is correct: it is a security
/// scanner and its error level is a security judgement. For a STYLE LINTER it is not —
/// ESLint and Clippy "error" means the project chose to fail its build on that lint, which
/// is a strictness preference and says nothing about exploitability.
///
/// The 2026-10-18 hearth export shipped 31 ESLint preview rows at HIGH, outranking every
/// planted security finding in the queue. The rules were `no-non-null-assertion`,
/// `no-explicit-any`, `react-hooks/exhaustive-deps` and `no-console` — all real code-quality
/// signals, none of them a HIGH, and all of them sitting above a live exposure.
///
/// Ruff and Semgrep are deliberately NOT capped. Semgrep is a security scanner outright, and
/// Ruff carries the bandit-derived S-prefix family (S608 hardcoded SQL and friends), so
/// neither is a pure style linter and a blanket cap would bury real security findings. The
/// line is drawn on what the tool's severity is ABOUT, not on whether it is a "linter".
fn capped_preview_severity(tool: ScanTool, raw: &str) -> String {
    let normalised = norm_severity(raw);
    match tool {
        ScanTool::Eslint | ScanTool::Clippy if normalised == "high" => "medium".to_string(),
        _ => normalised,
    }
}

fn norm_severity(s: &str) -> String {
    match s.trim().to_ascii_lowercase().as_str() {
        "error" | "high" | "critical" | "blocker" => "high",
        "warning" | "warn" | "medium" | "moderate" => "medium",
        "note" | "info" | "information" | "low" | "hint" => "low",
        _ => "medium",
    }
    .to_string()
}

/// Normalize a raw semgrep rule id emitted by `semgrep --sarif` into the clean,
/// portable id stored in security.yml.
///
/// When semgrep is invoked with an absolute `--config` path (as Camerata does when
/// scanning an external repository), it prefixes every rule id with the config path,
/// dotted. For example, scanning `/repos/myrepo` with
/// `--config /Users/alice/camerata/tooling/semgrep-rules` produces:
///
/// ```text
/// Users.alice.camerata.tooling.semgrep-rules.camerata.security.sql-string-concat-rust
/// ```
///
/// The portable id we register in `finding_security_category` and display in the UI is
/// `camerata.security.sql-string-concat-rust` — the suffix starting at `camerata.security.`.
///
/// This function strips the path prefix: if the raw id contains the sentinel
/// `camerata.security.` it returns everything from that sentinel onward. Otherwise the id
/// is returned unchanged (non-Camerata semgrep rules, already-clean ids).
///
/// # Examples (as unit tests — see `#[cfg(test)]` block below)
///
/// - `"Users.alice.camerata.tooling.semgrep-rules.camerata.security.sql-string-concat-rust"`
///   → `"camerata.security.sql-string-concat-rust"`
/// - `"camerata.security.hardcoded-secret"` → unchanged
/// - `"python.lang.security.audit.exec-detected"` → unchanged
pub fn normalize_semgrep_rule_id(raw: &str) -> String {
    const SENTINEL: &str = "camerata.security.";
    if let Some(pos) = raw.find(SENTINEL) {
        raw[pos..].to_string()
    } else {
        raw.to_string()
    }
}

/// Parse a SARIF 2.x document (semgrep `--sarif`, eslint via a SARIF formatter)
/// into preview [`Finding`]s. SARIF is the preferred format: stable rule ids in
/// `result.ruleId`, location in `physicalLocation.region.startLine`.
///
/// Best-effort: a malformed doc yields `Ok(vec![])` from the caller's view (we
/// return `Err` only on unparseable JSON, which the caller turns into a note).
pub fn parse_sarif(repo: &str, tool: ScanTool, json: &str) -> anyhow::Result<Vec<Finding>> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    let mut out = Vec::new();
    let Some(runs) = v.get("runs").and_then(|r| r.as_array()) else {
        return Ok(out);
    };
    for run in runs {
        let Some(results) = run.get("results").and_then(|r| r.as_array()) else {
            continue;
        };
        for res in results {
            let rule_id_raw = res
                .get("ruleId")
                .and_then(|r| r.as_str())
                .unwrap_or("(unknown)");
            // Semgrep prefixes rule ids with the (absolute) --config path when
            // the config is given as an absolute directory.  Strip the prefix so
            // the stored id is always the clean portable form
            // (`camerata.security.<name>`). Non-semgrep tools and already-clean
            // ids are returned unchanged.
            let rule_id = if matches!(tool, ScanTool::Semgrep) {
                normalize_semgrep_rule_id(rule_id_raw)
            } else {
                rule_id_raw.to_string()
            };
            let message = res
                .get("message")
                .and_then(|m| m.get("text"))
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            let level = res
                .get("level")
                .and_then(|l| l.as_str())
                .unwrap_or("warning");
            // First physical location.
            let (path, line) = res
                .get("locations")
                .and_then(|l| l.as_array())
                .and_then(|a| a.first())
                .and_then(|loc| loc.get("physicalLocation"))
                .map(|pl| {
                    let path = pl
                        .get("artifactLocation")
                        .and_then(|al| al.get("uri"))
                        .and_then(|u| u.as_str())
                        .unwrap_or("(repo)")
                        .to_string();
                    let line = pl
                        .get("region")
                        .and_then(|r| r.get("startLine"))
                        .and_then(|n| n.as_u64())
                        .unwrap_or(0) as usize;
                    (path, line)
                })
                .unwrap_or_else(|| ("(repo)".to_string(), 0));
            out.push(preview_finding(
                repo, tool, &path, line, &rule_id, level, &message,
            ));
        }
    }
    Ok(out)
}

/// How many files eslint reported, and how many of them it could not PARSE —
/// returned alongside the findings by [`parse_eslint_json`] so the caller can tell
/// "ran and found nothing" apart from "ran, but could not read a single file".
///
/// The second case is a real silent-degradation shape: with the TypeScript parser
/// missing (see `assets/eslint/camerata.config.mjs`'s note 2 for the path bug that
/// caused exactly this), eslint happily processes every `.ts` file and reports a
/// `fatal` parse error for each, with ZERO rule findings. Exit status, stdout
/// validity and finding count are all indistinguishable from a genuinely clean
/// TypeScript repo. Counting it is the only way to notice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EslintRunShape {
    pub files_reported: usize,
    pub files_unparseable: usize,
}

impl EslintRunShape {
    /// `true` when eslint reported at least one file and could not parse ANY of
    /// them — the "ran but read nothing" shape described on the struct. A run with
    /// zero files reported is NOT this shape (that is an ordinary "no matching
    /// files", already disclosed elsewhere).
    pub fn read_nothing(&self) -> bool {
        self.files_reported > 0 && self.files_unparseable == self.files_reported
    }
}

/// Parse `eslint --format json` into preview [`Finding`]s.
///
/// # Why the BUILT-IN json formatter and not SARIF (W6 / C11)
///
/// The eslint arm used to request `--format @microsoft/eslint-formatter-sarif` and
/// feed the result to [`parse_sarif`]. That formatter is an npm package, and eslint
/// resolves a non-built-in `--format` name relative to its own **cwd** — which, for
/// a scan, is the SCANNED REPO, not Camerata's provisioned tooling workspace where
/// the package actually lives. Resolution therefore failed on every repo, eslint
/// fell back to its built-in formatter directory, the import failed there too, and
/// it exited 2 having written the explanation to stderr and NOTHING to stdout. The
/// resulting `serde_json` error — `EOF while parsing a value at line 1 column 0` —
/// is the C11 symptom, and it was never an eslint-config problem at all.
///
/// `json` is one of eslint's BUILT-IN formatters, shipped inside the eslint package
/// itself. It needs no module resolution from any cwd, so the entire failure class
/// is gone rather than worked around. It also carries two things SARIF drops: the
/// per-file `fatal` flag (see [`EslintRunShape`]) and the full file list, including
/// files with no findings.
///
/// Shape: a flat array of per-file objects, `{ filePath, messages: [ { ruleId,
/// severity, message, line } ] }`, where `severity` is `2` (error) or `1` (warning)
/// and a `ruleId` of `null` marks a message eslint produced on its own behalf (a
/// parse error, or an unused-disable-directive report) rather than a rule firing.
/// Those are counted, never emitted as findings — a parse error is not a finding
/// about the client's code, and attributing one to a rule id would be a fabrication.
pub fn parse_eslint_json(repo: &str, json: &str) -> anyhow::Result<(Vec<Finding>, EslintRunShape)> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    let mut out = Vec::new();
    let mut shape = EslintRunShape::default();
    let Some(files) = v.as_array() else {
        return Ok((out, shape));
    };
    for file in files {
        shape.files_reported += 1;
        let path = file
            .get("filePath")
            .and_then(|p| p.as_str())
            .unwrap_or("(repo)");
        let messages = file
            .get("messages")
            .and_then(|m| m.as_array())
            .map(|a| a.as_slice())
            .unwrap_or(&[]);
        let mut fatal_here = false;
        for msg in messages {
            // `fatal: true` is eslint's own parse failure for this file.
            if msg.get("fatal").and_then(|f| f.as_bool()).unwrap_or(false) {
                fatal_here = true;
                continue;
            }
            // A null/absent ruleId is eslint speaking for itself, not a rule
            // finding — never fabricate a rule id for it.
            let Some(rule_id) = msg.get("ruleId").and_then(|r| r.as_str()) else {
                continue;
            };
            let severity = match msg.get("severity").and_then(|s| s.as_u64()) {
                Some(2) => "error",
                Some(1) => "warning",
                _ => "warning",
            };
            let message = msg
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string();
            let line = msg.get("line").and_then(|l| l.as_u64()).unwrap_or(0) as usize;
            out.push(preview_finding(
                repo,
                ScanTool::Eslint,
                path,
                line,
                rule_id,
                severity,
                &message,
            ));
        }
        if fatal_here {
            shape.files_unparseable += 1;
        }
    }
    Ok((out, shape))
}

/// Parse `ruff check --output-format json` into preview [`Finding`]s. Ruff emits a
/// flat JSON array of diagnostics: `code`, `message`, `filename`, `location.row`.
pub fn parse_ruff_json(repo: &str, json: &str) -> anyhow::Result<Vec<Finding>> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    let mut out = Vec::new();
    let Some(arr) = v.as_array() else {
        return Ok(out);
    };
    for d in arr {
        let code = d.get("code").and_then(|c| c.as_str()).unwrap_or("(ruff)");
        let message = d.get("message").and_then(|m| m.as_str()).unwrap_or("");
        let path = d
            .get("filename")
            .and_then(|f| f.as_str())
            .unwrap_or("(repo)");
        let line = d
            .get("location")
            .and_then(|l| l.get("row"))
            .and_then(|r| r.as_u64())
            .unwrap_or(0) as usize;
        // Ruff's `S*` (flake8-bandit) are security; treat as medium by default —
        // the preview is advisory, severity is indicative.
        out.push(preview_finding(
            repo,
            ScanTool::Ruff,
            path,
            line,
            code,
            "medium",
            message,
        ));
    }
    Ok(out)
}

/// Parse `cargo clippy --message-format=json` into preview [`Finding`]s. Clippy
/// emits NDJSON (one JSON object per line); the relevant ones are
/// `{"reason":"compiler-message","message":{...}}` whose `code.code` is the lint
/// id (`clippy::unwrap_used`), with `level` and a primary span.
pub fn parse_clippy_json(repo: &str, ndjson: &str) -> anyhow::Result<Vec<Finding>> {
    let mut out = Vec::new();
    for raw in ndjson.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            // Skip non-JSON lines (cargo prints human status lines too); a single
            // bad line must not abort the whole parse.
            Err(_) => continue,
        };
        if v.get("reason").and_then(|r| r.as_str()) != Some("compiler-message") {
            continue;
        }
        let Some(msg) = v.get("message") else {
            continue;
        };
        let code = msg
            .get("code")
            .and_then(|c| c.get("code"))
            .and_then(|c| c.as_str());
        // Only surface lints with a code (skip codeless notes/help).
        let Some(code) = code else { continue };
        let level = msg
            .get("level")
            .and_then(|l| l.as_str())
            .unwrap_or("warning");
        let text = msg
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        // The primary span (is_primary) carries the file + line.
        let (path, line_no) = msg
            .get("spans")
            .and_then(|s| s.as_array())
            .and_then(|spans| {
                spans
                    .iter()
                    .find(|s| {
                        s.get("is_primary")
                            .and_then(|p| p.as_bool())
                            .unwrap_or(false)
                    })
                    .or_else(|| spans.first())
            })
            .map(|sp| {
                let path = sp
                    .get("file_name")
                    .and_then(|f| f.as_str())
                    .unwrap_or("(repo)")
                    .to_string();
                let line = sp.get("line_start").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
                (path, line)
            })
            .unwrap_or_else(|| ("(repo)".to_string(), 0));
        out.push(preview_finding(
            repo,
            ScanTool::Clippy,
            &path,
            line_no,
            code,
            level,
            &text,
        ));
    }
    Ok(out)
}

/// Build one preview [`Finding`] with the shared shape: `preview = true`, the
/// tool recorded, a snippet that names the tool/rule honestly, and a client-readable
/// detail (never internal gate vocabulary) so it is honest wherever it surfaces.
///
/// `status`: left as `"suppressed-baseline"` for back-compat with the internal
/// active/suppressed accounting several OTHER consumers do over this string (e.g.
/// `render_scan_results_for_chat`'s "N active (enforced), M suppressed" line, which — like
/// this finding — is correct in calling a preview row "not active/enforced"). This does
/// NOT mean, and must never be read to mean, "a prior run accepted this exact finding into
/// a committed baseline" — no such record exists for a scan-time preview row; the ONLY
/// legitimate producer of that specific claim is `onboard::audit::classify_repo_findings`,
/// which stamps this same string strictly when a finding's content fingerprint matches an
/// entry actually present in `.camerata/baseline.json`, and which never sees preview
/// findings (they are appended to the report AFTER that pass runs). The CLIENT-facing
/// disposition layer (`report_export::classify`) is the one place this ambiguity could
/// have mattered, and it is guarded there: a `preview == true` row can never classify as
/// `Disposition::BaselineAccepted` regardless of this field's value. See `classify`'s doc
/// comment for the full defect history (a preview row used to render "pre-existing accepted
/// debt (baseline suppression)" on a repo nobody had ever triaged).
fn preview_finding(
    repo: &str,
    tool: ScanTool,
    path: &str,
    line: usize,
    rule_id: &str,
    severity: &str,
    message: &str,
) -> Finding {
    let tool_name = tool.name();
    // W24 — the ONE place a preview row's severity is decided, normalisation and tool cap
    // together. It used to be `norm_severity` applied at each of the three parser call
    // sites, which left no single point that knew both the raw level AND which tool produced
    // it; the cap below needs both.
    let severity = capped_preview_severity(tool, severity);
    let severity = severity.as_str();
    let detail = if message.is_empty() {
        format!(
            "Found by {tool_name} during this scan ({rule_id}). This check has not yet been \
             added to your CI pipeline, so it does not block builds on its own yet."
        )
    } else {
        format!(
            "{message} (found by {tool_name} during this scan; not yet part of your CI \
             pipeline)."
        )
    };
    Finding {
        repo: repo.to_string(),
        path: path.to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: severity.to_string(),
        snippet: message.chars().take(160).collect(),
        detail,
        // A preview is advisory, not an enforced/active gate hit. See this function's own
        // doc comment above for why this is NOT a baseline-acceptance claim.
        status: "suppressed-baseline".to_string(),
        preview: true,
        preview_tool: Some(tool.name().to_string()),
        // A5.6: an external tool's reported violation is an objective fault.
        kind: Some(camerata_api_types::audit::FindingKind::Defect),
        ..Finding::default()
    }
}

// ─── repo-scope filter ───────────────────────────────────────────────────────

/// Return `true` when `finding_path` belongs to the scanned repo's OWN source
/// tree and should be included in the preview results.  Return `false` to DROP
/// the finding.
///
/// Rules (applied in order; first match wins):
///
/// 1. **Special / synthetic placeholder** — paths like `"(repo)"` or
///    `"(scan preview)"` that our own parsers inject when no real path was
///    present are always kept.
/// 2. **Relative path** — resolved against `repo_root`.  A linter that runs
///    with `current_dir = repo_root` emits repo-relative paths, so any
///    relative path is by definition inside the repo. Kept (subject to the
///    generated-file rules below).
/// 3. **Absolute outside the repo** — dropped.  Covers `/rustc/<hash>/…`,
///    `~/.cargo/…`, `~/.rustup/…`, `/usr/…`, and any other system path.
/// 4. **Build output** — `<repo_root>/target/` (Cargo), dropped.
/// 5. **Generated files** — any path component matching `out` directly under
///    a `build/…` segment (Cargo `OUT_DIR` pattern), or a file whose name
///    ends with `_generated.rs` / `.generated.<ext>`, dropped.
/// 6. **Bundled dist** — paths containing `/dist/` or `/dist-server/`
///    components (bundled JS/CSS, server bundles), dropped.
/// 7. Everything else within the repo: kept.
///
/// Rewrite an external tool's reported path to be REPO-RELATIVE, with forward slashes.
///
/// # The defect this exists to fix (W21)
///
/// External linters are invoked with absolute paths and report absolute paths back. Nothing
/// rewrote them, so every ESLint-preview row shipped the auditor's own filesystem layout to
/// the client. Measured on the 2026-10-18 hearth export: **32 of 237 rows** carried a path
/// like `/Users/<auditor>/Documents/Repos/Camerata tests/hearth/app/admin/page.tsx`, in the
/// PDF and the workbook both. Every one of them was `Preview: eslint`; no other tier leaked.
///
/// Two things are wrong with that beyond untidiness. It tells the client the auditor's
/// username, directory habits and where else their work lives, none of which is theirs to
/// know. And it makes the path useless to them: they cannot open it, search for it, or paste
/// it into their own tooling, because the prefix describes a machine they have no access to.
///
/// [`is_in_repo_scope`] already computed exactly this relative path in order to make its
/// keep/drop decision and then threw it away. This returns it instead.
///
/// A path that is already relative, or that is a synthetic placeholder like `"(repo)"`, or
/// that lies outside `repo_root` (which [`is_in_repo_scope`] drops anyway) is returned
/// unchanged — this function narrows paths, it never invents one.
pub fn repo_relative_path(repo_root: &Path, finding_path: &str) -> String {
    if finding_path.starts_with('(') {
        return finding_path.to_string();
    }
    let p = Path::new(finding_path);
    if !p.is_absolute() {
        return finding_path.replace('\\', "/");
    }
    match p.strip_prefix(repo_root) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        // Outside the repo: left alone deliberately. `is_in_repo_scope` is the component that
        // decides such a row has no business in the results; silently reshaping it here would
        // hide that decision from whoever reads the filter.
        Err(_) => finding_path.replace('\\', "/"),
    }
}

/// Pure and side-effect-free; all filtering decisions are deterministic.
pub fn is_in_repo_scope(repo_root: &Path, finding_path: &str) -> bool {
    // Rule 1: synthetic placeholders — always keep.
    if finding_path.starts_with('(') {
        return true;
    }

    let p = std::path::Path::new(finding_path);

    // Rule 2: relative paths are repo-relative by construction → resolve, then
    // apply the generated-file / dist rules below.
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        repo_root.join(p)
    };

    // Resolve symlinks on BOTH sides before the containment check. A tool (eslint, semgrep)
    // canonicalizes the paths it reports, so on macOS it emits `/private/var/folders/…` while
    // `repo_root` passed in is the symlinked `/var/folders/…` (the OS tempdir, and any repo under
    // a symlinked mount). A raw textual `starts_with` then judges every finding "outside the
    // repo" and drops ALL of them — silently zeroing a tool tier. Compare canonical forms,
    // falling back to the raw path whenever canonicalization fails (a synthetic test path, or a
    // file that no longer exists), so existing behaviour is preserved where there is no symlink.
    let root_c = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    let abs_c = abs.canonicalize().unwrap_or_else(|_| abs.clone());

    // Rule 3: absolute path outside the repo → drop.
    if !abs_c.starts_with(&root_c) {
        return false;
    }

    // The remainder of the rules apply to the path RELATIVE to repo_root.
    let rel = abs_c.strip_prefix(&root_c).unwrap_or(&abs_c);

    // Rule 4: Cargo build output directory `target/`.
    if rel.starts_with("target") {
        return false;
    }

    // Rules 5 & 6: inspect individual path components and the filename.
    let path_str = rel.to_string_lossy();

    // Rule 5a: Cargo OUT_DIR pattern — `build/<pkg>/out/`.
    // Matches any segment sequence `…/out/…` that sits under a `build/` dir.
    if path_str.contains("/out/") || path_str.starts_with("out/") {
        return false;
    }

    // Rule 5b: generated file by name suffix.
    if let Some(fname) = rel.file_name().and_then(|n| n.to_str()) {
        if fname.ends_with("_generated.rs") {
            return false;
        }
        // `foo.generated.ts`, `foo.generated.js`, etc.
        if let Some(stem) = std::path::Path::new(fname)
            .file_stem()
            .and_then(|s| s.to_str())
        {
            if stem.ends_with(".generated") {
                return false;
            }
        }
    }

    // Rule 6: bundled dist directories.
    if path_str.contains("/dist/")
        || path_str.starts_with("dist/")
        || path_str.contains("/dist-server/")
        || path_str.starts_with("dist-server/")
    {
        return false;
    }

    // Rule 7: within the repo and not excluded → keep.
    true
}

// ─── tool invocation (I/O) ───────────────────────────────────────────────────

/// Run a program in `dir`, capturing stdout SEPARATELY (the parsers need clean
/// JSON, not stdout+stderr interleaved). A spawn failure (binary not on PATH)
/// returns `Err` so the caller can emit a graceful note. A non-zero exit is NOT
/// an error — linters exit non-zero when they find issues, which is the normal
/// "there are findings" signal.
///
/// # Why stderr is RETURNED, not dropped (W6 / C11)
///
/// This used to discard stderr entirely. That is how the eslint preview could
/// fail wholesale on every repo for months while the only diagnostic anyone ever
/// saw was `EOF while parsing a value at line 1 column 0` — a `serde_json` error
/// about an EMPTY stdout, which says nothing about WHY the tool produced nothing.
/// The actual explanation (`There was a problem loading formatter: …`) was written
/// to stderr and thrown away one stack frame below the code that reported the
/// useless error. A tool that exits without usable output is exactly the moment
/// its stderr becomes the only evidence that exists, so the third element of the
/// returned tuple carries it to whoever builds the failure message
/// (see [`tool_output_error`]).
///
/// When `on_progress` is `Some`, stdout is READ LINE BY LINE and the callback is
/// fired on every line received. This keeps `last_activity_ms` fresh while a tool
/// like `cargo clippy` compiles a big repo (the rivet fix): the compile phase
/// streams many output lines even before any lint result appears, so the job never
/// appears stalled mid-compile. Output is accumulated into a single `String` and
/// returned exactly as before, so all callers and parsers are unchanged.
///
/// When `on_progress` is `None` the function falls back to `.output().await`
/// (buffered, no heartbeat) — identical to the previous behaviour. This is the
/// path taken by the no-job synchronous callers.
async fn run_capture_stdout(
    dir: &Path,
    program: &str,
    args: &[&str],
    on_progress: Option<&HeartbeatFn>,
) -> std::io::Result<(String, bool, String)> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

    match on_progress {
        None => {
            // Fast path: no heartbeat needed. Buffer everything.
            let out = tokio::process::Command::new(program)
                .args(args)
                .current_dir(dir)
                .kill_on_drop(true)
                .output()
                .await?;
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            Ok((stdout, out.status.success(), stderr))
        }
        Some(cb) => {
            // Streaming path: read stdout line-by-line, firing the heartbeat on
            // every line.  Stderr is captured separately (piped, read to the end
            // AFTER stdout closes) so it doesn't interleave with the JSON stdout
            // the parsers expect but is still available as failure evidence.
            let mut child = tokio::process::Command::new(program)
                .args(args)
                .current_dir(dir)
                .kill_on_drop(true)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()?;

            let stdout = child.stdout.take().expect("stdout is piped");
            let mut err_pipe = child.stderr.take();
            let mut lines = BufReader::new(stdout).lines();
            let mut accumulated = String::new();

            while let Ok(Some(line)) = lines.next_line().await {
                cb();
                accumulated.push_str(&line);
                accumulated.push('\n');
            }

            // Read stderr to EOF rather than dropping the pipe. Best-effort: a
            // read failure must never turn a successful tool run into an error,
            // so it degrades to an empty string.
            let mut stderr = String::new();
            if let Some(pipe) = err_pipe.as_mut() {
                let _ = pipe.read_to_string(&mut stderr).await;
            }

            let status = child.wait().await?;
            Ok((accumulated, status.success(), stderr))
        }
    }
}

/// The error a tool run produces when its output could not be turned into
/// findings — built so the message names the EVIDENCE rather than only the
/// symptom (W6 / C11; see [`run_capture_stdout`]'s doc comment for the failure
/// this exists to make legible).
///
/// Three cases, in the order they matter to whoever reads the resulting
/// `CoverageNote`:
///
/// 1. **Empty stdout.** The tool produced nothing at all, which is never a valid
///    "clean" result for a tool invoked with a JSON output format — every JSON
///    formatter emits at least `[]`. The parse error (`EOF while parsing a value
///    at line 1 column 0`) is a tautology here, so it is replaced with the plain
///    statement plus the first lines of stderr, which is where the real cause
///    lives.
/// 2. **Non-empty stdout that would not parse.** Report the parse error AND the
///    stderr tail: a tool can write a warning banner to stderr and still emit
///    truncated JSON.
/// 3. **Parsed fine.** Not an error; this function is never called.
///
/// `stderr` is truncated to [`STDERR_EVIDENCE_LIMIT`] bytes because this string
/// lands in a client-visible coverage note, not a log file.
fn tool_output_error(
    tool: ScanTool,
    stdout: &str,
    stderr: &str,
    parse_error: Option<&anyhow::Error>,
) -> anyhow::Error {
    let evidence = stderr.trim();
    let evidence: String = if evidence.is_empty() {
        "(the tool wrote nothing to stderr either)".to_string()
    } else {
        evidence.chars().take(STDERR_EVIDENCE_LIMIT).collect()
    };
    if stdout.trim().is_empty() {
        anyhow::anyhow!(
            "{} produced NO output at all (exit without a result document, which a JSON \
             output format never does on a successful run). Tool stderr: {evidence}",
            tool.name(),
        )
    } else {
        match parse_error {
            Some(e) => anyhow::anyhow!(
                "could not parse {} output ({e}). Tool stderr: {evidence}",
                tool.name(),
            ),
            None => anyhow::anyhow!(
                "could not parse {} output. Tool stderr: {evidence}",
                tool.name(),
            ),
        }
    }
}

/// Byte cap on the stderr excerpt embedded in a [`CoverageNote`] by
/// [`tool_output_error`]. Enough for a node/python stack-trace header (which is
/// where the cause is), short enough that a note stays readable.
const STDERR_EVIDENCE_LIMIT: usize = 600;

/// When set (to any non-empty value), the `Semgrep` arm of [`run_one_tool`] fails
/// IMMEDIATELY with a `CoverageNote`-producing error, skipping provisioning and every
/// network/process call — mirrors `crate::dep_audit::DISABLE_ENV_VAR`'s test-isolation
/// rationale, scoped to Semgrep specifically. Lets a test exercise the REAL "the
/// commodity taint pass could not run this scan" degradation path deterministically,
/// without depending on whether `semgrep` happens to be installed or network access is
/// available to `pip install` it.
///
/// Never set this variable in production code paths.
pub const DISABLE_SEMGREP_ENV_VAR: &str = "CAMERATA_DISABLE_SEMGREP";

/// Run the SCAN-TIME deterministic preview pass for ONE repo: group the selected
/// mechanical rules by tool, run each tool ONCE with a Camerata-supplied config
/// enabling exactly those rules, parse the output into preview findings, and
/// return them. Graceful throughout: a missing tool / unparseable output yields a
/// benign NOTE finding, never a false clean.
///
/// `repo` is the `owner/repo` spec (tagged onto each finding); `dir` is the local
/// working tree. `selected` is the SELECTED set (the caller passes the mechanical
/// scan-runnable subset — but this fn re-checks `is_ci_enforced` + `layer3_only`
/// defensively via [`group_by_tool`]).
///
/// `present_languages` — when `Some`, stack-gates each tool: a tool whose required
/// language is absent is omitted from the run AND from the pre-declared count (so
/// a Rust-only repo never runs eslint, even if a JS rule was selected). When `None`,
/// no gating (all tools run). Callers should pass
/// `Some(&languages_from_files(&files))` for the repo's actual file set.
///
/// `progress` — when `Some`, the pass reports PER-TOOL progress into the job
/// (`(store, job_id)`): each tool registers (`starting`), is marked `running` before
/// it executes, and `done` with its findings count when it finishes — mirroring how the
/// AI passes stream progress. `None` runs silently (the synchronous path that has no job).
///
/// Returns `(findings, coverage_notes, attempted_tools)`. `attempted_tools` is the set
/// of tool names THIS call actually invoked (i.e. `by_tool`'s keys, computed before any
/// tool runs) — regardless of whether each one then succeeded or failed. A caller
/// (`merge_scan_preview` / `reconcile_external_tool_ledger`) needs this to distinguish
/// "the tool ran and genuinely found nothing" (ran = true) from "the tool was never
/// even invoked this scan" (ran = false + disclosure) — a distinction a `CoverageNote`
/// alone cannot make, since a tool nothing routed to emits no note at all (see
/// [`tool_for_rule`]'s doc comment for the exact wiring gap this closes).
pub async fn run_scan_tools<'r>(
    repo: &str,
    dir: &Path,
    selected: &[SelectedRule],
    lookup: &(dyn Fn(&str) -> Option<&'r Rule> + Send + Sync),
    present_languages: Option<&HashSet<String>>,
    progress: Option<(&crate::jobs::JobStore, &str)>,
) -> (Vec<Finding>, Vec<CoverageNote>, HashSet<&'static str>) {
    let (by_tool, ungrouped) = group_by_tool(selected, lookup, present_languages);
    let attempted: HashSet<&'static str> = by_tool.keys().map(|t| t.name()).collect();
    let mut findings = Vec::new();
    let mut coverage_notes: Vec<CoverageNote> = Vec::new();

    // Pre-register every tool we know we'll drive so the progress denominator is accurate
    // from the start (the UI shows the full set of tools queued, not one-at-a-time growth).
    if let Some((jstore, jid)) = progress {
        for tool in by_tool.keys() {
            jstore.det_register_tool(jid, tool.name());
        }
        if !ungrouped.is_empty() {
            jstore.det_register_tool(jid, "unrouted");
        }
    }

    // Note any selected mechanical rule we couldn't route to a driven tool, so a
    // preview gap is visible rather than a silent clean.
    if let Some((jstore, jid)) = progress {
        if !ungrouped.is_empty() {
            jstore.det_tool_running(jid, "unrouted");
        }
    }
    for sr in &ungrouped {
        coverage_notes.push(CoverageNote {
            tool: "unrouted".to_string(),
            message: format!(
                "Could not preview {} — no scan-runnable tool wired for its linter source; \
                 it enforces once wired into CI.",
                sr.id
            ),
        });
    }
    if let Some((jstore, jid)) = progress {
        if !ungrouped.is_empty() {
            jstore.det_tool_done(jid, "unrouted", ungrouped.len());
        }
    }

    // Build a HeartbeatFn that ticks `last_activity_ms` on the job whenever a
    // scan tool emits a stdout line OR the build-dir mtime advances (the rivet
    // fix). When there is no progress context (silent synchronous path), this is
    // `None` and both signals are disabled — identical to the previous behaviour.
    let on_progress: Option<HeartbeatFn> = progress.map(|(jstore, jid)| {
        let store = jstore.clone();
        let id = jid.to_string();
        Arc::new(move || store.touch_activity(&id)) as HeartbeatFn
    });

    for (tool, rules) in by_tool {
        if let Some((jstore, jid)) = progress {
            jstore.det_tool_running(jid, tool.name());
        }
        let produced = match run_one_tool(repo, dir, tool, &rules, lookup, on_progress.clone())
            .await
        {
            Ok((mut fs, mut gaps)) => {
                let n = fs.len();
                findings.append(&mut fs);
                // A tool can RUN and still have covered less than was selected —
                // disclose those partial gaps alongside the findings rather than
                // letting a withheld rule read as checked-and-clean. See
                // `run_one_tool`'s doc comment.
                coverage_notes.append(&mut gaps);
                n
            }
            Err(e) => {
                coverage_notes.push(CoverageNote {
                    tool: tool.name().to_string(),
                    message: format!(
                        "Could not preview {} rule(s) with {}: {e}. It enforces once wired into CI.",
                        rules.len(),
                        tool.name()
                    ),
                });
                0
            }
        };
        if let Some((jstore, jid)) = progress {
            jstore.det_tool_done(jid, tool.name(), produced);
        }
    }

    (findings, coverage_notes, attempted)
}

/// Run a SINGLE tool over the repo with a Camerata-supplied config that enables
/// exactly `rules`, and parse the result into preview findings. Returns `Err`
/// (which the caller turns into a note) when the tool cannot be spawned or its
/// output cannot be parsed — never a false clean.
///
/// When `on_progress` is `Some`, two liveness signals fire for the duration of
/// this tool's execution:
///
/// 1. **Output-line signal** — `run_capture_stdout` fires the callback on every
///    stdout line received (fast for linters that stream JSON results).
///
/// 2. **Build-dir mtime probe** — a background `tokio::spawn` task polls
///    `dir/target/` every 15s and fires the callback when the mtime advances.
///    This covers `cargo clippy` cold-compiling a big repo (rocksdb/native deps,
///    8+ min) that writes to `target/` continuously but emits no stdout lines
///    until compilation finishes. The probe is aborted when the tool exits.
///
/// Both signals call the same `on_progress` callback, which the caller wires to
/// `JobStore::touch_activity`. The combined effect: even a completely quiet compile
/// keeps `last_activity_ms` updated, so the stall-detection idle counter stays low
/// and the UI banner never false-fires for a legitimately busy tool.
/// Returns `(findings, partial_gaps)`. `partial_gaps` is non-empty when the tool
/// RAN but could not cover part of what was selected — today: an eslint rule whose
/// plugin package is absent from the provisioned workspace, and an eslint run that
/// could not parse any file it read. These are disclosed as [`CoverageNote`]s by
/// the caller exactly like a whole-tool failure, because the alternative (dropping
/// them) is the silencing shape W6 exists to close: a rule that was selected,
/// withheld, and never mentioned reads as "checked and clean".
async fn run_one_tool<'r>(
    repo: &str,
    dir: &Path,
    tool: ScanTool,
    rules: &[&SelectedRule],
    lookup: &(dyn Fn(&str) -> Option<&'r Rule> + Send + Sync),
    on_progress: Option<HeartbeatFn>,
) -> anyhow::Result<(Vec<Finding>, Vec<CoverageNote>)> {
    // Start the build-dir mtime probe for the duration of this tool run.
    // Targets the `target/` subdirectory of the repo root (cargo's default output
    // dir). The probe is best-effort: if `target/` doesn't exist (non-Rust repo),
    // newest_mtime returns None and the probe just skips its ticks silently.
    let _mtime_probe = on_progress.as_ref().map(|cb| {
        let target_dir: PathBuf = dir.join("target");
        spawn_mtime_probe(target_dir, cb.clone(), MTIME_PROBE_INTERVAL)
    });
    // Collect the per-rule selector tokens from each rule's linter source.
    let selectors: Vec<String> = rules
        .iter()
        .filter_map(|sr| lookup(&sr.id))
        .flat_map(|rule| {
            rule.sources
                .iter()
                .filter_map(|s| s.linter.as_deref())
                .filter_map(selector_for_linter)
                .collect::<Vec<_>>()
        })
        .collect();

    match tool {
        ScanTool::Semgrep => {
            // Test isolation (see `DISABLE_SEMGREP_ENV_VAR`'s doc comment): fail exactly as
            // a genuinely unavailable tool would (a `CoverageNote`, never a silent empty-
            // but-"ran" result) without touching provisioning or the network.
            if std::env::var(DISABLE_SEMGREP_ENV_VAR)
                .map(|v| !v.is_empty())
                .unwrap_or(false)
            {
                anyhow::bail!("semgrep disabled via {DISABLE_SEMGREP_ENV_VAR} (test isolation)");
            }
            // Semgrep selects by config PACK, not individual ids.  Camerata
            // auto-provisions semgrep into a stable venv so the user never
            // needs to install it manually.  The preview runs against the
            // bundled offline ruleset (no network call to the semgrep registry).
            let tooling_dir = tool_provisioning::tooling_dir().ok_or_else(|| {
                anyhow::anyhow!("could not resolve Camerata data dir for tool provisioning")
            })?;
            let semgrep_bin = tool_provisioning::ensure_semgrep(&tooling_dir)
                .await
                .map_err(|e| anyhow::anyhow!("semgrep provisioning: {e}"))?;
            let rules_dir = tool_provisioning::bundled_semgrep_rules_dir();
            let rules_str = rules_dir.to_string_lossy().into_owned();
            let bin_str = semgrep_bin.to_string_lossy().into_owned();
            let (stdout, _ok, stderr) = run_capture_stdout(
                dir,
                &bin_str,
                &["--sarif", "--config", &rules_str, "--quiet", "."],
                on_progress.as_ref(),
            )
            .await?;
            match parse_sarif(repo, ScanTool::Semgrep, &stdout) {
                Ok(fs) => Ok((fs, Vec::new())),
                Err(e) => Err(tool_output_error(
                    ScanTool::Semgrep,
                    &stdout,
                    &stderr,
                    Some(&e),
                )),
            }
        }
        ScanTool::Ruff => {
            if selectors.is_empty() {
                anyhow::bail!("no ruff rule codes derived from the selection");
            }
            let select = selectors.join(",");
            let (stdout, _ok, stderr) = run_capture_stdout(
                dir,
                "ruff",
                &["check", "--output-format", "json", "--select", &select, "."],
                on_progress.as_ref(),
            )
            .await?;
            match parse_ruff_json(repo, &stdout) {
                Ok(fs) => Ok((fs, Vec::new())),
                Err(e) => Err(tool_output_error(
                    ScanTool::Ruff,
                    &stdout,
                    &stderr,
                    Some(&e),
                )),
            }
        }
        ScanTool::Clippy => {
            // Camerata-supplied config: warn on exactly the selected lints via
            // RUSTFLAGS-style `-W clippy::<lint>` passed after `--`. Output as JSON.
            let mut args: Vec<String> = vec![
                "clippy".into(),
                "--message-format=json".into(),
                "--quiet".into(),
                "--".into(),
            ];
            for sel in &selectors {
                // Selectors from the corpus are bare lint names (`unwrap_used`);
                // clippy wants the `clippy::` namespace.
                let lint = if sel.contains("::") {
                    sel.clone()
                } else {
                    format!("clippy::{sel}")
                };
                args.push("-W".into());
                args.push(lint);
            }
            let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let (stdout, _ok, stderr) =
                run_capture_stdout(dir, "cargo", &arg_refs, on_progress.as_ref()).await?;
            match parse_clippy_json(repo, &stdout) {
                Ok(fs) => Ok((fs, Vec::new())),
                Err(e) => Err(tool_output_error(
                    ScanTool::Clippy,
                    &stdout,
                    &stderr,
                    Some(&e),
                )),
            }
        }
        ScanTool::Eslint => {
            if selectors.is_empty() {
                anyhow::bail!("no eslint rule ids derived from the selection");
            }
            // Camerata auto-provisions eslint into a stable node_modules
            // workspace so the user never needs to install it manually. We use
            // the bundled flat config as the base and override individual rules
            // to "error" via `--rule`, with an explicit `--config` pointing at
            // the bundled flat config (eslint v9 flat-config style).
            //
            // `--format json` is eslint's BUILT-IN formatter, deliberately not an
            // npm formatter package: eslint resolves a non-built-in formatter name
            // relative to its own cwd, which here is the SCANNED REPO, so the
            // previous `--format @microsoft/eslint-formatter-sarif` could never
            // resolve and aborted every run with an empty stdout. See
            // [`parse_eslint_json`]'s doc comment for the full C11 post-mortem.
            let tooling_dir = tool_provisioning::tooling_dir().ok_or_else(|| {
                anyhow::anyhow!("could not resolve Camerata data dir for tool provisioning")
            })?;
            let eslint_bin = tool_provisioning::ensure_eslint(&tooling_dir)
                .await
                .map_err(|e| anyhow::anyhow!("eslint provisioning: {e}"))?;
            let workspace = tool_provisioning::eslint_workspace_dir(&tooling_dir);
            let config_path = tool_provisioning::eslint_config_path(&workspace);
            let bin_str = eslint_bin.to_string_lossy().into_owned();
            let config_str = config_path.to_string_lossy().into_owned();

            // Withhold any selector whose plugin package is absent — handing one
            // to `--rule` is fatal to the WHOLE invocation, not just that rule.
            // Each withheld selector becomes its own disclosed gap.
            let (runnable, unresolvable) = partition_eslint_selectors(&selectors, &workspace);
            let mut gaps: Vec<CoverageNote> = unresolvable
                .iter()
                .map(|(sel, pkg)| CoverageNote {
                    tool: ScanTool::Eslint.name().to_string(),
                    message: format!(
                        "Could not preview the eslint rule `{sel}`: its plugin package \
                         `{pkg}` is not installed in Camerata's eslint workspace, so eslint \
                         cannot resolve the rule. Every OTHER selected eslint rule was still \
                         previewed. It enforces once wired into CI."
                    ),
                })
                .collect();
            if runnable.is_empty() {
                anyhow::bail!(
                    "none of the {} selected eslint rule(s) could be resolved: every one needs \
                     a plugin package that is not installed in Camerata's eslint workspace",
                    selectors.len(),
                );
            }

            let args = eslint_args(&config_str, &runnable);
            let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let (stdout, _ok, stderr) =
                run_capture_stdout(dir, &bin_str, &arg_refs, on_progress.as_ref()).await?;
            match parse_eslint_json(repo, &stdout) {
                Ok((fs, shape)) => {
                    // "Read every file and could not parse a single one" is a
                    // zero-coverage run wearing a clean run's clothes — exit 0,
                    // valid JSON, no findings. Never leave it undisclosed.
                    if shape.read_nothing() {
                        gaps.push(CoverageNote {
                            tool: ScanTool::Eslint.name().to_string(),
                            message: format!(
                                "eslint could not PARSE any of the {} file(s) it read, so none \
                                 of the {} selected rule(s) was actually evaluated — a parser \
                                 for this repo's sources is missing from Camerata's eslint \
                                 workspace. These rules enforce once wired into CI.",
                                shape.files_reported,
                                runnable.len(),
                            ),
                        });
                    }
                    Ok((keep_selected_eslint_findings(fs, &runnable), gaps))
                }
                Err(e) => Err(tool_output_error(
                    ScanTool::Eslint,
                    &stdout,
                    &stderr,
                    Some(&e),
                )),
            }
        }
    }
    // Apply the repo-scope filter uniformly across ALL tools (clippy, ruff,
    // eslint, semgrep).  This drops findings whose path resolves outside the
    // scanned repo — e.g. Rust stdlib paths like `/rustc/<hash>/…`, Cargo build
    // output under `target/`, generated files, and bundled dist directories.
    // The filter is pure and applied here once rather than per-parser so new
    // tool arms inherit it automatically.
    .map(|(mut findings, gaps)| {
        findings.retain(|f| is_in_repo_scope(dir, &f.path));
        // W21: normalise in the SAME uniform pass, for the reason the comment above gives for
        // filtering here — a new tool arm inherits it automatically instead of each parser
        // having to remember. Order matters: filter first, so `is_in_repo_scope` still sees
        // the absolute path it needs to judge whether the file is even inside the repo.
        for f in &mut findings {
            f.path = repo_relative_path(dir, &f.path);
        }
        (findings, gaps)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_core::RuleId;
    use camerata_rules::{EnforcementKind, RuleSource, Verification};

    /// Build a `lookup` closure over a slice of hand-built rules (the real caller
    /// passes `|id| set.get_by_id(id)`; tests avoid the private `RuleSet` ctor).
    /// The explicit HRTB matches the `&dyn Fn` param's `for<'a>` bound.
    fn lookup_over<'r>(rules: &'r [Rule]) -> impl Fn(&str) -> Option<&'r Rule> + Send + Sync {
        move |id: &str| rules.iter().find(|r| r.id.0 == id)
    }

    fn rule_with(id: &str, enforcement: EnforcementKind, layer3: bool, linters: &[&str]) -> Rule {
        Rule {
            id: RuleId(id.to_string()),
            title: id.to_string(),
            enforcement,
            domain: "universal".to_string(),
            summary: String::new(),
            decision_question: None,
            decision_why: None,
            options: Vec::new(),
            default_option: None,
            cwe: Vec::new(),
            asvs: Vec::new(),
            superseded_since: None,
            current_recommendation: None,
            version: None,
            last_reviewed: None,
            verification: Verification::Grounded,
            sources: linters
                .iter()
                .map(|l| RuleSource {
                    url: "https://example".to_string(),
                    title: "src".to_string(),
                    linter: Some(l.to_string()),
                    // W18 added `relevance` (author-facing source provenance). Nothing in
                    // `scan_tools` reads it; `None` keeps this fixture at its prior behaviour.
                    relevance: None,
                })
                .collect(),
            verified: None,
            opt_in_only: false,
            layer3_only: layer3,
            stack_exceptions: Vec::new(),
            extra_domains: Vec::new(),
            kind: None,
            inspectable: None,
            max_severity: None,
            min_severity: None,
            deviation_ceiling: None,
        }
    }

    fn selected(id: &str) -> SelectedRule {
        SelectedRule {
            id: id.to_string(),
            directive: "do the thing".to_string(),
            repos: Vec::new(),
        }
    }

    // ── linter-source -> tool grouping ───────────────────────────────────────

    #[test]
    fn linter_source_maps_to_tool() {
        assert_eq!(
            tool_for_linter("clippy: unwrap_used"),
            Some(ScanTool::Clippy)
        );
        assert_eq!(
            tool_for_linter("clippy::unwrap_used"),
            Some(ScanTool::Clippy)
        );
        assert_eq!(tool_for_linter("Ruff: S608"), Some(ScanTool::Ruff));
        assert_eq!(tool_for_linter("semgrep"), Some(ScanTool::Semgrep));
        assert_eq!(tool_for_linter("eslint: eqeqeq"), Some(ScanTool::Eslint));
        assert_eq!(
            tool_for_linter("@typescript-eslint: no-explicit-any"),
            Some(ScanTool::Eslint)
        );
        assert_eq!(
            tool_for_linter("@angular-eslint/prefer-inject"),
            Some(ScanTool::Eslint)
        );
        // Not driven end-to-end here -> None (caller emits a graceful note).
        assert_eq!(tool_for_linter("golangci-lint: errcheck"), None);
        assert_eq!(tool_for_linter("Checkstyle: FinalClass"), None);
        assert_eq!(tool_for_linter("RuboCop: Metrics/MethodLength"), None);
    }

    #[test]
    fn selector_extracts_rule_id() {
        assert_eq!(selector_for_linter("Ruff: S608").as_deref(), Some("S608"));
        assert_eq!(
            selector_for_linter("clippy: unwrap_used").as_deref(),
            Some("unwrap_used")
        );
        assert_eq!(
            selector_for_linter("eslint: eqeqeq").as_deref(),
            Some("eqeqeq")
        );
        assert_eq!(
            selector_for_linter("@typescript-eslint: no-explicit-any").as_deref(),
            Some("@typescript-eslint/no-explicit-any")
        );
        // semgrep selects by config pack, not id.
        assert_eq!(selector_for_linter("semgrep"), None);
    }

    /// JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1 / JAVASCRIPT-REACT-RULES-OF-HOOKS-1 regression
    /// guard: the `react-hooks:` scope (eslint-plugin-react-hooks) must route to
    /// `ScanTool::Eslint` and produce the real `react-hooks/<rule>` eslint id — before this
    /// fix, `tool_for_linter` did not recognize the `react-hooks` token at all, so both
    /// corpus rules (declared `enforcement = "mechanical"`) silently had NO deterministic
    /// detector of any kind, relying entirely on the semantic/AI pass.
    #[test]
    fn react_hooks_sources_route_to_eslint() {
        assert_eq!(
            tool_for_linter("react-hooks: exhaustive-deps"),
            Some(ScanTool::Eslint)
        );
        assert_eq!(
            tool_for_linter("react-hooks: rules-of-hooks"),
            Some(ScanTool::Eslint)
        );
        assert_eq!(
            selector_for_linter("react-hooks: exhaustive-deps").as_deref(),
            Some("react-hooks/exhaustive-deps")
        );
        assert_eq!(
            selector_for_linter("react-hooks: rules-of-hooks").as_deref(),
            Some("react-hooks/rules-of-hooks")
        );
    }

    /// eslint-plugin-jest: real rule ids route and produce `jest/<rule>`.
    #[test]
    fn jest_rule_sources_route_to_eslint() {
        for (source, expected) in [
            ("jest: no-conditional-expect", "jest/no-conditional-expect"),
            ("jest: expect-expect", "jest/expect-expect"),
            ("jest: no-done-callback", "jest/no-done-callback"),
            ("jest: prefer-spy-on", "jest/prefer-spy-on"),
            ("jest: no-identical-title", "jest/no-identical-title"),
            ("jest: valid-title", "jest/valid-title"),
            ("jest: no-disabled-tests", "jest/no-disabled-tests"),
            ("jest: no-focused-tests", "jest/no-focused-tests"),
        ] {
            assert_eq!(tool_for_linter(source), Some(ScanTool::Eslint), "{source}");
            assert_eq!(
                selector_for_linter(source).as_deref(),
                Some(expected),
                "{source}"
            );
        }
    }

    /// A handful of `linter` annotations describe a tool CONVENTION (Jest's/Vitest's own
    /// default glob matching, or a method-call mention) rather than naming a real lint
    /// rule — these must route to NO tool at all (never Eslint with a bogus selector that
    /// would either no-op or error when handed to `eslint --rule`).
    #[test]
    fn non_rule_jest_and_vitest_conventions_are_not_eslint_routable() {
        assert_eq!(
            tool_for_linter("jest: jest.useFakeTimers() API"),
            None,
            "a method-call mention is not a rule id"
        );
        assert_eq!(
            tool_for_linter("jest: testMatch default glob"),
            None,
            "a config-default description is not a rule id"
        );
        assert_eq!(
            tool_for_linter("vitest: include default glob"),
            None,
            "vitest is not a recognized eslint-family token, and this isn't a rule id either"
        );
        assert_eq!(selector_for_linter("jest: jest.useFakeTimers() API"), None);
        assert_eq!(selector_for_linter("jest: testMatch default glob"), None);
    }

    /// Regression: `eslint-plugin-vue: vue/component-api-style` must resolve to
    /// `vue/component-api-style`, NOT `vue/vue/component-api-style`. Before this fix the
    /// scope-prefixing branch always prepended `{scope}/` without checking whether `after`
    /// already carried it — every Vue corpus rule (which records the rule in
    /// already-fully-qualified `vue/<rule>` form after the colon) produced a doubled,
    /// invalid eslint rule id that could never have matched a real rule when handed to
    /// `eslint --rule`.
    #[test]
    fn vue_plugin_sources_are_not_double_prefixed() {
        for (source, expected) in [
            (
                "eslint-plugin-vue: vue/component-api-style",
                "vue/component-api-style",
            ),
            (
                "eslint-plugin-vue: vue/no-side-effects-in-computed-properties",
                "vue/no-side-effects-in-computed-properties",
            ),
            (
                "eslint-plugin-vue: vue/no-mutating-props",
                "vue/no-mutating-props",
            ),
            (
                "eslint-plugin-vue: vue/enforce-style-attribute",
                "vue/enforce-style-attribute",
            ),
        ] {
            assert_eq!(
                selector_for_linter(source).as_deref(),
                Some(expected),
                "{source}"
            );
        }
    }

    #[test]
    fn group_by_tool_routes_and_excludes_layer3() {
        let rules = vec![
            rule_with(
                "RUST-A",
                EnforcementKind::Mechanical,
                false,
                &["clippy: unwrap_used"],
            ),
            rule_with("PY-A", EnforcementKind::Mechanical, false, &["Ruff: S608"]),
            // layer3_only (CodeQL-style) must be EXCLUDED from the preview pass.
            rule_with("CODEQL-1", EnforcementKind::Mechanical, true, &["semgrep"]),
            // A prose rule is not CI-enforced -> excluded.
            rule_with("PROSE-1", EnforcementKind::Prose, false, &["clippy: foo"]),
            // No corpus tool -> ungrouped (graceful note).
            rule_with(
                "GO-A",
                EnforcementKind::Mechanical,
                false,
                &["golangci-lint: errcheck"],
            ),
        ];
        let lookup = lookup_over(&rules);
        let sel = vec![
            selected("RUST-A"),
            selected("PY-A"),
            selected("CODEQL-1"),
            selected("PROSE-1"),
            selected("GO-A"),
        ];
        let (by_tool, ungrouped) = group_by_tool(&sel, &lookup, None);
        assert!(by_tool.contains_key(&ScanTool::Clippy));
        assert!(by_tool.contains_key(&ScanTool::Ruff));
        // layer3_only never previews.
        assert!(!by_tool.values().flatten().any(|s| s.id == "CODEQL-1"));
        // prose isn't CI-enforced.
        assert!(!by_tool.values().flatten().any(|s| s.id == "PROSE-1"));
        // golangci-lint isn't driven -> ungrouped.
        assert_eq!(ungrouped.len(), 1);
        assert_eq!(ungrouped[0].id, "GO-A");
    }

    /// W3 wiring-gap regression guard: a corpus rule with NO `[[sources]].linter` at all
    /// (exactly `SEC-NO-RAW-SQL-CONCAT-1`'s and `SEC-NO-COMMAND-INJECTION-1`'s real shape —
    /// see `crates/rules/principles/universal/sec-no-raw-sql-concat-1.toml`) but present in
    /// `mechanical_gate::semgrep_covered_rule_ids` must still resolve to `ScanTool::Semgrep` —
    /// never `None`/`ungrouped`. Before the `tool_for_rule` fallback, this id silently never
    /// drove Semgrep in EITHER the headless or server scan path; see that function's doc
    /// comment for the full wiring-gap writeup.
    #[test]
    fn tool_for_rule_falls_back_to_semgrep_covered_rule_ids_when_no_linter_source() {
        let semgrep_ids = crate::mechanical_gate::semgrep_covered_rule_ids();
        assert!(
            semgrep_ids.contains("SEC-NO-RAW-SQL-CONCAT-1"),
            "precondition: this id must be claimed-covered by semgrep"
        );
        let rule = rule_with(
            "SEC-NO-RAW-SQL-CONCAT-1",
            EnforcementKind::Mechanical,
            false,
            &[],
        );
        assert!(
            rule.sources.is_empty(),
            "precondition: no linter source at all, mirroring the real corpus TOML"
        );
        assert_eq!(
            tool_for_rule(&rule),
            Some(ScanTool::Semgrep),
            "a semgrep_covered_rule_ids id with no linter source must still route to Semgrep"
        );
    }

    /// A rule with NO linter source AND absent from `semgrep_covered_rule_ids` must still fall
    /// through to `None` — the fallback must not swallow every ungrouped rule indiscriminately.
    #[test]
    fn tool_for_rule_returns_none_when_not_semgrep_covered_and_no_linter_source() {
        let rule = rule_with(
            "SOME-UNRELATED-RULE-1",
            EnforcementKind::Mechanical,
            false,
            &[],
        );
        assert_eq!(tool_for_rule(&rule), None);
    }

    /// `group_by_tool`-level proof of the same fix: the rule actually lands in
    /// `by_tool[Semgrep]`, not `ungrouped` — this is what makes `run_scan_tools` actually
    /// attempt (and `ensure_semgrep` provision) Semgrep for it at scan time.
    #[test]
    fn group_by_tool_routes_a_semgrep_covered_rule_with_no_linter_source_to_semgrep() {
        let rules = vec![rule_with(
            "SEC-NO-RAW-SQL-CONCAT-1",
            EnforcementKind::Mechanical,
            false,
            &[],
        )];
        let lookup = lookup_over(&rules);
        let sel = vec![selected("SEC-NO-RAW-SQL-CONCAT-1")];
        let (by_tool, ungrouped) = group_by_tool(&sel, &lookup, None);
        assert!(
            by_tool
                .get(&ScanTool::Semgrep)
                .map(|v| v.iter().any(|s| s.id == "SEC-NO-RAW-SQL-CONCAT-1"))
                .unwrap_or(false),
            "must route to semgrep via the semgrep_covered_rule_ids fallback: {by_tool:?}"
        );
        assert!(
            ungrouped.is_empty(),
            "must never land in ungrouped now that the fallback resolves it"
        );
    }

    /// `run_scan_tools`'s third return value reports a tool as "attempted" even when it then
    /// FAILS — `CAMERATA_DISABLE_SEMGREP` forces a deterministic, network-free failure (see
    /// `DISABLE_SEMGREP_ENV_VAR`'s doc comment) so this doesn't depend on whether semgrep
    /// happens to be installed on the machine running the test.
    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn run_scan_tools_reports_semgrep_as_attempted_even_when_it_fails() {
        let _env = camerata_test_env::EnvGuard::set(DISABLE_SEMGREP_ENV_VAR, "1");
        let rules = vec![rule_with(
            "SEC-NO-RAW-SQL-CONCAT-1",
            EnforcementKind::Mechanical,
            false,
            &[],
        )];
        let lookup = lookup_over(&rules);
        let dir = tempfile::tempdir().expect("tempdir");
        let (findings, notes, attempted) = run_scan_tools(
            "me/api",
            dir.path(),
            &[selected("SEC-NO-RAW-SQL-CONCAT-1")],
            &lookup,
            None,
            None,
        )
        .await;

        assert!(
            findings.is_empty(),
            "a disabled tool must yield no finding rows"
        );
        assert!(
            attempted.contains("semgrep"),
            "semgrep was routed via group_by_tool, so it must count as attempted even though \
             it was then disabled: {attempted:?}"
        );
        assert!(
            notes
                .iter()
                .any(|n| n.tool == "semgrep" && n.message.contains(DISABLE_SEMGREP_ENV_VAR)),
            "a disabled tool must yield a real semgrep coverage note (never a silent clean): \
             {notes:?}"
        );
    }

    // ── SARIF + per-tool JSON parsing ────────────────────────────────────────

    // ── normalize_semgrep_rule_id ─────────────────────────────────────────────

    #[test]
    fn normalize_strips_absolute_path_prefix() {
        // Real example from scanning an external repo with an absolute --config path.
        let raw = "Users.zacharyernst.Documents.Repos.camerata-orchestrator.crates.server.assets.semgrep-rules.camerata.security.sql-string-concat-rust";
        assert_eq!(
            normalize_semgrep_rule_id(raw),
            "camerata.security.sql-string-concat-rust"
        );
    }

    #[test]
    fn normalize_already_clean_id_unchanged() {
        let clean = "camerata.security.hardcoded-secret";
        assert_eq!(normalize_semgrep_rule_id(clean), clean);
    }

    #[test]
    fn normalize_non_camerata_id_unchanged() {
        let other = "python.lang.security.audit.exec-detected";
        assert_eq!(normalize_semgrep_rule_id(other), other);
    }

    #[test]
    fn normalize_all_camerata_rule_ids() {
        // All rule ids that appear in security.yml: confirm they survive normalization
        // unchanged (already clean) and that path-prefixed variants strip correctly.
        let clean_ids = [
            "camerata.security.hardcoded-secret",
            "camerata.security.hardcoded-secret-dquote",
            "camerata.security.exec-injection",
            "camerata.security.exec-injection-js",
            "camerata.security.sql-string-concat-python",
            "camerata.security.sql-string-concat-js",
            "camerata.security.sql-string-concat-rust",
            "camerata.security.sql-string-concat-csharp",
            "camerata.security.weak-hash-python",
            "camerata.security.weak-hash-js",
            "camerata.security.weak-hash-rust",
            "camerata.security.weak-hash-csharp",
            "camerata.security.path-traversal-python",
            "camerata.security.subprocess-shell-true",
            "camerata.security.yaml-unsafe-load",
            "camerata.security.disabled-tls-rust",
            "camerata.security.disabled-tls-csharp",
        ];
        for id in &clean_ids {
            assert_eq!(normalize_semgrep_rule_id(id), *id, "clean id mutated: {id}");
            // Simulate path-prefixed form (mimics absolute --config path).
            let prefixed = format!("some.path.prefix.{id}");
            assert_eq!(
                normalize_semgrep_rule_id(&prefixed),
                *id,
                "path-prefixed form not stripped for: {id}"
            );
        }
    }

    #[test]
    fn parse_sarif_into_findings() {
        let sarif = r#"{
          "version": "2.1.0",
          "runs": [{
            "results": [{
              "ruleId": "python.lang.security.audit.exec-detected",
              "level": "error",
              "message": { "text": "Detected use of exec" },
              "locations": [{
                "physicalLocation": {
                  "artifactLocation": { "uri": "src/app.py" },
                  "region": { "startLine": 42 }
                }
              }]
            }]
          }]
        }"#;
        let f = parse_sarif("me/api", ScanTool::Semgrep, sarif).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule_id, "python.lang.security.audit.exec-detected");
        assert_eq!(f[0].path, "src/app.py");
        assert_eq!(f[0].line, 42);
        assert_eq!(f[0].severity, "high");
        assert!(f[0].preview);
        assert_eq!(f[0].preview_tool.as_deref(), Some("semgrep"));
    }

    /// W3 (commodity-class taint layer): a SYNTHETIC semgrep SARIF payload for one of the new
    /// taint rule ids — exactly the shape semgrep emits with an absolute `--config` path —
    /// parses into a preview `Finding` with the clean, portable rule id and external-tool
    /// provenance. No semgrep binary required; this is the ingestion/mapping unit test the W3
    /// plan calls for.
    #[test]
    fn parse_sarif_ingests_a_synthetic_taint_sqli_finding() {
        let sarif = r#"{
          "version": "2.1.0",
          "runs": [{
            "results": [{
              "ruleId": "Users.alice.camerata.tooling.semgrep-rules.camerata.security.taint-sql-injection-go",
              "level": "error",
              "message": { "text": "Possible SQL injection (taint)." },
              "locations": [{
                "physicalLocation": {
                  "artifactLocation": { "uri": "internal/db.go" },
                  "region": { "startLine": 88 }
                }
              }]
            }]
          }]
        }"#;
        let f = parse_sarif("me/svc", ScanTool::Semgrep, sarif).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(
            f[0].rule_id, "camerata.security.taint-sql-injection-go",
            "path prefix must be stripped"
        );
        assert_eq!(f[0].path, "internal/db.go");
        assert_eq!(f[0].line, 88);
        assert_eq!(f[0].severity, "high");
        // External-tool provenance — never our own deterministic tier.
        assert!(
            f[0].preview,
            "must carry preview=true (external-tool), never our own tier"
        );
        assert_eq!(f[0].preview_tool.as_deref(), Some("semgrep"));
    }

    /// Sensitive-data-in-logs taint class, secret/credential family: a SYNTHETIC semgrep SARIF
    /// payload for `camerata.security.taint-log-secret-js` (the real id's absolute-`--config`-
    /// path-prefixed form) parses into a preview `Finding` with the clean rule id and `"high"`
    /// severity — `severity: ERROR` in the bundled YAML maps through `norm_severity` to
    /// `"high"`, confirming this class's secret/credential severity is set by the detector
    /// itself, not inflated or calibrated afterward.
    #[test]
    fn parse_sarif_ingests_a_synthetic_taint_log_secret_finding() {
        let sarif = r#"{
          "version": "2.1.0",
          "runs": [{
            "results": [{
              "ruleId": "Users.ci.camerata.tooling.semgrep-rules.camerata.security.taint-log-secret-js",
              "level": "error",
              "message": { "text": "Possible sensitive-data logging (taint)." },
              "locations": [{
                "physicalLocation": {
                  "artifactLocation": { "uri": "src/auth.ts" },
                  "region": { "startLine": 17 }
                }
              }]
            }]
          }]
        }"#;
        let f = parse_sarif("me/svc", ScanTool::Semgrep, sarif).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(
            f[0].rule_id, "camerata.security.taint-log-secret-js",
            "path prefix must be stripped"
        );
        assert_eq!(f[0].path, "src/auth.ts");
        assert_eq!(f[0].line, 17);
        assert_eq!(
            f[0].severity, "high",
            "a secret/credential value reaching a log sink must be HIGH, not inflated or deflated"
        );
        assert!(f[0].preview);
        assert_eq!(f[0].preview_tool.as_deref(), Some("semgrep"));
    }

    /// Sensitive-data-in-logs taint class, PII family: the SAME synthetic ingestion as above,
    /// but for `camerata.security.taint-log-pii-python` with `level: "warning"` — confirming
    /// the PII family's `severity: WARNING` maps through `norm_severity` to `"medium"`, the
    /// deliberately LOWER severity than the secret/credential family above (a PII value is a
    /// real exposure but not itself a reusable credential — see
    /// `SEC-NO-SENSITIVE-DATA-IN-LOGS-1`'s `[decision].why`). Pinned as its own test (not just
    /// an assertion added to the secret-family test) because the entire point is that the TWO
    /// severities must differ, which only a side-by-side pair of tests makes obvious.
    #[test]
    fn parse_sarif_ingests_a_synthetic_taint_log_pii_finding_at_medium_not_high() {
        let sarif = r#"{
          "version": "2.1.0",
          "runs": [{
            "results": [{
              "ruleId": "Users.ci.camerata.tooling.semgrep-rules.camerata.security.taint-log-pii-python",
              "level": "warning",
              "message": { "text": "Possible sensitive-data logging (taint)." },
              "locations": [{
                "physicalLocation": {
                  "artifactLocation": { "uri": "app/billing.py" },
                  "region": { "startLine": 9 }
                }
              }]
            }]
          }]
        }"#;
        let f = parse_sarif("me/svc", ScanTool::Semgrep, sarif).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(
            f[0].rule_id, "camerata.security.taint-log-pii-python",
            "path prefix must be stripped"
        );
        assert_eq!(f[0].path, "app/billing.py");
        assert_eq!(f[0].line, 9);
        assert_eq!(
            f[0].severity, "medium",
            "PII reaching a log sink must be MEDIUM — never inflated to HIGH alongside a real credential"
        );
        assert!(f[0].preview);
        assert_eq!(f[0].preview_tool.as_deref(), Some("semgrep"));
    }

    /// Shape-variant coverage for the hooks rules: a SYNTHETIC eslint SARIF payload with one
    /// `react-hooks/exhaustive-deps` result in a `.tsx` file, one in a plain `.jsx` file, and
    /// one `react-hooks/rules-of-hooks` result in a `.ts` custom-hook file — covering the
    /// realistic range of file extensions eslint's `ruleId`/location fields take no notice
    /// of (eslint reports the SAME rule ids regardless of `.jsx` vs `.tsx`, unlike semgrep's
    /// per-language rule id suffixes). No eslint binary required; this is the ingestion/
    /// mapping unit test the task calls for, exercising `parse_sarif` exactly as the live
    /// pass's ingestion path does.
    #[test]
    fn parse_sarif_ingests_synthetic_react_hooks_shape_variants() {
        let sarif = r#"{
          "version": "2.1.0",
          "runs": [{
            "results": [
              {
                "ruleId": "react-hooks/exhaustive-deps",
                "level": "warning",
                "message": { "text": "React Hook useEffect has a missing dependency: 'id'." },
                "locations": [{
                  "physicalLocation": {
                    "artifactLocation": { "uri": "src/components/Thing.tsx" },
                    "region": { "startLine": 12 }
                  }
                }]
              },
              {
                "ruleId": "react-hooks/exhaustive-deps",
                "level": "warning",
                "message": { "text": "React Hook useMemo has a missing dependency: 'value'." },
                "locations": [{
                  "physicalLocation": {
                    "artifactLocation": { "uri": "src/components/Other.jsx" },
                    "region": { "startLine": 7 }
                  }
                }]
              },
              {
                "ruleId": "react-hooks/rules-of-hooks",
                "level": "error",
                "message": {
                  "text": "React Hook \"useState\" is called conditionally. React Hooks must be called in the exact same order in every component render."
                },
                "locations": [{
                  "physicalLocation": {
                    "artifactLocation": { "uri": "src/hooks/useThing.ts" },
                    "region": { "startLine": 5 }
                  }
                }]
              }
            ]
          }]
        }"#;
        let f = parse_sarif("me/web", ScanTool::Eslint, sarif).unwrap();
        assert_eq!(f.len(), 3);

        let tsx = f
            .iter()
            .find(|x| x.path == "src/components/Thing.tsx")
            .unwrap();
        assert_eq!(tsx.rule_id, "react-hooks/exhaustive-deps");
        assert_eq!(tsx.line, 12);
        assert_eq!(
            tsx.severity, "medium",
            "SARIF 'warning' level normalizes to medium"
        );

        let jsx = f
            .iter()
            .find(|x| x.path == "src/components/Other.jsx")
            .unwrap();
        assert_eq!(jsx.rule_id, "react-hooks/exhaustive-deps");
        assert_eq!(jsx.line, 7);

        let hook = f
            .iter()
            .find(|x| x.path == "src/hooks/useThing.ts")
            .unwrap();
        assert_eq!(hook.rule_id, "react-hooks/rules-of-hooks");
        assert_eq!(hook.line, 5);
        // W24 (2026-10-20): was `"high"`. This fixture is parsed as `ScanTool::Eslint`, so
        // the style-linter cap applies - a SARIF "error" from eslint still means the project
        // chose to fail its build on that lint, not that the finding is a security high. The
        // same SARIF path carries Semgrep, which is NOT capped; see
        // `capped_preview_severity` for why the line is drawn on what a tool's severity is
        // about rather than on the file format it arrives in.
        assert_eq!(
            hook.severity, "medium",
            "an eslint SARIF 'error' is capped at medium like any other style-linter error"
        );

        // External-tool provenance on every variant — never our own deterministic tier,
        // regardless of file extension or SARIF level.
        for finding in &f {
            assert!(finding.preview, "must carry preview=true: {finding:?}");
            assert_eq!(finding.preview_tool.as_deref(), Some("eslint"));
        }
    }

    /// End-to-end ingestion + grounding, from raw SARIF all the way to OUR citation,
    /// severity, and authored remediation — the exact chain the task calls for ("an eslint
    /// finding maps to the right corpus rule with our citation/severity/fix and preview
    /// provenance"), starting from `parse_sarif` (not a hand-built `Finding`) and ending at
    /// `report_export::resolve_citation`/`resolve_fix` against the REAL loaded corpus.
    #[tokio::test]
    async fn eslint_sarif_finding_grounds_to_corpus_citation_severity_and_fix() {
        let (corpus, errors) =
            camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
        assert!(
            errors.is_empty(),
            "corpus must load cleanly, got: {errors:?}"
        );

        let sarif = r#"{
          "version": "2.1.0",
          "runs": [{
            "results": [{
              "ruleId": "react-hooks/rules-of-hooks",
              "level": "error",
              "message": { "text": "React Hook \"useState\" is called conditionally." },
              "locations": [{
                "physicalLocation": {
                  "artifactLocation": { "uri": "src/hooks/useThing.ts" },
                  "region": { "startLine": 5 }
                }
              }]
            }]
          }]
        }"#;
        let findings = parse_sarif("me/web", ScanTool::Eslint, sarif).expect("must parse SARIF");
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.rule_id, "react-hooks/rules-of-hooks");
        assert!(f.preview && f.preview_tool.as_deref() == Some("eslint"));

        let citation = crate::report_export::resolve_citation(
            &f.rule_id,
            f.preview_tool.as_deref(),
            Some(&corpus),
        );
        assert_eq!(
            citation.kind, "grounded",
            "must resolve to OUR corpus citation, not a generic preview label: {citation:?}"
        );
        assert!(
            citation.sources.iter().any(|s| s
                .url
                .contains("react.dev/reference/eslint-plugin-react-hooks")),
            "must cite the SAME React docs source JAVASCRIPT-REACT-RULES-OF-HOOKS-1 cites: \
             {citation:?}"
        );

        let fix = crate::report_export::resolve_fix(&f.rule_id, Some(&corpus), f, None);
        assert!(
            fix.as_deref()
                .is_some_and(|s| s.to_ascii_lowercase().contains("top level")),
            "must resolve OUR authored remediation (moving the hook to the top level), got: \
             {fix:?}"
        );
    }

    #[test]
    fn parse_sarif_normalizes_path_prefixed_semgrep_rule_id() {
        // When semgrep is invoked with an absolute --config path, it prefixes
        // the rule id with the dotted path.  parse_sarif must strip this prefix.
        let sarif = r#"{
          "version": "2.1.0",
          "runs": [{
            "results": [{
              "ruleId": "Users.alice.camerata.tooling.semgrep-rules.camerata.security.sql-string-concat-rust",
              "level": "error",
              "message": { "text": "Possible SQL injection" },
              "locations": [{
                "physicalLocation": {
                  "artifactLocation": { "uri": "src/db.rs" },
                  "region": { "startLine": 17 }
                }
              }]
            }]
          }]
        }"#;
        let f = parse_sarif("owner/repo", ScanTool::Semgrep, sarif).unwrap();
        assert_eq!(f.len(), 1);
        // Must be the clean id, NOT the path-prefixed form.
        assert_eq!(f[0].rule_id, "camerata.security.sql-string-concat-rust");
    }

    #[test]
    fn parse_ruff_json_into_findings() {
        let json = r#"[
          {"code":"S608","message":"Possible SQL injection","filename":"q.py","location":{"row":12,"column":5}},
          {"code":"S105","message":"Hardcoded password","filename":"c.py","location":{"row":3,"column":1}}
        ]"#;
        let f = parse_ruff_json("me/api", json).unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].rule_id, "S608");
        assert_eq!(f[0].path, "q.py");
        assert_eq!(f[0].line, 12);
        assert!(f[0].preview);
        assert_eq!(f[1].preview_tool.as_deref(), Some("ruff"));
    }

    #[test]
    fn parse_clippy_ndjson_into_findings() {
        // Two NDJSON lines: a human status line (skipped) + a compiler-message.
        let ndjson = r#"{"reason":"build-script-executed"}
{"reason":"compiler-message","message":{"code":{"code":"clippy::unwrap_used"},"level":"warning","message":"used unwrap","spans":[{"is_primary":true,"file_name":"src/main.rs","line_start":7}]}}"#;
        let f = parse_clippy_json("me/svc", ndjson).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule_id, "clippy::unwrap_used");
        assert_eq!(f[0].path, "src/main.rs");
        assert_eq!(f[0].line, 7);
        assert_eq!(f[0].severity, "medium");
        assert!(f[0].preview);
        assert_eq!(f[0].preview_tool.as_deref(), Some("clippy"));
    }

    #[test]
    fn malformed_json_is_err_not_clean() {
        assert!(parse_sarif("r", ScanTool::Semgrep, "not json").is_err());
        assert!(parse_ruff_json("r", "{not json").is_err());
        // clippy NDJSON skips bad lines rather than erroring (it interleaves text).
        let ok = parse_clippy_json("r", "garbage line\nmore garbage").unwrap();
        assert!(ok.is_empty());
    }

    // ── preview flag round-trips through serde ───────────────────────────────

    #[test]
    fn preview_flag_round_trips() {
        let f = preview_finding("me/api", ScanTool::Ruff, "a.py", 1, "S608", "medium", "msg");
        let json = serde_json::to_string(&f).unwrap();
        let back: Finding = serde_json::from_str(&json).unwrap();
        assert!(back.preview);
        assert_eq!(back.preview_tool.as_deref(), Some("ruff"));

        // Back-compat: a finding serialized WITHOUT the preview fields deserializes
        // with preview = false (the #[serde(default)] contract).
        let legacy = r#"{"repo":"r","path":"p","line":1,"rule_id":"X","severity":"high","snippet":"s","detail":"d"}"#;
        let f2: Finding = serde_json::from_str(legacy).unwrap();
        assert!(!f2.preview);
        assert_eq!(f2.preview_tool, None);
    }

    /// Defect 1 (2026-10-07 grading pass): a preview finding's `detail` text is rendered
    /// VERBATIM in the client PDF/xlsx whenever the rule has no authored floor-finding
    /// template (`client_headline_and_detail`'s fallback) — so this text must never carry
    /// internal engineering phrasing like "NOT enforced until wired into CI" or raw gate
    /// jargon. The message-present and message-absent branches both get a client-readable
    /// rewrite.
    /// W24 — a STYLE LINTER's severity is a project config choice, not a security verdict.
    ///
    /// `norm_severity` maps a tool's "error" to `high`. For Semgrep that is right: it is a
    /// security scanner and its error level is a security judgement. For ESLint and Clippy it
    /// is not — "error" there means the project chose to fail its build on that lint, which
    /// says nothing about exploitability.
    ///
    /// The 2026-10-18 hearth export shipped 31 ESLint preview rows at HIGH, outranking every
    /// planted security HIGH in the queue. The rules were `no-non-null-assertion`,
    /// `no-explicit-any`, `react-hooks/exhaustive-deps` and `no-console`. Each is a real
    /// code-quality signal and none is remotely a HIGH, and putting them above a genuine
    /// exposure spends the reader's attention in the wrong order.
    #[test]
    fn req_w24_style_linter_errors_do_not_outrank_security_findings() {
        let sev = |tool: ScanTool, raw: &str| {
            preview_finding("me/api", tool, "a.ts", 1, "some-rule", raw, "msg").severity
        };

        // The two pure style linters cap at medium however strict the project's config is.
        for tool in [ScanTool::Eslint, ScanTool::Clippy] {
            assert_eq!(
                sev(tool, "error"),
                "medium",
                "{tool:?} 'error' is the project's own strictness setting, not a security \
                 verdict - it must not outrank a real exposure"
            );
            // Below the cap is untouched: a warning is still a warning.
            assert_eq!(sev(tool, "warning"), "medium");
            assert_eq!(sev(tool, "note"), "low");
        }

        // Semgrep IS a security scanner; its error level is a security judgement and keeps
        // its mapping. Ruff likewise carries bandit-derived security rules (the S-prefix
        // family, e.g. S608 hardcoded SQL), so it is not a pure style linter either.
        assert_eq!(sev(ScanTool::Semgrep, "error"), "high");
        assert_eq!(sev(ScanTool::Ruff, "error"), "high");
    }

    #[test]
    fn preview_finding_detail_has_no_engineering_phrasing() {
        let no_message = preview_finding("me/api", ScanTool::Ruff, "a.py", 1, "S608", "medium", "");
        for banned in ["NOT enforced until wired into CI", "wired into CI"] {
            assert!(
                !no_message.detail.contains(banned),
                "preview detail (no message) must not leak engineering phrasing {banned:?}: \
                 {}",
                no_message.detail
            );
        }
        assert!(
            no_message.detail.contains("Ruff")
                || no_message.detail.to_ascii_lowercase().contains("ruff")
        );

        let with_message = preview_finding(
            "me/api",
            ScanTool::Ruff,
            "a.py",
            1,
            "S608",
            "medium",
            "raw msg",
        );
        for banned in ["NOT enforced until wired into CI", "wired into CI"] {
            assert!(
                !with_message.detail.contains(banned),
                "preview detail (with message) must not leak engineering phrasing {banned:?}: \
                 {}",
                with_message.detail
            );
        }
        assert!(with_message.detail.contains("raw msg"));
    }

    // ── graceful no-tool path emits a NOTE, never a clean ─────────────────────

    #[tokio::test]
    async fn missing_tool_emits_note_not_clean() {
        let rules = vec![rule_with(
            "PY-A",
            EnforcementKind::Mechanical,
            false,
            &["Ruff: S608"],
        )];
        let lookup = lookup_over(&rules);
        // A non-existent dir + (almost certainly) absent `ruff` on the test host:
        // the pass must emit a coverage NOTE, NOT an empty (clean) result and NOT a finding.
        let dir = std::path::Path::new("/nonexistent-camerata-scan-preview-dir");
        let (findings, notes, attempted) =
            run_scan_tools("me/api", dir, &[selected("PY-A")], &lookup, None, None).await;
        assert!(
            findings.is_empty(),
            "missing tool must yield no finding rows"
        );
        assert!(!notes.is_empty(), "missing tool must yield a coverage note");
        assert!(notes
            .iter()
            .any(|n| n.message.contains("Could not preview") || !n.tool.is_empty()));
        assert!(
            attempted.contains("ruff"),
            "ruff was routed via group_by_tool, so it must count as attempted even though it \
             then failed: {attempted:?}"
        );
    }

    #[test]
    fn group_by_tool_skips_architectural_rules() {
        // An architectural rule must be SKIPPED entirely by group_by_tool —
        // not ungrouped (no note), not routed to a tool.
        let rules = vec![
            rule_with(
                "MECH-1",
                EnforcementKind::Mechanical,
                false,
                &["clippy: unwrap_used"],
            ),
            rule_with(
                "ARCH-1",
                EnforcementKind::Architectural,
                false,
                &["clippy: some_ast_check"],
            ),
        ];
        let lookup = lookup_over(&rules);
        let sel = vec![selected("MECH-1"), selected("ARCH-1")];
        let (by_tool, ungrouped) = group_by_tool(&sel, &lookup, None);
        // MECH-1 routes to clippy
        assert!(by_tool
            .get(&ScanTool::Clippy)
            .map(|v| v.iter().any(|s| s.id == "MECH-1"))
            .unwrap_or(false));
        // ARCH-1 must not appear anywhere
        assert!(
            !by_tool.values().flatten().any(|s| s.id == "ARCH-1"),
            "architectural must not route to a tool"
        );
        assert!(
            !ungrouped.iter().any(|s| s.id == "ARCH-1"),
            "architectural must not be ungrouped (no note)"
        );
    }

    #[tokio::test]
    async fn missing_tool_emits_coverage_note_not_finding() {
        let rules = vec![rule_with(
            "PY-A",
            EnforcementKind::Mechanical,
            false,
            &["Ruff: S608"],
        )];
        let lookup = lookup_over(&rules);
        let dir = std::path::Path::new("/nonexistent-camerata-scan-preview-dir");
        let (findings, notes, attempted) =
            run_scan_tools("me/api", dir, &[selected("PY-A")], &lookup, None, None).await;
        assert!(
            findings.is_empty(),
            "a missing tool must yield NO finding row"
        );
        assert!(
            !notes.is_empty(),
            "a missing tool must yield a coverage note"
        );
        assert!(attempted.contains("ruff"), "{attempted:?}");
        assert!(notes
            .iter()
            .any(|n| n.message.contains("Could not preview") || !n.tool.is_empty()));
    }

    #[test]
    fn note_finding_is_preview_and_not_active() {
        let n = note_finding("me/api", "ruff", "could not run");
        assert!(n.preview);
        assert_eq!(n.preview_tool.as_deref(), Some("ruff"));
        assert_ne!(
            n.status, "active",
            "a note must not be an enforced/active hit"
        );
    }

    // ── preview_tool_ids_for_rules ────────────────────────────────────────────

    /// `preview_tool_ids_for_rules` must return the same tool names that
    /// `run_scan_tools` would register on the job, without executing any tool.
    /// Used by the pre-declaration step so the progress denominator ("N") reflects
    /// the full pipeline before any tool starts.
    #[test]
    fn preview_tool_ids_returns_distinct_tool_names() {
        // Three mechanical rules backed by two distinct tools (clippy + ruff).
        let rules = vec![
            rule_with(
                "R-1",
                EnforcementKind::Mechanical,
                false,
                &["clippy: unwrap_used"],
            ),
            rule_with(
                "R-2",
                EnforcementKind::Mechanical,
                false,
                &["clippy: expect_used"],
            ),
            rule_with("R-3", EnforcementKind::Mechanical, false, &["Ruff: S608"]),
        ];
        let lookup = lookup_over(&rules);
        let sel = vec![selected("R-1"), selected("R-2"), selected("R-3")];
        let ids = preview_tool_ids_for_rules(&sel, &lookup, None);
        // Two distinct tools: clippy and ruff (order: BTreeMap order = clippy < ruff).
        assert_eq!(
            ids.len(),
            2,
            "two distinct tools for three rules: {:?}",
            ids
        );
        assert!(ids.contains(&"clippy".to_string()), "must include clippy");
        assert!(ids.contains(&"ruff".to_string()), "must include ruff");
    }

    #[test]
    fn preview_tool_ids_empty_when_no_mechanical_rules() {
        // When no mechanical rules are selected, the tool id list is empty.
        let rules = vec![rule_with(
            "ARCH-1",
            EnforcementKind::Architectural,
            false,
            &["clippy: some_ast_check"],
        )];
        let lookup = lookup_over(&rules);
        let sel = vec![selected("ARCH-1")];
        let ids = preview_tool_ids_for_rules(&sel, &lookup, None);
        assert!(
            ids.is_empty(),
            "architectural rules yield no preview tool ids"
        );
    }

    #[test]
    fn preview_tool_ids_includes_unrouted_for_unknown_linter() {
        // A mechanical rule whose linter is not recognized → "unrouted" in the list.
        let rules = vec![rule_with(
            "JAVA-1",
            EnforcementKind::Mechanical,
            false,
            &["Checkstyle: com.puppycrawl.tools.checkstyle.checks.naming.ConstantNameCheck"],
        )];
        let lookup = lookup_over(&rules);
        let sel = vec![selected("JAVA-1")];
        let ids = preview_tool_ids_for_rules(&sel, &lookup, None);
        assert!(
            ids.contains(&"unrouted".to_string()),
            "an ungrouped rule must produce 'unrouted' in the id list: {:?}",
            ids
        );
    }

    // ── FIX 1: stack-aware language gating tests ─────────────────────────────

    /// `languages_from_files`: verify extension→language mapping for key extensions.
    #[test]
    fn languages_from_files_maps_extensions() {
        let files: Vec<(String, String)> = vec![
            ("src/main.rs".to_string(), String::new()),
            ("app.py".to_string(), String::new()),
            ("index.ts".to_string(), String::new()),
            ("utils.jsx".to_string(), String::new()),
            ("Cargo.toml".to_string(), String::new()), // no extension match → ignored
        ];
        let langs = languages_from_files(&files);
        assert!(langs.contains("Rust"), "should detect Rust from .rs");
        assert!(langs.contains("Python"), "should detect Python from .py");
        assert!(
            langs.contains("TypeScript"),
            "should detect TypeScript from .ts"
        );
        assert!(
            langs.contains("JavaScript"),
            "should detect JavaScript from .jsx"
        );
        assert!(!langs.contains("TOML"), "TOML has no language label");
    }

    /// `tool_languages_present` — None passthrough (backward-compat).
    #[test]
    fn tool_languages_present_none_always_passes() {
        for tool in [
            ScanTool::Clippy,
            ScanTool::Ruff,
            ScanTool::Eslint,
            ScanTool::Semgrep,
        ] {
            assert!(
                tool_languages_present(tool, None),
                "{:?} must pass when present_languages is None",
                tool
            );
        }
    }

    /// `tool_languages_present` — Rust-only repo: clippy+semgrep pass, eslint+ruff don't.
    #[test]
    fn tool_languages_present_rust_only() {
        let langs: HashSet<String> = ["Rust".to_string()].into();
        assert!(
            tool_languages_present(ScanTool::Clippy, Some(&langs)),
            "clippy needs Rust → present"
        );
        assert!(
            tool_languages_present(ScanTool::Semgrep, Some(&langs)),
            "semgrep supports Rust → present"
        );
        assert!(
            !tool_languages_present(ScanTool::Ruff, Some(&langs)),
            "ruff needs Python → absent"
        );
        assert!(
            !tool_languages_present(ScanTool::Eslint, Some(&langs)),
            "eslint needs JS/TS → absent"
        );
    }

    /// `tool_languages_present` — empty lang set → all tools pass (conservative).
    #[test]
    fn tool_languages_present_empty_set_is_permissive() {
        let langs: HashSet<String> = HashSet::new();
        for tool in [
            ScanTool::Clippy,
            ScanTool::Ruff,
            ScanTool::Eslint,
            ScanTool::Semgrep,
        ] {
            assert!(
                tool_languages_present(tool, Some(&langs)),
                "{:?} must pass on an empty language set (conservative)",
                tool
            );
        }
    }

    /// FIX 1 core: a JS rule selected on a Rust-only repo must NOT include eslint in
    /// the pre-declared tool list (`preview_tool_ids_for_rules`) or in the group.
    #[test]
    fn stack_gating_rust_only_repo_excludes_eslint_and_ruff() {
        let rules = vec![
            rule_with(
                "RUST-1",
                EnforcementKind::Mechanical,
                false,
                &["clippy: unwrap_used"],
            ),
            rule_with(
                "JS-1",
                EnforcementKind::Mechanical,
                false,
                &["eslint: eqeqeq"],
            ),
            rule_with("PY-1", EnforcementKind::Mechanical, false, &["Ruff: S608"]),
            rule_with("SG-1", EnforcementKind::Mechanical, false, &["semgrep"]),
        ];
        let lookup = lookup_over(&rules);
        let sel = vec![
            selected("RUST-1"),
            selected("JS-1"),
            selected("PY-1"),
            selected("SG-1"),
        ];

        // Rust-only language set.
        let rust_only: HashSet<String> = ["Rust".to_string()].into();

        let (by_tool, _ungrouped) = group_by_tool(&sel, &lookup, Some(&rust_only));
        assert!(
            by_tool.contains_key(&ScanTool::Clippy),
            "clippy must run (Rust present)"
        );
        assert!(
            by_tool.contains_key(&ScanTool::Semgrep),
            "semgrep must run (Rust is semgrep-supported)"
        );
        assert!(
            !by_tool.contains_key(&ScanTool::Eslint),
            "eslint must be OMITTED (no JS/TS)"
        );
        assert!(
            !by_tool.contains_key(&ScanTool::Ruff),
            "ruff must be OMITTED (no Python)"
        );

        // The pre-declared IDs must match the stack-gated set.
        let ids = preview_tool_ids_for_rules(&sel, &lookup, Some(&rust_only));
        assert!(
            ids.contains(&"clippy".to_string()),
            "clippy in pre-declared ids"
        );
        assert!(
            ids.contains(&"semgrep".to_string()),
            "semgrep in pre-declared ids"
        );
        assert!(
            !ids.contains(&"eslint".to_string()),
            "eslint NOT in pre-declared ids for Rust-only repo"
        );
        assert!(
            !ids.contains(&"ruff".to_string()),
            "ruff NOT in pre-declared ids for Rust-only repo"
        );
    }

    /// FIX 1: even if the selected JS rule was the ONLY selection, the stack gate
    /// must omit eslint entirely from a Rust-only repo (no false "✓ 0").
    #[test]
    fn stack_gating_js_rule_on_rust_repo_yields_no_eslint() {
        let rules = vec![rule_with(
            "JS-ONLY",
            EnforcementKind::Mechanical,
            false,
            &["eslint: no-eval"],
        )];
        let lookup = lookup_over(&rules);
        let sel = vec![selected("JS-ONLY")];
        let rust_only: HashSet<String> = ["Rust".to_string()].into();

        let ids = preview_tool_ids_for_rules(&sel, &lookup, Some(&rust_only));
        assert!(
            !ids.contains(&"eslint".to_string()),
            "a JS rule on a Rust-only repo must produce NO eslint tool id: {:?}",
            ids
        );
    }

    // ── is_in_repo_scope ─────────────────────────────────────────────────────

    /// Helper: build a repo root path for scoping tests.
    fn repo(path: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(path)
    }

    /// Rust stdlib paths emitted by clippy (absolute, outside any repo) are dropped.
    #[test]
    fn scope_excludes_rustc_stdlib_path() {
        let root = repo("/home/user/myproject");
        assert!(
            !is_in_repo_scope(
                &root,
                "/rustc/abcdef1234567890/library/core/src/macros/mod.rs"
            ),
            "rustc stdlib path must be excluded"
        );
        assert!(
            !is_in_repo_scope(&root, "/rustc/abcdef1234567890/library/std/src/lib.rs"),
            "rustc std lib path must be excluded"
        );
    }

    /// Cargo build output under `target/` is dropped (generated, not user source).
    #[test]
    fn scope_excludes_target_directory() {
        let root = repo("/home/user/myproject");
        assert!(
            !is_in_repo_scope(
                &root,
                "/home/user/myproject/target/debug/build/pkg/out/v2_generated.rs"
            ),
            "target/debug/build path must be excluded"
        );
        assert!(
            !is_in_repo_scope(&root, "/home/user/myproject/target/release/libfoo.rlib"),
            "target/release build artifact must be excluded"
        );
    }

    /// Files inside the `target/` tree are excluded even with relative paths.
    #[test]
    fn scope_excludes_target_relative() {
        let root = repo("/home/user/myproject");
        assert!(
            !is_in_repo_scope(&root, "target/debug/build/pkg/out/v2_generated.rs"),
            "relative target/ path must be excluded"
        );
    }

    /// W21 — an external tool's absolute path becomes repo-relative before it can reach an
    /// artifact.
    ///
    /// The 2026-10-18 hearth export shipped 32 of 237 rows carrying the auditor's own
    /// filesystem layout, all of them `Preview: eslint`. That discloses the auditor's
    /// username and directory habits to the client, and it makes the path useless to them:
    /// they cannot open or search a path rooted on a machine they have no access to.
    #[test]
    fn external_tool_absolute_paths_become_repo_relative() {
        let root = repo("/Users/auditor/Documents/Repos/clients/acme");

        // The exact shape that leaked, modulo the name.
        assert_eq!(
            repo_relative_path(
                &root,
                "/Users/auditor/Documents/Repos/clients/acme/app/admin/page.tsx"
            ),
            "app/admin/page.tsx"
        );
        // A path that is already relative is untouched.
        assert_eq!(
            repo_relative_path(&root, "app/api/checkout/route.ts"),
            "app/api/checkout/route.ts"
        );
        // Synthetic placeholders our own parsers inject must survive verbatim, since
        // `is_in_repo_scope` deliberately keeps them.
        assert_eq!(repo_relative_path(&root, "(repo)"), "(repo)");
        assert_eq!(
            repo_relative_path(&root, "(scan preview)"),
            "(scan preview)"
        );
        // Nested deeply, and a repo whose own path contains a space (the real fixture did).
        let spaced = repo("/Users/auditor/Camerata tests/hearth");
        assert_eq!(
            repo_relative_path(
                &spaced,
                "/Users/auditor/Camerata tests/hearth/app/listings/[id]/page.tsx"
            ),
            "app/listings/[id]/page.tsx"
        );
        // Outside the repo: left alone on purpose. `is_in_repo_scope` is what drops these,
        // and quietly reshaping one here would hide that decision.
        assert_eq!(
            repo_relative_path(&root, "/rustc/abcdef/library/core/src/macros/mod.rs"),
            "/rustc/abcdef/library/core/src/macros/mod.rs"
        );
    }

    /// The normalisation must never produce a path that still names the auditor's machine.
    /// Asserted as its own property rather than folded into the case above, because this is
    /// the client-facing consequence and it should fail loudly and by name.
    #[test]
    fn no_normalised_path_retains_the_auditor_prefix() {
        let root = repo("/Users/auditor/Documents/Repos/clients/acme");
        for raw in [
            "/Users/auditor/Documents/Repos/clients/acme/app/admin/page.tsx",
            "/Users/auditor/Documents/Repos/clients/acme/supabase/migrations/0001_init.sql",
            "/Users/auditor/Documents/Repos/clients/acme/components/Btn.tsx",
        ] {
            let out = repo_relative_path(&root, raw);
            assert!(
                !out.contains("/Users/") && !out.contains("auditor"),
                "`{out}` still discloses the auditor's filesystem"
            );
            assert!(
                !out.starts_with('/'),
                "`{out}` must be relative, not absolute"
            );
        }
    }

    /// User source files in `src/` are always kept.
    #[test]
    fn scope_keeps_src_main_rs() {
        let root = repo("/home/user/myproject");
        assert!(
            is_in_repo_scope(&root, "/home/user/myproject/src/main.rs"),
            "src/main.rs (absolute) must be kept"
        );
        assert!(
            is_in_repo_scope(&root, "src/lib.rs"),
            "src/lib.rs (relative) must be kept"
        );
        assert!(
            is_in_repo_scope(&root, "src/main.rs"),
            "src/main.rs (relative) must be kept"
        );
    }

    /// `dist-server/server.mjs` — bundled server output — is excluded.
    #[test]
    fn scope_excludes_dist_server() {
        let root = repo("/home/user/myproject");
        assert!(
            !is_in_repo_scope(&root, "/home/user/myproject/dist-server/server.mjs"),
            "dist-server/ bundle must be excluded"
        );
        assert!(
            !is_in_repo_scope(&root, "dist-server/worker.js"),
            "relative dist-server/ path must be excluded"
        );
    }

    /// Files under `dist/` (JS/CSS bundles) are excluded.
    #[test]
    fn scope_excludes_dist() {
        let root = repo("/home/user/myproject");
        assert!(
            !is_in_repo_scope(&root, "/home/user/myproject/dist/app.js"),
            "dist/ bundle must be excluded"
        );
        assert!(
            !is_in_repo_scope(&root, "dist/index.css"),
            "relative dist/ path must be excluded"
        );
    }

    /// Files whose name ends with `_generated.rs` are excluded (Cargo OUT_DIR
    /// convention: proto/codegen emits these via build scripts).
    #[test]
    fn scope_excludes_generated_rs_suffix() {
        let root = repo("/home/user/myproject");
        assert!(
            !is_in_repo_scope(&root, "src/v2_generated.rs"),
            "file ending _generated.rs must be excluded"
        );
        assert!(
            !is_in_repo_scope(&root, "/home/user/myproject/src/proto_generated.rs"),
            "absolute _generated.rs must be excluded"
        );
    }

    /// Files whose name ends with `.generated.<ext>` are excluded.
    #[test]
    fn scope_excludes_dot_generated_ext() {
        let root = repo("/home/user/myproject");
        assert!(
            !is_in_repo_scope(&root, "src/schema.generated.ts"),
            "file with .generated.ts suffix must be excluded"
        );
        assert!(
            !is_in_repo_scope(&root, "src/api.generated.js"),
            "file with .generated.js suffix must be excluded"
        );
    }

    /// Files inside an `out/` segment under a build-script path are excluded.
    #[test]
    fn scope_excludes_out_dir_segment() {
        let root = repo("/home/user/myproject");
        assert!(
            !is_in_repo_scope(
                &root,
                "/home/user/myproject/target/debug/build/somepackage/out/bindings.rs"
            ),
            "OUT_DIR (build/.../out/) path must be excluded"
        );
    }

    /// Synthetic placeholder paths that our parsers produce (`"(repo)"`,
    /// `"(scan preview)"`) are always kept even if they look unusual.
    #[test]
    fn scope_keeps_synthetic_placeholders() {
        let root = repo("/home/user/myproject");
        assert!(
            is_in_repo_scope(&root, "(repo)"),
            "synthetic (repo) placeholder must be kept"
        );
        assert!(
            is_in_repo_scope(&root, "(scan preview)"),
            "synthetic (scan preview) placeholder must be kept"
        );
    }

    /// A file from a completely different repo / home directory is dropped.
    #[test]
    fn scope_excludes_different_repo() {
        let root = repo("/home/user/myproject");
        assert!(
            !is_in_repo_scope(&root, "/home/user/otherproject/src/lib.rs"),
            "path from a different repo must be excluded"
        );
        assert!(
            !is_in_repo_scope(&root, "/home/user/.cargo/registry/src/foo/src/lib.rs"),
            "cargo registry path must be excluded"
        );
    }

    // ── streaming path liveness tests ────────────────────────────────────────

    /// `run_capture_stdout` with `on_progress = Some` fires the heartbeat for each
    /// line of output from the child process. This is the output-line signal half
    /// of the rivet fix: even before clippy emits any JSON result it produces
    /// compiler progress lines that keep the job alive.
    #[tokio::test]
    async fn run_capture_stdout_streaming_fires_heartbeat_per_line() {
        use camerata_agent::HeartbeatFn;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::Arc;

        let count = Arc::new(AtomicU64::new(0));
        let count_cb = count.clone();
        let cb: HeartbeatFn = Arc::new(move || {
            count_cb.fetch_add(1, Ordering::Relaxed);
        });

        // `echo` outputs exactly 3 lines; printf packs them separated by newlines.
        // We use `sh -c 'printf ...'` for portability across test environments.
        let dir = std::env::temp_dir();
        let (stdout, ok, _stderr) = run_capture_stdout(
            &dir,
            "sh",
            &["-c", "printf 'line1\\nline2\\nline3\\n'"],
            Some(&cb),
        )
        .await
        .expect("sh should be on PATH");

        assert!(ok, "sh exit should be 0");
        assert_eq!(stdout.lines().count(), 3, "stdout should contain 3 lines");
        assert_eq!(
            count.load(Ordering::Relaxed),
            3,
            "heartbeat should fire once per output line"
        );
    }

    /// `run_capture_stdout` with `on_progress = None` does NOT require a streaming
    /// path and still returns correct output (the buffered fast path is unchanged).
    #[tokio::test]
    async fn run_capture_stdout_no_progress_still_works() {
        let dir = std::env::temp_dir();
        let (stdout, ok, _stderr) =
            run_capture_stdout(&dir, "sh", &["-c", "printf 'hello\\nworld\\n'"], None)
                .await
                .expect("sh should be on PATH");

        assert!(ok);
        assert!(stdout.contains("hello"));
        assert!(stdout.contains("world"));
    }

    // ── req_c11: the eslint preview failed WHOLESALE, at zero coverage and nonzero cost ──
    //
    // `docs/plans/2026-10-09_audit-findings-tracker.md`, C11 / Fable priority #4:
    //
    //   > The ESLint preview pass FAILED WHOLESALE on these runs: `Could not preview 10
    //   > rule(s) with eslint: EOF while parsing a value at line 1 column 0`.
    //
    // Root cause, reproduced directly against the real provisioned workspace before any code
    // changed: `EOF while parsing a value at line 1 column 0` is `serde_json` on an EMPTY
    // string, and eslint's stdout was empty because
    // `--format @microsoft/eslint-formatter-sarif` names an npm PACKAGE. eslint resolves a
    // non-built-in `--format` with `ModuleResolver.resolve(pkg, path.join(cwd, "…"))` where
    // cwd is the SCANNED REPO — not Camerata's tooling workspace, the only place that package
    // is installed. Resolution failed, eslint fell back to its built-in formatter directory,
    // the import failed there too, and it exited 2 having written
    // `There was a problem loading formatter: …` to stderr and NOTHING to stdout.
    //
    // `run_capture_stdout` then discarded that stderr, so the only evidence that ever reached
    // an operator was a parse error about column 0 of an empty document.
    //
    // These tests pin each link of that chain separately, so a regression names its own cause.

    /// req_c11: the formatter Camerata asks for must be a BUILT-IN eslint formatter, never an
    /// npm package name. This is the single assertion that would have prevented C11 outright.
    ///
    /// FAILS BEFORE THE FIX: the arm passed the literal
    /// `"@microsoft/eslint-formatter-sarif"`, which is a package name (it contains a `/` and
    /// an `@` scope) and is not in eslint's built-in set.
    #[test]
    fn req_c11_eslint_formatter_is_built_in_never_an_npm_package_name() {
        // eslint's built-in formatters, as shipped inside the `eslint` package itself. Any
        // name outside this set is resolved as a module relative to eslint's CWD.
        const BUILT_IN: [&str; 4] = ["json", "stylish", "html", "json-with-metadata"];
        assert!(
            BUILT_IN.contains(&ESLINT_FORMATTER),
            "`{ESLINT_FORMATTER}` is not one of eslint's built-in formatters, so eslint will \
             try to resolve it as an npm module relative to the SCANNED REPO — the exact C11 \
             failure. Built-ins: {BUILT_IN:?}"
        );
        assert!(
            !ESLINT_FORMATTER.contains('/') && !ESLINT_FORMATTER.contains('@'),
            "a `/` or `@` in the formatter name means it is a package specifier, not a \
             built-in: {ESLINT_FORMATTER}"
        );

        let args = eslint_args("/tmp/camerata.config.mjs", &["eqeqeq".to_string()]);
        let fmt_pos = args
            .iter()
            .position(|a| a == "--format")
            .expect("eslint must always be given an explicit --format");
        assert_eq!(args[fmt_pos + 1], ESLINT_FORMATTER);
        assert!(
            !args.iter().any(|a| a.contains("eslint-formatter")),
            "no argument may name an eslint formatter PACKAGE: {args:?}"
        );
    }

    /// req_c11: the full argument vector's shape — config, built-in formatter, one `--rule`
    /// override per selector, and the `.` target last.
    #[test]
    fn req_c11_eslint_args_carry_the_config_and_one_rule_override_per_selector() {
        let selectors = vec![
            "eqeqeq".to_string(),
            "react-hooks/exhaustive-deps".to_string(),
        ];
        let args = eslint_args("/cfg/camerata.config.mjs", &selectors);
        assert_eq!(args.first().map(String::as_str), Some("--config"));
        assert_eq!(
            args.get(1).map(String::as_str),
            Some("/cfg/camerata.config.mjs")
        );
        assert_eq!(args.last().map(String::as_str), Some("."));
        assert_eq!(
            args.iter().filter(|a| *a == "--rule").count(),
            2,
            "one --rule per selector: {args:?}"
        );
        assert!(
            args.iter().any(|a| a == r#"{"eqeqeq": "error"}"#),
            "{args:?}"
        );
        assert!(
            args.iter()
                .any(|a| a == r#"{"react-hooks/exhaustive-deps": "error"}"#),
            "{args:?}"
        );
    }

    /// req_c11: an EMPTY stdout must be reported as "the tool produced no output", with the
    /// tool's own stderr as the evidence — never as a bare JSON parse position.
    ///
    /// FAILS BEFORE THE FIX: the `Err` arm formatted the raw `serde_json` error, producing
    /// exactly `Could not preview 10 rule(s) with eslint: EOF while parsing a value at line 1
    /// column 0`, and stderr had already been dropped by `run_capture_stdout`, so the real
    /// cause was unrecoverable from the note. This test asserts the inverse of that string.
    #[test]
    fn req_c11_an_empty_stdout_error_names_the_tool_stderr_not_a_parse_position() {
        let parse_err = serde_json::from_str::<serde_json::Value>("")
            .err()
            .map(anyhow::Error::from)
            .expect("the empty string must be a serde_json error");
        assert!(
            parse_err.to_string().contains("EOF while parsing"),
            "sanity: this is the C11 symptom string: {parse_err}"
        );

        let stderr = "There was a problem loading formatter: \
                      /x/node_modules/eslint/lib/cli-engine/formatters/@microsoft/\
                      eslint-formatter-sarif.js";
        let err = tool_output_error(ScanTool::Eslint, "", stderr, Some(&parse_err));
        let msg = err.to_string();

        assert!(
            !msg.contains("EOF while parsing"),
            "an empty stdout must not be reported as a JSON parse position — that is a \
             tautology that hides the cause: {msg}"
        );
        assert!(
            msg.contains("produced NO output"),
            "must state plainly that the tool emitted nothing: {msg}"
        );
        assert!(
            msg.contains("problem loading formatter"),
            "must carry the tool's own stderr as the evidence: {msg}"
        );
        assert!(msg.contains("eslint"), "must name the tool: {msg}");
    }

    /// req_c11 companion: a NON-empty stdout that will not parse keeps the parse error (it is
    /// genuinely informative there) and still appends the stderr evidence.
    #[test]
    fn req_c11_an_unparseable_non_empty_stdout_keeps_the_parse_error_and_adds_stderr() {
        let parse_err = serde_json::from_str::<serde_json::Value>("{\"truncated\":")
            .err()
            .map(anyhow::Error::from)
            .expect("truncated JSON must be a serde_json error");
        let err = tool_output_error(
            ScanTool::Eslint,
            "{\"truncated\":",
            "warning: heap limit reached",
            Some(&parse_err),
        );
        let msg = err.to_string();
        assert!(msg.contains("could not parse"), "{msg}");
        assert!(msg.contains("heap limit reached"), "{msg}");
    }

    /// req_c11: stderr is RETURNED by `run_capture_stdout`, on both the buffered and the
    /// streaming path — it used to be dropped on the floor, which is why C11 was diagnosable
    /// only by re-running eslint by hand outside Camerata.
    #[tokio::test]
    async fn req_c11_run_capture_stdout_returns_stderr_on_both_paths() {
        use camerata_agent::HeartbeatFn;
        use std::sync::Arc;

        let dir = std::env::temp_dir();
        let script = "printf 'out\\n'; printf 'the real cause\\n' >&2; exit 2";

        let (stdout, ok, stderr) = run_capture_stdout(&dir, "sh", &["-c", script], None)
            .await
            .expect("sh should be on PATH");
        assert!(!ok, "the script exits 2");
        assert!(stdout.contains("out"));
        assert!(
            stderr.contains("the real cause"),
            "buffered path must return stderr: {stderr:?}"
        );

        let cb: HeartbeatFn = Arc::new(|| {});
        let (stdout, ok, stderr) = run_capture_stdout(&dir, "sh", &["-c", script], Some(&cb))
            .await
            .expect("sh should be on PATH");
        assert!(!ok);
        assert!(stdout.contains("out"));
        assert!(
            stderr.contains("the real cause"),
            "streaming path must return stderr too: {stderr:?}"
        );
    }

    /// req_c11: the plugin-package derivation follows eslint's own naming convention, so a
    /// corpus rule citing a plugin Camerata has never heard of is still classified correctly
    /// rather than mistaken for a core rule (which would hand it to `--rule` and abort the
    /// whole invocation).
    #[test]
    fn req_c11_eslint_plugin_package_is_derived_from_the_selector_namespace() {
        assert_eq!(eslint_plugin_package_for_selector("eqeqeq"), None);
        assert_eq!(eslint_plugin_package_for_selector("no-var"), None);
        assert_eq!(
            eslint_plugin_package_for_selector("react-hooks/exhaustive-deps").as_deref(),
            Some("eslint-plugin-react-hooks")
        );
        assert_eq!(
            eslint_plugin_package_for_selector("jest/no-focused-tests").as_deref(),
            Some("eslint-plugin-jest")
        );
        assert_eq!(
            eslint_plugin_package_for_selector("@typescript-eslint/no-explicit-any").as_deref(),
            Some("@typescript-eslint/eslint-plugin")
        );
        assert_eq!(
            eslint_plugin_package_for_selector("@angular-eslint/prefer-inject").as_deref(),
            Some("@angular-eslint/eslint-plugin")
        );
        assert_eq!(
            eslint_plugin_package_for_selector("@scope/someplugin/some-rule").as_deref(),
            Some("@scope/eslint-plugin-someplugin")
        );
        // A plugin nobody wired still resolves to a package name, never to `None`.
        assert_eq!(
            eslint_plugin_package_for_selector("brand-new-plugin/a-rule").as_deref(),
            Some("eslint-plugin-brand-new-plugin")
        );
    }

    /// req_c11: the SECOND wholesale-abort path. A selector whose plugin package is absent
    /// from the provisioned workspace must be WITHHELD from `--rule` (where it would be a
    /// fatal config error killing every other rule in the same run) and disclosed, while
    /// every resolvable selector still runs.
    ///
    /// Verified against the real eslint 9.39.4 in the provisioned workspace before writing
    /// this: `--rule '{"@typescript-eslint/no-explicit-any":"error"}'` with that plugin
    /// absent exits 2 with `A configuration object specifies rule …, but could not find
    /// plugin …` and an empty stdout — indistinguishable at the call site from the formatter
    /// failure above.
    #[test]
    fn req_c11_a_selector_whose_plugin_package_is_absent_is_withheld_not_handed_to_eslint() {
        let ws = tempfile::TempDir::new().expect("tempdir");
        let present = ws
            .path()
            .join("node_modules")
            .join("eslint-plugin-react-hooks");
        std::fs::create_dir_all(&present).expect("mkdir");
        std::fs::write(present.join("package.json"), "{}").expect("write");

        let selectors = vec![
            "eqeqeq".to_string(),                             // core: always runnable
            "react-hooks/exhaustive-deps".to_string(),        // plugin present
            "@typescript-eslint/no-explicit-any".to_string(), // plugin ABSENT
            "jest/no-focused-tests".to_string(),              // plugin ABSENT
        ];
        let (runnable, unresolvable) = partition_eslint_selectors(&selectors, ws.path());

        assert_eq!(
            runnable,
            vec![
                "eqeqeq".to_string(),
                "react-hooks/exhaustive-deps".to_string()
            ],
            "a core rule and a provisioned plugin's rule must still run"
        );
        let missing: Vec<&str> = unresolvable.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(
            missing,
            vec![
                "@typescript-eslint/no-explicit-any",
                "jest/no-focused-tests"
            ],
            "an absent plugin's rule must be withheld, never attempted"
        );
        assert_eq!(
            unresolvable[0].1, "@typescript-eslint/eslint-plugin",
            "the withheld entry must name the package that would fix it"
        );
    }

    /// req_c11: an empty directory marker (an interrupted `npm install`) counts as ABSENT —
    /// the conservative direction, since a present-but-broken package is the fatal case.
    #[test]
    fn req_c11_a_package_dir_without_a_package_json_counts_as_absent() {
        let ws = tempfile::TempDir::new().expect("tempdir");
        std::fs::create_dir_all(ws.path().join("node_modules").join("eslint-plugin-jest"))
            .expect("mkdir");
        let (runnable, unresolvable) =
            partition_eslint_selectors(&["jest/no-focused-tests".to_string()], ws.path());
        assert!(runnable.is_empty());
        assert_eq!(unresolvable.len(), 1);
    }

    /// req_c11: `parse_eslint_json` over REAL `eslint --format json` output (captured verbatim
    /// from eslint 9.39.4 driving the bundled config over a two-violation fixture): both rule
    /// findings are ingested with the right path, line, rule id and severity mapping, and the
    /// run shape reports a fully-parseable repo.
    #[test]
    fn req_c11_parse_eslint_json_ingests_real_eslint_output() {
        let raw = r#"[
          {
            "filePath": "/repo/app.js",
            "messages": [
              {"ruleId":"eqeqeq","severity":2,"message":"Expected '===' and instead saw '=='.","line":2,"column":9},
              {"ruleId":"no-console","severity":1,"message":"Unexpected console statement.","line":2,"column":18},
              {"ruleId":"no-var","severity":2,"message":"Unexpected var, use let or const instead.","line":3,"column":3}
            ],
            "errorCount":2,"fatalErrorCount":0,"warningCount":1
          },
          {
            "filePath": "/repo/clean.js",
            "messages": [],
            "errorCount":0,"fatalErrorCount":0,"warningCount":0
          }
        ]"#;
        let (findings, shape) = parse_eslint_json("me/web", raw).expect("must parse");

        assert_eq!(findings.len(), 3, "{findings:?}");
        assert_eq!(shape.files_reported, 2);
        assert_eq!(shape.files_unparseable, 0);
        assert!(!shape.read_nothing());

        let eq = findings
            .iter()
            .find(|f| f.rule_id == "eqeqeq")
            .expect("eqeqeq finding");
        assert_eq!(eq.path, "/repo/app.js");
        assert_eq!(eq.line, 2);
        assert_eq!(eq.repo, "me/web");
        // W24 (2026-10-20): was `"high"`, asserting that eslint severity 2 maps straight
        // through. That mapping was the defect, not the contract. An eslint "error" is the
        // scanned PROJECT's own strictness setting - it means they chose to fail their build
        // on that lint - and reading it as a security severity put 31 style rows above every
        // planted exposure on the 2026-10-18 hearth export. `capped_preview_severity` now
        // caps the two pure style linters at medium; Semgrep and Ruff are untouched because
        // their severities genuinely are security judgements.
        assert_eq!(
            eq.severity, "medium",
            "a style linter's 'error' is the project's strictness choice, capped at medium"
        );
        assert!(eq.preview);
        assert_eq!(eq.preview_tool.as_deref(), Some("eslint"));

        let console = findings
            .iter()
            .find(|f| f.rule_id == "no-console")
            .expect("no-console finding");
        assert_eq!(
            console.severity, "medium",
            "eslint severity 1 (warning) maps to medium"
        );

        // The parser is faithful to eslint's output: it does not know which rules the project
        // selected. The selection filter is a separate step (`keep_selected_eslint_findings`),
        // and THAT is what stops the bundled baseline (`no-console`, `eqeqeq`, ...) from
        // reaching the report as findings nobody asked for. Only `no-var` was selected here.
        let kept = keep_selected_eslint_findings(findings, &["no-var".to_string()]);
        let ids: Vec<&str> = kept.iter().map(|f| f.rule_id.as_str()).collect();
        assert_eq!(ids, ["no-var"], "{kept:?}");
    }

    /// The baseline-leak filter: only findings whose eslint rule id is in the runnable selector
    /// set survive, for core, plugin and scoped ids alike; an empty selection keeps nothing.
    #[test]
    fn keep_selected_eslint_findings_keeps_only_the_selected_rule_ids() {
        let mk = |rule: &str| {
            preview_finding("me/web", ScanTool::Eslint, "a.js", 1, rule, "warning", "m")
        };
        let all = vec![
            mk("no-console"),
            mk("eqeqeq"),
            mk("react-hooks/exhaustive-deps"),
            mk("@typescript-eslint/no-explicit-any"),
            mk("react-hooks/rules-of-hooks"),
        ];
        let selectors = vec![
            "react-hooks/exhaustive-deps".to_string(),
            "@typescript-eslint/no-explicit-any".to_string(),
        ];
        let kept = keep_selected_eslint_findings(all.clone(), &selectors);
        let ids: Vec<&str> = kept.iter().map(|f| f.rule_id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "react-hooks/exhaustive-deps",
                "@typescript-eslint/no-explicit-any"
            ]
        );
        assert!(keep_selected_eslint_findings(all, &[]).is_empty());
    }

    /// Belt and braces: `--quiet` drops every warning-level baseline row at the source, and the
    /// baseline's one `error`-level rule (`no-duplicate-case`) is demoted to `warn` so `--quiet`
    /// removes it too. A selected rule is always passed at `error`, so it survives `--quiet`.
    #[test]
    fn eslint_runs_quiet_and_the_bundled_baseline_has_no_error_level_rule() {
        let args = eslint_args("/cfg/c.mjs", &["eqeqeq".to_string()]);
        assert!(args.iter().any(|a| a == "--quiet"), "{args:?}");

        let config = include_str!("../assets/eslint/camerata.config.mjs");
        let code_lines: Vec<&str> = config
            .lines()
            .map(str::trim)
            .filter(|l| !(l.starts_with("//") || l.starts_with('*') || l.starts_with("/*")))
            .collect();
        assert!(
            code_lines.iter().all(|l| !l.contains("\"error\"")),
            "a baseline rule at error level would survive --quiet and reach the report"
        );
        assert!(config.contains("\"no-duplicate-case\": \"warn\""));
    }

    /// req_c11: a `fatal` parse error is NEVER emitted as a finding (it is not a statement
    /// about the client's code, and attributing it to a rule id would be a fabrication), and
    /// a run where EVERY file failed to parse is reported as `read_nothing` — a zero-coverage
    /// run that is otherwise indistinguishable from a clean one (exit 0, valid JSON, no
    /// findings). This is the shape the broken TypeScript-parser path produced.
    #[test]
    fn req_c11_a_run_that_could_not_parse_any_file_is_read_nothing_and_emits_no_findings() {
        let raw = r#"[
          {"filePath":"/repo/a.ts","messages":[{"ruleId":null,"fatal":true,"severity":2,"message":"Parsing error: Unexpected token :","line":1,"column":12}],"errorCount":1,"fatalErrorCount":1},
          {"filePath":"/repo/b.ts","messages":[{"ruleId":null,"fatal":true,"severity":2,"message":"Parsing error: Unexpected token :","line":3,"column":4}],"errorCount":1,"fatalErrorCount":1}
        ]"#;
        let (findings, shape) = parse_eslint_json("me/web", raw).expect("must parse");
        assert!(
            findings.is_empty(),
            "a parse error is not a finding about the client's code: {findings:?}"
        );
        assert_eq!(shape.files_reported, 2);
        assert_eq!(shape.files_unparseable, 2);
        assert!(
            shape.read_nothing(),
            "every file unreadable must be reported as a zero-coverage run"
        );
    }

    /// req_c11: a PARTIALLY unparseable repo is NOT `read_nothing` — the rules that did
    /// evaluate genuinely evaluated, so claiming total failure would be its own false
    /// statement. Guards against the obvious over-correction.
    #[test]
    fn req_c11_a_partially_unparseable_run_is_not_read_nothing() {
        let raw = r#"[
          {"filePath":"/repo/a.ts","messages":[{"ruleId":null,"fatal":true,"severity":2,"message":"Parsing error"}]},
          {"filePath":"/repo/b.js","messages":[{"ruleId":"no-var","severity":2,"message":"Unexpected var.","line":1}]}
        ]"#;
        let (findings, shape) = parse_eslint_json("me/web", raw).expect("must parse");
        assert_eq!(findings.len(), 1);
        assert_eq!(shape.files_reported, 2);
        assert_eq!(shape.files_unparseable, 1);
        assert!(!shape.read_nothing());
    }

    /// req_c11: eslint's "nothing matched / clean" output is the literal `[]`, which must
    /// parse to zero findings and a zero-file shape — explicitly NOT `read_nothing` (there
    /// was nothing to read, which is an ordinary disclosed outcome, not a degradation).
    #[test]
    fn req_c11_an_empty_eslint_result_array_is_clean_not_a_degradation() {
        let (findings, shape) = parse_eslint_json("me/web", "[]").expect("must parse");
        assert!(findings.is_empty());
        assert_eq!(shape.files_reported, 0);
        assert!(!shape.read_nothing());
    }

    /// req_c11: and the actual C11 input — an EMPTY string — must still be an `Err`, so the
    /// caller builds a `CoverageNote` rather than reporting a clean preview. The fix changes
    /// the MESSAGE (see `req_c11_an_empty_stdout_error_names_the_tool_stderr_not_a_parse_
    /// position`), never the fail-closed posture.
    #[test]
    fn req_c11_empty_eslint_stdout_is_an_error_never_a_clean_preview() {
        assert!(
            parse_eslint_json("me/web", "").is_err(),
            "empty stdout must never be ingested as a clean result"
        );
    }

    // ── W3: commodity-class taint layer — shape-variant SQLi corpus ──────────────────────
    //
    // The bundled taint-mode rule family (`assets/semgrep-rules/taint-security.yml`) is
    // validated two ways:
    //
    // 1. SYNTHETIC tool-output fixtures (immediately below) — no semgrep binary required,
    //    always runs, exercises the SAME ingestion path (`parse_sarif`) the live pass uses.
    // 2. A LIVE end-to-end run against the real `semgrep` binary and the bundled YAML,
    //    gated on binary presence (mirrors `report_export.rs`'s `which_typst()` gate for the
    //    PDF tests) — skips with a message rather than failing when semgrep isn't on PATH.

    /// Best-effort `semgrep` presence check for the live-run test's skip gate. Mirrors
    /// `report_export.rs`'s `which_typst()`: a synchronous, side-effect-free PATH probe.
    fn which_semgrep() -> Option<()> {
        std::process::Command::new("semgrep")
            .arg("--version")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|_| ())
    }

    /// Synthetic SARIF fixtures standing in for what a real semgrep run over the bundled
    /// taint-security.yml would emit for the shape-variant SQLi corpus — one entry per
    /// syntax shape, plus three safe twins that must NEVER appear in real semgrep output
    /// (asserted by the live test below, when it runs). Exercises `parse_sarif` +
    /// `normalize_semgrep_rule_id` exactly as the live pass's ingestion path does, with NO
    /// semgrep binary required — this always runs.
    #[test]
    fn synthetic_shape_variant_sqli_fixtures_ingest_correctly() {
        fn sarif_result(rule_id: &str, path: &str, line: usize) -> serde_json::Value {
            serde_json::json!({
                "ruleId": rule_id,
                "level": "error",
                "message": { "text": "Possible SQL injection (taint)." },
                "locations": [{
                    "physicalLocation": {
                        "artifactLocation": { "uri": path },
                        "region": { "startLine": line }
                    }
                }]
            })
        }
        // 7 distinct unsafe shapes (single-quoted, double-quoted, f-string/template literal,
        // %-format, multi-segment concat, intermediate-variable-then-execute, driver raw
        // method) — the exact shapes the old double-quote-only regex could never cover.
        let unsafe_shapes = [
            ("src/a.py", 2, "double-quoted concatenation"),
            ("src/b.py", 2, "single-quoted concatenation"),
            ("src/c.py", 2, "f-string interpolation"),
            ("src/d.py", 2, "%-format interpolation"),
            ("src/e.py", 3, "intermediate variable then execute"),
            ("src/f.py", 4, "multi-segment concatenation"),
            ("src/g.py", 2, "driver .raw() unsafe method"),
        ];
        let results: Vec<serde_json::Value> = unsafe_shapes
            .iter()
            .map(|(path, line, _desc)| {
                sarif_result(
                    "Users.ci.camerata.tooling.semgrep-rules.camerata.security.taint-sql-injection-python",
                    path,
                    *line,
                )
            })
            .collect();
        let sarif = serde_json::json!({
            "version": "2.1.0",
            "runs": [{ "results": results }]
        })
        .to_string();

        let findings = parse_sarif("me/svc", ScanTool::Semgrep, &sarif).expect("must parse");
        assert_eq!(
            findings.len(),
            unsafe_shapes.len(),
            "every unsafe shape must ingest as its own finding"
        );
        for (f, (path, line, desc)) in findings.iter().zip(unsafe_shapes.iter()) {
            assert_eq!(
                f.rule_id, "camerata.security.taint-sql-injection-python",
                "{desc}: id must normalize"
            );
            assert_eq!(&f.path, path, "{desc}");
            assert_eq!(f.line, *line, "{desc}");
            assert!(
                f.preview && f.preview_tool.as_deref() == Some("semgrep"),
                "{desc}: external-tool provenance"
            );
        }
        // Safe twins (parameterized query, tagged template / constant string) are simply
        // ABSENT from a real tool's output — nothing to parse, nothing to assert beyond "the
        // ingestion path doesn't invent findings that aren't in the input", which the exact
        // `findings.len()` assertion above already proves.
    }

    /// LIVE end-to-end run: the REAL `semgrep` binary against the bundled
    /// `taint-security.yml`, over a small shape-variant SQLi corpus written to a temp dir
    /// (synthetic fixtures — never reads from the repo's own test corpus). Skips gracefully
    /// when semgrep is not on PATH, per [`which_semgrep`].
    #[tokio::test]
    async fn live_semgrep_detects_shape_variant_sqli_and_spares_safe_twins() {
        if which_semgrep().is_none() {
            eprintln!(
                "skipping live_semgrep_detects_shape_variant_sqli_and_spares_safe_twins: \
                 semgrep not on PATH"
            );
            return;
        }

        let dir = tempfile::TempDir::new().expect("tempdir");
        let src = r#"
def get_user_dquote(user_id):
    cursor.execute("SELECT * FROM users WHERE id = " + user_id)

def get_user_squote(user_id):
    cursor.execute('SELECT * FROM users WHERE id = ' + user_id)

def get_user_fstring(user_id):
    cursor.execute(f"SELECT * FROM users WHERE id = {user_id}")

def get_user_percent(user_id):
    cursor.execute("SELECT * FROM users WHERE id = %s" % user_id)

def get_user_intermediate(user_id):
    query = f"SELECT * FROM users WHERE id = {user_id}"
    cursor.execute(query)

def get_user_multisegment(user_id):
    q1 = "SELECT * FROM users WHERE id = " + user_id
    q2 = q1 + " AND active = 1"
    cursor.execute(q2)

def get_user_safe_param(user_id):
    cursor.execute("SELECT * FROM users WHERE id = %s", (user_id,))

def get_user_safe_constant():
    cursor.execute("SELECT * FROM users")
"#;
        std::fs::write(dir.path().join("t1.py"), src).expect("write fixture");

        let rules_dir = crate::tool_provisioning::bundled_semgrep_rules_dir();
        let config = rules_dir.join("taint-security.yml");
        assert!(
            config.exists(),
            "bundled taint-security.yml must exist at {}",
            config.display()
        );

        let (stdout, _ok, _stderr) = run_capture_stdout(
            dir.path(),
            "semgrep",
            &[
                "--sarif",
                "--config",
                config.to_str().expect("utf8 path"),
                "--quiet",
                ".",
            ],
            None,
        )
        .await
        .expect("semgrep must run");

        let findings = parse_sarif("me/svc", ScanTool::Semgrep, &stdout).expect("must parse SARIF");
        let lines: std::collections::HashSet<usize> = findings
            .iter()
            .filter(|f| f.rule_id == "camerata.security.taint-sql-injection-python")
            .map(|f| f.line)
            .collect();

        // The 6 unsafe shapes (dquote, squote, fstring, percent, intermediate-var,
        // multi-segment) each produce a finding on their `cursor.execute(...)` line.
        for line in [2, 5, 8, 11, 15, 20] {
            assert!(
                lines.contains(&line),
                "unsafe shape at line {line} must be flagged, got lines: {lines:?}"
            );
        }
        // The 2 safe twins (parameterized query, constant string) must NEVER be flagged.
        for line in [23, 26] {
            assert!(
                !lines.contains(&line),
                "safe twin at line {line} must NOT be flagged, got lines: {lines:?}"
            );
        }
    }

    /// Whether a Camerata-managed eslint workspace, with `eslint-plugin-react-hooks`
    /// already installed inside it, is cached on THIS machine from a prior real
    /// provisioning run. Mirrors `which_semgrep`'s role above: gates the live test on
    /// tool presence WITHOUT triggering a network `npm install` at test time (which would
    /// make this test flaky/slow/offline-hostile — `ensure_eslint`'s probe-first design
    /// means this check and the live test below never provision anything new). A bare
    /// `eslint` binary on PATH is not enough here (unlike `which_semgrep`, which only
    /// needs semgrep itself): the assertion below needs `eslint-plugin-react-hooks`
    /// registered, which only Camerata's OWN managed workspace + bundled config provide.
    ///
    /// READ-ONLY by construction: it asks the full health probe
    /// ([`tool_provisioning::eslint_is_provisioned`]) and returns `None` for a workspace that is
    /// stale or half-installed, so the live test SKIPS rather than driving a re-provision. A test
    /// process is also refused provisioning into the real tooling dir outright (see
    /// `tool_provisioning::refuse_real_tooling_in_test_process`); this probe is what keeps the
    /// skip decision honest instead of letting the refusal surface as a confusing test failure.
    async fn cached_eslint_workspace_with_react_hooks_plugin() -> Option<std::path::PathBuf> {
        let tooling = tool_provisioning::tooling_dir()?;
        let workspace = tool_provisioning::eslint_workspace_dir(&tooling);
        if !tool_provisioning::eslint_is_provisioned(&workspace).await {
            return None;
        }
        let plugin_marker = workspace
            .join("node_modules")
            .join("eslint-plugin-react-hooks")
            .join("package.json");
        if !plugin_marker.exists() {
            return None;
        }
        Some(workspace)
    }

    /// Live end-to-end proof (gated on tool presence, like `live_semgrep_detects_shape_
    /// variant_sqli_and_spares_safe_twins` above): over a REAL `useEffect` that omits a
    /// reactive dependency, the full `run_scan_tools` path — provisioning probe, bundled
    /// config, `--rule` override, SARIF parse — produces a real
    /// `react-hooks/exhaustive-deps` finding. Skips (never fails) when no Camerata-managed
    /// eslint workspace with the react-hooks plugin is cached on this machine.
    #[tokio::test]
    async fn live_eslint_detects_a_real_exhaustive_deps_violation() {
        if cached_eslint_workspace_with_react_hooks_plugin()
            .await
            .is_none()
        {
            eprintln!(
                "skipping live_eslint_detects_a_real_exhaustive_deps_violation: no cached \
                 Camerata eslint workspace with eslint-plugin-react-hooks provisioned on this \
                 machine (this test never provisions over the network)"
            );
            return;
        }

        let dir = tempfile::TempDir::new().expect("tempdir");
        let src = r#"
import { useEffect, useState } from "react";

export function Thing({ id }) {
  const [value, setValue] = useState(0);
  useEffect(() => {
    console.log(id, value);
  }, []); // missing both `id` and `value`
  return value;
}
"#;
        std::fs::write(dir.path().join("Thing.jsx"), src).expect("write fixture");

        let rules = vec![rule_with(
            "JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1",
            camerata_rules::EnforcementKind::Mechanical,
            false,
            &["react-hooks: exhaustive-deps"],
        )];
        let lookup = lookup_over(&rules);
        let selection = vec![selected("JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1")];

        let (findings, notes, attempted) =
            run_scan_tools("me/web", dir.path(), &selection, &lookup, None, None).await;

        assert!(
            attempted.contains("eslint"),
            "eslint must have been attempted this run: notes={notes:?}"
        );
        assert!(
            findings
                .iter()
                .any(|f| f.rule_id == "react-hooks/exhaustive-deps" && f.path.contains("Thing")),
            "a real missing-dependency violation must produce a real eslint finding: \
             findings={findings:?} notes={notes:?}"
        );
    }
    // ── UI-REACT-HOOKS-LINT-1: who owns what, so a violation is reported exactly once ──────

    /// The three React rules, selected together the way a React project's scan selects them.
    const HOOKS_OWNERS: [&str; 2] = [
        "JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1",
        "JAVASCRIPT-REACT-RULES-OF-HOOKS-1",
    ];
    const HOOKS_LINT_RULE: &str = "UI-REACT-HOOKS-LINT-1";

    /// `UI-REACT-HOOKS-LINT-1` is the lint-configuration gap; the two `JAVASCRIPT-REACT-*`
    /// rules own the eslint rule ids. If the new rule also carried a `react-hooks:` linter
    /// source, the eslint run would receive each selector twice and every violation would be
    /// reported under two rule ids. This pins the split against the REAL corpus.
    #[tokio::test]
    async fn ui_react_hooks_lint_adds_no_second_eslint_route_so_a_violation_is_never_reported_twice(
    ) {
        let (corpus, errors) =
            camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
        assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
        let rule = corpus
            .get_by_id(HOOKS_LINT_RULE)
            .expect("UI-REACT-HOOKS-LINT-1 is in the corpus");
        assert_eq!(rule.enforcement, EnforcementKind::Architectural);
        assert!(
            rule.sources.iter().all(|s| s.linter.is_none()),
            "a `linter` annotation would route this rule to eslint a second time: {:?}",
            rule.sources
        );
        assert_eq!(tool_for_rule(rule), None);

        // Exactly the two owners claim a `react-hooks/*` selector.
        let mut claimants: Vec<&str> = corpus
            .iter()
            .filter(|r| {
                r.sources
                    .iter()
                    .filter_map(|s| s.linter.as_deref())
                    .filter_map(selector_for_linter)
                    .any(|sel| sel.starts_with("react-hooks/"))
            })
            .map(|r| r.id.0.as_str())
            .collect();
        claimants.sort_unstable();
        assert_eq!(claimants, HOOKS_OWNERS);

        // Selected together, the eslint run is handed each owner once and the new rule is
        // neither routed nor reported as unrouted (it has its own checker).
        let selection: Vec<SelectedRule> = [HOOKS_OWNERS[0], HOOKS_OWNERS[1], HOOKS_LINT_RULE]
            .into_iter()
            .map(selected)
            .collect();
        let lookup = |id: &str| corpus.get_by_id(id);
        let (by_tool, ungrouped) = group_by_tool(&selection, &lookup, None);
        assert!(
            ungrouped.is_empty(),
            "the new rule must not appear as unrouted: {ungrouped:?}"
        );
        let mut routed: Vec<&str> = by_tool[&ScanTool::Eslint]
            .iter()
            .map(|sr| sr.id.as_str())
            .collect();
        routed.sort_unstable();
        assert_eq!(routed, HOOKS_OWNERS);
        assert_eq!(
            by_tool.len(),
            1,
            "no other tool is driven: {:?}",
            by_tool.keys()
        );
    }

    /// The real shape of eslint's own JSON output for the two hooks rules: each report is a
    /// finding at its line, carrying the lint message (the fix, instantiated for that hook and
    /// that variable), under the raw eslint rule id the two owner rules ground.
    #[test]
    fn eslint_json_hooks_reports_become_findings_at_their_line_with_the_lint_message() {
        let json = r#"[{
          "filePath": "/work/web/src/Bad.jsx",
          "messages": [
            { "ruleId": "react-hooks/rules-of-hooks", "severity": 2, "line": 5,
              "message": "React Hook \"useState\" is called conditionally. React Hooks must be called in the exact same order in every component render." },
            { "ruleId": "react-hooks/exhaustive-deps", "severity": 1, "line": 9,
              "message": "React Hook useEffect has a missing dependency: 'id'. Either include it or remove the dependency array." },
            { "ruleId": null, "severity": 2, "line": 1, "message": "eslint speaking for itself" }
          ]
        }]"#;
        let (findings, shape) = parse_eslint_json("me/web", json).expect("parses");
        assert_eq!(shape.files_reported, 1);
        assert_eq!(findings.len(), 2, "{findings:#?}");

        let hook = &findings[0];
        assert_eq!(hook.rule_id, "react-hooks/rules-of-hooks");
        assert_eq!(hook.line, 5);
        assert!(
            hook.detail.contains("is called conditionally"),
            "{}",
            hook.detail
        );

        let deps = &findings[1];
        assert_eq!(deps.rule_id, "react-hooks/exhaustive-deps");
        assert_eq!(deps.line, 9);
        assert!(
            deps.detail.contains("missing dependency: 'id'"),
            "the instantiated fix names the variable: {}",
            deps.detail
        );

        for f in &findings {
            assert_eq!(
                f.severity, "medium",
                "an eslint error/warning caps at medium"
            );
            assert_eq!(f.kind, Some(camerata_api_types::audit::FindingKind::Defect));
            assert!(f.preview && f.preview_tool.as_deref() == Some("eslint"));
            assert_eq!(
                crate::eslint_floor_category(&f.rule_id).map(|id| HOOKS_OWNERS.contains(&id)),
                Some(true),
                "grounds to one of the two owner rules, never to {HOOKS_LINT_RULE}"
            );
        }
    }

    /// Live, over the real corpus rules and the real provisioned eslint: a file with a hook
    /// called in a condition and an effect missing a dependency produces one finding per
    /// violation at its line, under the raw eslint ids and NEVER under `UI-REACT-HOOKS-LINT-1`;
    /// a second file written correctly (complete dependencies, hooks at the top level) produces
    /// none. The violating file in the same run is the positive control that proves eslint
    /// evaluated the rules rather than reading nothing. Skips (never fails) when no Camerata
    /// eslint workspace with the plugin is cached.
    #[tokio::test]
    async fn live_eslint_reports_a_conditional_hook_and_a_missing_dependency_once_and_spares_their_safe_twins(
    ) {
        if cached_eslint_workspace_with_react_hooks_plugin()
            .await
            .is_none()
        {
            eprintln!(
                "skipping live_eslint_reports_a_conditional_hook_and_a_missing_dependency_once_\
                 and_spares_their_safe_twins: no cached Camerata eslint workspace with \
                 eslint-plugin-react-hooks on this machine (this test never provisions)"
            );
            return;
        }
        let (corpus, errors) =
            camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
        assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

        let bad = "import { useEffect, useState } from \"react\";\n\
                   \n\
                   export function Bad({ id, enabled }) {\n\
                   \x20 if (enabled) {\n\
                   \x20   const [open, setOpen] = useState(false);\n\
                   \x20   console.log(open, setOpen);\n\
                   \x20 }\n\
                   \x20 useEffect(() => {\n\
                   \x20   console.log(id);\n\
                   \x20 }, []);\n\
                   \x20 return null;\n\
                   }\n";
        let good = "import { useEffect, useState } from \"react\";\n\
                    \n\
                    export function Good({ id, enabled }) {\n\
                    \x20 const [open, setOpen] = useState(false);\n\
                    \x20 useEffect(() => {\n\
                    \x20   console.log(id, enabled, open, setOpen);\n\
                    \x20 }, [id, enabled, open]);\n\
                    \x20 return null;\n\
                    }\n";
        let dir = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("Bad.jsx"), bad).expect("write Bad.jsx");
        std::fs::write(dir.path().join("Good.jsx"), good).expect("write Good.jsx");

        let selection: Vec<SelectedRule> = [HOOKS_OWNERS[0], HOOKS_OWNERS[1], HOOKS_LINT_RULE]
            .into_iter()
            .map(selected)
            .collect();
        let lookup = |id: &str| corpus.get_by_id(id);
        let (findings, notes, attempted) =
            run_scan_tools("me/web", dir.path(), &selection, &lookup, None, None).await;
        assert!(attempted.contains("eslint"), "notes={notes:?}");
        assert!(
            notes.iter().all(|n| !n.message.contains(HOOKS_LINT_RULE)),
            "the new rule is not an unrouted rule: {notes:?}"
        );

        let hooks: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.rule_id.starts_with("react-hooks/"))
            .collect();
        let at = |rule: &str, file: &str| -> Vec<usize> {
            hooks
                .iter()
                .filter(|f| f.rule_id == rule && f.path.ends_with(file))
                .map(|f| f.line)
                .collect()
        };
        // The conditional hook is reported at its call, once.
        assert_eq!(
            at("react-hooks/rules-of-hooks", "Bad.jsx"),
            [5],
            "{hooks:#?}"
        );
        // The incomplete array is reported once, on the effect.
        let deps_lines = at("react-hooks/exhaustive-deps", "Bad.jsx");
        assert_eq!(deps_lines.len(), 1, "{hooks:#?}");
        assert!((8..=10).contains(&deps_lines[0]), "{deps_lines:?}");
        let deps = hooks
            .iter()
            .find(|f| f.rule_id == "react-hooks/exhaustive-deps")
            .expect("present");
        assert!(
            deps.detail.contains("'id'"),
            "names the missing variable: {}",
            deps.detail
        );
        // Safe twins: nothing for the correct file, and nothing under the new rule's id.
        assert!(
            hooks.iter().all(|f| !f.path.ends_with("Good.jsx")),
            "complete dependencies and top-level hooks must be silent: {hooks:#?}"
        );
        assert!(
            findings.iter().all(|f| f.rule_id != HOOKS_LINT_RULE),
            "{findings:#?}"
        );
    }

    /// Best-effort probe for a provisioned Camerata eslint workspace — the eslint BINARY and
    /// the bundled flat config, nothing more (unlike
    /// `cached_eslint_workspace_with_react_hooks_plugin`, this test needs no plugin: it uses
    /// CORE eslint rules precisely so it exercises the formatter/plumbing axis in isolation).
    ///
    /// READ-ONLY, like `cached_eslint_workspace_with_react_hooks_plugin`: it uses the full health
    /// probe, so a stale or half-installed workspace makes the live test skip instead of
    /// triggering (or racing) a re-provision of the shared tree.
    async fn cached_eslint_workspace() -> Option<std::path::PathBuf> {
        let tooling = tool_provisioning::tooling_dir()?;
        let workspace = tool_provisioning::eslint_workspace_dir(&tooling);
        if !tool_provisioning::eslint_is_provisioned(&workspace).await {
            return None;
        }
        Some(workspace)
    }

    /// req_c11, THE DECISIVE LIVE PROOF: against the real eslint binary in the real
    /// provisioned workspace, over a repo with two planted CORE-rule violations:
    ///
    /// 1. The OLD argument vector (`--format @microsoft/eslint-formatter-sarif`) produces an
    ///    EMPTY stdout and a non-zero exit — reproducing C11's
    ///    `EOF while parsing a value at line 1 column 0` exactly, from first principles, with
    ///    no mocking anywhere. If this half ever stops reproducing, the assertion says so
    ///    rather than silently passing.
    /// 2. The NEW argument vector ([`eslint_args`], built-in `json` formatter) over the SAME
    ///    workspace, SAME config and SAME repo produces real findings for both planted
    ///    violations.
    ///
    /// Same-process, same-fixture, one variable changed. Skips (never fails) when no Camerata
    /// eslint workspace is cached on this machine — it never provisions over the network.
    #[tokio::test]
    async fn req_c11_live_eslint_json_formatter_finds_what_the_npm_sarif_formatter_could_not() {
        let Some(workspace) = cached_eslint_workspace().await else {
            eprintln!(
                "skipping req_c11_live_eslint_json_formatter_finds_what_the_npm_sarif_\
                 formatter_could_not: no cached Camerata eslint workspace on this machine"
            );
            return;
        };
        let bin = tool_provisioning::eslint_bin(&workspace);
        let bin_str = bin.to_string_lossy().into_owned();
        let config_str = tool_provisioning::eslint_config_path(&workspace)
            .to_string_lossy()
            .into_owned();

        let dir = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(
            dir.path().join("app.js"),
            "export function f(a, b) {\n  if (a == b) { return 1; }\n  var x = 2;\n  return x;\n}\n",
        )
        .expect("write fixture");

        // Two CORE eslint rules, both present in the bundled config, both genuinely violated
        // by the fixture above — so neither half of this test can fail for a plugin reason.
        let selectors = vec!["eqeqeq".to_string(), "no-var".to_string()];

        // ── 1. The OLD invocation: the npm SARIF formatter package. ──────────────────────
        let mut old_args: Vec<String> = vec![
            "--config".to_string(),
            config_str.clone(),
            "--format".to_string(),
            "@microsoft/eslint-formatter-sarif".to_string(),
        ];
        for sel in &selectors {
            old_args.push("--rule".to_string());
            old_args.push(format!("{{\"{sel}\": \"error\"}}"));
        }
        old_args.push(".".to_string());
        let old_refs: Vec<&str> = old_args.iter().map(|s| s.as_str()).collect();
        let (old_stdout, old_ok, old_stderr) =
            run_capture_stdout(dir.path(), &bin_str, &old_refs, None)
                .await
                .expect("eslint must spawn");

        assert!(
            old_stdout.trim().is_empty(),
            "C11 must still reproduce: the npm formatter package cannot resolve from the \
             scanned repo's cwd, so eslint writes NOTHING to stdout. Got {} bytes: {old_stdout}",
            old_stdout.len()
        );
        assert!(!old_ok, "that invocation exits non-zero");
        assert!(
            parse_eslint_json("me/web", &old_stdout).is_err(),
            "the empty stdout is what produced C11's parse error"
        );
        assert!(
            old_stderr.to_lowercase().contains("formatter"),
            "the real cause is on stderr, which Camerata used to discard: {old_stderr}"
        );

        // ── 2. The NEW invocation: eslint's built-in json formatter. ─────────────────────
        let new_args = eslint_args(&config_str, &selectors);
        let new_refs: Vec<&str> = new_args.iter().map(|s| s.as_str()).collect();
        let (new_stdout, _new_ok, new_stderr) =
            run_capture_stdout(dir.path(), &bin_str, &new_refs, None)
                .await
                .expect("eslint must spawn");

        assert!(
            !new_stdout.trim().is_empty(),
            "the built-in formatter needs no module resolution and must always emit a \
             document. stderr: {new_stderr}"
        );
        let (findings, shape) =
            parse_eslint_json("me/web", &new_stdout).expect("built-in json output must parse");
        assert!(
            !shape.read_nothing(),
            "the fixture is plain JS; eslint must have parsed it: {shape:?}"
        );
        for rule in ["eqeqeq", "no-var"] {
            assert!(
                findings
                    .iter()
                    .any(|f| f.rule_id == rule && f.path.contains("app.js")),
                "the planted `{rule}` violation must surface as a real preview finding — this \
                 is the coverage C11 cost us entirely. findings={findings:?}"
            );
        }
    }
}
