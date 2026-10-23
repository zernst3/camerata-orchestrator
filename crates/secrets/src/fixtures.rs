//! Synthetic secret-shaped values for tests, in this crate and in every crate that proves it
//! never leaks one.
//!
//! Every value is BUILT AT RUNTIME from fragments (spec section 2.4): the repository holds no
//! secret-shaped literal, so push protection and secret scanners stay quiet, and no value here
//! was ever a credential. Each body is a deterministic pseudo-random run of the shape's own
//! character class, chosen so it carries no placeholder word (it is a real secret to the
//! detector, not a safe twin).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

use crate::shapes::SecretShape;

const ALNUM: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const UPPER_DIGITS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
const URL_SAFE: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-";
const HEX: &str = "0123456789abcdef";
const BASE64: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// A fixture: `text` is a snippet containing the secret, `value` is the exact substring that must
/// never survive redaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planted {
    pub shape: SecretShape,
    pub text: String,
    pub value: String,
}

/// A deterministic pseudo-random string of `len` chars drawn from `alphabet`.
pub fn body(len: usize, alphabet: &str, seed: u64) -> String {
    let chars: Vec<char> = alphabet.chars().collect();
    let mut state = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let index = usize::try_from(state >> 33).unwrap_or(0) % chars.len();
            chars[index]
        })
        .collect()
}

/// A JWT whose payload carries `role` (or no `role` claim when `None`).
pub fn jwt(role: Option<&str>) -> String {
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","typ":"JWT"}"#);
    let claims = match role {
        Some(role) => format!(r#"{{"iss":"supabase","ref":"abcdefghij","role":"{role}"}}"#),
        None => r#"{"iss":"supabase","ref":"abcdefghij"}"#.to_string(),
    };
    let payload = URL_SAFE_NO_PAD.encode(claims);
    format!("{header}.{payload}.{}", body(43, URL_SAFE, 7))
}

/// Stripe's documented test key, assembled from fragments (it is published, and allowlisted by
/// digest in [`crate::published`]).
pub fn stripe_docs_test_key() -> String {
    ["sk_test_", "4eC39HqLyjWD", "arjtT1zdp7dc"].concat()
}

/// A `-----BEGIN ... PRIVATE KEY-----` block with `body_lines` lines of key body.
pub fn pem_block(qualifier: &str, body_lines: usize) -> String {
    let header = format!("-----BEGIN {qualifier}PRIVATE KEY-----");
    let footer = format!("-----END {qualifier}PRIVATE KEY-----");
    let lines: Vec<String> = (0..body_lines)
        .map(|i| body(64, BASE64, 100 + i as u64))
        .collect();
    format!("{header}\n{}\n{footer}", lines.join("\n"))
}

/// One planted secret per shape, in [`SecretShape::ALL`] order.
pub fn planted(shape: SecretShape) -> Planted {
    let (text, value) = match shape {
        SecretShape::SupabaseSecretKey => {
            let v = format!("sb_secret_{}", body(41, ALNUM, 1));
            (format!("SUPABASE_KEY={v}"), v)
        }
        SecretShape::SupabaseServiceRoleJwt => {
            let v = jwt(Some("service_role"));
            (format!("const key = {v};"), v)
        }
        SecretShape::OpaqueJwt => {
            let v = jwt(None);
            (format!("session = {v}"), v)
        }
        SecretShape::StripeLiveSecretKey => {
            let v = format!("sk_live_51{}", body(67, ALNUM, 3));
            (format!("STRIPE_SECRET_KEY={v}"), v)
        }
        SecretShape::StripeTestSecretKey => {
            let v = format!("sk_test_51{}", body(40, ALNUM, 4));
            (format!("STRIPE_TEST_KEY={v}"), v)
        }
        SecretShape::StripeWebhookSecret => {
            let v = format!("whsec_{}", body(32, ALNUM, 5));
            (format!("STRIPE_WEBHOOK_SECRET={v}"), v)
        }
        SecretShape::AwsAccessKeyId => {
            let v = format!("AKIA{}", body(16, UPPER_DIGITS, 6));
            (format!("aws_access_key_id = {v}"), v)
        }
        SecretShape::GitHubToken => {
            let v = format!("ghp_{}", body(36, ALNUM, 8));
            (format!("GITHUB_TOKEN={v}"), v)
        }
        SecretShape::SlackToken => {
            let v = format!("xoxb-{}", body(30, ALNUM, 9));
            (format!("SLACK_BOT={v}"), v)
        }
        SecretShape::GoogleApiKey => {
            let v = format!("AIza{}", body(35, URL_SAFE, 10));
            (format!("MAPS_KEY={v}"), v)
        }
        SecretShape::AnthropicKey => {
            let v = format!("sk-ant-{}", body(40, URL_SAFE, 11));
            (format!("ANTHROPIC_API_KEY={v}"), v)
        }
        SecretShape::OpenAiKey => {
            let v = format!("sk-proj-{}", body(48, URL_SAFE, 12));
            (format!("OPENAI_API_KEY={v}"), v)
        }
        SecretShape::SendGridApiKey => {
            let v = format!("SG.{}.{}", body(22, URL_SAFE, 18), body(43, URL_SAFE, 19));
            (format!("SENDGRID_API_KEY={v}"), v)
        }
        SecretShape::TwilioApiKey => {
            let v = format!("SK{}", body(32, HEX, 20));
            (format!("TWILIO_API_KEY_SID={v}"), v)
        }
        SecretShape::ShopifyToken => {
            let v = format!("shpat_{}", body(32, HEX, 21));
            (format!("SHOPIFY_ACCESS_TOKEN={v}"), v)
        }
        SecretShape::MailgunApiKey => {
            let v = format!("key-{}", body(32, HEX, 22));
            (format!("MAILGUN_API_KEY={v}"), v)
        }
        SecretShape::NpmToken => {
            let v = format!("npm_{}", body(36, ALNUM, 23));
            (format!("NPM_TOKEN={v}"), v)
        }
        SecretShape::MapboxSecretToken => {
            let v = format!(
                "sk.eyJ{}.{}",
                body(80, URL_SAFE, 24),
                body(22, URL_SAFE, 25)
            );
            (format!("MAPBOX_SECRET_TOKEN={v}"), v)
        }
        SecretShape::PemPrivateKey => {
            let block = pem_block("RSA ", 3);
            let lines: Vec<&str> = block.lines().collect();
            let value = lines[1..lines.len() - 1].join("\n");
            (block, value)
        }
        SecretShape::DiscordBotToken => {
            let v = format!(
                "M{}.{}.{}",
                body(23, URL_SAFE, 26),
                body(6, URL_SAFE, 27),
                body(27, URL_SAFE, 28)
            );
            (format!("DISCORD_TOKEN={v}"), v)
        }
        SecretShape::AwsSecretAccessKey => {
            let v = body(40, BASE64, 29);
            (format!("aws_secret_access_key = {v}"), v)
        }
        SecretShape::TwilioAuthToken => {
            let v = body(32, HEX, 30);
            (format!("TWILIO_AUTH_TOKEN={v}"), v)
        }
        SecretShape::NpmrcAuthToken => {
            let v = body(36, ALNUM, 31);
            (format!("//registry.npmjs.org/:_authToken={v}"), v)
        }
        SecretShape::DockerAuthEntry => {
            let v = body(36, BASE64, 32);
            (
                format!(
                    "{{\"auths\": {{\"https://index.docker.io/v1/\": {{\"auth\": \"{v}\"}}}}}}"
                ),
                v,
            )
        }
        SecretShape::PgPassPassword => {
            let v = body(24, ALNUM, 33);
            (format!("db.internal.test:5432:appdb:appuser:{v}"), v)
        }
        SecretShape::NamedSecretLiteral => {
            let v = body(40, ALNUM, 13);
            (format!("const vendorApiKey = \"{v}\";"), v)
        }
        SecretShape::BearerToken => {
            let v = body(40, ALNUM, 14);
            (format!("Authorization: Bearer {v}"), v)
        }
        SecretShape::BasicAuthCredential => {
            let v = format!("{}==", body(30, ALNUM, 34));
            (format!("Authorization: Basic {v}"), v)
        }
        SecretShape::ConnectionStringPassword => {
            let v = body(24, ALNUM, 15);
            (
                format!(
                    "DATABASE_URL=postgres://postgres:{v}@db.abcdefghij.supabase.co:5432/postgres"
                ),
                v,
            )
        }
        SecretShape::AzureStorageConnectionString => {
            let v = format!("{}==", body(86, BASE64, 16));
            (
                format!("DefaultEndpointsProtocol=https;AccountName=acct;AccountKey={v};EndpointSuffix=core.windows.net"),
                v,
            )
        }
        SecretShape::SecretInUrlQuery => {
            let v = body(32, ALNUM, 17);
            (
                format!("https://api.vendor.test/v1/items?token={v}&page=2"),
                v,
            )
        }
    };
    Planted { shape, text, value }
}

/// Every shape's fixture.
pub fn planted_all() -> Vec<Planted> {
    SecretShape::ALL.iter().map(|s| planted(*s)).collect()
}
