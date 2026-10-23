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
    /// S18: SendGrid `SG.<22>.<43>`.
    SendGridApiKey,
    /// S19: Twilio API key SID `SK<32 hex>`.
    TwilioApiKey,
    /// S20: Shopify `shpat_` / `shpss_` / `shpca_`.
    ShopifyToken,
    /// S21: Mailgun `key-<32 hex>`.
    MailgunApiKey,
    /// S22: npm `npm_<36>`.
    NpmToken,
    /// S23: Mapbox secret token `sk.eyJ...`.
    MapboxSecretToken,
    /// S12: a PEM private key block.
    PemPrivateKey,
    /// S24: a Discord bot token (three dotted base64url segments).
    DiscordBotToken,
    /// S25: the 40-char AWS-format secret access key on a `secret`-named line.
    AwsSecretAccessKey,
    /// S26: the 32-hex Twilio auth token on a token-named line.
    TwilioAuthToken,
    /// S27: an `.npmrc` `_authToken` / `_auth` / `_password` value.
    NpmrcAuthToken,
    /// S28: a docker `config.json` `"auth"` entry (base64 `user:password`).
    DockerAuthEntry,
    /// S29: the password field of a `.pgpass` line.
    PgPassPassword,
    /// S13: a long literal (quoted, or an unquoted `.env`/YAML/ini value) assigned to a
    /// credential-named identifier.
    NamedSecretLiteral,
    /// S14: `Bearer <token>`.
    BearerToken,
    /// S30: `Authorization: Basic <base64>`.
    BasicAuthCredential,
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
const LABELS: [&str; 31] = [
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
    "sendgrid_api_key",
    "twilio_api_key",
    "shopify_token",
    "mailgun_api_key",
    "npm_token",
    "mapbox_secret_token",
    "pem_private_key",
    "discord_bot_token",
    "aws_secret_access_key",
    "twilio_auth_token",
    "npmrc_auth_token",
    "docker_auth_entry",
    "pgpass_password",
    "named_secret_literal",
    "bearer_token",
    "basic_auth_credential",
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
        SecretShape::SendGridApiKey,
        SecretShape::TwilioApiKey,
        SecretShape::ShopifyToken,
        SecretShape::MailgunApiKey,
        SecretShape::NpmToken,
        SecretShape::MapboxSecretToken,
        SecretShape::PemPrivateKey,
        SecretShape::DiscordBotToken,
        SecretShape::AwsSecretAccessKey,
        SecretShape::TwilioAuthToken,
        SecretShape::NpmrcAuthToken,
        SecretShape::DockerAuthEntry,
        SecretShape::PgPassPassword,
        SecretShape::NamedSecretLiteral,
        SecretShape::BearerToken,
        SecretShape::BasicAuthCredential,
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
            SecretShape::SendGridApiKey => "SendGrid API key",
            SecretShape::TwilioApiKey => "Twilio API key",
            SecretShape::ShopifyToken => "Shopify access token",
            SecretShape::MailgunApiKey => "Mailgun API key",
            SecretShape::NpmToken => "npm access token",
            SecretShape::MapboxSecretToken => "Mapbox secret token",
            SecretShape::PemPrivateKey => "private key",
            SecretShape::DiscordBotToken => "Discord bot token",
            SecretShape::AwsSecretAccessKey => "secret access key (AWS format)",
            SecretShape::TwilioAuthToken => "Twilio auth token",
            SecretShape::NpmrcAuthToken => "npm registry credential",
            SecretShape::DockerAuthEntry => "Docker registry credential",
            SecretShape::PgPassPassword => "Postgres password-file entry",
            SecretShape::NamedSecretLiteral => "hardcoded credential",
            SecretShape::BearerToken => "bearer token",
            SecretShape::BasicAuthCredential => "basic-auth credential",
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
            SecretShape::SendGridApiKey => Some("SG."),
            SecretShape::TwilioApiKey => Some("SK"),
            SecretShape::ShopifyToken => Some("shpat_"),
            SecretShape::MailgunApiKey => Some("key-"),
            SecretShape::NpmToken => Some("npm_"),
            SecretShape::MapboxSecretToken => Some("sk."),
            SecretShape::PemPrivateKey
            | SecretShape::DiscordBotToken
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
            | SecretShape::SecretInUrlQuery => None,
        }
    }

    /// True for the shapes with a fixed vendor prefix (S1..S12 and S18..S23; near-zero false
    /// positives). False for the context-shaped ones (S13..S17, S24..S30).
    pub fn is_vendor_shaped(self) -> bool {
        !matches!(
            self,
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
        SecretShape::SendGridApiKey => fixed_len(&["SG."]),
        SecretShape::TwilioApiKey => fixed_len(&["SK"]),
        SecretShape::ShopifyToken => fixed_len(&["shpat_", "shpss_", "shpca_"]),
        SecretShape::MailgunApiKey => fixed_len(&["key-"]),
        SecretShape::NpmToken => fixed_len(&["npm_"]),
        SecretShape::MapboxSecretToken => fixed_len(&["sk."]),
        SecretShape::PemPrivateKey
        | SecretShape::DiscordBotToken
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
        | SecretShape::SecretInUrlQuery => 0,
    };
    &value[..len]
}

// ---------------------------------------------------------------------------------------
// Patterns (spec section 2.2). Quoted verbatim so the catalogue and the code cannot drift;
// `patterns_compile_and_match_their_documented_example` pins each one.
// ---------------------------------------------------------------------------------------

pub(crate) const S1_SUPABASE_SECRET: &str = r"sb_secret_[A-Za-z0-9_][A-Za-z0-9_-]{9,}";
pub(crate) const S2_JWT: &str =
    r"eyJ[A-Za-z0-9_-]{10,}={0,2}\.[A-Za-z0-9_-]{10,}={0,2}\.[A-Za-z0-9_-]{10,}={0,2}";
pub(crate) const S3_STRIPE_LIVE: &str = r"(?:sk_live_|rk_live_)[A-Za-z0-9]{16,}";
pub(crate) const S4_STRIPE_TEST: &str = r"(?:sk_test_|rk_test_)[A-Za-z0-9]{16,}";
pub(crate) const S5_STRIPE_WEBHOOK: &str = r"whsec_[A-Za-z0-9]{16,}";
pub(crate) const S6_AWS: &str = r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b";
pub(crate) const S7_GITHUB: &str = r"\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{22,})";
pub(crate) const S8_SLACK: &str = r"\b(?:xox[abceprs]-|xapp-)[A-Za-z0-9-]{10,}";
pub(crate) const S9_GOOGLE: &str = r"\bAIza[0-9A-Za-z_-]{35}\b";
pub(crate) const S10_ANTHROPIC: &str = r"\bsk-ant-[A-Za-z0-9_-]{20,}";
pub(crate) const S11_OPENAI: &str = r"\bsk-(?:proj-)?[A-Za-z0-9_-]{20,}";
pub(crate) const S12_PEM_HEADER: &str =
    r"-----BEGIN (?:RSA |EC |DSA |OPENSSH |PGP |ENCRYPTED )?PRIVATE KEY(?: BLOCK)?-----";
pub(crate) const S12_PEM_FOOTER: &str =
    r"-----END (?:RSA |EC |DSA |OPENSSH |PGP |ENCRYPTED )?PRIVATE KEY(?: BLOCK)?-----";
/// S13, quoted form. The literal admits `-` and `.` (UUID-shaped tokens) and an escaped closing
/// quote (`\"`, JSON source inside a JSON string).
pub(crate) const S13_NAMED_LITERAL: &str = r#"(?i:[A-Za-z0-9_]*(?:key|secret|token|password|passwd|pwd|passphrase|credential)[A-Za-z0-9_]*)[^\n]{0,40}?(?:"([A-Za-z0-9+/_.-]{24,}=*)\\?"|'([A-Za-z0-9+/_.-]{24,}=*)\\?')"#;
/// S13, unquoted form: a `.env`, YAML or ini line whose whole value is a bare run. `[ \t]`
/// rather than `\s`, so a match never starts on an earlier blank line.
pub(crate) const S13_UNQUOTED: &str = r"(?im)^[ \t]*(?:export[ \t]+|-[ \t]+)?[A-Za-z0-9_.-]*(?:key|secret|token|password|passwd|pwd|passphrase|credential)[A-Za-z0-9_]*[ \t]*[:=][ \t]*([A-Za-z0-9+/_=.-]{20,})[ \t\r]*$";
// `[ \t]+` rather than `\s+`: a match never spans a newline, so line numbers stay valid.
pub(crate) const S14_BEARER: &str = r"(?i)\bbearer[ \t]+([A-Za-z0-9_.=-]{20,})";
pub(crate) const S15_CONNECTION_STRING: &str =
    r#"\b[a-z][a-z0-9+.-]{1,30}://[^\s/:@"']+:([^\s@/"']{4,})@[^\s"']+"#;
pub(crate) const S16_AZURE: &str =
    r"DefaultEndpointsProtocol=https?;[^\n]{0,300}?AccountKey=([A-Za-z0-9+/=]{20,})";
pub(crate) const S18_SENDGRID: &str = r"\bSG\.[A-Za-z0-9_-]{22}\.[A-Za-z0-9_-]{43,}";
pub(crate) const S19_TWILIO_API_KEY: &str = r"\bSK[0-9a-fA-F]{32}\b";
pub(crate) const S20_SHOPIFY: &str = r"\bshp(?:at|ss|ca)_[A-Za-z0-9]{32,}";
pub(crate) const S21_MAILGUN: &str = r"\bkey-[0-9a-fA-F]{32}\b";
pub(crate) const S22_NPM: &str = r"\bnpm_[A-Za-z0-9]{36,}";
pub(crate) const S23_MAPBOX: &str = r"\bsk\.eyJ[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{10,}";
pub(crate) const S24_DISCORD: &str =
    r"\b[MNO][A-Za-z0-9_-]{23,25}\.[A-Za-z0-9_-]{6}\.[A-Za-z0-9_-]{27,}";
// The 40-char run needs an exact end, so the pattern consumes one delimiter after it (the value
// is the capture group). `\\?` lets the quotes be JSON-escaped.
pub(crate) const S25_AWS_SECRET_KEY: &str = r#"(?i)secret[A-Za-z0-9_.-]*\\?["']?(?:[ \t]*(?:=>|[:=])[ \t]*|[ \t]+)\\?["']?([A-Za-z0-9/+]{40})(?:[^A-Za-z0-9/+=]|\z)"#;
pub(crate) const S26_TWILIO_AUTH_TOKEN: &str = r#"(?i)(?:twilio[A-Za-z0-9_.-]*(?:token|secret)|auth[_-]?token)[A-Za-z0-9_]*\\?["']?[ \t]*[:=][ \t]*\\?["']?([0-9a-f]{32})(?:[^0-9a-f]|\z)"#;
pub(crate) const S27_NPMRC_AUTH: &str =
    r#"(?i)\b_(?:authToken|auth|password)[ \t]*=[ \t]*["']?([A-Za-z0-9+/_=.-]{8,})"#;
pub(crate) const S28_DOCKER_AUTH: &str =
    r#"(?i)\\?"(?:auth|identitytoken|registrytoken)\\?"[ \t]*:[ \t]*\\?"([A-Za-z0-9+/=]{12,})"#;
// `host:port:database:user:password`. Not anchored to a whole line (a finding's prose quotes the
// line inline) but to a delimiter before the host and a delimiter after the password, so a
// `/etc/passwd` line (more colon-separated fields after the fifth) does not match.
pub(crate) const S29_PGPASS: &str = r#"(?m)(?:^|[\s"'`(\[,;=>])[A-Za-z0-9_.*-]+:(?:[0-9]{1,5}|\*):[^:\s]+:[^:\s]+:([^\s:"'`]{4,})(?:[\s"'`]|\z)"#;
pub(crate) const S30_BASIC_AUTH: &str =
    r#"(?i)\bauthorization\\?["']?[ \t]*[:=,][ \t]*\\?["']?basic[ \t]+([A-Za-z0-9+/]{8,}={0,2})"#;

/// The secret-named URL query parameter alternation (the `ARCH-NO-SECRETS-IN-URL-1` list).
/// Public so the gateway's structural URL lint is built from the same list the redactor uses.
pub const URL_SECRET_PARAMS: &str = "access_token|refresh_token|auth_token|client_secret|private_key|api_?key|passwd|password|pwd|secret|signature|sig|token|key";

fn s17_pattern() -> String {
    format!(r#"[?&](?i:{URL_SECRET_PARAMS})=([^\s&"'<>{{}}$]{{8,}})"#)
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
cached!(re_sendgrid, S18_SENDGRID);
cached!(re_twilio_api_key, S19_TWILIO_API_KEY);
cached!(re_shopify, S20_SHOPIFY);
cached!(re_mailgun, S21_MAILGUN);
cached!(re_npm, S22_NPM);
cached!(re_mapbox, S23_MAPBOX);
cached!(re_discord, S24_DISCORD);
cached!(re_aws_secret_key, S25_AWS_SECRET_KEY);
cached!(re_twilio_auth_token, S26_TWILIO_AUTH_TOKEN);
cached!(re_npmrc_auth, S27_NPMRC_AUTH);
cached!(re_docker_auth, S28_DOCKER_AUTH);
cached!(re_pgpass, S29_PGPASS);
cached!(re_basic_auth, S30_BASIC_AUTH);
cached!(re_named_literal_unquoted, S13_UNQUOTED);
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
    SecretShape::SendGridApiKey,
    SecretShape::TwilioApiKey,
    SecretShape::ShopifyToken,
    SecretShape::MailgunApiKey,
    SecretShape::NpmToken,
    SecretShape::MapboxSecretToken,
    SecretShape::DiscordBotToken,
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
    SecretShape::AwsSecretAccessKey,
    SecretShape::NamedSecretLiteral,
];

const PRIVATE_KEY_SHAPES: &[SecretShape] = &[SecretShape::PemPrivateKey];

const URL_SECRET_SHAPES: &[SecretShape] = &[SecretShape::SecretInUrlQuery];

/// The shapes a corpus rule id governs. Unknown id yields an empty slice.
///
/// `SEC-NO-VENDOR-TOKEN-1` lists the connection-string shape too, but a connection string is a
/// vendor finding only on a Supabase host; use [`crate::detect_for_rule`] (which applies that
/// filter) rather than `detect_shapes(shapes_for_rule(..))` for the rule's findings.
/// `OpaqueJwt`, `BearerToken`, `BasicAuthCredential` and the credential-file shapes (npmrc,
/// docker, pgpass, Twilio auth token) belong to no rule: they are redaction-only shapes.
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
