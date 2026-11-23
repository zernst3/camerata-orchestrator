//! V1 item 34, round 1 fixes: identifiers written kebab or dotted, suffixed and unqualified
//! credential names, a key swallowed by a path literal, and the credential words `pass`, `auth`
//! and `bearer`.

use crate::fixtures::body;
use crate::{
    detect_all, detect_client_secrets, detect_secrets, redact_for_client, redact_str_for_client,
};

const PHRASE: &str = "blue-sky-seven-rivers-falcon";
const ALNUM: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

fn masked_for_client(text: &str, value: &str) {
    let masked = redact_for_client(text);
    assert!(
        !masked.text.contains(value),
        "the value must not survive: {}",
        masked.text
    );
    assert!(!masked.redactions.is_empty(), "nothing was masked: {text}");
}

fn readable_for_client(text: &str) {
    assert!(
        detect_client_secrets(text).is_empty(),
        "must stay readable: {text}"
    );
    assert_eq!(redact_str_for_client(text), text);
}

// ---- 1: kebab and dotted identifiers -----------------------------------------------------------

#[test]
fn a_passphrase_on_a_kebab_or_dotted_credential_identifier_is_masked() {
    for text in [
        format!("\"x-api-key\": \"{PHRASE}\""),
        format!("'api-key' => '{PHRASE}'"),
        format!("\"api.key\": \"{PHRASE}\""),
        format!("headers[\"X-Api-Key\"] = \"{PHRASE}\""),
        format!("\"x-auth-token\": \"{PHRASE}\""),
    ] {
        assert_eq!(detect_secrets(&text).len(), 1, "not a secret: {text}");
        masked_for_client(&text, PHRASE);
    }
    // Names in code stay names, kebab or not.
    readable_for_client("\"x-storage-key\": \"app-preferences-sidebar-collapsed\"");
    readable_for_client("\"api-key-header\": \"app-preferences-sidebar-collapsed\"");
}

// ---- 2: suffixes and more qualifiers -----------------------------------------------------------

#[test]
fn a_credential_name_with_an_environment_suffix_still_holds_a_passphrase() {
    for ident in [
        "API_KEY_PROD",
        "API_TOKEN_STAGING",
        "apiKeyV2",
        "access_token_value",
        "API_KEY_PRODUCTION_2",
        "AUTH_TOKEN_DEFAULT",
    ] {
        let text = format!("{ident} = \"{PHRASE}\"");
        assert_eq!(detect_secrets(&text).len(), 1, "not a secret: {text}");
        masked_for_client(&text, PHRASE);
    }
}

#[test]
fn an_unqualified_credential_key_name_holds_a_passphrase() {
    for ident in [
        "STRIPE_KEY",
        "CLIENT_KEY",
        "SERVICE_ROLE_KEY",
        "JWT_KEY",
        "SSH_KEY",
        "DEPLOY_KEY",
        "HMAC_KEY",
        "WEBHOOK_KEY",
        "APP_KEY",
        "CONSUMER_KEY",
        "CRYPTO_KEY",
        "BOT_KEY",
    ] {
        let text = format!("{ident} = \"{PHRASE}\"");
        assert_eq!(detect_secrets(&text).len(), 1, "not a secret: {text}");
        masked_for_client(&text, PHRASE);
    }
}

#[test]
fn names_in_code_stay_names_after_the_wider_rule() {
    let value = "app-preferences-sidebar-collapsed";
    for ident in [
        "STORAGE_KEY",
        "queryKey",
        "tokenName",
        "api_key_header",
        "AUTH_STORAGE_KEY",
        "keyboardLayout",
        "monkeyPatch",
    ] {
        readable_for_client(&format!("const {ident} = \"{value}\";"));
    }
}

// ---- 5: swallowing by a path literal ----------------------------------------------------------

#[test]
fn a_path_literal_does_not_hide_a_vendor_key_inside_it() {
    let hex = body(32, "0123456789abcdef", 5);
    let key = format!("key-{hex}");
    let text = format!("secret_file = \"secrets/prod/{key}/app\"");
    assert!(
        !detect_secrets(&text).is_empty(),
        "the floor missed the key: {text}"
    );
    masked_for_client(&text, &key);
    // The path itself, with no key in it, is still a readable path.
    readable_for_client("secret_file = \"secrets/prod/some-long-folder-name/app\"");
    // The overlap rule is the call site: one kept match, the inner one.
    let all = detect_all(&text);
    assert_eq!(all.len(), 1, "{all:?}");
}

// ---- 7: the words pass, auth and bearer as identifiers ----

#[test]
fn pass_auth_and_bearer_identifiers_are_detected() {
    let value = body(30, ALNUM, 21);
    for text in [
        format!("DB_PASS = \"{value}\""),
        format!("SMTP_PWD = \"{value}\""),
        format!("AUTH = \"{value}\""),
        format!("bearer: \"{value}\""),
        format!("{{\"auth\": \"{value}\"}}"),
        format!("DB_PASS={value}"),
        format!("AUTH: {value}"),
        format!("export BEARER={value}"),
        format!("pass = '{value}'"),
    ] {
        assert_eq!(detect_secrets(&text).len(), 1, "not detected: {text}");
        masked_for_client(&text, &value);
    }
}

#[test]
fn ordinary_words_that_contain_pass_or_auth_are_not_identifiers() {
    let value = body(30, ALNUM, 22);
    for ident in [
        "passive",
        "bypass",
        "compass",
        "author",
        "authority",
        "authorization",
        "passenger",
        "bearerless",
    ] {
        for text in [
            format!("const {ident} = \"{value}\";"),
            format!("{ident}={value}"),
            format!("{ident}: \"{value}\""),
        ] {
            assert!(detect_all(&text).is_empty(), "matched: {text}");
        }
    }
}

// ---- 8: the name is not "assigned" the way the round 1 reading wanted -----------------------------

#[test]
fn a_passphrase_on_a_credential_name_is_masked_whatever_the_line_looks_like() {
    let mut readable = Vec::new();
    for text in [
        // The unquoted pattern's match starts at the line start: indentation, `export`, `- `.
        format!("  api_key: {PHRASE}"),
        format!("- API_TOKEN={PHRASE}"),
        format!("export API_KEY={PHRASE}"),
        format!("export API_KEY_PROD={PHRASE}"),
        format!("  x-api-key: {PHRASE}"),
        // A fallback or a call argument after the name.
        format!("process.env.API_KEY || \"{PHRASE}\""),
        format!("process.env.API_KEY ?? \"{PHRASE}\""),
        format!("os.getenv(\"API_KEY\") or \"{PHRASE}\""),
        format!("if API_KEY != \"{PHRASE}\" {{"),
        format!("getenv(\"API_KEY\", \"{PHRASE}\")"),
        format!("set(\"api_key\", \"{PHRASE}\")"),
        format!("setApiKey(\"{PHRASE}\")"),
        // JSON inside a string, and the minor forms.
        format!("\\\"api_key\\\": \\\"{PHRASE}\\\""),
        format!("public string ApiKey {{ get; }} = \"{PHRASE}\";"),
        format!("API_KEY ?= \"{PHRASE}\""),
    ] {
        if detect_secrets(&text).len() != 1 || redact_for_client(&text).text.contains(PHRASE) {
            readable.push(text);
        }
    }
    assert!(readable.is_empty(), "shipped readable: {readable:#?}");
}

#[test]
fn a_quoted_string_in_a_list_is_still_a_value() {
    readable_for_client(
        "for rule in [\"SEC-NO-VENDOR-TOKEN-1\", \"SEC-NO-HARDCODED-SECRETS-1\"] {",
    );
    readable_for_client("  storage_key: app-preferences-sidebar-collapsed");
    readable_for_client("export STORAGE_KEY=app-preferences-sidebar-collapsed");
}
