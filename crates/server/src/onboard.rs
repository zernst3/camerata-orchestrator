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

use serde::Serialize;

/// The content rules the brownfield audit runs (the ones that are pure functions
/// over file content). Path-based rules (GOV-1 forbidden paths, SEC-NO-PATH-ESCAPE-1)
/// govern WRITE TARGETS, not existing content, so they are not part of the audit.
pub const AUDIT_RULES: &[&str] = &[
    "SEC-NO-HARDCODED-SECRETS-1",
    "SEC-NO-RAW-SQL-CONCAT-1",
    "ARCH-NO-SECRETS-IN-URL-1",
];

/// One violation already present in the repo.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
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
    /// Scope: `repo-local` (applies within each repo), `cross-repo` (spans the
    /// repo set, e.g. API contracts), or `process` (VCS-workflow, per account).
    pub scope: String,
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

/// The full scan result across one or more repos. Brownfield onboarding treats a
/// SET of inter-related repos (e.g. a .NET API + a Python worker + a React app) as
/// one unit: findings and the proposed ruleset aggregate across all of them, each
/// finding tagged with its repo.
#[derive(Debug, Clone, Serialize)]
pub struct ScanReport {
    /// The repos scanned (`owner/repo`).
    pub repos: Vec<String>,
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
fn severity_for(rule_id: &str) -> &'static str {
    match rule_id {
        "SEC-NO-HARDCODED-SECRETS-1" | "ARCH-NO-SECRETS-IN-URL-1" => "high",
        _ => "medium",
    }
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
    for (i, line) in content.lines().enumerate() {
        for rule_id in AUDIT_RULES {
            let Some(arm) = camerata_gateway::lookup_arm(rule_id) else {
                continue;
            };
            if let Err(detail) = arm(path, line) {
                let snippet: String = line.trim().chars().take(160).collect();
                findings.push(Finding {
                    repo: repo.to_string(),
                    path: path.to_string(),
                    line: i + 1,
                    rule_id: rule_id.to_string(),
                    severity: severity_for(rule_id).to_string(),
                    snippet,
                    detail,
                });
            }
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

    // 1. Content rules: repo-local, mechanical, the gate lives in each repo.
    for &id in AUDIT_RULES {
        let finding_count = findings.iter().filter(|f| f.rule_id == id).count();
        let mut bound: Vec<String> = findings
            .iter()
            .filter(|f| f.rule_id == id)
            .map(|f| f.repo.clone())
            .collect();
        bound.sort();
        bound.dedup();
        let repos_for_rule = if bound.is_empty() {
            repos.to_vec()
        } else {
            bound
        };
        out.push(ProposedRule {
            id: id.to_string(),
            title: title_for(id),
            kind: "mechanical".to_string(),
            scope: "repo-local".to_string(),
            enforcement_point: "content".to_string(),
            repos: repos_for_rule,
            placement: "CI gate + gate config installed in each repo".to_string(),
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
            scope: "cross-repo".to_string(),
            enforcement_point: "integration".to_string(),
            repos: repos.to_vec(),
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
        scope: "process".to_string(),
        enforcement_point: "vcs-action".to_string(),
        repos: repos.to_vec(),
        placement: "VCS-action gate at commit/PR (per account, all repos)".to_string(),
        finding_count: 0,
        recommended: false,
    });

    out
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

/// Build a report from already-aggregated findings across `repos`. Pure.
pub fn build_report(repos: Vec<String>, files_scanned: usize, findings: Vec<Finding>) -> ScanReport {
    let proposed_rules = propose_rules(&findings, &repos);
    ScanReport {
        repos,
        files_scanned,
        findings,
        proposed_rules,
        gated: false,
        message: None,
    }
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
pub async fn scan_repos(specs: &[String], token: &str) -> ScanReport {
    let mut all_findings = Vec::new();
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
                all_findings.extend(audit_files(spec, &files));
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

    let mut report = build_report(repos_ok, files_total, all_findings);
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
        assert_eq!(f.severity, "high");
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
        assert_eq!(secrets.repos, vec!["me/api".to_string()], "bound to the repo with the violation");
        assert!(secrets.placement.contains("each repo"));
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
            2,
            findings,
        );
        assert_eq!(report.repos.len(), 2);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].repo, "me/api");
        assert!(!report.gated);
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
