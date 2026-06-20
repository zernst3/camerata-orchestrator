//! Suppressions: the baseline/ratchet + the auditable waiver mechanism.
//!
//! The make-or-break brownfield decision: a legacy repo has hundreds of pre-existing
//! violations. If onboarding froze all new work until that debt was fixed, no team
//! would adopt it. So Camerata REPORTS everything but ENFORCES on the delta — new or
//! changed code — exactly like eslint/ruff/sonar baselines. A violation that is
//! suppressed (baselined or waived) does not block; a NEW one does.
//!
//! Two homes for two kinds of exception (per Zach's design):
//! - **Inline waiver** — a per-line, surgical exception, co-located with the code:
//!   `// camerata:allow RULE-ID -- reason [, TICKET]`. Shows in the PR diff (a
//!   reviewable, challengeable act), `git blame` gives who/when for free, travels
//!   through refactors. The linter model.
//! - **Central baseline** — bulk / legacy / policy exceptions in `.camerata/baseline.json`
//!   with metadata. The 400-violations-at-onboarding snapshot lives here, NOT as 400
//!   scattered comments.
//!
//! Three governance invariants regardless of home:
//! 1. **Reason required.** A reason-less waiver is itself a violation
//!    (`CAM-WAIVER-NEEDS-REASON`) — the un-auditable hole this exists to prevent.
//! 2. **Indexed centrally.** Inline waivers roll up into one queryable registry, so
//!    "show me everything we've waived" is a lookup, not a grep.
//! 3. **Stale ones surfaced.** A waiver whose violation no longer exists is a dead
//!    directive silently masking future violations — flag it for removal.
//!
//! Tie-back: a waiver can carry its tech-debt ticket (`-- accepted as debt, JIRA-123`),
//! so "ignore" and "create a story" become one auditable act.

use serde::{Deserialize, Serialize};

/// The inline waiver marker scanned out of source.
const MARKER: &str = "camerata:allow";

/// A per-line inline waiver parsed from a source comment.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InlineWaiver {
    pub rule_id: String,
    /// The mandatory justification. `None` = a reason-less waiver (itself a violation).
    pub reason: Option<String>,
    /// Optional tracked ticket the waiver links to (the debt-story tie-back).
    pub ticket: Option<String>,
    pub path: String,
    /// 1-based line the marker is on.
    pub line: usize,
}

/// One entry in the central baseline (`.camerata/baseline.json`): a bulk/legacy/policy
/// suppression with full provenance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BaselineEntry {
    pub rule_id: String,
    pub path: String,
    /// Content fingerprint (survives line drift; changes when the offending code itself
    /// changes — touching debt un-baselines it, which is the ratchet working).
    pub fingerprint: String,
    /// Mandatory reason (e.g. "pre-existing at onboarding").
    pub reason: String,
    pub accepted_by: String,
    /// ISO-8601 timestamp (stamped by the caller — this module stays time-free/pure).
    pub accepted_at: String,
    /// `baseline` (onboarding snapshot) | `policy` (org-wide waive-until).
    #[serde(default)]
    pub kind: String,
    /// Optional linked ticket.
    #[serde(default)]
    pub ticket: Option<String>,
}

/// The committed baseline document.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Baseline {
    #[serde(default)]
    pub entries: Vec<BaselineEntry>,
}

/// A minimal view of a finding this module needs to classify it (decoupled from
/// `onboard::Finding` so the logic stays pure and testable).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingRef {
    pub rule_id: String,
    pub path: String,
    pub line: usize,
    pub snippet: String,
}

/// How a finding is classified against the suppressions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// New / unwaived — the gate ENFORCES on these.
    Active,
    /// Waived by an inline `camerata:allow` comment.
    SuppressedInline,
    /// In the central baseline (accepted pre-existing debt / policy).
    SuppressedBaseline,
}

/// A unified suppression record for the central audit registry (inline + baseline rolled
/// up into one queryable view).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SuppressionRecord {
    pub rule_id: String,
    pub path: String,
    #[serde(default)]
    pub line: Option<usize>,
    pub reason: Option<String>,
    #[serde(default)]
    pub ticket: Option<String>,
    /// `inline` | `baseline`.
    pub source: String,
    #[serde(default)]
    pub accepted_by: Option<String>,
    #[serde(default)]
    pub accepted_at: Option<String>,
    /// True when this suppression no longer matches a live violation (dead directive).
    pub stale: bool,
}

// ── fingerprinting ──────────────────────────────────────────────────────────

/// A stable, dependency-free FNV-1a 64-bit hash. Stable across machines and Rust
/// versions (unlike `DefaultHasher`), which a COMMITTED baseline file requires.
/// `pub(crate)` so the scan cache can fingerprint whole-file content with the same
/// stable hash the suppression baseline uses.
pub(crate) fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Fingerprint a violation by rule + normalized offending snippet. Whitespace is
/// collapsed so reformatting/indent shifts don't break the match, but a real edit to
/// the offending code does (so touching debt un-baselines it — the ratchet).
pub fn fingerprint(rule_id: &str, snippet: &str) -> String {
    let norm = snippet.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{:016x}", fnv1a(&format!("{rule_id}|{norm}")))
}

/// Build a baseline entry for a finding (accepted pre-existing debt). The caller
/// supplies who/when/reason so this stays time-free and pure.
pub fn baseline_entry(
    f: &FindingRef,
    accepted_by: &str,
    accepted_at: &str,
    reason: &str,
) -> BaselineEntry {
    BaselineEntry {
        rule_id: f.rule_id.clone(),
        path: f.path.clone(),
        fingerprint: fingerprint(&f.rule_id, &f.snippet),
        reason: reason.to_string(),
        accepted_by: accepted_by.to_string(),
        accepted_at: accepted_at.to_string(),
        kind: "baseline".to_string(),
        ticket: None,
    }
}

// ── inline waiver parsing ───────────────────────────────────────────────────

/// True if `t` looks like a ticket id, e.g. `JIRA-123`, `AB#42`, `GH-7`.
fn is_ticket(t: &str) -> bool {
    let Some(i) = t.find(['-', '#']) else {
        return false;
    };
    i > 0
        && i < t.len() - 1
        && t[..i].chars().all(|c| c.is_ascii_uppercase())
        && t[i + 1..].chars().all(|c| c.is_ascii_digit())
}

fn extract_ticket(reason: &str) -> Option<String> {
    reason
        .split(|c: char| c.is_whitespace() || c == ',')
        .map(|t| t.trim())
        .find(|t| is_ticket(t))
        .map(|t| t.to_string())
}

/// Parse inline `camerata:allow RULE-ID -- reason [, TICKET]` waivers from one file.
pub fn parse_inline_waivers(path: &str, content: &str) -> Vec<InlineWaiver> {
    let mut out = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let Some(idx) = line.find(MARKER) else {
            continue;
        };
        let rest = line[idx + MARKER.len()..].trim();
        let mut parts = rest.splitn(2, char::is_whitespace);
        let rule_id = parts.next().unwrap_or("").trim().to_string();
        if rule_id.is_empty() {
            continue;
        }
        let after = parts.next().unwrap_or("").trim();
        let reason = after
            .strip_prefix("--")
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty());
        let ticket = reason.as_deref().and_then(extract_ticket);
        out.push(InlineWaiver {
            rule_id,
            reason,
            ticket,
            path: path.to_string(),
            line: i + 1,
        });
    }
    out
}

// ── classification ──────────────────────────────────────────────────────────

/// An inline waiver applies to a finding when they share rule + file and the marker is
/// on the offending line (trailing) or the line directly above it (the linter convention).
fn inline_applies(w: &InlineWaiver, f: &FindingRef) -> bool {
    w.rule_id == f.rule_id && w.path == f.path && (f.line == w.line || f.line == w.line + 1)
}

/// Classify one finding against the inline waivers + baseline.
pub fn classify_one(f: &FindingRef, inline: &[InlineWaiver], baseline: &Baseline) -> Status {
    // A reason-less inline waiver does NOT suppress (it's invalid; the finding stays
    // active and the waiver itself becomes a violation elsewhere).
    if inline
        .iter()
        .any(|w| w.reason.is_some() && inline_applies(w, f))
    {
        return Status::SuppressedInline;
    }
    let fp = fingerprint(&f.rule_id, &f.snippet);
    if baseline
        .entries
        .iter()
        .any(|e| e.rule_id == f.rule_id && e.fingerprint == fp)
    {
        return Status::SuppressedBaseline;
    }
    Status::Active
}

/// The require-reason violations: every inline waiver with no reason. These are findings
/// the gate enforces — a bare `camerata:allow` is the un-auditable hole.
pub fn reasonless_waivers(inline: &[InlineWaiver]) -> Vec<&InlineWaiver> {
    inline.iter().filter(|w| w.reason.is_none()).collect()
}

/// The rule id surfaced for a reason-less waiver.
pub const REASONLESS_RULE_ID: &str = "CAM-WAIVER-NEEDS-REASON";

// ── stale detection + registry ──────────────────────────────────────────────

/// Inline waivers that match NO current finding — dead directives masking future
/// violations. (Only reasoned waivers can be "covering" something, so we check those.)
pub fn stale_inline<'a>(
    inline: &'a [InlineWaiver],
    findings: &[FindingRef],
) -> Vec<&'a InlineWaiver> {
    inline
        .iter()
        .filter(|w| w.reason.is_some())
        .filter(|w| !findings.iter().any(|f| inline_applies(w, f)))
        .collect()
}

/// Baseline entries whose fingerprint matches no current finding — resolved/moved debt
/// whose suppression should be removed.
pub fn stale_baseline<'a>(
    baseline: &'a Baseline,
    findings: &[FindingRef],
) -> Vec<&'a BaselineEntry> {
    baseline
        .entries
        .iter()
        .filter(|e| {
            !findings.iter().any(|f| {
                f.rule_id == e.rule_id && fingerprint(&f.rule_id, &f.snippet) == e.fingerprint
            })
        })
        .collect()
}

/// Roll inline waivers + baseline entries into one auditable registry, each flagged
/// stale or not against the current findings.
pub fn registry(
    inline: &[InlineWaiver],
    baseline: &Baseline,
    findings: &[FindingRef],
) -> Vec<SuppressionRecord> {
    let stale_in: Vec<usize> = stale_inline(inline, findings)
        .iter()
        .map(|w| w.line)
        .collect();
    let mut out: Vec<SuppressionRecord> = inline
        .iter()
        .filter(|w| w.reason.is_some())
        .map(|w| SuppressionRecord {
            rule_id: w.rule_id.clone(),
            path: w.path.clone(),
            line: Some(w.line),
            reason: w.reason.clone(),
            ticket: w.ticket.clone(),
            source: "inline".to_string(),
            accepted_by: None,
            accepted_at: None,
            stale: stale_in.contains(&w.line),
        })
        .collect();
    let stale_bl: Vec<String> = stale_baseline(baseline, findings)
        .iter()
        .map(|e| e.fingerprint.clone())
        .collect();
    out.extend(baseline.entries.iter().map(|e| SuppressionRecord {
        rule_id: e.rule_id.clone(),
        path: e.path.clone(),
        line: None,
        reason: Some(e.reason.clone()),
        ticket: e.ticket.clone(),
        source: "baseline".to_string(),
        accepted_by: Some(e.accepted_by.clone()),
        accepted_at: Some(e.accepted_at.clone()),
        stale: stale_bl.contains(&e.fingerprint),
    }));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(rule: &str, path: &str, line: usize, snip: &str) -> FindingRef {
        FindingRef {
            rule_id: rule.to_string(),
            path: path.to_string(),
            line,
            snippet: snip.to_string(),
        }
    }

    #[test]
    fn fingerprint_is_stable_and_whitespace_insensitive() {
        let a = fingerprint("SEC-X", "let t =  \"abc\";");
        let b = fingerprint("SEC-X", "let t = \"abc\";");
        assert_eq!(a, b, "whitespace collapses");
        // A real edit to the offending code changes it (un-baselines -> ratchet).
        assert_ne!(a, fingerprint("SEC-X", "let t = \"xyz\";"));
        // Stable value (regression guard on the algorithm).
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn parses_inline_waiver_with_reason_and_ticket() {
        let src = "let k = sandbox_key; // camerata:allow SEC-NO-HARDCODED-SECRETS-1 -- public sandbox value, JIRA-123\n";
        let w = parse_inline_waivers("a.rs", src);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].rule_id, "SEC-NO-HARDCODED-SECRETS-1");
        assert_eq!(
            w[0].reason.as_deref(),
            Some("public sandbox value, JIRA-123")
        );
        assert_eq!(w[0].ticket.as_deref(), Some("JIRA-123"));
        assert_eq!(w[0].line, 1);
    }

    #[test]
    fn reasonless_waiver_is_flagged_and_does_not_suppress() {
        let src = "bad(); // camerata:allow SEC-X\n";
        let w = parse_inline_waivers("a.rs", src);
        assert_eq!(w.len(), 1);
        assert!(w[0].reason.is_none());
        assert_eq!(reasonless_waivers(&w).len(), 1);
        // It does NOT suppress a matching finding (invalid waiver).
        let finding = f("SEC-X", "a.rs", 1, "bad()");
        assert_eq!(
            classify_one(&finding, &w, &Baseline::default()),
            Status::Active
        );
    }

    #[test]
    fn inline_suppresses_same_line_or_line_above() {
        let waivers = vec![InlineWaiver {
            rule_id: "R".into(),
            reason: Some("ok".into()),
            ticket: None,
            path: "a.rs".into(),
            line: 5,
        }];
        // trailing (same line)
        assert_eq!(
            classify_one(&f("R", "a.rs", 5, "x"), &waivers, &Baseline::default()),
            Status::SuppressedInline
        );
        // comment directly above (finding on line 6)
        assert_eq!(
            classify_one(&f("R", "a.rs", 6, "x"), &waivers, &Baseline::default()),
            Status::SuppressedInline
        );
        // unrelated line is NOT suppressed
        assert_eq!(
            classify_one(&f("R", "a.rs", 9, "x"), &waivers, &Baseline::default()),
            Status::Active
        );
        // different rule is NOT suppressed
        assert_eq!(
            classify_one(&f("OTHER", "a.rs", 5, "x"), &waivers, &Baseline::default()),
            Status::Active
        );
    }

    #[test]
    fn baseline_suppresses_by_fingerprint_and_ratchets_on_edit() {
        let snippet = "let token = \"ghp_xxx\";";
        let baseline = Baseline {
            entries: vec![BaselineEntry {
                rule_id: "SEC-X".into(),
                path: "a.rs".into(),
                fingerprint: fingerprint("SEC-X", snippet),
                reason: "pre-existing at onboarding".into(),
                accepted_by: "zach".into(),
                accepted_at: "2026-06-16T00:00:00Z".into(),
                kind: "baseline".into(),
                ticket: None,
            }],
        };
        // Same offending content (even on a drifted line) is suppressed.
        assert_eq!(
            classify_one(&f("SEC-X", "a.rs", 99, snippet), &[], &baseline),
            Status::SuppressedBaseline
        );
        // Edited offending content is NEW -> active (the ratchet tightens).
        assert_eq!(
            classify_one(
                &f("SEC-X", "a.rs", 99, "let token = \"new\";"),
                &[],
                &baseline
            ),
            Status::Active
        );
    }

    #[test]
    fn stale_suppressions_are_detected() {
        // An inline waiver covering nothing (the violation was fixed) is stale.
        let waivers = vec![InlineWaiver {
            rule_id: "R".into(),
            reason: Some("x".into()),
            ticket: None,
            path: "a.rs".into(),
            line: 5,
        }];
        let findings: Vec<FindingRef> = vec![]; // no live violation
        assert_eq!(stale_inline(&waivers, &findings).len(), 1);
        // A baseline entry with no matching finding is stale.
        let baseline = Baseline {
            entries: vec![BaselineEntry {
                rule_id: "R".into(),
                path: "a.rs".into(),
                fingerprint: fingerprint("R", "gone"),
                reason: "x".into(),
                accepted_by: "z".into(),
                accepted_at: "t".into(),
                kind: "baseline".into(),
                ticket: None,
            }],
        };
        assert_eq!(stale_baseline(&baseline, &findings).len(), 1);
    }

    #[test]
    fn registry_rolls_up_inline_and_baseline_with_stale_flags() {
        let waivers = vec![InlineWaiver {
            rule_id: "R".into(),
            reason: Some("x".into()),
            ticket: Some("AB-9".into()),
            path: "a.rs".into(),
            line: 5,
        }];
        let baseline = Baseline {
            entries: vec![BaselineEntry {
                rule_id: "S".into(),
                path: "b.rs".into(),
                fingerprint: fingerprint("S", "live"),
                reason: "debt".into(),
                accepted_by: "z".into(),
                accepted_at: "t".into(),
                kind: "baseline".into(),
                ticket: None,
            }],
        };
        // The baseline entry matches a live finding; the inline one does not.
        let findings = vec![f("S", "b.rs", 3, "live")];
        let reg = registry(&waivers, &baseline, &findings);
        assert_eq!(reg.len(), 2);
        let inline_rec = reg.iter().find(|r| r.source == "inline").unwrap();
        assert!(inline_rec.stale, "inline waiver covers no live finding");
        assert_eq!(inline_rec.ticket.as_deref(), Some("AB-9"));
        let bl_rec = reg.iter().find(|r| r.source == "baseline").unwrap();
        assert!(!bl_rec.stale, "baseline entry still covers a live finding");
        assert_eq!(bl_rec.accepted_by.as_deref(), Some("z"));
    }

    // ── baseline_entry builder ──────────────────────────────────────────────────

    #[test]
    fn baseline_entry_builder_fills_all_fields() {
        let finding = f("SEC-X", "src/main.rs", 42, "let k = \"secret\";");
        let entry = baseline_entry(&finding, "alice", "2026-06-19T00:00:00Z", "pre-existing");
        assert_eq!(entry.rule_id, "SEC-X");
        assert_eq!(entry.path, "src/main.rs");
        assert_eq!(entry.accepted_by, "alice");
        assert_eq!(entry.accepted_at, "2026-06-19T00:00:00Z");
        assert_eq!(entry.reason, "pre-existing");
        assert_eq!(entry.kind, "baseline");
        assert!(entry.ticket.is_none());
        // The fingerprint must match what fingerprint() would compute independently.
        assert_eq!(
            entry.fingerprint,
            fingerprint("SEC-X", "let k = \"secret\";")
        );
    }

    #[test]
    fn baseline_entry_fingerprint_is_whitespace_insensitive() {
        let f1 = f("R", "a.rs", 1, "let x  =  1;");
        let f2 = f("R", "a.rs", 1, "let x = 1;");
        let e1 = baseline_entry(&f1, "z", "t", "reason");
        let e2 = baseline_entry(&f2, "z", "t", "reason");
        assert_eq!(
            e1.fingerprint, e2.fingerprint,
            "whitespace collapse means both produce same fingerprint"
        );
    }

    // ── fingerprint changes when rule-id changes ─────────────────────────────

    #[test]
    fn fingerprint_differs_across_rule_ids() {
        let fp1 = fingerprint("SEC-A", "let k = 1;");
        let fp2 = fingerprint("SEC-B", "let k = 1;");
        assert_ne!(
            fp1, fp2,
            "same snippet under different rules must produce different fingerprints"
        );
    }

    // ── parse_inline_waivers edge cases ─────────────────────────────────────

    #[test]
    fn parses_multiple_waivers_in_same_file() {
        let src = "\
line1; // camerata:allow R1 -- reason one\n\
line2;\n\
line3; // camerata:allow R2 -- reason two, GH-7\n";
        let w = parse_inline_waivers("x.rs", src);
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].rule_id, "R1");
        assert_eq!(w[0].line, 1);
        assert_eq!(w[1].rule_id, "R2");
        assert_eq!(w[1].line, 3);
        // GH-7 matches the ticket pattern (2 uppercase letters + hyphen + digits).
        assert_eq!(w[1].ticket.as_deref(), Some("GH-7"));
    }

    #[test]
    fn parse_inline_waivers_ignores_lines_without_marker() {
        let src = "// just a normal comment\ncode();\n// TODO: fix this\n";
        let w = parse_inline_waivers("x.rs", src);
        assert!(w.is_empty());
    }

    #[test]
    fn parse_inline_waivers_marker_embedded_in_code_comment() {
        // The marker can appear anywhere on the line (typical: end of code line).
        let src = "foo.bar(); /* camerata:allow SEC-Y -- embedded ok */\n";
        let w = parse_inline_waivers("y.rs", src);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].rule_id, "SEC-Y");
        assert!(w[0].reason.is_some());
    }

    #[test]
    fn parse_inline_waivers_bare_marker_with_no_rule_id_is_skipped() {
        // A bare `camerata:allow` with nothing after it should produce no entry.
        let src = "code(); // camerata:allow\n";
        let w = parse_inline_waivers("a.rs", src);
        assert!(w.is_empty(), "a marker with no rule id must be skipped");
    }

    // ── ticket extraction with both separators ───────────────────────────────

    #[test]
    fn ticket_with_hyphen_separator_is_extracted() {
        // AB-123 uses a hyphen (different from the JIRA-style hash).
        let src = "code(); // camerata:allow R -- reason, AB-123\n";
        let w = parse_inline_waivers("a.rs", src);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].ticket.as_deref(), Some("AB-123"));
    }

    #[test]
    fn ticket_with_hash_separator_is_extracted() {
        // GH#42 uses a hash (GitHub issue link style).
        let src = "code(); // camerata:allow R -- see GH#42\n";
        let w = parse_inline_waivers("a.rs", src);
        // GH#42: prefix="GH" (uppercase), separator='#', digits="42" -> valid ticket.
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].ticket.as_deref(), Some("GH#42"));
    }

    // ── classify_one: different path means no suppression ───────────────────

    #[test]
    fn inline_does_not_suppress_finding_in_different_file() {
        let waivers = vec![InlineWaiver {
            rule_id: "R".into(),
            reason: Some("ok".into()),
            ticket: None,
            path: "a.rs".into(),
            line: 1,
        }];
        // Same rule, same line, but DIFFERENT path — must not suppress.
        let finding = f("R", "b.rs", 1, "x");
        assert_eq!(
            classify_one(&finding, &waivers, &Baseline::default()),
            Status::Active
        );
    }

    // ── reasonless_waivers: only returns waivers without a reason ────────────

    #[test]
    fn reasonless_waivers_does_not_include_reasoned_waivers() {
        let waivers = vec![
            InlineWaiver {
                rule_id: "A".into(),
                reason: Some("because".into()),
                ticket: None,
                path: "a.rs".into(),
                line: 1,
            },
            InlineWaiver {
                rule_id: "B".into(),
                reason: None, // reasonless
                ticket: None,
                path: "a.rs".into(),
                line: 2,
            },
        ];
        let bad = reasonless_waivers(&waivers);
        assert_eq!(bad.len(), 1);
        assert_eq!(bad[0].rule_id, "B");
    }
}
