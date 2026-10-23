//! One positive test per shape S1..S17 and the JWT role decisions.

use super::{all_shapes, only_match};
use crate::fixtures::{jwt, planted, planted_all};
use crate::{detect_secrets, SecretShape};

#[test]
fn detects_every_shape_with_the_right_span_and_no_twin() {
    for p in planted_all() {
        let m = only_match(&p.text);
        assert_eq!(m.shape, p.shape, "shape for {:?}", p.shape);
        assert!(m.twin.is_none(), "{:?} must not be a twin", p.shape);
        // A PEM body span includes the newlines around the base64 lines.
        assert_eq!(
            p.text[m.value.clone()].trim(),
            p.value,
            "value span for {:?}",
            p.shape
        );
        assert!(m.start <= m.value.start && m.value.end <= m.end);
    }
}

#[test]
fn every_fixture_shape_is_covered() {
    let covered: Vec<SecretShape> = planted_all().iter().map(|p| p.shape).collect();
    assert_eq!(covered.as_slice(), all_shapes());
}

#[test]
fn detects_supabase_secret_key() {
    let p = planted(SecretShape::SupabaseSecretKey);
    assert_eq!(only_match(&p.text).shape, SecretShape::SupabaseSecretKey);
}

#[test]
fn supabase_jwt_with_service_role_claim_is_a_secret_and_the_form_names_the_role() {
    let token = jwt(Some("service_role"));
    let m = only_match(&token);
    assert_eq!(m.shape, SecretShape::SupabaseServiceRoleJwt);
    assert!(m.is_secret());
    assert_eq!(m.jwt_role.as_deref(), Some("service_role"));
    let form = m.redacted_form(&token);
    assert!(form.starts_with("eyJ…"), "form: {form}");
    assert!(form.ends_with(&format!("({} chars, role service_role)", token.len())));
    assert!(!form.contains(&token));
}

#[test]
fn jwt_with_no_role_claim_is_opaque_jwt_redacted_but_belongs_to_no_rule() {
    let token = jwt(None);
    let m = only_match(&token);
    assert_eq!(m.shape, SecretShape::OpaqueJwt);
    assert!(m.is_secret());
    assert!(m.redacted_form(&token).ends_with("role unknown)"));
    for rule in [
        "SEC-NO-VENDOR-TOKEN-1",
        "SEC-NO-HARDCODED-SECRETS-1",
        "SEC-NO-PRIVATE-KEY-1",
        "ARCH-NO-SECRETS-IN-URL-1",
    ] {
        assert!(
            !crate::shapes_for_rule(rule).contains(&SecretShape::OpaqueJwt),
            "OpaqueJwt must belong to no rule ({rule})"
        );
    }
}

#[test]
fn undecodable_jwt_payload_is_opaque_jwt() {
    // Three segments, but the payload decodes to bytes that are not JSON.
    let token = format!(
        "eyJ{}.{}.{}",
        "A".repeat(20),
        "A".repeat(20),
        "B".repeat(20)
    );
    let m = only_match(&token);
    assert_eq!(m.shape, SecretShape::OpaqueJwt);
    assert_eq!(m.jwt_role, None);
    assert!(m.is_secret());
}

#[test]
fn a_service_role_jwt_is_found_inside_surrounding_text() {
    let token = jwt(Some("service_role"));
    let text = format!("const client = createClient(url, \"{token}\");");
    let found = detect_secrets(&text);
    assert_eq!(found.len(), 1);
    assert_eq!(&text[found[0].value.clone()], token);
}

#[test]
fn bearer_wrapping_a_github_token_is_reported_once_as_github() {
    let token = planted(SecretShape::GitHubToken).value;
    let text = format!("Authorization: Bearer {token}");
    let m = only_match(&text);
    assert_eq!(m.shape, SecretShape::GitHubToken);
}

#[test]
fn a_quoted_literal_that_is_a_vendor_key_is_reported_as_the_vendor_shape() {
    let key = planted(SecretShape::StripeLiveSecretKey).value;
    let text = format!("const stripeApiKey = \"{key}\";");
    let m = only_match(&text);
    assert_eq!(m.shape, SecretShape::StripeLiveSecretKey);
}

#[test]
fn overlapping_matches_keep_the_longest_then_earliest() {
    // `sk-ant-` matches both the Anthropic and the OpenAI shape over the same span: the earlier
    // shape in priority order wins, and the pair is reported once.
    let key = planted(SecretShape::AnthropicKey).value;
    let m = only_match(&key);
    assert_eq!(m.shape, SecretShape::AnthropicKey);
    // A URL query value that is longer than the JWT inside it: the longer span wins.
    let token = jwt(Some("service_role"));
    let text = format!("https://x.test/cb?token={token}.extra-tail");
    let matches = detect_secrets(&text);
    assert_eq!(matches.len(), 1, "one match, not two overlapping ones");
}

#[test]
fn match_lines_attributes_each_match_to_its_starting_line_and_dedupes() {
    let a = planted(SecretShape::StripeLiveSecretKey).value;
    let b = planted(SecretShape::SupabaseSecretKey).value;
    let text = format!("// header\nA={a}\n\nB={b} C={a}\n");
    let lines = crate::match_lines(
        &text,
        &[
            SecretShape::StripeLiveSecretKey,
            SecretShape::SupabaseSecretKey,
        ],
        true,
    );
    assert_eq!(lines, vec![2, 4]);
}

#[test]
fn an_anthropic_key_is_never_reported_as_an_openai_key() {
    let key = planted(SecretShape::AnthropicKey).value;
    // A rule that governs OpenAI keys but not Anthropic ones must not fire on it.
    let found = crate::detect_shapes(&key, &[SecretShape::OpenAiKey]);
    assert!(
        found.is_empty(),
        "sk-ant- belongs to the Anthropic shape only"
    );
    let both = crate::detect_shapes(&key, &[SecretShape::OpenAiKey, SecretShape::AnthropicKey]);
    assert_eq!(both.len(), 1);
    assert_eq!(both[0].shape, SecretShape::AnthropicKey);
    // And a real OpenAI key still is one.
    let openai = planted(SecretShape::OpenAiKey).value;
    assert_eq!(
        crate::detect_shapes(&openai, &[SecretShape::OpenAiKey]).len(),
        1
    );
}

#[test]
fn pem_block_is_one_match_spanning_header_to_footer() {
    let p = planted(SecretShape::PemPrivateKey);
    let m = only_match(&p.text);
    assert_eq!(m.start, 0);
    assert_eq!(m.end, p.text.len());
    assert_eq!(p.text[m.value.clone()].trim(), p.value);
}

#[test]
fn pem_header_alone_is_left_readable() {
    let text = "-----BEGIN RSA PRIVATE KEY-----";
    let m = only_match(text);
    assert_eq!(m.shape, SecretShape::PemPrivateKey);
    assert!(m.twin.is_some(), "a bare header carries no key material");
    assert_eq!(crate::redact(text).text, text);
    // The floor still lists a header-only line when asked to count twins.
    assert_eq!(
        crate::match_lines(text, &[SecretShape::PemPrivateKey], true),
        vec![1]
    );
    assert!(crate::match_lines(text, &[SecretShape::PemPrivateKey], false).is_empty());
}

#[test]
fn pem_body_without_a_footer_is_still_key_material() {
    let block = crate::fixtures::pem_block("", 3);
    let truncated = block.rsplit_once('\n').map(|(head, _)| head).unwrap_or("");
    let found = detect_secrets(truncated);
    assert_eq!(
        found.len(),
        1,
        "a footer-less block still carries key body lines"
    );
    assert!(!crate::redact(truncated)
        .text
        .contains(block.lines().nth(1).unwrap_or("?")));
}

#[test]
fn pem_in_a_json_string_with_escaped_newlines_is_one_inline_secret() {
    let block = crate::fixtures::pem_block("", 2).replace('\n', "\\n");
    let text = format!("{{\"private_key\": \"{block}\"}}");
    let found = detect_secrets(&text);
    assert_eq!(found.len(), 1);
    let redacted = crate::redact(&text).text;
    assert!(redacted.contains("-----BEGIN PRIVATE KEY-----"));
    assert!(redacted.contains("-----END PRIVATE KEY-----"));
    assert!(redacted.contains("private key material"));
    assert!(!redacted.contains(block.split("\\n").nth(1).unwrap_or("?")));
}

#[test]
fn code_between_a_header_literal_and_a_footer_literal_is_not_key_material() {
    // A detector's own source mentions both markers with code in between.
    let text = "let start = \"-----BEGIN PRIVATE KEY-----\";\nfn work() { run(); }\nlet end = \"-----END PRIVATE KEY-----\";";
    assert!(detect_secrets(text).is_empty());
    assert_eq!(crate::redact(text).text, text);
}
