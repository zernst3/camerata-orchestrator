//! `truncate_output` cuts tool output at a byte budget. The cut must land on a character
//! boundary: a file whose multi-byte character straddles the budget must not panic the run.

use super::truncate_output;

#[test]
fn truncate_output_backs_off_to_a_character_boundary_at_the_cut() {
    // "é" is two bytes (0xC3 0xA9). Three ASCII bytes, then "é": byte 4 is inside the "é".
    let text = "abcé and more".to_string();
    let result = std::panic::catch_unwind(|| truncate_output(text.clone(), 4));
    let out = result.expect("truncating inside a multi-byte character must not panic");
    assert!(
        out.starts_with("abc\n"),
        "cut backs off to before the `é`: {out:?}"
    );
    assert!(
        !out.contains('é'),
        "a partial character is never emitted: {out:?}"
    );
    assert!(
        out.contains("output truncated"),
        "the cut is still announced: {out:?}"
    );
}

#[test]
fn truncate_output_keeps_a_whole_multi_byte_character_that_ends_at_the_cut() {
    // "abcé" is exactly five bytes, so a five-byte budget keeps it whole.
    let out = truncate_output("abcé tail".to_string(), 5);
    assert!(out.starts_with("abcé\n"), "{out:?}");
}

#[test]
fn truncate_output_leaves_short_text_alone() {
    assert_eq!(truncate_output("日本語".to_string(), 64), "日本語");
}
