//! The vendor and credential-file shapes added by the leak hunt (owner decision 2026-10-23 (f)),
//! each with a positive test and a safe-twin (or look-alike) test. Every value is built at
//! runtime from fragments.

use crate::fixtures::{body, planted};
use crate::{detect_all, detect_secrets, redact, SecretShape, Twin};

const ALNUM: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const URL_SAFE: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-";
const BASE64: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const HEX: &str = "0123456789abcdef";

/// The one secret in `text`, as `(shape, value)`; panics (without echoing `text`) otherwise.
fn the_secret(text: &str) -> (SecretShape, String) {
    let found = detect_secrets(text);
    assert_eq!(found.len(), 1, "expected exactly one secret");
    (found[0].shape, text[found[0].value.clone()].to_string())
}

fn assert_redacted(text: &str, shape: SecretShape, value: &str) {
    assert_eq!(the_secret(text), (shape, value.to_string()));
    let out = redact(text).text;
    assert!(!out.contains(value), "{shape:?} leaked its value");
    assert_eq!(redact(&out).text, out, "{shape:?} is not idempotent");
}

#[test]
fn the_new_shapes_have_labels_prefixes_and_display_names() {
    use SecretShape::*;
    for (shape, label, prefix) in [
        (SendGridApiKey, "sendgrid_api_key", Some("SG.")),
        (TwilioApiKey, "twilio_api_key", Some("SK")),
        (ShopifyToken, "shopify_token", Some("shpat_")),
        (MailgunApiKey, "mailgun_api_key", Some("key-")),
        (NpmToken, "npm_token", Some("npm_")),
        (MapboxSecretToken, "mapbox_secret_token", Some("sk.")),
        (DiscordBotToken, "discord_bot_token", None),
        (AwsSecretAccessKey, "aws_secret_access_key", None),
        (TwilioAuthToken, "twilio_auth_token", None),
        (NpmrcAuthToken, "npmrc_auth_token", None),
        (DockerAuthEntry, "docker_auth_entry", None),
        (PgPassPassword, "pgpass_password", None),
        (BasicAuthCredential, "basic_auth_credential", None),
    ] {
        assert_eq!(shape.label(), label);
        assert_eq!(shape.vendor_prefix(), prefix, "{label}");
        assert!(!shape.display_name().is_empty());
        assert_eq!(shape.is_vendor_shaped(), prefix.is_some(), "{label}");
    }
}

#[test]
fn sendgrid_key_is_a_secret_and_a_placeholder_is_not() {
    let p = planted(SecretShape::SendGridApiKey);
    assert_redacted(&p.text, SecretShape::SendGridApiKey, &p.value);
    let twin = format!("SG.your_sendgrid_key_here.{:-<43}", "replace-with-your-key");
    let all = detect_all(&twin);
    assert_eq!(all.len(), 1);
    assert!(matches!(all[0].twin, Some(Twin::PlaceholderWord(_))));
    assert!(detect_secrets(&twin).is_empty());
    // Wrong segment lengths are not a SendGrid key.
    assert!(detect_all(&format!(
        "SG.{}.{}",
        body(10, URL_SAFE, 3),
        body(43, URL_SAFE, 4)
    ))
    .is_empty());
}

#[test]
fn twilio_api_key_sid_is_a_secret_and_look_alikes_are_not() {
    let p = planted(SecretShape::TwilioApiKey);
    assert_redacted(&p.text, SecretShape::TwilioApiKey, &p.value);
    // One hex digit short, a longer word around it, lowercase prefix, non-hex body.
    for lookalike in [
        format!("SK{}", body(31, HEX, 5)),
        format!("RISK{}", body(32, HEX, 6)),
        format!("sk{}", body(32, HEX, 7)),
        format!("SK{}", "g".repeat(32)),
    ] {
        assert!(detect_secrets(&lookalike).is_empty(), "{lookalike}");
    }
}

#[test]
fn twilio_auth_token_needs_a_token_named_line_and_32_hex() {
    let p = planted(SecretShape::TwilioAuthToken);
    assert_redacted(&p.text, SecretShape::TwilioAuthToken, &p.value);
    let value = body(32, HEX, 8);
    for text in [
        format!("const authToken = \"{value}\";"),
        format!("auth_token: {value}"),
        format!("{{\"twilio_secret\": \"{value}\"}}"),
    ] {
        assert!(
            !redact(&text).text.contains(&value),
            "{}",
            text.replace(&value, "<v>")
        );
    }
    // 31 hex digits is not a Twilio auth token (it is still a credential-named assignment).
    let short = format!("TWILIO_AUTH_TOKEN={}", body(31, HEX, 9));
    assert_eq!(the_secret(&short).0, SecretShape::NamedSecretLiteral);
    // A placeholder is neither.
    assert!(detect_secrets("TWILIO_AUTH_TOKEN=your_twilio_auth_token_here").is_empty());
}

#[test]
fn slack_app_level_token_is_a_slack_token() {
    let value = format!(
        "xapp-1-A{}-{}-{}",
        body(10, ALNUM, 10),
        body(13, "0123456789", 11),
        body(40, HEX, 12)
    );
    let text = format!("SLACK_APP_TOKEN={value}");
    assert_redacted(&text, SecretShape::SlackToken, &value);
    assert!(redact(&value).text.starts_with("xapp-…"));
    assert!(detect_secrets("see xapp-docs for details").is_empty());
}

#[test]
fn shopify_tokens_of_all_three_prefixes_are_secrets() {
    for prefix in ["shpat_", "shpss_", "shpca_"] {
        let value = format!("{prefix}{}", body(32, HEX, 13));
        let text = format!("SHOPIFY_TOKEN={value}");
        assert_redacted(&text, SecretShape::ShopifyToken, &value);
        assert!(redact(&value).text.starts_with(&format!("{prefix}…")));
    }
    let placeholder = "shpat_your_shopify_admin_token_goes_here";
    assert!(detect_secrets(placeholder).is_empty());
    assert!(detect_secrets(&format!("shpat_{}", body(10, HEX, 14))).is_empty());
}

#[test]
fn mailgun_key_is_a_secret_and_look_alikes_are_not() {
    let p = planted(SecretShape::MailgunApiKey);
    assert_redacted(&p.text, SecretShape::MailgunApiKey, &p.value);
    for lookalike in [
        format!("key-{}", body(31, HEX, 15)),
        format!("monkey-{}", body(32, HEX, 16)),
        format!("key-{}", "z".repeat(32)),
    ] {
        assert!(detect_secrets(&lookalike).is_empty(), "{lookalike}");
    }
}

#[test]
fn npm_token_is_a_secret_and_a_placeholder_is_not() {
    let p = planted(SecretShape::NpmToken);
    assert_redacted(&p.text, SecretShape::NpmToken, &p.value);
    assert!(detect_secrets("NPM_TOKEN=npm_your_npm_access_token_goes_here_ok").is_empty());
    assert!(detect_secrets(&format!("npm_{}", body(20, ALNUM, 17))).is_empty());
}

#[test]
fn discord_bot_token_is_a_secret_and_dotted_words_are_not() {
    let p = planted(SecretShape::DiscordBotToken);
    assert_redacted(&p.text, SecretShape::DiscordBotToken, &p.value);
    assert!(detect_secrets("Module.Namespace.Class").is_empty());
    let placeholder = format!(
        "M{}.{}.{}",
        "your_bot_token_goes_here_1",
        "x".repeat(6),
        "y".repeat(27)
    );
    assert!(detect_secrets(&placeholder).is_empty());
}

#[test]
fn mapbox_secret_token_is_a_secret_and_the_public_token_is_not() {
    let p = planted(SecretShape::MapboxSecretToken);
    assert_redacted(&p.text, SecretShape::MapboxSecretToken, &p.value);
    assert!(redact(&p.value).text.starts_with("sk.…"));
    // The public Mapbox token is `pk.`.
    let public = format!(
        "pk.eyJ{}.{}",
        body(80, URL_SAFE, 18),
        body(22, URL_SAFE, 19)
    );
    assert!(detect_secrets(&format!("MAPBOX_PUBLIC={public}")).is_empty());
    assert!(detect_secrets(&format!(
        "sk.eyJ{}.{}",
        "your_mapbox_token_goes_here", "replace_me_here"
    ))
    .is_empty());
}

#[test]
fn aws_secret_access_key_on_a_secret_named_line_is_a_secret() {
    let p = planted(SecretShape::AwsSecretAccessKey);
    assert_redacted(&p.text, SecretShape::AwsSecretAccessKey, &p.value);
    let value = body(40, BASE64, 20);
    for text in [
        format!("AWS_SECRET_ACCESS_KEY={value}"),
        format!("aws_secret_access_key = \"{value}\""),
        format!("{{\"SecretAccessKey\": \"{value}\"}}"),
        format!("secret_key: '{value}'\n"),
    ] {
        let out = redact(&text).text;
        assert!(!out.contains(&value), "{}", text.replace(&value, "<v>"));
    }
    // Not on a `secret` line, or not exactly 40 characters.
    assert!(detect_secrets(&format!("session_id = {value}")).is_empty());
    // (A 39-char value is still a credential-named assignment: a named literal, not this shape.)
    let short = format!("secret_access_key = {}", body(39, BASE64, 21));
    assert_eq!(the_secret(&short).0, SecretShape::NamedSecretLiteral);
    // AWS's own documented example secret key is a placeholder (it ends in EXAMPLEKEY).
    let documented = ["wJalrXUtnFEMI/K7MDENG/bPxRfiCY", "EXAMPLEKEY"].concat();
    assert!(detect_secrets(&format!("aws_secret_access_key = {documented}")).is_empty());
}

#[test]
fn basic_auth_header_credential_is_a_secret_and_prose_is_not() {
    let p = planted(SecretShape::BasicAuthCredential);
    assert_redacted(&p.text, SecretShape::BasicAuthCredential, &p.value);
    let value = format!("{}=", body(30, ALNUM, 22));
    for text in [
        format!("curl -H \"Authorization: Basic {value}\" https://x.test"),
        format!("{{'Authorization': 'Basic {value}'}}"),
        format!("authorization=Basic {value}"),
    ] {
        assert!(
            !redact(&text).text.contains(&value),
            "{}",
            text.replace(&value, "<v>")
        );
    }
    assert!(detect_secrets("Authorization: Basic authentication is discouraged").is_empty());
    assert!(detect_secrets("Authorization: Basic ${CREDENTIALS}").is_empty());
}

#[test]
fn npmrc_auth_values_are_secrets_and_env_references_are_not() {
    let p = planted(SecretShape::NpmrcAuthToken);
    assert_redacted(&p.text, SecretShape::NpmrcAuthToken, &p.value);
    let value = body(30, ALNUM, 23);
    for text in [
        format!("_authToken={value}"),
        format!("//npm.pkg.github.com/:_authToken=\"{value}\""),
        format!("_password={value}"),
        format!("_auth = {value}"),
    ] {
        assert!(
            !redact(&text).text.contains(&value),
            "{}",
            text.replace(&value, "<v>")
        );
    }
    assert!(detect_secrets("//registry.npmjs.org/:_authToken=${NPM_TOKEN}").is_empty());
    assert!(detect_secrets("//registry.npmjs.org/:_authToken=YOUR_NPM_TOKEN_HERE").is_empty());
}

#[test]
fn docker_config_auth_entry_is_a_secret_and_a_short_word_is_not() {
    let p = planted(SecretShape::DockerAuthEntry);
    assert_redacted(&p.text, SecretShape::DockerAuthEntry, &p.value);
    let value = body(24, ALNUM, 24);
    let escaped = format!("{{\\\"auth\\\": \\\"{value}\\\"}}");
    assert!(!redact(&escaped).text.contains(&value));
    assert!(detect_secrets("{\"auth\": \"basic\"}").is_empty());
    assert!(detect_secrets("{\"auth\": \"oauth2\", \"user\": \"ci\"}").is_empty());
    assert!(detect_secrets("{\"auth\": \"\"}").is_empty());
}

#[test]
fn pgpass_line_password_is_a_secret_and_other_colon_lines_are_not() {
    let p = planted(SecretShape::PgPassPassword);
    assert_redacted(&p.text, SecretShape::PgPassPassword, &p.value);
    let value = body(20, ALNUM, 25);
    for text in [
        format!("localhost:5432:*:postgres:{value}"),
        ["*:*:appdb:appuser:", value.as_str(), "\n"].concat(),
        format!("  10.0.0.5:6432:analytics:etl:{value}"),
    ] {
        assert!(
            !redact(&text).text.contains(&value),
            "{}",
            text.replace(&value, "<v>")
        );
    }
    // /etc/passwd lines, a MAC address and a time are not pgpass entries.
    for other in [
        "root:x:0:0:root:/root:/bin/bash",
        "daemon:*:1:1:daemon:/usr/sbin:/usr/sbin/nologin",
        "00:11:22:33:44:55",
        "localhost:5432:appdb:appuser:<password>",
        "localhost:5432:appdb:appuser:[YOUR-PASSWORD]",
    ] {
        assert!(detect_secrets(other).is_empty(), "{other}");
    }
}
