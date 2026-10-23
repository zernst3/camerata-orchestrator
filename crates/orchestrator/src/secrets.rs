//! Secret detector + scrubber (FOLD E — the chat secret-interceptor,
//! `docs/plans/2026-07-09_product-owner-head-vibe-mode.md`'s usability backlog:
//! "chat secret-interceptor (scrub pasted keys from transcript/memory/audit)").
//!
//! A secret pasted into a chat (or captured in a deployed app's own logs) must
//! never land in the transcript, session memory, or the governance audit trail.
//! This module is the pure substrate: [`detect_secrets`] finds secret matches in
//! free text; [`scrub`] replaces each match with a stable placeholder token and
//! returns the extracted values so a caller can store them in a vault BY NAME
//! instead of just discarding them.
//!
//! # A thin adapter over `camerata-secrets`
//! The patterns, the safe twins (a placeholder such as `sb_secret_your_key_here` is not a
//! secret) and the overlap rules all live in the shared leaf crate
//! [`camerata_secrets`] (docs/design/2026-10-23_secret-redaction-and-release-gate.md), the same
//! detector the gateway SEC arms, the scan floor and the export release gate read. This module
//! owns only the vault-flow shape: the [`SecretKind`] labels, the `{{SECRET:<kind>_<n>}}`
//! placeholders and the extracted values. (It replaced seven private regexes that had drifted
//! from the gateway's.)
//!
//! # Applied today
//! The one live free-text ingest path this module is wired into now is
//! `camerata_server::submit_feedback`: `DefectContext.console` and
//! `DefectContext.stack` are scrubbed BEFORE the `DefectReport` is stored, so a
//! deployed scaffolded app's captured logs can't leak a secret into the feedback
//! store or the governance trail (see that function's call to
//! `camerata_orchestrator_core::secrets::scrub`).
//!
//! # Follow-on (not built here)
//! The chat/PO head's paste-interception ("you just pasted something that looks
//! like a secret — store it in the vault as `<name>` instead?") reuses
//! `detect_secrets`/`scrub` directly; this module is written to be that seam
//! already, not just an ingest-time filter.

use std::collections::HashMap;

use camerata_secrets::{SecretShape, Twin};

// ─── kinds ──────────────────────────────────────────────────────────────────────

/// Which well-known secret shape a [`SecretMatch`]/[`ExtractedSecret`] is. Drives
/// the placeholder token's label (`{{SECRET:<label>_<n>}}`) and doubles as a
/// vault-entry-name hint for the (future) chat-head "store this by name" flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretKind {
    /// Stripe secret/restricted key (`sk_live_`, `sk_test_`, `rk_live_`, `rk_test_`).
    StripeKey,
    /// A Stripe webhook signing secret (`whsec_`).
    StripeWebhookSecret,
    /// A Supabase secret key (`sb_secret_`) or service-role JWT.
    SupabaseKey,
    /// Any other JWT (an opaque bearer credential).
    JwtToken,
    /// A PEM/armored private-key block (`-----BEGIN ... PRIVATE KEY-----`).
    PemPrivateKey,
    /// An AWS access key ID (`AKIA`/`ASIA` + 16 uppercase alphanumeric chars).
    AwsAccessKey,
    /// A GitHub personal access token (`ghp_...`) or fine-grained token
    /// (`github_pat_...`).
    GitHubToken,
    /// A Slack token (`xoxb-...`).
    SlackToken,
    /// A Google API key (`AIza...`).
    GoogleApiKey,
    /// An Anthropic API key (`sk-ant-...`).
    AnthropicKey,
    /// An Azure Storage connection string (`DefaultEndpointsProtocol=...;
    /// AccountKey=...`).
    AzureStorageConnectionString,
    /// An OpenAI API key (`sk-` + a long alphanumeric run).
    OpenAiKey,
    /// A generic `Bearer <token>` in an obvious auth context (the token itself is
    /// long/opaque enough to be confident it's a real credential, not a stray
    /// word after "Bearer").
    BearerToken,
    /// The password inside a `scheme://user:password@host` connection string.
    ConnectionStringPassword,
    /// A secret-named URL query parameter's value (`?token=...`).
    UrlQuerySecret,
    /// A long literal (quoted or an unquoted `.env`/YAML value) assigned to a credential-named
    /// identifier, or a credential file entry (`.npmrc`, docker `config.json`, `.pgpass`, a
    /// `Basic` header).
    HardcodedCredential,
    /// A token from a vendor not named above (SendGrid, Twilio, Shopify, Mailgun, npm, Mapbox,
    /// Discord).
    VendorToken,
}

impl SecretKind {
    /// Stable lowercase snake_case label used in the placeholder token and as the
    /// vault-entry-name hint.
    pub fn label(self) -> &'static str {
        match self {
            SecretKind::StripeKey => "stripe_key",
            SecretKind::StripeWebhookSecret => "stripe_webhook_secret",
            SecretKind::SupabaseKey => "supabase_key",
            SecretKind::JwtToken => "jwt_token",
            SecretKind::PemPrivateKey => "pem_private_key",
            SecretKind::AwsAccessKey => "aws_access_key",
            SecretKind::GitHubToken => "github_token",
            SecretKind::SlackToken => "slack_token",
            SecretKind::GoogleApiKey => "google_api_key",
            SecretKind::AnthropicKey => "anthropic_key",
            SecretKind::AzureStorageConnectionString => "azure_storage_connection_string",
            SecretKind::OpenAiKey => "openai_key",
            SecretKind::BearerToken => "bearer_token",
            SecretKind::ConnectionStringPassword => "connection_string_password",
            SecretKind::UrlQuerySecret => "url_query_secret",
            SecretKind::HardcodedCredential => "hardcoded_credential",
            SecretKind::VendorToken => "vendor_token",
        }
    }
}

impl From<SecretShape> for SecretKind {
    fn from(shape: SecretShape) -> Self {
        match shape {
            SecretShape::SupabaseSecretKey | SecretShape::SupabaseServiceRoleJwt => {
                SecretKind::SupabaseKey
            }
            SecretShape::OpaqueJwt => SecretKind::JwtToken,
            SecretShape::StripeLiveSecretKey | SecretShape::StripeTestSecretKey => {
                SecretKind::StripeKey
            }
            SecretShape::StripeWebhookSecret => SecretKind::StripeWebhookSecret,
            SecretShape::AwsAccessKeyId => SecretKind::AwsAccessKey,
            SecretShape::GitHubToken => SecretKind::GitHubToken,
            SecretShape::SlackToken => SecretKind::SlackToken,
            SecretShape::GoogleApiKey => SecretKind::GoogleApiKey,
            SecretShape::AnthropicKey => SecretKind::AnthropicKey,
            SecretShape::OpenAiKey => SecretKind::OpenAiKey,
            SecretShape::PemPrivateKey => SecretKind::PemPrivateKey,
            SecretShape::SendGridApiKey
            | SecretShape::TwilioApiKey
            | SecretShape::ShopifyToken
            | SecretShape::MailgunApiKey
            | SecretShape::NpmToken
            | SecretShape::MapboxSecretToken
            | SecretShape::DiscordBotToken => SecretKind::VendorToken,
            SecretShape::NamedSecretLiteral
            | SecretShape::AwsSecretAccessKey
            | SecretShape::TwilioAuthToken
            | SecretShape::NpmrcAuthToken
            | SecretShape::DockerAuthEntry
            | SecretShape::PgPassPassword
            | SecretShape::BasicAuthCredential => SecretKind::HardcodedCredential,
            SecretShape::BearerToken => SecretKind::BearerToken,
            SecretShape::ConnectionStringPassword => SecretKind::ConnectionStringPassword,
            SecretShape::AzureStorageConnectionString => SecretKind::AzureStorageConnectionString,
            SecretShape::SecretInUrlQuery => SecretKind::UrlQuerySecret,
        }
    }
}

// ─── detect_secrets ─────────────────────────────────────────────────────────────

/// One secret match found by [`detect_secrets`]: `text[start..end]` is the span
/// [`scrub`] replaces (the secret VALUE: for a connection string only the password, for a
/// `Bearer` header only the token; a PEM block is replaced whole, header to footer), `kind`
/// says which shape matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretMatch {
    pub start: usize,
    pub end: usize,
    pub kind: SecretKind,
}

/// Whether a shared-detector match is something the chat scrubber removes: a secret, or a bare
/// PEM header (a truncated paste still carries the header and the user is about to paste the body).
fn is_scrubbable(m: &camerata_secrets::SecretMatch) -> bool {
    m.is_secret() || matches!(m.twin, Some(Twin::NoKeyMaterial))
}

/// Find every secret in `text`, in left-to-right order. Safe twins (placeholders, the public
/// anon key, Stripe's published documentation key) are not secrets and are left readable.
pub fn detect_secrets(text: &str) -> Vec<SecretMatch> {
    camerata_secrets::detect_all(text)
        .into_iter()
        .filter(is_scrubbable)
        .map(|m| {
            // A PEM block is scrubbed whole (header, body and footer), as before; every other
            // shape replaces just its value.
            let span = if m.shape == SecretShape::PemPrivateKey {
                m.start..m.end
            } else {
                m.value.clone()
            };
            SecretMatch {
                start: span.start,
                end: span.end,
                kind: m.shape.into(),
            }
        })
        .collect()
}

// ─── scrub ──────────────────────────────────────────────────────────────────────

/// One secret [`scrub`] pulled out of the text, paired with the placeholder token
/// that replaced it in the scrubbed output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedSecret {
    /// The exact placeholder token substituted into the scrubbed text (e.g.
    /// `{{SECRET:stripe_key_1}}`) — unique within one [`scrub`] call (a running
    /// per-kind counter disambiguates repeats of the same kind), so a caller can
    /// map each extracted value back to its exact substitution site.
    pub placeholder: String,
    /// The raw secret value that was removed.
    pub value: String,
    pub kind: SecretKind,
}

/// The result of [`scrub`]: `text` with every secret replaced by a stable
/// placeholder, plus the extracted values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrubResult {
    pub scrubbed: String,
    pub extracted: Vec<ExtractedSecret>,
}

/// Replace every [`detect_secrets`] match in `text` with a stable placeholder
/// token (`{{SECRET:<kind>_<n>}}`), returning the scrubbed text plus the extracted
/// raw values so a caller can store them in a vault by name rather than just
/// discarding them.
///
/// Idempotent: a placeholder token itself matches none of the patterns, so
/// scrubbing already-scrubbed text is a no-op (`scrub(&scrub(text).scrubbed)`
/// yields the same text back with an empty `extracted`).
pub fn scrub(text: &str) -> ScrubResult {
    let matches = detect_secrets(text);
    if matches.is_empty() {
        return ScrubResult {
            scrubbed: text.to_string(),
            extracted: Vec::new(),
        };
    }

    let mut scrubbed = String::with_capacity(text.len());
    let mut extracted = Vec::with_capacity(matches.len());
    let mut seen_of_kind: HashMap<&'static str, usize> = HashMap::new();
    let mut cursor = 0usize;

    for m in &matches {
        scrubbed.push_str(&text[cursor..m.start]);
        let label = m.kind.label();
        let n = seen_of_kind.entry(label).or_insert(0);
        *n += 1;
        let placeholder = format!("{{{{SECRET:{label}_{n}}}}}");
        extracted.push(ExtractedSecret {
            placeholder: placeholder.clone(),
            value: text[m.start..m.end].to_string(),
            kind: m.kind,
        });
        scrubbed.push_str(&placeholder);
        cursor = m.end;
    }
    scrubbed.push_str(&text[cursor..]);

    ScrubResult {
        scrubbed,
        extracted,
    }
}

// ─── tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── each pattern is detected ────────────────────────────────────────────

    // Build a Stripe-shaped fixture at runtime from fragments so no literal
    // secret-shaped token sits in the source. Secret scanners (e.g. GitHub push
    // protection) match the `sk_`/`rk_` prefix by SHAPE regardless of the body, so
    // de-literalized construction is the only way to keep the detector's own tests
    // from tripping them. `prefix` is e.g. "sk_live" (no trailing `_<body>`, so the
    // literal fragment is itself not secret-shaped); the `_` + body are appended here.
    //
    // The body comes from the shared fixtures: it must NOT be a placeholder word (a body
    // containing `EXAMPLE` is a safe twin the detector deliberately leaves readable).
    fn stripe(prefix: &str) -> String {
        format!(
            "{prefix}_{}",
            camerata_secrets::fixtures::body(28, ALNUM, 41)
        )
    }

    const ALNUM: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

    fn fixture(shape: camerata_secrets::SecretShape) -> String {
        camerata_secrets::fixtures::planted(shape).value
    }

    #[test]
    fn detects_stripe_live_and_test_and_restricted_keys() {
        for prefix in ["sk_live", "sk_test", "rk_live"] {
            let sample = stripe(prefix);
            let matches = detect_secrets(&sample);
            assert_eq!(matches.len(), 1, "sample: {sample}");
            assert_eq!(matches[0].kind, SecretKind::StripeKey);
            assert_eq!(&sample[matches[0].start..matches[0].end], sample);
        }
    }

    #[test]
    fn detects_pem_private_key_header_only() {
        let sample =
            "here is a key:\n-----BEGIN RSA PRIVATE KEY-----\nMIIEow...(truncated, no footer)";
        let matches = detect_secrets(sample);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].kind, SecretKind::PemPrivateKey);
    }

    #[test]
    fn detects_full_pem_private_key_block_including_body() {
        let sample = "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEA\nAoIBAQDEXAMPLEBODYONLY\n-----END PRIVATE KEY-----";
        let matches = detect_secrets(sample);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].kind, SecretKind::PemPrivateKey);
        let matched = &sample[matches[0].start..matches[0].end];
        assert!(matched.contains("-----BEGIN PRIVATE KEY-----"));
        assert!(matched.contains("-----END PRIVATE KEY-----"));
        assert!(matched.contains("EXAMPLEBODYONLY"));
    }

    #[test]
    fn detects_aws_access_key() {
        let key = fixture(camerata_secrets::SecretShape::AwsAccessKeyId);
        let sample = format!("aws_access_key_id = {key}");
        let matches = detect_secrets(&sample);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].kind, SecretKind::AwsAccessKey);
        assert_eq!(&sample[matches[0].start..matches[0].end], key);
    }

    #[test]
    fn detects_github_personal_access_token_and_fine_grained_token() {
        let ghp = format!(
            "token: {}",
            fixture(camerata_secrets::SecretShape::GitHubToken)
        );
        let matches = detect_secrets(&ghp);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].kind, SecretKind::GitHubToken);

        let pat = "token: github_pat_11ABCDEFG0123456789abcdefghijklmnopqrstuvwxyz";
        let matches = detect_secrets(pat);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].kind, SecretKind::GitHubToken);
    }

    #[test]
    fn detects_azure_storage_connection_string() {
        let sample = "DefaultEndpointsProtocol=https;AccountName=devstoreaccount1;AccountKey=Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==;EndpointSuffix=core.windows.net";
        let matches = detect_secrets(sample);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].kind, SecretKind::AzureStorageConnectionString);
        // Only the account KEY is replaced; the protocol and account name stay readable.
        let matched = &sample[matches[0].start..matches[0].end];
        assert!(matched.starts_with("Eby8vdM02xNO"));
        assert!(sample[..matches[0].start].ends_with("AccountKey="));
    }

    #[test]
    fn detects_openai_key() {
        let sample = "OPENAI_API_KEY=sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let matches = detect_secrets(sample);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].kind, SecretKind::OpenAiKey);
    }

    #[test]
    fn detects_generic_bearer_token_in_auth_context() {
        let token = fixture(camerata_secrets::SecretShape::BearerToken);
        let sample = format!("Authorization: Bearer {token}");
        let matches = detect_secrets(&sample);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].kind, SecretKind::BearerToken);
        // Only the token is replaced, not the word `Bearer`.
        assert_eq!(&sample[matches[0].start..matches[0].end], token);
    }

    #[test]
    fn a_bearer_jwt_is_reported_once_as_the_jwt() {
        let jwt = camerata_secrets::fixtures::jwt(None);
        let sample = format!("Authorization: Bearer {jwt}");
        let matches = detect_secrets(&sample);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].kind, SecretKind::JwtToken);
    }

    #[test]
    fn bearer_wrapping_a_specific_token_is_reported_once_as_the_specific_kind() {
        let sample = format!(
            "Authorization: Bearer {}",
            fixture(camerata_secrets::SecretShape::GitHubToken)
        );
        let matches = detect_secrets(&sample);
        assert_eq!(matches.len(), 1, "matches: {matches:?}");
        assert_eq!(matches[0].kind, SecretKind::GitHubToken);
    }

    // ── non-secrets are NOT flagged (precision) ─────────────────────────────

    #[test]
    fn ordinary_prose_is_never_flagged() {
        let sample = "The API key rotation policy says to rotate secrets every 90 days. \
            Please update your token before the deadline. Bearer bonds are a kind of \
            debt instrument, unrelated to HTTP auth.";
        assert!(
            detect_secrets(sample).is_empty(),
            "false positive on ordinary prose"
        );
    }

    #[test]
    fn short_aws_looking_prefix_is_not_flagged() {
        assert!(detect_secrets("AKIA1234").is_empty());
        assert!(detect_secrets("this AKIAMENTIONS bird migration").is_empty());
    }

    #[test]
    fn short_stripe_and_openai_prefixes_are_not_flagged() {
        assert!(detect_secrets("sk_live_short").is_empty());
        assert!(detect_secrets("sk-short").is_empty());
    }

    #[test]
    fn bare_bearer_word_with_no_token_is_not_flagged() {
        assert!(detect_secrets("Bearer").is_empty());
        assert!(detect_secrets("Bearer 123").is_empty());
        assert!(detect_secrets("the bearer of this letter").is_empty());
    }

    #[test]
    fn short_github_prefix_is_not_flagged() {
        assert!(detect_secrets("ghp_abc").is_empty());
    }

    #[test]
    fn azure_account_key_clause_alone_is_a_hardcoded_credential_not_the_azure_shape() {
        // Without the protocol clause it is a credential-named assignment.
        let key = format!("{}==", camerata_secrets::fixtures::body(64, ALNUM, 52));
        let found = detect_secrets(&format!("AccountKey={key}"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, SecretKind::HardcodedCredential);
    }

    #[test]
    fn pem_like_text_missing_the_begin_marker_is_not_flagged() {
        assert!(detect_secrets("this is definitely PRIVATE KEY material, trust me").is_empty());
    }

    // ── scrub: replaces + extracts ──────────────────────────────────────────

    #[test]
    fn scrub_replaces_a_single_secret_and_extracts_its_value() {
        let secret = stripe("sk_live");
        let text = format!("leaked: {secret} in the console dump");
        let result = scrub(&text);

        assert!(!result.scrubbed.contains(&secret));
        assert!(result.scrubbed.contains("{{SECRET:stripe_key_1}}"));
        assert_eq!(
            result.scrubbed,
            "leaked: {{SECRET:stripe_key_1}} in the console dump"
        );

        assert_eq!(result.extracted.len(), 1);
        assert_eq!(result.extracted[0].kind, SecretKind::StripeKey);
        assert_eq!(result.extracted[0].value, secret);
        assert_eq!(result.extracted[0].placeholder, "{{SECRET:stripe_key_1}}");
    }

    #[test]
    fn scrub_handles_multiple_secrets_of_different_kinds() {
        let aws = fixture(camerata_secrets::SecretShape::AwsAccessKeyId);
        let ghp = fixture(camerata_secrets::SecretShape::GitHubToken);
        let text = format!("aws={aws} github={ghp}");
        let result = scrub(&text);

        assert_eq!(result.extracted.len(), 2);
        assert!(!result.scrubbed.contains(&aws));
        assert!(!result.scrubbed.contains(&ghp));
        assert!(result.scrubbed.contains("{{SECRET:aws_access_key_1}}"));
        assert!(result.scrubbed.contains("{{SECRET:github_token_1}}"));
    }

    #[test]
    fn scrub_numbers_repeats_of_the_same_kind_distinctly() {
        let text = format!("first {} then {}", stripe("sk_live"), stripe("sk_test"));
        let result = scrub(&text);

        assert_eq!(result.extracted.len(), 2);
        assert_eq!(result.extracted[0].placeholder, "{{SECRET:stripe_key_1}}");
        assert_eq!(result.extracted[1].placeholder, "{{SECRET:stripe_key_2}}");
        assert_ne!(result.extracted[0].value, result.extracted[1].value);
    }

    #[test]
    fn scrub_is_a_no_op_when_no_secret_is_present() {
        let text = "nothing sensitive here, just a normal console log line";
        let result = scrub(text);
        assert_eq!(result.scrubbed, text);
        assert!(result.extracted.is_empty());
    }

    #[test]
    fn scrub_handles_empty_text() {
        let result = scrub("");
        assert_eq!(result.scrubbed, "");
        assert!(result.extracted.is_empty());
    }

    // ── idempotence ──────────────────────────────────────────────────────────

    #[test]
    fn scrubbing_already_scrubbed_text_is_idempotent() {
        let text = format!("key: {} and AKIAIOSFODNN7EXAMPLE", stripe("sk_live"));
        let once = scrub(&text);
        let twice = scrub(&once.scrubbed);

        assert_eq!(twice.scrubbed, once.scrubbed);
        assert!(
            twice.extracted.is_empty(),
            "re-scrubbing already-placeholdered text must find nothing new"
        );
    }

    // ── the shared detector's wider shapes (docs/design/2026-10-23) ─────────

    #[test]
    fn scrub_covers_the_shapes_the_old_seven_regexes_lacked() {
        use camerata_secrets::fixtures::planted;
        use camerata_secrets::SecretShape;
        for (shape, kind) in [
            (SecretShape::SupabaseSecretKey, SecretKind::SupabaseKey),
            (SecretShape::SupabaseServiceRoleJwt, SecretKind::SupabaseKey),
            (SecretShape::SlackToken, SecretKind::SlackToken),
            (SecretShape::GoogleApiKey, SecretKind::GoogleApiKey),
            (SecretShape::AnthropicKey, SecretKind::AnthropicKey),
            (
                SecretShape::StripeWebhookSecret,
                SecretKind::StripeWebhookSecret,
            ),
            (
                SecretShape::ConnectionStringPassword,
                SecretKind::ConnectionStringPassword,
            ),
            (SecretShape::SecretInUrlQuery, SecretKind::UrlQuerySecret),
            (
                SecretShape::NamedSecretLiteral,
                SecretKind::HardcodedCredential,
            ),
            (SecretShape::SendGridApiKey, SecretKind::VendorToken),
            (
                SecretShape::AwsSecretAccessKey,
                SecretKind::HardcodedCredential,
            ),
        ] {
            let p = planted(shape);
            let result = scrub(&p.text);
            assert_eq!(result.extracted.len(), 1, "{shape:?}");
            assert_eq!(result.extracted[0].kind, kind, "{shape:?}");
            assert_eq!(result.extracted[0].value, p.value, "{shape:?}");
            assert!(!result.scrubbed.contains(&p.value), "{shape:?}");
        }
    }

    #[test]
    fn scrub_replaces_only_the_value_of_a_context_shape() {
        use camerata_secrets::fixtures::planted;
        use camerata_secrets::SecretShape;
        let p = planted(SecretShape::ConnectionStringPassword);
        let result = scrub(&p.text);
        assert!(result
            .scrubbed
            .contains("postgres://postgres:{{SECRET:connection_string_password_1}}@db."));
    }

    #[test]
    fn scrub_leaves_a_placeholder_alone() {
        let text = "SUPABASE_KEY=sb_secret_your_key_here and AKIAIOSFODNN7EXAMPLE";
        assert_eq!(scrub(text).scrubbed, text);
    }

    // ── SecretKind::label ────────────────────────────────────────────────────

    #[test]
    fn every_kind_has_a_stable_lowercase_label() {
        for (kind, expected) in [
            (SecretKind::StripeKey, "stripe_key"),
            (SecretKind::PemPrivateKey, "pem_private_key"),
            (SecretKind::VendorToken, "vendor_token"),
            (SecretKind::AwsAccessKey, "aws_access_key"),
            (SecretKind::GitHubToken, "github_token"),
            (
                SecretKind::AzureStorageConnectionString,
                "azure_storage_connection_string",
            ),
            (SecretKind::OpenAiKey, "openai_key"),
            (SecretKind::BearerToken, "bearer_token"),
        ] {
            assert_eq!(kind.label(), expected);
        }
    }
}
