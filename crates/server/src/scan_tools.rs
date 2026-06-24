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
use std::path::Path;

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
/// grounding sources' `linter` field (the corpus's tool+rule provenance).
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
pub fn tool_for_rule(rule: &Rule) -> Option<ScanTool> {
    rule.sources
        .iter()
        .filter_map(|s| s.linter.as_deref())
        .find_map(tool_for_linter)
}

/// Map a single `linter` source string to a scan tool. Pure; the core of the
/// linter-source -> tool grouping that the tests pin.
pub fn tool_for_linter(linter: &str) -> Option<ScanTool> {
    let lower = linter.trim().to_ascii_lowercase();
    // The "tool token" is the bit before the first `:` or `::` separator.
    let token = lower
        .split([':'].as_ref())
        .next()
        .unwrap_or(&lower)
        .trim();

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
    // `@angular-eslint`), `eslint-plugin-*`, and the `vue/` rule namespace which
    // is enforced via eslint-plugin-vue.
    if token == "eslint"
        || token.starts_with("eslint-")
        || token.starts_with("@typescript-eslint")
        || token.starts_with("@angular-eslint")
        || token.starts_with("vue/")
    {
        return Some(ScanTool::Eslint);
    }
    None
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
/// - `"semgrep"`               -> `None` (semgrep selects by config pack, not id)
pub fn selector_for_linter(linter: &str) -> Option<String> {
    let trimmed = linter.trim();
    let tool = tool_for_linter(trimmed)?;
    if tool == ScanTool::Semgrep {
        return None;
    }
    // Split on the first `:` (the corpus convention is `Tool: rule-id`).
    let after = trimmed.splitn(2, ':').nth(1).map(str::trim).unwrap_or("");
    if after.is_empty() {
        // No `:` separator — the whole token IS the rule id for some eslint
        // plugins recorded as `@angular-eslint/prefer-inject` with no colon.
        if tool == ScanTool::Eslint {
            return Some(trimmed.to_string());
        }
        return None;
    }
    // eslint scoped plugins record `@typescript-eslint: no-explicit-any`; the
    // real eslint rule id is `@typescript-eslint/no-explicit-any`.
    let lower = trimmed.to_ascii_lowercase();
    if tool == ScanTool::Eslint
        && (lower.starts_with("@typescript-eslint")
            || lower.starts_with("@angular-eslint")
            || lower.starts_with("eslint-plugin"))
    {
        let scope = trimmed.splitn(2, ':').next().unwrap_or("").trim();
        // eslint-plugin-foo: rule  ->  foo/rule ; @scope: rule -> @scope/rule
        let scope = scope.strip_prefix("eslint-plugin-").unwrap_or(scope);
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
            let rule_id = res
                .get("ruleId")
                .and_then(|r| r.as_str())
                .unwrap_or("(unknown)")
                .to_string();
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
/// tool recorded, a snippet that names the tool/rule honestly, and the detail
/// carrying the not-enforced caveat so it is honest wherever it surfaces.
fn preview_finding(
    repo: &str,
    tool: ScanTool,
    path: &str,
    line: usize,
    rule_id: &str,
    severity: &str,
    message: &str,
) -> Finding {
    let detail = if message.is_empty() {
        format!(
            "Preview ({tool}): {rule_id} — found by Camerata; NOT enforced until wired into CI.",
            tool = tool.name()
        )
    } else {
        format!(
            "{message} · Preview ({tool}): NOT enforced until wired into CI.",
            tool = tool.name()
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
        // A preview is advisory, not an enforced/active gate hit.
        status: "suppressed-baseline".to_string(),
        preview: true,
        preview_tool: Some(tool.name().to_string()),
        ..Finding::default()
    }
}

// ─── tool invocation (I/O) ───────────────────────────────────────────────────

/// Run a program in `dir`, capturing stdout SEPARATELY (the parsers need clean
/// JSON, not stdout+stderr interleaved). A spawn failure (binary not on PATH)
/// returns `Err` so the caller can emit a graceful note. A non-zero exit is NOT
/// an error — linters exit non-zero when they find issues, which is the normal
/// "there are findings" signal.
async fn run_capture_stdout(
    dir: &Path,
    program: &str,
    args: &[&str],
) -> std::io::Result<(String, bool)> {
    let out = tokio::process::Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .await?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    Ok((stdout, out.status.success()))
}

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
pub async fn run_scan_tools<'r>(
    repo: &str,
    dir: &Path,
    selected: &[SelectedRule],
    lookup: &(dyn Fn(&str) -> Option<&'r Rule> + Send + Sync),
    present_languages: Option<&HashSet<String>>,
    progress: Option<(&crate::jobs::JobStore, &str)>,
) -> (Vec<Finding>, Vec<CoverageNote>) {
    let (by_tool, ungrouped) = group_by_tool(selected, lookup, present_languages);
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

    for (tool, rules) in by_tool {
        if let Some((jstore, jid)) = progress {
            jstore.det_tool_running(jid, tool.name());
        }
        let produced = match run_one_tool(repo, dir, tool, &rules, lookup).await {
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

    (findings, coverage_notes)
}

/// Run a SINGLE tool over the repo with a Camerata-supplied config that enables
/// exactly `rules`, and parse the result into preview findings. Returns `Err`
/// (which the caller turns into a note) when the tool cannot be spawned or its
/// output cannot be parsed — never a false clean.
async fn run_one_tool<'r>(
    repo: &str,
    dir: &Path,
    tool: ScanTool,
    rules: &[&SelectedRule],
    lookup: &(dyn Fn(&str) -> Option<&'r Rule> + Send + Sync),
) -> anyhow::Result<Vec<Finding>> {
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
            let (stdout, _ok) = run_capture_stdout(dir, "cargo", &arg_refs).await?;
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
            let (stdout, _ok) = run_capture_stdout(dir, &bin_str, &arg_refs).await?;
            parse_sarif(repo, ScanTool::Eslint, &stdout)
        }
    }
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
            domain: "*".to_string(),
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

    // ── SARIF + per-tool JSON parsing ────────────────────────────────────────

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
        let (findings, notes) = run_scan_tools("me/api", dir, &[selected("PY-A")], &lookup, None, None).await;
        assert!(findings.is_empty(), "missing tool must yield no finding rows");
        assert!(!notes.is_empty(), "missing tool must yield a coverage note");
        assert!(notes.iter().any(|n| n.message.contains("Could not preview") || !n.tool.is_empty()));
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
        let (findings, notes) =
            run_scan_tools("me/api", dir, &[selected("PY-A")], &lookup, None, None).await;
        assert!(findings.is_empty(), "a missing tool must yield NO finding row");
        assert!(!notes.is_empty(), "a missing tool must yield a coverage note");
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
}
