//! V1 item 25: what a CLIENT may not see unmasked. A key a vendor publishes in its documentation
//! is a twin for every other purpose, but a client package masks it (labelled as a published
//! example) and the release gate refuses it unmasked. Public-by-design values stay readable.

use crate::fixtures::{body, jwt, stripe_docs_test_key};
use crate::{
    detect_client_secrets, detect_secrets, find_secret_hits_in_json, redact, redact_for_client,
    redact_str_for_client, Twin,
};

const ALNUM_LOWER: &str = "abcdefghijklmnopqrstuvwxyz0123456789";

fn azurite_key() -> String {
    [
        "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1",
        "OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==",
    ]
    .concat()
}

#[test]
fn a_published_example_key_is_masked_for_a_client_and_labelled() {
    let key = stripe_docs_test_key();
    let text = format!("const stripe = require('stripe')('{key}');");
    let masked = redact_for_client(&text);
    assert_eq!(masked.redactions.len(), 1);
    assert!(!masked.text.contains(&key), "{}", masked.text);
    assert!(
        masked.text.contains("published example"),
        "the mask is labelled: {}",
        masked.text
    );
    // No part of the body survives, not even the last characters.
    let body_tail: String = key
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    assert!(!masked.text.contains(&body_tail), "{}", masked.text);
    // Idempotent, and the line count is kept.
    assert_eq!(redact_for_client(&masked.text).text, masked.text);
    assert_eq!(
        masked.text.matches('\n').count(),
        text.matches('\n').count()
    );
}

#[test]
fn the_other_redactors_still_leave_a_published_example_key_readable() {
    let key = stripe_docs_test_key();
    let text = format!("STRIPE_SECRET_KEY={key}");
    assert_eq!(redact(&text).text, text);
    assert!(detect_secrets(&text).is_empty());
    assert_eq!(detect_client_secrets(&text).len(), 1);
}

#[test]
fn the_azurite_emulator_key_is_masked_for_a_client_too() {
    let key = azurite_key();
    let conn =
        format!("DefaultEndpointsProtocol=http;AccountName=devstoreaccount1;AccountKey={key};");
    let masked = redact_str_for_client(&conn);
    assert!(!masked.contains(&key), "{masked}");
    assert!(masked.contains("published example"), "{masked}");
}

#[test]
fn a_setting_value_documented_by_a_vendor_is_not_a_key_and_stays_readable() {
    let value = ["lower_upper_", "letters_digits"].concat();
    let line = format!("password_requirements = \"{value}\"");
    assert!(detect_client_secrets(&line).is_empty());
    assert_eq!(redact_str_for_client(&line), line);
}

#[test]
fn public_by_design_values_stay_readable_for_a_client() {
    let publishable = format!("pk_live_{}", body(40, ALNUM_LOWER, 51));
    for line in [
        format!("NEXT_PUBLIC_STRIPE_KEY={publishable}"),
        format!("NEXT_PUBLIC_SUPABASE_ANON_KEY={}", jwt(Some("anon"))),
        format!(
            "NEXT_PUBLIC_SUPABASE_ANON_KEY={}",
            jwt(Some("authenticated"))
        ),
        "SUPABASE_KEY=sb_secret_your_key_here_please".to_string(),
    ] {
        assert!(detect_client_secrets(&line).is_empty(), "flagged: {line}");
        assert_eq!(redact_str_for_client(&line), line);
    }
}

#[test]
fn an_ordinary_secret_is_masked_for_a_client_exactly_as_before() {
    let line = format!("key = \"sk_live_{}\"", body(30, ALNUM_LOWER, 9));
    assert_eq!(redact_for_client(&line).text, redact(&line).text);
}

/// One arm per twin: a twin added tomorrow must be decided here and in `ships_unmasked`.
#[test]
fn every_twin_has_a_decided_client_policy() {
    let allowed = [
        Twin::PlaceholderWord("your"),
        Twin::EnvNameLiteral,
        Twin::PublicKeyRole("anon".to_string()),
        Twin::TemplateToken,
        Twin::PathLike,
        Twin::RedactionMarker,
        Twin::NoKeyMaterial,
        Twin::ReadableName,
        Twin::PublishableKey,
        Twin::FirebaseWebApiKey,
        Twin::QualifiedLabelName,
        Twin::BlockHeaderLabel,
        Twin::ModulePath,
        Twin::SealedCiphertext,
    ];
    for twin in &allowed {
        assert!(twin.ships_unmasked(), "{twin:?}");
    }
    assert!(!Twin::PublishedExampleKey.ships_unmasked());
    // The exhaustive match: adding a variant without deciding it fails to compile here.
    let decided = |t: &Twin| match t {
        Twin::PublishedExampleKey => false,
        Twin::PlaceholderWord(_)
        | Twin::EnvNameLiteral
        | Twin::PublicKeyRole(_)
        | Twin::TemplateToken
        | Twin::PathLike
        | Twin::RedactionMarker
        | Twin::NoKeyMaterial
        | Twin::ReadableName
        | Twin::PublishableKey
        | Twin::FirebaseWebApiKey
        | Twin::QualifiedLabelName
        | Twin::BlockHeaderLabel
        | Twin::ModulePath
        | Twin::SealedCiphertext => true,
    };
    for twin in allowed.iter().chain([&Twin::PublishedExampleKey]) {
        assert_eq!(decided(twin), twin.ships_unmasked(), "{twin:?}");
    }
}

#[test]
fn the_json_gate_walk_finds_an_unmasked_published_key_and_never_echoes_it() {
    let key = stripe_docs_test_key();
    let tree = serde_json::json!({ "a": [{ "snippet": format!("k = '{key}'") }], key.clone(): 1 });
    let hits = find_secret_hits_in_json(&tree);
    assert_eq!(hits.len(), 2, "{hits:?}");
    for hit in &hits {
        assert_eq!(hit.twin, Some(Twin::PublishedExampleKey));
        let shown = format!("{hit:?}");
        assert!(!shown.contains(&key), "{shown}");
    }
}

#[test]
fn the_mask_is_recognised_by_a_helper_and_a_real_redaction_is_not() {
    let key = stripe_docs_test_key();
    let masked = redact_str_for_client(&key).into_owned();
    assert!(crate::is_published_example_mask(&masked), "{masked}");
    let real = format!("sk_live_{}", body(30, ALNUM_LOWER, 9));
    let redacted = redact(&real).text;
    assert!(redacted.contains('\u{2026}'));
    assert!(!crate::is_published_example_mask(&redacted), "{redacted}");
    assert!(!crate::is_published_example_mask("plain text"));
}

/// Placeholder words clear a value on the client path only when delimited or single-case, the
/// rule vendor shapes already use. A word buried in mixed-case random text is not a placeholder.
#[test]
fn a_placeholder_word_buried_in_a_real_looking_value_is_masked_for_a_client() {
    for line in [
        "api_key = \"Kx9fakeT2mQ8vLp3Zr7wYn5Bd1Hs\"".to_string(),
        "DATABASE_URL=postgres://app:SampleCo2024xQ9@db.prod.internal/app".to_string(),
    ] {
        // The scan floor still reads the word as a placeholder; the client path does not.
        assert!(detect_secrets(&line).is_empty(), "twin expected: {line}");
        let masked = redact_str_for_client(&line);
        assert_ne!(masked, line, "shipped unmasked: {line}");
    }
}

#[test]
fn a_delimited_or_single_case_placeholder_still_ships_for_a_client() {
    for line in [
        "api_key = \"sk_live_your_key_here_please\"".to_string(),
        "SUPABASE_KEY=sb_secret_your_key_here_please".to_string(),
        "api_key = \"CHANGE_ME_TO_A_REAL_KEY_1234\"".to_string(),
        "DATABASE_URL=postgres://app:your-password@db.example.internal/app".to_string(),
    ] {
        assert!(detect_client_secrets(&line).is_empty(), "flagged: {line}");
    }
}

#[test]
fn the_github_docs_token_is_masked_for_a_client_too() {
    let token = ["ghp_", "16C7e42F292c6912", "E7710c838347Ae178B4a"].concat();
    let line = format!("GITHUB_TOKEN={token}");
    assert!(
        detect_secrets(&line).is_empty(),
        "a twin for every other purpose"
    );
    let masked = redact_str_for_client(&line);
    assert!(!masked.contains(&token), "{masked}");
    assert!(masked.contains("published example"), "{masked}");
}
