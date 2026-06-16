//! The AI architectural audit — the half of brownfield that genuinely needs a model.
//!
//! The deterministic scan ([`crate::onboard::audit_files`]) catches MECHANICAL
//! violations (hardcoded secrets, raw SQL, path escapes) precisely, line by line —
//! that is the "linting" tier, and determinism is the right tool there. This pass is
//! the other tier: it READS the code and finds the GENUINE architectural / security
//! violations that are not line-level lint —
//!   - a write/mutation path with no authorization check,
//!   - services reaching the database directly, bypassing the repository layer,
//!   - N+1 query patterns,
//!   - imports that cross a module boundary they shouldn't,
//!   - inconsistent money/date/id handling across modules,
//!   - god objects / dead abstractions / duplicated logic that should be one seam.
//!
//! AI DISCOVERS these; the architect APPROVES; approved rules become gate config
//! (mechanical where possible) or AI-assisted integration checks (where a rule is
//! inherently semantic). Enforcement stays deterministic-or-codified; discovery is AI.
//!
//! Output is the SAME `Finding` / `ProposedRule` shapes the deterministic scan emits,
//! so the onboarding UI renders both tiers in one table. AI findings carry an `AI-`
//! rule-id prefix so the UI can mark their provenance.

use crate::llm::{Llm, LlmRequest};
use crate::onboard::{Finding, ProposedRule};

/// Cap on the code digest sent to the model (chars), to bound prompt size. ~60k chars
/// is roughly 15k tokens — enough for a meaningful architectural read without blowing
/// the context on a large repo (the digest truncates with a note).
const MAX_DIGEST_CHARS: usize = 60_000;

/// Build a single code digest from the repo's files, capped at [`MAX_DIGEST_CHARS`].
/// Each file is delimited so the model can cite paths/lines.
pub fn build_digest(files: &[(String, String)]) -> String {
    let mut out = String::new();
    let mut truncated = false;
    for (path, content) in files {
        let header = format!("// ===== FILE: {path} =====\n");
        if out.len() + header.len() + content.len() > MAX_DIGEST_CHARS {
            // Add a partial slice of this file if there's room, then stop.
            let remaining = MAX_DIGEST_CHARS.saturating_sub(out.len() + header.len());
            if remaining > 200 {
                out.push_str(&header);
                let slice: String = content.chars().take(remaining).collect();
                out.push_str(&slice);
                out.push('\n');
            }
            truncated = true;
            break;
        }
        out.push_str(&header);
        out.push_str(content);
        out.push('\n');
    }
    if truncated {
        out.push_str("\n// [digest truncated at the size cap — audit the largest files first]\n");
    }
    out
}

/// The system prompt: what to look for, what NOT to (the mechanical tier is already
/// covered), and the STRICT JSON schema to return.
pub fn audit_system_prompt() -> String {
    r#"You are a senior software architect performing a governance audit of an existing codebase for Camerata.

Find GENUINE architectural and security violations — the kind that require understanding the code, NOT line-level lint. Examples of what to look for:
- a write / mutation / delete path with no authorization or permission check
- business/service code reaching the database directly, bypassing a repository/data layer
- N+1 query patterns (a query inside a loop over rows)
- imports that cross a module/layer boundary they should not (e.g. UI importing DB internals)
- inconsistent handling of money (floats), dates/timezones, or ids across modules
- god objects, dead abstractions, or duplicated logic that should be a single seam
- missing input validation on an external boundary
- secrets/config read in ways that leak, broad error swallowing that hides failures

DO NOT report: hardcoded secrets, raw SQL string concatenation, or path-escape writes — a separate deterministic scanner already covers those precisely. Do not report pure style/formatting nits.

Return ONLY a JSON object, no prose, no markdown fences, in EXACTLY this shape:
{
  "findings": [
    {
      "path": "relative/file/path",
      "line": 0,
      "severity": "high|medium|low",
      "rule": "short-kebab-rule-name (e.g. auth-on-write-paths)",
      "title": "one-line statement of the specific violation here",
      "detail": "why it's a problem and what the fix direction is"
    }
  ],
  "proposed_rules": [
    {
      "name": "short-kebab-name (matches the finding rule names)",
      "title": "the rule to enforce going forward",
      "rationale": "why this rule, grounded in the findings",
      "severity": "high|medium|low",
      "enforcement": "mechanical|review"
    }
  ]
}
If you find nothing genuine, return {"findings": [], "proposed_rules": []}. Be specific and conservative — every finding must point at real code."#
        .to_string()
}

/// Pull the first balanced-looking JSON object out of a model response (tolerates
/// markdown fences or stray prose around it).
fn extract_json_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let end = s.rfind('}')?;
    if end > start {
        Some(&s[start..=end])
    } else {
        None
    }
}

/// Parse a model audit response into Findings + ProposedRules in the scan's shapes.
/// Robust: malformed output yields empty vecs rather than erroring the whole scan.
pub fn parse_ai_findings(repo: &str, raw: &str) -> (Vec<Finding>, Vec<ProposedRule>) {
    let Some(json) = extract_json_object(raw) else {
        return (Vec::new(), Vec::new());
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return (Vec::new(), Vec::new());
    };

    let mut findings = Vec::new();
    if let Some(arr) = v["findings"].as_array() {
        for f in arr {
            let rule = f["rule"].as_str().unwrap_or("architecture").trim();
            let rule_id = format!("AI-{}", rule.to_ascii_uppercase().replace(' ', "-"));
            let severity = match f["severity"].as_str().unwrap_or("medium") {
                "high" => "high",
                "low" => "low",
                _ => "medium",
            };
            let title = f["title"].as_str().unwrap_or("").trim().to_string();
            let detail = f["detail"].as_str().unwrap_or("").trim().to_string();
            if title.is_empty() && detail.is_empty() {
                continue;
            }
            findings.push(Finding {
                repo: repo.to_string(),
                path: f["path"].as_str().unwrap_or("(repo)").to_string(),
                line: f["line"].as_u64().unwrap_or(0) as usize,
                rule_id,
                severity: severity.to_string(),
                snippet: title,
                detail,
            });
        }
    }

    let mut proposed = Vec::new();
    if let Some(arr) = v["proposed_rules"].as_array() {
        for r in arr {
            let name = r["name"].as_str().unwrap_or("").trim();
            if name.is_empty() {
                continue;
            }
            let id = format!("AI-{}", name.to_ascii_uppercase().replace(' ', "-"));
            let mechanical = r["enforcement"].as_str() == Some("mechanical");
            let title = r["title"].as_str().unwrap_or(name).trim().to_string();
            // How many AI findings this rule's name accounts for.
            let finding_count = findings
                .iter()
                .filter(|f| f.rule_id == id)
                .count();
            proposed.push(ProposedRule {
                id,
                title,
                // AI-discovered architectural rules are human-judged, not auto-mechanical.
                kind: if mechanical { "mechanical".to_string() } else { "review".to_string() },
                // Architectural guidance partitions to CONVENTIONS.md (structured).
                enforcement: "structured".to_string(),
                options: Vec::new(),
                default_option: None,
                scope: "repo-local".to_string(),
                // Inherently semantic -> enforced at the cross-agent integration tier
                // (an AI-assisted pre-PR check), not the line-level content gate.
                enforcement_point: "integration".to_string(),
                repos: vec![repo.to_string()],
                placement: "project (AI-assisted integration check)".to_string(),
                finding_count,
                recommended: true,
            });
        }
    }

    (findings, proposed)
}

/// Run the AI architectural audit for one repo. Returns the findings + proposed rules.
/// A model/transport failure surfaces as an error the caller turns into a scan note,
/// so the deterministic findings always still return.
pub async fn audit_repo(
    llm: &Llm,
    repo: &str,
    files: &[(String, String)],
) -> anyhow::Result<(Vec<Finding>, Vec<ProposedRule>)> {
    if files.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let digest = build_digest(files);
    let prompt = format!(
        "Repository: {repo}\n\nAudit this code for genuine architectural/security violations.\n\n{digest}"
    );
    let resp = llm
        .complete(
            LlmRequest::new(prompt)
                .with_system(audit_system_prompt())
                .with_max_tokens(4096),
        )
        .await?;
    Ok(parse_ai_findings(repo, &resp.text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_concatenates_and_caps() {
        let files = vec![
            ("a.rs".to_string(), "fn a() {}".to_string()),
            ("b.rs".to_string(), "fn b() {}".to_string()),
        ];
        let d = build_digest(&files);
        assert!(d.contains("FILE: a.rs"));
        assert!(d.contains("FILE: b.rs"));
        assert!(d.contains("fn a()"));

        // A file larger than the cap truncates and notes it.
        let big = vec![("big.rs".to_string(), "x".repeat(MAX_DIGEST_CHARS + 1000))];
        let d2 = build_digest(&big);
        assert!(d2.len() <= MAX_DIGEST_CHARS + 200);
        assert!(d2.contains("truncated"));
    }

    #[test]
    fn parse_valid_json_into_findings_and_rules() {
        let raw = r#"Here is the audit:
        {
          "findings": [
            {"path": "src/orders.rs", "line": 42, "severity": "high", "rule": "auth-on-write-paths", "title": "create_order writes with no auth check", "detail": "Anyone can POST."},
            {"path": "src/svc.rs", "line": 10, "severity": "medium", "rule": "no-db-in-services", "title": "OrderService queries db directly", "detail": "Bypasses repo."}
          ],
          "proposed_rules": [
            {"name": "auth-on-write-paths", "title": "Every write path checks authorization", "rationale": "x", "severity": "high", "enforcement": "review"}
          ]
        }
        Thanks!"#;
        let (findings, rules) = parse_ai_findings("me/api", raw);
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].rule_id, "AI-AUTH-ON-WRITE-PATHS");
        assert_eq!(findings[0].repo, "me/api");
        assert_eq!(findings[0].severity, "high");
        assert_eq!(findings[0].line, 42);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, "AI-AUTH-ON-WRITE-PATHS");
        assert_eq!(rules[0].kind, "review");
        assert_eq!(rules[0].enforcement_point, "integration");
        // The rule's finding_count picks up its matching finding.
        assert_eq!(rules[0].finding_count, 1);
    }

    #[test]
    fn parse_garbage_yields_empty_not_error() {
        let (f, r) = parse_ai_findings("me/api", "the model declined to answer in JSON");
        assert!(f.is_empty());
        assert!(r.is_empty());
        let (f2, r2) = parse_ai_findings("me/api", "{ not valid json ]");
        assert!(f2.is_empty());
        assert!(r2.is_empty());
    }

    #[test]
    fn empty_findings_object_is_clean() {
        let (f, r) = parse_ai_findings("me/api", r#"{"findings": [], "proposed_rules": []}"#);
        assert!(f.is_empty());
        assert!(r.is_empty());
    }
}
