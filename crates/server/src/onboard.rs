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
}

/// Findings default to `active` (enforced) until classified against suppressions.
fn default_status() -> String {
    "active".to_string()
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
    /// Every violation found, across all repos (each tagged with its repo).
    pub findings: Vec<Finding>,
    /// The proposed starter ruleset (aggregated over all repos).
    pub proposed_rules: Vec<ProposedRule>,
    /// True when no scan was performed because GitHub is not connected.
    pub gated: bool,
    /// A human message (e.g. the connect-GitHub gate, a per-repo error, or a cap).
    pub message: Option<String>,
}

impl ScanReport {
    /// The connect-GitHub gate result: no scan performed.
    pub fn gated(repos: &[String]) -> Self {
        Self {
            repos: repos.to_vec(),
            stacks: Vec::new(),
            files_scanned: 0,
            findings: Vec::new(),
            proposed_rules: Vec::new(),
            gated: true,
            message: Some(
                "Connect GitHub (set CAMERATA_GITHUB_TOKEN) so Camerata can read the repo(s)."
                    .to_string(),
            ),
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
pub fn audit_content(repo: &str, path: &str, content: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    // Whole-content matching (not line-by-line) so MULTI-LINE constructs are caught —
    // e.g. a `format!` SQL whose keyword and interpolation are on different lines. Each
    // match is attributed to the line where it starts.
    for rule_id in AUDIT_RULES {
        for line_no in camerata_gateway::content_match_lines(rule_id, content) {
            let snippet: String = lines
                .get(line_no.saturating_sub(1))
                .map(|l| l.trim().chars().take(160).collect())
                .unwrap_or_default();
            findings.push(Finding {
                repo: repo.to_string(),
                path: path.to_string(),
                line: line_no,
                rule_id: rule_id.to_string(),
                severity: severity_for(rule_id).to_string(),
                snippet,
                detail: title_for(rule_id),
                status: default_status(),
                also_matches: Vec::new(),
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
            scope: "repo-local".to_string(),
            enforcement_point: "content".to_string(),
            domain: "security".to_string(),
            repos: repos.to_vec(),
            placement: "CI gate + gate config installed in every repo".to_string(),
            finding_count,
            recommended: true,
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
            scope: "cross-repo".to_string(),
            enforcement_point: "integration".to_string(),
            repos: repos.to_vec(),
            domain: "integration".to_string(),
            placement: "Integration gate, pre-PR, run across the assembled repo set".to_string(),
            finding_count: 0,
            recommended: true,
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
        scope: "process".to_string(),
        domain: "process".to_string(),
        enforcement_point: "vcs-action".to_string(),
        repos: repos.to_vec(),
        placement: "VCS-action gate at commit/PR (per account, all repos)".to_string(),
        finding_count: 0,
        recommended: false,
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
            "JavaScript" | "TypeScript" => {
                domains.insert("fullstack");
                domains.insert("api-layer");
            }
            "Rust" => {
                domains.insert("rust");
                domains.insert("api-layer");
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
            // An ORM/DB layer implies both the ORM-specific rules and the SQL +
            // migration-hygiene (ci-cd) rules.
            "SeaORM" | "Diesel" | "sqlx" => {
                domains.insert("rust:seaorm");
                domains.insert("sql");
                domains.insert("ci-cd");
            }
            "Next.js" => {
                domains.insert("javascript:next");
                domains.insert("fullstack");
                domains.insert("ui");
            }
            "React" | "Vue" | "Svelte" | "Angular" => {
                domains.insert("ui");
                domains.insert("fullstack");
            }
            "Axum" | "Actix" | "Express" | "FastAPI" | "Flask" | "Django" | "Rails"
            | "ASP.NET" => {
                domains.insert("api-layer");
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
                })
                .collect();
            let enforcement = match r.enforcement {
                camerata_rules::EnforcementKind::Prose => "prose",
                camerata_rules::EnforcementKind::Structured => "structured",
                camerata_rules::EnforcementKind::Mechanical => "mechanical",
            };
            let kind = if matches!(r.enforcement, camerata_rules::EnforcementKind::Mechanical) {
                "mechanical"
            } else {
                "review"
            };
            ProposedRule {
                id: r.id.0.clone(),
                title: r.title.clone(),
                kind: kind.to_string(),
                enforcement: enforcement.to_string(),
                options,
                default_option: r.default_option.clone(),
                scope: "repo-local".to_string(),
                domain: r.domain.clone(),
                enforcement_point: "content".to_string(),
                repos,
                placement: "CI gate + gate config in each repo this rule's domain applies to".to_string(),
                finding_count: 0,
                // SUGGESTED = the rule's domain matches the scanned stack. AGENTIC rules
                // are ALWAYS suggested by design (they govern how the AI fleet builds,
                // regardless of stack). The rest are available but not recommended here.
                recommended: is_suggested || r.domain == "agentic",
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
        findings,
        proposed_rules,
        gated: false,
        message: None,
    }
}

// ── Tech-debt ticket (accept findings as debt -> open a GitHub issue) ───────────

/// Render selected findings as a GitHub issue body, grouped by repo.
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
    for (repo, fs) in by_repo {
        s.push_str(&format!("### {repo}\n\n"));
        for f in fs {
            s.push_str(&format!(
                "- **[{}]** `{}` — `{}:{}`\n",
                f.severity.to_uppercase(),
                f.rule_id,
                f.path,
                f.line
            ));
        }
        s.push('\n');
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
    use camerata_worktracker::{HttpTransport, ReqwestTransport};
    let transport = ReqwestTransport::new(format!("Bearer {token}"))?;
    let url = format!("https://api.github.com/repos/{owner}/{repo}/issues");
    let body = serde_json::to_string(&serde_json::json!({
        "title": title,
        "body": tech_debt_issue_body(findings),
    }))?;
    let resp = transport.post(&url, &body).await?;
    if !(200..300).contains(&resp.status) {
        anyhow::bail!("GitHub create issue: HTTP {} {}", resp.status, resp.body);
    }
    let v: serde_json::Value = serde_json::from_str(&resp.body)?;
    Ok(v["html_url"].as_str().unwrap_or_default().to_string())
}

// ── GitHub repo reader (needs the token) ────────────────────────────────────────

use std::io::Read as _;

use flate2::read::GzDecoder;

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
];

fn has_code_ext(path: &str) -> bool {
    match path.rsplit_once('.') {
        Some((_, ext)) => CODE_EXTS.contains(&ext.to_ascii_lowercase().as_str()),
        None => false,
    }
}

/// Fetch the WHOLE repo's auditable files in ONE request: download the repo
/// tarball (gzipped tar) and gunzip + untar it in memory, keeping the text/code
/// files under the size cap. No per-file API calls, so a large repo is scanned
/// fully without N requests or rate-limit blowups. Returns the files and whether
/// the `HARD_CAP_FILES` safety net was hit (only for pathological monorepos).
pub async fn fetch_repo_files(
    owner: &str,
    repo: &str,
    token: &str,
) -> anyhow::Result<(Vec<(String, String)>, bool)> {
    // The shared transport is text-only; the tarball is binary, so use reqwest
    // directly. GitHub redirects the tarball to a pre-signed codeload URL, so the
    // Authorization header being dropped on the cross-host redirect is fine.
    let client = reqwest::Client::builder()
        .user_agent(concat!("camerata-orchestrator/", env!("CARGO_PKG_VERSION")))
        .use_rustls_tls()
        .build()?;
    let url = format!("https://api.github.com/repos/{owner}/{repo}/tarball");
    let resp = client
        .get(&url)
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("GitHub tarball {owner}/{repo}: HTTP {}", resp.status());
    }
    let bytes = resp.bytes().await?;

    // Decompress + untar over the in-memory bytes on a blocking thread (sync IO).
    tokio::task::spawn_blocking(move || extract_code_files(&bytes))
        .await
        .map_err(|e| anyhow::anyhow!("tarball extraction task failed: {e}"))?
}

/// Gunzip + untar a repo tarball, returning its auditable text/code files (path
/// relative to the repo root) plus whether the file cap was hit. Pure over bytes.
fn extract_code_files(gz_bytes: &[u8]) -> anyhow::Result<(Vec<(String, String)>, bool)> {
    let gz = GzDecoder::new(gz_bytes);
    let mut archive = tar::Archive::new(gz);
    let mut files = Vec::new();
    let mut truncated = false;

    for entry in archive.entries()? {
        let mut e = entry?;
        if e.header().entry_type() != tar::EntryType::Regular {
            continue;
        }
        // Tarball paths are `<repo>-<sha>/<path>`; strip the top dir.
        let raw = e.path()?.to_string_lossy().into_owned();
        let Some((_, path)) = raw.split_once('/') else {
            continue;
        };
        if path.is_empty() || !has_code_ext(path) {
            continue;
        }
        if e.header().size().unwrap_or(0) as usize > MAX_FILE_BYTES {
            continue;
        }
        // Read the whole entry (keeps tar positioning correct), skip non-UTF-8.
        let mut buf = Vec::new();
        if e.read_to_end(&mut buf).is_err() {
            continue;
        }
        let Ok(content) = String::from_utf8(buf) else {
            continue;
        };
        files.push((path.to_string(), content));
        if files.len() >= HARD_CAP_FILES {
            truncated = true;
            break;
        }
    }
    Ok((files, truncated))
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
    repos: &[String],
    token: &str,
) -> Vec<crate::suppression::SuppressionRecord> {
    use crate::suppression::{parse_inline_waivers, registry, Baseline, FindingRef};
    let mut out = Vec::new();
    for spec in repos {
        let Some((owner, repo)) = spec.split_once('/') else {
            continue;
        };
        let Ok((files, _)) = fetch_repo_files(owner, repo, token).await else {
            continue;
        };
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
        });
    }
}

/// Phase 1 — DETECT. Fetch the repos, detect each stack, and PROPOSE a starter ruleset.
/// It does NOT audit code yet — that's [`audit_repos`], run after the architect picks
/// which rules to enforce. This is the "scan to determine languages / frameworks /
/// domains → suggest rules" step the two-phase flow opens with.
pub async fn scan_repos(specs: &[String], token: &str) -> ScanReport {
    let mut stacks = Vec::new();
    let mut files_total = 0usize;
    let mut repos_ok = Vec::new();
    let mut notes = Vec::new();

    for spec in specs {
        let spec = spec.trim();
        if spec.is_empty() {
            continue;
        }
        let Some((owner, repo)) = spec.split_once('/') else {
            notes.push(format!("{spec}: not `owner/repo`, skipped"));
            continue;
        };
        match fetch_repo_files(owner, repo, token).await {
            Ok((files, truncated)) => {
                files_total += files.len();
                stacks.push(detect_stack(spec, &files));
                repos_ok.push(spec.to_string());
                if truncated {
                    notes.push(format!(
                        "{spec}: more than {HARD_CAP_FILES} files; truncated at the safety limit"
                    ));
                }
            }
            Err(e) => notes.push(format!("{spec}: scan failed ({e})")),
        }
    }

    let repo_domains: Vec<(String, Vec<String>)> = stacks
        .iter()
        .map(|s| (s.repo.clone(), domains_for_stack(s)))
        .collect();
    let mut report = build_report(repos_ok, stacks, files_total, Vec::new());
    report.proposed_rules = propose_corpus_rules(&repo_domains).await;
    if !notes.is_empty() {
        report.message = Some(notes.join(" · "));
    }
    report
}

/// Phase 2 — AUDIT against the SELECTED rules. After the architect picks rules (Phase 1),
/// this audits the code: the deterministic content rules (secrets / raw-SQL / secret-URL)
/// are the always-on SECURITY floor and produce ENFORCED findings; the AI audit is
/// PARAMETERIZED by the selected rules' directives (so it checks the code against what the
/// project actually adopted) and produces ADVISORY findings plus its investigative pass.
/// `selected` is `(rule_id, directive)` for each adopted rule.
/// Whether a rule describes what CODE should look like (audit it against source) vs how
/// the FLEET/TEAM operates (governance/process — arm it, but don't code-audit). The
/// orchestration (`ORCH-`), meta-principle (`SPIRIT-`), and process (`PROC-`) families
/// are governance/process; everything else (ARCH-/RUST-/SQL-/UI-/SEC-/…) is code.
fn is_code_auditable_rule(id: &str) -> bool {
    !(id.starts_with("ORCH-") || id.starts_with("SPIRIT-") || id.starts_with("PROC-"))
}

#[allow(clippy::too_many_arguments)]
pub async fn audit_repos(
    specs: &[String],
    selected: &[(String, String)],
    token: &str,
    model: Option<&str>,
    mode: crate::ai_audit::ScanMode,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    job: Option<(&crate::jobs::JobStore, &str)>,
) -> ScanReport {
    let mut all_findings = Vec::new();
    let mut stacks = Vec::new();
    let mut files_total = 0usize;
    let mut repos_ok = Vec::new();
    let mut notes = Vec::new();
    let llm = crate::llm::Llm::from_env();

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
    let semantic: Vec<(String, String)> = selected
        .iter()
        .filter(|(id, _)| camerata_gateway::lookup_arm(id).is_none())
        .filter(|(id, _)| is_code_auditable_rule(id))
        .cloned()
        .collect();

    for spec in specs {
        let spec = spec.trim();
        if spec.is_empty() {
            continue;
        }
        let Some((owner, repo)) = spec.split_once('/') else {
            notes.push(format!("{spec}: not `owner/repo`, skipped"));
            continue;
        };
        match fetch_repo_files(owner, repo, token).await {
            Ok((files, truncated)) => {
                files_total += files.len();
                stacks.push(detect_stack(spec, &files));
                // Deterministic security floor (always-on): ENFORCED findings. Push them to
                // the job up front so the live preview shows the criticals immediately.
                let mut repo_findings = audit_files(spec, &files);
                if let Some((jstore, jid)) = job {
                    jstore.add_findings(jid, repo_findings.clone());
                }
                // AI audit parameterized by the SEMANTIC rules only: ADVISORY findings.
                match crate::ai_audit::audit_repo(
                    &llm, spec, &files, &semantic, model, mode, feedback, job,
                )
                .await
                {
                    Ok((ai_findings, _ai_rules)) => repo_findings.extend(ai_findings),
                    Err(e) => notes.push(format!("{spec}: AI audit skipped ({e})")),
                }
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

    let mut report = build_report(repos_ok, stacks, files_total, all_findings);
    if !notes.is_empty() {
        report.message = Some(notes.join(" · "));
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_pulls_code_files_strips_top_dir_and_skips_binaries() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        // Build a gzipped tar like GitHub's: entries under a `<repo>-<sha>/` root.
        let mut tar_buf = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_buf);
            let mut add = |name: &str, data: &[u8]| {
                let mut h = tar::Header::new_gnu();
                h.set_size(data.len() as u64);
                h.set_entry_type(tar::EntryType::Regular);
                h.set_mode(0o644);
                h.set_cksum();
                builder.append_data(&mut h, name, data).unwrap();
            };
            add("repo-abc123/src/main.rs", b"fn main() {}\n");
            add("repo-abc123/README.md", b"# readme"); // not a code ext -> skipped
            add("repo-abc123/logo.png", b"\x89PNG\r\n"); // not code -> skipped
            builder.finish().unwrap();
        }
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(&tar_buf).unwrap();
        let gz_bytes = gz.finish().unwrap();

        let (files, truncated) = extract_code_files(&gz_bytes).unwrap();
        assert!(!truncated);
        assert_eq!(files.len(), 1, "only the .rs file is auditable: {files:?}");
        assert_eq!(files[0].0, "src/main.rs", "top dir stripped");
        assert_eq!(files[0].1, "fn main() {}\n");
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
    fn audit_flags_a_hardcoded_secret_with_line_severity_and_repo() {
        // A GitHub PAT literal is exactly what SEC-NO-HARDCODED-SECRETS-1 denies.
        let content = "let cfg = load();\nconst TOKEN = \"ghp_0123456789012345678901234567890123456\";\nok();";
        let findings = audit_content("me/api", "src/config.rs", content);
        assert_eq!(findings.len(), 1, "one secret -> one finding: {findings:?}");
        let f = &findings[0];
        assert_eq!(f.repo, "me/api", "finding tagged with its repo");
        assert_eq!(f.line, 2, "finding on the right line");
        assert_eq!(f.rule_id, "SEC-NO-HARDCODED-SECRETS-1");
        assert_eq!(f.severity, "critical", "exploitable security defects rank critical");
        assert!(f.path == "src/config.rs");
    }

    #[test]
    fn audit_is_clean_on_clean_content() {
        let content = "fn add(a: i32, b: i32) -> i32 { a + b }\n// nothing to see here";
        assert!(audit_content("me/api", "src/math.rs", content).is_empty());
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
        assert_eq!(secrets.repos, vec!["me/api".to_string()], "universal -> all scanned repos");
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
            &[("b.tsx".to_string(), "export const ok = () => 1;".to_string())],
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
            sql_findings.iter().any(|f| f.rule_id == "SEC-NO-RAW-SQL-CONCAT-1"),
            "multi-line named-arg SQL format! must be caught"
        );

        let key = "const FALLBACK_FINNHUB_KEY: &str = \"c8r9v2aad3i9q1m4f7g0bv8s5p2qk1n7\";";
        let key_findings = audit_content("me/api", "finnhub.rs", key);
        assert!(
            key_findings.iter().any(|f| f.rule_id == "SEC-NO-HARDCODED-SECRETS-1"),
            "bare provider-agnostic key on a *_KEY const must be caught"
        );

        let url = "        format!(\"{base}?symbol={symbol}&token={token}\")";
        let url_findings = audit_content("me/api", "finnhub.rs", url);
        assert!(
            url_findings.iter().any(|f| f.rule_id == "ARCH-NO-SECRETS-IN-URL-1"),
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
}
