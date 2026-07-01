//! Rule-table display helpers, extracted from the rules UI. Pure functions with no rendering-framework
//! dependency, unit-tested here.

/// The verification badge `(label, css-modifier)` for a rule's `verification` value. An unrecognised
/// value falls back to the neutral "Draft" visual (never panics).
pub fn verif_badge(verif: &str) -> (&'static str, &'static str) {
    match verif {
        "verified" => ("\u{2713} Verified", "verified"),
        // Grounded carries its OWN distinct glyph (a circled source-dot) so it reads as a clear status
        // on the rule tables, visually distinct from the verified checkmark and the symbol-less draft /
        // needs-re-check badges.
        "grounded" => ("\u{29bf} Grounded", "grounded"),
        "needs_recheck" => ("Needs re-check", "needs-recheck"),
        _ => ("Draft", "draft"),
    }
}

/// Split a finding's detail into `(body, optional "needs review" reason)`. If the detail carries a
/// trailing `[needs review]` or `[needs review: <reason>]` marker, the reason is extracted and the
/// marker is stripped from the body; otherwise the detail passes through unchanged with `None`.
pub fn split_needs_review(detail: &str) -> (String, Option<String>) {
    if let Some(start) = detail.rfind("[needs review") {
        if let Some(end_rel) = detail[start..].find(']') {
            let inside = &detail[start + 1..start + end_rel];
            let reason = inside
                .strip_prefix("needs review")
                .unwrap_or("")
                .trim_start_matches([':', ' '])
                .trim()
                .to_string();
            let body = detail[..start].trim_end().to_string();
            return (body, Some(reason));
        }
    }
    (detail.to_string(), None)
}

/// Which of the three lists a rule_id lives in (selections / cross_repo / process).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectionBucket {
    Selections,
    CrossRepo,
    Process,
}

pub fn bucket_of(rule: &ProposedRuleView) -> SelectionBucket {
    match rule.scope.as_str() {
        "cross-repo" => SelectionBucket::CrossRepo,
        "process" => SelectionBucket::Process,
        _ => SelectionBucket::Selections,
    }
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct RuleOptionView {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub directive: String,
    #[serde(default)]
    pub why: String,
}

/// One authoritative source backing a rule's grounding (mirrors `RuleSourceView`
/// from the server DTO). Used in `ProposedRuleView.sources`.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
pub struct RuleSourceView {
    pub url: String,
    pub title: String,
    #[serde(default)]
    pub linter: Option<String>,
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ProposedRuleView {
    pub id: String,
    pub title: String,
    pub kind: String,
    #[serde(default)]
    pub enforcement: String,
    #[serde(default)]
    pub options: Vec<RuleOptionView>,
    #[serde(default)]
    pub default_option: Option<String>,
    #[serde(default)]
    pub decision_question: Option<String>,
    #[serde(default)]
    pub decision_why: Option<String>,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub placement: String,
    #[serde(default)]
    pub finding_count: usize,
    #[serde(default)]
    pub recommended: bool,
    /// Server-side auto-recommend flag (pw/cockpit-ui product wave). The server
    /// emits `is_auto_recommended: true` for rules whose `verification` is
    /// `grounded` or `verified` (the two rungs that have been reviewed against a
    /// real source). `draft` and `needs_recheck` rules arrive with it `false`.
    /// Falls back to `recommended` when the field is absent so old server payloads
    /// continue to work.
    #[serde(default)]
    pub is_auto_recommended: bool,
    /// Provenance / verification status: `draft` | `grounded` | `verified` |
    /// `needs_recheck`. Defaults to `draft` for any rule that omits the field
    /// (pre-schema corpus rules, AI-discovered rules). See
    /// `docs/decisions/2026-06-20_rule_provenance_schema.md`.
    #[serde(default = "default_draft")]
    pub verification: String,
    /// Authoritative sources backing this rule's grounding (empty for `draft`).
    #[serde(default)]
    pub sources: Vec<RuleSourceView>,
}

pub fn default_draft() -> String {
    "draft".to_string()
}

impl ProposedRuleView {
    /// True when this rule should be pre-checked on first view of the proposed-rules
    /// table.
    ///
    /// The SERVER is authoritative for this value. It gates on three conditions:
    /// stack-relevance (the rule's domain matches the scanned repo) + provenance
    /// (`grounded` or `verified`) + `!opt_in_only`. `opt_in_only` rules (e.g.
    /// CICD-CODEQL-SECURITY-SCAN-1, CICD-SEMGREP-SECURITY-SCAN-1) are NEVER
    /// pre-checked even when they are grounded and stack-relevant — they appear in
    /// the list so the architect can deliberately opt in, but the server sends
    /// `is_auto_recommended: false` for them and the UI must honour that flag
    /// without re-deriving it from `recommended` or `verification`.
    ///
    /// `draft` and `needs_recheck` rules appear LISTED but unchecked so the
    /// architect must explicitly opt them in.
    pub fn effective_auto_recommended(&self) -> bool {
        // The server encodes the full gate (stack-relevance + grounded/verified +
        // !opt_in_only) into `is_auto_recommended`. Use it directly — do NOT
        // fall back to `recommended` or re-derive from `verification`. A fallback
        // that re-derives from `recommended && grounded/verified` would incorrectly
        // pre-check opt_in_only rules (which are grounded + recommended but must
        // never be pre-selected). The server is always co-versioned with the UI in
        // this codebase, so there is no version-skew risk.
        self.is_auto_recommended
    }
}

/// Quote a CSV field if it contains a comma, quote, or newline (RFC 4180).
pub fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Build CSV for the proposed-rules table.
pub fn rules_csv(rules: &[ProposedRuleView]) -> String {
    let mut out =
        String::from("rule_id,title,kind,scope,enforcement,placement,finding_count,repos\n");
    for r in rules {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{}\n",
            csv_field(&r.id),
            csv_field(&r.title),
            csv_field(&r.kind),
            csv_field(&r.scope),
            csv_field(&r.enforcement),
            csv_field(&r.placement),
            r.finding_count,
            csv_field(&r.repos.join(" ")),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // verif_badge() unit tests — pure function, no DOM. Moved verbatim from cockpit.rs; all four
    // canonical values + an unknown value (falls back to draft).

    #[test]
    fn verif_badge_verified_returns_checkmark_label_and_green_class() {
        let (label, cls) = verif_badge("verified");
        assert!(label.contains("Verified"), "label should mention Verified, got: {label}");
        assert_eq!(cls, "verified");
    }

    #[test]
    fn verif_badge_grounded_returns_grounded_label_and_blue_class() {
        let (label, cls) = verif_badge("grounded");
        assert!(label.contains("Grounded"), "label should mention Grounded, got: {label}");
        // Grounded must carry its own distinct symbol (the circled source-dot), separate from
        // the verified checkmark, so it's a clear table status not a faint tint.
        assert!(label.contains('\u{29bf}'), "grounded label should carry its source-dot symbol");
        assert!(!label.contains('\u{2713}'), "grounded must NOT reuse the verified checkmark");
        assert_eq!(cls, "grounded");
    }

    #[test]
    fn verif_badge_draft_returns_draft_label_and_gray_class() {
        let (label, cls) = verif_badge("draft");
        assert_eq!(label, "Draft");
        assert_eq!(cls, "draft");
    }

    #[test]
    fn verif_badge_needs_recheck_returns_distinct_label_and_class() {
        let (label, cls) = verif_badge("needs_recheck");
        assert!(label.contains("re-check") || label.contains("recheck"), "label should signal re-check, got: {label}");
        assert_eq!(cls, "needs-recheck");
    }

    #[test]
    fn verif_badge_unknown_value_falls_back_to_draft() {
        // An unrecognised value (e.g. a future extension the UI hasn't caught up to)
        // must not panic and must fall back to the `draft` visual.
        let (label, cls) = verif_badge("something_new");
        assert_eq!(label, "Draft");
        assert_eq!(cls, "draft");
    }

    #[test]
    fn split_needs_review_no_flag_returns_detail_and_none() {
        let (body, reason) = split_needs_review("Plain finding detail.");
        assert_eq!(body, "Plain finding detail.");
        assert_eq!(reason, None);
    }

    #[test]
    fn split_needs_review_bare_flag_returns_empty_reason() {
        let (body, reason) = split_needs_review("Some detail [needs review]");
        assert_eq!(body, "Some detail");
        assert_eq!(reason, Some(String::new()));
    }

    #[test]
    fn split_needs_review_flag_with_reason_extracts_reason() {
        let (body, reason) =
            split_needs_review("Some detail [needs review: premature for a mini app]");
        assert_eq!(body, "Some detail");
        assert_eq!(reason, Some("premature for a mini app".to_string()));
    }

    // ── bucket_of ─────────────────────────────────────────────────────────────

    fn rule_with_scope(scope: &str) -> ProposedRuleView {
        serde_json::from_value(serde_json::json!({
            "id": "RULE-1", "title": "T", "kind": "structured", "scope": scope
        }))
        .expect("valid ProposedRuleView fixture")
    }

    #[test]
    fn bucket_of_maps_scope_to_bucket() {
        assert_eq!(bucket_of(&rule_with_scope("cross-repo")), SelectionBucket::CrossRepo);
        assert_eq!(bucket_of(&rule_with_scope("process")), SelectionBucket::Process);
        assert_eq!(bucket_of(&rule_with_scope("repo-local")), SelectionBucket::Selections);
        // An unknown scope defaults to the repo-local Selections bucket.
        assert_eq!(bucket_of(&rule_with_scope("whatever")), SelectionBucket::Selections);
    }

    // ── rules_csv ─────────────────────────────────────────────────────────────

    #[test]
    fn rules_csv_emits_header_and_one_row_per_rule() {
        let r: ProposedRuleView = serde_json::from_value(serde_json::json!({
            "id": "RUST-FMT-1",
            "title": "Format with rustfmt",
            "kind": "mechanical",
            "scope": "repo-local",
            "enforcement": "mechanical",
            "placement": "CI",
            "finding_count": 3,
            "repos": ["me/api", "me/web"]
        }))
        .expect("valid ProposedRuleView fixture");
        let csv = rules_csv(std::slice::from_ref(&r));
        let mut lines = csv.lines();
        assert_eq!(
            lines.next().unwrap(),
            "rule_id,title,kind,scope,enforcement,placement,finding_count,repos"
        );
        let row = lines.next().unwrap();
        assert!(row.starts_with("RUST-FMT-1,Format with rustfmt,mechanical,repo-local,mechanical,CI,3,"));
        // repos are space-joined inside the single CSV field.
        assert!(row.contains("me/api me/web"), "row=\n{row}");
    }

    // ── default_draft sentinel ────────────────────────────────────────────────

    #[test]
    fn default_draft_is_draft_and_drives_serde_default() {
        assert_eq!(default_draft(), "draft");
        // A corpus rule JSON omitting `verification` deserializes with the draft default.
        let r: ProposedRuleView = serde_json::from_value(serde_json::json!({
            "id": "R-1", "title": "T", "kind": "review"
        }))
        .expect("valid ProposedRuleView");
        assert_eq!(r.verification, "draft");
    }

    // ── csv_field (RFC 4180 quoting) ──────────────────────────────────────────
    // Moved from scan.rs: csv_field is shared between rules_csv (moved) and findings_csv
    // (staying in scan.rs), so it lives here and is re-exported back to scan.rs.

    #[test]
    fn csv_field_passthrough_when_no_special_chars() {
        assert_eq!(csv_field("plain"), "plain");
    }

    #[test]
    fn csv_field_quotes_and_escapes_when_special() {
        // A comma forces quoting; an embedded quote is doubled.
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        // Newlines also force quoting.
        assert_eq!(csv_field("line1\nline2"), "\"line1\nline2\"");
    }
}
