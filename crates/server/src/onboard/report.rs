//! Report-building functions: assemble `ScanReport`, render CSV/issue-body output,
//! create GitHub issues, and merge deep-tier reports.

use super::{Finding, RepoStack, ScanReport};
use super::propose::propose_rules;

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

/// Escape a single field for RFC 4180 CSV: if the value contains a comma, double-quote,
/// or newline, wrap it in double-quotes and double any internal double-quotes.
pub(crate) fn csv_escape(field: &str) -> String {
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

/// Merge the per-repo deep-tier reports into ONE tier-level [`crate::ai_audit::DeepReport`].
/// The three lenses keep their identity across repos: every repo's SOC-2 gaps fold into the
/// single SOC-2 lens result, every repo's security findings into the security lens, etc., so
/// the consumer sees three lens results (not three-per-repo). The advisory envelope + honesty
/// disclaimer are preserved. Lens errors are concatenated so a repo that failed a lens is
/// visible rather than silently dropped.
pub(crate) fn merge_deep_reports(
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
