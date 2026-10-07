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
///
/// This is INTERNAL/enforcement prose (the same directive text the agent reads when the gate
/// denies a write) — it is stored on `Finding::detail` for backward compatibility and for the
/// P2 fix-generation prompt (which benefits from the precise defect description), but it is
/// NEVER the client-facing text a report renders. See `report_export::resolve_floor_finding_text`
/// / `client_headline_and_detail` (P4), which substitute an AUTHORED corpus template for the
/// headline/detail a client actually sees, so the gate's own "Deny…" phrasing never reaches
/// the report.
pub(crate) fn title_for(rule_id: &str) -> String {
    camerata_gateway::RULE_REGISTRY
        .iter()
        .find(|e| e.id == rule_id)
        .map(|e| e.description.to_string())
        .unwrap_or_else(|| rule_id.to_string())
}

/// Sensible default estimated-effort TIER for a deterministic floor finding (P4 — floor
/// findings get finding-level treatment: a curated finding must never render "not yet
/// estimated" — see `report_export::effort_hours_bounds`). Floor findings are never seen by
/// the AI calibration pass (`Finding::effort`'s doc comment), so without this default they'd
/// carry `None` forever; this is the FLOOR under that gap, not a replacement for real
/// calibration where it exists. Two tiers only, matching the report's low/medium/high scale:
/// `"low"` for a fix that removes/rotates/toggles a single artifact with no logic change
/// (delete a committed secret, flip a TLS flag), `"medium"` for a fix that rewrites query or
/// deserialization logic and its tests (raw-SQL concatenation, unsafe deserialization).
pub(crate) fn default_effort_for(rule_id: &str) -> &'static str {
    match rule_id {
        "SEC-NO-RAW-SQL-CONCAT-1" | "SEC-NO-UNSAFE-DESERIALIZATION-1" => "medium",
        _ => "low",
    }
}

// ── P4 (2026-09-29): cheap, local context facts for a committed-secret finding ──────────
//
// A secret's headline is far more useful when it says WHAT kind of secret was found (a
// Stripe key vs. a generic "hardcoded credential") and whether the repo's own `.gitignore`
// already tried (and failed) to keep it out. Both facts are computed here, at detection
// time, from data this module already has in hand — the ALREADY-FETCHED file set — and
// carried on `Finding::captures` for `report_export::resolve_floor_finding_text` to
// substitute into the corpus's authored `finding_headline`/`finding_detail` templates (the
// SAME placeholder mechanism `resolve_fix` uses for `remediation`). This stays pure (no
// filesystem/git access) — see `crate::onboard::attach_secret_history_capture` in the
// impure orchestration layer for the one context fact (git history) that genuinely needs
// git plumbing and therefore can't live here.

/// The rule ids this enrichment treats as "secret-shaped": the finding IS a literal
/// credential/secret-bearing artifact, so naming its KIND and `.gitignore` coverage is
/// meaningful. Structural rules (raw-SQL, disabled TLS, unsafe deserialization,
/// secrets-in-URL) have no "kind of secret" to name, so they're deliberately excluded —
/// their authored corpus templates don't reference the tokens this fills.
pub(crate) const SECRET_SHAPED_RULES: &[&str] = &[
    "SEC-NO-SECRET-FILE-1",
    "SEC-NO-VENDOR-TOKEN-1",
    "SEC-NO-PRIVATE-KEY-1",
    "SEC-NO-HARDCODED-SECRETS-1",
];

/// Populate `<secret-kind>`/`<gitignore-status>` captures on every SECRET-SHAPED finding in
/// `findings`, from the already-fetched `files` set. Pure — never touches the filesystem or
/// git; see the module comment above. Never overwrites a capture the caller already set.
pub(crate) fn enrich_secret_context(findings: &mut [Finding], files: &[(String, String)]) {
    for f in findings.iter_mut() {
        if !SECRET_SHAPED_RULES.contains(&f.rule_id.as_str()) {
            continue;
        }
        f.captures
            .entry("secret-kind".to_string())
            .or_insert_with(|| classify_secret_kind(&f.rule_id, &f.snippet, &f.path).to_string());
        f.captures
            .entry("gitignore-status".to_string())
            .or_insert_with(|| gitignore_status_phrase(&f.path, files).to_string());
    }
}

/// Human phrase for whether `path` is covered by any `.gitignore` present in `files` — built
/// IN-MEMORY from those files' own content (the same globbing engine
/// `onboard::files::read_local_repo_files` already trusts for the scan's own walk), never
/// touching the filesystem again. General: works whether the `.gitignore` lives at the repo
/// root or nested in a subdirectory, and for a repo fetched via an API (no local `.git`
/// checkout) exactly as well as a local clone, since it only reads already-fetched content.
fn gitignore_status_phrase(path: &str, files: &[(String, String)]) -> &'static str {
    let mut builder = ignore::gitignore::GitignoreBuilder::new(".");
    for (p, content) in files {
        let name = p.rsplit('/').next().unwrap_or(p.as_str());
        if name != ".gitignore" {
            continue;
        }
        let dir = p.strip_suffix(".gitignore").unwrap_or("");
        let dir = dir.strip_suffix('/').unwrap_or(dir);
        let from = if dir.is_empty() {
            std::path::PathBuf::from(".")
        } else {
            std::path::PathBuf::from(dir)
        };
        for line in content.lines() {
            let _ = builder.add_line(Some(from.clone()), line);
        }
    }
    let covered = builder
        .build()
        .map(|gi| gi.matched(path, false).is_ignore())
        .unwrap_or(false);
    if covered {
        "already listed in this repository's `.gitignore` — it was committed before that rule \
         was added, or with `git add -f`, which overrides `.gitignore`"
    } else {
        "not covered by any `.gitignore` rule in this repository"
    }
}

/// Classify what KIND of secret a finding is, from the rule that fired plus the matched
/// evidence (`snippet`/`path`) — general vendor-prefix / file-extension / PEM-header
/// vocabulary (never a fixture-specific string), so it generalizes to any repo the scan
/// meets. Falls back to an honest generic noun when the evidence doesn't match a known
/// vocabulary word (a bare hardcoded-secret literal has no further shape to classify).
///
/// # Bare noun phrases — NO leading article (W4, 2026-09-30)
/// Every value returned here (and the `"hardcoded credential"` fallback in
/// `report_export::generic_placeholder_filler`) is a BARE noun phrase with no leading "a"/"an".
/// The corpus's authored `finding_headline` templates that splice this in either supply their
/// OWN fixed article before an adjective (`"a live <secret-kind>"`, `"a committed
/// <secret-kind>"` — grammatically correct regardless of the noun's own initial sound, since
/// the article agrees with the ADJECTIVE that follows it, not the noun) or request one dynamically
/// via the `<a:secret-kind>` token syntax (`report_export::instantiate_remediation`), which
/// computes "a" vs "an" from the resolved value at substitution time
/// (`report_export::indefinite_article`). A value that carried its own article here would
/// double up with either mechanism — see the W4 bug this shape fixes
/// (`docs/plans/2026-09-30_cycle2-queue-hardening.md`).
pub(crate) fn classify_secret_kind(rule_id: &str, snippet: &str, path: &str) -> &'static str {
    match rule_id {
        "SEC-NO-SECRET-FILE-1" => secret_file_kind(path),
        "SEC-NO-VENDOR-TOKEN-1" => vendor_token_kind(snippet),
        "SEC-NO-PRIVATE-KEY-1" => private_key_kind(snippet),
        _ => "hardcoded credential",
    }
}

fn secret_file_kind(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(lower.as_str());
    if name == ".env" || name.starts_with(".env.") {
        "live `.env` file"
    } else if name.ends_with(".pem") || name.ends_with(".key") {
        "private-key file"
    } else if name.ends_with(".p12") || name.ends_with(".pfx") {
        "PKCS#12 key store"
    } else if name.ends_with(".jks") || name.ends_with(".keystore") {
        "Java key store"
    } else if matches!(name, "id_rsa" | "id_dsa" | "id_ecdsa" | "id_ed25519") {
        "SSH private key"
    } else {
        "secret-bearing file"
    }
}

fn vendor_token_kind(snippet: &str) -> &'static str {
    const GITHUB_PREFIXES: &[&str] = &["ghp_", "gho_", "ghu_", "ghr_", "ghs_", "github_pat_"];
    const SLACK_PREFIXES: &[&str] = &["xoxb-", "xoxp-", "xoxa-", "xoxr-", "xoxs-"];
    if snippet.contains("AKIA") || snippet.contains("ASIA") {
        "AWS access key"
    } else if GITHUB_PREFIXES.iter().any(|p| snippet.contains(p)) {
        "GitHub access token"
    } else if SLACK_PREFIXES.iter().any(|p| snippet.contains(p)) {
        "Slack token"
    } else if snippet.contains("sk_live_") {
        "live Stripe secret key"
    } else if snippet.contains("AIza") {
        "Google API key"
    } else if snippet.contains("sk-ant-") {
        "Anthropic API key"
    } else if snippet.contains("sb_secret_") {
        "Supabase secret key"
    } else {
        "vendor credential token"
    }
}

fn private_key_kind(snippet: &str) -> &'static str {
    if snippet.contains("OPENSSH") {
        "OpenSSH private key"
    } else if snippet.contains("EC PRIVATE KEY") {
        "EC private key"
    } else if snippet.contains("DSA PRIVATE KEY") {
        "DSA private key"
    } else if snippet.contains("PGP PRIVATE KEY") {
        "PGP private key"
    } else {
        "private key"
    }
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
                        confidence: None,
                        // P4: a sensible default estimate — a curated floor finding must
                        // never render "not yet estimated" (see `default_effort_for`).
                        effort: Some(default_effort_for(rule_id).to_string()),
                        category: None,
                        located: true,
                        // These universal (non-Supabase) rules have no domain object (table/
                        // function/bucket) for a capture to name — the finding's own `path` is
                        // already the only object-identifying detail, and `resolve_fix` reads
                        // that straight off `Finding.path` for `<path>`/`<file>` tokens.
                        captures: Default::default(),
                        // Deterministic floor rules are never multi-option.
                        evaluated_option_id: None,
                        also_locations: Vec::new(),
                        fix_specific: None,
                        // The deterministic floor never runs through calibration.
                        calibration_rationale: None,
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
                confidence: None,
                // P4: a sensible default estimate — a curated floor finding must never
                // render "not yet estimated" (see `default_effort_for`).
                effort: Some(default_effort_for(rule_id).to_string()),
                category: None,
                located: true,
                captures: Default::default(),
                evaluated_option_id: None,
                also_locations: Vec::new(),
                fix_specific: None,
                // The deterministic floor never runs through calibration.
                calibration_rationale: None,
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
    // P4: cheap, local context facts (secret kind + `.gitignore` coverage) need the WHOLE
    // file set, not just the one file `audit_content` sees — a second pass over the
    // now-complete `findings` list, not threaded into `audit_content` itself.
    enrich_secret_context(&mut findings, files);
    findings
}

/// Whether a rule describes what CODE should look like (audit it against source) vs how
/// the FLEET/TEAM operates (governance/process — arm it, but don't code-audit). The
/// orchestration (`ORCH-`), meta-principle (`SPIRIT-`), and process (`PROC-`) families
/// are governance/process; everything else (ARCH-/RUST-/SQL-/UI-/SEC-/…) is code.
pub(crate) fn is_code_auditable_rule(id: &str) -> bool {
    !(id.starts_with("ORCH-") || id.starts_with("SPIRIT-") || id.starts_with("PROC-"))
}

/// Whether `id` is a CI-tier (mechanical or architectural) corpus rule — i.e. one that DECLARES
/// deterministic CI enforcement (lint pattern or AST/static-analysis pass) as its enforcement
/// tier. `unwrap_or(false)` for an id the corpus doesn't know about — an unrecognized id
/// defaults to "not CI-tier" rather than silently excluded from anything that branches on this.
///
/// W4 (root-cause fix): this predicate used to ALSO mean "never shown to the LLM" — `audit_repos`
/// filtered its semantic/AI prompt on `!is_ci_tier_rule(...)`, on the theory that a CI-tier
/// declaration meant a deterministic gate already covered it. That conflated "deterministically
/// checkable IN PRINCIPLE" with "actually checked on this repo" — a rule like strict layering
/// needs a repo-specific layer map no generic detector ships with, so on a repo Camerata has
/// never seen, declaring it `mechanical` bought it NO detector and NO LLM coverage either: it was
/// checked by nothing, while the report called it clean. `audit_repos` no longer excludes
/// CI-tier rules from its semantic prompt at all — see that function's per-repo `semantic`
/// construction. This predicate is kept for what is still genuinely true about a CI-tier rule:
/// which [`crate::mechanical_gate::detector_channel`] (if any) answers it deterministically, for
/// the pipeline-integrity ledger's per-rule accounting and the scan-time linter preview.
pub(crate) fn is_ci_tier_rule(id: &str, corpus: Option<&camerata_rules::RuleSet>) -> bool {
    corpus
        .and_then(|c| c.get_by_id(id))
        .map(|r| r.enforcement.is_ci_enforced())
        .unwrap_or(false)
}

/// Whether `rule` is a MULTI-OPTION SEMANTIC rule eligible for the audit-integrated
/// alternative-recommendation feature (see
/// `docs/design/2026-09-22_audit-integrated-alternatives.md`): NOT gate-armed (a deterministic
/// detector already answers it exactly, so offering the model a choice between directive
/// WORDINGS would be moot — the detector enforces the one behavior regardless of phrasing),
/// NOT CI-tier mechanical/architectural for the same reason (a registered architectural checker
/// answers one specific behavior, not a chosen wording), code-auditable (not a
/// governance/process rule), and offers two or more `[[option]]` alternatives — a single-option
/// rule keeps today's one-directive behavior untouched. This is UNRELATED to whether the rule
/// reaches the model's code-audit prompt (as of W4, every code-auditable selected rule does,
/// CI-tier or not — see `is_ci_tier_rule`'s doc comment); it is specifically about whether the
/// model is asked to pick AMONG DIRECTIVE WORDINGS for a rule that doesn't have one fixed, exact
/// answer. Shared by the main scan's per-repo alternative-set construction
/// ([`build_rule_alternatives`]) and the `rescan-alternatives` endpoint, so the two paths can
/// never drift on which rules are eligible.
pub(crate) fn is_semantic_multi_option_rule(rule: &camerata_rules::Rule) -> bool {
    let id = rule.id.0.as_str();
    rule.options.len() >= 2
        && !rule.enforcement.is_ci_enforced()
        && camerata_gateway::lookup_arm(id).is_none()
        && is_code_auditable_rule(id)
}

/// Build the [`crate::ai_audit::RuleAlternatives`] set for every id in `ids` that resolves, in
/// `corpus`, to a [`is_semantic_multi_option_rule`]. Ids that don't resolve (unknown to the
/// corpus, or not eligible) are simply skipped — this is a best-effort JOIN, never a hard
/// requirement that every selected id have alternatives. Duplicate ids collapse to one entry.
///
/// `chosen_options` supplies each rule's PROJECT-level chosen option (uppercased rule id ->
/// option id, as persisted on `RuleSelection.chosen_option`); a rule absent from that map (or
/// with no persisted choice) falls back to the corpus rule's own `default_option` — matching
/// `Rule::resolved_option`'s own fallback order, just computed ahead of the LLM call so the
/// prompt can mark "currently selected" honestly.
pub(crate) fn build_rule_alternatives<'a>(
    corpus: &camerata_rules::RuleSet,
    ids: impl Iterator<Item = &'a str>,
    chosen_options: &std::collections::HashMap<String, String>,
) -> Vec<crate::ai_audit::RuleAlternatives> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for id in ids {
        let upper = id.trim().to_ascii_uppercase();
        if upper.is_empty() || !seen.insert(upper.clone()) {
            continue;
        }
        let Some(rule) = corpus.get_by_id(id) else {
            continue;
        };
        if !is_semantic_multi_option_rule(rule) {
            continue;
        }
        let options: Vec<crate::onboard::RuleOptionView> = rule
            .options
            .iter()
            .map(|o| crate::onboard::RuleOptionView {
                id: o.id.clone(),
                label: o.label.clone(),
                directive: o.directive.clone(),
                why: o.why.clone(),
            })
            .collect();
        let selected_option_id = chosen_options
            .get(&upper)
            .cloned()
            .or_else(|| rule.default_option.clone());
        out.push(crate::ai_audit::RuleAlternatives {
            rule_id: upper,
            options,
            selected_option_id,
        });
    }
    out
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
        let mut status = classify_one(&fr, &inline, &baseline);
        // A merged finding carries the sibling rule ids it also matches (`also_matches`).
        // A waiver written against ANY of those ids must suppress the merged row — otherwise
        // deduplication (which picks one primary) would silently defeat a legitimate waiver
        // aimed at a demoted sibling. Only an inline waiver can hinge on rule-id like this
        // (baseline matches by content fingerprint, which is identical across the group), so
        // we only escalate the inline case.
        if status == Status::Active && !f.also_matches.is_empty() {
            for alt in &f.also_matches {
                let alt_fr = FindingRef {
                    rule_id: alt.clone(),
                    ..fr.clone()
                };
                if classify_one(&alt_fr, &inline, &baseline) == Status::SuppressedInline {
                    status = Status::SuppressedInline;
                    break;
                }
            }
        }
        f.status = match status {
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
            confidence: None,
            effort: None,
            category: None,
            located: true,
            captures: Default::default(),
            evaluated_option_id: None,
            also_locations: Vec::new(),
            fix_specific: None,
            calibration_rationale: None,
        });
    }
}

#[cfg(test)]
mod classify_tests {
    use super::*;

    fn finding(rule_id: &str, path: &str, line: usize, also: &[&str]) -> Finding {
        Finding {
            rule_id: rule_id.to_string(),
            path: path.to_string(),
            line,
            also_matches: also.iter().map(|s| s.to_string()).collect(),
            snippet: "x".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn waiver_against_a_demoted_sibling_suppresses_the_merged_row() {
        // A merged finding's PRIMARY rule is R-PRIMARY, but the same site also matched
        // R-SIBLING (demoted into also_matches by dedup). A waiver written for R-SIBLING must
        // still suppress the merged row — otherwise dedup silently defeats a valid waiver.
        let files = vec![(
            "src/a.rs".to_string(),
            "risky(); // camerata:allow R-SIBLING -- accepted here\n".to_string(),
        )];
        let mut findings = vec![finding("R-PRIMARY", "src/a.rs", 1, &["R-SIBLING"])];
        classify_repo_findings(&mut findings, "o/r", &files);
        assert_eq!(
            findings[0].status, "suppressed-inline",
            "waiver on a demoted sibling rule id must suppress the merged finding"
        );
    }

    #[test]
    fn waiver_matching_neither_primary_nor_sibling_leaves_finding_active() {
        let files = vec![(
            "src/a.rs".to_string(),
            "risky(); // camerata:allow R-UNRELATED -- for something else\n".to_string(),
        )];
        let mut findings = vec![finding("R-PRIMARY", "src/a.rs", 1, &["R-SIBLING"])];
        classify_repo_findings(&mut findings, "o/r", &files);
        assert_eq!(findings[0].status, "active");
    }

    #[test]
    fn primary_rule_waiver_still_suppresses_without_relying_on_siblings() {
        let files = vec![(
            "src/a.rs".to_string(),
            "risky(); // camerata:allow R-PRIMARY -- accepted\n".to_string(),
        )];
        let mut findings = vec![finding("R-PRIMARY", "src/a.rs", 1, &[])];
        classify_repo_findings(&mut findings, "o/r", &files);
        assert_eq!(findings[0].status, "suppressed-inline");
    }
}

#[cfg(test)]
mod p4_context_tests {
    use super::*;

    // ── default_effort_for ──────────────────────────────────────────────────────

    #[test]
    fn every_audit_rule_gets_a_low_or_medium_default_effort() {
        for rule_id in AUDIT_RULES {
            let effort = default_effort_for(rule_id);
            assert!(
                effort == "low" || effort == "medium",
                "{rule_id} must default to low or medium, got {effort:?}"
            );
        }
    }

    #[test]
    fn audit_content_never_leaves_effort_none_for_a_floor_finding() {
        // Every finding `audit_content` produces (path-based or line-based) must carry SOME
        // effort — a curated floor finding must never render "not yet estimated"
        // (`report_export::effort_hours_bounds`).
        let secret_py = "API_KEY = \"hardcoded-not-a-real-secret-abcdefgh\"\n";
        for f in audit_content("o/r", "app/config.py", secret_py) {
            assert!(f.effort.is_some(), "{}: effort must be set", f.rule_id);
        }
        for f in audit_content("o/r", ".env", "SECRET=1\n") {
            assert!(
                f.effort.is_some(),
                "{}: effort must be set (path-based)",
                f.rule_id
            );
        }
    }

    // ── classify_secret_kind ─────────────────────────────────────────────────────

    // W4 (2026-09-30): every `classify_secret_kind` value is now a BARE noun phrase (no
    // leading "a"/"an") — see that function's doc comment. The article is supplied either by
    // the authored template's own fixed adjective phrase ("a live <secret-kind>") or, where the
    // template has nothing of its own to supply one, dynamically via `<a:secret-kind>`
    // (`report_export::indefinite_article`). A bare value here can never double an article a
    // template already provides.

    #[test]
    fn classifies_known_vendor_token_shapes_by_prefix() {
        assert_eq!(
            classify_secret_kind(
                "SEC-NO-VENDOR-TOKEN-1",
                concat!("key = \"AK", "IAABCDEFGHIJKLMNOP\""),
                "a.py"
            ),
            "AWS access key"
        );
        assert_eq!(
            classify_secret_kind(
                "SEC-NO-VENDOR-TOKEN-1",
                concat!("token = \"sk_li", "ve_abcdefghijklmnopqrstuvwx\""),
                "a.py"
            ),
            "live Stripe secret key"
        );
        assert_eq!(
            classify_secret_kind(
                "SEC-NO-VENDOR-TOKEN-1",
                concat!("key = \"sb_sec", "ret_abcdef123456\""),
                "a.py"
            ),
            "Supabase secret key"
        );
        assert_eq!(
            classify_secret_kind("SEC-NO-VENDOR-TOKEN-1", "nothing recognizable here", "a.py"),
            "vendor credential token",
            "an unrecognized shape must still get an honest generic label, never panic"
        );
    }

    #[test]
    fn classifies_secret_file_kind_by_name_and_extension() {
        assert_eq!(
            classify_secret_kind("SEC-NO-SECRET-FILE-1", "", ".env"),
            "live `.env` file"
        );
        assert_eq!(
            classify_secret_kind("SEC-NO-SECRET-FILE-1", "", "certs/prod.pem"),
            "private-key file"
        );
        assert_eq!(
            classify_secret_kind("SEC-NO-SECRET-FILE-1", "", "id_rsa"),
            "SSH private key"
        );
    }

    #[test]
    fn classifies_private_key_kind_from_the_pem_header() {
        assert_eq!(
            classify_secret_kind(
                "SEC-NO-PRIVATE-KEY-1",
                concat!("-----BEGIN OPENSSH PRIV", "ATE KEY-----"),
                "a.pem"
            ),
            "OpenSSH private key"
        );
        assert_eq!(
            classify_secret_kind(
                "SEC-NO-PRIVATE-KEY-1",
                concat!("-----BEGIN RSA PRIV", "ATE KEY-----"),
                "a.pem"
            ),
            "private key"
        );
    }

    // ── gitignore coverage (via enrich_secret_context / audit_files) ────────────

    #[test]
    fn committed_secret_file_not_covered_by_gitignore_is_flagged_as_such() {
        let files = vec![
            (".gitignore".to_string(), "node_modules/\n".to_string()),
            (".env".to_string(), "SERVICE_ROLE_KEY=abc\n".to_string()),
        ];
        let findings = audit_files("o/r", &files);
        let f = findings
            .iter()
            .find(|f| f.rule_id == "SEC-NO-SECRET-FILE-1" && f.path == ".env")
            .expect("SEC-NO-SECRET-FILE-1 must fire on a real committed .env");
        assert_eq!(
            f.captures.get("gitignore-status").map(String::as_str),
            Some("not covered by any `.gitignore` rule in this repository")
        );
        assert_eq!(
            f.captures.get("secret-kind").map(String::as_str),
            Some("live `.env` file")
        );
    }

    #[test]
    fn committed_secret_file_already_gitignored_is_flagged_as_such() {
        // The highest-value case: `.env` WAS committed, and someone later added it to
        // `.gitignore` without ever removing it from the index — the file is still tracked
        // (and still scanned; see `onboard::files::read_local_repo_files`'s `git ls-files`
        // union), and the fix must say so rather than implying `.gitignore` will save it.
        let files = vec![
            (".gitignore".to_string(), ".env\n".to_string()),
            (".env".to_string(), "SERVICE_ROLE_KEY=abc\n".to_string()),
        ];
        let findings = audit_files("o/r", &files);
        let f = findings
            .iter()
            .find(|f| f.rule_id == "SEC-NO-SECRET-FILE-1" && f.path == ".env")
            .expect("SEC-NO-SECRET-FILE-1 must fire even when later gitignored");
        let status = f
            .captures
            .get("gitignore-status")
            .expect("gitignore-status must be set");
        assert!(
            status.contains("already listed"),
            "expected the already-ignored phrasing, got: {status:?}"
        );
    }

    #[test]
    fn structural_rules_never_get_secret_shaped_captures() {
        // ARCH-NO-SECRETS-IN-URL-1 has no "kind of secret" to name — enrichment must leave it
        // alone rather than inventing a nonsense classification.
        let files = vec![(
            "a.py".to_string(),
            "url = f\"https://api.example.com/x?api_key={key}\"\n".to_string(),
        )];
        let findings = audit_files("o/r", &files);
        let f = findings
            .iter()
            .find(|f| f.rule_id == "ARCH-NO-SECRETS-IN-URL-1")
            .expect("ARCH-NO-SECRETS-IN-URL-1 must fire");
        assert!(f.captures.get("secret-kind").is_none());
        assert!(f.captures.get("gitignore-status").is_none());
    }
}
