//! One positive test per shape S1..S17 and the JWT role decisions.

use super::{all_shapes, only_match};
use crate::fixtures::{body, jwt, planted, planted_all};
use crate::{detect_secrets, redact, SecretShape};

const ALNUM: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const HEX: &str = "0123456789abcdef";

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

// ---- leak hunt: S17, S13, S1, S2 ------------------------------------------------------------

#[test]
fn url_query_value_is_redacted_in_full_when_percent_encoded() {
    // Azure SAS: the `sig=` value is base64 with `+ / =` percent-encoded.
    let sig = format!(
        "{}%2B{}%2F{}%3D",
        body(20, ALNUM, 71),
        body(12, ALNUM, 72),
        body(8, ALNUM, 73)
    );
    let url =
        format!("https://acct.blob.core.windows.net/c/b.txt?sv=2022-11-02&sig={sig}&se=2026-10-23");
    let found = detect_secrets(&url);
    assert_eq!(found.len(), 1);
    assert_eq!(&url[found[0].value.clone()], sig);
    let out = redact(&url).text;
    assert!(!out.contains(&sig));
    assert!(!out.contains("%2B") && !out.contains("%2F"), "{out}");
    assert!(out.ends_with("&se=2026-10-23"), "{out}");
}

#[test]
fn url_query_value_starting_with_percent_is_a_secret_not_a_template() {
    let value = format!("%21{}", body(20, ALNUM, 74));
    let url = format!("https://x.test/cb?token={value}&a=1");
    assert_eq!(detect_secrets(&url).len(), 1);
    assert!(!redact(&url).text.contains(&value));
}

#[test]
fn unquoted_credential_assignments_in_env_yaml_and_ini_are_secrets() {
    let value = body(36, ALNUM, 81);
    for text in [
        format!("SERVICE_API_KEY={value}\n"),
        format!("export CLIENT_SECRET={value}"),
        format!("  client_secret: {value}  "),
        format!("- api_token: {value}"),
        format!("[db]\npassword = {value}\n"),
        format!("db-password:{value}\r\n"),
    ] {
        let found = detect_secrets(&text);
        assert_eq!(found.len(), 1, "{}", text.replace(&value, "<v>"));
        assert_eq!(found[0].shape, SecretShape::NamedSecretLiteral);
        assert_eq!(&text[found[0].value.clone()], value);
        assert!(!redact(&text).text.contains(&value));
    }
}

#[test]
fn unquoted_assignment_value_may_carry_base64_and_dotted_characters() {
    let value = format!(
        "{}+{}/{}==",
        body(14, ALNUM, 82),
        body(10, ALNUM, 83),
        body(8, ALNUM, 84)
    );
    let text = format!("AWS_SESSION_TOKEN={value}");
    assert!(!redact(&text).text.contains(&value));
}

#[test]
fn unquoted_assignment_does_not_span_lines_or_match_short_or_non_credential_names() {
    let value = body(40, ALNUM, 85);
    // Not credential-named.
    assert!(detect_secrets(&format!("SERVICE_NAME={value}")).is_empty());
    // Too short.
    assert!(detect_secrets("API_KEY=short_value").is_empty());
    // Code, not a bare value: a call, a trailing comment, a semicolon.
    assert!(detect_secrets("const token = getTokenFromTheSessionStore(request);").is_empty());
    assert!(detect_secrets(&format!("API_KEY={value} # rotate me")).is_empty());
    // The value never starts on a later line, so reported lines stay valid.
    let text = format!("# header\n\n\nAPI_KEY={value}\n");
    let found = detect_secrets(&text);
    assert_eq!(crate::match_lines(&text, SecretShape::ALL, false), vec![4]);
    assert_eq!(found.len(), 1);
}

#[test]
fn unquoted_assignment_of_a_template_or_placeholder_is_a_twin() {
    for line in [
        "API_KEY=your_api_key_goes_here_please",
        "SECRET_TOKEN=changeme_changeme_changeme",
        "client_secret: example-client-secret-value",
    ] {
        assert!(detect_secrets(line).is_empty(), "{line}");
        assert!(
            !crate::detect_all(line).is_empty(),
            "{line} is still seen as a twin"
        );
    }
}

#[test]
fn json_escaped_named_literal_is_a_secret() {
    // A snippet field carrying JSON source: the quotes are `\"` inside the outer string.
    let value = body(40, ALNUM, 86);
    let inner = format!("{{\"apiKey\": \"{value}\"}}");
    let escaped = inner.replace('"', "\\\"");
    let tree = serde_json::json!({"snippet": inner});
    assert_eq!(crate::find_secrets_in_json(&tree).len(), 1);
    let found = detect_secrets(&escaped);
    assert_eq!(
        found.len(),
        1,
        "escaped form: {}",
        escaped.replace(&value, "<v>")
    );
    assert!(!redact(&escaped).text.contains(&value));
}

#[test]
fn hyphenated_uuid_shaped_token_on_a_credential_named_identifier_is_a_secret() {
    let uuid = format!(
        "{}-{}-{}-{}-{}",
        body(8, HEX, 91),
        body(4, HEX, 92),
        body(4, HEX, 93),
        body(4, HEX, 94),
        body(12, HEX, 95)
    );
    assert!(
        uuid.chars().filter(char::is_ascii_digit).count() > 2,
        "fixture guard"
    );
    for text in [
        format!("const apiToken = \"{uuid}\";"),
        format!("{{\"api_token\": \"{uuid}\"}}"),
        format!("API_TOKEN={uuid}"),
        format!("service.credential: {uuid}"),
    ] {
        let found = detect_secrets(&text);
        assert_eq!(found.len(), 1, "{}", text.replace(&uuid, "<v>"));
        assert!(!redact(&text).text.contains(&uuid));
    }
}

#[test]
fn supabase_secret_body_may_contain_hyphens_but_not_start_with_one() {
    let hyphenated = format!(
        "sb_secret_{}-{}-{}",
        body(12, ALNUM, 96),
        body(10, ALNUM, 97),
        body(10, ALNUM, 98)
    );
    let found = detect_secrets(&format!("KEY={hyphenated}"));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].shape, SecretShape::SupabaseSecretKey);
    assert_eq!(
        &hyphenated,
        &format!("KEY={hyphenated}")[found[0].value.clone()],
        "whole body"
    );
    assert!(!redact(&hyphenated).text.contains(&hyphenated));
    // Prose seen in the portal export: a hyphen right after the prefix is not a key.
    assert!(crate::detect_all("a key with the sb_secret_-prefixed shape").is_empty());
}

#[test]
fn padded_jwt_segments_match() {
    let token = jwt(Some("service_role"));
    let mut parts: Vec<String> = token.split('.').map(str::to_string).collect();
    parts[0].push('=');
    parts[1].push_str("==");
    parts[2].push('=');
    let padded = parts.join(".");
    let found = detect_secrets(&padded);
    assert_eq!(found.len(), 1);
    assert_eq!(
        &padded[found[0].value.clone()],
        padded,
        "whole token, padding included"
    );
    assert_eq!(
        found[0].shape,
        SecretShape::SupabaseServiceRoleJwt,
        "role still decoded"
    );
}

// ---- leak hunt: PEM as quoted line lists -----------------------------------------------------

fn pem_lines(qualifier: &str, body_lines: usize) -> Vec<String> {
    crate::fixtures::pem_block(qualifier, body_lines)
        .lines()
        .map(str::to_string)
        .collect()
}

fn assert_pem_redacted_cleanly(text: &str, lines: &[String]) {
    let out = redact(text);
    assert_eq!(out.redactions.len(), 1, "one PEM redaction");
    for line in &lines[1..lines.len() - 1] {
        assert!(!out.text.contains(line.as_str()), "a key line survived");
    }
    assert!(out.text.contains(&lines[0]) && out.text.contains(&lines[lines.len() - 1]));
    assert!(out.text.contains("private key material"));
    assert_eq!(out.text.matches('\n').count(), text.matches('\n').count());
    let again = redact(&out.text);
    assert_eq!(again.text, out.text, "idempotent");
    assert!(again.redactions.is_empty());
    assert!(detect_secrets(&out.text).is_empty());
}

#[test]
fn pem_as_a_json_string_array_is_one_secret() {
    let lines = pem_lines("RSA ", 3);
    let items: Vec<String> = lines.iter().map(|l| format!("    \"{l}\"")).collect();
    let text = format!("{{\n  \"private_key\": [\n{}\n  ]\n}}", items.join(",\n"));
    assert_eq!(detect_secrets(&text).len(), 1);
    assert_pem_redacted_cleanly(&text, &lines);
}

#[test]
fn pem_as_a_minified_json_array_is_one_secret() {
    let lines = pem_lines("", 2);
    let items: Vec<String> = lines.iter().map(|l| format!("\"{l}\"")).collect();
    let text = format!("{{\"private_key\":[{}]}}", items.join(","));
    assert_eq!(detect_secrets(&text).len(), 1);
    assert_pem_redacted_cleanly(&text, &lines);
}

#[test]
fn pem_as_a_yaml_list_is_one_secret() {
    let lines = pem_lines("EC ", 3);
    let items: Vec<String> = lines.iter().map(|l| format!("  - {l}")).collect();
    let text = format!("private_key:\n{}\nafter: true", items.join("\n"));
    assert_eq!(detect_secrets(&text).len(), 1);
    assert_pem_redacted_cleanly(&text, &lines);
}

#[test]
fn pem_list_without_a_footer_is_still_key_material() {
    // Truncated by a cap: the footer line is missing.
    let lines = pem_lines("", 3);
    for (open, close) in [("    \"", "\","), ("  - ", "")] {
        let body_lines: Vec<String> = lines[..lines.len() - 1]
            .iter()
            .map(|l| format!("{open}{l}{close}"))
            .collect();
        let text = body_lines.join("\n");
        assert_eq!(detect_secrets(&text).len(), 1, "unfooted list {open:?}");
        assert_pem_redacted_cleanly_unfooted(&text, &lines);
    }
}

fn assert_pem_redacted_cleanly_unfooted(text: &str, lines: &[String]) {
    let out = redact(text);
    for line in &lines[1..lines.len() - 1] {
        assert!(!out.text.contains(line.as_str()), "a key line survived");
    }
    assert_eq!(out.text.matches('\n').count(), text.matches('\n').count());
    assert_eq!(redact(&out.text).text, out.text, "idempotent");
}

#[test]
fn string_concatenation_of_the_markers_is_not_key_material() {
    let text = "let both = \"-----BEGIN PRIVATE KEY-----\" + \"-----END PRIVATE KEY-----\";";
    assert!(detect_secrets(text).is_empty());
    assert_eq!(redact(text).text, text);
    let list = "[\"-----BEGIN PRIVATE KEY-----\", \"-----END PRIVATE KEY-----\"]";
    assert!(detect_secrets(list).is_empty());
}

// ---- a body line seen alone ------------------------------------------------------------------

#[test]
fn every_body_line_of_a_generated_key_is_a_key_body_line_plain_or_decorated() {
    let block = crate::fixtures::pem_block("RSA ", 5);
    let body: Vec<&str> = block.lines().filter(|l| !l.starts_with("-----")).collect();
    assert!(!body.is_empty());
    for line in body {
        for shown in [
            line.to_string(),
            format!("  {line}  "),
            format!("\"{line}\","),
            format!("  - {line}"),
            format!("  - \"{line}\""),
            format!("    '{line}',"),
        ] {
            assert!(crate::is_key_body_line(&shown), "{shown}");
        }
    }
}

#[test]
fn a_short_padded_final_line_is_a_key_body_line_but_words_and_code_are_not() {
    for yes in [
        "kQ==",
        "  - \"Zm9v+A==\",",
        "AbCd+/12=",
        "\"abcdEFGH1234ijkl\",",
    ] {
        assert!(crate::is_key_body_line(yes), "{yes}");
    }
    for no in [
        "",
        "foo",
        "build_sql",
        "let q = build_sql(x);",
        "fn main() {",
        "somethingLongButOnlyLetters",
        "shortword",
        "-----BEGIN RSA PRIVATE KEY-----",
        "AB",
        "a.b.c.d.e.f.g.h.i.j.k.l.m.n.o.p",
    ] {
        assert!(!crate::is_key_body_line(no), "{no:?}");
    }
}
