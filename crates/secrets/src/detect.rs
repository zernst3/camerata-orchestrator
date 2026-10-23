//! Detection: run every shape's pattern, classify twins, resolve overlaps.

use std::ops::Range;
use std::sync::OnceLock;

use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use base64::Engine;
use regex::Regex;

use crate::shapes::{self, is_supabase_dsn, shapes_for_rule, SecretShape};
use crate::twins::{classify, Twin};

/// How far past a PEM header the matching footer is searched for.
const PEM_FOOTER_WINDOW: usize = 8000;
/// The largest JWT payload segment decoded for its `role` claim.
const JWT_PAYLOAD_DECODE_CAP: usize = 4096;

/// One match. `start..end` is the WHOLE matched span in `text`; `value` is the sub-span the
/// redactor replaces (the whole span for vendor shapes; the capture group for the context shapes;
/// the key body for a PEM block). Byte offsets, always on char boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretMatch {
    pub start: usize,
    pub end: usize,
    pub value: Range<usize>,
    pub shape: SecretShape,
    pub twin: Option<Twin>,
    /// For the JWT shapes: the decoded `role` claim, if any.
    pub jwt_role: Option<String>,
}

impl SecretMatch {
    /// True when the match is a secret (not a safe twin).
    pub fn is_secret(&self) -> bool {
        self.twin.is_none()
    }

    /// True when the match should still be listed as a finding row: a secret, or a twin the floor
    /// keeps (a committed placeholder key is still worth a row; a public anon key is not).
    pub fn counts_as_finding(&self) -> bool {
        self.twin.as_ref().is_none_or(Twin::counts_as_line)
    }
}

fn push(
    out: &mut Vec<SecretMatch>,
    text: &str,
    shape: SecretShape,
    span: Range<usize>,
    value: Range<usize>,
    jwt_role: Option<String>,
) {
    let twin = classify(shape, &text[value.clone()], jwt_role.as_deref());
    out.push(SecretMatch {
        start: span.start,
        end: span.end,
        value,
        shape,
        twin,
        jwt_role,
    });
}

fn collect_whole(out: &mut Vec<SecretMatch>, text: &str, shape: SecretShape, re: &Regex) {
    for m in re.find_iter(text) {
        push(out, text, shape, m.range(), m.range(), None);
    }
}

/// Context shapes: the first participating capture group is the value. `span_is_value` makes the
/// reported span the value alone (Bearer: the word `Bearer` is not part of the secret).
fn collect_group(
    out: &mut Vec<SecretMatch>,
    text: &str,
    shape: SecretShape,
    re: &Regex,
    span_is_value: bool,
) {
    for caps in re.captures_iter(text) {
        let (Some(whole), Some(group)) = (caps.get(0), caps.iter().skip(1).flatten().next()) else {
            continue;
        };
        let span = if span_is_value {
            group.range()
        } else {
            whole.range()
        };
        push(out, text, shape, span, group.range(), None);
    }
}

fn decode_segment(segment: &str) -> Option<Vec<u8>> {
    if segment.len() > JWT_PAYLOAD_DECODE_CAP {
        return None;
    }
    URL_SAFE_NO_PAD
        .decode(segment)
        .or_else(|_| STANDARD_NO_PAD.decode(segment))
        .ok()
}

/// The decoded `role` claim of a JWT's payload segment, if it decodes to a JSON object with one.
fn jwt_role(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = decode_segment(payload)?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    claims.get("role")?.as_str().map(str::to_string)
}

fn collect_jwt(out: &mut Vec<SecretMatch>, text: &str, wanted: &[SecretShape]) {
    for m in shapes::re_jwt().find_iter(text) {
        let role = jwt_role(m.as_str());
        let shape = if role.as_deref() == Some("service_role") {
            SecretShape::SupabaseServiceRoleJwt
        } else {
            SecretShape::OpaqueJwt
        };
        if wanted.contains(&shape) {
            push(out, text, shape, m.range(), m.range(), role);
        }
    }
}

// ---- PEM ----------------------------------------------------------------------------------

fn re_pem_inline_body() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\A[ \t]*([A-Za-z0-9+/=\\]{16,})").expect("re_pem_inline_body must compile")
    })
}

fn re_pem_body_line() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\A[A-Za-z0-9+/=]{4,}\z").expect("re_pem_body_line must compile"))
}

/// The largest index `<= index` that is a char boundary of `text`.
fn floor_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// True when `inner` is exactly the redaction marker line our own redactor writes (plus blank
/// lines), so a redacted PEM block re-scans as a twin.
pub(crate) fn is_pem_marker(inner: &str) -> bool {
    let trimmed = inner.trim();
    trimmed.starts_with('[')
        && trimmed.ends_with(']')
        && trimmed.contains("private key material:")
        && !trimmed.contains('\n')
}

/// True when `inner` can be PEM body text: base64 characters, whitespace, literal `\n`/`\r`
/// escapes and the RFC 1421 header characters (`Proc-Type: 4,ENCRYPTED`). Code between a header
/// literal and a footer literal (a detector's own source) is not key material.
fn looks_like_pem_body(inner: &str) -> bool {
    inner
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c.is_whitespace() || "+/=\\:,.-".contains(c))
}

/// The key-body range after a header whose footer was not found: an inline run (a JSON string
/// cut off by a cap) or the base64 lines directly below the header. Empty when there is none.
fn unfooted_body(text: &str, header_end: usize) -> Range<usize> {
    let rest = &text[header_end..];
    if let Some(group) = re_pem_inline_body()
        .captures(rest)
        .and_then(|caps| caps.get(1))
    {
        return header_end + group.start()..header_end + group.end();
    }
    let line_end = rest.find('\n').unwrap_or(rest.len());
    if !rest[..line_end].trim().is_empty() {
        return header_end..header_end;
    }
    let mut body_start: Option<usize> = None;
    let mut body_end = header_end;
    let mut offset = header_end + line_end;
    while offset < text.len() {
        let line_start = offset + 1;
        if line_start > text.len() {
            break;
        }
        let line_len = text[line_start..]
            .find('\n')
            .unwrap_or(text.len() - line_start);
        let line = text[line_start..line_start + line_len].trim_end_matches('\r');
        let long_enough = line.len() >= 16 || body_start.is_some();
        if !(long_enough && re_pem_body_line().is_match(line)) {
            break;
        }
        body_start.get_or_insert(line_start);
        body_end = line_start + line.len();
        offset = line_start + line_len;
    }
    body_start.map_or(header_end..header_end, |start| start..body_end)
}

fn collect_pem(out: &mut Vec<SecretMatch>, text: &str) {
    let header_re = shapes::re_pem_header();
    let footer_re = shapes::re_pem_footer();
    let mut cursor = 0;
    while let Some(header) = header_re.find_at(text, cursor) {
        let header_end = header.end();
        let window_end = floor_boundary(text, header_end + PEM_FOOTER_WINDOW);
        let footed = footer_re
            .find(&text[header_end..window_end])
            .map(|footer| (header_end + footer.start(), header_end + footer.end()))
            .filter(|(inner_end, _)| {
                let inner = &text[header_end..*inner_end];
                is_pem_marker(inner) || looks_like_pem_body(inner)
            });
        let (end, value) = match footed {
            Some((inner_end, footer_end)) => (footer_end, header_end..inner_end),
            None => {
                let body = unfooted_body(text, header_end);
                (body.end.max(header_end), body)
            }
        };
        let inner = &text[value.clone()];
        let twin = if inner.trim().is_empty() {
            Some(Twin::NoKeyMaterial)
        } else if is_pem_marker(inner) {
            Some(Twin::RedactionMarker)
        } else {
            None
        };
        out.push(SecretMatch {
            start: header.start(),
            end,
            value,
            shape: SecretShape::PemPrivateKey,
            twin,
            jwt_role: None,
        });
        cursor = end.max(header_end);
    }
}

// ---- driver -------------------------------------------------------------------------------

fn collect(out: &mut Vec<SecretMatch>, text: &str, shapes: &[SecretShape]) {
    let mut jwt_done = false;
    for &shape in shapes {
        match shape {
            SecretShape::SupabaseSecretKey => {
                collect_whole(out, text, shape, shapes::re_supabase_secret())
            }
            SecretShape::SupabaseServiceRoleJwt | SecretShape::OpaqueJwt => {
                if !jwt_done {
                    jwt_done = true;
                    collect_jwt(out, text, shapes);
                }
            }
            SecretShape::StripeLiveSecretKey => {
                collect_whole(out, text, shape, shapes::re_stripe_live())
            }
            SecretShape::StripeTestSecretKey => {
                collect_whole(out, text, shape, shapes::re_stripe_test())
            }
            SecretShape::StripeWebhookSecret => {
                collect_whole(out, text, shape, shapes::re_stripe_webhook())
            }
            SecretShape::AwsAccessKeyId => collect_whole(out, text, shape, shapes::re_aws()),
            SecretShape::GitHubToken => collect_whole(out, text, shape, shapes::re_github()),
            SecretShape::SlackToken => collect_whole(out, text, shape, shapes::re_slack()),
            SecretShape::GoogleApiKey => collect_whole(out, text, shape, shapes::re_google()),
            SecretShape::AnthropicKey => collect_whole(out, text, shape, shapes::re_anthropic()),
            SecretShape::OpenAiKey => collect_whole(out, text, shape, shapes::re_openai()),
            SecretShape::PemPrivateKey => collect_pem(out, text),
            SecretShape::NamedSecretLiteral => {
                collect_group(out, text, shape, shapes::re_named_literal(), false)
            }
            SecretShape::BearerToken => collect_group(out, text, shape, shapes::re_bearer(), true),
            SecretShape::ConnectionStringPassword => {
                collect_group(out, text, shape, shapes::re_connection_string(), false)
            }
            SecretShape::AzureStorageConnectionString => {
                collect_group(out, text, shape, shapes::re_azure(), false)
            }
            SecretShape::SecretInUrlQuery => {
                collect_group(out, text, shape, shapes::re_url_query(), false)
            }
        }
    }
}

/// Resolve overlaps. Candidates conflict when their VALUE spans overlap (so a bearer token that is
/// itself a GitHub token, or a quoted literal that is a Stripe key, is reported once, as the
/// specific shape). Sorted by `(value.start, longest first, shape order)`; a candidate that starts
/// before the previous kept candidate's end is dropped.
fn resolve(mut candidates: Vec<SecretMatch>) -> Vec<SecretMatch> {
    candidates.sort_by(|a, b| {
        a.value
            .start
            .cmp(&b.value.start)
            .then(b.value.len().cmp(&a.value.len()))
            .then(a.shape.rank().cmp(&b.shape.rank()))
    });
    let mut kept: Vec<SecretMatch> = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let overlaps = kept
            .last()
            .is_some_and(|previous| candidate.value.start < previous.value.end);
        if !overlaps {
            kept.push(candidate);
        }
    }
    kept
}

/// Every match, twins INCLUDED, left to right, non-overlapping. Deterministic.
pub fn detect_all(text: &str) -> Vec<SecretMatch> {
    detect_shapes(text, SecretShape::ALL)
}

/// `detect_all` minus twins. This is what the redactor and the release gate use.
pub fn detect_secrets(text: &str) -> Vec<SecretMatch> {
    let mut matches = detect_all(text);
    matches.retain(SecretMatch::is_secret);
    matches
}

/// The first non-twin match, if any.
pub fn first_secret(text: &str) -> Option<SecretMatch> {
    detect_secrets(text).into_iter().next()
}

/// `detect_all` restricted to `shapes`, twins included.
pub fn detect_shapes(text: &str, shapes: &[SecretShape]) -> Vec<SecretMatch> {
    let mut candidates = Vec::new();
    collect(&mut candidates, text, shapes);
    resolve(candidates)
}

/// The matches a corpus rule governs: `detect_shapes` over `shapes_for_rule`, plus the
/// `SEC-NO-VENDOR-TOKEN-1` rule's Supabase-host filter on connection strings.
pub fn detect_for_rule(text: &str, rule_id: &str) -> Vec<SecretMatch> {
    let mut matches = detect_shapes(text, shapes_for_rule(rule_id));
    if rule_id == "SEC-NO-VENDOR-TOKEN-1" {
        matches.retain(|m| {
            m.shape != SecretShape::ConnectionStringPassword
                || is_supabase_dsn(&text[m.start..m.end])
        });
    }
    matches
}

fn lines_of(text: &str, matches: Vec<SecretMatch>, twins_count: bool) -> Vec<usize> {
    let mut lines: Vec<usize> = matches
        .iter()
        .filter(|m| {
            if twins_count {
                m.counts_as_finding()
            } else {
                m.is_secret()
            }
        })
        .map(|m| text[..m.start].bytes().filter(|&b| b == b'\n').count() + 1)
        .collect();
    lines.sort_unstable();
    lines.dedup();
    lines
}

/// 1-based line numbers (sorted, deduped) where any of `shapes` matches. `twins_count` says
/// whether a twin match still counts as a line.
pub fn match_lines(text: &str, shapes: &[SecretShape], twins_count: bool) -> Vec<usize> {
    lines_of(text, detect_shapes(text, shapes), twins_count)
}

/// [`match_lines`] for a corpus rule id, with the rule's host filter applied.
pub fn match_lines_for_rule(text: &str, rule_id: &str, twins_count: bool) -> Vec<usize> {
    lines_of(text, detect_for_rule(text, rule_id), twins_count)
}
