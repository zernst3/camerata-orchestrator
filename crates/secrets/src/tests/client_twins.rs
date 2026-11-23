//! V1 item 34: a REAL secret never reaches a client package readable because a twin was mistaken
//! for a name, a placeholder or a public key. Each case also checks the legitimate twin it was
//! confused with stays readable.

use crate::detect::resolve;
use crate::fixtures::{body, jwt, jwt_with_subject, stripe_docs_test_key};
use crate::{
    detect_client_secrets, detect_secrets, redact_for_client, redact_str_for_client, SecretMatch,
    SecretShape, Twin,
};

const ALNUM_LOWER: &str = "abcdefghijklmnopqrstuvwxyz0123456789";

fn masked_for_client(text: &str, value: &str) {
    let masked = redact_for_client(text);
    assert!(
        !masked.text.contains(value),
        "the value must not survive: {}",
        masked.text
    );
    assert!(!masked.redactions.is_empty(), "nothing was masked: {text}");
    // The mask is stable: scanning the masked text finds nothing more.
    assert!(detect_client_secrets(&masked.text).is_empty());
}

fn readable_for_client(text: &str) {
    assert!(
        detect_client_secrets(text).is_empty(),
        "must stay readable: {text}"
    );
    assert_eq!(redact_str_for_client(text), text);
}

// ---- EnvNameLiteral --------------------------------------------------------------------------

#[test]
fn a_shouted_password_with_digits_is_masked_for_a_client() {
    let value = "ACME_PROD_7731_QX_DATABASE_MAIN";
    masked_for_client(&format!("DB_PASSWORD = \"{value}\""), value);
    // The twin it was confused with: an env variable NAME in quotes stays readable.
    readable_for_client("const serviceRoleKey = process.env['SUPABASE_SERVICE_ROLE_KEY'];");
}

#[test]
fn a_shouted_value_on_a_password_name_is_masked_even_without_digits() {
    let value = "ACME_PRODUCTION_DATABASE_MAIN";
    masked_for_client(&format!("DB_PASSWORD = \"{value}\""), value);
}

// ---- ReadableName ----------------------------------------------------------------------------

#[test]
fn a_passphrase_on_a_token_or_key_name_is_masked_for_a_client() {
    let value = "blue-sky-seven-rivers-falcon";
    for text in [
        format!("API_TOKEN = \"{value}\""),
        format!("const authToken = \"{value}\";"),
        format!("accessTokens: \"{value}\""),
        format!("api_key = \"{value}\""),
        format!("const apiKey = \"{value}\";"),
        format!("ACCESS_KEY=\"{value}\""),
        format!("AUTH_KEY: \"{value}\""),
    ] {
        assert_eq!(detect_secrets(&text).len(), 1, "not a secret: {text}");
        masked_for_client(&text, value);
    }
}

#[test]
fn code_constants_that_merely_contain_key_or_auth_words_stay_readable() {
    let value = "app-preferences-sidebar-collapsed";
    for text in [
        format!("STORAGE_KEY = \"{value}\""),
        format!("const queryKey = \"{value}\";"),
        format!("const keyboardLayout = \"{value}\";"),
        format!("const monkeyPatch = \"{value}\";"),
        format!("const AUTH_STORAGE_KEY = \"{value}\";"),
    ] {
        readable_for_client(&text);
    }
}

// ---- PublicKeyRole ---------------------------------------------------------------------------

#[test]
fn a_user_session_token_is_never_a_public_key() {
    for role in ["authenticated", "anon"] {
        let token = jwt_with_subject(role);
        let text = format!("const session = \"{token}\";");
        assert_eq!(detect_secrets(&text).len(), 1, "role {role} with a subject");
        masked_for_client(&text, &token);
    }
    // The twin it was confused with: a subject-less authenticated or anon key is public.
    for role in ["authenticated", "anon"] {
        readable_for_client(&format!(
            "NEXT_PUBLIC_SUPABASE_ANON_KEY={}",
            jwt(Some(role))
        ));
    }
}

// ---- swallowing ------------------------------------------------------------------------------

#[test]
fn a_longer_placeholder_literal_does_not_hide_a_docs_key_inside_it() {
    let key = stripe_docs_test_key();
    masked_for_client(&format!("const apiKey = \"example_{key}_rotate\";"), &key);
}

#[test]
fn a_longer_placeholder_literal_does_not_hide_a_real_key_inside_it() {
    let key = format!("sk_live_{}", body(30, ALNUM_LOWER, 77));
    let text = format!("const apiKey = \"example_{key}_rotate\";");
    assert!(
        !detect_client_secrets(&text).is_empty(),
        "not found: {text}"
    );
    masked_for_client(&text, &key);
}

/// The overlap rule itself: a longer readable match never hides a shorter one a client may not
/// see. Built by hand because no literal a real scan produces keeps its longer match readable
/// once a key sits inside it.
#[test]
fn a_longer_readable_match_gives_way_to_an_inner_match_a_client_may_not_see() {
    let key = stripe_docs_test_key();
    let text = format!("const label = \"app_sidebar_{key}_collapsed\";");
    let at = text.find(&key).unwrap_or(0);
    let outer_start = text.find("app_sidebar").unwrap_or(0);
    let outer_end = text.find("_collapsed").unwrap_or(0) + "_collapsed".len();
    let outer = SecretMatch {
        start: outer_start,
        end: outer_end,
        value: outer_start..outer_end,
        shape: SecretShape::NamedSecretLiteral,
        twin: Some(Twin::ReadableName),
        jwt_role: None,
    };
    let inner = SecretMatch {
        start: at,
        end: at + key.len(),
        value: at..at + key.len(),
        shape: SecretShape::StripeTestSecretKey,
        twin: Some(Twin::PublishedExampleKey),
        jwt_role: None,
    };
    let kept = resolve(vec![outer.clone(), inner.clone()], &text);
    assert_eq!(kept, vec![inner]);
    // Two readable matches: the longer one still wins, as before.
    let inside = SecretMatch {
        start: outer_start + 1,
        end: outer_start + 4,
        value: outer_start + 1..outer_start + 4,
        shape: SecretShape::NamedSecretLiteral,
        twin: Some(Twin::ReadableName),
        jwt_role: None,
    };
    assert_eq!(resolve(vec![outer.clone(), inside], &text), vec![outer]);
}

#[test]
fn a_plain_placeholder_literal_is_still_one_match_and_readable() {
    readable_for_client("const apiKey = \"your_example_placeholder_value_here\";");
}

// ---- residual placeholder holes (item 25 addition C) ----------------------------------------

#[test]
fn a_single_case_body_with_a_buried_word_is_masked_for_a_client() {
    let value = "kx9fake2mq8vlp3zr7wyn5bd1hs";
    masked_for_client(&format!("password = \"{value}\""), value);
    masked_for_client(&format!("sk_live_{value}"), value);
    // The twins it was confused with.
    readable_for_client("sb_secret_yourkeyhere");
    readable_for_client("const id = \"AKIAIOSFODNN7EXAMPLE\";");
    readable_for_client("const id = \"AKIAEXAMPLEONLY00000\";");
    readable_for_client("password = \"changeme1234567890123456\"");
    readable_for_client("API_KEY = \"FAKE_DO_NOT_USE_THIS_KEY_1234\"");
}

#[test]
fn a_human_password_with_a_delimited_word_is_masked_for_a_client() {
    let value = "MyCorp-Dummy-8842xQ-Prod-Main";
    masked_for_client(&format!("password = \"{value}\""), value);
    readable_for_client("password = \"your-password-goes-here-please\"");
    readable_for_client("password = \"REPLACE_WITH_YOUR_PASSWORD_v2\"");
}

#[test]
fn a_bearer_token_ending_in_a_delimited_word_is_masked_for_a_client() {
    let value = "Kx9mQ2vLp3Zr7wYn5Bd1Hs-example";
    masked_for_client(&format!("Authorization: Bearer {value}"), value);
    readable_for_client("Authorization: Bearer your_token_here_example");
}
