//! Stack detection and rule-proposal functions: detect languages/frameworks from
//! a repo's files and propose a starter ruleset from findings and corpus.

use super::{Finding, ProposedRule, RepoStack, RuleOptionView, RuleSourceView, AUDIT_RULES};

use super::audit::title_for;

/// Map a file extension to a language label.
pub(crate) fn lang_for_ext(path: &str) -> Option<&'static str> {
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
pub(crate) fn detect_frameworks(
    path: &str,
    content: &str,
    out: &mut std::collections::BTreeSet<String>,
) {
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
pub(crate) fn domains_for_stack(s: &RepoStack) -> Vec<String> {
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

/// Propose corpus rules (the architectural ones that carry ALTERNATIVES) for the
/// detected stacks, each bound to ONLY the repos whose domain it applies to (a
/// universal `*` rule binds to all). The architect can override the binding. Each
/// carries its options + default so the architect chooses which alternative to
/// codify. finding_count is 0: scanning these needs the per-language AST checker
/// (future); the selection is real now.
///
/// `repo_domains` is each repo paired with the corpus domains its stack maps to.
pub async fn propose_corpus_rules(
    repo_domains: &[(String, Vec<String>)],
) -> Vec<ProposedRule> {
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
