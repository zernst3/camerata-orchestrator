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

/// Decide whether choosing an OPTION on a rule that is not yet present in any of the
/// project's three ruleset buckets should auto-select (adopt) that rule, and if so
/// into which bucket with which repos.
///
/// The model: `cross_repo` and `process` are PROJECT-LEVEL buckets — they apply
/// project-wide and are chosen unambiguously by engaging with the rule, so picking an
/// option on such a rule adopts it. Repo-local `Selections` are chosen per-repo via a
/// separate add flow (which repo would an option-pick target?), so they are NEVER
/// auto-added here.
///
/// Project-level selections MUST carry a non-empty `repos` list — a downstream
/// garbage-collection step drops selections whose repos become empty. So the returned
/// repos are ALL the project's real repos. If the project has no repos, there is
/// nothing sensible to scope to, so we return `None` (skip the add) rather than
/// persist a selection that would be immediately dropped.
///
/// Returns `Some((bucket, repos))` when the rule should be inserted, `None` otherwise.
/// The returned repos are cloned from `project_repos` (real `owner/repo` strings) — the
/// caller must never substitute a sentinel key here.
pub fn project_level_insert(
    bucket: SelectionBucket,
    project_repos: &[String],
) -> Option<(SelectionBucket, Vec<String>)> {
    match bucket {
        // Repo-local rules are chosen per-repo elsewhere; never auto-add from an option pick.
        SelectionBucket::Selections => None,
        SelectionBucket::CrossRepo | SelectionBucket::Process => {
            if project_repos.is_empty() {
                // Nothing sensible to scope a project-level selection to; a repos-empty
                // selection would be garbage-collected, so skip the add entirely.
                None
            } else {
                Some((bucket, project_repos.to_vec()))
            }
        }
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

/// Whether a rule's alternative is UNRESOLVED: it has options (alternatives to choose
/// between) but neither the caller-supplied `chosen_option_id` (the architect's saved pick,
/// already looked up scoped to whichever repo is being viewed) nor the rule's own
/// `default_option` resolves to an option carrying a non-empty directive. A rule with no
/// options is never unresolved — there is nothing to pick.
///
/// This is the ONE predicate backing the proposed-rules table's yellow "needs an
/// alternative chosen" row highlight, the audit/arm gate ("Choose an alternative first
/// for: ..."), and the "Needs option" table filter (`rules_needing_option_chosen` below) —
/// all three read it so they can never disagree about which rules still need a choice.
pub fn rule_option_unresolved(rule: &ProposedRuleView, chosen_option_id: Option<&str>) -> bool {
    if rule.options.is_empty() {
        return false;
    }
    let oid = chosen_option_id.or(rule.default_option.as_deref());
    let directive = oid
        .and_then(|id| rule.options.iter().find(|o| o.id == id))
        .map(|o| o.directive.as_str())
        .unwrap_or("");
    directive.is_empty()
}

/// The ids of the rules that are BOTH selected and still [`rule_option_unresolved`] — the
/// exact set the proposed-rules table's yellow row highlight, the "before you can audit or
/// add them" gate warning, and the "Needs option" table filter all show. `resolve_chosen`
/// looks up a rule's saved chosen-option id (already scoped by the caller to whichever repo
/// is being viewed); return `None` when there is no saved pick (falls back to the rule's own
/// default inside [`rule_option_unresolved`]).
///
/// Pure and framework-free so this is unit-tested directly against fixtures, with no
/// VirtualDom — `ProposedRulesTable` itself is intentionally excluded from SSR render tests
/// (it depends on six+ contexts and async-loaded data).
pub fn rules_needing_option_chosen<'a>(
    rules: impl IntoIterator<Item = &'a ProposedRuleView>,
    selected_ids: &std::collections::HashSet<String>,
    mut resolve_chosen: impl FnMut(&str) -> Option<String>,
) -> std::collections::HashSet<String> {
    rules
        .into_iter()
        .filter(|r| selected_ids.contains(&r.id))
        .filter(|r| rule_option_unresolved(r, resolve_chosen(&r.id).as_deref()))
        .map(|r| r.id.clone())
        .collect()
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

    // ── project_level_insert ──────────────────────────────────────────────────
    // Decides whether picking an OPTION on an unselected rule adopts it. Only
    // project-level buckets (process / cross-repo) auto-add, and only with a
    // non-empty repos list (so the selection survives repos-empty GC).

    #[test]
    fn project_level_insert_process_adds_with_project_repos() {
        let repos = vec!["me/api".to_string(), "me/web".to_string()];
        let got = project_level_insert(SelectionBucket::Process, &repos);
        assert_eq!(got, Some((SelectionBucket::Process, repos.clone())));
    }

    #[test]
    fn project_level_insert_cross_repo_adds_with_project_repos() {
        let repos = vec!["me/api".to_string()];
        let got = project_level_insert(SelectionBucket::CrossRepo, &repos);
        assert_eq!(got, Some((SelectionBucket::CrossRepo, repos.clone())));
    }

    #[test]
    fn project_level_insert_repo_local_never_auto_adds() {
        // Repo-local selections are chosen per-repo via a separate flow; an option
        // pick must not auto-add them (which repo would it target?).
        let repos = vec!["me/api".to_string(), "me/web".to_string()];
        assert_eq!(project_level_insert(SelectionBucket::Selections, &repos), None);
    }

    #[test]
    fn project_level_insert_empty_repos_skips_project_level() {
        // With no project repos there is nothing sensible to scope to, and a
        // repos-empty selection would be garbage-collected downstream — so skip.
        assert_eq!(project_level_insert(SelectionBucket::Process, &[]), None);
        assert_eq!(project_level_insert(SelectionBucket::CrossRepo, &[]), None);
    }

    // ── rule_option_unresolved / rules_needing_option_chosen ───────────────────
    // These back the yellow row highlight, the audit/arm gate warning, and the
    // "Needs option" table filter — all three must read the SAME predicate so
    // they can never disagree about which rules still need a choice.

    fn rule_with_options(
        default_option: Option<&str>,
        options: &[(&str, &str)],
    ) -> ProposedRuleView {
        let opts: Vec<serde_json::Value> = options
            .iter()
            .map(|(id, directive)| {
                serde_json::json!({"id": id, "label": id, "directive": directive})
            })
            .collect();
        serde_json::from_value(serde_json::json!({
            "id": "RULE-OPT-1",
            "title": "T",
            "kind": "structured",
            "scope": "repo-local",
            "options": opts,
            "default_option": default_option,
        }))
        .expect("valid ProposedRuleView fixture")
    }

    #[test]
    fn rule_option_unresolved_no_options_is_always_resolved() {
        // A rule with no options has nothing to pick — never unresolved, regardless
        // of what (if anything) is passed as the chosen option id.
        let r = rule_with_options(None, &[]);
        assert!(!rule_option_unresolved(&r, None));
        assert!(!rule_option_unresolved(&r, Some("whatever")));
    }

    #[test]
    fn rule_option_unresolved_options_no_pick_no_default_is_unresolved() {
        // Options exist, but neither a caller pick nor a corpus default resolves —
        // the architect must choose.
        let r = rule_with_options(None, &[("a", "Do the thing")]);
        assert!(rule_option_unresolved(&r, None));
    }

    #[test]
    fn rule_option_unresolved_chosen_pick_with_directive_is_resolved() {
        // A caller-supplied pick that resolves to a non-empty directive resolves the
        // rule even though there is no default_option at all.
        let r = rule_with_options(None, &[("a", "Do the thing"), ("b", "Do the other thing")]);
        assert!(!rule_option_unresolved(&r, Some("b")));
    }

    #[test]
    fn rule_option_unresolved_default_option_with_directive_is_resolved() {
        // No caller pick — falls back to the rule's own default_option, which
        // resolves to a non-empty directive.
        let r = rule_with_options(Some("a"), &[("a", "Do the thing")]);
        assert!(!rule_option_unresolved(&r, None));
    }

    #[test]
    fn rule_option_unresolved_pick_resolving_to_empty_directive_is_unresolved() {
        // The pick resolves to a REAL option, but that option's directive is empty
        // (an alternative that hasn't been fleshed out yet) — still unresolved.
        let r = rule_with_options(None, &[("a", "")]);
        assert!(rule_option_unresolved(&r, Some("a")));
    }

    #[test]
    fn rule_option_unresolved_pick_overrides_default() {
        // The caller's pick takes priority over default_option even when the
        // default would itself have resolved — the architect's explicit choice wins.
        let r = rule_with_options(Some("a"), &[("a", "Default directive"), ("b", "")]);
        // Picking "b" (empty directive) must NOT fall back to the resolved default "a".
        assert!(rule_option_unresolved(&r, Some("b")));
    }

    #[test]
    fn rules_needing_option_chosen_returns_exactly_selected_and_unresolved() {
        let mut r1 = rule_with_options(None, &[("a", "")]); // unresolved
        r1.id = "R1".to_string();
        let mut r2 = rule_with_options(Some("a"), &[("a", "Resolved")]); // resolved
        r2.id = "R2".to_string();
        let mut r3 = rule_with_options(None, &[("a", "")]); // unresolved, but NOT selected
        r3.id = "R3".to_string();
        let mut r4 = rule_with_options(Some("a"), &[("a", "Resolved")]); // resolved, NOT selected
        r4.id = "R4".to_string();
        let rules = vec![r1, r2, r3, r4];
        let selected: std::collections::HashSet<String> =
            ["R1", "R2"].iter().map(|s| s.to_string()).collect();

        let needing = rules_needing_option_chosen(&rules, &selected, |_id| None);

        let expected: std::collections::HashSet<String> = ["R1".to_string()].into_iter().collect();
        assert_eq!(needing, expected, "must be exactly selected-AND-unresolved: not unselected-but-unresolved (R3), not selected-but-resolved (R2)");
    }

    #[test]
    fn rules_needing_option_chosen_resolve_chosen_takes_priority_over_default() {
        // R1's default would resolve it, but the caller's saved pick (via
        // resolve_chosen) points at an option with an empty directive — still unresolved.
        let mut r1 = rule_with_options(Some("a"), &[("a", "Resolved default"), ("b", "")]);
        r1.id = "R1".to_string();
        let rules = vec![r1];
        let selected: std::collections::HashSet<String> = ["R1".to_string()].into_iter().collect();

        let needing = rules_needing_option_chosen(&rules, &selected, |id| {
            if id == "R1" { Some("b".to_string()) } else { None }
        });

        assert_eq!(needing, ["R1".to_string()].into_iter().collect());
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
