//! Every sentence of the client package that carries a count, built from the counts object and
//! nothing else (single counts, design 5.6).
//!
//! A builder here takes `&ReportCounts` (and, where the wording needs it, the review state or
//! whether the project has several repositories), never a loose integer. The PDF data, the
//! workbook and the machine files print these strings as they are; the Typst template does no
//! arithmetic. The release gate rebuilds each one from its own recount of the package's files
//! with these same functions, so the gate shares the TEXT with the surfaces and nothing else: the
//! numbers it feeds in are its own.
//!
//! No em dashes, en dashes or serial commas (client text).

use super::{ReportCounts, ReviewState};

/// A count with thousands commas: `7981` is `7,981`.
pub(crate) fn n(x: usize) -> String {
    let digits = x.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// `"1 file"` or `"2,793 files"`.
pub(crate) fn noun(count: usize, one: &str, many: &str) -> String {
    format!("{} {}", n(count), if count == 1 { one } else { many })
}

fn is_are(count: usize) -> &'static str {
    if count == 1 {
        "is"
    } else {
        "are"
    }
}

fn needs_need(count: usize) -> &'static str {
    if count == 1 {
        "needs"
    } else {
        "need"
    }
}

/// `a`, `b` and `c`: commas between, "and" before the last, no serial comma.
pub(crate) fn join_and(parts: &[String]) -> String {
    match parts.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

/// The sentence of the executive summary: the problems and how they divide (design 5.6). Both
/// review states state the same numbers; a draft says first that nobody has reviewed it, and a
/// reviewed report says how many source rows a reviewer ruled out.
pub(crate) fn narrative(c: &ReportCounts, state: ReviewState) -> String {
    if c.rows.total == 0 {
        return "The scan surfaced no problems to review in this run.".to_string();
    }
    let p = &c.problems;
    let found = if p.total == 0 {
        "no problems".to_string()
    } else {
        noun(p.total, "problem", "problems")
    };
    let mut s = String::new();
    if state.is_draft() {
        s.push_str("This is a draft export: no problem in this run has had a human review. ");
    }
    s.push_str(&format!(
        "The engine analyzed {} and found {found}.",
        noun(c.other.files_scanned, "file", "files")
    ));
    if p.total > 0 {
        s.push_str(&format!(
            " {} {} for action ({} First, {} Next, {} Later), {} {} a reviewer's decision, {} {} information only and {} {} accepted risk.",
            n(p.for_action),
            is_are(p.for_action),
            n(p.first),
            n(p.next),
            n(p.later),
            n(p.needs_decision),
            needs_need(p.needs_decision),
            n(p.information_only),
            is_are(p.information_only),
            n(p.accepted),
            is_are(p.accepted),
        ));
    }
    s.push_str(&format!(
        " {} {} listed separately.",
        noun(
            c.rows.dependency,
            "dependency advisory",
            "dependency advisories"
        ),
        is_are(c.rows.dependency),
    ));
    if !state.is_draft() && c.rows.ruled_out > 0 {
        s.push_str(&format!(
            " {} {} ruled out and left out of this report.",
            noun(c.rows.ruled_out, "source row", "source rows"),
            if c.rows.ruled_out == 1 { "was" } else { "were" },
        ));
    }
    s
}

/// The clause of the next steps (and the only one the gate looks for) that states how many
/// problems need a decision.
pub(crate) fn decision_sentence(c: &ReportCounts) -> String {
    if c.problems.total == 0 {
        return "no problems need a reviewer's decision".to_string();
    }
    format!(
        "{} of {} {} a reviewer's decision",
        n(c.problems.needs_decision),
        noun(c.problems.total, "problem", "problems"),
        needs_need(c.problems.needs_decision),
    )
}

/// The methodology line: the source rows reconciled once, in row words (design 5.6).
pub(crate) fn methodology_line(c: &ReportCounts, state: ReviewState) -> String {
    let (r, p) = (&c.rows, &c.problems);
    // A problem that no row of its own stands for (a place another rule's ruled-out row carries)
    // is not one of the places the problem rows are at.
    let at_rows = p.total.saturating_sub(p.beside_ruled_out);
    let mut problem_rows = format!(
        "{} at {}",
        noun(r.problem, "problem row", "problem rows"),
        noun(at_rows, "place", "places"),
    );
    if p.beside_ruled_out > 0 {
        problem_rows.push_str(&format!(
            " and {} carried by a ruled out row",
            noun(p.beside_ruled_out, "place", "places")
        ));
    }
    let parts = vec![
        problem_rows,
        noun(r.dependency, "dependency advisory", "dependency advisories"),
        noun(r.convention, "convention row", "convention rows"),
        noun(r.not_checked, "not checked row", "not checked rows"),
        format!("{} ruled out", n(r.ruled_out)),
    ];
    let core = format!(
        "{}: {}.",
        noun(r.total, "source row", "source rows"),
        join_and(&parts)
    );
    let review = if state.is_draft() {
        "This export has not yet had a human review."
    } else {
        // Not "reviewed by a human reviewer": a person read only part of the places, and the
        // method text states how many.
        "A person has ruled on part of this export; the Reviewed in person column of the All \
         problems sheet shows which places."
    };
    format!("{core} {review}{}", suppressions_sentence(c))
}

/// The sentence that says how many waivers and baseline entries the client's repository held and
/// that none was honoured; empty when it held none.
pub(crate) fn suppressions_sentence(c: &ReportCounts) -> String {
    let found = c.other.client_suppressions_found;
    if found == 0 {
        return String::new();
    }
    format!(
        " The inspected code held {}; this first inspection did not honour them, so every problem they cover is open.",
        noun(found, "waiver or baseline entry", "waivers and baseline entries"),
    )
}

/// The three steps from here, with the decision sentence in the first.
pub(crate) fn next_steps(c: &ReportCounts, multi_repo: bool, state: ReviewState) -> String {
    let (rescan_object, introduces) = if multi_repo {
        ("repositories", "the codebases introduce")
    } else {
        ("repository", "the codebase introduces")
    };
    let mut status = if c.problems.total == 0 {
        "This run produced no problems, so there is no fix rate to report and no problems need a reviewer's decision.".to_string()
    } else {
        format!("{}.", decision_sentence(c))
    };
    if !state.is_draft() && c.rows.ruled_out > 0 {
        status.push_str(&format!(
            " {} {} ruled out so far.",
            noun(c.rows.ruled_out, "source row", "source rows"),
            if c.rows.ruled_out == 1 { "is" } else { "are" },
        ));
    }
    format!(
        "There are three steps from here. First, the fixes above get done, by the \
         client's own team, an outside contractor or the reviewing engineer. {status} Second, a \
         retest: the reviewing engineer re-scans the {rescan_object} once the fixes are in and \
         signs a short addendum confirming each item is closed. Third, ongoing coverage: a \
         monthly delta rescan plus on-call architect availability for anything new \
         {introduces} between engagements."
    )
}

/// The strip under the cover title: the problems by severity (they add up to the problems), the
/// accepted ones and the dependency advisories.
pub(crate) fn cover_strip(c: &ReportCounts) -> String {
    let (p, s) = (&c.problems, &c.problems.by_severity);
    format!(
        "{} critical · {} high · {} medium · {} low · {} accepted risk · {}",
        n(s.critical),
        n(s.high),
        n(s.medium),
        n(s.low),
        n(p.accepted),
        noun(
            c.rows.dependency,
            "dependency advisory",
            "dependency advisories"
        ),
    )
}

/// The grid's note on the problems that are accepted risk; empty when there are none.
pub(crate) fn grid_accepted_line(c: &ReportCounts) -> String {
    let a = c.problems.accepted;
    if a == 0 {
        return String::new();
    }
    format!(
        "{} {} accepted risk and out of scope for this grid.",
        noun(a, "problem", "problems"),
        if a == 1 { "is" } else { "are" },
    )
}

/// The grid's note on the problems it leaves out, by severity, so the grid closes to the cover;
/// empty when nothing is left out.
pub(crate) fn grid_left_out(c: &ReportCounts) -> String {
    let p = &c.problems;
    let part = |by: &super::counts::SeverityCounts| -> String {
        let items: Vec<String> = [
            ("critical", by.critical),
            ("high", by.high),
            ("medium", by.medium),
            ("low", by.low),
        ]
        .iter()
        .filter(|(_, count)| *count > 0)
        .map(|(sev, count)| format!("{} {sev}", n(*count)))
        .collect();
        join_and(&items)
    };
    let (decide, info) = (
        part(&p.needs_decision_by_severity),
        part(&p.information_only_by_severity),
    );
    // The noun and the verb agree with the number of problems, not with the severities listed.
    let (decide_n, info_n) = (
        p.needs_decision_by_severity.sum(),
        p.information_only_by_severity.sum(),
    );
    let (decide_noun, decide_verb) = if decide_n == 1 {
        ("problem", "needs")
    } else {
        ("problems", "need")
    };
    let (info_noun, info_verb) = if info_n == 1 {
        ("problem", "is")
    } else {
        ("problems", "are")
    };
    let decide_part = format!("{decide} {decide_noun} that {decide_verb} a decision first");
    let info_part = format!("{info} {info_noun} that {info_verb} information only");
    let mut out = match (decide.is_empty(), info.is_empty()) {
        (true, true) => return String::new(),
        (false, true) => format!("Not in this grid: {decide_part}."),
        (true, false) => format!("Not in this grid: {info_part}."),
        (false, false) => format!("Not in this grid: {decide_part} and {info_part}."),
    };
    out.push_str(" The cover counts all of them.");
    out
}

/// What the three things box says when it is empty. With no dependency fix First nothing is due
/// first; otherwise no code problem is, and the dependency fixes that are say so.
pub(crate) fn nothing_first_sentence(dependency_fixes_first: usize) -> String {
    match dependency_fixes_first {
        0 => "No fix is due first this run.".to_string(),
        1 => "No code problem is due first this run. 1 dependency fix is due first; see Dependencies."
            .to_string(),
        k => format!(
            "No code problem is due first this run. {} dependency fixes are due first; see Dependencies.",
            n(k)
        ),
    }
}

/// "and 3 more": the line under a list that shows only its first entries. `more` is the number of
/// entries left out and `words` what the template adds after the number.
pub(crate) fn more_line(more: usize, words: &str) -> String {
    if more == 0 {
        return String::new();
    }
    format!("and {} {words}", n(more))
}

#[cfg(test)]
mod grid_note_tests {
    use super::*;
    use crate::report_export::ReportCounts;

    /// The grid's note agrees in number and verb with the count, and has no comma before "and".
    #[test]
    fn the_grid_note_agrees_with_the_count() {
        let mut one = ReportCounts::default();
        one.problems.needs_decision_by_severity.low = 1;
        assert_eq!(
            grid_left_out(&one),
            "Not in this grid: 1 low problem that needs a decision first. The cover counts all of them."
        );
        let mut two = ReportCounts::default();
        two.problems.needs_decision_by_severity.low = 2;
        assert_eq!(
            grid_left_out(&two),
            "Not in this grid: 2 low problems that need a decision first. The cover counts all of them."
        );
        let mut both = ReportCounts::default();
        both.problems.needs_decision_by_severity.low = 2;
        both.problems.information_only_by_severity.medium = 1;
        let note = grid_left_out(&both);
        assert_eq!(
            note,
            "Not in this grid: 2 low problems that need a decision first and 1 medium problem that is information only. The cover counts all of them."
        );
        assert!(!note.contains(", and") && !note.contains(['\u{2014}', '\u{2013}']));
    }

    /// What the box says when it is empty: nothing is due first, or only dependency fixes are.
    #[test]
    fn the_empty_box_sentence_names_the_dependency_fixes_that_are_due_first() {
        use crate::report_export::classification::total_hours_label_of;
        assert_eq!(
            total_hours_label_of(&[], 0),
            "No fix is due first this run."
        );
        assert_eq!(
            total_hours_label_of(&[], 1),
            "No code problem is due first this run. 1 dependency fix is due first; see Dependencies."
        );
        assert_eq!(
            total_hours_label_of(&[], 2),
            "No code problem is due first this run. 2 dependency fixes are due first; see Dependencies."
        );
        for n in 0..3 {
            let s = total_hours_label_of(&[], n);
            assert!(
                !s.contains(['\u{2014}', '\u{2013}']) && !s.contains(", and"),
                "{s}"
            );
        }
    }
}
