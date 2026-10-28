//! The span and context machinery behind the Firebase `apiKey` twin: which `{...}` or `(...)`
//! encloses a key, with strings, comments and division skipped correctly.
//!
//! Written against the mutants `cargo mutants -p camerata-secrets` left alive on 2026-10-28
//! (docs/MUTATION_TESTING.md): the twin tests in [`super::twins`] never put a bracket inside a
//! comment or string, a `/` that is division, a nested object, or a Maps object BEFORE the
//! Firebase config, so the bracket matcher could lose any of those and still pass. Each case
//! below is either a Maps key that a too-wide span would wrongly turn into a twin (a real key
//! left readable in a client report), or a Firebase key that a too-narrow span would turn into
//! a secret.

use super::twins::{assert_firebase_twin, assert_google_secret, google_key, js_config};
use crate::{detect_secrets, redact};

// ---- a Maps key after a Firebase config: brackets before the value -----------------------

#[test]
fn a_closing_brace_in_a_line_comment_does_not_close_the_maps_object() {
    let maps = google_key(201);
    let text = format!(
        "{}const maps = {{\n  // a }} in a comment closes nothing\n  apiKey: \"{maps}\",\n}};\n",
        js_config(&google_key(202))
    );
    assert_google_secret(&text, &maps);
}

#[test]
fn a_closing_brace_in_a_block_comment_does_not_close_the_maps_object() {
    let maps = google_key(203);
    let text = format!(
        "{}const maps = {{\n  /* }} */\n  apiKey: \"{maps}\",\n}};\n",
        js_config(&google_key(204))
    );
    assert_google_secret(&text, &maps);
}

#[test]
fn a_division_before_the_maps_object_is_not_a_comment() {
    let maps = google_key(205);
    let text = format!(
        "{}const half = 1 / 2; const maps = {{\n  apiKey: \"{maps}\",\n}};\n",
        js_config(&google_key(206))
    );
    assert_google_secret(&text, &maps);
}

#[test]
fn a_comment_at_the_very_start_of_the_text_is_read_safely() {
    let key = google_key(207);
    assert_google_secret(
        &format!("// Maps loader }}\nconst maps = {{ apiKey: \"{key}\" }};\n"),
        &key,
    );
    assert_google_secret(
        &format!("/* }} */\nconst maps = {{ apiKey: \"{key}\" }};\n"),
        &key,
    );
}

// ---- a Maps key BEFORE a Firebase config: brackets after the value ------------------------

#[test]
fn a_maps_object_ends_at_its_own_closer_not_at_the_window_edge() {
    let maps = google_key(208);
    let text = format!(
        "const maps = {{ apiKey: \"{maps}\" }};\n{}",
        js_config(&google_key(209))
    );
    assert_google_secret(&text, &maps);
}

#[test]
fn a_nested_object_inside_the_maps_object_keeps_the_depth_balanced() {
    let maps = google_key(210);
    let text = format!(
        "const maps = {{ apiKey: \"{maps}\", opts: {{ a: 1 }} }};\n{}",
        js_config(&google_key(211))
    );
    assert_google_secret(&text, &maps);
}

#[test]
fn a_division_after_the_maps_key_does_not_swallow_its_closer() {
    let maps = google_key(212);
    let text = format!(
        "const maps = {{ apiKey: \"{maps}\", ratio: 1 / 2 }};\n{}",
        js_config(&google_key(213))
    );
    assert_google_secret(&text, &maps);
}

// ---- a Firebase key whose config has brackets after it ------------------------------------

fn firebase_config_with(key: &str, line: &str) -> String {
    format!(
        "const firebaseConfig = {{\n  apiKey: \"{key}\",\n  {line}\n  authDomain: \"x.firebaseapp.com\",\n  projectId: \"x\",\n}};\n"
    )
}

#[test]
fn a_nested_object_after_the_firebase_key_does_not_end_the_config_early() {
    assert_firebase_twin(&firebase_config_with(&google_key(214), "extra: { a: 1 },"));
}

#[test]
fn a_closer_inside_a_string_does_not_end_the_config_early() {
    assert_firebase_twin(&firebase_config_with(&google_key(215), "note: \"}\","));
}

#[test]
fn a_closer_inside_a_line_comment_does_not_end_the_config_early() {
    assert_firebase_twin(&firebase_config_with(&google_key(216), "// }"));
}

#[test]
fn a_closer_inside_a_block_comment_does_not_end_the_config_early() {
    assert_firebase_twin(&firebase_config_with(&google_key(217), "/* } */"));
}

// ---- what counts as a Firebase initialisation span ----------------------------------------

#[test]
fn a_call_that_is_not_a_firebase_initialiser_is_not_an_init_span() {
    let key = google_key(218);
    assert_google_secret(
        &format!("const client = createClient({{ key: \"{key}\", projectId: \"x\" }});"),
        &key,
    );
}

#[test]
fn an_object_assigned_to_a_binding_not_named_firebase_config_is_not_an_init_span() {
    let key = google_key(219);
    assert_google_secret(
        &format!("const cfg = {{ key: \"{key}\", projectId: \"x\" }};"),
        &key,
    );
}

#[test]
fn a_firebase_named_key_with_only_a_plain_sibling_is_a_twin() {
    // No Firebase-prefixed sibling, one plain one: the two counts are summed, never subtracted.
    assert_firebase_twin(&format!(
        "FIREBASE_API_KEY={}\nPROJECT_ID=x\n",
        google_key(220)
    ));
}

// ---- the context window --------------------------------------------------------------------

#[test]
fn the_value_offset_is_window_relative_when_text_precedes_the_window() {
    // More than a window's worth of text before the value: the offset of the value inside its
    // window is no longer its offset in the text.
    let maps = google_key(221);
    let padding = "// padding\n".repeat(100);
    let text = format!(
        "{padding}{}const maps = {{ apiKey: \"{maps}\" }};\n",
        js_config(&google_key(222))
    );
    assert_google_secret(&text, &maps);
}

#[test]
fn a_newline_at_the_start_of_the_text_starts_the_value_line() {
    let key = google_key(223);
    assert_google_secret(&format!("\nconst key = \"{key}\";"), &key);
}

#[test]
fn window_edges_inside_a_multibyte_character_are_moved_to_a_boundary() {
    // A window edge (800 bytes either side) and the line-prefix edge (200 bytes) land inside a
    // two-byte character for one of these two paddings.
    for extra in ["", "x"] {
        let key = google_key(224);
        let wide = "\u{e9}".repeat(500);
        let text = format!("{wide}{extra} key = \"{key}\" {wide}");
        let secrets = detect_secrets(&text);
        assert_eq!(secrets.len(), 1, "padding {extra:?}");
        assert_eq!(&text[secrets[0].value.clone()], key);
        assert!(!redact(&text).text.contains(&key));
    }
}
