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
//!
//! The implementation is split into submodules for readability; all public items
//! are re-exported here so `crate::onboard::X` paths remain stable.

// ── Submodules ──────────────────────────────────────────────────────────────────
pub mod audit;
pub mod files;
pub mod greenfield;
pub mod propose;
pub mod report;
pub mod self_ref;

// ── Re-exports (keep crate::onboard::X paths stable) ────────────────────────────
pub use audit::{audit_content, audit_files};
pub use files::{read_local_repo_files, ExtractedRepo};
pub use greenfield::{scaffold_greenfield_blocking, GreenfieldResult};
pub use propose::{detect_stack, propose_corpus_rules, propose_rules};
pub use report::{
    build_report, create_issue, create_tech_debt_ticket, tech_debt_csv, tech_debt_issue_body,
};
#[allow(unused_imports)]
pub(crate) use report::csv_escape;
pub use self_ref::{
    corpus_texts_from_ruleset, is_governance_or_corpus_artifact, is_self_referential_snippet,
    suppress_self_referential,
};

// Pull private/crate-internal helpers into scope for the orchestration functions and
// test module (via `use super::*`).
pub(crate) use audit::{classify_repo_findings, is_code_auditable_rule};
// Re-export camerata_gateway test-scope primitives so the test module's `use super::*`
// can reach them directly (they're used in inline test assertions).
#[allow(unused_imports)]
pub(crate) use camerata_gateway::{
    is_in_test_scope, is_test_or_fixture_path, test_scope_line_ranges, TEST_PATH_NOTE,
    TEST_PATH_SEVERITY,
};
// `has_code_ext`, `is_noise_path`, and `extra_exclude_dirs` are used only in `#[cfg(test)]`
// from root's perspective (the root orchestration functions call files::* directly via
// the submodule). The re-export is needed so `use super::*` in the test module reaches them.
#[allow(unused_imports)]
pub(crate) use files::{extra_exclude_dirs, has_code_ext, is_noise_path, HARD_CAP_FILES};
pub(crate) use propose::domains_for_stack;
pub(crate) use report::merge_deep_reports;

use serde::{Deserialize, Serialize};

/// The content rules the brownfield audit runs (the ones that are pure functions
/// over file content). Path-based rules (GOV-1 forbidden paths, SEC-NO-PATH-ESCAPE-1)
/// govern WRITE TARGETS, not existing content, so they are not part of the audit.
pub const AUDIT_RULES: &[&str] = &[
    "SEC-NO-HARDCODED-SECRETS-1",
    "SEC-NO-RAW-SQL-CONCAT-1",
    "ARCH-NO-SECRETS-IN-URL-1",
    "SEC-NO-PRIVATE-KEY-1",
    "SEC-NO-VENDOR-TOKEN-1",
    // SEC-NO-SECRET-FILE-1 is path-based (not content-based). content_match_lines returns
    // empty for it, so it produces no line-numbered scan finding. It is included here for
    // completeness (brownfield path-level audit visibility) — its primary home is the gate.
    "SEC-NO-SECRET-FILE-1",
    "SEC-NO-DISABLED-TLS-1",
    "SEC-NO-UNSAFE-DESERIALIZATION-1",
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

// ── Orchestration functions ───────────────────────────────────────────────────────
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
        // Tag each record with its repo so the registry view can show which repo it came from.
        let mut recs = registry(&inline, &baseline, &findings);
        for r in &mut recs {
            r.repo = spec.to_string();
        }
        out.extend(recs);
    }
    out
}
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
        // Clone `dir` for spawn_blocking (which moves it); the outer `dir` ref
        // comes from the loop binding and is the PathBuf we're iterating.
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
    // Fold the always-on dep-audit coverage notes into the report.  The scan-tools
    // preview notes are merged separately (via `merge_scan_preview` in lib.rs); both
    // sets land in `coverage_notes` so the UI sees them in one place.
    // Dep-audit (osv-scanner) runs AFTER this function returns — in the caller
    // (`onboard_audit_start` in lib.rs), AFTER the preview linters, so it never
    // blocks them.  Its coverage notes are appended to the report by the caller.
    if !notes.is_empty() {
        report.message = Some(notes.join(" · "));
    }
    (report, manifest_builder.finish())
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
        // A GitHub PAT literal fires SEC-NO-HARDCODED-SECRETS-1 and SEC-NO-VENDOR-TOKEN-1.
        let content = "let cfg = load();\nconst TOKEN = \"ghp_0123456789012345678901234567890123456\";\nok();";
        let findings = audit_content("me/api", "src/config.rs", content);
        // The ghp_ token matches both the generic secrets rule and the vendor-token rule.
        let f = findings
            .iter()
            .find(|f| f.rule_id == "SEC-NO-HARDCODED-SECRETS-1")
            .expect("SEC-NO-HARDCODED-SECRETS-1 must fire: {findings:?}");
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
        // finding_count is per-rule (2 ghp_ tokens → 2 SEC-NO-HARDCODED-SECRETS-1 findings);
        // findings.len() is total across all rules (same tokens also fire SEC-NO-VENDOR-TOKEN-1).
        let secrets_finding_count = findings
            .iter()
            .filter(|f| f.rule_id == "SEC-NO-HARDCODED-SECRETS-1")
            .count();
        assert_eq!(secrets.finding_count, secrets_finding_count);
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
        // Two repos: a secret in one, clean in the other. The ghp_ token fires both
        // SEC-NO-HARDCODED-SECRETS-1 and SEC-NO-VENDOR-TOKEN-1, so we get 2 findings.
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
        // All findings are from the me/api repo (the ghp_ token fires 2 rules).
        assert!(!report.findings.is_empty());
        assert!(report.findings.iter().all(|f| f.repo == "me/api"));
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

        // The CI workflow must be emitted for mechanical rules (camerata-gates.yml is the
        // SSOT-generated workflow; camerata-governance.yml was the old placeholder, replaced
        // by the arm/emit SSOT reconciliation — see docs/decisions/2026-06-23_ssot_emit_reconciliation.md).
        assert!(
            result
                .files_written
                .iter()
                .any(|f| f.ends_with("camerata-gates.yml")),
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
        // Disable the always-on dep-audit pass so this test does not trigger
        // `ensure_osv_scanner` → `download_osv_scanner` (live network request).
        // This test exercises the deterministic floor + scan-mode selector logic,
        // not dep-audit. See `crate::dep_audit::DISABLE_ENV_VAR`.
        std::env::set_var("CAMERATA_DISABLE_DEP_AUDIT", "1");
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
        // Disable the always-on dep-audit pass so this test does not trigger
        // `ensure_osv_scanner` → `download_osv_scanner` (live network request).
        // This test exercises the scan-mode selector (deterministic gate), not dep-audit.
        // See `crate::dep_audit::DISABLE_ENV_VAR`.
        std::env::set_var("CAMERATA_DISABLE_DEP_AUDIT", "1");
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

    /// dep-audit was MOVED OUT of audit_repos and now runs AFTER the preview linters in
    /// the caller (lib.rs `onboard_audit_start`).  This test verifies that audit_repos
    /// itself no longer calls dep-audit: when we pass a job handle and DISABLE_DEP_AUDIT
    /// is NOT set, the job must show ONLY the floor tool (done = 1/1) after audit_repos
    /// returns — no "dep-audit" row.  That row is added by the caller after the linter
    /// pass, which is tested separately.
    ///
    /// This is the ordering regression guard: if dep-audit ever drifts back inside
    /// audit_repos, the floor-only assertion will fail.
    #[tokio::test]
    async fn audit_repos_does_not_call_dep_audit() {
        // DISABLE_DEP_AUDIT silences the provisioning path in run_dep_audit, but that
        // function is no longer called from audit_repos at all — so this env var is
        // belt-and-suspenders (and matches what real test callers set for isolation).
        std::env::set_var("CAMERATA_DISABLE_DEP_AUDIT", "1");
        let (_dir, sources) = scratch_repo_with_secret();
        let jobs = crate::jobs::JobStore::new();
        let jid = jobs.create("audit");
        let _ = audit_repos(
            &sources,
            &[],
            Vec::new(),
            None,
            None,
            crate::ai_audit::ScanMode::Parallel,
            false,
            None,
            Some((&jobs, &jid)),
            None,
            false,
            true,
            false, // run_ai_review off
            true,  // run_deterministic on → floor runs
            None,
        )
        .await;
        let progress = jobs.det_progress(&jid).unwrap();
        // Only the floor tool must have been registered inside audit_repos.
        assert_eq!(
            progress.total, 1,
            "audit_repos must register exactly ONE tool (floor); dep-audit must NOT be inside it: {:?}",
            progress.tools
        );
        assert!(
            progress.tools.iter().any(|t| t.tool == "floor"),
            "floor tool must be registered"
        );
        assert!(
            !progress.tools.iter().any(|t| t.tool == "dep-audit"),
            "dep-audit must NOT appear inside audit_repos tool list; it runs in the caller"
        );
    }

    // ── corpus-rules wire payload carries full content ────────────────────────
    // The Applied/Project Rules modal joins each ruleset selection against the
    // `/api/corpus-rules` payload (which is `propose_corpus_rules(&[])`). This test
    // proves the SERVER side: a real corpus rule (RUST-DIOXUS-12) is present in that
    // payload WITH its full content — decision question + multiple options — so the
    // UI has everything it needs to render the real modal (not a stub). If this
    // regresses, the modal can only ever show a stub no matter what the UI does.
    #[tokio::test]
    async fn propose_corpus_rules_includes_rust_dioxus_12_with_full_content() {
        let proposed = propose_corpus_rules(&[]).await;
        assert!(
            !proposed.is_empty(),
            "the corpus must load at least one rule (CARGO_MANIFEST_DIR-relative principles dir)"
        );
        let rule = proposed
            .iter()
            .find(|r| r.id == "RUST-DIOXUS-12")
            .expect("RUST-DIOXUS-12 must be present in the corpus-rules payload");
        // Full content the modal renders: a decision question and a multi-option set.
        assert!(
            rule.decision_question
                .as_deref()
                .map(|q| !q.trim().is_empty())
                .unwrap_or(false),
            "RUST-DIOXUS-12 must carry a non-empty decision_question on the wire: {:?}",
            rule.decision_question
        );
        assert!(
            rule.options.len() >= 2,
            "RUST-DIOXUS-12 must carry its alternatives (it has 4): got {}",
            rule.options.len()
        );
        // Title is the human description shown at the top of the modal.
        assert!(
            !rule.title.trim().is_empty(),
            "RUST-DIOXUS-12 must carry a non-empty title"
        );
        // It is a real corpus rule, so it must NOT look like a single-variant stub.
        assert!(
            !rule.options.is_empty(),
            "a real corpus rule must not have empty options (that is the stub the bug showed)"
        );
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
        // The ghp_ token fires both SEC-NO-HARDCODED-SECRETS-1 and SEC-NO-VENDOR-TOKEN-1;
        // check the SEC-NO-HARDCODED-SECRETS-1 finding specifically.
        let secret = "const TOKEN = \"ghp_0123456789012345678901234567890123456\";";
        let findings = audit_content("me/api", "tests/auth_test.rs", secret);
        assert!(!findings.is_empty(), "finding still visible: {findings:?}");
        // All findings in a test path must be down-ranked.
        let f = findings
            .iter()
            .find(|f| f.rule_id == "SEC-NO-HARDCODED-SECRETS-1")
            .expect("SEC-NO-HARDCODED-SECRETS-1 must fire");
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
        // The ghp_ token fires both SEC-NO-HARDCODED-SECRETS-1 and SEC-NO-VENDOR-TOKEN-1.
        let secret = "const TOKEN = \"ghp_0123456789012345678901234567890123456\";";
        let findings = audit_content("me/api", "src/config.rs", secret);
        assert!(!findings.is_empty(), "finding still visible: {findings:?}");
        let f = findings
            .iter()
            .find(|f| f.rule_id == "SEC-NO-HARDCODED-SECRETS-1")
            .expect("SEC-NO-HARDCODED-SECRETS-1 must fire");
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

    // ── Gitignore-aware walk tests (Feature: scan-hygiene) ────────────────

    /// Helper: run `git init` in `dir` and return the path as a string.
    fn git_init(dir: &std::path::Path) {
        let status = std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("git init failed");
        assert!(status.success(), "git init must succeed");
    }

    #[test]
    fn gitignore_walk_skips_gitignored_file() {
        // A secrets.env listed in .gitignore must NOT appear in the extracted files.
        // Note: the file is untracked but gitignored — the `ignore` crate respects the
        // .gitignore file regardless of git-index state.
        let dir = tempfile::TempDir::new().expect("tempdir");
        let root = dir.path();
        git_init(root);

        // Write .gitignore so secrets.env is gitignored.
        std::fs::write(root.join(".gitignore"), "secrets.env\n")
            .expect("write .gitignore");
        // Write the secret file — gitignored, so should NOT appear in output.
        // Split token across concat!() so the scanner (if it were run) can't see the literal.
        let secret_content = concat!("TOKEN=", "ghp_00000000000000000000000000000000000000\n");
        std::fs::write(root.join("secrets.env"), secret_content)
            .expect("write secrets.env");
        // Write a normal source file that SHOULD appear.
        std::fs::create_dir_all(root.join("src")).expect("mkdir src");
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n")
            .expect("write main.rs");

        let extracted = read_local_repo_files(root).expect("scan ok");
        let paths: Vec<&str> = extracted.files.iter().map(|(p, _)| p.as_str()).collect();
        assert!(
            !paths.iter().any(|p| p.contains("secrets.env")),
            "gitignored secrets.env must not appear in scan: {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p.contains("main.rs")),
            "src/main.rs must appear in scan: {paths:?}"
        );
    }

    #[test]
    fn gitignore_walk_scans_tracked_env() {
        // A .env file that is NOT in .gitignore (committed/tracked) must still be scanned.
        // `.env` has extension `env` which is in CODE_EXTS, so has_code_ext passes.
        let dir = tempfile::TempDir::new().expect("tempdir");
        let root = dir.path();
        git_init(root);

        // No .gitignore (or .gitignore that does not mention .env) → .env is NOT ignored.
        std::fs::write(root.join(".gitignore"), "# nothing here\n")
            .expect("write empty .gitignore");
        // Write a .env with detectable content. Split so the scanner doesn't fire on the test.
        let env_content = concat!("DATABASE_URL=postgres://user:", "hunter2@localhost/db\n");
        std::fs::write(root.join(".env"), env_content).expect("write .env");
        // A clean Rust file so there are multiple files.
        std::fs::create_dir_all(root.join("src")).expect("mkdir src");
        std::fs::write(root.join("src/lib.rs"), "pub fn lib() {}\n")
            .expect("write lib.rs");

        let extracted = read_local_repo_files(root).expect("scan ok");
        let paths: Vec<&str> = extracted.files.iter().map(|(p, _)| p.as_str()).collect();
        // The .env is NOT in .gitignore → must be scanned.
        assert!(
            paths.iter().any(|p| *p == ".env"),
            "tracked .env must be scanned when not gitignored: {paths:?}"
        );
    }

    #[test]
    fn non_git_dir_falls_back_to_noise_denylist() {
        // A directory without .git must not error; it should fall back to the
        // noise-denylist walk and prune standard noise dirs like node_modules/.
        let dir = tempfile::TempDir::new().expect("tempdir");
        let root = dir.path();
        // No git init → no .git directory.

        // Write a file inside node_modules (noise dir — must be pruned).
        let nm = root.join("node_modules");
        std::fs::create_dir_all(&nm).expect("mkdir node_modules");
        std::fs::write(nm.join("foo.ts"), "export const x = 1;\n")
            .expect("write node_modules/foo.ts");
        // Write a clean source file (must be included).
        std::fs::create_dir_all(root.join("src")).expect("mkdir src");
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n")
            .expect("write main.rs");

        let extracted = read_local_repo_files(root).expect("non-git fallback ok");
        let paths: Vec<&str> = extracted.files.iter().map(|(p, _)| p.as_str()).collect();
        assert!(
            !paths.iter().any(|p| p.contains("node_modules")),
            "node_modules must be pruned by noise denylist: {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p.contains("main.rs")),
            "src/main.rs must be included: {paths:?}"
        );
    }

    // ── Self-reference suppression tests (Feature: scan-hygiene) ─────────

    #[test]
    fn governance_artifact_detected_by_header() {
        let content =
            "<!-- Generated by Camerata. Edit the rule selection, not this file. -->\n\n# AGENTS.md\n";
        assert!(
            is_governance_or_corpus_artifact("AGENTS.md", content),
            "AGENTS.md with Generated-by header must be detected"
        );
        assert!(
            is_governance_or_corpus_artifact("CONVENTIONS.md", content),
            "CONVENTIONS.md with Generated-by header must be detected"
        );
    }

    #[test]
    fn governance_artifact_detected_by_camerata_dir() {
        assert!(
            is_governance_or_corpus_artifact(".camerata/baseline.json", "{}"),
            ".camerata/ prefix must be detected"
        );
        assert!(
            is_governance_or_corpus_artifact("project/.camerata/rules.json", "{}"),
            "nested /.camerata/ segment must be detected"
        );
    }

    #[test]
    fn governance_artifact_detected_by_principles_toml() {
        assert!(
            is_governance_or_corpus_artifact(
                "crates/rules/principles/rust/foo.toml",
                "[package]\n"
            ),
            "principles/ segment + .toml extension must be detected"
        );
        assert!(
            is_governance_or_corpus_artifact("principles/api-layer/bar.toml", "..."),
            "top-level principles/ must be detected"
        );
        // A .toml that is NOT under any principles/ segment is NOT a corpus artifact.
        assert!(
            !is_governance_or_corpus_artifact("Cargo.toml", "[package]\nname = \"x\"\n"),
            "Cargo.toml at root is not a corpus artifact"
        );
    }

    #[test]
    fn ordinary_source_file_not_a_governance_artifact() {
        assert!(
            !is_governance_or_corpus_artifact("src/main.rs", "fn main() {}"),
            "ordinary Rust source must not be a governance artifact"
        );
        assert!(
            !is_governance_or_corpus_artifact("app/config.py", "SECRET_KEY = 'abc'"),
            "ordinary Python source must not be a governance artifact"
        );
    }

    #[test]
    fn self_referential_snippet_matched() {
        let texts = vec!["verify=False disables TLS certificate validation".to_string()];
        assert!(
            is_self_referential_snippet("verify=False", &texts),
            "exact substring must match"
        );
        assert!(
            is_self_referential_snippet("  verify=False  ", &texts),
            "whitespace-padded snippet must match after trim"
        );
    }

    #[test]
    fn self_referential_snippet_no_match() {
        let texts = vec!["verify=False disables TLS".to_string()];
        // A real credential is not in any rule description.
        assert!(
            !is_self_referential_snippet("ghp_actual_secret_here", &texts),
            "non-corpus string must not match"
        );
        // Empty snippet must never match, regardless of corpus.
        assert!(
            !is_self_referential_snippet("", &texts),
            "empty snippet must never match"
        );
        assert!(
            !is_self_referential_snippet("   ", &texts),
            "whitespace-only snippet must never match"
        );
    }

    #[test]
    fn suppress_self_referential_marks_governance_corpus_findings() {
        // A CONVENTIONS.md with a Generated-by header. One finding matches the rule
        // description (suppress it); one finding is a real credential (keep it active).
        let conventions_content = concat!(
            "<!-- Generated by Camerata. Edit the rule selection, not this file. -->\n\n",
            "## SEC-NO-DISABLED-TLS-1\n\n",
            "Do not set verify=False in TLS configuration.\n",
            "\nAnd here is a planted real credential: ghp_",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n"
        );
        let files = vec![("CONVENTIONS.md".to_string(), conventions_content.to_string())];
        // The corpus describes "verify=False" in a rule summary.
        let corpus_texts =
            vec!["Do not set verify=False in TLS configuration.".to_string()];

        let mut findings = vec![
            Finding {
                repo: "me/repo".to_string(),
                path: "CONVENTIONS.md".to_string(),
                line: 4,
                rule_id: "SEC-NO-DISABLED-TLS-1".to_string(),
                severity: "high".to_string(),
                snippet: "Do not set verify=False in TLS configuration.".to_string(),
                detail: "TLS verification disabled".to_string(),
                ..Finding::default()
            },
            Finding {
                repo: "me/repo".to_string(),
                path: "CONVENTIONS.md".to_string(),
                line: 6,
                rule_id: "SEC-NO-VENDOR-TOKEN-1".to_string(),
                severity: "critical".to_string(),
                // Split the token so the scan can't fire on this test file itself.
                snippet: concat!("ghp_", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
                    .to_string(),
                detail: "Vendor token detected".to_string(),
                ..Finding::default()
            },
        ];
        suppress_self_referential(&mut findings, &files, &corpus_texts);
        // The rule-description finding must become suppressed-self-reference.
        assert_eq!(
            findings[0].status, "suppressed-self-reference",
            "rule-description snippet must be suppressed: {:?}",
            findings[0]
        );
        assert!(
            findings[0].detail.contains("self-referential"),
            "suppressed detail must mention self-referential"
        );
        // The real credential must stay active.
        assert_eq!(
            findings[1].status, "active",
            "real credential must stay active: {:?}",
            findings[1]
        );
    }

    #[test]
    fn suppress_self_referential_never_touches_ordinary_files() {
        // Even if the snippet matches corpus text, an ordinary source file must never
        // be suppressed — only governance/corpus artifacts are in scope.
        let files = vec![(
            "app/config.py".to_string(),
            "verify=False  # disable TLS\n".to_string(),
        )];
        let corpus_texts = vec!["verify=False disables TLS".to_string()];
        let mut findings = vec![Finding {
            repo: "me/repo".to_string(),
            path: "app/config.py".to_string(),
            line: 1,
            rule_id: "SEC-NO-DISABLED-TLS-1".to_string(),
            severity: "high".to_string(),
            snippet: "verify=False  # disable TLS".to_string(),
            detail: "TLS verification disabled".to_string(),
            ..Finding::default()
        }];
        suppress_self_referential(&mut findings, &files, &corpus_texts);
        assert_eq!(
            findings[0].status, "active",
            "ordinary file finding must stay active even if snippet matches corpus"
        );
    }
}
