//! Content-audit functions: run gate rule arms over file content and classify
//! findings against suppressions.

use camerata_gateway::{
    is_in_test_scope, is_test_or_fixture_path, test_scope_line_ranges, TEST_PATH_NOTE,
    TEST_PATH_SEVERITY,
};

use super::{default_status, Finding, AUDIT_RULES};

/// Severity for a rule id (for grouping/sorting in the table).
pub(crate) fn severity_for(_rule_id: &str) -> &'static str {
    // Deterministic floor findings are ACTUAL exploitable bugs (a hardcoded credential, a
    // secret in a URL, SQL built by string concatenation) — not "doesn't follow a preferred
    // pattern." They rank CRITICAL so they float above the architectural conformance
    // findings (high/medium/low) and can never be buried under "no mappers crate." Every
    // rule that reaches the gate's deterministic arm is, by construction, a real defect.
    "critical"
}

/// The gate's description for a rule id, or the id if unknown.
pub(crate) fn title_for(rule_id: &str) -> String {
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
        let content_lines = camerata_gateway::content_match_lines(rule_id, content);
        if content_lines.is_empty() {
            // Path-based rule: fire the arm with the real path and empty content to
            // check whether this file's PATH marks it as secret-bearing. A path-based
            // finding is attributed to line 0 (no line-numbered content match). This is
            // intentional and documented (SEC-NO-SECRET-FILE-1's primary home is the gate;
            // the scan entry is informational at the path level).
            if let Some(arm) = camerata_gateway::lookup_arm(rule_id) {
                if arm(path, "").is_err() {
                    let detail = format!(
                        "{} (path-based: the file path itself marks this as a secret-bearing file)",
                        title_for(rule_id)
                    );
                    findings.push(Finding {
                        repo: repo.to_string(),
                        path: path.to_string(),
                        line: 0,
                        rule_id: rule_id.to_string(),
                        severity: severity_for(rule_id).to_string(),
                        snippet: path.to_string(),
                        detail,
                        status: default_status(),
                        also_matches: Vec::new(),
                        preview: false,
                        preview_tool: None,
                        in_test: false,
                        needs_review: false,
                    });
                }
            }
            continue;
        }
        for line_no in content_lines {
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

/// Audit one repo's already-fetched files into a flat finding list (each tagged
/// with `repo`). Pure.
pub fn audit_files(repo: &str, files: &[(String, String)]) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (path, content) in files {
        findings.extend(audit_content(repo, path, content));
    }
    findings
}

/// Whether a rule describes what CODE should look like (audit it against source) vs how
/// the FLEET/TEAM operates (governance/process — arm it, but don't code-audit). The
/// orchestration (`ORCH-`), meta-principle (`SPIRIT-`), and process (`PROC-`) families
/// are governance/process; everything else (ARCH-/RUST-/SQL-/UI-/SEC-/…) is code.
pub(crate) fn is_code_auditable_rule(id: &str) -> bool {
    !(id.starts_with("ORCH-") || id.starts_with("SPIRIT-") || id.starts_with("PROC-"))
}

/// Classify a repo's findings against its suppressions (inline `camerata:allow` waivers
/// parsed from the files + the committed `.camerata/baseline.json`), setting each
/// finding's `status`. Also appends a `CAM-WAIVER-NEEDS-REASON` finding for every
/// reason-less waiver (the require-reason invariant). REPORT everything; the `status`
/// is what lets enforcement act on the delta only.
pub(crate) fn classify_repo_findings(
    findings: &mut Vec<Finding>,
    repo: &str,
    files: &[(String, String)],
) {
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
