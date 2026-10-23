//! The redactor (spec section 2.1): replace every non-twin match's value with
//! `prefix…tail (N chars)`; keep the surrounding syntax and the line count.

use std::borrow::Cow;
use std::ops::Range;

use crate::detect::{detect_secrets, is_decor, SecretMatch};
use crate::shapes::{self, prefix_of, SecretShape};
use crate::twins::ELLIPSIS;

/// A body longer than this many chars shows its last four chars; a body of this length or less
/// shows only the total length (a four-char tail on a twenty-char body would reveal 20% of it).
/// The body is what follows the vendor prefix, or the whole value for a prefix-less shape.
const TAIL_MIN_BODY_CHARS: usize = 20;
const TAIL_CHARS: usize = 4;

/// Result of [`redact`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redacted {
    pub text: String,
    /// One entry per replaced match, in order. Never the value.
    pub redactions: Vec<Redaction>,
}

/// One replaced match: the shape, the redacted form that went into the output, and the original
/// byte span of the value in the INPUT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redaction {
    pub shape: SecretShape,
    pub form: String,
    pub span: Range<usize>,
}

fn last_chars(value: &str, count: usize) -> String {
    let total = value.chars().count();
    value.chars().skip(total.saturating_sub(count)).collect()
}

fn generic_form(shape: SecretShape, value: &str) -> String {
    let prefix = prefix_of(shape, value);
    let total = value.chars().count();
    let body = total - prefix.chars().count();
    let tail = if body > TAIL_MIN_BODY_CHARS {
        last_chars(value, TAIL_CHARS)
    } else {
        String::new()
    };
    format!("{prefix}{ELLIPSIS}{tail} ({total} chars)")
}

fn jwt_form(value: &str, role: Option<&str>) -> String {
    let total = value.chars().count();
    let tail = last_chars(value, TAIL_CHARS);
    let role = role.unwrap_or("unknown");
    format!("eyJ{ELLIPSIS}{tail} ({total} chars, role {role})")
}

/// The header qualifier (`RSA`, `EC`, ...) of the PEM block starting at `start`, or `PEM`.
fn pem_kind(text: &str, start: usize) -> String {
    let header = shapes::re_pem_header()
        .find(&text[start..])
        .map_or("", |m| m.as_str());
    let qualifier = header
        .trim_start_matches("-----BEGIN ")
        .split("PRIVATE KEY")
        .next()
        .unwrap_or("")
        .trim();
    if qualifier.is_empty() {
        "PEM".to_string()
    } else {
        qualifier.to_string()
    }
}

/// The content of one line of a key body: the span between its leading and trailing list
/// decoration (indentation, `- `, quotes, a trailing comma). `None` for a decoration-only line.
fn content_span(line: &str) -> Option<Range<usize>> {
    let start = line.find(|c: char| !is_decor(c))?;
    let end = line.rfind(|c: char| !is_decor(c))?;
    let last_len = line[end..].chars().next().map_or(1, char::len_utf8);
    Some(start..end + last_len)
}

/// The replacement for a PEM key body: ONE marker in place of the first content line, every other
/// content line emptied, and each line's own decoration (indentation, `- `, quotes, commas) kept.
/// The number of `\n` is unchanged, so every line number downstream stays valid, and a JSON or
/// YAML list keeps its shape.
fn pem_form(text: &str, m: &SecretMatch) -> String {
    let inner = &text[m.value.clone()];
    if inner.trim().is_empty() {
        return String::new();
    }
    let lines: Vec<&str> = inner.split('\n').collect();
    let content = lines
        .iter()
        .filter(|line| content_span(line).is_some())
        .count();
    let noun = if content == 1 { "line" } else { "lines" };
    let mut marker = Some(format!(
        "[{} private key material: {content} {noun} redacted]",
        pem_kind(text, m.start)
    ));
    let rewritten: Vec<String> = lines
        .iter()
        .map(|line| match content_span(line) {
            None => (*line).to_string(),
            Some(span) => format!(
                "{}{}{}",
                &line[..span.start],
                marker.take().unwrap_or_default(),
                &line[span.end..]
            ),
        })
        .collect();
    rewritten.join("\n")
}

impl SecretMatch {
    /// The redacted replacement for this match's `value`. Never contains the value.
    pub fn redacted_form(&self, text: &str) -> String {
        let value = &text[self.value.clone()];
        match self.shape {
            SecretShape::PemPrivateKey => pem_form(text, self),
            SecretShape::SupabaseServiceRoleJwt | SecretShape::OpaqueJwt => {
                jwt_form(value, self.jwt_role.as_deref())
            }
            shape => generic_form(shape, value),
        }
    }
}

fn apply(text: &str, matches: &[SecretMatch]) -> Redacted {
    let mut out = String::with_capacity(text.len());
    let mut redactions = Vec::with_capacity(matches.len());
    let mut cursor = 0;
    for m in matches {
        out.push_str(&text[cursor..m.value.start]);
        let form = m.redacted_form(text);
        out.push_str(&form);
        redactions.push(Redaction {
            shape: m.shape,
            form,
            span: m.value.clone(),
        });
        cursor = m.value.end;
    }
    out.push_str(&text[cursor..]);
    Redacted {
        text: out,
        redactions,
    }
}

/// Replace every non-twin match with its redacted form. Preserves the number of `\n` in `text`
/// (a PEM block pads). Idempotent: a redacted form matches no shape.
pub fn redact(text: &str) -> Redacted {
    apply(text, &detect_secrets(text))
}

/// `redact(text).text`, borrowing when nothing matched.
pub fn redact_str(text: &str) -> Cow<'_, str> {
    let matches = detect_secrets(text);
    if matches.is_empty() {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(apply(text, &matches).text)
    }
}
