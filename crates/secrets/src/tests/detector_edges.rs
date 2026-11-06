//! Edge cases of the PEM, JWT and overlap logic that no other test reached.
//!
//! Written against the mutants `cargo mutants -p camerata-secrets` left alive on 2026-10-28
//! (docs/MUTATION_TESTING.md). Several of them changed only where a PEM match ENDS (whether its
//! footer is inside the match), not the redacted text, so these tests pin the span as well as
//! the redaction.

use crate::detect::is_pem_marker;
use crate::fixtures::{body, planted};
use crate::{detect_all, detect_secrets, first_secret, redact, SecretShape, Twin};

const B64: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const HEADER: &str = "-----BEGIN PRIVATE KEY-----";
const FOOTER: &str = "-----END PRIVATE KEY-----";

fn run(len: usize, seed: u64) -> String {
    body(len, B64, seed)
}

/// The single PEM match in `text`.
fn only_pem(text: &str) -> crate::SecretMatch {
    let matches: Vec<_> = detect_all(text)
        .into_iter()
        .filter(|m| m.shape == SecretShape::PemPrivateKey)
        .collect();
    assert_eq!(matches.len(), 1, "expected one PEM match");
    matches.into_iter().next().unwrap()
}

// ---- footed blocks ----------------------------------------------------------------------

#[test]
fn a_quoted_json_line_list_between_header_and_footer_is_one_block_ending_at_its_footer() {
    let (one, two) = (run(64, 301), run(40, 302));
    let text =
        format!("\"key\": [\n  \"{HEADER}\",\n  \"{one}\",\n  \"{two}\",\n  \"{FOOTER}\"\n]\n");
    let m = only_pem(&text);
    assert!(m.is_secret());
    assert!(
        text[m.start..m.end].ends_with(FOOTER),
        "the match runs to the footer, not to the last body line"
    );
    let redacted = redact(&text).text;
    assert!(!redacted.contains(&one) && !redacted.contains(&two));
}

#[test]
fn code_between_a_header_literal_and_a_footer_literal_is_not_key_material() {
    // A detector's own source: a long identifier sits between the two literals, but `;`, `(`
    // and `=` with spaces make it code, not a key body.
    let text = format!(
        "const H = \"{HEADER}\"; const someverylongidentifier = check(H); const F = \"{FOOTER}\";\n"
    );
    assert!(detect_secrets(&text).is_empty(), "code is not a key");
    assert_eq!(only_pem(&text).twin, Some(Twin::NoKeyMaterial));
}

#[test]
fn a_footer_beyond_the_search_window_is_not_part_of_the_block() {
    // 130 body lines of 64 chars put the footer past the 8,000-byte footer window.
    let lines: Vec<String> = (0..130).map(|i| run(64, 400 + i)).collect();
    let text = format!("{HEADER}\n{}\n{FOOTER}\n", lines.join("\n"));
    let m = only_pem(&text);
    assert!(m.is_secret());
    assert_eq!(&text[m.value.clone()], lines.join("\n"));
    assert!(
        !text[m.start..m.end].contains("-----END"),
        "an out-of-window footer is not claimed"
    );
}

// ---- unfooted blocks ----------------------------------------------------------------------

#[test]
fn an_inline_unfooted_body_behind_list_decoration_is_exactly_the_run() {
    // A minified JSON array cut off by a cap: header, `","`, then the body run, no footer.
    let body_run = run(64, 303);
    let text = format!("[\"{HEADER}\",\"{body_run}");
    let m = only_pem(&text);
    assert!(m.is_secret());
    assert_eq!(&text[m.value.clone()], body_run);
    assert!(!redact(&text).text.contains(&body_run));
}

#[test]
fn an_unfooted_yaml_list_body_starts_at_the_first_run_not_its_dash() {
    let (one, two) = (run(64, 304), run(64, 305));
    let text = format!("{HEADER}\n  - {one}\n  - {two}\n");
    let m = only_pem(&text);
    assert!(m.is_secret());
    assert_eq!(&text[m.value.clone()], format!("{one}\n  - {two}"));
}

#[test]
fn an_unfooted_first_line_needs_sixteen_body_characters() {
    let short = run(8, 306);
    let text = format!("{HEADER}\n{short}\n");
    assert!(
        detect_secrets(&text).is_empty(),
        "an 8-char line is not a key body"
    );
    assert_eq!(only_pem(&text).twin, Some(Twin::NoKeyMaterial));

    let sixteen = run(16, 307);
    let text = format!("{HEADER}\n{sixteen}\n");
    let m = only_pem(&text);
    assert!(m.is_secret(), "a 16-char first line is a key body");
    assert_eq!(&text[m.value.clone()], sixteen);
}

// ---- the redaction marker ------------------------------------------------------------------

#[test]
fn a_redacted_block_rescans_as_the_redaction_marker_twin() {
    let text = crate::fixtures::pem_block("RSA ", 3);
    let redacted = redact(&text).text;
    assert_eq!(only_pem(&redacted).twin, Some(Twin::RedactionMarker));
}

#[test]
fn only_our_own_single_line_bracketed_marker_is_a_redaction_marker() {
    assert!(is_pem_marker(
        "[RSA private key material: 3 lines redacted]"
    ));
    assert!(!is_pem_marker(
        "RSA private key material: 3 lines redacted]"
    ));
    assert!(!is_pem_marker(
        "[RSA private key material: 3 lines redacted"
    ));
    assert!(!is_pem_marker(
        "[RSA private key material:\n3 lines redacted]"
    ));
    assert!(!is_pem_marker("[a bracketed note]"));
}

// ---- JWT payload decoding cap ------------------------------------------------------------

/// A JWT whose payload segment is exactly `payload_len` base64url chars (a multiple of 4) and
/// carries `"role":"service_role"`.
fn service_role_jwt_with_payload_len(payload_len: usize) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let bytes = payload_len / 4 * 3;
    let skeleton = r#"{"role":"service_role","pad":""}"#;
    let claims = format!(
        r#"{{"role":"service_role","pad":"{}"}}"#,
        "a".repeat(bytes - skeleton.len())
    );
    let payload = URL_SAFE_NO_PAD.encode(claims);
    assert_eq!(payload.len(), payload_len);
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","typ":"JWT"}"#);
    format!(
        "{header}.{payload}.{}",
        body(43, B64.trim_end_matches("+/"), 9)
    )
}

#[test]
fn a_payload_at_the_decode_cap_is_decoded_and_one_past_it_is_not() {
    let at_cap = service_role_jwt_with_payload_len(4096);
    assert_eq!(
        detect_all(&at_cap)[0].shape,
        SecretShape::SupabaseServiceRoleJwt
    );
    let over_cap = service_role_jwt_with_payload_len(4100);
    assert_eq!(detect_all(&over_cap)[0].shape, SecretShape::OpaqueJwt);
}

// ---- first_secret ------------------------------------------------------------------------

#[test]
fn first_secret_returns_the_leftmost_secret() {
    let first = planted(SecretShape::GitHubToken);
    let second = planted(SecretShape::StripeLiveSecretKey);
    let text = format!("{}\n{}\n", first.text, second.text);
    let found = first_secret(&text).expect("a secret is present");
    assert_eq!(found.shape, SecretShape::GitHubToken);
    assert!(first_secret("nothing secret here").is_none());
}
