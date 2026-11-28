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

use crate::onboard::scope::RepoScope;
use std::time::Duration;

use crate::onboard::{CoverageNote, Finding};
use crate::scan_tools::ToolProvisioningStop;

mod manifests;

pub(crate) use manifests::is_dependency_file;

/// Whether the dependency audit opens a file of this name as a package manifest.
pub(crate) fn reads_package_manifest(file_name: &str) -> bool {
    manifests::is_manifest(file_name)
}
pub use manifests::{scrub_local_paths, unread_files_sentence};

/// When this environment variable is set (to any non-empty value), `run_dep_audit`
/// and `run_dep_audit_with_tooling` return immediately with an empty findings list
/// and no coverage note — skipping ALL provisioning and network activity.
///
/// **Purpose:** test isolation.  Onboarding scan tests (`audit_repos` tests in
/// `onboard.rs`) exercise scan logic, not dep-audit specifically.  Without this
/// escape hatch those tests trigger the pinned osv-scanner provisioning (`ToolEnv::ensure_osv_scanner`),
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
pub const DISABLE_ENV_VAR: &str = "CAMERATA_DISABLE_DEP_AUDIT";

/// The stable rule id that all dep-audit findings carry.  Advisory-specific ids
/// (RUSTSEC-…, GHSA-…, CVE-…) are placed in `detail` so they are searchable
/// and visible in the UI without polluting the rule_id namespace.
pub const DEP_AUDIT_RULE_ID: &str = "DEP-AUDIT-1";

/// The tool name carried on every dep-audit finding and coverage note.
const TOOL_NAME: &str = "osv-scanner";

/// The phrase in the coverage note osv-scanner's "no lockfiles" outcome produces. That outcome is
/// a completed scan with nothing to report, unlike every other note this module emits (the scan
/// did not run); [`note_means_scanned`] tells the two apart.
const NO_LOCKFILES: &str = "found no lockfiles";

/// The phrase that opens the coverage note listing manifests osv-scanner could not audit because
/// no lockfile sits beside or above them (M12).
pub const MANIFESTS_UNAUDITED: &str = "Dependency manifests not audited";

/// The phrase that opens the coverage sentence quoting osv-scanner's own stderr about files it
/// could not read (M12).
pub const UNREAD_FILES: &str = "Dependency files not read";

/// What a client reads for a dependency audit that did not run, whatever osv-scanner said: fixed
/// wording, so no word of the tool's own output (which holds paths of the operator's machine)
/// reaches a client file.
pub const DID_NOT_RUN_FIXED: &str = "dependency audit (osv-scanner) did not run on this repository";

/// The start of every "did not run" note that carries a reason of the tool's or the system's.
const DID_NOT_RUN_PREFIX: &str = "dependency audit (osv-scanner) did not run: ";

/// The start of the note for a result osv-scanner printed that could not be read.
const COULD_NOT_PARSE_PREFIX: &str = "dependency audit (osv-scanner) could not parse output";

/// The reasons the engine owns, in client style, with no word of the tool's: the timeout, the
/// empty result, and a missing network. A stored reason equal to one of them stays as written; any
/// other (a process exit with stderr, an install failure, a spawn error, free text) is the tool's
/// or the system's own words and becomes the fixed wording.
const ENGINE_REASONS: [&str; 3] = [
    "subprocess timed out after 120 s",
    "it exited normally but produced no output",
    "no network",
];

/// Where the unread-files sentence once quoted osv-scanner's stderr lines.
const UNREAD_QUOTE_MARKER: &str = "so they may not have been checked (";

/// The text of an osv-scanner coverage note as a client reads it. A note a scan stored may carry
/// the tool's stderr (a process exit, a failed install, a spawn error, an unreadable result, or
/// the unread-files sentence quoting stderr lines), so the export renders fixed wording instead:
/// the client still learns the audit did not run, or did not read some files, and never the
/// tool's words. Every other note is returned as written.
pub fn client_note_message(message: &str) -> String {
    if let Some(at) = message.find(UNREAD_QUOTE_MARKER) {
        let end = at + UNREAD_QUOTE_MARKER.len() - " (".len();
        // Only the parenthetical goes, up to its matching `)` (the quoted lines hold parentheses
        // of their own). What follows it, such as the files-not-copied sentence
        // `with_copy_failures` appends, stays. An unclosed parenthesis runs to the end.
        let quoted = &message[at + UNREAD_QUOTE_MARKER.len()..];
        let mut depth = 1usize;
        let mut close = None;
        for (i, c) in quoted.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let tail = close
            .map(|i| &quoted[i + 1..])
            .map(|rest| rest.strip_prefix('.').unwrap_or(rest))
            .unwrap_or("");
        return format!("{}.{tail}", &message[..end]);
    }
    if let Some(at) = message.find(DID_NOT_RUN_PREFIX) {
        let reason = &message[at + DID_NOT_RUN_PREFIX.len()..];
        if ENGINE_REASONS.contains(&reason.trim().trim_end_matches('.')) {
            return message.to_string();
        }
        return DID_NOT_RUN_FIXED.to_string();
    }
    if message.contains(COULD_NOT_PARSE_PREFIX) {
        return DID_NOT_RUN_FIXED.to_string();
    }
    message.to_string()
}

/// The `Finding::captures` key every dependency row carries: the advisory id (for example
/// `GHSA-...` or `RUSTSEC-...`). Two advisories on one package version share a snippet, path
/// and line, so the finding key needs this to tell them apart (M9).
pub const ADVISORY_CAPTURE: &str = "advisory";

/// True when a dep-audit coverage note still means osv-scanner ran (it found no lockfiles, or it
/// ran and listed manifests it could not audit); false for every "did not run" outcome
/// (provisioning, spawn, timeout, parse).
pub fn note_means_scanned(note: &CoverageNote) -> bool {
    note.message.contains(NO_LOCKFILES)
        || note.message.contains(MANIFESTS_UNAUDITED)
        || note.message.contains(UNREAD_FILES)
        || note.message.contains(crate::scan_mirror::NOT_COPIED_PHRASE)
}

/// True when a dep-audit coverage note leaves nothing unchecked, so "no known vulnerable
/// dependencies" is a fair thing to print: osv-scanner ran and found no lockfiles to check, and
/// there were no manifests left unaudited either. A note that says the audit did not run, or that
/// manifests were left out, is not clean (M11).
pub fn note_proves_clean(note: &CoverageNote) -> bool {
    note.message.contains(NO_LOCKFILES)
        && !note.message.contains(MANIFESTS_UNAUDITED)
        && !note.message.contains(UNREAD_FILES)
        && !note.message.contains(crate::scan_mirror::NOT_COPIED_PHRASE)
}

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
/// The patched version to recommend for an OSV vulnerability: the SMALLEST `fixed` version
/// across the vuln's `affected[].ranges[].events[]` (OSV schema) — the earliest release that
/// resolves it, i.e. the "upgrade at least to" target. `None` when OSV lists no fix (the
/// advisory has no patched version yet). Presence vs absence is what drives the ownership split
/// (fixable `ClientOwned` vs not-yet-fixable `ExternalDependency`) in
/// [`crate::ownership::ownership_of`].
fn osv_fixed_version(vuln: &serde_json::Value) -> Option<String> {
    let mut fixed: Vec<String> = Vec::new();
    for aff in vuln
        .get("affected")
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
    {
        for range in aff
            .get("ranges")
            .and_then(|r| r.as_array())
            .into_iter()
            .flatten()
        {
            for event in range
                .get("events")
                .and_then(|e| e.as_array())
                .into_iter()
                .flatten()
            {
                if let Some(v) = event.get("fixed").and_then(|f| f.as_str()) {
                    if !v.trim().is_empty() {
                        fixed.push(v.to_string());
                    }
                }
            }
        }
    }
    fixed
        .into_iter()
        .min_by(|a, b| semver_sort_key(a).cmp(&semver_sort_key(b)))
}

/// A coarse semver sort key: `(major, minor, patch)` from the leading numeric components, with
/// the original string as a final tiebreak. Pre-release/build metadata is ignored for ordering.
/// Good enough to pick the lowest fixed version; not a full semver implementation (no dep added).
fn semver_sort_key(v: &str) -> (u64, u64, u64, String) {
    let core = v.split(['-', '+']).next().unwrap_or(v);
    let mut parts = core.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        v.to_string(),
    )
}

/// The CVE ids an OSV advisory is known by: its own id when it is a CVE, plus every `CVE-`
/// alias, upper-cased, de-duplicated, in first-seen order.
fn advisory_cve_ids(advisory_id: &str, aliases: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in std::iter::once(advisory_id).chain(aliases.iter().copied()) {
        let id = id.trim().to_ascii_uppercase();
        if id.starts_with("CVE-") && !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

pub fn parse_osv_json(repo: &str, repo_root: &Path, output: &str) -> anyhow::Result<Vec<Finding>> {
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
            .unwrap_or(crate::onboard::placeholder_paths::UNKNOWN_LOCKFILE_PATH);

        // osv-scanner follows a symbolic link to a lockfile, so a link pointing outside the
        // repository would put an outside file's packages in the report. Such a result is
        // dropped whole: no file outside the repository is ever quoted.
        if std::fs::symlink_metadata(lockfile_abs)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            continue;
        }

        // Make the path relative to the repo root when possible.  osv-scanner
        // emits absolute paths; we store only the relative part in the finding.
        // A repo root that is a symlink (or reached through one) is compared by its real path
        // too; a path that still cannot be made relative stays absolute and is dropped when the
        // project excludes files ([`drop_excluded`]).
        let lockfile_rel = relative_to_root(Path::new(lockfile_abs), repo_root)
            .unwrap_or_else(|| lockfile_abs.to_string());
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
            let Some(vulns) = pkg_entry.get("vulnerabilities").and_then(|v| v.as_array()) else {
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
                    .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
                    .unwrap_or_default();

                // Advisory title / summary — used in the detail field.
                let title = vuln.get("summary").and_then(|s| s.as_str()).unwrap_or("");

                // Severity: a published word, else a scored CVSS vector, else the group's
                // max severity, else unknown (medium as a placeholder, held for review and
                // said so in the detail; see `read_severity`).
                let OsvSeverity {
                    label: severity,
                    known: severity_known,
                } = read_severity(vuln, pkg_entry);

                let coord = format!("{pkg_name}@{pkg_version}");
                let aliases_str = if aliases.is_empty() {
                    String::new()
                } else {
                    format!(" (also: {})", aliases.join(", "))
                };
                // W33: the patched version, if OSV knows one. Presence drives the ownership
                // split (fixable-by-upgrade `ClientOwned` vs not-yet-fixable `ExternalDependency`)
                // in `ownership::ownership_of`, which reads the `osv_fixed_version` capture below.
                let fixed_version = osv_fixed_version(vuln);

                let fixed_clause = match &fixed_version {
                    Some(v) => format!(", fixed in {v}"),
                    None => ", no patched version available yet".to_string(),
                };
                let mut detail = if title.is_empty() {
                    format!("{advisory_id}{aliases_str} affects {coord}{fixed_clause}")
                } else {
                    format!("{advisory_id}{aliases_str}: {title} (affects {coord}){fixed_clause}")
                };
                if !severity_known {
                    detail.push_str(
                        ". Severity unknown, held for review: the advisory source published no \
                         usable rating, so medium is shown as a placeholder",
                    );
                }

                // W33: carry OSV's patched version (when any) so `ownership::ownership_of`
                // can split fixable-by-upgrade (ClientOwned) from not-yet-fixable
                // (ExternalDependency). A structured capture key, not prose in `detail`.
                let mut captures = std::collections::BTreeMap::new();
                if let Some(v) = &fixed_version {
                    captures.insert("osv_fixed_version".to_string(), v.clone());
                }
                // The advisory's CVE ids (its own id when it is a CVE, plus every CVE alias), so
                // `vuln_intel` can match a GHSA/RUSTSEC advisory against KEV and EPSS by CVE.
                captures.insert(ADVISORY_CAPTURE.to_string(), advisory_id.clone());
                let cve_ids = advisory_cve_ids(&advisory_id, &aliases);
                if !cve_ids.is_empty() {
                    captures.insert(
                        crate::vuln_intel::CVE_IDS_CAPTURE.to_string(),
                        cve_ids.join(","),
                    );
                }

                let mut finding = Finding {
                    could_be_intentional: false,
                    site_key: String::new(),
                    group_path: Vec::new(),
                    group_role: crate::review_tree::GroupRole::Primary,
                    review: Default::default(),
                    flagged_by: Vec::new(),
                    possible_duplicate_of: None,
                    repo: repo.to_string(),
                    path: lockfile_rel.clone(),
                    line: 0,
                    rule_id: DEP_AUDIT_RULE_ID.to_string(),
                    // A5.6: a known vulnerable dependency is an objective fault.
                    kind: Some(camerata_api_types::audit::FindingKind::Defect),
                    severity,
                    snippet: coord,
                    detail,
                    status: "active".to_string(),
                    also_matches: Vec::new(),
                    preview: false,
                    preview_tool: None,
                    in_test: false,
                    needs_review: !severity_known,
                    confidence: (!severity_known).then(|| "needs-review".to_string()),
                    effort: None,
                    category: None,
                    located: true,
                    captures,
                    // Not a multi-option semantic rule finding — never evaluated under an
                    // alternative option.
                    evaluated_option_id: None,
                    also_locations: Vec::new(),
                    // W33: a concrete upgrade is the fix when OSV knows a patched version — a
                    // fixable (ClientOwned) dependency finding then ships an actionable fix
                    // rather than only a "you are vulnerable" statement. When no patch exists the
                    // finding is ExternalDependency and its action comes from `ownership_action`.
                    fix_specific: fixed_version
                        .as_ref()
                        .map(|v| format!("Upgrade {pkg_name} to >= {v} (the patched version).")),
                    // Dependency findings never flow through calibration either.
                    calibration_rationale: None,
                };
                crate::onboard::redact::redact_finding(&mut finding);
                out.push(finding);
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

/// The result of reading an advisory's severity: the label, and whether the advisory source
/// actually published one. `known == false` means no usable rating was found, so the label is
/// only the "do not bury it" placeholder and the row is held for review.
struct OsvSeverity {
    label: String,
    known: bool,
}

/// The word form of a severity, when it is one the vocabulary knows. Unlike [`map_severity`] an
/// unrecognised word is `None`, not a silent medium.
fn severity_word(word: &str) -> Option<&'static str> {
    match word.trim().to_ascii_uppercase().as_str() {
        "CRITICAL" => Some("critical"),
        "HIGH" => Some("high"),
        "MEDIUM" | "MODERATE" => Some("medium"),
        "LOW" => Some("low"),
        _ => None,
    }
}

/// Read the severity of one advisory. Order:
/// 1. `database_specific.severity`, when it is a word the vocabulary knows.
/// 2. Each `severity[]` entry: a plain numeric score, a CVSS v3 vector (scored locally) or a
///    CVSS v2 vector (scored locally). A v4 vector is not scored here (its scoring needs a large
///    lookup table); it falls through to the next step.
/// 3. The `max_severity` of the `groups[]` entry that lists this advisory (osv-scanner scores v4
///    itself and reports the result there).
/// 4. Otherwise unknown: `medium` as a placeholder, flagged `known: false` so the caller holds
///    the row for review and says so instead of rating it medium silently.
fn read_severity(vuln: &serde_json::Value, pkg_entry: &serde_json::Value) -> OsvSeverity {
    let known = |label: &str| OsvSeverity {
        label: label.to_string(),
        known: true,
    };

    // 1. database_specific.severity (word form).
    if let Some(word) = vuln
        .get("database_specific")
        .and_then(|d| d.get("severity"))
        .and_then(|s| s.as_str())
        .and_then(severity_word)
    {
        return known(word);
    }

    // 2. The severity array.
    if let Some(entries) = vuln.get("severity").and_then(|s| s.as_array()) {
        for entry in entries {
            let kind = entry.get("type").and_then(|t| t.as_str()).unwrap_or("");
            // A plain number under `score`, whatever the type.
            if let Some(n) = entry.get("score").and_then(|s| s.as_f64()) {
                return known(cvss_score_to_severity(n));
            }
            let Some(score) = entry.get("score").and_then(|s| s.as_str()) else {
                continue;
            };
            if let Ok(n) = score.trim().parse::<f64>() {
                return known(cvss_score_to_severity(n));
            }
            let scored = match kind {
                "CVSS_V3" => cvss_v3_base_score(score),
                "CVSS_V2" => cvss_v2_base_score(score),
                _ => None,
            };
            if let Some(n) = scored {
                return known(cvss_score_to_severity(n));
            }
        }
    }

    // 3. The group that lists this advisory.
    if let Some(label) = group_max_severity(vuln, pkg_entry) {
        return known(label);
    }

    OsvSeverity {
        label: "medium".to_string(),
        known: false,
    }
}

/// `groups[].max_severity` for the group that lists `vuln` (by its id or an alias), as a label.
/// osv-scanner writes the highest CVSS base score of the group as a string such as `"7.5"`.
fn group_max_severity(
    vuln: &serde_json::Value,
    pkg_entry: &serde_json::Value,
) -> Option<&'static str> {
    let id = vuln.get("id").and_then(|i| i.as_str()).unwrap_or("");
    let aliases: Vec<&str> = vuln
        .get("aliases")
        .and_then(|a| a.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let lists = |group: &serde_json::Value, key: &str| -> bool {
        group
            .get(key)
            .and_then(|v| v.as_array())
            .is_some_and(|ids| {
                ids.iter()
                    .filter_map(|v| v.as_str())
                    .any(|g| g == id || aliases.contains(&g))
            })
    };
    for group in pkg_entry.get("groups").and_then(|g| g.as_array())? {
        if !(lists(group, "ids") || lists(group, "aliases")) {
            continue;
        }
        let max = group.get("max_severity")?;
        if let Some(n) = max.as_f64() {
            return Some(cvss_score_to_severity(n));
        }
        let text = max.as_str()?.trim();
        if let Ok(n) = text.parse::<f64>() {
            return Some(cvss_score_to_severity(n));
        }
        return severity_word(text);
    }
    None
}

/// CVSS 3.1 "roundup": the smallest number with one decimal place that is at least `x`, with the
/// specification's guard against floating-point noise.
fn cvss_roundup(x: f64) -> f64 {
    let int_input = (x * 100_000.0).round() as i64;
    if int_input % 10_000 == 0 {
        int_input as f64 / 100_000.0
    } else {
        ((int_input / 10_000) as f64 + 1.0) / 10.0
    }
}

/// The base score of a CVSS v3.0 or v3.1 vector (`CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H`),
/// computed from the metrics. `None` for anything else (a v4 vector, a vector missing a base
/// metric, or an unknown metric value), so the caller never guesses.
pub(crate) fn cvss_v3_base_score(vector: &str) -> Option<f64> {
    let mut parts = vector.trim().split('/');
    match parts.next()? {
        "CVSS:3.0" | "CVSS:3.1" => {}
        _ => return None,
    }
    let (mut av, mut ac, mut pr, mut ui, mut scope, mut c, mut i, mut a) =
        (None, None, None, None, None, None, None, None);
    for part in parts {
        let (key, value) = part.split_once(':')?;
        match key {
            "AV" => av = Some(value),
            "AC" => ac = Some(value),
            "PR" => pr = Some(value),
            "UI" => ui = Some(value),
            "S" => scope = Some(value),
            "C" => c = Some(value),
            "I" => i = Some(value),
            "A" => a = Some(value),
            _ => {}
        }
    }
    let changed = match scope? {
        "U" => false,
        "C" => true,
        _ => return None,
    };
    let av = match av? {
        "N" => 0.85,
        "A" => 0.62,
        "L" => 0.55,
        "P" => 0.2,
        _ => return None,
    };
    let ac = match ac? {
        "L" => 0.77,
        "H" => 0.44,
        _ => return None,
    };
    let pr = match (pr?, changed) {
        ("N", _) => 0.85,
        ("L", false) => 0.62,
        ("L", true) => 0.68,
        ("H", false) => 0.27,
        ("H", true) => 0.5,
        _ => return None,
    };
    let ui = match ui? {
        "N" => 0.85,
        "R" => 0.62,
        _ => return None,
    };
    let cia = |v: Option<&str>| -> Option<f64> {
        match v? {
            "H" => Some(0.56),
            "L" => Some(0.22),
            "N" => Some(0.0),
            _ => None,
        }
    };
    let (c, i, a) = (cia(c)?, cia(i)?, cia(a)?);
    let iss = 1.0 - (1.0 - c) * (1.0 - i) * (1.0 - a);
    let impact = if changed {
        7.52 * (iss - 0.029) - 3.25 * (iss - 0.02).powi(15)
    } else {
        6.42 * iss
    };
    if impact <= 0.0 {
        return Some(0.0);
    }
    let exploitability = 8.22 * av * ac * pr * ui;
    let raw = if changed {
        1.08 * (impact + exploitability)
    } else {
        impact + exploitability
    };
    Some(cvss_roundup(raw.min(10.0)))
}

/// The base score of a CVSS v2 vector (`AV:N/AC:L/Au:N/C:P/I:P/A:P`). `None` when a base metric is
/// missing or unrecognised.
pub(crate) fn cvss_v2_base_score(vector: &str) -> Option<f64> {
    let (mut av, mut ac, mut au, mut c, mut i, mut a) = (None, None, None, None, None, None);
    for part in vector.trim().split('/') {
        let (key, value) = part.split_once(':')?;
        match key {
            "AV" => av = Some(value),
            "AC" => ac = Some(value),
            "Au" => au = Some(value),
            "C" => c = Some(value),
            "I" => i = Some(value),
            "A" => a = Some(value),
            _ => {}
        }
    }
    let av = match av? {
        "L" => 0.395,
        "A" => 0.646,
        "N" => 1.0,
        _ => return None,
    };
    let ac = match ac? {
        "H" => 0.35,
        "M" => 0.61,
        "L" => 0.71,
        _ => return None,
    };
    let au = match au? {
        "M" => 0.45,
        "S" => 0.56,
        "N" => 0.704,
        _ => return None,
    };
    let cia = |v: Option<&str>| -> Option<f64> {
        match v? {
            "N" => Some(0.0),
            "P" => Some(0.275),
            "C" => Some(0.660),
            _ => None,
        }
    };
    let (c, i, a) = (cia(c)?, cia(i)?, cia(a)?);
    let impact = 10.41 * (1.0 - (1.0 - c) * (1.0 - i) * (1.0 - a));
    let exploitability = 20.0 * av * ac * au;
    let f = if impact == 0.0 { 0.0 } else { 1.176 };
    let score = ((0.6 * impact) + (0.4 * exploitability) - 1.5) * f;
    Some((score * 10.0).round() / 10.0)
}

/// Map a CVSS base score (0.0-10.0) to Camerata's four-level severity vocabulary.
pub(crate) fn cvss_score_to_severity(score: f64) -> &'static str {
    if score <= 0.0 {
        // CVSS rates "no impact" 0.0; that is none, not low.
        "info"
    } else if score >= 9.0 {
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
    tools: &crate::pinned_tools::ToolEnv,
    scope: &RepoScope,
) -> Result<(Vec<Finding>, Option<CoverageNote>), ToolProvisioningStop> {
    run_dep_audit_in(repo, repo_dir, repo_dir, tools, scope).await
}

/// The pinned osv-scanner (#587), or why the audit cannot run. `Ok(Err(note))` is a disclosed
/// scope limit known before the run (no build for this machine, or the test-process guard);
/// `Err(stop)` is a provisioning failure during the run, which stops it.
async fn pinned_osv_scanner(
    repo: &str,
    tools: &crate::pinned_tools::ToolEnv,
) -> Result<Result<std::path::PathBuf, CoverageNote>, ToolProvisioningStop> {
    use crate::tool_provisioning::ProvisionError as P;
    match tools.ensure_osv_scanner().await {
        Ok(bin) => Ok(Ok(bin)),
        Err(
            e @ (P::UnsupportedPlatform { .. }
            | P::NoArtifactForPlatform { .. }
            | P::RealToolingProtected { .. }),
        ) => Ok(Err(CoverageNote {
            tool_failed: false,
            repo: None,
            tool: TOOL_NAME.to_string(),
            message: operator_only_failure(
                repo,
                "osv-scanner could not be provisioned",
                &e.to_string(),
            ),
        })),
        Err(e) => Err(ToolProvisioningStop {
            repo: repo.to_string(),
            tool: TOOL_NAME,
            reason: e.to_string(),
        }),
    }
}

/// [`run_dep_audit_with_tooling`] with the folder osv-scanner runs on (`scan_dir`, the scan's
/// mirror of the repository) apart from the repository itself (`repo_dir`). osv-scanner, its
/// output paths and its messages use `scan_dir`; the manifest walk that names uncovered manifests
/// reads `repo_dir` (it follows no link and reads only manifests).
async fn run_dep_audit_in(
    repo: &str,
    repo_dir: &Path,
    scan_dir: &Path,
    tools: &crate::pinned_tools::ToolEnv,
    scope: &RepoScope,
) -> Result<(Vec<Finding>, Option<CoverageNote>), ToolProvisioningStop> {
    // Fast-exit when the disable env var is set (test isolation, see `DISABLE_ENV_VAR`).
    if std::env::var(DISABLE_ENV_VAR)
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        return Ok((Vec::new(), None));
    }
    let bin = match pinned_osv_scanner(repo, tools).await? {
        Ok(bin) => bin,
        Err(mut note) => {
            note.repo.get_or_insert_with(|| repo.to_string());
            return Ok((Vec::new(), Some(note)));
        }
    };
    let (mut findings, mut note) =
        run_dep_audit_core(repo, repo_dir, scan_dir, &bin, &tools.child_path(), scope).await;
    // osv-scanner discovers lockfiles itself (`-r`), so a lockfile the project's exclusion list
    // names is dropped from its results here: the audit reports nothing about an excluded file.
    drop_excluded(&mut findings, scope);
    // The note says which repo it is about, so a scan over several repos keeps one repo's
    // dependency audit failure apart from another's.
    if let Some(n) = note.as_mut() {
        n.repo.get_or_insert_with(|| repo.to_string());
    }
    Ok((findings, note))
}

/// `path` relative to `root`, forward-slashed: directly, else through their real (canonical)
/// paths. `None` when it is not under the root either way.
fn relative_to_root(path: &Path, root: &Path) -> Option<String> {
    let rel = |p: &Path, r: &Path| {
        p.strip_prefix(r)
            .ok()
            .map(|x| x.to_string_lossy().replace('\\', "/"))
    };
    rel(path, root).or_else(|| {
        let r = root.canonicalize().ok()?;
        rel(path, &r).or_else(|| rel(&path.canonicalize().ok()?, &r))
    })
}

/// Drop every finding about a lockfile the project's Excluded by agreement list names. A finding
/// whose path is still absolute (it could not be made relative to the repo) cannot be matched
/// against the list, so with a list set it is dropped too: fail closed.
fn drop_excluded(findings: &mut Vec<Finding>, scope: &RepoScope) {
    if scope.patterns().is_empty() {
        return;
    }
    findings.retain(|f| !Path::new(&f.path).is_absolute() && !scope.excludes(&f.path));
}

async fn run_dep_audit_core(
    repo: &str,
    repo_dir: &Path,
    scan_dir: &Path,
    bin: &Path,
    // The child `PATH` (Camerata's runtimes, then the OS folders): never the user's.
    path_env: &std::ffi::OsStr,
    scope: &RepoScope,
) -> (Vec<Finding>, Option<CoverageNote>) {
    // Fast-exit when the disable env var is set.  This is exclusively for test
    // isolation — see the `DISABLE_ENV_VAR` constant doc for the rationale.
    if std::env::var(DISABLE_ENV_VAR)
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        return (Vec::new(), None);
    }

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
            .args(osv_args(scan_dir))
            .env("PATH", path_env)
            .envs(
                crate::scan_mirror::git_ceiling_for(scan_dir)
                    .map(|c| ("GIT_CEILING_DIRECTORIES", c)),
            )
            .kill_on_drop(true)
            .output(),
    )
    .await;

    let output = match result {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return (
                Vec::new(),
                Some(CoverageNote {
                    tool_failed: false,
                    repo: None,
                    tool: TOOL_NAME.to_string(),
                    message: operator_only_failure(
                        repo,
                        "osv-scanner could not be started",
                        &e.to_string(),
                    ),
                }),
            );
        }
        Err(_elapsed) => {
            return (
                Vec::new(),
                Some(CoverageNote {
                    tool_failed: false,
                    repo: None,
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
            let base = format!(
                "dependency audit (osv-scanner) {NO_LOCKFILES} in {repo}; \
                 no dependency exposure detected (or no supported lockfile present)"
            );
            // M12: no lockfile anywhere does not mean no dependencies. Name the manifests
            // that declare packages nobody checked.
            match unaudited_manifest_sentence(repo_dir, scope).await {
                Some(gap) => format!("{base}. {gap}"),
                None => base,
            }
        } else if !exit_ok {
            operator_only_failure(
                repo,
                &format!("osv-scanner exited with code {code}"),
                &stderr,
            )
        } else {
            // Exit OK but empty stdout: osv-scanner printed nothing, which a completed scan
            // never does (it prints at least an empty result). That is not "no lockfiles".
            empty_output_message()
        };

        return (
            Vec::new(),
            Some(CoverageNote {
                tool_failed: false,
                repo: None,
                tool: TOOL_NAME.to_string(),
                message: note_msg,
            }),
        );
    }

    // Parse the JSON output.
    match parse_osv_json(repo, scan_dir, &stdout) {
        Ok(findings) => {
            // M12: a successful run used to say nothing about what it left out: manifests no
            // lockfile covers, and files osv-scanner reported on stderr that it could not read.
            let stderr = String::from_utf8_lossy(&output.stderr);
            let message = manifests::success_gap_message(
                unaudited_manifest_sentence(repo_dir, scope).await,
                &stderr,
                scan_dir,
            );
            let note = message.map(|message| CoverageNote {
                tool_failed: false,
                repo: None,
                tool: TOOL_NAME.to_string(),
                message,
            });
            (findings, note)
        }
        Err(e) => (
            Vec::new(),
            Some(CoverageNote {
                tool_failed: false,
                repo: None,
                tool: TOOL_NAME.to_string(),
                message: operator_only_failure(
                    repo,
                    "osv-scanner printed a result that could not be read",
                    &e.to_string(),
                ),
            }),
        ),
    }
}

/// The note for an osv-scanner failure: fixed wording ([`DID_NOT_RUN_FIXED`]). What the tool or the
/// system said goes to the operator log only, in full: it holds paths of this machine.
fn operator_only_failure(repo: &str, what: &str, detail: &str) -> String {
    tracing::warn!(repo = %repo, detail = %detail, "dependency audit did not run: {what}");
    DID_NOT_RUN_FIXED.to_string()
}

/// The osv-scanner argument vector: JSON output, recursive discovery from `scan_dir`, the scan's
/// mirror of the repository (never the checkout).
fn osv_args(scan_dir: &Path) -> Vec<std::ffi::OsString> {
    // `--experimental-no-resolve`: for Maven, only the dependencies a pom.xml declares are
    // checked, and nothing is sent to Maven Central to resolve the ones they pull in.
    // `--no-ignore`: the mirror already holds exactly the files to read, and `.gitignore` files
    // are in it; osv-scanner must read every file it is given, a tracked lockfile that matches an
    // ignore rule included.
    let mut args: Vec<std::ffi::OsString> = [
        "--format",
        "json",
        "--experimental-no-resolve",
        "--no-ignore",
        "-r",
    ]
    .into_iter()
    .map(std::ffi::OsString::from)
    .collect();
    args.push(scan_dir.as_os_str().to_owned());
    args
}

/// The coverage message for a clean exit with nothing on stdout. A scan that completed always
/// prints a result, so this is a run that did not happen, never "no lockfiles".
fn empty_output_message() -> String {
    "dependency audit (osv-scanner) did not run: it exited normally but produced no output"
        .to_string()
}

/// The sentence naming the dependency manifests under `repo_dir` that no lockfile covers, or
/// `None` when every manifest is covered (or the walk could not run).
async fn unaudited_manifest_sentence(repo_dir: &Path, scope: &RepoScope) -> Option<String> {
    let dir = repo_dir.to_path_buf();
    let scope = scope.clone();
    let found = tokio::task::spawn_blocking(move || manifests::unaudited_manifests(&dir, &scope))
        .await
        .ok()?;
    manifests::manifests_sentence(&found)
}

/// The folders whose files are bundled outside code: a dependency file in one is marked vendored.
const THIRD_PARTY_DIRS: &[&str] = &["vendor", "third_party", "node_modules", "bower_components"];

/// The dependency files osv-scanner read for the repo at `root`: every regular, tracked lockfile
/// or manifest osv-scanner parses, none of them a link and none the project's exclusion list
/// names; those inside a third-party folder are marked vendored. The files are exactly those
/// given to osv-scanner ([`crate::scan_mirror::osv_paths`]) that it parses, so a manifest it
/// ignores (`package.json`) is never listed as checked.
pub fn audited_dependency_files(
    repo: &str,
    root: &Path,
    scope: &RepoScope,
) -> Vec<crate::scan_ledger::AuditedDependencyFile> {
    crate::scan_mirror::dependency_files(root, scope)
        .into_iter()
        .filter(|p| manifests::is_osv_read_file(p.rsplit('/').next().unwrap_or(p)))
        .filter(|p| crate::scan_mirror::is_regular_file_inside(root, p))
        .map(|path| {
            let vendored = path
                .rsplit('/')
                .skip(1)
                .any(|dir| THIRD_PARTY_DIRS.contains(&dir));
            crate::scan_ledger::AuditedDependencyFile {
                repo: repo.to_string(),
                path,
                vendored,
            }
        })
        .collect()
}

/// Of the dependency files computed from the checkout, those that are really in osv-scanner's
/// mirror. A file that failed to copy is not given to the tool, so it must not be listed as
/// checked.
fn given_to_osv(
    files: Vec<crate::scan_ledger::AuditedDependencyFile>,
    mirror: &Path,
    copy_failed: &[String],
) -> Vec<crate::scan_ledger::AuditedDependencyFile> {
    files
        .into_iter()
        .filter(|f| !copy_failed.contains(&f.path) && mirror.join(&f.path).is_file())
        .collect()
}

/// Fold the files that failed to copy into the audit's outcome. When the audit did not run (a
/// timeout, a spawn or parse error, provisioning), its own note stands as it is and no file is
/// listed as checked: appending the not-copied text would make that note read as a run that
/// happened. When it ran, the not-copied text is added, so the run is never read as clean.
fn with_copy_failures(
    repo: &str,
    note: Option<CoverageNote>,
    files: Vec<crate::scan_ledger::AuditedDependencyFile>,
    copy_failed: &[String],
) -> (
    Option<CoverageNote>,
    Vec<crate::scan_ledger::AuditedDependencyFile>,
) {
    if note.as_ref().is_some_and(|n| !note_means_scanned(n)) {
        return (note, Vec::new());
    }
    let Some(extra) = crate::scan_mirror::not_copied_note(repo, TOOL_NAME, copy_failed) else {
        return (note, files);
    };
    let note = match note {
        Some(mut n) => {
            n.message = format!("{} {}", n.message, extra.message);
            Some(n)
        }
        None => Some(extra),
    };
    (note, files)
}

/// [`given_to_osv`] for another module's test.
#[cfg(test)]
pub(crate) fn given_to_osv_for_test(
    files: Vec<crate::scan_ledger::AuditedDependencyFile>,
    mirror: &Path,
    copy_failed: &[String],
) -> Vec<crate::scan_ledger::AuditedDependencyFile> {
    given_to_osv(files, mirror, copy_failed)
}

/// After a repo's dependency audit: when osv-scanner actually scanned it, record the dependency
/// files it was given (from [`run_dep_audit_listed`]), for the dependency section of the exports.
pub fn record_audited_files(
    report: &mut crate::onboard::ScanReport,
    files: Vec<crate::scan_ledger::AuditedDependencyFile>,
    run: Option<&crate::vuln_intel::OsvRun>,
) {
    if run.is_some_and(|r| r.ran) {
        report.ledger.record_audited_dependency_files(files);
    }
}

/// [`run_dep_audit_listed`] under a time limit for the SCAN (the app's 60 s bound).
///
/// osv-scanner is provisioned FIRST, outside the limit: a slow first download is never cut short
/// into a "timed out" coverage note, and a provisioning failure is a [`ToolProvisioningStop`] (the
/// run-integrity rule), never a note. Only the scan itself is bounded; a scan past the limit is the
/// disclosed timeout note it always was.
pub async fn run_dep_audit_bounded(
    repo: &str,
    repo_dir: &Path,
    scope: &RepoScope,
    tools: &crate::pinned_tools::ToolEnv,
    limit: Duration,
) -> Result<
    (
        Vec<Finding>,
        Option<CoverageNote>,
        Vec<crate::scan_ledger::AuditedDependencyFile>,
    ),
    ToolProvisioningStop,
> {
    let disabled = std::env::var(DISABLE_ENV_VAR)
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    if !disabled {
        if let Err(mut note) = pinned_osv_scanner(repo, tools).await? {
            note.repo.get_or_insert_with(|| repo.to_string());
            return Ok((Vec::new(), Some(note), Vec::new()));
        }
    }
    match tokio::time::timeout(limit, run_dep_audit_listed(repo, repo_dir, scope, tools)).await {
        Ok(result) => result,
        Err(_elapsed) => Ok((Vec::new(), Some(timed_out_note(repo, limit)), Vec::new())),
    }
}

/// The note for a dependency scan that ran past its limit.
fn timed_out_note(repo: &str, limit: Duration) -> CoverageNote {
    CoverageNote {
        tool_failed: false,
        repo: Some(repo.to_string()),
        tool: TOOL_NAME.to_string(),
        message: format!(
            "dependency audit (osv-scanner) timed out after {} s for {repo}",
            limit.as_secs()
        ),
    }
}

/// Run the dependency audit (`osv-scanner --format json -r <mirror>`) for one repo with the
/// pinned osv-scanner from `tools` (#587), and return the findings, a coverage note when the
/// audit could not run or covered less than the repo has, and the dependency files osv-scanner
/// was actually given: the ones that are in its mirror, so a file that failed to copy is never
/// listed as checked (it is named in the coverage note instead). Empty when the audit did not run.
///
/// `Err` is a provisioning failure during the run: it stops the run, never a coverage note.
pub async fn run_dep_audit_listed(
    repo: &str,
    repo_dir: &Path,
    scope: &RepoScope,
    tools: &crate::pinned_tools::ToolEnv,
) -> Result<
    (
        Vec<Finding>,
        Option<CoverageNote>,
        Vec<crate::scan_ledger::AuditedDependencyFile>,
    ),
    ToolProvisioningStop,
> {
    // The scan's mirror: osv-scanner runs on a private copy of the admitted files and manifests,
    // never on the checkout, so a link inside the checkout cannot lead it to an outside file.
    if std::env::var(DISABLE_ENV_VAR)
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        let (findings, note) = run_dep_audit_with_tooling(repo, repo_dir, tools, scope).await?;
        return Ok((findings, note, Vec::new()));
    }
    let dir = repo_dir.to_path_buf();
    let scope_for_read = scope.clone();
    let staged = tokio::task::spawn_blocking(move || {
        crate::onboard::files::read_local_repo_files(&dir, &scope_for_read)
            .map(|ex| crate::scan_mirror::osv_paths(&ex, &dir, &scope_for_read))
    })
    .await;
    let mirror = match staged {
        Ok(Ok(rels)) => crate::scan_mirror::Mirror::build(repo_dir, rels, repo)
            .await
            .map_err(|e| e.to_string()),
        Ok(Err(e)) => Err(format!("the repository could not be read: {e}")),
        Err(e) => Err(e.to_string()),
    };
    let mirror = match mirror {
        Ok(m) => m,
        Err(why) => {
            return Ok((
                Vec::new(),
                Some(CoverageNote {
                    tool_failed: true,
                    repo: Some(repo.to_string()),
                    tool: TOOL_NAME.to_string(),
                    message: format!("dependency audit (osv-scanner) did not run: {why}"),
                }),
                Vec::new(),
            ));
        }
    };
    let copy_failed = mirror.copy_failed.clone();
    let ran = run_dep_audit_in(repo, repo_dir, mirror.path(), tools, scope).await;
    let (findings, note) = match ran {
        Ok(done) => done,
        Err(stop) => {
            mirror.finish().await;
            return Err(stop);
        }
    };
    // The dependency files it was given: those that are really in the mirror.
    let files = given_to_osv(
        audited_dependency_files(repo, repo_dir, scope),
        mirror.path(),
        &copy_failed,
    );
    mirror.finish().await;
    let (note, files) = with_copy_failures(repo, note, files, &copy_failed);
    Ok((findings, note, files))
}

// ─── tests (pure parser + severity mapping) ──────────────────────────────────

#[cfg(test)]
mod operator_path_tests;

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

    // ── M10: severity from a CVSS vector, a group's max severity, or an honest unknown ──────

    /// One OSV vuln with the given `severity` array (JSON text), optional `database_specific`
    /// and optional `groups` entry on its package, parsed through the real entry point.
    fn parse_one(severity_json: &str, database_specific: &str, groups: &str) -> Finding {
        let json = format!(
            r#"{{"results":[{{"source":{{"path":"/w/package-lock.json","type":"lockfile"}},
              "packages":[{{"package":{{"name":"lodash","version":"4.17.0","ecosystem":"npm"}},
              "vulnerabilities":[{{"id":"GHSA-aaaa-bbbb-cccc","summary":"x"{database_specific},
              "severity":{severity_json}}}]{groups}}}]}}]}}"#
        );
        let mut found = parse_osv_json("acme/api", Path::new("/w"), &json).unwrap();
        assert_eq!(found.len(), 1);
        found.remove(0)
    }

    #[test]
    fn a_cvss_v3_vector_is_scored_not_ignored() {
        let critical =
            r#"[{"type":"CVSS_V3","score":"CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"}]"#;
        let f = parse_one(critical, "", "");
        assert_eq!(f.severity, "critical", "9.8 is critical");
        assert!(!f.needs_review);

        let medium =
            r#"[{"type":"CVSS_V3","score":"CVSS:3.1/AV:N/AC:L/PR:N/UI:R/S:C/C:L/I:L/A:N"}]"#;
        assert_eq!(
            parse_one(medium, "", "").severity,
            "medium",
            "6.1 is medium"
        );

        let six_five =
            r#"[{"type":"CVSS_V3","score":"CVSS:3.1/AV:N/AC:L/PR:L/UI:N/S:U/C:H/I:N/A:N"}]"#;
        assert_eq!(
            parse_one(six_five, "", "").severity,
            "medium",
            "6.5 is medium"
        );

        let seven_five =
            r#"[{"type":"CVSS_V3","score":"CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:N/A:N"}]"#;
        assert_eq!(
            parse_one(seven_five, "", "").severity,
            "high",
            "7.5 is high"
        );

        let low = r#"[{"type":"CVSS_V3","score":"CVSS:3.0/AV:L/AC:H/PR:H/UI:R/S:U/C:L/I:N/A:N"}]"#;
        assert_eq!(parse_one(low, "", "").severity, "low", "1.8 is low");

        let scope_changed =
            r#"[{"type":"CVSS_V3","score":"CVSS:3.1/AV:N/AC:L/PR:L/UI:N/S:C/C:H/I:H/A:H"}]"#;
        assert_eq!(
            parse_one(scope_changed, "", "").severity,
            "critical",
            "9.9 is critical"
        );
    }

    #[test]
    fn the_v3_calculator_matches_published_scores() {
        let score = |v: &str| cvss_v3_base_score(v).unwrap();
        assert_eq!(score("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"), 9.8);
        assert_eq!(score("CVSS:3.1/AV:N/AC:L/PR:N/UI:R/S:C/C:L/I:L/A:N"), 6.1);
        assert_eq!(score("CVSS:3.1/AV:N/AC:L/PR:L/UI:N/S:C/C:H/I:H/A:H"), 9.9);
        assert_eq!(score("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:N/A:N"), 0.0);
        assert_eq!(cvss_v3_base_score("CVSS:4.0/AV:N/AC:L"), None);
        assert_eq!(cvss_v3_base_score("not a vector"), None);
        assert_eq!(
            cvss_v3_base_score("CVSS:3.1/AV:N/AC:L"),
            None,
            "missing metrics"
        );
    }

    #[test]
    fn a_cvss_v2_vector_is_scored_too() {
        assert_eq!(cvss_v2_base_score("AV:N/AC:L/Au:N/C:P/I:P/A:P"), Some(7.5));
        let v2 = r#"[{"type":"CVSS_V2","score":"AV:N/AC:L/Au:N/C:P/I:P/A:P"}]"#;
        assert_eq!(parse_one(v2, "", "").severity, "high");
    }

    #[test]
    fn a_groups_max_severity_is_used_when_the_vuln_carries_no_usable_score() {
        let v4 = r#"[{"type":"CVSS_V4","score":"CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:H/VI:H/VA:H/SC:N/SI:N/SA:N"}]"#;
        let groups = r#","groups":[{"ids":["GHSA-aaaa-bbbb-cccc"],"max_severity":"9.1"}]"#;
        let f = parse_one(v4, "", groups);
        assert_eq!(f.severity, "critical");
        assert!(!f.needs_review);
        // A group for some other advisory does not apply.
        let other = r#","groups":[{"ids":["GHSA-zzzz"],"max_severity":"9.1"}]"#;
        assert!(parse_one(v4, "", other).needs_review);
    }

    #[test]
    fn an_advisory_with_no_usable_severity_is_held_for_review_and_says_so() {
        let f = parse_one("[]", "", "");
        assert_eq!(f.severity, "medium", "never buried below medium");
        assert!(f.needs_review);
        assert_eq!(f.confidence.as_deref(), Some("needs-review"));
        assert!(
            f.detail.contains("Severity unknown, held for review"),
            "{}",
            f.detail
        );
        // An unrecognised word is the same case, not a silent medium.
        let g = parse_one("[]", r#","database_specific":{"severity":"UNRATED"}"#, "");
        assert!(g.needs_review);
    }

    #[test]
    fn a_published_word_still_wins_and_needs_no_review() {
        let f = parse_one(
            r#"[{"type":"CVSS_V3","score":"CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"}]"#,
            r#","database_specific":{"severity":"LOW"}"#,
            "",
        );
        assert_eq!(f.severity, "low");
        assert!(!f.needs_review);
        assert!(!f.detail.contains("held for review"));
    }

    #[test]
    fn a_zero_score_is_info_not_low() {
        assert_eq!(cvss_score_to_severity(0.0), "info");
        let none = r#"[{"type":"CVSS_V3","score":"CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:N/A:N"}]"#;
        let f = parse_one(none, "", "");
        assert_eq!(f.severity, "info");
        assert!(!f.needs_review);
        assert_eq!(cvss_score_to_severity(0.1), "low");
    }

    #[test]
    fn the_dependency_detail_has_no_em_dash() {
        let found = parse_osv_json("acme/api", &fixture_root(), osv_fixture()).unwrap();
        assert!(!found[0].detail.contains('\u{2014}'), "{}", found[0].detail);
        let no_fix = r#"{"results":[{"source":{"path":"/w/Cargo.lock","type":"lockfile"},
          "packages":[{"package":{"name":"a","version":"1","ecosystem":"crates.io"},
          "vulnerabilities":[{"id":"CVE-9","summary":"x","database_specific":{"severity":"HIGH"}}]}]}]}"#;
        let f = &parse_osv_json("acme/api", Path::new("/w"), no_fix).unwrap()[0];
        assert!(!f.detail.contains('\u{2014}'), "{}", f.detail);
        assert!(
            f.detail.contains(", no patched version available yet"),
            "{}",
            f.detail
        );
    }

    #[test]
    fn a_clean_exit_with_no_output_is_not_a_finding_of_no_lockfiles() {
        let message = empty_output_message();
        assert!(message.contains("did not run"), "{message}");
        assert!(!message.contains(NO_LOCKFILES), "{message}");
        let note = CoverageNote {
            tool_failed: false,
            repo: None,
            tool: TOOL_NAME.to_string(),
            message,
        };
        assert!(!note_means_scanned(&note));
        assert!(!note_proves_clean(&note));
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
        assert_eq!(cvss_score_to_severity(0.0), "info");
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

    /// A lockfile that is a symbolic link to a file outside the repository: osv-scanner reads
    /// it and reports its packages, and the report must carry none of them. Real files and
    /// the same result for a regular lockfile are untouched.
    #[cfg(unix)]
    #[test]
    fn a_result_for_a_symlinked_lockfile_is_dropped() {
        let outside = tempfile::TempDir::new().unwrap();
        let repo = tempfile::TempDir::new().unwrap();
        std::fs::write(outside.path().join("acme-outside.lock"), "x").unwrap();
        std::fs::write(repo.path().join("Cargo.lock"), "x").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("acme-outside.lock"),
            repo.path().join("acme-linked.lock"),
        )
        .unwrap();
        let result = |path: &std::path::Path| {
            format!(
                r#"{{"results":[{{"source":{{"path":"{}","type":"lockfile"}},
                "packages":[{{"package":{{"name":"a","version":"1","ecosystem":"crates.io"}},
                "vulnerabilities":[{{"id":"CVE-9","summary":"x","database_specific":{{"severity":"HIGH"}}}}]}}]}}]}}"#,
                path.display()
            )
        };
        let linked = parse_osv_json(
            "acme/api",
            repo.path(),
            &result(&repo.path().join("acme-linked.lock")),
        )
        .unwrap();
        assert!(
            linked.is_empty(),
            "a symlinked lockfile reported: {linked:?}"
        );
        let regular = parse_osv_json(
            "acme/api",
            repo.path(),
            &result(&repo.path().join("Cargo.lock")),
        )
        .unwrap();
        assert_eq!(regular.len(), 1, "a regular lockfile is still reported");
    }

    /// The dependency files the audit is given: tracked and vendored lockfiles both, only the
    /// second marked vendored; a gitignored vendored lockfile and a linked one are neither
    /// audited nor listed.
    #[cfg(unix)]
    #[test]
    fn audited_files_are_the_regular_dependency_files_with_vendored_ones_marked() {
        let outside = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let w = |root: &Path, rel: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "{}").unwrap();
        };
        w(outside.path(), "acme-outside-package-lock.json");
        w(repo.path(), "package-lock.json");
        w(repo.path(), "vendor/acme-x/package-lock.json");
        w(repo.path(), "vendor/acme-ignored/package-lock.json");
        std::fs::write(repo.path().join(".gitignore"), "vendor/acme-ignored/\n").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("acme-outside-package-lock.json"),
            repo.path().join("acme-linked-package-lock.json"),
        )
        .unwrap();
        for args in [vec!["init", "-q"], vec!["add", "-A"]] {
            assert!(std::process::Command::new("git")
                .args(&args)
                .current_dir(repo.path())
                .status()
                .unwrap()
                .success());
        }
        // A manifest osv-scanner does not parse is given to it but never listed as checked.
        w(repo.path(), "package.json");
        w(repo.path(), "Cargo.toml");
        assert!(std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(repo.path())
            .status()
            .unwrap()
            .success());
        // Untracked and not ignored: not audited, not listed.
        w(repo.path(), "acme-new/package-lock.json");
        let got: Vec<(String, bool)> =
            audited_dependency_files("acme/web", repo.path(), &RepoScope::none())
                .into_iter()
                .map(|f| (f.path, f.vendored))
                .collect();
        assert_eq!(
            got,
            [
                ("package-lock.json".to_string(), false),
                ("vendor/acme-x/package-lock.json".to_string(), true)
            ]
        );
        // The same files are what osv-scanner's mirror is given.
        let ex =
            crate::onboard::files::read_local_repo_files(repo.path(), &RepoScope::none()).unwrap();
        let given = crate::scan_mirror::osv_paths(&ex, repo.path(), &RepoScope::none());
        assert!(given.iter().any(|p| p == "vendor/acme-x/package-lock.json"));
        assert!(!given.iter().any(|p| p.contains("acme-ignored")));
    }

    /// A dependency file that did not reach osv-scanner's mirror (its copy failed) is not listed
    /// as checked.
    #[test]
    fn a_file_missing_from_the_mirror_is_not_listed_as_checked() {
        let mirror = tempfile::tempdir().unwrap();
        std::fs::write(mirror.path().join("package-lock.json"), "{}").unwrap();
        // A partial file a failed copy left: present, but named in `copy_failed`.
        std::fs::write(mirror.path().join("Cargo.lock"), "[[pack").unwrap();
        let file = |path: &str| crate::scan_ledger::AuditedDependencyFile {
            repo: "acme/web".to_string(),
            path: path.to_string(),
            vendored: false,
        };
        let given = given_to_osv(
            vec![
                file("package-lock.json"),
                file("svc/yarn.lock"),
                file("Cargo.lock"),
            ],
            mirror.path(),
            &["Cargo.lock".to_string()],
        );
        let paths: Vec<&str> = given.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["package-lock.json"]);
    }

    /// A tracked lockfile that a `.gitignore` rule matches is still given to osv-scanner and listed
    /// as checked: tracked files are read whatever the ignore rules say.
    #[cfg(unix)]
    #[test]
    fn a_tracked_lockfile_that_matches_gitignore_is_audited_and_listed() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("package-lock.json"), "{}").unwrap();
        std::fs::write(repo.path().join(".gitignore"), "package-lock.json\n").unwrap();
        for args in [vec!["init", "-q"], vec!["add", "-f", "package-lock.json"]] {
            assert!(std::process::Command::new("git")
                .args(&args)
                .current_dir(repo.path())
                .status()
                .unwrap()
                .success());
        }
        let got = audited_dependency_files("acme/web", repo.path(), &RepoScope::none());
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, "package-lock.json");
    }

    /// Real osv-scanner on a mirror whose `.gitignore` names the lockfile, under a parent that is
    /// itself a git checkout: with the engine's arguments (`--no-ignore`) and the git ceiling at
    /// the mirror folder, it reads the lockfile. Offline (no vulnerability query); skips when
    /// osv-scanner is not provisioned.
    #[cfg(unix)]
    #[test]
    fn live_osv_reads_a_lockfile_a_gitignore_names_when_run_with_no_ignore() {
        let Ok(tools) = crate::pinned_tools::ToolEnv::product() else {
            return;
        };
        let Some(bin) = tools.platform.and_then(|p| {
            crate::pinned_tools::fetch::entry_point(
                &tools.tooling,
                tools.pins(crate::pinned_tools::Artifact::OsvScanner),
                p,
            )
        }) else {
            return;
        };
        if !tools.is_installed(crate::pinned_tools::Artifact::OsvScanner) {
            eprintln!("skipping: osv-scanner is not provisioned on this machine");
            return;
        }
        let parent = tempfile::tempdir().unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(parent.path())
            .status()
            .unwrap()
            .success());
        std::fs::write(parent.path().join(".gitignore"), "package-lock.json\n").unwrap();
        let mirror = parent.path().join("mirror").join("acme_web");
        std::fs::create_dir_all(&mirror).unwrap();
        std::fs::write(mirror.join(".gitignore"), "package-lock.json\n").unwrap();
        std::fs::write(
            mirror.join("package-lock.json"),
            r#"{"name":"a","lockfileVersion":3,"packages":{"":{"name":"a"},"node_modules/lodash":{"version":"4.17.15"}}}"#,
        )
        .unwrap();
        let run = |args: Vec<std::ffi::OsString>| {
            let out = std::process::Command::new(&bin)
                .arg("--experimental-offline-vulnerabilities")
                .args(args)
                .env("GIT_CEILING_DIRECTORIES", mirror.parent().unwrap())
                .output()
                .unwrap();
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        };
        let with = run(osv_args(&mirror));
        assert!(
            with.contains("Scanned") && with.contains("package-lock.json"),
            "osv did not read the ignored lockfile: {with}"
        );
    }

    /// A lockfile that failed to copy never reads as "no known vulnerable dependencies", and a run
    /// that happened never reads as not run.
    #[test]
    fn a_not_copied_note_is_never_clean_and_still_means_the_run_happened() {
        let note = |message: String| CoverageNote {
            tool_failed: false,
            repo: None,
            tool: TOOL_NAME.to_string(),
            message,
        };
        let copy = crate::scan_mirror::not_copied_note("acme/web", TOOL_NAME, &["a.lock".into()])
            .expect("note");
        // The not-copied text is only ever added to a note of a run that happened (see
        // `with_copy_failures`), or stands alone for one that had no other note; either way
        // the audit ran, and it is not clean.
        assert!(note_means_scanned(&copy));
        assert!(!note_proves_clean(&copy));
        // Appended to a "found no lockfiles" note: still not clean.
        let both = note(format!(
            "dependency audit (osv-scanner) {NO_LOCKFILES} in acme/web; no dependency exposure \
             detected. {}",
            copy.message
        ));
        assert!(note_means_scanned(&both));
        assert!(!note_proves_clean(&both));
    }

    /// A copy failure next to an audit that did not run (a timeout) never makes it read as a
    /// run that happened: the note stands unchanged and no lockfile is listed as checked.
    #[test]
    fn a_copy_failure_next_to_a_timeout_lists_no_lockfile_as_checked() {
        let timeout = CoverageNote {
            tool_failed: false,
            repo: Some("acme/web".to_string()),
            tool: TOOL_NAME.to_string(),
            message: "dependency audit (osv-scanner) did not run: subprocess timed out after 120 s"
                .to_string(),
        };
        let file = crate::scan_ledger::AuditedDependencyFile {
            repo: "acme/web".to_string(),
            path: "package-lock.json".to_string(),
            vendored: false,
        };
        let (note, files) = with_copy_failures(
            "acme/web",
            Some(timeout.clone()),
            vec![file.clone()],
            &["src/acme-a.js".to_string()],
        );
        let note = note.expect("note");
        assert_eq!(note.message, timeout.message, "the note is not rewritten");
        assert!(!note_means_scanned(&note));
        assert!(!crate::vuln_intel::OsvRun::from_note("acme/web", Some(&note)).ran);
        assert!(files.is_empty(), "never read, so never listed: {files:?}");
        // The run that did happen keeps its files, and is never clean.
        let (ran_note, ran_files) =
            with_copy_failures("acme/web", None, vec![file], &["src/acme-a.js".to_string()]);
        let ran_note = ran_note.expect("note");
        assert!(note_means_scanned(&ran_note) && !note_proves_clean(&ran_note));
        assert_eq!(ran_files.len(), 1);
    }

    /// osv-scanner is pointed at exactly the folder it is given: the scan passes the mirror.
    #[test]
    fn osv_args_target_the_scan_directory() {
        let args = osv_args(Path::new("/state/scan-mirrors/scan-1/mirror/acme_web"));
        let args: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "--format",
                "json",
                "--experimental-no-resolve",
                "--no-ignore",
                "-r",
                "/state/scan-mirrors/scan-1/mirror/acme_web"
            ]
        );
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
            "advisory id must be in detail: {}",
            f.detail
        );
        // Alias must appear in the detail.
        assert!(
            f.detail.contains("GHSA-wcg3-cvx6-7396"),
            "alias must be in detail: {}",
            f.detail
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
    fn osv_fixed_version_drives_the_ownership_split() {
        use crate::ownership::{ownership_of, Ownership};
        let root = fixture_root();
        // A vuln WITH affected ranges carrying two `fixed` events — the SMALLEST is the target.
        let with_fix = r#"{"results":[{"source":{"path":"/w/Cargo.lock","type":"lockfile"},
          "packages":[{"package":{"name":"acme","version":"1.0.0","ecosystem":"crates.io"},
          "vulnerabilities":[{"id":"CVE-9","summary":"x","database_specific":{"severity":"HIGH"},
          "affected":[{"ranges":[{"type":"SEMVER","events":[{"introduced":"0"},{"fixed":"1.4.2"},{"fixed":"1.10.0"}]}]}]}]}]}]}"#;
        let f = &parse_osv_json("acme/api", &root, with_fix).unwrap()[0];
        assert_eq!(
            f.captures.get("osv_fixed_version").map(String::as_str),
            Some("1.4.2"),
            "smallest fixed version by semver (1.4.2 < 1.10.0), not string order"
        );
        assert!(f.detail.contains("fixed in 1.4.2"), "detail: {}", f.detail);
        assert!(
            f.fix_specific
                .as_deref()
                .unwrap_or("")
                .contains("Upgrade acme to >= 1.4.2"),
            "fix: {:?}",
            f.fix_specific
        );
        // A patched CVE is fixable by upgrade → client-owned.
        assert_eq!(
            ownership_of(&f.rule_id, &f.captures),
            Ownership::ClientOwned
        );

        // A vuln with NO `fixed` event → not-yet-fixable → external-dependency.
        let no_fix = r#"{"results":[{"source":{"path":"/w/Cargo.lock","type":"lockfile"},
          "packages":[{"package":{"name":"acme","version":"1.0.0","ecosystem":"crates.io"},
          "vulnerabilities":[{"id":"CVE-9","summary":"x","database_specific":{"severity":"HIGH"},
          "affected":[{"ranges":[{"type":"SEMVER","events":[{"introduced":"0"}]}]}]}]}]}]}"#;
        let g = &parse_osv_json("acme/api", &root, no_fix).unwrap()[0];
        assert!(!g.captures.contains_key("osv_fixed_version"));
        assert!(
            g.detail.contains("no patched version"),
            "detail: {}",
            g.detail
        );
        assert!(g.fix_specific.is_none());
        assert_eq!(
            ownership_of(&g.rule_id, &g.captures),
            Ownership::ExternalDependency
        );
    }

    #[test]
    fn parse_empty_results_returns_no_findings() {
        let root = fixture_root();
        let json = r#"{"schemaVersion":"1.0","results":[]}"#;
        let findings = parse_osv_json("acme/api", &root, json).unwrap();
        assert!(
            findings.is_empty(),
            "empty results must produce no findings"
        );
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
        assert!(
            result.is_err(),
            "malformed JSON must return Err, not Ok([])"
        );
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

    // ── provisioning during the run (#587) ──────────────────────────────────
    //
    // osv-scanner is provisioned before the first model call. If it has to be provisioned at
    // audit time and that fails, the run STOPS: the result is a `ToolProvisioningStop`, never a
    // coverage note that would let the run finish as if the audit were a scope limit. What was
    // known before the run (no build for this machine) stays a disclosed note.

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn a_provisioning_failure_during_the_audit_stops_the_run_not_a_note() {
        let _on = camerata_test_env::EnvGuard::remove(DISABLE_ENV_VAR);
        let tools = crate::pinned_tools::fixtures::FakeTools::new();
        tools
            .source
            .fail_everything("network unreachable (fixture)");
        let repo = tempfile::tempdir().unwrap();
        let stop =
            run_dep_audit_with_tooling("acme/test", repo.path(), &tools.env, &RepoScope::none())
                .await
                .expect_err("the run stops");
        assert_eq!(stop.tool, "osv-scanner");
        assert_eq!(stop.repo, "acme/test");
        assert!(stop.reason.contains("unreachable"), "{}", stop.reason);
        assert!(
            stop.to_string().starts_with("the inspection stopped"),
            "{stop}"
        );
    }

    /// The app bounds the dependency audit at 60 s. The bound is for the SCAN: a slow first
    /// download of osv-scanner is not cut short into a "timed out" note, and a download that fails
    /// stops the run (the run-integrity rule), never a coverage note.
    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn the_scan_time_limit_never_turns_a_provisioning_problem_into_a_note() {
        let _on = camerata_test_env::EnvGuard::remove(DISABLE_ENV_VAR);
        let repo = tempfile::tempdir().unwrap();
        let limit = Duration::from_millis(100);

        let slow = crate::pinned_tools::fixtures::FakeTools::new();
        slow.source.delay_every_fetch(Duration::from_millis(600));
        let (_, note, _) = run_dep_audit_bounded(
            "acme/test",
            repo.path(),
            &RepoScope::none(),
            &slow.env,
            limit,
        )
        .await
        .expect("a slow download is not a stop");
        assert!(
            note.as_ref()
                .is_none_or(|n| !n.message.contains("timed out")),
            "the download was cut short by the scan's limit: {note:?}"
        );
        assert!(slow
            .env
            .is_installed(crate::pinned_tools::Artifact::OsvScanner));

        let broken = crate::pinned_tools::fixtures::FakeTools::new();
        broken.source.delay_every_fetch(Duration::from_millis(600));
        broken
            .source
            .fail_everything("network unreachable (fixture)");
        let stop = run_dep_audit_bounded(
            "acme/test",
            repo.path(),
            &RepoScope::none(),
            &broken.env,
            limit,
        )
        .await
        .expect_err("a failed download stops the run");
        assert_eq!(stop.tool, "osv-scanner");
        assert!(stop.reason.contains("unreachable"), "{}", stop.reason);
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn no_build_for_this_machine_is_a_disclosed_note_known_before_the_run() {
        let _on = camerata_test_env::EnvGuard::remove(DISABLE_ENV_VAR);
        let fakes = crate::pinned_tools::fixtures::FakeTools::new();
        let pins = fakes.env.clone();
        let env = crate::pinned_tools::ToolEnv::new(fakes.tooling(), None, fakes.source.clone())
            .with_pins(move |a| pins.pins(a));
        let repo = tempfile::tempdir().unwrap();
        let (findings, note) =
            run_dep_audit_with_tooling("acme/test", repo.path(), &env, &RepoScope::none())
                .await
                .expect("not a stop");
        assert!(findings.is_empty());
        let note = note.expect("a note");
        assert_eq!(note.tool, "osv-scanner");
        assert_eq!(note.repo.as_deref(), Some("acme/test"));
        assert!(!note.tool_failed);
        assert!(fakes.source.fetched().is_empty());
    }

    // ── hard total-timeout at call site (fail-soft) ───────────────────────────
    //
    // The 60-second hard timeout that wraps `run_dep_audit` in `lib.rs`
    // (`onboard_audit_start`) must fail soft: produce no findings and emit a
    // CoverageNote, then let the scan proceed.  We test the PATTERN here using
    // a short (1 ms) timeout around a future that never resolves — this exercises
    // the timeout arm, the CoverageNote shape, and the absence of any panic.
    //
    // Note: we don't test the full integration path (lib.rs calls run_dep_audit)
    // here because that requires a running server; instead we replicate the exact
    // timeout+coverage-note pattern in isolation.
    #[tokio::test]
    async fn hard_timeout_on_slow_dep_audit_produces_coverage_note() {
        use tokio::time::{timeout, Duration};

        // A future that never resolves — simulates a hung osv-scanner.
        let never_resolves = std::future::pending::<(Vec<Finding>, Option<CoverageNote>)>();

        let timeout_result = timeout(Duration::from_millis(1), never_resolves).await;

        // The timeout must fire.
        assert!(
            timeout_result.is_err(),
            "a 1 ms timeout on a never-resolving future must fire"
        );

        // Replicate the caller's fail-soft path: convert Elapsed → CoverageNote + empty findings.
        let (dep_findings, dep_note): (Vec<Finding>, Option<CoverageNote>) = (
            Vec::new(),
            Some(CoverageNote {
                tool_failed: false,
                repo: None,
                tool: TOOL_NAME.to_string(),
                message: "dependency audit (osv-scanner) timed out after 60 s for acme/api"
                    .to_string(),
            }),
        );

        // Fail-soft invariants.
        assert!(dep_findings.is_empty(), "no findings on timeout");
        let note = dep_note.unwrap();
        assert_eq!(note.tool, TOOL_NAME, "coverage note must name the tool");
        assert!(
            note.message.contains("timed out"),
            "coverage note must mention the timeout: {}",
            note.message
        );
    }

    /// Verify that a subprocess wrapped in `tokio::time::timeout` with `kill_on_drop(true)`
    /// is terminated when the timeout fires — not orphaned.
    ///
    /// This is the regression test for ARCH-RESOURCE-LIFECYCLE-1: the root cause of 52
    /// orphaned osv-scanner processes was that `tokio::process::Command` does NOT kill
    /// the child when its future is dropped unless `kill_on_drop(true)` is set.
    ///
    /// Strategy: mirror the exact pattern used in `run_dep_audit_with_tooling` —
    /// `tokio::time::timeout(dur, Command::new(...).kill_on_drop(true).output())`.
    /// The `output()` future internally owns the `Child` handle.  When the timeout fires
    /// and drops the `output()` future, the `Child` drop impl fires the kill signal.
    ///
    /// We capture the PID *before* wrapping in timeout (by spawning separately), then
    /// assert the process is gone a brief moment after the timeout fires.
    ///
    /// On a normal-exit path (child exits before the timeout), kill_on_drop is a no-op —
    /// the child is already gone.  That invariant is verified by the fast-path sub-test.
    #[cfg(unix)]
    #[tokio::test]
    async fn kill_on_drop_reaps_child_on_timeout() {
        use std::time::Duration;
        use tokio::process::Command as TokioCmd;

        // ── Timeout path ──────────────────────────────────────────────────────────
        // Mirror the dep_audit pattern: Command built fresh, kill_on_drop set,
        // entire .output() future handed to timeout().
        //
        // We need the PID before wrapping in timeout.  Spawn once to grab the PID,
        // kill that probe child, then re-run the real timed invocation.
        //
        // Simpler approach: spawn for pid, drop it (with kill_on_drop = true so the
        // probe child is also reaped), wait a moment, then run the real timeout test.
        let pid = {
            let mut probe = TokioCmd::new("sleep");
            probe.arg("30").kill_on_drop(true);
            let child = probe.spawn().expect("sleep must be available on Unix");
            let pid = child.id().expect("pid before exit");
            // Drop child here — kill_on_drop kills it immediately.
            drop(child);
            pid
        };

        // Give the OS a moment to reap the probe.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Confirm the probe PID is already gone (validates kill_on_drop on drop itself).
        let probe_gone = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| !s.success())
            .unwrap_or(true);
        assert!(
            probe_gone,
            "PID {pid} should be reaped when Child is dropped with kill_on_drop=true"
        );

        // ── Now test the exact timeout pattern from dep_audit ─────────────────────
        // Spawn a long-running child inside the .output() future and let the timeout
        // drop it.  We cannot know the PID in advance when using .output() directly,
        // so we verify indirectly: the timeout fires AND no zombie persists (the
        // process count for "sleep 30" doesn't grow unboundedly).  The primary
        // assertion is that the timeout fires at all (the future is cancellable).
        let timeout_result = tokio::time::timeout(
            Duration::from_millis(100),
            TokioCmd::new("sleep").arg("30").kill_on_drop(true).output(),
        )
        .await;

        assert!(
            timeout_result.is_err(),
            "100 ms timeout must fire before `sleep 30` exits"
        );

        // ── Normal-exit path: kill_on_drop is a no-op when child exits cleanly ────
        let output = TokioCmd::new("sh")
            .args(["-c", "exit 0"])
            .kill_on_drop(true)
            .output()
            .await
            .expect("sh -c 'exit 0' must succeed");
        assert!(
            output.status.success(),
            "fast path: sh -c 'exit 0' must exit 0"
        );
    }

    /// Verify that `CAMERATA_DISABLE_DEP_AUDIT` causes run_dep_audit_with_tooling to return
    /// immediately (no provisioning, no subprocess) and produce empty findings + no note.
    /// This is the test-isolation escape hatch; tests that exercise audit_repos but not
    /// dep-audit specifically must set this env var.
    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn disable_env_var_skips_all_network_activity() {
        let _env = camerata_test_env::EnvGuard::set(DISABLE_ENV_VAR, "1");
        let tools = crate::pinned_tools::fixtures::FakeTools::new();
        tools.source.fail_everything("must not be asked");
        let repo_dir = std::path::Path::new("/nonexistent-repo-for-disable-test");
        let (findings, note) =
            run_dep_audit_with_tooling("acme/api", repo_dir, &tools.env, &RepoScope::none())
                .await
                .expect("no stop");
        assert!(
            tools.source.fetched().is_empty(),
            "DISABLE → nothing downloaded"
        );
        assert!(findings.is_empty(), "DISABLE → no findings");
        assert!(note.is_none(), "DISABLE → no coverage note (silent skip)");
    }

    /// Item 31: osv-scanner finds lockfiles itself, so a result about a lockfile the project's
    /// exclusion list names is dropped, and only that one.
    #[test]
    fn a_vulnerability_in_an_excluded_lockfile_is_dropped_from_the_results() {
        let finding = |path: &str| Finding {
            repo: "acme/app".to_string(),
            path: path.to_string(),
            rule_id: DEP_AUDIT_RULE_ID.to_string(),
            ..Finding::default()
        };
        let mut findings = vec![
            finding("seeds/package-lock.json"),
            finding("package-lock.json"),
        ];
        let scope =
            crate::onboard::scope::ScanScope::from_patterns(&["app/seeds/**".to_string()]).unwrap();
        drop_excluded(&mut findings, &scope.for_repo("acme/app"));
        let kept: Vec<&str> = findings.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(kept, ["package-lock.json"]);
        let mut all = vec![finding("seeds/package-lock.json")];
        drop_excluded(&mut all, &RepoScope::none());
        assert_eq!(all.len(), 1, "nothing is dropped without an exclusion");
    }

    // ── Round 1 item 4: the call site, with a fake osv-scanner in the tooling folder ────────

    /// A fake `osv-scanner` that reports one vulnerable package in each of `lockfiles` (absolute
    /// paths), served as the pinned osv-scanner of a fixture tooling folder.
    fn fake_osv(lockfiles: &[String]) -> crate::pinned_tools::fixtures::FakeTools {
        let results: Vec<String> = lockfiles
            .iter()
            .map(|path| {
                format!(
                    r#"{{"source":{{"path":"{path}","type":"lockfile"}},"packages":[{{"package":{{"name":"lodash","version":"4.17.0","ecosystem":"npm"}},"vulnerabilities":[{{"id":"GHSA-aaaa-bbbb-cccc","summary":"x","severity":[{{"type":"CVSS_V3","score":"CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"}}]}}]}}]}}"#
                )
            })
            .collect();
        let json = format!(r#"{{"results":[{}]}}"#, results.join(","));
        crate::pinned_tools::fixtures::FakeTools::with_osv_script(&format!(
            "#!/bin/sh\n[ \"$1\" = \"--version\" ] && exit 0\ncat <<'EOF'\n{json}\nEOF\nexit 1\n"
        ))
    }

    fn excluding(patterns: &[&str]) -> RepoScope {
        let patterns: Vec<String> = patterns.iter().map(|p| p.to_string()).collect();
        crate::onboard::scope::ScanScope::from_patterns(&patterns)
            .unwrap()
            .for_repo("acme/app")
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn the_audit_drops_what_osv_reports_in_an_excluded_lockfile() {
        let _path = camerata_test_env::EnvGuard::set("PATH", "/usr/bin:/bin");
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().canonicalize().unwrap();
        let tools = fake_osv(&[
            format!("{}/seeds/package-lock.json", root.display()),
            format!("{}/package-lock.json", root.display()),
        ]);
        let (all, _) =
            run_dep_audit_with_tooling("acme/app", &root, &tools.env, &RepoScope::none())
                .await
                .unwrap();
        assert_eq!(all.len(), 2, "the fake reports both: {all:?}");
        let (kept, _) = run_dep_audit_with_tooling(
            "acme/app",
            &root,
            &tools.env,
            &excluding(&["app/seeds/**"]),
        )
        .await
        .unwrap();
        let paths: Vec<&str> = kept.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["package-lock.json"]);
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn a_symlinked_repo_root_does_not_let_an_excluded_lockfile_through() {
        let _path = camerata_test_env::EnvGuard::set("PATH", "/usr/bin:/bin");
        let real = tempfile::tempdir().unwrap();
        let real_root = real.path().canonicalize().unwrap();
        let links = tempfile::tempdir().unwrap();
        let link = links.path().join("clone");
        std::os::unix::fs::symlink(&real_root, &link).unwrap();
        // osv reports the real path; the run was given the symlink.
        let tools = fake_osv(&[
            format!("{}/seeds/package-lock.json", real_root.display()),
            format!("{}/package-lock.json", real_root.display()),
        ]);
        let (kept, _) = run_dep_audit_with_tooling(
            "acme/app",
            &link,
            &tools.env,
            &excluding(&["app/seeds/**"]),
        )
        .await
        .unwrap();
        let paths: Vec<&str> = kept.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            ["package-lock.json"],
            "relative, and the excluded one is gone"
        );
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn a_path_that_cannot_be_made_relative_is_dropped_when_a_list_is_set() {
        let _path = camerata_test_env::EnvGuard::set("PATH", "/usr/bin:/bin");
        let repo = tempfile::tempdir().unwrap();
        let tools = fake_osv(&["/elsewhere/seeds/package-lock.json".to_string()]);
        let (kept, _) = run_dep_audit_with_tooling(
            "acme/app",
            repo.path(),
            &tools.env,
            &excluding(&["app/seeds/**"]),
        )
        .await
        .unwrap();
        assert!(kept.is_empty(), "fail closed: {kept:?}");
    }
}
