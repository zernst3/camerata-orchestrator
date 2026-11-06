//! The exact edges of the twin rules that decide whether a value stays readable: where a
//! placeholder word counts as delimited inside a vendor body, what an env-var name is, what a
//! readable name is, and what a format placeholder is. A rule that drifts wide here leaves a
//! real secret readable in a client artifact.
//!
//! Written against the mutants `cargo mutants -p camerata-secrets` left alive on 2026-10-28
//! (docs/MUTATION_TESTING.md). Bodies are hand-written so no other placeholder word hides in
//! them (`your`, `fake`, `mock`, `todo`, `xxxx` and the rest).

use crate::{detect_all, detect_secrets, SecretShape, Twin};

/// The only match in `text` is a secret of `shape`.
fn assert_secret(text: &str, shape: SecretShape) {
    let secrets = detect_secrets(text);
    assert_eq!(secrets.len(), 1, "expected one secret in: {text}");
    assert_eq!(secrets[0].shape, shape, "{text}");
}

/// The only match in `text` is a twin.
fn twin_of(text: &str) -> Twin {
    let all = detect_all(text);
    assert_eq!(all.len(), 1, "expected one match in: {text}");
    assert!(
        detect_secrets(text).is_empty(),
        "expected a twin in: {text}"
    );
    all[0].twin.clone().expect("a twin")
}

/// `ident = "<parts joined>"`, assembled at runtime: no quoted run in this file is long enough to
/// be a secret-shaped literal itself (`source_hygiene`).
fn literal(ident: &str, parts: &[&str]) -> String {
    format!("{ident} = \"{}\"", parts.concat())
}

/// A Google API key: `AIza` plus a 35-character body.
fn google(body: &str) -> String {
    assert_eq!(body.len(), 35, "{body}");
    format!("const k = \"AIza{body}\";")
}

// ---- placeholder words inside a mixed-case vendor body -----------------------------------

#[test]
fn an_upper_case_word_between_underscores_is_delimited() {
    assert_eq!(
        twin_of(&google("Qz9xWv3Lm_FAKE_Tn7Rb2Ks4Yp6Hd8Jc1GB")),
        Twin::PlaceholderWord("fake")
    );
}

#[test]
fn a_word_with_a_case_change_on_one_side_only_is_not_delimited() {
    // `qfakeZ`: a case change after the word but none before it.
    assert_secret(
        &google("Wv3Lm9Tn7Rb2qfakeZKs4Yp6Hd8Jc1GBn5P"),
        SecretShape::GoogleApiKey,
    );
}

#[test]
fn a_word_at_the_start_of_the_body_is_delimited_on_that_side() {
    assert_eq!(
        twin_of(&google("fakeQz9xWv3Lm2Tn7Rb2Ks4Yp6Hd8Jc1GBn")),
        Twin::PlaceholderWord("fake")
    );
}

#[test]
fn a_word_at_the_end_of_the_body_is_delimited_on_that_side() {
    assert_eq!(
        twin_of(&google("Qz9xWv3Lm2Tn7Rb2Ks4Yp6Hd8Jc1GBQfake")),
        Twin::PlaceholderWord("fake")
    );
}

#[test]
fn the_right_boundary_compares_the_word_last_byte_with_its_neighbour() {
    // `9fakeQZ`: digit then word (boundary), word then upper case (boundary). The two bytes after
    // the word are both upper case, so comparing them instead would find no boundary.
    assert_eq!(
        twin_of(&google("Wv3Lm2Tn7Rb2Ks4Yp6H9fakeQZd8Jc1GBn5")),
        Twin::PlaceholderWord("fake")
    );
}

// ---- env-var names --------------------------------------------------------------------------

#[test]
fn an_upper_case_value_with_no_underscore_is_not_an_env_name() {
    assert_secret(
        &literal("password", &["QWRTPLKJHGFD", "SZXCVBNMQWRTPL"]),
        SecretShape::NamedSecretLiteral,
    );
}

#[test]
fn a_doubled_or_trailing_underscore_is_not_an_env_name() {
    assert_secret(
        &literal("password", &["QWRT__PLKJHGF", "DSZXCVBNMQWRT"]),
        SecretShape::NamedSecretLiteral,
    );
    assert_secret(
        &literal("password", &["QWRT_PLKJHGFD", "SZXCVBNMQWRT_"]),
        SecretShape::NamedSecretLiteral,
    );
    assert_eq!(
        twin_of(&literal("password", &["QWRT_PLKJHGFDSZX", "_CVBNMQWRT"])),
        Twin::EnvNameLiteral
    );
}

// ---- readable names -------------------------------------------------------------------------

#[test]
fn mixed_case_segments_are_not_a_readable_name() {
    assert_secret(
        &literal("token", &["xQzvBnkTrW-pLmhJ", "gFdSa-PkIuVcZqWr"]),
        SecretShape::NamedSecretLiteral,
    );
}

#[test]
fn a_lower_then_upper_segment_is_not_a_name_word() {
    assert_secret(
        &literal("token", &["qWRTPLKJHG-zXCVBN", "MLKJ-hGFDSPLKJH"]),
        SecretShape::NamedSecretLiteral,
    );
    assert_eq!(
        twin_of(&literal("token", &["Qwrtplkjhg-ZXCVBN", "MLKJ-hgfdsplkjh"])),
        Twin::ReadableName
    );
}

// ---- format placeholders --------------------------------------------------------------------

#[test]
fn a_brace_field_must_name_an_identifier_to_be_a_placeholder() {
    assert_secret(
        &["postgres://admin:", "{9db}", "@db.internal:5432/app"].concat(),
        SecretShape::ConnectionStringPassword,
    );
    assert_eq!(
        twin_of(&["postgres://admin:", "{password}", "@db.internal:5432/app"].concat()),
        Twin::TemplateToken
    );
}
