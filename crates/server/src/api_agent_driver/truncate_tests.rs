//! `truncate_output` cuts a tool result to a byte budget. A byte budget can land inside a
//! multi-byte character; slicing there panics and kills the run.

use super::truncate_output;

#[test]
fn a_cut_inside_a_multi_byte_character_backs_up_to_the_boundary() {
    // 70,000 bytes of a three-byte character; 65,536 is not a multiple of 3, so the byte
    // budget lands inside a character.
    let s = "\u{20AC}".repeat(70_000 / 3);
    let out = truncate_output(s, 65_536);
    let kept = out.split("\n... (output truncated").next().unwrap();
    assert!(kept.len() <= 65_536, "kept {} bytes", kept.len());
    assert!(
        kept.chars().all(|c| c == '\u{20AC}'),
        "no character was split in two"
    );
    assert!(out.contains("(output truncated at 65536 bytes)"), "{out}");
}

#[test]
fn a_short_string_is_returned_whole() {
    assert_eq!(truncate_output("abc".to_string(), 10), "abc");
}

#[test]
fn a_cut_on_a_boundary_keeps_exactly_the_budget() {
    let out = truncate_output("a".repeat(100), 64);
    assert!(out.starts_with(&"a".repeat(64)));
    assert!(!out.starts_with(&"a".repeat(65)));
}
