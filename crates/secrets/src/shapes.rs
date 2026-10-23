//! The shape catalogue (spec section 2.2): one `SecretShape` per secret form the shared
//! detector knows, its exact pattern, and the corpus-rule mapping.

use std::sync::OnceLock;

use regex::Regex;

/// Every secret shape the detector knows. Variant order is the match-priority order used to
/// break ties between equal-span overlapping matches (lower variant first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SecretShape {
    /// S1: `sb_secret_` + opaque body.
    SupabaseSecretKey,
    /// S2: a JWT whose decoded `role` claim is `service_role`.
    SupabaseServiceRoleJwt,
    /// S2b: any other JWT (no `role`, an undecodable payload, or a non-service role).
    OpaqueJwt,
    /// S3: `sk_live_` / `rk_live_`.
    StripeLiveSecretKey,
    /// S4: `sk_test_` / `rk_test_`.
    StripeTestSecretKey,
    /// S5: `whsec_`.
    StripeWebhookSecret,
    /// S6: `AKIA` / `ASIA` access key id.
    AwsAccessKeyId,
    /// S7: `ghp_` family and `github_pat_`.
    GitHubToken,
    /// S8: `xox?-` Slack tokens.
    SlackToken,
    /// S9: `AIza` Google API key.
    GoogleApiKey,
    /// S10: `sk-ant-`.
    AnthropicKey,
    /// S11: `sk-` / `sk-proj-`.
    OpenAiKey,
    /// S12: a PEM private key block.
    PemPrivateKey,
    /// S13: a long quoted literal assigned to a credential-named identifier.
    NamedSecretLiteral,
    /// S14: `Bearer <token>`.
    BearerToken,
    /// S15: the password inside a `scheme://user:password@host` connection string.
    ConnectionStringPassword,
    /// S16: the `AccountKey=` of an Azure storage connection string.
    AzureStorageConnectionString,
    /// S17: a secret-named query parameter's value.
    SecretInUrlQuery,
}

/// Labels, indexed by [`SecretShape::rank`] (the order of [`SecretShape::ALL`]). A table rather
/// than a `match` so no line pairs a credential-named variant with a long snake_case literal,
/// which the named-literal heuristic (rightly) cannot tell from a hardcoded secret.
const LABELS: [&str; 18] = [
    "supabase_secret_key",
    "supabase_service_role_jwt",
    "opaque_jwt",
    "stripe_live_secret_key",
    "stripe_test_secret_key",
    "stripe_webhook_secret",
    "aws_access_key_id",
    "github_token",
    "slack_token",
    "google_api_key",
    "anthropic_key",
    "openai_key",
    "pem_private_key",
    "named_secret_literal",
    "bearer_token",
    "connection_string_password",
    "azure_storage_connection_string",
    "secret_in_url_query",
];

impl SecretShape {
    /// All shapes, in match-priority order.
    pub const ALL: &'static [SecretShape] = &[
        SecretShape::SupabaseSecretKey,
        SecretShape::SupabaseServiceRoleJwt,
        SecretShape::OpaqueJwt,
        SecretShape::StripeLiveSecretKey,
        SecretShape::StripeTestSecretKey,
        SecretShape::StripeWebhookSecret,
        SecretShape::AwsAccessKeyId,
        SecretShape::GitHubToken,
        SecretShape::SlackToken,
        SecretShape::GoogleApiKey,
        SecretShape::AnthropicKey,
        SecretShape::OpenAiKey,
        SecretShape::PemPrivateKey,
        SecretShape::NamedSecretLiteral,
        SecretShape::BearerToken,
        SecretShape::ConnectionStringPassword,
        SecretShape::AzureStorageConnectionString,
        SecretShape::SecretInUrlQuery,
    ];

    /// Stable snake_case label for messages and JSON.
    pub fn label(self) -> &'static str {
        LABELS[self.rank()]
    }

    /// Client-facing noun for the release gate's message.
    pub fn display_name(self) -> &'static str {
        match self {
            SecretShape::SupabaseSecretKey => "Supabase secret key",
            SecretShape::SupabaseServiceRoleJwt => "Supabase service-role key",
            SecretShape::OpaqueJwt => "JWT",
            SecretShape::StripeLiveSecretKey => "Stripe live secret key",
            SecretShape::StripeTestSecretKey => "Stripe test secret key",
            SecretShape::StripeWebhookSecret => "Stripe webhook secret",
            SecretShape::AwsAccessKeyId => "AWS access key id",
            SecretShape::GitHubToken => "GitHub token",
            SecretShape::SlackToken => "Slack token",
            SecretShape::GoogleApiKey => "Google API key",
            SecretShape::AnthropicKey => "Anthropic API key",
            SecretShape::OpenAiKey => "OpenAI API key",
            SecretShape::PemPrivateKey => "private key",
            SecretShape::NamedSecretLiteral => "hardcoded credential",
            SecretShape::BearerToken => "bearer token",
            SecretShape::ConnectionStringPassword => "connection-string password",
            SecretShape::AzureStorageConnectionString => "Azure storage account key",
            SecretShape::SecretInUrlQuery => "secret in a URL query",
        }
    }

    /// The canonical fixed prefix kept in the redacted form, if the shape has one. Shapes with
    /// several accepted prefixes (`sk_live_`/`rk_live_`, `AKIA`/`ASIA`, ...) name the first;
    /// the redactor keeps the prefix the value actually carries (see [`prefix_of`]).
    pub fn vendor_prefix(self) -> Option<&'static str> {
        match self {
            SecretShape::SupabaseSecretKey => Some("sb_secret_"),
            SecretShape::SupabaseServiceRoleJwt | SecretShape::OpaqueJwt => Some("eyJ"),
            SecretShape::StripeLiveSecretKey => Some("sk_live_"),
            SecretShape::StripeTestSecretKey => Some("sk_test_"),
            SecretShape::StripeWebhookSecret => Some("whsec_"),
            SecretShape::AwsAccessKeyId => Some("AKIA"),
            SecretShape::GitHubToken => Some("ghp_"),
            SecretShape::SlackToken => Some("xoxb-"),
            SecretShape::GoogleApiKey => Some("AIza"),
            SecretShape::AnthropicKey => Some("sk-ant-"),
            SecretShape::OpenAiKey => Some("sk-"),
            SecretShape::PemPrivateKey
            | SecretShape::NamedSecretLiteral
            | SecretShape::BearerToken
            | SecretShape::ConnectionStringPassword
            | SecretShape::AzureStorageConnectionString
            | SecretShape::SecretInUrlQuery => None,
        }
    }

    /// True for S1..S12 (a fixed vendor prefix; near-zero false positives). False for the
    /// context-shaped S13..S17.
    pub fn is_vendor_shaped(self) -> bool {
        !matches!(
            self,
            SecretShape::NamedSecretLiteral
                | SecretShape::BearerToken
                | SecretShape::ConnectionStringPassword
                | SecretShape::AzureStorageConnectionString
                | SecretShape::SecretInUrlQuery
        )
    }

    /// Match-priority rank (position in [`SecretShape::ALL`]).
    pub(crate) fn rank(self) -> usize {
        SecretShape::ALL
            .iter()
            .position(|s| *s == self)
            .unwrap_or(usize::MAX)
    }
}

/// The prefix a concrete `value` of `shape` carries, for the redacted form. Empty for the
/// context-shaped shapes (the redactor replaces only their value).
pub(crate) fn prefix_of(shape: SecretShape, value: &str) -> &str {
    let fixed_len = |candidates: &[&str]| {
        candidates
            .iter()
            .find(|p| value.starts_with(**p))
            .map_or(0, |p| p.len())
    };
    let len = match shape {
        SecretShape::SupabaseSecretKey => fixed_len(&["sb_secret_"]),
        SecretShape::SupabaseServiceRoleJwt | SecretShape::OpaqueJwt => fixed_len(&["eyJ"]),
        SecretShape::StripeLiveSecretKey => fixed_len(&["sk_live_", "rk_live_"]),
        SecretShape::StripeTestSecretKey => fixed_len(&["sk_test_", "rk_test_"]),
        SecretShape::StripeWebhookSecret => fixed_len(&["whsec_"]),
        SecretShape::AwsAccessKeyId => fixed_len(&["AKIA", "ASIA"]),
        SecretShape::GitHubToken => {
            fixed_len(&["github_pat_", "ghp_", "gho_", "ghu_", "ghs_", "ghr_"])
        }
        // `xox` + one class letter + `-`.
        SecretShape::SlackToken => 5.min(value.len()),
        SecretShape::GoogleApiKey => fixed_len(&["AIza"]),
        SecretShape::AnthropicKey => fixed_len(&["sk-ant-"]),
        SecretShape::OpenAiKey => fixed_len(&["sk-proj-", "sk-"]),
        SecretShape::PemPrivateKey
        | SecretShape::NamedSecretLiteral
        | SecretShape::BearerToken
        | SecretShape::ConnectionStringPassword
        | SecretShape::AzureStorageConnectionString
        | SecretShape::SecretInUrlQuery => 0,
    };
    &value[..len]
}

// ---------------------------------------------------------------------------------------
// Patterns (spec section 2.2). Quoted verbatim so the catalogue and the code cannot drift;
// `patterns_compile_and_match_their_documented_example` pins each one.
// ---------------------------------------------------------------------------------------

pub(crate) const S1_SUPABASE_SECRET: &str = r"sb_secret_[A-Za-z0-9_]{10,}";
pub(crate) const S2_JWT: &str = r"eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}";
pub(crate) const S3_STRIPE_LIVE: &str = r"(?:sk_live_|rk_live_)[A-Za-z0-9]{16,}";
pub(crate) const S4_STRIPE_TEST: &str = r"(?:sk_test_|rk_test_)[A-Za-z0-9]{16,}";
pub(crate) const S5_STRIPE_WEBHOOK: &str = r"whsec_[A-Za-z0-9]{16,}";
pub(crate) const S6_AWS: &str = r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b";
pub(crate) const S7_GITHUB: &str = r"\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{22,})";
pub(crate) const S8_SLACK: &str = r"\bxox[abceprs]-[A-Za-z0-9-]{10,}";
pub(crate) const S9_GOOGLE: &str = r"\bAIza[0-9A-Za-z_-]{35}\b";
pub(crate) const S10_ANTHROPIC: &str = r"\bsk-ant-[A-Za-z0-9_-]{20,}";
pub(crate) const S11_OPENAI: &str = r"\bsk-(?:proj-)?[A-Za-z0-9_-]{20,}";
pub(crate) const S12_PEM_HEADER: &str =
    r"-----BEGIN (?:RSA |EC |DSA |OPENSSH |PGP |ENCRYPTED )?PRIVATE KEY(?: BLOCK)?-----";
pub(crate) const S12_PEM_FOOTER: &str =
    r"-----END (?:RSA |EC |DSA |OPENSSH |PGP |ENCRYPTED )?PRIVATE KEY(?: BLOCK)?-----";
pub(crate) const S13_NAMED_LITERAL: &str = r#"(?i:[A-Za-z0-9_]*(?:key|secret|token|password|passwd|pwd|credential)[A-Za-z0-9_]*)[^\n]{0,40}?(?:"([A-Za-z0-9+/_]{24,}=*)"|'([A-Za-z0-9+/_]{24,}=*)')"#;
// `[ \t]+` rather than `\s+`: a match never spans a newline, so line numbers stay valid.
pub(crate) const S14_BEARER: &str = r"(?i)\bbearer[ \t]+([A-Za-z0-9_.=-]{20,})";
pub(crate) const S15_CONNECTION_STRING: &str =
    r#"\b[a-z][a-z0-9+.-]{1,30}://[^\s/:@"']+:([^\s@/"']{4,})@[^\s"']+"#;
pub(crate) const S16_AZURE: &str =
    r"DefaultEndpointsProtocol=https?;[^\n]{0,300}?AccountKey=([A-Za-z0-9+/=]{20,})";

/// The secret-named URL query parameter alternation (the `ARCH-NO-SECRETS-IN-URL-1` list).
/// Public so the gateway's structural URL lint is built from the same list the redactor uses.
pub const URL_SECRET_PARAMS: &str = "access_token|refresh_token|auth_token|client_secret|private_key|api_?key|passwd|password|pwd|secret|signature|sig|token|key";

fn s17_pattern() -> String {
    format!(r#"[?&](?i:{URL_SECRET_PARAMS})=([^\s&"'<>{{}}$%]{{8,}})"#)
}

macro_rules! cached {
    ($name:ident, $pattern:expr) => {
        pub(crate) fn $name() -> &'static Regex {
            static RE: OnceLock<Regex> = OnceLock::new();
            RE.get_or_init(|| {
                Regex::new($pattern).expect(concat!(stringify!($name), " must compile"))
            })
        }
    };
}

cached!(re_supabase_secret, S1_SUPABASE_SECRET);
cached!(re_jwt, S2_JWT);
cached!(re_stripe_live, S3_STRIPE_LIVE);
cached!(re_stripe_test, S4_STRIPE_TEST);
cached!(re_stripe_webhook, S5_STRIPE_WEBHOOK);
cached!(re_aws, S6_AWS);
cached!(re_github, S7_GITHUB);
cached!(re_slack, S8_SLACK);
cached!(re_google, S9_GOOGLE);
cached!(re_anthropic, S10_ANTHROPIC);
cached!(re_openai, S11_OPENAI);
cached!(re_pem_header, S12_PEM_HEADER);
cached!(re_pem_footer, S12_PEM_FOOTER);
cached!(re_named_literal, S13_NAMED_LITERAL);
cached!(re_bearer, S14_BEARER);
cached!(re_connection_string, S15_CONNECTION_STRING);
cached!(re_azure, S16_AZURE);

pub(crate) fn re_url_query() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(&s17_pattern()).expect("re_url_query must compile"))
}

// ---------------------------------------------------------------------------------------
// Corpus-rule mapping
// ---------------------------------------------------------------------------------------

const VENDOR_TOKEN_SHAPES: &[SecretShape] = &[
    SecretShape::SupabaseSecretKey,
    SecretShape::SupabaseServiceRoleJwt,
    SecretShape::StripeLiveSecretKey,
    SecretShape::StripeTestSecretKey,
    SecretShape::StripeWebhookSecret,
    SecretShape::AwsAccessKeyId,
    SecretShape::GitHubToken,
    SecretShape::SlackToken,
    SecretShape::GoogleApiKey,
    SecretShape::AnthropicKey,
    SecretShape::OpenAiKey,
    SecretShape::ConnectionStringPassword,
    SecretShape::AzureStorageConnectionString,
];

const HARDCODED_SECRET_SHAPES: &[SecretShape] = &[
    SecretShape::SupabaseServiceRoleJwt,
    SecretShape::AwsAccessKeyId,
    SecretShape::GitHubToken,
    SecretShape::SlackToken,
    SecretShape::GoogleApiKey,
    SecretShape::OpenAiKey,
    SecretShape::PemPrivateKey,
    SecretShape::NamedSecretLiteral,
];

const PRIVATE_KEY_SHAPES: &[SecretShape] = &[SecretShape::PemPrivateKey];

const URL_SECRET_SHAPES: &[SecretShape] = &[SecretShape::SecretInUrlQuery];

/// The shapes a corpus rule id governs. Unknown id yields an empty slice.
///
/// `SEC-NO-VENDOR-TOKEN-1` lists the connection-string shape too, but a connection string is a
/// vendor finding only on a Supabase host; use [`crate::detect_for_rule`] (which applies that
/// filter) rather than `detect_shapes(shapes_for_rule(..))` for the rule's findings.
/// `OpaqueJwt` and `BearerToken` belong to no rule: they are redaction-only shapes.
pub fn shapes_for_rule(rule_id: &str) -> &'static [SecretShape] {
    match rule_id {
        "SEC-NO-VENDOR-TOKEN-1" => VENDOR_TOKEN_SHAPES,
        "SEC-NO-HARDCODED-SECRETS-1" => HARDCODED_SECRET_SHAPES,
        "SEC-NO-PRIVATE-KEY-1" => PRIVATE_KEY_SHAPES,
        "ARCH-NO-SECRETS-IN-URL-1" => URL_SECRET_SHAPES,
        _ => &[],
    }
}

/// True when a matched connection string points at a Supabase host (the only connection strings
/// `SEC-NO-VENDOR-TOKEN-1` treats as a vendor credential).
pub fn is_supabase_dsn(matched: &str) -> bool {
    let host = matched.rsplit_once('@').map_or(matched, |(_, host)| host);
    let host = host.split(['/', '?', '#']).next().unwrap_or(host);
    let host = host.rsplit_once(':').map_or(host, |(h, _)| h);
    host.ends_with(".supabase.co") || host.ends_with(".supabase.com")
}
