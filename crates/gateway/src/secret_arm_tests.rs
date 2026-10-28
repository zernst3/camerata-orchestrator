//! The SEC content arms read `camerata-secrets` (docs/design/2026-10-23_secret-redaction-and-
//! release-gate.md section 3.3). Values are built at runtime by the crate's fixtures, so no
//! secret-shaped literal sits in this file.

use camerata_secrets::fixtures::{body, jwt, planted};
use camerata_secrets::SecretShape;

use super::*;

const ALNUM: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

#[test]
fn vendor_token_arm_denies_a_service_role_jwt_and_allows_the_anon_jwt() {
    let service = format!(
        "const client = createClient(url, \"{}\");",
        jwt(Some("service_role"))
    );
    assert!(arm_sec_no_vendor_token_1("src/db.ts", &service).is_err());
    for role in ["anon", "authenticated"] {
        let public = format!("const client = createClient(url, \"{}\");", jwt(Some(role)));
        assert!(
            arm_sec_no_vendor_token_1("src/db.ts", &public).is_ok(),
            "the {role} key is public by design"
        );
    }
}

#[test]
fn vendor_token_arm_denies_whsec_and_rk_live() {
    let webhook = planted(SecretShape::StripeWebhookSecret).text;
    assert!(arm_sec_no_vendor_token_1("src/pay.ts", &webhook).is_err());
    let restricted = format!("STRIPE_RESTRICTED=rk_live_{}", body(30, ALNUM, 21));
    assert!(arm_sec_no_vendor_token_1("src/pay.ts", &restricted).is_err());
}

#[test]
fn vendor_token_arm_denies_a_real_looking_stripe_test_key_and_lists_the_docs_key_as_a_twin_row() {
    // Owner decision (2026-10-23): a real-looking test-mode key is redacted like a live key.
    let test_key = planted(SecretShape::StripeTestSecretKey).text;
    assert!(arm_sec_no_vendor_token_1("src/pay.ts", &test_key).is_err());
    let docs = format!(
        "key = \"{}\"",
        camerata_secrets::fixtures::stripe_docs_test_key()
    );
    // The published documentation key is a twin; the arm still lists twins (a committed key is a
    // row), so it is denied exactly as a placeholder is. The redactor, not the arm, keeps it readable.
    assert!(arm_sec_no_vendor_token_1("src/pay.ts", &docs).is_err());
}

#[test]
fn vendor_token_arm_denies_a_supabase_dsn_with_inline_password() {
    let dsn = planted(SecretShape::ConnectionStringPassword).text;
    assert!(arm_sec_no_vendor_token_1("src/db.ts", &dsn).is_err());
    // A DSN on a non-Supabase host is a secret to the redactor but not a vendor-token finding.
    let other = format!(
        "DATABASE_URL=postgres://app:{}@db.internal.test:5432/app",
        body(20, ALNUM, 3)
    );
    assert!(arm_sec_no_vendor_token_1("src/db.ts", &other).is_ok());
}

#[test]
fn hardcoded_secrets_arm_denial_message_shows_only_the_redacted_form() {
    for shape in [
        SecretShape::NamedSecretLiteral,
        SecretShape::GitHubToken,
        SecretShape::AwsAccessKeyId,
    ] {
        let p = planted(shape);
        let message = arm_sec_no_hardcoded_secrets_1("src/config.rs", &p.text)
            .expect_err("a planted secret must be denied");
        assert!(
            message.contains('\u{2026}'),
            "message carries the redacted form: {message}"
        );
        assert!(
            !message.contains(&p.value),
            "message must never echo the value ({shape:?})"
        );
    }
}

#[test]
fn vendor_token_arm_denial_message_shows_only_the_redacted_form() {
    for shape in [
        SecretShape::SupabaseSecretKey,
        SecretShape::StripeLiveSecretKey,
        SecretShape::ConnectionStringPassword,
        SecretShape::SupabaseServiceRoleJwt,
    ] {
        let p = planted(shape);
        let message = arm_sec_no_vendor_token_1("src/config.rs", &p.text)
            .expect_err("a planted secret must be denied");
        assert!(message.contains('\u{2026}'), "{message}");
        assert!(
            !message.contains(&p.value),
            "message must never echo the value ({shape:?})"
        );
    }
}

#[test]
fn private_key_and_url_arms_never_echo_a_value() {
    let pem = planted(SecretShape::PemPrivateKey);
    let message = arm_sec_no_private_key_1("src/k.rs", &pem.text).expect_err("denied");
    for line in pem.value.lines() {
        assert!(!message.contains(line));
    }
    let url = planted(SecretShape::SecretInUrlQuery);
    let message = arm_arch_no_secrets_in_url_1("src/u.rs", &url.text).expect_err("denied");
    assert!(!message.contains(&url.value));
}

#[test]
fn content_match_lines_agrees_with_camerata_secrets_match_lines_for_every_sec_rule() {
    let mut text = String::from("// header\n");
    for shape in SecretShape::ALL {
        text.push_str(&planted(*shape).text);
        text.push_str("\n// between\n");
    }
    for rule in [
        "SEC-NO-VENDOR-TOKEN-1",
        "SEC-NO-HARDCODED-SECRETS-1",
        "SEC-NO-PRIVATE-KEY-1",
    ] {
        assert_eq!(
            content_match_lines(rule, &text),
            camerata_secrets::match_lines_for_rule(&text, rule, true),
            "{rule}"
        );
        assert!(
            !content_match_lines(rule, &text).is_empty(),
            "{rule} must find its shapes"
        );
    }
}

#[test]
fn url_rule_still_flags_the_structural_shapes_the_value_detector_ignores() {
    // ARCH-NO-SECRETS-IN-URL-1 is a structural lint (a secret-named param in a URL, whatever its
    // value, templates included); the redaction shape (S17) deliberately ignores short and
    // templated values. Both read one param list (`URL_SECRET_PARAMS`).
    assert!(!content_match_lines(
        "ARCH-NO-SECRETS-IN-URL-1",
        "https://api.x.com/q?api_key=abc123"
    )
    .is_empty());
    assert!(!content_match_lines(
        "ARCH-NO-SECRETS-IN-URL-1",
        "format!(\"{base}?id={id}&auth_token={t}\")"
    )
    .is_empty());
}

#[test]
fn the_hardcoded_arm_keeps_its_precision_guards() {
    // A path on a token-named variable and a hyphenated secret NAME are not secrets.
    assert!(arm_sec_no_hardcoded_secrets_1(
        "src/a.rs",
        "let token_path = \"app/api/auth/token/refresh/route\";"
    )
    .is_ok());
    assert!(
        arm_sec_no_hardcoded_secrets_1("src/a.rs", "let k = \"plaid-access-token-item-1\";")
            .is_ok()
    );
}
