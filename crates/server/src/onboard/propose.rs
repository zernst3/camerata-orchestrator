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

/// Non-blank source lines in `files`, split by language via [`lang_for_ext`], plus the grand
/// total across EVERY file — recognized-language extension or not, matching `code_chars`'s
/// all-files scope (a `.yaml`/`.json` config file still counts toward total code volume even
/// though it has no language attribution). P6 (2026-09-29): real numbers only — every count
/// here is `content.lines().filter(not blank).count()` on the exact files the scan already
/// read (after noise-pruning), never a synthetic average-bytes-per-line guess. Callers merge
/// the returned per-language map across repos before finalizing into a sorted
/// `Vec<super::LanguageVolume>`.
pub(crate) fn count_source_lines(
    files: &[(String, String)],
) -> (usize, std::collections::BTreeMap<String, usize>) {
    let mut by_lang: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut total = 0usize;
    for (path, content) in files {
        let lines = content.lines().filter(|l| !l.trim().is_empty()).count();
        total += lines;
        if let Some(lang) = lang_for_ext(path) {
            *by_lang.entry(lang.to_string()).or_insert(0) += lines;
        }
    }
    (total, by_lang)
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
            // Nest is a first-class server framework with its own corpus domain
            // (`javascript:nest`, 5 rules). Any `@nestjs/*` package means the repo IS a Nest
            // app; `@nestjs/core` alone would miss a workspace that only pulls in
            // `@nestjs/common` in a given package.json.
            if lc.contains("\"@nestjs/") {
                add("NestJS");
            }
            if lc.contains("redux") {
                add("Redux");
            }
            if lc.contains("\"svelte\"") {
                add("Svelte");
            }
            // Any @supabase/* package (supabase-js, ssr, auth-helpers-*, etc.) means the
            // repo talks to a Supabase project — drive the supabase:* domains below.
            if lc.contains("\"@supabase/") {
                add("Supabase");
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
        // Go modules: the module file is ALSO the dependency manifest, so the per-framework
        // markers below are read from its content exactly like package.json's. Without these,
        // the `go:web` / `go:gorm` / `go:grpc` corpus domains had no detection path at all and
        // their 15 rules could never arm for any repo (W4/A6).
        "go.mod" => {
            add("Go modules");
            // HTTP routers/frameworks → `go:web`. Each is matched on its real module path so a
            // package merely MENTIONING the word (a comment, a vendored path) can't trip it.
            if lc.contains("github.com/gin-gonic/gin") {
                add("Gin");
            }
            if lc.contains("github.com/labstack/echo") {
                add("Echo");
            }
            if lc.contains("github.com/go-chi/chi") {
                add("Chi");
            }
            if lc.contains("github.com/gofiber/fiber") {
                add("Fiber");
            }
            if lc.contains("github.com/gorilla/mux") {
                add("Gorilla Mux");
            }
            // ORM → `go:gorm`.
            if lc.contains("gorm.io/gorm") || lc.contains("github.com/jinzhu/gorm") {
                add("GORM");
            }
            // RPC → `go:grpc`.
            if lc.contains("google.golang.org/grpc") {
                add("gRPC-Go");
            }
        }
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
            // The async runtime itself — drives `rust:tokio` (bounded channels, blocking-in-
            // async, std::sync::Mutex across await, JoinSet). Matched the same loose way
            // `axum`/`actix` already are: presence of the crate name in the manifest.
            if lc.contains("tokio") {
                add("Tokio");
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
        // JVM dependency manifests. These were not matched at ALL before, so the `java:spring`
        // domain (9 rules) had no detection path for any repo (W4/A6).
        "pom.xml" | "build.gradle" | "build.gradle.kts" | "build.sbt" => {
            if lc.contains("org.springframework") || lc.contains("spring-boot") {
                add("Spring");
            }
        }
        _ => {
            if file.ends_with(".csproj") || file.ends_with(".sln") || file.ends_with(".fsproj") {
                add(".NET");
                if lc.contains("microsoft.aspnetcore") {
                    add("ASP.NET");
                }
                // EF Core is the dominant .NET data layer — drives `csharp:efcore` (8 rules
                // that previously had no detection path at all).
                if lc.contains("microsoft.entityframeworkcore") {
                    add("Entity Framework Core");
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
    // Supabase: the CLI project config or a migration file under `supabase/` is a
    // reliable structural marker independent of any package.json dependency (a repo
    // can point at a Supabase Postgres instance with only the CLI + raw SQL, no JS
    // client at all). Either marker alone is sufficient.
    if path.contains("supabase/config.toml") {
        add("Supabase");
    }
    if path.contains("supabase/migrations/") && file.ends_with(".sql") {
        add("Supabase");
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

// ── The stack → corpus-domain mapping tables (Pass-1 W4, requirement H1 / finding A6) ───────
//
// `domains_for_stack` used to be a hand-written `match` with one arm per language and one per
// framework. That shape had no way to answer the only question H1 actually cares about — "can
// EVERY domain in the corpus be emitted by SOME stack?" — because the answer was spread across
// ~40 unrelated match arms with no enumerable union. 94 of 417 rules (23%) sat in corpus folders
// (`javascript/angular`, `javascript/vue`, `javascript/nest`, `rust/axum`, `rust/tokio`,
// `rust/sqlx`, `go/web`, `go/gorm`, `go/grpc`, `java/spring`, `ruby/rails`,
// `csharp/aspnetcore`, `csharp/efcore`, `concurrency`, `integration`, `process`) that no arm
// ever named, so they could never arm for ANY repo, ever.
//
// The GENERAL fix is not "add 16 more match arms" — that is the same defect with a bigger
// surface, and the next corpus folder someone adds is orphaned again the same silent way. It is
// to make the mapping DECLARATIVE DATA with a single computable union
// ([`emittable_domains`]), and then gate the build on `corpus domains ⊆ emittable domains`
// (`req_h1_every_corpus_domain_is_emittable_by_some_stack`) plus a per-domain witness repo that
// proves the DETECTION half too (`req_a6_every_corpus_domain_has_a_detectable_witness_repo`).
// With those two gates, adding a `crates/rules/principles/<x>/` folder without wiring its
// detection FAILS THE BUILD instead of silently shipping ~N unrunnable rules.

/// Corpus domains emitted for EVERY repo regardless of stack, because their rules are about
/// facts that are true of any code repository:
/// - `testing` — the cross-language test-pyramid/AAA/determinism principles.
/// - `process` — commit-message, branch-naming and work-item-link hygiene.
/// - `concurrency` — today a single rule about the PR template carrying a concurrency story
///   (`PROC-PR-CONCURRENCY-1`); a process fact, not a language fact.
pub(crate) const ALWAYS_ON_DOMAINS: &[&str] = &["testing", "process", "concurrency"];

/// Corpus domains the proposal pipeline selects WITHOUT consulting `domains_for_stack` at all,
/// so their absence from a repo's domain list is CORRECT, not an orphan:
/// - `universal` — `propose_corpus_rules` binds every `universal` rule to all repos by id.
/// - `agentic` — always recommended by design (it governs how the AI fleet builds, which is
///   independent of the scanned stack); see `propose_corpus_rules`'s `r.domain == "agentic"`.
///
/// [`emittable_domains`] includes these so the H1 gate measures "can this rule ever be
/// selected" rather than the narrower "does `domains_for_stack` name it".
#[cfg(test)]
pub(crate) const PIPELINE_SELECTED_DOMAINS: &[&str] = &["universal", "agentic"];

/// Corpus domains implied by the presence of `api-layer` — i.e. any repo with a backend API
/// surface:
/// - `permissions` — an app with an API almost certainly enforces authorization.
/// - `integration` — consumer/producer seams (route shapes, event wiring, the UI-affordance →
///   guarded-endpoint map) only exist where there IS an API surface. Note the single-repo case
///   is real: `INTEGRATION-AUTH-SEAM-1` and `INTEGRATION-EVENT-WIRING-1` are intra-repo seams.
pub(crate) const API_SURFACE_DOMAINS: &[&str] = &["permissions", "integration"];

/// LANGUAGE → corpus domains. The `<lang>:testing` domain is added separately by
/// [`testing_domain_for_language`] (kept apart because it is keyed on the same label but is a
/// different KIND of claim — "this language has an idiomatic testing corpus").
const LANGUAGE_DOMAINS: &[(&str, &[&str])] = &[
    // The corpus has a `javascript` family (javascript, javascript:typescript, :react,
    // :redux, :express, :next, :nest, :vue, :angular). Map the language to its own domain so
    // those baseline rules are suggested; the child → parent expansion in `domains_for_stack`
    // adds plain `javascript` whenever a `javascript:*` framework domain is present.
    ("JavaScript", &["javascript", "fullstack", "api-layer"]),
    (
        "TypeScript",
        &["javascript:typescript", "fullstack", "api-layer"],
    ),
    ("Rust", &["rust", "api-layer"]),
    // Python is overwhelmingly a backend/data-layer language: its own `python` baseline domain
    // (typing/idiom/web-API rules), the cross-language `api-layer` architecture rules, and the
    // generic `sql` rules (raw SQL via f-string is a textbook Python footgun the deterministic
    // floor catches). Framework specifics (FastAPI/Django/Flask/SQLAlchemy) come from
    // `FRAMEWORK_DOMAINS` as `python:*` child domains.
    ("Python", &["python", "api-layer", "sql"]),
    // A repo with hand-written .sql files clearly has a SQL surface.
    ("SQL", &["sql"]),
    // The remaining corpus-backed backend languages each own a baseline domain. These were
    // previously reached only as a SIDE EFFECT of the `<lang>:testing` child → parent
    // expansion, which worked but meant the baseline domain's presence depended on an
    // unrelated table — stated outright here instead.
    ("Go", &["go", "api-layer"]),
    ("Java", &["java", "api-layer"]),
    ("C#", &["csharp", "api-layer"]),
    ("Ruby", &["ruby", "api-layer"]),
];

/// Domains for a language with no [`LANGUAGE_DOMAINS`] row (Kotlin, PHP, Swift, C, C++): the
/// cross-language API-layer architecture rules, which is what the old `_ =>` arm did.
const DEFAULT_LANGUAGE_DOMAINS: &[&str] = &["api-layer"];

/// FRAMEWORK (as labelled by [`detect_frameworks`]) → corpus domains. Every label this table
/// names must be producible by `detect_frameworks` for SOME input — proved by
/// `req_a6_every_framework_label_in_the_mapping_table_is_detectable`, so a row can never claim
/// a domain is reachable via a framework nothing detects.
const FRAMEWORK_DOMAINS: &[(&str, &[&str])] = &[
    // ── Rust ────────────────────────────────────────────────────────────────────────────────
    ("Dioxus", &["rust:dioxus", "ui"]),
    ("Leptos", &["ui"]),
    ("Axum", &["rust:axum", "api-layer"]),
    ("Actix", &["api-layer"]),
    // The async runtime: bounded channels, blocking-in-async, std::sync::Mutex across await.
    ("Tokio", &["rust:tokio"]),
    // SeaORM is the only data layer that maps to the SeaORM-specific domain (`rust:seaorm`
    // holds entity-pattern + SeaORM-raw-SQL rules). Diesel is NOT SeaORM — proposing
    // entity/SeaORM rules for a Diesel repo is a misfire (#52). sqlx now has its OWN domain
    // (`rust:sqlx`: compile-checked queries, pool sizing, explicit transactions) rather than
    // being folded in with Diesel. Both still get the generic SQL + migration-hygiene rules.
    ("SeaORM", &["rust:seaorm", "sql", "ci-cd"]),
    ("sqlx", &["rust:sqlx", "sql", "ci-cd"]),
    ("Diesel", &["sql", "ci-cd"]),
    // ── JavaScript / TypeScript ─────────────────────────────────────────────────────────────
    ("Next.js", &["javascript:next", "fullstack", "ui"]),
    ("React", &["javascript:react", "ui", "fullstack"]),
    ("Redux", &["javascript:redux", "fullstack"]),
    ("Vue", &["javascript:vue", "ui", "fullstack"]),
    ("Angular", &["javascript:angular", "ui", "fullstack"]),
    // Svelte has no corpus domain of its own yet; it still gets the cross-framework UI rules.
    ("Svelte", &["ui", "fullstack"]),
    ("Express", &["javascript:express", "api-layer"]),
    ("NestJS", &["javascript:nest", "api-layer"]),
    // ── Python ──────────────────────────────────────────────────────────────────────────────
    // Each web framework maps to its `python:*` child domain (which pulls in the `python`
    // baseline via the child → parent expansion) plus the cross-language `api-layer` rules.
    ("FastAPI", &["python:fastapi", "api-layer"]),
    ("Django", &["python:django", "api-layer"]),
    ("Flask", &["python:flask", "api-layer"]),
    // SQLAlchemy is a Python data layer: `python` baseline plus generic SQL + migration
    // hygiene (same shape as sqlx/Diesel). Pydantic is the typed-model boundary library.
    ("SQLAlchemy", &["python", "sql", "ci-cd"]),
    ("Pydantic", &["python"]),
    // ── Go ──────────────────────────────────────────────────────────────────────────────────
    ("Gin", &["go:web", "api-layer"]),
    ("Echo", &["go:web", "api-layer"]),
    ("Chi", &["go:web", "api-layer"]),
    ("Fiber", &["go:web", "api-layer"]),
    ("Gorilla Mux", &["go:web", "api-layer"]),
    ("GORM", &["go:gorm", "sql", "ci-cd"]),
    ("gRPC-Go", &["go:grpc", "api-layer"]),
    // ── JVM ─────────────────────────────────────────────────────────────────────────────────
    ("Spring", &["java:spring", "api-layer"]),
    // ── .NET ────────────────────────────────────────────────────────────────────────────────
    ("ASP.NET", &["csharp:aspnetcore", "api-layer"]),
    ("Entity Framework Core", &["csharp:efcore", "sql", "ci-cd"]),
    // ── Ruby ────────────────────────────────────────────────────────────────────────────────
    ("Rails", &["ruby:rails", "api-layer"]),
    // ── Supabase ────────────────────────────────────────────────────────────────────────────
    // Every `supabase:*` child domain in the corpus (crates/rules/principles/supabase/{rls,
    // auth,secrets,storage,database-functions,exposure,realtime}/), plus `sql` since migrations
    // are raw SQL. `propose_corpus_rules` matches on EXACT domain string (repo domain ==
    // rule.domain), so the parent `supabase` alone would match nothing — every rule's domain is
    // a `supabase:<area>` leaf. Each leaf is listed explicitly; a new `supabase/<area>/` folder
    // with no row here fails `req_h1_every_corpus_domain_is_emittable_by_some_stack`.
    (
        "Supabase",
        &[
            "supabase:rls",
            "supabase:auth",
            "supabase:secrets",
            "supabase:storage",
            "supabase:database-functions",
            "supabase:exposure",
            "supabase:realtime",
            "sql",
        ],
    ),
    // ── Infrastructure-as-code → `iac` ──────────────────────────────────────────────────────
    ("Terraform", &["iac"]),
    ("Terragrunt", &["iac"]),
    ("Bicep", &["iac"]),
    ("Pulumi", &["iac"]),
    ("CloudFormation", &["iac"]),
    // ── CI/CD platforms → `ci-cd` ───────────────────────────────────────────────────────────
    ("GitHub Actions", &["ci-cd"]),
    ("GitLab CI", &["ci-cd"]),
    ("CircleCI", &["ci-cd"]),
    ("Azure Pipelines", &["ci-cd"]),
    ("Travis CI", &["ci-cd"]),
    ("Bitbucket Pipelines", &["ci-cd"]),
    ("Drone CI", &["ci-cd"]),
    ("Jenkins", &["ci-cd"]),
];

/// The PARENT of every namespaced domain in `domains`, added in place. A child domain ALWAYS
/// implies its parent: recommending `javascript:next` without `javascript` is incoherent (the
/// framework rules sit on top of the language baseline) and reads as a bug in the UI (child
/// ticked, parent not).
fn expand_parent_domains(domains: &mut std::collections::BTreeSet<&'static str>) {
    // The split borrows from the 'static keys, so the parent stays `&'static str`.
    let parents: Vec<&'static str> = domains
        .iter()
        .filter_map(|d| d.split_once(':').map(|(p, _)| p))
        .collect();
    for p in parents {
        domains.insert(p);
    }
}

/// EVERY corpus domain [`domains_for_stack`] can emit, across all possible stacks, PLUS the
/// domains the proposal pipeline selects without it ([`PIPELINE_SELECTED_DOMAINS`]).
///
/// This is the union the H1 build gate diffs the real corpus against: a domain present under
/// `crates/rules/principles/` but absent here is a rule family that can never arm for any repo
/// — the A6 defect class. Derived from the SAME tables `domains_for_stack` reads, so the two
/// can never drift; the gate is still load-bearing because the thing it compares against (the
/// corpus folder layout) is independent data this file knows nothing about.
#[cfg(test)]
pub(crate) fn emittable_domains() -> std::collections::BTreeSet<&'static str> {
    let mut domains: std::collections::BTreeSet<&'static str> = std::collections::BTreeSet::new();
    domains.extend(ALWAYS_ON_DOMAINS.iter().copied());
    domains.extend(PIPELINE_SELECTED_DOMAINS.iter().copied());
    domains.extend(API_SURFACE_DOMAINS.iter().copied());
    domains.extend(DEFAULT_LANGUAGE_DOMAINS.iter().copied());
    for (_, ds) in LANGUAGE_DOMAINS {
        domains.extend(ds.iter().copied());
    }
    for (_, ds) in FRAMEWORK_DOMAINS {
        domains.extend(ds.iter().copied());
    }
    for lang in LANGUAGE_DOMAINS.iter().map(|(l, _)| *l) {
        if let Some(t) = testing_domain_for_language(lang) {
            domains.insert(t);
        }
    }
    expand_parent_domains(&mut domains);
    domains
}

/// Every framework label [`FRAMEWORK_DOMAINS`] maps. Used by the witness gate to assert each
/// one is actually producible by [`detect_frameworks`] — a mapping row for a label nothing
/// detects would make `emittable_domains` claim reachability it does not have.
#[cfg(test)]
pub(crate) fn mapped_framework_labels() -> Vec<&'static str> {
    FRAMEWORK_DOMAINS.iter().map(|(fw, _)| *fw).collect()
}

/// The corpus domains ONE repo's stack maps to. Used to bind each rule to only the
/// repos whose domain it applies to (minimum domains per repo).
pub(crate) fn domains_for_stack(s: &RepoStack) -> Vec<String> {
    let mut domains: std::collections::BTreeSet<&'static str> = std::collections::BTreeSet::new();
    for lang in &s.languages {
        match LANGUAGE_DOMAINS.iter().find(|(l, _)| *l == lang.as_str()) {
            Some((_, ds)) => domains.extend(ds.iter().copied()),
            // A language with no row maps to the cross-language API-layer rules.
            None => domains.extend(DEFAULT_LANGUAGE_DOMAINS.iter().copied()),
        }
        // Suggest the language's idiomatic testing corpus whenever the language is present.
        if let Some(t) = testing_domain_for_language(lang) {
            domains.insert(t);
        }
    }
    for fw in &s.frameworks {
        if let Some((_, ds)) = FRAMEWORK_DOMAINS.iter().find(|(f, _)| *f == fw.as_str()) {
            domains.extend(ds.iter().copied());
        }
    }
    // Domains implied by having a backend API surface at all.
    if domains.contains("api-layer") {
        domains.extend(API_SURFACE_DOMAINS.iter().copied());
    }
    // Principles that hold for every repo whatever its stack.
    domains.extend(ALWAYS_ON_DOMAINS.iter().copied());
    expand_parent_domains(&mut domains);
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
            let matched_repos: Vec<String> = if r.domain == "universal" {
                all_repos.clone()
            } else {
                // A repo matches when its domain set contains the rule's own folder-derived
                // `domain` OR any of the rule's `extra_domains` (e.g. SUPABASE-FUNC-SEARCH-
                // PATH-1 lives under `supabase:database-functions` but also carries
                // `extra_domains = ["sql"]` so a plain-Postgres repo — no Supabase layout, just
                // hand-written `.sql` — still proposes+arms it; see Rule::extra_domains).
                repo_domains
                    .iter()
                    .filter(|(_, ds)| {
                        ds.iter().any(|d| d == &r.domain || r.extra_domains.iter().any(|ed| ed == d))
                    })
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
                    violates: o.violates,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic repo: `(path, content)` pairs, built the shape `detect_stack` consumes.
    fn files(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(p, c)| (p.to_string(), c.to_string()))
            .collect()
    }

    /// Every distinct `domain` present in the REAL shipped corpus, derived the same way
    /// `camerata_rules::load_corpus_lenient` derives it (folder path → `a:b`). Returns `None`
    /// when the bundled corpus isn't checked out, so these gates skip rather than fail in a
    /// corpus-less build — matching `camerata_rules`'s own corpus-gate tests.
    async fn corpus_domains() -> Option<std::collections::BTreeSet<String>> {
        let path = camerata_rules::corpus_path();
        if !path.exists() {
            return None;
        }
        let (set, _errs) = camerata_rules::load_corpus_lenient(&path).await;
        Some(set.iter().map(|r| r.domain.clone()).collect())
    }

    // ── req_h1 / req_a6: the "every rule can run" build gate ────────────────────────────────
    //
    // Requirement H1 (`docs/plans/2026-10-09_audit-findings-tracker.md`): "every rule must be
    // CAPABLE of running. A rule that does not run because it is irrelevant to the scanned
    // stack is CORRECT. A rule that could never run for ANY stack is a DEFECT."
    //
    // Finding A6 was 94 of 417 rules (23%) sitting in corpus folders no `domains_for_stack`
    // arm ever named. The individual mappings are today's instance; THESE TWO GATES are the
    // deliverable, because they make the next instance a build failure:
    //
    //   1. `req_h1_every_corpus_domain_is_emittable_by_some_stack` — the DOMAIN half. Diffs
    //      the corpus folder layout (data this module knows nothing about) against
    //      `emittable_domains()`.
    //   2. `req_a6_every_corpus_domain_has_a_detectable_witness_repo` — the DETECTION half. A
    //      mapping row is worthless if no real repo can produce the framework label it keys
    //      on, so each domain must be reachable end-to-end through the production pipeline
    //      (`detect_stack` → `domains_for_stack`) from a synthetic repo.
    //
    // Gate 1 alone could be satisfied by a mapping row for a label nothing detects; gate 2
    // alone could be satisfied by witnesses that skip a corpus folder. Together they can't.

    /// req_h1: every domain in the shipped corpus must be in the union `domains_for_stack` can
    /// emit (plus the two the proposal pipeline selects without it — see
    /// `PIPELINE_SELECTED_DOMAINS`).
    ///
    /// FAILS BEFORE THE FIX: on unmodified HEAD this reported 16 orphaned domains covering 94
    /// rules — `concurrency` (1), `csharp:aspnetcore` (8), `csharp:efcore` (7), `go:gorm` (5),
    /// `go:grpc` (5), `go:web` (5), `integration` (3), `java:spring` (9),
    /// `javascript:angular` (11), `javascript:nest` (5), `javascript:vue` (6), `process` (4),
    /// `ruby:rails` (10), `rust:axum` (6), `rust:sqlx` (4), `rust:tokio` (5). Verified by
    /// enumerating `crates/rules/principles/**` against the pre-change match arms.
    #[tokio::test]
    async fn req_h1_every_corpus_domain_is_emittable_by_some_stack() {
        let Some(corpus) = corpus_domains().await else {
            return;
        };
        let emittable = emittable_domains();
        let orphaned: Vec<&str> = corpus
            .iter()
            .map(String::as_str)
            .filter(|d| !emittable.contains(d))
            .collect();
        assert!(
            orphaned.is_empty(),
            "H1: these corpus domains can never be emitted for ANY stack, so every rule in \
             them is permanently unrunnable: {orphaned:?}\nemittable: {emittable:?}"
        );
    }

    /// One synthetic repo per corpus domain, proving that domain is reachable END TO END
    /// through the production pipeline. Deliberately generic: every witness is a manifest
    /// marker or file extension that identifies the language/framework in general (an
    /// `@angular/core` dependency, a `gorm.io/gorm` require line), never a path, table name or
    /// string lifted from any fixture or benchmark repo.
    ///
    /// `universal` and `agentic` are absent on purpose — `propose_corpus_rules` selects those
    /// without consulting `domains_for_stack` at all (see `PIPELINE_SELECTED_DOMAINS`), so
    /// they are covered by `req_a6_pipeline_selected_domains_bind_without_a_stack_match`.
    const DOMAIN_WITNESSES: &[(&str, &[(&str, &str)])] = &[
        // ── always-on: true of any repo whatever its stack ──────────────────────────────────
        ("testing", &[("README.md", "")]),
        ("process", &[("README.md", "")]),
        ("concurrency", &[("README.md", "")]),
        // ── JavaScript / TypeScript ─────────────────────────────────────────────────────────
        ("javascript", &[("src/app.js", "")]),
        ("javascript:typescript", &[("src/app.ts", "")]),
        ("javascript:testing", &[("src/app.ts", "")]),
        (
            "javascript:react",
            &[("src/app.jsx", ""), ("package.json", r#"{"dependencies":{"react":"18.2.0"}}"#)],
        ),
        (
            "javascript:next",
            &[("src/app.tsx", ""), ("package.json", r#"{"dependencies":{"next":"14.0.0"}}"#)],
        ),
        (
            "javascript:redux",
            &[("src/store.ts", ""), ("package.json", r#"{"dependencies":{"redux":"5.0.0"}}"#)],
        ),
        (
            "javascript:express",
            &[("src/server.js", ""), ("package.json", r#"{"dependencies":{"express":"4.19.0"}}"#)],
        ),
        (
            "javascript:nest",
            &[
                ("src/main.ts", ""),
                ("package.json", r#"{"dependencies":{"@nestjs/core":"10.0.0"}}"#),
            ],
        ),
        (
            "javascript:vue",
            &[("src/app.ts", ""), ("package.json", r#"{"dependencies":{"vue":"3.4.0"}}"#)],
        ),
        (
            "javascript:angular",
            &[
                ("src/main.ts", ""),
                ("package.json", r#"{"dependencies":{"@angular/core":"17.0.0"}}"#),
            ],
        ),
        ("fullstack", &[("src/app.js", "")]),
        ("api-layer", &[("src/app.js", "")]),
        ("permissions", &[("src/app.js", "")]),
        ("integration", &[("src/app.js", "")]),
        (
            "ui",
            &[("src/app.jsx", ""), ("package.json", r#"{"dependencies":{"react":"18.2.0"}}"#)],
        ),
        // ── Rust ────────────────────────────────────────────────────────────────────────────
        ("rust", &[("src/main.rs", "")]),
        ("rust:testing", &[("src/main.rs", "")]),
        (
            "rust:dioxus",
            &[("src/main.rs", ""), ("Cargo.toml", "[dependencies]\ndioxus = \"0.5\"\n")],
        ),
        (
            "rust:seaorm",
            &[("src/main.rs", ""), ("Cargo.toml", "[dependencies]\nsea-orm = \"1.0\"\n")],
        ),
        (
            "rust:sqlx",
            &[("src/main.rs", ""), ("Cargo.toml", "[dependencies]\nsqlx = \"0.8\"\n")],
        ),
        (
            "rust:axum",
            &[("src/main.rs", ""), ("Cargo.toml", "[dependencies]\naxum = \"0.7\"\n")],
        ),
        (
            "rust:tokio",
            &[("src/main.rs", ""), ("Cargo.toml", "[dependencies]\ntokio = \"1\"\n")],
        ),
        // ── Python ──────────────────────────────────────────────────────────────────────────
        ("python", &[("app.py", "")]),
        ("python:testing", &[("app.py", "")]),
        ("python:fastapi", &[("app.py", ""), ("requirements.txt", "fastapi==0.110.0\n")]),
        ("python:django", &[("app.py", ""), ("requirements.txt", "django==5.0\n")]),
        ("python:flask", &[("app.py", ""), ("requirements.txt", "flask==3.0\n")]),
        // ── Go ──────────────────────────────────────────────────────────────────────────────
        ("go", &[("main.go", "")]),
        ("go:testing", &[("main.go", "")]),
        (
            "go:web",
            &[
                ("main.go", ""),
                ("go.mod", "module example\n\nrequire github.com/gin-gonic/gin v1.9.1\n"),
            ],
        ),
        (
            "go:gorm",
            &[("main.go", ""), ("go.mod", "module example\n\nrequire gorm.io/gorm v1.25.0\n")],
        ),
        (
            "go:grpc",
            &[
                ("main.go", ""),
                ("go.mod", "module example\n\nrequire google.golang.org/grpc v1.62.0\n"),
            ],
        ),
        // ── JVM ─────────────────────────────────────────────────────────────────────────────
        ("java", &[("src/Main.java", "")]),
        ("java:testing", &[("src/Main.java", "")]),
        (
            "java:spring",
            &[
                ("src/Main.java", ""),
                (
                    "pom.xml",
                    "<dependency><groupId>org.springframework.boot</groupId>\
                     <artifactId>spring-boot-starter-web</artifactId></dependency>",
                ),
            ],
        ),
        // ── .NET ────────────────────────────────────────────────────────────────────────────
        ("csharp", &[("Program.cs", "")]),
        ("csharp:testing", &[("Program.cs", "")]),
        (
            "csharp:aspnetcore",
            &[
                ("Program.cs", ""),
                ("App.csproj", "<PackageReference Include=\"Microsoft.AspNetCore.App\" />"),
            ],
        ),
        (
            "csharp:efcore",
            &[
                ("Program.cs", ""),
                (
                    "App.csproj",
                    "<PackageReference Include=\"Microsoft.EntityFrameworkCore\" Version=\"8.0.0\" />",
                ),
            ],
        ),
        // ── Ruby ────────────────────────────────────────────────────────────────────────────
        ("ruby", &[("app.rb", "")]),
        ("ruby:testing", &[("app.rb", "")]),
        ("ruby:rails", &[("app.rb", ""), ("Gemfile", "gem \"rails\", \"~> 7.1\"\n")]),
        // ── Data / infra / CI ───────────────────────────────────────────────────────────────
        ("sql", &[("db/schema.sql", "create table t (id int);")]),
        ("ci-cd", &[(".github/workflows/ci.yml", "on: push\n")]),
        ("iac", &[("main.tf", "resource \"null_resource\" \"a\" {}\n")]),
        // ── Supabase: the CLI project config alone is a sufficient structural marker ─────────
        // (There is no bare `supabase` row: no rule lives directly in
        // `crates/rules/principles/supabase/`, so it is not a corpus domain —
        // `req_a6_no_witness_names_a_domain_the_corpus_does_not_have` caught exactly that.)
        ("supabase:rls", &[("supabase/config.toml", "project_id = \"example\"\n")]),
        ("supabase:auth", &[("supabase/config.toml", "project_id = \"example\"\n")]),
        ("supabase:secrets", &[("supabase/config.toml", "project_id = \"example\"\n")]),
        ("supabase:storage", &[("supabase/config.toml", "project_id = \"example\"\n")]),
        (
            "supabase:database-functions",
            &[("supabase/config.toml", "project_id = \"example\"\n")],
        ),
        ("supabase:exposure", &[("supabase/config.toml", "project_id = \"example\"\n")]),
        ("supabase:realtime", &[("supabase/config.toml", "project_id = \"example\"\n")]),
    ];

    /// req_a6: every corpus domain has a witness repo in [`DOMAIN_WITNESSES`], AND that witness
    /// really does produce the domain through the PRODUCTION pipeline (`detect_stack` →
    /// `domains_for_stack`) — not through the mapping table read directly.
    ///
    /// FAILS BEFORE THE FIX: the 16 orphaned domains listed on
    /// `req_h1_every_corpus_domain_is_emittable_by_some_stack` had no witness that could
    /// produce them, and for 7 of them (`javascript:nest`, `rust:tokio`, `java:spring`,
    /// `csharp:efcore`, `go:web`, `go:gorm`, `go:grpc`) `detect_frameworks` had no marker at
    /// all, so no witness COULD have been written without also adding detection.
    #[tokio::test]
    async fn req_a6_every_corpus_domain_has_a_detectable_witness_repo() {
        let Some(corpus) = corpus_domains().await else {
            return;
        };
        let mut missing_witness: Vec<&str> = Vec::new();
        let mut not_emitted: Vec<String> = Vec::new();
        for domain in corpus.iter().map(String::as_str) {
            if PIPELINE_SELECTED_DOMAINS.contains(&domain) {
                continue;
            }
            let Some((_, witness)) = DOMAIN_WITNESSES.iter().find(|(d, _)| *d == domain) else {
                missing_witness.push(domain);
                continue;
            };
            let repo_files = files(witness);
            let stack = detect_stack(&format!("witness/{domain}"), &repo_files);
            let emitted = domains_for_stack(&stack);
            if !emitted.iter().any(|d| d == domain) {
                not_emitted.push(format!(
                    "{domain}: witness detected as {stack:?} which emitted {emitted:?}"
                ));
            }
        }
        assert!(
            missing_witness.is_empty(),
            "A6: these corpus domains have no witness repo proving any real repo can reach \
             them — add detection + a witness, or the rules in them can never arm: \
             {missing_witness:?}"
        );
        assert!(
            not_emitted.is_empty(),
            "A6: these witnesses did NOT produce their domain through detect_stack -> \
             domains_for_stack: {not_emitted:#?}"
        );
    }

    /// req_a6 (anti-rot): no witness in [`DOMAIN_WITNESSES`] names a domain the corpus no
    /// longer has. Without this the witness table silently accumulates entries for deleted
    /// folders and stops being a readable statement of what the corpus covers.
    #[tokio::test]
    async fn req_a6_no_witness_names_a_domain_the_corpus_does_not_have() {
        let Some(corpus) = corpus_domains().await else {
            return;
        };
        let stale: Vec<&str> = DOMAIN_WITNESSES
            .iter()
            .map(|(d, _)| *d)
            .filter(|d| !corpus.contains(*d))
            .collect();
        assert!(
            stale.is_empty(),
            "witnesses for domains not in the corpus: {stale:?}"
        );
    }

    /// One manifest file per framework label [`FRAMEWORK_DOMAINS`] maps: `(label, path,
    /// content)`. A mapping row keyed on a label `detect_frameworks` can never produce would
    /// make `emittable_domains` claim reachability the pipeline does not have — the subtler
    /// half of A6, and the reason gate 1 alone is not enough.
    const FRAMEWORK_WITNESSES: &[(&str, &str, &str)] = &[
        ("Dioxus", "Cargo.toml", "[dependencies]\ndioxus = \"0.5\"\n"),
        ("Leptos", "Cargo.toml", "[dependencies]\nleptos = \"0.6\"\n"),
        ("Axum", "Cargo.toml", "[dependencies]\naxum = \"0.7\"\n"),
        ("Actix", "Cargo.toml", "[dependencies]\nactix-web = \"4\"\n"),
        ("Tokio", "Cargo.toml", "[dependencies]\ntokio = \"1\"\n"),
        (
            "SeaORM",
            "Cargo.toml",
            "[dependencies]\nsea-orm = \"1.0\"\n",
        ),
        ("sqlx", "Cargo.toml", "[dependencies]\nsqlx = \"0.8\"\n"),
        ("Diesel", "Cargo.toml", "[dependencies]\ndiesel = \"2\"\n"),
        (
            "Next.js",
            "package.json",
            r#"{"dependencies":{"next":"14.0.0"}}"#,
        ),
        (
            "React",
            "package.json",
            r#"{"dependencies":{"react":"18.2.0"}}"#,
        ),
        (
            "Redux",
            "package.json",
            r#"{"dependencies":{"redux":"5.0.0"}}"#,
        ),
        ("Vue", "package.json", r#"{"dependencies":{"vue":"3.4.0"}}"#),
        (
            "Angular",
            "package.json",
            r#"{"dependencies":{"@angular/core":"17.0.0"}}"#,
        ),
        (
            "Svelte",
            "package.json",
            r#"{"dependencies":{"svelte":"4.2.0"}}"#,
        ),
        (
            "Express",
            "package.json",
            r#"{"dependencies":{"express":"4.19.0"}}"#,
        ),
        (
            "NestJS",
            "package.json",
            r#"{"dependencies":{"@nestjs/core":"10.0.0"}}"#,
        ),
        ("FastAPI", "requirements.txt", "fastapi==0.110.0\n"),
        ("Django", "requirements.txt", "django==5.0\n"),
        ("Flask", "requirements.txt", "flask==3.0\n"),
        ("SQLAlchemy", "requirements.txt", "sqlalchemy==2.0.29\n"),
        ("Pydantic", "requirements.txt", "pydantic==2.6.0\n"),
        (
            "Gin",
            "go.mod",
            "module example\n\nrequire github.com/gin-gonic/gin v1.9.1\n",
        ),
        (
            "Echo",
            "go.mod",
            "module example\n\nrequire github.com/labstack/echo/v4 v4.11.0\n",
        ),
        (
            "Chi",
            "go.mod",
            "module example\n\nrequire github.com/go-chi/chi/v5 v5.0.12\n",
        ),
        (
            "Fiber",
            "go.mod",
            "module example\n\nrequire github.com/gofiber/fiber/v2 v2.52.0\n",
        ),
        (
            "Gorilla Mux",
            "go.mod",
            "module example\n\nrequire github.com/gorilla/mux v1.8.1\n",
        ),
        (
            "GORM",
            "go.mod",
            "module example\n\nrequire gorm.io/gorm v1.25.0\n",
        ),
        (
            "gRPC-Go",
            "go.mod",
            "module example\n\nrequire google.golang.org/grpc v1.62.0\n",
        ),
        (
            "Spring",
            "pom.xml",
            "<dependency><groupId>org.springframework.boot</groupId></dependency>",
        ),
        (
            "ASP.NET",
            "App.csproj",
            "<PackageReference Include=\"Microsoft.AspNetCore.App\" />",
        ),
        (
            "Entity Framework Core",
            "App.csproj",
            "<PackageReference Include=\"Microsoft.EntityFrameworkCore\" Version=\"8.0.0\" />",
        ),
        ("Rails", "Gemfile", "gem \"rails\", \"~> 7.1\"\n"),
        (
            "Supabase",
            "supabase/config.toml",
            "project_id = \"example\"\n",
        ),
        (
            "Terraform",
            "main.tf",
            "resource \"null_resource\" \"a\" {}\n",
        ),
        (
            "Terragrunt",
            "terragrunt.hcl",
            "terraform { source = \".\" }\n",
        ),
        ("Bicep", "main.bicep", "param location string\n"),
        ("Pulumi", "Pulumi.yaml", "name: example\nruntime: nodejs\n"),
        (
            "CloudFormation",
            "template.yaml",
            "AWSTemplateFormatVersion: '2010-09-09'\nResources: {}\n",
        ),
        ("GitHub Actions", ".github/workflows/ci.yml", "on: push\n"),
        ("GitLab CI", ".gitlab-ci.yml", "stages: [build]\n"),
        ("CircleCI", ".circleci/config.yml", "version: 2.1\n"),
        (
            "Azure Pipelines",
            "azure-pipelines.yml",
            "trigger: [main]\n",
        ),
        ("Travis CI", ".travis.yml", "language: node_js\n"),
        (
            "Bitbucket Pipelines",
            "bitbucket-pipelines.yml",
            "pipelines: {}\n",
        ),
        ("Drone CI", ".drone.yml", "kind: pipeline\n"),
        ("Jenkins", "Jenkinsfile", "pipeline { agent any }\n"),
    ];

    /// req_a6: every framework label in the mapping table is producible by
    /// `detect_frameworks`, and every witness really produces its own label.
    ///
    /// FAILS BEFORE THE FIX: `NestJS`, `Tokio`, `Spring`, `Entity Framework Core`, `Gin`,
    /// `Echo`, `Chi`, `Fiber`, `Gorilla Mux`, `GORM` and `gRPC-Go` had no marker in
    /// `detect_frameworks` at all — `pom.xml`/`build.gradle` were not even matched as
    /// manifests, and `go.mod` only ever emitted the bare label "Go modules".
    #[test]
    fn req_a6_every_framework_label_in_the_mapping_table_is_detectable() {
        let missing_witness: Vec<&str> = mapped_framework_labels()
            .into_iter()
            .filter(|fw| !FRAMEWORK_WITNESSES.iter().any(|(label, _, _)| label == fw))
            .collect();
        assert!(
            missing_witness.is_empty(),
            "A6: FRAMEWORK_DOMAINS maps these labels but no witness proves detect_frameworks \
             can produce them: {missing_witness:?}"
        );
        let mut undetected: Vec<String> = Vec::new();
        for (label, path, content) in FRAMEWORK_WITNESSES {
            let mut out = std::collections::BTreeSet::new();
            detect_frameworks(path, content, &mut out);
            if !out.contains(*label) {
                undetected.push(format!("{label}: {path} detected {out:?}"));
            }
        }
        assert!(
            undetected.is_empty(),
            "A6: these witnesses did not produce their own framework label: {undetected:#?}"
        );
    }

    /// req_a6: `universal` and `agentic` rules bind WITHOUT any stack match, which is why they
    /// are excluded from the witness table. Proves the exclusion is real behaviour, not an
    /// unverified assumption that would let a genuinely orphaned domain hide behind it.
    #[tokio::test]
    async fn req_a6_pipeline_selected_domains_bind_without_a_stack_match() {
        let Some(corpus) = corpus_domains().await else {
            return;
        };
        // A repo whose stack maps to NONE of the pipeline-selected domains.
        let repo_files = files(&[("README.md", "# docs only")]);
        let stack = detect_stack("witness/pipeline-selected", &repo_files);
        let domains = domains_for_stack(&stack);
        for special in PIPELINE_SELECTED_DOMAINS {
            if !corpus.contains(*special) {
                continue;
            }
            assert!(
                !domains.iter().any(|d| d == special),
                "sanity: {special} must NOT come from domains_for_stack — if it does, it \
                 belongs in the witness table instead"
            );
        }
        let repo_domains = vec![("witness/pipeline-selected".to_string(), domains)];
        let proposed = propose_corpus_rules(&repo_domains).await;
        for special in PIPELINE_SELECTED_DOMAINS {
            if !corpus.contains(*special) {
                continue;
            }
            let bound: Vec<&ProposedRule> = proposed
                .iter()
                .filter(|r| &r.domain == special && !r.repos.is_empty())
                .collect();
            assert!(
                !bound.is_empty(),
                "{special} rules must bind to a repo with no matching stack domain — that is \
                 why they are exempt from the witness gate"
            );
        }
    }

    // ── targeted regression tests for the families A6 named ─────────────────────────────────

    /// req_a6: the exact shape the audit called out — a Nest + TypeScript API repo must arm the
    /// `javascript:nest` rules. Before the fix `detect_frameworks` had no `@nestjs/*` marker
    /// and `domains_for_stack` had no `javascript:nest` arm, so all 5 rules in that folder were
    /// unreachable for every repo that has ever been or will ever be scanned.
    #[tokio::test]
    async fn req_a6_a_nest_repo_arms_the_nest_rules() {
        let repo_files = files(&[
            ("src/main.ts", "import { NestFactory } from '@nestjs/core';"),
            (
                "package.json",
                r#"{"dependencies":{"@nestjs/core":"10.0.0"}}"#,
            ),
        ]);
        let stack = detect_stack("acme/api", &repo_files);
        assert!(
            stack.frameworks.contains(&"NestJS".to_string()),
            "{stack:?}"
        );
        let domains = domains_for_stack(&stack);
        assert!(
            domains.contains(&"javascript:nest".to_string()),
            "{domains:?}"
        );
        // The `javascript` baseline must come along via the child → parent expansion.
        assert!(domains.contains(&"javascript".to_string()), "{domains:?}");

        let repo_domains = vec![("acme/api".to_string(), domains)];
        let proposed = propose_corpus_rules(&repo_domains).await;
        let nest: Vec<&ProposedRule> = proposed
            .iter()
            .filter(|r| r.domain == "javascript:nest")
            .collect();
        assert!(
            !nest.is_empty(),
            "the corpus has javascript:nest rules to propose"
        );
        let unbound: Vec<&String> = nest
            .iter()
            .filter(|r| !r.repos.contains(&"acme/api".to_string()))
            .map(|r| &r.id)
            .collect();
        assert!(
            unbound.is_empty(),
            "every nest rule must bind to this repo: {unbound:?}"
        );
        let unarmed: Vec<&String> = nest
            .iter()
            .filter(|r| !r.is_auto_recommended)
            .map(|r| &r.id)
            .collect();
        assert!(
            unarmed.is_empty(),
            "every stack-matched, grounded nest rule must be pre-checked: {unarmed:?}"
        );
    }

    /// `ARCH-ONE-IMPORT-PATH-1` lives in the `javascript` domain, which a TypeScript repo
    /// reaches via `javascript:typescript` and a plain JavaScript repo reaches directly. No
    /// new wiring is needed; this proves it rather than assuming it. Also pins the negative:
    /// a Python-only repo must not be handed an ES-module re-export rule.
    #[tokio::test]
    async fn arch_one_import_path_is_proposed_for_typescript_and_javascript_stacks() {
        const ID: &str = "ARCH-ONE-IMPORT-PATH-1";
        for (repo, path) in [("acme/ts-app", "src/app.ts"), ("acme/js-app", "src/app.js")] {
            let repo_files = files(&[(path, "export const a = 1;")]);
            let stack = detect_stack(repo, &repo_files);
            let domains = domains_for_stack(&stack);
            assert!(
                domains.contains(&"javascript".to_string()),
                "{repo}: {domains:?}"
            );
            let proposed = propose_corpus_rules(&[(repo.to_string(), domains)]).await;
            let rule = proposed
                .iter()
                .find(|r| r.id == ID)
                .unwrap_or_else(|| panic!("{ID} must be proposed for {repo}"));
            assert_eq!(rule.domain, "javascript");
            assert!(rule.recommended, "{repo}: suggested, not merely available");
            assert!(
                rule.is_auto_recommended,
                "{repo}: a policy rule is still auto-recommended (pre-checked)"
            );
            assert_eq!(rule.repos, vec![repo.to_string()]);
        }

        let py_files = files(&[("app/main.py", "print('hi')")]);
        let stack = detect_stack("acme/py-app", &py_files);
        let domains = domains_for_stack(&stack);
        let proposed = propose_corpus_rules(&[("acme/py-app".to_string(), domains)]).await;
        let rule = proposed.iter().find(|r| r.id == ID);
        assert!(
            rule.is_none_or(|r| !r.recommended),
            "a Python-only repo must not have {ID} suggested: {rule:?}"
        );
    }

    /// `ARCH-SHARED-CONSTANT-1` lives in the `universal` domain, which `propose_corpus_rules`
    /// binds to every repo by id with no stack match. The literal extractor covers TypeScript,
    /// JavaScript, Python and Rust, so each of those stacks must be handed the rule, pre-checked
    /// (a policy rule is auto-recommended).
    #[tokio::test]
    async fn arch_shared_constant_is_proposed_for_typescript_python_and_rust_stacks() {
        const ID: &str = "ARCH-SHARED-CONSTANT-1";
        for (repo, path) in [
            ("acme/ts-app", "src/app.ts"),
            ("acme/js-app", "src/app.js"),
            ("acme/py-app", "app/main.py"),
            ("acme/rs-app", "src/main.rs"),
        ] {
            let repo_files = files(&[(path, "x")]);
            let stack = detect_stack(repo, &repo_files);
            let domains = domains_for_stack(&stack);
            let proposed = propose_corpus_rules(&[(repo.to_string(), domains)]).await;
            let rule = proposed
                .iter()
                .find(|r| r.id == ID)
                .unwrap_or_else(|| panic!("{ID} must be proposed for {repo}"));
            assert_eq!(rule.domain, "universal");
            assert!(rule.recommended, "{repo}: suggested, not merely available");
            assert!(
                rule.is_auto_recommended,
                "{repo}: a policy rule is still auto-recommended (pre-checked)"
            );
            assert_eq!(rule.repos, vec![repo.to_string()]);
        }
    }

    /// `ARCH-DEAD-CODE-1` lives in the `universal` domain, which is pipeline-selected and bound
    /// to every repo, so no stack wiring is needed; this proves it for the three languages the
    /// detector reads rather than assuming it. A `policy` rule is still auto-recommended.
    #[tokio::test]
    async fn arch_dead_code_is_proposed_for_typescript_python_and_rust_stacks() {
        const ID: &str = "ARCH-DEAD-CODE-1";
        for (repo, path, content) in [
            ("acme/ts-app", "src/app.ts", "export const a = 1;"),
            ("acme/py-app", "app/main.py", "print('hi')"),
            ("acme/rs-app", "src/lib.rs", "pub fn f() {}"),
        ] {
            let repo_files = files(&[(path, content)]);
            let stack = detect_stack(repo, &repo_files);
            let domains = domains_for_stack(&stack);
            let proposed = propose_corpus_rules(&[(repo.to_string(), domains)]).await;
            let rule = proposed
                .iter()
                .find(|r| r.id == ID)
                .unwrap_or_else(|| panic!("{ID} must be proposed for {repo}"));
            assert_eq!(rule.domain, "universal");
            assert!(rule.recommended, "{repo}: suggested, not merely available");
            assert!(
                rule.is_auto_recommended,
                "{repo}: a policy rule is still auto-recommended (pre-checked)"
            );
            assert_eq!(rule.repos, vec![repo.to_string()]);
        }
    }

    /// `ARCH-SINGLE-SOURCE-LOGIC-1` and `ARCH-NAMING-ONE-CONCEPT-1` (Wave C) live in the
    /// `universal` domain, bound to every repo by id with no stack match. The clone detector
    /// reads TypeScript, JavaScript, Python and Rust, so each of those stacks must be handed
    /// both rules, pre-checked (a policy rule is auto-recommended).
    #[tokio::test]
    async fn clone_logic_and_naming_rules_are_proposed_for_typescript_python_and_rust_stacks() {
        for id in ["ARCH-SINGLE-SOURCE-LOGIC-1", "ARCH-NAMING-ONE-CONCEPT-1"] {
            for (repo, path) in [
                ("acme/ts-app", "src/app.ts"),
                ("acme/js-app", "src/app.js"),
                ("acme/py-app", "app/main.py"),
                ("acme/rs-app", "src/main.rs"),
            ] {
                let repo_files = files(&[(path, "x")]);
                let stack = detect_stack(repo, &repo_files);
                let domains = domains_for_stack(&stack);
                let proposed = propose_corpus_rules(&[(repo.to_string(), domains)]).await;
                let rule = proposed
                    .iter()
                    .find(|r| r.id == id)
                    .unwrap_or_else(|| panic!("{id} must be proposed for {repo}"));
                assert_eq!(rule.domain, "universal");
                assert!(
                    rule.recommended,
                    "{id} {repo}: suggested, not merely available"
                );
                assert!(
                    rule.is_auto_recommended,
                    "{id} {repo}: a policy rule is still auto-recommended (pre-checked)"
                );
                assert_eq!(rule.repos, vec![repo.to_string()]);
            }
        }
    }

    /// `UI-SHARED-PRIMITIVE-1` (Wave C) lives in the `ui` domain, which a React stack arms. The
    /// fixture is the `src/app.jsx` + React manifest every `ui` witness uses: a `.jsx` file in a
    /// React package must be handed the rule, pre-checked. A Python-only repo must not be.
    #[tokio::test]
    async fn ui_shared_primitive_is_proposed_for_a_react_jsx_stack_and_not_for_python() {
        const ID: &str = "UI-SHARED-PRIMITIVE-1";
        let repo = "acme/web";
        let repo_files = files(&[
            ("src/app.jsx", ""),
            ("package.json", r#"{"dependencies":{"react":"18.2.0"}}"#),
        ]);
        let stack = detect_stack(repo, &repo_files);
        let domains = domains_for_stack(&stack);
        assert!(domains.contains(&"ui".to_string()), "{domains:?}");
        let proposed = propose_corpus_rules(&[(repo.to_string(), domains)]).await;
        let rule = proposed
            .iter()
            .find(|r| r.id == ID)
            .unwrap_or_else(|| panic!("{ID} must be proposed for a React stack"));
        assert_eq!(rule.domain, "ui");
        assert!(rule.recommended, "suggested, not merely available");
        assert!(rule.is_auto_recommended, "a policy rule is pre-checked");
        assert_eq!(rule.repos, vec![repo.to_string()]);

        let py_files = files(&[("app/main.py", "print('hi')")]);
        let stack = detect_stack("acme/py-app", &py_files);
        let domains = domains_for_stack(&stack);
        let proposed = propose_corpus_rules(&[("acme/py-app".to_string(), domains)]).await;
        let rule = proposed.iter().find(|r| r.id == ID);
        assert!(
            rule.is_none_or(|r| !r.recommended),
            "a Python-only repo must not have {ID} suggested: {rule:?}"
        );
    }

    /// req_a6: a plain Rust + Axum + Tokio + sqlx service must arm `rust:axum`, `rust:tokio`
    /// and `rust:sqlx` — the three Rust families the audit named, 15 rules between them.
    #[test]
    fn req_a6_a_rust_service_arms_axum_tokio_and_sqlx_domains() {
        let repo_files = files(&[
            ("src/main.rs", "fn main() {}"),
            (
                "Cargo.toml",
                "[dependencies]\naxum = \"0.7\"\ntokio = { version = \"1\" }\nsqlx = \"0.8\"\n",
            ),
        ]);
        let stack = detect_stack("acme/svc", &repo_files);
        let domains = domains_for_stack(&stack);
        for expected in [
            "rust",
            "rust:axum",
            "rust:tokio",
            "rust:sqlx",
            "sql",
            "api-layer",
        ] {
            assert!(
                domains.iter().any(|d| d == expected),
                "{expected} missing from {domains:?} (stack {stack:?})"
            );
        }
        // Still NOT SeaORM: a sqlx repo must not get entity-pattern rules (#52).
        assert!(!domains.iter().any(|d| d == "rust:seaorm"), "{domains:?}");
    }

    /// req_a6: `process` and `concurrency` are always-on. Their rules
    /// (`PROCESS-CONVENTIONAL-COMMIT-1`, `PROCESS-BRANCH-NAMING-1`,
    /// `PROCESS-COMMIT-DOC-1`, `PROCESS-ADO-LINK-1`, `PROC-PR-CONCURRENCY-1`) are about how
    /// commits/PRs are made, which is true of any repo — there is no stack that makes them
    /// irrelevant, so gating them on one was the defect.
    #[test]
    fn req_a6_process_and_concurrency_domains_are_emitted_for_any_repo() {
        for witness in [
            vec![("README.md", "")],
            vec![("src/main.rs", "fn main() {}")],
            vec![("app.py", "")],
        ] {
            let stack = detect_stack("acme/x", &files(&witness));
            let domains = domains_for_stack(&stack);
            for expected in ALWAYS_ON_DOMAINS {
                assert!(
                    domains.iter().any(|d| d == expected),
                    "{expected} must be emitted for {witness:?}: {domains:?}"
                );
            }
        }
    }

    /// req_a6: `integration` rides on having an API surface, not on repo count —
    /// `INTEGRATION-AUTH-SEAM-1` and `INTEGRATION-EVENT-WIRING-1` are intra-repo seams. A
    /// docs-only repo must NOT get them (H1: "a rule that does not run because it is
    /// irrelevant to the scanned stack is CORRECT").
    #[test]
    fn req_a6_integration_rides_on_an_api_surface_not_on_repo_count() {
        let api = detect_stack("acme/api", &files(&[("src/server.js", "")]));
        assert!(domains_for_stack(&api).iter().any(|d| d == "integration"));

        let docs = detect_stack("acme/docs", &files(&[("README.md", "")]));
        let docs_domains = domains_for_stack(&docs);
        assert!(
            !docs_domains.iter().any(|d| d == "integration"),
            "a repo with no API surface has no consumer/producer seam: {docs_domains:?}"
        );
    }

    /// Guards the no-false-positive direction of the new framework detectors: a plain
    /// TypeScript/React repo must not pick up Nest, Angular, Vue or any Go/JVM/.NET framework
    /// just because the detectors were widened.
    #[test]
    fn req_a6_widened_detection_does_not_misfire_on_an_unrelated_stack() {
        let repo_files = files(&[
            ("src/app.tsx", "export const App = () => null;"),
            (
                "package.json",
                r#"{"dependencies":{"react":"18.2.0","react-dom":"18.2.0"}}"#,
            ),
        ]);
        let stack = detect_stack("acme/web", &repo_files);
        let domains = domains_for_stack(&stack);
        for forbidden in [
            "javascript:nest",
            "javascript:angular",
            "javascript:vue",
            "go:web",
            "go:gorm",
            "go:grpc",
            "java:spring",
            "csharp:aspnetcore",
            "csharp:efcore",
            "ruby:rails",
            "rust:tokio",
            "rust:axum",
            "rust:sqlx",
        ] {
            assert!(
                !domains.iter().any(|d| d == forbidden),
                "{forbidden} must not be emitted for a plain React repo: {domains:?}"
            );
        }
    }
}
