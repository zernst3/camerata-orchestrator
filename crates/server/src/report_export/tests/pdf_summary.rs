//! WO-A: the PDF is a summary at every size, at most 20 pages, in 15 sections. It holds no list
//! of problems, no per-place card or table, no code snippet and no low or info row; every row
//! lives in the workbook and the CSVs. Synthetic fixtures only.

use std::sync::OnceLock;

use camerata_api_types::review::EscalationState;

use super::*;
use crate::onboard::{Finding, SealedReport};
use crate::report_export::{apply_overlay, build_report_json, render_data_json};
use crate::xlsx_export::vocabulary as v;

/// The 15 sections of the redesign's section 1, in order, as the PDF outline names them.
const SECTIONS: [&str; 15] = [
    "Cover",
    "Contents",
    "Executive summary",
    "If you only do three things this week",
    "Area scorecard",
    "Severity by effort matrix",
    "The most serious problems",
    "The other fixes",
    "Your conventions",
    "What is healthy",
    "Dependencies",
    "Next steps",
    "Where the details are",
    "How this was done and its limits",
    "Disclaimer",
];

const MAX_PAGES: usize = 20;
const ESCALATED: usize = 12;
const CLEAN_RULES: usize = 40;

fn options() -> ReportOptions {
    ReportOptions {
        client_name: "Example Client Ltd".to_string(),
        ..ReportOptions::default()
    }
}

/// `problems` problems spread over `rules` rules (each rule is its own Fix: every rule has its
/// own places), in 24 areas. The first `ESCALATED` rules are critical and one place of each is
/// escalated. Every seventh place is a whole file. Details carry the characters the PDF must
/// never print, every place carries a long unbroken file name and every snippet a marker.
fn synthetic_report(problems: usize, rules: usize) -> SealedReport {
    let mut report = crate::release_gate::test_scan_report();
    let mut findings = Vec::with_capacity(problems);
    for i in 0..problems {
        let rule = i % rules;
        let severity = match rule {
            r if r < ESCALATED => "critical",
            r if r % 4 == 1 => "high",
            r if r % 4 == 2 => "medium",
            _ => "low",
        };
        let whole_file = i % 7 == 3;
        let mut f = Finding {
            repo: "owner/repo".to_string(),
            path: format!(
                "src/zone_{rule}/module_{i}_with_a_very_long_unbroken_file_name_{}.rs",
                "x".repeat(40)
            ),
            line: if whole_file { 0 } else { i % 400 + 1 },
            rule_id: format!("SYN-RULE-{rule:03}"),
            severity: severity.to_string(),
            snippet: format!("SNIPPET_MARK_{i}"),
            detail: format!(
                "Input reaches a query \u{2014} unsafe in module {i} \u{00D7} twice. It must be \
                 bound. Then it must be checked."
            ),
            effort: Some(
                if rule.is_multiple_of(3) {
                    "low"
                } else {
                    "medium"
                }
                .to_string(),
            ),
            category: Some(format!("Area {}", rule % 24)),
            fix_specific: Some(
                "Bind the value with the driver. Then add a test that proves it. A third \
                 sentence that must not be printed."
                    .to_string(),
            ),
            located: true,
            ..Finding::default()
        };
        if i < ESCALATED {
            f.review.escalation_state = EscalationState::Escalated;
        }
        findings.push(f);
    }
    report.findings = findings;
    let mut audited: Vec<String> = (0..rules).map(|r| format!("SYN-RULE-{r:03}")).collect();
    audited.extend((0..CLEAN_RULES).map(|c| format!("CLEAN-RULE-{c:02}")));
    report.provenance.audited_rule_ids = audited;
    report.seal()
}

struct Built {
    data: String,
    json: serde_json::Value,
    text: String,
    flat: String,
    pages: usize,
    outline: Vec<String>,
}

fn build_in_thread(problems: usize, rules: usize) -> Built {
    std::thread::spawn(move || {
        let sealed = synthetic_report(problems, rules);
        let reviewed = apply_overlay(&sealed, &[], &[], &[]);
        let built = build_report_json(&reviewed, None, &options());
        let data = render_data_json(&built).expect("data.json");
        let json: serde_json::Value = serde_json::from_str(&data).expect("data.json parses");
        let pdf = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime")
            .block_on(crate::report_export::compile_pdf_from_data(&data))
            .expect("the PDF compiles");
        let text = pdf_extract::extract_text_from_mem(&pdf).expect("the PDF's text extracts");
        let doc = pdf_extract::Document::load_mem(&pdf).expect("the PDF loads");
        let pages = doc.get_pages().len();
        let outline = doc
            .get_toc()
            .map(|t| t.toc.iter().map(|e| e.title.clone()).collect())
            .unwrap_or_default();
        let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
        Built {
            data,
            json,
            text,
            flat,
            pages,
            outline,
        }
    })
    .join()
    .expect("the build thread")
}

/// 5,000 problems over 200 rules: the synthetic report of the work order.
fn big() -> Option<&'static Built> {
    static BIG: OnceLock<Option<Built>> = OnceLock::new();
    BIG.get_or_init(|| camerata_test_env::typst_on_path().then(|| build_in_thread(5_000, 200)))
        .as_ref()
}

/// A report the size of the largest saved package (141 places over 85 rules).
fn fixture_sized() -> Option<&'static Built> {
    static FIXTURE: OnceLock<Option<Built>> = OnceLock::new();
    FIXTURE
        .get_or_init(|| camerata_test_env::typst_on_path().then(|| build_in_thread(141, 85)))
        .as_ref()
}

#[test]
fn the_pdf_is_at_most_twenty_pages_on_the_largest_fixture_package() {
    let Some(b) = fixture_sized() else {
        eprintln!("skipping: typst not on PATH");
        return;
    };
    assert!(
        b.pages <= MAX_PAGES,
        "the fixture-sized PDF is {} pages",
        b.pages
    );
}

#[test]
fn the_pdf_is_at_most_twenty_pages_on_a_five_thousand_problem_report() {
    let Some(b) = big() else {
        eprintln!("skipping: typst not on PATH");
        return;
    };
    assert!(b.pages <= MAX_PAGES, "the PDF is {} pages", b.pages);
}

#[test]
fn the_pdf_outline_is_the_fifteen_sections_in_order() {
    let Some(b) = fixture_sized() else {
        eprintln!("skipping: typst not on PATH");
        return;
    };
    assert_eq!(b.outline, SECTIONS, "the PDF outline");
}

#[test]
fn the_vocabulary_names_the_same_fifteen_sections() {
    assert_eq!(v::PDF_SECTIONS, SECTIONS);
}

#[test]
fn no_em_dash_en_dash_or_multiplication_sign_is_in_the_rendered_text() {
    for b in [big(), fixture_sized()].into_iter().flatten() {
        for bad in ['\u{2014}', '\u{2013}', '\u{00D7}'] {
            assert!(
                !b.text.contains(bad),
                "U+{:04X} is in the PDF text",
                bad as u32
            );
        }
        assert!(
            !b.data.contains('\u{2014}'),
            "an em dash is in the PDF's data"
        );
    }
}

#[test]
fn a_whole_file_place_never_prints_line_zero() {
    let Some(b) = big() else {
        eprintln!("skipping: typst not on PATH");
        return;
    };
    let squashed: String = b.text.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(!squashed.contains("line0"), "line 0 is printed");
    assert!(!squashed.contains(".rs:0"), "path:0 is printed");
    assert!(
        squashed.contains(&v::WHOLE_FILE.replace(' ', "")),
        "no whole file place is named although some are escalated or in the top three"
    );
}

#[test]
fn a_many_place_pdf_holds_no_per_place_card_table_or_snippet() {
    let Some(b) = big() else {
        eprintln!("skipping: typst not on PATH");
        return;
    };
    for retired in [
        "Curated findings",
        "Held for review",
        "Sites reviewed in person",
        "What the findings have in common",
        "Where the individual findings are",
    ] {
        assert!(!b.flat.contains(retired), "{retired:?} is still a section");
    }
    assert!(
        !b.text.contains("SNIPPET_MARK_"),
        "a code snippet is printed"
    );
    assert!(
        !b.flat.contains("Worked example"),
        "a worked example is printed"
    );
    // A place's file name appears for the top three and for the escalated entries only.
    let squashed: String = b.text.chars().filter(|c| !c.is_whitespace()).collect();
    let named = squashed.matches("src/zone_").count();
    assert!(
        named <= 3 + 8,
        "{named} places are named in a PDF that lists at most eleven"
    );
}

#[test]
fn the_most_serious_problems_are_at_most_eight_narrative_entries_with_an_overflow_line() {
    let Some(b) = big() else {
        eprintln!("skipping: typst not on PATH");
        return;
    };
    assert_eq!(
        b.flat.matches("What to do: ").count(),
        8,
        "eight entries, no more"
    );
    let more = more_counts(&b.flat);
    assert!(
        more.contains(&(ESCALATED - 8)),
        "the overflow line names the {} places left: {more:?}",
        ESCALATED - 8
    );
    // Two sentences of what to do, never a third.
    assert!(!b.flat.contains("A third sentence"));
    assert!(b.flat.contains("Rules: SYN-RULE-"));
}

/// Every N in "and N more in the Fix plan sheet" the PDF prints.
fn more_counts(flat: &str) -> Vec<usize> {
    let re = regex::Regex::new(r"and (\d+) more in the Fix plan sheet").expect("a regex");
    re.captures_iter(flat)
        .filter_map(|c| c[1].parse().ok())
        .collect()
}

#[test]
fn the_other_fixes_are_one_line_each_capped_at_sixty_with_a_count_of_the_rest() {
    let Some(b) = big() else {
        eprintln!("skipping: typst not on PATH");
        return;
    };
    let open_fixes = b.json["sub_group_patterns"]
        .as_array()
        .expect("patterns")
        .iter()
        .filter(|p| p["open_sites"].as_u64().unwrap_or(0) > 0)
        .count();
    assert_eq!(open_fixes, 200);
    let rest = open_fixes - 8 - 60;
    assert!(
        more_counts(&b.flat).contains(&rest),
        "the other fixes name the {rest} not listed: {:?}",
        more_counts(&b.flat)
    );
}

#[test]
fn what_is_healthy_lists_at_most_twenty_five_rules_by_plain_title() {
    let Some(b) = big() else {
        eprintln!("skipping: typst not on PATH");
        return;
    };
    assert_eq!(b.flat.matches("(CLEAN-RULE-").count(), 25);
    assert!(
        b.flat.contains(&format!(
            "{} further rules verified clean",
            CLEAN_RULES - 25
        )),
        "the rest are counted"
    );
}

#[test]
fn where_the_details_are_uses_the_workbooks_tab_names() {
    let Some(b) = fixture_sized() else {
        eprintln!("skipping: typst not on PATH");
        return;
    };
    for (question, sheet) in v::DETAIL_MAP {
        assert!(b.flat.contains(question), "{question:?} is not printed");
        assert!(b.flat.contains(sheet), "{sheet:?} is not printed");
    }
}

#[test]
fn the_methodology_holds_no_engine_internals() {
    let Some(b) = fixture_sized() else {
        eprintln!("skipping: typst not on PATH");
        return;
    };
    for internal in [
        "Engine commit",
        "Rule library lock hash",
        "Calibration model",
        "of a budget of",
        "Escalated ",
        "Model backend",
        "Scanned with Camerata",
    ] {
        assert!(!b.flat.contains(internal), "{internal:?} is printed");
    }
}

#[test]
fn no_headline_in_the_pdf_starts_with_a_banned_prefix() {
    let Some(b) = big() else {
        eprintln!("skipping: typst not on PATH");
        return;
    };
    for line in b.text.lines() {
        assert!(
            crate::client_text::banned_headline_prefix(line).is_none(),
            "a line of the PDF opens with an engine note: {line}"
        );
    }
    let headlines = b.json["three_things"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(
            b.json["escalations"]["items"]
                .as_array()
                .into_iter()
                .flatten(),
        )
        .filter_map(|i| i["headline"].as_str());
    for h in headlines {
        assert!(
            crate::client_text::banned_headline_prefix(h).is_none(),
            "{h}"
        );
    }
}

#[test]
fn the_template_has_no_per_place_section_and_no_word_of_its_own_for_counts() {
    let template = include_str!("../../../templates/audit_report.typ");
    for gone in [
        "render_curated_group",
        "render_site",
        "d.curated_findings",
        "d.held_for_review_findings",
        "d.reduced",
        "FULL_PDF_SITE_LIMIT",
    ] {
        assert!(!template.contains(gone), "the template still has {gone}");
    }
    // "Closes N rules" and "N places" are built from the vocabulary, not spelled here.
    assert!(!template.contains("Closes"), "the template spells Closes");
}

#[test]
fn a_long_unbroken_area_name_wraps_and_the_pdf_stays_in_budget() {
    if !camerata_test_env::typst_on_path() {
        eprintln!("skipping: typst not on PATH");
        return;
    }
    let long = "W".repeat(90);
    let built = std::thread::spawn(move || {
        let mut sealed = synthetic_report(30, 5).into_inner();
        for f in &mut sealed.findings {
            f.category = Some(long.clone());
        }
        let reviewed = apply_overlay(&sealed.seal(), &[], &[], &[]);
        let data = render_data_json(&build_report_json(&reviewed, None, &options())).unwrap();
        let pdf = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(crate::report_export::compile_pdf_from_data(&data))
            .expect("the PDF compiles");
        let text = pdf_extract::extract_text_from_mem(&pdf).unwrap();
        let pages = pdf_extract::Document::load_mem(&pdf)
            .unwrap()
            .get_pages()
            .len();
        (text, pages)
    })
    .join()
    .unwrap();
    let squashed: String = built.0.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        squashed.to_lowercase().contains(&"w".repeat(90)),
        "the long name is cut off instead of wrapped"
    );
    assert!(built.1 <= MAX_PAGES);
}

/// W2: Start here said All problems held 118 rows and its scorecard Total said 126, because the
/// scorecard counted convention rows and the rows behind a place. A problem is a place: the
/// scorecard, the PDF scorecard and the PDF cover count the places All problems lists, once each.
#[test]
fn the_scorecards_and_the_cover_count_the_places_all_problems_lists() {
    use camerata_api_types::audit::{ConventionObservedKind, FindingKind};
    let mut rows = vec![
        finding("SEC-A-1", "src/a.rs", 1, "critical"),
        // A second rule at the same place: one problem, not two.
        finding("SEC-B-1", "src/a.rs", 1, "high"),
        finding("SEC-C-1", "src/c.rs", 3, "medium"),
        finding("SEC-D-1", "src/d.rs", 4, "low"),
    ];
    for n in 0..2 {
        let mut convention = finding("ARCH-CONV-1", &format!("src/conv{n}.rs"), 1, "low");
        convention.kind = Some(FindingKind::ConventionObserved(ConventionObservedKind {
            convention_option_id: String::new(),
            convention_label: "a habit".to_string(),
        }));
        rows.push(convention);
    }
    let report = report_with(
        rows,
        vec!["SEC-A-1", "SEC-B-1", "SEC-C-1", "SEC-D-1", "ARCH-CONV-1"],
    )
    .seal();
    let reviewed = apply_overlay(&report, &[], &[], &[]);
    let json = build_report_json(&reviewed, None, &options());
    let pdf_scorecard: usize = json
        .scorecard
        .rows
        .iter()
        .map(|r| r.critical + r.high + r.medium + r.low)
        .sum();
    let s = &json.cover.stats;
    let cover = s.critical + s.high + s.medium + s.low;

    let xlsx = crate::xlsx_export::build_workbook(&reviewed, None, &options()).expect("workbook");
    let grid = crate::release_gate::xlsx_sheet_grid(&xlsx, v::START_HERE).expect("Start here");
    let all_problems: usize = grid
        .iter()
        .find(|r| r.first().is_some_and(|c| c == v::ALL_PROBLEMS))
        .and_then(|r| r.get(2))
        .and_then(|c| c.parse().ok())
        .expect("Start here lists All problems with its row count");
    let header_at = grid
        .iter()
        .position(|r| {
            r.first().is_some_and(|c| c == v::H_AREA) && r.get(5).is_some_and(|c| c == "Total")
        })
        .expect("the scorecard header");
    let start_total: usize = grid
        .iter()
        .skip(header_at + 1)
        .find(|r| r.first().is_some_and(|c| c == "Total"))
        .map(|r| r[1..=4].iter().map(|c| c.parse::<usize>().unwrap()).sum())
        .expect("the scorecard Total");

    assert_eq!(all_problems, 3, "three places hold a problem");
    assert_eq!(start_total, all_problems, "Start here scorecard Total");
    assert_eq!(pdf_scorecard, all_problems, "PDF scorecard Total");
    assert_eq!(cover, all_problems, "PDF cover counts");
}

/// "Your conventions" is counts and short lines: what the code follows, where it has no single
/// habit and where it departs from itself. The rows are on the Your conventions and Departures
/// sheets.
#[test]
fn your_conventions_is_counts_and_short_lines() {
    use camerata_api_types::audit::{
        ConventionObservedKind, DeviationKind, FindingKind, NoConventionKind,
    };
    let mut rows = Vec::new();
    for (n, area) in ["Area One", "Area Two"].into_iter().enumerate() {
        let mut f = finding(&format!("ARCH-CONV-{n}"), &format!("src/c{n}.rs"), 1, "low");
        f.category = Some(area.to_string());
        f.kind = Some(FindingKind::ConventionObserved(ConventionObservedKind {
            convention_option_id: String::new(),
            convention_label: "a habit".to_string(),
        }));
        rows.push(f);
    }
    let mut none = finding("ARCH-NONE-1", "src/n.rs", 1, "low");
    none.kind = Some(FindingKind::NoConvention(NoConventionKind {
        observed_options: Vec::new(),
    }));
    rows.push(none);
    for line in [3, 4] {
        let mut f = finding("ARCH-DEP-1", "src/d.rs", line, "medium");
        f.category = Some("Area One".to_string());
        f.kind = Some(FindingKind::Deviation(DeviationKind {
            deviating_sites: 2,
            assessed_sites: Some(5),
            convention_option_id: None,
            convention_label: "Handlers call services".to_string(),
        }));
        rows.push(f);
    }
    let report = report_with(rows, vec![]).seal();
    let reviewed = apply_overlay(&report, &[], &[], &[]);
    let c = build_report_json(&reviewed, None, &options()).conventions;
    assert!(c.any);
    assert_eq!(
        c.followed_line,
        "2 conventions followed consistently, in 2 areas."
    );
    assert_eq!(
        c.none_adopted_line,
        "1 rule where your code has settled on no single habit."
    );
    assert_eq!(
        c.departures_line,
        "1 departure from a convention your code mostly follows, in 1 area (2 places)."
    );
    assert_eq!(c.departures.len(), 1);
    assert!(
        c.departures[0].ends_with("Handlers call services (2 of 5 places)"),
        "{:?}",
        c.departures
    );
    assert_eq!(c.more_line, "");
}

/// F1: the PDF states one number of problems. Two rows at one place (a critical at low effort and
/// a high at medium effort) and one other place are two problems: the cover, the severity by
/// effort matrix, the scorecard and the executive summary all say 2, and the gate fails when the
/// matrix says 3.
#[test]
fn the_cover_the_matrix_the_scorecard_and_the_summary_count_the_same_places() {
    let mut a = finding("SEC-A-1", "src/a.rs", 1, "critical");
    a.effort = Some("low".to_string());
    let mut b = finding("SEC-B-1", "src/a.rs", 1, "high");
    b.effort = Some("medium".to_string());
    let c = finding("SEC-C-1", "src/c.rs", 3, "medium");
    let report = report_with(vec![a, b, c], vec!["SEC-A-1", "SEC-B-1", "SEC-C-1"]).seal();
    let reviewed = apply_overlay(&report, &[], &[], &[]);
    let json = build_report_json(&reviewed, None, &options());
    let s = &json.cover.stats;
    assert_eq!(s.critical + s.high + s.medium + s.low, 2, "cover");
    let grid: usize = json
        .priority_grid
        .rows
        .iter()
        .flat_map(|r| &r.cells)
        .map(|c| c.findings.len())
        .sum();
    assert_eq!(grid, 2, "matrix");
    assert_eq!(json.priority_grid.held_count, 0);
    let card: usize = json
        .scorecard
        .rows
        .iter()
        .map(|r| r.critical + r.high + r.medium + r.low)
        .sum();
    assert_eq!(card, 2, "scorecard");
    assert!(
        json.executive_summary
            .narrative
            .contains("produced 2 problems"),
        "{}",
        json.executive_summary.narrative
    );
    // The dataset checks keep their rows: the matrix still holds all three.
    let rows: usize = json.matrix.do_now.len()
        + json.matrix.do_next.len()
        + json.matrix.plan.len()
        + json.matrix.accepted.len()
        + json.matrix.informational.len()
        + json.matrix.held.len();
    assert_eq!(rows, 3);
}

/// F2: a place where rules of two areas fired is counted in each area; the cover and the Total
/// count it once; no area row of zeros is marked Action-needed.
#[test]
fn an_area_row_counts_each_place_where_a_rule_of_that_area_fired() {
    let mut a = finding("SEC-A-1", "src/a.rs", 1, "critical");
    a.category = Some("universal".to_string());
    let mut b = finding("SEC-B-1", "src/a.rs", 1, "critical");
    b.category = Some("secret-exposure".to_string());
    let report = report_with(vec![a, b], vec!["SEC-A-1", "SEC-B-1"]).seal();
    let reviewed = apply_overlay(&report, &[], &[], &[]);
    let json = build_report_json(&reviewed, None, &options());
    for area in ["Universal", "Secret Exposure"] {
        let row = json
            .scorecard
            .rows
            .iter()
            .find(|r| r.category == area)
            .unwrap_or_else(|| panic!("no {area} row"));
        assert_eq!(row.critical, 1, "{area}");
    }
    assert_eq!(
        json.cover.stats.critical, 1,
        "the cover counts the place once"
    );
    for row in &json.scorecard.rows {
        let n = row.critical + row.high + row.medium + row.low;
        assert!(
            n > 0 || row.status != "Action-needed",
            "an all-zero row marked Action-needed: {row:?}"
        );
    }
    assert!(json.scorecard.footnote.contains("counted in each"));
    // Start here's scorecard agrees: each area 1, Total 1.
    let xlsx = crate::xlsx_export::build_workbook(&reviewed, None, &options()).expect("workbook");
    let grid = crate::release_gate::xlsx_sheet_grid(&xlsx, v::START_HERE).expect("Start here");
    let total = grid
        .iter()
        .find(|r| r.first().is_some_and(|c| c == "Total"))
        .expect("Total");
    assert_eq!(total[1], "1");
    let secret = grid
        .iter()
        .find(|r| r.first().is_some_and(|c| c == "Secret Exposure"))
        .expect("Secret Exposure row");
    assert_eq!(secret[1], "1");
}

/// F3: each escalated place prints the fix written for it, not the one worked example of its Fix.
#[test]
fn each_escalated_place_prints_its_own_fix() {
    let mut rows = Vec::new();
    for (line, fix) in [
        (1, "Rotate the key in a.rs. Then redeploy."),
        (2, "Enable row security on b.rs. Then add a policy."),
    ] {
        let mut f = finding("SEC-SAME-1", &format!("src/f{line}.rs"), line, "critical");
        f.fix_specific = Some(fix.to_string());
        f.review.escalation_state = EscalationState::Escalated;
        rows.push(f);
    }
    let report = report_with(rows, vec!["SEC-SAME-1"]).seal();
    let reviewed = apply_overlay(&report, &[], &[], &[]);
    let json = build_report_json(&reviewed, None, &options());
    let items = &json.escalations.items;
    assert_eq!(items.len(), 2);
    let a = items.iter().find(|i| i.path == "src/f1.rs").unwrap();
    let b = items.iter().find(|i| i.path == "src/f2.rs").unwrap();
    assert_eq!(a.fix_id, b.fix_id, "one Fix");
    assert_eq!(a.what_to_do, "Rotate the key in a.rs. Then redeploy.");
    assert_eq!(
        b.what_to_do,
        "Enable row security on b.rs. Then add a policy."
    );
}

/// F4: before a review the section says it is for review; "Reviewed in person" only when reviewed.
#[test]
fn the_most_serious_problems_say_for_review_until_they_are_reviewed() {
    let mut f = finding("SEC-A-1", "src/a.rs", 1, "critical");
    f.review.escalation_state = EscalationState::Escalated;
    let report = report_with(vec![f.clone()], vec!["SEC-A-1"]).seal();
    let raw = build_report_json(&apply_overlay(&report, &[], &[], &[]), None, &options());
    assert!(raw
        .escalations
        .intro
        .starts_with("For review in person, worst first."));
    assert!(!raw.escalations.intro.contains("Reviewed in person"));

    let key = report.findings[0].site_key.clone();
    let decision = camerata_api_types::decisions::Decision {
        id: "dec-000001".to_string(),
        key,
        state: camerata_api_types::decisions::DecisionState::Confirmed,
        reason: String::new(),
        scope: camerata_api_types::decisions::Scope::Site,
        applies_to: Vec::new(),
        except: Vec::new(),
        inherited_from: None,
        decided_by: "reviewer".to_string(),
        decided_at: "2026-11-04T09:00:00Z".to_string(),
        run_id: "insp-1".to_string(),
        engine_version: "0.0.0-test".to_string(),
        needed_me: None,
        sample_verdict: None,
        confirmed_by_client: None,
        revoked_at: None,
    };
    let reviewed = build_report_json(
        &apply_overlay(&report, &[decision], &[], &[]),
        None,
        &options(),
    );
    assert!(
        reviewed
            .escalations
            .intro
            .starts_with("Reviewed in person, worst first."),
        "{}",
        reviewed.escalations.intro
    );
}

/// F5: every "<name> sheet" the PDF names is a tab of the workbook.
#[test]
fn every_sheet_the_pdf_names_is_a_workbook_tab() {
    let rule = super::super::coverage_gaps::ScopeLimitedRuleJson {
        rule_id: "SEC-A-1".to_string(),
        files_checked: 3,
        files_total: 9,
        changed_files_only: false,
        scope: "language".to_string(),
    };
    let words = crate::report_export::PdfWordsJson::new();
    let mut strings = vec![
        super::super::coverage_gaps::scope_limited_sentence(&[rule]).unwrap(),
        crate::report_export::ai_tier_note_for(ReviewState::Raw, 1, 1),
        crate::report_export::ai_tier_note_for(ReviewState::Raw, 1, 2),
        words.details_intro.clone(),
        words.skipped_files_more.clone(),
        words.failed_passes_more.clone(),
    ];
    if let Some(b) = fixture_sized() {
        strings.push(b.data.clone());
        strings.push(b.flat.clone());
    }
    let tabs: Vec<&str> = v::SHEETS.iter().map(|s| s.name).collect();
    for text in strings {
        for (at, _) in text.match_indices(" sheet") {
            let after = &text[at + 6..];
            if after.starts_with('s') {
                continue;
            }
            let before = &text[..at];
            assert!(
                tabs.iter().any(|t| before.ends_with(t)),
                "the PDF names a sheet that is not a tab: ...{}",
                &before[before.len().saturating_sub(30)..]
            );
        }
        assert!(
            !text.contains("see Coverage"),
            "a pointer to a Coverage tab"
        );
    }
}
