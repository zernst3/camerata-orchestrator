//! The redacted form, per shape, plus the structural guarantees (line count, idempotence).

use super::{count, tail};
use crate::fixtures::{body, jwt, planted, planted_all};
use crate::{redact, redact_str, SecretShape};

const LOWER_DIGITS: &str = "abcdefghijklmnopqrstuvwxyz0123456789";

/// The expected redacted value for each fixture, written out by hand per shape (not derived
/// from the production prefix table) so the two can disagree and fail.
fn expected_form(shape: SecretShape, value: &str) -> String {
    let n = count(value);
    // Owner decision (d): a body of 20 chars or fewer shows its length only.
    let with_prefix = |prefix: &str| {
        let body = n - count(prefix);
        let shown = if body > 20 {
            tail(value)
        } else {
            String::new()
        };
        format!("{prefix}…{shown} ({n} chars)")
    };
    match shape {
        SecretShape::SupabaseSecretKey => with_prefix("sb_secret_"),
        SecretShape::SupabaseServiceRoleJwt => {
            format!("eyJ…{} ({n} chars, role service_role)", tail(value))
        }
        SecretShape::OpaqueJwt => format!("eyJ…{} ({n} chars, role unknown)", tail(value)),
        SecretShape::StripeLiveSecretKey => with_prefix("sk_live_"),
        SecretShape::StripeTestSecretKey => with_prefix("sk_test_"),
        SecretShape::StripeWebhookSecret => with_prefix("whsec_"),
        SecretShape::AwsAccessKeyId => with_prefix("AKIA"),
        SecretShape::GitHubToken => with_prefix("ghp_"),
        SecretShape::SlackToken => with_prefix("xoxb-"),
        SecretShape::GoogleApiKey => with_prefix("AIza"),
        SecretShape::AnthropicKey => with_prefix("sk-ant-"),
        SecretShape::OpenAiKey => with_prefix("sk-proj-"),
        SecretShape::SendGridApiKey => with_prefix("SG."),
        SecretShape::TwilioApiKey => with_prefix("SK"),
        SecretShape::ShopifyToken => with_prefix("shpat_"),
        SecretShape::MailgunApiKey => with_prefix("key-"),
        SecretShape::NpmToken => with_prefix("npm_"),
        SecretShape::MapboxSecretToken => with_prefix("sk."),
        SecretShape::DiscordBotToken
        | SecretShape::AwsSecretAccessKey
        | SecretShape::TwilioAuthToken
        | SecretShape::NpmrcAuthToken
        | SecretShape::DockerAuthEntry
        | SecretShape::PgPassPassword
        | SecretShape::NamedSecretLiteral
        | SecretShape::BearerToken
        | SecretShape::BasicAuthCredential
        | SecretShape::ConnectionStringPassword
        | SecretShape::AzureStorageConnectionString
        | SecretShape::SecretInUrlQuery => with_prefix(""),
        SecretShape::PemPrivateKey => "[RSA private key material: 3 lines redacted]".to_string(),
    }
}

#[test]
fn redacts_every_shape_to_prefix_tail_length() {
    for p in planted_all() {
        let out = redact(&p.text);
        let expected = expected_form(p.shape, &p.value);
        assert_eq!(out.redactions.len(), 1, "{:?}", p.shape);
        assert_eq!(out.redactions[0].shape, p.shape);
        let form = out.redactions[0].form.trim_matches('\n');
        assert_eq!(form, expected, "form for {:?}", p.shape);
        assert!(
            !out.text.contains(&p.value),
            "{:?} leaked its value",
            p.shape
        );
        assert!(
            out.text.contains(&expected),
            "{:?} form missing from output",
            p.shape
        );
    }
}

#[test]
fn surrounding_syntax_survives_for_the_context_shapes() {
    let dsn = planted(SecretShape::ConnectionStringPassword);
    let out = redact(&dsn.text).text;
    assert!(
        out.starts_with("DATABASE_URL=postgres://postgres:…"),
        "{out}"
    );
    assert!(
        out.ends_with("@db.abcdefghij.supabase.co:5432/postgres"),
        "{out}"
    );

    let url = planted(SecretShape::SecretInUrlQuery);
    let out = redact(&url.text).text;
    assert!(
        out.starts_with("https://api.vendor.test/v1/items?token=…"),
        "{out}"
    );
    assert!(out.ends_with("&page=2"), "{out}");

    let named = planted(SecretShape::NamedSecretLiteral);
    let out = redact(&named.text).text;
    assert!(out.starts_with("const vendorApiKey = \"…"), "{out}");
    assert!(out.ends_with("\";"), "{out}");

    let bearer = planted(SecretShape::BearerToken);
    assert!(redact(&bearer.text)
        .text
        .starts_with("Authorization: Bearer …"));

    let azure = planted(SecretShape::AzureStorageConnectionString);
    let out = redact(&azure.text).text;
    assert!(out.contains("AccountName=acct;AccountKey=…"), "{out}");
    assert!(out.ends_with(";EndpointSuffix=core.windows.net"), "{out}");
}

#[test]
fn bodies_of_twenty_chars_or_fewer_show_their_length_only() {
    // Owner decision (d). Vendor shape: the body is what follows the prefix.
    let twenty = format!("sb_secret_{}", body(20, "QRSTUVWXYZ0123456789", 3));
    assert_eq!(redact(&twenty).text, "sb_secret_… (30 chars)");
    let twenty_one = format!("sb_secret_{}", body(21, "QRSTUVWXYZ0123456789", 4));
    assert_eq!(
        redact(&twenty_one).text,
        format!("sb_secret_…{} (31 chars)", tail(&twenty_one))
    );
    // An AWS access key id has a 16-char body: no tail.
    let aws = planted(SecretShape::AwsAccessKeyId).value;
    assert_eq!(redact(&aws).text, "AKIA… (20 chars)");
    // The 10-char minimum body still redacts.
    let ten = format!("sb_secret_{}", body(10, LOWER_DIGITS, 5));
    assert_eq!(redact(&ten).text, "sb_secret_… (20 chars)");
}

#[test]
fn prefixless_shapes_show_a_tail_only_above_twenty_chars() {
    let twenty = body(20, "QRSTUVWXYZ0123456789", 6);
    let out = redact(&format!("https://x.test/a?token={twenty}&b=1")).text;
    assert_eq!(out, "https://x.test/a?token=… (20 chars)&b=1");
    let twenty_one = body(21, "QRSTUVWXYZ0123456789", 7);
    let out = redact(&format!("https://x.test/a?token={twenty_one}&b=1")).text;
    assert_eq!(
        out,
        format!(
            "https://x.test/a?token=…{} (21 chars)&b=1",
            tail(&twenty_one)
        )
    );
    let named = body(25, "QRSTUVWXYZ0123456789", 8);
    let out = redact(&format!("const authToken = \"{named}\";")).text;
    assert_eq!(
        out,
        format!("const authToken = \"…{} (25 chars)\";", tail(&named))
    );
}

#[test]
fn connection_string_redacts_only_the_password_and_keeps_user_and_host() {
    let p = planted(SecretShape::ConnectionStringPassword);
    let out = redact(&p.text).text;
    assert!(out.contains("postgres://postgres:"));
    assert!(out.contains("@db.abcdefghij.supabase.co:5432/postgres"));
    assert!(!out.contains(&p.value));
}

#[test]
fn url_query_redacts_only_the_value_and_keeps_the_param_name() {
    let p = planted(SecretShape::SecretInUrlQuery);
    let out = redact(&p.text).text;
    assert!(out.contains("?token=…"));
    assert!(out.contains("&page=2"));
}

#[test]
fn named_literal_redacts_only_the_quoted_literal_and_keeps_the_identifier() {
    let p = planted(SecretShape::NamedSecretLiteral);
    let out = redact(&p.text).text;
    assert!(out.starts_with("const vendorApiKey = \""));
}

#[test]
fn bearer_redacts_only_the_token() {
    let p = planted(SecretShape::BearerToken);
    let out = redact(&p.text).text;
    assert!(out.starts_with("Authorization: Bearer "));
    assert!(!out.contains(&p.value));
}

#[test]
fn pem_block_collapses_body_lines_and_keeps_line_count() {
    let p = planted(SecretShape::PemPrivateKey);
    let text = format!("before\n{}\nafter", p.text);
    let out = redact(&text).text;
    assert_eq!(out.matches('\n').count(), text.matches('\n').count());
    assert!(out.contains(
        "-----BEGIN RSA PRIVATE KEY-----\n[RSA private key material: 3 lines redacted]\n"
    ));
    assert!(out.contains("-----END RSA PRIVATE KEY-----\nafter"));
    for line in p.value.lines() {
        assert!(!out.contains(line));
    }
}

#[test]
fn redaction_preserves_newline_count_for_every_shape() {
    for p in planted_all() {
        let text = format!("line one\n{}\nline three\n\nline five\n", p.text);
        let out = redact(&text).text;
        assert_eq!(
            out.matches('\n').count(),
            text.matches('\n').count(),
            "newline count changed for {:?}",
            p.shape
        );
        assert_eq!(out.lines().next(), Some("line one"));
        assert_eq!(out.lines().last(), Some("line five"));
    }
}

#[test]
fn redaction_is_idempotent_for_every_shape() {
    for p in planted_all() {
        let once = redact(&p.text).text;
        let twice = redact(&once);
        assert_eq!(twice.text, once, "{:?} is not idempotent", p.shape);
        assert!(
            twice.redactions.is_empty(),
            "{:?} re-matched its own form",
            p.shape
        );
    }
}

#[test]
fn redacted_text_is_clean_for_the_detector() {
    for p in planted_all() {
        let once = redact(&p.text).text;
        assert!(
            crate::detect_secrets(&once).is_empty(),
            "{:?} redacted form still matches a shape",
            p.shape
        );
    }
}

#[test]
fn redaction_replaces_every_occurrence_and_keeps_the_text_between() {
    let a = planted(SecretShape::StripeLiveSecretKey).value;
    let b = planted(SecretShape::SupabaseSecretKey).value;
    let text = format!("one {a} two {b} three {a}");
    let out = redact(&text);
    assert_eq!(out.redactions.len(), 3);
    assert!(out.text.starts_with("one sk_live_…"));
    assert!(out.text.contains(" two sb_secret_…"));
    assert!(out.text.contains(" three sk_live_…"));
    assert!(!out.text.contains(&a) && !out.text.contains(&b));
}

#[test]
fn redact_str_borrows_when_nothing_matched() {
    let clean = "just some ordinary source code";
    assert!(matches!(redact_str(clean), std::borrow::Cow::Borrowed(_)));
    let secret = planted(SecretShape::GitHubToken).text;
    assert!(matches!(redact_str(&secret), std::borrow::Cow::Owned(_)));
}

#[test]
fn redactions_record_the_span_and_never_the_value() {
    let p = planted(SecretShape::StripeWebhookSecret);
    let out = redact(&p.text);
    assert_eq!(&p.text[out.redactions[0].span.clone()], p.value);
    assert!(!out.redactions[0].form.contains(&p.value));
}

#[test]
fn jwt_form_carries_total_length_and_role() {
    let token = jwt(Some("service_role"));
    let out = redact(&token).text;
    assert_eq!(
        out,
        format!(
            "eyJ…{} ({} chars, role service_role)",
            tail(&token),
            count(&token)
        )
    );
}

#[test]
fn multibyte_text_around_a_secret_is_preserved() {
    let p = planted(SecretShape::GitHubToken);
    let text = format!("// 日本語のコメント\n{} // ünïcode\n", p.text);
    let out = redact(&text).text;
    assert!(out.starts_with("// 日本語のコメント\n"));
    assert!(out.ends_with(" // ünïcode\n"));
    assert!(!out.contains(&p.value));
}
