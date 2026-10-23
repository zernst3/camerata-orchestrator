//! Safe twins (spec section 2.3): values that match a secret SHAPE but are not secrets, and so
//! stay readable and never count as a gate failure.

use sha2::{Digest, Sha256};

use crate::shapes::{prefix_of, SecretShape};

/// Why a match is NOT a secret (left readable, never a gate failure).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum Twin {
    /// T1: the opaque body contains a placeholder word (which one is carried).
    PlaceholderWord(&'static str),
    /// T2: a quoted identifier such as `SUPABASE_SERVICE_ROLE_KEY` (a NAME, not a value).
    EnvNameLiteral,
    /// T3: a key a vendor publishes in its documentation (matched by SHA-256 digest).
    PublishedExampleKey,
    /// T4: a Supabase JWT whose role is `anon` or `authenticated` (public by design).
    PublicKeyRole(String),
    /// T5: a template token (`${...}`, `{{...}}`, `<...>`, `[YOUR-PASSWORD]`, `%s`).
    TemplateToken,
    /// Extension: a path-like quoted literal (`app/api/auth/token/refresh`) on a credential-named
    /// identifier. Real base64 secrets are not all-lowercase with several slashes.
    PathLike,
    /// T6: the value already carries the redaction ellipsis, so it is a redacted form.
    RedactionMarker,
    /// Extension: a PEM header with no key material after it (a one-line snippet).
    NoKeyMaterial,
}

impl Twin {
    /// Whether the scan floor still lists a twin match as a finding row when asked to count
    /// twins (a committed placeholder `sb_secret_` is still worth a row; a public anon key, an
    /// already-redacted form, or a file path on a credential-named variable never is).
    pub fn counts_as_line(&self) -> bool {
        !matches!(
            self,
            Twin::PublicKeyRole(_) | Twin::RedactionMarker | Twin::PathLike
        )
    }
}

/// T1 placeholder words, matched case-insensitively against the opaque body. Adding one is a
/// one-line change with a twin test.
pub const PLACEHOLDER_WORDS: &[&str] = &[
    "your",
    "example",
    "placeholder",
    "replace",
    "changeme",
    "change_me",
    "change-me",
    "dummy",
    "fake",
    "sample",
    "redacted",
    "xxxx",
    "todo",
    "insert",
    "_here",
    "-here",
    "do_not",
    "donot",
    "mock",
];

/// The redaction ellipsis (U+2026). Outside every body class, so a redacted form never re-matches.
pub(crate) const ELLIPSIS: char = '\u{2026}';

pub(crate) fn sha256_hex(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn placeholder_word(body: &str) -> Option<&'static str> {
    let lower = body.to_ascii_lowercase();
    PLACEHOLDER_WORDS
        .iter()
        .copied()
        .find(|word| lower.contains(word))
}

/// T2: `^[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+$`.
fn is_env_name(value: &str) -> bool {
    let mut chars = value.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_uppercase()) {
        return false;
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        return false;
    }
    value.contains('_') && !value.ends_with('_') && !value.contains("__")
}

/// T5.
fn is_template(value: &str) -> bool {
    const OPENERS: &[&str] = &["{", "<", "[", "%", "$"];
    if OPENERS.iter().any(|opener| value.starts_with(opener)) {
        return true;
    }
    ["password", "secret", "token", "changeme"]
        .iter()
        .any(|word| value.eq_ignore_ascii_case(word))
}

fn is_path_like(value: &str) -> bool {
    value.matches('/').count() >= 2
        && value.split('/').all(|segment| {
            segment
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
}

fn is_context_shaped(shape: SecretShape) -> bool {
    !shape.is_vendor_shaped()
}

/// Classify a candidate. `role` is the decoded JWT role for the JWT shapes. Order (spec 3.2):
/// redaction marker, T3 digest, T4 role, T2, T1, T5, then the path-like extension.
pub(crate) fn classify(shape: SecretShape, value: &str, role: Option<&str>) -> Option<Twin> {
    if value.contains(ELLIPSIS) {
        return Some(Twin::RedactionMarker);
    }
    if crate::published::is_published_example(&sha256_hex(value)) {
        return Some(Twin::PublishedExampleKey);
    }
    if let Some(role) = role {
        if role == "anon" || role == "authenticated" {
            return Some(Twin::PublicKeyRole(role.to_string()));
        }
    }
    if shape == SecretShape::NamedSecretLiteral && is_env_name(value) {
        return Some(Twin::EnvNameLiteral);
    }
    // A JWT is base64 of structured data: a placeholder word inside it is coincidence, so the
    // body-word rule does not apply to the JWT shapes or the PEM body.
    let word_rule_applies = !matches!(
        shape,
        SecretShape::SupabaseServiceRoleJwt | SecretShape::OpaqueJwt | SecretShape::PemPrivateKey
    );
    if word_rule_applies {
        let body = &value[prefix_of(shape, value).len()..];
        if let Some(word) = placeholder_word(body) {
            return Some(Twin::PlaceholderWord(word));
        }
    }
    if is_context_shaped(shape) && is_template(value) {
        return Some(Twin::TemplateToken);
    }
    if shape == SecretShape::NamedSecretLiteral && is_path_like(value) {
        return Some(Twin::PathLike);
    }
    None
}
