//! Cause 2 of the 2026-11-15 false-alarm diagnosis: a style linter's label folded into a reviewed
//! row outranked the reviewed severity, with no note. Synthetic rule ids and rows only.

use super::*;
use crate::severity_bounds::recorded_adjustments;

const NATIVE: &str = "SYN-TS-NON-NULL-1";
const TOOL_RULE: &str = "@typescript-eslint/no-non-null-assertion";

fn selectors() -> std::collections::HashMap<String, Vec<String>> {
    let mut m = std::collections::HashMap::new();
    m.insert(NATIVE.to_string(), vec![TOOL_RULE.to_string()]);
    m
}

fn corpus(min_severity: Option<&str>) -> camerata_rules::RuleSet {
    let mut rule = camerata_rules::bare_rule(NATIVE, "test-fixture");
    rule.min_severity = min_severity.map(str::to_string);
    let mut set = camerata_rules::RuleSet::default();
    set.push(rule);
    set
}

/// The model's row at the place, rated `severity` by the auditor and by stage 1.
fn reviewed_model_row(severity: &str) -> crate::onboard::Finding {
    let mut f = crate::onboard::Finding {
        repo: "acme/web".to_string(),
        path: "src/profile.ts".to_string(),
        line: 21,
        rule_id: NATIVE.to_string(),
        severity: severity.to_string(),
        snippet: "const name = user!.name;".to_string(),
        kind: Some(camerata_api_types::audit::FindingKind::Defect),
        ..crate::onboard::Finding::default()
    };
    f.captures.insert(
        crate::ai_audit::STAGE1_CAPTURE.to_string(),
        serde_json::json!({ "real": "yes", "reason": "synthetic", "severity": severity })
            .to_string(),
    );
    f
}

/// A style linter's row at the same place. The linter said "error", which the preview pass
/// reads as medium for a style linter.
fn lint_row(tool: &str) -> crate::onboard::Finding {
    crate::onboard::Finding {
        repo: "acme/web".to_string(),
        path: "src/profile.ts".to_string(),
        line: 21,
        rule_id: TOOL_RULE.to_string(),
        severity: "medium".to_string(),
        snippet: "const name = user!.name;".to_string(),
        preview: true,
        preview_tool: Some(tool.to_string()),
        kind: Some(camerata_api_types::audit::FindingKind::Defect),
        ..crate::onboard::Finding::default()
    }
}

#[test]
fn a_style_lint_error_folded_into_a_reviewed_low_row_stays_low_with_a_note() {
    for tool in ["eslint", "clippy"] {
        let set = corpus(None);
        let mut existing = vec![reviewed_model_row("low")];
        let out = dedup_scan_previews_with(
            &mut existing,
            vec![lint_row(tool)],
            Some(&selectors()),
            Some(&set),
        );
        assert!(out.is_empty(), "{tool}: the lint row folds in: {out:?}");
        assert_eq!(existing.len(), 1);
        assert_eq!(existing[0].flagged_by.len(), 1, "{tool}: the fold is kept");
        assert_eq!(existing[0].severity, "low", "{tool}");
        let notes = recorded_adjustments(&existing[0]).join(" | ");
        assert!(notes.contains("severity kept at low"), "{tool}: {notes}");
        assert!(notes.contains(tool), "{tool}: {notes}");
        // The bounds that run after the merge agree.
        crate::severity_bounds::apply_scan_severity_bounds(&mut existing, Some(&set));
        assert_eq!(existing[0].severity, "low", "{tool}");
    }
}

#[test]
fn a_corpus_authored_severity_for_the_check_still_applies() {
    let set = corpus(Some("medium"));
    let mut existing = vec![reviewed_model_row("low")];
    let out = dedup_scan_previews_with(
        &mut existing,
        vec![lint_row("eslint")],
        Some(&selectors()),
        Some(&set),
    );
    assert!(out.is_empty());
    assert_eq!(existing[0].severity, "medium");
    let notes = recorded_adjustments(&existing[0]).join(" | ");
    assert!(
        notes.contains("severity raised from low to medium"),
        "{notes}"
    );
    crate::severity_bounds::apply_scan_severity_bounds(&mut existing, Some(&set));
    assert_eq!(existing[0].severity, "medium");
}

/// A row no reviewer rated still takes the higher label, and now says so.
#[test]
fn a_merge_that_raises_an_unreviewed_row_records_the_adjustment() {
    let mut existing = vec![crate::onboard::Finding {
        captures: Default::default(),
        ..reviewed_model_row("low")
    }];
    let out = dedup_scan_previews_with(
        &mut existing,
        vec![lint_row("eslint")],
        Some(&selectors()),
        None,
    );
    assert!(out.is_empty());
    assert_eq!(existing[0].severity, "medium");
    let notes = recorded_adjustments(&existing[0]).join(" | ");
    assert!(
        notes.contains("severity raised from low to medium"),
        "{notes}"
    );
}

/// A security scanner is not a style linter: its higher rating still wins over a reviewed row,
/// with the note.
#[test]
fn a_security_scanner_label_still_wins_and_records_the_adjustment() {
    let mut existing = vec![reviewed_model_row("low")];
    let mut scanner = lint_row("semgrep");
    scanner.rule_id = NATIVE.to_string();
    scanner.severity = "high".to_string();
    let out = dedup_scan_previews_with(&mut existing, vec![scanner], None, None);
    assert!(out.is_empty());
    assert_eq!(existing[0].severity, "high");
    let notes = recorded_adjustments(&existing[0]).join(" | ");
    assert!(
        notes.contains("severity raised from low to high"),
        "{notes}"
    );
}

// ── Round 1 of PR #156's review ─────────────────────────────────────────────────────────────

/// `tool`'s row of `rule` at the same place, rated `severity`.
fn tool_row(tool: &str, rule: &str, severity: &str) -> crate::onboard::Finding {
    crate::onboard::Finding {
        rule_id: rule.to_string(),
        severity: severity.to_string(),
        preview_tool: Some(tool.to_string()),
        ..lint_row("eslint")
    }
}

/// `native` is the corpus rule `tool_rule` is enforced by; the model's row of it is reviewed low.
fn merge_into_reviewed_low(
    native: &str,
    tool: &str,
    tool_rule: &str,
    tool_severity: &str,
) -> crate::onboard::Finding {
    let mut m = std::collections::HashMap::new();
    // The mapping holds tool ids as the preview pass normalises them: lower case, no `clippy::`.
    let wanted = tool_rule.to_ascii_lowercase();
    m.insert(
        native.to_string(),
        vec![wanted
            .strip_prefix("clippy::")
            .unwrap_or(&wanted)
            .to_string()],
    );
    let mut model = reviewed_model_row("low");
    model.rule_id = native.to_string();
    let mut existing = vec![model];
    let out = dedup_scan_previews_with(
        &mut existing,
        vec![tool_row(tool, tool_rule, tool_severity)],
        Some(&m),
        None,
    );
    assert!(out.is_empty(), "{tool}: the row folds in: {out:?}");
    assert_eq!(existing.len(), 1);
    existing.remove(0)
}

/// Owner ruling: a Ruff style "error" no longer lifts a reviewed row, like ESLint and Clippy.
#[test]
fn a_ruff_style_error_folded_into_a_reviewed_low_row_stays_low_with_a_note() {
    let merged = merge_into_reviewed_low(NATIVE, "ruff", "E501", "high");
    assert_eq!(merged.severity, "low");
    let notes = recorded_adjustments(&merged).join(" | ");
    assert!(notes.contains("severity kept at low"), "{notes}");
    assert!(notes.contains("ruff"), "{notes}");
}

/// The exception: a security-family lint id still lifts, one test per family.
#[test]
fn an_eslint_plugin_security_error_still_lifts_a_reviewed_low_row() {
    let merged = merge_into_reviewed_low(
        NATIVE,
        "eslint",
        "security/detect-eval-with-expression",
        "high",
    );
    assert_eq!(
        merged.severity,
        "high",
        "{}",
        recorded_adjustments(&merged).join(" | ")
    );
    let notes = recorded_adjustments(&merged).join(" | ");
    assert!(
        notes.contains("severity raised from low to high"),
        "{notes}"
    );
}

#[test]
fn a_ruff_bandit_rule_error_still_lifts_a_reviewed_low_row() {
    let merged = merge_into_reviewed_low(NATIVE, "ruff", "S608", "high");
    assert_eq!(
        merged.severity,
        "high",
        "{}",
        recorded_adjustments(&merged).join(" | ")
    );
}

#[test]
fn a_clippy_lint_the_corpus_maps_to_a_security_rule_still_lifts_a_reviewed_low_row() {
    let merged = merge_into_reviewed_low(
        "SEC-SYN-UNSAFE-BLOCK-1",
        "clippy",
        "clippy::syn_unsafe_block",
        "high",
    );
    assert_eq!(
        merged.severity,
        "high",
        "{}",
        recorded_adjustments(&merged).join(" | ")
    );
}

/// M5: a corpus severity authored for the CHECK the preview reports, reached through the tool's
/// own id (the built-in map grounds `eqeqeq` in a corpus rule), outranks the style rule even
/// when the row it folds into is of another rule.
#[test]
fn a_corpus_severity_authored_for_the_tools_own_check_still_applies() {
    let mut rule = camerata_rules::bare_rule("JAVASCRIPT-STRICT-EQUALITY-1", "test-fixture");
    rule.min_severity = Some("medium".to_string());
    let mut set = camerata_rules::RuleSet::default();
    set.push(rule);
    // The reviewed row is of a rule the corpus authors nothing for, and no mapping ties it to
    // the tool's check, so only the preview's own id can reach the authored severity.
    let mut existing = reviewed_model_row("low");
    existing.rule_id = "SYN-UNRELATED-1".to_string();
    let preview = tool_row("eslint", "eqeqeq", "high");
    let merged = merged_preview_severity(&existing, &preview, None, Some(&set));
    assert_eq!(merged.severity, "high");
}
