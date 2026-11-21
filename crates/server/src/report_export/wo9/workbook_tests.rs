//! WO-B: the workbook a human reviewer can navigate. The sheet set and order, the Start here page
//! and its links, the shape of every table, the duplicates rule, the internal columns and the one
//! vocabulary. Every test reads the workbook's bytes, sheet by sheet.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use camerata_api_types::decisions::DecisionState;

use super::support::*;
use crate::release_gate::{read_zip_entries, xlsx_sheet_grid, xlsx_sheet_names};
use crate::report_export::{apply_overlay, ReportOptions, ReviewedReport};
use crate::xlsx_export::vocabulary as v;

/// The mixed report: SQL rows (one with a collapsed source behind it), layering rows, a
/// dependency advisory, one false positive and one ignored row.
fn reviewed_mixed() -> (ReviewedReport, crate::onboard::SealedReport) {
    let mut report = mixed_sealed().into_inner();
    report.findings[0].fix_specific = Some("Bind the parameters with the driver.".to_string());
    report.findings[0].flagged_by = vec![crate::review_tree::FlaggedBy {
        rule_id: "SEC-OTHER-1".to_string(),
        tool: None,
        site_key: "other-key".to_string(),
        path: "src/a.rs".to_string(),
        line: 11,
    }];
    let sealed = report.seal();
    let records = vec![
        decision(
            "dec-000001",
            &key_at(&sealed, "src/c.rs"),
            DecisionState::Ignored,
            "accepted for now",
        ),
        decision(
            "dec-000002",
            &key_at(&sealed, "src/e.rs"),
            DecisionState::FalsePositive,
            "the layer is allowed here",
        ),
    ];
    (apply_overlay(&sealed, &records, &[], &[]), sealed)
}

fn workbook_of(reviewed: &ReviewedReport, internal: bool) -> Vec<u8> {
    let options = ReportOptions {
        internal,
        ..client_options()
    };
    crate::xlsx_export::build_workbook(reviewed, None, &options).expect("the workbook builds")
}

fn client_workbook() -> Vec<u8> {
    workbook_of(&reviewed_mixed().0, false)
}

fn sheet_xml(xlsx: &[u8], sheet: &str) -> String {
    let names = xlsx_sheet_names(xlsx).unwrap();
    let at = names
        .iter()
        .position(|n| n == sheet)
        .unwrap_or_else(|| panic!("no sheet {sheet:?} in {names:?}"));
    let entries = read_zip_entries(xlsx).unwrap();
    String::from_utf8(entries[&format!("xl/worksheets/sheet{}.xml", at + 1)].clone()).unwrap()
}

/// Every internal link on a sheet: (cell, target sheet, target row 1-based).
pub(super) fn links(xlsx: &[u8], sheet: &str) -> Vec<(String, String, usize)> {
    let xml = sheet_xml(xlsx, sheet);
    let mut out = Vec::new();
    for chunk in xml.split("<hyperlink ").skip(1) {
        let attr = |name: &str| -> Option<String> {
            let key = format!("{name}=\"");
            let at = chunk.find(&key)? + key.len();
            Some(chunk[at..at + chunk[at..].find('"')?].to_string())
        };
        let (Some(cell), Some(location)) = (attr("ref"), attr("location")) else {
            continue;
        };
        let (name, at) = location.rsplit_once('!').unwrap();
        let name = name.trim_matches('\'').replace("''", "'");
        let row: usize = at
            .trim_start_matches(|c: char| c.is_ascii_alphabetic())
            .parse()
            .unwrap();
        out.push((cell, name, row));
    }
    out
}

/// A table's grid with its header row first, whatever sits above it.
pub(super) fn table(xlsx: &[u8], sheet: &str) -> Vec<Vec<String>> {
    let grid = xlsx_sheet_grid(xlsx, sheet).unwrap();
    let header_at = if sheet == v::ALL_SOURCE_ROWS { 2 } else { 0 };
    grid.into_iter().skip(header_at).collect()
}

pub(super) fn column(grid: &[Vec<String>], header: &str) -> Option<usize> {
    grid.first()?.iter().position(|h| h == header)
}

pub(super) fn cell_of<'a>(grid: &'a [Vec<String>], row: usize, header: &str) -> &'a str {
    let col = column(grid, header).unwrap_or_else(|| panic!("no {header:?} in {:?}", grid[0]));
    grid[row].get(col).map_or("", String::as_str)
}

// ── Item 2: the sheet set, the order and the names ─────────────────────────────────────────

#[test]
fn the_client_workbook_has_the_proposed_sheets_in_the_proposed_order() {
    let names = xlsx_sheet_names(&client_workbook()).unwrap();
    assert_eq!(
        names,
        [
            "Start here",
            "Glossary",
            "Fix plan",
            "All problems",
            "Needs a decision",
            "Your conventions",
            "Departures",
            "Dependencies",
            "Rules checked",
            "Ruled out",
            "Not checked",
            "All source rows",
            "Reviewer decisions",
            "How problems were grouped",
        ]
    );
}

#[test]
fn no_old_sheet_name_survives_and_no_area_has_a_sheet_of_its_own() {
    let xlsx = client_workbook();
    let names = xlsx_sheet_names(&xlsx).unwrap();
    for old in [
        "Index",
        "Sites",
        "Tree",
        "Decisions",
        "All Findings",
        "Remediation plan",
        "Deviations",
        "Conventions",
        "Coverage",
        "False Positives",
        "Thrown out by the engine",
        "Patterns across services",
    ] {
        assert!(!names.iter().any(|n| n == old), "{old:?} is still a sheet");
    }
    let known: BTreeSet<&str> = v::SHEETS.iter().map(|s| s.name).collect();
    for name in &names {
        assert!(
            known.contains(name.as_str()),
            "{name:?} is not a vocabulary sheet (a per-area sheet?)"
        );
    }
    // The Area column does the job of the per-area sheets.
    let problems = table(&xlsx, v::ALL_PROBLEMS);
    assert!(column(&problems, v::H_AREA).is_some());
}

#[test]
fn every_sheet_name_in_the_workbook_comes_from_the_vocabulary_and_the_pdf_map_uses_them() {
    let xlsx = client_workbook();
    let names = xlsx_sheet_names(&xlsx).unwrap();
    for (question, sheet) in v::DETAIL_MAP {
        assert!(
            names.iter().any(|n| n == sheet),
            "the PDF answers {question:?} with {sheet:?}, which is not a tab"
        );
    }
}

// ── Item 3: Start here and the way back ────────────────────────────────────────────────────

#[test]
fn start_here_links_to_every_other_sheet_and_every_sheet_links_back() {
    let xlsx = client_workbook();
    let names = xlsx_sheet_names(&xlsx).unwrap();
    let from_start: BTreeSet<String> = links(&xlsx, v::START_HERE)
        .into_iter()
        .map(|(_, sheet, _)| sheet)
        .collect();
    for name in names.iter().filter(|n| *n != v::START_HERE) {
        assert!(
            from_start.contains(name),
            "Start here does not link to {name:?}"
        );
        let back = links(&xlsx, name)
            .into_iter()
            .any(|(_, target, _)| target == v::START_HERE);
        assert!(back, "{name:?} has no link back to Start here");
    }
}

#[test]
fn start_here_says_what_each_sheet_is_and_how_many_rows_it_holds() {
    let xlsx = client_workbook();
    let grid = xlsx_sheet_grid(&xlsx, v::START_HERE).unwrap();
    for info in v::SHEETS.iter().filter(|s| s.name != v::START_HERE) {
        // The differences sheet exists only for a project of several repos.
        if info.name == v::PATTERNS_ACROSS_SERVICES {
            continue;
        }
        let row = grid
            .iter()
            .find(|r| r.first().is_some_and(|c| c == info.name))
            .unwrap_or_else(|| panic!("Start here does not list {:?}", info.name));
        assert_eq!(row.get(1).map(String::as_str), Some(info.purpose));
        let held = xlsx_sheet_grid(&xlsx, info.name).unwrap();
        // The banner sheet has two lines above its header.
        let above = if info.name == v::ALL_SOURCE_ROWS {
            3
        } else {
            1
        };
        // An empty table carries one line of words in place of a row.
        let listed: usize = row[2].parse().unwrap();
        let held_rows = held.len().saturating_sub(above);
        // A count of 0 is true only for an empty table, whose one line is the note.
        let empty_table =
            listed == 0 && held_rows == 1 && held[above].iter().skip(1).all(|c| c.is_empty());
        assert!(
            listed == held_rows || empty_table,
            "{:?}: Start here says {listed}, the sheet holds {}",
            info.name,
            held.len().saturating_sub(above)
        );
    }
}

#[test]
fn the_start_here_scorecard_equals_the_pdf_scorecard() {
    let (reviewed, _) = reviewed_mixed();
    let options = client_options();
    let json = crate::report_export::build_report_json(&reviewed, None, &options);
    let xlsx = workbook_of(&reviewed, false);
    let grid = xlsx_sheet_grid(&xlsx, v::START_HERE).unwrap();
    let header_at = grid
        .iter()
        .position(|r| {
            r.first().is_some_and(|c| c == v::H_AREA) && r.get(5).is_some_and(|c| c == "Total")
        })
        .expect("the scorecard header");
    let sheet: BTreeMap<String, [usize; 4]> = grid
        .iter()
        .skip(header_at + 1)
        .take_while(|r| r.first().is_some_and(|c| c != "Total"))
        .map(|r| {
            let n = |i: usize| r[i].parse::<usize>().unwrap();
            (r[0].clone(), [n(1), n(2), n(3), n(4)])
        })
        .collect();
    let pdf: BTreeMap<String, [usize; 4]> = json
        .scorecard
        .rows
        .iter()
        .map(|r| (r.category.clone(), [r.critical, r.high, r.medium, r.low]))
        .collect();
    assert_eq!(
        sheet, pdf,
        "Start here and the PDF scorecard count differently"
    );
    assert!(!pdf.is_empty());
}

// ── Item 4: the shape of every table ───────────────────────────────────────────────────────

#[test]
fn every_table_has_its_header_on_the_first_row_an_autofilter_a_frozen_top_row_and_first_column() {
    let xlsx = client_workbook();
    for name in xlsx_sheet_names(&xlsx).unwrap() {
        if name == v::START_HERE {
            continue;
        }
        let xml = sheet_xml(&xlsx, &name);
        let header_row = if name == v::ALL_SOURCE_ROWS { 3 } else { 1 };
        assert!(xml.contains("<autoFilter"), "{name:?} has no autofilter");
        assert!(
            xml.contains("state=\"frozen\"")
                && xml.contains(&format!("ySplit=\"{header_row}\""))
                && xml.contains("xSplit=\""),
            "{name:?} does not freeze the header row and the first column"
        );
        let grid = xlsx_sheet_grid(&xlsx, &name).unwrap();
        let first = &grid[header_row - 1];
        assert!(
            first.iter().any(|h| h == v::H_BACK_TO_START),
            "{name:?}: the header row {first:?} has no way back"
        );
        if name != v::GLOSSARY {
            assert!(
                xml.contains("outlineLevel=\"1\""),
                "{name:?} has no collapsed technical band"
            );
        }
    }
}

#[test]
fn the_technical_band_is_collapsed_and_holds_the_ids() {
    let xlsx = client_workbook();
    let xml = sheet_xml(&xlsx, v::ALL_PROBLEMS);
    assert!(
        xml.contains("hidden=\"1\""),
        "the technical columns start hidden"
    );
    let grid = table(&xlsx, v::ALL_PROBLEMS);
    let plain_last = column(&grid, v::H_SEVERITY_NOTE).unwrap();
    for id in [v::H_PLACE_ID, v::H_FIX_ID, v::H_RULE_IDS, v::H_KIND] {
        assert!(
            column(&grid, id).unwrap() > plain_last,
            "{id:?} is not in the technical band"
        );
    }
}

#[test]
fn problems_say_whole_file_instead_of_line_zero() {
    let mut report = mixed_sealed().into_inner();
    report.findings[0].line = 0;
    let (reviewed, _) = {
        let sealed = report.seal();
        (apply_overlay(&sealed, &[], &[], &[]), sealed)
    };
    let xlsx = workbook_of(&reviewed, false);
    let grid = table(&xlsx, v::ALL_PROBLEMS);
    let lines: Vec<&str> = (1..grid.len())
        .map(|r| cell_of(&grid, r, v::H_LINE))
        .collect();
    assert!(lines.contains(&"whole file"), "{lines:?}");
    assert!(!lines.contains(&"0"), "{lines:?}");
}

#[test]
fn the_fix_plan_and_all_problems_link_to_each_other_by_fix_id() {
    let xlsx = client_workbook();
    let plan = table(&xlsx, v::FIX_PLAN);
    let problems = table(&xlsx, v::ALL_PROBLEMS);
    assert!(plan.len() > 1, "the fixture has fixes to plan");
    // Fix plan to All problems: the link lands on a row with the same Fix id.
    let go = column(&plan, v::H_GO_TO_PLACES).unwrap();
    let fix_id_col = column(&plan, v::H_FIX_ID).unwrap();
    let plan_links: HashMap<String, (String, usize)> = links(&xlsx, v::FIX_PLAN)
        .into_iter()
        .map(|(cell, sheet, row)| (cell, (sheet, row)))
        .collect();
    let mut followed = 0;
    for (i, row) in plan.iter().enumerate().skip(1) {
        let col_letter = col_letter(go);
        let (sheet, target) = plan_links
            .get(&format!("{col_letter}{}", i + 1))
            .unwrap_or_else(|| panic!("the fix in row {} links to no places", i + 1));
        assert!(
            sheet == v::ALL_PROBLEMS || sheet == v::DEPENDENCIES,
            "the link goes to {sheet:?}"
        );
        let landed = table(&xlsx, sheet);
        assert_eq!(
            cell_of(&landed, target - 1, v::H_FIX_ID),
            row[fix_id_col],
            "the link lands on another fix's place"
        );
        followed += 1;
    }
    assert!(followed > 0, "no Fix plan row links to its places");
    // All problems to Fix plan, and the way back.
    let fix_col = column(&problems, v::H_FIX).unwrap();
    let problem_links: HashMap<String, (String, usize)> = links(&xlsx, v::ALL_PROBLEMS)
        .into_iter()
        .map(|(cell, sheet, row)| (cell, (sheet, row)))
        .collect();
    let mut back = 0;
    for (i, row) in problems.iter().enumerate().skip(1) {
        let Some((sheet, target)) = problem_links.get(&format!("{}{}", col_letter(fix_col), i + 1))
        else {
            continue;
        };
        assert_eq!(sheet, v::FIX_PLAN);
        let id_col = column(&problems, v::H_FIX_ID).unwrap();
        assert_eq!(cell_of(&plan, target - 1, v::H_FIX_ID), row[id_col]);
        back += 1;
    }
    assert!(back > 0, "no problem links to its fix");
}

pub(super) fn col_letter(mut col: usize) -> String {
    let mut out = String::new();
    loop {
        out.insert(0, (b'A' + (col % 26) as u8) as char);
        if col < 26 {
            return out;
        }
        col = col / 26 - 1;
    }
}

// ── Item 5: only All source rows lists a problem more than once ───────────────────────────

#[test]
fn only_all_source_rows_lists_a_place_more_than_once() {
    let xlsx = client_workbook();
    assert!(
        xlsx_sheet_names(&xlsx)
            .unwrap()
            .iter()
            .any(|n| n == v::ALL_SOURCE_ROWS),
        "the sheet that may repeat a problem exists"
    );
    for name in xlsx_sheet_names(&xlsx).unwrap() {
        if matches!(
            name.as_str(),
            "Start here" | "Glossary" | "Reviewer decisions" | "How problems were grouped"
        ) {
            continue;
        }
        let grid = table(&xlsx, &name);
        let Some(col) = column(&grid, v::H_PLACE_ID) else {
            continue;
        };
        let mut seen = BTreeSet::new();
        let mut repeated = 0;
        for row in grid.iter().skip(1) {
            let id = row.get(col).map_or("", String::as_str);
            if !id.is_empty() && !seen.insert(id.to_string()) {
                repeated += 1;
            }
        }
        if name == v::ALL_SOURCE_ROWS {
            assert!(
                repeated > 0,
                "the fixture has a collapsed source behind a problem"
            );
        } else {
            assert_eq!(repeated, 0, "{name:?} lists a place more than once");
        }
    }
}

#[test]
fn all_source_rows_says_in_its_title_and_first_line_that_it_repeats_problems() {
    let xlsx = client_workbook();
    let grid = xlsx_sheet_grid(&xlsx, v::ALL_SOURCE_ROWS).unwrap();
    assert_eq!(grid[0][0], "All source rows (complete record)");
    assert!(grid[1][0].starts_with("Raw record."), "{:?}", grid[1][0]);
    assert!(grid[1][0].contains("A problem can be listed several times here"));
    assert!(grid[1][0].contains("Every other sheet shows it once"));
}

#[test]
fn every_source_row_names_how_many_ways_its_place_was_found_and_links_to_its_row() {
    let xlsx = client_workbook();
    let sources = table(&xlsx, v::ALL_SOURCE_ROWS);
    let problems = table(&xlsx, v::ALL_PROBLEMS);
    let roles: BTreeSet<&str> = (1..sources.len())
        .map(|r| cell_of(&sources, r, v::H_ROW_ROLE))
        .collect();
    assert!(
        roles.contains("Primary") && roles.contains("Collapsed source"),
        "{roles:?}"
    );
    let place_col = column(&sources, v::H_PLACE_ID).unwrap();
    let by_place: HashMap<&str, Vec<usize>> =
        (1..sources.len()).fold(HashMap::new(), |mut m, r| {
            m.entry(sources[r][place_col].as_str()).or_default().push(r);
            m
        });
    let ways_col = column(&sources, v::H_FOUND_HOW_MANY_WAYS).unwrap();
    for (place, rows) in &by_place {
        for r in rows {
            assert_eq!(
                sources[*r][ways_col],
                rows.len().to_string(),
                "{place}: Found how many ways"
            );
        }
    }
    // Every link lands on a row of the sheet it names, with the same Place id.
    let represented = col_letter(column(&sources, v::H_REPRESENTED_BY).unwrap());
    let mut resolved = 0;
    for (cell, sheet, target) in links(&xlsx, v::ALL_SOURCE_ROWS) {
        let letters: String = cell.chars().take_while(char::is_ascii_alphabetic).collect();
        if letters != represented {
            continue;
        }
        // The sheet has a title and a first line above its header, so row 4 is the first row.
        let at = cell[letters.len()..].parse::<usize>().unwrap() - 3;
        let grid = table(&xlsx, &sheet);
        assert_eq!(
            cell_of(&grid, target - 1, v::H_PLACE_ID),
            sources[at][place_col],
            "the link in {cell} lands on another place"
        );
        resolved += 1;
    }
    assert!(resolved > 0);
    assert!(problems.len() > 1);
}

// ── Item 6: internal columns only on an internal export ───────────────────────────────────

fn internal_headers() -> Vec<String> {
    let mut headers: Vec<String> = crate::report_export::row_facts::INTERNAL_TRIAGE_HEADERS
        .iter()
        .map(|h| h.to_string())
        .collect();
    headers.extend([
        v::H_REVIEW_FLAGS.to_string(),
        v::H_HELD.to_string(),
        v::POSSIBLE_FALSE_POSITIVE.to_string(),
        v::H_ESCALATED.to_string(),
        "Summary source".to_string(),
        "Below the line".to_string(),
        "Outliers".to_string(),
        "Unplaced".to_string(),
    ]);
    headers
}

#[test]
fn a_client_export_carries_none_of_the_internal_columns() {
    let xlsx = client_workbook();
    let internal = internal_headers();
    for name in xlsx_sheet_names(&xlsx).unwrap() {
        let grid = table(&xlsx, &name);
        for h in &grid[0] {
            assert!(
                !internal.contains(h),
                "{name:?} carries the internal column {h:?} in a client export"
            );
        }
    }
    let glossary = xlsx_sheet_grid(&xlsx, v::GLOSSARY).unwrap();
    let terms: Vec<&str> = glossary.iter().map(|r| r[0].as_str()).collect();
    for reviewers_only in [
        "Escalated",
        v::POSSIBLE_FALSE_POSITIVE,
        camerata_api_types::review::FpReason::TestCodeOnly.label(),
    ] {
        assert!(
            !terms.contains(&reviewers_only),
            "the Glossary defines the reviewers-only term {reviewers_only:?}"
        );
    }
}

#[test]
fn an_internal_export_adds_the_internal_columns_to_all_problems() {
    let xlsx = workbook_of(&reviewed_mixed().0, true);
    let grid = table(&xlsx, v::ALL_PROBLEMS);
    for h in internal_headers().iter().take(12) {
        assert!(
            column(&grid, h).is_some(),
            "{h:?} is missing from an internal export"
        );
    }
}

// ── Item 7 and the vocabulary ──────────────────────────────────────────────────────────────

fn all_strings(xlsx: &[u8], sheets: &[&str]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for name in xlsx_sheet_names(xlsx).unwrap() {
        if !sheets.is_empty() && !sheets.contains(&name.as_str()) {
            continue;
        }
        for row in xlsx_sheet_grid(xlsx, &name).unwrap() {
            for cell in row {
                out.push((name.clone(), cell));
            }
        }
    }
    out
}

#[test]
fn no_dash_or_multiplication_sign_in_the_strings_the_workbook_code_writes() {
    let xlsx = client_workbook();
    // The strings this crate writes: the static sheets, every header, and the columns whose
    // words the workbook code authors. The prose of a problem comes from the report text, which
    // another work order owns.
    let authored = [
        v::H_FIX_ORDER,
        v::H_DECISION_SO_FAR,
        v::H_WHY_IT_IS_WAITING,
        v::H_HOW_CONSISTENTLY,
        v::H_RESULT,
        v::H_RULED_OUT_BY,
        v::H_GAP,
        v::H_ROW_ROLE,
        v::H_SHOWN_ON,
        v::H_WITHIN,
        v::H_SOURCE,
        v::H_PLACES_CHECKED,
    ];
    let dash = |t: &str| t.contains(['\u{2014}', '\u{2013}', '\u{00D7}']);
    for (sheet, text) in all_strings(&xlsx, &[v::START_HERE, v::GLOSSARY]) {
        assert!(
            !dash(&text),
            "{sheet:?} carries a dash or a multiplication sign: {text:?}"
        );
    }
    for name in xlsx_sheet_names(&xlsx).unwrap() {
        let grid = table(&xlsx, &name);
        for h in &grid[0] {
            assert!(!dash(h), "header {h:?} on {name:?}");
        }
        for h in authored {
            if column(&grid, h).is_some() {
                for r in 1..grid.len() {
                    let text = cell_of(&grid, r, h);
                    assert!(!dash(text), "{name:?} / {h:?}: {text:?}");
                }
            }
        }
    }
}

#[test]
fn no_retired_synonym_in_a_sheet_name_a_header_or_the_static_sheets() {
    let xlsx = client_workbook();
    for name in xlsx_sheet_names(&xlsx).unwrap() {
        assert_eq!(
            v::banned_synonym_in(&name),
            None,
            "sheet name {name:?} uses a retired word"
        );
        let grid = table(&xlsx, &name);
        for h in &grid[0] {
            assert_eq!(
                v::banned_synonym_in(h),
                None,
                "header {h:?} on {name:?} uses a retired word (the second item is its replacement)"
            );
        }
    }
    for (sheet, text) in all_strings(&xlsx, &[v::START_HERE, v::GLOSSARY]) {
        assert_eq!(
            v::banned_synonym_in(&text),
            None,
            "{sheet:?} says {text:?}, which uses a retired word (the second item is its replacement)"
        );
    }
}

#[test]
fn every_sheet_name_and_every_shared_header_is_defined_in_the_glossary() {
    let xlsx = client_workbook();
    let glossary: BTreeSet<String> = xlsx_sheet_grid(&xlsx, v::GLOSSARY)
        .unwrap()
        .iter()
        .skip(1)
        .map(|r| r[0].to_lowercase())
        .collect();
    for name in xlsx_sheet_names(&xlsx).unwrap() {
        assert!(
            glossary.contains(&name.to_lowercase()),
            "sheet {name:?} is not in the Glossary"
        );
    }
    let mut on_sheets: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for name in xlsx_sheet_names(&xlsx).unwrap() {
        for h in &table(&xlsx, &name)[0] {
            on_sheets.entry(h.clone()).or_default().insert(name.clone());
        }
    }
    for (header, sheets) in &on_sheets {
        if sheets.len() > 1 {
            assert!(
                glossary.contains(&header.to_lowercase()),
                "{header:?} heads a column on {sheets:?} and is not in the Glossary"
            );
        }
    }
    // The technical band is defined too.
    for h in [
        v::H_PLACE_ID,
        v::H_FIX_ID,
        v::H_RULE_ID,
        v::H_KIND,
        v::H_STATUS,
        v::H_CARRIED_FROM,
    ] {
        assert!(
            glossary.contains(&h.to_lowercase()),
            "{h:?} is not in the Glossary"
        );
    }
}

#[test]
fn fix_order_uses_one_set_of_words_on_every_sheet() {
    let xlsx = client_workbook();
    // A place or a fix with nothing to act on prints its class in place of an order.
    let allowed = [
        "First",
        "Next",
        "Later",
        "Needs a decision",
        "Information only",
        "Accepted risk",
    ];
    for sheet in [v::FIX_PLAN, v::ALL_PROBLEMS, v::NEEDS_A_DECISION] {
        let grid = table(&xlsx, sheet);
        for r in 1..grid.len() {
            // An empty table carries one line of words in place of a row.
            if cell_of(&grid, r, v::H_SEVERITY).is_empty() {
                continue;
            }
            let word = cell_of(&grid, r, v::H_FIX_ORDER);
            assert!(allowed.contains(&word), "{sheet:?}: Fix order {word:?}");
        }
    }
    for (sheet, text) in all_strings(&xlsx, &[]) {
        for old in ["Do now", "Do next", "Fix first", "Backlog"] {
            assert!(!text.contains(old), "{sheet:?} says {old:?}");
        }
    }
}

#[test]
fn needs_a_decision_lists_only_problems_that_wait_and_says_why() {
    let mut report = mixed_sealed().into_inner();
    report.findings[1].needs_review = true;
    let sealed = report.seal();
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let xlsx = workbook_of(&reviewed, false);
    let waiting = table(&xlsx, v::NEEDS_A_DECISION);
    let problems = table(&xlsx, v::ALL_PROBLEMS);
    for r in 1..waiting.len() {
        assert_eq!(cell_of(&waiting, r, v::H_NEEDS_A_DECISION), "Yes");
        assert!(!cell_of(&waiting, r, v::H_WHY_IT_IS_WAITING).is_empty());
    }
    let marked = (1..problems.len())
        .filter(|r| cell_of(&problems, *r, v::H_NEEDS_A_DECISION) == "Yes")
        .count();
    assert_eq!(
        waiting.len() - 1,
        marked,
        "the sheet and the column disagree"
    );
}

/// "Pattern" names one concept only: how each service follows each rule. In the static sheets the
/// word appears only in that concept's own entries.
#[test]
fn the_word_pattern_names_only_how_services_follow_rules() {
    let xlsx = client_workbook();
    // The sheet's own name may be written anywhere; any other use of the word must be the
    // concept's own entry.
    let has_pattern = |t: &str| {
        t.replace(v::PATTERNS_ACROSS_SERVICES, "")
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .any(|w| w == "pattern" || w == "patterns")
    };
    let glossary = xlsx_sheet_grid(&xlsx, v::GLOSSARY).unwrap();
    let mut seen = 0;
    for row in &glossary {
        if row.iter().any(|c| has_pattern(c)) {
            seen += 1;
            assert!(
                row[0] == "Pattern" || row[0] == v::PATTERNS_ACROSS_SERVICES,
                "the Glossary uses pattern in the entry {:?}",
                row[0]
            );
        }
    }
    assert!(seen >= 2, "the pattern concept is defined");
    for row in xlsx_sheet_grid(&xlsx, v::START_HERE).unwrap() {
        if row.iter().any(|c| has_pattern(c)) {
            assert_eq!(row[0], v::PATTERNS_ACROSS_SERVICES, "{row:?}");
        }
    }
    for name in xlsx_sheet_names(&xlsx).unwrap() {
        for h in &table(&xlsx, &name)[0] {
            assert!(!has_pattern(h), "header {h:?} on {name:?} uses pattern");
        }
    }
}

/// A group that holds only conventions is not a fix: the Fix plan lists a group only when it has
/// problems (or dependency advisories) to go to, every row links to them, and Rules checked does
/// not call a convention a place with a problem.
#[test]
fn the_fix_plan_lists_only_fixes_and_rules_checked_never_calls_a_convention_a_problem() {
    use camerata_api_types::audit::{ConventionObservedKind, FindingKind};
    let mut report = mixed_sealed().into_inner();
    let mut convention = sql_row("src/api/list.rs", 7);
    convention.rule_id = "ARCH-PAGINATION-1".to_string();
    convention.severity = "info".to_string();
    convention.kind = Some(FindingKind::ConventionObserved(ConventionObservedKind {
        convention_option_id: "cursor".to_string(),
        convention_label: "Cursor pagination".to_string(),
        standard_source: None,
        differs_from_default: None,
    }));
    report.findings.push(convention);
    report
        .provenance
        .audited_rule_ids
        .push("ARCH-PAGINATION-1".to_string());
    let sealed = report.seal();
    let xlsx = workbook_of(&apply_overlay(&sealed, &[], &[], &[]), false);

    let plan = table(&xlsx, v::FIX_PLAN);
    let go = column(&plan, v::H_GO_TO_PLACES).unwrap();
    let rules = column(&plan, v::H_RULES).unwrap();
    let order = column(&plan, v::H_ORDER).unwrap();
    let linked: BTreeSet<String> = links(&xlsx, v::FIX_PLAN)
        .into_iter()
        .map(|(cell, _, _)| cell)
        .collect();
    assert!(plan.len() > 1, "the fixture has fixes");
    for (i, row) in plan.iter().enumerate().skip(1) {
        assert!(
            linked.contains(&format!("{}{}", col_letter(go), i + 1)),
            "the fix {:?} has no link to its places",
            row[column(&plan, v::H_FIX).unwrap()]
        );
        assert!(
            !row[rules].contains("ARCH-PAGINATION-1"),
            "a convention-only group is not a fix: {row:?}"
        );
        assert_eq!(row[order], i.to_string(), "Order is 1, 2, 3 without gaps");
    }

    let checked = table(&xlsx, v::RULES_CHECKED);
    let at = (1..checked.len())
        .find(|r| cell_of(&checked, *r, v::H_RULE_ID) == "ARCH-PAGINATION-1")
        .expect("the convention's rule was checked");
    let result = cell_of(&checked, at, v::H_RESULT);
    assert!(
        !result.starts_with("Found"),
        "a convention is not a place with a problem: {result:?}"
    );
}

// ── the convention model's wording (design 2026-11-20, sections 7 and 11, tests 17 to 19) ──

mod convention_wording {
    use camerata_api_types::audit::{
        ConventionObservedKind, DeviationKind, FindingKind, NoConventionKind, ObservedOptionUsage,
        StandardSource,
    };

    use super::*;

    fn rule_row(rule: &str, path: &str, kind: FindingKind) -> crate::onboard::Finding {
        let mut f = sql_row(path, 5);
        f.rule_id = rule.to_string();
        f.severity = "low".to_string();
        f.kind = Some(kind);
        f
    }

    fn deviation(source: Option<StandardSource>, differs: Option<&str>) -> FindingKind {
        FindingKind::Deviation(DeviationKind {
            deviating_sites: 2,
            assessed_sites: Some(5),
            convention_option_id: Some("a".into()),
            convention_label: "Option A".into(),
            standard_source: source,
            differs_from_default: differs.map(str::to_string),
        })
    }

    fn workbook_with(rows: Vec<crate::onboard::Finding>) -> Vec<u8> {
        let mut report = mixed_sealed().into_inner();
        for r in &rows {
            report.provenance.audited_rule_ids.push(r.rule_id.clone());
        }
        report.findings.extend(rows);
        workbook_of(&apply_overlay(&report.seal(), &[], &[], &[]), false)
    }

    fn departures_cell(xlsx: &[u8], rule: &str) -> String {
        let t = table(xlsx, v::DEPARTURES);
        let at = (1..t.len())
            .find(|r| cell_of(&t, *r, v::H_RULE_ID) == rule)
            .unwrap_or_else(|| panic!("no departure row for {rule}: {t:?}"));
        cell_of(&t, at, v::H_CONVENTION_SOURCE).to_string()
    }

    fn conventions_row(xlsx: &[u8], rule: &str) -> (String, String) {
        let t = table(xlsx, v::YOUR_CONVENTIONS);
        let at = (1..t.len())
            .find(|r| cell_of(&t, *r, v::H_RULE_ID) == rule)
            .unwrap_or_else(|| panic!("no convention row for {rule}: {t:?}"));
        (
            cell_of(&t, at, v::H_HOW_CONSISTENTLY).to_string(),
            cell_of(&t, at, v::H_WHAT_YOUR_CODE_DOES).to_string(),
        )
    }

    /// Test 17: the observed, reviewer and open question wordings, and the untouched split.
    #[test]
    fn the_sheets_use_the_wording_of_the_observed_reviewer_open_and_split_cases() {
        let xlsx = workbook_with(vec![
            rule_row("ARCH-OBS-1", "src/o1.rs", deviation(None, Some("Option B"))),
            rule_row("ARCH-OBS-1", "src/o2.rs", deviation(None, Some("Option B"))),
            rule_row(
                "ARCH-REV-1",
                "src/r1.rs",
                deviation(Some(StandardSource::Reviewer), None),
            ),
            rule_row(
                "ARCH-REV-1",
                "src/r2.rs",
                deviation(Some(StandardSource::Reviewer), None),
            ),
            rule_row(
                "ARCH-SETREV-1",
                "src/s.rs",
                FindingKind::ConventionObserved(ConventionObservedKind {
                    convention_option_id: "a".into(),
                    convention_label: "Option A".into(),
                    standard_source: Some(StandardSource::Reviewer),
                    differs_from_default: None,
                }),
            ),
            rule_row(
                "ARCH-OPEN-1",
                "",
                FindingKind::NoConvention(NoConventionKind {
                    observed_options: vec![
                        ObservedOptionUsage {
                            option_id: "a".into(),
                            label: "Option A".into(),
                            sites: Some(3),
                        },
                        ObservedOptionUsage {
                            option_id: "b".into(),
                            label: "Option B".into(),
                            sites: Some(1),
                        },
                    ],
                    left_to_client: true,
                }),
            ),
            rule_row(
                "ARCH-SPLIT-1",
                "",
                FindingKind::NoConvention(NoConventionKind {
                    observed_options: vec![],
                    left_to_client: false,
                }),
            ),
        ]);
        let obs = departures_cell(&xlsx, "ARCH-OBS-1");
        assert!(
            obs.contains("Observed in your code: 3 of 5 places follow it. 2 places depart. Differs from the rule's default: Option B."),
            "{obs}"
        );
        let rev = departures_cell(&xlsx, "ARCH-REV-1");
        assert!(
            rev.contains("Convention set by the reviewer: Option A. 2 places depart."),
            "{rev}"
        );
        let (status, _) = conventions_row(&xlsx, "ARCH-SETREV-1");
        assert_eq!(
            status,
            crate::xlsx_export::CONVENTION_STATUS_SET_BY_REVIEWER
        );
        let (status, what) = conventions_row(&xlsx, "ARCH-OPEN-1");
        assert_eq!(status, crate::xlsx_export::CONVENTION_STATUS_OPEN_QUESTION);
        assert!(
            what.contains("Option A (3 places)") && what.contains("Option B (1 place)"),
            "{what}"
        );
        let (status, _) = conventions_row(&xlsx, "ARCH-SPLIT-1");
        assert_eq!(status, crate::xlsx_export::CONVENTION_STATUS_NONE_ADOPTED);
    }

    /// Test 18: an open question lists every option of the rule from the corpus (label, directive,
    /// why, places) and marks none.
    #[test]
    fn an_open_question_lists_every_option_with_directive_and_why() {
        let mut rule = camerata_rules::bare_rule("ARCH-OPEN-1", "universal");
        for (id, label) in [("a", "Option A"), ("b", "Option B")] {
            rule.options.push(camerata_rules::RuleOption {
                id: id.into(),
                label: label.into(),
                directive: format!("Use {label}."),
                why: format!("{label} is simpler."),
                remediation: None,
                finding_headline: None,
                finding_detail: None,
                escalation: None,
                effort: None,
                violates: false,
            });
        }
        let mut corpus = camerata_rules::RuleSet::default();
        corpus.push(rule);
        let mut report = mixed_sealed().into_inner();
        report
            .provenance
            .audited_rule_ids
            .push("ARCH-OPEN-1".into());
        report.findings.push(rule_row(
            "ARCH-OPEN-1",
            "",
            FindingKind::NoConvention(NoConventionKind {
                observed_options: vec![ObservedOptionUsage {
                    option_id: "a".into(),
                    label: "Option A".into(),
                    sites: Some(3),
                }],
                left_to_client: true,
            }),
        ));
        let reviewed = apply_overlay(&report.seal(), &[], &[], &[]);
        let options = ReportOptions {
            internal: false,
            ..client_options()
        };
        let xlsx = crate::xlsx_export::build_workbook(&reviewed, Some(&corpus), &options).unwrap();
        let (_, what) = conventions_row(&xlsx, "ARCH-OPEN-1");
        assert!(
            what.contains("Option A: Use Option A. Why: Option A is simpler. (3 places)"),
            "{what}"
        );
        assert!(
            what.contains("Option B: Use Option B. Why: Option B is simpler."),
            "{what}"
        );
        let t = table(&xlsx, v::YOUR_CONVENTIONS);
        let at = (1..t.len())
            .find(|r| cell_of(&t, *r, v::H_RULE_ID) == "ARCH-OPEN-1")
            .unwrap();
        assert!(!cell_of(&t, at, v::H_HOW_CONSISTENTLY).contains("recommend"));
    }
}
