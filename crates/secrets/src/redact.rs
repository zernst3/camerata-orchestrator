//! The redactor (spec section 2.1): replace every non-twin match's value with
//! `prefix…tail (N chars)`; keep the surrounding syntax and the line count.

use std::borrow::Cow;
use std::ops::Range;

use crate::detect::{detect_secrets, SecretMatch};
use crate::shapes::{self, prefix_of, SecretShape};
use crate::twins::ELLIPSIS;

/// A body longer than this many chars shows its last four chars; a shorter one shows only the
/// length (a four-char tail on a ten-char body would reveal 40% of it).
const TAIL_MIN_BODY_CHARS: usize = 12;
/// The same rule for prefix-less shapes, measured on the whole value.
const TAIL_MIN_VALUE_CHARS: usize = 16;
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
    let show_tail = if prefix.is_empty() {
        total > TAIL_MIN_VALUE_CHARS
    } else {
        body > TAIL_MIN_BODY_CHARS
    };
    let tail = if show_tail {
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

/// The replacement for a PEM key body: ONE marker line, padded with blank lines so the number of
/// `\n` in the text is unchanged and every line number downstream stays valid.
fn pem_form(text: &str, m: &SecretMatch) -> String {
    let inner = &text[m.value.clone()];
    if inner.trim().is_empty() {
        return String::new();
    }
    let newlines = inner.matches('\n').count();
    let lines = if newlines == 0 {
        1
    } else {
        inner.lines().filter(|line| !line.trim().is_empty()).count()
    };
    let noun = if lines == 1 { "line" } else { "lines" };
    let marker = format!(
        "[{} private key material: {lines} {noun} redacted]",
        pem_kind(text, m.start)
    );
    if newlines == 0 {
        marker
    } else if inner.starts_with('\n') {
        format!("\n{marker}{}", "\n".repeat(newlines - 1))
    } else {
        format!("{marker}{}", "\n".repeat(newlines))
    }
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
