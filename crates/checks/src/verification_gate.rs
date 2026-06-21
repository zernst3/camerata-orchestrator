//! Verification-mechanics gates over the rule corpus: the **deny-gate** (who may
//! set `verification = "verified"`) and the **staleness** demotion pass.
//!
//! These are the dogfood of the grounding ladder defined in
//! [`camerata_rules::Verification`]. They are deterministic and pure — string /
//! diff matchers and a version comparison, no LLM judgement, no network — so the
//! verdict is binary and reproducible, the same hard line the other gates hold.
//! See `docs/decisions/2026-06-20_verification_mechanics.md`.
//!
//! ## Deny-gate (`verified` is human-only)
//!
//! [`Verification::Verified`](camerata_rules::Verification::Verified) is the
//! strongest assertion the corpus can make and is **human-only**: no automated
//! process — and so no agent — may promote a rule to `verified` or add a
//! `[verified]` block. [`deny_verified_promotion`] inspects a diff/changeset to
//! the rule TOMLs under `crates/rules/principles/` and DENIES (deny-before-execute,
//! modelled on the [`crate::vcs_action`] gate) any change that introduces or
//! promotes `verification = "verified"` or adds a `[verified]` table. An agent may
//! set at most `grounded`.
//!
//! ## Staleness (drift → `needs_recheck`)
//!
//! A rule that is `verified` records, in its `[verified]` table, the source/linter
//! versions it was verified against (`against`). When any of those versions drifts
//! from the *current* version, the human confirmation can no longer be trusted, so
//! [`demote_if_stale`] demotes the status to
//! [`Verification::NeedsRecheck`](camerata_rules::Verification::NeedsRecheck). The
//! current-versions input is a clean seam (a passed-in map); live fetching is a
//! follow-up.

use std::collections::HashMap;

use camerata_rules::{Verification, VerifiedProvenance};

// ────────────────────────────────────────────────────────────────────────────
// Deny-gate: verification="verified" is human-only
// ────────────────────────────────────────────────────────────────────────────

/// The rule id this gate reports under (the `PROCESS-*` family of metadata gates).
pub const VERIFIED_HUMAN_ONLY_RULE_ID: &str = "PROCESS-VERIFIED-HUMAN-ONLY-1";

/// A single denial emitted by [`deny_verified_promotion`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenyVerifiedViolation {
    /// The rule id of this gate.
    pub rule_id: String,
    /// The offending added line (verbatim, trimmed of the leading `+`).
    pub offending_line: String,
    /// Human-readable explanation.
    pub detail: String,
}

/// The denial message — agents may set at most `grounded`.
const DENY_MESSAGE: &str = "verification=verified is human-only; agents may set at most grounded";

/// Scan a unified-diff `changeset` (against the rule TOMLs under
/// `crates/rules/principles/`) and DENY any change that introduces or promotes
/// `verification = "verified"` or adds a `[verified]` table.
///
/// Only **added** lines (those beginning with a single `+`, but not the `+++`
/// file header) are inspected — removing or leaving an existing `verified` line
/// untouched is not a promotion. This mirrors the deny-before-execute posture of
/// [`crate::vcs_action::gate`]: a non-empty result means the action is refused.
///
/// Detection (deterministic, no regex):
/// - an added line whose trimmed text sets `verification` to the string
///   `"verified"` (e.g. `verification = "verified"`), OR
/// - an added line that is a `[verified]` table header (e.g. `[verified]`).
///
/// Adding `verification = "grounded"` (or `"draft"`, or `"needs_recheck"`), or a
/// diff with no verification change, passes (returns an empty vec).
pub fn deny_verified_promotion(changeset: &str) -> Vec<DenyVerifiedViolation> {
    let mut violations = Vec::new();

    for raw in changeset.lines() {
        // Only added lines. `+++ ` is a diff file header, not content.
        let Some(added) = added_content(raw) else {
            continue;
        };
        let trimmed = added.trim();

        if is_verified_assignment(trimmed) || is_verified_table_header(trimmed) {
            violations.push(DenyVerifiedViolation {
                rule_id: VERIFIED_HUMAN_ONLY_RULE_ID.to_string(),
                offending_line: trimmed.to_string(),
                detail: format!(
                    "[{VERIFIED_HUMAN_ONLY_RULE_ID}] {DENY_MESSAGE} (offending: `{trimmed}`)"
                ),
            });
        }
    }

    violations
}

/// The deny-gate as a binary verdict: `Ok(())` when the changeset introduces no
/// `verified` promotion, else `Err(violations)`. Reusable by the governed-dev
/// gate as a deny-before-execute check.
pub fn gate_verified_promotion(changeset: &str) -> Result<(), Vec<DenyVerifiedViolation>> {
    let violations = deny_verified_promotion(changeset);
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

/// If `raw` is an added diff line (`+...` but not the `+++` header), return its
/// content with the single leading `+` stripped. Otherwise `None`.
fn added_content(raw: &str) -> Option<&str> {
    let rest = raw.strip_prefix('+')?;
    // `+++ b/path` is a file header, not content. A real added line never starts
    // with another `+` immediately after the first in our TOML corpus.
    if rest.starts_with("++") {
        return None;
    }
    Some(rest)
}

/// True when `trimmed` assigns `verification` to the literal `"verified"`.
///
/// Matches `verification = "verified"` with any whitespace around `=` and accepts
/// either quote style. Deliberately strict: it must be the `verification` key set
/// to exactly `verified` — `verification = "grounded"` etc. do not match.
fn is_verified_assignment(trimmed: &str) -> bool {
    let Some(rest) = trimmed.strip_prefix("verification") else {
        return false;
    };
    let rest = rest.trim_start();
    let Some(value) = rest.strip_prefix('=') else {
        return false;
    };
    let value = value.trim();
    value == "\"verified\"" || value == "'verified'"
}

/// True when `trimmed` is a `[verified]` table header (the provenance block whose
/// presence implies a human verification).
fn is_verified_table_header(trimmed: &str) -> bool {
    // Allow an optional trailing comment after the header.
    let head = trimmed.split('#').next().unwrap_or(trimmed).trim_end();
    head == "[verified]"
}

// ────────────────────────────────────────────────────────────────────────────
// Staleness: drift demotes Verified → NeedsRecheck
// ────────────────────────────────────────────────────────────────────────────

/// The outcome of a staleness check on a single rule's verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StalenessOutcome {
    /// The status is unchanged (not `verified`, or every cited version still matches).
    Unchanged(Verification),
    /// The status was `verified` but a cited version drifted; demoted to
    /// [`Verification::NeedsRecheck`]. Carries the entries that drifted.
    Demoted {
        /// The new status (always [`Verification::NeedsRecheck`]).
        new_status: Verification,
        /// The `against` entries whose current version differs (verified-against → current).
        drifted: Vec<VersionDrift>,
    },
}

/// One drifted source/linter: the version the rule was verified against vs. the
/// current version observed now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionDrift {
    /// The name component (e.g. `"clippy"`).
    pub name: String,
    /// The version the rule was verified against (e.g. `"1.83"`).
    pub verified_against: String,
    /// The current version observed now (e.g. `"1.84"`).
    pub current: String,
}

/// Pure staleness detection + demotion.
///
/// Given a rule's `status`, its [`VerifiedProvenance`] (the `[verified]` table,
/// when present), and a `current_versions` map (`name` → current version string),
/// determine whether the verification has gone stale:
///
/// - If `status` is not [`Verification::Verified`], the status is returned
///   unchanged — only a live human verification can go stale.
/// - Otherwise each `against` entry is parsed as `"<name> <version>"`; the
///   `<version>` is compared to `current_versions[name]`. If any cited version
///   differs from the current one, the rule is demoted to
///   [`Verification::NeedsRecheck`] and the drifted entries are reported.
/// - A `name` absent from `current_versions` is treated as "unknown / not drifted"
///   (we cannot prove drift), so it does not trigger a demotion on its own.
///
/// `current_versions` is the clean seam for the version input: callers pass a map
/// today; a live fetcher can populate it later (a follow-up).
pub fn demote_if_stale(
    status: Verification,
    provenance: Option<&VerifiedProvenance>,
    current_versions: &HashMap<String, String>,
) -> StalenessOutcome {
    // Only a live human verification can go stale.
    if status != Verification::Verified {
        return StalenessOutcome::Unchanged(status);
    }

    let Some(prov) = provenance else {
        // Verified but no recorded versions to check against → nothing to drift.
        return StalenessOutcome::Unchanged(status);
    };

    let mut drifted = Vec::new();
    for entry in &prov.against {
        let Some((name, verified_version)) = parse_against_entry(entry) else {
            continue; // unparseable entry; cannot prove drift
        };
        if let Some(current) = current_versions.get(name) {
            if current != verified_version {
                drifted.push(VersionDrift {
                    name: name.to_string(),
                    verified_against: verified_version.to_string(),
                    current: current.clone(),
                });
            }
        }
        // name not in current_versions → unknown, not counted as drift.
    }

    if drifted.is_empty() {
        StalenessOutcome::Unchanged(status)
    } else {
        StalenessOutcome::Demoted {
            new_status: Verification::NeedsRecheck,
            drifted,
        }
    }
}

/// Parse an `against` entry of the form `"<name> <version>"` into `(name, version)`.
///
/// The name is everything up to the LAST whitespace-separated token, which is the
/// version. This lets multi-word names like `"Google Java Style Guide 2024-01"`
/// parse as name `"Google Java Style Guide"` + version `"2024-01"`. Returns `None`
/// when there is no whitespace (no version component).
fn parse_against_entry(entry: &str) -> Option<(&str, &str)> {
    let trimmed = entry.trim();
    let idx = trimmed.rfind(char::is_whitespace)?;
    let name = trimmed[..idx].trim_end();
    let version = trimmed[idx..].trim_start();
    if name.is_empty() || version.is_empty() {
        return None;
    }
    Some((name, version))
}

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── deny-gate ──────────────────────────────────────────────────────────────

    #[test]
    fn diff_adding_verified_is_denied() {
        let diff = r#"--- a/crates/rules/principles/rust/RUST-X.toml
+++ b/crates/rules/principles/rust/RUST-X.toml
@@ -1,3 +1,3 @@
 id = "RUST-X"
-verification = "grounded"
+verification = "verified"
"#;
        let v = deny_verified_promotion(diff);
        assert_eq!(v.len(), 1, "exactly one denial: {v:?}");
        assert_eq!(v[0].rule_id, VERIFIED_HUMAN_ONLY_RULE_ID);
        assert!(v[0].detail.contains("human-only"));
        assert!(gate_verified_promotion(diff).is_err());
    }

    #[test]
    fn diff_adding_verified_table_is_denied() {
        let diff = r#"+++ b/crates/rules/principles/rust/RUST-Y.toml
@@ -1,1 +1,4 @@
 id = "RUST-Y"
+
+[verified]
+by = "some-agent"
"#;
        let v = deny_verified_promotion(diff);
        assert!(
            v.iter().any(|x| x.offending_line == "[verified]"),
            "the [verified] header must be denied: {v:?}"
        );
        assert!(gate_verified_promotion(diff).is_err());
    }

    #[test]
    fn diff_adding_grounded_passes() {
        let diff = r#"--- a/crates/rules/principles/rust/RUST-Z.toml
+++ b/crates/rules/principles/rust/RUST-Z.toml
@@ -1,2 +1,2 @@
 id = "RUST-Z"
-verification = "draft"
+verification = "grounded"
"#;
        assert!(deny_verified_promotion(diff).is_empty());
        assert!(gate_verified_promotion(diff).is_ok());
    }

    #[test]
    fn diff_with_no_verification_change_passes() {
        let diff = r#"--- a/crates/rules/principles/rust/RUST-W.toml
+++ b/crates/rules/principles/rust/RUST-W.toml
@@ -1,2 +1,3 @@
 id = "RUST-W"
+title = "A new title line"
"#;
        assert!(deny_verified_promotion(diff).is_empty());
        assert!(gate_verified_promotion(diff).is_ok());
    }

    #[test]
    fn removing_a_verified_line_is_not_a_promotion() {
        // A removed `verification = "verified"` (leading `-`) is not an added line.
        let diff = r#"--- a/x.toml
+++ b/x.toml
@@ -1,2 +1,1 @@
-verification = "verified"
 id = "X"
"#;
        assert!(deny_verified_promotion(diff).is_empty());
    }

    #[test]
    fn plus_plus_plus_file_header_is_not_treated_as_content() {
        // The +++ header path could theoretically contain "verified"; it must not match.
        let diff = "+++ b/crates/rules/principles/verified/whatever.toml\n id = \"X\"\n";
        assert!(deny_verified_promotion(diff).is_empty());
    }

    #[test]
    fn verified_assignment_matcher_is_strict() {
        assert!(is_verified_assignment(r#"verification = "verified""#));
        assert!(is_verified_assignment(r#"verification="verified""#));
        assert!(is_verified_assignment(r#"verification = 'verified'"#));
        assert!(!is_verified_assignment(r#"verification = "grounded""#));
        assert!(!is_verified_assignment(r#"verification = "needs_recheck""#));
        // A comment mentioning verified must not match.
        assert!(!is_verified_assignment(
            r#"# verification = "verified" someday"#
        ));
    }

    #[test]
    fn verified_table_header_matcher_allows_trailing_comment() {
        assert!(is_verified_table_header("[verified]"));
        assert!(is_verified_table_header("[verified]  # human only"));
        assert!(!is_verified_table_header("[verified.sub]"));
        assert!(!is_verified_table_header("[sources]"));
    }

    // ── staleness ──────────────────────────────────────────────────────────────

    fn prov(against: &[&str]) -> VerifiedProvenance {
        VerifiedProvenance {
            by: "zach".to_string(),
            at: "2026-06-20".to_string(),
            against: against.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn versions(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn drift_demotes_verified_to_needs_recheck() {
        let p = prov(&["clippy 1.83"]);
        let current = versions(&[("clippy", "1.84")]);
        let outcome = demote_if_stale(Verification::Verified, Some(&p), &current);
        match outcome {
            StalenessOutcome::Demoted {
                new_status,
                drifted,
            } => {
                assert_eq!(new_status, Verification::NeedsRecheck);
                assert_eq!(drifted.len(), 1);
                assert_eq!(drifted[0].name, "clippy");
                assert_eq!(drifted[0].verified_against, "1.83");
                assert_eq!(drifted[0].current, "1.84");
            }
            other => panic!("expected demotion, got {other:?}"),
        }
    }

    #[test]
    fn matching_versions_stay_verified() {
        let p = prov(&["clippy 1.83", "Google Java Style Guide 2024-01"]);
        let current = versions(&[("clippy", "1.83"), ("Google Java Style Guide", "2024-01")]);
        let outcome = demote_if_stale(Verification::Verified, Some(&p), &current);
        assert_eq!(
            outcome,
            StalenessOutcome::Unchanged(Verification::Verified),
            "no drift → stays Verified"
        );
    }

    #[test]
    fn unknown_current_version_does_not_demote() {
        // A cited source whose name is not in current_versions cannot be proven
        // drifted, so it must not trigger a demotion.
        let p = prov(&["obscure-linter 1.0"]);
        let current = versions(&[("clippy", "1.84")]);
        let outcome = demote_if_stale(Verification::Verified, Some(&p), &current);
        assert_eq!(outcome, StalenessOutcome::Unchanged(Verification::Verified));
    }

    #[test]
    fn non_verified_status_is_never_demoted() {
        let p = prov(&["clippy 1.83"]);
        let current = versions(&[("clippy", "1.84")]);
        for status in [
            Verification::Draft,
            Verification::Grounded,
            Verification::NeedsRecheck,
        ] {
            assert_eq!(
                demote_if_stale(status, Some(&p), &current),
                StalenessOutcome::Unchanged(status),
                "{status} must not be demoted by staleness"
            );
        }
    }

    #[test]
    fn verified_without_provenance_stays_verified() {
        let current = versions(&[("clippy", "1.84")]);
        assert_eq!(
            demote_if_stale(Verification::Verified, None, &current),
            StalenessOutcome::Unchanged(Verification::Verified),
        );
    }

    #[test]
    fn parse_against_entry_handles_multiword_names() {
        assert_eq!(
            parse_against_entry("Google Java Style Guide 2024-01"),
            Some(("Google Java Style Guide", "2024-01"))
        );
        assert_eq!(parse_against_entry("clippy 1.83"), Some(("clippy", "1.83")));
        assert_eq!(parse_against_entry("noversion"), None);
        assert_eq!(parse_against_entry(""), None);
    }
}
