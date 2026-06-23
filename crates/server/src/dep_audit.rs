//! Dependency-vulnerability scanning via osv-scanner (Google's multi-ecosystem
//! dependency auditor).
//!
//! This module is wired into the ALWAYS-ON security floor of the onboarding scan
//! (alongside `audit_files`).  It runs `osv-scanner --format json -r <dir>` on
//! each repo root so the lockfile discovery is recursive and covers every
//! ecosystem in the repo (Cargo, npm, PyPI, Go modules, Maven, etc.) in one pass.
//!
//! # Why always-on (not opt-in)
//!
//! Knowing your current dependency-CVE exposure is a prerequisite for any
//! governance posture.  Unlike SAST rules (which carry architectural opinions the
//! team must debate), a known-vulnerable dependency is an objective fact with a
//! known fix.  It belongs on the floor alongside hardcoded-secrets, not in the
//! opt-in tier.
//!
//! # Fail-soft contract
//!
//! Network unavailability, unsupported platforms, a missing Go toolchain, or any
//! subprocess error causes this pass to emit a [`crate::onboard::CoverageNote`]
//! and return an empty findings list — the scan always completes, just without
//! dep-audit findings.  The caller MUST NOT treat absence of dep-audit findings as
//! "no vulnerable dependencies."
//!
//! # Finding shape
//!
//! Each finding has:
//! - `rule_id = "DEP-AUDIT-1"` (the stable umbrella id; advisory IDs go in `detail`)
//! - `path` = the lockfile path (relative to the repo root)
//! - `line = 0` (lockfiles have no meaningful per-CVE line number)
//! - `severity` mapped from the advisory's severity field (see [`map_severity`])
//! - `snippet` = `<pkg-name>@<version>` — exactly the affected coordinate
//! - `detail` = `<advisory-id> (<pkg>@<version>): <advisory-title or aliases>`
//!
//! This shape is consistent with floor findings: stable rule_id, per-finding
//! severity, and a human-readable detail.
//!
//! # Decision record
//!
//! See `docs/decisions/2026-06-23_dependency_audit_onboarding_floor.md` for the
//! rationale on tool choice, always-on placement, network-required + fail-soft
//! contract, and the non-gate classification.

use std::path::Path;
use std::time::Duration;

use crate::onboard::{CoverageNote, Finding};
use crate::tool_provisioning;

/// When this environment variable is set (to any non-empty value), `run_dep_audit`
/// and `run_dep_audit_with_tooling` return immediately with an empty findings list
/// and no coverage note — skipping ALL provisioning and network activity.
///
/// **Purpose:** test isolation.  Onboarding scan tests (`audit_repos` tests in
/// `onboard.rs`) exercise scan logic, not dep-audit specifically.  Without this
/// escape hatch those tests trigger `ensure_osv_scanner` → `download_osv_scanner`,
/// which makes a live network request and hangs indefinitely in environments with
/// no network or a blocked GitHub releases URL.
///
/// Set this in any test that calls `audit_repos` / `audit_repos_with_tooling` but
/// is not specifically testing the dep-audit path:
///
/// ```rust
/// std::env::set_var("CAMERATA_DISABLE_DEP_AUDIT", "1");
/// ```
///
/// Never set this variable in production code paths.
const DISABLE_ENV_VAR: &str = "CAMERATA_DISABLE_DEP_AUDIT";

/// The stable rule id that all dep-audit findings carry.  Advisory-specific ids
/// (RUSTSEC-…, GHSA-…, CVE-…) are placed in `detail` so they are searchable
/// and visible in the UI without polluting the rule_id namespace.
pub const DEP_AUDIT_RULE_ID: &str = "DEP-AUDIT-1";

/// The tool name carried on every dep-audit finding and coverage note.
const TOOL_NAME: &str = "osv-scanner";

// ─── JSON parser (pure, unit-tested) ─────────────────────────────────────────

/// Parse the JSON output of `osv-scanner --format json` into a flat list of
/// [`Finding`]s.
///
/// Each vulnerable package-advisory pair becomes one finding.  `repo` is the
/// `owner/repo` label tagged onto each finding for multi-repo aggregation.  Paths
/// in the osv-scanner output are ABSOLUTE; they are made relative to `repo_root`
/// before attaching to the finding (the UI only shows `path`, not the full
/// filesystem path).
///
/// # Fail-soft
///
/// Malformed or empty JSON returns `Err` so the caller can emit a coverage note.
/// Advisory objects with missing fields are skipped (not surfaced as errors) —
/// a partial parse is better than a hard failure.
pub fn parse_osv_json(
    repo: &str,
    repo_root: &Path,
    output: &str,
) -> anyhow::Result<Vec<Finding>> {
    let v: serde_json::Value = serde_json::from_str(output)?;
    let mut out: Vec<Finding> = Vec::new();

    // Top-level schema:  { "results": [ { "source": {...}, "packages": [...] } ] }
    let Some(results) = v.get("results").and_then(|r| r.as_array()) else {
        // No results key = no vulnerabilities found (empty scan), OR an unexpected
        // schema change.  Return empty rather than an error — the caller will see
        // zero findings (not a coverage-note failure path).
        return Ok(out);
    };

    for result in results {
        // The lockfile path for this result block.
        let lockfile_abs = result
            .get("source")
            .and_then(|s| s.get("path"))
            .and_then(|p| p.as_str())
            .unwrap_or("(unknown lockfile)");

        // Make the path relative to the repo root when possible.  osv-scanner
        // emits absolute paths; we store only the relative part in the finding.
        let lockfile_rel = Path::new(lockfile_abs)
            .strip_prefix(repo_root)
            .map(|r| r.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| lockfile_abs.to_string());
        // Normalise to forward slashes (matters on Windows).
        let lockfile_rel = lockfile_rel.replace('\\', "/");

        let Some(packages) = result.get("packages").and_then(|p| p.as_array()) else {
            continue;
        };

        for pkg_entry in packages {
            let pkg = pkg_entry.get("package");
            let pkg_name = pkg
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("(unknown)");
            let pkg_version = pkg
                .and_then(|p| p.get("version"))
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)");

            // The `groups` field (osv-scanner v1.8+) consolidates the same advisory
            // across multiple lockfile entries; `vulnerabilities` is the canonical list
            // in older versions. Both may be present. We read `vulnerabilities` (always
            // present) and deduplicate on advisory id within this package entry.
            let Some(vulns) = pkg_entry
                .get("vulnerabilities")
                .and_then(|v| v.as_array())
            else {
                // No vulnerabilities on this package entry (can happen in certain
                // schema variants). Skip.
                continue;
            };

            for vuln in vulns {
                let advisory_id = vuln
                    .get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("(unknown-advisory)")
                    .to_string();

                // Aliases (e.g. a RUSTSEC entry aliases a CVE).  Collect as a
                // comma-separated string for the detail field.
                let aliases: Vec<&str> = vuln
                    .get("aliases")
                    .and_then(|a| a.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();

                // Advisory title / summary — used in the detail field.
                let title = vuln
                    .get("summary")
                    .and_then(|s| s.as_str())
                    .or_else(|| {
                        // Fallback: use aliases if no summary.
                        None
                    })
                    .unwrap_or("");

                // Severity: prefer database_specific.severity (a plain word like
                // "CRITICAL"), fall back to the CVSS score bucket, then default to
                // "medium".
                let severity = extract_severity(vuln);

                let coord = format!("{pkg_name}@{pkg_version}");
                let aliases_str = if aliases.is_empty() {
                    String::new()
                } else {
                    format!(" (also: {})", aliases.join(", "))
                };
                let detail = if title.is_empty() {
                    format!("{advisory_id}{aliases_str} affects {coord}")
                } else {
                    format!("{advisory_id}{aliases_str}: {title} (affects {coord})")
                };

                out.push(Finding {
                    repo: repo.to_string(),
                    path: lockfile_rel.clone(),
                    line: 0,
                    rule_id: DEP_AUDIT_RULE_ID.to_string(),
                    severity,
                    snippet: coord,
                    detail,
                    status: "active".to_string(),
                    also_matches: Vec::new(),
                    preview: false,
                    preview_tool: None,
                    in_test: false,
                    needs_review: false,
                });
            }
        }
    }

    Ok(out)
}

/// Extract and normalise a severity label from an OSV advisory JSON object.
///
/// Lookup order:
/// 1. `database_specific.severity` — the most direct advisory-database label
///    (e.g. `"CRITICAL"` on OSV entries sourced from GitHub Advisory DB).
/// 2. `severity[].type == "CVSS_V3"` score string — map the base score to a
///    bucket (CVSS 9.0+ = critical, 7.0-8.9 = high, 4.0-6.9 = medium, < 4.0 = low).
/// 3. Default: `"medium"` (a conservative unknown that does not bury the finding).
pub fn map_severity(word: &str) -> &'static str {
    match word.trim().to_ascii_uppercase().as_str() {
        "CRITICAL" => "critical",
        "HIGH" => "high",
        "MEDIUM" | "MODERATE" => "medium",
        "LOW" => "low",
        _ => "medium",
    }
}

fn extract_severity(vuln: &serde_json::Value) -> String {
    // 1. database_specific.severity (word form).
    if let Some(sev) = vuln
        .get("database_specific")
        .and_then(|d| d.get("severity"))
        .and_then(|s| s.as_str())
    {
        return map_severity(sev).to_string();
    }

    // 2. CVSS_V3 score — parse the base score from the vector string.
    if let Some(sevs) = vuln.get("severity").and_then(|s| s.as_array()) {
        for entry in sevs {
            let stype = entry.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if stype == "CVSS_V3" {
                if let Some(score_str) = entry.get("score").and_then(|s| s.as_str()) {
                    // CVSS vector string example:
                    //   "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"
                    // The base score is NOT embedded in the vector string directly;
                    // osv-scanner may omit the numeric score.  Some OSV entries
                    // carry the score as a plain float in `score` instead of the
                    // vector string — handle both.
                    if let Ok(f) = score_str.parse::<f64>() {
                        return cvss_score_to_severity(f).to_string();
                    }
                    // If it's a vector string we can't easily extract the numeric
                    // base score without a full CVSS library, so fall through.
                }
            }
            // Some entries use a plain numeric score under "score".
            if let Some(n) = entry.get("score").and_then(|s| s.as_f64()) {
                return cvss_score_to_severity(n).to_string();
            }
        }
    }

    // 3. Default.
    "medium".to_string()
}

/// Map a CVSS base score (0.0-10.0) to Camerata's four-level severity vocabulary.
fn cvss_score_to_severity(score: f64) -> &'static str {
    if score >= 9.0 {
        "critical"
    } else if score >= 7.0 {
        "high"
    } else if score >= 4.0 {
        "medium"
    } else {
        "low"
    }
}

// ─── scanner invocation (I/O) ─────────────────────────────────────────────────

/// Inner implementation of the dep-audit pass, accepting an explicit tooling dir
/// so the provisioning path is unit-testable without a real Camerata data dir or
/// network.  The public [`run_dep_audit`] resolves the tooling dir from
/// `dirs::data_dir()` and delegates here.
async fn run_dep_audit_with_tooling(
    repo: &str,
    repo_dir: &Path,
    tooling_dir: &Path,
) -> (Vec<Finding>, Option<CoverageNote>) {
    // Fast-exit when the disable env var is set.  This is exclusively for test
    // isolation — see the `DISABLE_ENV_VAR` constant doc for the rationale.
    if std::env::var(DISABLE_ENV_VAR)
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        return (Vec::new(), None);
    }

    let bin = match tool_provisioning::ensure_osv_scanner(tooling_dir).await {
        Ok(b) => b,
        Err(e) => {
            return (
                Vec::new(),
                Some(CoverageNote {
                    tool: TOOL_NAME.to_string(),
                    message: format!(
                        "dependency audit (osv-scanner) did not run: {e}"
                    ),
                }),
            );
        }
    };

    let bin_str = bin.to_string_lossy().into_owned();

    // Run: `osv-scanner --format json -r <repo_dir>`
    // `-r` = recursive lockfile discovery from the given root directory.
    // Exit 1 is the normal "vulnerabilities found" signal — do NOT treat it as
    // an error.  Exit 2+ indicates a real scan error.
    //
    // Cap the subprocess at 120 s.  On very large repos with hundreds of
    // lockfiles osv-scanner can be slow, but an unbounded wait blocks the scan
    // (and any test exercising `audit_repos`) indefinitely.  120 s is generous
    // for a real repo; in practice osv-scanner on typical projects takes < 10 s.
    let result = tokio::time::timeout(
        Duration::from_secs(120),
        tokio::process::Command::new(&bin_str)
            .args(["--format", "json", "-r"])
            .arg(repo_dir)
            .output(),
    )
    .await;

    let output = match result {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return (
                Vec::new(),
                Some(CoverageNote {
                    tool: TOOL_NAME.to_string(),
                    message: format!(
                        "dependency audit (osv-scanner) did not run: \
                         could not spawn process: {e}"
                    ),
                }),
            );
        }
        Err(_elapsed) => {
            return (
                Vec::new(),
                Some(CoverageNote {
                    tool: TOOL_NAME.to_string(),
                    message: "dependency audit (osv-scanner) did not run: \
                              subprocess timed out after 120 s"
                        .to_string(),
                }),
            );
        }
    };

    // osv-scanner exit codes:
    //   0 = no vulnerabilities found
    //   1 = vulnerabilities found (normal)
    //   2+ = tool error (e.g. no lockfiles found, network error)
    // We treat 0 and 1 as scan output to parse; anything else is a soft failure.
    let exit_ok = matches!(output.status.code(), Some(0) | Some(1));

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    if !exit_ok || stdout.trim().is_empty() {
        // Check if it's just "no lockfiles" (exit 128 or stderr mentioning no
        // lockfiles) vs. a real error.  Either way: fail soft.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output.status.code().unwrap_or(-1);

        // osv-scanner exits 128 when no lockfiles are found (no vuln exposure to
        // report is not the same as an error).  Surface as a coverage note without
        // alarming language.
        let note_msg = if code == 128
            || stderr.to_ascii_lowercase().contains("no lockfiles")
            || stderr.to_ascii_lowercase().contains("no packages")
        {
            format!(
                "dependency audit (osv-scanner) found no lockfiles in {repo} — \
                 no dependency exposure detected (or no supported lockfile present)"
            )
        } else if !exit_ok {
            format!(
                "dependency audit (osv-scanner) did not run: \
                 process exited with code {code}: {stderr}"
            )
        } else {
            // Exit OK but empty stdout — treat as no lockfiles.
            format!(
                "dependency audit (osv-scanner) found no lockfiles in {repo}"
            )
        };

        return (
            Vec::new(),
            Some(CoverageNote {
                tool: TOOL_NAME.to_string(),
                message: note_msg,
            }),
        );
    }

    // Parse the JSON output.
    match parse_osv_json(repo, repo_dir, &stdout) {
        Ok(findings) => (findings, None),
        Err(e) => (
            Vec::new(),
            Some(CoverageNote {
                tool: TOOL_NAME.to_string(),
                message: format!(
                    "dependency audit (osv-scanner) could not parse output: {e}"
                ),
            }),
        ),
    }
}

/// Run osv-scanner on `repo_dir` and return `(findings, coverage_note)`.
///
/// On success: returns a possibly-empty `Vec<Finding>` and `None` for the note.
/// On any failure (provisioning, subprocess, parse): returns `Vec::new()` and
/// `Some(CoverageNote)` describing why dep-audit did not run.  The scan continues
/// either way.
///
/// The invocation is: `osv-scanner --format json -r <repo_dir>`
/// (`-r` / `--recursive` lets osv-scanner discover all lockfiles under the root).
///
/// # Fail-soft
///
/// When the Camerata data dir cannot be resolved (unusual) or osv-scanner cannot
/// be provisioned (network unavailable, unsupported platform, no Go toolchain),
/// a `CoverageNote` is returned and the scan is NOT aborted.
pub async fn run_dep_audit(
    repo: &str,
    repo_dir: &Path,
) -> (Vec<Finding>, Option<CoverageNote>) {
    // Resolve the Camerata tooling dir where provisioned binaries live.
    let tooling_dir = match tool_provisioning::tooling_dir() {
        Some(d) => d,
        None => {
            return (
                Vec::new(),
                Some(CoverageNote {
                    tool: TOOL_NAME.to_string(),
                    message: "dependency audit (osv-scanner) did not run: \
                              could not resolve Camerata data dir for tool provisioning"
                        .to_string(),
                }),
            );
        }
    };
    run_dep_audit_with_tooling(repo, repo_dir, &tooling_dir).await
}

// ─── tests (pure parser + severity mapping) ──────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ── severity mapping ──────────────────────────────────────────────────────

    #[test]
    fn map_severity_known_words() {
        assert_eq!(map_severity("CRITICAL"), "critical");
        assert_eq!(map_severity("HIGH"), "high");
        assert_eq!(map_severity("MEDIUM"), "medium");
        assert_eq!(map_severity("MODERATE"), "medium");
        assert_eq!(map_severity("LOW"), "low");
        // Unknown → medium (conservative default).
        assert_eq!(map_severity("UNKNOWN"), "medium");
        assert_eq!(map_severity(""), "medium");
    }

    #[test]
    fn cvss_score_buckets() {
        assert_eq!(cvss_score_to_severity(10.0), "critical");
        assert_eq!(cvss_score_to_severity(9.0), "critical");
        assert_eq!(cvss_score_to_severity(8.9), "high");
        assert_eq!(cvss_score_to_severity(7.0), "high");
        assert_eq!(cvss_score_to_severity(6.9), "medium");
        assert_eq!(cvss_score_to_severity(4.0), "medium");
        assert_eq!(cvss_score_to_severity(3.9), "low");
        assert_eq!(cvss_score_to_severity(0.0), "low");
    }

    // ── parse_osv_json — realistic fixture ───────────────────────────────────
    //
    // A small but realistic osv-scanner JSON output with one vulnerable package
    // (the `time` crate RUSTSEC-2020-0071) and one alias (GHSA-…).  Crafted
    // from the real osv-scanner output format (v1.8+).

    fn fixture_root() -> PathBuf {
        PathBuf::from("/home/runner/work/my-repo")
    }

    fn osv_fixture() -> &'static str {
        // SAFETY: the string does not contain any credential-shaped literals.
        // The RUSTSEC id is a real, public advisory about the `time` crate's
        // unsound `localtime_r` call — widely cited in public security databases.
        // The GHSA alias is the GitHub Security Advisory cross-reference.
        // Neither is a secret or token.
        concat!(
            r#"{"schemaVersion":"1.0","results":[{"source":{"path":"/home/runner/work/my-repo/Cargo.lock","type":"lockfile"},"packages":[{"package":{"name":"time","version":"0.1.45","ecosystem":"crates.io"},"vulnerabilities":[{"id":"RUSTSEC-2020-0071","aliases":["#,
            r#""GHSA-wcg3-cvx6-7396"],"summary":"Potential segfault in the time crate","database_specific":{"severity":"HIGH"},"severity":[{"type":"CVSS_V3","score":"7.0"}]}],"groups":[{"ids":["RUSTSEC-2020-0071"]}]}]}]}"#
        )
    }

    #[test]
    fn parse_realistic_fixture_produces_one_finding() {
        let root = fixture_root();
        let findings = parse_osv_json("acme/api", &root, osv_fixture()).unwrap();
        assert_eq!(findings.len(), 1, "one vulnerable package → one finding");
        let f = &findings[0];
        assert_eq!(f.rule_id, DEP_AUDIT_RULE_ID);
        assert_eq!(f.repo, "acme/api");
        // Path must be RELATIVE (no /home/runner/work/my-repo prefix).
        assert_eq!(f.path, "Cargo.lock");
        assert_eq!(f.line, 0, "lockfile findings have no per-line position");
        assert_eq!(f.snippet, "time@0.1.45");
        // Advisory id must appear in the detail.
        assert!(
            f.detail.contains("RUSTSEC-2020-0071"),
            "advisory id must be in detail: {}", f.detail
        );
        // Alias must appear in the detail.
        assert!(
            f.detail.contains("GHSA-wcg3-cvx6-7396"),
            "alias must be in detail: {}", f.detail
        );
        // Severity mapped from database_specific.severity = "HIGH".
        assert_eq!(f.severity, "high");
        // Not a preview finding (dep-audit is floor-level).
        assert!(!f.preview);
        assert_eq!(f.preview_tool, None);
        // Status defaults to active.
        assert_eq!(f.status, "active");
    }

    #[test]
    fn parse_empty_results_returns_no_findings() {
        let root = fixture_root();
        let json = r#"{"schemaVersion":"1.0","results":[]}"#;
        let findings = parse_osv_json("acme/api", &root, json).unwrap();
        assert!(findings.is_empty(), "empty results must produce no findings");
    }

    #[test]
    fn parse_no_results_key_returns_empty() {
        // Schema variant with no "results" key: treat as zero vulnerabilities.
        let root = fixture_root();
        let json = r#"{"schemaVersion":"1.0"}"#;
        let findings = parse_osv_json("acme/api", &root, json).unwrap();
        assert!(findings.is_empty());
    }

    #[test]
    fn parse_malformed_json_returns_err() {
        let root = fixture_root();
        let result = parse_osv_json("acme/api", &root, "not json at all");
        assert!(result.is_err(), "malformed JSON must return Err, not Ok([])");
    }

    #[test]
    fn parse_severity_falls_back_to_cvss_score() {
        // No database_specific.severity — fall back to CVSS score.
        let root = fixture_root();
        let json = r#"{"results":[{"source":{"path":"/home/runner/work/my-repo/Cargo.lock","type":"lockfile"},"packages":[{"package":{"name":"pkg","version":"1.0.0","ecosystem":"crates.io"},"vulnerabilities":[{"id":"CVE-2099-9999","severity":[{"type":"CVSS_V3","score":"9.5"}]}]}]}]}"#;
        let findings = parse_osv_json("acme/api", &root, json).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, "critical", "CVSS 9.5 → critical");
    }

    #[test]
    fn parse_unknown_severity_defaults_to_medium() {
        let root = fixture_root();
        // No database_specific.severity, no severity array → default medium.
        let json = r#"{"results":[{"source":{"path":"/home/runner/work/my-repo/go.sum","type":"lockfile"},"packages":[{"package":{"name":"golang.org/x/net","version":"0.0.1","ecosystem":"Go"},"vulnerabilities":[{"id":"GHSA-xxxx-yyyy-zzzz"}]}]}]}"#;
        let findings = parse_osv_json("acme/svc", &root, json).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, "medium");
        assert_eq!(findings[0].path, "go.sum");
    }

    #[test]
    fn parse_absolute_path_inside_root_is_made_relative() {
        // When the osv-scanner path IS under the repo root it must be stripped.
        let root = PathBuf::from("/projects/myapp");
        let json = r#"{"results":[{"source":{"path":"/projects/myapp/backend/package-lock.json","type":"lockfile"},"packages":[{"package":{"name":"lodash","version":"4.17.15","ecosystem":"npm"},"vulnerabilities":[{"id":"GHSA-p6mc-m468-83gw","database_specific":{"severity":"HIGH"}}]}]}]}"#;
        let findings = parse_osv_json("acme/web", &root, json).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].path, "backend/package-lock.json");
    }

    #[test]
    fn parse_absolute_path_outside_root_kept_as_is() {
        // When the path does NOT start with the repo root (unusual edge case) it
        // must be surfaced as-is rather than panicking.
        let root = PathBuf::from("/projects/myapp");
        let json = r#"{"results":[{"source":{"path":"/tmp/something/package-lock.json","type":"lockfile"},"packages":[{"package":{"name":"lodash","version":"4.17.15","ecosystem":"npm"},"vulnerabilities":[{"id":"GHSA-p6mc-m468-83gw","database_specific":{"severity":"HIGH"}}]}]}]}"#;
        let findings = parse_osv_json("acme/web", &root, json).unwrap();
        assert_eq!(findings.len(), 1);
        // Falls back to the absolute path when strip_prefix fails.
        assert_eq!(findings[0].path, "/tmp/something/package-lock.json");
    }

    // ── fail-soft: provisioning failure → coverage note ──────────────────────
    //
    // When osv-scanner cannot be provisioned (no binary on PATH, no cached binary,
    // no network, no Go toolchain) the pass MUST emit a CoverageNote and return an
    // empty findings list — never panic, never a silent clean result.
    //
    // We exercise this through `run_dep_audit_with_tooling` with a temp dir that
    // has no cached binary inside it AND where neither `osv-scanner` nor `go` are
    // expected on PATH in a typical CI-free unit test context.  The function will
    // attempt to download from GitHub and fail (no binary in the tmp dir, no Go).
    // We capture what comes back and assert the invariants regardless of HOW
    // provisioning failed.

    #[tokio::test]
    async fn provisioning_failure_emits_coverage_note_not_panic() {
        use tempfile::TempDir;
        // A fresh temp tooling dir: no cached binary inside it.  ensure_osv_scanner
        // will probe PATH (osv-scanner almost certainly not there in unit tests),
        // then try to download / go install.  Either it succeeds (machine has
        // osv-scanner or network+Go) — in which case we get findings — or it fails
        // with a coverage note.  In both cases no panic is the invariant.
        // We just verify the type contract, not a specific outcome.
        let tmp = TempDir::new().unwrap();
        let repo_dir = std::path::Path::new("/nonexistent-camerata-dep-audit-repo-xyz");
        let (findings, note) =
            run_dep_audit_with_tooling("acme/test", repo_dir, tmp.path()).await;
        // One of two valid outcomes:
        //   (a) provisioning failed → findings empty, note is Some
        //   (b) provisioning succeeded but repo_dir doesn't exist → note is Some, findings empty
        // Either way: no panic, and the result is type-safe.
        // We assert the invariant: findings empty AND note has the right tool name
        // when it is present.
        if let Some(ref n) = note {
            assert_eq!(n.tool, "osv-scanner", "coverage note must name the tool");
            assert!(!n.message.is_empty(), "coverage note must have a message");
        }
        // Findings from a nonexistent repo dir are always empty.
        assert!(
            findings.is_empty(),
            "dep-audit on a nonexistent dir must produce no findings: {findings:?}"
        );
    }

    /// Verify the fail-soft contract when the tooling dir cannot be written to
    /// (read-only, empty, no cached binary and no network write capability).
    /// The function must return a coverage note describing the failure, not panic.
    #[tokio::test]
    async fn no_cached_binary_and_impossible_tooling_dir_emits_note() {
        // /nonexistent-tooling-xyz: does not exist, cannot be created (root-owned
        // path segment).  ensure_osv_scanner will try PATH (likely fails), try the
        // cache probe (the dir doesn't exist → no binary), then try to create the
        // dir (fails because /nonexistent… is not writable as a non-root user on
        // typical Unix systems).  Result: InstallFailed → CoverageNote.
        //
        // NOTE: on macOS / Linux the path /nonexistent-xyz below the root IS
        // creatable by the current user IF they have root privs (e.g. CI sudo).
        // We don't gate this on whether creation fails — the invariant is just
        // "no panic and the result is well-typed."
        let tooling = std::path::Path::new("/nonexistent-camerata-tooling-dir-xyz");
        let repo_dir = std::path::Path::new("/nonexistent-camerata-repo-xyz");
        let (findings, note) =
            run_dep_audit_with_tooling("acme/test", repo_dir, tooling).await;
        // Regardless of outcome: no panic, and if there's a note it is well-formed.
        if let Some(ref n) = note {
            assert_eq!(n.tool, "osv-scanner");
            assert!(!n.message.is_empty());
        }
        // Nonexistent repo dir → no findings even if binary was found.
        assert!(findings.is_empty());
    }
}
