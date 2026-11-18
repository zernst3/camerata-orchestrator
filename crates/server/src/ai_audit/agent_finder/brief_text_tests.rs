use super::brief_text::{repair_reminder, JOINTS_BRIEF, PIECE_BRIEF, PROBLEM_FIELDS};

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
    running bill as usage grows, AI coding tools copying the pattern or exposure of data or \
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
    assert!(
        JOINTS_BRIEF.contains("`why`, `business`, `fix`"),
        "{JOINTS_BRIEF}"
    );
}

const UNSURE_SENTENCE: &str = "When `sure` is `unsure`, add `unsure_why`: one sentence saying what the code you could see does not settle.";

#[test]
fn both_briefs_ask_for_unsure_why_inside_the_sure_description() {
    let at = PIECE_BRIEF.find("`sure` (`yes`").expect("the sure field");
    let sentence = PIECE_BRIEF.find(UNSURE_SENTENCE).expect("the sentence");
    let effort = PIECE_BRIEF
        .find("`effort` to fix")
        .expect("the effort field");
    assert!(at < sentence && sentence < effort, "{PIECE_BRIEF}");
    assert!(PIECE_BRIEF.contains("does not settle it. When `sure` is `unsure`"));
    // The joints brief names the fields in a list; the sentence follows that list.
    assert!(JOINTS_BRIEF.contains(UNSURE_SENTENCE), "{JOINTS_BRIEF}");
    assert!(
        JOINTS_BRIEF.contains("`sure`, `unsure_why`, `effort`"),
        "{JOINTS_BRIEF}"
    );
}

#[test]
fn the_finder_prompt_has_no_serial_comma_before_its_last_example() {
    assert!(!PIECE_BRIEF.contains("copying the pattern, or exposure"));
    assert!(PIECE_BRIEF.contains("copying the pattern or exposure"));
}

#[test]
fn every_brief_that_writes_problems_bans_secret_values_and_attack_recipes() {
    for (name, brief) in [("piece", PIECE_BRIEF), ("joints", JOINTS_BRIEF)] {
        for needle in [
            "Never copy a credential",
            "even masked, partial or public",
            "write only the variable or key name",
            "stopping before the `=` or `:`",
            "Still report it as a problem.",
            "Name the variable and the file, never the value",
            "step-by-step exploit instructions",
            "ready-to-run attack code",
        ] {
            assert!(brief.contains(needle), "the {name} brief lacks {needle}");
        }
    }
}

#[test]
fn the_shared_field_list_is_in_the_piece_and_joints_briefs() {
    assert!(!PROBLEM_FIELDS.is_empty());
    for field in PROBLEM_FIELDS.split(", ") {
        for (name, brief) in [("piece", PIECE_BRIEF), ("joints", JOINTS_BRIEF)] {
            assert!(brief.contains(field), "the {name} brief lacks {field}");
        }
    }
}

#[test]
fn the_repair_reminder_lists_every_field_and_forbids_dropping_one() {
    let text = repair_reminder();
    for field in PROBLEM_FIELDS.split(", ") {
        assert!(text.contains(field), "the reminder lacks {field}: {text}");
    }
    assert!(text.contains("may not be dropped"), "{text}");
    assert!(text.contains("stopping before the `=` or `:`"), "{text}");
    assert!(!text.contains("value omitted"), "{text}");
}

#[test]
fn no_brief_or_reminder_tells_the_agent_to_write_a_value_omitted_marker() {
    for text in [PIECE_BRIEF, JOINTS_BRIEF, &repair_reminder()] {
        assert!(!text.contains("value omitted"), "{text}");
    }
}

#[test]
fn the_piece_codes_description_names_the_secret_line_exception() {
    let at = PIECE_BRIEF.find("`code` (").expect("the code field");
    let end = PIECE_BRIEF[at..].find("`what`").expect("the next field");
    let described = &PIECE_BRIEF[at..at + end];
    assert!(described.contains("copied exactly"), "{described}");
    assert!(
        described.contains("except a line that holds a credential"),
        "{described}"
    );
}

fn secret_row(path: &str, snippet: &str) -> crate::onboard::Finding {
    crate::onboard::Finding {
        repo: "acme/app".to_string(),
        path: path.to_string(),
        line: 3,
        rule_id: "SEC-NO-HARDCODED-SECRETS-1".to_string(),
        severity: "critical".to_string(),
        snippet: snippet.to_string(),
        ..crate::onboard::Finding::default()
    }
}

/// The example the secret rule gives for `code`, taken from the brief itself.
const BRIEF_EXAMPLE: &str = "STRIPE_SECRET_KEY";

#[test]
fn the_brief_example_is_the_bare_name_and_the_engine_keeps_that_row_at_its_severity() {
    use crate::ai_audit::finding_merge::{cap_placeholder_severity, is_placeholder_secret_row};
    assert!(
        PIECE_BRIEF.contains(&format!("(for example `{BRIEF_EXAMPLE}`)")),
        "{PIECE_BRIEF}"
    );
    let mut row = secret_row(".env.production", BRIEF_EXAMPLE);
    assert!(!is_placeholder_secret_row(&row));
    cap_placeholder_severity(&mut row);
    assert_eq!(row.severity, "critical");
}

#[test]
fn the_brief_example_is_found_near_its_line_in_the_real_file() {
    let file = "NODE_ENV=production\nPORT=8080\nSTRIPE_SECRET_KEY=sk_live_abcdefghijklmnop1234\n";
    assert!(crate::onboard::evidence_check::snippet_near_line(
        file,
        BRIEF_EXAMPLE,
        3
    ));
}

#[test]
fn a_real_template_value_in_an_example_file_is_still_capped() {
    use crate::ai_audit::finding_merge::{cap_placeholder_severity, is_placeholder_secret_row};
    for snippet in ["STRIPE_SECRET_KEY=<your-key>", "STRIPE_SECRET_KEY="] {
        let mut row = secret_row(".env.example", snippet);
        assert!(is_placeholder_secret_row(&row), "{snippet}");
        cap_placeholder_severity(&mut row);
        assert_ne!(row.severity, "critical", "{snippet}");
    }
}

pub(super) const SEVERAL_LINES: &str =
    "When several lines hold credentials, `code` is the first name only and the other lines go in `places`.";
pub(super) const NO_NAME: &str = "When a credential has no variable or key name (a PEM block or a key inline in code), `code` is the part of the line before the value.";

#[test]
fn every_brief_and_the_reminder_carry_the_several_lines_and_no_name_rules() {
    for (name, text) in [
        ("piece", PIECE_BRIEF.to_string()),
        ("joints", JOINTS_BRIEF.to_string()),
        ("reminder", repair_reminder()),
    ] {
        assert!(text.contains(SEVERAL_LINES), "{name}: several lines");
        assert!(text.contains(NO_NAME), "{name}: no name");
    }
}

#[test]
fn a_snippet_of_two_names_is_not_found_but_the_first_name_alone_is() {
    let file = "NODE_ENV=production\nSTRIPE_SECRET_KEY=sk_live_abcdefghijklmnop1234\nDB_PASSWORD=hunter2hunter2\n";
    let near = crate::onboard::evidence_check::snippet_near_line;
    assert!(!near(file, "STRIPE_SECRET_KEY\nDB_PASSWORD", 2));
    assert!(near(file, "STRIPE_SECRET_KEY", 2));
}
