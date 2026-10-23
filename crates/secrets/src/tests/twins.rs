//! Safe twins: shaped like a secret, not a secret. Each stays readable and is never a gate hit.

use crate::fixtures::{body, jwt, planted, stripe_docs_test_key};
use crate::twins::PLACEHOLDER_WORDS;
use crate::{detect_all, detect_secrets, redact, SecretShape, Twin};

const ACCOUNT_ALPHABET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef0123456789";
const BODY_ALPHABET: &str = "QRSTUVWXYZ0123456789";

fn assert_twin(text: &str, expected_shape: SecretShape) -> Twin {
    let all = detect_all(text);
    assert_eq!(all.len(), 1, "expected one (twin) match, got {}", all.len());
    assert_eq!(all[0].shape, expected_shape);
    let twin = all[0].twin.clone().expect("must be a twin");
    assert!(detect_secrets(text).is_empty(), "a twin is not a secret");
    assert_eq!(redact(text).text, text, "a twin stays readable");
    twin
}

#[test]
fn twin_placeholder_word_in_vendor_body_stays_readable() {
    for word in PLACEHOLDER_WORDS {
        // Upper-case the word too: matching is case-insensitive.
        for variant in [word.to_string(), word.to_ascii_uppercase()] {
            // `sk-ant-` bodies admit `-` and `_`, so every word in the list can appear in one.
            let text = format!(
                "sk-ant-{}{variant}{}",
                body(12, BODY_ALPHABET, 1),
                body(12, BODY_ALPHABET, 2)
            );
            let twin = assert_twin(&text, SecretShape::AnthropicKey);
            assert!(matches!(twin, Twin::PlaceholderWord(_)), "word {word}");
        }
    }
}

#[test]
fn twin_uppercase_snake_case_body_with_do_not_and_fake_stays_readable() {
    // The portal-fixture shape: an all-caps body containing DO_NOT and FAKE.
    let text = "NEXT_PUBLIC_SUPABASE_SERVICE_ROLE_KEY=sb_secret_FAKE_DO_NOT_USE";
    let twin = assert_twin(text, SecretShape::SupabaseSecretKey);
    assert!(matches!(twin, Twin::PlaceholderWord(_)));
}

#[test]
fn twin_your_key_here_stays_readable() {
    assert_twin(
        "SUPABASE_SECRET=sb_secret_your_key_here",
        SecretShape::SupabaseSecretKey,
    );
}

#[test]
fn twin_env_name_literal_is_a_name_not_a_value() {
    // The faktura page-40 case: the NAME of the key, quoted, 25 chars, next to the word key.
    let text = "const serviceRoleKey = process.env['SUPABASE_SERVICE_ROLE_KEY'];";
    assert_twin(text, SecretShape::NamedSecretLiteral);
    assert!(matches!(
        detect_all(text)[0].twin,
        Some(Twin::EnvNameLiteral)
    ));
}

#[test]
fn twin_published_stripe_docs_test_key_by_digest() {
    let key = stripe_docs_test_key();
    assert_eq!(key.len(), 32);
    let twin = assert_twin(
        &format!("STRIPE_KEY={key}"),
        SecretShape::StripeTestSecretKey,
    );
    assert_eq!(twin, Twin::PublishedExampleKey);
}

#[test]
fn not_a_twin_real_looking_sk_test_is_redacted() {
    // Owner decision (2026-10-23): a real-looking test-mode key is redacted like a live key.
    let p = planted(SecretShape::StripeTestSecretKey);
    let all = detect_all(&p.text);
    assert_eq!(all.len(), 1);
    assert!(all[0].twin.is_none());
    assert!(!redact(&p.text).text.contains(&p.value));
}

#[test]
fn twin_published_aws_example_ids() {
    for id in ["AKIAIOSFODNN7EXAMPLE", "ASIAIOSFODNN7EXAMPLE"] {
        assert_twin(
            &format!("aws_access_key_id = {id}"),
            SecretShape::AwsAccessKeyId,
        );
    }
}

#[test]
fn twin_published_github_format_example() {
    let token = ["ghp_", "16C7e42F292c6912", "E7710c838347Ae178B4a"].concat();
    let twin = assert_twin(&format!("token: {token}"), SecretShape::GitHubToken);
    assert_eq!(twin, Twin::PublishedExampleKey);
}

#[test]
fn twin_supabase_anon_and_authenticated_jwt_are_the_public_key() {
    for role in ["anon", "authenticated"] {
        let token = jwt(Some(role));
        let twin = assert_twin(&token, SecretShape::OpaqueJwt);
        assert_eq!(twin, Twin::PublicKeyRole(role.to_string()));
        assert!(
            !twin.counts_as_line(),
            "a public key is never a finding row"
        );
    }
}

#[test]
fn twin_supabase_publishable_prefix_is_never_matched() {
    let text = format!(
        "NEXT_PUBLIC_SUPABASE_ANON_KEY=sb_publishable_{}",
        body(40, BODY_ALPHABET, 3)
    );
    assert!(detect_all(&text).is_empty());
}

#[test]
fn twin_stripe_publishable_live_and_test_are_never_matched() {
    for prefix in ["pk_live_", "pk_test_"] {
        let text = format!(
            "STRIPE_PUBLISHABLE={prefix}51{}",
            body(60, BODY_ALPHABET, 4)
        );
        assert!(detect_all(&text).is_empty(), "{prefix}");
    }
}

#[test]
fn twin_template_tokens_in_dsn_and_query_stay_readable() {
    for template in [
        "[YOUR-PASSWORD]",
        "${PASSWORD}",
        "{{token}}",
        "<secret>",
        "%s",
        "$DB_PASS",
    ] {
        let dsn = format!("postgres://postgres:{template}@db.abcdefghij.supabase.co:5432/postgres");
        // Whether the dsn pattern matches the template at all, it must never be a secret.
        assert!(detect_secrets(&dsn).is_empty(), "dsn template {template}");
        assert_eq!(redact(&dsn).text, dsn);
        let query = format!("https://x.test/cb?token={template}&a=1");
        assert!(
            detect_secrets(&query).is_empty(),
            "query template {template}"
        );
        assert_eq!(redact(&query).text, query);
    }
}

#[test]
fn twin_template_word_values_stay_readable() {
    for word in ["password", "secret", "token", "changeme"] {
        let dsn = format!("postgres://postgres:{word}@localhost:5432/app");
        assert!(detect_secrets(&dsn).is_empty(), "{word}");
    }
}

#[test]
fn twin_path_like_literal_on_a_credential_named_identifier_stays_readable() {
    let text = "const tokenRoute = \"app/api/auth/token/refresh/route\";";
    let twin = assert_twin(text, SecretShape::NamedSecretLiteral);
    assert_eq!(twin, Twin::PathLike);
}

#[test]
fn twin_prose_secret_hyphen_prefixed_does_not_match() {
    // Seen in the portal export: "sb_secret_-prefixed" is prose, not a value.
    assert!(detect_all("a key with the sb_secret_-prefixed shape").is_empty());
}

#[test]
fn twin_redaction_marker_never_rematches() {
    for p in crate::fixtures::planted_all() {
        let once = redact(&p.text).text;
        assert!(
            detect_all(&once).iter().all(|m| m.twin.is_some()),
            "{:?}",
            p.shape
        );
    }
}

#[test]
fn twin_ordinary_prose_about_keys_tokens_and_bearer_bonds_is_untouched() {
    let sample = "The API key rotation policy says to rotate secrets every 90 days. \
        Please update your token before the deadline. Bearer bonds are a kind of \
        debt instrument, unrelated to HTTP auth.";
    assert!(
        detect_all(sample).is_empty(),
        "false positive on ordinary prose"
    );
}

#[test]
fn short_vendor_lookalikes_are_not_flagged() {
    for text in [
        "AKIA1234",
        "this AKIAMENTIONS bird migration",
        "sk_live_short",
        "sk-short",
        "ghp_abc",
        "Bearer",
        "Bearer 123",
        "the bearer of this letter",
    ] {
        assert!(detect_all(text).is_empty(), "{text}");
    }
}

#[test]
fn azure_account_key_clause_alone_without_the_protocol_clause_is_not_flagged() {
    let material = body(64, ACCOUNT_ALPHABET, 9);
    assert!(detect_all(&format!("AccountKey={material}==")).is_empty());
}

#[test]
fn not_a_twin_env_example_filename_does_not_change_detection() {
    // The detector has no path input: redaction is content-only. A template FILE NAME never
    // makes its contents readable (a real-looking live key in `.env.local.example` still leaks).
    let p = planted(SecretShape::StripeLiveSecretKey);
    let content = format!("# .env.local.example\n{}\n", p.text);
    assert!(!redact(&content).text.contains(&p.value));
}
