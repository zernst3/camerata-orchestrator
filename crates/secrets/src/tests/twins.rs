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
fn twin_supabase_publishable_prefix_is_never_a_secret() {
    let text = format!(
        "NEXT_PUBLIC_SUPABASE_ANON_KEY=sb_publishable_{}",
        body(40, BODY_ALPHABET, 3)
    );
    assert!(detect_secrets(&text).is_empty());
    assert_eq!(redact(&text).text, text);
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
        "%(db_password)s",
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
fn a_bare_leading_template_character_is_not_a_template() {
    // Leak hunt T5: `$`, `%`, `[`, `<` and `{` at the start of a REAL password made the whole
    // value a "template". Only a CLOSED template is a twin.
    for lead in ["$", "%21", "[", "<", "{", "$DB_"] {
        let password = format!("{lead}{}", body(20, ACCOUNT_ALPHABET, 31));
        let dsn = format!("postgres://postgres:{password}@db.abcdefghij.supabase.co:5432/postgres");
        let found = detect_secrets(&dsn);
        assert_eq!(found.len(), 1, "dsn password starting with {lead}");
        assert_eq!(found[0].shape, SecretShape::ConnectionStringPassword);
        assert!(!redact(&dsn).text.contains(&password), "{lead}");
        // The release gate walks the JSON tree with the same detector.
        let tree = serde_json::json!({"detail": dsn});
        assert_eq!(crate::find_secrets_in_json(&tree).len(), 1, "gate, {lead}");
    }
}

#[test]
fn only_a_closed_template_is_a_template() {
    for template in [
        "${DB_PASSWORD}",
        "{{db_password}}",
        "<db-password>",
        "[YOUR-PASSWORD]",
        "%s",
        "%(db_password)s",
        "{db_password}",
    ] {
        let dsn = format!("postgres://postgres:{template}@localhost:5432/app");
        assert!(
            detect_secrets(&dsn).is_empty(),
            "closed template {template}"
        );
    }
    // Opened but never closed: a password that happens to start like a template.
    for unclosed in [
        "${DB_PASSWORD",
        "{{db_password",
        "<db-password",
        "[ABCD-EFGH",
        "%xyz",
        "%(a)",
        "{a-b}",
    ] {
        let dsn = format!("postgres://postgres:{unclosed}@localhost:5432/app");
        assert_eq!(detect_secrets(&dsn).len(), 1, "unclosed {unclosed}");
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
fn twin_path_like_literal_may_carry_an_extension_and_hyphens() {
    // The literal class admits `.` and `-` (UUID-shaped tokens), so file paths with an
    // extension or a hyphenated directory are in it; they are still paths.
    for value in [
        "src/some/very/long/path.rs",
        "app/api/auth-token/refresh.route.ts",
    ] {
        let text = format!("let token_path = \"{value}\";");
        assert_eq!(
            assert_twin(&text, SecretShape::NamedSecretLiteral),
            Twin::PathLike
        );
    }
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
fn azure_account_key_clause_alone_is_not_the_azure_shape() {
    // Without the protocol clause the Azure shape does not apply. A bare `AccountKey=<base64>`
    // line is still a credential-named assignment, so it is redacted as a named literal.
    let material = body(64, ACCOUNT_ALPHABET, 9);
    let text = format!("AccountKey={material}==");
    let matches = detect_all(&text);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].shape, SecretShape::NamedSecretLiteral);
    assert!(!redact(&text).text.contains(&material));
}

#[test]
fn not_a_twin_env_example_filename_does_not_change_detection() {
    // The detector has no path input: redaction is content-only. A template FILE NAME never
    // makes its contents readable (a real-looking live key in `.env.local.example` still leaks).
    let p = planted(SecretShape::StripeLiveSecretKey);
    let content = format!("# .env.local.example\n{}\n", p.text);
    assert!(!redact(&content).text.contains(&p.value));
}

#[test]
fn twin_readable_snake_case_literal_on_a_credential_named_identifier() {
    // Owner decision (e): lowercase snake_case, 2+ underscores, at most 2 digits.
    for value in [
        "app_preferences_sidebar_collapsed_v2",
        "user_session_storage_key_name",
        "feature_flag_dark_mode_enabled",
    ] {
        let text = format!("const storageKey = \"{value}\";");
        assert_eq!(
            assert_twin(&text, SecretShape::NamedSecretLiteral),
            Twin::ReadableName
        );
        let dotenv = format!("STORAGE_KEY={value}");
        assert_eq!(
            assert_twin(&dotenv, SecretShape::NamedSecretLiteral),
            Twin::ReadableName
        );
    }
}

#[test]
fn twin_readable_name_also_covers_kebab_dotted_and_env_reference_values() {
    // The literal class admits `-` and `.` (UUID-shaped tokens), so the same readability rule
    // must cover the identifiers those characters carry.
    for value in [
        "app-preferences-sidebar-collapsed",
        "auth.password.reset.email.subject",
        "service-account-credentials.json",
        "process.env.SUPABASE_SERVICE_ROLE_KEY",
        "secrets.GITHUB_TOKEN_ACTIONS_VALUE",
    ] {
        let text = format!("const tokenName = \"{value}\";");
        assert_eq!(
            assert_twin(&text, SecretShape::NamedSecretLiteral),
            Twin::ReadableName,
            "{value}"
        );
    }
}

#[test]
fn not_a_readable_name_when_it_is_not_all_words() {
    let lower = "abcdefghijklmnopqrstuvwxyz";
    let digits = "app_prefs_sidebar_9_8_7_collapsed_state".to_string();
    let one_underscore = format!("{}_{}", body(14, lower, 41), body(14, lower, 42));
    let mixed_case = body(28, &format!("{lower}{}", lower.to_ascii_uppercase()), 43);
    let hyphenated_mixed = format!(
        "{}-{}-{}",
        body(10, ACCOUNT_ALPHABET, 44),
        body(10, ACCOUNT_ALPHABET, 45),
        body(10, ACCOUNT_ALPHABET, 46)
    );
    for value in [digits, one_underscore, mixed_case, hyphenated_mixed] {
        let text = format!("const apiToken = \"{value}\";");
        assert_eq!(
            detect_secrets(&text).len(),
            1,
            "a non-readable value must be redacted"
        );
        assert!(!redact(&text).text.contains(&value));
    }
}

#[test]
fn twin_publishable_keys_after_bearer_stay_readable() {
    // Owner decision (e): public keys are public by design wherever they appear.
    for prefix in ["sb_publishable_", "pk_live_", "pk_test_"] {
        let key = format!("{prefix}{}", body(40, BODY_ALPHABET, 51));
        let header = format!("Authorization: Bearer {key}");
        assert_eq!(
            assert_twin(&header, SecretShape::BearerToken),
            Twin::PublishableKey,
            "{prefix}"
        );
        let env = format!("NEXT_PUBLIC_API_KEY={key}");
        assert_eq!(
            assert_twin(&env, SecretShape::NamedSecretLiteral),
            Twin::PublishableKey,
            "{prefix}"
        );
    }
    assert!(!Twin::PublishableKey.counts_as_line());
    // A secret key behind Bearer is still redacted.
    let secret = planted(SecretShape::StripeLiveSecretKey).value;
    assert_eq!(
        detect_secrets(&format!("Authorization: Bearer {secret}")).len(),
        1
    );
}

#[test]
fn a_placeholder_word_inside_a_mixed_case_vendor_body_is_not_a_twin() {
    // Owner decision (a): a vendor body counts a placeholder word only when it is DELIMITED
    // (`_`, `-`, start, end, a case change) or the body is single-case. A random Stripe key that
    // happens to contain "Mock" in the middle is still a secret.
    let key = ["sk_live_", "aB3dEfGhJkLmNoPQ", "Mock", "abcdEfGhJkLm"].concat();
    let text = format!("STRIPE_KEY={key}");
    let found = detect_secrets(&text);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].shape, SecretShape::StripeLiveSecretKey);
    assert!(!redact(&text).text.contains(&key));
    // Every alphanumeric placeholder word is subject to the same rule (the neighbours share the
    // word's case at its right edge, so it is not delimited there).
    for word in PLACEHOLDER_WORDS
        .iter()
        .filter(|w| w.chars().all(|c| c.is_ascii_alphanumeric()))
    {
        let embedded = ["sk_live_", "aB3dEfGhJkLmNoPQ", word, "abcdEfGhJkLm"].concat();
        assert_eq!(detect_secrets(&embedded).len(), 1, "undelimited {word}");
    }
}

#[test]
fn a_delimited_or_single_case_placeholder_word_is_still_a_twin() {
    for (key, shape) in [
        (
            "sk-ant-ABCDEFGH_your_KEYabcdefghij",
            SecretShape::AnthropicKey,
        ),
        (
            "sk-ant-abcdefgh-example-abcdefgh",
            SecretShape::AnthropicKey,
        ),
        (
            "sk_live_AbCdEfGhYourKeyHereAbCd",
            SecretShape::StripeLiveSecretKey,
        ),
        (
            "sk_live_yourlivesecretkeyhere",
            SecretShape::StripeLiveSecretKey,
        ),
        (
            "sk_live_YOURLIVESECRETKEYHERE12",
            SecretShape::StripeLiveSecretKey,
        ),
    ] {
        let twin = assert_twin(&format!("STRIPE_KEY={key}"), shape);
        assert!(matches!(twin, Twin::PlaceholderWord(_)), "{key}");
    }
}

#[test]
fn context_shapes_keep_substring_placeholder_matching() {
    let value = format!(
        "{}Mock{}",
        body(10, ACCOUNT_ALPHABET, 61),
        body(10, ACCOUNT_ALPHABET, 62)
    );
    let text = format!("const apiToken = \"{value}\";");
    assert!(matches!(
        assert_twin(&text, SecretShape::NamedSecretLiteral),
        Twin::PlaceholderWord(_)
    ));
}

#[test]
fn twin_published_azure_emulator_account_key() {
    // The well-known Azurite development key, assembled from fragments.
    let key = [
        "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1",
        "OUzFT50uSRZ6IFsuFq2UVErCz4I6tq==",
    ]
    .concat();
    let line = format!("AccountKey={key}");
    assert_eq!(
        assert_twin(&line, SecretShape::NamedSecretLiteral),
        Twin::PublishedExampleKey
    );
    let conn =
        format!("DefaultEndpointsProtocol=http;AccountName=devstoreaccount1;AccountKey={key};");
    assert_eq!(
        assert_twin(&conn, SecretShape::AzureStorageConnectionString),
        Twin::PublishedExampleKey
    );
}

#[test]
fn a_passphrase_on_a_password_like_identifier_is_never_a_readable_name() {
    // Diceware-style passphrases are words joined by `-` or `.`: they look exactly like a
    // readable name, but on a password-like identifier they are real secrets.
    let phrases = [
        "correct-horse-battery-staple",
        "correct.horse.battery.staple",
        "Correct-Horse-Battery-Staple",
    ];
    for word in [
        "password",
        "passwd",
        "pwd",
        "passphrase",
        "secret",
        "credential",
    ] {
        for ident in [
            word.to_string(),
            word.to_ascii_uppercase(),
            format!("db_{word}_value"),
        ] {
            for phrase in phrases {
                for text in [
                    format!("{ident}={phrase}"),
                    format!("{ident}: {phrase}"),
                    format!("const {ident} = \"{phrase}\";"),
                    format!("{{\"{ident}\": \"{phrase}\"}}"),
                ] {
                    let found = detect_secrets(&text);
                    assert_eq!(found.len(), 1, "not flagged: {text}");
                    assert!(!redact(&text).text.contains(phrase), "leaked: {text}");
                }
            }
        }
    }
}

#[test]
fn a_readable_name_on_a_code_constant_identifier_stays_readable() {
    for text in [
        "STORAGE_KEY = \"app-preferences-sidebar-state\"".to_string(),
        "STORAGE_KEY=app-preferences-sidebar-state".to_string(),
        "const queryKey = \"user.profile.settings.sidebar\";".to_string(),
        "key=app-preferences-sidebar-state".to_string(),
        "api_key_header: x-app-preferences-sidebar".to_string(),
    ] {
        assert_eq!(
            assert_twin(&text, SecretShape::NamedSecretLiteral),
            Twin::ReadableName,
            "{text}"
        );
    }
    // The owner's example is below the 24-char literal minimum, so it is not even a candidate.
    assert!(detect_secrets("STORAGE_KEY = \"app-preferences-sidebar\"").is_empty());
}

// ---- Firebase web `apiKey` (item 24): public by design, so a safe twin -----------------------

/// A runtime-built `AIza` key: the repository holds no 39-character literal.
fn google_key(seed: u64) -> String {
    format!(
        "AIza{}",
        body(
            35,
            "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-",
            seed
        )
    )
}

fn assert_firebase_twin(text: &str) {
    let twin = assert_twin(text, SecretShape::GoogleApiKey);
    assert_eq!(twin, Twin::FirebaseWebApiKey, "{text}");
}

fn assert_google_secret(text: &str, expected: &str) {
    let secrets = detect_secrets(text);
    assert_eq!(secrets.len(), 1, "expected one secret in: {text}");
    assert_eq!(secrets[0].shape, SecretShape::GoogleApiKey);
    assert_eq!(&text[secrets[0].value.clone()], expected);
}

fn js_config(key: &str) -> String {
    format!(
        "const firebaseConfig = {{\n  apiKey: \"{key}\",\n  authDomain: \"x.firebaseapp.com\",\n  projectId: \"x\",\n  appId: \"1:2:web:3\"\n}};\n"
    )
}

#[test]
fn twin_firebase_web_config_api_key_in_a_js_object_stays_readable() {
    assert_firebase_twin(&js_config(&google_key(101)));
}

#[test]
fn twin_firebase_api_key_inside_initialize_app_call_stays_readable() {
    let key = google_key(102);
    assert_firebase_twin(&format!(
        "const app = initializeApp({{ apiKey: \"{key}\", projectId: \"x\", messagingSenderId: \"1\" }});"
    ));
}

#[test]
fn twin_firebase_api_key_in_firebase_options_dart_stays_readable() {
    let key = google_key(103);
    assert_firebase_twin(&format!(
        "static const FirebaseOptions web = FirebaseOptions(\n  apiKey: '{key}',\n  appId: '1:2:web:3',\n  messagingSenderId: '1',\n  projectId: 'x',\n);\n"
    ));
}

#[test]
fn twin_firebase_api_key_in_google_services_json_stays_readable() {
    let key = google_key(104);
    assert_firebase_twin(&format!(
        "{{\n  \"project_info\": {{ \"project_id\": \"x\", \"storage_bucket\": \"x.appspot.com\" }},\n  \"client\": [{{\n    \"client_info\": {{ \"mobilesdk_app_id\": \"1:2:android:3\" }},\n    \"api_key\": [{{ \"current_key\": \"{key}\" }}]\n  }}]\n}}\n"
    ));
}

#[test]
fn twin_firebase_api_key_in_google_service_info_plist_stays_readable() {
    let key = google_key(105);
    assert_firebase_twin(&format!(
        "<dict>\n\t<key>API_KEY</key>\n\t<string>{key}</string>\n\t<key>GOOGLE_APP_ID</key>\n\t<string>1:2:ios:3</string>\n\t<key>PROJECT_ID</key>\n\t<string>x</string>\n</dict>\n"
    ));
}

#[test]
fn twin_firebase_named_env_var_stays_readable() {
    assert_firebase_twin(&format!("VITE_FIREBASE_API_KEY={}", google_key(106)));
    assert_firebase_twin(&format!(
        "{{ \"EXPO_PUBLIC_FIREBASE_API_KEY\": \"{}\" }}",
        google_key(107)
    ));
}

#[test]
fn twin_firebase_api_key_is_not_a_floor_row() {
    let text = js_config(&google_key(108));
    for rule in ["SEC-NO-VENDOR-TOKEN-1", "SEC-NO-HARDCODED-SECRETS-1"] {
        assert!(
            crate::match_lines_for_rule(&text, rule, true).is_empty(),
            "{rule}"
        );
    }
}

#[test]
fn not_a_twin_google_key_without_firebase_context_is_a_secret() {
    let key = google_key(109);
    assert_google_secret(&format!("const key = \"{key}\";"), &key);
    // A key-shaped name alone is not Firebase context.
    assert_google_secret(&format!("const cfg = {{ apiKey: \"{key}\" }};"), &key);
}

#[test]
fn not_a_twin_maps_key_in_its_own_object_beside_a_firebase_config_is_a_secret() {
    let firebase = google_key(110);
    let maps = google_key(111);
    let text = format!(
        "{}const loader = new Loader({{ apiKey: \"{maps}\", version: \"weekly\" }});\n",
        js_config(&firebase)
    );
    let secrets = detect_secrets(&text);
    assert_eq!(secrets.len(), 1, "exactly the Maps key is a secret");
    assert_eq!(&text[secrets[0].value.clone()], maps);
    assert_eq!(detect_all(&text).len(), 2);
}

#[test]
fn not_a_twin_env_maps_key_beside_a_firebase_env_key_is_a_secret() {
    let firebase = google_key(112);
    let maps = google_key(113);
    let text = format!("VITE_FIREBASE_API_KEY={firebase}\nVITE_GOOGLE_MAPS_KEY={maps}\n");
    assert_google_secret(&text, &maps);
}

#[test]
fn not_a_twin_firebase_siblings_outside_the_window_do_not_count() {
    let key = google_key(114);
    let padding = "// padding\n".repeat(200);
    let text = format!(
        "const c = {{ apiKey: \"{key}\" }};\n{padding}const firebaseConfig = {{ authDomain: \"x\", projectId: \"x\" }};\n"
    );
    assert_google_secret(&text, &key);
}

#[test]
fn not_a_twin_stripe_key_inside_a_firebase_config_object_is_still_a_secret() {
    let planted = planted(SecretShape::StripeLiveSecretKey);
    let text = format!(
        "const firebaseConfig = {{\n  apiKey: \"{}\",\n  authDomain: \"x.firebaseapp.com\",\n  projectId: \"x\"\n}};\n",
        planted.value
    );
    let secrets = detect_secrets(&text);
    assert_eq!(secrets.len(), 1);
    assert_eq!(secrets[0].shape, SecretShape::StripeLiveSecretKey);
}

#[test]
fn not_a_twin_a_longer_key_name_is_not_a_firebase_config_key() {
    let key = google_key(115);
    // `GOOGLE_MAPS_API_KEY` and `googleMapsApiKey` end like a config key but are Maps keys, even
    // with project-looking siblings beside them.
    assert_google_secret(
        &format!("PROJECT_ID=x\nSTORAGE_BUCKET=y\nGOOGLE_MAPS_API_KEY={key}\n"),
        &key,
    );
    assert_google_secret(
        &format!("const c = {{ projectId: \"x\", appId: \"y\", googleMapsApiKey: \"{key}\" }};"),
        &key,
    );
}
