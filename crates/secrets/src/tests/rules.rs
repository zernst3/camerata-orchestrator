//! Rule mapping, labels, and the pattern catalogue.

use crate::fixtures::{body, planted};
use crate::{
    detect_for_rule, is_supabase_dsn, match_lines_for_rule, shapes_for_rule, SecretShape,
    URL_SECRET_PARAMS,
};

#[test]
fn shapes_for_rule_maps_every_sec_rule_and_unknown_is_empty() {
    use SecretShape::*;
    assert_eq!(
        shapes_for_rule("SEC-NO-VENDOR-TOKEN-1"),
        &[
            SupabaseSecretKey,
            SupabaseServiceRoleJwt,
            StripeLiveSecretKey,
            StripeTestSecretKey,
            StripeWebhookSecret,
            AwsAccessKeyId,
            GitHubToken,
            SlackToken,
            GoogleApiKey,
            AnthropicKey,
            OpenAiKey,
            ConnectionStringPassword,
            AzureStorageConnectionString,
        ]
    );
    assert_eq!(
        shapes_for_rule("SEC-NO-HARDCODED-SECRETS-1"),
        &[
            SupabaseServiceRoleJwt,
            AwsAccessKeyId,
            GitHubToken,
            SlackToken,
            GoogleApiKey,
            OpenAiKey,
            PemPrivateKey,
            NamedSecretLiteral,
        ]
    );
    assert_eq!(shapes_for_rule("SEC-NO-PRIVATE-KEY-1"), &[PemPrivateKey]);
    assert_eq!(
        shapes_for_rule("ARCH-NO-SECRETS-IN-URL-1"),
        &[SecretInUrlQuery]
    );
    assert!(shapes_for_rule("SOME-OTHER-RULE-1").is_empty());
    assert!(shapes_for_rule("").is_empty());
    // Redaction-only shapes belong to no rule.
    for rule in [
        "SEC-NO-VENDOR-TOKEN-1",
        "SEC-NO-HARDCODED-SECRETS-1",
        "SEC-NO-PRIVATE-KEY-1",
        "ARCH-NO-SECRETS-IN-URL-1",
    ] {
        assert!(!shapes_for_rule(rule).contains(&BearerToken), "{rule}");
        assert!(!shapes_for_rule(rule).contains(&OpaqueJwt), "{rule}");
    }
}

#[test]
fn every_shape_has_a_label_display_name_and_prefix_decision() {
    let mut labels = std::collections::HashSet::new();
    for shape in SecretShape::ALL {
        assert!(!shape.label().is_empty());
        assert!(shape
            .label()
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == '_'));
        assert!(
            labels.insert(shape.label()),
            "duplicate label {}",
            shape.label()
        );
        assert!(!shape.display_name().is_empty());
        assert_eq!(
            shape.vendor_prefix().is_some(),
            shape.is_vendor_shaped() && *shape != SecretShape::PemPrivateKey
        );
        assert_eq!(
            shape.rank(),
            SecretShape::ALL
                .iter()
                .position(|s| s == shape)
                .unwrap_or(usize::MAX)
        );
    }
    // The label table is indexed by rank: spot-check both ends and the middle.
    assert_eq!(
        SecretShape::SupabaseSecretKey.label(),
        "supabase_secret_key"
    );
    assert_eq!(SecretShape::PemPrivateKey.label(), "pem_private_key");
    assert_eq!(SecretShape::SecretInUrlQuery.label(), "secret_in_url_query");
    assert_eq!(
        SecretShape::SupabaseSecretKey.vendor_prefix(),
        Some("sb_secret_")
    );
    assert!(SecretShape::SupabaseSecretKey.is_vendor_shaped());
    assert!(!SecretShape::NamedSecretLiteral.is_vendor_shaped());
}

#[test]
fn shape_serializes_as_its_variant_name() {
    let json = serde_json::to_string(&SecretShape::SupabaseSecretKey).unwrap_or_default();
    assert_eq!(json, "\"SupabaseSecretKey\"");
    let back: SecretShape = serde_json::from_str(&json).unwrap_or(SecretShape::OpaqueJwt);
    assert_eq!(back, SecretShape::SupabaseSecretKey);
}

#[test]
fn vendor_rule_lists_a_connection_string_only_on_a_supabase_host() {
    let supabase = planted(SecretShape::ConnectionStringPassword);
    assert_eq!(
        detect_for_rule(&supabase.text, "SEC-NO-VENDOR-TOKEN-1").len(),
        1
    );
    let other = format!(
        "DATABASE_URL=postgres://app:{}@db.internal.test:5432/app",
        body(20, "abcXYZ0123456789", 2)
    );
    assert!(detect_for_rule(&other, "SEC-NO-VENDOR-TOKEN-1").is_empty());
    // It is still a secret to the redactor.
    assert_eq!(crate::detect_secrets(&other).len(), 1);
}

#[test]
fn is_supabase_dsn_checks_the_host_not_the_whole_string() {
    assert!(is_supabase_dsn(
        "postgres://u:pw@db.abcdefghij.supabase.co:5432/postgres"
    ));
    assert!(is_supabase_dsn(
        "postgresql://u:pw@aws-0-us-east-1.pooler.supabase.com:6543/postgres"
    ));
    assert!(!is_supabase_dsn(
        "postgres://u:pw@db.internal.test:5432/x?note=supabase.co"
    ));
    assert!(!is_supabase_dsn("postgres://supabase.co:pw@evil.test/x"));
}

#[test]
fn match_lines_for_rule_applies_the_rule_filter() {
    let text = format!("a\n{}\nb\n", planted(SecretShape::SupabaseSecretKey).text);
    assert_eq!(
        match_lines_for_rule(&text, "SEC-NO-VENDOR-TOKEN-1", true),
        vec![2]
    );
    assert!(match_lines_for_rule(&text, "SEC-NO-PRIVATE-KEY-1", true).is_empty());
    assert!(match_lines_for_rule(&text, "NOT-A-RULE", true).is_empty());
}

#[test]
fn twins_count_decides_whether_a_placeholder_is_a_row() {
    let text = "SUPABASE_KEY=sb_secret_your_key_here_please";
    assert_eq!(
        match_lines_for_rule(text, "SEC-NO-VENDOR-TOKEN-1", true),
        vec![1]
    );
    assert!(match_lines_for_rule(text, "SEC-NO-VENDOR-TOKEN-1", false).is_empty());
}

#[test]
fn url_secret_param_list_is_the_documented_set() {
    for param in [
        "access_token",
        "refresh_token",
        "auth_token",
        "client_secret",
        "private_key",
        "api_?key",
        "passwd",
        "password",
        "pwd",
        "secret",
        "signature",
        "sig",
        "token",
        "key",
    ] {
        assert!(URL_SECRET_PARAMS.split('|').any(|p| p == param), "{param}");
    }
}

#[test]
fn patterns_compile_and_the_bound_matches_the_catalogue() {
    // Boundaries from the catalogue: S1 needs >= 10 body chars, S3 >= 16, S7 >= 20.
    let nine = format!("sb_secret_{}", body(9, "QRSTUVWXYZ0123456789", 1));
    let ten = format!("sb_secret_{}", body(10, "QRSTUVWXYZ0123456789", 2));
    assert!(crate::detect_all(&nine).is_empty());
    assert_eq!(crate::detect_all(&ten).len(), 1);
    let fifteen = format!("sk_live_{}", body(15, "QRSTUVWXYZ0123456789", 3));
    let sixteen = format!("sk_live_{}", body(16, "QRSTUVWXYZ0123456789", 4));
    assert!(crate::detect_all(&fifteen).is_empty());
    assert_eq!(crate::detect_all(&sixteen).len(), 1);
    let nineteen = format!("ghp_{}", body(19, "QRSTUVWXYZ0123456789", 5));
    let twenty = format!("ghp_{}", body(20, "QRSTUVWXYZ0123456789", 6));
    assert!(crate::detect_all(&nineteen).is_empty());
    assert_eq!(crate::detect_all(&twenty).len(), 1);
}
