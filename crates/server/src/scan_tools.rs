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

use camerata_agent::{HeartbeatFn, MTIME_PROBE_INTERVAL, spawn_mtime_probe};

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
pub fn note_finding(repo: &str, tool: &str, message: impl Into<String>) -> Finding {
    Finding {
        repo: repo.to_string(),
        path: "(scan preview)".to_string(),
        line: 0,
        rule_id: format!("PREVIEW-NOTE-{}", tool.to_ascii_uppercase()),
        severity: "info".to_string(),
        snippet: String::new(),
        detail: message.into(),
        // Info notes are not enforced — keep them out of the active/enforced set.
        status: "suppressed-baseline".to_string(),
        preview: true,
        preview_tool: Some(tool.to_string()),
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
///               Java, Ruby, Rust, C#, PHP, C, C++); passes if present_languages
///               is empty (unknown / can't derive → run conservatively).
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
) -> (BTreeMap<ScanTool, Vec<&'a SelectedRule>>, Vec<&'a SelectedRule>) {
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
        if rule.enforcement != camerata_rules::EnforcementKind::Mechanical || rule.is_layer3_only() {
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
                repo,
                tool,
                &path,
                line,
                &rule_id,
                &norm_severity(level),
                &message,
            ));
        }
    }
    Ok(out)
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
            repo, ScanTool::Ruff, path, line, code, "medium", message,
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
        let Some(msg) = v.get("message") else { continue };
        let code = msg
            .get("code")
            .and_then(|c| c.get("code"))
            .and_then(|c| c.as_str());
        // Only surface lints with a code (skip codeless notes/help).
        let Some(code) = code else { continue };
        let level = msg.get("level").and_then(|l| l.as_str()).unwrap_or("warning");
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
                    .find(|s| s.get("is_primary").and_then(|p| p.as_bool()).unwrap_or(false))
                    .or_else(|| spans.first())
            })
            .map(|sp| {
                let path = sp
                    .get("file_name")
                    .and_then(|f| f.as_str())
                    .unwrap_or("(repo)")
                    .to_string();
                let line = sp
                    .get("line_start")
                    .and_then(|n| n.as_u64())
                    .unwrap_or(0) as usize;
                (path, line)
            })
            .unwrap_or_else(|| ("(repo)".to_string(), 0));
        out.push(preview_finding(
            repo,
            ScanTool::Clippy,
            &path,
            line_no,
            code,
            &norm_severity(level),
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

    // Rule 3: absolute path outside the repo → drop.
    if !abs.starts_with(repo_root) {
        return false;
    }

    // The remainder of the rules apply to the path RELATIVE to repo_root.
    let rel = abs.strip_prefix(repo_root).unwrap_or(&abs);

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
        if let Some(stem) = std::path::Path::new(fname).file_stem().and_then(|s| s.to_str()) {
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
) -> std::io::Result<(String, bool)> {
    use tokio::io::{AsyncBufReadExt, BufReader};

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
            Ok((stdout, out.status.success()))
        }
        Some(cb) => {
            // Streaming path: read stdout line-by-line, firing the heartbeat on
            // every line.  Stderr is captured separately (piped but not streamed)
            // so it doesn't interleave with the JSON stdout the parsers expect.
            let mut child = tokio::process::Command::new(program)
                .args(args)
                .current_dir(dir)
                .kill_on_drop(true)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()?;

            let stdout = child.stdout.take().expect("stdout is piped");
            let mut lines = BufReader::new(stdout).lines();
            let mut accumulated = String::new();

            while let Ok(Some(line)) = lines.next_line().await {
                cb();
                accumulated.push_str(&line);
                accumulated.push('\n');
            }

            // Drain stderr (ignored for content; only the exit status matters for
            // the "is non-zero exit an error" judgement in the caller).
            let _ = child.stderr.take();

            let status = child.wait().await?;
            Ok((accumulated, status.success()))
        }
    }
}

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
        let produced = match run_one_tool(repo, dir, tool, &rules, lookup, on_progress.clone()).await {
            Ok(mut fs) => {
                let n = fs.len();
                findings.append(&mut fs);
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
async fn run_one_tool<'r>(
    repo: &str,
    dir: &Path,
    tool: ScanTool,
    rules: &[&SelectedRule],
    lookup: &(dyn Fn(&str) -> Option<&'r Rule> + Send + Sync),
    on_progress: Option<HeartbeatFn>,
) -> anyhow::Result<Vec<Finding>> {
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
            let (stdout, _ok) = run_capture_stdout(
                dir,
                &bin_str,
                &["--sarif", "--config", &rules_str, "--quiet", "."],
                on_progress.as_ref(),
            )
            .await?;
            parse_sarif(repo, ScanTool::Semgrep, &stdout)
        }
        ScanTool::Ruff => {
            if selectors.is_empty() {
                anyhow::bail!("no ruff rule codes derived from the selection");
            }
            let select = selectors.join(",");
            let (stdout, _ok) = run_capture_stdout(
                dir,
                "ruff",
                &[
                    "check",
                    "--output-format",
                    "json",
                    "--select",
                    &select,
                    ".",
                ],
                on_progress.as_ref(),
            )
            .await?;
            parse_ruff_json(repo, &stdout)
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
            let (stdout, _ok) =
                run_capture_stdout(dir, "cargo", &arg_refs, on_progress.as_ref()).await?;
            parse_clippy_json(repo, &stdout)
        }
        ScanTool::Eslint => {
            if selectors.is_empty() {
                anyhow::bail!("no eslint rule ids derived from the selection");
            }
            // Camerata auto-provisions eslint + the SARIF formatter into a
            // stable node_modules workspace so the user never needs to install
            // it manually.  We use the bundled flat config as the base and
            // override individual rules to "error" via `--rule`.  `--no-eslintrc`
            // is replaced by `--no-ignore` + an explicit `--config` pointing at
            // the bundled flat config (eslint v9 flat-config style).
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
            let mut args: Vec<String> = vec![
                "--config".into(),
                config_str,
                "--format".into(),
                "@microsoft/eslint-formatter-sarif".into(),
            ];
            for sel in &selectors {
                args.push("--rule".into());
                args.push(format!("{{\"{sel}\": \"error\"}}"));
            }
            args.push(".".into());
            let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let (stdout, _ok) =
                run_capture_stdout(dir, &bin_str, &arg_refs, on_progress.as_ref()).await?;
            parse_sarif(repo, ScanTool::Eslint, &stdout)
        }
    }
    // Apply the repo-scope filter uniformly across ALL tools (clippy, ruff,
    // eslint, semgrep).  This drops findings whose path resolves outside the
    // scanned repo — e.g. Rust stdlib paths like `/rustc/<hash>/…`, Cargo build
    // output under `target/`, generated files, and bundled dist directories.
    // The filter is pure and applied here once rather than per-parser so new
    // tool arms inherit it automatically.
    .map(|mut findings| {
        findings.retain(|f| is_in_repo_scope(dir, &f.path));
        findings
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
    fn lookup_over<'r>(
        rules: &'r [Rule],
    ) -> impl Fn(&str) -> Option<&'r Rule> + Send + Sync {
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
            verification: Verification::Grounded,
            sources: linters
                .iter()
                .map(|l| RuleSource {
                    url: "https://example".to_string(),
                    title: "src".to_string(),
                    linter: Some(l.to_string()),
                })
                .collect(),
            verified: None,
            opt_in_only: false,
            layer3_only: layer3,
            stack_exceptions: Vec::new(),
            extra_domains: Vec::new(),
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
        assert_eq!(tool_for_linter("clippy: unwrap_used"), Some(ScanTool::Clippy));
        assert_eq!(tool_for_linter("clippy::unwrap_used"), Some(ScanTool::Clippy));
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
        assert_eq!(selector_for_linter("eslint: eqeqeq").as_deref(), Some("eqeqeq"));
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
            rule_with("RUST-A", EnforcementKind::Mechanical, false, &["clippy: unwrap_used"]),
            rule_with("PY-A", EnforcementKind::Mechanical, false, &["Ruff: S608"]),
            // layer3_only (CodeQL-style) must be EXCLUDED from the preview pass.
            rule_with("CODEQL-1", EnforcementKind::Mechanical, true, &["semgrep"]),
            // A prose rule is not CI-enforced -> excluded.
            rule_with("PROSE-1", EnforcementKind::Prose, false, &["clippy: foo"]),
            // No corpus tool -> ungrouped (graceful note).
            rule_with("GO-A", EnforcementKind::Mechanical, false, &["golangci-lint: errcheck"]),
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
    async fn run_scan_tools_reports_semgrep_as_attempted_even_when_it_fails() {
        std::env::set_var(DISABLE_SEMGREP_ENV_VAR, "1");
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
        std::env::remove_var(DISABLE_SEMGREP_ENV_VAR);

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
        assert_eq!(
            hook.severity, "high",
            "SARIF 'error' level normalizes to high"
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
        let f = preview_finding(
            "me/api",
            ScanTool::Ruff,
            "a.py",
            1,
            "S608",
            "medium",
            "msg",
        );
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
        assert!(findings.is_empty(), "missing tool must yield no finding rows");
        assert!(!notes.is_empty(), "missing tool must yield a coverage note");
        assert!(notes.iter().any(|n| n.message.contains("Could not preview") || !n.tool.is_empty()));
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
            rule_with("MECH-1", EnforcementKind::Mechanical, false, &["clippy: unwrap_used"]),
            rule_with("ARCH-1", EnforcementKind::Architectural, false, &["clippy: some_ast_check"]),
        ];
        let lookup = lookup_over(&rules);
        let sel = vec![selected("MECH-1"), selected("ARCH-1")];
        let (by_tool, ungrouped) = group_by_tool(&sel, &lookup, None);
        // MECH-1 routes to clippy
        assert!(
            by_tool
                .get(&ScanTool::Clippy)
                .map(|v| v.iter().any(|s| s.id == "MECH-1"))
                .unwrap_or(false)
        );
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
        assert!(findings.is_empty(), "a missing tool must yield NO finding row");
        assert!(!notes.is_empty(), "a missing tool must yield a coverage note");
        assert!(attempted.contains("ruff"), "{attempted:?}");
        assert!(
            notes
                .iter()
                .any(|n| n.message.contains("Could not preview") || !n.tool.is_empty())
        );
    }

    #[test]
    fn note_finding_is_preview_and_not_active() {
        let n = note_finding("me/api", "ruff", "could not run");
        assert!(n.preview);
        assert_eq!(n.preview_tool.as_deref(), Some("ruff"));
        assert_ne!(n.status, "active", "a note must not be an enforced/active hit");
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
            rule_with("R-1", EnforcementKind::Mechanical, false, &["clippy: unwrap_used"]),
            rule_with("R-2", EnforcementKind::Mechanical, false, &["clippy: expect_used"]),
            rule_with("R-3", EnforcementKind::Mechanical, false, &["Ruff: S608"]),
        ];
        let lookup = lookup_over(&rules);
        let sel = vec![selected("R-1"), selected("R-2"), selected("R-3")];
        let ids = preview_tool_ids_for_rules(&sel, &lookup, None);
        // Two distinct tools: clippy and ruff (order: BTreeMap order = clippy < ruff).
        assert_eq!(ids.len(), 2, "two distinct tools for three rules: {:?}", ids);
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
        assert!(ids.is_empty(), "architectural rules yield no preview tool ids");
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
        assert!(langs.contains("TypeScript"), "should detect TypeScript from .ts");
        assert!(langs.contains("JavaScript"), "should detect JavaScript from .jsx");
        assert!(!langs.contains("TOML"), "TOML has no language label");
    }

    /// `tool_languages_present` — None passthrough (backward-compat).
    #[test]
    fn tool_languages_present_none_always_passes() {
        for tool in [ScanTool::Clippy, ScanTool::Ruff, ScanTool::Eslint, ScanTool::Semgrep] {
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
        assert!(tool_languages_present(ScanTool::Clippy, Some(&langs)), "clippy needs Rust → present");
        assert!(tool_languages_present(ScanTool::Semgrep, Some(&langs)), "semgrep supports Rust → present");
        assert!(!tool_languages_present(ScanTool::Ruff, Some(&langs)), "ruff needs Python → absent");
        assert!(!tool_languages_present(ScanTool::Eslint, Some(&langs)), "eslint needs JS/TS → absent");
    }

    /// `tool_languages_present` — empty lang set → all tools pass (conservative).
    #[test]
    fn tool_languages_present_empty_set_is_permissive() {
        let langs: HashSet<String> = HashSet::new();
        for tool in [ScanTool::Clippy, ScanTool::Ruff, ScanTool::Eslint, ScanTool::Semgrep] {
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
            rule_with("RUST-1", EnforcementKind::Mechanical, false, &["clippy: unwrap_used"]),
            rule_with("JS-1",   EnforcementKind::Mechanical, false, &["eslint: eqeqeq"]),
            rule_with("PY-1",   EnforcementKind::Mechanical, false, &["Ruff: S608"]),
            rule_with("SG-1",   EnforcementKind::Mechanical, false, &["semgrep"]),
        ];
        let lookup = lookup_over(&rules);
        let sel = vec![selected("RUST-1"), selected("JS-1"), selected("PY-1"), selected("SG-1")];

        // Rust-only language set.
        let rust_only: HashSet<String> = ["Rust".to_string()].into();

        let (by_tool, _ungrouped) = group_by_tool(&sel, &lookup, Some(&rust_only));
        assert!(by_tool.contains_key(&ScanTool::Clippy), "clippy must run (Rust present)");
        assert!(by_tool.contains_key(&ScanTool::Semgrep), "semgrep must run (Rust is semgrep-supported)");
        assert!(!by_tool.contains_key(&ScanTool::Eslint), "eslint must be OMITTED (no JS/TS)");
        assert!(!by_tool.contains_key(&ScanTool::Ruff), "ruff must be OMITTED (no Python)");

        // The pre-declared IDs must match the stack-gated set.
        let ids = preview_tool_ids_for_rules(&sel, &lookup, Some(&rust_only));
        assert!(ids.contains(&"clippy".to_string()), "clippy in pre-declared ids");
        assert!(ids.contains(&"semgrep".to_string()), "semgrep in pre-declared ids");
        assert!(!ids.contains(&"eslint".to_string()), "eslint NOT in pre-declared ids for Rust-only repo");
        assert!(!ids.contains(&"ruff".to_string()), "ruff NOT in pre-declared ids for Rust-only repo");
    }

    /// FIX 1: even if the selected JS rule was the ONLY selection, the stack gate
    /// must omit eslint entirely from a Rust-only repo (no false "✓ 0").
    #[test]
    fn stack_gating_js_rule_on_rust_repo_yields_no_eslint() {
        let rules = vec![
            rule_with("JS-ONLY", EnforcementKind::Mechanical, false, &["eslint: no-eval"]),
        ];
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
            !is_in_repo_scope(&root, "/rustc/abcdef1234567890/library/core/src/macros/mod.rs"),
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
            !is_in_repo_scope(&root, "/home/user/myproject/target/debug/build/pkg/out/v2_generated.rs"),
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
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};
        use camerata_agent::HeartbeatFn;

        let count = Arc::new(AtomicU64::new(0));
        let count_cb = count.clone();
        let cb: HeartbeatFn = Arc::new(move || {
            count_cb.fetch_add(1, Ordering::Relaxed);
        });

        // `echo` outputs exactly 3 lines; printf packs them separated by newlines.
        // We use `sh -c 'printf ...'` for portability across test environments.
        let dir = std::env::temp_dir();
        let (stdout, ok) = run_capture_stdout(
            &dir,
            "sh",
            &["-c", "printf 'line1\\nline2\\nline3\\n'"],
            Some(&cb),
        )
        .await
        .expect("sh should be on PATH");

        assert!(ok, "sh exit should be 0");
        assert_eq!(
            stdout.lines().count(),
            3,
            "stdout should contain 3 lines"
        );
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
        let (stdout, ok) = run_capture_stdout(
            &dir,
            "sh",
            &["-c", "printf 'hello\\nworld\\n'"],
            None,
        )
        .await
        .expect("sh should be on PATH");

        assert!(ok);
        assert!(stdout.contains("hello"));
        assert!(stdout.contains("world"));
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

        let (stdout, _ok) = run_capture_stdout(
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
    fn cached_eslint_workspace_with_react_hooks_plugin() -> Option<std::path::PathBuf> {
        let tooling = tool_provisioning::tooling_dir()?;
        let workspace = tool_provisioning::eslint_workspace_dir(&tooling);
        let bin = tool_provisioning::eslint_bin(&workspace);
        if !bin.exists() {
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
        if cached_eslint_workspace_with_react_hooks_plugin().is_none() {
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
}
