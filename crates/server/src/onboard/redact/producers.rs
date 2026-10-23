//! Construction-time redaction at the deterministic producers and the export-side sanitizer
//! (docs/design/2026-10-23, paths A1 to A7, B8, F2). Secrets are built at runtime by
//! `camerata_secrets::fixtures`.

use camerata_api_types::audit::FindingKind;
use camerata_secrets::fixtures::{planted, planted_all};
use camerata_secrets::{detect_secrets, SecretShape};

use crate::onboard::{audit_content, Finding, MergedLocation};

/// The shapes the floor's rules govern.
const FLOOR_SHAPES: &[SecretShape] = &[
    SecretShape::SupabaseSecretKey,
    SecretShape::SupabaseServiceRoleJwt,
    SecretShape::StripeLiveSecretKey,
    SecretShape::StripeWebhookSecret,
    SecretShape::PemPrivateKey,
    SecretShape::NamedSecretLiteral,
    SecretShape::ConnectionStringPassword,
    SecretShape::GitHubToken,
    SecretShape::AwsAccessKeyId,
];

#[test]
fn floor_audit_content_snippet_is_redacted_for_each_shape() {
    for shape in FLOOR_SHAPES {
        let p = planted(*shape);
        let content = format!("# config\n{}\n", p.text);
        let findings = audit_content("acme/app", "src/config.env.ts", &content);
        assert!(
            !findings.is_empty(),
            "{shape:?}: the floor must still FIND the secret"
        );
        for f in &findings {
            let json = serde_json::to_string(f).expect("serializes");
            assert!(
                !p.value.lines().any(|line| json.contains(line)),
                "{shape:?} leaked in the floor row: {json}"
            );
            assert!(detect_secrets(&f.snippet).is_empty());
        }
    }
}

#[test]
fn floor_snippet_is_redacted_before_the_160_char_cap() {
    let p = planted(SecretShape::StripeLiveSecretKey);
    // The key starts near char 150, so a raw 160-char cap would keep a fragment of it.
    let line = format!(
        "const pad = \"{}\"; STRIPE_SECRET_KEY={}",
        "q".repeat(120),
        p.value
    );
    let findings = audit_content("acme/app", "src/pay.ts", &format!("{line}\n"));
    let row = findings
        .iter()
        .find(|f| f.rule_id == "SEC-NO-VENDOR-TOKEN-1")
        .expect("the vendor rule fires");
    assert!(row.snippet.chars().count() <= 160);
    assert!(!row.snippet.contains(&p.value[..14]), "{}", row.snippet);
}

#[test]
fn arch_violation_to_finding_redacts_message_and_object() {
    let p = planted(SecretShape::StripeLiveSecretKey);
    let v = camerata_checks::arch_checker::ArchViolation {
        rule_id: "SUPABASE-RLS-ENABLED-1".to_string(),
        file: "supabase/migrations/1.sql".to_string(),
        line: 3,
        object: Some(format!("public.{}", p.value)),
        message: format!("the policy compares to {}", p.value),
        severity: "high",
    };
    let f = crate::onboard::arch_violation_to_finding("acme/app", &v);
    let json = serde_json::to_string(&f).expect("serializes");
    assert!(!json.contains(&p.value), "{json}");
}

#[test]
fn dep_audit_advisory_text_is_redacted() {
    // OSV text is public data, but the class rule holds: no producer writes a value through.
    let p = planted(SecretShape::GitHubToken);
    let output = format!(
        r#"{{"results":[{{"source":{{"path":"/r/Cargo.lock","type":"lockfile"}},"packages":[{{"package":{{"name":"time","version":"0.1.45","ecosystem":"crates.io"}},"vulnerabilities":[{{"id":"RUSTSEC-2020-0071","summary":"leaks {}","database_specific":{{"severity":"HIGH"}}}}]}}]}}]}}"#,
        p.value
    );
    let findings =
        crate::dep_audit::parse_osv_json("acme/app", std::path::Path::new("/r"), &output)
            .expect("parses");
    assert_eq!(findings.len(), 1);
    let json = serde_json::to_string(&findings[0]).expect("serializes");
    assert!(!json.contains(&p.value), "{json}");
}

#[test]
fn sanitize_finding_text_covers_fix_specific_also_locations_and_rationale() {
    for p in planted_all() {
        let finding = Finding {
            snippet: "clean".to_string(),
            fix_specific: Some(format!("Replace {} now.", p.text)),
            calibration_rationale: Some(format!("because {}", p.text)),
            also_locations: vec![MergedLocation {
                repo: "acme/app".to_string(),
                path: ".env".to_string(),
                line: 1,
                rule_id: "R-1".to_string(),
                snippet: p.text.clone(),
                consequence: false,
            }],
            kind: Some(FindingKind::Defect),
            ..Finding::default()
        };
        let clean = crate::report_export::sanitize_finding_text(&finding);
        let json = serde_json::to_string(&clean).expect("serializes");
        assert!(
            !p.value.lines().any(|line| json.contains(line)),
            "{:?} survived the export sanitizer",
            p.shape
        );
    }
}

#[test]
fn sanitize_finding_text_strips_zero_width_inside_a_key_before_redacting() {
    // A zero-width space inside the key would hide it from the detector but not from a reader.
    let p = planted(SecretShape::SupabaseSecretKey);
    let (head, tail) = p.value.split_at(14);
    let hidden = format!("{head}\u{200B}{tail}");
    let finding = Finding {
        snippet: format!("KEY={hidden}"),
        ..Finding::default()
    };
    let clean = crate::report_export::sanitize_finding_text(&finding);
    assert!(!clean.snippet.contains(tail), "{}", clean.snippet);
    assert!(clean.snippet.contains("sb_secret_…"));
}

#[tokio::test]
async fn finding_context_lookup_redacts_the_returned_block() {
    let p = planted(SecretShape::StripeLiveSecretKey);
    let tmp = tempfile::tempdir().expect("tempdir");
    let source = format!(
        "fn f() -> &'static str {{\n    let key = \"{}\";\n    key\n}}\n",
        p.value
    );
    std::fs::create_dir_all(tmp.path().join("src")).expect("mkdir");
    std::fs::write(tmp.path().join("src/lib.rs"), &source).expect("write");
    // The stored snippet was redacted at construction; the staleness check must still match.
    let stored =
        crate::onboard::redact::snippet_from_line(&format!("    let key = \"{}\";", p.value), 160);
    let outcome =
        crate::onboard::finding_context::lookup(tmp.path(), "src/lib.rs", 2, Some(&stored)).await;
    match outcome {
        crate::onboard::finding_context::FindingContextOutcome::Ok {
            lines,
            matches_snippet,
            ..
        } => {
            assert_eq!(lines.len(), 4, "line count is preserved");
            assert!(lines.iter().all(|l| !l.contains(&p.value)), "{lines:?}");
            assert!(lines.iter().any(|l| l.contains("sk_live_")));
            assert!(
                matches_snippet,
                "a redacted stored snippet must not read as stale"
            );
        }
        other => panic!("expected Ok, got {other:?}"),
    }
}
