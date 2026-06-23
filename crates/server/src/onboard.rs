//! Brownfield onboarding: scan a repo, audit it against the content rules, and
//! propose a starter ruleset (ADR brownfield_onboarding_flow).
//!
//! The audit reuses the GATE'S OWN rule arms (`camerata_gateway::lookup_arm`) over
//! the repo's existing files, so "what the gate would deny on a new write" and
//! "what's already wrong in your repo" are the SAME check — no second
//! implementation to drift. This is the real-now half the ADR calls out: the
//! content rules (hardcoded secrets, raw-SQL-concat, secrets-in-URL) are pure
//! functions over file content, so they audit an existing repo today. The
//! AST-level architecture rules are the future half and are not scanned here.
//!
//! Everything in this module is pure (files in -> report out); fetching the files
//! from GitHub lives in `repo_reader` and needs the token.

use serde::{Deserialize, Serialize};

/// The content rules the brownfield audit runs (the ones that are pure functions
/// over file content). Path-based rules (GOV-1 forbidden paths, SEC-NO-PATH-ESCAPE-1)
/// govern WRITE TARGETS, not existing content, so they are not part of the audit.
pub const AUDIT_RULES: &[&str] = &[
    "SEC-NO-HARDCODED-SECRETS-1",
    "SEC-NO-RAW-SQL-CONCAT-1",
    "ARCH-NO-SECRETS-IN-URL-1",
];

/// One violation already present in the repo.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Finding {
    /// Which repo this finding is in (`owner/repo`). Lets a multi-repo scan group
    /// and filter findings by repo.
    pub repo: String,
    /// File path within the repo.
    pub path: String,
    /// 1-based line number.
    pub line: usize,
    /// The rule id that fired (a gate rule).
    pub rule_id: String,
    /// `high` | `medium` — for grouping/sorting in the findings table.
    pub severity: String,
    /// The offending line, trimmed and length-capped.
    pub snippet: String,
    /// The gate's own explanation of the violation.
    pub detail: String,
    /// Suppression status: `active` (NEW/changed — the gate enforces), `suppressed-inline`
    /// (waived by a `camerata:allow` comment), or `suppressed-baseline` (accepted
    /// pre-existing debt / policy). Report shows all; enforcement is on `active` only.
    #[serde(default = "default_status")]
    pub status: String,
    /// Other rule ids this SAME code location also violates, demoted here when several
    /// findings at one `(path, line)` were merged into this single row (the primary in
    /// `rule_id`). Empty for an un-merged finding. Lets one row honestly read "violates
    /// layering + DI + entities-chain" instead of emitting five near-duplicate rows.
    #[serde(default)]
    pub also_matches: Vec<String>,
    /// PREVIEW flag (CI-security Part B). `true` when this finding came from the
    /// SCAN-TIME deterministic preview pass ([`crate::scan_tools::run_scan_tools`]):
    /// Camerata ran the rule's underlying tool itself with a supplied config, even
    /// though the rule is not yet wired into the repo's gate. A preview finding is
    /// ADVISORY-but-deterministic: stable rule-id (treated like the floor, NOT the
    /// AI bucket), but NOT enforcement — the CI story must still wire it for the gate
    /// to block on it. The UI labels these "preview — not enforced until wired".
    /// Defaults to `false` (back-compatible: an absent field = a normal finding).
    #[serde(default)]
    pub preview: bool,
    /// For a preview finding, the tool that produced it (`clippy` | `ruff` | `eslint`
    /// | `semgrep`), or a graceful note's source. `None` for non-preview findings.
    /// Surfaced in the UI badge tooltip and the CSV. Carried so a preview is honest
    /// about which tool/version generated it (the gate may pin a different version).
    #[serde(default)]
    pub preview_tool: Option<String>,
    /// True when this finding is in a test/fixture scope (inline `#[cfg(test)]` block
    /// or a test-path file). A test-scoped finding is down-ranked to `low` and
    /// `needs_review = true` is set. False for production findings.
    #[serde(default)]
    pub in_test: bool,
    /// True when this finding's applicability warrants manual verification — either
    /// because it is in test/fixture code (`in_test = true`) or the calibration pass
    /// flagged it with `[needs review]`. False for clear-cut production findings.
    #[serde(default)]
    pub needs_review: bool,
}

/// Findings default to `active` (enforced) until classified against suppressions.
fn default_status() -> String {
    "active".to_string()
}

impl Default for Finding {
    /// A blank finding with sensible defaults — `status = "active"`, `preview = false`.
    /// Lets call sites (and the scan-tools preview pass) build a finding with
    /// `..Finding::default()` instead of spelling out every field, so adding a new
    /// field doesn't break every literal.
    fn default() -> Self {
        Self {
            repo: String::new(),
            path: String::new(),
            line: 0,
            rule_id: String::new(),
            severity: String::new(),
            snippet: String::new(),
            detail: String::new(),
            status: default_status(),
            also_matches: Vec::new(),
            preview: false,
            preview_tool: None,
            in_test: false,
            needs_review: false,
        }
    }
}

/// One alternative the architect can codify for a proposed rule.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RuleOptionView {
    /// Stable option id (what gets codified as the choice).
    pub id: String,
    /// Human label.
    pub label: String,
    /// The concrete directive this alternative codifies.
    pub directive: String,
    /// Why this alternative — the rationale shown in the rule-detail view.
    #[serde(default)]
    pub why: String,
}

/// One authoritative source backing a rule's grounding, mirrored from
/// [`camerata_rules::RuleSource`] for the wire/UI. Lets the UI link out to the
/// standard or linter rule a proposed rule is grounded in.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuleSourceView {
    /// Canonical URL of the source (style-guide section, standard, linter docs).
    pub url: String,
    /// Human-readable title of the source.
    pub title: String,
    /// Enforcing tool + rule id when this is a real linter rule; `None` for a
    /// style-guide / documentation-only source.
    #[serde(default)]
    pub linter: Option<String>,
}

/// One rule proposed for the starter ruleset, classified by SCOPE and PLACEMENT so
/// brownfielding decides, up front, where each rule and its mechanical gate live.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProposedRule {
    /// The rule id.
    pub id: String,
    /// Human description (from the gate registry).
    pub title: String,
    /// `mechanical` (deterministic check exists) | `review` (human-judged).
    pub kind: String,
    /// The corpus enforcement level: `prose` | `structured` | `mechanical`. Drives
    /// where arm emits the rule (prose -> AGENTS.md, the rest -> CONVENTIONS.md),
    /// matching camerata-ai's emit partitioning.
    #[serde(default)]
    pub enforcement: String,
    /// The alternatives the architect chooses among. Empty for mechanical rules
    /// with no variants (the content/security rules).
    #[serde(default)]
    pub options: Vec<RuleOptionView>,
    /// The default option id, or `None` when the architect MUST choose one.
    #[serde(default)]
    pub default_option: Option<String>,
    /// Provenance / verification status: `draft` | `grounded` | `verified`
    /// (the grounding ladder from camerata-rules). `draft` = AI-designed, not
    /// yet grounded (not shippable); `grounded` = mapped to a cited source /
    /// real linter rule; `verified` = a human confirmed it. Defaults to `draft`
    /// so un-grounded rules are visibly so in the UI. See
    /// `docs/decisions/2026-06-20_rule_provenance_schema.md`.
    pub verification: String,
    /// Authoritative sources backing the rule's grounding (empty for `draft`).
    #[serde(default)]
    pub sources: Vec<RuleSourceView>,
    /// The decision this rule frames (`[decision].question`) — what the architect is choosing
    /// between. None for rules with no decision block (content/security rules).
    #[serde(default)]
    pub decision_question: Option<String>,
    /// The rationale for the adopted default (`[decision].why`), when present.
    #[serde(default)]
    pub decision_why: Option<String>,
    /// Scope: `repo-local` (applies within each repo), `cross-repo` (spans the
    /// repo set, e.g. API contracts), or `process` (VCS-workflow, per account).
    pub scope: String,
    /// The corpus domain this rule belongs to (`sql`, `api-layer`, `ui`, `security`,
    /// `architecture`, `*` universal, …). Drives group-by-domain in the rules table.
    #[serde(default)]
    pub domain: String,
    /// Which gate enforces it: `content` (Layer 1/2), `integration` (cross-agent
    /// tier), or `vcs-action` (commit/PR metadata).
    pub enforcement_point: String,
    /// The repos this rule binds to (repo-local) or spans (cross-repo); the full
    /// set for process rules.
    pub repos: Vec<String>,
    /// Where the mechanical gate is installed — the placement decision.
    pub placement: String,
    /// How many existing violations this rule found in the scan.
    pub finding_count: usize,
    /// Whether it is recommended for the starter set.
    pub recommended: bool,
    /// Whether this rule should be PRE-CHECKED (auto-selected) in the onboarding
    /// scan proposal. True iff the rule is `grounded` or `verified` — i.e. it is
    /// backed by a cited authoritative source that was verified at some level.
    /// `draft` and `needs_recheck` rules are listed but NOT pre-checked: the
    /// architect must explicitly opt in. Absent on rules built inline (the
    /// deterministic-floor rules), where it defaults to `false`.
    #[serde(default)]
    pub is_auto_recommended: bool,
}

/// The detected tech stack for one repo (languages from extensions, frameworks
/// from manifests). Drives the stack-specific rule proposals (Approach B).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RepoStack {
    /// `owner/repo`.
    pub repo: String,
    /// Languages detected from file extensions (e.g. `TypeScript`, `Python`).
    pub languages: Vec<String>,
    /// Frameworks detected from manifest contents (e.g. `React`, `ASP.NET`).
    pub frameworks: Vec<String>,
}

/// A scan-coverage note: a tool that was skipped or could not run during the
/// preview pass. This is informational (not a violation). The UI renders these
/// in a separate "Scan coverage" section, distinct from the violations table.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CoverageNote {
    /// The tool or source that generated this note (e.g. `ruff`, `eslint`, `unrouted`).
    pub tool: String,
    /// Human-readable explanation of why this tool/rule was skipped or failed.
    pub message: String,
}

/// The full scan result across one or more repos. Brownfield onboarding treats a
/// SET of inter-related repos (e.g. a .NET API + a Python worker + a React app) as
/// one unit: findings and the proposed ruleset aggregate across all of them, each
/// finding tagged with its repo.
#[derive(Debug, Clone, Serialize)]
pub struct ScanReport {
    /// The repos scanned (`owner/repo`).
    pub repos: Vec<String>,
    /// The detected stack per repo (languages + frameworks).
    #[serde(default)]
    pub stacks: Vec<RepoStack>,
    /// Number of files scanned across all repos.
    pub files_scanned: usize,
    /// Scannable files (code extension) PRUNED as build/dep/cache/generated noise before the
    /// scan. Surfaced so the filter's effect is visible ("N scanned, M excluded as noise").
    #[serde(default)]
    pub files_excluded: usize,
    /// Total characters of scannable code across all repos (after noise pruning). Drives the
    /// pre-audit cost estimate: the digest the model sees is built from this, so it's the
    /// honest token base before chunk/batch multipliers.
    #[serde(default)]
    pub code_chars: usize,
    /// Rule ids EXCLUDED from this code-only audit because they're MECHANICAL — enforced in
    /// CI from build/runtime/DB context (query-plan inspection, migration audit, AST lint),
    /// not judgeable from a static code digest. They're wired into `.camerata/ci-checks.json`
    /// instead. Surfaced (like `files_excluded`) so the re-tiering is visible, not silent.
    #[serde(default)]
    pub excluded_mechanical_rules: Vec<String>,
    /// Every violation found, across all repos (each tagged with its repo).
    pub findings: Vec<Finding>,
    /// The proposed starter ruleset (aggregated over all repos).
    pub proposed_rules: Vec<ProposedRule>,
    /// True when no scan was performed because GitHub is not connected.
    pub gated: bool,
    /// A human message (e.g. the connect-GitHub gate, a per-repo error, or a cap).
    pub message: Option<String>,
    /// REAL token usage + cost for the Phase-2 audit (every pass + calibration), when the
    /// backend reported it. Drives the actual-vs-estimated readout. None on a Phase-1 scan.
    #[serde(default)]
    pub actual_usage: Option<crate::ai_audit::ActualUsage>,
    /// The OPT-IN deep compliance & security tier output (#55): SOC-2 gap analysis + deep
    /// security audit + threat model. `None` unless the audit request set `deep` — the
    /// standard scan never populates this. Everything inside is ADVISORY + model-inferred
    /// (#62); the tier carries its own honesty disclaimer.
    #[serde(default)]
    pub deep: Option<crate::ai_audit::DeepReport>,
    /// Coverage notes from the scan-time preview pass: tools that were skipped or
    /// unavailable. These are scan-COVERAGE information, not violations — they must
    /// not appear in the findings/violations table. Use [`CoverageNote`] entries.
    #[serde(default)]
    pub coverage_notes: Vec<CoverageNote>,
}

impl ScanReport {
    /// The connect-GitHub gate result: no scan performed.
    pub fn gated(repos: &[String]) -> Self {
        Self {
            repos: repos.to_vec(),
            stacks: Vec::new(),
            files_scanned: 0,
            files_excluded: 0,
            excluded_mechanical_rules: Vec::new(),
            code_chars: 0,
            findings: Vec::new(),
            proposed_rules: Vec::new(),
            gated: true,
            actual_usage: None,
            deep: None,
            message: Some(
                "Connect GitHub (set CAMERATA_GITHUB_TOKEN) so Camerata can read the repo(s)."
                    .to_string(),
            ),
            coverage_notes: Vec::new(),
        }
    }
}

/// Severity for a rule id (for grouping/sorting in the table).
fn severity_for(_rule_id: &str) -> &'static str {
    // Deterministic floor findings are ACTUAL exploitable bugs (a hardcoded credential, a
    // secret in a URL, SQL built by string concatenation) — not "doesn't follow a preferred
    // pattern." They rank CRITICAL so they float above the architectural conformance
    // findings (high/medium/low) and can never be buried under "no mappers crate." Every
    // rule that reaches the gate's deterministic arm is, by construction, a real defect.
    "critical"
}

/// Returns `true` when `path` is in a test, fixture, or example context where a
/// flagged secret/SQL pattern is almost certainly a non-exploitable test value.
///
/// Matching rules (all comparisons are case-insensitive):
/// - Any **path segment** (directory component) equals one of:
///   `tests`, `test`, `testdata`, `fixtures`, `__tests__`, `examples`, `benches`
/// - The **filename** (last segment) matches one of:
///   `*_test.<ext>`, `*.test.<ext>`, `*.spec.<ext>`, `test_*.py`, `conftest.py`
///
/// Production paths are unchanged — only test/fixture paths are down-ranked.
pub fn is_test_or_fixture_path(path: &str) -> bool {
    use std::path::Path;

    // Normalise to forward-slash components (handles both Unix and Windows paths).
    let p = Path::new(path);
    let segments: Vec<String> = p
        .components()
        .filter_map(|c| {
            c.as_os_str().to_str().map(|s| s.to_ascii_lowercase())
        })
        .collect();

    // Check every directory segment (all but the last, which is the filename).
    let dir_segments = if segments.len() > 1 {
        &segments[..segments.len() - 1]
    } else {
        &[][..]
    };
    for seg in dir_segments {
        match seg.as_str() {
            "tests" | "test" | "testdata" | "fixtures" | "__tests__" | "examples" | "benches" => {
                return true;
            }
            _ => {}
        }
    }

    // Check the filename against test-file naming conventions.
    if let Some(filename) = segments.last() {
        // conftest.py
        if filename == "conftest.py" {
            return true;
        }
        // test_*.py  (Python unittest convention)
        if filename.starts_with("test_") && filename.ends_with(".py") {
            return true;
        }
        // *_test.<ext>  (Go / Rust convention: foo_test.go, auth_test.rs)
        // *.test.<ext>  (JS/TS convention: auth.test.ts)
        // *.spec.<ext>  (JS/TS convention: auth.spec.ts)
        //
        // Strategy: strip the final extension to get the stem, then check whether
        // the stem ends with "_test", ".test", or ".spec".
        if let Some(dot_pos) = filename.rfind('.') {
            let stem = &filename[..dot_pos];
            if stem.ends_with("_test") || stem.ends_with(".test") || stem.ends_with(".spec") {
                return true;
            }
        }
    }

    false
}

/// Compute the 1-based inclusive line ranges that are test code in a Rust file.
/// Returns an empty vec for non-`.rs` files.
///
/// Detects `#[cfg(test)]`, `#[test]`, and `#[tokio::test]` attribute lines and
/// tracks the brace-delimited block that follows, skipping braces inside `//` line
/// comments, `/* */` block comments, `"..."` string literals, `r#"..."#` raw strings,
/// and `'{'` char literals. The span from the attribute line to the closing `}` is a
/// test scope.
///
/// # Limitation
/// Simple brace counter that skips strings/comments. Does not handle all edge cases
/// (e.g. nested raw strings with mismatched hashes), but correct for the overwhelming
/// majority of Rust test code.
pub fn test_scope_line_ranges(path: &str, content: &str) -> Vec<(usize, usize)> {
    // Only applicable to Rust files.
    if !path.ends_with(".rs") {
        return Vec::new();
    }

    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Normal,
        LineComment,
        BlockComment,
        StringLit,
        RawString(usize), // number of leading hashes
        CharLit,
    }

    let mut ranges = Vec::new();
    let mut state = State::Normal;
    let mut brace_depth: i32 = 0;
    // When Some((attr_line, scope_start_depth)), we are inside a test scope that
    // started at attr_line and whose opening brace raised depth to scope_start_depth.
    let mut scope: Option<(usize, i32)> = None;
    // The line of the most-recently-seen test attribute (before the opening brace).
    let mut pending_attr_line: Option<usize> = None;
    let mut current_line: usize = 1;
    let chars: Vec<char> = content.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];

        // Track line numbers.
        if ch == '\n' {
            current_line += 1;
            if state == State::LineComment {
                state = State::Normal;
            }
            i += 1;
            continue;
        }

        match state {
            State::LineComment => {
                // Already handled newline above; anything else is inside the comment.
                i += 1;
                continue;
            }
            State::BlockComment => {
                if ch == '*' && i + 1 < chars.len() && chars[i + 1] == '/' {
                    state = State::Normal;
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            State::StringLit => {
                if ch == '\\' {
                    // Skip escaped character.
                    i += 2;
                } else if ch == '"' {
                    state = State::Normal;
                    i += 1;
                } else {
                    i += 1;
                }
                continue;
            }
            State::RawString(hashes) => {
                // Look for `"` followed by exactly `hashes` `#` chars.
                if ch == '"' {
                    let end_hashes = chars[i + 1..].iter().take_while(|&&c| c == '#').count();
                    if end_hashes >= hashes {
                        state = State::Normal;
                        i += 1 + hashes;
                    } else {
                        i += 1;
                    }
                } else {
                    i += 1;
                }
                continue;
            }
            State::CharLit => {
                if ch == '\\' {
                    i += 2;
                } else if ch == '\'' {
                    state = State::Normal;
                    i += 1;
                } else {
                    i += 1;
                }
                continue;
            }
            State::Normal => {}
        }

        // --- Normal state ---
        // Detect transitions OUT of Normal.
        if ch == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            state = State::LineComment;
            i += 2;
            continue;
        }
        if ch == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            state = State::BlockComment;
            i += 2;
            continue;
        }
        if ch == '"' {
            state = State::StringLit;
            i += 1;
            continue;
        }
        // Raw string: r#"..."#  (variable hashes)
        if ch == 'r' {
            let hash_count = chars[i + 1..].iter().take_while(|&&c| c == '#').count();
            if i + 1 + hash_count < chars.len() && chars[i + 1 + hash_count] == '"' {
                state = State::RawString(hash_count);
                i += 2 + hash_count; // skip r + hashes + opening "
                continue;
            }
        }
        if ch == '\'' {
            // Simple char literal `'x'` or `'\n'` — single-char (or escape).
            // If the next char is `\`, skip escape + closing quote; else skip char + closing quote.
            if i + 2 < chars.len() && chars[i + 1] == '\\' && i + 3 < chars.len() && chars[i + 3] == '\'' {
                // '\x'
                i += 4;
                continue;
            }
            if i + 2 < chars.len() && chars[i + 2] == '\'' {
                // 'x'
                i += 3;
                continue;
            }
            // Not a char literal (e.g. lifetime 'a or complex case) — pass through.
        }

        // Detect test attribute lines (only when not inside a scope).
        // We look for the attribute prefix at the start of a token on the current line.
        if scope.is_none() && ch == '#' {
            // Peek ahead for [cfg(test)], [test], or [tokio::test].
            let rest: String = chars[i..].iter().take(30).collect();
            if rest.starts_with("#[cfg(test)]")
                || rest.starts_with("#[test]")
                || rest.starts_with("#[tokio::test]")
            {
                pending_attr_line = Some(current_line);
            }
        }

        // Track brace depth.
        if ch == '{' {
            brace_depth += 1;
            if let Some(attr_line) = pending_attr_line.take() {
                // The first `{` after a test attribute opens the scope.
                scope = Some((attr_line, brace_depth));
            }
        } else if ch == '}' {
            if let Some((attr_line, scope_depth)) = scope {
                if brace_depth == scope_depth {
                    // Closing brace at the scope's depth — scope ends.
                    ranges.push((attr_line, current_line));
                    scope = None;
                }
            }
            brace_depth -= 1;
        }

        i += 1;
    }

    ranges
}

/// Returns `true` if `line` (1-based) falls in any of the given ranges (inclusive on both ends).
pub fn is_in_test_scope(line: usize, ranges: &[(usize, usize)]) -> bool {
    ranges.iter().any(|&(start, end)| line >= start && line <= end)
}

/// The note appended to floor findings whose path is in test/fixture code.
const TEST_PATH_NOTE: &str =
    " (in test/fixture code — likely a non-exploitable test value; verify)";

/// The down-ranked severity used for floor findings in test/fixture paths.
const TEST_PATH_SEVERITY: &str = "low";

/// The gate's description for a rule id, or the id if unknown.
fn title_for(rule_id: &str) -> String {
    camerata_gateway::RULE_REGISTRY
        .iter()
        .find(|e| e.id == rule_id)
        .map(|e| e.description.to_string())
        .unwrap_or_else(|| rule_id.to_string())
}

/// Audit one file's content against the content rules, line by line, reusing the
/// gate's own arms. A line the gate would deny becomes a finding tagged with `repo`.
///
/// Findings in test/fixture paths (see `is_test_or_fixture_path`) are down-ranked
/// from `critical` to `low` and annotated with a note so the architect can verify
/// without being alarmed by fake credentials in unit-test fixtures. The finding
/// is still surfaced — a real secret in a test file still merits a look.
pub fn audit_content(repo: &str, path: &str, content: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let in_test_path = is_test_or_fixture_path(path);
    let test_ranges = test_scope_line_ranges(path, content);
    // Whole-content matching (not line-by-line) so MULTI-LINE constructs are caught —
    // e.g. a `format!` SQL whose keyword and interpolation are on different lines. Each
    // match is attributed to the line where it starts.
    for rule_id in AUDIT_RULES {
        for line_no in camerata_gateway::content_match_lines(rule_id, content) {
            let snippet: String = lines
                .get(line_no.saturating_sub(1))
                .map(|l| l.trim().chars().take(160).collect())
                .unwrap_or_default();
            // Per-finding classification: in_test_path covers whole test-path files;
            // is_in_test_scope covers inline #[cfg(test)] blocks in production-path files.
            // This is per-finding-by-line: a production secret in a file that also has a
            // test block stays Critical; only findings inside a test scope are downgraded.
            let is_test = in_test_path || is_in_test_scope(line_no, &test_ranges);
            let (severity, detail, in_test, needs_review) = if is_test {
                (
                    TEST_PATH_SEVERITY.to_string(),
                    format!("{}{}", title_for(rule_id), TEST_PATH_NOTE),
                    true,
                    true,
                )
            } else {
                (
                    severity_for(rule_id).to_string(),
                    title_for(rule_id),
                    false,
                    false,
                )
            };
            findings.push(Finding {
                repo: repo.to_string(),
                path: path.to_string(),
                line: line_no,
                rule_id: rule_id.to_string(),
                severity,
                snippet,
                detail,
                status: default_status(),
                also_matches: Vec::new(),
                preview: false,
                preview_tool: None,
                in_test,
                needs_review,
            });
        }
    }
    findings
}

/// Propose the starter ruleset from the audit, classified at ALL levels so
/// placement is decided in the brownfield phase. Three tiers: repo-local content
/// rules (mechanical; the CI gate + config installed in each repo, bound to the
/// repos that have the violation); a cross-repo contract rule (only for a
/// multi-repo set; spans all repos at the integration tier, review-tier until the
/// integration gate is built); and a process rule (account-level, the VCS-action
/// gate across all repos' commits/PRs).
pub fn propose_rules(findings: &[Finding], repos: &[String]) -> Vec<ProposedRule> {
    let mut out = Vec::new();

    // 1. Content rules: universal (secrets/SQL/URL apply to ANY repo regardless of
    //    stack), so they bind to ALL scanned repos — they don't add domain
    //    ambiguity. Single-variant, mechanical, the gate lives in each repo.
    for &id in AUDIT_RULES {
        let finding_count = findings.iter().filter(|f| f.rule_id == id).count();
        out.push(ProposedRule {
            id: id.to_string(),
            title: title_for(id),
            kind: "mechanical".to_string(),
            enforcement: "mechanical".to_string(),
            options: Vec::new(),
            default_option: None,
            verification: "draft".to_string(),
            sources: Vec::new(),
            decision_question: None,
            decision_why: None,
            scope: "repo-local".to_string(),
            enforcement_point: "content".to_string(),
            domain: "security".to_string(),
            repos: repos.to_vec(),
            placement: "CI gate + gate config installed in every repo".to_string(),
            finding_count,
            recommended: true,
            // Inline deterministic-floor rules are not corpus-grounded yet.
            is_auto_recommended: false,
        });
    }

    // 2. Cross-repo contract rule: only meaningful when the set has >1 repo.
    if repos.len() > 1 {
        out.push(ProposedRule {
            id: "INTEGRATION-API-CONTRACT-1".to_string(),
            title: "Consumers match producer contracts across the repo set (shapes, \
                    status codes, events)."
                .to_string(),
            // Deterministic enforcement needs the integration gate (designed, not
            // built), so it is review-tier until that lands.
            kind: "review".to_string(),
            enforcement: "structured".to_string(),
            options: Vec::new(),
            default_option: None,
            verification: "draft".to_string(),
            sources: Vec::new(),
            decision_question: None,
            decision_why: None,
            scope: "cross-repo".to_string(),
            enforcement_point: "integration".to_string(),
            repos: repos.to_vec(),
            domain: "integration".to_string(),
            placement: "Integration gate, pre-PR, run across the assembled repo set".to_string(),
            finding_count: 0,
            recommended: true,
            is_auto_recommended: false,
        });
    }

    // 3. Process rule: account-level, all repos' commits/PRs.
    out.push(ProposedRule {
        id: "PROCESS-CONVENTIONAL-COMMIT-1".to_string(),
        title: "Commit subject follows conventional-commits (type: subject).".to_string(),
        kind: "mechanical".to_string(),
        enforcement: "mechanical".to_string(),
        options: Vec::new(),
        default_option: None,
        verification: "draft".to_string(),
        sources: Vec::new(),
        decision_question: None,
        decision_why: None,
        scope: "process".to_string(),
        domain: "process".to_string(),
        enforcement_point: "vcs-action".to_string(),
        repos: repos.to_vec(),
        placement: "VCS-action gate at commit/PR (per account, all repos)".to_string(),
        finding_count: 0,
        recommended: false,
        is_auto_recommended: false,
    });

    out
}

/// Map a file extension to a language label.
fn lang_for_ext(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => "Rust",
        "ts" | "tsx" => "TypeScript",
        "js" | "jsx" => "JavaScript",
        "py" => "Python",
        "go" => "Go",
        "cs" => "C#",
        "java" => "Java",
        "kt" => "Kotlin",
        "rb" => "Ruby",
        "php" => "PHP",
        "swift" => "Swift",
        "c" | "h" => "C",
        "cpp" => "C++",
        "sql" => "SQL",
        _ => return None,
    })
}

/// Detect frameworks from a manifest file's path + content.
fn detect_frameworks(path: &str, content: &str, out: &mut std::collections::BTreeSet<String>) {
    let file = path.rsplit_once('/').map(|(_, f)| f).unwrap_or(path);
    let lc = content.to_ascii_lowercase();
    let mut add = |s: &str| {
        out.insert(s.to_string());
    };
    match file {
        "package.json" => {
            if lc.contains("\"next\"") {
                add("Next.js");
            }
            if lc.contains("\"react\"") {
                add("React");
            }
            if lc.contains("\"vue\"") {
                add("Vue");
            }
            if lc.contains("\"@angular/core\"") {
                add("Angular");
            }
            if lc.contains("\"express\"") {
                add("Express");
            }
            if lc.contains("redux") {
                add("Redux");
            }
            if lc.contains("\"svelte\"") {
                add("Svelte");
            }
        }
        "requirements.txt" | "pyproject.toml" | "Pipfile" => {
            if lc.contains("django") {
                add("Django");
            }
            if lc.contains("flask") {
                add("Flask");
            }
            if lc.contains("fastapi") {
                add("FastAPI");
            }
            // ORM / data layer and validation library — drive the python:* + sql rule
            // domains. SQLAlchemy is the dominant Python ORM (session/scope misuse,
            // N+1 via lazy loading); Pydantic is the typed-model boundary for FastAPI.
            if lc.contains("sqlalchemy") {
                add("SQLAlchemy");
            }
            if lc.contains("pydantic") {
                add("Pydantic");
            }
        }
        "go.mod" => add("Go modules"),
        "Cargo.toml" => {
            if lc.contains("dioxus") {
                add("Dioxus");
            }
            if lc.contains("axum") {
                add("Axum");
            }
            if lc.contains("actix") {
                add("Actix");
            }
            if lc.contains("leptos") {
                add("Leptos");
            }
            // ORMs / DB layers — drive the SeaORM + SQL rule domains.
            if lc.contains("sea-orm") || lc.contains("sea_orm") || lc.contains("seaorm") {
                add("SeaORM");
            }
            if lc.contains("sqlx") {
                add("sqlx");
            }
            if lc.contains("diesel") {
                add("Diesel");
            }
        }
        "Gemfile" => {
            if lc.contains("rails") {
                add("Rails");
            }
        }
        _ => {
            if file.ends_with(".csproj") || file.ends_with(".sln") {
                add(".NET");
                if lc.contains("microsoft.aspnetcore") {
                    add("ASP.NET");
                }
            }
        }
    }
    // Path/extension signals that aren't keyed on a manifest basename. These are detected by
    // PATH/basename across ANY file (not just manifests), and every match maps to the `iac`
    // or `ci-cd` corpus domain in domains_for_stack. Detection was previously GitHub-Actions-
    // and-Terraform-only, so any other CI/IaC tooling silently produced nothing.
    //
    // Infrastructure-as-code → `iac`:
    if file.ends_with(".tf") || file.ends_with(".tf.json") {
        add("Terraform");
    }
    if file == "terragrunt.hcl" || file.ends_with(".terragrunt.hcl") {
        add("Terragrunt");
    }
    if file.ends_with(".bicep") {
        add("Bicep");
    }
    if file == "Pulumi.yaml" || file == "Pulumi.yml" {
        add("Pulumi");
    }
    // CloudFormation templates declare a format version or AWS::* resource types.
    if (file.ends_with(".yaml") || file.ends_with(".yml") || file.ends_with(".json"))
        && (lc.contains("awstemplateformatversion") || lc.contains("aws::"))
    {
        add("CloudFormation");
    }
    // CI/CD → `ci-cd`:
    if path.contains(".github/workflows/") {
        add("GitHub Actions");
    }
    if file == ".gitlab-ci.yml" || file.ends_with(".gitlab-ci.yml") {
        add("GitLab CI");
    }
    if path.contains(".circleci/") {
        add("CircleCI");
    }
    if file.starts_with("azure-pipelines") && (file.ends_with(".yml") || file.ends_with(".yaml")) {
        add("Azure Pipelines");
    }
    if file == ".travis.yml" {
        add("Travis CI");
    }
    if file == "bitbucket-pipelines.yml" {
        add("Bitbucket Pipelines");
    }
    if file == ".drone.yml" {
        add("Drone CI");
    }
    if file == "Jenkinsfile" || file.starts_with("Jenkinsfile") {
        add("Jenkins");
    }
}

/// Detect a repo's stack from its files: languages from extensions, frameworks
/// from manifests. Pure and deterministic.
pub fn detect_stack(repo: &str, files: &[(String, String)]) -> RepoStack {
    let mut languages = std::collections::BTreeSet::new();
    let mut frameworks = std::collections::BTreeSet::new();
    for (path, content) in files {
        if let Some(lang) = lang_for_ext(path) {
            languages.insert(lang.to_string());
        }
        detect_frameworks(path, content, &mut frameworks);
    }
    RepoStack {
        repo: repo.to_string(),
        languages: languages.into_iter().collect(),
        frameworks: frameworks.into_iter().collect(),
    }
}

/// Audit one repo's already-fetched files into a flat finding list (each tagged
/// with `repo`). Pure.
pub fn audit_files(repo: &str, files: &[(String, String)]) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (path, content) in files {
        findings.extend(audit_content(repo, path, content));
    }
    findings
}

/// The `<lang>:testing` corpus domain for a detected language, if one exists. Idiomatic
/// testing conventions apply to any repo in that language, so they are suggested whenever
/// that language is present (every codebase has tests).
fn testing_domain_for_language(lang: &str) -> Option<&'static str> {
    Some(match lang {
        "JavaScript" | "TypeScript" => "javascript:testing",
        "Rust" => "rust:testing",
        "Python" => "python:testing",
        "Go" => "go:testing",
        "Java" => "java:testing",
        "C#" => "csharp:testing",
        "Ruby" => "ruby:testing",
        _ => return None,
    })
}

/// The corpus domains ONE repo's stack maps to. Used to bind each rule to only the
/// repos whose domain it applies to (minimum domains per repo).
fn domains_for_stack(s: &RepoStack) -> Vec<String> {
    // Map to the ACTUAL corpus domain taxonomy (see crates/rules/principles/*):
    // rust, rust:dioxus, rust:seaorm, ui, sql, api-layer, ci-cd, permissions,
    // javascript:next, fullstack. Earlier this only emitted language domains
    // (rust/javascript) + a generic "fullstack", so framework-specific domains
    // (Dioxus / SeaORM / UI / SQL) were never suggested even when obviously present.
    let mut domains = std::collections::BTreeSet::new();
    for lang in &s.languages {
        match lang.as_str() {
            // The corpus has a `javascript` family (javascript, javascript:typescript,
            // :react, :redux, :express, :next). Map the language to its own domain so those
            // baseline rules are suggested; the child-domain → parent expansion below adds
            // plain `javascript` whenever a `javascript:*` framework domain is present.
            "JavaScript" => {
                domains.insert("javascript");
                domains.insert("fullstack");
                domains.insert("api-layer");
            }
            "TypeScript" => {
                domains.insert("javascript:typescript");
                domains.insert("fullstack");
                domains.insert("api-layer");
            }
            "Rust" => {
                domains.insert("rust");
                domains.insert("api-layer");
            }
            // Python is overwhelmingly a backend/data-layer language: it gets its own
            // `python` baseline domain (typing/idiom/web-API rules), the cross-language
            // `api-layer` architecture rules, and the generic `sql` rules (raw-SQL-via-
            // f-string is a textbook Python footgun the deterministic floor catches).
            // Framework specifics (FastAPI/Django/Flask/SQLAlchemy) are added in the
            // framework loop below as `python:*` child domains.
            "Python" => {
                domains.insert("python");
                domains.insert("api-layer");
                domains.insert("sql");
            }
            // A repo with hand-written .sql files clearly has a SQL surface.
            "SQL" => {
                domains.insert("sql");
            }
            // Other backend languages map to the API-layer architecture rules.
            _ => {
                domains.insert("api-layer");
            }
        }
        // Suggest the language's idiomatic testing corpus whenever the language is present.
        if let Some(t) = testing_domain_for_language(lang) {
            domains.insert(t);
        }
    }
    for fw in &s.frameworks {
        match fw.as_str() {
            "Dioxus" => {
                domains.insert("rust:dioxus");
                domains.insert("ui");
            }
            "Leptos" => {
                domains.insert("ui");
            }
            // SeaORM is the only data layer that maps to the SeaORM-specific domain
            // (`rust:seaorm` holds entity-pattern + SeaORM-raw-SQL rules). sqlx and Diesel
            // are NOT SeaORM — proposing entity/SeaORM rules for a sqlx repo is a misfire
            // (#52). They still get the generic SQL + migration-hygiene (ci-cd) rules, which
            // apply to any SQL data layer (the raw-SQL-concat critical is the deterministic
            // floor and fires regardless of domain).
            "SeaORM" => {
                domains.insert("rust:seaorm");
                domains.insert("sql");
                domains.insert("ci-cd");
            }
            "Diesel" | "sqlx" => {
                domains.insert("sql");
                domains.insert("ci-cd");
            }
            "Next.js" => {
                domains.insert("javascript:next");
                domains.insert("fullstack");
                domains.insert("ui");
            }
            "React" => {
                domains.insert("javascript:react");
                domains.insert("ui");
                domains.insert("fullstack");
            }
            "Redux" => {
                domains.insert("javascript:redux");
                domains.insert("fullstack");
            }
            "Vue" | "Svelte" | "Angular" => {
                domains.insert("ui");
                domains.insert("fullstack");
            }
            "Express" => {
                domains.insert("javascript:express");
                domains.insert("api-layer");
            }
            "Axum" | "Actix" | "Rails" | "ASP.NET" => {
                domains.insert("api-layer");
            }
            // Python web frameworks map to their `python:*` child domain (which pulls in
            // the `python` baseline via the child→parent expansion below) plus the
            // cross-language `api-layer` rules. Each child domain holds the framework's
            // own architectural rules (FastAPI dependency injection, Django service layer,
            // etc.).
            "FastAPI" => {
                domains.insert("python:fastapi");
                domains.insert("api-layer");
            }
            "Django" => {
                domains.insert("python:django");
                domains.insert("api-layer");
            }
            "Flask" => {
                domains.insert("python:flask");
                domains.insert("api-layer");
            }
            // SQLAlchemy is a Python data layer: it pulls in the `python` baseline plus
            // the generic SQL + migration-hygiene rules (same shape as sqlx/Diesel).
            "SQLAlchemy" => {
                domains.insert("python");
                domains.insert("sql");
                domains.insert("ci-cd");
            }
            // Pydantic is the typed-model boundary library; its rules live in the
            // `python` baseline domain.
            "Pydantic" => {
                domains.insert("python");
            }
            // Infrastructure-as-code tooling → the `iac` corpus domain.
            "Terraform" | "Terragrunt" | "Bicep" | "Pulumi" | "CloudFormation" => {
                domains.insert("iac");
            }
            // CI/CD platforms → the `ci-cd` corpus domain.
            "GitHub Actions"
            | "GitLab CI"
            | "CircleCI"
            | "Azure Pipelines"
            | "Travis CI"
            | "Bitbucket Pipelines"
            | "Drone CI"
            | "Jenkins" => {
                domains.insert("ci-cd");
            }
            _ => {}
        }
    }
    // Any app with a backend API layer almost certainly enforces authorization, so
    // suggest the permissions rules too. (The `agentic` domain is always-suggested
    // downstream in propose_corpus_rules, regardless of stack.)
    if domains.contains("api-layer") {
        domains.insert("permissions");
    }
    // Universal testing principles (the test pyramid, AAA, determinism, etc.) apply to EVERY
    // repo, so the cross-language `testing` domain is always suggested.
    domains.insert("testing");
    // A child domain ALWAYS implies its parent: recommending `javascript:next` without
    // `javascript` is incoherent (the framework rules sit on top of the language baseline)
    // and reads as a bug in the UI (child ticked, parent not). Add the primary component of
    // every namespaced domain. The split borrows from the 'static keys, so it stays `&str`.
    let parents: Vec<&str> = domains
        .iter()
        .filter_map(|d| d.split_once(':').map(|(p, _)| p))
        .collect();
    for p in parents {
        domains.insert(p);
    }
    domains.into_iter().map(String::from).collect()
}

/// Propose corpus rules (the architectural ones that carry ALTERNATIVES) for the
/// detected stacks, each bound to ONLY the repos whose domain it applies to (a
/// universal `*` rule binds to all). The architect can override the binding. Each
/// carries its options + default so the architect chooses which alternative to
/// codify. finding_count is 0: scanning these needs the per-language AST checker
/// (future); the selection is real now.
///
/// `repo_domains` is each repo paired with the corpus domains its stack maps to.
pub async fn propose_corpus_rules(repo_domains: &[(String, Vec<String>)]) -> Vec<ProposedRule> {
    let path = camerata_rules::corpus_path();
    if !path.exists() {
        return Vec::new();
    }
    let (set, _errs) = camerata_rules::load_corpus_lenient(&path).await;
    // The union of all repos' domains selects the candidate rules from the corpus.
    let mut all_domains = std::collections::BTreeSet::new();
    for (_, ds) in repo_domains {
        for d in ds {
            all_domains.insert(d.clone());
        }
    }
    let all_repos: Vec<String> = repo_domains.iter().map(|(repo, _)| repo.clone()).collect();
    // ALL corpus rules, not just the domain-matched ones — the architect should see the
    // whole library and the suggested subset in one place. A rule whose domain matches the
    // scanned stack is SUGGESTED (recommended) and pre-bound to its matching repos; the
    // rest are AVAILABLE (recommended=false), bound to all repos so they can still be armed.
    let mut proposed = set
        .iter()
        .map(|r| {
            let matched_repos: Vec<String> = if r.domain == "*" {
                all_repos.clone()
            } else {
                repo_domains
                    .iter()
                    .filter(|(_, ds)| ds.iter().any(|d| d == &r.domain))
                    .map(|(repo, _)| repo.clone())
                    .collect()
            };
            let suggested = !matched_repos.is_empty();
            let repos = if suggested {
                matched_repos
            } else {
                all_repos.clone()
            };
            (r, repos, suggested)
        })
        .map(|(r, repos, is_suggested)| {
            let options = r
                .options
                .iter()
                .map(|o| RuleOptionView {
                    id: o.id.clone(),
                    label: o.label.clone(),
                    directive: o.directive.clone(),
                    why: o.why.clone(),
                })
                .collect();
            let enforcement = r.enforcement.as_str();
            // Both mechanical and architectural tiers carry a deterministic CI-tier check;
            // everything else is human-reviewed.
            let kind = if r.enforcement.is_ci_enforced() {
                "mechanical"
            } else {
                "review"
            };
            // Placement is HONEST per enforcement tier, not a one-size string: mechanical and
            // architectural rules get a deterministic CI-tier check; structured/prose rules are
            // human-reviewed at PR (structured against CONVENTIONS.md, prose as AGENTS.md guidance).
            let placement = match r.enforcement {
                camerata_rules::EnforcementKind::Mechanical => {
                    "Mechanical CI gate (deterministic check) in each repo this rule's domain applies to"
                }
                camerata_rules::EnforcementKind::Architectural => {
                    "Architectural CI gate (deterministic AST/static-analysis check) in each repo this rule's domain applies to"
                }
                camerata_rules::EnforcementKind::Structured => {
                    "Reviewed at PR against CONVENTIONS.md (structured; no mechanical gate)"
                }
                camerata_rules::EnforcementKind::Prose => {
                    "Guidance in AGENTS.md, reviewed at PR (prose; no mechanical gate)"
                }
            };
            let sources = r
                .sources
                .iter()
                .map(|s| RuleSourceView {
                    url: s.url.clone(),
                    title: s.title.clone(),
                    linter: s.linter.clone(),
                })
                .collect();
            ProposedRule {
                id: r.id.0.clone(),
                title: r.title.clone(),
                kind: kind.to_string(),
                enforcement: enforcement.to_string(),
                options,
                default_option: r.default_option.clone(),
                verification: r.verification().to_string(),
                sources,
                decision_question: r.decision_question.clone(),
                decision_why: r.decision_why.clone(),
                scope: "repo-local".to_string(),
                domain: r.domain.clone(),
                enforcement_point: "content".to_string(),
                repos,
                placement: placement.to_string(),
                finding_count: 0,
                // SUGGESTED = the rule's domain matches the scanned stack. AGENTIC rules
                // are ALWAYS suggested by design (they govern how the AI fleet builds,
                // regardless of stack). The rest are available but not recommended here.
                // OPT-IN ONLY rules (e.g. CICD-CODEQL-SECURITY-SCAN-1,
                // CICD-SEMGREP-SECURITY-SCAN-1) are excluded from the "✓ Recommended"
                // badge even when stack-relevant — they are available for opt-in but
                // must not signal "recommended" in the UI.
                recommended: (is_suggested || r.domain == "agentic") && !r.is_opt_in_only(),
                // AUTO-RECOMMENDED (pre-checked) = stack-relevant AND grounded/verified.
                // Stack-relevant means the rule's domain matches the scanned stack (or it's
                // an `agentic` rule, which governs the AI fleet regardless of stack). A
                // grounded rule for a language the repo does NOT use must never be pre-checked
                // (e.g. Go/Ruby/Python rules on a TS/Node repo); and a draft/needs_recheck
                // rule is never pre-checked even when stack-relevant. Without the stack gate,
                // every grounded rule in the whole corpus was auto-recommended on every repo.
                //
                // OPT-IN ONLY rules (e.g. the CI-security Semgrep/CodeQL rules) are NEVER
                // pre-checked, even when grounded and stack-relevant — they still appear in the
                // proposal so the architect can deliberately opt in. `!r.is_opt_in_only()` is the
                // gate that enforces this.
                is_auto_recommended: (is_suggested || r.domain == "agentic")
                    && r.is_auto_recommended()
                    && !r.is_opt_in_only(),
            }
        })
        .collect::<Vec<_>>();
    // Order SUGGESTED rules first, then the rest — grouped by domain, the suggested
    // domains surface at the top.
    proposed.sort_by(|a, b| {
        b.recommended
            .cmp(&a.recommended)
            .then_with(|| a.domain.cmp(&b.domain))
    });
    proposed
}

/// Build a report from already-aggregated findings + per-repo stacks. Pure.
pub fn build_report(
    repos: Vec<String>,
    stacks: Vec<RepoStack>,
    files_scanned: usize,
    findings: Vec<Finding>,
) -> ScanReport {
    let proposed_rules = propose_rules(&findings, &repos);
    ScanReport {
        repos,
        stacks,
        files_scanned,
        files_excluded: 0,
        excluded_mechanical_rules: Vec::new(),
        code_chars: 0,
        findings,
        proposed_rules,
        gated: false,
        message: None,
        actual_usage: None,
        deep: None,
        coverage_notes: Vec::new(),
    }
}

// ── Tech-debt ticket (accept findings as debt -> open a GitHub issue) ───────────

/// Escape a single field for RFC 4180 CSV: if the value contains a comma, double-quote,
/// or newline, wrap it in double-quotes and double any internal double-quotes.
fn csv_escape(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r') {
        let escaped = field.replace('"', "\"\"");
        format!("\"{escaped}\"")
    } else {
        field.to_string()
    }
}

/// Render accepted findings for one repo as a CSV (RFC 4180).
///
/// Columns: `rule_id`, `severity`, `path`, `line`, `detail`.
///
/// Fields containing commas, double-quotes, or newlines are quoted; internal
/// double-quotes are doubled. This is a pure function and is used by
/// [`tech_debt_issue_body`] to embed a fenced ```csv block in each repo's
/// issue body.
pub fn tech_debt_csv(findings: &[Finding]) -> String {
    let mut out = String::from("rule_id,severity,path,line,detail\n");
    for f in findings {
        out.push_str(&csv_escape(&f.rule_id));
        out.push(',');
        out.push_str(&csv_escape(&f.severity));
        out.push(',');
        out.push_str(&csv_escape(&f.path));
        out.push(',');
        // Line is a usize — never needs escaping.
        out.push_str(&f.line.to_string());
        out.push(',');
        out.push_str(&csv_escape(&f.detail));
        out.push('\n');
    }
    out
}

/// Render selected findings as a GitHub issue body, grouped by repo.
///
/// Each repo section includes a fenced ```csv block containing that repo's
/// findings (columns: rule_id, severity, path, line, detail). GitHub Issues
/// cannot receive true file attachments via the API, so the CSV is embedded
/// inline as a fenced code block — the pragmatic delivery path that requires
/// no new API capability.
pub fn tech_debt_issue_body(findings: &[Finding]) -> String {
    use std::collections::BTreeMap;
    let mut s = String::from(
        "Accepted tech debt from a Camerata brownfield audit. These existing \
         violations were reviewed and deferred.\n\n",
    );
    s.push_str(&format!("**{} finding(s):**\n\n", findings.len()));
    let mut by_repo: BTreeMap<&str, Vec<&Finding>> = BTreeMap::new();
    for f in findings {
        by_repo.entry(f.repo.as_str()).or_default().push(f);
    }
    for (repo, fs) in &by_repo {
        s.push_str(&format!("### {repo}\n\n"));
        for f in fs.iter() {
            s.push_str(&format!(
                "- **[{}]** `{}` — `{}:{}`\n",
                f.severity.to_uppercase(),
                f.rule_id,
                f.path,
                f.line
            ));
        }
        s.push('\n');
        // Embed a per-repo CSV so each issue is self-contained and machine-readable.
        // The CSV columns mirror the Finding fields most useful for triage tooling:
        // rule_id, severity, path, line, detail.
        let repo_findings: Vec<Finding> = fs.iter().map(|f| (*f).clone()).collect();
        let csv = tech_debt_csv(&repo_findings);
        s.push_str("```csv\n");
        s.push_str(&csv);
        s.push_str("```\n\n");
    }
    s.push_str("\n_Filed by Camerata onboarding._");
    s
}

/// Open a GitHub issue in `owner/repo` with the selected findings as accepted tech
/// debt. Returns the issue URL. Needs Issues write on the token.
pub async fn create_tech_debt_ticket(
    owner: &str,
    repo: &str,
    token: &str,
    title: &str,
    findings: &[Finding],
) -> anyhow::Result<String> {
    create_issue(owner, repo, token, title, &tech_debt_issue_body(findings)).await
}

/// Create a GitHub issue (the generic "emit a story to the tracker" primitive). Onboarding
/// produces stories — tech-debt tickets, the CI-wiring task, resolve-now items — as issues;
/// the dev layer (Pillar 2) does the actual work. Returns the issue's html_url.
pub async fn create_issue(
    owner: &str,
    repo: &str,
    token: &str,
    title: &str,
    body: &str,
) -> anyhow::Result<String> {
    use camerata_worktracker::{HttpTransport, ReqwestTransport};
    let transport = ReqwestTransport::new(format!("Bearer {token}"))?;
    let url = format!("https://api.github.com/repos/{owner}/{repo}/issues");
    let payload = serde_json::to_string(&serde_json::json!({
        "title": title,
        "body": body,
    }))?;
    let resp = transport.post(&url, &payload).await?;
    if !(200..300).contains(&resp.status) {
        anyhow::bail!("GitHub create issue: HTTP {} {}", resp.status, resp.body);
    }
    let v: serde_json::Value = serde_json::from_str(&resp.body)?;
    Ok(v["html_url"].as_str().unwrap_or_default().to_string())
}

// ── Local repo reader (reads code from disk; never GitHub) ──────────────────────

/// Safety net for pathological monorepos so one scan can't exhaust memory. This
/// is NOT a per-scan window that rotates: a single tarball download covers the
/// WHOLE repo, and only a repo with more than this many auditable files is
/// truncated (and the report says so). Normal repos are fully scanned.
const HARD_CAP_FILES: usize = 20_000;
/// Skip files larger than this (likely generated/vendored/binary).
const MAX_FILE_BYTES: usize = 400_000;

/// Extensions worth auditing (source + config text). Keeps the scan off images,
/// lockfiles, and binaries.
const CODE_EXTS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "rb", "php", "cs", "sql", "toml", "yaml",
    "yml", "json", "sh", "env", "cfg", "ini", "tf", "kt", "swift", "c", "cpp", "h",
    // IaC: Terragrunt/Packer HCL, Azure Bicep (Terraform `.tf` is already above).
    "hcl", "bicep",
];

/// Extensionless basenames that still carry stack/CI signal and must be extracted so
/// detection can see them (e.g. a Jenkins pipeline). Without this they'd be dropped as
/// "no code extension" and the CI/CD domain would never be detected from them.
const CODE_BASENAMES: &[&str] = &["Jenkinsfile"];

fn has_code_ext(path: &str) -> bool {
    let basename = path.rsplit('/').next().unwrap_or(path);
    if CODE_BASENAMES
        .iter()
        .any(|b| basename == *b || basename.starts_with(b))
    {
        return true;
    }
    match path.rsplit_once('.') {
        Some((_, ext)) => CODE_EXTS.contains(&ext.to_ascii_lowercase().as_str()),
        None => false,
    }
}

/// Directory names that are build output, dependency trees, caches, or tool state — pure
/// noise for an architecture audit, and the bulk of a repo's bytes/tokens. A real consumer
/// found 14 of 25 MB of one monorepo was `.turbo/cache` manifests + lockfiles; scanning
/// that is paying to audit generated artifacts. Matched on ANY path segment, so
/// `apps/web/node_modules/...` and `node_modules/...` both prune. Extend per-project via
/// the `CAMERATA_SCAN_EXCLUDE_DIRS` env (comma-separated extra dir names).
const NOISE_DIRS: &[&str] = &[
    "node_modules",
    "bower_components",
    "jspm_packages",
    ".yarn",
    ".pnpm-store",
    ".git",
    ".svn",
    ".hg",
    "target",
    "dist",
    "build",
    "out",
    "obj",
    "bin",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".angular",
    ".expo",
    ".docusaurus",
    "storybook-static",
    ".turbo",
    ".cache",
    ".parcel-cache",
    ".serverless",
    "coverage",
    ".nyc_output",
    "vendor",
    "Pods",
    "DerivedData",
    ".dart_tool",
    ".venv",
    "venv",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".tox",
    ".gradle",
    ".terraform",
    ".terragrunt-cache",
    ".idea",
    ".vscode",
    // Generated-code + test-artifact dirs (codegen output, snapshot fixtures).
    "generated",
    "__generated__",
    "__snapshots__",
    "node_modules.bin",
];

/// Generated / lock / vendored FILE basenames that carry no architectural signal but are
/// large (lockfiles are often the single biggest text files in a repo).
const NOISE_FILES: &[&str] = &[
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "packages.lock.json",
    "Cargo.lock",
    "composer.lock",
    "Gemfile.lock",
    "poetry.lock",
    "Pipfile.lock",
    "go.sum",
    "bun.lock",
    "deno.lock",
    "flake.lock",
];

/// Generated-file suffixes: minified bundles, source maps, and codegen output. The codegen
/// patterns (`.gen.ts`, `.pb.go`, protobuf/relay/graphql/openapi output, etc.) are machine-
/// written from a schema — auditing them is paying to review code no human owns.
const NOISE_SUFFIXES: &[&str] = &[
    ".min.js",
    ".min.css",
    ".bundle.js",
    ".map",
    ".gen.ts",
    ".gen.tsx",
    ".gen.js",
    ".gen.go",
    ".gen.dart",
    ".generated.ts",
    ".generated.tsx",
    ".generated.js",
    ".generated.go",
    ".generated.cs",
    ".pb.go",
    ".pb.ts",
    ".pb.cc",
    ".pb.h",
    "_pb2.py",
    "_pb2.pyi",
    ".g.dart",
    ".freezed.dart",
];

/// True when a path should be pruned BEFORE scanning: it lives under a build/dep/cache
/// directory, or is a lockfile / minified bundle / source map. `extra_dirs` holds any
/// project-specific dir names from `CAMERATA_SCAN_EXCLUDE_DIRS`.
fn is_noise_path(path: &str, extra_dirs: &[String]) -> bool {
    let mut segments = path.split('/');
    let basename = path.rsplit('/').next().unwrap_or(path);
    if NOISE_FILES.contains(&basename) {
        return true;
    }
    if NOISE_SUFFIXES.iter().any(|s| basename.ends_with(s)) {
        return true;
    }
    segments.any(|seg| NOISE_DIRS.contains(&seg) || extra_dirs.iter().any(|d| d == seg))
}

/// Parse the `CAMERATA_SCAN_EXCLUDE_DIRS` env (comma-separated) into extra dir names.
fn extra_exclude_dirs() -> Vec<String> {
    std::env::var("CAMERATA_SCAN_EXCLUDE_DIRS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// What `read_local_repo_files` pulled from a working tree: the auditable files, whether the file
/// cap was hit, and how many would-be-scannable files were pruned as noise (so the scan can
/// SHOW the filter doing its job — "1,583 scanned, 2,800 excluded as build/generated noise").
pub struct ExtractedRepo {
    pub files: Vec<(String, String)>,
    pub truncated: bool,
    pub excluded_noise: usize,
}

/// Read a repo's auditable files from its LOCAL working tree — the local-first scan source.
/// Onboarding (scan + audit) reads the code that's on disk; GitHub is never consulted for
/// code (only later, at development time, for clone/fetch/push). Applies the same filters
/// the scan has always used: noise pruning, code-extension filter, per-file size cap, and HARD_CAP_FILES
/// safety net, but walks the directory on disk. Paths are relative to the repo root,
/// forward-slashed. Noise directories (.git / node_modules / target / …) are pruned DURING
/// descent so we never recurse into them. Synchronous (blocking IO) — call via spawn_blocking.
pub fn read_local_repo_files(root: &std::path::Path) -> anyhow::Result<ExtractedRepo> {
    if !root.join(".git").exists() {
        anyhow::bail!("{} is not a local git clone", root.display());
    }
    let extra_dirs = extra_exclude_dirs();
    let mut files = Vec::new();
    let mut excluded_noise = 0usize;
    let mut truncated = false;
    // Iterative DFS so a deep tree can't blow the stack.
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            // Path relative to the repo root, forward-slashed (matches tarball paths).
            let Ok(rel) = p.strip_prefix(root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if rel.is_empty() {
                continue;
            }
            if ft.is_dir() {
                // Prune noise dirs (don't descend) — is_noise_path matches any segment, so a
                // noise dir name prunes the whole subtree before we read a single file in it.
                if !is_noise_path(&rel, &extra_dirs) {
                    stack.push(p);
                }
                continue;
            }
            if !ft.is_file() {
                continue; // skip symlinks / fifos / etc.
            }
            let noise = is_noise_path(&rel, &extra_dirs);
            let code = has_code_ext(&rel);
            if noise && code {
                excluded_noise += 1;
            }
            if noise || !code {
                continue;
            }
            if entry
                .metadata()
                .map(|m| m.len() as usize)
                .unwrap_or(usize::MAX)
                > MAX_FILE_BYTES
            {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&p) else {
                continue; // skip non-UTF-8 / unreadable
            };
            files.push((rel, content));
            if files.len() >= HARD_CAP_FILES {
                truncated = true;
                break;
            }
        }
        if truncated {
            break;
        }
    }
    Ok(ExtractedRepo {
        files,
        truncated,
        excluded_noise,
    })
}

/// Scan a SET of repos end to end: download and audit each whole repo, then
/// aggregate the findings and proposed ruleset across all of them (each finding
/// tagged with its repo). Brownfield onboarding of inter-related repos (an API, a
/// worker, a frontend) is one scan. A per-repo failure (bad name, no access) is
/// noted in the report message and does not abort the others; the scan returns
/// what it could read. The token is required (the caller gates on it).
/// Build the central suppression registry across a project's repos: every inline
/// `camerata:allow` waiver + every `.camerata/baseline.json` entry, each flagged stale
/// against the current deterministic findings. This is the "show me everything we've
/// waived" audit view (the require-indexing invariant). Uses the cheap mechanical audit
/// for stale-detection (free, deterministic).
pub async fn suppression_registry(
    sources: &[(String, std::path::PathBuf)],
) -> Vec<crate::suppression::SuppressionRecord> {
    use crate::suppression::{parse_inline_waivers, registry, Baseline, FindingRef};
    let mut out = Vec::new();
    for (spec, dir) in sources {
        let spec = spec.as_str();
        let dir = dir.clone();
        let Ok(Ok(extracted)) =
            tokio::task::spawn_blocking(move || read_local_repo_files(&dir)).await
        else {
            continue;
        };
        let files = extracted.files;
        let mut inline = Vec::new();
        for (path, content) in &files {
            inline.extend(parse_inline_waivers(path, content));
        }
        let baseline = files
            .iter()
            .find(|(p, _)| p == ".camerata/baseline.json")
            .and_then(|(_, c)| serde_json::from_str::<Baseline>(c).ok())
            .unwrap_or_default();
        let findings: Vec<FindingRef> = audit_files(spec, &files)
            .into_iter()
            .map(|f| FindingRef {
                rule_id: f.rule_id,
                path: f.path,
                line: f.line,
                snippet: f.snippet,
            })
            .collect();
        out.extend(registry(&inline, &baseline, &findings));
    }
    out
}

/// Classify a repo's findings against its suppressions (inline `camerata:allow` waivers
/// parsed from the files + the committed `.camerata/baseline.json`), setting each
/// finding's `status`. Also appends a `CAM-WAIVER-NEEDS-REASON` finding for every
/// reason-less waiver (the require-reason invariant). REPORT everything; the `status`
/// is what lets enforcement act on the delta only.
fn classify_repo_findings(findings: &mut Vec<Finding>, repo: &str, files: &[(String, String)]) {
    use crate::suppression::{
        classify_one, parse_inline_waivers, reasonless_waivers, Baseline, FindingRef, Status,
        REASONLESS_RULE_ID,
    };

    let mut inline = Vec::new();
    for (path, content) in files {
        inline.extend(parse_inline_waivers(path, content));
    }
    let baseline = files
        .iter()
        .find(|(p, _)| p == ".camerata/baseline.json")
        .and_then(|(_, c)| serde_json::from_str::<Baseline>(c).ok())
        .unwrap_or_default();

    for f in findings.iter_mut() {
        let fr = FindingRef {
            rule_id: f.rule_id.clone(),
            path: f.path.clone(),
            line: f.line,
            snippet: f.snippet.clone(),
        };
        f.status = match classify_one(&fr, &inline, &baseline) {
            Status::Active => "active",
            Status::SuppressedInline => "suppressed-inline",
            Status::SuppressedBaseline => "suppressed-baseline",
        }
        .to_string();
    }

    // A reason-less waiver is itself a violation (the un-auditable hole this prevents).
    for w in reasonless_waivers(&inline) {
        findings.push(Finding {
            repo: repo.to_string(),
            path: w.path.clone(),
            line: w.line,
            rule_id: REASONLESS_RULE_ID.to_string(),
            severity: "high".to_string(),
            snippet: "camerata:allow without a reason".to_string(),
            detail: "A waiver must carry a justification (`-- reason`); a reason-less \
                     suppression is itself a violation."
                .to_string(),
            status: "active".to_string(),
            also_matches: Vec::new(),
            preview: false,
            preview_tool: None,
            in_test: false,
            needs_review: false,
        });
    }
}

// ── Greenfield scaffold ──────────────────────────────────────────────────────

/// The outcome of a greenfield scaffold operation: the local directory created,
/// the governance files written into it, and the git commit sha of the initial
/// commit.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GreenfieldResult {
    /// Absolute path to the newly-created repo directory on disk.
    pub path: String,
    /// Governance files written (path -> content), in the order they were written.
    pub files_written: Vec<String>,
    /// The sha of the initial commit (shortened), or empty on commit failure.
    pub commit_sha: String,
    /// Human-readable summary for the UI.
    pub message: String,
}

/// Scaffold a NEW local git repo with governance baked in from commit zero.
///
/// Given a target directory (`dest`) that MUST NOT already exist, a list of arm
/// rules (already resolved by the caller — same shape `arm.rs` emits), and the
/// project's custom rules, this function:
///
/// 1. Creates `dest` and `git init`s it.
/// 2. Calls [`crate::arm::arm_files_for_repo`] to emit AGENTS.md, CONVENTIONS.md,
///    `.camerata/rules.json`, and (when mechanical rules are present) the CI
///    governance workflow — reusing the EXACT same emit path as the brownfield apply
///    flow so there is no duplicate logic.
/// 3. Writes every emitted file into the new working tree, creating parent dirs.
/// 4. Stages all files (`git add -A`) and makes the initial commit.
/// 5. Returns a [`GreenfieldResult`] describing what was created.
///
/// The function is intentionally synchronous-via-blocking (call via
/// `tokio::task::spawn_blocking`) so the git operations don't block the async runtime.
pub fn scaffold_greenfield_blocking(
    dest: &std::path::Path,
    rules: &[&crate::arm::ArmRule],
    custom: &[&crate::project::CustomRule],
    repo_label: &str,
) -> anyhow::Result<GreenfieldResult> {
    // Safety: refuse to clobber an existing directory.
    if dest.exists() {
        anyhow::bail!(
            "{} already exists — greenfield scaffold requires a new (non-existent) directory",
            dest.display()
        );
    }

    // 1. Create the root directory (and any parents the caller chose to nest under).
    std::fs::create_dir_all(dest).map_err(|e| {
        anyhow::anyhow!("could not create {}: {e}", dest.display())
    })?;

    // 2. `git init` the new directory.
    let git_init = std::process::Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dest)
        .output()
        .map_err(|e| anyhow::anyhow!("git init failed: {e}"))?;
    if !git_init.status.success() {
        // Older git versions don't support `-b main`; fall back to plain `init`.
        let git_init2 = std::process::Command::new("git")
            .arg("init")
            .current_dir(dest)
            .output()
            .map_err(|e| anyhow::anyhow!("git init failed: {e}"))?;
        if !git_init2.status.success() {
            let err = String::from_utf8_lossy(&git_init2.stderr);
            anyhow::bail!("git init: {err}");
        }
    }

    // 3. Emit governance files using the SAME arm_files_for_repo primitive as the
    //    brownfield apply path — zero code duplication, guaranteed identical output.
    let emitted = crate::arm::arm_files_for_repo(rules, custom);

    // 4. Write every emitted file into the working tree.
    let mut files_written = Vec::with_capacity(emitted.len());
    for (rel, content) in &emitted {
        let full = dest.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("create dir {}: {e}", parent.display()))?;
        }
        std::fs::write(&full, content)
            .map_err(|e| anyhow::anyhow!("write {}: {e}", full.display()))?;
        files_written.push(rel.clone());
    }

    // 5. Stage all files.
    let add = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dest)
        .output()
        .map_err(|e| anyhow::anyhow!("git add: {e}"))?;
    if !add.status.success() {
        let err = String::from_utf8_lossy(&add.stderr);
        anyhow::bail!("git add: {err}");
    }

    // 6. Initial commit. We need a user identity; use a fallback when the environment
    //    has no global git config (common in CI/test environments).
    let _ = std::process::Command::new("git")
        .args(["config", "user.email", "camerata@example.com"])
        .current_dir(dest)
        .output();
    let _ = std::process::Command::new("git")
        .args(["config", "user.name", "Camerata"])
        .current_dir(dest)
        .output();

    let commit_msg = format!(
        "chore(governance): greenfield scaffold for {repo_label}\n\n\
         Governance baked in from commit zero via Camerata.\n\
         Rules: AGENTS.md, CONVENTIONS.md, .camerata/rules.json"
    );
    let commit = std::process::Command::new("git")
        .args(["commit", "-m", &commit_msg])
        .current_dir(dest)
        .output()
        .map_err(|e| anyhow::anyhow!("git commit: {e}"))?;
    if !commit.status.success() {
        let err = String::from_utf8_lossy(&commit.stderr);
        anyhow::bail!("git commit: {err}");
    }

    // 7. Read the short sha of the initial commit.
    let commit_sha = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(dest)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let n_rules = rules.len();
    let n_custom = custom.len();
    let message = format!(
        "Scaffolded {repo_label} at {} with {n_rules} base rule(s) and {n_custom} custom rule(s). \
         {n_files} governance file(s) committed as the initial commit ({commit_sha}).",
        dest.display(),
        n_files = files_written.len(),
    );

    Ok(GreenfieldResult {
        path: dest.to_string_lossy().into_owned(),
        files_written,
        commit_sha,
        message,
    })
}

/// Phase 1 — DETECT. Fetch the repos, detect each stack, and PROPOSE a starter ruleset.
/// It does NOT audit code yet — that's [`audit_repos`], run after the architect picks
/// which rules to enforce. This is the "scan to determine languages / frameworks /
/// domains → suggest rules" step the two-phase flow opens with.
/// Scan a set of LOCAL repo working trees: detect each stack and propose a starter ruleset.
/// `sources` pairs each repo's `owner/repo` label with its local clone directory. Reads code
/// from disk (local-first); GitHub is never consulted here. `extra_notes` carries messages
/// from the caller's path resolution (e.g. repos that had no local folder) so they surface in
/// the report alongside per-repo scan notes.
pub async fn scan_repos(
    sources: &[(String, std::path::PathBuf)],
    extra_notes: Vec<String>,
) -> ScanReport {
    let mut stacks = Vec::new();
    let mut files_total = 0usize;
    let mut files_excluded = 0usize;
    let mut code_chars = 0usize;
    let mut repos_ok = Vec::new();
    let mut notes = extra_notes;

    for (spec, dir) in sources {
        let spec = spec.trim();
        if spec.is_empty() {
            continue;
        }
        let dir = dir.clone();
        match tokio::task::spawn_blocking(move || read_local_repo_files(&dir)).await {
            Ok(Ok(extracted)) => {
                let ExtractedRepo {
                    files,
                    truncated,
                    excluded_noise,
                } = extracted;
                files_total += files.len();
                files_excluded += excluded_noise;
                code_chars += files.iter().map(|(_, c)| c.len()).sum::<usize>();
                stacks.push(detect_stack(spec, &files));
                repos_ok.push(spec.to_string());
                if truncated {
                    notes.push(format!(
                        "{spec}: more than {HARD_CAP_FILES} files; truncated at the safety limit"
                    ));
                }
            }
            Ok(Err(e)) => notes.push(format!("{spec}: scan failed ({e})")),
            Err(e) => notes.push(format!("{spec}: scan task failed ({e})")),
        }
    }

    let repo_domains: Vec<(String, Vec<String>)> = stacks
        .iter()
        .map(|s| (s.repo.clone(), domains_for_stack(s)))
        .collect();
    let mut report = build_report(repos_ok, stacks, files_total, Vec::new());
    report.code_chars = code_chars;
    report.files_excluded = files_excluded;
    report.proposed_rules = propose_corpus_rules(&repo_domains).await;
    if !notes.is_empty() {
        report.message = Some(notes.join(" · "));
    }
    report
}

/// One rule the architect selected for the Phase-2 audit, with its per-repo binding.
///
/// A SelectedRule with an EMPTY `repos` is PROJECT-LEVEL: it applies to every repo in the
/// scan. A SelectedRule with a NON-EMPTY `repos` applies ONLY to those repos. So the
/// effective LLM rule set for any one repo is `(project-level rules) ∪ (rules bound to that
/// repo)` — matching the onboarding decision that project-level rules scan across the board
/// while per-repo selections are additive on top. This is what makes a multi-repo scan run
/// each repo against ITS OWN chosen rules instead of every rule across the board.
#[derive(Debug, Clone)]
pub struct SelectedRule {
    /// The rule id.
    pub id: String,
    /// The chosen directive text the audit prompt is parameterized by.
    pub directive: String,
    /// Repos this rule is scoped to. EMPTY = project-level (applies to all repos).
    pub repos: Vec<String>,
}

impl SelectedRule {
    /// Does this selected rule apply to `repo`? Project-level rules (empty `repos`) apply to
    /// every repo; a repo-scoped rule applies only when `repo` is in its set.
    pub fn applies_to(&self, repo: &str) -> bool {
        self.repos.is_empty() || self.repos.iter().any(|r| r == repo)
    }
}

/// Phase 2 — AUDIT against the SELECTED rules. After the architect picks rules (Phase 1),
/// this audits the code: the deterministic content rules (secrets / raw-SQL / secret-URL)
/// are the always-on SECURITY floor and produce ENFORCED findings; the AI audit is
/// PARAMETERIZED by the selected rules' directives (so it checks the code against what the
/// project actually adopted) and produces ADVISORY findings plus its investigative pass.
/// Each repo is audited against only the rules that [`SelectedRule::applies_to`] it (its
/// own selections plus the project-level set), never the whole selection across the board.
/// Whether a rule describes what CODE should look like (audit it against source) vs how
/// the FLEET/TEAM operates (governance/process — arm it, but don't code-audit). The
/// orchestration (`ORCH-`), meta-principle (`SPIRIT-`), and process (`PROC-`) families
/// are governance/process; everything else (ARCH-/RUST-/SQL-/UI-/SEC-/…) is code.
fn is_code_auditable_rule(id: &str) -> bool {
    !(id.starts_with("ORCH-") || id.starts_with("SPIRIT-") || id.starts_with("PROC-"))
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub async fn audit_repos(
    sources: &[(String, std::path::PathBuf)],
    selected: &[SelectedRule],
    extra_notes: Vec<String>,
    model: Option<&str>,
    calibration_model: Option<&str>,
    mode: crate::ai_audit::ScanMode,
    thorough: bool,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    job: Option<(&crate::jobs::JobStore, &str)>,
    // The project's prior scan manifest, when incremental scanning is on. `None` = a full scan
    // (no cache, or the user forced it). Returned alongside the report is the FRESH manifest to
    // persist, so the next scan can go incremental.
    incremental_prior: Option<&crate::scan_cache::ScanManifest>,
    // OPT-IN deep compliance & security tier (#55). When true, AFTER the standard audit the
    // three deep lenses (SOC-2 gap analysis, deep security audit, threat model) run over each
    // repo on the same model; their output is attached to the report's `deep` field. ADVISORY
    // + model-inferred (#62). When false the behavior is unchanged — the most expensive tier
    // never runs by default.
    deep: bool,
    // Feature flag: whether the SOC-2 gap-analysis lens in the deep tier is enabled. When
    // false, ONLY the soc2 lens is skipped inside `run_deep_tier`; the other two lenses still
    // run. Pass `true` to restore full three-lens behaviour (the old default).
    soc2_enabled: bool,
    // Scan-type selector (Part C). `run_ai_review` gates the LLM architectural passes (the
    // semantic per-repo audit AND the deep tier) — false skips ALL model calls / tokens.
    // `run_deterministic` gates the always-on security floor (`audit_files`). Both default
    // true (today's behaviour) at the request layer; if BOTH arrive false the caller forces
    // them back to true (never a no-op scan). The scan-preview pass is gated by the caller
    // (`merge_scan_preview`) on the same `run_deterministic` flag.
    run_ai_review: bool,
    run_deterministic: bool,
    // Process-global cumulative usage ledger. When `Some`, the audit's `Llm` is built WITH it
    // attached, so every audit/calibration/deep-tier model call folds into the cockpit's
    // session-wide usage meter (in addition to the per-audit `UsageMeter` below). `None` in
    // tests / non-cockpit callers — recording is then simply skipped. Observability only.
    ledger: Option<std::sync::Arc<crate::usage_ledger::UsageLedger>>,
) -> (ScanReport, crate::scan_cache::ScanManifest) {
    // Fingerprint the rule selection so a change to it invalidates the incremental cache
    // (carried findings must always reflect the CURRENT rules). A prior manifest is only usable
    // if its rule fingerprint matches.
    let rules_fp = crate::scan_cache::rules_fingerprint(
        selected.iter().map(|r| (r.id.as_str(), r.repos.as_slice())),
    );
    let effective_prior = incremental_prior.filter(|m| m.matches_rules(&rules_fp));
    let mut manifest_builder =
        crate::scan_cache::ManifestBuilder::new().with_rules_fingerprint(rules_fp);
    let mut all_findings = Vec::new();
    let mut stacks = Vec::new();
    let mut files_total = 0usize;
    let mut repos_ok = Vec::new();
    let mut notes = extra_notes;
    // When the deep tier is on, the WHOLE file set per repo is captured here (the deep lenses
    // read the full repo, not just the incrementally-changed files) and run after the standard
    // audit completes. Empty / unused when `deep` is false.
    let mut deep_inputs: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let llm = match ledger {
        Some(l) => crate::llm::Llm::from_env_with_ledger(l),
        None => crate::llm::Llm::from_env(),
    };
    // Aggregates REAL usage across every repo's audit (passes + calibration) for the
    // actual-vs-estimated readout.
    let meter = crate::ai_audit::UsageMeter::default();

    // A re-run must start from a clean transcript: drop the prior audit's per-agent
    // prompts/output so the Agent-activity drawer shows THIS run, not the last one.
    if let Some((store, key)) = feedback {
        store.clear(key);
    }

    // ROUTE BY ENGINE, not by domain. A rule with a deterministic gate arm
    // (secrets / raw-SQL / secret-URL / path / secret-files) runs through real
    // deterministic code (`audit_files`) and must NEVER go to the LLM — fuzzy
    // keyword-matching a deterministic rule is the flood. Only the SEMANTIC rules
    // (no arm: layering, idempotency, authz, …) are handed to the model.
    //
    // SECOND, drop GOVERNANCE / PROCESS / ORCHESTRATION rules from the CODE audit.
    // ORCH-* / SPIRIT-* / PROC-* describe how the fleet and team OPERATE (track AI
    // spend, split author/reviewer agents, cite convention ids in commits, document
    // decisions). They are correct to ARM into a repo's governance, but auditing
    // application SOURCE against them is a category error ("this app doesn't track its
    // AI token budget"). The arm path still installs them; only the AI code-audit
    // prompt is filtered.
    //
    // THIRD, scope by REPO. The engine/governance filters above are global, but which
    // rules reach a given repo's LLM audit is decided PER REPO inside the loop, from each
    // SelectedRule's binding — so a multi-repo scan runs each repo against its own chosen
    // rules ∪ the project-level set, never the whole selection across the board.

    for (spec, dir) in sources {
        let spec = spec.trim();
        if spec.is_empty() {
            continue;
        }
        // The SEMANTIC (LLM-audited) rule set for THIS repo: rules bound to it (or
        // project-level), minus the deterministic-arm and governance/process families.
        let semantic: Vec<(String, String)> = selected
            .iter()
            .filter(|r| r.applies_to(spec))
            .filter(|r| camerata_gateway::lookup_arm(&r.id).is_none())
            .filter(|r| is_code_auditable_rule(&r.id))
            .map(|r| (r.id.clone(), r.directive.clone()))
            .collect();
        let dir = dir.clone();
        match tokio::task::spawn_blocking(move || read_local_repo_files(&dir))
            .await
            .unwrap_or_else(|e| Err(anyhow::anyhow!("scan task failed: {e}")))
        {
            Ok(ExtractedRepo {
                files,
                truncated,
                excluded_noise: _,
            }) => {
                files_total += files.len();
                // Capture the WHOLE file set for the deep tier (it reads the full repo, not the
                // incremental subset). Only when the deep tier is on, to avoid the clone otherwise.
                if deep && run_ai_review {
                    deep_inputs.push((spec.to_string(), files.clone()));
                }
                stacks.push(detect_stack(spec, &files));
                // Deterministic security floor (always-on, every repo): ENFORCED findings.
                // This is the non-deselectable critical floor, so it is NOT repo-scoped —
                // hardcoded secrets / raw-SQL concat are unsafe in any code repo. It is
                // token-free, so it ALWAYS runs over the whole tree (never incremental) —
                // the floor must never go stale.
                //
                // Scan-type selector (Part C): the floor is the DETERMINISTIC pass. When the
                // user deselects deterministic scans (`run_deterministic == false`) the floor
                // is skipped. It also emits PER-TOOL progress into the job (tool name `floor`,
                // running → done with its findings count) so the cockpit's deterministic
                // progress view has live state even in deterministic-only mode.
                let mut repo_findings = Vec::new();
                if run_deterministic {
                    if let Some((jstore, jid)) = job {
                        jstore.det_tool_running(jid, "floor");
                    }
                    let floor = audit_files(spec, &files);
                    if let Some((jstore, jid)) = job {
                        jstore.det_tool_done(jid, "floor", floor.len());
                        jstore.add_findings(jid, floor.clone());
                    }
                    repo_findings = floor;
                }

                // ── Incremental: only the AI audit (the token cost) is short-circuited. ──
                // Partition the repo's files into changed vs unchanged against the prior
                // manifest; AI-audit only the CHANGED set, and carry forward cached findings
                // for unchanged, still-present files. The AI audit still receives the WHOLE
                // file set as repo-map context (cheap symbol list) so cross-file rules keep
                // their architectural view even when only changed bodies are sent.
                let part = crate::scan_cache::partition(effective_prior, spec, &files);
                if incremental_prior.is_some() && effective_prior.is_none() {
                    notes.push(format!(
                        "{spec}: rule selection changed since last scan — full re-scan"
                    ));
                }
                if effective_prior.is_some() && part.unchanged_count > 0 {
                    notes.push(format!(
                        "{spec}: incremental — {} changed, {} reused from cache",
                        part.changed.len(),
                        part.unchanged_count
                    ));
                }
                // The AI findings that apply to this repo after the scan: carried-forward
                // (unchanged files) ∪ freshly audited (changed files).
                //
                // Scan-type selector (Part C): the ENTIRE AI review is gated on
                // `run_ai_review`. When it's false we make NO model calls — no carried
                // findings (those are AI results from a prior run), no per-repo audit, no
                // tokens. A deterministic-only scan therefore never touches the LLM.
                let mut ai_for_repo: Vec<Finding> = Vec::new();
                if run_ai_review {
                    ai_for_repo = part.carried.clone();
                    if let Some((jstore, jid)) = job {
                        // Carried findings are real results for this run — surface them in the
                        // live preview alongside the floor.
                        jstore.add_findings(jid, part.carried.clone());
                    }
                    // AI audit parameterized by THIS repo's SEMANTIC rules only: ADVISORY
                    // findings. Skipped when nothing changed (a fully-cached repo costs zero
                    // tokens).
                    if part.changed.is_empty() {
                        if effective_prior.is_some() && !files.is_empty() {
                            notes.push(format!(
                                "{spec}: no changes — AI audit skipped (fully cached)"
                            ));
                        }
                    } else {
                        match crate::ai_audit::audit_repo(
                            &llm,
                            spec,
                            &part.changed,
                            &semantic,
                            model,
                            calibration_model,
                            mode,
                            thorough,
                            feedback,
                            job,
                            Some(&meter),
                            Some(&files),
                        )
                        .await
                        {
                            Ok((ai_findings, _ai_rules)) => ai_for_repo.extend(ai_findings),
                            Err(e) => notes.push(format!("{spec}: AI audit skipped ({e})")),
                        }
                    }
                } else if !files.is_empty() {
                    notes.push(format!("{spec}: AI review deselected — deterministic only"));
                }

                // Record this repo into the fresh manifest: fingerprints of EVERY current file
                // (so next run can tell what changed) + the repo's AI findings (carried ∪ fresh).
                manifest_builder.record_repo(spec, &files, &ai_for_repo);

                repo_findings.extend(ai_for_repo);
                classify_repo_findings(&mut repo_findings, spec, &files);
                all_findings.extend(repo_findings);
                repos_ok.push(spec.to_string());
                if truncated {
                    notes.push(format!(
                        "{spec}: more than {HARD_CAP_FILES} files; truncated at the safety limit"
                    ));
                }
            }
            Err(e) => notes.push(format!("{spec}: audit failed ({e})")),
        }
    }

    // ── OPT-IN deep compliance & security tier (#55) ──────────────────────────────────
    // Runs AFTER the standard audit, only when requested. Each repo gets the three lenses;
    // results are merged into one tier-level report and attached. Spend folds into the same
    // meter (so the actual-vs-estimated readout includes the deep tier). It is the most
    // expensive tier — that is why it is opt-in and never default (#62).
    // Deep tier is three LLM lenses — part of the AI review. When AI review is deselected it
    // never runs, even if `deep` was somehow set (the UI hides the deep toggle in that mode).
    let deep_report = if run_ai_review && deep && !deep_inputs.is_empty() {
        let mut per_repo = Vec::new();
        for (spec, files) in &deep_inputs {
            let dr = crate::ai_audit::run_deep_tier(
                &llm,
                spec,
                files,
                model,
                mode,
                feedback,
                Some(&meter),
                soc2_enabled,
            )
            .await;
            per_repo.push(dr);
        }
        Some(merge_deep_reports(per_repo))
    } else {
        None
    };

    let mut report = build_report(repos_ok, stacks, files_total, all_findings);
    report.actual_usage = Some(meter.snapshot());
    report.deep = deep_report;
    if !notes.is_empty() {
        report.message = Some(notes.join(" · "));
    }
    (report, manifest_builder.finish())
}

/// Merge the per-repo deep-tier reports into ONE tier-level [`crate::ai_audit::DeepReport`].
/// The three lenses keep their identity across repos: every repo's SOC-2 gaps fold into the
/// single SOC-2 lens result, every repo's security findings into the security lens, etc., so
/// the consumer sees three lens results (not three-per-repo). The advisory envelope + honesty
/// disclaimer are preserved. Lens errors are concatenated so a repo that failed a lens is
/// visible rather than silently dropped.
fn merge_deep_reports(
    reports: Vec<crate::ai_audit::DeepReport>,
) -> crate::ai_audit::DeepReport {
    use crate::ai_audit::{DeepLens, DeepLensResult, DeepReport, DEEP_ADVISORY_DISCLAIMER};
    let mut soc2 = DeepLensResult::merged_empty(DeepLens::Soc2Gap);
    let mut security = DeepLensResult::merged_empty(DeepLens::DeepSecurity);
    let mut threat = DeepLensResult::merged_empty(DeepLens::ThreatModel);
    let mut summaries: (Vec<String>, Vec<String>, Vec<String>) =
        (Vec::new(), Vec::new(), Vec::new());
    let mut errors: (Vec<String>, Vec<String>, Vec<String>) = (Vec::new(), Vec::new(), Vec::new());
    for r in reports {
        for lens in r.lenses {
            match lens.lens.as_str() {
                "soc2-gap" => {
                    soc2.soc2_gaps.extend(lens.soc2_gaps);
                    if !lens.summary.is_empty() {
                        summaries.0.push(lens.summary);
                    }
                    if let Some(e) = lens.error {
                        errors.0.push(e);
                    }
                }
                "deep-security" => {
                    security.security_findings.extend(lens.security_findings);
                    if !lens.summary.is_empty() {
                        summaries.1.push(lens.summary);
                    }
                    if let Some(e) = lens.error {
                        errors.1.push(e);
                    }
                }
                "threat-model" => {
                    threat.threats.extend(lens.threats);
                    if !lens.summary.is_empty() {
                        summaries.2.push(lens.summary);
                    }
                    if let Some(e) = lens.error {
                        errors.2.push(e);
                    }
                }
                _ => {}
            }
        }
    }
    soc2.summary = summaries.0.join("\n\n");
    security.summary = summaries.1.join("\n\n");
    threat.summary = summaries.2.join("\n\n");
    soc2.error = (!errors.0.is_empty()).then(|| errors.0.join(" · "));
    security.error = (!errors.1.is_empty()).then(|| errors.1.join(" · "));
    threat.error = (!errors.2.is_empty()).then(|| errors.2.join(" · "));
    DeepReport {
        lenses: vec![soc2, security, threat],
        advisory: true,
        disclaimer: DEEP_ADVISORY_DISCLAIMER.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(id: &str, repos: &[&str]) -> SelectedRule {
        SelectedRule {
            id: id.to_string(),
            directive: format!("directive for {id}"),
            repos: repos.iter().map(|r| r.to_string()).collect(),
        }
    }

    #[test]
    fn selected_rule_project_level_applies_to_every_repo() {
        let r = sel("ARCH-1", &[]);
        assert!(r.applies_to("acme/api"));
        assert!(r.applies_to("acme/ui"));
    }

    #[test]
    fn selected_rule_repo_scoped_applies_only_to_its_repos() {
        let r = sel("RUST-DIOXUS-2", &["acme/ui"]);
        assert!(r.applies_to("acme/ui"));
        assert!(!r.applies_to("acme/api"));
    }

    #[test]
    fn per_repo_semantic_set_is_union_of_project_level_and_repo_rules() {
        // Mirrors the per-repo filter inside `audit_repos`: each repo sees project-level
        // rules (empty `repos`) plus the rules bound to it, and nothing bound to a sibling.
        let selected = vec![
            sel("ARCH-1", &[]),                 // project-level → both repos
            sel("RUST-DIOXUS-2", &["acme/ui"]), // ui only
            sel("SQL-1", &["acme/api"]),        // api only
        ];

        let for_repo = |spec: &str| -> Vec<String> {
            selected
                .iter()
                .filter(|r| r.applies_to(spec))
                .map(|r| r.id.clone())
                .collect()
        };

        assert_eq!(for_repo("acme/ui"), vec!["ARCH-1", "RUST-DIOXUS-2"]);
        assert_eq!(for_repo("acme/api"), vec!["ARCH-1", "SQL-1"]);
    }

    #[test]
    fn read_local_pulls_code_files_and_prunes_noise() {
        use std::fs;

        // A real on-disk working tree: a .git marker, a code file, a non-code file, and a
        // noise dir. The reader keeps only the auditable code and never descends node_modules.
        let base =
            std::env::temp_dir().join(format!("camerata_localreader_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join(".git")).unwrap(); // marks it a local clone
        fs::create_dir_all(base.join("src")).unwrap();
        fs::create_dir_all(base.join("node_modules/x")).unwrap();
        fs::write(base.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(base.join("README.md"), "# readme").unwrap(); // not a code ext -> skipped
        fs::write(base.join("node_modules/x/index.js"), "noise").unwrap(); // noise dir -> skipped

        let extracted = read_local_repo_files(&base).unwrap();
        let files = extracted.files;
        let _ = fs::remove_dir_all(&base);

        assert!(!extracted.truncated);
        assert_eq!(files.len(), 1, "only the .rs file is auditable: {files:?}");
        assert_eq!(files[0].0, "src/main.rs");
        assert_eq!(files[0].1, "fn main() {}\n");
    }

    #[test]
    fn domains_for_stack_does_not_map_sqlx_to_seaorm() {
        // #52: a sqlx repo must NOT be proposed SeaORM/entity rules — only the generic SQL +
        // migration-hygiene domains. SeaORM is the only data layer that maps to `rust:seaorm`.
        let sqlx = RepoStack {
            repo: "me/budget".into(),
            languages: vec!["Rust".into()],
            frameworks: vec!["sqlx".into()],
        };
        let d = domains_for_stack(&sqlx);
        assert!(
            !d.contains(&"rust:seaorm".to_string()),
            "sqlx must not get SeaORM rules: {d:?}"
        );
        assert!(
            d.contains(&"sql".to_string()),
            "sqlx still gets generic SQL rules: {d:?}"
        );
        assert!(
            d.contains(&"ci-cd".to_string()),
            "sqlx still gets migration-hygiene rules: {d:?}"
        );

        // SeaORM DOES map to the SeaORM-specific domain.
        let seaorm = RepoStack {
            repo: "me/api".into(),
            languages: vec!["Rust".into()],
            frameworks: vec!["SeaORM".into()],
        };
        assert!(domains_for_stack(&seaorm).contains(&"rust:seaorm".to_string()));
    }

    #[test]
    fn domains_for_stack_includes_parent_of_child_domain() {
        // Next.js => javascript:next, and the parent `javascript` must come along.
        let s = RepoStack {
            repo: "me/web".into(),
            languages: vec!["JavaScript".into()],
            frameworks: vec!["Next.js".into()],
        };
        let domains = domains_for_stack(&s);
        assert!(domains.contains(&"javascript:next".to_string()));
        assert!(
            domains.contains(&"javascript".to_string()),
            "child domain must pull in its parent: {domains:?}"
        );
    }

    #[test]
    fn domains_for_stack_maps_ts_react_express_to_javascript_family() {
        // A TypeScript + React + Express repo (e.g. agora-mono) should auto-suggest the
        // javascript:typescript / :react / :express domains, and `javascript` via the
        // child→parent expansion — not just generic fullstack/api-layer/ui.
        let s = RepoStack {
            repo: "acme/app".into(),
            languages: vec!["TypeScript".into()],
            frameworks: vec!["React".into(), "Express".into()],
        };
        let domains = domains_for_stack(&s);
        for want in [
            "javascript",
            "javascript:typescript",
            "javascript:react",
            "javascript:express",
        ] {
            assert!(
                domains.contains(&want.to_string()),
                "expected {want} in {domains:?}"
            );
        }
    }

    #[test]
    fn detect_stack_recognizes_python_and_its_frameworks() {
        // #48: a Python repo with a FastAPI + SQLAlchemy + Pydantic manifest is detected
        // as the Python language plus those three frameworks (Pydantic / SQLAlchemy were
        // previously undetected).
        let files = vec![
            ("app/main.py".to_string(), "def main(): ...\n".to_string()),
            (
                "requirements.txt".to_string(),
                "fastapi==0.110\nsqlalchemy==2.0\npydantic==2.6\n".to_string(),
            ),
        ];
        let stack = detect_stack("me/svc", &files);
        assert!(
            stack.languages.contains(&"Python".to_string()),
            ".py maps to Python: {stack:?}"
        );
        for fw in ["FastAPI", "SQLAlchemy", "Pydantic"] {
            assert!(
                stack.frameworks.contains(&fw.to_string()),
                "expected {fw} detected: {stack:?}"
            );
        }
    }

    #[test]
    fn domains_for_stack_maps_python_fastapi_to_python_family() {
        // #48: a Python + FastAPI + SQLAlchemy repo must surface the `python` baseline,
        // the `python:fastapi` child domain, and the cross-language api-layer / sql
        // domains — not just the generic api-layer fallback Python used to hit.
        let s = RepoStack {
            repo: "me/svc".into(),
            languages: vec!["Python".into()],
            frameworks: vec!["FastAPI".into(), "SQLAlchemy".into()],
        };
        let domains = domains_for_stack(&s);
        for want in ["python", "python:fastapi", "api-layer", "sql"] {
            assert!(
                domains.contains(&want.to_string()),
                "expected {want} in {domains:?}"
            );
        }
        // The child→parent expansion must pull `python` in from `python:fastapi`.
        assert!(
            domains.contains(&"python".to_string()),
            "child domain must pull in its parent: {domains:?}"
        );
        // A backend api-layer always implies the permissions rules.
        assert!(domains.contains(&"permissions".to_string()), "{domains:?}");
    }

    #[test]
    fn code_ext_filter() {
        assert!(has_code_ext("src/main.rs"));
        assert!(has_code_ext("a/b/config.YAML"));
        assert!(!has_code_ext("logo.png"));
        assert!(!has_code_ext("Dockerfile"));
        assert!(!has_code_ext("README"));
    }

    #[test]
    fn noise_paths_are_pruned() {
        let none: &[String] = &[];
        // Build / dep / cache dirs at any depth.
        assert!(is_noise_path("node_modules/react/index.js", none));
        assert!(is_noise_path("apps/web/node_modules/x/y.ts", none));
        assert!(is_noise_path(".turbo/cache/abc-manifest.json", none));
        assert!(is_noise_path("target/debug/build/x.rs", none));
        assert!(is_noise_path("apps/ui/.next/server/page.js", none));
        // Lockfiles + minified + maps.
        assert!(is_noise_path("package-lock.json", none));
        assert!(is_noise_path("apps/api/Cargo.lock", none));
        assert!(is_noise_path("public/app.min.js", none));
        assert!(is_noise_path("dist/bundle.js.map", none));
        // Generated / codegen output.
        assert!(is_noise_path("src/api/client.gen.ts", none));
        assert!(is_noise_path("proto/service.pb.go", none));
        assert!(is_noise_path("src/__generated__/schema.ts", none));
        assert!(is_noise_path("components/__snapshots__/Btn.test.tsx", none));
        assert!(is_noise_path("models/user.freezed.dart", none));
        // Real source survives.
        assert!(!is_noise_path("crates/api/src/handlers.rs", none));
        assert!(!is_noise_path("apps/ui/src/page.tsx", none));
        assert!(!is_noise_path("migrations/001_init.sql", none));
        // Project-specific extra exclusions (a dir not in the default set).
        let extra = vec!["fixtures".to_string()];
        assert!(is_noise_path("test/fixtures/big.ts", &extra));
        assert!(!is_noise_path("test/fixtures/big.ts", none));
    }

    #[test]
    fn audit_flags_a_hardcoded_secret_with_line_severity_and_repo() {
        // A GitHub PAT literal is exactly what SEC-NO-HARDCODED-SECRETS-1 denies.
        let content = "let cfg = load();\nconst TOKEN = \"ghp_0123456789012345678901234567890123456\";\nok();";
        let findings = audit_content("me/api", "src/config.rs", content);
        assert_eq!(findings.len(), 1, "one secret -> one finding: {findings:?}");
        let f = &findings[0];
        assert_eq!(f.repo, "me/api", "finding tagged with its repo");
        assert_eq!(f.line, 2, "finding on the right line");
        assert_eq!(f.rule_id, "SEC-NO-HARDCODED-SECRETS-1");
        assert_eq!(
            f.severity, "critical",
            "exploitable security defects rank critical"
        );
        assert!(f.path == "src/config.rs");
    }

    #[test]
    fn audit_is_clean_on_clean_content() {
        let content = "fn add(a: i32, b: i32) -> i32 { a + b }\n// nothing to see here";
        assert!(audit_content("me/api", "src/math.rs", content).is_empty());
    }

    #[test]
    fn audit_floor_flags_python_secret_and_fstring_sql() {
        // #48 acceptance: the language-agnostic deterministic floor must fire on Python
        // idioms — a hardcoded secret and a raw-SQL-via-f-string — on a .py fixture.
        let secret_py = "import os\nTOKEN = \"ghp_0123456789012345678901234567890123456\"\n";
        let sec = audit_content("me/svc", "app/config.py", secret_py);
        assert!(
            sec.iter()
                .any(|f| f.rule_id == "SEC-NO-HARDCODED-SECRETS-1"),
            "Python hardcoded secret must be flagged: {sec:?}"
        );

        let sql_py = "def get(uid):\n    cur.execute(f\"SELECT * FROM users WHERE id = {uid}\")\n";
        let sql = audit_content("me/svc", "app/db.py", sql_py);
        assert!(
            sql.iter().any(|f| f.rule_id == "SEC-NO-RAW-SQL-CONCAT-1"),
            "Python f-string SQL must be flagged: {sql:?}"
        );
    }

    #[test]
    fn propose_rules_classifies_by_scope_and_placement() {
        let content =
            "const T = \"ghp_0123456789012345678901234567890123456\";\nconst U = \"ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\";";
        let findings = audit_content("me/api", "a.rs", content);
        // Single repo: content rules (repo-local) + a process rule, no cross-repo.
        let single = propose_rules(&findings, &["me/api".to_string()]);
        let secrets = single
            .iter()
            .find(|r| r.id == "SEC-NO-HARDCODED-SECRETS-1")
            .unwrap();
        assert_eq!(secrets.finding_count, findings.len());
        assert_eq!(secrets.scope, "repo-local");
        assert_eq!(secrets.enforcement_point, "content");
        assert_eq!(
            secrets.repos,
            vec!["me/api".to_string()],
            "universal -> all scanned repos"
        );
        assert!(secrets.placement.contains("every repo"));
        assert!(single.iter().any(|r| r.scope == "process"));
        assert!(
            !single.iter().any(|r| r.scope == "cross-repo"),
            "no cross-repo rule for a single repo"
        );

        // Multi-repo: a cross-repo contract rule appears, spanning the set.
        let multi = propose_rules(&findings, &["me/api".to_string(), "me/web".to_string()]);
        let xrepo = multi
            .iter()
            .find(|r| r.scope == "cross-repo")
            .expect("multi-repo set proposes a cross-repo rule");
        assert_eq!(xrepo.enforcement_point, "integration");
        assert_eq!(xrepo.repos.len(), 2, "spans both repos");
    }

    #[test]
    fn build_report_aggregates_findings_across_repos() {
        // Two repos: a secret in one, clean in the other -> one finding, tagged.
        let mut findings = audit_files(
            "me/api",
            &[(
                "a.rs".to_string(),
                "const T = \"ghp_0123456789012345678901234567890123456\";".to_string(),
            )],
        );
        findings.extend(audit_files(
            "me/web",
            &[(
                "b.tsx".to_string(),
                "export const ok = () => 1;".to_string(),
            )],
        ));
        let report = build_report(
            vec!["me/api".to_string(), "me/web".to_string()],
            vec![],
            2,
            findings,
        );
        assert_eq!(report.repos.len(), 2);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].repo, "me/api");
        assert!(!report.gated);
    }

    #[test]
    fn detect_stack_finds_languages_and_frameworks() {
        let files = vec![
            ("src/App.tsx".to_string(), "export default App".to_string()),
            (
                "package.json".to_string(),
                r#"{ "dependencies": { "react": "18", "@reduxjs/toolkit": "2", "express": "4" } }"#
                    .to_string(),
            ),
            ("api/Program.cs".to_string(), "class Program {}".to_string()),
            (
                "api/Api.csproj".to_string(),
                "<Project><PackageReference Include=\"Microsoft.AspNetCore.App\"/></Project>"
                    .to_string(),
            ),
        ];
        let stack = detect_stack("acme/app", &files);
        assert!(stack.languages.contains(&"TypeScript".to_string()));
        assert!(stack.languages.contains(&"C#".to_string()));
        assert!(stack.frameworks.contains(&"React".to_string()));
        assert!(stack.frameworks.contains(&"Redux".to_string()));
        assert!(stack.frameworks.contains(&"Express".to_string()));
        assert!(stack.frameworks.contains(&".NET".to_string()));
        assert!(stack.frameworks.contains(&"ASP.NET".to_string()));
    }

    #[test]
    fn detect_stack_finds_terraform_and_github_actions() {
        let files = vec![
            (
                "infra/main.tf".to_string(),
                "resource \"aws_s3_bucket\" \"b\" {}".to_string(),
            ),
            (
                ".github/workflows/ci.yml".to_string(),
                "name: CI\non: [push]\njobs: {}".to_string(),
            ),
        ];
        let stack = detect_stack("acme/infra", &files);
        assert!(stack.frameworks.contains(&"Terraform".to_string()));
        assert!(stack.frameworks.contains(&"GitHub Actions".to_string()));
        // ...and those map to the iac / ci-cd corpus domains.
        let domains = domains_for_stack(&stack);
        assert!(domains.contains(&"iac".to_string()), "{domains:?}");
        assert!(domains.contains(&"ci-cd".to_string()), "{domains:?}");
    }

    #[test]
    fn detect_stack_finds_non_github_ci_and_non_terraform_iac() {
        // CI/IaC tooling beyond GitHub Actions + Terraform must still map to ci-cd / iac.
        let cases: &[(&str, &str, &str)] = &[
            (".gitlab-ci.yml", "stages: [test]", "GitLab CI"),
            (".circleci/config.yml", "version: 2.1", "CircleCI"),
            ("azure-pipelines.yml", "trigger: [main]", "Azure Pipelines"),
            ("Jenkinsfile", "pipeline { agent any }", "Jenkins"),
            (
                "infra/main.bicep",
                "resource sa 'Microsoft.Storage'",
                "Bicep",
            ),
            (
                "live/terragrunt.hcl",
                "include { path = \"x\" }",
                "Terragrunt",
            ),
            (
                "infra/Pulumi.yaml",
                "name: my-stack\nruntime: nodejs",
                "Pulumi",
            ),
            (
                "cfn/stack.yaml",
                "AWSTemplateFormatVersion: '2010-09-09'",
                "CloudFormation",
            ),
        ];
        for (path, content, fw) in cases {
            // has_code_ext must keep the file so detection can see it.
            assert!(has_code_ext(path), "{path} should be extracted");
            let stack = detect_stack("acme/infra", &[(path.to_string(), content.to_string())]);
            assert!(
                stack.frameworks.contains(&fw.to_string()),
                "{path} -> {fw}: {stack:?}"
            );
            let domains = domains_for_stack(&stack);
            let expect = if ["Jenkins", "GitLab CI", "CircleCI", "Azure Pipelines"].contains(fw) {
                "ci-cd"
            } else {
                "iac"
            };
            assert!(
                domains.contains(&expect.to_string()),
                "{path} -> {expect}: {domains:?}"
            );
        }
    }

    #[test]
    fn audit_catches_the_testbed_tier1_plants() {
        // The three Tier-1 plants from budget-tracker-testrepo, in their real shapes.
        let sql = "        let sql = format!(\n\
            \x20            \"SELECT category_id, SUM(amount) AS spent \\\n\
            \x20             FROM transactions \\\n\
            \x20             WHERE user_id = '{user_id}' \\\n\
            \x20               AND EXTRACT(YEAR FROM date) = {year}\",\n\
            \x20            user_id = user_id.value(),\n        );";
        let sql_findings = audit_content("me/api", "transactions.rs", sql);
        assert!(
            sql_findings
                .iter()
                .any(|f| f.rule_id == "SEC-NO-RAW-SQL-CONCAT-1"),
            "multi-line named-arg SQL format! must be caught"
        );

        let key = "const FALLBACK_FINNHUB_KEY: &str = \"c8r9v2aad3i9q1m4f7g0bv8s5p2qk1n7\";";
        let key_findings = audit_content("me/api", "finnhub.rs", key);
        assert!(
            key_findings
                .iter()
                .any(|f| f.rule_id == "SEC-NO-HARDCODED-SECRETS-1"),
            "bare provider-agnostic key on a *_KEY const must be caught"
        );

        let url = "        format!(\"{base}?symbol={symbol}&token={token}\")";
        let url_findings = audit_content("me/api", "finnhub.rs", url);
        assert!(
            url_findings
                .iter()
                .any(|f| f.rule_id == "ARCH-NO-SECRETS-IN-URL-1"),
            "templated URL with a token param must be caught"
        );
    }

    #[test]
    fn classify_marks_baseline_inline_and_reasonless() {
        use crate::suppression::{fingerprint, Baseline, BaselineEntry};
        let snippet = "let token = \"ghp_x\";";
        let baseline = Baseline {
            entries: vec![BaselineEntry {
                rule_id: "SEC-NO-HARDCODED-SECRETS-1".into(),
                path: "a.rs".into(),
                fingerprint: fingerprint("SEC-NO-HARDCODED-SECRETS-1", snippet),
                reason: "pre-existing".into(),
                accepted_by: "z".into(),
                accepted_at: "t".into(),
                kind: "baseline".into(),
                ticket: None,
            }],
        };
        let files = vec![
            (
                ".camerata/baseline.json".to_string(),
                serde_json::to_string(&baseline).unwrap(),
            ),
            (
                "b.rs".to_string(),
                "danger(); // camerata:allow SEC-NO-HARDCODED-SECRETS-1 -- vetted\n\
                 bare(); // camerata:allow SEC-NO-RAW-SQL-CONCAT-1\n"
                    .to_string(),
            ),
        ];
        let mk = |path: &str, line: usize, rule: &str, snip: &str| Finding {
            repo: "me/api".into(),
            path: path.into(),
            line,
            rule_id: rule.into(),
            severity: "high".into(),
            snippet: snip.into(),
            detail: "d".into(),
            status: "active".into(),
            also_matches: Vec::new(),
            preview: false,
            preview_tool: None,
            in_test: false,
            needs_review: false,
        };
        let mut findings = vec![
            mk("a.rs", 5, "SEC-NO-HARDCODED-SECRETS-1", snippet), // baselined
            mk("b.rs", 1, "SEC-NO-HARDCODED-SECRETS-1", "danger()"), // inline-waived
        ];
        classify_repo_findings(&mut findings, "me/api", &files);
        assert_eq!(findings[0].status, "suppressed-baseline");
        assert_eq!(findings[1].status, "suppressed-inline");
        // The reason-less waiver on b.rs:2 surfaced as its own violation.
        assert!(findings
            .iter()
            .any(|f| f.rule_id == "CAM-WAIVER-NEEDS-REASON" && f.status == "active"));
    }

    #[test]
    fn tech_debt_body_groups_by_repo() {
        let findings = vec![
            Finding {
                repo: "me/api".into(),
                path: "a.rs".into(),
                line: 3,
                rule_id: "SEC-NO-HARDCODED-SECRETS-1".into(),
                severity: "high".into(),
                snippet: "x".into(),
                detail: "d".into(),
                status: "active".into(),
                also_matches: Vec::new(),
                preview: false,
                preview_tool: None,
                in_test: false,
                needs_review: false,
            },
            Finding {
                repo: "me/web".into(),
                path: "b.tsx".into(),
                line: 7,
                rule_id: "ARCH-NO-SECRETS-IN-URL-1".into(),
                severity: "high".into(),
                snippet: "y".into(),
                detail: "d".into(),
                status: "active".into(),
                also_matches: Vec::new(),
                preview: false,
                preview_tool: None,
                in_test: false,
                needs_review: false,
            },
        ];
        let body = tech_debt_issue_body(&findings);
        assert!(body.contains("### me/api"));
        assert!(body.contains("### me/web"));
        assert!(body.contains("a.rs:3"));
        assert!(body.contains("2 finding"));
    }

    #[test]
    fn gated_report_has_no_findings_and_a_message() {
        let r = ScanReport::gated(&["me/api".to_string(), "me/web".to_string()]);
        assert!(r.gated);
        assert!(r.findings.is_empty());
        assert_eq!(r.repos.len(), 2);
        assert!(r.message.unwrap().contains("CAMERATA_GITHUB_TOKEN"));
    }

    // ── Regression: repo↔issue boundary (issue #41) ─────────────────────────────

    /// Helper: build a minimal Finding for a given repo.
    fn finding_for(repo: &str, path: &str, line: usize, rule_id: &str) -> Finding {
        Finding {
            repo: repo.to_string(),
            path: path.to_string(),
            line,
            rule_id: rule_id.to_string(),
            severity: "high".to_string(),
            snippet: "s".to_string(),
            detail: "detail text".to_string(),
            status: "active".to_string(),
            also_matches: Vec::new(),
            preview: false,
            preview_tool: None,
            in_test: false,
            needs_review: false,
        }
    }

    /// The UI calls create_ticket(repo, group) once per repo, where `group` is
    /// already filtered to that repo's findings. This test locks that boundary:
    /// when `tech_debt_issue_body` is called with findings from repo A only, the
    /// produced body must contain repo A's paths and must NOT contain any path
    /// from repo B (even if repo B findings exist in the broader selection).
    #[test]
    fn tech_debt_ticket_body_isolates_repo_findings() {
        let api_findings = vec![
            finding_for("me/api", "src/config.rs", 12, "SEC-NO-HARDCODED-SECRETS-1"),
            finding_for("me/api", "src/db.rs", 55, "SEC-NO-RAW-SQL-CONCAT-1"),
        ];
        let web_findings = vec![finding_for(
            "me/web",
            "pages/index.tsx",
            3,
            "ARCH-NO-SECRETS-IN-URL-1",
        )];

        // Simulate the per-repo issue bodies the UI creates (one call per repo).
        let api_body = tech_debt_issue_body(&api_findings);
        let web_body = tech_debt_issue_body(&web_findings);

        // API issue: contains only API paths.
        assert!(
            api_body.contains("src/config.rs"),
            "api body must contain its own path"
        );
        assert!(
            api_body.contains("src/db.rs"),
            "api body must contain its own path"
        );
        assert!(
            !api_body.contains("pages/index.tsx"),
            "api body must NOT contain web repo path: {api_body}"
        );

        // Web issue: contains only web paths.
        assert!(
            web_body.contains("pages/index.tsx"),
            "web body must contain its own path"
        );
        assert!(
            !web_body.contains("src/config.rs"),
            "web body must NOT contain api repo path: {web_body}"
        );
        assert!(
            !web_body.contains("src/db.rs"),
            "web body must NOT contain api repo path: {web_body}"
        );

        // Each body also embeds its repo's CSV block.
        assert!(
            api_body.contains("```csv"),
            "api body must embed a csv block"
        );
        assert!(
            web_body.contains("```csv"),
            "web body must embed a csv block"
        );
        assert!(
            api_body.contains("SEC-NO-HARDCODED-SECRETS-1"),
            "csv must include rule_id"
        );
        assert!(
            web_body.contains("ARCH-NO-SECRETS-IN-URL-1"),
            "csv must include rule_id"
        );
    }

    // ── Per-repo CSV (issue #41) ──────────────────────────────────────────────────

    #[test]
    fn tech_debt_csv_header_and_basic_row() {
        let findings = vec![finding_for(
            "me/api",
            "src/main.rs",
            10,
            "SEC-NO-HARDCODED-SECRETS-1",
        )];
        let csv = tech_debt_csv(&findings);
        let mut lines = csv.lines();
        assert_eq!(lines.next().unwrap(), "rule_id,severity,path,line,detail");
        let data_row = lines.next().expect("expected a data row");
        assert!(data_row.contains("SEC-NO-HARDCODED-SECRETS-1"));
        assert!(data_row.contains("src/main.rs"));
        assert!(data_row.contains("10"));
        assert!(data_row.contains("high"));
    }

    #[test]
    fn tech_debt_csv_empty_findings_produces_header_only() {
        let csv = tech_debt_csv(&[]);
        assert_eq!(csv, "rule_id,severity,path,line,detail\n");
    }

    #[test]
    fn csv_escape_plain_value_is_unchanged() {
        assert_eq!(csv_escape("hello"), "hello");
        assert_eq!(
            csv_escape("SEC-NO-HARDCODED-SECRETS-1"),
            "SEC-NO-HARDCODED-SECRETS-1"
        );
    }

    #[test]
    fn csv_escape_value_with_comma_is_quoted() {
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
    }

    #[test]
    fn csv_escape_value_with_internal_double_quote_doubles_it() {
        // RFC 4180: a double-quote inside a quoted field is escaped by doubling it.
        assert_eq!(csv_escape("say \"hello\""), "\"say \"\"hello\"\"\"");
    }

    #[test]
    fn csv_escape_value_with_newline_is_quoted() {
        assert_eq!(csv_escape("line1\nline2"), "\"line1\nline2\"");
    }

    #[test]
    fn tech_debt_csv_escapes_special_fields_correctly() {
        let f = Finding {
            repo: "me/api".into(),
            path: "src/tricky,path.rs".into(),
            line: 1,
            rule_id: "RULE-1".into(),
            severity: "high".into(),
            snippet: "s".into(),
            detail: "contains a \"quoted\" word and a comma, here".into(),
            status: "active".into(),
            also_matches: Vec::new(),
            preview: false,
            preview_tool: None,
            in_test: false,
            needs_review: false,
        };
        let csv = tech_debt_csv(&[f]);
        let data_row = csv.lines().nth(1).expect("expected data row");
        // The path field contains a comma so it must be wrapped in quotes.
        assert!(
            data_row.contains("\"src/tricky,path.rs\""),
            "comma in path must be quoted: {data_row}"
        );
        // The detail contains both quotes and a comma — the whole field is quoted and
        // internal quotes are doubled.
        assert!(
            data_row.contains("\"contains a \"\"quoted\"\" word and a comma, here\""),
            "detail with comma+quote must be fully escaped: {data_row}"
        );
    }

    #[test]
    fn tech_debt_issue_body_embeds_csv_block_per_repo() {
        let findings = vec![finding_for(
            "me/api",
            "src/config.rs",
            5,
            "SEC-NO-HARDCODED-SECRETS-1",
        )];
        let body = tech_debt_issue_body(&findings);
        // The body must contain a fenced csv block.
        assert!(
            body.contains("```csv\n"),
            "body must open a csv fence: {body}"
        );
        assert!(
            body.contains("```"),
            "body must close the csv fence: {body}"
        );
        // The CSV block contains the header row.
        assert!(
            body.contains("rule_id,severity,path,line,detail"),
            "csv header must appear in body: {body}"
        );
        // The CSV block contains the data row.
        assert!(
            body.contains("SEC-NO-HARDCODED-SECRETS-1"),
            "csv data must appear in body: {body}"
        );
        assert!(
            body.contains("src/config.rs"),
            "path must appear in csv block: {body}"
        );
    }

    // ── Greenfield scaffold tests ─────────────────────────────────────────────

    /// Build a minimal ArmRule for test use.
    fn arm_rule(id: &str, enf: &str) -> crate::arm::ArmRule {
        crate::arm::ArmRule {
            id: id.to_string(),
            title: format!("Title {id}"),
            directive: format!("Do {id}."),
            option: None,
            enforcement: enf.to_string(),
            scope: "repo-local".to_string(),
            conformance: None,
            repos: vec!["me/new-repo".to_string()],
        }
    }

    /// A unique temp dir that does NOT yet exist (scaffold must create it).
    fn scaffold_dest(suffix: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "camerata-greenfield-{}-{}-{}",
            std::process::id(),
            suffix,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn greenfield_scaffold_emits_governance_files_and_initial_commit() {
        let dest = scaffold_dest("emit");
        let rules = [
            arm_rule("SEC-NO-HARDCODED-SECRETS-1", "mechanical"),
            arm_rule("RUST-DOMAIN-6", "structured"),
        ];
        let refs: Vec<&crate::arm::ArmRule> = rules.iter().collect();

        let result = scaffold_greenfield_blocking(&dest, &refs, &[], "me/new-repo")
            .expect("scaffold must succeed on a fresh directory");

        // The path must exist and be a git repo.
        assert!(dest.exists(), "dest must exist after scaffold");
        assert!(dest.join(".git").exists(), "must be a git repo");

        // The governance files must be present on disk.
        assert!(
            dest.join("CONVENTIONS.md").exists(),
            "CONVENTIONS.md must be written"
        );
        assert!(
            dest.join(".camerata").join("rules.json").exists(),
            ".camerata/rules.json must be written"
        );

        // The commit sha must be non-empty.
        assert!(
            !result.commit_sha.is_empty(),
            "commit sha must be non-empty"
        );

        // The files_written list must include what arm emitted.
        assert!(
            result.files_written.contains(&"CONVENTIONS.md".to_string()),
            "CONVENTIONS.md in files_written"
        );
        assert!(
            result.files_written.contains(&".camerata/rules.json".to_string()),
            ".camerata/rules.json in files_written"
        );

        // The CI workflow must be emitted for mechanical rules.
        assert!(
            result
                .files_written
                .iter()
                .any(|f| f.ends_with("camerata-governance.yml")),
            "CI workflow must be emitted for mechanical rules"
        );

        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn greenfield_scaffold_prose_rule_emits_agents_md() {
        let dest = scaffold_dest("prose");
        let rules = [arm_rule("SPIRIT-COMMIT-1", "prose")];
        let refs: Vec<&crate::arm::ArmRule> = rules.iter().collect();

        let result = scaffold_greenfield_blocking(&dest, &refs, &[], "me/prose-repo")
            .expect("scaffold must succeed");

        assert!(
            dest.join("AGENTS.md").exists(),
            "AGENTS.md must be written for prose rules"
        );
        assert!(
            result.files_written.contains(&"AGENTS.md".to_string()),
            "AGENTS.md in files_written"
        );

        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn greenfield_scaffold_refuses_existing_directory() {
        // Create the dir before calling scaffold — it must refuse.
        let dest = scaffold_dest("collision");
        std::fs::create_dir_all(&dest).unwrap();

        let rules: Vec<&crate::arm::ArmRule> = vec![];
        let err = scaffold_greenfield_blocking(&dest, &rules, &[], "me/collision-repo")
            .expect_err("must refuse to clobber an existing directory");
        assert!(
            err.to_string().contains("already exists"),
            "error must mention existing directory: {err}"
        );

        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn greenfield_scaffold_ruleset_is_baked_into_conventions() {
        let dest = scaffold_dest("ruleset-content");
        let rules = [arm_rule("ARCH-NO-SECRETS-IN-URL-1", "mechanical")];
        let refs: Vec<&crate::arm::ArmRule> = rules.iter().collect();

        scaffold_greenfield_blocking(&dest, &refs, &[], "me/ruleset-repo")
            .expect("scaffold must succeed");

        // The rule id must appear in CONVENTIONS.md (the structured/mechanical file).
        let conv = std::fs::read_to_string(dest.join("CONVENTIONS.md")).unwrap();
        assert!(
            conv.contains("ARCH-NO-SECRETS-IN-URL-1"),
            "CONVENTIONS.md must contain the rule id"
        );

        // The rule config must be in .camerata/rules.json.
        let gate = std::fs::read_to_string(dest.join(".camerata").join("rules.json")).unwrap();
        assert!(
            gate.contains("ARCH-NO-SECRETS-IN-URL-1"),
            ".camerata/rules.json must list the rule id"
        );

        let _ = std::fs::remove_dir_all(&dest);
    }

    /// Build a throwaway local "repo" (a dir with a `.git` marker + one source file carrying
    /// a hardcoded secret the deterministic floor flags). Returned as the `sources` shape
    /// `audit_repos` consumes. The secret guarantees the floor produces ≥1 finding so the
    /// gating assertions have a concrete signal to check.
    fn scratch_repo_with_secret() -> (tempfile::TempDir, Vec<(String, std::path::PathBuf)>) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(
            dir.path().join("config.rs"),
            "let cfg = load();\nconst TOKEN = \"ghp_0123456789012345678901234567890123456\";\nok();\n",
        )
        .unwrap();
        let sources = vec![("me/api".to_string(), dir.path().to_path_buf())];
        (dir, sources)
    }

    /// Scan-type selector: with deterministic ON and AI review OFF, the audit runs the
    /// always-on floor (catching the secret) and makes NO model call — a token-free assertion
    /// that the AI passes are bypassed. (`run_ai_review = false` is exactly the path that skips
    /// every `audit_repo` / deep-tier LLM call, so this test never touches a model.)
    #[tokio::test]
    async fn deterministic_only_runs_floor_and_skips_ai() {
        let (_dir, sources) = scratch_repo_with_secret();
        let (report, _manifest) = audit_repos(
            &sources,
            &[],            // no semantic rules
            Vec::new(),     // no extra notes
            None,           // model
            None,           // calibration model
            crate::ai_audit::ScanMode::Parallel,
            false,          // thorough
            None,           // feedback
            None,           // job
            None,           // incremental_prior
            false,          // deep
            true,           // soc2_enabled
            false,          // run_ai_review  -> AI path fully skipped (no model call)
            true,           // run_deterministic -> floor runs
            None,           // usage_ledger
        )
        .await;
        // The floor caught the secret.
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.rule_id == "SEC-NO-HARDCODED-SECRETS-1"),
            "deterministic floor must flag the secret: {:?}",
            report.findings
        );
        // No AI usage was recorded — the AI review was skipped end-to-end.
        let calls = report.actual_usage.as_ref().map(|u| u.calls).unwrap_or(0);
        assert_eq!(calls, 0, "AI review skipped -> zero model calls");
    }

    /// Scan-type selector: with deterministic OFF (and AI review OFF too, to stay token-free),
    /// the floor is skipped — the secret is NOT flagged. Pairing AI-off keeps the test from
    /// invoking a model; the assertion isolates the deterministic-floor gate.
    #[tokio::test]
    async fn deterministic_off_skips_floor() {
        let (_dir, sources) = scratch_repo_with_secret();
        let (report, _manifest) = audit_repos(
            &sources,
            &[],
            Vec::new(),
            None,
            None,
            crate::ai_audit::ScanMode::Parallel,
            false,
            None,
            None,
            None,
            false,
            true,
            false, // run_ai_review off (token-free)
            false, // run_deterministic off -> floor skipped
            None,  // usage_ledger
        )
        .await;
        assert!(
            !report
                .findings
                .iter()
                .any(|f| f.rule_id == "SEC-NO-HARDCODED-SECRETS-1"),
            "deterministic OFF must skip the floor: {:?}",
            report.findings
        );
        let calls = report.actual_usage.as_ref().map(|u| u.calls).unwrap_or(0);
        assert_eq!(calls, 0, "AI also off -> zero model calls");
    }

    // ── opt_in_only gate: recommended + is_auto_recommended ───────────────────
    // These tests verify that opt_in_only rules (e.g. CICD-CODEQL-SECURITY-SCAN-1,
    // CICD-SEMGREP-SECURITY-SCAN-1) yield BOTH `recommended = false` AND
    // `is_auto_recommended = false` in the proposed payload, even when they are
    // grounded and stack-relevant. They also verify that a normal grounded,
    // stack-relevant, non-opt-in-only rule yields both = true.
    //
    // The test builds a Rule directly and replicates the `recommended` /
    // `is_auto_recommended` computation from `propose_corpus_rules`, making this a
    // regression guard for the onboard.rs side of the fix.

    fn make_ci_security_rule(opt_in_only: bool) -> camerata_rules::Rule {
        // Shapes like CICD-CODEQL-SECURITY-SCAN-1 / CICD-SEMGREP-SECURITY-SCAN-1:
        // grounded, ci-cd domain (mechanical), opt_in_only: <varies>.
        camerata_rules::Rule {
            id: camerata_core::RuleId("CICD-TEST-SECURITY-SCAN-1".to_string()),
            title: "Test CI security scan".to_string(),
            enforcement: camerata_rules::EnforcementKind::Mechanical,
            domain: "ci-cd".to_string(),
            summary: "A CI security scanning rule.".to_string(),
            decision_question: None,
            decision_why: None,
            options: Vec::new(),
            default_option: None,
            verification: camerata_rules::Verification::Grounded,
            sources: Vec::new(),
            verified: None,
            opt_in_only,
            layer3_only: false,
        }
    }

    /// Replicate the proposed-rule mapping logic for a single Rule + a single repo
    /// that has the matching domain in its stack. Returns (recommended, is_auto_recommended).
    fn compute_proposed_flags(r: &camerata_rules::Rule, is_suggested: bool) -> (bool, bool) {
        let recommended = (is_suggested || r.domain == "agentic") && !r.is_opt_in_only();
        let is_auto_recommended = (is_suggested || r.domain == "agentic")
            && r.is_auto_recommended()
            && !r.is_opt_in_only();
        (recommended, is_auto_recommended)
    }

    /// A grounded, stack-relevant, opt_in_only rule must yield recommended=false AND
    /// is_auto_recommended=false. This directly guards against
    /// CICD-CODEQL-SECURITY-SCAN-1 / CICD-SEMGREP-SECURITY-SCAN-1 being pre-checked
    /// or badged "✓ Recommended" in the onboarding proposal.
    #[test]
    fn opt_in_only_grounded_stack_relevant_yields_both_false() {
        let r = make_ci_security_rule(true /* opt_in_only */);
        let (recommended, is_auto_recommended) =
            compute_proposed_flags(&r, true /* is_suggested = stack-relevant */);
        assert!(
            !recommended,
            "opt_in_only rule must not be recommended (no '✓ Recommended' badge)"
        );
        assert!(
            !is_auto_recommended,
            "opt_in_only rule must not be auto-recommended (no pre-check)"
        );
    }

    /// A grounded, stack-relevant, non-opt-in-only rule must yield both true —
    /// it gets the "✓ Recommended" badge AND is pre-checked. This is the
    /// counterpart positive case.
    #[test]
    fn normal_grounded_stack_relevant_rule_yields_both_true() {
        let r = make_ci_security_rule(false /* not opt_in_only */);
        let (recommended, is_auto_recommended) =
            compute_proposed_flags(&r, true /* is_suggested = stack-relevant */);
        assert!(
            recommended,
            "normal grounded stack-relevant rule must be recommended"
        );
        assert!(
            is_auto_recommended,
            "normal grounded stack-relevant rule must be auto-recommended (pre-checked)"
        );
    }

    /// A stack-relevant opt_in_only rule also stays false when NOT stack-relevant.
    #[test]
    fn opt_in_only_not_stack_relevant_also_false() {
        let r = make_ci_security_rule(true /* opt_in_only */);
        let (recommended, is_auto_recommended) =
            compute_proposed_flags(&r, false /* not stack-relevant */);
        assert!(!recommended, "not stack-relevant + opt_in_only must not be recommended");
        assert!(!is_auto_recommended, "not stack-relevant + opt_in_only must not be auto-recommended");
    }

    // ── is_test_or_fixture_path ───────────────────────────────────────────────

    #[test]
    fn test_path_classifier_false_cases() {
        // Production paths must not be classified as test paths.
        assert!(!is_test_or_fixture_path("src/auth.rs"), "src/auth.rs is production");
        assert!(!is_test_or_fixture_path("src/config.rs"), "src/config.rs is production");
        assert!(!is_test_or_fixture_path("crates/api/src/handlers.rs"), "nested production path");
        assert!(!is_test_or_fixture_path("apps/ui/src/page.tsx"), "JS production file");
        assert!(!is_test_or_fixture_path("migrations/001_init.sql"), "migrations are production");
        assert!(!is_test_or_fixture_path("lib/utils.py"), "plain Python lib");
        assert!(!is_test_or_fixture_path("app.rs"), "bare filename with no test indicators");
    }

    #[test]
    fn test_path_classifier_true_cases() {
        // Directory-segment matches.
        assert!(is_test_or_fixture_path("tests/auth_test.rs"), "tests/ segment");
        assert!(is_test_or_fixture_path("test/helpers.ts"), "test/ segment");
        assert!(is_test_or_fixture_path("testdata/sample.json"), "testdata/ segment");
        assert!(is_test_or_fixture_path("fixtures/keys.env"), "fixtures/ segment");
        assert!(is_test_or_fixture_path("crates/x/src/fixtures/keys.py"), "nested fixtures/");
        assert!(is_test_or_fixture_path("__tests__/auth.test.ts"), "__tests__/ segment");
        assert!(is_test_or_fixture_path("examples/demo.rs"), "examples/ segment");
        assert!(is_test_or_fixture_path("benches/bench.rs"), "benches/ segment");

        // Filename-pattern matches.
        assert!(is_test_or_fixture_path("tests/auth_test.rs"), "*_test.rs (Rust/Go style)");
        assert!(is_test_or_fixture_path("src/auth_test.go"), "*_test.go in production dir");
        assert!(is_test_or_fixture_path("app.test.ts"), "*.test.ts (JS/TS style)");
        assert!(is_test_or_fixture_path("app.spec.ts"), "*.spec.ts (JS/TS style)");
        assert!(is_test_or_fixture_path("src/auth.spec.tsx"), "*.spec.tsx");
        assert!(is_test_or_fixture_path("test_secrets.py"), "test_*.py (Python unittest)");
        assert!(is_test_or_fixture_path("conftest.py"), "conftest.py (pytest)");

        // Case-insensitive segment match.
        assert!(is_test_or_fixture_path("Tests/config.rs"), "Tests/ (capital T)");
        assert!(is_test_or_fixture_path("FIXTURES/creds.json"), "FIXTURES/ (all caps)");
    }

    #[test]
    fn floor_finding_on_test_path_is_low_with_note() {
        // A GitHub PAT inside a tests/ directory must be down-ranked to "low" and
        // annotated, not critical — it's overwhelmingly a detection-fixture value.
        let secret = "const TOKEN = \"ghp_0123456789012345678901234567890123456\";";
        let findings = audit_content("me/api", "tests/auth_test.rs", secret);
        assert_eq!(findings.len(), 1, "finding still visible: {findings:?}");
        let f = &findings[0];
        assert_eq!(
            f.severity, TEST_PATH_SEVERITY,
            "test-path finding must be down-ranked to {TEST_PATH_SEVERITY}"
        );
        assert!(
            f.detail.contains("test/fixture code"),
            "detail must contain the test-path note: {:?}",
            f.detail
        );
        assert!(
            f.detail.contains("verify"),
            "note must tell the architect to verify: {:?}",
            f.detail
        );
    }

    #[test]
    fn floor_finding_on_production_path_stays_critical() {
        // The SAME secret on a production path must remain critical — no down-rank.
        let secret = "const TOKEN = \"ghp_0123456789012345678901234567890123456\";";
        let findings = audit_content("me/api", "src/config.rs", secret);
        assert_eq!(findings.len(), 1, "finding still visible: {findings:?}");
        let f = &findings[0];
        assert_eq!(
            f.severity, "critical",
            "production-path finding must stay critical"
        );
        assert!(
            !f.detail.contains("test/fixture code"),
            "production finding must NOT carry the test-path note: {:?}",
            f.detail
        );
    }

    #[test]
    fn floor_finding_on_fixture_subdir_is_low() {
        // `crates/x/src/fixtures/keys.py` contains a secret the floor would normally
        // flag critical — but fixtures/ makes it a test context.
        let secret = "API_KEY = \"ghp_0123456789012345678901234567890123456\"\n";
        let findings = audit_content("me/svc", "crates/x/src/fixtures/keys.py", secret);
        assert!(
            findings.iter().all(|f| f.severity == TEST_PATH_SEVERITY),
            "all findings in fixture subdir must be down-ranked: {findings:?}"
        );
    }

    #[test]
    fn test_scope_line_ranges_finds_cfg_test_mod() {
        let content = r#"
fn production_fn() {
    // normal code
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        let secret = "fake-secret";
    }
}
"#;
        let ranges = test_scope_line_ranges("src/foo.rs", content);
        assert!(!ranges.is_empty(), "should find at least one test scope");
        // The #[cfg(test)] mod must be in a range
        let cfg_line = content
            .lines()
            .enumerate()
            .find(|(_, l)| l.contains("#[cfg(test)]"))
            .map(|(i, _)| i + 1)
            .unwrap();
        assert!(
            is_in_test_scope(cfg_line, &ranges),
            "cfg(test) line must be in scope"
        );
        // Production fn lines must NOT be in any test scope
        let prod_line = content
            .lines()
            .enumerate()
            .find(|(_, l)| l.contains("production_fn"))
            .map(|(i, _)| i + 1)
            .unwrap();
        assert!(
            !is_in_test_scope(prod_line, &ranges),
            "production fn must not be in test scope"
        );
    }

    #[test]
    fn test_scope_line_ranges_not_fooled_by_braces_in_strings_comments() {
        let content = r#"
fn production() {
    let s = "{ this brace is in a string }";
    // { this is in a comment }
}

#[cfg(test)]
mod tests {
    fn fake() {
        let x = 1;
    }
}
"#;
        let ranges = test_scope_line_ranges("src/foo.rs", content);
        // The production function must NOT be in any test scope
        let prod_line = content
            .lines()
            .enumerate()
            .find(|(_, l)| l.contains("production()"))
            .map(|(i, _)| i + 1)
            .unwrap();
        assert!(
            !is_in_test_scope(prod_line, &ranges),
            "production must not be in test scope"
        );
    }

    #[test]
    fn audit_content_production_secret_stays_critical_beside_inline_test_block() {
        let content = concat!(
            "// production code\n",
            "const REAL_TOKEN: &str = \"ghp_0123456789012345678901234567890123456\";\n", // line 2
            "\n",
            "fn do_thing() {}\n",
            "\n",
            "#[cfg(test)]\n",   // line 6
            "mod tests {\n",
            "    #[test]\n",
            "    fn test_detection() {\n",
            "        // This fake token must be low/in_test\n",
            "        let fake = \"ghp_9999999999999999999999999999999999999999\";\n", // line 11
            "        assert!(fake.len() > 10);\n",
            "    }\n",
            "}\n",
        );
        let findings = audit_content("me/repo", "src/auth.rs", content);
        assert!(!findings.is_empty(), "must find something");

        let prod_findings: Vec<_> = findings.iter().filter(|f| !f.in_test).collect();
        let test_findings: Vec<_> = findings.iter().filter(|f| f.in_test).collect();

        assert!(!prod_findings.is_empty(), "must find the production secret");
        assert!(!test_findings.is_empty(), "must find the test-block secret");

        for f in &prod_findings {
            assert_eq!(
                f.severity, "critical",
                "production secret must be critical: {:?}",
                f
            );
            assert!(!f.in_test, "production finding must have in_test=false: {:?}", f);
            assert!(
                !f.needs_review,
                "production finding must have needs_review=false: {:?}",
                f
            );
        }
        for f in &test_findings {
            assert_eq!(
                f.severity,
                TEST_PATH_SEVERITY,
                "test finding must be low: {:?}",
                f
            );
            assert!(f.in_test, "test finding must have in_test=true: {:?}", f);
            assert!(
                f.needs_review,
                "test finding must have needs_review=true: {:?}",
                f
            );
        }
    }

    #[test]
    fn finding_flags_serialize_and_production_has_false_defaults() {
        let f = Finding {
            repo: "me/repo".to_string(),
            path: "src/main.rs".to_string(),
            line: 5,
            rule_id: "SEC-NO-HARDCODED-SECRETS-1".to_string(),
            severity: "critical".to_string(),
            snippet: "secret".to_string(),
            detail: "Hardcoded secret".to_string(),
            in_test: false,
            needs_review: false,
            ..Finding::default()
        };
        let json = serde_json::to_string(&f).unwrap();
        let back: Finding = serde_json::from_str(&json).unwrap();
        assert!(!back.in_test, "production finding: in_test must be false");
        assert!(
            !back.needs_review,
            "production finding: needs_review must be false"
        );

        // Back-compat: old serialized finding without the new fields deserializes with false defaults
        let legacy = r#"{"repo":"r","path":"p","line":1,"rule_id":"X","severity":"high","snippet":"s","detail":"d","status":"active"}"#;
        let f2: Finding = serde_json::from_str(legacy).unwrap();
        assert!(!f2.in_test);
        assert!(!f2.needs_review);
    }
}
