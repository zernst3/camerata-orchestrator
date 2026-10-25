//! Detection: run every shape's pattern, classify twins, resolve overlaps.

use std::ops::Range;
use std::sync::OnceLock;

use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use base64::Engine;
use regex::Regex;

use crate::firebase::{TwinContext, LINE_PREFIX_CAP, WINDOW};
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

/// The text around the value at `value_start`: its own line up to the value (capped) and a window
/// either side. Both are char-boundary safe.
fn twin_context(text: &str, value_start: usize) -> TwinContext<'_> {
    let line_start = text[..value_start].rfind('\n').map_or(0, |i| i + 1);
    let prefix_start = floor_boundary(
        text,
        line_start.max(value_start.saturating_sub(LINE_PREFIX_CAP)),
    );
    let win_start = floor_boundary(text, value_start.saturating_sub(WINDOW));
    let win_end = floor_boundary(text, value_start.saturating_add(WINDOW));
    TwinContext {
        line_prefix: &text[prefix_start..value_start],
        window: &text[win_start..win_end],
        at: value_start - win_start,
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
    // The text of the match before the value: for a context shape it holds the identifier.
    let before_value = &text[span.start..value.start];
    let context = twin_context(text, value.start);
    let twin = classify(
        shape,
        &text[value.clone()],
        before_value,
        jwt_role.as_deref(),
        &context,
    );
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
    // A padded segment (`=`) decodes the same once the padding is dropped.
    let segment = segment.trim_end_matches('=');
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

/// `sk-` keys. An `sk-ant-` run also matches the OpenAI pattern, but it is an Anthropic key
/// whichever shapes the caller asked for, so it is never reported as an OpenAI one (a rule that
/// governs OpenAI keys but not Anthropic ones must not fire on it).
fn collect_openai(out: &mut Vec<SecretMatch>, text: &str) {
    for m in shapes::re_openai().find_iter(text) {
        if m.as_str().starts_with("sk-ant-") {
            continue;
        }
        push(
            out,
            text,
            SecretShape::OpenAiKey,
            m.range(),
            m.range(),
            None,
        );
    }
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
        // An inline run, optionally behind list decoration (`","`) and optionally followed by
        // further quoted runs on the same line (a minified JSON array).
        Regex::new(
            r#"\A[ \t]*["',-]*[ \t]*([A-Za-z0-9+/=\\]{16,}(?:[ \t]*["',-]+[ \t]*[A-Za-z0-9+/=]{4,})*)"#,
        )
        .expect("re_pem_inline_body must compile")
    })
}

/// One body line of a key: a base64 run, optionally behind list decoration (`- `, `"`) and
/// followed by a quote or comma (a YAML list item, a JSON array element). Group 1 is the run.
fn re_pem_body_line() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"\A[ \t]*["'-]*[ \t]*([A-Za-z0-9+/=]{4,})["',]*[ \t]*\z"#)
            .expect("re_pem_body_line must compile")
    })
}

/// Characters around a key body that are list syntax, not key material: whitespace, quotes,
/// commas and the YAML list dash.
pub(crate) fn is_decor(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | ',' | '-')
}

/// The largest index `<= index` that is a char boundary of `text`.
fn floor_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// True when `core` (the text between header and footer, list decoration trimmed) is exactly the
/// redaction marker line our own redactor writes, so a redacted PEM block re-scans as a twin.
pub(crate) fn is_pem_marker(core: &str) -> bool {
    core.starts_with('[')
        && core.ends_with(']')
        && core.contains("private key material:")
        && !core.contains('\n')
}

/// True when `inner` can be PEM body text: base64 characters, whitespace, literal `\n`/`\r`
/// escapes and the RFC 1421 header characters (`Proc-Type: 4,ENCRYPTED`). Code between a header
/// literal and a footer literal (a detector's own source) is not key material. Quotes are allowed
/// only around a real base64 run (a quoted line list), never between two bare markers.
fn looks_like_pem_body(inner: &str) -> bool {
    let base = |c: char| c.is_ascii_alphanumeric() || c.is_whitespace() || "+/=\\:,.-".contains(c);
    if inner.chars().all(base) {
        return true;
    }
    inner.chars().all(|c| base(c) || c == '"' || c == '\'')
        && inner
            .split(|c: char| !(c.is_ascii_alphanumeric() || "+/=".contains(c)))
            .any(|run| run.len() >= 16)
}

/// The key-body range after a header whose footer was not found: an inline run (a JSON string
/// cut off by a cap) or the base64 lines directly below the header (plain, or a quoted/dashed
/// list). Empty when there is none.
fn unfooted_body(text: &str, header_end: usize) -> Range<usize> {
    let rest = &text[header_end..];
    if let Some(group) = re_pem_inline_body()
        .captures(rest)
        .and_then(|caps| caps.get(1))
    {
        return header_end + group.start()..header_end + group.end();
    }
    let line_end = rest.find('\n').unwrap_or(rest.len());
    if !rest[..line_end].chars().all(is_decor) {
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
        let Some(run) = re_pem_body_line()
            .captures(line)
            .and_then(|caps| caps.get(1))
        else {
            break;
        };
        if run.len() < 16 && body_start.is_none() {
            break;
        }
        body_start.get_or_insert(line_start + run.start());
        body_end = line_start + run.end();
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
                is_pem_marker(inner.trim_matches(is_decor)) || looks_like_pem_body(inner)
            });
        let (end, value) = match footed {
            Some((inner_end, footer_end)) => (footer_end, header_end..inner_end),
            None => {
                let body = unfooted_body(text, header_end);
                (body.end.max(header_end), body)
            }
        };
        let core = text[value.clone()].trim_matches(is_decor);
        let twin = if core.is_empty() {
            Some(Twin::NoKeyMaterial)
        } else if is_pem_marker(core) {
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
            SecretShape::OpenAiKey => collect_openai(out, text),
            SecretShape::SendGridApiKey => collect_whole(out, text, shape, shapes::re_sendgrid()),
            SecretShape::TwilioApiKey => {
                collect_whole(out, text, shape, shapes::re_twilio_api_key())
            }
            SecretShape::ShopifyToken => collect_whole(out, text, shape, shapes::re_shopify()),
            SecretShape::MailgunApiKey => collect_whole(out, text, shape, shapes::re_mailgun()),
            SecretShape::NpmToken => collect_whole(out, text, shape, shapes::re_npm()),
            SecretShape::MapboxSecretToken => collect_whole(out, text, shape, shapes::re_mapbox()),
            SecretShape::DiscordBotToken => collect_whole(out, text, shape, shapes::re_discord()),
            SecretShape::PemPrivateKey => collect_pem(out, text),
            SecretShape::AwsSecretAccessKey => {
                collect_group(out, text, shape, shapes::re_aws_secret_key(), false)
            }
            SecretShape::TwilioAuthToken => {
                collect_group(out, text, shape, shapes::re_twilio_auth_token(), false)
            }
            SecretShape::NpmrcAuthToken => {
                collect_group(out, text, shape, shapes::re_npmrc_auth(), false)
            }
            SecretShape::DockerAuthEntry => {
                collect_group(out, text, shape, shapes::re_docker_auth(), false)
            }
            SecretShape::PgPassPassword => {
                collect_group(out, text, shape, shapes::re_pgpass(), false)
            }
            SecretShape::NamedSecretLiteral => {
                collect_group(out, text, shape, shapes::re_named_literal(), false);
                collect_group(out, text, shape, shapes::re_named_literal_unquoted(), false);
            }
            SecretShape::BearerToken => collect_group(out, text, shape, shapes::re_bearer(), true),
            SecretShape::BasicAuthCredential => {
                collect_group(out, text, shape, shapes::re_basic_auth(), false)
            }
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
