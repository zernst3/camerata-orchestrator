//! Scan-tool producers (docs/design/2026-10-23, paths A5 and A6): a tool's message can echo the
//! matched client text, so the row built from it is redacted at construction.

use camerata_secrets::fixtures::planted;
use camerata_secrets::SecretShape;

use super::*;

#[test]
fn preview_finding_redacts_a_tool_message_that_echoes_a_literal() {
    let p = planted(SecretShape::NamedSecretLiteral);
    let message = format!("hardcoded credential detected: {}", p.text);
    let f = preview_finding(
        "acme/app",
        ScanTool::Semgrep,
        "src/config.ts",
        4,
        "SEC-NO-HARDCODED-SECRETS-1",
        "high",
        &message,
    );
    let json = serde_json::to_string(&f).expect("serializes");
    assert!(!json.contains(&p.value), "preview row leaked: {json}");
    assert!(f.snippet.contains('\u{2026}'));
}

#[test]
fn preview_finding_redacts_before_the_160_char_cap() {
    // The echoed literal starts at char 141: capping first would leave a key fragment.
    let p = planted(SecretShape::StripeLiveSecretKey);
    let message = format!("{} {}", "m".repeat(140), p.value);
    let f = preview_finding(
        "acme/app",
        ScanTool::Eslint,
        "a.ts",
        1,
        "R-1",
        "low",
        &message,
    );
    assert!(f.snippet.chars().count() <= 160);
    assert!(!f.snippet.contains(&p.value[..19]), "{}", f.snippet);
}

#[test]
fn note_finding_redacts_tool_stderr_in_detail_and_reason_detail() {
    let p = planted(SecretShape::GitHubToken);
    let f = note_finding(
        "acme/app",
        "eslint",
        format!("eslint failed: token {} rejected", p.value),
    );
    let json = serde_json::to_string(&f).expect("serializes");
    assert!(!json.contains(&p.value), "note row leaked: {json}");
    assert!(
        f.detail.contains("ghp_"),
        "the redacted form stands in: {}",
        f.detail
    );
}
