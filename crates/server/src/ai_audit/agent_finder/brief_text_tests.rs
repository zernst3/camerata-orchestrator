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
const ONE_FAULT_SENTENCE: &str = "Each entry of `places` is an object with `path`, `line` and \
     `end_line`. When two different things are wrong, write two problems, each with its own \
     severity, even in the same place.";

#[test]
fn the_piece_brief_says_a_place_is_an_object() {
    assert!(PIECE_BRIEF.contains(PLACES_SENTENCE), "{PIECE_BRIEF}");
}

#[test]
fn the_joints_brief_says_a_place_is_an_object() {
    assert!(JOINTS_BRIEF.contains(PLACES_SENTENCE), "{JOINTS_BRIEF}");
}

#[test]
fn the_piece_brief_says_two_faults_are_two_problems() {
    assert!(PIECE_BRIEF.contains(ONE_FAULT_SENTENCE), "{PIECE_BRIEF}");
}

#[test]
fn the_joints_brief_says_two_faults_are_two_problems() {
    assert!(JOINTS_BRIEF.contains(ONE_FAULT_SENTENCE), "{JOINTS_BRIEF}");
}

const BUSINESS_FIELD: &str = "`why` (why it matters technically: what breaks and under what \
    conditions), `business` (why it matters to the client's business: what can go wrong for the \
    product, its data, its users or the people who work on it, and when it starts to matter. \
    Pick the one consequence that fits, such as data that quietly goes wrong, the cost of \
    changing it later, someone new working on the code, an outside review of the code, the \
    running bill as usage grows, AI coding tools copying the pattern, or exposure of data or \
    access. Say what can happen, never what will happen, and give no figures, times or costs \
    the code does not show.)";

#[test]
fn the_piece_brief_field_list_describes_why_and_business() {
    assert!(PIECE_BRIEF.contains(BUSINESS_FIELD), "{PIECE_BRIEF}");
    // The first paragraph keeps its words; the new text is in the field list only.
    let first = PIECE_BRIEF.split("\n\n").next().unwrap_or("");
    assert!(!first.contains("business"));
}

#[test]
fn the_joints_brief_field_list_names_business() {
    assert!(JOINTS_BRIEF.contains("`why`, `business`, `fix`"), "{JOINTS_BRIEF}");
}

const UNSURE_SENTENCE: &str = "When `sure` is `unsure`, add `unsure_why`: one sentence saying what the code you could see does not settle.";

#[test]
fn both_briefs_ask_for_unsure_why_inside_the_sure_description() {
    let at = PIECE_BRIEF.find("`sure` (`yes`").expect("the sure field");
    let sentence = PIECE_BRIEF.find(UNSURE_SENTENCE).expect("the sentence");
    let effort = PIECE_BRIEF.find("`effort` to fix").expect("the effort field");
    assert!(at < sentence && sentence < effort, "{PIECE_BRIEF}");
    assert!(PIECE_BRIEF.contains("does not settle it. When `sure` is `unsure`"));
    // The joints brief names the fields in a list; the sentence follows that list.
    assert!(JOINTS_BRIEF.contains(UNSURE_SENTENCE), "{JOINTS_BRIEF}");
    assert!(JOINTS_BRIEF.contains("`sure`, `unsure_why`, `effort`"), "{JOINTS_BRIEF}");
}
