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

/// Number each line `NNNN| line`, so the model cites ACCURATE line numbers. Without
/// this the digest had no line markers and the model estimated by counting (and drifted —
/// 3 of 4 findings on the testbed cited the wrong line).
fn number_lines(content: &str) -> String {
    let mut s = String::new();
    for (i, line) in content.lines().enumerate() {
        s.push_str(&format!("{:>4}| {}\n", i + 1, line));
    }
    s
}

/// Build a single code digest from the repo's files, capped at [`MAX_DIGEST_CHARS`].
/// Each file is delimited and LINE-NUMBERED so the model can cite exact paths + lines.
pub fn build_digest(files: &[(String, String)]) -> String {
    let mut out = String::new();
    let mut truncated = false;
    for (path, content) in files {
        let header = format!("// ===== FILE: {path} =====\n");
        let numbered = number_lines(content);
        if out.len() + header.len() + numbered.len() > MAX_DIGEST_CHARS {
            // Add a partial slice of this file if there's room, then stop.
            let remaining = MAX_DIGEST_CHARS.saturating_sub(out.len() + header.len());
            if remaining > 200 {
                out.push_str(&header);
                let slice: String = numbered.chars().take(remaining).collect();
                out.push_str(&slice);
                out.push('\n');
            }
            truncated = true;
            break;
        }
        out.push_str(&header);
        out.push_str(&numbered);
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

Each line in the digest is prefixed with its line number as `NNNN| `. Cite that exact
number in `line` — do not estimate.

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
                status: "active".to_string(),
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
                domain: "architecture".to_string(),
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

/// The skeptic system prompt for the adversarial-verify pass: try to REFUTE each
/// finding, and calibrate severity to the app's real context. This is the precision
/// guard — AI security analysis over-flags (a negligible timing residual, a single-user
/// authz "gap"), and advisory findings shouldn't carry noise into the architect's queue.
pub fn verify_system_prompt() -> String {
    r#"You are a hard-to-convince security reviewer auditing an AUTOMATED tool's findings.
For EACH finding, decide: is it a REAL, concrete, actionable issue in THIS codebase's
context, or should it be DROPPED as vacuous / theoretical / over-flagged?

Drop (keep=false) the classic over-flags:
- a timing "oracle" whose delta is negligible (e.g. one HMAC compare against a deliberately
  slow Argon2 baseline + network jitter) — not realistically measurable;
- an authorization "gap" in a SINGLE-USER app where only one user exists (low/no impact);
- anything the surrounding code already mitigates, or that needs a precondition the app
  makes impossible.

Keep (keep=true) only findings that point at a concrete, exploitable-or-clearly-wrong
issue, and assign a CALIBRATED severity (high/medium/low) for the real context — a real
but low-impact issue is keep=true with severity=low, not a drop.

Return ONLY JSON, no prose:
{"verdicts":[{"index":0,"keep":true,"severity":"high|medium|low","reason":"one line"}]}
One verdict per finding, addressed by its [index]."#
        .to_string()
}

/// Apply the skeptic's verdicts: drop refuted findings, recalibrate severity, and append
/// the verifier's one-line reason. Robust — unparseable verdicts keep all findings as-is
/// (fail-open: never silently lose a finding to a parse error).
pub fn apply_verdicts(raw: &str, findings: Vec<Finding>) -> Vec<Finding> {
    let Some(json) = extract_json_object(raw) else {
        return findings;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return findings;
    };
    let Some(arr) = v["verdicts"].as_array() else {
        return findings;
    };
    let mut out = Vec::new();
    for (i, mut f) in findings.into_iter().enumerate() {
        match arr.iter().find(|x| x["index"].as_u64() == Some(i as u64)) {
            Some(verdict) => {
                if !verdict["keep"].as_bool().unwrap_or(true) {
                    continue; // refuted -> drop
                }
                if let Some(sev) = verdict["severity"].as_str() {
                    f.severity = match sev {
                        "high" => "high",
                        "low" => "low",
                        _ => "medium",
                    }
                    .to_string();
                }
                if let Some(reason) = verdict["reason"].as_str().filter(|s| !s.is_empty()) {
                    f.detail = format!("{} [verified: {reason}]", f.detail);
                }
                out.push(f);
            }
            // No verdict for this finding -> keep it (fail-open).
            None => out.push(f),
        }
    }
    out
}

/// Run the skeptic pass over a repo's AI findings (a fresh, reasoning-based perspective —
/// deliberately NOT re-sent the whole digest, so it judges exploitability/context, not
/// code minutiae). Graceful: on any model failure the findings pass through unchanged.
pub async fn verify_findings(llm: &Llm, repo: &str, findings: Vec<Finding>) -> Vec<Finding> {
    if findings.is_empty() {
        return findings;
    }
    let mut prompt = format!("Repository: {repo}\n\nScrutinize these findings:\n");
    for (i, f) in findings.iter().enumerate() {
        prompt.push_str(&format!(
            "[{i}] (severity {}) {}:{} — {} :: {}\n",
            f.severity, f.path, f.line, f.snippet, f.detail
        ));
    }
    match llm
        .complete(
            LlmRequest::new(prompt)
                .with_system(verify_system_prompt())
                .with_max_tokens(2048),
        )
        .await
    {
        Ok(resp) => apply_verdicts(&resp.text, findings),
        Err(_) => findings,
    }
}

/// Run the AI architectural audit for one repo. Returns the findings + proposed rules.
/// A model/transport failure surfaces as an error the caller turns into a scan note,
/// so the deterministic findings always still return.
pub async fn audit_repo(
    llm: &Llm,
    repo: &str,
    files: &[(String, String)],
    selected: &[(String, String)],
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
) -> anyhow::Result<(Vec<Finding>, Vec<ProposedRule>)> {
    if files.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let digest = build_digest(files);
    // PARAMETERIZE by the architect's selected rules: the audit checks the code against
    // the rules the project actually adopted, THEN flags anything else genuine. Without a
    // selection it falls back to the free-form investigative audit.
    let rules_block = if selected.is_empty() {
        String::new()
    } else {
        let mut b = String::from(
            "The project has ADOPTED these rules — check the code against each, AND flag \
             any other genuine issues you find:\n",
        );
        for (id, directive) in selected {
            b.push_str(&format!("- [{id}] {directive}\n"));
        }
        b.push('\n');
        b
    };
    let prompt = format!(
        "Repository: {repo}\n\n{rules_block}Audit this code for genuine architectural/security violations.\n\n{digest}"
    );
    // Feedback: register this agent's GENERATED prompt so the scan UI can show, live,
    // that the AI is actually working (the "see the AI's output" panel).
    let session = format!("audit-{repo}");
    if let Some((store, key)) = feedback {
        store.register(
            key,
            crate::transcript::AgentTranscript {
                session_id: session.clone(),
                role: format!("AI audit — {repo}"),
                prompt: prompt.clone(),
                output: String::new(),
                status: "running".to_string(),
            },
        );
    }
    let req = LlmRequest::new(prompt)
        .with_system(audit_system_prompt())
        .with_max_tokens(4096);
    // With feedback, STREAM the model's output into the transcript as it generates (so the
    // drawer fills in live instead of staying blank until the end).
    let resp = if let Some((store, key)) = feedback {
        let mut on_delta = |t: &str| store.append_output_raw(key, &session, t);
        llm.complete_streaming(req, &mut on_delta).await?
    } else {
        llm.complete(req).await?
    };
    let (findings, proposed) = parse_ai_findings(repo, &resp.text);
    // Adversarial-verify pass: a fresh skeptic refutes over-flags + recalibrates severity
    // before these advisory findings reach the architect's queue.
    let verified = verify_findings(llm, repo, findings).await;
    if let Some((store, key)) = feedback {
        store.append_output(
            key,
            &session,
            &format!(
                "\n[verify pass complete — {} advisory finding(s) after refute]",
                verified.len()
            ),
        );
        store.set_status(key, &session, "done");
    }
    Ok((verified, proposed))
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

    fn finding(rule: &str, sev: &str) -> Finding {
        Finding {
            repo: "me/api".into(),
            path: "a.rs".into(),
            line: 1,
            rule_id: rule.into(),
            severity: sev.into(),
            snippet: "x".into(),
            detail: "d".into(),
            status: "active".into(),
        }
    }

    #[test]
    fn apply_verdicts_drops_refuted_and_recalibrates() {
        let findings = vec![
            finding("AI-TIMING", "medium"), // index 0 -> refuted
            finding("AI-AUTHZ", "high"),    // index 1 -> kept, downgraded to low
            finding("AI-REAL", "high"),     // index 2 -> kept as-is
        ];
        let raw = r#"{"verdicts":[
            {"index":0,"keep":false,"reason":"negligible timing residual"},
            {"index":1,"keep":true,"severity":"low","reason":"single-user, low impact"},
            {"index":2,"keep":true,"severity":"high","reason":"concrete"}
        ]}"#;
        let out = apply_verdicts(raw, findings);
        assert_eq!(out.len(), 2, "the refuted finding is dropped");
        assert!(out.iter().all(|f| f.rule_id != "AI-TIMING"));
        let authz = out.iter().find(|f| f.rule_id == "AI-AUTHZ").unwrap();
        assert_eq!(authz.severity, "low", "recalibrated down");
        assert!(authz.detail.contains("[verified:"), "carries the verifier reason");
    }

    #[test]
    fn apply_verdicts_fail_open_on_garbage() {
        let findings = vec![finding("AI-X", "high")];
        // Unparseable verdicts -> keep all (never silently lose a finding).
        let out = apply_verdicts("the model rambled", findings);
        assert_eq!(out.len(), 1);
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
