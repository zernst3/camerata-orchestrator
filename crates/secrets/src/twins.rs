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
    /// Extension: a path-like quoted literal (`app/api/auth/token/refresh`, `src/some/path.rs`) on
    /// a credential-named identifier. Real base64 secrets are not all-lowercase with several
    /// slashes.
    PathLike,
    /// T6: the value already carries the redaction ellipsis, so it is a redacted form.
    RedactionMarker,
    /// Extension: a PEM header with no key material after it (a one-line snippet).
    NoKeyMaterial,
    /// Extension: a value made only of words joined by `_`, `-` or `.`
    /// (`app_preferences_sidebar_collapsed_v2`, `process.env.SERVICE_ROLE_KEY`): an identifier
    /// or a key name, not a credential.
    ReadableName,
    /// Extension: a key that is public by design (`sb_publishable_`, `pk_live_`, `pk_test_`).
    PublishableKey,
}

impl Twin {
    /// Whether the scan floor still lists a twin match as a finding row when asked to count
    /// twins (a committed placeholder `sb_secret_` is still worth a row; a public anon key, an
    /// already-redacted form, or a file path on a credential-named variable never is).
    pub fn counts_as_line(&self) -> bool {
        !matches!(
            self,
            Twin::PublicKeyRole(_)
                | Twin::RedactionMarker
                | Twin::PathLike
                | Twin::ReadableName
                | Twin::PublishableKey
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

/// The case class of a body byte: a boundary lies between two bytes of different classes
/// (`_`, `-`, a case change, a digit next to a letter).
fn byte_class(byte: u8) -> u8 {
    match byte {
        b'a'..=b'z' => 0,
        b'A'..=b'Z' => 1,
        b'0'..=b'9' => 2,
        _ => 3,
    }
}

/// Whether `body[start..end]` (a placeholder word) stands apart from its neighbours: at the
/// start or end of the body, next to `_` or `-` (or carrying one itself), or at a case change.
fn is_delimited(body: &str, start: usize, end: usize, word: &str) -> bool {
    let bytes = body.as_bytes();
    let left = start == 0
        || word.starts_with(['_', '-'])
        || byte_class(bytes[start - 1]) != byte_class(bytes[start]);
    let right = end == bytes.len()
        || word.ends_with(['_', '-'])
        || byte_class(bytes[end - 1]) != byte_class(bytes[end]);
    left && right
}

/// T1. A VENDOR body is a random run, so a short word inside it is coincidence unless it is
/// delimited or the whole body is single-case (`sb_secret_yourkeyhere`, `FAKE_DO_NOT_USE`); a
/// context-shaped value is human-written, so any substring counts.
fn placeholder_word(body: &str, delimited_only: bool) -> Option<&'static str> {
    let lower = body.to_ascii_lowercase();
    let single_case = !body.bytes().any(|b| b.is_ascii_lowercase())
        || !body.bytes().any(|b| b.is_ascii_uppercase());
    PLACEHOLDER_WORDS.iter().copied().find(|word| {
        lower.match_indices(word).any(|(start, _)| {
            !delimited_only || single_case || is_delimited(body, start, start + word.len(), word)
        })
    })
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

/// `value` opens with `open` and closes with `close` (and is longer than the two together).
fn is_closed(value: &str, open: &str, close: char) -> bool {
    value.len() > open.len() && value.starts_with(open) && value.ends_with(close)
}

fn is_identifier(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

/// A format-string placeholder: `%s`, `%(name)s`, or a `{name}` field (a Python or Rust format
/// string; `postgres://user:{password}@host` is code, not a password).
fn is_format_placeholder(value: &str) -> bool {
    value == "%s"
        || value
            .strip_prefix("%(")
            .and_then(|rest| rest.strip_suffix(")s"))
            .is_some_and(is_identifier)
        || value
            .strip_prefix('{')
            .and_then(|rest| rest.strip_suffix('}'))
            .is_some_and(is_identifier)
}

/// T5. Only a CLOSED template is one: a real password may start with `$`, `%`, `[`, `<` or `{`.
fn is_template(value: &str) -> bool {
    is_closed(value, "${", '}')
        || is_closed(value, "{{", '}')
        || is_closed(value, "<", '>')
        || is_closed(value, "[", ']')
        || is_format_placeholder(value)
        || ["password", "secret", "token", "changeme"]
            .iter()
            .any(|word| value.eq_ignore_ascii_case(word))
}

/// A key that is public by design.
fn is_publishable_key(value: &str) -> bool {
    ["sb_publishable_", "pk_live_", "pk_test_"]
        .iter()
        .any(|prefix| value.starts_with(prefix))
}

fn is_name_separator(c: char) -> bool {
    matches!(c, '_' | '-' | '.')
}

/// One word of a readable name: all lowercase, all uppercase or Capitalized (digits tolerated).
fn is_name_word(segment: &str) -> bool {
    let mut letters = segment.chars().filter(char::is_ascii_alphabetic);
    let Some(first) = letters.next() else {
        return !segment.is_empty();
    };
    let rest: Vec<char> = letters.collect();
    rest.iter().all(char::is_ascii_lowercase)
        || (first.is_ascii_uppercase() && rest.iter().all(char::is_ascii_uppercase))
}

/// A readable name (owner decision 2026-10-23): at least three words joined by `_`, `-` or `.`,
/// at most two digits in all (`app_preferences_sidebar_collapsed_v2`,
/// `process.env.SERVICE_ROLE_KEY`). A random token has digits and mixed case.
fn is_readable_name(value: &str) -> bool {
    value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || is_name_separator(c))
        && value.chars().filter(char::is_ascii_digit).count() <= 2
        && value.split(is_name_separator).count() >= 3
        && value.split(is_name_separator).all(is_name_word)
}

fn is_path_like(value: &str) -> bool {
    value.matches('/').count() >= 2
        && value.split('/').all(|segment| {
            segment.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.')
            })
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
    if is_context_shaped(shape) && is_publishable_key(value) {
        return Some(Twin::PublishableKey);
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
        if let Some(word) = placeholder_word(body, shape.is_vendor_shaped()) {
            return Some(Twin::PlaceholderWord(word));
        }
    }
    if is_context_shaped(shape) && is_template(value) {
        return Some(Twin::TemplateToken);
    }
    if shape == SecretShape::NamedSecretLiteral && is_path_like(value) {
        return Some(Twin::PathLike);
    }
    if shape == SecretShape::NamedSecretLiteral && is_readable_name(value) {
        return Some(Twin::ReadableName);
    }
    if shape == SecretShape::BasicAuthCredential && value.bytes().all(|b| b.is_ascii_lowercase()) {
        // `Authorization: Basic authentication ...` is prose; a real credential is not one word.
        return Some(Twin::ReadableName);
    }
    None
}
