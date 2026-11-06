//! The model-input gate: redacts while the switch is on, leaves clean text byte for byte, and
//! keeps positions inside the text (the prompt-cache breakpoints) pointing at the same place.

use std::borrow::Cow;

use crate::fixtures::{planted, planted_all};
use crate::model_input::{redact_for_model_with, redact_with_offsets_using};
use crate::SecretShape;

#[test]
fn every_shape_is_redacted_while_the_switch_is_on() {
    for fixture in planted_all() {
        let out = redact_for_model_with(&fixture.text, true);
        assert!(
            !out.contains(&fixture.value),
            "{:?}: the value survived the model gate",
            fixture.shape
        );
    }
}

#[test]
fn text_with_no_secret_comes_back_as_the_same_bytes() {
    let clean = "fn main() {\n    println!(\"hello\");\n}\nsb_secret_\u{2026}WvXy (51 chars)\n";
    let out = redact_for_model_with(clean, true);
    assert!(matches!(out, Cow::Borrowed(_)), "no allocation, no change");
    assert_eq!(out, clean);
}

#[test]
fn an_already_redacted_text_passes_through_unchanged() {
    for fixture in planted_all() {
        let once = redact_for_model_with(&fixture.text, true).into_owned();
        let twice = redact_for_model_with(&once, true);
        assert_eq!(
            twice, once,
            "{:?}: the gate is not idempotent",
            fixture.shape
        );
        assert!(matches!(twice, Cow::Borrowed(_)), "{:?}", fixture.shape);
    }
}

#[test]
fn the_switch_off_sends_the_text_as_it_is() {
    let fixture = planted(SecretShape::StripeLiveSecretKey);
    let out = redact_for_model_with(&fixture.text, false);
    assert_eq!(out, fixture.text);
}

#[test]
fn offsets_after_a_replaced_value_move_by_the_change_in_length() {
    let fixture = planted(SecretShape::SupabaseSecretKey);
    let text = format!("head\n{}\ntail of the prompt", fixture.text);
    let tail_at = text.find("tail of").expect("tail");
    let (redacted, moved) = redact_with_offsets_using(&text, &[5, tail_at], true);
    assert!(!redacted.contains(&fixture.value));
    assert_eq!(moved[0], 5, "an offset before the value does not move");
    assert_eq!(
        &redacted[moved[1]..],
        "tail of the prompt",
        "the offset after the value still marks the start of the tail"
    );
}

#[test]
fn an_offset_inside_a_value_lands_on_the_start_of_its_replacement() {
    let fixture = planted(SecretShape::StripeLiveSecretKey);
    let text = format!("a {}", fixture.text);
    let inside = text.find(&fixture.value).expect("value") + 6;
    let (redacted, moved) = redact_with_offsets_using(&text, &[inside], true);
    assert!(redacted.is_char_boundary(moved[0]));
    assert!(
        moved[0] <= redacted.len(),
        "the moved offset stays inside the text"
    );
    assert!(
        !redacted[..moved[0]].contains(&fixture.value),
        "no part of the value is left before the offset"
    );
}

#[test]
fn offsets_are_untouched_when_nothing_is_replaced() {
    let text = "nothing secret here, just prose";
    let (out, moved) = redact_with_offsets_using(text, &[0, 7, 31], true);
    assert!(matches!(out, Cow::Borrowed(_)));
    assert_eq!(moved, vec![0, 7, 31]);
}
