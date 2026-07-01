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
}
