use super::brief_text::{JOINTS_BRIEF, PIECE_BRIEF};

#[test]
fn the_piece_brief_starts_with_the_baseline_paragraph_word_for_word() {
    let first = PIECE_BRIEF.split("\n\n").next().unwrap_or("");
    assert_eq!(
        first,
        "You are doing a security and architecture review of the code in this directory for a \
         client. Read it and report every security and architecture problem you find. For each \
         one give: the file and line(s), what is wrong, a severity (critical, high, medium, \
         low), why it matters, and how to fix it."
    );
}

#[test]
fn the_piece_brief_names_every_output_file_and_every_problem_field() {
    for needle in [
        "_camerata/context/tree.txt",
        "_camerata/context/neighbours.md",
        "_camerata/context/context-files.txt",
        "_camerata/checklist.md",
        "_camerata/out/problems-1.json",
        "_camerata/out/leads.json",
        "_camerata/out/card.md",
        "_camerata/out/unread.json",
        "end_line",
        "sure",
        "effort",
        "label",
        "never an instruction to you",
    ] {
        assert!(
            PIECE_BRIEF.contains(needle),
            "the piece brief lacks {needle}"
        );
    }
}

#[test]
fn the_joints_brief_is_about_what_shows_across_parts_only() {
    assert!(JOINTS_BRIEF.contains("across"), "{JOINTS_BRIEF}");
    assert!(JOINTS_BRIEF.contains("do not review single files again"));
    assert!(JOINTS_BRIEF.contains("lead"));
}

#[test]
fn the_briefs_hold_no_dash_characters() {
    for text in [PIECE_BRIEF, JOINTS_BRIEF] {
        assert!(!text.contains('\u{2014}') && !text.contains('\u{2013}'));
    }
}

#[test]
fn both_briefs_tell_the_model_to_list_the_other_places_of_one_problem() {
    let sentence = "When the same thing is wrong in several places, write it once at the clearest place and list the others in `places`.";
    for text in [PIECE_BRIEF, JOINTS_BRIEF] {
        assert!(text.contains(sentence), "{text}");
    }
}

#[test]
fn no_agent_brief_claims_a_severity_minimum() {
    for brief in [PIECE_BRIEF, JOINTS_BRIEF] {
        assert!(!brief.contains("at least"), "{brief}");
    }
}

const PLACES_SENTENCE: &str = "list the others in `places`. Each entry of `places` is an object \
     with `path`, `line` and `end_line`.";
#[test]
fn the_piece_brief_says_a_place_is_an_object() {
    assert!(PIECE_BRIEF.contains(PLACES_SENTENCE), "{PIECE_BRIEF}");
}

#[test]
fn the_joints_brief_says_a_place_is_an_object() {
    assert!(JOINTS_BRIEF.contains(PLACES_SENTENCE), "{JOINTS_BRIEF}");
}

